//! App integration for ingestion, approvals, relays and event forwarding.

use eframe::egui;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use super::{MyrstackApp, now_ms};
use crate::actions::{Action, ActionId, ActionResult};
use crate::control::{ControlError, ErrorCode, Reply};
use crate::envoy::channel::InboundEvent;
use crate::envoy::control::EnvoyCommand;
use crate::envoy::drafts::{Draft, DraftKind, DraftQueue, DraftStatus, ReplyTo};
use crate::envoy::inbox::Inbox;
use crate::envoy::relay::{RelayLimiter, RelayRequest};
use crate::envoy::settings::EnvoySettings;
use crate::envoy::worker::{Update, Work, Worker};
use crate::envoy::{BridgeStatus, EnvoyIdentity, EnvoyStatus};
use crate::notify::{Incoming, Severity, Source};
use crate::orca::TerminalInput;
use crate::traffic::{AgentMessage, MessageKind};

pub const DRAFTS_TARGET: &str = "envoy:drafts";
const SHAKE_COOLDOWN_MS: i64 = 8_000;

pub struct AppEnvoy {
    handle: Option<String>,
    settings: EnvoySettings,
    inbox: Inbox,
    drafts: Option<DraftQueue>,
    worker: Option<Worker>,
    bridge: BridgeStatus,
    bridge_seen: bool,
    error_notified: Option<String>,
    acknowledged: HashSet<String>,
    relays: RelayLimiter,
    relay_path: PathBuf,
    pending: VecDeque<InboundEvent>,
    forwarding: Option<(ActionId, Vec<InboundEvent>)>,
    relay_actions: HashMap<ActionId, AgentMessage>,
    outgoing: HashMap<String, String>,
    traffic_handle: Option<String>,
    forward_after_ms: i64,
    notices: HashSet<String>,
    pub shake_started_ms: Option<i64>,
    pub open_request: bool,
    pub error: Option<String>,
}

impl EnvoyIdentity for AppEnvoy {
    fn envoy_handle(&self) -> Option<&str> {
        self.handle.as_deref()
    }
}

impl AppEnvoy {
    pub fn start(ctx: &egui::Context) -> Self {
        Self::at(
            crate::claude::hook::data_dir(),
            ctx,
            crate::envoy::slack::SlackCli,
            crate::envoy::slack::bridge_dir(),
        )
    }

    pub fn at(
        dir: PathBuf,
        ctx: &egui::Context,
        io: impl crate::envoy::slack::SlackIo + 'static,
        bridge: PathBuf,
    ) -> Self {
        let mut core = Self::load(dir.clone());
        let wake = ctx.clone();
        if core.error.is_none() {
            core.worker = Some(Worker::spawn(
                dir.join(crate::envoy::inbox::INBOX_FILE),
                core.inbox.clone(),
                io,
                bridge,
                move || wake.request_repaint(),
            ));
        }
        core
    }

    fn load(dir: PathBuf) -> Self {
        let inbox_path = dir.join(crate::envoy::inbox::INBOX_FILE);
        let loaded = Inbox::load(&inbox_path);
        let drafts = DraftQueue::load(dir.join(crate::envoy::drafts::DRAFTS_FILE));
        let relay_path = dir.join("envoy-relays.json");
        let relays = RelayLimiter::load(&relay_path);
        let error = loaded
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .or_else(|| drafts.as_ref().err().cloned())
            .or_else(|| relays.as_ref().err().cloned());
        let inbox = loaded.unwrap_or_default();
        Self {
            handle: None,
            settings: EnvoySettings::default(),
            inbox,
            drafts: drafts.ok(),
            worker: None,
            bridge: BridgeStatus::Missing,
            bridge_seen: false,
            error_notified: None,
            acknowledged: HashSet::new(),
            relays: relays.unwrap_or_default(),
            relay_path,
            pending: VecDeque::new(),
            forwarding: None,
            relay_actions: HashMap::new(),
            outgoing: HashMap::new(),
            traffic_handle: None,
            forward_after_ms: 0,
            notices: HashSet::new(),
            shake_started_ms: None,
            open_request: false,
            error,
        }
    }

    /// The lifecycle owner supplies the handle after persisting it.
    pub fn set_enabled(&mut self, handle: String, settings: EnvoySettings) -> Result<(), String> {
        if handle.trim().is_empty() {
            return Err("Envoy terminal handle is empty".into());
        }
        if self.handle.as_deref() == Some(&handle) && self.settings == settings {
            return Ok(());
        }
        self.worker()?
            .submit(Work::Configure(Some(settings.clone())))?;
        if self.handle.is_none() {
            self.bridge_seen = false;
            self.pending = self
                .inbox
                .unforwarded()
                .into_iter()
                .filter(|e| {
                    !self.acknowledged.contains(&e.id())
                        && self
                            .forwarding
                            .as_ref()
                            .is_none_or(|(_, batch)| !batch.iter().any(|b| b.id() == e.id()))
                })
                .collect();
        }
        self.traffic_handle = Some(handle.clone());
        self.handle = Some(handle);
        self.settings = settings;
        Ok(())
    }

    pub fn stop(&mut self) {
        self.handle = None;
        if let Some(worker) = &self.worker {
            let _ = worker.submit(Work::Configure(None));
        }
        self.pending.clear();
    }

    fn merge_polled(&mut self, mut inbox: Inbox, events: Vec<InboundEvent>) {
        self.pending.retain(|e| inbox.get(&e.id()).is_some());
        self.acknowledged.retain(|id| inbox.get(id).is_some());
        inbox.mark_forwarded(&self.acknowledged.iter().cloned().collect::<Vec<_>>());
        self.inbox = inbox;
        self.error = None;
        self.error_notified = None;
        if self.handle.is_some() {
            self.pending.extend(events);
            while self.pending.len() > 2000 {
                self.pending.pop_front();
            }
        }
    }

    fn acknowledge(&mut self, batch: &[InboundEvent]) {
        let ids: Vec<_> = batch.iter().map(InboundEvent::id).collect();
        self.acknowledged.extend(ids.iter().cloned());
        self.inbox.mark_forwarded(&ids);
        if let Some(worker) = &self.worker {
            let _ = worker.submit(Work::Forwarded(ids));
        }
    }

    fn forward_done(
        &mut self,
        result: &ActionResult,
        now: i64,
        mut delivered: impl FnMut(AgentMessage),
    ) {
        if self
            .forwarding
            .as_ref()
            .is_none_or(|(id, _)| *id != result.id)
        {
            return;
        }
        let (_, batch) = self.forwarding.take().expect("matching action");
        if result.outcome.is_ok() {
            if self
                .shake_started_ms
                .is_none_or(|start| now - start >= SHAKE_COOLDOWN_MS)
            {
                self.shake_started_ms = Some(now);
            }
            for _ in &batch {
                delivered(AgentMessage::new(
                    "slack",
                    result.action.handle(),
                    MessageKind::Relay,
                ));
            }
            self.acknowledge(&batch);
        }
        if result.outcome.is_err() && self.handle.is_some() {
            for event in batch.into_iter().rev() {
                self.pending.push_front(event);
            }
            self.forward_after_ms = now + 30_000;
        }
    }

    fn observe_bridge(&mut self, bridge: BridgeStatus) -> bool {
        let notify = self.handle.is_some()
            && (!self.bridge_seen || self.bridge != bridge)
            && bridge != BridgeStatus::Ok;
        self.bridge = bridge;
        self.bridge_seen = self.handle.is_some();
        notify
    }

    pub fn apply_status(&self, status: &mut EnvoyStatus) {
        status.bridge = self.bridge;
        status.pending_drafts = self.pending_count();
        status.dry_run = self.settings.dry_run;
    }

    pub fn pending_count(&self) -> usize {
        self.drafts.as_ref().map_or(0, DraftQueue::pending_count)
    }

    pub fn drafts(&self) -> &[Draft] {
        self.drafts.as_ref().map_or(&[], DraftQueue::drafts)
    }
    pub fn relays(&self) -> &[crate::envoy::relay::RelayRecord] {
        self.relays.records()
    }
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }
    pub fn settings(&self) -> &EnvoySettings {
        &self.settings
    }
    pub fn configure(&mut self, settings: EnvoySettings) -> Result<(), String> {
        if self.handle.is_some() {
            self.worker()?
                .submit(Work::Configure(Some(settings.clone())))?;
        }
        self.settings = settings;
        Ok(())
    }
    fn worker(&self) -> Result<&Worker, String> {
        self.worker.as_ref().ok_or_else(|| {
            self.error
                .clone()
                .unwrap_or("Envoy worker unavailable".into())
        })
    }
    fn request(&mut self, command: &EnvoyCommand) -> Result<Value, String> {
        crate::envoy::authorize(self, &command.origin)?;
        let method = command.method.as_str();
        let args = &command.args;
        match method {
            "envoy_inbox" => {
                let since = optional(args, "since")?;
                if let Some(ts) = since {
                    crate::envoy::slack::validate_ts(ts)?;
                }
                let limit = match args.get("limit") {
                    None => 100,
                    Some(v) => v
                        .as_u64()
                        .filter(|n| (1..=2000).contains(n))
                        .ok_or("limit must be between 1 and 2000")?
                        as usize,
                };
                Ok(json!({"events":self.inbox.since(since, limit)}))
            }
            "envoy_drafts" => {
                let status = optional(args, "status")?
                    .map(|s| {
                        serde_json::from_value::<DraftStatus>(json!(s))
                            .map_err(|_| "Unknown draft status")
                    })
                    .transpose()?;
                Ok(
                    json!({"drafts":self.drafts().iter().filter(|d| status.is_none_or(|s| s==d.status)).collect::<Vec<_>>()}),
                )
            }
            "envoy_draft" | "envoy_propose_task" => {
                let draft = if method == "envoy_draft" {
                    let target: ReplyTo = serde_json::from_value(args["in_reply_to"].clone())
                        .map_err(|_| "in_reply_to requires conversation and ts")?;
                    let event = self
                        .inbox
                        .get(&format!("{}:{}", target.conversation, target.ts))
                        .ok_or("Draft must reply to a retained inbox event")?;
                    let thread_ts = optional(args, "thread_ts")?
                        .map(str::to_owned)
                        .or_else(|| event.thread_ts.clone());
                    let mut draft = Draft::reply(
                        target,
                        thread_ts,
                        required(args, "text")?.into(),
                        required(args, "rationale")?.into(),
                        now_ms(),
                    );
                    draft.slack_link = event.permalink.clone();
                    draft
                } else {
                    Draft::proposal(
                        required(args, "summary")?.into(),
                        optional(args, "repo")?.map(str::to_owned),
                        required(args, "why")?.into(),
                        required(args, "slack_link")?.into(),
                        now_ms(),
                    )
                };
                let draft = self.queue()?.create(draft)?;
                Ok(json!({"draft":draft}))
            }
            _ => Err("Unknown Envoy method".into()),
        }
    }

    fn envoy_approve(
        &mut self,
        id: &str,
        edited_text: Option<&str>,
        queen_ready: bool,
        queen: impl FnOnce(&str) -> bool,
    ) -> Result<bool, String> {
        let proposal = self
            .drafts()
            .iter()
            .any(|d| d.id == id && d.kind == DraftKind::TaskProposal);
        if !self.settings.dry_run {
            if proposal && !queen_ready {
                return Err("Queen chat is busy; try approval when she is ready".into());
            }
            if !proposal {
                self.worker()?;
            }
        }
        let queue = self
            .drafts
            .as_mut()
            .ok_or("Envoy draft store unavailable")?;
        apply_approval(
            queue,
            id,
            edited_text,
            self.settings.dry_run,
            now_ms(),
            |draft| {
                if draft.kind == DraftKind::TaskProposal {
                    if queen(&draft.queen_request()) {
                        Ok(true)
                    } else {
                        Err("Queen chat refused the request".into())
                    }
                } else {
                    let worker = self.worker.as_ref().ok_or("Envoy worker unavailable")?;
                    worker.submit(Work::Send {
                        id: id.into(),
                        message: draft.outbound()?,
                    })?;
                    if let Some(from) = &self.traffic_handle {
                        self.outgoing.insert(id.into(), from.clone());
                    }
                    Ok(false)
                }
            },
        )
    }

    fn poll_updates(&mut self) -> (Vec<Incoming>, Vec<AgentMessage>) {
        let mut notifications = Vec::new();
        let mut traffic = Vec::new();
        let updates = self.worker.as_ref().map_or_else(Vec::new, Worker::updates);
        for update in updates {
            match update {
                Update::Polled {
                    inbox,
                    events,
                    bridge,
                    notices,
                } => {
                    self.merge_polled(inbox, events);
                    if self.observe_bridge(bridge) {
                        notifications.push(Incoming::new(
                            Source::Envoy,
                            Severity::Warn,
                            "Envoy bridge down",
                            "Slack channel ingestion is falling back to polling.",
                        ));
                    }
                    let current: HashSet<String> = notices.into_iter().collect();
                    for notice in current
                        .difference(&self.notices)
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        notifications.push(Incoming::new(
                            Source::Envoy,
                            Severity::Warn,
                            "Envoy ingestion",
                            &notice,
                        ));
                    }
                    self.notices = current;
                }
                Update::Sent { id, result } => {
                    if let Err(error) = &result {
                        notifications.push(
                            Incoming::new(
                                Source::Envoy,
                                Severity::Warn,
                                "Envoy reply failed",
                                error,
                            )
                            .with_target(DRAFTS_TARGET),
                        );
                    }
                    if let Some(from) = self.outgoing.remove(&id)
                        && result.is_ok()
                    {
                        traffic.push(AgentMessage::new(from, "slack", MessageKind::Relay));
                    }
                    if let Err(e) = self.queue().and_then(|q| q.finish(&id, result, now_ms())) {
                        self.error = Some(e);
                    }
                }
                Update::Error(error) => {
                    self.error = Some(error);
                }
            }
        }
        if self.error != self.error_notified {
            if let Some(error) = self.error.clone() {
                notifications.push(Incoming::new(
                    Source::Envoy,
                    Severity::Warn,
                    "Envoy storage unavailable",
                    &error,
                ));
            }
            self.error_notified = self.error.clone();
        }
        (notifications, traffic)
    }

    fn next_forward(&mut self, now: i64) -> Option<(Action, Vec<InboundEvent>)> {
        if self.pending.is_empty() || self.forwarding.is_some() || now < self.forward_after_ms {
            return None;
        }
        let handle = self.handle.clone()?;
        let batch: Vec<_> = self.pending.drain(..self.pending.len().min(20)).collect();
        let text = batch
            .iter()
            .map(InboundEvent::summary)
            .collect::<Vec<_>>()
            .join(" | ");
        Some((
            Action::Send {
                handle,
                input: TerminalInput::line(text),
            },
            batch,
        ))
    }

    fn reserve_relay(
        &mut self,
        request: RelayRequest,
        ready: bool,
    ) -> Result<crate::envoy::relay::RelayRecord, String> {
        if !self.settings.dry_run && !ready {
            return Err(
                "Relay target must be idle at an empty agent prompt with no pending input".into(),
            );
        }
        let mut limiter = self.relays.clone();
        let record = limiter.reserve(request, self.settings.dry_run, now_ms())?;
        limiter.save(&self.relay_path)?;
        self.relays = limiter;
        Ok(record)
    }

    fn envoy_edit(&mut self, id: &str, text: &str) -> Result<(), String> {
        self.queue()?.edit(id, text, now_ms())
    }

    fn panel_action(
        &mut self,
        action: crate::ui::envoy::DraftAction,
        queen_ready: bool,
        queen: impl FnOnce(&str) -> bool,
    ) -> (String, Result<bool, String>) {
        match action {
            crate::ui::envoy::DraftAction::Approve { id, text } => {
                let result = self
                    .envoy_edit(&id, &text)
                    .and_then(|()| self.envoy_approve(&id, None, queen_ready, queen));
                (id, result)
            }
            crate::ui::envoy::DraftAction::Reject(id) => {
                let result = self
                    .queue()
                    .and_then(|q| q.reject(&id, now_ms()))
                    .map(|()| false);
                (id, result)
            }
        }
    }

    fn queue(&mut self) -> Result<&mut DraftQueue, String> {
        self.drafts
            .as_mut()
            .ok_or("Envoy draft store unavailable".into())
    }
}

impl MyrstackApp {
    pub(super) fn envoy_drafts_button(&mut self, ui: &mut egui::Ui) {
        if crate::ui::queen::envoy_drafts_button(ui, self.envoy_core.pending_count()) {
            self.envoy_core.open_request = true;
        }
    }

    pub(super) fn envoy_pane(&mut self, ui: &mut egui::Ui) {
        let action = self.envoy_panel.show(
            ui,
            self.envoy_core.drafts(),
            self.envoy_core.inbox(),
            self.envoy_core.settings().dry_run,
            self.envoy_core.error.as_deref(),
        );
        if let Some(action) = action {
            let ready = self.chat_ready();
            let chat = &mut self.chat;
            let (id, result) = self
                .envoy_core
                .panel_action(action, ready, |text| chat.session_mut().send(text));
            if matches!(result, Ok(true)) {
                self.open_chat();
            }
            self.envoy_panel.result(&id, result.map(|_| ()));
        }
    }

    pub(super) fn control_envoy(
        &mut self,
        _ctx: &egui::Context,
        command: EnvoyCommand,
        reply: Reply,
    ) -> Result<(), ControlError> {
        if let Err(e) = crate::envoy::authorize(&self.envoy_core, &command.origin) {
            let error = ControlError::new(ErrorCode::Refused, e);
            reply.send(Err(error.clone()));
            return Err(error);
        }
        if command.method == "envoy_event" {
            let result = required(&command.args, "id").and_then(|id| {
                self.envoy_core
                    .inbox
                    .get(id)
                    .cloned()
                    .ok_or("Unknown event".into())
            });
            match result {
                Ok(event) => match self.envoy_core.worker() {
                    Ok(worker) => {
                        let _ = worker.submit(Work::Thread { event, reply });
                    }
                    Err(error) => reply.send(Err(ControlError::new(ErrorCode::Unavailable, error))),
                },
                Err(error) => reply.send(Err(ControlError::invalid_params(error))),
            }
            return Ok(());
        }
        if command.method == "envoy_relay" {
            let result = serde_json::from_value::<RelayRequest>(command.args.clone())
                .map_err(|_| "Relay requires to, summary, slack_link and why".to_owned())
                .and_then(|mut request| {
                    if request.to == "coordinators" {
                        let snapshot =
                            self.snapshot.as_ref().ok_or("Fleet snapshot unavailable")?;
                        let selection = crate::broadcast::BroadcastSelection::new(
                            snapshot.runs.iter().map(|r| r.id.clone()),
                        );
                        if crate::broadcast::plan_broadcast(snapshot, &selection, &request.text())
                            .deliveries
                            .is_empty()
                        {
                            return Err("No live coordinators".into());
                        }
                    } else {
                        let node = self.control_node(&request.to).map_err(|e| e.to_string())?;
                        request.to = crate::ui::terminal::watch_handle(node)
                            .ok_or("Relay target has no live terminal")?
                            .into();
                    }
                    let ready =
                        request.to == "coordinators" || self.envoy_target_ready(&request.to);
                    self.envoy_core.reserve_relay(request, ready)
                });
            return match result {
                Ok(record) if record.dry_run => {
                    reply.send(Ok(json!({"dry_run":true,"relay":record})));
                    Ok(())
                }
                Ok(record) => self.deliver_envoy_relay(&record.request, reply),
                Err(error) => {
                    let error = ControlError::new(ErrorCode::Refused, error);
                    reply.send(Err(error.clone()));
                    Err(error)
                }
            };
        }
        let result = self
            .envoy_core
            .request(&command)
            .map_err(ControlError::invalid_params);
        if result.is_ok()
            && matches!(
                command.method.as_str(),
                "envoy_draft" | "envoy_propose_task"
            )
        {
            self.raise(
                Incoming::new(
                    Source::Envoy,
                    Severity::Info,
                    "Envoy draft ready",
                    "A draft is waiting for your approval.",
                )
                .with_target(DRAFTS_TARGET),
            );
        }
        let error = result.as_ref().err().cloned();
        reply.send(result);
        error.map_or(Ok(()), Err)
    }

    fn deliver_envoy_relay(
        &mut self,
        request: &RelayRequest,
        reply: Reply,
    ) -> Result<(), ControlError> {
        let from = self.envoy_core.handle.clone().expect("authorized Envoy");
        if request.to == "coordinators" {
            let Some(snapshot) = &self.snapshot else {
                let error = ControlError::new(ErrorCode::Unavailable, "Fleet snapshot unavailable");
                reply.send(Err(error.clone()));
                return Err(error);
            };
            let selection = crate::broadcast::BroadcastSelection::new(
                snapshot.runs.iter().map(|r| r.id.clone()),
            );
            let mut plan = crate::broadcast::plan_broadcast(snapshot, &selection, &request.text());
            crate::envoy::relay::attribute_broadcast(&mut plan, &from, &request.text());
            if plan.deliveries.is_empty() {
                let error = ControlError::not_found("No live coordinators");
                reply.send(Err(error.clone()));
                return Err(error);
            }
            let traffic = self.traffic.sender();
            std::thread::spawn(move || {
                let mut sender = crate::orca::OrcaClient::new();
                reply.send(Ok(crate::envoy::relay::deliver_broadcast(
                    &mut sender,
                    &plan,
                    &traffic,
                    &from,
                )));
            });
            return Ok(());
        }
        let target = self.control_node(&request.to).and_then(|node| {
            crate::ui::terminal::watch_handle(node)
                .map(str::to_owned)
                .ok_or_else(|| {
                    ControlError::new(ErrorCode::Refused, "Relay target has no live terminal")
                })
        });
        let handle = match target {
            Ok(handle) => handle,
            Err(error) => {
                reply.send(Err(error.clone()));
                return Err(error);
            }
        };
        let message = AgentMessage::new(from, handle.clone(), MessageKind::Relay);
        let ids = self.submit_control_actions(
            vec![Action::Send {
                handle: handle.clone(),
                input: TerminalInput::line(request.text()),
            }],
            reply,
            json!({"handle":handle,"sent":"relay"}),
            false,
        );
        for id in ids {
            self.envoy_core.relay_actions.insert(id, message.clone());
        }
        Ok(())
    }

    pub(super) fn poll_envoy(&mut self) {
        let (notifications, traffic) = self.envoy_core.poll_updates();
        for notice in notifications {
            self.raise(notice);
        }
        for message in traffic {
            self.traffic.sender().send(message);
        }
        self.forward_envoy();
    }

    fn envoy_target_ready(&self, handle: &str) -> bool {
        let Some(terminal) = self.data.as_ref().and_then(|d| d.terminal(handle)) else {
            return false;
        };
        let idle = self.snapshot.as_ref().is_some_and(|s| {
            s.nodes.iter().any(|n| {
                n.terminal_handle.as_deref() == Some(handle)
                    && n.live
                    && n.state == crate::model::NodeState::Idle
            })
        });
        can_forward(
            terminal,
            idle,
            self.input.text_pending(handle)
                || self.control.typing_pending(handle)
                || !self.input.draft(handle).is_empty(),
        )
    }

    fn forward_envoy(&mut self) {
        if !self
            .envoy_core
            .handle
            .as_deref()
            .is_some_and(|handle| self.envoy_target_ready(handle))
        {
            return;
        }
        if let Some((action, batch)) = self.envoy_core.next_forward(now_ms()) {
            let id = self.actions.submit(action);
            self.envoy_core.forwarding = Some((id, batch));
        }
    }

    pub(super) fn envoy_action_done(&mut self, result: &ActionResult) {
        if let Some(message) = self.envoy_core.relay_actions.remove(&result.id)
            && result.outcome.is_ok()
        {
            self.traffic.sender().send(message);
        }
        let traffic = self.traffic.sender();
        self.envoy_core
            .forward_done(result, now_ms(), |message| traffic.send(message));
    }
}

/// `deliver` runs only after a durable live approval; false means completion is asynchronous.
fn apply_approval(
    queue: &mut DraftQueue,
    id: &str,
    edited_text: Option<&str>,
    dry_run: bool,
    now: i64,
    deliver: impl FnOnce(&Draft) -> Result<bool, String>,
) -> Result<bool, String> {
    let draft = queue.begin_approval(id, edited_text, dry_run, now)?;
    if draft.status == DraftStatus::DryRunSent {
        return Ok(false);
    }
    match deliver(&draft) {
        Ok(true) => {
            queue.finish(id, Ok(()), now)?;
            Ok(true)
        }
        Ok(false) => Ok(false),
        Err(error) => {
            queue.finish(id, Err(error.clone()), now)?;
            Err(error)
        }
    }
}

pub fn can_forward(terminal: &crate::orca::Terminal, idle: bool, typing: bool) -> bool {
    if !idle || typing || terminal.orphaned || !terminal.writable {
        return false;
    }
    let Some(preview) = terminal.preview.as_deref() else {
        return false;
    };
    if crate::detect::detect_in_preview(preview).is_some() {
        return false;
    }
    let live = match terminal.agent_identity {
        Some(crate::orca::AgentIdentity::Claude) => crate::detect::claude_session_live(terminal),
        Some(crate::orca::AgentIdentity::Codex) => preview.lines().any(|l| l.trim() == "›"),
        _ => false,
    };
    let composer = preview
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('❯') || l.starts_with('›'));
    live && composer.is_some_and(|l| l.chars().skip(1).all(char::is_whitespace))
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    optional(args, key)?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} is required"))
}
fn optional<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        _ => Err(format!("{key} must be a string")),
    }
}

#[cfg(test)]
mod tests;

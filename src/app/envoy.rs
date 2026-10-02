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
        Self::at(crate::claude::hook::data_dir(), ctx)
    }

    pub fn at(dir: PathBuf, ctx: &egui::Context) -> Self {
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
        let wake = ctx.clone();
        let worker = error
            .is_none()
            .then(|| Worker::spawn(inbox_path, inbox.clone(), move || wake.request_repaint()));
        Self {
            handle: None,
            settings: EnvoySettings::default(),
            inbox,
            drafts: drafts.ok(),
            worker,
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
        status.pending_drafts = self.drafts.as_ref().map_or(0, DraftQueue::pending_count);
        status.dry_run = self.settings.dry_run;
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
    fn queue(&mut self) -> Result<&mut DraftQueue, String> {
        self.drafts
            .as_mut()
            .ok_or("Envoy draft store unavailable".into())
    }
}

impl MyrstackApp {
    pub fn envoy_approve(&mut self, id: &str, edited_text: Option<&str>) -> Result<(), String> {
        let dry_run = self.envoy_core.settings.dry_run;
        let proposal = self
            .envoy_core
            .drafts()
            .iter()
            .any(|d| d.id == id && d.kind == DraftKind::TaskProposal);
        if !dry_run {
            if proposal && !self.chat_ready() {
                return Err("Queen chat is busy; try approval when she is ready".into());
            }
            if !proposal {
                self.envoy_core.worker()?;
            }
        }
        let core = &mut self.envoy_core;
        let queue = core
            .drafts
            .as_mut()
            .ok_or("Envoy draft store unavailable")?;
        let chat = &mut self.chat;
        let completed = apply_approval(queue, id, edited_text, dry_run, now_ms(), |draft| {
            if draft.kind == DraftKind::TaskProposal {
                if chat.session_mut().send(&draft.queen_request()) {
                    Ok(true)
                } else {
                    Err("Queen chat refused the request".into())
                }
            } else {
                let worker = core.worker.as_ref().ok_or("Envoy worker unavailable")?;
                worker.submit(Work::Send {
                    id: id.into(),
                    message: draft.outbound()?,
                })?;
                if let Some(from) = &core.traffic_handle {
                    core.outgoing.insert(id.into(), from.clone());
                }
                Ok(false)
            }
        })?;
        if completed && proposal {
            self.open_chat();
        }
        Ok(())
    }

    pub fn envoy_reject(&mut self, id: &str) -> Result<(), String> {
        self.envoy_core.queue()?.reject(id, now_ms())
    }
    pub fn envoy_edit(&mut self, id: &str, text: &str) -> Result<(), String> {
        self.envoy_core.queue()?.edit(id, text, now_ms())
    }
    pub fn envoy_drafts(&self) -> &[Draft] {
        self.envoy_core.drafts()
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
                    let mut limiter = self.envoy_core.relays.clone();
                    let record =
                        limiter.reserve(request, self.envoy_core.settings.dry_run, now_ms())?;
                    limiter.save(&self.envoy_core.relay_path)?;
                    self.envoy_core.relays = limiter;
                    Ok(record)
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
            .envoy_request(&command.method, &command.args)
            .map_err(ControlError::invalid_params);
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

    fn envoy_request(&mut self, method: &str, args: &Value) -> Result<Value, String> {
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
                Ok(json!({"events":self.envoy_core.inbox.since(since, limit)}))
            }
            "envoy_drafts" => {
                let status = optional(args, "status")?
                    .map(|s| {
                        serde_json::from_value::<DraftStatus>(json!(s))
                            .map_err(|_| "Unknown draft status")
                    })
                    .transpose()?;
                Ok(
                    json!({"drafts":self.envoy_core.drafts().iter().filter(|d| status.is_none_or(|s| s==d.status)).collect::<Vec<_>>()}),
                )
            }
            "envoy_draft" | "envoy_propose_task" => {
                let draft = if method == "envoy_draft" {
                    let target: ReplyTo = serde_json::from_value(args["in_reply_to"].clone())
                        .map_err(|_| "in_reply_to requires conversation and ts")?;
                    let event = self
                        .envoy_core
                        .inbox
                        .get(&format!("{}:{}", target.conversation, target.ts))
                        .ok_or("Draft must reply to a retained inbox event")?;
                    let thread_ts = optional(args, "thread_ts")?
                        .map(str::to_owned)
                        .or_else(|| event.thread_ts.clone());
                    Draft::reply(
                        target,
                        thread_ts,
                        required(args, "text")?.into(),
                        required(args, "rationale")?.into(),
                        now_ms(),
                    )
                } else {
                    Draft::proposal(
                        required(args, "summary")?.into(),
                        optional(args, "repo")?.map(str::to_owned),
                        required(args, "why")?.into(),
                        required(args, "slack_link")?.into(),
                        now_ms(),
                    )
                };
                let draft = self.envoy_core.queue()?.create(draft)?;
                self.raise(
                    Incoming::new(
                        Source::Envoy,
                        Severity::Info,
                        "Envoy draft ready",
                        "A draft is waiting for your approval.",
                    )
                    .with_target(DRAFTS_TARGET),
                );
                Ok(json!({"draft":draft}))
            }
            _ => Err("Unknown Envoy method".into()),
        }
    }

    pub(super) fn poll_envoy(&mut self) {
        let updates = self
            .envoy_core
            .worker
            .as_ref()
            .map_or_else(Vec::new, Worker::updates);
        for update in updates {
            match update {
                Update::Polled {
                    inbox,
                    events,
                    bridge,
                    notices,
                } => {
                    self.envoy_core
                        .pending
                        .retain(|e| inbox.get(&e.id()).is_some());
                    let mut inbox = inbox;
                    self.envoy_core
                        .acknowledged
                        .retain(|id| inbox.get(id).is_some());
                    inbox.mark_forwarded(
                        &self
                            .envoy_core
                            .acknowledged
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>(),
                    );
                    self.envoy_core.inbox = inbox;
                    self.envoy_core.error = None;
                    self.envoy_core.error_notified = None;
                    if self.envoy_core.handle.is_some() {
                        self.envoy_core.pending.extend(events);
                        while self.envoy_core.pending.len() > 2000 {
                            self.envoy_core.pending.pop_front();
                        }
                    }
                    if self.envoy_core.observe_bridge(bridge) {
                        self.raise(Incoming::new(
                            Source::Envoy,
                            Severity::Warn,
                            "Envoy bridge down",
                            "Slack channel ingestion is falling back to polling.",
                        ));
                    }
                    let current: HashSet<String> = notices.into_iter().collect();
                    for notice in current
                        .difference(&self.envoy_core.notices)
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        self.raise(Incoming::new(
                            Source::Envoy,
                            Severity::Warn,
                            "Envoy ingestion",
                            &notice,
                        ));
                    }
                    self.envoy_core.notices = current;
                }
                Update::Sent { id, result } => {
                    if let Some(from) = self.envoy_core.outgoing.remove(&id)
                        && result.is_ok()
                    {
                        self.traffic.sender().send(AgentMessage::new(
                            from,
                            "slack",
                            MessageKind::Relay,
                        ));
                    }
                    if let Err(e) = self
                        .envoy_core
                        .queue()
                        .and_then(|q| q.finish(&id, result, now_ms()))
                    {
                        self.envoy_core.error = Some(e);
                    }
                }
                Update::Error(error) => {
                    self.envoy_core.error = Some(error);
                }
            }
        }
        if self.envoy_core.error != self.envoy_core.error_notified {
            if let Some(error) = self.envoy_core.error.clone() {
                self.raise(Incoming::new(
                    Source::Envoy,
                    Severity::Warn,
                    "Envoy storage unavailable",
                    &error,
                ));
            }
            self.envoy_core.error_notified = self.envoy_core.error.clone();
        }
        self.forward_envoy();
    }

    fn forward_envoy(&mut self) {
        let core = &mut self.envoy_core;
        if core.pending.is_empty() || core.forwarding.is_some() || now_ms() < core.forward_after_ms
        {
            return;
        }
        let Some(handle) = core.handle.as_deref() else {
            return;
        };
        let Some(terminal) = self.data.as_ref().and_then(|d| d.terminal(handle)) else {
            return;
        };
        let idle = self.snapshot.as_ref().is_some_and(|s| {
            s.nodes.iter().any(|n| {
                n.terminal_handle.as_deref() == Some(handle)
                    && n.live
                    && n.state == crate::model::NodeState::Idle
            })
        });
        if !can_forward(
            terminal,
            idle,
            self.input.text_pending(handle)
                || self.control.typing_pending(handle)
                || !self.input.draft(handle).is_empty(),
        ) {
            return;
        }
        let batch: Vec<_> = core.pending.drain(..core.pending.len().min(20)).collect();
        let text = batch
            .iter()
            .map(InboundEvent::summary)
            .collect::<Vec<_>>()
            .join(" | ");
        let id = self.actions.submit(Action::Send {
            handle: handle.into(),
            input: TerminalInput::line(text),
        });
        core.forwarding = Some((id, batch));
    }

    pub(super) fn envoy_action_done(&mut self, result: &ActionResult) {
        if let Some(message) = self.envoy_core.relay_actions.remove(&result.id)
            && result.outcome.is_ok()
        {
            self.traffic.sender().send(message);
        }
        if self
            .envoy_core
            .forwarding
            .as_ref()
            .is_none_or(|(id, _)| *id != result.id)
        {
            return;
        }
        let (_, batch) = self.envoy_core.forwarding.take().expect("matching action");
        if result.outcome.is_ok() {
            for _ in &batch {
                self.traffic.sender().send(AgentMessage::new(
                    "slack",
                    result.action.handle(),
                    MessageKind::Relay,
                ));
            }
            let ids: Vec<_> = batch.iter().map(InboundEvent::id).collect();
            self.envoy_core.acknowledged.extend(ids.iter().cloned());
            self.envoy_core.inbox.mark_forwarded(&ids);
            if let Some(worker) = &self.envoy_core.worker {
                let _ = worker.submit(Work::Forwarded(ids));
            }
        }
        if result.outcome.is_err() && self.envoy_core.handle.is_some() {
            for event in batch.into_iter().rev() {
                self.envoy_core.pending.push_front(event);
            }
            self.envoy_core.forward_after_ms = now_ms() + 30_000;
        }
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
mod tests {
    use super::*;
    use crate::orca::{AgentIdentity, Terminal};
    #[test]
    fn forwarding_waits_for_idle_empty_composer_and_live_agent() {
        let mut t = Terminal {
            agent_identity: Some(AgentIdentity::Claude),
            writable: true,
            preview: Some("Claude Code\n❯ prior task\nResponse\n❯\n? for shortcuts".into()),
            ..Terminal::default()
        };
        assert!(can_forward(&t, true, false));
        assert!(!can_forward(&t, false, false));
        assert!(!can_forward(&t, true, true));
        t.preview = Some("Claude Code\n❯ unfinished user text".into());
        assert!(!can_forward(&t, true, false));
        t.preview = Some("Do you want to proceed?\n❯ 1. Yes\n  2. No".into());
        assert!(!can_forward(&t, true, false));
        t.preview = Some("Claude Code\nuser@host ~ %".into());
        assert!(!can_forward(&t, true, false));
        t.agent_identity = Some(AgentIdentity::Codex);
        t.preview = Some("OpenAI Codex\n› prior task\nAnswer\n›".into());
        assert!(can_forward(&t, true, false));
        t.orphaned = true;
        assert!(!can_forward(&t, true, false));
    }
    #[test]
    fn production_approval_transaction_never_calls_delivery_for_dry_run_or_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut queue = DraftQueue::load(dir.path().join("drafts.json")).unwrap();
        let reply = Draft::reply(
            ReplyTo {
                conversation: "C123".into(),
                ts: "1.000001".into(),
            },
            None,
            "Sample reply".into(),
            "Requested".into(),
            0,
        );
        let proposal = Draft::proposal(
            "Sample task".into(),
            None,
            "Requested".into(),
            "https://example.slack.com".into(),
            0,
        );
        for draft in [reply.clone(), proposal] {
            let draft = queue.create(draft).unwrap();
            apply_approval(&mut queue, &draft.id, Some("Edited"), true, 1, |_| {
                panic!("dry-run invoked live delivery")
            })
            .unwrap();
            assert_eq!(
                queue.get(&draft.id).unwrap().status,
                DraftStatus::DryRunSent
            );
        }
        let draft = queue.create(reply.clone()).unwrap();
        assert!(
            apply_approval(
                &mut queue,
                &draft.id,
                Some("ghp_123456789012345678901234567890"),
                false,
                1,
                |_| panic!("secret reached live delivery")
            )
            .is_err()
        );
        assert_eq!(queue.get(&draft.id).unwrap().status, DraftStatus::Pending);
        apply_approval(&mut queue, &draft.id, None, false, 2, |d| {
            assert_eq!(d.status, DraftStatus::Approved);
            Ok(false)
        })
        .unwrap();
        assert_eq!(queue.get(&draft.id).unwrap().status, DraftStatus::Approved);
        let draft = queue.create(reply).unwrap();
        assert!(
            apply_approval(&mut queue, &draft.id, None, false, 3, |_| Err(
                "offline".into()
            ))
            .is_err()
        );
        assert_eq!(queue.get(&draft.id).unwrap().status, DraftStatus::Failed);
    }

    #[test]
    fn bridge_alerts_on_start_reenable_and_each_down_transition() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppEnvoy::at(dir.path().into(), &egui::Context::default());
        assert!(!core.observe_bridge(BridgeStatus::Missing));
        core.handle = Some("term_envoy".into());
        assert!(core.observe_bridge(BridgeStatus::Missing));
        assert!(!core.observe_bridge(BridgeStatus::Missing));
        assert!(!core.observe_bridge(BridgeStatus::Ok));
        assert!(core.observe_bridge(BridgeStatus::Stale));
        core.bridge_seen = false;
        assert!(core.observe_bridge(BridgeStatus::Stale));
    }
}

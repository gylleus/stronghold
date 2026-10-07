//! App integration for ingestion, approvals, relays and event forwarding.

use eframe::egui;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use super::{MyrstackApp, now_ms};
use crate::actions::{Action, ActionId, ActionResult};
use crate::control::{ControlError, ErrorCode, Reply};
use crate::envoy::channel::{EventKind, InboundEvent};
use crate::envoy::context::{CLEAR_COMMAND, ContextBudget, ResetReason, handoff_preamble};
use crate::envoy::control::EnvoyCommand;
use crate::envoy::drafts::{Draft, DraftKind, DraftQueue, DraftStatus, ReplyTo};
use crate::envoy::inbox::{Inbox, timestamp_us};
use crate::envoy::matching::{self, Ant};
use crate::envoy::notes::ThreadNotes;
use crate::envoy::relay::{RelayLimiter, RelayRequest};
use crate::envoy::settings::EnvoySettings;
use crate::envoy::worker::{Update, Work, Worker};
use crate::envoy::{BridgeStatus, EnvoyIdentity, EnvoyStatus, POLL_STALE_MS, STUCK_AFTER_MS};
use crate::model::NodeKind;
use crate::notify::{Incoming, Severity, Source};
use crate::orca::TerminalInput;
use crate::traffic::{AgentMessage, MessageKind};

mod gateway;
use crate::envoy::requests::{Requester, Requests};
use gateway::GatewayAction;

pub const DRAFTS_TARGET: &str = "envoy:drafts";
const SHAKE_COOLDOWN_MS: i64 = 8_000;
/// A stuck or stale warning is repeated this often while the condition lasts.
const RENOTIFY_MS: i64 = 30 * 60_000;
/// Events older than this at forwarding time collapse into one backlog line.
const BACKLOG_AFTER_MS: i64 = 30 * 60_000;

pub struct AppEnvoy {
    handle: Option<String>,
    settings: EnvoySettings,
    inbox: Inbox,
    drafts: Option<DraftQueue>,
    requests: Option<Requests>,
    notes: Option<ThreadNotes>,
    notes_path: PathBuf,
    /// What the session's context holds since its last fresh start.
    context: ContextBudget,
    /// The `/clear` in flight.
    resetting: Option<ActionId>,
    watches: Option<Vec<crate::envoy::slack::threads::Watch>>,
    watches_pending: bool,
    watch_retry_ms: i64,
    cleanup_retry_ms: i64,
    dispatch_blocked: bool,
    gateway_actions: HashMap<ActionId, GatewayAction>,
    gateway_retry_ms: i64,
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
    blocked: Option<&'static str>,
    /// Since when events have waited with nothing typed (see [`STUCK_AFTER_MS`]).
    pending_since_ms: Option<i64>,
    /// The last poll result's time, or the session's enabling; a silent loop is stale.
    last_polled_ms: Option<i64>,
    stuck_notified_ms: Option<i64>,
    stale_notified_ms: Option<i64>,
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
        if let Err(e) = core.reconcile_requests() {
            core.error = Some(e);
        }
        core
    }

    fn load(dir: PathBuf) -> Self {
        let inbox_path = dir.join(crate::envoy::inbox::INBOX_FILE);
        let loaded = Inbox::load(&inbox_path);
        let drafts = DraftQueue::load(dir.join(crate::envoy::drafts::DRAFTS_FILE));
        let requests = Requests::load(dir.join(crate::envoy::requests::REQUESTS_FILE));
        let relay_path = dir.join("envoy-relays.json");
        let relays = RelayLimiter::load(&relay_path);
        let notes_path = dir.join(crate::envoy::notes::NOTES_FILE);
        let notes = ThreadNotes::load(&notes_path)
            .map_err(|e| format!("Cannot read {}: {e}", notes_path.display()));
        let error = loaded
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .or_else(|| drafts.as_ref().err().cloned())
            .or_else(|| relays.as_ref().err().cloned())
            .or_else(|| requests.as_ref().err().cloned())
            .or_else(|| notes.as_ref().err().cloned());
        let inbox = loaded.unwrap_or_default();
        Self {
            handle: None,
            settings: EnvoySettings::default(),
            inbox,
            drafts: drafts.ok(),
            requests: requests.ok(),
            notes: notes.ok(),
            notes_path,
            context: ContextBudget::default(),
            resetting: None,
            watches: None,
            watches_pending: false,
            watch_retry_ms: 0,
            cleanup_retry_ms: 0,
            dispatch_blocked: false,
            gateway_actions: HashMap::new(),
            gateway_retry_ms: 0,
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
            blocked: None,
            pending_since_ms: None,
            last_polled_ms: None,
            stuck_notified_ms: None,
            stale_notified_ms: None,
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
        if self.handle.as_deref() != Some(&handle) {
            // A new session starts from its launch prompt: nothing to clear yet.
            self.context = ContextBudget::default();
            self.resetting = None;
            self.last_polled_ms = Some(now_ms());
            self.pending_since_ms = None;
            self.stuck_notified_ms = None;
            self.stale_notified_ms = None;
        }
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
        settings.validate_policies()?;
        self.settings = settings;
        self.reconcile_requests()?;
        Ok(())
    }

    pub fn stop(&mut self) {
        self.handle = None;
        if let Some(worker) = &self.worker {
            let _ = worker.submit(Work::Configure(None));
        }
        self.pending.clear();
        self.context = ContextBudget::default();
        self.resetting = None;
        self.pending_since_ms = None;
        self.last_polled_ms = None;
    }

    /// Starts or clears the stuck clock as events wait or stop waiting.
    fn note_pending(&mut self, now: i64) {
        if self.pending.is_empty() {
            self.pending_since_ms = None;
        } else if self.pending_since_ms.is_none() {
            self.pending_since_ms = Some(now);
        }
    }

    /// Events that have waited past [`STUCK_AFTER_MS`] with nothing typed in between.
    pub fn stuck_events(&self, now: i64) -> usize {
        if self.handle.is_none() {
            return 0;
        }
        match self.pending_since_ms {
            Some(since) if now.saturating_sub(since) >= STUCK_AFTER_MS => self.pending.len(),
            _ => 0,
        }
    }

    /// No poll result for [`POLL_STALE_MS`] while the Envoy is enabled.
    pub fn poll_stale(&self, now: i64) -> bool {
        self.handle.is_some()
            && self
                .last_polled_ms
                .is_some_and(|at| now.saturating_sub(at) >= POLL_STALE_MS)
    }

    /// Warnings for a stall, once when it starts and again every [`RENOTIFY_MS`].
    fn stall_notices(&mut self, now: i64) -> Vec<Incoming> {
        let mut out = Vec::new();
        let due = |notified: Option<i64>| notified.is_none_or(|at| now - at >= RENOTIFY_MS);
        let stuck = self.stuck_events(now);
        if stuck > 0 {
            if due(self.stuck_notified_ms) {
                let waited = now.saturating_sub(self.pending_since_ms.unwrap_or(now)) / 60_000;
                let why = self.blocked.unwrap_or("waiting for the terminal");
                out.push(
                    Incoming::new(
                        Source::Envoy,
                        Severity::Warn,
                        "Envoy events stuck",
                        &format!(
                            "{stuck} event(s) have waited {waited} min without reaching the Envoy terminal: {why}."
                        ),
                    )
                    .with_target(DRAFTS_TARGET),
                );
                self.stuck_notified_ms = Some(now);
            }
        } else {
            self.stuck_notified_ms = None;
        }
        if self.poll_stale(now) {
            if due(self.stale_notified_ms) {
                let quiet = now.saturating_sub(self.last_polled_ms.unwrap_or(now)) / 60_000;
                out.push(Incoming::new(
                    Source::Envoy,
                    Severity::Warn,
                    "Envoy poll stale",
                    &format!(
                        "No Slack poll result for {quiet} min: the Envoy's ingestion loop is wedged, so mentions, DMs and channel events are not being read."
                    ),
                ));
                self.stale_notified_ms = Some(now);
            }
        } else {
            self.stale_notified_ms = None;
        }
        out
    }

    pub fn context(&self) -> &ContextBudget {
        &self.context
    }

    /// The note for an event's thread, else its conversation.
    pub(super) fn note_for(
        &self,
        event: &InboundEvent,
    ) -> Option<&crate::envoy::notes::ThreadNote> {
        self.notes.as_ref()?.for_event(event)
    }

    /// ` — note: …` for a typed event line, when its thread has one.
    fn note_suffix(&self, event: &InboundEvent) -> String {
        self.note_for(event)
            .map(|n| format!(" — note: {}", n.text))
            .unwrap_or_default()
    }

    /// The first line typed after a reset, so a fresh context knows where it stands.
    pub(super) fn handoff(&self, text: String) -> String {
        if self.context.fresh {
            format!("{} | {text}", handoff_preamble(self.settings.dry_run))
        } else {
            text
        }
    }

    /// Why the context must not be cleared right now, or `None`.
    pub(super) fn in_flight(&self) -> Option<&'static str> {
        if self.forwarding.is_some() {
            Some("batch in flight")
        } else if !self.gateway_actions.is_empty() {
            Some("gateway send in flight")
        } else if !self.relay_actions.is_empty() {
            Some("relay in flight")
        } else if !self.outgoing.is_empty() {
            Some("Slack send in flight")
        } else if self
            .drafts()
            .iter()
            .any(|d| d.status == DraftStatus::Approved)
        {
            Some("approved draft being sent")
        } else {
            None
        }
    }

    /// The `/clear` to type before the next batch when a reset is due and nothing is in
    /// flight. `context_tokens` is the session's context size as of its last turn.
    pub(super) fn reset_action(
        &self,
        now: i64,
        context_tokens: Option<u64>,
    ) -> Option<(Action, ResetReason)> {
        if self.resetting.is_some() || self.in_flight().is_some() {
            return None;
        }
        let handle = self.handle.clone()?;
        let reason = self
            .context
            .reset_due(now, &self.settings, context_tokens)?;
        Some((
            Action::Send {
                handle,
                input: TerminalInput::line(CLEAR_COMMAND),
            },
            reason,
        ))
    }

    /// The `/clear` finished: a cleared context gets the handoff with its next batch, a
    /// failed one is retried after a backoff while batches go into the old context.
    pub(super) fn reset_done(&mut self, result: &ActionResult, now: i64) {
        if self.resetting != Some(result.id) {
            return;
        }
        self.resetting = None;
        let ok = result.outcome.is_ok();
        self.context.reset_done(ok, now);
        // Let the cleared prompt settle before the batch, or hold back after a failure.
        let delay = if ok { 2_000 } else { 30_000 };
        self.forward_after_ms = self.forward_after_ms.max(now + delay);
        self.gateway_retry_ms = self.gateway_retry_ms.max(now + delay);
        if let Err(error) = &result.outcome {
            log::warn!("envoy: context reset failed, keeping the old context: {error}");
        }
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
        self.note_pending(now_ms());
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
            self.context.forwarded(batch.len(), now);
            // Progress: whatever still waits is measured from now.
            self.pending_since_ms = None;
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
        self.note_pending(now);
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
        let now = now_ms();
        status.stuck_events = self.stuck_events(now);
        status.poll_stale = self.poll_stale(now);
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
    pub(super) fn block_dispatch(&mut self, blocked: bool) {
        self.dispatch_blocked = blocked;
        if blocked {
            self.settings.dry_run = true;
            self.stop();
        }
    }

    pub fn configure(&mut self, settings: EnvoySettings) -> Result<(), String> {
        settings.validate_policies()?;
        let cancel: Vec<_> = self
            .drafts()
            .iter()
            .filter(|d| {
                d.status == DraftStatus::Pending
                    && d.auto_at_ms.is_some()
                    && d.destination().is_some_and(|c| {
                        matches!(
                            settings.approval(c),
                            crate::envoy::settings::ApprovalMode::Approve
                        )
                    })
            })
            .map(|d| d.id.clone())
            .collect();
        for id in cancel {
            self.queue()?.schedule(&id, None)?;
        }
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
        if crate::envoy::control::PUBLIC_METHODS.contains(&command.method.as_str()) {
            return self.gateway_request(command, Requester::from_origin(command.origin.clone())?);
        }
        crate::envoy::authorize(self, &command.origin)?;
        if [
            "envoy_request",
            "envoy_route",
            "envoy_decline",
            "envoy_link_reply",
        ]
        .contains(&command.method.as_str())
        {
            return self.gateway_envoy_request(command);
        }
        let method = command.method.as_str();
        let args = &command.args;
        match method {
            "envoy_inbox" => {
                let since = optional(args, "since")?;
                if let Some(ts) = since {
                    crate::envoy::slack::validate_ts(ts)?;
                }
                let limit = match args.get("limit") {
                    None => 20,
                    Some(v) => v
                        .as_u64()
                        .filter(|n| (1..=2000).contains(n))
                        .ok_or("limit must be between 1 and 2000")?
                        as usize,
                };
                let events = self.inbox.since(since, limit);
                let mut notes: Vec<_> = events.iter().filter_map(|e| self.note_for(e)).collect();
                notes.sort_by_key(|n| n.key());
                notes.dedup_by_key(|n| n.key());
                Ok(json!({"events":events, "notes":notes}))
            }
            "envoy_note" => {
                let conversation = required(args, "conversation")?;
                let thread_ts = optional(args, "thread_ts")?.filter(|s| !s.trim().is_empty());
                let text = args
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or("text must be a string")?;
                let notes = self.notes.as_mut().ok_or("Envoy note store unavailable")?;
                let note = notes.set(conversation, thread_ts, text, now_ms())?;
                notes
                    .save(&self.notes_path)
                    .map_err(|e| format!("Cannot save {}: {e}", self.notes_path.display()))?;
                Ok(json!({"note":note, "notes":notes.len()}))
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
                self.schedule_draft(&draft.id)?;
                Ok(json!({"draft":self.queue()?.get(&draft.id)}))
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
        if self.dispatch_blocked {
            return Err(
                "Dispatch paused after settings persistence failure; save settings again".into(),
            );
        }
        self.reconcile_requests()?;
        let draft = self.queue()?.get(id).cloned().ok_or("Unknown draft")?;
        if let Some(link) = &draft.request {
            let request = self.requests_mut()?.get(&link.id)?;
            request.next_turn(Some(link.turn_id))?;
            if let Some(thread) = &request.thread
                && (draft.conversation.as_deref() != Some(&thread.conversation)
                    || draft.thread_ts.as_deref() != Some(&thread.ts))
            {
                return Err(
                    "Draft conflicts with the confirmed request thread; decline this turn".into(),
                );
            }
            if request.requires_thread(link.turn_id)
                && request.thread.is_none()
                && !self.settings.dry_run
            {
                return Err("Follow-up requires a confirmed live thread".into());
            }
        }
        let proposal = draft.kind == DraftKind::TaskProposal;
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
        if self
            .drafts
            .as_mut()
            .is_some_and(|q| q.take_restarted_automatic() > 0)
        {
            notifications.push(
                Incoming::new(
                    Source::Envoy,
                    Severity::Info,
                    "Envoy draft ready",
                    "Automatic drafts were paused after restart and need your approval.",
                )
                .with_target(DRAFTS_TARGET),
            );
        }
        let mut traffic = Vec::new();
        let updates = self.worker.as_ref().map_or_else(Vec::new, Worker::updates);
        for update in updates {
            match update {
                Update::Polled {
                    inbox,
                    events,
                    bridge,
                    notices,
                    polled_at_ms,
                } => {
                    if self.handle.is_some() {
                        self.last_polled_ms = Some(polled_at_ms);
                    }
                    if let Err(e) = self.record_replies(&inbox) {
                        self.error = Some(e);
                        continue;
                    }
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
                    if let Err(e) = self
                        .queue()
                        .and_then(|q| q.finish_receipt(&id, result, now_ms()))
                    {
                        self.error = Some(e);
                    }
                }
                Update::Watches { watches, result } => {
                    self.watches_pending = false;
                    match result {
                        Ok(()) => self.watches = Some(watches),
                        Err(e) => {
                            self.watch_retry_ms = now_ms() + 30_000;
                            self.error = Some(e);
                        }
                    }
                }
                Update::Removed { thread, result } => {
                    match result.and_then(|()| {
                        let ids: Vec<_> = self
                            .requests()
                            .iter()
                            .filter(|r| r.settled && r.thread.as_ref() == Some(&thread))
                            .map(|r| r.id.clone())
                            .collect();
                        for id in ids {
                            self.requests_mut()?.change(&id, |r| {
                                r.close_requested = false;
                                Ok(())
                            })?;
                        }
                        Ok(())
                    }) {
                        Ok(()) => {}
                        Err(e) => self.error = Some(e),
                    }
                }
                Update::Adopted(result) => {
                    match result.and_then(|threads| {
                        self.reconcile_requests()?;
                        for (thread, label) in threads {
                            self.requests_mut()?.adopt(thread, label, now_ms())?;
                        }
                        self.reconcile_requests()
                    }) {
                        Ok(()) => {}
                        Err(e) => self.error = Some(e),
                    }
                }
                Update::Resolved {
                    command,
                    reply,
                    result,
                } => {
                    let result = crate::envoy::authorize(self, &command.origin)
                        .and(result)
                        .and_then(|destination| self.route(&command.args, &destination));
                    if result.is_ok() {
                        notifications.push(
                            Incoming::new(
                                Source::Envoy,
                                Severity::Info,
                                "Envoy draft ready",
                                "A draft is waiting for your approval.",
                            )
                            .with_target(DRAFTS_TARGET),
                        );
                    }
                    reply.send(result.map_err(ControlError::invalid_params));
                }
                Update::BindingPreview {
                    command,
                    requester,
                    reply,
                    result,
                } => {
                    let result = crate::envoy::authorize(self, &command.origin)
                        .and(result)
                        .and_then(|preview| {
                            self.propose_binding(&command.args, *requester, preview)?;
                            self.gateway_envoy_request(&command)
                        });
                    if result.is_ok() {
                        notifications.push(
                            Incoming::new(
                                Source::Envoy,
                                Severity::Info,
                                "Envoy reply routing proposal",
                                "Review the proposed reply recipient in the Envoy Inbox.",
                            )
                            .with_target(DRAFTS_TARGET),
                        );
                    }
                    reply.send(result.map_err(ControlError::invalid_params));
                }
                Update::Error(error) => {
                    self.error = Some(error);
                }
            }
        }
        if let Err(e) = self.auto_approve(now_ms()) {
            self.error = Some(e);
        }
        notifications.extend(self.stall_notices(now_ms()));
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

    fn next_forward(&mut self, now: i64, ants: &[Ant]) -> Option<(Action, Vec<InboundEvent>)> {
        if self.pending.is_empty() || self.forwarding.is_some() || now < self.forward_after_ms {
            return None;
        }
        let handle = self.handle.clone()?;
        // A pin's ts is the pinned message's, so its age says nothing.
        let stale = |e: &InboundEvent| {
            e.kind != EventKind::Pin
                && timestamp_us(&e.ts).is_none_or(|ts| ts / 1000 < now - BACKLOG_AFTER_MS)
        };
        let (old, fresh): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(|e| stale(e));
        self.pending = fresh.into();
        let mut batch: Vec<_> = self.pending.drain(..self.pending.len().min(20)).collect();
        let mut lines: Vec<String> = batch
            .iter()
            .map(|e| {
                let root = e.thread_ts.as_deref().unwrap_or(&e.ts);
                let thread: Vec<&InboundEvent> = self
                    .inbox
                    .events()
                    .iter()
                    .filter(|t| {
                        t.conversation == e.conversation
                            && t.ts != e.ts
                            && t.thread_ts.as_deref().unwrap_or(&t.ts) == root
                    })
                    .collect();
                let thread = &thread[thread.len().saturating_sub(20)..];
                e.summary()
                    + &matching::annotation(&matching::candidates(
                        e,
                        thread,
                        ants,
                        &self.inbox.pr_heads,
                    ))
                    + &self.note_suffix(e)
            })
            .collect();
        if !old.is_empty() {
            lines.insert(
                0,
                format!(
                    "Envoy backlog: {} event(s) older than {} min were not typed; envoy_inbox has them if you need them",
                    old.len(),
                    BACKLOG_AFTER_MS / 60_000
                ),
            );
            batch.extend(old);
        }
        Some((
            Action::Send {
                handle,
                input: TerminalInput::line(self.handoff(lines.join(" | "))),
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
            crate::ui::envoy::DraftAction::Pause(id) => {
                let result = self
                    .queue()
                    .and_then(|q| q.schedule(&id, None))
                    .map(|()| false);
                (id, result)
            }
            crate::ui::envoy::DraftAction::Adopt => (
                "adopt".into(),
                self.worker()
                    .and_then(|w| w.submit(Work::Adopt))
                    .map(|()| false),
            ),
            crate::ui::envoy::DraftAction::Binding {
                id,
                proposal_id,
                accept,
            } => {
                let result = self
                    .decide_binding(&id, proposal_id, accept)
                    .map(|()| false);
                (id, result)
            }
            crate::ui::envoy::DraftAction::Keep(id) => {
                let result = self
                    .requests_mut()
                    .and_then(|s| {
                        s.change(&id, |r| {
                            r.orphan_kept = true;
                            Ok(())
                        })
                    })
                    .map(|()| false);
                (id, result)
            }
            crate::ui::envoy::DraftAction::Settle(id) => {
                let result = (|| {
                    let request = self.requests_mut()?.get(&id)?.clone();
                    if request.turns.iter().any(|t| {
                        matches!(
                            t.status,
                            crate::envoy::requests::Status::Queued
                                | crate::envoy::requests::Status::Drafted
                                | crate::envoy::requests::Status::Approved
                        )
                    }) {
                        return Err("Resolve pending turns before settling".into());
                    }
                    let dry_run = self.settings.dry_run;
                    let borrowed = request
                        .thread
                        .as_ref()
                        .is_some_and(|t| self.borrowed_thread(t));
                    self.requests_mut()?.change(&id, |r| {
                        r.close_requested =
                            r.thread.is_some() && r.reply_after_us.is_some() && !borrowed;
                        if !dry_run || !r.close_requested {
                            r.settled = true;
                        }
                        Ok(())
                    })?;
                    self.reconcile_requests()?;
                    Ok(false)
                })();
                (id, result)
            }
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
        self.envoy_panel.set_requests(self.envoy_core.requests());
        let action = self.envoy_panel.show(
            ui,
            self.envoy_core.drafts(),
            self.envoy_core.inbox(),
            self.envoy_core.settings().dry_run,
            self.envoy_core.error.as_deref(),
        );
        if let Some(action) = action {
            let pause = matches!(action, crate::ui::envoy::DraftAction::Pause(_));
            let ready = self.chat_ready();
            let chat = &mut self.chat;
            let (id, result) = self
                .envoy_core
                .panel_action(action, ready, |text| chat.session_mut().send(text));
            if matches!(result, Ok(true)) {
                self.open_chat();
            }
            if !pause || result.is_err() {
                self.envoy_panel.result(&id, result.map(|_| ()));
            }
            if let Err(e) = self.envoy_core.reconcile_requests() {
                self.envoy_core.error = Some(e);
            }
        }
    }

    pub(super) fn control_envoy(
        &mut self,
        _ctx: &egui::Context,
        command: EnvoyCommand,
        reply: Reply,
    ) -> Result<(), ControlError> {
        if crate::envoy::control::PUBLIC_METHODS.contains(&command.method.as_str()) {
            let result = self
                .gateway_requester(command.origin.clone())
                .and_then(|who| self.envoy_core.gateway_request(&command, who));
            if command.method == "reach_out"
                && result.as_ref().is_ok_and(|v| v["merged"] == false)
                && command.args["urgency"] == "blocking"
            {
                self.raise(
                    Incoming::new(
                        Source::Envoy,
                        Severity::Warn,
                        "Blocking request to Envoy",
                        "An ant is waiting for an external reply.",
                    )
                    .with_target(DRAFTS_TARGET),
                );
            }
            reply.send(result.map_err(ControlError::invalid_params));
            return Ok(());
        }
        if let Err(e) = crate::envoy::authorize(&self.envoy_core, &command.origin) {
            let error = ControlError::new(ErrorCode::Refused, e);
            reply.send(Err(error.clone()));
            return Err(error);
        }
        if command.method == "envoy_route" {
            match self.envoy_core.worker() {
                Ok(worker) => {
                    let _ = worker.submit(Work::Resolve { command, reply });
                }
                Err(e) => reply.send(Err(ControlError::new(ErrorCode::Unavailable, e))),
            }
            return Ok(());
        }
        if command.method == "envoy_link_reply" && command.args.get("requester").is_some() {
            let result: Result<_, String> = (|| {
                let target = required(&command.args, "requester")?;
                let node = self.control_node(target).map_err(|e| e.to_string())?;
                if !node.live {
                    return Err("Requester must be live".into());
                }
                let handle = node
                    .terminal_handle
                    .clone()
                    .ok_or("Requester has no terminal")?;
                let who = self.gateway_requester(crate::review::mark::Origin {
                    terminal_handle: Some(handle),
                    ..Default::default()
                })?;
                let thread = self.envoy_core.binding_thread(&command.args, &who)?;
                Ok((who, thread))
            })();
            match result {
                Ok((requester, Some(thread))) => {
                    match self.envoy_core.worker() {
                        Ok(worker) => {
                            let _ = worker.submit(Work::BindingPreview {
                                command,
                                requester: Box::new(requester),
                                thread,
                                reply,
                            });
                        }
                        Err(e) => reply.send(Err(ControlError::new(ErrorCode::Unavailable, e))),
                    }
                    return Ok(());
                }
                Ok((_, None)) => {}
                Err(e) => {
                    reply.send(Err(ControlError::invalid_params(e)));
                    return Ok(());
                }
            }
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
                        let note = self.envoy_core.note_for(&event).cloned();
                        let _ = worker.submit(Work::Thread { event, note, reply });
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
        self.forward_gateway();
    }

    fn envoy_target_ready(&self, handle: &str) -> bool {
        self.target_blocked(handle).is_none()
    }

    /// Why `handle` can't take typed input now, or `None` when it can.
    fn target_blocked(&self, handle: &str) -> Option<&'static str> {
        let Some(terminal) = self.data.as_ref().and_then(|d| d.terminal(handle)) else {
            return Some("terminal not listed");
        };
        let idle = self.snapshot.as_ref().is_some_and(|s| {
            s.nodes.iter().any(|n| {
                n.terminal_handle.as_deref() == Some(handle)
                    && n.live
                    && n.state == crate::model::NodeState::Idle
            })
        });
        if !idle {
            return Some("not idle");
        }
        if self.input.text_pending(handle)
            || self.control.typing_pending(handle)
            || !self.input.draft(handle).is_empty()
        {
            return Some("input pending");
        }
        let composer = self.data.as_ref().and_then(|d| d.screen_composer(handle));
        (!can_forward(terminal, true, false, composer)).then_some("no empty agent prompt")
    }

    fn forward_envoy(&mut self) {
        let blocked = match self.envoy_core.handle.as_deref() {
            None => Some("no Envoy session"),
            Some(_) if !self.envoy_core.gateway_actions.is_empty() => {
                Some("gateway send in flight")
            }
            Some(_) if self.envoy_core.forwarding.is_some() => Some("batch in flight"),
            Some(_) if self.envoy_core.resetting.is_some() => Some("context reset in flight"),
            Some(handle) => self.target_blocked(handle),
        };
        if self.envoy_core.pending.is_empty() || blocked.is_some() {
            if !self.envoy_core.pending.is_empty() && blocked != self.envoy_core.blocked {
                log::info!(
                    "envoy: {} event(s) waiting: {}",
                    self.envoy_core.pending.len(),
                    blocked.unwrap_or_default()
                );
            }
            self.envoy_core.blocked = blocked;
            return;
        }
        self.envoy_core.blocked = None;
        if self.envoy_reset() {
            return;
        }
        let ants = self.envoy_ants();
        if let Some((action, batch)) = self.envoy_core.next_forward(now_ms(), &ants) {
            log::info!("envoy: typing {} event(s)", batch.len());
            let id = self.actions.submit(action);
            self.envoy_core.forwarding = Some((id, batch));
        }
    }

    /// Clears the Envoy's context when a reset is due before what is about to be typed;
    /// true when the `/clear` was submitted and the batch must wait.
    pub(super) fn envoy_reset(&mut self) -> bool {
        let now = now_ms();
        let tokens = self
            .envoy_core
            .handle
            .as_deref()
            .and_then(|h| self.claude_data.as_ref()?.context_for_handle(h));
        let Some((action, reason)) = self.envoy_core.reset_action(now, tokens) else {
            return false;
        };
        log::info!(
            "envoy: fresh context ({}): {} event(s) typed since the last one, context {} tokens, {} pending",
            reason.label(),
            self.envoy_core.context.events_since_reset,
            tokens.map_or("unknown".to_owned(), |t| t.to_string()),
            self.envoy_core.pending.len()
        );
        let id = self.actions.submit(action);
        self.envoy_core.resetting = Some(id);
        true
    }

    /// Live ants the Envoy may relay to, with what identifies their work.
    fn envoy_ants(&self) -> Vec<Ant> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        snapshot
            .nodes
            .iter()
            .filter(|n| {
                n.live
                    && !n.is_parked()
                    && matches!(
                        n.kind,
                        NodeKind::Coordinator
                            | NodeKind::Attendant
                            | NodeKind::Worker
                            | NodeKind::Terminal
                    )
            })
            .map(|n| {
                let handle = n.terminal_handle.as_deref();
                let mine = |origin: &crate::review::mark::Origin| {
                    handle.is_some() && origin.terminal_handle.as_deref() == handle
                };
                let mut context = Vec::new();
                if let Some(run) = n.run_id.as_deref() {
                    context.extend(snapshot.run(run).and_then(|r| r.objective.clone()));
                    // A coordinator answers for every task in its run, a worker for its own.
                    let lead = matches!(n.kind, NodeKind::Coordinator | NodeKind::Attendant);
                    let tasks = self.data.as_ref().and_then(|d| d.tasks.get(run));
                    context.extend(
                        tasks
                            .into_iter()
                            .flatten()
                            .filter(|t| lead || Some(&t.id) == n.task_id.as_ref())
                            .filter_map(|t| t.task_title.clone()),
                    );
                }
                let mut prs = Vec::new();
                for entry in self.review.entries().filter(|e| mine(&e.origin)) {
                    prs.push(format!("{}#{}", entry.pr.repo, entry.pr.number));
                    context.extend(entry.title.clone());
                }
                let threads = self
                    .envoy_core
                    .requests()
                    .iter()
                    .filter(|r| !r.settled && r.bound_requester().is_some_and(|q| mine(&q.origin)))
                    .filter_map(|r| r.thread.as_ref())
                    .map(|t| format!("{}:{}", t.conversation, t.ts))
                    .collect();
                let repo = crate::model::node_repo(n, snapshot);
                Ant {
                    id: n.id.0.clone(),
                    label: n.label.clone(),
                    repo: (repo != crate::model::NO_REPO).then_some(repo),
                    branch: n.branch.clone().filter(|b| !b.is_empty()),
                    context,
                    prs,
                    threads,
                }
            })
            .collect()
    }

    pub(super) fn envoy_action_done(&mut self, result: &ActionResult) {
        if let Some(message) = self.envoy_core.relay_actions.remove(&result.id)
            && result.outcome.is_ok()
        {
            self.traffic.sender().send(message);
        }
        let traffic = self.traffic.sender();
        self.envoy_core
            .gateway_done(result, |message| traffic.send(message));
        self.envoy_core
            .forward_done(result, now_ms(), |message| traffic.send(message));
        self.envoy_core.reset_done(result, now_ms());
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

/// Whether typed text would land in the agent's empty composer. `composer` is the sampled
/// screen's verdict and settles it; without one, the preview is read, which Orca builds from
/// a stale banner and the last redrawn fragment, so the composer line is often missing from
/// it and the answer errs towards `false`.
pub fn can_forward(
    terminal: &crate::orca::Terminal,
    idle: bool,
    typing: bool,
    composer: Option<crate::detect::ComposerState>,
) -> bool {
    if !idle || typing || terminal.orphaned || !terminal.writable {
        return false;
    }
    if let Some(composer) = composer {
        return composer == crate::detect::ComposerState::Empty;
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
mod gateway_tests;
#[cfg(test)]
mod tests;

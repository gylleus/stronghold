//! App integration for ingestion, approvals, relays and event forwarding.

use eframe::egui;
use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

use super::{MyrstackApp, now_ms};
use crate::actions::{Action, ActionId, ActionResult};
use crate::control::{ControlCommand, ControlError, ErrorCode, Reply};
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

pub const DRAFTS_TARGET: &str = "envoy:drafts";

pub struct AppEnvoy {
    handle: Option<String>,
    settings: EnvoySettings,
    inbox: Inbox,
    drafts: Option<DraftQueue>,
    worker: Option<Worker>,
    bridge: BridgeStatus,
    relays: RelayLimiter,
    relay_path: PathBuf,
    pending: VecDeque<InboundEvent>,
    forwarding: Option<(ActionId, Vec<InboundEvent>)>,
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
            relays: relays.unwrap_or_default(),
            relay_path,
            pending: VecDeque::new(),
            forwarding: None,
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
            self.pending = self.inbox.unforwarded().into();
        }
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
        let draft = self
            .envoy_core
            .queue()?
            .begin_approval(id, edited_text, dry_run, now_ms())?;
        if draft.status == DraftStatus::DryRunSent {
            return Ok(());
        }
        if proposal {
            let sent = self.chat.session_mut().send(&draft.queen_request());
            self.envoy_core.queue()?.finish(
                id,
                if sent {
                    Ok(())
                } else {
                    Err("Queen chat refused the request".into())
                },
                now_ms(),
            )?;
            if sent {
                self.open_chat();
            }
        } else {
            let result = self.envoy_core.worker()?.submit(Work::Send {
                id: id.into(),
                message: draft.outbound()?,
            });
            if let Err(e) = result {
                self.envoy_core
                    .queue()?
                    .finish(id, Err(e.clone()), now_ms())?;
                return Err(e);
            }
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
        ctx: &egui::Context,
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
                .and_then(|request| {
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
                Ok(record) => {
                    let command = if record.request.to == "coordinators" {
                        ControlCommand::Broadcast {
                            text: record.request.text(),
                            runs: None,
                        }
                    } else {
                        ControlCommand::Send {
                            agent: record.request.to.clone(),
                            text: record.request.text(),
                            enter: true,
                        }
                    };
                    self.run_control_command(ctx, command, reply)
                }
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
                    if self
                        .envoy_core
                        .inbox
                        .get(&format!("{}:{}", target.conversation, target.ts))
                        .is_none()
                    {
                        return Err("Draft must reply to a retained inbox event".into());
                    }
                    Draft::reply(
                        target,
                        optional(args, "thread_ts")?.map(str::to_owned),
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
                    self.envoy_core.inbox = inbox;
                    if self.envoy_core.handle.is_some() {
                        self.envoy_core.pending.extend(events);
                        while self.envoy_core.pending.len() > 2000 {
                            self.envoy_core.pending.pop_front();
                        }
                        if bridge != self.envoy_core.bridge && bridge != BridgeStatus::Ok {
                            self.raise(Incoming::new(
                                Source::Envoy,
                                Severity::Warn,
                                "Envoy bridge down",
                                "Slack channel ingestion is falling back to polling.",
                            ));
                        }
                    }
                    self.envoy_core.bridge = bridge;
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
                    if let Err(e) = self
                        .envoy_core
                        .queue()
                        .and_then(|q| q.finish(&id, result, now_ms()))
                    {
                        self.envoy_core.error = Some(e);
                    }
                }
                Update::Error(error) => {
                    if self.envoy_core.error.as_ref() != Some(&error) {
                        self.raise(Incoming::new(
                            Source::Envoy,
                            Severity::Warn,
                            "Envoy storage unavailable",
                            &error,
                        ));
                        self.envoy_core.error = Some(error);
                    }
                }
            }
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
            .join("\n");
        let id = self.actions.submit(Action::Send {
            handle: handle.into(),
            input: TerminalInput::line(text),
        });
        core.forwarding = Some((id, batch));
    }

    pub(super) fn envoy_action_done(&mut self, result: &ActionResult) {
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
            let ids: Vec<_> = batch.iter().map(InboundEvent::id).collect();
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
}

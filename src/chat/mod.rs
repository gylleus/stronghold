//! The King's chat: a Claude Code session Stronghold spawns and owns (`claude -p` in
//! stream-json mode), with the stronghold MCP tools, shown in the app's conversation column. It is not an
//! Orca terminal, so it never appears in the farm.

pub mod auto_mode;
pub mod config;
pub mod context;
pub mod history;
pub mod permissions;
pub mod process;
pub mod protocol;
pub mod say;
pub mod session;
pub mod settings;
pub mod transcript;
pub mod voice;

pub use config::ChatConfig;
pub use process::{ChatCommand, ChatEvent, ChatHandle, SessionState};
pub use transcript::{ChatItem, Decision, Transcript};
pub use voice::SpeechEvent;

use protocol::{ContentBlock, Incoming};

use crate::traffic::ledger;

/// Text of the "Status report" button.
pub const STATUS_REPORT_PROMPT: &str = "Give me a status report. Fetch clan_status and review_queue \
fresh in this turn. Only list PRs waiting on me from that fresh queue with verified open state; \
alerts and agent messages are context, not pending-review evidence.";

/// The chat as the app holds it: the process handle and the transcript built from its events.
pub struct ChatSession {
    handle: ChatHandle,
    pub transcript: Transcript,
    pub state: SessionState,
    /// Messages to send once the session is ready (`STRONGHOLD_CHAT_SEND`, for smoke tests).
    queued: Vec<String>,
    inbox_inflight: Option<String>,
    /// Where auto mode is saved (the chat directory).
    dir: std::path::PathBuf,
    auto_mode: bool,
    /// The auto mode the King was last told about in this conversation.
    told_auto: Option<bool>,
    /// `--model` / `--effort` the process runs with.
    model: Option<String>,
    effort: Option<String>,
    /// Turn starts and reply text for the speaker, drained by [`take_speech`](Self::take_speech).
    speech: Vec<SpeechEvent>,
    /// "Speak replies" is on: typed turns are spoken too and carry [`voice::SPEAK_NOTE`].
    speak_replies: bool,
    /// The resumed conversation's earlier messages, being read from its transcript.
    history: Option<std::sync::mpsc::Receiver<Transcript>>,
    /// The King's Ledger calls as traffic, drained by [`take_ledger_calls`](Self::take_ledger_calls).
    ledger_calls: Vec<crate::traffic::AgentMessage>,
}

/// Environment variable with a message the chat sends as soon as it is ready; with it the
/// transcript is also logged at info level. For scripted smoke tests.
pub const AUTOSEND_ENV: &str = "STRONGHOLD_CHAT_SEND";

impl ChatSession {
    pub fn spawn(config: ChatConfig, repaint: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        let queued = crate::legacy::env(AUTOSEND_ENV)
            .filter(|s| !s.trim().is_empty())
            .into_iter()
            .collect();
        let dir = config.dir.clone();
        let auto_mode = auto_mode::load(&dir);
        let (model, effort) = (config.model.clone(), config.effort.clone());
        let repaint: Option<std::sync::Arc<dyn Fn() + Send + Sync>> = repaint.map(Into::into);
        let history = session::SessionStore::saved(&dir)
            .filter(|r| r.started)
            .map(|r| {
                let locator = crate::claude::SessionLocator::new(&config.projects_root);
                load_history(locator, dir.clone(), r.session_id, repaint.clone())
            });
        let handle = ChatHandle::spawn(
            config,
            repaint.map(|r| Box::new(move || r()) as Box<dyn Fn() + Send + Sync>),
        );
        handle.send(ChatCommand::SetAuto(auto_mode));
        Self {
            handle,
            transcript: Transcript::default(),
            state: SessionState::Starting,
            queued,
            inbox_inflight: None,
            dir,
            auto_mode,
            told_auto: None,
            model,
            effort,
            speech: Vec::new(),
            speak_replies: false,
            history,
            ledger_calls: Vec::new(),
        }
    }

    /// The Ledger calls the King made since the last call, as traffic.
    pub fn take_ledger_calls(&mut self) -> Vec<crate::traffic::AgentMessage> {
        std::mem::take(&mut self.ledger_calls)
    }

    /// The `--model` the process was started with (`None`: the CLI default).
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }

    /// Restarts the process with a new `--model` / `--effort`, resuming the same conversation
    /// (the transcript stays). A running turn ends and waiting cards are cancelled. Returns
    /// false when nothing changed.
    pub fn set_model(&mut self, model: Option<String>, effort: Option<String>) -> bool {
        if model == self.model && effort == self.effort {
            return false;
        }
        let mut notice = format!(
            "Restarting the King with model {}",
            model.as_deref().unwrap_or("CLI default")
        );
        if let Some(effort) = &effort {
            notice.push_str(&format!(" (effort {effort})"));
        }
        notice.push('…');
        self.inbox_inflight = None;
        self.end_speech_turn();
        self.transcript.session_ended();
        self.transcript.push_system(notice);
        // The header shows the requested model until the CLI reports the resolved one.
        self.transcript.model = model.clone();
        self.model = model.clone();
        self.effort = effort.clone();
        if self.state == SessionState::Ready {
            self.state = SessionState::Starting;
        }
        self.handle.send(ChatCommand::SetModel { model, effort });
        true
    }

    /// Whether the King's tool calls run without asking.
    pub fn auto_mode(&self) -> bool {
        self.auto_mode
    }

    /// The user's auto mode toggle: saves it, applies it to the running session (same process and
    /// conversation), shows a notice, and tells the King with the next message.
    pub fn set_auto_mode(&mut self, on: bool) {
        if on == self.auto_mode {
            return;
        }
        self.auto_mode = on;
        if let Err(e) = auto_mode::save(&self.dir, on) {
            log::warn!("chat: cannot save auto mode: {e}");
        }
        self.handle.send(ChatCommand::SetAuto(on));
        self.transcript.push_system(auto_mode::notice(on));
    }

    /// The text the CLI gets for the user's `text`: prefixed with a Stronghold note when the King
    /// has not been told the current auto mode (nothing to tell while it is off and he was
    /// never told it was on).
    fn outgoing(&mut self, text: &str) -> String {
        let tell = match self.told_auto {
            Some(told) => told != self.auto_mode,
            None => self.auto_mode,
        };
        if !tell {
            return text.to_owned();
        }
        self.told_auto = Some(self.auto_mode);
        format!("{}\n\n{text}", auto_mode::note(self.auto_mode))
    }

    /// Applies the supervisor's events; returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        let events = self.handle.poll();
        let mut changed = !events.is_empty();
        if let Some(rx) = &self.history {
            match rx.try_recv() {
                Ok(earlier) => {
                    log::info!("chat: loaded {} earlier messages", earlier.items.len());
                    self.transcript.prepend(earlier);
                    self.history = None;
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.history = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        for event in events {
            self.apply(event);
        }
        if self.state == SessionState::Ready && !self.transcript.running {
            for text in std::mem::take(&mut self.queued) {
                log::info!("chat: sending queued message: {text}");
                self.send(&text);
            }
        }
        changed
    }

    pub fn apply(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::State(state) => {
                if !matches!(state, SessionState::Ready | SessionState::Starting) {
                    self.inbox_inflight = None;
                    self.end_speech_turn();
                    self.transcript.session_ended();
                }
                if let SessionState::Crashed { reason, .. } = &state {
                    self.transcript
                        .push_error(format!("Chat process exited ({reason}); restarting…"));
                }
                self.state = state;
            }
            ChatEvent::Message { message, timestamp } => {
                voice::tap(&message, &mut self.speech);
                if let Incoming::Assistant { content, .. } = &message {
                    for block in content {
                        if let ContentBlock::ToolUse { name, .. } = block
                            && let Some(tool) = ledger::claude_tool(name)
                        {
                            self.ledger_calls.push(ledger::message("king", tool));
                        }
                    }
                }
                if matches!(message, Incoming::Result(_)) {
                    self.log_turn();
                }
                self.transcript.apply_at(&message, timestamp);
                if let Incoming::Result(result) = &message {
                    log::info!(
                        "chat: turn finished ({}, {} ms, ${:.4})",
                        result.subtype,
                        result.duration_ms.unwrap_or_default(),
                        result.cost_usd.unwrap_or_default()
                    );
                }
            }
            ChatEvent::PermissionNeeded(request) => {
                log::info!("chat: permission requested for {}", request.tool_name);
                self.transcript.push_permission(request);
            }
            ChatEvent::AutoApproved(request) => self.transcript.mark_auto(&request),
            ChatEvent::InboxSettled { id, error } => {
                if self.inbox_inflight.as_deref() == Some(&id) {
                    self.inbox_inflight = None;
                    self.transcript.running = false;
                }
                if let Some(error) = error {
                    self.transcript.push_error(error);
                }
            }
            ChatEvent::NewSession(id) => {
                self.inbox_inflight = None;
                log::info!("chat: new session {id}");
                self.history = None;
                self.transcript = Transcript::default();
                self.told_auto = None;
            }
            ChatEvent::Error(text) => self.transcript.push_error(text),
        }
    }

    /// Logs the newest turn's tool calls and replies when a smoke test is driving the chat.
    fn log_turn(&self) {
        if crate::legacy::env_os(AUTOSEND_ENV).is_none() {
            return;
        }
        for call in self.transcript.tool_calls() {
            let status = match &call.result {
                Some(r) if r.is_error => "error",
                Some(_) => "ok",
                None => "no result",
            };
            log::info!("chat: tool {} ({status})", call.name);
        }
        for text in self.transcript.assistant_text() {
            log::info!("chat: assistant: {text}");
        }
        for say in self.transcript.say_lines() {
            log::info!("chat: say: {say}");
        }
    }

    /// Whether the King is working on a turn.
    pub fn is_thinking(&self) -> bool {
        self.transcript.running
    }

    pub fn can_send(&self) -> bool {
        self.state == SessionState::Ready && !self.transcript.running
    }

    /// Import durable inbox items independently of whether the King is busy. Only an idle
    /// session gets a wake-up turn; the supervisor commits its receipt and handles recovery.
    pub fn poll_ant_messages(
        &mut self,
        path: &std::path::Path,
        snapshot: Option<&crate::model::ClanSnapshot>,
        terminals: &[crate::orca::Terminal],
    ) -> std::io::Result<()> {
        if self.history.is_some() {
            return Ok(());
        }
        let mut inbox = crate::king_inbox::Inbox::open(path)?;
        let session_id = session::SessionStore::saved(&self.dir).map(|r| r.session_id);
        let mut changed = false;
        for message in &mut inbox.messages {
            if let Some(agent) = snapshot
                .and_then(|s| crate::review::view::resolve_agent(&message.origin, s, terminals))
                && (message.node_id.as_deref() != Some(agent.id.as_str())
                    || message.label != agent.label)
            {
                message.node_id = Some(agent.id.0);
                message.label = agent.label;
                changed = true;
            }
            if !message.delivered
                || (message.session_id.is_some() && message.session_id == session_id)
            {
                self.transcript.push_inbox(message);
            }
        }
        if changed {
            inbox.save()?;
        }
        if self.can_send()
            && self.inbox_inflight.is_none()
            && let Some(message) = inbox.messages.iter().find(|m| !m.delivered)
        {
            self.inbox_inflight = Some(message.id.clone());
            self.transcript.running = true;
            // Auto mode is host-owned. It is never read from an agent's payload.
            let note = format!("{}\n\n", auto_mode::note(self.auto_mode));
            self.handle.send(ChatCommand::Inbox {
                path: path.into(),
                id: message.id.clone(),
                note,
            });
        }
        Ok(())
    }

    /// Whether typed turns are spoken ("Speak replies"), so they ask for a `<say>` line.
    pub fn set_speak_replies(&mut self, on: bool) {
        self.speak_replies = on;
    }

    /// Sends a user message; false when the session cannot take one now.
    pub fn send(&mut self, text: &str) -> bool {
        self.send_as(text, false)
    }

    /// Sends a message the user spoke: their bubble gets the voice marker and the King gets
    /// [`voice::VOICE_NOTE`] with it (not shown), asking for a `<say>` line to speak.
    pub fn send_voice(&mut self, text: &str) -> bool {
        self.send_as(text, true)
    }

    fn send_as(&mut self, text: &str, voice: bool) -> bool {
        let text = text.trim();
        if text.is_empty() || !self.can_send() {
            return false;
        }
        let mut outgoing = self.outgoing(text);
        if voice {
            self.transcript.push_voice_user(text);
            outgoing = format!("{}\n\n{outgoing}", crate::user::fill(voice::VOICE_NOTE));
        } else {
            self.transcript.push_user(text);
            if self.speak_replies {
                outgoing = format!("{}\n\n{outgoing}", voice::SPEAK_NOTE);
            }
        }
        self.speech.push(SpeechEvent::TurnStart { voice });
        self.handle.send(ChatCommand::Send(outgoing));
        true
    }

    /// Sends `/compact` as is (no notes in front, so the CLI runs it as a slash command);
    /// false when the session cannot take a message now.
    pub fn compact(&mut self) -> bool {
        if !self.can_send() {
            return false;
        }
        self.transcript.push_user(context::COMPACT_COMMAND);
        self.handle
            .send(ChatCommand::Send(context::COMPACT_COMMAND.to_owned()));
        true
    }

    /// Tells the speaker a running turn is over when it ends without a result (restart, crash,
    /// new chat).
    fn end_speech_turn(&mut self) {
        if self.transcript.running {
            self.speech.push(SpeechEvent::TurnEnd);
        }
    }

    /// Turn starts and reply text since the last call, for the speaker.
    pub fn take_speech(&mut self) -> Vec<SpeechEvent> {
        std::mem::take(&mut self.speech)
    }

    pub fn interrupt(&self) {
        if self.transcript.running {
            self.handle.send(ChatCommand::Interrupt);
        }
    }

    /// Answers the permission card of `request_id`.
    pub fn decide(&mut self, request_id: &str, decision: Decision) {
        // Only the supervisor answers with auto mode.
        if decision == Decision::Auto || !self.transcript.decide(request_id, decision) {
            return;
        }
        let (allow, always) = match decision {
            Decision::Allow => (true, false),
            Decision::AlwaysAllow => (true, true),
            Decision::Deny | Decision::Cancelled | Decision::Auto => (false, false),
        };
        self.handle.send(ChatCommand::Permission {
            request_id: request_id.to_owned(),
            allow,
            always,
        });
    }

    pub fn new_chat(&mut self) {
        self.end_speech_turn();
        self.history = None;
        self.transcript = Transcript::default();
        self.told_auto = None;
        self.handle.send(ChatCommand::NewChat);
    }

    pub fn retry(&self) {
        self.handle.send(ChatCommand::Retry);
    }
}

/// Reads the earlier messages of the King's session `session_id` (run in `dir`) on a thread.
fn load_history(
    locator: crate::claude::SessionLocator,
    dir: std::path::PathBuf,
    session_id: String,
    repaint: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
) -> std::sync::mpsc::Receiver<Transcript> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("chat-history".into())
        .spawn(move || {
            let Some(info) = locator.find_session(&session_id, Some(&dir)) else {
                log::info!("chat: no transcript for session {session_id}");
                return;
            };
            match history::load(&info.transcript_path) {
                Ok(earlier) => {
                    if tx.send(earlier).is_ok()
                        && let Some(repaint) = repaint
                    {
                        repaint();
                    }
                }
                Err(e) => log::warn!("chat: cannot read {}: {e}", info.transcript_path.display()),
            }
        });
    if let Err(e) = spawned {
        log::warn!("chat: cannot start the history thread: {e}");
    }
    rx
}

#[cfg(test)]
mod tests;

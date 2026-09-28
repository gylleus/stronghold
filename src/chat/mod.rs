//! The Queen's chat: a Claude Code session Myrstack spawns and owns (`claude -p` in
//! stream-json mode), with the myrstack MCP tools, shown in the Queen's panel. It is not an
//! Orca terminal, so it never appears in the graph.

pub mod auto_mode;
pub mod config;
pub mod permissions;
pub mod process;
pub mod protocol;
pub mod session;
pub mod transcript;

pub use config::ChatConfig;
pub use process::{ChatCommand, ChatEvent, ChatHandle, SessionState};
pub use transcript::{ChatItem, Decision, Transcript};

use protocol::Incoming;

/// Text of the "Status report" button.
pub const STATUS_REPORT_PROMPT: &str = "Give me a status report.";

/// The chat as the app holds it: the process handle and the transcript built from its events.
pub struct ChatSession {
    handle: ChatHandle,
    pub transcript: Transcript,
    pub state: SessionState,
    /// Messages to send once the session is ready (`MYRSTACK_CHAT_SEND`, for smoke tests).
    queued: Vec<String>,
    /// Where auto mode is saved (the chat directory).
    dir: std::path::PathBuf,
    auto_mode: bool,
    /// The auto mode the Queen was last told about in this conversation.
    told_auto: Option<bool>,
}

/// Environment variable with a message the chat sends as soon as it is ready; with it the
/// transcript is also logged at info level. For scripted smoke tests.
pub const AUTOSEND_ENV: &str = "MYRSTACK_CHAT_SEND";

impl ChatSession {
    pub fn spawn(config: ChatConfig, repaint: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        let queued = std::env::var(AUTOSEND_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .into_iter()
            .collect();
        let dir = config.dir.clone();
        let auto_mode = auto_mode::load(&dir);
        let handle = ChatHandle::spawn(config, repaint);
        handle.send(ChatCommand::SetAuto(auto_mode));
        Self {
            handle,
            transcript: Transcript::default(),
            state: SessionState::Starting,
            queued,
            dir,
            auto_mode,
            told_auto: None,
        }
    }

    /// Whether the Queen's Myrstack tool calls run without asking.
    pub fn auto_mode(&self) -> bool {
        self.auto_mode
    }

    /// Karl's auto mode toggle: saves it, applies it to the running session (same process and
    /// conversation), shows a notice, and tells the Queen with the next message.
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

    /// The text the CLI gets for Karl's `text`: prefixed with a Myrstack note when the Queen
    /// has not been told the current auto mode (nothing to tell while it is off and she was
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
        let changed = !events.is_empty();
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
                    self.transcript.session_ended();
                }
                if let SessionState::Crashed { reason, .. } = &state {
                    self.transcript
                        .push_error(format!("Chat process exited ({reason}); restarting…"));
                }
                self.state = state;
            }
            ChatEvent::Message(message) => {
                if matches!(message, Incoming::Result(_)) {
                    self.log_turn();
                }
                self.transcript.apply(&message);
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
            ChatEvent::NewSession(id) => {
                log::info!("chat: new session {id}");
                self.transcript = Transcript::default();
                self.told_auto = None;
            }
            ChatEvent::Error(text) => self.transcript.push_error(text),
        }
    }

    /// Logs the newest turn's tool calls and replies when a smoke test is driving the chat.
    fn log_turn(&self) {
        if std::env::var_os(AUTOSEND_ENV).is_none() {
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
    }

    pub fn can_send(&self) -> bool {
        self.state == SessionState::Ready && !self.transcript.running
    }

    /// Sends a user message; false when the session cannot take one now.
    pub fn send(&mut self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() || !self.can_send() {
            return false;
        }
        self.transcript.push_user(text);
        let outgoing = self.outgoing(text);
        self.handle.send(ChatCommand::Send(outgoing));
        true
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
        self.transcript = Transcript::default();
        self.told_auto = None;
        self.handle.send(ChatCommand::NewChat);
    }

    pub fn retry(&self) {
        self.handle.send(ChatCommand::Retry);
    }
}

#[cfg(test)]
mod tests;

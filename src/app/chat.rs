//! The app side of the King's chat: owns the chat session and draws it in the conversation column.

use eframe::egui;

use super::{StrongholdApp, repaint_once};
use crate::chat::settings::{KingSettings, resolve_model_from_env};
use crate::chat::{ChatConfig, ChatSession, STATUS_REPORT_PROMPT};
use crate::model::{ClanSnapshot, NodeId};
use crate::traffic::{AgentMessage, MessageKind, Traffic};
use crate::ui::chat::{ChatAction, ChatPanelState, ChatView, chat_ui};

/// Karl writing (or speaking) to the King in the chat.
pub(super) fn karl_to_king() -> AgentMessage {
    AgentMessage::new("karl", "king", MessageKind::Instruction)
}

pub(super) struct AppChat {
    session: ChatSession,
    panel: ChatPanelState,
    inbox_polled: Option<std::time::Instant>,
    inbox_error: Option<String>,
}

impl AppChat {
    /// Spawns the chat's `claude` process (or notes that it is missing) with the saved model.
    pub(super) fn start(ctx: &egui::Context, king: &KingSettings) -> Self {
        Self {
            session: ChatSession::spawn(ChatConfig::from_env(king), Some(repaint_once(ctx))),
            panel: ChatPanelState::default(),
            inbox_polled: None,
            inbox_error: None,
        }
    }

    /// Draws the chat and runs what the user did; returns a node whose link was clicked. What
    /// Karl sends flies to the King on `traffic`.
    pub(super) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: Option<&ClanSnapshot>,
        traffic: &Traffic,
    ) -> Option<NodeId> {
        let is_node = |id: &NodeId| snapshot.is_some_and(|s| s.node(id).is_some());
        let view = ChatView {
            transcript: &self.session.transcript,
            state: &self.session.state,
            is_node: &is_node,
            auto_mode: self.session.auto_mode(),
        };
        let mut select = None;
        for action in chat_ui(ui, &mut self.panel, view) {
            match action {
                ChatAction::Send(text) => {
                    if self.session.send(&text) {
                        traffic.push(karl_to_king());
                    }
                }
                ChatAction::Interrupt => self.session.interrupt(),
                ChatAction::Decide {
                    request_id,
                    decision,
                } => self.session.decide(&request_id, decision),
                ChatAction::NewChat => self.session.new_chat(),
                ChatAction::Compact => {
                    self.session.compact();
                }
                ChatAction::Retry => self.session.retry(),
                ChatAction::SelectNode(id) => select = Some(id),
                ChatAction::SetAutoMode(on) => self.session.set_auto_mode(on),
            }
        }
        select
    }
}

impl AppChat {
    /// The input box's text, for dictation to insert into.
    pub(super) fn draft_mut(&mut self) -> &mut String {
        &mut self.panel.draft
    }

    /// The session, for talking to the King (voice messages and his reply text).
    pub(super) fn session_mut(&mut self) -> &mut ChatSession {
        &mut self.session
    }
}

impl AppChat {
    /// Read-only: the control API and MCP report it, nothing outside the header sets it.
    /// The King's chat process state.
    pub(super) fn state(&self) -> &crate::chat::SessionState {
        &self.session.state
    }

    pub(super) fn auto_mode(&self) -> bool {
        self.session.auto_mode()
    }

    /// The model the chat header shows.
    pub(super) fn running_model(&self) -> Option<&str> {
        self.session.transcript.model.as_deref()
    }

    /// Applies saved King settings: restarts his process (same conversation) when the
    /// effective model (`STRONGHOLD_CHAT_MODEL` still wins) or the effort changed.
    pub(super) fn apply_king_settings(&mut self, king: &KingSettings) {
        let model = resolve_model_from_env(king).model().map(str::to_owned);
        let effort = king.normalized().effort;
        if self.session.set_model(model, effort) {
            log::info!("chat: restarting the King with new settings");
        }
    }

    pub(super) fn set_auto_mode(&mut self, on: bool) {
        self.session.set_auto_mode(on);
    }
}

impl StrongholdApp {
    pub(super) fn poll_chat(&mut self) {
        self.chat.session.poll();
        if self
            .chat
            .inbox_polled
            .is_none_or(|at| at.elapsed().as_secs() >= 1)
        {
            self.chat.inbox_polled = Some(std::time::Instant::now());
            if let Some(path) = crate::queen_inbox::default_path() {
                let terminals = self
                    .data
                    .as_ref()
                    .map_or(&[][..], |d| d.terminals.as_slice());
                let result =
                    self.chat
                        .session
                        .poll_ant_messages(&path, self.snapshot.as_ref(), terminals);
                let error = result.err().map(|e| format!("Queen inbox: {e}"));
                if error != self.chat.inbox_error {
                    if let Some(error) = &error {
                        self.chat.session.transcript.push_error(error);
                    }
                    self.chat.inbox_error = error;
                }
            }
        }
    }

    /// Cmd+J: hide the conversation pane when it shows the King's chat, else show his chat
    /// with the cursor in it. A chat tabbed behind another pane comes to the front instead.
    pub(super) fn toggle_chat(&mut self) {
        use crate::ui::dock::Pane;
        let behind =
            self.panes.contains(Pane::Conversation) && !self.panes.is_visible(Pane::Conversation);
        if self.shows_chat() && !behind {
            self.hide_conversation();
        } else {
            self.open_chat();
        }
    }

    /// Selects the King, brings the conversation pane forward and focuses the chat's input.
    pub(super) fn open_chat(&mut self) {
        self.chat_hidden = false;
        self.select_king();
        self.bring_forward = Some(crate::ui::dock::Pane::Conversation);
        self.chat.panel.focus_input = true;
    }

    /// Whether the conversation column shows the King's chat now.
    pub(super) fn shows_chat(&self) -> bool {
        use crate::ui::columns::{Picked, shows_chat};
        let picked = if self.clan.selection().len() > 1 {
            Picked::Other
        } else if self.king_selected() {
            Picked::King
        } else if self.selected_node().is_some() {
            Picked::Other
        } else {
            Picked::Nothing
        };
        shows_chat(picked, self.chat_hidden)
    }

    /// Whether the chat can take a message now.
    pub(super) fn chat_ready(&self) -> bool {
        self.chat.session.can_send()
    }

    /// Whether the King is working on a chat turn.
    pub(super) fn king_thinking(&self) -> bool {
        self.chat.session.is_thinking()
    }

    /// Opens the chat and sends the Status report prompt, as its button's `ChatAction::Send`
    /// does.
    pub(super) fn status_report(&mut self) {
        self.open_chat();
        self.chat.session.send(STATUS_REPORT_PROMPT);
        self.farm.pulse(crate::ui::farm::Pulse::StatusReport);
    }
}

impl StrongholdApp {
    /// The top bar's King and Status report buttons, run through the command palette's
    /// actions so they behave as picking "Open King chat" or "Status report" there.
    pub(super) fn king_quick_actions(&mut self, ui: &mut egui::Ui) {
        use crate::ui::commands::{self, ActionId};
        use crate::ui::king::{KingQuickAction, quick_actions};
        self.envoy_quick_actions(ui);
        self.envoy_drafts_button(ui);
        let cx = self.palette_context();
        let status_disabled = commands::disabled_reason(ActionId::StatusReport, &cx);
        let action = match quick_actions(ui, status_disabled) {
            Some(KingQuickAction::OpenChat) => ActionId::OpenChat,
            Some(KingQuickAction::StatusReport) => ActionId::StatusReport,
            None => return,
        };
        self.run_palette_action(ui.ctx(), action);
    }
}

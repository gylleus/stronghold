//! The app side of the Queen's chat: owns the chat session and draws it in the conversation column.

use eframe::egui;

use super::{MyrstackApp, repaint_once};
use crate::chat::settings::{QueenSettings, resolve_model_from_env};
use crate::chat::{ChatConfig, ChatSession, STATUS_REPORT_PROMPT};
use crate::model::{FleetSnapshot, NodeId};
use crate::ui::chat::{ChatAction, ChatPanelState, ChatView, chat_ui};

pub(super) struct AppChat {
    session: ChatSession,
    panel: ChatPanelState,
}

impl AppChat {
    /// Spawns the chat's `claude` process (or notes that it is missing) with the saved model.
    pub(super) fn start(ctx: &egui::Context, queen: &QueenSettings) -> Self {
        Self {
            session: ChatSession::spawn(ChatConfig::from_env(queen), Some(repaint_once(ctx))),
            panel: ChatPanelState::default(),
        }
    }

    /// Draws the chat and runs what the user did; returns a node whose link was clicked.
    pub(super) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: Option<&FleetSnapshot>,
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
                    self.session.send(&text);
                }
                ChatAction::Interrupt => self.session.interrupt(),
                ChatAction::Decide {
                    request_id,
                    decision,
                } => self.session.decide(&request_id, decision),
                ChatAction::NewChat => self.session.new_chat(),
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

    /// The session, for talking to the Queen (voice messages and her reply text).
    pub(super) fn session_mut(&mut self) -> &mut ChatSession {
        &mut self.session
    }
}

impl AppChat {
    /// Read-only: the control API and MCP report it, nothing outside the header sets it.
    pub(super) fn auto_mode(&self) -> bool {
        self.session.auto_mode()
    }

    /// The model the chat header shows.
    pub(super) fn running_model(&self) -> Option<&str> {
        self.session.transcript.model.as_deref()
    }

    /// Applies saved Queen settings: restarts her process (same conversation) when the
    /// effective model (`MYRSTACK_CHAT_MODEL` still wins) or the effort changed.
    pub(super) fn apply_queen_settings(&mut self, queen: &QueenSettings) {
        let model = resolve_model_from_env(queen).model().map(str::to_owned);
        let effort = queen.normalized().effort;
        if self.session.set_model(model, effort) {
            log::info!("chat: restarting the Queen with new settings");
        }
    }

    pub(super) fn set_auto_mode(&mut self, on: bool) {
        self.session.set_auto_mode(on);
    }
}

impl MyrstackApp {
    pub(super) fn poll_chat(&mut self) {
        self.chat.session.poll();
    }

    /// Cmd+J: hide the conversation pane when it shows the Queen's chat, else show her chat
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

    /// Selects the Queen, brings the conversation pane forward and focuses the chat's input.
    pub(super) fn open_chat(&mut self) {
        self.chat_hidden = false;
        self.select_queen();
        self.bring_forward = Some(crate::ui::dock::Pane::Conversation);
        self.chat.panel.focus_input = true;
    }

    /// Whether the conversation column shows the Queen's chat now.
    pub(super) fn shows_chat(&self) -> bool {
        use crate::ui::columns::{Picked, shows_chat};
        let picked = if self.graph.selection().len() > 1 {
            Picked::Other
        } else if self.queen_selected() {
            Picked::Queen
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

    /// Opens the chat and sends the Status report prompt, as its button's `ChatAction::Send`
    /// does.
    pub(super) fn status_report(&mut self) {
        self.open_chat();
        self.chat.session.send(STATUS_REPORT_PROMPT);
        self.ant_farm
            .pulse(crate::ui::ant_farm::Pulse::StatusReport);
    }
}

impl MyrstackApp {
    /// The top bar's Queen and Status report buttons, run through the command palette's
    /// actions so they behave as picking "Open Queen chat" or "Status report" there.
    pub(super) fn queen_quick_actions(&mut self, ui: &mut egui::Ui) {
        use crate::ui::commands::{self, ActionId};
        use crate::ui::queen::{QueenQuickAction, quick_actions};
        let cx = self.palette_context();
        let status_disabled = commands::disabled_reason(ActionId::StatusReport, &cx);
        let action = match quick_actions(ui, status_disabled) {
            Some(QueenQuickAction::OpenChat) => ActionId::OpenChat,
            Some(QueenQuickAction::StatusReport) => ActionId::StatusReport,
            None => return,
        };
        self.run_palette_action(ui.ctx(), action);
    }
}

//! Hold clicks focus an input once its conversation pane has been drawn.

use eframe::egui;

use super::StrongholdApp;
use crate::model::{Node, NodeId};
use crate::orca::Terminal;
use crate::poller::ScreenUpdate;
use crate::ui::{self, dock::Pane};

#[cfg(test)]
mod tests;

pub(super) struct Request {
    node: NodeId,
    input: egui::Id,
}

impl Request {
    pub(super) fn from_click(
        node: &NodeId,
        modifiers: egui::Modifiers,
        input: Option<egui::Id>,
    ) -> Option<Self> {
        if !modifiers.is_none() {
            return None;
        }
        Some(Self {
            node: node.clone(),
            input: input?,
        })
    }

    pub(super) fn apply(
        &self,
        ctx: &egui::Context,
        selected: Option<&NodeId>,
        input: Option<egui::Id>,
    ) {
        if selected == Some(&self.node) && input == Some(self.input) {
            // A newly opened pane may not have drawn this field yet. Mark it as a text
            // edit before shortcuts run, so the first typed j/k is text, not navigation.
            if egui::text_edit::TextEditState::load(ctx, self.input).is_none() {
                egui::text_edit::TextEditState::default().store(ctx, self.input);
            }
            ctx.memory_mut(|m| m.request_focus(self.input));
        }
    }
}

fn terminal_input(
    node: &Node,
    terminal: Option<&Terminal>,
    screen: Option<&ScreenUpdate>,
) -> Option<egui::Id> {
    if node.parked_at_ms.is_some() {
        return None;
    }
    let handle = ui::terminal::watch_handle(node)?;
    let terminal = terminal.filter(|t| t.handle == handle);
    let screen = screen.filter(|s| s.handle == handle);
    ui::terminal::input_block_reason(terminal, screen)
        .is_none()
        .then(|| ui::terminal::input_id(handle))
}

impl StrongholdApp {
    fn selected_input(&self) -> Option<egui::Id> {
        if self.clan.selection().len() > 1 {
            return None;
        }
        if self.king_selected() {
            return (*self.chat.state() == crate::chat::SessionState::Ready)
                .then(ui::chat::input_id);
        }
        let node = self.selected_node()?;
        let terminal = node.terminal_handle.as_deref().and_then(|handle| {
            self.data
                .as_ref()?
                .terminals
                .iter()
                .find(|t| t.handle == handle)
        });
        terminal_input(node, terminal, self.screen.as_ref())
    }

    pub(super) fn request_clicked_input(&mut self, node: &NodeId, modifiers: egui::Modifiers) {
        self.hold_input_focus = Request::from_click(node, modifiers, self.selected_input());
        if self.hold_input_focus.is_some() {
            self.bring_forward = Some(Pane::Conversation);
        }
    }

    pub(super) fn prepare_clicked_input(&self, ctx: &egui::Context) {
        if let Some(request) = &self.hold_input_focus {
            request.apply(ctx, self.selected.as_ref(), self.selected_input());
        }
    }

    pub(super) fn focus_clicked_input(&mut self, ctx: &egui::Context) {
        if let Some(request) = self.hold_input_focus.take() {
            request.apply(ctx, self.selected.as_ref(), self.selected_input());
        }
    }
}

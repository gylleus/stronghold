//! Envoy lifecycle controls alongside the Queen's quick actions.

use super::{MyrstackApp, repaint_once};
use crate::envoy::SessionState;

impl MyrstackApp {
    pub(super) fn envoy_quick_actions(&mut self, ui: &mut eframe::egui::Ui) {
        let mut status = self.envoy_session.status();
        if status.session == SessionState::Stopped {
            status.dry_run = self.whip_window.envoy().dry_run;
        }
        if let Some(start) = crate::ui::queen::envoy_control(ui, status) {
            if start {
                if let Err(error) = self
                    .envoy_session
                    .start(self.whip_window.envoy(), repaint_once(ui.ctx()))
                {
                    self.envoy_session.error = Some(error);
                }
                self.overlays_changed = true;
            } else if let Some(handle) = self.envoy_session.handle() {
                self.request_close(vec![crate::model::NodeId::terminal(handle)]);
            }
        }
        if let Some(error) = &self.envoy_session.error {
            ui.colored_label(ui.visuals().error_fg_color, "Envoy error")
                .on_hover_text(error);
        }
    }
}

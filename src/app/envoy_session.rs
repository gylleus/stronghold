//! Envoy lifecycle controls alongside the King's quick actions.

use super::{StrongholdApp, repaint_once};
use crate::envoy::{EnvoyIdentity, EnvoyStatus, SessionState};

impl StrongholdApp {
    pub(super) fn apply_envoy_settings(
        &mut self,
        settings: &crate::envoy::settings::EnvoySettings,
    ) {
        let applied = self.envoy_session.apply_policies(settings).and_then(|()| {
            self.envoy_core
                .configure(self.envoy_session.settings().clone())
        });
        self.envoy_core.block_dispatch(applied.is_err());
        if let Err(error) = applied {
            self.envoy_session.error = Some(error);
        }
        self.overlays_changed = true;
    }

    pub(super) fn envoy_status(&self) -> EnvoyStatus {
        let mut status = self.envoy_session.status();
        if self.envoy_core.envoy_handle().is_some() {
            self.envoy_core.apply_status(&mut status);
        }
        status
    }

    pub(super) fn sync_envoy_core(&mut self) {
        match self.envoy_session.running_handle() {
            Some(handle) if self.envoy_core.envoy_handle() != Some(handle) => {
                if let Err(error) = self
                    .envoy_core
                    .set_enabled(handle.to_owned(), self.envoy_session.settings().clone())
                {
                    self.envoy_session.error = Some(error);
                }
            }
            None if self.envoy_core.envoy_handle().is_some() => self.envoy_core.stop(),
            _ => {}
        }
    }

    pub(super) fn envoy_quick_actions(&mut self, ui: &mut eframe::egui::Ui) {
        let mut status = self.envoy_status();
        if status.session == SessionState::Stopped {
            status.dry_run = self.whip_window.envoy().dry_run;
        }
        if let Some(start) = crate::ui::king::envoy_control(ui, status) {
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

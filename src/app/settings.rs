//! The app side of the Settings window: what it shows and applying what it saved.

use eframe::egui;

use super::MyrstackApp;
use crate::chat::config::MODEL_ENV;
use crate::ui::settings::{GeneralInfo, PaneContext, QueenStatus};

impl MyrstackApp {
    /// Opens the Settings window on the section it showed last (⌘,, "Settings…").
    pub(super) fn open_settings(&mut self) {
        self.whip_window.open_settings();
    }

    /// Draws the Settings window and applies what was saved in it.
    pub(super) fn settings_window(&mut self, ctx: &egui::Context, now_ms: i64) {
        if !self.whip_window.open {
            return;
        }
        let general = self.general_info();
        let env_model = std::env::var(MODEL_ENV)
            .ok()
            .filter(|m| !m.trim().is_empty());
        let cx = PaneContext {
            whip_status: &self.whip_status,
            now_ms,
            queen: QueenStatus {
                auto_mode: self.chat.auto_mode(),
                running_model: self.chat.running_model(),
                env_model: env_model.as_deref(),
            },
            general: &general,
        };
        let out = self.whip_window.show(ctx, &cx);
        if let Some(whip) = out.whip {
            self.whip.update_settings(whip);
        }
        if let Some(queen) = &out.queen {
            self.chat.apply_queen_settings(queen);
        }
        if let Some(on) = out.auto_mode {
            self.chat.set_auto_mode(on);
        }
        if out.voice.is_some() {
            log::info!("settings: voice settings saved");
        }
    }

    /// Paths, build and playbook facts for the General section.
    fn general_info(&self) -> GeneralInfo {
        let (version, commit) = GeneralInfo::build();
        GeneralInfo {
            data_dir: crate::claude::hook::data_dir(),
            socket_path: crate::control::socket_path(),
            socket_listening: self.control.listening(),
            version,
            commit,
            playbook_dir: crate::playbook::Playbook::locate()
                .map(|p| p.dir().to_owned())
                .map_err(|e| format!("{e:#}")),
            playbook_version: self.playbook_version(),
        }
    }
}

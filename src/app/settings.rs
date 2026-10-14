//! The app side of the Settings window: what it shows and applying what it saved.

use eframe::egui;

use super::StrongholdApp;
use crate::chat::config::MODEL_ENV;
use crate::ui::settings::{GeneralInfo, KingStatus, NotifyStatus, PaneContext};

impl StrongholdApp {
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
        let env_model = crate::legacy::env(MODEL_ENV).filter(|m| !m.trim().is_empty());
        let (native, last_failure, system_sounds) = self.notify_status();
        let cx = PaneContext {
            whip_status: &self.whip_status,
            now_ms,
            king: KingStatus {
                auto_mode: self.chat.auto_mode(),
                running_model: self.chat.running_model(),
                env_model: env_model.as_deref(),
            },
            general: &general,
            voice: self.talk.voice_status(super::talk::speech_model_found()),
            notify: NotifyStatus {
                native,
                last_failure: last_failure.as_deref(),
                system_sounds: &system_sounds,
            },
        };
        let out = self.whip_window.show(ctx, &cx);
        if let Some(settings) = &out.envoy {
            self.apply_envoy_settings(settings);
        }
        if let Some(whip) = out.whip {
            self.whip.update_settings(whip);
        }
        if let Some(king) = &out.king {
            self.chat.apply_king_settings(king);
        }
        if let Some(on) = out.auto_mode {
            self.chat.set_auto_mode(on);
        }
        if let Some(voice) = &out.voice {
            log::info!("settings: voice settings saved");
            self.apply_voice_settings(voice);
        }
        if let Some(draft) = &out.test_voice {
            self.test_voice(draft);
        }
        if let Some(model) = out.download {
            self.download_voice_model(model);
        }
        if out.notifications.is_some() {
            log::info!("settings: notification settings saved");
        }
        self.notification_tests(out.test_sound, out.test_notification);
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

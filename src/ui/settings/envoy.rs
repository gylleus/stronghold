//! Settings for the next Envoy session.

use eframe::egui;

use super::{SettingsPane, queen, save_row};
use crate::envoy::settings::EnvoySettings;

pub(super) struct EnvoyDraft {
    settings: EnvoySettings,
    model: queen::QueenDraft,
    channels: String,
}

impl EnvoyDraft {
    pub(super) fn new(settings: &EnvoySettings) -> Self {
        Self {
            settings: settings.clone(),
            model: queen::QueenDraft::new(&crate::chat::settings::QueenSettings {
                model: settings.model.clone(),
                ..Default::default()
            }),
            channels: settings.channels.join("\n"),
        }
    }

    fn value(&self) -> EnvoySettings {
        EnvoySettings {
            model: self.model.settings.normalized().model,
            channels: self
                .channels
                .split(['\n', ','])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            ..self.settings.clone()
        }
    }
}

pub(super) fn section(pane: &mut SettingsPane, ui: &mut egui::Ui) {
    ui.weak("Changes apply on the next Start.");
    let draft = &mut pane.envoy_draft;
    egui::Grid::new("envoy_settings")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Agent");
            egui::ComboBox::from_id_salt("envoy_agent")
                .selected_text(&draft.settings.agent)
                .show_ui(ui, |ui| {
                    for agent in ["claude", "codex"] {
                        if ui
                            .selectable_value(&mut draft.settings.agent, agent.into(), agent)
                            .changed()
                        {
                            draft.model.pick(if agent == "claude" {
                                Some("sonnet")
                            } else {
                                None
                            });
                        }
                    }
                });
            ui.end_row();
            ui.label("Model");
            ui.push_id("envoy", |ui| queen::model_picker(ui, &mut draft.model));
            ui.end_row();
            ui.label("Poll interval (seconds)");
            ui.add(egui::DragValue::new(&mut draft.settings.poll_secs).range(1..=86400));
            ui.end_row();
            ui.label("Channels (IDs, one per line)");
            ui.add(egui::TextEdit::multiline(&mut draft.channels).desired_rows(3));
            ui.end_row();
        });
    ui.checkbox(
        &mut draft.settings.dry_run,
        "Dry run (record approvals and relays without sending)",
    );
    if !draft.settings.dry_run {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "Live mode: approved drafts post to Slack and relays reach agents.",
        );
    }
    if draft.settings.agent == "codex" {
        ui.weak("Codex uses a read-only sandbox; it has no per-command deny flag.");
    }
    let changed = draft.value() != pane.applied.envoy;
    let (save, revert) = save_row(ui, pane, changed, "Save Envoy settings");
    if revert {
        pane.envoy_draft = EnvoyDraft::new(&pane.applied.envoy);
    }
    if save {
        pane.save_envoy();
    }
}

impl SettingsPane {
    fn save_envoy(&mut self) {
        let settings = self.envoy_draft.value();
        if let Some(model) = &settings.model
            && !crate::spawn::valid_model(model)
        {
            self.message = Some((true, "Invalid model ID".into()));
            return;
        }
        let mut next = self.applied.clone();
        next.envoy = settings;
        self.commit(next);
        self.envoy_draft = EnvoyDraft::new(&self.applied.envoy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_preserves_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut pane = SettingsPane::load(Some(path.clone()));
        pane.envoy_draft.settings.agent = "codex".into();
        pane.envoy_draft.settings.dry_run = false;
        pane.envoy_draft.settings.poll_secs = 30;
        pane.envoy_draft.channels = " C_TEST \n C_OTHER\n".into();
        pane.envoy_draft.model.pick_custom("test-model");
        pane.save_envoy();
        let loaded = SettingsPane::load(Some(path));
        assert_eq!(loaded.envoy(), pane.envoy());
        assert_eq!(loaded.envoy().channels, ["C_TEST", "C_OTHER"]);
        assert_eq!(loaded.envoy().model.as_deref(), Some("test-model"));
        assert_eq!(loaded.queen(), pane.queen());
        assert!(!loaded.envoy().dry_run);
    }
}

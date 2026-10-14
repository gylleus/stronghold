//! Envoy preferences and its immediate dry-run safety switch.

use eframe::egui;

use super::{SettingsPane, king, save_row};
use crate::envoy::settings::EnvoySettings;

pub(super) struct EnvoyDraft {
    settings: EnvoySettings,
    model: king::KingDraft,
    channels: String,
}

impl EnvoyDraft {
    pub(super) fn new(settings: &EnvoySettings) -> Self {
        Self {
            settings: settings.clone(),
            model: king::KingDraft::new(&crate::chat::settings::KingSettings {
                model: settings.model.clone(),
                ..Default::default()
            }),
            channels: settings.channels.join("\n"),
        }
    }

    fn value(&self) -> EnvoySettings {
        EnvoySettings {
            poll_secs: self
                .settings
                .poll_secs
                .max(crate::envoy::settings::MIN_POLL_SECS),
            model: self.model.settings.normalized().model,
            channels: self
                .channels
                .split(['\n', ','])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            karl_user_id: self.settings.karl_user_id.trim().to_owned(),
            github_owner: self.settings.github_owner.trim().to_owned(),
            ..self.settings.clone()
        }
    }
}

pub(super) fn section(pane: &mut SettingsPane, ui: &mut egui::Ui, out: &mut super::PaneOutput) {
    ui.weak("Policies and dry run ON apply immediately when saved. Other changes apply on the next Start.");
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
            ui.push_id("envoy", |ui| {
                king::model_picker(ui, "envoy", &mut draft.model)
            });
            ui.end_row();
            ui.label("Poll interval (seconds)");
            ui.add(
                egui::DragValue::new(&mut draft.settings.poll_secs)
                    .range(crate::envoy::settings::MIN_POLL_SECS..=86400),
            );
            ui.end_row();
            ui.label("Channels (IDs, one per line)");
            ui.add(egui::TextEdit::multiline(&mut draft.channels).desired_rows(3));
            ui.end_row();
            ui.label("Karl's Slack member id")
                .on_hover_text("Whose mentions and DMs are pings to Karl. Slack's auth.test replaces it at start.");
            ui.text_edit_singleline(&mut draft.settings.karl_user_id);
            ui.end_row();
            ui.label("GitHub owner")
                .on_hover_text("Owner of the repos Slack names as repo#N, used to look up their PR branches; empty turns the lookup off.");
            ui.text_edit_singleline(&mut draft.settings.github_owner);
            ui.end_row();
            ui.label("Fresh context after quiet (seconds)")
                .on_hover_text("A batch typed after this much quiet starts from a cleared context; 0 clears before every batch. Bursts share one context.");
            ui.add(egui::DragValue::new(&mut draft.settings.fresh_after_secs).range(0..=86400));
            ui.end_row();
            ui.label("Reset after events")
                .on_hover_text("Clear the context at the next idle point once this many events went into it; 0 turns the guard off.");
            ui.add(egui::DragValue::new(&mut draft.settings.reset_after_events).range(0..=10_000));
            ui.end_row();
            ui.label("Reset after tokens")
                .on_hover_text("Clear the context at the next idle point once the last turn's context passed this many tokens; 0 turns the guard off.");
            ui.add(
                egui::DragValue::new(&mut draft.settings.reset_after_tokens)
                    .range(0..=1_000_000)
                    .speed(1_000),
            );
            ui.end_row();
        });
    ui.separator();
    ui.strong("Approval policies");
    ui.weak("Exact destination, then dm, then *. Unlisted destinations require your approval.");
    let mut remove = None;
    for (i, policy) in draft.settings.policies.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut policy.destination).desired_width(120.0));
                let mut auto = matches!(
                    policy.mode,
                    crate::envoy::settings::ApprovalMode::Auto { .. }
                );
                if ui.checkbox(&mut auto, "Auto").changed() {
                    policy.mode = if auto {
                        crate::envoy::settings::ApprovalMode::Auto { veto_secs: 120 }
                    } else {
                        crate::envoy::settings::ApprovalMode::Approve
                    };
                }
                if let crate::envoy::settings::ApprovalMode::Auto { veto_secs } = &mut policy.mode {
                    ui.add(
                        egui::DragValue::new(veto_secs)
                            .range(0..=86400)
                            .suffix("s veto"),
                    );
                }
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        draft.settings.policies.remove(i);
    }
    if ui.button("Add policy").clicked() {
        draft
            .settings
            .policies
            .push(crate::envoy::settings::Policy {
                destination: "*".into(),
                mode: Default::default(),
            });
    }
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
        ui.weak("Codex Envoy starts are unavailable: its MCP tool restrictions are not supported. Choose Claude.");
    }
    let changed = draft.value() != pane.applied.envoy;
    let (save, revert) = save_row(ui, pane, changed, "Save Envoy settings");
    if revert {
        pane.envoy_draft = EnvoyDraft::new(&pane.applied.envoy);
    }
    if save {
        out.envoy = pane.save_envoy();
    }
}

impl SettingsPane {
    fn save_envoy(&mut self) -> Option<EnvoySettings> {
        let settings = self.envoy_draft.value();
        if let Err(e) = settings.validate_policies() {
            self.message = Some((true, e));
            return None;
        }
        if let Some(model) = &settings.model
            && !crate::spawn::valid_model(model)
        {
            self.message = Some((true, "Invalid model ID".into()));
            return None;
        }
        let mut next = self.applied.clone();
        next.envoy = settings;
        self.commit(next);
        self.envoy_draft = EnvoyDraft::new(&self.applied.envoy);
        Some(self.applied.envoy.clone())
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
        let applied = pane.save_envoy().unwrap();
        assert_eq!(&applied, pane.envoy());
        let loaded = SettingsPane::load(Some(path));
        assert_eq!(loaded.envoy(), pane.envoy());
        assert_eq!(loaded.envoy().channels, ["C_TEST", "C_OTHER"]);
        assert_eq!(loaded.envoy().model.as_deref(), Some("test-model"));
        assert_eq!(loaded.king(), pane.king());
        assert!(!loaded.envoy().dry_run);
    }

    #[test]
    fn draft_cannot_save_a_poll_interval_below_thirty() {
        let mut draft = EnvoyDraft::new(&EnvoySettings::default());
        draft.settings.poll_secs = 1;
        assert_eq!(draft.value().poll_secs, 30);
    }
}

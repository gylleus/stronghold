//! Model, mode and schedule for future Ledger maintenance runs.

use super::{
    SettingsPane,
    king::{KingDraft, model_picker},
    save_row,
};
use crate::lorekeeper::{Interval, Mode, Settings};
use eframe::egui;

pub(super) struct Draft {
    pub settings: Settings,
    pub model: KingDraft,
}

impl Draft {
    pub fn new(settings: &Settings) -> Self {
        Self {
            settings: settings.clone(),
            model: KingDraft::new(&crate::chat::settings::KingSettings {
                model: Some(settings.model.clone()),
                ..Default::default()
            }),
        }
    }
}

pub(super) fn section(pane: &mut SettingsPane, ui: &mut egui::Ui) {
    ui.label("The Lorekeeper verifies Ledger entries against code and pull requests.");
    egui::Grid::new("lorekeeper_settings")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Model");
            model_picker(ui, "lorekeeper", &mut pane.lorekeeper_draft.model);
            ui.end_row();
            ui.label("Mode");
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut pane.lorekeeper_draft.settings.mode,
                    Mode::DryRun,
                    "Dry run",
                );
                ui.selectable_value(&mut pane.lorekeeper_draft.settings.mode, Mode::Live, "Live");
            });
            ui.end_row();
            ui.label("Schedule");
            ui.horizontal(|ui| {
                let draft = &mut pane.lorekeeper_draft.settings;
                if ui.selectable_label(!draft.enabled, "Off").clicked() {
                    draft.enabled = false;
                }
                for (value, label) in [(Interval::Daily, "Daily"), (Interval::Weekly, "Weekly")] {
                    if ui
                        .selectable_label(draft.enabled && draft.interval == value, label)
                        .clicked()
                    {
                        draft.enabled = true;
                        draft.interval = value;
                    }
                }
            });
            ui.end_row();
        });
    ui.weak("Dry run reports proposed changes. Live can update or delete entries; a snapshot is kept before every run. Scheduling runs while Stronghold is open.");
    pane.lorekeeper_draft.settings.model = pane
        .lorekeeper_draft
        .model
        .settings
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "sonnet".into());
    let changed = pane.lorekeeper_draft.settings != pane.applied.lorekeeper;
    let (save, revert) = save_row(ui, pane, changed, "Save for future runs");
    if revert {
        pane.lorekeeper_draft = Draft::new(&pane.applied.lorekeeper);
    }
    if save {
        let mut next = pane.applied.clone();
        next.lorekeeper = pane.lorekeeper_draft.settings.clone();
        pane.commit(next);
        pane.lorekeeper_draft = Draft::new(&pane.applied.lorekeeper);
    }
}

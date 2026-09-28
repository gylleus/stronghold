//! The Queen section: her model and effort (saving restarts her `claude` process on the same
//! conversation), auto mode, and the spoken-replies placeholder.

use eframe::egui;

use super::{PaneOutput, SettingsPane, save_row};
use crate::chat::config::MODEL_ENV;
use crate::chat::settings::{EFFORT_LEVELS, MODEL_ALIASES, MODEL_IDS, QueenSettings};

/// Label of the "no `--model`" / "no `--effort`" choice.
pub const CLI_DEFAULT: &str = "Default (CLI default)";
/// Label of the free-text model choice.
pub const CUSTOM: &str = "Custom…";

/// What the Queen section shows about the running chat.
#[derive(Debug, Clone, Copy, Default)]
pub struct QueenStatus<'a> {
    pub auto_mode: bool,
    /// The model the chat header shows (from the CLI's `system/init`, or the one requested).
    pub running_model: Option<&'a str>,
    /// `MYRSTACK_CHAT_MODEL`, when set: it wins over the saved model.
    pub env_model: Option<&'a str>,
}

/// The edited Queen settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueenDraft {
    pub(super) settings: QueenSettings,
    /// "Custom…" is picked: the model comes from `custom_text`.
    pub(super) custom: bool,
    custom_text: String,
}

impl QueenDraft {
    pub(super) fn new(saved: &QueenSettings) -> Self {
        let model = saved.model.as_deref();
        let custom = model.is_some_and(|m| !is_listed(m));
        Self {
            settings: saved.clone(),
            custom,
            custom_text: if custom {
                model.unwrap_or_default().to_owned()
            } else {
                String::new()
            },
        }
    }

    /// Picks a listed model (`None`: the CLI default).
    pub(super) fn pick(&mut self, model: Option<&str>) {
        self.custom = false;
        self.settings.model = model.map(str::to_owned);
    }

    /// Picks "Custom…" with `text`.
    pub(super) fn pick_custom(&mut self, text: &str) {
        self.custom = true;
        self.custom_text = text.to_owned();
        self.settings.model = Some(text.to_owned());
    }
}

fn is_listed(model: &str) -> bool {
    MODEL_ALIASES.contains(&model) || MODEL_IDS.contains(&model)
}

pub(super) fn section(
    pane: &mut SettingsPane,
    ui: &mut egui::Ui,
    status: &QueenStatus<'_>,
    out: &mut PaneOutput,
) {
    ui.label("The Queen is the Claude Code session behind the chat.");
    ui.add_space(4.0);
    egui::Grid::new("queen_settings")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Model");
            model_picker(ui, &mut pane.queen_draft);
            ui.end_row();

            ui.label("Effort");
            effort_picker(ui, &mut pane.queen_draft.settings.effort);
            ui.end_row();

            ui.label("Running");
            ui.label(status.running_model.unwrap_or("CLI default"))
                .on_hover_text("As the chat header shows it; the CLI reports the full model id with its next reply");
            ui.end_row();
        });
    if let Some(env) = status.env_model {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!(
                "Overridden by env: {MODEL_ENV}={env}. The Queen runs {env}; the model above \
                 applies once the variable is unset."
            ),
        );
    }

    let changed = pane.queen_draft.settings.normalized() != pane.applied.queen;
    let (save, revert) = save_row(ui, pane, changed, "Save and restart the Queen");
    if revert {
        pane.queen_draft = QueenDraft::new(&pane.applied.queen);
    }
    if save {
        out.queen = pane.save_queen();
    }
    ui.weak("Saving restarts her claude process and resumes the same conversation.");

    ui.separator();
    let mut auto = status.auto_mode;
    if ui
        .checkbox(
            &mut auto,
            "Auto mode: her Myrstack actions run without asking",
        )
        .on_hover_text("The same switch as Auto in the chat header; applies at once")
        .changed()
    {
        out.auto_mode = Some(auto);
    }
    let mut speak = false;
    ui.add_enabled(false, egui::Checkbox::new(&mut speak, "Speak replies"))
        .on_disabled_hover_text("Arrives with the voice feature");
    ui.weak("Speak replies arrives with the voice feature.");
}

fn model_picker(ui: &mut egui::Ui, draft: &mut QueenDraft) {
    ui.vertical(|ui| {
        let selected = if draft.custom {
            CUSTOM.to_owned()
        } else {
            draft
                .settings
                .model
                .clone()
                .unwrap_or_else(|| CLI_DEFAULT.to_owned())
        };
        egui::ComboBox::from_id_salt("queen_model")
            .selected_text(selected)
            .width(220.0)
            .show_ui(ui, |ui| {
                let current = (!draft.custom).then(|| draft.settings.model.clone());
                if ui
                    .selectable_label(current == Some(None), CLI_DEFAULT)
                    .clicked()
                {
                    draft.pick(None);
                }
                ui.separator();
                ui.weak("Aliases (latest of each family)");
                for (i, list) in [MODEL_ALIASES, MODEL_IDS].into_iter().enumerate() {
                    if i == 1 {
                        ui.separator();
                        ui.weak("Model ids");
                    }
                    for model in list {
                        let on = current.as_ref().and_then(|m| m.as_deref()) == Some(*model);
                        if ui.selectable_label(on, *model).clicked() {
                            draft.pick(Some(model));
                        }
                    }
                }
                ui.separator();
                if ui.selectable_label(draft.custom, CUSTOM).clicked() {
                    let text = draft.custom_text.clone();
                    draft.pick_custom(&text);
                }
            });
        if draft.custom {
            let mut text = draft.custom_text.clone();
            let edit = egui::TextEdit::singleline(&mut text)
                .hint_text("e.g. claude-opus-5-5")
                .desired_width(220.0);
            if ui.add(edit).changed() {
                draft.pick_custom(&text);
            }
        }
    });
}

fn effort_picker(ui: &mut egui::Ui, effort: &mut Option<String>) {
    egui::ComboBox::from_id_salt("queen_effort")
        .selected_text(effort.clone().unwrap_or_else(|| CLI_DEFAULT.to_owned()))
        .width(220.0)
        .show_ui(ui, |ui| {
            if ui.selectable_label(effort.is_none(), CLI_DEFAULT).clicked() {
                *effort = None;
            }
            for level in EFFORT_LEVELS {
                if ui
                    .selectable_label(effort.as_deref() == Some(*level), *level)
                    .clicked()
                {
                    *effort = Some((*level).to_owned());
                }
            }
        });
}

//! "Whip…" top-bar button with its DRY RUN / LIVE badge, and the whip settings window.

use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText};

use crate::ui::table::format_age;
use crate::whip::{Settings, WhipSettings, WhipStatus};

const SEC: i64 = 1_000;

/// Settings window state: the applied settings, the edited draft and the file they live in.
pub struct WhipSettingsWindow {
    pub open: bool,
    /// Settings as loaded or last saved; `whip` is what the runner uses.
    applied: Settings,
    draft: WhipSettings,
    path: Option<PathBuf>,
    /// Result of the last load or save: `(is_error, text)`.
    message: Option<(bool, String)>,
}

impl WhipSettingsWindow {
    /// Loads the settings from `path` (defaults when `None` or missing).
    pub fn load(path: Option<PathBuf>) -> Self {
        let (applied, message) = match &path {
            Some(p) => {
                let loaded = Settings::load_from(p);
                (loaded.settings, loaded.warning.map(|w| (true, w)))
            }
            None => (
                Settings::default(),
                Some((true, "no config directory; settings are not saved".into())),
            ),
        };
        Self {
            open: false,
            draft: applied.whip.clone(),
            applied,
            path,
            message,
        }
    }

    /// The whip settings in effect.
    pub fn whip(&self) -> &WhipSettings {
        &self.applied.whip
    }

    /// Top-bar button plus mode badge; opens the window on click.
    pub fn button(&mut self, ui: &mut egui::Ui) {
        if ui.button("Whip…").clicked() {
            self.draft = self.applied.whip.clone();
            self.open = true;
        }
        let whip = &self.applied.whip;
        let (text, color) = if !whip.enabled {
            ("OFF", ui.visuals().weak_text_color())
        } else if whip.dry_run {
            ("DRY RUN", Color32::from_rgb(0xd0, 0x9a, 0x2a))
        } else {
            ("LIVE", Color32::from_rgb(0xd0, 0x40, 0x40))
        };
        ui.label(RichText::new(text).strong().small().color(color))
            .on_hover_text(match text {
                "LIVE" => "The whip sends nudges and notifications",
                "DRY RUN" => "The whip only logs what it would do",
                _ => "The whip is disabled",
            });
    }

    /// Draws the window if open. Returns the new settings when the user saved them.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        status: &WhipStatus,
        now_ms: i64,
    ) -> Option<WhipSettings> {
        if !self.open {
            return None;
        }
        let mut open = true;
        let mut saved = None;
        egui::Window::new("Whip")
            .open(&mut open)
            .default_width(560.0)
            .collapsible(false)
            .show(ctx, |ui| {
                saved = self.form(ui);
                ui.separator();
                status_view(ui, status, now_ms);
            });
        self.open = open;
        saved
    }

    fn form(&mut self, ui: &mut egui::Ui) -> Option<WhipSettings> {
        let d = &mut self.draft;
        ui.checkbox(&mut d.enabled, "Enabled");
        ui.checkbox(&mut d.dry_run, "Dry run (log only, send nothing)");
        if d.enabled && !d.dry_run {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Live: nudges are sent to agents on the bus and typed into idle terminals.",
            );
        }
        egui::Grid::new("whip_settings")
            .num_columns(2)
            .show(ui, |ui| {
                secs_row(ui, "Stall threshold", &mut d.threshold_ms);
                secs_row(ui, "Nudge cooldown", &mut d.cooldown_ms);
                ui.label("Nudges before escalating");
                ui.add(egui::DragValue::new(&mut d.max_nudges).range(0..=20));
                ui.end_row();
                secs_row(ui, "Tick every", &mut d.tick_ms);
            });
        ui.checkbox(
            &mut d.notify,
            "Notify on escalation and long NeedsInput waits",
        );

        let mut saved = None;
        ui.horizontal(|ui| {
            let changed = self.draft != self.applied.whip;
            if ui.add_enabled(changed, egui::Button::new("Save")).clicked() {
                saved = self.save();
            }
            if ui
                .add_enabled(changed, egui::Button::new("Revert"))
                .clicked()
            {
                self.draft = self.applied.whip.clone();
            }
            if let Some(path) = &self.path {
                ui.weak(path.display().to_string());
            }
        });
        if let Some((is_error, text)) = &self.message {
            let color = if *is_error {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().weak_text_color()
            };
            ui.colored_label(color, text);
        }
        saved
    }

    /// Validates and writes the draft, then makes it the applied settings.
    fn save(&mut self) -> Option<WhipSettings> {
        if let Err(e) = self.draft.validate() {
            self.message = Some((true, e));
            return None;
        }
        let mut next = self.applied.clone();
        next.whip = self.draft.clone();
        self.message = match &self.path {
            Some(path) => match next.save_to(path) {
                Ok(()) => Some((false, "Saved.".into())),
                Err(e) => Some((true, format!("Applied, but saving failed: {e}"))),
            },
            None => Some((
                true,
                "Applied for this session only (no settings path).".into(),
            )),
        };
        self.applied = next;
        Some(self.applied.whip.clone())
    }
}

/// A duration setting edited in whole seconds.
fn secs_row(ui: &mut egui::Ui, label: &str, ms: &mut i64) {
    ui.label(label);
    let mut secs = *ms / SEC;
    if ui
        .add(
            egui::DragValue::new(&mut secs)
                .range(1..=24 * 60 * 60)
                .suffix(" s"),
        )
        .changed()
    {
        *ms = secs * SEC;
    }
    ui.end_row();
}

fn status_view(ui: &mut egui::Ui, status: &WhipStatus, now_ms: i64) {
    ui.horizontal(|ui| {
        match status.last_tick_ms {
            Some(t) => ui.label(format!("Last tick {} ago", format_age(now_ms - t))),
            None => ui.label("Waiting for the first tick"),
        };
        ui.separator();
        ui.label(format!("{} stall findings", status.findings));
        ui.separator();
        ui.label(format!("{} actions logged", status.actions_logged));
    });
    if !status.escalated.is_empty() {
        let list: Vec<&str> = status.escalated.iter().map(String::as_str).collect();
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("Escalated: {}", list.join(", ")),
        );
    }
    ui.weak("The action log is in the sidebar's Whip section.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_writes_the_file_and_applies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut window = WhipSettingsWindow::load(Some(path.clone()));
        assert!(window.whip().dry_run);
        assert_eq!(window.message, None);

        window.draft.dry_run = false;
        window.draft.threshold_ms = 60 * SEC;
        let saved = window.save().unwrap();
        assert!(!saved.dry_run);
        assert_eq!(window.whip(), &saved);
        assert_eq!(Settings::load_from(&path).settings.whip, saved);
    }

    #[test]
    fn invalid_draft_is_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut window = WhipSettingsWindow::load(Some(path.clone()));
        window.draft.tick_ms = 0;
        assert_eq!(window.save(), None);
        assert!(window.message.as_ref().unwrap().0);
        assert!(!path.exists());
        assert_eq!(window.whip(), &WhipSettings::default());
    }
}

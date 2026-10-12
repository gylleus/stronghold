//! The Whip section: the whip's settings form, its status and "Whip all stalled now".

use eframe::egui::{self, Color32, RichText};

use super::{SettingsPane, save_row};
use crate::ui::fleet_view::format_age;
use crate::whip::{WhipSettings, WhipStatus};

const SEC: i64 = 1_000;
pub(super) const HOUR: i64 = 60 * 60 * SEC;
/// Longest abandon window the form offers: 30 days.
pub(super) const MAX_ABANDON_HOURS: i64 = 30 * 24;

/// The DRY RUN / LIVE / OFF badge next to the top bar's "Whip…" button.
pub fn mode_badge(ui: &mut egui::Ui, whip: &WhipSettings) {
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
            "DRY RUN" => {
                "The whip only logs the nudges it would send; event forwarding to coordinators \
                 stays live"
            }
            _ => "The whip is disabled",
        });
}

/// The settings form; returns the new settings when the user saved them.
pub(super) fn form(pane: &mut SettingsPane, ui: &mut egui::Ui) -> Option<WhipSettings> {
    let d = &mut pane.whip_draft;
    ui.checkbox(&mut d.enabled, "Enabled");
    ui.checkbox(&mut d.dry_run, "Dry run (log nudges, send none)")
        .on_hover_text("Event forwarding to coordinators stays live in dry run");
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
            hours_row(ui, "Leave runs alone after", &mut d.abandon_after_ms).on_hover_text(
                "A Run with no activity (bus messages, task changes, worker progress) \
                 for this long is abandoned and never nudged",
            );
        });
    ui.checkbox(
        &mut d.notify,
        "Notify on escalation and long NeedsInput waits",
    );

    let changed = pane.whip_draft != pane.applied.whip;
    let (save, revert) = save_row(ui, pane, changed, "Save");
    if revert {
        pane.whip_draft = pane.applied.whip.clone();
    }
    if save { pane.save_whip() } else { None }
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

/// A duration setting edited in whole hours. A value that is not a whole number of hours (set
/// in the file) is kept until the field is edited.
fn hours_row(ui: &mut egui::Ui, label: &str, ms: &mut i64) -> egui::Response {
    ui.label(label);
    let mut hours = (*ms + HOUR - 1) / HOUR;
    let response = ui.add(
        egui::DragValue::new(&mut hours)
            .range(1..=MAX_ABANDON_HOURS)
            .suffix(" h"),
    );
    if response.changed() {
        *ms = hours * HOUR;
    }
    ui.end_row();
    response
}

/// The settings file's own checks plus the form's: the abandon window must be longer than the
/// stall threshold, or nothing could ever stall before being abandoned.
pub(super) fn validate_draft(draft: &WhipSettings) -> Result<(), String> {
    draft.validate()?;
    if draft.abandon_after_ms <= draft.threshold_ms {
        return Err(format!(
            "abandon window ({}) must be longer than the stall threshold ({})",
            format_age(draft.abandon_after_ms),
            format_age(draft.threshold_ms)
        ));
    }
    if draft.abandon_after_ms > MAX_ABANDON_HOURS * HOUR {
        return Err(format!(
            "abandon window must be at most {MAX_ABANDON_HOURS} h"
        ));
    }
    Ok(())
}

pub(super) fn status_view(ui: &mut egui::Ui, status: &WhipStatus, now_ms: i64) {
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

//! Scheduled cleanup controls and the last batch's quiet summary.

use eframe::egui;

use super::{PaneOutput, SettingsPane};
use crate::worktrees::automatic::{Interval, Run};

#[derive(Default)]
pub struct Status<'a> {
    pub last: Option<&'a Run>,
    pub running: bool,
    pub busy: bool,
}

pub(super) fn section(
    pane: &mut SettingsPane,
    ui: &mut egui::Ui,
    status: &Status<'_>,
    out: &mut PaneOutput,
) {
    let mut settings = pane.applied.worktrees;
    ui.checkbox(&mut settings.enabled, "Automatic cleanup");
    ui.horizontal(|ui| {
        ui.label("Every");
        egui::ComboBox::from_id_salt("cleanup_interval")
            .selected_text(settings.interval.label())
            .show_ui(ui, |ui| {
                for interval in Interval::ALL {
                    ui.selectable_value(&mut settings.interval, interval, interval.label());
                }
            });
    });
    if settings != pane.applied.worktrees {
        let mut next = pane.applied.clone();
        next.worktrees = settings;
        pane.commit(next);
    }
    ui.weak("While Stronghold is open, remove only merged worktrees that pass the cleanup checks. Work needing an archive decision stays. Live or parked agents, running processes, open PRs, pins and protected recent work stay.");
    ui.separator();
    if status.running {
        ui.label("Auto-cleanup running…");
    }
    if let Some(last) = status.last {
        let time = jiff::Timestamp::from_millisecond(last.at_ms)
            .map(|t| {
                t.to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|_| "unknown".into());
        ui.label(format!("Last run: {time}"));
        ui.label(&last.summary);
    } else {
        ui.weak("Last run: never");
    }
    if ui
        .add_enabled(!status.busy, egui::Button::new("Run now"))
        .on_hover_text("Run these same checks once, even when the schedule is off.")
        .clicked()
    {
        out.run_cleanup = true;
    }
}

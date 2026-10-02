//! The top bar's health badge: a green, amber or red dot that opens the list of checks, each
//! with what was found and a one-line fix.

use eframe::egui::{self, Color32, RichText, Ui};

use crate::health::{HealthReport, Level};

/// Green of a passing check (as the review queue's passing CI).
const OK_GREEN: Color32 = Color32::from_rgb(0x2d, 0xa4, 0x4e);

/// The color of a level.
pub fn level_color(level: Level, ui: &Ui) -> Color32 {
    match level {
        Level::Ok => OK_GREEN,
        Level::Off => ui.visuals().weak_text_color(),
        Level::Warn => ui.visuals().warn_fg_color,
        Level::Error => ui.visuals().error_fg_color,
    }
}

/// The badge's text: a dot, plus the problem count when there are problems.
pub fn badge_text(report: &HealthReport) -> String {
    match report.problems().count() {
        0 => "●".into(),
        1 => "● 1 issue".into(),
        n => format!("● {n} issues"),
    }
}

/// What the user did in the badge's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthAction {
    /// Run the slow checks again now.
    CheckNow,
    OpenLog,
}

/// Draws the badge and, when clicked, the checks; returns what was clicked in the menu.
pub fn health_badge(ui: &mut Ui, report: &HealthReport, now_ms: i64) -> Option<HealthAction> {
    let color = level_color(report.level, ui);
    let mut action = None;
    let response = ui
        .menu_button(RichText::new(badge_text(report)).color(color), |ui| {
            ui.set_min_width(360.0);
            ui.strong("Health");
            ui.add_space(4.0);
            egui::Grid::new("health_checks")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    for check in &report.checks {
                        ui.label(RichText::new("●").color(level_color(check.level, ui)));
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.strong(&check.name);
                                ui.label(RichText::new(&check.detail).weak());
                            });
                            if let Some(fix) = &check.fix {
                                ui.label(RichText::new(format!("Fix: {fix}")).small());
                            }
                        });
                        ui.end_row();
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                let checked = report.checked_at_ms.map_or_else(
                    || "checking…".to_owned(),
                    |at| format!("checked {} ago", crate::ui::table::format_age(now_ms - at)),
                );
                ui.label(RichText::new(checked).small().weak());
                if ui.button("Check now").clicked() {
                    action = Some(HealthAction::CheckNow);
                }
                if ui.button("Open log").clicked() {
                    action = Some(HealthAction::OpenLog);
                }
            });
        })
        .response;
    response.on_hover_ui(|ui| {
        let problems: Vec<_> = report.problems().collect();
        if problems.is_empty() {
            ui.label("Health: all checks pass");
        }
        for check in problems {
            ui.label(format!("{}: {}", check.name, check.detail));
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::Check;

    fn report(levels: &[Level]) -> HealthReport {
        HealthReport {
            level: levels.iter().copied().max().unwrap_or_default(),
            checked_at_ms: Some(0),
            checks: levels
                .iter()
                .map(|&level| Check {
                    key: "k".into(),
                    name: "Check".into(),
                    level,
                    detail: "detail".into(),
                    fix: Some("do this".into()),
                })
                .collect(),
        }
    }

    #[test]
    fn badge_counts_only_warnings_and_errors() {
        assert_eq!(badge_text(&report(&[Level::Ok, Level::Off])), "●");
        assert_eq!(badge_text(&report(&[Level::Warn, Level::Ok])), "● 1 issue");
        assert_eq!(
            badge_text(&report(&[Level::Warn, Level::Error])),
            "● 2 issues"
        );
    }

    #[test]
    fn clicking_the_badge_opens_its_menu_without_acting() {
        let report = report(&[Level::Warn]);
        let action =
            crate::ui::test_util::click_label("● 1 issue", |ui| health_badge(ui, &report, 5_000));
        assert_eq!(action, None);
    }
}

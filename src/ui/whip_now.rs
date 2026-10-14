//! Manual whip UI: the confirmation popover and the result toast.
//!
//! A manual whip is sent at once unless a target waits on the user's input or it covers
//! [`CONFIRM_AT`](crate::whip::manual::CONFIRM_AT) or more targets; then a small popover lists
//! the targets (with warnings and skips) and asks first. Results show in a toast and in the
//! whip log.

use eframe::egui::{self, RichText};

use crate::whip::WhipNowReport;
use crate::whip::manual::{ManualNudge, ManualPlan, ManualSkip, needs_confirmation, nudges};

/// How long a result toast stays up.
pub const TOAST_MS: i64 = 6_000;

/// A short result line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub at_ms: i64,
}

/// A confirmed whip: the nudges to send and the skipped nodes.
pub type Confirmed = (Vec<ManualNudge>, Vec<ManualSkip>);

/// Popover and toast state.
#[derive(Debug, Default)]
pub struct WhipNowUi {
    /// Plans waiting for confirmation.
    confirm: Option<Vec<ManualPlan>>,
    toast: Option<Toast>,
}

impl WhipNowUi {
    /// Starts a manual whip of `plans`: the nudges to send now, or `None` when there is nothing
    /// to send (a toast says why) or the popover must confirm first.
    pub fn request(&mut self, plans: Vec<ManualPlan>, now_ms: i64) -> Option<Vec<ManualNudge>> {
        let send = nudges(&plans);
        if send.is_empty() {
            let skips: Vec<&ManualSkip> = plans.iter().filter_map(ManualPlan::skip).collect();
            self.set_toast(nothing_to_whip_text(&skips), true, now_ms);
            return None;
        }
        if needs_confirmation(&plans) {
            self.confirm = Some(plans);
            return None;
        }
        Some(send)
    }

    pub fn is_confirming(&self) -> bool {
        self.confirm.is_some()
    }

    pub fn toast(&self) -> Option<&Toast> {
        self.toast.as_ref()
    }

    pub fn set_toast(&mut self, text: String, error: bool, now_ms: i64) {
        self.toast = Some(Toast {
            text,
            error,
            at_ms: now_ms,
        });
    }

    /// Draws the popover (when confirming) and the toast. Returns the nudges to send and the
    /// skipped nodes when the popover was confirmed.
    pub fn show(&mut self, ctx: &egui::Context, now_ms: i64) -> Option<Confirmed> {
        let confirmed = self.confirm_popover(ctx);
        if self
            .toast
            .as_ref()
            .is_some_and(|t| now_ms - t.at_ms > TOAST_MS)
        {
            self.toast = None;
        }
        if let Some(toast) = &self.toast {
            egui::Area::new(egui::Id::new("whip_now_toast"))
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -48.0))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(520.0);
                        let color = if toast.error {
                            ui.visuals().warn_fg_color
                        } else {
                            ui.visuals().strong_text_color()
                        };
                        ui.label(RichText::new(&toast.text).color(color));
                    });
                });
        }
        confirmed
    }

    fn confirm_popover(&mut self, ctx: &egui::Context) -> Option<Confirmed> {
        let plans = self.confirm.as_ref()?;
        let mut open = true;
        let mut choice = None;
        egui::Window::new("Whip now?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| choice = confirm_view(ui, plans));
        match choice {
            Some(true) => self.confirm.take().map(|p| {
                let skips = p.iter().filter_map(ManualPlan::skip).cloned().collect();
                (nudges(&p), skips)
            }),
            Some(false) => {
                self.confirm = None;
                None
            }
            None => {
                if !open {
                    self.confirm = None;
                }
                None
            }
        }
    }
}

/// The popover's body; `Some(true)` on "Whip", `Some(false)` on "Cancel".
fn confirm_view(ui: &mut egui::Ui, plans: &[ManualPlan]) -> Option<bool> {
    let send = nudges(plans);
    ui.label(format!(
        "Send a Stronghold nudge to {} agent{} now? Manual whips are sent even when the \
         automatic whip is in dry-run.",
        send.len(),
        if send.len() == 1 { "" } else { "s" }
    ));
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .max_height(240.0)
        .show(ui, |ui| {
            for plan in plans {
                match plan {
                    ManualPlan::Nudge(n) => {
                        ui.label(format!("• {} — {}", n.label, n.via.label()));
                        if let Some(warn) = &n.warn {
                            ui.colored_label(ui.visuals().warn_fg_color, format!("   ⚠ {warn}"));
                        }
                    }
                    ManualPlan::Skip(s) => {
                        ui.weak(format!("• {} — skipped: {}", s.label, s.reason));
                    }
                }
            }
        });
    ui.add_space(4.0);
    let mut choice = None;
    ui.horizontal(|ui| {
        if ui
            .button(RichText::new(format!("Whip {}", send.len())).strong())
            .clicked()
        {
            choice = Some(true);
        }
        if ui.button("Cancel").clicked() {
            choice = Some(false);
        }
    });
    choice
}

fn nothing_to_whip_text(skips: &[&ManualSkip]) -> String {
    match skips {
        [] => "Nothing to whip.".into(),
        skips => format!(
            "Nothing to whip: {}",
            skips
                .iter()
                .map(|s| format!("{} ({})", s.label, s.reason))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// "Whip all stalled now": an immediate whip pass with the normal rules (threshold, cooldown,
/// dry-run). Shared by the King panel and the whip settings window.
pub fn whip_stalled_button(ui: &mut egui::Ui) -> bool {
    ui.button("Whip all stalled now")
        .on_hover_text(
            "Run a whip pass now instead of waiting for the next tick. Threshold, cooldown and \
             dry-run apply as usual.",
        )
        .clicked()
}

/// Toast text for a finished manual whip: `(text, any failure)`.
pub fn manual_result_text(report: &WhipNowReport, skips: &[ManualSkip]) -> (String, bool) {
    let failed: Vec<String> = report
        .items
        .iter()
        .filter(|i| i.entry.outcome != "sent")
        .map(|i| {
            format!(
                "{} {}",
                i.handle.as_deref().unwrap_or(&i.entry.target),
                i.entry.outcome
            )
        })
        .collect();
    let sent = report.items.len() - failed.len();
    let mut text = format!("Whipped {sent} agent{}", if sent == 1 { "" } else { "s" });
    if !failed.is_empty() {
        text.push_str(&format!("; {} failed: {}", failed.len(), failed.join(", ")));
    }
    if !skips.is_empty() {
        let list: Vec<String> = skips
            .iter()
            .map(|s| format!("{} ({})", s.label, s.reason))
            .collect();
        text.push_str(&format!("; skipped {}", list.join(", ")));
    }
    text.push('.');
    (text, !failed.is_empty())
}

/// Toast text for a finished "Whip all stalled now" pass.
pub fn stalled_result_text(report: &WhipNowReport) -> String {
    if !report.ran {
        return "Whip pass not run: no clan data yet.".into();
    }
    let mode = if report.dry_run { " (dry-run)" } else { "" };
    match report.items.len() {
        0 => {
            format!("Whip pass{mode}: nothing to do (no stalls due, or all in cooldown/escalated).")
        }
        n => format!(
            "Whip pass{mode}: {n} action{} — see the whip log.",
            if n == 1 { "" } else { "s" }
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeId;
    use crate::whip::manual::{CONFIRM_AT, NEEDS_INPUT_WARNING};
    use crate::whip::{NudgeVia, WhipLogEntry, WhipLogItem};

    fn nudge(handle: &str, warn: bool) -> ManualPlan {
        ManualPlan::Nudge(ManualNudge {
            node: NodeId::terminal(handle),
            label: handle.into(),
            handle: handle.into(),
            targets: vec![],
            via: NudgeVia::Terminal {
                handle: handle.into(),
            },
            text: "Stronghold: hi".into(),
            warn: warn.then(|| NEEDS_INPUT_WARNING.to_string()),
        })
    }

    fn skip(label: &str) -> ManualSkip {
        ManualSkip {
            node: NodeId::king(),
            label: label.into(),
            reason: "the King is Stronghold itself".into(),
        }
    }

    fn item(handle: &str, outcome: &str) -> WhipLogItem {
        WhipLogItem {
            seq: 1,
            entry: WhipLogEntry {
                at_ms: 0,
                action: "nudge via terminal input".into(),
                target: format!("terminal {handle}"),
                reason: String::new(),
                dry_run: false,
                outcome: outcome.into(),
                manual: true,
            },
            handle: Some(handle.into()),
            run_id: None,
            task_id: None,
        }
    }

    #[test]
    fn a_few_plain_targets_are_sent_at_once() {
        let mut ui = WhipNowUi::default();
        let sent = ui.request(vec![nudge("a", false), nudge("b", false)], 0);
        assert_eq!(sent.map(|n| n.len()), Some(2));
        assert!(!ui.is_confirming());
    }

    #[test]
    fn needs_input_or_many_targets_ask_first() {
        let mut ui = WhipNowUi::default();
        assert_eq!(ui.request(vec![nudge("a", true)], 0), None);
        assert!(ui.is_confirming());

        let mut ui = WhipNowUi::default();
        let many = (0..CONFIRM_AT)
            .map(|i| nudge(&format!("t{i}"), false))
            .collect();
        assert_eq!(ui.request(many, 0), None);
        assert!(ui.is_confirming());
    }

    #[test]
    fn nothing_to_send_shows_why() {
        let mut ui = WhipNowUi::default();
        assert_eq!(ui.request(vec![ManualPlan::Skip(skip("King"))], 5), None);
        assert!(!ui.is_confirming());
        let toast = ui.toast().unwrap();
        assert!(toast.error);
        assert_eq!(
            toast.text,
            "Nothing to whip: King (the King is Stronghold itself)"
        );
    }

    #[test]
    fn result_texts() {
        let report = WhipNowReport {
            ran: true,
            items: vec![item("a", "sent"), item("b", "failed: boom")],
            dry_run: true,
        };
        let (text, failed) = manual_result_text(&report, &[skip("King")]);
        assert!(failed);
        assert_eq!(
            text,
            "Whipped 1 agent; 1 failed: b failed: boom; skipped King (the King is Stronghold \
             itself)."
        );
        assert_eq!(
            stalled_result_text(&report),
            "Whip pass (dry-run): 2 actions — see the whip log."
        );
        let idle = WhipNowReport {
            ran: true,
            ..WhipNowReport::default()
        };
        assert!(stalled_result_text(&idle).starts_with("Whip pass: nothing to do"));
        assert!(stalled_result_text(&WhipNowReport::default()).contains("no clan data"));
    }

    #[test]
    fn confirming_the_popover_sends_the_nudges() {
        let mut state = WhipNowUi::default();
        state.request(vec![nudge("a", true), ManualPlan::Skip(skip("King"))], 0);
        let sent = crate::ui::test_util::click_label("Whip 1", |ui| state.show(ui.ctx(), 0));
        let (send, skips) = sent.expect("confirmed");
        assert_eq!(send[0].handle, "a");
        assert_eq!(skips[0].label, "King");
        assert!(!state.is_confirming());

        let mut state = WhipNowUi::default();
        state.request(vec![nudge("a", true)], 0);
        let sent = crate::ui::test_util::click_label("Cancel", |ui| state.show(ui.ctx(), 0));
        assert_eq!(sent, None);
        assert!(!state.is_confirming());
    }

    #[test]
    fn popover_and_toast_draw_headless() {
        let mut ui = WhipNowUi::default();
        ui.request(vec![nudge("a", true), ManualPlan::Skip(skip("King"))], 0);
        ui.set_toast("Whipped 1 agent.".into(), false, 0);
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui_| {
                assert_eq!(ui.show(ui_.ctx(), 1_000), None);
            });
            output.textures_delta.clear();
            assert!(!output.shapes.is_empty());
        }
        assert!(ui.is_confirming());
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui_| {
            ui.show(ui_.ctx(), TOAST_MS + 1);
        });
        output.textures_delta.clear();
        assert!(ui.toast().is_none(), "the toast expires");
    }
}

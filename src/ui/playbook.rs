//! The King panel's playbook line ("Playbook 1b0b685 · 3 coordinators current · 1 on an older
//! version") and the list of agents on an older version (click selects).

use eframe::egui::{RichText, Ui};

use crate::model::NodeId;
use crate::playbook::fetches::{NodeFetch, PlaybookStatus};
use crate::quota::format_duration;
use crate::ui::clan_view::kind_label;

/// The compact line for the King's header; hover lists every agent's newest fetch. Drawn in
/// the warning colour when someone runs an older version.
pub fn playbook_line(ui: &mut Ui, status: &PlaybookStatus, now_ms: i64) {
    let text = RichText::new(status.line()).small();
    let line = if status.stale().next().is_some() {
        ui.label(text.color(ui.visuals().warn_fg_color))
    } else {
        ui.label(text.weak())
    };
    if !status.nodes.is_empty() {
        line.on_hover_text(hover_text(status, now_ms));
    }
}

/// The agents on an older version, one row each; returns the node whose row was clicked.
pub fn stale_list(ui: &mut Ui, status: &PlaybookStatus, now_ms: i64) -> Option<NodeId> {
    let stale: Vec<&NodeFetch> = status.stale().collect();
    if stale.is_empty() {
        return None;
    }
    let mut select = None;
    ui.add_space(4.0);
    ui.label(RichText::new(format!("Older playbook ({})", stale.len())).strong());
    let warn = ui.visuals().warn_fg_color;
    for n in stale {
        let text = RichText::new(format!(
            "{} · {} · fetched {} ({} ago)",
            n.label,
            kind_label(n.kind),
            n.fetch.record.version_label(),
            ago(n, now_ms)
        ))
        .color(warn);
        if ui
            .selectable_label(false, text)
            .on_hover_text(format!(
                "{} got \"{}\" via {}; the playbook has changed since. Click to select.",
                n.fetch.handle, n.fetch.record.target, n.fetch.record.via
            ))
            .clicked()
        {
            select = Some(n.node.clone());
        }
    }
    select
}

fn hover_text(status: &PlaybookStatus, now_ms: i64) -> String {
    let mut out = String::from("Newest playbook fetch per agent:\n");
    for n in &status.nodes {
        out.push_str(&format!(
            "{} {} · {} · {} · {} ago\n",
            if n.fetch.stale { "⚠" } else { "✓" },
            n.label,
            n.fetch.record.target,
            n.fetch.record.version_label(),
            ago(n, now_ms)
        ));
    }
    out.trim_end().to_owned()
}

fn ago(n: &NodeFetch, now_ms: i64) -> String {
    format_duration((now_ms - n.fetch.record.at_ms).max(0))
}

/// Draws the line in a fresh frame, for tests.
#[cfg(test)]
pub(crate) fn draw_headless(status: &PlaybookStatus) {
    let ctx = eframe::egui::Context::default();
    for _ in 0..2 {
        let mut output = ctx.run_ui(eframe::egui::RawInput::default(), |ui| {
            playbook_line(ui, status, 0);
            assert_eq!(stale_list(ui, status, 0), None);
        });
        output.textures_delta.clear();
    }
}

//! Top-bar chips showing each provider's most constrained plan quota window.

use eframe::egui;

use crate::quota::{Quotas, Severity};

/// One chip per provider, colored amber at 80% and red at 95%; hover lists every window.
pub fn quota_chips(ui: &mut egui::Ui, quotas: &Quotas, now_ms: i64) {
    for chip in quotas.chips(now_ms) {
        let color = match chip.severity {
            Severity::Normal => ui.visuals().text_color(),
            Severity::Warn => ui.visuals().warn_fg_color,
            Severity::Critical => ui.visuals().error_fg_color,
        };
        let mut hover = chip.hover;
        if let Some(error) = &quotas.error {
            hover.push_str(&format!("\nLast refresh failed: {error}"));
        }
        ui.label(egui::RichText::new(chip.text).color(color).small())
            .on_hover_text(hover);
    }
}

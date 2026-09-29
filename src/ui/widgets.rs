//! Small decorations shared by the panes: the primary action button and the input focus ring.

use eframe::egui::{self, Response, RichText, Ui};

/// A primary action button: accent-filled while `enabled`, plain otherwise. Interaction is
/// still gated by `add_enabled`; this only styles the affordance.
pub fn primary_button(ui: &Ui, enabled: bool, text: &str) -> egui::Button<'static> {
    if enabled {
        egui::Button::new(RichText::new(text.to_owned()).color(ui.visuals().selection.stroke.color))
            .fill(ui.visuals().selection.bg_fill)
    } else {
        egui::Button::new(text.to_owned())
    }
}

/// Draws the accent ring around a text box while it has focus.
pub fn focus_ring(ui: &Ui, response: &Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(1.0),
            6.0,
            egui::Stroke::new(1.5, ui.visuals().selection.bg_fill),
            egui::StrokeKind::Outside,
        );
    }
}

/// A small rounded badge tinted with `color`, e.g. a node state.
pub fn state_pill(ui: &mut Ui, color: egui::Color32, text: &str) -> Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(color));
        })
        .response
}

/// An uppercase section header, for sidebars and panels.
pub fn section_label(ui: &mut Ui, text: &str) -> Response {
    ui.add_space(2.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .small()
            .color(ui.visuals().weak_text_color()),
    )
}

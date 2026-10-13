//! Small decorations shared by the panes: the app title, the primary and Open in Orca buttons
//! and the input focus ring.

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

/// The top bar's title: the King's avatar and the app name.
pub fn app_title(ui: &mut Ui) {
    let size = ui.text_style_height(&egui::TextStyle::Heading) + 4.0;
    ui.add(crate::ui::art::king_avatar(ui.ctx(), size));
    ui.heading(crate::APP_NAME);
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

/// Teal of the "Open in Orca" button: apart from the accent, the King's gold and the state
/// colours, so the most-pressed button is easy to find.
pub fn orca_color(dark_mode: bool) -> egui::Color32 {
    if dark_mode {
        egui::Color32::from_rgb(0x3e, 0xb8, 0xa6)
    } else {
        egui::Color32::from_rgb(0x1c, 0x7c, 0x70)
    }
}

/// The "Open in Orca" button: teal-filled while `enabled`, plain otherwise.
pub fn orca_button(ui: &Ui, enabled: bool) -> egui::Button<'static> {
    let text = "Open in Orca";
    if !enabled {
        return egui::Button::new(text);
    }
    let dark = ui.visuals().dark_mode;
    let ink = if dark {
        ui.visuals().extreme_bg_color
    } else {
        egui::Color32::WHITE
    };
    egui::Button::new(RichText::new(text).strong().color(ink)).fill(orca_color(dark))
}

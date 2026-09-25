//! Headless click helpers for UI tests.

use eframe::egui::{self, Event, PointerButton, Pos2, Rect, Ui};

const SCREEN: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1200.0, 900.0));

/// Screen centre of the text `label` in a frame's shapes.
pub fn text_center(output: &egui::FullOutput, label: &str) -> Option<Pos2> {
    fn walk(shape: &egui::Shape, label: &str) -> Option<Pos2> {
        match shape {
            egui::Shape::Text(t) if t.galley.text() == label => {
                Some(t.pos + t.galley.rect.center().to_vec2())
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| walk(s, label)),
            _ => None,
        }
    }
    output.shapes.iter().find_map(|c| walk(&c.shape, label))
}

/// Draws `draw` in a fresh context, clicks the text `label` and returns what `draw` returned
/// in the frame the click landed. Panics when `label` is not drawn.
pub fn click_label<T>(label: &str, mut draw: impl FnMut(&mut Ui) -> T) -> T {
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |events: Vec<Event>, out: &mut Option<T>| {
        time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(SCREEN),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| *out = Some(draw(ui)));
        output.textures_delta.clear();
        output
    };
    let mut out = None;
    // Windows skip drawing in their first (sizing) frame.
    let at = (0..3)
        .find_map(|_| text_center(&frame(vec![], &mut out), label))
        .unwrap_or_else(|| panic!("{label:?} is not drawn"));
    let button = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(vec![Event::PointerMoved(at)], &mut out);
    frame(vec![button(true)], &mut out);
    frame(vec![button(false)], &mut out);
    out.expect("a frame ran")
}

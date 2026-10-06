//! Tool calls as beams from the caller's node to the nodes it acted on, rim to rim; see
//! [`crate::ui::beam`].

use std::time::Duration;

use eframe::egui::{Pos2, Rect, Ui};
use egui_graphs::MetadataFrame;

use super::GraphView;
use crate::traffic::beam::Beam;
use crate::ui::beam;

/// Redraw interval while a beam shows.
const FRAME: Duration = Duration::from_millis(16);

impl GraphView {
    /// Shows a tool call's beam, asked for at `now_s` (egui time); `still` for Reduce motion.
    pub fn beam(&mut self, beam: Beam, now_s: f64, still: bool) {
        self.beams.push(beam, now_s, still);
    }

    /// Draws the beams showing over the widget at `rect`.
    pub(super) fn paint_beams(&mut self, ui: &Ui, rect: Rect) {
        let now_s = ui.input(|i| i.time);
        let mut beams = std::mem::take(&mut self.beams);
        let lit = beams.tick(now_s);
        if lit.is_empty() {
            self.beams = beams;
            return;
        }
        let meta = MetadataFrame::new(Some(self.id.clone())).load(ui);
        let origin = meta.pan + rect.left_top().to_vec2();
        let screen =
            |(p, r): (Pos2, f32)| ((p.to_vec2() * meta.zoom + origin).to_pos2(), r * meta.zoom);
        let size = (16.0 * meta.zoom).clamp(12.0, 24.0);
        let painter = ui.painter().with_clip_rect(rect);
        for l in lit {
            let Some(from) = self.mail_end(&l.beam.from).map(screen) else {
                continue;
            };
            let targets: Vec<_> = l
                .beam
                .targets
                .iter()
                .filter_map(|t| self.mail_end(t).map(screen))
                .collect();
            beam::paint(&painter, l, from, &targets, size, now_s);
        }
        self.beams = beams;
        ui.ctx().request_repaint_after(FRAME);
    }
}

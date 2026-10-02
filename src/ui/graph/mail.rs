//! Messages between nodes: an envelope flies along the straight line from sender to receiver,
//! edge to edge. Karl and the outside world are drawn as the Queen. See
//! [`crate::ui::envelope`].

use std::time::Duration;

use eframe::egui::{Pos2, Rect, Ui};
use egui_graphs::MetadataFrame;

use super::GraphView;
use super::shape::node_radius;
use crate::model::NodeId;
use crate::traffic::{Endpoint, Flight};
use crate::ui::envelope;

/// Redraw interval while a message is in the air.
const FRAME: Duration = Duration::from_millis(33);

impl GraphView {
    /// Flies a message between nodes, asked for at `now_s` (egui time); `still` for Reduce
    /// motion.
    pub fn message(&mut self, flight: Flight, now_s: f64, still: bool) {
        self.mail.push(flight, now_s, still);
    }

    /// A node's canvas position and radius; `None` when it is not drawn.
    fn mail_end(&self, end: &Endpoint) -> Option<(Pos2, f32)> {
        let id = match end {
            Endpoint::Node(id) => id.clone(),
            Endpoint::Karl | Endpoint::Outside => NodeId::queen(),
        };
        let node = self.graph.node(*self.index.get(&id)?)?;
        Some((node.location(), node_radius(node.payload().kind)))
    }

    /// Draws the messages in the air over the widget at `rect`.
    pub(super) fn paint_mail(&mut self, ui: &Ui, rect: Rect) {
        let now_s = ui.input(|i| i.time);
        let mut mail = std::mem::take(&mut self.mail);
        let air = mail.tick(now_s);
        if air.is_empty() {
            self.mail = mail;
            return;
        }
        let meta = MetadataFrame::new(Some(self.id.clone())).load(ui);
        let origin = meta.pan + rect.left_top().to_vec2();
        let screen = |p: Pos2| (p.to_vec2() * meta.zoom + origin).to_pos2();
        let size = (16.0 * meta.zoom).clamp(12.0, 24.0);
        let painter = ui.painter().with_clip_rect(rect);
        for f in air {
            let (Some((a, ra)), Some((b, rb))) =
                (self.mail_end(&f.flight.from), self.mail_end(&f.flight.to))
            else {
                continue;
            };
            let dir = (b - a).normalized();
            let path = [screen(a + dir * ra), screen(b - dir * rb)];
            if path[0] != path[1] {
                envelope::paint(&painter, f, &path, size, now_s);
            }
        }
        self.mail = mail;
        ui.ctx().request_repaint_after(FRAME);
    }
}

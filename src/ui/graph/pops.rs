//! The graph's side of [`crate::ui::state_pop`]: a node that just changed state hops and
//! flashes, and a ring and a burst of sparkles spread from it; with Reduce motion a ring that
//! fades.

use std::collections::HashSet;

use eframe::egui::{Pos2, Shape, Stroke, Ui, Vec2};

use super::GraphView;
use crate::model::NodeId;
use crate::ui::state_pop::{self, Cue, PopFrame};

impl GraphView {
    /// Pops a node that just changed state, asked for at `now_s` (egui time); `still` for
    /// Reduce motion.
    pub fn pop(&mut self, node: NodeId, cue: Cue, now_s: f64, still: bool) {
        self.pops.push(node, cue, now_s, still);
    }

    /// Hands each drawn node its pop for this frame, clearing finished ones.
    pub(super) fn apply_pops(&mut self, ui: &Ui) {
        if self.pops.is_empty() && self.popped.is_empty() {
            return;
        }
        let frames = self.pops.frames(ui.input(|i| i.time));
        let now: HashSet<NodeId> = frames.iter().map(|(id, _)| id.clone()).collect();
        for id in self.popped.difference(&now) {
            if let Some(node) = self.index.get(id).and_then(|&i| self.graph.node_mut(i)) {
                node.display_mut().pop = None;
            }
        }
        for (id, frame) in frames {
            if let Some(node) = self.index.get(&id).and_then(|&i| self.graph.node_mut(i)) {
                node.display_mut().pop = Some(frame);
            }
        }
        if !now.is_empty() {
            ui.ctx().request_repaint_after(state_pop::FRAME);
        }
        self.popped = now;
    }
}

/// How far up a node of screen radius `radius` is lifted by its pop.
pub(super) fn lift(pop: Option<&PopFrame>, radius: f32) -> f32 {
    pop.map_or(0.0, |p| p.lift * radius)
}

/// A node's pop drawn behind it (`behind`: the flash) or over it (the ring, sparkles or
/// Reduce motion's highlight), around `center` with the node's outer screen radius `outer`.
pub(super) fn shapes(pop: &PopFrame, center: Pos2, outer: f32, behind: bool) -> Vec<Shape> {
    let color = pop.cue.color();
    if behind {
        if pop.flash <= 0.0 {
            return Vec::new();
        }
        return vec![Shape::circle_filled(
            center,
            outer * (1.15 + 0.25 * pop.flash),
            color.gamma_multiply(0.45 * pop.flash),
        )];
    }
    if pop.highlight > 0.0 {
        return vec![Shape::circle_stroke(
            center,
            outer + 4.0,
            Stroke::new(3.0, color.gamma_multiply(pop.highlight)),
        )];
    }
    if pop.fade <= 0.0 {
        return Vec::new();
    }
    let mut shapes = Vec::with_capacity(pop.sparkles + 1);
    let ring = outer * (1.1 + 1.1 * pop.spread);
    shapes.push(Shape::circle_stroke(
        center,
        ring,
        Stroke::new(
            (outer * 0.12).max(1.0),
            color.gamma_multiply(0.7 * pop.fade),
        ),
    ));
    let dot = (outer * 0.12).clamp(1.5, 4.0) * (1.0 - 0.5 * pop.t);
    for i in 0..pop.sparkles {
        let angle = i as f32 / pop.sparkles as f32 * std::f32::consts::TAU + 0.4;
        let reach = ring + outer * 0.25 * ((i % 3) as f32 - 1.0);
        let p = center + Vec2::angled(angle) * reach;
        shapes.push(Shape::circle_filled(p, dot, color.gamma_multiply(pop.fade)));
    }
    shapes
}

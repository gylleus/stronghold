//! The farm's side of [`crate::ui::state_pop`]: a flash behind a dwarf that just changed
//! state, then a ring and a burst of sparkles over it; with Reduce motion a ring that fades.
//! The dwarf's hop is drawn with the dwarf, from [`lift`].

use eframe::egui::{Painter, Pos2, Rect, Stroke, StrokeKind, Vec2, vec2};

use super::Placed;
use super::layout::{CELL, Scene};
use super::sprites::Sprites;
use crate::model::NodeId;
use crate::ui::state_pop::PopFrame;

/// The pop running on `id`, if any.
pub fn of<'a>(pops: &'a [(NodeId, PopFrame)], id: &NodeId) -> Option<&'a PopFrame> {
    pops.iter().find(|(n, _)| n == id).map(|(_, f)| f)
}

/// How far up a dwarf of `size` is lifted by its pop, snapped to its texels.
pub fn lift(pop: Option<&PopFrame>, size: f32) -> f32 {
    let Some(pop) = pop else {
        return 0.0;
    };
    let tpx = size / CELL;
    (pop.lift * size / tpx).round() * tpx
}

/// The flashes behind the dwarves (`behind`), or the rings, sparkles and highlights over them.
pub fn paint(
    painter: &Painter,
    sprites: &Sprites,
    scene: &Scene,
    placed: &[Placed],
    pops: &[(NodeId, PopFrame)],
    behind: bool,
) {
    if pops.is_empty() {
        return;
    }
    let px = scene.px;
    for (dwarf, at) in scene.dwarves.iter().zip(placed) {
        let Some(pop) = of(pops, &dwarf.id) else {
            continue;
        };
        let color = super::world::GOLD;
        let center = at.pos;
        if behind {
            if pop.flash > 0.0 {
                sprites.glow(painter, center, 0.9 * dwarf.size, color, pop.flash);
            }
            continue;
        }
        if pop.highlight > 0.0 {
            let ring = Rect::from_center_size(center, Vec2::splat(dwarf.size)).expand(2.0 * px);
            painter.rect_stroke(
                ring,
                0.0,
                Stroke::new(2.0 * px, color.gamma_multiply(pop.highlight)),
                StrokeKind::Outside,
            );
            continue;
        }
        if pop.fade <= 0.0 {
            continue;
        }
        let radius = dwarf.size * (0.5 + 0.8 * pop.spread);
        sprites.ring(
            painter,
            center,
            radius,
            px,
            color.gamma_multiply(0.6 * pop.fade),
        );
        let seed = (dwarf.pos.x * 0.37 + dwarf.pos.y * 0.11).rem_euclid(1.0);
        for i in 0..pop.sparkles {
            let angle = (i as f32 / pop.sparkles as f32 + seed) * std::f32::consts::TAU;
            let reach = dwarf.size * (0.35 + 1.1 * pop.spread) + ((i * 7) % 3) as f32 * px;
            let p = center + vec2(angle.cos(), angle.sin()) * reach;
            let snapped = Pos2::new((p.x / px).round() * px, (p.y / px).round() * px);
            let big = i % 2 == 0 && pop.t < 0.6;
            sprites.sparkle(painter, snapped, px, color.gamma_multiply(pop.fade), big);
        }
    }
}

//! Tool calls as beams straight through the rock, from the caller's ant to the ants it acted
//! on; see [`crate::ui::beam`].

use eframe::egui::Painter;

use super::Placed;
use super::layout::{Scene, Spot};
use crate::traffic::Endpoint;
use crate::ui::beam::{self, End, Lit};

/// Draws the beams showing over the ants.
pub fn paint(painter: &Painter, scene: &Scene, placed: &[Placed], lit: &[Lit], now_s: f64) {
    let cell = scene.cell();
    let size = cell.clamp(14.0, 28.0);
    let end = |e: &Endpoint| -> Option<End> {
        let (at, _) = super::mail::locate(scene, placed, e)?;
        let queen = match e {
            Endpoint::Node(id) => scene.ant(id).is_some_and(|a| a.spot == Spot::Queen),
            _ => false,
        };
        Some((at, cell * if queen { 1.0 } else { 0.6 }))
    };
    for l in lit {
        let Some(from) = end(&l.beam.from) else {
            continue;
        };
        let targets: Vec<_> = l.beam.targets.iter().filter_map(end).collect();
        beam::paint(painter, l, from, &targets, size, now_s, super::world::GOLD);
    }
}

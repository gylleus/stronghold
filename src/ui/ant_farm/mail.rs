//! Messages between ants, carried through the farm's passages: up the sender's shaft, along
//! the gallery and down to the receiver. Karl stands by his sign on the surface and the outside
//! world (Slack) comes in at the landing mound. See [`crate::ui::envelope`].

use eframe::egui::{Painter, Pos2, pos2};

use super::Placed;
use super::layout::Scene;
use crate::traffic::Endpoint;
use crate::ui::envelope::{self, InFlight};

/// Where `end` is this frame and whether it is on the surface; `None` when its ant is not
/// drawn (filtered out or gone).
fn locate(scene: &Scene, placed: &[Placed], end: &Endpoint) -> Option<(Pos2, bool)> {
    let a = scene.cell();
    match end {
        Endpoint::Node(id) => scene
            .ants
            .iter()
            .zip(placed)
            .find(|(ant, _)| &ant.id == id)
            .map(|(_, at)| (at.pos, false)),
        Endpoint::Karl => Some((
            pos2(
                scene.rect.left() + 0.3 * a + 6.0 * scene.px,
                scene.surface_y - 7.0 * scene.px,
            ),
            true,
        )),
        Endpoint::Outside => Some((
            pos2(scene.landing.center().x, scene.surface_y - 0.5 * a),
            true,
        )),
    }
}

/// The way from `from` to `to` through the passages; each is a point and whether it is on the
/// surface.
pub fn path(scene: &Scene, from: (Pos2, bool), to: (Pos2, bool)) -> Vec<Pos2> {
    if from.1 && to.1 {
        return vec![from.0, to.0];
    }
    let gy = scene.gallery.center().y;
    let lx = scene.landing.center().x;
    let leg = |(p, surface): (Pos2, bool)| {
        if surface {
            vec![p, pos2(lx, scene.surface_y), pos2(lx, gy)]
        } else {
            vec![p, pos2(p.x, gy)]
        }
    };
    let mut path = leg(from);
    path.extend(leg(to).into_iter().rev());
    path.dedup();
    path
}

/// Draws the messages in the air over the ants.
pub fn paint(painter: &Painter, scene: &Scene, placed: &[Placed], air: &[InFlight], now_s: f64) {
    let size = scene.cell().clamp(14.0, 28.0);
    for f in air {
        let (Some(from), Some(to)) = (
            locate(scene, placed, &f.flight.from),
            locate(scene, placed, &f.flight.to),
        ) else {
            continue;
        };
        envelope::paint(painter, f, &path(scene, from, to), size, now_s);
    }
}

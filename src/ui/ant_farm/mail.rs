//! Messages between ants, carried through the farm's passages: up the sender's shaft, along
//! the gallery and down to the receiver. Karl stands by his sign on the surface and the outside
//! world comes in at the landing mound, or the dish for the Envoy. See [`crate::ui::envelope`].

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
                scene.karl_sign().x + 6.0 * scene.px,
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

/// Outside mail for the Envoy follows the receiver cable instead of the public entrance.
fn flight_path(
    scene: &Scene,
    placed: &[Placed],
    from: &Endpoint,
    to: &Endpoint,
) -> Option<Vec<Pos2>> {
    let from_at = locate(scene, placed, from)?;
    let to_at = locate(scene, placed, to)?;
    let envoy = |end: &Endpoint| match end {
        Endpoint::Node(id) => scene
            .ant(id)
            .is_some_and(|a| a.kind == crate::model::NodeKind::Envoy),
        _ => false,
    };
    let incoming = *from == Endpoint::Outside && envoy(to);
    let outgoing = *to == Endpoint::Outside && envoy(from);
    if let Some(room) = scene.comms.filter(|_| incoming || outgoing) {
        let at = if incoming { to_at.0 } else { from_at.0 };
        let mut route = vec![room.feed(scene.px), room.dish, room.chamber.center(), at];
        if outgoing {
            route.reverse();
        }
        route.dedup();
        return Some(route);
    }
    Some(path(scene, from_at, to_at))
}

/// Draws the messages in the air over the ants.
pub fn paint(painter: &Painter, scene: &Scene, placed: &[Placed], air: &[InFlight], now_s: f64) {
    let size = scene.cell().clamp(14.0, 28.0);
    for f in air {
        let Some(route) = flight_path(scene, placed, &f.flight.from, &f.flight.to) else {
            continue;
        };
        envelope::paint(painter, f, &route, size, now_s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeKind;
    use crate::ui::ant_farm::layout_tests::{id, scene, snapshot};

    #[test]
    fn envoy_outside_mail_uses_cable_in_both_directions() {
        let mut snapshot = snapshot();
        let envoy = snapshot
            .nodes
            .iter_mut()
            .find(|n| n.id == id("loose"))
            .unwrap();
        envoy.kind = NodeKind::Envoy;
        let scene = scene(&snapshot);
        let placed: Vec<_> = scene
            .ants
            .iter()
            .map(|a| Placed {
                pos: a.pos,
                heading: None,
            })
            .collect();
        let envoy = Endpoint::Node(id("loose"));
        let incoming = flight_path(&scene, &placed, &Endpoint::Outside, &envoy).unwrap();
        let room = scene.comms.unwrap();
        assert_eq!(incoming.first(), Some(&room.feed(scene.px)));
        assert!(incoming.contains(&room.dish));
        assert!(incoming.contains(&room.chamber.center()));
        assert_eq!(incoming.last(), Some(&scene.ant(&id("loose")).unwrap().pos));
        let mut outgoing = flight_path(&scene, &placed, &envoy, &Endpoint::Outside).unwrap();
        outgoing.reverse();
        assert_eq!(incoming, outgoing);

        let worker = Endpoint::Node(id("working"));
        let ordinary = flight_path(&scene, &placed, &Endpoint::Outside, &worker).unwrap();
        assert_eq!(ordinary[0].x, scene.landing.center().x);
        assert!(!ordinary.contains(&room.dish));
        let internal = flight_path(&scene, &placed, &envoy, &worker).unwrap();
        assert!(internal.iter().any(|p| p.y == scene.gallery.center().y));
        assert!(!internal.contains(&room.dish));
        assert!(
            flight_path(
                &scene,
                &placed,
                &Endpoint::Outside,
                &Endpoint::Node(id("hidden"))
            )
            .is_none()
        );
    }
}

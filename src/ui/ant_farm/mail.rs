//! Messages between ants, carried through the farm's passages: up the sender's shaft, along
//! the gallery and down to the receiver. Karl's messages, and the Queen's instructions that
//! carry them, pop out of his green pipe on the surface (mail for Karl drops into it); the
//! outside world comes in at the landing mound, or the dish for the Envoy. See
//! [`crate::ui::envelope`].

use eframe::egui::{Painter, Pos2, pos2, vec2};

use super::Placed;
use super::layout::Scene;
use crate::model::NodeId;
use crate::traffic::{Endpoint, Flight, MessageKind};
use crate::ui::envelope::{self, InFlight};

/// How long the pipe stretches as a message pops out of it.
const POP_S: f64 = 0.35;
/// How many texels taller it gets at the height of the pop.
const POP_TEXELS: f32 = 3.0;

/// Whether `flight` leaves from Karl's pipe: Karl's own, or the Queen instructing an ant (her
/// whip's nudges stay with her).
pub fn from_pipe(flight: &Flight) -> bool {
    match &flight.from {
        Endpoint::Karl => true,
        Endpoint::Node(id) => *id == NodeId::queen() && flight.kind == MessageKind::Instruction,
        Endpoint::Outside => false,
    }
}

/// Texels the pipe stretches at `now_s`, while messages pop out of it; still flights (Reduce
/// motion) leave it be.
pub fn pipe_stretch(air: &[InFlight], now_s: f64) -> f32 {
    air.iter()
        .filter(|f| !f.still && from_pipe(&f.flight))
        .filter_map(|f| {
            let t = (now_s - f.start_s?) / POP_S;
            (0.0..1.0)
                .contains(&t)
                .then(|| (POP_TEXELS * (std::f64::consts::PI * t).sin() as f32).round())
        })
        .fold(0.0, f32::max)
}

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
        Endpoint::Karl => Some((above_pipe(scene), true)),
        Endpoint::Outside => Some((
            pos2(scene.landing.center().x, scene.surface_y - 0.5 * a),
            true,
        )),
    }
}

/// Where a message clears the pipe's mouth, popping out (or about to drop in).
fn above_pipe(scene: &Scene) -> Pos2 {
    scene.karl_pipe_mouth(0.0) - vec2(0.0, 0.8 * scene.cell())
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

/// Outside mail for the Envoy follows the receiver cable instead of the public entrance. Mail
/// from Karl's pipe rises out of its mouth first; mail for Karl drops into it last.
fn flight_path(scene: &Scene, placed: &[Placed], flight: &Flight) -> Option<Vec<Pos2>> {
    let (from, to) = (&flight.from, &flight.to);
    let to_at = locate(scene, placed, to)?;
    if from_pipe(flight) {
        let mut route = vec![scene.karl_pipe_mouth(0.0)];
        route.extend(path(scene, (above_pipe(scene), true), to_at));
        if *to == Endpoint::Karl {
            route.push(scene.karl_pipe_mouth(0.0));
        }
        route.dedup();
        return Some(route);
    }
    let from_at = locate(scene, placed, from)?;
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
        let mut route = vec![
            room.feed(),
            room.dish,
            room.cable_elbow(),
            room.cable_port(),
            at,
        ];
        if outgoing {
            route.reverse();
        }
        route.dedup();
        return Some(route);
    }
    let mut route = path(scene, from_at, to_at);
    if *to == Endpoint::Karl {
        route.push(scene.karl_pipe_mouth(0.0));
    }
    Some(route)
}

/// Draws the messages in the air over the ants.
pub fn paint(painter: &Painter, scene: &Scene, placed: &[Placed], air: &[InFlight], now_s: f64) {
    let size = scene.cell().clamp(14.0, 28.0);
    for f in air {
        let Some(route) = flight_path(scene, placed, &f.flight) else {
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

    fn message(from: &Endpoint, to: &Endpoint) -> Flight {
        Flight {
            from: from.clone(),
            to: to.clone(),
            kind: MessageKind::Message,
        }
    }

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
        let incoming = flight_path(&scene, &placed, &message(&Endpoint::Outside, &envoy)).unwrap();
        let room = scene.comms.unwrap();
        assert_eq!(incoming.first(), Some(&room.feed()));
        assert!(incoming.contains(&room.dish));
        assert!(incoming.contains(&room.cable_port()));
        assert_eq!(incoming.last(), Some(&scene.ant(&id("loose")).unwrap().pos));
        let mut outgoing =
            flight_path(&scene, &placed, &message(&envoy, &Endpoint::Outside)).unwrap();
        outgoing.reverse();
        assert_eq!(incoming, outgoing);

        let worker = Endpoint::Node(id("working"));
        let ordinary = flight_path(&scene, &placed, &message(&Endpoint::Outside, &worker)).unwrap();
        assert_eq!(ordinary[0].x, scene.landing.center().x);
        assert!(!ordinary.contains(&room.dish));
        let internal = flight_path(&scene, &placed, &message(&envoy, &worker)).unwrap();
        assert!(internal.iter().any(|p| p.y == scene.gallery.center().y));
        assert!(!internal.contains(&room.dish));
        assert!(internal.contains(&pos2(room.shaft.center().x, scene.gallery.center().y)));
        assert!(
            flight_path(
                &scene,
                &placed,
                &message(&Endpoint::Outside, &Endpoint::Node(id("hidden")))
            )
            .is_none()
        );
    }

    #[test]
    fn karl_and_queen_instructions_pop_out_of_the_pipe() {
        let scene = scene(&snapshot());
        let placed: Vec<_> = scene
            .ants
            .iter()
            .map(|a| Placed {
                pos: a.pos,
                heading: None,
            })
            .collect();
        let worker = Endpoint::Node(id("working"));
        let at_worker = scene.ant(&id("working")).unwrap().pos;
        let mouth = scene.karl_pipe_mouth(0.0);
        let queen = Endpoint::Node(NodeId::queen());
        let flight = |from: &Endpoint, to: &Endpoint, kind| Flight {
            from: from.clone(),
            to: to.clone(),
            kind,
        };
        for from in [&Endpoint::Karl, &queen] {
            let f = flight(from, &worker, MessageKind::Instruction);
            assert!(from_pipe(&f));
            let route = flight_path(&scene, &placed, &f).unwrap();
            assert_eq!(route[0], mouth);
            assert!(route[1].y < mouth.y, "rises out of the mouth first");
            assert_eq!(route.last(), Some(&at_worker));
        }
        // The whip's nudges come from the Queen herself.
        let nudge = flight(&queen, &worker, MessageKind::Nudge);
        assert!(!from_pipe(&nudge));
        assert_ne!(flight_path(&scene, &placed, &nudge).unwrap()[0], mouth);
        // Karl's chat message reaches the Queen; an ask for Karl drops into the pipe.
        let chat = flight(&Endpoint::Karl, &queen, MessageKind::Instruction);
        let to_queen = flight_path(&scene, &placed, &chat).unwrap();
        assert_eq!(to_queen[0], mouth);
        let ask = flight(&worker, &Endpoint::Karl, MessageKind::Ask);
        assert_eq!(
            flight_path(&scene, &placed, &ask).unwrap().last(),
            Some(&mouth)
        );
    }

    #[test]
    fn the_pipe_stretches_only_while_popping_and_not_when_still() {
        let air = |still| {
            vec![InFlight {
                flight: Flight {
                    from: Endpoint::Karl,
                    to: Endpoint::Node(id("working")),
                    kind: MessageKind::Instruction,
                },
                queued_s: 0.0,
                start_s: Some(0.0),
                count: 1,
                still,
            }]
        };
        assert_eq!(pipe_stretch(&air(false), POP_S / 2.0), POP_TEXELS);
        assert_eq!(pipe_stretch(&air(false), POP_S + 0.1), 0.0);
        assert_eq!(pipe_stretch(&air(true), POP_S / 2.0), 0.0);
    }
}

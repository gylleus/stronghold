//! Sealed scrolls carried along the mine's passages. The user's messages appear in his rune
//! portal; outside messages reach the priest through the receiver stone's rune conduit.
//! Knowledge read from or filed in the Ledger travels its chamber's shaft in blue.

use eframe::egui::{Align2, FontId, Painter, Pos2, Rect, Stroke, pos2, vec2};

use super::Placed;
use super::layout::Scene;
use super::{
    motion,
    sprites::Sprites,
    world::{BLUE, GOLD, INK, LIGHT, Prop},
};
use crate::model::NodeId;
use crate::traffic::{Endpoint, Flight, MessageKind};
use crate::ui::envelope::{InFlight, Phase};

/// How long the portal flares as a scroll materialises.
const PORTAL_S: f64 = 0.7;
/// The share of its trip a scroll taken from the Ledger lights the lectern for.
const LEDGER_FLARE: f32 = 0.2;

/// Whether `flight` leaves from the user's portal: the user's own, or the King instructing a dwarf (his
/// whip's nudges stay with him).
pub fn from_portal(flight: &Flight) -> bool {
    match &flight.from {
        Endpoint::User => true,
        Endpoint::Node(id) => *id == NodeId::king() && flight.kind == MessageKind::Instruction,
        Endpoint::Outside | Endpoint::Ledger => false,
    }
}

/// Event-driven light, in three intensity steps. Reduced motion leaves the portal still.
pub fn portal_flare(air: &[InFlight], now_s: f64) -> u8 {
    air.iter()
        .filter(|f| !f.still)
        .filter_map(|f| {
            let elapsed = now_s - f.start_s?;
            let departure = if from_portal(&f.flight) {
                elapsed / PORTAL_S
            } else {
                -1.0
            };
            let arrival = if f.flight.to == Endpoint::User {
                (elapsed - crate::ui::envelope::TRAVEL_S + PORTAL_S / 2.0) / PORTAL_S
            } else {
                -1.0
            };
            [departure, arrival]
                .into_iter()
                .filter(|t| (0.0..1.0).contains(t))
                .map(|t| (3.0 * (std::f64::consts::PI * t).sin()).round() as u8)
                .max()
        })
        .max()
        .unwrap_or(0)
}

/// Where `end` is this frame and whether it is on the surface; `None` when its dwarf is not
/// drawn (filtered out or gone).
pub(super) fn locate(scene: &Scene, placed: &[Placed], end: &Endpoint) -> Option<(Pos2, bool)> {
    let a = scene.cell();
    match end {
        Endpoint::Node(id) => scene
            .dwarves
            .iter()
            .zip(placed)
            .find(|(dwarf, _)| &dwarf.id == id)
            .map(|(_, at)| (at.pos, false)),
        Endpoint::User => Some((above_portal(scene), true)),
        Endpoint::Outside => Some((pos2(scene.entrance_x(), scene.surface_y - 0.5 * a), true)),
        Endpoint::Ledger => super::ledger::lectern(scene).map(|at| (at, false)),
    }
}

/// Where a scroll clears the arch before joining the surface route.
fn above_portal(scene: &Scene) -> Pos2 {
    scene.user_portal() - vec2(0.0, super::layout::PORTAL_H * scene.px + 0.3 * scene.cell())
}

/// The way from `from` to `to` through the passages; each is a point and whether it is on the
/// surface.
pub fn path(scene: &Scene, from: (Pos2, bool), to: (Pos2, bool)) -> Vec<Pos2> {
    if from.1 && to.1 {
        return vec![from.0, to.0];
    }
    let gy = scene.gallery.center().y;
    let lx = scene.entrance_x();
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

/// Outside mail for the Envoy follows the rune conduit instead of the public entrance. Mail
/// from the user's portal emerges from its centre first; mail for the user disappears into it last.
fn flight_path(scene: &Scene, placed: &[Placed], flight: &Flight) -> Option<Vec<Pos2>> {
    let (from, to) = (&flight.from, &flight.to);
    let to_at = locate(scene, placed, to)?;
    if from_portal(flight) {
        let mut route = vec![scene.user_portal_focus()];
        route.extend(path(scene, (above_portal(scene), true), to_at));
        if *to == Endpoint::User {
            route.push(scene.user_portal_focus());
        }
        route.dedup();
        return Some(route);
    }
    let from_at = locate(scene, placed, from)?;
    let envoy = |end: &Endpoint| match end {
        Endpoint::Node(id) => scene
            .dwarf(id)
            .is_some_and(|a| a.kind == crate::model::NodeKind::Envoy),
        _ => false,
    };
    let incoming = *from == Endpoint::Outside && envoy(to);
    let outgoing = *to == Endpoint::Outside && envoy(from);
    if let Some(room) = scene.comms.filter(|_| incoming || outgoing) {
        let at = if incoming { to_at.0 } else { from_at.0 };
        let mut route = vec![
            room.feed(),
            room.receiver,
            room.rune_turn(),
            room.rune_port(),
            at,
        ];
        if outgoing {
            route.reverse();
        }
        route.dedup();
        return Some(route);
    }
    let mut route = path(scene, from_at, to_at);
    if *to == Endpoint::User {
        route.push(scene.user_portal_focus());
    }
    Some(route)
}

/// Existing travel, arrival and reduced-motion timing, drawn as parchment and warm light.
pub fn paint(
    painter: &Painter,
    sprites: &Sprites,
    scene: &Scene,
    placed: &[Placed],
    air: &[InFlight],
    now_s: f64,
) {
    let px = scene.px;
    let size = 14.0 * px;
    for f in air {
        let Some(route) = flight_path(scene, placed, &f.flight) else {
            continue;
        };
        let (Some(phase), Some(&end)) = (f.phase(now_s), route.last()) else {
            continue;
        };
        let (tint, light) = if f.flight.kind == MessageKind::Ledger {
            (BLUE, BLUE)
        } else {
            (GOLD, LIGHT)
        };
        let (at, alpha, trail_end) = match phase {
            Phase::Travel(t) => (motion::along(&route, t).0, 1.0, t),
            Phase::Glow(a) => (end, a, 1.0),
            Phase::Arrive(t) => {
                sprites.ring(
                    painter,
                    end,
                    size * (0.4 + 0.9 * t),
                    px,
                    tint.gamma_multiply(1.0 - t),
                );
                sprites.glow(painter, end, size * 0.6, light, 1.0 - t);
                continue;
            }
        };
        // The lectern lights up as a scroll is taken from the Ledger.
        if let (Phase::Travel(t), Endpoint::Ledger) = (phase, &f.flight.from)
            && t < LEDGER_FLARE
        {
            sprites.glow(painter, route[0], size * 0.8, BLUE, 1.0 - t / LEDGER_FLARE);
        }
        for leg in route.windows(2) {
            painter.line_segment(
                [leg[0], leg[1]],
                Stroke::new(px, tint.gamma_multiply(0.18 * alpha)),
            );
        }
        for i in 0..12 {
            let t = (trail_end - 0.25 + i as f32 / 12.0 * 0.25).max(0.0);
            let p = motion::along(&route, t).0;
            let p = pos2((p.x / px).round() * px, (p.y / px).round() * px);
            sprites.sparkle(
                painter,
                p,
                px,
                tint.gamma_multiply(alpha * i as f32 / 24.0),
                false,
            );
        }
        sprites.prop(
            painter,
            Prop::Scroll,
            Rect::from_center_size(at, vec2(size, size * 2.0 / 3.0)),
            alpha,
        );
        if f.count > 1 {
            let text = f.count.to_string();
            let at = at + vec2(size * 0.5, -size * 0.3);
            let rect = Rect::from_center_size(at, vec2(8.0, 7.0) * px);
            painter.rect_filled(rect, 0.0, tint.gamma_multiply(alpha));
            painter.text(
                at,
                Align2::CENTER_CENTER,
                text,
                FontId::monospace(6.0 * px),
                INK.gamma_multiply(alpha),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeKind;
    use crate::ui::farm::layout_tests::{id, scene, snapshot};

    fn message(from: &Endpoint, to: &Endpoint) -> Flight {
        Flight {
            from: from.clone(),
            to: to.clone(),
            kind: MessageKind::Message,
        }
    }

    #[test]
    fn ledger_scrolls_travel_between_the_lectern_and_the_dwarf_by_the_chamber_shaft() {
        use crate::model::{NodeKind, NodeState};
        use crate::ui::farm::layout_tests::node;

        for envoy in [false, true] {
            let mut snapshot = snapshot();
            if envoy {
                snapshot
                    .nodes
                    .push(node("envoy", NodeKind::Envoy, None, NodeState::Idle));
            }
            let scene = scene(&snapshot);
            let placed: Vec<_> = scene
                .dwarves
                .iter()
                .map(|a| Placed {
                    pos: a.pos,
                    heading: None,
                })
                .collect();
            let lectern = super::super::ledger::lectern(&scene).unwrap();
            let worker = Endpoint::Node(id("working"));
            let read = flight_path(&scene, &placed, &message(&Endpoint::Ledger, &worker)).unwrap();
            assert_eq!(read[0], lectern);
            assert_eq!(read[1], pos2(lectern.x, scene.gallery.center().y));
            assert_eq!(read.last(), Some(&scene.dwarf(&id("working")).unwrap().pos));
            let mut filed =
                flight_path(&scene, &placed, &message(&worker, &Endpoint::Ledger)).unwrap();
            filed.reverse();
            assert_eq!(read, filed);
            assert!(!from_portal(&message(&Endpoint::Ledger, &worker)));
        }
    }

    #[test]
    fn envoy_outside_mail_uses_rune_conduit_in_both_directions() {
        let mut snapshot = snapshot();
        let envoy = snapshot
            .nodes
            .iter_mut()
            .find(|n| n.id == id("loose"))
            .unwrap();
        envoy.kind = NodeKind::Envoy;
        let scene = scene(&snapshot);
        let placed: Vec<_> = scene
            .dwarves
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
        assert!(incoming.contains(&room.receiver));
        assert!(incoming.contains(&room.rune_port()));
        assert_eq!(
            incoming.last(),
            Some(&scene.dwarf(&id("loose")).unwrap().pos)
        );
        let mut outgoing =
            flight_path(&scene, &placed, &message(&envoy, &Endpoint::Outside)).unwrap();
        outgoing.reverse();
        assert_eq!(incoming, outgoing);

        let worker = Endpoint::Node(id("working"));
        let ordinary = flight_path(&scene, &placed, &message(&Endpoint::Outside, &worker)).unwrap();
        assert_eq!(ordinary[0].x, scene.entrance_x());
        assert!(!ordinary.contains(&room.receiver));
        let internal = flight_path(&scene, &placed, &message(&envoy, &worker)).unwrap();
        assert!(internal.iter().any(|p| p.y == scene.gallery.center().y));
        assert!(!internal.contains(&room.receiver));
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
    fn karl_and_king_instructions_emerge_from_the_portal() {
        let scene = scene(&snapshot());
        let placed: Vec<_> = scene
            .dwarves
            .iter()
            .map(|a| Placed {
                pos: a.pos,
                heading: None,
            })
            .collect();
        let worker = Endpoint::Node(id("working"));
        let at_worker = scene.dwarf(&id("working")).unwrap().pos;
        let focus = scene.user_portal_focus();
        let king = Endpoint::Node(NodeId::king());
        let flight = |from: &Endpoint, to: &Endpoint, kind| Flight {
            from: from.clone(),
            to: to.clone(),
            kind,
        };
        for from in [&Endpoint::User, &king] {
            let f = flight(from, &worker, MessageKind::Instruction);
            assert!(from_portal(&f));
            let route = flight_path(&scene, &placed, &f).unwrap();
            assert_eq!(route[0], focus);
            assert!(route[1].y < focus.y, "clears the arch first");
            assert_eq!(route.last(), Some(&at_worker));
        }
        // The whip's nudges come from the King himself.
        let nudge = flight(&king, &worker, MessageKind::Nudge);
        assert!(!from_portal(&nudge));
        assert_ne!(flight_path(&scene, &placed, &nudge).unwrap()[0], focus);
        // Karl's chat message reaches the King; an ask for Karl disappears into the portal.
        let chat = flight(&Endpoint::User, &king, MessageKind::Instruction);
        let to_king = flight_path(&scene, &placed, &chat).unwrap();
        assert_eq!(to_king[0], focus);
        let ask = flight(&worker, &Endpoint::User, MessageKind::Ask);
        assert_eq!(
            flight_path(&scene, &placed, &ask).unwrap().last(),
            Some(&focus)
        );
    }

    #[test]
    fn the_portal_flares_only_during_messages_and_not_when_still() {
        let air = |still| {
            vec![InFlight {
                flight: Flight {
                    from: Endpoint::User,
                    to: Endpoint::Node(id("working")),
                    kind: MessageKind::Instruction,
                },
                queued_s: 0.0,
                start_s: Some(0.0),
                count: 1,
                still,
            }]
        };
        assert_eq!(portal_flare(&air(false), PORTAL_S / 2.0), 3);
        assert_eq!(portal_flare(&air(false), PORTAL_S + 0.1), 0);
        assert_eq!(portal_flare(&air(true), PORTAL_S / 2.0), 0);
        let mut incoming = air(false);
        incoming[0].flight.from = Endpoint::Node(id("working"));
        incoming[0].flight.to = Endpoint::User;
        let arrival = crate::ui::envelope::TRAVEL_S;
        assert_eq!(portal_flare(&incoming, PORTAL_S / 2.0), 0);
        assert_eq!(portal_flare(&incoming, arrival), 3);
        assert_eq!(portal_flare(&incoming, arrival + PORTAL_S), 0);
        incoming[0].still = true;
        assert_eq!(portal_flare(&incoming, arrival), 0);
        incoming[0].start_s = None;
        assert_eq!(portal_flare(&incoming, 0.0), 0);
        assert_eq!(portal_flare(&[], 123.0), 0);
    }
}

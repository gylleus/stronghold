//! A King close keeps the last drawing of its target until a spinning tool lands.
//! Only the view waits: process shutdown and the clan snapshot are never delayed.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::hash::BuildHasher;

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2, vec2};

use super::{Figures, art, death::Body, layout::Scene, motion};
use crate::model::{NodeId, NodeKind};
use crate::ui::dwarf::{self, Action, Look, Role, Tool};

const FLIGHT_S: f64 = 0.55;
const HIT_S: f64 = 0.22;
// A close can take three minutes. Failed closes must not leave a permanent mark.
const WAIT_S: f64 = 200.0;

#[derive(Debug)]
struct Throw {
    body: Body,
    tool: Tool,
    queued_s: f64,
    start_s: Option<f64>,
    from: Pos2,
    still: bool,
    hit_s: Option<f64>,
}

impl Throw {
    fn flight(&self, now_s: f64) -> f32 {
        ((now_s - self.start_s.unwrap_or(now_s)) / FLIGHT_S).clamp(0.0, 1.0) as f32
    }

    fn shows_body(&self, now_s: f64) -> bool {
        // Keep the dwarf on the first impact frame, even after a dropped frame.
        self.hit_s.is_none_or(|hit| now_s <= hit)
    }
}

#[derive(Debug, Default)]
pub(super) struct Throws {
    targets: HashMap<NodeId, Throw>,
}

impl Throws {
    pub fn prepare(&mut self, id: NodeId, body: Body, now_s: f64) {
        self.targets.entry(id.clone()).or_insert_with(|| Throw {
            body,
            tool: dwarf::thrown_tool(std::collections::hash_map::RandomState::new().hash_one(&id)),
            queued_s: now_s,
            start_s: None,
            from: Pos2::ZERO,
            still: false,
            hit_s: None,
        });
    }

    pub fn contains(&self, id: &NodeId) -> bool {
        self.targets.contains_key(id)
    }

    pub fn animating(&self) -> bool {
        self.targets.values().any(|t| t.start_s.is_some())
    }

    /// Launch only after removal; refresh bodies while waiting so moving dwarves don't jump.
    /// Returns one impact cue per frame, coalescing a batch of closes.
    pub fn tick(
        &mut self,
        now_s: f64,
        from: Option<Pos2>,
        still: bool,
        mut live: impl FnMut(&NodeId) -> bool,
        mut body: impl FnMut(&NodeId) -> Option<Body>,
    ) -> bool {
        let mut bonk = false;
        self.targets.retain(|id, t| {
            if let Some(hit) = t.hit_s {
                return now_s - hit < HIT_S;
            }
            if t.start_s.is_none() {
                if now_s - t.queued_s >= WAIT_S {
                    return false;
                }
                if let Some(body) = body(id) {
                    t.body = body;
                }
                if live(id) {
                    return true;
                }
                let Some(from) = from else { return false };
                t.from = from;
                t.still = still;
                t.start_s = Some(now_s);
            }
            if t.flight(now_s) >= 1.0 {
                t.hit_s = Some(now_s);
                bonk = true;
            }
            true
        });
        bonk
    }

    pub fn clear(&mut self) {
        self.targets.clear();
    }

    pub fn paint(&self, painter: &Painter, figures: &mut Figures, scene: &Scene, now_s: f64) {
        for (id, t) in &self.targets {
            if t.start_s.is_none() {
                continue;
            }
            let at = motion::from_unit(t.body.at, scene.rect);
            let size = t.body.size;
            if t.shows_body(now_s) {
                let role = match t.body.kind {
                    NodeKind::Coordinator | NodeKind::Attendant => Role::Foreman,
                    NodeKind::Envoy => Role::RunePriest,
                    NodeKind::Lorekeeper => Role::Librarian,
                    _ => Role::Miner,
                };
                let mut look = Look::for_role(role, t.body.seed);
                art::dress(&mut look, t.body.coat);
                let figure = art::Figure {
                    key: id.as_str(),
                    look,
                    action: Action::Still,
                    upside_down: false,
                    facing_right: t.body.facing_right,
                    alpha: t.body.alpha,
                };
                figures.body(
                    painter,
                    &figure,
                    Rect::from_center_size(at, Vec2::splat(size)),
                    scene.px,
                    now_s,
                );
            }
            // Body positions are cell centres; aim at the dwarf's upper body.
            let target = at - vec2(0.0, size * 0.15);
            let from = motion::from_unit(t.from, scene.rect);
            if t.hit_s.is_none() {
                let progress = t.flight(now_s);
                let (pos, angle) = if t.still {
                    (target, -0.5)
                } else {
                    (from + (target - from) * progress, progress * TAU * 2.5)
                };
                painter.extend(dwarf::draw_tool(pos, size * 0.8, angle, t.tool));
            }
            if let Some(hit) = t.hit_s {
                let age = ((now_s - hit) / HIT_S) as f32;
                let fade = (1.0 - age).clamp(0.0, 1.0);
                let gold = Color32::from_rgb(255, 220, 100).gamma_multiply(fade);
                painter.circle_filled(
                    target,
                    size * 0.3 * fade,
                    Color32::WHITE.gamma_multiply(0.65 * fade),
                );
                for i in 0..5 {
                    let dir = Vec2::angled(i as f32 * TAU / 5.0 - 0.3);
                    let spread = if t.still { 0.35 } else { 0.25 + age * 0.5 };
                    let c = target + dir * size * spread;
                    let r = size * 0.12 * fade;
                    painter.add(Shape::convex_polygon(
                        vec![
                            c - vec2(0.0, r),
                            c + vec2(r * 0.45, 0.0),
                            c + vec2(0.0, r),
                            c - vec2(r * 0.45, 0.0),
                        ],
                        gold,
                        Stroke::NONE,
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::sprites::Coat;
    use super::*;

    fn body() -> Body {
        Body {
            kind: NodeKind::Terminal,
            at: Pos2::ZERO,
            size: 24.0,
            facing_right: true,
            coat: Coat::Plain,
            alpha: 1.0,
            seed: 42,
        }
    }

    #[test]
    fn removal_launches_and_keeps_dwarf_through_first_impact_frame() {
        for still in [false, true] {
            let id = NodeId::terminal("a");
            let mut throws = Throws::default();
            throws.prepare(id.clone(), body(), 0.0);
            assert!(!throws.tick(1.0, Some(Pos2::ZERO), still, |_| true, |_| None));
            assert!(!throws.animating());
            assert!(!throws.tick(2.0, Some(Pos2::ZERO), still, |_| false, |_| None));
            assert!(throws.animating());
            let before = 2.0 + FLIGHT_S - 0.001;
            assert!(!throws.tick(before, None, still, |_| false, |_| None));
            assert!(throws.targets[&id].shows_body(before));
            // A late frame still paints the dwarf and hit together and sounds exactly once.
            let impact = 3.0;
            assert!(throws.tick(impact, None, still, |_| false, |_| None));
            assert!(throws.targets[&id].shows_body(impact));
            assert!(!throws.tick(impact + 0.01, None, still, |_| false, |_| None));
            assert!(!throws.targets[&id].shows_body(impact + 0.01));
            throws.tick(impact + HIT_S + 0.01, None, still, |_| false, |_| None);
            assert!(!throws.contains(&id));
        }
    }

    #[test]
    fn failed_close_expires_without_throwing_or_sounding() {
        let id = NodeId::terminal("a");
        let mut throws = Throws::default();
        throws.prepare(id.clone(), body(), 0.0);
        assert!(!throws.tick(WAIT_S, Some(Pos2::ZERO), false, |_| true, |_| None));
        assert!(!throws.contains(&id));
    }

    #[test]
    fn every_tool_is_available_but_empty_hands_are_not() {
        let choices: std::collections::HashSet<_> = (0..1000).map(dwarf::thrown_tool).collect();
        assert_eq!(
            choices,
            [
                Tool::Pickaxe,
                Tool::Hammer,
                Tool::Shovel,
                Tool::Lantern,
                Tool::Staff
            ]
            .into()
        );
        for tool in choices {
            assert!(!dwarf::draw_tool(Pos2::ZERO, 24.0, 1.2, tool).is_empty());
        }
    }
}

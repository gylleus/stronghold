//! A rune priest's scrying chamber and the carved receiver stone on the mountainside.
use super::{
    layout::Scene,
    sprites::Sprites,
    world::{BLUE, GOLD, INK, LIGHT, Prop},
};
use crate::{
    envoy::EnvoyHealth,
    model::{NodeKind, NodeState},
};
use eframe::egui::{Align2, Color32, FontId, Painter, Pos2, Rect, vec2};
use std::f32::consts::TAU;

/// The stone's idle rune light.
pub(super) const RUNE_GLOW: Color32 = Color32::from_rgb(0x8a, 0xd8, 0xdc);
/// One calm lap of the orbiting runes, and one slow breath of the stone's halo, in seconds.
const ORBIT_S: f64 = 14.0;
const BREATH_S: f64 = 5.0;
/// Three runes, 3 by 5 texels each, orbiting the stone's head.
const GLYPHS: [[&str; 5]; 3] = [
    ["#.#", "##.", "#.#", "##.", "#.."],
    ["#.#", "#.#", ".#.", ".#.", ".#."],
    [".#.", "#.#", ".#.", ".#.", ".#."],
];

pub fn working(scene: &Scene) -> bool {
    scene
        .dwarves
        .iter()
        .any(|a| a.kind == NodeKind::Envoy && a.state == NodeState::Working)
}

pub fn paint(
    painter: &Painter,
    sprites: &Sprites,
    scene: &Scene,
    health: EnvoyHealth,
    now_s: f64,
    receipt: crate::ui::envoy_receipt::ReceiptCue,
    still: bool,
) {
    let Some(mut room) = scene.comms else { return };
    let px = room.px;
    sprites.passage(painter, room.shaft, scene.px);
    sprites.ladder(painter, room.shaft, scene.px);
    sprites.masonry(painter, room.chamber);
    let offset = vec2(receipt.offset, 0.0);
    room.chamber = room.chamber.translate(offset);
    room.receiver += offset;
    let prop = if receipt.highlight > 0.0 {
        Prop::ReceivingStone
    } else if matches!(
        health,
        EnvoyHealth::Stopped
            | EnvoyHealth::BridgeDown
            | EnvoyHealth::PollStale
            | EnvoyHealth::EventsStuck
    ) {
        Prop::CrackedStone
    } else {
        Prop::RuneStone
    };
    let live = prop != Prop::CrackedStone;
    let panel = room.receiver - vec2(0.0, 22.0 * px);
    if live {
        let breath = if still {
            0.0
        } else {
            (now_s * TAU as f64 / BREATH_S).sin() as f32
        };
        sprites.glow(painter, panel, 16.0 * px, RUNE_GLOW, 0.24 + 0.08 * breath);
    }
    if receipt.highlight > 0.0 {
        sprites.glow(
            painter,
            room.feed(),
            20.0 * px,
            GOLD,
            receipt.highlight * 0.7,
        );
    }
    for u in receipt.waves() {
        wave(painter, panel, room.receiver.y, u, px);
    }
    let orbit = Orbit {
        center: room.receiver - vec2(0.0, 29.0 * px),
        turn: if still {
            0.0
        } else {
            (now_s * TAU as f64 / ORBIT_S) as f32
        } + receipt.spin(),
        flare: receipt.highlight,
        px,
    };
    if live {
        orbit.paint(painter, sprites, false);
    }
    sprites.standing(painter, prop, room.receiver, px, 1.0);
    if live {
        orbit.paint(painter, sprites, true);
    }
    let active = working(scene);
    let foot = room.pool() + vec2(0.0, 23.0 * px);
    if active {
        sprites.glow(painter, foot - vec2(0.0, 13.0 * px), 13.0 * px, BLUE, 0.45);
    }
    sprites.prop(
        painter,
        if active {
            Prop::ScryingBasin
        } else {
            Prop::Basin
        },
        room.basin_rect(),
        1.0,
    );
    painter.text(
        room.chamber.min + vec2(5.0, 3.0) * px,
        Align2::LEFT_TOP,
        "ENVOY",
        FontId::monospace(5.0 * px + 2.0),
        GOLD,
    );
}

/// Runes circling the stone's head on a tilted ring, like satellites. A receipt swings them
/// wider and lights them gold.
struct Orbit {
    center: Pos2,
    turn: f32,
    flare: f32,
    px: f32,
}

impl Orbit {
    /// The runes on the `near` half of the ring (in front of the stone).
    fn paint(&self, painter: &Painter, sprites: &Sprites, near: bool) {
        let px = self.px;
        let (rx, ry) = (24.0 + 6.0 * self.flare, 6.0 + 2.0 * self.flare);
        for (i, glyph) in GLYPHS.iter().enumerate() {
            let a = self.turn + i as f32 * TAU / 3.0;
            if (a.sin() > 0.0) != near {
                continue;
            }
            let at = self.center + vec2(rx * a.cos(), ry * a.sin() - 2.5 * a.cos()) * px;
            let at = Pos2::new((at.x / px).round() * px, (at.y / px).round() * px);
            let color = lerp_color(RUNE_GLOW, LIGHT, self.flare);
            let depth = if near { 1.0 } else { 0.5 };
            sprites.glow(
                painter,
                at,
                5.0 * px,
                color,
                (0.5 + 0.5 * self.flare) * depth,
            );
            let texels = || {
                glyph.iter().enumerate().flat_map(|(y, row)| {
                    row.chars()
                        .enumerate()
                        .filter(|(_, c)| *c == '#')
                        .map(move |(x, _)| at + vec2(x as f32 - 1.5, y as f32 - 2.5) * px)
                })
            };
            // An ink rim keeps the glyph legible against the sky.
            for min in texels() {
                painter.rect_filled(
                    Rect::from_min_size(min - vec2(px, px), vec2(3.0 * px, 3.0 * px)),
                    0.0,
                    INK.gamma_multiply(0.55 * depth),
                );
            }
            for min in texels() {
                painter.rect_filled(
                    Rect::from_min_size(min, vec2(px, px)),
                    0.0,
                    color.gamma_multiply(depth),
                );
            }
        }
    }
}

fn lerp_color(a: Color32, b: Color32, k: f32) -> Color32 {
    let k = k.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// A gold wave spreading `u` (0..1) of its way out from the stone's runes, above ground only.
fn wave(painter: &Painter, center: Pos2, ground_y: f32, u: f32, px: f32) {
    let radius = (12.0 + 64.0 * u) * px;
    let color = GOLD.gamma_multiply((1.0 - u).powf(1.2));
    let dots = ((radius * TAU) / (2.0 * px)).clamp(24.0, 600.0) as usize;
    for i in 0..dots {
        // Dashes of three dots with a gap, like the King's call.
        if i % 4 == 3 {
            continue;
        }
        let angle = i as f32 / dots as f32 * TAU;
        let p = center + vec2(angle.cos(), angle.sin()) * radius;
        if p.y > ground_y - px {
            continue;
        }
        let snapped = Pos2::new((p.x / px).round() * px, (p.y / px).round() * px);
        painter.rect_filled(
            Rect::from_center_size(snapped, vec2(2.0 * px, 2.0 * px)),
            0.0,
            color,
        );
    }
}

pub fn tray(
    painter: &Painter,
    sprites: &Sprites,
    scene: &Scene,
    pending: usize,
    hovered: bool,
    offset: f32,
) {
    let Some(room) = scene.comms else { return };
    let rect = room.tray_rect().translate(vec2(offset, 0.0));
    let px = room.px;
    if hovered {
        sprites.glow(painter, rect.center(), rect.width(), GOLD, 0.5);
    }
    sprites.prop(painter, Prop::Tablets, rect, 1.0);
    if pending > 0 {
        let galley = painter.layout_no_wrap(pending.to_string(), FontId::monospace(6.0 * px), INK);
        let badge = Rect::from_min_size(
            rect.min + vec2(17.0 * px - galley.size().x - 2.0 * px, -2.0 * px),
            galley.size() + vec2(2.0, 1.0) * px,
        );
        painter.rect_filled(badge.expand(px), 0.0, INK);
        painter.rect_filled(badge, 0.0, GOLD);
        painter.galley(badge.min + vec2(px, 0.0), galley, INK);
    }
}

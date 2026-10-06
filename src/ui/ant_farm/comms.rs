//! The Envoy's surface receiver and underground listening post.

use eframe::egui::{Color32, Painter, Pos2, Rect, Stroke, pos2, vec2};

use super::{layout::Scene, sprites::Sprites};
use crate::envoy::EnvoyHealth;

pub fn paint(painter: &Painter, sprites: &Sprites, scene: &Scene, health: EnvoyHealth, now_s: f64) {
    let Some(room) = scene.comms else { return };
    let px = scene.px;
    let ink = Color32::from_rgb(29, 43, 48);
    let metal = if health == EnvoyHealth::Stopped {
        Color32::from_rgb(82, 91, 94)
    } else {
        Color32::from_rgb(171, 200, 204)
    };
    let accent = crate::ui::graph::envoy_color(painter.ctx().global_style().visuals.dark_mode);
    sprites.passage(painter, room.shaft, px);
    sprites.passage(painter, room.chamber, px);
    let cable_end = room.chamber.center();
    painter.line_segment([room.dish, cable_end], Stroke::new(3.0 * px, ink));
    painter.line_segment([room.dish, cable_end], Stroke::new(px, metal));

    let pixel = |origin: Pos2, x: f32, y: f32, w: f32, h: f32, color| {
        painter.rect_filled(
            Rect::from_min_size(origin + vec2(x * px, y * px), vec2(w * px, h * px)),
            0.0,
            color,
        );
    };
    // Stepped bowl, mast and feed keep the receiver on the sprite grid.
    pixel(room.dish, -5.0, -2.0, 10.0, 2.0, ink);
    pixel(room.dish, -1.0, -8.0, 2.0, 6.0, metal);
    for (x, y, w, h) in [
        (-8.0, -15.0, 2.0, 5.0),
        (-6.0, -11.0, 2.0, 3.0),
        (-4.0, -9.0, 3.0, 2.0),
        (-1.0, -8.0, 4.0, 2.0),
        (3.0, -10.0, 3.0, 2.0),
    ] {
        pixel(room.dish, x, y, w, h, metal);
    }
    painter.line_segment(
        [room.feed(px), room.dish + vec2(3.0, -16.0) * px],
        Stroke::new(px, metal),
    );
    pixel(room.dish, 2.0, -17.0, 3.0, 3.0, accent);
    let blink = (now_s * 2.0).floor() as i64 % 2 == 0;
    let light = match health {
        EnvoyHealth::Listening => accent,
        EnvoyHealth::AwaitingApproval if blink => Color32::from_rgb(255, 192, 65),
        EnvoyHealth::BridgeDown => Color32::from_rgb(232, 85, 74),
        EnvoyHealth::Starting => Color32::from_rgb(127, 137, 141),
        EnvoyHealth::Stopped | EnvoyHealth::AwaitingApproval => ink,
    };
    if health == EnvoyHealth::Listening {
        let pulse = (now_s * 3.0).sin() as f32 * 0.15 + 0.35;
        for step in [0.0, 4.0] {
            for (x, y) in [(5.0, -23.0), (8.0, -22.0), (10.0, -19.0)] {
                pixel(
                    room.dish,
                    x + step,
                    y - step,
                    2.0,
                    2.0,
                    accent.gamma_multiply(pulse),
                );
            }
        }
    }
    if health == EnvoyHealth::BridgeDown {
        pixel(room.dish, 7.0, -7.0, 2.0, 4.0, light);
        pixel(room.dish, 7.0, -2.0, 2.0, 1.0, light);
    }
    let console = pos2(room.chamber.center().x, room.chamber.top() + 7.0 * px);
    pixel(console, -8.0, -4.0, 16.0, 8.0, ink);
    pixel(console, -6.0, -3.0, 9.0, 5.0, metal);
    pixel(console, -5.0, -2.0, 7.0, 3.0, light.gamma_multiply(0.65));
    pixel(console, 5.0, -1.0, 2.0, 2.0, light);
    pixel(console, -9.0, 4.0, 18.0, 2.0, metal);
}

pub fn headphones(painter: &Painter, cell: Rect, px: f32, facing_right: bool, alpha: f32) {
    let dir = if facing_right { 1.0 } else { -1.0 };
    let head = cell.center() + vec2(dir * 3.0, -2.0) * px;
    let color = Color32::from_rgb(127, 220, 224).gamma_multiply(alpha);
    for (offset, size) in [
        (vec2(-3.0, -4.0), vec2(6.0, 1.0)),
        (vec2(-3.0, -3.0), vec2(1.0, 3.0)),
        (vec2(2.0, -3.0), vec2(1.0, 3.0)),
        (vec2(-4.0, -1.0), vec2(2.0, 3.0)),
        (vec2(2.0, -1.0), vec2(2.0, 3.0)),
    ] {
        painter.rect_filled(
            Rect::from_min_size(head + offset * px, size * px),
            0.0,
            color,
        );
    }
}

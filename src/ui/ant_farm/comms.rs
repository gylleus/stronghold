//! The Envoy's surface receiver and underground listening post.

use eframe::egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, vec2};

use super::{layout::Scene, sprites::Sprites};
use crate::envoy::EnvoyHealth;

pub fn paint(
    painter: &Painter,
    sprites: &Sprites,
    scene: &Scene,
    health: EnvoyHealth,
    now_s: f64,
    receipt: crate::ui::envoy_receipt::ReceiptCue,
) {
    let Some(mut room) = scene.comms else { return };
    let px = room.px;
    let ink = Color32::from_rgb(25, 39, 48);
    let steel = Color32::from_rgb(85, 114, 130);
    let metal = if receipt.highlight > 0.0 {
        Color32::from_rgb(255, 220, 120)
            .lerp_to_gamma(Color32::from_rgb(188, 215, 219), 1.0 - receipt.highlight)
    } else if health == EnvoyHealth::Stopped {
        Color32::from_rgb(82, 91, 94)
    } else {
        Color32::from_rgb(188, 215, 219)
    };
    let accent = crate::ui::graph::envoy_color(painter.ctx().global_style().visuals.dark_mode);
    let white = Color32::from_rgb(229, 244, 239);
    let pixel = |origin: Pos2, x: f32, y: f32, w: f32, h: f32, color| {
        painter.rect_filled(
            Rect::from_min_size(origin + vec2(x * px, y * px), vec2(w * px, h * px)),
            0.0,
            color,
        );
    };
    sprites.passage(painter, room.shaft, scene.px);
    sprites.passage(painter, room.chamber, scene.px);
    // Move the equipment, keeping the soil cutouts and hit geometry fixed.
    let offset = vec2(receipt.offset, 0.0);
    room.chamber = room.chamber.translate(offset);
    room.dish += offset;
    for leg in [room.dish, room.cable_elbow(), room.cable_port()].windows(2) {
        painter.line_segment([leg[0], leg[1]], Stroke::new(3.0 * px, ink));
        painter.line_segment([leg[0], leg[1]], Stroke::new(px, metal));
    }

    pixel(room.dish, -10.0, -3.0, 20.0, 3.0, ink);
    pixel(room.dish, -8.0, -4.0, 16.0, 2.0, steel);
    pixel(room.dish, -4.0, -17.0, 5.0, 13.0, steel);
    pixel(room.dish, -2.0, -17.0, 2.0, 13.0, metal);
    // Rasterize the tilted parabola's face and its thick back shell on the sprite grid.
    for y in -40..=-7 {
        for x in -24..=23 {
            let delta = vec2(x as f32 + 5.0, y as f32 + 16.0);
            let depth = (delta.x - delta.y) * std::f32::consts::FRAC_1_SQRT_2;
            let across = (delta.x + delta.y) * std::f32::consts::FRAC_1_SQRT_2;
            let curve = across * across / 44.0;
            if across.abs() <= 20.0 && depth >= curve - 3.0 && depth <= 9.5 {
                let color = if depth < curve {
                    steel
                } else if depth > 7.5 {
                    white
                } else {
                    metal
                };
                pixel(room.dish, x as f32, y as f32, 1.0, 1.0, color);
            }
        }
    }
    painter.line_segment(
        [room.dish + vec2(16.0, -8.0) * px, room.feed()],
        Stroke::new(2.0 * px, ink),
    );
    painter.line_segment(
        [room.dish + vec2(16.0, -8.0) * px, room.feed()],
        Stroke::new(px, white),
    );
    pixel(room.feed(), -2.0, -2.0, 5.0, 5.0, ink);
    pixel(room.feed(), -1.0, -1.0, 3.0, 3.0, accent);

    let blink = (now_s * 2.0).floor() as i64 % 2 == 0;
    let light = match health {
        EnvoyHealth::Listening => Color32::from_rgb(100, 230, 207),
        EnvoyHealth::AwaitingApproval if blink => Color32::from_rgb(255, 192, 65),
        EnvoyHealth::BridgeDown => Color32::from_rgb(243, 77, 66),
        EnvoyHealth::Starting => steel,
        EnvoyHealth::Stopped | EnvoyHealth::AwaitingApproval => ink,
    };
    if health == EnvoyHealth::Listening {
        let phase = (now_s * 0.8).fract() as f32;
        let signal = if crate::ui::daylight::daylight() > 0.5 {
            Color32::from_rgb(24, 99, 139)
        } else {
            Color32::from_rgb(139, 235, 255)
        };
        for ring in 0..3 {
            let radius = 10.0 + ring as f32 * 7.0 + phase * 5.0;
            for degree in (-80..=-10).step_by(3) {
                let angle = (degree as f32).to_radians();
                pixel(
                    room.feed(),
                    (radius * angle.cos()).round(),
                    (radius * angle.sin()).round(),
                    1.5,
                    1.5,
                    signal.gamma_multiply(0.95 - phase * 0.35),
                );
            }
        }
    }
    if health == EnvoyHealth::BridgeDown {
        pixel(room.dish, -14.0, -24.0, 10.0, 13.0, ink);
        pixel(room.dish, -12.0, -22.0, 6.0, 6.0, light);
        pixel(room.dish, -12.0, -14.0, 6.0, 2.0, light);
    }

    let console = room.console();
    pixel(console, -9.0, -13.0, 18.0, 22.0, ink);
    pixel(console, -8.0, -11.0, 16.0, 18.0, steel);
    pixel(console, -8.0, -9.0, 11.0, 12.0, ink);
    pixel(console, -7.0, -8.0, 9.0, 10.0, light.gamma_multiply(0.35));
    if health != EnvoyHealth::Stopped {
        for (x, y, w) in [(-6.0, -5.0, 3.0), (-3.0, -7.0, 2.0), (-2.0, -3.0, 3.0)] {
            pixel(console, x, y, w, 1.0, light);
        }
    }
    pixel(console, 5.0, -8.0, 3.0, 5.0, light);
    for row in 0..3 {
        pixel(console, 5.0, row as f32 * 3.0, 3.0, 1.0, metal);
    }
    pixel(console, -7.0, 5.0, 10.0, 2.0, metal);
    pixel(console, -10.0, 9.0, 20.0, 3.0, metal);
    pixel(console, -8.0, 12.0, 2.0, 6.0, steel);
    pixel(console, 6.0, 12.0, 2.0, 6.0, steel);
    // A rack and wall label distinguish the room from a worker's empty digging chamber.
    let rack = room.chamber.min + vec2(6.0, 10.0) * px;
    pixel(rack, 0.0, 0.0, 8.0, 23.0, ink);
    for row in 0..4 {
        pixel(rack, 1.0, 1.0 + row as f32 * 5.0, 6.0, 4.0, steel);
        pixel(rack, 2.0, 2.0 + row as f32 * 5.0, 2.0, 1.0, light);
    }
    painter.text(
        room.chamber.min + vec2(5.0, 3.0) * px,
        Align2::LEFT_TOP,
        "COMMS",
        FontId::monospace(5.0 * px + 2.0),
        metal,
    );
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

pub fn tray(painter: &Painter, scene: &Scene, pending: usize, hovered: bool, offset: f32) {
    let Some(room) = scene.comms else { return };
    let rect = room.tray_rect().translate(vec2(offset, 0.0));
    let px = room.px;
    let ink = Color32::from_rgb(25, 39, 48);
    let metal = Color32::from_rgb(188, 215, 219);
    let amber = Color32::from_rgb(255, 192, 65);
    let pixel = |x, y, w, h, color| {
        painter.rect_filled(
            Rect::from_min_size(rect.min + vec2(x, y) * px, vec2(w, h) * px),
            0.0,
            color,
        );
    };
    if hovered {
        painter.rect_filled(rect.expand(px), 0.0, amber.gamma_multiply(0.35));
    }
    for y in [7.0, 18.0] {
        pixel(0.0, y, 17.0, 7.0, ink);
        pixel(1.0, y + 1.0, 15.0, 4.0, metal);
        pixel(3.0, y - 4.0, 11.0, 6.0, Color32::from_rgb(229, 244, 239));
        painter.line_segment(
            [
                rect.min + vec2(3.0, y - 4.0) * px,
                rect.min + vec2(8.0, y) * px,
            ],
            Stroke::new(px, ink),
        );
        painter.line_segment(
            [
                rect.min + vec2(8.0, y) * px,
                rect.min + vec2(14.0, y - 4.0) * px,
            ],
            Stroke::new(px, ink),
        );
        pixel(1.0, y + 5.0, 15.0, 1.0, metal);
    }
    if pending > 0 {
        let text = pending.to_string();
        let font = FontId::monospace(6.0 * px);
        let galley = painter.layout_no_wrap(text, font, ink);
        let badge = Rect::from_min_size(
            rect.min + vec2(17.0 * px - galley.size().x - 2.0 * px, -2.0 * px),
            galley.size() + vec2(2.0, 1.0) * px,
        );
        painter.rect_filled(badge, 0.0, amber);
        painter.galley(badge.min + vec2(px, 0.0), galley, ink);
    }
}

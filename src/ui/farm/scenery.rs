//! Where the surface's nature goes: a pure function of the scene (and, for the sky, the
//! time), so it stays put from frame to frame and is testable.

use eframe::egui::{Pos2, pos2};

use super::layout::Scene;
use super::nature::Deco;

/// One piece of scenery, anchored at the middle of its bottom edge, in screen points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub deco: Deco,
    pub foot: Pos2,
}

/// Stars in the night sky.
const STARS: usize = 36;

/// A small deterministic hash, so scenery is the same every frame.
fn hash(i: usize, salt: u32) -> u32 {
    let mut h = (i as u32).wrapping_mul(0x9e37_79b1) ^ salt.wrapping_mul(0x85eb_ca77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h
}

/// The plants, rocks and mushrooms along the surface, left to right from past the landing to
/// the right edge, each standing a texel deep in the grass.
pub fn surface(scene: &Scene) -> Vec<Placement> {
    let px = scene.px;
    let a = scene.cell();
    // With the rune stone at the left, the user's portal stands past the waiting dwarves.
    let start = scene.user_portal().x + 1.5 * a;
    let end = scene.rect.right() - 0.5 * a;
    let mut out = Vec::new();
    let mut x = start;
    for i in 0.. {
        let h = hash(i, 7);
        let deco = match h % 100 {
            0..40 => Deco::Boulder,
            40..62 => Deco::Heather,
            62..75 => Deco::Edelweiss,
            75..86 => Deco::Gentian,
            86..94 => Deco::Rock,
            _ => Deco::Mushroom,
        };
        let w = deco.extent(px)[0];
        if x + w > end {
            break;
        }
        out.push(Placement {
            deco,
            foot: pos2(x + w / 2.0, scene.surface_y + px),
        });
        let gap = (2 + (h / 7) % 7) as f32 * px;
        x += w + gap;
    }
    out
}

/// The sky's pieces at `now_s`: the moon (`dark`) or the sun near the top right, and three
/// motionless alpine clouds. The clock changes the lighting, never idle animation.
pub fn sky(scene: &Scene, _now_s: f64, dark: bool) -> Vec<Placement> {
    let px = scene.px;
    let a = scene.cell();
    let rect = scene.rect;
    let height = scene.surface_y - rect.top();
    let orb = if dark { Deco::Moon } else { Deco::Sun };
    let orb_h = orb.extent(px)[1];
    let mut out = vec![Placement {
        deco: orb,
        foot: pos2(rect.right() - 2.5 * a, rect.top() + 0.3 * a + orb_h),
    }];
    let [cloud_w, cloud_h] = Deco::Cloud.extent(px);
    let span = rect.width() + cloud_w;
    for (base, y) in [(0.12, 0.30), (0.47, 0.18), (0.78, 0.40)].iter() {
        let travelled = base * span;
        let left = rect.left() - cloud_w + travelled;
        let top = rect.top() + (y * height).min(height - cloud_h - a).max(0.0);
        out.push(Placement {
            deco: Deco::Cloud,
            foot: pos2(left + cloud_w / 2.0, top + cloud_h),
        });
    }
    out
}

/// Stars in the upper sky, and whether each is a bright one. They hold still.
pub fn stars(scene: &Scene) -> Vec<(Pos2, bool)> {
    let rect = scene.rect;
    let height = (scene.surface_y - rect.top()) * 0.7;
    (0..STARS)
        .map(|i| {
            let h = hash(i, 31);
            let x = rect.left() + (h % 10_000) as f32 / 10_000.0 * rect.width();
            let y = rect.top() + (h / 10_000 % 1_000) as f32 / 1_000.0 * height;
            let p = pos2(
                (x / scene.px).round() * scene.px,
                (y / scene.px).round() * scene.px,
            );
            (p, h.is_multiple_of(5))
        })
        .collect()
}

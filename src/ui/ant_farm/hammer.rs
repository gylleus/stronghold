//! Generated hammer source, reduced to hard farm texels and the mine's material colours.
//! The shared prop cache applies the selected world palette after this pass.
use std::sync::OnceLock;

use eframe::egui::{Color32, ColorImage, Pos2, Rect, Vec2, vec2};

use super::world::{GOLD_DARK, INK, IRON, STONE_LIGHT, WOOD, WOOD_DARK, WOOD_LIGHT, rgb};
use crate::ui::dwarf::{
    cutout::Outline,
    pixel::{Mapper, Palette},
};

fn source() -> &'static image::RgbaImage {
    static SOURCE: OnceLock<image::RgbaImage> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let image = image::load_from_memory(include_bytes!(
            "../../../assets/art/tools/cleanup_hammer.png"
        ))
        .expect("bundled hammer PNG")
        .to_rgba8();
        let (mut left, mut top, mut right, mut bottom) = (image.width(), image.height(), 0, 0);
        for (x, y, p) in image.enumerate_pixels() {
            if p[3] >= 128 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x);
                bottom = bottom.max(y);
            }
        }
        image::imageops::crop_imm(&image, left, top, right - left + 1, bottom - top + 1).to_image()
    })
}

pub fn render(size: [usize; 2], outline: Outline) -> ColorImage {
    let source = source();
    let [w, h] = size;
    let mut image = ColorImage::filled(size, Color32::TRANSPARENT);
    let pad = outline.pixels().ceil() as usize;
    let scale = ((w.saturating_sub(2 * pad)).max(1) as f32 / source.width() as f32)
        .min((h.saturating_sub(2 * pad)).max(1) as f32 / source.height() as f32);
    let sw = (source.width() as f32 * scale).round().max(1.0) as usize;
    let sh = (source.height() as f32 * scale).round().max(1.0) as usize;
    let ox = w.saturating_sub(sw) / 2;
    let oy = h.saturating_sub(sh) / 2;
    let mut mapper = Mapper::new(&Palette {
        name: "mine materials".into(),
        colors: vec![
            INK,
            IRON,
            STONE_LIGHT,
            rgb(0xc1bba7),
            WOOD,
            WOOD_DARK,
            WOOD_LIGHT,
            GOLD_DARK,
            rgb(0xc8963c),
        ],
    });
    for y in 0..sh.min(h) {
        for x in 0..sw.min(w) {
            let sx = ((x as f32 + 0.5) / sw as f32 * source.width() as f32) as u32;
            let sy = ((y as f32 + 0.5) / sh as f32 * source.height() as f32) as u32;
            let p = source.get_pixel(sx.min(source.width() - 1), sy.min(source.height() - 1));
            if p[3] >= 128 {
                image[(ox + x, oy + y)] = Color32::from_rgb(p[0], p[1], p[2]);
            }
        }
    }
    mapper.apply(&mut image);
    let silhouette = image.clone();
    let radius = outline.pixels();
    let reach = radius.ceil() as isize;
    for y in 0..h {
        for x in 0..w {
            if silhouette[(x, y)].a() != 0 {
                continue;
            }
            'neighbours: for dy in -reach..=reach {
                for dx in -reach..=reach {
                    if (dx * dx + dy * dy) as f32 > radius * radius {
                        continue;
                    }
                    let (sx, sy) = (x as isize + dx, y as isize + dy);
                    if sx >= 0
                        && sy >= 0
                        && sx < w as isize
                        && sy < h as isize
                        && silhouette[(sx as usize, sy as usize)].a() != 0
                    {
                        image[(x, y)] = INK;
                        break 'neighbours;
                    }
                }
            }
        }
    }
    image
}

/// Slow backswing, fast strike, then a short hold at contact. Frozen at contact for reduced motion.
pub fn phase(now_s: f64, still: bool) -> f32 {
    if still {
        0.7
    } else {
        (now_s / 1.2).rem_euclid(1.0) as f32
    }
}

/// Upright sprite rotated about its grip; the head meets `impact` at the end of each stroke.
pub fn placement(impact: Pos2, scale: f32, phase: f32) -> (Rect, (f32, Pos2)) {
    let angle = if phase < 0.48 {
        -2.05 + 1.55 * (phase / 0.48)
    } else if phase < 0.68 {
        -0.5 - 1.55 * ((phase - 0.48) / 0.2).powi(2)
    } else {
        -2.05
    };
    let size = vec2(26.0, 40.0) * scale;
    let contact = head_offset(scale);
    let pivot = impact - eframe::egui::emath::Rot2::from_angle(-2.05) * contact;
    let rect = Rect::from_min_size(pivot - vec2(size.x / 2.0, size.y - 4.0 * scale), size);
    (rect, (angle, pivot))
}

fn head_offset(scale: f32) -> Vec2 {
    vec2(0.0, -30.0) * scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;
    #[test]
    fn sprite_is_hard_alpha_and_outline_changes_it() {
        let fine = render([26, 40], Outline::Fine);
        assert!(fine.pixels.iter().all(|p| p.a() == 0 || p.a() == 255));
        assert!(fine.pixels.iter().filter(|p| p.a() == 255).count() > 150);
        assert_ne!(fine, render([26, 40], Outline::Bold));
    }
    #[test]
    fn strike_contacts_rock_with_the_grip_above_ground() {
        let impact = pos2(60.0, 50.0);
        for scale in [0.75, 1.0, 2.0] {
            let (_, (angle, pivot)) = placement(impact, scale, phase(0.0, true));
            let head = pivot + eframe::egui::emath::Rot2::from_angle(angle) * head_offset(scale);
            assert!(head.distance(impact) < 0.001);
            assert!(pivot.y < impact.y, "grip stays above the rock");
        }
    }
    #[test]
    fn reduced_motion_freezes_the_strike() {
        assert_eq!(
            placement(pos2(50.0, 50.0), 1.0, phase(1.0, true)),
            placement(pos2(50.0, 50.0), 1.0, phase(2.0, true))
        );
        assert_ne!(phase(1.0, false), phase(1.5, false));
    }
}

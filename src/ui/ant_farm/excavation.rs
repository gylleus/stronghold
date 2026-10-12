//! Room silhouettes at their actual texel size: rough excavations and dressed stone.
use super::world::{Canvas, GOLD_DARK, STONE, pts, rgb};

pub const WALL: eframe::egui::Color32 = rgb(0x4c4b44);

/// Repeatable irregularity for stone faces, independent of the animation clock.
fn grain(i: u32, seed: u8) -> f32 {
    let mut h = i.wrapping_mul(0x9e3779b1) ^ (seed as u32).wrapping_mul(0x85ebca77);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    (h >> 16) as f32 / 65535.0
}

/// Broken slabs around an excavated face. Detail collects at the edges so workers read.
pub fn cave(c: &mut Canvas, w: f32, h: f32, seed: u8) {
    let dark = rgb(0x3d3c37);
    let light = rgb(0x646258);
    let corner = (h * 0.18).min(10.0).min(w * 0.15);
    let mut roof = vec![(1.0, corner + 4.0), (corner, 2.0)];
    let mut x = corner + 8.0;
    let mut i = 0;
    while x < w - corner - 6.0 {
        roof.push((x, 1.0 + grain(i, seed) * 5.0));
        x += 9.0 + grain(i + 41, seed) * 14.0;
        i += 1;
    }
    roof.push((w - corner, 3.0));
    let mut outline = roof.clone();
    outline.extend([
        (w - 2.0, corner),
        (w - 4.0, h * 0.37),
        (w - 1.0, h * 0.69),
        (w - 3.0, h - 5.0),
        (w - 10.0, h - 1.0),
        (w * 0.66, h - 2.0),
        (w * 0.38, h - 1.0),
        (9.0, h - 2.0),
        (2.0, h - 7.0),
        (4.0, h * 0.57),
        (1.0, h * 0.38),
    ]);
    c.cut(pts(&outline), WALL);
    // Unequal overlapping slabs replace the scalloped ceiling fringe.
    for (i, pair) in roof[1..].windows(2).enumerate() {
        let [(x, y), (xx, yy)] = [pair[0], pair[1]];
        let drop = 3.0 + grain(i as u32 + 83, seed) * 8.0;
        c.paint(
            pts(&[
                (x, y + 0.8),
                (xx, yy + 0.8),
                (xx - 3.0, yy + drop),
                (x + 3.0, y + drop + 2.0),
            ]),
            dark,
        );
        c.paint(
            pts(&[
                (x, y + 0.8),
                (xx, yy + 0.8),
                (xx - 6.0, yy + 3.0),
                (x + 2.0, y + 4.0),
            ]),
            light,
        );
    }
    // Cleaved sidewalls recede towards the flatter, dimmer working face.
    c.paint(
        pts(&[
            (3.0, corner + 5.0),
            (10.0, corner + 2.0),
            (15.0, h * 0.37),
            (9.0, h * 0.63),
            (11.0, h - 11.0),
            (4.0, h - 7.0),
            (5.0, h * 0.49),
        ]),
        light,
    );
    c.paint(
        pts(&[
            (9.0, corner + 8.0),
            (15.0, h * 0.37),
            (11.0, h * 0.48),
            (7.0, h * 0.34),
        ]),
        dark,
    );
    c.paint(
        pts(&[
            (w - 3.0, corner + 3.0),
            (w - 14.0, corner + 9.0),
            (w - 10.0, h * 0.38),
            (w - 17.0, h * 0.61),
            (w - 7.0, h - 10.0),
            (w - 2.0, h - 6.0),
            (w - 5.0, h * 0.5),
        ]),
        dark,
    );
    c.paint(
        pts(&[
            (w - 12.0, corner + 10.0),
            (w - 6.0, corner + 7.0),
            (w - 5.0, h * 0.44),
            (w - 10.0, h * 0.4),
        ]),
        light,
    );
    // Discontinuous bedding planes and the diagonal scars of a pick, not wallpaper.
    for i in 0..(w / 24.0) as u32 {
        let x = 15.0 + grain(i + 101, seed) * (w - 35.0).max(1.0);
        let y = 15.0 + grain(i + 207, seed) * (h - 30.0).max(1.0);
        let span = (9.0 + grain(i + 51, seed) * 17.0).min(w - x - 10.0);
        if span < 4.0 {
            continue;
        }
        c.line((x, y), (x + span * 0.7, y - 3.0), 0.5, dark);
        c.line((x + span * 0.7, y - 3.0), (x + span, y - 2.0), 0.5, dark);
        if i % 2 == 0 {
            c.line(
                (x + span * 0.7, y - 3.0),
                (x + span * 0.7 - 2.0, y - 8.0),
                0.5,
                dark,
            );
        } else {
            for d in [0.0, 3.0, 6.0] {
                c.line((x + d, y + 3.0), (x + d + 2.0, y + 1.0), 0.45, light);
            }
        }
    }
    c.paint(
        pts(&[
            (5.0, h - 7.0),
            (w * 0.28, h - 4.0),
            (w * 0.62, h - 6.0),
            (w - 4.0, h - 7.0),
            (w - 9.0, h - 2.0),
            (9.0, h - 3.0),
        ]),
        dark,
    );
    // Scree gathers below exposed rock, with a few chips further out on the floor.
    for side in [false, true] {
        let count = 3 + (seed as u32 + u32::from(side)) % 3;
        for i in 0..count {
            let dx = 6.0 + i as f32 * 3.7 + grain(i + 300, seed) * 3.0;
            let x = if side { w - dx } else { dx };
            let y = h - 3.0 - grain(i + 331, seed) * 4.0;
            let r = 1.0 + grain(i + 410, seed) * 2.8;
            c.paint(
                pts(&[
                    (x - r - 1.0, y),
                    (x - r * 0.5, y - r - 1.0),
                    (x + r * 0.5, y - r),
                    (x + r + 1.0, y + 1.0),
                    (x - r, y + 1.0),
                ]),
                dark,
            );
            c.paint(
                pts(&[
                    (x - r, y - 0.5),
                    (x - r * 0.4, y - r),
                    (x + r * 0.3, y - r + 0.5),
                    (x + r, y),
                ]),
                light,
            );
        }
    }
    if w > 50.0 && h > 36.0 {
        let x = w - 11.0;
        let y = h * 0.54;
        for (a, b) in [
            ((x, y), (x - 3.0, y + 4.0)),
            ((x - 3.0, y + 4.0), (x - 1.0, y + 9.0)),
            ((x - 3.0, y + 4.0), (x - 7.0, y + 5.0)),
        ] {
            c.line(a, b, 0.6, GOLD_DARK);
        }
        // Old drill holes beside the seam, with a pale chipped lip.
        for d in [0.0, 3.0, 6.0] {
            c.detail(x - 2.0, y - 7.0 + d, 1.0, 1.0, light);
            c.detail(x - 1.0, y - 6.0 + d, 1.0, 1.0, dark);
        }
        // Low, worn pit-props bracing one side of the working face.
        if seed.is_multiple_of(3) {
            let wood = super::world::WOOD_DARK;
            c.cut(
                pts(&[
                    (w - 12.0, 10.0),
                    (w - 9.0, 10.0),
                    (w - 7.0, h - 7.0),
                    (w - 10.0, h - 7.0),
                ]),
                wood,
            );
            c.line(
                (w - 10.0, 12.0),
                (w - 8.0, h - 9.0),
                0.45,
                super::world::WOOD,
            );
            c.cut(
                pts(&[
                    (w - 22.0, 9.0),
                    (w - 5.0, 7.0),
                    (w - 5.0, 10.0),
                    (w - 22.0, 12.0),
                ]),
                wood,
            );
        }
    }
}

/// Staggered, chipped blocks for the hold's finished rooms. Dark mortar, matte faces.
pub fn masonry(c: &mut Canvas, w: f32, h: f32) {
    c.rect(0.5, 0.5, w - 1.0, h - 1.0, rgb(0x393630));
    let mut y = 2.0;
    let mut row = 0;
    while y < h - 2.0 {
        let height = 11.0_f32.min(h - 2.0 - y);
        let mut x = if row % 2 == 0 { 2.0 } else { -9.0 };
        while x < w - 2.0 {
            let left = x.max(2.0);
            let right = (x + 23.0).min(w - 2.0);
            if right - left > 2.0 {
                let fill = if row % 3 == 1 {
                    rgb(0x605d53)
                } else {
                    rgb(0x69655b)
                };
                c.paint(
                    pts(&[
                        (left + 1.0, y),
                        (right - 1.0, y),
                        (right, y + 2.0),
                        (right - 1.0, y + height),
                        (left, y + height - 1.0),
                        (left, y + 1.0),
                    ]),
                    fill,
                );
                c.line((left + 3.0, y + 3.0), (left + 5.0, y + 2.0), 0.45, STONE);
                if (row + (x.max(0.0) / 25.0) as usize).is_multiple_of(4) {
                    c.line(
                        (right - 6.0, y + 1.0),
                        (right - 8.0, y + 4.0),
                        0.45,
                        rgb(0x49473e),
                    );
                    c.line(
                        (right - 8.0, y + 4.0),
                        (right - 7.0, y + 6.0),
                        0.45,
                        rgb(0x49473e),
                    );
                }
            }
            x += 25.0;
        }
        y += 13.0;
        row += 1;
    }
}

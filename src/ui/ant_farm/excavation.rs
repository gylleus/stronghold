//! Room silhouettes at their actual texel size: rough excavations and dressed stone.
use super::world::{Canvas, GOLD_DARK, STONE, pts, rgb};

pub const WALL: eframe::egui::Color32 = rgb(0x50483f);

/// Angular overhangs, exposed strata and scree. Keep the centre clear for the workers.
pub fn cave(c: &mut Canvas, w: f32, h: f32, seed: u8) {
    let s = seed as f32;
    let corner = (h * 0.18).min(10.0).min(w * 0.15);
    let roof = |x: f32| 3.0 + ((x * 0.17 + s).sin() * 2.3).round();
    let mut outline = vec![(1.0, corner + 4.0), (corner, 3.0)];
    let mut x = corner + 8.0;
    while x < w - corner {
        outline.push((x, roof(x)));
        x += 9.0;
    }
    outline.extend([
        (w - corner, 3.0),
        (w - 2.0, corner),
        (w - 4.0, h * 0.42),
        (w - 1.0, h * 0.63),
        (w - 3.0, h - 5.0),
        (w - 10.0, h - 1.0),
        (w * 0.68, h - 2.0),
        (w * 0.43, h - 1.0),
        (9.0, h - 2.0),
        (2.0, h - 7.0),
        (4.0, h * 0.57),
        (1.0, h * 0.38),
    ]);
    c.cut(pts(&outline), WALL);
    // Freshly exposed planes: broad diagonal faces, never square texture patches.
    let dark = rgb(0x423c35);
    let light = rgb(0x655a4c);
    x = corner;
    while x < w - corner - 12.0 {
        let span = (17.0 + (x + s).sin().abs() * 13.0).min(w - corner - x);
        let y = roof(x).max(3.0);
        c.paint(
            pts(&[
                (x, y),
                (x + span, y + 1.0),
                (x + span - 5.0, y + 7.0),
                (x + span * 0.4, y + 11.0),
                (x + 3.0, y + 6.0),
            ]),
            dark,
        );
        c.paint(
            pts(&[
                (x, y),
                (x + span, y + 1.0),
                (x + span - 6.0, y + 4.0),
                (x + 4.0, y + 3.0),
            ]),
            light,
        );
        x += span + 3.0;
    }
    c.paint(
        pts(&[
            (3.0, corner + 6.0),
            (9.0, corner + 2.0),
            (12.0, h * 0.48),
            (7.0, h - 10.0),
            (4.0, h * 0.62),
        ]),
        light,
    );
    c.paint(
        pts(&[
            (w - 4.0, corner + 2.0),
            (w - 11.0, corner + 8.0),
            (w - 8.0, h * 0.55),
            (w - 4.0, h - 7.0),
            (w - 2.0, h * 0.64),
        ]),
        dark,
    );
    // Broken bedding planes in the back wall, with short branching fractures.
    for i in 0..((w / 39.0) as usize).max(1) {
        let x = 16.0 + i as f32 * 37.0;
        let y = (h * (0.36 + ((i + seed as usize) % 3) as f32 * 0.12)).round();
        if x + 16.0 < w - 9.0 {
            c.line((x, y), (x + 11.0, y - 3.0), 0.5, dark);
            c.line((x + 11.0, y - 3.0), (x + 17.0, y - 2.0), 0.5, dark);
            c.line((x + 11.0, y - 3.0), (x + 13.0, y - 7.0), 0.5, dark);
        }
    }
    // A rough ledge at the floor and loose chips beneath the working faces.
    c.paint(
        pts(&[
            (5.0, h - 7.0),
            (w * 0.3, h - 5.0),
            (w * 0.6, h - 7.0),
            (w - 4.0, h - 6.0),
            (w - 9.0, h - 2.0),
            (9.0, h - 3.0),
        ]),
        dark,
    );
    for i in 0..((w / 19.0) as usize).max(1) {
        let jitter = ((i as f32 * 7.3 + s).sin() * 4.0).round();
        let x = 9.0 + i as f32 * 18.0 + jitter;
        let y = h - 4.0 - (i % 3) as f32;
        let r = 1.5 + ((i + seed as usize) % 3) as f32 * 0.7;
        c.cut(
            pts(&[
                (x - r, y),
                (x - r * 0.6, y - r),
                (x + r * 0.4, y - r * 1.1),
                (x + r, y),
                (x + r * 0.3, y + 0.5),
            ]),
            light,
        );
    }
    // A seam still in the right-hand working face.
    if w > 40.0 && h > 32.0 {
        let x = w - 8.0;
        let y = h * 0.58;
        for (a, b) in [
            ((x, y), (x - 3.0, y + 4.0)),
            ((x - 3.0, y + 4.0), (x, y + 8.0)),
            ((x - 3.0, y + 4.0), (x - 6.0, y + 5.0)),
        ] {
            c.line(a, b, 0.6, GOLD_DARK);
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
            }
            x += 25.0;
        }
        y += 13.0;
        row += 1;
    }
}

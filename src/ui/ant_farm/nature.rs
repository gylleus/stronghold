//! Alpine plants and sky pieces, cut and painted with the dwarf's ink tools.
use super::world::{
    Canvas, GOLD, LIGHT, PARCHMENT, STONE, STONE_DARK, STONE_LIGHT, WOOD, WOOD_DARK, WOOD_LIGHT,
    pts, rgb,
};
use crate::ui::dwarf::cutout::{blob, capsule, ellipse};
use eframe::egui::{Color32, pos2};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Deco {
    Pine,
    Spruce,
    Boulder,
    Rock,
    Heather,
    Edelweiss,
    Gentian,
    Mushroom,
    Cloud,
    Moon,
    Sun,
}
impl Deco {
    pub const ALL: [Deco; 11] = [
        Self::Pine,
        Self::Spruce,
        Self::Boulder,
        Self::Rock,
        Self::Heather,
        Self::Edelweiss,
        Self::Gentian,
        Self::Mushroom,
        Self::Cloud,
        Self::Moon,
        Self::Sun,
    ];
    pub fn size(self) -> [usize; 2] {
        match self {
            Self::Pine => [28, 38],
            Self::Spruce => [24, 36],
            Self::Boulder => [21, 14],
            Self::Rock => [10, 7],
            Self::Heather => [16, 11],
            Self::Edelweiss | Self::Gentian => [9, 11],
            Self::Mushroom => [10, 10],
            Self::Cloud => [42, 14],
            Self::Moon | Self::Sun => [20, 20],
        }
    }
    pub fn extent(self, px: f32) -> [f32; 2] {
        self.size().map(|n| n as f32 * px)
    }
    pub fn rooted(self) -> bool {
        matches!(self, Self::Pine | Self::Spruce)
    }
}
const GREEN: Color32 = rgb(0x476351);
const GREEN_DARK: Color32 = rgb(0x2b443c);
const GREEN_LIGHT: Color32 = rgb(0x748267);

pub fn draw(c: &mut Canvas, d: Deco) {
    let [w, h] = d.size().map(|n| n as f32);
    match d {
        Deco::Pine | Deco::Spruce => {
            let x = w / 2.0;
            c.cut(
                capsule(pos2(x - 1.0, h - 10.0), pos2(x, h - 2.0), 2.0),
                WOOD,
            );
            c.line((x - 1.0, h - 6.0), (x + 3.0, h - 2.0), 0.6, WOOD_LIGHT);
            let layers = if d == Deco::Pine { 3 } else { 4 };
            for i in (0..layers).rev() {
                let y = 3.0 + i as f32 * 6.0;
                let r = 4.0 + i as f32 * 3.0;
                c.cut(
                    pts(&[
                        (x, y),
                        (x - r, y + 12.0),
                        (x - r * 0.7, y + 13.0),
                        (x - 1.0, y + 12.0),
                        (x + r, y + 13.0),
                    ]),
                    GREEN,
                );
                c.paint(
                    pts(&[(x, y + 1.0), (x - r + 2.0, y + 11.0), (x - 1.0, y + 9.0)]),
                    GREEN_LIGHT,
                );
                c.paint(
                    pts(&[(x, y + 4.0), (x + r - 1.0, y + 12.0), (x + 1.0, y + 11.0)]),
                    GREEN_DARK,
                );
            }
        }
        Deco::Boulder | Deco::Rock => {
            c.cut(
                blob(
                    &pts(&[
                        (1.0, h - 3.0),
                        (w * 0.2, 4.0),
                        (w * 0.5, 1.0),
                        (w * 0.83, 3.0),
                        (w - 1.0, h - 3.0),
                        (w * 0.75, h - 1.0),
                        (w * 0.2, h - 1.0),
                    ]),
                    3,
                ),
                STONE,
            );
            c.paint(
                pts(&[
                    (2.0, h - 4.0),
                    (w * 0.25, 4.0),
                    (w * 0.5, 2.0),
                    (w * 0.68, 4.0),
                    (w * 0.35, 5.0),
                ]),
                STONE_LIGHT,
            );
            c.paint(
                pts(&[(w * 0.65, 4.0), (w - 2.0, h - 3.0), (w * 0.7, h - 2.0)]),
                STONE_DARK,
            );
            if d == Deco::Boulder {
                c.line((9.0, 6.0), (7.0, 10.0), 0.5, STONE_DARK);
            }
        }
        Deco::Heather => {
            for (x, y) in [(3.0, 5.0), (7.0, 3.0), (11.0, 5.0), (13.0, 3.0)] {
                c.cut(capsule(pos2(8.0, 10.0), pos2(x, y), 0.7), GREEN);
                c.oval(x, y, 1.6, 2.0, rgb(0x9d728d));
                c.paint(ellipse(pos2(x - 0.5, y - 1.0), 0.6, 0.8, 8), rgb(0xc9a1b1));
            }
        }
        Deco::Edelweiss | Deco::Gentian => {
            c.cut(capsule(pos2(4.0, 9.0), pos2(4.0, 4.0), 0.6), GREEN);
            c.paint(pts(&[(4.0, 8.0), (1.0, 6.0), (1.0, 8.0)]), GREEN_LIGHT);
            let color = if d == Deco::Edelweiss {
                PARCHMENT
            } else {
                rgb(0x557fba)
            };
            for (x, y) in [(2.0, 3.0), (5.0, 2.0), (7.0, 4.0), (5.0, 6.0), (2.0, 5.0)] {
                c.cut(capsule(pos2(4.0, 4.0), pos2(x, y), 0.7), color);
            }
            c.detail(4.0, 3.0, 1.0, 2.0, GOLD);
        }
        Deco::Mushroom => {
            c.cut(capsule(pos2(5.0, 5.0), pos2(5.0, 9.0), 1.4), PARCHMENT);
            c.cut(
                blob(
                    &pts(&[
                        (1.0, 5.0),
                        (3.0, 2.0),
                        (6.0, 1.0),
                        (9.0, 4.0),
                        (8.0, 6.0),
                        (3.0, 6.0),
                    ]),
                    4,
                ),
                rgb(0xae4240),
            );
            c.detail(3.0, 3.0, 2.0, 1.0, PARCHMENT);
            c.detail(7.0, 4.0, 1.0, 1.0, PARCHMENT);
        }
        Deco::Cloud => {
            c.cut(
                blob(
                    &pts(&[
                        (2.0, 10.0),
                        (7.0, 7.0),
                        (13.0, 7.0),
                        (16.0, 3.0),
                        (24.0, 2.0),
                        (30.0, 7.0),
                        (36.0, 6.0),
                        (40.0, 10.0),
                        (35.0, 12.0),
                        (8.0, 12.0),
                    ]),
                    4,
                ),
                rgb(0xc8cec7),
            );
            c.paint(
                blob(
                    &pts(&[
                        (6.0, 10.0),
                        (18.0, 9.0),
                        (30.0, 10.0),
                        (36.0, 11.0),
                        (10.0, 12.0),
                    ]),
                    3,
                ),
                rgb(0x969fa4),
            );
        }
        Deco::Moon => {
            c.oval(10.0, 10.0, 8.0, 8.0, rgb(0xd4d0b9));
            for (x, y, r) in [(7.0, 6.0, 2.0), (13.0, 12.0, 2.5), (6.0, 13.0, 1.2)] {
                c.paint(ellipse(pos2(x, y), r, r, 12), rgb(0x9da8a6));
            }
            c.line((6.0, 4.0), (10.0, 3.0), 0.6, PARCHMENT);
        }
        Deco::Sun => {
            for (a, b) in [
                ((10.0, 1.0), (10.0, 3.0)),
                ((10.0, 17.0), (10.0, 19.0)),
                ((1.0, 10.0), (3.0, 10.0)),
                ((17.0, 10.0), (19.0, 10.0)),
            ] {
                c.line(a, b, 0.8, WOOD_DARK);
            }
            c.oval(10.0, 10.0, 6.5, 6.5, GOLD);
            c.paint(ellipse(pos2(8.0, 8.0), 3.5, 3.0, 16), LIGHT);
        }
    }
}

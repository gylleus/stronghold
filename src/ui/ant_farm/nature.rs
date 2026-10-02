//! The surface's pixel art: plants, rocks and sky pieces drawn as character maps, one
//! character per texel. Part of the art layer with [`super::sprites`]; replace a map (or load
//! a PNG in its place) to restyle the surface.

use eframe::egui::{Color32, ColorImage};

/// Something growing or lying on the surface, or hanging in the sky.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Deco {
    Tree,
    Pine,
    Bush,
    FlowerRed,
    FlowerYellow,
    FlowerPurple,
    Tuft,
    Rock,
    Mushroom,
    Cloud,
    Moon,
    Sun,
}

impl Deco {
    pub const ALL: [Deco; 12] = [
        Deco::Tree,
        Deco::Pine,
        Deco::Bush,
        Deco::FlowerRed,
        Deco::FlowerYellow,
        Deco::FlowerPurple,
        Deco::Tuft,
        Deco::Rock,
        Deco::Mushroom,
        Deco::Cloud,
        Deco::Moon,
        Deco::Sun,
    ];

    /// Texels per farm texel: big things are drawn twice as chunky as the ants.
    pub fn scale(self) -> f32 {
        match self {
            Deco::Tree | Deco::Pine | Deco::Bush | Deco::Cloud | Deco::Moon | Deco::Sun => 2.0,
            _ => 1.0,
        }
    }

    /// Size in screen points at `px` points per farm texel.
    pub fn extent(self, px: f32) -> [f32; 2] {
        let [w, h] = self.size();
        let k = px * self.scale();
        [w as f32 * k, h as f32 * k]
    }

    /// Whether it puts roots into the soil below it.
    pub fn rooted(self) -> bool {
        matches!(self, Deco::Tree | Deco::Pine | Deco::Bush)
    }

    fn map(self) -> &'static [&'static str] {
        match self {
            Deco::Tree => TREE,
            Deco::Pine => PINE,
            Deco::Bush => BUSH,
            Deco::FlowerRed | Deco::FlowerYellow | Deco::FlowerPurple => FLOWER,
            Deco::Tuft => TUFT,
            Deco::Rock => ROCK,
            Deco::Mushroom => MUSHROOM,
            Deco::Cloud => CLOUD,
            Deco::Moon => MOON,
            Deco::Sun => SUN,
        }
    }

    /// The petal color `X` stands for in [`FLOWER`].
    fn petals(self) -> Color32 {
        match self {
            Deco::FlowerYellow => rgb(0xf2c94c),
            Deco::FlowerPurple => rgb(0xa77be8),
            _ => rgb(0xe0483e),
        }
    }

    /// Size in texels.
    pub fn size(self) -> [usize; 2] {
        let map = self.map();
        let w = map.iter().map(|r| r.chars().count()).max().unwrap_or(0);
        [w, map.len()]
    }

    /// The map as an image; rows shorter than the widest are padded with transparency.
    pub fn image(self) -> ColorImage {
        let map = self.map();
        let [w, h] = self.size();
        let mut image = ColorImage::filled([w, h], Color32::TRANSPARENT);
        for (y, row) in map.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                image[(x, y)] = color(c, self.petals());
            }
        }
        image
    }
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The palette: `.` is transparent, `X` the flower's petal color.
fn color(c: char, petals: Color32) -> Color32 {
    match c {
        'k' => rgb(0x1f2a1a),
        'G' => rgb(0x2f6b28),
        'g' => rgb(0x4c9a3b),
        'l' => rgb(0x7cc35a),
        'B' => rgb(0x4a2e17),
        'b' => rgb(0x6b4423),
        'y' => rgb(0xf2c94c),
        'o' => rgb(0xf29a3c),
        'w' => rgb(0xf5f0e6),
        'm' => rgb(0xc8372d),
        'S' => rgb(0x5d5d63),
        's' => rgb(0x8e8e93),
        'h' => rgb(0xc4c4c8),
        'c' => rgb(0xffffff),
        'C' => rgb(0xd8e4f0),
        'X' => petals,
        _ => Color32::TRANSPARENT,
    }
}

const TREE: &[&str] = &[
    ".....kkkkk.....",
    "...kkGgglGkk...",
    "..kGgglllgggk..",
    ".kGgglllggggGk.",
    ".kGggllggggglk.",
    "kGgggggggggllgk",
    "kGgglggggggglgk",
    "kGgggllgggggggk",
    ".kGggggggglggGk",
    ".kGGgggggglgGk.",
    "..kGGggggggGk..",
    "...kkGGbGGkk...",
    "....kkkbBkk....",
    "......bB.......",
    "......bB.......",
    "......bB.......",
    ".....bbBB......",
    "....bb..BB.....",
];

const PINE: &[&str] = &[
    "....k....",
    "...kGk...",
    "..kGlgk..",
    "..kGggk..",
    ".kGglggk.",
    "..kGggk..",
    ".kGgglgk.",
    "kGggglggk",
    ".kGggggk.",
    "kGgglgggk",
    "kGgggglgk",
    "kkkkbkkkk",
    "....b....",
    "...bB....",
];

const BUSH: &[&str] = &[
    "...kkkkk...",
    ".kkGgglgkk.",
    "kGgglggggGk",
    "kGggggglggk",
    "kGgglggggGk",
    ".kGGgggGGk.",
];

const FLOWER: &[&str] = &[
    ".X.X.", "XXyXX", ".XXX.", "..g..", ".gg..", "..gg.", "..g..",
];

const TUFT: &[&str] = &[".g...g.", ".g.l.g.", "gG.g.Gg", ".gGgGg.", "..ggg.."];

const ROCK: &[&str] = &["...kkk..", ".kkshsk.", "kSsshhsk", "kSsssssk", ".kkkkkk."];

const MUSHROOM: &[&str] = &[".kkkk.", "kmwmmk", "kmmmwk", ".kwwk.", "..ww..", ".kwwk."];

const CLOUD: &[&str] = &[
    ".......cccc.........",
    "....ccccccccc..ccc..",
    "..cccccccccccccccccc",
    ".ccccccccccccccccccc",
    "cccccccccccccccccccc",
    ".CCCCCCCCCCCCCCCCCC.",
];

const MOON: &[&str] = &[
    "..wwww..", ".wwhwww.", "wwwwwhww", "wwhwwwww", "wwwwwwhw", "wwwhwwww", ".wwwwww.", "..wwww..",
];

const SUN: &[&str] = &[
    "....y....",
    ".y.yyy.y.",
    "..yyyyy..",
    ".yyyoyyy.",
    "yyyoyoyyy",
    ".yyyoyyy.",
    "..yyyyy..",
    ".y.yyy.y.",
    "....y....",
];

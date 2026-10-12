//! Worktree tree states and cleanup sprites.
use eframe::egui::{Color32, ColorImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Deco {
    Tree,
    AutumnTree,
    BareTree,
    Stump,
    Saw,
}
impl Deco {
    pub const ALL: [Self; 5] = [
        Self::Tree,
        Self::AutumnTree,
        Self::BareTree,
        Self::Stump,
        Self::Saw,
    ];
    fn map(self) -> &'static [&'static str] {
        match self {
            Self::Tree | Self::AutumnTree => TREE,
            Self::BareTree => BARE_TREE,
            Self::Stump => STUMP,
            Self::Saw => SAW,
        }
    }
    /// An autumn tree's leaves in place of the greens.
    fn leaves(self, c: char) -> Option<Color32> {
        if self != Deco::AutumnTree {
            return None;
        }
        match c {
            'G' => Some(rgb(0x8a3a1c)),
            'g' => Some(rgb(0xc8682a)),
            'l' => Some(rgb(0xf0b048)),
            _ => None,
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
                image[(x, y)] = self.leaves(c).unwrap_or_else(|| color(c));
            }
        }
        image
    }
}
const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The palette: `.` is transparent.
fn color(c: char) -> Color32 {
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

const BARE_TREE: &[&str] = &[
    "..b.....b...b..",
    "...b...b...b...",
    ".b..b..b..b..b.",
    "..b..b.b.b..b..",
    "...bb..bbb.bb..",
    "b....b.bBb.b...",
    ".bb...bbB.b....",
    "...bb.bbBbb..b.",
    ".....bbbB..bb..",
    "..b...bbB.b....",
    "...bbb.bBb.....",
    "......bbB......",
    "......bbB......",
    "......bB.......",
    "......bB.......",
    "......bB.......",
    ".....bbBB......",
    "....bb..BB.....",
];

const STUMP: &[&str] = &[".kkkk.", "kywwyk", "kbBbBk", "kbBbBk", "bbBbBB"];

const SAW: &[&str] = &[
    "...kkkkk..............",
    "..k.....k.............",
    "..k..kkkkkk...........",
    ".kkkkoooooook.h.h.h.h.",
    "koooooooowook.sssssssh",
    "kooooooooooook.ssssssh",
    ".kooooooooook.h.h.h.h.",
    "..kkkkkkkkkk..........",
];

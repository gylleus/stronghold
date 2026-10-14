//! Dwarves drawn in code, built like cutout puppets: a few pieces (boots, body, arms, head,
//! tool), each tinted from one colour, jointed at the neck and shoulders and posed per
//! frame. A [`Look`] picks the role, parts and colours, so any number of different dwarves
//! come from a seed; an [`Action`] and the time pick the [`Pose`].
//!
//! Units: a dwarf is about 100 units tall, standing on the origin and facing right (+x),
//! y down. [`draw`] scales and mirrors him into place.

pub mod cutout;
mod parts;
pub mod pixel;
mod pose;

use cutout::{Finish, Xf};
use eframe::egui::{Align2, Color32, FontId, Painter, Pos2, Shape, pos2, vec2};
pub use pose::{Action, Eyes, Pose, pose};

/// Who a dwarf is in the hold; sets his kit and the parts he's likely to have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Workers and loose terminals: a digging tool, any hat.
    Miner,
    /// Coordinators and attendants: older, a cap, a vest with a badge, a lantern on a pole.
    Foreman,
    /// Subagents: small, barely bearded, a helmet too big for him, a little lantern.
    Apprentice,
    /// The Envoy: a broad rune hat, long robe and beard, rune staff and blue crystal.
    RunePriest,
    /// The King: a crown, heavy brows, white braids and embroidered regalia.
    King,
}

impl Role {
    pub const ALL: [Role; 5] = [
        Role::Miner,
        Role::Foreman,
        Role::Apprentice,
        Role::RunePriest,
        Role::King,
    ];

    /// Height relative to a miner.
    pub fn scale(self) -> f32 {
        match self {
            Role::Apprentice => 0.8,
            _ => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hair {
    Short,
    Long,
    Ponytail,
    Braid,
}

impl Hair {
    pub const ALL: [Hair; 4] = [Hair::Short, Hair::Long, Hair::Ponytail, Hair::Braid];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Beard {
    Long,
    Forked,
    Bushy,
    Short,
    Braided,
    /// Mutton chops: whiskers down the cheeks, chin bare.
    Chops,
    Goatee,
    Stubble,
}

impl Beard {
    pub const ALL: [Beard; 8] = [
        Beard::Long,
        Beard::Forked,
        Beard::Bushy,
        Beard::Short,
        Beard::Braided,
        Beard::Chops,
        Beard::Goatee,
        Beard::Stubble,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Brows {
    Bushy,
    Thin,
    Wild,
    /// Heavy sweeping brows completely hide the eyes.
    Shaggy,
}

impl Brows {
    pub const ALL: [Brows; 4] = [Brows::Bushy, Brows::Thin, Brows::Wild, Brows::Shaggy];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Nose {
    Round,
    Long,
    Hooked,
}

impl Nose {
    pub const ALL: [Nose; 3] = [Nose::Round, Nose::Long, Nose::Hooked];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hat {
    Helmet,
    CandleHelmet,
    HornedHelmet,
    Cap,
    RuneHat,
    Bandana,
    Bald,
    Crown,
}

impl Hat {
    pub const ALL: [Hat; 8] = [
        Hat::Helmet,
        Hat::CandleHelmet,
        Hat::HornedHelmet,
        Hat::Cap,
        Hat::RuneHat,
        Hat::Bandana,
        Hat::Bald,
        Hat::Crown,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Garment {
    Tunic,
    Apron,
    Vest,
    Robe,
}

impl Garment {
    pub const ALL: [Garment; 4] = [Garment::Tunic, Garment::Apron, Garment::Vest, Garment::Robe];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Pickaxe,
    Hammer,
    Shovel,
    Lantern,
    /// The rune priest's crystal-topped staff.
    Staff,
    /// Empty hands, including the King.
    None,
}

impl Tool {
    pub const ALL: [Tool; 6] = [
        Tool::Pickaxe,
        Tool::Hammer,
        Tool::Shovel,
        Tool::Lantern,
        Tool::Staff,
        Tool::None,
    ];
    /// What miners carry.
    pub const DIGGING: [Tool; 3] = [Tool::Pickaxe, Tool::Hammer, Tool::Shovel];
}

/// A fresh throw uses its own random seed, unrelated to either dwarf's appearance.
pub fn thrown_tool(seed: u64) -> Tool {
    Rng(seed).pick(&[
        Tool::Pickaxe,
        Tool::Hammer,
        Tool::Shovel,
        Tool::Lantern,
        Tool::Staff,
    ])
}

/// Draw the same tool a dwarf carries, centred on `at` and spinning about its centre.
pub fn draw_tool(at: Pos2, size: f32, angle: f32, tool: Tool) -> Vec<Shape> {
    let mut piece = cutout::Piece::default();
    parts::tool(&mut piece, tool, false);
    let (centre, span) = if tool == Tool::Staff {
        (pos2(SHOULDER.x, -46.0), 94.0)
    } else {
        (pos2(SHOULDER.x, -12.0), 44.0)
    };
    let k = size / span;
    let xf = Xf::translate(at.to_vec2())
        .then(Xf::rotate(angle))
        .then(Xf::scale(k, k))
        .then(Xf::translate(-centre.to_vec2()));
    let mut shapes = Vec::new();
    cutout::draw(&mut shapes, &piece, xf, &Finish::INK);
    shapes
}

/// Small things a dwarf may have about him.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Extras {
    pub eyepatch: bool,
    pub pipe: bool,
    pub earring: bool,
    pub scar: bool,
}

/// A guild emblem carried on the hat. Shape survives even a tiny colour palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Crest {
    #[default]
    None,
    Sun,
    Diamond,
}
impl Crest {
    pub fn color(self) -> Option<Color32> {
        match self {
            Self::None => None,
            Self::Sun => Some(Color32::from_rgb(0xd9, 0x8c, 0x40)),
            Self::Diamond => Some(Color32::from_rgb(0x53, 0x9d, 0xbf)),
        }
    }
}

/// Everything that makes one dwarf look like himself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub role: Role,
    pub crest: Crest,
    pub skin: Color32,
    pub hair: Color32,
    pub tunic: Color32,
    pub trousers: Color32,
    pub hair_style: Hair,
    pub beard: Beard,
    pub brows: Brows,
    pub nose: Nose,
    pub hat: Hat,
    pub garment: Garment,
    pub tool: Tool,
    pub extras: Extras,
    /// Nose size, about 0.8–1.3.
    pub nose_size: f32,
    /// Belly width, about 0.9–1.15.
    pub girth: f32,
}

pub const SKINS: [Color32; 6] = [
    Color32::from_rgb(0xf2, 0xc4, 0xa0),
    Color32::from_rgb(0xe0, 0xa8, 0x82),
    Color32::from_rgb(0xc6, 0x8a, 0x64),
    Color32::from_rgb(0xa0, 0x6a, 0x4a),
    Color32::from_rgb(0x7a, 0x4e, 0x36),
    Color32::from_rgb(0xd8, 0xb0, 0x9a),
];

pub const HAIRS: [Color32; 8] = [
    Color32::from_rgb(0xc2, 0x5a, 0x22), // ginger
    Color32::from_rgb(0x8a, 0x3a, 0x1c), // auburn
    Color32::from_rgb(0x4a, 0x30, 0x22), // brown
    Color32::from_rgb(0x2a, 0x22, 0x20), // black
    Color32::from_rgb(0xd8, 0xb0, 0x5a), // blond
    Color32::from_rgb(0x9c, 0x98, 0x92), // grey
    Color32::from_rgb(0xe8, 0xe4, 0xdc), // white
    Color32::from_rgb(0x6e, 0x5a, 0x48), // ash brown
];

pub const TUNICS: [Color32; 6] = [
    Color32::from_rgb(0x4e, 0x6b, 0x3a),
    Color32::from_rgb(0x7a, 0x2e, 0x28),
    Color32::from_rgb(0x34, 0x4e, 0x6a),
    Color32::from_rgb(0x8a, 0x6a, 0x2c),
    Color32::from_rgb(0x5a, 0x48, 0x6e),
    Color32::from_rgb(0x5e, 0x5a, 0x52),
];

const TROUSERS: [Color32; 3] = [
    Color32::from_rgb(0x4a, 0x3a, 0x2c),
    Color32::from_rgb(0x3a, 0x3c, 0x40),
    Color32::from_rgb(0x5a, 0x44, 0x30),
];

const LEATHER: Color32 = Color32::from_rgb(0x3e, 0x2a, 0x1e);
const IRON: Color32 = Color32::from_rgb(0x7c, 0x80, 0x86);
const WOOD: Color32 = Color32::from_rgb(0x8a, 0x5a, 0x30);
const GOLD: Color32 = Color32::from_rgb(0xe0, 0xa8, 0x30);
const RUBY: Color32 = Color32::from_rgb(0xc8, 0x20, 0x30);
const INK: Color32 = Color32::from_rgb(0x1e, 0x16, 0x14);
const QUESTION: Color32 = Color32::from_rgb(0xf4, 0xe8, 0xc8);

impl Look {
    /// A miner picked from `seed`; the same seed always gives the same dwarf.
    pub fn from_seed(seed: u64) -> Self {
        Self::for_role(Role::Miner, seed)
    }

    /// A dwarf in `role` picked from `seed`. Faces, hair and colours vary freely; the role
    /// sets the kit. Crowns are left to the King.
    pub fn for_role(role: Role, seed: u64) -> Self {
        if role == Role::King {
            return Self::king();
        }
        let mut r = Rng(seed ^ 0x9e37_79b9_7f4a_7c15);
        let mut look = Self {
            role,
            crest: Crest::None,
            skin: r.pick(&SKINS),
            hair: r.pick(&HAIRS),
            tunic: r.pick(&TUNICS),
            trousers: r.pick(&TROUSERS),
            hair_style: r.pick(&Hair::ALL),
            beard: r.pick(&[
                Beard::Long,
                Beard::Forked,
                Beard::Bushy,
                Beard::Short,
                Beard::Braided,
                Beard::Chops,
                Beard::Goatee,
            ]),
            brows: r.pick(&Brows::ALL),
            nose: r.pick(&Nose::ALL),
            hat: r.pick(&[
                Hat::Helmet,
                Hat::CandleHelmet,
                Hat::HornedHelmet,
                Hat::Cap,
                Hat::Bandana,
                Hat::Bald,
            ]),
            garment: r.pick(&[
                Garment::Tunic,
                Garment::Apron,
                Garment::Apron,
                Garment::Vest,
            ]),
            tool: r.pick(&Tool::DIGGING),
            extras: Extras {
                eyepatch: r.chance(0.08),
                pipe: r.chance(0.15),
                earring: r.chance(0.2),
                scar: r.chance(0.15),
            },
            nose_size: 0.8 + r.unit() * 0.5,
            girth: 0.9 + r.unit() * 0.25,
        };
        match role {
            Role::Miner | Role::King => {}
            Role::Foreman => {
                look.hair = r.pick(&[HAIRS[5], HAIRS[6], HAIRS[7], HAIRS[2]]);
                look.beard = r.pick(&[Beard::Long, Beard::Braided, Beard::Forked]);
                look.hat = Hat::Cap;
                look.garment = Garment::Vest;
                look.tool = Tool::Lantern;
                look.extras.pipe = r.chance(0.4);
            }
            Role::Apprentice => {
                look.hair = r.pick(&[HAIRS[0], HAIRS[1], HAIRS[2], HAIRS[3], HAIRS[4]]);
                look.beard = r.pick(&[Beard::Stubble, Beard::Stubble, Beard::Goatee]);
                look.hat = Hat::Helmet;
                look.garment = Garment::Tunic;
                look.tool = Tool::Lantern;
                look.extras = Extras::default();
                look.girth = 0.92;
            }
            Role::RunePriest => {
                look.beard = Beard::Braided;
                look.hair = HAIRS[6];
                look.brows = Brows::Shaggy;
                look.hair_style = Hair::Long;
                look.hat = Hat::RuneHat;
                look.garment = Garment::Robe;
                look.tunic = Color32::from_rgb(0x3b, 0x50, 0x62);
                look.tool = Tool::Staff;
                look.extras = Extras::default();
            }
        }
        look
    }

    /// The King: white braids, heavy brows, a crown and embroidered red regalia.
    pub fn king() -> Self {
        Self {
            role: Role::King,
            crest: Crest::None,
            skin: SKINS[1],
            hair: HAIRS[6],
            tunic: Color32::from_rgb(0x8c, 0x1c, 0x24),
            trousers: Color32::from_rgb(0x3a, 0x2a, 0x30),
            hair_style: Hair::Long,
            beard: Beard::Braided,
            brows: Brows::Shaggy,
            nose: Nose::Round,
            hat: Hat::Crown,
            garment: Garment::Tunic,
            tool: Tool::None,
            extras: Extras {
                earring: true,
                ..Extras::default()
            },
            nose_size: 1.2,
            girth: 1.15,
        }
    }
}

/// A dwarvish name for `seed`, such as "Grimdok Coalbeard": a given name of two syllables
/// and a clan name.
pub fn name(seed: u64) -> String {
    const FIRST: [&str; 20] = [
        "Grim", "Thra", "Kor", "Brom", "Gor", "Hor", "Mor", "Rur", "Nor", "Fun", "Gro", "Ulf",
        "Hald", "Brok", "Dun", "Tor", "Bar", "Kel", "Dag", "Vor",
    ];
    const SECOND: [&str; 12] = [
        "ur", "grim", "dok", "ek", "ar", "ri", "bor", "nar", "rik", "dal", "gar", "mund",
    ];
    const CLAN: [&str; 12] = [
        "Iron", "Stone", "Gold", "Coal", "Deep", "Ember", "Flint", "Copper", "Granite", "Anvil",
        "Ale", "Grit",
    ];
    const KIN: [&str; 10] = [
        "foot", "beard", "hammer", "fist", "delve", "brow", "shield", "helm", "pick", "belly",
    ];
    let mut r = Rng(seed ^ 0x51ed_270b_a39f_4c21);
    format!(
        "{}{} {}{}",
        r.pick(&FIRST),
        r.pick(&SECOND),
        r.pick(&CLAN),
        r.pick(&KIN)
    )
}

/// A stable seed for `key` (FNV-1a), for picking a dwarf per agent.
pub fn seed_of(key: &str) -> u64 {
    key.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// splitmix64: small, fast and stable across platforms and versions.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[(self.next() % from.len() as u64) as usize]
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

fn shade(c: Color32, k: f32) -> Color32 {
    let f = |v: u8| (v as f32 * (1.0 - k)) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

fn tint(c: Color32, k: f32) -> Color32 {
    let f = |v: u8| (v as f32 + (255.0 - v as f32) * k) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

const NECK: Pos2 = pos2(3.0, -50.0);
const SHOULDER: Pos2 = pos2(5.0, -43.0);
const BACK_SHOULDER: Pos2 = pos2(-9.0, -44.0);

/// A dwarf as shapes, ready to paint or to rasterize.
#[derive(Debug, Clone, Default)]
pub struct Drawing {
    pub shapes: Vec<Shape>,
    /// A "?" over his head: where, and its height in points.
    pub question: Option<(Pos2, f32)>,
}

/// A "?" `size` points tall centred on `at`, with a soft shadow.
pub fn question(painter: &Painter, at: Pos2, size: f32) {
    let font = FontId::proportional(size);
    painter.text(
        at + vec2(1.2, 1.6) * size / 22.0,
        Align2::CENTER_CENTER,
        "?",
        font.clone(),
        Color32::from_black_alpha(140),
    );
    painter.text(at, Align2::CENTER_CENTER, "?", font, QUESTION);
}

/// The shapes of a dwarf standing on `foot`, `height` points tall (for a miner; other roles
/// scale from it), in `pose`, with `finish`.
pub fn draw(
    foot: Pos2,
    height: f32,
    look: &Look,
    pose: &Pose,
    facing_right: bool,
    finish: &Finish,
) -> Drawing {
    let k = height * look.role.scale() / 100.0;
    let place = Xf::translate(foot.to_vec2() - vec2(0.0, pose.hop * k))
        .then(Xf::scale(if facing_right { k } else { -k }, k))
        .then(Xf::rotate_about(pos2(0.0, -45.0), pose.roll));
    // Squash and stretch keep the volume; lean tips him over his feet.
    let body = place
        .then(Xf::rotate(pose.lean))
        .then(Xf::scale(1.0 - pose.stretch * 0.6, 1.0 + pose.stretch));
    let head = body.then(Xf::rotate_about(NECK, pose.head));
    let arm = body.then(Xf::rotate_about(SHOULDER, -pose.arm));
    let back_arm = body.then(Xf::rotate_about(BACK_SHOULDER, -pose.back_arm));

    let mut out = Vec::new();
    let o = &mut out;
    cutout::draw(o, &parts::back_arm(look), back_arm, finish);
    cutout::draw(
        o,
        &parts::boot(-6.0 - pose.stride, look, true),
        place,
        finish,
    );
    cutout::draw(o, &parts::body(look), body, finish);
    cutout::draw(
        o,
        &parts::boot(6.0 + pose.stride, look, false),
        place,
        finish,
    );
    if look.role == Role::RunePriest && !pose.stowed {
        cutout::draw(o, &parts::priest_staff(pose), body, finish);
    }
    cutout::draw(o, &parts::head(look, pose), head, finish);
    if pose.nugget {
        cutout::draw(o, &parts::nugget(), body, finish);
    }
    cutout::draw(o, &parts::arm(look, pose), arm, finish);
    if look.role == Role::RunePriest && !pose.stowed {
        cutout::draw(o, &parts::priest_spell(pose), body, finish);
    }
    if let Some(s) = pose
        .since_strike
        .filter(|s| *s < 0.16 && look.tool != Tool::None)
    {
        let tip =
            Xf::rotate_about(pos2(SHOULDER.x, -25.5), -pose.grip).apply(parts::tool_tip(look.tool));
        sparks(o, arm.apply(tip), s, k);
    }
    Drawing {
        shapes: out,
        question: (pose.question > 0.0)
            .then(|| (head.apply(pos2(8.0, -108.0)), 22.0 * k * pose.question)),
    }
}

/// Little four-pointed sparks flying off the tool tip `s` seconds after a strike.
fn sparks(out: &mut Vec<Shape>, tip: Pos2, s: f32, k: f32) {
    let spread = 6.0 + s * 160.0;
    let fade = 1.0 - s / 0.16;
    for (i, dir) in [
        vec2(1.0, -0.6),
        vec2(0.4, -1.0),
        vec2(1.0, 0.2),
        vec2(-0.3, -1.0),
    ]
    .into_iter()
    .enumerate()
    {
        let at = tip + dir.normalized() * spread * k * (0.7 + 0.15 * i as f32);
        let r = (2.6 - i as f32 * 0.4) * k * fade.max(0.0) * 1.4;
        let star = [
            at + vec2(0.0, -r * 1.8),
            at + vec2(r * 0.5, -r * 0.5),
            at + vec2(r * 1.8, 0.0),
            at + vec2(r * 0.5, r * 0.5),
            at + vec2(0.0, r * 1.8),
            at + vec2(-r * 0.5, r * 0.5),
            at + vec2(-r * 1.8, 0.0),
            at + vec2(-r * 0.5, -r * 0.5),
        ];
        cutout::fill(out, &star, Color32::from_rgb(0xff, 0xd8, 0x60));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_always_gives_the_same_dwarf() {
        assert_eq!(Look::from_seed(42), Look::from_seed(42));
        let distinct: std::collections::HashSet<String> = (0..64)
            .map(|s| format!("{:?}", Look::from_seed(s)))
            .collect();
        assert!(distinct.len() > 60, "seeds should rarely collide");
        assert_eq!(seed_of("term:abc"), seed_of("term:abc"));
        assert_ne!(seed_of("term:abc"), seed_of("term:abd"));
    }

    #[test]
    fn crowns_are_left_to_the_king() {
        for role in [
            Role::Miner,
            Role::Foreman,
            Role::Apprentice,
            Role::RunePriest,
        ] {
            for s in 0..300 {
                let look = Look::for_role(role, s);
                assert_eq!(look.role, role);
                assert!(look.hat != Hat::Crown, "{look:?}");
            }
        }
        assert_eq!(Look::for_role(Role::King, 7), Look::king());
    }

    /// Each role carries its own kit, whatever the seed.
    #[test]
    fn roles_set_the_kit() {
        for s in 0..100 {
            assert!(Tool::DIGGING.contains(&Look::for_role(Role::Miner, s).tool));
            let foreman = Look::for_role(Role::Foreman, s);
            assert_eq!((foreman.hat, foreman.tool), (Hat::Cap, Tool::Lantern));
            let priest = Look::for_role(Role::RunePriest, s);
            assert_eq!((priest.hat, priest.tool), (Hat::RuneHat, Tool::Staff));
            assert_eq!(priest.garment, Garment::Robe);
            assert_eq!(priest.beard, Beard::Braided);
            assert_eq!(priest.brows, Brows::Shaggy);
            assert_eq!(Look::king().brows, Brows::Shaggy);
            assert_eq!(Look::king().tool, Tool::None);
            let small = Look::for_role(Role::Apprentice, s);
            assert!(matches!(small.beard, Beard::Stubble | Beard::Goatee));
        }
    }

    /// Every part turns up somewhere in a crowd.
    #[test]
    fn crowds_use_every_part() {
        let crowd: Vec<Look> = Role::ALL
            .into_iter()
            .flat_map(|role| (0..400).map(move |seed| Look::for_role(role, seed)))
            .collect();
        for hair in Hair::ALL {
            assert!(crowd.iter().any(|l| l.hair_style == hair), "{hair:?}");
        }
        for brows in Brows::ALL {
            assert!(crowd.iter().any(|l| l.brows == brows), "{brows:?}");
        }
        for nose in Nose::ALL {
            assert!(crowd.iter().any(|l| l.nose == nose), "{nose:?}");
        }
        for garment in Garment::ALL {
            assert!(crowd.iter().any(|l| l.garment == garment), "{garment:?}");
        }
        assert!(crowd.iter().any(|l| l.extras.eyepatch));
        assert!(crowd.iter().any(|l| l.extras.pipe));
    }

    #[test]
    fn names_are_stable_and_varied() {
        assert_eq!(name(9), name(9));
        let names: std::collections::HashSet<String> = (0..200).map(name).collect();
        assert!(names.len() > 190);
        assert!(name(3).contains(' '));
    }

    /// Every role and part draws in every pose without losing a piece.
    #[test]
    fn every_look_draws_in_every_pose() {
        for role in Role::ALL {
            for s in 0..20 {
                let look = Look::for_role(role, s);
                for action in Action::ALL {
                    let d = draw(
                        Pos2::ZERO,
                        100.0,
                        &look,
                        &pose(action, 0.3),
                        s % 2 == 0,
                        &Finish::INK,
                    );
                    assert!(d.shapes.len() > 20, "{role:?} {action:?}");
                }
            }
        }
    }
}

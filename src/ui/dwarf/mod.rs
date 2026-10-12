//! Dwarves drawn in code as paper cutouts: a few pieces (boots, body, arms, head, tool) cut
//! from card, each tinted from one colour, pinned at the neck and shoulder and posed per
//! frame. A [`Look`] picks the styles and colours, so any number of different dwarves come
//! from a seed; an [`Action`] and the time pick the pose.
//!
//! Units: a dwarf is about 100 units tall, standing on the origin and facing right (+x),
//! y down. [`paint`] scales and mirrors him into place.

pub mod paper;

use eframe::egui::{Align2, Color32, FontId, Painter, Pos2, pos2, vec2};
use paper::{Card, Piece, Xf, blob, capsule, ellipse};

/// What a dwarf is doing. There is no idle loop: an idle dwarf stands still.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Standing still (idle, done, parked).
    Still,
    /// Working something out: head tilting, scratching under the hat, a "?" popping.
    Study,
    /// Swinging the tool into the rock.
    Mine,
    Walk,
}

impl Action {
    pub const ALL: [Action; 4] = [Action::Still, Action::Study, Action::Mine, Action::Walk];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Beard {
    Long,
    Forked,
    Bushy,
    Short,
    Braided,
}

impl Beard {
    pub const ALL: [Beard; 5] = [
        Beard::Long,
        Beard::Forked,
        Beard::Bushy,
        Beard::Short,
        Beard::Braided,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hat {
    Helmet,
    CandleHelmet,
    Cap,
    Bald,
    Crown,
}

impl Hat {
    pub const ALL: [Hat; 5] = [
        Hat::Helmet,
        Hat::CandleHelmet,
        Hat::Cap,
        Hat::Bald,
        Hat::Crown,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Pickaxe,
    Hammer,
    Shovel,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::Pickaxe, Tool::Hammer, Tool::Shovel];
}

/// Everything that makes one dwarf look like himself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub skin: Color32,
    pub hair: Color32,
    pub tunic: Color32,
    pub trousers: Color32,
    pub beard: Beard,
    pub hat: Hat,
    pub tool: Tool,
    /// Nose size, about 0.8–1.3.
    pub nose: f32,
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

impl Look {
    /// A dwarf picked from `seed`; the same seed always gives the same dwarf. Crowns are left
    /// to kings.
    pub fn from_seed(seed: u64) -> Self {
        let mut r = Rng(seed ^ 0x9e37_79b9_7f4a_7c15);
        let hats = [Hat::Helmet, Hat::CandleHelmet, Hat::Cap, Hat::Bald];
        Self {
            skin: SKINS[r.below(SKINS.len())],
            hair: HAIRS[r.below(HAIRS.len())],
            tunic: TUNICS[r.below(TUNICS.len())],
            trousers: TROUSERS[r.below(TROUSERS.len())],
            beard: Beard::ALL[r.below(Beard::ALL.len())],
            hat: hats[r.below(hats.len())],
            tool: Tool::ALL[r.below(Tool::ALL.len())],
            nose: 0.8 + r.unit() * 0.5,
            girth: 0.9 + r.unit() * 0.25,
        }
    }
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

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Joint angles and body deformation at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pose {
    /// Front arm, radians forward from hanging straight down.
    pub arm: f32,
    /// Back arm, radians forward.
    pub back_arm: f32,
    /// Head tilt, radians, positive nods forward.
    pub head: f32,
    /// Whole-body lean about the feet, radians, positive forward.
    pub lean: f32,
    /// Squash (negative) or stretch (positive) about the feet.
    pub stretch: f32,
    /// Lift off the ground, units.
    pub hop: f32,
    /// Boot stride, units forward for the front boot (the back boot mirrors it).
    pub stride: f32,
    /// Brows raised (1) or furrowed (-1).
    pub brows: f32,
    /// Seconds since the tool last struck, for sparks.
    pub since_strike: Option<f32>,
    /// A "?" over the head, 0..1 grown.
    pub question: f32,
}

/// The pose for `action` at `t` seconds (each dwarf passes its own phase offset in `t`).
pub fn pose(action: Action, t: f64) -> Pose {
    let rest = Pose {
        arm: 0.25,
        back_arm: -0.1,
        ..Pose::default()
    };
    match action {
        Action::Still => rest,
        Action::Mine => {
            const CYCLE: f64 = 0.95;
            let u = (t.rem_euclid(CYCLE) / CYCLE) as f32;
            // Wind up slowly, hang at the top, strike fast, recoil.
            let (top, hit) = (-2.5, 1.45);
            let (arm, lean, stretch) = if u < 0.42 {
                let k = ease_in_out(u / 0.42);
                (lerp(rest.arm, top, k), -0.08 * k, 0.06 * k)
            } else if u < 0.52 {
                (top, -0.08, 0.06)
            } else if u < 0.6 {
                let k = ease_in((u - 0.52) / 0.08);
                (lerp(top, hit, k), lerp(-0.08, 0.14, k), lerp(0.06, -0.1, k))
            } else {
                let k = ease_out((u - 0.6) / 0.4);
                (
                    lerp(hit, rest.arm, k) + 0.12 * (1.0 - k) * (k * 9.0).sin(),
                    lerp(0.14, 0.0, k),
                    lerp(-0.1, 0.0, k) + 0.03 * (1.0 - k) * (k * 14.0).sin(),
                )
            };
            Pose {
                arm,
                back_arm: arm * 0.35,
                head: lean * 0.8,
                lean,
                stretch,
                brows: -1.0,
                since_strike: (u >= 0.6).then_some((u - 0.6) * CYCLE as f32),
                ..rest
            }
        }
        Action::Study => {
            const CYCLE: f64 = 3.2;
            let u = (t.rem_euclid(CYCLE) / CYCLE) as f32;
            // Lean in and squint, then scratch under the hat while a "?" pops.
            let scratch = window(u, 0.45, 0.85);
            let jiggle = (t as f32 * 26.0).sin() * 0.08 * scratch;
            Pose {
                arm: lerp(rest.arm, -2.15, scratch) + jiggle,
                back_arm: -0.1,
                head: 0.12 * (1.0 - scratch) - 0.14 * scratch + 0.04 * (t as f32 * 2.0).sin(),
                lean: 0.1 * window(u, 0.05, 0.4),
                brows: lerp(-0.8, 1.0, scratch),
                question: grow(window(u, 0.5, 0.92)),
                ..rest
            }
        }
        Action::Walk => {
            const CYCLE: f64 = 0.56;
            let u = (t.rem_euclid(CYCLE) / CYCLE) as f32;
            let s = (u * std::f32::consts::TAU).sin();
            Pose {
                arm: 0.25 - 0.35 * s,
                back_arm: 0.35 * s,
                head: 0.03 * s,
                lean: 0.06,
                hop: 2.2 * s.abs(),
                stride: 5.0 * s,
                stretch: 0.03 * s.abs(),
                ..rest
            }
        }
    }
}

fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

fn ease_in(k: f32) -> f32 {
    k * k * k
}

fn ease_out(k: f32) -> f32 {
    1.0 - (1.0 - k).powi(3)
}

fn ease_in_out(k: f32) -> f32 {
    if k < 0.5 {
        4.0 * k * k * k
    } else {
        1.0 - (-2.0 * k + 2.0).powi(3) / 2.0
    }
}

/// 0 outside `from..to`, rising and falling smoothly inside it.
fn window(u: f32, from: f32, to: f32) -> f32 {
    if u <= from || u >= to {
        return 0.0;
    }
    let k = (u - from) / (to - from);
    let edge = 0.2;
    (k / edge).min((1.0 - k) / edge).min(1.0)
}

/// A pop: overshoots a little on the way to 1.
fn grow(k: f32) -> f32 {
    if k <= 0.0 {
        0.0
    } else {
        1.0 + 0.25 * (1.0 - k) * (k * 9.0).sin()
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

/// Draws a dwarf standing on `foot`, `height` points tall, doing `action` at `t` seconds.
pub fn paint(
    painter: &Painter,
    foot: Pos2,
    height: f32,
    look: &Look,
    pose: &Pose,
    facing_right: bool,
    card: &Card,
) {
    let k = height / 100.0;
    let place = Xf::translate(foot.to_vec2() - vec2(0.0, pose.hop * k))
        .then(Xf::scale(if facing_right { k } else { -k }, k));
    // Squash and stretch keep the volume; lean tips him over his feet.
    let body = place
        .then(Xf::rotate(pose.lean))
        .then(Xf::scale(1.0 - pose.stretch * 0.6, 1.0 + pose.stretch));
    let card = Card {
        shadow: if facing_right {
            card.shadow
        } else {
            vec2(-card.shadow.x, card.shadow.y)
        },
        ..*card
    };
    let head = body.then(Xf::rotate_about(NECK, pose.head));
    let arm = body.then(Xf::rotate_about(SHOULDER, -pose.arm));
    let back_arm = body.then(Xf::rotate_about(BACK_SHOULDER, -pose.back_arm));

    paper::draw(painter, &back_arm_piece(look), back_arm, &card);
    paper::draw(painter, &boot(-6.0 - pose.stride, look, true), place, &card);
    paper::draw(painter, &body_piece(look), body, &card);
    paper::draw(painter, &boot(6.0 + pose.stride, look, false), place, &card);
    paper::draw(painter, &head_piece(look, pose), head, &card);
    paper::pin(painter, NECK + vec2(-6.0, 3.0), 1.6, head);
    paper::draw(painter, &arm_piece(look), arm, &card);
    paper::pin(painter, SHOULDER, 1.8, arm);

    if let Some(s) = pose.since_strike.filter(|s| *s < 0.16) {
        sparks(painter, arm.apply(tool_tip(look.tool)), s, k);
    }
    if pose.question > 0.0 {
        let at = head.apply(pos2(8.0, -108.0));
        let size = 22.0 * k * pose.question;
        let font = FontId::proportional(size);
        painter.text(
            at + vec2(1.2, 1.6) * k,
            Align2::CENTER_CENTER,
            "?",
            font.clone(),
            Color32::from_black_alpha(140),
        );
        painter.text(at, Align2::CENTER_CENTER, "?", font, card.border_color);
    }
}

fn boot(x: f32, look: &Look, back: bool) -> Piece {
    let mut p = Piece::default();
    let leg = if back {
        shade(look.trousers, 0.25)
    } else {
        look.trousers
    };
    let leather = if back { shade(LEATHER, 0.3) } else { LEATHER };
    p.cut(capsule(pos2(x - 1.0, -15.0), pos2(x, -7.0), 4.2), leg);
    p.cut(
        blob(
            &[
                pos2(x - 7.0, -1.0),
                pos2(x - 6.0, -9.0),
                pos2(x + 3.0, -9.5),
                pos2(x + 9.0, -5.0),
                pos2(x + 10.0, -0.5),
            ],
            6,
        ),
        leather,
    );
    p
}

fn body_piece(look: &Look) -> Piece {
    let g = look.girth;
    let mut p = Piece::default();
    p.cut(
        blob(
            &[
                pos2(-17.0 * g, -14.0),
                pos2(-20.0 * g, -30.0),
                pos2(-15.0, -46.0),
                pos2(0.0, -52.0),
                pos2(14.0, -47.0),
                pos2(21.0 * g, -32.0),
                pos2(18.0 * g, -15.0),
                pos2(0.0, -11.0),
            ],
            8,
        ),
        look.tunic,
    );
    // Belt and buckle across the belly.
    p.paint(
        vec![
            pos2(-19.5 * g, -27.0),
            pos2(20.5 * g, -27.0),
            pos2(20.0 * g, -21.5),
            pos2(-19.0 * g, -21.5),
        ],
        LEATHER,
    );
    p.paint(ellipse(pos2(15.0 * g, -24.2), 3.0, 3.4, 10), GOLD);
    p.paint(ellipse(pos2(15.0 * g, -24.2), 1.4, 1.8, 8), LEATHER);
    // A darker hem.
    p.paint(
        blob(
            &[
                pos2(-16.0 * g, -14.5),
                pos2(-6.0, -17.0),
                pos2(8.0, -17.0),
                pos2(17.5 * g, -15.5),
                pos2(10.0, -11.8),
                pos2(-10.0, -12.0),
            ],
            5,
        ),
        shade(look.tunic, 0.25),
    );
    p
}

fn back_arm_piece(look: &Look) -> Piece {
    let mut p = Piece::default();
    let sleeve = shade(look.tunic, 0.3);
    p.cut(capsule(BACK_SHOULDER, pos2(-10.0, -27.0), 5.2), sleeve);
    p.cut(
        ellipse(pos2(-10.0, -25.5), 5.0, 5.0, 14),
        shade(look.skin, 0.2),
    );
    p
}

fn arm_piece(look: &Look) -> Piece {
    let mut p = Piece::default();
    let hand = pos2(SHOULDER.x, -25.5);
    // The tool goes under the fist, so it is cut first.
    tool(&mut p, look.tool, hand);
    p.cut(capsule(SHOULDER, pos2(SHOULDER.x, -28.0), 5.6), look.tunic);
    p.paint(
        capsule(pos2(SHOULDER.x, -31.5), pos2(SHOULDER.x, -29.0), 5.0),
        shade(look.tunic, 0.25),
    );
    p.cut(ellipse(hand, 5.6, 5.4, 16), look.skin);
    p.paint(
        ellipse(hand + vec2(2.0, 1.0), 2.4, 2.0, 10),
        shade(look.skin, 0.15),
    );
    p
}

/// The tool held in a fist at `hand`, its shaft running on past the hand along the arm.
fn tool(p: &mut Piece, tool: Tool, hand: Pos2) {
    let end = tool_tip(tool) - vec2(0.0, 0.0);
    let butt = hand - vec2(0.0, 6.0);
    match tool {
        Tool::Pickaxe => {
            p.cut(capsule(butt, pos2(hand.x, end.y), 1.9), WOOD);
            p.cut(
                blob(
                    &[
                        pos2(hand.x - 13.0, end.y + 6.0),
                        pos2(hand.x - 6.0, end.y - 1.0),
                        pos2(hand.x + 6.0, end.y - 1.0),
                        pos2(hand.x + 15.0, end.y + 7.0),
                        pos2(hand.x + 5.0, end.y + 2.5),
                        pos2(hand.x - 5.0, end.y + 2.5),
                    ],
                    6,
                ),
                IRON,
            );
        }
        Tool::Hammer => {
            p.cut(capsule(butt, pos2(hand.x, end.y), 2.0), WOOD);
            p.cut(
                vec![
                    pos2(hand.x - 9.0, end.y - 4.0),
                    pos2(hand.x + 9.0, end.y - 4.0),
                    pos2(hand.x + 9.0, end.y + 5.0),
                    pos2(hand.x - 9.0, end.y + 5.0),
                ],
                IRON,
            );
            p.paint(
                vec![
                    pos2(hand.x - 9.0, end.y + 2.0),
                    pos2(hand.x + 9.0, end.y + 2.0),
                    pos2(hand.x + 9.0, end.y + 5.0),
                    pos2(hand.x - 9.0, end.y + 5.0),
                ],
                shade(IRON, 0.3),
            );
        }
        Tool::Shovel => {
            p.cut(capsule(butt, pos2(hand.x, end.y - 6.0), 1.9), WOOD);
            p.cut(
                blob(
                    &[
                        pos2(hand.x - 6.0, end.y - 7.0),
                        pos2(hand.x + 6.0, end.y - 7.0),
                        pos2(hand.x + 5.5, end.y + 2.0),
                        pos2(hand.x, end.y + 7.0),
                        pos2(hand.x - 5.5, end.y + 2.0),
                    ],
                    6,
                ),
                IRON,
            );
        }
    }
}

/// Where the tool's working end is, in arm coordinates.
fn tool_tip(tool: Tool) -> Pos2 {
    match tool {
        Tool::Pickaxe => pos2(SHOULDER.x, 2.0),
        Tool::Hammer => pos2(SHOULDER.x, -6.0),
        Tool::Shovel => pos2(SHOULDER.x, 4.0),
    }
}

fn head_piece(look: &Look, pose: &Pose) -> Piece {
    let mut p = Piece::default();
    let skin = look.skin;
    let hair = look.hair;
    // Hair at the back of the head, then the skull and ear.
    if look.hat != Hat::Bald {
        p.cut(
            blob(
                &[
                    pos2(-13.0, -76.0),
                    pos2(-4.0, -82.0),
                    pos2(0.0, -70.0),
                    pos2(-4.0, -58.0),
                    pos2(-12.0, -60.0),
                ],
                6,
            ),
            hair,
        );
    }
    p.cut(ellipse(pos2(5.0, -68.0), 16.5, 15.5, 28), skin);
    p.paint(ellipse(pos2(-2.5, -66.0), 3.6, 4.6, 12), shade(skin, 0.18));
    if look.hat == Hat::Bald {
        // A fringe of hair round the back, a shine on top.
        p.paint(
            blob(
                &[
                    pos2(-11.0, -72.0),
                    pos2(-6.0, -73.0),
                    pos2(-5.0, -62.0),
                    pos2(-10.0, -59.0),
                ],
                5,
            ),
            hair,
        );
        p.paint(ellipse(pos2(6.0, -80.0), 5.0, 2.0, 10), tint(skin, 0.35));
    }
    // Eye and brow.
    let squint = if pose.brows < 0.0 { 1.0 } else { 2.4 };
    p.paint(ellipse(pos2(14.5, -70.0), 1.9, squint, 10), INK);
    let lift = pose.brows * 2.2;
    let tilt = -pose.brows * 1.4;
    p.paint(
        vec![
            pos2(8.0, -75.0 - lift + tilt),
            pos2(20.0, -76.5 - lift - tilt),
            pos2(20.5, -73.2 - lift - tilt),
            pos2(8.5, -72.0 - lift + tilt),
        ],
        shade(hair, 0.2),
    );
    beard(&mut p, look);
    // The nose sits on the beard and moustache.
    let n = look.nose;
    p.cut(
        ellipse(pos2(21.0 + 2.0 * n, -64.5), 6.5 * n, 6.0 * n, 20),
        tint(
            Color32::from_rgb(0xd0, 0x70, 0x60).lerp_to_gamma(skin, 0.45),
            0.0,
        ),
    );
    p.paint(
        ellipse(pos2(19.0 + 2.0 * n, -66.5), 2.0 * n, 1.6 * n, 8),
        tint(skin, 0.35),
    );
    hat(&mut p, look);
    p
}

fn beard(p: &mut Piece, look: &Look) {
    let c = look.hair;
    let outline: Vec<Pos2> = match look.beard {
        Beard::Long => vec![
            pos2(-1.0, -64.0),
            pos2(8.0, -60.0),
            pos2(18.0, -58.5),
            pos2(26.0, -56.0),
            pos2(24.0, -44.0),
            pos2(18.0, -30.0),
            pos2(12.0, -22.0),
            pos2(8.0, -32.0),
            pos2(2.0, -46.0),
            pos2(-3.0, -55.0),
        ],
        Beard::Braided => vec![
            pos2(-1.0, -64.0),
            pos2(8.0, -60.0),
            pos2(18.0, -58.5),
            pos2(26.0, -56.0),
            pos2(22.0, -46.0),
            pos2(17.0, -34.0),
            pos2(15.0, -18.0),
            pos2(11.0, -20.0),
            pos2(9.0, -36.0),
            pos2(2.0, -48.0),
            pos2(-3.0, -55.0),
        ],
        Beard::Forked => vec![
            pos2(-1.0, -64.0),
            pos2(8.0, -60.0),
            pos2(18.0, -58.5),
            pos2(26.0, -56.0),
            pos2(25.0, -44.0),
            pos2(24.0, -30.0),
            pos2(19.0, -38.0),
            pos2(14.0, -28.0),
            pos2(9.0, -40.0),
            pos2(2.0, -48.0),
            pos2(-3.0, -55.0),
        ],
        Beard::Bushy => vec![
            pos2(-3.0, -66.0),
            pos2(8.0, -60.0),
            pos2(20.0, -60.0),
            pos2(29.0, -54.0),
            pos2(29.0, -42.0),
            pos2(20.0, -34.0),
            pos2(8.0, -35.0),
            pos2(-2.0, -42.0),
            pos2(-6.0, -54.0),
        ],
        Beard::Short => vec![
            pos2(-1.0, -64.0),
            pos2(8.0, -60.0),
            pos2(18.0, -59.0),
            pos2(25.0, -56.0),
            pos2(23.0, -47.0),
            pos2(14.0, -44.0),
            pos2(4.0, -47.0),
            pos2(-2.0, -54.0),
        ],
    };
    p.cut(blob(&outline, 6), c);
    // A darker strand down the middle for depth.
    p.paint(
        blob(
            &[
                pos2(10.0, -54.0),
                pos2(16.0, -52.0),
                pos2(14.0, -42.0),
                pos2(11.0, -44.0),
            ],
            5,
        ),
        shade(c, 0.22),
    );
    if look.beard == Beard::Braided {
        for y in [-30.0, -24.0] {
            p.paint(
                vec![
                    pos2(10.5, y - 1.6),
                    pos2(16.5, y - 1.6),
                    pos2(16.5, y + 1.6),
                    pos2(10.5, y + 1.6),
                ],
                GOLD,
            );
        }
    }
    // Moustache under the nose.
    p.cut(
        blob(
            &[
                pos2(12.0, -61.0),
                pos2(21.0, -60.0),
                pos2(30.0, -58.0),
                pos2(28.0, -54.0),
                pos2(20.0, -55.5),
                pos2(13.0, -57.0),
            ],
            5,
        ),
        shade(c, 0.1),
    );
}

fn hat(p: &mut Piece, look: &Look) {
    match look.hat {
        Hat::Helmet | Hat::CandleHelmet => {
            if look.hat == Hat::CandleHelmet {
                p.cut(
                    capsule(pos2(4.0, -91.0), pos2(4.0, -96.0), 2.4),
                    tint(GOLD, 0.6),
                );
                p.cut(
                    blob(
                        &[
                            pos2(4.0, -106.0),
                            pos2(6.6, -99.0),
                            pos2(4.0, -97.0),
                            pos2(1.4, -99.0),
                        ],
                        5,
                    ),
                    Color32::from_rgb(0xff, 0xb0, 0x30),
                );
                p.paint(
                    ellipse(pos2(4.0, -99.5), 1.0, 1.8, 8),
                    Color32::from_rgb(0xff, 0xf0, 0xb0),
                );
            }
            let dome: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
                    pos2(4.0 + 18.0 * a.cos(), -73.0 + 18.0 * a.sin())
                })
                .collect();
            p.cut(dome, IRON);
            p.paint(ellipse(pos2(-2.0, -84.0), 4.5, 2.2, 10), tint(IRON, 0.35));
            p.cut(
                capsule(pos2(-15.0, -73.0), pos2(25.0, -73.0), 2.6),
                shade(IRON, 0.3),
            );
            for x in [-8.0, 2.0, 12.0] {
                p.paint(ellipse(pos2(x, -73.0), 1.1, 1.1, 6), tint(IRON, 0.5));
            }
        }
        Hat::Cap => {
            p.cut(
                blob(
                    &[
                        pos2(-13.0, -74.0),
                        pos2(-8.0, -86.0),
                        pos2(6.0, -89.0),
                        pos2(18.0, -82.0),
                        pos2(28.0, -76.0),
                        pos2(18.0, -74.0),
                    ],
                    6,
                ),
                shade(LEATHER, -0.4),
            );
            p.paint(capsule(pos2(-12.0, -75.0), pos2(18.0, -75.0), 1.4), LEATHER);
        }
        Hat::Crown => {
            p.cut(
                vec![
                    pos2(-10.0, -78.0),
                    pos2(-11.0, -94.0),
                    pos2(-4.0, -86.0),
                    pos2(3.0, -97.0),
                    pos2(10.0, -86.0),
                    pos2(17.0, -95.0),
                    pos2(18.0, -78.0),
                ],
                GOLD,
            );
            p.paint(ellipse(pos2(4.0, -82.0), 2.6, 2.6, 10), RUBY);
            p.paint(ellipse(pos2(-6.0, -82.0), 1.6, 1.6, 8), tint(GOLD, 0.5));
            p.paint(ellipse(pos2(13.0, -82.0), 1.6, 1.6, 8), tint(GOLD, 0.5));
        }
        Hat::Bald => {}
    }
}

/// Little four-pointed sparks flying off the tool tip `s` seconds after a strike.
fn sparks(painter: &Painter, tip: Pos2, s: f32, k: f32) {
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
        paper::fill(painter, &star, Color32::from_rgb(0xff, 0xd8, 0x60));
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
    }

    #[test]
    fn crowns_are_left_to_kings() {
        assert!((0..500).all(|s| Look::from_seed(s).hat != Hat::Crown));
    }

    #[test]
    fn standing_still_never_moves() {
        let a = pose(Action::Still, 0.0);
        assert!((0..50).all(|i| pose(Action::Still, i as f64 * 0.37) == a));
    }

    #[test]
    fn mining_strikes_once_per_swing() {
        let strikes = (0..950)
            .filter(|ms| {
                let t = *ms as f64 / 1000.0;
                pose(Action::Mine, t).since_strike.is_some()
                    && pose(Action::Mine, t - 0.001).since_strike.is_none()
            })
            .count();
        assert_eq!(strikes, 1);
    }
}

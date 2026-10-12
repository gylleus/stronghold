//! The pieces a dwarf is built from, in his own units (about 100 tall, facing right, feet on
//! the origin): boots, body, arms with the tool, head with hair, beard, face and hat, and the
//! Ravenmaster's raven. Each is cut in the look's colours; [`super::draw`] places them.

use eframe::egui::{Color32, Pos2, pos2, vec2};

use super::cutout::{Piece, Xf, blob, capsule, ellipse, place};
use super::pose::{Eyes, Pose};
use super::{
    BACK_SHOULDER, Beard, Brows, GOLD, Garment, Hair, Hat, INK, IRON, LEATHER, Look, Nose, RUBY,
    Role, SHOULDER, Tool, WOOD, shade, tint,
};

const BRASS: Color32 = Color32::from_rgb(0xc8, 0x96, 0x3c);
const FLAME: Color32 = Color32::from_rgb(0xff, 0xb0, 0x30);
const FLAME_CORE: Color32 = Color32::from_rgb(0xff, 0xf0, 0xb0);
const BANDANA: Color32 = Color32::from_rgb(0xa8, 0x34, 0x2a);
const RAVEN: Color32 = Color32::from_rgb(0x22, 0x22, 0x2e);
/// Where the fist is, in arm coordinates (the arm hangs straight down from the shoulder).
const HAND: Pos2 = pos2(SHOULDER.x, -25.5);

pub(super) fn boot(x: f32, look: &Look, back: bool) -> Piece {
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

pub(super) fn body(look: &Look) -> Piece {
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
    match look.garment {
        Garment::Tunic => {}
        Garment::Apron => {
            let apron = Color32::from_rgb(0x7a, 0x52, 0x34);
            p.paint(
                blob(
                    &[
                        pos2(3.0, -44.0),
                        pos2(12.0, -45.5),
                        pos2(19.5 * g, -33.0),
                        pos2(19.0 * g, -18.0),
                        pos2(14.0, -12.5),
                        pos2(4.0, -13.0),
                        pos2(2.0, -30.0),
                    ],
                    5,
                ),
                apron,
            );
            p.paint(
                capsule(pos2(3.0, -44.0), pos2(-7.0, -50.0), 1.2),
                shade(apron, 0.25),
            );
        }
        Garment::Vest => {
            let vest = shade(LEATHER, -0.25);
            p.paint(
                blob(
                    &[
                        pos2(-17.5 * g, -22.0),
                        pos2(-19.5 * g, -31.0),
                        pos2(-14.0, -45.0),
                        pos2(-2.0, -50.5),
                        pos2(8.0, -47.0),
                        pos2(6.0, -22.0),
                    ],
                    5,
                ),
                vest,
            );
            for y in [-40.0, -33.0] {
                p.paint(ellipse(pos2(5.5, y), 1.2, 1.2, 8), BRASS);
            }
        }
    }
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
    if look.role == Role::Foreman {
        // The foreman's brass badge.
        p.paint(ellipse(pos2(10.0, -39.0), 3.2, 3.2, 12), BRASS);
        p.paint(ellipse(pos2(10.0, -39.0), 1.6, 1.6, 8), shade(BRASS, 0.35));
    }
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

pub(super) fn back_arm(look: &Look) -> Piece {
    let mut p = Piece::default();
    let sleeve = shade(look.tunic, 0.3);
    p.cut(capsule(BACK_SHOULDER, pos2(-10.0, -27.0), 5.2), sleeve);
    p.cut(
        ellipse(pos2(-10.0, -25.5), 5.0, 5.0, 14),
        shade(look.skin, 0.2),
    );
    p
}

/// The front arm with the tool in its fist, turned by the pose's grip; empty-handed while
/// he lugs a nugget or lies down.
pub(super) fn arm(look: &Look, pose: &Pose) -> Piece {
    let mut p = Piece::default();
    // The tool goes under the fist, so it is cut first.
    if !pose.nugget && !pose.stowed {
        let start = p.polys.len();
        tool(&mut p, look.tool, look.role == Role::Apprentice);
        let grip = Xf::rotate_about(HAND, -pose.grip);
        for poly in &mut p.polys[start..] {
            poly.points = place(std::mem::take(&mut poly.points), grip);
        }
    }
    p.cut(capsule(SHOULDER, pos2(SHOULDER.x, -28.0), 5.6), look.tunic);
    p.paint(
        capsule(pos2(SHOULDER.x, -31.5), pos2(SHOULDER.x, -29.0), 5.0),
        shade(look.tunic, 0.25),
    );
    p.cut(ellipse(HAND, 5.6, 5.4, 16), look.skin);
    p.paint(
        ellipse(HAND + vec2(2.0, 1.0), 2.4, 2.0, 10),
        shade(look.skin, 0.15),
    );
    p
}

/// A big gold nugget hugged against the belly.
pub(super) fn nugget() -> Piece {
    let mut p = Piece::default();
    let at = pos2(24.0, -41.0);
    let s = 1.6;
    let pt = |x: f32, y: f32| at + vec2(x, y) * s;
    p.cut(
        blob(
            &[
                pt(-7.0, 1.0),
                pt(-4.0, -5.0),
                pt(3.0, -6.0),
                pt(8.0, -1.0),
                pt(5.0, 5.0),
                pt(-3.0, 5.5),
            ],
            5,
        ),
        GOLD,
    );
    p.paint(
        ellipse(pt(-2.0, -2.5), 2.2 * s, 1.4 * s, 8),
        tint(GOLD, 0.6),
    );
    p.paint(
        ellipse(pt(3.5, 2.5), 2.0 * s, 1.4 * s, 8),
        shade(GOLD, 0.25),
    );
    p
}

/// Where the tool's working end is, in arm coordinates (before the grip turns it).
pub(super) fn tool_tip(tool: Tool) -> Pos2 {
    let y = match tool {
        Tool::Pickaxe => 2.0,
        Tool::Hammer => -6.0,
        Tool::Shovel => 4.0,
        Tool::Lantern => -4.0,
        Tool::Trumpet => -10.0,
        Tool::Scepter => -2.0,
    };
    pos2(SHOULDER.x, y)
}

/// The tool held in the fist at [`HAND`], its shaft running on past the hand along the arm.
fn tool(p: &mut Piece, tool: Tool, small: bool) {
    let hand = HAND;
    let end = tool_tip(tool);
    let butt = hand - vec2(0.0, 6.0);
    let x = hand.x;
    match tool {
        Tool::Pickaxe => {
            p.cut(capsule(butt, pos2(x, end.y), 1.9), WOOD);
            p.cut(
                blob(
                    &[
                        // The points curve back towards the hands.
                        pos2(x - 13.0, end.y - 6.0),
                        pos2(x - 6.0, end.y + 1.0),
                        pos2(x + 6.0, end.y + 1.0),
                        pos2(x + 15.0, end.y - 7.0),
                        pos2(x + 5.0, end.y - 2.5),
                        pos2(x - 5.0, end.y - 2.5),
                    ],
                    6,
                ),
                IRON,
            );
        }
        Tool::Hammer => {
            p.cut(capsule(butt, pos2(x, end.y), 2.0), WOOD);
            p.cut(rect(x - 9.0, end.y - 4.0, x + 9.0, end.y + 5.0), IRON);
            p.paint(
                rect(x - 9.0, end.y + 2.0, x + 9.0, end.y + 5.0),
                shade(IRON, 0.3),
            );
        }
        Tool::Shovel => {
            p.cut(capsule(butt, pos2(x, end.y - 6.0), 1.9), WOOD);
            p.cut(
                blob(
                    &[
                        pos2(x - 6.0, end.y - 7.0),
                        pos2(x + 6.0, end.y - 7.0),
                        pos2(x + 5.5, end.y + 2.0),
                        pos2(x, end.y + 7.0),
                        pos2(x - 5.5, end.y + 2.0),
                    ],
                    6,
                ),
                IRON,
            );
        }
        Tool::Lantern => {
            // A lantern hanging from a pole (a short handle for an apprentice).
            let top = if small { hand.y + 5.0 } else { end.y };
            p.cut(capsule(butt, pos2(x, top), 1.6), WOOD);
            let c = pos2(x, top + 7.0);
            p.cut(capsule(pos2(x, top), c - vec2(0.0, 5.0), 0.8), IRON);
            p.cut(
                rect(c.x - 4.5, c.y - 5.5, c.x + 4.5, c.y + 5.5),
                shade(IRON, 0.35),
            );
            p.paint(rect(c.x - 3.0, c.y - 3.5, c.x + 3.0, c.y + 3.5), FLAME);
            p.paint(ellipse(c, 1.4, 2.2, 8), FLAME_CORE);
            p.cut(
                vec![
                    pos2(c.x - 5.5, c.y - 5.5),
                    pos2(c.x + 5.5, c.y - 5.5),
                    pos2(c.x, c.y - 9.0),
                ],
                shade(IRON, 0.2),
            );
        }
        Tool::Trumpet => {
            // A brass ear trumpet, its bell beyond the fist.
            p.cut(capsule(butt, pos2(x, end.y - 6.0), 1.6), BRASS);
            p.cut(
                blob(
                    &[
                        pos2(x - 1.8, end.y - 8.0),
                        pos2(x + 1.8, end.y - 8.0),
                        pos2(x + 8.0, end.y + 2.0),
                        pos2(x + 9.0, end.y + 6.0),
                        pos2(x - 9.0, end.y + 6.0),
                        pos2(x - 8.0, end.y + 2.0),
                    ],
                    5,
                ),
                BRASS,
            );
            p.paint(
                ellipse(pos2(x, end.y + 5.0), 7.0, 1.6, 12),
                shade(BRASS, 0.5),
            );
        }
        Tool::Scepter => {
            p.cut(capsule(butt, pos2(x, end.y), 1.8), GOLD);
            p.cut(ellipse(pos2(x, end.y + 2.0), 5.2, 5.2, 16), GOLD);
            p.paint(ellipse(pos2(x, end.y + 2.0), 3.6, 3.6, 14), RUBY);
            p.paint(
                ellipse(pos2(x - 1.2, end.y + 0.8), 1.2, 1.2, 8),
                tint(RUBY, 0.6),
            );
        }
    }
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Pos2> {
    vec![pos2(x0, y0), pos2(x1, y0), pos2(x1, y1), pos2(x0, y1)]
}

/// The Ravenmaster's raven, perched on his back shoulder.
pub(super) fn raven() -> Piece {
    let mut p = Piece::default();
    p.cut(
        blob(
            &[
                pos2(-22.0, -50.0),
                pos2(-18.0, -57.0),
                pos2(-10.0, -60.0),
                pos2(-4.0, -56.0),
                pos2(-8.0, -50.0),
                pos2(-15.0, -49.0),
            ],
            5,
        ),
        RAVEN,
    );
    p.cut(ellipse(pos2(-6.0, -63.0), 4.6, 4.4, 14), RAVEN);
    p.cut(
        vec![pos2(-2.5, -64.5), pos2(4.0, -62.5), pos2(-2.0, -61.5)],
        Color32::from_rgb(0x5a, 0x56, 0x52),
    );
    p.paint(
        ellipse(pos2(-5.0, -64.2), 1.1, 1.1, 8),
        Color32::from_rgb(0xe8, 0xe0, 0xc0),
    );
    p.paint(
        blob(
            &[
                pos2(-17.0, -55.0),
                pos2(-10.0, -57.0),
                pos2(-8.0, -52.0),
                pos2(-15.0, -51.0),
            ],
            4,
        ),
        tint(RAVEN, 0.15),
    );
    p
}

pub(super) fn head(look: &Look, pose: &Pose) -> Piece {
    let mut p = Piece::default();
    let skin = look.skin;
    let hooded = look.hat == Hat::Hood && !pose.nightcap;
    if look.hat != Hat::Bald || pose.nightcap {
        hair(&mut p, look);
    }
    p.cut(ellipse(pos2(5.0, -68.0), 16.5, 15.5, 28), skin);
    // Ear, and an earring.
    p.paint(ellipse(pos2(-2.5, -66.0), 3.6, 4.6, 12), shade(skin, 0.18));
    if look.extras.earring && !hooded {
        p.cut(ellipse(pos2(-2.5, -60.0), 1.6, 1.6, 8), GOLD);
    }
    if look.hat == Hat::Bald && !pose.nightcap {
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
            look.hair,
        );
        p.paint(ellipse(pos2(6.0, -80.0), 5.0, 2.0, 10), tint(skin, 0.35));
    }
    face(&mut p, look, pose);
    beard(&mut p, look);
    nose(&mut p, look);
    if look.extras.pipe {
        let pipe = shade(WOOD, 0.35);
        p.cut(capsule(pos2(22.0, -54.0), pos2(31.0, -51.5), 1.1), pipe);
        p.cut(rect(29.5, -56.5, 34.0, -50.0), pipe);
        p.paint(rect(30.3, -56.5, 33.2, -55.2), FLAME);
    }
    if pose.nightcap {
        nightcap(&mut p, look);
    } else {
        let start = p.polys.len();
        hat(&mut p, look);
        if look.role == Role::Apprentice {
            // A hat a size too big, down over his brow.
            let big = Xf::translate(vec2(0.0, 3.0)).then(scale_about(pos2(4.0, -73.0), 1.15));
            for poly in &mut p.polys[start..] {
                poly.points = place(std::mem::take(&mut poly.points), big);
            }
        }
    }
    p
}

fn scale_about(c: Pos2, k: f32) -> Xf {
    Xf::translate(c.to_vec2())
        .then(Xf::scale(k, k))
        .then(Xf::translate(-c.to_vec2()))
}

fn hair(p: &mut Piece, look: &Look) {
    let c = look.hair;
    let back = [
        pos2(-13.0, -76.0),
        pos2(-4.0, -82.0),
        pos2(0.0, -70.0),
        pos2(-4.0, -58.0),
        pos2(-12.0, -60.0),
    ];
    match look.hair_style {
        Hair::Short => {
            p.cut(blob(&back, 6), c);
        }
        Hair::Long => {
            p.cut(
                blob(
                    &[
                        pos2(-13.0, -78.0),
                        pos2(-4.0, -83.0),
                        pos2(0.0, -70.0),
                        pos2(-6.0, -52.0),
                        pos2(-14.0, -44.0),
                        pos2(-19.0, -56.0),
                    ],
                    6,
                ),
                c,
            );
        }
        Hair::Ponytail => {
            p.cut(blob(&back, 6), c);
            p.cut(capsule(pos2(-13.0, -70.0), pos2(-22.0, -54.0), 3.6), c);
            p.paint(
                capsule(pos2(-14.0, -68.5), pos2(-15.0, -66.5), 3.4),
                LEATHER,
            );
        }
        Hair::Braid => {
            p.cut(blob(&back, 6), c);
            for (i, y) in [-60.0, -53.0, -46.0].into_iter().enumerate() {
                p.cut(ellipse(pos2(-13.0 - i as f32, y), 3.4, 3.8, 10), c);
            }
            p.paint(rect(-17.5, -43.5, -12.0, -41.0), GOLD);
        }
    }
}

fn face(p: &mut Piece, look: &Look, pose: &Pose) {
    let brow_color = shade(look.hair, 0.2);
    let eye = pos2(14.5, -70.0);
    match pose.eyes {
        Eyes::Open => {
            let squint = if pose.brows < 0.0 { 1.0 } else { 2.4 };
            p.paint(ellipse(eye, 1.9, squint, 10), INK);
        }
        Eyes::Shut => {
            p.paint(
                blob(
                    &[
                        eye + vec2(-2.4, -0.4),
                        eye + vec2(0.0, 1.2),
                        eye + vec2(2.4, -0.4),
                        eye + vec2(0.0, 0.4),
                    ],
                    3,
                ),
                INK,
            );
        }
        Eyes::Crossed => {
            for d in [vec2(1.0, 1.0), vec2(1.0, -1.0)] {
                p.paint(capsule(eye - d * 2.0, eye + d * 2.0, 0.7), INK);
            }
        }
    }
    if look.extras.scar {
        p.paint(
            capsule(pos2(10.0, -75.5), pos2(18.0, -63.5), 0.7),
            tint(look.skin, 0.45),
        );
    }
    if look.extras.eyepatch {
        p.paint(capsule(pos2(-8.0, -77.0), pos2(19.0, -72.0), 0.8), INK);
        p.paint(ellipse(eye, 3.6, 3.4, 12), INK);
    }
    let lift = pose.brows * 2.2;
    let tilt = -pose.brows * 1.4;
    let (thick, spikes) = match look.brows {
        Brows::Bushy => (3.3, false),
        Brows::Thin => (1.6, false),
        Brows::Wild => (3.0, true),
    };
    let top = |x: f32, t: f32| pos2(x, -72.0 - thick - lift + t);
    p.paint(
        vec![
            top(8.0, tilt),
            top(20.0, -tilt - 1.5),
            pos2(20.5, -73.2 - lift - tilt),
            pos2(8.5, -72.0 - lift + tilt),
        ],
        brow_color,
    );
    if spikes {
        for x in [11.0, 16.0] {
            let base = -75.0 - lift;
            p.paint(
                vec![
                    pos2(x - 2.0, base),
                    pos2(x + 2.0, base),
                    pos2(x + 3.5, base - 4.0),
                ],
                brow_color,
            );
        }
    }
}

fn nose(p: &mut Piece, look: &Look) {
    let n = look.nose_size;
    let color = Color32::from_rgb(0xd0, 0x70, 0x60).lerp_to_gamma(look.skin, 0.45);
    match look.nose {
        Nose::Round => {
            p.cut(
                ellipse(pos2(21.0 + 2.0 * n, -64.5), 6.5 * n, 6.0 * n, 20),
                color,
            );
        }
        Nose::Long => {
            p.cut(
                blob(
                    &[
                        pos2(16.0, -69.0),
                        pos2(24.0 + 6.0 * n, -66.0),
                        pos2(27.0 + 8.0 * n, -60.5),
                        pos2(22.0 + 4.0 * n, -58.5),
                        pos2(16.0, -61.0),
                    ],
                    5,
                ),
                color,
            );
        }
        Nose::Hooked => {
            p.cut(
                blob(
                    &[
                        pos2(15.0, -71.0),
                        pos2(22.0 + 3.0 * n, -70.0),
                        pos2(27.0 + 5.0 * n, -63.0),
                        pos2(24.0 + 4.0 * n, -57.5),
                        pos2(21.0 + 2.0 * n, -60.0),
                        pos2(16.0, -61.0),
                    ],
                    5,
                ),
                color,
            );
        }
    }
    p.paint(
        ellipse(pos2(19.0 + 2.0 * n, -66.5), 2.0 * n, 1.6 * n, 8),
        tint(look.skin, 0.35),
    );
}

fn beard(p: &mut Piece, look: &Look) {
    let c = look.hair;
    let full: Option<Vec<Pos2>> = match look.beard {
        Beard::Long => Some(vec![
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
        ]),
        Beard::Braided => Some(vec![
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
        ]),
        Beard::Forked => Some(vec![
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
        ]),
        Beard::Bushy => Some(vec![
            pos2(-3.0, -66.0),
            pos2(8.0, -60.0),
            pos2(20.0, -60.0),
            pos2(29.0, -54.0),
            pos2(29.0, -42.0),
            pos2(20.0, -34.0),
            pos2(8.0, -35.0),
            pos2(-2.0, -42.0),
            pos2(-6.0, -54.0),
        ]),
        Beard::Short => Some(vec![
            pos2(-1.0, -64.0),
            pos2(8.0, -60.0),
            pos2(18.0, -59.0),
            pos2(25.0, -56.0),
            pos2(23.0, -47.0),
            pos2(14.0, -44.0),
            pos2(4.0, -47.0),
            pos2(-2.0, -54.0),
        ]),
        Beard::Chops => Some(vec![
            pos2(-2.0, -68.0),
            pos2(3.0, -64.0),
            pos2(10.0, -57.0),
            pos2(9.0, -50.0),
            pos2(1.0, -51.0),
            pos2(-4.0, -58.0),
        ]),
        Beard::Goatee => {
            p.cut(
                blob(
                    &[
                        pos2(14.0, -57.0),
                        pos2(23.0, -56.0),
                        pos2(21.0, -48.0),
                        pos2(17.0, -42.0),
                        pos2(15.0, -50.0),
                    ],
                    5,
                ),
                c,
            );
            None
        }
        Beard::Stubble => {
            p.paint(
                blob(
                    &[
                        pos2(0.0, -62.0),
                        pos2(10.0, -58.5),
                        pos2(21.0, -57.5),
                        pos2(20.0, -53.0),
                        pos2(10.0, -52.5),
                        pos2(2.0, -55.5),
                    ],
                    5,
                ),
                look.skin.lerp_to_gamma(c, 0.45),
            );
            None
        }
    };
    if let Some(outline) = full {
        p.cut(blob(&outline, 6), c);
        if !matches!(look.beard, Beard::Chops) {
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
        }
    }
    if look.beard == Beard::Braided {
        for y in [-30.0, -24.0] {
            p.paint(rect(10.5, y - 1.6, 16.5, y + 1.6), GOLD);
        }
    }
    if look.beard != Beard::Stubble {
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
}

fn helmet_dome() -> Vec<Pos2> {
    (0..=16)
        .map(|i| {
            let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
            pos2(4.0 + 18.0 * a.cos(), -73.0 + 18.0 * a.sin())
        })
        .collect()
}

fn hat(p: &mut Piece, look: &Look) {
    match look.hat {
        Hat::Helmet | Hat::CandleHelmet | Hat::HornedHelmet => {
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
                    FLAME,
                );
                p.paint(ellipse(pos2(4.0, -99.5), 1.0, 1.8, 8), FLAME_CORE);
            }
            if look.hat == Hat::HornedHelmet {
                let horn = Color32::from_rgb(0xe8, 0xdc, 0xc0);
                p.cut(
                    blob(
                        &[
                            pos2(12.0, -84.0),
                            pos2(20.0, -88.0),
                            pos2(25.0, -100.0),
                            pos2(19.0, -94.0),
                            pos2(11.0, -89.0),
                        ],
                        5,
                    ),
                    horn,
                );
                p.cut(
                    blob(
                        &[
                            pos2(-6.0, -86.0),
                            pos2(-13.0, -92.0),
                            pos2(-15.0, -103.0),
                            pos2(-10.0, -96.0),
                            pos2(-3.0, -90.0),
                        ],
                        5,
                    ),
                    shade(horn, 0.2),
                );
            }
            p.cut(helmet_dome(), IRON);
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
        Hat::Hood => {
            let cloth = if look.role == Role::Ravenmaster {
                Color32::from_rgb(0x2c, 0x28, 0x34)
            } else {
                shade(look.tunic, 0.3)
            };
            p.cut(
                blob(
                    &[
                        pos2(22.0, -73.0),
                        pos2(15.0, -84.0),
                        pos2(2.0, -89.0),
                        pos2(-12.0, -88.0),
                        pos2(-24.0, -95.0),
                        pos2(-19.0, -82.0),
                        pos2(-19.0, -66.0),
                        pos2(-14.0, -54.0),
                        pos2(-5.0, -55.0),
                        pos2(-3.0, -66.0),
                        pos2(6.0, -74.0),
                    ],
                    5,
                ),
                cloth,
            );
            p.paint(
                capsule(pos2(4.0, -75.0), pos2(21.0, -74.0), 1.3),
                shade(cloth, 0.35),
            );
            if look.role == Role::Ravenmaster {
                // Feathers down the back of the hood.
                for (x, y) in [(-17.0, -80.0), (-16.0, -71.0), (-13.0, -62.0)] {
                    p.paint(
                        vec![pos2(x, y), pos2(x + 5.0, y - 2.0), pos2(x - 3.0, y + 6.0)],
                        tint(cloth, 0.2),
                    );
                }
            }
        }
        Hat::Bandana => {
            p.cut(
                blob(
                    &[
                        pos2(-13.0, -73.0),
                        pos2(-9.0, -84.0),
                        pos2(4.0, -87.0),
                        pos2(17.0, -82.0),
                        pos2(21.0, -75.0),
                        pos2(4.0, -76.0),
                    ],
                    6,
                ),
                BANDANA,
            );
            p.cut(
                blob(
                    &[
                        pos2(-12.0, -76.0),
                        pos2(-21.0, -70.0),
                        pos2(-20.0, -64.0),
                        pos2(-15.0, -70.0),
                    ],
                    4,
                ),
                shade(BANDANA, 0.2),
            );
            for (x, y) in [(-1.0, -82.0), (9.0, -80.0), (-7.0, -78.0)] {
                p.paint(ellipse(pos2(x, y), 1.0, 1.0, 6), tint(BANDANA, 0.6));
            }
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

/// A floppy striped nightcap with a pompom, for sleepers.
fn nightcap(p: &mut Piece, look: &Look) {
    let cloth = tint(look.tunic, 0.25);
    p.cut(
        blob(
            &[
                pos2(-13.0, -73.0),
                pos2(-6.0, -86.0),
                pos2(6.0, -88.0),
                pos2(16.0, -82.0),
                pos2(20.0, -74.0),
                pos2(4.0, -76.0),
                pos2(-16.0, -84.0),
                pos2(-26.0, -78.0),
                pos2(-20.0, -86.0),
            ],
            5,
        ),
        cloth,
    );
    for (a, b) in [((-2.0, -86.5), (-4.0, -77.0)), ((8.0, -86.0), (9.0, -76.0))] {
        p.paint(
            capsule(pos2(a.0, a.1), pos2(b.0, b.1), 1.6),
            shade(cloth, 0.3),
        );
    }
    p.cut(
        ellipse(pos2(-25.0, -78.0), 3.6, 3.6, 12),
        Color32::from_rgb(0xf0, 0xec, 0xe0),
    );
}

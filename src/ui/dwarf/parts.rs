//! The pieces a dwarf is built from, in his own units (about 100 tall, facing right, feet on
//! the origin): boots, body, arms with the tool, head with hair, beard, face and hat, and the
//! rune priest's staff. Each is cut in the look's colours; [`super::draw`] places them.

use std::f32::consts::TAU;

use eframe::egui::{Color32, Pos2, pos2, vec2};

use super::cutout::{Piece, Xf, blob, capsule, ellipse, place};
use super::pose::{Eyes, Pose};
use super::{
    BACK_SHOULDER, Beard, Brows, Crest, GOLD, Garment, Hair, Hat, INK, IRON, LEATHER, Look, Nose,
    RUBY, Role, SHOULDER, Tool, WOOD, shade, tint,
};

const HAIRS_FUR: Color32 = Color32::from_rgb(0xd2, 0xca, 0xb6);
const PARCHMENT: Color32 = Color32::from_rgb(0xd8, 0xc8, 0x9e);
const BRASS: Color32 = Color32::from_rgb(0xc8, 0x96, 0x3c);
const FLAME: Color32 = Color32::from_rgb(0xff, 0xb0, 0x30);
const FLAME_CORE: Color32 = Color32::from_rgb(0xff, 0xf0, 0xb0);
const BANDANA: Color32 = Color32::from_rgb(0xa8, 0x34, 0x2a);
const CRYSTAL: Color32 = Color32::from_rgb(0x69, 0xae, 0xc4);
/// The runes circling the crystal of a casting priest, on the near side of it.
const CRYSTAL_LIGHT: Color32 = Color32::from_rgb(0xd4, 0xf4, 0xf8);
/// Where a casting priest grips his staff.
const STAFF_GRIP: Pos2 = pos2(29.0, -30.0);
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
    p.paint(rect(x - 5.0, -9.0, x + 3.0, -6.5), tint(leather, 0.35));
    p.paint(rect(x - 6.0, -2.5, x + 9.0, -0.5), shade(leather, 0.35));
    p.paint(rect(x - 2.0, -8.5, x + 1.0, -6.0), BRASS);
    p
}

pub(super) fn body(look: &Look) -> Piece {
    let g = look.girth;
    let mut p = Piece::default();
    if look.role == Role::King {
        // A heavy mantle, kept behind the arms and belly rather than enlarging the King.
        p.cut(
            blob(
                &[
                    pos2(-15.0, -48.0),
                    pos2(-23.0, -39.0),
                    pos2(-27.0, -8.0),
                    pos2(-9.0, -6.0),
                    pos2(9.0, -12.0),
                    pos2(7.0, -45.0),
                ],
                4,
            ),
            shade(look.tunic, 0.3),
        );
        p.paint(capsule(pos2(-22.0, -36.0), pos2(-25.0, -9.0), 1.7), GOLD);
        p.paint(capsule(pos2(-24.0, -8.0), pos2(-10.0, -8.0), 1.7), GOLD);
    }
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
        Garment::Robe => {
            // A continuous skirt down to the boots, broad enough to read at farm size.
            p.cut(
                blob(
                    &[
                        pos2(-16.0, -46.0),
                        pos2(12.0, -48.0),
                        pos2(18.0, -31.0),
                        pos2(23.0, -5.0),
                        pos2(0.0, -3.0),
                        pos2(-22.0, -6.0),
                        pos2(-18.0, -31.0),
                    ],
                    6,
                ),
                look.tunic,
            );
            p.paint(
                vec![
                    pos2(-16.0, -37.0),
                    pos2(-6.0, -32.0),
                    pos2(-8.0, -6.0),
                    pos2(-19.0, -7.0),
                ],
                shade(look.tunic, 0.3),
            );
            p.paint(
                capsule(pos2(10.0, -33.0), pos2(16.0, -8.0), 1.8),
                tint(look.tunic, 0.2),
            );
            if look.role == Role::Librarian {
                p.paint(rect(-12.0, -45.0, -6.0, -16.0), PARCHMENT);
                p.paint(capsule(pos2(-18.0, -8.0), pos2(19.0, -8.0), 1.2), PARCHMENT);
                return p;
            }
            p.paint(capsule(pos2(-18.0, -8.0), pos2(19.0, -8.0), 1.2), BRASS);
            for x in [-13.0, -2.0, 9.0] {
                rune(&mut p, pos2(x, -16.0), BRASS);
            }
            // Woven shoulder stole and belt scroll-case sit either side of the long beard.
            p.paint(
                vec![
                    pos2(-15.0, -45.0),
                    pos2(-8.0, -47.0),
                    pos2(-10.0, -20.0),
                    pos2(-17.0, -18.0),
                ],
                shade(look.tunic, 0.4),
            );
            p.paint(capsule(pos2(-15.0, -43.0), pos2(-17.0, -20.0), 1.0), BRASS);
            for y in [-40.0, -29.0] {
                rune(&mut p, pos2(-13.0, y), CRYSTAL);
            }
            p.cut(rect(-22.0, -26.0, -16.0, -15.0), LEATHER);
            p.paint(rect(-22.0, -25.0, -16.0, -22.0), BRASS);
            return p;
        }
        Garment::Tunic => {
            // Woven side seam and a darned patch, visible beside the belt and beard.
            p.paint(
                capsule(pos2(-15.0, -41.0), pos2(-16.0, -18.0), 1.0),
                tint(look.tunic, 0.3),
            );
            if look.role != Role::King {
                p.paint(rect(-12.0, -22.0, -3.0, -15.0), shade(look.tunic, 0.3));
                for x in [-10.0, -6.0] {
                    p.paint(
                        capsule(pos2(x, -23.0), pos2(x + 1.0, -20.0), 0.7),
                        tint(look.tunic, 0.45),
                    );
                }
            }
        }
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
            p.paint(rect(5.0, -21.0, 16.0, -15.0), shade(apron, 0.3));
            p.paint(
                capsule(pos2(5.0, -21.0), pos2(16.0, -21.0), 1.0),
                tint(apron, 0.3),
            );
            for x in [6.0, 14.0] {
                p.paint(rect(x, -19.0, x + 1.5, -17.0), BRASS);
            }
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
            p.paint(
                capsule(pos2(-12.0, -44.0), pos2(-7.0, -31.0), 1.5),
                tint(vest, 0.25),
            );
            p.paint(capsule(pos2(-15.0, -30.0), pos2(-6.0, -30.0), 1.0), BRASS);
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
    for x in [-14.0, -8.0, -2.0] {
        p.paint(rect(x, -25.0, x + 1.5, -23.5), BRASS);
    }
    if look.role != Role::King {
        p.cut(
            blob(
                &[
                    pos2(-22.0, -25.0),
                    pos2(-14.0, -25.0),
                    pos2(-13.0, -16.0),
                    pos2(-22.0, -15.0),
                ],
                3,
            ),
            LEATHER,
        );
        p.paint(rect(-22.0, -24.0, -14.0, -21.0), tint(LEATHER, 0.25));
        p.paint(rect(-19.0, -22.0, -17.0, -19.0), BRASS);
    }
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
    if look.role == Role::King {
        // Fur collar, gold embroidery and a ruby clasp, away from the face silhouette.
        p.cut(
            blob(
                &[
                    pos2(-18.0, -43.0),
                    pos2(-13.0, -50.0),
                    pos2(-4.0, -49.0),
                    pos2(4.0, -45.0),
                    pos2(15.0, -46.0),
                    pos2(19.0, -41.0),
                    pos2(8.0, -38.0),
                    pos2(-8.0, -40.0),
                ],
                3,
            ),
            HAIRS_FUR,
        );
        for x in [-13.0, -6.0, 1.0] {
            p.paint(
                capsule(pos2(x, -45.0), pos2(x + 1.0, -42.0), 0.8),
                shade(HAIRS_FUR, 0.35),
            );
        }
        p.paint(capsule(pos2(-18.0, -16.0), pos2(18.0, -16.0), 1.8), GOLD);
        for x in [-12.0, -2.0, 8.0] {
            p.paint(
                vec![
                    pos2(x, -21.0),
                    pos2(x + 3.0, -18.0),
                    pos2(x, -15.0),
                    pos2(x - 3.0, -18.0),
                ],
                GOLD,
            );
        }
        p.cut(ellipse(pos2(-12.0, -40.0), 3.0, 3.5, 10), GOLD);
        p.paint(ellipse(pos2(-12.0, -40.0), 1.5, 2.0, 8), RUBY);
    }
    p
}

fn cuff(look: &Look) -> Color32 {
    if matches!(look.role, Role::King | Role::RunePriest) {
        GOLD
    } else {
        LEATHER
    }
}

pub(super) fn back_arm(look: &Look) -> Piece {
    let mut p = Piece::default();
    let sleeve = shade(look.tunic, 0.3);
    p.cut(capsule(BACK_SHOULDER, pos2(-10.0, -27.0), 5.2), sleeve);
    p.paint(
        capsule(pos2(-14.0, -31.0), pos2(-6.0, -31.0), 1.5),
        cuff(look),
    );
    p.cut(
        ellipse(pos2(-10.0, -25.5), 5.0, 5.0, 14),
        shade(look.skin, 0.2),
    );
    p
}

/// The far hand grips the same large prop as the front hand.
pub(super) fn support_arm(look: &Look, hand: Pos2) -> Piece {
    let mut p = Piece::default();
    let elbow = BACK_SHOULDER.lerp(hand, 0.5) + vec2(-3.0, 6.0);
    p.cut(capsule(BACK_SHOULDER, elbow, 5.2), shade(look.tunic, 0.3));
    p.cut(capsule(elbow, hand, 4.5), look.tunic);
    p.cut(ellipse(hand, 5.0, 5.0, 14), shade(look.skin, 0.15));
    p
}

/// The front arm with the tool in its fist, turned by the pose's grip; empty-handed while
/// he lugs a nugget or lies down.
pub(super) fn arm(look: &Look, pose: &Pose) -> Piece {
    let mut p = Piece::default();
    // The tool goes under the fist, so it is cut first.
    if !pose.nugget && !pose.stowed && look.role != Role::RunePriest {
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
        cuff(look),
    );
    p.paint(
        capsule(
            pos2(SHOULDER.x - 3.0, -42.0),
            pos2(SHOULDER.x - 3.0, -35.0),
            1.1,
        ),
        tint(look.tunic, 0.3),
    );
    if matches!(look.role, Role::King | Role::RunePriest) {
        p.paint(
            rect(SHOULDER.x - 2.0, -33.0, SHOULDER.x, -28.0),
            shade(GOLD, 0.35),
        );
    }
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
        Tool::Staff => -10.0,
        Tool::Book | Tool::None => -25.5,
    };
    pos2(SHOULDER.x, y)
}

/// The tool held in the fist at [`HAND`], its shaft running on past the hand along the arm.
pub(super) fn tool(p: &mut Piece, tool: Tool, small: bool) {
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
        Tool::Staff => {
            p.cut(capsule(pos2(x, -82.0), pos2(x, 0.0), 1.8), WOOD);
            for y in [-65.0, -51.0, -37.0] {
                rune(p, pos2(x - 1.0, y), BRASS);
            }
            crystal(p, pos2(x, -85.0), 0.0);
        }
        Tool::Book => {
            let y = hand.y;
            p.cut(rect(x - 5.0, y - 20.0, x + 25.0, y + 1.0), LEATHER);
            p.paint(rect(x - 3.0, y - 18.0, x + 23.0, y - 1.0), PARCHMENT);
            p.paint(
                rect(x + 9.0, y - 18.0, x + 11.0, y - 1.0),
                shade(PARCHMENT, 0.4),
            );
            for line in [-14.0, -9.0, -4.0] {
                p.paint(
                    rect(x - 1.0, y + line, x + 6.0, y + line + 1.0),
                    shade(PARCHMENT, 0.45),
                );
                p.paint(
                    rect(x + 14.0, y + line, x + 21.0, y + line + 1.0),
                    shade(PARCHMENT, 0.45),
                );
            }
        }
        Tool::None => {}
    }
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Pos2> {
    vec![pos2(x0, y0), pos2(x1, y0), pos2(x1, y1), pos2(x0, y1)]
}

fn rune(p: &mut Piece, at: Pos2, color: Color32) {
    p.paint(capsule(at, at + vec2(0.0, 5.0), 0.8), color);
    p.paint(capsule(at, at + vec2(3.0, 2.0), 0.8), color);
    p.paint(
        capsule(at + vec2(3.0, 2.0), at + vec2(0.0, 3.0), 0.8),
        color,
    );
}
fn crystal(p: &mut Piece, at: Pos2, pulse: f32) {
    p.cut(
        vec![
            at + vec2(0.0, -8.0),
            at + vec2(5.0, -2.0),
            at + vec2(4.0, 5.0),
            at + vec2(0.0, 8.0),
            at + vec2(-4.0, 4.0),
            at + vec2(-5.0, -2.0),
        ],
        tint(CRYSTAL, pulse * 0.4),
    );
    p.paint(
        vec![
            at + vec2(0.0, -7.0),
            at + vec2(-4.0, -1.0),
            at + vec2(0.0, 6.0),
        ],
        tint(CRYSTAL, 0.65),
    );
}
/// A planted staff leaves both hands free over the pool. No animation at rest. Casting, he
/// thrusts it up in his fist, and the runes circling its crystal pass behind his hat.
pub(super) fn priest_staff(pose: &Pose) -> Piece {
    let mut p = Piece::default();
    p.cut(capsule(pos2(29.0, -76.0), pos2(29.0, -2.0), 1.8), WOOD);
    for y in [-62.0, -49.0, -36.0] {
        rune(&mut p, pos2(28.0, y), BRASS);
    }
    p.cut(capsule(pos2(24.0, -79.0), pos2(34.0, -79.0), 1.6), GOLD);
    crystal(&mut p, pos2(29.0, -84.0), pose.scry);
    if pose.cast.is_some() {
        let held = staff_grip(pose);
        for poly in &mut p.polys {
            poly.points = place(std::mem::take(&mut poly.points), held);
        }
        spell(&mut p, pose, false);
        return p;
    }
    if pose.scry > 0.0 {
        for (x, y) in [(40.0, -63.0), (48.0, -48.0), (37.0, -33.0)] {
            rune(
                &mut p,
                pos2(x, y - pose.scry * 2.0),
                tint(CRYSTAL, pose.scry * 0.4),
            );
        }
    }
    p
}

/// The casting priest's spell on the near side: rays off the crystal and the runes passing in
/// front of it, drawn over his hat and fist.
pub(super) fn priest_spell(pose: &Pose) -> Piece {
    let mut p = Piece::default();
    spell(&mut p, pose, true);
    p
}

/// The staff gripped a third of the way up and tipped forward, its crystal high over his hat.
fn staff_grip(pose: &Pose) -> Xf {
    let hand = Xf::rotate_about(SHOULDER, -pose.arm).apply(HAND);
    Xf::translate(hand.to_vec2())
        .then(Xf::rotate(0.22))
        .then(Xf::translate(-STAFF_GRIP.to_vec2()))
}

/// The half of a spell on the `near` side of the crystal (none unless casting).
fn spell(p: &mut Piece, pose: &Pose, near: bool) {
    let Some(turn) = pose.cast else { return };
    let c = staff_grip(pose).apply(pos2(29.0, -84.0));
    if near {
        for i in 0..8 {
            let a = i as f32 * TAU / 8.0 - turn * 0.5;
            let dir = vec2(a.cos(), a.sin());
            let reach = if i % 2 == 0 { 17.0 } else { 13.0 };
            p.paint(
                capsule(c + dir * 10.0, c + dir * reach, 0.9),
                tint(CRYSTAL, 0.7),
            );
        }
    }
    for i in 0..3 {
        let a = turn + i as f32 * TAU / 3.0;
        if (a.sin() > 0.0) == near {
            let at = c + vec2(a.cos() * 21.0, a.sin() * 8.0) - vec2(1.5, 2.5);
            rune(p, at, if near { CRYSTAL_LIGHT } else { CRYSTAL });
        }
    }
}

pub(super) fn head(look: &Look, pose: &Pose) -> Piece {
    let mut p = Piece::default();
    let skin = look.skin;
    if look.hat != Hat::Bald || pose.nightcap {
        hair(&mut p, look);
    }
    p.cut(ellipse(pos2(5.0, -68.0), 16.5, 15.5, 28), skin);
    // Ear, and an earring.
    p.paint(ellipse(pos2(-2.5, -66.0), 3.6, 4.6, 12), shade(skin, 0.18));
    if look.extras.earring {
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
    let beard_start = p.polys.len();
    beard(&mut p, look);
    if look.role == Role::RunePriest {
        // Stretch only below the cheeks: an elder's braid reaches his robe hem.
        for poly in &mut p.polys[beard_start..] {
            for point in &mut poly.points {
                if point.y > -60.0 {
                    point.y = -60.0 + (point.y + 60.0) * 1.28;
                }
            }
        }
    }
    nose(&mut p, look);
    if look.role == Role::Librarian {
        p.paint(capsule(pos2(-3.0, -70.0), pos2(11.0, -70.0), 1.0), BRASS);
        p.cut(ellipse(pos2(15.0, -70.0), 5.0, 4.6, 16), BRASS);
        p.paint(ellipse(pos2(15.0, -70.0), 3.5, 3.2, 12), tint(skin, 0.3));
        if pose.eyes == Eyes::Open {
            p.paint(ellipse(pos2(15.5, -69.5), 1.2, 1.8, 8), INK);
        }
    }
    if look.extras.pipe {
        let pipe = shade(WOOD, 0.35);
        p.cut(capsule(pos2(22.0, -54.0), pos2(31.0, -51.5), 1.1), pipe);
        p.cut(rect(29.5, -56.5, 34.0, -50.0), pipe);
        p.paint(rect(30.3, -56.5, 33.2, -55.2), FLAME);
    }
    if pose.nightcap {
        nightcap(&mut p, look);
        crest(&mut p, look);
    } else {
        let start = p.polys.len();
        hat(&mut p, look);
        crest(&mut p, look);
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

fn elder_brows(p: &mut Piece, look: &Look) {
    // Solid locks, not an expression above an eye: no pupils or eyelids are drawn.
    for (x, width) in [(-5.0, 13.0), (7.0, 18.0)] {
        p.cut(
            vec![
                pos2(x - 2.0, -77.0),
                pos2(x + 4.0, -78.0),
                pos2(x + width, -76.0),
                pos2(x + width + 3.0, -69.0),
                pos2(x + width - 1.0, -70.0),
                pos2(x + width - 3.0, -65.0),
                pos2(x + width - 7.0, -68.0),
                pos2(x + 3.0, -66.0),
                pos2(x, -70.0),
                pos2(x - 4.0, -70.0),
            ],
            look.hair,
        );
        p.paint(
            vec![
                pos2(x - 2.0, -75.0),
                pos2(x + 3.0, -76.0),
                pos2(x + 4.0, -71.0),
                pos2(x + 1.0, -68.0),
                pos2(x - 1.0, -72.0),
            ],
            shade(look.hair, 0.22),
        );
    }
}

fn face(p: &mut Piece, look: &Look, pose: &Pose) {
    if look.brows == Brows::Shaggy {
        elder_brows(p, look);
        return;
    }
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
        Brows::Shaggy => unreachable!("elder brows have no eyes"),
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
        for y in [-44.0, -37.0, -30.0] {
            p.paint(
                capsule(pos2(10.0, y - 2.0), pos2(17.0, y + 2.0), 1.0),
                shade(c, 0.22),
            );
            p.paint(
                capsule(pos2(15.0, y - 2.0), pos2(11.0, y + 1.0), 0.8),
                tint(c, 0.22),
            );
        }
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

/// A round sun brooch or tall diamond, large enough to read on a farm-sized hat.
fn crest(p: &mut Piece, look: &Look) {
    let Some(color) = look.crest.color() else {
        return;
    };
    let at = pos2(-5.0, -87.0);
    match look.crest {
        Crest::None => {}
        Crest::Sun => {
            p.cut(ellipse(at, 8.0, 8.0, 12), color);
            p.paint(ellipse(at, 3.0, 3.0, 10), tint(color, 0.65));
        }
        Crest::Diamond => {
            p.cut(
                vec![
                    at + vec2(0.0, -11.0),
                    at + vec2(8.0, 0.0),
                    at + vec2(0.0, 9.0),
                    at + vec2(-8.0, 0.0),
                ],
                color,
            );
            p.paint(
                vec![
                    at + vec2(0.0, -7.0),
                    at + vec2(4.0, 0.0),
                    at + vec2(0.0, 5.0),
                ],
                tint(color, 0.65),
            );
        }
    }
}

fn hat(p: &mut Piece, look: &Look) {
    let metal = look.crest.color().unwrap_or(IRON);
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
            p.cut(helmet_dome(), metal);
            p.paint(ellipse(pos2(-2.0, -84.0), 4.5, 2.2, 10), tint(metal, 0.35));
            p.cut(
                capsule(pos2(-15.0, -73.0), pos2(25.0, -73.0), 2.6),
                shade(metal, 0.3),
            );
            for x in [-8.0, 2.0, 12.0] {
                p.paint(ellipse(pos2(x, -73.0), 1.1, 1.1, 6), tint(metal, 0.5));
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
                look.crest.color().unwrap_or(shade(LEATHER, -0.4)),
            );
            p.paint(capsule(pos2(-12.0, -75.0), pos2(18.0, -75.0), 1.4), LEATHER);
        }
        Hat::RuneHat => {
            // A broad, low brim and a bent ceremonial crown, stitched with runes.
            let cloth = shade(look.tunic, 0.15);
            p.cut(
                blob(
                    &[
                        pos2(-22.0, -80.0),
                        pos2(-16.0, -98.0),
                        pos2(-8.0, -116.0),
                        pos2(4.0, -124.0),
                        pos2(18.0, -119.0),
                        pos2(8.0, -113.0),
                        pos2(12.0, -99.0),
                        pos2(23.0, -80.0),
                    ],
                    4,
                ),
                cloth,
            );
            p.paint(
                vec![
                    pos2(-16.0, -86.0),
                    pos2(-7.0, -112.0),
                    pos2(5.0, -120.0),
                    pos2(0.0, -110.0),
                    pos2(-6.0, -86.0),
                ],
                tint(cloth, 0.22),
            );
            p.paint(
                vec![
                    pos2(7.0, -108.0),
                    pos2(17.0, -88.0),
                    pos2(12.0, -82.0),
                    pos2(1.0, -94.0),
                ],
                shade(cloth, 0.3),
            );
            p.paint(rect(-16.0, -91.0, 17.0, -85.0), LEATHER);
            for x in [-12.0, -2.0, 8.0] {
                rune(p, pos2(x, -91.0), BRASS);
            }
            p.cut(
                blob(
                    &[
                        pos2(-31.0, -81.0),
                        pos2(-16.0, -84.0),
                        pos2(9.0, -84.0),
                        pos2(33.0, -78.0),
                        pos2(29.0, -74.0),
                        pos2(6.0, -76.0),
                        pos2(-18.0, -75.0),
                        pos2(-33.0, -77.0),
                    ],
                    4,
                ),
                cloth,
            );
            p.paint(capsule(pos2(-28.0, -79.0), pos2(25.0, -78.0), 1.0), BRASS);
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
                look.crest.color().unwrap_or(BANDANA),
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
                shade(look.crest.color().unwrap_or(BANDANA), 0.2),
            );
            for (x, y) in [(-1.0, -82.0), (9.0, -80.0), (-7.0, -78.0)] {
                p.paint(ellipse(pos2(x, y), 1.0, 1.0, 6), tint(BANDANA, 0.6));
            }
        }
        Hat::Crown => {
            // Broad, tall battlements identify the King without enlarging his body.
            p.cut(
                vec![
                    pos2(-16.0, -78.0),
                    pos2(-19.0, -108.0),
                    pos2(-8.0, -96.0),
                    pos2(-3.0, -112.0),
                    pos2(4.0, -99.0),
                    pos2(11.0, -114.0),
                    pos2(17.0, -97.0),
                    pos2(25.0, -109.0),
                    pos2(22.0, -78.0),
                ],
                GOLD,
            );
            p.paint(rect(-15.0, -88.0, 22.0, -78.0), shade(GOLD, 0.25));
            p.paint(rect(-15.0, -87.0, 22.0, -84.0), tint(GOLD, 0.45));
            p.cut(ellipse(pos2(4.0, -85.0), 4.0, 5.0, 10), RUBY);
            for x in [-9.0, 17.0] {
                p.paint(ellipse(pos2(x, -81.5), 1.8, 1.8, 8), tint(GOLD, 0.5));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::dwarf::{
        Action,
        cutout::{self, Finish},
        pixel, pose,
    };
    use eframe::egui::Rect;

    #[test]
    fn elder_faces_never_reveal_eyes_in_any_pose() {
        for role in [Role::RunePriest, Role::King] {
            let look = Look::for_role(role, 7);
            let render = |pose: &Pose| {
                let mut face_piece = Piece::default();
                face(&mut face_piece, &look, pose);
                let mut shapes = Vec::new();
                cutout::draw(&mut shapes, &face_piece, Xf::IDENTITY, &Finish::INK);
                pixel::rasterize(
                    &shapes,
                    Rect::from_min_size(pos2(-15.0, -85.0), vec2(50.0, 30.0)),
                    [50, 30],
                    1,
                )
            };
            let still = render(&pose(Action::Still, 0.0));
            assert!(still.pixels.iter().any(|c| c.a() > 0));
            for action in Action::ALL {
                for time in [0.0, 0.3, 0.7] {
                    assert_eq!(render(&pose(action, time)), still, "{role:?} {action:?}");
                }
            }
        }
        // Ordinary eyebrows still reveal the different eye expressions.
        let mut look = Look::from_seed(7);
        look.brows = Brows::Thin;
        let mut open = Piece::default();
        let mut shut = Piece::default();
        face(&mut open, &look, &pose(Action::Still, 0.0));
        face(&mut shut, &look, &pose(Action::Sleep, 0.0));
        assert_ne!(format!("{open:?}"), format!("{shut:?}"));
    }
}

//! What a dwarf is doing, and the pose that gives at any moment. Only work moves: a dwarf
//! that is idle, waiting, asleep or knocked out holds one pose.

use std::f32::consts::{FRAC_PI_2, TAU};

/// What a dwarf is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Standing still (idle, done).
    Still,
    /// Waiting on the user: leaning on its tool, glaring up. Holds still.
    Wait,
    /// Working something out: head tilting, scratching under the hat, a "?" popping.
    Study,
    /// Reading a book held at chest height.
    Read,
    /// Writing with a quill, the book supported in the other hand.
    Write,
    /// Hands raised over a scrying basin; only active work pulses the runes.
    Scry,
    /// Staff thrust up in his fist, the other arm flung back, the crystal blazing and runes
    /// circling it: the rune priest taking in a message.
    Cast,
    /// Swinging the tool overhead into the rock.
    Mine,
    /// Hammering at chest height (editing, building).
    Build,
    Walk,
    /// Alternating hands and feet on a ladder, with the tool stowed.
    Climb,
    /// Braced against a minecart rim, tool stowed; only used during travel.
    Ride,
    /// Walking with a gold nugget held overhead.
    Carry,
    /// Standing with a gold nugget held overhead.
    Hold,
    /// On his back with a nightcap. Holds still.
    Sleep,
    /// On his back, eyes crossed. Holds still.
    KnockedOut,
}

impl Action {
    pub const ALL: [Action; 16] = [
        Action::Still,
        Action::Wait,
        Action::Study,
        Action::Read,
        Action::Write,
        Action::Scry,
        Action::Cast,
        Action::Mine,
        Action::Build,
        Action::Walk,
        Action::Climb,
        Action::Ride,
        Action::Carry,
        Action::Hold,
        Action::Sleep,
        Action::KnockedOut,
    ];

    /// Whether the pose changes over time.
    pub fn moves(self) -> bool {
        matches!(
            self,
            Action::Study
                | Action::Read
                | Action::Write
                | Action::Scry
                | Action::Cast
                | Action::Mine
                | Action::Build
                | Action::Walk
                | Action::Climb
                | Action::Ride
                | Action::Carry
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Eyes {
    #[default]
    Open,
    Shut,
    Crossed,
}

/// Joint angles and body deformation at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pose {
    /// Front arm, radians forward from hanging straight down.
    pub arm: f32,
    /// Quill in the front hand, book in the supporting hand.
    pub writing: bool,
    /// Back arm, radians forward.
    pub back_arm: f32,
    /// The tool turned in the fist, radians forward from lying along the arm.
    pub grip: f32,
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
    pub eyes: Eyes,
    /// A nightcap instead of his hat.
    pub nightcap: bool,
    /// A gold nugget in his arms instead of his tool.
    pub nugget: bool,
    /// His tool put away (lying down).
    pub stowed: bool,
    /// Both hands grip a separately drawn prop; omit the free back arm.
    pub two_handed: bool,
    /// Seconds since the tool last struck, for sparks.
    pub since_strike: Option<f32>,
    /// A "?" over the head, 0..1 grown.
    pub question: f32,
    /// The whole dwarf turned about his middle, radians clockwise (lying down, tumbling).
    pub roll: f32,
    /// Blue crystal and rune pulse, zero except while scrying.
    pub scry: f32,
    /// While casting: the staff raised and the turn, radians, of the runes circling its crystal.
    pub cast: Option<f32>,
}

/// The pose for `action` at `t` seconds (each dwarf passes its own phase offset in `t`).
pub fn pose(action: Action, t: f64) -> Pose {
    let rest = Pose {
        arm: 0.25,
        back_arm: -0.1,
        ..Pose::default()
    };
    // On his back, his back on the floor.
    let lying = Pose {
        roll: -FRAC_PI_2,
        hop: -24.0,
        arm: 0.6,
        back_arm: 0.4,
        stowed: true,
        ..rest
    };
    match action {
        Action::Still => rest,
        Action::Read => Pose {
            arm: 1.2 + 0.06 * (t as f32 * 2.0).sin(),
            grip: -1.2,
            head: 0.16 + 0.08 * (t as f32 * 1.5).sin(),
            ..rest
        },
        Action::Write => Pose {
            arm: 1.1 + 0.14 * (t as f32 * 9.0).sin(),
            back_arm: 1.1,
            grip: -0.8,
            head: 0.2,
            writing: true,
            ..rest
        },
        Action::Wait => Pose {
            // Both fists on the tool, its head on the floor in front; chin up, glaring.
            arm: 0.6,
            grip: -0.6,
            back_arm: 0.75,
            head: -0.28,
            brows: -1.0,
            ..rest
        },
        Action::Sleep => Pose {
            eyes: Eyes::Shut,
            nightcap: true,
            brows: 0.3,
            ..lying
        },
        Action::KnockedOut => Pose {
            eyes: Eyes::Crossed,
            brows: 1.0,
            arm: -0.7,
            back_arm: 0.9,
            ..lying
        },
        Action::Mine => {
            const CYCLE: f64 = 0.95;
            let u = (t.rem_euclid(CYCLE) / CYCLE) as f32;
            // An overhead swing, both hands on the handle. Angles are backward from hanging
            // down: the tool rises behind him, hangs above his head, comes over the top and
            // down in front, and follows through to hang again (a full turn later).
            let rest_back = -rest.arm;
            let (top, hit) = (2.75, 4.95);
            let (back, lean, stretch) = if u < 0.42 {
                let k = ease_in_out(u / 0.42);
                (lerp(rest_back, top, k), -0.08 * k, 0.06 * k)
            } else if u < 0.52 {
                (top, -0.08, 0.06)
            } else if u < 0.6 {
                let k = ease_in((u - 0.52) / 0.08);
                (lerp(top, hit, k), lerp(-0.08, 0.14, k), lerp(0.06, -0.1, k))
            } else {
                let k = ease_out((u - 0.6) / 0.4);
                (
                    lerp(hit, rest_back + TAU, k) - 0.12 * (1.0 - k) * (k * 9.0).sin(),
                    lerp(0.14, 0.0, k),
                    lerp(-0.1, 0.0, k) + 0.03 * (1.0 - k) * (k * 14.0).sin(),
                )
            };
            let arm = -back;
            Pose {
                arm,
                back_arm: arm,
                head: lean * 0.8,
                lean,
                stretch,
                brows: -1.0,
                since_strike: (u >= 0.6).then_some((u - 0.6) * CYCLE as f32),
                ..rest
            }
        }
        Action::Build => {
            const CYCLE: f64 = 0.6;
            let u = (t.rem_euclid(CYCLE) / CYCLE) as f32;
            // Short chest-high blows: lift, strike level, bounce back.
            let (up, hit) = (2.5, 1.45);
            let arm = if u < 0.55 {
                lerp(hit, up, ease_out(u / 0.55))
            } else if u < 0.7 {
                lerp(up, hit, ease_in((u - 0.55) / 0.15))
            } else {
                hit + 0.1 * (1.0 - (u - 0.7) / 0.3) * ((u - 0.7) * 40.0).sin()
            };
            let strike = (u >= 0.7).then_some((u - 0.7) * CYCLE as f32);
            Pose {
                arm,
                back_arm: 1.2,
                head: 0.08,
                lean: if strike.is_some() { 0.06 } else { 0.0 },
                stretch: if strike.is_some() { -0.04 } else { 0.0 },
                brows: -1.0,
                since_strike: strike,
                ..rest
            }
        }
        Action::Scry => {
            let wave = (t as f32 * 3.0).sin();
            Pose {
                arm: 1.35 + wave * 0.08,
                back_arm: 1.55 - wave * 0.06,
                head: 0.12,
                lean: 0.05,
                brows: -0.5,
                scry: 0.55 + 0.45 * wave,
                ..rest
            }
        }
        Action::Cast => {
            let surge = (t as f32 * 7.0).sin();
            Pose {
                arm: 1.9 + surge * 0.04,
                back_arm: -1.3 - surge * 0.1,
                head: -0.22,
                lean: -0.04,
                stretch: 0.03 + 0.015 * surge,
                brows: 1.0,
                scry: 1.6 + 0.4 * surge,
                cast: Some((t as f32 * 2.4).rem_euclid(TAU)),
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
        Action::Climb => {
            let s = (t as f32 * TAU / 0.7).sin();
            Pose {
                arm: -1.8 + 0.45 * s,
                back_arm: -1.8 - 0.45 * s,
                stride: 4.0 * s,
                stowed: true,
                ..rest
            }
        }
        Action::Ride => Pose {
            arm: 0.95,
            back_arm: 0.85,
            stowed: true,
            lean: 0.025,
            head: (t as f32 * 9.0).sin() * 0.025,
            hop: 2.0 + (t as f32 * 15.0).sin().abs(),
            ..rest
        },
        Action::Walk | Action::Carry => {
            const CYCLE: f64 = 0.56;
            let u = (t.rem_euclid(CYCLE) / CYCLE) as f32;
            let s = (u * TAU).sin();
            let gait = Pose {
                arm: 0.25 - 0.35 * s,
                back_arm: 0.35 * s,
                head: 0.03 * s,
                lean: 0.06,
                hop: 2.2 * s.abs(),
                stride: 5.0 * s,
                stretch: 0.03 * s.abs(),
                ..rest
            };
            if action == Action::Carry {
                // Lugging it in both arms: leaning back a little under the weight.
                Pose {
                    arm: 1.0 + 0.04 * s,
                    back_arm: 1.25 + 0.04 * s,
                    lean: -0.05,
                    nugget: true,
                    ..gait
                }
            } else {
                gait
            }
        }
        Action::Hold => Pose {
            arm: 1.0,
            back_arm: 1.25,
            lean: -0.05,
            nugget: true,
            brows: 0.6,
            ..rest
        },
    }
}

/// A demolition stroke: quick lift, accelerating blow, a firm contact hold, then recovery.
/// `contact_arm` aims the sledge at the top of the particular heap.
pub fn demolition(phase: f32, contact_arm: f32) -> Pose {
    let raised = 3.5;
    let arm = if phase < 0.32 {
        lerp(2.1, raised, ease_out(phase / 0.32))
    } else if phase < 0.44 {
        lerp(raised, contact_arm, ease_in((phase - 0.32) / 0.12))
    } else if phase < 0.56 {
        contact_arm
    } else {
        lerp(contact_arm, 2.1, ease_in_out((phase - 0.56) / 0.44))
    };
    Pose {
        arm,
        back_arm: arm,
        head: if (0.44..0.56).contains(&phase) {
            0.12
        } else {
            -0.04
        },
        brows: -1.0,
        stowed: true,
        two_handed: true,
        ..Pose::default()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// No idle loops: anything that isn't work holds one pose.
    #[test]
    fn only_work_moves() {
        for action in Action::ALL {
            let first = pose(action, 0.0);
            let still = (0..60).all(|i| pose(action, i as f64 * 0.137) == first);
            assert_eq!(still, !action.moves(), "{action:?}");
        }
    }

    #[test]
    fn tools_strike_once_per_swing() {
        for (action, cycle) in [(Action::Mine, 950), (Action::Build, 600)] {
            let strikes = (0..cycle)
                .filter(|ms| {
                    let t = *ms as f64 / 1000.0;
                    pose(action, t).since_strike.is_some()
                        && pose(action, t - 0.001).since_strike.is_none()
                })
                .count();
            assert_eq!(strikes, 1, "{action:?}");
        }
    }

    #[test]
    fn demolition_holds_contact_then_recovers_without_a_jump() {
        let contact = 1.5;
        assert_eq!(demolition(0.32, contact).arm, 3.5);
        for phase in [0.44, 0.48, 0.55, 0.56] {
            assert!((demolition(phase, contact).arm - contact).abs() < 0.001);
        }
        assert!((demolition(0.99999, contact).arm - demolition(0.0, contact).arm).abs() < 0.001);
        assert!(demolition(0.43, contact).arm > contact);
    }

    #[test]
    fn sleepers_lie_down_in_a_nightcap() {
        let p = pose(Action::Sleep, 0.0);
        assert!(p.nightcap && p.eyes == Eyes::Shut && p.roll != 0.0);
        let k = pose(Action::KnockedOut, 0.0);
        assert!(!k.nightcap && k.eyes == Eyes::Crossed && k.roll == p.roll);
    }
}

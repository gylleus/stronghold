//! An ant leaving the fleet (closed, its terminal gone, or it exited itself): it hops, flips
//! legs up and falls to the bottom of the farm under gravity, bounces once and fades. With
//! the system's Reduce motion on it only fades where it stood.

use eframe::egui::{Pos2, Rect, pos2};

use super::motion;
use super::sprites::Coat;

/// From the hop to hitting the bottom.
pub const FALL_S: f64 = 1.0;
/// Lying at the bottom, with one small bounce.
pub const REST_S: f64 = 0.25;
/// Fading out at the bottom.
pub const FADE_S: f64 = 0.4;
/// The whole fade when motion is reduced.
pub const STILL_FADE_S: f64 = 0.5;
/// The hop before the fall, and the bounce on landing, in ant sizes.
const HOP: f32 = 0.5;
const BOUNCE: f32 = 0.15;

/// How an ant looked when it was last drawn, for its fall.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    pub kind: crate::model::NodeKind,
    /// Relative to the farm's rect; see [`motion::to_unit`].
    pub at: Pos2,
    pub size: f32,
    pub facing_right: bool,
    pub coat: Coat,
    pub alpha: f32,
}

/// A dead ant falling, from `start_s`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fall {
    pub body: Body,
    pub start_s: f64,
    /// Reduce motion: fade in place.
    pub still: bool,
}

/// Where a falling ant is drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FallFrame {
    pub pos: Pos2,
    pub upside_down: bool,
    pub alpha: f32,
}

impl Fall {
    pub fn duration(&self) -> f64 {
        if self.still {
            STILL_FADE_S
        } else {
            FALL_S + REST_S + FADE_S
        }
    }

    /// The fall at `now_s` in `rect`; `None` before it starts or once it is over.
    pub fn frame(&self, rect: Rect, now_s: f64) -> Option<FallFrame> {
        let t = now_s - self.start_s;
        if !(0.0..self.duration()).contains(&t) {
            return None;
        }
        let from = motion::from_unit(self.body.at, rect);
        if self.still {
            return Some(FallFrame {
                pos: from,
                upside_down: false,
                alpha: self.body.alpha * (1.0 - (t / STILL_FADE_S) as f32),
            });
        }
        let size = self.body.size;
        let floor = rect.bottom() - 0.5 * size;
        let (hop, drop) = (HOP * size, (floor - from.y).max(0.0));
        // Gravity chosen so every fall, whatever its height, lands after FALL_S.
        let g = 2.0 * (hop.sqrt() + (hop + drop).sqrt()).powi(2) / (FALL_S * FALL_S) as f32;
        let v0 = (2.0 * g * hop).sqrt();
        let ft = t.min(FALL_S) as f32;
        let mut y = (from.y - v0 * ft + 0.5 * g * ft * ft).min(floor);
        let after = t - FALL_S;
        if (0.0..REST_S).contains(&after) {
            y -= BOUNCE * size * (std::f64::consts::PI * after / REST_S).sin() as f32;
        }
        let fade = ((after - REST_S) / FADE_S).clamp(0.0, 1.0) as f32;
        Some(FallFrame {
            pos: pos2(from.x, y),
            // It flips at the top of its hop.
            upside_down: ft >= v0 / g,
            alpha: self.body.alpha * (1.0 - fade),
        })
    }
}

/// Whether the system asks for reduced motion (macOS Accessibility > Display > Reduce motion).
#[cfg(target_os = "macos")]
pub fn reduce_motion() -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    let Some(class) = AnyClass::get(c"NSWorkspace") else {
        return false;
    };
    // SAFETY: both are documented NSWorkspace methods without arguments; the shared
    // workspace is checked for nil before use.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let reduce: Bool = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        reduce.as_bool()
    }
}

#[cfg(not(target_os = "macos"))]
pub fn reduce_motion() -> bool {
    false
}

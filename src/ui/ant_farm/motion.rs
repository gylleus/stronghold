//! Ants walking to their new spot when their state changes: up their tunnel to the gallery,
//! along it and down to the new place. No pathfinding; the farm's passages are simple enough.

use eframe::egui::{Pos2, Rect, Vec2, pos2};

/// Walking speed in points per second.
pub const SPEED: f32 = 240.0;
pub const MIN_WALK_S: f64 = 0.5;
pub const MAX_WALK_S: f64 = 2.5;

/// The waypoints from `from` to `to`: straight within one area, else by way of the gallery at
/// height `gallery_y`.
pub fn route(from: Pos2, to: Pos2, same_area: bool, gallery_y: f32) -> Vec<Pos2> {
    if same_area {
        return vec![from, to];
    }
    vec![from, pos2(from.x, gallery_y), pos2(to.x, gallery_y), to]
}

pub fn path_length(path: &[Pos2]) -> f32 {
    path.windows(2).map(|w| (w[1] - w[0]).length()).sum()
}

/// The point `t` (0..=1) of the way along `path` by distance, and the direction it heads.
pub fn along(path: &[Pos2], t: f32) -> (Pos2, Vec2) {
    let Some(&first) = path.first() else {
        return (Pos2::ZERO, Vec2::ZERO);
    };
    let total = path_length(path);
    if total <= f32::EPSILON {
        return (first, Vec2::ZERO);
    }
    let mut left = total * t.clamp(0.0, 1.0);
    for w in path.windows(2) {
        let d = w[1] - w[0];
        let len = d.length();
        if left <= len && len > 0.0 {
            return (w[0] + d * (left / len), d);
        }
        left -= len;
    }
    let n = path.len();
    (path[n - 1], path[n - 1] - path[n.saturating_sub(2)])
}

/// Eases a walk in and out, so ants start and stop gently.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A walk in progress, in coordinates relative to the farm's rect (0..1 on each axis) so a
/// resize mid-walk keeps it on track.
#[derive(Debug, Clone, PartialEq)]
pub struct Walk {
    pub path: Vec<Pos2>,
    pub start_s: f64,
    pub duration_s: f64,
}

impl Walk {
    /// A walk along `path` (screen points in `rect`) starting at `now_s`.
    pub fn new(path: &[Pos2], rect: Rect, now_s: f64) -> Self {
        let duration_s =
            (f64::from(path_length(path)) / f64::from(SPEED)).clamp(MIN_WALK_S, MAX_WALK_S);
        Self {
            path: path.iter().map(|p| to_unit(*p, rect)).collect(),
            start_s: now_s,
            duration_s,
        }
    }

    /// A straight walk from `from` to `to` taking `duration_s`, as when its room moved.
    pub fn glide(from: Pos2, to: Pos2, rect: Rect, now_s: f64, duration_s: f64) -> Self {
        Self {
            path: vec![to_unit(from, rect), to_unit(to, rect)],
            start_s: now_s,
            duration_s,
        }
    }

    pub fn done(&self, now_s: f64) -> bool {
        now_s >= self.start_s + self.duration_s
    }

    /// Where the walker is at `now_s` in `rect`, and the direction it heads.
    pub fn sample(&self, rect: Rect, now_s: f64) -> (Pos2, Vec2) {
        let t = ((now_s - self.start_s) / self.duration_s) as f32;
        let path: Vec<Pos2> = self.path.iter().map(|p| from_unit(*p, rect)).collect();
        along(&path, ease(t))
    }

    /// Moves the walk's end to `to` (screen points in `rect`), as when the view resized.
    pub fn retarget(&mut self, to: Pos2, rect: Rect) {
        let to = to_unit(to, rect);
        let n = self.path.len();
        if n == 4 {
            self.path[2].x = to.x;
        }
        if let Some(last) = self.path.last_mut() {
            *last = to;
        }
    }
}

pub fn to_unit(p: Pos2, rect: Rect) -> Pos2 {
    let size = rect.size().max(Vec2::splat(1.0));
    ((p - rect.min) / size).to_pos2()
}

pub fn from_unit(p: Pos2, rect: Rect) -> Pos2 {
    rect.min + p.to_vec2() * rect.size()
}

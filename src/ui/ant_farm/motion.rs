//! Minecart journeys through chamber sidings, shaft hoists and the gallery.

use super::layout::{Scene, Spot};
use eframe::egui::{Pos2, Rect, Vec2, pos2};

/// Walking speed in points per second.
pub const SPEED: f32 = 240.0;
pub const MIN_WALK_S: f64 = 0.5;
pub const MAX_WALK_S: f64 = 2.5;

/// A cart uses the actual shaft mouths instead of cutting vertically through solid rock.
/// Chamber legs stay inside the room; shaft legs are carried by a hoist.
pub fn route(from: Pos2, to: Pos2, from_spot: &Spot, to_spot: &Spot, scene: &Scene) -> Vec<Pos2> {
    if from_spot.area() == to_spot.area() {
        return vec![from, pos2(to.x, from.y), to];
    }
    fn access(at: Pos2, spot: &Spot, scene: &Scene) -> Vec<Pos2> {
        let gy = scene.gallery.center().y;
        if scene.gallery.contains(at) {
            return vec![at, pos2(at.x, gy)];
        }
        // A second state change can interrupt a journey in its shaft.
        if let Some(t) = scene
            .tunnels
            .iter()
            .find(|t| t.shaft.contains(at) && !t.chamber.contains(at))
        {
            return vec![
                at,
                pos2(t.shaft.center().x, at.y),
                pos2(t.shaft.center().x, gy),
            ];
        }
        let x = match spot {
            Spot::Landing(_) => scene.entrance_x(),
            Spot::Queen => scene.queen_chamber.center().x,
            Spot::Storage(_) => scene.storage.center().x,
            Spot::Comms => scene.comms.map_or(at.x, |r| r.shaft.center().x),
            Spot::Mouth(key, _) | Spot::Chamber(key, _) => {
                if let Some(t) = scene.tunnels.iter().find(|t| &t.key == key) {
                    return vec![
                        at,
                        pos2(t.shaft.center().x, at.y),
                        pos2(t.shaft.center().x, gy),
                    ];
                }
                at.x
            }
            Spot::Trail(..) => at.x,
        };
        vec![at, pos2(x, at.y), pos2(x, gy)]
    }
    let mut path = access(from, from_spot, scene);
    let arrival = access(to, to_spot, scene);
    path.extend(arrival.into_iter().rev());
    path.dedup();
    path
}

/// The overhead anchor for a cart hoist, in the shaft or room it currently occupies.
pub fn hoist_ceiling(scene: &Scene, at: Pos2) -> f32 {
    if let Some(t) = scene.tunnels.iter().find(|t| t.chamber.contains(at)) {
        return t.chamber.top() + 2.0 * scene.px;
    }
    if let Some(t) = scene.tunnels.iter().find(|t| t.shaft.contains(at)) {
        return t.shaft.top();
    }
    for r in [scene.queen_chamber, scene.storage] {
        if r.contains(at) {
            return r.top();
        }
    }
    scene.surface_y
}

pub fn path_length(path: &[Pos2]) -> f32 {
    path.windows(2).map(|w| (w[1] - w[0]).length()).sum()
}

/// The point `t` (0..=1) of the way along `path` by distance, and the direction it heads.
pub fn along(path: &[Pos2], t: f32) -> (Pos2, Vec2) {
    let Some(&first) = path.first() else {
        return (Pos2::ZERO, Vec2::ZERO);
    };
    if t >= 1.0 {
        let n = path.len();
        return (path[n - 1], path[n - 1] - path[n.saturating_sub(2)]);
    }
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
        if n > 2 {
            let old = self.path[n - 1];
            if self.path[n - 2].x == old.x {
                self.path[n - 2].x = to.x;
            }
            if self.path[n - 2].y == old.y {
                self.path[n - 2].y = to.y;
            }
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

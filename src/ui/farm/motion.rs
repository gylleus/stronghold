//! Walking journeys along horizontal galleries and their shared ladders.

use super::layout::{Scene, Spot};
use eframe::egui::{Pos2, Rect, Vec2, pos2};

/// Walking speed in points per second.
pub const SPEED: f32 = 240.0;
pub const MIN_WALK_S: f64 = 0.5;
pub const MAX_WALK_S: f64 = 2.5;

/// Walk through each group's ladder and its level's access passage. Resolve the
/// current physical segment first so an interrupted walk cannot cut through a wall.
pub fn route(from: Pos2, to: Pos2, from_spot: &Spot, to_spot: &Spot, scene: &Scene) -> Vec<Pos2> {
    let underground = |p: Pos2| p.y > scene.gallery.bottom();
    let main_x = scene.work_ladder().center().x;
    let tunnel_at = |at: Pos2| {
        scene.tunnels.iter().find(|t| {
            let ladder = scene.tunnel_ladder(t);
            ladder.contains(at)
                || Rect::from_min_max(pos2(ladder.left(), t.chamber.top()), t.chamber.max)
                    .contains(at)
        })
    };
    if underground(from)
        && underground(to)
        && let (Some(a), Some(b)) = (tunnel_at(from), tunnel_at(to))
        && a.key == b.key
    {
        let x = scene.tunnel_ladder(a).center().x;
        let mut path = if from.y == to.y {
            vec![from, to]
        } else {
            vec![from, pos2(x, from.y), pos2(x, to.y), to]
        };
        path.dedup();
        return path;
    }
    if !underground(from)
        && !underground(to)
        && from_spot.area() == to_spot.area()
        && from.y == to.y
    {
        return vec![from, to];
    }
    let access = |at: Pos2, spot: &Spot| {
        let gy = scene.gallery.center().y;
        if underground(at) {
            if scene.work_ladder().contains(at)
                || scene.access_galleries().iter().any(|r| r.contains(at))
            {
                return vec![at, pos2(main_x, at.y), pos2(main_x, gy)];
            }
            if let Some(t) = tunnel_at(at).or_else(|| {
                scene.tunnels.iter().find(|t|
                matches!(spot, Spot::Mouth(key, _) | Spot::Chamber(key, _) if *key == t.key))
            }) {
                let x = scene.tunnel_ladder(t).center().x;
                let y = scene.access_y(t);
                return vec![
                    at,
                    pos2(x, at.y),
                    pos2(x, y),
                    pos2(main_x, y),
                    pos2(main_x, gy),
                ];
            }
        }
        let x = if scene.gallery.contains(at) {
            at.x
        } else {
            match spot {
                Spot::Landing(_) => scene.entrance_x(),
                Spot::King => scene.throne_hall.center().x,
                Spot::Storage(_) => scene.storage.center().x,
                Spot::Ledger => scene.ledger.map_or(at.x, |r| r.center().x),
                Spot::Comms => scene.comms.map_or(at.x, |r| r.shaft.center().x),
                Spot::Mouth(..) | Spot::Chamber(..) | Spot::Trail(..) => at.x,
            }
        };
        vec![at, pos2(x, at.y), pos2(x, gy)]
    };
    let mut path = access(from, from_spot);
    let mut end = access(to, to_spot);
    path.dedup();
    end.dedup();
    // Join at the nearest shared junction instead of walking up and back down.
    while path.len() > 1
        && end.len() > 1
        && path.last() == end.last()
        && path[path.len() - 2] == end[end.len() - 2]
    {
        path.pop();
        end.pop();
    }
    path.extend(end.into_iter().rev());
    path.dedup();
    path
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

/// Eases a walk in and out, so dwarves start and stop gently.
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

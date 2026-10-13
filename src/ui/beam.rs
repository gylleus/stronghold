//! Tool calls drawn as beams from the caller to the agents they act on, see
//! [`crate::traffic::beam`]: warm light pulled back along a read, a ring for a listing,
//! a shot and burst for a close (dashed for a dry run), and a burst for a spawned agent. Each lasts under a second. With
//! the system's Reduce motion on, they show in place and fade instead. Shared by the farm, which supply the screen positions.
//!
//! Bursts stay calm: the King's parallel reads within [`MERGE_S`] become one fan of lines, a
//! repeat of a beam still showing is dropped, and at most [`MAX_BEAMS`] show at once.

use std::f32::consts::TAU;

use eframe::egui::{Color32, Painter, Pos2, Shape, Stroke, Vec2};

use crate::traffic::beam::{Beam, BeamKind};
use crate::ui::farm::motion;

/// How long a beam shows, moving or still.
pub const LIFE_S: f64 = 0.8;
/// Beams of one caller and kind queued within this long are drawn as one.
pub const MERGE_S: f64 = 0.3;
/// Beams queued longer than this before a frame drew them (their view was not shown) are
/// dropped.
pub const STALE_S: f64 = 2.0;
pub const MAX_BEAMS: usize = 8;

/// One tool call currently lighting its targets.
#[derive(Debug, Clone, PartialEq)]
pub struct Lit {
    pub beam: Beam,
    /// When it was queued, and when it lit (`None` until a frame starts it).
    pub queued_s: f64,
    pub start_s: Option<f64>,
    /// Reduce motion: show in place and fade.
    pub still: bool,
}

impl Lit {
    /// How far into its life it is at `now_s` (0..1); `None` before or after.
    pub fn progress(&self, now_s: f64) -> Option<f32> {
        let t = (now_s - self.start_s?) / LIFE_S;
        (0.0..1.0).contains(&t).then_some(t as f32)
    }
}

/// The beams a view is drawing.
#[derive(Debug, Default)]
pub struct Beams {
    lit: Vec<Lit>,
}

impl Beams {
    /// Queues `beam`, asked for at `now_s`.
    pub fn push(&mut self, beam: Beam, now_s: f64, still: bool) {
        let same = |l: &Lit| l.beam.from == beam.from && l.beam.kind == beam.kind;
        let showing = |l: &Lit| l.start_s.is_none_or(|s| now_s - s < LIFE_S);
        let covered = |l: &Lit| beam.targets.iter().all(|t| l.beam.targets.contains(t));
        if self.lit.iter().any(|l| same(l) && showing(l) && covered(l)) {
            return;
        }
        let joins = |l: &&mut Lit| same(l) && now_s - l.queued_s < MERGE_S;
        if let Some(l) = self.lit.iter_mut().find(joins) {
            for t in beam.targets {
                if !l.beam.targets.contains(&t) {
                    l.beam.targets.push(t);
                }
            }
            return;
        }
        if self.lit.len() >= MAX_BEAMS {
            log::debug!("beam: {MAX_BEAMS} beams showing, dropped one");
            return;
        }
        self.lit.push(Lit {
            beam,
            queued_s: now_s,
            start_s: None,
            still,
        });
    }

    /// Lights queued beams, drops stale and finished ones, and returns those showing at
    /// `now_s`.
    pub fn tick(&mut self, now_s: f64) -> &[Lit] {
        self.lit
            .retain(|l| l.start_s.is_some() || now_s - l.queued_s < STALE_S);
        for l in self.lit.iter_mut().filter(|l| l.start_s.is_none()) {
            l.start_s = Some(now_s);
        }
        self.lit
            .retain(|l| now_s - l.start_s.unwrap_or(now_s) < LIFE_S);
        &self.lit
    }

    pub fn is_empty(&self) -> bool {
        self.lit.is_empty()
    }
}

/// Where a beam's end is drawn: a center and the radius of what is drawn there.
pub type End = (Pos2, f32);

/// Draws `l` from `from` to `targets` (screen positions of those drawn) at `now_s`; `size`
/// scales line widths and bursts with the view's zoom.
pub fn paint(
    painter: &Painter,
    l: &Lit,
    from: End,
    targets: &[End],
    size: f32,
    now_s: f64,
    color: Color32,
) {
    let Some(t) = l.progress(now_s) else {
        return;
    };
    let width = (size * 0.06).max(1.0);
    if l.beam.kind == BeamKind::Sweep {
        sweep(painter, l.still, t, from, targets, color, width);
        return;
    }
    for &to in targets {
        let Some([a, b]) = rim_to_rim(from, to) else {
            continue;
        };
        if l.still {
            still(painter, l.beam.kind, t, [a, b], to, color, width);
            continue;
        }
        match l.beam.kind {
            BeamKind::Read | BeamKind::Write => read(painter, t, [a, b], color, width),
            BeamKind::Kill => kill(painter, t, [a, b], to, color, width),
            BeamKind::DryKill => {
                dashed(painter, [a, b], color.gamma_multiply(0.4 * fade(t)), width);
            }
            BeamKind::Spawn => spawn(painter, t, [a, b], to, color, width),
            BeamKind::Sweep => {}
        }
    }
}

/// The segment between the rims of `from` and `to`; `None` when they overlap.
fn rim_to_rim(from: End, to: End) -> Option<[Pos2; 2]> {
    let d = to.0 - from.0;
    let len = d.length();
    if len <= from.1 + to.1 {
        return None;
    }
    let dir = d / len;
    Some([from.0 + dir * from.1, to.0 - dir * to.1])
}

/// In over the first tenth, out over the last quarter.
fn fade(t: f32) -> f32 {
    (t / 0.1).min(1.0).min((1.0 - t) / 0.25).clamp(0.0, 1.0)
}

/// A thin line, with a dot pulled from the target back to the caller.
fn read(painter: &Painter, t: f32, [a, b]: [Pos2; 2], color: Color32, width: f32) {
    let alpha = fade(t);
    painter.line_segment(
        [a, b],
        Stroke::new(width, color.gamma_multiply(0.55 * alpha)),
    );
    let pull = motion::ease(((t - 0.1) / 0.55).clamp(0.0, 1.0));
    if pull < 1.0 {
        let at = b + (a - b) * pull;
        painter.circle_filled(at, width * 2.2, color.gamma_multiply(alpha));
    }
}

/// A laser shot from the caller, held, then a burst at the target as it fades.
fn kill(painter: &Painter, t: f32, [a, b]: [Pos2; 2], to: End, color: Color32, width: f32) {
    const SHOT: f32 = 0.15;
    const BURST: f32 = 0.45;
    let w = width * 2.0;
    let tip = a + (b - a) * (t / SHOT).min(1.0);
    let alpha = if t < BURST {
        1.0
    } else {
        (1.0 - (t - BURST) / 0.2).max(0.0)
    };
    if alpha > 0.0 {
        painter.line_segment(
            [a, tip],
            Stroke::new(w * 3.0, color.gamma_multiply(0.25 * alpha)),
        );
        painter.line_segment([a, tip], Stroke::new(w, color.gamma_multiply(alpha)));
        let core = Color32::from_rgb(0xff, 0xe0, 0xe0).gamma_multiply(alpha);
        painter.line_segment([a, tip], Stroke::new((w * 0.4).max(1.0), core));
    }
    if t >= BURST {
        let u = (t - BURST) / (1.0 - BURST);
        burst(painter, to, u, 8, 0.0, color, w);
    }
}

/// A faint dashed line to the new agent, which flares as it appears.
fn spawn(painter: &Painter, t: f32, [a, b]: [Pos2; 2], to: End, color: Color32, width: f32) {
    dashed(painter, [a, b], color.gamma_multiply(0.45 * fade(t)), width);
    burst(painter, to, t, 6, t * 0.6, color, width * 1.5);
}

/// A ring growing from the rim of `at` with `rays` spikes turned by `turn` (turns), fading as
/// `u` goes from 0 to 1.
fn burst(painter: &Painter, at: End, u: f32, rays: usize, turn: f32, color: Color32, width: f32) {
    let (c, r) = at;
    let r = r.max(4.0);
    let grow = motion::ease(u);
    let color = color.gamma_multiply(1.0 - u);
    painter.circle_stroke(c, r * (1.0 + 0.9 * grow), Stroke::new(width, color));
    for i in 0..rays {
        let angle = (i as f32 / rays as f32 + turn) * TAU;
        let dir = Vec2::angled(angle);
        let inner = r * (1.1 + 0.6 * grow);
        let outer = r * (1.4 + 1.1 * grow);
        painter.line_segment(
            [c + dir * inner, c + dir * outer],
            Stroke::new(width, color),
        );
    }
}

/// A ring spreading from the caller to the farthest target; each target rings as it passes.
fn sweep(
    painter: &Painter,
    still: bool,
    t: f32,
    from: End,
    targets: &[End],
    color: Color32,
    width: f32,
) {
    if still {
        let alpha = 1.0 - t;
        for &(c, r) in targets {
            let stroke = Stroke::new(width * 1.5, color.gamma_multiply(0.8 * alpha));
            painter.circle_stroke(c, r.max(4.0) * 1.3, stroke);
        }
        return;
    }
    let far = targets
        .iter()
        .map(|(c, r)| (*c - from.0).length() + r)
        .fold(from.1, f32::max);
    let reach = from.1 + (far - from.1) * motion::ease((t / 0.7).min(1.0));
    let alpha = 1.0 - t;
    painter.circle_stroke(
        from.0,
        reach,
        Stroke::new(width * 2.0, color.gamma_multiply(0.5 * alpha)),
    );
    let band = ((far - from.1) * 0.2).max(1.0);
    for &(c, r) in targets {
        let near = 1.0 - ((c - from.0).length() - reach).abs() / band;
        if near > 0.0 {
            let stroke = Stroke::new(width * 1.5, color.gamma_multiply(near * alpha));
            painter.circle_stroke(c, r.max(4.0) * (1.1 + 0.3 * near), stroke);
        }
    }
}

/// Reduce motion: the whole beam in place, fading; a close's target ringed, never burst.
fn still(
    painter: &Painter,
    kind: BeamKind,
    t: f32,
    [a, b]: [Pos2; 2],
    to: End,
    color: Color32,
    width: f32,
) {
    let alpha = 1.0 - t;
    match kind {
        BeamKind::DryKill | BeamKind::Spawn => {
            dashed(painter, [a, b], color.gamma_multiply(0.45 * alpha), width);
        }
        BeamKind::Kill => {
            painter.line_segment(
                [a, b],
                Stroke::new(width * 2.0, color.gamma_multiply(alpha)),
            );
        }
        _ => {
            painter.line_segment(
                [a, b],
                Stroke::new(width, color.gamma_multiply(0.6 * alpha)),
            );
        }
    }
    if matches!(kind, BeamKind::Kill | BeamKind::Spawn) {
        let stroke = Stroke::new(width * 1.5, color.gamma_multiply(alpha));
        painter.circle_stroke(to.0, to.1.max(4.0) * 1.3, stroke);
    }
}

fn dashed(painter: &Painter, [a, b]: [Pos2; 2], color: Color32, width: f32) {
    let dash = (width * 4.0).max(4.0);
    painter.extend(Shape::dashed_line(
        &[a, b],
        Stroke::new(width, color),
        dash,
        dash,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeId;
    use crate::traffic::Endpoint;

    fn beam(kind: BeamKind, targets: &[&str]) -> Beam {
        Beam {
            from: Endpoint::Node(NodeId::king()),
            targets: targets
                .iter()
                .map(|t| Endpoint::Node(NodeId((*t).into())))
                .collect(),
            kind,
        }
    }

    #[test]
    fn parallel_reads_merge_into_one_fan() {
        let mut beams = Beams::default();
        for (i, t) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            beams.push(beam(BeamKind::Read, &[t]), i as f64 * 0.02, false);
        }
        let lit = beams.tick(0.1);
        assert_eq!(lit.len(), 1);
        assert_eq!(lit[0].beam.targets.len(), 5);
    }

    #[test]
    fn a_repeat_while_showing_is_dropped_and_kinds_stay_apart() {
        let mut beams = Beams::default();
        beams.push(beam(BeamKind::Read, &["a"]), 0.0, false);
        beams.tick(0.0);
        beams.push(beam(BeamKind::Read, &["a"]), 0.5, false);
        beams.push(beam(BeamKind::Kill, &["a"]), 0.5, false);
        let lit = beams.tick(0.5);
        assert_eq!(lit.len(), 2);
        // After the first has gone, the same read shows again.
        beams.push(beam(BeamKind::Read, &["a"]), 1.0, false);
        assert_eq!(beams.tick(1.0).len(), 2);
    }

    #[test]
    fn beams_are_short_capped_and_drop_when_stale() {
        let mut beams = Beams::default();
        for i in 0..MAX_BEAMS + 3 {
            let kind = [BeamKind::Read, BeamKind::Kill, BeamKind::Spawn][i % 3];
            let mut b = beam(kind, &["a"]);
            b.from = Endpoint::Node(NodeId(format!("caller{i}")));
            beams.push(b, 0.0, false);
        }
        assert_eq!(beams.tick(0.0).len(), MAX_BEAMS);
        assert!(beams.tick(LIFE_S).is_empty());
        beams.push(beam(BeamKind::Read, &["a"]), 10.0, true);
        assert!(beams.tick(10.0 + STALE_S).is_empty());
    }

    #[test]
    fn rims_leave_out_the_nodes_themselves() {
        let seg = rim_to_rim((Pos2::new(0.0, 0.0), 2.0), (Pos2::new(10.0, 0.0), 3.0)).unwrap();
        assert_eq!(seg, [Pos2::new(2.0, 0.0), Pos2::new(7.0, 0.0)]);
        assert!(rim_to_rim((Pos2::ZERO, 5.0), (Pos2::new(6.0, 0.0), 5.0)).is_none());
    }
}

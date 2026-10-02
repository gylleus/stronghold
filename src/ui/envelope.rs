//! Messages in flight: an envelope (or, for a nudge or a status, a small ping) travels from
//! sender to receiver and pulses on arrival. With the system's Reduce motion on, the path
//! between them lights up briefly instead. Shared by the graph and the ant farm, which
//! supply the screen positions and the path.
//!
//! Bursts stay calm: a repeat of a message still in the air adds to its count (`×3`) rather
//! than flying again, starts are staggered, and at most [`MAX_FLIGHTS`] fly at once.

use eframe::egui::{self, Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2, vec2};

use crate::traffic::{Flight, MessageKind};
use crate::ui::ant_farm::motion;

/// From leaving the sender to reaching the receiver.
pub const TRAVEL_S: f64 = 1.2;
/// The arrival pulse.
pub const ARRIVE_S: f64 = 0.45;
/// The path's glow when motion is reduced.
pub const STILL_S: f64 = 1.0;
/// A message repeating one still in the air within this long joins it.
pub const MERGE_S: f64 = 2.0;
/// Gap between consecutive starts.
pub const STAGGER_S: f64 = 0.15;
/// Messages queued longer than this before a frame drew them (their view was not shown) are
/// dropped.
pub const STALE_S: f64 = 3.0;
pub const MAX_FLIGHTS: usize = 6;
/// Opacity of the line between sender and receiver.
const RAIL_ALPHA: f32 = 0.45;

/// How a kind of message looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub color: Color32,
    /// An envelope; otherwise a ping.
    pub envelope: bool,
    pub mark: Mark,
    /// The trail is dotted (relays from outside) rather than solid.
    pub dotted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    None,
    Question,
    Check,
}

pub fn look(kind: MessageKind) -> Look {
    let rgb = Color32::from_rgb;
    let (color, envelope, mark, dotted) = match kind {
        MessageKind::Instruction => (rgb(0xff, 0xd5, 0x4a), true, Mark::None, false),
        MessageKind::Message => (rgb(0x9c, 0xc8, 0xff), true, Mark::None, false),
        MessageKind::Ask => (rgb(0xff, 0x8a, 0x4c), true, Mark::Question, false),
        MessageKind::MergeReady => (rgb(0x8f, 0xd0, 0x5a), true, Mark::Check, false),
        MessageKind::Relay => (rgb(0xc3, 0x9b, 0xff), true, Mark::None, true),
        MessageKind::Nudge => (rgb(0xff, 0x6f, 0xa8), false, Mark::None, false),
        MessageKind::Status => (rgb(0x6f, 0xe3, 0xff), false, Mark::None, false),
    };
    Look {
        color,
        envelope,
        mark,
        dotted,
    }
}

/// One message on its way.
#[derive(Debug, Clone, PartialEq)]
pub struct InFlight {
    pub flight: Flight,
    /// When it was queued, and when it leaves (`None` until a frame starts it).
    pub queued_s: f64,
    pub start_s: Option<f64>,
    /// Messages it stands for.
    pub count: u32,
    /// Reduce motion: glow the path instead of flying.
    pub still: bool,
}

/// Where a message is this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    /// `t` (0..1, eased) of the way along.
    Travel(f32),
    /// `t` (0..1) into the arrival pulse.
    Arrive(f32),
    /// Reduce motion: the path's glow, fading from 1 to 0.
    Glow(f32),
}

impl InFlight {
    fn end_s(&self) -> f64 {
        if self.still {
            STILL_S
        } else {
            TRAVEL_S + ARRIVE_S
        }
    }

    pub fn phase(&self, now_s: f64) -> Option<Phase> {
        let t = now_s - self.start_s?;
        if !(0.0..self.end_s()).contains(&t) {
            return None;
        }
        Some(if self.still {
            Phase::Glow(1.0 - (t / STILL_S) as f32)
        } else if t < TRAVEL_S {
            Phase::Travel(motion::ease((t / TRAVEL_S) as f32))
        } else {
            Phase::Arrive(((t - TRAVEL_S) / ARRIVE_S) as f32)
        })
    }
}

/// The messages a view is drawing.
#[derive(Debug, Default)]
pub struct Flights {
    flights: Vec<InFlight>,
}

impl Flights {
    /// Queues `flight`, asked for at `now_s`; a repeat of one not yet arrived joins it.
    pub fn push(&mut self, flight: Flight, now_s: f64, still: bool) {
        let repeat = self.flights.iter_mut().find(|f| {
            f.flight == flight
                && now_s - f.queued_s < MERGE_S
                && f.start_s.is_none_or(|s| now_s - s < TRAVEL_S)
        });
        if let Some(f) = repeat {
            f.count += 1;
            return;
        }
        if self.flights.len() >= MAX_FLIGHTS {
            log::debug!("envelope: {} flights in the air, dropped one", MAX_FLIGHTS);
            return;
        }
        self.flights.push(InFlight {
            flight,
            queued_s: now_s,
            start_s: None,
            count: 1,
            still,
        });
    }

    /// Starts queued messages (staggered), drops stale and finished ones, and returns those in
    /// the air at `now_s`.
    pub fn tick(&mut self, now_s: f64) -> &[InFlight] {
        self.flights
            .retain(|f| f.start_s.is_some() || now_s - f.queued_s < STALE_S);
        let mut next = self
            .flights
            .iter()
            .filter_map(|f| f.start_s)
            .fold(f64::MIN, f64::max)
            + STAGGER_S;
        for f in self.flights.iter_mut().filter(|f| f.start_s.is_none()) {
            let start = now_s.max(next);
            f.start_s = Some(start);
            next = start + STAGGER_S;
        }
        self.flights
            .retain(|f| f.start_s.is_some_and(|s| now_s < s + f.end_s()));
        &self.flights
    }

    pub fn is_empty(&self) -> bool {
        self.flights.is_empty()
    }
}

/// Draws `f` along `path` (screen points, sender first) at `now_s`; `size` is the envelope's
/// width.
pub fn paint(painter: &Painter, f: &InFlight, path: &[Pos2], size: f32, now_s: f64) {
    let (Some(phase), Some(&end)) = (f.phase(now_s), path.last()) else {
        return;
    };
    let look = look(f.flight.kind);
    match phase {
        Phase::Glow(alpha) => {
            trail(painter, path, 0.0, 1.0, look, size, alpha * 0.8);
            badge(painter, end, size, look, alpha, f.count);
        }
        Phase::Travel(t) => {
            // The line fades in over the first stretch, so it doesn't pop.
            rail(painter, path, look, size, RAIL_ALPHA * (t / 0.15).min(1.0));
            trail(painter, path, (t - 0.25).max(0.0), t, look, size, 0.6);
            let (at, _) = motion::along(path, t);
            badge(painter, at, size, look, 1.0, f.count);
        }
        Phase::Arrive(t) => {
            let r = size * (0.4 + 0.9 * t);
            let fade = 1.0 - t;
            rail(painter, path, look, size, RAIL_ALPHA * fade);
            painter.circle_stroke(
                end,
                r,
                Stroke::new(size * 0.12, look.color.gamma_multiply(fade)),
            );
            painter.circle_filled(end, size * 0.25 * fade, look.color.gamma_multiply(fade));
        }
    }
}

/// The whole path from sender to receiver, the line the envelope rides: faint, dotted for
/// relays, with a dot at each end.
fn rail(painter: &Painter, path: &[Pos2], look: Look, size: f32, alpha: f32) {
    let (Some(&start), Some(&end)) = (path.first(), path.last()) else {
        return;
    };
    let color = look.color.gamma_multiply(alpha);
    let width = (size * 0.08).max(1.0);
    if look.dotted {
        let gap = size * 0.4;
        let steps = (motion::path_length(path) / gap).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let (p, _) = motion::along(path, i as f32 / steps as f32);
            painter.circle_filled(p, width * 0.8, color);
        }
    } else {
        painter.add(egui::Shape::line(path.to_vec(), Stroke::new(width, color)));
    }
    for p in [start, end] {
        painter.circle_filled(p, width * 1.8, color);
    }
}

/// The stretch of `path` from `t0` to `t1` (fractions of its length), brighter towards `t1`.
fn trail(painter: &Painter, path: &[Pos2], t0: f32, t1: f32, look: Look, size: f32, alpha: f32) {
    const STEPS: usize = 12;
    let width = (size * 0.15).max(1.5);
    let points: Vec<Pos2> = (0..=STEPS)
        .map(|i| motion::along(path, t0 + (t1 - t0) * i as f32 / STEPS as f32).0)
        .collect();
    for (i, w) in points.windows(2).enumerate() {
        let a = alpha * (i + 1) as f32 / STEPS as f32;
        let color = look.color.gamma_multiply(a);
        if look.dotted {
            if i % 2 == 0 {
                painter.circle_filled(w[1], width, color);
            }
        } else {
            painter.line_segment([w[0], w[1]], Stroke::new(width, color));
        }
    }
}

/// The envelope or ping at `at`, with its count when it stands for several messages.
fn badge(painter: &Painter, at: Pos2, size: f32, look: Look, alpha: f32, count: u32) {
    let ink = Color32::from_rgb(0x2a, 0x1c, 0x10).gamma_multiply(alpha);
    let color = look.color.gamma_multiply(alpha);
    painter.circle_filled(at, size * 0.85, look.color.gamma_multiply(alpha * 0.18));
    if !look.envelope {
        painter.circle_filled(at, size * 0.3, color);
        painter.circle_stroke(at, size * 0.45, Stroke::new(size * 0.08, color));
    } else {
        let body = Rect::from_center_size(at, vec2(size, size * 0.66));
        let line = Stroke::new((size * 0.08).max(1.0), ink);
        painter.rect_filled(body, size * 0.08, color);
        painter.rect_stroke(body, size * 0.08, line, egui::StrokeKind::Inside);
        let fold = body.center() + vec2(0.0, size * 0.04);
        painter.line_segment([body.left_top(), fold], line);
        painter.line_segment([body.right_top(), fold], line);
        let corner = body.right_top() + vec2(size * 0.1, -size * 0.1);
        match look.mark {
            Mark::None => {}
            Mark::Question => {
                painter.circle_filled(corner, size * 0.24, ink);
                painter.text(
                    corner,
                    egui::Align2::CENTER_CENTER,
                    "?",
                    FontId::proportional(size * 0.4),
                    color,
                );
            }
            Mark::Check => {
                painter.circle_filled(corner, size * 0.24, ink);
                let s = size * 0.12;
                let check = Stroke::new((size * 0.07).max(1.0), color);
                let low = corner + vec2(-s * 0.3, s * 0.7);
                painter.line_segment([corner + vec2(-s, 0.0), low], check);
                painter.line_segment([low, corner + vec2(s, -s)], check);
            }
        }
    }
    if count > 1 {
        painter.text(
            at + Vec2::new(size * 0.55, size * 0.35),
            egui::Align2::LEFT_TOP,
            format!("×{count}"),
            FontId::proportional((size * 0.55).max(9.0)),
            color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeId;
    use crate::traffic::Endpoint;

    fn flight(to: &str, kind: MessageKind) -> Flight {
        Flight {
            from: Endpoint::Node(NodeId::queen()),
            to: Endpoint::Node(NodeId::terminal(to)),
            kind,
        }
    }

    #[test]
    fn repeats_in_the_air_join_one_envelope() {
        let mut fl = Flights::default();
        fl.push(flight("a", MessageKind::Ask), 0.0, false);
        fl.push(flight("a", MessageKind::Ask), 0.5, false);
        fl.push(flight("a", MessageKind::Status), 0.5, false);
        let air = fl.tick(0.5);
        assert_eq!(air.len(), 2);
        assert_eq!(air[0].count, 2);
        // After it arrives, the same message flies anew.
        fl.push(flight("a", MessageKind::Ask), 2.5, false);
        let air = fl.tick(2.5);
        assert_eq!((air.len(), air[0].count), (1, 1));
    }

    #[test]
    fn starts_are_staggered_and_capped() {
        let mut fl = Flights::default();
        for i in 0..10 {
            fl.push(flight(&format!("t{i}"), MessageKind::Message), 0.0, false);
        }
        let air = fl.tick(0.0);
        assert_eq!(air.len(), MAX_FLIGHTS);
        let starts: Vec<f64> = air.iter().map(|f| f.start_s.unwrap()).collect();
        assert_eq!(starts[0], 0.0);
        assert!((starts[1] - STAGGER_S).abs() < 1e-9);
        assert!(air[1].phase(0.0).is_none(), "not left yet");
    }

    #[test]
    fn messages_queued_while_hidden_are_dropped() {
        let mut fl = Flights::default();
        fl.push(flight("a", MessageKind::Message), 0.0, false);
        assert!(fl.tick(STALE_S + 1.0).is_empty());
    }

    #[test]
    fn phases_travel_then_arrive_then_end() {
        let mut fl = Flights::default();
        fl.push(flight("a", MessageKind::Message), 0.0, false);
        let f = fl.tick(0.0)[0].clone();
        assert!(matches!(f.phase(0.6), Some(Phase::Travel(t)) if t > 0.4 && t < 0.6));
        assert!(matches!(f.phase(TRAVEL_S + 0.1), Some(Phase::Arrive(_))));
        assert_eq!(f.phase(TRAVEL_S + ARRIVE_S + 0.01), None);
        assert!(fl.tick(TRAVEL_S + ARRIVE_S + 0.01).is_empty());
    }

    #[test]
    fn reduced_motion_glows_the_path() {
        let mut fl = Flights::default();
        fl.push(flight("a", MessageKind::Message), 0.0, true);
        let f = fl.tick(0.0)[0].clone();
        assert_eq!(f.phase(0.0), Some(Phase::Glow(1.0)));
        assert_eq!(f.phase(STILL_S), None);
    }

    #[test]
    fn kinds_look_different() {
        let kinds = [
            MessageKind::Instruction,
            MessageKind::Message,
            MessageKind::Ask,
            MessageKind::MergeReady,
            MessageKind::Relay,
            MessageKind::Nudge,
            MessageKind::Status,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(look(*a), look(*b), "{a:?} vs {b:?}");
            }
        }
    }
}

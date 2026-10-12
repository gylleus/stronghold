//! Message flight timing and coalescing. The mine draws these as sealed scrolls in
//! `ant_farm::mail`; reduced motion lights the route in place and fades.
//!
//! Bursts stay calm: a repeat of a message still in the air adds to its count (`×3`) rather
//! than flying again, starts are staggered, and at most [`MAX_FLIGHTS`] fly at once.

use crate::traffic::Flight;
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
/// One message on its way through the mine.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeId;
    use crate::traffic::{Endpoint, MessageKind};

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
}

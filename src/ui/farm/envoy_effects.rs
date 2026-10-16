//! A short local flash for arrivals/drafts/sends and the hold's shared beam, sent upward.

use eframe::egui::{Painter, pos2};

use super::{Placed, layout::Scene, sprites::Sprites, world::GOLD};
use crate::{model::NodeKind, ui::beam};

const PUNCH_S: f64 = 0.24;
const QUIET_S: f64 = 1.5;

#[derive(Default)]
pub(super) struct Effects {
    punch_s: Option<f64>,
    send_s: Option<f64>,
    last_activity_s: Option<f64>,
    last_send_s: Option<f64>,
}

impl Effects {
    pub fn trigger(&mut self, sent: bool, now_s: f64) {
        // A stream of bulk updates is one burst, with no queued animation at its tail.
        if self.last_activity_s.is_none_or(|s| now_s - s >= QUIET_S) {
            self.punch_s = Some(now_s);
        }
        self.last_activity_s = Some(now_s);
        if sent {
            if self.last_send_s.is_none_or(|s| now_s - s >= QUIET_S) {
                self.send_s = Some(now_s);
                if Self::progress(self.punch_s, now_s, PUNCH_S).is_none() {
                    self.punch_s = Some(now_s);
                }
            }
            self.last_send_s = Some(now_s);
        }
    }

    fn progress(start: Option<f64>, now_s: f64, life: f64) -> Option<f32> {
        let t = (now_s - start?) / life;
        (0.0..1.0).contains(&t).then_some(t as f32)
    }

    pub fn animating(&self, now_s: f64) -> bool {
        Self::progress(self.punch_s, now_s, PUNCH_S).is_some()
            || Self::progress(self.send_s, now_s, beam::LIFE_S).is_some()
    }

    pub fn paint(
        &self,
        painter: &Painter,
        sprites: &Sprites,
        scene: &Scene,
        placed: &[Placed],
        now_s: f64,
        still: bool,
    ) {
        let Some((dwarf, at)) = scene
            .dwarves
            .iter()
            .zip(placed)
            .find(|(d, _)| d.kind == NodeKind::Envoy)
        else {
            return;
        };
        if let Some(t) = Self::progress(self.punch_s, now_s, PUNCH_S) {
            // A fixed, small halo leaves the camera and click targets steady, including
            // with Reduce Motion. Use the same gold glow as the receiver's receipt cue.
            sprites.glow(
                painter,
                at.pos,
                dwarf.size * 0.9,
                GOLD,
                0.45 * (1.0 - t).powi(2),
            );
        }
        if let Some(t) = Self::progress(self.send_s, now_s, beam::LIFE_S)
            && let Some(room) = scene.comms
        {
            beam::paint_outgoing(
                painter,
                t,
                still,
                (at.pos, scene.cell() * 0.6),
                (pos2(at.pos.x, room.feed().y), 0.0),
                scene.cell().clamp(14.0, 28.0),
                GOLD,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_do_not_restart_or_queue_effects_and_sends_survive_arrival_bursts() {
        let mut effects = Effects::default();
        assert!(!effects.animating(0.0));
        effects.trigger(false, 1.0);
        effects.trigger(true, 1.1);
        assert_eq!(effects.punch_s, Some(1.0));
        assert_eq!(effects.send_s, Some(1.1));
        for i in 2..30 {
            effects.trigger(true, 1.0 + i as f64 * 0.1);
        }
        assert!(!effects.animating(4.0));
        assert!(!effects.animating(8.0));
        effects.trigger(true, 8.0);
        assert!(effects.animating(8.1));
        assert!(!effects.animating(8.81));
        effects.trigger(false, 10.0);
        effects.trigger(true, 10.5);
        assert_eq!(effects.punch_s, Some(10.5));
    }
}

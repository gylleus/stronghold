//! The receipt cue shared by the farm: when messages reach the Envoy, the rune stone shakes
//! and sends out waves, its orbiting runes spin up, and the priest casts.

use std::f32::consts::TAU;
use std::time::Duration;

use eframe::egui;

/// The stone shakes for the first half second.
const SHAKE_MS: i64 = 500;
/// The whole cue; the scroll reaches the priest well inside it.
const DURATION_MS: i64 = 2_400;
/// The priest casts from the start until the scroll has landed.
const CAST_S: f32 = 1.9;
/// Waves leave the stone this far apart, and each spreads for this long.
const WAVE_GAP_S: f32 = 0.3;
const WAVE_S: f32 = 1.3;
const WAVES: usize = 3;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ReceiptCue {
    /// The stone's shake, in points.
    pub offset: f32,
    /// The stone's gold flare, 1 at receipt fading to 0.
    pub highlight: f32,
    /// Seconds since receipt while the cue runs.
    elapsed: Option<f32>,
    /// Reduce motion: no shake, waves or spin.
    still: bool,
}

impl ReceiptCue {
    pub fn at(started_ms: Option<i64>, now_ms: i64, reduced_motion: bool) -> Self {
        let Some(elapsed) = started_ms.map(|start| now_ms - start) else {
            return Self::default();
        };
        if !(0..DURATION_MS).contains(&elapsed) {
            return Self::default();
        }
        let shake = (1.0 - elapsed as f32 / SHAKE_MS as f32).max(0.0);
        let progress = elapsed as f32 / SHAKE_MS as f32;
        Self {
            offset: if reduced_motion {
                0.0
            } else {
                4.0 * shake * shake * (progress * TAU * 4.0).sin()
            },
            highlight: (1.0 - elapsed as f32 / DURATION_MS as f32).powi(2),
            elapsed: Some(elapsed as f32 / 1000.0),
            still: reduced_motion,
        }
    }

    /// Whether the priest is casting.
    pub fn casting(self) -> bool {
        self.elapsed.is_some_and(|s| s < CAST_S)
    }

    /// How far each wave leaving the stone has spread, 0..1; none with reduced motion.
    pub fn waves(self) -> impl Iterator<Item = f32> {
        let elapsed = self.elapsed.filter(|_| !self.still);
        (0..WAVES).filter_map(move |i| {
            let u = (elapsed? - i as f32 * WAVE_GAP_S) / WAVE_S;
            (0.0..1.0).contains(&u).then_some(u)
        })
    }

    /// The extra turn of the stone's orbiting runes: one more lap over the cue, easing in
    /// and out so it joins their calm orbit on both ends.
    pub fn spin(self) -> f32 {
        match self.elapsed.filter(|_| !self.still) {
            Some(s) => {
                let k = s / (DURATION_MS as f32 / 1000.0);
                TAU * k * k * (3.0 - 2.0 * k)
            }
            None => 0.0,
        }
    }

    pub fn repaint(self, ctx: &egui::Context) {
        if self.elapsed.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_cue_is_brief_damped_and_motion_free_when_reduced() {
        assert_eq!(ReceiptCue::at(None, 100, false), ReceiptCue::default());
        for reduced in [false, true] {
            for now in [999, 1_000 + DURATION_MS, 10_000] {
                assert_eq!(
                    ReceiptCue::at(Some(1_000), now, reduced),
                    ReceiptCue::default()
                );
            }
            for elapsed in 0..DURATION_MS {
                let cue = ReceiptCue::at(Some(1_000), 1_000 + elapsed, reduced);
                assert!(cue.elapsed.is_some());
                assert!(cue.highlight > 0.0);
                let envelope = 4.0 * (1.0 - elapsed as f32 / SHAKE_MS as f32).max(0.0).powi(2);
                assert!(cue.offset.abs() <= envelope);
                if reduced {
                    assert_eq!(cue.offset, 0.0);
                    assert_eq!(cue.waves().count(), 0);
                    assert_eq!(cue.spin(), 0.0);
                }
            }
        }
        assert!(ReceiptCue::at(Some(0), 31, false).offset > 3.0);
        assert!(ReceiptCue::at(Some(0), 94, false).offset < -2.0);
        assert_eq!(ReceiptCue::at(Some(0), 600, false).offset, 0.0);
    }

    #[test]
    fn priest_casts_while_waves_spread_then_all_settles() {
        let cue = |ms| ReceiptCue::at(Some(0), ms, false);
        assert!(cue(0).casting() && cue(1_800).casting());
        assert!(!cue(2_000).casting() && !ReceiptCue::default().casting());
        // Waves leave one after another and are gone before the cue ends.
        assert_eq!(cue(0).waves().count(), 1);
        assert_eq!(cue(700).waves().count(), 3);
        assert_eq!(cue(2_300).waves().count(), 0);
        // The extra lap starts and ends at rest, so the orbit never jumps.
        assert_eq!(cue(0).spin(), 0.0);
        assert!((cue(DURATION_MS - 1).spin() - TAU).abs() < 0.01);
        assert!(ReceiptCue::default().spin() == 0.0);
        // Reduced motion still casts and flares.
        let still = ReceiptCue::at(Some(0), 300, true);
        assert!(still.casting() && still.highlight > 0.0);
    }
}

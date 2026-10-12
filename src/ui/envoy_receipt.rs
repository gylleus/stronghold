//! A brief receipt cue shared by the farm.

use std::time::Duration;

use eframe::egui;

const DURATION_MS: i64 = 500;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ReceiptCue {
    pub offset: f32,
    pub highlight: f32,
    active: bool,
}

impl ReceiptCue {
    pub fn at(started_ms: Option<i64>, now_ms: i64, reduced_motion: bool) -> Self {
        let Some(elapsed) = started_ms.map(|start| now_ms - start) else {
            return Self::default();
        };
        if !(0..DURATION_MS).contains(&elapsed) {
            return Self::default();
        }
        let progress = elapsed as f32 / DURATION_MS as f32;
        let fade = 1.0 - progress;
        Self {
            offset: if reduced_motion {
                0.0
            } else {
                4.0 * fade * fade * (progress * std::f32::consts::TAU * 4.0).sin()
            },
            highlight: fade,
            active: true,
        }
    }

    pub fn repaint(self, ctx: &egui::Context) {
        if self.active {
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
            for now in [999, 1_500, 10_000] {
                assert_eq!(
                    ReceiptCue::at(Some(1_000), now, reduced),
                    ReceiptCue::default()
                );
            }
            for elapsed in 0..DURATION_MS {
                let cue = ReceiptCue::at(Some(1_000), 1_000 + elapsed, reduced);
                assert!(cue.active);
                if reduced {
                    assert_eq!(cue.offset, 0.0);
                    assert!(cue.highlight > 0.0);
                } else {
                    let envelope = 4.0 * (1.0 - elapsed as f32 / DURATION_MS as f32).powi(2);
                    assert!(cue.offset.abs() <= envelope);
                    assert!(cue.highlight > 0.0);
                }
            }
        }
        assert!(ReceiptCue::at(Some(0), 31, false).offset > 3.0);
        assert!(ReceiptCue::at(Some(0), 94, false).offset < -2.0);
    }
}

//! Day and night by the local clock, for the farm's sky. The app's panels keep following the system theme.
//!
//! [`DAWN_H`], [`DUSK_H`] and [`FADE_H`] are the knobs. `MYRSTACK_HOUR` (a local hour such
//! as `21.5`) fakes the clock, to check both looks.

use std::time::Duration;

use eframe::egui::{self, Color32};

/// Local hour the day begins, in the middle of the fade from night.
pub const DAWN_H: f32 = 6.0;
/// Local hour the night begins, in the middle of the fade from day.
pub const DUSK_H: f32 = 19.0;
/// How long each fade lasts, in hours.
pub const FADE_H: f32 = 0.75;

/// How often to look at the clock while a fade runs.
const FADE_STEP: Duration = Duration::from_secs(15);
/// The longest wait between looks otherwise, so a slept laptop catches up.
const IDLE_STEP: Duration = Duration::from_secs(60);

/// How much it is day at local hour `hour` (0–24): 1 by day, 0 by night, eased between in
/// the fades around [`DAWN_H`] and [`DUSK_H`].
pub fn daylight_at(hour: f32) -> f32 {
    let half = FADE_H / 2.0;
    let rise = smoothstep((hour - (DAWN_H - half)) / FADE_H);
    let set = smoothstep((hour - (DUSK_H - half)) / FADE_H);
    (rise - set).clamp(0.0, 1.0)
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The local hour now, or `MYRSTACK_HOUR` when set.
pub fn local_hour() -> f32 {
    if let Some(hour) = std::env::var("MYRSTACK_HOUR")
        .ok()
        .and_then(|h| h.parse::<f32>().ok())
    {
        return hour.rem_euclid(24.0);
    }
    let now = jiff::Zoned::now();
    now.hour() as f32 + now.minute() as f32 / 60.0 + now.second() as f32 / 3600.0
}

/// How much it is day now; see [`daylight_at`].
pub fn daylight() -> f32 {
    daylight_at(local_hour())
}

/// When to look at the clock again from `hour`: often during a fade, at the next fade's
/// start or [`IDLE_STEP`] otherwise.
pub fn recheck_after(hour: f32) -> Duration {
    let half = FADE_H / 2.0;
    let mut until = f32::INFINITY;
    for edge in [DAWN_H, DUSK_H] {
        let start = (edge - half - hour).rem_euclid(24.0);
        if start > 24.0 - FADE_H {
            return FADE_STEP;
        }
        until = until.min(start);
    }
    Duration::from_secs_f32(until * 3600.0).clamp(FADE_STEP, IDLE_STEP)
}

/// Repaints `ctx` when the daylight may next have changed.
pub fn schedule_repaint(ctx: &egui::Context) {
    ctx.request_repaint_after(recheck_after(local_hour()));
}

/// Mixes `night` into `day` by `daylight`.
pub fn mix(night: Color32, day: Color32, daylight: f32) -> Color32 {
    night.lerp_to_gamma(day, daylight)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_and_night_by_the_clock() {
        for hour in [0.0, 3.0, 5.0, 19.5, 22.0, 23.99] {
            assert_eq!(daylight_at(hour), 0.0, "night at {hour}");
        }
        for hour in [6.5, 9.0, 12.0, 18.5] {
            assert_eq!(daylight_at(hour), 1.0, "day at {hour}");
        }
        assert!((daylight_at(DAWN_H) - 0.5).abs() < 1e-4);
        assert!((daylight_at(DUSK_H) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn fades_are_monotone() {
        let steps = |from: f32| (0..=20).map(move |i| daylight_at(from + i as f32 * FADE_H / 20.0));
        let dawn: Vec<f32> = steps(DAWN_H - FADE_H / 2.0).collect();
        let dusk: Vec<f32> = steps(DUSK_H - FADE_H / 2.0).collect();
        assert!(dawn.windows(2).all(|w| w[0] <= w[1]));
        assert!(dusk.windows(2).all(|w| w[0] >= w[1]));
    }

    #[test]
    fn rechecks_often_in_a_fade_and_rarely_otherwise() {
        assert_eq!(recheck_after(DAWN_H), FADE_STEP);
        assert_eq!(recheck_after(DUSK_H + 0.1), FADE_STEP);
        assert_eq!(recheck_after(12.0), IDLE_STEP);
        assert_eq!(recheck_after(2.0), IDLE_STEP);
        // Just before the dawn fade, wake at its start.
        let start = DAWN_H - FADE_H / 2.0;
        let soon = recheck_after(start - 30.0 / 3600.0);
        assert!(
            soon >= FADE_STEP && soon <= Duration::from_secs(31),
            "{soon:?}"
        );
    }
}

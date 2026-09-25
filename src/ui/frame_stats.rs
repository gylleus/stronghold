//! Frames-per-second counter that logs, at debug level, how often the UI redraws and why.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use eframe::egui;

/// How often the counter logs.
const REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// Counts frames and the repaint requests that caused them; see [`FrameStats::tick`].
#[derive(Debug)]
pub struct FrameStats {
    since: Instant,
    /// `App::logic` calls; these also run while the window is hidden and nothing is drawn.
    ticks: u32,
    /// egui passes (drawn frames) completed when the interval started.
    passes_at_start: Option<u64>,
    /// `file:line` of each repaint request → number of ticks it caused.
    causes: BTreeMap<String, u32>,
}

impl Default for FrameStats {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

impl FrameStats {
    pub fn new(now: Instant) -> Self {
        Self {
            since: now,
            ticks: 0,
            passes_at_start: None,
            causes: BTreeMap::new(),
        }
    }

    /// Call once per `App::logic`. Logs drawn frames per second, logic ticks per second and the
    /// top repaint causes every [`REPORT_INTERVAL`] when debug logging is on.
    pub fn tick(&mut self, ctx: &egui::Context) {
        if !log::log_enabled!(log::Level::Debug) {
            return;
        }
        let causes = ctx
            .repaint_causes()
            .into_iter()
            .map(|c| format!("{}:{}", c.file, c.line));
        if let Some(report) = self.record(Instant::now(), ctx.cumulative_pass_nr(), causes) {
            log::debug!("{report}");
        }
    }

    /// Adds a tick at `passes` completed egui passes; returns the report line once
    /// [`REPORT_INTERVAL`] has passed.
    pub fn record(
        &mut self,
        now: Instant,
        passes: u64,
        causes: impl IntoIterator<Item = String>,
    ) -> Option<String> {
        let start = *self.passes_at_start.get_or_insert(passes);
        self.ticks += 1;
        for cause in causes {
            *self.causes.entry(cause).or_default() += 1;
        }
        let elapsed = now.duration_since(self.since);
        if elapsed < REPORT_INTERVAL {
            return None;
        }
        let secs = elapsed.as_secs_f64();
        let fps = passes.saturating_sub(start) as f64 / secs;
        let ticks = f64::from(self.ticks) / secs;
        let mut top: Vec<(String, u32)> = std::mem::take(&mut self.causes).into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let top: Vec<String> = top
            .into_iter()
            .take(5)
            .map(|(cause, n)| format!("{cause}×{n}"))
            .collect();
        self.ticks = 0;
        self.passes_at_start = Some(passes);
        self.since = now;
        Some(format!(
            "fps {fps:.1} (logic {ticks:.1}/s); repaint causes: {}",
            top.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_fps_and_top_causes_every_interval() {
        let t0 = Instant::now();
        let mut stats = FrameStats::new(t0);
        for i in 0..9 {
            let at = t0 + Duration::from_millis(500 * i);
            assert_eq!(stats.record(at, 100 + i, ["a.rs:1".to_owned()]), None);
        }
        let report = stats
            .record(t0 + REPORT_INTERVAL, 105, ["b.rs:2".to_owned()])
            .expect("report after the interval");
        assert_eq!(
            report,
            "fps 1.0 (logic 2.0/s); repaint causes: a.rs:1×9, b.rs:2×1"
        );
        // The counter starts over.
        assert_eq!(stats.record(t0 + REPORT_INTERVAL, 105, []), None);
    }
}

//! One deadline per job and a persisted record of attempts.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

/// One deadline, with no catch-up queue after sleep. Changing the setting starts a fresh interval.
#[derive(Default)]
pub struct Schedule {
    configured: Option<Option<i64>>,
    next_ms: Option<i64>,
}

impl Schedule {
    pub fn finished(&mut self, now_ms: i64) {
        self.next_ms = self
            .configured
            .flatten()
            .map(|period| now_ms.saturating_add(period));
    }

    pub fn is_due(&self, now_ms: i64) -> bool {
        self.next_ms.is_some_and(|next| now_ms >= next)
    }

    pub fn claim(
        &mut self,
        period: Option<i64>,
        now_ms: i64,
        last_ms: Option<i64>,
        busy: bool,
        run_now: bool,
    ) -> bool {
        let period = period.filter(|p| *p > 0);
        if self.configured != Some(period) {
            let anchor = if self.configured.is_none() {
                last_ms.unwrap_or(now_ms).min(now_ms)
            } else {
                now_ms
            };
            self.next_ms = period.map(|p| anchor.saturating_add(p));
            self.configured = Some(period);
        }
        if busy || !(run_now || self.is_due(now_ms)) {
            return false;
        }
        self.next_ms = period.map(|p| now_ms.saturating_add(p));
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub at_ms: i64,
    pub summary: String,
}

impl Run {
    pub fn load_last(path: &Path) -> Option<Self> {
        fs::read_to_string(path)
            .ok()?
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str(line).ok())
    }

    pub fn append(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(
            file,
            "{}",
            serde_json::to_string(self).map_err(std::io::Error::other)?
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_weekly_boundaries_and_backwards_clock() {
        for period in [86_400_000, 7 * 86_400_000] {
            let mut schedule = Schedule::default();
            assert!(!schedule.claim(Some(period), 100, None, false, false));
            assert!(!schedule.is_due(99));
            assert!(!schedule.is_due(100 + period - 1));
            assert!(schedule.claim(Some(period), 100 + period, None, false, false));
            assert!(!schedule.is_due(100 + period + 1));
        }
    }
}

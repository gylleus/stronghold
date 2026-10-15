//! Opt-in scheduling and the stricter selection used by unattended cleanup.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Plan, Removal, Verdict};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interval {
    ThirtyMinutes,
    Hour,
    #[default]
    ThreeHours,
    SixHours,
    Day,
}

impl Interval {
    pub const ALL: [Self; 5] = [
        Self::ThirtyMinutes,
        Self::Hour,
        Self::ThreeHours,
        Self::SixHours,
        Self::Day,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ThirtyMinutes => "30 min",
            Self::Hour => "1 h",
            Self::ThreeHours => "3 h",
            Self::SixHours => "6 h",
            Self::Day => "24 h",
        }
    }

    pub fn millis(self) -> i64 {
        (match self {
            Self::ThirtyMinutes => 30,
            Self::Hour => 60,
            Self::ThreeHours => 180,
            Self::SixHours => 360,
            Self::Day => 1440,
        }) * 60_000
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub interval: Interval,
}

/// Archive-required entries can be `remove` in a manual plan. Never select them here.
pub fn eligible(verdict: &Verdict) -> bool {
    verdict.auto_remove && verdict.remove && !verdict.archivable && !verdict.archive_required
}

pub fn select(plan: &Plan) -> Plan {
    Plan {
        entries: plan
            .entries
            .iter()
            .filter(|e| eligible(&e.verdict))
            .cloned()
            .collect(),
        orphans: Vec::new(),
        evaluated_at_ms: plan.evaluated_at_ms,
    }
}

pub use crate::schedule::{Run, Schedule};

impl Settings {
    pub fn period_ms(self) -> Option<i64> {
        self.enabled.then(|| self.interval.millis())
    }
}

pub fn finished_run(at_ms: i64, removals: &[Removal], error: Option<&str>) -> Run {
    let mut summary = format!(
        "Auto-cleanup {}",
        super::summary(removals).replacen("Removed", "removed", 1)
    );
    if let Some(error) = error {
        summary.push_str(&format!("; {error}"));
    }
    Run { at_ms, summary }
}

pub fn log_path() -> PathBuf {
    crate::claude::hook::data_dir().join("worktree-auto-cleanup.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktrees::PlanEntry;
    use std::fs;

    #[test]
    fn off_is_inert_but_run_now_is_an_explicit_one_off() {
        let mut schedule = Schedule::default();
        let off = Settings::default();
        for now in [0, 1_000, i64::MAX - 1] {
            assert!(!schedule.claim(off.period_ms(), now, None, false, false));
        }
        assert!(schedule.claim(off.period_ms(), 1_000, None, false, true));
        assert!(!schedule.claim(off.period_ms(), 2_000, None, false, false));
    }

    #[test]
    fn deadlines_defer_while_busy_and_never_catch_up_in_a_burst() {
        for interval in Interval::ALL {
            let settings = Settings {
                enabled: true,
                interval,
            };
            let mut schedule = Schedule::default();
            let period = interval.millis();
            assert!(!schedule.claim(settings.period_ms(), 0, None, false, false));
            assert!(!schedule.claim(settings.period_ms(), period - 1, None, false, false));
            assert!(!schedule.claim(settings.period_ms(), period, None, true, false));
            assert!(schedule.claim(settings.period_ms(), period * 10, None, false, false));
            assert!(!schedule.claim(settings.period_ms(), period * 10 + 1, None, false, false));
            assert!(schedule.claim(settings.period_ms(), period * 11, None, false, false));
        }
    }

    #[test]
    fn restart_uses_the_last_run_and_changes_reset_the_deadline() {
        let on = Settings {
            enabled: true,
            interval: Interval::Hour,
        };
        let mut schedule = Schedule::default();
        assert!(schedule.claim(on.period_ms(), 4_000_000, Some(0), false, false));
        let slower = Settings {
            interval: Interval::Day,
            ..on
        };
        assert!(!schedule.claim(slower.period_ms(), 5_000_000, Some(0), false, false));
        assert!(!schedule.claim(
            Settings::default().period_ms(),
            i64::MAX - 1,
            Some(0),
            false,
            false
        ));
        assert!(!schedule.claim(on.period_ms(), 0, Some(0), false, false));
        assert!(!schedule.claim(on.period_ms(), -100, Some(0), false, false));
        assert!(schedule.claim(on.period_ms(), on.interval.millis(), Some(0), false, false));
    }

    #[test]
    fn completion_waits_a_full_interval_even_after_a_long_run() {
        let on = Settings {
            enabled: true,
            interval: Interval::Hour,
        };
        let mut schedule = Schedule::default();
        assert!(schedule.claim(on.period_ms(), 0, None, false, true));
        schedule.finished(10_000_000);
        assert!(!schedule.claim(on.period_ms(), 10_000_001, Some(10_000_000), false, false));
        assert!(schedule.claim(
            on.period_ms(),
            10_000_000 + on.interval.millis(),
            Some(10_000_000),
            false,
            false
        ));
    }

    #[test]
    fn settings_round_trip_every_interval_and_old_files_default_off() {
        assert_eq!(
            serde_json::from_str::<Settings>("{}").unwrap(),
            Settings::default()
        );
        for interval in Interval::ALL {
            let settings = Settings {
                enabled: true,
                interval,
            };
            assert_eq!(
                serde_json::from_str::<Settings>(&serde_json::to_string(&settings).unwrap())
                    .unwrap(),
                settings
            );
        }
    }

    #[test]
    fn selection_excludes_archive_required_archivable_and_kept_entries() {
        let safe = Verdict {
            auto_remove: true,
            remove: true,
            ..Verdict::default()
        };
        let entries = [
            safe.clone(),
            Verdict {
                archive_required: true,
                ..safe.clone()
            },
            Verdict {
                archivable: true,
                ..safe.clone()
            },
            Verdict {
                remove: false,
                ..safe.clone()
            },
            Verdict {
                auto_remove: false,
                ..safe
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(i, verdict)| PlanEntry {
            id: i.to_string(),
            verdict,
            ..PlanEntry::default()
        })
        .collect();
        let plan = select(&Plan {
            entries,
            ..Plan::default()
        });
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].id, "0");
    }

    #[test]
    fn one_summary_per_run_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.jsonl");
        let run = finished_run(42, &[], None);
        run.append(&path).unwrap();
        assert_eq!(Run::load_last(&path), Some(run));
        assert_eq!(fs::read_to_string(path).unwrap().lines().count(), 1);
    }
}

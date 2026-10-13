//! How fresh the clan data is. A failed Orca call keeps the previous data, so the UI must
//! say when what it shows is old: "updated 12s ago" once it is a couple of polls late, and a
//! banner and a dimmed canvas once Orca has failed several times in a row.

use std::time::Duration;

/// Consecutive failed terminal polls after which the data counts as failing.
pub const FAILING_AFTER: u32 = 3;
/// Added to two poll intervals before data counts as stale, for a slow but working call.
const SLACK_MS: i64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Polled within about two intervals (or still waiting for the first poll).
    Fresh,
    /// Older than two intervals; `age_ms` since the last successful poll.
    Stale { age_ms: i64 },
    /// Orca failed [`FAILING_AFTER`] times in a row; `age_ms` is `None` when it never answered.
    Failing { age_ms: Option<i64>, failures: u32 },
}

impl Freshness {
    pub fn is_failing(self) -> bool {
        matches!(self, Freshness::Failing { .. })
    }
}

/// The freshness of data last fetched at `fresh_at_ms` after `failures` failed polls in a row,
/// with the terminal list polled every `interval`.
pub fn freshness(
    fresh_at_ms: Option<i64>,
    failures: u32,
    now_ms: i64,
    interval: Duration,
) -> Freshness {
    let age_ms = fresh_at_ms.map(|at| (now_ms - at).max(0));
    if failures >= FAILING_AFTER {
        return Freshness::Failing { age_ms, failures };
    }
    let limit = 2 * i64::try_from(interval.as_millis()).unwrap_or(i64::MAX / 4) + SLACK_MS;
    match age_ms {
        Some(age_ms) if age_ms > limit => Freshness::Stale { age_ms },
        _ => Freshness::Fresh,
    }
}

/// The top bar's age label: `None` while fresh.
pub fn age_label(freshness: Freshness) -> Option<String> {
    let age = |ms: i64| format!("updated {} ago", crate::ui::clan_view::format_age(ms));
    match freshness {
        Freshness::Fresh => None,
        Freshness::Stale { age_ms } => Some(age(age_ms)),
        Freshness::Failing {
            age_ms: Some(ms), ..
        } => Some(age(ms)),
        Freshness::Failing { age_ms: None, .. } => Some("no data from Orca yet".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;
    const TWO_S: Duration = Duration::from_secs(2);

    #[test]
    fn fresh_until_two_intervals_and_a_second_late() {
        assert_eq!(
            freshness(Some(NOW - 5_000), 0, NOW, TWO_S),
            Freshness::Fresh
        );
        assert_eq!(
            freshness(Some(NOW - 5_001), 1, NOW, TWO_S),
            Freshness::Stale { age_ms: 5_001 }
        );
        assert_eq!(
            freshness(None, 0, NOW, TWO_S),
            Freshness::Fresh,
            "first poll pending"
        );
        assert_eq!(freshness(Some(NOW + 50), 0, NOW, TWO_S), Freshness::Fresh);
    }

    #[test]
    fn repeated_failures_are_failing_however_recent() {
        assert_eq!(
            freshness(Some(NOW - 1_000), FAILING_AFTER, NOW, TWO_S),
            Freshness::Failing {
                age_ms: Some(1_000),
                failures: FAILING_AFTER
            }
        );
        assert!(freshness(None, 5, NOW, TWO_S).is_failing());
        assert!(!freshness(Some(NOW), FAILING_AFTER - 1, NOW, TWO_S).is_failing());
    }

    #[test]
    fn age_label_shows_only_when_late() {
        assert_eq!(age_label(Freshness::Fresh), None);
        assert_eq!(
            age_label(Freshness::Stale { age_ms: 12_000 }).as_deref(),
            Some("updated 12s ago")
        );
        assert_eq!(
            age_label(Freshness::Failing {
                age_ms: None,
                failures: 3
            })
            .as_deref(),
            Some("no data from Orca yet")
        );
    }
}

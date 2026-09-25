//! Plan quota per provider account, from `orca account list`, polled slowly off the UI thread.
//!
//! The top bar shows one chip per provider with its most constrained window, e.g.
//! "Claude 5h 62%". A window whose reset time has passed is stale (Orca has not refetched
//! it yet), so it is listed but never picked for the chip.

use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::orca::{AccountList, OrcaClient, ProviderRateLimit, RateWindow};
use crate::poller::Repaint;

/// How often `orca account list` is called.
pub const POLL_INTERVAL: Duration = Duration::from_secs(180);

/// Used share at which a chip turns amber.
pub const WARN_PERCENT: f64 = 80.0;
/// Used share at which a chip turns red.
pub const CRITICAL_PERCENT: f64 = 95.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Normal,
    Warn,
    Critical,
}

impl Severity {
    pub fn from_percent(used: f64) -> Self {
        if used >= CRITICAL_PERCENT {
            Severity::Critical
        } else if used >= WARN_PERCENT {
            Severity::Warn
        } else {
            Severity::Normal
        }
    }
}

/// One provider's quota, as last fetched.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderQuota {
    pub limit: ProviderRateLimit,
}

impl ProviderQuota {
    /// "Claude", "Codex", "OpencodeGo".
    pub fn name(&self) -> String {
        let mut chars = self.limit.key.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect()
        })
    }

    /// The window with the highest used share among those not yet past their reset.
    pub fn most_constrained(&self, now_ms: i64) -> Option<&RateWindow> {
        self.limit
            .windows
            .iter()
            .filter(|w| !is_stale(w, now_ms))
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
    }

    /// The top-bar chip, if any window is current.
    pub fn chip(&self, now_ms: i64) -> Option<Chip> {
        let window = self.most_constrained(now_ms)?;
        Some(Chip {
            text: format!(
                "{} {} {}",
                self.name(),
                window_label(window),
                format_percent(window.used_percent)
            ),
            severity: Severity::from_percent(window.used_percent),
            hover: self.hover(now_ms),
        })
    }

    /// One line per window with its reset time, then the fetch age.
    pub fn hover(&self, now_ms: i64) -> String {
        let mut lines = vec![self.name()];
        for w in &self.limit.windows {
            let mut line = format!(
                "{}: {} used",
                window_label(w),
                format_percent(w.used_percent)
            );
            if let Some(at) = w.resets_at_ms {
                line.push_str(" · ");
                line.push_str(&format_reset(at - now_ms));
                if let Some(desc) = &w.reset_description {
                    line.push_str(&format!(" ({desc})"));
                }
            }
            lines.push(line);
        }
        if let Some(error) = &self.limit.error {
            lines.push(format!("error: {error}"));
        }
        if let Some(at) = self.limit.updated_at_ms {
            lines.push(format!(
                "Orca updated {} ago",
                format_duration(now_ms.saturating_sub(at))
            ));
        }
        lines.join("\n")
    }
}

/// A top-bar chip.
#[derive(Debug, Clone, PartialEq)]
pub struct Chip {
    pub text: String,
    pub severity: Severity,
    pub hover: String,
}

/// The providers from the last good fetch, and the latest error.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Quotas {
    /// Providers that reported at least one window, Claude and Codex first.
    pub providers: Vec<ProviderQuota>,
    /// Wall-clock time of the last successful fetch (ms since the epoch); 0 before one.
    pub fetched_at_ms: i64,
    /// Error from the latest fetch, if it failed.
    pub error: Option<String>,
}

impl Quotas {
    pub fn from_account_list(list: &AccountList, now_ms: i64) -> Self {
        let mut providers: Vec<ProviderQuota> = list
            .providers()
            .into_iter()
            .filter(|p| !p.windows.is_empty())
            .map(|limit| ProviderQuota { limit })
            .collect();
        providers.sort_by_key(|p| {
            let rank = match p.limit.key.as_str() {
                "claude" => 0,
                "codex" => 1,
                _ => 2,
            };
            (rank, p.limit.key.clone())
        });
        Self {
            providers,
            fetched_at_ms: now_ms,
            error: None,
        }
    }

    pub fn chips(&self, now_ms: i64) -> Vec<Chip> {
        self.providers
            .iter()
            .filter_map(|p| p.chip(now_ms))
            .collect()
    }
}

/// True when the window's reset time has passed, so its percentage is out of date.
fn is_stale(window: &RateWindow, now_ms: i64) -> bool {
    window.resets_at_ms.is_some_and(|at| at <= now_ms)
}

/// Short window name: "5h" for Claude's session, "wk", "Fable wk", "mo", else from the length.
pub fn window_label(window: &RateWindow) -> String {
    match window.key.as_str() {
        "session" => window
            .window_minutes
            .map_or_else(|| "session".into(), format_window_minutes),
        "weekly" => "wk".into(),
        "fableWeekly" => "Fable wk".into(),
        "monthly" => "mo".into(),
        other => other.into(),
    }
}

/// 300 → "5h", 90 → "90m", 10080 → "7d".
fn format_window_minutes(minutes: u64) -> String {
    if minutes >= 24 * 60 && minutes.is_multiple_of(24 * 60) {
        format!("{}d", minutes / (24 * 60))
    } else if minutes >= 60 && minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

fn format_percent(p: f64) -> String {
    format!("{:.0}%", p.clamp(0.0, 999.0))
}

/// "resets in 2h 10m", or "reset time passed" when `in_ms` is not positive.
pub fn format_reset(in_ms: i64) -> String {
    if in_ms <= 0 {
        return "reset time passed (stale)".into();
    }
    format!("resets in {}", format_duration(in_ms))
}

/// "45s", "12m", "2h 10m", "3d 4h".
pub fn format_duration(ms: i64) -> String {
    let mins = ms.max(0) / 60_000;
    let (d, h, m) = (mins / (24 * 60), mins / 60 % 24, mins % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{}s", ms.max(0) / 1000)
    }
}

/// Where quotas come from; [`OrcaClient`] in the app, a stub in tests.
pub trait QuotaSource: Send + 'static {
    fn fetch(&self) -> Result<AccountList, String>;
}

impl QuotaSource for OrcaClient {
    fn fetch(&self) -> Result<AccountList, String> {
        self.account_list().map_err(|e| e.to_string())
    }
}

/// Background thread polling a [`QuotaSource`]. Dropping it stops and joins the thread.
pub struct QuotaPoller {
    latest: Receiver<Quotas>,
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl QuotaPoller {
    /// Fetches at once, then every `interval`.
    pub fn spawn(source: impl QuotaSource, interval: Duration, repaint: Option<Repaint>) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(2);
        let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
        let thread = std::thread::Builder::new()
            .name("quota-poller".into())
            .spawn(move || {
                let mut last = Quotas::default();
                loop {
                    let now = wall_clock_ms();
                    match source.fetch() {
                        Ok(list) => {
                            last = Quotas::from_account_list(&list, now);
                            let chips: Vec<String> =
                                last.chips(now).into_iter().map(|c| c.text).collect();
                            log::debug!("quota chips: {chips:?}");
                        }
                        Err(e) => {
                            log::debug!("orca account list failed: {e}");
                            last.error = Some(e);
                        }
                    }
                    if tx.try_send(last.clone()).is_ok()
                        && let Some(repaint) = &repaint
                    {
                        repaint();
                    }
                    match stop_rx.recv_timeout(interval) {
                        Err(RecvTimeoutError::Timeout) => {}
                        _ => return,
                    }
                }
            })
            .expect("spawn quota poller thread");
        Self {
            latest: rx,
            stop: stop_tx,
            thread: Some(thread),
        }
    }

    /// The newest pending result, discarding older ones.
    pub fn latest(&self) -> Option<Quotas> {
        self.latest.try_iter().last()
    }
}

impl Drop for QuotaPoller {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("quota poller thread panicked");
        }
    }
}

fn wall_clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Instant;

    use serde_json::json;

    use super::*;
    use crate::orca::accounts::parse_account_list;

    const MIN: i64 = 60_000;

    fn commands_fixture_account_list() -> AccountList {
        parse_account_list(include_bytes!("../tests/fixtures/orca/account_list.json")).unwrap()
    }

    fn window(key: &str, used: f64, minutes: u64, resets_at_ms: Option<i64>) -> RateWindow {
        RateWindow {
            key: key.into(),
            used_percent: used,
            window_minutes: Some(minutes),
            resets_at_ms,
            reset_description: None,
        }
    }

    fn provider(key: &str, windows: Vec<RateWindow>) -> ProviderQuota {
        ProviderQuota {
            limit: ProviderRateLimit {
                key: key.into(),
                status: Some("ok".into()),
                error: None,
                updated_at_ms: None,
                windows,
            },
        }
    }

    #[test]
    fn severity_thresholds() {
        assert_eq!(Severity::from_percent(0.0), Severity::Normal);
        assert_eq!(Severity::from_percent(79.9), Severity::Normal);
        assert_eq!(Severity::from_percent(80.0), Severity::Warn);
        assert_eq!(Severity::from_percent(94.9), Severity::Warn);
        assert_eq!(Severity::from_percent(95.0), Severity::Critical);
        assert_eq!(Severity::from_percent(120.0), Severity::Critical);
    }

    #[test]
    fn chip_picks_most_constrained_current_window() {
        let now = 1_000 * MIN;
        let p = provider(
            "claude",
            vec![
                window("session", 62.0, 300, Some(now + 130 * MIN)),
                window("weekly", 40.0, 10080, Some(now + 3 * 24 * 60 * MIN)),
                // Past its reset: stale, never picked.
                window("fableWeekly", 99.0, 10080, Some(now - MIN)),
            ],
        );
        let chip = p.chip(now).unwrap();
        assert_eq!(chip.text, "Claude 5h 62%");
        assert_eq!(chip.severity, Severity::Normal);
        assert_eq!(
            chip.hover.lines().collect::<Vec<_>>(),
            [
                "Claude",
                "5h: 62% used · resets in 2h 10m",
                "wk: 40% used · resets in 3d 0h",
                "Fable wk: 99% used · reset time passed (stale)",
            ]
        );

        let hot = provider("codex", vec![window("weekly", 96.4, 10080, None)]);
        let chip = hot.chip(now).unwrap();
        assert_eq!(chip.text, "Codex wk 96%");
        assert_eq!(chip.severity, Severity::Critical);

        let stale_only = provider("codex", vec![window("weekly", 50.0, 10080, Some(now))]);
        assert!(stale_only.chip(now).is_none());
    }

    #[test]
    fn quotas_from_fixture() {
        let list = commands_fixture_account_list();
        // Before every reset time in the fixture except fableWeekly's.
        let now = 1_790_339_413_651;
        let quotas = Quotas::from_account_list(&list, now);
        let keys: Vec<&str> = quotas
            .providers
            .iter()
            .map(|p| p.limit.key.as_str())
            .collect();
        assert_eq!(keys, ["claude", "codex"], "unavailable providers dropped");
        let chips = quotas.chips(now);
        let texts: Vec<&str> = chips.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["Claude wk 17%", "Codex wk 0%"]);
        assert!(
            chips[0]
                .hover
                .contains("Fable wk: 91% used · reset time passed")
        );
        assert!(
            chips[0]
                .hover
                .contains("5h: 8% used · resets in 3h 59m (6:30 PM)")
        );
        // Earlier, while the Fable window was still current, it is the constrained one.
        let chips = quotas.chips(1_790_270_000_000);
        assert_eq!(chips[0].text, "Claude Fable wk 91%");
        assert_eq!(chips[0].severity, Severity::Warn);
    }

    #[test]
    fn labels_and_durations() {
        let w = |key: &str, minutes| window(key, 0.0, minutes, None);
        assert_eq!(window_label(&w("session", 300)), "5h");
        assert_eq!(window_label(&w("session", 90)), "90m");
        assert_eq!(window_label(&w("session", 2880)), "2d");
        assert_eq!(window_label(&w("monthly", 0)), "mo");
        assert_eq!(window_label(&w("daily", 1440)), "daily");
        assert_eq!(format_duration(45_000), "45s");
        assert_eq!(format_duration(12 * MIN), "12m");
        assert_eq!(format_duration(26 * 60 * MIN + 5 * MIN), "1d 2h");
        assert_eq!(provider("opencodeGo", vec![]).name(), "OpencodeGo");
    }

    struct Stub(Mutex<Vec<Result<AccountList, String>>>);

    impl QuotaSource for Stub {
        fn fetch(&self) -> Result<AccountList, String> {
            let mut queue = self.0.lock().unwrap();
            if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue[0].clone()
            }
        }
    }

    #[test]
    fn poller_keeps_last_good_data_on_error() {
        let good: AccountList = serde_json::from_value(json!({"rateLimits": {
            "claude": {"provider": "claude", "status": "ok",
                "session": {"usedPercent": 10, "windowMinutes": 300}}}}))
        .unwrap();
        let stub = Stub(Mutex::new(vec![Ok(good), Err("boom".into())]));
        let poller = QuotaPoller::spawn(stub, Duration::from_millis(20), None);
        let deadline = Instant::now() + Duration::from_secs(10);
        let quotas = loop {
            if let Some(q) = poller.latest().filter(|q| q.error.is_some()) {
                break q;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(quotas.error.as_deref(), Some("boom"));
        assert_eq!(quotas.chips(0)[0].text, "Claude 5h 10%");
        drop(poller);
    }
}

//! The health doctor: checks for silent misconfigurations (whip left in dry run, `gh` or
//! `orca` missing from the app's `PATH`, the session hook not installed, a dead Slack reply
//! bridge, notifications denied, the Queen not running).
//!
//! The slow checks ([`Probes`]) run on the [`HealthWorker`] thread at startup and every
//! [`INTERVAL`]; [`evaluate`] combines them with what the app knows ([`AppFacts`]) into a
//! [`HealthReport`], which the top bar's badge and the control API's `health` method show.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chat::SessionState;

/// How often the probes run again.
pub const INTERVAL: Duration = Duration::from_secs(3 * 60);
/// Longest a `--version` or `orca status` call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(8);
/// A bridge heartbeat older than this means the daemon is down (it writes every 30 s).
pub const HEARTBEAT_STALE_MS: i64 = 90_000;

/// How bad a check is. Ordered from best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    #[default]
    Ok,
    /// Not configured or not applicable; never counts against the overall level.
    Off,
    Warn,
    Error,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Off => "off",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// One check's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Check {
    /// Stable key, e.g. `whip`, `gh`.
    pub key: String,
    pub name: String,
    pub level: Level,
    /// What was found, one line.
    pub detail: String,
    /// How to fix it, one line; `None` when there is nothing to fix.
    pub fix: Option<String>,
}

impl Check {
    fn new(key: &str, name: &str, level: Level, detail: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            name: name.into(),
            level,
            detail: detail.into(),
            fix: None,
        }
    }

    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// Every check's verdict and the worst of them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HealthReport {
    /// The worst check's level (`off` counts as `ok`).
    pub level: Level,
    /// When the slow checks last ran; `None` until their first run finished.
    pub checked_at_ms: Option<i64>,
    pub checks: Vec<Check>,
}

impl HealthReport {
    /// Checks that need fixing (warn or error).
    pub fn problems(&self) -> impl Iterator<Item = &Check> {
        self.checks
            .iter()
            .filter(|c| matches!(c.level, Level::Warn | Level::Error))
    }
}

/// The Queen's chat process, as the health check sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueenFact {
    Running,
    Starting,
    Crashed(String),
    Unavailable(String),
    Stopped,
}

impl From<&SessionState> for QueenFact {
    fn from(state: &SessionState) -> Self {
        match state {
            SessionState::Ready => QueenFact::Running,
            SessionState::Starting => QueenFact::Starting,
            SessionState::Crashed { reason, .. } => QueenFact::Crashed(reason.clone()),
            SessionState::Unavailable(why) => QueenFact::Unavailable(why.clone()),
            SessionState::Stopped => QueenFact::Stopped,
        }
    }
}

/// What the app knows itself, cheaply, every frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppFacts {
    pub whip_enabled: bool,
    pub whip_dry_run: bool,
    pub queen: QueenFact,
}

/// A command-line tool the app runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolProbe {
    /// Where it was found; `None` when it is neither on `PATH` nor in Homebrew's bin dirs.
    pub path: Option<String>,
    pub version: Option<String>,
    /// Running it failed (or, for `orca`, its runtime is not reachable).
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookProbe {
    /// Claude's settings have our `SessionStart` entry.
    pub installed: bool,
    /// The hook script exists in the data dir.
    pub script: bool,
    /// Claude's settings could not be read.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BridgeProbe {
    /// No bridge directory: the bridge is not set up here.
    #[default]
    NotConfigured,
    /// The heartbeat's time, `None` when the file is missing or unreadable.
    Heartbeat(Option<i64>),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NotifyProbe {
    /// Not from `Myrstack.app`, so macOS keeps no permission for this process.
    #[default]
    NotBundled,
    /// macOS's answer: `allowed`, `provisional`, `not asked yet`, `denied; …`, …
    Status(String),
}

/// The slow checks' findings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Probes {
    pub checked_at_ms: i64,
    pub orca: ToolProbe,
    pub gh: ToolProbe,
    pub hook: HookProbe,
    pub bridge: BridgeProbe,
    pub notifications: NotifyProbe,
}

/// The report from the app's facts and the latest probes (`None` before their first run).
pub fn evaluate(facts: &AppFacts, probes: Option<&Probes>, now_ms: i64) -> HealthReport {
    let mut checks = vec![whip_check(facts)];
    if let Some(p) = probes {
        checks.push(orca_check(&p.orca));
        checks.push(gh_check(&p.gh));
    }
    checks.push(queen_check(&facts.queen));
    if let Some(p) = probes {
        checks.push(hook_check(&p.hook));
        checks.push(notify_check(&p.notifications));
        checks.push(bridge_check(&p.bridge, now_ms));
    }
    let level = checks
        .iter()
        .map(|c| c.level)
        .filter(|l| *l != Level::Off)
        .max()
        .unwrap_or_default();
    HealthReport {
        level,
        checked_at_ms: probes.map(|p| p.checked_at_ms),
        checks,
    }
}

fn whip_check(facts: &AppFacts) -> Check {
    const NAME: &str = "Whip";
    match (facts.whip_enabled, facts.whip_dry_run) {
        (true, false) => Check::new("whip", NAME, Level::Ok, "live: nudges stalled agents"),
        (true, true) => Check::new(
            "whip",
            NAME,
            Level::Warn,
            "dry run: logs nudges but sends none",
        )
        .fix("Settings › Whip: turn Dry run off (or palette: Toggle whip dry-run)"),
        (false, _) => Check::new(
            "whip",
            NAME,
            Level::Warn,
            "off: stalled agents are not nudged",
        )
        .fix("Settings › Whip: turn the whip on"),
    }
}

fn orca_check(p: &ToolProbe) -> Check {
    const NAME: &str = "orca";
    let Some(path) = &p.path else {
        return Check::new("orca", NAME, Level::Error, "not found on the app's PATH")
            .fix("Install the Orca CLI into /opt/homebrew/bin or /usr/local/bin");
    };
    if let Some(error) = &p.error {
        return Check::new("orca", NAME, Level::Error, format!("{path}: {error}"))
            .fix("Open Orca (`orca open`) and check `orca status`");
    }
    let version = p.version.as_deref().unwrap_or("version unknown");
    Check::new(
        "orca",
        NAME,
        Level::Ok,
        format!("runtime ready, {version} ({path})"),
    )
}

fn gh_check(p: &ToolProbe) -> Check {
    const NAME: &str = "gh";
    let Some(path) = &p.path else {
        return Check::new(
            "gh",
            NAME,
            Level::Error,
            "not found on the app's PATH: the review queue can't read PRs",
        )
        .fix("`brew install gh`, or set MYRSTACK_GH_BIN to its path");
    };
    if let Some(error) = &p.error {
        return Check::new("gh", NAME, Level::Warn, format!("{path}: {error}"))
            .fix("Run `gh --version` and `gh auth status` in a terminal");
    }
    let version = p.version.as_deref().unwrap_or("version unknown");
    Check::new("gh", NAME, Level::Ok, format!("{version} ({path})"))
}

fn queen_check(queen: &QueenFact) -> Check {
    const NAME: &str = "Queen";
    match queen {
        QueenFact::Running => Check::new("queen", NAME, Level::Ok, "chat process running"),
        QueenFact::Starting => Check::new("queen", NAME, Level::Ok, "chat process starting"),
        QueenFact::Crashed(reason) => {
            Check::new("queen", NAME, Level::Error, format!("crashed: {reason}"))
                .fix("It restarts by itself; see the log (palette: Open log) if it keeps crashing")
        }
        QueenFact::Unavailable(why) => Check::new("queen", NAME, Level::Error, why.clone())
            .fix("Install the `claude` CLI or set MYRSTACK_CLAUDE_BIN"),
        QueenFact::Stopped => Check::new("queen", NAME, Level::Warn, "chat process stopped")
            .fix("Open the Queen's chat (⌘J) to start it"),
    }
}

fn hook_check(p: &HookProbe) -> Check {
    const NAME: &str = "Session hook";
    const FIX: &str = "Run `myrstack install-hook`";
    if let Some(error) = &p.error {
        return Check::new(
            "hook",
            NAME,
            Level::Warn,
            format!("can't read Claude settings: {error}"),
        )
        .fix(FIX);
    }
    match (p.installed, p.script) {
        (true, true) => Check::new("hook", NAME, Level::Ok, "installed in Claude's settings"),
        (true, false) => Check::new(
            "hook",
            NAME,
            Level::Warn,
            "in Claude's settings, but the script is missing",
        )
        .fix(FIX),
        (false, _) => Check::new(
            "hook",
            NAME,
            Level::Warn,
            "not installed: terminals are matched to sessions by guesswork",
        )
        .fix(FIX),
    }
}

fn notify_check(p: &NotifyProbe) -> Check {
    const NAME: &str = "Notifications";
    let status = match p {
        NotifyProbe::NotBundled => {
            return Check::new(
                "notifications",
                NAME,
                Level::Off,
                "not running from Myrstack.app: in-app pop-ups only",
            );
        }
        NotifyProbe::Status(status) => status,
    };
    match status.as_str() {
        "allowed" | "provisional" => Check::new("notifications", NAME, Level::Ok, status.clone()),
        s if s.starts_with("denied") => {
            Check::new("notifications", NAME, Level::Warn, "denied by macOS")
                .fix("System Settings › Notifications › Myrstack: allow notifications")
        }
        "not asked yet" => Check::new(
            "notifications",
            NAME,
            Level::Warn,
            "permission not asked yet",
        )
        .fix("Send a test notification from Settings › Notifications"),
        other => Check::new(
            "notifications",
            NAME,
            Level::Off,
            format!("permission {other}"),
        ),
    }
}

fn bridge_check(p: &BridgeProbe, now_ms: i64) -> Check {
    const NAME: &str = "Slack reply bridge";
    const FIX: &str = "Re-run ~/repo/saker/slack-reply-bridge/install.sh (logs in \
                       ~/Library/Logs/slack-reply-bridge/)";
    match p {
        BridgeProbe::NotConfigured => {
            Check::new("slack_bridge", NAME, Level::Off, "not configured")
        }
        BridgeProbe::Heartbeat(None) => Check::new(
            "slack_bridge",
            NAME,
            Level::Warn,
            "no heartbeat: the daemon is down",
        )
        .fix(FIX),
        BridgeProbe::Heartbeat(Some(at)) => {
            let age = now_ms - at;
            if age > HEARTBEAT_STALE_MS {
                Check::new(
                    "slack_bridge",
                    NAME,
                    Level::Warn,
                    format!("heartbeat {}s old: the daemon is down", age / 1000),
                )
                .fix(FIX)
            } else {
                Check::new(
                    "slack_bridge",
                    NAME,
                    Level::Ok,
                    format!("heartbeat {}s ago", age.max(0) / 1000),
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Probes

impl Probes {
    /// Runs every slow check. Takes up to a few seconds (it runs `orca status` and
    /// `gh --version`).
    pub fn gather() -> Self {
        Self {
            checked_at_ms: crate::control::server::now_ms(),
            orca: probe_orca(),
            gh: probe_gh(),
            hook: probe_hook(
                &crate::claude::hook::default_settings_path(),
                &crate::claude::hook::data_dir(),
            ),
            bridge: probe_bridge(&crate::envoy::slack::bridge_dir()),
            notifications: probe_notifications(),
        }
    }
}

fn probe_orca() -> ToolProbe {
    let client = crate::orca::OrcaClient::new().timeout(CALL_TIMEOUT);
    let path = client.binary();
    if !path.is_absolute() {
        return ToolProbe::default();
    }
    let mut probe = ToolProbe {
        path: Some(path.display().to_string()),
        ..ToolProbe::default()
    };
    match client.call_value(&["status"]) {
        Ok(status) => {
            let (version, error) = parse_orca_status(&status);
            probe.version = version;
            probe.error = error;
        }
        Err(e) => probe.error = Some(e.to_string()),
    }
    probe
}

/// The runtime's version and, when it is not reachable, why: from `orca status --json`.
pub fn parse_orca_status(status: &Value) -> (Option<String>, Option<String>) {
    let runtime = &status["runtime"];
    let version = runtime["appVersion"].as_str().map(|v| format!("Orca {v}"));
    let error = match runtime["reachable"].as_bool() {
        Some(true) => None,
        _ => Some(format!(
            "runtime not reachable (state: {})",
            runtime["state"].as_str().unwrap_or("unknown")
        )),
    };
    (version, error)
}

fn probe_gh() -> ToolProbe {
    let binary = crate::review::github::GhCli::default().binary;
    if !Path::new(&binary).is_absolute() {
        return ToolProbe::default();
    }
    let (version, error) = match run_capture(&binary, &["--version"], CALL_TIMEOUT) {
        Ok(out) => (out.lines().next().map(|l| l.trim().to_owned()), None),
        Err(e) => (None, Some(e)),
    };
    ToolProbe {
        path: Some(binary),
        version,
        error,
    }
}

fn probe_hook(settings: &Path, data_dir: &Path) -> HookProbe {
    let script = data_dir
        .join(crate::claude::hook::HOOK_SCRIPT_NAME)
        .is_file();
    let parsed = match std::fs::read_to_string(settings) {
        Ok(text) if text.trim().is_empty() => Ok(Value::Null),
        Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(e) => Err(e.to_string()),
    };
    match parsed {
        Ok(value) => HookProbe {
            installed: crate::claude::hook::hook_installed(&value),
            script,
            error: None,
        },
        Err(error) => HookProbe {
            installed: false,
            script,
            error: Some(error),
        },
    }
}

fn probe_bridge(dir: &Path) -> BridgeProbe {
    if !dir.is_dir() {
        return BridgeProbe::NotConfigured;
    }
    let at = std::fs::read_to_string(dir.join("heartbeat"))
        .ok()
        .and_then(|text| parse_heartbeat(&text));
    BridgeProbe::Heartbeat(at)
}

/// The heartbeat file's ISO timestamp, in ms since the epoch.
pub fn parse_heartbeat(text: &str) -> Option<i64> {
    let ts: jiff::Timestamp = text.trim().parse().ok()?;
    Some(ts.as_millisecond())
}

fn probe_notifications() -> NotifyProbe {
    #[cfg(target_os = "macos")]
    if crate::notify::native::is_bundled() {
        return NotifyProbe::Status(crate::notify::native::authorization_status().to_owned());
    }
    NotifyProbe::NotBundled
}

/// Runs `binary args`, returning stdout; fails on a non-zero exit or after `timeout`.
fn run_capture(binary: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    if !status.success() {
        return Err(format!("exited with {status}"));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Worker

/// Runs the [`Probes`] off the UI thread: at once, then every [`INTERVAL`] or on
/// [`HealthWorker::check_now`].
pub struct HealthWorker {
    results: Receiver<Probes>,
    wake: Sender<()>,
}

impl HealthWorker {
    pub fn spawn(repaint: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        Self::spawn_with(Probes::gather, INTERVAL, repaint)
    }

    fn spawn_with(
        gather: impl Fn() -> Probes + Send + 'static,
        interval: Duration,
        repaint: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let (tx, results) = crossbeam_channel::unbounded();
        let (wake, wake_rx) = crossbeam_channel::bounded::<()>(1);
        let spawned = std::thread::Builder::new()
            .name("health".into())
            .spawn(move || {
                loop {
                    if tx.send(gather()).is_err() {
                        return;
                    }
                    if let Some(repaint) = &repaint {
                        repaint();
                    }
                    match wake_rx.recv_timeout(interval) {
                        Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            });
        if let Err(e) = spawned {
            log::warn!("health checks disabled: {e}");
        }
        Self { results, wake }
    }

    /// The newest probes, if a run finished since the last call.
    pub fn latest(&self) -> Option<Probes> {
        self.results.try_iter().last()
    }

    /// Runs the probes again now (a run already queued counts).
    pub fn check_now(&self) {
        let _ = self.wake.try_send(());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn facts() -> AppFacts {
        AppFacts {
            whip_enabled: true,
            whip_dry_run: false,
            queen: QueenFact::Running,
        }
    }

    fn tool(path: &str, version: &str) -> ToolProbe {
        ToolProbe {
            path: Some(path.into()),
            version: Some(version.into()),
            error: None,
        }
    }

    fn healthy() -> Probes {
        Probes {
            checked_at_ms: NOW - 1000,
            orca: tool("/opt/homebrew/bin/orca", "Orca 1.4.192"),
            gh: tool("/opt/homebrew/bin/gh", "gh version 2.92.0"),
            hook: HookProbe {
                installed: true,
                script: true,
                error: None,
            },
            bridge: BridgeProbe::Heartbeat(Some(NOW - 10_000)),
            notifications: NotifyProbe::Status("allowed".into()),
        }
    }

    fn check<'a>(report: &'a HealthReport, key: &str) -> &'a Check {
        report.checks.iter().find(|c| c.key == key).unwrap()
    }

    #[test]
    fn all_good_is_ok_with_no_fixes() {
        let report = evaluate(&facts(), Some(&healthy()), NOW);
        assert_eq!(report.level, Level::Ok);
        assert_eq!(report.checked_at_ms, Some(NOW - 1000));
        let keys: Vec<&str> = report.checks.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "whip",
                "orca",
                "gh",
                "queen",
                "hook",
                "notifications",
                "slack_bridge"
            ]
        );
        assert!(report.checks.iter().all(|c| c.fix.is_none()), "{report:?}");
        assert_eq!(report.problems().count(), 0);
    }

    #[test]
    fn before_the_first_probe_only_app_facts_are_checked() {
        let mut f = facts();
        f.whip_dry_run = true;
        let report = evaluate(&f, None, NOW);
        assert_eq!(report.checked_at_ms, None);
        assert_eq!(report.checks.len(), 2);
        assert_eq!(report.level, Level::Warn);
    }

    #[test]
    fn whip_dry_run_or_off_is_amber_with_a_fix() {
        let mut f = facts();
        f.whip_dry_run = true;
        let report = evaluate(&f, Some(&healthy()), NOW);
        assert_eq!(report.level, Level::Warn);
        let whip = check(&report, "whip");
        assert!(whip.detail.contains("dry run"));
        assert!(whip.fix.as_deref().unwrap().contains("Dry run"));
        f.whip_enabled = false;
        let report = evaluate(&f, Some(&healthy()), NOW);
        assert!(check(&report, "whip").detail.starts_with("off"));
    }

    #[test]
    fn missing_gh_or_unreachable_orca_is_red() {
        let mut p = healthy();
        p.gh = ToolProbe::default();
        let report = evaluate(&facts(), Some(&p), NOW);
        assert_eq!(report.level, Level::Error);
        assert!(check(&report, "gh").detail.contains("review queue"));
        assert!(check(&report, "gh").fix.is_some());

        let mut p = healthy();
        p.orca.error = Some("runtime not reachable (state: stopped)".into());
        let report = evaluate(&facts(), Some(&p), NOW);
        assert_eq!(check(&report, "orca").level, Level::Error);
        assert_eq!(report.problems().count(), 1);
    }

    #[test]
    fn bridge_absence_is_not_an_error_but_a_stale_heartbeat_is_amber() {
        let mut p = healthy();
        p.bridge = BridgeProbe::NotConfigured;
        let report = evaluate(&facts(), Some(&p), NOW);
        assert_eq!(check(&report, "slack_bridge").level, Level::Off);
        assert_eq!(report.level, Level::Ok);

        p.bridge = BridgeProbe::Heartbeat(Some(NOW - HEARTBEAT_STALE_MS - 1));
        assert_eq!(
            check(&evaluate(&facts(), Some(&p), NOW), "slack_bridge").level,
            Level::Warn
        );
        p.bridge = BridgeProbe::Heartbeat(None);
        assert_eq!(evaluate(&facts(), Some(&p), NOW).level, Level::Warn);
    }

    #[test]
    fn hook_notifications_and_queen_verdicts() {
        let mut p = healthy();
        p.hook.installed = false;
        p.notifications = NotifyProbe::Status("denied; allow Myrstack in System Settings".into());
        let mut f = facts();
        f.queen = QueenFact::Unavailable("no claude binary".into());
        let report = evaluate(&f, Some(&p), NOW);
        assert_eq!(check(&report, "hook").level, Level::Warn);
        assert_eq!(check(&report, "notifications").level, Level::Warn);
        assert_eq!(check(&report, "queen").level, Level::Error);
        assert_eq!(report.level, Level::Error);

        p.notifications = NotifyProbe::NotBundled;
        assert_eq!(
            check(&evaluate(&facts(), Some(&p), NOW), "notifications").level,
            Level::Off
        );
    }

    #[test]
    fn orca_status_parsing() {
        let ok = json!({"runtime": {"reachable": true, "state": "ready", "appVersion": "1.4.192"}});
        assert_eq!(parse_orca_status(&ok), (Some("Orca 1.4.192".into()), None));
        let down = json!({"runtime": {"reachable": false, "state": "stopped"}});
        assert_eq!(
            parse_orca_status(&down).1.as_deref(),
            Some("runtime not reachable (state: stopped)")
        );
    }

    #[test]
    fn heartbeat_parsing() {
        assert_eq!(
            parse_heartbeat("2026-10-02T21:15:03.321Z\n"),
            Some(1_790_975_703_321)
        );
        assert_eq!(parse_heartbeat("garbage"), None);
    }

    #[test]
    fn probes_read_the_hook_and_bridge_files() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        assert_eq!(
            probe_hook(&settings, dir.path()),
            HookProbe::default(),
            "no settings file: not installed"
        );
        std::fs::write(
            &settings,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"command":"\"/d/myrstack-session-hook.sh\""}]}]}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("myrstack-session-hook.sh"), "").unwrap();
        let hook = probe_hook(&settings, dir.path());
        assert!(hook.installed && hook.script && hook.error.is_none());
        std::fs::write(&settings, "{not json").unwrap();
        assert!(probe_hook(&settings, dir.path()).error.is_some());

        let bridge = dir.path().join("subs");
        assert_eq!(probe_bridge(&bridge), BridgeProbe::NotConfigured);
        std::fs::create_dir(&bridge).unwrap();
        assert_eq!(probe_bridge(&bridge), BridgeProbe::Heartbeat(None));
        std::fs::write(bridge.join("heartbeat"), "2026-10-02T21:15:03.321Z").unwrap();
        assert_eq!(
            probe_bridge(&bridge),
            BridgeProbe::Heartbeat(Some(1_790_975_703_321))
        );
    }

    #[test]
    fn worker_runs_at_once_and_on_request() {
        let worker = HealthWorker::spawn_with(Probes::default, Duration::from_secs(3600), None);
        let first = worker.results.recv_timeout(Duration::from_secs(5));
        assert!(first.is_ok());
        worker.check_now();
        assert!(worker.results.recv_timeout(Duration::from_secs(5)).is_ok());
    }
}

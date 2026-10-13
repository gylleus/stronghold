//! A PR's state on GitHub, read with `gh pr view --json` (read-only): open/draft/merged/closed,
//! CI, the Ankra review verdict and the SHA it reviewed, the coordinator's ready marker (first
//! body line) and the review decision.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::orca::client::resolve_binary;

/// Fields `gh pr view --json` is asked for.
pub const GH_FIELDS: &str = "number,title,state,isDraft,headRefOid,mergedAt,url,body,\
reviewDecision,statusCheckRollup,comments";
/// How long one `gh` call may take before it is killed.
pub const GH_TIMEOUT: Duration = Duration::from_secs(45);
/// Author login of Ankra's review comment (the REST name carries `[bot]`).
const ANKRA_LOGIN: &str = "ankra-platform";
const ANKRA_HEADING: &str = "## Ankra AI review";
/// Check runs that never gate a PR: Ankra's advisory row and the retired `claude` workflow.
const IGNORED_CHECKS: &[&str] = &["Ankra AI review", "claude"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrState {
    /// Merged or closed: the PR left the queue.
    pub fn is_gone(self) -> bool {
        matches!(self, Self::Merged | Self::Closed)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Draft => "draft",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }
}

/// CI across the PR's checks (Ankra's and the retired `claude` row are ignored).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Ci {
    /// Every check passed (skipped and neutral count as passed).
    Passing {
        total: usize,
    },
    Failing {
        failed: usize,
        total: usize,
    },
    Pending {
        pending: usize,
        total: usize,
    },
    /// No checks at all.
    None,
}

impl Ci {
    /// `✓`, `✗`, `…` or `–`.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Passing { .. } => "✓",
            Self::Failing { .. } => "✗",
            Self::Pending { .. } => "…",
            Self::None => "–",
        }
    }

    pub fn describe(self) -> String {
        match self {
            Self::Passing { total } => format!("CI green ({total} checks)"),
            Self::Failing { failed, total } => format!("CI failing ({failed} of {total} checks)"),
            Self::Pending { pending, total } => {
                format!("CI running ({pending} of {total} checks pending)")
            }
            Self::None => "no CI checks".into(),
        }
    }
}

/// Ankra's `## Ankra AI review` comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ankra {
    /// The `**Verdict: …**` text without trailing punctuation, e.g. `comments`, `looks good`.
    pub verdict: Option<String>,
    /// From the `_Reviewed commit <sha>` footer.
    pub reviewed_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    /// `✅ **Ready for review** — …`
    Ready,
    /// `⏸ **Not ready** — …`
    NotReady,
}

/// The coordinator's ready marker: the first line of the PR description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub kind: MarkerKind,
    /// The text after the dash, without the SHA.
    pub note: Option<String>,
    /// The verified SHA it names.
    pub sha: Option<String>,
}

/// What one `gh pr view` said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrStatus {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PrState,
    pub head_sha: String,
    pub ci: Ci,
    pub ankra: Option<Ankra>,
    pub marker: Option<Marker>,
    /// `APPROVED`, `CHANGES_REQUESTED` or `REVIEW_REQUIRED`; `None` when GitHub gives none.
    pub review_decision: Option<String>,
    pub merged_at: Option<String>,
    pub fetched_at_ms: i64,
}

impl PrStatus {
    /// Whether Ankra's review is of the current head. `None` without a review or footer.
    pub fn ankra_on_head(&self) -> Option<bool> {
        let sha = self.ankra.as_ref()?.reviewed_sha.as_deref()?;
        Some(same_sha(sha, &self.head_sha))
    }

    /// Whether the ready marker names the current head. `None` without a marker or SHA.
    pub fn marker_on_head(&self) -> Option<bool> {
        let sha = self.marker.as_ref()?.sha.as_deref()?;
        Some(same_sha(sha, &self.head_sha))
    }

    /// `Ankra: comments`, `Ankra: looks good (stale)`, `Ankra: no review`.
    pub fn ankra_text(&self) -> String {
        match &self.ankra {
            None => "Ankra: no review".into(),
            Some(a) => {
                let verdict = a.verdict.as_deref().unwrap_or("no verdict");
                let stale = if self.ankra_on_head() == Some(false) {
                    " (stale)"
                } else {
                    ""
                };
                format!("Ankra: {verdict}{stale}")
            }
        }
    }

    /// `✅ ready`, `⏸ not ready (stale)`, `no marker`.
    pub fn marker_text(&self) -> String {
        let Some(m) = &self.marker else {
            return "no marker".into();
        };
        let base = match m.kind {
            MarkerKind::Ready => "✅ ready",
            MarkerKind::NotReady => "⏸ not ready",
        };
        let stale = if self.marker_on_head() == Some(false) {
            " (stale)"
        } else {
            ""
        };
        format!("{base}{stale}")
    }
}

/// Whether two SHAs name the same commit: one is a prefix of the other (at least 7 chars).
pub fn same_sha(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().to_ascii_lowercase(), b.trim().to_ascii_lowercase());
    let n = a.len().min(b.len());
    n >= 7 && a[..n] == b[..n]
}

// ---------------------------------------------------------------------------
// Parsing

/// Parses `gh pr view --json <GH_FIELDS>` output.
pub fn parse_view(json: &Value, fetched_at_ms: i64) -> Result<PrStatus, String> {
    let str_field = |k: &str| json.get(k).and_then(Value::as_str);
    let number = json
        .get("number")
        .and_then(Value::as_u64)
        .ok_or("gh output has no \"number\"")?;
    let state = match (
        str_field("state"),
        json.get("isDraft").and_then(Value::as_bool),
    ) {
        (Some("MERGED"), _) => PrState::Merged,
        (Some("CLOSED"), _) => PrState::Closed,
        (Some("OPEN"), Some(true)) => PrState::Draft,
        (Some("OPEN"), _) => PrState::Open,
        (other, _) => return Err(format!("unexpected PR state {other:?}")),
    };
    let checks = json
        .get("statusCheckRollup")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let comments = json
        .get("comments")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    Ok(PrStatus {
        number,
        title: str_field("title").unwrap_or_default().to_owned(),
        url: str_field("url").unwrap_or_default().to_owned(),
        state,
        head_sha: str_field("headRefOid").unwrap_or_default().to_owned(),
        ci: ci_of(checks),
        ankra: ankra_of(comments),
        marker: str_field("body").and_then(parse_marker),
        review_decision: str_field("reviewDecision")
            .filter(|d| !d.is_empty())
            .map(str::to_owned),
        merged_at: str_field("mergedAt")
            .filter(|d| !d.is_empty())
            .map(str::to_owned),
        fetched_at_ms,
    })
}

/// CI from `statusCheckRollup`: check runs (`status`/`conclusion`) and commit statuses
/// (`state`).
pub fn ci_of(checks: &[Value]) -> Ci {
    let mut total = 0;
    let (mut failed, mut pending) = (0, 0);
    for check in checks {
        let s = |k: &str| check.get(k).and_then(Value::as_str).unwrap_or_default();
        let name = if s("name").is_empty() {
            s("context")
        } else {
            s("name")
        };
        if IGNORED_CHECKS.contains(&name) {
            continue;
        }
        total += 1;
        if check.get("__typename").and_then(Value::as_str) == Some("StatusContext")
            || (check.get("state").is_some() && check.get("status").is_none())
        {
            match s("state") {
                "SUCCESS" => {}
                "PENDING" | "EXPECTED" => pending += 1,
                _ => failed += 1,
            }
            continue;
        }
        if s("status") != "COMPLETED" {
            pending += 1;
            continue;
        }
        match s("conclusion") {
            "SUCCESS" | "SKIPPED" | "NEUTRAL" => {}
            _ => failed += 1,
        }
    }
    match (total, failed, pending) {
        (0, _, _) => Ci::None,
        (_, f, _) if f > 0 => Ci::Failing { failed: f, total },
        (_, _, p) if p > 0 => Ci::Pending { pending: p, total },
        _ => Ci::Passing { total },
    }
}

/// Ankra's review comment: the newest comment by Ankra that has its heading.
pub fn ankra_of(comments: &[Value]) -> Option<Ankra> {
    let body = comments.iter().rev().find_map(|c| {
        let login = c.pointer("/author/login").and_then(Value::as_str)?;
        let body = c.get("body").and_then(Value::as_str)?;
        (login.trim_end_matches("[bot]") == ANKRA_LOGIN && body.contains(ANKRA_HEADING))
            .then_some(body)
    })?;
    Some(parse_ankra(body))
}

/// The verdict and reviewed SHA of an Ankra review body.
pub fn parse_ankra(body: &str) -> Ankra {
    let verdict = body.lines().find_map(|l| {
        let rest = l.trim().trim_start_matches('*').trim_start();
        let rest = rest.strip_prefix("Verdict:")?;
        let v = rest
            .trim()
            .trim_end_matches('*')
            .trim_end_matches(['.', ':'])
            .trim();
        (!v.is_empty()).then(|| v.to_owned())
    });
    let reviewed_sha = body.lines().find_map(|l| {
        let at = l.find("Reviewed commit")?;
        let rest = l[at + "Reviewed commit".len()..].trim_start();
        let sha: String = rest
            .trim_start_matches('`')
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        (sha.len() >= 7).then_some(sha)
    });
    Ankra {
        verdict,
        reviewed_sha,
    }
}

/// The ready marker on the first non-empty line of `body`, if it is one.
pub fn parse_marker(body: &str) -> Option<Marker> {
    let line = body.lines().map(str::trim).find(|l| !l.is_empty())?;
    let (kind, rest) = if let Some(rest) = line.strip_prefix("✅") {
        (MarkerKind::Ready, rest)
    } else if let Some(rest) = line.strip_prefix("⏸") {
        (MarkerKind::NotReady, rest.trim_start_matches('\u{fe0f}'))
    } else {
        return None;
    };
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix("**Ready for review**")
        .or_else(|| rest.strip_prefix("**Not ready**"))
        .unwrap_or(rest)
        .trim_start()
        .trim_start_matches(['—', '–', '-'])
        .trim();
    let (note, sha) = split_sha(rest);
    Some(Marker {
        kind,
        note: (!note.is_empty()).then(|| note.to_owned()),
        sha,
    })
}

/// Splits a trailing SHA off a marker's text: `… (72179e7)`, `… 72179e7` or just `72179e7`.
fn split_sha(text: &str) -> (&str, Option<String>) {
    let trimmed = text.trim_end();
    let (inner, paren_start) = match trimmed.strip_suffix(')') {
        Some(t) => match t.rfind('(') {
            Some(open) => (&t[open + 1..], Some(open)),
            None => return (trimmed, None),
        },
        None => {
            let start = trimmed.rfind(char::is_whitespace).map_or(0, |i| i + 1);
            (&trimmed[start..], None)
        }
    };
    let candidate = inner.trim().trim_matches('`');
    let is_sha = candidate.len() >= 7
        && candidate.len() <= 40
        && candidate.chars().all(|c| c.is_ascii_hexdigit());
    if !is_sha {
        return (trimmed, None);
    }
    let note_end = paren_start.unwrap_or(trimmed.len() - inner.len());
    let note = trimmed[..note_end]
        .trim_end()
        .trim_end_matches(['—', '–', '-', ','])
        .trim_end();
    (note, Some(candidate.to_ascii_lowercase()))
}

// ---------------------------------------------------------------------------
// Running gh

/// Why a status could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhError {
    /// `gh` is missing or not logged in: no PR can be read.
    Unavailable(String),
    /// This PR could not be read (not found, network, timeout).
    Failed(String),
}

impl std::fmt::Display for GhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(e) => write!(f, "GitHub status unavailable: {e}"),
            Self::Failed(e) => f.write_str(e),
        }
    }
}

/// Reads a PR's status; the app uses [`GhCli`].
pub trait GhSource: Send {
    fn view(&self, url: &str, now_ms: i64) -> Result<PrStatus, GhError>;
}

/// `gh pr view <url> --json …`: read-only.
#[derive(Debug, Clone)]
pub struct GhCli {
    pub binary: String,
}

/// Install locations checked when `gh` isn't on `PATH`: an app launched from Finder gets only
/// `/usr/bin:/bin:/usr/sbin:/sbin`, without Homebrew's bin dirs.
pub const GH_FALLBACK_PATHS: &[&str] = &["/opt/homebrew/bin/gh", "/usr/local/bin/gh"];

impl Default for GhCli {
    /// `$STRONGHOLD_GH_BIN` when set, else `gh` resolved on `PATH`, then [`GH_FALLBACK_PATHS`].
    fn default() -> Self {
        let binary = crate::legacy::env("STRONGHOLD_GH_BIN")
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "gh".into());
        let path = std::env::var_os("PATH");
        let resolved = resolve_binary(Path::new(&binary), path.as_deref(), GH_FALLBACK_PATHS);
        Self {
            binary: resolved.to_string_lossy().into_owned(),
        }
    }
}

impl GhSource for GhCli {
    fn view(&self, url: &str, now_ms: i64) -> Result<PrStatus, GhError> {
        let args = ["pr", "view", url, "--json", GH_FIELDS];
        let (ok, stdout, stderr) = run_with_timeout(&self.binary, &args, GH_TIMEOUT)?;
        if !ok {
            return Err(classify_failure(&stderr));
        }
        let json: Value = serde_json::from_str(&stdout)
            .map_err(|e| GhError::Failed(format!("unreadable gh output: {e}")))?;
        parse_view(&json, now_ms).map_err(GhError::Failed)
    }
}

impl GhCli {
    /// The head branch of `owner/repo#number`.
    pub fn head_branch(&self, owner: &str, repo: &str, number: u64) -> Result<String, GhError> {
        let number = number.to_string();
        let repo = format!("{owner}/{repo}");
        let args = [
            "pr",
            "view",
            &number,
            "--repo",
            &repo,
            "--json",
            "headRefName",
            "-q",
            ".headRefName",
        ];
        let (ok, stdout, stderr) = run_with_timeout(&self.binary, &args, GH_TIMEOUT)?;
        if !ok {
            return Err(classify_failure(&stderr));
        }
        Ok(stdout.trim().to_owned())
    }
}

/// A failed `gh` run: auth problems make every PR unreadable.
pub fn classify_failure(stderr: &str) -> GhError {
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("gh failed")
        .to_owned();
    let lower = stderr.to_lowercase();
    if lower.contains("gh auth login")
        || lower.contains("not logged in")
        || lower.contains("authentication")
    {
        GhError::Unavailable(first)
    } else {
        GhError::Failed(first)
    }
}

fn run_with_timeout(
    binary: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(bool, String, String), GhError> {
    let mut child = Command::new(binary)
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GhError::Unavailable(format!(
                    "`{binary}` not found on PATH or in {}",
                    GH_FALLBACK_PATHS.join(", ")
                ))
            } else {
                GhError::Unavailable(format!("cannot run `{binary}`: {e}"))
            }
        })?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GhError::Failed(format!(
                    "gh took longer than {} s",
                    timeout.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(GhError::Failed(format!("waiting for gh: {e}"))),
        }
    };
    let out = out_thread.join().unwrap_or_default();
    let err = err_thread.join().unwrap_or_default();
    Ok((status.success(), out, err))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fixture(name: &str) -> Value {
        let path = format!(
            "{}/tests/fixtures/github/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn stale_ankra_and_stale_ready_marker() {
        let s = parse_view(&fixture("pr_ready_marker_stale.json"), 7).unwrap();
        assert_eq!(s.number, 255);
        assert_eq!(s.title, "recs: gate legacy config retirement");
        assert_eq!(s.url, "https://github.com/depict-org/realtime/pull/255");
        assert_eq!(s.state, PrState::Open);
        assert_eq!(s.head_sha, "92b1c9463bf2ddf30382046261754d7005b753e3");
        assert_eq!(
            s.ci,
            Ci::Passing { total: 9 },
            "Ankra's neutral row is ignored"
        );
        let ankra = s.ankra.clone().unwrap();
        assert_eq!(ankra.verdict.as_deref(), Some("comments"));
        assert_eq!(ankra.reviewed_sha.as_deref(), Some("42ee4463fb6c"));
        assert_eq!(s.ankra_on_head(), Some(false));
        assert_eq!(s.ankra_text(), "Ankra: comments (stale)");
        let marker = s.marker.clone().unwrap();
        assert_eq!(marker.kind, MarkerKind::Ready);
        assert_eq!(marker.sha.as_deref(), Some("72179e7"));
        assert_eq!(marker.note, None);
        assert_eq!(s.marker_on_head(), Some(false));
        assert_eq!(s.marker_text(), "✅ ready (stale)");
        assert_eq!(s.review_decision, None);
        assert_eq!(s.fetched_at_ms, 7);
    }

    #[test]
    fn ankra_on_head_without_marker() {
        let s = parse_view(&fixture("pr_ankra_on_head.json"), 0).unwrap();
        assert_eq!(s.state, PrState::Open);
        assert_eq!(s.ankra_on_head(), Some(true));
        assert_eq!(s.ankra_text(), "Ankra: comments");
        assert_eq!(s.marker, None);
        assert_eq!(s.marker_text(), "no marker");
        assert!(matches!(s.ci, Ci::Passing { .. }));
    }

    #[test]
    fn draft_and_merged() {
        let draft = parse_view(&fixture("pr_draft.json"), 0).unwrap();
        assert_eq!(draft.state, PrState::Draft);
        assert!(!draft.state.is_gone());
        assert!(matches!(draft.ci, Ci::Passing { .. }), "{:?}", draft.ci);
        let merged = parse_view(&fixture("pr_merged.json"), 0).unwrap();
        assert_eq!(merged.state, PrState::Merged);
        assert!(merged.state.is_gone());
        assert!(merged.merged_at.is_some());
        let marker = merged.marker.clone().unwrap();
        assert_eq!(
            marker.sha.as_deref(),
            Some("50e7dbc14b57afb25df977b6f830dd10a667d2d8")
        );
        assert!(marker.note.unwrap().starts_with("stashed liquid schemas"));
        assert_eq!(merged.marker_on_head(), Some(true));
        assert_eq!(merged.ankra.unwrap().verdict.as_deref(), Some("looks good"));
    }

    #[test]
    fn ci_rolls_up_failures_before_pending() {
        let run = |status: &str, conclusion: &str| json!({"__typename": "CheckRun", "name": "t", "status": status, "conclusion": conclusion});
        let ctx =
            |state: &str| json!({"__typename": "StatusContext", "context": "c", "state": state});
        assert_eq!(ci_of(&[]), Ci::None);
        assert_eq!(
            ci_of(&[
                run("COMPLETED", "SUCCESS"),
                run("COMPLETED", "SKIPPED"),
                ctx("SUCCESS")
            ]),
            Ci::Passing { total: 3 }
        );
        assert_eq!(
            ci_of(&[
                run("IN_PROGRESS", ""),
                run("COMPLETED", "SUCCESS"),
                ctx("PENDING")
            ]),
            Ci::Pending {
                pending: 2,
                total: 3
            }
        );
        assert_eq!(
            ci_of(&[
                run("IN_PROGRESS", ""),
                run("COMPLETED", "FAILURE"),
                ctx("ERROR")
            ]),
            Ci::Failing {
                failed: 2,
                total: 3
            }
        );
        let ignored = [
            json!({"__typename": "CheckRun", "name": "Ankra AI review", "status": "COMPLETED", "conclusion": "FAILURE"}),
            json!({"__typename": "CheckRun", "name": "claude", "status": "COMPLETED", "conclusion": "FAILURE"}),
        ];
        assert_eq!(ci_of(&ignored), Ci::None);
        assert_eq!(
            Ci::Failing {
                failed: 1,
                total: 2
            }
            .symbol(),
            "✗"
        );
    }

    #[test]
    fn markers_in_their_forms() {
        let m =
            parse_marker("✅ **Ready for review** — CI green, threads closed (72179e7)\n\n## Why")
                .unwrap();
        assert_eq!(m.kind, MarkerKind::Ready);
        assert_eq!(m.note.as_deref(), Some("CI green, threads closed"));
        assert_eq!(m.sha.as_deref(), Some("72179e7"));
        let m = parse_marker("⏸ **Not ready** — Ankra has not reviewed the new head (ABCDEF1234)")
            .unwrap();
        assert_eq!(m.kind, MarkerKind::NotReady);
        assert_eq!(m.sha.as_deref(), Some("abcdef1234"));
        assert_eq!(
            m.note.as_deref(),
            Some("Ankra has not reviewed the new head")
        );
        let m = parse_marker("⏸️ **Not ready** — waiting on a deploy").unwrap();
        assert_eq!(m.kind, MarkerKind::NotReady);
        assert_eq!(m.sha, None);
        assert_eq!(m.note.as_deref(), Some("waiting on a deploy"));
        let m = parse_marker("\n✅ **Ready for review** — 72179e7").unwrap();
        assert_eq!((m.note, m.sha.as_deref()), (None, Some("72179e7")));
        let m = parse_marker("✅ **Ready for review** — fixes (see notes)").unwrap();
        assert_eq!(m.sha, None);
        assert_eq!(m.note.as_deref(), Some("fixes (see notes)"));
        assert_eq!(
            parse_marker("## Why\n✅ **Ready for review** — x (1234567)"),
            None
        );
        assert_eq!(parse_marker(""), None);
    }

    #[test]
    fn ankra_bodies_and_shas() {
        let a = parse_ankra(
            "## Ankra AI review\n\n**Verdict: looks good.**\n\n_Reviewed commit `d15af08348b1`._",
        );
        assert_eq!(a.verdict.as_deref(), Some("looks good"));
        assert_eq!(a.reviewed_sha.as_deref(), Some("d15af08348b1"));
        let a = parse_ankra("## Ankra AI review\nno verdict yet");
        assert_eq!(
            a,
            Ankra {
                verdict: None,
                reviewed_sha: None
            }
        );
        let comments = [
            json!({"author": {"login": "ankra-platform[bot]"}, "body": "## Ankra AI review\n**Verdict: blocking.**\n_Reviewed commit 1234567._"}),
            json!({"author": {"login": "gylleus"}, "body": "## Ankra AI review (quoted)"}),
        ];
        assert_eq!(
            ankra_of(&comments).unwrap().verdict.as_deref(),
            Some("blocking")
        );
        assert_eq!(ankra_of(&comments[1..]), None);
        assert!(same_sha("92b1c94", "92B1C9463BF2"));
        assert!(!same_sha("92b1c9", "92b1c9463bf2"), "too short to trust");
        assert!(!same_sha("72179e7", "92b1c9463bf2"));
    }

    #[test]
    fn gh_failures_are_classified() {
        assert!(matches!(
            classify_failure("To get started with GitHub CLI, please run:  gh auth login"),
            GhError::Unavailable(_)
        ));
        assert_eq!(
            classify_failure("GraphQL: Could not resolve to a PullRequest with the number of 9.\n"),
            GhError::Failed(
                "GraphQL: Could not resolve to a PullRequest with the number of 9.".into()
            )
        );
        let missing = GhCli {
            binary: "/nonexistent/gh-for-stronghold-tests".into(),
        };
        assert!(matches!(
            missing.view("https://github.com/o/r/pull/1", 0),
            Err(GhError::Unavailable(_))
        ));
    }
}

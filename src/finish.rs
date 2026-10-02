//! Agents ending their own sessions (`finish_session`): the guards that keep an agent from
//! leaving with work stranded, and the record of how each finished session ended.
//!
//! The MCP server checks the agent's git worktree ([`git_problems`]); the app checks the fleet
//! ([`fleet_problems`]: workers belong to their coordinator, a coordinator's open tasks, a PR
//! still in Karl's review queue). Past the
//! guards the app records a [`FinishedSession`] in `<data dir>/finished.jsonl`, closes the
//! terminal with the same plan as `close_agents`, and shows the node as done while it fades
//! out. The worktree and branch are kept: removing them is the coordinator's post-merge cleanup
//! or Karl's call, never the leaving agent's.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::close::has_active_dispatch;
use crate::control::ControlError;
use crate::control::protocol::Params;
use crate::model::{FleetSnapshot, Node, NodeKind};
use crate::review::mark::Origin;

/// The record file in the data dir.
pub const LOG_FILE: &str = "finished.jsonl";
/// The record file is trimmed to its newest half past this size.
const MAX_LOG_BYTES: u64 = 512 * 1024;
/// How long after the reply the terminal is closed, so the agent receives it.
pub const CLOSE_DELAY_MS: i64 = 2_000;
/// Longest summary kept.
pub const MAX_SUMMARY_CHARS: usize = 300;
/// Paths `git status` may show without counting as uncommitted work: the beads export.
const IGNORED_DIRTY: &[&str] = &[".beads/"];

/// How a session ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishOutcome {
    /// The work landed (merged, or nothing to merge) and nothing is left.
    #[default]
    Done,
    /// The work is with someone else now (a PR Karl or another agent takes over, a merge
    /// handled elsewhere) and this terminal isn't needed for it.
    HandedOver,
    /// Karl called the work off.
    Abandoned,
}

impl FinishOutcome {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "done" => Some(Self::Done),
            "handed_over" => Some(Self::HandedOver),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::HandedOver => "handed_over",
            Self::Abandoned => "abandoned",
        }
    }

    /// "Finished", "Handed over", "Abandoned".
    pub fn verb(self) -> &'static str {
        match self {
            Self::Done => "Finished",
            Self::HandedOver => "Handed over",
            Self::Abandoned => "Abandoned",
        }
    }
}

/// One agent session that ended itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishedSession {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    pub label: String,
    pub outcome: FinishOutcome,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// Guard problems the agent overrode with `force`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forced: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub at_ms: i64,
}

/// The finished sessions the app knows of, newest last, mirrored to a file.
#[derive(Debug, Default)]
pub struct FinishLog {
    path: Option<PathBuf>,
    entries: Vec<FinishedSession>,
}

impl FinishLog {
    /// `<data dir>/finished.jsonl`.
    pub fn default_path() -> PathBuf {
        crate::claude::hook::data_dir().join(LOG_FILE)
    }

    /// Loads the entries newer than `since_ms` from `path`; records are appended there.
    pub fn load(path: PathBuf, since_ms: i64) -> Self {
        let entries = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<FinishedSession>(l).ok())
            .filter(|f| f.at_ms >= since_ms)
            .collect();
        Self {
            path: Some(path),
            entries,
        }
    }

    /// Records `finished`; a write error is logged, the entry is kept in memory.
    pub fn record(&mut self, finished: FinishedSession) {
        if let Some(path) = &self.path
            && let Err(e) = append(path, &finished)
        {
            log::warn!("{}: {e}", path.display());
        }
        self.entries.push(finished);
    }

    /// Entries from the last `window_ms`.
    pub fn recent(&self, now_ms: i64, window_ms: i64) -> impl Iterator<Item = &FinishedSession> {
        self.entries
            .iter()
            .filter(move |f| now_ms - f.at_ms <= window_ms)
    }

    /// Drops entries older than `window_ms` from memory (the file keeps them).
    pub fn prune(&mut self, now_ms: i64, window_ms: i64) {
        self.entries.retain(|f| now_ms - f.at_ms <= window_ms);
    }
}

fn append(path: &Path, finished: &FinishedSession) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let text = fs::read_to_string(path)?;
        let lines: Vec<&str> = text.lines().collect();
        let kept = lines[lines.len() / 2..].join("\n");
        fs::write(path, format!("{kept}\n"))?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(finished).map_err(io::Error::other)?;
    writeln!(file, "{line}")
}

/// A `finish_session` call, carried to the app as the control API's `finish` params.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishRequest {
    pub summary: String,
    pub outcome: FinishOutcome,
    pub pr_url: Option<String>,
    /// Override the guards' problems (not a refusal).
    pub force: bool,
    /// Problems found outside the app (the git worktree), overridden by `force`.
    pub forced: Vec<String>,
    /// Only check: report the caller's node and the fleet's problems.
    pub dry_run: bool,
    pub origin: Origin,
}

impl FinishRequest {
    pub fn to_params(&self) -> Value {
        json!({
            "summary": self.summary,
            "outcome": self.outcome.name(),
            "pr_url": self.pr_url,
            "force": self.force,
            "forced": self.forced,
            "dry_run": self.dry_run,
            "origin": self.origin,
        })
    }

    pub fn from_params(params: &Value) -> Result<Self, ControlError> {
        let p = Params::new(params);
        let summary = clip(p.str("summary")?.unwrap_or_default(), MAX_SUMMARY_CHARS);
        let dry_run = p.bool("dry_run")?.unwrap_or(false);
        if summary.is_empty() && !dry_run {
            return Err(ControlError::invalid_params("\"summary\" is empty"));
        }
        let outcome = match p.str("outcome")?.map(str::trim).filter(|o| !o.is_empty()) {
            None => FinishOutcome::Done,
            Some(o) => FinishOutcome::parse(o).ok_or_else(|| {
                ControlError::invalid_params(format!(
                    "unknown outcome {o:?}; use done, handed_over or abandoned"
                ))
            })?,
        };
        Ok(Self {
            summary,
            outcome,
            pr_url: p
                .str("pr_url")?
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_owned),
            force: p.bool("force")?.unwrap_or(false),
            forced: p.strings("forced")?.unwrap_or_default(),
            dry_run,
            origin: origin_of(params)?,
        })
    }
}

/// The `origin` object of control params.
pub fn origin_of(params: &Value) -> Result<Origin, ControlError> {
    match params.get("origin") {
        None | Some(Value::Null) => Ok(Origin::default()),
        Some(o) => serde_json::from_value(o.clone()).map_err(|e| {
            ControlError::invalid_params(format!("\"origin\" must be an object: {e}"))
        }),
    }
}

/// What the fleet says about `node` leaving: a refusal no `force` overrides, and problems
/// that `force` does. `open_prs` are the node's PRs still in Karl's review queue.
pub fn fleet_problems(
    node: &Node,
    snapshot: &FleetSnapshot,
    open_prs: &[String],
) -> (Option<String>, Vec<String>) {
    let refusal = match node.kind {
        NodeKind::Queen | NodeKind::Subagent => {
            Some(format!("{} isn't an agent session of its own", node.label))
        }
        NodeKind::Worker if has_active_dispatch(node) => Some(format!(
            "you're a worker in run {} (task {}): report to your coordinator as the worker \
             protocol says (worker_done, merge_ready or an escalation); it reuses or releases \
             your terminal",
            node.run_id.as_deref().unwrap_or("?"),
            node.task_id.as_deref().unwrap_or("?"),
        )),
        NodeKind::Worker => Some(format!(
            "you're a worker in run {}: after worker_done your terminal is your coordinator's \
             to reuse or release (or Karl's to close), so end your turn and idle",
            node.run_id.as_deref().unwrap_or("?"),
        )),
        _ => None,
    };
    let mut problems = Vec::new();
    if matches!(node.kind, NodeKind::Coordinator | NodeKind::Attendant) {
        let runs = snapshot.runs.iter().filter(|r| {
            r.coordinator.as_ref() == Some(&node.id) || node.run_id.as_deref() == Some(&r.id)
        });
        for run in runs {
            let Some(c) = run.task_counts else { continue };
            let open = c.pending + c.ready + c.dispatched + c.blocked;
            if open > 0 {
                problems.push(format!(
                    "run {} still has {open} open task{}: finish or cancel them first",
                    run.objective.as_deref().unwrap_or(&run.id),
                    if open == 1 { "" } else { "s" }
                ));
            }
        }
    }
    for url in open_prs {
        problems.push(format!(
            "{url} is waiting for Karl's review: stay so you can address his comments; finish \
             once it's merged, or when Karl says your terminal isn't needed"
        ));
    }
    (refusal, problems)
}

/// Cuts `s` to `max` characters, ending in `…` when cut.
pub fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// What in the git worktree at `dir` would be stranded if its agent left: uncommitted changes,
/// and commits that are neither merged into the default branch nor pushed. Empty when `dir` is
/// not a git worktree (nothing to check).
pub fn git_problems(dir: &Path) -> Vec<String> {
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    let ok = |args: &[&str]| git(args).is_some();
    if git(&["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return Vec::new();
    }
    let mut problems = Vec::new();

    let dirty: Vec<String> = git(&["status", "--porcelain"])
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            let path = l.get(3..).unwrap_or_default();
            !IGNORED_DIRTY.iter().any(|p| path.starts_with(p))
        })
        .map(str::to_owned)
        .collect();
    if !dirty.is_empty() {
        let shown: Vec<&str> = dirty.iter().take(5).map(|l| l.trim()).collect();
        let more = dirty.len().saturating_sub(shown.len());
        problems.push(format!(
            "{} uncommitted change{} in {} ({}{}): commit them, or drop them if they're scratch",
            dirty.len(),
            if dirty.len() == 1 { "" } else { "s" },
            dir.display(),
            shown.join(", "),
            if more > 0 {
                format!(", +{more} more")
            } else {
                String::new()
            }
        ));
    }

    let Some(head) = git(&["rev-parse", "--verify", "-q", "HEAD"]) else {
        return problems;
    };
    let branch = git(&["symbolic-ref", "--short", "-q", "HEAD"]);
    let bases = default_branches(&git);
    let merged = bases
        .iter()
        .any(|b| ok(&["merge-base", "--is-ancestor", &head, b]));
    let pushed = ok(&["rev-parse", "--verify", "-q", "@{u}"])
        && ok(&["merge-base", "--is-ancestor", &head, "@{u}"]);
    if !merged && !pushed {
        let base = bases.first().map_or("main", String::as_str);
        let ahead = git(&["rev-list", "--count", &format!("{base}..{head}")])
            .and_then(|n| n.parse::<u64>().ok());
        let what = match ahead {
            Some(n) => format!("{n} commit{}", if n == 1 { "" } else { "s" }),
            None => "commits".to_owned(),
        };
        problems.push(format!(
            "{what} on {} {} neither merged into {base} nor pushed: merge or push them (or open \
             the PR) first",
            branch.as_deref().unwrap_or("the detached HEAD"),
            if ahead == Some(1) { "is" } else { "are" },
        ));
    }
    problems
}

/// Branches that count as "merged into": the remote's default branch, and local `main` or
/// `master`.
fn default_branches(git: &impl Fn(&[&str]) -> Option<String>) -> Vec<String> {
    let mut bases = Vec::new();
    if let Some(remote_head) = git(&["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"]) {
        bases.push(remote_head);
    }
    for name in ["main", "master", "origin/main", "origin/master"] {
        if !bases.iter().any(|b| b == name)
            && git(&["rev-parse", "--verify", "-q", &format!("{name}^{{commit}}")]).is_some()
        {
            bases.push(name.to_owned());
        }
    }
    bases
}

#[cfg(test)]
mod tests;

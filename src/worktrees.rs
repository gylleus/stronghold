//! Orca's worktrees on disk: the count in the top bar, and the cleanup that removes the ones
//! nothing needs any more.
//!
//! The cleanup is evaluate → plan → confirm → execute. [`evaluate`] gathers [`Facts`] for every
//! linked worktree Orca lists (git state, live terminals and processes, dispatches, the PR) and
//! [`decide`] turns them into a [`Verdict`]. A worktree is removed only when nothing argues for
//! keeping it: losing unmerged work is far worse than leaving a stale worktree, so anything
//! unknown keeps it. [`execute`] re-checks every candidate right before removing it through
//! `orca worktree rm` (never a raw `rm`), and logs each removal to `worktree-cleanup.jsonl`.
//!
//! The rules match `finish_session`'s guards and the playbook's post-merge cleanup: clean
//! `git status`, and HEAD's work already in the base or on GitHub. That is HEAD an ancestor of
//! the base, every commit patch-equivalent to one in it (a rebase merge), the branch's changes
//! already in its tree (a squash merge), HEAD inside a merged PR's head, or the PR merged or
//! closed with every commit on a remote. Untracked agent notes (`*.md`) and logs (`*.log`)
//! don't keep a worktree: notes and untracked `.orca/` / `.local/` scratch (up to 50 MB)
//! are saved under `~/Documents/agents/notes/` and logs deleted before removal. Unpushed,
//! unmerged idle work is archived locally first; other git-work keep reasons can be explicitly
//! archived. Activity and protection guards always apply. [`archive`] owns recovery artifacts.

pub mod archive;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::orca::spawn::Worktree;
use crate::orca::{
    DispatchStatus, OrcaClient, OrcaError, Terminal, Worker, WorkerState, WorkerTerminalState,
};

/// The removal log in the data dir.
pub const LOG_FILE: &str = "worktree-cleanup.jsonl";
/// Present next to the log while a cleanup runs, from the app or the MCP tool, so the app
/// shows the saw whoever started it.
pub const SWEEP_FILE: &str = "worktree-sweep.json";
/// A worktree created this recently is kept: an agent may be about to start in it.
pub const MIN_AGE_MS: i64 = 2 * 60 * 60 * 1000;
/// `orca worktree rm` deletes the whole directory, which takes a while for a big one.
pub const REMOVE_TIMEOUT: Duration = Duration::from_secs(180);
/// Paths `git status` may show without counting as uncommitted work: the beads export.
const IGNORED_DIRTY: &[&str] = &[".beads/"];
/// Build output directories reported in a worktree's size breakdown.
const BUILD_DIRS: &[&str] = &["target", ".venv", "node_modules", ".yarn"];
/// How deep below the worktree build output directories are looked for.
const BUILD_DEPTH: usize = 4;
/// Smaller build output directories are left out of the size text.
const MIN_BUILD_SHOWN: u64 = 1024 * 1024;
/// How many worktrees are checked at once.
const PARALLEL: usize = 8;
/// PRs fetched per repo to match branches against.
const PR_LIST_LIMIT: u32 = 400;

// ---------------------------------------------------------------------------
// Census: the count in the top bar

/// How many linked worktrees Orca has, per repo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Census {
    pub total: usize,
    /// Repo name and count, most first.
    pub per_repo: Vec<(String, usize)>,
}

/// Counts the linked (non-main) worktrees.
pub fn census(worktrees: &[Worktree]) -> Census {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for w in worktrees.iter().filter(|w| !w.is_main_worktree) {
        *counts.entry(repo_name(w).to_owned()).or_default() += 1;
    }
    let mut per_repo: Vec<(String, usize)> = counts.into_iter().collect();
    per_repo.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Census {
        total: per_repo.iter().map(|(_, n)| n).sum(),
        per_repo,
    }
}

/// The repo's short name: the last part of Orca's project id (`github:owner/repo`), else the
/// folder above an `~/orca/workspaces/<repo>/<name>` worktree, else the repo id.
pub fn repo_name(w: &Worktree) -> &str {
    if let Some(project) = w.project_id.as_deref()
        && let Some(rest) = project.strip_prefix("github:")
        && let Some(name) = rest.rsplit('/').next().filter(|n| !n.is_empty())
    {
        return name;
    }
    let path = Path::new(&w.path);
    if let Some(parent) = path.parent()
        && parent.parent().and_then(Path::file_name) == Some("workspaces".as_ref())
        && let Some(name) = parent.file_name().and_then(|n| n.to_str())
    {
        return name;
    }
    w.repo().unwrap_or("?")
}

/// Directories under the workspaces root (`<root>/<repo>/<name>`) that Orca doesn't list:
/// left behind by a git-only remove or created outside Orca. Shown, never removed here.
/// Hidden ones (Orca's `.orca-worktree-trash`) are skipped.
pub fn orphan_dirs(root: &Path, worktrees: &[Worktree]) -> Vec<PathBuf> {
    let known: HashSet<&Path> = worktrees.iter().map(|w| Path::new(&w.path)).collect();
    let mut orphans = Vec::new();
    let Ok(repos) = fs::read_dir(root) else {
        return orphans;
    };
    for repo in repos.flatten().filter(|e| e.path().is_dir()) {
        let Ok(entries) = fs::read_dir(repo.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if path.is_dir() && !hidden && !known.contains(path.as_path()) {
                orphans.push(path);
            }
        }
    }
    orphans.sort();
    orphans
}

/// `~/orca/workspaces`, where Orca creates worktrees.
pub fn workspaces_root() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().join("orca").join("workspaces"))
}

/// Disk usage of a directory in bytes (`du -sk`); `None` when it can't be measured.
pub fn disk_usage(path: &Path) -> Option<u64> {
    let out = Command::new("du").arg("-sk").arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let kb: u64 = text.split_whitespace().next()?.parse().ok()?;
    Some(kb * 1024)
}

/// A build output directory inside a worktree, relative to it, and its size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildDir {
    pub path: String,
    pub bytes: u64,
}

/// A worktree's disk usage and the build output in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskBreakdown {
    pub total: u64,
    /// Biggest first.
    pub build: Vec<BuildDir>,
}

/// Disk usage of a worktree with its build output (`du -k -d 4`), one walk of the tree.
pub fn disk_breakdown(path: &Path) -> Option<DiskBreakdown> {
    let out = Command::new("du")
        .arg("-k")
        .arg("-d")
        .arg(BUILD_DEPTH.to_string())
        .arg(path)
        .output()
        .ok()?;
    parse_du_breakdown(path, &String::from_utf8_lossy(&out.stdout))
}

/// Parses `du -k -d N <root>` output: the root's line is the total; the outermost
/// [`BUILD_DIRS`] below it are the build output.
pub fn parse_du_breakdown(root: &Path, text: &str) -> Option<DiskBreakdown> {
    let mut total = None;
    let mut build = Vec::new();
    for line in text.lines() {
        let Some((kb, path)) = line.split_once('\t') else {
            continue;
        };
        let Ok(kb) = kb.trim().parse::<u64>() else {
            continue;
        };
        let Ok(rel) = Path::new(path).strip_prefix(root) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            total = Some(kb * 1024);
            continue;
        }
        let parts: Vec<&str> = rel.iter().filter_map(|c| c.to_str()).collect();
        let is_build = |c: &&str| BUILD_DIRS.contains(c);
        if parts.last().is_some_and(is_build) && !parts[..parts.len() - 1].iter().any(is_build) {
            build.push(BuildDir {
                path: parts.join("/"),
                bytes: kb * 1024,
            });
        }
    }
    build.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    Some(DiskBreakdown {
        total: total?,
        build,
    })
}

/// Disk usage of each path, measured in parallel.
pub fn disk_usages(paths: &[PathBuf]) -> HashMap<PathBuf, u64> {
    let mut sizes = HashMap::new();
    for chunk in paths.chunks(PARALLEL) {
        let measured: Vec<(PathBuf, Option<u64>)> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|p| s.spawn(move || (p.clone(), disk_usage(p))))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        sizes.extend(measured.into_iter().filter_map(|(p, b)| Some((p, b?))));
    }
    sizes
}

/// "1.2 GB", "340 MB", "12 KB".
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------
// Facts and the decision

/// A PR's state on GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

impl PrState {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "OPEN" => Some(Self::Open),
            "MERGED" => Some(Self::Merged),
            "CLOSED" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// What is known about a worktree's PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrLookup {
    /// GitHub was asked and has no PR for the branch.
    None,
    Found {
        number: u64,
        state: PrState,
    },
    /// The PR state couldn't be read (no GitHub remote, `gh` failed, the linked PR wasn't
    /// found).
    Unknown(String),
}

/// A worktree's git state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitFacts {
    /// Exact untracked paths under top-level agent scratch directories.
    pub scratch: Vec<String>,
    /// A recovery copy cannot safely include all git work (unsupported files or size cap).
    pub archive_error: Option<String>,
    /// Scratch cannot be preserved under the notes cap; blocks ordinary removal too.
    pub scratch_error: Option<String>,
    pub head: String,
    /// Local branch, `None` on a detached HEAD.
    pub branch: Option<String>,
    /// `git status --porcelain` lines for tracked files that count as uncommitted work.
    pub dirty: Vec<String>,
    /// Untracked agent notes (`*.md`), relative paths: saved before removal.
    pub notes: Vec<String>,
    /// Untracked logs (`*.log`), relative paths: deleted before removal.
    pub logs: Vec<String>,
    /// Other untracked files, grouped by their top-level folder (`demo-check/`) or file, with
    /// their size: they keep the worktree until Karl decides.
    pub untracked: Vec<(String, u64)>,
    /// A base branch HEAD is an ancestor of, e.g. `main` or `origin/main`.
    pub merged_into: Option<String>,
    /// A remote-tracking ref HEAD is an ancestor of.
    pub on_remote: Option<String>,
    /// Commits on HEAD not in the first base branch.
    pub ahead: Option<u64>,
    /// A base branch that has a patch-equivalent commit for every commit on HEAD
    /// (`git cherry`: a rebase or cherry-pick merge).
    pub patches_in: Option<String>,
    /// A base branch whose tree already has all of HEAD's changes: merging HEAD into it changes
    /// nothing (a squash merge).
    pub content_in: Option<String>,
    /// A merged PR whose head commit is HEAD or has HEAD in its history, so GitHub keeps every
    /// commit (`refs/pull/<n>/head`) even after the branch is deleted.
    pub merged_pr_head: Option<u64>,
}

/// Everything [`decide`] looks at for one worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub is_main: bool,
    pub pinned: bool,
    /// An agent Karl parked ([`crate::park`]) lives in it.
    pub parked: bool,
    pub exists: bool,
    /// Milliseconds since Orca created it, when known.
    pub age_ms: Option<i64>,
    /// Orca terminals open in it.
    pub terminals: Vec<String>,
    /// Other processes whose working directory is inside it.
    pub processes: Vec<String>,
    /// Dispatches still running in it.
    pub dispatches: Vec<String>,
    pub git: Result<GitFacts, String>,
    pub pr: PrLookup,
}

/// Keep or remove, and why.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    #[serde(default)]
    pub archivable: bool,
    #[serde(default)]
    pub archive_blockers: Vec<String>,
    #[serde(default)]
    pub archive_required: bool,
    #[serde(default)]
    pub scratch: Vec<String>,
    pub remove: bool,
    /// For a kept worktree, every reason to keep it; for a removed one, why it is safe.
    pub reasons: Vec<String>,
    /// For a removed one: untracked notes to save under [`notes_root`] first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// For a removed one: untracked logs to delete first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
}

impl Verdict {
    fn keep(reasons: Vec<String>) -> Self {
        Self {
            remove: false,
            reasons,
            notes: Vec::new(),
            logs: Vec::new(),
            ..Self::default()
        }
    }
}

/// Decides ordinary removal, automatic archive-first removal, and explicit archive eligibility.
/// Activity/protection guards apply to every path; unknown PR state permits explicit archive only.
pub fn decide(facts: &Facts) -> Verdict {
    let mut verdict = decide_regular(facts);
    // Remove only git-work reasons from a second decision: all activity guards still apply.
    let mut guards = facts.clone();
    guards.git = facts
        .git
        .as_ref()
        .map(|_| GitFacts {
            merged_into: Some("archive".into()),
            ..GitFacts::default()
        })
        .map_err(Clone::clone);
    if matches!(guards.pr, PrLookup::Unknown(_)) {
        guards.pr = PrLookup::None;
    }
    let guard_verdict = decide_regular(&guards);
    let mut blockers = if guard_verdict.remove {
        Vec::new()
    } else {
        guard_verdict.reasons
    };
    if let Ok(git) = &facts.git {
        if let Some(error) = &git.archive_error {
            blockers.push(error.clone());
            if !verdict.remove {
                verdict.reasons.push(format!("cannot archive: {error}"));
            }
        }
        if let Some(error) = &git.scratch_error {
            blockers.push(error.clone());
            verdict.remove = false;
            verdict.reasons.push(error.clone());
        }
        if verdict.remove {
            verdict.scratch = git.scratch.clone();
        }
        let unmerged = git.merged_into.is_none()
            && git.patches_in.is_none()
            && git.content_in.is_none()
            && git.merged_pr_head.is_none();
        if blockers.is_empty()
            && unmerged
            && git.on_remote.is_none()
            && !matches!(facts.pr, PrLookup::Unknown(_))
        {
            verdict.remove = true;
            verdict.archive_required = true;
            verdict.reasons = vec!["unpushed and no dwarf in it: archived first".into()];
        }
    }
    verdict.archivable = !verdict.remove && blockers.is_empty();
    verdict.archive_blockers = blockers;
    verdict
}

fn decide_regular(facts: &Facts) -> Verdict {
    let mut keep = Vec::new();
    if facts.is_main {
        keep.push("main checkout".to_owned());
    }
    if facts.pinned {
        keep.push("pinned in Orca".to_owned());
    }
    if facts.parked {
        keep.push("a parked agent sleeps in it (kept for later)".to_owned());
    }
    if !facts.exists {
        keep.push("directory is missing; nothing to free, left for Orca".to_owned());
        return Verdict::keep(keep);
    }
    if let Some(age) = facts.age_ms
        && age < MIN_AGE_MS
    {
        keep.push(format!("created {} min ago", (age / 60_000).max(0)));
    }
    if !facts.terminals.is_empty() {
        keep.push(format!("live terminal: {}", facts.terminals.join(", ")));
    }
    if !facts.processes.is_empty() {
        keep.push(format!(
            "process running in it: {}",
            shortlist(&facts.processes, 3)
        ));
    }
    if !facts.dispatches.is_empty() {
        keep.push(format!("active dispatch: {}", facts.dispatches.join(", ")));
    }
    match &facts.pr {
        PrLookup::Found {
            number,
            state: PrState::Open,
        } => keep.push(format!("PR #{number} is open")),
        PrLookup::Unknown(why) => keep.push(format!("PR state unknown ({why})")),
        _ => {}
    }
    let git = match &facts.git {
        Ok(git) => git,
        Err(e) => {
            keep.push(format!("git state unreadable: {e}"));
            return Verdict::keep(keep);
        }
    };
    if !git.dirty.is_empty() {
        let shown: Vec<&str> = git.dirty.iter().take(3).map(|l| l.trim()).collect();
        let more = git.dirty.len() - shown.len();
        keep.push(format!(
            "{} uncommitted change{} ({}{})",
            git.dirty.len(),
            plural(git.dirty.len()),
            shown.join(", "),
            if more > 0 {
                format!(", +{more} more")
            } else {
                String::new()
            }
        ));
    }
    if !git.untracked.is_empty() {
        let listed: Vec<String> = git
            .untracked
            .iter()
            .map(|(path, bytes)| format!("{path} ({})", format_bytes(*bytes)))
            .collect();
        keep.push(format!(
            "untracked files need a decision: {}",
            shortlist(&listed, 4)
        ));
    }
    let safe_because = if let Some(base) = &git.merged_into {
        Some(match git.ahead {
            Some(0) | None => format!("everything is in {base}"),
            Some(_) => format!("merged into {base}"),
        })
    } else if let Some(base) = &git.patches_in {
        Some(format!("every commit's patch is in {base}"))
    } else if let Some(base) = &git.content_in {
        Some(format!("its changes are already in {base} (squash-merged)"))
    } else if let Some(number) = git.merged_pr_head {
        Some(format!("HEAD is in merged PR #{number}'s head"))
    } else {
        let ahead = match git.ahead {
            Some(n) => format!("{n} commit{}", plural(n as usize)),
            None => "commits".to_owned(),
        };
        match (&facts.pr, &git.on_remote) {
            (
                PrLookup::Found {
                    number,
                    state: state @ (PrState::Merged | PrState::Closed),
                },
                Some(remote),
            ) => Some(format!(
                "PR #{number} {}; HEAD is on {remote}",
                if *state == PrState::Merged {
                    "merged"
                } else {
                    "closed"
                }
            )),
            (
                PrLookup::Found {
                    state: PrState::Merged | PrState::Closed,
                    ..
                },
                None,
            ) => {
                keep.push(format!("{ahead} not on any remote"));
                None
            }
            _ => {
                keep.push(format!("{ahead} not merged into the base"));
                None
            }
        }
    };
    match safe_because {
        Some(reason) if keep.is_empty() => {
            let mut reasons = vec![reason];
            if let Some(first) = cleanup_note(&git.notes, &git.logs) {
                reasons.push(first);
            }
            Verdict {
                remove: true,
                reasons,
                notes: git.notes.clone(),
                logs: git.logs.clone(),
                ..Verdict::default()
            }
        }
        _ => Verdict::keep(keep),
    }
}

/// "first saves 3 notes (PLAN.md, …) and deletes 1 log".
fn cleanup_note(notes: &[String], logs: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    if !notes.is_empty() {
        parts.push(format!(
            "saves {} note{} ({})",
            notes.len(),
            plural(notes.len()),
            shortlist(notes, 3)
        ));
    }
    if !logs.is_empty() {
        parts.push(format!("deletes {} log{}", logs.len(), plural(logs.len())));
    }
    (!parts.is_empty()).then(|| format!("first {}", parts.join(" and ")))
}

/// The first `n` items, then "+k more".
fn shortlist(items: &[String], n: usize) -> String {
    let mut text = items.iter().take(n).cloned().collect::<Vec<_>>().join(", ");
    if items.len() > n {
        text.push_str(&format!(", +{} more", items.len() - n));
    }
    text
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Whether a dispatch from `worker-list` is still running (the same rule as closing an agent).
pub fn worker_is_live(w: &Worker) -> bool {
    if w.terminal_state == Some(WorkerTerminalState::Active) {
        return true;
    }
    let settled_worker = matches!(
        w.worker_state,
        Some(WorkerState::Succeeded | WorkerState::Failed | WorkerState::Stopped)
    );
    let settled_dispatch = matches!(
        w.dispatch_status,
        Some(DispatchStatus::Completed | DispatchStatus::Failed | DispatchStatus::CircuitBroken)
    );
    !(settled_worker || settled_dispatch)
}

/// `git status --porcelain` lines that are real work (the beads export doesn't count).
pub fn dirty_lines(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| {
            let path = l.get(3..).unwrap_or_default();
            !IGNORED_DIRTY.iter().any(|p| path.starts_with(p))
        })
        .map(str::to_owned)
        .collect()
}

/// `git status --porcelain` split into what decides a worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusSplit {
    /// Lines for tracked files.
    pub dirty: Vec<String>,
    /// Untracked `*.md` paths.
    pub notes: Vec<String>,
    /// Untracked `*.log` paths.
    pub logs: Vec<String>,
    /// Every other untracked path.
    pub other: Vec<String>,
}

/// Splits `git status --porcelain --untracked-files=all` output. A quoted path (special
/// characters) is never a note or a log, so it lands in `other` and keeps the worktree.
pub fn split_status(porcelain: &str) -> StatusSplit {
    let mut split = StatusSplit::default();
    for line in dirty_lines(porcelain) {
        let Some(path) = line.strip_prefix("?? ") else {
            split.dirty.push(line);
            continue;
        };
        let lower = path.to_ascii_lowercase();
        let plain = !path.starts_with('"') && safe_relative(path);
        if plain && lower.ends_with(".md") {
            split.notes.push(path.to_owned());
        } else if plain && lower.ends_with(".log") {
            split.logs.push(path.to_owned());
        } else {
            split.other.push(path.to_owned());
        }
    }
    split
}

/// A relative path that stays inside the directory it is relative to.
fn safe_relative(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Untracked paths grouped by their first component (`demo-check/` for anything in it), with
/// the summed size of their files, biggest first.
pub fn group_untracked(dir: &Path, paths: &[String]) -> Vec<(String, u64)> {
    let mut groups: BTreeMap<String, u64> = BTreeMap::new();
    for path in paths {
        let key = match path.split_once('/') {
            Some((top, _)) => format!("{top}/"),
            None => path.clone(),
        };
        let bytes = fs::symlink_metadata(dir.join(path)).map_or(0, |m| m.len());
        *groups.entry(key).or_default() += bytes;
    }
    let mut groups: Vec<(String, u64)> = groups.into_iter().collect();
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    groups
}

/// One process and its working directory, from `lsof -d cwd -Fpcn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCwd {
    pub pid: u32,
    pub command: String,
    pub cwd: PathBuf,
}

/// Parses `lsof -d cwd -Fpcn` output: `p<pid>`, `c<command>`, `n<path>` lines.
pub fn parse_lsof_cwd(text: &str) -> Vec<ProcessCwd> {
    let mut out = Vec::new();
    let (mut pid, mut command) = (None, String::new());
    for line in text.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => {
                pid = value.parse().ok();
                command.clear();
            }
            "c" => command = value.to_owned(),
            "n" => {
                if let Some(pid) = pid {
                    out.push(ProcessCwd {
                        pid,
                        command: command.clone(),
                        cwd: PathBuf::from(value),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// The processes working inside `dir`, as `command (pid)`, excluding `own_pid`. Only the
/// working directory counts: a process elsewhere whose command line merely mentions the path
/// (an orphaned watch loop from a dead session) doesn't hold the worktree.
pub fn processes_inside(dir: &Path, processes: &[ProcessCwd], own_pid: u32) -> Vec<String> {
    processes
        .iter()
        .filter(|p| p.pid != own_pid && p.cwd.starts_with(dir))
        .map(|p| format!("{} ({})", p.command, p.pid))
        .collect()
}

/// The Orca terminals open in a worktree, by title (else handle).
pub fn terminals_in(w: &Worktree, terminals: &[Terminal]) -> Vec<String> {
    terminals
        .iter()
        .filter(|t| {
            t.worktree_id.as_deref() == Some(w.id.as_str())
                || t.worktree_path
                    .as_deref()
                    .is_some_and(|p| Path::new(p).starts_with(&w.path))
        })
        .map(|t| {
            t.title
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&t.handle)
                .to_owned()
        })
        .collect()
}

/// The live dispatches whose worker resource is this worktree.
pub fn dispatches_in(w: &Worktree, workers: &[Worker]) -> Vec<String> {
    workers
        .iter()
        .filter(|d| {
            d.resource.as_ref().and_then(|r| r.worktree_id.as_deref()) == Some(w.id.as_str())
        })
        .filter(|d| worker_is_live(d))
        .map(|d| d.dispatch_id.clone())
        .collect()
}

/// One PR from `gh pr list --json number,state,headRefName,headRefOid`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrRow {
    pub number: u64,
    pub state: String,
    #[serde(default)]
    pub head_ref_name: String,
    #[serde(default)]
    pub head_ref_oid: String,
}

/// The PR for a worktree: its linked PR when Orca knows one, else the PRs whose head is the
/// branch (an open one wins, else the newest). `prs` is `Err` when GitHub couldn't be asked.
pub fn pr_for(w: &Worktree, branch: Option<&str>, prs: &Result<Vec<PrRow>, String>) -> PrLookup {
    let prs = match prs {
        Ok(prs) => prs,
        Err(e) => return PrLookup::Unknown(e.clone()),
    };
    let found = |row: &PrRow| match PrState::parse(&row.state) {
        Some(state) => PrLookup::Found {
            number: row.number,
            state,
        },
        None => PrLookup::Unknown(format!("PR #{} state {:?}", row.number, row.state)),
    };
    if let Some(number) = w.linked_pr_number() {
        return prs
            .iter()
            .find(|p| p.number == number)
            .map(found)
            .unwrap_or_else(|| PrLookup::Unknown(format!("linked PR #{number} not found")));
    }
    let heads: Vec<&str> = w
        .push_target
        .as_ref()
        .and_then(|t| t.branch_name.as_deref())
        .into_iter()
        .chain(branch)
        .collect();
    let mut matching: Vec<&PrRow> = prs
        .iter()
        .filter(|p| heads.contains(&p.head_ref_name.as_str()))
        .collect();
    matching.sort_by_key(|p| std::cmp::Reverse(p.number));
    match matching
        .iter()
        .find(|p| PrState::parse(&p.state) == Some(PrState::Open))
        .or(matching.first())
    {
        Some(row) => found(row),
        None => PrLookup::None,
    }
}

// ---------------------------------------------------------------------------
// The plan

/// One linked worktree in the plan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    /// Orca's `<repoId>::<path>`.
    pub id: String,
    pub name: String,
    pub repo: String,
    pub path: String,
    pub branch: Option<String>,
    pub bytes: Option<u64>,
    /// Build output inside it, biggest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build: Vec<BuildDir>,
    pub verdict: Verdict,
}

impl PlanEntry {
    /// "12 GB: target 9.1 GB, .venv 1.2 GB" (build output of 1 MB or more); empty when
    /// unmeasured.
    pub fn size_text(&self) -> String {
        let Some(bytes) = self.bytes else {
            return String::new();
        };
        let mut text = format_bytes(bytes);
        let parts: Vec<String> = self
            .build
            .iter()
            .filter(|b| b.bytes >= MIN_BUILD_SHOWN)
            .map(|b| format!("{} {}", b.path, format_bytes(b.bytes)))
            .collect();
        if !parts.is_empty() {
            text.push_str(&format!(": {}", shortlist(&parts, 4)));
        }
        text
    }

    /// Where its notes are saved: `<notes root>/<repo>/<worktree folder>`.
    pub fn notes_dir(&self, root: &Path) -> PathBuf {
        let folder = Path::new(&self.path)
            .file_name()
            .map_or_else(|| self.name.clone(), |n| n.to_string_lossy().into_owned());
        root.join(&self.repo).join(folder)
    }
}

/// The evaluated cleanup: every linked worktree with its verdict.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub entries: Vec<PlanEntry>,
    /// Directories under the workspaces root Orca doesn't list (never removed here).
    pub orphans: Vec<String>,
    pub evaluated_at_ms: i64,
}

impl Plan {
    pub fn removable(&self) -> impl Iterator<Item = &PlanEntry> {
        self.entries.iter().filter(|e| e.verdict.remove)
    }

    pub fn kept(&self) -> impl Iterator<Item = &PlanEntry> {
        self.entries.iter().filter(|e| !e.verdict.remove)
    }

    /// Bytes the removals would free (the measured ones).
    pub fn freed_bytes(&self) -> u64 {
        self.removable().filter_map(|e| e.bytes).sum()
    }

    /// Bytes the kept worktrees hold (the measured ones).
    pub fn kept_bytes(&self) -> u64 {
        self.kept().filter_map(|e| e.bytes).sum()
    }

    /// A plain-text rendering, for the MCP tool and logs.
    pub fn text(&self) -> String {
        let removable: Vec<&PlanEntry> = self.removable().collect();
        let mut out = vec![format!(
            "{} linked worktrees: {} to remove (frees {}), {} kept (holding {}).",
            self.entries.len(),
            removable.len(),
            format_bytes(self.freed_bytes()),
            self.entries.len() - removable.len(),
            format_bytes(self.kept_bytes())
        )];
        if !removable.is_empty() {
            out.push("Remove:".into());
            for e in &removable {
                out.push(format!(
                    "- {}/{} ({}): {}",
                    e.repo,
                    e.name,
                    e.bytes.map_or("size ?".into(), format_bytes),
                    e.verdict.reasons.join("; ")
                ));
                if !e.verdict.notes.is_empty()
                    && let Some(root) = notes_root()
                {
                    out.push(format!("  notes go to {}", e.notes_dir(&root).display()));
                }
            }
        }
        let kept: Vec<&PlanEntry> = self.kept().collect();
        if !kept.is_empty() {
            out.push("Keep:".into());
            for e in kept {
                let size = e.size_text();
                out.push(format!(
                    "- {}/{}{}: {}",
                    e.repo,
                    e.name,
                    if size.is_empty() {
                        String::new()
                    } else {
                        format!(" ({size})")
                    },
                    e.verdict.reasons.join("; ")
                ));
            }
        }
        if !self.orphans.is_empty() {
            out.push(format!(
                "Directories Orca doesn't list (not touched): {}",
                self.orphans.join(", ")
            ));
        }
        out.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Gathering

/// The clan-wide inputs one evaluation shares across worktrees.
struct Context {
    terminals: Vec<Terminal>,
    workers: Vec<Worker>,
    processes: Vec<ProcessCwd>,
    /// Worktree paths of parked agents.
    parked: BTreeSet<String>,
    /// PRs per repo id.
    prs: HashMap<String, Result<Vec<PrRow>, String>>,
    now_ms: i64,
}

impl Context {
    /// Fetches terminals, dispatches and processes; any failure aborts (unknown means keep).
    fn fetch(client: &OrcaClient, now_ms: i64) -> Result<Self, String> {
        let terminals = client
            .terminal_list()
            .map_err(|e| format!("terminal list: {e}"))?
            .terminals;
        let workers = client
            .worker_list(None)
            .map_err(|e| format!("worker list: {e}"))?
            .workers;
        let processes = process_cwds()?;
        Ok(Self {
            terminals,
            workers,
            processes,
            parked: crate::park::parked_worktrees(&crate::park::Parking::default_path()),
            prs: HashMap::new(),
            now_ms,
        })
    }

    fn facts(&self, w: &Worktree) -> Facts {
        let path = Path::new(&w.path);
        let exists = path.is_dir();
        let mut git = if exists {
            git_facts(path, w.base_ref.as_deref())
        } else {
            Err("missing".into())
        };
        let no_prs = Err("not looked up".to_owned());
        let prs = w.repo().and_then(|r| self.prs.get(r)).unwrap_or(&no_prs);
        if let (Ok(g), Ok(rows)) = (&mut git, prs)
            && g.merged_into.is_none()
        {
            g.merged_pr_head = merged_pr_containing(path, &g.head, rows);
        }
        let branch = git.as_ref().ok().and_then(|g| g.branch.clone());
        Facts {
            is_main: w.is_main_worktree,
            pinned: w.is_pinned,
            parked: self.parked.contains(w.path.trim_end_matches('/')),
            exists,
            age_ms: w.created_at.map(|c| self.now_ms - c),
            terminals: terminals_in(w, &self.terminals),
            processes: processes_inside(path, &self.processes, std::process::id()),
            dispatches: dispatches_in(w, &self.workers),
            pr: pr_for(w, branch.as_deref(), prs),
            git,
        }
    }
}

/// Every process's working directory. Failing to list them fails the evaluation.
fn process_cwds() -> Result<Vec<ProcessCwd>, String> {
    let out = Command::new("lsof")
        .args(["-n", "-P", "-d", "cwd", "-Fpcn"])
        .output()
        .map_err(|e| format!("lsof: {e}"))?;
    // lsof exits 1 when some processes couldn't be read; the rest is still listed.
    let list = parse_lsof_cwd(&String::from_utf8_lossy(&out.stdout));
    if list.is_empty() {
        return Err(format!(
            "lsof listed no processes ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(list)
}

/// The PRs of the repo checked out at `repo_path`, newest first.
fn repo_prs(repo_path: &Path) -> Result<Vec<PrRow>, String> {
    let out = Command::new(gh_binary())
        .current_dir(repo_path)
        .args([
            "pr",
            "list",
            "--state",
            "all",
            "--limit",
            &PR_LIST_LIMIT.to_string(),
            "--json",
            "number,state,headRefName,headRefOid",
        ])
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "gh pr list: {}",
            err.lines().next().unwrap_or("failed").trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("gh pr list: {e}"))
}

/// `gh` from `PATH`, else Homebrew's (apps launched from Finder lack the shell `PATH`).
fn gh_binary() -> PathBuf {
    for dir in ["/opt/homebrew/bin/gh", "/usr/local/bin/gh"] {
        if Path::new(dir).exists() {
            return dir.into();
        }
    }
    "gh".into()
}

/// Reads one worktree's git state.
pub fn git_facts(dir: &Path, base_ref: Option<&str>) -> Result<GitFacts, String> {
    let git = |args: &[&str]| -> Result<(bool, String), String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map_err(|e| format!("git: {e}"))?;
        Ok((
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        ))
    };
    let (ok, status) = git(&[
        "-c",
        "core.quotePath=false",
        "status",
        "--porcelain",
        "--untracked-files=all",
    ])?;
    if !ok {
        return Err("git status failed".into());
    }
    let (ok, head) = git(&["rev-parse", "--verify", "-q", "HEAD"])?;
    if !ok || head.is_empty() {
        return Err("no HEAD".into());
    }
    let (_, branch) = git(&["symbolic-ref", "--short", "-q", "HEAD"])?;
    let bases = base_branches(base_ref, |r| {
        git(&["rev-parse", "--verify", "-q", r]).is_ok_and(|(ok, _)| ok)
    });
    let mut merged_into = None;
    for base in &bases {
        if git(&["merge-base", "--is-ancestor", &head, base])?.0 {
            merged_into = Some(base.clone());
            break;
        }
    }
    let ahead = bases.first().and_then(|b| {
        git(&["rev-list", "--count", &format!("{b}..{head}")])
            .ok()
            .filter(|(ok, _)| *ok)
            .and_then(|(_, n)| n.parse().ok())
    });
    let (remote_ok, remotes) = git(&[
        "for-each-ref",
        "--contains",
        &head,
        "--format=%(refname:short)",
        "refs/remotes",
    ])?;
    if !remote_ok {
        return Err("remote-tracking refs unreadable".into());
    }
    let on_remote = remotes
        .lines()
        .map(str::trim)
        .find(|r| !r.is_empty() && !r.ends_with("/HEAD"))
        .map(str::to_owned);
    let (mut patches_in, mut content_in) = (None, None);
    if merged_into.is_none() {
        for base in &bases {
            let (ok, cherry) = git(&["cherry", base, &head])?;
            if ok && all_patches_upstream(&cherry) {
                patches_in = Some(base.clone());
                break;
            }
        }
        for base in &bases {
            let (ok, merged) = git(&["merge-tree", "--write-tree", base, &head])?;
            let (tree_ok, tree) = git(&["rev-parse", &format!("{base}^{{tree}}")])?;
            if ok && tree_ok && merged.lines().next() == Some(tree.as_str()) {
                content_in = Some(base.clone());
                break;
            }
        }
    }
    archive::check_supported(dir)?;
    let mut split = split_status(&status);
    let files = archive::untracked(dir)?;
    let scratch: Vec<_> = files
        .iter()
        .filter(|p| archive::is_scratch(p))
        .cloned()
        .collect();
    split.notes.retain(|p| !archive::is_scratch(p));
    split.logs.retain(|p| !archive::is_scratch(p));
    split.other.retain(|p| !archive::is_scratch(p));
    let scratch_error = archive::measure(dir, &scratch, archive::SCRATCH_CAP).err();
    let archive_error = archive::measure(dir, &files, archive::UNTRACKED_CAP).err();
    Ok(GitFacts {
        scratch,
        scratch_error,
        archive_error,
        branch: Some(branch).filter(|b| !b.is_empty()),
        untracked: group_untracked(dir, &split.other),
        dirty: split.dirty,
        notes: split.notes,
        logs: split.logs,
        merged_into,
        on_remote,
        ahead,
        patches_in,
        content_in,
        merged_pr_head: None,
        head,
    })
}

/// Whether `git cherry` output marks every commit `-` (an equivalent change is upstream).
pub fn all_patches_upstream(cherry: &str) -> bool {
    let lines: Vec<&str> = cherry.lines().filter(|l| !l.trim().is_empty()).collect();
    !lines.is_empty() && lines.iter().all(|l| l.starts_with("- "))
}

/// The merged PR whose head commit is `head` or has it in its history. Only heads present in
/// the local object store can be checked; nothing is fetched.
pub fn merged_pr_containing(dir: &Path, head: &str, prs: &[PrRow]) -> Option<u64> {
    let mut merged: Vec<&PrRow> = prs
        .iter()
        .filter(|p| PrState::parse(&p.state) == Some(PrState::Merged))
        .filter(|p| is_oid(&p.head_ref_oid))
        .collect();
    merged.sort_by_key(|p| std::cmp::Reverse(p.number));
    if let Some(p) = merged.iter().find(|p| p.head_ref_oid == head) {
        return Some(p.number);
    }
    let present = present_commits(dir, merged.iter().map(|p| p.head_ref_oid.as_str()));
    let merged: Vec<&&PrRow> = merged
        .iter()
        .filter(|p| present.contains(&p.head_ref_oid))
        .collect();
    if merged.is_empty() {
        return None;
    }
    // One walk: nothing left of HEAD once every merged head is excluded means some head has it.
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-list", "--count", head, "--not"])
        .args(merged.iter().map(|p| p.head_ref_oid.as_str()))
        .output()
        .ok()?;
    if !out.status.success() || String::from_utf8_lossy(&out.stdout).trim() != "0" {
        return None;
    }
    merged.into_iter().find_map(|p| {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["merge-base", "--is-ancestor", head, &p.head_ref_oid])
            .status()
            .is_ok_and(|s| s.success())
            .then_some(p.number)
    })
}

fn is_oid(s: &str) -> bool {
    s.len() >= 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commits among `oids` that exist locally (`git cat-file --batch-check`).
fn present_commits<'a>(dir: &Path, oids: impl Iterator<Item = &'a str>) -> HashSet<String> {
    let input: String = oids.map(|o| format!("{o}\n")).collect();
    let child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["cat-file", "--batch-check"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return HashSet::new();
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else {
        return HashSet::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let oid = parts.next()?;
            (parts.next() == Some("commit")).then(|| oid.to_owned())
        })
        .collect()
}

/// Branches that count as the base, in order: the worktree's base ref, `origin/HEAD`'s target,
/// and local and remote `main`/`master` (the same set `finish_session` checks). Only those
/// `exists` says are there.
pub fn base_branches(base_ref: Option<&str>, exists: impl Fn(&str) -> bool) -> Vec<String> {
    let mut bases: Vec<String> = Vec::new();
    let from_base_ref = base_ref.map(|r| {
        r.strip_prefix("refs/remotes/")
            .or_else(|| r.strip_prefix("refs/heads/"))
            .unwrap_or(r)
            .to_owned()
    });
    for name in from_base_ref.into_iter().chain(
        [
            "origin/HEAD",
            "main",
            "master",
            "origin/main",
            "origin/master",
        ]
        .iter()
        .map(|s| s.to_string()),
    ) {
        if !bases.contains(&name) && exists(&name) {
            bases.push(name);
        }
    }
    bases
}

/// Evaluates every linked worktree Orca lists. Every worktree is measured with its build
/// output; `sizes` are known disk usages by path, used where measuring fails.
pub fn evaluate(
    client: &OrcaClient,
    sizes: &HashMap<PathBuf, u64>,
    now_ms: i64,
) -> Result<Plan, String> {
    let list = client
        .worktree_list()
        .map_err(|e| format!("worktree list: {e}"))?;
    if list.truncated {
        return Err("Orca's worktree list was truncated".into());
    }
    let repos = client.repo_list().map_err(|e| format!("repo list: {e}"))?;
    let mut ctx = Context::fetch(client, now_ms)?;
    let linked: Vec<&Worktree> = list
        .worktrees
        .iter()
        .filter(|w| !w.is_main_worktree)
        .collect();
    let repo_ids: HashSet<&str> = linked.iter().filter_map(|w| w.repo()).collect();
    ctx.prs = std::thread::scope(|s| {
        let handles: Vec<_> = repo_ids
            .into_iter()
            .map(|id| {
                let repo = repos.iter().find(|r| r.id == id);
                s.spawn(move || {
                    let prs = match repo {
                        Some(r) => repo_prs(Path::new(&r.path)),
                        None => Err("repo not in Orca's repo list".to_owned()),
                    };
                    (id.to_owned(), prs)
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let mut entries = Vec::new();
    for chunk in linked.chunks(PARALLEL) {
        let ctx = &ctx;
        let decided: Vec<PlanEntry> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|w| s.spawn(move || plan_entry(w, &ctx.facts(w))))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        entries.extend(decided);
    }
    let paths: Vec<PathBuf> = entries.iter().map(|e| PathBuf::from(&e.path)).collect();
    let measured = disk_breakdowns(&paths);
    for e in &mut entries {
        let path = PathBuf::from(&e.path);
        match measured.get(&path) {
            Some(m) => {
                e.bytes = Some(m.total);
                e.build = m.build.clone();
            }
            None => e.bytes = sizes.get(&path).copied(),
        }
    }
    entries.sort_by(|a, b| {
        b.verdict
            .remove
            .cmp(&a.verdict.remove)
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.name.cmp(&b.name))
    });
    let orphans = workspaces_root()
        .map(|root| orphan_dirs(&root, &list.worktrees))
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.display().to_string())
        .collect();
    Ok(Plan {
        entries,
        orphans,
        evaluated_at_ms: now_ms,
    })
}

/// [`disk_breakdown`] of each existing path, measured in parallel.
fn disk_breakdowns(paths: &[PathBuf]) -> HashMap<PathBuf, DiskBreakdown> {
    let mut sizes = HashMap::new();
    for chunk in paths.chunks(PARALLEL) {
        let measured: Vec<(PathBuf, Option<DiskBreakdown>)> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .filter(|p| p.is_dir())
                .map(|p| s.spawn(move || (p.clone(), disk_breakdown(p))))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        sizes.extend(measured.into_iter().filter_map(|(p, b)| Some((p, b?))));
    }
    sizes
}

fn plan_entry(w: &Worktree, facts: &Facts) -> PlanEntry {
    PlanEntry {
        id: w.id.clone(),
        name: w.name().to_owned(),
        repo: repo_name(w).to_owned(),
        path: w.path.clone(),
        branch: w.branch_name().map(str::to_owned),
        bytes: None,
        build: Vec::new(),
        verdict: decide(facts),
    }
}

// ---------------------------------------------------------------------------
// Execution

/// What happened to one planned removal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Removed,
    /// The re-check right before removal found a reason to keep it.
    Skipped {
        reasons: Vec<String>,
    },
    Failed {
        error: String,
    },
}

/// One logged removal attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Removal {
    pub id: String,
    pub repo: String,
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Why it was judged safe.
    pub reason: String,
    /// Where its notes were saved before removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_saved_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_path: Option<String>,
    #[serde(flatten)]
    pub outcome: Outcome,
    pub at_ms: i64,
}

/// `orca worktree rm` for one worktree: no `--force`, so git refuses a dirty one, and Orca
/// keeps any branch it can't prove merged.
pub fn worktree_rm_args(id: &str) -> Vec<String> {
    vec![
        "worktree".into(),
        "rm".into(),
        format!("--worktree=id:{id}"),
    ]
}

/// Removes the plan's removable worktrees, re-checking each first, and logs every attempt to
/// `log`. `progress` is told after each one.
pub fn execute(
    client: &OrcaClient,
    plan: &Plan,
    log: &Path,
    now_ms: impl Fn() -> i64,
    progress: impl FnMut(&Removal),
) -> Result<Vec<Removal>, String> {
    execute_archives(client, plan, &HashSet::new(), log, now_ms, progress)
}

/// Explicit archive selections are checked against fresh facts just like ordinary removals.
pub fn execute_archives(
    client: &OrcaClient,
    plan: &Plan,
    archive_ids: &HashSet<String>,
    log: &Path,
    now_ms: impl Fn() -> i64,
    mut progress: impl FnMut(&Removal),
) -> Result<Vec<Removal>, String> {
    let marker = log.with_file_name(SWEEP_FILE);
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // The sweep marker doubles as exclusion for app and MCP cleanup executions.
    let marker_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .map_err(|e| {
            format!(
                "cleanup already running or sweep marker unavailable ({}): {e}",
                marker.display()
            )
        })?;
    struct SweepGuard(PathBuf);
    impl Drop for SweepGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _sweep = SweepGuard(marker.clone());
    drop(marker_file);
    let list = client
        .worktree_list()
        .map_err(|e| format!("worktree list: {e}"))?;
    let repos = client.repo_list().map_err(|e| format!("repo list: {e}"))?;
    let mut ctx = Context::fetch(client, now_ms())?;
    let remover = client.clone().timeout(REMOVE_TIMEOUT);
    let selected = || {
        plan.entries
            .iter()
            .filter(|e| e.verdict.remove || archive_ids.contains(&e.id))
    };
    let total = selected().count();
    let mut done = Vec::new();
    for entry in selected() {
        write_marker(
            &marker,
            &SweepMarker {
                started_at_ms: plan.evaluated_at_ms,
                updated_at_ms: now_ms(),
                total,
                done: done.len(),
                current: Some(entry.name.clone()),
                current_id: Some(entry.id.clone()),
            },
        );
        let mut removal = Removal {
            id: entry.id.clone(),
            repo: entry.repo.clone(),
            name: entry.name.clone(),
            path: entry.path.clone(),
            branch: entry.branch.clone(),
            bytes: entry.bytes,
            reason: entry.verdict.reasons.join("; "),
            notes_saved_to: None,
            archive_path: None,
            outcome: Outcome::Removed,
            at_ms: 0,
        };
        removal.outcome = match list.worktrees.iter().find(|w| w.id == entry.id) {
            None => Outcome::Skipped {
                reasons: vec!["no longer in Orca's list".into()],
            },
            Some(w) => {
                if let Some(repo_id) = w.repo()
                    && !ctx.prs.contains_key(repo_id)
                {
                    let prs = match repos.iter().find(|r| r.id == repo_id) {
                        Some(r) => repo_prs(Path::new(&r.path)),
                        None => Err("repo not in Orca's repo list".to_owned()),
                    };
                    ctx.prs.insert(repo_id.to_owned(), prs);
                }
                let facts = ctx.facts(w);
                let verdict = decide(&facts);
                let wants_archive = archive_ids.contains(&entry.id) || verdict.archive_required;
                let mut saved = None;
                let tidied = if wants_archive
                    && (verdict.archivable
                        || verdict.archive_required
                        || verdict.remove && verdict.archive_blockers.is_empty())
                {
                    let result = (|| -> Result<(), String> {
                        let root = archive::root().ok_or("no home directory for archive")?;
                        let mut recovery_entry = entry.clone();
                        recovery_entry.verdict = decide_regular(&facts);
                        let archived = archive::prepare(
                            &recovery_entry,
                            w.base_ref.as_deref(),
                            &format!("{:?}", facts.pr),
                            &root,
                        )?;
                        removal.archive_path = Some(archived.path.display().to_string());
                        // Archive creation can take time: refresh activity and pinned state before mutation.
                        let refreshed = client.worktree_list().map_err(|e| e.to_string())?;
                        let current = refreshed
                            .worktrees
                            .iter()
                            .find(|x| x.id == w.id)
                            .ok_or("worktree no longer listed")?;
                        let mut fresh = Context::fetch(client, now_ms())?;
                        if let Some(repo_id) = current.repo() {
                            let prs = match repos.iter().find(|r| r.id == repo_id) {
                                Some(repo) => repo_prs(Path::new(&repo.path)),
                                None => Err("repo not listed".into()),
                            };
                            fresh.prs.insert(repo_id.to_owned(), prs);
                        }
                        let checked = decide(&fresh.facts(current));
                        let allowed = if archive_ids.contains(&entry.id) {
                            checked.archivable
                                || checked.remove && checked.archive_blockers.is_empty()
                        } else {
                            checked.archive_required
                        };
                        if !allowed {
                            return Err(format!(
                                "archive saved, worktree kept: {}",
                                checked.reasons.join("; ")
                            ));
                        }
                        archived.clean(Path::new(&w.path))?;
                        saved = Some(archived);
                        Ok(())
                    })();
                    result.map_err(|error| Outcome::Failed { error })
                } else if !verdict.remove || wants_archive {
                    Err(Outcome::Skipped {
                        reasons: verdict.reasons,
                    })
                } else if verdict.notes.is_empty()
                    && verdict.logs.is_empty()
                    && verdict.scratch.is_empty()
                {
                    Ok(())
                } else {
                    match notes_root() {
                        None => Err(Outcome::Failed {
                            error: "no home directory to save notes in".into(),
                        }),
                        Some(root) => {
                            let dest = entry.notes_dir(&root);
                            let scratch_saved = if verdict.scratch.is_empty() {
                                Ok(dest.clone())
                            } else {
                                archive::save_scratch(Path::new(&w.path), &dest, &verdict.scratch)
                            };
                            match scratch_saved.and_then(|saved_to| {
                                if !verdict.scratch.is_empty() {
                                    removal.notes_saved_to = Some(saved_to.display().to_string());
                                }
                                save_notes(Path::new(&w.path), &dest, &verdict.notes, &verdict.logs)
                            }) {
                                Ok(()) => {
                                    if !verdict.notes.is_empty() {
                                        removal.notes_saved_to = Some(dest.display().to_string());
                                    }
                                    Ok(())
                                }
                                Err(error) => Err(Outcome::Failed { error }),
                            }
                        }
                    }
                };
                let outcome = match tidied {
                    Err(outcome) => outcome,
                    Ok(()) => match remover.call_value(&worktree_rm_args(&w.id)) {
                        Ok(_) => Outcome::Removed,
                        Err(e) => Outcome::Failed {
                            error: rm_error(&e),
                        },
                    },
                };
                if let Some(saved) = saved {
                    match saved.retain_branch() {
                        Ok(()) => outcome,
                        Err(error) => Outcome::Failed { error },
                    }
                } else {
                    outcome
                }
            }
        };
        removal.at_ms = now_ms();
        if let Err(e) = append_log(log, &removal) {
            log::warn!("{}: {e}", log.display());
        }
        progress(&removal);
        done.push(removal);
    }
    Ok(done)
}

/// `~/Documents/agents/notes`, where a removed worktree's notes are saved.
pub fn notes_root() -> Option<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join("Documents").join("agents").join("notes"))
}

/// Copies each note from `dir` to the same relative path under `dest`, checks every copy, and
/// only then deletes the originals and the logs, so `orca worktree rm` (no force) finds the
/// worktree clean. A note never overwrites a different file: it gets a `-2`, `-3`… suffix.
pub fn save_notes(
    dir: &Path,
    dest: &Path,
    notes: &[String],
    logs: &[String],
) -> Result<(), String> {
    if let Some(bad) = notes.iter().chain(logs).find(|p| !safe_relative(p)) {
        return Err(format!("unsafe path {bad:?}"));
    }
    for note in notes {
        let src = dir.join(note);
        let content = fs::read(&src).map_err(|e| format!("read {note}: {e}"))?;
        let target = free_target(&dest.join(note), &content);
        if !target.exists() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
            }
            fs::write(&target, &content).map_err(|e| format!("save {}: {e}", target.display()))?;
        }
        if fs::read(&target).ok().as_deref() != Some(content.as_slice()) {
            return Err(format!("saved copy of {note} doesn't match"));
        }
    }
    for path in notes.iter().chain(logs) {
        fs::remove_file(dir.join(path)).map_err(|e| format!("delete {path}: {e}"))?;
    }
    Ok(())
}

/// `path` if it is free or already holds `content`, else the first free `<stem>-<n>.<ext>`.
fn free_target(path: &Path, content: &[u8]) -> PathBuf {
    let holds = |p: &Path| fs::read(p).ok().as_deref() == Some(content);
    if !path.exists() || holds(path) {
        return path.to_owned();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| path.with_file_name(format!("{stem}-{n}{ext}")))
        .find(|p| !p.exists() || holds(p))
        .expect("an unbounded range finds a free name")
}

/// The sweep marker's contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepMarker {
    pub started_at_ms: i64,
    /// Rewritten before each removal.
    pub updated_at_ms: i64,
    pub total: usize,
    pub done: usize,
    pub current: Option<String>,
    /// The worktree id of `current`, for the saw in the farm.
    #[serde(default)]
    pub current_id: Option<String>,
}

fn write_marker(path: &Path, marker: &SweepMarker) {
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(
            path,
            serde_json::to_vec(marker).map_err(std::io::Error::other)?,
        )
    };
    if let Err(e) = write() {
        log::warn!("{}: {e}", path.display());
    }
}

/// The running sweep, if the marker is there and fresh (a crashed run's marker goes stale
/// once a removal would have timed out).
pub fn running_sweep(path: &Path, now_ms: i64) -> Option<SweepMarker> {
    let marker: SweepMarker = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let stale_after = i64::try_from(REMOVE_TIMEOUT.as_millis()).unwrap_or(i64::MAX) + 60_000;
    (now_ms - marker.updated_at_ms < stale_after).then_some(marker)
}

/// `<data dir>/worktree-sweep.json`.
pub fn default_marker_path() -> PathBuf {
    crate::claude::hook::data_dir().join(SWEEP_FILE)
}

fn rm_error(e: &OrcaError) -> String {
    match e {
        OrcaError::Api(api) => format!("{}: {}", api.code, api.message),
        other => other.to_string(),
    }
}

/// `<data dir>/worktree-cleanup.jsonl`.
pub fn default_log_path() -> PathBuf {
    crate::claude::hook::data_dir().join(LOG_FILE)
}

fn append_log(path: &Path, removal: &Removal) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(removal).map_err(std::io::Error::other)?;
    writeln!(file, "{line}")
}

/// A one-line summary of an execution.
pub fn summary(removals: &[Removal]) -> String {
    let removed: Vec<&Removal> = removals
        .iter()
        .filter(|r| r.outcome == Outcome::Removed)
        .collect();
    let freed: u64 = removed.iter().filter_map(|r| r.bytes).sum();
    let skipped = removals
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Skipped { .. }))
        .count();
    let failed = removals.len() - removed.len() - skipped;
    let mut text = format!(
        "Removed {} worktree{}, freed {}",
        removed.len(),
        plural(removed.len()),
        format_bytes(freed)
    );
    if skipped > 0 {
        text.push_str(&format!(", {skipped} kept on re-check"));
    }
    if failed > 0 {
        text.push_str(&format!(", {failed} failed"));
    }
    text
}

#[cfg(test)]
mod tests;

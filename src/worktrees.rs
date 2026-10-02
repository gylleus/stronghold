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
//! `git status`, and HEAD an ancestor of the base (or the PR merged or closed with every
//! commit on a remote).

use std::collections::{BTreeMap, HashMap, HashSet};
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
/// shows the broom whoever started it.
pub const SWEEP_FILE: &str = "worktree-sweep.json";
/// A worktree created this recently is kept: an agent may be about to start in it.
pub const MIN_AGE_MS: i64 = 2 * 60 * 60 * 1000;
/// `orca worktree rm` deletes the whole directory, which takes a while for a big one.
pub const REMOVE_TIMEOUT: Duration = Duration::from_secs(180);
/// Paths `git status` may show without counting as uncommitted work: the beads export.
const IGNORED_DIRTY: &[&str] = &[".beads/"];
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
    /// Local branch, `None` on a detached HEAD.
    pub branch: Option<String>,
    /// `git status --porcelain` lines that count as uncommitted work.
    pub dirty: Vec<String>,
    /// A base branch HEAD is an ancestor of, e.g. `main` or `origin/main`.
    pub merged_into: Option<String>,
    /// A remote-tracking ref HEAD is an ancestor of.
    pub on_remote: Option<String>,
    /// Commits on HEAD not in the first base branch.
    pub ahead: Option<u64>,
}

/// Everything [`decide`] looks at for one worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub is_main: bool,
    pub pinned: bool,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub remove: bool,
    /// For a kept worktree, every reason to keep it; for a removed one, why it is safe.
    pub reasons: Vec<String>,
}

impl Verdict {
    fn keep(reasons: Vec<String>) -> Self {
        Self {
            remove: false,
            reasons,
        }
    }
}

/// Decides one worktree. It is removed only when every check passed and was answered: no
/// terminal, process or dispatch in it, no uncommitted change, no open PR, and HEAD merged
/// into a base branch (or its PR merged or closed with every commit on a remote).
pub fn decide(facts: &Facts) -> Verdict {
    let mut keep = Vec::new();
    if facts.is_main {
        keep.push("main checkout".to_owned());
    }
    if facts.pinned {
        keep.push("pinned in Orca".to_owned());
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
    let safe_because = if let Some(base) = &git.merged_into {
        Some(match git.ahead {
            Some(0) | None => format!("everything is in {base}"),
            Some(_) => format!("merged into {base}"),
        })
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
        Some(reason) if keep.is_empty() => Verdict {
            remove: true,
            reasons: vec![reason],
        },
        _ => Verdict::keep(keep),
    }
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

/// The processes working inside `dir`, as `command (pid)`, excluding `own_pid`.
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

/// One PR from `gh pr list --json number,state,headRefName`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrRow {
    pub number: u64,
    pub state: String,
    #[serde(default)]
    pub head_ref_name: String,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    /// Orca's `<repoId>::<path>`.
    pub id: String,
    pub name: String,
    pub repo: String,
    pub path: String,
    pub branch: Option<String>,
    pub bytes: Option<u64>,
    pub verdict: Verdict,
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

    /// A plain-text rendering, for the MCP tool and logs.
    pub fn text(&self) -> String {
        let removable: Vec<&PlanEntry> = self.removable().collect();
        let mut out = vec![format!(
            "{} linked worktrees: {} to remove (frees {}), {} kept.",
            self.entries.len(),
            removable.len(),
            format_bytes(self.freed_bytes()),
            self.entries.len() - removable.len()
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
            }
        }
        let kept: Vec<&PlanEntry> = self.kept().collect();
        if !kept.is_empty() {
            out.push("Keep:".into());
            for e in kept {
                out.push(format!(
                    "- {}/{}: {}",
                    e.repo,
                    e.name,
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

/// The fleet-wide inputs one evaluation shares across worktrees.
struct Context {
    terminals: Vec<Terminal>,
    workers: Vec<Worker>,
    processes: Vec<ProcessCwd>,
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
            prs: HashMap::new(),
            now_ms,
        })
    }

    fn facts(&self, w: &Worktree) -> Facts {
        let path = Path::new(&w.path);
        let exists = path.is_dir();
        let git = if exists {
            git_facts(path, w.base_ref.as_deref())
        } else {
            Err("missing".into())
        };
        let branch = git.as_ref().ok().and_then(|g| g.branch.clone());
        let no_prs = Err("not looked up".to_owned());
        let prs = w.repo().and_then(|r| self.prs.get(r)).unwrap_or(&no_prs);
        Facts {
            is_main: w.is_main_worktree,
            pinned: w.is_pinned,
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
            "number,state,headRefName",
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
    let (ok, status) = git(&["status", "--porcelain", "--untracked-files=all"])?;
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
    let (_, remotes) = git(&[
        "for-each-ref",
        "--contains",
        &head,
        "--format=%(refname:short)",
        "refs/remotes",
    ])?;
    let on_remote = remotes
        .lines()
        .map(str::trim)
        .find(|r| !r.is_empty() && !r.ends_with("/HEAD"))
        .map(str::to_owned);
    Ok(GitFacts {
        branch: Some(branch).filter(|b| !b.is_empty()),
        dirty: dirty_lines(&status),
        merged_into,
        on_remote,
        ahead,
    })
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

/// Evaluates every linked worktree Orca lists. `sizes` are known disk usages by path; the
/// removable ones without a size are measured.
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
    let unmeasured: Vec<PathBuf> = entries
        .iter()
        .filter(|e| e.verdict.remove)
        .map(|e| PathBuf::from(&e.path))
        .filter(|p| !sizes.contains_key(p))
        .collect();
    let measured = disk_usages(&unmeasured);
    for e in &mut entries {
        let path = PathBuf::from(&e.path);
        e.bytes = sizes.get(&path).or(measured.get(&path)).copied();
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

fn plan_entry(w: &Worktree, facts: &Facts) -> PlanEntry {
    PlanEntry {
        id: w.id.clone(),
        name: w.name().to_owned(),
        repo: repo_name(w).to_owned(),
        path: w.path.clone(),
        branch: w.branch_name().map(str::to_owned),
        bytes: None,
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
    mut progress: impl FnMut(&Removal),
) -> Result<Vec<Removal>, String> {
    let list = client
        .worktree_list()
        .map_err(|e| format!("worktree list: {e}"))?;
    let repos = client.repo_list().map_err(|e| format!("repo list: {e}"))?;
    let mut ctx = Context::fetch(client, now_ms())?;
    let remover = client.clone().timeout(REMOVE_TIMEOUT);
    let marker = log.with_file_name(SWEEP_FILE);
    let total = plan.removable().count();
    let mut done = Vec::new();
    for entry in plan.removable() {
        write_marker(
            &marker,
            &SweepMarker {
                started_at_ms: plan.evaluated_at_ms,
                updated_at_ms: now_ms(),
                total,
                done: done.len(),
                current: Some(entry.name.clone()),
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
                let verdict = decide(&ctx.facts(w));
                if !verdict.remove {
                    Outcome::Skipped {
                        reasons: verdict.reasons,
                    }
                } else {
                    match remover.call_value(&worktree_rm_args(&w.id)) {
                        Ok(_) => Outcome::Removed,
                        Err(e) => Outcome::Failed {
                            error: rm_error(&e),
                        },
                    }
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
    let _ = fs::remove_file(&marker);
    Ok(done)
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

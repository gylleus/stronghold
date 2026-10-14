//! The `orca` commands that start new sessions: repo and worktree listings, `worktree create`,
//! `terminal create`, `terminal wait`, `orchestration task-create` and `worker-start`.
//!
//! Result shapes follow Orca's runtime (`worktree.create`, `terminal.create`, `terminal.wait`,
//! `orchestration.taskCreate`, `orchestration.workerStart`). Every field is optional so an
//! older or newer runtime still parses.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::client::{OrcaClient, OrcaError, parse_result};
use super::types::Task;

/// Worktrees `worktree list` returns at most (Orca's default is 200).
pub const WORKTREE_LIST_LIMIT: u64 = 500;

fn flag(name: &str, value: impl std::fmt::Display) -> String {
    format!("--{name}={value}")
}

fn base(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Types

/// One row of `orca repo list`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Repo {
    pub id: String,
    pub path: String,
    pub display_name: Option<String>,
    /// `git` or `folder`.
    pub kind: Option<String>,
    /// Default base ref of new worktrees, e.g. `origin/main`.
    pub worktree_base_ref: Option<String>,
}

impl Repo {
    /// Display name, falling back to the path's last component.
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|n| !n.is_empty())
            .or_else(|| self.path.rsplit('/').find(|c| !c.is_empty()))
            .unwrap_or(&self.id)
    }

    /// The `--repo` selector.
    pub fn selector(&self) -> String {
        format!("id:{}", self.id)
    }
}

/// `result` of `orca repo list`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct RepoList {
    pub repos: Vec<Repo>,
}

/// One row of `orca worktree list` (and `result.worktree` of `worktree create`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Worktree {
    /// `<repoId>::<path>`.
    pub id: String,
    pub repo_id: Option<String>,
    pub path: String,
    /// Full ref, e.g. `refs/heads/main`.
    pub branch: Option<String>,
    pub display_name: Option<String>,
    pub is_main_worktree: bool,
    pub is_archived: bool,
    pub workspace_status: Option<String>,
    pub last_activity_at: Option<i64>,
    /// E.g. `github:owner/repo`; Orca's id for a repo without a remote is `repo:<id>`.
    pub project_id: Option<String>,
    pub is_pinned: bool,
    /// The linked GitHub PR's number, as Orca sends it.
    #[serde(rename = "linkedPR")]
    pub linked_pr: Option<Value>,
    /// Where the branch pushes to.
    pub push_target: Option<PushTarget>,
    /// The ref the worktree was created from, e.g. `refs/remotes/origin/main`.
    pub base_ref: Option<String>,
    pub created_at: Option<i64>,
}

/// `pushTarget` of a worktree.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PushTarget {
    pub remote_name: Option<String>,
    pub branch_name: Option<String>,
}

impl Worktree {
    /// Display name, falling back to the branch without `refs/heads/`, then the path.
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|n| !n.is_empty())
            .or_else(|| self.branch_name())
            .or_else(|| self.path.rsplit('/').find(|c| !c.is_empty()))
            .unwrap_or(&self.id)
    }

    pub fn branch_name(&self) -> Option<&str> {
        let branch = self.branch.as_deref()?;
        Some(branch.strip_prefix("refs/heads/").unwrap_or(branch))
    }

    /// The repo id: `repo_id`, else the part of `id` before `::`.
    pub fn repo(&self) -> Option<&str> {
        self.repo_id
            .as_deref()
            .or_else(|| self.id.split_once("::").map(|(repo, _)| repo))
    }

    /// The `--worktree` selector.
    pub fn selector(&self) -> String {
        format!("id:{}", self.id)
    }

    /// The linked PR's number, when Orca sends one.
    pub fn linked_pr_number(&self) -> Option<u64> {
        match self.linked_pr.as_ref()? {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.trim().trim_start_matches('#').parse().ok(),
            Value::Object(o) => o.get("number").and_then(Value::as_u64),
            _ => None,
        }
    }
}

/// `result` of `orca worktree list`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorktreeList {
    pub worktrees: Vec<Worktree>,
    pub total_count: Option<u64>,
    pub truncated: bool,
}

/// `result.startupTerminal` of `worktree create --agent`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StartupTerminal {
    pub handle: Option<String>,
    pub tab_id: Option<String>,
    pub pane_key: Option<String>,
}

/// `result` of `orca worktree create`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorktreeCreated {
    pub worktree: Worktree,
    /// The agent's terminal with `--agent` (newer runtimes).
    pub agent_terminal_handle: Option<String>,
    /// The first terminal Orca started (older runtimes only report this one).
    pub startup_terminal: Option<StartupTerminal>,
    pub warning: Option<String>,
}

impl WorktreeCreated {
    /// The agent terminal's handle: `agentTerminalHandle`, else `startupTerminal.handle`.
    pub fn agent_handle(&self) -> Option<&str> {
        self.agent_terminal_handle
            .as_deref()
            .or_else(|| self.startup_terminal.as_ref()?.handle.as_deref())
    }
}

/// `result.terminal` of `orca terminal create`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TerminalCreated {
    pub handle: String,
    pub worktree_id: Option<String>,
    pub tab_id: Option<String>,
    pub title: Option<String>,
    /// `background` or `focused`.
    pub surface: Option<String>,
    pub warning: Option<String>,
}

/// `result.wait` of `orca terminal wait`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TerminalWait {
    pub handle: Option<String>,
    pub condition: Option<String>,
    /// `false` when a startup modal blocks the agent (see `blocked_reason`); the CLI exits 1.
    pub satisfied: bool,
    pub status: Option<String>,
    pub exit_code: Option<i64>,
    /// e.g. `codex-trust-workspace`.
    pub blocked_reason: Option<String>,
}

/// One entry of a worker-start receipt's `effects`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct WorkerEffect {
    /// `worktree`, `terminal`, `setup` or `dispatch_input`.
    pub kind: Option<String>,
    pub role: Option<String>,
    pub action: Option<String>,
    pub id: Option<String>,
    pub state: Option<String>,
}

/// `result` of `orca orchestration worker-start` (a receipt, also on failure).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkerStarted {
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub dispatch_id: Option<String>,
    /// `ready` on success; `failed` or `outcome_unknown` otherwise.
    pub state: Option<String>,
    pub stage: Option<String>,
    pub failed_stage: Option<String>,
    pub last_error: Option<String>,
    pub warning: Option<String>,
    pub effects: Vec<WorkerEffect>,
    pub residual_resources: Vec<Value>,
    pub next_commands: Vec<String>,
}

impl WorkerStarted {
    pub fn is_ready(&self) -> bool {
        self.state.as_deref() == Some("ready")
    }

    /// The agent terminal: the `terminal` effect with role `agent`, else where the dispatch
    /// input went.
    pub fn agent_handle(&self) -> Option<&str> {
        let find = |kind: &str| {
            self.effects
                .iter()
                .find(|e| {
                    e.kind.as_deref() == Some(kind)
                        && e.role.as_deref().is_none_or(|r| r == "agent")
                })
                .and_then(|e| e.id.as_deref())
        };
        find("terminal").or_else(|| find("dispatch_input"))
    }

    /// The worker's worktree (created or reused).
    pub fn worktree_id(&self) -> Option<&str> {
        self.effects
            .iter()
            .find(|e| e.kind.as_deref() == Some("worktree"))
            .and_then(|e| e.id.as_deref())
    }

    /// Failed while typing the brief into an agent that was up: Orca judged the prompt stalled
    /// or blocked, which it often does while the agent takes the brief anyway.
    pub fn input_unconfirmed(&self) -> bool {
        let stage = self.failed_stage.as_deref().or(self.stage.as_deref());
        !self.is_ready() && stage == Some("dispatch_input")
    }

    /// Why the start failed, in the words of Orca's CLI.
    pub fn failure(&self) -> String {
        let state = self.state.as_deref().unwrap_or("failed");
        let stage = self
            .failed_stage
            .as_deref()
            .or(self.stage.as_deref())
            .unwrap_or("start");
        let mut text = format!("worker {state} at {stage}");
        if let Some(error) = &self.last_error {
            text.push_str(": ");
            text.push_str(error);
        }
        if !self.residual_resources.is_empty() {
            text.push_str(&format!(
                " ({} leftover resource(s); see `orca orchestration worker-show`)",
                self.residual_resources.len()
            ));
        }
        text
    }
}

// ---------------------------------------------------------------------------
// Requests

/// `orca worktree create` for a new independent worktree off the repo's base ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeCreate {
    pub repo_id: String,
    pub name: String,
    /// `None`: the repo's default base ref.
    pub base_branch: Option<String>,
    /// Orca's TUI agent id (`claude`, `codex`) to launch in the first terminal.
    pub agent: Option<String>,
    /// Initial prompt for `agent` (Orca passes it as the agent's argv prompt).
    pub prompt: Option<String>,
}

/// `orca orchestration worker-start` placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerPlacement {
    /// `--worktree new-top-level --name --repo`.
    NewWorktree { repo_id: String, name: String },
    /// `--worktree id:<worktree>`, a fresh agent terminal there.
    Worktree { worktree_id: String },
    /// `--worktree id:<worktree> --terminal <handle>`: the agent already running there.
    Terminal { worktree_id: String, handle: String },
}

/// `orca orchestration worker-start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerStart {
    pub task_id: String,
    pub run_id: String,
    /// The run's coordinator terminal; worker-start must come from it.
    pub coordinator: String,
    pub placement: WorkerPlacement,
    /// Agent for a fresh terminal (ignored for [`WorkerPlacement::Terminal`]).
    pub agent: String,
    pub model: Option<String>,
    /// `--effort`; Orca only takes it together with `--model`.
    pub effort: Option<String>,
    pub timeout_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Argument builders

pub fn repo_list_args() -> Vec<String> {
    base(&["repo", "list"])
}

pub fn worktree_list_args() -> Vec<String> {
    let mut args = base(&["worktree", "list"]);
    args.push(flag("limit", WORKTREE_LIST_LIMIT));
    args
}

pub fn worktree_create_args(req: &WorktreeCreate) -> Vec<String> {
    let mut args = base(&["worktree", "create"]);
    args.push(flag("repo", format!("id:{}", req.repo_id)));
    args.push(flag("name", &req.name));
    args.push("--no-parent".into());
    if let Some(base_branch) = &req.base_branch {
        args.push(flag("base-branch", base_branch));
    }
    if let Some(agent) = &req.agent {
        args.push(flag("agent", agent));
        if let Some(prompt) = req.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            args.push(flag("prompt", prompt));
        }
    }
    args
}

pub fn terminal_create_args(worktree_id: &str, command: &str, title: Option<&str>) -> Vec<String> {
    let mut args = base(&["terminal", "create"]);
    args.push(flag("worktree", format!("id:{worktree_id}")));
    args.push(flag("command", command));
    if let Some(title) = title {
        args.push(flag("title", title));
    }
    args
}

pub fn terminal_wait_idle_args(handle: &str, timeout_ms: u64) -> Vec<String> {
    let mut args = base(&["terminal", "wait"]);
    args.push(flag("terminal", handle));
    args.push(flag("for", "tui-idle"));
    args.push(flag("timeout-ms", timeout_ms));
    args
}

pub fn task_create_args(
    run_id: &str,
    coordinator: &str,
    spec: &str,
    title: Option<&str>,
    display_name: Option<&str>,
) -> Vec<String> {
    let mut args = base(&["orchestration", "task-create"]);
    args.push(flag("run", run_id));
    args.push(flag("from", coordinator));
    args.push(flag("spec", spec));
    if let Some(title) = title {
        args.push(flag("task-title", title));
    }
    if let Some(name) = display_name {
        args.push(flag("display-name", name));
    }
    args
}

/// `orca worktree set --display-name`: the name Orca (and Stronghold) shows for a worktree.
pub fn worktree_name_args(worktree_id: &str, name: &str) -> Vec<String> {
    let mut args = base(&["worktree", "set"]);
    args.push(flag("worktree", format!("id:{worktree_id}")));
    args.push(flag("display-name", name));
    args
}

pub fn worker_start_args(req: &WorkerStart) -> Vec<String> {
    let mut args = base(&["orchestration", "worker-start"]);
    args.push(flag("task", &req.task_id));
    args.push(flag("run", &req.run_id));
    args.push(flag("from", &req.coordinator));
    match &req.placement {
        WorkerPlacement::NewWorktree { repo_id, name } => {
            args.push(flag("worktree", "new-top-level"));
            args.push(flag("name", name));
            args.push(flag("repo", format!("id:{repo_id}")));
        }
        WorkerPlacement::Worktree { worktree_id } => {
            args.push(flag("worktree", format!("id:{worktree_id}")));
        }
        WorkerPlacement::Terminal {
            worktree_id,
            handle,
        } => {
            args.push(flag("worktree", format!("id:{worktree_id}")));
            args.push(flag("terminal", handle));
        }
    }
    if !matches!(req.placement, WorkerPlacement::Terminal { .. }) {
        args.push(flag("agent", &req.agent));
        if let Some(model) = &req.model {
            args.push(flag("model", model));
            if let Some(effort) = &req.effort {
                args.push(flag("effort", effort));
            }
        }
    }
    if let Some(timeout) = req.timeout_ms {
        args.push(flag("timeout-ms", timeout));
    }
    args
}

// ---------------------------------------------------------------------------
// Parsers

#[derive(Deserialize)]
struct TerminalCreateResult {
    terminal: TerminalCreated,
}

#[derive(Deserialize)]
struct TerminalWaitResult {
    wait: TerminalWait,
}

#[derive(Deserialize)]
struct TaskCreateResult {
    task: Task,
}

pub fn parse_repo_list(stdout: &[u8]) -> Result<RepoList, OrcaError> {
    parse_result(stdout)
}

pub fn parse_worktree_list(stdout: &[u8]) -> Result<WorktreeList, OrcaError> {
    parse_result(stdout)
}

pub fn parse_worktree_create(stdout: &[u8]) -> Result<WorktreeCreated, OrcaError> {
    parse_result(stdout)
}

pub fn parse_terminal_create(stdout: &[u8]) -> Result<TerminalCreated, OrcaError> {
    parse_result::<TerminalCreateResult>(stdout).map(|r| r.terminal)
}

pub fn parse_terminal_wait(stdout: &[u8]) -> Result<TerminalWait, OrcaError> {
    parse_result::<TerminalWaitResult>(stdout).map(|r| r.wait)
}

pub fn parse_task_create(stdout: &[u8]) -> Result<Task, OrcaError> {
    parse_result::<TaskCreateResult>(stdout).map(|r| r.task)
}

pub fn parse_worker_start(stdout: &[u8]) -> Result<WorkerStarted, OrcaError> {
    parse_result(stdout)
}

/// Decodes a `result` value already taken out of its envelope.
pub fn from_result<T: serde::de::DeserializeOwned>(result: Value) -> Result<T, OrcaError> {
    serde_json::from_value(result.clone()).map_err(|source| OrcaError::Json {
        source,
        snippet: result.to_string().chars().take(200).collect(),
    })
}

/// Decodes `result.<key>` of a result value.
pub fn from_result_field<T: serde::de::DeserializeOwned>(
    mut result: Value,
    key: &str,
) -> Result<T, OrcaError> {
    from_result(result.get_mut(key).map(Value::take).unwrap_or(Value::Null))
}

impl OrcaClient {
    pub fn repo_list(&self) -> Result<Vec<Repo>, OrcaError> {
        self.call::<RepoList, _>(&repo_list_args()).map(|r| r.repos)
    }

    pub fn worktree_list(&self) -> Result<WorktreeList, OrcaError> {
        self.call(&worktree_list_args())
    }
}

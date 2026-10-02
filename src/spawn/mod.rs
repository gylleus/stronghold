//! Starting new agent sessions from Myrstack: in an existing worktree, in a new worktree, or as
//! a worker of an existing run.
//!
//! A [`SpawnRequest`] (what Karl or the Queen asked for, resolved to Orca ids) becomes a
//! [`SpawnPlan`]: the `orca` calls to make, in order ([`Step`]). Building the plan is pure, so
//! the dialog can show the exact commands before launch; [`exec`] runs them on a background
//! thread and makes sure the prompt was really submitted ([`verify`]).

pub mod catalog;
pub mod exec;
pub mod interrupted;
pub mod pending;
pub mod resolve;
pub mod verify;

#[cfg(test)]
mod tests;

use crate::orca::spawn::{
    self as cmd, WorkerPlacement, WorkerStart, WorktreeCreate, task_create_args,
    terminal_create_args, terminal_wait_idle_args, worker_start_args, worktree_create_args,
    worktree_name_args,
};

pub use catalog::Catalog;
pub use exec::{SpawnEvent, SpawnOutcome, SpawnRunner, StepStatus};
pub use resolve::{LiveAgent, ModeName, NameConflict, SpawnArgs, name_conflict, new_worktree_of};
pub use verify::{Expect, ScreenVerdict, classify_screen, codex_approvals_on};

/// How long `terminal wait --for tui-idle` waits after launch. An agent that took its prompt
/// is busy, not idle, so this is short and running out is not an error.
pub const WAIT_IDLE_MS: u64 = 8_000;
/// Readiness budget passed to `worker-start --timeout-ms`.
pub const WORKER_START_TIMEOUT_MS: u64 = 90_000;
/// Longest task title derived from the prompt.
const TITLE_CHARS: usize = 60;

/// The agent CLI to launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Agent {
    #[default]
    Claude,
    Codex,
}

impl Agent {
    pub const ALL: [Agent; 2] = [Agent::Claude, Agent::Codex];

    /// Orca's TUI agent id, also the binary name.
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            _ => None,
        }
    }
}

/// Where the new session runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnTarget {
    /// A fresh agent terminal in an existing worktree.
    Worktree { worktree_id: String, path: String },
    /// A new independent worktree off the repo's base ref (`--no-parent`).
    NewWorktree {
        repo_id: String,
        name: String,
        base_branch: Option<String>,
    },
    /// A worker of an existing run: a task created in the run, then `worker-start`.
    Worker {
        run_id: String,
        /// The run's coordinator terminal; Orca only takes run mutations from it.
        coordinator: String,
        placement: WorkerPlacement,
        /// Task title; defaults to the prompt's first line.
        title: Option<String>,
        /// Path of the worktree (for `direnv allow` on a new one, display otherwise).
        path: Option<String>,
    },
}

/// A resolved request to start a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnRequest {
    pub target: SpawnTarget,
    pub agent: Agent,
    /// Model alias or id (`--model`); `None` for the agent's default.
    pub model: Option<String>,
    /// Initial prompt; for a worker, the task spec.
    pub prompt: String,
    /// Short name Myrstack shows for the agent: the worktree's Orca display name (new or
    /// existing worktree), or a worker's task display name.
    pub label: Option<String>,
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `orca worktree create`; records the worktree and, with an agent, its terminal.
    CreateWorktree(WorktreeCreate),
    /// `orca terminal create --command "<agent> …"`; `worktree_id` `None` means the worktree
    /// created by an earlier step.
    CreateTerminal {
        worktree_id: Option<String>,
        command: String,
    },
    /// `orca orchestration task-create`, sent as the run's coordinator.
    CreateTask {
        run_id: String,
        coordinator: String,
        spec: String,
        title: String,
        /// `--display-name`: the worker's name in Myrstack.
        display_name: Option<String>,
    },
    /// `orca worktree set --display-name`: names the agent's worktree (shown by Orca and, for
    /// its terminals, by Myrstack). `worktree_id` `None` means the one an earlier step created.
    NameWorktree {
        worktree_id: Option<String>,
        name: String,
    },
    /// `orca orchestration worker-start` for the task an earlier step created.
    StartWorker(WorkerStart),
    /// `direnv allow <worktree>` when the new worktree has an `.envrc`.
    DirenvAllow,
    /// `orca terminal wait --for tui-idle`, short; running out is fine.
    WaitIdle { timeout_ms: u64 },
    /// Read the screen until the agent visibly took its prompt, answering the Bypass
    /// Permissions and folder trust modals and submitting a parked prompt on the way.
    /// Never types the prompt: the launch already handed it over, so typing it could run it
    /// twice.
    Verify { expect: Expect },
    /// Close the plain shell Orca opens in a new worktree next to the agent's terminal, so no
    /// empty terminal is left behind.
    CloseStartupShell,
}

/// What earlier steps produced; later steps read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnContext {
    pub worktree_id: Option<String>,
    pub worktree_path: Option<String>,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub dispatch_id: Option<String>,
    /// The agent's terminal.
    pub handle: Option<String>,
}

/// Placeholders shown in the planned commands for values an earlier step produces.
const NEW_WORKTREE: &str = "<new worktree>";
const NEW_TASK: &str = "<new task>";
const NEW_TERMINAL: &str = "<agent terminal>";

impl Step {
    /// Short name for progress rows.
    pub fn title(&self) -> &'static str {
        match self {
            Step::CreateWorktree(_) => "Create worktree",
            Step::CreateTerminal { .. } => "Launch agent terminal",
            Step::CreateTask { .. } => "Create task",
            Step::NameWorktree { .. } => "Name the worktree",
            Step::StartWorker(_) => "Start worker",
            Step::DirenvAllow => "Trust direnv",
            Step::WaitIdle { .. } => "Wait for the agent",
            Step::Verify { .. } => "Check the prompt started",
            Step::CloseStartupShell => "Close the startup shell",
        }
    }

    /// The `orca` arguments (without `--json`), filled from `ctx`. `None` for steps that are
    /// not a single orca call; `Err` names the missing value when `ctx` lacks it.
    pub fn orca_args(&self, ctx: &SpawnContext) -> Option<Result<Vec<String>, String>> {
        self.args_with(ctx, false)
    }

    fn args_with(
        &self,
        ctx: &SpawnContext,
        placeholders: bool,
    ) -> Option<Result<Vec<String>, String>> {
        let pick = |value: &Option<String>, placeholder: &str, what: &str| match value {
            Some(v) => Ok(v.clone()),
            None if placeholders => Ok(placeholder.to_string()),
            None => Err(format!("no {what} from an earlier step")),
        };
        Some(match self {
            Step::CreateWorktree(req) => Ok(worktree_create_args(req)),
            Step::CreateTerminal {
                worktree_id,
                command,
            } => {
                let worktree = match worktree_id {
                    Some(id) => Ok(id.clone()),
                    None => pick(&ctx.worktree_id, NEW_WORKTREE, "worktree"),
                };
                worktree.map(|w| terminal_create_args(&w, command, None))
            }
            Step::CreateTask {
                run_id,
                coordinator,
                spec,
                title,
                display_name,
            } => Ok(task_create_args(
                run_id,
                coordinator,
                spec,
                Some(title),
                display_name.as_deref(),
            )),
            Step::NameWorktree { worktree_id, name } => {
                let worktree = match worktree_id {
                    Some(id) => Ok(id.clone()),
                    None => pick(&ctx.worktree_id, NEW_WORKTREE, "worktree"),
                };
                worktree.map(|w| worktree_name_args(&w, name))
            }
            Step::StartWorker(req) => pick(&ctx.task_id, NEW_TASK, "task").map(|task_id| {
                worker_start_args(&WorkerStart {
                    task_id,
                    ..req.clone()
                })
            }),
            Step::WaitIdle { timeout_ms } => pick(&ctx.handle, NEW_TERMINAL, "agent terminal")
                .map(|h| terminal_wait_idle_args(&h, *timeout_ms)),
            Step::DirenvAllow | Step::Verify { .. } | Step::CloseStartupShell => return None,
        })
    }

    /// The command as the dialog shows it, with placeholders for values not known yet.
    pub fn display(&self, ctx: &SpawnContext) -> String {
        match self {
            Step::DirenvAllow => format!(
                "direnv allow {}  (if it has an .envrc)",
                ctx.worktree_path
                    .as_deref()
                    .map_or_else(|| NEW_WORKTREE.to_string(), shell_quote)
            ),
            Step::Verify { .. } => {
                let handle = ctx.handle.as_deref().unwrap_or(NEW_TERMINAL);
                format!(
                    "orca terminal read --terminal={handle} --screen until the prompt shows as \
                     submitted; accept Bypass Permissions (2) / folder trust (1), submit a \
                     parked prompt (Enter)"
                )
            }
            Step::CloseStartupShell => format!(
                "orca terminal close --terminal=<startup shell> --tab  (the plain shell Orca \
                 opened in {}, if any)",
                ctx.worktree_path
                    .as_deref()
                    .map_or_else(|| NEW_WORKTREE.to_string(), shell_quote)
            ),
            _ => match self.args_with(ctx, true) {
                Some(Ok(args)) => format!("orca {}", display_args(&args)),
                Some(Err(e)) => e,
                None => String::new(),
            },
        }
    }
}

/// `args` as one shell-like line: values quoted when needed, long ones shortened.
fn display_args(args: &[String]) -> String {
    const MAX: usize = 120;
    args.iter()
        .map(|a| {
            let shown = if a.chars().count() > MAX {
                let cut: String = a.chars().take(MAX).collect();
                format!("{cut}…")
            } else {
                a.clone()
            };
            match shown.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => {
                    format!("{flag}={}", display_quote(value))
                }
                _ => display_quote(&shown),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Like [`shell_quote`], but prefers double quotes when nothing in `value` is special inside
/// them, which reads better around a command that quotes its own prompt.
fn display_quote(value: &str) -> String {
    let quoted = shell_quote(value);
    if quoted == value || value.contains(['"', '$', '`', '\\', '!']) {
        return quoted;
    }
    format!("\"{value}\"")
}

/// The ordered steps for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnPlan {
    pub request: SpawnRequest,
    pub steps: Vec<Step>,
    /// Known before the first step runs (an existing worktree's id and path).
    pub initial: SpawnContext,
}

impl SpawnPlan {
    /// Every step as the dialog shows it.
    pub fn display_lines(&self) -> Vec<String> {
        self.steps
            .iter()
            .map(|s| s.display(&self.initial))
            .collect()
    }
}

/// Why a request can't become a plan.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PlanError(pub String);

/// Builds the plan for `request`, or says what is missing or invalid.
pub fn build_plan(request: &SpawnRequest) -> Result<SpawnPlan, PlanError> {
    let err = |m: &str| Err(PlanError(m.to_string()));
    let prompt = request.prompt.trim().to_string();
    let model = match request.model.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(m) if valid_model(m) => Some(m.to_string()),
        Some(m) => return err(&format!("model {m:?} has characters a model id can't have")),
    };
    let agent = request.agent;
    let launch = || launch_command(agent, model.as_deref(), &prompt);
    let verify = || Step::Verify {
        expect: if prompt.is_empty() {
            Expect::Ready
        } else {
            Expect::Prompt(prompt.clone())
        },
    };
    let wait = Step::WaitIdle {
        timeout_ms: WAIT_IDLE_MS,
    };
    let label = request
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned);
    let mut initial = SpawnContext::default();
    let mut steps = match &request.target {
        SpawnTarget::Worktree { worktree_id, path } => {
            if worktree_id.is_empty() {
                return err("pick a worktree");
            }
            initial.worktree_id = Some(worktree_id.clone());
            initial.worktree_path = Some(path.clone()).filter(|p| !p.is_empty());
            vec![
                Step::CreateTerminal {
                    worktree_id: Some(worktree_id.clone()),
                    command: launch(),
                },
                wait,
                verify(),
            ]
        }
        SpawnTarget::NewWorktree {
            repo_id,
            name,
            base_branch,
        } => {
            if repo_id.is_empty() {
                return err("pick a repo");
            }
            let name = validate_name(name)?;
            let base_branch = base_branch
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(str::to_owned);
            // `worktree create` has no --model; with a model the agent gets its own terminal.
            // So does Codex, to launch with the never-ask flag rather than Orca's default args.
            let orca_agent = model.is_none() && agent != Agent::Codex;
            let create = Step::CreateWorktree(WorktreeCreate {
                repo_id: repo_id.clone(),
                name,
                base_branch,
                agent: orca_agent.then(|| agent.as_str().to_string()),
                prompt: (orca_agent && !prompt.is_empty()).then(|| prompt.clone()),
            });
            if orca_agent {
                vec![
                    create,
                    Step::DirenvAllow,
                    wait,
                    verify(),
                    Step::CloseStartupShell,
                ]
            } else {
                vec![
                    create,
                    Step::DirenvAllow,
                    Step::CreateTerminal {
                        worktree_id: None,
                        command: launch(),
                    },
                    wait,
                    verify(),
                    Step::CloseStartupShell,
                ]
            }
        }
        SpawnTarget::Worker {
            run_id,
            coordinator,
            placement,
            title,
            path,
        } => {
            if run_id.is_empty() {
                return err("pick a run");
            }
            if coordinator.is_empty() {
                return err("the run has no coordinator terminal to create its task from");
            }
            if prompt.is_empty() {
                return err("a worker needs a prompt: it is the task spec");
            }
            let placement = match placement {
                WorkerPlacement::NewWorktree { repo_id, name } => {
                    if repo_id.is_empty() {
                        return err("pick a repo for the worker's worktree");
                    }
                    WorkerPlacement::NewWorktree {
                        repo_id: repo_id.clone(),
                        name: validate_name(name)?,
                    }
                }
                WorkerPlacement::Worktree { worktree_id } if worktree_id.is_empty() => {
                    return err("pick a worktree for the worker");
                }
                WorkerPlacement::Terminal { handle, .. } if handle.is_empty() => {
                    return err("pick the terminal the worker runs in");
                }
                WorkerPlacement::Terminal { worktree_id, .. } if worktree_id.is_empty() => {
                    return err("that terminal has no worktree Orca knows");
                }
                other => other.clone(),
            };
            if model.is_some() && matches!(placement, WorkerPlacement::Terminal { .. }) {
                return err(
                    "a model can only be chosen for a fresh agent, not an existing terminal",
                );
            }
            let new_worktree = matches!(placement, WorkerPlacement::NewWorktree { .. });
            if !new_worktree {
                initial.worktree_path = path.clone();
            }
            if let WorkerPlacement::Worktree { worktree_id }
            | WorkerPlacement::Terminal { worktree_id, .. } = &placement
            {
                initial.worktree_id = Some(worktree_id.clone());
            }
            if let WorkerPlacement::Terminal { handle, .. } = &placement {
                initial.handle = Some(handle.clone());
            }
            let title = title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map_or_else(|| title_from(&prompt), str::to_owned);
            let mut steps = vec![
                Step::CreateTask {
                    run_id: run_id.clone(),
                    coordinator: coordinator.clone(),
                    spec: prompt.clone(),
                    title,
                    display_name: label.clone(),
                },
                Step::StartWorker(WorkerStart {
                    task_id: String::new(),
                    run_id: run_id.clone(),
                    coordinator: coordinator.clone(),
                    placement,
                    agent: agent.as_str().to_string(),
                    model: model.clone(),
                    timeout_ms: Some(WORKER_START_TIMEOUT_MS),
                }),
            ];
            if new_worktree {
                steps.push(Step::DirenvAllow);
            }
            // Orca already waited for the agent and typed its preamble (with the spec).
            steps.push(Step::Verify {
                expect: Expect::AnyInput,
            });
            if new_worktree {
                steps.push(Step::CloseStartupShell);
            }
            steps
        }
    };
    // Name the worktree once it exists: an existing one first, a new one after its creation.
    if let Some(name) = &label {
        match &request.target {
            SpawnTarget::Worktree { worktree_id, .. } => steps.insert(
                0,
                Step::NameWorktree {
                    worktree_id: Some(worktree_id.clone()),
                    name: name.clone(),
                },
            ),
            SpawnTarget::NewWorktree { .. } => steps.insert(
                1,
                Step::NameWorktree {
                    worktree_id: None,
                    name: name.clone(),
                },
            ),
            SpawnTarget::Worker { .. } => {}
        }
    }
    Ok(SpawnPlan {
        request: SpawnRequest {
            label,
            prompt,
            model,
            ..request.clone()
        },
        steps,
        initial,
    })
}

/// Codex's never-ask flag. Orca adds its configured default args only to a bare `codex`, so
/// every Codex command Myrstack builds carries it itself.
pub const CODEX_NEVER_ASK: &str = "--dangerously-bypass-approvals-and-sandbox";

/// The command `terminal create` runs: `claude [--model m] '<prompt>'` (Codex also gets
/// [`CODEX_NEVER_ASK`]), quoted for the login shell. A prompt starting with `-` goes after `--`
/// so it isn't read as a flag.
pub fn launch_command(agent: Agent, model: Option<&str>, prompt: &str) -> String {
    let mut parts = vec![agent.as_str().to_string()];
    if agent == Agent::Codex {
        parts.push(CODEX_NEVER_ASK.into());
    }
    if let Some(model) = model {
        parts.push("--model".into());
        parts.push(shell_quote(model));
    }
    let prompt = prompt.trim();
    if !prompt.is_empty() {
        if prompt.starts_with('-') {
            parts.push("--".into());
        }
        parts.push(shell_quote(prompt));
    }
    parts.join(" ")
}

/// Quotes `value` for a POSIX shell (and fish) the way Orca quotes startup arguments: single
/// quotes, with `'` and `\` spliced in double quotes. Plain words stay bare.
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }
    let plain = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if plain {
        return value.to_string();
    }
    let mut out = String::new();
    let mut literal = String::new();
    let flush = |literal: &mut String, out: &mut String| {
        if !literal.is_empty() {
            out.push('\'');
            out.push_str(literal);
            out.push('\'');
            literal.clear();
        }
    };
    for c in value.chars() {
        match c {
            '\'' => {
                flush(&mut literal, &mut out);
                out.push_str("\"'\"");
            }
            '\\' => {
                flush(&mut literal, &mut out);
                out.push_str("\"\\\\\"");
            }
            c => literal.push(c),
        }
    }
    flush(&mut literal, &mut out);
    out
}

/// A model alias or id: `opus`, `sonnet[1m]`, `gpt-6-astra`, `claude-opus-4-8`.
pub fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 80
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/[]".contains(c))
}

/// A worktree name: trimmed, spaces become `-`; letters, digits, `.`, `_`, `-` and `/`.
pub fn validate_name(name: &str) -> Result<String, PlanError> {
    let name = name.trim().replace(char::is_whitespace, "-");
    if name.is_empty() {
        return Err(PlanError("give the new worktree a name".into()));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
        && !name.starts_with(['-', '.', '/'])
        && !name.ends_with(['/', '.'])
        && !name.contains("..");
    if !ok {
        return Err(PlanError(format!(
            "worktree name {name:?} may only use letters, digits, '-', '_', '.' and '/'"
        )));
    }
    Ok(name)
}

/// A task title from the prompt's first non-empty line, shortened.
pub fn title_from(prompt: &str) -> String {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty());
    let line = line.unwrap_or("Task from Myrstack");
    if line.chars().count() <= TITLE_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(TITLE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The Orca result shapes this module reads, re-exported for the executor.
pub use cmd::{TerminalCreated, TerminalWait, WorkerStarted, WorktreeCreated};

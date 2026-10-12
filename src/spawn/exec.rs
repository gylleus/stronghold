//! Runs a [`SpawnPlan`] on a background thread, one step after another, reporting each step's
//! progress, then checks on screen that the agent took its prompt.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value;

use super::resolve::{LiveAgent, name_conflict, new_worktree_of};
use super::verify::{Expect, ScreenVerdict, classify_screen, codex_approvals_on};
use super::{Agent, SpawnArgs, SpawnContext, SpawnPlan, Step, build_plan, catalog::Catalog};
use crate::actions::{GONE_TERMINAL_CODES, Key, Wake};
use crate::model::NodeId;
use crate::orca::commands::{terminal_close_args, terminal_list_args};
use crate::orca::spawn::{
    TerminalCreated, TerminalWait, WorkerStarted, WorktreeCreated, from_result, from_result_field,
};
use crate::orca::{OrcaClient, OrcaError, Run, Task, Terminal, TerminalInput, TerminalList};

/// Per-call timeouts. Creating a worktree runs `git worktree add` (and maybe fetches).
const WORKTREE_CREATE_TIMEOUT: Duration = Duration::from_secs(120);
const TERMINAL_CREATE_TIMEOUT: Duration = Duration::from_secs(30);
const TASK_CREATE_TIMEOUT: Duration = Duration::from_secs(20);
/// On top of worker-start's own readiness budget.
const WORKER_START_GRACE: Duration = Duration::from_secs(30);
const WAIT_GRACE: Duration = Duration::from_secs(10);
/// How long the screen check keeps looking for the started prompt.
pub const VERIFY_DEADLINE: Duration = Duration::from_secs(45);
/// Pause between screen reads.
pub const VERIFY_POLL: Duration = Duration::from_millis(1500);
/// Most times each fix is tried.
const MAX_MODAL_ANSWERS: u32 = 2;
/// Note on a worker that took its brief although Orca failed its start.
pub const MISREPORTED_LAUNCH: &str = "the worker is running although Orca failed its start and \
     revoked the dispatch: its worker_done/merge_ready arrive as \"Rejected …\" messages, which \
     Myrstack forwards to the coordinator as the worker's report";
const MAX_ENTERS: u32 = 3;
/// The note when a Codex session came up asking for approval.
pub const CODEX_APPROVALS_ON: &str = "Codex started with approvals on (its header lacks \
     \"permissions: YOLO mode\"); it will ask before every action. Relaunch it with \
     --dangerously-bypass-approvals-and-sandbox";

/// Caller identity variables `orca` reads besides [`crate::orca::client::SENDER_IDENTITY_ENV`];
/// Myrstack acts as itself (or explicitly `--from` a coordinator), never as the terminal that
/// launched it.
pub const EXTRA_IDENTITY_ENV: &[&str] = &[
    "ORCA_AGENT_LAUNCH_TOKEN",
    "ORCA_ORCHESTRATION_COMPATIBILITY_HOST_KIND",
    "ORCA_ORCHESTRATION_COMPATIBILITY_HOST_ID",
    "ORCA_ORCHESTRATION_COMPATIBILITY_HOST_INCARNATION",
    "ORCA_ORCHESTRATION_COMPATIBILITY_ATTACHMENT",
];

/// Where a step stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Done(Option<String>),
    /// Finished with something worth knowing; the plan carries on.
    Warned(String),
    /// Not needed (e.g. no `.envrc`).
    Skipped(String),
    Failed(String),
}

impl StepStatus {
    pub fn is_finished(&self) -> bool {
        !matches!(self, StepStatus::Pending | StepStatus::Running)
    }
}

/// A finished spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnOutcome {
    pub context: SpawnContext,
    /// The screen showed the prompt taken.
    pub verified: bool,
    /// Warnings and what was done on the way (modal answered, prompt resubmitted, …).
    pub notes: Vec<String>,
}

impl SpawnOutcome {
    /// The farm node the new session appears as.
    pub fn node_id(&self) -> Option<NodeId> {
        let ctx = &self.context;
        match (&ctx.run_id, &ctx.dispatch_id, &ctx.handle) {
            (Some(run), Some(dispatch), _) => Some(NodeId::dispatch(run, dispatch)),
            (_, _, Some(handle)) => Some(NodeId::terminal(handle)),
            _ => None,
        }
    }
}

/// A spawn that stopped at a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnFailure {
    /// The failed step; `None` when the request could not become a plan.
    pub step: Option<usize>,
    pub message: String,
    /// What was created before the failure (a worktree, a terminal …).
    pub context: Box<SpawnContext>,
}

impl std::fmt::Display for SpawnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        let ctx = &self.context;
        let mut left = Vec::new();
        if let Some(path) = &ctx.worktree_path {
            left.push(format!("worktree {path}"));
        }
        if let Some(handle) = &ctx.handle {
            left.push(format!("terminal {handle}"));
        }
        if let Some(task) = &ctx.task_id {
            left.push(format!("task {task}"));
        }
        if !left.is_empty() {
            write!(f, " (already created: {})", left.join(", "))?;
        }
        Ok(())
    }
}

/// The side effects a spawn needs; faked in tests.
pub trait SpawnBackend: Send + 'static {
    /// `orca <args> --json` → `result`.
    fn orca(&mut self, args: &[String], timeout: Duration) -> Result<Value, OrcaError>;
    /// The terminal's rendered screen rows.
    fn read_screen(&mut self, handle: &str) -> Result<Vec<String>, OrcaError>;
    /// `orca terminal send`; `Err` when Orca refused the input.
    fn send(&mut self, handle: &str, input: &TerminalInput) -> Result<(), String>;
    /// `direnv allow <path>` when `path` has an `.envrc`: `Ok(None)` when there was nothing
    /// to do.
    fn direnv_allow(&mut self, path: &str) -> Result<Option<String>, String>;
    /// Orca's repos and worktrees.
    fn catalog(&mut self) -> Result<Catalog, String>;
    /// The local branches of the repo checked out at `repo_path`.
    fn branches(&mut self, repo_path: &str) -> Result<Vec<String>, String>;
    fn sleep(&mut self, duration: Duration);
    /// Monotonic time.
    fn elapsed(&self) -> Duration;
}

/// The real backend: the `orca` CLI, `direnv`, wall-clock sleeps.
pub struct OrcaBackend {
    client: OrcaClient,
    started: Instant,
}

impl Default for OrcaBackend {
    fn default() -> Self {
        Self::new(OrcaClient::new())
    }
}

impl OrcaBackend {
    pub fn new(client: OrcaClient) -> Self {
        Self {
            client: client
                .without_sender_identity()
                .without_env(EXTRA_IDENTITY_ENV.iter().copied()),
            started: Instant::now(),
        }
    }
}

impl SpawnBackend for OrcaBackend {
    fn orca(&mut self, args: &[String], timeout: Duration) -> Result<Value, OrcaError> {
        log::info!("spawn: orca {}", args.join(" "));
        self.client.clone().timeout(timeout).call_value(args)
    }

    fn read_screen(&mut self, handle: &str) -> Result<Vec<String>, OrcaError> {
        self.client.terminal_read_screen(handle).map(|r| r.tail)
    }

    fn send(&mut self, handle: &str, input: &TerminalInput) -> Result<(), String> {
        log::info!("spawn: send {input:?} to {handle}");
        let send = self
            .client
            .clone()
            .timeout(crate::actions::ACTION_TIMEOUT)
            .terminal_send(handle, input)
            .map_err(|e| e.to_string())?;
        if send.accepted == Some(false) {
            return Err(send
                .refused_reason
                .unwrap_or_else(|| "Orca did not accept the input".into()));
        }
        Ok(())
    }

    fn direnv_allow(&mut self, path: &str) -> Result<Option<String>, String> {
        if !Path::new(path).join(".envrc").is_file() {
            return Ok(None);
        }
        let Some(direnv) = find_direnv() else {
            return Err("the worktree has an .envrc but direnv is not installed".into());
        };
        let out = Command::new(&direnv)
            .arg("allow")
            .arg(path)
            .output()
            .map_err(|e| format!("running {}: {e}", direnv.display()))?;
        if out.status.success() {
            Ok(Some(format!("direnv allow {path}")))
        } else {
            Err(format!(
                "direnv allow exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn catalog(&mut self) -> Result<Catalog, String> {
        Catalog::fetch(&self.client)
    }

    fn branches(&mut self, repo_path: &str) -> Result<Vec<String>, String> {
        let out = Command::new("git")
            .args(["-C", repo_path, "for-each-ref", "--format=%(refname:short)"])
            .arg("refs/heads")
            .output()
            .map_err(|e| format!("running git: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect())
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

/// `direnv` on `PATH` or in Homebrew's bin dirs (apps started from Finder lack the shell PATH).
fn find_direnv() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join("direnv"));
    on_path
        .chain(
            ["/opt/homebrew/bin/direnv", "/usr/local/bin/direnv"]
                .into_iter()
                .map(PathBuf::from),
        )
        .find(|p| p.is_file())
}

/// Progress reported while a plan runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Step { index: usize, status: StepStatus },
}

/// Runs `plan`'s steps in order; `progress` hears every step start and finish.
pub fn run_plan(
    plan: &SpawnPlan,
    backend: &mut dyn SpawnBackend,
    progress: &mut dyn FnMut(Progress),
) -> Result<SpawnOutcome, SpawnFailure> {
    run_plan_observed(plan, backend, progress, &mut |_| Ok(()))
}

/// Publishes created handles before readiness checks, so long-lived sessions survive an app restart.
pub fn run_plan_observed(
    plan: &SpawnPlan,
    backend: &mut dyn SpawnBackend,
    progress: &mut dyn FnMut(Progress),
    context: &mut dyn FnMut(&SpawnContext) -> Result<(), String>,
) -> Result<SpawnOutcome, SpawnFailure> {
    let mut run = PlanRun {
        plan,
        ctx: plan.initial.clone(),
        notes: Vec::new(),
        verified: false,
        misreported: None,
    };
    for (index, step) in plan.steps.iter().enumerate() {
        progress(Progress::Step {
            index,
            status: StepStatus::Running,
        });
        let status = match run.step(step, backend) {
            Ok(status) => status,
            Err(message) => {
                progress(Progress::Step {
                    index,
                    status: StepStatus::Failed(message.clone()),
                });
                return Err(SpawnFailure {
                    step: Some(index),
                    message: format!("{}: {message}", step.title()),
                    context: Box::new(run.ctx),
                });
            }
        };
        if let Err(message) = context(&run.ctx) {
            return Err(SpawnFailure {
                step: Some(index),
                message,
                context: Box::new(run.ctx),
            });
        }
        if let StepStatus::Warned(note) = &status {
            run.notes.push(note.clone());
        }
        progress(Progress::Step { index, status });
    }
    Ok(SpawnOutcome {
        context: run.ctx,
        verified: run.verified,
        notes: run.notes,
    })
}

struct PlanRun<'a> {
    plan: &'a SpawnPlan,
    ctx: SpawnContext,
    notes: Vec<String>,
    verified: bool,
    /// Orca's failure report for a worker start whose brief may still have landed; the screen
    /// check must then see the agent take it, or the spawn fails with this.
    misreported: Option<String>,
}

fn orca_error(e: OrcaError) -> String {
    match e {
        OrcaError::Api(api) => format!("{} ({})", api.message, api.code),
        other => other.to_string(),
    }
}

fn is_gone(e: &OrcaError) -> bool {
    e.code().is_some_and(|c| GONE_TERMINAL_CODES.contains(&c))
}

/// The path part of a worktree id (`<repoId>::<path>`).
fn path_of_worktree_id(id: &str) -> Option<String> {
    id.split_once("::").map(|(_, path)| path.to_string())
}

impl PlanRun<'_> {
    fn call(
        &self,
        step: &Step,
        backend: &mut dyn SpawnBackend,
        timeout: Duration,
    ) -> Result<Value, String> {
        let args = step
            .orca_args(&self.ctx)
            .expect("an orca step")
            .map_err(|e| e.to_string())?;
        backend.orca(&args, timeout).map_err(orca_error)
    }

    fn step(&mut self, step: &Step, backend: &mut dyn SpawnBackend) -> Result<StepStatus, String> {
        match step {
            Step::CreateWorktree(req) => {
                let result = self.call(step, backend, WORKTREE_CREATE_TIMEOUT)?;
                let created: WorktreeCreated = from_result(result).map_err(orca_error)?;
                self.ctx.worktree_id = Some(created.worktree.id.clone());
                self.ctx.worktree_path = Some(created.worktree.path.clone())
                    .filter(|p| !p.is_empty())
                    .or_else(|| path_of_worktree_id(&created.worktree.id));
                let mut note = format!(
                    "{} at {}",
                    created.worktree.name(),
                    self.ctx.worktree_path.as_deref().unwrap_or("?")
                );
                if req.agent.is_some() {
                    let handle = created.agent_handle().ok_or_else(|| {
                        format!(
                            "Orca created {} but reported no agent terminal{}",
                            created.worktree.name(),
                            created
                                .warning
                                .as_deref()
                                .map(|w| format!(": {w}"))
                                .unwrap_or_default()
                        )
                    })?;
                    self.ctx.handle = Some(handle.to_string());
                    note.push_str(&format!(", agent in {handle}"));
                }
                Ok(match created.warning {
                    Some(w) => StepStatus::Warned(format!("{note}; Orca: {w}")),
                    None => StepStatus::Done(Some(note)),
                })
            }
            Step::CreateTerminal { .. } => {
                let result = self.call(step, backend, TERMINAL_CREATE_TIMEOUT)?;
                let terminal: TerminalCreated =
                    from_result_field(result, "terminal").map_err(orca_error)?;
                if terminal.handle.is_empty() {
                    return Err("Orca returned no terminal handle".into());
                }
                self.ctx.handle = Some(terminal.handle.clone());
                Ok(match terminal.warning {
                    Some(w) => StepStatus::Warned(format!("{}; Orca: {w}", terminal.handle)),
                    None => StepStatus::Done(Some(terminal.handle)),
                })
            }
            Step::CreateTask { run_id, .. } => {
                let result = self.call(step, backend, TASK_CREATE_TIMEOUT)?;
                let task: Task = from_result_field(result, "task").map_err(orca_error)?;
                self.ctx.run_id = Some(run_id.clone());
                self.ctx.task_id = Some(task.id.clone());
                Ok(StepStatus::Done(Some(task.id)))
            }
            // A name is a nicety: failing to set it only warns.
            Step::NameWorktree { name, .. } => {
                Ok(match self.call(step, backend, TASK_CREATE_TIMEOUT) {
                    Ok(_) => StepStatus::Done(Some(name.clone())),
                    Err(e) => StepStatus::Warned(format!("could not name the worktree: {e}")),
                })
            }
            Step::StartWorker(req) => {
                let timeout = Duration::from_millis(req.timeout_ms.unwrap_or(0))
                    + WORKER_START_GRACE
                    + Duration::from_secs(60) * u32::from(req.timeout_ms.is_none());
                let result = self.call(step, backend, timeout)?;
                let started: WorkerStarted = from_result(result).map_err(orca_error)?;
                self.ctx.dispatch_id = started.dispatch_id.clone();
                if let Some(handle) = started.agent_handle() {
                    self.ctx.handle = Some(handle.to_string());
                }
                if let Some(id) = started.worktree_id() {
                    self.ctx.worktree_id = Some(id.to_string());
                    if self.ctx.worktree_path.is_none() {
                        self.ctx.worktree_path = path_of_worktree_id(id);
                    }
                }
                if !started.is_ready() {
                    if !started.input_unconfirmed() || self.ctx.handle.is_none() {
                        return Err(started.failure());
                    }
                    // Orca often fails this stage while the agent takes the brief anyway; the
                    // screen check decides.
                    let failure = started.failure();
                    self.misreported = Some(failure.clone());
                    return Ok(StepStatus::Warned(format!(
                        "Orca reported {failure}; checking the agent's screen"
                    )));
                }
                let mut note = format!(
                    "dispatch {} ready in {}",
                    started.dispatch_id.as_deref().unwrap_or("?"),
                    self.ctx.handle.as_deref().unwrap_or("?")
                );
                if let Some(w) = &started.warning {
                    note.push_str(&format!("; Orca: {w}"));
                    return Ok(StepStatus::Warned(note));
                }
                Ok(StepStatus::Done(Some(note)))
            }
            Step::DirenvAllow => {
                let Some(path) = self.ctx.worktree_path.clone() else {
                    return Ok(StepStatus::Skipped("no worktree path".into()));
                };
                Ok(match backend.direnv_allow(&path) {
                    Ok(Some(note)) => StepStatus::Done(Some(note)),
                    Ok(None) => StepStatus::Skipped("no .envrc".into()),
                    Err(e) => StepStatus::Warned(e),
                })
            }
            Step::WaitIdle { timeout_ms } => {
                let timeout = Duration::from_millis(*timeout_ms) + WAIT_GRACE;
                let args = step
                    .orca_args(&self.ctx)
                    .expect("an orca step")
                    .map_err(|e| e.to_string())?;
                match backend.orca(&args, timeout) {
                    Ok(result) => {
                        let wait: TerminalWait =
                            from_result_field(result, "wait").map_err(orca_error)?;
                        if wait.status.as_deref() == Some("exited") {
                            return Err(format!(
                                "the agent exited{}",
                                wait.exit_code
                                    .map(|c| format!(" with code {c}"))
                                    .unwrap_or_default()
                            ));
                        }
                        Ok(match wait.blocked_reason {
                            Some(reason) => StepStatus::Done(Some(format!("blocked: {reason}"))),
                            None => StepStatus::Done(Some("idle".into())),
                        })
                    }
                    Err(e) if is_gone(&e) => Err("the agent's terminal is gone".into()),
                    // Busy: an agent working on its prompt is not idle.
                    Err(e) => Ok(StepStatus::Done(Some(format!(
                        "not idle yet ({})",
                        orca_error(e)
                    )))),
                }
            }
            Step::Verify {
                expect,
                allow_bypass,
            } => self.verify(expect, *allow_bypass, backend),
            Step::CloseStartupShell => Ok(self.close_startup_shell(backend)),
        }
    }

    /// Closes the terminals of the new worktree that are neither the agent's nor running an
    /// agent or setup. Never fails the spawn: the agent is already running.
    fn close_startup_shell(&mut self, backend: &mut dyn SpawnBackend) -> StepStatus {
        let (Some(worktree), Some(agent)) = (self.ctx.worktree_id.clone(), self.ctx.handle.clone())
        else {
            return StepStatus::Skipped("no new worktree or agent terminal".into());
        };
        let mut args = terminal_list_args();
        args.push(format!("--worktree=id:{worktree}"));
        let list: TerminalList = match backend
            .orca(&args, TERMINAL_CREATE_TIMEOUT)
            .and_then(from_result)
        {
            Ok(list) => list,
            Err(e) => {
                return StepStatus::Warned(format!(
                    "could not list the worktree's terminals: {}",
                    orca_error(e)
                ));
            }
        };
        let agent_tab = list
            .terminals
            .iter()
            .find(|t| t.handle == agent)
            .and_then(|t| t.tab_id.clone());
        let shells: Vec<&Terminal> = list
            .terminals
            .iter()
            .filter(|t| is_startup_shell(t, &worktree, &agent))
            .collect();
        if shells.is_empty() {
            return StepStatus::Skipped("no startup shell".into());
        }
        let (mut closed, mut failed) = (Vec::new(), Vec::new());
        for shell in shells {
            // A shell in its own tab goes with its tab; one split beside the agent only as a pane.
            let tab = shell.tab_id.is_some() && shell.tab_id != agent_tab;
            match backend.orca(
                &terminal_close_args(&shell.handle, tab),
                TERMINAL_CREATE_TIMEOUT,
            ) {
                Ok(_) => closed.push(shell.handle.clone()),
                Err(e) => failed.push(format!("{}: {}", shell.handle, orca_error(e))),
            }
        }
        if failed.is_empty() {
            StepStatus::Done(Some(format!("closed {}", closed.join(", "))))
        } else {
            StepStatus::Warned(format!(
                "could not close the startup shell ({})",
                failed.join("; ")
            ))
        }
    }

    fn verify(
        &mut self,
        expect: &Expect,
        allow_bypass: bool,
        backend: &mut dyn SpawnBackend,
    ) -> Result<StepStatus, String> {
        let handle = self
            .ctx
            .handle
            .clone()
            .ok_or("no agent terminal from an earlier step")?;
        let agent = self.plan.request.agent;
        let mut check = Verifier::new(agent, expect);
        let deadline = backend.elapsed() + VERIFY_DEADLINE;
        loop {
            let screen = match backend.read_screen(&handle) {
                Ok(lines) => Some(lines),
                Err(e) if is_gone(&e) => return Err("the agent's terminal is gone".into()),
                Err(e) => {
                    log::debug!("spawn: reading {handle}: {e}");
                    None
                }
            };
            if let Some(lines) = screen {
                if !allow_bypass
                    && classify_screen(&lines, agent, expect) == ScreenVerdict::BypassModal
                {
                    return Err("Restricted launch refused the Bypass Permissions prompt".into());
                }
                match check.next(&lines) {
                    VerifyStep::Done => {
                        let mut summary = check.summary();
                        if let Some(failure) = &self.misreported {
                            summary = format!("{summary}, although Orca reported {failure}");
                            self.notes.push(format!("{MISREPORTED_LAUNCH} ({failure})"));
                        }
                        self.verified = true;
                        self.notes.append(&mut check.notes);
                        if check.approvals_on {
                            return Ok(StepStatus::Warned(format!(
                                "{summary}; {CODEX_APPROVALS_ON}"
                            )));
                        }
                        return Ok(StepStatus::Done(Some(summary)));
                    }
                    VerifyStep::Send(input, why) => {
                        log::info!("spawn: {why} in {handle}");
                        if let Err(e) = backend.send(&handle, &input) {
                            check.notes.push(format!("{why} failed: {e}"));
                        }
                    }
                    VerifyStep::Wait => {}
                }
            }
            if backend.elapsed() >= deadline {
                if let Some(failure) = self.misreported.take() {
                    return Err(format!(
                        "{failure}; the agent's screen never showed the brief taken (last screen: \
                         {})",
                        check.last_label()
                    ));
                }
                if check.approvals_on {
                    check.notes.push(CODEX_APPROVALS_ON.into());
                }
                self.notes.append(&mut check.notes);
                if check.idle() {
                    return Ok(StepStatus::Warned(format!(
                        "prompt not seen yet after {} s (idle with an empty input box); check \
                         the terminal",
                        VERIFY_DEADLINE.as_secs()
                    )));
                }
                return Ok(StepStatus::Warned(format!(
                    "could not confirm the prompt started within {} s (last screen: {}); check \
                     the terminal",
                    VERIFY_DEADLINE.as_secs(),
                    check.last_label()
                )));
            }
            backend.sleep(VERIFY_POLL);
        }
    }
}

/// A plain shell in `worktree` other than the agent's terminal: no agent runs in it and it is
/// not a setup terminal.
fn is_startup_shell(t: &Terminal, worktree: &str, agent: &str) -> bool {
    let title = t.title.as_deref().unwrap_or_default().to_ascii_lowercase();
    t.worktree_id.as_deref() == Some(worktree)
        && t.handle != agent
        && t.agent_identity.is_none()
        && !title.contains("setup")
}

/// What the screen check does next.
#[derive(Debug, Clone, PartialEq)]
pub enum VerifyStep {
    Done,
    /// Send this input (and why, for the log and notes).
    Send(TerminalInput, &'static str),
    Wait,
}

/// The screen check's state between reads: which fixes it tried.
///
/// It never types the prompt: every launch hands it to the agent itself (on the command line or
/// through Orca), and an empty input box can just mean the agent hasn't submitted it yet (hooks
/// or MCP servers still connecting) or that its echo scrolled away after a quick reply. Typing
/// it again would run it twice.
#[derive(Debug, Clone)]
pub struct Verifier {
    agent: Agent,
    expect: Expect,
    bypass: u32,
    trust: u32,
    updates: u32,
    enters: u32,
    /// Codex's header showed it asking for approval (no "YOLO mode").
    pub approvals_on: bool,
    last: Option<ScreenVerdict>,
    pub notes: Vec<String>,
}

impl Verifier {
    pub fn new(agent: Agent, expect: &Expect) -> Self {
        Self {
            agent,
            expect: expect.clone(),
            bypass: 0,
            trust: 0,
            updates: 0,
            enters: 0,
            approvals_on: false,
            last: None,
            notes: Vec::new(),
        }
    }

    /// Decides on one screen read.
    pub fn next(&mut self, lines: &[String]) -> VerifyStep {
        if self.agent == Agent::Codex && codex_approvals_on(lines) {
            self.approvals_on = true;
        }
        let verdict = classify_screen(lines, self.agent, &self.expect);
        self.last = Some(verdict.clone());
        match verdict {
            ScreenVerdict::Started => VerifyStep::Done,
            ScreenVerdict::BypassModal if self.bypass < MAX_MODAL_ANSWERS => {
                self.bypass += 1;
                self.note("accepted the Bypass Permissions warning (2)");
                VerifyStep::Send(
                    TerminalInput::key(Key::Digit(2)),
                    "accept Bypass Permissions",
                )
            }
            ScreenVerdict::TrustPrompt if self.trust < MAX_MODAL_ANSWERS => {
                self.trust += 1;
                self.note("trusted the folder (1)");
                VerifyStep::Send(TerminalInput::key(Key::Digit(1)), "trust the folder")
            }
            ScreenVerdict::UpdatePrompt if self.updates < MAX_MODAL_ANSWERS => {
                self.updates += 1;
                self.note("skipped the agent's update screen (Esc)");
                VerifyStep::Send(TerminalInput::key(Key::Escape), "skip the update screen")
            }
            ScreenVerdict::Parked if self.enters < MAX_ENTERS => {
                self.enters += 1;
                self.note("submitted the parked prompt (Enter)");
                VerifyStep::Send(TerminalInput::enter_key(), "submit the parked prompt")
            }
            _ => VerifyStep::Wait,
        }
    }

    /// Whether the last read showed the agent idle with an empty input box.
    pub fn idle(&self) -> bool {
        self.last == Some(ScreenVerdict::IdleEmpty)
    }

    fn note(&mut self, text: &str) {
        self.notes.push(text.to_string());
    }

    /// The step's note once the prompt shows as started.
    fn summary(&self) -> String {
        if self.notes.is_empty() {
            "prompt taken".into()
        } else {
            format!("prompt taken after: {}", self.notes.join("; "))
        }
    }

    fn last_label(&self) -> &'static str {
        match self.last {
            None => "unreadable",
            Some(ScreenVerdict::Started) => "started",
            Some(ScreenVerdict::Parked) => "prompt still in the input box",
            Some(ScreenVerdict::BypassModal) => "Bypass Permissions warning",
            Some(ScreenVerdict::TrustPrompt) => "folder trust question",
            Some(ScreenVerdict::UpdatePrompt) => "update screen",
            Some(ScreenVerdict::IdleEmpty) => "idle with an empty input box",
            Some(ScreenVerdict::Booting) => "agent not drawn yet",
        }
    }
}

// ---------------------------------------------------------------------------
// Runner thread

/// Id of a submitted spawn.
pub type JobId = u64;

/// Something to run.
#[derive(Debug, Clone)]
pub enum Job {
    /// A plan built (and shown) in the dialog.
    Plan(SpawnPlan),
    /// API arguments, resolved on the thread against Orca's lists and these runs/terminals.
    Args {
        args: SpawnArgs,
        runs: Vec<Run>,
        terminals: Vec<Terminal>,
        /// Live agents, named when a new worktree's name is taken.
        agents: Vec<LiveAgent>,
    },
}

/// What the runner reports.
#[derive(Debug, Clone, PartialEq)]
pub enum SpawnEvent {
    /// The plan being run (for [`Job::Args`], after resolving).
    Planned { job: JobId, plan: Box<SpawnPlan> },
    Step {
        job: JobId,
        index: usize,
        status: StepStatus,
    },
    Done {
        job: JobId,
        result: Result<SpawnOutcome, SpawnFailure>,
    },
}

impl SpawnEvent {
    pub fn job(&self) -> JobId {
        match self {
            SpawnEvent::Planned { job, .. }
            | SpawnEvent::Step { job, .. }
            | SpawnEvent::Done { job, .. } => *job,
        }
    }
}

/// Runs one job: resolves it if needed, reports the plan, runs it.
pub fn run_job(
    job_id: JobId,
    job: Job,
    backend: &mut dyn SpawnBackend,
    emit: &mut dyn FnMut(SpawnEvent),
) {
    let plan = match job {
        Job::Plan(plan) => plan,
        Job::Args {
            args,
            runs,
            terminals,
            agents,
        } => {
            let plan = backend.catalog().and_then(|catalog| {
                let request = args.resolve(&catalog, &runs, &terminals)?;
                let plan = build_plan(&request).map_err(|e| e.0)?;
                if !args.allow_suffix {
                    check_new_name(&plan, &catalog, &agents, backend)?;
                }
                Ok(plan)
            });
            match plan {
                Ok(plan) => plan,
                Err(message) => {
                    emit(SpawnEvent::Done {
                        job: job_id,
                        result: Err(SpawnFailure {
                            step: None,
                            message,
                            context: Box::default(),
                        }),
                    });
                    return;
                }
            }
        }
    };
    emit(SpawnEvent::Planned {
        job: job_id,
        plan: Box::new(plan.clone()),
    });
    let result = run_plan(&plan, backend, &mut |Progress::Step { index, status }| {
        emit(SpawnEvent::Step {
            job: job_id,
            index,
            status,
        })
    });
    match &result {
        Ok(outcome) => log::info!("spawn: job {job_id} done: {:?}", outcome.context),
        Err(failure) => log::warn!("spawn: job {job_id} failed: {failure}"),
    }
    emit(SpawnEvent::Done {
        job: job_id,
        result,
    });
}

/// Fails when the plan's new worktree would get a suffixed name because its name or branch is
/// taken; the message names the existing worktree and the live agents in it.
pub fn check_new_name(
    plan: &SpawnPlan,
    catalog: &Catalog,
    agents: &[LiveAgent],
    backend: &mut dyn SpawnBackend,
) -> Result<(), String> {
    let Some((repo_id, name)) = new_worktree_of(&plan.request) else {
        return Ok(());
    };
    let branches = match catalog.repo(repo_id) {
        Some(repo) => backend.branches(&repo.path).unwrap_or_else(|e| {
            log::warn!("spawn: listing branches of {}: {e}", repo.path);
            Vec::new()
        }),
        None => Vec::new(),
    };
    match name_conflict(catalog, repo_id, name, &branches) {
        Some(conflict) => Err(conflict.message(agents)),
        None => Ok(()),
    }
}

/// Runs spawn jobs one at a time on a background thread.
pub struct SpawnRunner {
    jobs: Option<Sender<(JobId, Job)>>,
    events: Receiver<SpawnEvent>,
    next_id: JobId,
    thread: Option<JoinHandle<()>>,
}

impl SpawnRunner {
    pub fn spawn(mut backend: impl SpawnBackend, wake: Option<Wake>) -> Self {
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<(JobId, Job)>();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("spawn".into())
            .spawn(move || {
                for (id, job) in job_rx {
                    let mut emit = |event: SpawnEvent| {
                        let _ = event_tx.send(event);
                        if let Some(wake) = &wake {
                            wake();
                        }
                    };
                    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_job(id, job, &mut backend, &mut emit)
                    }));
                    if caught.is_err() {
                        emit(SpawnEvent::Done {
                            job: id,
                            result: Err(SpawnFailure {
                                step: None,
                                message: "the spawn thread panicked".into(),
                                context: Box::default(),
                            }),
                        });
                    }
                }
            })
            .expect("spawn the spawn thread");
        Self {
            jobs: Some(job_tx),
            events: event_rx,
            next_id: 1,
            thread: Some(thread),
        }
    }

    /// Queues `job`; its events carry the returned id.
    pub fn submit(&mut self, job: Job) -> JobId {
        let id = self.next_id;
        self.next_id += 1;
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send((id, job));
        }
        id
    }

    /// Events since the last call.
    pub fn poll(&self) -> Vec<SpawnEvent> {
        self.events.try_iter().collect()
    }
}

impl Drop for SpawnRunner {
    fn drop(&mut self) {
        // A running spawn may take minutes; don't block shutdown on it.
        self.jobs.take();
        if let Some(thread) = self.thread.take()
            && thread.is_finished()
        {
            let _ = thread.join();
        }
    }
}

//! Nudge and notify actions, plus the action log.
//!
//! [`WhipAction`] is what the engine decides; a [`WhipExecutor`] carries it out. Nudge texts are
//! pure functions so they can be tested and previewed. Every nudge starts with `Myrstack:` so
//! agents can recognise it (see `docs/heartbeat-convention.md`).

use std::sync::{Arc, Mutex};

use crate::notify::{Notifier, whip_request};
use crate::orca::{OrcaClient, OutgoingMessage, TerminalInput};
use crate::whip::rules::{StallReason, StallTarget};

/// Prefix of every nudge.
pub const NUDGE_PREFIX: &str = "Myrstack:";

/// How a nudge reaches its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NudgeVia {
    /// `orca orchestration send --to <to>`: `dispatch:<id>` for a worker, the terminal handle
    /// for a coordinator.
    Bus { to: String, run_id: Option<String> },
    /// `orca terminal send --text … --enter`, for a terminal idle at its prompt that ignored the
    /// bus nudge.
    Terminal { handle: String },
}

/// One decision of the whip engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhipAction {
    /// Ask a stalled terminal for a heartbeat. `targets` holds every stall folded into this one
    /// nudge (several tasks on one worker terminal, or several Runs of one coordinator).
    Nudge {
        handle: String,
        targets: Vec<StallTarget>,
        /// 1 for the first nudge of this stall, 2 for the second, …
        attempt: u32,
        via: NudgeVia,
        text: String,
    },
    /// Nudges were ignored: notify Karl and mark the node red. Sent once per stall.
    Escalate {
        target: StallTarget,
        reason: StallReason,
        evidence: String,
    },
    /// A terminal has waited on Karl longer than the threshold. Sent once per wait.
    NotifyNeedsInput { handle: String, since_ms: i64 },
}

impl WhipAction {
    /// Short description for the action log, e.g. `nudge #2 via terminal`.
    pub fn summary(&self) -> String {
        match self {
            Self::Nudge { attempt, via, .. } => {
                let via = match via {
                    NudgeVia::Bus { to, .. } => format!("bus to {to}"),
                    NudgeVia::Terminal { .. } => "terminal input".into(),
                };
                format!("nudge #{attempt} via {via}")
            }
            Self::Escalate { .. } => "escalate".into(),
            Self::NotifyNeedsInput { .. } => "notify needs-input".into(),
        }
    }

    /// Who the action is about, e.g. `worker term_1 task t1`.
    pub fn target_label(&self) -> String {
        match self {
            Self::Nudge { targets, .. } => targets
                .iter()
                .map(target_label)
                .collect::<Vec<_>>()
                .join(", "),
            Self::Escalate { target, .. } => target_label(target),
            Self::NotifyNeedsInput { handle, .. } => format!("terminal {handle}"),
        }
    }
}

/// `worker <handle> task <id>` or `coordinator <handle> run <id>`.
pub fn target_label(target: &StallTarget) -> String {
    match target {
        StallTarget::Worker {
            handle, task_id, ..
        } => format!("worker {handle} task {task_id}"),
        StallTarget::Coordinator { handle, run_id } => {
            format!("coordinator {handle} run {run_id}")
        }
    }
}

/// Short name of a stall reason for the log.
pub fn reason_label(reason: &StallReason) -> String {
    match reason {
        StallReason::WorkerNoProgress => "worker no progress".into(),
        StallReason::CoordinatorIdle { .. } => "coordinator idle".into(),
        StallReason::UnansweredAsk { message_id } => format!("unanswered ask {message_id}"),
        StallReason::QuietNoTasks => "quiet run without tasks".into(),
    }
}

// ---------------------------------------------------------------------------
// Nudge texts

/// A task named in a worker nudge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRef {
    pub id: String,
    /// Display name, when it differs from the id.
    pub title: Option<String>,
}

impl TaskRef {
    fn label(&self) -> String {
        match &self.title {
            Some(t) => format!("{} ({t})", self.id),
            None => self.id.clone(),
        }
    }
}

/// Worker nudge for one or more stalled tasks on the same terminal.
pub fn worker_nudge_text(tasks: &[TaskRef], idle_ms: i64) -> String {
    let (noun, list) = match tasks {
        [one] => ("task", one.label()),
        _ => (
            "tasks",
            tasks
                .iter()
                .map(TaskRef::label)
                .collect::<Vec<_>>()
                .join(", "),
        ),
    };
    let each = if tasks.len() > 1 { " for each" } else { "" };
    format!(
        "{NUDGE_PREFIX} no progress on {noun} {list} for {}. Send a heartbeat{each} with your \
         current phase and a one-line status, then continue — or worker_done / ask / escalation \
         if that's the truth.",
        fmt_duration(idle_ms)
    )
}

/// Coordinator nudge: `ready` tasks not started and/or finished tasks with no follow-up.
pub fn coordinator_idle_text(
    run_id: &str,
    ready: &[String],
    finished: &[String],
    idle_ms: i64,
) -> String {
    let mut work = Vec::new();
    if !ready.is_empty() {
        work.push(format!("ready task(s) not started: {}", ready.join(", ")));
    }
    if !finished.is_empty() {
        work.push(format!(
            "finished task(s) with no follow-up: {}",
            finished.join(", ")
        ));
    }
    format!(
        "{NUDGE_PREFIX} run {run_id} has had no coordinator activity for {} ({}). Dispatch or \
         task-update them, or send a `holding: <reason>` status if you're waiting on purpose.",
        fmt_duration(idle_ms),
        work.join("; ")
    )
}

/// Coordinator nudge: a worker's question has gone unanswered.
pub fn unanswered_ask_text(run_id: &str, message_id: &str, idle_ms: i64) -> String {
    format!(
        "{NUDGE_PREFIX} question {message_id} in run {run_id} has been unanswered for {} and the \
         worker is blocked on it. Reply to it, or send a `holding: <reason>` status.",
        fmt_duration(idle_ms)
    )
}

/// Coordinator nudge: a Run without tasks has gone quiet.
pub fn quiet_run_text(run_id: &str, idle_ms: i64) -> String {
    format!(
        "{NUDGE_PREFIX} run {run_id} has been quiet for {}. Carry on, or send a \
         `holding: <reason>` status if you're waiting on purpose.",
        fmt_duration(idle_ms)
    )
}

/// `45s`, `23m`, `2h05m`.
pub fn fmt_duration(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

// ---------------------------------------------------------------------------
// Log

/// One entry of the whip's action log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhipLogEntry {
    pub at_ms: i64,
    /// E.g. `nudge #1 via bus to dispatch:ctx_1`.
    pub action: String,
    /// E.g. `worker term_1 task t1`.
    pub target: String,
    /// Why, with evidence, e.g. the nudge text or the stall evidence.
    pub reason: String,
    pub dry_run: bool,
    /// `sent`, `dry-run`, `failed: …`, `notifications off`.
    pub outcome: String,
}

// ---------------------------------------------------------------------------
// Executors

/// Carries out whip actions. Implementations must be cheap to call from a background thread.
pub trait WhipExecutor {
    fn send_bus(&self, msg: &OutgoingMessage) -> anyhow::Result<()>;
    fn send_terminal(&self, handle: &str, text: &str) -> anyhow::Result<()>;
    fn notify(&self, title: &str, body: &str);
}

/// Sends through the `orca` CLI; notifications go to the [`Notifier`] when one is attached.
#[derive(Debug)]
pub struct OrcaExecutor {
    client: OrcaClient,
    notifier: Option<Arc<Notifier>>,
}

/// A client that runs `orca` without the launching terminal's identity, so nudges are not
/// attributed to the Orca terminal Myrstack was started from.
impl Default for OrcaExecutor {
    fn default() -> Self {
        Self::new(OrcaClient::new().without_sender_identity())
    }
}

impl OrcaExecutor {
    pub fn new(client: OrcaClient) -> Self {
        Self {
            client,
            notifier: None,
        }
    }

    /// Posts notifications through `notifier` instead of only logging them.
    pub fn with_notifier(mut self, notifier: Arc<Notifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }
}

impl WhipExecutor for OrcaExecutor {
    fn send_bus(&self, msg: &OutgoingMessage) -> anyhow::Result<()> {
        self.client.orchestration_send(msg)?;
        Ok(())
    }

    fn send_terminal(&self, handle: &str, text: &str) -> anyhow::Result<()> {
        let input = TerminalInput {
            text: Some(text.to_string()),
            enter: true,
            interrupt: false,
        };
        self.client.terminal_send(handle, &input)?;
        Ok(())
    }

    fn notify(&self, title: &str, body: &str) {
        log::warn!("whip notification: {title}: {body}");
        if let Some(notifier) = &self.notifier {
            notifier.notify(whip_request(title, body));
        }
    }
}

/// Sends nothing; records what it was asked to do. For dry runs and tests.
#[derive(Debug, Default)]
pub struct DryRunExecutor {
    calls: Mutex<Vec<String>>,
}

impl DryRunExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call so far, as `bus <to>: <subject>`, `terminal <handle>: <text>` or
    /// `notify <title>: <body>`.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn record(&self, call: String) {
        log::info!("whip dry-run: {call}");
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call);
        }
    }
}

impl WhipExecutor for DryRunExecutor {
    fn send_bus(&self, msg: &OutgoingMessage) -> anyhow::Result<()> {
        let to = msg.to.as_deref().unwrap_or("-");
        self.record(format!("bus {to}: {}", msg.subject));
        Ok(())
    }

    fn send_terminal(&self, handle: &str, text: &str) -> anyhow::Result<()> {
        self.record(format!("terminal {handle}: {text}"));
        Ok(())
    }

    fn notify(&self, title: &str, body: &str) {
        self.record(format!("notify {title}: {body}"));
    }
}

/// The bus message for a nudge sent via [`NudgeVia::Bus`]. The whole text goes in the subject,
/// which is what agents see first; a single worker task also goes in `--task-id`.
pub fn nudge_message(
    to: &str,
    run_id: Option<&str>,
    targets: &[StallTarget],
    text: &str,
) -> OutgoingMessage {
    let task_id = match targets {
        [StallTarget::Worker { task_id, .. }] => Some(task_id.clone()),
        _ => None,
    };
    OutgoingMessage {
        subject: text.to_string(),
        to: Some(to.to_string()),
        run: run_id.map(str::to_string),
        task_id,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn t(id: &str, title: Option<&str>) -> TaskRef {
        TaskRef {
            id: id.into(),
            title: title.map(str::to_string),
        }
    }

    #[test]
    fn worker_text_single_task() {
        assert_eq!(
            worker_nudge_text(&[t("t1", Some("Fix login"))], 17 * MIN),
            "Myrstack: no progress on task t1 (Fix login) for 17m. Send a heartbeat with your \
             current phase and a one-line status, then continue — or worker_done / ask / \
             escalation if that's the truth."
        );
    }

    #[test]
    fn worker_text_folds_tasks() {
        let text = worker_nudge_text(&[t("t1", None), t("t2", Some("Docs"))], 65 * MIN);
        assert!(text.starts_with("Myrstack: no progress on tasks t1, t2 (Docs) for 1h05m."));
        assert!(text.contains("heartbeat for each"));
    }

    #[test]
    fn coordinator_texts() {
        let idle = coordinator_idle_text("run_1", &["t2".into()], &["t1".into()], 20 * MIN);
        assert_eq!(
            idle,
            "Myrstack: run run_1 has had no coordinator activity for 20m (ready task(s) not \
             started: t2; finished task(s) with no follow-up: t1). Dispatch or task-update them, \
             or send a `holding: <reason>` status if you're waiting on purpose."
        );
        let ask = unanswered_ask_text("run_1", "msg_9", 30 * MIN);
        assert!(ask.starts_with("Myrstack: question msg_9 in run run_1"));
        assert!(ask.contains("worker is blocked on it"));
        let quiet = quiet_run_text("run_1", 16 * MIN);
        assert!(quiet.starts_with("Myrstack: run run_1 has been quiet for 16m."));
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(-5), "0s");
        assert_eq!(fmt_duration(45_000), "45s");
        assert_eq!(fmt_duration(23 * MIN), "23m");
        assert_eq!(fmt_duration(125 * MIN), "2h05m");
    }

    #[test]
    fn nudge_message_fields() {
        let worker = StallTarget::Worker {
            handle: "term_w".into(),
            task_id: "t1".into(),
            run_id: "run_1".into(),
        };
        let msg = nudge_message(
            "dispatch:ctx_1",
            None,
            std::slice::from_ref(&worker),
            "Myrstack: hi",
        );
        assert_eq!(msg.to.as_deref(), Some("dispatch:ctx_1"));
        assert_eq!(msg.task_id.as_deref(), Some("t1"));
        assert_eq!(msg.subject, "Myrstack: hi");
        assert_eq!(msg.from, None);
        let two = nudge_message("term_w", Some("run_1"), &[worker.clone(), worker], "x");
        assert_eq!(two.task_id, None);
        assert_eq!(two.run.as_deref(), Some("run_1"));
    }

    #[test]
    fn dry_run_executor_records() {
        let exec = DryRunExecutor::new();
        exec.send_terminal("term_1", "hello").unwrap();
        exec.notify("title", "body");
        assert_eq!(
            exec.calls(),
            vec!["terminal term_1: hello", "notify title: body"]
        );
    }
}

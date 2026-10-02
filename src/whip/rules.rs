//! Stall rules for workers and coordinators.
//!
//! [`evaluate`] turns a [`ProgressReport`] into [`StallFinding`]s: who looks stalled, why, and
//! for how long. It is pure and decides nothing about nudging; cooldowns and escalation live in
//! `actions`. See PLAN.md "Whip design" and `docs/heartbeat-convention.md`.

use std::collections::BTreeSet;

use crate::detect::claude_session_live;
use crate::fleet::FleetData;
use crate::orca::{Task, TaskStatus, Timestamp};
use crate::whip::progress::{ProgressReport, ProgressSource, RunProgress, TaskProgress};

/// Default stall threshold: 15 minutes.
pub const DEFAULT_THRESHOLD_MS: i64 = 15 * 60_000;
/// Default abandon window: a Run with no activity for 24 h is left alone.
pub const DEFAULT_ABANDON_AFTER_MS: i64 = 24 * 60 * 60_000;

/// Inputs to [`evaluate`] besides the report and the fleet data.
#[derive(Debug, Clone, Copy)]
pub struct RuleContext<'a> {
    /// Silence longer than this is a stall.
    pub threshold_ms: i64,
    /// A Run with no activity for longer than this is abandoned; see [`is_abandoned`].
    pub abandon_after_ms: i64,
    /// Terminal handles currently waiting on Karl (NeedsInput). They are never flagged.
    pub needs_input: &'a BTreeSet<String>,
}

impl<'a> RuleContext<'a> {
    /// Context with the default threshold.
    pub fn new(needs_input: &'a BTreeSet<String>) -> Self {
        Self {
            threshold_ms: DEFAULT_THRESHOLD_MS,
            abandon_after_ms: DEFAULT_ABANDON_AFTER_MS,
            needs_input,
        }
    }

    fn is_needs_input(&self, handle: &str) -> bool {
        self.needs_input.contains(handle)
    }
}

/// Who a finding is about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StallTarget {
    Worker {
        handle: String,
        task_id: String,
        run_id: String,
    },
    Coordinator {
        handle: String,
        run_id: String,
    },
}

impl StallTarget {
    pub fn handle(&self) -> &str {
        match self {
            Self::Worker { handle, .. } | Self::Coordinator { handle, .. } => handle,
        }
    }

    pub fn run_id(&self) -> &str {
        match self {
            Self::Worker { run_id, .. } | Self::Coordinator { run_id, .. } => run_id,
        }
    }
}

/// Why a target looks stalled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StallReason {
    /// A dispatched task's owner has shown no progress within the threshold.
    WorkerNoProgress,
    /// A dispatched task's owner sits idle at its Claude prompt past the threshold and never
    /// reported (`worker_done` / `merge_ready`) on this dispatch: it looks finished, but its
    /// report never reached the bus (e.g. its `orca orchestration send` failed).
    MissingReport,
    /// The coordinator has been idle while it has work: `ready` tasks nobody started, or
    /// finished tasks it has not acted on.
    CoordinatorIdle {
        ready_tasks: Vec<String>,
        finished_tasks: Vec<String>,
    },
    /// A worker's `ask` has gone unanswered for longer than the threshold.
    UnansweredAsk { message_id: String },
    /// A Run without tasks whose coordinator has gone quiet. Softer: it may simply be done.
    QuietNoTasks,
}

impl StallReason {
    /// Higher is more severe; used to keep one finding per target.
    fn severity(&self) -> u8 {
        match self {
            Self::QuietNoTasks => 0,
            Self::WorkerNoProgress | Self::MissingReport | Self::CoordinatorIdle { .. } => 1,
            Self::UnansweredAsk { .. } => 2,
        }
    }
}

/// One stalled target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StallFinding {
    pub target: StallTarget,
    pub reason: StallReason,
    /// How long the target has been silent, in ms.
    pub idle_for_ms: i64,
    /// Human-readable explanation for the action log.
    pub evidence: String,
}

/// Applies the stall rules to one poll's progress report. At most one finding per target: the
/// most severe, then the longest idle.
pub fn evaluate(
    report: &ProgressReport,
    data: &FleetData,
    ctx: &RuleContext<'_>,
    now_ms: i64,
) -> Vec<StallFinding> {
    let mut findings = Vec::new();
    let abandoned = |run_id: &str| {
        report
            .runs
            .iter()
            .find(|r| r.run_id == run_id)
            .is_some_and(|r| is_abandoned(r, ctx, now_ms))
    };
    let superseded: BTreeSet<&str> = report
        .runs
        .iter()
        .map(|r| r.run_id.as_str())
        .filter(|id| is_superseded(id, data))
        .collect();
    let silent_past_window = |handle: &str| {
        data.terminal(handle)
            .and_then(|t| ts_ms(&t.last_output_at))
            .is_none_or(|out| now_ms - out > ctx.abandon_after_ms)
    };
    for task in &report.tasks {
        findings.extend(worker_stalled(task, data, ctx, now_ms).filter(|f| {
            let quiet = f.idle_for_ms > ctx.abandon_after_ms && abandoned(&task.run_id);
            let left_behind =
                superseded.contains(task.run_id.as_str()) && silent_past_window(f.target.handle());
            !quiet && !left_behind
        }));
    }
    for run in &report.runs {
        if is_abandoned(run, ctx, now_ms) || superseded.contains(run.run_id.as_str()) {
            continue;
        }
        let Some(coordinator) = effective_coordinator(&run.run_id, data) else {
            continue;
        };
        if ctx.is_needs_input(&coordinator) {
            continue;
        }
        findings.extend(coordinator_idle(
            run,
            &coordinator,
            report,
            data,
            ctx,
            now_ms,
        ));
        findings.extend(unanswered_asks(run, &coordinator, data, ctx, now_ms));
        findings.extend(quiet_taskless_run(run, &coordinator, data, ctx, now_ms));
    }
    dedupe(findings)
}

/// A Run is abandoned when nothing has happened in it (see [`RunProgress::last_activity_ms`])
/// for longer than the abandon window. Its coordinator is never nudged, nor a worker of it
/// that has been silent that long too. A Run with no known activity time is not abandoned.
pub fn is_abandoned(run: &RunProgress, ctx: &RuleContext<'_>, now_ms: i64) -> bool {
    run.last_activity_ms
        .is_some_and(|a| now_ms - a > ctx.abandon_after_ms)
}

/// A Run is superseded when its effective coordinator is also the coordinator (effective or
/// declared) of a newer Run: the terminal has moved on, so its output and silence are about
/// the newer Run. Its coordinator is never nudged, nor a worker of it whose terminal has been
/// silent for longer than the abandon window.
pub fn is_superseded(run_id: &str, data: &FleetData) -> bool {
    let Some(coordinator) = effective_coordinator(run_id, data) else {
        return false;
    };
    let created = data.run(run_id).and_then(|r| ts_ms(&r.created_at));
    data.runs.iter().any(|other| {
        other.id != run_id
            && ts_ms(&other.created_at) > created
            && (other.coordinator_handle.as_deref() == Some(coordinator.as_str())
                || effective_coordinator(&other.id, data).as_deref() == Some(coordinator.as_str()))
    })
}

/// The terminal actually coordinating a Run; see [`FleetData::effective_coordinator`].
pub fn effective_coordinator(run_id: &str, data: &FleetData) -> Option<String> {
    data.effective_coordinator(run_id)
}

/// Rule 1: a dispatched task whose live owner has shown no progress within the threshold, or
/// a failed task whose worker kept working (see [`TaskProgress::failed_but_alive`]) and then
/// went silent. A worker waiting on an unanswered ask is the coordinator's stall (rule 3), not
/// its own.
///
/// Rule 1b takes precedence over it for the same stall: when the dispatched task's owner is a
/// live Claude session idle at its prompt (silent past the threshold, so not mid-turn) and has
/// not sent `worker_done` / `merge_ready` on this dispatch, the reason is
/// [`StallReason::MissingReport`].
fn worker_stalled(
    task: &TaskProgress,
    data: &FleetData,
    ctx: &RuleContext<'_>,
    now_ms: i64,
) -> Option<StallFinding> {
    let working = task.status == TaskStatus::Dispatched || task.failed_but_alive;
    if !working || task.waiting_on_ask {
        return None;
    }
    let owner = task.owner_handle.as_deref()?;
    data.terminal(owner)?;
    if ctx.is_needs_input(owner) {
        return None;
    }
    let idle = now_ms - task.last_progress_ms?;
    if idle <= ctx.threshold_ms {
        return None;
    }
    let phase = task
        .phase
        .as_deref()
        .map(|p| format!(", last phase {p}"))
        .unwrap_or_default();
    let target = StallTarget::Worker {
        handle: owner.to_string(),
        task_id: task.task_id.clone(),
        run_id: task.run_id.clone(),
    };
    if looks_unreported(task, data, owner) {
        return Some(StallFinding {
            target,
            reason: StallReason::MissingReport,
            idle_for_ms: idle,
            evidence: format!(
                "task {} dispatched, worker idle at its prompt for {} and no worker_done or \
                 merge_ready on the bus{phase}",
                task_label(data, &task.run_id, &task.task_id),
                fmt_duration(idle),
            ),
        });
    }
    Some(StallFinding {
        target,
        reason: StallReason::WorkerNoProgress,
        idle_for_ms: idle,
        evidence: format!(
            "task {} {}, no progress for {} (last signal: {}{phase})",
            task_label(data, &task.run_id, &task.task_id),
            if task.failed_but_alive {
                "failed in Orca but its worker kept going"
            } else {
                "dispatched"
            },
            fmt_duration(idle),
            source_label(&task.source),
        ),
    })
}

/// See rule 1b on [`worker_stalled`]; the caller has checked the silence.
fn looks_unreported(task: &TaskProgress, data: &FleetData, owner: &str) -> bool {
    task.status == TaskStatus::Dispatched
        && !task.failed_but_alive
        && !task.reported
        && data.terminal(owner).is_some_and(claude_session_live)
}

/// Rule 2: the coordinator has `ready` tasks nobody started, or tasks that finished more than
/// the threshold ago with no coordinator activity since, and has itself been idle longer than
/// the threshold. A `holding:` status exempts it.
fn coordinator_idle(
    run: &RunProgress,
    coordinator: &str,
    report: &ProgressReport,
    data: &FleetData,
    ctx: &RuleContext<'_>,
    now_ms: i64,
) -> Option<StallFinding> {
    if run.holding.is_some() {
        return None;
    }
    let activity = run.last_coordinator_activity_ms;
    let run_tasks = data
        .tasks
        .get(&run.run_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut unhandled_since: Vec<i64> = Vec::new();
    let mut finished_tasks = Vec::new();
    for task in report.tasks.iter().filter(|t| t.run_id == run.run_id) {
        if !matches!(task.status, TaskStatus::Completed | TaskStatus::Failed) {
            continue;
        }
        let completed_at = run_tasks
            .iter()
            .find(|t| t.id == task.task_id)
            .and_then(|t| ts_ms(&t.completed_at));
        let Some(finished) = task.last_progress_ms.max(completed_at) else {
            continue;
        };
        let handled = activity.is_some_and(|a| a >= finished);
        if !handled && now_ms - finished > ctx.threshold_ms {
            finished_tasks.push(task.task_id.clone());
            unhandled_since.push(finished);
        }
    }
    let ready_tasks = run.open_ready_tasks.clone();
    if ready_tasks.is_empty() && finished_tasks.is_empty() {
        return None;
    }

    // Without any recorded activity, measure from the oldest piece of pending work.
    let ready_since = report
        .tasks
        .iter()
        .filter(|t| ready_tasks.contains(&t.task_id))
        .filter_map(|t| t.last_progress_ms);
    let since = activity.or_else(|| unhandled_since.into_iter().chain(ready_since).min())?;
    let idle = now_ms - since;
    if idle <= ctx.threshold_ms {
        return None;
    }

    let mut work = Vec::new();
    if !ready_tasks.is_empty() {
        work.push(format!(
            "{} ready task(s) not started ({})",
            ready_tasks.len(),
            labels(data, &run.run_id, &ready_tasks)
        ));
    }
    if !finished_tasks.is_empty() {
        work.push(format!(
            "{} finished task(s) with no follow-up ({})",
            finished_tasks.len(),
            labels(data, &run.run_id, &finished_tasks)
        ));
    }
    Some(StallFinding {
        target: coordinator_target(coordinator, run),
        reason: StallReason::CoordinatorIdle {
            ready_tasks,
            finished_tasks,
        },
        idle_for_ms: idle,
        evidence: format!(
            "coordinator of {} idle for {} with {}",
            run.run_id,
            fmt_duration(idle),
            work.join(" and ")
        ),
    })
}

/// Rule 3: an unanswered ask older than the threshold. Keeps the oldest one.
fn unanswered_asks(
    run: &RunProgress,
    coordinator: &str,
    data: &FleetData,
    ctx: &RuleContext<'_>,
    now_ms: i64,
) -> Option<StallFinding> {
    run.unanswered_asks
        .iter()
        .filter_map(|id| {
            let msg = data.inbox.iter().find(|m| &m.id == id)?;
            let idle = now_ms - ts_ms(&msg.created_at)?;
            (idle > ctx.threshold_ms).then_some((msg, idle))
        })
        .max_by_key(|(_, idle)| *idle)
        .map(|(msg, idle)| {
            let asker = msg.from_handle.as_deref().unwrap_or("a worker");
            let subject = msg.subject.as_deref().unwrap_or("");
            StallFinding {
                target: coordinator_target(coordinator, run),
                reason: StallReason::UnansweredAsk {
                    message_id: msg.id.clone(),
                },
                idle_for_ms: idle,
                evidence: format!(
                    "question {} from {asker} unanswered for {}: {subject}",
                    msg.id,
                    fmt_duration(idle)
                ),
            }
        })
}

/// Rule 4: a Run with no tasks whose coordinator has been quiet longer than the threshold.
/// Skipped when the Run looks finished: abandoned or superseded (checked by the caller), or its
/// record not updated within the abandon window (a taskless Run has no other Run-specific
/// signal). Runs whose tasks were not fetched are skipped too.
fn quiet_taskless_run(
    run: &RunProgress,
    coordinator: &str,
    data: &FleetData,
    ctx: &RuleContext<'_>,
    now_ms: i64,
) -> Option<StallFinding> {
    if !data.tasks.get(&run.run_id)?.is_empty() || run.holding.is_some() {
        return None;
    }
    let meta = data.run(&run.run_id);
    let updated = meta
        .and_then(|r| ts_ms(&r.updated_at))
        .or_else(|| meta.and_then(|r| ts_ms(&r.created_at)));
    if updated.is_some_and(|u| now_ms - u > ctx.abandon_after_ms) {
        return None;
    }
    let idle = now_ms - run.last_coordinator_activity_ms?;
    if idle <= ctx.threshold_ms {
        return None;
    }
    Some(StallFinding {
        target: coordinator_target(coordinator, run),
        reason: StallReason::QuietNoTasks,
        idle_for_ms: idle,
        evidence: format!(
            "run {} has no tasks and its coordinator has been quiet for {}",
            run.run_id,
            fmt_duration(idle)
        ),
    })
}

/// Keeps the most severe, then longest-idle, finding per target, in first-seen order.
fn dedupe(findings: Vec<StallFinding>) -> Vec<StallFinding> {
    let mut kept: Vec<StallFinding> = Vec::new();
    for f in findings {
        match kept.iter_mut().find(|k| k.target == f.target) {
            Some(k) => {
                if (f.reason.severity(), f.idle_for_ms) > (k.reason.severity(), k.idle_for_ms) {
                    *k = f;
                }
            }
            None => kept.push(f),
        }
    }
    kept
}

fn coordinator_target(handle: &str, run: &RunProgress) -> StallTarget {
    StallTarget::Coordinator {
        handle: handle.to_string(),
        run_id: run.run_id.clone(),
    }
}

fn task_label(data: &FleetData, run_id: &str, task_id: &str) -> String {
    let label = data
        .tasks
        .get(run_id)
        .and_then(|ts| ts.iter().find(|t| t.id == task_id))
        .map(Task::label)
        .filter(|l| *l != task_id);
    match label {
        Some(l) => format!("{task_id} ({l})"),
        None => task_id.to_string(),
    }
}

fn labels(data: &FleetData, run_id: &str, ids: &[String]) -> String {
    ids.iter()
        .map(|id| task_label(data, run_id, id))
        .collect::<Vec<_>>()
        .join(", ")
}

fn source_label(source: &ProgressSource) -> String {
    match source {
        ProgressSource::Heartbeat { .. } => "heartbeat".into(),
        ProgressSource::Message { kind } => format!("{kind} message"),
        ProgressSource::StatusChange => "status change".into(),
        ProgressSource::TerminalOutput => "terminal output".into(),
        ProgressSource::None => "none".into(),
    }
}

/// `45s`, `23m`, `2h05m`.
fn fmt_duration(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn ts_ms(ts: &Option<Timestamp>) -> Option<i64> {
    ts.as_ref().and_then(Timestamp::epoch_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::{InboxMessage, Run, Terminal, Worker};
    use crate::whip::progress::ProgressTracker;
    use serde_json::{Value, json};

    const RUN: &str = "run_1";
    const COORD: &str = "term_coord";
    const WORKER: &str = "term_worker";
    const MIN: i64 = 60_000;
    const NOW: i64 = 1_790_000_000_000;

    fn run(id: &str, coordinator: &str, created: i64, updated: i64) -> Run {
        serde_json::from_value(json!({
            "id": id, "coordinator_handle": coordinator,
            "created_at": created, "updated_at": updated,
        }))
        .unwrap()
    }

    fn terminal(handle: &str) -> Terminal {
        Terminal {
            handle: handle.into(),
            ..Default::default()
        }
    }

    fn task(id: &str, status: &str, created_by: &str, created_at: i64) -> Task {
        serde_json::from_value(json!({
            "id": id, "run_id": RUN, "status": status,
            "created_by_terminal_handle": created_by, "created_at": created_at,
        }))
        .unwrap()
    }

    fn worker(task_id: &str, handle: &str) -> Worker {
        serde_json::from_value(json!({
            "dispatchId": format!("ctx_{task_id}"), "taskId": task_id, "runId": RUN,
            "dispatchStatus": "dispatched", "agentTerminalHandle": handle,
        }))
        .unwrap()
    }

    fn msg(id: &str, from: &str, kind: &str, at: i64, payload: Value) -> InboxMessage {
        serde_json::from_value(json!({
            "id": id, "run_id": RUN, "from_handle": from, "to_handle": format!("run:{RUN}"),
            "type": kind, "subject": kind, "payload": payload.to_string(), "created_at": at,
        }))
        .unwrap()
    }

    fn task_progress(id: &str, status: TaskStatus, last_progress: i64) -> TaskProgress {
        TaskProgress {
            task_id: id.into(),
            run_id: RUN.into(),
            status,
            owner_handle: Some(WORKER.into()),
            dispatch_id: Some(format!("ctx_{id}")),
            last_progress_ms: Some(last_progress),
            source: ProgressSource::Heartbeat {
                phase: Some("implementing".into()),
            },
            phase: Some("implementing".into()),
            waiting_on_ask: false,
            failed_but_alive: false,
            reported: false,
        }
    }

    /// A Run with a live coordinator and one dispatched task `t1` owned by a live worker. Both
    /// have been active within the last minute.
    struct Scenario {
        data: FleetData,
        report: ProgressReport,
        needs_input: BTreeSet<String>,
        abandon_after_ms: i64,
    }

    impl Scenario {
        fn new() -> Self {
            let data = FleetData {
                terminals: vec![terminal(COORD), terminal(WORKER)],
                runs: vec![run(RUN, COORD, NOW - 60 * MIN, NOW - MIN)],
                workers: vec![worker("t1", WORKER)],
                tasks: [(
                    RUN.to_string(),
                    vec![task("t1", "dispatched", COORD, NOW - 60 * MIN)],
                )]
                .into(),
                ..Default::default()
            };
            let report = ProgressReport {
                tasks: vec![task_progress("t1", TaskStatus::Dispatched, NOW - MIN)],
                runs: vec![RunProgress {
                    run_id: RUN.into(),
                    coordinator_handle: Some(COORD.into()),
                    last_coordinator_activity_ms: Some(NOW - MIN),
                    source: ProgressSource::TerminalOutput,
                    open_ready_tasks: vec![],
                    unanswered_asks: vec![],
                    holding: None,
                    last_activity_ms: Some(NOW - MIN),
                }],
            };
            Self {
                data,
                report,
                needs_input: BTreeSet::new(),
                abandon_after_ms: DEFAULT_ABANDON_AFTER_MS,
            }
        }

        fn eval(&self) -> Vec<StallFinding> {
            let ctx = RuleContext {
                abandon_after_ms: self.abandon_after_ms,
                ..RuleContext::new(&self.needs_input)
            };
            evaluate(&self.report, &self.data, &ctx, NOW)
        }

        fn t1(&mut self) -> &mut TaskProgress {
            &mut self.report.tasks[0]
        }

        fn run(&mut self) -> &mut RunProgress {
            &mut self.report.runs[0]
        }

        fn add_ready(&mut self, id: &str, since: i64) {
            self.data
                .tasks
                .get_mut(RUN)
                .unwrap()
                .push(task(id, "ready", COORD, since));
            let mut p = task_progress(id, TaskStatus::Ready, since);
            p.owner_handle = None;
            self.report.tasks.push(p);
            self.run().open_ready_tasks.push(id.into());
        }

        fn complete_t1(&mut self, at: i64) {
            self.data.tasks.get_mut(RUN).unwrap()[0].status = TaskStatus::Completed;
            self.data.tasks.get_mut(RUN).unwrap()[0].completed_at = Some(Timestamp::Millis(at));
            let t1 = self.t1();
            t1.status = TaskStatus::Completed;
            t1.last_progress_ms = Some(at);
        }

        fn ask(&mut self, id: &str, at: i64) {
            self.data.inbox.push(msg(
                id,
                "dispatch:ctx_t1",
                "question",
                at,
                json!({"taskId": "t1"}),
            ));
            self.t1().waiting_on_ask = true;
            self.run().unanswered_asks.push(id.into());
        }

        fn make_taskless(&mut self) {
            self.data.tasks.insert(RUN.into(), vec![]);
            self.data.workers.clear();
            self.report.tasks.clear();
        }
    }

    fn worker_target() -> StallTarget {
        StallTarget::Worker {
            handle: WORKER.into(),
            task_id: "t1".into(),
            run_id: RUN.into(),
        }
    }

    fn coord_target() -> StallTarget {
        StallTarget::Coordinator {
            handle: COORD.into(),
            run_id: RUN.into(),
        }
    }

    /// `(target, reason, idle minutes)` of each finding.
    fn summary(findings: &[StallFinding]) -> Vec<(StallTarget, StallReason, i64)> {
        findings
            .iter()
            .map(|f| (f.target.clone(), f.reason.clone(), f.idle_for_ms / MIN))
            .collect()
    }

    type Case = (
        &'static str,
        fn(&mut Scenario),
        Vec<(StallTarget, StallReason, i64)>,
    );

    fn run_cases(cases: Vec<Case>) {
        for (name, setup, expected) in cases {
            let mut s = Scenario::new();
            setup(&mut s);
            assert_eq!(summary(&s.eval()), expected, "case: {name}");
        }
    }

    #[test]
    fn worker_stalled_rule() {
        let stalled = || vec![(worker_target(), StallReason::WorkerNoProgress, 20)];
        run_cases(vec![
            ("all active", |_| {}, vec![]),
            (
                "silent past threshold",
                |s| s.t1().last_progress_ms = Some(NOW - 20 * MIN),
                stalled(),
            ),
            (
                "silent exactly threshold",
                |s| s.t1().last_progress_ms = Some(NOW - 15 * MIN),
                vec![],
            ),
            (
                "owner NeedsInput",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 20 * MIN);
                    s.needs_input.insert(WORKER.into());
                },
                vec![],
            ),
            (
                "owner terminal gone",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 20 * MIN);
                    s.data.terminals.retain(|t| t.handle != WORKER);
                },
                vec![],
            ),
            (
                "owner unknown",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 20 * MIN);
                    s.t1().owner_handle = None;
                },
                vec![],
            ),
            (
                "no progress signal at all",
                |s| s.t1().last_progress_ms = None,
                vec![],
            ),
            (
                "not dispatched",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 20 * MIN);
                    s.t1().status = TaskStatus::Blocked;
                },
                vec![],
            ),
            (
                "waiting on a fresh ask is not the worker's stall",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 20 * MIN);
                    s.ask("q1", NOW - 5 * MIN);
                },
                vec![],
            ),
        ]);
    }

    /// Puts a live, idle Claude session in the worker's terminal and silences `t1` for 20 min.
    fn idle_claude_worker(s: &mut Scenario) {
        let worker = s
            .data
            .terminals
            .iter_mut()
            .find(|t| t.handle == WORKER)
            .unwrap();
        worker.agent_identity = Some(crate::orca::AgentIdentity::Claude);
        worker.preview = Some("⏺ Done, PR #9 open.\nnew task? /clear to save 80k tokens".into());
        s.t1().last_progress_ms = Some(NOW - 20 * MIN);
    }

    #[test]
    fn missing_report_rule() {
        let missing = || vec![(worker_target(), StallReason::MissingReport, 20)];
        let no_progress = || vec![(worker_target(), StallReason::WorkerNoProgress, 20)];
        run_cases(vec![
            (
                "idle Claude worker, never reported",
                idle_claude_worker,
                missing(),
            ),
            (
                "still within the threshold",
                |s| {
                    idle_claude_worker(s);
                    s.t1().last_progress_ms = Some(NOW - 10 * MIN);
                },
                vec![],
            ),
            (
                "already sent merge_ready: plain stall",
                |s| {
                    idle_claude_worker(s);
                    s.t1().reported = true;
                },
                no_progress(),
            ),
            (
                "not a live Claude session: plain stall",
                |s| s.t1().last_progress_ms = Some(NOW - 20 * MIN),
                no_progress(),
            ),
            (
                "failed but alive: plain stall",
                |s| {
                    idle_claude_worker(s);
                    s.t1().status = TaskStatus::Failed;
                    s.t1().failed_but_alive = true;
                },
                no_progress(),
            ),
            (
                "pending ask: the coordinator's stall",
                |s| {
                    idle_claude_worker(s);
                    s.ask("q1", NOW - 5 * MIN);
                },
                vec![],
            ),
            (
                "waiting on Karl",
                |s| {
                    idle_claude_worker(s);
                    s.needs_input.insert(WORKER.into());
                },
                vec![],
            ),
        ]);
        let mut s = Scenario::new();
        idle_claude_worker(&mut s);
        let evidence = &s.eval()[0].evidence;
        assert!(
            evidence.contains("no worker_done or merge_ready on the bus"),
            "{evidence}"
        );
    }

    #[test]
    fn coordinator_idle_rule() {
        run_cases(vec![
            (
                "ready task, coordinator idle",
                |s| {
                    s.add_ready("t2", NOW - 40 * MIN);
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                },
                vec![(
                    coord_target(),
                    StallReason::CoordinatorIdle {
                        ready_tasks: vec!["t2".into()],
                        finished_tasks: vec![],
                    },
                    20,
                )],
            ),
            (
                "ready task, coordinator recently active",
                |s| s.add_ready("t2", NOW - 40 * MIN),
                vec![],
            ),
            (
                "ready task, no recorded activity: idle since the task became ready",
                |s| {
                    s.add_ready("t2", NOW - 40 * MIN);
                    s.run().last_coordinator_activity_ms = None;
                },
                vec![(
                    coord_target(),
                    StallReason::CoordinatorIdle {
                        ready_tasks: vec!["t2".into()],
                        finished_tasks: vec![],
                    },
                    40,
                )],
            ),
            (
                "finished task with no follow-up",
                |s| {
                    s.complete_t1(NOW - 20 * MIN);
                    s.run().last_coordinator_activity_ms = Some(NOW - 30 * MIN);
                },
                vec![(
                    coord_target(),
                    StallReason::CoordinatorIdle {
                        ready_tasks: vec![],
                        finished_tasks: vec!["t1".into()],
                    },
                    30,
                )],
            ),
            (
                "finished task followed up",
                |s| {
                    s.complete_t1(NOW - 30 * MIN);
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                },
                vec![],
            ),
            (
                "finished task too recent",
                |s| {
                    s.complete_t1(NOW - 10 * MIN);
                    s.run().last_coordinator_activity_ms = Some(NOW - 30 * MIN);
                },
                vec![],
            ),
            (
                "holding",
                |s| {
                    s.add_ready("t2", NOW - 40 * MIN);
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                    s.run().holding = Some("merge window".into());
                },
                vec![],
            ),
            (
                "coordinator NeedsInput",
                |s| {
                    s.add_ready("t2", NOW - 40 * MIN);
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                    s.needs_input.insert(COORD.into());
                },
                vec![],
            ),
            (
                "coordinator dead",
                |s| {
                    s.add_ready("t2", NOW - 40 * MIN);
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                    s.data.terminals.retain(|t| t.handle != COORD);
                },
                vec![],
            ),
        ]);
    }

    #[test]
    fn unanswered_ask_rule() {
        let ask = |id: &str, mins| {
            vec![(
                coord_target(),
                StallReason::UnansweredAsk {
                    message_id: id.into(),
                },
                mins,
            )]
        };
        run_cases(vec![
            (
                "old ask flags the coordinator, not the silent worker",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 20 * MIN);
                    s.ask("q1", NOW - 20 * MIN);
                },
                ask("q1", 20),
            ),
            ("fresh ask", |s| s.ask("q1", NOW - 5 * MIN), vec![]),
            (
                "coordinator NeedsInput",
                |s| {
                    s.ask("q1", NOW - 20 * MIN);
                    s.needs_input.insert(COORD.into());
                },
                vec![],
            ),
            (
                "holding does not excuse an unanswered ask",
                |s| {
                    s.ask("q1", NOW - 20 * MIN);
                    s.run().holding = Some("CI".into());
                },
                ask("q1", 20),
            ),
            (
                "oldest of several asks",
                |s| {
                    s.ask("q1", NOW - 20 * MIN);
                    s.ask("q2", NOW - 50 * MIN);
                },
                ask("q2", 50),
            ),
        ]);
    }

    #[test]
    fn taskless_run_rule() {
        run_cases(vec![
            (
                "quiet taskless run",
                |s| {
                    s.make_taskless();
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                },
                vec![(coord_target(), StallReason::QuietNoTasks, 20)],
            ),
            ("recently active", |s| s.make_taskless(), vec![]),
            (
                "not updated in 24h",
                |s| {
                    s.make_taskless();
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                    s.data.runs[0] = run(RUN, COORD, NOW - 30 * 60 * MIN, NOW - 25 * 60 * MIN);
                },
                vec![],
            ),
            (
                "coordinator terminal gone",
                |s| {
                    s.make_taskless();
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                    s.data.terminals.retain(|t| t.handle != COORD);
                },
                vec![],
            ),
            (
                "coordinator moved on to a newer run",
                |s| {
                    s.make_taskless();
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                    s.data
                        .runs
                        .push(run("run_2", COORD, NOW - 30 * MIN, NOW - MIN));
                },
                vec![],
            ),
            (
                "tasks not fetched",
                |s| {
                    s.make_taskless();
                    s.data.tasks.clear();
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                },
                vec![],
            ),
            (
                "holding",
                |s| {
                    s.make_taskless();
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                    s.run().holding = Some("waiting for Karl".into());
                },
                vec![],
            ),
            (
                "coordinator NeedsInput",
                |s| {
                    s.make_taskless();
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                    s.needs_input.insert(COORD.into());
                },
                vec![],
            ),
        ]);
    }

    #[test]
    fn abandoned_runs_are_skipped() {
        const HOUR: i64 = 60 * MIN;
        /// Everything in the Run last happened `hours` ago.
        fn quiet_for(s: &mut Scenario, hours: i64) {
            let at = NOW - hours * HOUR;
            s.data.runs[0] = run(RUN, COORD, at - HOUR, at);
            s.run().last_coordinator_activity_ms = Some(at);
            s.run().last_activity_ms = Some(at);
        }
        let idle = |finished: bool, mins| {
            vec![(
                coord_target(),
                StallReason::CoordinatorIdle {
                    ready_tasks: if finished { vec![] } else { vec!["t2".into()] },
                    finished_tasks: if finished { vec!["t1".into()] } else { vec![] },
                },
                mins,
            )]
        };
        run_cases(vec![
            (
                "finished task with no follow-up, run idle 719h",
                |s| {
                    s.complete_t1(NOW - 719 * HOUR);
                    quiet_for(s, 719);
                },
                vec![],
            ),
            (
                "finished task with no follow-up, run idle 23h",
                |s| {
                    s.complete_t1(NOW - 22 * HOUR);
                    quiet_for(s, 23);
                },
                idle(true, 23 * 60),
            ),
            (
                "ready task in an abandoned run",
                |s| {
                    s.add_ready("t2", NOW - 30 * HOUR);
                    quiet_for(s, 25);
                },
                vec![],
            ),
            (
                "ready task, coordinator quiet 25h but the run is active",
                |s| {
                    s.add_ready("t2", NOW - 30 * HOUR);
                    quiet_for(s, 25);
                    s.run().last_activity_ms = Some(NOW - HOUR);
                },
                idle(false, 25 * 60),
            ),
            (
                "unanswered ask in an abandoned run",
                |s| {
                    s.ask("q1", NOW - 30 * HOUR);
                    quiet_for(s, 25);
                },
                vec![],
            ),
            (
                "worker silent past the window in an abandoned run",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 25 * HOUR);
                    quiet_for(s, 25);
                },
                vec![],
            ),
            (
                "worker silent past the window, run still active",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 25 * HOUR);
                    s.run().last_activity_ms = Some(NOW - HOUR);
                },
                vec![(worker_target(), StallReason::WorkerNoProgress, 25 * 60)],
            ),
            (
                "quiet taskless run, abandoned",
                |s| {
                    s.make_taskless();
                    quiet_for(s, 25);
                },
                vec![],
            ),
            (
                "shorter abandon window",
                |s| {
                    s.add_ready("t2", NOW - 3 * HOUR);
                    quiet_for(s, 2);
                    s.abandon_after_ms = HOUR;
                },
                vec![],
            ),
            (
                "no known activity time is not abandoned",
                |s| {
                    s.add_ready("t2", NOW - 30 * HOUR);
                    quiet_for(s, 25);
                    s.run().last_activity_ms = None;
                },
                idle(false, 25 * 60),
            ),
        ]);
    }

    #[test]
    fn superseded_detection() {
        const REBOUND: &str = "term_440";
        let with = |f: fn(&mut FleetData)| {
            let mut d = Scenario::new().data;
            d.terminals.push(terminal(REBOUND));
            f(&mut d);
            d
        };
        let cases: Vec<(&str, FleetData, bool)> = vec![
            ("only run", with(|_| {}), false),
            (
                "newer run, same declared coordinator",
                with(|d| d.runs.push(run("run_2", COORD, NOW - 30 * MIN, NOW))),
                true,
            ),
            (
                "older run, same coordinator",
                with(|d| d.runs.push(run("run_0", COORD, NOW - 90 * MIN, NOW))),
                false,
            ),
            (
                "same creation time",
                with(|d| d.runs.push(run("run_2", COORD, NOW - 60 * MIN, NOW))),
                false,
            ),
            (
                "newer run, other coordinator",
                with(|d| d.runs.push(run("run_2", REBOUND, NOW - 30 * MIN, NOW))),
                false,
            ),
            (
                "newer run whose effective coordinator re-bound to ours",
                with(|d| {
                    d.runs.push(run("run_2", "term_dead", NOW - 30 * MIN, NOW));
                    d.tasks.insert(
                        "run_2".into(),
                        vec![task("t9", "ready", COORD, NOW - 20 * MIN)],
                    );
                }),
                true,
            ),
            (
                "our coordinator re-bound, newer run declares the new terminal",
                with(|d| {
                    d.terminals.retain(|t| t.handle != COORD);
                    d.tasks.get_mut(RUN).unwrap()[0].created_by_terminal_handle =
                        Some(REBOUND.into());
                    d.runs.push(run("run_2", REBOUND, NOW - 30 * MIN, NOW));
                }),
                true,
            ),
            (
                "our coordinator is gone and nobody took over",
                with(|d| {
                    d.terminals.retain(|t| t.handle != COORD);
                    d.tasks.get_mut(RUN).unwrap()[0].created_by_terminal_handle = None;
                    d.runs.push(run("run_2", COORD, NOW - 30 * MIN, NOW));
                }),
                false,
            ),
        ];
        for (name, data, expected) in cases {
            assert_eq!(is_superseded(RUN, &data), expected, "case: {name}");
        }
    }

    #[test]
    fn superseded_runs_are_skipped() {
        const HOUR: i64 = 60 * MIN;
        fn supersede(s: &mut Scenario) {
            s.data
                .runs
                .push(run("run_2", COORD, NOW - 30 * MIN, NOW - MIN));
        }
        fn worker_output(s: &mut Scenario, at: i64) {
            s.data.terminals.retain(|t| t.handle != WORKER);
            s.data.terminals.push(Terminal {
                handle: WORKER.into(),
                last_output_at: Some(Timestamp::Millis(at)),
                ..Default::default()
            });
        }
        let ready_idle = || {
            vec![(
                coord_target(),
                StallReason::CoordinatorIdle {
                    ready_tasks: vec!["t2".into()],
                    finished_tasks: vec![],
                },
                20,
            )]
        };
        run_cases(vec![
            (
                "idle coordinator, not superseded",
                |s| {
                    s.add_ready("t2", NOW - 40 * MIN);
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                },
                ready_idle(),
            ),
            (
                "idle coordinator of a superseded run",
                |s| {
                    s.add_ready("t2", NOW - 40 * MIN);
                    s.run().last_coordinator_activity_ms = Some(NOW - 20 * MIN);
                    supersede(s);
                },
                vec![],
            ),
            (
                "unanswered ask in a superseded run",
                |s| {
                    s.ask("q1", NOW - 20 * MIN);
                    supersede(s);
                },
                vec![],
            ),
            (
                "finished task with no follow-up in a superseded run",
                |s| {
                    s.complete_t1(NOW - 20 * MIN);
                    s.run().last_coordinator_activity_ms = Some(NOW - 30 * MIN);
                    supersede(s);
                },
                vec![],
            ),
            (
                "worker of a superseded run, terminal silent past the window",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 20 * MIN);
                    worker_output(s, NOW - 25 * HOUR);
                    supersede(s);
                },
                vec![],
            ),
            (
                "worker of a superseded run, terminal never printed",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 20 * MIN);
                    supersede(s);
                },
                vec![],
            ),
            (
                "live worker of a superseded run is still nudged",
                |s| {
                    s.t1().last_progress_ms = Some(NOW - 20 * MIN);
                    worker_output(s, NOW - 2 * HOUR);
                    supersede(s);
                },
                vec![(worker_target(), StallReason::WorkerNoProgress, 20)],
            ),
        ]);
    }

    /// The live case that prompted the abandon window: a finished task nobody followed up,
    /// its coordinator's terminal still open but silent for 719 h.
    #[test]
    fn abandoned_run_end_to_end() {
        let quiet = 719 * 60 * MIN;
        let mut data = Scenario::new().data;
        data.runs = vec![run(RUN, COORD, NOW - quiet - 2 * MIN, NOW - quiet)];
        data.terminals = vec![Terminal {
            handle: COORD.into(),
            last_output_at: Some(Timestamp::Millis(NOW - quiet - MIN)),
            ..Default::default()
        }];
        data.workers.clear();
        let t1 = &mut data.tasks.get_mut(RUN).unwrap()[0];
        t1.status = TaskStatus::Completed;
        t1.created_at = Some(Timestamp::Millis(NOW - quiet - 2 * MIN));
        t1.completed_at = Some(Timestamp::Millis(NOW - quiet));
        // A whip nudge sent since does not revive it.
        data.inbox = vec![msg(
            "m1",
            crate::orca::WHIP_SENDER,
            "status",
            NOW - MIN,
            json!({}),
        )];
        let needs_input = BTreeSet::new();
        let ctx = RuleContext::new(&needs_input);
        let report = ProgressTracker::new().update(&data, NOW);
        assert!(is_abandoned(&report.runs[0], &ctx, NOW));
        assert_eq!(evaluate(&report, &data, &ctx, NOW), vec![]);

        // A message in the Run an hour ago makes it live again, and its coordinator is nudged.
        data.inbox
            .push(msg("m2", "term_other", "status", NOW - 60 * MIN, json!({})));
        let report = ProgressTracker::new().update(&data, NOW);
        assert!(!is_abandoned(&report.runs[0], &ctx, NOW));
        assert_eq!(evaluate(&report, &data, &ctx, NOW).len(), 1);
    }

    /// The live case behind output gating: a Run whose record and tasks were last touched a
    /// month ago still has a `ready` task, and its coordinator terminal, reused for unrelated
    /// work, printed output 2 h ago. That output does not keep the Run alive.
    #[test]
    fn reused_coordinator_does_not_revive_an_old_run() {
        let month = 30 * 24 * 60 * MIN;
        let mut data = Scenario::new().data;
        data.runs = vec![run(RUN, COORD, NOW - month, NOW - month)];
        data.terminals = vec![Terminal {
            handle: COORD.into(),
            last_output_at: Some(Timestamp::Millis(NOW - 120 * MIN)),
            ..Default::default()
        }];
        data.workers.clear();
        data.tasks = [(
            RUN.to_string(),
            vec![task("t2", "ready", COORD, NOW - month)],
        )]
        .into();
        let needs_input = BTreeSet::new();
        let ctx = RuleContext::new(&needs_input);
        let report = ProgressTracker::new().update(&data, NOW);
        assert_eq!(report.runs[0].last_activity_ms, Some(NOW - month));
        assert!(is_abandoned(&report.runs[0], &ctx, NOW));
        assert_eq!(evaluate(&report, &data, &ctx, NOW), vec![]);

        // The same Run touched an hour ago is live, and its idle coordinator is nudged.
        data.runs = vec![run(RUN, COORD, NOW - month, NOW - 60 * MIN)];
        let report = ProgressTracker::new().update(&data, NOW);
        assert_eq!(
            report.runs[0].last_coordinator_activity_ms,
            Some(NOW - 120 * MIN)
        );
        assert_eq!(evaluate(&report, &data, &ctx, NOW).len(), 1);
    }

    #[test]
    fn one_finding_per_target() {
        let mut s = Scenario::new();
        s.t1().last_progress_ms = Some(NOW - 20 * MIN);
        s.add_ready("t2", NOW - 60 * MIN);
        s.run().last_coordinator_activity_ms = Some(NOW - 40 * MIN);
        let findings = s.eval();
        // Worker and coordinator are different targets; each gets its own finding.
        assert_eq!(findings.len(), 2);

        // Idle coordinator with an unanswered ask: the ask is more severe, even though the
        // idle finding is longer.
        let mut s = Scenario::new();
        s.add_ready("t2", NOW - 60 * MIN);
        s.run().last_coordinator_activity_ms = Some(NOW - 40 * MIN);
        s.ask("q1", NOW - 20 * MIN);
        let findings = s.eval();
        assert_eq!(
            summary(&findings),
            vec![(
                coord_target(),
                StallReason::UnansweredAsk {
                    message_id: "q1".into()
                },
                20
            )]
        );

        // Same severity: the longest idle wins.
        let a = StallFinding {
            target: coord_target(),
            reason: StallReason::CoordinatorIdle {
                ready_tasks: vec![],
                finished_tasks: vec![],
            },
            idle_for_ms: 10,
            evidence: String::new(),
        };
        let b = StallFinding {
            idle_for_ms: 30,
            ..a.clone()
        };
        assert_eq!(dedupe(vec![a, b.clone()]), vec![b]);
    }

    #[test]
    fn evidence_is_readable() {
        let mut s = Scenario::new();
        s.data.tasks.get_mut(RUN).unwrap()[0].display_name = Some("Add stall rules".into());
        s.t1().last_progress_ms = Some(NOW - 83 * MIN);
        let findings = s.eval();
        assert_eq!(
            findings[0].evidence,
            "task t1 (Add stall rules) dispatched, no progress for 1h23m \
             (last signal: heartbeat, last phase implementing)"
        );
    }

    #[test]
    fn effective_coordinator_cases() {
        let rebound = "term_440";
        let base = || {
            let mut s = Scenario::new();
            s.data.terminals = vec![terminal(WORKER), terminal(rebound), terminal("term_x")];
            s.data
        };
        let cases: Vec<(&str, FleetData, Option<&str>)> = vec![
            ("declared live", Scenario::new().data, Some(COORD)),
            (
                "declared dead, no other evidence",
                {
                    let mut d = base();
                    d.tasks.get_mut(RUN).unwrap()[0].created_by_terminal_handle = None;
                    d
                },
                None,
            ),
            (
                "declared dead, task created by live terminal",
                {
                    let mut d = base();
                    d.tasks.get_mut(RUN).unwrap()[0].created_by_terminal_handle =
                        Some(rebound.into());
                    d
                },
                Some(rebound),
            ),
            (
                "most recent live task creator",
                {
                    let mut d = base();
                    let tasks = d.tasks.get_mut(RUN).unwrap();
                    tasks[0].created_by_terminal_handle = Some(rebound.into());
                    tasks.push(task("t2", "ready", "term_x", NOW - 70 * MIN));
                    tasks.push(task("t3", "ready", "term_dead", NOW - MIN));
                    d
                },
                Some(rebound),
            ),
            (
                "worker-created task is skipped, falls back to messages",
                {
                    let mut d = base();
                    d.tasks.get_mut(RUN).unwrap()[0].created_by_terminal_handle =
                        Some(WORKER.into());
                    d.inbox = vec![
                        msg("m1", WORKER, "heartbeat", NOW - MIN, json!({})),
                        msg("m2", "term_x", "status", NOW - 30 * MIN, json!({})),
                        msg("m3", rebound, "dispatch", NOW - 10 * MIN, json!({})),
                        msg("m4", &format!("run:{RUN}"), "status", NOW, json!({})),
                        msg("m5", "dispatch:ctx_t1", "question", NOW, json!({})),
                    ];
                    d
                },
                Some(rebound),
            ),
            (
                "messages for other runs are ignored",
                {
                    let mut d = base();
                    d.tasks.get_mut(RUN).unwrap()[0].created_by_terminal_handle = None;
                    let mut other = msg("m1", rebound, "status", NOW, json!({}));
                    other.run_id = Some("run_other".into());
                    other.to_handle = Some("run:run_other".into());
                    d.inbox = vec![other];
                    d
                },
                None,
            ),
        ];
        for (name, data, expected) in cases {
            assert_eq!(
                effective_coordinator(RUN, &data).as_deref(),
                expected,
                "case: {name}"
            );
        }
    }

    #[test]
    fn rebound_coordinator_end_to_end() {
        // The declared coordinator's terminal is gone; term_440 creates tasks and messages
        // the Run. A ready task is waiting and term_440 has been quiet for 20 min.
        let rebound = "term_440";
        let mut data = Scenario::new().data;
        data.terminals = vec![
            terminal(WORKER),
            Terminal {
                handle: rebound.into(),
                last_output_at: Some(Timestamp::Millis(NOW - 20 * MIN)),
                ..Default::default()
            },
        ];
        data.tasks = [(
            RUN.to_string(),
            vec![task("t2", "ready", rebound, NOW - 40 * MIN)],
        )]
        .into();
        data.workers.clear();
        data.inbox = vec![msg("m1", rebound, "dispatch", NOW - 25 * MIN, json!({}))];

        let report = ProgressTracker::new().update(&data, NOW);
        assert_eq!(report.runs[0].coordinator_handle.as_deref(), Some(rebound));
        assert_eq!(
            report.runs[0].last_coordinator_activity_ms,
            Some(NOW - 20 * MIN)
        );
        let needs_input = BTreeSet::new();
        let findings = evaluate(&report, &data, &RuleContext::new(&needs_input), NOW);
        assert_eq!(
            summary(&findings),
            vec![(
                StallTarget::Coordinator {
                    handle: rebound.into(),
                    run_id: RUN.into()
                },
                StallReason::CoordinatorIdle {
                    ready_tasks: vec!["t2".into()],
                    finished_tasks: vec![]
                },
                20
            )]
        );
    }

    #[test]
    fn fmt_duration_forms() {
        assert_eq!(fmt_duration(45_000), "45s");
        assert_eq!(fmt_duration(23 * MIN), "23m");
        assert_eq!(fmt_duration(125 * MIN), "2h05m");
        assert_eq!(fmt_duration(-5), "0s");
    }
}

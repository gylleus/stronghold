//! Computes the last progress time per task from bus messages, status and output.
//!
//! [`ProgressTracker::update`] turns one poll's [`FleetData`] into facts for the whip's stall
//! rules: when each task last showed progress and why, whether its worker is blocked on an
//! unanswered question, and when each Run's coordinator last did something. It decides nothing
//! itself. See `docs/heartbeat-convention.md` for what counts as progress.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::fleet::FleetData;
use crate::orca::{DispatchStatus, InboxMessage, MessageType, Run, Task, TaskStatus, Worker};

/// Payload key Orca adds to a message it rejected (e.g. a heartbeat after `worker_done`).
const REJECTION_KEY: &str = "_orcaLifecycleRejection";
/// Prefix of a coordinator's deliberate-idle `status` message.
const HOLDING_PREFIX: &str = "holding:";

/// Which signal produced a progress time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressSource {
    Heartbeat {
        phase: Option<String>,
    },
    /// Any other bus message, e.g. `status`, `question`, `worker_done`.
    Message {
        kind: MessageType,
    },
    StatusChange,
    TerminalOutput,
    None,
}

/// Progress facts for one task.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskProgress {
    pub task_id: String,
    pub run_id: String,
    pub status: TaskStatus,
    /// Terminal of the worker that owns the task (latest dispatch).
    pub owner_handle: Option<String>,
    pub dispatch_id: Option<String>,
    /// Newest progress signal, in ms since the Unix epoch.
    pub last_progress_ms: Option<i64>,
    pub source: ProgressSource,
    /// Phase of the owner's newest accepted heartbeat for this task.
    pub phase: Option<String>,
    /// The owner asked a question on the current dispatch that nobody has answered yet, so a
    /// silent worker is the coordinator's problem, not the worker's.
    pub waiting_on_ask: bool,
}

/// Activity facts for one Run's coordinator.
#[derive(Debug, Clone, PartialEq)]
pub struct RunProgress {
    pub run_id: String,
    pub coordinator_handle: Option<String>,
    pub last_coordinator_activity_ms: Option<i64>,
    pub source: ProgressSource,
    /// `ready` tasks nobody has started.
    pub open_ready_tasks: Vec<String>,
    /// Ids of unanswered `question` messages from this Run's dispatched workers.
    pub unanswered_asks: Vec<String>,
    /// Reason from the coordinator's `holding: <reason>` status, when that is its newest message.
    pub holding: Option<String>,
}

/// Output of one [`ProgressTracker::update`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressReport {
    pub tasks: Vec<TaskProgress>,
    pub runs: Vec<RunProgress>,
}

/// A task's status and when it entered it.
#[derive(Debug, Clone, PartialEq)]
struct SeenStatus {
    status: TaskStatus,
    /// When the status was first observed, or the task's own timestamp on first sight; `None`
    /// when first seen without one.
    since_ms: Option<i64>,
}

/// Remembers task statuses across polls, since Orca exposes no status-change time.
#[derive(Debug, Default)]
pub struct ProgressTracker {
    seen: HashMap<String, SeenStatus>,
}

impl ProgressTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records status transitions in `data` (observed at `now_ms`) and computes the report.
    pub fn update(&mut self, data: &FleetData, now_ms: i64) -> ProgressReport {
        let mut present = HashSet::new();
        for task in data.tasks.values().flatten() {
            present.insert(task.id.clone());
            match self.seen.get_mut(&task.id) {
                Some(seen) if seen.status == task.status => {}
                Some(seen) => {
                    *seen = SeenStatus {
                        status: task.status.clone(),
                        since_ms: Some(now_ms),
                    }
                }
                None => {
                    self.seen.insert(
                        task.id.clone(),
                        SeenStatus {
                            status: task.status.clone(),
                            since_ms: initial_status_time(task),
                        },
                    );
                }
            }
        }
        self.seen.retain(|id, _| present.contains(id));

        let bus = Bus::new(data);
        let mut tasks = Vec::new();
        for (run_id, run_tasks) in &data.tasks {
            for task in run_tasks {
                let since = self.seen.get(&task.id).and_then(|s| s.since_ms);
                tasks.push(task_progress(data, &bus, run_id, task, since));
            }
        }

        let mut run_ids: Vec<&str> = data.runs.iter().map(|r| r.id.as_str()).collect();
        for run_id in data.tasks.keys() {
            if !run_ids.contains(&run_id.as_str()) {
                run_ids.push(run_id);
            }
        }
        let runs = run_ids
            .into_iter()
            .map(|run_id| self.run_progress(data, &bus, run_id, &tasks))
            .collect();

        ProgressReport { tasks, runs }
    }

    fn run_progress(
        &self,
        data: &FleetData,
        bus: &Bus<'_>,
        run_id: &str,
        tasks: &[TaskProgress],
    ) -> RunProgress {
        let coordinator = data
            .run(run_id)
            .and_then(|r: &Run| r.coordinator_handle.clone());
        let run_home = format!("run:{run_id}");
        let run_tasks = data.tasks.get(run_id).map(Vec::as_slice).unwrap_or(&[]);
        let mut latest = Latest::default();

        // Answers to asks are sent from the Run home (`run:<id>`), not the coordinator terminal.
        let from_coordinator = |m: &InboxMessage| {
            m.from_handle.as_deref() == Some(&run_home)
                || (coordinator.is_some() && m.from_handle == coordinator)
        };
        let coordinator_msgs: Vec<&InboxMessage> = data
            .inbox
            .iter()
            .filter(|m| from_coordinator(m) && !is_rejected(m))
            .collect();
        for msg in &coordinator_msgs {
            latest.offer(msg_ms(msg), message_source(msg));
        }

        // Tasks are created by the coordinator; status changes other than Orca settling a
        // `worker_done` into `completed` are the coordinator acting on the board.
        let worker_handles: HashSet<&str> = data
            .workers
            .iter()
            .filter_map(|w| w.agent_terminal_handle.as_deref())
            .collect();
        for task in run_tasks {
            let by_worker = task
                .created_by_terminal_handle
                .as_deref()
                .is_some_and(|h| worker_handles.contains(h) && Some(h) != coordinator.as_deref());
            if !by_worker {
                latest.offer(ts_ms(&task.created_at), ProgressSource::StatusChange);
            }
            if task.status != TaskStatus::Completed {
                let since = self.seen.get(&task.id).and_then(|s| s.since_ms);
                latest.offer(since, ProgressSource::StatusChange);
            }
        }

        if let Some(term) = coordinator.as_deref().and_then(|h| data.terminal(h)) {
            latest.offer(ts_ms(&term.last_output_at), ProgressSource::TerminalOutput);
        }

        let holding = coordinator_msgs
            .iter()
            .max_by_key(|m| msg_order(m))
            .filter(|m| m.message_type == Some(MessageType::Status))
            .and_then(|m| holding_reason(m));

        let open_ready_tasks = run_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Ready)
            .map(|t| t.id.clone())
            .collect();

        let unanswered_asks = tasks
            .iter()
            .filter(|t| t.run_id == run_id && t.waiting_on_ask)
            .flat_map(|t| {
                let task = run_tasks.iter().find(|task| task.id == t.task_id);
                task.map(|task| bus.open_asks(data, task, t))
                    .unwrap_or_default()
            })
            .map(|m| m.id.clone())
            .collect();

        RunProgress {
            run_id: run_id.to_string(),
            coordinator_handle: coordinator,
            last_coordinator_activity_ms: latest.ms,
            source: latest.source,
            open_ready_tasks,
            unanswered_asks,
            holding,
        }
    }
}

/// On first sight a task's status is at least as old as its completion (finished tasks) or
/// its creation (everything else).
fn initial_status_time(task: &Task) -> Option<i64> {
    let finished = matches!(task.status, TaskStatus::Completed | TaskStatus::Failed);
    let completed = if finished {
        ts_ms(&task.completed_at)
    } else {
        None
    };
    completed.or_else(|| ts_ms(&task.created_at))
}

/// Accepted bus messages indexed for per-task lookups.
struct Bus<'a> {
    /// Messages whose payload names a task, keyed by task id.
    by_task: HashMap<String, Vec<(&'a InboxMessage, Payload)>>,
    /// Earliest accepted `worker_done` time per dispatch id.
    done_at: HashMap<String, i64>,
    all: &'a [InboxMessage],
}

/// The payload fields progress needs.
struct Payload {
    dispatch_id: Option<String>,
    phase: Option<String>,
}

impl<'a> Bus<'a> {
    fn new(data: &'a FleetData) -> Self {
        let mut by_task: HashMap<String, Vec<_>> = HashMap::new();
        let mut done_at: HashMap<String, i64> = HashMap::new();
        for msg in data.inbox.iter().filter(|m| !is_rejected(m)) {
            let Some(payload) = msg.parsed_payload() else {
                continue;
            };
            if msg.message_type == Some(MessageType::WorkerDone)
                && let (Some(d), Some(ms)) = (&payload.dispatch_id, msg_ms(msg))
            {
                let at = done_at.entry(d.clone()).or_insert(ms);
                *at = (*at).min(ms);
            }
            if let Some(task_id) = payload.task_id {
                by_task.entry(task_id).or_default().push((
                    msg,
                    Payload {
                        dispatch_id: payload.dispatch_id,
                        phase: payload.phase,
                    },
                ));
            }
        }
        Self {
            by_task,
            done_at,
            all: &data.inbox,
        }
    }

    /// Accepted messages about `task` sent by its owner, either as its terminal or as its
    /// dispatch (`ask` questions arrive `from: dispatch:<id>`).
    fn owner_messages(
        &self,
        task: &Task,
        progress: &TaskProgress,
    ) -> impl Iterator<Item = (&'a InboxMessage, &Payload)> {
        let owner = progress.owner_handle.clone();
        let from_dispatch = progress
            .dispatch_id
            .as_ref()
            .map(|d| format!("dispatch:{d}"));
        self.by_task
            .get(&task.id)
            .into_iter()
            .flatten()
            .filter(move |(m, _)| {
                let from = m.from_handle.as_deref();
                from.is_some() && (from == owner.as_deref() || from == from_dispatch.as_deref())
            })
            .map(|(m, p)| (*m, p))
    }

    /// Time the dispatch was settled, after which Orca no longer accepts its heartbeats.
    fn dispatch_done_at(&self, dispatch_id: &str, data: &FleetData, task: &Task) -> Option<i64> {
        if let Some(ms) = self.done_at.get(dispatch_id) {
            return Some(*ms);
        }
        let settled = data.workers.iter().any(|w| {
            w.dispatch_id == dispatch_id
                && w.dispatch_status
                    .as_ref()
                    .is_some_and(|s| *s != DispatchStatus::Dispatched)
        });
        if settled {
            ts_ms(&task.completed_at)
        } else {
            None
        }
    }

    /// The owner's questions on the current dispatch of a dispatched task with no answer yet.
    fn open_asks(
        &self,
        data: &FleetData,
        task: &Task,
        progress: &TaskProgress,
    ) -> Vec<&'a InboxMessage> {
        if task.status != TaskStatus::Dispatched {
            return Vec::new();
        }
        self.owner_messages(task, progress)
            .filter(|(m, p)| {
                m.message_type == Some(MessageType::Question)
                    && (p.dispatch_id.is_none() || p.dispatch_id == progress.dispatch_id)
            })
            .map(|(m, _)| m)
            .filter(|q| !self.answered(data, q, progress))
            .collect()
    }

    /// A later message from someone else in the question's thread, or sent straight to the
    /// asker by the coordinator or Run home.
    fn answered(&self, data: &FleetData, q: &InboxMessage, progress: &TaskProgress) -> bool {
        let thread = q.thread_id.as_deref().unwrap_or(&q.id);
        let run_home = q.run_id.as_ref().map(|r| format!("run:{r}"));
        let coordinator = q
            .run_id
            .as_deref()
            .and_then(|r| data.run(r))
            .and_then(|r| r.coordinator_handle.clone());
        let asker_addresses = [
            progress.owner_handle.clone(),
            progress
                .dispatch_id
                .as_ref()
                .map(|d| format!("dispatch:{d}")),
        ];
        self.all.iter().any(|m| {
            if m.id == q.id || m.from_handle == q.from_handle || !is_after(m, q) {
                return false;
            }
            let in_thread = m.thread_id.as_deref() == Some(thread);
            let from_coordinator = m.from_handle.is_some()
                && (m.from_handle == run_home || m.from_handle == coordinator);
            let to_asker = m.to_handle.is_some() && asker_addresses.contains(&m.to_handle);
            in_thread || (from_coordinator && to_asker)
        })
    }
}

fn task_progress(
    data: &FleetData,
    bus: &Bus<'_>,
    run_id: &str,
    task: &Task,
    status_since: Option<i64>,
) -> TaskProgress {
    let worker = owning_worker(data, task);
    let mut progress = TaskProgress {
        task_id: task.id.clone(),
        run_id: run_id.to_string(),
        status: task.status.clone(),
        owner_handle: worker
            .and_then(|w| w.agent_terminal_handle.clone())
            .or_else(|| task.assignee_handle.clone()),
        dispatch_id: worker
            .map(|w| w.dispatch_id.clone())
            .filter(|d| !d.is_empty())
            .or_else(|| task.dispatch_id.clone()),
        last_progress_ms: None,
        source: ProgressSource::None,
        phase: None,
        waiting_on_ask: false,
    };

    let mut latest = Latest::default();
    let mut newest_heartbeat: Option<(i64, Option<String>)> = None;
    for (msg, payload) in bus.owner_messages(task, &progress) {
        let Some(ms) = msg_ms(msg) else { continue };
        if msg.message_type == Some(MessageType::Heartbeat) {
            let done = payload
                .dispatch_id
                .as_deref()
                .and_then(|d| bus.dispatch_done_at(d, data, task));
            if done.is_some_and(|done| ms > done) {
                continue;
            }
            if newest_heartbeat.as_ref().is_none_or(|(t, _)| ms >= *t) {
                newest_heartbeat = Some((ms, payload.phase.clone()));
            }
            latest.offer(
                Some(ms),
                ProgressSource::Heartbeat {
                    phase: payload.phase.clone(),
                },
            );
        } else {
            latest.offer(Some(ms), message_source(msg));
        }
    }
    latest.offer(status_since, ProgressSource::StatusChange);

    // The owner's output only speaks for this task while the task holds its terminal.
    if task.status == TaskStatus::Dispatched
        && let Some(term) = progress
            .owner_handle
            .as_deref()
            .and_then(|h| data.terminal(h))
    {
        latest.offer(ts_ms(&term.last_output_at), ProgressSource::TerminalOutput);
    }

    progress.last_progress_ms = latest.ms;
    progress.source = latest.source;
    progress.phase = newest_heartbeat.and_then(|(_, phase)| phase);
    progress.waiting_on_ask = !bus.open_asks(data, task, &progress).is_empty();
    progress
}

/// The task's worker: the live dispatch if any, else the task's recorded dispatch, else the
/// last listed one.
fn owning_worker<'a>(data: &'a FleetData, task: &Task) -> Option<&'a Worker> {
    let workers: Vec<&Worker> = data
        .workers
        .iter()
        .filter(|w| w.task_id.as_deref() == Some(&task.id))
        .collect();
    workers
        .iter()
        .find(|w| w.dispatch_status == Some(DispatchStatus::Dispatched))
        .or_else(|| {
            workers
                .iter()
                .find(|w| task.dispatch_id.as_deref() == Some(&w.dispatch_id))
        })
        .or(workers.last())
        .copied()
}

/// Newest signal seen so far; on a tie the first offered wins.
#[derive(Debug)]
struct Latest {
    ms: Option<i64>,
    source: ProgressSource,
}

impl Default for Latest {
    fn default() -> Self {
        Self {
            ms: None,
            source: ProgressSource::None,
        }
    }
}

impl Latest {
    fn offer(&mut self, ms: Option<i64>, source: ProgressSource) {
        if let Some(ms) = ms
            && self.ms.is_none_or(|cur| ms > cur)
        {
            self.ms = Some(ms);
            self.source = source;
        }
    }
}

fn message_source(msg: &InboxMessage) -> ProgressSource {
    ProgressSource::Message {
        kind: msg
            .message_type
            .clone()
            .unwrap_or_else(|| MessageType::Other(String::new())),
    }
}

/// Orca echoes a message it refused back to the Run with a rejection marker in the payload.
fn is_rejected(msg: &InboxMessage) -> bool {
    let marked = msg
        .payload
        .as_deref()
        .and_then(|p| serde_json::from_str::<Value>(p).ok())
        .is_some_and(|v| v.get(REJECTION_KEY).is_some());
    marked
        || msg
            .subject
            .as_deref()
            .is_some_and(|s| s.starts_with("Rejected "))
}

fn holding_reason(msg: &InboxMessage) -> Option<String> {
    [msg.subject.as_deref(), msg.body.as_deref()]
        .into_iter()
        .flatten()
        .find_map(|text| {
            let text = text.trim_start();
            let head = text.get(..HOLDING_PREFIX.len())?;
            head.eq_ignore_ascii_case(HOLDING_PREFIX)
                .then(|| text[HOLDING_PREFIX.len()..].trim().to_string())
        })
}

fn ts_ms(ts: &Option<crate::orca::Timestamp>) -> Option<i64> {
    ts.as_ref().and_then(|t| t.epoch_ms())
}

fn msg_ms(msg: &InboxMessage) -> Option<i64> {
    ts_ms(&msg.created_at)
}

/// Bus order: creation time, then Orca's sequence for same-second messages.
fn msg_order(msg: &InboxMessage) -> (i64, u64) {
    (msg_ms(msg).unwrap_or(i64::MIN), msg.sequence.unwrap_or(0))
}

fn is_after(m: &InboxMessage, q: &InboxMessage) -> bool {
    match (m.sequence, q.sequence) {
        (Some(a), Some(b)) => a > b,
        _ => msg_order(m) >= msg_order(q),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::commands::{parse_inbox, parse_run_list, parse_task_list, parse_worker_list};
    use crate::orca::{Terminal, Timestamp};
    use serde_json::json;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!("../../tests/fixtures/orca/", $name))
        };
    }

    const RUN: &str = "run_1";
    const WORKER: &str = "term_worker";
    const COORD: &str = "term_coord";
    const T0: i64 = 1_790_000_000_000;
    const MIN: i64 = 60_000;

    fn task(id: &str, status: &str) -> Task {
        serde_json::from_value(json!({
            "id": id, "run_id": RUN, "status": status,
            "dispatch_id": "ctx_1", "assignee_handle": WORKER,
            "created_by_terminal_handle": COORD,
            "created_at": Timestamp::Millis(T0),
        }))
        .unwrap()
    }

    fn worker(task_id: &str, dispatch: &str, status: &str) -> Worker {
        serde_json::from_value(json!({
            "dispatchId": dispatch, "taskId": task_id, "runId": RUN,
            "dispatchStatus": status, "agentTerminalHandle": WORKER,
        }))
        .unwrap()
    }

    fn msg(id: &str, from: &str, kind: &str, at: i64, payload: Value) -> InboxMessage {
        serde_json::from_value(json!({
            "id": id, "run_id": RUN, "from_handle": from, "to_handle": format!("run:{RUN}"),
            "type": kind, "subject": kind, "payload": payload.to_string(),
            "created_at": Timestamp::Millis(at),
        }))
        .unwrap()
    }

    fn heartbeat(id: &str, at: i64, phase: &str) -> InboxMessage {
        msg(
            id,
            WORKER,
            "heartbeat",
            at,
            json!({"taskId": "t1", "dispatchId": "ctx_1", "phase": phase}),
        )
    }

    fn terminal(handle: &str, last_output: i64) -> Terminal {
        Terminal {
            handle: handle.into(),
            last_output_at: Some(Timestamp::Millis(last_output)),
            ..Default::default()
        }
    }

    fn fleet(tasks: Vec<Task>, inbox: Vec<InboxMessage>) -> FleetData {
        let run: Run =
            serde_json::from_value(json!({"id": RUN, "coordinator_handle": COORD})).unwrap();
        FleetData {
            runs: vec![run],
            workers: vec![worker("t1", "ctx_1", "dispatched")],
            tasks: [(RUN.to_string(), tasks)].into(),
            inbox,
            ..Default::default()
        }
    }

    fn only_task(report: &ProgressReport) -> &TaskProgress {
        assert_eq!(report.tasks.len(), 1);
        &report.tasks[0]
    }

    #[test]
    fn heartbeat_wins_over_older_output() {
        let mut data = fleet(
            vec![task("t1", "dispatched")],
            vec![heartbeat("m1", T0 + 10 * MIN, "implementing")],
        );
        data.terminals = vec![terminal(WORKER, T0 + 5 * MIN)];
        let report = ProgressTracker::new().update(&data, T0 + 20 * MIN);
        let t = only_task(&report);
        assert_eq!(t.owner_handle.as_deref(), Some(WORKER));
        assert_eq!(t.dispatch_id.as_deref(), Some("ctx_1"));
        assert_eq!(t.last_progress_ms, Some(T0 + 10 * MIN));
        assert_eq!(
            t.source,
            ProgressSource::Heartbeat {
                phase: Some("implementing".into())
            }
        );
        assert_eq!(t.phase.as_deref(), Some("implementing"));
        assert!(!t.waiting_on_ask);
    }

    #[test]
    fn newer_output_wins_and_is_the_fallback() {
        let mut data = fleet(vec![task("t1", "dispatched")], vec![]);
        data.terminals = vec![terminal(WORKER, T0 + 7 * MIN)];
        let report = ProgressTracker::new().update(&data, T0 + 20 * MIN);
        let t = only_task(&report);
        assert_eq!(t.last_progress_ms, Some(T0 + 7 * MIN));
        assert_eq!(t.source, ProgressSource::TerminalOutput);
        assert_eq!(t.phase, None);

        data.inbox = vec![heartbeat("m1", T0 + 3 * MIN, "reviewing")];
        let report = ProgressTracker::new().update(&data, T0 + 20 * MIN);
        let t = only_task(&report);
        assert_eq!(t.source, ProgressSource::TerminalOutput);
        assert_eq!(t.phase.as_deref(), Some("reviewing"));
    }

    #[test]
    fn other_messages_about_the_task_count() {
        let status = msg(
            "m1",
            WORKER,
            "merge_ready",
            T0 + 4 * MIN,
            json!({"taskId": "t1"}),
        );
        let other_task = msg(
            "m2",
            WORKER,
            "status",
            T0 + 9 * MIN,
            json!({"taskId": "t9"}),
        );
        let not_owner = msg(
            "m3",
            "term_other",
            "status",
            T0 + 9 * MIN,
            json!({"taskId": "t1"}),
        );
        let data = fleet(
            vec![task("t1", "dispatched")],
            vec![other_task, not_owner, status],
        );
        let report = ProgressTracker::new().update(&data, T0 + 20 * MIN);
        let t = only_task(&report);
        assert_eq!(t.last_progress_ms, Some(T0 + 4 * MIN));
        assert_eq!(
            t.source,
            ProgressSource::Message {
                kind: MessageType::MergeReady
            }
        );
    }

    #[test]
    fn status_transition_detected_on_second_update() {
        let mut tracker = ProgressTracker::new();
        let mut data = fleet(vec![task("t1", "ready")], vec![]);
        let report = tracker.update(&data, T0 + 5 * MIN);
        let t = only_task(&report);
        assert_eq!(t.last_progress_ms, Some(T0));
        assert_eq!(t.source, ProgressSource::StatusChange);

        // Unchanged status keeps its time.
        let report = tracker.update(&data, T0 + 6 * MIN);
        assert_eq!(only_task(&report).last_progress_ms, Some(T0));

        data.tasks.get_mut(RUN).unwrap()[0].status = TaskStatus::Dispatched;
        let report = tracker.update(&data, T0 + 7 * MIN);
        let t = only_task(&report);
        assert_eq!(t.last_progress_ms, Some(T0 + 7 * MIN));
        assert_eq!(t.source, ProgressSource::StatusChange);

        let report = tracker.update(&data, T0 + 30 * MIN);
        assert_eq!(only_task(&report).last_progress_ms, Some(T0 + 7 * MIN));
    }

    #[test]
    fn first_sight_of_finished_task_uses_completed_at() {
        let mut t = task("t1", "completed");
        t.completed_at = Some(Timestamp::Text("2026-09-23 13:00:56".into()));
        let data = fleet(vec![t], vec![]);
        let report = ProgressTracker::new().update(&data, T0 * 2);
        assert_eq!(only_task(&report).last_progress_ms, Some(1_790_168_456_000));
    }

    #[test]
    fn post_completion_heartbeat_ignored() {
        let done = msg(
            "m2",
            WORKER,
            "worker_done",
            T0 + 10 * MIN,
            json!({"taskId": "t1", "dispatchId": "ctx_1", "outcome": "succeeded"}),
        );
        // Accepted by Orca but after worker_done, and one Orca echoed back as rejected.
        let late = heartbeat("m3", T0 + 12 * MIN, "implementing");
        let mut rejected = heartbeat("m4", T0 + 14 * MIN, "implementing");
        rejected.subject = Some("Rejected heartbeat: alive".into());
        rejected.payload = Some(
            json!({"taskId": "t1", "dispatchId": "ctx_1", "phase": "implementing",
                   "_orcaLifecycleRejection": {"code": "dispatch_capability_invalid"}})
            .to_string(),
        );
        let early = heartbeat("m1", T0 + 2 * MIN, "reviewing");
        let mut data = fleet(
            vec![task("t1", "dispatched")],
            vec![rejected, late, done, early],
        );
        data.workers = vec![worker("t1", "ctx_1", "completed")];
        let report = ProgressTracker::new().update(&data, T0 + 20 * MIN);
        let t = only_task(&report);
        assert_eq!(t.last_progress_ms, Some(T0 + 10 * MIN));
        assert_eq!(
            t.source,
            ProgressSource::Message {
                kind: MessageType::WorkerDone
            }
        );
        assert_eq!(t.phase.as_deref(), Some("reviewing"));
    }

    #[test]
    fn post_completion_heartbeat_ignored_via_completed_at() {
        let mut t = task("t1", "completed");
        t.completed_at = Some(Timestamp::Millis(T0 + 10 * MIN));
        let mut data = fleet(vec![t], vec![heartbeat("m1", T0 + 12 * MIN, "reviewing")]);
        data.workers = vec![worker("t1", "ctx_1", "completed")];
        let report = ProgressTracker::new().update(&data, T0 + 20 * MIN);
        let t = only_task(&report);
        assert_eq!(t.last_progress_ms, Some(T0 + 10 * MIN));
        assert_eq!(t.source, ProgressSource::StatusChange);
    }

    fn question(id: &str, at: i64) -> InboxMessage {
        let mut q = msg(
            id,
            "dispatch:ctx_1",
            "question",
            at,
            json!({"taskId": "t1", "dispatchId": "ctx_1", "question": "go?"}),
        );
        q.thread_id = Some(id.into());
        q
    }

    #[test]
    fn waiting_on_unanswered_ask() {
        let data = fleet(
            vec![task("t1", "dispatched")],
            vec![question("q1", T0 + 3 * MIN)],
        );
        let report = ProgressTracker::new().update(&data, T0 + 30 * MIN);
        let t = only_task(&report);
        assert!(t.waiting_on_ask);
        assert_eq!(t.last_progress_ms, Some(T0 + 3 * MIN));
        assert_eq!(
            t.source,
            ProgressSource::Message {
                kind: MessageType::Question
            }
        );
        assert_eq!(report.runs[0].unanswered_asks, vec!["q1"]);
    }

    #[test]
    fn answered_ask_is_not_waiting() {
        let mut reply = msg(
            "r1",
            &format!("run:{RUN}"),
            "status",
            T0 + 4 * MIN,
            json!({}),
        );
        reply.thread_id = Some("q1".into());
        reply.to_handle = Some("dispatch:ctx_1".into());
        let data = fleet(
            vec![task("t1", "dispatched")],
            vec![reply, question("q1", T0 + 3 * MIN)],
        );
        let report = ProgressTracker::new().update(&data, T0 + 30 * MIN);
        assert!(!only_task(&report).waiting_on_ask);
        assert!(report.runs[0].unanswered_asks.is_empty());

        // A direct message from the coordinator to the asker also answers it.
        let mut direct = msg("r2", COORD, "status", T0 + 4 * MIN, json!({}));
        direct.to_handle = Some(WORKER.into());
        let data = fleet(
            vec![task("t1", "dispatched")],
            vec![direct, question("q1", T0 + 3 * MIN)],
        );
        let report = ProgressTracker::new().update(&data, T0 + 30 * MIN);
        assert!(!only_task(&report).waiting_on_ask);
    }

    #[test]
    fn ask_on_finished_task_is_not_waiting() {
        let data = fleet(
            vec![task("t1", "completed")],
            vec![question("q1", T0 + 3 * MIN)],
        );
        let report = ProgressTracker::new().update(&data, T0 + 30 * MIN);
        assert!(!only_task(&report).waiting_on_ask);
    }

    #[test]
    fn real_ask_and_reply_fixture() {
        let inbox = parse_inbox(fixture!("inbox_asks.json")).unwrap();
        let task_for = |id: &str, dispatch: &str| -> Task {
            serde_json::from_value(json!({
                "id": id, "run_id": "run_7c25cdcf46e0", "status": "dispatched",
                "dispatch_id": dispatch, "created_at": "2026-09-24 07:00:00",
            }))
            .unwrap()
        };
        let w = |task: &str, dispatch: &str, handle: &str| -> Worker {
            serde_json::from_value(json!({
                "dispatchId": dispatch, "taskId": task, "runId": "run_7c25cdcf46e0",
                "dispatchStatus": "dispatched", "agentTerminalHandle": handle,
            }))
            .unwrap()
        };
        let data = FleetData {
            workers: vec![
                w("task_6bd9c2a18900", "ctx_be46072c6286", "term_a"),
                w("task_40a5deb4db5d", "ctx_6784d2c7c310", "term_b"),
                w(
                    "task_a2aa098cf934",
                    "ctx_f86ca27f0183",
                    "term_6169a9ce-59b8-4454-b652-af7da25a563b",
                ),
            ],
            tasks: [(
                "run_7c25cdcf46e0".to_string(),
                vec![
                    task_for("task_6bd9c2a18900", "ctx_be46072c6286"),
                    task_for("task_40a5deb4db5d", "ctx_6784d2c7c310"),
                    task_for("task_a2aa098cf934", "ctx_f86ca27f0183"),
                ],
            )]
            .into(),
            inbox,
            ..Default::default()
        };
        let report = ProgressTracker::new().update(&data, T0 * 2);
        let by_id = |id: &str| report.tasks.iter().find(|t| t.task_id == id).unwrap();

        let answered = by_id("task_6bd9c2a18900");
        assert!(!answered.waiting_on_ask);
        assert_eq!(
            answered.last_progress_ms,
            parse_timestamp("2026-09-25T09:49:04Z")
        );

        assert!(by_id("task_40a5deb4db5d").waiting_on_ask);
        assert_eq!(report.runs[0].unanswered_asks, vec!["msg_3a6b210dd525"]);

        // Only a rejected heartbeat for this one: no message progress.
        let rejected = by_id("task_a2aa098cf934");
        assert_eq!(rejected.source, ProgressSource::StatusChange);
        assert_eq!(rejected.phase, None);

        // Replies come from the Run home, which counts as coordinator activity.
        assert_eq!(
            report.runs[0].last_coordinator_activity_ms,
            parse_timestamp("2026-09-25T09:49:34Z")
        );
    }

    fn parse_timestamp(s: &str) -> Option<i64> {
        crate::orca::types::parse_timestamp_ms(s)
    }

    #[test]
    fn coordinator_activity_sources() {
        let mut data = fleet(vec![task("t1", "dispatched"), task("t2", "ready")], vec![]);
        data.terminals = vec![terminal(COORD, T0 + 2 * MIN)];
        let mut tracker = ProgressTracker::new();
        let report = tracker.update(&data, T0 + 5 * MIN);
        let run = &report.runs[0];
        assert_eq!(run.coordinator_handle.as_deref(), Some(COORD));
        assert_eq!(run.last_coordinator_activity_ms, Some(T0 + 2 * MIN));
        assert_eq!(run.source, ProgressSource::TerminalOutput);
        assert_eq!(run.open_ready_tasks, vec!["t2"]);
        assert_eq!(run.holding, None);

        // Coordinator dispatches t2: a transition it made.
        data.tasks.get_mut(RUN).unwrap()[1].status = TaskStatus::Dispatched;
        let report = tracker.update(&data, T0 + 8 * MIN);
        let run = &report.runs[0];
        assert_eq!(run.last_coordinator_activity_ms, Some(T0 + 8 * MIN));
        assert_eq!(run.source, ProgressSource::StatusChange);
        assert!(run.open_ready_tasks.is_empty());

        // Orca settling a worker_done into `completed` is not coordinator activity.
        data.tasks.get_mut(RUN).unwrap()[0].status = TaskStatus::Completed;
        let report = tracker.update(&data, T0 + 9 * MIN);
        assert_eq!(
            report.runs[0].last_coordinator_activity_ms,
            Some(T0 + 8 * MIN)
        );

        // Bus messages from the coordinator count, whatever they are about.
        data.inbox = vec![msg("m1", COORD, "dispatch", T0 + 10 * MIN, json!({}))];
        let report = tracker.update(&data, T0 + 11 * MIN);
        let run = &report.runs[0];
        assert_eq!(run.last_coordinator_activity_ms, Some(T0 + 10 * MIN));
        assert_eq!(
            run.source,
            ProgressSource::Message {
                kind: MessageType::Other("dispatch".into())
            }
        );
    }

    #[test]
    fn coordinator_holding() {
        let mut hold = msg("m1", COORD, "status", T0 + 3 * MIN, json!({}));
        hold.subject = Some("holding: waiting for Karl's merge window".into());
        let data = fleet(vec![task("t1", "dispatched")], vec![hold.clone()]);
        let report = ProgressTracker::new().update(&data, T0 + 5 * MIN);
        let run = &report.runs[0];
        assert_eq!(
            run.holding.as_deref(),
            Some("waiting for Karl's merge window")
        );
        assert_eq!(run.last_coordinator_activity_ms, Some(T0 + 3 * MIN));

        // Body form, per the task spec.
        let mut body_hold = msg("m1", COORD, "status", T0 + 3 * MIN, json!({}));
        body_hold.subject = Some("status".into());
        body_hold.body = Some("Holding: CI".into());
        let data = fleet(vec![], vec![body_hold]);
        let report = ProgressTracker::new().update(&data, T0 + 5 * MIN);
        assert_eq!(report.runs[0].holding.as_deref(), Some("CI"));

        // A newer coordinator message ends the hold.
        let later = msg("m2", COORD, "dispatch", T0 + 4 * MIN, json!({}));
        let data = fleet(vec![], vec![later, hold]);
        let report = ProgressTracker::new().update(&data, T0 + 5 * MIN);
        assert_eq!(report.runs[0].holding, None);
    }

    #[test]
    fn owner_prefers_live_dispatch() {
        let mut data = fleet(vec![task("t1", "dispatched")], vec![]);
        let mut old = worker("t1", "ctx_0", "completed");
        old.agent_terminal_handle = Some("term_old".into());
        let mut live = worker("t1", "ctx_2", "dispatched");
        live.agent_terminal_handle = Some("term_new".into());
        data.workers = vec![old, live];
        let report = ProgressTracker::new().update(&data, T0);
        let t = only_task(&report);
        assert_eq!(t.owner_handle.as_deref(), Some("term_new"));
        assert_eq!(t.dispatch_id.as_deref(), Some("ctx_2"));
    }

    #[test]
    fn fixtures_parse_into_a_report() {
        let tasks = parse_task_list(fixture!("task_list.json")).unwrap();
        let data = FleetData {
            runs: parse_run_list(fixture!("run_list.json")).unwrap(),
            workers: parse_worker_list(fixture!("worker_list.json"))
                .unwrap()
                .workers,
            tasks: [("run_7c25cdcf46e0".to_string(), tasks)].into(),
            inbox: parse_inbox(fixture!("inbox.json")).unwrap(),
            ..Default::default()
        };
        let report = ProgressTracker::new().update(&data, T0 * 2);
        assert_eq!(report.tasks.len(), data.tasks["run_7c25cdcf46e0"].len());
        assert_eq!(report.runs.len(), 3);
        let dispatched = report
            .tasks
            .iter()
            .find(|t| t.task_id == "task_e02460f3b723")
            .unwrap();
        // No worker row in the fixture: the owner falls back to the task's assignee.
        assert_eq!(
            dispatched.owner_handle.as_deref(),
            Some("term_a6c4f600-9ec0-4529-84ee-19857c2fc7c0")
        );
        assert_eq!(dispatched.dispatch_id.as_deref(), Some("ctx_ad41e5edf34a"));
        assert!(report.tasks.iter().all(|t| t.last_progress_ms.is_some()));
    }
}

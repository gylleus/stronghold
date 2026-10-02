//! Event forwarder: wakes a Run's coordinator when something it must act on happens.
//!
//! Coordinators never watch the bus themselves (every wake replays their whole context). The
//! forwarder does it for them at no token cost: each tick it picks the Run's real events out of
//! the polled inbox (`worker_done`, `merge_ready`, `question`, `escalation`, never heartbeats)
//! plus tasks Orca failed while their worker kept working, and types one line per batch into
//! the coordinator's terminal. It types only into a live Claude session
//! ([`claude_session_live`]): a coordinator that exited leaves a shell, where the line would run
//! as a command. If the coordinator stays unreachable, Karl gets one notification per Run.
//!
//! Exactly-once within a session: each event is forwarded once, and on the first tick events
//! older than [`STARTUP_REPLAY_MS`] count as already handled, so a restart replays only the
//! last few minutes. Runs with an attendant (see [`crate::fleet::RunRoles`]) are left to it.

use std::collections::{BTreeMap, BTreeSet};

use crate::detect::claude_session_live;
use crate::fleet::FleetData;
use crate::orca::{InboxMessage, MessageType, Timestamp};
use crate::whip::actions::WhipAction;
use crate::whip::progress::ProgressReport;

/// Events older than this are history, never forwarded.
pub const MAX_EVENT_AGE_MS: i64 = 6 * 60 * 60_000;
/// On the first tick, events younger than this are still forwarded (they may have arrived while
/// Myrstack was restarting); older ones count as handled.
pub const STARTUP_REPLAY_MS: i64 = 10 * 60_000;
/// How long a Run's events may wait for an unreachable coordinator before Karl is told.
pub const UNREACHABLE_GRACE_MS: i64 = 5 * 60_000;
/// Prefix of every forwarded line.
pub const EVENT_PREFIX: &str = "Fleet event:";

/// Bus message types a coordinator must act on.
const FORWARDED: &[MessageType] = &[
    MessageType::WorkerDone,
    MessageType::MergeReady,
    MessageType::Question,
    MessageType::Escalation,
];

/// One thing a coordinator must hear about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Dedupe key: the message id, or `revived:<task id>`.
    pub key: String,
    pub run_id: String,
    pub at_ms: i64,
    /// One line without the prefix, e.g. `worker_done from fix-login: PR open — msg msg_1`.
    pub line: String,
}

/// Forwarding state. Owned by the [`crate::whip::WhipEngine`]; pure apart from its memory.
#[derive(Debug, Default)]
pub struct Forwarder {
    seeded: bool,
    /// Keys already forwarded (or seeded), pruned to events still visible.
    sent: BTreeSet<String>,
    /// Runs with events waiting on an unreachable coordinator, since when.
    waiting: BTreeMap<String, i64>,
    /// Runs Karl has been told about; cleared once the coordinator is reachable again.
    alerted: BTreeSet<String>,
}

impl Forwarder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decides this tick's forwards and alerts, and records them as done (dry runs included, as
    /// for the rest of the whip).
    pub fn tick(
        &mut self,
        data: &FleetData,
        report: &ProgressReport,
        now_ms: i64,
    ) -> Vec<WhipAction> {
        let events = collect_events(data, report, now_ms);
        let visible: BTreeSet<&str> = events.iter().map(|e| e.key.as_str()).collect();
        self.sent.retain(|k| visible.contains(k.as_str()));
        if !self.seeded {
            self.seeded = true;
            for e in &events {
                if now_ms - e.at_ms > STARTUP_REPLAY_MS {
                    self.sent.insert(e.key.clone());
                }
            }
        }

        let mut by_run: BTreeMap<&str, Vec<&Event>> = BTreeMap::new();
        for e in events.iter().filter(|e| !self.sent.contains(&e.key)) {
            by_run.entry(e.run_id.as_str()).or_default().push(e);
        }
        self.waiting
            .retain(|run, _| by_run.contains_key(run.as_str()));

        let mut per_coordinator: BTreeMap<String, Vec<&Event>> = BTreeMap::new();
        let mut actions = Vec::new();
        for (run_id, run_events) in by_run {
            let roles = data.run_roles(run_id);
            if roles.attendant.is_some() {
                self.sent.extend(run_events.iter().map(|e| e.key.clone()));
                continue;
            }
            let reachable = roles
                .coordinator
                .filter(|h| data.terminal(h).is_some_and(claude_session_live));
            match reachable {
                Some(handle) => {
                    self.waiting.remove(run_id);
                    self.alerted.remove(run_id);
                    per_coordinator
                        .entry(handle)
                        .or_default()
                        .extend(run_events);
                }
                None => {
                    let since = *self.waiting.entry(run_id.to_string()).or_insert(now_ms);
                    if now_ms - since >= UNREACHABLE_GRACE_MS
                        && self.alerted.insert(run_id.to_string())
                    {
                        actions.push(WhipAction::CoordinatorUnreachable {
                            run_id: run_id.to_string(),
                            waiting: run_events.len(),
                            first: run_events[0].line.clone(),
                        });
                    }
                }
            }
        }

        for (handle, mut batch) in per_coordinator {
            batch.sort_by_key(|e| e.at_ms);
            self.sent.extend(batch.iter().map(|e| e.key.clone()));
            let run_ids: BTreeSet<String> = batch.iter().map(|e| e.run_id.clone()).collect();
            actions.push(WhipAction::Forward {
                text: forward_text(&batch, &run_ids),
                handle,
                run_ids: run_ids.into_iter().collect(),
                events: batch.len(),
            });
        }
        actions
    }
}

/// The line typed into the coordinator: every event, then how to read them in full.
pub fn forward_text(batch: &[&Event], run_ids: &BTreeSet<String>) -> String {
    let lines: Vec<&str> = batch.iter().map(|e| e.line.as_str()).collect();
    let read: Vec<String> = run_ids
        .iter()
        .map(|r| format!("`orca orchestration check --run {r} --all`"))
        .collect();
    format!(
        "{EVENT_PREFIX} {}. Read in full with {}.",
        lines.join(" | "),
        read.join(" / ")
    )
}

/// The Runs' real events still in the polled inbox, plus one per task that Orca failed while
/// its worker kept working.
pub fn collect_events(data: &FleetData, report: &ProgressReport, now_ms: i64) -> Vec<Event> {
    let mut events: Vec<Event> = data
        .inbox
        .iter()
        .filter_map(|m| message_event(data, m, now_ms))
        .collect();
    for task in report.tasks.iter().filter(|t| t.failed_but_alive) {
        let worker = task.owner_handle.as_deref().unwrap_or("its worker");
        events.push(Event {
            key: format!("revived:{}", task.task_id),
            run_id: task.run_id.clone(),
            at_ms: now_ms,
            line: format!(
                "task {} is marked failed in Orca but {worker} is still working on it; Orca \
                 rejects its heartbeats and worker_done, so read its terminal for the result",
                task_label(data, &task.run_id, &task.task_id)
            ),
        });
    }
    events
}

fn message_event(data: &FleetData, m: &InboxMessage, now_ms: i64) -> Option<Event> {
    let kind = m.message_type.as_ref().filter(|k| FORWARDED.contains(k))?;
    if is_rejected(m) {
        return None;
    }
    let run_id = m.run_id.clone()?;
    let at_ms = m.created_at.as_ref().and_then(Timestamp::epoch_ms)?;
    if now_ms - at_ms > MAX_EVENT_AGE_MS {
        return None;
    }
    let from = m.from_handle.as_deref().unwrap_or("?");
    if data.effective_coordinator(&run_id).as_deref() == Some(from) {
        return None;
    }
    let source = m
        .parsed_payload()
        .and_then(|p| p.task_id)
        .map(|t| task_label(data, &run_id, &t))
        .unwrap_or_else(|| from.to_string());
    let subject = m
        .subject
        .as_deref()
        .map(|s| s.replace(['\n', '\r'], " "))
        .unwrap_or_default();
    Some(Event {
        key: m.id.clone(),
        run_id,
        at_ms,
        line: format!("{} from {source}: {subject} — msg {}", kind.as_str(), m.id),
    })
}

fn task_label(data: &FleetData, run_id: &str, task_id: &str) -> String {
    data.tasks
        .get(run_id)
        .and_then(|ts| ts.iter().find(|t| t.id == task_id))
        .map_or_else(|| task_id.to_string(), |t| t.label().to_string())
}

/// Orca echoes a refused message (e.g. a report on a revoked dispatch) with this marker.
fn is_rejected(m: &InboxMessage) -> bool {
    m.payload
        .as_deref()
        .is_some_and(|p| p.contains("_orcaLifecycleRejection"))
        || m.subject
            .as_deref()
            .is_some_and(|s| s.starts_with("Rejected "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::{AgentIdentity, Run, Task, Terminal, Worker};
    use crate::whip::progress::ProgressTracker;
    use serde_json::json;

    const RUN: &str = "run_1";
    const COORD: &str = "term_coord";
    const WORKER: &str = "term_worker";
    const MIN: i64 = 60_000;
    const NOW: i64 = 1_790_000_000_000;
    const CLAUDE_IDLE: &str = "⏺ Dispatched.\nnew task? /clear to save 120k tokens";
    const SHELL: &str = "~/orca/workspaces/x  main ✔        3m";

    fn coordinator(preview: &str) -> Terminal {
        Terminal {
            handle: COORD.into(),
            connected: true,
            agent_identity: Some(AgentIdentity::Claude),
            preview: Some(preview.into()),
            ..Default::default()
        }
    }

    fn msg(id: &str, from: &str, kind: &str, at: i64, subject: &str) -> InboxMessage {
        serde_json::from_value(json!({
            "id": id, "run_id": RUN, "from_handle": from, "to_handle": format!("run:{RUN}"),
            "type": kind, "subject": subject, "created_at": at,
            "payload": json!({"taskId": "t1", "dispatchId": "ctx_t1"}).to_string(),
        }))
        .unwrap()
    }

    fn fleet(preview: &str, inbox: Vec<InboxMessage>) -> FleetData {
        let task: Task = serde_json::from_value(json!({
            "id": "t1", "run_id": RUN, "status": "dispatched", "display_name": "fix-login",
            "created_by_terminal_handle": COORD, "created_at": NOW - 60 * MIN,
        }))
        .unwrap();
        let worker: Worker = serde_json::from_value(json!({
            "dispatchId": "ctx_t1", "taskId": "t1", "runId": RUN,
            "dispatchStatus": "dispatched", "agentTerminalHandle": WORKER,
        }))
        .unwrap();
        FleetData {
            polled_at_ms: NOW,
            terminals: vec![
                coordinator(preview),
                Terminal {
                    handle: WORKER.into(),
                    ..Default::default()
                },
            ],
            runs: vec![
                serde_json::from_value::<Run>(json!({
                    "id": RUN, "coordinator_handle": COORD,
                    "created_at": NOW - 60 * MIN, "updated_at": NOW,
                }))
                .unwrap(),
            ],
            workers: vec![worker],
            tasks: [(RUN.to_string(), vec![task])].into(),
            inbox,
            ..Default::default()
        }
    }

    fn tick(f: &mut Forwarder, data: &FleetData, now: i64) -> Vec<WhipAction> {
        let report = ProgressTracker::new().update(data, now);
        f.tick(data, &report, now)
    }

    fn forwards(actions: &[WhipAction]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                WhipAction::Forward { handle, text, .. } => Some(format!("{handle}: {text}")),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn forwards_real_events_once_and_skips_heartbeats() {
        let data = fleet(
            CLAUDE_IDLE,
            vec![
                msg("m2", WORKER, "merge_ready", NOW - MIN, "PR #9 green"),
                msg("m1", WORKER, "heartbeat", NOW - 2 * MIN, "alive"),
            ],
        );
        let mut f = Forwarder::new();
        let first = forwards(&tick(&mut f, &data, NOW));
        assert_eq!(
            first,
            vec![
                "term_coord: Fleet event: merge_ready from fix-login: PR #9 green — msg m2. \
                 Read in full with `orca orchestration check --run run_1 --all`."
            ]
        );
        assert!(tick(&mut f, &data, NOW + MIN).is_empty());
    }

    #[test]
    fn batches_events_in_time_order() {
        let data = fleet(
            CLAUDE_IDLE,
            vec![
                msg("m2", WORKER, "question", NOW - MIN, "A or B?"),
                msg("m1", WORKER, "escalation", NOW - 2 * MIN, "CI on fire"),
            ],
        );
        let out = forwards(&tick(&mut Forwarder::new(), &data, NOW));
        assert_eq!(out.len(), 1);
        let (esc, q) = (
            out[0].find("escalation").unwrap(),
            out[0].find("question").unwrap(),
        );
        assert!(esc < q, "{}", out[0]);
    }

    #[test]
    fn startup_skips_old_events_and_never_forwards_history() {
        let data = fleet(
            CLAUDE_IDLE,
            vec![
                msg(
                    "old",
                    WORKER,
                    "worker_done",
                    NOW - 30 * MIN,
                    "done long ago",
                ),
                msg("new", WORKER, "worker_done", NOW - 2 * MIN, "just done"),
                msg(
                    "ancient",
                    WORKER,
                    "worker_done",
                    NOW - 7 * 60 * MIN,
                    "yesterday",
                ),
            ],
        );
        let out = forwards(&tick(&mut Forwarder::new(), &data, NOW));
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("msg new") && !out[0].contains("msg old"));
    }

    #[test]
    fn ignores_the_coordinators_own_and_rejected_messages() {
        let mut rejected = msg(
            "m2",
            WORKER,
            "worker_done",
            NOW - MIN,
            "Rejected worker_done: x",
        );
        rejected.payload = Some(json!({"_orcaLifecycleRejection": "revoked"}).to_string());
        let data = fleet(
            CLAUDE_IDLE,
            vec![msg("m1", COORD, "question", NOW - MIN, "my own"), rejected],
        );
        assert!(tick(&mut Forwarder::new(), &data, NOW).is_empty());
    }

    #[test]
    fn never_types_into_a_shell_and_alerts_once() {
        let data = fleet(
            SHELL,
            vec![msg("m1", WORKER, "worker_done", NOW - MIN, "done")],
        );
        let mut f = Forwarder::new();
        assert!(tick(&mut f, &data, NOW).is_empty());
        let later = tick(&mut f, &data, NOW + UNREACHABLE_GRACE_MS);
        assert!(matches!(
            later.as_slice(),
            [WhipAction::CoordinatorUnreachable { run_id, waiting: 1, .. }] if run_id == RUN
        ));
        assert!(tick(&mut f, &data, NOW + 2 * UNREACHABLE_GRACE_MS).is_empty());

        // The coordinator comes back: the queued event goes through.
        let back = fleet(
            CLAUDE_IDLE,
            vec![msg("m1", WORKER, "worker_done", NOW - MIN, "done")],
        );
        let out = forwards(&tick(&mut f, &back, NOW + 2 * UNREACHABLE_GRACE_MS + MIN));
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("msg m1"));
    }

    #[test]
    fn tells_the_coordinator_about_a_failed_task_whose_worker_kept_going() {
        let mut data = fleet(
            CLAUDE_IDLE,
            vec![msg("hb", WORKER, "heartbeat", NOW - MIN, "implementing")],
        );
        let task = &mut data.tasks.get_mut(RUN).unwrap()[0];
        task.status = crate::orca::TaskStatus::Failed;
        task.completed_at = Some(Timestamp::Millis(NOW - 20 * MIN));
        data.workers[0].dispatch_status = Some(crate::orca::DispatchStatus::Failed);
        let mut f = Forwarder::new();
        let out = forwards(&tick(&mut f, &data, NOW));
        assert_eq!(out.len(), 1);
        assert!(
            out[0].contains(
                "task fix-login is marked failed in Orca but term_worker is still working"
            ),
            "{}",
            out[0]
        );
        assert!(tick(&mut f, &data, NOW + MIN).is_empty());
    }
}

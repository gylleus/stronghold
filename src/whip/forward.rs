//! Event forwarder: wakes a Run's coordinator when something it must act on happens.
//!
//! Coordinators never watch the bus themselves (every wake replays their whole context). The
//! forwarder does it for them at no token cost: each tick it picks the Run's real events out of
//! the polled inbox (`worker_done`, `merge_ready`, `question`, `escalation`, never heartbeats)
//! plus tasks Orca failed while their worker kept working, and types one line per batch into
//! the coordinator's terminal (a long backlog becomes one digest line). A report Orca refused
//! only because it revoked the dispatch after misreporting its start
//! ([`ClanData::is_misreported_launch_report`]) is forwarded as the worker's report. It types only into a live Claude session
//! ([`claude_session_live`]): a coordinator that exited leaves a shell, where the line would run
//! as a command. If the coordinator stays unreachable, Karl gets one notification per Run.
//!
//! At-least-once: an event counts as forwarded only once its line was typed (see
//! [`Forwarder::record`]). A failed send keeps it pending and retries with a backoff
//! ([`retry_delay_ms`]); after [`ALERT_AFTER_FAILURES`] failures in a row Karl gets one
//! notification per Run. Forwarding is live even when the whip is in dry run: it only delivers
//! facts. The forwarded set survives restarts ([`load_forwarded`] / [`save_forwarded`],
//! `forwarded.json` next to `settings.json`), so events that arrived while Myrstack was down are
//! forwarded on start, up to [`MAX_EVENT_AGE_MS`] old. Without that file the first tick counts
//! events older than [`STARTUP_REPLAY_MS`] as already handled. Runs with an attendant (see
//! [`crate::clan::RunRoles`]) are left to it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::clan::ClanData;
use crate::detect::claude_session_live;
use crate::orca::{InboxMessage, MessageType, Timestamp};
use crate::whip::actions::WhipAction;
use crate::whip::progress::ProgressReport;
use crate::whip::rules::{StallFinding, StallReason, StallTarget};

/// Events older than this are history, never forwarded.
pub const MAX_EVENT_AGE_MS: i64 = 6 * 60 * 60_000;
/// Without a saved forwarded set, events younger than this are still forwarded on the first
/// tick (they may have arrived while Myrstack was restarting); older ones count as handled.
pub const STARTUP_REPLAY_MS: i64 = 10 * 60_000;
/// How long a Run's events may wait for an unreachable coordinator before Karl is told.
pub const UNREACHABLE_GRACE_MS: i64 = 5 * 60_000;
/// Longest wait between two attempts to forward a Run's events after failed sends; also how
/// long a handed-out forward may go without an outcome before it is retried.
pub const RETRY_MAX_MS: i64 = 10 * 60_000;
/// Failed sends in a row for a Run before Karl is told.
pub const ALERT_AFTER_FAILURES: u32 = 3;
/// File holding the forwarded set, next to `settings.json`.
pub const FORWARDED_FILE: &str = "forwarded.json";
/// Most events a forwarded line lists; a longer backlog is one digest line ending in a count.
pub const MAX_LISTED: usize = 5;
/// Prefix of every forwarded line.
pub const EVENT_PREFIX: &str = "Clan event:";

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
    /// Dedupe key: the message id, `revived:<task id>` or `unreported:<task id>:<dispatch id>`.
    pub key: String,
    pub run_id: String,
    pub at_ms: i64,
    /// One line without the prefix, e.g. `worker_done from fix-login: PR open — msg msg_1`.
    pub line: String,
}

/// Wait before the next attempt after `failures` failed sends in a row: the next tick, then
/// 1, 2, 4, 8 minutes, capped at [`RETRY_MAX_MS`].
pub fn retry_delay_ms(failures: u32) -> i64 {
    match failures {
        0 | 1 => 0,
        n => (60_000i64 << (n - 2).min(10)).min(RETRY_MAX_MS),
    }
}

/// Failed sends of one Run's events.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failures {
    count: u32,
    retry_at_ms: i64,
    handle: String,
    error: String,
    alerted: bool,
}

/// Forwarding state. Owned by the [`crate::whip::WhipEngine`]; pure apart from its memory.
#[derive(Debug, Default)]
pub struct Forwarder {
    seeded: bool,
    /// Keys forwarded (or seeded) with their event time, pruned to events still visible or
    /// younger than [`MAX_EVENT_AGE_MS`].
    sent: BTreeMap<String, i64>,
    /// Keys handed out in a `Forward` and not yet [`Forwarder::record`]ed: event time, and when
    /// they were handed out.
    in_flight: BTreeMap<String, (i64, i64)>,
    /// Runs whose last send failed.
    failures: BTreeMap<String, Failures>,
    /// Runs with events waiting on an unreachable coordinator, since when.
    waiting: BTreeMap<String, i64>,
    /// Runs Karl has been told about; cleared once the coordinator is reachable again.
    alerted: BTreeSet<String>,
    /// `sent` changed since the last [`Forwarder::take_changed`].
    changed: bool,
}

impl Forwarder {
    pub fn new() -> Self {
        Self::default()
    }

    /// A forwarder that starts from a saved forwarded set (see [`load_forwarded`]) instead of
    /// the startup replay window.
    pub fn restored(sent: BTreeMap<String, i64>) -> Self {
        Self {
            seeded: true,
            sent,
            ..Self::default()
        }
    }

    /// The forwarded set, when it changed since the last call; for [`save_forwarded`].
    pub fn take_changed(&mut self) -> Option<BTreeMap<String, i64>> {
        std::mem::take(&mut self.changed).then(|| self.sent.clone())
    }

    fn mark_sent(&mut self, key: &str, at_ms: i64) {
        if self.sent.insert(key.to_string(), at_ms).is_none() {
            self.changed = true;
        }
    }

    /// Decides this tick's forwards and alerts. A forwarded event stays pending until
    /// [`Forwarder::record`] reports its send.
    pub fn tick(
        &mut self,
        data: &ClanData,
        report: &ProgressReport,
        findings: &[StallFinding],
        now_ms: i64,
    ) -> Vec<WhipAction> {
        let events = collect_events(data, report, findings, now_ms);
        let visible: BTreeSet<&str> = events.iter().map(|e| e.key.as_str()).collect();
        let before = self.sent.len();
        self.sent
            .retain(|k, at| visible.contains(k.as_str()) || now_ms - *at <= MAX_EVENT_AGE_MS);
        self.changed |= self.sent.len() != before;
        self.in_flight.retain(|k, _| visible.contains(k.as_str()));
        if !self.seeded {
            self.seeded = true;
            for e in &events {
                if now_ms - e.at_ms > STARTUP_REPLAY_MS {
                    self.mark_sent(&e.key, e.at_ms);
                }
            }
        }

        let mut by_run: BTreeMap<String, Vec<&Event>> = BTreeMap::new();
        for e in events.iter().filter(|e| !self.sent.contains_key(&e.key)) {
            by_run.entry(e.run_id.clone()).or_default().push(e);
        }
        self.waiting.retain(|run, _| by_run.contains_key(run));
        self.failures.retain(|run, _| by_run.contains_key(run));

        let mut actions = Vec::new();
        for (run_id, f) in &mut self.failures {
            if f.count >= ALERT_AFTER_FAILURES && !f.alerted {
                f.alerted = true;
                actions.push(WhipAction::ForwardFailing {
                    run_id: run_id.clone(),
                    handle: f.handle.clone(),
                    failures: f.count,
                    error: f.error.clone(),
                });
            }
        }

        let mut per_coordinator: BTreeMap<String, Vec<&Event>> = BTreeMap::new();
        for (run_id, run_events) in by_run {
            let roles = data.run_roles(&run_id);
            if roles.attendant.is_some() {
                for e in run_events {
                    self.mark_sent(&e.key, e.at_ms);
                }
                continue;
            }
            let reachable = roles
                .coordinator
                .filter(|h| data.terminal(h).is_some_and(claude_session_live));
            match reachable {
                Some(handle) => {
                    self.waiting.remove(&run_id);
                    self.alerted.remove(&run_id);
                    let backing_off = self
                        .failures
                        .get(&run_id)
                        .is_some_and(|f| now_ms < f.retry_at_ms);
                    if backing_off {
                        continue;
                    }
                    let due: Vec<&Event> = run_events
                        .into_iter()
                        .filter(|e| {
                            self.in_flight
                                .get(&e.key)
                                .is_none_or(|(_, since)| now_ms - since >= RETRY_MAX_MS)
                        })
                        .collect();
                    if !due.is_empty() {
                        per_coordinator.entry(handle).or_default().extend(due);
                    }
                }
                None => {
                    let since = *self.waiting.entry(run_id.clone()).or_insert(now_ms);
                    if now_ms - since >= UNREACHABLE_GRACE_MS && self.alerted.insert(run_id.clone())
                    {
                        actions.push(WhipAction::CoordinatorUnreachable {
                            run_id,
                            waiting: run_events.len(),
                            first: run_events[0].line.clone(),
                        });
                    }
                }
            }
        }

        for (handle, mut batch) in per_coordinator {
            batch.sort_by_key(|e| e.at_ms);
            for e in &batch {
                self.in_flight.insert(e.key.clone(), (e.at_ms, now_ms));
            }
            let run_ids: BTreeSet<String> = batch.iter().map(|e| e.run_id.clone()).collect();
            actions.push(WhipAction::Forward {
                text: forward_text(&batch, &run_ids),
                handle,
                run_ids: run_ids.into_iter().collect(),
                keys: batch.iter().map(|e| e.key.clone()).collect(),
            });
        }
        actions
    }

    /// Records how a `Forward` of `keys` to `handle` went. Sent marks the events forwarded and
    /// clears the Runs' failures; an error keeps them pending and backs the Runs off.
    pub fn record(
        &mut self,
        handle: &str,
        keys: &[String],
        run_ids: &[String],
        result: Result<(), &str>,
        now_ms: i64,
    ) {
        for key in keys {
            if let Some((at_ms, _)) = self.in_flight.remove(key)
                && result.is_ok()
            {
                self.mark_sent(key, at_ms);
            }
        }
        for run_id in run_ids {
            let Err(error) = result else {
                self.failures.remove(run_id);
                continue;
            };
            let f = self
                .failures
                .entry(run_id.clone())
                .or_insert_with(|| Failures {
                    count: 0,
                    retry_at_ms: now_ms,
                    handle: handle.to_string(),
                    error: String::new(),
                    alerted: false,
                });
            f.count += 1;
            f.retry_at_ms = now_ms + retry_delay_ms(f.count);
            f.handle = handle.to_string();
            f.error = error.to_string();
        }
    }
}

/// The saved forwarded set: event key → event time (ms since the Unix epoch).
#[derive(Debug, Serialize, Deserialize)]
struct ForwardedFile {
    sent: BTreeMap<String, i64>,
}

/// Reads the forwarded set saved at `path`. `None` when there is none or it is unreadable
/// (logged), so the forwarder falls back to the startup replay window.
pub fn load_forwarded(path: &Path) -> Option<BTreeMap<String, i64>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!("whip: cannot read {}: {e}", path.display());
            return None;
        }
    };
    serde_json::from_str::<ForwardedFile>(&text)
        .inspect_err(|e| log::warn!("whip: ignoring invalid {}: {e}", path.display()))
        .ok()
        .map(|f| f.sent)
}

/// Writes the forwarded set to `path` atomically.
pub fn save_forwarded(path: &Path, sent: &BTreeMap<String, i64>) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string(&ForwardedFile { sent: sent.clone() })?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)
}

/// The line typed into the coordinator: the events (the first [`MAX_LISTED`] of a backlog, then
/// a count), then how to read them in full.
pub fn forward_text(batch: &[&Event], run_ids: &BTreeSet<String>) -> String {
    let mut lines: Vec<String> = batch
        .iter()
        .take(MAX_LISTED)
        .map(|e| e.line.clone())
        .collect();
    if batch.len() > MAX_LISTED {
        lines.push(format!("and {} more", batch.len() - MAX_LISTED));
    }
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
/// its worker kept working, and one per worker that looks finished but never reported (a
/// [`StallReason::MissingReport`] finding).
pub fn collect_events(
    data: &ClanData,
    report: &ProgressReport,
    findings: &[StallFinding],
    now_ms: i64,
) -> Vec<Event> {
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
    for f in findings
        .iter()
        .filter(|f| f.reason == StallReason::MissingReport)
    {
        let StallTarget::Worker {
            handle,
            task_id,
            run_id,
        } = &f.target
        else {
            continue;
        };
        let dispatch = report
            .tasks
            .iter()
            .find(|t| &t.task_id == task_id)
            .and_then(|t| t.dispatch_id.as_deref())
            .unwrap_or("");
        events.push(Event {
            key: format!("unreported:{task_id}:{dispatch}"),
            run_id: run_id.clone(),
            at_ms: now_ms,
            line: format!(
                "worker {handle} looks finished on task {} but never reported: idle at its \
                 prompt and no worker_done/merge_ready on the bus; read its terminal",
                task_label(data, run_id, task_id)
            ),
        });
    }
    events
}

fn message_event(data: &ClanData, m: &InboxMessage, now_ms: i64) -> Option<Event> {
    let kind = m.message_type.as_ref().filter(|k| FORWARDED.contains(k))?;
    let revoked = data.is_misreported_launch_report(m);
    if is_rejected(m) && !revoked {
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
    let mut subject = m
        .subject
        .as_deref()
        .map(|s| s.replace(['\n', '\r'], " "))
        .unwrap_or_default();
    if revoked {
        let original = subject.strip_prefix("Rejected ").unwrap_or(&subject);
        let original = original
            .split_once(": ")
            .map_or(original, |(_, rest)| rest)
            .to_string();
        subject = format!(
            "{original} (rejected by Orca on a dispatch it revoked after a misreported launch; \
             the report stands, its body has the original)"
        );
    }
    Some(Event {
        key: m.id.clone(),
        run_id,
        at_ms,
        line: format!("{} from {source}: {subject} — msg {}", kind.as_str(), m.id),
    })
}

fn task_label(data: &ClanData, run_id: &str, task_id: &str) -> String {
    data.tasks
        .get(run_id)
        .and_then(|ts| ts.iter().find(|t| t.id == task_id))
        .map_or_else(|| task_id.to_string(), |t| t.label().to_string())
}

/// Orca echoes a refused message (e.g. a report on a revoked dispatch) with this marker.
fn is_rejected(m: &InboxMessage) -> bool {
    m.payload
        .as_deref()
        .is_some_and(|p| p.contains(crate::orca::REJECTION_KEY))
        || m.subject
            .as_deref()
            .is_some_and(|s| s.starts_with("Rejected "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::{AgentIdentity, Run, Task, TaskStatus, Terminal, Worker};
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

    fn clan(preview: &str, inbox: Vec<InboxMessage>) -> ClanData {
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
        ClanData {
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

    fn tick(f: &mut Forwarder, data: &ClanData, now: i64) -> Vec<WhipAction> {
        let report = ProgressTracker::new().update(data, now);
        f.tick(data, &report, &[], now)
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
        let data = clan(
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
                "term_coord: Clan event: merge_ready from fix-login: PR #9 green — msg m2. \
                 Read in full with `orca orchestration check --run run_1 --all`."
            ]
        );
        assert!(tick(&mut f, &data, NOW + MIN).is_empty());
    }

    #[test]
    fn batches_events_in_time_order() {
        let data = clan(
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
        let data = clan(
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
        let data = clan(
            CLAUDE_IDLE,
            vec![msg("m1", COORD, "question", NOW - MIN, "my own"), rejected],
        );
        assert!(tick(&mut Forwarder::new(), &data, NOW).is_empty());
    }

    #[test]
    fn forwards_a_report_rejected_on_a_revoked_dispatch() {
        let mut revoked = msg(
            "m1",
            WORKER,
            "worker_done",
            NOW - MIN,
            "Rejected worker_done: PR 8118 open",
        );
        revoked.payload = Some(
            json!({"taskId": "t1", "dispatchId": "ctx_1", "_orcaLifecycleRejection": {
                "code": "dispatch_capability_invalid",
                "reason": "Dispatch ctx_1 capability is revoked."}})
            .to_string(),
        );
        let mut data = clan(CLAUDE_IDLE, vec![revoked]);
        let task = &mut data.tasks.get_mut(RUN).unwrap()[0];
        task.status = TaskStatus::Failed;
        task.dispatch_id = Some("ctx_1".into());
        let out = forwards(&tick(&mut Forwarder::new(), &data, NOW));
        assert_eq!(out.len(), 1);
        assert!(
            out[0].contains(": PR 8118 open (rejected by Orca"),
            "{}",
            out[0]
        );
        assert!(out[0].contains("the report stands"));

        // A dispatch the coordinator replaced stays ignored.
        data.tasks.get_mut(RUN).unwrap()[0].dispatch_id = Some("ctx_2".into());
        assert!(tick(&mut Forwarder::new(), &data, NOW).is_empty());
    }

    #[test]
    fn a_backlog_is_one_digest_line() {
        let inbox = (0..8)
            .map(|i| {
                let id = format!("m{i}");
                msg(
                    &id,
                    WORKER,
                    "worker_done",
                    NOW - MIN + i,
                    &format!("done {i}"),
                )
            })
            .collect();
        let out = forwards(&tick(&mut Forwarder::new(), &clan(CLAUDE_IDLE, inbox), NOW));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].matches("worker_done from").count(), MAX_LISTED);
        assert!(out[0].contains("| and 3 more."), "{}", out[0]);
    }

    #[test]
    fn never_types_into_a_shell_and_alerts_once() {
        let data = clan(
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
        let back = clan(
            CLAUDE_IDLE,
            vec![msg("m1", WORKER, "worker_done", NOW - MIN, "done")],
        );
        let out = forwards(&tick(&mut f, &back, NOW + 2 * UNREACHABLE_GRACE_MS + MIN));
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("msg m1"));
    }

    #[test]
    fn tells_the_coordinator_about_a_failed_task_whose_worker_kept_going() {
        let mut data = clan(
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

    /// The keys and run ids of the single `Forward` in `actions`.
    fn forward_of(actions: &[WhipAction]) -> (String, Vec<String>, Vec<String>) {
        match actions {
            [
                WhipAction::Forward {
                    handle,
                    keys,
                    run_ids,
                    ..
                },
            ] => (handle.clone(), keys.clone(), run_ids.clone()),
            other => panic!("expected one forward, got {other:?}"),
        }
    }

    #[test]
    fn retry_delay_backs_off_to_ten_minutes() {
        let delays: Vec<i64> = (1..=7).map(retry_delay_ms).collect();
        assert_eq!(
            delays,
            vec![0, MIN, 2 * MIN, 4 * MIN, 8 * MIN, 10 * MIN, 10 * MIN]
        );
    }

    #[test]
    fn success_marks_events_sent() {
        let data = clan(
            CLAUDE_IDLE,
            vec![msg("m1", WORKER, "worker_done", NOW - MIN, "done")],
        );
        let mut f = Forwarder::new();
        let (handle, keys, runs) = forward_of(&tick(&mut f, &data, NOW));
        assert_eq!(keys, vec!["m1"]);
        // Handed out but not yet confirmed: not sent again meanwhile.
        assert!(tick(&mut f, &data, NOW + MIN).is_empty());
        f.record(&handle, &keys, &runs, Ok(()), NOW + MIN);
        assert!(f.take_changed().is_some_and(|s| s.contains_key("m1")));
        assert!(tick(&mut f, &data, NOW + 30 * MIN).is_empty());
    }

    #[test]
    fn unconfirmed_forward_is_retried_after_the_retry_cap() {
        let data = clan(
            CLAUDE_IDLE,
            vec![msg("m1", WORKER, "worker_done", NOW - MIN, "done")],
        );
        let mut f = Forwarder::new();
        forward_of(&tick(&mut f, &data, NOW));
        assert!(tick(&mut f, &data, NOW + RETRY_MAX_MS - 1).is_empty());
        forward_of(&tick(&mut f, &data, NOW + RETRY_MAX_MS));
    }

    #[test]
    fn failed_send_is_retried_with_backoff_and_alerts_once() {
        let data = clan(
            CLAUDE_IDLE,
            vec![msg("m1", WORKER, "worker_done", NOW - MIN, "done")],
        );
        let mut f = Forwarder::new();
        let fail = |f: &mut Forwarder, now: i64| {
            let (handle, keys, runs) = forward_of(&tick(f, &data, now));
            f.record(&handle, &keys, &runs, Err("failed: orca down"), now);
        };
        let mut now = NOW;
        // First failure: retried on the next tick.
        fail(&mut f, now);
        now += MIN;
        // Second failure: next try a minute later.
        fail(&mut f, now);
        assert!(tick(&mut f, &data, now + MIN - 1).is_empty());
        now += MIN;
        // Third failure: Karl is told once, on the next tick, while retries go on.
        fail(&mut f, now);
        let alert = tick(&mut f, &data, now + 1);
        assert!(
            matches!(
                alert.as_slice(),
                [WhipAction::ForwardFailing { run_id, handle, failures: 3, error }]
                    if run_id == RUN && handle == COORD && error.contains("orca down")
            ),
            "{alert:?}"
        );
        now += 2 * MIN;
        fail(&mut f, now);
        assert!(tick(&mut f, &data, now + 4 * MIN - 1).is_empty());
        now += 4 * MIN;
        let (handle, keys, runs) = forward_of(&tick(&mut f, &data, now));
        assert!(f.take_changed().is_none(), "nothing sent yet");
        f.record(&handle, &keys, &runs, Ok(()), now);
        assert!(f.take_changed().is_some_and(|s| s.contains_key("m1")));
        assert!(tick(&mut f, &data, now + MIN).is_empty());
    }

    #[test]
    fn restored_set_forwards_what_arrived_while_down() {
        let data = clan(
            CLAUDE_IDLE,
            vec![
                msg("seen", WORKER, "worker_done", NOW - 3 * 60 * MIN, "old"),
                msg(
                    "missed",
                    WORKER,
                    "merge_ready",
                    NOW - 2 * 60 * MIN,
                    "while down",
                ),
                msg(
                    "ancient",
                    WORKER,
                    "worker_done",
                    NOW - 7 * 60 * MIN,
                    "history",
                ),
            ],
        );
        let saved: BTreeMap<String, i64> = [("seen".to_string(), NOW - 3 * 60 * MIN)].into();
        let mut f = Forwarder::restored(saved);
        let (_, keys, _) = forward_of(&tick(&mut f, &data, NOW));
        assert_eq!(keys, vec!["missed"]);

        // A fresh forwarder without a saved set replays only the startup window.
        assert!(tick(&mut Forwarder::new(), &data, NOW).is_empty());
    }

    #[test]
    fn forwarded_set_round_trips_and_prunes_old_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(FORWARDED_FILE);
        assert_eq!(load_forwarded(&path), None);
        let sent: BTreeMap<String, i64> = [
            ("m1".to_string(), NOW - MIN),
            ("gone".to_string(), NOW - 7 * 60 * MIN),
        ]
        .into();
        save_forwarded(&path, &sent).unwrap();
        assert_eq!(load_forwarded(&path), Some(sent.clone()));

        // A key no longer visible and older than the event window is dropped.
        let mut f = Forwarder::restored(sent);
        tick(&mut f, &clan(CLAUDE_IDLE, vec![]), NOW);
        let kept = f.take_changed().expect("pruned");
        assert_eq!(kept.keys().collect::<Vec<_>>(), vec!["m1"]);

        fs::write(&path, "not json").unwrap();
        assert_eq!(load_forwarded(&path), None);
    }
}

//! The whip loop: stall findings in, nudges and escalations out.
//!
//! [`WhipEngine::tick`] is a pure state machine over one poll's [`FleetData`]; it decides and
//! sends nothing. [`WhipEngine::execute`] carries the decisions out through a
//! [`WhipExecutor`] (or only logs them in dry-run, except event forwarding, which is always live;
//! see [`WhipAction::live_in_dry_run`]) and records each one in the action log.
//!
//! Per stall target the ladder is: nudge → (cooldown, still stalled) nudge → … → after
//! `max_nudges` nudges and one more cooldown, escalate once, then stay quiet until the target
//! recovers. Recovery (the finding disappears) forgets the target, so a later stall starts
//! fresh. A terminal is nudged at most once per cooldown; everything stalled on it is folded
//! into that one nudge.
//!
//! Typical use from a background thread, every `settings.tick_ms`:
//!
//! ```ignore
//! let actions = engine.tick(&data, &needs_input, &settings, now_ms);
//! engine.execute(&actions, &executor, &settings, now_ms);
//! ```

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::fleet::FleetData;
use crate::whip::actions::{
    NUDGE_PREFIX, NudgeVia, TaskRef, WhipAction, WhipExecutor, WhipLogEntry, coordinator_idle_text,
    missing_report_nudge_text, nudge_message, quiet_run_text, reason_label,
    revoked_worker_nudge_text, target_label, unanswered_ask_text, worker_nudge_text,
};
use crate::whip::forward::Forwarder;
use crate::whip::manual::ManualNudge;
use crate::whip::progress::{ProgressReport, ProgressTracker};
use crate::whip::rules::{RuleContext, StallFinding, StallReason, StallTarget, evaluate};
use crate::whip::settings::WhipSettings;

/// Outcome of an action that was carried out.
const SENT: &str = "sent";

/// Default size of the action log ring buffer.
pub const DEFAULT_LOG_CAPACITY: usize = 500;

/// Escalation state of one stalled target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetState {
    pub first_seen_ms: i64,
    pub nudges_sent: u32,
    pub last_nudge_ms: Option<i64>,
    pub escalated: bool,
}

/// Nudge bookkeeping per terminal (and role), for the per-terminal cooldown and the
/// idle-at-prompt fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalNudge {
    last_nudge_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    Worker,
    Coordinator,
}

type GroupKey = (Role, String);

fn group_key(target: &StallTarget) -> GroupKey {
    let role = match target {
        StallTarget::Worker { .. } => Role::Worker,
        StallTarget::Coordinator { .. } => Role::Coordinator,
    };
    (role, target.handle().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NeedsInputWait {
    since_ms: i64,
    notified: bool,
}

/// Stateful whip. One per app; not thread-safe by itself (wrap it in a `Mutex` or own it on the
/// whip thread).
#[derive(Debug)]
pub struct WhipEngine {
    tracker: ProgressTracker,
    targets: BTreeMap<StallTarget, TargetState>,
    terminals: BTreeMap<GroupKey, TerminalNudge>,
    needs_input: BTreeMap<String, NeedsInputWait>,
    findings: Vec<StallFinding>,
    forwarder: Forwarder,
    log: VecDeque<WhipLogEntry>,
    log_capacity: usize,
}

impl Default for WhipEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl WhipEngine {
    pub fn new() -> Self {
        Self::with_log_capacity(DEFAULT_LOG_CAPACITY)
    }

    pub fn with_log_capacity(log_capacity: usize) -> Self {
        Self {
            tracker: ProgressTracker::new(),
            targets: BTreeMap::new(),
            terminals: BTreeMap::new(),
            needs_input: BTreeMap::new(),
            findings: Vec::new(),
            forwarder: Forwarder::new(),
            log: VecDeque::new(),
            log_capacity: log_capacity.max(1),
        }
    }

    /// Evaluates one poll and returns what to do. Updates escalation state as if every returned
    /// action is carried out (dry-run included), so the ladder advances the same way.
    pub fn tick(
        &mut self,
        data: &FleetData,
        needs_input: &BTreeSet<String>,
        settings: &WhipSettings,
        now_ms: i64,
    ) -> Vec<WhipAction> {
        let report = self.tracker.update(data, now_ms);
        if !settings.enabled {
            self.targets.clear();
            self.terminals.clear();
            self.needs_input.clear();
            self.findings.clear();
            return Vec::new();
        }
        let ctx = RuleContext {
            threshold_ms: settings.threshold_ms,
            abandon_after_ms: settings.abandon_after_ms,
            needs_input,
        };
        let findings = evaluate(&report, data, &ctx, now_ms);

        // Forget recovered targets, and terminals with nothing stalled left.
        let current: BTreeSet<&StallTarget> = findings.iter().map(|f| &f.target).collect();
        self.targets.retain(|t, _| current.contains(t));
        let stalled_groups: BTreeSet<GroupKey> = current.iter().map(|t| group_key(t)).collect();
        self.terminals.retain(|k, _| stalled_groups.contains(k));

        let mut actions = Vec::new();
        let mut due: BTreeMap<GroupKey, Vec<&StallFinding>> = BTreeMap::new();
        for finding in &findings {
            let state = self
                .targets
                .entry(finding.target.clone())
                .or_insert(TargetState {
                    first_seen_ms: now_ms,
                    nudges_sent: 0,
                    last_nudge_ms: None,
                    escalated: false,
                });
            let cooled =
                |last: Option<i64>| last.is_none_or(|l| now_ms - l >= settings.cooldown_ms);
            if state.escalated || !cooled(state.last_nudge_ms) {
                continue;
            }
            if state.nudges_sent >= settings.max_nudges {
                state.escalated = true;
                actions.push(WhipAction::Escalate {
                    target: finding.target.clone(),
                    reason: finding.reason.clone(),
                    evidence: finding.evidence.clone(),
                });
                continue;
            }
            let key = group_key(&finding.target);
            let terminal_last = self.terminals.get(&key).map(|t| t.last_nudge_ms);
            if cooled(terminal_last) {
                due.entry(key).or_default().push(finding);
            }
        }

        for ((role, handle), group) in due {
            let key = (role, handle.clone());
            let previous = self.terminals.get(&key).map(|t| t.last_nudge_ms);
            let mut attempt = 0;
            for f in &group {
                let state = self
                    .targets
                    .get_mut(&f.target)
                    .expect("state inserted above");
                state.nudges_sent += 1;
                state.last_nudge_ms = Some(now_ms);
                attempt = attempt.max(state.nudges_sent);
            }
            self.terminals.insert(
                key,
                TerminalNudge {
                    last_nudge_ms: now_ms,
                },
            );
            let revoked = role == Role::Worker && any_failed_but_alive(&group, &report);
            let via = if revoked
                || (attempt > 1 && idle_since(self.tracker.output_ms(&handle), previous))
            {
                NudgeVia::Terminal {
                    handle: handle.clone(),
                }
            } else {
                bus_route(role, &handle, &group, &report)
            };
            actions.push(WhipAction::Nudge {
                handle,
                targets: group.iter().map(|f| f.target.clone()).collect(),
                attempt,
                text: nudge_text(role, &group, data, revoked),
                via,
            });
        }

        actions.extend(self.track_needs_input(data, needs_input, settings, now_ms));
        if settings.forward_events {
            actions.extend(self.forwarder.tick(data, &report, &findings, now_ms));
        }
        self.findings = findings;
        actions
    }

    /// Carries out `actions`, or in dry-run only logs them, and appends each to the action log.
    /// In dry-run `executor` is called only for actions that are
    /// [`WhipAction::live_in_dry_run`]. Forward outcomes go back to the forwarder, so a failed
    /// send is retried.
    pub fn execute(
        &mut self,
        actions: &[WhipAction],
        executor: &dyn WhipExecutor,
        settings: &WhipSettings,
        now_ms: i64,
    ) {
        for action in actions {
            let dry_run = settings.dry_run && !action.live_in_dry_run();
            let outcome = if dry_run {
                "dry-run".to_string()
            } else {
                perform(action, executor, settings)
            };
            if let WhipAction::Forward {
                handle,
                run_ids,
                keys,
                ..
            } = action
            {
                let result = if outcome == SENT {
                    Ok(())
                } else {
                    Err(outcome.as_str())
                };
                self.forwarder.record(handle, keys, run_ids, result, now_ms);
            }
            if let WhipAction::Nudge { handle, .. } = action
                && outcome == SENT
            {
                self.tracker.record_nudge(handle, now_ms);
            }
            let entry = WhipLogEntry {
                at_ms: now_ms,
                action: action.summary(),
                target: action.target_label(),
                reason: action_reason(action),
                dry_run,
                outcome,
                manual: false,
            };
            log::info!(
                "whip: {} → {} ({}): {} [{}]",
                entry.action,
                entry.target,
                entry.outcome,
                entry.reason,
                if entry.dry_run { "dry-run" } else { "live" }
            );
            self.push_log(entry);
        }
    }

    /// Sends a manual nudge now through `executor` (never dry-run: Karl asked for it) and logs
    /// it marked manual. A sent nudge is recorded like one of the engine's own: its echo is not
    /// taken for progress, and each stalled target it covers climbs the escalation ladder and
    /// starts a new cooldown.
    pub fn execute_manual(
        &mut self,
        nudge: &ManualNudge,
        executor: &dyn WhipExecutor,
        now_ms: i64,
    ) -> WhipLogEntry {
        let outcome = match send_nudge(&nudge.via, &nudge.targets, &nudge.text, executor) {
            Ok(()) => SENT.to_string(),
            Err(e) => {
                log::warn!("whip: manual nudge to {} failed: {e:#}", nudge.handle);
                format!("failed: {e:#}")
            }
        };
        if outcome == SENT {
            self.record_manual(nudge, now_ms);
        }
        let entry = WhipLogEntry {
            at_ms: now_ms,
            action: format!("nudge via {}", nudge.via.label()),
            target: nudge.target_label(),
            reason: nudge.text.clone(),
            dry_run: false,
            outcome,
            manual: true,
        };
        log::info!(
            "whip: manual {} → {} ({}): {}",
            entry.action,
            entry.target,
            entry.outcome,
            entry.reason
        );
        self.push_log(entry.clone());
        entry
    }

    fn record_manual(&mut self, nudge: &ManualNudge, now_ms: i64) {
        self.tracker.record_nudge(&nudge.handle, now_ms);
        for target in &nudge.targets {
            let state = self.targets.entry(target.clone()).or_insert(TargetState {
                first_seen_ms: now_ms,
                nudges_sent: 0,
                last_nudge_ms: None,
                escalated: false,
            });
            state.nudges_sent += 1;
            state.last_nudge_ms = Some(now_ms);
            self.terminals.insert(
                group_key(target),
                TerminalNudge {
                    last_nudge_ms: now_ms,
                },
            );
        }
    }

    /// Starts forwarding from a saved forwarded set instead of the startup replay window; see
    /// [`crate::whip::forward::load_forwarded`].
    pub fn restore_forwarded(&mut self, sent: BTreeMap<String, i64>) {
        self.forwarder = Forwarder::restored(sent);
    }

    /// The forwarded set when it changed since the last call, to be saved.
    pub fn take_forwarded_changes(&mut self) -> Option<BTreeMap<String, i64>> {
        self.forwarder.take_changed()
    }

    fn push_log(&mut self, entry: WhipLogEntry) {
        if self.log.len() >= self.log_capacity {
            self.log.pop_front();
        }
        self.log.push_back(entry);
    }

    /// Action log, oldest first, at most the log capacity.
    pub fn log(&self) -> &VecDeque<WhipLogEntry> {
        &self.log
    }

    /// Findings of the last tick.
    pub fn findings(&self) -> &[StallFinding] {
        &self.findings
    }

    /// Escalation state of a currently stalled target.
    pub fn target_state(&self, target: &StallTarget) -> Option<&TargetState> {
        self.targets.get(target)
    }

    /// Targets escalated and not yet recovered (red nodes).
    pub fn escalated(&self) -> impl Iterator<Item = &StallTarget> {
        self.targets
            .iter()
            .filter(|(_, s)| s.escalated)
            .map(|(t, _)| t)
    }

    /// Terminal handles with an escalated target.
    pub fn escalated_handles(&self) -> BTreeSet<String> {
        self.escalated().map(|t| t.handle().to_string()).collect()
    }

    fn track_needs_input(
        &mut self,
        data: &FleetData,
        needs_input: &BTreeSet<String>,
        settings: &WhipSettings,
        now_ms: i64,
    ) -> Vec<WhipAction> {
        self.needs_input.retain(|h, _| needs_input.contains(h));
        let mut actions = Vec::new();
        for handle in needs_input {
            let wait = self.needs_input.entry(handle.clone()).or_insert_with(|| {
                // The wait began at the terminal's last output, when that is known.
                let since = last_output_ms(data, handle)
                    .filter(|&t| t <= now_ms)
                    .unwrap_or(now_ms);
                NeedsInputWait {
                    since_ms: since,
                    notified: false,
                }
            });
            if !wait.notified && now_ms - wait.since_ms > settings.threshold_ms {
                wait.notified = true;
                actions.push(WhipAction::NotifyNeedsInput {
                    handle: handle.clone(),
                    since_ms: wait.since_ms,
                });
            }
        }
        actions
    }
}

fn last_output_ms(data: &FleetData, handle: &str) -> Option<i64> {
    data.terminal(handle)?.last_output_at.as_ref()?.epoch_ms()
}

/// The terminal (last output `output_ms`, nudge echoes excluded) has printed nothing since the
/// previous nudge, so the bus nudge did not wake it.
fn idle_since(output_ms: Option<i64>, previous_nudge_ms: Option<i64>) -> bool {
    let Some(prev) = previous_nudge_ms else {
        return false;
    };
    output_ms.is_none_or(|out| out <= prev)
}

/// Workers get `dispatch:<id>` when the nudge covers one task with a known dispatch, otherwise
/// their terminal handle. Coordinators get their terminal handle.
fn bus_route(
    role: Role,
    handle: &str,
    group: &[&StallFinding],
    report: &ProgressReport,
) -> NudgeVia {
    let run_id = match group {
        [one] => Some(one.target.run_id().to_string()),
        _ => None,
    };
    let dispatch = match (role, group) {
        (Role::Worker, [one]) => match &one.target {
            StallTarget::Worker { task_id, .. } => report
                .tasks
                .iter()
                .find(|t| &t.task_id == task_id)
                .and_then(|t| t.dispatch_id.clone()),
            StallTarget::Coordinator { .. } => None,
        },
        _ => None,
    };
    NudgeVia::Bus {
        to: dispatch.map_or_else(|| handle.to_string(), |d| format!("dispatch:{d}")),
        run_id,
    }
}

/// A worker in `group` whose task Orca failed while it kept working: its dispatch is revoked,
/// so a bus nudge to it would be rejected and it cannot heartbeat.
fn any_failed_but_alive(group: &[&StallFinding], report: &ProgressReport) -> bool {
    group.iter().any(|f| match &f.target {
        StallTarget::Worker { task_id, .. } => report
            .tasks
            .iter()
            .any(|t| &t.task_id == task_id && t.failed_but_alive),
        StallTarget::Coordinator { .. } => false,
    })
}

fn nudge_text(role: Role, group: &[&StallFinding], data: &FleetData, revoked: bool) -> String {
    let idle = group.iter().map(|f| f.idle_for_ms).max().unwrap_or(0);
    match role {
        Role::Worker => {
            let tasks: Vec<TaskRef> = group
                .iter()
                .filter_map(|f| match &f.target {
                    StallTarget::Worker {
                        task_id, run_id, ..
                    } => Some(task_ref(data, run_id, task_id)),
                    StallTarget::Coordinator { .. } => None,
                })
                .collect();
            if revoked {
                revoked_worker_nudge_text(&tasks, idle)
            } else if group.iter().all(|f| f.reason == StallReason::MissingReport) {
                missing_report_nudge_text(&tasks, idle)
            } else {
                worker_nudge_text(&tasks, idle)
            }
        }
        Role::Coordinator => {
            let texts: Vec<String> = group.iter().map(|f| coordinator_text(f)).collect();
            let mut out = texts[0].clone();
            for more in &texts[1..] {
                out.push(' ');
                out.push_str(more.trim_start_matches(NUDGE_PREFIX).trim_start());
            }
            out
        }
    }
}

fn coordinator_text(f: &StallFinding) -> String {
    let run_id = f.target.run_id();
    match &f.reason {
        StallReason::CoordinatorIdle {
            ready_tasks,
            finished_tasks,
        } => coordinator_idle_text(run_id, ready_tasks, finished_tasks, f.idle_for_ms),
        StallReason::UnansweredAsk { message_id } => {
            unanswered_ask_text(run_id, message_id, f.idle_for_ms)
        }
        StallReason::QuietNoTasks | StallReason::WorkerNoProgress | StallReason::MissingReport => {
            quiet_run_text(run_id, f.idle_for_ms)
        }
    }
}

fn task_ref(data: &FleetData, run_id: &str, task_id: &str) -> TaskRef {
    let title = data
        .tasks
        .get(run_id)
        .and_then(|ts| ts.iter().find(|t| t.id == task_id))
        .map(|t| t.label().to_string())
        .filter(|l| l != task_id);
    TaskRef {
        id: task_id.to_string(),
        title,
    }
}

fn perform(action: &WhipAction, executor: &dyn WhipExecutor, settings: &WhipSettings) -> String {
    let result = match action {
        WhipAction::Nudge {
            targets, via, text, ..
        } => send_nudge(via, targets, text, executor),
        WhipAction::Escalate {
            target, evidence, ..
        } => {
            if !settings.notify {
                return "notifications off".into();
            }
            executor.notify(
                &format!("Myrstack: {} is stalled", target_label(target)),
                evidence,
            );
            Ok(())
        }
        WhipAction::NotifyNeedsInput { handle, .. } => {
            if !settings.notify {
                return "notifications off".into();
            }
            executor.notify(
                &format!("Myrstack: {handle} is waiting on you"),
                &action_reason(action),
            );
            Ok(())
        }
        WhipAction::Forward { handle, text, .. } => executor.send_terminal(handle, text),
        WhipAction::CoordinatorUnreachable { run_id, .. } => {
            if !settings.notify {
                return "notifications off".into();
            }
            executor.notify(
                &format!("Myrstack: coordinator of run {run_id} is unreachable"),
                &action_reason(action),
            );
            Ok(())
        }
        WhipAction::ForwardFailing { run_id, .. } => {
            if !settings.notify {
                return "notifications off".into();
            }
            executor.notify(
                &format!("Myrstack: cannot forward run {run_id}'s events"),
                &action_reason(action),
            );
            Ok(())
        }
    };
    match result {
        Ok(()) => SENT.into(),
        Err(e) => {
            log::warn!("whip: {} failed: {e:#}", action.summary());
            format!("failed: {e:#}")
        }
    }
}

fn send_nudge(
    via: &NudgeVia,
    targets: &[StallTarget],
    text: &str,
    executor: &dyn WhipExecutor,
) -> anyhow::Result<()> {
    match via {
        NudgeVia::Bus { to, run_id } => {
            executor.send_bus(&nudge_message(to, run_id.as_deref(), targets, text))
        }
        NudgeVia::Terminal { handle } => executor.send_terminal(handle, text),
    }
}

fn action_reason(action: &WhipAction) -> String {
    match action {
        WhipAction::Nudge { text, .. } => text.clone(),
        WhipAction::Escalate {
            reason, evidence, ..
        } => format!("{}: {evidence}", reason_label(reason)),
        WhipAction::NotifyNeedsInput { handle, since_ms } => {
            format!("{handle} needs input since {since_ms} (epoch ms)")
        }
        WhipAction::Forward { text, .. } => text.clone(),
        WhipAction::CoordinatorUnreachable { waiting, first, .. } => format!(
            "{waiting} event(s) waiting and no live Claude session to forward them to, \
             first: {first}"
        ),
        WhipAction::ForwardFailing {
            handle,
            failures,
            error,
            ..
        } => format!(
            "{failures} sends in a row to coordinator {handle} failed, still retrying; \
             last: {error}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::{InboxMessage, OutgoingMessage, Run, Task, Terminal, Timestamp, Worker};
    use crate::whip::actions::DryRunExecutor;
    use serde_json::json;
    use std::sync::Mutex;

    const RUN: &str = "run_1";
    const COORD: &str = "term_coord";
    const WORKER: &str = "term_worker";
    const MIN: i64 = 60_000;
    /// Everything was last active at T0.
    const T0: i64 = 1_790_000_000_000;

    /// Records calls; used to prove what was (not) sent.
    #[derive(Default)]
    struct FakeExecutor {
        calls: Mutex<Vec<String>>,
        fail_bus: bool,
        /// Terminal sends fail while this is set.
        fail_terminal: std::sync::atomic::AtomicBool,
    }

    impl FakeExecutor {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl WhipExecutor for FakeExecutor {
        fn send_bus(&self, msg: &OutgoingMessage) -> anyhow::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("bus {}", msg.to.as_deref().unwrap_or("-")));
            if self.fail_bus {
                anyhow::bail!("boom");
            }
            Ok(())
        }
        fn send_terminal(&self, handle: &str, _text: &str) -> anyhow::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("terminal {handle}"));
            if self
                .fail_terminal
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                anyhow::bail!("pane gone");
            }
            Ok(())
        }
        fn notify(&self, title: &str, _body: &str) {
            self.calls.lock().unwrap().push(format!("notify {title}"));
        }
    }

    fn terminal(handle: &str, last_output: i64) -> Terminal {
        Terminal {
            handle: handle.into(),
            last_output_at: Some(Timestamp::Millis(last_output)),
            ..Default::default()
        }
    }

    fn task(id: &str, status: &str, title: &str) -> Task {
        serde_json::from_value(json!({
            "id": id, "run_id": RUN, "status": status, "display_name": title,
            "created_by_terminal_handle": COORD, "created_at": T0 - 60 * MIN,
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

    fn heartbeat(id: &str, task_id: &str, from: &str, at: i64) -> InboxMessage {
        serde_json::from_value(json!({
            "id": id, "run_id": RUN, "from_handle": from, "to_handle": format!("run:{RUN}"),
            "type": "heartbeat", "subject": "alive", "created_at": at,
            "payload": json!({"taskId": task_id, "dispatchId": format!("ctx_{task_id}"),
                              "phase": "implementing"}).to_string(),
        }))
        .unwrap()
    }

    /// One Run, a live coordinator, and a worker on dispatched task `t1` that went silent at T0.
    fn fleet() -> FleetData {
        FleetData {
            polled_at_ms: T0,
            terminals: vec![terminal(COORD, T0), terminal(WORKER, T0)],
            runs: vec![
                serde_json::from_value::<Run>(json!({
                    "id": RUN, "coordinator_handle": COORD,
                    "created_at": T0 - 60 * MIN, "updated_at": T0,
                }))
                .unwrap(),
            ],
            workers: vec![worker("t1", WORKER)],
            tasks: [(RUN.to_string(), vec![task("t1", "dispatched", "Fix login")])].into(),
            inbox: vec![heartbeat("m1", "t1", WORKER, T0)],
            ..Default::default()
        }
    }

    fn worker_target(task_id: &str) -> StallTarget {
        StallTarget::Worker {
            handle: WORKER.into(),
            task_id: task_id.into(),
            run_id: RUN.into(),
        }
    }

    fn settings() -> WhipSettings {
        WhipSettings {
            dry_run: false,
            ..Default::default()
        }
    }

    /// Ticks every minute from `from` to `to` minutes after T0 (inclusive); returns
    /// `(minute, action)` for every action.
    fn run_minutes(
        engine: &mut WhipEngine,
        data: &FleetData,
        needs_input: &BTreeSet<String>,
        from: i64,
        to: i64,
    ) -> Vec<(i64, WhipAction)> {
        let s = settings();
        let mut out = Vec::new();
        for m in from..=to {
            for a in engine.tick(data, needs_input, &s, T0 + m * MIN) {
                out.push((m, a));
            }
        }
        out
    }

    fn kinds(actions: &[(i64, WhipAction)]) -> Vec<(i64, String)> {
        actions.iter().map(|(m, a)| (*m, a.summary())).collect()
    }

    #[test]
    fn failed_but_alive_worker_is_nudged_by_terminal() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        let task = &mut data.tasks.get_mut(RUN).unwrap()[0];
        task.status = crate::orca::TaskStatus::Failed;
        task.completed_at = Some(Timestamp::Millis(T0 - 30 * MIN));
        data.workers[0].dispatch_status = Some(crate::orca::DispatchStatus::Failed);
        let actions = run_minutes(&mut engine, &data, &BTreeSet::new(), 0, 20);
        let nudges: Vec<&WhipAction> = actions
            .iter()
            .map(|(_, a)| a)
            .filter(|a| matches!(a, WhipAction::Nudge { handle, .. } if handle == WORKER))
            .collect();
        let [WhipAction::Nudge { via, text, .. }] = nudges.as_slice() else {
            panic!("expected one nudge, got {actions:?}");
        };
        assert_eq!(
            via,
            &NudgeVia::Terminal {
                handle: WORKER.into()
            }
        );
        assert!(text.contains("Orca marked this dispatch failed"), "{text}");
    }

    #[test]
    fn paused_worker_ladder() {
        let mut engine = WhipEngine::new();
        let data = fleet();
        let none = BTreeSet::new();
        let actions = run_minutes(&mut engine, &data, &none, 0, 60);
        // Stalled after 15 min → nudge at 16; 10-min cooldown → nudge #2 at 26 (the terminal has
        // printed nothing since #1, so it is typed); escalate at 36; then quiet.
        assert_eq!(
            kinds(&actions),
            vec![
                (16, "nudge #1 via bus to dispatch:ctx_t1".to_string()),
                (26, "nudge #2 via terminal input".to_string()),
                (36, "escalate".to_string()),
            ]
        );
        let WhipAction::Nudge { text, targets, .. } = &actions[0].1 else {
            panic!("expected nudge");
        };
        assert_eq!(targets, &vec![worker_target("t1")]);
        assert!(
            text.starts_with("Myrstack: no progress on task t1 (Fix login) for 16m."),
            "{text}"
        );
        assert_eq!(
            engine.escalated().cloned().collect::<Vec<_>>(),
            vec![worker_target("t1")]
        );
        assert_eq!(
            engine.escalated_handles(),
            BTreeSet::from([WORKER.to_string()])
        );
        let state = engine.target_state(&worker_target("t1")).unwrap();
        assert_eq!((state.nudges_sent, state.first_seen_ms), (2, T0 + 16 * MIN));
    }

    #[test]
    fn terminal_fallback_only_when_idle_since_last_nudge() {
        // First nudge: never the terminal.
        assert!(!idle_since(Some(T0), None));
        // Last output at T0, nudged at T0 + 16: no reaction.
        assert!(idle_since(Some(T0), Some(T0 + 16 * MIN)));
        // Output after the nudge: the terminal is busy, keep using the bus.
        assert!(!idle_since(Some(T0 + 17 * MIN), Some(T0 + 16 * MIN)));
        // Unknown output time counts as idle.
        assert!(idle_since(None, Some(T0 + 16 * MIN)));
    }

    /// Ticks and executes live every minute from `from` to `to` (inclusive), with the worker's
    /// terminal printing `echo_after_ms` after every nudge sent to it.
    fn run_live_with_echo(
        engine: &mut WhipEngine,
        data: &mut FleetData,
        exec: &FakeExecutor,
        from: i64,
        to: i64,
        echo_after_ms: i64,
    ) -> Vec<(i64, String)> {
        let s = settings();
        let mut out = Vec::new();
        for m in from..=to {
            let now = T0 + m * MIN;
            let actions = engine.tick(data, &BTreeSet::new(), &s, now);
            engine.execute(&actions, exec, &s, now);
            for a in &actions {
                if let WhipAction::Nudge { handle, .. } = a {
                    let term = data.terminals.iter_mut().find(|t| &t.handle == handle);
                    term.unwrap().last_output_at = Some(Timestamp::Millis(now + echo_after_ms));
                }
                out.push((m, a.summary()));
            }
        }
        out
    }

    #[test]
    fn nudge_echo_does_not_reset_the_ladder() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        let exec = FakeExecutor::default();
        // Every nudge prints into the worker's pane 3 s later; the worker does nothing else.
        let actions = run_live_with_echo(&mut engine, &mut data, &exec, 0, 40, 3_000);
        assert_eq!(
            actions,
            vec![
                (16, "nudge #1 via bus to dispatch:ctx_t1".to_string()),
                (26, "nudge #2 via terminal input".to_string()),
                (36, "escalate".to_string()),
            ]
        );
    }

    #[test]
    fn late_output_after_a_nudge_is_progress() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        let exec = FakeExecutor::default();
        // The worker wakes up and prints 2 minutes after the nudge: real progress.
        let actions = run_live_with_echo(&mut engine, &mut data, &exec, 0, 30, 2 * MIN);
        assert_eq!(
            actions,
            vec![(16, "nudge #1 via bus to dispatch:ctx_t1".to_string())]
        );
        assert!(engine.target_state(&worker_target("t1")).is_none());
    }

    #[test]
    fn dry_run_nudges_do_not_hide_output() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        let s = WhipSettings::default();
        let exec = DryRunExecutor::new();
        for m in 0..=16 {
            let now = T0 + m * MIN;
            let actions = engine.tick(&data, &BTreeSet::new(), &s, now);
            engine.execute(&actions, &exec, &s, now);
        }
        // Nothing was sent, so output right after the dry-run nudge is the agent's own.
        data.terminals[1].last_output_at = Some(Timestamp::Millis(T0 + 16 * MIN + 1_000));
        engine.tick(&data, &BTreeSet::new(), &s, T0 + 17 * MIN);
        assert!(engine.target_state(&worker_target("t1")).is_none());
    }

    #[test]
    fn recovery_resets_the_ladder() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        let none = BTreeSet::new();
        let first = run_minutes(&mut engine, &data, &none, 0, 40);
        assert_eq!(first.len(), 3);
        // A heartbeat at 41 clears the stall and the escalation.
        data.inbox
            .insert(0, heartbeat("m2", "t1", WORKER, T0 + 41 * MIN));
        assert!(run_minutes(&mut engine, &data, &none, 41, 41).is_empty());
        assert!(engine.target_state(&worker_target("t1")).is_none());
        assert_eq!(engine.escalated().count(), 0);
        // The next stall starts at nudge #1 again.
        let again = run_minutes(&mut engine, &data, &none, 42, 57);
        assert_eq!(
            kinds(&again),
            vec![(57, "nudge #1 via bus to dispatch:ctx_t1".to_string())]
        );
    }

    #[test]
    fn folds_tasks_on_one_terminal() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        data.workers.push(worker("t2", WORKER));
        data.tasks
            .get_mut(RUN)
            .unwrap()
            .push(task("t2", "dispatched", "Docs"));
        data.inbox.push(heartbeat("m0", "t2", WORKER, T0 - MIN));
        let none = BTreeSet::new();
        let actions = run_minutes(&mut engine, &data, &none, 0, 16);
        assert_eq!(actions.len(), 1, "{actions:?}");
        let WhipAction::Nudge {
            handle,
            targets,
            via,
            text,
            attempt,
        } = &actions[0].1
        else {
            panic!("expected nudge");
        };
        assert_eq!(handle, WORKER);
        assert_eq!(*attempt, 1);
        assert_eq!(targets.len(), 2);
        // Two dispatches: address the terminal, not one dispatch.
        assert_eq!(
            via,
            &NudgeVia::Bus {
                to: WORKER.into(),
                run_id: None
            }
        );
        assert!(text.contains("tasks t1 (Fix login), t2 (Docs)"), "{text}");
    }

    #[test]
    fn late_stall_on_same_terminal_waits_for_the_terminal_cooldown() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        data.workers.push(worker("t2", WORKER));
        data.tasks
            .get_mut(RUN)
            .unwrap()
            .push(task("t2", "dispatched", "Docs"));
        // t2 stalls 3 minutes after t1.
        data.inbox.push(heartbeat("m0", "t2", WORKER, T0 + 3 * MIN));
        let none = BTreeSet::new();
        let actions = run_minutes(&mut engine, &data, &none, 0, 30);
        let nudges: Vec<(i64, usize)> = actions
            .iter()
            .filter_map(|(m, a)| match a {
                WhipAction::Nudge { targets, .. } => Some((*m, targets.len())),
                _ => None,
            })
            .collect();
        // t1 alone at 16; t2 is not nudged separately at 19 but folded into the 26 nudge.
        assert_eq!(nudges, vec![(16, 1), (26, 2)]);
    }

    #[test]
    fn coordinator_variants() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        // t1 is done; a ready task t2 waits; the coordinator stopped at T0.
        data.workers.clear();
        data.tasks.insert(
            RUN.into(),
            vec![
                task("t1", "completed", "Fix login"),
                task("t2", "ready", "Docs"),
            ],
        );
        data.tasks.get_mut(RUN).unwrap()[0].completed_at = Some(Timestamp::Millis(T0 - 5 * MIN));
        data.inbox = vec![];
        let none = BTreeSet::new();
        let actions = run_minutes(&mut engine, &data, &none, 0, 40);
        assert_eq!(
            kinds(&actions),
            vec![
                (16, format!("nudge #1 via bus to {COORD}")),
                (26, "nudge #2 via terminal input".to_string()),
                (36, "escalate".to_string()),
            ]
        );
        let WhipAction::Nudge { text, via, .. } = &actions[0].1 else {
            panic!("expected nudge");
        };
        assert_eq!(
            via,
            &NudgeVia::Bus {
                to: COORD.into(),
                run_id: Some(RUN.into())
            }
        );
        assert!(text.starts_with("Myrstack: run run_1 has had no coordinator activity"));
        assert!(text.contains("ready task(s) not started: t2"), "{text}");
    }

    #[test]
    fn coordinator_texts_by_reason() {
        let coord = StallTarget::Coordinator {
            handle: COORD.into(),
            run_id: RUN.into(),
        };
        let finding = |reason| StallFinding {
            target: coord.clone(),
            reason,
            idle_for_ms: 20 * MIN,
            evidence: String::new(),
        };
        let ask = coordinator_text(&finding(StallReason::UnansweredAsk {
            message_id: "msg_q".into(),
        }));
        assert!(ask.contains("question msg_q"), "{ask}");
        assert!(ask.contains("blocked"));
        let quiet = coordinator_text(&finding(StallReason::QuietNoTasks));
        assert!(quiet.starts_with("Myrstack: run run_1 has been quiet for 20m"));
        // Two Runs on one coordinator fold into one text with one prefix.
        let a = finding(StallReason::QuietNoTasks);
        let mut b = finding(StallReason::QuietNoTasks);
        b.target = StallTarget::Coordinator {
            handle: COORD.into(),
            run_id: "run_2".into(),
        };
        let folded = nudge_text(Role::Coordinator, &[&a, &b], &FleetData::default(), false);
        assert_eq!(folded.matches(NUDGE_PREFIX).count(), 1, "{folded}");
        assert!(folded.contains("run run_2"));
    }

    #[test]
    fn needs_input_is_never_nudged_but_notified_once_per_wait() {
        let mut engine = WhipEngine::new();
        let data = fleet();
        let waiting = BTreeSet::from([WORKER.to_string()]);
        let actions = run_minutes(&mut engine, &data, &waiting, 0, 60);
        // Waiting since its last output at T0: notify once after the threshold.
        assert_eq!(
            actions,
            vec![(
                16,
                WhipAction::NotifyNeedsInput {
                    handle: WORKER.into(),
                    since_ms: T0
                }
            )]
        );
        // Answered, then waiting again: a new wait, a new notification.
        let none = BTreeSet::new();
        engine.tick(&data, &none, &settings(), T0 + 61 * MIN);
        let mut fresh = data.clone();
        fresh.terminals[1].last_output_at = Some(Timestamp::Millis(T0 + 62 * MIN));
        fresh
            .inbox
            .insert(0, heartbeat("m2", "t1", WORKER, T0 + 62 * MIN));
        let again = run_minutes(&mut engine, &fresh, &waiting, 62, 90);
        assert_eq!(kinds(&again), vec![(78, "notify needs-input".to_string())]);
    }

    #[test]
    fn disabled_does_nothing() {
        let mut engine = WhipEngine::new();
        let s = WhipSettings {
            enabled: false,
            ..settings()
        };
        for m in 0..60 {
            assert!(
                engine
                    .tick(&fleet(), &BTreeSet::new(), &s, T0 + m * MIN)
                    .is_empty()
            );
        }
    }

    #[test]
    fn dry_run_sends_nothing_but_logs() {
        let mut engine = WhipEngine::new();
        let data = fleet();
        let exec = FakeExecutor::default();
        let s = WhipSettings::default();
        assert!(s.dry_run);
        for m in 0..=40 {
            let now = T0 + m * MIN;
            let actions = engine.tick(&data, &BTreeSet::new(), &s, now);
            engine.execute(&actions, &exec, &s, now);
        }
        assert!(exec.calls().is_empty(), "{:?}", exec.calls());
        let log: Vec<(&str, bool, &str)> = engine
            .log()
            .iter()
            .map(|e| (e.action.as_str(), e.dry_run, e.outcome.as_str()))
            .collect();
        assert_eq!(
            log,
            vec![
                ("nudge #1 via bus to dispatch:ctx_t1", true, "dry-run"),
                ("nudge #2 via terminal input", true, "dry-run"),
                ("escalate", true, "dry-run"),
            ]
        );
        assert_eq!(engine.log()[0].target, "worker term_worker task t1");
        assert!(engine.log()[0].reason.starts_with("Myrstack:"));
    }

    #[test]
    fn live_execute_calls_executor_and_records_outcomes() {
        let mut engine = WhipEngine::new();
        let s = settings();
        let exec = FakeExecutor {
            fail_bus: true,
            ..Default::default()
        };
        let data = fleet();
        for m in 0..=40 {
            let now = T0 + m * MIN;
            let actions = engine.tick(&data, &BTreeSet::new(), &s, now);
            engine.execute(&actions, &exec, &s, now);
        }
        assert_eq!(
            exec.calls(),
            vec![
                "bus dispatch:ctx_t1".to_string(),
                format!("terminal {WORKER}"),
                format!("notify Myrstack: worker {WORKER} task t1 is stalled"),
            ]
        );
        let outcomes: Vec<&str> = engine.log().iter().map(|e| e.outcome.as_str()).collect();
        assert_eq!(outcomes, vec!["failed: boom", "sent", "sent"]);

        // Notifications can be switched off.
        let quiet = WhipSettings { notify: false, ..s };
        let exec = DryRunExecutor::new();
        let action = WhipAction::NotifyNeedsInput {
            handle: WORKER.into(),
            since_ms: T0,
        };
        engine.execute(&[action], &exec, &quiet, T0);
        assert!(exec.calls().is_empty());
        assert_eq!(engine.log().back().unwrap().outcome, "notifications off");
    }

    /// A live Claude coordinator, no stalls, and a fresh `worker_done` on the bus.
    fn fleet_with_report() -> FleetData {
        let mut data = fleet();
        data.terminals[0].agent_identity = Some(crate::orca::AgentIdentity::Claude);
        data.terminals[0].preview = Some("⏺ Dispatched.\nnew task? /clear to save 120k".into());
        data.inbox.push(
            serde_json::from_value(json!({
                "id": "done1", "run_id": RUN, "from_handle": WORKER,
                "to_handle": format!("run:{RUN}"), "type": "worker_done", "subject": "PR open",
                "created_at": T0,
                "payload": json!({"taskId": "t1", "dispatchId": "ctx_t1"}).to_string(),
            }))
            .unwrap(),
        );
        data
    }

    #[test]
    fn missing_report_nudges_the_worker_and_tells_the_coordinator_once() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        for t in &mut data.terminals {
            t.agent_identity = Some(crate::orca::AgentIdentity::Claude);
            t.preview = Some("⏺ Done.\nnew task? /clear to save 80k tokens".into());
        }
        let exec = FakeExecutor::default();
        let s = settings();
        let mut out = Vec::new();
        for m in 0..=40 {
            let now = T0 + m * MIN;
            let actions = engine.tick(&data, &BTreeSet::new(), &s, now);
            engine.execute(&actions, &exec, &s, now);
            out.extend(actions.into_iter().map(|a| (m, a)));
        }
        assert_eq!(
            kinds(&out),
            vec![
                (16, "nudge #1 via bus to dispatch:ctx_t1".to_string()),
                (16, "forward 1 event(s)".to_string()),
                (26, "nudge #2 via terminal input".to_string()),
                (36, "escalate".to_string()),
            ]
        );
        let texts: Vec<&str> = out
            .iter()
            .filter_map(|(_, a)| match a {
                WhipAction::Nudge { text, .. } | WhipAction::Forward { text, .. } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(
            texts[0].contains("your report to the coordinator never arrived on the bus"),
            "{}",
            texts[0]
        );
        assert!(
            texts[1]
                .contains("worker term_worker looks finished on task Fix login but never reported"),
            "{}",
            texts[1]
        );
        assert!(matches!(&out[1].1, WhipAction::Forward { handle, .. } if handle == COORD));
    }

    #[test]
    fn dry_run_still_forwards_events() {
        let mut engine = WhipEngine::new();
        let data = fleet_with_report();
        let exec = FakeExecutor::default();
        let s = WhipSettings::default();
        assert!(s.dry_run);
        for m in 0..3 {
            let now = T0 + m * MIN;
            let actions = engine.tick(&data, &BTreeSet::new(), &s, now);
            engine.execute(&actions, &exec, &s, now);
        }
        assert_eq!(exec.calls(), vec![format!("terminal {COORD}")]);
        let entry = engine.log().back().unwrap();
        assert_eq!(entry.action, "forward 1 event(s)");
        assert!(!entry.dry_run && entry.outcome == SENT, "{entry:?}");
        assert!(
            engine
                .take_forwarded_changes()
                .is_some_and(|s| s.contains_key("done1"))
        );
    }

    #[test]
    fn failed_forward_is_retried_until_it_lands() {
        use std::sync::atomic::Ordering;
        let mut engine = WhipEngine::new();
        let data = fleet_with_report();
        let exec = FakeExecutor::default();
        exec.fail_terminal.store(true, Ordering::Relaxed);
        let s = settings();
        let mut run = |m: i64| {
            let now = T0 + m * MIN;
            let actions = engine.tick(&data, &BTreeSet::new(), &s, now);
            engine.execute(&actions, &exec, &s, now);
        };
        run(0);
        run(1);
        exec.fail_terminal.store(false, Ordering::Relaxed);
        run(2);
        run(3);
        run(4);
        let forwards = exec
            .calls()
            .iter()
            .filter(|c| *c == &format!("terminal {COORD}"))
            .count();
        assert_eq!(forwards, 3, "{:?}", exec.calls());
        let outcomes: Vec<String> = engine
            .log()
            .iter()
            .filter(|e| e.action.starts_with("forward"))
            .map(|e| e.outcome.clone())
            .collect();
        assert_eq!(
            outcomes,
            vec!["failed: pane gone", "failed: pane gone", SENT]
        );
    }

    #[test]
    fn log_is_bounded() {
        let mut engine = WhipEngine::with_log_capacity(3);
        let exec = DryRunExecutor::new();
        let s = WhipSettings::default();
        for i in 0..5 {
            let action = WhipAction::NotifyNeedsInput {
                handle: format!("term_{i}"),
                since_ms: T0,
            };
            engine.execute(&[action], &exec, &s, T0 + i);
        }
        let targets: Vec<&str> = engine.log().iter().map(|e| e.target.as_str()).collect();
        assert_eq!(
            targets,
            vec!["terminal term_2", "terminal term_3", "terminal term_4"]
        );
    }

    fn manual_worker_nudge() -> ManualNudge {
        ManualNudge {
            node: crate::model::NodeId::terminal(WORKER),
            label: WORKER.into(),
            handle: WORKER.into(),
            targets: vec![worker_target("t1")],
            via: NudgeVia::Bus {
                to: "dispatch:ctx_t1".into(),
                run_id: Some(RUN.into()),
            },
            text: "Myrstack: manual".into(),
            warn: None,
        }
    }

    /// Sends the manual nudge at `minute`, with the worker's pane echoing it 3 s later.
    fn manual_at(engine: &mut WhipEngine, data: &mut FleetData, exec: &FakeExecutor, minute: i64) {
        let now = T0 + minute * MIN;
        let entry = engine.execute_manual(&manual_worker_nudge(), exec, now);
        assert_eq!(entry.outcome, "sent");
        data.terminals[1].last_output_at = Some(Timestamp::Millis(now + 3_000));
    }

    #[test]
    fn manual_nudge_bypasses_the_cooldown_and_climbs_the_ladder() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        let exec = FakeExecutor::default();
        let auto = run_live_with_echo(&mut engine, &mut data, &exec, 0, 16, 3_000);
        assert_eq!(
            auto,
            vec![(16, "nudge #1 via bus to dispatch:ctx_t1".to_string())]
        );
        // One minute later, well inside the cooldown: a manual whip still goes out.
        manual_at(&mut engine, &mut data, &exec, 17);
        assert_eq!(
            exec.calls(),
            vec!["bus dispatch:ctx_t1", "bus dispatch:ctx_t1"]
        );
        let state = engine.target_state(&worker_target("t1")).unwrap();
        assert_eq!(
            (state.nudges_sent, state.last_nudge_ms),
            (2, Some(T0 + 17 * MIN))
        );
        let entry = engine.log().back().unwrap();
        assert!(entry.manual && !entry.dry_run);
        assert_eq!(entry.action, "nudge via bus to dispatch:ctx_t1");
        assert_eq!(entry.target, "worker term_worker task t1");
        // Its echo is not progress, and it counts as nudge #2: the next step, one cooldown
        // after the manual nudge, is the escalation.
        let later = run_live_with_echo(&mut engine, &mut data, &exec, 18, 40, 3_000);
        assert_eq!(later, vec![(27, "escalate".to_string())]);
    }

    #[test]
    fn manual_nudge_echo_is_not_progress() {
        let mut engine = WhipEngine::new();
        let mut data = fleet();
        let exec = FakeExecutor::default();
        run_live_with_echo(&mut engine, &mut data, &exec, 0, 4, 3_000);
        manual_at(&mut engine, &mut data, &exec, 5);
        // Without echo suppression the pane output at 5m would push the stall to 21m.
        let auto = run_live_with_echo(&mut engine, &mut data, &exec, 6, 20, 3_000);
        assert_eq!(
            auto,
            vec![(16, "nudge #1 via bus to dispatch:ctx_t1".to_string())]
        );
    }

    #[test]
    fn failed_manual_nudge_is_logged_but_not_recorded() {
        let mut engine = WhipEngine::new();
        let exec = FakeExecutor {
            fail_bus: true,
            ..Default::default()
        };
        let entry = engine.execute_manual(&manual_worker_nudge(), &exec, T0);
        assert!(
            entry.outcome.starts_with("failed: boom"),
            "{}",
            entry.outcome
        );
        assert!(entry.manual);
        assert!(engine.target_state(&worker_target("t1")).is_none());
    }
}

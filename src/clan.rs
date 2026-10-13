//! `ClanData`: one poll's raw Orca data, shared by the poller (producer), the farm model and
//! the whip (consumers).

mod roles;
#[cfg(test)]
mod roles_tests;

use std::collections::{BTreeMap, BTreeSet};

use crate::detect::PromptKind;
use crate::orca::spawn::Worktree;
use crate::orca::{
    DispatchStatus, InboxMessage, Run, Task, TaskStatus, Terminal, Timestamp, Worker,
};

pub use roles::{AttendantSignal, Forward, RunRoles};

/// Terminal output this long after a task failed means its agent is working anyway: an agent
/// that died at launch prints its error within moments and then stays quiet.
pub const REVIVAL_GRACE_MS: i64 = 2 * 60_000;

/// Everything one poll cycle fetched from Orca, unprocessed.
#[derive(Debug, Clone, Default)]
pub struct ClanData {
    /// Wall-clock time the poll finished, in ms since the Unix epoch.
    pub polled_at_ms: i64,
    /// When the terminal list (polled most often) last succeeded; `None` if it never has.
    /// How fresh the data is: see [`crate::stale`].
    pub fresh_at_ms: Option<i64>,
    /// Terminal list calls that failed in a row since it last succeeded.
    pub failed_polls: u32,
    /// Start of the latest successful terminal-list poll.
    pub terminals_polled_at_ms: Option<i64>,
    pub terminals: Vec<Terminal>,
    pub runs: Vec<Run>,
    pub workers: Vec<Worker>,
    /// Tasks per run ID. Only runs the poller considers active are fetched.
    pub tasks: BTreeMap<String, Vec<Task>>,
    /// Recent bus messages, newest first.
    pub inbox: Vec<InboxMessage>,
    /// Errors from this cycle's orca calls, for display. Data from a failed call keeps its
    /// previous value.
    pub errors: Vec<String>,
    /// Screen-detector verdicts for idle agent terminals sampled in the background, by handle:
    /// `Some` when the screen showed a prompt, `None` when it did not. A terminal that was not
    /// sampled, or has produced output since, is absent.
    pub needs_input: BTreeMap<String, Option<PromptKind>>,
    /// What the sampled screen's composer held, by handle, kept and pruned with
    /// [`ClanData::needs_input`]: the preview cannot tell (see
    /// [`crate::detect::composer_state`]).
    pub composers: BTreeMap<String, crate::detect::ComposerState>,
    /// Terminals each Claude terminal forwarded input to with `orca terminal send`, read from
    /// its transcript by the Claude worker, by sender handle. Only declared coordinators are
    /// scanned; see [`ClanData::run_roles`].
    pub forwards: BTreeMap<String, Vec<Forward>>,
    /// Names chosen for worktrees in Orca (by the King at spawn, or renamed in Orca), by
    /// worktree path; see [`chosen_worktree_names`]. Agents in a named worktree go by it.
    pub worktree_names: BTreeMap<String, String>,
    /// Orca's worktree list, for the worktree count and cleanup.
    pub worktrees: Vec<Worktree>,
}

/// The worktrees whose Orca display name was chosen rather than defaulted, by path. Orca
/// names a worktree after its folder or branch, and keeps a main checkout's old branch name,
/// so a name counts only on a linked worktree and when it is neither its folder's nor its
/// branch's name.
pub fn chosen_worktree_names(worktrees: &[Worktree]) -> BTreeMap<String, String> {
    worktrees
        .iter()
        .filter(|w| !w.is_main_worktree && !w.is_archived)
        .filter_map(|w| {
            let name = w.display_name.as_deref()?.trim();
            let path = w.path.trim_end_matches('/');
            let folder = path.rsplit('/').next().unwrap_or(path);
            let defaulted = name.is_empty() || name == folder || Some(name) == w.branch_name();
            (!defaulted).then(|| (path.to_string(), name.to_string()))
        })
        .collect()
}

impl ClanData {
    /// The name chosen for the worktree at `path`, if any.
    pub fn worktree_name(&self, path: Option<&str>) -> Option<&str> {
        let path = path?.trim_end_matches('/');
        self.worktree_names.get(path).map(String::as_str)
    }

    pub fn terminal(&self, handle: &str) -> Option<&Terminal> {
        self.terminals.iter().find(|t| t.handle == handle)
    }

    /// The sampled screen's verdict for [`crate::model::StateInputs::needs_input`]: `None`
    /// when the terminal has no current sample.
    pub fn screen_needs_input(&self, handle: &str) -> Option<bool> {
        self.needs_input.get(handle).map(Option::is_some)
    }

    /// The sampled screen's composer, when the terminal has a current sample.
    pub fn screen_composer(&self, handle: &str) -> Option<crate::detect::ComposerState> {
        self.composers.get(handle).copied()
    }

    pub fn run(&self, run_id: &str) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == run_id)
    }

    /// The terminal actually coordinating a Run: [`RunRoles::coordinator`] of
    /// [`ClanData::run_roles`]. `None` when no live candidate exists.
    pub fn effective_coordinator(&self, run_id: &str) -> Option<String> {
        self.run_roles(run_id).coordinator
    }

    /// Latest sign of life, in ms, of a worker whose task Orca marked failed although the
    /// worker kept working: the Codex launch race (`agent_prompt_blocked`/`_stalled`) fails the
    /// dispatch while the agent starts anyway, and Orca then rejects its heartbeats and reports.
    ///
    /// Counts only a failure with no recorded result (a coordinator retiring a task writes its
    /// reason there) on a terminal no newer dispatch has taken over. Signs of life are the
    /// worker's messages about the task after the failure, rejected ones included, and
    /// `output_ms` (its terminal's last output) more than [`REVIVAL_GRACE_MS`] after it.
    pub fn alive_after_failure(&self, task: &Task, output_ms: Option<i64>) -> Option<i64> {
        if task.status != TaskStatus::Failed || task_has_result(task) {
            return None;
        }
        let failed_at = ts_ms(&task.completed_at)?;
        let worker = self
            .workers
            .iter()
            .filter(|w| w.task_id.as_deref() == Some(task.id.as_str()))
            .find(|w| {
                task.dispatch_id.is_none() || task.dispatch_id.as_deref() == Some(&w.dispatch_id)
            })?;
        let handle = worker.agent_terminal_handle.as_deref()?;
        let terminal = self.terminal(handle).filter(|t| !t.orphaned)?;
        let taken_over = self.workers.iter().any(|w| {
            w.dispatch_id != worker.dispatch_id
                && w.agent_terminal_handle.as_deref() == Some(handle)
                && w.dispatch_status == Some(DispatchStatus::Dispatched)
        });
        if taken_over {
            return None;
        }
        let from_dispatch = format!("dispatch:{}", worker.dispatch_id);
        let messages = self
            .inbox
            .iter()
            .filter(|m| {
                let from = m.from_handle.as_deref();
                from == Some(handle) || from == Some(from_dispatch.as_str())
            })
            .filter(|m| {
                m.parsed_payload()
                    .and_then(|p| p.task_id)
                    .is_some_and(|id| id == task.id)
            })
            .filter_map(|m| ts_ms(&m.created_at))
            .filter(|&ms| ms > failed_at);
        let output = output_ms
            .or_else(|| ts_ms(&terminal.last_output_at))
            .filter(|&ms| ms > failed_at + REVIVAL_GRACE_MS);
        messages.chain(output).max()
    }

    /// The coordinator when the declared one is not an attendant. `coordinator_handle` can be
    /// stale when the coordinator re-bound from another terminal, so fall back to the newest live
    /// creator of the Run's tasks, then the newest live terminal messaging the Run. Workers of the
    /// Run are never the coordinator. `None` when no live candidate exists.
    fn declared_or_rebound_coordinator(&self, run_id: &str) -> Option<String> {
        let live = |h: &str| self.terminal(h).is_some();
        let declared = self
            .run(run_id)
            .and_then(|r| r.coordinator_handle.as_deref());
        if let Some(h) = declared.filter(|h| live(h)) {
            return Some(h.to_string());
        }

        let workers: BTreeSet<&str> = self
            .workers
            .iter()
            .filter(|w| w.run_id.as_deref() == Some(run_id))
            .filter_map(|w| w.agent_terminal_handle.as_deref())
            .collect();
        let candidate = |h: &str| live(h) && !workers.contains(h);

        let tasks = self.tasks.get(run_id).map(Vec::as_slice).unwrap_or(&[]);
        let creator = tasks
            .iter()
            .filter_map(|t: &Task| {
                let h = t.created_by_terminal_handle.as_deref()?;
                candidate(h).then_some((ts_ms(&t.created_at), h))
            })
            .max_by_key(|(ms, _)| *ms);
        if let Some((_, h)) = creator {
            return Some(h.to_string());
        }

        let run_home = format!("run:{run_id}");
        self.inbox
            .iter()
            .filter(|m| {
                m.run_id.as_deref() == Some(run_id) || m.to_handle.as_deref() == Some(&run_home)
            })
            .filter_map(|m: &InboxMessage| {
                let h = m.from_handle.as_deref()?;
                candidate(h).then_some(((ts_ms(&m.created_at), m.sequence), h))
            })
            .max_by_key(|(order, _)| *order)
            .map(|(_, h)| h.to_string())
    }
}

/// A failed task whose result says why: the coordinator retired it on purpose.
impl ClanData {
    /// Whether `m` is the report of a worker whose start Orca misreported: one Orca refused on
    /// a revoked dispatch ([`InboxMessage::is_revoked_report`]) that is still its task's
    /// dispatch, the task failed with no recorded result. A dispatch a coordinator superseded or
    /// retired doesn't count.
    pub fn is_misreported_launch_report(&self, m: &InboxMessage) -> bool {
        if !m.is_revoked_report() {
            return false;
        }
        let (Some(run_id), Some(payload)) = (m.run_id.as_deref(), m.parsed_payload()) else {
            return false;
        };
        let Some(task_id) = payload.task_id else {
            return false;
        };
        self.tasks
            .get(run_id)
            .and_then(|ts| ts.iter().find(|t| t.id == task_id))
            .is_some_and(|t| {
                t.status == TaskStatus::Failed
                    && !task_has_result(t)
                    && (t.dispatch_id.is_none() || t.dispatch_id == payload.dispatch_id)
            })
    }
}

fn task_has_result(task: &Task) -> bool {
    task.result
        .as_deref()
        .map(str::trim)
        .is_some_and(|r| !r.is_empty() && r != "null" && r != "\"\"")
}

fn ts_ms(ts: &Option<Timestamp>) -> Option<i64> {
    ts.as_ref().and_then(Timestamp::epoch_ms)
}

#[cfg(test)]
mod revival_tests {
    use super::*;
    use serde_json::json;

    const MIN: i64 = 60_000;
    const FAILED_AT: i64 = 1_790_000_000_000;

    fn data(result: Option<&str>, last_output: i64, inbox: Vec<InboxMessage>) -> ClanData {
        let mut task = json!({
            "id": "t1", "run_id": "run_1", "status": "failed", "dispatch_id": "ctx_1",
            "completed_at": FAILED_AT,
        });
        if let Some(r) = result {
            task["result"] = json!(r);
        }
        ClanData {
            terminals: vec![Terminal {
                handle: "term_w".into(),
                connected: true,
                last_output_at: Some(Timestamp::Millis(last_output)),
                ..Default::default()
            }],
            workers: vec![worker("ctx_1", "failed")],
            tasks: [("run_1".into(), vec![serde_json::from_value(task).unwrap()])].into(),
            inbox,
            ..Default::default()
        }
    }

    fn worker(dispatch: &str, status: &str) -> Worker {
        serde_json::from_value(json!({
            "dispatchId": dispatch, "taskId": "t1", "runId": "run_1",
            "dispatchStatus": status, "agentTerminalHandle": "term_w",
        }))
        .unwrap()
    }

    fn task(d: &ClanData) -> &Task {
        &d.tasks["run_1"][0]
    }

    fn rejected_done(at: i64) -> InboxMessage {
        serde_json::from_value(json!({
            "id": "m1", "run_id": "run_1", "from_handle": "term_w", "type": "worker_done",
            "subject": "Rejected worker_done: done", "created_at": at,
            "payload": json!({"taskId": "t1", "_orcaLifecycleRejection": "revoked"}).to_string(),
        }))
        .unwrap()
    }

    #[test]
    fn output_well_after_the_failure_revives() {
        let d = data(None, FAILED_AT + 10 * MIN, vec![]);
        assert_eq!(
            d.alive_after_failure(task(&d), None),
            Some(FAILED_AT + 10 * MIN)
        );
    }

    #[test]
    fn launch_noise_alone_does_not() {
        let d = data(None, FAILED_AT + 30_000, vec![]);
        assert_eq!(d.alive_after_failure(task(&d), None), None);
    }

    #[test]
    fn rejected_reports_count_as_life() {
        let d = data(None, FAILED_AT, vec![rejected_done(FAILED_AT + 5 * MIN)]);
        assert_eq!(
            d.alive_after_failure(task(&d), None),
            Some(FAILED_AT + 5 * MIN)
        );
    }

    #[test]
    fn a_recorded_reason_or_a_newer_dispatch_means_it_is_over() {
        let d = data(Some("Superseded by t2"), FAILED_AT + 10 * MIN, vec![]);
        assert_eq!(d.alive_after_failure(task(&d), None), None);

        let mut d = data(None, FAILED_AT + 10 * MIN, vec![]);
        d.workers.push(worker("ctx_2", "dispatched"));
        assert_eq!(d.alive_after_failure(task(&d), None), None);
    }
}

#[cfg(test)]
mod names_tests {
    use super::*;

    fn worktree(path: &str, branch: &str, name: &str, main: bool) -> Worktree {
        Worktree {
            path: path.into(),
            branch: Some(format!("refs/heads/{branch}")),
            display_name: Some(name.into()),
            is_main_worktree: main,
            ..Worktree::default()
        }
    }

    /// Only names that differ from the folder and branch, on linked worktrees, count.
    #[test]
    fn chosen_names_skip_orca_defaults() {
        let names = chosen_worktree_names(&[
            worktree("/w/shop/fix-cart", "gylleus/fix-cart", "fix-cart", false),
            worktree("/w/shop/retry/", "gylleus/retry", "Checkout retries", false),
            worktree("/w/shop/x", "gylleus/x-branch", "gylleus/x-branch", false),
            worktree("/r/shop", "main", "old-branch-name", true),
            worktree("/w/shop/blank", "b", "  ", false),
        ]);
        let expected: BTreeMap<String, String> =
            [("/w/shop/retry".to_string(), "Checkout retries".to_string())].into();
        assert_eq!(names, expected);
        let data = ClanData {
            worktree_names: names,
            ..ClanData::default()
        };
        assert_eq!(
            data.worktree_name(Some("/w/shop/retry/")),
            Some("Checkout retries")
        );
        assert_eq!(data.worktree_name(Some("/w/shop/fix-cart")), None);
    }
}

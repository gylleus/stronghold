//! `FleetData`: one poll's raw Orca data, shared by the poller (producer), the graph model and
//! the whip (consumers).

mod roles;
#[cfg(test)]
mod roles_tests;

use std::collections::{BTreeMap, BTreeSet};

use crate::detect::PromptKind;
use crate::orca::spawn::Worktree;
use crate::orca::{InboxMessage, Run, Task, Terminal, Timestamp, Worker};

pub use roles::{AttendantSignal, Forward, RunRoles};

/// Everything one poll cycle fetched from Orca, unprocessed.
#[derive(Debug, Clone, Default)]
pub struct FleetData {
    /// Wall-clock time the poll finished, in ms since the Unix epoch.
    pub polled_at_ms: i64,
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
    /// Terminals each Claude terminal forwarded input to with `orca terminal send`, read from
    /// its transcript by the Claude worker, by sender handle. Only declared coordinators are
    /// scanned; see [`FleetData::run_roles`].
    pub forwards: BTreeMap<String, Vec<Forward>>,
    /// Names chosen for worktrees in Orca (by the Queen at spawn, or renamed in Orca), by
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

impl FleetData {
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

    pub fn run(&self, run_id: &str) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == run_id)
    }

    /// The terminal actually coordinating a Run: [`RunRoles::coordinator`] of
    /// [`FleetData::run_roles`]. `None` when no live candidate exists.
    pub fn effective_coordinator(&self, run_id: &str) -> Option<String> {
        self.run_roles(run_id).coordinator
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

fn ts_ms(ts: &Option<Timestamp>) -> Option<i64> {
    ts.as_ref().and_then(Timestamp::epoch_ms)
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
        let data = FleetData {
            worktree_names: names,
            ..FleetData::default()
        };
        assert_eq!(
            data.worktree_name(Some("/w/shop/retry/")),
            Some("Checkout retries")
        );
        assert_eq!(data.worktree_name(Some("/w/shop/fix-cart")), None);
    }
}

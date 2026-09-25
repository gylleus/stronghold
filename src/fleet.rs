//! `FleetData`: one poll's raw Orca data, shared by the poller (producer), the graph model and
//! the whip (consumers).

use std::collections::{BTreeMap, BTreeSet};

use crate::detect::PromptKind;
use crate::orca::{InboxMessage, Run, Task, Terminal, Timestamp, Worker};

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
}

impl FleetData {
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

    /// The terminal actually coordinating a Run. `coordinator_handle` can be stale when the
    /// coordinator re-bound from another terminal, so fall back to the newest live creator of the
    /// Run's tasks, then the newest live terminal messaging the Run. Workers of the Run are never
    /// the coordinator. `None` when no live candidate exists.
    pub fn effective_coordinator(&self, run_id: &str) -> Option<String> {
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

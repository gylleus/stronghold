//! `FleetData`: one poll's raw Orca data, shared by the poller (producer), the graph model and
//! the whip (consumers).

use std::collections::BTreeMap;

use crate::detect::PromptKind;
use crate::orca::{InboxMessage, Run, Task, Terminal, Worker};

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
}

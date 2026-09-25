//! `FleetData`: one poll's raw Orca data, shared by the poller (producer), the graph model and
//! the whip (consumers).

use std::collections::BTreeMap;

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
}

impl FleetData {
    pub fn terminal(&self, handle: &str) -> Option<&Terminal> {
        self.terminals.iter().find(|t| t.handle == handle)
    }

    pub fn run(&self, run_id: &str) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == run_id)
    }
}

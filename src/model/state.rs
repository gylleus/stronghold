//! Node state derivation. Placeholder rules; myr-k7y.2 replaces the body of [`derive_state`].

use crate::orca::{DispatchStatus, Terminal, Worker, WorkerState, WorkerTerminalState};

use super::NodeState;

/// Output newer than this counts as Working.
pub const WORKING_WINDOW_MS: i64 = 30_000;

/// Everything [`derive_state`] may look at for one node.
#[derive(Debug, Clone, Copy)]
pub struct StateInputs<'a> {
    /// The node's live terminal; `None` when it is not in `terminal list`.
    pub terminal: Option<&'a Terminal>,
    /// The dispatch the node represents, for worker nodes.
    pub worker: Option<&'a Worker>,
    pub now_ms: i64,
}

/// The single place that decides a node's [`NodeState`].
pub fn derive_state(inputs: &StateInputs<'_>) -> NodeState {
    if let Some(worker) = inputs.worker {
        let failed = worker.worker_state == Some(WorkerState::Failed)
            || matches!(
                worker.dispatch_status,
                Some(DispatchStatus::Failed | DispatchStatus::CircuitBroken)
            );
        if failed {
            return NodeState::Failed;
        }
        let done = worker.worker_state == Some(WorkerState::Succeeded)
            || worker.dispatch_status == Some(DispatchStatus::Completed)
            || worker.terminal_state == Some(WorkerTerminalState::Released);
        if done {
            return NodeState::Done;
        }
        if inputs.terminal.is_none() {
            // Dispatched, not released, and its terminal is gone.
            return NodeState::Failed;
        }
    }
    let last_output = inputs
        .terminal
        .and_then(|t| t.last_output_at.as_ref())
        .and_then(|t| t.epoch_ms());
    match last_output {
        Some(ms) if inputs.now_ms - ms < WORKING_WINDOW_MS => NodeState::Working,
        _ => NodeState::Idle,
    }
}

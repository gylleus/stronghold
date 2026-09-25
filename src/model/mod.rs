//! `FleetSnapshot`: runs, nodes and edges built from Orca and Claude data.

mod label;
mod state;
mod subagent;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use crate::claude::ClaudeData;
use crate::fleet::FleetData;
use crate::orca::{
    AgentIdentity, DispatchStatus, Task, TaskStatus, Terminal, Worker, WorkerState,
    WorkerTerminalState,
};

pub use label::{MAX_LABEL_CHARS, strip_spinner, truncate_label};
pub use state::{StateInputs, WORKING_WINDOW_MS, derive_state};

/// How long a released worker stays in the snapshot after its release, so the UI can fade it.
pub const RELEASED_FADE_MS: i64 = 10 * 60 * 1_000;

/// Identifies a node across polls; the graph view keys layout positions by it.
///
/// Live terminals use `term:<handle>`; worker nodes without a live terminal use
/// `dispatch:<run id>:<dispatch id>`; Claude subagents use `sub:<session id>:<agent id>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn terminal(handle: &str) -> Self {
        Self(format!("term:{handle}"))
    }

    pub fn dispatch(run_id: &str, dispatch_id: &str) -> Self {
        Self(format!("dispatch:{run_id}:{dispatch_id}"))
    }

    pub fn subagent(session_id: &str, agent_id: &str) -> Self {
        Self(format!("sub:{session_id}:{agent_id}"))
    }

    /// `(session id, agent id)` of a subagent node id.
    pub fn subagent_parts(&self) -> Option<(&str, &str)> {
        self.0.strip_prefix("sub:")?.split_once(':')
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// A run's coordinator terminal.
    Coordinator,
    /// A terminal (live or not) dispatched to a run's task.
    Worker,
    /// A live terminal outside any run.
    Terminal,
    /// A Claude subagent under a terminal (M3).
    Subagent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeState {
    Working,
    Idle,
    NeedsInput,
    Stalled,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    /// Set for live terminals and for workers (their last known terminal).
    pub terminal_handle: Option<String>,
    /// `false` when the node's terminal is not in `terminal list`.
    pub live: bool,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub dispatch_id: Option<String>,
    pub agent: Option<AgentIdentity>,
    pub worktree_path: Option<String>,
    /// Branch without `refs/heads/`.
    pub branch: Option<String>,
    pub last_output_ms: Option<i64>,
    pub preview: Option<String>,
    pub state: NodeState,
    pub worker_state: Option<WorkerState>,
    pub dispatch_status: Option<DispatchStatus>,
    pub worker_terminal_state: Option<WorkerTerminalState>,
    /// When a released worker's terminal was released, or a subagent finished; the UI fades
    /// these out.
    pub released_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    CoordinatorWorker,
    /// Terminal → subagent, or parent subagent → nested subagent.
    Subagent,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
}

/// Tasks of a run by status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskCounts {
    pub pending: usize,
    pub ready: usize,
    pub dispatched: usize,
    pub completed: usize,
    pub failed: usize,
    pub blocked: usize,
    pub other: usize,
}

impl TaskCounts {
    pub fn from_tasks(tasks: &[Task]) -> Self {
        let mut counts = Self::default();
        for task in tasks {
            let slot = match task.status {
                TaskStatus::Pending => &mut counts.pending,
                TaskStatus::Ready => &mut counts.ready,
                TaskStatus::Dispatched => &mut counts.dispatched,
                TaskStatus::Completed => &mut counts.completed,
                TaskStatus::Failed => &mut counts.failed,
                TaskStatus::Blocked => &mut counts.blocked,
                TaskStatus::Other(_) => &mut counts.other,
            };
            *slot += 1;
        }
        counts
    }

    pub fn total(&self) -> usize {
        self.pending
            + self.ready
            + self.dispatched
            + self.completed
            + self.failed
            + self.blocked
            + self.other
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunSummary {
    pub id: String,
    pub objective: Option<String>,
    /// `None` when the coordinator terminal is not live.
    pub coordinator: Option<NodeId>,
    /// `None` when this poll fetched no tasks for the run.
    pub task_counts: Option<TaskCounts>,
}

/// A point-in-time view of the whole fleet.
#[derive(Debug, Clone, Default)]
pub struct FleetSnapshot {
    pub built_at_ms: i64,
    /// Runs with at least one live terminal, in `run-list` order.
    pub runs: Vec<RunSummary>,
    /// Live terminals in `terminal list` order, then non-live workers.
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    index: HashMap<NodeId, usize>,
}

impl FleetSnapshot {
    /// Builds the snapshot from one poll's data. Pure: the same input gives the same output.
    pub fn build(data: &FleetData, now_ms: i64) -> Self {
        Self::build_with_claude(data, None, now_ms)
    }

    /// Like [`FleetSnapshot::build`], plus subagent nodes and edges from `claude`.
    pub fn build_with_claude(data: &FleetData, claude: Option<&ClaudeData>, now_ms: i64) -> Self {
        let live: HashMap<&str, &Terminal> = data
            .terminals
            .iter()
            .map(|t| (t.handle.as_str(), t))
            .collect();
        let coordinator_run: HashMap<&str, &str> = data
            .runs
            .iter()
            .filter_map(|r| Some((r.coordinator_handle.as_deref()?, r.id.as_str())))
            .collect();
        let worker_for_terminal = pick_worker_per_terminal(data);

        let mut nodes = Vec::new();
        for terminal in &data.terminals {
            let handle = terminal.handle.as_str();
            let node = if let Some(run_id) = coordinator_run.get(handle) {
                let mut node = terminal_node(terminal, NodeKind::Coordinator, now_ms);
                node.run_id = Some((*run_id).to_string());
                node
            } else if let Some(worker) = worker_for_terminal.get(handle) {
                worker_node(data, worker, Some(terminal), now_ms)
            } else {
                terminal_node(terminal, NodeKind::Terminal, now_ms)
            };
            nodes.push(node);
        }

        let live_runs: BTreeSet<String> = nodes.iter().filter_map(|n| n.run_id.clone()).collect();

        for worker in &data.workers {
            let Some(run_id) = worker.run_id.as_deref() else {
                continue;
            };
            let handle = worker.agent_terminal_handle.as_deref();
            // A live terminal is already shown, as this or a more recent dispatch.
            if handle.is_some_and(|h| live.contains_key(h)) || !live_runs.contains(run_id) {
                continue;
            }
            let mut node = worker_node(data, worker, None, now_ms);
            if worker.terminal_state == Some(WorkerTerminalState::Released) {
                match released_at_ms(worker) {
                    Some(at) if now_ms - at <= RELEASED_FADE_MS => node.released_at_ms = Some(at),
                    _ => continue,
                }
            }
            nodes.push(node);
        }

        let mut subagent_edges = Vec::new();
        if let Some(claude) = claude {
            subagent::add_subagents(claude, now_ms, &mut nodes, &mut subagent_edges);
        }

        let index: HashMap<NodeId, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect();

        let runs: Vec<RunSummary> = data
            .runs
            .iter()
            .filter(|r| live_runs.contains(&r.id))
            .map(|r| RunSummary {
                id: r.id.clone(),
                objective: r.objective.clone(),
                coordinator: r
                    .coordinator_handle
                    .as_deref()
                    .filter(|h| live.contains_key(h))
                    .map(NodeId::terminal),
                task_counts: data.tasks.get(&r.id).map(|t| TaskCounts::from_tasks(t)),
            })
            .collect();

        let mut edges = Vec::new();
        for run in &runs {
            let Some(coordinator) = &run.coordinator else {
                continue;
            };
            edges.extend(
                nodes
                    .iter()
                    .filter(|n| n.kind == NodeKind::Worker && n.run_id.as_deref() == Some(&run.id))
                    .map(|n| Edge {
                        from: coordinator.clone(),
                        to: n.id.clone(),
                        kind: EdgeKind::CoordinatorWorker,
                    }),
            );
        }

        edges.extend(subagent_edges);

        Self {
            built_at_ms: now_ms,
            runs,
            nodes,
            edges,
            index,
        }
    }

    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.index.get(id).map(|&i| &self.nodes[i])
    }

    pub fn run(&self, run_id: &str) -> Option<&RunSummary> {
        self.runs.iter().find(|r| r.id == run_id)
    }

    /// Coordinator and workers of a run.
    pub fn nodes_in_run<'a>(&'a self, run_id: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.nodes
            .iter()
            .filter(move |n| n.run_id.as_deref() == Some(run_id))
    }

    /// Live terminals outside any run.
    pub fn loose_nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(|n| n.kind == NodeKind::Terminal)
    }
}

/// For each terminal handle, the worker it currently serves: an active dispatch beats a
/// finished one, then the most recently created task, then the later `worker-list` row.
fn pick_worker_per_terminal(data: &FleetData) -> HashMap<&str, &Worker> {
    let mut best: HashMap<&str, (WorkerRank, &Worker)> = HashMap::new();
    for (position, worker) in data.workers.iter().enumerate() {
        let Some(handle) = worker.agent_terminal_handle.as_deref() else {
            continue;
        };
        let rank = WorkerRank {
            active: is_active_dispatch(worker),
            task_created_ms: find_task(data, worker)
                .and_then(|t| t.created_at.as_ref())
                .and_then(|t| t.epoch_ms()),
            position,
        };
        match best.get(handle) {
            Some((current, _)) if *current >= rank => {}
            _ => {
                best.insert(handle, (rank, worker));
            }
        }
    }
    best.into_iter().map(|(h, (_, w))| (h, w)).collect()
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct WorkerRank {
    active: bool,
    task_created_ms: Option<i64>,
    position: usize,
}

fn is_active_dispatch(worker: &Worker) -> bool {
    worker.dispatch_status == Some(DispatchStatus::Dispatched)
        || worker.terminal_state == Some(WorkerTerminalState::Active)
}

fn find_task<'a>(data: &'a FleetData, worker: &Worker) -> Option<&'a Task> {
    let task_id = worker.task_id.as_deref()?;
    let tasks = data.tasks.get(worker.run_id.as_deref()?)?;
    tasks.iter().find(|t| t.id == task_id)
}

fn released_at_ms(worker: &Worker) -> Option<i64> {
    let resource = worker.resource.as_ref()?;
    resource
        .release_completed_at
        .as_ref()
        .or(resource.release_requested_at.as_ref())?
        .epoch_ms()
}

fn terminal_node(terminal: &Terminal, kind: NodeKind, now_ms: i64) -> Node {
    let inputs = StateInputs::new(Some(terminal), None, now_ms);
    Node {
        id: NodeId::terminal(&terminal.handle),
        kind,
        label: label::terminal_label(terminal),
        terminal_handle: Some(terminal.handle.clone()),
        live: true,
        run_id: None,
        task_id: None,
        dispatch_id: None,
        agent: terminal.agent_identity.clone(),
        worktree_path: terminal.worktree_path.clone(),
        branch: terminal.branch_name().map(str::to_string),
        last_output_ms: terminal.last_output_at.as_ref().and_then(|t| t.epoch_ms()),
        preview: terminal.preview.clone(),
        state: derive_state(&inputs),
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
    }
}

fn worker_node(
    data: &FleetData,
    worker: &Worker,
    terminal: Option<&Terminal>,
    now_ms: i64,
) -> Node {
    let task = find_task(data, worker);
    let mut node = match terminal {
        Some(t) => terminal_node(t, NodeKind::Worker, now_ms),
        None => Node {
            id: NodeId::dispatch(worker.run_id.as_deref().unwrap_or(""), &worker.dispatch_id),
            kind: NodeKind::Worker,
            label: String::new(),
            terminal_handle: worker.agent_terminal_handle.clone(),
            live: false,
            run_id: None,
            task_id: None,
            dispatch_id: None,
            agent: None,
            worktree_path: None,
            branch: None,
            last_output_ms: None,
            preview: None,
            state: NodeState::Idle,
            worker_state: None,
            dispatch_status: None,
            worker_terminal_state: None,
            released_at_ms: None,
        },
    };
    node.kind = NodeKind::Worker;
    node.run_id = worker.run_id.clone();
    node.task_id = worker.task_id.clone();
    node.dispatch_id = Some(worker.dispatch_id.clone());
    node.worker_state = worker.worker_state.clone();
    node.dispatch_status = worker.dispatch_status.clone();
    node.worker_terminal_state = worker.terminal_state.clone();
    node.label = label::worker_label(task, terminal, worker);
    node.state = derive_state(&StateInputs::new(terminal, Some(worker), now_ms));
    node
}

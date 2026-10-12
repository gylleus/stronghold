//! `FleetSnapshot`: runs, nodes and edges built from Orca and Claude data.

mod label;
mod queen;
mod repo;
mod state;
mod subagent;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use crate::claude::ClaudeData;
use crate::fleet::{FleetData, RunRoles};
use crate::orca::{
    AgentIdentity, DispatchStatus, Task, TaskStatus, Terminal, Worker, WorkerState,
    WorkerTerminalState,
};

pub use label::{MAX_LABEL_CHARS, strip_spinner, truncate_label};
pub use queen::{QUEEN_ID, QUEEN_LABEL, fleet_state};
pub use repo::{NO_REPO, RepoIndex, node_repo, repo_of, run_repo};
pub use state::{StateInputs, WORKING_WINDOW_MS, derive_state, dispatch_ended};

/// How long a released worker stays in the snapshot after its release, so the UI can fade it.
pub const RELEASED_FADE_MS: i64 = 10 * 60 * 1_000;

/// Identifies a node across polls; the farm view keys layout positions by it.
///
/// Live terminals use `term:<handle>`; worker nodes without a live terminal use
/// `dispatch:<run id>:<dispatch id>`; Claude subagents use `sub:<session id>:<agent id>`; the
/// queen is `queen`.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn terminal(handle: &str) -> Self {
        Self(format!("term:{handle}"))
    }

    pub fn dispatch(run_id: &str, dispatch_id: &str) -> Self {
        Self(format!("dispatch:{run_id}:{dispatch_id}"))
    }

    /// The id of the snapshot's one [`NodeKind::Queen`] node.
    pub fn queen() -> Self {
        Self(QUEEN_ID.to_string())
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum NodeKind {
    /// A run's coordinator terminal.
    Coordinator,
    /// The terminal that created a run and forwards its bus events to the coordinator (Orca
    /// records it as the run's coordinator); see [`crate::fleet::RunRoles`].
    Attendant,
    /// A terminal (live or not) dispatched to a run's task.
    Worker,
    /// A live terminal outside any run.
    Terminal,
    /// The persistent listener session, identified by terminal handle.
    Envoy,
    /// A Claude subagent under a terminal (M3).
    Subagent,
    /// The fleet's hub, linked to every coordinator and loose terminal; broadcasts start here.
    /// Exactly one per snapshot, with no terminal.
    Queen,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
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
    /// When Karl parked the agent ([`crate::park`]): its terminal and worktree are kept, but
    /// it sleeps out of the live fleet until woken.
    pub parked_at_ms: Option<i64>,
}

impl Node {
    pub fn is_parked(&self) -> bool {
        self.parked_at_ms.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    CoordinatorWorker,
    /// Terminal → subagent, or parent subagent → nested subagent.
    Subagent,
    /// Queen → coordinator or loose terminal, or → attendant when its run has no coordinator.
    Queen,
    /// Coordinator → its run's attendant.
    Attendant,
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
    /// Attendant node per run id, for runs whose recorded coordinator is an attendant
    /// forwarding to the real one; see [`crate::fleet::RunRoles`].
    pub attendants: BTreeMap<String, NodeId>,
    index: HashMap<NodeId, usize>,
}

impl FleetSnapshot {
    /// Builds the snapshot from one poll's data. Pure: the same input gives the same output.
    pub fn build(data: &FleetData, now_ms: i64) -> Self {
        Self::build_with_claude(data, None, now_ms)
    }

    /// Like [`FleetSnapshot::build`], plus subagent nodes and edges from `claude`.
    pub fn build_with_claude(data: &FleetData, claude: Option<&ClaudeData>, now_ms: i64) -> Self {
        Self::build_with_stalled(data, claude, &BTreeSet::new(), now_ms)
    }

    /// Like [`FleetSnapshot::build_with_claude`], with the terminals in `stalled` (the whip's
    /// escalated handles) marked stalled, see [`StateInputs::stalled`].
    pub fn build_with_stalled(
        data: &FleetData,
        claude: Option<&ClaudeData>,
        stalled: &BTreeSet<String>,
        now_ms: i64,
    ) -> Self {
        let live: HashMap<&str, &Terminal> = data
            .terminals
            .iter()
            .map(|t| (t.handle.as_str(), t))
            .collect();
        let roles: HashMap<&str, RunRoles> = data
            .runs
            .iter()
            .map(|r| (r.id.as_str(), data.run_roles(&r.id)))
            .collect();
        let coordinators: HashMap<&str, String> = roles
            .iter()
            .filter_map(|(run, r)| Some((*run, r.coordinator.clone()?)))
            .collect();
        let coordinator_run = pick_run_per_coordinator(data, &coordinators);
        let attendants: HashMap<&str, String> = roles
            .iter()
            .filter_map(|(run, r)| Some((*run, r.attendant.clone()?)))
            .filter(|(_, h)| !coordinator_run.contains_key(h))
            .collect();
        let attendant_run = pick_run_per_coordinator(data, &attendants);
        let worker_for_terminal = pick_worker_per_terminal(data);

        let mut nodes = Vec::new();
        for terminal in &data.terminals {
            let handle = terminal.handle.as_str();
            let node = if let Some(run_id) = coordinator_run.get(handle) {
                let mut node =
                    terminal_node(terminal, NodeKind::Coordinator, now_ms, data, stalled);
                node.run_id = Some((*run_id).to_string());
                node
            } else if let Some(run_id) = attendant_run.get(handle) {
                let mut node = terminal_node(terminal, NodeKind::Attendant, now_ms, data, stalled);
                node.run_id = Some((*run_id).to_string());
                node
            } else if let Some(worker) = worker_for_terminal.get(handle) {
                worker_node(data, worker, Some(terminal), now_ms, stalled)
            } else {
                terminal_node(terminal, NodeKind::Terminal, now_ms, data, stalled)
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
            let mut node = worker_node(data, worker, None, now_ms, stalled);
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
                coordinator: coordinators.get(r.id.as_str()).map(|h| NodeId::terminal(h)),
                task_counts: data.tasks.get(&r.id).map(|t| TaskCounts::from_tasks(t)),
            })
            .collect();
        let run_attendants: BTreeMap<String, NodeId> = runs
            .iter()
            .filter_map(|r| {
                Some((
                    r.id.clone(),
                    NodeId::terminal(attendants.get(r.id.as_str())?),
                ))
            })
            .collect();

        let mut edges = Vec::new();
        for run in &runs {
            let Some(coordinator) = &run.coordinator else {
                continue;
            };
            if let Some(attendant) = run_attendants.get(&run.id) {
                edges.push(Edge {
                    from: coordinator.clone(),
                    to: attendant.clone(),
                    kind: EdgeKind::Attendant,
                });
            }
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

        let mut snapshot = Self {
            built_at_ms: now_ms,
            runs,
            nodes,
            edges,
            attendants: run_attendants,
            index,
        };
        snapshot.add_queen();
        snapshot
    }

    /// Adds the queen node and its edges to every coordinator and loose terminal, and to
    /// attendants whose run has no coordinator, replacing any earlier queen, and reindexes. [`FleetSnapshot::build`] calls it; tests that assemble
    /// snapshots by hand can too.
    pub fn add_queen(&mut self) {
        self.nodes.retain(|n| n.kind != NodeKind::Queen);
        self.edges.retain(|e| e.kind != EdgeKind::Queen);
        let queen = queen::queen_node(&self.nodes);
        let spokes: Vec<Edge> = self
            .nodes
            .iter()
            .filter(|n| {
                !n.is_parked()
                    && (matches!(
                        n.kind,
                        NodeKind::Coordinator | NodeKind::Terminal | NodeKind::Envoy
                    ) || self.coordinator_gone(n))
            })
            .map(|n| Edge {
                from: queen.id.clone(),
                to: n.id.clone(),
                kind: EdgeKind::Queen,
            })
            .collect();
        self.edges.extend(spokes);
        self.nodes.push(queen);
        self.index = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect();
    }

    /// Marks the live agent terminals in `waiting` (by handle: the question they wait on Karl
    /// to answer) as NeedsInput, with the question as their preview. Done, failed and
    /// non-live nodes keep their state. Updates the queen.
    pub fn mark_waiting(&mut self, waiting: &BTreeMap<String, String>) {
        if waiting.is_empty() {
            return;
        }
        for node in &mut self.nodes {
            let Some(question) = node
                .terminal_handle
                .as_deref()
                .filter(|_| node.live && node.kind != NodeKind::Subagent)
                .and_then(|h| waiting.get(h))
            else {
                continue;
            };
            if matches!(
                node.state,
                NodeState::Idle | NodeState::Working | NodeState::Stalled
            ) {
                node.state = NodeState::NeedsInput;
                node.preview = Some(format!("Asks: {question}"));
            }
        }
        self.add_queen();
    }

    /// Identity follows the handle even when the agent rewrites its terminal title.
    pub fn mark_envoy(&mut self, handle: Option<&str>, status: crate::envoy::EnvoyStatus) {
        let Some(handle) = handle else {
            return;
        };
        for node in &mut self.nodes {
            if node.live
                && node.kind != NodeKind::Subagent
                && node.terminal_handle.as_deref() == Some(handle)
            {
                node.kind = NodeKind::Envoy;
                node.label = format!("Envoy · {}", status.health().label());
            }
        }
        self.add_queen();
    }

    /// Shows sessions that finished themselves ([`crate::finish`]) as done: a node still in
    /// the snapshot (its terminal not yet closed) turns Done; a closed one comes back as a
    /// fading Done terminal. Workers keep their run's own node. Updates the queen.
    pub fn add_finished<'a>(
        &mut self,
        finished: impl IntoIterator<Item = &'a crate::finish::FinishedSession>,
    ) {
        let mut changed = false;
        for f in finished {
            let id = NodeId(f.node_id.clone());
            let summary = format!("{}: {}", f.outcome.verb(), f.summary);
            if let Some(&i) = self.index.get(&id) {
                let node = &mut self.nodes[i];
                if node.kind != NodeKind::Worker {
                    node.state = NodeState::Done;
                    node.preview = Some(summary);
                    changed = true;
                }
                continue;
            }
            if !id.as_str().starts_with("term:") {
                continue;
            }
            self.nodes.push(Node {
                id: id.clone(),
                kind: NodeKind::Terminal,
                label: f.label.clone(),
                terminal_handle: f.handle.clone(),
                live: false,
                run_id: None,
                task_id: None,
                dispatch_id: None,
                agent: None,
                worktree_path: f.worktree_path.clone(),
                branch: f.branch.clone(),
                last_output_ms: Some(f.at_ms),
                preview: Some(summary),
                state: NodeState::Done,
                worker_state: None,
                dispatch_status: None,
                worker_terminal_state: None,
                released_at_ms: Some(f.at_ms),
                parked_at_ms: None,
            });
            self.index.insert(id, self.nodes.len() - 1);
            changed = true;
        }
        if changed {
            self.add_queen();
        }
    }

    /// Marks the nodes Karl parked ([`crate::park`]) and takes them off the Queen's spokes and
    /// out of her state. Entries without a node here are skipped. Updates the queen.
    pub fn mark_parked<'a>(
        &mut self,
        parked: impl IntoIterator<Item = &'a crate::park::ParkedAgent>,
    ) {
        let mut changed = false;
        for p in parked {
            if let Some(&i) = self.index.get(&NodeId(p.node_id.clone())) {
                self.nodes[i].parked_at_ms = Some(p.at_ms);
                changed = true;
            }
        }
        if changed {
            self.add_queen();
        }
    }

    /// Parked agent nodes.
    pub fn parked_nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(|n| n.is_parked())
    }

    /// The queen node, present in every built snapshot.
    pub fn queen(&self) -> Option<&Node> {
        self.node(&NodeId::queen())
    }

    /// Nodes that are agents or terminals: every node but the queen.
    pub fn agent_nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(|n| n.kind != NodeKind::Queen)
    }

    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.index.get(id).map(|&i| &self.nodes[i])
    }

    pub fn run(&self, run_id: &str) -> Option<&RunSummary> {
        self.runs.iter().find(|r| r.id == run_id)
    }

    /// The attendant node of a run, if it has one.
    pub fn attendant(&self, run_id: &str) -> Option<&NodeId> {
        self.attendants.get(run_id)
    }

    /// Whether `node` is an attendant whose run has no live coordinator.
    pub fn coordinator_gone(&self, node: &Node) -> bool {
        node.kind == NodeKind::Attendant
            && node
                .run_id
                .as_deref()
                .and_then(|r| self.run(r))
                .is_none_or(|r| r.coordinator.is_none())
    }

    /// Coordinator, attendant and workers of a run.
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

/// For each coordinator terminal, the run its node belongs to when it coordinates several: a
/// run the poller fetched tasks for beats one it did not, then the most recently updated run,
/// then the earlier `run-list` row. Every run still gets edges from its coordinator.
fn pick_run_per_coordinator<'a>(
    data: &'a FleetData,
    coordinators: &HashMap<&str, String>,
) -> HashMap<String, &'a str> {
    let mut best: HashMap<String, (RunRank, &str)> = HashMap::new();
    for (position, run) in data.runs.iter().enumerate() {
        let Some(handle) = coordinators.get(run.id.as_str()) else {
            continue;
        };
        let rank = RunRank {
            active: data.tasks.contains_key(&run.id),
            updated_ms: run
                .updated_at
                .as_ref()
                .or(run.created_at.as_ref())
                .and_then(|t| t.epoch_ms()),
            earlier: std::cmp::Reverse(position),
        };
        match best.get(handle) {
            Some((current, _)) if *current >= rank => {}
            _ => {
                best.insert(handle.clone(), (rank, run.id.as_str()));
            }
        }
    }
    best.into_iter().map(|(h, (_, r))| (h, r)).collect()
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct RunRank {
    active: bool,
    updated_ms: Option<i64>,
    earlier: std::cmp::Reverse<usize>,
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

fn terminal_node(
    terminal: &Terminal,
    kind: NodeKind,
    now_ms: i64,
    data: &FleetData,
    stalled: &BTreeSet<String>,
) -> Node {
    let inputs = StateInputs {
        needs_input: data.screen_needs_input(&terminal.handle),
        stalled: stalled.contains(&terminal.handle),
        ..StateInputs::new(Some(terminal), None, now_ms)
    };
    Node {
        id: NodeId::terminal(&terminal.handle),
        kind,
        label: data
            .worktree_name(terminal.worktree_path.as_deref())
            .map_or_else(|| label::terminal_label(terminal), label::truncate_label),
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
        parked_at_ms: None,
    }
}

fn worker_node(
    data: &FleetData,
    worker: &Worker,
    terminal: Option<&Terminal>,
    now_ms: i64,
    stalled: &BTreeSet<String>,
) -> Node {
    let task = find_task(data, worker);
    let mut node = match terminal {
        Some(t) => terminal_node(t, NodeKind::Worker, now_ms, data, stalled),
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
            parked_at_ms: None,
        },
    };
    node.kind = NodeKind::Worker;
    node.run_id = worker.run_id.clone();
    node.task_id = worker.task_id.clone();
    node.dispatch_id = Some(worker.dispatch_id.clone());
    node.worker_state = worker.worker_state.clone();
    node.dispatch_status = worker.dispatch_status.clone();
    node.worker_terminal_state = worker.terminal_state.clone();
    let worktree_name = data.worktree_name(terminal.and_then(|t| t.worktree_path.as_deref()));
    node.label = label::worker_label(task, worktree_name, terminal, worker);
    node.state = derive_state(&StateInputs {
        needs_input: terminal.and_then(|t| data.screen_needs_input(&t.handle)),
        stalled: terminal.is_some_and(|t| stalled.contains(&t.handle)),
        alive_after_failure: task.is_some_and(|t| {
            t.dispatch_id
                .as_deref()
                .is_none_or(|d| d == worker.dispatch_id)
                && data.alive_after_failure(t, None).is_some()
        }),
        ..StateInputs::new(terminal, Some(worker), now_ms)
    });
    node
}

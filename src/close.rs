//! Closing sessions: plans which Orca calls kill a node's session.
//!
//! [`plan_close`] and [`plan_bulk_close`] are pure; the UI (and later the control API) shows the
//! plan for confirmation and submits [`CloseStep::action`]s to the [`crate::actions::ActionRunner`],
//! which runs them in order.
//!
//! A worker with an active dispatch is fenced with `orchestration worker-stop` first, so the
//! run's state knows it ended. worker-stop closes the pane itself when Orca supervises the
//! terminal; the `terminal close` that follows then finds the terminal gone and counts that as
//! closed (see [`crate::actions::close_outcome`]). For an unsupervised dispatch worker-stop only
//! fences, and the close does the killing.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::actions::Action;
use crate::model::{FleetSnapshot, Node, NodeId, NodeKind};
use crate::orca::{DispatchStatus, Terminal, WorkerState};

pub const SUBAGENT_REFUSAL: &str =
    "subagents run inside their parent session; close the parent instead";
pub const QUEEN_REFUSAL: &str = "the Queen isn't a session; it stands for the whole fleet";

/// Choices in the confirmation dialog.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CloseOptions {
    /// Close each terminal's whole Orca tab (`terminal close --tab`), not just its pane.
    pub whole_tab: bool,
}

/// One Orca call of a close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseStep {
    /// `orca orchestration worker-stop --dispatch <id>`.
    WorkerStop {
        dispatch_id: String,
        handle: Option<String>,
    },
    /// `orca terminal close --terminal <handle> [--tab]`.
    CloseTerminal { handle: String, tab: bool },
}

impl CloseStep {
    pub fn action(&self) -> Action {
        match self {
            CloseStep::WorkerStop {
                dispatch_id,
                handle,
            } => Action::WorkerStop {
                dispatch_id: dispatch_id.clone(),
                handle: handle.clone(),
            },
            CloseStep::CloseTerminal { handle, tab } => Action::CloseTerminal {
                handle: handle.clone(),
                tab: *tab,
            },
        }
    }

    /// The command line, for the confirmation dialog.
    pub fn command(&self) -> String {
        format!("orca {}", self.action().cli_args().join(" "))
    }
}

/// A node a plan closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseTarget {
    pub id: NodeId,
    pub label: String,
    pub kind: NodeKind,
}

impl CloseTarget {
    fn of(node: &Node) -> Self {
        Self {
            id: node.id.clone(),
            label: node.label.clone(),
            kind: node.kind,
        }
    }
}

/// What closing some nodes would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClosePlan {
    pub targets: Vec<CloseTarget>,
    /// Orca calls, in the order they must run.
    pub steps: Vec<CloseStep>,
    /// Side effects the user should know about before confirming.
    pub warnings: Vec<String>,
    /// Why nothing can be closed.
    pub refused: Option<String>,
}

impl ClosePlan {
    fn refuse(node: &Node, reason: impl Into<String>) -> Self {
        Self {
            targets: vec![CloseTarget::of(node)],
            refused: Some(reason.into()),
            ..Self::default()
        }
    }

    /// Whether confirming would run anything.
    pub fn can_run(&self) -> bool {
        self.refused.is_none() && !self.steps.is_empty()
    }

    pub fn actions(&self) -> Vec<Action> {
        self.steps.iter().map(CloseStep::action).collect()
    }
}

/// Whether `node`'s dispatch still counts as running (not settled), so it needs fencing.
pub fn has_active_dispatch(node: &Node) -> bool {
    if node.dispatch_id.is_none() {
        return false;
    }
    let settled_worker = matches!(
        node.worker_state,
        Some(WorkerState::Succeeded | WorkerState::Failed | WorkerState::Stopped)
    );
    let settled_dispatch = matches!(
        node.dispatch_status,
        Some(DispatchStatus::Completed | DispatchStatus::Failed | DispatchStatus::CircuitBroken)
    );
    !(settled_worker || settled_dispatch)
}

/// Plans closing one node's session. `terminals` is the poll's `terminal list` (for tabs).
pub fn plan_close(
    node: &Node,
    snapshot: &FleetSnapshot,
    terminals: &[Terminal],
    opts: CloseOptions,
) -> ClosePlan {
    match node.kind {
        NodeKind::Subagent => return ClosePlan::refuse(node, SUBAGENT_REFUSAL),
        NodeKind::Queen => return ClosePlan::refuse(node, QUEEN_REFUSAL),
        NodeKind::Coordinator | NodeKind::Attendant | NodeKind::Worker | NodeKind::Terminal => {}
    }
    let live_handle = node.terminal_handle.as_deref().filter(|_| node.live);
    let stop = node.kind == NodeKind::Worker && has_active_dispatch(node);
    if live_handle.is_none() && !stop {
        let reason = if node.released_at_ms.is_some() {
            "the worker's terminal was already released"
        } else {
            "no live terminal and no active dispatch; nothing to close"
        };
        return ClosePlan::refuse(node, reason);
    }

    let mut plan = ClosePlan {
        targets: vec![CloseTarget::of(node)],
        ..ClosePlan::default()
    };
    if stop && let Some(dispatch_id) = &node.dispatch_id {
        plan.steps.push(CloseStep::WorkerStop {
            dispatch_id: dispatch_id.clone(),
            handle: node.terminal_handle.clone(),
        });
        if !node.live {
            plan.warnings.push(format!(
                "{}: its terminal is not live; only the dispatch is stopped",
                node.label
            ));
        }
    }
    if let Some(handle) = live_handle {
        let close_handle = if opts.whole_tab {
            let siblings = tab_siblings(handle, terminals);
            if !siblings.is_empty() {
                plan.warnings
                    .push(sibling_warning(&node.label, &siblings, snapshot));
            }
            // worker-stop may already have closed this pane (and, if it was alone, the tab);
            // a sibling's handle still reaches the rest of the tab.
            match siblings.first() {
                Some(sibling) if stop => sibling.handle.clone(),
                _ => handle.to_owned(),
            }
        } else {
            handle.to_owned()
        };
        plan.steps.push(CloseStep::CloseTerminal {
            handle: close_handle,
            tab: opts.whole_tab,
        });
    }
    if node.kind == NodeKind::Coordinator {
        plan.warnings.extend(coordinator_warnings(node, snapshot));
    }
    if node.kind == NodeKind::Attendant {
        plan.warnings.extend(attendant_warnings(node, snapshot));
    }
    plan
}

/// Plans closing several nodes: refused nodes are skipped (with a warning), steps are merged
/// with worker-stops first, and a terminal or tab is closed once.
pub fn plan_bulk_close<'a>(
    nodes: impl IntoIterator<Item = &'a Node>,
    snapshot: &FleetSnapshot,
    terminals: &[Terminal],
    opts: CloseOptions,
) -> ClosePlan {
    let tab_of: HashMap<&str, &str> = terminals
        .iter()
        .filter_map(|t| Some((t.handle.as_str(), t.tab_id.as_deref()?)))
        .collect();
    let mut merged = ClosePlan::default();
    let mut stops = Vec::new();
    let mut closes: Vec<CloseStep> = Vec::new();
    let mut seen_nodes = HashSet::new();
    for node in nodes {
        if !seen_nodes.insert(&node.id) {
            continue;
        }
        let plan = plan_close(node, snapshot, terminals, opts);
        if let Some(reason) = plan.refused {
            push_unique(
                &mut merged.warnings,
                format!("skipped {}: {reason}", node.label),
            );
            continue;
        }
        merged.targets.extend(plan.targets);
        for warning in plan.warnings {
            push_unique(&mut merged.warnings, warning);
        }
        for step in plan.steps {
            match step {
                CloseStep::WorkerStop { .. } => {
                    if !stops.contains(&step) {
                        stops.push(step);
                    }
                }
                CloseStep::CloseTerminal { .. } => closes.push(step),
            }
        }
    }
    merged.steps = stops;
    merged.steps.extend(dedupe_closes(closes, &tab_of));
    if merged.steps.is_empty() {
        merged.refused = Some("nothing to close".into());
    }
    merged
}

/// Drops repeated closes of a handle (a tab close wins over a pane close) and pane closes of
/// terminals whose tab is closed anyway; keeps one close per tab.
fn dedupe_closes(closes: Vec<CloseStep>, tab_of: &HashMap<&str, &str>) -> Vec<CloseStep> {
    let tab_handles: HashSet<&str> = closes
        .iter()
        .filter_map(|s| match s {
            CloseStep::CloseTerminal { handle, tab: true } => Some(handle.as_str()),
            _ => None,
        })
        .collect();
    let closed_tabs: HashSet<&str> = tab_handles
        .iter()
        .filter_map(|h| tab_of.get(h).copied())
        .collect();
    let mut seen_handles = HashSet::new();
    let mut seen_tabs = HashSet::new();
    let mut out = Vec::new();
    for step in &closes {
        let CloseStep::CloseTerminal { handle, tab } = step else {
            continue;
        };
        let tab_id = tab_of.get(handle.as_str()).copied();
        if !*tab
            && (tab_handles.contains(handle.as_str())
                || tab_id.is_some_and(|t| closed_tabs.contains(t)))
        {
            continue;
        }
        if *tab
            && let Some(tab_id) = tab_id
            && !seen_tabs.insert(tab_id)
        {
            continue;
        }
        if seen_handles.insert(handle.clone()) {
            out.push(step.clone());
        }
    }
    out
}

/// Visible nodes "Close shown…" offers: loose terminals and workers (and coordinators when
/// asked) that have something to close.
pub fn bulk_candidates<'a>(
    snapshot: &'a FleetSnapshot,
    hidden: &BTreeSet<NodeId>,
    include_coordinators: bool,
) -> Vec<&'a Node> {
    snapshot
        .nodes
        .iter()
        .filter(|n| !hidden.contains(&n.id))
        .filter(|n| match n.kind {
            NodeKind::Terminal | NodeKind::Worker => true,
            NodeKind::Coordinator | NodeKind::Attendant => include_coordinators,
            NodeKind::Subagent | NodeKind::Queen => false,
        })
        .filter(|n| {
            (n.live && n.terminal_handle.is_some())
                || (n.kind == NodeKind::Worker && has_active_dispatch(n))
        })
        .collect()
}

/// Names of the other sessions that closing the targets' whole tabs would also close,
/// excluding the targets themselves.
pub fn tab_companions(
    targets: &[CloseTarget],
    snapshot: &FleetSnapshot,
    terminals: &[Terminal],
) -> Vec<String> {
    let handles: Vec<&str> = targets
        .iter()
        .filter_map(|t| snapshot.node(&t.id)?.terminal_handle.as_deref())
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    handles
        .iter()
        .flat_map(|h| tab_siblings(h, terminals))
        .filter(|t| !handles.contains(&t.handle.as_str()) && seen.insert(t.handle.clone()))
        .map(|t| {
            snapshot
                .node(&NodeId::terminal(&t.handle))
                .map(|n| n.label.clone())
                .or_else(|| t.title.clone())
                .unwrap_or_else(|| t.handle.clone())
        })
        .collect()
}

/// Other live terminals in `handle`'s Orca tab.
fn tab_siblings<'a>(handle: &str, terminals: &'a [Terminal]) -> Vec<&'a Terminal> {
    let Some(tab_id) = terminals
        .iter()
        .find(|t| t.handle == handle)
        .and_then(|t| t.tab_id.as_deref())
    else {
        return Vec::new();
    };
    terminals
        .iter()
        .filter(|t| t.handle != handle && t.tab_id.as_deref() == Some(tab_id))
        .collect()
}

fn sibling_warning(label: &str, siblings: &[&Terminal], snapshot: &FleetSnapshot) -> String {
    let names: Vec<String> = siblings
        .iter()
        .map(|t| {
            snapshot
                .node(&NodeId::terminal(&t.handle))
                .map(|n| n.label.clone())
                .or_else(|| t.title.clone())
                .unwrap_or_else(|| t.handle.clone())
        })
        .collect();
    let count = names.len();
    let noun = if count == 1 { "terminal" } else { "terminals" };
    format!(
        "{label}: closing the whole tab also closes {count} other {noun}: {}",
        names.join(", ")
    )
}

fn coordinator_warnings(node: &Node, snapshot: &FleetSnapshot) -> Vec<String> {
    let runs = snapshot.runs.iter().filter(|r| {
        r.coordinator.as_ref() == Some(&node.id) || node.run_id.as_deref() == Some(&r.id)
    });
    let mut warnings = Vec::new();
    for run in runs {
        let objective = run.objective.as_deref().unwrap_or(&run.id);
        let tasks = match run.task_counts {
            Some(c) => {
                let open = c.pending + c.ready + c.dispatched + c.blocked;
                format!(" ({open} open tasks)")
            }
            None => String::new(),
        };
        push_unique(
            &mut warnings,
            format!("run {objective} loses its coordinator; its workers keep running{tasks}"),
        );
    }
    if warnings.is_empty() {
        warnings.push(format!(
            "{} is a coordinator; its workers keep running",
            node.label
        ));
    }
    warnings
}

fn attendant_warnings(node: &Node, snapshot: &FleetSnapshot) -> Vec<String> {
    let run = node.run_id.as_deref().and_then(|r| snapshot.run(r));
    let objective = run.map_or(node.label.as_str(), |r| {
        r.objective.as_deref().unwrap_or(&r.id)
    });
    vec![format!(
        "{} is the attendant of run {objective}: bus events stop reaching its coordinator",
        node.label
    )]
}

fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

#[cfg(test)]
mod tests;

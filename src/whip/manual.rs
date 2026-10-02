//! Manual whip: nudge chosen agents now, without waiting for a stall, the threshold or the
//! cooldown.
//!
//! [`plan_manual_whip`] is pure: per selected node it builds the nudge the automatic whip would
//! send (same texts, routing and sender) or says why the node is skipped. The runner executes
//! the nudges live, even while the automatic whip is in dry-run, and records them in the engine
//! so nudge-echo suppression and the escalation ladder know about them.

use std::collections::BTreeSet;

use crate::fleet::FleetData;
use crate::model::{FleetSnapshot, Node, NodeId, NodeKind, NodeState};
use crate::orca::{DispatchStatus, WorkerTerminalState};
use crate::whip::actions::{
    NudgeVia, TaskRef, coordinator_idle_text, quiet_run_text, terminal_nudge_text,
    unanswered_ask_text, worker_nudge_text,
};
use crate::whip::progress::{ProgressReport, ProgressTracker};
use crate::whip::rules::StallTarget;

/// Whipping this many targets or more asks for confirmation first.
pub const CONFIRM_AT: usize = 5;

/// Warning on a nudge to a terminal waiting on Karl.
pub const NEEDS_INPUT_WARNING: &str = "waiting on your input; a nudge won't answer it";

/// One nudge to send now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualNudge {
    pub node: NodeId,
    pub label: String,
    /// Terminal the nudge is about.
    pub handle: String,
    /// Stall targets the nudge covers, for the escalation ladder; empty for a terminal outside
    /// any dispatch or run.
    pub targets: Vec<StallTarget>,
    pub via: NudgeVia,
    pub text: String,
    /// Shown before sending, e.g. [`NEEDS_INPUT_WARNING`].
    pub warn: Option<String>,
}

impl ManualNudge {
    /// Who the nudge is about, for the whip log.
    pub fn target_label(&self) -> String {
        match self.targets.as_slice() {
            [] => format!("terminal {}", self.handle),
            targets => targets
                .iter()
                .map(crate::whip::actions::target_label)
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

/// A node that is not nudged, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualSkip {
    pub node: NodeId,
    pub label: String,
    pub reason: String,
}

/// What a manual whip does with one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualPlan {
    Nudge(ManualNudge),
    Skip(ManualSkip),
}

impl ManualPlan {
    pub fn nudge(&self) -> Option<&ManualNudge> {
        match self {
            Self::Nudge(n) => Some(n),
            Self::Skip(_) => None,
        }
    }

    pub fn skip(&self) -> Option<&ManualSkip> {
        match self {
            Self::Nudge(_) => None,
            Self::Skip(s) => Some(s),
        }
    }
}

/// The nudges of `plans`, in order.
pub fn nudges(plans: &[ManualPlan]) -> Vec<ManualNudge> {
    plans
        .iter()
        .filter_map(ManualPlan::nudge)
        .cloned()
        .collect()
}

/// Whether sending `plans` should be confirmed first: a target waits on Karl's input, or there
/// are [`CONFIRM_AT`] or more nudges.
pub fn needs_confirmation(plans: &[ManualPlan]) -> bool {
    let nudges: Vec<&ManualNudge> = plans.iter().filter_map(ManualPlan::nudge).collect();
    nudges.len() >= CONFIRM_AT || nudges.iter().any(|n| n.warn.is_some())
}

/// Plans a manual whip of `nodes` (in order; duplicates and nodes sharing a terminal are
/// nudged once). Threshold and cooldown do not apply.
///
/// - Worker with a live dispatched task: bus to `dispatch:<id>` about the task, as a stall
///   nudge.
/// - Coordinator of a run: bus to its terminal handle with `--run`.
/// - Loose terminal, or a worker without a live dispatch: typed into its terminal.
/// - Skipped: the Queen, subagents, released workers and terminals that are gone.
/// - NeedsInput targets are nudged but carry [`NEEDS_INPUT_WARNING`]; they are never typed
///   into (that could answer their prompt), so a loose one gets a bus message instead.
pub fn plan_manual_whip(
    nodes: &[NodeId],
    data: &FleetData,
    snapshot: &FleetSnapshot,
    now_ms: i64,
) -> Vec<ManualPlan> {
    let report = ProgressTracker::new().update(data, now_ms);
    let mut seen_nodes = BTreeSet::new();
    let mut seen_handles = BTreeSet::new();
    let mut plans = Vec::new();
    for id in nodes {
        if !seen_nodes.insert(id.clone()) {
            continue;
        }
        let Some(node) = snapshot.node(id) else {
            plans.push(ManualPlan::Skip(ManualSkip {
                node: id.clone(),
                label: id.to_string(),
                reason: "not in the fleet any more".into(),
            }));
            continue;
        };
        let plan = match plan_node(node, data, &report, now_ms) {
            Ok(nudge) if !seen_handles.insert(nudge.handle.clone()) => Err(format!(
                "terminal {} is already nudged by this whip",
                nudge.handle
            )),
            other => other,
        };
        plans.push(match plan {
            Ok(nudge) => ManualPlan::Nudge(nudge),
            Err(reason) => ManualPlan::Skip(ManualSkip {
                node: node.id.clone(),
                label: node.label.clone(),
                reason,
            }),
        });
    }
    plans
}

fn plan_node(
    node: &Node,
    data: &FleetData,
    report: &ProgressReport,
    now_ms: i64,
) -> Result<ManualNudge, String> {
    match node.kind {
        NodeKind::Envoy => {
            return Err("the Envoy listens while idle; it does not need nudging".into());
        }
        NodeKind::Queen => return Err("the Queen is Myrstack itself".into()),
        NodeKind::Subagent => return Err("subagents have no terminal to nudge".into()),
        NodeKind::Coordinator | NodeKind::Attendant | NodeKind::Worker | NodeKind::Terminal => {}
    }
    if node.released_at_ms.is_some()
        || node.worker_terminal_state == Some(WorkerTerminalState::Released)
    {
        return Err("released worker".into());
    }
    let handle = node
        .terminal_handle
        .clone()
        .filter(|h| node.live && data.terminal(h).is_some())
        .ok_or_else(|| "its terminal is gone".to_string())?;
    let warn = (node.state == NodeState::NeedsInput).then(|| NEEDS_INPUT_WARNING.to_string());
    let output_idle = data
        .terminal(&handle)
        .and_then(|t| t.last_output_at.as_ref()?.epoch_ms())
        .map_or(0, |out| now_ms - out);
    let (targets, via, text) = match (node.kind, &node.run_id) {
        (NodeKind::Worker, Some(run_id)) if live_dispatch(node) => {
            worker_nudge(node, &handle, run_id, data, report, now_ms, output_idle)
        }
        (NodeKind::Coordinator, Some(run_id)) => {
            coordinator_nudge(&handle, run_id, report, now_ms, output_idle)
        }
        // Typing into a terminal that shows a prompt could answer it; use the bus instead.
        _ if warn.is_some() => (
            Vec::new(),
            NudgeVia::Bus {
                to: handle.clone(),
                run_id: node.run_id.clone(),
            },
            terminal_nudge_text(output_idle),
        ),
        _ => (
            Vec::new(),
            NudgeVia::Terminal {
                handle: handle.clone(),
            },
            terminal_nudge_text(output_idle),
        ),
    };
    Ok(ManualNudge {
        node: node.id.clone(),
        label: node.label.clone(),
        handle,
        targets,
        via,
        text,
        warn,
    })
}

fn live_dispatch(node: &Node) -> bool {
    node.dispatch_status == Some(DispatchStatus::Dispatched)
        && node.dispatch_id.is_some()
        && node.task_id.is_some()
}

type Route = (Vec<StallTarget>, NudgeVia, String);

fn worker_nudge(
    node: &Node,
    handle: &str,
    run_id: &str,
    data: &FleetData,
    report: &ProgressReport,
    now_ms: i64,
    output_idle: i64,
) -> Route {
    let task_id = node.task_id.clone().unwrap_or_default();
    let dispatch_id = node.dispatch_id.clone().unwrap_or_default();
    let idle = report
        .tasks
        .iter()
        .find(|t| t.task_id == task_id)
        .and_then(|t| t.last_progress_ms)
        .map_or(output_idle, |p| now_ms - p);
    let title = data
        .tasks
        .get(run_id)
        .and_then(|ts| ts.iter().find(|t| t.id == task_id))
        .map(|t| t.label().to_string())
        .filter(|l| *l != task_id);
    let text = worker_nudge_text(
        &[TaskRef {
            id: task_id.clone(),
            title,
        }],
        idle,
    );
    let target = StallTarget::Worker {
        handle: handle.to_string(),
        task_id,
        run_id: run_id.to_string(),
    };
    let via = NudgeVia::Bus {
        to: format!("dispatch:{dispatch_id}"),
        run_id: Some(run_id.to_string()),
    };
    (vec![target], via, text)
}

fn coordinator_nudge(
    handle: &str,
    run_id: &str,
    report: &ProgressReport,
    now_ms: i64,
    output_idle: i64,
) -> Route {
    let run = report.runs.iter().find(|r| r.run_id == run_id);
    let idle = run
        .and_then(|r| r.last_coordinator_activity_ms)
        .map_or(output_idle, |a| now_ms - a);
    let text = match run {
        Some(r) if !r.unanswered_asks.is_empty() => {
            unanswered_ask_text(run_id, &r.unanswered_asks[0], idle)
        }
        Some(r) if !r.open_ready_tasks.is_empty() => {
            coordinator_idle_text(run_id, &r.open_ready_tasks, &[], idle)
        }
        _ => quiet_run_text(run_id, idle),
    };
    let target = StallTarget::Coordinator {
        handle: handle.to_string(),
        run_id: run_id.to_string(),
    };
    let via = NudgeVia::Bus {
        to: handle.to_string(),
        run_id: Some(run_id.to_string()),
    };
    (vec![target], via, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::{Run, Task, Terminal, Timestamp, Worker};
    use serde_json::json;

    const RUN: &str = "run_1";
    const COORD: &str = "term_coord";
    const WORKER: &str = "term_worker";
    const LOOSE: &str = "term_loose";
    const MIN: i64 = 60_000;
    const T0: i64 = 1_790_000_000_000;
    const NOW: i64 = T0 + 20 * MIN;

    fn terminal(handle: &str) -> Terminal {
        Terminal {
            handle: handle.into(),
            last_output_at: Some(Timestamp::Millis(T0)),
            ..Default::default()
        }
    }

    fn task(id: &str, status: &str) -> Task {
        serde_json::from_value(json!({
            "id": id, "run_id": RUN, "status": status, "display_name": "Fix login",
            "created_by_terminal_handle": COORD, "created_at": T0 - 60 * MIN,
        }))
        .unwrap()
    }

    fn worker(task_id: &str, handle: &str, status: &str) -> Worker {
        serde_json::from_value(json!({
            "dispatchId": format!("ctx_{task_id}"), "taskId": task_id, "runId": RUN,
            "dispatchStatus": status, "agentTerminalHandle": handle,
        }))
        .unwrap()
    }

    fn fleet(dispatch_status: &str) -> FleetData {
        FleetData {
            polled_at_ms: NOW,
            terminals: vec![terminal(COORD), terminal(WORKER), terminal(LOOSE)],
            runs: vec![
                serde_json::from_value::<Run>(json!({
                    "id": RUN, "coordinator_handle": COORD,
                    "created_at": T0 - 60 * MIN, "updated_at": T0,
                }))
                .unwrap(),
            ],
            workers: vec![worker("t1", WORKER, dispatch_status)],
            tasks: [(RUN.to_string(), vec![task("t1", "dispatched")])].into(),
            ..Default::default()
        }
    }

    fn plan(data: &FleetData, ids: &[NodeId]) -> Vec<ManualPlan> {
        let snapshot = FleetSnapshot::build(data, NOW);
        plan_manual_whip(ids, data, &snapshot, NOW)
    }

    fn one_nudge(data: &FleetData, id: NodeId) -> ManualNudge {
        let plans = plan(data, &[id]);
        assert_eq!(plans.len(), 1);
        plans[0].nudge().cloned().expect("a nudge")
    }

    fn skip_reason(data: &FleetData, id: NodeId) -> String {
        let plans = plan(data, &[id]);
        plans[0].skip().expect("a skip").reason.clone()
    }

    #[test]
    fn worker_with_a_live_dispatch_gets_the_stall_nudge_on_its_dispatch() {
        let data = fleet("dispatched");
        let nudge = one_nudge(&data, NodeId::terminal(WORKER));
        assert_eq!(
            nudge.via,
            NudgeVia::Bus {
                to: "dispatch:ctx_t1".into(),
                run_id: Some(RUN.into()),
            }
        );
        assert_eq!(
            nudge.targets,
            vec![StallTarget::Worker {
                handle: WORKER.into(),
                task_id: "t1".into(),
                run_id: RUN.into(),
            }]
        );
        assert!(
            nudge
                .text
                .starts_with("Myrstack: no progress on task t1 (Fix login) for 20m."),
            "{}",
            nudge.text
        );
        assert_eq!(nudge.warn, None);
        assert_eq!(nudge.target_label(), "worker term_worker task t1");
    }

    #[test]
    fn worker_without_a_live_dispatch_is_typed_into() {
        let data = fleet("completed");
        let nudge = one_nudge(&data, NodeId::terminal(WORKER));
        assert_eq!(
            nudge.via,
            NudgeVia::Terminal {
                handle: WORKER.into()
            }
        );
        assert!(nudge.targets.is_empty());
        assert!(
            nudge
                .text
                .starts_with("Myrstack: Karl asks for a status check")
        );
        assert_eq!(nudge.target_label(), "terminal term_worker");
    }

    #[test]
    fn coordinator_gets_a_bus_nudge_with_its_run() {
        let data = fleet("dispatched");
        let nudge = one_nudge(&data, NodeId::terminal(COORD));
        assert_eq!(
            nudge.via,
            NudgeVia::Bus {
                to: COORD.into(),
                run_id: Some(RUN.into()),
            }
        );
        assert_eq!(
            nudge.targets,
            vec![StallTarget::Coordinator {
                handle: COORD.into(),
                run_id: RUN.into(),
            }]
        );
        assert!(
            nudge.text.starts_with("Myrstack: run run_1"),
            "{}",
            nudge.text
        );
    }

    #[test]
    fn coordinator_with_ready_tasks_is_told_about_them() {
        let mut data = fleet("dispatched");
        data.tasks.get_mut(RUN).unwrap().push(task("t2", "ready"));
        let nudge = one_nudge(&data, NodeId::terminal(COORD));
        assert!(
            nudge.text.contains("ready task(s) not started: t2"),
            "{}",
            nudge.text
        );
    }

    #[test]
    fn loose_terminal_is_typed_into() {
        let data = fleet("dispatched");
        let nudge = one_nudge(&data, NodeId::terminal(LOOSE));
        assert_eq!(
            nudge.via,
            NudgeVia::Terminal {
                handle: LOOSE.into()
            }
        );
        assert!(nudge.text.contains("last output was 20m ago"));
    }

    #[test]
    fn skips_the_queen_subagents_gone_and_released() {
        let data = fleet("dispatched");
        assert_eq!(
            skip_reason(&data, NodeId::queen()),
            "the Queen is Myrstack itself"
        );
        assert_eq!(
            skip_reason(&data, NodeId("nope".into())),
            "not in the fleet any more"
        );

        let snapshot = FleetSnapshot::build(&data, NOW);
        let mut sub = snapshot.node(&NodeId::terminal(LOOSE)).unwrap().clone();
        sub.kind = NodeKind::Subagent;
        let mut released = snapshot.node(&NodeId::terminal(WORKER)).unwrap().clone();
        released.worker_terminal_state = Some(WorkerTerminalState::Released);
        let mut dead = snapshot.node(&NodeId::terminal(COORD)).unwrap().clone();
        dead.live = false;
        let report = ProgressReport::default();
        assert_eq!(
            plan_node(&sub, &data, &report, NOW).unwrap_err(),
            "subagents have no terminal to nudge"
        );
        assert_eq!(
            plan_node(&released, &data, &report, NOW).unwrap_err(),
            "released worker"
        );
        assert_eq!(
            plan_node(&dead, &data, &report, NOW).unwrap_err(),
            "its terminal is gone"
        );
    }

    #[test]
    fn a_worker_whose_terminal_is_gone_is_skipped() {
        let mut data = fleet("dispatched");
        data.terminals.retain(|t| t.handle != WORKER);
        let snapshot = FleetSnapshot::build(&data, NOW);
        let worker = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Worker)
            .expect("a non-live worker node");
        let plans = plan_manual_whip(std::slice::from_ref(&worker.id), &data, &snapshot, NOW);
        assert_eq!(plans[0].skip().unwrap().reason, "its terminal is gone");
    }

    #[test]
    fn needs_input_targets_are_nudged_with_a_warning() {
        let data = fleet("dispatched");
        let snapshot = FleetSnapshot::build(&data, NOW);
        let mut node = snapshot.node(&NodeId::terminal(LOOSE)).unwrap().clone();
        node.state = NodeState::NeedsInput;
        let nudge = plan_node(&node, &data, &ProgressReport::default(), NOW).unwrap();
        assert_eq!(nudge.warn.as_deref(), Some(NEEDS_INPUT_WARNING));
        // Never typed into: that could answer the prompt.
        assert_eq!(
            nudge.via,
            NudgeVia::Bus {
                to: LOOSE.into(),
                run_id: None
            }
        );
        assert!(needs_confirmation(&[ManualPlan::Nudge(nudge)]));
    }

    #[test]
    fn duplicates_are_nudged_once() {
        let data = fleet("dispatched");
        let loose = NodeId::terminal(LOOSE);
        let plans = plan(&data, &[loose.clone(), loose.clone(), NodeId::queen()]);
        assert_eq!(plans.len(), 2);
        assert_eq!(nudges(&plans).len(), 1);
        assert!(!needs_confirmation(&plans));
    }

    #[test]
    fn many_targets_need_confirmation() {
        let data = fleet("dispatched");
        let nudge = one_nudge(&data, NodeId::terminal(LOOSE));
        let plans: Vec<ManualPlan> = (0..CONFIRM_AT)
            .map(|_| ManualPlan::Nudge(nudge.clone()))
            .collect();
        assert!(needs_confirmation(&plans));
        assert!(!needs_confirmation(&plans[..CONFIRM_AT - 1]));
    }
}

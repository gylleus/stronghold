use super::*;
use crate::orca::Timestamp;
use crate::orca::commands::{
    parse_run_list, parse_task_list, parse_terminal_list, parse_worker_list,
};

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../../tests/fixtures/orca/", $name))
    };
}

const RUN: &str = "run_7c25cdcf46e0";
const LOOSE: &str = "term_08fe0725-96a1-4355-a4fd-6c8a07aa6c72";
const COORDINATOR: &str = "term_70c8df0b-cf78-4b43-a6f8-aefab63bca21";
const LIVE_WORKER: &str = "term_1744da75-821f-46fe-912d-e252e0bbf3fb";
/// Coordinator's last output + 10 s.
const NOW: i64 = 1_790_319_595_528 + 10_000;
const MINUTE: i64 = 60_000;

/// A coherent fleet built from the captured fixtures:
/// - run `run_7c25…` coordinated by the Claude terminal, with the Codex terminal as its live
///   worker (active dispatch `ctx_a4a6…` on task `task_e024…`), which also served an older
///   completed dispatch;
/// - the shell terminal stays loose;
/// - released workers 5 and 20 minutes ago, a circuit-broken worker whose terminal is gone,
///   and a retained worker of a run with no live terminal.
fn scenario() -> FleetData {
    let terminals = parse_terminal_list(fixture!("terminal_list.json"))
        .unwrap()
        .terminals;
    let mut runs = parse_run_list(fixture!("run_list.json")).unwrap();
    let tasks = parse_task_list(fixture!("task_list.json")).unwrap();
    let workers = parse_worker_list(fixture!("worker_list.json"))
        .unwrap()
        .workers;

    runs[0].coordinator_handle = Some(COORDINATOR.into());
    let by_id = |id: &str| {
        workers
            .iter()
            .find(|w| w.dispatch_id == id)
            .unwrap()
            .clone()
    };
    let into_run = |mut w: Worker| {
        w.run_id = Some(RUN.into());
        w
    };

    let mut live = into_run(by_id("ctx_a4a62d1e6146"));
    live.agent_terminal_handle = Some(LIVE_WORKER.into());
    live.task_id = Some("task_e02460f3b723".into());

    let mut earlier = into_run(by_id("ctx_055d47949c82"));
    earlier.agent_terminal_handle = Some(LIVE_WORKER.into());
    earlier.task_id = Some("task_65485013f22c".into());

    let released = |dispatch: &str, ago: i64| {
        let mut w = into_run(by_id("ctx_efb8c6c722b8"));
        w.dispatch_id = dispatch.into();
        w.task_id = Some("task_1803b842d880".into());
        let resource = w.resource.as_mut().unwrap();
        resource.release_completed_at = Some(Timestamp::Millis(NOW - ago));
        w
    };

    let dead = into_run(by_id("ctx_5606e1fe5584"));
    let other_run = by_id("ctx_e89e8238e52b");

    FleetData {
        polled_at_ms: NOW,
        terminals,
        runs,
        workers: vec![
            earlier,
            live,
            released("ctx_recent", 5 * MINUTE),
            released("ctx_old", 20 * MINUTE),
            dead,
            other_run,
        ],
        tasks: [(RUN.to_string(), tasks)].into_iter().collect(),
        ..Default::default()
    }
}

fn term(handle: &str) -> NodeId {
    NodeId::terminal(handle)
}

#[test]
fn run_has_coordinator_workers_and_task_counts() {
    let snap = FleetSnapshot::build(&scenario(), NOW);

    assert_eq!(snap.runs.len(), 1);
    let run = &snap.runs[0];
    assert_eq!(run.id, RUN);
    assert!(
        run.objective
            .as_deref()
            .unwrap()
            .starts_with("commerce-platform wave")
    );
    assert_eq!(run.coordinator, Some(term(COORDINATOR)));
    let counts = run.task_counts.unwrap();
    assert_eq!(
        (counts.completed, counts.dispatched, counts.failed),
        (1, 1, 1)
    );
    assert_eq!(counts.total(), 3);

    let coord = snap.node(&term(COORDINATOR)).unwrap();
    assert_eq!(coord.kind, NodeKind::Coordinator);
    assert_eq!(coord.label, "Hack 77 core in template");
    assert_eq!(coord.run_id.as_deref(), Some(RUN));
    assert_eq!(coord.agent, Some(AgentIdentity::Claude));
    assert_eq!(
        coord.branch.as_deref(),
        Some("gylleus/hack-77-service-shops")
    );
    assert_eq!(coord.state, NodeState::Working);

    let worker = snap.node(&term(LIVE_WORKER)).unwrap();
    assert_eq!(worker.kind, NodeKind::Worker);
    assert!(worker.live);
    assert_eq!(worker.dispatch_id.as_deref(), Some("ctx_a4a62d1e6146"));
    assert_eq!(worker.task_id.as_deref(), Some("task_e02460f3b723"));
    assert_eq!(worker.label, "hack-77-parallel-recipes");
    assert_eq!(worker.state, NodeState::Idle);

    let mut in_run: Vec<_> = snap.nodes_in_run(RUN).map(|n| n.id.as_str()).collect();
    in_run.sort();
    assert_eq!(
        in_run,
        vec![
            format!("dispatch:{RUN}:ctx_5606e1fe5584").as_str(),
            format!("dispatch:{RUN}:ctx_recent").as_str(),
            term(LIVE_WORKER).as_str(),
            term(COORDINATOR).as_str(),
        ]
    );

    assert_eq!(snap.edges.len(), 3);
    assert!(snap.edges.iter().all(|e| e.from == term(COORDINATOR)
        && e.kind == EdgeKind::CoordinatorWorker
        && snap.node(&e.to).unwrap().kind == NodeKind::Worker));
}

#[test]
fn loose_terminal_is_labelled_by_title() {
    let snap = FleetSnapshot::build(&scenario(), NOW);
    let loose: Vec<_> = snap.loose_nodes().collect();
    assert_eq!(loose.len(), 1);
    assert_eq!(loose[0].id, term(LOOSE));
    assert_eq!(loose[0].label, "..play-sessions");
    assert_eq!(loose[0].run_id, None);
    assert_eq!(loose[0].state, NodeState::Idle);
}

#[test]
fn released_workers_fade_within_window_only() {
    let snap = FleetSnapshot::build(&scenario(), NOW);
    let recent = snap.node(&NodeId::dispatch(RUN, "ctx_recent")).unwrap();
    assert_eq!(recent.released_at_ms, Some(NOW - 5 * MINUTE));
    assert_eq!(recent.state, NodeState::Done);
    assert!(!recent.live);
    assert!(recent.label.starts_with("hack-77-merchandising-recipe"));
    assert!(recent.label.ends_with('…'));
    assert_eq!(recent.label.chars().count(), MAX_LABEL_CHARS);
    assert!(snap.node(&NodeId::dispatch(RUN, "ctx_old")).is_none());
}

#[test]
fn worker_with_dead_terminal_is_failed() {
    let snap = FleetSnapshot::build(&scenario(), NOW);
    let dead = snap
        .node(&NodeId::dispatch(RUN, "ctx_5606e1fe5584"))
        .unwrap();
    assert_eq!(dead.kind, NodeKind::Worker);
    assert!(!dead.live);
    assert_eq!(
        dead.terminal_handle.as_deref(),
        Some("term_ab8b3c04-8a42-4bff-a299-cec43c385ca4")
    );
    assert_eq!(dead.state, NodeState::Failed);
    assert_eq!(dead.released_at_ms, None);
}

#[test]
fn dead_worker_of_a_run_without_live_terminals_is_dropped() {
    let snap = FleetSnapshot::build(&scenario(), NOW);
    assert!(snap.nodes_in_run("run_9558d04ad3b3").next().is_none());
    assert!(snap.run("run_9558d04ad3b3").is_none());
    // The older dispatch on the live worker's terminal is not shown separately.
    assert!(
        snap.nodes
            .iter()
            .all(|n| n.dispatch_id.as_deref() != Some("ctx_055d47949c82"))
    );
}

#[test]
fn node_ids_are_stable_across_polls() {
    let mut data = scenario();
    let first = FleetSnapshot::build(&data, NOW);

    data.terminals.reverse();
    data.workers.reverse();
    data.terminals[0].last_output_at = Some(Timestamp::Millis(NOW + 1_000));
    let second = FleetSnapshot::build(&data, NOW + 2_000);

    let ids = |s: &FleetSnapshot| {
        let mut ids: Vec<NodeId> = s.nodes.iter().map(|n| n.id.clone()).collect();
        ids.sort();
        ids
    };
    assert_eq!(ids(&first), ids(&second));
    assert_eq!(
        second.node(&term(LIVE_WORKER)).unwrap().dispatch_id,
        first.node(&term(LIVE_WORKER)).unwrap().dispatch_id
    );
}

#[test]
fn most_recent_active_dispatch_wins_a_shared_terminal() {
    let mut data = scenario();
    // Put the active dispatch first so position alone would pick the older one.
    data.workers.swap(0, 1);
    let snap = FleetSnapshot::build(&data, NOW);
    let worker = snap.node(&term(LIVE_WORKER)).unwrap();
    assert_eq!(worker.dispatch_id.as_deref(), Some("ctx_a4a62d1e6146"));
}

#[test]
fn spinner_glyphs_are_stripped() {
    assert_eq!(strip_spinner("✳ Hack 77"), "Hack 77");
    assert_eq!(strip_spinner("⠂ Review PR"), "Review PR");
    assert_eq!(strip_spinner("..play-sessions"), "..play-sessions");
    assert_eq!(strip_spinner("✳ "), "");
}

#[test]
fn terminal_label_falls_back_to_branch_then_handle() {
    let mut t = Terminal {
        handle: "term_x".into(),
        title: Some("⠂ ".into()),
        branch: Some("refs/heads/gylleus/feature".into()),
        ..Default::default()
    };
    assert_eq!(label::terminal_label(&t), "gylleus/feature");
    t.branch = None;
    assert_eq!(label::terminal_label(&t), "term_x");
}

#[test]
fn derive_state_placeholder_rules() {
    let terminal = Terminal {
        last_output_at: Some(Timestamp::Millis(NOW - 29_000)),
        ..Default::default()
    };
    let state = |terminal, worker, now_ms| {
        derive_state(&StateInputs {
            terminal,
            worker,
            now_ms,
        })
    };
    assert_eq!(state(Some(&terminal), None, NOW), NodeState::Working);
    assert_eq!(state(Some(&terminal), None, NOW + 2_000), NodeState::Idle);
    assert_eq!(state(None, None, NOW), NodeState::Idle);

    let data = scenario();
    let active = &data.workers[1];
    assert_eq!(
        state(Some(&terminal), Some(active), NOW),
        NodeState::Working
    );
    assert_eq!(state(None, Some(active), NOW), NodeState::Failed);
    let succeeded = &data.workers[0];
    assert_eq!(
        state(Some(&terminal), Some(succeeded), NOW),
        NodeState::Done
    );
}

#[test]
fn empty_data_gives_empty_snapshot() {
    let snap = FleetSnapshot::build(&FleetData::default(), NOW);
    assert!(snap.runs.is_empty() && snap.nodes.is_empty() && snap.edges.is_empty());
}

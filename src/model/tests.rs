use super::*;
use crate::orca::commands::{
    parse_run_list, parse_task_list, parse_terminal_list, parse_worker_list,
};
use crate::orca::{Run, Timestamp};

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
    // Its preview shows Codex's "Press enter to confirm" model-switch menu.
    assert_eq!(worker.state, NodeState::NeedsInput);

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
fn derive_state_on_fixture_workers() {
    let terminal = Terminal {
        connected: true,
        last_output_at: Some(Timestamp::Millis(NOW - 29_000)),
        ..Default::default()
    };
    let state =
        |terminal, worker, now_ms| derive_state(&StateInputs::new(terminal, worker, now_ms));
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

fn coordinator_edges(snap: &FleetSnapshot, from: &str) -> Vec<NodeId> {
    let mut to: Vec<NodeId> = snap
        .edges
        .iter()
        .filter(|e| e.from == term(from) && e.kind == EdgeKind::CoordinatorWorker)
        .map(|e| e.to.clone())
        .collect();
    to.sort();
    to
}

fn run_workers(snap: &FleetSnapshot, run_id: &str) -> Vec<NodeId> {
    let mut ids: Vec<NodeId> = snap
        .nodes_in_run(run_id)
        .filter(|n| n.kind == NodeKind::Worker)
        .map(|n| n.id.clone())
        .collect();
    ids.sort();
    ids
}

#[test]
fn rebound_coordinator_is_the_coordinator_node() {
    let mut data = scenario();
    data.runs[0].coordinator_handle = Some("term_dead".into());
    data.tasks.get_mut(RUN).unwrap()[0].created_by_terminal_handle = Some(LOOSE.into());
    let snap = FleetSnapshot::build(&data, NOW);

    let run = snap.run(RUN).unwrap();
    assert_eq!(run.coordinator, Some(term(LOOSE)));
    let coord = snap.node(&term(LOOSE)).unwrap();
    assert_eq!(coord.kind, NodeKind::Coordinator);
    assert_eq!(coord.run_id.as_deref(), Some(RUN));
    // The terminal that used to be declared coordinator is now loose.
    assert_eq!(
        snap.node(&term(COORDINATOR)).unwrap().kind,
        NodeKind::Terminal
    );

    assert_eq!(snap.edges.len(), 3);
    assert_eq!(coordinator_edges(&snap, LOOSE), run_workers(&snap, RUN));
}

#[test]
fn dead_declared_coordinator_without_candidates_gives_none() {
    let mut data = scenario();
    data.runs[0].coordinator_handle = Some("term_dead".into());
    let snap = FleetSnapshot::build(&data, NOW);

    let run = snap.run(RUN).unwrap();
    assert_eq!(run.coordinator, None);
    assert!(snap.edges.is_empty());
    assert!(snap.nodes.iter().all(|n| n.kind != NodeKind::Coordinator));
}

#[test]
fn live_declared_coordinator_beats_other_task_creators() {
    let mut data = scenario();
    data.tasks.get_mut(RUN).unwrap()[0].created_by_terminal_handle = Some(LOOSE.into());
    let snap = FleetSnapshot::build(&data, NOW);

    assert_eq!(snap.run(RUN).unwrap().coordinator, Some(term(COORDINATOR)));
    assert_eq!(snap.node(&term(LOOSE)).unwrap().kind, NodeKind::Terminal);
}

#[test]
fn shared_coordinator_has_one_node_with_edges_to_both_runs() {
    const OTHER: &str = "run_other";
    let mut data = scenario();
    data.runs.push(Run {
        id: OTHER.into(),
        coordinator_handle: Some(COORDINATOR.into()),
        ..Default::default()
    });
    let mut worker = data.workers[1].clone();
    worker.run_id = Some(OTHER.into());
    worker.dispatch_id = "ctx_other".into();
    worker.task_id = Some("task_other".into());
    worker.agent_terminal_handle = Some(LOOSE.into());
    data.workers.push(worker);
    let snap = FleetSnapshot::build(&data, NOW);

    let coordinators: Vec<_> = snap
        .nodes
        .iter()
        .filter(|n| n.id == term(COORDINATOR))
        .collect();
    assert_eq!(coordinators.len(), 1);
    assert_eq!(coordinators[0].kind, NodeKind::Coordinator);
    // The run the poller fetched tasks for owns the node.
    assert_eq!(coordinators[0].run_id.as_deref(), Some(RUN));

    assert_eq!(
        snap.run(OTHER).unwrap().coordinator,
        Some(term(COORDINATOR))
    );
    assert_eq!(run_workers(&snap, OTHER), vec![term(LOOSE)]);
    let mut expected = run_workers(&snap, RUN);
    expected.push(term(LOOSE));
    expected.sort();
    assert_eq!(coordinator_edges(&snap, COORDINATOR), expected);
    assert_eq!(snap.edges.len(), 4);
}

#[test]
fn coordinator_also_listed_as_worker_stays_coordinator() {
    let mut data = scenario();
    let mut worker = data.workers[5].clone();
    worker.agent_terminal_handle = Some(COORDINATOR.into());
    data.workers.push(worker);
    let snap = FleetSnapshot::build(&data, NOW);

    let coord = snap.node(&term(COORDINATOR)).unwrap();
    assert_eq!(coord.kind, NodeKind::Coordinator);
    assert_eq!(coord.run_id.as_deref(), Some(RUN));
    assert_eq!(snap.run(RUN).unwrap().coordinator, Some(term(COORDINATOR)));
}

/// Live check against this machine's Orca: prints declared vs effective coordinator per
/// active run. Read-only. Run with `cargo test live_coordinators -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_coordinators() {
    use crate::orca::OrcaClient;
    use crate::poller::{ACTIVE_RUN_WINDOW, FleetSource, active_run_ids};

    let client = OrcaClient::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut data = FleetData {
        polled_at_ms: now,
        terminals: FleetSource::terminals(&client).unwrap(),
        runs: FleetSource::runs(&client).unwrap(),
        workers: FleetSource::workers(&client).unwrap(),
        inbox: FleetSource::inbox(&client, 300).unwrap(),
        ..Default::default()
    };
    let active = active_run_ids(
        &data.runs,
        &data.workers,
        &data.terminals,
        now,
        ACTIVE_RUN_WINDOW,
    );
    for run_id in &active {
        data.tasks
            .insert(run_id.clone(), FleetSource::tasks(&client, run_id).unwrap());
    }
    let snap = FleetSnapshot::build(&data, now);

    let live = |h: &str| {
        if data.terminal(h).is_some() {
            "live"
        } else {
            "dead"
        }
    };
    println!("run | declared | effective | snapshot coordinator | workers | edges");
    for run in data.runs.iter().filter(|r| active.contains(&r.id)) {
        let declared = run
            .coordinator_handle
            .as_deref()
            .map(|h| format!("{h} ({})", live(h)))
            .unwrap_or_else(|| "-".into());
        let effective = data
            .effective_coordinator(&run.id)
            .unwrap_or_else(|| "-".into());
        let summary = snap.run(&run.id);
        let in_snap = summary
            .and_then(|r| r.coordinator.as_ref())
            .map(|n| n.to_string())
            .unwrap_or_else(|| {
                if summary.is_some() {
                    "-".into()
                } else {
                    "(run not shown)".into()
                }
            });
        let workers = snap
            .nodes_in_run(&run.id)
            .filter(|n| n.kind == NodeKind::Worker)
            .count();
        let edges = summary
            .and_then(|r| r.coordinator.as_ref())
            .map(|c| {
                snap.edges
                    .iter()
                    .filter(|e| {
                        &e.from == c && snap.node(&e.to).unwrap().run_id.as_deref() == Some(&run.id)
                    })
                    .count()
            })
            .unwrap_or(0);
        println!(
            "{} | {declared} | {effective} | {in_snap} | {workers} | {edges}",
            run.id
        );
    }
}

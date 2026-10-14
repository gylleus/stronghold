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

/// A coherent clan built from the captured fixtures:
/// - run `run_7c25…` coordinated by the Claude terminal, with the Codex terminal as its live
///   worker (active dispatch `ctx_a4a6…` on task `task_e024…`), which also served an older
///   completed dispatch;
/// - the shell terminal stays loose;
/// - released workers 5 and 20 minutes ago, a circuit-broken worker whose terminal is gone,
///   and a retained worker of a run with no live terminal.
fn scenario() -> ClanData {
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

    ClanData {
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
    let snap = ClanSnapshot::build(&scenario(), NOW);

    assert_eq!(snap.runs.len(), 1);
    let run = &snap.runs[0];
    assert_eq!(run.id, RUN);
    assert!(
        run.objective
            .as_deref()
            .unwrap()
            .starts_with("storefront wave")
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
    assert_eq!(coord.branch.as_deref(), Some("karl/hack-77-service-shops"));
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

    assert_eq!(clan_edges(&snap).len(), 3);
    assert!(clan_edges(&snap).iter().all(|e| e.from == term(COORDINATOR)
        && e.kind == EdgeKind::CoordinatorWorker
        && snap.node(&e.to).unwrap().kind == NodeKind::Worker));
}

#[test]
fn escalated_handles_render_stalled() {
    let data = scenario();
    let stalled = BTreeSet::from([
        COORDINATOR.to_string(),
        LIVE_WORKER.to_string(),
        LOOSE.to_string(),
    ]);
    let snap = ClanSnapshot::build_with_stalled(&data, None, &stalled, NOW);
    // Stalled beats Working and Idle, but not NeedsInput.
    assert_eq!(
        snap.node(&term(COORDINATOR)).unwrap().state,
        NodeState::Stalled
    );
    assert_eq!(snap.node(&term(LOOSE)).unwrap().state, NodeState::Stalled);
    assert_eq!(
        snap.node(&term(LIVE_WORKER)).unwrap().state,
        NodeState::NeedsInput
    );
    let plain = ClanSnapshot::build_with_claude(&data, None, NOW);
    assert_eq!(
        plain.node(&term(COORDINATOR)).unwrap().state,
        NodeState::Working
    );
}

#[test]
fn loose_terminal_is_labelled_by_title() {
    let snap = ClanSnapshot::build(&scenario(), NOW);
    let loose: Vec<_> = snap.loose_nodes().collect();
    assert_eq!(loose.len(), 1);
    assert_eq!(loose[0].id, term(LOOSE));
    assert_eq!(loose[0].label, "..play-sessions");
    assert_eq!(loose[0].run_id, None);
    assert_eq!(loose[0].state, NodeState::Idle);
}

/// A name chosen for the worktree (by the King at spawn, or in Orca) beats the title.
#[test]
fn loose_terminal_goes_by_its_worktree_name() {
    let mut data = scenario();
    let path = data
        .terminal(LOOSE)
        .and_then(|t| t.worktree_path.clone())
        .expect("the loose terminal has a worktree");
    data.worktree_names.insert(
        path.trim_end_matches('/').to_string(),
        "Replay sessions".into(),
    );
    let snap = ClanSnapshot::build(&data, NOW);
    assert_eq!(snap.node(&term(LOOSE)).unwrap().label, "Replay sessions");
}

#[test]
fn released_workers_fade_within_window_only() {
    let snap = ClanSnapshot::build(&scenario(), NOW);
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
    let snap = ClanSnapshot::build(&scenario(), NOW);
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
    let snap = ClanSnapshot::build(&scenario(), NOW);
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
    let first = ClanSnapshot::build(&data, NOW);

    data.terminals.reverse();
    data.workers.reverse();
    data.terminals[0].last_output_at = Some(Timestamp::Millis(NOW + 1_000));
    let second = ClanSnapshot::build(&data, NOW + 2_000);

    let ids = |s: &ClanSnapshot| {
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
    let snap = ClanSnapshot::build(&data, NOW);
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
        branch: Some("refs/heads/karl/feature".into()),
        ..Default::default()
    };
    assert_eq!(label::terminal_label(&t), "karl/feature");
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
fn empty_data_gives_only_the_king() {
    let snap = ClanSnapshot::build(&ClanData::default(), NOW);
    assert!(snap.runs.is_empty() && snap.edges.is_empty());
    let ids: Vec<&str> = snap.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, ["king"]);
    let king = snap.king().unwrap();
    assert_eq!(king.state, NodeState::Idle);
    assert_eq!(king.preview.as_deref(), Some("0 coordinators · 0 dwarves"));
}

/// Edges other than the king's.
fn clan_edges(snap: &ClanSnapshot) -> Vec<&Edge> {
    snap.edges
        .iter()
        .filter(|e| e.kind != EdgeKind::King)
        .collect()
}

#[test]
fn king_links_every_coordinator_and_loose_terminal() {
    let snap = ClanSnapshot::build(&scenario(), NOW);
    let kings: Vec<&Node> = snap
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::King)
        .collect();
    assert_eq!(kings.len(), 1);
    let king = kings[0];
    assert_eq!(king.id, NodeId::king());
    assert_eq!(king.id.as_str(), "king");
    assert_eq!(king.label, "King");
    assert_eq!(king.terminal_handle, None);
    assert_eq!(snap.node(&NodeId::king()), Some(king));

    let mut linked: Vec<&NodeId> = snap
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::King)
        .inspect(|e| assert_eq!(e.from, king.id))
        .map(|e| &e.to)
        .collect();
    linked.sort();
    let mut expected: Vec<&NodeId> = snap
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Coordinator | NodeKind::Terminal))
        .map(|n| &n.id)
        .collect();
    expected.sort();
    assert_eq!(linked, expected);
    assert_eq!(linked, [&term(LOOSE), &term(COORDINATOR)]);

    let agents = snap.agent_nodes().count();
    assert_eq!(agents, snap.nodes.len() - 1);
    assert_eq!(
        king.preview.as_deref(),
        Some(format!("1 coordinator · {agents} dwarves").as_str())
    );
}

#[test]
fn king_is_rebuilt_not_duplicated() {
    let mut snap = ClanSnapshot::build(&scenario(), NOW);
    let edges = snap.edges.len();
    snap.add_king();
    snap.add_king();
    assert_eq!(
        snap.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::King)
            .count(),
        1
    );
    assert_eq!(snap.edges.len(), edges);
    assert!(snap.node(&NodeId::king()).is_some());
}

#[test]
fn king_state_is_the_most_urgent_clan_state() {
    let with = |states: &[NodeState]| {
        let nodes: Vec<Node> = states
            .iter()
            .map(|s| {
                let mut n = king::king_node(&[]);
                n.kind = NodeKind::Worker;
                n.state = *s;
                n
            })
            .collect();
        clan_state(&nodes)
    };
    use NodeState::*;
    assert_eq!(with(&[]), Idle);
    assert_eq!(with(&[Done, Failed, Idle]), Idle);
    assert_eq!(with(&[Idle, Working, Done]), Working);
    assert_eq!(with(&[Working, Stalled, Idle]), Stalled);
    assert_eq!(with(&[Stalled, NeedsInput, Working]), NeedsInput);

    let mut data = scenario();
    let stalled = std::collections::BTreeSet::from([COORDINATOR.to_string()]);
    let snap = ClanSnapshot::build_with_stalled(&data, None, &stalled, NOW);
    // The scenario's live worker needs input, which outranks the escalated coordinator.
    assert_eq!(snap.king().unwrap().state, NeedsInput);
    data.needs_input.clear();
    data.terminals.retain(|t| t.handle != LIVE_WORKER);
    let snap = ClanSnapshot::build_with_stalled(&data, None, &stalled, NOW);
    assert_eq!(snap.king().unwrap().state, Stalled);
}

mod subagents {
    use std::path::PathBuf;

    use super::*;
    use crate::claude::{
        ClaudeData, SessionInfo, SessionMatch, SubagentInfo, SubagentParent, SubagentState,
    };

    const SESSION: &str = "s1";

    fn session() -> SessionInfo {
        SessionInfo {
            session_id: SESSION.into(),
            transcript_path: PathBuf::from("/p/s1.jsonl"),
            dir: None,
            modified_ms: NOW,
            cwd_from_transcript: None,
        }
    }

    fn agent(
        id: &str,
        parent: Option<&str>,
        state: SubagentState,
        finished_ago: i64,
    ) -> SubagentInfo {
        SubagentInfo {
            agent_id: id.into(),
            session_id: SESSION.into(),
            agent_type: "general-purpose".into(),
            description: format!("Review {id}"),
            model: Some("opus".into()),
            spawn_depth: if parent.is_some() { 2 } else { 1 },
            tool_use_id: Some(format!("toolu_{id}")),
            parent: parent.map_or(SubagentParent::Session, |p| SubagentParent::Agent(p.into())),
            background: true,
            started_ms: NOW - 30 * MINUTE,
            last_activity_ms: NOW - finished_ago,
            state,
            finished_ms: state.is_finished().then_some(NOW - finished_ago),
            transcript_path: PathBuf::from(format!("/p/s1/subagents/agent-{id}.jsonl")),
        }
    }

    fn claude(agents: Vec<SubagentInfo>) -> ClaudeData {
        ClaudeData {
            session_by_handle: [
                (COORDINATOR.to_string(), SessionMatch::Unique(session())),
                ("term_gone".to_string(), SessionMatch::Unique(session())),
            ]
            .into_iter()
            .collect(),
            subagents: [(SESSION.to_string(), agents)].into_iter().collect(),
            at_ms: NOW,
            ..Default::default()
        }
    }

    fn sub(id: &str) -> NodeId {
        NodeId::subagent(SESSION, id)
    }

    #[test]
    fn subagents_hang_off_their_terminal_and_parent() {
        let mut described = agent("a1", None, SubagentState::Running, 0);
        described.description = "A very long description that goes past the label limit".into();
        let mut untitled = agent("a3", None, SubagentState::Stale, 0);
        untitled.description = String::new();
        untitled.finished_ms = None;
        let data = claude(vec![
            described,
            agent("a2", Some("a1"), SubagentState::Running, 0),
            untitled,
            agent("a4", Some("gone"), SubagentState::Failed, MINUTE),
        ]);
        let snap = ClanSnapshot::build_with_claude(&scenario(), Some(&data), NOW);
        let plain = ClanSnapshot::build(&scenario(), NOW);
        assert_eq!(snap.nodes.len(), plain.nodes.len() + 4);

        let a1 = snap.node(&sub("a1")).unwrap();
        assert_eq!(a1.kind, NodeKind::Subagent);
        assert_eq!(a1.state, NodeState::Working);
        assert!(a1.label.ends_with('…') && a1.label.starts_with("A very long"));
        assert_eq!(a1.run_id.as_deref(), Some(RUN));
        assert_eq!(a1.terminal_handle, None);
        assert_eq!(a1.released_at_ms, None);
        assert_eq!(sub("a1").subagent_parts(), Some((SESSION, "a1")));
        assert_eq!(term(COORDINATOR).subagent_parts(), None);

        let a3 = snap.node(&sub("a3")).unwrap();
        assert_eq!(
            (a3.label.as_str(), a3.state),
            ("general-purpose", NodeState::Idle)
        );
        let a4 = snap.node(&sub("a4")).unwrap();
        assert_eq!(a4.state, NodeState::Failed);
        assert_eq!(a4.released_at_ms, Some(NOW - MINUTE));

        let edge_from = |id: &str| {
            let edges: Vec<&Edge> = snap.edges.iter().filter(|e| e.to == sub(id)).collect();
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].kind, EdgeKind::Subagent);
            edges[0].from.clone()
        };
        assert_eq!(edge_from("a1"), term(COORDINATOR));
        assert_eq!(edge_from("a2"), sub("a1"));
        assert_eq!(
            edge_from("a4"),
            term(COORDINATOR),
            "missing parent falls back"
        );
    }

    #[test]
    fn finished_subagents_fade_then_drop() {
        let data = claude(vec![
            agent("done", None, SubagentState::Done, 5 * MINUTE),
            agent("old", None, SubagentState::Done, RELEASED_FADE_MS + 1),
            agent("child", Some("old"), SubagentState::Running, 0),
        ]);
        let snap = ClanSnapshot::build_with_claude(&scenario(), Some(&data), NOW);
        let done = snap.node(&sub("done")).unwrap();
        assert_eq!(done.state, NodeState::Done);
        assert_eq!(done.released_at_ms, Some(NOW - 5 * MINUTE));
        assert!(snap.node(&sub("old")).is_none());
        let child = snap.edges.iter().find(|e| e.to == sub("child")).unwrap();
        assert_eq!(child.from, term(COORDINATOR));
    }

    #[test]
    fn sessions_without_a_terminal_node_are_ignored() {
        let mut data = claude(vec![agent("a1", None, SubagentState::Running, 0)]);
        data.session_by_handle.remove(COORDINATOR);
        let snap = ClanSnapshot::build_with_claude(&scenario(), Some(&data), NOW);
        assert!(snap.nodes.iter().all(|n| n.kind != NodeKind::Subagent));
        assert!(snap.edges.iter().all(|e| e.kind != EdgeKind::Subagent));
        let none = ClanSnapshot::build_with_claude(&scenario(), None, NOW);
        assert_eq!(none.nodes, ClanSnapshot::build(&scenario(), NOW).nodes);
    }
}

fn coordinator_edges(snap: &ClanSnapshot, from: &str) -> Vec<NodeId> {
    let mut to: Vec<NodeId> = snap
        .edges
        .iter()
        .filter(|e| e.from == term(from) && e.kind == EdgeKind::CoordinatorWorker)
        .map(|e| e.to.clone())
        .collect();
    to.sort();
    to
}

fn run_workers(snap: &ClanSnapshot, run_id: &str) -> Vec<NodeId> {
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
    let snap = ClanSnapshot::build(&data, NOW);

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

    assert_eq!(clan_edges(&snap).len(), 3);
    assert_eq!(coordinator_edges(&snap, LOOSE), run_workers(&snap, RUN));
}

#[test]
fn dead_declared_coordinator_without_candidates_gives_none() {
    let mut data = scenario();
    data.runs[0].coordinator_handle = Some("term_dead".into());
    let snap = ClanSnapshot::build(&data, NOW);

    let run = snap.run(RUN).unwrap();
    assert_eq!(run.coordinator, None);
    assert!(clan_edges(&snap).is_empty());
    assert!(snap.nodes.iter().all(|n| n.kind != NodeKind::Coordinator));
}

#[test]
fn live_declared_coordinator_that_creates_tasks_stays_coordinator() {
    let mut data = scenario();
    let mut own = data.tasks[RUN][0].clone();
    own.id = "task_own".into();
    own.created_by_terminal_handle = Some(COORDINATOR.into());
    let tasks = data.tasks.get_mut(RUN).unwrap();
    tasks[0].created_by_terminal_handle = Some(LOOSE.into());
    tasks.push(own);
    let snap = ClanSnapshot::build(&data, NOW);

    assert_eq!(snap.run(RUN).unwrap().coordinator, Some(term(COORDINATOR)));
    assert_eq!(snap.node(&term(LOOSE)).unwrap().kind, NodeKind::Terminal);
    assert!(snap.attendants.is_empty());
}

/// The declared coordinator is an attendant: LOOSE creates the run's tasks and the declared
/// one forwards to it.
fn attendant_scenario() -> ClanData {
    let mut data = scenario();
    data.terminals
        .iter_mut()
        .find(|t| t.handle == COORDINATOR)
        .unwrap()
        .title = Some("✳ cp-wave-0923-ATTENDANT brief".into());
    data.tasks.get_mut(RUN).unwrap()[0].created_by_terminal_handle = Some(LOOSE.into());
    data.forwards.insert(
        COORDINATOR.into(),
        vec![crate::clan::Forward {
            target: LOOSE.into(),
            at_ms: Some(NOW),
        }],
    );
    data
}

/// A coordinator whose topic is the attendant brief: its title says "attendant" but it
/// creates the run's tasks, so the king links it as the coordinator.
#[test]
fn attendant_titled_coordinator_is_not_an_attendant() {
    let mut data = scenario();
    data.terminals
        .iter_mut()
        .find(|t| t.handle == COORDINATOR)
        .unwrap()
        .title = Some("◑ Attendant brief review".into());
    data.tasks.get_mut(RUN).unwrap()[0].created_by_terminal_handle = Some(COORDINATOR.into());
    let snap = ClanSnapshot::build(&data, NOW);

    assert_eq!(snap.run(RUN).unwrap().coordinator, Some(term(COORDINATOR)));
    assert_eq!(
        snap.node(&term(COORDINATOR)).unwrap().kind,
        NodeKind::Coordinator
    );
    assert!(snap.attendants.is_empty());
}

#[test]
fn attendant_hangs_off_the_real_coordinator() {
    let snap = ClanSnapshot::build(&attendant_scenario(), NOW);

    let run = snap.run(RUN).unwrap();
    assert_eq!(run.coordinator, Some(term(LOOSE)));
    assert_eq!(snap.attendant(RUN), Some(&term(COORDINATOR)));
    let coordinator = snap.node(&term(LOOSE)).unwrap();
    assert_eq!(coordinator.kind, NodeKind::Coordinator);
    let attendant = snap.node(&term(COORDINATOR)).unwrap();
    assert_eq!(attendant.kind, NodeKind::Attendant);
    assert_eq!(attendant.run_id.as_deref(), Some(RUN));
    assert!(!snap.coordinator_gone(attendant));

    let attendant_edges: Vec<&Edge> = snap
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Attendant)
        .collect();
    assert_eq!(attendant_edges.len(), 1);
    assert_eq!(attendant_edges[0].from, term(LOOSE));
    assert_eq!(attendant_edges[0].to, term(COORDINATOR));
    // Workers stay with the coordinator; the king links the coordinator, not the attendant.
    assert_eq!(coordinator_edges(&snap, LOOSE), run_workers(&snap, RUN));
    let king_to = |id: &NodeId| {
        snap.edges
            .iter()
            .any(|e| e.kind == EdgeKind::King && &e.to == id)
    };
    assert!(king_to(&term(LOOSE)));
    assert!(!king_to(&term(COORDINATOR)));
}

#[test]
fn attendant_without_coordinator_links_to_the_king() {
    let mut data = attendant_scenario();
    data.terminals.retain(|t| t.handle != LOOSE);
    let snap = ClanSnapshot::build(&data, NOW);

    let run = snap.run(RUN).expect("the attendant keeps the run shown");
    assert_eq!(run.coordinator, None);
    let attendant = snap.node(&term(COORDINATOR)).unwrap();
    assert_eq!(attendant.kind, NodeKind::Attendant);
    assert!(snap.coordinator_gone(attendant));
    assert!(
        snap.edges
            .iter()
            .any(|e| e.kind == EdgeKind::King && e.to == attendant.id)
    );
    assert!(snap.edges.iter().all(|e| e.kind != EdgeKind::Attendant));
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
    let snap = ClanSnapshot::build(&data, NOW);

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
    assert_eq!(clan_edges(&snap).len(), 4);
}

#[test]
fn coordinator_also_listed_as_worker_stays_coordinator() {
    let mut data = scenario();
    let mut worker = data.workers[5].clone();
    worker.agent_terminal_handle = Some(COORDINATOR.into());
    data.workers.push(worker);
    let snap = ClanSnapshot::build(&data, NOW);

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
    use crate::poller::{ACTIVE_RUN_WINDOW, ClanSource, active_run_ids};

    let client = OrcaClient::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut data = ClanData {
        polled_at_ms: now,
        terminals: ClanSource::terminals(&client).unwrap(),
        runs: ClanSource::runs(&client).unwrap(),
        workers: ClanSource::workers(&client).unwrap(),
        inbox: ClanSource::inbox(&client, 300).unwrap(),
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
            .insert(run_id.clone(), ClanSource::tasks(&client, run_id).unwrap());
    }
    let snap = ClanSnapshot::build(&data, now);

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

fn finished(node_id: &str, at_ms: i64) -> crate::finish::FinishedSession {
    crate::finish::FinishedSession {
        node_id: node_id.into(),
        handle: node_id.strip_prefix("term:").map(str::to_string),
        label: "dwarf".into(),
        outcome: crate::finish::FinishOutcome::Done,
        approval: crate::finish::Approval::User,
        summary: "merged into main".into(),
        pr_url: None,
        forced: Vec::new(),
        worktree_path: None,
        branch: None,
        at_ms,
    }
}

#[test]
fn a_terminal_waiting_on_karls_answer_needs_input() {
    let mut snap = ClanSnapshot::build(&scenario(), NOW);
    let loose = NodeId::terminal(LOOSE);
    assert_ne!(snap.node(&loose).unwrap().state, NodeState::NeedsInput);
    let waiting = [
        (LOOSE.to_string(), "Merge now?".to_string()),
        ("term_gone".to_string(), "Ignored?".to_string()),
    ]
    .into_iter()
    .collect();
    snap.mark_waiting(&waiting);
    let node = snap.node(&loose).unwrap();
    assert_eq!(node.state, NodeState::NeedsInput);
    assert_eq!(node.preview.as_deref(), Some("Asks: Merge now?"));
    assert_eq!(snap.king().unwrap().state, NodeState::NeedsInput);
    assert_eq!(
        snap.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::King)
            .count(),
        1
    );
}

#[test]
fn a_finished_session_shows_done_then_fades_once_closed() {
    let mut snap = ClanSnapshot::build(&scenario(), NOW);
    let nodes = snap.nodes.len();
    let edges = snap.edges.len();
    snap.add_finished(&[
        finished(&NodeId::terminal(LOOSE).0, NOW - MINUTE),
        finished("term:term_closed", NOW - MINUTE),
    ]);
    // Still open (the close is on its way): the same node, done.
    let live = snap.node(&NodeId::terminal(LOOSE)).unwrap();
    assert_eq!(live.state, NodeState::Done);
    assert_eq!(live.preview.as_deref(), Some("Finished: merged into main"));
    assert_eq!(live.released_at_ms, None);
    // Closed: back as a fading done terminal, linked to the king.
    let gone = snap.node(&NodeId("term:term_closed".into())).unwrap();
    assert_eq!(gone.state, NodeState::Done);
    assert!(!gone.live);
    assert_eq!(gone.kind, NodeKind::Terminal);
    assert_eq!(gone.released_at_ms, Some(NOW - MINUTE));
    assert_eq!(snap.nodes.len(), nodes + 1);
    assert_eq!(snap.edges.len(), edges + 1);
    assert!(snap.edges.iter().any(|e| e.to.0 == "term:term_closed"));
}

fn parked(node_id: &NodeId, at_ms: i64) -> crate::park::ParkedAgent {
    crate::park::ParkedAgent {
        node_id: node_id.to_string(),
        handle: node_id.as_str().trim_start_matches("term:").into(),
        label: "parked".into(),
        worktree_path: None,
        branch: None,
        at_ms,
    }
}

#[test]
fn a_parked_agent_leaves_the_kings_spokes_and_state() {
    let mut snap = ClanSnapshot::build(&scenario(), NOW);
    let loose = term(LOOSE);
    snap.mark_waiting(&[(LOOSE.to_string(), "Ship it?".to_string())].into());
    assert_eq!(snap.node(&loose).unwrap().state, NodeState::NeedsInput);
    let spoke = |s: &ClanSnapshot| {
        s.edges
            .iter()
            .any(|e| e.kind == EdgeKind::King && e.to == loose)
    };
    assert!(spoke(&snap));

    let mut sleepers: Vec<_> = snap
        .agent_nodes()
        .filter(|n| n.state == NodeState::NeedsInput && n.id != loose)
        .map(|n| parked(&n.id, NOW))
        .collect();
    sleepers.extend([parked(&loose, NOW - MINUTE), parked(&term("gone"), NOW)]);
    snap.mark_parked(&sleepers);
    let node = snap.node(&loose).unwrap();
    assert_eq!(node.parked_at_ms, Some(NOW - MINUTE));
    assert_eq!(node.state, NodeState::NeedsInput, "parking keeps the state");
    assert!(!spoke(&snap));
    assert_ne!(snap.king().unwrap().state, NodeState::NeedsInput);
    assert!(
        snap.king()
            .unwrap()
            .preview
            .as_deref()
            .unwrap()
            .ends_with(&format!("· {} parked", sleepers.len() - 1))
    );
    assert_eq!(snap.parked_nodes().count(), sleepers.len() - 1);
}

#[test]
fn envoy_overlay_uses_handle_not_title_and_keeps_king_edge() {
    let data = scenario();
    let mut snapshot = ClanSnapshot::build(&data, NOW);
    let status = crate::envoy::EnvoyStatus {
        session: crate::envoy::SessionState::Running,
        bridge: crate::envoy::BridgeStatus::Ok,
        pending_drafts: 2,
        dry_run: true,
        stuck_events: 0,
        poll_stale: false,
    };
    snapshot.mark_envoy(Some(LOOSE), status);
    let envoy = snapshot
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Envoy)
        .unwrap();
    assert_eq!(envoy.terminal_handle.as_deref(), Some(LOOSE));
    assert_eq!(envoy.label, "Envoy · waiting on approval");
    assert!(
        snapshot
            .edges
            .iter()
            .any(|e| e.from == NodeId::king() && e.to == envoy.id && e.kind == EdgeKind::King)
    );
    let mut fresh = ClanSnapshot::build(&data, NOW);
    fresh.mark_envoy(Some("absent"), status);
    assert!(!fresh.nodes.iter().any(|n| n.kind == NodeKind::Envoy));
}

#[test]
fn idle_envoy_has_no_automatic_or_manual_whip() {
    let data = ClanData {
        terminals: vec![crate::orca::Terminal {
            handle: "envoy".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut snapshot = ClanSnapshot::build(&data, NOW);
    snapshot.mark_envoy(
        Some("envoy"),
        crate::envoy::EnvoyStatus {
            session: crate::envoy::SessionState::Running,
            bridge: crate::envoy::BridgeStatus::Missing,
            pending_drafts: 0,
            dry_run: true,
            stuck_events: 0,
            poll_stale: false,
        },
    );
    let plans = crate::whip::plan_manual_whip(&[NodeId::terminal("envoy")], &data, &snapshot, NOW);
    assert!(crate::whip::manual::nudges(&plans).is_empty());
    let mut engine = crate::whip::WhipEngine::new();
    let settings = crate::whip::WhipSettings {
        enabled: true,
        ..Default::default()
    };
    assert!(
        engine
            .tick(&data, &Default::default(), &settings, NOW)
            .is_empty()
    );
    assert!(
        engine
            .tick(
                &data,
                &Default::default(),
                &settings,
                NOW + 24 * 60 * MINUTE
            )
            .is_empty()
    );
}

#[test]
fn saved_queen_kind_loads_as_the_king() {
    let kind: NodeKind = serde_json::from_str("\"Queen\"").unwrap();
    assert_eq!(kind, NodeKind::King);
    assert_eq!(serde_json::to_string(&NodeKind::King).unwrap(), "\"King\"");
}

use super::*;
use crate::model::{NodeState, RunSummary, TaskCounts};

fn node(id: &str, kind: NodeKind, handle: Option<&str>) -> Node {
    Node {
        id: NodeId(id.to_string()),
        kind,
        label: id.to_string(),
        terminal_handle: handle.map(str::to_string),
        live: handle.is_some(),
        run_id: None,
        task_id: None,
        dispatch_id: None,
        agent: None,
        worktree_path: None,
        branch: None,
        last_output_ms: None,
        preview: None,
        state: NodeState::Working,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
    }
}

fn worker(id: &str, handle: Option<&str>, dispatch: &str, active: bool) -> Node {
    let mut n = node(id, NodeKind::Worker, handle);
    n.run_id = Some("run_1".into());
    n.dispatch_id = Some(dispatch.into());
    if active {
        n.worker_state = Some(WorkerState::Ready);
        n.dispatch_status = Some(DispatchStatus::Dispatched);
    } else {
        n.worker_state = Some(WorkerState::Succeeded);
        n.dispatch_status = Some(DispatchStatus::Completed);
    }
    n
}

fn terminal(handle: &str, tab: &str) -> Terminal {
    Terminal {
        handle: handle.into(),
        tab_id: Some(tab.into()),
        title: Some(format!("{handle} title")),
        ..Terminal::default()
    }
}

fn close(handle: &str, tab: bool) -> CloseStep {
    CloseStep::CloseTerminal {
        handle: handle.into(),
        tab,
    }
}

fn stop(dispatch: &str, handle: Option<&str>) -> CloseStep {
    CloseStep::WorkerStop {
        dispatch_id: dispatch.into(),
        handle: handle.map(str::to_string),
    }
}

fn snapshot() -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    s.runs = vec![RunSummary {
        id: "run_1".into(),
        objective: Some("ship it".into()),
        coordinator: Some(NodeId("coord".into())),
        task_counts: Some(TaskCounts {
            pending: 1,
            ready: 1,
            dispatched: 2,
            completed: 5,
            ..TaskCounts::default()
        }),
    }];
    s
}

const PANE: CloseOptions = CloseOptions { whole_tab: false };
const TAB: CloseOptions = CloseOptions { whole_tab: true };

#[test]
fn active_worker_is_fenced_then_closed() {
    let w = worker("w", Some("t_w"), "d1", true);
    let plan = plan_close(&w, &snapshot(), &[], PANE);
    assert_eq!(plan.refused, None);
    assert_eq!(
        plan.steps,
        vec![stop("d1", Some("t_w")), close("t_w", false)]
    );
    assert!(plan.warnings.is_empty());
    assert!(plan.can_run());
    assert_eq!(
        plan.steps[0].command(),
        "orca orchestration worker-stop --dispatch=d1"
    );
    assert_eq!(
        plan.steps[1].command(),
        "orca terminal close --terminal=t_w"
    );
}

#[test]
fn unsupervised_and_stop_unknown_dispatches_count_as_active() {
    let mut w = worker("w", Some("t_w"), "d1", true);
    w.worker_state = Some(WorkerState::Unsupervised);
    assert!(has_active_dispatch(&w));
    w.worker_state = Some(WorkerState::StopUnknown);
    assert!(has_active_dispatch(&w));
    w.worker_state = Some(WorkerState::Stopped);
    assert!(!has_active_dispatch(&w));
    let mut failed = worker("w", Some("t_w"), "d1", true);
    failed.dispatch_status = Some(DispatchStatus::CircuitBroken);
    assert!(!has_active_dispatch(&failed));
}

#[test]
fn settled_worker_just_closes_its_terminal() {
    let w = worker("w", Some("t_w"), "d1", false);
    let plan = plan_close(&w, &snapshot(), &[], PANE);
    assert_eq!(plan.steps, vec![close("t_w", false)]);
}

#[test]
fn dead_active_worker_is_only_fenced() {
    let mut w = worker("w", Some("t_w"), "d1", true);
    w.live = false;
    let plan = plan_close(&w, &snapshot(), &[], PANE);
    assert_eq!(plan.steps, vec![stop("d1", Some("t_w"))]);
    assert_eq!(plan.warnings.len(), 1);
}

#[test]
fn released_worker_is_refused() {
    let mut w = worker("w", Some("t_w"), "d1", false);
    w.live = false;
    w.released_at_ms = Some(1);
    let plan = plan_close(&w, &snapshot(), &[], PANE);
    assert!(plan.refused.unwrap().contains("already released"));
    assert!(plan.steps.is_empty());
}

#[test]
fn coordinator_close_warns_about_its_run() {
    let mut c = node("coord", NodeKind::Coordinator, Some("t_c"));
    c.run_id = Some("run_1".into());
    let plan = plan_close(&c, &snapshot(), &[], PANE);
    assert_eq!(plan.steps, vec![close("t_c", false)]);
    assert_eq!(
        plan.warnings,
        vec!["run ship it loses its coordinator; its workers keep running (4 open tasks)"]
    );
}

#[test]
fn loose_terminal_closes_directly() {
    let t = node("loose", NodeKind::Terminal, Some("t_l"));
    let plan = plan_close(&t, &snapshot(), &[], PANE);
    assert_eq!(plan.steps, vec![close("t_l", false)]);
    assert!(plan.warnings.is_empty() && plan.refused.is_none());
}

#[test]
fn subagent_is_refused() {
    let s = node("sub", NodeKind::Subagent, None);
    let plan = plan_close(&s, &snapshot(), &[], PANE);
    assert_eq!(plan.refused.as_deref(), Some(SUBAGENT_REFUSAL));
    assert!(!plan.can_run());
}

#[test]
fn whole_tab_closes_the_tab_and_names_its_other_terminals() {
    let t = node("loose", NodeKind::Terminal, Some("t_l"));
    let terminals = [
        terminal("t_l", "tab_1"),
        terminal("t_other", "tab_1"),
        terminal("t_else", "tab_2"),
    ];
    let plan = plan_close(&t, &snapshot(), &terminals, TAB);
    assert_eq!(plan.steps, vec![close("t_l", true)]);
    assert_eq!(
        plan.steps[0].command(),
        "orca terminal close --terminal=t_l --tab"
    );
    assert_eq!(
        plan.warnings,
        vec!["loose: closing the whole tab also closes 1 other terminal: t_other title"]
    );
}

#[test]
fn whole_tab_of_a_fenced_worker_goes_through_a_sibling() {
    // worker-stop closes the worker's own pane, so its handle is stale by the tab close.
    let w = worker("w", Some("t_w"), "d1", true);
    let terminals = [terminal("t_w", "tab_1"), terminal("t_shell", "tab_1")];
    let plan = plan_close(&w, &snapshot(), &terminals, TAB);
    assert_eq!(
        plan.steps,
        vec![stop("d1", Some("t_w")), close("t_shell", true)]
    );
    // Alone in its tab: worker-stop's close takes the tab along; the tab close is a no-op.
    let alone = plan_close(&w, &snapshot(), &[terminal("t_w", "tab_1")], TAB);
    assert_eq!(
        alone.steps,
        vec![stop("d1", Some("t_w")), close("t_w", true)]
    );
    assert!(alone.warnings.is_empty());
}

#[test]
fn bulk_merges_stops_first_and_dedupes() {
    let w1 = worker("w1", Some("t_1"), "d1", true);
    let w2 = worker("w2", Some("t_2"), "d2", false);
    let loose = node("loose", NodeKind::Terminal, Some("t_1"));
    let sub = node("sub", NodeKind::Subagent, None);
    let nodes = [&w1, &w2, &loose, &w1, &sub];
    let plan = plan_bulk_close(nodes, &snapshot(), &[], PANE);
    assert_eq!(
        plan.steps,
        vec![
            stop("d1", Some("t_1")),
            close("t_1", false),
            close("t_2", false)
        ]
    );
    let targets: Vec<&str> = plan.targets.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(targets, vec!["w1", "w2", "loose"]);
    assert_eq!(
        plan.warnings,
        vec![format!("skipped sub: {SUBAGENT_REFUSAL}")]
    );
    assert!(plan.can_run());
}

#[test]
fn bulk_whole_tab_closes_each_tab_once() {
    let a = node("a", NodeKind::Terminal, Some("t_a"));
    let b = node("b", NodeKind::Terminal, Some("t_b"));
    let c = node("c", NodeKind::Terminal, Some("t_c"));
    let terminals = [
        terminal("t_a", "tab_1"),
        terminal("t_b", "tab_1"),
        terminal("t_c", "tab_2"),
    ];
    let plan = plan_bulk_close([&a, &b, &c], &snapshot(), &terminals, TAB);
    assert_eq!(plan.steps, vec![close("t_a", true), close("t_c", true)]);
}

#[test]
fn bulk_pane_close_is_dropped_when_its_tab_is_closed() {
    let dedupe = |closes| {
        let tab_of = HashMap::from([("t_a", "tab_1"), ("t_b", "tab_1")]);
        dedupe_closes(closes, &tab_of)
    };
    assert_eq!(
        dedupe(vec![
            close("t_b", false),
            close("t_a", true),
            close("t_a", false)
        ]),
        vec![close("t_a", true)]
    );
}

#[test]
fn bulk_of_nothing_is_refused() {
    let sub = node("sub", NodeKind::Subagent, None);
    let plan = plan_bulk_close([&sub], &snapshot(), &[], PANE);
    assert_eq!(plan.refused.as_deref(), Some("nothing to close"));
    assert!(plan.steps.is_empty());
}

#[test]
fn bulk_candidates_skip_hidden_coordinators_and_dead_nodes() {
    let mut s = snapshot();
    let mut dead = worker("dead", Some("t_d"), "d9", false);
    dead.live = false;
    s.nodes = vec![
        node("coord", NodeKind::Coordinator, Some("t_c")),
        worker("w", Some("t_w"), "d1", true),
        node("loose", NodeKind::Terminal, Some("t_l")),
        node("hidden", NodeKind::Terminal, Some("t_h")),
        node("sub", NodeKind::Subagent, None),
        dead,
    ];
    let hidden = BTreeSet::from([NodeId("hidden".into())]);
    let ids = |include| {
        bulk_candidates(&s, &hidden, include)
            .iter()
            .map(|n| n.id.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(false), vec!["w", "loose"]);
    assert_eq!(ids(true), vec!["coord", "w", "loose"]);
}

#[test]
fn queen_is_refused_and_never_a_bulk_candidate() {
    let mut s = snapshot();
    s.add_queen();
    let queen = s.queen().unwrap().clone();
    let plan = plan_close(&queen, &s, &[], PANE);
    assert_eq!(plan.refused.as_deref(), Some(QUEEN_REFUSAL));
    assert!(!plan.can_run());
    for coordinators in [false, true] {
        assert!(
            bulk_candidates(&s, &BTreeSet::new(), coordinators)
                .iter()
                .all(|n| n.kind != NodeKind::Queen)
        );
    }
}

#[test]
fn tab_companions_lists_other_sessions_in_the_targets_tabs() {
    let mut s = snapshot();
    s.nodes = vec![
        node("a", NodeKind::Terminal, Some("t_a")),
        node("b", NodeKind::Terminal, Some("t_b")),
        node("term:t_other", NodeKind::Terminal, Some("t_other")),
    ];
    s.add_queen();
    let terminals = [
        terminal("t_a", "tab_1"),
        terminal("t_other", "tab_1"),
        terminal("t_b", "tab_2"),
        terminal("t_lone", "tab_3"),
    ];
    let targets = |ids: &[&str]| -> Vec<CloseTarget> {
        ids.iter()
            .map(|id| CloseTarget::of(s.node(&NodeId(id.to_string())).unwrap()))
            .collect()
    };
    // The sibling's node label wins over the terminal title.
    assert_eq!(
        tab_companions(&targets(&["a"]), &s, &terminals),
        vec!["term:t_other"]
    );
    // Nothing else in b's tab.
    assert!(tab_companions(&targets(&["b"]), &s, &terminals).is_empty());
    // A target is never listed as its own companion, and siblings are listed once.
    let mut both = targets(&["a", "b"]);
    both.push(CloseTarget::of(
        s.node(&NodeId("term:t_other".into())).unwrap(),
    ));
    assert!(tab_companions(&both, &s, &terminals).is_empty());
}

#[test]
fn bulk_close_of_a_selection_skips_the_queen_and_subagents() {
    let mut s = snapshot();
    s.add_queen();
    let queen = s.queen().unwrap().clone();
    let loose = node("loose", NodeKind::Terminal, Some("t_1"));
    let sub = node("sub", NodeKind::Subagent, None);
    let plan = plan_bulk_close([&queen, &loose, &sub], &s, &[], PANE);
    assert!(plan.can_run());
    assert_eq!(plan.steps, vec![close("t_1", false)]);
    assert_eq!(
        plan.warnings,
        vec![
            format!("skipped {}: {QUEEN_REFUSAL}", queen.label),
            format!("skipped sub: {SUBAGENT_REFUSAL}"),
        ]
    );
    // Nothing closable left: refused, but the skips still explain why.
    let none = plan_bulk_close([&queen, &sub], &s, &[], PANE);
    assert!(!none.can_run());
    assert_eq!(none.warnings.len(), 2);
}

#[test]
fn attendant_closes_with_a_forwarding_warning() {
    let mut a = node("att", NodeKind::Attendant, Some("t_att"));
    a.run_id = Some("run_1".into());
    let plan = plan_close(&a, &snapshot(), &[], PANE);
    assert_eq!(plan.refused, None);
    assert_eq!(plan.steps, vec![close("t_att", false)]);
    assert_eq!(
        plan.warnings,
        vec!["att is the attendant of run ship it: bus events stop reaching its coordinator"]
    );
    let s = snapshot();
    let mut with = s.clone();
    with.nodes = vec![a];
    assert!(bulk_candidates(&with, &BTreeSet::new(), false).is_empty());
    assert_eq!(bulk_candidates(&with, &BTreeSet::new(), true).len(), 1);
}

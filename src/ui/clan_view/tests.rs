//! Selection and metadata used by the farm and side panels.
use super::*;
use crate::model::{Edge, EdgeKind, NodeState, RunSummary};
const RUN: &str = "run_a";
fn id(s: &str) -> NodeId {
    NodeId(s.to_string())
}
fn node(name: &str, kind: NodeKind, run: Option<&str>) -> Node {
    Node {
        id: id(name),
        kind,
        label: name.to_string(),
        terminal_handle: None,
        live: true,
        run_id: run.map(str::to_string),
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
        parked_at_ms: None,
    }
}
fn clan() -> ClanSnapshot {
    let mut s = ClanSnapshot::default();
    s.runs = vec![RunSummary {
        id: RUN.into(),
        objective: None,
        coordinator: Some(id("coord")),
        task_counts: None,
    }];
    s.nodes
        .push(node("coord", NodeKind::Coordinator, Some(RUN)));
    s.nodes.push(node("loose", NodeKind::Terminal, None));
    for w in ["w1", "w2", "w3"] {
        s.nodes.push(node(w, NodeKind::Worker, Some(RUN)));
        s.edges.push(Edge {
            from: id("coord"),
            to: id(w),
            kind: EdgeKind::CoordinatorWorker,
        });
    }
    s
}

#[test]
fn clicks_keep_a_visible_primary_and_toggle_selection() {
    let mut view = ClanView::new();
    view.sync(&clan());
    view.click_node(&id("w1"), false);
    view.click_node(&id("w2"), true);
    assert_eq!(view.selection(), BTreeSet::from([id("w1"), id("w2")]));
    assert_eq!(view.selected(), Some(id("w2")));
    view.click_node(&id("w2"), true);
    assert_eq!(view.selected(), Some(id("w1")));
    view.click_node(&id("w1"), false);
    assert!(view.selection().is_empty());
    view.set_selection(&BTreeSet::from([id("w2"), id("w3")]), Some(&id("missing")));
    assert_eq!(view.selected(), Some(id("w2")));
}

#[test]
fn hidden_and_vanished_nodes_leave_selection_without_restoring_it() {
    let mut snapshot = clan();
    let mut view = ClanView::new();
    view.sync(&snapshot);
    view.set_selection(&BTreeSet::from([id("w1"), id("w2")]), Some(&id("w2")));
    view.set_hidden(&BTreeSet::from([id("w2")]));
    assert_eq!(view.selected(), Some(id("w1")));
    assert!(!view.is_visible(&id("w2")));
    view.set_hidden(&BTreeSet::new());
    assert_eq!(view.selection(), BTreeSet::from([id("w1")]));
    snapshot.nodes.retain(|n| n.id != id("w1"));
    view.sync(&snapshot);
    assert!(view.selection().is_empty());
    assert_eq!(view.selected(), None);
    view.click_node(&id("missing"), false);
    assert!(view.selection().is_empty());
}

#[test]
fn badges_and_usage_survive_snapshot_refreshes() {
    let mut view = ClanView::new();
    let badge = UnreadBadge {
        count: 3,
        severity: crate::notify::Severity::Info,
    };
    view.set_unread(HashMap::from([(id("w1"), badge)]));
    view.set_pr_ready(HashSet::from([id("w1")]));
    view.set_building(HashSet::from([id("w2")]));
    view.set_usage(HashMap::from([(id("w1"), Usage::default())]));
    view.sync(&clan());
    assert_eq!(view.unread(&id("w1")), Some(badge));
    assert!(view.pr_ready().contains(&id("w1")));
    assert!(view.building().contains(&id("w2")));
    assert!(view.usage.contains_key(&id("w1")));
    view.set_unread(HashMap::new());
    view.set_pr_ready(HashSet::new());
    assert!(view.unread(&id("w1")).is_none());
    assert!(view.pr_ready().is_empty());
}

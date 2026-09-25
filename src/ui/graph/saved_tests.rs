use std::collections::BTreeSet;

use eframe::egui::Pos2;

use super::*;
use crate::model::{Edge, EdgeKind, NodeState, RunSummary};

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
    }
}

fn snapshot(workers: &[&str]) -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    s.runs = vec![RunSummary {
        id: "run".into(),
        objective: None,
        coordinator: Some(id("coord")),
        task_counts: None,
    }];
    s.nodes
        .push(node("coord", NodeKind::Coordinator, Some("run")));
    for w in workers {
        s.nodes.push(node(w, NodeKind::Worker, Some("run")));
        s.edges.push(Edge {
            from: id("coord"),
            to: id(w),
            kind: EdgeKind::CoordinatorWorker,
        });
    }
    s
}

fn saved(positions: &[(&str, [f32; 2])]) -> SavedLayout {
    SavedLayout {
        positions: positions.iter().map(|(n, p)| (id(n), *p)).collect(),
        ..SavedLayout::default()
    }
}

#[test]
fn saved_positions_apply_on_first_sight() {
    let mut view = GraphView::new();
    view.import_layout(saved(&[("coord", [10.0, 20.0]), ("w1", [300.0, -40.0])]));
    view.sync(&snapshot(&["w1"]));

    assert_eq!(
        view.node_position(&id("coord")),
        Some(Pos2::new(10.0, 20.0))
    );
    assert_eq!(view.node_position(&id("w1")), Some(Pos2::new(300.0, -40.0)));
    assert_eq!(view.pending_positions(), 0);
}

#[test]
fn saved_positions_are_ignored_after_first_sight() {
    let mut view = GraphView::new();
    view.import_layout(saved(&[("w1", [300.0, -40.0])]));
    view.sync(&snapshot(&["w1"]));
    view.set_node_position(&id("w1"), Pos2::new(5.0, 5.0));

    // The worker leaves and comes back: it spawns fresh instead of at its saved spot.
    view.sync(&snapshot(&[]));
    view.sync(&snapshot(&["w1"]));
    let coord = view.node_position(&id("coord")).unwrap();
    let w1 = view.node_position(&id("w1")).unwrap();
    assert!(((w1 - coord).length() - SPAWN_NEAR).abs() < 0.01);
}

#[test]
fn saved_position_waits_for_a_node_that_appears_later() {
    let mut view = GraphView::new();
    view.import_layout(saved(&[("w2", [7.0, 8.0])]));
    view.sync(&snapshot(&["w1"]));
    assert_eq!(view.pending_positions(), 1);
    view.sync(&snapshot(&["w1", "w2"]));
    assert_eq!(view.node_position(&id("w2")), Some(Pos2::new(7.0, 8.0)));
}

#[test]
fn unknown_saved_ids_are_ignored() {
    let mut view = GraphView::new();
    view.import_layout(saved(&[("gone", [1.0, 2.0]), ("w1", [3.0, 4.0])]));
    view.sync(&snapshot(&["w1"]));

    assert_eq!(view.node_count(), 2);
    assert!(view.node_position(&id("gone")).is_none());
    // Positions of nodes absent at save time are not carried forward.
    let exported = view.export_layout();
    assert!(!exported.positions.contains_key(&id("gone")));
    assert_eq!(exported.positions[&id("w1")], [3.0, 4.0]);
}

#[test]
fn non_finite_saved_positions_are_dropped() {
    let mut view = GraphView::new();
    view.import_layout(saved(&[("w1", [f32::NAN, 0.0])]));
    view.sync(&snapshot(&["w1"]));
    assert!(view.node_position(&id("w1")).unwrap().x.is_finite());
}

#[test]
fn export_includes_hidden_nodes_and_round_trips() {
    let mut view = GraphView::new();
    view.sync(&snapshot(&["w1", "w2"]));
    view.set_node_position(&id("w1"), Pos2::new(11.0, 12.0));
    view.set_node_position(&id("w2"), Pos2::new(21.0, 22.0));
    view.set_hidden(&BTreeSet::from([id("w2")]));
    view.set_selected(Some(&id("w1")));

    let exported = view.export_layout();
    assert_eq!(exported.positions.len(), 3);
    assert_eq!(exported.positions[&id("w2")], [21.0, 22.0]);
    assert_eq!(exported.selected, Some(id("w1")));

    let mut restored = GraphView::new();
    restored.import_layout(exported);
    restored.sync(&snapshot(&["w1", "w2"]));
    assert_eq!(
        restored.node_position(&id("w1")),
        Some(Pos2::new(11.0, 12.0))
    );
    assert_eq!(
        restored.node_position(&id("w2")),
        Some(Pos2::new(21.0, 22.0))
    );
    assert_eq!(restored.selected(), Some(id("w1")));
}

#[test]
fn export_before_first_sync_keeps_restored_positions() {
    let mut view = GraphView::new();
    let layout = saved(&[("w1", [1.0, 2.0])]);
    view.import_layout(layout.clone());
    assert_eq!(view.export_layout().positions, layout.positions);
}

#[test]
fn export_caps_positions() {
    let mut view = GraphView::new();
    let names: Vec<String> = (0..MAX_SAVED_POSITIONS + 10)
        .map(|i| format!("w{i}"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    view.sync(&snapshot(&refs));
    assert_eq!(view.export_layout().positions.len(), MAX_SAVED_POSITIONS);
}

#[test]
fn manual_view_skips_auto_fit_on_first_population() {
    let mut view = GraphView::new();
    view.import_layout(SavedLayout {
        view: Some(SavedView {
            zoom: 2.0,
            pan: [5.0, 6.0],
        }),
        manual_view: true,
        ..saved(&[])
    });
    view.sync(&snapshot(&["w1"]));
    assert!(!view.auto_fit);
    assert!(view.export_layout().manual_view);

    let mut fresh = GraphView::new();
    fresh.import_layout(saved(&[]));
    fresh.sync(&snapshot(&["w1"]));
    assert!(fresh.auto_fit);
}

#[test]
fn reset_layout_forgets_saved_positions_and_refits() {
    let mut view = GraphView::new();
    view.import_layout(SavedLayout {
        manual_view: true,
        view: Some(SavedView {
            zoom: 2.0,
            pan: [0.0, 0.0],
        }),
        ..saved(&[("w1", [900.0, 900.0]), ("w2", [1.0, 1.0])])
    });
    view.sync(&snapshot(&["w1"]));
    view.set_selected(Some(&id("w1")));

    view.reset_layout();
    let coord = view.node_position(&id("coord")).unwrap();
    let w1 = view.node_position(&id("w1")).unwrap();
    assert!(((w1 - coord).length() - SPAWN_NEAR).abs() < 0.01);
    assert_eq!(view.pending_positions(), 0);
    assert_eq!(view.selected(), Some(id("w1")));
    assert!(view.auto_fit);
    assert!(!view.export_layout().manual_view);
}

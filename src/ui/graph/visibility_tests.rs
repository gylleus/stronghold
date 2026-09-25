use std::collections::BTreeSet;

use eframe::egui::{self, Pos2, Rect, Vec2};

use super::*;
use crate::model::{Edge, EdgeKind, NodeState, RunSummary};

fn node(id: &str, kind: NodeKind, run: Option<&str>) -> Node {
    Node {
        id: NodeId(id.to_string()),
        kind,
        label: id.to_string(),
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

fn id(s: &str) -> NodeId {
    NodeId(s.to_string())
}

fn ids(list: &[&str]) -> BTreeSet<NodeId> {
    list.iter().map(|s| id(s)).collect()
}

/// Run `a` (coordinator `ca`, workers `a1`, `a2`), run `b` (coordinator `cb`, worker `b1`)
/// and a loose terminal.
fn snapshot() -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    for (run, coord, workers) in [("a", "ca", &["a1", "a2"][..]), ("b", "cb", &["b1"][..])] {
        s.runs.push(RunSummary {
            id: run.into(),
            objective: None,
            coordinator: Some(id(coord)),
            task_counts: None,
        });
        s.nodes.push(node(coord, NodeKind::Coordinator, Some(run)));
        for w in workers {
            s.nodes.push(node(w, NodeKind::Worker, Some(run)));
            s.edges.push(Edge {
                from: id(coord),
                to: id(w),
                kind: EdgeKind::CoordinatorWorker,
            });
        }
    }
    s.nodes.push(node("loose", NodeKind::Terminal, None));
    s
}

fn view_with_positions() -> GraphView {
    let mut view = GraphView::new();
    view.sync(&snapshot());
    for (i, n) in ["ca", "a1", "a2", "cb", "b1", "loose"].iter().enumerate() {
        view.set_node_position(&id(n), Pos2::new(i as f32 * 100.0, i as f32 * 10.0));
    }
    view
}

#[test]
fn hiding_removes_nodes_and_their_edges() {
    let mut view = view_with_positions();
    view.set_hidden(&ids(&["a1", "loose"]));

    assert_eq!(view.node_count(), 4);
    assert_eq!(view.edge_count(), 2);
    assert!(!view.is_visible(&id("a1")));
    assert!(!view.contains_edge(&id("ca"), &id("a1")));
    assert!(view.contains_edge(&id("ca"), &id("a2")));
}

#[test]
fn hide_then_show_restores_positions() {
    let mut view = view_with_positions();
    let before: Vec<Pos2> = ["a1", "loose", "ca"]
        .iter()
        .map(|n| view.node_position(&id(n)).unwrap())
        .collect();

    view.set_hidden(&ids(&["a1", "loose", "ca"]));
    // Hidden nodes still report where they will reappear.
    assert_eq!(view.node_position(&id("a1")), Some(before[0]));
    view.set_hidden(&BTreeSet::new());

    let after: Vec<Pos2> = ["a1", "loose", "ca"]
        .iter()
        .map(|n| view.node_position(&id(n)).unwrap())
        .collect();
    assert_eq!(before, after);
    assert_eq!(view.node_count(), 6);
    assert_eq!(view.edge_count(), 3);
}

#[test]
fn hidden_nodes_stay_hidden_across_syncs() {
    let mut view = view_with_positions();
    let pos = view.node_position(&id("b1")).unwrap();
    view.set_hidden(&ids(&["b1"]));
    view.sync(&snapshot());
    assert!(!view.is_visible(&id("b1")));
    assert_eq!(view.node_count(), 5);

    view.set_hidden(&BTreeSet::new());
    assert_eq!(view.node_position(&id("b1")), Some(pos));
}

#[test]
fn hidden_node_that_vanishes_is_forgotten() {
    let mut view = view_with_positions();
    view.set_hidden(&ids(&["b1"]));
    let mut s = snapshot();
    s.nodes.retain(|n| n.id != id("b1"));
    s.edges.retain(|e| e.to != id("b1"));
    view.sync(&s);
    assert_eq!(view.node_position(&id("b1")), None);
}

#[test]
fn filtering_does_not_wake_layout() {
    let mut view = view_with_positions();
    view.wake_layout = false;
    view.set_hidden(&ids(&["a1"]));
    assert!(!view.wake_layout);
    view.set_hidden(&BTreeSet::new());
    assert!(!view.wake_layout);
}

#[test]
fn run_focus_bounds_cover_visible_run_nodes() {
    let mut view = view_with_positions();
    view.set_hidden(&ids(&["a2"]));
    view.focus_run("a");
    let requested = view.focus_requested.clone().unwrap();
    assert_eq!(requested, vec![id("a1"), id("a2"), id("ca")]);

    let bounds = view.nodes_bounds(&requested).unwrap();
    let ca = view.node_position(&id("ca")).unwrap();
    let a1 = view.node_position(&id("a1")).unwrap();
    assert!(bounds.contains(ca) && bounds.contains(a1));
    let r = node_radius(NodeKind::Coordinator);
    assert_eq!(bounds.min, Pos2::new(ca.x - r, ca.y - r));
    // The hidden worker at x = 200 is not included.
    assert!(bounds.max.x < 150.0);
}

#[test]
fn focus_transform_centers_and_fits() {
    let bounds = Rect::from_min_max(Pos2::new(100.0, 100.0), Pos2::new(300.0, 200.0));
    let view = Vec2::new(800.0, 400.0);
    let (zoom, pan) = focus_transform(bounds, view, 0.0, 10.0);
    // Height is the tighter dimension: 400 / 100.
    assert!((zoom - 4.0).abs() < 1e-5);
    let center = (bounds.center().to_vec2() * zoom + pan).to_pos2();
    assert!((center - Pos2::new(400.0, 200.0)).length() < 1e-3);
}

#[test]
fn focus_transform_caps_zoom_for_single_node() {
    let bounds = Rect::from_center_size(Pos2::new(50.0, 50.0), Vec2::splat(20.0));
    let (zoom, pan) = focus_transform(bounds, Vec2::new(800.0, 600.0), 0.3, 2.0);
    assert_eq!(zoom, 2.0);
    assert_eq!(pan, Vec2::new(400.0 - 100.0, 300.0 - 100.0));
}

#[test]
fn focus_applies_on_next_frame_headless() {
    let ctx = egui::Context::default();
    let mut view = view_with_positions();
    view.focus_run("b");
    let mut size = Vec2::ZERO;
    ctx.run_ui(egui::RawInput::default(), |ui| {
        size = ui.available_size();
        view.show(ui);
    })
    .drop_without_applying_deltas();
    assert!(view.focus_requested.is_none());
    assert!(!view.auto_fit);

    let bounds = view.nodes_bounds(&[id("cb"), id("b1")]).unwrap();
    let (zoom, pan) = focus_transform(bounds, size, FOCUS_PADDING, FOCUS_MAX_ZOOM);
    let mut meta = None;
    ctx.run_ui(egui::RawInput::default(), |ui| {
        meta = Some(MetadataFrame::new(Some(DEFAULT_GRAPH_ID.into())).load(ui));
    })
    .drop_without_applying_deltas();
    let meta = meta.unwrap();
    assert!((meta.zoom - zoom).abs() < 1e-4, "{} vs {zoom}", meta.zoom);
    assert!((meta.pan - pan).length() < 1e-2);
}

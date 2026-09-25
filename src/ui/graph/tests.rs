use eframe::egui::{self, Pos2, Rect};
use egui_graphs::{ForceAlgorithm, FruchtermanReingoldWithExtras};

use super::*;
use crate::model::{Edge, EdgeKind, NodeState, RunSummary};

const RUN: &str = "run_a";

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

/// A run with a coordinator and `workers`, plus one loose terminal.
fn snapshot(workers: &[&str]) -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    s.runs = vec![RunSummary {
        id: RUN.into(),
        objective: Some("Ship it".into()),
        coordinator: Some(id("coord")),
        task_counts: None,
    }];
    s.nodes
        .push(node("coord", NodeKind::Coordinator, Some(RUN)));
    s.nodes.push(node("loose", NodeKind::Terminal, None));
    for w in workers {
        s.nodes.push(node(w, NodeKind::Worker, Some(RUN)));
        s.edges.push(Edge {
            from: id("coord"),
            to: id(w),
            kind: EdgeKind::CoordinatorWorker,
        });
    }
    s
}

fn spread(view: &mut GraphView) {
    for (i, n) in ["coord", "loose", "w1", "w2", "w3"].iter().enumerate() {
        view.set_node_position(&id(n), Pos2::new(i as f32 * 100.0, 50.0));
    }
}

#[test]
fn sync_adds_nodes_and_edges() {
    let mut view = GraphView::new();
    view.sync(&snapshot(&["w1", "w2"]));

    assert_eq!(view.node_count(), 4);
    assert_eq!(view.edge_count(), 2);
    assert!(view.contains_edge(&id("coord"), &id("w1")));
    assert!(view.contains_edge(&id("coord"), &id("w2")));
    assert!(!view.contains_edge(&id("coord"), &id("loose")));

    let coord = view.node_position(&id("coord")).unwrap();
    for w in ["w1", "w2"] {
        let d = (view.node_position(&id(w)).unwrap() - coord).length();
        assert!(
            (d - SPAWN_NEAR).abs() < 0.01,
            "{w} spawned {d} from coordinator"
        );
    }
    assert_ne!(view.node_position(&id("w1")), view.node_position(&id("w2")));
}

#[test]
fn sync_updates_in_place() {
    let mut view = GraphView::new();
    view.sync(&snapshot(&["w1", "w2"]));
    spread(&mut view);
    let before: Vec<_> = ["coord", "loose", "w1", "w2"]
        .iter()
        .map(|n| (view.node_index(&id(n)), view.node_position(&id(n))))
        .collect();

    let mut next = snapshot(&["w1", "w2"]);
    next.nodes[2].state = NodeState::NeedsInput;
    next.nodes[2].label = "renamed".into();
    view.sync(&next);

    let after: Vec<_> = ["coord", "loose", "w1", "w2"]
        .iter()
        .map(|n| (view.node_index(&id(n)), view.node_position(&id(n))))
        .collect();
    assert_eq!(before, after);
    let w1 = view
        .graph()
        .node(view.node_index(&id("w1")).unwrap())
        .unwrap();
    assert_eq!(w1.payload().state, NodeState::NeedsInput);
    assert_eq!(w1.label(), "renamed");
    assert_eq!(view.edge_count(), 2);
}

#[test]
fn sync_add_keeps_existing_positions() {
    let mut view = GraphView::new();
    view.sync(&snapshot(&["w1"]));
    spread(&mut view);
    let coord = (
        view.node_index(&id("coord")),
        view.node_position(&id("coord")),
    );
    let w1 = (view.node_index(&id("w1")), view.node_position(&id("w1")));

    view.sync(&snapshot(&["w1", "w2"]));

    assert_eq!(
        coord,
        (
            view.node_index(&id("coord")),
            view.node_position(&id("coord"))
        )
    );
    assert_eq!(
        w1,
        (view.node_index(&id("w1")), view.node_position(&id("w1")))
    );
    let d = (view.node_position(&id("w2")).unwrap() - coord.1.unwrap()).length();
    assert!((d - SPAWN_NEAR).abs() < 0.01);
    assert!(view.contains_edge(&id("coord"), &id("w2")));
    assert_eq!(view.edge_count(), 2);
}

#[test]
fn sync_remove_keeps_other_indices_and_positions() {
    let mut view = GraphView::new();
    view.sync(&snapshot(&["w1", "w2", "w3"]));
    spread(&mut view);
    let kept = ["coord", "loose", "w1", "w3"];
    let before: Vec<_> = kept
        .iter()
        .map(|n| (view.node_index(&id(n)), view.node_position(&id(n))))
        .collect();

    view.sync(&snapshot(&["w1", "w3"]));

    let after: Vec<_> = kept
        .iter()
        .map(|n| (view.node_index(&id(n)), view.node_position(&id(n))))
        .collect();
    assert_eq!(before, after);
    assert_eq!(view.node_index(&id("w2")), None);
    assert_eq!(view.node_count(), 4);
    assert_eq!(view.edge_count(), 2);
    assert!(!view.contains_edge(&id("coord"), &id("w2")));
}

#[test]
fn edges_follow_snapshot() {
    let mut view = GraphView::new();
    view.sync(&snapshot(&["w1", "w2"]));

    let mut next = snapshot(&["w1", "w2"]);
    next.edges.retain(|e| e.to != id("w2"));
    view.sync(&next);
    assert!(!view.contains_edge(&id("coord"), &id("w2")));
    assert_eq!(view.edge_count(), 1);

    view.sync(&snapshot(&["w1", "w2"]));
    assert!(view.contains_edge(&id("coord"), &id("w2")));
    assert_eq!(view.edge_count(), 2);

    // Removing the coordinator drops its edges; re-adding restores them.
    let mut headless = snapshot(&["w1", "w2"]);
    headless.nodes.retain(|n| n.id != id("coord"));
    view.sync(&headless);
    assert_eq!(view.edge_count(), 0);
    view.sync(&snapshot(&["w1", "w2"]));
    assert_eq!(view.edge_count(), 2);
}

#[test]
fn coordinator_added_with_workers_spawns_first() {
    let mut view = GraphView::new();
    let mut s = snapshot(&["w1"]);
    // Worker listed before its coordinator.
    s.nodes.reverse();
    view.sync(&s);
    let d = (view.node_position(&id("w1")).unwrap() - view.node_position(&id("coord")).unwrap())
        .length();
    assert!((d - SPAWN_NEAR).abs() < 0.01);
}

#[test]
fn worker_without_coordinator_spawns_near_run() {
    let mut view = GraphView::new();
    let mut s = snapshot(&["w1"]);
    s.runs[0].coordinator = None;
    s.nodes.retain(|n| n.id != id("coord"));
    view.sync(&s);
    view.set_node_position(&id("w1"), Pos2::new(1000.0, 1000.0));

    let mut s2 = s.clone();
    s2.nodes.push(node("w2", NodeKind::Worker, Some(RUN)));
    view.sync(&s2);
    let d = (view.node_position(&id("w2")).unwrap() - Pos2::new(1000.0, 1000.0)).length();
    assert!((d - SPAWN_NEAR).abs() < 0.01);
}

#[test]
fn selection_round_trips() {
    let mut view = GraphView::new();
    view.sync(&snapshot(&["w1", "w2"]));
    assert_eq!(view.selected(), None);
    view.set_selected(Some(&id("w2")));
    assert_eq!(view.selected(), Some(id("w2")));
    view.set_selected(Some(&id("w1")));
    assert_eq!(view.selected(), Some(id("w1")));
    view.set_selected(None);
    assert_eq!(view.selected(), None);
}

#[test]
fn released_nodes_are_faded() {
    let mut view = GraphView::new();
    let mut s = snapshot(&["w1"]);
    s.nodes[2].released_at_ms = Some(1);
    view.sync(&s);
    let w1 = view
        .graph()
        .node(view.node_index(&id("w1")).unwrap())
        .unwrap();
    assert!(w1.payload().faded);
}

/// The layout settles (and so pauses) for a realistically sized fleet without collapsing.
#[test]
fn layout_settles_for_large_fleet() {
    let mut s = FleetSnapshot::default();
    for r in 0..10 {
        let run = format!("run{r}");
        let coord = format!("c{r}");
        s.runs.push(RunSummary {
            id: run.clone(),
            objective: None,
            coordinator: Some(id(&coord)),
            task_counts: None,
        });
        s.nodes
            .push(node(&coord, NodeKind::Coordinator, Some(&run)));
        for w in 0..11 {
            let worker = format!("w{r}_{w}");
            s.nodes.push(node(&worker, NodeKind::Worker, Some(&run)));
            s.edges.push(Edge {
                from: id(&coord),
                to: id(&worker),
                kind: EdgeKind::CoordinatorWorker,
            });
        }
    }
    let mut view = GraphView::new();
    view.sync(&s);
    assert_eq!(view.node_count(), 120);

    let viewport = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0));
    let mut algorithm = FruchtermanReingoldWithExtras::<Extras>::from_state(view.layout_state());
    let mut settle = SettleTracker::default();
    let mut settled_at = None;
    for step in 0..5_000 {
        algorithm.step(&mut view.graph, viewport);
        if settle.observe(algorithm.state().base.last_avg_displacement) {
            settled_at = Some(step);
            break;
        }
    }
    assert!(
        settled_at.is_some_and(|s| s < 1_000),
        "layout settled at {settled_at:?}"
    );

    let positions: Vec<Pos2> = view.graph.nodes_iter().map(|(_, n)| n.location()).collect();
    let mut min_distance = f32::MAX;
    for (i, a) in positions.iter().enumerate() {
        for b in &positions[i + 1..] {
            min_distance = min_distance.min((*a - *b).length());
        }
    }
    assert!(min_distance > 5.0, "nodes overlap: {min_distance}");
}

#[test]
fn settle_tracker_needs_consecutive_calm_frames() {
    let mut t = SettleTracker::default();
    for _ in 0..layout::SETTLE_FRAMES - 1 {
        assert!(!t.observe(Some(0.0)));
    }
    assert!(!t.observe(Some(1.0)));
    for _ in 0..layout::SETTLE_FRAMES - 1 {
        assert!(!t.observe(Some(0.0)));
    }
    assert!(t.observe(Some(0.0)));
}

#[test]
fn formats_ago() {
    let now = 10_000_000;
    assert_eq!(format_ago(now, now + 5_000), "just now");
    assert_eq!(format_ago(now, now - 2_000), "just now");
    assert_eq!(format_ago(now, now - 42_000), "42s ago");
    assert_eq!(format_ago(now, now - 125_000), "2m ago");
    assert_eq!(format_ago(now, now - 3 * 3_600_000), "3h ago");
    assert_eq!(format_ago(now * 100, now * 100 - 2 * 86_400_000), "2d ago");
}

#[test]
fn preview_first_line() {
    assert_eq!(
        preview_line("\n  \n  hello world  \nsecond"),
        Some("hello world".into())
    );
    assert_eq!(preview_line("  \n"), None);
    let long = "x".repeat(300);
    let line = preview_line(&long).unwrap();
    assert_eq!(line.chars().count(), 100);
    assert!(line.ends_with('…'));
}

#[test]
fn state_colors_are_distinct() {
    let states = [
        NodeState::Working,
        NodeState::Idle,
        NodeState::NeedsInput,
        NodeState::Stalled,
        NodeState::Done,
        NodeState::Failed,
    ];
    for dark in [true, false] {
        let colors: HashSet<_> = states.iter().map(|s| state_color(*s, dark)).collect();
        assert_eq!(colors.len(), states.len());
    }
}

/// `show` runs headless: the layout moves nodes, and a sync between frames keeps positions.
#[test]
fn show_runs_layout_headless() {
    let ctx = egui::Context::default();
    let mut view = GraphView::new();
    view.sync(&snapshot(&["w1", "w2", "w3"]));
    let start = view.node_position(&id("w1")).unwrap();

    let frame = |view: &mut GraphView| {
        let mut response = GraphResponse::default();
        ctx.run_ui(egui::RawInput::default(), |ui| response = view.show(ui))
            .drop_without_applying_deltas();
        response
    };
    for _ in 0..10 {
        let response = frame(&mut view);
        assert_eq!(response.clicked, None);
    }
    assert_ne!(view.node_position(&id("w1")).unwrap(), start);

    let before = view.node_position(&id("coord"));
    view.sync(&snapshot(&["w1", "w2", "w3", "w4"]));
    assert_eq!(view.node_position(&id("coord")), before);
    frame(&mut view);
    assert_eq!(view.node_count(), 6);
}

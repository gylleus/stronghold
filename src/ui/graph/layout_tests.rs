use std::collections::HashMap;

use eframe::egui::{self, Pos2, Rect};
use egui_graphs::{AnimatedState, ForceAlgorithm, FruchtermanReingoldWithExtras};

use super::*;
use crate::model::{Edge, EdgeKind, FleetSnapshot, Node, NodeId, NodeState, RunSummary};
use crate::ui::graph::GraphView;

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

/// `runs` runs of a coordinator `c{r}` with `workers` workers `w{r}_{w}`, plus `loose`
/// terminals `t{i}` and the queen.
fn fleet(runs: usize, workers: usize, loose: usize) -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    for r in 0..runs {
        let run = format!("run{r:02}");
        let coord = format!("c{r}");
        s.runs.push(RunSummary {
            id: run.clone(),
            objective: None,
            coordinator: Some(id(&coord)),
            task_counts: None,
        });
        s.nodes
            .push(node(&coord, NodeKind::Coordinator, Some(&run)));
        for w in 0..workers {
            let worker = format!("w{r}_{w}");
            s.nodes.push(node(&worker, NodeKind::Worker, Some(&run)));
            s.edges.push(Edge {
                from: id(&coord),
                to: id(&worker),
                kind: EdgeKind::CoordinatorWorker,
            });
        }
    }
    for t in 0..loose {
        s.nodes
            .push(node(&format!("t{t}"), NodeKind::Terminal, None));
    }
    s.add_queen();
    s
}

/// Steps the layout until it settles; returns the step count.
fn settle(view: &mut GraphView) -> usize {
    let viewport = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0));
    let mut algorithm = FruchtermanReingoldWithExtras::<Extras>::from_state(view.layout_state());
    let mut tracker = SettleTracker::default();
    for step in 0..5_000 {
        algorithm.step(&mut view.graph, viewport);
        if tracker.observe(algorithm.state().last_avg_displacement()) {
            return step;
        }
    }
    panic!("layout did not settle");
}

fn positions(view: &GraphView, snapshot: &FleetSnapshot) -> HashMap<NodeId, Pos2> {
    snapshot
        .nodes
        .iter()
        .map(|n| (n.id.clone(), view.node_position(&n.id).unwrap()))
        .collect()
}

struct Cluster {
    coordinator: Pos2,
    workers_center: Pos2,
    center: Pos2,
    /// Distance from `center` to the farthest node.
    radius: f32,
}

fn clusters(snapshot: &FleetSnapshot, pos: &HashMap<NodeId, Pos2>) -> Vec<Cluster> {
    snapshot
        .runs
        .iter()
        .map(|run| {
            let coord = run.coordinator.clone().unwrap();
            let all: Vec<Pos2> = snapshot.nodes_in_run(&run.id).map(|n| pos[&n.id]).collect();
            let workers: Vec<Pos2> = snapshot
                .nodes_in_run(&run.id)
                .filter(|n| n.id != coord)
                .map(|n| pos[&n.id])
                .collect();
            let center = centroid(&all).unwrap();
            Cluster {
                coordinator: pos[&coord],
                workers_center: centroid(&workers).unwrap(),
                center,
                radius: all
                    .iter()
                    .map(|p| (*p - center).length())
                    .fold(0.0, f32::max),
            }
        })
        .collect()
}

fn settled(runs: usize, workers: usize, loose: usize) -> (GraphView, FleetSnapshot) {
    let snapshot = fleet(runs, workers, loose);
    let mut view = GraphView::new();
    view.sync(&snapshot);
    let steps = settle(&mut view);
    assert!(steps < 1_500, "settled after {steps} steps");
    (view, snapshot)
}

#[test]
fn coordinators_sit_at_their_cluster_center() {
    let (view, snapshot) = settled(12, 8, 4);
    let cs = clusters(&snapshot, &positions(&view, &snapshot));
    for (i, own) in cs.iter().enumerate() {
        let to_own = (own.coordinator - own.workers_center).length();
        for (j, other) in cs.iter().enumerate().filter(|(j, _)| *j != i) {
            let to_other = (own.coordinator - other.workers_center).length();
            assert!(
                to_own < to_other,
                "coordinator {i} is {to_own} from its workers, {to_other} from run {j}"
            );
        }
        assert!(
            to_own < own.radius / 2.0,
            "coordinator {i} is off-center by {to_own}"
        );
    }
}

#[test]
fn clusters_do_not_overlap() {
    let (view, snapshot) = settled(12, 8, 4);
    let cs = clusters(&snapshot, &positions(&view, &snapshot));
    for (i, a) in cs.iter().enumerate() {
        for (j, b) in cs.iter().enumerate().skip(i + 1) {
            let distance = (a.center - b.center).length();
            assert!(
                distance > a.radius + b.radius,
                "runs {i} and {j}: {distance} apart, radii {} + {}",
                a.radius,
                b.radius
            );
        }
    }
}

#[test]
fn loose_terminals_stay_out_of_clusters_and_together() {
    let (view, snapshot) = settled(12, 8, 5);
    let pos = positions(&view, &snapshot);
    let cs = clusters(&snapshot, &pos);
    let loose: Vec<Pos2> = (0..5).map(|t| pos[&id(&format!("t{t}"))]).collect();
    for (t, p) in loose.iter().enumerate() {
        for (i, c) in cs.iter().enumerate() {
            let distance = (*p - c.center).length();
            assert!(
                distance > c.radius,
                "t{t} is inside run {i} ({distance} < {})",
                c.radius
            );
        }
    }
    let center = centroid(&loose).unwrap();
    let spread = loose
        .iter()
        .map(|p| (*p - center).length())
        .fold(0.0, f32::max);
    let runs_left = cs
        .iter()
        .map(|c| c.center.x - c.radius)
        .fold(f32::MAX, f32::min);
    assert!(spread < 150.0, "loose terminals spread over {spread}");
    assert!(
        center.x < runs_left,
        "loose group at {center:?}, runs start at {runs_left}"
    );
}

#[test]
fn settled_layout_barely_moves_when_a_node_is_added_elsewhere() {
    let (mut view, snapshot) = settled(12, 8, 4);
    let before = positions(&view, &snapshot);

    let mut next = snapshot.clone();
    next.nodes.push(node("t_new", NodeKind::Terminal, None));
    view.sync(&next);
    settle(&mut view);

    let after = positions(&view, &snapshot);
    let moved = before
        .iter()
        .filter(|(id, _)| id.0.starts_with(['c', 'w']))
        .map(|(id, p)| (after[id] - *p).length())
        .fold(0.0, f32::max);
    assert!(moved < 20.0, "a run node moved {moved}");
}

#[test]
fn memberships_follow_runs_and_parents() {
    let nodes = [
        ClusterNode {
            index: 0,
            run: Some("b"),
            kind: NodeKind::Coordinator,
            parent: None,
        },
        ClusterNode {
            index: 1,
            run: Some("b"),
            kind: NodeKind::Worker,
            parent: None,
        },
        ClusterNode {
            index: 3,
            run: Some("a"),
            kind: NodeKind::Worker,
            parent: None,
        },
        ClusterNode {
            index: 4,
            run: None,
            kind: NodeKind::Terminal,
            parent: None,
        },
        ClusterNode {
            index: 5,
            run: None,
            kind: NodeKind::Subagent,
            parent: Some(1),
        },
        ClusterNode {
            index: 6,
            run: None,
            kind: NodeKind::Subagent,
            parent: Some(4),
        },
        ClusterNode {
            index: 7,
            run: None,
            kind: NodeKind::Subagent,
            parent: Some(5),
        },
    ];
    assert_eq!(
        memberships(&nodes),
        vec![
            Some(Membership::Coordinator(1)),
            Some(Membership::Member(1)),
            None,
            Some(Membership::Member(0)),
            Some(Membership::Loose),
            Some(Membership::Member(1)),
            Some(Membership::Loose),
            Some(Membership::Member(1)),
        ]
    );
}

#[test]
fn queen_sits_at_the_center_outside_every_cluster() {
    let (view, snapshot) = settled(12, 8, 4);
    let pos = positions(&view, &snapshot);
    let cs = clusters(&snapshot, &pos);
    let queen = pos[&NodeId::queen()];
    let centers: Vec<Pos2> = cs.iter().map(|c| c.center).collect();
    let middle = centroid(&centers).unwrap();
    let spread = centers
        .iter()
        .map(|p| (*p - middle).length())
        .fold(0.0, f32::max);
    let off = (queen - middle).length();
    assert!(
        off < spread / 3.0,
        "queen {off} from the clusters' centroid (spread {spread})"
    );
    for (i, c) in cs.iter().enumerate() {
        let distance = (queen - c.center).length();
        assert!(
            distance > c.radius,
            "queen inside run {i} ({distance} < {})",
            c.radius
        );
    }
}

#[test]
fn queen_edges_do_not_knot_the_coordinators() {
    let (view, snapshot) = settled(12, 8, 4);
    let pos = positions(&view, &snapshot);
    let queen = pos[&NodeId::queen()];
    let near = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Coordinator)
        .filter(|n| (pos[&n.id] - queen).length() < 60.0)
        .count();
    assert_eq!(near, 0, "{near} coordinators crowd the queen");
}

#[test]
fn queen_gets_its_own_membership() {
    let nodes = [
        ClusterNode {
            index: 0,
            run: None,
            kind: NodeKind::Queen,
            parent: None,
        },
        ClusterNode {
            index: 1,
            run: None,
            kind: NodeKind::Terminal,
            parent: None,
        },
    ];
    assert_eq!(
        memberships(&nodes),
        vec![Some(Membership::Queen), Some(Membership::Loose)]
    );
}

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

/// Number of repos [`fleet`] spreads its runs and loose terminals over.
const REPOS: usize = 3;

fn repo_name(i: usize) -> String {
    format!("repo{}", i % REPOS)
}

fn in_repo(mut node: Node, repo: &str) -> Node {
    node.worktree_path = Some(format!("/Users/x/orca/workspaces/{repo}/{}", node.id.0));
    node
}

/// `runs` runs of a coordinator `c{r}` with `workers` workers `w{r}_{w}`, plus `loose`
/// terminals `t{i}` and the queen. Run `r` and terminal `t{i}` are in repo `repo{r % 3}`
/// and `repo{i % 3}`.
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
        let repo = repo_name(r);
        s.nodes.push(in_repo(
            node(&coord, NodeKind::Coordinator, Some(&run)),
            &repo,
        ));
        for w in 0..workers {
            let worker = format!("w{r}_{w}");
            s.nodes
                .push(in_repo(node(&worker, NodeKind::Worker, Some(&run)), &repo));
            s.edges.push(Edge {
                from: id(&coord),
                to: id(&worker),
                kind: EdgeKind::CoordinatorWorker,
            });
        }
    }
    for t in 0..loose {
        s.nodes.push(in_repo(
            node(&format!("t{t}"), NodeKind::Terminal, None),
            &repo_name(t),
        ));
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
fn loose_terminals_stay_in_their_repo_out_of_clusters_and_together() {
    let (view, snapshot) = settled(12, 8, 6);
    let pos = positions(&view, &snapshot);
    let cs = clusters(&snapshot, &pos);
    let regions = view.repo_regions();
    for repo in 0..REPOS {
        let loose: Vec<Pos2> = (0..6)
            .filter(|t| t % REPOS == repo)
            .map(|t| pos[&id(&format!("t{t}"))])
            .collect();
        let region = regions.iter().find(|r| r.repo == repo_name(repo)).unwrap();
        for p in &loose {
            assert!(region.rect.contains(*p), "{p:?} outside {}", region.repo);
            for (i, c) in cs.iter().enumerate() {
                let distance = (*p - c.center).length();
                assert!(
                    distance > c.radius,
                    "{p:?} is inside run {i} ({distance} < {})",
                    c.radius
                );
            }
        }
        let center = centroid(&loose).unwrap();
        let spread = loose
            .iter()
            .map(|p| (*p - center).length())
            .fold(0.0, f32::max);
        assert!(
            spread < 150.0,
            "repo{repo} loose terminals spread over {spread}"
        );
    }
}

#[test]
fn repo_regions_hold_their_nodes_apart_from_each_other_and_the_queen() {
    let (view, snapshot) = settled(12, 8, 6);
    let pos = positions(&view, &snapshot);
    let regions = view.repo_regions();
    assert_eq!(regions.len(), REPOS);
    for (i, a) in regions.iter().enumerate() {
        for b in &regions[i + 1..] {
            assert!(
                !a.rect.intersects(b.rect),
                "{} {:?} overlaps {} {:?}",
                a.repo,
                a.rect,
                b.repo,
                b.rect
            );
        }
        assert!(
            !a.rect.contains(pos[&NodeId::queen()]),
            "queen inside {}",
            a.repo
        );
    }
    for node in snapshot.agent_nodes() {
        let repo = view.node_repo(&node.id).unwrap();
        for region in &regions {
            assert_eq!(
                region.rect.contains(pos[&node.id]),
                region.repo == repo,
                "{} (repo {repo}) vs region {}",
                node.id,
                region.repo
            );
        }
    }
}

#[test]
fn repo_indices_follow_parents() {
    let nodes = [
        ClusterNode {
            index: 0,
            run: None,
            kind: NodeKind::Queen,
            parent: None,
            repo: Some("zz"),
        },
        ClusterNode {
            index: 1,
            run: Some("r"),
            kind: NodeKind::Coordinator,
            parent: None,
            repo: Some("shop"),
        },
        ClusterNode {
            index: 2,
            run: None,
            kind: NodeKind::Terminal,
            parent: None,
            repo: Some("infra"),
        },
        ClusterNode {
            index: 3,
            run: None,
            kind: NodeKind::Subagent,
            parent: Some(2),
            repo: None,
        },
        ClusterNode {
            index: 4,
            run: None,
            kind: NodeKind::Terminal,
            parent: None,
            repo: None,
        },
    ];
    assert_eq!(
        repo_indices(&nodes),
        vec![None, Some(1), Some(0), Some(0), None]
    );
}

#[test]
fn settled_layout_barely_moves_outside_the_repo_a_node_joins() {
    let (mut view, snapshot) = settled(12, 8, 4);
    let before = positions(&view, &snapshot);

    let mut next = snapshot.clone();
    next.nodes
        .push(in_repo(node("t_new", NodeKind::Terminal, None), "repo0"));
    view.sync(&next);
    settle(&mut view);

    let after = positions(&view, &snapshot);
    let moved = before
        .iter()
        .filter(|(id, _)| id.0.starts_with(['c', 'w']))
        .filter(|(id, _)| view.node_repo(id) != Some("repo0"))
        .map(|(id, p)| (after[id] - *p).length())
        .fold(0.0, f32::max);
    assert!(moved < 20.0, "a run node of another repo moved {moved}");
}

#[test]
fn memberships_follow_runs_and_parents() {
    let nodes = [
        ClusterNode {
            index: 0,
            run: Some("b"),
            kind: NodeKind::Coordinator,
            parent: None,
            repo: None,
        },
        ClusterNode {
            index: 1,
            run: Some("b"),
            kind: NodeKind::Worker,
            parent: None,
            repo: None,
        },
        ClusterNode {
            index: 3,
            run: Some("a"),
            kind: NodeKind::Worker,
            parent: None,
            repo: None,
        },
        ClusterNode {
            index: 4,
            run: None,
            kind: NodeKind::Terminal,
            parent: None,
            repo: None,
        },
        ClusterNode {
            index: 5,
            run: None,
            kind: NodeKind::Subagent,
            parent: Some(1),
            repo: None,
        },
        ClusterNode {
            index: 6,
            run: None,
            kind: NodeKind::Subagent,
            parent: Some(4),
            repo: None,
        },
        ClusterNode {
            index: 7,
            run: None,
            kind: NodeKind::Subagent,
            parent: Some(5),
            repo: None,
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
            repo: None,
        },
        ClusterNode {
            index: 1,
            run: None,
            kind: NodeKind::Terminal,
            parent: None,
            repo: None,
        },
    ];
    assert_eq!(
        memberships(&nodes),
        vec![Some(Membership::Queen), Some(Membership::Loose)]
    );
}

#[test]
fn reset_layout_regroups_scrambled_repos() {
    let (mut view, snapshot) = settled(9, 4, 6);
    // Pile everything onto one spot, as a stale saved layout might.
    for (i, node) in snapshot.nodes.iter().enumerate() {
        view.set_node_position(&node.id, Pos2::new(400.0 + i as f32, 400.0));
    }
    view.reset_layout();
    settle(&mut view);
    let regions = view.repo_regions();
    assert_eq!(regions.len(), REPOS);
    for (i, a) in regions.iter().enumerate() {
        for b in &regions[i + 1..] {
            assert!(!a.rect.intersects(b.rect), "{} overlaps {}", a.repo, b.repo);
        }
    }
}

/// `repos` repos of `per_repo` loose terminals `r{r}_t{t}` each, plus the queen.
fn loose_fleet(repos: usize, per_repo: usize) -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    for r in 0..repos {
        for t in 0..per_repo {
            s.nodes.push(in_repo(
                node(&format!("r{r}_t{t}"), NodeKind::Terminal, None),
                &format!("repo{r}"),
            ));
        }
    }
    s.add_queen();
    s
}

/// Steps the layout in a view of `size` until it settles.
fn settle_in(view: &mut GraphView, size: egui::Vec2) {
    let viewport = Rect::from_min_size(Pos2::ZERO, size);
    let mut algorithm = FruchtermanReingoldWithExtras::<Extras>::from_state(view.layout_state());
    let mut tracker = SettleTracker::default();
    for _ in 0..5_000 {
        algorithm.step(&mut view.graph, viewport);
        if tracker.observe(algorithm.state().last_avg_displacement()) {
            return;
        }
    }
    panic!("layout did not settle");
}

#[test]
fn repo_regions_stay_compact_at_any_group_size() {
    // Nodes pack at about NODE_SPACING, so a group of n spans about √n spacings each way.
    for per_repo in [1, 2, 4, 8, 12] {
        let snapshot = loose_fleet(6, per_repo);
        let mut view = GraphView::new();
        view.sync(&snapshot);
        settle(&mut view);
        let side = (per_repo as f32).sqrt().ceil() + 1.0;
        let max = NODE_SPACING * side + region_rect(&[Pos2::ZERO]).size();
        for region in view.repo_regions() {
            let size = region.rect.size();
            assert!(
                size.x <= max.x && size.y <= max.y,
                "{} of {per_repo} nodes spans {size:?} (max {max:?})",
                region.repo
            );
        }
    }
}

#[test]
fn repo_regions_do_not_grow_with_the_window() {
    let size_in = |window: egui::Vec2| {
        let snapshot = loose_fleet(4, 4);
        let mut view = GraphView::new();
        view.sync(&snapshot);
        settle_in(&mut view, window);
        view.repo_regions()
            .iter()
            .map(|r| r.rect.area())
            .fold(0.0, f32::max)
    };
    let small = size_in(egui::vec2(1280.0, 800.0));
    let large = size_in(egui::vec2(2560.0, 1600.0));
    assert!(
        large < small * 1.3,
        "largest region {small} in a small window, {large} in a large one"
    );
}

#[test]
fn nodes_of_a_group_keep_apart() {
    let (view, snapshot) = settled(6, 8, 6);
    let pos = positions(&view, &snapshot);
    let nodes: Vec<&Node> = snapshot.agent_nodes().collect();
    for (i, a) in nodes.iter().enumerate() {
        for b in &nodes[i + 1..] {
            if a.run_id != b.run_id || view.node_repo(&a.id) != view.node_repo(&b.id) {
                continue;
            }
            let d = pos[&a.id] - pos[&b.id];
            let scaled = egui::vec2(d.x / NODE_SPACING.x, d.y / NODE_SPACING.y).length();
            assert!(scaled > 0.6, "{} and {} are {d:?} apart", a.id, b.id);
        }
    }
}

#[test]
fn a_dropped_node_stays_where_it_was_dropped() {
    let snapshot = loose_fleet(3, 4);
    let mut view = GraphView::new();
    view.sync(&snapshot);
    settle(&mut view);
    // Drop r0_t0 right next to r0_t1, then well away from its group.
    for offset in [egui::vec2(70.0, 10.0), egui::vec2(-150.0, 120.0)] {
        let dropped = view.node_position(&id("r0_t1")).unwrap() + offset;
        view.set_node_position(&id("r0_t0"), dropped);
        view.pin(id("r0_t0"));
        settle(&mut view);
        assert_eq!(view.node_position(&id("r0_t0")), Some(dropped));
        let region = view
            .repo_regions()
            .into_iter()
            .find(|r| r.repo == "repo0")
            .unwrap();
        assert!(region.rect.contains(dropped));
    }
}

#[test]
fn a_new_node_leaves_pinned_nodes_alone_and_reset_unpins() {
    let snapshot = loose_fleet(3, 3);
    let mut view = GraphView::new();
    view.sync(&snapshot);
    settle(&mut view);
    view.pin(id("r1_t0"));
    let pinned = view.node_position(&id("r1_t0")).unwrap();
    let mut next = snapshot.clone();
    next.nodes
        .push(in_repo(node("r1_new", NodeKind::Terminal, None), "repo1"));
    view.sync(&next);
    settle(&mut view);
    assert_eq!(view.node_position(&id("r1_t0")), Some(pinned));

    view.reset_layout();
    assert!(!view.is_pinned(&id("r1_t0")));
    view.pin(id("r1_t0"));
    view.sync(&snapshot);
    view.sync(&loose_fleet(3, 0));
    assert!(
        !view.is_pinned(&id("r1_t0")),
        "a vanished node stays pinned"
    );
}

#[test]
fn collision_is_zero_beyond_the_spacing_and_one_on_top() {
    assert_eq!(super::collision(Vec2::ZERO), 1.0);
    assert_eq!(super::collision(egui::vec2(NODE_SPACING.x, 0.0)), 0.0);
    assert_eq!(super::collision(egui::vec2(0.0, NODE_SPACING.y * 2.0)), 0.0);
    assert!((super::collision(NODE_SPACING * 0.5 / 2f32.sqrt()) - 0.5).abs() < 1e-5);
}

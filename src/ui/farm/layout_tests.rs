use std::collections::{BTreeSet, HashSet};

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

use super::FarmView;
use super::layout::*;
use super::motion::{along, route};
use crate::model::{
    ClanSnapshot, Edge, EdgeKind, Node, NodeId, NodeKind, NodeState, RunSummary, TaskCounts,
};
use crate::orca::{DispatchStatus, WorkerState, WorkerTerminalState};

const RECT: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1280.0, 800.0));

pub(super) fn id(s: &str) -> NodeId {
    NodeId(s.to_string())
}

pub(super) fn node(name: &str, kind: NodeKind, run: Option<&str>, state: NodeState) -> Node {
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
        worktree_path: Some(format!("/x/orca/workspaces/repo-{name}/wt")),
        branch: None,
        last_output_ms: None,
        preview: None,
        state,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
        parked_at_ms: None,
    }
}

pub(super) fn run(name: &str, completed: usize, total: usize) -> RunSummary {
    RunSummary {
        id: name.into(),
        objective: Some(format!("Objective {name}")),
        coordinator: Some(id(&format!("{name}-coord"))),
        task_counts: (total > 0).then(|| TaskCounts {
            completed,
            pending: total - completed,
            ..TaskCounts::default()
        }),
    }
}

/// The King, run "r1" with a coordinator and one worker per state, a loose terminal.
pub(super) fn snapshot() -> ClanSnapshot {
    let mut s = ClanSnapshot::default();
    s.runs = vec![run("r1", 1, 4)];
    s.nodes
        .push(node("king", NodeKind::King, None, NodeState::Working));
    s.nodes.push(node(
        "r1-coord",
        NodeKind::Coordinator,
        Some("r1"),
        NodeState::Working,
    ));
    for (name, state) in [
        ("working", NodeState::Working),
        ("idle", NodeState::Idle),
        ("waiting", NodeState::NeedsInput),
        ("stalled", NodeState::Stalled),
        ("done", NodeState::Done),
        ("failed", NodeState::Failed),
    ] {
        s.nodes
            .push(node(name, NodeKind::Worker, Some("r1"), state));
    }
    s.nodes
        .push(node("loose", NodeKind::Terminal, None, NodeState::Idle));
    s
}

fn scene_with(snapshot: &ClanSnapshot, hidden: &BTreeSet<NodeId>, pr: &[&str]) -> Scene {
    let pr_ready: HashSet<NodeId> = pr.iter().map(|p| id(p)).collect();
    let input = FarmInput {
        snapshot,
        hidden,
        pr_ready: &pr_ready,
        building: &HashSet::new(),
        now_ms: 0,
    };
    layout(&input, RECT)
}

pub(super) fn scene(snapshot: &ClanSnapshot) -> Scene {
    scene_with(snapshot, &BTreeSet::new(), &[])
}

pub(super) fn dwarf<'a>(scene: &'a Scene, name: &str) -> &'a Dwarf {
    scene
        .dwarf(&id(name))
        .unwrap_or_else(|| panic!("{name} is drawn"))
}

fn tunnel_of(scene: &Scene, name: &str) -> usize {
    match dwarf(scene, name).spot {
        Spot::Mouth(ref t, _) | Spot::Chamber(ref t, _) => scene
            .tunnels
            .iter()
            .position(|u| &u.key == t)
            .expect("its tunnel is drawn"),
        ref other => panic!("{name} is not in a tunnel: {other:?}"),
    }
}

#[test]
fn king_has_a_regular_body_and_stands_on_the_throne_hall_floor() {
    let scene = scene(&snapshot());
    let king = dwarf(&scene, "king");
    assert_eq!(king.size, dwarf(&scene, "working").size);
    assert_eq!(
        crate::ui::dwarf::Role::King.scale(),
        crate::ui::dwarf::Role::Miner.scale()
    );
    let foot = king.pos.y + king.size / 2.0;
    assert!((scene.throne_hall.bottom() - foot - 0.1 * king.size).abs() < 0.01);
}

#[test]
fn finished_dwarves_only_visit_the_treasury_for_review_work() {
    for kind in [NodeKind::Worker, NodeKind::Coordinator, NodeKind::Terminal] {
        for state in [NodeState::Done, NodeState::Idle] {
            for pr in [false, true] {
                let mut s = ClanSnapshot::default();
                s.runs = vec![run("r1", 0, 0)];
                let run = (kind != NodeKind::Terminal).then_some("r1");
                let mut n = node("finished", kind, run, state);
                n.terminal_handle = Some("open-terminal".into());
                s.nodes.push(n);
                let scene = scene_with(&s, &BTreeSet::new(), if pr { &["finished"] } else { &[] });
                let dwarf = dwarf(&scene, "finished");
                if pr && kind != NodeKind::Worker {
                    assert!(matches!(dwarf.spot, Spot::Storage(_)));
                    assert_eq!(dwarf.pose, Pose::Carry);
                    assert_eq!(dwarf.mark, Some(Mark::PrReady));
                    assert_eq!(scene.pr_ready, 1);
                } else {
                    let key = if kind == NodeKind::Terminal {
                        TunnelKey::Repo("repo-finished".into())
                    } else {
                        TunnelKey::Run("r1".into())
                    };
                    let expected = if kind == NodeKind::Coordinator {
                        Spot::Mouth(key, 0)
                    } else {
                        Spot::Chamber(key, 0)
                    };
                    assert_eq!(dwarf.spot, expected, "{kind:?} {state:?} pr={pr}");
                    assert_eq!(dwarf.pose, Pose::Sleep);
                    assert_eq!(dwarf.mark, Some(Mark::Sleep));
                    assert_eq!(scene.pr_ready, 0);
                }
            }
        }
    }
}

#[test]
fn state_picks_the_place() {
    use Home::*;
    use NodeState::*;
    let agents = [
        NodeKind::Coordinator,
        NodeKind::Attendant,
        NodeKind::Worker,
        NodeKind::Terminal,
    ];
    for kind in agents {
        let post = if matches!(kind, NodeKind::Coordinator | NodeKind::Attendant) {
            Mouth
        } else {
            Chamber
        };
        for pr in [false, true] {
            assert_eq!(home(kind, NeedsInput, pr), Landing, "{kind:?}");
            let resting = if pr && kind != NodeKind::Worker {
                Storage
            } else {
                post
            };
            for state in [Done, Idle] {
                assert_eq!(home(kind, state, pr), resting, "{kind:?} {state:?}");
            }
            for state in [Working, Stalled, Failed] {
                assert_eq!(home(kind, state, pr), post, "{kind:?} {state:?}");
            }
        }
    }
    for state in [Working, Idle, NeedsInput, Stalled, Done, Failed] {
        assert_eq!(home(NodeKind::King, state, false), King);
        assert_eq!(home(NodeKind::Subagent, state, false), Trail);
    }
}

#[test]
fn state_picks_pose_and_mark() {
    use NodeState::*;
    let worker = |state| {
        pose(
            NodeKind::Worker,
            state,
            home(NodeKind::Worker, state, false),
        )
    };
    assert_eq!(worker(Working), Pose::Dig);
    assert_eq!(worker(Idle), Pose::Sleep);
    assert_eq!(worker(Stalled), Pose::Sleep);
    assert_eq!(worker(NeedsInput), Pose::Wait);
    assert_eq!(worker(Done), Pose::Sleep);
    assert_eq!(worker(Failed), Pose::Failed);
    let coord = pose(NodeKind::Coordinator, Working, Home::Mouth);
    assert_eq!(coord, Pose::Stand, "coordinators stand at the mouth");
    assert_eq!(mark(Idle, false), Some(Mark::Sleep));
    assert_eq!(
        mark(Stalled, false),
        Some(Mark::Stalled),
        "stalled differs from idle"
    );
    assert_eq!(mark(NeedsInput, false), Some(Mark::Alert));
    assert_eq!(mark(Failed, false), Some(Mark::Failed));
    assert_eq!(mark(Done, false), Some(Mark::Sleep));
    assert_eq!(mark(Done, true), Some(Mark::PrReady));
    assert_eq!(mark(Idle, true), Some(Mark::PrReady));
    assert_eq!(mark(Working, false), None);
}

#[test]
fn dwarves_go_where_their_state_says() {
    let scene = scene(&snapshot());
    let t = tunnel_of(&scene, "r1-coord");
    let tunnel = &scene.tunnels[t];
    assert_eq!(tunnel.key, TunnelKey::Run("r1".into()));

    let king = dwarf(&scene, "king");
    assert_eq!(king.spot, Spot::King);
    assert!(scene.throne_hall.contains(king.pos));

    let coord = dwarf(&scene, "r1-coord");
    assert!(matches!(coord.spot, Spot::Mouth(..)));
    assert_eq!(coord.tag, None, "its tunnel's label names it");
    assert_eq!(dwarf(&scene, "working").tag.as_deref(), Some("working"));
    assert!(
        tunnel.chamber.contains(coord.pos),
        "inside the gallery mouth"
    );

    for name in ["working", "idle", "stalled", "done", "failed"] {
        let a = dwarf(&scene, name);
        assert!(matches!(a.spot, Spot::Chamber(..)), "{name}");
        assert_eq!(tunnel_of(&scene, name), t, "{name}");
        assert!(tunnel.chamber.contains(a.pos), "{name} in the chamber");
    }
    assert!(
        dwarf(&scene, "working").facing_right,
        "diggers face the dig face"
    );
    let (working, idle) = (dwarf(&scene, "working").pos, dwarf(&scene, "idle").pos);
    assert!(
        working.x > dwarf(&scene, "stalled").pos.x && working.x >= idle.x,
        "diggers at the face, sleepers behind them"
    );

    let waiting = dwarf(&scene, "waiting");
    assert_eq!(waiting.spot, Spot::Landing(0));
    assert!(waiting.pos.y < scene.surface_y, "on the surface");
    assert!(scene.landing.contains(waiting.pos));

    let done = dwarf(&scene, "done");
    assert_eq!(done.pose, Pose::Sleep);
    assert_eq!(done.mark, Some(Mark::Sleep));
}

#[test]
fn everyone_waiting_on_karl_shares_the_landing() {
    let mut s = ClanSnapshot::default();
    s.runs = vec![run("a", 0, 0), run("b", 0, 0)];
    s.nodes.push(node(
        "a-coord",
        NodeKind::Coordinator,
        Some("a"),
        NodeState::NeedsInput,
    ));
    s.nodes.push(node(
        "b1",
        NodeKind::Worker,
        Some("b"),
        NodeState::NeedsInput,
    ));
    s.nodes
        .push(node("t", NodeKind::Terminal, None, NodeState::NeedsInput));
    let scene = scene(&s);
    assert_eq!(
        dwarf(&scene, "a-coord").tag.as_deref(),
        Some("Objective a"),
        "a coordinator away from its tunnel is named by its run"
    );
    assert_eq!(dwarf(&scene, "b1").tag.as_deref(), Some("b1"));
    let spots: Vec<Spot> = ["a-coord", "b1", "t"]
        .iter()
        .map(|n| dwarf(&scene, n).spot.clone())
        .collect();
    assert_eq!(
        spots,
        [Spot::Landing(0), Spot::Landing(1), Spot::Landing(2)]
    );
    for n in ["a-coord", "b1", "t"] {
        assert!(scene.landing.contains(dwarf(&scene, n).pos), "{n}");
    }
}

#[test]
fn pr_ready_dwarves_mark_and_pile_in_storage() {
    let mut s = snapshot();
    for n in &mut s.nodes {
        if matches!(n.id.as_str(), "done" | "idle") {
            n.kind = NodeKind::Terminal;
        }
    }
    let scene = scene_with(&s, &BTreeSet::new(), &["done", "idle"]);
    assert_eq!(dwarf(&scene, "done").mark, Some(Mark::PrReady));
    let idle = dwarf(&scene, "idle");
    assert_eq!(
        idle.spot,
        Spot::Storage(0),
        "idle with a PR waits for review in storage"
    );
    assert_eq!(idle.pose, Pose::Carry);
    assert_eq!(scene.pr_ready, 2);
}

#[test]
fn one_tunnel_per_run_then_per_repo() {
    let mut s = ClanSnapshot::default();
    s.runs = vec![run("second", 0, 0), run("first", 0, 0)];
    s.nodes.push(node(
        "first-coord",
        NodeKind::Coordinator,
        Some("first"),
        NodeState::Working,
    ));
    s.nodes.push(node(
        "second-coord",
        NodeKind::Coordinator,
        Some("second"),
        NodeState::Working,
    ));
    let mut a1 = node("a1", NodeKind::Terminal, None, NodeState::Working);
    let mut a2 = node("a2", NodeKind::Terminal, None, NodeState::Idle);
    a1.worktree_path = Some("/x/orca/workspaces/alpha/one".into());
    a2.worktree_path = Some("/x/orca/workspaces/alpha/two".into());
    s.nodes.extend([a1, a2]);
    let scene = scene(&s);
    let keys: Vec<TunnelKey> = scene.tunnels.iter().map(|t| t.key.clone()).collect();
    assert_eq!(
        keys,
        [
            TunnelKey::Run("second".into()),
            TunnelKey::Run("first".into()),
            TunnelKey::Repo("alpha".into()),
        ],
        "runs in run-list order, then repos"
    );
    assert_eq!(tunnel_of(&scene, "a1"), tunnel_of(&scene, "a2"));
    assert_eq!(scene.tunnels[0].label, "Objective second");
    assert_eq!(
        scene.tunnels[0].tasks, None,
        "no task counts, no progress bar"
    );
    let ys: Vec<f32> = scene.tunnels.iter().map(|t| t.chamber.top()).collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "top to bottom: {ys:?}");
}

#[test]
fn past_dispatches_without_a_terminal_are_not_drawn() {
    let mut s = ClanSnapshot::default();
    s.runs = vec![run("r1", 1, 4)];
    s.nodes.push(node(
        "r1-coord",
        NodeKind::Coordinator,
        Some("r1"),
        NodeState::Idle,
    ));
    let past = |name: &str, state, worker_state, dispatch_status| Node {
        live: false,
        worker_state: Some(worker_state),
        dispatch_status: Some(dispatch_status),
        worker_terminal_state: Some(WorkerTerminalState::Reclaimable),
        ..node(name, NodeKind::Worker, Some("r1"), state)
    };
    s.nodes.push(past(
        "old-done",
        NodeState::Done,
        WorkerState::Succeeded,
        DispatchStatus::Completed,
    ));
    s.nodes.push(past(
        "superseded",
        NodeState::Failed,
        WorkerState::Failed,
        DispatchStatus::Failed,
    ));
    s.nodes.push(Node {
        live: false,
        dispatch_status: Some(DispatchStatus::Dispatched),
        ..node("lost", NodeKind::Worker, Some("r1"), NodeState::Failed)
    });
    s.nodes.push(Node {
        live: false,
        worker_terminal_state: Some(WorkerTerminalState::Released),
        released_at_ms: Some(0),
        parked_at_ms: None,
        ..node("released", NodeKind::Worker, Some("r1"), NodeState::Done)
    });
    let scene = scene(&s);
    assert!(
        scene.dwarf(&id("old-done")).is_none(),
        "no done without a terminal"
    );
    assert!(scene.dwarf(&id("superseded")).is_none());
    assert_eq!(
        dwarf(&scene, "lost").pose,
        Pose::Failed,
        "an open dispatch that lost its terminal stays, failed"
    );
    assert!(
        scene.dwarf(&id("released")).is_some(),
        "released ones fade out"
    );
}

#[test]
fn a_finished_run_without_workers_has_no_tunnel() {
    let mut s = ClanSnapshot::default();
    s.runs = vec![run("over", 1, 1), run("ongoing", 2, 2)];
    let mut coord = node(
        "over-coord",
        NodeKind::Coordinator,
        Some("over"),
        NodeState::Idle,
    );
    coord.label = "Other work".into();
    coord.worktree_path = Some("/x/orca/workspaces/alpha/one".into());
    s.nodes.push(coord);
    s.nodes.push(node(
        "ongoing-coord",
        NodeKind::Coordinator,
        Some("ongoing"),
        NodeState::Idle,
    ));
    s.nodes.push(node(
        "w",
        NodeKind::Worker,
        Some("ongoing"),
        NodeState::Idle,
    ));
    let scene = scene(&s);
    let keys: Vec<TunnelKey> = scene.tunnels.iter().map(|t| t.key.clone()).collect();
    assert_eq!(
        keys,
        [
            TunnelKey::Run("ongoing".into()),
            TunnelKey::Repo("alpha".into()),
        ],
        "a finished run's coordinator digs in its repo's tunnel; one with a worker keeps its own"
    );
    let c = dwarf(&scene, "over-coord");
    assert!(matches!(c.spot, Spot::Chamber(TunnelKey::Repo(ref r), _) if r == "alpha"));
    assert_eq!(c.tag.as_deref(), Some("Other work"));
}

#[test]
fn task_progress_keeps_galleries_on_their_level() {
    let mut s = ClanSnapshot::default();
    s.runs = vec![run("early", 0, 4), run("late", 4, 4)];
    s.nodes.push(node(
        "e",
        NodeKind::Worker,
        Some("early"),
        NodeState::Working,
    ));
    s.nodes.push(node(
        "l",
        NodeKind::Worker,
        Some("late"),
        NodeState::Working,
    ));
    // A view with height to spare under the rooms.
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 1400.0));
    let input = FarmInput {
        snapshot: &s,
        hidden: &BTreeSet::new(),
        pr_ready: &HashSet::new(),
        building: &HashSet::new(),
        now_ms: 0,
    };
    let scene = layout(&input, rect);
    let (early, late) = (&scene.tunnels[0], &scene.tunnels[1]);
    assert_eq!(early.progress, Some(0.0));
    assert_eq!(late.progress, Some(1.0));
    assert_eq!(early.tasks, Some((0, 4)));
    assert_eq!(late.tasks, Some((4, 4)));
    assert!(late.chamber.top() > early.chamber.bottom());
    assert!(late.chamber.bottom() <= rect.bottom());
    let before = scene;
    s.runs[0].task_counts.as_mut().unwrap().completed = 2;
    let after = layout(
        &FarmInput {
            snapshot: &s,
            hidden: &BTreeSet::new(),
            pr_ready: &HashSet::new(),
            building: &HashSet::new(),
            now_ms: 0,
        },
        rect,
    );
    for (a, b) in before.tunnels.iter().zip(&after.tunnels) {
        assert_eq!(a.chamber, b.chamber, "progress must not move its workspace");
    }
}

#[test]
fn subagents_trail_their_parent() {
    let mut s = snapshot();
    s.nodes
        .push(node("sub", NodeKind::Subagent, None, NodeState::Working));
    s.nodes
        .push(node("orphan", NodeKind::Subagent, None, NodeState::Working));
    s.edges.push(Edge {
        from: id("idle"),
        to: id("sub"),
        kind: EdgeKind::Subagent,
    });
    let scene = scene(&s);
    let (parent, sub) = (dwarf(&scene, "idle"), dwarf(&scene, "sub"));
    assert_eq!(sub.spot, Spot::Trail(id("idle"), 0));
    assert!(sub.size < parent.size, "smaller than its parent");
    assert!(sub.pos.x > parent.pos.x, "behind a left-facing parent");
    assert!((sub.pos - parent.pos).length() < 2.0 * parent.size);
    let parent_at = scene.dwarves.iter().position(|a| a.id == id("idle"));
    let sub_at = scene.dwarves.iter().position(|a| a.id == id("sub"));
    assert!(parent_at < sub_at, "drawn after its parent");
    assert!(matches!(dwarf(&scene, "orphan").spot, Spot::Chamber(..)));
}

#[test]
fn hidden_nodes_are_not_drawn() {
    let hidden: BTreeSet<NodeId> = [id("idle"), id("loose")].into();
    let scene = scene_with(&snapshot(), &hidden, &[]);
    assert!(scene.dwarf(&id("idle")).is_none());
    assert!(
        scene
            .tunnels
            .iter()
            .all(|t| matches!(t.key, TunnelKey::Run(_))),
        "the hidden terminal's tunnel goes too"
    );
}

/// About 30 agents in 6 runs plus loose terminals all fit, without dwarves on top of each other.
#[test]
fn a_big_clan_fits_without_overlap() {
    let mut s = ClanSnapshot::default();
    s.nodes
        .push(node("king", NodeKind::King, None, NodeState::Working));
    let states = [
        NodeState::Working,
        NodeState::Working,
        NodeState::Idle,
        NodeState::NeedsInput,
        NodeState::Done,
    ];
    for r in 0..6 {
        let name = format!("run{r}");
        s.runs.push(run(&name, r, 6));
        s.nodes.push(node(
            &format!("{name}-coord"),
            NodeKind::Coordinator,
            Some(&name),
            NodeState::Working,
        ));
        for (w, state) in states.iter().enumerate() {
            s.nodes.push(node(
                &format!("{name}-w{w}"),
                NodeKind::Worker,
                Some(&name),
                *state,
            ));
        }
    }
    for t in 0..3 {
        s.nodes.push(node(
            &format!("t{t}"),
            NodeKind::Terminal,
            None,
            NodeState::Working,
        ));
    }
    assert_fits(&s);
}

/// A crowd waiting on Karl and a pile of finished work grow the landing and storage.
#[test]
fn crowds_on_the_landing_and_in_storage_fit() {
    let mut s = ClanSnapshot::default();
    s.runs = vec![run("r", 0, 0)];
    for i in 0..14 {
        s.nodes.push(node(
            &format!("wait{i}"),
            NodeKind::Worker,
            Some("r"),
            NodeState::NeedsInput,
        ));
        s.nodes.push(node(
            &format!("done{i}"),
            NodeKind::Terminal,
            Some("r"),
            NodeState::Done,
        ));
    }
    let pr: Vec<String> = (0..14).map(|i| format!("done{i}")).collect();
    let pr: Vec<&str> = pr.iter().map(String::as_str).collect();
    let scene = assert_fits_with_pr(&s, &pr);
    for a in &scene.dwarves {
        let area = if a.state == NodeState::Done {
            scene.storage
        } else {
            scene.landing
        };
        assert!(
            area.contains(a.pos),
            "{} at {:?} outside {area:?}",
            a.id,
            a.pos
        );
    }
    assert!(scene.storage.bottom() < scene.gallery.top());
}

/// Lays `s` out and checks every dwarf is drawn inside the view and none sit on each other.
fn assert_fits(s: &ClanSnapshot) -> Scene {
    assert_fits_with_pr(s, &[])
}

fn assert_fits_with_pr(s: &ClanSnapshot, pr: &[&str]) -> Scene {
    let scene = scene_with(s, &BTreeSet::new(), pr);
    assert_eq!(scene.dwarves.len(), s.nodes.len());
    for a in &scene.dwarves {
        let cell = Rect::from_center_size(a.pos, Vec2::splat(a.size));
        assert!(
            RECT.expand(1.0).contains_rect(cell),
            "{} at {:?}",
            a.id,
            a.pos
        );
    }
    for (i, a) in scene.dwarves.iter().enumerate() {
        for b in &scene.dwarves[i + 1..] {
            let d = (a.pos - b.pos).abs();
            let apart = (a.size + b.size) / 2.0 - 1.0;
            assert!(d.x >= apart || d.y >= apart, "{} overlaps {}", a.id, b.id);
        }
    }
    scene
}

#[test]
fn walking_routes_use_the_shared_ladder_and_entrance() {
    let scene = scene(&snapshot());
    let worker = dwarf(&scene, "working");
    let t = &scene.tunnels[tunnel_of(&scene, "working")];
    let to = pos2(
        scene.landing.center().x,
        scene.surface_y - scene.cell() / 2.0,
    );
    let path = route(worker.pos, to, &worker.spot, &Spot::Landing(0), &scene);
    assert_eq!(along(&path, 0.0).0, worker.pos);
    assert_eq!(along(&path, 1.0).0, to);
    assert!(path.contains(&pos2(
        scene.work_ladder().center().x,
        scene.gallery.center().y
    )));
    assert!(path.contains(&pos2(scene.entrance_x(), scene.gallery.center().y)));
    for leg in path.windows(2) {
        assert!(
            leg[0].x == leg[1].x || leg[0].y == leg[1].y,
            "axis-aligned galleries and ladders"
        );
        let mid = leg[0].lerp(leg[1], 0.5);
        if mid.y > scene.gallery.bottom() && mid.y < t.chamber.top() {
            assert!(
                scene.work_ladder().contains(mid),
                "vertical travel stays on the shared ladder"
            );
        }
    }
    let back = route(to, worker.pos, &Spot::Landing(0), &worker.spot, &scene);
    assert_eq!(back, path.iter().copied().rev().collect::<Vec<_>>());
}

#[test]
fn dwarves_walk_and_climb_then_arrive_in_their_work_pose() {
    use crate::ui::dwarf::{Action, pose};
    let scene = scene(&snapshot());
    let worker = dwarf(&scene, "working");
    assert_eq!(
        super::dwarf_action(worker, true, false, false),
        Action::Walk
    );
    assert_eq!(
        super::dwarf_action(worker, true, true, false),
        Action::Carry
    );
    assert_eq!(
        super::dwarf_action(worker, false, false, false),
        Action::Mine
    );
    let climbing = pose(Action::Climb, 0.4);
    assert!(climbing.stowed);
    assert_ne!(climbing.stride, 0.0, "feet climb the rungs");
    assert!(
        climbing.arm < -1.0 && climbing.back_arm < -1.0,
        "hands reach up"
    );
    assert!(!climbing.nugget);
}

#[test]
fn a_state_change_walks_the_dwarf_over() {
    let mut farm = FarmView::new();
    let mut s = snapshot();
    let before = scene(&s);
    farm.advance(&before, 0.0);
    let start = dwarf(&before, "idle").pos;

    s.nodes
        .iter_mut()
        .find(|n| n.id == id("idle"))
        .unwrap()
        .state = NodeState::NeedsInput;
    let after = scene(&s);
    let target = dwarf(&after, "idle").pos;
    let at = |farm: &mut FarmView, t: f64| {
        let placed = farm.advance(&after, t);
        let i = after
            .dwarves
            .iter()
            .position(|a| a.id == id("idle"))
            .unwrap();
        placed[i]
    };
    let first = at(&mut farm, 10.0);
    assert!(
        first.pos.distance(start) < 0.001,
        "sets off from where it was"
    );
    assert!(first.heading.is_some());
    let mid = at(&mut farm, 10.4);
    assert!(mid.pos != start && mid.pos != target, "on its way");
    let end = at(&mut farm, 20.0);
    assert_eq!(end.pos, target);
    assert_eq!(end.heading, None, "arrived");
    // Unchanged dwarves never walked.
    let i = after
        .dwarves
        .iter()
        .position(|a| a.id == id("working"))
        .unwrap();
    assert_eq!(farm.advance(&after, 20.0)[i].heading, None);
}

#[test]
fn a_resize_moves_dwarves_without_walking() {
    let mut farm = FarmView::new();
    let s = snapshot();
    farm.advance(&scene(&s), 0.0);
    let pr_ready = HashSet::new();
    let hidden = BTreeSet::new();
    let input = FarmInput {
        snapshot: &s,
        hidden: &hidden,
        pr_ready: &pr_ready,
        building: &HashSet::new(),
        now_ms: 0,
    };
    let smaller = layout(&input, Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0)));
    assert!(
        farm.advance(&smaller, 1.0)
            .iter()
            .all(|p| p.heading.is_none())
    );
}

/// Hovering the Ledger's chamber shows its count and newest entries.
#[test]
fn hovering_the_ledger_chamber_lists_its_newest_entries() {
    use eframe::egui::{self, Event};
    let s = snapshot();
    let mut clan = crate::ui::clan_view::ClanView::new();
    clan.sync(&s);
    let mut farm = FarmView::new();
    farm.set_ledger(crate::ledger::Shelf {
        count: 2,
        newest: vec![crate::ledger::Entry {
            path: "stronghold/Setup gotchas.md".into(),
            title: "Setup gotchas".into(),
            folder: "stronghold".into(),
            kind: Some("finding".into()),
            modified_ms: 0,
        }],
    });
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |events: Vec<Event>, farm: &mut FarmView| {
        time += 0.25;
        let input = egui::RawInput {
            screen_rect: Some(RECT),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut rect = Rect::NOTHING;
        let mut output = ctx.run_ui(input, |ui| {
            rect = ui.available_rect_before_wrap();
            farm.show(ui, &s, &mut clan);
        });
        output.textures_delta.clear();
        (rect, output)
    };
    let texts = |output: &egui::FullOutput| {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        output.shapes.iter().for_each(|c| walk(&c.shape, &mut out));
        out
    };
    let (rect, output) = frame(vec![], &mut farm);
    assert!(texts(&output).iter().any(|t| t == "LEDGER · 2"));
    let pr_ready = HashSet::new();
    let hidden = BTreeSet::new();
    let input = FarmInput {
        snapshot: &s,
        hidden: &hidden,
        pr_ready: &pr_ready,
        building: &HashSet::new(),
        now_ms: 0,
    };
    let room = layout(&input, rect).ledger.unwrap();
    frame(vec![Event::PointerMoved(room.center())], &mut farm);
    let mut shown = Vec::new();
    for _ in 0..6 {
        shown = texts(&frame(vec![], &mut farm).1);
    }
    let tooltip = shown
        .iter()
        .find(|t| t.starts_with("The Ledger:"))
        .expect("the chamber's tooltip");
    assert!(
        tooltip
            .starts_with("The Ledger: 2 entries\nNewest:\n• Setup gotchas (stronghold, finding, ")
    );
}

/// A chamber click opens one Ledger pane and never clears the dwarf selection.
#[test]
fn clicking_the_ledger_chamber_opens_its_pane() {
    use crate::ui::dock::{Pane, PaneLayout};
    use eframe::egui::{self, Event, PointerButton};

    let snapshot = snapshot();
    let mut clan = crate::ui::clan_view::ClanView::new();
    clan.sync(&snapshot);
    let mut farm = FarmView::new();
    let mut panes = PaneLayout::default();
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |events: Vec<Event>, panes: &mut PaneLayout| {
        time += 1.0 / 60.0;
        let mut rect = Rect::NOTHING;
        let mut clicked = false;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0))),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                rect = ui.available_rect_before_wrap();
                let response = farm.show(ui, &snapshot, &mut clan);
                if response.open_ledger {
                    assert!(!response.clicked_empty);
                    assert!(!response.open_envoy);
                    assert!(response.clicked.is_none());
                    panes.open_ledger();
                    clicked = true;
                }
            },
        );
        output.textures_delta.clear();
        (rect, clicked)
    };
    let (rect, _) = frame(vec![], &mut panes);
    assert!(!panes.contains(Pane::Ledger));
    let scene = layout(
        &FarmInput {
            snapshot: &snapshot,
            hidden: &BTreeSet::new(),
            pr_ready: &HashSet::new(),
            building: &HashSet::new(),
            now_ms: 0,
        },
        rect,
    );
    let target = scene
        .ledger
        .expect("visible chamber without Envoy at 1280×800")
        .center();
    for _ in 0..2 {
        let button = |pressed| Event::PointerButton {
            pos: target,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![Event::PointerMoved(target)], &mut panes);
        frame(vec![button(true)], &mut panes);
        assert!(frame(vec![button(false)], &mut panes).1);
        assert!(panes.is_visible(Pane::Ledger));
    }
    let saved = panes.encode().unwrap();
    let restored = PaneLayout::decode(&saved).expect("valid layout with just one Ledger tab");
    assert!(restored.is_visible(Pane::Ledger));
    let path = panes.dock_mut().find_tab(&Pane::Ledger).unwrap();
    panes.dock_mut().remove_tab(path);
    assert!(!panes.contains(Pane::Ledger));
    panes.open_ledger();
    assert!(panes.is_visible(Pane::Ledger));
}

/// Clicking a dwarf reports it, as clicking a node does in the clan; clicking soil reports
/// an empty click.
#[test]
fn clicking_an_dwarf_reports_it() {
    use eframe::egui::{self, Event, PointerButton};
    let s = snapshot();
    let mut clan = crate::ui::clan_view::ClanView::new();
    clan.sync(&s);
    let mut farm = FarmView::new();
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |events: Vec<Event>, farm: &mut FarmView| {
        time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(RECT),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut out = None;
        let mut output = ctx.run_ui(input, |ui| {
            let rect = ui.available_rect_before_wrap();
            out = Some((rect, farm.show(ui, &s, &mut clan)));
        });
        output.textures_delta.clear();
        out.expect("a frame ran")
    };
    let (rect, _) = frame(vec![], &mut farm);
    let pr_ready = HashSet::new();
    let hidden = BTreeSet::new();
    let input = FarmInput {
        snapshot: &s,
        hidden: &hidden,
        pr_ready: &pr_ready,
        building: &HashSet::new(),
        now_ms: 0,
    };
    let scene = layout(&input, rect);
    let target = dwarf(&scene, "working").pos;
    let soil = pos2(rect.center().x, rect.bottom() - 4.0);
    let mut responses = Vec::new();
    for at in [target, soil] {
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![Event::PointerMoved(at)], &mut farm);
        frame(vec![button(true)], &mut farm);
        responses.push(frame(vec![button(false)], &mut farm).1);
    }
    assert_eq!(responses[0].clicked, Some(id("working")));
    assert!(!responses[0].clicked_empty);
    assert_eq!(responses[1].clicked, None);
    assert!(responses[1].clicked_empty);
}

/// Dragging a box from empty ground over a chamber selects the dwarves in it, as in the clan.
#[test]
fn dragging_a_box_selects_the_dwarves_in_it() {
    use eframe::egui::{self, Event, PointerButton};
    let s = snapshot();
    let mut clan = crate::ui::clan_view::ClanView::new();
    clan.sync(&s);
    let mut farm = FarmView::new();
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |events: Vec<Event>, farm: &mut FarmView| {
        time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(RECT),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut out = None;
        let mut output = ctx.run_ui(input, |ui| {
            let rect = ui.available_rect_before_wrap();
            out = Some((rect, farm.show(ui, &s, &mut clan)));
        });
        output.textures_delta.clear();
        out.expect("a frame ran")
    };
    let (rect, _) = frame(vec![], &mut farm);
    let pr_ready = HashSet::new();
    let hidden = BTreeSet::new();
    let input = FarmInput {
        snapshot: &s,
        hidden: &hidden,
        pr_ready: &pr_ready,
        building: &HashSet::new(),
        now_ms: 0,
    };
    let scene = layout(&input, rect);
    let chamber = scene.tunnels[tunnel_of(&scene, "working")].chamber;
    let (from, to) = (
        chamber.left_top() - vec2(4.0, 4.0),
        chamber.right_bottom() + vec2(4.0, 4.0),
    );
    let button = |pos, pressed| Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(vec![Event::PointerMoved(from)], &mut farm);
    frame(vec![button(from, true)], &mut farm);
    let mut boxed = None;
    for step in 1..=10 {
        let at = from + (to - from) * (step as f32 / 10.0);
        boxed = boxed.or(frame(vec![Event::PointerMoved(at)], &mut farm).1.boxed);
    }
    boxed = boxed.or(frame(vec![button(to, false)], &mut farm).1.boxed);
    let (hits, additive) = boxed.expect("the box was released");
    assert!(!additive);
    let hits: BTreeSet<NodeId> = hits.into_iter().collect();
    let expected: BTreeSet<NodeId> = ["working", "idle", "stalled", "done", "failed", "r1-coord"]
        .iter()
        .map(|n| id(n))
        .collect();
    assert_eq!(hits, expected);
}

/// A broadcast grows from the King to the farthest corner, a status report closes in from
/// there to him, and both are gone after; a dwarf hops only while the ring is on it.
#[test]
fn ripples_spread_and_dwarves_hop_as_they_pass() {
    use super::{Pulse, hop, ripple_radius};
    let out = |t| ripple_radius(Pulse::Broadcast, t, 1000.0);
    assert_eq!(out(-0.1), None);
    assert_eq!(out(0.0), Some(0.0));
    let early = out(0.3).unwrap();
    let late = out(1.5).unwrap();
    assert!(0.0 < early && early < late && late < 1000.0);
    assert_eq!(out(5.0), None);
    let inward = |t| ripple_radius(Pulse::StatusReport, t, 1000.0);
    assert_eq!(inward(0.0), Some(1000.0));
    let early = inward(0.3).unwrap();
    let late = inward(1.5).unwrap();
    assert!(1000.0 > early && early > late && late > 0.0);
    assert_eq!(inward(5.0), None);
    assert_eq!(hop(300.0, 300.0, 40.0, 8.0), 8.0, "the ring is on it");
    assert_eq!(hop(300.0, 280.0, 40.0, 8.0), 4.0);
    assert_eq!(hop(300.0, 100.0, 40.0, 8.0), 0.0, "not reached yet");
}

/// Dwarves present when the farm opens are placed; one that appears later walks out of the
/// Throne hall.
#[test]
fn new_dwarves_emerge_from_the_king() {
    let mut farm = FarmView::new();
    let mut s = snapshot();
    let first = scene(&s);
    assert!(
        farm.advance(&first, 0.0)
            .iter()
            .all(|p| p.heading.is_none()),
        "everyone already there is placed"
    );
    s.nodes.push(node(
        "hatchling",
        NodeKind::Worker,
        Some("r1"),
        NodeState::Working,
    ));
    let after = scene(&s);
    let i = after
        .dwarves
        .iter()
        .position(|a| a.id == id("hatchling"))
        .unwrap();
    let placed = farm.advance(&after, 1.0);
    assert!(placed[i].heading.is_some(), "walks");
    assert!(
        after
            .throne_hall
            .expand(after.cell())
            .contains(placed[i].pos),
        "from the throne hall"
    );
    assert_eq!(
        farm.advance(&after, 10.0)[i].pos,
        dwarf(&after, "hatchling").pos
    );
}

/// Plants stand on the surface past the landing, in order and apart, the same every frame;
/// the sky's pieces stay in the sky.
#[test]
fn scenery_stays_on_the_surface_and_in_the_sky() {
    use super::nature::Deco;
    use super::scenery;
    let scene = scene(&snapshot());
    let plants = scenery::surface(&scene);
    assert!(plants.len() > 5, "the surface has nature on it");
    assert_eq!(plants, scenery::surface(&scene), "deterministic");
    let mut right = scene.landing.right();
    for p in &plants {
        let [w, h] = p.deco.extent(scene.px);
        assert_eq!(
            p.foot.y,
            scene.surface_y + scene.px,
            "{:?} stands in the grass",
            p.deco
        );
        assert!(
            p.foot.x - w / 2.0 >= right,
            "{:?} overlaps its neighbour",
            p.deco
        );
        assert!(p.foot.x + w / 2.0 <= scene.rect.right());
        assert!(
            p.foot.y - h >= scene.rect.top(),
            "{:?} fits under the top",
            p.deco
        );
        right = p.foot.x + w / 2.0;
    }
    for now in [0.0, 37.5, 10_000.0] {
        for dark in [false, true] {
            for piece in scenery::sky(&scene, now, dark) {
                let h = piece.deco.extent(scene.px)[1];
                assert!(
                    piece.foot.y <= scene.surface_y,
                    "{:?} in the sky",
                    piece.deco
                );
                assert!(piece.foot.y - h >= scene.rect.top() - 0.1);
                if piece.deco == Deco::Cloud {
                    let w = piece.deco.extent(scene.px)[0];
                    assert!(piece.foot.x + w / 2.0 >= scene.rect.left() - 0.1, "wrapped");
                    assert!(piece.foot.x - w / 2.0 <= scene.rect.right() + 0.1);
                }
            }
        }
        for (star, _) in scenery::stars(&scene) {
            assert!(scene.rect.contains(star) && star.y < scene.surface_y);
        }
    }
}

#[test]
fn building_diggers_build() {
    let s = snapshot();
    let building: HashSet<NodeId> = ["working", "idle", "r1-coord"].map(id).into();
    let input = FarmInput {
        snapshot: &s,
        hidden: &BTreeSet::new(),
        pr_ready: &HashSet::new(),
        building: &building,
        now_ms: 0,
    };
    let scene = layout(&input, RECT);
    let pose_of = |name: &str| scene.dwarf(&id(name)).unwrap().pose;
    assert_eq!(pose_of("working"), Pose::Build);
    assert!(scene.dwarf(&id("working")).unwrap().facing_right);
    assert_eq!(pose_of("idle"), Pose::Sleep, "only working dwarves build");
    assert_eq!(
        pose_of("r1-coord"),
        Pose::Stand,
        "coordinators keep standing"
    );
}

/// A dwarf removed from the clan falls; one filtered out or changing state does not.
#[test]
fn only_removed_dwarves_fall() {
    let mut farm = FarmView::new();
    let mut s = snapshot();
    let first = scene(&s);
    farm.bury(&first, &s, 0.0, || false);
    farm.advance(&first, 0.0);

    s.nodes
        .iter_mut()
        .find(|n| n.id == id("working"))
        .unwrap()
        .state = NodeState::Done;
    let hidden: BTreeSet<NodeId> = [id("idle")].into();
    let filtered = scene_with(&s, &hidden, &[]);
    farm.bury(&filtered, &s, 1.0, || false);
    let drawn = farm.advance(&filtered, 1.0);
    let waiting = filtered
        .dwarves
        .iter()
        .position(|a| a.id == id("waiting"))
        .unwrap();
    assert!(
        farm.falls.is_empty(),
        "done and filtered dwarves stay alive"
    );

    s.nodes
        .retain(|n| n.id != id("waiting") && n.id != id("idle"));
    let after = scene_with(&s, &hidden, &[]);
    farm.bury(&after, &s, 2.0, || false);
    farm.advance(&after, 2.0);
    assert_eq!(
        farm.falls.len(),
        1,
        "only the removed dwarf, not the filtered one"
    );
    let fall = farm.falls[0];
    assert_eq!(
        fall.body.at,
        super::motion::to_unit(drawn[waiting].pos, RECT),
        "from where it was last drawn"
    );
    assert!(!fall.still);
    farm.bury(&after, &s, 2.1, || false);
    assert_eq!(farm.falls.len(), 1, "falls once");
}

/// A falling dwarf hops, flips legs up, lands on the farm's bottom after FALL_S, bounces and
/// fades; with reduced motion it fades where it stood.
#[test]
fn a_dead_dwarf_falls_to_the_bottom() {
    use super::death::*;
    let body = Body {
        kind: NodeKind::Worker,
        at: pos2(0.5, 0.2),
        size: 32.0,
        facing_right: true,
        coat: super::sprites::Coat::Plain,
        alpha: 1.0,
        seed: 0,
    };
    let fall = Fall {
        body,
        start_s: 10.0,
        still: false,
    };
    let at = |t: f64| fall.frame(RECT, 10.0 + t).unwrap();
    let start = at(0.0);
    assert_eq!(start.pos, pos2(640.0, 160.0));
    assert!(!start.upside_down);
    assert!(at(0.05).pos.y < 160.0, "hops first");
    assert!(at(0.5).upside_down, "legs up");
    let floor = RECT.bottom() - 16.0;
    assert!(
        at(0.9).pos.y < floor && at(0.9).pos.y > at(0.5).pos.y,
        "falling"
    );
    assert!(
        (at(FALL_S).pos.y - floor).abs() < 0.5,
        "lands at the bottom"
    );
    assert!(at(FALL_S + REST_S / 2.0).pos.y < floor, "bounces");
    assert_eq!(at(FALL_S + REST_S).alpha, 1.0);
    assert!(at(FALL_S + REST_S + FADE_S / 2.0).alpha < 1.0, "fades");
    assert_eq!(fall.frame(RECT, 10.0 + fall.duration()), None, "gone");

    let still = Fall {
        still: true,
        ..fall
    };
    let mid = still.frame(RECT, 10.0 + STILL_FADE_S / 2.0).unwrap();
    assert_eq!(mid.pos, start.pos);
    assert!(!mid.upside_down);
    assert!((mid.alpha - 0.5).abs() < 0.01);
    assert_eq!(still.frame(RECT, 10.0 + STILL_FADE_S), None);
}

/// A crowded chamber in a narrow tunnel packs its dwarves side by side, spread across the
/// chamber, rather than one per row down a tall farm.
#[test]
fn a_crowded_chamber_packs_side_by_side() {
    let mut s = ClanSnapshot::default();
    s.nodes
        .push(node("king", NodeKind::King, None, NodeState::Working));
    for r in 0..6 {
        let name = format!("r{r}");
        s.runs.push(run(&name, 1, 4));
        let workers = if r == 0 { 9 } else { 1 };
        for w in 0..workers {
            s.nodes.push(node(
                &format!("{name}-w{w}"),
                NodeKind::Worker,
                Some(&name),
                NodeState::Failed,
            ));
        }
    }
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 1400.0));
    let pr_ready = HashSet::new();
    let hidden = BTreeSet::new();
    let input = FarmInput {
        snapshot: &s,
        hidden: &hidden,
        pr_ready: &pr_ready,
        building: &HashSet::new(),
        now_ms: 0,
    };
    let scene = layout(&input, rect);
    let chamber = scene.tunnels[tunnel_of(&scene, "r0-w0")].chamber;
    let dwarves: Vec<&Dwarf> = scene
        .dwarves
        .iter()
        .filter(|a| a.id.as_str().starts_with("r0-"))
        .collect();
    let mut rows: Vec<f32> = dwarves.iter().map(|a| a.pos.y).collect();
    rows.sort_by(f32::total_cmp);
    rows.dedup();
    let widest = rows
        .iter()
        .map(|y| dwarves.iter().filter(|a| a.pos.y == *y).count())
        .max()
        .unwrap();
    assert!(widest >= 2, "{} rows of up to {widest}", rows.len());
    for dwarf in dwarves {
        // The name tag is two cells wide.
        let tag = Rect::from_center_size(dwarf.pos, vec2(2.0 * dwarf.size, dwarf.size));
        assert!(
            tag.left() >= chamber.left() - 0.5 && tag.right() <= chamber.right() + 0.5,
            "{} stays in its chamber",
            dwarf.id.as_str()
        );
    }
}

/// A chamber dwarf's subagents, nested ones too, line up behind it in room it reserves:
/// they never cover another dwarf or each other.
#[test]
fn subagent_trails_take_room_in_the_chamber() {
    let mut s = ClanSnapshot::default();
    s.runs = vec![run("r1", 1, 4)];
    for name in ["a", "b", "c", "d"] {
        s.nodes
            .push(node(name, NodeKind::Worker, Some("r1"), NodeState::Working));
    }
    for (sub, parent) in [
        ("s1", "a"),
        ("s2", "a"),
        ("s3", "a"),
        ("s4", "s1"),
        ("s5", "b"),
    ] {
        s.nodes
            .push(node(sub, NodeKind::Subagent, None, NodeState::Working));
        s.edges.push(Edge {
            from: id(parent),
            to: id(sub),
            kind: EdgeKind::Subagent,
        });
    }
    let scene = scene(&s);
    let nested = dwarf(&scene, "s4");
    assert!(matches!(&nested.spot, Spot::Trail(root, _) if *root == id("a")));
    let boxes: Vec<(&str, Rect)> = scene
        .dwarves
        .iter()
        .map(|a| {
            (
                a.id.0.as_str(),
                Rect::from_center_size(a.pos, Vec2::splat(a.size)),
            )
        })
        .collect();
    for (i, (x, rx)) in boxes.iter().enumerate() {
        for (y, ry) in &boxes[i + 1..] {
            let overlap = rx.intersect(*ry);
            assert!(
                overlap.width() <= 0.5 || overlap.height() <= 0.5,
                "{x} overlaps {y}"
            );
        }
    }
    let chamber = scene.tunnels[0].chamber;
    for a in &scene.dwarves {
        assert!(chamber.expand(1.0).contains(a.pos), "{} outside", a.id.0);
    }
}

#[test]
fn parked_dwarves_sleep_in_the_resting_area() {
    let mut s = snapshot();
    let room = |scene: &Scene| {
        scene
            .tunnels
            .iter()
            .position(|t| t.key == TunnelKey::RestingArea)
    };
    assert_eq!(room(&scene(&s)), None, "no room while nobody is parked");
    for i in 0..9 {
        let mut n = node(
            &format!("parked{i}"),
            NodeKind::Terminal,
            None,
            NodeState::Working,
        );
        n.parked_at_ms = Some(1);
        s.nodes.push(n);
    }
    let mut waiting = node(
        "parked-waiting",
        NodeKind::Worker,
        Some("r1"),
        NodeState::NeedsInput,
    );
    waiting.parked_at_ms = Some(1);
    s.nodes.push(waiting);
    let scene = assert_fits(&s);
    let t = room(&scene).expect("a resting area");
    assert_eq!(t, scene.tunnels.len() - 1, "the last tunnel");
    assert_eq!(scene.tunnels[t].label, crate::park::RESTING_AREA);
    for a in scene
        .dwarves
        .iter()
        .filter(|a| a.id.as_str().starts_with("parked"))
    {
        assert!(
            matches!(a.spot, Spot::Chamber(TunnelKey::RestingArea, _)),
            "{}",
            a.id
        );
        assert_eq!(a.pose, Pose::Sleep);
        assert_eq!(a.mark, Some(Mark::Sleep));
    }
    // Parked loose terminals open no tunnel of their own.
    assert!(
        scene
            .tunnels
            .iter()
            .all(|t| !matches!(&t.key, TunnelKey::Repo(r) if r.starts_with("repo-parked")))
    );
    // Each sleeper lies on a bed of its own in the room; the beds don't overlap.
    let chamber = scene.tunnels[t].chamber;
    let sleepers: Vec<&Dwarf> = scene
        .dwarves
        .iter()
        .filter(|a| a.id.as_str().starts_with("parked"))
        .collect();
    assert!(scene.beds.len() >= sleepers.len());
    for a in &sleepers {
        let bed = scene
            .beds
            .iter()
            .find(|b| b.sleeper.as_ref() == Some(&a.id))
            .unwrap_or_else(|| panic!("{} has a bed", a.id));
        assert!((bed.rect.center().x - a.pos.x).abs() < 0.5);
        assert!(bed.rect.bottom() > a.pos.y && bed.rect.top() < a.pos.y + a.size);
    }
    for (i, bed) in scene.beds.iter().enumerate() {
        assert!(chamber.contains_rect(bed.rect), "bed {i} in the room");
        for other in &scene.beds[i + 1..] {
            assert!(!bed.rect.intersects(other.rect), "beds overlap");
        }
    }
}

#[test]
fn only_the_resting_area_has_beds() {
    assert!(scene(&snapshot()).beds.is_empty());
}

/// Many runs in a tall view keep separate horizontal bands and a clear shared ladder.
#[test]
fn many_tunnels_keep_separate_bands_in_a_tall_view() {
    let mut s = ClanSnapshot::default();
    s.nodes
        .push(node("king", NodeKind::King, None, NodeState::Working));
    for r in 0..10 {
        let name = format!("r{r}");
        s.runs.push(run(&name, r % 4, 4));
        for w in 0..2 {
            s.nodes.push(node(
                &format!("{name}-w{w}"),
                NodeKind::Worker,
                Some(&name),
                NodeState::Working,
            ));
        }
    }
    // A tall, narrow view: ten rooms side by side would draw the dwarves at scale 1.
    let size = vec2(800.0, 1400.0);
    let rect = Rect::from_min_size(Pos2::ZERO, size);
    let input = FarmInput {
        snapshot: &s,
        hidden: &BTreeSet::new(),
        pr_ready: &HashSet::new(),
        building: &HashSet::new(),
        now_ms: 0,
    };
    let mut plan = super::rooms::RoomPlan::default();
    let scene = layout_in(&input, rect, &mut plan);
    assert!(plan.grid().unwrap().levels >= 2, "{:?}", plan.grid());
    assert!(scene.px >= 2.0, "{}", scene.px);
    let tunnels = &scene.tunnels;
    assert!(
        tunnels
            .iter()
            .any(|t| tunnels.iter().any(|u| u.chamber.top() > t.chamber.bottom())),
        "chambers stack"
    );
    for (i, t) in tunnels.iter().enumerate() {
        assert!(rect.contains_rect(t.chamber), "chamber {i} in the view");
        for (j, u) in tunnels.iter().enumerate().filter(|(j, _)| *j != i) {
            assert!(
                !t.chamber.intersects(u.chamber),
                "chambers {i} and {j} overlap"
            );
            assert!(
                !scene.work_ladder().intersects(u.chamber),
                "shared ladder crosses chamber {j}"
            );
        }
    }
    for a in scene.dwarves.iter().filter(|a| a.kind == NodeKind::Worker) {
        let t = &tunnels[tunnel_of(&scene, a.id.as_str())];
        assert!(t.chamber.contains(a.pos), "{} in its chamber", a.id);
    }
}

#[test]
fn envoy_stays_in_its_comms_room_without_a_work_tunnel() {
    let mut snapshot = ClanSnapshot::default();
    snapshot
        .nodes
        .push(node("king", NodeKind::King, None, NodeState::Idle));
    snapshot
        .nodes
        .push(node("envoy", NodeKind::Envoy, None, NodeState::Idle));
    for state in [NodeState::Idle, NodeState::Working, NodeState::NeedsInput] {
        snapshot.nodes[1].state = state;
        let scene = scene(&snapshot);
        let envoy = dwarf(&scene, "envoy");
        assert_eq!(envoy.spot, Spot::Comms);
        assert_eq!(envoy.pose, Pose::Stand);
        assert_eq!(envoy.mark, None);
        assert!(scene.comms.unwrap().chamber.contains(envoy.pos));
        assert!(envoy.pos.x < scene.throne_hall.left());
        assert!(scene.tunnels.is_empty());
    }
}

#[test]
fn comms_room_fits_beside_entrance_at_small_and_large_clans() {
    for size in [
        vec2(176.0, 400.0),
        vec2(320.0, 400.0),
        vec2(640.0, 400.0),
        vec2(1280.0, 800.0),
        vec2(2560.0, 1600.0),
    ] {
        for count in [0, 1, 8, 40, 100] {
            let mut snapshot = ClanSnapshot::default();
            snapshot
                .nodes
                .push(node("king", NodeKind::King, None, NodeState::Idle));
            snapshot
                .nodes
                .push(node("envoy", NodeKind::Envoy, None, NodeState::Idle));
            for i in 0..count {
                snapshot.nodes.push(node(
                    &format!("w{i}"),
                    NodeKind::Worker,
                    Some(&format!("r{}", i % 8)),
                    NodeState::Working,
                ));
            }
            let scene = layout(
                &FarmInput {
                    snapshot: &snapshot,
                    hidden: &BTreeSet::new(),
                    pr_ready: &HashSet::new(),
                    building: &HashSet::new(),
                    now_ms: 0,
                },
                Rect::from_min_size(pos2(13.0, 29.0), size),
            );
            let room = scene.comms.unwrap();
            assert!(scene.rect.contains_rect(room.chamber));
            assert!(room.chamber.top() > scene.surface_y);
            assert!(room.chamber.width() >= scene.throne_hall.width());
            assert!(room.chamber.height() >= scene.throne_hall.height());
            assert!(scene.landing.left() - room.chamber.right() >= scene.cell());
            assert!(room.feed().y > scene.rect.top());
            assert!(!room.chamber.intersects(scene.throne_hall));
            assert!(!room.chamber.intersects(scene.storage));
            let entrance = Rect::from_min_max(
                pos2(scene.entrance_x() - 0.6 * scene.cell(), scene.surface_y),
                pos2(
                    scene.entrance_x() + 0.6 * scene.cell(),
                    scene.gallery.center().y,
                ),
            );
            assert!(!room.chamber.intersects(entrance));
            for tunnel in &scene.tunnels {
                assert!(!room.chamber.intersects(tunnel.chamber));
                assert!(!room.chamber.intersects(scene.work_ladder()));
            }
            assert!(room.shaft.intersects(scene.gallery));
            assert!(room.shaft.intersects(room.chamber));
            assert_eq!(
                room.receiver,
                pos2(room.chamber.center().x, scene.surface_y)
            );
            let envoy = dwarf(&scene, "envoy");
            let dwarf_cell = Rect::from_center_size(envoy.pos, Vec2::splat(envoy.size));
            let basin = room.basin_rect();
            let tray = room.tray_rect();
            assert!(room.chamber.contains_rect(tray));
            assert!(room.chamber.contains_rect(basin));
            assert!(!tray.intersects(basin));
            assert!(!tray.intersects(dwarf_cell));
            assert!(!tray.intersects(scene.throne_hall));
            assert!(basin.center().x >= dwarf_cell.right());
            assert!(
                basin.top() > dwarf_cell.top(),
                "rim below the priest's head"
            );
            assert!(
                basin.top() < dwarf_cell.bottom(),
                "hands can reach across the rim"
            );
            assert_eq!(room.shaft.center().x, envoy.pos.x);
            assert!(!room.shaft.intersects(scene.throne_hall));
            assert!(
                room.chamber
                    .contains_rect(Rect::from_center_size(envoy.pos, Vec2::splat(envoy.size)))
            );
        }
    }
}

#[test]
fn comms_room_disappears_with_envoy() {
    let mut snapshot = snapshot();
    assert!(scene(&snapshot).comms.is_none());
    snapshot
        .nodes
        .push(node("envoy", NodeKind::Envoy, None, NodeState::Idle));
    assert!(scene(&snapshot).comms.is_some());
    let hidden = BTreeSet::from([id("envoy")]);
    assert!(scene_with(&snapshot, &hidden, &[]).comms.is_none());
}

#[test]
fn waiting_dwarves_keep_clear_of_the_comms_room() {
    for (size, count) in [(vec2(640.0, 800.0), 8), (vec2(1280.0, 800.0), 24)] {
        let mut snapshot = ClanSnapshot::default();
        snapshot
            .nodes
            .push(node("king", NodeKind::King, None, NodeState::Idle));
        snapshot
            .nodes
            .push(node("envoy", NodeKind::Envoy, None, NodeState::Idle));
        for i in 0..count {
            snapshot.nodes.push(node(
                &format!("waiting-{i}"),
                NodeKind::Worker,
                None,
                NodeState::NeedsInput,
            ));
        }
        let scene = layout(
            &FarmInput {
                snapshot: &snapshot,
                hidden: &BTreeSet::new(),
                pr_ready: &HashSet::new(),
                building: &HashSet::new(),
                now_ms: 0,
            },
            Rect::from_min_size(Pos2::ZERO, size),
        );
        let room = scene.comms.unwrap();
        for dwarf in scene
            .dwarves
            .iter()
            .filter(|dwarf| matches!(dwarf.spot, Spot::Landing(_)))
        {
            let cell = Rect::from_center_size(dwarf.pos, Vec2::splat(dwarf.size));
            assert!(scene.landing.contains_rect(cell));
            assert!(scene.rect.contains_rect(cell));
            assert!(cell.left() - room.chamber.right() >= scene.cell());
        }
    }
}

/// The Envoy, the King and `n` agents: runs of a coordinator and up to four workers, every
/// third worker idle, every fifth waiting on Karl, every seventh done.
pub(super) fn typical_clan(n: usize) -> ClanSnapshot {
    let mut s = ClanSnapshot::default();
    s.nodes
        .push(node("king", NodeKind::King, None, NodeState::Working));
    s.nodes
        .push(node("envoy", NodeKind::Envoy, None, NodeState::Idle));
    for i in 0..n {
        let r = i / 5;
        let name = format!("r{r}");
        if i % 5 == 0 {
            s.runs.push(run(&name, r % 4, 6));
            s.nodes.push(node(
                &format!("{name}-coord"),
                NodeKind::Coordinator,
                Some(&name),
                NodeState::Working,
            ));
            continue;
        }
        let state = match i {
            _ if i % 7 == 6 => NodeState::Done,
            _ if i % 5 == 4 => NodeState::NeedsInput,
            _ if i % 3 == 2 => NodeState::Idle,
            _ => NodeState::Working,
        };
        s.nodes.push(node(
            &format!("{name}-w{i}"),
            NodeKind::Worker,
            Some(&name),
            state,
        ));
    }
    s
}

/// Share of the occupied work levels used by rooms and their labels. Unexcavated rock
/// below the last gallery is no longer counted as tunnel depth.
pub(super) fn fill(scene: &Scene) -> f32 {
    let a = scene.cell();
    let rooms: f32 = scene
        .tunnels
        .iter()
        .map(|t| (t.chamber.width() + 0.6 * a) * (t.chamber.bottom() - t.label_top.y))
        .sum();
    let bottom = scene
        .tunnels
        .iter()
        .map(|t| t.chamber.bottom())
        .fold(scene.gallery.bottom(), f32::max);
    rooms / (scene.rect.width() * (bottom - scene.gallery.bottom()))
}

/// Share of the rect's height the farm reaches down to.
pub(super) fn reach(scene: &Scene) -> f32 {
    let bottom = scene
        .tunnels
        .iter()
        .map(|t| t.chamber.bottom())
        .fold(scene.gallery.bottom(), f32::max);
    (bottom - scene.rect.top()) / scene.rect.height()
}

/// Workspace bands fit typical clan sizes without overlapping. Keeping groups on
/// separate rows trades some dwarf size and ground fill for clear ownership.
#[test]
fn the_farm_fills_the_view() {
    // (view, dwarves, smallest dwarf cell in points, least fill)
    let cases = [
        (vec2(1280.0, 800.0), 5, 56.0, 0.75),
        // Finished workers now occupy chamber slots alongside the other miners.
        (vec2(1280.0, 800.0), 15, 32.0, 0.65),
        (vec2(1280.0, 800.0), 30, 24.0, 0.65),
        (vec2(900.0, 650.0), 5, 48.0, 0.75),
        (vec2(900.0, 650.0), 15, 24.0, 0.65),
        (vec2(900.0, 650.0), 30, 16.0, 0.65),
    ];
    for (size, n, cell, least) in cases {
        let s = typical_clan(n);
        let rect = Rect::from_min_size(Pos2::ZERO, size);
        let input = FarmInput {
            snapshot: &s,
            hidden: &BTreeSet::new(),
            pr_ready: &HashSet::new(),
            building: &HashSet::new(),
            now_ms: 0,
        };
        let scene = layout(&input, rect);
        let fill = fill(&scene);
        assert!(
            (least..=0.95).contains(&fill),
            "{n} dwarves in {size:?}: fill {fill}"
        );
        assert!(
            reach(&scene) <= 1.0,
            "{n} dwarves in {size:?} fit vertically"
        );
        assert!(
            scene.cell() >= cell,
            "{n} dwarves in {size:?}: {}",
            scene.cell()
        );
        for (i, t) in scene.tunnels.iter().enumerate() {
            assert!(rect.contains_rect(t.chamber), "chamber {i} in the view");
            for u in &scene.tunnels[i + 1..] {
                assert!(!t.chamber.intersects(u.chamber), "chambers overlap");
            }
        }
        for a in scene.dwarves.iter().filter(|a| a.kind == NodeKind::Worker) {
            if let Spot::Chamber(..) = a.spot {
                let t = &scene.tunnels[tunnel_of(&scene, a.id.as_str())];
                assert!(t.chamber.contains(a.pos), "{} in its chamber", a.id);
            }
        }
    }
}

/// A near-empty farm in a big view draws its dwarves no bigger than the largest scale.
#[test]
fn a_lone_dwarf_is_not_drawn_giant() {
    let s = typical_clan(2);
    let input = FarmInput {
        snapshot: &s,
        hidden: &BTreeSet::new(),
        pr_ready: &HashSet::new(),
        building: &HashSet::new(),
        now_ms: 0,
    };
    let scene = layout(
        &input,
        Rect::from_min_size(Pos2::ZERO, vec2(2400.0, 1600.0)),
    );
    assert_eq!(scene.px, super::rooms::MAX_PX);
}

#[test]
fn rune_priest_scries_only_while_working() {
    use crate::ui::dwarf::{self, Action, Role};
    let mut dwarf = scene(&snapshot())
        .dwarves
        .into_iter()
        .find(|a| a.kind != NodeKind::King)
        .unwrap();
    dwarf.kind = NodeKind::Envoy;
    for state in [
        NodeState::Working,
        NodeState::Idle,
        NodeState::NeedsInput,
        NodeState::Done,
        NodeState::Failed,
        NodeState::Stalled,
    ] {
        dwarf.state = state;
        let action = super::dwarf_action(&dwarf, false, false, false);
        assert_eq!(
            action,
            if state == NodeState::Working {
                Action::Scry
            } else {
                Action::Still
            }
        );
        assert_eq!(
            dwarf::pose(action, 0.0) != dwarf::pose(action, 0.3),
            state == NodeState::Working
        );
        assert_eq!(super::dwarf_look(&dwarf).role, Role::RunePriest);
    }
}

#[test]
fn stalled_dwarf_holds_still_in_a_helmet_for_the_pebble() {
    use crate::ui::dwarf::{Action, Hat};
    let scene = scene(&snapshot());
    let stalled = dwarf(&scene, "stalled");
    assert_eq!(
        super::dwarf_action(stalled, false, false, false),
        Action::Wait
    );
    assert_eq!(super::dwarf_look(stalled).hat, Hat::Helmet);
}

#[test]
fn portal_stone_threshold_meets_the_surface_in_every_outline() {
    use super::world::{Prop, render};
    use crate::ui::dwarf::cutout::Outline;
    let scene = scene(&snapshot());
    let foot = scene.user_portal();
    for outline in Outline::ALL {
        let image = render(Prop::Portal, Prop::Portal.size(), 0, outline);
        let bottom = (0..image.size[1])
            .rev()
            .find(|&y| image[(image.size[0] / 2, y)].a() > 0)
            .unwrap()
            + 1;
        let stone_bottom = foot.y + (bottom as f32 - PORTAL_H) * scene.px;
        assert!(
            stone_bottom >= scene.surface_y,
            "{outline:?}: floating threshold"
        );
        assert!(
            stone_bottom <= scene.surface_y + 3.0 * scene.px,
            "{outline:?}: buried plate"
        );
    }
}

#[test]
fn king_close_holds_target_in_view_and_bonks_once_instead_of_falling() {
    use eframe::egui;
    let mut snapshot = snapshot();
    snapshot.add_king();
    let mut clan = crate::ui::clan_view::ClanView::new();
    let mut farm = FarmView::new();
    farm.reduce_motion = Some(false);
    let ctx = egui::Context::default();
    let mut frame = |farm: &mut FarmView, snapshot: &ClanSnapshot, time: f64| {
        clan.sync(snapshot);
        let mut response = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(RECT),
                time: Some(time),
                ..Default::default()
            },
            |ui| response = Some(farm.show(ui, snapshot, &mut clan)),
        );
        output.textures_delta.clear();
        response.unwrap()
    };
    frame(&mut farm, &snapshot, 0.0);
    farm.prepare_king_close([id("loose")].into_iter(), 0.01);
    assert!(farm.throws.contains(&id("loose")));
    // The close request itself must not remove or hit a still-live dwarf.
    frame(&mut farm, &snapshot, 0.02);
    assert!(!farm.throws.animating());
    snapshot.nodes.retain(|n| n.id != id("loose"));
    snapshot.add_king();
    assert!(!frame(&mut farm, &snapshot, 0.03).bonk);
    assert!(farm.throws.animating());
    assert!(
        farm.falls.is_empty(),
        "the regular death must wait for the tool"
    );
    assert!(!frame(&mut farm, &snapshot, 0.30).bonk);
    assert!(frame(&mut farm, &snapshot, 0.59).bonk);
    assert!(!frame(&mut farm, &snapshot, 0.60).bonk);
    frame(&mut farm, &snapshot, 0.90);
    assert!(!farm.throws.contains(&id("loose")));
    assert!(farm.falls.is_empty());
}

/// Every work row is horizontal, supported by rock, and connected by the same ladder.
/// Exercise wide and narrow panes with several runs, attendants and subagents.
#[test]
fn work_galleries_and_routes_share_one_ladder_at_every_crowd_size() {
    for (size, n) in [
        (vec2(1280.0, 800.0), 30),
        (vec2(900.0, 650.0), 30),
        (vec2(800.0, 1400.0), 60),
    ] {
        let mut s = typical_clan(n);
        let run = s.runs[0].id.clone();
        for i in 0..6 {
            s.nodes.push(node(
                &format!("attendant-{i}"),
                NodeKind::Attendant,
                Some(&run),
                NodeState::Working,
            ));
        }
        for i in 0..3 {
            let name = format!("apprentice-{i}");
            s.nodes
                .push(node(&name, NodeKind::Subagent, None, NodeState::Working));
            s.edges.push(Edge {
                from: id("attendant-0"),
                to: id(&name),
                kind: EdgeKind::Subagent,
            });
        }
        let input = FarmInput {
            snapshot: &s,
            hidden: &BTreeSet::new(),
            pr_ready: &HashSet::new(),
            building: &HashSet::new(),
            now_ms: 0,
        };
        let scene = layout(&input, Rect::from_min_size(Pos2::new(23.0, 47.0), size));
        let ladder = scene.work_ladder();
        let galleries = scene.work_galleries();
        assert!(scene.rect.contains_rect(ladder));
        for row in &galleries {
            assert!(scene.rect.contains_rect(*row), "{row:?}");
            assert!(row.width() > row.height() * 3.0, "long horizontal gallery");
            assert!(
                row.intersects(ladder),
                "every level reaches the shared ladder"
            );
        }
        for (i, row) in galleries.iter().enumerate() {
            for other in &galleries[i + 1..] {
                assert!(!row.intersects(*other), "rock separates work floors");
            }
        }
        let workers: Vec<_> = scene
            .dwarves
            .iter()
            .filter(|d| {
                matches!(
                    d.spot,
                    Spot::Chamber(..) | Spot::Mouth(..) | Spot::Trail(..)
                )
            })
            .collect();
        for (i, dwarf) in workers.iter().enumerate() {
            let floor = galleries
                .iter()
                .find(|g| g.contains(dwarf.pos))
                .expect("dwarf in a gallery");
            assert!(
                (dwarf.pos.y + dwarf.size / 2.0 - floor.bottom()).abs() < 0.01,
                "feet on the floor"
            );
            for other in &workers[i + 1..] {
                let bodies = Rect::from_center_size(dwarf.pos, Vec2::splat(dwarf.size))
                    .intersect(Rect::from_center_size(other.pos, Vec2::splat(other.size)));
                assert!(
                    bodies.width() < 0.01 || bodies.height() < 0.01,
                    "{} overlaps {}",
                    dwarf.id,
                    other.id
                );
                let path = route(dwarf.pos, other.pos, &dwarf.spot, &other.spot, &scene);
                for leg in path.windows(2) {
                    if leg[0].y != leg[1].y {
                        assert_eq!(leg[0].x, ladder.center().x);
                        assert_eq!(leg[1].x, ladder.center().x);
                    }
                    for step in 0..=8 {
                        let at = leg[0].lerp(leg[1], step as f32 / 8.0);
                        assert!(
                            ladder.contains(at) || galleries.iter().any(|g| g.contains(at)),
                            "journey leaves excavated ground: {at:?}"
                        );
                    }
                }
            }
        }
        // A second state change halfway up the ladder keeps the new route on its rungs.
        let at = ladder.center();
        let target = workers[0];
        let interrupted = route(at, target.pos, &target.spot, &target.spot, &scene);
        assert_eq!(interrupted[0], at);
        assert_eq!(interrupted[1].x, ladder.center().x);
        assert_eq!(*interrupted.last().unwrap(), target.pos);
    }
}

/// Every excavated work row belongs to exactly one labelled workspace group,
/// including overflow rows and the resting area.
#[test]
fn uneven_workspaces_never_share_an_excavated_row() {
    let mut s = ClanSnapshot::default();
    for (repo, count) in [("alpha", 1), ("beta", 3), ("gamma", 9)] {
        for i in 0..count {
            let mut n = node(
                &format!("{repo}-{i}"),
                NodeKind::Terminal,
                None,
                NodeState::Working,
            );
            n.worktree_path = Some(format!("/x/orca/workspaces/{repo}/main"));
            s.nodes.push(n);
        }
    }
    let mut sleeper = node("sleeper", NodeKind::Terminal, None, NodeState::Idle);
    sleeper.parked_at_ms = Some(1);
    s.nodes.push(sleeper);
    for size in [vec2(1280.0, 800.0), vec2(900.0, 650.0), vec2(600.0, 800.0)] {
        let scene = layout(
            &FarmInput {
                snapshot: &s,
                hidden: &BTreeSet::new(),
                pr_ready: &HashSet::new(),
                building: &HashSet::new(),
                now_ms: 0,
            },
            Rect::from_min_size(Pos2::ZERO, size),
        );
        for row in scene.work_galleries() {
            let owners: Vec<_> = scene
                .tunnels
                .iter()
                .filter(|t| t.chamber.intersects(row))
                .collect();
            assert_eq!(
                owners.len(),
                1,
                "{size:?}: gallery {row:?} crosses {owners:?}"
            );
            let owner = owners[0];
            assert!(row.left() <= owner.label_top.x && owner.label_top.x <= row.right());
        }
        for dwarf in &scene.dwarves {
            let Spot::Chamber(key, _) = &dwarf.spot else {
                continue;
            };
            let tunnel = scene.tunnels.iter().find(|t| &t.key == key).unwrap();
            assert!(tunnel.chamber.contains(dwarf.pos));
            assert!(
                scene
                    .work_galleries()
                    .iter()
                    .any(|row| row.contains(dwarf.pos))
            );
        }
        let gamma = scene
            .tunnels
            .iter()
            .find(|t| t.key == TunnelKey::Repo("gamma".into()))
            .unwrap();
        if size.x == 600.0 {
            assert!(
                scene
                    .work_galleries()
                    .iter()
                    .filter(|row| gamma.chamber.intersects(**row))
                    .count()
                    > 1,
                "the large workspace wraps within its own section"
            );
        }
        assert_eq!(scene.beds.iter().filter(|b| b.sleeper.is_some()).count(), 1);
    }
}

#[test]
fn lorekeeper_reads_in_ink_robes_and_can_rest() {
    use crate::ui::dwarf::{Action, Garment, Role, Tool};
    let mut s = snapshot();
    let mut librarian = node("lorekeeper", NodeKind::Lorekeeper, None, NodeState::Working);
    librarian.agent = Some(crate::orca::AgentIdentity::Claude);
    s.nodes.push(librarian);
    let active = scene(&s);
    let reader = dwarf(&active, "lorekeeper");
    assert_eq!(reader.pose, Pose::Stand);
    assert_eq!(
        super::dwarf_action(reader, false, false, false),
        Action::Read
    );
    let look = super::dwarf_look(reader);
    assert_eq!(
        (look.role, look.garment, look.tool),
        (Role::Librarian, Garment::Robe, Tool::Book)
    );
    assert_eq!(
        look.tunic,
        crate::ui::dwarf::Look::for_role(Role::Librarian, 0).tunic
    );
    assert_eq!(
        home(NodeKind::Lorekeeper, NodeState::NeedsInput, false),
        Home::Landing
    );
    s.nodes.last_mut().unwrap().parked_at_ms = Some(1);
    let parked = scene(&s);
    assert_eq!(dwarf(&parked, "lorekeeper").pose, Pose::Sleep);
    assert_eq!(
        super::dwarf_action(dwarf(&parked, "lorekeeper"), false, false, false),
        Action::Sleep
    );
}

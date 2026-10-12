use std::collections::{BTreeSet, HashSet};

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

use super::AntFarmView;
use super::layout::*;
use super::motion::{along, route};
use crate::model::{
    Edge, EdgeKind, FleetSnapshot, Node, NodeId, NodeKind, NodeState, RunSummary, TaskCounts,
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

/// The Queen, run "r1" with a coordinator and one worker per state, a loose terminal.
pub(super) fn snapshot() -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    s.runs = vec![run("r1", 1, 4)];
    s.nodes
        .push(node("queen", NodeKind::Queen, None, NodeState::Working));
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

fn scene_with(snapshot: &FleetSnapshot, hidden: &BTreeSet<NodeId>, pr: &[&str]) -> Scene {
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

pub(super) fn scene(snapshot: &FleetSnapshot) -> Scene {
    scene_with(snapshot, &BTreeSet::new(), &[])
}

pub(super) fn ant<'a>(scene: &'a Scene, name: &str) -> &'a Ant {
    scene
        .ant(&id(name))
        .unwrap_or_else(|| panic!("{name} is drawn"))
}

fn tunnel_of(scene: &Scene, name: &str) -> usize {
    match ant(scene, name).spot {
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
    let king = ant(&scene, "queen");
    assert_eq!(king.size, ant(&scene, "working").size);
    assert_eq!(
        crate::ui::dwarf::Role::King.scale(),
        crate::ui::dwarf::Role::Miner.scale()
    );
    let foot = king.pos.y + king.size / 2.0;
    assert!((scene.queen_chamber.bottom() - foot - 0.1 * king.size).abs() < 0.01);
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
            assert_eq!(home(kind, Done, pr), Storage, "{kind:?}");
            for state in [Working, Stalled, Failed] {
                assert_eq!(home(kind, state, pr), post, "{kind:?} {state:?}");
            }
        }
        assert_eq!(home(kind, Idle, false), post, "{kind:?}");
        assert_eq!(
            home(kind, Idle, true),
            Storage,
            "idle with a PR waits in storage"
        );
    }
    for state in [Working, Idle, NeedsInput, Stalled, Done, Failed] {
        assert_eq!(home(NodeKind::Queen, state, false), Queen);
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
    assert_eq!(worker(Done), Pose::Carry);
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
    assert_eq!(mark(Done, false), None);
    assert_eq!(mark(Done, true), Some(Mark::PrReady));
    assert_eq!(mark(Idle, true), Some(Mark::PrReady));
    assert_eq!(mark(Working, false), None);
}

#[test]
fn ants_go_where_their_state_says() {
    let scene = scene(&snapshot());
    let t = tunnel_of(&scene, "r1-coord");
    let tunnel = &scene.tunnels[t];
    assert_eq!(tunnel.key, TunnelKey::Run("r1".into()));

    let queen = ant(&scene, "queen");
    assert_eq!(queen.spot, Spot::Queen);
    assert!(scene.queen_chamber.contains(queen.pos));

    let coord = ant(&scene, "r1-coord");
    assert!(matches!(coord.spot, Spot::Mouth(..)));
    assert_eq!(coord.tag, None, "its tunnel's label names it");
    assert_eq!(ant(&scene, "working").tag.as_deref(), Some("working"));
    assert!(
        (coord.pos.x - tunnel.shaft.center().x).abs() < 1.0,
        "at the mouth"
    );
    assert!(coord.pos.y < tunnel.chamber.top(), "above the chamber");

    for name in ["working", "idle", "stalled", "failed"] {
        let a = ant(&scene, name);
        assert!(matches!(a.spot, Spot::Chamber(..)), "{name}");
        assert_eq!(tunnel_of(&scene, name), t, "{name}");
        assert!(tunnel.chamber.contains(a.pos), "{name} in the chamber");
    }
    assert!(
        ant(&scene, "working").facing_right,
        "diggers face the dig face"
    );
    let (working, idle) = (ant(&scene, "working").pos, ant(&scene, "idle").pos);
    assert!(
        working.x > ant(&scene, "stalled").pos.x && working.x >= idle.x,
        "diggers at the face, sleepers behind them"
    );

    let waiting = ant(&scene, "waiting");
    assert_eq!(waiting.spot, Spot::Landing(0));
    assert!(waiting.pos.y < scene.surface_y, "on the surface");
    assert!(scene.landing.contains(waiting.pos));

    let done = ant(&scene, "done");
    assert_eq!(done.spot, Spot::Storage(0));
    assert!(scene.storage.contains(done.pos));
    assert_eq!(done.pose, Pose::Carry);
}

#[test]
fn everyone_waiting_on_karl_shares_the_landing() {
    let mut s = FleetSnapshot::default();
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
        ant(&scene, "a-coord").tag.as_deref(),
        Some("Objective a"),
        "a coordinator away from its tunnel is named by its run"
    );
    assert_eq!(ant(&scene, "b1").tag.as_deref(), Some("b1"));
    let spots: Vec<Spot> = ["a-coord", "b1", "t"]
        .iter()
        .map(|n| ant(&scene, n).spot.clone())
        .collect();
    assert_eq!(
        spots,
        [Spot::Landing(0), Spot::Landing(1), Spot::Landing(2)]
    );
    for n in ["a-coord", "b1", "t"] {
        assert!(scene.landing.contains(ant(&scene, n).pos), "{n}");
    }
}

#[test]
fn pr_ready_ants_mark_and_pile_in_storage() {
    let scene = scene_with(&snapshot(), &BTreeSet::new(), &["done", "idle"]);
    assert_eq!(ant(&scene, "done").mark, Some(Mark::PrReady));
    let idle = ant(&scene, "idle");
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
    let mut s = FleetSnapshot::default();
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
    let xs: Vec<f32> = scene.tunnels.iter().map(|t| t.shaft.center().x).collect();
    assert!(xs.windows(2).all(|w| w[0] < w[1]), "left to right: {xs:?}");
}

#[test]
fn past_dispatches_without_a_terminal_are_not_drawn() {
    let mut s = FleetSnapshot::default();
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
        scene.ant(&id("old-done")).is_none(),
        "no done without a terminal"
    );
    assert!(scene.ant(&id("superseded")).is_none());
    assert_eq!(
        ant(&scene, "lost").pose,
        Pose::Failed,
        "an open dispatch that lost its terminal stays, failed"
    );
    assert!(
        scene.ant(&id("released")).is_some(),
        "released ones fade out"
    );
}

#[test]
fn a_finished_run_without_workers_has_no_tunnel() {
    let mut s = FleetSnapshot::default();
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
    let c = ant(&scene, "over-coord");
    assert!(matches!(c.spot, Spot::Chamber(TunnelKey::Repo(ref r), _) if r == "alpha"));
    assert_eq!(c.tag.as_deref(), Some("Other work"));
}

#[test]
fn tunnels_deepen_with_progress() {
    let mut s = FleetSnapshot::default();
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
    assert!(late.chamber.top() > early.chamber.top() + 50.0);
    assert!(late.chamber.bottom() <= rect.bottom());
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
    let (parent, sub) = (ant(&scene, "idle"), ant(&scene, "sub"));
    assert_eq!(sub.spot, Spot::Trail(id("idle"), 0));
    assert!(sub.size < parent.size, "smaller than its parent");
    assert!(sub.pos.x > parent.pos.x, "behind a left-facing parent");
    assert!((sub.pos - parent.pos).length() < 2.0 * parent.size);
    let parent_at = scene.ants.iter().position(|a| a.id == id("idle"));
    let sub_at = scene.ants.iter().position(|a| a.id == id("sub"));
    assert!(parent_at < sub_at, "drawn after its parent");
    assert!(matches!(ant(&scene, "orphan").spot, Spot::Chamber(..)));
}

#[test]
fn hidden_nodes_are_not_drawn() {
    let hidden: BTreeSet<NodeId> = [id("idle"), id("loose")].into();
    let scene = scene_with(&snapshot(), &hidden, &[]);
    assert!(scene.ant(&id("idle")).is_none());
    assert!(
        scene
            .tunnels
            .iter()
            .all(|t| matches!(t.key, TunnelKey::Run(_))),
        "the hidden terminal's tunnel goes too"
    );
}

/// About 30 agents in 6 runs plus loose terminals all fit, without ants on top of each other.
#[test]
fn a_big_fleet_fits_without_overlap() {
    let mut s = FleetSnapshot::default();
    s.nodes
        .push(node("queen", NodeKind::Queen, None, NodeState::Working));
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
    let mut s = FleetSnapshot::default();
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
            NodeKind::Worker,
            Some("r"),
            NodeState::Done,
        ));
    }
    let scene = assert_fits(&s);
    for a in &scene.ants {
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

/// Lays `s` out and checks every ant is drawn inside the view and none sit on each other.
fn assert_fits(s: &FleetSnapshot) -> Scene {
    let scene = scene(s);
    assert_eq!(scene.ants.len(), s.nodes.len());
    for a in &scene.ants {
        let cell = Rect::from_center_size(a.pos, Vec2::splat(a.size));
        assert!(
            RECT.expand(1.0).contains_rect(cell),
            "{} at {:?}",
            a.id,
            a.pos
        );
    }
    for (i, a) in scene.ants.iter().enumerate() {
        for b in &scene.ants[i + 1..] {
            let d = (a.pos - b.pos).abs();
            let apart = (a.size + b.size) / 2.0 - 1.0;
            assert!(d.x >= apart || d.y >= apart, "{} overlaps {}", a.id, b.id);
        }
    }
    scene
}

#[test]
fn cart_routes_use_the_chamber_shaft_and_entrance() {
    let scene = scene(&snapshot());
    let worker = ant(&scene, "working");
    let t = &scene.tunnels[tunnel_of(&scene, "working")];
    let to = pos2(
        scene.landing.center().x,
        scene.surface_y - scene.cell() / 2.0,
    );
    let path = route(worker.pos, to, &worker.spot, &Spot::Landing(0), &scene);
    assert_eq!(along(&path, 0.0).0, worker.pos);
    assert_eq!(along(&path, 1.0).0, to);
    assert!(path.contains(&pos2(t.shaft.center().x, scene.gallery.center().y)));
    assert!(path.contains(&pos2(scene.entrance_x(), scene.gallery.center().y)));
    for leg in path.windows(2) {
        assert!(
            leg[0].x == leg[1].x || leg[0].y == leg[1].y,
            "axis-aligned rails and hoists"
        );
        let mid = leg[0].lerp(leg[1], 0.5);
        if mid.y > scene.gallery.bottom() && mid.y < t.chamber.top() {
            assert!(
                t.shaft.contains(mid),
                "cart must stay in the excavated shaft"
            );
        }
    }
    let back = route(to, worker.pos, &Spot::Landing(0), &worker.spot, &scene);
    assert_eq!(back, path.iter().copied().rev().collect::<Vec<_>>());
}

#[test]
fn dwarves_ride_only_during_travel_and_arrive_in_their_work_pose() {
    use crate::ui::dwarf::{Action, pose};
    let scene = scene(&snapshot());
    let worker = ant(&scene, "working");
    assert_eq!(
        super::dwarf_action(worker, true, false, false),
        Action::Ride
    );
    assert_eq!(super::dwarf_action(worker, true, true, false), Action::Ride);
    assert_eq!(
        super::dwarf_action(worker, false, false, false),
        Action::Mine
    );
    let seated = pose(Action::Ride, 0.4);
    assert!(seated.stowed);
    assert_eq!(seated.stride, 0.0, "feet stay inside the cart");
    assert!(!seated.nugget, "cargo goes in the cart");
}

#[test]
fn a_state_change_walks_the_ant_over() {
    let mut farm = AntFarmView::new();
    let mut s = snapshot();
    let before = scene(&s);
    farm.advance(&before, 0.0);
    let start = ant(&before, "idle").pos;

    s.nodes
        .iter_mut()
        .find(|n| n.id == id("idle"))
        .unwrap()
        .state = NodeState::NeedsInput;
    let after = scene(&s);
    let target = ant(&after, "idle").pos;
    let at = |farm: &mut AntFarmView, t: f64| {
        let placed = farm.advance(&after, t);
        let i = after.ants.iter().position(|a| a.id == id("idle")).unwrap();
        placed[i]
    };
    let first = at(&mut farm, 10.0);
    assert_eq!(first.pos, start, "sets off from where it was");
    assert!(first.heading.is_some());
    let mid = at(&mut farm, 10.4);
    assert!(mid.pos != start && mid.pos != target, "on its way");
    let end = at(&mut farm, 20.0);
    assert_eq!(end.pos, target);
    assert_eq!(end.heading, None, "arrived");
    // Unchanged ants never walked.
    let i = after
        .ants
        .iter()
        .position(|a| a.id == id("working"))
        .unwrap();
    assert_eq!(farm.advance(&after, 20.0)[i].heading, None);
}

#[test]
fn a_resize_moves_ants_without_walking() {
    let mut farm = AntFarmView::new();
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

/// Clicking an ant reports it, as clicking a node does in the fleet; clicking soil reports
/// an empty click.
#[test]
fn clicking_an_ant_reports_it() {
    use eframe::egui::{self, Event, PointerButton};
    let s = snapshot();
    let mut fleet = crate::ui::fleet_view::FleetView::new();
    fleet.sync(&s);
    let mut farm = AntFarmView::new();
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |events: Vec<Event>, farm: &mut AntFarmView| {
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
            out = Some((rect, farm.show(ui, &s, &mut fleet)));
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
    let target = ant(&scene, "working").pos;
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

/// Dragging a box from empty ground over a chamber selects the ants in it, as in the fleet.
#[test]
fn dragging_a_box_selects_the_ants_in_it() {
    use eframe::egui::{self, Event, PointerButton};
    let s = snapshot();
    let mut fleet = crate::ui::fleet_view::FleetView::new();
    fleet.sync(&s);
    let mut farm = AntFarmView::new();
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |events: Vec<Event>, farm: &mut AntFarmView| {
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
            out = Some((rect, farm.show(ui, &s, &mut fleet)));
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
    let expected: BTreeSet<NodeId> = ["working", "idle", "stalled", "failed"]
        .iter()
        .map(|n| id(n))
        .collect();
    assert_eq!(hits, expected);
}

/// A broadcast grows from the Queen to the farthest corner, a status report closes in from
/// there to her, and both are gone after; an ant hops only while the ring is on it.
#[test]
fn ripples_spread_and_ants_hop_as_they_pass() {
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

/// Ants present when the farm opens are placed; one that appears later walks out of the
/// Queen's chamber.
#[test]
fn new_ants_emerge_from_the_queen() {
    let mut farm = AntFarmView::new();
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
        .ants
        .iter()
        .position(|a| a.id == id("hatchling"))
        .unwrap();
    let placed = farm.advance(&after, 1.0);
    assert!(placed[i].heading.is_some(), "walks");
    assert!(
        after
            .queen_chamber
            .expand(after.cell())
            .contains(placed[i].pos),
        "from the Queen's chamber"
    );
    assert_eq!(
        farm.advance(&after, 10.0)[i].pos,
        ant(&after, "hatchling").pos
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
    let pose_of = |name: &str| scene.ant(&id(name)).unwrap().pose;
    assert_eq!(pose_of("working"), Pose::Build);
    assert!(scene.ant(&id("working")).unwrap().facing_right);
    assert_eq!(pose_of("idle"), Pose::Sleep, "only working ants build");
    assert_eq!(
        pose_of("r1-coord"),
        Pose::Stand,
        "coordinators keep standing"
    );
}

/// An ant removed from the fleet falls; one filtered out or changing state does not.
#[test]
fn only_removed_ants_fall() {
    let mut farm = AntFarmView::new();
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
        .ants
        .iter()
        .position(|a| a.id == id("waiting"))
        .unwrap();
    assert!(farm.falls.is_empty(), "done and filtered ants stay alive");

    s.nodes
        .retain(|n| n.id != id("waiting") && n.id != id("idle"));
    let after = scene_with(&s, &hidden, &[]);
    farm.bury(&after, &s, 2.0, || false);
    farm.advance(&after, 2.0);
    assert_eq!(
        farm.falls.len(),
        1,
        "only the removed ant, not the filtered one"
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

/// A falling ant hops, flips legs up, lands on the farm's bottom after FALL_S, bounces and
/// fades; with reduced motion it fades where it stood.
#[test]
fn a_dead_ant_falls_to_the_bottom() {
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

/// A crowded chamber in a narrow tunnel packs its ants side by side, spread across the
/// chamber, rather than one per row down a tall farm.
#[test]
fn a_crowded_chamber_packs_side_by_side() {
    let mut s = FleetSnapshot::default();
    s.nodes
        .push(node("queen", NodeKind::Queen, None, NodeState::Working));
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
    let ants: Vec<&Ant> = scene
        .ants
        .iter()
        .filter(|a| a.id.as_str().starts_with("r0-"))
        .collect();
    let mut rows: Vec<f32> = ants.iter().map(|a| a.pos.y).collect();
    rows.sort_by(f32::total_cmp);
    rows.dedup();
    let widest = rows
        .iter()
        .map(|y| ants.iter().filter(|a| a.pos.y == *y).count())
        .max()
        .unwrap();
    assert!(widest >= 2, "{} rows of up to {widest}", rows.len());
    for ant in ants {
        // The name tag is two cells wide.
        let tag = Rect::from_center_size(ant.pos, vec2(2.0 * ant.size, ant.size));
        assert!(
            tag.left() >= chamber.left() - 0.5 && tag.right() <= chamber.right() + 0.5,
            "{} stays in its chamber",
            ant.id.as_str()
        );
    }
}

/// A chamber ant's subagents, nested ones too, line up behind it in room it reserves:
/// they never cover another ant or each other.
#[test]
fn subagent_trails_take_room_in_the_chamber() {
    let mut s = FleetSnapshot::default();
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
    let nested = ant(&scene, "s4");
    assert!(matches!(&nested.spot, Spot::Trail(root, _) if *root == id("a")));
    let boxes: Vec<(&str, Rect)> = scene
        .ants
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
    for a in &scene.ants {
        assert!(chamber.expand(1.0).contains(a.pos), "{} outside", a.id.0);
    }
}

#[test]
fn parked_ants_sleep_in_the_resting_area() {
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
        .ants
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
    let sleepers: Vec<&Ant> = scene
        .ants
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

/// Many runs in a tall view stack their chambers two to a column, which draws the ants
/// bigger than one long row of narrow columns, and no shaft cuts through another chamber.
#[test]
fn many_tunnels_stack_chambers_to_draw_ants_bigger() {
    let mut s = FleetSnapshot::default();
    s.nodes
        .push(node("queen", NodeKind::Queen, None, NodeState::Working));
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
    // A tall, narrow view: ten rooms side by side would draw the ants at scale 1.
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
                !t.shaft.intersects(u.chamber),
                "shaft {i} crosses chamber {j}"
            );
        }
    }
    for a in scene.ants.iter().filter(|a| a.kind == NodeKind::Worker) {
        let t = &tunnels[tunnel_of(&scene, a.id.as_str())];
        assert!(t.chamber.contains(a.pos), "{} in its chamber", a.id);
    }
}

#[test]
fn envoy_stays_in_its_comms_room_without_a_work_tunnel() {
    let mut snapshot = FleetSnapshot::default();
    snapshot
        .nodes
        .push(node("queen", NodeKind::Queen, None, NodeState::Idle));
    snapshot
        .nodes
        .push(node("envoy", NodeKind::Envoy, None, NodeState::Idle));
    for state in [NodeState::Idle, NodeState::Working, NodeState::NeedsInput] {
        snapshot.nodes[1].state = state;
        let scene = scene(&snapshot);
        let envoy = ant(&scene, "envoy");
        assert_eq!(envoy.spot, Spot::Comms);
        assert_eq!(envoy.pose, Pose::Stand);
        assert_eq!(envoy.mark, None);
        assert!(scene.comms.unwrap().chamber.contains(envoy.pos));
        assert!(envoy.pos.x < scene.queen_chamber.left());
        assert!(scene.tunnels.is_empty());
    }
}

#[test]
fn comms_room_fits_beside_entrance_at_small_and_large_fleets() {
    for size in [
        vec2(176.0, 400.0),
        vec2(320.0, 400.0),
        vec2(640.0, 400.0),
        vec2(1280.0, 800.0),
        vec2(2560.0, 1600.0),
    ] {
        for count in [0, 1, 8, 40, 100] {
            let mut snapshot = FleetSnapshot::default();
            snapshot
                .nodes
                .push(node("queen", NodeKind::Queen, None, NodeState::Idle));
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
            assert!(room.chamber.width() >= scene.queen_chamber.width());
            assert!(room.chamber.height() >= scene.queen_chamber.height());
            assert!(scene.landing.left() - room.chamber.right() >= scene.cell());
            assert!(room.feed().y > scene.rect.top());
            assert!(!room.chamber.intersects(scene.queen_chamber));
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
                assert!(!room.chamber.intersects(tunnel.shaft));
            }
            assert!(room.shaft.intersects(scene.gallery));
            assert!(room.shaft.intersects(room.chamber));
            assert_eq!(
                room.receiver,
                pos2(room.chamber.center().x, scene.surface_y)
            );
            let envoy = ant(&scene, "envoy");
            let ant_cell = Rect::from_center_size(envoy.pos, Vec2::splat(envoy.size));
            let basin = room.basin_rect();
            let tray = room.tray_rect();
            assert!(room.chamber.contains_rect(tray));
            assert!(room.chamber.contains_rect(basin));
            assert!(!tray.intersects(basin));
            assert!(!tray.intersects(ant_cell));
            assert!(!tray.intersects(scene.queen_chamber));
            assert!(basin.center().x >= ant_cell.right());
            assert!(basin.top() > ant_cell.top(), "rim below the priest's head");
            assert!(
                basin.top() < ant_cell.bottom(),
                "hands can reach across the rim"
            );
            assert_eq!(room.shaft.center().x, envoy.pos.x);
            assert!(!room.shaft.intersects(scene.queen_chamber));
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
fn waiting_ants_keep_clear_of_the_comms_room() {
    for (size, count) in [(vec2(640.0, 800.0), 8), (vec2(1280.0, 800.0), 24)] {
        let mut snapshot = FleetSnapshot::default();
        snapshot
            .nodes
            .push(node("queen", NodeKind::Queen, None, NodeState::Idle));
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
        for ant in scene
            .ants
            .iter()
            .filter(|ant| matches!(ant.spot, Spot::Landing(_)))
        {
            let cell = Rect::from_center_size(ant.pos, Vec2::splat(ant.size));
            assert!(scene.landing.contains_rect(cell));
            assert!(scene.rect.contains_rect(cell));
            assert!(cell.left() - room.chamber.right() >= scene.cell());
        }
    }
}

/// The Envoy, the Queen and `n` agents: runs of a coordinator and up to four workers, every
/// third worker idle, every fifth waiting on Karl, every seventh done.
pub(super) fn typical_fleet(n: usize) -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    s.nodes
        .push(node("queen", NodeKind::Queen, None, NodeState::Working));
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

/// Share of the ground under the gallery that rooms use: each chamber with the margin
/// around it, from its label down.
pub(super) fn fill(scene: &Scene) -> f32 {
    let a = scene.cell();
    let rooms: f32 = scene
        .tunnels
        .iter()
        .map(|t| (t.chamber.width() + 0.6 * a) * (t.chamber.bottom() - t.label_top.y))
        .sum();
    rooms / (scene.rect.width() * (scene.rect.bottom() - scene.gallery.bottom()))
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

/// At typical fleet sizes the rooms use most of the ground under the gallery, with ants
/// drawn bigger than a fixed grid drew them, and every ant in its chamber.
#[test]
fn the_farm_fills_the_view() {
    // (view, ants, smallest ant cell in points, least fill)
    let cases = [
        (vec2(1280.0, 800.0), 5, 56.0, 0.75),
        (vec2(1280.0, 800.0), 15, 48.0, 0.75),
        (vec2(1280.0, 800.0), 30, 40.0, 0.75),
        (vec2(900.0, 650.0), 5, 48.0, 0.75),
        (vec2(900.0, 650.0), 15, 32.0, 0.75),
        (vec2(900.0, 650.0), 30, 24.0, 0.7),
    ];
    for (size, n, cell, least) in cases {
        let s = typical_fleet(n);
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
            "{n} ants in {size:?}: fill {fill}"
        );
        assert!(reach(&scene) > 0.85, "{n} ants in {size:?} reach down");
        assert!(
            scene.cell() >= cell,
            "{n} ants in {size:?}: {}",
            scene.cell()
        );
        for (i, t) in scene.tunnels.iter().enumerate() {
            assert!(rect.contains_rect(t.chamber), "chamber {i} in the view");
            for u in &scene.tunnels[i + 1..] {
                assert!(!t.chamber.intersects(u.chamber), "chambers overlap");
            }
        }
        for a in scene.ants.iter().filter(|a| a.kind == NodeKind::Worker) {
            if let Spot::Chamber(..) = a.spot {
                let t = &scene.tunnels[tunnel_of(&scene, a.id.as_str())];
                assert!(t.chamber.contains(a.pos), "{} in its chamber", a.id);
            }
        }
    }
}

/// A near-empty farm in a big view draws its ants no bigger than the largest scale.
#[test]
fn a_lone_ant_is_not_drawn_giant() {
    let s = typical_fleet(2);
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
    let mut ant = scene(&snapshot())
        .ants
        .into_iter()
        .find(|a| a.kind != NodeKind::Queen)
        .unwrap();
    ant.kind = NodeKind::Envoy;
    for state in [
        NodeState::Working,
        NodeState::Idle,
        NodeState::NeedsInput,
        NodeState::Done,
        NodeState::Failed,
        NodeState::Stalled,
    ] {
        ant.state = state;
        let action = super::dwarf_action(&ant, false, false, false);
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
        assert_eq!(super::dwarf_look(&ant).role, Role::RunePriest);
    }
}

#[test]
fn stalled_dwarf_holds_still_in_a_helmet_for_the_pebble() {
    use crate::ui::dwarf::{Action, Hat};
    let scene = scene(&snapshot());
    let stalled = ant(&scene, "stalled");
    assert_eq!(
        super::dwarf_action(stalled, false, false, false),
        Action::Wait
    );
    assert_eq!(super::dwarf_look(stalled).hat, Hat::Helmet);
}

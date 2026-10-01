use std::collections::{BTreeSet, HashSet};

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

use super::AntFarmView;
use super::layout::*;
use super::motion::{along, route};
use crate::model::{
    Edge, EdgeKind, FleetSnapshot, Node, NodeId, NodeKind, NodeState, RunSummary, TaskCounts,
};

const RECT: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1280.0, 800.0));

fn id(s: &str) -> NodeId {
    NodeId(s.to_string())
}

fn node(name: &str, kind: NodeKind, run: Option<&str>, state: NodeState) -> Node {
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
    }
}

fn run(name: &str, completed: usize, total: usize) -> RunSummary {
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
fn snapshot() -> FleetSnapshot {
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
        now_ms: 0,
    };
    layout(&input, RECT)
}

fn scene(snapshot: &FleetSnapshot) -> Scene {
    scene_with(snapshot, &BTreeSet::new(), &[])
}

fn ant<'a>(scene: &'a Scene, name: &str) -> &'a Ant {
    scene
        .ant(&id(name))
        .unwrap_or_else(|| panic!("{name} is drawn"))
}

fn tunnel_of(scene: &Scene, name: &str) -> usize {
    match ant(scene, name).spot {
        Spot::Mouth(t, _) | Spot::Chamber(t, _) => t,
        ref other => panic!("{name} is not in a tunnel: {other:?}"),
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
        Some(Mark::Whip),
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
        assert!(matches!(a.spot, Spot::Chamber(tt, _) if tt == t), "{name}");
        assert!(tunnel.chamber.contains(a.pos), "{name} in the chamber");
    }
    assert!(
        ant(&scene, "working").facing_right,
        "diggers face the dig face"
    );
    assert!(
        ant(&scene, "working").pos.x > ant(&scene, "idle").pos.x,
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
    let scene = scene(&s);
    let (early, late) = (&scene.tunnels[0], &scene.tunnels[1]);
    assert_eq!(early.progress, Some(0.0));
    assert_eq!(late.progress, Some(1.0));
    assert_eq!(early.tasks, Some((0, 4)));
    assert_eq!(late.tasks, Some((4, 4)));
    assert!(late.chamber.top() > early.chamber.top() + 50.0);
    assert!(late.chamber.bottom() <= RECT.bottom());
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
fn routes_go_by_the_gallery_between_areas() {
    let (from, to) = (pos2(10.0, 300.0), pos2(200.0, 50.0));
    assert_eq!(route(from, to, true, 120.0), [from, to]);
    assert_eq!(
        route(from, to, false, 120.0),
        [from, pos2(10.0, 120.0), pos2(200.0, 120.0), to]
    );
    let path = route(from, to, false, 120.0);
    assert_eq!(along(&path, 0.0).0, from);
    assert_eq!(along(&path, 1.0).0, to);
    // The first leg is 180 of 440 points: half way is on the gallery, heading right.
    let (mid, heading) = along(&path, 0.5);
    assert!((mid.y - 120.0).abs() < 0.01);
    assert!(heading.x > 0.0);
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
        now_ms: 0,
    };
    let smaller = layout(&input, Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0)));
    assert!(
        farm.advance(&smaller, 1.0)
            .iter()
            .all(|p| p.heading.is_none())
    );
}

/// Clicking an ant reports it, as clicking a node does in the graph; clicking soil reports
/// an empty click.
#[test]
fn clicking_an_ant_reports_it() {
    use eframe::egui::{self, Event, PointerButton};
    let s = snapshot();
    let mut graph = crate::ui::graph::GraphView::new();
    graph.sync(&s);
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
            out = Some((rect, farm.show(ui, &s, &graph)));
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

/// Dragging a box from empty ground over a chamber selects the ants in it, as in the graph.
#[test]
fn dragging_a_box_selects_the_ants_in_it() {
    use eframe::egui::{self, Event, PointerButton};
    let s = snapshot();
    let mut graph = crate::ui::graph::GraphView::new();
    graph.sync(&s);
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
            out = Some((rect, farm.show(ui, &s, &graph)));
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

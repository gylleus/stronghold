use std::collections::{BTreeSet, HashSet};

use eframe::egui::{Pos2, Rect, vec2};

use super::layout::*;
use super::layout_tests::{id, node, run};
use super::rooms::{Change, Demand, GRACE_MS, RoomPlan, spare_cells};
use super::{AntFarmView, GLIDE_S};
use crate::model::{FleetSnapshot, NodeKind, NodeState};

const RECT: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1280.0, 800.0));

/// The Queen and `runs` runs of `workers` working workers each.
fn fleet(runs: usize, workers: usize) -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    s.nodes
        .push(node("queen", NodeKind::Queen, None, NodeState::Working));
    for r in 0..runs {
        add_run(&mut s, r, workers);
    }
    s
}

fn add_run(s: &mut FleetSnapshot, r: usize, workers: usize) {
    let name = format!("r{r}");
    s.runs.push(run(&name, 0, 0));
    for w in 0..workers {
        add_worker(s, r, w);
    }
}

fn add_worker(s: &mut FleetSnapshot, r: usize, w: usize) {
    s.nodes.push(node(
        &format!("r{r}-w{w}"),
        NodeKind::Worker,
        Some(&format!("r{r}")),
        NodeState::Working,
    ));
}

fn drop_run(s: &mut FleetSnapshot, r: usize) {
    let name = format!("r{r}");
    s.nodes.retain(|n| n.run_id.as_deref() != Some(&name));
    s.runs.retain(|run| run.id != name);
}

fn frame(s: &FleetSnapshot, plan: &mut RoomPlan, now_ms: i64) -> Scene {
    frame_in(s, plan, now_ms, RECT)
}

fn frame_in(s: &FleetSnapshot, plan: &mut RoomPlan, now_ms: i64, rect: Rect) -> Scene {
    let input = FarmInput {
        snapshot: s,
        hidden: &BTreeSet::new(),
        pr_ready: &HashSet::new(),
        building: &HashSet::new(),
        now_ms,
    };
    layout_in(&input, rect, plan)
}

fn tunnel(scene: &Scene, r: usize) -> &Tunnel {
    let key = TunnelKey::Run(format!("r{r}"));
    scene
        .tunnels
        .iter()
        .find(|t| t.key == key)
        .unwrap_or_else(|| panic!("r{r} has a room"))
}

/// Every room of `before` is where it was in `after`.
fn assert_kept(before: &Scene, after: &Scene) {
    for t in &before.tunnels {
        let u = after
            .tunnels
            .iter()
            .find(|u| u.key == t.key)
            .unwrap_or_else(|| panic!("{:?} kept", t.key));
        assert_eq!(t.chamber, u.chamber, "{:?} chamber moved", t.key);
        assert_eq!(t.shaft, u.shaft, "{:?} shaft moved", t.key);
        assert_eq!(t.label_top, u.label_top, "{:?} label moved", t.key);
    }
    assert_eq!(before.px, after.px, "the scale is kept");
}

#[test]
fn a_new_room_is_dug_beside_the_others_and_moves_none() {
    let mut s = fleet(3, 2);
    let mut plan = RoomPlan::default();
    let before = frame(&s, &mut plan, 0);
    assert_eq!(plan.rearranges, 1, "the first frame lays the plan out");
    add_run(&mut s, 3, 2);
    let after = frame(&s, &mut plan, 1_000);
    assert_eq!(plan.rearranges, 1, "dug into a spare cell");
    assert_kept(&before, &after);
    let new = tunnel(&after, 3);
    for t in &before.tunnels {
        assert!(!new.chamber.intersects(t.chamber), "{:?}", t.key);
        assert!(!new.shaft.intersects(t.chamber), "{:?}", t.key);
    }
    for w in 0..2 {
        let a = after.ant(&id(&format!("r3-w{w}"))).expect("drawn");
        assert!(new.chamber.contains(a.pos));
    }
}

#[test]
fn an_ant_within_the_room_s_capacity_moves_no_room() {
    let mut s = fleet(3, 2);
    let mut plan = RoomPlan::default();
    let before = frame(&s, &mut plan, 0);
    let rows = plan.room(&TunnelKey::Run("r1".into())).unwrap().rows;
    add_worker(&mut s, 1, 2);
    let after = frame(&s, &mut plan, 1_000);
    assert_eq!(plan.room(&TunnelKey::Run("r1".into())).unwrap().rows, rows);
    assert_kept(&before, &after);
    assert_eq!(plan.rearranges, 1);
    // The others' ants did not move either.
    for a in before.ants.iter().filter(|a| a.kind == NodeKind::Worker) {
        assert_eq!(after.ant(&a.id).unwrap().pos, a.pos, "{}", a.id);
    }
}

#[test]
fn a_full_room_grows_without_moving_its_neighbours() {
    let mut s = fleet(3, 1);
    let mut plan = RoomPlan::default();
    let before = frame(&s, &mut plan, 0);
    let grid = plan.grid().unwrap();
    assert_eq!(grid.levels, 1, "{grid:?}");
    // The last room has a free cell on its right.
    let key = TunnelKey::Run("r2".into());
    let start = plan.room(&key).unwrap().clone();
    let mut w = 1;
    let mut deeper = false;
    while plan.room(&key).unwrap().span == start.span {
        add_worker(&mut s, 2, w);
        w += 1;
        let scene = frame(&s, &mut plan, w as i64);
        assert_eq!(plan.rearranges, 1, "no rearrange with {w} ants in r2");
        deeper |= plan.room(&key).unwrap().rows > start.rows;
        for t in before.tunnels.iter().filter(|t| t.key != key) {
            let u = scene.tunnels.iter().find(|u| u.key == t.key).unwrap();
            assert_eq!(
                t.chamber, u.chamber,
                "{:?} moved with {w} ants in r2",
                t.key
            );
        }
        let grown = tunnel(&scene, 2);
        for a in scene
            .ants
            .iter()
            .filter(|a| a.id.as_str().starts_with("r2-"))
        {
            assert!(grown.chamber.contains(a.pos), "{} in r2", a.id);
        }
    }
    assert!(deeper, "dug deeper first");
    assert_eq!(plan.room(&key).unwrap().span, 2, "then wider");
}

#[test]
fn rearranges_only_when_no_cell_is_free() {
    let mut s = fleet(1, 1);
    let mut plan = RoomPlan::default();
    frame(&s, &mut plan, 0);
    let grid = plan.grid().unwrap();
    let free = grid.cols * grid.levels - plan.rooms.len();
    assert!(free >= 1, "spare cells");
    let mut r = 1;
    for _ in 0..free {
        add_run(&mut s, r, 1);
        r += 1;
        frame(&s, &mut plan, r as i64);
    }
    assert_eq!(plan.rearranges, 1, "the spare cells took the new rooms");
    add_run(&mut s, r, 1);
    let mut probe = plan.clone();
    let demand = Demand {
        rooms: (0..=r)
            .map(|i| (TunnelKey::Run(format!("r{i}")), vec![1]))
            .collect(),
        ..Demand::default()
    };
    assert_eq!(probe.update(&demand, RECT.size(), 100), Change::Rearranged);
    frame(&s, &mut plan, 100);
    assert_eq!(plan.rearranges, 2, "no room left: one rearrange");
    let grid = plan.grid().unwrap();
    assert!(
        grid.cols * grid.levels - plan.rooms.len() >= spare_cells(plan.rooms.len()).min(1),
        "with spare cells again"
    );
}

#[test]
fn an_emptied_room_waits_out_its_grace() {
    let mut s = fleet(3, 2);
    let mut plan = RoomPlan::default();
    let before = frame(&s, &mut plan, 0);
    drop_run(&mut s, 1);
    let empty = frame(&s, &mut plan, 1_000);
    assert_kept(&before, &empty);
    assert!(
        empty
            .tunnels
            .iter()
            .any(|t| t.key == TunnelKey::Run("r1".into()))
    );
    // Its ants come back within the grace: the same room.
    add_run(&mut s, 1, 2);
    let back = frame(&s, &mut plan, 30_000);
    assert_kept(&before, &back);
    drop_run(&mut s, 1);
    frame(&s, &mut plan, 40_000);
    let gone = frame(&s, &mut plan, 40_000 + GRACE_MS);
    assert!(
        !gone
            .tunnels
            .iter()
            .any(|t| t.key == TunnelKey::Run("r1".into()))
    );
    for r in [0, 2] {
        assert_eq!(tunnel(&gone, r).chamber, tunnel(&before, r).chamber);
    }
    assert_eq!(plan.rearranges, 1);
}

#[test]
fn a_mostly_empty_grid_is_compacted() {
    let mut s = fleet(8, 1);
    let mut plan = RoomPlan::default();
    frame(&s, &mut plan, 0);
    for r in 0..6 {
        drop_run(&mut s, r);
    }
    frame(&s, &mut plan, 1_000);
    assert_eq!(plan.rearranges, 1, "rooms wait out their grace first");
    let after = frame(&s, &mut plan, 1_000 + GRACE_MS);
    assert_eq!(plan.rearranges, 2, "then the grid is laid out again");
    assert_eq!(after.tunnels.len(), 2);
}

#[test]
fn the_same_history_lays_out_the_same() {
    let history = |plan: &mut RoomPlan| {
        let mut s = fleet(2, 1);
        let mut scenes = vec![frame(&s, plan, 0)];
        add_run(&mut s, 2, 3);
        scenes.push(frame(&s, plan, 10));
        for w in 1..9 {
            add_worker(&mut s, 0, w);
            scenes.push(frame(&s, plan, 20 + w as i64));
        }
        drop_run(&mut s, 1);
        scenes.push(frame(&s, plan, 100));
        add_run(&mut s, 3, 1);
        scenes.push(frame(&s, plan, 200 + GRACE_MS));
        scenes
    };
    let (mut a, mut b) = (RoomPlan::default(), RoomPlan::default());
    assert_eq!(history(&mut a), history(&mut b));
    assert_eq!(a, b);
    // A plan saved and restored carries on alike.
    let saved: RoomPlan = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
    let s = fleet(4, 1);
    assert_eq!(
        frame(&s, &mut a.clone(), 1_000_000),
        frame(&s, &mut saved.clone(), 1_000_000)
    );
}

#[test]
fn a_resize_keeps_the_rooms_in_their_cells() {
    let s = fleet(4, 2);
    let mut plan = RoomPlan::default();
    frame(&s, &mut plan, 0);
    let cells: Vec<_> = plan.rooms.iter().map(|r| (r.level, r.col)).collect();
    frame_in(
        &s,
        &mut plan,
        0,
        Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0)),
    );
    let after: Vec<_> = plan.rooms.iter().map(|r| (r.level, r.col)).collect();
    assert_eq!(cells, after);
    assert_eq!(plan.rearranges, 1);
}

/// Rooms and ants glide to a rearranged plan's places; with Reduce motion they snap.
#[test]
fn a_rearrange_glides_unless_motion_is_reduced() {
    for still in [false, true] {
        let mut farm = AntFarmView::new();
        farm.set_reduce_motion(still);
        let mut s = fleet(1, 1);
        let mut plan = RoomPlan::default();
        let mut before = frame(&s, &mut plan, 0);
        farm.glide_rooms(&mut before, 0.0);
        farm.advance(&before, 0.0);
        // Rooms until the plan is laid out again.
        let mut r = 1;
        let mut after = before.clone();
        while plan.rearranges == 1 {
            add_run(&mut s, r, 1);
            r += 1;
            after = frame(&s, &mut plan, r as i64);
            farm.glide_rooms(&mut after.clone(), 0.0);
            farm.advance(&after, 0.0);
        }
        let target = tunnel(&after, 0).chamber;
        let from = tunnel(&before, 0).chamber;
        assert_ne!(target, from, "r0 moved");
        let mut mid = after.clone();
        farm.glide_rooms(&mut mid, GLIDE_S / 2.0);
        let shown = tunnel(&mid, 0).chamber;
        let i = after.ants.iter().position(|a| a.id == id("r0-w0")).unwrap();
        let ant = farm.advance(&after, GLIDE_S / 2.0)[i];
        if still {
            assert_eq!(shown, target, "snaps");
            assert_eq!(ant.pos, after.ants[i].pos);
        } else {
            assert!(shown != target && shown != from, "on its way");
            assert!(ant.heading.is_some(), "the ant glides along");
            let mut end = after.clone();
            farm.glide_rooms(&mut end, GLIDE_S * 2.0);
            assert_eq!(tunnel(&end, 0).chamber, target);
        }
    }
}

/// An ant going up to wait on Karl leaves its room as dug, the others where they were.
#[test]
fn an_ant_leaving_its_room_moves_no_room() {
    let mut s = fleet(3, 3);
    let mut plan = RoomPlan::default();
    let before = frame(&s, &mut plan, 0);
    let worker = s.nodes.iter_mut().find(|n| n.id == id("r0-w2")).unwrap();
    worker.state = NodeState::NeedsInput;
    let after = frame(&s, &mut plan, 1_000);
    assert_kept(&before, &after);
    assert_eq!(plan.rearranges, 1);
}

use std::collections::{BTreeSet, HashSet};

use eframe::egui::{Pos2, Rect, vec2};

use super::layout::*;
use super::layout_tests::{id, node, run};
use super::rooms::{GRACE_MS, RoomPlan};
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

/// Every chamber in `scene` is in its view, clear of the others and their shafts, and holds
/// its ants.
fn assert_laid_out(scene: &Scene) {
    let tunnels = &scene.tunnels;
    for (i, t) in tunnels.iter().enumerate() {
        assert!(
            scene.rect.contains_rect(t.chamber),
            "{:?} in the view",
            t.key
        );
        for u in tunnels.iter().skip(i + 1) {
            assert!(!t.chamber.intersects(u.chamber), "{:?}, {:?}", t.key, u.key);
            assert!(
                !t.shaft.intersects(u.chamber),
                "{:?} crosses {:?}",
                t.key,
                u.key
            );
            assert!(
                !u.shaft.intersects(t.chamber),
                "{:?} crosses {:?}",
                u.key,
                t.key
            );
        }
    }
    for a in scene.ants.iter().filter(|a| a.kind == NodeKind::Worker) {
        let Spot::Chamber(key, _) = &a.spot else {
            continue;
        };
        let t = tunnels.iter().find(|t| &t.key == key).unwrap();
        assert!(t.chamber.contains(a.pos), "{} in its chamber", a.id);
    }
}

/// A new room lays the farm out afresh, once, with no spare cells kept for it.
#[test]
fn a_new_room_lays_the_farm_out_again() {
    let mut s = fleet(3, 2);
    let mut plan = RoomPlan::default();
    frame(&s, &mut plan, 0);
    assert_eq!(plan.rearranges, 1, "the first frame lays the plan out");
    let cells = |plan: &RoomPlan| plan.grid().unwrap().depths.iter().sum::<usize>();
    assert_eq!(cells(&plan), 3, "no spare cells");
    add_run(&mut s, 3, 2);
    let after = frame(&s, &mut plan, 1_000);
    assert_eq!(plan.rearranges, 2, "laid out again");
    assert_eq!(cells(&plan), 4);
    assert_laid_out(&after);
    frame(&s, &mut plan, 2_000);
    assert_eq!(plan.rearranges, 2, "once");
}

/// Ants joining a room within its spare slots move nothing: no room, no other ant, no
/// change of scale. The ant after that lays the farm out again, once, with spare slots
/// anew.
#[test]
fn growth_within_the_margin_moves_nothing() {
    let mut s = fleet(3, 2);
    let mut plan = RoomPlan::default();
    let before = frame(&s, &mut plan, 0);
    let mut w = 2;
    while plan.rearranges == 1 {
        add_worker(&mut s, 1, w);
        w += 1;
        let after = frame(&s, &mut plan, w as i64);
        if plan.rearranges > 1 {
            break;
        }
        assert_eq!(after.px, before.px, "the scale is kept with {w} ants");
        for t in before
            .tunnels
            .iter()
            .filter(|t| t.key != TunnelKey::Run("r1".into()))
        {
            let u = after.tunnels.iter().find(|u| u.key == t.key).unwrap();
            assert_eq!(
                (t.chamber, t.shaft),
                (u.chamber, u.shaft),
                "{:?} moved",
                t.key
            );
        }
        for a in before
            .ants
            .iter()
            .filter(|a| !a.id.as_str().starts_with("r1-"))
        {
            assert_eq!(after.ant(&a.id).unwrap().pos, a.pos, "{} moved", a.id);
        }
        assert_laid_out(&after);
    }
    // The last ant added is the one that did not fit.
    let joined = w - 3;
    assert!(joined >= 1, "a spare slot at least");
    assert_eq!(plan.rearranges, 2);
    add_worker(&mut s, 1, w);
    let after = frame(&s, &mut plan, 1_000);
    assert_eq!(plan.rearranges, 2, "spare slots again");
    assert_laid_out(&after);
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
    assert_eq!(plan.rearranges, 2, "its ground is given back");
    assert_laid_out(&gone);
}

#[test]
fn filled_in_rooms_give_their_ground_back() {
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
    assert_eq!(plan.grid().unwrap().depths.iter().sum::<usize>(), 2);
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

/// A resize fits the farm to the new view, the rooms in the same order.
#[test]
fn a_resize_fits_the_farm_to_the_view() {
    let s = fleet(4, 2);
    let mut plan = RoomPlan::default();
    frame(&s, &mut plan, 0);
    let keys: Vec<_> = plan.rooms.iter().map(|r| r.key.clone()).collect();
    let small = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0));
    let scene = frame_in(&s, &mut plan, 0, small);
    let after: Vec<_> = plan.rooms.iter().map(|r| r.key.clone()).collect();
    assert_eq!(keys, after);
    assert_eq!(plan.rearranges, 2);
    assert_laid_out(&scene);
    let widths = &plan.grid().unwrap().widths;
    assert!(
        (widths.iter().sum::<f32>() - 1000.0).abs() < 0.01,
        "{widths:?}"
    );
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

/// A re-layout that draws the ants smaller zooms out smoothly: the scale, the gallery and
/// the ants' size pass through the sizes between. With Reduce motion it snaps.
#[test]
fn a_new_scale_is_a_smooth_camera_move() {
    for still in [false, true] {
        let mut farm = AntFarmView::new();
        farm.set_reduce_motion(still);
        let mut plan = RoomPlan::default();
        let mut before = frame(&fleet(1, 1), &mut plan, 0);
        farm.glide_rooms(&mut before, 0.0);
        let after = frame(&fleet(8, 4), &mut plan, 1);
        assert!(after.px < before.px, "{} then {}", before.px, after.px);
        farm.glide_rooms(&mut after.clone(), 0.0);
        let mut mid = after.clone();
        farm.glide_rooms(&mut mid, GLIDE_S / 2.0);
        let ant = |scene: &Scene| scene.ant(&id("r0-w0")).unwrap().size;
        if still {
            assert_eq!(mid, after, "snaps");
        } else {
            assert!(after.px < mid.px && mid.px < before.px, "{}", mid.px);
            let (from, to) = (before.gallery.top(), after.gallery.top());
            let shown = mid.gallery.top();
            assert!(shown < from.max(to) && shown > from.min(to), "{shown}");
            assert!(ant(&after) < ant(&mid) && ant(&mid) < ant(&before));
            let mut end = after.clone();
            farm.glide_rooms(&mut end, GLIDE_S * 2.0);
            assert_eq!(end, after, "there at the end");
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

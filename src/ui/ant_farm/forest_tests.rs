use eframe::egui::vec2;

use super::forest::{self, Forest, ForestView, Grove, Health, SawJob};
use super::layout::Scene;
use super::layout_tests::{node, scene, snapshot};
use crate::model::{NodeKind, NodeState};
use crate::orca::spawn::Worktree;

fn worktree(name: &str, created_at: i64) -> Worktree {
    Worktree {
        id: format!("repo::/w/{name}"),
        path: format!("/w/{name}"),
        created_at: Some(created_at),
        ..Worktree::default()
    }
}

fn grove(name: &str) -> Grove {
    Grove {
        id: format!("repo::/w/{name}"),
        name: name.into(),
        health: Health::Bare,
    }
}

fn groves(n: usize) -> Vec<Grove> {
    (0..n).map(|i| grove(&format!("wt-{i}"))).collect()
}

fn farm() -> Scene {
    scene(&snapshot())
}

fn foot_of(forest: &Forest, id: &str) -> eframe::egui::Pos2 {
    forest.tree(id).expect("has a tree").foot
}

/// Linked, unarchived worktrees grow one tree each, oldest first, and look like what is in
/// them: an agent at work, only resting ones, or none.
#[test]
fn a_tree_per_linked_worktree_looking_like_its_agents() {
    let mut main = worktree("main", 0);
    main.is_main_worktree = true;
    let mut archived = worktree("archived", 1);
    archived.is_archived = true;
    let worktrees = vec![
        worktree("empty", 50),
        worktree("busy", 10),
        main,
        archived,
        worktree("resting", 20),
        worktree("parked", 30),
        worktree("gone", 40),
    ];
    let mut nodes = Vec::new();
    for (name, path, state) in [
        ("a", "/w/busy", NodeState::Working),
        ("b", "/w/busy/", NodeState::Idle),
        ("c", "/w/resting", NodeState::Done),
        ("d", "/w/parked", NodeState::Working),
        ("e", "/w/gone", NodeState::Working),
    ] {
        let mut n = node(name, NodeKind::Terminal, None, state);
        n.worktree_path = Some(path.into());
        nodes.push(n);
    }
    nodes[3].parked_at_ms = Some(1);
    nodes[4].live = false;
    let got: Vec<(String, Health)> = forest::groves(&worktrees, &nodes)
        .into_iter()
        .map(|g| (g.name, g.health))
        .collect();
    let want = [
        ("busy", Health::Leafy),
        ("resting", Health::Resting),
        ("parked", Health::Resting),
        ("gone", Health::Bare),
        ("empty", Health::Bare),
    ]
    .map(|(n, h)| (n.to_string(), h));
    assert_eq!(got, want);
}

/// Few worktrees: every one has a full-size tree in the grass, on the strip past the landing,
/// the same every frame.
#[test]
fn trees_stand_on_the_surface() {
    let scene = farm();
    let groves = groves(5);
    let forest = forest::place(&scene, &groves);
    assert_eq!(forest.trees.len(), 5);
    assert_eq!((forest.more, forest.sign), (0, None));
    assert_eq!(forest, forest::place(&scene, &groves), "deterministic");
    for tree in &forest.trees {
        assert_eq!(tree.k, forest::texel(&scene, forest::FULL), "full size");
        assert_eq!(tree.foot.y, scene.surface_y + scene.px, "in the grass");
        let rect = tree.rect();
        assert!(rect.left() >= scene.landing.right(), "past the landing");
        assert!(rect.right() <= scene.rect.right());
        assert!(rect.top() >= scene.rect.top(), "under the top");
    }
    let mut feet: Vec<f32> = forest.trees.iter().map(|t| t.foot.x).collect();
    feet.sort_by(f32::total_cmp);
    feet.dedup();
    assert_eq!(feet.len(), 5, "no two share a spot");
}

/// A worktree keeps its spot when others open or close.
#[test]
fn trees_keep_their_spots() {
    let scene = farm();
    let all = groves(6);
    let before = forest::place(&scene, &all[..5]);
    let opened = forest::place(&scene, &all);
    let closed = forest::place(&scene, &all[1..5]);
    for g in &all[1..5] {
        assert_eq!(
            foot_of(&before, &g.id),
            foot_of(&opened, &g.id),
            "{}",
            g.name
        );
        assert_eq!(
            foot_of(&before, &g.id),
            foot_of(&closed, &g.id),
            "{}",
            g.name
        );
    }
}

/// Many worktrees: smaller trees; past that, the newest go into a "+N" sign, and every
/// worktree is either a tree or counted on it.
#[test]
fn a_crowd_shrinks_then_counts_the_rest() {
    let scene = farm();
    let mut full = 0;
    for n in 1..200 {
        let forest = forest::place(&scene, &groves(n));
        assert_eq!(forest.trees.len() + forest.more, n, "{n} worktrees");
        if forest.trees[0].k == forest::texel(&scene, forest::FULL) {
            full = n;
        }
        assert_eq!(forest.sign.is_some(), forest.more > 0);
        if forest.more > 0 {
            let crowded = forest::texel(&scene, forest::CROWDED);
            assert!(forest.trees.iter().all(|t| t.k == crowded), "smaller");
            assert!(
                forest.tree(&format!("repo::/w/wt-{}", n - 1)).is_none(),
                "the newest has no tree"
            );
        }
    }
    assert!(full >= 5, "a handful fit at full size ({full})");
    assert!(forest::place(&scene, &groves(199)).more > 0, "capped");
}

/// Hovering a tree finds it, the front one where two overlap.
#[test]
fn hovering_finds_the_tree() {
    let scene = farm();
    let forest = forest::place(&scene, &groves(8));
    for tree in &forest.trees {
        let at = tree.foot - vec2(0.0, tree.rect().height() / 2.0);
        let found = forest::hit(&forest, at).expect("a tree there");
        assert!(
            found.grove.id == tree.grove.id || !found.back,
            "front row wins"
        );
    }
    assert!(forest::hit(&forest, scene.queen_chamber.center()).is_none());
}

/// A tree whose worktree closes falls; the saw fells its trees before the next poll; a gap
/// (the farm off screen) fells nothing.
#[test]
fn closed_worktrees_fall() {
    let scene = farm();
    let mut view = ForestView::default();
    view.set(groves(3), None);
    assert_eq!(view.layout(&scene, 0.0, false).trees.len(), 3);
    assert!(!view.animating());
    view.set(groves(2), None);
    assert_eq!(view.layout(&scene, 1.0, false).trees.len(), 2);
    assert!(view.animating(), "the closed one falls");
    assert!(!{
        view.layout(&scene, 10.0, false);
        view.animating()
    });

    let job = SawJob {
        done: 1,
        total: 2,
        current: Some(grove("wt-1").id),
        felled: vec![grove("wt-0").id],
    };
    view.set(groves(2), Some(job));
    let forest = view.layout(&scene, 11.0, false);
    assert!(forest.tree(&grove("wt-0").id).is_none(), "felled at once");
    assert!(forest.tree(&grove("wt-1").id).is_some(), "still being cut");

    let mut away = ForestView::default();
    away.set(groves(3), None);
    away.layout(&scene, 0.0, false);
    away.set(groves(1), None);
    away.layout(&scene, 60.0, true);
    assert!(!away.animating());
}

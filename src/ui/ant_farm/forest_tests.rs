use eframe::egui::vec2;

use super::forest::{self, ForestView, Grove, Health, SawJob};
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

/// Every tree stands in the grass across the farm, sized by its id, the same
/// every frame.
#[test]
fn trees_stand_on_the_surface() {
    let scene = farm();
    let groves = groves(30);
    let forest = forest::place(&scene, &groves);
    assert_eq!(forest.trees.len(), 30, "one tree each");
    assert_eq!((forest.more, forest.sign), (0, None));
    assert_eq!(forest, forest::place(&scene, &groves), "deterministic");
    let full = forest::texel(&scene, forest::FULL);
    for tree in &forest.trees {
        assert!(
            tree.k <= full && tree.k >= 0.75 * scene.px - 0.01,
            "{}",
            tree.k
        );
        assert_eq!(tree.foot.y, scene.surface_y + scene.px, "in the grass");
        let rect = tree.rect();
        assert!(rect.left() >= scene.rect.left(), "inside the farm");
        assert!(rect.right() <= scene.rect.right());
        assert!(rect.top() >= scene.rect.top(), "under the top");
    }
    let mut sizes: Vec<f32> = forest.trees.iter().map(|t| t.k).collect();
    sizes.sort_by(f32::total_cmp);
    assert!(sizes[29] - sizes[0] > sizes[29] / 4.0, "sizes vary");
    let small = forest::place(&scene, &groves[..1]).trees[0].k;
    assert_eq!(
        small,
        forest.tree(&groves[0].id).unwrap().k,
        "a tree keeps its size"
    );
}

/// The forest grows from the left like a loading bar, oldest first: the more worktrees, the
/// further right it reaches, all the way at [`forest::FILL_AT`].
#[test]
fn the_forest_fills_from_the_left() {
    let scene = farm();
    let reach = |n: usize| {
        let forest = forest::place(&scene, &groves(n));
        forest
            .trees
            .iter()
            .map(|t| t.foot.x)
            .fold(f32::MIN, f32::max)
    };
    let mut last = f32::MIN;
    for n in [2, 5, 10, 20, forest::FILL_AT] {
        let r = reach(n);
        assert!(r > last, "{n} worktrees reach further ({r} after {last})");
        last = r;
    }
    assert!(
        reach(5) < scene.rect.center().x + scene.rect.width() / 4.0,
        "5 stay left"
    );
    assert!(
        reach(forest::FILL_AT) > scene.rect.right() - 6.0 * scene.cell(),
        "a full forest reaches the right end"
    );
    let forest = forest::place(&scene, &groves(20));
    let x = |i: usize| forest.tree(&format!("repo::/w/wt-{i}")).unwrap().foot.x;
    assert!(x(0) < x(10) && x(10) < x(19), "oldest at the left");
}

/// Up to [`forest::MAX_TREES`] every worktree is a tree; past that the newest go on a "+N"
/// sign.
#[test]
fn a_crowd_is_all_trees_up_to_the_cap() {
    let scene = farm();
    for n in [1, 35, 80, forest::MAX_TREES, forest::MAX_TREES + 25] {
        let forest = forest::place(&scene, &groves(n));
        assert_eq!(forest.trees.len() + forest.more, n, "{n} worktrees");
        assert_eq!(forest.trees.len(), n.min(forest::MAX_TREES));
        assert_eq!(forest.sign.is_some(), forest.more > 0);
    }
    let capped = forest::place(&scene, &groves(forest::MAX_TREES + 1));
    assert!(
        capped
            .tree(&format!("repo::/w/wt-{}", forest::MAX_TREES))
            .is_none()
    );
}

/// Hovering a tree finds it, the one in front where they overlap.
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
    assert_eq!(view.layout(&scene, 0.0, false, false).trees.len(), 3);
    assert!(!view.animating());
    view.set(groves(2), None);
    assert_eq!(view.layout(&scene, 1.0, false, false).trees.len(), 2);
    assert!(view.animating(), "the closed one falls");
    assert!(!{
        view.layout(&scene, 10.0, false, false);
        view.animating()
    });

    let job = SawJob {
        done: 1,
        total: 2,
        current: Some(grove("wt-1").id),
        felled: vec![grove("wt-0").id],
    };
    view.set(groves(2), Some(job));
    let forest = view.layout(&scene, 11.0, false, false);
    assert!(forest.tree(&grove("wt-0").id).is_none(), "felled at once");
    assert!(forest.tree(&grove("wt-1").id).is_some(), "still being cut");

    let mut away = ForestView::default();
    away.set(groves(3), None);
    away.layout(&scene, 0.0, false, false);
    away.set(groves(1), None);
    away.layout(&scene, 60.0, true, false);
    assert!(!away.animating());
}

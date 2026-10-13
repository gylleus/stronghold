use eframe::egui::vec2;

use super::layout::Scene;
use super::layout_tests::{node, scene, snapshot};
use super::rubble::{self, CleanupJob, Health, RubbleView, Site};
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

fn site(name: &str) -> Site {
    Site {
        id: format!("repo::/w/{name}"),
        name: name.into(),
        health: Health::Empty,
    }
}

fn sites(n: usize) -> Vec<Site> {
    (0..n).map(|i| site(&format!("wt-{i}"))).collect()
}

fn farm() -> Scene {
    scene(&snapshot())
}

/// Linked, unarchived worktrees grow one heap each, oldest first, and look like what is in
/// them: an agent at work, only resting ones, or none.
#[test]
fn a_heap_per_linked_worktree_looking_like_its_agents() {
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
    let got: Vec<(String, Health)> = rubble::sites(&worktrees, &nodes)
        .into_iter()
        .map(|g| (g.name, g.health))
        .collect();
    let want = [
        ("busy", Health::Active),
        ("resting", Health::Resting),
        ("parked", Health::Resting),
        ("gone", Health::Empty),
        ("empty", Health::Empty),
    ]
    .map(|(n, h)| (n.to_string(), h));
    assert_eq!(got, want);
}

/// Every heap stands on the surface across the farm, sized by its id, the same
/// every frame.
#[test]
fn heaps_stand_on_the_surface() {
    let scene = farm();
    let sites = sites(30);
    let rubble = rubble::place(&scene, &sites);
    assert_eq!(rubble.heaps.len(), 30, "one heap each");
    assert_eq!((rubble.more, rubble.sign), (0, None));
    assert_eq!(rubble, rubble::place(&scene, &sites), "deterministic");
    let full = rubble::texel(&scene, rubble::FULL);
    for heap in &rubble.heaps {
        assert!(
            heap.k <= full && heap.k >= 0.5 * scene.px - 0.01,
            "{}",
            heap.k
        );
        assert_eq!(heap.foot.y, scene.surface_y + scene.px, "on the surface");
        let rect = heap.rect();
        assert!(rect.left() >= scene.rect.left(), "inside the farm");
        assert!(rect.right() <= scene.rect.right());
        assert!(rect.top() >= scene.rect.top(), "under the top");
    }
    let mut sizes: Vec<f32> = rubble.heaps.iter().map(|t| t.k).collect();
    sizes.sort_by(f32::total_cmp);
    assert!(sizes[29] - sizes[0] > sizes[29] / 4.0, "sizes vary");
    let small = rubble::place(&scene, &sites[..1]).heaps[0].k;
    assert_eq!(
        small,
        rubble.heap(&sites[0].id).unwrap().k,
        "a heap keeps its size"
    );
}

/// The rubble grows from the left like a loading bar, oldest first: the more worktrees, the
/// further right it reaches, all the way at [`rubble::FILL_AT`].
#[test]
fn the_rubble_fills_from_the_left() {
    let scene = farm();
    let reach = |n: usize| {
        let rubble = rubble::place(&scene, &sites(n));
        rubble
            .heaps
            .iter()
            .map(|t| t.foot.x)
            .fold(f32::MIN, f32::max)
    };
    let mut last = f32::MIN;
    for n in [2, 5, 10, 20, rubble::FILL_AT] {
        let r = reach(n);
        assert!(r > last, "{n} worktrees reach further ({r} after {last})");
        last = r;
    }
    assert!(
        reach(5) < scene.rect.center().x + scene.rect.width() / 4.0,
        "5 stay left"
    );
    assert!(
        reach(rubble::FILL_AT) > scene.rect.right() - 6.0 * scene.cell(),
        "a full rubble reaches the right end"
    );
    let rubble = rubble::place(&scene, &sites(20));
    let x = |i: usize| rubble.heap(&format!("repo::/w/wt-{i}")).unwrap().foot.x;
    assert!(x(0) < x(10) && x(10) < x(19), "oldest at the left");
}

/// Up to [`rubble::MAX_HEAPS`] every worktree is a heap; past that the newest go on a "+N"
/// sign.
#[test]
fn a_crowd_is_all_heaps_up_to_the_cap() {
    let scene = farm();
    for n in [1, 35, 80, rubble::MAX_HEAPS, rubble::MAX_HEAPS + 25] {
        let rubble = rubble::place(&scene, &sites(n));
        assert_eq!(rubble.heaps.len() + rubble.more, n, "{n} worktrees");
        assert_eq!(rubble.heaps.len(), n.min(rubble::MAX_HEAPS));
        assert_eq!(rubble.sign.is_some(), rubble.more > 0);
    }
    let capped = rubble::place(&scene, &sites(rubble::MAX_HEAPS + 1));
    assert!(
        capped
            .heap(&format!("repo::/w/wt-{}", rubble::MAX_HEAPS))
            .is_none()
    );
}

/// Hovering a heap finds it, the one in front where they overlap.
#[test]
fn hovering_finds_the_heap() {
    let scene = farm();
    let rubble = rubble::place(&scene, &sites(8));
    for heap in &rubble.heaps {
        let at = heap.foot - vec2(0.0, heap.rect().height() / 2.0);
        let found = rubble::hit(&rubble, at).expect("a heap there");
        assert!(
            found.site.id == heap.site.id || !found.back,
            "front row wins"
        );
    }
    assert!(rubble::hit(&rubble, scene.throne_hall.center()).is_none());
}

/// A heap whose worktree closes crumbles; the hammer clears its heaps before the next poll; a gap
/// (the farm off screen) clears nothing.
#[test]
fn closed_worktrees_crumble() {
    let scene = farm();
    let mut view = RubbleView::default();
    view.set(sites(3), None);
    assert_eq!(view.layout(&scene, 0.0, false, false).heaps.len(), 3);
    assert!(!view.animating());
    view.set(sites(2), None);
    assert_eq!(view.layout(&scene, 1.0, false, false).heaps.len(), 2);
    assert!(view.animating(), "the closed one crumbles");
    assert!(!{
        view.layout(&scene, 10.0, false, false);
        view.animating()
    });

    let job = CleanupJob {
        done: 1,
        total: 2,
        current: Some(site("wt-1").id),
        cleared: vec![site("wt-0").id],
    };
    view.set(sites(2), Some(job));
    let rubble = view.layout(&scene, 11.0, false, false);
    assert!(rubble.heap(&site("wt-0").id).is_none(), "cleared at once");
    assert!(
        rubble.heap(&site("wt-1").id).is_some(),
        "still being cleared"
    );

    let mut away = RubbleView::default();
    away.set(sites(3), None);
    away.layout(&scene, 0.0, false, false);
    away.set(sites(1), None);
    away.layout(&scene, 60.0, true, false);
    assert!(!away.animating());
}

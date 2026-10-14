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
        health: Health::Removable,
    }
}

fn sites(n: usize) -> Vec<Site> {
    (0..n).map(|i| site(&format!("wt-{i}"))).collect()
}

fn farm() -> Scene {
    scene(&snapshot())
}

#[test]
fn only_planner_stale_entries_are_drawn_and_archive_work_is_distinct() {
    use crate::worktrees::{Plan, PlanEntry, Verdict};
    let mut trees: Vec<_> = [
        "remove",
        "archive",
        "kept",
        "busy",
        "parked",
        "pinned",
        "auto_archive",
    ]
    .iter()
    .enumerate()
    .map(|(i, n)| worktree(n, i as i64))
    .collect();
    trees[5].is_pinned = true;
    let plan = Plan {
        entries: trees
            .iter()
            .map(|w| PlanEntry {
                id: w.id.clone(),
                path: w.path.clone(),
                name: w.name().into(),
                verdict: Verdict {
                    remove: w.name() != "kept" && w.name() != "archive",
                    archivable: w.name() == "archive",
                    archive_required: w.name() == "auto_archive",
                    ..Verdict::default()
                },
                ..PlanEntry::default()
            })
            .collect(),
        ..Plan::default()
    };
    let mut busy = node("worker", NodeKind::Terminal, None, NodeState::Done);
    busy.worktree_path = Some("/w/busy/".into());
    let mut parked = busy.clone();
    parked.worktree_path = Some("/w/parked".into());
    parked.live = false;
    parked.parked_at_ms = Some(1);
    let nodes = [busy, parked];
    let got: Vec<_> = rubble::sites(&trees, &nodes, Some(&plan))
        .into_iter()
        .map(|s| (s.name, s.health))
        .collect();
    assert_eq!(
        got,
        vec![
            ("remove".into(), Health::Removable),
            ("archive".into(), Health::Archive),
            ("auto_archive".into(), Health::Archive)
        ]
    );
    assert!(rubble::sites(&trees, &nodes, None).is_empty());
    assert!(rubble::sites(&trees, &nodes, Some(&Plan::default())).is_empty());
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

/// A heap whose worktree closes bursts; the hammer clears its heaps before the next poll; a gap
/// (the farm off screen) clears nothing.
#[test]
fn closed_worktrees_scatter() {
    let scene = farm();
    let mut view = RubbleView::default();
    view.set(sites(3), None);
    assert_eq!(view.layout(&scene, 0.0, false, false).heaps.len(), 3);
    assert!(!view.animating());
    view.set(sites(2), None);
    assert_eq!(view.layout(&scene, 1.0, false, false).heaps.len(), 2);
    assert!(view.animating(), "the closed one scatters");
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

#[test]
fn each_hammer_contact_emits_dust_chips_and_sparks() {
    use rubble::{ParticleKind, impact_particles};
    for phase in [0.0, 0.32, 0.439, 0.79, 1.0] {
        assert_eq!(impact_particles(phase, 17).count(), 0, "outside contact");
    }
    for cycle in [0.0, 0.65, 1.3] {
        let phase = super::hammer::phase(cycle + 0.36, false);
        let particles: Vec<_> = impact_particles(phase, 17).collect();
        assert_eq!(particles.len(), 12, "bounded on every stroke");
        for kind in [ParticleKind::Dust, ParticleKind::Chip, ParticleKind::Spark] {
            assert!(particles.iter().any(|p| p.kind == kind));
        }
        assert!(particles.iter().any(|p| p.offset.x < -1.0));
        assert!(particles.iter().any(|p| p.offset.x > 1.0));
        assert!(particles.iter().any(|p| p.offset.y < -1.0));
    }
    let early: Vec<_> = impact_particles(0.5, 17).collect();
    let late: Vec<_> = impact_particles(0.74, 17).collect();
    assert!(early.iter().zip(&late).all(|(a, b)| b.alpha < a.alpha));
    assert_eq!(early, impact_particles(0.5, 17).collect::<Vec<_>>());
    assert_ne!(early, impact_particles(0.5, 18).collect::<Vec<_>>());
}

#[test]
fn broken_heap_scatters_solid_chunks_then_fades_without_shrinking() {
    use rubble::{ParticleKind, break_particles};
    let chunks = |age| {
        break_particles(age, 42)
            .filter(|p| p.kind == ParticleKind::Chunk)
            .collect::<Vec<_>>()
    };
    let start = chunks(0.0);
    let flying = chunks(0.4);
    let fading = chunks(1.2);
    assert_eq!(start.len(), 9);
    assert_eq!(flying.len(), 9);
    assert_eq!(fading.len(), 9);
    assert_eq!(
        flying,
        chunks(0.4),
        "sampling does not depend on frame history"
    );
    assert!(flying.iter().any(|p| p.offset.x < -15.0));
    assert!(flying.iter().any(|p| p.offset.x > 15.0));
    for ((a, b), c) in start.iter().zip(&flying).zip(&fading) {
        assert!(b.offset.y < a.offset.y, "chunks fly upward first");
        assert!(c.offset.y > b.offset.y, "gravity brings them back down");
        assert_ne!(a.angle, b.angle, "chunks tumble");
        assert_eq!(a.size, c.size, "solid pieces never shrink away");
        assert_eq!(b.alpha, 1.0);
        assert!(c.alpha > 0.0 && c.alpha < b.alpha);
    }
    assert!(break_particles(0.2, 42).count() <= 37);
    assert_eq!(break_particles(-0.1, 42).count(), 0);
    assert_eq!(break_particles(rubble::BREAK_S, 42).count(), 0);
    assert_eq!(break_particles(10.0, 42).count(), 0);
}

#[test]
fn cleared_heap_bursts_once_and_expires_even_while_cleanup_is_reported() {
    let scene = farm();
    let mut view = RubbleView::default();
    view.set(sites(1), None);
    view.layout(&scene, 0.0, false, false);
    view.set(
        sites(1),
        Some(CleanupJob {
            cleared: vec![site("wt-0").id],
            ..CleanupJob::default()
        }),
    );
    assert!(view.layout(&scene, 1.0, false, false).heaps.is_empty());
    view.layout(&scene, 1.0 + rubble::BREAK_S, false, false);
    view.set(Vec::new(), None);
    view.layout(&scene, 3.0, false, false);
    assert!(!view.animating(), "poll must not spawn a second burst");
}

#[test]
fn reduced_motion_and_leaving_the_farm_discard_break_effects() {
    let scene = farm();
    for (away, still) in [(true, false), (false, true)] {
        let mut view = RubbleView::default();
        view.set(sites(3), None);
        view.layout(&scene, 0.0, false, false);
        view.set(sites(2), None);
        view.layout(&scene, 0.1, false, false);
        assert!(view.animating());
        view.set(sites(1), None);
        view.layout(&scene, 0.2, away, still);
        assert!(
            !view.animating(),
            "discard existing and newly cleared effects"
        );
        view.layout(&scene, 0.3, false, false);
        assert!(!view.animating(), "never replay suppressed bursts");
    }
}

#[test]
fn bulk_cleanup_caps_bursts_and_drops_them_after_the_fade() {
    let scene = farm();
    let mut view = RubbleView::default();
    view.set(sites(rubble::MAX_HEAPS), None);
    view.layout(&scene, 0.0, false, false);
    view.set(Vec::new(), None);
    view.layout(&scene, 1.0, false, false);
    assert_eq!(view.burst_count(), rubble::MAX_BURSTS);
    view.layout(&scene, 1.1, false, false);
    assert_eq!(
        view.burst_count(),
        rubble::MAX_BURSTS,
        "no duplicate bursts"
    );
    view.layout(&scene, 1.0 + rubble::BREAK_S, false, false);
    assert_eq!(view.burst_count(), 0);
    assert!(!view.animating(), "no repaint needed after the fade");
}

#[test]
fn impact_shake_kicks_only_at_contact_and_decays_to_rest() {
    use super::hammer::{CONTACT, IMPACT_SPAN};
    assert_eq!(rubble::impact_shake(CONTACT - 0.001), vec2(0.0, 0.0));
    let kick = rubble::impact_shake(CONTACT).length();
    assert!(kick > 2.5);
    for fraction in [0.25, 0.5, 0.75, 0.95] {
        assert!(
            rubble::impact_shake(CONTACT + IMPACT_SPAN * fraction).length()
                <= kick * (1.0 - fraction).powi(3) + 0.001
        );
    }
    assert_eq!(rubble::impact_shake(CONTACT + IMPACT_SPAN), vec2(0.0, 0.0));
}

#[test]
fn a_heap_becoming_protected_disappears_without_a_demolition_burst() {
    let scene = farm();
    let mut view = RubbleView::default();
    view.set_stale(sites(2), None);
    view.layout(&scene, 0.0, false, false);
    view.set_stale(sites(1), None);
    view.layout(&scene, 0.1, false, false);
    assert_eq!(view.burst_count(), 0);
    view.set_stale(
        vec![],
        Some(CleanupJob {
            cleared: vec![site("wt-0").id],
            ..CleanupJob::default()
        }),
    );
    view.layout(&scene, 0.2, false, false);
    assert_eq!(view.burst_count(), 1, "reported removals still break apart");
}

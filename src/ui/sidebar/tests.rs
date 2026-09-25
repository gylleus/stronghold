use super::*;
use crate::model::RunSummary;

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

const ORCA: &str = "/Users/x/orca/workspaces";

/// Run `alpha` in repo `shop` (coordinator, a live worker, a released dispatch without a
/// worktree), run `beta` in repo `infra` and a loose terminal in `myrstack`.
fn snapshot() -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    s.runs = vec![
        RunSummary {
            id: "alpha".into(),
            objective: Some("Migrate checkout to Stripe".into()),
            coordinator: Some(id("ca")),
            task_counts: Some(TaskCounts {
                ready: 3,
                dispatched: 2,
                completed: 14,
                ..TaskCounts::default()
            }),
        },
        RunSummary {
            id: "beta".into(),
            objective: Some("Upgrade Postgres".into()),
            coordinator: Some(id("cb")),
            task_counts: None,
        },
    ];

    let mut ca = node("ca", NodeKind::Coordinator, Some("alpha"));
    ca.worktree_path = Some(format!("{ORCA}/shop/main"));
    ca.last_output_ms = Some(1_000);
    ca.state = NodeState::Idle;
    let mut a1 = node("a1", NodeKind::Worker, Some("alpha"));
    a1.worktree_path = Some(format!("{ORCA}/shop/feat-pay"));
    a1.branch = Some("feat/pay-intents".into());
    a1.state = NodeState::Stalled;
    let mut a2 = node("a2", NodeKind::Worker, Some("alpha"));
    a2.live = false;
    a2.released_at_ms = Some(500);
    a2.state = NodeState::Done;

    let mut cb = node("cb", NodeKind::Coordinator, Some("beta"));
    cb.worktree_path = Some(format!("{ORCA}/infra/pg"));
    cb.last_output_ms = Some(5_000);
    let mut b1 = node("b1", NodeKind::Worker, Some("beta"));
    b1.worktree_path = Some(format!("{ORCA}/infra/pg-17"));

    let mut loose = node("loose", NodeKind::Terminal, None);
    loose.worktree_path = Some("/Users/x/repo/saker/myrstack-worktrees/myr-1".into());
    loose.state = NodeState::NeedsInput;

    s.nodes = vec![ca, a1, a2, cb, b1, loose];
    s
}

fn visible(filter: &FleetFilter, s: &FleetSnapshot) -> Vec<String> {
    s.nodes
        .iter()
        .filter(|n| filter.matches(n, s))
        .map(|n| n.id.0.clone())
        .collect()
}

#[test]
fn repo_of_orca_workspace() {
    assert_eq!(
        repo_of("/Users/x/orca/workspaces/shop/feat-pay").as_deref(),
        Some("shop")
    );
    assert_eq!(
        repo_of("/Users/x/orca/workspaces/shop/feat-pay/").as_deref(),
        Some("shop")
    );
    assert_eq!(
        repo_of("/Users/x/orca/workspaces/shop").as_deref(),
        Some("shop")
    );
}

#[test]
fn repo_of_falls_back_to_parent_dir() {
    assert_eq!(
        repo_of("/Users/x/code/saker/myrstack").as_deref(),
        Some("saker")
    );
    assert_eq!(
        repo_of("/Users/x/repo/myrstack-worktrees/myr-k7y.6").as_deref(),
        Some("myrstack")
    );
    assert_eq!(repo_of("/"), None);
    assert_eq!(repo_of(""), None);
}

#[test]
fn default_filter_shows_everything() {
    let s = snapshot();
    let filter = FleetFilter::default();
    assert!(!filter.is_active());
    assert_eq!(visible(&filter, &s).len(), s.nodes.len());
    assert!(filter.hidden(&s).is_empty());
}

#[test]
fn worker_without_worktree_inherits_coordinator_repo() {
    let s = snapshot();
    assert_eq!(node_repo(&s.nodes[2], &s), "shop");
    let mut orphan = node("x", NodeKind::Worker, None);
    orphan.worktree_path = None;
    assert_eq!(node_repo(&orphan, &s), NO_REPO);
}

#[test]
fn repo_filter() {
    let s = snapshot();
    let filter = FleetFilter {
        repos: ["infra".to_string()].into(),
        ..FleetFilter::default()
    };
    assert_eq!(visible(&filter, &s), ["cb", "b1"]);
}

#[test]
fn state_and_kind_filters() {
    let s = snapshot();
    let filter = FleetFilter {
        states: [NodeState::Stalled, NodeState::NeedsInput].into(),
        ..FleetFilter::default()
    };
    assert_eq!(visible(&filter, &s), ["a1", "loose"]);

    let filter = FleetFilter {
        kinds: [NodeKind::Coordinator].into(),
        ..FleetFilter::default()
    };
    assert_eq!(visible(&filter, &s), ["ca", "cb"]);
}

#[test]
fn loose_and_released_toggles() {
    let s = snapshot();
    let filter = FleetFilter {
        show_loose: false,
        show_released: false,
        ..FleetFilter::default()
    };
    assert_eq!(visible(&filter, &s), ["ca", "a1", "cb", "b1"]);
    assert_eq!(filter.hidden(&s), [id("a2"), id("loose")].into());
}

#[test]
fn text_matches_label_branch_and_objective_case_insensitively() {
    let s = snapshot();
    let by = |text: &str| {
        let filter = FleetFilter {
            text: text.into(),
            ..FleetFilter::default()
        };
        visible(&filter, &s)
    };
    assert_eq!(by("PAY-INTENTS"), ["a1"]);
    assert_eq!(by("stripe"), ["ca", "a1", "a2"]);
    assert_eq!(by("loose"), ["loose"]);
    assert_eq!(by("  "), ["ca", "a1", "a2", "cb", "b1", "loose"]);
    assert!(by("nothing-matches").is_empty());
}

#[test]
fn filters_combine() {
    let s = snapshot();
    let filter = FleetFilter {
        repos: ["shop".to_string()].into(),
        kinds: [NodeKind::Worker].into(),
        show_released: false,
        ..FleetFilter::default()
    };
    assert_eq!(visible(&filter, &s), ["a1"]);
}

#[test]
fn counts() {
    let s = snapshot();
    let repos = repo_counts(&s);
    assert_eq!(repos["shop"], 3);
    assert_eq!(repos["infra"], 2);
    assert_eq!(repos["myrstack"], 1);
    assert_eq!(state_counts(&s)[&NodeState::Working], 2);
    assert_eq!(kind_counts(&s)[&NodeKind::Worker], 3);
}

#[test]
fn toggle_member_treats_empty_as_all() {
    let universe = ["a", "b", "c"].map(String::from);
    let mut set = BTreeSet::new();
    toggle_member(&mut set, &universe, &"b".to_string());
    assert_eq!(set, ["a", "c"].map(String::from).into());
    toggle_member(&mut set, &universe, &"a".to_string());
    assert_eq!(set, ["c"].map(String::from).into());
    toggle_member(&mut set, &universe, &"a".to_string());
    toggle_member(&mut set, &universe, &"b".to_string());
    assert!(set.is_empty(), "all selected collapses to empty");
}

#[test]
fn toggle_chip_adds_and_removes() {
    let mut set = BTreeSet::new();
    toggle_chip(&mut set, NodeState::Stalled);
    assert_eq!(set, [NodeState::Stalled].into());
    toggle_chip(&mut set, NodeState::Stalled);
    assert!(set.is_empty());
}

#[test]
fn runs_sorted_by_recent_activity() {
    let s = snapshot();
    let rows = run_rows(&s);
    assert_eq!(rows[0].id, "beta");
    assert_eq!(rows[0].last_activity_ms, Some(5_000));
    assert_eq!(rows[0].workers, 1);
    assert_eq!(rows[1].id, "alpha");
    assert_eq!(rows[1].workers, 2);
    assert_eq!(rows[1].coordinator_state, Some(NodeState::Idle));
}

#[test]
fn task_summary_lists_non_zero_buckets() {
    let s = snapshot();
    assert_eq!(
        task_summary(&s.runs[0].task_counts.unwrap()),
        "3 ready · 2 running · 14 done"
    );
    assert_eq!(task_summary(&TaskCounts::default()), "no tasks");
}

#[test]
fn sidebar_renders_headless() {
    let ctx = egui::Context::default();
    let s = snapshot();
    let mut filter = FleetFilter::default();
    let mut response = SidebarResponse::default();
    ctx.run_ui(egui::RawInput::default(), |ui| {
        response = sidebar(ui, &s, &mut filter);
    })
    .drop_without_applying_deltas();
    assert_eq!(response, SidebarResponse::default());
}

/// Regression: a row wider than the panel made the resizable sidebar grow every frame until the
/// graph was squeezed out.
#[test]
fn sidebar_panel_width_is_stable() {
    let ctx = egui::Context::default();
    let s = snapshot();
    let mut filter = FleetFilter::default();
    let mut whip_log = crate::ui::whip_log::WhipLog::default();
    whip_log.extend((1..=40).map(long_whip_item));
    let whip_status = crate::whip::WhipStatus {
        enabled: true,
        dry_run: true,
        findings: 12,
        last_tick_ms: Some(0),
        ..Default::default()
    };
    let input = || egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    };
    let mut widths = Vec::new();
    for _ in 0..30 {
        ctx.run_ui(input(), |ui| {
            let width = egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| {
                    sidebar_with(ui, &s, &mut filter, |ui| {
                        whip_log.section(ui, &whip_status, &s, 5_000);
                    });
                })
                .response
                .rect
                .width();
            widths.push(width);
        })
        .drop_without_applying_deltas();
    }
    let settled = widths[5];
    assert!(
        widths[5..].iter().all(|w| (w - settled).abs() < 0.5),
        "sidebar width drifted: {widths:?}"
    );
    assert!(settled < 400.0, "sidebar grew to {settled}");
}

/// A whip log entry with long texts on a node of [`snapshot`].
fn long_whip_item(seq: u64) -> crate::whip::WhipLogItem {
    let long = "a very long piece of text without any natural break ".repeat(8);
    crate::whip::WhipLogItem {
        seq,
        entry: crate::whip::WhipLogEntry {
            at_ms: 1_000,
            action: "nudge #1 via bus to dispatch:d1".into(),
            target: long.clone(),
            reason: long.clone(),
            dry_run: seq.is_multiple_of(2),
            outcome: format!("failed: {long}"),
            manual: false,
        },
        handle: Some(long),
        run_id: Some("alpha".into()),
        task_id: None,
    }
}

#[test]
fn cache_derives_once_per_snapshot_and_recounts_hidden_per_filter() {
    let mut s = snapshot();
    let mut cache = SidebarCache::default();
    assert_eq!(cache.derived(&s).runs[0].row, run_rows(&s)[0]);
    assert_eq!(cache.derived(&s).repos, repo_counts(&s));

    let workers = FleetFilter {
        kinds: [NodeKind::Worker].into(),
        ..FleetFilter::default()
    };
    let hidden = workers.hidden(&s).len();
    assert_eq!(cache.hidden_count(&s, &FleetFilter::default()), 0);
    assert_eq!(cache.hidden_count(&s, &workers), hidden);

    // Changes to the snapshot show once the cache is told.
    s.nodes.pop();
    assert_eq!(cache.derived(&s).repos, snapshot_repo_counts());
    cache.snapshot_changed();
    assert_eq!(cache.derived(&s).repos, repo_counts(&s));
    assert_ne!(repo_counts(&s), snapshot_repo_counts());
}

fn snapshot_repo_counts() -> BTreeMap<String, usize> {
    repo_counts(&snapshot())
}

#[test]
fn filters_never_hide_the_queen_and_counts_skip_it() {
    let mut s = snapshot();
    s.add_queen();
    let filters = [
        FleetFilter {
            show_loose: false,
            show_released: false,
            ..FleetFilter::default()
        },
        FleetFilter {
            states: [NodeState::Done].into(),
            ..FleetFilter::default()
        },
        FleetFilter {
            kinds: [NodeKind::Worker].into(),
            ..FleetFilter::default()
        },
        FleetFilter {
            repos: ["infra".to_string()].into(),
            ..FleetFilter::default()
        },
        FleetFilter {
            text: "no such node".into(),
            ..FleetFilter::default()
        },
    ];
    for filter in filters {
        assert!(!filter.hidden(&s).contains(&NodeId::queen()), "{filter:?}");
    }
    assert_eq!(kind_counts(&s).get(&NodeKind::Queen), None);
    assert_eq!(state_counts(&s).values().sum::<usize>(), 6);
    assert_eq!(repo_counts(&s).values().sum::<usize>(), 6);
}

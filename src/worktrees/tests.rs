use std::process::Command;

use serde_json::{Value, json};

use super::*;
use crate::orca::WorkerResource;
use crate::orca::spawn::PushTarget;

/// Facts for a worktree with every check passed: merged into `main`, nothing running in it.
fn clean() -> Facts {
    Facts {
        is_main: false,
        pinned: false,
        parked: false,
        exists: true,
        age_ms: Some(3 * 24 * 60 * 60 * 1000),
        terminals: Vec::new(),
        processes: Vec::new(),
        dispatches: Vec::new(),
        git: Ok(GitFacts {
            branch: Some("gylleus/done".into()),
            dirty: Vec::new(),
            merged_into: Some("main".into()),
            on_remote: None,
            ahead: Some(2),
            ..GitFacts::default()
        }),
        pr: PrLookup::None,
    }
}

fn git_mut(f: &mut Facts) -> &mut GitFacts {
    f.git.as_mut().unwrap()
}

fn unmerged(f: &mut Facts) {
    let g = git_mut(f);
    g.merged_into = None;
    g.ahead = Some(3);
}

fn kept_for(f: &Facts, needle: &str) {
    let v = decide(f);
    assert!(!v.remove, "expected keep, got {v:?}");
    assert!(
        v.reasons.iter().any(|r| r.contains(needle)),
        "no reason containing {needle:?} in {:?}",
        v.reasons
    );
}

#[test]
fn merged_and_idle_is_removed() {
    let v = decide(&clean());
    assert!(v.remove);
    assert_eq!(v.reasons, vec!["merged into main".to_string()]);
}

#[test]
fn nothing_beyond_the_base_is_removed() {
    let mut f = clean();
    git_mut(&mut f).ahead = Some(0);
    assert_eq!(
        decide(&f).reasons,
        vec!["everything is in main".to_string()]
    );
}

#[test]
fn uncommitted_changes_keep_it() {
    let mut f = clean();
    git_mut(&mut f).dirty = vec![" M src/a.rs".into(), "?? notes.md".into()];
    kept_for(&f, "2 uncommitted changes (M src/a.rs, ?? notes.md)");
}

#[test]
fn unmerged_commits_keep_it() {
    let mut f = clean();
    unmerged(&mut f);
    kept_for(&f, "3 commits not merged into the base");
}

/// Unmerged commits pushed somewhere still keep it without a merged or closed PR.
#[test]
fn pushed_but_unmerged_without_a_pr_is_kept() {
    let mut f = clean();
    unmerged(&mut f);
    git_mut(&mut f).on_remote = Some("origin/gylleus/x".into());
    kept_for(&f, "not merged into the base");
}

#[test]
fn open_pr_keeps_it_even_when_merged() {
    let mut f = clean();
    f.pr = PrLookup::Found {
        number: 12,
        state: PrState::Open,
    };
    kept_for(&f, "PR #12 is open");
}

#[test]
fn unknown_pr_state_keeps_it() {
    let mut f = clean();
    f.pr = PrLookup::Unknown("gh pr list: not logged in".into());
    kept_for(&f, "PR state unknown (gh pr list: not logged in)");
}

/// A squash-merged PR leaves HEAD outside main: removable once HEAD is on a remote.
#[test]
fn squash_merged_pr_with_pushed_head_is_removed() {
    let mut f = clean();
    unmerged(&mut f);
    git_mut(&mut f).on_remote = Some("origin/gylleus/x".into());
    f.pr = PrLookup::Found {
        number: 7,
        state: PrState::Merged,
    };
    let v = decide(&f);
    assert!(v.remove, "{v:?}");
    assert_eq!(
        v.reasons,
        vec!["PR #7 merged; HEAD is on origin/gylleus/x".to_string()]
    );
}

#[test]
fn closed_pr_with_pushed_head_is_removed() {
    let mut f = clean();
    unmerged(&mut f);
    git_mut(&mut f).on_remote = Some("origin/x".into());
    f.pr = PrLookup::Found {
        number: 8,
        state: PrState::Closed,
    };
    assert_eq!(
        decide(&f),
        Verdict {
            remove: true,
            reasons: vec!["PR #8 closed; HEAD is on origin/x".into()],
            ..Verdict::default()
        }
    );
}

/// Commits made after the PR merged exist only locally.
#[test]
fn merged_pr_with_local_only_commits_is_kept() {
    let mut f = clean();
    unmerged(&mut f);
    f.pr = PrLookup::Found {
        number: 7,
        state: PrState::Merged,
    };
    kept_for(&f, "3 commits not on any remote");
}

/// Unmerged, local-only commits: the squash-aware checks all came back empty.
fn local_only(f: &mut Facts) {
    unmerged(f);
    f.pr = PrLookup::None;
}

#[test]
fn rebase_merged_commits_are_removed() {
    let mut f = clean();
    local_only(&mut f);
    git_mut(&mut f).patches_in = Some("origin/main".into());
    let v = decide(&f);
    assert!(v.remove, "{v:?}");
    assert_eq!(v.reasons, vec!["every commit's patch is in origin/main"]);
}

#[test]
fn squash_merged_content_is_removed() {
    let mut f = clean();
    local_only(&mut f);
    git_mut(&mut f).content_in = Some("origin/main".into());
    let v = decide(&f);
    assert!(v.remove, "{v:?}");
    assert_eq!(
        v.reasons,
        vec!["its changes are already in origin/main (squash-merged)"]
    );
}

/// The branch was deleted after the PR merged; GitHub still has HEAD under the PR. A detached
/// HEAD gets the same check.
#[test]
fn head_inside_a_merged_pr_is_removed_even_detached() {
    let mut f = clean();
    local_only(&mut f);
    let g = git_mut(&mut f);
    g.merged_pr_head = Some(150);
    g.branch = None;
    let v = decide(&f);
    assert!(v.remove, "{v:?}");
    assert_eq!(v.reasons, vec!["HEAD is in merged PR #150's head"]);
}

/// Content already in the base doesn't override an open PR or unknown PR state.
#[test]
fn squash_checks_never_override_an_open_or_unknown_pr() {
    let mut f = clean();
    local_only(&mut f);
    git_mut(&mut f).content_in = Some("main".into());
    f.pr = PrLookup::Found {
        number: 3,
        state: PrState::Open,
    };
    kept_for(&f, "PR #3 is open");
    f.pr = PrLookup::Unknown("gh: offline".into());
    kept_for(&f, "PR state unknown");
}

/// A commit that exists nowhere else keeps the worktree.
#[test]
fn local_only_commit_keeps_it() {
    let mut f = clean();
    local_only(&mut f);
    kept_for(&f, "3 commits not merged into the base");
}

#[test]
fn notes_and_logs_alone_are_saved_then_removed() {
    let mut f = clean();
    let g = git_mut(&mut f);
    g.notes = vec!["PLAN.md".into(), "DIAGNOSIS.md".into()];
    g.logs = vec!["tsall.log".into()];
    let v = decide(&f);
    assert!(v.remove, "{v:?}");
    assert_eq!(
        v.reasons,
        vec![
            "merged into main",
            "first saves 2 notes (PLAN.md, DIAGNOSIS.md) and deletes 1 log"
        ]
    );
    assert_eq!(v.notes, vec!["PLAN.md", "DIAGNOSIS.md"]);
    assert_eq!(v.logs, vec!["tsall.log"]);
}

/// Notes don't make unmerged work removable.
#[test]
fn notes_on_unmerged_work_keep_it_and_are_not_planned() {
    let mut f = clean();
    local_only(&mut f);
    git_mut(&mut f).notes = vec!["PLAN.md".into()];
    let v = decide(&f);
    assert!(!v.remove);
    assert!(v.notes.is_empty());
}

#[test]
fn other_untracked_files_keep_it_with_their_size() {
    let mut f = clean();
    let g = git_mut(&mut f);
    g.notes = vec!["DEMO-READINESS.md".into()];
    g.untracked = vec![
        ("demo-check/".into(), 139 * 1024 * 1024),
        ("probe.py".into(), 2048),
    ];
    kept_for(
        &f,
        "untracked files need a decision: demo-check/ (139 MB), probe.py (2.0 KB)",
    );
}

#[test]
fn status_splits_notes_logs_and_the_rest() {
    let split = split_status(
        " M src/a.rs\n?? DIAGNOSIS.md\n?? docs/Answer-2.MD\n?? tsall.log\n?? demo-check/x.jsonl\n\
         ?? \"we\\\"ird.md\"\n?? .beads/issues.jsonl\n",
    );
    assert_eq!(split.dirty, vec![" M src/a.rs"]);
    assert_eq!(split.notes, vec!["DIAGNOSIS.md", "docs/Answer-2.MD"]);
    assert_eq!(split.logs, vec!["tsall.log"]);
    assert_eq!(split.other, vec!["demo-check/x.jsonl", "\"we\\\"ird.md\""]);
}

#[test]
fn untracked_files_group_by_top_folder() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("demo-check/kv")).unwrap();
    fs::write(dir.path().join("demo-check/a.jsonl"), "1234").unwrap();
    fs::write(dir.path().join("demo-check/kv/b.txt"), "12").unwrap();
    fs::write(dir.path().join("probe.py"), "123456789").unwrap();
    let paths = ["demo-check/a.jsonl", "demo-check/kv/b.txt", "probe.py"].map(String::from);
    assert_eq!(
        group_untracked(dir.path(), &paths),
        vec![("probe.py".to_string(), 9), ("demo-check/".to_string(), 6)]
    );
}

#[test]
fn save_notes_copies_then_deletes_notes_and_logs() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    let dest = tmp.path().join("notes/recommendations/rperf");
    fs::create_dir_all(wt.join("docs")).unwrap();
    fs::write(wt.join("PLAN.md"), "plan").unwrap();
    fs::write(wt.join("docs/REVIEW-1.md"), "review").unwrap();
    fs::write(wt.join("run.log"), "log").unwrap();
    // A different PLAN.md from an earlier save is kept; the new one gets a suffix.
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("PLAN.md"), "older plan").unwrap();

    let notes = vec!["PLAN.md".to_string(), "docs/REVIEW-1.md".to_string()];
    save_notes(&wt, &dest, &notes, &["run.log".to_string()]).unwrap();
    assert_eq!(
        fs::read_to_string(dest.join("PLAN.md")).unwrap(),
        "older plan"
    );
    assert_eq!(fs::read_to_string(dest.join("PLAN-2.md")).unwrap(), "plan");
    assert_eq!(
        fs::read_to_string(dest.join("docs/REVIEW-1.md")).unwrap(),
        "review"
    );
    assert!(!wt.join("PLAN.md").exists());
    assert!(!wt.join("docs/REVIEW-1.md").exists());
    assert!(!wt.join("run.log").exists());

    // Saving the same note again doesn't duplicate it.
    fs::write(wt.join("PLAN.md"), "plan").unwrap();
    save_notes(&wt, &dest, &["PLAN.md".to_string()], &[]).unwrap();
    assert!(!dest.join("PLAN-3.md").exists());
}

#[test]
fn save_notes_refuses_paths_outside_the_worktree_and_keeps_everything() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    fs::create_dir_all(&wt).unwrap();
    fs::write(wt.join("PLAN.md"), "plan").unwrap();
    let err = save_notes(
        &wt,
        &tmp.path().join("dest"),
        &["PLAN.md".to_string()],
        &["../x.log".to_string()],
    )
    .unwrap_err();
    assert!(err.contains("unsafe path"), "{err}");
    assert!(wt.join("PLAN.md").exists());
    // A missing note fails before anything is deleted.
    fs::write(wt.join("a.log"), "x").unwrap();
    assert!(
        save_notes(
            &wt,
            &tmp.path().join("dest"),
            &["PLAN.md".to_string(), "GONE.md".to_string()],
            &["a.log".to_string()]
        )
        .is_err()
    );
    assert!(wt.join("PLAN.md").exists());
    assert!(wt.join("a.log").exists());
}

#[test]
fn du_breakdown_finds_the_outermost_build_dirs() {
    let text = "9000000\t/w/rperf/target\n\
                100\t/w/rperf/src\n\
                300000\t/w/rperf/ts/node_modules/x/node_modules\n\
                800000\t/w/rperf/ts/node_modules\n\
                200000\t/w/rperf/ts/.yarn\n\
                1200000\t/w/rperf/.venv\n\
                5\t/w/rperf-other/target\n\
                11300000\t/w/rperf\n";
    let b = parse_du_breakdown(Path::new("/w/rperf"), text).unwrap();
    assert_eq!(b.total, 11_300_000 * 1024);
    let found: Vec<(&str, u64)> = b
        .build
        .iter()
        .map(|d| (d.path.as_str(), d.bytes / 1024))
        .collect();
    assert_eq!(
        found,
        vec![
            ("target", 9_000_000),
            (".venv", 1_200_000),
            ("ts/node_modules", 800_000),
            ("ts/.yarn", 200_000)
        ]
    );
    assert_eq!(parse_du_breakdown(Path::new("/w/rperf"), ""), None);
}

#[test]
fn kept_entries_show_their_size_and_build_output() {
    let e = PlanEntry {
        bytes: Some(12 * 1024 * 1024 * 1024),
        build: vec![
            BuildDir {
                path: "target".into(),
                bytes: 9 * 1024 * 1024 * 1024,
            },
            BuildDir {
                path: "apps/x/node_modules".into(),
                bytes: 44 * 1024,
            },
        ],
        ..PlanEntry::default()
    };
    assert_eq!(e.size_text(), "12.0 GB: target 9.0 GB");
    let e = PlanEntry {
        repo: "recommendations".into(),
        name: "recs-core-fewer-docs".into(),
        path: "/private/tmp/rperf".into(),
        ..PlanEntry::default()
    };
    assert_eq!(
        e.notes_dir(Path::new("/n")),
        Path::new("/n/recommendations/rperf")
    );
}

#[test]
fn live_terminal_keeps_it() {
    let mut f = clean();
    f.terminals = vec!["✳ worker".into()];
    kept_for(&f, "live terminal: ✳ worker");
}

#[test]
fn process_inside_keeps_it() {
    let mut f = clean();
    f.processes = vec!["claude (42)".into()];
    kept_for(&f, "process running in it: claude (42)");
    f.processes = (1..=5).map(|i| format!("node ({i})")).collect();
    kept_for(&f, "node (1), node (2), node (3), +2 more");
}

#[test]
fn active_dispatch_keeps_it() {
    let mut f = clean();
    f.dispatches = vec!["ctx_1".into()];
    kept_for(&f, "active dispatch: ctx_1");
}

#[test]
fn main_pinned_and_young_worktrees_are_kept() {
    let mut f = clean();
    f.is_main = true;
    kept_for(&f, "main checkout");
    let mut f = clean();
    f.pinned = true;
    kept_for(&f, "pinned in Orca");
    let mut f = clean();
    f.age_ms = Some(10 * 60 * 1000);
    git_mut(&mut f).ahead = Some(0);
    kept_for(&f, "created 10 min ago");
}

#[test]
fn age_gate_only_protects_empty_or_unknown_branches() {
    let mut f = clean();
    f.age_ms = Some(10 * 60 * 1000);
    assert!(decide(&f).remove, "finished work needs no grace period");
    git_mut(&mut f).ahead = None;
    kept_for(&f, "created 10 min ago");
    git_mut(&mut f).ahead = Some(0);
    kept_for(&f, "created 10 min ago");
}

#[test]
fn unreadable_git_or_missing_dir_keeps_it() {
    let mut f = clean();
    f.git = Err("no HEAD".into());
    kept_for(&f, "git state unreadable: no HEAD");
    let mut f = clean();
    f.exists = false;
    kept_for(&f, "directory is missing");
}

/// Every reason to keep is listed, not just the first.
#[test]
fn all_keep_reasons_are_listed() {
    let mut f = clean();
    unmerged(&mut f);
    f.terminals = vec!["t".into()];
    git_mut(&mut f).dirty = vec![" M a".into()];
    assert_eq!(decide(&f).reasons.len(), 3);
}

#[test]
fn beads_export_is_not_uncommitted_work() {
    assert_eq!(
        dirty_lines(" M .beads/issues.jsonl\n?? scratch.txt\n\n"),
        vec!["?? scratch.txt".to_string()]
    );
}

#[test]
fn lsof_output_parses_and_matches_inside_dirs() {
    let text = "p100\nccargo\nn/w/myrstack/a/target\np200\nczsh\nn/w/myrstack/ab\np300\ncclaude\nn/w/myrstack/a\n";
    let list = parse_lsof_cwd(text);
    assert_eq!(list.len(), 3);
    assert_eq!(list[1].command, "zsh");
    // `/w/myrstack/ab` is not inside `/w/myrstack/a`; our own pid is skipped.
    assert_eq!(
        processes_inside(
            Path::new("/w/myrstack/a"),
            &list,
            300,
            &HashSet::new(),
            false
        ),
        vec!["cargo (100)".to_string()]
    );
}

/// An orphaned watch loop elsewhere whose command line names the worktree doesn't hold it;
/// one whose working directory is inside does, orphaned or not.
#[test]
fn only_the_working_directory_holds_a_worktree() {
    let list = vec![
        ProcessCwd {
            pid: 66845,
            command: "zsh".into(),
            cwd: "/Users/k/repo/depict/depict".into(),
        },
        ProcessCwd {
            pid: 7,
            command: "sleep".into(),
            cwd: "/w/recommendations/serving-field/ts".into(),
        },
    ];
    assert_eq!(
        processes_inside(
            Path::new("/w/recommendations/serving-field"),
            &list,
            1,
            &HashSet::new(),
            false
        ),
        vec!["sleep (7)".to_string()]
    );
}

fn worktree(id: &str, path: &str) -> Worktree {
    Worktree {
        id: id.into(),
        path: path.into(),
        branch: Some("refs/heads/gylleus/x".into()),
        ..Worktree::default()
    }
}

#[test]
fn terminals_and_dispatches_match_their_worktree() {
    let w = worktree("r::/w/a", "/w/a");
    let terminals = vec![
        Terminal {
            handle: "term_1".into(),
            worktree_id: Some("r::/w/a".into()),
            title: Some("agent".into()),
            ..Terminal::default()
        },
        Terminal {
            handle: "term_2".into(),
            worktree_path: Some("/w/a/sub".into()),
            ..Terminal::default()
        },
        Terminal {
            handle: "term_3".into(),
            worktree_path: Some("/w/ab".into()),
            ..Terminal::default()
        },
    ];
    assert_eq!(terminals_in(&w, &terminals), vec!["agent", "term_2"]);

    let worker = |id: &str, wt: &str, state: Value| -> Worker {
        let mut v = json!({"dispatchId": id, "resource": {"worktreeId": wt}});
        v.as_object_mut()
            .unwrap()
            .extend(state.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    };
    let workers = vec![
        worker(
            "live",
            "r::/w/a",
            json!({"workerState": "ready", "dispatchStatus": "dispatched"}),
        ),
        worker(
            "done",
            "r::/w/a",
            json!({"workerState": "succeeded", "dispatchStatus": "completed", "terminalState": "released"}),
        ),
        worker(
            "elsewhere",
            "r::/w/b",
            json!({"dispatchStatus": "dispatched"}),
        ),
    ];
    assert_eq!(dispatches_in(&w, &workers), vec!["live"]);
    // An active terminal resource counts even if the dispatch settled.
    let mut held = workers[1].clone();
    held.terminal_state = Some(WorkerTerminalState::Active);
    held.resource = Some(WorkerResource {
        worktree_id: Some("r::/w/a".into()),
        ..WorkerResource::default()
    });
    assert!(worker_is_live(&held));
}

#[test]
fn pr_lookup_prefers_linked_then_open_then_newest() {
    let rows = |v: Value| -> Result<Vec<PrRow>, String> { Ok(serde_json::from_value(v).unwrap()) };
    let prs = rows(json!([
        {"number": 5, "state": "MERGED", "headRefName": "gylleus/x"},
        {"number": 9, "state": "CLOSED", "headRefName": "gylleus/x"},
        {"number": 3, "state": "OPEN", "headRefName": "gylleus/x"},
        {"number": 11, "state": "MERGED", "headRefName": "gylleus/remote-name"},
    ]));
    let mut w = worktree("r::/w/a", "/w/a");
    assert_eq!(
        pr_for(&w, Some("gylleus/x"), &prs),
        PrLookup::Found {
            number: 3,
            state: PrState::Open
        }
    );
    assert_eq!(pr_for(&w, Some("other"), &prs), PrLookup::None);

    w.push_target = Some(PushTarget {
        remote_name: Some("origin".into()),
        branch_name: Some("gylleus/remote-name".into()),
    });
    assert_eq!(
        pr_for(&w, Some("local-name"), &prs),
        PrLookup::Found {
            number: 11,
            state: PrState::Merged
        }
    );

    w.linked_pr = Some(json!(5));
    assert_eq!(
        pr_for(&w, Some("gylleus/x"), &prs),
        PrLookup::Found {
            number: 5,
            state: PrState::Merged
        }
    );
    w.linked_pr = Some(json!(404));
    assert!(matches!(pr_for(&w, None, &prs), PrLookup::Unknown(_)));
    assert!(matches!(
        pr_for(&w, None, &Err("gh: offline".into())),
        PrLookup::Unknown(e) if e == "gh: offline"
    ));
}

#[test]
fn base_branches_start_with_the_worktree_base_ref() {
    let exists = |r: &str| r != "master" && r != "origin/master";
    assert_eq!(
        base_branches(Some("refs/remotes/origin/main"), exists),
        vec!["origin/main", "origin/HEAD", "main"]
    );
    assert_eq!(base_branches(None, |r| r == "main"), vec!["main"]);
}

#[test]
fn census_counts_linked_worktrees_per_repo() {
    let mut list = vec![
        worktree("a::/x", "/home/orca/workspaces/myrstack/one"),
        worktree("a::/y", "/home/orca/workspaces/myrstack/two"),
        worktree("b::/z", "/tmp/elsewhere"),
    ];
    list[2].project_id = Some("github:depict-org/depict".into());
    let mut main = worktree("a::/r", "/repo/myrstack");
    main.is_main_worktree = true;
    list.push(main);
    assert_eq!(
        census(&list),
        Census {
            total: 3,
            per_repo: vec![("myrstack".into(), 2), ("depict".into(), 1)]
        }
    );
}

#[test]
fn orphan_dirs_are_the_unlisted_ones() {
    let root = tempfile::tempdir().unwrap();
    for d in [
        "repo/listed",
        "repo/stray",
        "other/gone",
        "repo/.orca-worktree-trash",
    ] {
        fs::create_dir_all(root.path().join(d)).unwrap();
    }
    let listed = root.path().join("repo/listed");
    let list = vec![worktree("r::x", listed.to_str().unwrap())];
    let orphans = orphan_dirs(root.path(), &list);
    assert_eq!(
        orphans,
        vec![
            root.path().join("other/gone"),
            root.path().join("repo/stray")
        ]
    );
}

#[test]
fn bytes_format_compactly() {
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(1536), "1.5 KB");
    assert_eq!(format_bytes(250 * 1024 * 1024), "250 MB");
    assert_eq!(
        format_bytes(12 * 1024 * 1024 * 1024 + 300 * 1024 * 1024),
        "12.3 GB"
    );
}

#[test]
fn plan_text_lists_removals_and_keeps() {
    let entry = |name: &str, remove: bool, reason: &str, bytes: Option<u64>| PlanEntry {
        id: format!("r::{name}"),
        name: name.into(),
        repo: "myrstack".into(),
        path: format!("/w/{name}"),
        branch: None,
        bytes,
        verdict: Verdict {
            remove,
            reasons: vec![reason.into()],
            ..Verdict::default()
        },
        ..PlanEntry::default()
    };
    let plan = Plan {
        entries: vec![
            entry(
                "old",
                true,
                "merged into main",
                Some(2 * 1024 * 1024 * 1024),
            ),
            entry("live", false, "live terminal: t", None),
        ],
        orphans: vec!["/w/stray".into()],
        evaluated_at_ms: 0,
    };
    assert_eq!(plan.freed_bytes(), 2 * 1024 * 1024 * 1024);
    assert_eq!(
        plan.text(),
        "2 linked worktrees: 1 to remove (frees 2.0 GB), 1 kept (holding 0 B).\n\
         Remove:\n- myrstack/old (2.0 GB): merged into main\n\
         Keep:\n- myrstack/live: live terminal: t\n\
         Directories without git registration (not touched): /w/stray"
    );
}

#[test]
fn rm_never_forces() {
    let args = worktree_rm_args("r::/w/a");
    assert_eq!(args, vec!["worktree", "rm", "--worktree=id:r::/w/a"]);
    assert!(!args.iter().any(|a| a.contains("force")));
}

#[test]
fn execution_summary_counts_outcomes() {
    let removal = |outcome: Outcome| Removal {
        id: "i".into(),
        repo: "r".into(),
        name: "n".into(),
        path: "/p".into(),
        branch: None,
        bytes: Some(1024 * 1024),
        reason: "merged into main".into(),
        notes_saved_to: None,
        outcome,
        at_ms: 0,
    };
    let text = summary(&[
        removal(Outcome::Removed),
        removal(Outcome::Removed),
        removal(Outcome::Skipped {
            reasons: vec!["live terminal: t".into()],
        }),
        removal(Outcome::Failed { error: "x".into() }),
    ]);
    assert_eq!(
        text,
        "Removed 2 worktrees, freed 2.0 MB, 1 kept on re-check, 1 failed"
    );
    let line = serde_json::to_value(removal(Outcome::Skipped {
        reasons: vec!["a".into()],
    }))
    .unwrap();
    assert_eq!(line["outcome"], "skipped");
    assert_eq!(line["reasons"], json!(["a"]));
}

// ---------------------------------------------------------------------------
// git_facts against a real repository

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit(dir: &Path, file: &str) {
    fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
}

#[test]
fn git_facts_reads_merged_ahead_and_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "base");
    let wt = tmp.path().join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            wt.to_str().unwrap(),
        ],
    );

    // Fresh worktree: nothing beyond main.
    let f = git_facts(&wt, None).unwrap();
    assert_eq!(f.branch.as_deref(), Some("feature"));
    assert_eq!(f.merged_into.as_deref(), Some("main"));
    assert_eq!(f.ahead, Some(0));
    assert!(f.dirty.is_empty());

    // A commit on the branch: not merged, not on any remote.
    commit(&wt, "work");
    let f = git_facts(&wt, None).unwrap();
    assert_eq!(f.merged_into, None);
    assert_eq!(f.ahead, Some(1));
    assert_eq!(f.on_remote, None);
    assert!(
        !decide(&Facts {
            git: Ok(f),
            ..clean()
        })
        .remove
    );

    // Untracked files inside new directories are listed: notes, logs and the rest by folder.
    fs::create_dir(wt.join("notes")).unwrap();
    fs::write(wt.join("notes/n.md"), "x").unwrap();
    fs::write(wt.join("notes/data.csv"), "12345").unwrap();
    fs::write(wt.join("run.log"), "x").unwrap();
    let f = git_facts(&wt, None).unwrap();
    assert!(f.dirty.is_empty());
    assert_eq!(f.notes, vec!["notes/n.md".to_string()]);
    assert_eq!(f.logs, vec!["run.log".to_string()]);
    assert_eq!(f.untracked, vec![("notes/".to_string(), 5)]);
    fs::remove_dir_all(wt.join("notes")).unwrap();
    fs::remove_file(wt.join("run.log")).unwrap();

    // Merged into main: removable.
    git(&repo, &["merge", "-q", "--no-ff", "-m", "merge", "feature"]);
    let f = git_facts(&wt, None).unwrap();
    assert_eq!(f.merged_into.as_deref(), Some("main"));
    assert!(
        decide(&Facts {
            git: Ok(f),
            ..clean()
        })
        .remove
    );
}

#[test]
fn sweep_marker_goes_stale() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(SWEEP_FILE);
    assert_eq!(running_sweep(&path, 0), None);
    let marker = SweepMarker {
        started_at_ms: 1_000,
        updated_at_ms: 2_000,
        total: 3,
        done: 1,
        current: Some("old".into()),
        current_id: Some("repo::/x/old".into()),
    };
    write_marker(&path, &marker);
    assert_eq!(running_sweep(&path, 3_000), Some(marker));
    assert_eq!(running_sweep(&path, 2_000 + 10 * 60 * 1000), None);
}

// ---------------------------------------------------------------------------
// Squash-aware git checks against a real repository

/// A repo on `main` with one base commit and a worktree on `feature`.
fn repo_with_feature() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "base");
    let wt = tmp.path().join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            wt.to_str().unwrap(),
        ],
    );
    (tmp, repo, wt)
}

fn rev(dir: &Path, r: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", r])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn git_facts_sees_a_squash_merge() {
    let (_tmp, repo, wt) = repo_with_feature();
    commit(&wt, "one");
    commit(&wt, "two");
    commit(&repo, "unrelated");
    git(&repo, &["merge", "-q", "--squash", "feature"]);
    git(&repo, &["commit", "-q", "-m", "squash"]);
    let f = git_facts(&wt, None).unwrap();
    assert_eq!(f.merged_into, None);
    assert_eq!(f.patches_in, None);
    assert_eq!(f.content_in.as_deref(), Some("main"));
    assert!(
        decide(&Facts {
            git: Ok(f),
            ..clean()
        })
        .remove
    );
}

#[test]
fn git_facts_sees_cherry_picked_commits() {
    let (_tmp, repo, wt) = repo_with_feature();
    commit(&wt, "one");
    commit(&repo, "unrelated");
    git(&repo, &["cherry-pick", "feature"]);
    let f = git_facts(&wt, None).unwrap();
    assert_eq!(f.merged_into, None);
    assert_eq!(f.patches_in.as_deref(), Some("main"));
}

/// A commit that exists nowhere else: every check comes back empty and the worktree is kept,
/// even when a merged PR exists for an earlier commit and main has part of the work.
#[test]
fn git_facts_keeps_a_commit_that_exists_nowhere_else() {
    let (_tmp, repo, wt) = repo_with_feature();
    commit(&wt, "one");
    let pr_head = rev(&wt, "HEAD");
    git(&repo, &["merge", "-q", "--squash", "feature"]);
    git(&repo, &["commit", "-q", "-m", "squash"]);
    commit(&wt, "after-merge");
    let mut f = git_facts(&wt, None).unwrap();
    assert_eq!(f.merged_into, None);
    assert_eq!(f.patches_in, None);
    assert_eq!(f.content_in, None);
    let prs = vec![PrRow {
        number: 9,
        state: "MERGED".into(),
        head_ref_name: "feature".into(),
        head_ref_oid: pr_head,
    }];
    f.merged_pr_head = merged_pr_containing(&wt, &f.head, &prs);
    assert_eq!(f.merged_pr_head, None);
    let v = decide(&Facts {
        git: Ok(f),
        pr: PrLookup::Found {
            number: 9,
            state: PrState::Merged,
        },
        ..clean()
    });
    assert!(!v.remove, "{v:?}");
    assert!(v.reasons.iter().any(|r| r.contains("not on any remote")));
}

#[test]
fn merged_pr_head_contains_head_or_its_ancestors() {
    let (_tmp, _repo, wt) = repo_with_feature();
    commit(&wt, "one");
    let first = rev(&wt, "HEAD");
    commit(&wt, "two");
    let second = rev(&wt, "HEAD");
    let row = |number: u64, state: &str, oid: &str| PrRow {
        number,
        state: state.into(),
        head_ref_name: "gone".into(),
        head_ref_oid: oid.into(),
    };
    let missing = "0123456789abcdef0123456789abcdef01234567";
    let prs = vec![
        row(1, "OPEN", &second),
        row(2, "MERGED", missing),
        row(3, "MERGED", &second),
    ];
    // HEAD is the PR head.
    assert_eq!(merged_pr_containing(&wt, &second, &prs), Some(3));
    // A detached HEAD on an earlier commit of the PR.
    git(&wt, &["checkout", "-q", "--detach", &first]);
    assert_eq!(merged_pr_containing(&wt, &first, &prs), Some(3));
    // Only an open PR or a head missing locally: no proof.
    assert_eq!(merged_pr_containing(&wt, &first, &prs[..2]), None);
    // A commit past the PR head isn't in it.
    git(&wt, &["checkout", "-q", "feature"]);
    commit(&wt, "three");
    assert_eq!(merged_pr_containing(&wt, &rev(&wt, "HEAD"), &prs), None);
}

#[test]
fn a_parked_agents_worktree_is_kept() {
    let v = decide(&Facts {
        parked: true,
        ..clean()
    });
    assert!(!v.remove);
    assert!(v.reasons.iter().any(|r| r.contains("parked")));
}

#[test]
fn targeted_prs_fill_old_number_and_branch_holes_and_deduplicate() {
    let row = |number, branch: &str, state: &str| PrRow {
        number,
        state: state.into(),
        head_ref_name: branch.into(),
        head_ref_oid: String::new(),
    };
    let mut linked = worktree("r::/old", "/old");
    linked.linked_pr = Some(json!(8312));
    linked.branch = Some("old".into());
    let mut open = worktree("r::/open", "/open");
    open.branch = Some("local".into());
    open.push_target = Some(PushTarget {
        branch_name: Some("remote".into()),
        ..PushTarget::default()
    });
    let mut rows = vec![row(9400, "new", "MERGED")];
    let mut calls = Vec::new();
    let errors = complete_prs(&mut rows, &[&linked, &open], |q| {
        calls.push(q.clone());
        Ok(match q {
            PrQuery::Number(8312) => vec![row(8312, "old", "MERGED")],
            PrQuery::Head(b) if b == "old" => vec![row(8312, "old", "MERGED")],
            PrQuery::Head(b) if b == "remote" => vec![row(8922, "remote", "OPEN")],
            PrQuery::Head(b) if b == "local" => vec![],
            _ => panic!("unexpected query {q:?}"),
        })
    });
    assert!(errors.is_empty());
    assert!(calls.contains(&PrQuery::Number(8312)));
    assert!(calls.contains(&PrQuery::Head("remote".into())));
    assert_eq!(rows.iter().filter(|p| p.number == 8312).count(), 1);
    let prs = Ok(rows);
    assert_eq!(
        pr_for(&linked, linked.branch_name(), &prs),
        PrLookup::Found {
            number: 8312,
            state: PrState::Merged
        }
    );
    let mut f = clean();
    f.pr = pr_for(&open, open.branch_name(), &prs);
    kept_for(&f, "PR #8922 is open");
}

#[test]
fn targeted_failure_is_unknown_only_for_affected_worktrees() {
    let a = worktree("r::/a", "/a");
    let b = worktree("r::/b", "/b");
    let mut rows = Vec::new();
    let mut calls = 0;
    let errors = complete_prs(&mut rows, &[&a, &b], |_| {
        calls += 1;
        Err("offline".into())
    });
    assert_eq!(calls, 1, "failed branch query is cached");
    let ctx = Context {
        terminals: vec![],
        workers: vec![],
        processes: vec![],
        daemon_pids: HashSet::new(),
        parked: BTreeSet::new(),
        prs: HashMap::from([("r".into(), Ok(rows))]),
        pr_errors: errors,
        now_ms: 0,
    };
    assert_eq!(ctx.facts(&a).pr, PrLookup::Unknown("offline".into()));
    assert_eq!(ctx.facts(&b).pr, PrLookup::Unknown("offline".into()));
    let mut unrelated = worktree("r::/c", "/c");
    unrelated.branch = None;
    assert_eq!(ctx.facts(&unrelated).pr, PrLookup::None);
}

#[test]
fn daemon_descendants_are_ignored_only_without_an_orca_terminal() {
    let ps = "52681 1 /Users/k/.codex/packages/app-server-daemon/releases/v/bin/codex app-server daemon pid-update-loop\n\
        78465 52681 /bin/codex app-server --listen unix://\n\
        80163 78465 /bin/codex-code-mode-host\n\
        100 78465 uv run mcp\n101 100 python mcp.py\n102 1 node server.js\n\
        103 1 /bin/zsh -c echo codex app-server\n104 999 python orphan.py\n105 106 loop\n106 105 loop\n";
    let daemons = codex_daemon_pids(ps);
    assert_eq!(daemons, HashSet::from([52681, 78465, 80163, 100, 101]));
    let processes =
        parse_lsof_cwd("p52681\nccodex\nn/w/a\np101\ncpython\nn/w/a/sub\np102\ncnode\nn/w/a\n");
    let mut f = clean();
    f.processes = processes_inside(Path::new("/w/a"), &processes[..2], 999, &daemons, false);
    assert!(decide(&f).remove);
    f.terminals = vec!["agent".into()];
    f.processes = processes_inside(Path::new("/w/a"), &processes[..2], 999, &daemons, true);
    kept_for(&f, "python (101)");
    f.terminals.clear();
    f.processes = processes_inside(Path::new("/w/a"), &processes, 999, &daemons, false);
    assert_eq!(f.processes, vec!["node (102)"]);
    kept_for(&f, "node (102)");
    f.processes = processes_inside(
        Path::new("/w/a"),
        &processes[..2],
        999,
        &HashSet::new(),
        false,
    );
    kept_for(&f, "codex (52681)");
}

#[test]
fn squash_into_matching_merged_pr_head_is_proof_but_divergence_is_not() {
    let (_tmp, repo, wt) = repo_with_feature();
    commit(&wt, "one");
    commit(&wt, "two");
    git(&repo, &["checkout", "-q", "-b", "pr-head"]);
    git(&repo, &["merge", "-q", "--squash", "feature"]);
    git(&repo, &["commit", "-q", "-m", "squashed"]);
    let pr_head = rev(&repo, "HEAD");
    git(&repo, &["checkout", "-q", "main"]);
    let mut w = worktree("r::wt", wt.to_str().unwrap());
    w.branch = Some("feature".into());
    let mut rows = vec![PrRow {
        number: 8786,
        state: "MERGED".into(),
        head_ref_name: "feature".into(),
        head_ref_oid: pr_head,
    }];
    let head = rev(&wt, "HEAD");
    assert_eq!(merged_pr_containing(&wt, &head, &rows), None);
    let proof = merged_pr_with_content(&wt, &head, &w, Some("feature"), &rows);
    assert_eq!(proof, Some(8786));
    let mut f = clean();
    unmerged(&mut f);
    git_mut(&mut f).content_in_merged_pr = proof;
    assert_eq!(
        decide(&f).reasons,
        vec!["its changes are in merged PR #8786's head (squash-merged)"]
    );
    rows[0].head_ref_name = "unrelated".into();
    assert_eq!(
        merged_pr_with_content(&wt, &head, &w, Some("feature"), &rows),
        None
    );
    w.linked_pr = Some(json!(8786));
    assert_eq!(
        merged_pr_with_content(&wt, &head, &w, Some("feature"), &rows),
        Some(8786)
    );
    rows[0].state = "OPEN".into();
    assert_eq!(
        merged_pr_with_content(&wt, &head, &w, Some("feature"), &rows),
        None
    );
    rows[0].state = "MERGED".into();
    commit(&wt, "after-merge");
    assert_eq!(
        merged_pr_with_content(&wt, &rev(&wt, "HEAD"), &w, Some("feature"), &rows),
        None
    );
    rows[0].head_ref_oid = "0123456789abcdef0123456789abcdef01234567".into();
    assert_eq!(
        merged_pr_with_content(&wt, &head, &w, Some("feature"), &rows),
        None
    );
}

#[test]
fn git_registered_worktrees_join_the_census_and_are_never_orphans() {
    let (tmp, repo, wt) = repo_with_feature();
    let r = Repo {
        id: "r".into(),
        path: repo.to_str().unwrap().into(),
        ..Repo::default()
    };
    let root = tmp.path().join("workspaces");
    let nested = root.join("repo/unlisted with spaces");
    let direct = root.join("direct");
    fs::create_dir_all(nested.parent().unwrap()).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "nested",
            nested.to_str().unwrap(),
        ],
    );
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            direct.to_str().unwrap(),
        ],
    );
    fs::create_dir(direct.join("subfolder")).unwrap();
    fs::create_dir_all(root.join("repo/stray")).unwrap();
    let listed = vec![worktree("r::wt", wt.to_str().unwrap())];
    let all = all_worktrees(&listed, std::slice::from_ref(&r)).unwrap();
    assert_eq!(all.len(), 4);
    assert_eq!(all.iter().filter(|w| w.is_main_worktree).count(), 1);
    assert_eq!(
        all.iter()
            .filter(|w| w.path == wt.to_str().unwrap())
            .count(),
        1
    );
    assert_eq!(
        all.iter().find(|w| w.id == "r::wt").unwrap().branch_name(),
        Some("feature")
    );
    let unlisted = all
        .iter()
        .find(|w| w.path.ends_with("unlisted with spaces"))
        .unwrap();
    assert!(unlisted.id.starts_with("git:"));
    assert_eq!(unlisted.repo(), Some("r"));
    assert_eq!(unlisted.base_ref.as_deref(), Some("origin/main"));
    assert!(plan_entry(unlisted, &clean()).unlisted);
    assert_eq!(orphan_dirs(&root, &listed), vec![root.join("repo/stray")]);
    assert_eq!(orphan_dirs(&root, &all), vec![root.join("repo/stray")]);
    let terminals = vec![Terminal {
        worktree_path: Some(nested.join("sub").to_str().unwrap().into()),
        handle: "term".into(),
        ..Terminal::default()
    }];
    assert_eq!(terminals_in(unlisted, &terminals), vec!["term"]);
    let args = git_worktree_rm_args(&r, &unlisted.path);
    assert_eq!(
        args,
        vec!["-C", &r.path, "worktree", "remove", "--", &unlisted.path]
    );
    assert!(!args.iter().any(|a| a.contains("force")));
    // Exercise only a disposable repository: git removal preserves the branch and refuses dirt.
    fs::write(nested.join("untracked.txt"), "keep").unwrap();
    assert!(matches!(
        remove_unlisted(&r, &unlisted.path),
        Outcome::Failed { .. }
    ));
    assert!(nested.exists());
    fs::remove_file(nested.join("untracked.txt")).unwrap();
    assert_eq!(remove_unlisted(&r, &unlisted.path), Outcome::Removed);
    assert!(!nested.exists());
    assert!(!rev(&repo, "refs/heads/nested").is_empty());
}

#[test]
fn squash_pr_check_never_lazily_fetches_promised_blobs() {
    let (tmp, repo, wt) = repo_with_feature();
    let base = (0..30).map(|i| format!("line {i}\n")).collect::<String>();
    fs::write(repo.join("data"), &base).unwrap();
    git(&repo, &["add", "data"]);
    git(&repo, &["commit", "-q", "-m", "data"]);
    git(&wt, &["merge", "-q", "main"]);
    fs::write(wt.join("data"), base.replace("line 2\n", "feature\n")).unwrap();
    git(&wt, &["commit", "-qam", "feature"]);
    let feature_head = rev(&wt, "HEAD");
    git(&repo, &["checkout", "-qb", "pr-head"]);
    git(&repo, &["merge", "-q", "--squash", "feature"]);
    git(&repo, &["commit", "-q", "-m", "squash"]);
    fs::write(
        repo.join("data"),
        base.replace("line 2\n", "feature\n")
            .replace("line 25\n", "later\n"),
    )
    .unwrap();
    git(&repo, &["commit", "-qam", "later"]);
    let pr_head = rev(&repo, "HEAD");
    let remote = tmp.path().join("remote.git");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            "--bare",
            repo.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    git(&remote, &["config", "uploadpack.allowFilter", "true"]);
    let partial = tmp.path().join("partial");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            "--filter=blob:none",
            "--no-checkout",
            &format!("file://{}", remote.display()),
            partial.to_str().unwrap(),
        ],
    );
    let packs = || {
        fs::read_dir(partial.join(".git/objects/pack"))
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "pack"))
            .count()
    };
    let before = packs();
    let rows = vec![PrRow {
        number: 7,
        state: "MERGED".into(),
        head_ref_name: "feature".into(),
        head_ref_oid: pr_head,
    }];
    let mut w = worktree("git:partial", partial.to_str().unwrap());
    w.branch = Some("feature".into());
    assert!(
        present_commits(&partial, rows.iter().map(|p| p.head_ref_oid.as_str()))
            .contains(&rows[0].head_ref_oid)
    );
    assert_eq!(
        merged_pr_with_content(&partial, &feature_head, &w, Some("feature"), &rows),
        None
    );
    assert_eq!(
        packs(),
        before,
        "the check must not fetch missing promised blobs"
    );
}

#[test]
fn unlisted_worktree_matches_retained_dispatch_by_path() {
    let w = worktree("git:/w/a", "/w/a");
    let worker: Worker = serde_json::from_value(json!({
        "dispatchId": "live", "dispatchStatus": "dispatched",
        "resource": {"worktreeId": "r::/w/a"}
    }))
    .unwrap();
    assert_eq!(dispatches_in(&w, &[worker]), vec!["live"]);
}

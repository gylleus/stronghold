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
            reasons: vec!["PR #8 closed; HEAD is on origin/x".into()]
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
        processes_inside(Path::new("/w/myrstack/a"), &list, 300),
        vec!["cargo (100)".to_string()]
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
        },
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
        "2 linked worktrees: 1 to remove (frees 2.0 GB), 1 kept.\n\
         Remove:\n- myrstack/old (2.0 GB): merged into main\n\
         Keep:\n- myrstack/live: live terminal: t\n\
         Directories Orca doesn't list (not touched): /w/stray"
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

    // Untracked files inside new directories count as dirty.
    fs::create_dir(wt.join("notes")).unwrap();
    fs::write(wt.join("notes/n.md"), "x").unwrap();
    let f = git_facts(&wt, None).unwrap();
    assert_eq!(f.dirty, vec!["?? notes/n.md".to_string()]);
    fs::remove_dir_all(wt.join("notes")).unwrap();

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
    };
    write_marker(&path, &marker);
    assert_eq!(running_sweep(&path, 3_000), Some(marker));
    assert_eq!(running_sweep(&path, 2_000 + 10 * 60 * 1000), None);
}

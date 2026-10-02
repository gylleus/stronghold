use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use super::*;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A repo on `main` with one commit, and a worktree branch `feature` checked out in `wt`.
fn repo() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "a").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "first"]);
    let wt = tmp.path().join("wt");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            wt.to_str().unwrap(),
        ],
    );
    (tmp, wt)
}

#[test]
fn a_clean_branch_with_nothing_new_has_no_problems() {
    let (_tmp, wt) = repo();
    assert_eq!(git_problems(&wt), Vec::<String>::new());
}

#[test]
fn uncommitted_changes_are_a_problem_but_the_beads_export_is_not() {
    let (_tmp, wt) = repo();
    std::fs::create_dir(wt.join(".beads")).unwrap();
    std::fs::write(wt.join(".beads/issues.jsonl"), "{}").unwrap();
    assert_eq!(git_problems(&wt), Vec::<String>::new());
    std::fs::write(wt.join("a.txt"), "changed").unwrap();
    let problems = git_problems(&wt);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with("1 uncommitted change in"),
        "{problems:?}"
    );
    assert!(problems[0].contains("a.txt"), "{problems:?}");
}

#[test]
fn unmerged_unpushed_commits_are_a_problem_until_merged() {
    let (tmp, wt) = repo();
    std::fs::write(wt.join("b.txt"), "b").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    let problems = git_problems(&wt);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("1 commit on feature is neither merged into main nor pushed"),
        "{problems:?}"
    );
    git(
        &tmp.path().join("main"),
        &["merge", "-q", "--no-ff", "-m", "merge", "feature"],
    );
    assert_eq!(git_problems(&wt), Vec::<String>::new());
}

#[test]
fn pushed_commits_are_not_a_problem() {
    let (tmp, wt) = repo();
    let remote = tmp.path().join("remote.git");
    git(
        tmp.path(),
        &["init", "-q", "--bare", remote.to_str().unwrap()],
    );
    git(&wt, &["remote", "add", "origin", remote.to_str().unwrap()]);
    std::fs::write(wt.join("b.txt"), "b").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    git(&wt, &["push", "-q", "-u", "origin", "feature"]);
    assert_eq!(git_problems(&wt), Vec::<String>::new());
}

#[test]
fn outside_git_there_is_nothing_to_check() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(git_problems(tmp.path()), Vec::<String>::new());
}

fn finished(at_ms: i64) -> FinishedSession {
    FinishedSession {
        node_id: "term:t1".into(),
        handle: Some("t1".into()),
        label: "ant".into(),
        outcome: FinishOutcome::Done,
        summary: "did it".into(),
        pr_url: None,
        forced: Vec::new(),
        worktree_path: None,
        branch: None,
        at_ms,
    }
}

#[test]
fn the_log_survives_a_restart_and_keeps_recent_entries() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(LOG_FILE);
    let mut log = FinishLog::load(path.clone(), 0);
    log.record(finished(1_000));
    log.record(finished(50_000));
    let reloaded = FinishLog::load(path, 10_000);
    let at: Vec<i64> = reloaded.recent(60_000, 60_000).map(|f| f.at_ms).collect();
    assert_eq!(at, vec![50_000]);
    assert_eq!(reloaded.recent(200_000, 60_000).count(), 0);
}

#[test]
fn outcomes_round_trip() {
    for o in [
        FinishOutcome::Done,
        FinishOutcome::HandedOver,
        FinishOutcome::Abandoned,
    ] {
        assert_eq!(FinishOutcome::parse(o.name()), Some(o));
    }
    assert_eq!(FinishOutcome::parse("merged"), None);
}

mod fleet {
    use super::super::*;
    use crate::model::{FleetSnapshot, Node, NodeId, NodeKind, NodeState, RunSummary, TaskCounts};
    use crate::orca::{DispatchStatus, WorkerState};

    fn node(kind: NodeKind) -> Node {
        Node {
            id: NodeId("term:t1".into()),
            kind,
            label: "ant".into(),
            terminal_handle: Some("t1".into()),
            live: true,
            run_id: None,
            task_id: None,
            dispatch_id: None,
            agent: None,
            worktree_path: None,
            branch: None,
            last_output_ms: None,
            preview: None,
            state: NodeState::Idle,
            worker_state: None,
            dispatch_status: None,
            worker_terminal_state: None,
            released_at_ms: None,
        }
    }

    fn snapshot(open: usize) -> FleetSnapshot {
        let mut s = FleetSnapshot::default();
        s.runs = vec![RunSummary {
            id: "run_1".into(),
            objective: Some("ship it".into()),
            coordinator: Some(NodeId("term:t1".into())),
            task_counts: Some(TaskCounts {
                dispatched: open,
                completed: 3,
                ..TaskCounts::default()
            }),
        }];
        s
    }

    #[test]
    fn a_loose_terminal_with_nothing_open_may_leave() {
        assert_eq!(
            fleet_problems(&node(NodeKind::Terminal), &snapshot(2), &[]),
            (None, Vec::new())
        );
    }

    #[test]
    fn a_worker_reports_to_its_coordinator_instead() {
        let mut w = node(NodeKind::Worker);
        w.run_id = Some("run_1".into());
        w.task_id = Some("task_9".into());
        w.dispatch_id = Some("ctx_1".into());
        w.worker_state = Some(WorkerState::Ready);
        w.dispatch_status = Some(DispatchStatus::Dispatched);
        let (refusal, _) = fleet_problems(&w, &snapshot(0), &[]);
        let refusal = refusal.expect("refused");
        assert!(refusal.contains("run run_1 (task task_9)"), "{refusal}");
        assert!(refusal.contains("coordinator"), "{refusal}");
        // Settled: the coordinator still reuses or releases it.
        w.worker_state = Some(WorkerState::Succeeded);
        w.dispatch_status = Some(DispatchStatus::Completed);
        let refusal = fleet_problems(&w, &snapshot(0), &[]).0.expect("refused");
        assert!(refusal.contains("after worker_done"), "{refusal}");
    }

    #[test]
    fn a_coordinator_with_open_tasks_and_an_agent_with_an_open_pr_are_held() {
        let (refusal, problems) = fleet_problems(&node(NodeKind::Coordinator), &snapshot(2), &[]);
        assert_eq!(refusal, None);
        assert_eq!(
            problems,
            vec!["run ship it still has 2 open tasks: finish or cancel them first"]
        );
        assert!(
            fleet_problems(&node(NodeKind::Coordinator), &snapshot(0), &[])
                .1
                .is_empty()
        );

        let pr = "https://github.com/o/r/pull/3".to_string();
        let (_, problems) = fleet_problems(&node(NodeKind::Terminal), &snapshot(0), &[pr]);
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].starts_with("https://github.com/o/r/pull/3 is waiting for Karl's review")
        );
    }

    #[test]
    fn requests_round_trip_through_control_params() {
        let request = FinishRequest {
            summary: "done".into(),
            outcome: FinishOutcome::HandedOver,
            pr_url: Some("https://github.com/o/r/pull/3".into()),
            force: true,
            forced: vec!["dirty".into()],
            dry_run: false,
            origin: Origin {
                terminal_handle: Some("t1".into()),
                ..Origin::default()
            },
        };
        assert_eq!(
            FinishRequest::from_params(&request.to_params()).unwrap(),
            request
        );
        let empty = serde_json::json!({"summary": "  "});
        assert!(FinishRequest::from_params(&empty).is_err());
        let check = serde_json::json!({"dry_run": true});
        assert!(FinishRequest::from_params(&check).unwrap().dry_run);
    }
}

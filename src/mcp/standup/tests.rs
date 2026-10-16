use super::*;
use crate::mcp::backend::CallError;
use crate::orca::{Run, Task, TaskStatus, Timestamp};
use crate::playbook::fill::ClanFacts;

struct ReadsOnly {
    facts: ClanFacts,
    calls: Vec<String>,
}

impl Control for ReadsOnly {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
        self.calls.push(method.into());
        match method {
            "status" => {
                Ok(json!({"ready": true, "clan": crate::control::view::ClanSummary::default()}))
            }
            "agents" => Ok(json!({"agents": [crate::control::view::AgentView {
                id: "term:waiting".into(), kind: "coordinator".into(), state: "needs_input".into(),
                ..Default::default()
            }]})),
            "agent" => {
                assert_eq!(params["screen"], false);
                Ok(
                    json!({"agent": {"id": "term:waiting", "state": "needs_input", "preview": "SECRET SCREEN"}}),
                )
            }
            "clan_data" => Ok(serde_json::to_value(&self.facts).unwrap()),
            "quotas" | "review_queue" => Err(CallError::NotRunning),
            other => panic!("standup must never call {other}"),
        }
    }

    fn emit(&mut self, method: &str, _: Value) {
        panic!("standup must not emit {method}");
    }
}

fn task(title: &str, at: Option<i64>, status: TaskStatus) -> Task {
    serde_json::from_value(json!({"id": title, "task_title": title, "status": status,
        "completed_at": at.map(Timestamp::Millis)}))
    .unwrap()
}

#[test]
fn collects_recent_completions_without_screens_or_writes_and_reports_missing_sources() {
    let now: jiff::Zoned = "2026-10-15T20:00:00+02:00[Europe/Stockholm]"
        .parse()
        .unwrap();
    let since: jiff::Timestamp = "2026-10-13T22:00:00Z".parse().unwrap();
    let since = since.as_millisecond();
    let mut control = ReadsOnly {
        facts: ClanFacts {
            runs: vec![Run {
                objective: Some("Improve recommendations".into()),
                ..Default::default()
            }],
            tasks: [(
                "run".into(),
                vec![
                    task("recent", Some(since), TaskStatus::Completed),
                    task("old", Some(since - 1), TaskStatus::Completed),
                    task("undated", None, TaskStatus::Completed),
                    task(
                        "future",
                        Some(now.timestamp().as_millisecond() + 1),
                        TaskStatus::Completed,
                    ),
                    task("working", None, TaskStatus::Dispatched),
                ],
            )]
            .into(),
            ..Default::default()
        },
        calls: Vec::new(),
    };
    let root = tempfile::tempdir().unwrap();
    let inbox_path = root.path().join("inbox.json");
    let env = ReviewEnv {
        king_inbox: Some(inbox_path.clone()),
        ..Default::default()
    };
    let output = collect(
        &mut control,
        &env,
        Some(&root.path().join("missing")),
        &now,
        |range| {
            assert_eq!(range, "2026-10-13T22:00:00Z..2026-10-15T18:00:00Z");
            Err("offline".into())
        },
    );
    let data = output.structured.unwrap();
    assert_eq!(data["runs_and_tasks"]["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(data["runs_and_tasks"]["tasks"][0]["title"], "recent");
    assert_eq!(data["runs_and_tasks"]["tasks"][1]["title"], "working");
    assert_eq!(data["merged_prs"]["unavailable"], true);
    assert_eq!(data["ledger"]["incomplete"], true);
    assert_eq!(data["review_queue"]["unavailable"], true);
    assert!(!output.text.contains("SECRET SCREEN"));
    assert!(
        !inbox_path.exists(),
        "reading a missing inbox must not create it"
    );
    assert!(output.text.contains("untrusted evidence"));
}

#[test]
fn calendar_window_crosses_dst_at_local_midnight() {
    for (day, hours) in [
        ("2026-03-30T12:00:00+02:00[Europe/Stockholm]", 23),
        ("2026-10-26T12:00:00+01:00[Europe/Stockholm]", 25),
    ] {
        let now: jiff::Zoned = day.parse().unwrap();
        let today = now.start_of_day().unwrap();
        let yesterday = today.yesterday().unwrap().start_of_day().unwrap();
        assert_eq!(
            today.timestamp().as_second() - yesterday.timestamp().as_second(),
            hours * 3600
        );
    }
}

#[test]
fn ledger_is_bounded_and_does_not_follow_symlinks_or_hidden_files() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), "do not read this").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link.md")).unwrap();
    std::fs::write(dir.path().join(".hidden.md"), "hidden").unwrap();
    for i in 0..25 {
        std::fs::write(dir.path().join(format!("note-{i}.md")), "å".repeat(2000)).unwrap();
    }
    let data = ledger::recent(dir.path(), 0, i64::MAX);
    let entries = data["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 20);
    assert_eq!(data["truncated"], true);
    assert!(!data.to_string().contains("do not read this"));
    assert!(!data.to_string().contains("hidden"));
    assert_eq!(
        entries[0]["excerpt"].as_str().unwrap().chars().count(),
        1600
    );
}

#[test]
fn standup_rejects_arguments_before_any_io() {
    let mut control = ReadsOnly {
        facts: ClanFacts::default(),
        calls: Vec::new(),
    };
    let result = call(
        &mut control,
        &ReviewEnv::default(),
        "standup",
        &json!({"send": true}),
    )
    .unwrap();
    assert!(result.is_error);
    assert!(control.calls.is_empty());
    assert!(crate::chat::permissions::is_auto_approved(
        "mcp__stronghold__standup"
    ));
}

#[test]
fn inbox_snapshot_keeps_receipts_and_legacy_files_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(crate::king_inbox::FILE);
    let old = dir.path().join("queen-inbox.json");
    let message = |id, at| {
        json!({
            "id": id, "at_ms": at, "message": "A reported blocker", "kind": "blocked",
            "url": null, "origin": {}, "node_id": null, "label": "worker",
            "session_id": null, "delivered": false
        })
    };
    let current = json!([message("new", 20), message("old", 9)]).to_string();
    let legacy = json!([
        message("new", 20),
        message("legacy", 10),
        message("future", 31)
    ])
    .to_string();
    std::fs::write(&path, &current).unwrap();
    std::fs::write(&old, &legacy).unwrap();
    let result = inbox_snapshot(&path, 10, 30);
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["at_ms"], 20);
    assert_eq!(messages[1]["at_ms"], 10);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), current);
    assert_eq!(std::fs::read_to_string(&old).unwrap(), legacy);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    std::fs::write(&old, "broken JSON").unwrap();
    assert_eq!(inbox_snapshot(&path, 10, 30)["incomplete"], true);
}

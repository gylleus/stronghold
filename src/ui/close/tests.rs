use super::*;
use crate::actions::{ALREADY_CLOSED, Action};

fn plan() -> ClosePlan {
    ClosePlan {
        targets: vec![close::CloseTarget {
            id: NodeId("term:t1".into()),
            label: "worker one".into(),
            kind: crate::model::NodeKind::Worker,
        }],
        steps: vec![
            CloseStep::WorkerStop {
                dispatch_id: "d1".into(),
                handle: Some("t1".into()),
            },
            CloseStep::CloseTerminal {
                handle: "t1".into(),
                tab: false,
            },
        ],
        ..ClosePlan::default()
    }
}

fn result(id: ActionId, outcome: Result<(), &str>, note: Option<&str>) -> ActionResult {
    ActionResult {
        id,
        action: Action::CloseTerminal {
            handle: "t1".into(),
            tab: false,
        },
        outcome: outcome.map_err(str::to_owned),
        note: note.map(str::to_owned),
    }
}

#[test]
fn step_labels_name_steps() {
    assert_eq!(
        step_labels(&plan(), None),
        vec!["Stop dispatch d1 of t1", "Close terminal t1"]
    );
}

#[test]
fn a_run_is_logged_once_all_steps_report() {
    let mut dialog = CloseDialog::default();
    dialog.request(CloseRequest::Nodes(vec![NodeId("term:t1".into())]));
    let labels = step_labels(&plan(), None);
    dialog.started(plan(), labels, &[7, 8], 1_000);
    assert!(dialog.log().is_empty());

    // Results of other actions are not ours.
    assert!(!dialog.on_result(&result(3, Ok(()), None), 1_100));
    assert!(!dialog.on_result(&result(7, Ok(()), Some("dispatch stopped")), 1_200));
    assert!(dialog.on_result(&result(8, Ok(()), Some(ALREADY_CLOSED)), 1_300));

    let log = dialog.log();
    assert_eq!(log.len(), 1);
    assert!(log[0].ok);
    assert_eq!(log[0].summary, "Closed worker one");
    assert_eq!(
        log[0].details,
        vec![
            "✓ Stop dispatch d1 of t1: dispatch stopped",
            "✓ Close terminal t1: already closed"
        ]
    );
    // The dialog still shows the finished run.
    assert!(dialog.is_open());
}

#[test]
fn a_failed_step_marks_the_log_entry() {
    let mut dialog = CloseDialog::default();
    dialog.started(plan(), step_labels(&plan(), None), &[1, 2], 0);
    dialog.on_result(&result(2, Err("orca timed out"), None), 5);
    assert!(dialog.on_result(&result(1, Ok(()), None), 6));
    let entry = &dialog.log()[0];
    assert!(!entry.ok);
    assert_eq!(entry.summary, "Close of worker one: 1 of 2 steps failed");
    assert_eq!(entry.details[1], "✗ Close terminal t1: orca timed out");
    // Not shown in a dialog, so the finished run is forgotten.
    assert!(dialog.runs.is_empty());
}

#[test]
fn the_log_is_bounded() {
    let mut dialog = CloseDialog::default();
    for i in 0..(MAX_LOG as u64 + 5) {
        dialog.started(plan(), step_labels(&plan(), None), &[i * 2, i * 2 + 1], 0);
        dialog.on_result(&result(i * 2, Ok(()), None), 0);
        dialog.on_result(&result(i * 2 + 1, Ok(()), None), 0);
    }
    assert_eq!(dialog.log().len(), MAX_LOG);
}

#[test]
fn plan_request_for_gone_node_is_refused() {
    let snapshot = ClanSnapshot::default();
    let plan = plan_request(
        &CloseRequest::Nodes(vec![NodeId("term:gone".into())]),
        &snapshot,
        &[],
        &BTreeSet::new(),
        CloseOptions::default(),
        false,
    );
    assert_eq!(plan.refused.as_deref(), Some("the node is gone"));
    let shown = plan_request(
        &CloseRequest::Shown,
        &snapshot,
        &[],
        &BTreeSet::new(),
        CloseOptions::default(),
        false,
    );
    assert_eq!(shown.refused.as_deref(), Some("nothing to close"));
}

use serde_json::json;

use super::*;
use crate::orca::{InboxMessage, Run, Task, Terminal, Worker};

const RUN: &str = "run_a";
const DECLARED: &str = "term_declared";
const COORD: &str = "term_coord";
const OTHER: &str = "term_other";
const WORKER: &str = "term_worker";
const MIN: i64 = 60_000;
const NOW: i64 = 1_790_000_000_000;

fn terminal(handle: &str, title: &str) -> Terminal {
    Terminal {
        handle: handle.into(),
        title: Some(title.into()),
        ..Default::default()
    }
}

fn task(id: &str, creator: &str, at: i64) -> Task {
    serde_json::from_value(json!({
        "id": id, "run_id": RUN, "status": "completed",
        "created_by_terminal_handle": creator, "created_at": at,
    }))
    .unwrap()
}

fn msg(id: &str, from: &str, to: &str, kind: &str, at: i64) -> InboxMessage {
    serde_json::from_value(json!({
        "id": id, "run_id": RUN, "from_handle": from, "to_handle": to,
        "type": kind, "created_at": at,
    }))
    .unwrap()
}

fn forward(target: &str, at: i64) -> Forward {
    Forward {
        target: target.into(),
        at_ms: Some(at),
    }
}

/// A run declared by DECLARED, with WORKER dispatched, COORD and OTHER live and no evidence.
fn base() -> FleetData {
    let worker: Worker = serde_json::from_value(json!({
        "dispatchId": "ctx_1", "runId": RUN, "agentTerminalHandle": WORKER,
        "dispatchStatus": "dispatched",
    }))
    .unwrap();
    FleetData {
        terminals: vec![
            terminal(DECLARED, "✳ run brief"),
            terminal(COORD, "✳ wave coordinator"),
            terminal(OTHER, "shell"),
            terminal(WORKER, "✳ worker"),
        ],
        runs: vec![Run {
            id: RUN.into(),
            coordinator_handle: Some(DECLARED.into()),
            ..Default::default()
        }],
        workers: vec![worker],
        tasks: [(RUN.to_string(), Vec::new())].into(),
        ..Default::default()
    }
}

fn roles(data: &FleetData) -> RunRoles {
    data.run_roles(RUN)
}

#[test]
fn plain_declared_coordinator_is_the_coordinator() {
    let mut data = base();
    data.tasks
        .get_mut(RUN)
        .unwrap()
        .push(task("t1", DECLARED, NOW));
    let r = roles(&data);
    assert_eq!(r.coordinator.as_deref(), Some(DECLARED));
    assert_eq!(r.attendant, None);
    assert_eq!(r.signal, None);
    assert_eq!(data.effective_coordinator(RUN).as_deref(), Some(DECLARED));
}

#[test]
fn title_marks_the_attendant() {
    let mut data = base();
    data.terminals[0].title = Some("✳ cp-wave-0923-ATTENDANT brief".into());
    data.tasks
        .get_mut(RUN)
        .unwrap()
        .push(task("t1", COORD, NOW - MIN));
    let r = roles(&data);
    assert_eq!(r.signal, Some(AttendantSignal::Title));
    assert_eq!(r.attendant.as_deref(), Some(DECLARED));
    assert_eq!(r.coordinator.as_deref(), Some(COORD));
    assert_eq!(r.declared.as_deref(), Some(DECLARED));
}

#[test]
fn other_task_creators_mark_the_attendant() {
    let mut data = base();
    let tasks = data.tasks.get_mut(RUN).unwrap();
    tasks.push(task("t1", COORD, NOW - 10 * MIN));
    tasks.push(task("t2", "term_gone", NOW));
    let r = roles(&data);
    assert_eq!(r.signal, Some(AttendantSignal::OthersCoordinate));
    assert_eq!(r.coordinator.as_deref(), Some(COORD), "newest live creator");

    // The declared terminal created tasks too: it is the coordinator.
    data.tasks
        .get_mut(RUN)
        .unwrap()
        .push(task("t3", DECLARED, NOW - 20 * MIN));
    let r = roles(&data);
    assert_eq!(r.signal, None);
    assert_eq!(r.coordinator.as_deref(), Some(DECLARED));
}

#[test]
fn worker_created_tasks_do_not_mark_the_attendant() {
    let mut data = base();
    data.tasks
        .get_mut(RUN)
        .unwrap()
        .push(task("t1", WORKER, NOW));
    assert_eq!(roles(&data).attendant, None);
}

#[test]
fn transcript_forward_target_is_the_coordinator() {
    let mut data = base();
    data.forwards.insert(
        DECLARED.into(),
        vec![
            forward("term_gone", NOW - 30 * MIN),
            forward(COORD, NOW - 5 * MIN),
        ],
    );
    let r = roles(&data);
    assert_eq!(r.signal, Some(AttendantSignal::Forwards));
    assert_eq!(r.attendant.as_deref(), Some(DECLARED));
    assert_eq!(r.coordinator.as_deref(), Some(COORD));
}

#[test]
fn forwards_to_workers_only_are_a_coordinator_nudging() {
    let mut data = base();
    data.forwards
        .insert(DECLARED.into(), vec![forward(WORKER, NOW)]);
    let r = roles(&data);
    assert_eq!(r.attendant, None);
    assert_eq!(r.coordinator.as_deref(), Some(DECLARED));
}

#[test]
fn coordinator_bus_senders_mark_the_attendant() {
    let mut data = base();
    data.inbox = vec![
        msg("m1", COORD, "dispatch:ctx_1", "status", NOW - 2 * MIN),
        msg("m2", WORKER, &format!("run:{RUN}"), "heartbeat", NOW),
    ];
    let r = roles(&data);
    assert_eq!(r.signal, Some(AttendantSignal::OthersCoordinate));
    assert_eq!(r.coordinator.as_deref(), Some(COORD));
}

#[test]
fn bus_senders_skip_workers_services_and_worker_messages() {
    let mut data = base();
    data.terminals[0].title = Some("attendant".into());
    data.inbox = vec![
        msg("m1", COORD, &format!("run:{RUN}"), "status", NOW - 30 * MIN),
        msg("m2", WORKER, &format!("run:{RUN}"), "status", NOW),
        msg("m3", "myrstack-karl", &format!("run:{RUN}"), "status", NOW),
        msg("m4", &format!("run:{RUN}"), "dispatch:ctx_1", "status", NOW),
        // Not in the worker list, but only workers send heartbeats.
        msg("m5", OTHER, &format!("run:{RUN}"), "heartbeat", NOW),
        msg("m6", DECLARED, &format!("run:{RUN}"), "status", NOW),
    ];
    assert_eq!(roles(&data).coordinator.as_deref(), Some(COORD));
}

#[test]
fn most_recent_live_signal_wins() {
    let mut data = base();
    data.terminals[0].title = Some("Attendant".into());
    data.forwards
        .insert(DECLARED.into(), vec![forward(COORD, NOW - 10 * MIN)]);
    data.tasks
        .get_mut(RUN)
        .unwrap()
        .push(task("t1", OTHER, NOW - 20 * MIN));
    assert_eq!(
        roles(&data).coordinator.as_deref(),
        Some(COORD),
        "forward newer"
    );

    data.tasks
        .get_mut(RUN)
        .unwrap()
        .push(task("t2", OTHER, NOW - MIN));
    assert_eq!(
        roles(&data).coordinator.as_deref(),
        Some(OTHER),
        "creator newer"
    );

    // The newest candidate is gone: the next live one wins.
    data.forwards
        .insert(DECLARED.into(), vec![forward("term_gone", NOW)]);
    data.tasks.get_mut(RUN).unwrap().truncate(1);
    data.tasks
        .get_mut(RUN)
        .unwrap()
        .push(task("t3", "term_gone", NOW));
    assert_eq!(roles(&data).coordinator.as_deref(), Some(OTHER));
}

#[test]
fn attendant_without_live_coordinator() {
    let mut data = base();
    data.terminals[0].title = Some("cp-wave ATTENDANT".into());
    data.forwards
        .insert(DECLARED.into(), vec![forward("term_gone", NOW)]);
    data.tasks
        .get_mut(RUN)
        .unwrap()
        .push(task("t1", "term_gone", NOW));
    let r = roles(&data);
    assert_eq!(r.attendant.as_deref(), Some(DECLARED));
    assert_eq!(r.coordinator, None);
    assert_eq!(data.effective_coordinator(RUN), None);
}

#[test]
fn dead_declared_keeps_the_rebound_fallback() {
    let mut data = base();
    data.terminals.remove(0);
    data.tasks
        .get_mut(RUN)
        .unwrap()
        .push(task("t1", COORD, NOW));
    let r = roles(&data);
    assert_eq!(r.attendant, None);
    assert_eq!(r.coordinator.as_deref(), Some(COORD));
}

/// Live check against this machine's Orca and Claude transcripts: prints declared, attendant
/// and coordinator per active run. Read-only. Run with
/// `cargo test live_run_roles -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_run_roles() {
    use crate::orca::OrcaClient;
    use crate::poller::{ACTIVE_RUN_WINDOW, FleetSource, active_run_ids};

    let client = OrcaClient::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut data = FleetData {
        polled_at_ms: now,
        terminals: FleetSource::terminals(&client).unwrap(),
        runs: FleetSource::runs(&client).unwrap(),
        workers: FleetSource::workers(&client).unwrap(),
        inbox: FleetSource::inbox(&client, 300).unwrap(),
        ..Default::default()
    };
    let active = active_run_ids(
        &data.runs,
        &data.workers,
        &data.terminals,
        now,
        ACTIVE_RUN_WINDOW,
    );
    for run_id in &active {
        data.tasks
            .insert(run_id.clone(), FleetSource::tasks(&client, run_id).unwrap());
    }
    data.forwards = crate::claude::forwards::read_forwards(&data);

    let short = |h: &Option<String>| match h {
        Some(h) => {
            let title = data
                .terminal(h)
                .and_then(|t| t.title.as_deref())
                .map(|t| format!(" \"{}\"", t.chars().take(32).collect::<String>()))
                .unwrap_or_else(|| " (dead)".into());
            format!("{}{title}", &h[..h.len().min(13)])
        }
        None => "-".into(),
    };
    println!("run | declared | attendant (signal) | coordinator | newest forward");
    for run_id in &active {
        let roles = data.run_roles(run_id);
        let newest = roles
            .declared
            .as_ref()
            .and_then(|d| data.forwards.get(d)?.last())
            .map(|f| short(&Some(f.target.clone())))
            .unwrap_or_else(|| "-".into());
        println!(
            "{run_id} | {} | {} ({:?}) | {} | {newest}",
            short(&roles.declared),
            short(&roles.attendant),
            roles.signal,
            short(&roles.coordinator),
        );
    }
}

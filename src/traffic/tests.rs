use serde_json::json;

use super::*;
use crate::model::{Node, NodeState, RunSummary};
use crate::orca::Timestamp;
use crate::whip::actions::WhipLogEntry;
use crate::whip::runner::WhipLogItem;

const NOW: i64 = 1_790_000_000_000;

fn node(id: NodeId, kind: NodeKind, handle: Option<&str>) -> Node {
    Node {
        label: id.0.clone(),
        id,
        kind,
        terminal_handle: handle.map(str::to_owned),
        live: true,
        run_id: None,
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

/// Coordinator `term_c` of run `r1`, its worker `term_w` on dispatch `d1`, a loose terminal.
fn snapshot() -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    s.runs = vec![RunSummary {
        id: "r1".into(),
        objective: None,
        coordinator: Some(NodeId::terminal("term_c")),
        task_counts: None,
    }];
    s.nodes.push(node(
        NodeId::terminal("term_c"),
        NodeKind::Coordinator,
        Some("term_c"),
    ));
    let mut worker = node(NodeId::terminal("term_w"), NodeKind::Worker, Some("term_w"));
    worker.dispatch_id = Some("d1".into());
    s.nodes.push(worker);
    s.nodes.push(node(
        NodeId::terminal("term_l"),
        NodeKind::Terminal,
        Some("term_l"),
    ));
    s.add_queen();
    s
}

fn at(handle: &str) -> Endpoint {
    Endpoint::Node(NodeId::terminal(handle))
}

#[test]
fn resolves_every_way_an_agent_is_named() {
    let s = snapshot();
    assert_eq!(resolve(&s, "term_w"), Some(at("term_w")));
    assert_eq!(resolve(&s, "term:term_w"), Some(at("term_w")));
    assert_eq!(resolve(&s, "dispatch:d1"), Some(at("term_w")));
    assert_eq!(resolve(&s, "run:r1"), Some(at("term_c")));
    assert_eq!(resolve(&s, "queen"), Some(Endpoint::Node(NodeId::queen())));
    assert_eq!(
        resolve(&s, "myrstack-whip"),
        Some(Endpoint::Node(NodeId::queen()))
    );
    assert_eq!(resolve(&s, "Karl"), Some(Endpoint::Karl));
    assert_eq!(resolve(&s, "slack"), Some(Endpoint::Outside));
    assert_eq!(resolve(&s, "term_gone"), None);
    assert_eq!(resolve(&s, "run:r9"), None);
}

#[test]
fn a_message_to_itself_or_a_stranger_is_not_drawn() {
    let s = snapshot();
    let msg = |from: &str, to: &str| AgentMessage::new(from, to, MessageKind::Message).resolve(&s);
    assert!(msg("term_w", "run:r1").is_some());
    assert!(msg("term_c", "run:r1").is_none());
    assert!(msg("term_w", "nobody").is_none());
}

fn inbox(id: &str, from: &str, to: &str, kind: &str, created: i64) -> InboxMessage {
    serde_json::from_value(json!({
        "id": id, "from_handle": from, "to_handle": to, "type": kind,
        "created_at": created,
    }))
    .unwrap()
}

#[test]
fn bus_messages_map_to_kinds() {
    let kind =
        |from: &str, to: &str, t: &str| from_inbox(&inbox("m", from, to, t, NOW)).map(|m| m.kind);
    assert_eq!(kind("term_w", "run:r1", "question"), Some(MessageKind::Ask));
    assert_eq!(
        kind("term_w", "run:r1", "escalation"),
        Some(MessageKind::Ask)
    );
    assert_eq!(
        kind("term_w", "run:r1", "merge_ready"),
        Some(MessageKind::MergeReady)
    );
    assert_eq!(
        kind("term_c", "dispatch:d1", "dispatch"),
        Some(MessageKind::Instruction)
    );
    assert_eq!(
        kind("term_w", "run:r1", "worker_done"),
        Some(MessageKind::Status)
    );
    assert_eq!(
        kind("queen", "term_c", "custom"),
        Some(MessageKind::Instruction)
    );
    assert_eq!(
        kind("term_w", "run:r1", "custom"),
        Some(MessageKind::Message)
    );
    assert_eq!(kind("term_w", "run:r1", "heartbeat"), None);
    assert_eq!(kind("myrstack-whip", "term_w", "status"), None);
    assert_eq!(kind("term_c", "@all", "status"), None);
}

#[test]
fn inbox_watch_draws_only_new_fresh_messages() {
    let mut watch = InboxWatch::default();
    let old = inbox("m1", "term_w", "run:r1", "question", NOW);
    assert!(
        watch.update(std::slice::from_ref(&old), NOW).is_empty(),
        "first poll is history"
    );
    let new = inbox("m2", "term_w", "run:r1", "merge_ready", NOW);
    let stale = inbox("m3", "term_w", "run:r1", "status", NOW - FRESH_MS - 1);
    let got = watch.update(&[old.clone(), new.clone(), stale], NOW);
    assert_eq!(
        got,
        vec![AgentMessage::new(
            "term_w",
            "run:r1",
            MessageKind::MergeReady
        )]
    );
    assert!(watch.update(&[old, new], NOW).is_empty());
}

#[test]
fn inbox_timestamps_may_be_text() {
    let mut m = inbox("m", "term_w", "run:r1", "question", NOW);
    m.created_at = Some(Timestamp::Text("garbage".into()));
    let mut watch = InboxWatch::default();
    watch.update(&[inbox("m0", "term_w", "run:r1", "status", NOW)], NOW);
    assert_eq!(watch.update(&[m], NOW).len(), 1);
}

#[test]
fn sent_whip_nudges_fly_from_the_queen() {
    let mut item = WhipLogItem {
        seq: 1,
        entry: WhipLogEntry {
            at_ms: NOW,
            action: "nudge #1 via terminal input".into(),
            target: "worker term_w task t1".into(),
            reason: String::new(),
            dry_run: false,
            outcome: "sent".into(),
            manual: false,
        },
        handle: Some("term_w".into()),
        run_id: None,
        task_id: None,
    };
    assert_eq!(
        from_whip(&item),
        Some(AgentMessage::new("queen", "term_w", MessageKind::Nudge))
    );
    item.entry.dry_run = true;
    assert_eq!(from_whip(&item), None);
    item.entry.dry_run = false;
    item.entry.outcome = "failed: boom".into();
    assert_eq!(from_whip(&item), None);
    item.entry.outcome = "sent".into();
    item.entry.action = "escalate".into();
    assert_eq!(from_whip(&item), None);
}

#[test]
fn agent_message_params() {
    let m = AgentMessage::from_params(&json!({"from": "envoy", "to": "term_c", "kind": "relay"}))
        .unwrap();
    assert_eq!(m, AgentMessage::new("envoy", "term_c", MessageKind::Relay));
    let plain = AgentMessage::from_params(&json!({"from": "a", "to": "b"})).unwrap();
    assert_eq!(plain.kind, MessageKind::Message);
    assert!(AgentMessage::from_params(&json!({"from": "a"})).is_err());
    assert!(AgentMessage::from_params(&json!({"from": " ", "to": "b"})).is_err());
}

#[test]
fn senders_on_other_threads_reach_the_queue() {
    let traffic = Traffic::default();
    let tx = traffic.sender();
    std::thread::spawn(move || tx.send(AgentMessage::new("a", "b", MessageKind::Relay)))
        .join()
        .unwrap();
    traffic.push(AgentMessage::new("b", "a", MessageKind::Ask));
    assert_eq!(traffic.drain().len(), 2);
    assert!(traffic.drain().is_empty());
}

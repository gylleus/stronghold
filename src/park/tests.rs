use tempfile::TempDir;

use super::*;
use crate::model::NodeState;
use crate::orca::Terminal;

fn node(handle: &str, kind: NodeKind, live: bool) -> Node {
    Node {
        id: NodeId::terminal(handle),
        kind,
        label: format!("ant {handle}"),
        terminal_handle: Some(handle.into()),
        live,
        run_id: None,
        task_id: None,
        dispatch_id: None,
        agent: None,
        worktree_path: Some(format!("/w/orca/workspaces/repo/{handle}/")),
        branch: Some(format!("b/{handle}")),
        last_output_ms: None,
        preview: None,
        state: NodeState::Done,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
        parked_at_ms: None,
    }
}

fn data(handles: &[&str]) -> FleetData {
    FleetData {
        terminals: handles
            .iter()
            .map(|h| Terminal {
                handle: (*h).into(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

#[test]
fn parking_survives_a_reload_and_waking_forgets() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(FILE);
    let mut parking = Parking::load(path.clone());
    assert_eq!(
        parking.park(&node("t1", NodeKind::Terminal, true), 5),
        Ok(true)
    );
    assert_eq!(
        parking.park(&node("t1", NodeKind::Terminal, true), 6),
        Ok(false)
    );
    assert_eq!(
        parking.park(&node("t2", NodeKind::Worker, true), 7),
        Ok(true)
    );

    let reloaded = Parking::load(path.clone());
    assert_eq!(reloaded.entries().len(), 2);
    let t1 = reloaded.get(&NodeId::terminal("t1")).unwrap();
    assert_eq!((t1.handle.as_str(), t1.at_ms), ("t1", 5));
    assert_eq!(t1.branch.as_deref(), Some("b/t1"));
    assert_eq!(
        reloaded.handles(),
        BTreeSet::from(["t1".to_string(), "t2".to_string()])
    );
    assert_eq!(
        parked_worktrees(&path),
        BTreeSet::from([
            "/w/orca/workspaces/repo/t1".to_string(),
            "/w/orca/workspaces/repo/t2".to_string()
        ])
    );

    assert!(parking.unpark(&NodeId::terminal("t1")).is_some());
    assert!(parking.unpark(&NodeId::terminal("t1")).is_none());
    let reloaded = Parking::load(path);
    assert!(!reloaded.is_parked(&NodeId::terminal("t1")));
    assert!(reloaded.is_parked(&NodeId::terminal("t2")));
}

#[test]
fn only_live_agent_terminals_can_be_parked() {
    let mut parking = Parking::default();
    for (n, ok) in [
        (node("t", NodeKind::Coordinator, true), true),
        (node("t", NodeKind::Subagent, true), false),
        (node("t", NodeKind::Queen, true), false),
        (node("closed", NodeKind::Terminal, false), false),
    ] {
        assert_eq!(refusal(&n).is_none(), ok, "{:?}", n.kind);
        assert_eq!(parking.park(&n, 1).is_ok(), ok, "{:?}", n.kind);
        parking.unpark(&n.id);
    }
}

#[test]
fn agents_whose_terminal_closed_are_dropped_on_a_clean_poll_only() {
    let mut parking = Parking::default();
    parking
        .park(&node("t1", NodeKind::Terminal, true), 1)
        .unwrap();
    parking
        .park(&node("t2", NodeKind::Terminal, true), 1)
        .unwrap();

    let mut failed = data(&["t1"]);
    failed.errors.push("terminal list: timeout".into());
    assert!(parking.prune_gone(&failed).is_empty());
    assert!(
        parking.prune_gone(&data(&[])).is_empty(),
        "no terminals proves nothing"
    );

    let gone = parking.prune_gone(&data(&["t1", "other"]));
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].handle, "t2");
    assert_eq!(parking.handles(), BTreeSet::from(["t1".to_string()]));
}

#[test]
fn an_unreadable_file_loads_empty() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(FILE);
    std::fs::write(&path, "not json").unwrap();
    assert!(Parking::load(path.clone()).entries().is_empty());
    assert!(parked_worktrees(&path).is_empty());
}

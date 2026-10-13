//! The king: one hub node per snapshot, linked to every coordinator and loose terminal, whose
//! state is the most urgent state in the clan.

use super::{Node, NodeId, NodeKind, NodeState};

/// The king's stable node id.
pub const KING_ID: &str = "king";
pub const KING_LABEL: &str = "King";

/// The clan's most urgent state: NeedsInput if any node needs input, else Stalled if any is
/// stalled (escalated), else Working if any works, else Idle. King nodes and parked agents
/// are ignored.
pub fn clan_state<'a>(nodes: impl IntoIterator<Item = &'a Node>) -> NodeState {
    let mut best = NodeState::Idle;
    for node in nodes
        .into_iter()
        .filter(|n| n.kind != NodeKind::King && !n.is_parked())
    {
        let rank = |s: NodeState| match s {
            NodeState::NeedsInput => 3,
            NodeState::Stalled => 2,
            NodeState::Working => 1,
            _ => 0,
        };
        if rank(node.state) > rank(best) {
            best = node.state;
        }
    }
    best
}

/// The king for a snapshot with `nodes`; its preview is the "N coordinators · M dwarves" line,
/// with "· K parked" when some sleep.
pub(super) fn king_node(nodes: &[Node]) -> Node {
    let parked = nodes.iter().filter(|n| n.is_parked()).count();
    let agents = nodes
        .iter()
        .filter(|n| n.kind != NodeKind::King && !n.is_parked());
    let coordinators = agents
        .clone()
        .filter(|n| n.kind == NodeKind::Coordinator)
        .count();
    let total = agents.count();
    let parked = if parked > 0 {
        format!(" · {parked} parked")
    } else {
        String::new()
    };
    Node {
        id: NodeId::king(),
        kind: NodeKind::King,
        label: KING_LABEL.to_string(),
        terminal_handle: None,
        live: true,
        run_id: None,
        task_id: None,
        dispatch_id: None,
        agent: None,
        worktree_path: None,
        branch: None,
        last_output_ms: None,
        preview: Some(format!(
            "{coordinators} coordinator{} · {total} {}{parked}",
            plural(coordinators),
            if total == 1 { "dwarf" } else { "dwarves" }
        )),
        state: clan_state(nodes),
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
        parked_at_ms: None,
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

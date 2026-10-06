//! Subagent nodes and edges from a [`ClaudeData`] scan.

use std::collections::{BTreeMap, HashSet};

use crate::claude::{ClaudeData, SessionMatch, SubagentInfo, SubagentParent, SubagentState};

use super::{Edge, EdgeKind, Node, NodeId, NodeKind, NodeState, RELEASED_FADE_MS, label};

/// Adds a node per subagent whose session belongs to a terminal node in `nodes`, and an edge
/// from its parent subagent (if shown) or else from that terminal. Finished subagents stay
/// for [`RELEASED_FADE_MS`] after finishing, marked for fading.
pub(super) fn add_subagents(
    claude: &ClaudeData,
    now_ms: i64,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let owners = session_owners(claude, nodes);
    let mut added: HashSet<NodeId> = HashSet::new();
    let mut pending: Vec<(NodeId, NodeId, Option<NodeId>)> = Vec::new();

    for (session_id, agents) in &claude.subagents {
        let Some(owner) = owners.get(session_id.as_str()) else {
            continue;
        };
        let owner_node = nodes.iter().find(|n| &n.id == owner).cloned();
        for info in agents {
            let Some(node) = subagent_node(info, owner_node.as_ref(), now_ms) else {
                continue;
            };
            let parent = match &info.parent {
                SubagentParent::Agent(p) => Some(NodeId::subagent(session_id, p)),
                SubagentParent::Session => None,
            };
            added.insert(node.id.clone());
            pending.push((node.id.clone(), (*owner).clone(), parent));
            nodes.push(node);
        }
    }

    edges.extend(pending.into_iter().map(|(id, owner, parent)| Edge {
        from: parent.filter(|p| added.contains(p)).unwrap_or(owner),
        to: id,
        kind: EdgeKind::Subagent,
    }));
}

/// Terminal node id per session id. A session claimed by several terminals goes to a unique
/// match first, then to the first handle.
fn session_owners<'a>(claude: &'a ClaudeData, nodes: &[Node]) -> BTreeMap<&'a str, NodeId> {
    let mut owners: BTreeMap<&str, (bool, NodeId)> = BTreeMap::new();
    for (handle, m) in &claude.session_by_handle {
        let Some(session) = m.session() else {
            continue;
        };
        let id = NodeId::terminal(handle);
        if !nodes.iter().any(|n| n.id == id) {
            continue;
        }
        let unique = matches!(m, SessionMatch::Unique(_));
        match owners.get(session.session_id.as_str()) {
            Some((true, _)) => {}
            Some((false, _)) if !unique => {}
            _ => {
                owners.insert(&session.session_id, (unique, id));
            }
        }
    }
    owners.into_iter().map(|(s, (_, id))| (s, id)).collect()
}

/// `None` when the subagent finished more than [`RELEASED_FADE_MS`] ago.
fn subagent_node(info: &SubagentInfo, owner: Option<&Node>, now_ms: i64) -> Option<Node> {
    let finished_at = info
        .state
        .is_finished()
        .then(|| info.finished_ms.unwrap_or(info.last_activity_ms));
    if finished_at.is_some_and(|at| now_ms - at > RELEASED_FADE_MS) {
        return None;
    }
    let text = if info.description.trim().is_empty() {
        &info.agent_type
    } else {
        &info.description
    };
    Some(Node {
        id: NodeId::subagent(&info.session_id, &info.agent_id),
        kind: NodeKind::Subagent,
        label: label::truncate_label(text),
        // Subagents take no input; the owning terminal is reached through the edge.
        terminal_handle: None,
        live: true,
        run_id: owner.and_then(|o| o.run_id.clone()),
        task_id: None,
        dispatch_id: None,
        agent: owner.and_then(|o| o.agent.clone()),
        worktree_path: owner.and_then(|o| o.worktree_path.clone()),
        branch: owner.and_then(|o| o.branch.clone()),
        last_output_ms: Some(info.last_activity_ms),
        preview: None,
        state: subagent_state(info.state),
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: finished_at,
        parked_at_ms: None,
    })
}

pub fn subagent_state(state: SubagentState) -> NodeState {
    match state {
        SubagentState::Running => NodeState::Working,
        SubagentState::Done => NodeState::Done,
        SubagentState::Failed => NodeState::Failed,
        SubagentState::Stale => NodeState::Idle,
    }
}

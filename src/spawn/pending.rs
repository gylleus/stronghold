//! Selecting a spawned session's node once the poller has seen its terminal.

use crate::model::{FleetSnapshot, NodeId, NodeKind};

/// How long to look for the new node before giving up.
pub const PENDING_SELECT_MS: i64 = 60_000;

/// A terminal whose node should be selected when it appears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSelect {
    pub handle: String,
    pub until_ms: i64,
}

impl PendingSelect {
    pub fn new(handle: impl Into<String>, now_ms: i64) -> Self {
        Self {
            handle: handle.into(),
            until_ms: now_ms + PENDING_SELECT_MS,
        }
    }

    pub fn expired(&self, now_ms: i64) -> bool {
        now_ms > self.until_ms
    }

    /// The node showing this terminal in `snapshot`, if it is there yet.
    pub fn find(&self, snapshot: &FleetSnapshot) -> Option<NodeId> {
        node_for_handle(snapshot, &self.handle)
    }
}

/// The node showing terminal `handle`, preferring a live one. Subagents share their parent's
/// handle and never count.
pub fn node_for_handle(snapshot: &FleetSnapshot, handle: &str) -> Option<NodeId> {
    let mut candidates = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::Subagent && n.terminal_handle.as_deref() == Some(handle));
    let first = candidates.next()?;
    if first.live {
        return Some(first.id.clone());
    }
    Some(
        candidates
            .find(|n| n.live)
            .map_or_else(|| first.id.clone(), |n| n.id.clone()),
    )
}

/// Takes the pending selection's node once it appears; clears it when it expired.
pub fn take_ready(
    pending: &mut Option<PendingSelect>,
    snapshot: &FleetSnapshot,
    now_ms: i64,
) -> Option<NodeId> {
    let current = pending.as_ref()?;
    if let Some(id) = current.find(snapshot) {
        *pending = None;
        return Some(id);
    }
    if current.expired(now_ms) {
        log::info!(
            "spawn: {} did not appear in the fleet within {} s",
            current.handle,
            PENDING_SELECT_MS / 1000
        );
        *pending = None;
    }
    None
}

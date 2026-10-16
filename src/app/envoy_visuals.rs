//! Visual feedback observes arrivals and confirmed traffic without changing draft workflows.

use std::collections::HashSet;

use super::StrongholdApp;
use crate::envoy::inbox::timestamp_us;
use crate::model::{ClanSnapshot, NodeKind};
use crate::traffic::{AgentMessage, Endpoint, MessageKind};

pub(super) struct Items {
    started_ms: i64,
    seen: HashSet<String>,
}

impl Items {
    pub fn new(started_ms: i64) -> Self {
        Self {
            started_ms,
            seen: HashSet::new(),
        }
    }

    /// Old persisted items and startup backlog never punch. Keep all current IDs so edits,
    /// status changes and replacing one item with another at the same count behave correctly.
    fn observe(&mut self, items: impl Iterator<Item = (String, i64)>) -> bool {
        let mut arrived = false;
        let mut seen = HashSet::new();
        for (id, created_ms) in items {
            arrived |= created_ms > self.started_ms && !self.seen.contains(&id);
            seen.insert(id);
        }
        self.seen = seen;
        arrived
    }
}

impl StrongholdApp {
    pub(super) fn poll_envoy_visuals(&mut self, ctx: &eframe::egui::Context) {
        let inbox = self.envoy_core.inbox().events().iter().map(|e| {
            (
                format!("inbox:{}", e.id()),
                timestamp_us(&e.ts).map_or(0, |ts| ts / 1000),
            )
        });
        let drafts = self
            .envoy_core
            .drafts()
            .iter()
            .map(|d| (format!("draft:{}", d.id), d.created_ms));
        let requests = self.envoy_core.requests().iter().flat_map(|r| {
            r.turns
                .iter()
                .map(move |t| (format!("request:{}:{}", r.id, t.id), t.created_ms))
        });
        if self
            .envoy_items
            .observe(inbox.chain(drafts).chain(requests))
        {
            self.farm.envoy_activity(false, ctx.input(|i| i.time));
            ctx.request_repaint();
        }
    }
}

/// Check the sender before resolving the destination: an external merchant need not be a
/// node in the hold. Ledger lookups are knowledge traffic, not messages relayed out.
pub(super) fn sent_by_envoy(message: &AgentMessage, snapshot: &ClanSnapshot) -> bool {
    message.kind != MessageKind::Ledger
        && message.from != message.to
        && matches!(
            crate::traffic::resolve(snapshot, &message.from),
            Some(Endpoint::Node(id)) if snapshot.nodes.iter().any(|n| n.id == id && n.kind == NodeKind::Envoy)
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envoy_messages_to_any_destination_send_but_other_traffic_does_not() {
        let mut snapshot = ClanSnapshot::default();
        snapshot.add_king();
        let mut envoy = snapshot.nodes[0].clone();
        envoy.id = crate::model::NodeId::terminal("envoy-handle");
        envoy.terminal_handle = Some("envoy-handle".into());
        envoy.kind = NodeKind::Envoy;
        snapshot.nodes.push(envoy);
        for sender in ["envoy", "envoy-handle", "term:envoy-handle"] {
            for destination in ["slack", "merchant-unknown-to-snapshot", "king"] {
                assert!(sent_by_envoy(
                    &AgentMessage::new(sender, destination, MessageKind::Relay),
                    &snapshot,
                ));
            }
        }
        for (from, to, kind) in [
            ("slack", "envoy", MessageKind::Relay),
            ("king", "slack", MessageKind::Message),
            ("envoy", "envoy", MessageKind::Message),
            ("envoy", "ledger", MessageKind::Ledger),
        ] {
            assert!(!sent_by_envoy(
                &AgentMessage::new(from, to, kind),
                &snapshot
            ));
        }
    }

    #[test]
    fn history_edits_and_bulk_refreshes_are_quiet_but_new_ids_are_not() {
        let mut items = Items::new(100);
        let snapshot = |rows: &[(&str, i64)]| {
            rows.iter()
                .map(|(id, at)| (id.to_string(), *at))
                .collect::<Vec<_>>()
                .into_iter()
        };
        assert!(!items.observe(snapshot(&[("old draft", 90), ("old inbox", 99)])));
        assert!(!items.observe(snapshot(&[("old draft", 90), ("backlog", 80)])));
        assert!(items.observe(snapshot(&[("new draft", 101), ("new inbox", 102)])));
        assert!(!items.observe(snapshot(&[("new draft", 101), ("new inbox", 102)])));
        // No change in count, but a genuinely new item replaced one that was handled.
        assert!(items.observe(snapshot(&[("new draft", 101), ("next inbox", 103)])));
        assert!(!items.observe(snapshot(&[])));
        assert!(!items.observe(snapshot(&[("backlog", 80)])));
    }
}

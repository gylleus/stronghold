//! Who talks to whom: one [`AgentMessage`] per message between agents, the King, Karl and the
//! outside world, which the farm draw as an envelope flying from sender to
//! receiver.
//!
//! Sources, all streams the app already has (no tool exists just for the animation):
//! - the Orca bus inbox the poller reads ([`InboxWatch`]): dispatch, question, escalation,
//!   merge_ready, status and worker_done between workers, coordinators and the King's
//!   broadcasts;
//! - control API commands: `send` (`send_to_agent`, from its caller or the King), `notify` and
//!   `ask` (an agent to Karl), and the generic `agent_message {from, to, kind}`;
//! - Karl typing into a terminal from Stronghold, and the whip's nudges ([`from_whip`]).
//! - `orca terminal send`s Claude terminals run, read from their transcripts
//!   ([`crate::claude::forwards::SendWatch`]). Stronghold's own sends (the whip's, Karl's) run in
//!   its process, never in a transcript, so they are not drawn twice.
//! - Ledger lookups and filings, from Claude transcripts and Codex rollouts
//!   ([`ledger::LedgerWatch`]) and the King's chat ([`ledger::message`]).
//!
//! In-process emitters (the Envoy's relays, say) push through a [`TrafficSender`]; other
//! processes call the control API's `agent_message`.

pub mod beam;
pub mod ledger;

use std::collections::HashSet;

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value;

use crate::control::protocol::{ControlError, Params};
use crate::model::{ClanSnapshot, NodeId, NodeKind};
use crate::orca::{InboxMessage, MessageType, is_broadcast_sender, is_whip_sender};
use crate::whip::runner::WhipLogItem;

/// Bus messages older than this when first seen are not drawn: they are history the inbox's
/// window slid over, not traffic.
pub const FRESH_MS: i64 = 2 * 60_000;

/// What a message is, which picks its look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// Karl or the King telling an agent what to do, a broadcast or a dispatch.
    Instruction,
    /// One agent writing to another.
    Message,
    /// A question or escalation, or an agent asking Karl for something.
    Ask,
    /// A worker's branch is ready to merge.
    MergeReady,
    /// Traffic relayed from outside (Slack, via the Envoy).
    Relay,
    /// The whip prodding a stalled agent.
    Nudge,
    /// A status report or an FYI.
    Status,
    /// Knowledge read from or filed in the Ledger.
    Ledger,
}

impl MessageKind {
    /// Parses a kind name; unknown names are plain messages.
    pub fn parse(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "instruction" | "dispatch" | "broadcast" => Self::Instruction,
            "ask" | "question" | "escalation" => Self::Ask,
            "merge_ready" | "merge-ready" => Self::MergeReady,
            "relay" | "slack" | "envoy" => Self::Relay,
            "nudge" | "whip" => Self::Nudge,
            "status" | "worker_done" | "notify" => Self::Status,
            "ledger" => Self::Ledger,
            _ => Self::Message,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Instruction => "instruction",
            Self::Message => "message",
            Self::Ask => "ask",
            Self::MergeReady => "merge_ready",
            Self::Relay => "relay",
            Self::Nudge => "nudge",
            Self::Status => "status",
            Self::Ledger => "ledger",
        }
    }
}

/// A message from `from` to `to`, named as the sender knows them: a node id, a terminal handle,
/// Orca's `run:<id>` (its coordinator) or `dispatch:<id>`, or `king`, `envoy`, `karl`, `slack` or
/// `ledger`; see [`resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMessage {
    pub from: String,
    pub to: String,
    pub kind: MessageKind,
}

impl AgentMessage {
    pub fn new(from: impl Into<String>, to: impl Into<String>, kind: MessageKind) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            kind,
        }
    }

    /// The control API's `agent_message` params: `from`, `to` and an optional `kind`.
    pub fn from_params(params: &Value) -> Result<Self, ControlError> {
        let p = Params::new(params);
        let from = p.required_str("from")?.trim();
        let to = p.required_str("to")?.trim();
        if from.is_empty() || to.is_empty() {
            return Err(ControlError::invalid_params(
                "\"from\" and \"to\" must not be empty",
            ));
        }
        let kind = p
            .str("kind")?
            .map_or(MessageKind::Message, MessageKind::parse);
        Ok(Self::new(from, to, kind))
    }

    /// Both ends in `snapshot`; `None` when either is unknown or they are the same.
    pub fn resolve(&self, snapshot: &ClanSnapshot) -> Option<Flight> {
        let from = resolve(snapshot, &self.from)?;
        let to = resolve(snapshot, &self.to)?;
        (from != to).then_some(Flight {
            from,
            to,
            kind: self.kind,
        })
    }
}

/// One end of a message as the views draw it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Endpoint {
    Node(NodeId),
    /// Karl himself; the farm draws him as the King.
    Karl,
    /// The world outside the clan (Slack); the farm draws it as the King too.
    Outside,
    /// The Ledger, the clan's knowledge store; the farm draws it as its chamber.
    Ledger,
}

/// A message resolved to the snapshot's nodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Flight {
    pub from: Endpoint,
    pub to: Endpoint,
    pub kind: MessageKind,
}

/// The endpoint `key` names in `snapshot`; see [`AgentMessage`].
pub fn resolve(snapshot: &ClanSnapshot, key: &str) -> Option<Endpoint> {
    let key = key.trim();
    // Snapshots assembled by hand (demos) have no index.
    let node = |id: NodeId| {
        snapshot
            .node(&id)
            .or_else(|| snapshot.nodes.iter().find(|n| n.id == id))
            .map(|n| Endpoint::Node(n.id.clone()))
    };
    match key.to_ascii_lowercase().as_str() {
        "karl" | "human" => return Some(Endpoint::Karl),
        "slack" | "outside" => return Some(Endpoint::Outside),
        "ledger" => return Some(Endpoint::Ledger),
        "king" | "queen" => return node(NodeId::king()),
        whip if is_whip_sender(whip) => return node(NodeId::king()),
        "envoy" => {
            return snapshot
                .nodes
                .iter()
                .find(|n| n.kind == NodeKind::Envoy && n.live)
                .map(|n| Endpoint::Node(n.id.clone()));
        }
        _ => {}
    }
    if let Some(found) = node(NodeId(key.to_owned())) {
        return Some(found);
    }
    if let Some(run_id) = key.strip_prefix("run:") {
        let run = snapshot.runs.iter().find(|r| r.id == run_id)?;
        return node(run.coordinator.clone()?);
    }
    if let Some(dispatch_id) = key.strip_prefix("dispatch:") {
        return snapshot
            .nodes
            .iter()
            .filter(|n| n.dispatch_id.as_deref() == Some(dispatch_id))
            .max_by_key(|n| n.live)
            .map(|n| Endpoint::Node(n.id.clone()));
    }
    node(NodeId::terminal(key)).or_else(|| {
        snapshot
            .nodes
            .iter()
            .filter(|n| n.kind != NodeKind::Subagent)
            .filter(|n| n.terminal_handle.as_deref() == Some(key))
            .max_by_key(|n| n.live)
            .map(|n| Endpoint::Node(n.id.clone()))
    })
}

/// The message a bus message stands for; `None` for heartbeats, the whip's nudges (drawn from
/// its log, which also has those typed into terminals) and messages to a group.
pub fn from_inbox(m: &InboxMessage) -> Option<AgentMessage> {
    let from = m.from_handle.as_deref()?;
    let to = m.to_handle.as_deref()?;
    if is_whip_sender(from) || to.starts_with('@') {
        return None;
    }
    let kind = match &m.message_type {
        Some(MessageType::Heartbeat) => return None,
        Some(MessageType::Question | MessageType::Escalation) => MessageKind::Ask,
        Some(MessageType::MergeReady) => MessageKind::MergeReady,
        Some(MessageType::Dispatch) => MessageKind::Instruction,
        Some(MessageType::Status | MessageType::WorkerDone) => MessageKind::Status,
        _ if is_broadcast_sender(from) => MessageKind::Instruction,
        _ => MessageKind::Message,
    };
    Some(AgentMessage::new(from, to, kind))
}

/// A nudge the whip sent: the King prodding the terminal.
pub fn from_whip(item: &WhipLogItem) -> Option<AgentMessage> {
    let entry = &item.entry;
    let to = item.handle.as_deref()?;
    (entry.action.starts_with("nudge") && entry.outcome == "sent" && !entry.dry_run)
        .then(|| AgentMessage::new("king", to, MessageKind::Nudge))
}

/// Picks the bus messages that are new since the last poll.
#[derive(Debug, Default)]
pub struct InboxWatch {
    /// Ids in the last inbox.
    seen: HashSet<String>,
}

impl InboxWatch {
    /// The messages of `inbox` not in the previous one and created within [`FRESH_MS`] of
    /// `now_ms`. After an empty inbox (the first, before the poller fetched one) everything is
    /// history and nothing is drawn.
    pub fn update(&mut self, inbox: &[InboxMessage], now_ms: i64) -> Vec<AgentMessage> {
        let ids: HashSet<String> = inbox.iter().map(|m| m.id.clone()).collect();
        let seen = std::mem::replace(&mut self.seen, ids);
        if seen.is_empty() {
            return Vec::new();
        }
        inbox
            .iter()
            .filter(|m| !seen.contains(&m.id))
            .filter(|m| {
                let at = m.created_at.as_ref().and_then(|t| t.epoch_ms());
                at.is_none_or(|at| now_ms - at < FRESH_MS)
            })
            .filter_map(from_inbox)
            .collect()
    }
}

/// Sends messages to the app from any thread.
#[derive(Debug, Clone)]
pub struct TrafficSender(Sender<AgentMessage>);

impl TrafficSender {
    pub fn send(&self, message: AgentMessage) {
        let _ = self.0.send(message);
    }
}

/// The app's queue of messages to draw.
#[derive(Debug)]
pub struct Traffic {
    tx: Sender<AgentMessage>,
    rx: Receiver<AgentMessage>,
    pub inbox: InboxWatch,
    /// Tool calls waiting for their ends to show up in the clan, with when they came (egui
    /// time); see [`beam`].
    pub calls: Vec<(beam::ToolCall, Option<f64>)>,
}

impl Default for Traffic {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            tx,
            rx,
            inbox: InboxWatch::default(),
            calls: Vec::new(),
        }
    }
}

impl Traffic {
    pub fn sender(&self) -> TrafficSender {
        TrafficSender(self.tx.clone())
    }

    pub fn push(&self, message: AgentMessage) {
        let _ = self.tx.send(message);
    }

    /// Everything sent since the last call.
    pub fn drain(&self) -> Vec<AgentMessage> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests;

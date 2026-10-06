//! Protocol-independent messages and the app-owned channel boundary.

use serde::{Deserialize, Serialize};

use super::BridgeStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Mention,
    Dm,
    AgentComms,
    ThreadReply,
    /// A message newly pinned in a conversation Karl can see.
    Pin,
}

impl EventKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Mention => "mention",
            Self::Dm => "dm",
            Self::AgentComms => "agent_comms",
            Self::ThreadReply => "thread_reply",
            Self::Pin => "pin",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundEvent {
    pub channel_kind: String,
    pub conversation: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub user: String,
    pub user_name: String,
    pub text: String,
    pub kind: EventKind,
    pub permalink: Option<String>,
}

impl InboundEvent {
    pub fn id(&self) -> String {
        format!("{}:{}", self.conversation, self.ts)
    }

    pub fn summary(&self) -> String {
        fn line(s: &str, max: usize) -> String {
            s.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(max)
                .collect()
        }
        format!(
            "Envoy event: {} from {} in {}: {} — id {}",
            self.kind.name(),
            line(&self.user_name, 80),
            line(&self.conversation, 80),
            line(&self.text, 120),
            self.id()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub conversation: String,
    pub thread_ts: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendReceipt {
    pub conversation: String,
    pub ts: String,
}

#[derive(Debug, Clone, Default)]
pub struct PollBatch {
    pub events: Vec<InboundEvent>,
    pub notices: Vec<String>,
}

pub trait Channel: Send {
    fn poll(&mut self, now_ms: i64) -> Result<PollBatch, String>;
    fn send(&mut self, approved: &OutboundMessage) -> Result<SendReceipt, String>;
    fn health(&self) -> BridgeStatus;
}

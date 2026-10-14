//! The conversation as the pane shows it, built by applying CLI messages in order: user
//! bubbles, assistant text streaming in from deltas, tool calls paired with their results,
//! permission cards and inline notices.

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::Value;

use super::context::ContextGauge;
use super::history::COMPACTED_NOTICE;
use super::protocol::{
    BlockKind, ContentBlock, Incoming, Init, PermissionRequest, StreamEvent, TurnResult,
};

/// Text the CLI puts in a user message when the host interrupts a turn.
const INTERRUPTED_NOTE: &str = "[Request interrupted by user]";

#[derive(Debug, Clone, PartialEq)]
pub enum ChatItem {
    User(String),
    /// An incoming agent message, never a Karl bubble.
    Inbox(Box<crate::king_inbox::InboxMessage>),
    /// Karl's message spoken with hold-to-talk (drawn with a 🎤 marker).
    VoiceUser(String),
    /// Reply text as streamed, `<say>` tags included; the pane shows the say-line apart
    /// (see [`split_say`](super::say::split_say)).
    Assistant {
        text: String,
        streaming: bool,
    },
    Tool(ToolCall),
    Permission(PermissionCard),
    Notice(String),
    /// A note from Stronghold itself (e.g. auto mode changed), not typed by Karl.
    System(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// `None` until the finished assistant message carries it.
    pub input: Option<Value>,
    pub result: Option<ToolOutcome>,
    /// Allowed without a card because auto mode was on.
    pub auto: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
}

/// A permission prompt; `decision` is `None` while it waits for the user.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionCard {
    pub request: PermissionRequest,
    pub decision: Option<Decision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Allow, and every later call of this tool in this chat.
    AlwaysAllow,
    Deny,
    /// The session ended before an answer.
    Cancelled,
    /// Auto mode answered it.
    Auto,
}

#[derive(Debug, Default)]
pub struct Transcript {
    pub items: Vec<ChatItem>,
    /// Source times keyed by item index; absent for legacy records without a timestamp.
    timestamps: HashMap<usize, jiff::Timestamp>,
    /// From the newest `system/init`.
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub tools: Vec<String>,
    /// A turn is running (a message was sent and its `result` has not arrived).
    pub running: bool,
    /// The newest turn's result.
    pub last_result: Option<TurnResult>,
    /// Summed cost of the turns seen in this pane.
    pub cost_usd: f64,
    /// Input tokens of the newest top-level API call: the context in use. `None` until one
    /// is seen, and again after a compaction.
    pub context_tokens: Option<u64>,
    /// The context window the CLI reported with the newest turn result.
    pub context_window: Option<u64>,
    /// Message id of the message being streamed.
    current_message: Option<String>,
    /// Stream block index → item index, for the message being streamed.
    stream_blocks: HashMap<u64, usize>,
    /// Per message id, streamed text items not yet replaced by their finished block.
    unfinished_text: HashMap<String, VecDeque<usize>>,
    /// Tool use id → item index.
    tools_by_id: HashMap<String, usize>,
    /// Tool use ids auto mode allowed before their row appeared.
    auto_ids: HashSet<String>,
}

impl Transcript {
    pub fn timestamp(&self, index: usize) -> Option<jiff::Timestamp> {
        self.timestamps.get(&index).copied()
    }

    pub(super) fn set_timestamps(&mut self, start: usize, timestamp: Option<jiff::Timestamp>) {
        for index in start..self.items.len() {
            if let Some(timestamp) = timestamp {
                self.timestamps.insert(index, timestamp);
            } else {
                self.timestamps.remove(&index);
            }
        }
    }

    /// Stronghold-authored messages are timed at creation, never during rendering.
    fn push_local(&mut self, item: ChatItem) {
        self.timestamps
            .insert(self.items.len(), jiff::Timestamp::now());
        self.items.push(item);
    }

    /// CLI messages are timed at the source or when received by the supervisor.
    pub fn apply_at(&mut self, message: &Incoming, timestamp: jiff::Timestamp) {
        let start = self.items.len();
        self.apply(message);
        self.set_timestamps(start, Some(timestamp));
    }

    pub fn push_inbox(&mut self, message: &crate::king_inbox::InboxMessage) {
        if let Some(ChatItem::Inbox(existing)) = self
            .items
            .iter_mut()
            .find(|item| matches!(item, ChatItem::Inbox(m) if m.id == message.id))
        {
            **existing = message.clone();
        } else {
            self.push_local(ChatItem::Inbox(Box::new(message.clone())));
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.push_local(ChatItem::User(text.to_owned()));
        self.running = true;
    }

    /// A message Karl spoke (hold-to-talk).
    pub fn push_voice_user(&mut self, text: &str) {
        self.push_local(ChatItem::VoiceUser(text.to_owned()));
        self.running = true;
    }

    /// Puts `earlier` (the conversation before this one was opened) in front of the items.
    pub fn prepend(&mut self, earlier: Transcript) {
        let shift = earlier.items.len();
        self.timestamps = std::mem::take(&mut self.timestamps)
            .into_iter()
            .map(|(index, timestamp)| (index + shift, timestamp))
            .chain(earlier.timestamps)
            .collect();
        let mut items = earlier.items;
        items.append(&mut self.items);
        self.items = items;
        for at in self.stream_blocks.values_mut() {
            *at += shift;
        }
        for at in self.unfinished_text.values_mut().flatten() {
            *at += shift;
        }
        for at in self.tools_by_id.values_mut() {
            *at += shift;
        }
        for (id, at) in earlier.tools_by_id {
            self.tools_by_id.entry(id).or_insert(at);
        }
        if self.session_id.is_none() {
            self.session_id = earlier.session_id;
        }
        if self.model.is_none() {
            self.model = earlier.model;
        }
        if self.context_tokens.is_none() {
            self.context_tokens = earlier.context_tokens;
        }
    }

    /// The context gauge, once a message reported its usage.
    pub fn context_gauge(&self) -> Option<ContextGauge> {
        self.context_tokens
            .map(|tokens| ContextGauge::new(tokens, self.context_window, self.model.as_deref()))
    }

    /// The CLI compacted the conversation: a notice, and the context size is unknown until
    /// the next message.
    pub fn compacted(&mut self) {
        self.push_notice(COMPACTED_NOTICE);
        self.context_tokens = None;
    }

    pub fn push_notice(&mut self, text: impl Into<String>) {
        self.push_local(ChatItem::Notice(text.into()));
    }

    pub fn push_system(&mut self, text: impl Into<String>) {
        self.push_local(ChatItem::System(text.into()));
    }

    /// Records that auto mode allowed `request`: its waiting card (if any) closes and its tool
    /// row gets the auto marker.
    pub fn mark_auto(&mut self, request: &PermissionRequest) {
        self.decide(&request.request_id, Decision::Auto);
        let Some(id) = &request.tool_use_id else {
            return;
        };
        match self
            .tools_by_id
            .get(id)
            .and_then(|&at| self.items.get_mut(at))
        {
            Some(ChatItem::Tool(call)) => call.auto = true,
            _ => {
                self.auto_ids.insert(id.clone());
            }
        }
    }

    pub fn push_error(&mut self, text: impl Into<String>) {
        self.push_local(ChatItem::Error(text.into()));
    }

    /// The first permission card still waiting for an answer.
    pub fn pending_permission(&self) -> Option<&PermissionCard> {
        self.items.iter().find_map(|item| match item {
            ChatItem::Permission(card) if card.decision.is_none() => Some(card),
            _ => None,
        })
    }

    pub fn push_permission(&mut self, request: PermissionRequest) {
        self.push_local(ChatItem::Permission(PermissionCard {
            request,
            decision: None,
        }));
    }

    /// Records the answer to the card of `request_id`; false if there is no such open card.
    pub fn decide(&mut self, request_id: &str, decision: Decision) -> bool {
        for item in self.items.iter_mut().rev() {
            if let ChatItem::Permission(card) = item
                && card.request.request_id == request_id
                && card.decision.is_none()
            {
                card.decision = Some(decision);
                return true;
            }
        }
        false
    }

    /// The session went away mid-turn: open cards are cancelled, streaming text is final and
    /// the turn is over.
    pub fn session_ended(&mut self) {
        for item in &mut self.items {
            match item {
                ChatItem::Permission(card) if card.decision.is_none() => {
                    card.decision = Some(Decision::Cancelled);
                }
                ChatItem::Assistant { streaming, .. } => *streaming = false,
                _ => {}
            }
        }
        self.running = false;
        self.current_message = None;
        self.stream_blocks.clear();
        self.unfinished_text.clear();
    }

    /// Applies one message from the CLI.
    pub fn apply(&mut self, message: &Incoming) {
        match message {
            Incoming::Init(init) => self.apply_init(init),
            // Subagent traffic stays inside its Task tool row.
            Incoming::Stream {
                parent: Some(_), ..
            }
            | Incoming::Assistant {
                parent: Some(_), ..
            }
            | Incoming::User {
                parent: Some(_), ..
            } => {}
            Incoming::Stream { event, .. } => self.apply_stream(event),
            Incoming::Assistant {
                message_id,
                model,
                content,
                usage,
                ..
            } => {
                if model.is_some() {
                    self.model = model.clone();
                }
                // Synthetic messages (an interrupt note) carry all-zero usage.
                if let Some(tokens) = usage.map(|u| u.context_tokens()).filter(|&t| t > 0) {
                    self.context_tokens = Some(tokens);
                }
                self.apply_assistant(message_id, content);
            }
            Incoming::User { content, .. } => self.apply_user(content),
            Incoming::Result(result) => self.apply_result(result),
            Incoming::Other(kind) if kind == super::protocol::COMPACT_BOUNDARY => self.compacted(),
            Incoming::Status(_)
            | Incoming::ControlRequest { .. }
            | Incoming::ControlResponse { .. }
            | Incoming::Other(_) => {}
        }
    }

    fn apply_init(&mut self, init: &Init) {
        self.session_id = Some(init.session_id.clone());
        if init.model.is_some() {
            self.model = init.model.clone();
        }
        self.tools = init.tools.clone();
    }

    fn apply_stream(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::MessageStart { message_id, model } => {
                if model.is_some() {
                    self.model = model.clone();
                }
                self.current_message = Some(message_id.clone());
                self.stream_blocks.clear();
            }
            StreamEvent::BlockStart { index, block } => match block {
                BlockKind::Text => {
                    let at = self.items.len();
                    self.items.push(ChatItem::Assistant {
                        text: String::new(),
                        streaming: true,
                    });
                    self.stream_blocks.insert(*index, at);
                    if let Some(id) = &self.current_message {
                        self.unfinished_text
                            .entry(id.clone())
                            .or_default()
                            .push_back(at);
                    }
                }
                BlockKind::ToolUse { id, name } => self.tool_item(id, name),
                BlockKind::Thinking | BlockKind::Other => {}
            },
            StreamEvent::TextDelta { index, text } => {
                if let Some(&at) = self.stream_blocks.get(index)
                    && let Some(ChatItem::Assistant { text: t, .. }) = self.items.get_mut(at)
                {
                    t.push_str(text);
                }
            }
            StreamEvent::BlockStop { .. } | StreamEvent::MessageStop | StreamEvent::Other => {}
        }
    }

    fn apply_assistant(&mut self, message_id: &str, content: &[ContentBlock]) {
        for block in content {
            match block {
                ContentBlock::Text(text) => {
                    let streamed = self
                        .unfinished_text
                        .get_mut(message_id)
                        .and_then(VecDeque::pop_front);
                    match streamed.and_then(|at| self.items.get_mut(at)) {
                        Some(ChatItem::Assistant {
                            text: t, streaming, ..
                        }) => {
                            *t = text.clone();
                            *streaming = false;
                        }
                        _ => self.items.push(ChatItem::Assistant {
                            text: text.clone(),
                            streaming: false,
                        }),
                    }
                }
                ContentBlock::ToolUse { id, name, input } => {
                    self.tool_item(id, name);
                    if let Some(ChatItem::Tool(call)) = self
                        .tools_by_id
                        .get(id)
                        .and_then(|&at| self.items.get_mut(at))
                    {
                        call.input = Some(input.clone());
                    }
                }
                ContentBlock::Thinking | ContentBlock::ToolResult { .. } | ContentBlock::Other => {}
            }
        }
    }

    /// The tool row of `id`, created if new.
    fn tool_item(&mut self, id: &str, name: &str) {
        if self.tools_by_id.contains_key(id) {
            return;
        }
        self.tools_by_id.insert(id.to_owned(), self.items.len());
        self.items.push(ChatItem::Tool(ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            input: None,
            result: None,
            auto: self.auto_ids.remove(id),
        }));
    }

    fn apply_user(&mut self, content: &[ContentBlock]) {
        for block in content {
            match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    text,
                    is_error,
                } => {
                    if let Some(ChatItem::Tool(call)) = self
                        .tools_by_id
                        .get(tool_use_id)
                        .and_then(|&at| self.items.get_mut(at))
                    {
                        call.result = Some(ToolOutcome {
                            text: text.clone(),
                            is_error: *is_error,
                        });
                    }
                }
                ContentBlock::Text(text) if text.trim() == INTERRUPTED_NOTE => {
                    self.push_notice("Interrupted");
                }
                _ => {}
            }
        }
    }

    fn apply_result(&mut self, result: &TurnResult) {
        for item in &mut self.items {
            if let ChatItem::Assistant { streaming, .. } = item {
                *streaming = false;
            }
        }
        self.unfinished_text.clear();
        self.stream_blocks.clear();
        self.current_message = None;
        if result.is_error && !result.interrupted() {
            let detail = if result.errors.is_empty() {
                result
                    .result
                    .clone()
                    .unwrap_or_else(|| result.subtype.clone())
            } else {
                result.errors.join("\n")
            };
            self.push_error(format!("Turn failed: {detail}"));
        }
        self.cost_usd += result.cost_usd.unwrap_or_default();
        if result.context_window.is_some() {
            self.context_window = result.context_window;
        }
        self.last_result = Some(result.clone());
        self.running = false;
    }

    /// Text of all assistant items, for logging and tests.
    pub fn assistant_text(&self) -> Vec<&str> {
        self.items
            .iter()
            .filter_map(|item| match item {
                ChatItem::Assistant { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The say-lines of the assistant items, tags removed.
    pub fn say_lines(&self) -> Vec<String> {
        self.assistant_text()
            .into_iter()
            .filter_map(|text| super::say::split_say(text).say)
            .collect()
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.items.iter().filter_map(|item| match item {
            ChatItem::Tool(call) => Some(call),
            _ => None,
        })
    }
}

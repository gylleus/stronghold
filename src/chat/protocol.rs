//! The `claude -p --input-format stream-json --output-format stream-json` wire protocol: one
//! JSON object per line each way. Recordings are in `tests/fixtures/chat/`.
//!
//! From the CLI: `system` (`init`, `status`, hook events…), `stream_event` (raw API stream
//! events, with `--include-partial-messages`), `assistant` (one per finished content block),
//! `user` (tool results, interrupt notes), `result` (end of a turn), `control_request`
//! (`can_use_tool` permission prompts) and `control_response` (answers to the host's requests).
//!
//! From the host: `user` messages, `control_request` (`initialize`, `interrupt`) and
//! `control_response` (answers to `can_use_tool`).

use serde_json::{Value, json};

/// [`Incoming::Other`] of the `system/compact_boundary` the CLI sends when it compacts.
pub const COMPACT_BOUNDARY: &str = "system/compact_boundary";

/// One parsed line from the CLI.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Init(Init),
    /// `system/status`; `Some("requesting")` while an API request is in flight.
    Status(Option<String>),
    /// A partial-message event. `parent` is set for events of a subagent (a Task tool call).
    Stream {
        parent: Option<String>,
        event: StreamEvent,
    },
    /// A finished assistant message; the CLI sends one per content block, repeating the
    /// message id.
    Assistant {
        parent: Option<String>,
        message_id: String,
        model: Option<String>,
        content: Vec<ContentBlock>,
        /// The API call's token usage; its input side is how full the context is.
        usage: Option<Usage>,
    },
    /// Tool results and notes such as `[Request interrupted by user]`.
    User {
        parent: Option<String>,
        content: Vec<ContentBlock>,
    },
    Result(TurnResult),
    /// A request the host must answer with a `control_response`.
    ControlRequest {
        request_id: String,
        request: HostRequest,
    },
    /// The CLI's answer to a request of the host.
    ControlResponse {
        request_id: String,
        /// The error message when the request failed.
        error: Option<String>,
        response: Value,
    },
    /// Anything else (hook events, rate limit notes, …), by type/subtype.
    Other(String),
}

/// `system/init`: sent at the start of each turn's first request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Init {
    pub session_id: String,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub tools: Vec<String>,
    pub permission_mode: Option<String>,
    /// `(name, status)` of each MCP server; they connect after init, so `pending` is normal.
    pub mcp_servers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    MessageStart {
        message_id: String,
        model: Option<String>,
    },
    BlockStart {
        index: u64,
        block: BlockKind,
    },
    TextDelta {
        index: u64,
        text: String,
    },
    BlockStop {
        index: u64,
    },
    MessageStop,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockKind {
    Text,
    Thinking,
    ToolUse { id: String, name: String },
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Text(String),
    Thinking,
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        /// The result flattened to text (text blocks joined, tool references by name).
        text: String,
        is_error: bool,
    },
    Other,
}

/// Token usage of one API call (an assistant message's `usage`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    /// Tokens the request sent: the conversation's current context size.
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }

    fn parse(v: Option<&Value>) -> Option<Self> {
        let v = v?.as_object()?;
        let n = |key: &str| v.get(key).and_then(Value::as_u64).unwrap_or_default();
        Some(Self {
            input_tokens: n("input_tokens"),
            cache_read_input_tokens: n("cache_read_input_tokens"),
            cache_creation_input_tokens: n("cache_creation_input_tokens"),
            output_tokens: n("output_tokens"),
        })
    }
}

/// `result`: the end of a turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnResult {
    /// `success`, `error_during_execution`, `error_max_turns`, …
    pub subtype: String,
    pub is_error: bool,
    /// The final text of a successful turn.
    pub result: Option<String>,
    pub errors: Vec<String>,
    /// e.g. `completed`, `aborted_streaming` (interrupted).
    pub terminal_reason: Option<String>,
    pub duration_ms: Option<u64>,
    pub num_turns: Option<u64>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// The largest `contextWindow` in `modelUsage`, when the CLI reports one.
    pub context_window: Option<u64>,
}

impl TurnResult {
    /// Whether the turn ended because the host interrupted it.
    pub fn interrupted(&self) -> bool {
        self.terminal_reason
            .as_deref()
            .is_some_and(|r| r.starts_with("aborted"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostRequest {
    CanUseTool(PermissionRequest),
    /// A request subtype this host does not handle; answered with an error.
    Other(String),
}

/// A `can_use_tool` request: the CLI asks whether a tool call may run.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub display_name: Option<String>,
    pub input: Value,
    pub tool_use_id: Option<String>,
    /// A short description of the call, e.g. the file a Write touches.
    pub description: Option<String>,
}

/// The host's answer to a [`PermissionRequest`].
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionAnswer {
    Allow,
    Deny { message: String },
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Synthetic interrupt/error messages have no running model.
fn model_of(v: &Value) -> Option<String> {
    str_of(v, "model").filter(|m| !m.trim().is_empty() && m != "<synthetic>")
}

/// Source time, when the CLI supplies one. Missing or malformed times stay unknown.
pub fn timestamp_of(v: &Value) -> Option<jiff::Timestamp> {
    v.get("timestamp")?.as_str()?.parse().ok()
}

fn parent_of(v: &Value) -> Option<String> {
    str_of(v, "parent_tool_use_id")
}

/// Parses one line from the CLI.
pub fn parse_line(line: &str) -> Result<Incoming, serde_json::Error> {
    let v: Value = serde_json::from_str(line)?;
    Ok(parse_value(&v))
}

pub fn parse_value(v: &Value) -> Incoming {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "system" => parse_system(v),
        "stream_event" => Incoming::Stream {
            parent: parent_of(v),
            event: parse_stream_event(v.get("event").unwrap_or(&Value::Null)),
        },
        "assistant" => {
            let message = v.get("message").unwrap_or(&Value::Null);
            Incoming::Assistant {
                parent: parent_of(v),
                message_id: str_of(message, "id").unwrap_or_default(),
                model: model_of(message),
                content: parse_content(message.get("content")),
                usage: Usage::parse(message.get("usage")),
            }
        }
        "user" => Incoming::User {
            parent: parent_of(v),
            content: parse_content(v.get("message").and_then(|m| m.get("content"))),
        },
        "result" => Incoming::Result(parse_result(v)),
        "control_request" => {
            let request_id = str_of(v, "request_id").unwrap_or_default();
            let request = v.get("request").unwrap_or(&Value::Null);
            let subtype = str_of(request, "subtype").unwrap_or_default();
            let request = if subtype == "can_use_tool" {
                HostRequest::CanUseTool(PermissionRequest {
                    request_id: request_id.clone(),
                    tool_name: str_of(request, "tool_name").unwrap_or_default(),
                    display_name: str_of(request, "display_name"),
                    input: request.get("input").cloned().unwrap_or(json!({})),
                    tool_use_id: str_of(request, "tool_use_id"),
                    description: str_of(request, "description"),
                })
            } else {
                HostRequest::Other(subtype)
            };
            Incoming::ControlRequest {
                request_id,
                request,
            }
        }
        "control_response" => {
            let response = v.get("response").unwrap_or(&Value::Null);
            let error = (str_of(response, "subtype").as_deref() == Some("error"))
                .then(|| str_of(response, "error").unwrap_or_else(|| "error".into()));
            Incoming::ControlResponse {
                request_id: str_of(response, "request_id").unwrap_or_default(),
                error,
                response: response.get("response").cloned().unwrap_or(Value::Null),
            }
        }
        other => Incoming::Other(other.to_owned()),
    }
}

fn parse_system(v: &Value) -> Incoming {
    match v.get("subtype").and_then(Value::as_str).unwrap_or_default() {
        "init" => Incoming::Init(Init {
            session_id: str_of(v, "session_id").unwrap_or_default(),
            model: model_of(v),
            cwd: str_of(v, "cwd"),
            tools: v
                .get("tools")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            permission_mode: str_of(v, "permissionMode"),
            mcp_servers: v
                .get("mcp_servers")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|s| Some((str_of(s, "name")?, str_of(s, "status")?)))
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "status" => Incoming::Status(str_of(v, "status")),
        other => Incoming::Other(format!("system/{other}")),
    }
}

fn parse_stream_event(e: &Value) -> StreamEvent {
    let index = || e.get("index").and_then(Value::as_u64).unwrap_or_default();
    match e.get("type").and_then(Value::as_str).unwrap_or_default() {
        "message_start" => StreamEvent::MessageStart {
            model: e.get("message").and_then(model_of),
            message_id: e
                .get("message")
                .and_then(|m| str_of(m, "id"))
                .unwrap_or_default(),
        },
        "content_block_start" => {
            let block = e.get("content_block").unwrap_or(&Value::Null);
            let block = match block.get("type").and_then(Value::as_str) {
                Some("text") => BlockKind::Text,
                Some("thinking") => BlockKind::Thinking,
                Some("tool_use") => BlockKind::ToolUse {
                    id: str_of(block, "id").unwrap_or_default(),
                    name: str_of(block, "name").unwrap_or_default(),
                },
                _ => BlockKind::Other,
            };
            StreamEvent::BlockStart {
                index: index(),
                block,
            }
        }
        "content_block_delta" => {
            let delta = e.get("delta").unwrap_or(&Value::Null);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => StreamEvent::TextDelta {
                    index: index(),
                    text: str_of(delta, "text").unwrap_or_default(),
                },
                _ => StreamEvent::Other,
            }
        }
        "content_block_stop" => StreamEvent::BlockStop { index: index() },
        "message_stop" => StreamEvent::MessageStop,
        _ => StreamEvent::Other,
    }
}

fn parse_content(content: Option<&Value>) -> Vec<ContentBlock> {
    match content {
        Some(Value::String(s)) => vec![ContentBlock::Text(s.clone())],
        Some(Value::Array(blocks)) => blocks.iter().map(parse_block).collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn parse_block(b: &Value) -> ContentBlock {
    match b.get("type").and_then(Value::as_str).unwrap_or_default() {
        "text" => ContentBlock::Text(str_of(b, "text").unwrap_or_default()),
        "thinking" | "redacted_thinking" => ContentBlock::Thinking,
        "tool_use" => ContentBlock::ToolUse {
            id: str_of(b, "id").unwrap_or_default(),
            name: str_of(b, "name").unwrap_or_default(),
            input: b.get("input").cloned().unwrap_or(json!({})),
        },
        "tool_result" => ContentBlock::ToolResult {
            tool_use_id: str_of(b, "tool_use_id").unwrap_or_default(),
            text: flatten_result(b.get("content")),
            is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        },
        _ => ContentBlock::Other,
    }
}

/// A tool result's content as text: a string as is, text blocks joined by newlines, tool
/// references by name.
fn flatten_result(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => str_of(b, "text"),
                Some("tool_reference") => str_of(b, "tool_name"),
                Some(other) => Some(format!("[{other}]")),
                None => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    }
}

fn parse_result(v: &Value) -> TurnResult {
    let usage = v.get("usage");
    let tokens = |key| usage.and_then(|u| u.get(key)).and_then(Value::as_u64);
    TurnResult {
        subtype: str_of(v, "subtype").unwrap_or_default(),
        is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        result: str_of(v, "result"),
        errors: v
            .get("errors")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        terminal_reason: str_of(v, "terminal_reason"),
        duration_ms: v.get("duration_ms").and_then(Value::as_u64),
        num_turns: v.get("num_turns").and_then(Value::as_u64),
        cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
        input_tokens: tokens("input_tokens"),
        output_tokens: tokens("output_tokens"),
        context_window: v
            .get("modelUsage")
            .and_then(Value::as_object)
            .and_then(|models| {
                models
                    .values()
                    .filter_map(|m| m.get("contextWindow").and_then(Value::as_u64))
                    .max()
            }),
    }
}

/// A user message carrying `text`.
pub fn user_message(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
}

/// The `initialize` request the host sends first.
pub fn initialize_request(request_id: &str) -> String {
    control_request(request_id, json!({"subtype": "initialize"}))
}

/// Interrupts the running turn; the CLI answers and ends the turn with an `aborted_*` result.
pub fn interrupt_request(request_id: &str) -> String {
    control_request(request_id, json!({"subtype": "interrupt"}))
}

fn control_request(request_id: &str, request: Value) -> String {
    json!({"type": "control_request", "request_id": request_id, "request": request}).to_string()
}

/// Answers a `can_use_tool` request; an allow passes the tool's input back unchanged.
pub fn permission_response(request: &PermissionRequest, answer: &PermissionAnswer) -> String {
    let response = match answer {
        PermissionAnswer::Allow => json!({"behavior": "allow", "updatedInput": request.input}),
        PermissionAnswer::Deny { message } => json!({"behavior": "deny", "message": message}),
    };
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request.request_id, "response": response},
    })
    .to_string()
}

/// Declines a control request this host does not handle.
pub fn error_response(request_id: &str, message: &str) -> String {
    json!({
        "type": "control_response",
        "response": {"subtype": "error", "request_id": request_id, "error": message},
    })
    .to_string()
}

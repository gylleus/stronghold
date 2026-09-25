//! Serde types for `orca … --json` responses.
//!
//! Decoding is lenient: missing or `null` fields fall back to defaults, unknown
//! fields are ignored, and unknown enum values land in an `Other(String)` variant.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Defines a string enum that keeps unrecognised values in `Other(String)`.
macro_rules! lenient_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant,)*
            Other(String),
        }

        impl $name {
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $text,)*
                    Self::Other(s) => s,
                }
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                match s {
                    $($text => Self::$variant,)*
                    other => Self::Other(other.to_string()),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Ok(Self::from(s.as_str()))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }
    };
}

lenient_enum!(
    /// Which coding agent runs in a terminal.
    AgentIdentity { Claude => "claude", Codex => "codex" }
);

lenient_enum!(
    /// Process status reported by `terminal read`.
    TerminalStatus { Running => "running", Exited => "exited" }
);

lenient_enum!(
    /// Where `terminal read` got its lines from.
    ReadSource { Stream => "stream", Screen => "screen", ScreenUnavailable => "screen-unavailable" }
);

lenient_enum!(
    /// Orchestration task status.
    TaskStatus {
        Pending => "pending",
        Ready => "ready",
        Dispatched => "dispatched",
        Completed => "completed",
        Failed => "failed",
        Blocked => "blocked",
    }
);

lenient_enum!(
    /// Supervisor view of a worker (`worker-list` `workerState`).
    WorkerState {
        Ready => "ready",
        Succeeded => "succeeded",
        Failed => "failed",
        Stopped => "stopped",
        StopUnknown => "stop_unknown",
        Unsupervised => "unsupervised",
    }
);

lenient_enum!(
    /// Dispatch status (`worker-list` `dispatchStatus`).
    DispatchStatus {
        Dispatched => "dispatched",
        Completed => "completed",
        Failed => "failed",
        CircuitBroken => "circuit_broken",
    }
);

lenient_enum!(
    /// Terminal resource accounting (`worker-list` `terminalState`).
    WorkerTerminalState {
        Active => "active",
        Reclaimable => "reclaimable",
        Retained => "retained",
        ReleasePending => "release_pending",
        ReleaseUnknown => "release_unknown",
        Released => "released",
    }
);

lenient_enum!(
    /// Bus message type.
    MessageType {
        Heartbeat => "heartbeat",
        WorkerDone => "worker_done",
        MergeReady => "merge_ready",
        Question => "question",
        Status => "status",
        Escalation => "escalation",
        Dispatch => "dispatch",
    }
);

/// A timestamp as Orca sends it: ms since the epoch, or an ISO-ish UTC string
/// such as `2026-09-23T08:37:04Z` or `2026-09-23 08:37:19`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Timestamp {
    Millis(i64),
    Text(String),
}

impl Timestamp {
    /// Milliseconds since the Unix epoch, if the value is parseable.
    pub fn epoch_ms(&self) -> Option<i64> {
        match self {
            Timestamp::Millis(ms) => Some(*ms),
            Timestamp::Text(s) => parse_timestamp_ms(s),
        }
    }

    pub fn system_time(&self) -> Option<SystemTime> {
        let ms = self.epoch_ms()?;
        let offset = Duration::from_millis(ms.unsigned_abs());
        if ms >= 0 {
            UNIX_EPOCH.checked_add(offset)
        } else {
            UNIX_EPOCH.checked_sub(offset)
        }
    }
}

/// Parses `YYYY-MM-DD[T ]HH:MM:SS[.fff][Z|±HH:MM]` (no zone means UTC) or a bare
/// integer of ms into ms since the epoch.
pub fn parse_timestamp_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().ok();
    }
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, min, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }

    let mut rest = &s[19..];
    let mut millis = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let padded = format!("{:0<3}", &frac[..digits.min(3)]);
        millis = padded.parse::<i64>().ok()?;
        rest = &frac[digits..];
    }
    let offset_min = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let (sign, tz) = match rest.as_bytes()[0] {
                b'+' => (1, &rest[1..]),
                b'-' => (-1, &rest[1..]),
                _ => return None,
            };
            let tz = tz.replace(':', "");
            if tz.len() != 4 || !tz.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            sign * (tz[..2].parse::<i64>().ok()? * 60 + tz[2..].parse::<i64>().ok()?)
        }
    };

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + min * 60 + sec - offset_min * 60;
    Some(secs * 1_000 + millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Treats `null` like a missing field.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Accepts `true`/`false`, `0`/`1` or `"true"`/`"1"`.
fn boolish<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::Bool(b)) => b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => matches!(s.as_str(), "true" | "1"),
        _ => false,
    })
}

/// Accepts a non-negative integer as a number or a numeric string (Orca's cursors).
fn lenient_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

/// Accepts a string list either as a JSON array or as a string holding one.
fn json_string_list<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let value = match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) => serde_json::from_str(&s).unwrap_or(Value::Null),
        Some(v) => v,
        None => Value::Null,
    };
    Ok(match value {
        Value::Array(items) => items
            .into_iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s),
                Value::Null => None,
                other => Some(other.to_string()),
            })
            .collect(),
        _ => Vec::new(),
    })
}

/// Keeps a field that Orca sends as a JSON-encoded string as raw text; a
/// structured value is re-encoded so it can still be parsed later.
fn raw_json_text<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s),
        Some(v) => Some(v.to_string()),
    })
}

// ---------------------------------------------------------------------------
// Terminals

/// One row of `orca terminal list`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Terminal {
    pub handle: String,
    pub pty_id: Option<String>,
    pub incarnation_id: Option<String>,
    pub worktree_id: Option<String>,
    pub worktree_path: Option<String>,
    /// Full ref, e.g. `refs/heads/main`.
    pub branch: Option<String>,
    pub tab_id: Option<String>,
    pub leaf_id: Option<String>,
    pub title: Option<String>,
    #[serde(deserialize_with = "boolish")]
    pub connected: bool,
    #[serde(deserialize_with = "boolish")]
    pub writable: bool,
    pub last_output_at: Option<Timestamp>,
    pub preview: Option<String>,
    pub execution_host_id: Option<String>,
    pub agent_identity: Option<AgentIdentity>,
    #[serde(deserialize_with = "boolish")]
    pub orphaned: bool,
}

impl Terminal {
    /// Branch without the `refs/heads/` prefix.
    pub fn branch_name(&self) -> Option<&str> {
        let branch = self.branch.as_deref()?;
        Some(branch.strip_prefix("refs/heads/").unwrap_or(branch))
    }
}

/// `result` of `orca terminal list`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TerminalList {
    #[serde(deserialize_with = "null_default")]
    pub terminals: Vec<Terminal>,
    pub total_count: Option<u64>,
    #[serde(deserialize_with = "boolish")]
    pub truncated: bool,
}

/// `result.terminal` of `orca terminal read`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRead {
    #[serde(default)]
    pub handle: String,
    #[serde(default)]
    pub status: Option<TerminalStatus>,
    #[serde(default, deserialize_with = "null_default")]
    pub tail: Vec<String>,
    #[serde(default, deserialize_with = "boolish")]
    pub truncated: bool,
    #[serde(default, deserialize_with = "boolish")]
    pub limited: bool,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub oldest_cursor: Option<u64>,
    /// Pass back as `--cursor` to read only newer output.
    #[serde(default, deserialize_with = "lenient_u64")]
    pub next_cursor: Option<u64>,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub latest_cursor: Option<u64>,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub returned_line_count: Option<u64>,
    #[serde(default)]
    pub source: Option<ReadSource>,
    /// UI-only composer text, not part of `tail`.
    #[serde(default)]
    pub draft: Option<String>,
}

/// `result.send` of `orca terminal send`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TerminalSend {
    pub handle: Option<String>,
    /// `false` when Orca skipped the write (terminal locked or not writable); the call still
    /// succeeds.
    pub accepted: Option<bool>,
    pub bytes_written: Option<u64>,
    /// Why a guarded send was refused.
    pub refused_reason: Option<String>,
}

/// `result.focus` of `orca terminal switch`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TerminalFocus {
    pub handle: Option<String>,
    pub tab_id: Option<String>,
    /// `false` when the focus request was superseded or skipped.
    pub navigated: Option<bool>,
}

/// Input for `orca terminal send`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerminalInput {
    pub text: Option<String>,
    /// Append Enter after the text.
    pub enter: bool,
    /// Send as an interrupt (Esc/Ctrl-C style) where supported.
    pub interrupt: bool,
}

// ---------------------------------------------------------------------------
// Orchestration

/// One row of `orca orchestration run-list`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Run {
    pub id: String,
    pub objective: Option<String>,
    pub coordinator_handle: Option<String>,
    pub coordinator_pane_key: Option<String>,
    pub home_database: Option<String>,
    pub consumer_generation: Option<i64>,
    #[serde(deserialize_with = "boolish")]
    pub legacy: bool,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

/// Terminal resource attached to a worker in `worker-list`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkerResource {
    pub id: Option<String>,
    pub ownership_state: Option<String>,
    pub release_state: Option<String>,
    pub retained_reason: Option<String>,
    pub terminal_handle: Option<String>,
    pub worktree_id: Option<String>,
    pub origin_dispatch_id: Option<String>,
    pub owner_dispatch_id: Option<String>,
    pub release_requested_at: Option<Timestamp>,
    pub release_completed_at: Option<Timestamp>,
    pub release_error: Option<String>,
    pub archive: Option<Value>,
}

/// One row of `orca orchestration worker-list`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Worker {
    #[serde(default)]
    pub dispatch_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub worker_state: Option<WorkerState>,
    #[serde(default)]
    pub dispatch_status: Option<DispatchStatus>,
    #[serde(default)]
    pub agent_terminal_handle: Option<String>,
    #[serde(default)]
    pub terminal_state: Option<WorkerTerminalState>,
    #[serde(default)]
    pub resource: Option<WorkerResource>,
}

/// `result` of `orca orchestration worker-list`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct WorkerList {
    #[serde(deserialize_with = "null_default")]
    pub workers: Vec<Worker>,
    /// Totals per terminal state, e.g. `{"active": 9, "released": 100}`.
    #[serde(deserialize_with = "null_default")]
    pub counts: BTreeMap<String, u64>,
}

/// One row of `orca orchestration task-list`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub task_title: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub spec: Option<String>,
    pub status: TaskStatus,
    /// Task ids this one waits on (Orca sends a JSON-encoded array string).
    #[serde(default, deserialize_with = "json_string_list")]
    pub deps: Vec<String>,
    /// Raw JSON text of the result; see [`Task::parsed_result`].
    #[serde(default, deserialize_with = "raw_json_text")]
    pub result: Option<String>,
    #[serde(default)]
    pub assignee_handle: Option<String>,
    #[serde(default)]
    pub dispatch_id: Option<String>,
    #[serde(default)]
    pub created_by_terminal_handle: Option<String>,
    #[serde(default)]
    pub created_by_pane_key: Option<String>,
    #[serde(default)]
    pub created_at: Option<Timestamp>,
    #[serde(default)]
    pub completed_at: Option<Timestamp>,
}

impl Task {
    /// `result` decoded as JSON, when it is valid JSON.
    pub fn parsed_result(&self) -> Option<Value> {
        serde_json::from_str(self.result.as_deref()?).ok()
    }

    /// `display_name`, falling back to `task_title`, then the id.
    pub fn label(&self) -> &str {
        self.display_name
            .as_deref()
            .or(self.task_title.as_deref())
            .unwrap_or(&self.id)
    }
}

/// Structured part of a bus message payload.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MessagePayload {
    pub task_id: Option<String>,
    pub dispatch_id: Option<String>,
    /// Worker phase on heartbeats, e.g. `implementing`.
    pub phase: Option<String>,
    /// `succeeded`/`failed` on `worker_done`.
    pub outcome: Option<String>,
}

/// One message from `orca orchestration inbox`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct InboxMessage {
    pub id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub from_handle: Option<String>,
    /// A terminal handle, `run:<id>` or `dispatch:<id>`.
    #[serde(default)]
    pub to_handle: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(rename = "type", default)]
    pub message_type: Option<MessageType>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Raw JSON text of the payload; see [`InboxMessage::parsed_payload`].
    #[serde(default, deserialize_with = "raw_json_text")]
    pub payload: Option<String>,
    #[serde(default, deserialize_with = "boolish")]
    pub read: bool,
    #[serde(default)]
    pub sequence: Option<u64>,
    #[serde(default)]
    pub created_at: Option<Timestamp>,
    #[serde(default)]
    pub delivered_at: Option<Timestamp>,
    #[serde(default)]
    pub sender_pane_key: Option<String>,
}

impl InboxMessage {
    /// The payload's `{taskId, dispatchId, phase, outcome}`, when it is a JSON object.
    pub fn parsed_payload(&self) -> Option<MessagePayload> {
        serde_json::from_str(self.payload.as_deref()?).ok()
    }
}

/// A message for `orca orchestration send`. Only `subject` is required.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutgoingMessage {
    pub subject: String,
    /// `run:<id>`, `dispatch:<id>`, a group such as `@all`, or a terminal handle.
    pub to: Option<String>,
    pub run: Option<String>,
    pub from: Option<String>,
    pub body: Option<String>,
    pub message_type: Option<String>,
    pub priority: Option<String>,
    pub thread_id: Option<String>,
    /// Raw payload JSON; prefer the structured fields below.
    pub payload: Option<String>,
    pub task_id: Option<String>,
    pub dispatch_id: Option<String>,
    /// `succeeded` or `failed`; only valid with type `worker_done`.
    pub outcome: Option<String>,
    pub files_modified: Vec<String>,
    pub report_path: Option<String>,
    pub phase: Option<String>,
    pub retry_request: Option<String>,
}

/// `result` of `orca orchestration send`: one of `message` (sent directly),
/// `relay` (queued for a worker or Run home) or `messages` (group send).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct SendResult {
    pub message: Option<Value>,
    pub relay: Option<Value>,
    pub messages: Option<Vec<Value>>,
    pub recipients: Option<Value>,
    #[serde(deserialize_with = "null_default")]
    pub warnings: Vec<Value>,
}

impl SendResult {
    /// Id of the sent or queued message, when there is exactly one.
    pub fn message_id(&self) -> Option<&str> {
        self.message
            .as_ref()
            .and_then(|m| m.get("id"))
            .or_else(|| self.relay.as_ref().and_then(|r| r.get("messageId")))
            .and_then(Value::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_parse_all_orca_formats() {
        // 2026-09-23T08:37:04Z
        let base = 1_790_152_624_000;
        assert_eq!(parse_timestamp_ms("2026-09-23T08:37:04Z"), Some(base));
        assert_eq!(parse_timestamp_ms("2026-09-23 08:37:04"), Some(base));
        assert_eq!(
            parse_timestamp_ms("2026-09-23T08:37:04.697Z"),
            Some(base + 697)
        );
        assert_eq!(
            parse_timestamp_ms("2026-09-23T08:37:04.5Z"),
            Some(base + 500)
        );
        assert_eq!(parse_timestamp_ms("2026-09-23T10:37:04+02:00"), Some(base));
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_timestamp_ms("2000-02-29 12:00:00"),
            Some(951_825_600_000)
        );
        assert_eq!(parse_timestamp_ms("1789598758208"), Some(1_789_598_758_208));
        assert_eq!(parse_timestamp_ms("yesterday"), None);
        assert_eq!(parse_timestamp_ms("2026-13-01 00:00:00"), None);
        assert_eq!(parse_timestamp_ms("2026-09-23T08:37:04 junk"), None);
    }

    #[test]
    fn timestamp_deserializes_numbers_and_strings() {
        let t: Timestamp = serde_json::from_str("1789598758208").unwrap();
        assert_eq!(t.epoch_ms(), Some(1_789_598_758_208));
        let t: Timestamp = serde_json::from_str("\"1970-01-01 00:00:01\"").unwrap();
        assert_eq!(t.system_time(), Some(UNIX_EPOCH + Duration::from_secs(1)));
    }

    #[test]
    fn unknown_enum_values_are_kept() {
        let s: TaskStatus = serde_json::from_str("\"archived\"").unwrap();
        assert_eq!(s, TaskStatus::Other("archived".into()));
        assert_eq!(s.as_str(), "archived");
        let s: TaskStatus = serde_json::from_str("\"blocked\"").unwrap();
        assert_eq!(s, TaskStatus::Blocked);
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"blocked\"");
    }

    #[test]
    fn task_deps_accept_string_array_or_null() {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "t1", "status": "pending", "deps": "[\"t0\",\"t2\"]", "result": null
        }))
        .unwrap();
        assert_eq!(task.deps, vec!["t0", "t2"]);
        assert_eq!(task.result, None);
        assert_eq!(task.label(), "t1");

        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "t1", "status": "ready", "deps": ["t0"], "result": {"outcome": "ok"}
        }))
        .unwrap();
        assert_eq!(task.deps, vec!["t0"]);
        assert_eq!(task.parsed_result().unwrap()["outcome"], "ok");

        let task: Task = serde_json::from_value(
            serde_json::json!({"id": "t1", "status": "ready", "deps": null}),
        )
        .unwrap();
        assert!(task.deps.is_empty());
    }

    #[test]
    fn inbox_payload_handles_missing_and_invalid() {
        let msg: InboxMessage = serde_json::from_value(serde_json::json!({
            "id": "m", "payload": "not json", "read": 0
        }))
        .unwrap();
        assert_eq!(msg.parsed_payload(), None);
        assert!(!msg.read);
        assert_eq!(msg.message_type, None);
    }

    #[test]
    fn send_result_message_id() {
        let r: SendResult =
            serde_json::from_value(serde_json::json!({"message": {"id": "msg_1"}})).unwrap();
        assert_eq!(r.message_id(), Some("msg_1"));
        let r: SendResult = serde_json::from_value(
            serde_json::json!({"relay": {"messageId": "msg_2", "destination": "worker"}}),
        )
        .unwrap();
        assert_eq!(r.message_id(), Some("msg_2"));
    }

    #[test]
    fn branch_name_strips_refs_heads() {
        let t = Terminal {
            branch: Some("refs/heads/gylleus/x".into()),
            ..Default::default()
        };
        assert_eq!(t.branch_name(), Some("gylleus/x"));
    }
}

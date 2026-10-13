//! A Claude session transcript on disk (`~/.claude/projects/<dir>/<session>.jsonl`) as chat
//! items: the whole file first, then whatever is appended, so a pane shows the full
//! conversation rather than a screen or a tail of it.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::protocol::{ContentBlock, Incoming, parse_block, parse_value};
use super::transcript::Transcript;
use super::{auto_mode, voice};

/// Tool results and tool input strings are kept up to this many characters (a row shows less).
pub const MAX_KEPT_CHARS: usize = 8000;

/// Shown where the CLI compacted the conversation.
pub const COMPACTED_NOTICE: &str = "Conversation compacted";

/// One transcript record, reduced to what a pane draws.
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryRecord {
    /// Source time of all visible blocks from one JSONL record.
    Timed {
        timestamp: jiff::Timestamp,
        records: Vec<HistoryRecord>,
    },
    /// A prompt: typed by the user or sent by another agent.
    Prompt(String),
    Ant(Box<crate::queen_inbox::AntMessage>),
    /// A prompt Karl spoke to the King.
    VoicePrompt(String),
    /// An assistant message or tool results, as the CLI streams them.
    Message(Incoming),
    Notice(String),
    /// A slash command, background task note or shell escape.
    System(String),
}

/// The records of one transcript `line`; empty for records a pane does not show (meta,
/// attachments, sidechains, hook summaries, unparsable lines).
pub fn parse_line(line: &str) -> Vec<HistoryRecord> {
    match serde_json::from_str::<Value>(line) {
        Ok(record) => parse_record(&record),
        Err(_) => Vec::new(),
    }
}

pub fn parse_record(record: &Value) -> Vec<HistoryRecord> {
    parse_record_in(record, false)
}

fn parse_record_in(record: &Value, subagent: bool) -> Vec<HistoryRecord> {
    let records = record_items(record, subagent);
    match super::protocol::timestamp_of(record) {
        Some(timestamp) if !records.is_empty() => vec![HistoryRecord::Timed { timestamp, records }],
        _ => records,
    }
}

fn record_items(record: &Value, subagent: bool) -> Vec<HistoryRecord> {
    let flag = |key| record.get(key).and_then(Value::as_bool) == Some(true);
    if flag("isMeta") || (flag("isSidechain") && !subagent) || flag("isCompactSummary") {
        return Vec::new();
    }
    match record.get("type").and_then(Value::as_str) {
        Some("user") => user_records(record),
        Some("assistant") => match parse_value(record) {
            Incoming::Assistant {
                message_id,
                model,
                content,
                usage,
                ..
            } => vec![HistoryRecord::Message(Incoming::Assistant {
                parent: None,
                message_id,
                model,
                content: content.into_iter().map(shrink_block).collect(),
                usage,
            })],
            _ => Vec::new(),
        },
        Some("system")
            if record.get("subtype").and_then(Value::as_str) == Some("compact_boundary") =>
        {
            vec![HistoryRecord::Notice(COMPACTED_NOTICE.to_owned())]
        }
        _ => Vec::new(),
    }
}

fn user_records(record: &Value) -> Vec<HistoryRecord> {
    let content = record.pointer("/message/content");
    let mut out = Vec::new();
    let mut results = Vec::new();
    let mut text = Vec::new();
    match content {
        Some(Value::String(s)) => text.push(s.as_str()),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(Value::as_str) {
                            text.push(t);
                        }
                    }
                    Some("image") => text.push("[image]"),
                    Some("tool_result") => results.push(shrink_block(parse_block(block))),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if !results.is_empty() {
        out.push(HistoryRecord::Message(Incoming::User {
            parent: None,
            content: results,
        }));
    }
    let text = text.join("\n");
    if let Some(record) = prompt_record(&text) {
        out.push(record);
    }
    out
}

/// What a user text shows as: harness-injected text is dropped, command and task wrappers
/// become short system lines, Stronghold's notes to the King are taken off.
fn prompt_record(text: &str) -> Option<HistoryRecord> {
    let text = text.trim();
    if text.is_empty()
        || text.starts_with("<system-reminder>")
        || text.starts_with("<local-command-")
        || text.starts_with("<bash-stdout>")
        || text.starts_with("<bash-stderr>")
    {
        return None;
    }
    if text == "[Request interrupted by user]"
        || text == "[Request interrupted by user for tool use]"
    {
        return Some(HistoryRecord::Notice("Interrupted".to_owned()));
    }
    if let Some(name) = tag(text, "command-name") {
        let args = tag(text, "command-args").unwrap_or_default();
        let line = format!("{name} {args}").trim().to_owned();
        return Some(HistoryRecord::System(line));
    }
    if let Some(command) = tag(text, "bash-input") {
        return Some(HistoryRecord::System(format!("! {command}")));
    }
    if text.starts_with("<task-notification>") {
        let summary = tag(text, "summary").unwrap_or("Background task finished");
        return Some(HistoryRecord::System(summary.to_owned()));
    }
    let (voice, text) = strip_notes(text);
    if text.is_empty() {
        return None;
    }
    if let Some(message) = crate::queen_inbox::AntMessage::from_prompt(text) {
        return Some(HistoryRecord::Ant(Box::new(message)));
    }
    Some(if voice {
        HistoryRecord::VoicePrompt(text.to_owned())
    } else {
        HistoryRecord::Prompt(text.to_owned())
    })
}

/// `text` without the notes Stronghold puts before the King's messages; true when one said
/// the message was spoken.
fn strip_notes(mut text: &str) -> (bool, &str) {
    let mut voice = false;
    loop {
        let before = text;
        for (note, spoken) in [
            (voice::VOICE_NOTE, true),
            (voice::SPEAK_NOTE, false),
            (auto_mode::NOTE_ON, false),
            (auto_mode::NOTE_OFF, false),
        ] {
            if let Some(rest) = text.strip_prefix(note) {
                voice |= spoken;
                text = rest.trim_start();
            }
        }
        if text == before {
            return (voice, text);
        }
    }
}

/// The text between `<name>` and `</name>` in `text`, trimmed.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim())
}

/// `block` with long tool results and input strings cut to [`MAX_KEPT_CHARS`].
fn shrink_block(block: ContentBlock) -> ContentBlock {
    match block {
        ContentBlock::ToolResult {
            tool_use_id,
            text,
            is_error,
        } => ContentBlock::ToolResult {
            tool_use_id,
            text: cut(text),
            is_error,
        },
        ContentBlock::ToolUse {
            id,
            name,
            mut input,
        } => {
            shrink_value(&mut input);
            ContentBlock::ToolUse { id, name, input }
        }
        other => other,
    }
}

fn shrink_value(value: &mut Value) {
    match value {
        Value::String(s) if s.len() > MAX_KEPT_CHARS => *s = cut(std::mem::take(s)),
        Value::Array(items) => items.iter_mut().for_each(shrink_value),
        Value::Object(map) => map.values_mut().for_each(shrink_value),
        _ => {}
    }
}

fn cut(text: String) -> String {
    match text.char_indices().nth(MAX_KEPT_CHARS) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
}

impl Transcript {
    /// Adds one record read from a session transcript.
    pub fn apply_history(&mut self, record: &HistoryRecord) {
        let start = self.items.len();
        if let HistoryRecord::Timed { timestamp, records } = record {
            for record in records {
                self.apply_history(record);
            }
            self.set_timestamps(start, Some(*timestamp));
            return;
        }
        match record {
            HistoryRecord::Timed { .. } => unreachable!(),
            HistoryRecord::Ant(message) => self.push_ant(message),
            HistoryRecord::Prompt(text) => self.items.push(super::ChatItem::User(text.clone())),
            HistoryRecord::VoicePrompt(text) => {
                self.items.push(super::ChatItem::VoiceUser(text.clone()));
            }
            HistoryRecord::Message(message) => self.apply(message),
            HistoryRecord::Notice(text) if text == COMPACTED_NOTICE => self.compacted(),
            HistoryRecord::Notice(text) => self.push_notice(text.clone()),
            HistoryRecord::System(text) => self.push_system(text.clone()),
        }
        // Legacy history must never acquire the time at which it was loaded.
        self.set_timestamps(start, None);
    }
}

/// New records of a transcript file since the last read.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryChunk {
    pub path: PathBuf,
    /// The file was replaced or truncated: drop what was read before.
    pub reset: bool,
    /// Byte range of the file these records were read from (`start` is 0 after a reset).
    pub start: u64,
    pub end: u64,
    pub records: Vec<HistoryRecord>,
}

/// Reads a transcript from the start, then only what is appended. Only complete lines are
/// parsed; a line being written is kept for the next read.
#[derive(Debug)]
pub struct HistoryReader {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl HistoryReader {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            partial: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The records appended since the last read; `None` when nothing changed.
    pub fn read_new(&mut self) -> io::Result<Option<HistoryChunk>> {
        let mut file = File::open(&self.path)?;
        let len = file.metadata()?.len();
        let reset = len < self.offset;
        if reset {
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset && !reset {
            return Ok(None);
        }
        let start = self.offset;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = std::mem::take(&mut self.partial);
        let read = file.take(len - self.offset).read_to_end(&mut bytes)?;
        self.offset += read as u64;
        let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        self.partial = bytes.split_off(complete);
        // A dedicated subagent transcript is itself a sidechain. Only filter sidechains
        // out of the parent session, where they would duplicate another chat's traffic.
        let subagent = self
            .path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "subagents");
        let records = String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .flat_map(|record| parse_record_in(&record, subagent))
            .collect();
        Ok(Some(HistoryChunk {
            path: self.path.clone(),
            reset,
            start,
            end: self.offset,
            records,
        }))
    }
}

/// The whole transcript at `path` as a [`Transcript`].
pub fn load(path: &Path) -> io::Result<Transcript> {
    let mut transcript = Transcript::default();
    if let Some(chunk) = HistoryReader::new(path).read_new()? {
        for record in &chunk.records {
            transcript.apply_history(record);
        }
    }
    Ok(transcript)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;

    use super::*;
    use crate::chat::ChatItem;
    use crate::chat::transcript::ToolOutcome;

    fn jsonl(records: &[Value]) -> String {
        records.iter().map(|r| format!("{r}\n")).collect()
    }

    fn prompt(text: &str) -> Value {
        json!({"type": "user", "message": {"role": "user", "content": text}})
    }

    fn reply(id: &str, text: &str) -> Value {
        json!({"type": "assistant", "message": {"id": id, "content": [{"type": "text", "text": text}]}})
    }

    fn transcript_of(text: &str) -> Transcript {
        let mut t = Transcript::default();
        for record in text.lines().flat_map(parse_line) {
            t.apply_history(&record);
        }
        t
    }

    #[test]
    fn source_times_and_actual_models_survive_history_loading() {
        let at = "2026-10-08T11:52:04Z";
        let later = "2026-10-08T11:53:04Z";
        let t = transcript_of(&jsonl(&[
            json!({"type": "user", "timestamp": at, "message": {"content": "Hello"}}),
            json!({"type": "assistant", "timestamp": at, "message": {
            "id": "m1", "model": "claude-opus-4-6", "content": [
                {"type": "text", "text": "Checking"},
                {"type": "tool_use", "id": "t1", "name": "Read", "input": {}}
            ]}}),
            json!({"type": "user", "timestamp": later, "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "done"}
            ]}}),
            json!({"type": "assistant", "timestamp": later, "message": {
                "id": "m2", "model": "<synthetic>", "content": [{"type": "text", "text": "Interrupted"}]
            }}),
            json!({"type": "assistant", "isSidechain": true, "message": {
                "id": "child", "model": "other-model", "content": []
            }}),
            prompt("Legacy prompt"),
            json!({"type": "system", "subtype": "compact_boundary", "timestamp": "bad"}),
            prompt("<command-name>/model</command-name><command-args>sonnet</command-args>"),
        ]));
        assert_eq!(t.model.as_deref(), Some("claude-opus-4-6"));
        for index in 0..3 {
            assert_eq!(t.timestamp(index), Some(at.parse().unwrap()));
        }
        assert_eq!(t.timestamp(3), Some(later.parse().unwrap()));
        for index in 4..7 {
            assert_eq!(
                t.timestamp(index),
                None,
                "legacy records must not get load time"
            );
        }
        assert!(t.tool_calls().next().unwrap().result.is_some());
        assert!(
            transcript_of(&jsonl(&[reply("m", "Unknown model")]))
                .model
                .is_none()
        );
    }

    #[test]
    fn dedicated_subagent_history_keeps_its_sidechain_times_and_model() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("subagents");
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("agent-a.jsonl");
        let at = "2026-10-08T11:52:04Z";
        std::fs::write(
            &path,
            jsonl(&[
                json!({"type": "user", "isSidechain": true, "timestamp": at,
                "message": {"content": "Explore"}}),
                json!({"type": "assistant", "isSidechain": true, "timestamp": at,
                "message": {"id": "m", "model": "claude-haiku-4-5", "content": [
                    {"type": "text", "text": "Found it"}
                ]}}),
            ]),
        )
        .unwrap();
        let t = load(&path).unwrap();
        assert_eq!(t.items.len(), 2);
        assert_eq!(t.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(t.timestamp(0), Some(at.parse().unwrap()));
        assert_eq!(t.timestamp(1), Some(at.parse().unwrap()));
    }

    #[test]
    fn prepending_history_and_finishing_streams_preserves_message_times() {
        let mut live = Transcript::default();
        live.push_user("now");
        let sent = live.timestamp(0).unwrap();
        let at: jiff::Timestamp = "2026-10-09T11:52:04Z".parse().unwrap();
        let messages = [
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "m", "model": "running-model"}}}),
            json!({"type": "stream_event", "event": {"type": "content_block_start", "index": 0, "content_block": {"type": "text"}}}),
        ];
        for message in messages {
            live.apply_at(&parse_value(&message), at);
        }
        let old = "2026-10-08T11:52:04Z";
        live.prepend(transcript_of(&jsonl(&[
            json!({"type": "user", "timestamp": old, "message": {"content": "Earlier"}}),
            reply("old", "Earlier reply without time"),
        ])));
        live.apply_at(
            &parse_value(&reply("m", "Finished")),
            "2026-10-09T11:54:00Z".parse().unwrap(),
        );
        assert_eq!(live.items.len(), 4);
        assert_eq!(live.timestamp(0), Some(old.parse().unwrap()));
        assert_eq!(live.timestamp(1), None);
        assert_eq!(live.timestamp(2), Some(sent));
        assert_eq!(live.timestamp(3), Some(at));
        assert_eq!(
            live.assistant_text(),
            ["Earlier reply without time", "Finished"]
        );
        assert_eq!(live.model.as_deref(), Some("running-model"));
    }

    #[test]
    fn records_become_prompts_replies_tools_and_notes() {
        let text = jsonl(&[
            prompt("Review the diff"),
            json!({"type": "user", "isMeta": true, "message": {"content": "meta"}}),
            prompt("<system-reminder>\nhi\n</system-reminder>"),
            json!({"type": "attachment", "attachment": {"type": "hook_success"}}),
            json!({"type": "pr-link", "url": "x"}),
            json!({"type": "assistant", "message": {"id": "m1", "content": [
                {"type": "thinking", "thinking": "plan", "signature": "s"},
                {"type": "text", "text": "Let me look."},
            ]}}),
            json!({"type": "assistant", "message": {"id": "m1", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "git diff"}},
            ]}}),
            json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "1 file changed"},
            ]}}),
            json!({"type": "assistant", "isSidechain": true, "message": {"id": "s1", "content": [
                {"type": "text", "text": "subagent chatter"},
            ]}}),
            prompt("<command-name>/model</command-name>\n<command-args>opus</command-args>"),
            prompt("<local-command-stdout>Set model</local-command-stdout>"),
            prompt("<bash-input>ls</bash-input>"),
            prompt(
                "<task-notification>\n<summary>Agent \"Map\" finished</summary>\n</task-notification>",
            ),
            json!({"type": "system", "subtype": "compact_boundary"}),
            json!({"type": "user", "isCompactSummary": true, "message": {"content": "long summary"}}),
            json!({"type": "user", "message": {"content": [{"type": "text", "text": "[Request interrupted by user]"}]}}),
            reply("m2", "Done."),
        ]);
        let text = format!("{text}not json\n{}", jsonl(&[prompt("after garbage")]));
        let t = transcript_of(&text);
        assert_eq!(
            t.items,
            vec![
                ChatItem::User("Review the diff".into()),
                ChatItem::Assistant {
                    text: "Let me look.".into(),
                    streaming: false
                },
                ChatItem::Tool(crate::chat::transcript::ToolCall {
                    id: "t1".into(),
                    name: "Bash".into(),
                    input: Some(json!({"command": "git diff"})),
                    result: Some(ToolOutcome {
                        text: "1 file changed".into(),
                        is_error: false
                    }),
                    auto: false,
                }),
                ChatItem::System("/model opus".into()),
                ChatItem::System("! ls".into()),
                ChatItem::System("Agent \"Map\" finished".into()),
                ChatItem::Notice(COMPACTED_NOTICE.into()),
                ChatItem::Notice("Interrupted".into()),
                ChatItem::Assistant {
                    text: "Done.".into(),
                    streaming: false
                },
                ChatItem::User("after garbage".into()),
            ]
        );
        assert!(!t.running);
    }

    #[test]
    fn notes_to_the_king_are_taken_off() {
        let voiced = format!(
            "{}\n\n{}\n\nHow many agents?",
            voice::VOICE_NOTE,
            auto_mode::NOTE_ON
        );
        let spoken = format!("{}\n\nStatus?", voice::SPEAK_NOTE);
        let t = transcript_of(&jsonl(&[prompt(&voiced), prompt(&spoken)]));
        assert_eq!(
            t.items,
            vec![
                ChatItem::VoiceUser("How many agents?".into()),
                ChatItem::User("Status?".into()),
            ]
        );
    }

    #[test]
    fn long_tool_output_is_cut_but_kept() {
        let long = "x".repeat(MAX_KEPT_CHARS * 3);
        let t = transcript_of(&jsonl(&[
            json!({"type": "assistant", "message": {"id": "m", "content": [
                {"type": "tool_use", "id": "t", "name": "Write", "input": {"content": long}},
            ]}}),
            json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t", "content": long},
            ]}}),
        ]));
        let call = t.tool_calls().next().unwrap();
        let input = call.input.as_ref().unwrap()["content"].as_str().unwrap();
        assert_eq!(input.chars().count(), MAX_KEPT_CHARS + 1);
        let result = &call.result.as_ref().unwrap().text;
        assert_eq!(result.chars().count(), MAX_KEPT_CHARS + 1);
        assert!(result.ends_with('…'));
    }

    /// A transcript far bigger than any tail window loads whole: the first message is there.
    #[test]
    fn a_long_transcript_loads_without_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut f = std::io::BufWriter::new(File::create(&path).unwrap());
        let turns = 6_000;
        for i in 0..turns {
            writeln!(f, "{}", prompt(&format!("question {i}"))).unwrap();
            writeln!(
                f,
                "{}",
                reply(&format!("m{i}"), &format!("answer {i} {}", "p".repeat(400)))
            )
            .unwrap();
            // Bulky records a pane skips, as in real transcripts.
            writeln!(
                f,
                "{}",
                json!({"type": "attachment", "blob": "b".repeat(500)})
            )
            .unwrap();
        }
        drop(f);
        assert!(std::fs::metadata(&path).unwrap().len() > 5 * 1024 * 1024);
        let t = load(&path).unwrap();
        assert_eq!(t.items.len(), turns * 2);
        assert_eq!(t.items[0], ChatItem::User("question 0".into()));
        assert_eq!(
            t.items.last(),
            Some(&ChatItem::Assistant {
                text: format!("answer {} {}", turns - 1, "p".repeat(400)),
                streaming: false
            })
        );
    }

    #[test]
    fn reader_reads_appends_keeps_partial_lines_and_resets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, jsonl(&[prompt("one"), reply("m1", "two")])).unwrap();
        let mut reader = HistoryReader::new(&path);
        let first = reader.read_new().unwrap().unwrap();
        assert_eq!(first.records.len(), 2);
        assert_eq!((first.reset, first.start), (false, 0));
        assert!(reader.read_new().unwrap().is_none(), "nothing new");

        // Half a record is kept until its line is complete.
        let line = prompt("three").to_string();
        let (head, tail) = line.split_at(10);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(f, "{head}").unwrap();
        let partial = reader.read_new().unwrap().unwrap();
        assert!(partial.records.is_empty());
        writeln!(f, "{tail}").unwrap();
        let next = reader.read_new().unwrap().unwrap();
        assert_eq!(next.records, vec![HistoryRecord::Prompt("three".into())]);
        assert_eq!(next.start, partial.end);

        // Rewritten shorter: read again from the start.
        std::fs::write(&path, jsonl(&[prompt("new")])).unwrap();
        let reset = reader.read_new().unwrap().unwrap();
        assert!(reset.reset);
        assert_eq!(reset.start, 0);
        assert_eq!(reset.records, vec![HistoryRecord::Prompt("new".into())]);
        assert!(
            HistoryReader::new(dir.path().join("missing"))
                .read_new()
                .is_err()
        );
    }

    /// History read after the session started goes in front, and later tool results still
    /// reach their rows.
    #[test]
    fn prepend_puts_earlier_messages_first() {
        let mut live = Transcript::default();
        live.push_user("now");
        live.apply(&crate::chat::protocol::parse_line(
            r#"{"type":"assistant","message":{"id":"m9","content":[{"type":"tool_use","id":"t9","name":"Read","input":{}}]},"parent_tool_use_id":null}"#,
        ).unwrap());
        let earlier = transcript_of(&jsonl(&[prompt("before"), reply("m1", "earlier reply")]));
        live.prepend(earlier);
        live.apply(&crate::chat::protocol::parse_line(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t9","content":"ok"}]},"parent_tool_use_id":null}"#,
        ).unwrap());
        assert_eq!(live.items[0], ChatItem::User("before".into()));
        assert_eq!(live.items[2], ChatItem::User("now".into()));
        let ChatItem::Tool(call) = &live.items[3] else {
            panic!("{:?}", live.items)
        };
        assert_eq!(call.result.as_ref().unwrap().text, "ok");
    }
}

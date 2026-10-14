//! How each Claude session's last turn ended, read from its main transcript: whether the agent
//! ended it asking a question (and so waits on the user) and when the user last prompted it.
//!
//! A turn ends with an assistant record whose `stop_reason` is `end_turn`. If its text closes
//! on a question, the session has an [`OpenQuestion`] until a user record that is not a tool
//! result arrives (the user's answer, or anything else that starts a new turn). Sidechain and meta
//! records are skipped. Files are tailed from a remembered offset like [`super::usage`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use serde_json::Value;

use super::sessions::SessionInfo;
use super::subagents::{Tail, tail_file};
use crate::orca::parse_timestamp_ms;

/// Non-blank lines at the end of a turn's text searched for the question.
const QUESTION_TAIL_LINES: usize = 4;
/// Longest question kept.
pub const MAX_QUESTION_CHARS: usize = 300;

/// A question the agent ended its turn on, still unanswered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenQuestion {
    pub text: String,
    /// When the turn ended.
    pub at_ms: i64,
}

/// What a session's transcript says about its last turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Turn {
    /// The last user prompt (not a tool result).
    pub last_prompt_ms: Option<i64>,
    pub question: Option<OpenQuestion>,
}

impl Turn {
    /// Folds one transcript line in.
    fn feed(&mut self, line: &str) {
        // Cheap filter: most lines are attachments, tool results and progress.
        if !(line.contains("\"user\"") || line.contains("end_turn") || line.contains("tool_use")) {
            return;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if record.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || record.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            return;
        }
        let at_ms = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp_ms);
        let content = record.pointer("/message/content");
        match record.get("type").and_then(Value::as_str) {
            Some("user") if content.is_some_and(is_prompt) => {
                self.last_prompt_ms = at_ms.or(self.last_prompt_ms);
                self.question = None;
            }
            Some("assistant") => {
                let stop = record
                    .pointer("/message/stop_reason")
                    .and_then(Value::as_str);
                self.question = match (stop, at_ms) {
                    (Some("end_turn"), Some(at_ms)) => content
                        .map(assistant_text)
                        .and_then(|t| question_in(&t))
                        .map(|text| OpenQuestion { text, at_ms }),
                    _ => None,
                };
            }
            _ => {}
        }
    }
}

/// A user record's content that is a prompt rather than only tool results.
fn is_prompt(content: &Value) -> bool {
    match content {
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(blocks) => blocks
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) != Some("tool_result")),
        _ => false,
    }
}

fn assistant_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The question a turn's text ends on: the last of its final few non-blank lines (outside code
/// blocks) that ends with `?`, ignoring trailing markdown.
pub fn question_in(text: &str) -> Option<String> {
    let mut in_code = false;
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| {
            if l.trim_start().starts_with("```") {
                in_code = !in_code;
                return false;
            }
            !in_code && !l.trim().is_empty()
        })
        .collect();
    let line = lines.iter().rev().take(QUESTION_TAIL_LINES).find(|l| {
        l.trim_end()
            .trim_end_matches(['*', '_', '`', ')', '"', '\'', '”'])
            .ends_with('?')
    })?;
    let line = line
        .trim()
        .trim_start_matches(['#', '>', '-', '*', '_', ' '])
        .trim_end_matches(['*', '_'])
        .trim();
    Some(clip(line, MAX_QUESTION_CHARS))
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[derive(Debug, Default)]
struct SessionTurn {
    tail: Tail,
    path: PathBuf,
    turn: Turn,
}

/// Incremental per-session turn reader.
#[derive(Debug, Default)]
pub struct TurnTracker {
    sessions: HashMap<String, SessionTurn>,
}

impl TurnTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads lines appended since the last refresh and returns each session's turn by session
    /// id. Sessions not in `sessions` are forgotten.
    pub fn refresh(&mut self, sessions: &[SessionInfo]) -> BTreeMap<String, Turn> {
        let keep: HashSet<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
        self.sessions.retain(|id, _| keep.contains(id.as_str()));
        sessions
            .iter()
            .map(|session| {
                let cache = self.sessions.entry(session.session_id.clone()).or_default();
                if cache.path != session.transcript_path {
                    *cache = SessionTurn {
                        path: session.transcript_path.clone(),
                        ..SessionTurn::default()
                    };
                }
                let turn = &mut cache.turn;
                tail_file(&session.transcript_path, &mut cache.tail, |line| {
                    turn.feed(line);
                });
                (session.session_id.clone(), cache.turn.clone())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn user(text: &str, at: &str) -> String {
        json!({"type": "user", "timestamp": at, "message": {"role": "user", "content": text}})
            .to_string()
    }

    fn tool_result(at: &str) -> String {
        json!({"type": "user", "timestamp": at, "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
        ]}})
        .to_string()
    }

    fn assistant(text: &str, stop: &str, at: &str) -> String {
        json!({"type": "assistant", "timestamp": at, "message": {
            "role": "assistant", "stop_reason": stop,
            "content": [{"type": "text", "text": text}]
        }})
        .to_string()
    }

    fn turn(lines: &[String]) -> Turn {
        let mut turn = Turn::default();
        for line in lines {
            turn.feed(line);
        }
        turn
    }

    const T1: &str = "2026-10-02T10:00:00.000Z";
    const T2: &str = "2026-10-02T10:01:00.000Z";
    const T3: &str = "2026-10-02T10:02:00.000Z";

    #[test]
    fn a_turn_ending_on_a_question_is_open_until_the_next_prompt() {
        let asked = turn(&[
            user("fix the bug", T1),
            assistant(
                "Fixed it.\n\nShould I also bump the version?",
                "end_turn",
                T2,
            ),
        ]);
        let q = asked.question.expect("open question");
        assert_eq!(q.text, "Should I also bump the version?");
        assert_eq!(q.at_ms, parse_timestamp_ms(T2).unwrap());
        assert_eq!(asked.last_prompt_ms, parse_timestamp_ms(T1));

        let answered = turn(&[
            user("fix the bug", T1),
            assistant("Should I also bump the version?", "end_turn", T2),
            user("yes", T3),
        ]);
        assert_eq!(answered.question, None);
        assert_eq!(answered.last_prompt_ms, parse_timestamp_ms(T3));
    }

    #[test]
    fn statements_tool_turns_and_tool_results_are_not_questions() {
        assert_eq!(
            turn(&[assistant("Done, merged into main.", "end_turn", T2)]).question,
            None
        );
        // Mid-turn text that asks something rhetorically, then a tool call.
        assert_eq!(
            turn(&[assistant("What does the parser do?", "tool_use", T2)]).question,
            None
        );
        // A tool result is not Karl answering.
        let t = turn(&[
            assistant("Which branch should I use?", "end_turn", T2),
            tool_result(T3),
        ]);
        assert!(t.question.is_some());
    }

    #[test]
    fn sidechain_records_are_ignored() {
        let mut side: Value = serde_json::from_str(&assistant("Shall I?", "end_turn", T2)).unwrap();
        side["isSidechain"] = json!(true);
        assert_eq!(turn(&[side.to_string()]).question, None);
    }

    #[test]
    fn the_question_may_be_followed_by_a_few_lines() {
        let text = "Two ways to fix it:\n\n**Which do you prefer?**\n\n1. Retry\n2. Fail fast";
        assert_eq!(question_in(text).as_deref(), Some("Which do you prefer?"));
        let long = "Is this a question?\na\nb\nc\nd";
        assert_eq!(question_in(long), None);
        let code = "Done:\n```\nwhy?\n```";
        assert_eq!(question_in(code), None);
        assert_eq!(
            question_in("- Want me to open a PR?").as_deref(),
            Some("Want me to open a PR?")
        );
    }

    #[test]
    fn the_tracker_tails_the_transcript() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("s1.jsonl");
        let session = SessionInfo {
            session_id: "s1".into(),
            transcript_path: path.clone(),
            dir: None,
            modified_ms: 0,
            cwd_from_transcript: None,
        };
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                user("go", T1),
                assistant("Merge it now?", "end_turn", T2)
            ),
        )
        .unwrap();
        let mut tracker = TurnTracker::new();
        let turns = tracker.refresh(std::slice::from_ref(&session));
        assert!(turns["s1"].question.is_some());

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut f, format!("{}\n", user("yes", T3)).as_bytes()).unwrap();
        let turns = tracker.refresh(&[session]);
        assert_eq!(turns["s1"].question, None);
        assert_eq!(turns["s1"].last_prompt_ms, parse_timestamp_ms(T3));
    }
}

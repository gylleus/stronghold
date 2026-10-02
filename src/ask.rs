//! Agents waiting on Karl's answer to a plain question, which no prompt on their screen shows.
//!
//! Two sources, so a question reaches Karl even when the agent forgets the tool:
//! - the `ask_karl` MCP tool records a [`PendingAsk`] (and notifies Karl straight away);
//! - a Claude session whose last turn ended on a question ([`crate::claude::turns`]).
//!
//! Either way the node shows NeedsInput (see [`crate::model::FleetSnapshot::mark_waiting`]),
//! which the dashboard and the needs-input notification pick up. An ask is answered once the
//! transcript shows a prompt after it; for agents without a Claude transcript (Codex), once
//! the terminal has produced output [`ASK_SETTLE_MS`] after it.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::claude::ClaudeData;
use crate::control::ControlError;
use crate::control::protocol::Params;
use crate::finish::{clip, origin_of};
use crate::fleet::FleetData;
use crate::notify::Severity;
use crate::review::mark::Origin;

/// Output this long after an ask, from an agent whose transcript isn't read, counts as the
/// conversation having moved on.
pub const ASK_SETTLE_MS: i64 = 2 * 60_000;
/// Longest question kept.
pub const MAX_QUESTION_CHARS: usize = 500;
/// Longest context kept.
pub const MAX_CONTEXT_CHARS: usize = 1_000;

/// An `ask_karl` call, carried to the app as the control API's `ask` params.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRequest {
    pub question: String,
    pub context: String,
    pub severity: Severity,
    pub url: Option<String>,
    pub origin: Origin,
}

impl AskRequest {
    pub fn to_params(&self) -> Value {
        json!({
            "question": self.question,
            "context": self.context,
            "severity": self.severity.name(),
            "url": self.url,
            "origin": self.origin,
        })
    }

    pub fn from_params(params: &Value) -> Result<Self, ControlError> {
        let p = Params::new(params);
        let question = clip(p.required_str("question")?, MAX_QUESTION_CHARS);
        if question.is_empty() {
            return Err(ControlError::invalid_params("\"question\" is empty"));
        }
        let severity = match p.str("severity")?.map(str::trim).filter(|s| !s.is_empty()) {
            None => Severity::Warn,
            Some(s) => Severity::parse(s)
                .filter(|s| *s != Severity::Info)
                .ok_or_else(|| {
                    ControlError::invalid_params(format!(
                        "severity {s:?} doesn't fit a question Karl must answer; use warn or urgent"
                    ))
                })?,
        };
        let url = p.str("url")?.map(str::trim).filter(|u| !u.is_empty());
        if let Some(u) = url
            && !(u.starts_with("https://") || u.starts_with("http://"))
        {
            return Err(ControlError::invalid_params(format!(
                "\"url\" {u:?} must be an http(s) link"
            )));
        }
        Ok(Self {
            question,
            context: clip(p.str("context")?.unwrap_or_default(), MAX_CONTEXT_CHARS),
            severity,
            url: url.map(str::to_owned),
            origin: origin_of(params)?,
        })
    }
}

/// A question an agent asked Karl with `ask_karl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAsk {
    pub question: String,
    pub at_ms: i64,
}

/// Open asks by terminal handle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Asks {
    by_handle: BTreeMap<String, PendingAsk>,
}

impl Asks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `ask` for `handle`, replacing an older one.
    pub fn insert(&mut self, handle: &str, ask: PendingAsk) {
        self.by_handle.insert(handle.to_owned(), ask);
    }

    /// Ends the ask of `handle` (its session finished).
    pub fn remove(&mut self, handle: &str) {
        self.by_handle.remove(handle);
    }

    pub fn get(&self, handle: &str) -> Option<&PendingAsk> {
        self.by_handle.get(handle)
    }

    pub fn is_empty(&self) -> bool {
        self.by_handle.is_empty()
    }

    /// Drops answered asks and those whose terminal is gone; `true` when any was dropped.
    pub fn prune(&mut self, data: &FleetData, claude: Option<&ClaudeData>) -> bool {
        let before = self.by_handle.len();
        self.by_handle
            .retain(|handle, ask| !answered(handle, ask, data, claude));
        self.by_handle.len() != before
    }

    /// Every terminal waiting on Karl, with its question: the open asks, then the Claude
    /// sessions whose last turn ended on a question.
    pub fn waiting(
        &self,
        data: &FleetData,
        claude: Option<&ClaudeData>,
    ) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = self
            .by_handle
            .iter()
            .map(|(h, a)| (h.clone(), a.question.clone()))
            .collect();
        if let Some(claude) = claude {
            for terminal in &data.terminals {
                if let Some(q) = claude
                    .turn(&terminal.handle)
                    .and_then(|t| t.question.as_ref())
                {
                    out.entry(terminal.handle.clone())
                        .or_insert_with(|| q.text.clone());
                }
            }
        }
        out
    }
}

/// Whether `ask` no longer stands: its terminal left, Karl answered in the transcript, or (with
/// no transcript to read) the terminal went on well after it.
fn answered(handle: &str, ask: &PendingAsk, data: &FleetData, claude: Option<&ClaudeData>) -> bool {
    let Some(terminal) = data.terminal(handle) else {
        return true;
    };
    if let Some(turn) = claude.and_then(|c| c.turn(handle)) {
        return turn.last_prompt_ms.is_some_and(|p| p > ask.at_ms);
    }
    terminal
        .last_output_at
        .as_ref()
        .and_then(|t| t.epoch_ms())
        .is_some_and(|out| out > ask.at_ms + ASK_SETTLE_MS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::{OpenQuestion, SessionInfo, SessionMatch, Turn};
    use crate::orca::{Terminal, Timestamp};

    const AT: i64 = 1_790_000_000_000;

    fn data(handles: &[(&str, Option<i64>)]) -> FleetData {
        FleetData {
            terminals: handles
                .iter()
                .map(|(h, out)| Terminal {
                    handle: (*h).to_owned(),
                    last_output_at: out.map(Timestamp::Millis),
                    ..Terminal::default()
                })
                .collect(),
            ..FleetData::default()
        }
    }

    fn claude(handle: &str, turn: Turn) -> ClaudeData {
        let session = SessionInfo {
            session_id: "s1".into(),
            transcript_path: "/p/s1.jsonl".into(),
            dir: None,
            modified_ms: 0,
            cwd_from_transcript: None,
        };
        ClaudeData {
            session_by_handle: [(handle.to_owned(), SessionMatch::Unique(session))]
                .into_iter()
                .collect(),
            turns: [("s1".to_owned(), turn)].into_iter().collect(),
            ..ClaudeData::default()
        }
    }

    fn ask() -> PendingAsk {
        PendingAsk {
            question: "Which DB?".into(),
            at_ms: AT,
        }
    }

    #[test]
    fn an_ask_stands_until_karl_prompts_the_session() {
        let mut asks = Asks::new();
        asks.insert("t1", ask());
        let data = data(&[("t1", Some(AT + 10 * ASK_SETTLE_MS))]);
        // The agent's own output after the ask doesn't answer it; only a prompt does.
        let before = claude(
            "t1",
            Turn {
                last_prompt_ms: Some(AT - 1),
                question: None,
            },
        );
        assert!(!asks.prune(&data, Some(&before)));
        assert_eq!(asks.waiting(&data, Some(&before))["t1"], "Which DB?");
        let after = claude(
            "t1",
            Turn {
                last_prompt_ms: Some(AT + 1),
                question: None,
            },
        );
        assert!(asks.prune(&data, Some(&after)));
        assert!(asks.waiting(&data, Some(&after)).is_empty());
    }

    #[test]
    fn without_a_transcript_later_output_answers_it() {
        let mut asks = Asks::new();
        asks.insert("t1", ask());
        assert!(!asks.prune(&data(&[("t1", Some(AT + 1_000))]), None));
        assert!(asks.prune(&data(&[("t1", Some(AT + ASK_SETTLE_MS + 1))]), None));
    }

    #[test]
    fn an_ask_ends_with_its_terminal() {
        let mut asks = Asks::new();
        asks.insert("t1", ask());
        assert!(asks.prune(&data(&[]), None));
    }

    #[test]
    fn a_turn_ending_on_a_question_is_waiting_too() {
        let claude = claude(
            "t1",
            Turn {
                last_prompt_ms: None,
                question: Some(OpenQuestion {
                    text: "Merge now?".into(),
                    at_ms: AT,
                }),
            },
        );
        let waiting = Asks::new().waiting(&data(&[("t1", None)]), Some(&claude));
        assert_eq!(waiting["t1"], "Merge now?");
        // An explicit ask's wording wins.
        let mut asks = Asks::new();
        asks.insert("t1", ask());
        assert_eq!(
            asks.waiting(&data(&[("t1", None)]), Some(&claude))["t1"],
            "Which DB?"
        );
    }
}

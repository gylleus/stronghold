//! The Envoy's memory of each Slack thread, kept by the app so a fresh context picks up where
//! the last one left off: who asked, what was promised, what we're waiting on.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::channel::InboundEvent;

pub const NOTES_FILE: &str = "envoy-notes.json";
/// Notes kept; the least recently updated go first.
pub const MAX_NOTES: usize = 500;
/// Characters a note may hold: a few lines, not a transcript.
pub const MAX_NOTE_CHARS: usize = 600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadNote {
    pub conversation: String,
    /// The thread root; `None` for a note on the whole conversation (a DM, say).
    pub thread_ts: Option<String>,
    pub text: String,
    pub updated_ms: i64,
}

impl ThreadNote {
    pub fn key(&self) -> String {
        key(&self.conversation, self.thread_ts.as_deref())
    }
}

/// `C123` or `C123:1700000000.000100`.
pub fn key(conversation: &str, thread_ts: Option<&str>) -> String {
    match thread_ts {
        Some(ts) => format!("{conversation}:{ts}"),
        None => conversation.to_owned(),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThreadNotes {
    notes: BTreeMap<String, ThreadNote>,
}

impl ThreadNotes {
    /// Replaces the note for the thread (or conversation); empty text removes it. Returns
    /// the stored note, or `None` when it was removed.
    pub fn set(
        &mut self,
        conversation: &str,
        thread_ts: Option<&str>,
        text: &str,
        now: i64,
    ) -> Result<Option<ThreadNote>, String> {
        super::slack::validate_conversation(conversation)?;
        if let Some(ts) = thread_ts {
            super::slack::validate_ts(ts)?;
        }
        let text = clean(text);
        if text.chars().count() > MAX_NOTE_CHARS {
            return Err(format!("A note holds at most {MAX_NOTE_CHARS} characters"));
        }
        let key = key(conversation, thread_ts);
        if text.is_empty() {
            self.notes.remove(&key);
            return Ok(None);
        }
        let note = ThreadNote {
            conversation: conversation.into(),
            thread_ts: thread_ts.map(str::to_owned),
            text,
            updated_ms: now,
        };
        self.notes.insert(key, note.clone());
        while self.notes.len() > MAX_NOTES {
            let oldest = self
                .notes
                .iter()
                .min_by_key(|(k, n)| (n.updated_ms, (*k).clone()))
                .map(|(k, _)| k.clone())
                .expect("nonempty");
            self.notes.remove(&oldest);
        }
        Ok(Some(note))
    }

    pub fn get(&self, conversation: &str, thread_ts: Option<&str>) -> Option<&ThreadNote> {
        self.notes.get(&key(conversation, thread_ts))
    }

    /// The note for an event's thread, else for its conversation.
    pub fn for_event(&self, event: &InboundEvent) -> Option<&ThreadNote> {
        self.for_thread(
            &event.conversation,
            event.thread_ts.as_deref().unwrap_or(&event.ts),
        )
    }

    /// The note for a thread, else for its conversation.
    pub fn for_thread(&self, conversation: &str, thread_ts: &str) -> Option<&ThreadNote> {
        self.get(conversation, Some(thread_ts))
            .or_else(|| self.get(conversation, None))
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        super::inbox::load(path)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        super::inbox::save(path, self)
    }
}

/// One line, trimmed: notes are typed into a terminal line beside their event.
fn clean(text: &str) -> String {
    text.split(char::is_control)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envoy::channel::EventKind;

    fn event(conversation: &str, ts: &str, thread_ts: Option<&str>) -> InboundEvent {
        InboundEvent {
            channel_kind: "slack".into(),
            conversation: conversation.into(),
            ts: ts.into(),
            thread_ts: thread_ts.map(str::to_owned),
            user: "U1".into(),
            user_name: "Avery".into(),
            text: "hi".into(),
            kind: EventKind::AgentComms,
            permalink: None,
        }
    }

    #[test]
    fn thread_notes_outrank_conversation_notes_and_roots_find_their_own_thread() {
        let mut notes = ThreadNotes::default();
        notes
            .set("D1", None, "Sam: wants the rollout date", 1)
            .unwrap();
        notes
            .set(
                "C1",
                Some("10.000001"),
                "  Asked for\napi#694 review;\twaiting on Alex ",
                2,
            )
            .unwrap();
        assert_eq!(
            notes
                .for_event(&event("C1", "10.000001", None))
                .unwrap()
                .text,
            "Asked for api#694 review; waiting on Alex",
            "a root message finds its thread's note, flattened to one line"
        );
        assert_eq!(
            notes
                .for_event(&event("C1", "11.000001", Some("10.000001")))
                .unwrap()
                .text,
            "Asked for api#694 review; waiting on Alex"
        );
        assert!(notes.for_event(&event("C1", "12.000001", None)).is_none());
        assert_eq!(
            notes
                .for_event(&event("D1", "13.000001", Some("12.000001")))
                .unwrap()
                .text,
            "Sam: wants the rollout date",
            "a thread without its own note falls back to the conversation note"
        );
        assert_eq!(notes.set("C1", Some("10.000001"), "  ", 3).unwrap(), None);
        assert!(notes.get("C1", Some("10.000001")).is_none());
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn invalid_ids_and_long_notes_are_refused() {
        let mut notes = ThreadNotes::default();
        assert!(notes.set("bogus", None, "x", 1).is_err());
        assert!(notes.set("C1", Some("not-a-ts"), "x", 1).is_err());
        assert!(
            notes
                .set("C1", None, &"x".repeat(MAX_NOTE_CHARS + 1), 1)
                .is_err()
        );
        assert!(
            notes
                .set("C1", None, &"x".repeat(MAX_NOTE_CHARS), 1)
                .is_ok()
        );
    }

    #[test]
    fn the_oldest_notes_go_first_and_the_file_round_trips() {
        let mut notes = ThreadNotes::default();
        for n in 0..MAX_NOTES + 1 {
            notes
                .set("C1", Some(&format!("{}.000001", n + 1)), "note", n as i64)
                .unwrap();
        }
        assert_eq!(notes.len(), MAX_NOTES);
        assert!(notes.get("C1", Some("1.000001")).is_none());
        assert!(notes.get("C1", Some("2.000001")).is_some());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(NOTES_FILE);
        notes.save(&path).unwrap();
        assert_eq!(ThreadNotes::load(&path).unwrap(), notes);
        assert!(
            ThreadNotes::load(&dir.path().join("missing.json"))
                .unwrap()
                .is_empty()
        );
    }
}

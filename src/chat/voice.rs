//! Voice turns in the chat: the note that asks the Queen for spoken-style answers, and the
//! reply text tapped off the stream for the speaker.

use super::protocol::{Incoming, StreamEvent};

/// Put before a message Karl spoke (not shown in his bubble): her reply is read aloud.
pub const VOICE_NOTE: &str = "[Myrstack] Karl said this by voice (hold-to-talk) and your reply \
will be read aloud. Answer like a spoken reply: one to three short sentences, the answer first, \
no markdown, tables, lists or code, round numbers, and name agents by their short name rather \
than reading out ids unless he asks.";

/// What the speaker needs from the chat, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechEvent {
    /// Karl sent a message; `voice` when he spoke it.
    TurnStart { voice: bool },
    /// A delta of the Queen's reply text (top-level text blocks only; tool calls and
    /// subagents are not spoken).
    Text(String),
    /// A content block ended: flush the sentence in progress.
    BlockEnd,
    /// The turn is over (finished, failed, interrupted or the process went away).
    TurnEnd,
}

/// The speech events in a CLI message.
pub fn tap(message: &Incoming, out: &mut Vec<SpeechEvent>) {
    match message {
        Incoming::Stream {
            parent: None,
            event,
            ..
        } => match event {
            StreamEvent::TextDelta { text, .. } if !text.is_empty() => {
                out.push(SpeechEvent::Text(text.clone()));
            }
            StreamEvent::BlockStop { .. } => out.push(SpeechEvent::BlockEnd),
            _ => {}
        },
        Incoming::Result(_) => out.push(SpeechEvent::TurnEnd),
        _ => {}
    }
}

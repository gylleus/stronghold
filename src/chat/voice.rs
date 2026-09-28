//! Spoken turns in the chat: the notes that ask the Queen for a `<say>` line, and the reply
//! text tapped off the stream for the speaker.

use super::protocol::{Incoming, StreamEvent};

/// How the spoken line is written, shared by both notes.
macro_rules! say_rules {
    () => {
        "Start your reply with exactly one <say>…</say> line; only that line is spoken. In it: \
at most 25 words, plain everyday words a non-engineer would use, the answer first, small \
rounded numbers. No ids, hashes, branch or file names and no jargon (dispatch, run id, handle, \
coordinator handle, PR number): say \"the search go-live PR\" or \"a pull request in \
depict\", and name agents by their short human name or repo. Don't list; offer the details \
instead (\"Want the list?\"). After the </say> line, write the normal detailed answer for the \
screen (markdown is fine there)."
    };
}

/// Put before a message Karl spoke (not shown in his bubble): her reply is read aloud.
pub const VOICE_NOTE: &str = concat!(
    "[Myrstack] Karl said this by voice (hold-to-talk); this turn is spoken. ",
    say_rules!()
);

/// Put before a typed message while "Speak replies" is on: her reply is read aloud.
pub const SPEAK_NOTE: &str = concat!(
    "[Myrstack] Speak replies is on; this turn is spoken. ",
    say_rules!()
);

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

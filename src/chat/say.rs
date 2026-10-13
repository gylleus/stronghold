//! The King's spoken line: a reply that is read aloud starts with one `<say>…</say>` line in
//! plain words, and only that line is spoken; the rest of the reply is for the screen.
//!
//! [`SayFilter`] picks the say-line out of the streamed reply text for the speaker;
//! [`split_say`] splits a (possibly still streaming) reply for the chat pane.

pub const OPEN: &str = "<say>";
pub const CLOSE: &str = "</say>";

/// A text block with no `<say>` in its first this many characters is spoken by its first
/// sentences instead.
pub const SAY_WINDOW: usize = 200;

/// How many sentences of a reply without a say-line are spoken.
pub const FALLBACK_SENTENCES: usize = 2;

/// What the filter passes to the speaker for one input.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Spoken {
    /// Text to speak (may be empty).
    pub text: String,
    /// The spoken part is over: flush the sentence in progress.
    pub flush: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Looking for `<say>` in the current text block.
    #[default]
    Seeking,
    /// Inside the say-line.
    Saying,
    /// No say-line: passing the block through for its first sentences.
    Fallback,
    /// Nothing more of this reply is spoken.
    Done,
}

/// Streaming filter over one turn's top-level reply text (text deltas and block ends, in
/// order): passes on only the say-line, robust to the tags being split across deltas.
///
/// - Text before `<say>` is held back; everything after `</say>` is dropped. A say-line
///   without `</say>` ends with its text block (or the turn).
/// - Nested or stray `<say>` / `</say>` tags inside spoken text are removed.
/// - A block with no `<say>` in its first [`SAY_WINDOW`] characters, or a turn that ends
///   without one, is passed through instead (the newest non-blank block at turn end) and ends
///   with that block; the caller speaks only its first [`FALLBACK_SENTENCES`] sentences
///   ([`is_fallback`](Self::is_fallback)).
#[derive(Debug, Default)]
pub struct SayFilter {
    mode: Mode,
    /// The current block's raw text while seeking.
    block: String,
    /// The newest finished non-blank block while seeking (preamble before tool calls).
    last_block: String,
    /// A possible tag cut off at the end of the last delta.
    held: String,
    fallback: bool,
}

impl SayFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The reply had no say-line and is being spoken by its first sentences.
    pub fn is_fallback(&self) -> bool {
        self.fallback
    }

    /// Feeds a text delta.
    pub fn push(&mut self, delta: &str) -> Spoken {
        match self.mode {
            Mode::Seeking => self.seek(delta),
            Mode::Saying => self.pass(delta, true),
            Mode::Fallback => self.pass(delta, false),
            Mode::Done => Spoken::default(),
        }
    }

    /// A text block ended.
    pub fn block_end(&mut self) -> Spoken {
        match self.mode {
            Mode::Seeking => {
                let block = std::mem::take(&mut self.block);
                if !block.trim().is_empty() {
                    self.last_block = block;
                }
                Spoken::default()
            }
            Mode::Saying | Mode::Fallback => self.done(String::new()),
            Mode::Done => Spoken::default(),
        }
    }

    /// The turn is over.
    pub fn turn_end(&mut self) -> Spoken {
        match self.mode {
            Mode::Seeking => {
                let block = std::mem::take(&mut self.block);
                let text = if block.trim().is_empty() {
                    std::mem::take(&mut self.last_block)
                } else {
                    block
                };
                self.mode = Mode::Fallback;
                self.fallback = true;
                let spoken = self.pass(&text, false);
                self.done(spoken.text)
            }
            Mode::Saying | Mode::Fallback => self.done(String::new()),
            Mode::Done => Spoken {
                text: String::new(),
                flush: true,
            },
        }
    }

    fn seek(&mut self, delta: &str) -> Spoken {
        self.block.push_str(delta);
        if let Some(at) = self.block.find(OPEN)
            && self.block[..at].chars().count() <= SAY_WINDOW
        {
            let after = self.block[at + OPEN.len()..].to_owned();
            self.block.clear();
            self.last_block.clear();
            self.mode = Mode::Saying;
            return self.pass(&after, true);
        }
        if self.block.chars().count() > SAY_WINDOW + OPEN.len() {
            let text = std::mem::take(&mut self.block);
            self.mode = Mode::Fallback;
            self.fallback = true;
            return self.pass(&text, false);
        }
        Spoken::default()
    }

    /// Passes `delta` on with tags removed; with `stop_at_close`, ends at `</say>`.
    fn pass(&mut self, delta: &str, stop_at_close: bool) -> Spoken {
        let mut buf = std::mem::take(&mut self.held);
        buf.push_str(delta);
        let mut out = String::new();
        let mut rest = buf.as_str();
        while let Some(at) = rest.find('<') {
            out.push_str(&rest[..at]);
            let tail = &rest[at..];
            if let Some(after) = tail.strip_prefix(CLOSE) {
                if stop_at_close {
                    return self.done(out);
                }
                rest = after;
            } else if let Some(after) = tail.strip_prefix(OPEN) {
                rest = after;
            } else if OPEN.starts_with(tail) || CLOSE.starts_with(tail) {
                self.held = tail.to_owned();
                rest = "";
            } else {
                out.push('<');
                rest = &tail[1..];
            }
        }
        out.push_str(rest);
        Spoken {
            text: out,
            flush: false,
        }
    }

    fn done(&mut self, text: String) -> Spoken {
        self.mode = Mode::Done;
        self.held.clear();
        Spoken { text, flush: true }
    }
}

/// A reply split for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaySplit {
    /// The say-line's text, tags removed (`None` when the reply has none).
    pub say: Option<String>,
    /// The rest of the reply, tags removed.
    pub rest: String,
}

/// Splits a reply into its say-line and the rest. Works on text still streaming in: an
/// unclosed say-line runs to the end, and a tag cut off at the end is hidden.
pub fn split_say(text: &str) -> SaySplit {
    let Some(at) = text.find(OPEN) else {
        return SaySplit {
            say: None,
            rest: strip_tags(trim_partial_tag(text)),
        };
    };
    let before = &text[..at];
    let inner = &text[at + OPEN.len()..];
    let (say, after) = match inner.find(CLOSE) {
        Some(end) => (&inner[..end], &inner[end + CLOSE.len()..]),
        None => (trim_partial_tag(inner), ""),
    };
    let before = strip_tags(before);
    let after = strip_tags(trim_partial_tag(after));
    let rest = match (before.trim().is_empty(), after.trim().is_empty()) {
        (true, _) => after.trim_start().to_owned(),
        (false, true) => before.trim_end().to_owned(),
        (false, false) => format!("{}\n\n{}", before.trim_end(), after.trim_start()),
    };
    SaySplit {
        say: Some(strip_tags(say).trim().to_owned()),
        rest,
    }
}

/// Removes `<say>` and `</say>` tags.
fn strip_tags(text: &str) -> String {
    text.replace(OPEN, "").replace(CLOSE, "")
}

/// `text` without a trailing cut-off tag (`<s`, `</sa`, …; a lone `<` stays).
fn trim_partial_tag(text: &str) -> &str {
    if let Some(at) = text.rfind('<') {
        let tail = &text[at..];
        if tail.len() >= 2
            && tail.len() < CLOSE.len()
            && (OPEN.starts_with(tail) || CLOSE.starts_with(tail))
        {
            return &text[..at];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `text` through the filter split at the given byte offsets, as one block and turn;
    /// returns the spoken text and whether it flushed.
    fn run(parts: &[&str]) -> (String, bool, bool) {
        let mut f = SayFilter::new();
        let mut text = String::new();
        let mut flushed = false;
        for p in parts {
            let s = f.push(p);
            assert!(!flushed || s.text.is_empty(), "nothing after the flush");
            text.push_str(&s.text);
            flushed |= s.flush;
        }
        let s = f.block_end();
        assert!(!flushed || s.text.is_empty());
        text.push_str(&s.text);
        flushed |= s.flush;
        let s = f.turn_end();
        assert!(!flushed || s.text.is_empty());
        text.push_str(&s.text);
        (text, flushed || s.flush, f.is_fallback())
    }

    fn splits(text: &str) -> Vec<Vec<&str>> {
        let bounds: Vec<usize> = (0..=text.len())
            .filter(|&i| text.is_char_boundary(i))
            .collect();
        let mut out = vec![vec![text]];
        // Every single split point, and every chunk size.
        for &i in &bounds {
            out.push(vec![&text[..i], &text[i..]]);
        }
        for size in 1..8 {
            let mut parts = Vec::new();
            let mut start = 0;
            while start < text.len() {
                let mut end = (start + size).min(text.len());
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                parts.push(&text[start..end]);
                start = end;
            }
            out.push(parts);
        }
        out
    }

    #[test]
    fn the_say_line_survives_any_chunking() {
        let reply = "  \n<say>Two agents need you — the search one and docs.</say>\n\n\
                     **Details**\n- `term:term_1` waits on <b>review</b>";
        for parts in splits(reply) {
            let (text, flushed, fallback) = run(&parts);
            assert_eq!(
                text, "Two agents need you — the search one and docs.",
                "{parts:?}"
            );
            assert!(flushed && !fallback, "{parts:?}");
        }
    }

    #[test]
    fn split_tags_like_sa_plus_y() {
        let mut f = SayFilter::new();
        assert_eq!(f.push("<sa"), Spoken::default());
        assert_eq!(
            f.push("y>All "),
            Spoken {
                text: "All ".into(),
                flush: false
            }
        );
        assert_eq!(f.push("good.</s").text, "good.");
        let end = f.push("ay> More text.");
        assert_eq!(
            end,
            Spoken {
                text: String::new(),
                flush: true
            }
        );
        assert_eq!(f.push("Even more."), Spoken::default());
        assert_eq!(f.block_end(), Spoken::default());
    }

    #[test]
    fn nested_and_malformed_tags() {
        // A nested open tag is dropped; the first close ends the line.
        assert_eq!(run(&["<say>One <say>two</say> three</say>"]).0, "One two");
        // Stray close before the open is ignored; a lone `<` is kept.
        assert_eq!(run(&["</say><say>a < b"]).0, "a < b");
        // Unclosed: runs to the end of the turn, a cut-off tag at the end is dropped.
        for parts in splits("<say>No closing tag here.</sa") {
            let (text, flushed, fallback) = run(&parts);
            assert_eq!(text, "No closing tag here.", "{parts:?}");
            assert!(flushed && !fallback);
        }
        // Tag-like text that is not a tag stays.
        assert_eq!(run(&["<say><sb>x</say>"]).0, "<sb>x");
    }

    #[test]
    fn unclosed_say_line_ends_at_turn_end_not_before() {
        let mut f = SayFilter::new();
        assert_eq!(f.push("<say>Still going").text, "Still going");
        assert_eq!(f.push(" on").text, " on");
        assert_eq!(
            f.turn_end(),
            Spoken {
                text: String::new(),
                flush: true
            }
        );
    }

    #[test]
    fn preamble_blocks_are_held_and_a_later_say_line_wins() {
        let mut f = SayFilter::new();
        assert_eq!(f.push("Let me check the clan."), Spoken::default());
        assert_eq!(f.block_end(), Spoken::default());
        assert_eq!(f.push("<say>All quiet.</say> details").text, "All quiet.");
        assert!(!f.is_fallback());
    }

    #[test]
    fn fallback_after_the_window_without_a_tag() {
        let long = format!("{} Second. Third.", "word ".repeat(40));
        let mut f = SayFilter::new();
        let first = f.push(&long[..150]);
        assert_eq!(first, Spoken::default(), "held while a tag may still come");
        let rest = f.push(&long[150..]);
        assert!(f.is_fallback());
        assert_eq!(rest.text, long, "the block passes through");
        assert_eq!(f.push(" more").text, " more");
        assert!(f.block_end().flush);
        assert_eq!(
            f.push("next block"),
            Spoken::default(),
            "ends with the block"
        );

        // A tag past the window does not count.
        let late = format!("{}<say>late</say>", "x".repeat(SAY_WINDOW + 1));
        let (text, _, fallback) = run(&[&late]);
        assert!(fallback);
        assert_eq!(text, format!("{}late", "x".repeat(SAY_WINDOW + 1)));
    }

    #[test]
    fn a_short_reply_without_a_tag_is_spoken_at_turn_end() {
        let mut f = SayFilter::new();
        assert_eq!(f.push("Checking."), Spoken::default());
        f.block_end();
        assert_eq!(f.push("Two agents need input."), Spoken::default());
        assert_eq!(
            f.turn_end(),
            Spoken {
                text: "Two agents need input.".into(),
                flush: true
            }
        );
        assert!(f.is_fallback());

        // Only a preamble: that is spoken.
        let mut f = SayFilter::new();
        f.push("Done.");
        f.block_end();
        assert_eq!(f.turn_end().text, "Done.");
    }

    #[test]
    fn split_for_display() {
        assert_eq!(
            split_say("<say>Two need you.</say>\n\n**Details**"),
            SaySplit {
                say: Some("Two need you.".into()),
                rest: "**Details**".into()
            }
        );
        assert_eq!(
            split_say("Checking.\n<say>All quiet.</say>\nMore."),
            SaySplit {
                say: Some("All quiet.".into()),
                rest: "Checking.\n\nMore.".into()
            }
        );
        // Streaming: unclosed, and a tag cut off at the end.
        assert_eq!(
            split_say("<say>Two ne"),
            SaySplit {
                say: Some("Two ne".into()),
                rest: String::new()
            }
        );
        assert_eq!(
            split_say("<say>Two.</s"),
            SaySplit {
                say: Some("Two.".into()),
                rest: String::new()
            }
        );
        assert_eq!(
            split_say("  <sa"),
            SaySplit {
                say: None,
                rest: "  ".into()
            }
        );
        // No tags: unchanged.
        let plain = "**None** need input; a < b.";
        assert_eq!(
            split_say(plain),
            SaySplit {
                say: None,
                rest: plain.into()
            }
        );
    }
}

//! Turning the Queen's markdown into speakable sentences.
//!
//! [`SentenceStream`] takes streaming text deltas and yields complete, cleaned sentences as
//! soon as they are known to be complete; [`speakable_sentences`] does the same for a whole
//! text. Cleanup drops markdown markup and code blocks, shortens tables, ids, URLs and paths,
//! and spells out symbols.

/// Said in place of a fenced code block.
pub const CODE_OMITTED: &str = "Code omitted.";

/// Tables with more data rows than this are summarised instead of read.
const MAX_SPOKEN_TABLE_ROWS: usize = 3;

/// A sentence with no end in sight is split at a clause boundary past this many bytes.
const LONG_SENTENCE: usize = 280;

/// Cleaned sentences of a whole markdown text.
pub fn speakable_sentences(markdown: &str) -> Vec<String> {
    let mut stream = SentenceStream::new();
    let mut out = stream.push(markdown);
    out.extend(stream.finish());
    out
}

/// A whole markdown text as one speakable string.
pub fn speakable_text(markdown: &str) -> String {
    speakable_sentences(markdown).join(" ")
}

/// Splits plain text into sentences, respecting abbreviations, initials and decimals.
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut rest = text.to_owned();
    let mut out = take_sentences(&mut rest);
    let tail = rest.trim();
    if !tail.is_empty() {
        out.push(tail.to_owned());
    }
    out
}

/// Incremental markdown-to-sentences: [`push`](Self::push) deltas as they arrive and speak
/// what comes back; [`finish`](Self::finish) at the end of the reply.
#[derive(Debug, Default)]
pub struct SentenceStream {
    /// The current, incomplete line.
    line: String,
    /// How the current line was classified, once enough of it has arrived.
    line_kind: Option<LineKind>,
    /// Bytes of `line` already moved into `pending`.
    consumed: usize,
    /// Inline text of the sentence(s) in progress, block markup removed.
    pending: String,
    in_code: bool,
    table: Vec<Vec<String>>,
    table_has_header: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Blank,
    Fence,
    TableRow,
    Rule,
    /// A heading, bullet or quote: spoken on its own; content starts at the byte offset.
    Block(usize),
    /// Paragraph text, continuing the current sentence; content starts at the byte offset.
    Paragraph(usize),
}

impl SentenceStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a delta; returns the sentences it completed.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = delta;
        while let Some(nl) = rest.find('\n') {
            self.line.push_str(&rest[..nl]);
            rest = &rest[nl + 1..];
            self.end_line(&mut out);
        }
        self.line.push_str(rest);
        self.feed_partial_line(&mut out);
        out
    }

    /// Flushes what is left at the end of the text.
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.line.is_empty() {
            self.end_line(&mut out);
        }
        self.flush_table(&mut out);
        self.flush_pending(&mut out);
        self.in_code = false;
        out
    }

    /// Moves as much of the incomplete line as is safe into `pending` and emits sentences.
    fn feed_partial_line(&mut self, out: &mut Vec<String>) {
        if self.in_code {
            return;
        }
        if self.line_kind.is_none() {
            let Some(kind) = classify(&self.line, false) else {
                return;
            };
            self.start_line(kind, out);
            self.line_kind = Some(kind);
        }
        if let Some(LineKind::Block(start) | LineKind::Paragraph(start)) = self.line_kind {
            let from = self.consumed.max(start);
            self.pending.push_str(&self.line[from..]);
            self.consumed = self.line.len();
            self.emit_complete(out);
        }
    }

    fn end_line(&mut self, out: &mut Vec<String>) {
        let line = std::mem::take(&mut self.line);
        let kind = self.line_kind.take();
        let consumed = std::mem::take(&mut self.consumed);
        if self.in_code {
            if classify(&line, true) == Some(LineKind::Fence) {
                self.in_code = false;
            }
            return;
        }
        let kind = match kind {
            Some(kind) => kind,
            None => {
                let kind = classify(&line, true).unwrap_or(LineKind::Paragraph(0));
                self.start_line(kind, out);
                kind
            }
        };
        match kind {
            LineKind::Blank | LineKind::Rule => self.flush_pending(out),
            LineKind::Fence => {
                self.in_code = true;
                out.push(CODE_OMITTED.to_owned());
            }
            LineKind::TableRow => self.table_row(&line),
            LineKind::Block(start) => {
                self.pending.push_str(&line[consumed.max(start)..]);
                self.flush_pending(out);
            }
            LineKind::Paragraph(start) => {
                self.pending.push_str(&line[consumed.max(start)..]);
                self.pending.push(' ');
                self.emit_complete(out);
            }
        }
    }

    /// Called once per line when its kind becomes known.
    fn start_line(&mut self, kind: LineKind, out: &mut Vec<String>) {
        if kind != LineKind::TableRow {
            self.flush_table(out);
        }
        if matches!(
            kind,
            LineKind::Block(_) | LineKind::Fence | LineKind::TableRow
        ) {
            self.flush_pending(out);
        }
    }

    fn emit_complete(&mut self, out: &mut Vec<String>) {
        for raw in take_sentences(&mut self.pending) {
            push_clean(out, &raw);
        }
    }

    fn flush_pending(&mut self, out: &mut Vec<String>) {
        self.emit_complete(out);
        let rest = std::mem::take(&mut self.pending);
        push_clean(out, &rest);
    }

    fn table_row(&mut self, line: &str) {
        let cells: Vec<String> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_owned())
            .collect();
        let separator = cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
        if separator {
            self.table_has_header = true;
        } else {
            self.table.push(cells);
        }
    }

    fn flush_table(&mut self, out: &mut Vec<String>) {
        if self.table.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.table);
        let skip = usize::from(std::mem::take(&mut self.table_has_header));
        let data = &rows[skip.min(rows.len())..];
        if data.len() > MAX_SPOKEN_TABLE_ROWS {
            out.push(format!("A table with {} rows.", data.len()));
            return;
        }
        for row in data {
            let cells: Vec<String> = row
                .iter()
                .map(|c| clean_inline(c))
                .filter(|c| !c.is_empty())
                .collect();
            if !cells.is_empty() {
                push_clean(out, &cells.join(", "));
            }
        }
    }
}

/// Classifies a line. With `complete == false` it may return `None`: not decidable yet.
fn classify(line: &str, complete: bool) -> Option<LineKind> {
    let indent = line.len() - line.trim_start().len();
    let t = &line[indent..];
    let undecided = if complete {
        Some(LineKind::Paragraph(indent))
    } else {
        None
    };
    let Some(first) = t.chars().next() else {
        return if complete {
            Some(LineKind::Blank)
        } else {
            None
        };
    };
    match first {
        '`' | '~' => {
            if t.starts_with("```") || t.starts_with("~~~") {
                Some(LineKind::Fence)
            } else if t.len() >= 3 || complete {
                Some(LineKind::Paragraph(indent))
            } else {
                None
            }
        }
        '|' => complete.then_some(LineKind::TableRow),
        '#' => {
            let hashes = t.bytes().take_while(|&b| b == b'#').count();
            match t.as_bytes().get(hashes) {
                Some(b' ') if hashes <= 6 => Some(LineKind::Block(indent + hashes + 1)),
                Some(_) => Some(LineKind::Paragraph(indent)),
                None => undecided,
            }
        }
        '>' => Some(LineKind::Block(indent + 1)),
        '-' | '*' | '+' | '_' => {
            let run = t.bytes().take_while(|&b| b == first as u8).count();
            let after = t.as_bytes().get(run);
            if after.is_none() {
                return if complete {
                    if run >= 3 && first != '+' {
                        Some(LineKind::Rule)
                    } else {
                        Some(LineKind::Blank)
                    }
                } else {
                    None
                };
            }
            if after == Some(&b' ') && run == 1 && first != '_' {
                let start = indent + 2;
                return Some(LineKind::Block(start + checkbox_len(&line[start..])));
            }
            // `***` followed by spaces etc. is still a rule; anything else is paragraph text.
            if run >= 3 && t.trim_end().bytes().all(|b| b == first as u8) {
                return if complete { Some(LineKind::Rule) } else { None };
            }
            Some(LineKind::Paragraph(indent))
        }
        '0'..='9' => {
            let digits = t.bytes().take_while(u8::is_ascii_digit).count();
            let b = t.as_bytes();
            match (b.get(digits), b.get(digits + 1)) {
                (Some(b'.' | b')'), Some(b' ')) if digits <= 3 => {
                    Some(LineKind::Block(indent + digits + 2))
                }
                (Some(b'.' | b')'), None) | (None, _) => undecided,
                _ => Some(LineKind::Paragraph(indent)),
            }
        }
        _ => Some(LineKind::Paragraph(indent)),
    }
}

/// Length of a task-list checkbox (`[ ] ` / `[x] `) at the start of `s`.
fn checkbox_len(s: &str) -> usize {
    ["[ ] ", "[x] ", "[X] "]
        .iter()
        .find(|c| s.starts_with(**c))
        .map_or(0, |c| c.len())
}

fn push_clean(out: &mut Vec<String>, raw: &str) {
    let mut s = clean_inline(raw);
    if s.is_empty() {
        return;
    }
    if s.ends_with(|c: char| c.is_alphanumeric()) {
        s.push('.');
    }
    out.push(s);
}

// ---------------------------------------------------------------------------
// Sentence boundaries
// ---------------------------------------------------------------------------

/// Words that end in a period without ending the sentence.
const ABBREVIATIONS: &[&str] = &[
    "e.g", "i.e", "vs", "cf", "mr", "mrs", "ms", "dr", "st", "jr", "sr", "approx", "fig", "incl",
    "esp", "etc", "al", "no", "min", "max", "ca",
];

/// Removes the complete sentences from the front of `text` and returns them (raw). A
/// sentence is complete when its end punctuation is followed by whitespace and more text.
fn take_sentences(text: &mut String) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let cut = sentence_end(text).or_else(|| clause_end(text));
        let Some(cut) = cut else { break };
        let sentence = text[..cut].trim().to_owned();
        text.drain(..cut);
        if !sentence.is_empty() {
            out.push(sentence);
        }
    }
    out
}

/// Byte index just past the first confirmed sentence end in `text`.
fn sentence_end(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    for (i, c) in text.char_indices() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        // Skip a run of end punctuation and closing marks: `?!`, `...`, `.)`, `."`, `.**`.
        let mut end = i + 1;
        while end < bytes.len()
            && matches!(
                bytes[end],
                b'.' | b'!' | b'?' | b')' | b']' | b'"' | b'\'' | b'*' | b'_' | b'`'
            )
        {
            end += 1;
        }
        // Needs whitespace and then something more to be sure the sentence ended.
        let after = &text[end..];
        let next = after.trim_start();
        if after.len() == next.len() || next.is_empty() {
            continue;
        }
        if c == '.' && end == i + 1 && !period_ends_sentence(&text[..i], next) {
            continue;
        }
        return Some(end);
    }
    None
}

/// Whether a period after `before` (and followed by `next`) ends a sentence.
fn period_ends_sentence(before: &str, next: &str) -> bool {
    let word = before
        .rsplit(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("");
    let lower = word
        .trim_start_matches(['"', '\'', '*', '_'])
        .to_lowercase();
    let next_upper = next.chars().next().is_some_and(char::is_uppercase);
    if lower == "etc" {
        return next_upper;
    }
    if ABBREVIATIONS.contains(&lower.as_str()) {
        return false;
    }
    // Initials: "J. Smith", "U.S. Army".
    let letters: Vec<&str> = word.split('.').collect();
    if !word.is_empty()
        && letters
            .iter()
            .all(|l| l.chars().count() == 1 && l.chars().all(char::is_alphabetic))
    {
        return false;
    }
    true
}

/// For a runaway sentence: byte index just past the last clause mark (`,` `;` `:`) before
/// [`LONG_SENTENCE`], if the text is that long.
fn clause_end(text: &str) -> Option<usize> {
    if text.len() <= LONG_SENTENCE {
        return None;
    }
    let mut limit = LONG_SENTENCE;
    while !text.is_char_boundary(limit) {
        limit -= 1;
    }
    let head = &text[..limit];
    head.rmatch_indices([',', ';', ':'])
        .map(|(i, _)| i + 1)
        .find(|&end| end > 40 && text[end..].starts_with(char::is_whitespace))
        .or_else(|| head.rfind(' ').filter(|&i| i > 40))
}

// ---------------------------------------------------------------------------
// Inline cleanup
// ---------------------------------------------------------------------------

/// Cleans one span of inline markdown into speakable text.
pub fn clean_inline(raw: &str) -> String {
    let s = strip_links(raw);
    let s = s.replace('`', " ");
    let words: Vec<String> = s.split_whitespace().filter_map(speak_token).collect();
    let s = words.join(" ");
    let s = replace_symbols(&s);
    tidy(&s)
}

/// `[text](url)` → `text`, `![alt](url)` → `alt`, `<https://…>` → `https://…`.
fn strip_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let (before, from_open) = rest.split_at(open);
        out.push_str(before.strip_suffix('!').unwrap_or(before));
        let Some(close) = from_open.find("](") else {
            out.push('[');
            rest = &from_open[1..];
            continue;
        };
        let Some(paren) = from_open[close..].find(')') else {
            out.push('[');
            rest = &from_open[1..];
            continue;
        };
        out.push_str(&from_open[1..close]);
        rest = &from_open[close + paren + 1..];
    }
    out.push_str(rest);
    out.replace("<http", "http")
        .replace(" - ", " , ")
        .replace(" – ", " , ")
        .replace(" — ", " , ")
        .replace('—', " , ")
}

/// Speakable form of one whitespace-separated token; `None` drops it.
fn speak_token(token: &str) -> Option<String> {
    let lead_len = token.len()
        - token
            .trim_start_matches(['(', '[', '"', '\'', '*', '_'])
            .len();
    let (lead, rest) = token.split_at(lead_len);
    let core_end = rest
        .trim_end_matches([
            '.', ',', ';', ':', '!', '?', ')', ']', '"', '\'', '*', '_', '>',
        ])
        .len();
    let (core, trail) = rest.split_at(core_end);
    // "e.g." / "i.e.": the trailing dot belongs to the word.
    let (core, trail) = match (core, trail.strip_prefix('.')) {
        ("e.g" | "i.e", Some(t)) => (core, t),
        _ => (core, trail),
    };
    let spoken = speak_core(core)?;
    let lead = lead.trim_start_matches(['*', '_']);
    let trail = trail.trim_start_matches(['*', '_']);
    Some(format!("{lead}{spoken}{trail}"))
}

fn speak_core(core: &str) -> Option<String> {
    if core.is_empty() {
        return Some(String::new());
    }
    if let Some(domain) = url_domain(core) {
        return Some(domain);
    }
    if let Some(id) = speak_id(core) {
        return Some(id);
    }
    let lower = core.to_lowercase();
    let word = match lower.as_str() {
        "e.g" => Some("for example"),
        "i.e" => Some("that is"),
        "vs" | "vs." => Some("versus"),
        "w/" => Some("with"),
        "w/o" => Some("without"),
        _ => None,
    };
    if let Some(word) = word {
        return Some(word.to_owned());
    }
    if let Some(rest) = core.strip_prefix('~')
        && rest.starts_with(|c: char| c.is_ascii_digit())
    {
        return speak_core(rest).map(|n| format!("about {n}"));
    }
    if let Some(number) = speak_number_with_unit(core) {
        return Some(number);
    }
    if let Some(name) = path_file_name(core) {
        return Some(name.replace('_', " "));
    }
    let core = core.replace("**", "").replace("__", "").replace("~~", "");
    let core = core.trim_matches(['*', '_']);
    // snake_case reads better as separate words.
    let core = if core.contains('_') && core.chars().any(char::is_alphanumeric) {
        core.replace('_', " ")
    } else {
        core.to_owned()
    };
    if let Some(n) = core.strip_prefix('#')
        && !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit())
    {
        return Some(format!("number {n}"));
    }
    Some(core)
}

/// `https://www.example.com/a/b` → `example.com`.
fn url_domain(core: &str) -> Option<String> {
    let rest = core
        .strip_prefix("https://")
        .or_else(|| core.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#', ':']).next().unwrap_or("");
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty()).then(|| host.to_owned())
}

/// Myrstack/Orca ids: `term:term_ab12cd…` → `terminal ab12`, `run_7c25cdcf46e0` → `run 7c25`,
/// UUIDs and long hex hashes → their first four characters.
fn speak_id(core: &str) -> Option<String> {
    let (core, forced_terminal) = match core.strip_prefix("term:") {
        Some(rest) => (rest, true),
        None => (core, false),
    };
    if let Some((prefix, suffix)) = core.split_once('_') {
        let word = match prefix {
            "term" => Some("terminal"),
            "run" => Some("run"),
            "task" => Some("task"),
            "wtr" => Some("worktree"),
            "msg" => Some("message"),
            "toolu" => Some("tool call"),
            _ => None,
        };
        if let Some(word) = word
            && !suffix.is_empty()
            && suffix.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Some(format!("{word} {}", short(suffix)));
        }
    }
    if forced_terminal && !core.is_empty() {
        return Some(format!("terminal {}", short(core)));
    }
    let is_hex = |s: &str| s.chars().all(|c| c.is_ascii_hexdigit());
    let groups: Vec<&str> = core.split('-').collect();
    if groups.len() == 5
        && groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|g| is_hex(g))
    {
        return Some(format!("id {}", short(core)));
    }
    if core.len() >= 7
        && is_hex(core)
        && core.chars().any(|c| c.is_ascii_digit())
        && core.chars().any(|c| c.is_ascii_alphabetic())
    {
        return Some(short(core));
    }
    None
}

fn short(id: &str) -> String {
    id.chars().take(4).collect()
}

/// `250ms` → `250 milliseconds`, `3.5x` → `3.5 times`, `12MB` → `12 megabytes`, `10k` → `10 thousand`.
fn speak_number_with_unit(core: &str) -> Option<String> {
    let split = core.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))?;
    let (number, unit) = core.split_at(split);
    if number.is_empty() || !number.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let unit = match unit {
        "ms" => "milliseconds",
        "s" | "sec" | "secs" => "seconds",
        "min" | "mins" => "minutes",
        "h" | "hr" | "hrs" => "hours",
        "x" | "×" => "times",
        "k" | "K" => "thousand",
        "KB" | "kB" => "kilobytes",
        "MB" => "megabytes",
        "GB" => "gigabytes",
        "%" => "percent",
        _ => return None,
    };
    Some(format!("{number} {unit}"))
}

/// `src/voice/tts.rs` → `tts.rs`; `~/.claude/settings.json` → `settings.json`.
fn path_file_name(core: &str) -> Option<&str> {
    if !core.contains('/') || core.contains("//") {
        return None;
    }
    let name = core.trim_end_matches('/').rsplit('/').next()?;
    let rooted = core.starts_with(['/', '~', '.']);
    let has_ext = name
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && (1..=5).contains(&ext.len()));
    let segments = core.split('/').filter(|s| !s.is_empty()).count();
    ((rooted || has_ext || segments >= 3) && !name.is_empty()).then_some(name)
}

fn replace_symbols(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        match c {
            '→' | '⇒' | '⟶' => out.push_str(" to "),
            '←' | '⇐' => out.push_str(" from "),
            '-' | '=' if next == Some('>') => {
                chars.next();
                out.push_str(" to ");
            }
            '<' if next == Some('-') => {
                chars.next();
                out.push_str(" from ");
            }
            '>' if next == Some('=') => {
                chars.next();
                out.push_str(" at least ");
            }
            '<' if next == Some('=') => {
                chars.next();
                out.push_str(" at most ");
            }
            '≥' => out.push_str(" at least "),
            '≤' => out.push_str(" at most "),
            '·' | '•' | '|' | '§' => out.push_str(", "),
            '&' => out.push_str(" and "),
            '≈' => out.push_str(" about "),
            '~' if next.is_some_and(|n| n.is_ascii_digit()) => out.push_str("about "),
            '×' => out.push_str(" times "),
            '±' => out.push_str(" plus or minus "),
            '…' => out.push_str("..."),
            '–' | '—' => out.push_str(", "),
            '✓' | '✔' | '✅' => out.push_str(" done "),
            '✗' | '✘' | '❌' => out.push_str(" failed "),
            '*' | '#' | '`' | '~' | '[' | ']' | '{' | '}' | '\\' | '^' => out.push(' '),
            c if is_emoji(c) => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn is_emoji(c: char) -> bool {
    matches!(u32::from(c), 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F | 0x200D)
}

/// Collapses whitespace and fixes spacing around punctuation.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        let attaches = word.starts_with([',', '.', ';', ':', '!', '?', ')']);
        if !out.is_empty() && !attaches {
            out.push(' ');
        }
        out.push_str(word);
    }
    // Collapse doubled commas left by removed symbols, and a comma before end punctuation.
    let mut s = out;
    for (from, to) in [(",,", ","), (", ,", ","), (",.", "."), (", .", ".")] {
        while s.contains(from) {
            s = s.replace(from, to);
        }
    }
    let s = s.trim_matches([',', ' ', ';', ':']).to_owned();
    if s.chars().any(char::is_alphanumeric) {
        s
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_table() {
        let cases: &[(&str, &str)] = &[
            ("**Done.** All green.", "Done. All green."),
            ("This is *really* _important_.", "This is really important."),
            ("Use `cargo test` now", "Use cargo test now."),
            (
                "See [the docs](https://example.com/x) please",
                "See the docs please.",
            ),
            (
                "Visit https://www.github.com/k2-fsa/sherpa-onnx today",
                "Visit github.com today.",
            ),
            ("Check term:term_ab12cd34ef56.", "Check terminal ab12."),
            ("run_7c25cdcf46e0 finished", "run 7c25 finished."),
            ("task_9f3e2a1b is blocked", "task 9f3e is blocked."),
            ("wtr_0a1b2c3d merged", "worktree 0a1b merged."),
            ("Commit f3fbb64 landed", "Commit f3fb landed."),
            ("idle → working", "idle to working."),
            ("a -> b", "a to b."),
            ("3 runs · 2 idle", "3 runs, 2 idle."),
            ("Edit src/voice/tts.rs next", "Edit tts.rs next."),
            (
                "Took 250ms and ~3s",
                "Took 250 milliseconds and about 3 seconds.",
            ),
            ("It's 3.5x faster", "It's 3.5 times faster."),
            ("Fixed #123 ✅", "Fixed number 123 done."),
            ("Tom & Jerry 🚀", "Tom and Jerry."),
            ("Use e.g. rust", "Use for example rust."),
            ("the speaker_names field", "the speaker names field."),
            ("cost ≈ 5 — cheap", "cost about 5, cheap."),
            ("and/or 3/4", "and/or 3/4."),
            ("```", "Code omitted."),
            ("***", ""),
        ];
        for (input, want) in cases {
            assert_eq!(&speakable_text(input), want, "input: {input:?}");
        }
    }

    #[test]
    fn markdown_blocks() {
        let md = "# Status\n\nTwo workers are idle:\n- **term_1** waiting\n- run_7c25cdcf46e0 done\n\n```rust\nfn main() {}\n```\nThat's all.";
        assert_eq!(
            speakable_sentences(md),
            [
                "Status.",
                "Two workers are idle.",
                "terminal 1 waiting.",
                "run 7c25 done.",
                "Code omitted.",
                "That's all.",
            ]
        );
    }

    #[test]
    fn numbered_lists_and_checkboxes() {
        let md = "1. First step\n2) Second step\n- [x] Checked item";
        assert_eq!(
            speakable_sentences(md),
            ["First step.", "Second step.", "Checked item."]
        );
    }

    #[test]
    fn small_tables_are_read_and_big_ones_summarised() {
        let small = "| Name | State |\n|---|---|\n| term_1 | idle |\n| term_2 | busy |\nAfter.";
        assert_eq!(
            speakable_sentences(small),
            ["terminal 1, idle.", "terminal 2, busy.", "After."]
        );
        let big = "|a|b|\n|-|-|\n|1|2|\n|3|4|\n|5|6|\n|7|8|\n";
        assert_eq!(speakable_sentences(big), ["A table with 4 rows."]);
    }

    #[test]
    fn splitter_respects_abbreviations_initials_and_decimals() {
        assert_eq!(
            split_sentences(
                "Version 3.5 is out. It is e.g. faster than v2. J. Smith agrees! Why? Mr. Lee asked."
            ),
            [
                "Version 3.5 is out.",
                "It is e.g. faster than v2.",
                "J. Smith agrees!",
                "Why?",
                "Mr. Lee asked.",
            ]
        );
        assert_eq!(
            split_sentences("Apples, pears, etc. are fruit. Also nuts etc. Then more."),
            [
                "Apples, pears, etc. are fruit.",
                "Also nuts etc.",
                "Then more."
            ]
        );
        assert_eq!(
            split_sentences("He said \"stop.\" Then left... Finally (done.) Next."),
            [
                "He said \"stop.\"",
                "Then left...",
                "Finally (done.)",
                "Next."
            ]
        );
        assert_eq!(
            split_sentences("See example.com and 1.2.3 now"),
            ["See example.com and 1.2.3 now"]
        );
    }

    #[test]
    fn splitter_breaks_runaway_sentences_at_clauses() {
        let long = format!(
            "{}, and then {}",
            "word ".repeat(40).trim(),
            "more ".repeat(40).trim()
        );
        let parts = split_sentences(&long);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0].ends_with(','));
        assert!(parts.iter().all(|p| p.len() <= LONG_SENTENCE));
    }

    #[test]
    fn stream_emits_sentences_as_soon_as_they_complete() {
        let mut s = SentenceStream::new();
        assert!(s.push("Hello the").is_empty());
        assert!(s.push("re.").is_empty());
        // The first sentence is confirmed once text follows its end.
        let mut got = s.push(" How are");
        assert_eq!(got, ["Hello there."]);
        got = s.push(" you? I'm **fi");
        assert_eq!(got, ["How are you?"]);
        got = s.push("ne**.");
        assert!(got.is_empty());
        assert_eq!(s.finish(), ["I'm fine."]);
    }

    #[test]
    fn stream_matches_whole_text_for_any_chunking() {
        let md = "# Fleet report\n\nThree runs are active. run_7c25cdcf46e0 is at 50%, e.g. halfway.\n\n- term:term_ab12cd34 idle → working\n- See https://example.com/page.\n\n```\nlet x = 1;\n```\n| a | b |\n|---|---|\n| 1 | 2 |\n\nDone!";
        let whole = speakable_sentences(md);
        for chunk in [1, 2, 3, 5, 7, 13] {
            let mut s = SentenceStream::new();
            let mut got = Vec::new();
            let chars: Vec<char> = md.chars().collect();
            for piece in chars.chunks(chunk) {
                got.extend(s.push(&piece.iter().collect::<String>()));
            }
            got.extend(s.finish());
            assert_eq!(got, whole, "chunk size {chunk}");
        }
        assert_eq!(
            whole,
            [
                "Fleet report.",
                "Three runs are active.",
                "run 7c25 is at 50 percent, for example halfway.",
                "terminal ab12 idle to working.",
                "See example.com.",
                "Code omitted.",
                "1, 2.",
                "Done!",
            ]
        );
    }

    #[test]
    fn stream_holds_back_possible_fences_and_tables() {
        let mut s = SentenceStream::new();
        assert!(s.push("``").is_empty());
        assert_eq!(s.push("`\ncode. more code. x\n"), ["Code omitted."]);
        assert!(s.push("```\n|a. b. c").is_empty());
        assert_eq!(s.push("|\n"), Vec::<String>::new());
        assert_eq!(s.push("Text.\n"), ["a. b. c."]);
        assert_eq!(s.finish(), ["Text."]);
    }

    #[test]
    fn soft_line_breaks_join_a_paragraph() {
        assert_eq!(
            speakable_sentences("This sentence\ncontinues here. Next one."),
            ["This sentence continues here.", "Next one."]
        );
    }
}

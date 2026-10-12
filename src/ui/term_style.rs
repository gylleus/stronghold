//! Styling pass for agent screens and transcript tails.
//!
//! `orca terminal read` strips escape codes in both its stream and `--screen` modes, so colour
//! is rebuilt from the shape of each line: Claude Code's `⏺`/`⎿` TUI, Codex's `•`/`└` TUI and the
//! `→`/`←`/`>` lines of a rendered transcript. [`style_lines`] is pure: the same input always
//! gives the same [`StyledLine`]s. Rendering lives in `ui::terminal`.

use eframe::egui::{Color32, Visuals};

use crate::model::NodeState;
use crate::orca::AgentIdentity;
use crate::ui::fleet_view::state_color;

/// Which TUI produced the lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Claude,
    Codex,
    /// Lines rendered by `claude::transcript` (`→ Tool(args)`, `  ← result`, `> prompt`).
    Transcript,
    /// A shell or an agent Orca did not identify; guessed from the lines.
    Unknown,
}

impl AgentKind {
    pub fn from_identity(agent: Option<&AgentIdentity>) -> Self {
        match agent {
            Some(AgentIdentity::Claude) => AgentKind::Claude,
            Some(AgentIdentity::Codex) => AgentKind::Codex,
            _ => AgentKind::Unknown,
        }
    }

    /// `Unknown` resolved from telltale markers in `lines`.
    fn resolve(self, lines: &[String]) -> Self {
        if self != AgentKind::Unknown {
            return self;
        }
        let starts = |p: &str| lines.iter().any(|l| l.starts_with(p));
        if starts("⏺ ") || lines.iter().any(|l| l.trim_start().starts_with("⎿")) {
            AgentKind::Claude
        } else if starts("• ") && lines.iter().any(|l| l.starts_with('›')) {
            AgentKind::Codex
        } else {
            AgentKind::Unknown
        }
    }
}

/// What a (logical) line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineKind {
    Blank,
    /// Assistant prose (`⏺ …`, `• …`) and its continuation.
    Assistant,
    /// A tool call header (`⏺ Bash(…)`, `• Ran …`, `→ Read(…)`).
    ToolCall,
    /// Tool output (`⎿ …`, `└ …`, `← …`, `Ran 2 shell commands`) and its continuation.
    ToolResult,
    /// A numbered `+` line of a diff.
    DiffAdd,
    /// A numbered `-` line of a diff.
    DiffDel,
    /// A numbered unchanged diff line or file listing line.
    DiffContext,
    /// A user message in the transcript.
    UserPrompt,
    /// Spinner / turn summary (`✻ Cogitating… (12s)`, `• Working (…)`).
    Status,
    /// Inside a ``` fence.
    Code,
    /// Box-drawing table row.
    Table,
    /// Markdown `#` heading.
    Heading,
    Error,
    Warning,
    /// A horizontal rule, optionally labelled (`─ Worked for 3m ───`). Drawn as a line.
    Rule,
    /// Footer, hints, right-aligned notices, idle animation.
    Chrome,
    /// The prompt input line.
    Input,
    /// The highlighted option of a choice menu (`❯ 1. Yes`).
    MenuSelected,
    Plain,
}

impl LineKind {
    /// Whether long lines of this kind wrap (rather than scroll sideways).
    pub fn wraps(self) -> bool {
        !matches!(
            self,
            LineKind::Table
                | LineKind::DiffAdd
                | LineKind::DiffDel
                | LineKind::DiffContext
                | LineKind::Code
                | LineKind::Rule
        )
    }
}

/// How a span is drawn; colours come from [`Palette::color`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    Text,
    Strong,
    Dim,
    /// Very dim chrome.
    Faint,
    AssistantBullet,
    ToolBullet,
    ToolName,
    /// User prompt text and markers.
    Accent,
    StatusGlyph,
    /// Muted italic.
    Status,
    Code,
    Link,
    Error,
    Warning,
    DiffAdd,
    DiffDel,
    LineNo,
    Input,
}

impl Style {
    pub fn italics(self) -> bool {
        self == Style::Status
    }

    /// Always drawn in the monospace font.
    pub fn monospace(self) -> bool {
        self == Style::Code
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    fn new(text: impl Into<String>, style: Style) -> Self {
        Span {
            text: text.into(),
            style,
        }
    }
}

/// One logical line: terminal-wrapped continuations of prose are joined back into it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StyledLine {
    pub kind: LineKind,
    /// Indentation and marker (`⏺ `, `  ⎿  `, `  - `), drawn monospace; wrapped rows of the body
    /// hang under the body's first character.
    pub prefix: Vec<Span>,
    pub body: Vec<Span>,
    /// The body is flowing prose that may use a proportional font.
    pub prose: bool,
}

impl StyledLine {
    pub fn prefix_text(&self) -> String {
        self.prefix.iter().map(|s| s.text.as_str()).collect()
    }

    pub fn body_text(&self) -> String {
        self.body.iter().map(|s| s.text.as_str()).collect()
    }

    pub fn text(&self) -> String {
        self.prefix_text() + &self.body_text()
    }

    /// Starts a new message or tool call (gets a little space above it).
    pub fn starts_block(&self) -> bool {
        self.prefix.iter().any(|s| {
            matches!(
                s.style,
                Style::AssistantBullet | Style::ToolBullet | Style::StatusGlyph
            )
        }) || (self.kind == LineKind::UserPrompt && !self.prefix.is_empty())
    }
}

/// Styles screen `lines` as drawn by `agent`.
pub fn style_lines(lines: &[String], agent: AgentKind) -> Vec<StyledLine> {
    let agent = agent.resolve(lines);
    let cleaned: Vec<String> = lines
        .iter()
        .map(|l| {
            let l = if agent == AgentKind::Codex {
                strip_braille(l)
            } else {
                l.clone()
            };
            l.trim_end().to_string()
        })
        .collect();
    let mut phys = classify(&cleaned, lines, agent);
    mark_input_and_footer(&mut phys, &cleaned, agent);
    let cols = if agent == AgentKind::Transcript {
        0
    } else {
        cleaned.iter().map(|l| l.chars().count()).max().unwrap_or(0)
    };
    join_and_style(phys, cols, agent)
}

// ---------------------------------------------------------------------------------------------
// Pass 1: classify each physical line.

const SPINNERS: &[char] = &['✻', '✢', '✽', '✶', '✳', '✺', '✦', '✷', '✸', '✹', '·', '*'];
const RULE_CHARS: &[char] = &['─', '━', '╌', '═', '┄', '┈'];
const BOX_CHARS: &[char] = &[
    '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '│', '─', '╭', '╮', '╰', '╯', '═', '║', '╔', '╗',
    '╚', '╝', '╠', '╣', '╦', '╩', '╬',
];
/// Claude's collapsed tool summaries (`  Ran 2 shell commands`).
const CLAUDE_SUMMARY_VERBS: &[&str] = &[
    "Ran",
    "Read",
    "Searched",
    "Called",
    "Recalled",
    "Made",
    "Created",
    "Wrote",
    "Listed",
    "Fetched",
    "Edited",
    "Updated",
    "Loaded",
    "Found",
    "Used",
    "Reading",
    "Searching",
];
/// Codex tool call headers (`• Ran cargo test`).
const CODEX_TOOL_VERBS: &[&str] = &[
    "Ran",
    "Running",
    "Explored",
    "Exploring",
    "Edited",
    "Added",
    "Deleted",
    "Read",
    "Search",
    "Searched",
    "Called",
    "Calling",
    "Proposed",
    "Updated",
    "Waited",
    "Waiting",
    "Listed",
    "List",
    "Applied",
    "Viewed",
];

/// The block a continuation line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    None,
    Assistant,
    Tool,
    Result,
    ErrorResult,
    User,
}

#[derive(Debug, Clone)]
struct Phys {
    kind: LineKind,
    prefix: Vec<Span>,
    /// Body text before inline styling, or pre-styled spans.
    body: Body,
    /// A continuation of the block above at this indent (joinable when wrapped).
    continuation: bool,
    /// Character count of the raw line.
    len: usize,
}

#[derive(Debug, Clone)]
enum Body {
    /// Inline-styled later with this base style; `markdown` enables `**`/`` ` ``/`#`.
    Inline {
        text: String,
        base: Style,
        markdown: bool,
    },
    Spans(Vec<Span>),
}

impl Phys {
    fn new(kind: LineKind, prefix: Vec<Span>, body: Body) -> Self {
        Phys {
            kind,
            prefix,
            body,
            continuation: false,
            len: 0,
        }
    }

    fn body_text(&self) -> String {
        match &self.body {
            Body::Inline { text, .. } => text.clone(),
            Body::Spans(spans) => spans.iter().map(|s| s.text.as_str()).collect(),
        }
    }
}

fn inline(text: &str, base: Style, markdown: bool) -> Body {
    Body::Inline {
        text: text.to_string(),
        base,
        markdown,
    }
}

fn plain(text: &str, style: Style) -> Body {
    Body::Spans(if text.is_empty() {
        Vec::new()
    } else {
        vec![Span::new(text, style)]
    })
}

fn strip_braille(line: &str) -> String {
    line.chars()
        .map(|c| {
            if ('\u{2800}'..='\u{28ff}').contains(&c) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

fn indent_of(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\u{a0}')
        .count()
}

/// Splits `line` after its first `n` characters.
fn split_chars(line: &str, n: usize) -> (&str, &str) {
    let at = line.char_indices().nth(n).map_or(line.len(), |(i, _)| i);
    line.split_at(at)
}

/// `line` split after `marker` and the whitespace that follows it.
fn after_marker<'a>(line: &'a str, marker: &str) -> Option<(&'a str, &'a str)> {
    let start = line.find(marker)?;
    let rest = &line[start + marker.len()..];
    let ws = rest.len() - rest.trim_start_matches([' ', '\u{a0}']).len();
    let cut = start + marker.len() + ws;
    Some(line.split_at(cut))
}

fn is_rule(body: &str) -> bool {
    body.chars().count() >= 3 && body.chars().all(|c| RULE_CHARS.contains(&c))
}

/// `─ Worked for 3m 26s ────` → the label.
fn labelled_rule(body: &str) -> Option<&str> {
    let rest = body.strip_prefix('─')?;
    let label = rest.trim_end_matches(|c| RULE_CHARS.contains(&c));
    if label.len() == rest.len() || rest.len() - label.len() < 3 * '─'.len_utf8() {
        return None;
    }
    let label = label.trim();
    (!label.is_empty()).then_some(label)
}

fn is_table(body: &str) -> bool {
    let mut chars = body.chars();
    let first = chars.next();
    let second = chars.next();
    match first {
        Some('┌' | '├' | '╭' | '╰' | '┘' | '╔' | '╚' | '╠') => true,
        Some('└') => second == Some('─'),
        Some('│' | '║') => body.chars().filter(|c| *c == '│' || *c == '║').count() >= 2,
        _ => false,
    }
}

fn is_spinner_status(body: &str) -> bool {
    let mut chars = body.chars();
    let (Some(glyph), Some(' ')) = (chars.next(), chars.next()) else {
        return false;
    };
    if !SPINNERS.contains(&glyph) {
        return false;
    }
    let rest = chars.as_str();
    let first_word = rest.split_whitespace().next().unwrap_or("");
    rest.contains('…')
        || (first_word.chars().next().is_some_and(char::is_uppercase)
            && rest.contains(" for ")
            && rest.chars().any(|c| c.is_ascii_digit()))
}

/// Numbered diff / listing line: `   634 -  text`, `  12 +text`, `     8  text`, `       2`.
fn numbered(line: &str, indent: usize, block: Block, loose: bool) -> Option<(LineKind, usize)> {
    if indent == 0 {
        return None;
    }
    let rest = &line[line.len() - line.trim_start_matches([' ', '\u{a0}']).len()..];
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 6 {
        return None;
    }
    let after = &rest[digits..];
    let prefix_len = indent + digits;
    if after.is_empty() {
        return Some((LineKind::DiffContext, prefix_len));
    }
    let mut chars = after.chars();
    if chars.next() != Some(' ') {
        return None;
    }
    match chars.next() {
        Some('+') => Some((LineKind::DiffAdd, prefix_len + 1)),
        Some('-') => Some((LineKind::DiffDel, prefix_len + 1)),
        Some(' ') => Some((LineKind::DiffContext, prefix_len + 1)),
        None => Some((LineKind::DiffContext, prefix_len + 1)),
        Some(_)
            if loose
                && indent >= 4
                && matches!(block, Block::Result | Block::Tool | Block::None) =>
        {
            Some((LineKind::DiffContext, prefix_len + 1))
        }
        _ => None,
    }
}

fn error_level(body: &str) -> Option<LineKind> {
    const ERRORS: &[&str] = &["Error", "error:", "✗", "✘", "FAILED", "fatal:", "panicked"];
    const WARNINGS: &[&str] = &["⚠", "Warning", "warning:", "■"];
    if ERRORS.iter().any(|p| body.starts_with(p)) {
        Some(LineKind::Error)
    } else if WARNINGS.iter().any(|p| body.starts_with(p)) {
        Some(LineKind::Warning)
    } else {
        None
    }
}

/// `Name(args)` → (name, args with parens). Names have no spaces, except MCP's
/// `server - tool (MCP)`.
fn tool_call(rest: &str) -> Option<(&str, &str)> {
    let open = rest.find('(')?;
    let name = &rest[..open];
    let first = name.chars().next()?;
    let mcp = name.ends_with(" (MCP)");
    if !(first.is_ascii_uppercase() || mcp) || name.len() > 60 {
        return None;
    }
    if !mcp && name.contains(char::is_whitespace) {
        return None;
    }
    Some((name, &rest[open..]))
}

fn tool_spans(name: &str, args: &str) -> Body {
    let mut spans = vec![Span::new(name, Style::ToolName)];
    if !args.is_empty() {
        spans.extend(inline_spans(args, Style::Dim, false));
    }
    Body::Spans(spans)
}

fn classify(cleaned: &[String], raw: &[String], agent: AgentKind) -> Vec<Phys> {
    let mut out = Vec::with_capacity(cleaned.len());
    let mut block = Block::None;
    let mut fence = false;
    for (line, raw) in cleaned.iter().zip(raw) {
        let mut phys = classify_one(line, raw, agent, &mut block, &mut fence);
        phys.len = line.chars().count();
        out.push(phys);
    }
    out
}

fn classify_one(
    line: &str,
    raw: &str,
    agent: AgentKind,
    block: &mut Block,
    fence: &mut bool,
) -> Phys {
    let indent = indent_of(line);
    let (lead, body) = split_chars(line, indent);
    if body.is_empty() {
        // Codex's idle animation leaves braille-only rows.
        if matches!(*block, Block::Tool | Block::Result | Block::ErrorResult) {
            *block = Block::None;
        }
        let kind = if raw.trim().is_empty() {
            LineKind::Blank
        } else {
            LineKind::Chrome
        };
        return Phys::new(kind, Vec::new(), plain("", Style::Faint));
    }
    let lead_span = |style| {
        if lead.is_empty() {
            Vec::new()
        } else {
            vec![Span::new(lead, style)]
        }
    };

    // Fenced code.
    if body.starts_with("```") {
        *fence = !*fence;
        return Phys::new(
            LineKind::Code,
            lead_span(Style::Faint),
            plain(body, Style::Faint),
        );
    }
    if *fence {
        return Phys::new(
            LineKind::Code,
            lead_span(Style::Code),
            plain(body, Style::Code),
        );
    }

    if is_rule(body) {
        *block = Block::None;
        return Phys::new(LineKind::Rule, Vec::new(), plain("", Style::Faint));
    }
    if let Some(label) = labelled_rule(body) {
        *block = Block::None;
        return Phys::new(LineKind::Rule, Vec::new(), plain(label, Style::Faint));
    }
    // Right-aligned notices (`✘ Auto-update failed`, `new task? /clear …`).
    if indent >= 20 && agent != AgentKind::Transcript {
        return Phys::new(LineKind::Chrome, Vec::new(), plain(body, Style::Faint));
    }
    if is_table(body) {
        return Phys::new(LineKind::Table, lead_span(Style::Text), table_spans(body));
    }

    match agent {
        AgentKind::Claude => {
            if let Some(p) = claude_line(line, indent, body, block) {
                return p;
            }
        }
        AgentKind::Codex => {
            if let Some(p) = codex_line(line, indent, body, block) {
                return p;
            }
        }
        AgentKind::Transcript => {
            if let Some(p) = transcript_line(line, body, block) {
                return p;
            }
        }
        AgentKind::Unknown => {}
    }

    if let Some((kind, n)) = numbered(line, indent, *block, agent == AgentKind::Claude) {
        let (number, rest) = split_chars(line, n);
        let style = match kind {
            LineKind::DiffAdd => Style::DiffAdd,
            LineKind::DiffDel => Style::DiffDel,
            _ => Style::Text,
        };
        return Phys::new(
            kind,
            vec![Span::new(number, Style::LineNo)],
            plain(rest, style),
        );
    }
    if matches!(body, "…" | "...") || body.starts_with("… +") {
        return Phys::new(
            LineKind::ToolResult,
            lead_span(Style::Faint),
            plain(body, Style::Faint),
        );
    }
    if let Some(level) = error_level(body) {
        let style = if level == LineKind::Error {
            Style::Error
        } else {
            Style::Warning
        };
        return Phys::new(level, lead_span(style), inline(body, style, false));
    }

    // Continuation of the block above.
    if indent > 0 && *block != Block::None {
        let (kind, base, markdown) = match *block {
            Block::Assistant => (LineKind::Assistant, Style::Text, true),
            Block::User => (LineKind::UserPrompt, Style::Accent, false),
            Block::Tool => (LineKind::ToolCall, Style::Dim, false),
            Block::Result => (LineKind::ToolResult, Style::Dim, false),
            Block::ErrorResult => (LineKind::Error, Style::Error, false),
            Block::None => unreachable!(),
        };
        let (prefix, body_text, kind) = list_or_heading(lead, body, kind, markdown);
        let mut phys = Phys::new(kind, prefix, inline(body_text, base, markdown));
        phys.continuation = true;
        return phys;
    }
    if indent == 0 {
        *block = Block::None;
    }
    let markdown = agent != AgentKind::Unknown;
    let (prefix, body_text, kind) = list_or_heading(lead, body, LineKind::Plain, markdown);
    let base = if kind == LineKind::Heading {
        Style::Strong
    } else {
        Style::Text
    };
    Phys::new(kind, prefix, inline(body_text, base, markdown))
}

/// Splits a list marker (`- `, `* `, `1. `) into the prefix, or recognises a `#` heading.
fn list_or_heading<'a>(
    lead: &str,
    body: &'a str,
    kind: LineKind,
    markdown: bool,
) -> (Vec<Span>, &'a str, LineKind) {
    let mut prefix = Vec::new();
    if !lead.is_empty() {
        prefix.push(Span::new(lead, Style::Text));
    }
    if markdown && matches!(kind, LineKind::Assistant | LineKind::Plain) {
        let hashes = body.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && body[hashes..].starts_with(' ') {
            return (prefix, body[hashes..].trim_start(), LineKind::Heading);
        }
    }
    let marker_len = if body.starts_with("- ") || body.starts_with("* ") || body.starts_with("• ")
    {
        Some(body.chars().next().map_or(1, char::len_utf8) + 1)
    } else {
        let digits = body.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && digits <= 3 && body[digits..].starts_with(". ")).then_some(digits + 2)
    };
    if let Some(n) = marker_len {
        prefix.push(Span::new(&body[..n], Style::Dim));
        return (prefix, &body[n..], kind);
    }
    (prefix, body, kind)
}

fn table_spans(body: &str) -> Body {
    let mut spans: Vec<Span> = Vec::new();
    for c in body.chars() {
        let style = if BOX_CHARS.contains(&c) {
            Style::Faint
        } else {
            Style::Text
        };
        push_char(&mut spans, c, style);
    }
    Body::Spans(spans)
}

fn push_char(spans: &mut Vec<Span>, c: char, style: Style) {
    match spans.last_mut() {
        Some(last) if last.style == style => last.text.push(c),
        _ => spans.push(Span::new(c.to_string(), style)),
    }
}

fn claude_line(line: &str, indent: usize, body: &str, block: &mut Block) -> Option<Phys> {
    if indent == 0 && (body.starts_with("⏺") || body.starts_with("●")) {
        let (marker, rest) = after_marker(
            line,
            if body.starts_with('⏺') {
                "⏺"
            } else {
                "●"
            },
        )?;
        if let Some((name, args)) = tool_call(rest) {
            *block = Block::Tool;
            return Some(Phys::new(
                LineKind::ToolCall,
                vec![Span::new(marker, Style::ToolBullet)],
                tool_spans(name, args),
            ));
        }
        *block = Block::Assistant;
        let (kind, base) = match error_level(rest) {
            Some(level @ LineKind::Error) => (level, Style::Error),
            Some(level) => (level, Style::Warning),
            None => (LineKind::Assistant, Style::Text),
        };
        return Some(Phys::new(
            kind,
            vec![Span::new(marker, Style::AssistantBullet)],
            inline(rest, base, true),
        ));
    }
    if body.starts_with('⎿') {
        let (marker, rest) = after_marker(line, "⎿")?;
        let error = rest.starts_with("Error") || rest.starts_with("error");
        *block = if error {
            Block::ErrorResult
        } else {
            Block::Result
        };
        let (kind, style) = if error {
            (LineKind::Error, Style::Error)
        } else {
            (LineKind::ToolResult, Style::Dim)
        };
        return Some(Phys::new(
            kind,
            vec![Span::new(marker, Style::Faint)],
            inline(rest, style, false),
        ));
    }
    if is_spinner_status(body) || body.starts_with("※ ") {
        *block = Block::None;
        let (glyph, rest) = split_chars(body, 2);
        return Some(Phys::new(
            LineKind::Status,
            vec![Span::new(glyph, Style::StatusGlyph)],
            plain(rest, Style::Status),
        ));
    }
    if let Some(marker) = ["❯", ">"].into_iter().find(|m| body.starts_with(m)) {
        let (m, rest) = after_marker(line, marker)?;
        if is_menu_option(rest) {
            *block = Block::None;
            return Some(Phys::new(
                LineKind::MenuSelected,
                vec![Span::new(m, Style::Accent)],
                plain(rest, Style::Accent),
            ));
        }
        if indent == 0 && (marker == "❯" || m.len() > marker.len() || rest.is_empty()) {
            *block = Block::User;
            return Some(Phys::new(
                LineKind::UserPrompt,
                vec![Span::new(m, Style::Accent)],
                plain(rest, Style::Accent),
            ));
        }
    }
    if indent == 2 && *block != Block::User {
        let verb = body.split_whitespace().next().unwrap_or("");
        let verb = verb.trim_end_matches(',');
        if CLAUDE_SUMMARY_VERBS.contains(&verb) && body.chars().any(|c| c.is_ascii_digit()) {
            *block = Block::None;
            let (v, rest) = body.split_at(verb.len());
            let mut spans = vec![Span::new(v, Style::Dim)];
            spans.extend(inline_spans(rest, Style::Dim, false));
            return Some(Phys::new(
                LineKind::ToolResult,
                vec![Span::new(split_chars(line, 2).0, Style::Text)],
                Body::Spans(spans),
            ));
        }
    }
    None
}

fn is_menu_option(rest: &str) -> bool {
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && rest[digits..].starts_with('.')
}

fn codex_line(line: &str, indent: usize, body: &str, block: &mut Block) -> Option<Phys> {
    if indent == 0 && body.starts_with('•') {
        let (marker, rest) = after_marker(line, "•")?;
        if rest.starts_with("Working (") || rest.starts_with("Waiting (") {
            *block = Block::None;
            return Some(Phys::new(
                LineKind::Status,
                vec![Span::new(marker, Style::StatusGlyph)],
                plain(rest, Style::Status),
            ));
        }
        let verb = rest.split_whitespace().next().unwrap_or("");
        if CODEX_TOOL_VERBS.contains(&verb) {
            *block = Block::Tool;
            let (v, args) = rest.split_at(verb.len());
            return Some(Phys::new(
                LineKind::ToolCall,
                vec![Span::new(marker, Style::ToolBullet)],
                tool_spans(v, args),
            ));
        }
        *block = Block::Assistant;
        return Some(Phys::new(
            LineKind::Assistant,
            vec![Span::new(marker, Style::AssistantBullet)],
            inline(rest, Style::Text, true),
        ));
    }
    if indent >= 2 && (body.starts_with("└ ") || body.starts_with("│ ")) {
        let tree = &body[..body.chars().next().map_or(1, char::len_utf8)];
        let (marker, rest) = after_marker(line, tree)?;
        let command = tree == "│";
        if !command {
            *block = Block::Result;
        }
        let kind = match error_level(rest) {
            Some(level) => level,
            None if command => LineKind::ToolCall,
            None => LineKind::ToolResult,
        };
        return Some(Phys::new(
            kind,
            vec![Span::new(marker, Style::Faint)],
            inline(rest, Style::Dim, false),
        ));
    }
    if body.starts_with('›') {
        let (m, rest) = after_marker(line, "›")?;
        if is_menu_option(rest) {
            return Some(Phys::new(
                LineKind::MenuSelected,
                vec![Span::new(m, Style::Accent)],
                plain(rest, Style::Accent),
            ));
        }
        if indent == 0 {
            *block = Block::User;
            return Some(Phys::new(
                LineKind::UserPrompt,
                vec![Span::new(m, Style::Accent)],
                plain(rest, Style::Accent),
            ));
        }
    }
    None
}

fn transcript_line(line: &str, body: &str, block: &mut Block) -> Option<Phys> {
    if let Some((marker, rest)) = line.strip_prefix("→ ").map(|r| ("→ ", r)) {
        *block = Block::None;
        let body = match tool_call(rest) {
            Some((name, args)) => tool_spans(name, args),
            None => inline(rest, Style::Dim, false),
        };
        return Some(Phys::new(
            LineKind::ToolCall,
            vec![Span::new(marker, Style::ToolBullet)],
            body,
        ));
    }
    if let Some(rest) = line.strip_prefix("  ← ") {
        *block = Block::None;
        if let Some(err) = rest.strip_prefix("error: ") {
            return Some(Phys::new(
                LineKind::Error,
                vec![Span::new("  ← ", Style::Faint)],
                plain(&format!("error: {err}"), Style::Error),
            ));
        }
        return Some(Phys::new(
            LineKind::ToolResult,
            vec![Span::new("  ← ", Style::Faint)],
            inline(rest, Style::Dim, false),
        ));
    }
    if let Some(rest) = line.strip_prefix("> ") {
        *block = Block::None;
        return Some(Phys::new(
            LineKind::UserPrompt,
            vec![Span::new("> ", Style::Accent)],
            plain(rest, Style::Accent),
        ));
    }
    // Everything else is assistant text (raw markdown).
    *block = Block::None;
    let indent = indent_of(line);
    let (lead, _) = split_chars(line, indent);
    if let Some(level) = error_level(body) {
        let style = if level == LineKind::Error {
            Style::Error
        } else {
            Style::Warning
        };
        return Some(Phys::new(level, Vec::new(), inline(line, style, false)));
    }
    let (prefix, text, kind) = list_or_heading(lead, body, LineKind::Assistant, true);
    let base = if kind == LineKind::Heading {
        Style::Strong
    } else {
        Style::Text
    };
    Some(Phys::new(kind, prefix, inline(text, base, true)))
}

/// Marks the prompt input box and everything below it as chrome.
fn mark_input_and_footer(phys: &mut [Phys], cleaned: &[String], agent: AgentKind) {
    match agent {
        AgentKind::Claude => {
            let rules: Vec<usize> = phys
                .iter()
                .enumerate()
                .filter(|(i, p)| p.kind == LineKind::Rule && is_rule(cleaned[*i].trim()))
                .map(|(i, _)| i)
                .collect();
            let [.., top, bottom] = rules[..] else {
                return;
            };
            let inside = &phys[top + 1..bottom];
            let is_input = !inside.is_empty()
                && inside.len() <= 12
                && inside[0].kind == LineKind::UserPrompt
                && inside[1..]
                    .iter()
                    .all(|p| p.continuation || p.kind == LineKind::Blank);
            if !is_input {
                return;
            }
            for p in &mut phys[top + 1..bottom] {
                to_input(p);
            }
            for p in &mut phys[bottom + 1..] {
                to_chrome(p);
            }
            // The blank or whitespace row the box leaves above itself.
            if top > 0 && phys[top - 1].kind == LineKind::Blank {
                phys[top - 1].kind = LineKind::Chrome;
            }
        }
        AgentKind::Codex => {
            let len = phys.len();
            let Some(at) = (len.saturating_sub(8)..len)
                .rev()
                .find(|&i| phys[i].kind == LineKind::UserPrompt && cleaned[i].starts_with('›'))
            else {
                return;
            };
            to_input(&mut phys[at]);
            for p in &mut phys[at + 1..] {
                to_chrome(p);
            }
        }
        _ => {}
    }
}

fn to_input(p: &mut Phys) {
    p.kind = LineKind::Input;
    p.continuation = false;
    let text = p.body_text();
    p.body = plain(&text, Style::Input);
}

fn to_chrome(p: &mut Phys) {
    if p.kind == LineKind::Blank || p.kind == LineKind::Rule {
        return;
    }
    let text = p.body_text();
    let prefix: String = p.prefix.iter().map(|s| s.text.as_str()).collect();
    p.kind = LineKind::Chrome;
    p.prefix = Vec::new();
    p.body = plain((prefix + &text).trim(), Style::Faint);
}

// ---------------------------------------------------------------------------------------------
// Pass 2: join terminal-wrapped prose and style inline markup.

fn joinable(kind: LineKind) -> bool {
    matches!(
        kind,
        LineKind::Assistant
            | LineKind::UserPrompt
            | LineKind::ToolResult
            | LineKind::ToolCall
            | LineKind::Error
            | LineKind::Warning
            | LineKind::Plain
    )
}

fn join_and_style(phys: Vec<Phys>, cols: usize, agent: AgentKind) -> Vec<StyledLine> {
    let mut out: Vec<(Phys, usize)> = Vec::with_capacity(phys.len());
    for p in phys {
        if cols >= 40
            && p.continuation
            && let Some((prev, prev_len)) = out.last_mut()
            && joinable(prev.kind)
            && p.kind == prev.kind
            && p.prefix.len() == 1
            && p.prefix[0].text.chars().count() == prefix_width(prev)
        {
            let text = p.body_text();
            let first_word = text
                .split_whitespace()
                .next()
                .map_or(0, |w| w.chars().count());
            // A word-wrapper breaks the line only when the next word would not fit.
            if *prev_len + 1 + first_word + WRAP_SLACK > cols
                && let Body::Inline {
                    text: prev_text, ..
                } = &mut prev.body
            {
                prev_text.push(' ');
                prev_text.push_str(&text);
                *prev_len = p.len;
                continue;
            }
        }
        let len = p.len;
        out.push((p, len));
    }
    out.into_iter()
        .map(|(p, _)| {
            let prose = agent != AgentKind::Unknown
                && matches!(
                    p.kind,
                    LineKind::Assistant | LineKind::UserPrompt | LineKind::Heading
                );
            let body = match p.body {
                Body::Inline {
                    text,
                    base,
                    markdown,
                } => inline_spans(&text, base, markdown),
                Body::Spans(spans) => spans,
            };
            StyledLine {
                kind: p.kind,
                prefix: p.prefix,
                body,
                prose,
            }
        })
        .collect()
}

/// Columns a wrapped line may stop short of the screen edge.
const WRAP_SLACK: usize = 2;

fn prefix_width(p: &Phys) -> usize {
    p.prefix.iter().map(|s| s.text.chars().count()).sum()
}

/// Inline markup of `text`: `` `code` `` and `**bold**` (with `markdown`), URLs and file paths,
/// and error/warning words.
pub fn inline_spans(text: &str, base: Style, markdown: bool) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if markdown {
            if let Some(after) = rest.strip_prefix('`')
                && let Some(end) = after.find('`')
                && end > 0
            {
                push_span(&mut spans, &after[..end], Style::Code);
                rest = &after[end + 1..];
                continue;
            }
            if let Some(after) = rest.strip_prefix("**")
                && let Some(end) = after.find("**")
                && end > 0
            {
                push_span(&mut spans, &after[..end], Style::Strong);
                rest = &after[end + 2..];
                continue;
            }
        }
        // Up to the next markup opener or whitespace boundary.
        let ws_end = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        if ws_end > 0 {
            push_span(&mut spans, &rest[..ws_end], base);
            rest = &rest[ws_end..];
            continue;
        }
        let mut word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if markdown {
            for opener in ["`", "**"] {
                if let Some(i) = rest[..word_end].find(opener)
                    && i > 0
                {
                    word_end = word_end.min(i);
                }
            }
        }
        let word = &rest[..word_end];
        word_spans(&mut spans, word, base);
        rest = &rest[word_end..];
    }
    spans
}

fn push_span(spans: &mut Vec<Span>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(last) if last.style == style => last.text.push_str(text),
        _ => spans.push(Span::new(text, style)),
    }
}

fn word_spans(spans: &mut Vec<Span>, word: &str, base: Style) {
    let core_start = word
        .find(|c: char| !matches!(c, '(' | '[' | '"' | '\'' | '<' | '{'))
        .unwrap_or(word.len());
    let core_end = word
        .rfind(|c: char| {
            !matches!(
                c,
                '.' | ',' | ';' | ':' | ')' | ']' | '"' | '\'' | '>' | '}' | '!' | '?'
            )
        })
        .map_or(core_start, |i| {
            i + word[i..].chars().next().map_or(1, char::len_utf8)
        });
    let core_end = core_end.max(core_start);
    let (head, core, tail) = (
        &word[..core_start],
        &word[core_start..core_end],
        &word[core_end..],
    );
    let style = if is_link(core) {
        Style::Link
    } else if is_error_word(core) {
        Style::Error
    } else if matches!(core, "⚠" | "⚠️") {
        Style::Warning
    } else {
        push_span(spans, word, base);
        return;
    };
    push_span(spans, head, base);
    push_span(spans, core, style);
    push_span(spans, tail, base);
}

fn is_error_word(word: &str) -> bool {
    matches!(
        word,
        "failed" | "FAILED" | "Failed" | "Error" | "error" | "ERROR" | "✗" | "✘" | "panicked"
    )
}

const PATH_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "json", "jsonl", "md", "toml", "yml", "yaml", "py",
    "sh", "txt", "lock", "sql", "html", "css", "go", "rb", "java", "kt", "swift", "c", "h", "cpp",
    "svelte", "vue", "env", "cfg", "ini", "xml", "csv", "log",
];

fn is_link(word: &str) -> bool {
    if word.starts_with("http://") || word.starts_with("https://") {
        return word.len() > 8;
    }
    if word.len() < 3 || !word.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    if !word.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | ':' | '@' | '+')
    }) {
        return false;
    }
    // Drop a `:line:col` suffix.
    let path = word.split(':').next().unwrap_or(word);
    let has_ext = path
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && PATH_EXTENSIONS.contains(&ext));
    let rooted = ["/", "~/", "./", "../"].iter().any(|p| path.starts_with(p));
    has_ext || (rooted && path.len() > 2)
}

// ---------------------------------------------------------------------------------------------
// Colours.

/// Colours for [`Style`]s and row backgrounds, derived from the egui theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub text: Color32,
    pub strong: Color32,
    pub dim: Color32,
    pub faint: Color32,
    pub assistant: Color32,
    pub tool: Color32,
    pub accent: Color32,
    pub accent_bg: Color32,
    pub status_glyph: Color32,
    pub code: Color32,
    pub code_bg: Color32,
    pub link: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub add: Color32,
    pub add_bg: Color32,
    pub del: Color32,
    pub del_bg: Color32,
    pub input_bg: Color32,
    pub screen_bg: Color32,
}

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// `a` moved `t` of the way towards `b`, opaque.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

fn tint(color: Color32, alpha: f32) -> Color32 {
    color.gamma_multiply(alpha)
}

impl Palette {
    pub fn new(visuals: &Visuals) -> Self {
        let dark = visuals.dark_mode;
        let pick = |d: u32, l: u32| rgb(if dark { d } else { l });
        let weak = visuals.weak_text_color();
        let accent = pick(0xc4b5fd, 0x6d28d9);
        let add = state_color(NodeState::Done, dark);
        let del = state_color(NodeState::Failed, dark);
        Palette {
            dark,
            text: visuals.text_color(),
            strong: visuals.strong_text_color(),
            dim: weak,
            faint: mix(weak, visuals.extreme_bg_color, 0.4),
            assistant: state_color(NodeState::Working, dark),
            tool: add,
            accent,
            accent_bg: tint(accent, if dark { 0.12 } else { 0.09 }),
            status_glyph: state_color(NodeState::Stalled, dark),
            code: pick(0xe8b77a, 0x9a3412),
            code_bg: visuals.code_bg_color,
            link: visuals.hyperlink_color,
            error: visuals.error_fg_color,
            warning: visuals.warn_fg_color,
            add: pick(0x86efac, 0x166534),
            add_bg: tint(add, if dark { 0.16 } else { 0.14 }),
            del: pick(0xfca5a5, 0x991b1b),
            del_bg: tint(del, if dark { 0.16 } else { 0.12 }),
            input_bg: tint(visuals.selection.bg_fill, if dark { 0.35 } else { 0.25 }),
            screen_bg: visuals.extreme_bg_color,
        }
    }

    pub fn color(&self, style: Style) -> Color32 {
        match style {
            Style::Text => self.text,
            Style::Strong | Style::ToolName | Style::Input => self.strong,
            Style::Dim | Style::Status | Style::LineNo => self.dim,
            Style::Faint => self.faint,
            Style::AssistantBullet => self.assistant,
            Style::ToolBullet => self.tool,
            Style::Accent => self.accent,
            Style::StatusGlyph => self.status_glyph,
            Style::Code => self.code,
            Style::Link => self.link,
            Style::Error => self.error,
            Style::Warning => self.warning,
            Style::DiffAdd => self.add,
            Style::DiffDel => self.del,
        }
    }

    /// Background of a span (inline code), transparent otherwise.
    pub fn span_bg(&self, style: Style, kind: LineKind) -> Color32 {
        if style == Style::Code && kind != LineKind::Code {
            self.code_bg
        } else {
            Color32::TRANSPARENT
        }
    }

    /// Full-width background of a row of `kind`.
    pub fn row_bg(&self, kind: LineKind) -> Option<Color32> {
        match kind {
            LineKind::UserPrompt | LineKind::MenuSelected => Some(self.accent_bg),
            LineKind::Input => Some(self.input_bg),
            LineKind::DiffAdd => Some(self.add_bg),
            LineKind::DiffDel => Some(self.del_bg),
            LineKind::Code => Some(self.code_bg),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "term_style_tests.rs"]
mod tests;

//! Did a freshly launched agent take its prompt? Classifies a rendered screen (`orca terminal
//! read --screen`) so the executor can answer startup modals and submit a parked prompt.
//!
//! Orca can report a launch as done while the prompt sits unsubmitted in the input box
//! (bracketed paste) or behind Claude's "Bypass Permissions" modal on a new worktree; see the
//! clan playbook's *Setup and dispatch*.

use super::Agent;

/// What the prompt should look like once the agent has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// This prompt, echoed as a user message or being worked on.
    Prompt(String),
    /// Some input Stronghold didn't write (a worker's dispatch preamble).
    AnyInput,
    /// No prompt: the agent's input box showing is enough.
    Ready,
}

/// What a screen shows about the launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenVerdict {
    /// The agent took the prompt: it is echoed above the input box, or the agent is working.
    Started,
    /// The prompt sits in the input box, not submitted: press Enter.
    Parked,
    /// Claude's "Bypass Permissions mode" warning: choose "2. Yes, I accept".
    BypassModal,
    /// A "do you trust this folder" question: choose "1. Yes".
    TrustPrompt,
    /// Codex's startup "Update available" menu, whose first option runs `brew upgrade`: skip
    /// it with Esc, never Enter. A brief pasted into it is lost.
    UpdatePrompt,
    /// The input box is empty and nothing is running: the prompt isn't submitted yet, or its
    /// echo scrolled away after a quick reply.
    IdleEmpty,
    /// No agent UI yet (the shell, or the agent still drawing).
    Booting,
}

/// Rows from the bottom that count as the live UI.
const WINDOW: usize = 60;
/// Characters of the prompt compared against echoed or parked text.
const MATCH_CHARS: usize = 24;

/// Lower-cased fragments of Claude's Bypass Permissions warning.
const BYPASS_MARKERS: &[&str] = &["bypass permissions mode"];
const BYPASS_ACCEPT: &str = "yes, i accept";
/// Lower-cased fragments of folder trust questions (Claude and Codex).
const TRUST_MARKERS: &[&str] = &[
    "do you trust the files",
    "do you trust the contents",
    "trust this folder",
    "quick safety check",
];
/// Lower-cased title and options of Codex's startup update menu.
const UPDATE_MARKERS: &[&str] = &["update available"];
const UPDATE_OPTIONS: &[&str] = &["update now", "skip until next version"];
/// Lower-cased hints that the agent is working.
const WORKING_MARKERS: &[&str] = &["esc to interrupt"];
/// Lower-cased starts of an input box's placeholder text.
const PLACEHOLDERS: &[&str] = &["try \"", "ask codex", "find and fix", "type your message"];

/// Lower-cased row of Codex's startup header box.
const CODEX_HEADER: &str = ">_ openai codex";
/// Lower-cased header row Codex draws only when it runs with approvals and sandbox off.
const CODEX_YOLO: &str = "permissions: yolo mode";

/// Whether `lines` show Codex's startup header without "permissions: YOLO mode": the session
/// asks for approval. `false` when the header isn't on screen, as nothing can be told then.
pub fn codex_approvals_on(lines: &[String]) -> bool {
    let lower: Vec<String> = lines.iter().map(|l| l.to_lowercase()).collect();
    lower.iter().any(|l| l.contains(CODEX_HEADER)) && !lower.iter().any(|l| l.contains(CODEX_YOLO))
}

/// Classifies `lines` (a rendered screen, top to bottom) for a launch of `agent`.
pub fn classify_screen(lines: &[String], agent: Agent, expect: &Expect) -> ScreenVerdict {
    let rows: Vec<&str> = lines.iter().map(|l| l.trim_end()).collect();
    let Some(end) = rows.iter().rposition(|l| !l.trim().is_empty()) else {
        return ScreenVerdict::Booting;
    };
    let rows = &rows[end.saturating_sub(WINDOW - 1)..=end];
    let lower: Vec<String> = rows.iter().map(|r| r.to_lowercase()).collect();
    let has = |markers: &[&str]| lower.iter().any(|l| markers.iter().any(|m| l.contains(m)));

    if has(BYPASS_MARKERS) && lower.iter().any(|l| l.contains(BYPASS_ACCEPT)) {
        return ScreenVerdict::BypassModal;
    }
    if has(UPDATE_MARKERS) && has(UPDATE_OPTIONS) {
        return ScreenVerdict::UpdatePrompt;
    }
    if has(TRUST_MARKERS) && lower.iter().any(|l| is_menu_row(l)) {
        return ScreenVerdict::TrustPrompt;
    }

    let inputs: Vec<(usize, &str)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| input_text(r, agent).map(|t| (i, t)))
        .collect();
    let Some(&(_, composer)) = inputs.last() else {
        return ScreenVerdict::Booting;
    };
    let echoes = &inputs[..inputs.len() - 1];
    let working = has(WORKING_MARKERS);

    let matches = |text: &str| match expect {
        Expect::Prompt(prompt) => starts_like(text, prompt),
        Expect::AnyInput => !text.is_empty() && !is_placeholder(text),
        Expect::Ready => false,
    };
    let pasted = composer.to_lowercase().starts_with("[pasted text");

    if matches!(expect, Expect::Ready) {
        return if composer.is_empty() || is_placeholder(composer) || working {
            ScreenVerdict::Started
        } else {
            ScreenVerdict::Parked
        };
    }
    if working || echoes.iter().any(|(_, t)| matches(t)) {
        return ScreenVerdict::Started;
    }
    if pasted || (!composer.is_empty() && matches(composer)) {
        return ScreenVerdict::Parked;
    }
    if composer.is_empty() || is_placeholder(composer) {
        return ScreenVerdict::IdleEmpty;
    }
    // Something else is typed in the box; leave it alone.
    ScreenVerdict::Booting
}

/// The text of an input-box row (`❯ …` for Claude, `› …` for Codex), without the glyph.
/// Numbered menu rows (`❯ 1. Yes`) are not input.
fn input_text(row: &str, agent: Agent) -> Option<&str> {
    let glyph = match agent {
        Agent::Claude => '❯',
        Agent::Codex => '›',
    };
    let rest = row.trim_start().strip_prefix(glyph)?;
    let text = rest.trim_matches(|c: char| c.is_whitespace() || is_braille(c));
    (!is_menu_row(text)).then_some(text)
}

fn is_braille(c: char) -> bool {
    ('\u{2800}'..='\u{28FF}').contains(&c)
}

/// `1. Yes`, `❯ 2. No, exit`, `│ ❯ 1. No, exit`.
fn is_menu_row(row: &str) -> bool {
    let row = row.trim_start_matches(|c: char| c.is_whitespace() || "│❯›>".contains(c));
    let digits = row.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && row[digits..].starts_with(". ")
}

fn is_placeholder(text: &str) -> bool {
    let lower = text.to_lowercase();
    PLACEHOLDERS.iter().any(|p| lower.starts_with(p))
}

/// Whether `shown` (one screen row) starts like `prompt`, ignoring whitespace differences.
fn starts_like(shown: &str, prompt: &str) -> bool {
    let norm = |s: &str| -> String {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MATCH_CHARS)
            .collect()
    };
    let want = norm(prompt);
    let got = norm(shown);
    if want.is_empty() || got.is_empty() {
        return false;
    }
    // A narrow screen wraps the prompt, so a short row only has to match its own length.
    let n = want.chars().count().min(got.chars().count());
    n >= want.chars().count().min(8) && want.chars().take(n).eq(got.chars().take(n))
}

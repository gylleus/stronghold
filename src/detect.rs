//! NeedsInput detection: recognizes permission prompts, choice menus and confirmations on an
//! agent's rendered screen ([`detect_prompt`]) or, more loosely, in a terminal preview
//! ([`detect_in_preview`]).
//!
//! Only the bottom of the screen counts: the active prompt is always drawn there, and anything
//! above the agent's input line (Claude's `❯`, Codex's `›`) is transcript, where old prompts and
//! quoted prompt text are harmless.

use crate::orca::AgentIdentity;

/// What kind of input a waiting terminal asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    /// A tool permission or approval: Claude Code's "Do you want to proceed?", plan approval,
    /// Codex's command and edit approvals.
    Permission,
    /// A numbered menu that is not an approval (AskUserQuestion, Codex's model switch menu).
    Choice { options: Vec<String> },
    /// A yes/no or press-enter confirmation (`[y/N]`, "Press enter to confirm").
    Confirm,
    /// A free-text prompt such as a password or passphrase request.
    FreeformQuestion,
}

/// Screen rows (after trailing blank rows are dropped) that [`detect_prompt`] looks at.
pub const SCREEN_WINDOW: usize = 30;

/// Non-blank rows at the bottom searched for a prompt that has no numbered menu.
const TAIL_ROWS: usize = 8;

/// Rows above a menu's first option searched for the question that makes it a permission.
const QUESTION_ROWS_ABOVE_MENU: usize = 12;

/// Most non-option rows (option descriptions, separators) allowed between two options.
const MAX_OPTION_GAP: usize = 4;

/// Lower-cased fragments that only appear in a permission or approval dialog.
const PERMISSION_MARKERS: &[&str] = &[
    // Claude Code tool permissions.
    "do you want to proceed?",
    "do you want to make this edit",
    "do you want to create",
    "do you want to allow",
    "do you want to overwrite",
    "tab to amend",
    // Claude Code plan approval.
    "would you like to proceed?",
    // Codex approvals.
    "would you like to run the following command",
    "would you like to make the following edits",
    "allow command?",
];

/// Lower-cased footer hints of a selection menu.
const CHOICE_HINTS: &[&str] = &["enter to select", "↑/↓ to navigate"];

/// Lower-cased hints that make a numbered list a live menu even without a visible cursor.
const MENU_HINTS: &[&str] = &[
    "enter to select",
    "↑/↓ to navigate",
    "press enter to confirm",
    "enter to confirm",
    "esc to cancel",
    "esc to go back",
];

/// Lower-cased hints of a confirmation without a menu.
const CONFIRM_HINTS: &[&str] = &["press enter to confirm", "press enter to continue"];

/// Lower-cased endings of a yes/no question line (after trailing `:`, `?` and spaces).
const CONFIRM_SUFFIXES: &[&str] = &[
    "[y/n]",
    "(y/n)",
    "[yes/no]",
    "(yes/no)",
    "(yes/no/[fingerprint])",
];

/// Glyphs that mark the selected option of a menu.
const CURSORS: &[char] = &['❯', '›', '▶', '>'];

/// Does the bottom of this rendered screen show the agent waiting on a prompt?
///
/// Looks at the last [`SCREEN_WINDOW`] non-blank-trailing rows, below the last input line of
/// `agent` (Claude `❯`, Codex `›`, either when unknown). The idle input box alone is never a
/// prompt.
pub fn detect_prompt(lines: &[String], agent: Option<&AgentIdentity>) -> Option<PromptKind> {
    let rows: Vec<&str> = lines.iter().map(|l| l.trim_end()).collect();
    let end = rows.iter().rposition(|l| !is_blank(l))? + 1;
    let window = &rows[end.saturating_sub(SCREEN_WINDOW)..end];
    let from = window
        .iter()
        .rposition(|l| is_input_line(l, agent))
        .map_or(0, |i| i + 1);
    classify(&window[from..])
}

/// Looser check on a terminal preview (recent output, not a laid-out screen): the same markers,
/// without the cut at the input line.
pub fn detect_in_preview(preview: &str) -> Option<PromptKind> {
    let rows: Vec<&str> = preview.lines().map(str::trim_end).collect();
    classify(&rows)
}

fn is_blank(line: &str) -> bool {
    line.chars().all(|c| c.is_whitespace())
}

/// The agent's input line: the idle composer, or an earlier user message in the transcript.
/// Both mean everything above is history.
fn is_input_line(line: &str, agent: Option<&AgentIdentity>) -> bool {
    let glyphs: &[char] = match agent {
        Some(AgentIdentity::Claude) => &['❯'],
        Some(AgentIdentity::Codex) => &['›'],
        _ => &['❯', '›'],
    };
    line.trim_start().starts_with(glyphs) && parse_option(line).is_none()
}

/// One numbered menu row: `❯ 1. Yes`, `  2. No, and tell Claude …`.
#[derive(Debug, Clone, PartialEq)]
struct MenuOption<'a> {
    cursor: bool,
    number: u32,
    label: &'a str,
}

fn parse_option(line: &str) -> Option<MenuOption<'_>> {
    let mut rest = line.trim_start();
    let mut cursor = false;
    if let Some(stripped) = rest.strip_prefix(CURSORS) {
        cursor = true;
        rest = stripped.trim_start();
    }
    let digits = rest.find(|c: char| !c.is_ascii_digit())?;
    if digits == 0 || digits > 2 {
        return None;
    }
    let number = rest[..digits].parse().ok()?;
    let after = rest[digits..].strip_prefix('.')?;
    if !after.starts_with(char::is_whitespace) {
        return None;
    }
    // Codex puts an option's description on the same row, after a run of spaces.
    let label = after.trim().split("  ").next().unwrap_or("").trim();
    if label.is_empty() {
        return None;
    }
    Some(MenuOption {
        cursor,
        number,
        label,
    })
}

/// A numbered menu: options `1..n` in order, at most [`MAX_OPTION_GAP`] rows apart.
struct Menu {
    /// Row of option 1.
    first_row: usize,
    options: Vec<String>,
}

/// The last live menu in `rows`: anchored on the selected option, or on the last option when a
/// menu hint is shown.
fn find_menu(rows: &[&str], lower: &[String]) -> Option<Menu> {
    let parsed: Vec<Option<MenuOption>> = rows.iter().map(|l| parse_option(l)).collect();
    let anchor = parsed
        .iter()
        .rposition(|o| o.as_ref().is_some_and(|o| o.cursor))
        .or_else(|| {
            contains_any(lower, MENU_HINTS)
                .then(|| parsed.iter().rposition(Option::is_some))
                .flatten()
        })?;
    let number_at = |row: usize| parsed[row].as_ref().map(|o| o.number);

    // Walk back to option 1.
    let mut first_row = anchor;
    let mut number = number_at(anchor)?;
    while number > 1 {
        let lo = first_row.saturating_sub(MAX_OPTION_GAP + 1);
        first_row = (lo..first_row)
            .rev()
            .find(|&r| number_at(r) == Some(number - 1))?;
        number -= 1;
    }
    if number != 1 {
        return None;
    }

    // Collect forward while the numbering continues.
    let mut options = Vec::new();
    let mut row = first_row;
    let mut next = 1;
    loop {
        options.push(parsed[row].as_ref()?.label.to_string());
        next += 1;
        let hi = (row + MAX_OPTION_GAP + 2).min(rows.len());
        match (row + 1..hi).find(|&r| number_at(r) == Some(next)) {
            Some(r) => row = r,
            None => break,
        }
    }
    (options.len() >= 2).then_some(Menu { first_row, options })
}

fn contains_any(lower: &[String], markers: &[&str]) -> bool {
    lower.iter().any(|l| markers.iter().any(|m| l.contains(m)))
}

/// A yes/no menu such as Claude's "1. Yes / 2. Yes, and don't ask again / 3. No".
fn is_approval_menu(options: &[String]) -> bool {
    let starts = |o: &String, word: &str| {
        let o = o.to_lowercase();
        o == word || o.starts_with(&format!("{word},")) || o.starts_with(&format!("{word} "))
    };
    options.first().is_some_and(|o| starts(o, "yes")) && options.iter().any(|o| starts(o, "no"))
}

fn ends_with_confirm(lower: &str) -> bool {
    let trimmed = lower.trim_end_matches(|c: char| c == ':' || c == '?' || c.is_whitespace());
    CONFIRM_SUFFIXES.iter().any(|s| trimmed.ends_with(s))
}

fn is_freeform(lower: &str) -> bool {
    let trimmed = lower.trim();
    trimmed.ends_with(':') && (trimmed.contains("password") || trimmed.contains("passphrase"))
}

/// Classifies the active region of a screen (or a preview).
fn classify(rows: &[&str]) -> Option<PromptKind> {
    let lower: Vec<String> = rows.iter().map(|l| l.to_lowercase()).collect();
    if let Some(menu) = find_menu(rows, &lower) {
        let question = &lower[menu.first_row.saturating_sub(QUESTION_ROWS_ABOVE_MENU)..];
        let approval = is_approval_menu(&menu.options) && !contains_any(&lower, CHOICE_HINTS);
        return Some(if approval || contains_any(question, PERMISSION_MARKERS) {
            PromptKind::Permission
        } else {
            PromptKind::Choice {
                options: menu.options,
            }
        });
    }

    let mut tail: Vec<String> = lower
        .iter()
        .rev()
        .filter(|l| !is_blank(l))
        .take(TAIL_ROWS)
        .cloned()
        .collect();
    tail.reverse();
    if contains_any(&tail, PERMISSION_MARKERS) {
        return Some(PromptKind::Permission);
    }
    if contains_any(&tail, CHOICE_HINTS) {
        let options = rows
            .iter()
            .filter_map(|l| parse_option(l))
            .map(|o| o.label.to_string())
            .collect();
        return Some(PromptKind::Choice { options });
    }
    let last_two = &tail[tail.len().saturating_sub(2)..];
    if last_two.iter().any(|l| ends_with_confirm(l)) || contains_any(&tail, CONFIRM_HINTS) {
        return Some(PromptKind::Confirm);
    }
    if tail.last().is_some_and(|l| is_freeform(l)) {
        return Some(PromptKind::FreeformQuestion);
    }
    None
}

#[cfg(test)]
mod tests;

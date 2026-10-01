//! Display labels for graph nodes.

use crate::orca::{Task, Terminal, Worker};

/// Labels longer than this are cut and end in `…`.
pub const MAX_LABEL_CHARS: usize = 40;

/// Drops leading status glyphs Claude and Codex put in terminal titles (`✳ `, `⠂ `, `· `).
pub fn strip_spinner(title: &str) -> &str {
    title
        .trim_start_matches(|c: char| {
            c.is_whitespace() || !(c.is_alphanumeric() || c.is_ascii_punctuation())
        })
        .trim_end()
}

/// First line of `text`, cut to [`MAX_LABEL_CHARS`] characters.
pub fn truncate_label(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= MAX_LABEL_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(MAX_LABEL_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// Title without spinner, else branch name, else handle.
pub fn terminal_label(terminal: &Terminal) -> String {
    let title = terminal.title.as_deref().map(strip_spinner);
    let text = title
        .filter(|t| !t.is_empty())
        .or(terminal.branch_name().filter(|b| !b.is_empty()))
        .unwrap_or(&terminal.handle);
    truncate_label(text)
}

/// The task's display name or title, else the worktree's chosen name, else the terminal's
/// label, else the dispatch id.
pub fn worker_label(
    task: Option<&Task>,
    worktree_name: Option<&str>,
    terminal: Option<&Terminal>,
    worker: &Worker,
) -> String {
    let task_text = task.and_then(|t| {
        t.display_name
            .as_deref()
            .or(t.task_title.as_deref())
            .filter(|s| !s.trim().is_empty())
    });
    match (task_text.or(worktree_name), terminal) {
        (Some(text), _) => truncate_label(text),
        (None, Some(t)) => terminal_label(t),
        (None, None) => truncate_label(worker.task_id.as_deref().unwrap_or(&worker.dispatch_id)),
    }
}

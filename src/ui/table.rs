//! Plain table of terminals.

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::fleet::FleetData;
use crate::orca::Terminal;

/// Longest preview text kept before handing it to the (visually truncating) label.
const PREVIEW_MAX_CHARS: usize = 500;

/// Draws every terminal in `data` as one row, newest output first.
pub fn terminal_table(ui: &mut egui::Ui, data: &FleetData, now_ms: i64) {
    let rows = sorted_terminals(&data.terminals);
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;

    TableBuilder::new(ui)
        .id_salt("terminal_table")
        .striped(true)
        .resizable(true)
        .auto_shrink(false)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::initial(220.0).at_least(60.0).clip(true))
        .column(Column::initial(140.0).at_least(40.0).clip(true))
        .column(Column::initial(160.0).at_least(40.0).clip(true))
        .column(Column::initial(60.0).at_least(40.0))
        .column(Column::initial(80.0).at_least(40.0))
        .column(Column::remainder().at_least(80.0).clip(true))
        .header(row_height + 4.0, |mut header| {
            for name in [
                "Title",
                "Worktree",
                "Branch",
                "Agent",
                "Last output",
                "Preview",
            ] {
                header.col(|ui| {
                    ui.strong(name);
                });
            }
        })
        .body(|body| {
            body.rows(row_height, rows.len(), |mut row| {
                let terminal = rows[row.index()];
                row.col(|ui| {
                    let title = terminal.title.as_deref().map(clean_title).unwrap_or("");
                    let title = if title.is_empty() {
                        terminal.handle.as_str()
                    } else {
                        title
                    };
                    ui.add(egui::Label::new(title).truncate())
                        .on_hover_text(&terminal.handle);
                });
                row.col(|ui| match terminal.worktree_path.as_deref() {
                    Some(path) => {
                        ui.add(egui::Label::new(last_path_component(path)).truncate())
                            .on_hover_text(path);
                    }
                    None => {
                        ui.weak("—");
                    }
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(terminal.branch_name().unwrap_or("—")).truncate());
                });
                row.col(|ui| {
                    ui.label(
                        terminal
                            .agent_identity
                            .as_ref()
                            .map_or("—", |agent| agent.as_str()),
                    );
                });
                row.col(|ui| {
                    let last = terminal.last_output_at.as_ref().and_then(|t| t.epoch_ms());
                    match last {
                        Some(ms) => {
                            ui.label(format_age(now_ms - ms));
                        }
                        None => {
                            ui.weak("—");
                        }
                    }
                });
                row.col(|ui| {
                    let preview = terminal.preview.as_deref().unwrap_or("");
                    ui.add(egui::Label::new(single_line(preview, PREVIEW_MAX_CHARS)).truncate())
                        .on_hover_text(preview);
                });
            });
        });
}

/// Compact age: `"12s"`, `"5m"`, `"3h"`, `"2d"`. Negative ages (clock skew) read as `"0s"`.
pub fn format_age(age_ms: i64) -> String {
    let secs = age_ms.max(0) / 1000;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 60 * 60 => format!("{}m", s / 60),
        s if s < 24 * 60 * 60 => format!("{}h", s / (60 * 60)),
        s => format!("{}d", s / (24 * 60 * 60)),
    }
}

/// Title without leading spinner/status glyphs (`"✳ "`, braille spinner frames, `"·"`, …).
pub fn clean_title(title: &str) -> &str {
    title.trim_start_matches(is_spinner_glyph).trim()
}

fn is_spinner_glyph(c: char) -> bool {
    c.is_whitespace()
        || ('\u{2800}'..='\u{28FF}').contains(&c)
        || matches!(
            c,
            '✳' | '✶' | '✻' | '✽' | '✢' | '·' | '*' | '●' | '○' | '◐' | '◓' | '◑' | '◒'
        )
}

/// Final component of a path, ignoring trailing slashes.
pub fn last_path_component(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(last) if !last.is_empty() => last,
        _ => path,
    }
}

/// Collapses all whitespace runs (including newlines) to single spaces, capped at `max_chars`.
pub fn single_line(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, word) in text.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() > max_chars {
            break;
        }
    }
    match out.char_indices().nth(max_chars) {
        Some((cut, _)) => {
            out.truncate(cut);
            out.push('…');
            out
        }
        None => out,
    }
}

/// Terminals ordered by last output, newest first; terminals without output go last.
/// Ties break on handle so the order is stable between polls.
pub fn sorted_terminals(terminals: &[Terminal]) -> Vec<&Terminal> {
    let mut rows: Vec<&Terminal> = terminals.iter().collect();
    rows.sort_by(|a, b| {
        let a_ms = a.last_output_at.as_ref().and_then(|t| t.epoch_ms());
        let b_ms = b.last_output_at.as_ref().and_then(|t| t.epoch_ms());
        b_ms.cmp(&a_ms).then_with(|| a.handle.cmp(&b.handle))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::Timestamp;

    #[test]
    fn format_age_units() {
        assert_eq!(format_age(0), "0s");
        assert_eq!(format_age(12_999), "12s");
        assert_eq!(format_age(59_999), "59s");
        assert_eq!(format_age(60_000), "1m");
        assert_eq!(format_age(5 * 60_000 + 30_000), "5m");
        assert_eq!(format_age(3 * 3_600_000 + 1), "3h");
        assert_eq!(format_age(2 * 86_400_000), "2d");
    }

    #[test]
    fn format_age_clamps_negative() {
        assert_eq!(format_age(-5_000), "0s");
    }

    #[test]
    fn clean_title_strips_spinners() {
        assert_eq!(clean_title("✳ Fix the bug"), "Fix the bug");
        assert_eq!(clean_title("⠋ Building"), "Building");
        assert_eq!(clean_title("⠙⠹  Running tests "), "Running tests");
        assert_eq!(clean_title("· thinking"), "thinking");
        assert_eq!(clean_title("plain title"), "plain title");
        assert_eq!(clean_title("✳ "), "");
        // Glyphs inside the title stay.
        assert_eq!(clean_title("a ✳ b"), "a ✳ b");
    }

    #[test]
    fn last_path_component_cases() {
        assert_eq!(last_path_component("/a/b/myr-zdd.5"), "myr-zdd.5");
        assert_eq!(last_path_component("/a/b/c/"), "c");
        assert_eq!(last_path_component("repo"), "repo");
        assert_eq!(last_path_component("/"), "/");
    }

    #[test]
    fn single_line_collapses_and_caps() {
        assert_eq!(single_line("a\n  b\tc\n", 100), "a b c");
        assert_eq!(single_line("abcdef", 3), "abc…");
        assert_eq!(single_line("åäö åäö", 5), "åäö å…");
        assert_eq!(single_line("", 10), "");
    }

    fn terminal(handle: &str, last: Option<Timestamp>) -> Terminal {
        Terminal {
            handle: handle.into(),
            last_output_at: last,
            ..Terminal::default()
        }
    }

    #[test]
    fn sorted_newest_first_missing_last() {
        let terminals = vec![
            terminal("old", Some(Timestamp::Millis(1_000))),
            terminal("none", None),
            terminal("new", Some(Timestamp::Millis(3_000))),
            terminal("mid", Some(Timestamp::Millis(2_000))),
            terminal("b-tie", Some(Timestamp::Millis(2_000))),
        ];
        let order: Vec<&str> = sorted_terminals(&terminals)
            .iter()
            .map(|t| t.handle.as_str())
            .collect();
        assert_eq!(order, ["new", "b-tie", "mid", "old", "none"]);
    }
}

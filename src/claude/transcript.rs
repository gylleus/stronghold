//! Renders Claude transcript jsonl records into short readable lines, and one-line summaries
//! of tool calls.

use serde_json::Value;

/// Lines longer than this are cut and end in `…`.
pub const MAX_LINE_CHARS: usize = 240;
/// Tool input summaries and tool result lines are cut to this.
const MAX_SUMMARY_CHARS: usize = 100;

/// Input fields that best describe a tool call, most telling first.
const SUMMARY_KEYS: &[&str] = &[
    "command",
    "file_path",
    "notebook_path",
    "path",
    "pattern",
    "url",
    "query",
    "skill",
    "description",
    "prompt",
    "subject",
    "message",
];

/// Readable lines of the jsonl `text`: assistant text, `→ Tool(summary)` for tool calls,
/// `  ← first line` for tool results, `> text` for user prompts. Thinking, meta records,
/// attachments and unparsable lines are skipped.
pub fn render_transcript(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Ok(record) = serde_json::from_str::<Value>(line) {
            render_record(&record, &mut out);
        }
    }
    out
}

fn render_record(record: &Value, out: &mut Vec<String>) {
    if record.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return;
    }
    let Some(content) = record.pointer("/message/content") else {
        return;
    };
    match record.get("type").and_then(Value::as_str) {
        Some("assistant") => render_assistant(content, out),
        Some("user") => render_user(content, out),
        _ => {}
    }
}

fn render_assistant(content: &Value, out: &mut Vec<String>) {
    let Some(blocks) = content.as_array() else {
        if let Some(text) = content.as_str() {
            push_text(text, "", out);
        }
        return;
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    push_text(text, "", out);
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                let summary = block.get("input").map(tool_summary).unwrap_or_default();
                out.push(cut(&format!("→ {name}({summary})"), MAX_LINE_CHARS));
            }
            _ => {}
        }
    }
}

fn render_user(content: &Value, out: &mut Vec<String>) {
    if let Some(text) = content.as_str() {
        if !is_injected(text) {
            push_text(text, "> ", out);
        }
        return;
    }
    let Some(blocks) = content.as_array() else {
        return;
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str)
                    && !is_injected(text)
                {
                    push_text(text, "> ", out);
                }
            }
            Some("tool_result") => {
                let first = block
                    .get("content")
                    .and_then(first_result_line)
                    .unwrap_or_default();
                let error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                let prefix = if error { "  ← error: " } else { "  ← " };
                out.push(format!("{prefix}{}", cut(&first, MAX_SUMMARY_CHARS)));
            }
            _ => {}
        }
    }
}

/// Harness-injected user text (system reminders, notifications) rather than a prompt.
fn is_injected(text: &str) -> bool {
    text.trim_start().starts_with("<system-reminder>")
}

fn push_text(text: &str, prefix: &str, out: &mut Vec<String>) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    out.extend(
        text.lines()
            .map(|l| cut(&format!("{prefix}{}", l.trim_end()), MAX_LINE_CHARS)),
    );
}

/// A short one-line description of a tool call's input.
pub fn tool_summary(input: &Value) -> String {
    let Some(map) = input.as_object() else {
        return String::new();
    };
    let text = SUMMARY_KEYS
        .iter()
        .find_map(|k| map.get(*k).and_then(Value::as_str))
        .or_else(|| map.values().find_map(Value::as_str))
        .unwrap_or("");
    cut(first_line(text), MAX_SUMMARY_CHARS)
}

/// First non-empty line of a tool result's content (a string or text blocks).
fn first_result_line(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(first_line(text).to_string());
    }
    content.as_array()?.iter().find_map(|b| {
        let text = b.get("text").and_then(Value::as_str)?;
        let line = first_line(text);
        (!line.is_empty()).then(|| line.to_string())
    })
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max - 1).collect();
    format!("{}…", head.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jsonl(records: &[Value]) -> String {
        records.iter().map(|r| format!("{r}\n")).collect::<String>()
    }

    #[test]
    fn renders_prompt_text_tool_calls_and_results() {
        let text = jsonl(&[
            json!({"type": "user", "message": {"role": "user", "content": "Review the diff\nplease"}}),
            json!({"type": "assistant", "message": {"content": [
                {"type": "thinking", "thinking": "secret plan", "signature": "x"},
                {"type": "text", "text": "Let me look.\n"},
                {"type": "tool_use", "id": "t1", "name": "Bash",
                 "input": {"description": "Show diff", "command": "git diff\n--stat"}},
            ]}}),
            json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "\n src/a.rs | 2 +-\n 1 file"},
            ]}}),
            json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t2", "is_error": true,
                 "content": [{"type": "text", "text": "Exit code 1\nboom"}]},
            ]}}),
            json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "t3", "name": "Read", "input": {"file_path": "/x/y.rs", "limit": 5}},
                {"type": "tool_use", "id": "t4", "name": "Custom", "input": {"n": 1, "thing": "abc"}},
                {"type": "tool_use", "id": "t5", "name": "Empty", "input": {}},
            ]}}),
        ]);
        assert_eq!(
            render_transcript(&text),
            vec![
                "> Review the diff",
                "> please",
                "Let me look.",
                "→ Bash(git diff)",
                "  ← src/a.rs | 2 +-",
                "  ← error: Exit code 1",
                "→ Read(/x/y.rs)",
                "→ Custom(abc)",
                "→ Empty()",
            ]
        );
    }

    #[test]
    fn skips_meta_attachments_reminders_and_garbage() {
        let text = format!(
            "{}{}not json\n{}{}",
            jsonl(&[json!({"type": "user", "isMeta": true, "message": {"content": "meta"}})]),
            jsonl(&[json!({"type": "attachment", "attachment": {"type": "hook_success"}})]),
            jsonl(&[
                json!({"type": "user", "message": {"content": "<system-reminder>\nhi\n</system-reminder>"}})
            ]),
            jsonl(&[json!({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "  "},
                {"type": "text", "text": "done"},
            ]}})]),
        );
        assert_eq!(render_transcript(&text), vec!["done"]);
    }

    #[test]
    fn cuts_long_lines() {
        let long = "x".repeat(500);
        let text = jsonl(&[json!({"type": "assistant", "message": {"content": [
            {"type": "text", "text": long},
            {"type": "tool_use", "name": "Bash", "input": {"command": long}},
        ]}})]);
        let lines = render_transcript(&text);
        assert_eq!(lines[0].chars().count(), MAX_LINE_CHARS);
        assert!(lines[0].ends_with('…'));
        assert_eq!(
            lines[1].chars().count(),
            "→ Bash()".chars().count() + MAX_SUMMARY_CHARS
        );
    }
}

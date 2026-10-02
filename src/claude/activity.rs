//! What a Claude session is doing right now, from its latest tool calls: building (editing
//! files, running a build or tests) or anything else. Read in the same transcript pass as
//! [`super::usage`], so it costs no extra file reads.

use serde::Deserialize;
use serde_json::Value;

use super::subagents::parse_iso_ms;

/// A build stays shown this long after its call, so an edit–read–edit loop doesn't flicker.
pub const BUILD_HOLD_MS: i64 = 10_000;

/// Tools that change files.
const EDIT_TOOLS: &[&str] = &["Edit", "MultiEdit", "Write", "NotebookEdit"];

/// Shell programs that build, test, lint or install.
const BUILD_PROGRAMS: &[&str] = &[
    "cargo",
    "make",
    "just",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bun",
    "go",
    "pytest",
    "tsc",
    "gradle",
    "mvn",
    "docker",
    "xcodebuild",
    "swift",
    "uv",
    "pip",
];

/// The latest tool calls of one transcript.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolActivity {
    latest_ms: Option<i64>,
    latest_build: bool,
    build_ms: Option<i64>,
}

impl ToolActivity {
    pub fn record(&mut self, build: bool, at_ms: i64) {
        self.latest_ms = Some(at_ms);
        self.latest_build = build;
        if build {
            self.build_ms = Some(at_ms);
        }
    }

    /// The latest call builds, or one did within [`BUILD_HOLD_MS`] before it.
    pub fn building(&self) -> bool {
        match (self.latest_ms, self.build_ms) {
            (Some(latest), Some(build)) => self.latest_build || latest - build < BUILD_HOLD_MS,
            _ => false,
        }
    }
}

#[derive(Deserialize)]
struct Record {
    timestamp: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    content: Option<Vec<Block>>,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
    name: Option<String>,
    input: Option<Value>,
}

/// `(builds, at ms)` for each tool call in a transcript line.
pub fn tool_calls(line: &str) -> Vec<(bool, i64)> {
    if !line.contains("\"tool_use\"") {
        return Vec::new();
    }
    let Ok(record) = serde_json::from_str::<Record>(line) else {
        return Vec::new();
    };
    let Some(at_ms) = record.timestamp.as_deref().and_then(parse_iso_ms) else {
        return Vec::new();
    };
    record
        .message
        .and_then(|m| m.content)
        .unwrap_or_default()
        .iter()
        .filter(|b| b.kind.as_deref() == Some("tool_use"))
        .map(|b| {
            (
                builds(b.name.as_deref().unwrap_or(""), b.input.as_ref()),
                at_ms,
            )
        })
        .collect()
}

/// An edit, or a shell command whose pipeline runs a build program.
fn builds(tool: &str, input: Option<&Value>) -> bool {
    if EDIT_TOOLS.contains(&tool) {
        return true;
    }
    if tool != "Bash" {
        return false;
    }
    let command = input
        .and_then(|i| i.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("");
    command
        .split([';', '&', '|', '(', ')', '\n'])
        .filter_map(|segment| {
            // The program: past `FOO=bar` assignments and `time`.
            segment
                .split_whitespace()
                .find(|w| !w.contains('=') && *w != "time")
        })
        .any(|program| BUILD_PROGRAMS.contains(&program))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, input: Value, at: &str) -> String {
        json!({"type": "assistant", "timestamp": at, "message": {"content": [
            {"type": "tool_use", "id": "t", "name": name, "input": input},
        ]}})
        .to_string()
    }

    #[test]
    fn classifies_edits_and_build_commands() {
        let at = "2026-10-02T10:00:00.000Z";
        let builds = |name, input| tool_calls(&call(name, input, at))[0].0;
        assert!(builds("Edit", json!({"file_path": "a.rs"})));
        assert!(builds("Write", json!({"file_path": "a.rs"})));
        assert!(builds("Bash", json!({"command": "cd x && cargo test"})));
        assert!(builds("Bash", json!({"command": "(just install)"})));
        assert!(builds(
            "Bash",
            json!({"command": "RUST_LOG=x time cargo build | tail"})
        ));
        assert!(!builds("Bash", json!({"command": "grep -rn cargo src"})));
        assert!(!builds("Bash", json!({"command": "git status; ls"})));
        assert!(!builds("Read", json!({"file_path": "Cargo.toml"})));
        assert!(!builds("Grep", json!({"pattern": "cargo"})));
    }

    #[test]
    fn ignores_lines_without_tool_calls() {
        assert!(tool_calls("not json").is_empty());
        let result = json!({"type": "user", "timestamp": "2026-10-02T10:00:00Z", "message": {
            "content": [{"type": "tool_result", "tool_use_id": "t", "content": "ok"}]}});
        assert!(tool_calls(&result.to_string()).is_empty());
    }

    #[test]
    fn a_build_holds_through_quick_reads() {
        let mut activity = ToolActivity::default();
        assert!(!activity.building());
        activity.record(false, 0);
        assert!(!activity.building());
        activity.record(true, 1_000);
        assert!(activity.building());
        activity.record(false, 1_000 + BUILD_HOLD_MS - 1);
        assert!(activity.building());
        activity.record(false, 1_000 + BUILD_HOLD_MS);
        assert!(!activity.building());
    }
}

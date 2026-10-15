//! Recent tool activity for the one terminal running Ledger maintenance.

use crate::lorekeeper::Mode;
use crate::orca::parse_timestamp_ms;
use serde_json::Value;

/// How long the librarian depicts a tool call without another observed call.
pub const FRESH_MS: i64 = 12_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Reading,
    Writing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecentActivity {
    pub activity: Activity,
    pub at_ms: i64,
}

impl RecentActivity {
    pub fn current(self, now_ms: i64) -> Option<Activity> {
        (0..FRESH_MS)
            .contains(&now_ms.saturating_sub(self.at_ms))
            .then_some(self.activity)
    }
}

fn activity(name: &str, input: &Value, mode: Mode) -> Option<Activity> {
    let writing = if let Some(tool) = super::claude_tool(name) {
        if super::WRITES.contains(&tool) {
            true
        } else if [
            "search_notes",
            "read_note",
            "list_directory",
            "build_context",
        ]
        .contains(&tool)
        {
            false
        } else {
            return None;
        }
    } else {
        match name {
            "Read" | "Grep" | "Glob" | "mcp__stronghold__lorekeeper_lookup" => false,
            "mcp__stronghold__lorekeeper_report" => input["dry_run"] != true,
            _ => return None,
        }
    };
    Some(if writing && mode == Mode::Live {
        Activity::Writing
    } else {
        Activity::Reading
    })
}

pub(super) fn in_line(line: &str, mode: Mode) -> Option<RecentActivity> {
    if !line.contains("\"tool_use\"") {
        return None;
    }
    let record: Value = serde_json::from_str(line).ok()?;
    if record["type"] != "assistant" {
        return None;
    }
    let at_ms = parse_timestamp_ms(record["timestamp"].as_str()?)?;
    record["message"]["content"]
        .as_array()?
        .iter()
        .filter(|block| block["type"] == "tool_use")
        .filter_map(|block| activity(block["name"].as_str()?, &block["input"], mode))
        .next_back()
        .map(|activity| RecentActivity { activity, at_ms })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_activity_and_dry_run_mapping() {
        for tool in [
            "Read",
            "Grep",
            "Glob",
            "mcp__stronghold__lorekeeper_lookup",
            "mcp__ledger__search_notes",
            "mcp__ledger__read_note",
            "mcp__ledger__list_directory",
            "mcp__ledger__build_context",
        ] {
            assert_eq!(
                activity(tool, &json!({}), Mode::Live),
                Some(Activity::Reading),
                "{tool}"
            );
        }
        for tool in [
            "mcp__ledger__edit_note",
            "mcp__ledger__write_note",
            "mcp__ledger__move_note",
            "mcp__ledger__delete_note",
            "mcp__stronghold__lorekeeper_report",
        ] {
            assert_eq!(
                activity(tool, &json!({}), Mode::Live),
                Some(Activity::Writing),
                "{tool}"
            );
            assert_eq!(
                activity(tool, &json!({}), Mode::DryRun),
                Some(Activity::Reading),
                "{tool}"
            );
        }
        assert_eq!(
            activity(
                "mcp__stronghold__lorekeeper_report",
                &json!({"dry_run": true}),
                Mode::Live
            ),
            Some(Activity::Reading)
        );
        assert_eq!(activity("Bash", &json!({}), Mode::Live), None);
        let recent = RecentActivity {
            activity: Activity::Reading,
            at_ms: 100,
        };
        assert_eq!(recent.current(100 + FRESH_MS - 1), Some(Activity::Reading));
        assert_eq!(recent.current(100 + FRESH_MS), None);
        assert_eq!(recent.current(99), None);
    }
}

//! Which tool calls the chat runs without asking, and how a call is summarised on its card.

use std::collections::HashSet;

use serde_json::Value;

/// Prefix of the tools of the `myrstack` MCP server as Claude Code names them.
pub const MYRSTACK_PREFIX: &str = "mcp__myrstack__";

/// Myrstack MCP tools that only read state or move the dashboard's view; they run without a
/// prompt. Everything else, including every tool that sends to agents, asks.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "fleet_status",
    "list_agents",
    "get_agent",
    "whip_status",
    "alerts",
    "quotas",
    "get_playbook",
    "list_playbook",
    "attendant_brief",
    "kickoff",
    "focus_agent",
    "set_filter",
    "set_view",
    "fit_graph",
    "open_whip_settings",
];

/// Myrstack tools whose effect reaches agents or the whip; the chat's settings mark them
/// `ask` so no allow rule elsewhere can skip the card.
pub const DESTRUCTIVE_TOOLS: &[&str] = &[
    "send_to_agent",
    "press_key",
    "close_agents",
    "broadcast_to_coordinators",
    "set_whip",
];

/// Whether `tool` (as named in a `can_use_tool` request) runs without asking.
pub fn is_auto_approved(tool: &str) -> bool {
    tool.strip_prefix(MYRSTACK_PREFIX)
        .is_some_and(|name| READ_ONLY_TOOLS.contains(&name))
}

/// Per-chat permission memory: the auto-approve list plus tools the user allowed for the rest
/// of the chat.
#[derive(Debug, Default, Clone)]
pub struct PermissionPolicy {
    always: HashSet<String>,
}

impl PermissionPolicy {
    pub fn allows(&self, tool: &str) -> bool {
        is_auto_approved(tool) || self.always.contains(tool)
    }

    pub fn always_allow(&mut self, tool: &str) {
        self.always.insert(tool.to_owned());
    }

    /// Forgets the "always allow" choices (a new chat).
    pub fn reset(&mut self) {
        self.always.clear();
    }
}

/// `--settings` JSON for the chat session: the destructive Myrstack tools always ask.
pub fn session_settings() -> String {
    let ask: Vec<String> = DESTRUCTIVE_TOOLS
        .iter()
        .map(|t| format!("{MYRSTACK_PREFIX}{t}"))
        .collect();
    serde_json::json!({"permissions": {"ask": ask}}).to_string()
}

/// A tool name for display: `mcp__myrstack__fleet_status` → `fleet_status`,
/// `mcp__other__x` → `other: x`.
pub fn short_tool_name(tool: &str) -> String {
    if let Some(name) = tool.strip_prefix(MYRSTACK_PREFIX) {
        return name.to_owned();
    }
    if let Some(rest) = tool.strip_prefix("mcp__")
        && let Some((server, name)) = rest.split_once("__")
    {
        return format!("{server}: {name}");
    }
    tool.to_owned()
}

/// One-line-per-field summary of a tool input: `key: value` with strings unquoted and long
/// values cut to `max_chars`.
pub fn summarize_input(input: &Value, max_chars: usize) -> Vec<(String, String)> {
    let Some(map) = input.as_object() else {
        return if input.is_null() {
            Vec::new()
        } else {
            vec![(String::new(), truncate(&input.to_string(), max_chars))]
        };
    };
    map.iter()
        .map(|(key, value)| {
            let text = match value {
                Value::String(s) => s.clone(),
                Value::Array(items) if items.iter().all(Value::is_string) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                other => other.to_string(),
            };
            (key.clone(), truncate(&text, max_chars))
        })
        .collect()
}

/// `text` cut to `max_chars` characters with an ellipsis.
pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn read_only_myrstack_tools_are_auto_approved() {
        for tool in READ_ONLY_TOOLS {
            assert!(
                is_auto_approved(&format!("mcp__myrstack__{tool}")),
                "{tool}"
            );
        }
        for tool in DESTRUCTIVE_TOOLS {
            assert!(
                !is_auto_approved(&format!("mcp__myrstack__{tool}")),
                "{tool}"
            );
        }
        assert!(!is_auto_approved("fleet_status"));
        assert!(!is_auto_approved("mcp__other__fleet_status"));
        assert!(!is_auto_approved("Bash"));
        assert!(!is_auto_approved("Write"));
    }

    #[test]
    fn always_allow_is_per_policy_and_resets() {
        let mut policy = PermissionPolicy::default();
        assert!(!policy.allows("mcp__myrstack__send_to_agent"));
        policy.always_allow("mcp__myrstack__send_to_agent");
        assert!(policy.allows("mcp__myrstack__send_to_agent"));
        assert!(!policy.allows("Bash"));
        policy.reset();
        assert!(!policy.allows("mcp__myrstack__send_to_agent"));
        assert!(policy.allows("mcp__myrstack__fleet_status"));
    }

    #[test]
    fn settings_mark_destructive_tools_ask() {
        let settings: Value = serde_json::from_str(&session_settings()).unwrap();
        let ask = settings["permissions"]["ask"].as_array().unwrap();
        assert!(ask.contains(&json!("mcp__myrstack__send_to_agent")));
        assert!(ask.contains(&json!("mcp__myrstack__broadcast_to_coordinators")));
        assert_eq!(ask.len(), DESTRUCTIVE_TOOLS.len());
    }

    #[test]
    fn names_and_summaries_read_well() {
        assert_eq!(short_tool_name("mcp__myrstack__get_agent"), "get_agent");
        assert_eq!(
            short_tool_name("mcp__linear__get_issue"),
            "linear: get_issue"
        );
        assert_eq!(short_tool_name("Write"), "Write");
        let input =
            json!({"agent": "term:term_1", "text": "x".repeat(50), "keys": ["a", "b"], "n": 2});
        let summary = summarize_input(&input, 10);
        assert_eq!(
            summary,
            vec![
                ("agent".into(), "term:term…".into()),
                ("text".into(), "xxxxxxxxx…".into()),
                ("keys".into(), "a, b".into()),
                ("n".into(), "2".into()),
            ]
        );
        assert!(summarize_input(&Value::Null, 10).is_empty());
    }
}

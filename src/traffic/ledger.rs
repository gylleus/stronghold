//! Ledger traffic: a search or read flies from the Ledger's chamber to the agent, a write or edit
//! flies from the agent to the chamber. Read from what the app already watches: the Claude
//! transcripts and Codex rollouts mapped to terminals ([`LedgerWatch`]: appended lines only,
//! never history) and the King's chat ([`message`]).

pub mod lorekeeper;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use serde_json::Value;

use super::{AgentMessage, FRESH_MS, MessageKind};
use crate::claude::forwards::read_appended;
use crate::orca::parse_timestamp_ms;

/// The Ledger's MCP server name, which is also its traffic endpoint (see [`super::resolve`]).
pub const SERVER: &str = "ledger";
/// How Claude Code names the Ledger's tools: `mcp__ledger__search_notes`.
const CLAUDE_PREFIX: &str = "mcp__ledger__";
/// Ledger tools that change entries; every other tool reads.
const WRITES: [&str; 4] = ["write_note", "edit_note", "move_note", "delete_note"];

/// The flight for `agent` (as [`super::resolve`] knows it) calling the Ledger's `tool`.
pub fn message(agent: &str, tool: &str) -> AgentMessage {
    if WRITES.contains(&tool) {
        AgentMessage::new(agent, SERVER, MessageKind::Ledger)
    } else {
        AgentMessage::new(SERVER, agent, MessageKind::Ledger)
    }
}

/// The Ledger tool a Claude Code tool name calls, if it is one.
pub fn claude_tool(name: &str) -> Option<&str> {
    name.strip_prefix(CLAUDE_PREFIX)
}

/// One Ledger call found in a transcript line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The tool call's id, so a record written twice is one flight.
    pub id: String,
    pub tool: String,
    pub at_ms: Option<i64>,
}

/// The Ledger calls in one line of a Claude transcript (an assistant record's `tool_use` blocks)
/// or a Codex rollout (an `McpToolCall` item completing).
pub fn calls_in_line(line: &str) -> Vec<Call> {
    let claude = line.contains("\"tool_use\"") && line.contains(CLAUDE_PREFIX);
    let codex = line.contains("\"McpToolCall\"");
    if !claude && !codex {
        return Vec::new();
    }
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let at_ms = record["timestamp"].as_str().and_then(parse_timestamp_ms);
    let text = |v: &Value, key: &str| v[key].as_str().map(str::to_owned);
    if record["type"] == "assistant" {
        return record["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|block| block["type"] == "tool_use")
            .filter_map(|block| {
                Some(Call {
                    id: text(block, "id")?,
                    tool: claude_tool(block["name"].as_str()?)?.to_owned(),
                    at_ms,
                })
            })
            .collect();
    }
    let item = &record["payload"]["item"];
    if record["payload"]["type"] == "item_completed"
        && item["type"] == "McpToolCall"
        && item["server"] == SERVER
        && let (Some(id), Some(tool)) = (text(item, "id"), text(item, "tool"))
    {
        return vec![Call { id, tool, at_ms }];
    }
    Vec::new()
}

/// Turns the Ledger calls agents make from now on into traffic.
#[derive(Debug, Default)]
pub struct LedgerWatch {
    /// Byte offset up to which complete lines were read, per transcript.
    offsets: HashMap<PathBuf, u64>,
    lorekeeper: Option<(String, crate::lorekeeper::Mode)>,
    activity: Option<lorekeeper::RecentActivity>,
    /// Calls already emitted (tool call id, time), kept while fresh.
    seen: HashSet<(String, i64)>,
}

impl LedgerWatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn watch_lorekeeper(&mut self, target: Option<(String, crate::lorekeeper::Mode)>) {
        if self.lorekeeper != target {
            self.lorekeeper = target;
            self.activity = None;
        }
    }

    pub fn lorekeeper_activity(&self, now_ms: i64) -> Option<(String, lorekeeper::RecentActivity)> {
        let (handle, _) = self.lorekeeper.as_ref()?;
        let activity = self.activity.filter(|a| a.current(now_ms).is_some())?;
        Some((handle.clone(), activity))
    }

    /// The Ledger calls appended to `transcripts` (agent handle → Claude transcript or Codex
    /// rollout) since the last call, made within [`FRESH_MS`] of `now_ms`. Transcripts no longer
    /// asked for are forgotten.
    pub fn refresh(
        &mut self,
        transcripts: &BTreeMap<String, PathBuf>,
        now_ms: i64,
    ) -> Vec<AgentMessage> {
        self.offsets
            .retain(|path, _| transcripts.values().any(|p| p == path));
        self.seen.retain(|(_, at)| now_ms - at < FRESH_MS);
        if self
            .lorekeeper
            .as_ref()
            .is_some_and(|(h, _)| !transcripts.contains_key(h))
        {
            self.activity = None;
        }
        let mut out = Vec::new();
        for (handle, path) in transcripts {
            let lines = match read_appended(path, self.offsets.get(path).copied()) {
                Ok((lines, offset)) => {
                    self.offsets.insert(path.clone(), offset);
                    lines
                }
                Err(e) => {
                    log::debug!("cannot scan {} for ledger calls: {e}", path.display());
                    continue;
                }
            };
            for line in lines.split(|&b| b == b'\n') {
                let Ok(line) = std::str::from_utf8(line) else {
                    continue;
                };
                if let Some((target, mode)) = &self.lorekeeper
                    && target == handle
                    && let Some(activity) = lorekeeper::in_line(line, *mode)
                    && activity.current(now_ms).is_some()
                    && self.activity.is_none_or(|a| activity.at_ms >= a.at_ms)
                {
                    self.activity = Some(activity);
                }
                for call in calls_in_line(line) {
                    if let Some(at) = call.at_ms
                        && (now_ms - at >= FRESH_MS || !self.seen.insert((call.id, at)))
                    {
                        continue;
                    }
                    out.push(message(handle, &call.tool));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;

    use super::*;
    use crate::traffic::Endpoint;

    const AT: &str = "2026-10-14T07:40:46.384Z";

    fn at_ms() -> i64 {
        parse_timestamp_ms(AT).unwrap()
    }

    fn claude(id: &str, name: &str) -> String {
        json!({
            "type": "assistant",
            "timestamp": AT,
            "message": {"content": [
                {"type": "text", "text": "Checking the Ledger."},
                {"type": "tool_use", "id": id, "name": name, "input": {"query": "export"}},
            ]},
        })
        .to_string()
    }

    fn codex(id: &str, server: &str, tool: &str) -> String {
        json!({
            "timestamp": AT,
            "type": "event_msg",
            "payload": {"type": "item_completed", "item": {
                "type": "McpToolCall", "id": id, "server": server, "tool": tool,
                "arguments": {"query": "export"}, "status": "completed",
            }},
        })
        .to_string()
    }

    #[test]
    fn reads_fly_from_the_ledger_and_writes_fly_to_it() {
        for tool in ["search_notes", "read_note", "build_context"] {
            let m = message("term_a", tool);
            assert_eq!(
                (m.from.as_str(), m.to.as_str()),
                ("ledger", "term_a"),
                "{tool}"
            );
            assert_eq!(m.kind, MessageKind::Ledger);
        }
        for tool in ["write_note", "edit_note"] {
            let m = message("term_a", tool);
            assert_eq!(
                (m.from.as_str(), m.to.as_str()),
                ("term_a", "ledger"),
                "{tool}"
            );
        }
        assert_eq!(MessageKind::parse("ledger"), MessageKind::Ledger);
        assert_eq!(MessageKind::Ledger.name(), "ledger");
    }

    #[test]
    fn finds_claude_tool_uses_and_codex_mcp_calls_of_the_ledger_only() {
        let call = |id: &str, tool: &str| Call {
            id: id.into(),
            tool: tool.into(),
            at_ms: Some(at_ms()),
        };
        assert_eq!(
            calls_in_line(&claude("toolu_1", "mcp__ledger__search_notes")),
            vec![call("toolu_1", "search_notes")]
        );
        assert_eq!(
            calls_in_line(&codex("exec-1", "ledger", "write_note")),
            vec![call("exec-1", "write_note")]
        );
        // Other servers, plain tools, and the deferred-tool list that names the tools.
        assert!(calls_in_line(&claude("toolu_2", "mcp__stronghold__clan_status")).is_empty());
        assert!(calls_in_line(&claude("toolu_3", "Bash")).is_empty());
        assert!(calls_in_line(&codex("exec-2", "linear", "list_issues")).is_empty());
        let listing = json!({"type": "attachment", "timestamp": AT,
            "attachment": {"names": ["mcp__ledger__search_notes"]}});
        assert!(calls_in_line(&listing.to_string()).is_empty());
        assert!(calls_in_line("not json mcp__ledger__ \"tool_use\"").is_empty());
    }

    #[test]
    fn watch_emits_new_calls_once_and_never_replays_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let append = |line: &str| {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "{line}").unwrap();
        };
        append(&claude("toolu_old", "mcp__ledger__search_notes"));
        let transcripts = BTreeMap::from([("term_a".to_owned(), path.clone())]);
        let mut watch = LedgerWatch::new();
        let now = at_ms() + 1_000;
        assert!(watch.refresh(&transcripts, now).is_empty(), "history");

        append(&claude("toolu_1", "mcp__ledger__search_notes"));
        append(&claude("toolu_1", "mcp__ledger__search_notes"));
        append(&codex("exec-1", "ledger", "edit_note"));
        let got = watch.refresh(&transcripts, now);
        assert_eq!(
            got,
            vec![
                message("term_a", "search_notes"),
                message("term_a", "edit_note")
            ]
        );
        assert!(watch.refresh(&transcripts, now).is_empty());

        append(&claude("toolu_2", "mcp__ledger__read_note"));
        assert!(
            watch.refresh(&transcripts, at_ms() + FRESH_MS).is_empty(),
            "stale"
        );
    }

    #[test]
    fn lorekeeper_watch_is_scoped_fresh_and_uses_the_launch_mode() {
        use crate::lorekeeper::Mode;
        use lorekeeper::Activity;
        let dir = tempfile::tempdir().unwrap();
        let reader = dir.path().join("reader.jsonl");
        let other = dir.path().join("other.jsonl");
        let append = |path: &std::path::Path, line: &str| {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(file, "{line}").unwrap();
        };
        append(&reader, &claude("old", "mcp__ledger__write_note"));
        append(&other, &claude("old", "Read"));
        let transcripts = BTreeMap::from([
            ("reader".into(), reader.clone()),
            ("other".into(), other.clone()),
        ]);
        let mut watch = LedgerWatch::new();
        watch.watch_lorekeeper(Some(("reader".into(), Mode::Live)));
        let now = at_ms() + 1_000;
        watch.refresh(&transcripts, now);
        assert!(watch.lorekeeper_activity(now).is_none(), "no history");
        append(&other, &claude("other-write", "mcp__ledger__write_note"));
        watch.refresh(&transcripts, now);
        assert!(watch.lorekeeper_activity(now).is_none(), "other terminal");
        for (tool, expected) in [
            ("Read", Activity::Reading),
            ("mcp__stronghold__lorekeeper_report", Activity::Writing),
        ] {
            append(&reader, &claude(tool, tool));
            assert!(
                watch.refresh(&transcripts, now).is_empty(),
                "local tools emit no Ledger traffic"
            );
            assert_eq!(watch.lorekeeper_activity(now).unwrap().1.activity, expected);
        }
        assert!(
            watch
                .lorekeeper_activity(at_ms() + lorekeeper::FRESH_MS)
                .is_none()
        );
        watch.watch_lorekeeper(Some(("reader".into(), Mode::DryRun)));
        assert!(
            watch.lorekeeper_activity(now).is_none(),
            "mode change clears activity"
        );
        append(
            &reader,
            &claude("report-dry", "mcp__stronghold__lorekeeper_report"),
        );
        watch.refresh(&transcripts, now);
        assert_eq!(
            watch.lorekeeper_activity(now).unwrap().1.activity,
            Activity::Reading
        );
        watch.refresh(&BTreeMap::new(), now);
        assert!(watch.lorekeeper_activity(now).is_none(), "closed terminal");
        watch.watch_lorekeeper(Some(("other".into(), Mode::Live)));
        assert!(watch.lorekeeper_activity(now).is_none(), "new run");
    }

    #[test]
    fn the_ledger_resolves_to_its_own_endpoint() {
        let snapshot = crate::model::ClanSnapshot::default();
        assert_eq!(
            crate::traffic::resolve(&snapshot, "ledger"),
            Some(Endpoint::Ledger)
        );
    }
}

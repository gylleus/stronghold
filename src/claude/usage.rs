//! Token usage per Claude session, summed from `message.usage` on assistant records.
//!
//! Claude Code writes one assistant record per content block, each repeating the whole
//! message's usage, so records are deduplicated by `message.id` within a session.
//! Subagent transcripts (`<session dir>/subagents/agent-*.jsonl`) count towards the session
//! and are also kept per agent. Files are tailed from a remembered offset, so after the first
//! read a refresh only parses appended lines. Only tokens are counted, never dollars. The
//! same pass reads the main transcript's tool calls for [`SessionUsage::building`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::activity::{ToolActivity, tool_calls};
use super::sessions::SessionInfo;
use super::subagents::{Tail, session_dir, tail_file};
use super::worker::ClaudeData;
use crate::model::{Node, NodeId, NodeKind};

/// Model name Claude Code gives records it made up itself (errors, interruptions).
const SYNTHETIC_MODEL: &str = "<synthetic>";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TokenCounts {
    /// All four counts; cache reads usually dominate.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }

    fn add(&mut self, other: &TokenCounts) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_creation += other.cache_creation;
    }

    /// "in 12k · out 340k · cache read 11M · cache write 1.1M".
    pub fn breakdown(&self) -> String {
        format!(
            "in {} · out {} · cache read {} · cache write {}",
            compact(self.input),
            compact(self.output),
            compact(self.cache_read),
            compact(self.cache_creation)
        )
    }
}

/// Token counts in total and per model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub tokens: TokenCounts,
    pub by_model: BTreeMap<String, TokenCounts>,
}

impl Usage {
    fn add(&mut self, model: &str, counts: &TokenCounts) {
        self.tokens.add(counts);
        self.by_model
            .entry(model.to_string())
            .or_default()
            .add(counts);
    }

    /// "1.2M tok".
    pub fn label(&self) -> String {
        format_tokens(self.tokens.total())
    }

    /// Breakdown, then one line per model.
    pub fn detail(&self) -> String {
        let mut text = self.tokens.breakdown();
        for (model, counts) in &self.by_model {
            text.push_str(&format!("\n{model}: {}", format_tokens(counts.total())));
        }
        text
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionUsage {
    /// The main transcript plus every subagent transcript.
    pub total: Usage,
    /// Per subagent id.
    pub subagents: BTreeMap<String, Usage>,
    /// The main agent is editing or building (see [`ToolActivity::building`]).
    pub building: bool,
    /// The main transcript's context as of its last assistant message: that turn's input
    /// tokens, cached and fresh. What the next turn re-reads.
    pub context: u64,
}

/// "950 tok", "12k tok", "1.2M tok", "3.4B tok".
pub fn format_tokens(n: u64) -> String {
    format!("{} tok", compact(n))
}

fn compact(n: u64) -> String {
    let scaled = |div: f64, suffix: &str| {
        let v = n as f64 / div;
        if v < 10.0 {
            format!("{v:.1}{suffix}")
        } else {
            format!("{v:.0}{suffix}")
        }
    };
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => scaled(1e3, "k"),
        1_000_000..1_000_000_000 => scaled(1e6, "M"),
        _ => scaled(1e9, "B"),
    }
}

#[derive(Debug, Default)]
struct SessionCache {
    tails: HashMap<PathBuf, Tail>,
    /// `message.id`s already counted.
    seen: HashSet<String>,
    usage: SessionUsage,
    activity: ToolActivity,
}

/// Incremental per-session token counter.
#[derive(Debug, Default)]
pub struct UsageTracker {
    sessions: HashMap<String, SessionCache>,
}

impl UsageTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads lines appended since the last refresh and returns every given session's usage.
    /// Sessions not in `sessions` are forgotten.
    pub fn refresh(&mut self, sessions: &[SessionInfo]) -> BTreeMap<String, SessionUsage> {
        let keep: HashSet<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
        self.sessions.retain(|id, _| keep.contains(id.as_str()));
        sessions
            .iter()
            .map(|session| {
                let cache = self.sessions.entry(session.session_id.clone()).or_default();
                update(cache, session);
                (session.session_id.clone(), cache.usage.clone())
            })
            .collect()
    }
}

fn update(cache: &mut SessionCache, session: &SessionInfo) {
    let mut files: Vec<(PathBuf, Option<String>)> = vec![(session.transcript_path.clone(), None)];
    if let Some(dir) = session_dir(session) {
        files.extend(subagent_transcripts(&dir.join("subagents")));
    }
    for (path, agent) in files {
        let tail = cache.tails.entry(path.clone()).or_default();
        let seen = &mut cache.seen;
        let usage = &mut cache.usage;
        let activity = &mut cache.activity;
        tail_file(&path, tail, |line| {
            if agent.is_none() {
                for (build, at_ms) in tool_calls(line) {
                    activity.record(build, at_ms);
                }
            }
            let Some((id, model, counts)) = parse_usage(line) else {
                return;
            };
            if !seen.insert(id) {
                return;
            }
            usage.total.add(&model, &counts);
            if agent.is_none() {
                usage.context = counts.input + counts.cache_read + counts.cache_creation;
            }
            if let Some(agent) = &agent {
                usage
                    .subagents
                    .entry(agent.clone())
                    .or_default()
                    .add(&model, &counts);
            }
        });
    }
    cache.usage.building = cache.activity.building();
}

/// `(path, agent id)` of every `agent-<id>.jsonl` in `dir`.
fn subagent_transcripts(dir: &Path) -> Vec<(PathBuf, Option<String>)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let id = name
                .to_str()?
                .strip_prefix("agent-")?
                .strip_suffix(".jsonl")?;
            Some((e.path(), Some(id.to_string())))
        })
        .collect();
    files.sort();
    files
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

/// `(message id, model, counts)` of an assistant record with usage.
fn parse_usage(line: &str) -> Option<(String, String, TokenCounts)> {
    if !line.contains("\"usage\"") || !line.contains("\"assistant\"") {
        return None;
    }
    let record: Record = serde_json::from_str(line).ok()?;
    if record.kind.as_deref() != Some("assistant") {
        return None;
    }
    let message = record.message?;
    let usage = message.usage?;
    let model = message.model.unwrap_or_else(|| "unknown".to_string());
    if model == SYNTHETIC_MODEL {
        return None;
    }
    let counts = TokenCounts {
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_read: usage.cache_read_input_tokens,
        cache_creation: usage.cache_creation_input_tokens,
    };
    Some((message.id?, model, counts))
}

impl ClaudeData {
    /// Usage of the session a terminal is mapped to (Claude, else Codex).
    pub fn usage_for_handle(&self, handle: &str) -> Option<&Usage> {
        if let Some(codex) = self.codex.get(handle) {
            return codex.usage.as_ref();
        }
        let session = self.session_by_handle.get(handle)?.session()?;
        Some(&self.usage.get(&session.session_id)?.total)
    }

    /// The context size of the Claude session a terminal is mapped to, as of its last turn.
    pub fn context_for_handle(&self, handle: &str) -> Option<u64> {
        let session = self.session_by_handle.get(handle)?.session()?;
        Some(self.usage.get(&session.session_id)?.context)
    }

    /// A subagent node's own usage, or its terminal's session usage.
    pub fn node_usage(&self, node: &Node) -> Option<&Usage> {
        if node.kind == NodeKind::Subagent {
            let (session, agent) = node.id.subagent_parts()?;
            return self.usage.get(session)?.subagents.get(agent);
        }
        self.usage_for_handle(node.terminal_handle.as_deref()?)
    }

    /// Nodes whose Claude session is editing or building right now.
    pub fn building_nodes<'a>(&self, nodes: impl IntoIterator<Item = &'a Node>) -> HashSet<NodeId> {
        nodes
            .into_iter()
            .filter(|n| n.kind != NodeKind::Subagent)
            .filter(|n| {
                n.terminal_handle
                    .as_deref()
                    .and_then(|h| self.session_by_handle.get(h)?.session())
                    .and_then(|s| self.usage.get(&s.session_id))
                    .is_some_and(|u| u.building)
            })
            .map(|n| n.id.clone())
            .collect()
    }

    /// [`ClaudeData::node_usage`] for every node that has any.
    pub fn node_usages<'a>(
        &self,
        nodes: impl IntoIterator<Item = &'a Node>,
    ) -> HashMap<NodeId, Usage> {
        nodes
            .into_iter()
            .filter_map(|n| Some((n.id.clone(), self.node_usage(n)?.clone())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;
    use crate::claude::SessionMatch;

    const SESSION: &str = "s1";

    fn assistant(id: &str, model: &str, usage: [u64; 4]) -> Value {
        json!({"type": "assistant", "message": {"id": id, "model": model, "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "usage": {"input_tokens": usage[0], "output_tokens": usage[1],
                "cache_read_input_tokens": usage[2], "cache_creation_input_tokens": usage[3],
                "cache_creation": {"ephemeral_1h_input_tokens": usage[3]}}}})
    }

    fn append(path: &Path, records: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for r in records {
            writeln!(f, "{r}").unwrap();
        }
    }

    struct Fake {
        tmp: TempDir,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                tmp: TempDir::new().unwrap(),
            }
        }

        fn main(&self) -> PathBuf {
            self.tmp.path().join(format!("{SESSION}.jsonl"))
        }

        fn agent(&self, id: &str) -> PathBuf {
            self.tmp
                .path()
                .join(SESSION)
                .join("subagents")
                .join(format!("agent-{id}.jsonl"))
        }

        fn session(&self) -> SessionInfo {
            SessionInfo {
                session_id: SESSION.into(),
                transcript_path: self.main(),
                dir: None,
                modified_ms: 0,
                cwd_from_transcript: None,
            }
        }
    }

    #[test]
    fn sums_and_deduplicates_by_message_id() {
        let fake = Fake::new();
        let m1 = assistant("msg_1", "claude-opus-5", [2, 100, 1_000, 50]);
        append(
            &fake.main(),
            &[
                json!({"type": "user", "message": {"role": "user", "content": "go"}}),
                m1.clone(),
                m1,
                assistant("msg_2", "claude-sonnet-5", [1, 10, 500, 0]),
                assistant("msg_3", SYNTHETIC_MODEL, [0, 0, 0, 0]),
            ],
        );
        let mut tracker = UsageTracker::new();
        let usage = &tracker.refresh(&[fake.session()])[SESSION];
        let expected = TokenCounts {
            input: 3,
            output: 110,
            cache_read: 1_500,
            cache_creation: 50,
        };
        assert_eq!(usage.total.tokens, expected);
        assert_eq!(usage.total.by_model.len(), 2);
        assert_eq!(usage.total.by_model["claude-opus-5"].output, 100);
        assert!(usage.subagents.is_empty());
        assert_eq!(
            usage.context, 501,
            "the last turn's input, cached and fresh"
        );
    }

    #[test]
    fn context_follows_the_main_transcript_only() {
        let fake = Fake::new();
        append(
            &fake.main(),
            &[assistant("msg_1", "claude-opus-5", [10, 1, 40_000, 2_000])],
        );
        append(
            &fake.agent("a1"),
            &[assistant("msg_a", "claude-haiku-4-5", [5, 20, 90_000, 0])],
        );
        let mut tracker = UsageTracker::new();
        let session = SessionInfo {
            dir: Some(fake.tmp.path().join(SESSION)),
            ..fake.session()
        };
        assert_eq!(
            tracker.refresh(std::slice::from_ref(&session))[SESSION].context,
            42_010
        );
        append(
            &fake.main(),
            &[assistant("msg_2", "claude-opus-5", [3, 1, 100, 0])],
        );
        assert_eq!(
            tracker.refresh(&[session])[SESSION].context,
            103,
            "a cleared context shows in the next turn"
        );
    }

    fn tool(name: &str, at: &str) -> Value {
        json!({"type": "assistant", "timestamp": at, "message": {"id": at, "model": "m",
            "content": [{"type": "tool_use", "id": "t", "name": name, "input": {}}],
            "usage": {"input_tokens": 1}}})
    }

    #[test]
    fn building_follows_the_main_agents_latest_tool_calls() {
        let fake = Fake::new();
        append(&fake.main(), &[tool("Edit", "2026-10-02T10:00:00Z")]);
        append(&fake.agent("a1"), &[tool("Read", "2026-10-02T10:01:00Z")]);
        let mut tracker = UsageTracker::new();
        let session = SessionInfo {
            dir: Some(fake.tmp.path().join(SESSION)),
            ..fake.session()
        };
        assert!(tracker.refresh(std::slice::from_ref(&session))[SESSION].building);
        append(&fake.main(), &[tool("Read", "2026-10-02T10:00:30Z")]);
        assert!(!tracker.refresh(&[session])[SESSION].building);
    }

    #[test]
    fn includes_subagents_and_reads_appends_incrementally() {
        let fake = Fake::new();
        append(
            &fake.main(),
            &[assistant("msg_1", "claude-opus-5", [1, 10, 0, 0])],
        );
        append(
            &fake.agent("a1"),
            &[assistant("msg_a", "claude-haiku-4-5", [5, 20, 0, 0])],
        );
        fs::write(fake.agent("a1").with_extension("meta.json"), "{}").unwrap();
        let mut tracker = UsageTracker::new();
        let usage = tracker.refresh(&[fake.session()]).remove(SESSION).unwrap();
        assert_eq!(usage.total.tokens.total(), 36);
        assert_eq!(usage.subagents.len(), 1);
        assert_eq!(usage.subagents["a1"].tokens.output, 20);

        append(
            &fake.agent("a1"),
            &[assistant("msg_b", "claude-haiku-4-5", [0, 7, 0, 0])],
        );
        // A partial line is left for the next refresh.
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(fake.main())
            .unwrap();
        write!(f, "{{\"type\":\"assistant\"").unwrap();
        drop(f);
        let usage = tracker.refresh(&[fake.session()]).remove(SESSION).unwrap();
        assert_eq!(usage.subagents["a1"].tokens.output, 27);
        assert_eq!(usage.total.tokens.output, 37);
        assert_eq!(usage.total.by_model["claude-haiku-4-5"].output, 27);

        assert!(tracker.refresh(&[]).is_empty());
        assert!(tracker.sessions.is_empty(), "forgotten");
    }

    #[test]
    fn formats_compact_counts() {
        assert_eq!(format_tokens(950), "950 tok");
        assert_eq!(format_tokens(1_234), "1.2k tok");
        assert_eq!(format_tokens(45_600), "46k tok");
        assert_eq!(format_tokens(1_234_567), "1.2M tok");
        assert_eq!(format_tokens(3_400_000_000), "3.4B tok");
        let counts = TokenCounts {
            input: 12,
            output: 3_400,
            cache_read: 11_000_000,
            cache_creation: 0,
        };
        assert_eq!(
            counts.breakdown(),
            "in 12 · out 3.4k · cache read 11M · cache write 0"
        );
    }

    #[test]
    fn node_usage_picks_session_or_subagent() {
        let fake = Fake::new();
        append(
            &fake.main(),
            &[assistant("msg_1", "claude-opus-5", [1, 10, 0, 0])],
        );
        append(
            &fake.agent("a1"),
            &[assistant("msg_a", "claude-opus-5", [0, 5, 0, 0])],
        );
        let usage = UsageTracker::new().refresh(&[fake.session()]);
        let claude = ClaudeData {
            session_by_handle: [("term_a".to_string(), SessionMatch::Unique(fake.session()))]
                .into_iter()
                .collect(),
            usage,
            ..Default::default()
        };
        let terminal = Node {
            id: NodeId::terminal("term_a"),
            kind: NodeKind::Terminal,
            terminal_handle: Some("term_a".into()),
            ..sample_node()
        };
        let sub = Node {
            id: NodeId::subagent(SESSION, "a1"),
            kind: NodeKind::Subagent,
            ..sample_node()
        };
        let other = Node {
            terminal_handle: Some("term_b".into()),
            ..sample_node()
        };
        assert_eq!(claude.node_usage(&terminal).unwrap().tokens.output, 15);
        assert_eq!(claude.node_usage(&sub).unwrap().tokens.output, 5);
        assert!(claude.node_usage(&other).is_none());
        let all = claude.node_usages([&terminal, &sub, &other]);
        assert_eq!(all.len(), 2);
    }

    fn sample_node() -> Node {
        Node {
            id: NodeId::terminal("x"),
            kind: NodeKind::Terminal,
            label: "x".into(),
            terminal_handle: None,
            live: true,
            run_id: None,
            task_id: None,
            dispatch_id: None,
            agent: None,
            worktree_path: None,
            branch: None,
            last_output_ms: None,
            preview: None,
            state: crate::model::NodeState::Idle,
            worker_state: None,
            dispatch_status: None,
            worker_terminal_state: None,
            released_at_ms: None,
            parked_at_ms: None,
        }
    }
}

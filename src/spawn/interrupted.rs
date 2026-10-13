//! Spawns whose `spawn_session` call was cancelled (or whose client went away) after the session
//! had started: the caller never saw the result, so it may spawn the same work again.
//!
//! The MCP server records them in a small JSON file in the data dir; `fleet_status` and
//! `list_agents` show them until the agent is gone or [`MAX_AGE_MS`] passed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const FILE: &str = "interrupted-spawns.json";
/// How long a record is shown.
pub const MAX_AGE_MS: i64 = 6 * 3600 * 1000;
/// A record whose agent is not in the clan is dropped once it is this old (the poller needs a
/// few seconds to see a new terminal).
pub const GONE_GRACE_MS: i64 = 2 * 60 * 1000;
/// Most records kept.
const MAX_RECORDS: usize = 20;

/// One interrupted spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterruptedSpawn {
    pub at_ms: i64,
    /// `cancelled` (the client cancelled the call) or `disconnected` (its input closed).
    pub reason: String,
    pub mode: String,
    pub repo: Option<String>,
    pub name: Option<String>,
    pub node: Option<String>,
    pub handle: Option<String>,
    pub worktree_path: Option<String>,
    /// First line of the prompt.
    pub prompt: String,
}

impl InterruptedSpawn {
    /// A record from `spawn_session`'s arguments and the app's reply.
    pub fn new(reason: &str, at_ms: i64, args: &Value, result: &Value) -> Self {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        Self {
            at_ms,
            reason: reason.to_owned(),
            mode: s(args, "mode").unwrap_or_default(),
            repo: s(args, "repo"),
            name: s(args, "name"),
            node: s(result, "node"),
            handle: s(result, "handle"),
            worktree_path: s(result, "worktree_path"),
            prompt: super::title_from(&s(args, "prompt").unwrap_or_default()),
        }
    }

    /// One line for `fleet_status`; `state` is the agent's current state when known.
    pub fn line(&self, state: Option<&str>, now_ms: i64) -> String {
        let agent = self
            .node
            .as_deref()
            .or(self.handle.as_deref())
            .unwrap_or("?");
        let place = self
            .worktree_path
            .as_deref()
            .or(self.name.as_deref())
            .unwrap_or("?");
        let state = state.map_or_else(String::new, |s| format!(", now {}", s.replace('_', " ")));
        let ago = (now_ms - self.at_ms).max(0) / 60_000;
        format!(
            "- `{agent}` in {place} ({} {ago} min ago{state}): \"{}\"",
            self.reason, self.prompt
        )
    }

    fn is_agent(&self, id: &str, handle: Option<&str>) -> bool {
        self.node.as_deref() == Some(id)
            || (self.handle.is_some() && self.handle.as_deref() == handle)
    }
}

/// `<data dir>/interrupted-spawns.json`; `None` in unit tests.
pub fn default_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(crate::claude::hook::data_dir().join(FILE))
}

/// The records younger than [`MAX_AGE_MS`]; a missing or unreadable file has none.
pub fn load(path: &Path, now_ms: i64) -> Vec<InterruptedSpawn> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let all: Vec<InterruptedSpawn> = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        log::warn!("{}: {e}", path.display());
        Vec::new()
    });
    all.into_iter()
        .filter(|r| now_ms - r.at_ms < MAX_AGE_MS)
        .collect()
}

fn save(path: &Path, records: &[InterruptedSpawn]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(records)?)?;
    fs::rename(&tmp, path)
}

/// Adds `record`, dropping expired and the oldest beyond [`MAX_RECORDS`].
pub fn record(path: &Path, record: InterruptedSpawn) -> io::Result<()> {
    let mut records = load(path, record.at_ms);
    records.push(record);
    let extra = records.len().saturating_sub(MAX_RECORDS);
    records.drain(..extra);
    save(path, &records)
}

/// The records whose agent is still around, each with its current state, given the clan's
/// agents as `(id, terminal handle, state)`. Records of agents that are gone are dropped from
/// the file once [`GONE_GRACE_MS`] old.
pub fn current(
    path: &Path,
    agents: &[(String, Option<String>, String)],
    now_ms: i64,
) -> Vec<(InterruptedSpawn, Option<String>)> {
    let records = load(path, now_ms);
    let mut kept = Vec::new();
    let mut shown = Vec::new();
    for r in records {
        let agent = agents
            .iter()
            .find(|(id, handle, _)| r.is_agent(id, handle.as_deref()));
        match agent {
            Some((_, _, state)) => shown.push((r.clone(), Some(state.clone()))),
            None if now_ms - r.at_ms < GONE_GRACE_MS => shown.push((r.clone(), None)),
            None => continue,
        }
        kept.push(r);
    }
    if kept.len() != load(path, now_ms).len()
        && let Err(e) = save(path, &kept)
    {
        log::warn!("{}: {e}", path.display());
    }
    shown
}

/// The `fleet_status` / `list_agents` section for `shown`, empty when there is nothing.
pub fn section(shown: &[(InterruptedSpawn, Option<String>)], now_ms: i64) -> String {
    if shown.is_empty() {
        return String::new();
    }
    let mut out = vec![format!(
        "Interrupted spawns ({}): spawn_session was cancelled after the session had started. The \
         agent is running: send changes with send_to_agent, don't spawn again.",
        shown.len()
    )];
    out.extend(
        shown
            .iter()
            .map(|(r, state)| r.line(state.as_deref(), now_ms)),
    );
    out.join("\n")
}

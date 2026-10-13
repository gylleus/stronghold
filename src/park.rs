//! Parking agents: Karl shelves a dwarf that finished its task but that he wants to keep for
//! later. A parked agent sleeps in the resting area, out of the live clan: the whip leaves it
//! alone, it counts in no stalled or needs-input total, and no cleanup closes its terminal or
//! removes its worktree unless it is named explicitly. Its terminal, session and worktree stay
//! as they are, so waking it (unparking, or sending it a message) continues the conversation.
//!
//! Parking is an overlay on a node, like the release fade ([`crate::model::Node::released_at_ms`])
//! and finished sessions ([`crate::finish`]), but the opposite of both: those mean the terminal
//! is gone or going, parking keeps it. Its state stays what Orca reports; parking only says
//! Karl doesn't want it in the live view.
//!
//! The parked set lives in `<data dir>/parked.json`, keyed by node id (`term:<handle>`: only a
//! live terminal can be parked), so it survives restarts and the MCP process (worktree
//! cleanup) reads it too. An entry whose terminal is gone is dropped.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::clan::ClanData;
use crate::model::{Node, NodeId, NodeKind};

/// Where parked agents are shown: the farm region and sidebar group they gather in, in place
/// of their repo's, and the farm's chamber.
pub const RESTING_AREA: &str = "Resting area";

/// The parked set's file in the data dir.
pub const FILE: &str = "parked.json";

/// One parked agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkedAgent {
    pub node_id: String,
    pub handle: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub at_ms: i64,
}

/// Why `node` can't be parked, if it can't.
pub fn refusal(node: &Node) -> Option<&'static str> {
    match node.kind {
        NodeKind::King => Some("the King isn't a session; it stands for the whole clan"),
        NodeKind::Subagent => Some("subagents run inside their parent session; park the parent"),
        NodeKind::Envoy => Some("the Envoy is a standing session with its own room"),
        NodeKind::Coordinator | NodeKind::Attendant | NodeKind::Worker | NodeKind::Terminal => {
            match &node.terminal_handle {
                Some(_) if node.live => None,
                _ => Some("its terminal is gone; there is no session to keep"),
            }
        }
    }
}

/// The parked agents, mirrored to [`FILE`].
#[derive(Debug, Default)]
pub struct Parking {
    path: Option<PathBuf>,
    entries: Vec<ParkedAgent>,
}

impl Parking {
    /// `<data dir>/parked.json`.
    pub fn default_path() -> PathBuf {
        crate::claude::hook::data_dir().join(FILE)
    }

    /// The parked set saved at `path` (empty when missing or unreadable); changes are saved
    /// there.
    pub fn load(path: PathBuf) -> Self {
        let entries = read(&path);
        Self {
            path: Some(path),
            entries,
        }
    }

    pub fn entries(&self) -> &[ParkedAgent] {
        &self.entries
    }

    pub fn get(&self, id: &NodeId) -> Option<&ParkedAgent> {
        self.entries.iter().find(|e| e.node_id == id.as_str())
    }

    pub fn is_parked(&self, id: &NodeId) -> bool {
        self.get(id).is_some()
    }

    /// Parks `node`. Errors with the reason it can't be parked; `Ok(false)` when it already
    /// was.
    pub fn park(&mut self, node: &Node, now_ms: i64) -> Result<bool, &'static str> {
        if let Some(reason) = refusal(node) {
            return Err(reason);
        }
        if self.is_parked(&node.id) {
            return Ok(false);
        }
        self.entries.push(ParkedAgent {
            node_id: node.id.to_string(),
            handle: node.terminal_handle.clone().unwrap_or_default(),
            label: node.label.clone(),
            worktree_path: node.worktree_path.clone(),
            branch: node.branch.clone(),
            at_ms: now_ms,
        });
        self.save();
        Ok(true)
    }

    /// Wakes `id`; returns its entry when it was parked.
    pub fn unpark(&mut self, id: &NodeId) -> Option<ParkedAgent> {
        let i = self.entries.iter().position(|e| e.node_id == id.as_str())?;
        let entry = self.entries.remove(i);
        self.save();
        Some(entry)
    }

    /// Terminal handles of the parked agents.
    pub fn handles(&self) -> BTreeSet<String> {
        self.entries.iter().map(|e| e.handle.clone()).collect()
    }

    /// Drops agents whose terminal is no longer in a clean poll's terminal list, and returns
    /// them. A poll with errors or no terminals at all proves nothing and drops none.
    pub fn prune_gone(&mut self, data: &ClanData) -> Vec<ParkedAgent> {
        if !data.errors.is_empty() || data.terminals.is_empty() {
            return Vec::new();
        }
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|e| !data.terminals.iter().any(|t| t.handle == e.handle));
        self.entries = kept;
        if !gone.is_empty() {
            self.save();
        }
        gone
    }

    fn save(&self) {
        if let Some(path) = &self.path
            && let Err(e) = write(path, &self.entries)
        {
            log::warn!("{}: {e}", path.display());
        }
    }
}

/// Worktree paths of the agents parked in the file at `path`, for the worktree cleanup.
pub fn parked_worktrees(path: &Path) -> BTreeSet<String> {
    read(path)
        .into_iter()
        .filter_map(|e| e.worktree_path)
        .map(|p| p.trim_end_matches('/').to_owned())
        .collect()
}

fn read(path: &Path) -> Vec<ParkedAgent> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write(path: &Path, entries: &[ParkedAgent]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(entries).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests;

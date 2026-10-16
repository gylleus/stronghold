//! Codex sessions: maps Codex terminals to rollout files under `~/.codex/sessions` and reads
//! their token usage. Runs on the Claude worker thread; never on the UI thread.

pub mod history;
pub mod rollouts;
pub mod usage;

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use rollouts::{Rollout, RolloutLocator, RolloutMatch, default_sessions_root};
pub use usage::{CodexSession, CodexUsageTracker, RawTokenUsage};

use crate::orca::Terminal;

/// Maps Codex terminals to rollouts and keeps their usage current.
#[derive(Debug)]
pub struct CodexTracker {
    locator: RolloutLocator,
    usage: CodexUsageTracker,
}

impl CodexTracker {
    pub fn new(sessions_root: impl Into<PathBuf>) -> Self {
        Self {
            locator: RolloutLocator::new(sessions_root),
            usage: CodexUsageTracker::new(),
        }
    }

    /// Codex session per terminal handle, for terminals with a recent rollout.
    pub fn refresh(
        &mut self,
        terminals: &[Terminal],
        now_ms: i64,
    ) -> BTreeMap<String, CodexSession> {
        let matches = self.locator.map_terminals(terminals, now_ms);
        self.usage.refresh(matches)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;
    use tempfile::TempDir;

    use super::rollouts::tests::{WT, codex_terminal, set_mtime, write_rollout};
    use super::*;

    #[test]
    fn tracker_maps_and_reads_usage() {
        let now = 1_790_000_000_000;
        let tmp = TempDir::new().unwrap();
        let path = write_rollout(tmp.path(), "2026/09/24", "s1", WT, json!("cli"), now);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let event = json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": 900, "cached_input_tokens": 600,
                "output_tokens": 100, "total_tokens": 1000}}}});
        writeln!(f, "{event}").unwrap();
        drop(f);
        set_mtime(&path, now);

        let mut tracker = CodexTracker::new(tmp.path());
        let sessions = tracker.refresh(&[codex_terminal("t1", WT, now)], now);
        let s = &sessions["t1"];
        assert_eq!(s.rollout.rollout.session_id, "s1");
        assert_eq!(s.usage.as_ref().unwrap().label(), "1.0k tok");
    }
}

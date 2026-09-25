//! Maps Codex terminals to their rollout files.
//!
//! Codex writes each session to `<sessions_root>/YYYY/MM/DD/rollout-<time>-<id>.jsonl`. The
//! first record is a `session_meta` whose payload has the session `id`, its `cwd` and its
//! `source`. There is no direct link from a terminal to its rollout, so, as for Claude, the
//! mapping is a heuristic: the most recently written interactive rollout whose `cwd` is the
//! terminal's worktree. Subagent rollouts (forks that copy the parent's history, token counts
//! included) and `codex exec` runs are never candidates.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::claude::sessions::RECENT_WINDOW_MS;
use crate::orca::{AgentIdentity, Terminal};

/// A `session_meta` line carries the full base instructions (~20 KiB); longer is malformed.
const MAX_META_BYTES: u64 = 1 << 20;

/// `$CODEX_HOME/sessions`, else `~/.codex/sessions`.
pub fn default_sessions_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("sessions");
    }
    let home = directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_default();
    home.join(".codex").join("sessions")
}

/// One interactive rollout on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rollout {
    pub session_id: String,
    pub path: PathBuf,
    pub cwd: String,
    /// File mtime in ms since the epoch.
    pub modified_ms: i64,
}

/// A Codex terminal's rollout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutMatch {
    pub rollout: Rollout,
    /// Several Codex terminals share the worktree, so this is a best-effort pick by recency.
    pub ambiguous: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Meta {
    session_id: String,
    cwd: String,
    /// False for subagent forks and `codex exec` runs.
    interactive: bool,
}

#[derive(Deserialize)]
struct MetaRecord {
    #[serde(rename = "type")]
    kind: String,
    payload: MetaPayload,
}

#[derive(Deserialize)]
struct MetaPayload {
    id: Option<String>,
    session_id: Option<String>,
    cwd: Option<String>,
    source: Option<serde_json::Value>,
    thread_source: Option<String>,
}

/// Finds rollouts under a sessions root; each file's `session_meta` is read once.
#[derive(Debug)]
pub struct RolloutLocator {
    root: PathBuf,
    /// `None` for files whose first line is not a usable `session_meta`.
    meta_cache: HashMap<PathBuf, Option<Meta>>,
}

impl RolloutLocator {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            meta_cache: HashMap::new(),
        }
    }

    /// Interactive rollouts written at or after `min_modified_ms`, newest first.
    pub fn recent(&mut self, min_modified_ms: i64) -> Vec<Rollout> {
        let mut out = Vec::new();
        let mut present = HashSet::new();
        for path in rollout_files(&self.root) {
            present.insert(path.clone());
            let Some(modified_ms) = fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .map(system_time_ms)
            else {
                continue;
            };
            if modified_ms < min_modified_ms {
                continue;
            }
            let meta = match self.meta_cache.get(&path) {
                Some(meta) => meta.clone(),
                None => {
                    let Some(meta) = read_meta(&path) else {
                        // First line not written yet; try again next time.
                        continue;
                    };
                    self.meta_cache.insert(path.clone(), meta.clone());
                    meta
                }
            };
            if let Some(meta) = meta.filter(|m| m.interactive) {
                out.push(Rollout {
                    session_id: meta.session_id,
                    cwd: meta.cwd,
                    path,
                    modified_ms,
                });
            }
        }
        self.meta_cache.retain(|p, _| present.contains(p));
        out.sort_by(|a, b| {
            b.modified_ms
                .cmp(&a.modified_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        out
    }

    /// Maps Codex terminals (by handle) to rollouts as of `now_ms`.
    ///
    /// Terminals are grouped by worktree: a lone terminal gets the newest recent rollout
    /// there; when several share a worktree, the most recently active terminal gets the
    /// newest rollout, the next the second newest, and so on, all marked ambiguous.
    pub fn map_terminals(
        &mut self,
        terminals: &[Terminal],
        now_ms: i64,
    ) -> BTreeMap<String, RolloutMatch> {
        let mut groups: BTreeMap<&str, Vec<&Terminal>> = BTreeMap::new();
        for t in terminals {
            if t.agent_identity != Some(AgentIdentity::Codex) {
                continue;
            }
            if let Some(cwd) = t.worktree_path.as_deref().filter(|p| !p.is_empty()) {
                groups.entry(cwd).or_default().push(t);
            }
        }
        let mut out = BTreeMap::new();
        if groups.is_empty() {
            return out;
        }
        let rollouts = self.recent(now_ms.saturating_sub(RECENT_WINDOW_MS));
        for (worktree, mut group) in groups {
            let candidates: Vec<&Rollout> = rollouts
                .iter()
                .filter(|r| in_worktree(&r.cwd, worktree))
                .collect();
            group.sort_by(|a, b| {
                let a_ms = a.last_output_at.as_ref().and_then(|t| t.epoch_ms());
                let b_ms = b.last_output_at.as_ref().and_then(|t| t.epoch_ms());
                b_ms.cmp(&a_ms).then_with(|| a.handle.cmp(&b.handle))
            });
            let ambiguous = group.len() > 1;
            for (terminal, rollout) in group.into_iter().zip(candidates) {
                out.insert(
                    terminal.handle.clone(),
                    RolloutMatch {
                        rollout: rollout.clone(),
                        ambiguous,
                    },
                );
            }
        }
        out
    }
}

/// True when `cwd` is `worktree` or inside it.
fn in_worktree(cwd: &str, worktree: &str) -> bool {
    let cwd = cwd.trim_end_matches('/');
    let worktree = worktree.trim_end_matches('/');
    cwd == worktree
        || cwd
            .strip_prefix(worktree)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Every `YYYY/MM/DD/rollout-*.jsonl` under `root`.
fn rollout_files(root: &Path) -> Vec<PathBuf> {
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect()
    };
    let mut files = Vec::new();
    for year in subdirs(root) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                let Ok(entries) = fs::read_dir(&day) else {
                    continue;
                };
                files.extend(entries.flatten().map(|e| e.path()).filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
                }));
            }
        }
    }
    files
}

/// `Some(None)` when the first line is complete but not a usable `session_meta`; `None`
/// when it is not complete yet.
fn read_meta(path: &Path) -> Option<Option<Meta>> {
    let file = File::open(path).ok()?;
    let mut line = Vec::new();
    BufReader::new(file.take(MAX_META_BYTES))
        .read_until(b'\n', &mut line)
        .ok()?;
    if line.last() != Some(&b'\n') {
        return (line.len() as u64 >= MAX_META_BYTES).then_some(None);
    }
    let Ok(record) = serde_json::from_slice::<MetaRecord>(&line) else {
        return Some(None);
    };
    if record.kind != "session_meta" {
        return Some(None);
    }
    let p = record.payload;
    let interactive = p.thread_source.as_deref() != Some("subagent")
        && !p
            .source
            .as_ref()
            .is_some_and(|s| s.get("subagent").is_some() || s.as_str() == Some("exec"));
    let (Some(session_id), Some(cwd)) = (p.id.or(p.session_id), p.cwd) else {
        return Some(None);
    };
    Some(Some(Meta {
        session_id,
        cwd,
        interactive,
    }))
}

fn system_time_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;
    use std::time::Duration;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::orca::Timestamp;

    pub(crate) const WT: &str = "/Users/me/orca/workspaces/app/feature-x";

    /// Writes a rollout under `root` for `day` and sets its mtime to `modified_ms`.
    pub(crate) fn write_rollout(
        root: &Path,
        day: &str,
        id: &str,
        cwd: &str,
        source: serde_json::Value,
        modified_ms: i64,
    ) -> PathBuf {
        let dir = root.join(day);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-09-24T11-26-41-{id}.jsonl"));
        let meta = json!({"timestamp": "2026-09-24T09:28:20.485Z", "type": "session_meta",
            "payload": {"id": id, "session_id": id, "cwd": cwd, "source": source,
                "base_instructions": {"text": "x".repeat(20_000)}}});
        let mut f = File::create(&path).unwrap();
        writeln!(f, "{meta}").unwrap();
        set_mtime(&path, modified_ms);
        path
    }

    pub(crate) fn set_mtime(path: &Path, ms: i64) {
        let f = fs::OpenOptions::new().append(true).open(path).unwrap();
        f.set_modified(UNIX_EPOCH + Duration::from_millis(ms as u64))
            .unwrap();
    }

    pub(crate) fn codex_terminal(handle: &str, worktree: &str, last_output_ms: i64) -> Terminal {
        Terminal {
            handle: handle.into(),
            worktree_path: Some(worktree.into()),
            agent_identity: Some(AgentIdentity::Codex),
            last_output_at: Some(Timestamp::Millis(last_output_ms)),
            connected: true,
            ..Default::default()
        }
    }

    const NOW: i64 = 1_790_000_000_000;
    const MIN: i64 = 60_000;

    #[test]
    fn maps_lone_terminal_to_newest_interactive_rollout() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_rollout(root, "2026/09/20", "old", WT, json!("cli"), NOW - 60 * MIN);
        let newest = write_rollout(root, "2026/09/24", "new", WT, json!("cli"), NOW - MIN);
        // Newer, but not candidates: a subagent fork, an exec run, another worktree, a
        // sibling worktree sharing the prefix, and one outside the recent window.
        let sub = json!({"subagent": {"thread_spawn": {"parent_thread_id": "new"}}});
        write_rollout(root, "2026/09/24", "sub", WT, sub, NOW);
        write_rollout(root, "2026/09/24", "exec", WT, json!("exec"), NOW);
        write_rollout(root, "2026/09/24", "other", "/else", json!("cli"), NOW);
        write_rollout(
            root,
            "2026/09/24",
            "sib",
            &format!("{WT}-2"),
            json!("cli"),
            NOW,
        );
        write_rollout(
            root,
            "2026/09/01",
            "ancient",
            WT,
            json!("cli"),
            NOW - 8 * 1440 * MIN,
        );
        fs::write(root.join("2026/09/24/notes.txt"), "ignored").unwrap();

        let mut locator = RolloutLocator::new(root);
        let terminals = vec![
            codex_terminal("t1", WT, NOW),
            codex_terminal("t2", "/nothing/here", NOW),
            Terminal {
                agent_identity: Some(AgentIdentity::Claude),
                ..codex_terminal("claude", WT, NOW)
            },
        ];
        let map = locator.map_terminals(&terminals, NOW);
        assert_eq!(map.len(), 1);
        let m = &map["t1"];
        assert!(!m.ambiguous);
        assert_eq!(m.rollout.session_id, "new");
        assert_eq!(m.rollout.path, newest);
        assert_eq!(m.rollout.cwd, WT);
    }

    #[test]
    fn subdirectory_cwd_counts_and_shared_worktree_is_ambiguous() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_rollout(
            root,
            "2026/09/24",
            "a",
            &format!("{WT}/sub"),
            json!("cli"),
            NOW - MIN,
        );
        write_rollout(root, "2026/09/24", "b", WT, json!("vscode"), NOW - 2 * MIN);
        let mut locator = RolloutLocator::new(root);
        let terminals = vec![
            codex_terminal("quiet", WT, NOW - 30 * MIN),
            codex_terminal("busy", WT, NOW),
            codex_terminal("third", WT, NOW - 60 * MIN),
        ];
        let map = locator.map_terminals(&terminals, NOW);
        assert_eq!(map["busy"].rollout.session_id, "a");
        assert_eq!(map["quiet"].rollout.session_id, "b");
        assert!(map["busy"].ambiguous);
        assert!(!map.contains_key("third"), "more terminals than rollouts");
    }

    #[test]
    fn incomplete_meta_line_is_retried() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("2026/09/24");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-x.jsonl");
        fs::write(&path, r#"{"type":"session_meta","payload":{"id":"x","#).unwrap();
        let mut locator = RolloutLocator::new(tmp.path());
        assert!(locator.recent(0).is_empty());
        let meta = json!({"type": "session_meta", "payload": {"id": "x", "cwd": WT}});
        fs::write(&path, format!("{meta}\n")).unwrap();
        let found = locator.recent(0);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "x");

        fs::remove_file(&path).unwrap();
        assert!(locator.recent(0).is_empty());
        assert!(locator.meta_cache.is_empty(), "deleted files are forgotten");
    }

    #[test]
    fn non_meta_first_line_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("2026/09/24");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("rollout-y.jsonl"), "{\"type\":\"event_msg\"}\n").unwrap();
        fs::write(dir.join("rollout-z.jsonl"), "not json\n").unwrap();
        let mut locator = RolloutLocator::new(tmp.path());
        assert!(locator.recent(0).is_empty());
        assert_eq!(locator.meta_cache.len(), 2);
    }
}

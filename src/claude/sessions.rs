//! Maps Claude terminals to their session transcripts.
//!
//! Claude Code writes each session to
//! `<projects_root>/<encoded cwd>/<session-id>.jsonl`, with an optional sibling
//! `<session-id>/` directory holding `subagents/` and `tool-results/`. Without
//! a hook there is no direct link from a terminal to its session, so the
//! mapping is a heuristic: the newest transcript in the terminal's worktree.
//! [`HookMappings`] is the seam for an exact mapping recorded by a
//! `SessionStart` hook; when it names a session it always wins.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::orca::{AgentIdentity, Terminal};

/// Sessions not written within this window are never picked heuristically.
pub const RECENT_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Claude Code caps encoded directory names at this length and appends a hash.
const MAX_ENCODED_LEN: usize = 200;

/// How much of a transcript's head is read to find its `cwd`. The first
/// record carrying `cwd` sits within the first ~12 KiB in practice.
const HEAD_BYTES: u64 = 64 * 1024;

/// Encodes a working directory the way Claude Code names its project dirs:
/// every character outside `[A-Za-z0-9]` becomes `-`, one per UTF-16 unit.
///
/// `/Users/me/repo/.claude/wt` becomes `-Users-me-repo--claude-wt`. Paths whose
/// encoding exceeds 200 characters are truncated and suffixed with a hash by
/// Claude Code; [`SessionLocator`] resolves those by prefix.
pub fn encode_project_dir(cwd: &Path) -> String {
    let raw = cwd.to_string_lossy();
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                out.push('-');
            }
        }
    }
    out
}

/// One Claude session transcript on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: String,
    pub transcript_path: PathBuf,
    /// The `<session-id>/` directory (subagents, tool results), if present.
    pub dir: Option<PathBuf>,
    /// Transcript mtime in ms since the epoch.
    pub modified_ms: i64,
    /// `cwd` from the first transcript record that has one.
    pub cwd_from_transcript: Option<String>,
}

/// How a Claude terminal was matched to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMatch {
    /// Exact (hook) match, or the only Claude terminal in its worktree.
    Unique(SessionInfo),
    /// Several Claude terminals share the worktree. `chosen` is the
    /// best-effort pick by recency; `candidates` are all recent sessions there.
    Ambiguous {
        candidates: Vec<SessionInfo>,
        chosen: Option<SessionInfo>,
    },
    /// No recent session found for the terminal's worktree.
    None,
}

impl SessionMatch {
    /// The session this terminal is believed to be running, if any.
    pub fn session(&self) -> Option<&SessionInfo> {
        match self {
            SessionMatch::Unique(s) => Some(s),
            SessionMatch::Ambiguous { chosen, .. } => chosen.as_ref(),
            SessionMatch::None => None,
        }
    }
}

/// Exact terminal → session ids, as recorded by a `SessionStart` hook.
///
/// Orca pane keys are `<tabId>:<leafId>`; `by_tab_id` covers hooks that only
/// know `ORCA_TAB_ID`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookMappings {
    pub by_pane_key: HashMap<String, String>,
    pub by_tab_id: HashMap<String, String>,
}

impl HookMappings {
    pub fn is_empty(&self) -> bool {
        self.by_pane_key.is_empty() && self.by_tab_id.is_empty()
    }

    /// Orca's pane key for a terminal, `<tabId>:<leafId>`.
    pub fn pane_key(terminal: &Terminal) -> Option<String> {
        Some(format!(
            "{}:{}",
            terminal.tab_id.as_deref()?,
            terminal.leaf_id.as_deref()?
        ))
    }

    /// Session id recorded for this terminal, pane key first.
    pub fn session_for(&self, terminal: &Terminal) -> Option<&str> {
        Self::pane_key(terminal)
            .and_then(|key| self.by_pane_key.get(&key))
            .or_else(|| {
                terminal
                    .tab_id
                    .as_deref()
                    .and_then(|tab| self.by_tab_id.get(tab))
            })
            .map(String::as_str)
    }
}

/// Finds Claude session transcripts under a projects root.
///
/// Only stats files and reads the head of transcripts; the `cwd` read from a
/// transcript is cached by (path, mtime).
#[derive(Debug)]
pub struct SessionLocator {
    projects_root: PathBuf,
    cwd_cache: Mutex<HashMap<PathBuf, (i64, Option<String>)>>,
}

impl Default for SessionLocator {
    fn default() -> Self {
        Self::new(default_projects_root())
    }
}

/// `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`.
pub fn default_projects_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("projects");
    }
    let home = directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_default();
    home.join(".claude").join("projects")
}

impl SessionLocator {
    pub fn new(projects_root: impl Into<PathBuf>) -> Self {
        Self {
            projects_root: projects_root.into(),
            cwd_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn projects_root(&self) -> &Path {
        &self.projects_root
    }

    /// The existing project directory for `cwd`, if any.
    pub fn project_dir(&self, cwd: &Path) -> Option<PathBuf> {
        let encoded = encode_project_dir(cwd);
        let exact = self.projects_root.join(&encoded);
        if exact.is_dir() {
            return Some(exact);
        }
        if encoded.len() <= MAX_ENCODED_LEN {
            return None;
        }
        // Long paths are stored as `<first 200 chars>-<hash>`.
        let prefix = format!("{}-", &encoded[..MAX_ENCODED_LEN]);
        fs::read_dir(&self.projects_root)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .map(|e| e.path())
    }

    /// All sessions recorded for `cwd`, newest first.
    pub fn sessions_for_cwd(&self, cwd: impl AsRef<Path>) -> Vec<SessionInfo> {
        self.sessions_since(cwd.as_ref(), None)
    }

    /// Sessions for `cwd` modified at or after `min_modified_ms`, newest first.
    /// Transcript heads are only read for sessions that pass the cutoff.
    pub fn sessions_since(&self, cwd: &Path, min_modified_ms: Option<i64>) -> Vec<SessionInfo> {
        let Some(dir) = self.project_dir(cwd) else {
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut sessions: Vec<SessionInfo> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "jsonl") {
                    return None;
                }
                let meta = entry.metadata().ok()?;
                if !meta.is_file() {
                    return None;
                }
                let modified_ms = meta.modified().ok().map(system_time_ms)?;
                if min_modified_ms.is_some_and(|min| modified_ms < min) {
                    return None;
                }
                let session_id = path.file_stem()?.to_string_lossy().into_owned();
                let session_dir = dir.join(&session_id);
                Some(SessionInfo {
                    dir: session_dir.is_dir().then_some(session_dir),
                    cwd_from_transcript: self.cached_cwd(&path, modified_ms),
                    session_id,
                    transcript_path: path,
                    modified_ms,
                })
            })
            .collect();
        sort_newest_first(&mut sessions);
        sessions
    }

    /// Looks up a session by id, first under `cwd`, then in every project dir.
    pub fn find_session(&self, session_id: &str, cwd: Option<&Path>) -> Option<SessionInfo> {
        let file_name = format!("{session_id}.jsonl");
        let preferred = cwd.and_then(|c| self.project_dir(c));
        let candidates = preferred.clone().into_iter().chain(
            fs::read_dir(&self.projects_root)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(move |p| Some(p) != preferred.as_ref()),
        );
        for dir in candidates {
            let path = dir.join(&file_name);
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            let modified_ms = meta.modified().map(system_time_ms).unwrap_or(0);
            let session_dir = dir.join(session_id);
            return Some(SessionInfo {
                session_id: session_id.to_string(),
                dir: session_dir.is_dir().then_some(session_dir),
                cwd_from_transcript: self.cached_cwd(&path, modified_ms),
                transcript_path: path,
                modified_ms,
            });
        }
        None
    }

    /// Maps Claude terminals (by handle) to sessions, using the current time.
    pub fn map_terminals(
        &self,
        terminals: &[Terminal],
        hooks: Option<&HookMappings>,
    ) -> BTreeMap<String, SessionMatch> {
        self.map_terminals_at(terminals, hooks, system_time_ms(SystemTime::now()))
    }

    /// Maps Claude terminals (by handle) to sessions as of `now_ms`.
    ///
    /// Hook mappings win. Remaining terminals are grouped by worktree: a lone
    /// terminal gets the newest recent session there; when several share a
    /// worktree, the most recently active terminal gets the newest session,
    /// the next gets the second newest, and so on, all marked ambiguous.
    pub fn map_terminals_at(
        &self,
        terminals: &[Terminal],
        hooks: Option<&HookMappings>,
        now_ms: i64,
    ) -> BTreeMap<String, SessionMatch> {
        let mut out = BTreeMap::new();
        let mut claimed: HashSet<String> = HashSet::new();
        let mut groups: BTreeMap<&str, Vec<&Terminal>> = BTreeMap::new();

        let claude = terminals
            .iter()
            .filter(|t| t.agent_identity == Some(AgentIdentity::Claude));
        for terminal in claude {
            let cwd = terminal.worktree_path.as_deref().filter(|p| !p.is_empty());
            let hooked = hooks
                .and_then(|h| h.session_for(terminal))
                .and_then(|id| self.find_session(id, cwd.map(Path::new)));
            if let Some(info) = hooked {
                claimed.insert(info.session_id.clone());
                out.insert(terminal.handle.clone(), SessionMatch::Unique(info));
            } else if let Some(cwd) = cwd {
                groups.entry(cwd).or_default().push(terminal);
            } else {
                out.insert(terminal.handle.clone(), SessionMatch::None);
            }
        }

        let min_modified = now_ms.saturating_sub(RECENT_WINDOW_MS);
        for (cwd, mut group) in groups {
            let candidates: Vec<SessionInfo> = self
                .sessions_since(Path::new(cwd), Some(min_modified))
                .into_iter()
                .filter(|s| !claimed.contains(&s.session_id))
                .collect();
            if let [only] = group.as_slice() {
                let m = candidates
                    .into_iter()
                    .next()
                    .map_or(SessionMatch::None, SessionMatch::Unique);
                out.insert(only.handle.clone(), m);
                continue;
            }
            if candidates.is_empty() {
                for t in group {
                    out.insert(t.handle.clone(), SessionMatch::None);
                }
                continue;
            }
            group.sort_by(|a, b| {
                let a_ms = a.last_output_at.as_ref().and_then(|t| t.epoch_ms());
                let b_ms = b.last_output_at.as_ref().and_then(|t| t.epoch_ms());
                b_ms.cmp(&a_ms).then_with(|| a.handle.cmp(&b.handle))
            });
            for (i, t) in group.into_iter().enumerate() {
                let m = SessionMatch::Ambiguous {
                    candidates: candidates.clone(),
                    chosen: candidates.get(i).cloned(),
                };
                out.insert(t.handle.clone(), m);
            }
        }
        out
    }

    fn cached_cwd(&self, path: &Path, modified_ms: i64) -> Option<String> {
        let mut cache = self.cwd_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((mtime, cwd)) = cache.get(path)
            && (*mtime == modified_ms || cwd.is_some())
        {
            // A transcript's first `cwd` never changes once written.
            return cwd.clone();
        }
        let cwd = read_transcript_cwd(path);
        cache.insert(path.to_path_buf(), (modified_ms, cwd.clone()));
        cwd
    }
}

/// `cwd` of the first complete record in the transcript head that has one.
pub fn read_transcript_cwd(path: &Path) -> Option<String> {
    let mut head = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(HEAD_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    head.split(|&b| b == b'\n')
        // A line cut off at the byte limit is not valid JSON and is skipped.
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .find_map(|record| record.get("cwd")?.as_str().map(str::to_string))
}

fn sort_newest_first(sessions: &mut [SessionInfo]) {
    sessions.sort_by(|a, b| {
        b.modified_ms
            .cmp(&a.modified_ms)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
}

fn system_time_ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::{OrcaClient, Timestamp};
    use std::time::Duration;
    use tempfile::TempDir;

    const NOW_MS: i64 = 1_790_000_000_000;
    const MIN_MS: i64 = 60 * 1000;
    const DAY_MS: i64 = 24 * 60 * MIN_MS;
    const WT: &str = "/Users/me/orca/workspaces/app/feature-x";

    #[test]
    fn encodes_real_project_dirs() {
        let cases = [
            (
                "/Users/gylleus/orca/workspaces/commerce-platform/hack-77-core-in-template",
                "-Users-gylleus-orca-workspaces-commerce-platform-hack-77-core-in-template",
            ),
            ("/private/tmp/themes/toteme", "-private-tmp-themes-toteme"),
            (
                "/Users/gylleus/repo/depict/recommendations/.claude/worktrees/vroom-299-pins",
                "-Users-gylleus-repo-depict-recommendations--claude-worktrees-vroom-299-pins",
            ),
            (
                "/Users/gylleus/repo/saker/stronghold-worktrees/myr-8hj.1",
                "-Users-gylleus-repo-saker-stronghold-worktrees-myr-8hj-1",
            ),
            ("/a/b_c d@e", "-a-b-c-d-e"),
            // One dash per UTF-16 unit: `å` is one unit, the emoji is two.
            ("/x/å/🦀", "-x-----"),
        ];
        for (cwd, want) in cases {
            assert_eq!(encode_project_dir(Path::new(cwd)), want, "{cwd}");
        }
    }

    struct Fake {
        _tmp: TempDir,
        root: PathBuf,
    }

    impl Fake {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path().join("projects");
            fs::create_dir_all(&root).unwrap();
            Self { _tmp: tmp, root }
        }

        fn locator(&self) -> SessionLocator {
            SessionLocator::new(&self.root)
        }

        /// Writes a transcript for `cwd` with the given mtime.
        fn session(&self, cwd: &str, id: &str, modified_ms: i64, with_dir: bool) -> PathBuf {
            let dir = self.root.join(encode_project_dir(Path::new(cwd)));
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{id}.jsonl"));
            let body = format!(
                "{{\"type\":\"permission-mode\",\"sessionId\":\"{id}\"}}\n\
                 {{\"type\":\"user\",\"cwd\":\"{cwd}\",\"sessionId\":\"{id}\"}}\n"
            );
            fs::write(&path, body).unwrap();
            if with_dir {
                fs::create_dir_all(dir.join(id).join("subagents")).unwrap();
            }
            set_mtime(&path, modified_ms);
            path
        }
    }

    fn set_mtime(path: &Path, ms: i64) {
        let t = UNIX_EPOCH + Duration::from_millis(ms as u64);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn claude(handle: &str, cwd: &str, last_output_ms: Option<i64>) -> Terminal {
        Terminal {
            handle: handle.into(),
            worktree_path: Some(cwd.into()),
            tab_id: Some(format!("tab-{handle}")),
            leaf_id: Some(format!("leaf-{handle}")),
            agent_identity: Some(AgentIdentity::Claude),
            last_output_at: last_output_ms.map(Timestamp::Millis),
            ..Default::default()
        }
    }

    fn chosen_id(m: &SessionMatch) -> Option<&str> {
        m.session().map(|s| s.session_id.as_str())
    }

    #[test]
    fn sessions_are_newest_first_with_dir_and_cwd() {
        let fake = Fake::new();
        fake.session(WT, "old", NOW_MS - 3 * DAY_MS, false);
        fake.session(WT, "new", NOW_MS - MIN_MS, true);
        fake.session(WT, "mid", NOW_MS - DAY_MS, false);
        // Not a transcript.
        let dir = fake.root.join(encode_project_dir(Path::new(WT)));
        fs::write(dir.join("notes.txt"), "x").unwrap();

        let sessions = fake.locator().sessions_for_cwd(WT);
        let ids: Vec<_> = sessions.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, ["new", "mid", "old"]);
        assert_eq!(sessions[0].dir, Some(dir.join("new")));
        assert_eq!(sessions[1].dir, None);
        assert_eq!(sessions[0].modified_ms, NOW_MS - MIN_MS);
        assert_eq!(sessions[0].cwd_from_transcript.as_deref(), Some(WT));
    }

    #[test]
    fn missing_project_dir_yields_nothing() {
        let fake = Fake::new();
        assert!(fake.locator().sessions_for_cwd("/nope").is_empty());
    }

    #[test]
    fn long_paths_resolve_by_prefix() {
        let fake = Fake::new();
        let cwd = format!("/{}", "a".repeat(250));
        let encoded = encode_project_dir(Path::new(&cwd));
        let dir = fake
            .root
            .join(format!("{}-1a2b3c", &encoded[..MAX_ENCODED_LEN]));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("s1.jsonl"), "{}\n").unwrap();
        let ids: Vec<_> = fake
            .locator()
            .sessions_for_cwd(&cwd)
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        assert_eq!(ids, ["s1"]);
    }

    #[test]
    fn cwd_read_skips_truncated_and_cwdless_lines() {
        let fake = Fake::new();
        let path = fake.root.join("t.jsonl");
        let huge = "x".repeat(HEAD_BYTES as usize);
        fs::write(
            &path,
            format!("{{\"type\":\"mode\"}}\n{{\"type\":\"attachment\",\"cwd\":\"/late\",\"blob\":\"{huge}\"}}\n"),
        )
        .unwrap();
        assert_eq!(read_transcript_cwd(&path), None);
        fs::write(&path, "not json\n{\"cwd\":\"/here\"}\n").unwrap();
        assert_eq!(read_transcript_cwd(&path).as_deref(), Some("/here"));
    }

    #[test]
    fn cwd_cache_is_keyed_by_mtime() {
        let fake = Fake::new();
        let path = fake.root.join("t.jsonl");
        fs::write(&path, "{\"type\":\"mode\"}\n").unwrap();
        let locator = fake.locator();
        assert_eq!(locator.cached_cwd(&path, 1), None);
        fs::write(&path, "{\"cwd\":\"/now\"}\n").unwrap();
        // Same mtime: cached miss is reused.
        assert_eq!(locator.cached_cwd(&path, 1), None);
        // New mtime: re-read.
        assert_eq!(locator.cached_cwd(&path, 2).as_deref(), Some("/now"));
        fs::write(&path, "{\"cwd\":\"/changed\"}\n").unwrap();
        // A found cwd is stable.
        assert_eq!(locator.cached_cwd(&path, 3).as_deref(), Some("/now"));
    }

    #[test]
    fn lone_terminal_gets_newest_recent_session() {
        let fake = Fake::new();
        fake.session(WT, "older", NOW_MS - 2 * DAY_MS, false);
        fake.session(WT, "newest", NOW_MS - MIN_MS, false);
        let terms = [claude("t1", WT, Some(NOW_MS))];
        let map = fake.locator().map_terminals_at(&terms, None, NOW_MS);
        match &map["t1"] {
            SessionMatch::Unique(s) => assert_eq!(s.session_id, "newest"),
            other => panic!("expected unique, got {other:?}"),
        }
    }

    #[test]
    fn sessions_older_than_seven_days_are_ignored() {
        let fake = Fake::new();
        fake.session(WT, "stale", NOW_MS - 8 * DAY_MS, false);
        let terms = [claude("t1", WT, None)];
        let map = fake.locator().map_terminals_at(&terms, None, NOW_MS);
        assert_eq!(map["t1"], SessionMatch::None);

        fake.session(WT, "fresh", NOW_MS - 6 * DAY_MS, false);
        let map = fake.locator().map_terminals_at(&terms, None, NOW_MS);
        assert_eq!(chosen_id(&map["t1"]), Some("fresh"));
    }

    #[test]
    fn non_claude_and_pathless_terminals() {
        let fake = Fake::new();
        fake.session(WT, "s", NOW_MS, false);
        let mut codex = claude("codex", WT, None);
        codex.agent_identity = Some(AgentIdentity::Codex);
        let mut shell = claude("shell", WT, None);
        shell.agent_identity = None;
        let mut nowhere = claude("nowhere", WT, None);
        nowhere.worktree_path = None;
        let map = fake
            .locator()
            .map_terminals_at(&[codex, shell, nowhere], None, NOW_MS);
        assert_eq!(map.len(), 1);
        assert_eq!(map["nowhere"], SessionMatch::None);
    }

    #[test]
    fn shared_worktree_is_ambiguous_and_assigned_by_recency() {
        let fake = Fake::new();
        fake.session(WT, "s-new", NOW_MS - MIN_MS, false);
        fake.session(WT, "s-mid", NOW_MS - 10 * MIN_MS, false);
        fake.session(WT, "s-old", NOW_MS - 60 * MIN_MS, false);
        let terms = [
            claude("quiet", WT, Some(NOW_MS - 30 * MIN_MS)),
            claude("busy", WT, Some(NOW_MS - 5_000)),
            claude("silent", WT, None),
        ];
        let map = fake.locator().map_terminals_at(&terms, None, NOW_MS);
        assert_eq!(chosen_id(&map["busy"]), Some("s-new"));
        assert_eq!(chosen_id(&map["quiet"]), Some("s-mid"));
        assert_eq!(chosen_id(&map["silent"]), Some("s-old"));
        for m in map.values() {
            let SessionMatch::Ambiguous { candidates, .. } = m else {
                panic!("expected ambiguous, got {m:?}");
            };
            assert_eq!(candidates.len(), 3);
        }
    }

    #[test]
    fn shared_worktree_with_fewer_sessions_than_terminals() {
        let fake = Fake::new();
        fake.session(WT, "only", NOW_MS - MIN_MS, false);
        let terms = [
            claude("a", WT, Some(NOW_MS - 2 * MIN_MS)),
            claude("b", WT, Some(NOW_MS - MIN_MS)),
        ];
        let map = fake.locator().map_terminals_at(&terms, None, NOW_MS);
        assert_eq!(chosen_id(&map["b"]), Some("only"));
        assert!(matches!(
            &map["a"],
            SessionMatch::Ambiguous { chosen: None, candidates } if candidates.len() == 1
        ));
    }

    #[test]
    fn hook_mapping_overrides_heuristic() {
        let fake = Fake::new();
        fake.session(WT, "s-new", NOW_MS - MIN_MS, false);
        // Older than the recency window, but hooks are exact.
        fake.session(WT, "s-hooked", NOW_MS - 30 * DAY_MS, true);
        let terms = [
            claude("a", WT, Some(NOW_MS)),
            claude("b", WT, Some(NOW_MS - MIN_MS)),
        ];
        let mut hooks = HookMappings::default();
        hooks
            .by_pane_key
            .insert("tab-a:leaf-a".into(), "s-hooked".into());
        let map = fake
            .locator()
            .map_terminals_at(&terms, Some(&hooks), NOW_MS);
        match &map["a"] {
            SessionMatch::Unique(s) => {
                assert_eq!(s.session_id, "s-hooked");
                assert!(s.dir.is_some());
            }
            other => panic!("expected unique, got {other:?}"),
        }
        // `b` is now alone in the heuristic pool.
        assert_eq!(map["b"], {
            let s = fake.locator().sessions_for_cwd(WT);
            SessionMatch::Unique(s.into_iter().find(|s| s.session_id == "s-new").unwrap())
        });
    }

    #[test]
    fn hook_by_tab_id_finds_session_in_other_project_dir() {
        let fake = Fake::new();
        fake.session("/elsewhere", "moved", NOW_MS, false);
        let mut hooks = HookMappings::default();
        hooks.by_tab_id.insert("tab-a".into(), "moved".into());
        let map = fake
            .locator()
            .map_terminals_at(&[claude("a", WT, None)], Some(&hooks), NOW_MS);
        let s = chosen_id(&map["a"]);
        assert_eq!(s, Some("moved"));
    }

    #[test]
    fn unknown_hook_session_falls_back_to_heuristic() {
        let fake = Fake::new();
        fake.session(WT, "s", NOW_MS, false);
        let mut hooks = HookMappings::default();
        hooks
            .by_pane_key
            .insert("tab-a:leaf-a".into(), "gone".into());
        let map = fake
            .locator()
            .map_terminals_at(&[claude("a", WT, None)], Some(&hooks), NOW_MS);
        assert_eq!(chosen_id(&map["a"]), Some("s"));
    }

    /// Maps the live `orca terminal list` Claude terminals against the real
    /// `~/.claude/projects`. Run with `cargo test -- --ignored live_`.
    #[test]
    #[ignore]
    fn live_map_orca_terminals() {
        let terminals = OrcaClient::new().terminal_list().unwrap().terminals;
        let locator = SessionLocator::default();
        let started = std::time::Instant::now();
        let map = locator.map_terminals(&terminals, None);
        let first = started.elapsed();
        let started = std::time::Instant::now();
        let again = locator.map_terminals(&terminals, None);
        let cached = started.elapsed();
        assert_eq!(map, again);

        let (mut unique, mut ambiguous, mut none) = (0, 0, 0);
        for (handle, m) in &map {
            let label = match m {
                SessionMatch::Unique(_) => {
                    unique += 1;
                    "unique"
                }
                SessionMatch::Ambiguous { .. } => {
                    ambiguous += 1;
                    "ambiguous"
                }
                SessionMatch::None => {
                    none += 1;
                    "none"
                }
            };
            println!("{handle} -> {:?} ({label})", chosen_id(m));
        }
        println!(
            "claude terminals: {}, unique: {unique}, ambiguous: {ambiguous}, none: {none}; \
             first pass {first:?}, cached pass {cached:?}",
            map.len()
        );
    }
}

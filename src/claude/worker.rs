//! Background thread that maps terminals to Claude sessions, scans their subagents and tails
//! the transcript the UI is showing. The UI thread never touches `~/.claude`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};

use super::hook::{default_session_map_path, load_hook_mappings};
use super::sessions::default_projects_root;
use super::sessions::{HookMappings, SessionInfo, SessionLocator, SessionMatch};
use super::subagents::{SubagentInfo, SubagentWatcher};
use super::transcript::read_transcript_tail;
use super::usage::{SessionUsage, UsageTracker};
use crate::orca::Terminal;
use crate::poller::Repaint;

/// Outputs buffered for the UI; when full, new ones are dropped until the UI catches up.
const OUTPUT_BUFFER: usize = 4;

/// One scan of Claude state for the current terminals.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeData {
    /// Session match per Claude terminal handle.
    pub session_by_handle: BTreeMap<String, SessionMatch>,
    /// Subagents per session id.
    pub subagents: BTreeMap<String, Vec<SubagentInfo>>,
    /// Token usage per session id, subagents included.
    pub usage: BTreeMap<String, SessionUsage>,
    /// Wall-clock time of the scan, in ms since the Unix epoch.
    pub at_ms: i64,
}

impl ClaudeData {
    pub fn subagent(&self, session_id: &str, agent_id: &str) -> Option<&SubagentInfo> {
        self.subagents
            .get(session_id)?
            .iter()
            .find(|a| a.agent_id == agent_id)
    }

    pub fn subagent_count(&self) -> usize {
        self.subagents.values().map(Vec::len).sum()
    }
}

/// Rendered tail of a watched transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptTail {
    pub path: PathBuf,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeWorkerConfig {
    pub projects_root: PathBuf,
    pub session_map_path: PathBuf,
    /// Rescan interval when terminals are unchanged.
    pub refresh: Duration,
    /// How often the watched transcript is checked for changes.
    pub transcript_poll: Duration,
    /// Rendered lines kept in a [`TranscriptTail`].
    pub tail_lines: usize,
}

impl Default for ClaudeWorkerConfig {
    fn default() -> Self {
        Self {
            projects_root: default_projects_root(),
            session_map_path: default_session_map_path(),
            refresh: Duration::from_secs(3),
            transcript_poll: Duration::from_millis(500),
            tail_lines: 200,
        }
    }
}

enum Command {
    Terminals(Vec<Terminal>),
    WatchTranscript(Option<PathBuf>),
    Shutdown,
}

/// Handle to the Claude worker thread. Dropping it shuts the thread down and joins it.
pub struct ClaudeWorker {
    data: Receiver<ClaudeData>,
    tails: Receiver<TranscriptTail>,
    commands: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl ClaudeWorker {
    pub fn spawn(repaint: Option<Repaint>) -> Self {
        Self::spawn_with(ClaudeWorkerConfig::default(), repaint)
    }

    pub fn spawn_with(config: ClaudeWorkerConfig, repaint: Option<Repaint>) -> Self {
        let (data_tx, data_rx) = crossbeam_channel::bounded(OUTPUT_BUFFER);
        let (tail_tx, tail_rx) = crossbeam_channel::bounded(OUTPUT_BUFFER);
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("claude-worker".into())
            .spawn(move || {
                let outputs = Outputs {
                    data: data_tx,
                    tails: tail_tx,
                    repaint,
                };
                run_loop(config, &outputs, &cmd_rx);
            })
            .expect("spawn claude worker thread");
        Self {
            data: data_rx,
            tails: tail_rx,
            commands: cmd_tx,
            thread: Some(thread),
        }
    }

    /// Hands the worker the latest terminal list; nothing is scanned before the first one.
    pub fn send_terminals(&self, terminals: Vec<Terminal>) {
        let _ = self.commands.send(Command::Terminals(terminals));
    }

    /// Tails the transcript at `path` (or stops tailing) and publishes it on change.
    pub fn watch_transcript(&self, path: Option<PathBuf>) {
        let _ = self.commands.send(Command::WatchTranscript(path));
    }

    /// The newest pending scan, discarding older ones.
    pub fn latest(&self) -> Option<ClaudeData> {
        self.data.try_iter().last()
    }

    /// The newest pending transcript tail, discarding older ones.
    pub fn latest_transcript(&self) -> Option<TranscriptTail> {
        self.tails.try_iter().last()
    }
}

impl Drop for ClaudeWorker {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("claude worker thread panicked");
        }
    }
}

/// Maps `terminals` to sessions and scans their subagents and usage as of `now_ms`.
pub fn collect(
    locator: &SessionLocator,
    watcher: &mut SubagentWatcher,
    usage: &mut UsageTracker,
    terminals: &[Terminal],
    hooks: &HookMappings,
    now_ms: i64,
) -> ClaudeData {
    let session_by_handle = locator.map_terminals_at(terminals, Some(hooks), now_ms);
    let mut seen = BTreeSet::new();
    let sessions: Vec<SessionInfo> = session_by_handle
        .values()
        .filter_map(SessionMatch::session)
        .filter(|s| seen.insert(s.session_id.clone()))
        .cloned()
        .collect();
    let subagents = watcher.refresh(&sessions, now_ms);
    let usage = usage.refresh(&sessions);
    ClaudeData {
        session_by_handle,
        subagents,
        usage,
        at_ms: now_ms,
    }
}

struct Outputs {
    data: Sender<ClaudeData>,
    tails: Sender<TranscriptTail>,
    repaint: Option<Repaint>,
}

impl Outputs {
    /// Sends `value`; `false` once the UI side is gone.
    fn send<T>(&self, tx: &Sender<T>, value: T, what: &str) -> bool {
        match tx.try_send(value) {
            Ok(()) => {
                if let Some(repaint) = &self.repaint {
                    repaint();
                }
                true
            }
            Err(TrySendError::Full(_)) => {
                log::debug!("UI behind; dropped a {what}");
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

/// The transcript being tailed and the file state it was last rendered at.
struct Watched {
    path: PathBuf,
    seen: Option<(u64, SystemTime)>,
}

fn run_loop(config: ClaudeWorkerConfig, outputs: &Outputs, commands: &Receiver<Command>) {
    let locator = SessionLocator::new(&config.projects_root);
    let mut watcher = SubagentWatcher::new();
    let mut usage = UsageTracker::new();
    let mut terminals: Option<Vec<Terminal>> = None;
    let mut handles: BTreeSet<String> = BTreeSet::new();
    let mut next_refresh = Instant::now();
    let mut watched: Option<Watched> = None;

    loop {
        if let Some(terms) = &terminals
            && Instant::now() >= next_refresh
        {
            next_refresh = Instant::now() + config.refresh;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let hooks = load_hook_mappings(&config.session_map_path);
                collect(
                    &locator,
                    &mut watcher,
                    &mut usage,
                    terms,
                    &hooks,
                    wall_clock_ms(),
                )
            }));
            match result {
                Ok(data) => {
                    log::debug!(
                        "claude scan: {} sessions, {} subagents ({} running)",
                        data.subagents.len(),
                        data.subagent_count(),
                        data.subagents
                            .values()
                            .flatten()
                            .filter(|a| !a.state.is_finished())
                            .count()
                    );
                    if !outputs.send(&outputs.data, data, "claude scan") {
                        return;
                    }
                }
                Err(_) => log::error!("claude scan panicked; continuing"),
            }
        }

        if let Some(w) = &mut watched
            && !poll_transcript(w, config.tail_lines, outputs)
        {
            return;
        }

        let wait = if terminals.is_some() {
            next_refresh
                .saturating_duration_since(Instant::now())
                .min(config.transcript_poll)
        } else {
            config.transcript_poll
        };
        let first = match commands.recv_timeout(wait) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        for cmd in std::iter::once(first).chain(commands.try_iter()) {
            match cmd {
                Command::Terminals(terms) => {
                    let new_handles: BTreeSet<String> =
                        terms.iter().map(|t| t.handle.clone()).collect();
                    if terminals.is_none() || new_handles != handles {
                        next_refresh = Instant::now();
                    }
                    handles = new_handles;
                    terminals = Some(terms);
                }
                Command::WatchTranscript(path) => {
                    if watched.as_ref().map(|w| &w.path) != path.as_ref() {
                        watched = path.map(|path| Watched { path, seen: None });
                    }
                }
                Command::Shutdown => return,
            }
        }
    }
}

/// Re-renders the watched transcript when its size or mtime changed; `false` once the UI
/// side is gone.
fn poll_transcript(watched: &mut Watched, tail_lines: usize, outputs: &Outputs) -> bool {
    let state = std::fs::metadata(&watched.path)
        .ok()
        .map(|m| (m.len(), m.modified().unwrap_or(UNIX_EPOCH)));
    if state.is_none() || state == watched.seen {
        return true;
    }
    watched.seen = state;
    let lines = match read_transcript_tail(&watched.path, tail_lines) {
        Ok(lines) => lines,
        Err(e) => {
            log::debug!("cannot read {}: {e}", watched.path.display());
            return true;
        }
    };
    let tail = TranscriptTail {
        path: watched.path.clone(),
        lines,
    };
    outputs.send(&outputs.tails, tail, "transcript tail")
}

fn wall_clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::Path;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::claude::encode_project_dir;
    use crate::claude::subagents::{SubagentParent, SubagentState};
    use crate::orca::AgentIdentity;

    const SESSION: &str = "11111111-2222-3333-4444-555555555555";
    const CWD: &str = "/Users/me/repo";

    /// A projects tree with one session for [`CWD`] and a subagent `a1` with child `a2`.
    struct Tree {
        tmp: TempDir,
    }

    impl Tree {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let project = tmp.path().join(encode_project_dir(Path::new(CWD)));
            let subagents = project.join(SESSION).join("subagents");
            fs::create_dir_all(&subagents).unwrap();
            fs::write(
                project.join(format!("{SESSION}.jsonl")),
                format!("{}\n", json!({"type": "user", "cwd": CWD})),
            )
            .unwrap();
            let tree = Self { tmp };
            tree.agent("a1", None, "Review the diff");
            tree.agent("a2", Some("a1"), "Check tests");
            tree
        }

        fn root(&self) -> PathBuf {
            self.tmp.path().to_path_buf()
        }

        fn transcript(&self, id: &str) -> PathBuf {
            self.root()
                .join(encode_project_dir(Path::new(CWD)))
                .join(SESSION)
                .join("subagents")
                .join(format!("agent-{id}.jsonl"))
        }

        fn agent(&self, id: &str, parent: Option<&str>, description: &str) {
            let mut meta = json!({
                "agentType": "general-purpose", "description": description,
                "toolUseId": format!("toolu_{id}"), "spawnDepth": 1, "model": "opus",
            });
            if let Some(p) = parent {
                meta["parentAgentId"] = json!(p);
                meta["spawnDepth"] = json!(2);
            }
            let jsonl = self.transcript(id);
            fs::write(jsonl.with_extension("meta.json"), meta.to_string()).unwrap();
            let first = json!({"type": "user", "agentId": id,
                "message": {"role": "user", "content": "do the thing"}});
            fs::write(&jsonl, format!("{first}\n")).unwrap();
        }
    }

    fn claude_terminal(handle: &str) -> Terminal {
        Terminal {
            handle: handle.into(),
            worktree_path: Some(CWD.into()),
            agent_identity: Some(AgentIdentity::Claude),
            connected: true,
            ..Default::default()
        }
    }

    fn config(tree: &Tree) -> ClaudeWorkerConfig {
        ClaudeWorkerConfig {
            projects_root: tree.root(),
            session_map_path: tree.root().join("no-session-map.jsonl"),
            refresh: Duration::from_millis(50),
            transcript_poll: Duration::from_millis(20),
            tail_lines: 10,
        }
    }

    fn wait_for<T>(mut poll: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(v) = poll() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::park_timeout(Duration::from_millis(10));
        }
    }

    #[test]
    fn collect_maps_terminals_and_scans_subagents() {
        let tree = Tree::new();
        let locator = SessionLocator::new(tree.root());
        let mut watcher = SubagentWatcher::new();
        let terminals = vec![claude_terminal("term_a"), Terminal::default()];
        let data = collect(
            &locator,
            &mut watcher,
            &mut UsageTracker::new(),
            &terminals,
            &HookMappings::default(),
            wall_clock_ms(),
        );
        assert_eq!(data.session_by_handle.len(), 1);
        let session = data.session_by_handle["term_a"].session().unwrap();
        assert_eq!(session.session_id, SESSION);
        assert_eq!(data.subagent_count(), 2);
        let child = data.subagent(SESSION, "a2").unwrap();
        assert_eq!(child.parent, SubagentParent::Agent("a1".into()));
        assert_eq!(child.state, SubagentState::Running);
        assert!(data.subagent(SESSION, "zz").is_none());
    }

    #[test]
    fn worker_publishes_scans_after_terminals_arrive() {
        let tree = Tree::new();
        let worker = ClaudeWorker::spawn_with(config(&tree), None);
        std::thread::sleep(Duration::from_millis(100));
        assert!(worker.latest().is_none(), "no scan before terminals");

        worker.send_terminals(vec![claude_terminal("term_a")]);
        let data = wait_for(|| worker.latest());
        assert_eq!(data.subagent_count(), 2);

        tree.agent("a3", None, "Third");
        let data = wait_for(|| worker.latest().filter(|d| d.subagent_count() == 3));
        assert!(data.subagent(SESSION, "a3").is_some());
    }

    #[test]
    fn worker_tails_the_watched_transcript() {
        let tree = Tree::new();
        let worker = ClaudeWorker::spawn_with(config(&tree), None);
        let path = tree.transcript("a1");
        worker.watch_transcript(Some(path.clone()));
        let tail = wait_for(|| worker.latest_transcript());
        assert_eq!(tail.path, path);
        assert_eq!(tail.lines, vec!["> do the thing"]);

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        let record = json!({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "on it"}]}});
        writeln!(f, "{record}").unwrap();
        drop(f);
        let tail = wait_for(|| worker.latest_transcript());
        assert_eq!(tail.lines, vec!["> do the thing", "on it"]);

        worker.watch_transcript(None);
        std::thread::sleep(Duration::from_millis(100));
        let _ = worker.latest_transcript();
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{record}").unwrap();
        drop(f);
        std::thread::sleep(Duration::from_millis(150));
        assert!(worker.latest_transcript().is_none(), "unwatched");
    }
}

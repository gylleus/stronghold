//! Background thread that maps terminals to Claude sessions, scans their subagents and reads
//! the transcript the UI is showing (all of it, then what is appended). The UI thread never
//! touches `~/.claude`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};

use super::forwards::{ForwardScanner, SendWatch};
use super::hook::{default_session_map_path, load_hook_mappings};
use super::sessions::default_projects_root;
use super::sessions::{HookMappings, SessionInfo, SessionLocator, SessionMatch};
use super::subagents::{SubagentInfo, SubagentWatcher};
use super::turns::{Turn, TurnTracker};
use super::usage::{SessionUsage, UsageTracker};
use crate::chat::history::{HistoryChunk, HistoryReader};
use crate::clan::Forward;
use crate::codex::{CodexSession, CodexTracker, default_sessions_root};
use crate::orca::Terminal;
use crate::poller::Repaint;
use crate::traffic::AgentMessage;

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
    /// Rollout and token usage per Codex terminal handle.
    pub codex: BTreeMap<String, CodexSession>,
    /// Terminals forwarded to with `orca terminal send`, per watched handle (see
    /// [`ClaudeWorker::watch_forwards`]) with a mapped session.
    pub forwards: BTreeMap<String, Vec<Forward>>,
    /// How each session's last turn ended, per session id.
    pub turns: BTreeMap<String, Turn>,
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

    /// The last turn of the session mapped to terminal `handle`.
    pub fn turn(&self, handle: &str) -> Option<&Turn> {
        let session = self.session_by_handle.get(handle)?.session()?;
        self.turns.get(&session.session_id)
    }

    pub fn subagent_count(&self) -> usize {
        self.subagents.values().map(Vec::len).sum()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeWorkerConfig {
    pub projects_root: PathBuf,
    pub session_map_path: PathBuf,
    /// Codex rollouts, `~/.codex/sessions`.
    pub codex_sessions_root: PathBuf,
    /// Rescan interval when terminals are unchanged.
    pub refresh: Duration,
    /// How often the watched transcript is checked for changes.
    pub transcript_poll: Duration,
}

impl Default for ClaudeWorkerConfig {
    fn default() -> Self {
        Self {
            projects_root: default_projects_root(),
            session_map_path: default_session_map_path(),
            codex_sessions_root: default_sessions_root(),
            refresh: Duration::from_secs(3),
            transcript_poll: Duration::from_millis(300),
        }
    }
}

enum Command {
    Terminals(Vec<Terminal>),
    WatchTranscript(Option<PathBuf>),
    WatchForwards(BTreeSet<String>),
    Shutdown,
}

/// Handle to the Claude worker thread. Dropping it shuts the thread down and joins it.
pub struct ClaudeWorker {
    data: Receiver<ClaudeData>,
    history: Receiver<HistoryChunk>,
    sends: Receiver<AgentMessage>,
    commands: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl ClaudeWorker {
    pub fn spawn(repaint: Option<Repaint>) -> Self {
        Self::spawn_with(ClaudeWorkerConfig::default(), repaint)
    }

    pub fn spawn_with(config: ClaudeWorkerConfig, repaint: Option<Repaint>) -> Self {
        let (data_tx, data_rx) = crossbeam_channel::bounded(OUTPUT_BUFFER);
        // Unbounded: each chunk carries records the UI has not seen, none may be dropped.
        let (history_tx, history_rx) = crossbeam_channel::unbounded();
        // Unbounded too: each send is drawn once, unlike a scan a newer one replaces.
        let (sends_tx, sends_rx) = crossbeam_channel::unbounded();
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("claude-worker".into())
            .spawn(move || {
                let outputs = Outputs {
                    data: data_tx,
                    history: history_tx,
                    sends: sends_tx,
                    repaint,
                };
                run_loop(config, &outputs, &cmd_rx);
            })
            .expect("spawn claude worker thread");
        Self {
            data: data_rx,
            history: history_rx,
            sends: sends_rx,
            commands: cmd_tx,
            thread: Some(thread),
        }
    }

    /// Hands the worker the latest terminal list; nothing is scanned before the first one.
    pub fn send_terminals(&self, terminals: Vec<Terminal>) {
        let _ = self.commands.send(Command::Terminals(terminals));
    }

    /// Reads the transcript at `path` (or stops): all of it, then each append, published as
    /// [`HistoryChunk`]s.
    pub fn watch_transcript(&self, path: Option<PathBuf>) {
        let _ = self.commands.send(Command::WatchTranscript(path));
    }

    /// Reads the forward commands of these terminals' transcripts on each scan (see
    /// [`ClaudeData::forwards`]); a Run's declared coordinator may be an attendant.
    pub fn watch_forwards(&self, handles: BTreeSet<String>) {
        let _ = self.commands.send(Command::WatchForwards(handles));
    }

    /// The newest pending scan, discarding older ones.
    pub fn latest(&self) -> Option<ClaudeData> {
        self.data.try_iter().last()
    }

    /// The `orca terminal send`s Claude terminals ran since the last call, as traffic (see
    /// [`SendWatch`]).
    pub fn terminal_sends(&self) -> Vec<AgentMessage> {
        self.sends.try_iter().collect()
    }

    /// The pending transcript chunks, oldest first.
    pub fn history_chunks(&self) -> Vec<HistoryChunk> {
        self.history.try_iter().collect()
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
    turns: &mut TurnTracker,
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
    let turns = turns.refresh(&sessions);
    ClaudeData {
        session_by_handle,
        subagents,
        usage,
        turns,
        codex: BTreeMap::new(),
        forwards: BTreeMap::new(),
        at_ms: now_ms,
    }
}

struct Outputs {
    data: Sender<ClaudeData>,
    history: Sender<HistoryChunk>,
    sends: Sender<AgentMessage>,
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

/// The transcript being read and the file state it was last read at.
struct Watched {
    reader: HistoryReader,
    seen: Option<(u64, SystemTime)>,
}

fn run_loop(config: ClaudeWorkerConfig, outputs: &Outputs, commands: &Receiver<Command>) {
    let locator = SessionLocator::new(&config.projects_root);
    let mut watcher = SubagentWatcher::new();
    let mut usage = UsageTracker::new();
    let mut turns = TurnTracker::new();
    let mut codex = CodexTracker::new(&config.codex_sessions_root);
    let mut forwards = ForwardScanner::new();
    let mut send_watch = SendWatch::new();
    let mut forward_handles: BTreeSet<String> = BTreeSet::new();
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
                let now_ms = wall_clock_ms();
                let mut data = collect(
                    &locator,
                    &mut watcher,
                    &mut usage,
                    &mut turns,
                    terms,
                    &hooks,
                    now_ms,
                );
                data.codex = codex.refresh(terms, now_ms);
                data.forwards = forwards.refresh(&forward_transcripts(&data, &forward_handles));
                let sends = send_watch.refresh(&sender_transcripts(&data), now_ms);
                (data, sends)
            }));
            match result {
                Ok((data, sends)) => {
                    for send in sends {
                        if outputs.sends.send(send).is_err() {
                            return;
                        }
                    }
                    log::debug!(
                        "claude scan: {} sessions, {} subagents ({} running), {} codex rollouts",
                        data.subagents.len(),
                        data.subagent_count(),
                        data.subagents
                            .values()
                            .flatten()
                            .filter(|a| !a.state.is_finished())
                            .count(),
                        data.codex.len()
                    );
                    if !outputs.send(&outputs.data, data, "claude scan") {
                        return;
                    }
                }
                Err(_) => log::error!("claude scan panicked; continuing"),
            }
        }

        if let Some(w) = &mut watched
            && !poll_transcript(w, outputs)
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
                    if watched.as_ref().map(|w| w.reader.path()) != path.as_deref() {
                        watched = path.map(|path| Watched {
                            reader: HistoryReader::new(path),
                            seen: None,
                        });
                    }
                }
                Command::WatchForwards(new) => {
                    if new != forward_handles {
                        forward_handles = new;
                        next_refresh = Instant::now();
                    }
                }
                Command::Shutdown => return,
            }
        }
    }
}

/// Transcript paths of the `handles` that are mapped to a session.
fn forward_transcripts(data: &ClaudeData, handles: &BTreeSet<String>) -> BTreeMap<String, PathBuf> {
    handles
        .iter()
        .filter_map(|h| {
            let session = data.session_by_handle.get(h)?.session()?;
            Some((h.clone(), session.transcript_path.clone()))
        })
        .collect()
}

/// Transcript paths of every Claude terminal mapped to a session, for [`SendWatch`]. A
/// transcript several terminals are mapped to goes to the one mapped exactly, and to none when
/// all are guesses: an envelope from the wrong terminal is worse than none.
fn sender_transcripts(data: &ClaudeData) -> BTreeMap<String, PathBuf> {
    let mut by_path: BTreeMap<&PathBuf, Vec<(&String, bool)>> = BTreeMap::new();
    for (handle, m) in &data.session_by_handle {
        if let Some(session) = m.session() {
            let exact = matches!(m, SessionMatch::Unique(_));
            by_path
                .entry(&session.transcript_path)
                .or_default()
                .push((handle, exact));
        }
    }
    by_path
        .into_iter()
        .filter_map(|(path, handles)| {
            let handle = match handles.as_slice() {
                [(h, _)] => *h,
                _ => {
                    let mut exact = handles.iter().filter(|(_, e)| *e);
                    match (exact.next(), exact.next()) {
                        (Some((h, _)), None) => *h,
                        _ => return None,
                    }
                }
            };
            Some((handle.clone(), path.clone()))
        })
        .collect()
}

/// Reads what was appended to the watched transcript when its size or mtime changed; `false`
/// once the UI side is gone.
fn poll_transcript(watched: &mut Watched, outputs: &Outputs) -> bool {
    let state = std::fs::metadata(watched.reader.path())
        .ok()
        .map(|m| (m.len(), m.modified().unwrap_or(UNIX_EPOCH)));
    if state.is_none() || state == watched.seen {
        return true;
    }
    watched.seen = state;
    match watched.reader.read_new() {
        Ok(Some(chunk)) => {
            if outputs.history.send(chunk).is_err() {
                return false;
            }
            if let Some(repaint) = &outputs.repaint {
                repaint();
            }
            true
        }
        Ok(None) => true,
        Err(e) => {
            log::debug!("cannot read {}: {e}", watched.reader.path().display());
            true
        }
    }
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
            codex_sessions_root: tree.root().join("no-codex-sessions"),
            refresh: Duration::from_millis(50),
            transcript_poll: Duration::from_millis(20),
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
            &mut TurnTracker::new(),
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

    /// Texts of the prompts and replies in `chunks`.
    fn texts(chunks: &[HistoryChunk]) -> Vec<String> {
        let mut t = crate::chat::Transcript::default();
        for record in chunks.iter().flat_map(|c| &c.records) {
            t.apply_history(record);
        }
        t.items
            .iter()
            .filter_map(|item| match item {
                crate::chat::ChatItem::User(s) => Some(format!("> {s}")),
                crate::chat::ChatItem::Assistant { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn worker_reads_the_whole_watched_transcript_then_appends() {
        let tree = Tree::new();
        let worker = ClaudeWorker::spawn_with(config(&tree), None);
        let path = tree.transcript("a1");
        worker.watch_transcript(Some(path.clone()));
        let first = wait_for(|| Some(worker.history_chunks()).filter(|c| !c.is_empty()));
        assert_eq!(first[0].path, path);
        assert_eq!(texts(&first), vec!["> do the thing"]);

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        let record = json!({"type": "assistant", "message": {"id": "m1", "content": [
            {"type": "text", "text": "on it"}]}});
        writeln!(f, "{record}").unwrap();
        drop(f);
        let next = wait_for(|| Some(worker.history_chunks()).filter(|c| !c.is_empty()));
        assert!(!next[0].reset);
        assert_eq!(texts(&next), vec!["on it"], "only the new record");

        worker.watch_transcript(None);
        std::thread::sleep(Duration::from_millis(100));
        let _ = worker.history_chunks();
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{record}").unwrap();
        drop(f);
        std::thread::sleep(Duration::from_millis(150));
        assert!(worker.history_chunks().is_empty(), "unwatched");
    }

    fn info(path: &str) -> SessionInfo {
        SessionInfo {
            session_id: path.into(),
            transcript_path: path.into(),
            dir: None,
            modified_ms: 0,
            cwd_from_transcript: None,
        }
    }

    #[test]
    fn sends_are_read_from_transcripts_owned_by_one_terminal() {
        let ambiguous = |path: &str| SessionMatch::Ambiguous {
            candidates: vec![info(path)],
            chosen: Some(info(path)),
        };
        let data = ClaudeData {
            session_by_handle: [
                ("term_unique".to_string(), SessionMatch::Unique(info("/a"))),
                ("term_guess".into(), ambiguous("/b")),
                ("term_exact".into(), SessionMatch::Unique(info("/c"))),
                ("term_guess_c".into(), ambiguous("/c")),
                ("term_guess_d1".into(), ambiguous("/d")),
                ("term_guess_d2".into(), ambiguous("/d")),
                ("term_none".into(), SessionMatch::None),
            ]
            .into(),
            ..Default::default()
        };
        let found = sender_transcripts(&data);
        let expected: BTreeMap<String, PathBuf> = [
            ("term_unique".to_string(), PathBuf::from("/a")),
            ("term_guess".into(), "/b".into()),
            ("term_exact".into(), "/c".into()),
        ]
        .into();
        assert_eq!(found, expected);
    }

    #[test]
    fn worker_reports_terminal_sends_made_after_it_started() {
        let tree = Tree::new();
        let transcript = tree
            .root()
            .join(encode_project_dir(Path::new(CWD)))
            .join(format!("{SESSION}.jsonl"));
        let send = |target: &str| {
            let at = jiff::Timestamp::now().to_string();
            json!({"type": "assistant", "timestamp": at, "message": {"content": [
                {"type": "tool_use", "name": "Bash", "input": {"command":
                    format!("orca terminal send --terminal {target} --text hi --enter")}}]}})
        };
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(f, "{}", send("term_old")).unwrap();
        f.flush().unwrap();

        let worker = ClaudeWorker::spawn_with(config(&tree), None);
        worker.send_terminals(vec![claude_terminal("term_a")]);
        wait_for(|| worker.latest());
        assert!(worker.terminal_sends().is_empty(), "history is not traffic");

        writeln!(f, "{}", send("term_b")).unwrap();
        f.flush().unwrap();
        let sent = wait_for(|| Some(worker.terminal_sends()).filter(|s| !s.is_empty()));
        assert_eq!(
            sent,
            [AgentMessage::new(
                "term_a",
                "term_b",
                crate::traffic::MessageKind::Message
            )]
        );
    }
}

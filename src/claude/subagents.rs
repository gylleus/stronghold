//! Discovers subagents from `subagents/agent-*.jsonl` and their meta files.
//!
//! A session's subagents live flat in `<session dir>/subagents/`, nested ones
//! included: `agent-<id>.meta.json` carries `toolUseId`, `spawnDepth` and, for
//! depth 2+, `parentAgentId` (the spawning agent's jsonl is in the same folder).
//!
//! Completion is read from transcripts as `tool_use_id` signals:
//! - a `tool_result` for the spawn call in the parent transcript, unless its
//!   `toolUseResult.status` is `async_launched` (background agents get that
//!   result immediately);
//! - a `<task-notification>` with `<tool-use-id>` and `<status>`, recorded as a
//!   `queue-operation` in the session jsonl and as a user message in the parent
//!   transcript. It fires each time a background agent stops, so an agent that
//!   writes again after its last signal (resumed by a message) is running.
//!
//! Messages resume a finished agent without a new signal, so an agent whose
//! own transcript last ended a turn (`stop_reason: end_turn`) is done too.
//!
//! Transcripts are append-only, so each file is tailed from a remembered offset.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use serde_json::Value;

use super::sessions::SessionInfo;

/// Subagents whose meta or jsonl is older than this are ignored.
pub const RECENT_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
/// A subagent without a result and without writes for this long is stale.
pub const STALE_AFTER_MS: i64 = 30 * 60 * 1000;
/// Writes this long after a completion signal mean the agent was resumed.
pub const RESUME_GRACE_MS: i64 = 5_000;
/// How often the watcher rescans every session regardless of events.
pub const FULL_RESCAN_MS: i64 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentParent {
    /// Spawned by the session's main agent.
    Session,
    /// Spawned by another subagent (its agent id).
    Agent(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentState {
    Running,
    Done,
    /// Finished with an error result or a `failed` notification.
    Failed,
    /// No result and no writes for [`STALE_AFTER_MS`]; presumed done.
    Stale,
}

impl SubagentState {
    pub fn is_finished(self) -> bool {
        self != SubagentState::Running
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentInfo {
    pub agent_id: String,
    pub session_id: String,
    pub agent_type: String,
    pub description: String,
    pub model: Option<String>,
    pub spawn_depth: u32,
    /// The spawning `Agent` tool call; absent for some agent kinds.
    pub tool_use_id: Option<String>,
    pub parent: SubagentParent,
    /// Spawned with `requestShape: "background"`.
    pub background: bool,
    /// First transcript record's timestamp, else the meta file's mtime.
    pub started_ms: i64,
    /// Transcript (or meta) mtime.
    pub last_activity_ms: i64,
    pub state: SubagentState,
    /// Timestamp of the completion signal the state is based on.
    pub finished_ms: Option<i64>,
    pub transcript_path: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentMeta {
    #[serde(default)]
    agent_type: String,
    #[serde(default)]
    description: String,
    model: Option<String>,
    #[serde(default)]
    spawn_depth: u32,
    tool_use_id: Option<String>,
    parent_agent_id: Option<String>,
    request_shape: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Completion {
    state: SubagentState,
    at_ms: i64,
}

#[derive(Debug)]
struct AgentEntry {
    meta: AgentMeta,
    meta_mtime: i64,
    jsonl_path: PathBuf,
    jsonl_mtime: Option<i64>,
    started_ms: Option<i64>,
}

impl AgentEntry {
    fn last_activity_ms(&self) -> i64 {
        self.jsonl_mtime.unwrap_or(self.meta_mtime)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Tail {
    pub(crate) offset: u64,
}

#[derive(Debug, Default)]
struct SessionCache {
    agents: BTreeMap<String, AgentEntry>,
    tails: HashMap<PathBuf, Tail>,
    /// Latest completion signal per spawn `tool_use_id`.
    signals: HashMap<String, Completion>,
    /// Per transcript: when its last user/assistant record ended the turn.
    turn_ended: HashMap<PathBuf, Option<i64>>,
}

/// Pure, incremental subagent scanner. Call [`SubagentScanner::scan`] on a
/// timer, or use [`SubagentWatcher`] to only re-read changed sessions.
#[derive(Debug, Default)]
pub struct SubagentScanner {
    sessions: HashMap<String, SessionCache>,
}

impl SubagentScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Re-reads every given session and returns its subagents. Sessions not
    /// in `sessions` are forgotten.
    pub fn scan(
        &mut self,
        sessions: &[SessionInfo],
        now_ms: i64,
    ) -> BTreeMap<String, Vec<SubagentInfo>> {
        self.retain_sessions(sessions);
        for session in sessions {
            self.update_session(session, now_ms);
        }
        self.snapshot(sessions, now_ms)
    }

    /// Drops cached state for sessions not in `sessions`.
    pub fn retain_sessions(&mut self, sessions: &[SessionInfo]) {
        let keep: HashSet<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
        self.sessions.retain(|id, _| keep.contains(id.as_str()));
    }

    /// Picks up new or changed meta files and appended transcript lines.
    pub fn update_session(&mut self, session: &SessionInfo, now_ms: i64) {
        let cache = self.sessions.entry(session.session_id.clone()).or_default();
        let min_ms = now_ms - RECENT_WINDOW_MS;
        if let Some(dir) = session_dir(session) {
            refresh_agents(cache, &dir.join("subagents"), min_ms);
        } else {
            cache.agents.clear();
        }

        if cache.agents.is_empty() {
            return;
        }
        let wanted: HashSet<&str> = cache
            .agents
            .values()
            .filter_map(|a| a.meta.tool_use_id.as_deref())
            .collect();
        let mut files = vec![session.transcript_path.clone()];
        for entry in cache.agents.values() {
            if !files.contains(&entry.jsonl_path) {
                files.push(entry.jsonl_path.clone());
            }
            if let Some(parent) = &entry.meta.parent_agent_id
                && let Some(parent_path) = entry.jsonl_path.parent()
            {
                let path = parent_path.join(format!("agent-{parent}.jsonl"));
                if !files.contains(&path) {
                    files.push(path);
                }
            }
        }
        for path in files {
            let tail = cache.tails.entry(path.clone()).or_default();
            let turn = cache.turn_ended.entry(path.clone()).or_default();
            tail_file(&path, tail, |line| {
                if let Some(ended) = turn_record(line) {
                    *turn = ended;
                }
                for (id, completion) in completion_signals(line, &wanted) {
                    let slot = cache.signals.entry(id).or_insert(completion);
                    if completion.at_ms >= slot.at_ms {
                        *slot = completion;
                    }
                }
            });
        }
    }

    /// Current subagents of the given sessions from cached data; no file I/O.
    pub fn snapshot(
        &self,
        sessions: &[SessionInfo],
        now_ms: i64,
    ) -> BTreeMap<String, Vec<SubagentInfo>> {
        sessions
            .iter()
            .map(|s| {
                (
                    s.session_id.clone(),
                    self.session_subagents(&s.session_id, now_ms),
                )
            })
            .collect()
    }

    fn session_subagents(&self, session_id: &str, now_ms: i64) -> Vec<SubagentInfo> {
        let Some(cache) = self.sessions.get(session_id) else {
            return Vec::new();
        };
        let min_ms = now_ms - RECENT_WINDOW_MS;
        let mut built: Vec<(SubagentInfo, bool)> = cache
            .agents
            .iter()
            .filter(|(_, e)| e.meta_mtime.max(e.jsonl_mtime.unwrap_or(i64::MIN)) >= min_ms)
            .map(|(id, e)| {
                let signal = e
                    .meta
                    .tool_use_id
                    .as_ref()
                    .and_then(|t| cache.signals.get(t));
                let turn_ended = cache.turn_ended.get(&e.jsonl_path).copied().flatten();
                build_info(session_id, id, e, signal.copied(), turn_ended, now_ms)
            })
            .collect();
        keep_parents_of_running_alive(&mut built);
        let mut out: Vec<SubagentInfo> = built.into_iter().map(|(info, _)| info).collect();
        out.sort_by(|a, b| (a.started_ms, &a.agent_id).cmp(&(b.started_ms, &b.agent_id)));
        out
    }
}

fn build_info(
    session_id: &str,
    agent_id: &str,
    entry: &AgentEntry,
    signal: Option<Completion>,
    turn_ended: Option<i64>,
    now_ms: i64,
) -> (SubagentInfo, bool) {
    let last_activity_ms = entry.last_activity_ms();
    let signalled = signal.filter(|c| last_activity_ms <= c.at_ms + RESUME_GRACE_MS);
    let finished = signalled.or(turn_ended.map(|at_ms| Completion {
        state: SubagentState::Done,
        at_ms,
    }));
    let state = match finished {
        Some(c) => c.state,
        None if now_ms - last_activity_ms > STALE_AFTER_MS => SubagentState::Stale,
        None => SubagentState::Running,
    };
    let meta = &entry.meta;
    let info = SubagentInfo {
        agent_id: agent_id.to_string(),
        session_id: session_id.to_string(),
        agent_type: meta.agent_type.clone(),
        description: meta.description.clone(),
        model: meta.model.clone(),
        spawn_depth: meta.spawn_depth,
        tool_use_id: meta.tool_use_id.clone(),
        parent: match &meta.parent_agent_id {
            Some(p) => SubagentParent::Agent(p.clone()),
            None => SubagentParent::Session,
        },
        background: meta.request_shape.as_deref() == Some("background"),
        started_ms: entry.started_ms.unwrap_or(entry.meta_mtime),
        last_activity_ms,
        state,
        finished_ms: finished.map(|c| c.at_ms),
        transcript_path: entry.jsonl_path.clone(),
    };
    (info, signalled.is_some())
}

/// An agent with a running child is still running unless its parent saw a
/// completion signal: an agent can end its turn while background children
/// work, and its writes stop while it waits.
fn keep_parents_of_running_alive(infos: &mut [(SubagentInfo, bool)]) {
    loop {
        let running: HashSet<String> = infos
            .iter()
            .map(|(i, _)| i)
            .filter(|i| i.state == SubagentState::Running)
            .filter_map(|i| match &i.parent {
                SubagentParent::Agent(p) => Some(p.clone()),
                SubagentParent::Session => None,
            })
            .collect();
        let mut changed = false;
        for (info, signalled) in infos.iter_mut() {
            let revivable =
                matches!(info.state, SubagentState::Done | SubagentState::Stale) && !*signalled;
            if revivable && running.contains(&info.agent_id) {
                info.state = SubagentState::Running;
                info.finished_ms = None;
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// For a user/assistant record: `Some(Some(ts))` if it ends the turn,
/// `Some(None)` if the agent is (again) working. `None` for other records.
fn turn_record(line: &str) -> Option<Option<i64>> {
    if line.contains("\"type\":\"assistant\"") {
        if !line.contains("\"stop_reason\":\"end_turn\"") {
            return Some(None);
        }
        let value: Value = serde_json::from_str(line).ok()?;
        let at_ms = value.get("timestamp")?.as_str().and_then(parse_iso_ms)?;
        Some(Some(at_ms))
    } else if line.contains("\"type\":\"user\"") {
        Some(None)
    } else {
        None
    }
}

/// The `<session-id>/` directory, even if it appeared after `session` was built.
pub(super) fn session_dir(session: &SessionInfo) -> Option<PathBuf> {
    session.dir.clone().or_else(|| {
        let dir = session.transcript_path.with_extension("");
        dir.is_dir().then_some(dir)
    })
}

fn refresh_agents(cache: &mut SessionCache, subagents_dir: &Path, min_ms: i64) {
    let Ok(entries) = fs::read_dir(subagents_dir) else {
        cache.agents.clear();
        return;
    };
    let mut seen = HashSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(agent_id) = name
            .to_str()
            .and_then(|n| n.strip_prefix("agent-"))
            .and_then(|n| n.strip_suffix(".meta.json"))
        else {
            continue;
        };
        let meta_path = entry.path();
        let jsonl_path = subagents_dir.join(format!("agent-{agent_id}.jsonl"));
        let Some(meta_mtime) = mtime_ms(&meta_path) else {
            continue;
        };
        let jsonl_mtime = mtime_ms(&jsonl_path);
        if meta_mtime.max(jsonl_mtime.unwrap_or(i64::MIN)) < min_ms {
            continue;
        }
        seen.insert(agent_id.to_string());
        match cache.agents.get_mut(agent_id) {
            Some(e) if e.meta_mtime == meta_mtime => e.jsonl_mtime = jsonl_mtime,
            current => {
                let started_ms = current.and_then(|e| e.started_ms);
                let Some(meta) = read_meta(&meta_path) else {
                    continue;
                };
                cache.agents.insert(
                    agent_id.to_string(),
                    AgentEntry {
                        meta,
                        meta_mtime,
                        jsonl_path,
                        jsonl_mtime,
                        started_ms,
                    },
                );
            }
        }
        if let Some(e) = cache.agents.get_mut(agent_id)
            && e.started_ms.is_none()
            && e.jsonl_mtime.is_some()
        {
            e.started_ms = first_timestamp_ms(&e.jsonl_path);
        }
    }
    cache.agents.retain(|id, _| seen.contains(id));
}

fn read_meta(path: &Path) -> Option<AgentMeta> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn first_timestamp_ms(path: &Path) -> Option<i64> {
    let mut line = Vec::new();
    BufReader::new(File::open(path).ok()?)
        .read_until(b'\n', &mut line)
        .ok()?;
    let value: Value = serde_json::from_slice(&line).ok()?;
    parse_iso_ms(value.get("timestamp")?.as_str()?)
}

/// Feeds complete lines appended since `tail.offset` to `on_line`. A file that
/// shrank is re-read from the start.
pub(crate) fn tail_file(path: &Path, tail: &mut Tail, mut on_line: impl FnMut(&str)) {
    let Ok(file) = File::open(path) else {
        return;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return;
    };
    if len < tail.offset {
        tail.offset = 0;
    }
    if len == tail.offset {
        return;
    }
    let mut reader = BufReader::with_capacity(1 << 16, file);
    if reader.seek(SeekFrom::Start(tail.offset)).is_err() {
        return;
    }
    let mut line = Vec::new();
    loop {
        line.clear();
        let Ok(n) = reader.read_until(b'\n', &mut line) else {
            return;
        };
        if n == 0 || line.last() != Some(&b'\n') {
            return;
        }
        tail.offset += n as u64;
        if let Ok(text) = std::str::from_utf8(&line) {
            on_line(text);
        }
    }
}

/// Completion signals in one transcript line for the `wanted` tool-use ids.
fn completion_signals(line: &str, wanted: &HashSet<&str>) -> Vec<(String, Completion)> {
    let has_result = line.contains("\"tool_result\"");
    let has_notification = line.contains("<task-notification>");
    if !(has_result || has_notification) || !wanted.iter().any(|id| line.contains(id)) {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let at_ms = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_iso_ms)
        .unwrap_or(0);
    let mut out = Vec::new();
    match value.get("type").and_then(Value::as_str) {
        Some("queue-operation") => {
            if let Some(text) = value.get("content").and_then(Value::as_str) {
                notification_signals(text, wanted, at_ms, &mut out);
            }
        }
        Some("attachment") => {
            let prompt = value.pointer("/attachment/prompt").and_then(Value::as_str);
            if let Some(text) = prompt {
                notification_signals(text, wanted, at_ms, &mut out);
            }
        }
        Some("user") => {
            let content = value.pointer("/message/content");
            if let Some(text) = content.and_then(Value::as_str) {
                notification_signals(text, wanted, at_ms, &mut out);
            }
            let launched = value
                .pointer("/toolUseResult/status")
                .and_then(Value::as_str)
                == Some("async_launched");
            for item in content.and_then(Value::as_array).into_iter().flatten() {
                match item.get("type").and_then(Value::as_str) {
                    Some("tool_result") if !launched => {
                        let Some(id) = item.get("tool_use_id").and_then(Value::as_str) else {
                            continue;
                        };
                        if wanted.contains(id) {
                            let failed =
                                item.get("is_error").and_then(Value::as_bool) == Some(true);
                            let state = if failed {
                                SubagentState::Failed
                            } else {
                                SubagentState::Done
                            };
                            out.push((id.to_string(), Completion { state, at_ms }));
                        }
                    }
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            notification_signals(text, wanted, at_ms, &mut out);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

fn notification_signals(
    text: &str,
    wanted: &HashSet<&str>,
    at_ms: i64,
    out: &mut Vec<(String, Completion)>,
) {
    for block in text.split("<task-notification>").skip(1) {
        let block = block.split("</task-notification>").next().unwrap_or(block);
        let Some(id) = xml_field(block, "tool-use-id") else {
            continue;
        };
        if !wanted.contains(id) {
            continue;
        }
        let state = match xml_field(block, "status") {
            Some("completed" | "killed") => SubagentState::Done,
            Some("failed") => SubagentState::Failed,
            _ => continue,
        };
        out.push((id.to_string(), Completion { state, at_ms }));
    }
}

fn xml_field<'a>(block: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&format!("</{tag}>"))? + start;
    Some(block[start..end].trim())
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff]Z` (the transcript format) to epoch ms.
fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = hms.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (h, min, sec) = (t.next()??, t.next()??, t.next()??);
    let ms = if frac.is_empty() {
        0
    } else {
        let digits: String = frac.chars().chain("000".chars()).take(3).collect();
        digits.parse::<i64>().ok()?
    };
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + min) * 60_000 + sec * 1000 + ms)
}

fn mtime_ms(path: &Path) -> Option<i64> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(system_time_ms)
}

fn system_time_ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

/// [`SubagentScanner`] plus a `notify` watcher that marks sessions dirty, so
/// [`SubagentWatcher::refresh`] only re-reads sessions whose files changed.
/// Every session is still re-read each [`FULL_RESCAN_MS`] in case an event
/// was missed; without a working watcher every refresh re-reads everything.
pub struct SubagentWatcher {
    scanner: SubagentScanner,
    watcher: Option<RecommendedWatcher>,
    events: Receiver<PathBuf>,
    sender: Sender<PathBuf>,
    /// Watched path → recursive.
    watched: HashMap<PathBuf, bool>,
    dirty_paths: Vec<PathBuf>,
    scanned: HashSet<String>,
    last_full_ms: Option<i64>,
    /// Re-read everything on each refresh (no watcher available).
    poll_all: bool,
}

impl Default for SubagentWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl SubagentWatcher {
    pub fn new() -> Self {
        let (sender, events) = unbounded();
        let tx = sender.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                for path in event.paths {
                    let _ = tx.send(path);
                }
            }
        });
        if let Err(e) = &watcher {
            log::warn!("subagent watcher unavailable, polling instead: {e}");
        }
        let mut this = Self::without_notify(sender, events);
        this.poll_all = watcher.is_err();
        this.watcher = watcher.ok();
        this
    }

    /// A watcher that only sees [`SubagentWatcher::mark_dirty`] marks.
    fn without_notify(sender: Sender<PathBuf>, events: Receiver<PathBuf>) -> Self {
        Self {
            scanner: SubagentScanner::new(),
            watcher: None,
            events,
            sender,
            watched: HashMap::new(),
            dirty_paths: Vec::new(),
            scanned: HashSet::new(),
            last_full_ms: None,
            poll_all: false,
        }
    }

    /// Marks a path as changed, as a filesystem event would.
    pub fn mark_dirty(&self, path: impl Into<PathBuf>) {
        let _ = self.sender.send(path.into());
    }

    /// Subagents per session id for `sessions` as of `now_ms`.
    pub fn refresh(
        &mut self,
        sessions: &[SessionInfo],
        now_ms: i64,
    ) -> BTreeMap<String, Vec<SubagentInfo>> {
        self.dirty_paths.extend(self.events.try_iter());
        self.update_watches(sessions);
        let full = self.poll_all
            || self
                .last_full_ms
                .is_none_or(|t| now_ms - t >= FULL_RESCAN_MS || now_ms < t);
        if full {
            self.last_full_ms = Some(now_ms);
        }

        let dirty_paths = std::mem::take(&mut self.dirty_paths);
        self.scanner.retain_sessions(sessions);
        self.scanned
            .retain(|id| sessions.iter().any(|s| &s.session_id == id));
        for session in sessions {
            let stale = full
                || !self.scanned.contains(&session.session_id)
                || dirty_paths.iter().any(|p| touches(session, p));
            if stale {
                self.scanner.update_session(session, now_ms);
                self.scanned.insert(session.session_id.clone());
            }
        }
        self.scanner.snapshot(sessions, now_ms)
    }

    /// Watches each session's project dir (transcript writes, session dir
    /// creation) and its session dir recursively (subagent files).
    fn update_watches(&mut self, sessions: &[SessionInfo]) {
        let Some(watcher) = self.watcher.as_mut() else {
            return;
        };
        let mut wanted: HashMap<PathBuf, bool> = HashMap::new();
        for session in sessions {
            if let Some(project) = session.transcript_path.parent() {
                wanted.entry(project.to_path_buf()).or_insert(false);
            }
            if let Some(dir) = session_dir(session) {
                wanted.insert(dir, true);
            }
        }
        self.watched.retain(|path, recursive| {
            let keep = wanted.get(path) == Some(recursive);
            if !keep {
                let _ = watcher.unwatch(path);
            }
            keep
        });
        for (path, recursive) in wanted {
            if self.watched.contains_key(&path) {
                continue;
            }
            let mode = if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            match watcher.watch(&path, mode) {
                Ok(()) => {
                    self.watched.insert(path, recursive);
                }
                Err(e) => log::debug!("cannot watch {}: {e}", path.display()),
            }
        }
    }
}

/// Whether a changed path can affect `session`'s subagents. Event paths may
/// be canonical (`/private/var/…` for `/var/…`), so both forms are compared.
fn touches(session: &SessionInfo, changed: &Path) -> bool {
    let transcript = &session.transcript_path;
    let dir = transcript.with_extension("");
    let project = transcript.parent();
    let check = |t: &Path, d: &Path, p: Option<&Path>| {
        changed == t || changed.starts_with(d) || Some(changed) == p
    };
    if check(transcript, &dir, project) {
        return true;
    }
    let canon = |p: &Path| fs::canonicalize(p).ok();
    match (canon(transcript), project.and_then(canon)) {
        (Some(t), Some(p)) => {
            let d = t.with_extension("");
            check(&t, &d, Some(&p))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;
    use tempfile::TempDir;

    const SESSION: &str = "11111111-2222-3333-4444-555555555555";
    const MIN_MS: i64 = 60 * 1000;
    const HOUR_MS: i64 = 60 * MIN_MS;

    struct Fake {
        _tmp: TempDir,
        project: PathBuf,
        now_ms: i64,
    }

    impl Fake {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let project = tmp.path().join("-Users-me-repo");
            fs::create_dir_all(project.join(SESSION).join("subagents")).unwrap();
            fs::write(project.join(format!("{SESSION}.jsonl")), "").unwrap();
            Self {
                _tmp: tmp,
                project,
                now_ms: parse_iso_ms("2026-09-25T10:10:00Z").unwrap(),
            }
        }

        fn session(&self) -> SessionInfo {
            SessionInfo {
                session_id: SESSION.into(),
                transcript_path: self.project.join(format!("{SESSION}.jsonl")),
                dir: Some(self.project.join(SESSION)),
                modified_ms: self.now_ms,
                cwd_from_transcript: None,
            }
        }

        fn subagents(&self) -> PathBuf {
            self.project.join(SESSION).join("subagents")
        }

        fn agent(&self, id: &str, meta: Value) -> PathBuf {
            fs::write(
                self.subagents().join(format!("agent-{id}.meta.json")),
                meta.to_string(),
            )
            .unwrap();
            let jsonl = self.subagents().join(format!("agent-{id}.jsonl"));
            let first = serde_json::json!({
                "type": "user", "agentId": id, "isSidechain": true,
                "timestamp": "2026-09-25T10:00:00.250Z",
                "message": {"role": "user", "content": "do the thing"},
            });
            fs::write(&jsonl, format!("{first}\n")).unwrap();
            let written = parse_iso_ms("2026-09-25T10:04:00Z").unwrap();
            set_mtime(&jsonl, written);
            set_mtime(
                &self.subagents().join(format!("agent-{id}.meta.json")),
                written,
            );
            jsonl
        }

        fn append(&self, path: &Path, line: &Value) {
            let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
            writeln!(f, "{line}").unwrap();
        }

        fn scan(&self, scanner: &mut SubagentScanner) -> Vec<SubagentInfo> {
            self.scan_at(scanner, self.now_ms)
        }

        fn scan_at(&self, scanner: &mut SubagentScanner, now_ms: i64) -> Vec<SubagentInfo> {
            let mut map = scanner.scan(&[self.session()], now_ms);
            map.remove(SESSION).unwrap()
        }
    }

    fn set_mtime(path: &Path, ms: i64) {
        let t = UNIX_EPOCH + Duration::from_millis(ms as u64);
        File::options()
            .append(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn meta(tool_use_id: &str) -> Value {
        serde_json::json!({
            "agentType": "general-purpose",
            "description": "Simplify: reuse angle",
            "toolUseId": tool_use_id,
            "spawnDepth": 1,
            "requestShape": "background",
            "requestNonInteractive": true,
            "model": "opus",
        })
    }

    fn tool_result(tool_use_id: &str, status: Option<&str>, is_error: bool) -> Value {
        let mut line = serde_json::json!({
            "type": "user",
            "timestamp": "2026-09-25T10:05:00.000Z",
            "message": {"role": "user", "content": [
                {"tool_use_id": tool_use_id, "type": "tool_result", "is_error": is_error,
                 "content": [{"type": "text", "text": "report"}]}
            ]},
        });
        if let Some(status) = status {
            line["toolUseResult"] = serde_json::json!({"isAsync": true, "status": status});
        }
        line
    }

    fn notification(tool_use_id: &str, status: &str, queue: bool) -> Value {
        let text = format!(
            "<task-notification>\n<task-id>a1</task-id>\n<tool-use-id>{tool_use_id}</tool-use-id>\n\
             <status>{status}</status>\n<summary>Agent finished</summary>\n</task-notification>"
        );
        if queue {
            serde_json::json!({"type": "queue-operation", "operation": "enqueue",
                "timestamp": "2026-09-25T10:06:00.000Z", "content": text})
        } else {
            serde_json::json!({"type": "user", "isMeta": true,
                "origin": {"kind": "task-notification"},
                "timestamp": "2026-09-25T10:06:00.000Z",
                "message": {"role": "user", "content": text}})
        }
    }

    #[test]
    fn discovers_subagents_from_meta_files() {
        let fake = Fake::new();
        let jsonl = fake.agent("a1", meta("toolu_1"));
        fs::write(fake.subagents().join("agent-a1.forked-skill.json"), "{}").unwrap();
        let mut scanner = SubagentScanner::new();
        let agents = fake.scan(&mut scanner);
        assert_eq!(agents.len(), 1);
        let a = &agents[0];
        assert_eq!(a.agent_id, "a1");
        assert_eq!(a.session_id, SESSION);
        assert_eq!(a.agent_type, "general-purpose");
        assert_eq!(a.description, "Simplify: reuse angle");
        assert_eq!(a.model.as_deref(), Some("opus"));
        assert_eq!(a.spawn_depth, 1);
        assert_eq!(a.tool_use_id.as_deref(), Some("toolu_1"));
        assert_eq!(a.parent, SubagentParent::Session);
        assert!(a.background);
        assert_eq!(
            a.started_ms,
            parse_iso_ms("2026-09-25T10:00:00.250Z").unwrap()
        );
        assert_eq!(a.last_activity_ms, mtime_ms(&jsonl).unwrap());
        assert_eq!(a.state, SubagentState::Running);
        assert_eq!(a.transcript_path, jsonl);
    }

    #[test]
    fn missing_session_dir_yields_nothing() {
        let fake = Fake::new();
        let mut session = fake.session();
        session.dir = None;
        fs::remove_dir_all(fake.project.join(SESSION)).unwrap();
        let mut scanner = SubagentScanner::new();
        assert!(scanner.scan(&[session], fake.now_ms)[SESSION].is_empty());
    }

    #[test]
    fn session_dir_created_later_is_found() {
        let fake = Fake::new();
        let mut session = fake.session();
        session.dir = None;
        fake.agent("a1", meta("toolu_1"));
        let mut scanner = SubagentScanner::new();
        assert_eq!(scanner.scan(&[session], fake.now_ms)[SESSION].len(), 1);
    }

    #[test]
    fn nested_parent_and_done_via_parent_agent_transcript() {
        let fake = Fake::new();
        let parent_jsonl = fake.agent("parent", meta("toolu_p"));
        let mut child = meta("toolu_c");
        child["parentAgentId"] = "parent".into();
        child["spawnDepth"] = 2.into();
        fake.agent("child", child);

        let mut scanner = SubagentScanner::new();
        let agents = fake.scan(&mut scanner);
        let child = agents.iter().find(|a| a.agent_id == "child").unwrap();
        assert_eq!(child.parent, SubagentParent::Agent("parent".into()));
        assert_eq!(child.spawn_depth, 2);
        assert_eq!(child.state, SubagentState::Running);

        fake.append(
            &parent_jsonl,
            &tool_result("toolu_c", Some("async_launched"), false),
        );
        let agents = fake.scan(&mut scanner);
        assert!(agents.iter().all(|a| a.state == SubagentState::Running));

        fake.append(&parent_jsonl, &notification("toolu_c", "completed", false));
        let agents = fake.scan(&mut scanner);
        let child = agents.iter().find(|a| a.agent_id == "child").unwrap();
        let parent = agents.iter().find(|a| a.agent_id == "parent").unwrap();
        assert_eq!(child.state, SubagentState::Done);
        assert_eq!(parent.state, SubagentState::Running);
    }

    #[test]
    fn done_via_tool_result_appended_later_reads_incrementally() {
        let fake = Fake::new();
        let session = fake.session();
        fake.agent("a1", meta("toolu_1"));
        let filler =
            serde_json::json!({"type": "assistant", "message": {"content": "x".repeat(4096)}});
        for _ in 0..8 {
            fake.append(&session.transcript_path, &filler);
        }

        let mut scanner = SubagentScanner::new();
        assert_eq!(fake.scan(&mut scanner)[0].state, SubagentState::Running);
        let offset = scanner.sessions[SESSION].tails[&session.transcript_path].offset;
        assert_eq!(
            offset,
            fs::metadata(&session.transcript_path).unwrap().len()
        );

        // A partial line is not consumed until its newline arrives.
        let result = tool_result("toolu_1", None, false).to_string();
        let (head, rest) = result.split_at(40);
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&session.transcript_path)
            .unwrap();
        write!(f, "{head}").unwrap();
        assert_eq!(fake.scan(&mut scanner)[0].state, SubagentState::Running);
        assert_eq!(
            scanner.sessions[SESSION].tails[&session.transcript_path].offset,
            offset
        );

        writeln!(f, "{rest}").unwrap();
        let agents = fake.scan(&mut scanner);
        assert_eq!(agents[0].state, SubagentState::Done);
        assert_eq!(agents[0].finished_ms, parse_iso_ms("2026-09-25T10:05:00Z"));

        // Truncating resets the offset; the cached signal stays.
        fs::write(&session.transcript_path, "").unwrap();
        assert_eq!(fake.scan(&mut scanner)[0].state, SubagentState::Done);
        assert_eq!(
            scanner.sessions[SESSION].tails[&session.transcript_path].offset,
            0
        );
    }

    #[test]
    fn error_result_and_failed_notification_mean_failed() {
        let fake = Fake::new();
        let session = fake.session();
        for (id, tool) in [("a1", "toolu_1"), ("a2", "toolu_2"), ("a3", "toolu_3")] {
            fake.agent(id, meta(tool));
        }
        fake.append(
            &session.transcript_path,
            &tool_result("toolu_1", None, true),
        );
        fake.append(
            &session.transcript_path,
            &notification("toolu_2", "failed", true),
        );
        fake.append(
            &session.transcript_path,
            &notification("toolu_3", "killed", true),
        );
        let mut scanner = SubagentScanner::new();
        let states: Vec<_> = fake.scan(&mut scanner).iter().map(|a| a.state).collect();
        assert_eq!(
            states,
            [
                SubagentState::Failed,
                SubagentState::Failed,
                SubagentState::Done
            ]
        );
    }

    #[test]
    fn writes_after_completion_mean_resumed() {
        let fake = Fake::new();
        let session = fake.session();
        let jsonl = fake.agent("a1", meta("toolu_1"));
        fake.append(
            &session.transcript_path,
            &notification("toolu_1", "completed", true),
        );
        let done_ms = parse_iso_ms("2026-09-25T10:06:00Z").unwrap();
        let mut scanner = SubagentScanner::new();

        set_mtime(&jsonl, done_ms - MIN_MS);
        let now = done_ms + MIN_MS;
        assert_eq!(
            fake.scan_at(&mut scanner, now)[0].state,
            SubagentState::Done
        );

        set_mtime(&jsonl, done_ms + 30_000);
        assert_eq!(
            fake.scan_at(&mut scanner, now)[0].state,
            SubagentState::Running
        );

        fake.append(&session.transcript_path, &{
            let mut n = notification("toolu_1", "completed", true);
            n["timestamp"] = "2026-09-25T10:07:00Z".into();
            n
        });
        assert_eq!(
            fake.scan_at(&mut scanner, now)[0].state,
            SubagentState::Done
        );
    }

    fn end_turn(at: &str) -> Value {
        serde_json::json!({"type": "assistant", "timestamp": at,
            "message": {"role": "assistant", "stop_reason": "end_turn",
                "content": [{"type": "text", "text": "final report"}]}})
    }

    #[test]
    fn attachment_notification_means_done() {
        let fake = Fake::new();
        let session = fake.session();
        fake.agent("a1", meta("toolu_1"));
        let text = notification("toolu_1", "completed", true)["content"].clone();
        let line = serde_json::json!({"type": "attachment",
            "timestamp": "2026-09-25T10:06:00.000Z",
            "attachment": {"type": "queued_command", "commandMode": "task-notification",
                "prompt": text}});
        fake.append(&session.transcript_path, &line);
        let mut scanner = SubagentScanner::new();
        assert_eq!(fake.scan(&mut scanner)[0].state, SubagentState::Done);
    }

    #[test]
    fn own_end_turn_means_done_until_the_next_message() {
        let fake = Fake::new();
        let jsonl = fake.agent("a1", meta("toolu_1"));
        let mut scanner = SubagentScanner::new();
        fake.append(&jsonl, &end_turn("2026-09-25T10:08:00.000Z"));
        let agents = fake.scan(&mut scanner);
        assert_eq!(agents[0].state, SubagentState::Done);
        assert_eq!(agents[0].finished_ms, parse_iso_ms("2026-09-25T10:08:00Z"));

        let resumed = serde_json::json!({"type": "user", "timestamp": "2026-09-25T10:09:00Z",
            "message": {"role": "user", "content": "The coordinator sent a message"}});
        fake.append(&jsonl, &resumed);
        assert_eq!(fake.scan(&mut scanner)[0].state, SubagentState::Running);
    }

    #[test]
    fn running_child_keeps_idle_parent_running() {
        let fake = Fake::new();
        let parent = fake.agent("parent", meta("toolu_p"));
        fake.append(&parent, &end_turn("2026-09-25T10:04:30.000Z"));
        set_mtime(&parent, fake.now_ms - 40 * MIN_MS);
        let mut child = meta("toolu_c");
        child["parentAgentId"] = "parent".into();
        let child_jsonl = fake.agent("child", child);
        set_mtime(&child_jsonl, fake.now_ms - MIN_MS);

        let mut scanner = SubagentScanner::new();
        let states: Vec<_> = fake.scan(&mut scanner).iter().map(|a| a.state).collect();
        assert_eq!(states, [SubagentState::Running, SubagentState::Running]);

        set_mtime(&child_jsonl, fake.now_ms - 31 * MIN_MS);
        let agents = fake.scan(&mut scanner);
        let parent = agents.iter().find(|a| a.agent_id == "parent").unwrap();
        assert_eq!(parent.state, SubagentState::Done);
    }

    #[test]
    fn inactive_without_result_is_stale() {
        let fake = Fake::new();
        let jsonl = fake.agent("a1", meta("toolu_1"));
        let mut scanner = SubagentScanner::new();
        set_mtime(&jsonl, fake.now_ms - 29 * MIN_MS);
        assert_eq!(fake.scan(&mut scanner)[0].state, SubagentState::Running);
        set_mtime(&jsonl, fake.now_ms - 31 * MIN_MS);
        let agents = fake.scan(&mut scanner);
        assert_eq!(agents[0].state, SubagentState::Stale);
        assert!(agents[0].state.is_finished());
    }

    #[test]
    fn ignores_subagents_older_than_a_day() {
        let fake = Fake::new();
        let old = fake.agent("old", meta("toolu_1"));
        let recent_meta_only = fake.agent("fresh", meta("toolu_2"));
        let day_ago = fake.now_ms - 25 * HOUR_MS;
        set_mtime(&old, day_ago);
        set_mtime(&fake.subagents().join("agent-old.meta.json"), day_ago);
        set_mtime(&recent_meta_only, day_ago);

        let mut scanner = SubagentScanner::new();
        let ids: Vec<_> = fake
            .scan(&mut scanner)
            .into_iter()
            .map(|a| a.agent_id)
            .collect();
        assert_eq!(ids, ["fresh"]);

        // Aging past the window without new I/O drops it from snapshots.
        let later = fake.now_ms + 25 * HOUR_MS;
        assert!(scanner.snapshot(&[fake.session()], later)[SESSION].is_empty());
    }

    #[test]
    fn meta_without_tool_use_id_is_kept() {
        let fake = Fake::new();
        fake.agent(
            "named",
            serde_json::json!({"agentType": "worker", "description": "d", "name": "n", "spawnDepth": 1}),
        );
        let mut scanner = SubagentScanner::new();
        let agents = fake.scan(&mut scanner);
        assert_eq!(agents[0].tool_use_id, None);
        assert!(!agents[0].background);
        assert_eq!(agents[0].state, SubagentState::Running);
    }

    #[test]
    fn notifications_only_count_for_matching_ids_and_final_statuses() {
        let wanted: HashSet<&str> = ["toolu_1"].into_iter().collect();
        let running = notification("toolu_1", "running", true).to_string();
        assert!(completion_signals(&running, &wanted).is_empty());
        let other = notification("toolu_9", "completed", true).to_string();
        assert!(completion_signals(&other, &wanted).is_empty());
        let assistant = serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "<task-notification><tool-use-id>toolu_1</tool-use-id><status>completed</status></task-notification>"}
        ]}})
        .to_string();
        assert!(completion_signals(&assistant, &wanted).is_empty());
    }

    #[test]
    fn parses_transcript_timestamps() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso_ms("2026-08-27T23:24:44.700Z"),
            Some(1_787_873_084_700)
        );
        assert_eq!(
            parse_iso_ms("2000-02-29T12:00:00.5Z"),
            Some(951_825_600_500)
        );
        assert_eq!(parse_iso_ms("garbage"), None);
    }

    #[test]
    fn watcher_rescans_only_dirty_sessions() {
        let fake = Fake::new();
        let sessions = [fake.session()];
        fake.agent("a1", meta("toolu_1"));
        let (tx, rx) = unbounded();
        let mut watcher = SubagentWatcher::without_notify(tx, rx);
        let now = fake.now_ms;
        assert_eq!(watcher.refresh(&sessions, now)[SESSION].len(), 1);

        fake.agent("a2", meta("toolu_2"));
        assert_eq!(watcher.refresh(&sessions, now + 1000)[SESSION].len(), 1);
        watcher.mark_dirty(fake.project.join("unrelated.jsonl"));
        assert_eq!(watcher.refresh(&sessions, now + 1500)[SESSION].len(), 1);
        watcher.mark_dirty(fake.subagents().join("agent-a2.meta.json"));
        assert_eq!(watcher.refresh(&sessions, now + 2000)[SESSION].len(), 2);

        fake.agent("a3", meta("toolu_3"));
        let later = now + FULL_RESCAN_MS + 3000;
        assert_eq!(watcher.refresh(&sessions, later)[SESSION].len(), 3);
    }

    /// Scans subagents of the sessions mapped from the live `orca terminal
    /// list`. Run with `cargo test -- --ignored live_ --nocapture`.
    #[test]
    #[ignore]
    fn live_subagents_of_orca_terminals() {
        use crate::claude::sessions::SessionLocator;
        use crate::orca::OrcaClient;

        let terminals = OrcaClient::new().terminal_list().unwrap().terminals;
        let map = SessionLocator::default().map_terminals(&terminals, None);
        let mut sessions: Vec<SessionInfo> =
            map.values().filter_map(|m| m.session().cloned()).collect();
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        sessions.dedup_by(|a, b| a.session_id == b.session_id);

        let now = system_time_ms(SystemTime::now());
        let mut watcher = SubagentWatcher::new();
        let started = std::time::Instant::now();
        let result = watcher.refresh(&sessions, now);
        let first = started.elapsed();
        let started = std::time::Instant::now();
        let again = watcher.refresh(&sessions, now + 1000);
        let second = started.elapsed();
        assert_eq!(result, again);

        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for (session, agents) in &result {
            let running = agents
                .iter()
                .filter(|a| a.state == SubagentState::Running)
                .count();
            println!("{session}: {} subagents, {running} running", agents.len());
            for a in agents {
                let key = match a.state {
                    SubagentState::Running => "running",
                    SubagentState::Done => "done",
                    SubagentState::Failed => "failed",
                    SubagentState::Stale => "stale",
                };
                *counts.entry(key).or_default() += 1;
                if a.state == SubagentState::Running {
                    println!(
                        "  running {} (depth {}) {}",
                        a.agent_id, a.spawn_depth, a.description
                    );
                }
            }
        }
        println!(
            "sessions: {}, subagents: {counts:?}; first refresh {first:?}, second {second:?}",
            sessions.len()
        );
    }
}

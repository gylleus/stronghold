//! Bridge ingestion and Slack CLI I/O. Called only on the Envoy worker thread.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::BridgeStatus;
use super::channel::{Channel, EventKind, InboundEvent, OutboundMessage, PollBatch, SendReceipt};
use super::inbox::{Cursor, Inbox, timestamp_us};
use super::settings::EnvoySettings;
use serde_json::Value;

/// `gh` lookups per poll, so a burst of refs can't stall ingestion.
const MAX_PR_LOOKUPS: usize = 4;
const POLL_BUDGET: Duration = Duration::from_secs(60);

thread_local! {
    static CLI_DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

fn cli_budget() -> Result<Duration, String> {
    CLI_DEADLINE
        .get()
        .map_or(Ok(Duration::from_secs(25)), |at| {
            at.checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .map(|left| left.min(Duration::from_secs(25)))
                .ok_or_else(|| "Poll deadline reached; remaining work deferred".into())
        })
}

pub trait SlackSender: Send {
    fn send(&mut self, message: &OutboundMessage) -> Result<SendReceipt, String>;
}

pub trait SlackIo: SlackSender {
    fn set_deadline(&mut self, _deadline: Option<Instant>) {}
    fn thread_subscription(
        &mut self,
        bridge: &Path,
        thread: &super::requests::Thread,
        label: Option<&str>,
    ) -> Result<(), String> {
        let _ = (bridge, thread, label);
        Err("Thread subscriptions unavailable".into())
    }
    fn subscriptions(&mut self, bridge: &Path) -> Result<String, String> {
        let _ = bridge;
        Err("Subscription listing unavailable".into())
    }
    /// The head branch of PR `owner/repo#number`; `Ok(None)` when there is no such PR.
    fn pr_head(&mut self, owner: &str, repo: &str, number: u64) -> Result<Option<String>, String> {
        let _ = (owner, repo, number);
        Err("PR lookup unavailable".into())
    }
    fn api(&mut self, method: &str, params: &[(&str, String)]) -> Result<Value, String>;
    fn subscribe(&mut self, bridge_dir: &Path, conversation: &str) -> Result<(), String>;
}

#[derive(Default)]
pub struct SlackCli;

fn binary(name: &str) -> Result<PathBuf, String> {
    let env = format!("STRONGHOLD_{}", name.to_ascii_uppercase().replace('-', "_"));
    if let Some(path) = crate::legacy::env_os(&env).filter(|p| !p.is_empty()) {
        return Ok(path.into());
    }
    let paths = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".local/bin").join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("{name} is not installed or not on PATH"))
}

fn run(name: &str, args: &[String], bridge: Option<&Path>) -> Result<String, String> {
    let timeout = cli_budget()?;
    let executable = binary(name)?;
    // Finder lacks shell exports; zsh reads the existing noninteractive environment.
    let mut command = Command::new("/bin/zsh");
    command
        .args(["-c", "exec \"$@\"", "envoy"])
        .arg(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dir) = bridge {
        command.env("SLACK_BRIDGE_DIR", dir);
    }
    let mut child = command
        .spawn()
        .map_err(|_| format!("Could not start {name}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        out
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{name} timed out or failed to wait"));
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "{name} failed (check CLI authentication and connectivity)"
        ));
    }
    Ok(out)
}

impl SlackIo for SlackCli {
    fn set_deadline(&mut self, deadline: Option<Instant>) {
        CLI_DEADLINE.set(deadline);
    }
    fn pr_head(&mut self, owner: &str, repo: &str, number: u64) -> Result<Option<String>, String> {
        use crate::review::github::{GH_TIMEOUT, GhCli, GhError};
        if CLI_DEADLINE
            .get()
            .is_some_and(|at| at.saturating_duration_since(Instant::now()) < GH_TIMEOUT)
        {
            return Err("PR lookup deferred to keep poll deadline".into());
        }
        match GhCli::default().head_branch(owner, repo, number) {
            Ok(branch) => Ok(Some(branch)),
            Err(GhError::Failed(_)) => Ok(None),
            Err(GhError::Unavailable(e)) => Err(e),
        }
    }
    fn thread_subscription(
        &mut self,
        bridge: &Path,
        thread: &super::requests::Thread,
        label: Option<&str>,
    ) -> Result<(), String> {
        validate_conversation(&thread.conversation)?;
        validate_ts(&thread.ts)?;
        let mut args = vec![
            if label.is_some() { "add" } else { "remove" }.into(),
            thread.conversation.clone(),
            thread.ts.clone(),
        ];
        if let Some(label) = label {
            args.extend(["--label".into(), label.into()]);
        }
        run("slack-sub", &args, Some(bridge)).map(|_| ())
    }
    fn subscriptions(&mut self, bridge: &Path) -> Result<String, String> {
        run("slack-sub", &["list".into()], Some(bridge))
    }
    fn api(&mut self, method: &str, params: &[(&str, String)]) -> Result<Value, String> {
        let args: Vec<String> = ["api".to_owned(), method.to_owned()]
            .into_iter()
            .chain(params.iter().map(|(k, v)| format!("{k}={v}")))
            .collect();
        let output = run("slack", &args, None)?;
        parse_api(&output)
    }
    fn subscribe(&mut self, bridge_dir: &Path, conversation: &str) -> Result<(), String> {
        validate_conversation(conversation)?;
        run(
            "slack-sub",
            &[
                "add-channel".into(),
                conversation.into(),
                "--label".into(),
                "Envoy".into(),
            ],
            Some(bridge_dir),
        )
        .map(|_| ())
    }
}

impl SlackSender for SlackCli {
    fn send(&mut self, message: &OutboundMessage) -> Result<SendReceipt, String> {
        validate_conversation(&message.conversation)?;
        super::secrets::scan(&message.text)?;
        if ![
            super::drafts::attribution(),
            super::drafts::auto_attribution(),
        ]
        .iter()
        .any(|a| message.text.ends_with(a))
        {
            return Err("Outbound attribution is required".into());
        }
        let mut params = vec![
            ("channel", message.conversation.clone()),
            ("text", message.text.clone()),
        ];
        if let Some(thread) = &message.thread_ts {
            validate_ts(thread)?;
            params.push(("thread_ts", thread.clone()));
        }
        let value = self.api("chat.postMessage", &params)?;
        let receipt = SendReceipt {
            conversation: string(&value, "channel"),
            ts: string(&value, "ts"),
        };
        validate_conversation(&receipt.conversation)?;
        validate_ts(&receipt.ts)?;
        if receipt.conversation != message.conversation {
            return Err("Slack receipt channel differs; outcome unknown".into());
        }
        Ok(receipt)
    }
}

pub fn parse_api(output: &str) -> Result<Value, String> {
    let start = output.find('{').ok_or("Slack returned no JSON")?;
    let value: Value =
        serde_json::from_str(&output[start..]).map_err(|_| "Slack returned invalid JSON")?;
    if value["ok"] != true {
        let code = value["error"].as_str().unwrap_or("unknown_error");
        let safe: String = code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(80)
            .collect();
        return Err(format!("Slack API: {safe}"));
    }
    Ok(value)
}

pub fn validate_conversation(s: &str) -> Result<(), String> {
    if !matches!(s.as_bytes().first(), Some(b'C' | b'D' | b'G'))
        || s.len() < 2
        || !s.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err("Expected a Slack conversation ID".into());
    }
    Ok(())
}
/// A Slack member id: `U…` or `W…` (enterprise), alphanumeric.
pub fn is_member_id(s: &str) -> bool {
    matches!(s.as_bytes().first(), Some(b'U' | b'W' | b'u' | b'w'))
        && s.len() >= 2
        && s.bytes().all(|b| b.is_ascii_alphanumeric())
}
pub fn validate_ts(s: &str) -> Result<(), String> {
    if timestamp_us(s).is_none_or(|t| t <= 0) {
        return Err("Invalid Slack timestamp".into());
    }
    Ok(())
}

pub fn bridge_dir() -> PathBuf {
    std::env::var_os("SLACK_BRIDGE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".claude/slack-subscriptions")
        })
}

pub fn heartbeat(path: &Path, now_ms: i64) -> BridgeStatus {
    let Ok(text) = fs::read_to_string(path.join("heartbeat")) else {
        return BridgeStatus::Missing;
    };
    let Ok(at) = text.trim().parse::<jiff::Timestamp>() else {
        return BridgeStatus::Stale;
    };
    if now_ms.saturating_sub(at.as_millisecond()) > 90_000 || at.as_millisecond() > now_ms + 30_000
    {
        BridgeStatus::Stale
    } else {
        BridgeStatus::Ok
    }
}

#[derive(Debug)]
pub enum BridgeLine {
    Event(InboundEvent),
    Reaction,
    Error(String),
    Warning(String),
    Unknown,
}

fn string(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}
fn opt_string(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

fn event(v: &Value, conversation: &str, kind: EventKind) -> Result<InboundEvent, String> {
    validate_conversation(conversation)?;
    let ts = string(v, "ts");
    validate_ts(&ts)?;
    let permalink = opt_string(v, "permalink");
    let thread_ts = opt_string(v, "thread_ts").or_else(|| {
        permalink.as_ref().and_then(|p| {
            p.split_once("thread_ts=")
                .map(|(_, s)| s.split('&').next().unwrap_or(s).to_owned())
        })
    });
    if let Some(ts) = &thread_ts {
        validate_ts(ts)?;
    }
    let user = string(v, "user");
    let user_name = opt_string(v, "user_name")
        .or_else(|| opt_string(v, "username"))
        .unwrap_or_else(|| user.clone());
    Ok(InboundEvent {
        channel_kind: "slack".into(),
        conversation: conversation.into(),
        ts,
        thread_ts,
        user,
        user_name,
        text: string(v, "text"),
        kind,
        permalink,
    })
}

pub fn parse_bridge_line(line: &[u8]) -> Result<BridgeLine, String> {
    let v: Value = serde_json::from_slice(line).map_err(|_| "Malformed bridge JSON line")?;
    Ok(match v["type"].as_str() {
        Some("message" | "reply" | "catchup") => {
            let kind = if v["thread_ts"].is_string() && v["thread_ts"] != v["ts"] {
                EventKind::ThreadReply
            } else {
                EventKind::AgentComms
            };
            BridgeLine::Event(event(&v, &string(&v, "channel"), kind)?)
        }
        Some("reaction") => BridgeLine::Reaction,
        Some("error") => BridgeLine::Error(string(&v, "reason")),
        Some("warning") => BridgeLine::Warning(string(&v, "reason")),
        _ => BridgeLine::Unknown,
    })
}

pub fn parse_search(
    value: &Value,
    kind: EventKind,
    user_id: &str,
) -> Result<Vec<InboundEvent>, String> {
    let matches = value["messages"]["matches"]
        .as_array()
        .ok_or("Slack search is missing messages.matches")?;
    matches
        .iter()
        .filter(|v| v["user"] != user_id)
        .filter(|v| {
            kind != EventKind::Dm
                || v["channel"]["is_im"] == true
                || v["channel"]["is_mpim"] == true
        })
        .map(|v| event(v, v["channel"]["id"].as_str().unwrap_or_default(), kind))
        .collect()
}

pub struct SlackChannel<I: SlackIo> {
    pub io: I,
    pub inbox: Inbox,
    path: PathBuf,
    bridge: PathBuf,
    settings: EnvoySettings,
    status: BridgeStatus,
    errors: BTreeMap<String, String>,
    poll_errors: BTreeMap<String, String>,
    command_errors: BTreeMap<String, String>,
    read_errors: BTreeMap<String, String>,
    subscribed: Vec<String>,
    next_jobs: BTreeMap<String, i64>,
    next_job: usize,
    deadline: Option<Instant>,
    last_tick_ms: Option<i64>,
    api_backoff: BTreeMap<String, Instant>,
    groups_unavailable: bool,
    groups_error_logged: bool,
    needs_save: bool,
}

impl<I: SlackIo> SlackChannel<I> {
    pub fn new(
        io: I,
        inbox: Inbox,
        path: PathBuf,
        bridge: PathBuf,
        settings: EnvoySettings,
    ) -> Self {
        let errors = inbox
            .subscription_errors
            .iter()
            .filter(|(_, error)| {
                error.as_str() == "Bridge subscription error; check channel membership"
            })
            .map(|(c, e)| (c.clone(), e.clone()))
            .collect();
        Self {
            io,
            inbox,
            path,
            bridge,
            settings,
            status: BridgeStatus::Missing,
            errors,
            poll_errors: BTreeMap::new(),
            command_errors: BTreeMap::new(),
            read_errors: BTreeMap::new(),
            subscribed: Vec::new(),
            next_jobs: BTreeMap::new(),
            next_job: 0,
            deadline: None,
            last_tick_ms: None,
            api_backoff: BTreeMap::new(),
            groups_unavailable: false,
            groups_error_logged: false,
            needs_save: false,
        }
    }

    /// The user's member id as Slack reports it (`auth.test`, what `slackmsg whoami` prints). A
    /// resolved id replaces the configured one for drops, searches and the addressee check;
    /// `Ok(None)` means Slack gave none and the configured id stands.
    pub fn resolve_user_id(&mut self) -> Result<Option<String>, String> {
        let value = self.api("auth.test", &[])?;
        let Some(id) = value["user_id"].as_str().map(str::trim) else {
            return Ok(None);
        };
        if !is_member_id(id) {
            return Err(format!("auth.test gave an invalid user id {id:?}"));
        }
        let id = id.to_ascii_uppercase();
        if self.settings.user_id != id {
            self.inbox.user_groups.clear();
            self.next_jobs.remove("groups");
            self.needs_save = true;
        }
        self.settings.user_id = id.clone();
        Ok(Some(id))
    }

    pub fn configure(&mut self, settings: EnvoySettings) {
        self.subscribed.clear();
        self.errors.retain(|c, _| settings.channels.contains(c));
        self.command_errors
            .retain(|c, _| settings.channels.contains(c));
        self.read_errors
            .retain(|c, _| settings.channels.contains(c));
        if self.settings.user_id != settings.user_id {
            self.inbox.user_groups.clear();
            self.needs_save = true;
        }
        self.settings = settings;
        self.next_jobs.clear();
        self.groups_unavailable = false;
        self.groups_error_logged = false;
    }

    fn remember_own_thread(&mut self, e: &InboundEvent) {
        if e.user == self.settings.user_id {
            let root = e.thread_ts.as_deref().unwrap_or(&e.ts);
            self.inbox
                .user_threads
                .insert(format!("{}:{root}", e.conversation));
        }
    }

    fn ingest(&mut self, e: InboundEvent, batch: &mut PollBatch) -> bool {
        if self.inbox.is_deleted(&e.id()) {
            return true;
        }
        self.remember_own_thread(&e);
        if e.user != self.settings.user_id && !self.capture_reply(&e) {
            return false;
        }
        let fresh = self
            .inbox
            .first_enabled_ms
            .is_none_or(|start| timestamp_us(&e.ts).is_some_and(|ts| ts / 1000 >= start));
        if fresh && e.user != self.settings.user_id && self.inbox.insert(e.clone()) {
            batch.events.push(e);
        }
        true
    }

    fn tail(&mut self, channel: &str, batch: &mut PollBatch) -> Result<bool, String> {
        validate_conversation(channel)?;
        let path = self
            .bridge
            .join("channels")
            .join(format!("{channel}.jsonl"));
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err("Cannot read bridge channel feed".into()),
        };
        let meta = file.metadata().map_err(|e| e.to_string())?;
        let mut cursor = self.inbox.cursors.get(channel).cloned().unwrap_or_default();
        if cursor.inode != meta.ino() || cursor.offset > meta.len() {
            cursor = Cursor {
                inode: meta.ino(),
                offset: 0,
            };
        }
        file.seek(SeekFrom::Start(cursor.offset))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.take(4 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let Some(end) = bytes.iter().rposition(|b| *b == b'\n') else {
            return Ok(cursor.offset == meta.len());
        };
        let mut consumed = 0;
        for line in bytes[..=end].split_inclusive(|b| *b == b'\n') {
            let parsed = parse_bridge_line(line);
            if matches!(&parsed, Ok(record) if !matches!(record, BridgeLine::Error(_))) {
                self.errors.remove(channel);
            }
            match parsed {
                Ok(BridgeLine::Event(e)) => {
                    if !self.ingest(e, batch) {
                        break;
                    }
                }
                Ok(BridgeLine::Error(_)) => {
                    self.subscribed.retain(|c| c != channel);
                    self.errors.insert(
                        channel.into(),
                        "Bridge subscription error; check channel membership".into(),
                    );
                }
                Ok(BridgeLine::Warning(reason)) if reason == "expiring" => {
                    self.subscribed.retain(|c| c != channel);
                }
                Ok(BridgeLine::Warning(reason))
                    if reason == "heartbeat_stale" || reason == "heartbeat_missing" =>
                {
                    self.status = BridgeStatus::Stale;
                }
                Ok(_) => {}
                Err(e) => {
                    batch.notices.push(e);
                }
            }
            consumed += line.len() as u64;
        }
        cursor.offset += consumed;
        let caught_up = cursor.offset == meta.len();
        self.inbox.cursors.insert(channel.into(), cursor);
        Ok(caught_up)
    }

    fn api(&mut self, method: &str, params: &[(&str, String)]) -> Result<Value, String> {
        self.check_deadline()?;
        if self
            .api_backoff
            .get(method)
            .is_some_and(|at| *at > Instant::now())
        {
            return Err(format!("{method}: rate limited; retry deferred"));
        }
        let result = self.io.api(method, params);
        if result
            .as_ref()
            .is_err_and(|error| error.contains("ratelimited"))
        {
            self.api_backoff
                .insert(method.into(), Instant::now() + Duration::from_secs(60));
        }
        result
    }

    fn check_deadline(&self) -> Result<(), String> {
        if self.deadline.is_some_and(|at| Instant::now() >= at) {
            Err("Poll deadline reached; remaining work deferred".into())
        } else {
            Ok(())
        }
    }

    fn groups(&mut self) -> Result<(), String> {
        if self.groups_unavailable {
            return Ok(());
        }
        let result = self.api("usergroups.list", &[("include_users", "true".into())]);
        match result {
            Ok(value) => {
                let groups = value["usergroups"]
                    .as_array()
                    .ok_or("Slack usergroups missing usergroups")?;
                self.inbox.user_groups = groups
                    .iter()
                    .filter(|group| {
                        group["users"]
                            .as_array()
                            .is_some_and(|users| users.iter().any(|u| u == &self.settings.user_id))
                    })
                    .filter_map(|group| group["id"].as_str().map(str::to_owned))
                    .collect();
                Ok(())
            }
            Err(error) => {
                if !self.groups_error_logged {
                    log::warn!("envoy: user groups unavailable: {error}");
                    self.groups_error_logged = true;
                }
                if error.contains("missing_scope") || error.contains("not_allowed_token_type") {
                    self.groups_unavailable = true;
                }
                Err(error)
            }
        }
    }

    #[cfg(test)]
    fn search(
        &mut self,
        kind: EventKind,
        now_ms: i64,
        batch: &mut PollBatch,
    ) -> Result<bool, String> {
        self.search_tag(kind, None, now_ms, batch)
    }

    fn search_tag(
        &mut self,
        kind: EventKind,
        group: Option<&str>,
        now_ms: i64,
        batch: &mut PollBatch,
    ) -> Result<bool, String> {
        if self.settings.user_id.is_empty() {
            return Err(crate::user::fill(
                "No Slack member id for {user} yet (auth.test or settings)",
            ));
        }
        let key = match group {
            Some(group) => format!("group:{group}"),
            None => format!("{}:{}", kind.name(), self.settings.user_id),
        };
        let start = self.inbox.first_enabled_ms.unwrap_or(now_ms);
        let mut progress =
            self.inbox
                .searches
                .get(&key)
                .cloned()
                .unwrap_or_else(|| backfill::SearchProgress {
                    since_ms: self
                        .inbox
                        .poll_watermarks
                        .get(&key)
                        .copied()
                        .unwrap_or(start)
                        .saturating_sub(600_000)
                        .max(start),
                    until_ms: now_ms,
                    page: 1,
                });
        self.inbox.searches.insert(key.clone(), progress.clone());
        let date = jiff::Timestamp::from_millisecond(progress.since_ms - 86_400_000)
            .map_err(|e| e.to_string())?
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date();
        let query = match (kind, group) {
            (_, Some(group)) => format!("<!subteam^{group}> after:{date}"),
            (EventKind::Mention, _) => format!("<@{}> after:{date}", self.settings.user_id),
            _ => format!("to:{} is:dm after:{date}", self.settings.user_id),
        };
        let value = self.api(
            "search.messages",
            &[
                ("query", query),
                ("sort", "timestamp".into()),
                ("sort_dir", "asc".into()),
                ("count", "100".into()),
                ("page", progress.page.to_string()),
            ],
        )?;
        let pages = value["messages"]["paging"]["pages"]
            .as_u64()
            .or_else(|| value["messages"]["pagination"]["page_count"].as_u64())
            .ok_or("Slack search is missing pagination")?;
        for event in parse_search(&value, kind, &self.settings.user_id)? {
            if timestamp_us(&event.ts).is_some_and(|ts| ts / 1000 >= progress.since_ms)
                && !self.ingest(event, batch)
            {
                return Err("Reply handoff full; search page retained for retry".into());
            }
        }
        if progress.page >= pages {
            self.inbox
                .poll_watermarks
                .insert(key.clone(), progress.until_ms);
            self.inbox.searches.remove(&key);
            Ok(false)
        } else {
            progress.page += 1;
            self.inbox.searches.insert(key, progress);
            Ok(true)
        }
    }

    /// Caches the head branches of PRs new events name, a few per poll, so dwarves match by branch.
    fn resolve_prs(&mut self, events: &[InboundEvent]) {
        if self.settings.github_owner.is_empty() {
            return;
        }
        let refs: BTreeSet<String> = events
            .iter()
            .chain(self.inbox.events())
            .flat_map(|e| super::matching::refs(&e.text))
            .filter(|r| !r.starts_with('#') && r.contains('#'))
            .filter(|r| !self.inbox.pr_heads.contains_key(r))
            .collect();
        for r in refs.into_iter().take(MAX_PR_LOOKUPS) {
            if self.check_deadline().is_err() {
                break;
            }
            let (repo, number) = r.split_once('#').expect("repo#N");
            let Ok(number) = number.parse() else { continue };
            match self.io.pr_head(&self.settings.github_owner, repo, number) {
                Ok(head) => {
                    self.inbox.pr_heads.insert(r, head.unwrap_or_default());
                }
                Err(_) => return,
            }
        }
    }

    /// New pins, found by searching `has:pin`; the first poll only records what is pinned.
    fn pins(&mut self, batch: &mut PollBatch) -> Result<(), String> {
        let mut found = Vec::new();
        for page in 1..=3 {
            let value = self.api(
                "search.messages",
                &[
                    ("query", "has:pin".into()),
                    ("sort", "timestamp".into()),
                    ("sort_dir", "desc".into()),
                    ("count", "100".into()),
                    ("page", page.to_string()),
                ],
            )?;
            found.extend(parse_search(
                &value,
                EventKind::Pin,
                &self.settings.user_id,
            )?);
            let pages = value["messages"]["paging"]["pages"].as_u64().unwrap_or(0);
            if page >= pages {
                break;
            }
        }
        let Some(seen) = self.inbox.pins_seen.as_mut() else {
            self.inbox.pins_seen = Some(found.iter().map(InboundEvent::id).collect());
            return Ok(());
        };
        let fresh: Vec<_> = found.into_iter().filter(|e| seen.insert(e.id())).collect();
        for e in fresh {
            if self.inbox.get(&e.id()).is_some() || self.inbox.insert(e.clone()) {
                batch.events.push(e);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn history(
        &mut self,
        channel: &str,
        now_ms: i64,
        batch: &mut PollBatch,
    ) -> Result<bool, String> {
        self.backfill(channel, now_ms, true, batch)
    }

    pub fn delete_events(&mut self, ids: &[String]) -> Result<(), String> {
        let old = self.inbox.clone();
        self.inbox.delete(ids);
        if let Err(error) = self.inbox.save(&self.path) {
            self.inbox = old;
            return Err(error.to_string());
        }
        Ok(())
    }

    pub fn mark_forwarded(&mut self, ids: &[String]) -> Result<(), String> {
        let old = self.inbox.clone();
        self.inbox.mark_forwarded(ids);
        if let Err(e) = self.inbox.save(&self.path) {
            self.inbox = old;
            return Err(format!("Cannot save Envoy delivery checkpoint: {e}"));
        }
        Ok(())
    }

    pub fn thread(&mut self, event: &InboundEvent) -> Result<Vec<InboundEvent>, String> {
        let root = event.thread_ts.as_deref().unwrap_or(&event.ts);
        let mut cursor = String::new();
        let mut result = BTreeMap::new();
        for _ in 0..100 {
            let v = self.api(
                "conversations.replies",
                &[
                    ("channel", event.conversation.clone()),
                    ("ts", root.into()),
                    ("limit", "100".into()),
                    ("cursor", cursor.clone()),
                ],
            )?;
            for m in v["messages"]
                .as_array()
                .ok_or("Slack thread missing messages")?
            {
                let e = self::event(m, &event.conversation, EventKind::ThreadReply)?;
                result.insert(e.ts.clone(), e);
            }
            cursor = v["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default()
                .into();
            if cursor.is_empty() {
                if v["has_more"] == true {
                    return Err("Slack thread omitted its next cursor".into());
                }
                return Ok(result.into_values().collect());
            }
        }
        Err("Slack thread exceeded 100 pages".into())
    }
}

#[derive(Clone)]
enum PollJob {
    Search(EventKind, Option<String>),
    Pins,
    Groups,
    Subscribe(String),
    Backfill(String, bool),
    Watch(String),
}

impl<I: SlackIo> SlackChannel<I> {
    fn poll_bounded(&mut self, now_ms: i64, budget: Duration) -> Result<PollBatch, String> {
        let deadline = Instant::now() + budget;
        self.deadline = Some(deadline);
        self.io.set_deadline(Some(deadline));
        let result = self.poll_inner(now_ms);
        self.deadline = None;
        self.io.set_deadline(None);
        result
    }

    fn poll_inner(&mut self, now_ms: i64) -> Result<PollBatch, String> {
        let old = self.inbox.clone();
        self.inbox.first_enabled_ms.get_or_insert(now_ms);
        let mut batch = PollBatch::default();
        let previous = self.status;
        let previous_tick = self.last_tick_ms;
        let observation_gap = self
            .last_tick_ms
            .is_some_and(|at| now_ms.saturating_sub(at) > 90_000);
        self.last_tick_ms = Some(now_ms);
        self.status = heartbeat(&self.bridge, now_ms);
        let channels = self.settings.channels.clone();
        let mut caught_up = BTreeSet::new();
        for channel in &channels {
            match self.tail(channel, &mut batch) {
                Ok(complete) => {
                    self.read_errors.remove(channel);
                    if complete {
                        caught_up.insert(channel.clone());
                    }
                }
                Err(error) => {
                    self.read_errors.insert(channel.clone(), error);
                }
            }
        }
        let watched: BTreeSet<_> = self
            .inbox
            .thread_watches
            .values()
            .filter(|s| !channels.contains(&s.watch.thread.conversation))
            .map(|s| {
                (
                    s.watch.thread.conversation.clone(),
                    s.watch.thread.ts.clone(),
                )
            })
            .collect();
        for (conversation, ts) in watched {
            if let Err(error) =
                self.tail_thread(&super::requests::Thread { conversation, ts }, &mut batch)
            {
                batch.notices.push(error);
            }
        }
        let feed_status = self.status;
        if !self.errors.is_empty()
            || !self.command_errors.is_empty()
            || !self.read_errors.is_empty()
        {
            self.status = BridgeStatus::Stale;
        }
        if self.status != BridgeStatus::Ok || previous != BridgeStatus::Ok || observation_gap {
            for channel in &channels {
                if self
                    .inbox
                    .recovery_needed_through
                    .insert(channel.clone(), now_ms)
                    .is_none()
                    || (previous != BridgeStatus::Ok && self.status == BridgeStatus::Ok)
                {
                    self.next_jobs.remove(&format!("history:{channel}"));
                    self.next_jobs.remove(&format!("recent:{channel}"));
                }
            }
        }
        let mut jobs = vec![
            ("mention".into(), PollJob::Search(EventKind::Mention, None)),
            ("dm".into(), PollJob::Search(EventKind::Dm, None)),
            ("pin".into(), PollJob::Pins),
        ];
        for group in &self.inbox.user_groups {
            jobs.push((
                format!("group:{group}"),
                PollJob::Search(EventKind::Mention, Some(group.clone())),
            ));
        }
        for id in self.inbox.thread_watches.keys() {
            jobs.push((format!("watch:{id}"), PollJob::Watch(id.clone())));
        }
        for channel in &channels {
            jobs.push((
                format!("subscribe:{channel}"),
                PollJob::Subscribe(channel.clone()),
            ));
            jobs.push((
                format!("recent:{channel}"),
                PollJob::Backfill(channel.clone(), false),
            ));
            jobs.push((
                format!("history:{channel}"),
                PollJob::Backfill(channel.clone(), true),
            ));
        }
        jobs.push(("groups".into(), PollJob::Groups));
        self.poll_errors
            .retain(|key, _| jobs.iter().any(|(job, _)| job == key));
        self.next_jobs
            .retain(|key, _| jobs.iter().any(|(job, _)| job == key));
        let interval = self.settings.poll_secs.clamp(10, 86_400) as i64 * 1000;
        for _ in 0..jobs.len() {
            if self.check_deadline().is_err() {
                break;
            }
            let index = self.next_job % jobs.len();
            self.next_job = (index + 1) % jobs.len();
            let (key, job) = &jobs[index];
            if self.next_jobs.get(key).is_some_and(|at| now_ms < *at) {
                continue;
            }
            let mut delay = interval;
            let result = match job {
                PollJob::Search(kind, group) => {
                    self.search_tag(*kind, group.as_deref(), now_ms, &mut batch)
                }
                PollJob::Pins => self.pins(&mut batch).map(|()| false),
                PollJob::Groups => {
                    delay = 3_600_000;
                    self.groups().map(|()| false)
                }
                PollJob::Subscribe(channel) => {
                    delay = 120_000;
                    if self.subscribed.contains(channel) && !self.errors.contains_key(channel) {
                        Ok(false)
                    } else {
                        match self.io.subscribe(&self.bridge, channel) {
                            Ok(()) => {
                                self.subscribed.push(channel.clone());
                                self.command_errors.remove(channel);
                                Ok(false)
                            }
                            Err(error) => {
                                self.command_errors.insert(channel.clone(), error.clone());
                                Err(error)
                            }
                        }
                    }
                }
                PollJob::Backfill(channel, deep) => {
                    if self.inbox.recovery_needed_through.contains_key(channel) {
                        self.backfill(channel, now_ms, *deep, &mut batch)
                    } else {
                        if caught_up.contains(channel) {
                            self.inbox.poll_watermarks.insert(key.clone(), now_ms);
                        }
                        self.inbox.backfills.remove(key);
                        Ok(false)
                    }
                }
                PollJob::Watch(id) => self
                    .poll_thread(id, now_ms, &mut batch)
                    .map(|()| !self.inbox.thread_watches[id].caught_up),
            };
            match result {
                Ok(more) => {
                    self.poll_errors.remove(key);
                    if more {
                        delay = 2_000;
                    }
                }
                Err(error) => {
                    if self.check_deadline().is_err() {
                        delay = 2_000;
                    }
                    if !matches!(job, PollJob::Groups) {
                        let error = match job {
                            PollJob::Watch(id) => format!("{id}: {error}"),
                            _ => error,
                        };
                        self.poll_errors.insert(key.clone(), error);
                    }
                }
            }
            self.next_jobs.insert(key.clone(), now_ms + delay);
        }
        self.status = feed_status;
        if !self.errors.is_empty()
            || !self.command_errors.is_empty()
            || !self.read_errors.is_empty()
        {
            self.status = BridgeStatus::Stale;
            for channel in &channels {
                self.inbox
                    .recovery_needed_through
                    .insert(channel.clone(), now_ms);
            }
        }
        self.resolve_prs(&batch.events);
        for (channel, error) in self
            .errors
            .iter()
            .chain(&self.command_errors)
            .chain(&self.read_errors)
        {
            batch.notices.push(format!("{channel}: {error}"));
        }
        batch.notices.extend(self.poll_errors.values().cloned());
        self.inbox.subscription_errors = self.errors.clone();
        if (self.inbox != old || self.needs_save)
            && let Err(error) = self.inbox.save(&self.path)
        {
            self.inbox = old;
            self.status = previous;
            self.last_tick_ms = previous_tick;
            self.next_jobs.clear();
            return Err(format!("Cannot save Envoy inbox: {error}"));
        }
        self.needs_save = false;
        batch.events.retain(|e| self.inbox.get(&e.id()).is_some());
        Ok(batch)
    }
}

impl<I: SlackIo> Channel for SlackChannel<I> {
    fn poll(&mut self, now_ms: i64) -> Result<PollBatch, String> {
        self.poll_bounded(now_ms, POLL_BUDGET)
    }
    fn send(&mut self, approved: &OutboundMessage) -> Result<SendReceipt, String> {
        let receipt = self.io.send(approved)?;
        let root = approved.thread_ts.as_deref().unwrap_or(&receipt.ts);
        self.inbox
            .user_threads
            .insert(format!("{}:{root}", receipt.conversation));
        self.needs_save = true;
        Ok(receipt)
    }
    fn health(&self) -> BridgeStatus {
        self.status
    }
}

pub mod backfill;

pub mod threads;

#[cfg(test)]
mod tests;

//! Bridge ingestion and Slack CLI I/O. Called only on the Envoy worker thread.

use std::collections::BTreeMap;
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

pub trait SlackSender: Send {
    fn send(&mut self, message: &OutboundMessage) -> Result<SendReceipt, String>;
}

pub trait SlackIo: SlackSender {
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
    fn api(&mut self, method: &str, params: &[(&str, String)]) -> Result<Value, String>;
    fn subscribe(&mut self, bridge_dir: &Path, conversation: &str) -> Result<(), String>;
}

#[derive(Default)]
pub struct SlackCli;

fn binary(name: &str) -> Result<PathBuf, String> {
    let env = format!("MYRSTACK_{}", name.to_ascii_uppercase().replace('-', "_"));
    if let Some(path) = std::env::var_os(&env).filter(|p| !p.is_empty()) {
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
            Ok(None) if start.elapsed() < Duration::from_secs(25) => {
                std::thread::sleep(Duration::from_millis(25))
            }
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
        if ![super::drafts::ATTRIBUTION, super::drafts::AUTO_ATTRIBUTION]
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
    karl: &str,
) -> Result<Vec<InboundEvent>, String> {
    let matches = value["messages"]["matches"]
        .as_array()
        .ok_or("Slack search is missing messages.matches")?;
    matches
        .iter()
        .filter(|v| v["user"] != karl)
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
    next_poll_ms: i64,
    next_subscribe_ms: i64,
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
            next_poll_ms: 0,
            next_subscribe_ms: 0,
        }
    }

    pub fn configure(&mut self, settings: EnvoySettings) {
        self.subscribed.clear();
        self.errors.retain(|c, _| settings.channels.contains(c));
        self.command_errors
            .retain(|c, _| settings.channels.contains(c));
        self.read_errors
            .retain(|c, _| settings.channels.contains(c));
        self.settings = settings;
        self.next_poll_ms = 0;
        self.next_subscribe_ms = 0;
    }

    fn ingest(&mut self, e: InboundEvent, batch: &mut PollBatch) -> bool {
        if e.user != self.settings.karl_user_id && !self.capture_reply(&e) {
            return false;
        }
        let fresh = self
            .inbox
            .first_enabled_ms
            .is_none_or(|start| timestamp_us(&e.ts).is_some_and(|ts| ts / 1000 >= start));
        if fresh && e.user != self.settings.karl_user_id && self.inbox.insert(e.clone()) {
            batch.events.push(e);
        }
        true
    }

    fn tail(&mut self, channel: &str, batch: &mut PollBatch) -> Result<(), String> {
        validate_conversation(channel)?;
        let path = self
            .bridge
            .join("channels")
            .join(format!("{channel}.jsonl"));
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
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
            return Ok(());
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
        self.inbox.cursors.insert(channel.into(), cursor);
        Ok(())
    }

    fn search(
        &mut self,
        kind: EventKind,
        now_ms: i64,
        batch: &mut PollBatch,
    ) -> Result<(), String> {
        let key = format!("{}:{}", kind.name(), self.settings.karl_user_id);
        let start = self.inbox.first_enabled_ms.unwrap_or(now_ms);
        let since = self
            .inbox
            .poll_watermarks
            .get(&key)
            .copied()
            .unwrap_or(start)
            .saturating_sub(600_000)
            .max(start);
        let date = jiff::Timestamp::from_millisecond(since - 86_400_000)
            .map_err(|e| e.to_string())?
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date();
        let query = match kind {
            EventKind::Mention => format!("<@{}> after:{date}", self.settings.karl_user_id),
            _ => format!("to:{} is:dm after:{date}", self.settings.karl_user_id),
        };
        for page in 1..=5 {
            let value = self.io.api(
                "search.messages",
                &[
                    ("query", query.clone()),
                    ("sort", "timestamp".into()),
                    ("sort_dir", "desc".into()),
                    ("count", "100".into()),
                    ("page", page.to_string()),
                ],
            )?;
            let events = parse_search(&value, kind, &self.settings.karl_user_id)?;
            let old = value["messages"]["matches"].as_array().is_some_and(|ms| {
                ms.iter().any(|v| {
                    v["ts"]
                        .as_str()
                        .and_then(timestamp_us)
                        .is_some_and(|t| t / 1000 < since)
                })
            });
            for e in events {
                if timestamp_us(&e.ts).unwrap_or_default() / 1000 >= since && !self.ingest(e, batch)
                {
                    return Err("Reply handoff full; ingestion will retry".into());
                }
            }
            let pages = value["messages"]["paging"]["pages"]
                .as_u64()
                .or_else(|| value["messages"]["pagination"]["page_count"].as_u64())
                .ok_or("Slack search is missing pagination")?;
            if page >= pages || old {
                self.inbox.poll_watermarks.insert(key, now_ms);
                return Ok(());
            }
        }
        self.inbox.poll_watermarks.insert(key, now_ms);
        Err(format!(
            "Slack {} search reached 5 pages; older messages skipped",
            kind.name()
        ))
    }

    fn history(&mut self, channel: &str, now_ms: i64, batch: &mut PollBatch) -> Result<(), String> {
        let key = format!("history:{channel}");
        let start = self.inbox.first_enabled_ms.unwrap_or(now_ms);
        let since = self
            .inbox
            .poll_watermarks
            .get(&key)
            .copied()
            .unwrap_or(start)
            .saturating_sub(600_000)
            .max(start);
        let mut cursor = String::new();
        for _ in 0..5 {
            let v = self.io.api(
                "conversations.history",
                &[
                    ("channel", channel.into()),
                    ("oldest", format!("{}", since.max(0) / 1000)),
                    ("limit", "100".into()),
                    ("cursor", cursor.clone()),
                ],
            )?;
            let messages = v["messages"]
                .as_array()
                .ok_or("Slack history missing messages")?;
            for m in messages {
                self.ingest(event(m, channel, EventKind::AgentComms)?, batch);
            }
            cursor = v["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default()
                .into();
            if cursor.is_empty() {
                if v["has_more"] == true {
                    return Err("Slack history omitted its next cursor".into());
                }
                self.inbox.poll_watermarks.insert(key, now_ms);
                return Ok(());
            }
        }
        self.inbox.poll_watermarks.insert(key, now_ms);
        Err(format!(
            "Slack history for {channel} reached 5 pages; older messages skipped"
        ))
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
            let v = self.io.api(
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

impl<I: SlackIo> Channel for SlackChannel<I> {
    fn poll(&mut self, now_ms: i64) -> Result<PollBatch, String> {
        let old = self.inbox.clone();
        self.inbox.first_enabled_ms.get_or_insert(now_ms);
        let mut batch = PollBatch::default();

        self.status = heartbeat(&self.bridge, now_ms);
        let channels = self.settings.channels.clone();
        if now_ms >= self.next_subscribe_ms {
            self.next_subscribe_ms = now_ms + 120_000;
            for channel in &channels {
                if !self.subscribed.contains(channel) {
                    match self.io.subscribe(&self.bridge, channel) {
                        Ok(()) => {
                            self.subscribed.push(channel.clone());
                            self.command_errors.remove(channel);
                        }
                        Err(e) => {
                            self.command_errors.insert(channel.clone(), e);
                        }
                    }
                }
            }
        }
        for channel in &channels {
            match self.tail(channel, &mut batch) {
                Ok(()) => {
                    self.read_errors.remove(channel);
                }
                Err(error) => {
                    self.read_errors.insert(channel.clone(), error);
                }
            }
        }
        if !self.errors.is_empty()
            || !self.command_errors.is_empty()
            || !self.read_errors.is_empty()
        {
            self.status = BridgeStatus::Stale;
        }
        if now_ms >= self.next_poll_ms {
            self.next_poll_ms = now_ms + (self.settings.poll_secs.clamp(10, 86_400) as i64 * 1000);
            self.poll_errors.clear();
            for kind in [EventKind::Mention, EventKind::Dm] {
                if let Err(e) = self.search(kind, now_ms, &mut batch) {
                    self.poll_errors.insert(kind.name().into(), e);
                }
            }
            if self.status != BridgeStatus::Ok {
                for channel in &channels {
                    if let Err(e) = self.history(channel, now_ms, &mut batch) {
                        self.poll_errors.insert(channel.clone(), e);
                    }
                }
            }
        }
        if let Err(error) = self.poll_threads(now_ms, &mut batch) {
            batch.notices.push(error);
        }
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
        if self.inbox != old
            && let Err(e) = self.inbox.save(&self.path)
        {
            self.inbox = old;
            self.next_poll_ms = 0;
            return Err(format!("Cannot save Envoy inbox: {e}"));
        }
        batch.events.retain(|e| self.inbox.get(&e.id()).is_some());
        Ok(batch)
    }
    fn send(&mut self, approved: &OutboundMessage) -> Result<SendReceipt, String> {
        self.io.send(approved)
    }
    fn health(&self) -> BridgeStatus {
        self.status
    }
}

pub mod threads;

#[cfg(test)]
mod tests;

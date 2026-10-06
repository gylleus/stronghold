//! Inbox and source checkpoints commit together so a restart cannot skip unsaved events.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::channel::InboundEvent;

pub const MAX_EVENTS: usize = 2_000;
pub const INBOX_FILE: &str = "envoy-inbox.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub inode: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Inbox {
    events: Vec<InboundEvent>,
    floor_us: i64,
    forwarded: BTreeSet<String>,
    /// Ingestion never backfills messages from before the first activation.
    pub first_enabled_ms: Option<i64>,
    pub subscription_errors: BTreeMap<String, String>,
    pub cursors: BTreeMap<String, Cursor>,
    pub poll_watermarks: BTreeMap<String, i64>,
    pub thread_watches: BTreeMap<String, super::slack::threads::WatchState>,
    pub pending_replies: Vec<super::slack::threads::MatchedReply>,
    /// Pinned messages already seen (`conversation:ts`); `None` until the first pin poll.
    pub pins_seen: Option<BTreeSet<String>>,
}

pub fn timestamp_us(ts: &str) -> Option<i64> {
    let (seconds, fraction) = ts.split_once('.').unwrap_or((ts, "0"));
    if fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if seconds.is_empty() || !seconds.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let seconds: i64 = seconds.parse().ok()?;
    let fraction: i64 = format!("{fraction:0<6}").parse().ok()?;
    seconds.checked_mul(1_000_000)?.checked_add(fraction)
}

impl Inbox {
    pub fn insert(&mut self, event: InboundEvent) -> bool {
        let Some(ts) = timestamp_us(&event.ts) else {
            return false;
        };
        if ts <= self.floor_us
            || self
                .events
                .iter()
                .any(|e| e.conversation == event.conversation && e.ts == event.ts)
        {
            return false;
        }
        self.events.push(event);
        self.events
            .sort_by_key(|e| timestamp_us(&e.ts).unwrap_or_default());
        if self.events.len() > MAX_EVENTS {
            let removed = self.events.remove(0);
            self.forwarded.remove(&removed.id());
            self.floor_us = timestamp_us(&removed.ts).unwrap_or(self.floor_us);
        }
        true
    }

    pub fn unforwarded(&self) -> Vec<InboundEvent> {
        self.events
            .iter()
            .filter(|e| !self.forwarded.contains(&e.id()))
            .cloned()
            .collect()
    }

    pub fn mark_forwarded(&mut self, ids: &[String]) {
        for id in ids {
            if self.get(id).is_some() {
                self.forwarded.insert(id.clone());
            }
        }
    }

    pub fn events(&self) -> &[InboundEvent] {
        &self.events
    }

    pub fn get(&self, id: &str) -> Option<&InboundEvent> {
        self.events.iter().find(|e| e.id() == id)
    }

    pub fn since(&self, since: Option<&str>, limit: usize) -> Vec<InboundEvent> {
        let floor = since.and_then(timestamp_us).unwrap_or_default();
        let events: Vec<_> = self
            .events
            .iter()
            .filter(|e| timestamp_us(&e.ts).unwrap_or_default() > floor)
            .collect();
        let limit = limit.min(MAX_EVENTS);
        let start = if since.is_none() {
            events.len().saturating_sub(limit)
        } else {
            0
        };
        events
            .into_iter()
            .skip(start)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn thread(&self, event: &InboundEvent) -> Vec<InboundEvent> {
        let root = event.thread_ts.as_deref().unwrap_or(&event.ts);
        self.events
            .iter()
            .filter(|e| {
                e.conversation == event.conversation
                    && (e.ts == root || e.thread_ts.as_deref() == Some(root))
            })
            .cloned()
            .collect()
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        load(path)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        save(path, self)
    }
}

pub(super) fn load<T: serde::de::DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e),
    }
}

pub(super) fn save(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut out = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    serde_json::to_writer(&mut out, value).map_err(io::Error::other)?;
    out.flush()?;
    out.sync_all()?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envoy::channel::EventKind;

    pub fn event(ts: &str) -> InboundEvent {
        InboundEvent {
            channel_kind: "slack".into(),
            conversation: "C123".into(),
            ts: ts.into(),
            thread_ts: None,
            user: "U123".into(),
            user_name: "Avery".into(),
            text: "Hello".into(),
            kind: EventKind::AgentComms,
            permalink: None,
        }
    }

    #[test]
    fn bounded_deduplication_survives_restart_and_eviction() {
        let mut inbox = Inbox::default();
        for n in 1..=MAX_EVENTS + 1 {
            assert!(inbox.insert(event(&format!("{n}.000001"))));
        }
        assert_eq!(inbox.events().len(), MAX_EVENTS);
        assert!(!inbox.insert(event("1.000001")));
        assert!(!inbox.insert(event("2.000001")));
        assert!(
            inbox.insert(event("2.000002")),
            "late messages inside retained window survive"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INBOX_FILE);
        inbox.save(&path).unwrap();
        let mut loaded = Inbox::load(&path).unwrap();
        assert!(!loaded.insert(event("1.000001")));
        assert_eq!(loaded.since(Some("1999"), 1).len(), 1);
        assert_eq!(
            loaded.since(None, 1)[0].ts,
            format!("{}.000001", MAX_EVENTS + 1)
        );
    }
}

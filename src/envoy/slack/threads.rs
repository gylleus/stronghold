//! Watched replies survive display-history eviction until the request store acknowledges them.

use super::*;
use crate::envoy::requests::Thread;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_PENDING: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDestination {
    pub conversation: String,
    pub person: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    pub request_id: String,
    pub thread: Thread,
    pub after_us: i64,
    #[serde(default)]
    pub borrowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchState {
    pub watch: Watch,
    pub seen: BTreeSet<String>,
    pub api_cursor: String,
    pub caught_up: bool,
    pub subscribed: bool,
    pub next_poll_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedReply {
    pub request_id: String,
    pub event: InboundEvent,
}
impl MatchedReply {
    pub fn key(&self) -> String {
        format!("{}:{}", self.request_id, self.event.id())
    }
}

impl<I: SlackIo> SlackChannel<I> {
    pub fn watches(&mut self, watches: Vec<Watch>) -> Result<(), String> {
        let old = self.inbox.clone();
        self.inbox
            .thread_watches
            .retain(|id, _| watches.iter().any(|w| &w.request_id == id));
        for watch in watches {
            let state = self
                .inbox
                .thread_watches
                .entry(watch.request_id.clone())
                .or_insert_with(|| WatchState {
                    watch: watch.clone(),
                    seen: BTreeSet::new(),
                    api_cursor: String::new(),
                    caught_up: false,
                    subscribed: watch.borrowed,
                    next_poll_ms: 0,
                });
            if state.watch != watch {
                state.seen.clear();
            }
            if watch.borrowed {
                state.subscribed = true;
            }
            state.watch = watch;
            state.api_cursor.clear();
            state.caught_up = false;
            state.next_poll_ms = 0;
        }
        if let Err(e) = self.inbox.save(&self.path) {
            self.inbox = old;
            return Err(e.to_string());
        }
        Ok(())
    }

    pub fn acknowledge_replies(&mut self, keys: &[String]) -> Result<(), String> {
        let old = self.inbox.clone();
        self.inbox
            .pending_replies
            .retain(|r| !keys.contains(&r.key()));
        if let Err(e) = self.inbox.save(&self.path) {
            self.inbox = old;
            return Err(e.to_string());
        }
        Ok(())
    }

    pub(super) fn capture_reply(&mut self, event: &InboundEvent) -> bool {
        let matching: Vec<_> = self
            .inbox
            .thread_watches
            .iter()
            .filter(|(_, s)| {
                s.watch.thread.matches(event)
                    && super::super::inbox::timestamp_us(&event.ts)
                        .is_some_and(|ts| ts > s.watch.after_us)
                    && !s.seen.contains(&event.id())
            })
            .map(|(id, _)| id.clone())
            .collect();
        if self.inbox.pending_replies.len() + matching.len() > MAX_PENDING {
            return false;
        }
        for id in matching {
            self.inbox
                .thread_watches
                .get_mut(&id)
                .unwrap()
                .seen
                .insert(event.id());
            self.inbox.pending_replies.push(MatchedReply {
                request_id: id,
                event: event.clone(),
            });
        }
        true
    }

    pub(super) fn poll_threads(&mut self, now: i64, batch: &mut PollBatch) -> Result<(), String> {
        let ids: Vec<_> = self.inbox.thread_watches.keys().cloned().collect();
        for id in ids {
            if let Err(error) = self.poll_thread(&id, now, batch) {
                self.inbox.thread_watches.get_mut(&id).unwrap().next_poll_ms = now + 30_000;
                batch.notices.push(format!("{id}: {error}"));
            }
        }
        Ok(())
    }
    fn poll_thread(&mut self, id: &str, now: i64, batch: &mut PollBatch) -> Result<(), String> {
        let state = self.inbox.thread_watches[id].clone();
        let thread = &state.watch.thread;
        if !self.settings.channels.contains(&thread.conversation) {
            if !state.watch.borrowed
                && !state.subscribed
                && now >= state.next_poll_ms
                && !self.settings.dry_run
            {
                match self.io.thread_subscription(
                    &self.bridge,
                    thread,
                    Some(&format!("envoy:{id}")),
                ) {
                    Ok(()) => self.inbox.thread_watches.get_mut(id).unwrap().subscribed = true,
                    Err(e) => batch.notices.push(e),
                }
            }
            self.tail_thread(thread, batch)?;
        }
        let state = self.inbox.thread_watches[id].clone();
        if now < state.next_poll_ms {
            return Ok(());
        }
        if !state.caught_up
            || self.status != BridgeStatus::Ok
            || (!state.subscribed && !self.settings.channels.contains(&thread.conversation))
        {
            let mut params = vec![
                ("channel", thread.conversation.clone()),
                ("ts", thread.ts.clone()),
                ("limit", "100".into()),
            ];
            if !state.api_cursor.is_empty() {
                params.push(("cursor", state.api_cursor.clone()));
            }
            let value = self.io.api("conversations.replies", &params)?;
            let messages = value["messages"]
                .as_array()
                .ok_or("Thread response is missing messages")?;
            for value in messages {
                let mut e = event(value, &thread.conversation, EventKind::ThreadReply)?;
                if e.ts != thread.ts {
                    e.thread_ts = Some(thread.ts.clone());
                    if e.permalink.is_none()
                        && e.user != self.settings.user_id
                        && !state.seen.contains(&e.id())
                    {
                        let link = self.io.api(
                            "chat.getPermalink",
                            &[
                                ("channel", thread.conversation.clone()),
                                ("message_ts", e.ts.clone()),
                            ],
                        )?;
                        let link = link["permalink"]
                            .as_str()
                            .ok_or("Reply permalink is missing")?;
                        if Thread::from_link(link)?.conversation != thread.conversation {
                            return Err("Reply permalink has a different conversation".into());
                        }
                        e.permalink = Some(link.into());
                    }
                }
                if !self.ingest(e, batch) {
                    return Err("Reply handoff full; thread page retained for retry".into());
                }
            }
            let next = value["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default();
            if next.is_empty() && value["has_more"] == true {
                return Err("Thread pagination missing cursor".into());
            }
            let state = self.inbox.thread_watches.get_mut(id).unwrap();
            state.api_cursor = next.into();
            state.caught_up = next.is_empty();
            state.next_poll_ms = if state.caught_up {
                now + self.settings.poll_secs as i64 * 1000
            } else {
                0
            };
        } else {
            self.inbox.thread_watches.get_mut(id).unwrap().next_poll_ms =
                now + self.settings.poll_secs as i64 * 1000;
        }
        Ok(())
    }

    fn tail_thread(&mut self, thread: &Thread, batch: &mut PollBatch) -> Result<(), String> {
        validate_conversation(&thread.conversation)?;
        validate_ts(&thread.ts)?;
        let path = self
            .bridge
            .join("threads")
            .join(format!("{}-{}.jsonl", thread.conversation, thread.ts));
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let meta = file.metadata().map_err(|e| e.to_string())?;
        let key = format!("thread:{}", thread.key());
        let mut cursor = self.inbox.cursors.get(&key).cloned().unwrap_or_default();
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
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if !line.ends_with(b"\n") {
                break;
            }
            match parse_bridge_line(line) {
                Ok(BridgeLine::Event(e)) => {
                    let full = !self.ingest(e, batch);
                    if full {
                        break;
                    }
                }
                Ok(BridgeLine::Error(_)) => {
                    for state in self
                        .inbox
                        .thread_watches
                        .values_mut()
                        .filter(|s| s.watch.thread == *thread)
                    {
                        state.subscribed = state.watch.borrowed;
                        state.caught_up = false;
                        state.next_poll_ms = 0;
                    }
                    batch
                        .notices
                        .push(format!("Thread {} subscription error", thread.key()));
                }
                Ok(BridgeLine::Warning(reason)) => {
                    batch
                        .notices
                        .push(format!("Thread {}: {reason}", thread.key()));
                    if reason == "expiring" {
                        for s in self
                            .inbox
                            .thread_watches
                            .values_mut()
                            .filter(|s| s.watch.thread == *thread)
                        {
                            s.subscribed = s.watch.borrowed;
                        }
                    }
                }
                Err(e) => batch.notices.push(e),
                _ => {}
            }
            cursor.offset += line.len() as u64;
        }
        self.inbox.cursors.insert(key, cursor);
        Ok(())
    }

    pub fn binding_preview(
        &mut self,
        thread: &Thread,
    ) -> Result<crate::envoy::requests::BindingPreview, String> {
        validate_conversation(&thread.conversation)?;
        validate_ts(&thread.ts)?;
        let result = self.io.api(
            "conversations.replies",
            &[
                ("channel", thread.conversation.clone()),
                ("ts", thread.ts.clone()),
                ("limit", "1".into()),
            ],
        )?;
        let root = result["messages"]
            .as_array()
            .and_then(|messages| messages.iter().find(|m| m["ts"] == thread.ts))
            .ok_or("Thread root unavailable for review")?;
        let first_line: String = root["text"]
            .as_str()
            .unwrap_or("(No text in thread root)")
            .lines()
            .next()
            .unwrap_or("(Empty thread root)")
            .chars()
            .take(500)
            .collect();
        let link = self.io.api(
            "chat.getPermalink",
            &[
                ("channel", thread.conversation.clone()),
                ("message_ts", thread.ts.clone()),
            ],
        )?;
        let slack_link = link["permalink"]
            .as_str()
            .ok_or("Thread permalink unavailable")?;
        if Thread::from_link(slack_link)? != *thread {
            return Err("Thread permalink conflicts with preview".into());
        }
        Ok(crate::envoy::requests::BindingPreview {
            thread: thread.clone(),
            first_line,
            slack_link: slack_link.into(),
        })
    }

    pub fn list_subscriptions(&mut self, now: i64) -> Result<Vec<(Thread, String)>, String> {
        parse_subscriptions(&self.io.subscriptions(&self.bridge)?, now)
    }
    pub fn remove_subscription(&mut self, thread: &Thread) -> Result<(), String> {
        if self.settings.dry_run {
            return Err("Dry run prevents subscription removal".into());
        }
        self.io.thread_subscription(&self.bridge, thread, None)
    }
    pub fn resolve_destination(
        &mut self,
        conversation: &str,
    ) -> Result<ResolvedDestination, String> {
        super::super::requests::refuse_user(conversation, &self.settings.user_id)?;
        validate_conversation(conversation)?;
        let mut person = false;
        if conversation.starts_with('D') {
            let value = self
                .io
                .api("conversations.info", &[("channel", conversation.into())])?;
            let user = value["channel"]["user"]
                .as_str()
                .ok_or("DM recipient could not be verified")?;
            super::super::requests::refuse_user(user, &self.settings.user_id)?;
            let info = self.io.api("users.info", &[("user", user.into())])?;
            person = info["user"]["is_bot"] == false
                && info["user"]["is_app_user"] != true
                && info["user"]["deleted"] != true;
        }
        Ok(ResolvedDestination {
            conversation: conversation.into(),
            person,
        })
    }
}

pub fn parse_subscriptions(output: &str, now: i64) -> Result<Vec<(Thread, String)>, String> {
    let mut out = Vec::new();
    for line in output
        .lines()
        .filter(|l| !l.trim().is_empty() && *l != "no active subscriptions")
    {
        let identity = line.split("  ").next().unwrap_or_default();
        if identity.ends_with(" (channel)") {
            continue;
        }
        let mut words = identity.split_whitespace();
        let conversation = words.next().ok_or("Malformed subscription row")?;
        let ts = words.next().ok_or("Malformed subscription timestamp")?;
        validate_conversation(conversation)?;
        validate_ts(ts)?;
        let expires = line
            .split("  ")
            .find_map(|s| s.strip_prefix("expires="))
            .ok_or("Missing subscription expiry")?;
        if expires != "never"
            && expires
                .parse::<jiff::Timestamp>()
                .map_err(|_| "Invalid subscription expiry")?
                .as_millisecond()
                <= now
        {
            continue;
        }
        let label = if let Some(start) = line.find("  label=") {
            let mut de = serde_json::Deserializer::from_str(&line[start + 8..]);
            String::deserialize(&mut de).map_err(|_| "Malformed subscription label")?
        } else {
            format!("{conversation} {ts}")
        };
        if label.starts_with("envoy:") {
            continue;
        }
        out.push((
            Thread {
                conversation: conversation.into(),
                ts: ts.into(),
            },
            label,
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests;

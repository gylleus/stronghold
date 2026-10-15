//! Resumable catch-up keeps checkpoints behind unfinished pages.

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchProgress {
    pub since_ms: i64,
    pub until_ms: i64,
    pub page: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backfill {
    since_ms: i64,
    until_ms: i64,
    cursor: String,
    history_done: bool,
    threads: VecDeque<(String, String)>,
}

fn next_cursor(value: &Value) -> Result<String, String> {
    let cursor = value["response_metadata"]["next_cursor"]
        .as_str()
        .unwrap_or_default();
    if cursor.is_empty() && value["has_more"] == true {
        return Err("Slack pagination omitted its next cursor".into());
    }
    Ok(cursor.into())
}

impl<I: SlackIo> SlackChannel<I> {
    pub(super) fn backfill(
        &mut self,
        channel: &str,
        now: i64,
        deep: bool,
        batch: &mut PollBatch,
    ) -> Result<bool, String> {
        let key = format!("{}:{channel}", if deep { "history" } else { "recent" });
        let start = self.inbox.first_enabled_ms.unwrap_or(now);
        let mut work = self
            .inbox
            .backfills
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Backfill {
                since_ms: self
                    .inbox
                    .poll_watermarks
                    .get(&key)
                    .or_else(|| {
                        self.inbox
                            .poll_watermarks
                            .get(&format!("history:{channel}"))
                    })
                    .copied()
                    .unwrap_or(start)
                    .saturating_sub(600_000)
                    .max(start),
                until_ms: now,
                cursor: String::new(),
                history_done: false,
                threads: VecDeque::new(),
            });
        if work.cursor.is_empty() && !work.history_done && work.threads.is_empty() {
            work.until_ms = now;
        }
        // Save the window before I/O; failures must not move its upper checkpoint.
        self.inbox.backfills.insert(key.clone(), work.clone());
        if let Some((root, cursor)) = work.threads.front().cloned() {
            let value = self.api(
                "conversations.replies",
                &[
                    ("channel", channel.into()),
                    ("ts", root.clone()),
                    ("oldest", format!("{:.3}", work.since_ms as f64 / 1000.0)),
                    ("latest", format!("{:.3}", work.until_ms as f64 / 1000.0)),
                    ("inclusive", "true".into()),
                    ("limit", "100".into()),
                    ("cursor", cursor),
                ],
            )?;
            let cursor = next_cursor(&value)?;
            for message in value["messages"]
                .as_array()
                .ok_or("Slack replies missing messages")?
            {
                let mut event = event(message, channel, EventKind::ThreadReply)?;
                if event.ts == root {
                    self.remember_own_thread(&event);
                    continue;
                }
                event.thread_ts = Some(root.clone());
                if timestamp_us(&event.ts)
                    .is_some_and(|ts| ts / 1000 >= work.since_ms && ts / 1000 <= work.until_ms)
                    && !self.ingest(event, batch)
                {
                    return Err("Reply handoff full; backfill page retained for retry".into());
                }
            }
            if cursor.is_empty() {
                work.threads.pop_front();
            } else {
                work.threads.front_mut().unwrap().1 = cursor;
            }
        } else if !work.history_done {
            let mut params = vec![
                ("channel", channel.into()),
                ("latest", format!("{:.3}", work.until_ms as f64 / 1000.0)),
                ("inclusive", "true".into()),
                ("limit", "100".into()),
                ("cursor", work.cursor.clone()),
            ];
            // Old roots can have new replies, so discovery must page past the outage window.
            if !deep {
                params.push(("oldest", format!("{:.3}", work.since_ms as f64 / 1000.0)));
            }
            let value = self.api("conversations.history", &params)?;
            let cursor = next_cursor(&value)?;
            for message in value["messages"]
                .as_array()
                .ok_or("Slack history missing messages")?
            {
                let event = event(message, channel, EventKind::AgentComms)?;
                self.remember_own_thread(&event);
                if timestamp_us(&event.ts)
                    .is_some_and(|ts| ts / 1000 >= work.since_ms && ts / 1000 <= work.until_ms)
                    && !self.ingest(event.clone(), batch)
                {
                    return Err("Reply handoff full; history page retained for retry".into());
                }
                if message["latest_reply"]
                    .as_str()
                    .and_then(timestamp_us)
                    .is_some_and(|ts| ts / 1000 >= work.since_ms)
                    || (message["reply_count"].as_u64().unwrap_or(0) > 0
                        && message["latest_reply"].is_null())
                {
                    work.threads.push_back((event.ts, String::new()));
                }
            }
            work.history_done = cursor.is_empty();
            work.cursor = cursor;
        }
        let complete = work.history_done && work.threads.is_empty();
        if complete {
            self.inbox
                .poll_watermarks
                .insert(key.clone(), work.until_ms);
            self.inbox.backfills.remove(&key);
            if deep
                && self.status == BridgeStatus::Ok
                && self
                    .inbox
                    .recovery_needed_through
                    .get(channel)
                    .is_some_and(|at| *at <= work.until_ms)
            {
                self.inbox.recovery_needed_through.remove(channel);
            }
        } else {
            self.inbox.backfills.insert(key, work);
        }
        Ok(!complete)
    }
}

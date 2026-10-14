//! Slack subprocesses are serialized here; the UI only sends work and drains results.

use super::BridgeStatus;
use super::channel::{Channel, InboundEvent, OutboundMessage, SendReceipt};
use super::inbox::Inbox;
use super::settings::EnvoySettings;
use super::slack::{SlackChannel, SlackIo};
use crate::control::{ControlError, ErrorCode, Reply};
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

pub enum Work {
    Configure(Option<EnvoySettings>),
    Watches(Vec<super::slack::threads::Watch>),
    RecordedReplies(Vec<String>),
    Adopt,
    Remove(super::requests::Thread),
    Resolve {
        command: super::control::EnvoyCommand,
        reply: Reply,
    },
    BindingPreview {
        command: super::control::EnvoyCommand,
        requester: Box<super::requests::Requester>,
        thread: super::requests::Thread,
        reply: Reply,
    },
    Forwarded(Vec<String>),
    Send {
        id: String,
        message: OutboundMessage,
    },
    Thread {
        event: InboundEvent,
        note: Option<super::notes::ThreadNote>,
        reply: Reply,
    },
}

pub enum Update {
    Polled {
        inbox: Inbox,
        events: Vec<InboundEvent>,
        bridge: BridgeStatus,
        notices: Vec<String>,
        /// When this poll ran: the app's liveness signal for the loop.
        polled_at_ms: i64,
    },
    Sent {
        id: String,
        result: Result<SendReceipt, String>,
    },
    Watches {
        watches: Vec<super::slack::threads::Watch>,
        result: Result<(), String>,
    },
    Removed {
        thread: super::requests::Thread,
        result: Result<(), String>,
    },
    Adopted(Result<Vec<(super::requests::Thread, String)>, String>),
    Resolved {
        command: super::control::EnvoyCommand,
        reply: Reply,
        result: Result<super::slack::threads::ResolvedDestination, String>,
    },
    BindingPreview {
        command: super::control::EnvoyCommand,
        requester: Box<super::requests::Requester>,
        reply: Reply,
        result: Result<super::requests::BindingPreview, String>,
    },
    /// The user's member id as `auth.test` reports it, resolved on each configure.
    UserId(String),
    Error(String),
}

pub struct Worker {
    tx: Sender<Work>,
    rx: Receiver<Update>,
}

impl Worker {
    pub fn spawn(
        path: PathBuf,
        inbox: Inbox,
        io: impl SlackIo + 'static,
        bridge: PathBuf,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let (tx, commands) = unbounded();
        let (results, rx) = unbounded();
        std::thread::spawn(move || {
            let mut slack = SlackChannel::new(io, inbox, path, bridge, EnvoySettings::default());
            let mut active = false;
            let mut published: Option<(Inbox, BridgeStatus, Vec<String>)> = None;
            let mut published_at_ms: Option<i64> = None;
            let mut last_error: Option<String> = None;
            loop {
                match commands.recv_timeout(Duration::from_secs(2)) {
                    Ok(Work::Configure(settings)) => {
                        active = settings.is_some();
                        published = None;
                        if let Some(settings) = settings {
                            slack.configure(settings);
                            match slack.resolve_user_id() {
                                Ok(Some(id)) => {
                                    let _ = results.send(Update::UserId(id));
                                    wake();
                                }
                                Ok(None) => {}
                                Err(e) => log::warn!(
                                    "envoy: {user}'s Slack id not resolved: {e}",
                                    user = crate::user::name()
                                ),
                            }
                        }
                    }
                    Ok(Work::Watches(watches)) => {
                        let result = slack.watches(watches.clone());
                        let _ = results.send(Update::Watches { watches, result });
                        wake();
                    }
                    Ok(Work::RecordedReplies(keys)) => {
                        if let Err(e) = slack.acknowledge_replies(&keys) {
                            let _ = results.send(Update::Error(e));
                            wake();
                        }
                    }
                    Ok(Work::Adopt) => {
                        let _ = results.send(Update::Adopted(
                            slack.list_subscriptions(crate::control::server::now_ms()),
                        ));
                        wake();
                    }
                    Ok(Work::Remove(thread)) => {
                        let result = slack.remove_subscription(&thread);
                        let _ = results.send(Update::Removed { thread, result });
                        wake();
                    }
                    Ok(Work::Resolve { command, reply }) => {
                        let result = slack.resolve_destination(
                            command.args["conversation"].as_str().unwrap_or_default(),
                        );
                        let _ = results.send(Update::Resolved {
                            command,
                            reply,
                            result,
                        });
                        wake();
                    }
                    Ok(Work::BindingPreview {
                        command,
                        requester,
                        thread,
                        reply,
                    }) => {
                        let result = slack.binding_preview(&thread);
                        let _ = results.send(Update::BindingPreview {
                            command,
                            requester,
                            reply,
                            result,
                        });
                        wake();
                    }
                    Ok(Work::Forwarded(ids)) => {
                        if let Err(e) = slack.mark_forwarded(&ids) {
                            let _ = results.send(Update::Error(e));
                            wake();
                        }
                    }
                    Ok(Work::Send { id, message }) => {
                        let result = slack.send(&message);
                        if results.send(Update::Sent { id, result }).is_err() {
                            break;
                        }
                        wake();
                    }
                    Ok(Work::Thread { event, note, reply }) => {
                        let cached = slack.inbox.thread(&event);
                        let result = slack.thread(&event);
                        reply.send(Ok(match result {
                            Ok(thread) => {
                                let (thread, omitted) = trim_thread(thread);
                                json!({"event": event, "note": note, "thread": thread, "omitted": omitted, "complete": true})
                            }
                            Err(error) => {
                                let (thread, omitted) = trim_thread(cached);
                                json!({"event": event, "note": note, "thread": thread, "omitted": omitted, "complete": false, "error": error})
                            }
                        }));
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                }
                if active {
                    let now_ms = crate::control::server::now_ms();
                    let update = match slack.poll(now_ms) {
                        Ok(batch) => {
                            let bridge = slack.health();
                            let unchanged =
                                published.as_ref().is_some_and(|(inbox, status, notices)| {
                                    inbox == &slack.inbox
                                        && *status == bridge
                                        && *notices == batch.notices
                                });
                            if unchanged
                                && last_error.is_none()
                                && !heartbeat_due(published_at_ms, now_ms)
                            {
                                continue;
                            }
                            last_error = None;
                            published = Some((slack.inbox.clone(), bridge, batch.notices.clone()));
                            published_at_ms = Some(now_ms);
                            Update::Polled {
                                inbox: slack.inbox.clone(),
                                events: batch.events,
                                bridge,
                                notices: batch.notices,
                                polled_at_ms: now_ms,
                            }
                        }
                        Err(e) => {
                            if last_error.as_ref() == Some(&e) {
                                continue;
                            }
                            last_error = Some(e.clone());
                            Update::Error(e)
                        }
                    };
                    if results.send(update).is_err() {
                        break;
                    }
                    wake();
                }
            }
        });
        Self { tx, rx }
    }

    #[cfg(test)]
    pub(crate) fn test_pair() -> (Self, Receiver<Work>, Sender<Update>) {
        let (tx, commands) = unbounded();
        let (results, rx) = unbounded();
        (Self { tx, rx }, commands, results)
    }

    pub fn submit(&self, work: Work) -> Result<(), String> {
        self.tx.send(work).map_err(|e| {
            if let Work::Thread { reply, .. } = e.0 {
                reply.send(Err(ControlError::new(
                    ErrorCode::Unavailable,
                    "Envoy worker stopped",
                )));
            }
            "Envoy worker stopped".into()
        })
    }
    pub fn updates(&self) -> Vec<Update> {
        self.rx.try_iter().collect()
    }
}

/// An unchanged poll is still reported this often, so the app can tell a quiet loop from a
/// wedged one (see [`crate::envoy::POLL_STALE_MS`]).
pub const HEARTBEAT_MS: i64 = 60_000;

/// Whether an unchanged poll result must be published anyway.
pub fn heartbeat_due(published_at_ms: Option<i64>, now_ms: i64) -> bool {
    published_at_ms.is_none_or(|at| now_ms.saturating_sub(at) >= HEARTBEAT_MS)
}

/// Thread messages `envoy_event` returns: the root and the latest ones.
const THREAD_KEEP: usize = 15;
/// Characters kept of each thread message.
const THREAD_TEXT: usize = 1_500;

/// The root and the last [`THREAD_KEEP`] messages, each cut to [`THREAD_TEXT`] characters, and
/// how many were left out: agent threads run to hundreds of long messages.
pub fn trim_thread(mut thread: Vec<InboundEvent>) -> (Vec<InboundEvent>, usize) {
    let omitted = thread.len().saturating_sub(THREAD_KEEP + 1);
    if omitted > 0 {
        thread.drain(1..=omitted);
    }
    for e in &mut thread {
        if e.text.chars().count() > THREAD_TEXT {
            e.text = e.text.chars().take(THREAD_TEXT).collect::<String>() + "…";
        }
    }
    (thread, omitted)
}

#[cfg(test)]
mod trim_tests {
    use super::*;

    #[test]
    fn an_unchanged_poll_is_still_reported_once_a_minute() {
        assert!(heartbeat_due(None, 1_000));
        assert!(!heartbeat_due(Some(1_000), 1_000 + HEARTBEAT_MS - 1));
        assert!(heartbeat_due(Some(1_000), 1_000 + HEARTBEAT_MS));
    }

    #[test]
    fn long_threads_keep_the_root_and_the_latest_messages_cut_short() {
        let thread: Vec<InboundEvent> = (0..40)
            .map(|i| {
                let mut e: InboundEvent = serde_json::from_value(json!({
                    "channel_kind":"slack","conversation":"C1","ts":format!("{i}.000001"),
                    "thread_ts":"0.000001","user":"U1","user_name":"a","text":"x".repeat(i * 100),
                    "kind":"thread_reply","permalink":null
                }))
                .unwrap();
                e.ts = format!("{i}.000001");
                e
            })
            .collect();
        let (kept, omitted) = trim_thread(thread);
        assert_eq!(omitted, 24);
        assert_eq!(kept.len(), 16);
        assert_eq!(kept[0].ts, "0.000001");
        assert_eq!(kept[1].ts, "25.000001");
        assert_eq!(kept[15].text.chars().count(), THREAD_TEXT + 1);
    }
}

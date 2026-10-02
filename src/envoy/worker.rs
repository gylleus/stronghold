//! Slack subprocesses are serialized here; the UI only sends work and drains results.

use super::BridgeStatus;
use super::channel::{Channel, InboundEvent, OutboundMessage};
use super::inbox::Inbox;
use super::settings::EnvoySettings;
use super::slack::{SlackChannel, SlackCli};
use crate::control::{ControlError, ErrorCode, Reply};
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

pub enum Work {
    Configure(Option<EnvoySettings>),
    Forwarded(Vec<String>),
    Send {
        id: String,
        message: OutboundMessage,
    },
    Thread {
        event: InboundEvent,
        reply: Reply,
    },
}

pub enum Update {
    Polled {
        inbox: Inbox,
        events: Vec<InboundEvent>,
        bridge: BridgeStatus,
        notices: Vec<String>,
    },
    Sent {
        id: String,
        result: Result<(), String>,
    },
    Error(String),
}

pub struct Worker {
    tx: Sender<Work>,
    rx: Receiver<Update>,
}

impl Worker {
    pub fn spawn(path: PathBuf, inbox: Inbox, wake: impl Fn() + Send + 'static) -> Self {
        let (tx, commands) = unbounded();
        let (results, rx) = unbounded();
        std::thread::spawn(move || {
            let mut slack = SlackChannel::new(
                SlackCli,
                inbox,
                path,
                super::slack::bridge_dir(),
                EnvoySettings::default(),
            );
            let mut active = false;
            let mut published: Option<(Inbox, BridgeStatus, Vec<String>)> = None;
            let mut last_error: Option<String> = None;
            loop {
                match commands.recv_timeout(Duration::from_secs(2)) {
                    Ok(Work::Configure(settings)) => {
                        active = settings.is_some();
                        published = None;
                        if let Some(settings) = settings {
                            slack.configure(settings);
                        }
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
                    Ok(Work::Thread { event, reply }) => {
                        let cached = slack.inbox.thread(&event);
                        let result = slack.thread(&event);
                        reply.send(Ok(match result {
                            Ok(thread) => json!({"event": event, "thread": thread, "complete": true}),
                            Err(error) => json!({"event": event, "thread": cached, "complete": false, "error": error}),
                        }));
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                }
                if active {
                    let update = match slack.poll(crate::control::server::now_ms()) {
                        Ok(batch) => {
                            let bridge = slack.health();
                            let unchanged =
                                published.as_ref().is_some_and(|(inbox, status, notices)| {
                                    inbox == &slack.inbox
                                        && *status == bridge
                                        && *notices == batch.notices
                                });
                            if unchanged && last_error.is_none() {
                                continue;
                            }
                            last_error = None;
                            published = Some((slack.inbox.clone(), bridge, batch.notices.clone()));
                            Update::Polled {
                                inbox: slack.inbox.clone(),
                                events: batch.events,
                                bridge,
                                notices: batch.notices,
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

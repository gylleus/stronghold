//! Who plays which part in a Run: the terminal Orca records as coordinator is often an
//! attendant, a cheap watcher that ran `run-create` and forwards bus events to the real
//! coordinator with `orca terminal send`.

use std::collections::BTreeSet;

use super::{FleetData, ts_ms};
use crate::orca::{InboxMessage, MessageType};

/// One terminal a Claude terminal sent input to with `orca terminal send --terminal <target>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    pub target: String,
    /// Time of the newest send to `target`, in ms since the epoch.
    pub at_ms: Option<i64>,
}

/// Why a Run's declared coordinator counts as its attendant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttendantSignal {
    /// Its title says "attendant".
    Title,
    /// Its transcript forwards input to a terminal that is not one of the Run's workers.
    Forwards,
    /// Other live terminals create the Run's tasks or message its workers; it does neither.
    OthersCoordinate,
}

/// The terminals playing a Run's parts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunRoles {
    /// `coordinator_handle` as Orca reports it (the terminal that ran `run-create`).
    pub declared: Option<String>,
    /// The live terminal coordinating the Run, the one Karl talks to.
    pub coordinator: Option<String>,
    /// The live declared terminal when it is an attendant forwarding to the coordinator.
    pub attendant: Option<String>,
    /// What gave the attendant away; `None` when there is no attendant.
    pub signal: Option<AttendantSignal>,
}

/// Bus senders that are never a coordinator terminal.
fn is_service_sender(handle: &str) -> bool {
    handle == "queen"
        || handle.starts_with("myrstack-")
        || handle.starts_with("run:")
        || handle.starts_with("dispatch:")
}

/// Message types only workers send; their senders are never coordinators, even when the
/// worker list misses them.
fn is_worker_message(m: &InboxMessage) -> bool {
    matches!(
        m.message_type,
        Some(
            MessageType::Heartbeat
                | MessageType::WorkerDone
                | MessageType::MergeReady
                | MessageType::Question
                | MessageType::Escalation
        )
    )
}

impl FleetData {
    /// Live terminals Orca records as some Run's coordinator: the ones that may be attendants,
    /// whose transcripts the Claude worker reads for forwards.
    pub fn declared_coordinators(&self) -> BTreeSet<String> {
        self.runs
            .iter()
            .filter_map(|r| r.coordinator_handle.as_deref())
            .filter(|h| self.terminal(h).is_some())
            .map(str::to_string)
            .collect()
    }

    /// Coordinator and attendant of a Run.
    ///
    /// The live declared terminal D is an attendant when its title contains "attendant", when
    /// its transcript forwards input to a terminal that is not a worker of the Run, or when
    /// other live terminals create the Run's tasks or message its workers while D does neither.
    /// The coordinator is then the most recent live of: D's forward targets, the Run's task
    /// creators and the Run's bus senders (never D, the Run's workers or service handles).
    /// Otherwise the coordinator is D, or the rebound fallback when D is gone.
    pub fn run_roles(&self, run_id: &str) -> RunRoles {
        let declared = self.run(run_id).and_then(|r| r.coordinator_handle.clone());
        let live_declared = declared.as_deref().filter(|h| self.terminal(h).is_some());
        if let Some(d) = live_declared {
            let workers = self.run_worker_handles(run_id);
            if let Some(signal) = self.attendant_signal(run_id, d, &workers) {
                return RunRoles {
                    coordinator: self.coordinator_behind(run_id, d, &workers),
                    attendant: Some(d.to_string()),
                    signal: Some(signal),
                    declared,
                };
            }
        }
        RunRoles {
            coordinator: self.declared_or_rebound_coordinator(run_id),
            declared,
            attendant: None,
            signal: None,
        }
    }

    /// Terminals working the Run's tasks: dispatched workers and task assignees.
    fn run_worker_handles(&self, run_id: &str) -> BTreeSet<&str> {
        let dispatched = self
            .workers
            .iter()
            .filter(|w| w.run_id.as_deref() == Some(run_id))
            .filter_map(|w| w.agent_terminal_handle.as_deref());
        let assignees = self
            .tasks
            .get(run_id)
            .into_iter()
            .flatten()
            .filter_map(|t| t.assignee_handle.as_deref());
        dispatched.chain(assignees).collect()
    }

    fn run_messages<'a>(&'a self, run_id: &'a str) -> impl Iterator<Item = &'a InboxMessage> + 'a {
        let home = format!("run:{run_id}");
        self.inbox.iter().filter(move |m| {
            m.run_id.as_deref() == Some(run_id) || m.to_handle.as_deref() == Some(home.as_str())
        })
    }

    fn attendant_signal(
        &self,
        run_id: &str,
        declared: &str,
        workers: &BTreeSet<&str>,
    ) -> Option<AttendantSignal> {
        let title = self
            .terminal(declared)
            .and_then(|t| t.title.as_deref())
            .unwrap_or_default();
        if title.to_lowercase().contains("attendant") {
            return Some(AttendantSignal::Title);
        }
        let forwards_elsewhere = self
            .forwards
            .get(declared)
            .into_iter()
            .flatten()
            .any(|f| f.target != declared && !workers.contains(f.target.as_str()));
        if forwards_elsewhere {
            return Some(AttendantSignal::Forwards);
        }
        let other = |h: &str| h != declared && !workers.contains(h) && self.terminal(h).is_some();
        let tasks = self.tasks.get(run_id).map(Vec::as_slice).unwrap_or(&[]);
        let creators: Vec<&str> = tasks
            .iter()
            .filter_map(|t| t.created_by_terminal_handle.as_deref())
            .collect();
        let others_create = creators.iter().any(|h| other(h)) && !creators.contains(&declared);
        let coordinating: Vec<&str> = self
            .run_messages(run_id)
            .filter(|m| is_coordinator_message(m, workers))
            .filter_map(|m| m.from_handle.as_deref())
            .collect();
        let others_message = coordinating
            .iter()
            .any(|h| other(h) && !is_service_sender(h))
            && !coordinating.contains(&declared);
        (others_create || others_message).then_some(AttendantSignal::OthersCoordinate)
    }

    /// The live terminal an attendant works for: the most recent of its forward targets, the
    /// Run's task creators and the Run's bus senders.
    fn coordinator_behind(
        &self,
        run_id: &str,
        attendant: &str,
        workers: &BTreeSet<&str>,
    ) -> Option<String> {
        let eligible = |h: &str| {
            h != attendant
                && !workers.contains(h)
                && !is_service_sender(h)
                && self.terminal(h).is_some()
        };
        let forwards = self
            .forwards
            .get(attendant)
            .into_iter()
            .flatten()
            .map(|f| ((f.at_ms, 2), f.target.as_str()));
        let creators = self
            .tasks
            .get(run_id)
            .into_iter()
            .flatten()
            .filter_map(|t| {
                Some((
                    (ts_ms(&t.created_at), 1),
                    t.created_by_terminal_handle.as_deref()?,
                ))
            });
        let senders = self
            .run_messages(run_id)
            .filter(|m| !is_worker_message(m))
            .filter_map(|m| Some(((ts_ms(&m.created_at), 0), m.from_handle.as_deref()?)));
        forwards
            .chain(creators)
            .chain(senders)
            .filter(|(_, h)| eligible(h))
            .max_by_key(|(order, _)| *order)
            .map(|(_, h)| h.to_string())
    }
}

/// A message only a coordinator sends: a dispatch, or anything addressed to a worker.
fn is_coordinator_message(m: &InboxMessage, workers: &BTreeSet<&str>) -> bool {
    if m.message_type == Some(MessageType::Dispatch) {
        return true;
    }
    let from_worker = m
        .from_handle
        .as_deref()
        .is_some_and(|h| workers.contains(h));
    let to = m.to_handle.as_deref().unwrap_or_default();
    !from_worker && !is_worker_message(m) && (to.starts_with("dispatch:") || workers.contains(to))
}

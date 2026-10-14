//! Who plays which part in a Run: the terminal Orca records as coordinator is often an
//! attendant, a cheap watcher that ran `run-create` and forwards bus events to the real
//! coordinator with `orca terminal send`. Attendants are recognised by behaviour; a title
//! containing "attendant" is only a hint, because a coordinator's topic can be an attendant.

use std::collections::BTreeSet;

use super::{ClanData, ts_ms};
use crate::orca::{InboxMessage, MessageType};

/// One terminal a Claude terminal sent input to with `orca terminal send --terminal <target>`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Forward {
    pub target: String,
    /// Time of the newest send to `target`, in ms since the epoch.
    pub at_ms: Option<i64>,
}

/// Why a Run's declared coordinator counts as its attendant. Always behaviour: a title that
/// says "attendant" is only a hint, see [`RunRoles::maybe_attendant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttendantSignal {
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
    /// The live terminal coordinating the Run, the one the user talks to.
    pub coordinator: Option<String>,
    /// The live declared terminal when it is an attendant forwarding to the coordinator.
    pub attendant: Option<String>,
    /// What gave the attendant away; `None` when there is no attendant.
    pub signal: Option<AttendantSignal>,
    /// Low confidence: the declared terminal is kept as coordinator but its title says
    /// "attendant" and it has not yet coordinated (created the Run's tasks or messaged its
    /// workers). The UI may show it as "maybe attendant".
    pub maybe_attendant: bool,
}

/// Bus senders that are never a coordinator terminal.
fn is_service_sender(handle: &str) -> bool {
    crate::orca::is_broadcast_sender(handle)
        || handle.starts_with("stronghold-")
        // Run history from before the rename still has the whip as `myrstack-whip`.
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

impl ClanData {
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
    /// The live declared terminal D is judged on behaviour (the precedence is documented on
    /// `DeclaredEvidence::attendant_signal`); its title alone never makes it an attendant. When D is an attendant the coordinator is
    /// the most recent live of: D's forward targets, the Run's task creators and the Run's bus
    /// senders (never D, the Run's workers or service handles), or `None` when all are gone.
    /// Otherwise the coordinator is D, or the rebound fallback when D is gone.
    pub fn run_roles(&self, run_id: &str) -> RunRoles {
        let declared = self.run(run_id).and_then(|r| r.coordinator_handle.clone());
        let live_declared = declared.as_deref().filter(|h| self.terminal(h).is_some());
        let mut maybe_attendant = false;
        if let Some(d) = live_declared {
            let workers = self.run_worker_handles(run_id);
            let evidence = self.declared_evidence(run_id, d, &workers);
            if let Some(signal) = evidence.attendant_signal() {
                return RunRoles {
                    coordinator: self.coordinator_behind(run_id, d, &workers),
                    attendant: Some(d.to_string()),
                    signal: Some(signal),
                    maybe_attendant: false,
                    declared,
                };
            }
            maybe_attendant = evidence.title && !evidence.creates && !evidence.messages_workers;
        }
        RunRoles {
            coordinator: self.declared_or_rebound_coordinator(run_id),
            declared,
            attendant: None,
            signal: None,
            maybe_attendant,
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

    /// What the live declared terminal `declared` did in the Run.
    fn declared_evidence(
        &self,
        run_id: &str,
        declared: &str,
        workers: &BTreeSet<&str>,
    ) -> DeclaredEvidence {
        let title = self
            .terminal(declared)
            .and_then(|t| t.title.as_deref())
            .is_some_and(|t| t.to_lowercase().contains("attendant"));
        let targets: Vec<&str> = self
            .forwards
            .get(declared)
            .into_iter()
            .flatten()
            .map(|f| f.target.as_str())
            .filter(|t| *t != declared && !workers.contains(t))
            .collect();
        let other = |h: &str| h != declared && !workers.contains(h) && self.terminal(h).is_some();
        let tasks = self.tasks.get(run_id).map(Vec::as_slice).unwrap_or(&[]);
        let creators: Vec<&str> = tasks
            .iter()
            .filter_map(|t| t.created_by_terminal_handle.as_deref())
            .collect();
        let coordinating: Vec<&str> = self
            .run_messages(run_id)
            .filter(|m| is_coordinator_message(m, workers))
            .filter_map(|m| m.from_handle.as_deref())
            .collect();
        DeclaredEvidence {
            title,
            forwards: !targets.is_empty(),
            forwards_to_live: targets.iter().any(|t| self.terminal(t).is_some()),
            creates: creators.contains(&declared),
            messages_workers: coordinating.contains(&declared),
            others_create: creators.iter().any(|h| other(h)),
            others_message: coordinating
                .iter()
                .any(|h| other(h) && !is_service_sender(h)),
        }
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

/// What a Run's live declared terminal did; decides whether it is the attendant.
#[derive(Debug, Clone, Copy, Default)]
struct DeclaredEvidence {
    /// Its title says "attendant": a hint only, never enough on its own.
    title: bool,
    /// It forwarded input to a terminal that is not itself or one of the Run's workers.
    forwards: bool,
    /// ...and at least one such forward target is live.
    forwards_to_live: bool,
    /// It created some of the Run's tasks.
    creates: bool,
    /// It dispatched or messaged the Run's workers on the bus.
    messages_workers: bool,
    /// Another live non-worker terminal created some of the Run's tasks.
    others_create: bool,
    /// Another live non-worker, non-service terminal dispatched or messaged the Run's workers.
    others_message: bool,
}

impl DeclaredEvidence {
    /// Whether the declared terminal is an attendant, and why. In order of precedence:
    ///
    /// 1. It forwards to a live non-worker terminal: attendant, even when it created the Run's
    ///    tasks. Attendants often run `run-create`/`task-create` for the coordinator they
    ///    forward to, and the live target is the better coordinator candidate.
    /// 2. It created the Run's tasks or messaged its workers: coordinator, whatever its title
    ///    says and even when its forwards all went to terminals that are now gone.
    /// 3. Other live terminals create the Run's tasks or message its workers while it does
    ///    neither: attendant.
    /// 4. It forwards only to terminals that are gone and does not coordinate: an attendant
    ///    whose coordinator died.
    /// 5. Otherwise coordinator; with an "attendant" title this is low confidence
    ///    ([`RunRoles::maybe_attendant`]), since a false attendant hides the coordinator.
    fn attendant_signal(&self) -> Option<AttendantSignal> {
        if self.forwards_to_live {
            return Some(AttendantSignal::Forwards);
        }
        if self.creates || self.messages_workers {
            return None;
        }
        if self.others_create || self.others_message {
            return Some(AttendantSignal::OthersCoordinate);
        }
        self.forwards.then_some(AttendantSignal::Forwards)
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

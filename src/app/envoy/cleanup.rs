//! UI cleanup is saved separately so queued ingestion snapshots cannot undo the user's decisions.

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Cleanup {
    pub events: BTreeSet<String>,
    pub drafts: BTreeSet<String>,
    requests: BTreeMap<String, (usize, usize)>,
}

impl Cleanup {
    pub fn draft_open(&self, draft: &Draft) -> bool {
        matches!(draft.status, DraftStatus::Pending | DraftStatus::Failed)
            && !self.drafts.contains(&draft.id)
    }
}

impl AppEnvoy {
    fn save_cleanup(&mut self, cleanup: Cleanup) -> Result<(), String> {
        crate::envoy::inbox::save(&self.cleanup_path, &cleanup).map_err(|e| e.to_string())?;
        self.cleanup = cleanup;
        self.resolve_handled();
        Ok(())
    }

    pub(super) fn resolve_handled(&mut self) {
        let mut resolved = self.cleanup.events.clone();
        for draft in self.drafts() {
            if (!matches!(
                draft.status,
                DraftStatus::Pending | DraftStatus::Approved | DraftStatus::Failed
            ) || self.cleanup.drafts.contains(&draft.id))
                && let Some(reply) = &draft.in_reply_to
            {
                resolved.insert(format!("{}:{}", reply.conversation, reply.ts));
            }
        }
        self.inbox.resolve(resolved);
        let open: BTreeSet<_> = self.inbox.open_events().map(InboundEvent::id).collect();
        self.pending.retain(|e| open.contains(&e.id()));
        if self.pending.is_empty() {
            self.pending_since_ms = None;
            self.stuck_notified_ms = None;
        }
    }

    pub(super) fn dismiss_event(&mut self, id: &str) -> Result<(), String> {
        if self.inbox.get(id).is_none() {
            return Err("Unknown inbox event".into());
        }
        let mut cleanup = self.cleanup.clone();
        cleanup.events.insert(id.into());
        self.save_cleanup(cleanup)
    }

    pub(super) fn dismiss_draft(&mut self, id: &str) -> Result<(), String> {
        let draft = self.queue()?.get(id).ok_or("Unknown draft")?;
        if matches!(draft.status, DraftStatus::Pending | DraftStatus::Approved) {
            return Err("Reject or finish the draft before dismissing it".into());
        }
        let mut cleanup = self.cleanup.clone();
        cleanup.drafts.insert(id.into());
        self.save_cleanup(cleanup)
    }

    pub(super) fn dismissed_requests(&self) -> BTreeSet<String> {
        self.requests()
            .iter()
            .filter(|r| {
                r.binding_proposal.is_none()
                    && self.cleanup.requests.get(&r.id)
                        == Some(&(r.turns.len(), r.deliveries.len()))
            })
            .map(|r| r.id.clone())
            .collect()
    }

    pub(super) fn dismiss_request(&mut self, id: &str) -> Result<(), String> {
        let request = self.requests_mut()?.get(id)?;
        if request.binding_proposal.is_some()
            || request.turns.iter().any(|t| {
                matches!(
                    t.status,
                    crate::envoy::requests::Status::Queued
                        | crate::envoy::requests::Status::Drafted
                        | crate::envoy::requests::Status::Approved
                )
            })
        {
            return Err("Resolve pending decisions before dismissing".into());
        }
        let version = (request.turns.len(), request.deliveries.len());
        let mut cleanup = self.cleanup.clone();
        cleanup.requests.insert(id.into(), version);
        self.save_cleanup(cleanup)
    }

    pub(super) fn clear_handled(&mut self) -> Result<(), String> {
        let mut cleanup = self.cleanup.clone();
        for draft in self.drafts() {
            if !matches!(draft.status, DraftStatus::Pending | DraftStatus::Approved) {
                cleanup.drafts.insert(draft.id.clone());
            }
        }
        for request in self.requests() {
            use crate::envoy::requests::Status;
            if request.binding_proposal.is_none()
                && (request.settled
                    || (!request.turns.is_empty()
                        && request.turns.iter().all(|t| {
                            matches!(
                                t.status,
                                Status::Answered | Status::Declined | Status::Failed
                            ) || (t.status == Status::Sent && !t.expect_reply)
                        })))
            {
                cleanup.requests.insert(
                    request.id.clone(),
                    (request.turns.len(), request.deliveries.len()),
                );
            }
        }
        let now = now_ms();
        for event in self.inbox.events() {
            let has_active_draft = self.drafts().iter().any(|d| {
                matches!(d.status, DraftStatus::Pending | DraftStatus::Approved)
                    && d.in_reply_to
                        .as_ref()
                        .is_some_and(|r| r.conversation == event.conversation && r.ts == event.ts)
            });
            if !has_active_draft
                && (!self.inbox.is_open(&event.id())
                    || timestamp_us(&event.ts).is_none_or(|ts| ts / 1000 < now - BACKLOG_AFTER_MS))
            {
                cleanup.events.insert(event.id());
            }
        }
        self.save_cleanup(cleanup)
    }

    pub(in crate::app) fn notice_resolved(&self, entry: &crate::notify::history::Entry) -> bool {
        if entry.source != Source::Envoy {
            return false;
        }
        match entry.title.as_str() {
            "Envoy draft ready" => {
                if let Some(id) = entry
                    .sender
                    .as_deref()
                    .filter(|s| s.starts_with("envoy-draft-"))
                {
                    self.drafts()
                        .iter()
                        .find(|d| d.id == id)
                        .is_some_and(|d| d.status != DraftStatus::Pending)
                } else {
                    // Legacy alerts had no draft ID. Keep them until every draft they
                    // could refer to has been decided; newer drafts have their own alerts.
                    self.drafts.is_some()
                        && !self.drafts().iter().any(|d| {
                            d.status == DraftStatus::Pending && d.created_ms <= entry.at_ms
                        })
                }
            }
            "Envoy events stuck" => self.inbox.unforwarded().is_empty(),
            _ => false,
        }
    }
}

pub(super) fn draft_notice(draft: &Value) -> Incoming {
    let mut notice = Incoming::new(
        Source::Envoy,
        Severity::Info,
        "Envoy draft ready",
        "A draft is waiting for your approval.",
    )
    .with_target(DRAFTS_TARGET);
    notice.sender = draft["id"].as_str().map(str::to_owned);
    notice
}

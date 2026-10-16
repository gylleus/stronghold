//! UI cleanup is saved separately so queued ingestion snapshots cannot undo the user's decisions.

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Cleanup {
    pub events: BTreeSet<String>,
    pub deleted_events: BTreeSet<String>,
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

    pub(super) fn delete_event(&mut self, id: &str) -> Result<(), String> {
        let mut cleanup = self.cleanup.clone();
        cleanup.deleted_events.insert(id.into());
        cleanup.events.remove(id);
        self.save_cleanup(cleanup)?;
        self.inbox.delete(&[id.into()]);
        self.resolve_handled();
        self.flush_event_deletions()
    }

    pub(super) fn flush_event_deletions(&mut self) -> Result<(), String> {
        if !self.event_deletion_pending && !self.cleanup.deleted_events.is_empty() {
            self.worker()?.submit(Work::DeleteEvents(
                self.cleanup.deleted_events.iter().cloned().collect(),
            ))?;
            self.event_deletion_pending = true;
        }
        Ok(())
    }

    pub(super) fn events_deleted(&mut self, ids: &[String]) -> Result<(), String> {
        let mut cleanup = self.cleanup.clone();
        cleanup.deleted_events.retain(|id| !ids.contains(id));
        self.save_cleanup(cleanup)
    }

    pub(super) fn delete_draft(&mut self, id: &str) -> Result<(), String> {
        let Some(draft) = self.queue()?.get(id).cloned() else {
            return Ok(());
        };
        if let Some(link) = &draft.request
            && self.requests_mut()?.get(&link.id).is_ok()
        {
            self.requests_mut()?.retain_draft_receipt(&draft)?;
            if matches!(draft.status, DraftStatus::Pending | DraftStatus::Failed) {
                self.queue()?.decline(
                    id,
                    &crate::user::fill("draft deleted by {user}"),
                    now_ms(),
                )?;
                self.reconcile_requests()?;
            }
            self.requests_mut()?.change(&link.id, |r| {
                if let Some(turn) = r.turns.iter_mut().find(|t| t.id == link.turn_id) {
                    turn.draft_id = None;
                    if draft.status == DraftStatus::Approved {
                        turn.status = crate::envoy::requests::Status::Failed;
                        turn.error = Some(
                            "Draft deleted while sending; delivery may already have started".into(),
                        );
                        r.status = turn.status;
                    }
                }
                Ok(())
            })?;
        }
        if !matches!(
            draft.status,
            DraftStatus::Pending | DraftStatus::Approved | DraftStatus::Failed
        ) && let Some(reply) = &draft.in_reply_to
            && self
                .inbox
                .get(&format!("{}:{}", reply.conversation, reply.ts))
                .is_some()
        {
            let mut cleanup = self.cleanup.clone();
            cleanup
                .events
                .insert(format!("{}:{}", reply.conversation, reply.ts));
            self.save_cleanup(cleanup)?;
        }
        self.queue()?.delete(id)?;
        if draft.status == DraftStatus::Approved
            && let Some(worker) = &self.worker
        {
            worker.cancel_send(id);
        }
        self.outgoing.remove(id);
        let mut cleanup = self.cleanup.clone();
        cleanup.drafts.remove(id);
        self.save_cleanup(cleanup)?;
        self.reconcile_requests()
    }

    pub(super) fn delete_request(&mut self, id: &str) -> Result<(), String> {
        let Ok(request) = self.requests_mut()?.get(id).cloned() else {
            return self.reconcile_requests();
        };
        let thread = request
            .thread
            .as_ref()
            .filter(|thread| request.reply_cutoff().is_some() && !self.borrowed_thread(thread))
            .cloned();
        let drafts: Vec<_> = self
            .drafts()
            .iter()
            .filter(|d| d.request.as_ref().is_some_and(|r| r.id == id))
            .map(|d| d.id.clone())
            .collect();
        self.requests_mut()?.delete(id, drafts, thread)?;
        self.reconcile_requests()
    }

    pub(super) fn finish_request_deletions(&mut self) -> Result<(), String> {
        let deletions: Vec<_> = self
            .requests
            .as_ref()
            .ok_or("Request store unavailable")?
            .deletions()
            .iter()
            .filter(|d| {
                !d.drafts.is_empty()
                    || self.cleanup.requests.contains_key(&d.id)
                    || (d.delivery.delivered && d.thread.is_none())
            })
            .cloned()
            .collect();
        let mut cleanup = self.cleanup.clone();
        for deletion in &deletions {
            cleanup.requests.remove(&deletion.id);
            for id in &deletion.drafts {
                let sending = self
                    .queue()?
                    .get(id)
                    .is_some_and(|d| d.status == DraftStatus::Approved);
                // Request deletion is already durable; draft cleanup can retry after cancellation.
                if sending && let Some(worker) = &self.worker {
                    worker.cancel_send(id);
                }
                self.queue()?.delete(id)?;
                self.outgoing.remove(id);
                cleanup.drafts.remove(id);
            }
        }
        if !deletions.is_empty() {
            self.save_cleanup(cleanup)?;
            self.requests_mut()?.change_deletions(|records| {
                for record in records {
                    record.drafts.clear();
                }
            })?;
        }
        Ok(())
    }

    pub(super) fn delete_items(
        &mut self,
        items: crate::ui::envoy::PanelItems,
    ) -> Result<(), String> {
        for id in items.requests {
            self.delete_request(&id)?;
        }
        for id in items.drafts {
            self.delete_draft(&id)?;
        }
        for id in items.events {
            self.delete_event(&id)?;
        }
        Ok(())
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
            return Err("Decide whether to send the draft before hiding it".into());
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
            return Err("Resolve pending decisions before hiding".into());
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
                        .is_none_or(|d| d.status != DraftStatus::Pending)
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

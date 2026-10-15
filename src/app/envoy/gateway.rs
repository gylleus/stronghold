//! The gateway shares draft approvals and terminal readiness with the existing Envoy flows.

use super::*;
use crate::envoy::drafts::RequestLink;
use crate::envoy::requests::{ReachOut, Request, Requester, Requests, Status, Thread};
use crate::envoy::settings::ApprovalMode;
use crate::envoy::slack::threads::Watch;

pub(super) enum GatewayAction {
    Requests(Vec<(String, u64, String)>),
    Reply {
        request: String,
        delivery: String,
        target: String,
    },
}

impl AppEnvoy {
    pub fn requests(&self) -> &[Request] {
        self.requests.as_ref().map_or(&[], Requests::all)
    }
    pub(super) fn requests_mut(&mut self) -> Result<&mut Requests, String> {
        self.requests
            .as_mut()
            .ok_or("Request store unavailable".into())
    }

    pub(super) fn gateway_request(
        &mut self,
        command: &EnvoyCommand,
        who: Requester,
    ) -> Result<Value, String> {
        match command.method.as_str() {
            "reach_out" => {
                let args: ReachOut = serde_json::from_value(command.args.clone())
                    .map_err(|e| format!("Invalid reach_out: {e}"))?;
                let user_id = self.settings.user_id.clone();
                Ok(json!(self.requests_mut()?.submit(
                    who,
                    args,
                    &user_id,
                    now_ms()
                )?))
            }
            "reach_out_status" => {
                let id = optional(&command.args, "request_id")?;
                let requests: Vec<_> = self
                    .requests()
                    .iter()
                    .filter(|r| r.owned_by(&who) && id.is_none_or(|id| id == r.id))
                    .collect();
                if id.is_some() && requests.is_empty() {
                    return Err("Request unavailable".into());
                }
                Ok(json!({"requests":requests}))
            }
            _ => Err("Unknown gateway method".into()),
        }
    }

    pub(super) fn route(
        &mut self,
        args: &Value,
        destination: &crate::envoy::slack::threads::ResolvedDestination,
    ) -> Result<Value, String> {
        let conversation = destination.conversation.as_str();
        self.reconcile_requests()?;
        crate::envoy::requests::refuse_user(conversation, &self.settings.user_id)?;
        crate::envoy::slack::validate_conversation(conversation)?;
        let request = self.requests_mut()?.get(required(args, "id")?)?.clone();
        let turn = request.next_turn(turn_id(args)?)?;
        if let Some(id) = &turn.draft_id {
            return Ok(
                json!({"request_id":request.id,"turn_id":turn.id,"draft":self.queue()?.get(id)}),
            );
        }
        if turn.status != Status::Queued {
            return Err("Turn is not queued".into());
        }
        let agent = match required(args, "destination_kind")? {
            "agent" => true,
            "person" => false,
            _ => return Err("destination_kind must be agent or person".into()),
        };
        if !(agent || conversation.starts_with('D') && destination.person) {
            return Err(
                "Rewritten asks require a verified human DM; use destination_kind agent".into(),
            );
        }
        if agent && args.get("draft_text").is_some() {
            return Err("Agent destinations do not accept draft_text".into());
        }
        let requested_thread = optional(args, "thread_ts")?;
        let thread_ts = if let Some(thread) = &request.thread {
            if thread.conversation != conversation
                || requested_thread.is_some_and(|ts| ts != thread.ts)
            {
                return Err("Route conflicts with the request thread".into());
            }
            Some(thread.ts.clone())
        } else {
            requested_thread.map(str::to_owned)
        };
        if request.requires_thread(turn.id) && request.thread.is_none() && !self.settings.dry_run {
            return Err("Follow-up has no confirmed live thread".into());
        }
        let link = RequestLink {
            id: request.id.clone(),
            turn_id: turn.id,
            original_ask: turn.ask.clone(),
            requester: request
                .requester
                .as_ref()
                .map_or("unknown", |r| r.label.as_str())
                .into(),
            why: turn.why.clone(),
            agent,
            karl_asked: turn.karl_asked.clone(),
        };
        let text = if agent {
            link.verbatim()
        } else {
            required(args, "draft_text")?.into()
        };
        let mut draft = Draft::outgoing(
            conversation.into(),
            thread_ts,
            text,
            required(args, "rationale")?.into(),
            link,
            now_ms(),
        );
        draft.manual_approval = match args.get("manual_approval") {
            None => false,
            Some(value) => value.as_bool().ok_or("manual_approval must be a boolean")?,
        };
        let draft = self.queue()?.create(draft)?;
        self.schedule_draft(&draft.id)?;
        self.reconcile_requests()?;
        Ok(json!({"request_id":request.id,"turn_id":turn.id,"draft":self.queue()?.get(&draft.id)}))
    }

    pub(super) fn gateway_envoy_request(
        &mut self,
        command: &EnvoyCommand,
    ) -> Result<Value, String> {
        let args = &command.args;
        match command.method.as_str() {
            "envoy_request" => {
                let request = self.requests_mut()?.get(required(args, "id")?)?.clone();
                let note = request.thread.as_ref().and_then(|t| {
                    self.notes
                        .as_ref()?
                        .for_thread(&t.conversation, &t.ts)
                        .cloned()
                });
                Ok(json!({"request":request, "note":note}))
            }
            "envoy_decline" => {
                self.reconcile_requests()?;
                let id = required(args, "id")?;
                let reason = required(args, "reason")?;
                let turn = self
                    .requests_mut()?
                    .get(id)?
                    .next_turn(turn_id(args)?)?
                    .clone();
                if !matches!(
                    turn.status,
                    Status::Queued | Status::Drafted | Status::Failed
                ) {
                    return Err("Only unsent turns can be declined".into());
                }
                if let Some(draft) = &turn.draft_id
                    && matches!(turn.status, Status::Drafted | Status::Failed)
                {
                    self.queue()?.decline(draft, reason, now_ms())?;
                    self.reconcile_requests()?;
                } else {
                    self.requests_mut()?.change(id, |r| {
                        let t = r.turns.iter_mut().find(|t| t.id == turn.id).unwrap();
                        t.status = Status::Declined;
                        t.error = Some(reason.into());
                        r.status = Status::Declined;
                        if !r.requires_thread(turn.id) && r.thread.is_none() {
                            r.settled = true;
                            for t in &mut r.turns {
                                if t.status == Status::Queued {
                                    t.status = Status::Declined;
                                }
                            }
                        }
                        r.deliveries.push(crate::envoy::requests::Delivery {
                            id: format!("decline:{}", turn.id),
                            text: format!("Envoy: request {id} declined: {reason}"),
                            event: None,
                            delivered: false,
                            dry_run: false,
                            inbox: false,
                        });
                        Ok(())
                    })?;
                }
                Ok(json!({"request":self.requests_mut()?.get(id)?}))
            }
            "envoy_link_reply" => {
                let id = required(args, "request_id")?;
                if let Some(event_id) = optional(args, "event_id")? {
                    let event = self.reply_to_link(id, event_id)?;
                    self.requests_mut()?.change(id, |r| {
                        let thread = Thread {
                            conversation: event.conversation.clone(),
                            ts: event.thread_ts.clone().unwrap_or_else(|| event.ts.clone()),
                        };
                        r.thread = Some(thread);
                        Ok(())
                    })?;
                    let failed: Vec<_> = self
                        .drafts()
                        .iter()
                        .filter(|d| {
                            d.status == DraftStatus::Failed
                                && d.request.as_ref().is_some_and(|r| r.id == id)
                        })
                        .map(|d| d.id.clone())
                        .collect();
                    for draft_id in failed {
                        self.queue()?.resolve_failed(
                            &draft_id,
                            crate::envoy::channel::SendReceipt {
                                conversation: event.conversation.clone(),
                                ts: event.thread_ts.clone().unwrap_or_else(|| event.ts.clone()),
                            },
                            now_ms(),
                        )?;
                    }
                    self.reconcile_requests()?;
                    self.requests_mut()?.reply(id, &event)?;
                } else if optional(args, "requester")?.is_none() {
                    return Err("event_id or requester is required".into());
                }
                Ok(json!({"request":self.requests_mut()?.get(id)?}))
            }
            _ => Err("Unknown gateway method".into()),
        }
    }

    pub(super) fn binding_thread(
        &mut self,
        args: &Value,
        who: &Requester,
    ) -> Result<Option<Thread>, String> {
        let id = required(args, "request_id")?;
        if let Some(event_id) = optional(args, "event_id")? {
            self.reply_to_link(id, event_id)?;
        }
        let request = self.requests_mut()?.get(id)?;
        if request.settled {
            return Err("Request closed; start a new reach_out".into());
        }
        if request.bound_requester().is_some() {
            return if request.owned_by(who) {
                Ok(None)
            } else {
                Err("A known requester cannot be reassigned".into())
            };
        }
        let reason = required(args, "reason")?;
        if reason.trim().is_empty() || reason.len() > 4000 {
            return Err("Binding reason is empty or too long".into());
        }
        Ok(Some(
            request
                .thread
                .clone()
                .ok_or("Binding requires a known thread")?,
        ))
    }

    pub(super) fn propose_binding(
        &mut self,
        args: &Value,
        who: Requester,
        preview: crate::envoy::requests::BindingPreview,
    ) -> Result<(), String> {
        let Some(thread) = self.binding_thread(args, &who)? else {
            return Ok(());
        };
        if thread != preview.thread {
            return Err("Binding thread changed during preview".into());
        }
        self.requests_mut()?
            .change(required(args, "request_id")?, |r| {
                r.binding_sequence += 1;
                r.binding_proposal = Some(crate::envoy::requests::BindingProposal {
                    id: r.binding_sequence,
                    requester: who,
                    reason: required(args, "reason")?.into(),
                    preview,
                });
                Ok(())
            })
    }

    pub(super) fn decide_binding(
        &mut self,
        id: &str,
        proposal_id: u64,
        accept: bool,
    ) -> Result<(), String> {
        self.requests_mut()?.change(id, |r| {
            let proposal = r
                .binding_proposal
                .as_ref()
                .ok_or("Binding proposal unavailable")?;
            if r.settled
                || proposal.id != proposal_id
                || r.thread.as_ref() != Some(&proposal.preview.thread)
            {
                return Err("Binding proposal changed; review it again".into());
            }
            if accept {
                r.requester = Some(proposal.requester.clone());
                r.binding_approved = true;
                for d in &mut r.deliveries {
                    d.inbox = false;
                }
            }
            r.binding_proposal = None;
            Ok(())
        })
    }

    fn reply_to_link(&mut self, id: &str, event_id: &str) -> Result<InboundEvent, String> {
        let event = self
            .inbox
            .get(event_id)
            .cloned()
            .ok_or("Unknown retained event")?;
        let request = self.requests_mut()?.get(id)?;
        if request.thread.as_ref().is_some_and(|t| {
            t.conversation != event.conversation
                || t.ts != *event.thread_ts.as_ref().unwrap_or(&event.ts)
        }) {
            return Err("Reply conflicts with request thread".into());
        }
        Ok(event)
    }

    pub(super) fn borrowed_thread(&self, thread: &Thread) -> bool {
        self.requests()
            .iter()
            .any(|r| r.adopted_ms.is_some() && r.thread.as_ref() == Some(thread))
    }

    pub(super) fn reconcile_requests(&mut self) -> Result<(), String> {
        self.finish_request_deletions()?;
        self.flush_event_deletions()?;
        if let (Some(requests), Some(drafts)) = (&mut self.requests, &self.drafts) {
            requests.reconcile(drafts.drafts())?;
        }
        let watches: Vec<_> = self
            .requests()
            .iter()
            .filter(|r| !r.settled)
            .filter_map(|r| {
                let thread = r.thread.clone()?;
                Some(Watch {
                    request_id: r.id.clone(),
                    after_us: r.reply_cutoff()?,
                    borrowed: self.borrowed_thread(&thread),
                    thread,
                })
            })
            .collect();
        if self.watches.as_ref() != Some(&watches)
            && !self.watches_pending
            && now_ms() >= self.watch_retry_ms
            && let Some(worker) = &self.worker
        {
            worker.submit(Work::Watches(watches.clone()))?;
            self.watches_pending = true;
        }
        if !self.settings.dry_run && !self.dispatch_blocked && now_ms() >= self.cleanup_retry_ms {
            let threads: Vec<_> = self
                .requests()
                .iter()
                .filter(|r| r.settled && r.close_requested && r.adopted_ms.is_none())
                .filter_map(|r| r.thread.clone())
                .chain(
                    self.requests
                        .as_ref()
                        .into_iter()
                        .flat_map(|s| s.deletions())
                        .filter_map(|d| d.thread.clone()),
                )
                .filter(|thread| !self.borrowed_thread(thread))
                .filter(|thread| {
                    !self
                        .requests()
                        .iter()
                        .any(|r| !r.settled && r.thread.as_ref() == Some(thread))
                })
                .collect();
            if !threads.is_empty()
                && let Some(worker) = &self.worker
            {
                for thread in threads {
                    worker.submit(Work::Remove(thread))?;
                }
                self.cleanup_retry_ms = now_ms() + 30_000;
            }
        }
        Ok(())
    }

    pub(super) fn schedule_draft(&mut self, id: &str) -> Result<(), String> {
        let draft = self.queue()?.get(id).ok_or("Unknown draft")?;
        let destination = draft.destination().map(str::to_owned);
        if draft.allows_auto()
            && let Some(destination) = destination
            && let ApprovalMode::Auto { veto_secs } = self.settings.approval(&destination)
        {
            self.queue()?
                .schedule(id, Some(now_ms().saturating_add(veto_secs as i64 * 1000)))?;
        }
        Ok(())
    }

    pub(super) fn auto_approve(&mut self, now: i64) -> Result<(), String> {
        if self.dispatch_blocked {
            return Ok(());
        }
        let due: Vec<_> = self
            .drafts()
            .iter()
            .filter(|d| {
                d.status == DraftStatus::Pending && d.auto_at_ms.is_some_and(|at| at <= now)
            })
            .map(|d| d.id.clone())
            .collect();
        for id in due {
            let destination = self
                .queue()?
                .get(&id)
                .and_then(Draft::destination)
                .unwrap_or_default()
                .to_owned();
            if matches!(self.settings.approval(&destination), ApprovalMode::Approve) {
                self.queue()?.schedule(&id, None)?;
                continue;
            }
            if let Err(error) = self.envoy_approve(&id, None, false, |_| false) {
                self.queue()?.schedule(&id, None)?;
                return Err(error);
            }
        }
        self.reconcile_requests()
    }

    pub(super) fn record_replies(&mut self, inbox: &Inbox) -> Result<(), String> {
        let mut keys = Vec::new();
        for reply in &inbox.pending_replies {
            if self.requests_mut()?.get(&reply.request_id).is_ok()
                && !self.cleanup.deleted_events.contains(&reply.event.id())
                && !inbox.is_deleted(&reply.event.id())
            {
                self.requests_mut()?
                    .reply(&reply.request_id, &reply.event)?;
            }
            keys.push(reply.key());
        }
        if !keys.is_empty() {
            self.worker()?.submit(Work::RecordedReplies(keys))?;
        }
        Ok(())
    }

    pub(super) fn request_event(&self, handle: &str) -> Option<(Action, GatewayAction)> {
        let turns: Vec<_> = self
            .requests()
            .iter()
            .filter(|r| !r.settled)
            .flat_map(|r| {
                r.turns
                    .iter()
                    .filter(|t| !t.forwarded && t.status == Status::Queued)
                    .map(move |t| (r, t))
            })
            .take(20)
            .collect();
        if !turns.is_empty() {
            let text = turns
                .iter()
                .map(|(r, t)| {
                    let note = r
                        .thread
                        .as_ref()
                        .and_then(|t| self.notes.as_ref()?.for_thread(&t.conversation, &t.ts))
                        .map(|n| format!(" — note: {}", n.text))
                        .unwrap_or_default();
                    let instruction = t
                        .karl_asked
                        .as_ref()
                        .map(|quote| {
                            format!(
                                " on {}'s instruction (unverified): {quote:?}",
                                crate::user::name()
                            )
                        })
                        .unwrap_or_default();
                    format!(
                        "Envoy event: request from {}{instruction}: {} — id {} turn {}{note}",
                        r.requester.as_ref().map_or("unknown", |r| r.label.as_str()),
                        t.ask.chars().take(120).collect::<String>(),
                        r.id,
                        t.id
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ");
            let text = self.handoff(safe_line(&text));
            let records = turns
                .iter()
                .map(|(r, t)| {
                    (
                        r.id.clone(),
                        t.id,
                        r.requester
                            .as_ref()
                            .map_or("unknown", |r| r.handle())
                            .into(),
                    )
                })
                .collect();
            return Some((
                Action::Send {
                    handle: handle.into(),
                    input: TerminalInput::line(text),
                },
                GatewayAction::Requests(records),
            ));
        }
        None
    }

    pub(super) fn reply_action(
        &self,
        request: &str,
        delivery: &str,
        target: &str,
    ) -> Option<(Action, GatewayAction)> {
        if self.settings.dry_run || self.dispatch_blocked {
            return None;
        }
        let store = self.requests.as_ref()?;
        let reply = if let Ok(record) = store.get(request) {
            record.bound_requester()?;
            let reply = record
                .deliveries
                .iter()
                .find(|d| d.id == delivery && !d.delivered)?;
            if reply
                .event
                .as_ref()
                .is_some_and(|event| !record.accepts_reply(event))
            {
                return None;
            }
            reply
        } else {
            let deletion = store.deletions().iter().find(|d| d.id == request)?;
            deletion.requester.as_ref()?;
            if deletion.delivery.delivered || deletion.delivery.id != delivery {
                return None;
            }
            &deletion.delivery
        };
        Some((
            Action::Send {
                handle: target.into(),
                input: TerminalInput::line(safe_line(&reply.text)),
            },
            GatewayAction::Reply {
                request: request.into(),
                delivery: delivery.into(),
                target: target.into(),
            },
        ))
    }

    pub(super) fn gateway_done(
        &mut self,
        result: &ActionResult,
        mut traffic: impl FnMut(AgentMessage),
    ) {
        let Some(action) = self.gateway_actions.remove(&result.id) else {
            return;
        };
        if result.outcome.is_err() {
            self.gateway_retry_ms = now_ms() + 30_000;
            return;
        }
        let recorded = match action {
            GatewayAction::Requests(turns) => {
                self.context.forwarded(turns.len(), now_ms());
                let mut result = Ok(());
                for (id, turn, from) in turns {
                    result = result.and_then(|()| {
                        if self.requests_mut()?.get(&id).is_err() {
                            return Ok(());
                        }
                        self.requests_mut()?.change(&id, |r| {
                            r.turns.iter_mut().find(|t| t.id == turn).unwrap().forwarded = true;
                            Ok(())
                        })
                    });
                    traffic(AgentMessage::new(
                        from,
                        self.handle.clone().unwrap_or_else(|| "envoy".into()),
                        MessageKind::Relay,
                    ));
                }
                result
            }
            GatewayAction::Reply {
                request,
                delivery,
                target,
            } => {
                let result = self.requests_mut().and_then(|s| {
                    if s.get(&request).is_err() {
                        return s.change_deletions(|records| {
                            if let Some(record) = records.iter_mut().find(|r| r.id == request)
                                && record.delivery.id == delivery
                            {
                                record.delivery.delivered = true;
                            }
                        });
                    }
                    s.change(&request, |r| {
                        let d = r
                            .deliveries
                            .iter_mut()
                            .find(|d| d.id == delivery)
                            .ok_or("Unknown delivery")?;
                        d.delivered = true;
                        d.inbox = false;
                        Ok(())
                    })
                });
                let envoy = self
                    .traffic_handle
                    .clone()
                    .unwrap_or_else(|| "envoy".into());
                traffic(AgentMessage::new("slack", &envoy, MessageKind::Relay));
                traffic(AgentMessage::new(envoy, target, MessageKind::Relay));
                result
            }
        };
        if let Err(e) = recorded {
            self.error = Some(e);
        }
    }
}

fn turn_id(args: &Value) -> Result<Option<u64>, String> {
    args.get("turn_id")
        .map(|v| {
            v.as_u64()
                .filter(|n| *n > 0)
                .ok_or("turn_id must be a positive integer".into())
        })
        .transpose()
}

impl StrongholdApp {
    pub(super) fn gateway_requester(
        &self,
        origin: crate::review::mark::Origin,
    ) -> Result<Requester, String> {
        let mut who = Requester::from_origin(origin)?;
        if let Some(snapshot) = &self.snapshot
            && let Some(node) = snapshot
                .nodes
                .iter()
                .find(|n| n.terminal_handle.as_deref() == Some(who.handle()))
        {
            who.label = node.label.clone();
            who.dispatch_id = node.dispatch_id.clone();
            who.run_id = node.run_id.clone();
            who.coordinator = node
                .run_id
                .as_deref()
                .and_then(|id| snapshot.run(id))
                .and_then(|r| r.coordinator.as_ref())
                .and_then(|id| snapshot.node(id))
                .and_then(|n| n.terminal_handle.clone());
            if let Some(coordinator) = who.coordinator.as_ref().and_then(|h| {
                snapshot
                    .nodes
                    .iter()
                    .find(|n| n.terminal_handle.as_ref() == Some(h))
            }) {
                who.coordinator_run_id = coordinator.run_id.clone();
                who.coordinator_dispatch_id = coordinator.dispatch_id.clone();
            }
        }
        if let Some(data) = &self.data {
            who.incarnation_id = data
                .terminal(who.handle())
                .and_then(|t| t.incarnation_id.clone());
            who.coordinator_incarnation_id = who
                .coordinator
                .as_deref()
                .and_then(|h| data.terminal(h))
                .and_then(|t| t.incarnation_id.clone());
        }
        Ok(who)
    }

    pub(super) fn forward_gateway(&mut self) {
        if now_ms() < self.envoy_core.gateway_retry_ms {
            return;
        }
        if let Some(handle) = self.envoy_core.handle.clone()
            && self.envoy_core.forwarding.is_none()
            && self.envoy_core.gateway_actions.is_empty()
            && self.envoy_core.resetting.is_none()
            && self.envoy_target_ready(&handle)
            && let Some((action, record)) = self.envoy_core.request_event(&handle)
            && !self.envoy_reset()
        {
            let id = self.actions.submit(action);
            self.envoy_core.gateway_actions.insert(id, record);
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if now_ms().saturating_sub(snapshot.built_at_ms) > 30_000
            || self.data.as_ref().is_none_or(|d| {
                d.failed_polls > 0
                    || d.fresh_at_ms
                        .is_none_or(|at| now_ms().saturating_sub(at) > 30_000)
            })
        {
            return;
        }
        let pending: Vec<_> = self
            .envoy_core
            .requests()
            .iter()
            .flat_map(|r| {
                r.deliveries
                    .iter()
                    .filter(|d| !d.delivered)
                    .map(move |d| (r.id.clone(), r.bound_requester().cloned(), d.clone()))
            })
            .chain(
                self.envoy_core
                    .requests
                    .as_ref()
                    .into_iter()
                    .flat_map(|s| s.deletions())
                    .filter(|d| !d.delivery.delivered)
                    .map(|d| (d.id.clone(), d.requester.clone(), d.delivery.clone())),
            )
            .collect();
        for (request, requester, delivery) in pending {
            let in_flight = self.envoy_core.gateway_actions.values().any(|action| {
                matches!(action, GatewayAction::Reply { request: r, delivery: d, .. }
                    if *r == request && *d == delivery.id)
            });
            if in_flight {
                continue;
            }
            let target = requester
                .as_ref()
                .and_then(|who| reply_target(snapshot, self.data.as_ref(), who));
            let dry_run = self.envoy_core.settings.dry_run;
            if dry_run || target.is_none() {
                if delivery.inbox && !dry_run {
                    continue;
                }
                let result = self.envoy_core.requests_mut().and_then(|s| {
                    if s.get(&request).is_err() {
                        return s.change_deletions(|records| {
                            if let Some(record) = records.iter_mut().find(|r| r.id == request) {
                                record.delivery.inbox = target.is_none();
                                record.delivery.dry_run = dry_run;
                                record.delivery.delivered = dry_run;
                            }
                        });
                    }
                    s.change(&request, |r| {
                        let d = r
                            .deliveries
                            .iter_mut()
                            .find(|d| d.id == delivery.id)
                            .unwrap();
                        d.inbox = target.is_none();
                        d.dry_run = dry_run;
                        d.delivered = dry_run;
                        Ok(())
                    })
                });
                if let Err(e) = result {
                    self.envoy_core.error = Some(e);
                }
                continue;
            }
            let target = target.unwrap();
            if !self.envoy_target_ready(&target)
                || self
                    .envoy_core
                    .gateway_actions
                    .values()
                    .any(|a| matches!(a,GatewayAction::Reply{target:t,..} if *t==target))
            {
                continue;
            }
            if let Some((action, record)) =
                self.envoy_core
                    .reply_action(&request, &delivery.id, &target)
            {
                let id = self.actions.submit(action);
                self.envoy_core.gateway_actions.insert(id, record);
            }
        }
    }
}

fn safe_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub(super) fn reply_target(
    snapshot: &crate::model::ClanSnapshot,
    data: Option<&crate::clan::ClanData>,
    who: &Requester,
) -> Option<String> {
    let live = |n: &crate::model::Node| {
        n.live
            && n.released_at_ms.is_none()
            && !matches!(
                n.state,
                crate::model::NodeState::Done | crate::model::NodeState::Failed
            )
    };
    if snapshot.nodes.iter().any(|n| {
        n.terminal_handle.as_deref() == Some(who.handle())
            && live(n)
            && n.dispatch_id == who.dispatch_id
            && n.run_id == who.run_id
            && who.incarnation_id.as_ref().is_none_or(|id| {
                data.and_then(|d| d.terminal(who.handle()))
                    .and_then(|t| t.incarnation_id.as_ref())
                    == Some(id)
            })
    }) {
        return Some(who.handle().into());
    }
    who.coordinator.clone().filter(|h| {
        snapshot.nodes.iter().any(|n| {
            n.terminal_handle.as_ref() == Some(h)
                && live(n)
                && n.run_id == who.coordinator_run_id
                && n.dispatch_id == who.coordinator_dispatch_id
                && who.coordinator_incarnation_id.as_ref().is_none_or(|id| {
                    data.and_then(|d| d.terminal(h))
                        .and_then(|t| t.incarnation_id.as_ref())
                        == Some(id)
                })
        })
    })
}

//! Request ownership, conversation sequencing and durable reply delivery.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{
    channel::InboundEvent,
    drafts::{Draft, DraftStatus},
    inbox,
};
use crate::review::mark::Origin;

pub const REQUESTS_FILE: &str = "envoy-requests.json";
const HOUR: i64 = 3_600_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requester {
    pub origin: Origin,
    pub label: String,
    pub dispatch_id: Option<String>,
    pub coordinator: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub coordinator_run_id: Option<String>,
    #[serde(default)]
    pub coordinator_dispatch_id: Option<String>,
    #[serde(default)]
    pub incarnation_id: Option<String>,
    #[serde(default)]
    pub coordinator_incarnation_id: Option<String>,
}

impl Requester {
    pub fn from_origin(origin: Origin) -> Result<Self, String> {
        let label = origin
            .terminal_handle
            .as_deref()
            .filter(|h| !h.trim().is_empty())
            .ok_or("Origin terminal is required")?
            .to_owned();
        Ok(Self {
            origin,
            label,
            dispatch_id: None,
            coordinator: None,
            run_id: None,
            coordinator_run_id: None,
            coordinator_dispatch_id: None,
            incarnation_id: None,
            coordinator_incarnation_id: None,
        })
    }
    pub fn handle(&self) -> &str {
        self.origin.terminal_handle.as_deref().unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub conversation: String,
    pub ts: String,
}

impl Thread {
    pub fn key(&self) -> String {
        format!("{}:{}", self.conversation, self.ts)
    }
    pub fn matches(&self, event: &InboundEvent) -> bool {
        self.conversation == event.conversation
            && event.thread_ts.as_deref() == Some(&self.ts)
            && event.ts != self.ts
    }
    pub fn from_link(link: &str) -> Result<Self, String> {
        let url = url::Url::parse(link).map_err(|_| "thread must be a Slack permalink")?;
        if url.scheme() != "https" || !url.host_str().is_some_and(|h| h.ends_with(".slack.com")) {
            return Err("thread must be a Slack permalink".into());
        }
        let parts: Vec<_> = url.path_segments().ok_or("Invalid thread link")?.collect();
        if parts.len() != 3 || parts[0] != "archives" {
            return Err("Invalid thread link".into());
        }
        super::slack::validate_conversation(parts[1])?;
        let raw = parts[2]
            .strip_prefix('p')
            .ok_or("Invalid thread timestamp")?;
        if raw.len() <= 6 || !raw.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Invalid thread timestamp".into());
        }
        let ts = url
            .query_pairs()
            .find(|(k, _)| k == "thread_ts")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_else(|| format!("{}.{}", &raw[..raw.len() - 6], &raw[raw.len() - 6..]));
        super::slack::validate_ts(&ts)?;
        Ok(Self {
            conversation: parts[1].into(),
            ts,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Queued,
    Drafted,
    Approved,
    Sent,
    Answered,
    Declined,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: u64,
    pub ask: String,
    pub why: String,
    pub urgency: String,
    pub expect_reply: bool,
    pub created_ms: i64,
    pub status: Status,
    pub draft_id: Option<String>,
    pub forwarded: bool,
    pub dry_run: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub text: String,
    pub event: Option<InboundEvent>,
    pub delivered: bool,
    pub dry_run: bool,
    pub inbox: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub to: String,
    pub requester: Option<Requester>,
    pub turns: Vec<Turn>,
    pub thread: Option<Thread>,
    pub status: Status,
    pub settled: bool,
    pub adopted_ms: Option<i64>,
    pub orphan_kept: bool,
    #[serde(default)]
    pub close_requested: bool,
    pub deliveries: Vec<Delivery>,
}

impl Request {
    pub fn owned_by(&self, requester: &Requester) -> bool {
        self.requester.as_ref().is_some_and(|r| {
            r.handle() == requester.handle()
                && r.dispatch_id == requester.dispatch_id
                && r.run_id == requester.run_id
                && r.incarnation_id == requester.incarnation_id
        })
    }
    pub fn next_turn(&self, id: Option<u64>) -> Result<&Turn, String> {
        if self.settled {
            return Err("Request is settled".into());
        }
        let turn = match id {
            Some(id) => self.turns.iter().find(|t| t.id == id),
            None => {
                let queued: Vec<_> = self
                    .turns
                    .iter()
                    .filter(|t| t.status == Status::Queued)
                    .collect();
                if queued.len() > 1 {
                    return Err("Multiple queued turns; supply turn_id".into());
                }
                queued.first().copied().or_else(|| self.turns.last())
            }
        }
        .ok_or("Unknown request turn")?;
        if self.turns.iter().any(|t| {
            t.id < turn.id
                && matches!(
                    t.status,
                    Status::Queued | Status::Drafted | Status::Approved | Status::Failed
                )
        }) {
            return Err("Earlier turn must finish before routing a follow-up".into());
        }
        if turn.id > 1
            && self.thread.is_none()
            && !self
                .turns
                .iter()
                .take((turn.id - 1) as usize)
                .all(|t| t.dry_run)
        {
            return Err("Follow-up needs the first send receipt and thread binding".into());
        }
        Ok(turn)
    }
}

#[derive(Debug, Deserialize)]
pub struct ReachOut {
    pub to: String,
    pub ask: String,
    pub why: String,
    pub thread: Option<String>,
    pub request_id: Option<String>,
    #[serde(default = "normal")]
    pub urgency: String,
    #[serde(default = "yes")]
    pub expect_reply: bool,
}
fn normal() -> String {
    "normal".into()
}
fn yes() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct Accepted {
    pub request_id: String,
    pub status: Status,
    pub turn_id: u64,
    pub merged: bool,
    pub follow_up: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Saved {
    next_id: u64,
    requests: Vec<Request>,
}

pub struct Requests {
    path: PathBuf,
    saved: Saved,
}

pub fn refuse_karl(to: &str, karl: &str) -> Result<(), String> {
    let to = to.trim();
    if to.eq_ignore_ascii_case("karl")
        || to.eq_ignore_ascii_case("me")
        || to.eq_ignore_ascii_case(karl)
        || to.eq_ignore_ascii_case(&format!("<@{karl}>"))
    {
        return Err("To reach Karl, use ask_karl".into());
    }
    Ok(())
}

impl Requests {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let saved = inbox::load(&path).map_err(|e| e.to_string())?;
        Ok(Self { path, saved })
    }
    pub fn all(&self) -> &[Request] {
        &self.saved.requests
    }
    pub fn get(&self, id: &str) -> Result<&Request, String> {
        self.all()
            .iter()
            .find(|r| r.id == id)
            .ok_or("Request unavailable".into())
    }
    pub fn change<T>(
        &mut self,
        id: &str,
        f: impl FnOnce(&mut Request) -> Result<T, String>,
    ) -> Result<T, String> {
        let old = self.saved.clone();
        let request = self
            .saved
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or("Request unavailable")?;
        match f(request).and_then(|value| self.save().map(|()| value)) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.saved = old;
                Err(error)
            }
        }
    }
    fn save(&self) -> Result<(), String> {
        inbox::save(&self.path, &self.saved).map_err(|e| e.to_string())
    }

    pub fn submit(
        &mut self,
        who: Requester,
        args: ReachOut,
        karl: &str,
        now: i64,
    ) -> Result<Accepted, String> {
        refuse_karl(&args.to, karl)?;
        for (value, max) in [(&args.to, 1000), (&args.ask, 35_000), (&args.why, 4000)] {
            if value.trim().is_empty() || value.len() > max {
                return Err("Request text is empty or too long".into());
            }
        }
        if !["normal", "soon", "blocking"].contains(&args.urgency.as_str()) {
            return Err("Invalid urgency".into());
        }
        let thread = args.thread.as_deref().map(Thread::from_link).transpose()?;
        let owned: Vec<_> = self
            .all()
            .iter()
            .filter(|r| r.owned_by(&who) && !r.settled)
            .collect();
        let target = if let Some(id) = &args.request_id {
            Some(
                owned
                    .iter()
                    .find(|r| &r.id == id)
                    .copied()
                    .ok_or("Request unavailable")?,
            )
        } else if let Some(thread) = &thread {
            let matches: Vec<_> = owned
                .iter()
                .filter(|r| r.thread.as_ref() == Some(thread))
                .copied()
                .collect();
            if matches.len() > 1 {
                return Err("Multiple requests in thread; supply request_id".into());
            }
            matches.first().copied()
        } else {
            None
        };
        if let Some(r) = target
            && thread.is_some()
            && thread != r.thread
        {
            return Err("Thread conflicts with request_id".into());
        }
        for r in &owned {
            if target.is_some_and(|t| t.id != r.id) {
                continue;
            }
            if r.to == args.to
                && (thread.is_none() || r.thread == thread)
                && let Some(t) = r.turns.iter().find(|t| t.ask == args.ask)
            {
                return Ok(Accepted {
                    request_id: r.id.clone(),
                    status: t.status,
                    turn_id: t.id,
                    merged: true,
                    follow_up: t.id > 1,
                });
            }
        }
        let target_id = target.map(|r| r.id.clone());
        if let Some(r) = target {
            let count = self
                .all()
                .iter()
                .filter(|other| {
                    other.owned_by(&who)
                        && (other.id == r.id || (r.thread.is_some() && other.thread == r.thread))
                })
                .flat_map(|r| r.turns.iter().skip(1))
                .filter(|t| now.saturating_sub(t.created_ms) < HOUR)
                .count();
            if count >= 30 {
                return Err("Follow-up limit: 30 per hour per conversation".into());
            }
        } else if self
            .all()
            .iter()
            .filter(|r| {
                r.requester
                    .as_ref()
                    .is_some_and(|r| r.handle() == who.handle())
            })
            .filter_map(|r| r.turns.first())
            .filter(|t| now.saturating_sub(t.created_ms) < HOUR)
            .count()
            >= 6
        {
            return Err("New request limit: 6 per hour per ant".into());
        }
        let old = self.saved.clone();
        let follow_up = target_id.is_some();
        let id = target_id.unwrap_or_else(|| {
            self.saved.next_id += 1;
            let id = format!("envoy-request-{}", self.saved.next_id);
            self.saved.requests.push(Request {
                id: id.clone(),
                to: args.to,
                requester: Some(who),
                turns: Vec::new(),
                thread,
                status: Status::Queued,
                settled: false,
                adopted_ms: None,
                orphan_kept: false,
                close_requested: false,
                deliveries: Vec::new(),
            });
            id
        });
        let request = self.saved.requests.iter_mut().find(|r| r.id == id).unwrap();
        let turn_id = request.turns.len() as u64 + 1;
        request.turns.push(Turn {
            id: turn_id,
            ask: args.ask,
            why: args.why,
            urgency: args.urgency,
            expect_reply: args.expect_reply,
            created_ms: now,
            status: Status::Queued,
            draft_id: None,
            forwarded: false,
            dry_run: false,
            error: None,
        });
        request.status = Status::Queued;
        if let Err(e) = self.save() {
            self.saved = old;
            return Err(e);
        }
        Ok(Accepted {
            request_id: id,
            status: Status::Queued,
            turn_id,
            merged: false,
            follow_up,
        })
    }

    pub fn reconcile(&mut self, drafts: &[Draft]) -> Result<(), String> {
        let old = self.saved.clone();
        for draft in drafts {
            let Some(link) = &draft.request else { continue };
            let Some(request) = self.saved.requests.iter_mut().find(|r| r.id == link.id) else {
                continue;
            };
            let Some(turn) = request.turns.iter_mut().find(|t| t.id == link.turn_id) else {
                continue;
            };
            turn.draft_id = Some(draft.id.clone());
            turn.dry_run = draft.status == DraftStatus::DryRunSent;
            turn.error = draft.error.clone();
            let status = match draft.status {
                DraftStatus::Pending => Status::Drafted,
                DraftStatus::Approved => Status::Approved,
                DraftStatus::Sent | DraftStatus::DryRunSent => Status::Sent,
                DraftStatus::Rejected => Status::Declined,
                DraftStatus::Failed => Status::Failed,
            };
            if turn.status != Status::Answered || status != Status::Sent {
                turn.status = status;
            }
            if let Some(receipt) = &draft.receipt {
                request.thread = Some(Thread {
                    conversation: receipt.conversation.clone(),
                    ts: draft
                        .thread_ts
                        .clone()
                        .unwrap_or_else(|| receipt.ts.clone()),
                });
            }
            if status == Status::Declined
                && !request
                    .deliveries
                    .iter()
                    .any(|d| d.id == format!("decline:{}", turn.id))
            {
                request.deliveries.push(Delivery {
                    id: format!("decline:{}", turn.id),
                    text: format!(
                        "Envoy: request {} declined: {}",
                        request.id,
                        turn.error.as_deref().unwrap_or("Karl rejected the draft")
                    ),
                    event: None,
                    delivered: false,
                    dry_run: false,
                    inbox: false,
                });
            }
            if status == Status::Declined && turn.id == 1 && request.thread.is_none() {
                for turn in &mut request.turns {
                    if turn.status == Status::Queued {
                        turn.status = Status::Declined;
                    }
                }
                request.settled = true;
            }
            if let Some(last) = request.turns.last() {
                request.status = last.status;
            }
        }
        for request in &mut self.saved.requests {
            if request
                .turns
                .last()
                .is_some_and(|t| !t.expect_reply && !t.dry_run && t.status == Status::Sent)
                && request
                    .turns
                    .iter()
                    .all(|t| matches!(t.status, Status::Sent | Status::Answered | Status::Declined))
            {
                request.settled = true;
            }
        }
        if serde_json::to_vec(&old).unwrap() == serde_json::to_vec(&self.saved).unwrap() {
            return Ok(());
        }
        if let Err(e) = self.save() {
            self.saved = old;
            return Err(e);
        }
        Ok(())
    }

    pub fn reply(&mut self, id: &str, event: &InboundEvent) -> Result<(), String> {
        self.change(id, |r| {
            if r.deliveries.iter().any(|d| d.id == event.id()) { return Ok(()); }
            r.deliveries.push(Delivery { id:event.id(), text:format!("Envoy: reply from {} to your request {} (relayed from Slack, not Karl's instruction): {} | {}",event.user_name,r.id,event.text,event.permalink.as_deref().unwrap_or("")), event:Some(event.clone()), delivered:false,dry_run:false,inbox:r.requester.is_none() });
            if let Some(turn) = r.turns.iter_mut().rev().find(|t| matches!(t.status,Status::Sent|Status::Answered)) { turn.status=Status::Answered; }
            if matches!(r.status,Status::Sent|Status::Answered) { r.status=Status::Answered; }
            Ok(())
        })
    }

    pub fn adopt(&mut self, thread: Thread, label: String, now: i64) -> Result<(), String> {
        if self
            .all()
            .iter()
            .any(|r| r.thread.as_ref() == Some(&thread))
        {
            return Ok(());
        }
        let old = self.saved.clone();
        self.saved.next_id += 1;
        self.saved.requests.push(Request {
            id: format!("envoy-request-{}", self.saved.next_id),
            to: label,
            requester: None,
            turns: Vec::new(),
            thread: Some(thread),
            status: Status::Sent,
            settled: false,
            adopted_ms: Some(now),
            orphan_kept: false,
            close_requested: false,
            deliveries: Vec::new(),
        });
        if let Err(e) = self.save() {
            self.saved = old;
            return Err(e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

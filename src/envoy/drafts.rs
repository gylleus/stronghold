//! Durable approvals. An approved send is never retried automatically.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::channel::{OutboundMessage, SendReceipt};
use super::{inbox, secrets};

pub const DRAFTS_FILE: &str = "envoy-drafts.json";
const AUTO_ATTRIBUTION: &str =
    "— {user}'s AI agent (the Envoy), authorized by {user}'s approval policy.";
const ATTRIBUTION: &str = "— {user}'s AI agent (the Envoy), approved by {user}.";

/// The footer of a draft the user approved.
pub fn attribution() -> String {
    crate::user::fill(ATTRIBUTION)
}

/// The footer of a draft sent under an automatic approval policy.
pub fn auto_attribution() -> String {
    crate::user::fill(AUTO_ATTRIBUTION)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    Pending,
    Approved,
    Sent,
    Failed,
    Rejected,
    DryRunSent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    Reply,
    TaskProposal,
    Outgoing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyTo {
    pub conversation: String,
    pub ts: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestLink {
    pub id: String,
    pub turn_id: u64,
    pub original_ask: String,
    pub requester: String,
    pub why: String,
    pub agent: bool,
    #[serde(default)]
    pub karl_asked: Option<String>,
}
impl RequestLink {
    pub fn verbatim(&self) -> String {
        self.with_header("")
    }

    fn with_header(&self, instruction: &str) -> String {
        format!(
            "Request from {}{instruction}:\n\n{}",
            self.requester
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>(),
            self.original_ask
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    pub kind: DraftKind,
    pub status: DraftStatus,
    pub in_reply_to: Option<ReplyTo>,
    pub thread_ts: Option<String>,
    pub text: String,
    pub rationale: String,
    pub repo: Option<String>,
    pub slack_link: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub error: Option<String>,
    #[serde(default)]
    pub conversation: Option<String>,
    #[serde(default)]
    pub request: Option<RequestLink>,
    #[serde(default)]
    pub receipt: Option<SendReceipt>,
    #[serde(default)]
    pub confirmed_send: bool,
    #[serde(default)]
    pub auto_at_ms: Option<i64>,
    #[serde(default)]
    pub automatic: bool,
    #[serde(default)]
    pub manual_approval: bool,
    #[serde(default)]
    pub approved_at: Option<i64>,
    #[serde(default)]
    pub approved_by: Option<String>,
}

impl Draft {
    /// Whether this draft went out (or would have, in dry-run) in `conversation`'s thread
    /// `root`: a reply in it, or the post that started it.
    pub fn posted_in(&self, conversation: &str, root: &str) -> bool {
        if !matches!(self.status, DraftStatus::Sent | DraftStatus::DryRunSent) {
            return false;
        }
        let same = |c: &str| c == conversation;
        let reply = self.in_reply_to.as_ref();
        (reply.is_some_and(|r| same(&r.conversation))
            || self.conversation.as_deref().is_some_and(same)
            || self.receipt.as_ref().is_some_and(|r| same(&r.conversation)))
            && (self.thread_ts.as_deref() == Some(root)
                || reply.is_some_and(|r| r.ts == root)
                || self.receipt.as_ref().is_some_and(|r| r.ts == root))
    }

    pub fn reply(
        in_reply_to: ReplyTo,
        thread_ts: Option<String>,
        text: String,
        rationale: String,
        now: i64,
    ) -> Self {
        Self {
            id: String::new(),
            kind: DraftKind::Reply,
            status: DraftStatus::Pending,
            in_reply_to: Some(in_reply_to),
            thread_ts,
            text,
            rationale,
            repo: None,
            slack_link: None,
            created_ms: now,
            updated_ms: now,
            error: None,
            conversation: None,
            request: None,
            receipt: None,
            confirmed_send: false,
            auto_at_ms: None,
            automatic: false,
            manual_approval: false,
            approved_at: None,
            approved_by: None,
        }
    }

    pub fn proposal(
        summary: String,
        repo: Option<String>,
        why: String,
        slack_link: String,
        now: i64,
    ) -> Self {
        Self {
            id: String::new(),
            kind: DraftKind::TaskProposal,
            status: DraftStatus::Pending,
            in_reply_to: None,
            thread_ts: None,
            text: summary,
            rationale: why,
            repo,
            slack_link: Some(slack_link),
            created_ms: now,
            updated_ms: now,
            error: None,
            conversation: None,
            request: None,
            receipt: None,
            confirmed_send: false,
            auto_at_ms: None,
            automatic: false,
            manual_approval: false,
            approved_at: None,
            approved_by: None,
        }
    }

    pub fn destination(&self) -> Option<&str> {
        self.conversation
            .as_deref()
            .or_else(|| self.in_reply_to.as_ref().map(|r| r.conversation.as_str()))
    }

    pub fn outgoing(
        conversation: String,
        thread_ts: Option<String>,
        text: String,
        rationale: String,
        link: RequestLink,
        now: i64,
    ) -> Self {
        let mut draft = Self::proposal(text, None, rationale, String::new(), now);
        draft.kind = DraftKind::Outgoing;
        draft.conversation = Some(conversation);
        draft.thread_ts = thread_ts;
        draft.slack_link = None;
        draft.request = Some(link);
        draft
    }

    pub fn allows_auto(&self) -> bool {
        !self.manual_approval
            && self.kind != DraftKind::TaskProposal
            && self.request.as_ref().is_none_or(|r| {
                r.karl_asked.is_none()
                    && r.agent
                    && (self.text == r.original_ask || self.text == r.verbatim())
            })
    }

    pub fn outbound(&self) -> Result<OutboundMessage, String> {
        self.validate()?;
        let conversation = self.destination().ok_or("Draft is a task proposal")?;
        let mut text = self.text.clone();
        let footer = if self.automatic {
            auto_attribution()
        } else {
            attribution()
        };
        let original_payload = self
            .request
            .as_ref()
            .is_some_and(|r| self.text == r.verbatim() || self.text == r.original_ask);
        if let Some(link) = &self.request
            && link.agent
            && link.karl_asked.is_some()
            && self.approved_at.is_some()
            && let Some(approved_by) = &self.approved_by
        {
            text = link.with_header(&format!(" (on {approved_by}'s instruction)"));
        }
        // Source ask bytes are opaque; only normalize a composed draft's footer.
        if !original_payload {
            loop {
                let trimmed = text.trim_end();
                if let Some(body) = [attribution(), auto_attribution()]
                    .iter()
                    .find_map(|line| trimmed.strip_suffix(line))
                {
                    text = body.trim_end_matches('\n').to_owned();
                } else {
                    break;
                }
            }
        }
        text.push_str("\n\n");
        text.push_str(&footer);
        Ok(OutboundMessage {
            conversation: conversation.into(),
            thread_ts: self.thread_ts.clone(),
            text,
        })
    }

    pub fn king_request(&self) -> String {
        format!(
            "{user} approved this task proposed by the Envoy:\n{}\nRepository: {}\nWhy: {}\nSlack: {}",
            self.text,
            self.repo.as_deref().unwrap_or("unspecified"),
            self.rationale,
            self.slack_link.as_deref().unwrap_or(""),
            user = crate::user::name()
        )
    }

    fn validate(&self) -> Result<(), String> {
        if self.automatic && !self.allows_auto() {
            return Err("This draft requires manual approval".into());
        }
        if let Some(link) = &self.request
            && link.agent
            && self.text != link.verbatim()
        {
            return Err("Agent requests must preserve the original ask verbatim".into());
        }
        if let Some(conversation) = &self.conversation {
            super::slack::validate_conversation(conversation)?;
        }
        if self.text.trim().is_empty() {
            return Err("Draft text is empty".into());
        }
        if self.text.len() > 40_000 {
            return Err("Draft text exceeds 40000 bytes".into());
        }
        for s in [
            Some(self.text.as_str()),
            Some(self.rationale.as_str()),
            self.repo.as_deref(),
            self.slack_link.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            secrets::scan(s)?;
        }
        if let Some(target) = &self.in_reply_to {
            super::slack::validate_conversation(&target.conversation)?;
            super::slack::validate_ts(&target.ts)?;
        }
        if let Some(ts) = &self.thread_ts {
            super::slack::validate_ts(ts)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    next_id: u64,
    drafts: Vec<Draft>,
}

pub struct DraftQueue {
    path: PathBuf,
    saved: Saved,
    restarted_automatic: usize,
}

impl DraftQueue {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let mut wire: serde_json::Value = inbox::load(&path).map_err(|e| e.to_string())?;
        let mut upgraded = false;
        if let Some(drafts) = wire
            .get_mut("drafts")
            .and_then(serde_json::Value::as_array_mut)
        {
            for draft in drafts {
                if draft.get("confirmed_send").is_none()
                    && draft["status"] == "sent"
                    && draft["receipt"]["ts"]
                        .as_str()
                        .is_some_and(|ts| super::slack::validate_ts(ts).is_ok())
                {
                    draft["confirmed_send"] = true.into();
                    upgraded = true;
                }
            }
        }
        let saved = if wire.is_null() {
            Saved::default()
        } else {
            serde_json::from_value(wire).map_err(|e| e.to_string())?
        };
        let mut queue = Self {
            path,
            saved,
            restarted_automatic: 0,
        };
        let mut interrupted = upgraded;
        for draft in &mut queue.saved.drafts {
            if draft.status == DraftStatus::Pending
                && (draft.automatic || draft.auto_at_ms.is_some())
            {
                draft.auto_at_ms = None;
                draft.automatic = false;
                queue.restarted_automatic += 1;
                interrupted = true;
            }
        }
        for draft in &mut queue.saved.drafts {
            if draft.status == DraftStatus::Approved {
                draft.status = DraftStatus::Failed;
                draft.error = Some("Outcome unknown: app restarted mid-send; check delivery before creating another draft".into());
                interrupted = true;
            }
        }
        if interrupted {
            queue.save()?;
        }
        Ok(queue)
    }

    pub fn drafts(&self) -> &[Draft] {
        &self.saved.drafts
    }
    pub fn take_restarted_automatic(&mut self) -> usize {
        std::mem::take(&mut self.restarted_automatic)
    }

    pub fn get(&self, id: &str) -> Option<&Draft> {
        self.saved.drafts.iter().find(|d| d.id == id)
    }
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let old = self.saved.clone();
        self.saved.drafts.retain(|d| d.id != id);
        self.commit(old)
    }

    pub fn pending_count(&self) -> usize {
        self.saved
            .drafts
            .iter()
            .filter(|d| d.status == DraftStatus::Pending)
            .count()
    }

    pub fn create(&mut self, mut draft: Draft) -> Result<Draft, String> {
        if let Some(link) = &draft.request
            && let Some(existing) = self.saved.drafts.iter().find(|d| {
                d.request
                    .as_ref()
                    .is_some_and(|r| r.id == link.id && r.turn_id == link.turn_id)
            })
        {
            return Ok(existing.clone());
        }
        draft.validate()?;
        let old = self.saved.clone();
        self.saved.next_id += 1;
        draft.id = format!("envoy-draft-{}", self.saved.next_id);
        self.saved.drafts.push(draft.clone());
        self.commit(old)?;
        Ok(draft)
    }

    pub fn edit(&mut self, id: &str, text: &str, now: i64) -> Result<(), String> {
        let old = self.saved.clone();
        let d = self.pending_mut(id)?;
        let mut edited = d.clone();
        edited.text = text.into();
        edited.validate()?;
        *d = edited;
        d.updated_ms = now;
        d.auto_at_ms = None;
        d.automatic = false;
        self.commit(old)
    }

    pub fn reject(&mut self, id: &str, now: i64) -> Result<(), String> {
        let old = self.saved.clone();
        let d = self.pending_mut(id)?;
        d.status = DraftStatus::Rejected;
        d.updated_ms = now;
        self.commit(old)
    }

    pub fn begin_approval(
        &mut self,
        id: &str,
        edited_text: Option<&str>,
        dry_run: bool,
        now: i64,
    ) -> Result<Draft, String> {
        let old = self.saved.clone();
        let d = self.pending_mut(id)?;
        let mut approved = d.clone();
        if let Some(text) = edited_text {
            approved.text = text.into();
        }
        approved.validate()?;
        approved.status = if dry_run {
            DraftStatus::DryRunSent
        } else {
            DraftStatus::Approved
        };
        approved.updated_ms = now;
        if !approved.automatic {
            approved.approved_at = Some(now);
            approved.approved_by = Some(crate::user::name());
        }
        *d = approved.clone();
        self.commit(old)?;
        Ok(approved)
    }

    pub fn finish(&mut self, id: &str, result: Result<(), String>, now: i64) -> Result<(), String> {
        let old = self.saved.clone();
        let d = self
            .saved
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or("Unknown draft")?;
        if d.status != DraftStatus::Approved {
            return Err("Draft is not awaiting delivery".into());
        }
        d.status = if result.is_ok() {
            DraftStatus::Sent
        } else {
            DraftStatus::Failed
        };
        d.error = result.err();
        d.updated_ms = now;
        self.commit(old)
    }

    pub fn finish_receipt(
        &mut self,
        id: &str,
        result: Result<SendReceipt, String>,
        now: i64,
    ) -> Result<(), String> {
        let old = self.saved.clone();
        let d = self
            .saved
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or("Unknown draft")?;
        if d.status != DraftStatus::Approved {
            return Err("Draft is not awaiting delivery".into());
        }
        match result {
            Ok(receipt) => {
                d.confirmed_send = true;
                d.receipt = Some(receipt);
                d.status = DraftStatus::Sent;
            }
            Err(e) => {
                d.error = Some(e);
                d.status = DraftStatus::Failed;
            }
        }
        d.updated_ms = now;
        self.commit(old)
    }

    pub fn schedule(&mut self, id: &str, at: Option<i64>) -> Result<(), String> {
        let old = self.saved.clone();
        let d = self.pending_mut(id)?;
        if at.is_some() && !d.allows_auto() {
            return Err("This draft requires manual approval".into());
        }
        d.auto_at_ms = at;
        d.automatic = at.is_some();
        self.commit(old)
    }

    pub fn decline(&mut self, id: &str, reason: &str, now: i64) -> Result<(), String> {
        let old = self.saved.clone();
        let d = self
            .saved
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or("Unknown draft")?;
        if !matches!(d.status, DraftStatus::Pending | DraftStatus::Failed) {
            return Err("Only unsent drafts can be declined".into());
        }
        d.status = DraftStatus::Rejected;
        d.error = Some(reason.into());
        d.updated_ms = now;
        self.commit(old)
    }
    pub fn resolve_failed(
        &mut self,
        id: &str,
        receipt: SendReceipt,
        now: i64,
    ) -> Result<(), String> {
        super::slack::validate_conversation(&receipt.conversation)?;
        super::slack::validate_ts(&receipt.ts)?;
        let old = self.saved.clone();
        let d = self
            .saved
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or("Unknown draft")?;
        if d.status != DraftStatus::Failed || d.destination() != Some(&receipt.conversation) {
            return Err("Receipt does not resolve failed draft".into());
        }
        d.status = DraftStatus::Sent;
        d.receipt = Some(receipt);
        d.error = None;
        d.updated_ms = now;
        self.commit(old)
    }

    fn pending_mut(&mut self, id: &str) -> Result<&mut Draft, String> {
        let d = self
            .saved
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or("Unknown draft")?;
        if d.status != DraftStatus::Pending {
            return Err("Only pending drafts can be changed or approved".into());
        }
        Ok(d)
    }
    fn save(&self) -> Result<(), String> {
        inbox::save(&self.path, &self.saved).map_err(|e| e.to_string())
    }
    fn commit(&mut self, old: Saved) -> Result<(), String> {
        if let Err(e) = self.save() {
            self.saved = old;
            return Err(e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn instruction_header_and_approval_trail_require_a_durable_manual_click() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DRAFTS_FILE);
        let mut queue = DraftQueue::load(&path).unwrap();
        let ask = format!(
            "sha abc012def\n```rust\nlet n = 42;\n```\n\n{}\n  ",
            attribution()
        );
        let link = RequestLink {
            id: "request".into(),
            turn_id: 1,
            original_ask: ask.clone(),
            requester: "Build dwarf".into(),
            why: "Review".into(),
            agent: true,
            karl_asked: Some("Send the exact details".into()),
        };
        let draft = Draft::outgoing(
            "C123".into(),
            None,
            link.verbatim(),
            "Review".into(),
            link,
            1,
        );
        let draft = queue.create(draft).unwrap();
        assert!(!draft.allows_auto());
        assert!(queue.schedule(&draft.id, Some(2)).is_err());
        assert_eq!(
            draft.outbound().unwrap().text,
            format!("Request from Build dwarf:\n\n{ask}\n\n{}", attribution())
        );
        let mut forged = draft.clone();
        forged.automatic = true;
        assert_eq!(
            forged.outbound().unwrap_err(),
            "This draft requires manual approval"
        );
        queue.edit(&draft.id, &draft.text, 2).unwrap();
        let approved = queue.begin_approval(&draft.id, None, false, 1234).unwrap();
        assert_eq!(approved.approved_at, Some(1234));
        assert_eq!(approved.approved_by, Some(crate::user::name()));
        let outbound = approved.outbound().unwrap();
        assert_eq!(
            outbound.text,
            format!(
                "Request from Build dwarf (on {}'s instruction):\n\n{ask}\n\n{}",
                crate::user::name(),
                attribution()
            )
        );
        let loaded = DraftQueue::load(&path).unwrap();
        let persisted = loaded.get(&draft.id).unwrap();
        assert_eq!(persisted.approved_at, Some(1234));
        assert_eq!(persisted.approved_by, approved.approved_by);

        let mut person = draft.clone();
        person.request.as_mut().unwrap().agent = false;
        person.text = "A personal message".into();
        person.approved_at = Some(1234);
        person.approved_by = approved.approved_by.clone();
        assert_eq!(
            person.outbound().unwrap().text,
            format!("A personal message\n\n{}", attribution())
        );
        person.request.as_mut().unwrap().karl_asked = None;
        person.text = person.request.as_ref().unwrap().original_ask.clone();
        assert!(!person.allows_auto());

        let mut legacy = serde_json::to_value(draft).unwrap();
        legacy.as_object_mut().unwrap().remove("approved_at");
        legacy.as_object_mut().unwrap().remove("approved_by");
        legacy["request"]
            .as_object_mut()
            .unwrap()
            .remove("karl_asked");
        let legacy: Draft = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.approved_at, None);
        assert_eq!(legacy.approved_by, None);
        assert_eq!(legacy.request.unwrap().karl_asked, None);
    }

    fn reply() -> Draft {
        Draft::reply(
            ReplyTo {
                conversation: "C123".into(),
                ts: "1.000001".into(),
            },
            Some("1.000001".into()),
            "Ready".into(),
            "Requested status".into(),
            1,
        )
    }
    #[test]
    fn manual_decisions_stay_manual_after_edit_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DRAFTS_FILE);
        let mut queue = DraftQueue::load(&path).unwrap();
        let mut decision = reply();
        decision.manual_approval = true;
        let draft = queue.create(decision).unwrap();
        assert!(queue.schedule(&draft.id, Some(2)).is_err());
        queue
            .edit(&draft.id, "Use settings for the alert.", 2)
            .unwrap();
        let mut queue = DraftQueue::load(&path).unwrap();
        assert!(queue.get(&draft.id).unwrap().manual_approval);
        assert!(queue.schedule(&draft.id, Some(3)).is_err());
        let approved = queue.begin_approval(&draft.id, None, false, 3).unwrap();
        assert_eq!(approved.status, DraftStatus::Approved);
        assert!(!approved.automatic);
        assert!(approved.outbound().unwrap().text.ends_with(&attribution()));

        let mut legacy = serde_json::to_value(reply()).unwrap();
        legacy.as_object_mut().unwrap().remove("manual_approval");
        let legacy: Draft = serde_json::from_value(legacy).unwrap();
        let legacy = queue.create(legacy).unwrap();
        queue.schedule(&legacy.id, Some(4)).unwrap();
    }

    #[test]
    fn posted_in_knows_sent_replies_by_thread_root() {
        let mut draft = reply();
        assert!(
            !draft.posted_in("C123", "1.000001"),
            "pending drafts posted nothing"
        );
        draft.status = DraftStatus::Sent;
        assert!(draft.posted_in("C123", "1.000001"));
        assert!(!draft.posted_in("C999", "1.000001"));
        assert!(!draft.posted_in("C123", "2.000001"));
        draft.status = DraftStatus::DryRunSent;
        assert!(draft.posted_in("C123", "1.000001"), "dry-run sends count");
        let mut started = reply();
        started.status = DraftStatus::Sent;
        started.thread_ts = None;
        started.receipt = Some(SendReceipt {
            conversation: "C123".into(),
            ts: "5.000001".into(),
        });
        assert!(
            started.posted_in("C123", "5.000001"),
            "a root post owns its thread"
        );
        assert!(
            started.posted_in("C123", "1.000001"),
            "so does a top-level reply"
        );
    }
    #[test]
    fn attribution_inside_original_ask_remains_byte_identical() {
        for automatic in [false, true] {
            for footer in [attribution(), auto_attribution()] {
                let ask = format!("Quote these literal bytes\n\n{footer}\n\n{footer}\n  ");
                let link = RequestLink {
                    id: "r".into(),
                    turn_id: 1,
                    original_ask: ask,
                    requester: "dwarf".into(),
                    why: "test".into(),
                    agent: true,
                    karl_asked: None,
                };
                let original = link.verbatim();
                let mut draft = Draft::outgoing(
                    "C123".into(),
                    None,
                    original.clone(),
                    "test".into(),
                    link,
                    0,
                );
                draft.automatic = automatic;
                let footer = if automatic {
                    auto_attribution()
                } else {
                    attribution()
                };
                assert_eq!(
                    draft.outbound().unwrap().text,
                    format!("{original}\n\n{footer}")
                );
            }
        }
    }

    #[test]
    fn outbound_has_one_current_attribution_footer() {
        for automatic in [false, true] {
            for footer in [attribution(), auto_attribution()] {
                let mut draft = reply();
                draft.text = format!("Fixture body\n\n{footer}\n\n{footer}\n");
                draft.automatic = automatic;
                let expected = if automatic {
                    auto_attribution()
                } else {
                    attribution()
                };
                assert_eq!(
                    draft.outbound().unwrap().text,
                    format!("Fixture body\n\n{expected}")
                );
            }
        }
    }

    #[test]
    fn dry_run_scan_edit_reject_and_one_shot_approval() {
        let dir = tempfile::tempdir().unwrap();
        let mut q = DraftQueue::load(dir.path().join(DRAFTS_FILE)).unwrap();
        let d = q.create(reply()).unwrap();
        assert!(
            q.begin_approval(&d.id, Some("ghp_123456789012345678901234567890"), true, 2)
                .is_err()
        );
        assert_eq!(q.get(&d.id).unwrap().text, "Ready");
        q.edit(&d.id, "Edited", 2).unwrap();
        let approved = q.begin_approval(&d.id, None, true, 3).unwrap();
        assert_eq!(approved.status, DraftStatus::DryRunSent);
        assert!(q.begin_approval(&d.id, None, false, 4).is_err());
        let d = q.create(reply()).unwrap();
        q.reject(&d.id, 4).unwrap();
        assert!(q.begin_approval(&d.id, None, false, 5).is_err());
        let mut bad = reply();
        bad.text = "sk-ant-api03-12345678901234567890".into();
        assert!(q.create(bad).is_err());
    }
    #[test]
    fn sends_attributed_text_and_recovers_ambiguous_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DRAFTS_FILE);
        let mut q = DraftQueue::load(&path).unwrap();
        let d = q.create(reply()).unwrap();
        let approved = q.begin_approval(&d.id, Some("New"), false, 2).unwrap();
        let outbound = approved.outbound().unwrap();
        assert!(outbound.text.ends_with(&attribution()));
        assert_eq!(outbound.thread_ts.as_deref(), Some("1.000001"));
        q.finish(&d.id, Ok(()), 2).unwrap();
        assert_eq!(q.get(&d.id).unwrap().status, DraftStatus::Sent);
        let d = q.create(reply()).unwrap();
        q.begin_approval(&d.id, None, false, 3).unwrap();
        q.finish(&d.id, Err("delivery failed".into()), 3).unwrap();
        assert_eq!(q.get(&d.id).unwrap().status, DraftStatus::Failed);
        let d = q.create(reply()).unwrap();
        q.begin_approval(&d.id, None, false, 4).unwrap();
        let loaded = DraftQueue::load(&path).unwrap();
        let recovered = loaded.get(&d.id).unwrap();
        assert_eq!(recovered.status, DraftStatus::Failed);
        assert!(
            recovered
                .error
                .as_ref()
                .unwrap()
                .contains("Outcome unknown")
        );
    }
}

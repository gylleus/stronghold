//! Durable approvals. An approved send is never retried automatically.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::channel::OutboundMessage;
use super::slack::SlackSender;
use super::{inbox, secrets};

pub const DRAFTS_FILE: &str = "envoy-drafts.json";
pub const ATTRIBUTION: &str = "— Karl's AI agent (the Envoy), approved by Karl.";

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyTo {
    pub conversation: String,
    pub ts: String,
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
}

impl Draft {
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
        }
    }

    pub fn outbound(&self) -> Result<OutboundMessage, String> {
        let target = self
            .in_reply_to
            .as_ref()
            .ok_or("Draft is a task proposal")?;
        let mut text = self.text.trim_end().to_owned();
        if !text.ends_with(ATTRIBUTION) {
            text.push_str("\n\n");
            text.push_str(ATTRIBUTION);
        }
        Ok(OutboundMessage {
            conversation: target.conversation.clone(),
            thread_ts: self.thread_ts.clone(),
            text,
        })
    }

    pub fn queen_request(&self) -> String {
        format!(
            "Karl approved this task proposed by the Envoy:\n{}\nRepository: {}\nWhy: {}\nSlack: {}",
            self.text,
            self.repo.as_deref().unwrap_or("unspecified"),
            self.rationale,
            self.slack_link.as_deref().unwrap_or("")
        )
    }

    fn validate(&self) -> Result<(), String> {
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
}

impl DraftQueue {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let saved = inbox::load(&path).map_err(|e| e.to_string())?;
        let mut queue = Self { path, saved };
        let mut interrupted = false;
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
    pub fn get(&self, id: &str) -> Option<&Draft> {
        self.saved.drafts.iter().find(|d| d.id == id)
    }
    pub fn pending_count(&self) -> usize {
        self.saved
            .drafts
            .iter()
            .filter(|d| d.status == DraftStatus::Pending)
            .count()
    }

    pub fn create(&mut self, mut draft: Draft) -> Result<Draft, String> {
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

    pub fn approve(
        &mut self,
        id: &str,
        edited_text: Option<&str>,
        dry_run: bool,
        sender: &mut dyn SlackSender,
        now: i64,
    ) -> Result<Draft, String> {
        if self
            .get(id)
            .is_some_and(|d| d.kind == DraftKind::TaskProposal)
        {
            return Err("Task proposals must be handed to the Queen by the app".into());
        }
        let draft = self.begin_approval(id, edited_text, dry_run, now)?;
        if draft.status == DraftStatus::Approved {
            let result = sender.send(&draft.outbound()?);
            self.finish(id, result, now)?;
        }
        Ok(self.get(id).expect("approved draft").clone())
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
    #[derive(Default)]
    struct Sender {
        calls: Vec<OutboundMessage>,
        fail: bool,
    }
    impl SlackSender for Sender {
        fn send(&mut self, m: &OutboundMessage) -> Result<(), String> {
            self.calls.push(m.clone());
            if self.fail {
                Err("delivery failed".into())
            } else {
                Ok(())
            }
        }
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
    fn dry_run_scan_edit_reject_and_one_shot_approval() {
        let dir = tempfile::tempdir().unwrap();
        let mut q = DraftQueue::load(dir.path().join(DRAFTS_FILE)).unwrap();
        let mut sender = Sender::default();
        let d = q.create(reply()).unwrap();
        assert!(
            q.approve(
                &d.id,
                Some("ghp_123456789012345678901234567890"),
                true,
                &mut sender,
                2
            )
            .is_err()
        );
        assert_eq!(q.get(&d.id).unwrap().text, "Ready");
        q.edit(&d.id, "Edited", 2).unwrap();
        let approved = q.approve(&d.id, None, true, &mut sender, 3).unwrap();
        assert_eq!(approved.status, DraftStatus::DryRunSent);
        assert!(sender.calls.is_empty());
        assert!(q.approve(&d.id, None, false, &mut sender, 4).is_err());
        let d = q.create(reply()).unwrap();
        q.reject(&d.id, 4).unwrap();
        assert!(q.approve(&d.id, None, false, &mut sender, 5).is_err());
        let mut bad = reply();
        bad.text = "sk-ant-api03-12345678901234567890".into();
        assert!(q.create(bad).is_err());
    }
    #[test]
    fn sends_attributed_text_and_recovers_ambiguous_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DRAFTS_FILE);
        let mut q = DraftQueue::load(&path).unwrap();
        let mut sender = Sender::default();
        let d = q.create(reply()).unwrap();
        assert_eq!(
            q.approve(&d.id, Some("New"), false, &mut sender, 2)
                .unwrap()
                .status,
            DraftStatus::Sent
        );
        assert!(sender.calls[0].text.ends_with(ATTRIBUTION));
        assert_eq!(sender.calls[0].thread_ts.as_deref(), Some("1.000001"));
        let d = q.create(reply()).unwrap();
        sender.fail = true;
        assert_eq!(
            q.approve(&d.id, None, false, &mut sender, 3)
                .unwrap()
                .status,
            DraftStatus::Failed
        );
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

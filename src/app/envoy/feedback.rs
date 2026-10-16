//! Pending feedback survives restarts in the draft store. Terminal notifications are hints:
//! retry at idle until a revision consumes the feedback, without replaying a Slack send.
use super::*;

impl AppEnvoy {
    fn due_feedback(&self, now: i64) -> Option<&Draft> {
        self.drafts().iter().find(|d| {
            d.status == DraftStatus::Pending
                && d.feedback.is_some()
                && self
                    .feedback_retry
                    .get(&(d.id.clone(), d.revision))
                    .is_none_or(|at| now >= *at)
        })
    }

    pub(super) fn feedback_due(&self, now: i64) -> bool {
        self.due_feedback(now).is_some()
    }

    pub(super) fn next_feedback(&self, now: i64) -> Option<(Action, String, u64)> {
        if self.in_flight().is_some() || self.resetting.is_some() || now < self.forward_after_ms {
            return None;
        }
        let d = self.due_feedback(now)?;
        // JSON keeps multiline instructions and terminal control characters out of the
        // composer protocol. The full private draft is available through envoy_drafts.
        let text = format!(
            "Envoy draft feedback from {user}: {payload}. Read envoy_drafts, follow this instruction, then call envoy_revise with id and current revision, revised text and rationale. Keep the same draft; do not send it. If a decision is missing, set question for an inline answer. This notification may repeat; revise only if feedback is still pending.",
            user = crate::user::name(),
            payload = json!({"id":d.id,"revision":d.revision,"feedback":d.feedback,"question":d.question}),
        );
        Some((
            Action::Send {
                handle: self.handle.clone()?,
                input: TerminalInput::line(self.handoff(text)),
            },
            d.id.clone(),
            d.revision,
        ))
    }

    pub(super) fn feedback_done(&mut self, result: &ActionResult, now: i64) {
        if self
            .feedback_forwarding
            .as_ref()
            .is_none_or(|(id, _, _)| *id != result.id)
        {
            return;
        }
        let (_, draft, revision) = self
            .feedback_forwarding
            .take()
            .expect("matching feedback action");
        self.feedback_retry.insert((draft, revision), now + 60_000);
        if result.outcome.is_ok() {
            self.context.forwarded(1, now);
        }
    }
}

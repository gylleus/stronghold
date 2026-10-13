//! When the Envoy's context is reset. The app keeps the inbox, drafts, requests and thread
//! notes, so the conversation history is only a cost: a batch after a quiet period starts
//! from a cleared context, and a context that grew past a size guard is cleared at the next
//! idle point even mid-burst.

use super::settings::EnvoySettings;

/// How long after a failed `/clear` the next batch goes into the old context instead.
pub const RESET_BACKOFF_MS: i64 = 10 * 60_000;

/// The Claude Code command that clears the conversation and keeps the system prompt.
pub const CLEAR_COMMAND: &str = "/clear";

/// What the session has taken in since its last fresh start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextBudget {
    /// Events and request turns typed since the last reset (or the launch).
    pub events_since_reset: u64,
    /// When the last batch was typed successfully; `0` before the first.
    pub last_forward_ms: i64,
    /// The context was cleared and has had no batch yet: the next one carries the handoff.
    pub fresh: bool,
    /// Resets are skipped until then, after a failed `/clear`.
    pub backoff_until_ms: i64,
    /// How many times the context was cleared.
    pub resets: u64,
}

/// Why a reset is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetReason {
    Quiet,
    Events,
    Tokens,
}

impl ResetReason {
    pub fn label(self) -> &'static str {
        match self {
            ResetReason::Quiet => "quiet period over",
            ResetReason::Events => "event guard",
            ResetReason::Tokens => "token guard",
        }
    }
}

impl ContextBudget {
    /// Whether the context should be cleared before the next batch. `context_tokens` is the
    /// session's context size as of its last turn, when known.
    pub fn reset_due(
        &self,
        now: i64,
        settings: &EnvoySettings,
        context_tokens: Option<u64>,
    ) -> Option<ResetReason> {
        if self.events_since_reset == 0 || now < self.backoff_until_ms {
            return None;
        }
        let quiet_ms =
            i64::try_from(settings.fresh_after_secs.saturating_mul(1_000)).unwrap_or(i64::MAX);
        if now.saturating_sub(self.last_forward_ms) >= quiet_ms {
            return Some(ResetReason::Quiet);
        }
        if settings.reset_after_events > 0 && self.events_since_reset >= settings.reset_after_events
        {
            return Some(ResetReason::Events);
        }
        if settings.reset_after_tokens > 0
            && context_tokens.is_some_and(|tokens| tokens >= settings.reset_after_tokens)
        {
            return Some(ResetReason::Tokens);
        }
        None
    }

    /// A batch of `count` events or turns was typed.
    pub fn forwarded(&mut self, count: usize, now: i64) {
        self.events_since_reset += count as u64;
        self.last_forward_ms = now;
        self.fresh = false;
    }

    /// The `/clear` finished.
    pub fn reset_done(&mut self, ok: bool, now: i64) {
        if ok {
            self.events_since_reset = 0;
            self.fresh = true;
            self.resets += 1;
            self.backoff_until_ms = 0;
        } else {
            self.backoff_until_ms = now + RESET_BACKOFF_MS;
        }
    }
}

/// The line that precedes the first batch after a reset.
pub fn handoff_preamble(dry_run: bool) -> String {
    let mode = if dry_run { "ON" } else { "OFF" };
    format!(
        "Fresh context: your earlier conversation was cleared. Your standing rules are the \
         Envoy playbook in your system prompt; Stronghold keeps the inbox, drafts, requests and \
         thread notes, and each event below carries its thread's note. Dry-run is {mode}. \
         The batch follows."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> EnvoySettings {
        EnvoySettings {
            fresh_after_secs: 300,
            reset_after_events: 40,
            reset_after_tokens: 80_000,
            ..Default::default()
        }
    }

    #[test]
    fn a_fresh_or_untouched_context_is_never_reset() {
        let budget = ContextBudget::default();
        assert_eq!(
            budget.reset_due(1_000_000_000, &settings(), Some(1_000_000)),
            None,
            "nothing was typed since the launch"
        );
        let mut budget = ContextBudget::default();
        budget.forwarded(3, 1_000);
        budget.reset_done(true, 2_000);
        assert!(budget.fresh);
        assert_eq!(budget.resets, 1);
        assert_eq!(
            budget.reset_due(10_000_000, &settings(), Some(1_000_000)),
            None,
            "a cleared context has nothing to clear, whatever a stale usage figure says"
        );
    }

    #[test]
    fn a_quiet_period_resets_and_a_burst_shares_the_context() {
        let mut budget = ContextBudget::default();
        budget.forwarded(2, 1_000);
        assert!(!budget.fresh);
        assert_eq!(budget.reset_due(1_000 + 299_999, &settings(), None), None);
        assert_eq!(
            budget.reset_due(1_000 + 300_000, &settings(), None),
            Some(ResetReason::Quiet)
        );
        let every = EnvoySettings {
            fresh_after_secs: 0,
            ..settings()
        };
        assert_eq!(
            budget.reset_due(1_000, &every, None),
            Some(ResetReason::Quiet),
            "zero quiet seconds resets before every batch"
        );
    }

    #[test]
    fn size_guards_trip_mid_burst_and_zero_turns_them_off() {
        let mut budget = ContextBudget::default();
        budget.forwarded(39, 1_000);
        assert_eq!(budget.reset_due(2_000, &settings(), Some(79_999)), None);
        budget.forwarded(1, 2_000);
        assert_eq!(
            budget.reset_due(3_000, &settings(), None),
            Some(ResetReason::Events)
        );
        let mut budget = ContextBudget::default();
        budget.forwarded(1, 1_000);
        assert_eq!(
            budget.reset_due(2_000, &settings(), Some(80_000)),
            Some(ResetReason::Tokens)
        );
        let off = EnvoySettings {
            reset_after_events: 0,
            reset_after_tokens: 0,
            ..settings()
        };
        budget.forwarded(1_000, 1_000);
        assert_eq!(budget.reset_due(2_000, &off, Some(u64::MAX)), None);
    }

    #[test]
    fn a_failed_clear_backs_off_and_a_later_success_clears_the_backoff() {
        let mut budget = ContextBudget::default();
        budget.forwarded(1, 0);
        budget.reset_done(false, 1_000);
        assert!(!budget.fresh);
        assert_eq!(budget.events_since_reset, 1);
        assert_eq!(
            budget.reset_due(1_000 + RESET_BACKOFF_MS - 1, &settings(), None),
            None
        );
        assert_eq!(
            budget.reset_due(1_000 + RESET_BACKOFF_MS, &settings(), None),
            Some(ResetReason::Quiet)
        );
        budget.reset_done(true, 2_000);
        assert_eq!(budget.backoff_until_ms, 0);
        assert!(handoff_preamble(true).contains("Dry-run is ON"));
        assert!(handoff_preamble(false).contains("Dry-run is OFF"));
    }
}

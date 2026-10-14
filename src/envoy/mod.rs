//! The Envoy: the user's long-running listener dwarf for the outside world (Slack now, other
//! protocols later). The app ingests channel events, keeps the drafts the user approves and sends
//! only on their approval; the Envoy session reads, triages and drafts.

pub mod addressee;
pub mod channel;
pub mod context;
pub mod control;
pub mod drafts;
pub mod inbox;
pub mod matching;
pub mod notes;
pub mod relay;
pub mod requests;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod slack;
pub mod worker;

/// Persisted session identity supplied by the app lifecycle owner.
pub trait EnvoyIdentity {
    fn envoy_handle(&self) -> Option<&str>;
}

pub fn authorize(
    identity: &dyn EnvoyIdentity,
    origin: &crate::review::mark::Origin,
) -> Result<(), String> {
    match (identity.envoy_handle(), origin.terminal_handle.as_deref()) {
        (Some(expected), Some(actual)) if !expected.is_empty() && expected == actual => Ok(()),
        _ => Err("Only the persisted Envoy terminal may call Envoy tools".into()),
    }
}

/// Whether the Envoy's session is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Stopped,
    Starting,
    Running,
}

/// Liveness of the slack-reply-bridge, from its heartbeat file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeStatus {
    Ok,
    /// Heartbeat older than 90 s: the daemon is dead or wedged; ingestion falls back to polling.
    Stale,
    /// No heartbeat file at all.
    Missing,
}

/// What the app knows about the Envoy, combined into one [`EnvoyHealth`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvoyStatus {
    pub session: SessionState,
    pub bridge: BridgeStatus,
    /// Drafts waiting on the user's approve/edit/reject.
    pub pending_drafts: usize,
    /// Approval records a would-be send instead of posting.
    pub dry_run: bool,
    /// Events that have waited longer than [`STUCK_AFTER_MS`] without being typed to the
    /// Envoy: the app has them, the Envoy has not seen them.
    pub stuck_events: usize,
    /// No Slack poll result for longer than [`POLL_STALE_MS`]: ingestion is wedged.
    pub poll_stale: bool,
}

/// Pending events older than this with nothing typed since are stuck.
pub const STUCK_AFTER_MS: i64 = 10 * 60_000;
/// A poll loop silent for this long (it reports at least every minute) is stale.
pub const POLL_STALE_MS: i64 = 5 * 60_000;

/// The one-word health shown on the Envoy's node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvoyHealth {
    Stopped,
    Starting,
    Listening,
    /// Events the app ingested are not reaching the Envoy terminal.
    EventsStuck,
    /// The Slack poll loop has gone quiet: nothing is being ingested.
    PollStale,
    BridgeDown,
    AwaitingApproval,
}

impl EnvoyStatus {
    /// Events not reaching the Envoy, then a dead poll loop, then a down bridge, all outrank
    /// pending drafts: the user must know when the Envoy is deaf.
    pub fn health(&self) -> EnvoyHealth {
        match self.session {
            SessionState::Stopped => EnvoyHealth::Stopped,
            SessionState::Starting => EnvoyHealth::Starting,
            SessionState::Running if self.stuck_events > 0 => EnvoyHealth::EventsStuck,
            SessionState::Running if self.poll_stale => EnvoyHealth::PollStale,
            SessionState::Running if self.bridge != BridgeStatus::Ok => EnvoyHealth::BridgeDown,
            SessionState::Running if self.pending_drafts > 0 => EnvoyHealth::AwaitingApproval,
            SessionState::Running => EnvoyHealth::Listening,
        }
    }
}

impl EnvoyHealth {
    pub fn label(self) -> &'static str {
        match self {
            EnvoyHealth::Stopped => "stopped",
            EnvoyHealth::Starting => "starting",
            EnvoyHealth::Listening => "listening",
            EnvoyHealth::EventsStuck => "events stuck",
            EnvoyHealth::PollStale => "poll stale",
            EnvoyHealth::BridgeDown => "bridge down",
            EnvoyHealth::AwaitingApproval => "waiting on approval",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(session: SessionState, bridge: BridgeStatus, pending: usize) -> EnvoyStatus {
        EnvoyStatus {
            session,
            bridge,
            pending_drafts: pending,
            dry_run: true,
            stuck_events: 0,
            poll_stale: false,
        }
    }

    #[test]
    fn health_ranks_bridge_down_over_pending_drafts() {
        use BridgeStatus::*;
        use SessionState::*;
        assert_eq!(status(Stopped, Stale, 3).health(), EnvoyHealth::Stopped);
        assert_eq!(status(Starting, Ok, 0).health(), EnvoyHealth::Starting);
        assert_eq!(
            status(Running, Missing, 2).health(),
            EnvoyHealth::BridgeDown
        );
        assert_eq!(
            status(Running, Ok, 2).health(),
            EnvoyHealth::AwaitingApproval
        );
        assert_eq!(status(Running, Ok, 0).health(), EnvoyHealth::Listening);
    }

    #[test]
    fn stuck_events_and_a_stale_poll_outrank_the_bridge_and_drafts() {
        use BridgeStatus::*;
        use SessionState::*;
        let stuck = EnvoyStatus {
            stuck_events: 3,
            ..status(Running, Missing, 2)
        };
        assert_eq!(stuck.health(), EnvoyHealth::EventsStuck);
        assert_eq!(stuck.health().label(), "events stuck");
        let stale = EnvoyStatus {
            poll_stale: true,
            ..status(Running, Stale, 2)
        };
        assert_eq!(stale.health(), EnvoyHealth::PollStale);
        assert_eq!(
            EnvoyStatus {
                stuck_events: 3,
                poll_stale: true,
                ..status(Stopped, Ok, 0)
            }
            .health(),
            EnvoyHealth::Stopped,
            "a stopped Envoy is just stopped"
        );
    }
}

#[cfg(test)]
mod authorization_tests {
    use super::*;
    use crate::review::mark::Origin;
    struct Identity(Option<String>);
    impl EnvoyIdentity for Identity {
        fn envoy_handle(&self) -> Option<&str> {
            self.0.as_deref()
        }
    }
    #[test]
    fn only_the_persisted_handle_is_accepted() {
        let envoy = Identity(Some("term_envoy".into()));
        let caller = |handle: &str| Origin {
            terminal_handle: Some(handle.into()),
            ..Origin::default()
        };
        assert!(authorize(&envoy, &caller("term_envoy")).is_ok());
        assert!(authorize(&envoy, &caller("term_other")).is_err());
        assert!(authorize(&envoy, &Origin::default()).is_err());
        assert!(authorize(&Identity(None), &caller("term_envoy")).is_err());
    }
}

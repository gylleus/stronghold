//! The Envoy: Karl's long-running listener ant for the outside world (Slack now, other
//! protocols later). The app ingests channel events, keeps the drafts Karl approves and sends
//! only on his approval; the Envoy session reads, triages and drafts.

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
    /// Drafts waiting on Karl's approve/edit/reject.
    pub pending_drafts: usize,
    /// Approval records a would-be send instead of posting.
    pub dry_run: bool,
}

/// The one-word health shown on the Envoy's node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvoyHealth {
    Stopped,
    Starting,
    Listening,
    BridgeDown,
    AwaitingApproval,
}

impl EnvoyStatus {
    /// A down bridge outranks pending drafts: Karl must know the Envoy is half-deaf.
    pub fn health(&self) -> EnvoyHealth {
        match self.session {
            SessionState::Stopped => EnvoyHealth::Stopped,
            SessionState::Starting => EnvoyHealth::Starting,
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

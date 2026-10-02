//! The Envoy: Karl's long-running listener ant for the outside world (Slack now, other
//! protocols later). The app ingests channel events, keeps the drafts Karl approves and sends
//! only on his approval; the Envoy session reads, triages and drafts.

pub mod settings;

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

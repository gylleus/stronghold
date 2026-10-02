//! The `envoy` section of `settings.json`.

use serde::{Deserialize, Serialize};

/// #agent-comms, where coworkers' agents talk to Karl's.
pub const AGENT_COMMS: &str = "C0BHZLG3X63";
/// Karl's Slack member id; mentions of it are pings to Karl.
pub const KARL_USER_ID: &str = "U0AMMLGSFL0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvoySettings {
    /// `claude` or `codex`.
    pub agent: String,
    /// `--model` for the Envoy session: an alias or a full id.
    pub model: Option<String>,
    /// Read and draft only: approving records a would-be send and relays are logged, not
    /// delivered. On until Karl turns it off.
    pub dry_run: bool,
    /// Channels whose whole feed the Envoy listens to through the bridge.
    pub channels: Vec<String>,
    /// Whose mentions and DMs count as pings to Karl.
    pub karl_user_id: String,
    /// How often mentions and DMs are polled with Slack search (the bridge can't see them).
    pub poll_secs: u64,
}

impl Default for EnvoySettings {
    fn default() -> Self {
        Self {
            agent: "claude".into(),
            model: Some("sonnet".into()),
            dry_run: true,
            channels: vec![AGENT_COMMS.into()],
            karl_user_id: KARL_USER_ID.into(),
            poll_secs: 120,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_safe_defaults() {
        let s: EnvoySettings = serde_json::from_str(r#"{"model":"opus"}"#).unwrap();
        assert_eq!(s.model.as_deref(), Some("opus"));
        assert!(s.dry_run);
        assert_eq!(s.channels, vec![AGENT_COMMS.to_string()]);
    }
}

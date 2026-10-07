//! The `envoy` section of `settings.json`.

use serde::{Deserialize, Serialize};

/// #agent-comms, where coworkers' agents talk to Karl's.
pub const AGENT_COMMS: &str = "C0BHZLG3X63";
/// Karl's Slack member id; mentions of it are pings to Karl.
pub const KARL_USER_ID: &str = "U0AMMLGSFL0";
/// Owner of the repos Slack names as `repo#N`.
pub const GITHUB_OWNER: &str = "depict-org";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvoySettings {
    /// Claude is supported; Codex starts are refused until its MCP tools can be restricted.
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
    #[serde(deserialize_with = "poll_secs")]
    pub poll_secs: u64,
    pub policies: Vec<Policy>,
    /// A batch typed after this much quiet since the last one starts a fresh context; `0`
    /// resets before every batch. Bursts share one context.
    pub fresh_after_secs: u64,
    /// Reset at the next idle point once this many events went into the context; `0` is off.
    pub reset_after_events: u64,
    /// Reset at the next idle point once the session's context (the last turn's input and
    /// cached tokens) passes this; `0` is off.
    pub reset_after_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub destination: String,
    pub mode: ApprovalMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    Approve,
    Auto {
        veto_secs: u64,
    },
}

impl EnvoySettings {
    pub fn validate_policies(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for policy in &self.policies {
            if !seen.insert(&policy.destination) {
                return Err("Duplicate policy destination".into());
            }
            if policy.destination != "*" && policy.destination != "dm" {
                super::slack::validate_conversation(&policy.destination)?;
            }
            if matches!(policy.mode, ApprovalMode::Auto { veto_secs } if veto_secs > 86400) {
                return Err("Veto window must be at most 86400 seconds".into());
            }
        }
        Ok(())
    }
    pub fn approval(&self, conversation: &str) -> ApprovalMode {
        for selector in [
            conversation,
            if conversation.starts_with('D') {
                "dm"
            } else {
                ""
            },
            "*",
        ] {
            if let Some(policy) = self.policies.iter().find(|p| p.destination == selector) {
                return policy.mode.clone();
            }
        }
        ApprovalMode::Approve
    }
}

pub const MIN_POLL_SECS: u64 = 30;

fn poll_secs<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    Ok(u64::deserialize(deserializer)?.max(MIN_POLL_SECS))
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
            policies: Vec::new(),
            fresh_after_secs: 300,
            reset_after_events: 40,
            reset_after_tokens: 80_000,
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
        assert_eq!(s.fresh_after_secs, 300);
        assert_eq!(s.reset_after_events, 40);
        assert_eq!(s.reset_after_tokens, 80_000);
        let s: EnvoySettings = serde_json::from_str(
            r#"{"fresh_after_secs":0,"reset_after_events":0,"reset_after_tokens":0}"#,
        )
        .unwrap();
        assert_eq!(
            (
                s.fresh_after_secs,
                s.reset_after_events,
                s.reset_after_tokens
            ),
            (0, 0, 0),
            "zero is a valid setting: every batch, or a guard off"
        );
    }

    #[test]
    fn loaded_poll_interval_has_a_thirty_second_floor() {
        for (input, expected) in [(0, 30), (1, 30), (29, 30), (30, 30), (120, 120)] {
            let settings: EnvoySettings =
                serde_json::from_value(serde_json::json!({"poll_secs":input})).unwrap();
            assert_eq!(settings.poll_secs, expected);
        }
    }
}

//! A notification an agent sends Karl: the `notify` MCP tool's arguments plus the caller's
//! Orca identity, carried to the app as the control API's `notify` params.

use serde_json::{Value, json};

use crate::control::ControlError;
use crate::control::protocol::Params;
use crate::review::mark::Origin;

use super::dispatch::{Incoming, Outcome, Source};
use super::settings::Severity;

/// Longest title kept.
pub const MAX_TITLE_CHARS: usize = 120;
/// Longest message kept.
pub const MAX_MESSAGE_CHARS: usize = 1_000;
/// Longest link accepted.
const MAX_URL_CHARS: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNotice {
    pub title: String,
    pub message: String,
    pub severity: Severity,
    /// An http(s) link Karl can open from the notification.
    pub url: Option<String>,
    /// The node it is about (node id, terminal handle or label), when not the caller's own.
    pub agent: Option<String>,
    /// The caller's Orca identity.
    pub origin: Origin,
}

impl AgentNotice {
    /// Validates and trims the fields.
    pub fn new(
        title: &str,
        message: Option<&str>,
        severity: Option<&str>,
        url: Option<&str>,
        agent: Option<&str>,
        origin: Origin,
    ) -> Result<Self, String> {
        let title = clip(title.trim(), MAX_TITLE_CHARS);
        if title.is_empty() {
            return Err("\"title\" is empty".into());
        }
        let severity = match severity.map(str::trim).filter(|s| !s.is_empty()) {
            None => Severity::Info,
            Some(s) => Severity::parse(s)
                .ok_or_else(|| format!("unknown severity {s:?}; use info, warn or urgent"))?,
        };
        let url = match url.map(str::trim).filter(|u| !u.is_empty()) {
            None => None,
            Some(u) if !is_web_url(u) => {
                return Err(format!("\"url\" {u:?} must be an http(s) link"));
            }
            Some(u) if u.chars().count() > MAX_URL_CHARS => {
                return Err("\"url\" is too long".into());
            }
            Some(u) => Some(u.to_owned()),
        };
        Ok(Self {
            title,
            message: clip(message.unwrap_or_default().trim(), MAX_MESSAGE_CHARS),
            severity,
            url,
            agent: agent
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(str::to_owned),
            origin,
        })
    }

    /// The control API `notify` params.
    pub fn to_params(&self) -> Value {
        let mut params = json!({
            "title": self.title,
            "message": self.message,
            "severity": self.severity.name(),
            "origin": self.origin,
        });
        if let Some(url) = &self.url {
            params["url"] = json!(url);
        }
        if let Some(agent) = &self.agent {
            params["agent"] = json!(agent);
        }
        params
    }

    /// Parses `notify` params.
    pub fn from_params(params: &Value) -> Result<Self, ControlError> {
        let p = Params::new(params);
        let origin = match params.get("origin") {
            None | Some(Value::Null) => Origin::default(),
            Some(o) => serde_json::from_value(o.clone()).map_err(|e| {
                ControlError::invalid_params(format!("\"origin\" must be an object: {e}"))
            })?,
        };
        Self::new(
            p.required_str("title")?,
            p.str("message")?,
            p.str("severity")?,
            p.str("url")?,
            p.str("agent")?,
            origin,
        )
        .map_err(ControlError::invalid_params)
    }

    /// Who sent it, for the per-agent limit: the caller's terminal, else its pane, else the
    /// node it names.
    pub fn sender(&self) -> Option<String> {
        self.origin
            .terminal_handle
            .clone()
            .or_else(|| self.origin.pane_key.clone())
            .or_else(|| self.origin.tab_id.clone())
            .or_else(|| self.agent.clone())
    }
}

impl AgentNotice {
    /// The notification to dispatch, about `node` (id, label) when it resolved, else the
    /// caller's terminal.
    pub fn incoming(&self, node: Option<(&str, &str)>) -> Incoming {
        let handle = self.origin.terminal_handle.clone();
        Incoming {
            source: Source::Agent,
            severity: self.severity,
            title: self.title.clone(),
            message: self.message.clone(),
            url: self.url.clone(),
            target: node.map(|(id, _)| id.to_owned()).or_else(|| handle.clone()),
            sender: self.sender(),
            agent_label: node.map(|(_, label)| label.to_owned()).or(handle),
        }
    }
}

/// The control API's `notify` reply: what became of the notification, the node it is filed
/// under, whether quiet mode held it back, and the `agent` that matched no node.
pub fn reply(
    outcome: &Outcome,
    notice: &AgentNotice,
    node: Option<(&str, &str)>,
    unknown_agent: Option<&str>,
    quiet: bool,
) -> Value {
    let mut reply = json!({
        "outcome": outcome.name(),
        "id": outcome.id(),
        "severity": notice.severity.name(),
        "agent": node.map(|(id, label)| json!({ "id": id, "label": label })),
        "quiet": quiet,
    });
    if let Some(agent) = unknown_agent {
        reply["unknown_agent"] = json!(agent);
    }
    match outcome {
        Outcome::Delivered { delivery, .. } => {
            reply["delivery"] = json!({
                "native": delivery.native,
                "in_app": delivery.in_app,
                "sound": delivery.sound.is_some(),
            });
        }
        Outcome::Collapsed { count, .. } => reply["count"] = json!(count),
        Outcome::RateLimited { retry_after_ms } => {
            reply["retry_after_ms"] = json!(retry_after_ms);
        }
        Outcome::Cooldown => {}
    }
    reply
}

/// Whether `url` is an http or https link (the only kind a notification opens).
pub fn is_web_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    ["http://", "https://"].iter().any(|scheme| {
        lower
            .strip_prefix(scheme)
            .is_some_and(|rest| !rest.is_empty())
    }) && !url.chars().any(char::is_whitespace)
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin {
            terminal_handle: Some("term_a".into()),
            ..Origin::default()
        }
    }

    #[test]
    fn notices_validate_and_round_trip() {
        let n = AgentNotice::new(
            "  Need a decision ",
            Some("Which schema?"),
            Some("WARN"),
            Some("https://github.com/o/r/pull/3"),
            Some(" "),
            origin(),
        )
        .unwrap();
        assert_eq!(n.title, "Need a decision");
        assert_eq!(n.severity, Severity::Warn);
        assert_eq!(n.agent, None);
        assert_eq!(n.sender().as_deref(), Some("term_a"));
        let params = n.to_params();
        assert_eq!(params["severity"], "warn");
        assert_eq!(params["origin"], json!({"terminal_handle": "term_a"}));
        assert_eq!(AgentNotice::from_params(&params).unwrap(), n);

        let plain = AgentNotice::new("t", None, None, None, None, Origin::default()).unwrap();
        assert_eq!(
            (plain.severity, plain.message.as_str()),
            (Severity::Info, "")
        );
        assert_eq!(plain.sender(), None);
    }

    #[test]
    fn bad_notices_are_refused() {
        let new = |title: &str, severity: Option<&str>, url: Option<&str>| {
            AgentNotice::new(title, None, severity, url, None, origin())
        };
        assert!(new("  ", None, None).unwrap_err().contains("title"));
        assert!(
            new("t", Some("loud"), None)
                .unwrap_err()
                .contains("severity")
        );
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "x-apple.systempreferences:",
            "https://",
            "https://a b",
        ] {
            assert!(new("t", None, Some(url)).is_err(), "{url}");
        }
        assert!(new("t", None, Some("HTTP://example.com/x")).is_ok());
        assert!(AgentNotice::from_params(&json!({"message": "m"})).is_err());
        assert!(AgentNotice::from_params(&json!({"title": "t", "origin": 3})).is_err());
    }

    #[test]
    fn long_texts_are_clipped() {
        let long = "x".repeat(MAX_TITLE_CHARS + 5);
        let n =
            AgentNotice::new(&long, Some(&"y".repeat(5_000)), None, None, None, origin()).unwrap();
        assert_eq!(n.title.chars().count(), MAX_TITLE_CHARS + 1);
        assert_eq!(n.message.chars().count(), MAX_MESSAGE_CHARS + 1);
    }
}

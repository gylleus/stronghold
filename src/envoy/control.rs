//! Envoy requests preserve the MCP process's Origin through the control socket.

use crate::control::{ControlError, ErrorCode};
use crate::review::mark::Origin;
use serde_json::Value;

pub const PUBLIC_METHODS: &[&str] = &["reach_out", "reach_out_status"];

pub const METHODS: &[&str] = &[
    "envoy_request",
    "envoy_route",
    "envoy_decline",
    "envoy_link_reply",
    "envoy_inbox",
    "envoy_event",
    "envoy_draft",
    "envoy_drafts",
    "envoy_relay",
    "envoy_propose_task",
];

#[derive(Clone, PartialEq, Eq)]
pub struct EnvoyCommand {
    pub method: String,
    pub args: Value,
    pub origin: Origin,
}

impl std::fmt::Debug for EnvoyCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvoyCommand")
            .field("method", &self.method)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl EnvoyCommand {
    pub fn parse(method: &str, params: &Value) -> Option<Result<Self, ControlError>> {
        if !METHODS.contains(&method) && !PUBLIC_METHODS.contains(&method) {
            return None;
        }
        Some((|| {
            let origin =
                serde_json::from_value(params.get("origin").cloned().unwrap_or(Value::Null))
                    .map_err(|_| {
                        ControlError::new(ErrorCode::Refused, "Envoy origin is required")
                    })?;
            Ok(Self {
                method: method.into(),
                args: params.clone(),
                origin,
            })
        })())
    }
}

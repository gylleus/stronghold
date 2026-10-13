//! What MCP tool calls do to agents, drawn as beams from the caller: a thin blue line pulling
//! from an agent it reads, a blue sweep over the agents it lists, a red laser at the agents it
//! closes (faint and dashed for a dry run), a green burst on a dwarf it spawns.
//!
//! The MCP server sends one `tool_call` per call after it answered ([`ToolCall::of`]); the app
//! resolves the ends ([`Beam`]) and both views draw them, see [`crate::ui::beam`]. Messages
//! (`send_to_agent`, `press_key`) are not beams: they fly as envelopes, see [`super`].

use serde_json::{Value, json};

use super::Endpoint;
use crate::control::protocol::{ControlError, Params};
use crate::review::mark::Origin;

/// What a tool call does to its targets, which picks its look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeamKind {
    /// Reads one agent (`get_agent`).
    Read,
    /// Reads many at once (`list_agents`, `clan_status`).
    Sweep,
    /// Types into an agent without a message (`press_key`); drawn as a message.
    Write,
    /// Closes agents (`close_agents`).
    Kill,
    /// Plans closing them without doing it (`close_agents` with `dry_run`).
    DryKill,
    /// Starts a new agent (`spawn_session`).
    Spawn,
}

impl BeamKind {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "read" => Self::Read,
            "sweep" => Self::Sweep,
            "write" => Self::Write,
            "kill" => Self::Kill,
            "dry_kill" => Self::DryKill,
            "spawn" => Self::Spawn,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Sweep => "sweep",
            Self::Write => "write",
            Self::Kill => "kill",
            Self::DryKill => "dry_kill",
            Self::Spawn => "spawn",
        }
    }
}

/// One tool call: who made it (`origin`, empty for the King), and the agents it acted on, named
/// as [`super::resolve`] or the control API's agent lookup knows them. A sweep with no targets
/// covers the whole clan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub tool: String,
    pub origin: Origin,
    pub targets: Vec<String>,
    pub kind: BeamKind,
}

impl ToolCall {
    /// The call of tool `name` with `args` that answered `result`; `None` for tools that act on
    /// no agent, or are drawn as messages already (`send_to_agent`).
    pub fn of(origin: &Origin, name: &str, args: &Value, result: &Value) -> Option<Self> {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let ids = |list: Option<&Value>| -> Vec<String> {
            list.and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|a| s(a, "id"))
                .collect()
        };
        let (kind, targets) = match name {
            "get_agent" => (
                BeamKind::Read,
                result
                    .get("agent")
                    .and_then(|a| s(a, "id"))
                    .or_else(|| s(args, "id"))
                    .into_iter()
                    .collect(),
            ),
            "list_agents" => (BeamKind::Sweep, ids(result.get("agents"))),
            "clan_status" => (BeamKind::Sweep, Vec::new()),
            "press_key" => (BeamKind::Write, s(args, "id").into_iter().collect()),
            "close_agents" => {
                let dry = result.get("dry_run").and_then(Value::as_bool) != Some(false);
                let kind = if dry {
                    BeamKind::DryKill
                } else {
                    BeamKind::Kill
                };
                let plan = result.get("plan").and_then(|p| p.get("targets"));
                (kind, ids(plan))
            }
            "spawn_session" => (
                BeamKind::Spawn,
                s(result, "node")
                    .or_else(|| s(result, "handle"))
                    .into_iter()
                    .collect(),
            ),
            _ => return None,
        };
        // A filtered listing that found nobody covers nobody.
        let empty = targets.is_empty() && name != "clan_status";
        (!empty).then(|| Self {
            tool: name.to_owned(),
            origin: origin.clone(),
            targets,
            kind,
        })
    }

    /// The control API's `tool_call` params.
    pub fn to_params(&self) -> Value {
        json!({
            "tool": self.tool,
            "origin": self.origin,
            "targets": self.targets,
            "kind": self.kind.name(),
        })
    }

    pub fn from_params(params: &Value) -> Result<Self, ControlError> {
        let p = Params::new(params);
        let tool = p.required_str("tool")?.to_owned();
        let kind = p.required_str("kind")?;
        let kind = BeamKind::parse(kind)
            .ok_or_else(|| ControlError::invalid_params(format!("unknown kind {kind:?}")))?;
        Ok(Self {
            tool,
            origin: crate::finish::origin_of(params)?,
            targets: p.strings("targets")?.unwrap_or_default(),
            kind,
        })
    }
}

/// A tool call resolved to the snapshot's nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beam {
    pub from: Endpoint,
    pub targets: Vec<Endpoint>,
    pub kind: BeamKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: Value, result: Value) -> Option<ToolCall> {
        ToolCall::of(&Origin::default(), name, &args, &result)
    }

    #[test]
    fn reads_target_the_agent_found() {
        let c = call(
            "get_agent",
            json!({"id": "Fix login"}),
            json!({"agent": {"id": "term:t1"}}),
        )
        .unwrap();
        assert_eq!(
            (c.kind, c.targets),
            (BeamKind::Read, vec!["term:t1".into()])
        );
    }

    #[test]
    fn listings_sweep_what_they_listed_and_clan_status_everyone() {
        let c = call(
            "list_agents",
            json!({}),
            json!({"agents": [{"id": "a"}, {"id": "b"}]}),
        )
        .unwrap();
        assert_eq!(
            (c.kind, c.targets),
            (BeamKind::Sweep, vec!["a".into(), "b".into()])
        );
        assert!(call("list_agents", json!({}), json!({"agents": []})).is_none());
        let c = call("clan_status", json!({}), json!({})).unwrap();
        assert_eq!((c.kind, c.targets.len()), (BeamKind::Sweep, 0));
    }

    #[test]
    fn closes_are_kills_only_when_not_dry() {
        let plan = |dry: bool| json!({"dry_run": dry, "plan": {"targets": [{"id": "term:a"}, {"id": "term:b"}]}});
        let dry = call("close_agents", json!({}), plan(true)).unwrap();
        assert_eq!(dry.kind, BeamKind::DryKill);
        assert_eq!(dry.targets, vec!["term:a".to_owned(), "term:b".into()]);
        let live = call("close_agents", json!({"dry_run": false}), plan(false)).unwrap();
        assert_eq!(live.kind, BeamKind::Kill);
        // Without a flag in the reply it is taken for a dry run, never a kill.
        let unsure = call(
            "close_agents",
            json!({}),
            json!({"plan": plan(true)["plan"]}),
        );
        assert_eq!(unsure.unwrap().kind, BeamKind::DryKill);
    }

    #[test]
    fn spawns_target_the_new_node_and_messages_are_not_beams() {
        let c = call(
            "spawn_session",
            json!({}),
            json!({"node": "term:new", "handle": "new"}),
        )
        .unwrap();
        assert_eq!(
            (c.kind, c.targets),
            (BeamKind::Spawn, vec!["term:new".into()])
        );
        assert!(call("send_to_agent", json!({"id": "a"}), json!({})).is_none());
        assert!(call("health", json!({}), json!({})).is_none());
        let key = call("press_key", json!({"id": "a", "key": "esc"}), json!({})).unwrap();
        assert_eq!(key.kind, BeamKind::Write);
    }

    #[test]
    fn round_trips_through_params() {
        let origin = Origin {
            terminal_handle: Some("term_q".into()),
            ..Origin::default()
        };
        let c = ToolCall::of(
            &origin,
            "get_agent",
            &json!({"id": "a"}),
            &json!({"agent": {"id": "a"}}),
        )
        .unwrap();
        assert_eq!(ToolCall::from_params(&c.to_params()).unwrap(), c);
        assert!(ToolCall::from_params(&json!({"tool": "x", "kind": "zap"})).is_err());
    }
}

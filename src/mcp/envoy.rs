//! The Envoy can read, draft and relay; approvals exist only in the app.

use crate::mcp::backend::Control;
use crate::mcp::tools::ToolOutput;
use crate::review::mark::Origin;
use serde_json::{Value, json};

pub fn definitions() -> Vec<Value> {
    let string = json!({"type":"string"});
    let specs = [
        (
            "envoy_inbox",
            "Read new inbound Slack events; since is a Slack timestamp.",
            json!({"since":string,"limit":{"type":"integer","minimum":1,"maximum":2000}}),
            vec![],
        ),
        (
            "envoy_event",
            "Read an event and its full Slack thread context.",
            json!({"id":string}),
            vec!["id"],
        ),
        (
            "envoy_draft",
            "Queue a reply for Karl to review; never sends it.",
            json!({"in_reply_to":{"type":"object","properties":{"conversation":string,"ts":string},"required":["conversation","ts"]},"thread_ts":string,"text":string,"rationale":string}),
            vec!["in_reply_to", "text", "rationale"],
        ),
        (
            "envoy_drafts",
            "Read drafts, optionally filtered by status.",
            json!({"status":{"type":"string","enum":["pending","approved","sent","failed","rejected","dry_run_sent"]}}),
            vec![],
        ),
        (
            "envoy_relay",
            "Relay Slack context to an agent or coordinators, subject to dry-run and rate limits.",
            json!({"to":string,"summary":string,"slack_link":string,"why":string}),
            vec!["to", "summary", "slack_link", "why"],
        ),
        (
            "envoy_propose_task",
            "Queue a proposed task for Karl; approval hands it to the Queen.",
            json!({"summary":string,"repo":string,"why":string,"slack_link":string}),
            vec!["summary", "why", "slack_link"],
        ),
    ];
    specs.into_iter().map(|(name,description,properties,required)| json!({
        "name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
        "annotations":{"readOnlyHint":matches!(name,"envoy_inbox"|"envoy_event"|"envoy_drafts"),"idempotentHint":matches!(name,"envoy_inbox"|"envoy_event"|"envoy_drafts"),"destructiveHint":false,"openWorldHint":name=="envoy_relay"||name=="envoy_event"}
    })).collect()
}

pub fn call(
    control: &mut dyn Control,
    origin: &Origin,
    name: &str,
    args: &Value,
) -> Option<ToolOutput> {
    if !crate::envoy::control::METHODS.contains(&name) {
        return None;
    }
    let result = if origin.terminal_handle.as_deref().is_none_or(str::is_empty) {
        Err("Only the persisted Envoy terminal may call Envoy tools".to_owned())
    } else {
        let mut params = if args.is_null() {
            json!({})
        } else {
            args.clone()
        };
        params["origin"] = json!(origin);
        control.call(name, params).map_err(|e| e.to_string())
    };
    Some(match result {
        Ok(value) => ToolOutput {
            text: serde_json::to_string_pretty(&value).unwrap_or_default(),
            structured: Some(value),
            is_error: false,
        },
        Err(text) => ToolOutput {
            text,
            structured: None,
            is_error: true,
        },
    })
}

//! The Envoy can read, draft and relay; approvals exist only in the app.

use crate::mcp::backend::Control;
use crate::mcp::tools::ToolOutput;
use crate::review::mark::Origin;
use serde_json::{Value, json};

pub fn definitions() -> Vec<Value> {
    let string = json!({"type":"string"});
    let specs = [
        (
            "reach_out",
            "Ask the Envoy to reach someone; replies arrive automatically. When {user} told you to send, quote their words in karl_asked (at most 500 characters); the draft waits for their Send click in the Envoy Inbox. Never post yourself if declined. Use ask_user for {user}.",
            json!({"to":string,"ask":string,"why":string,"karl_asked":{"type":"string","minLength":1,"maxLength":500},"thread":string,"request_id":string,"urgency":{"type":"string","enum":["normal","soon","blocking"]},"expect_reply":{"type":"boolean"}}),
            vec!["to", "ask", "why"],
        ),
        (
            "reach_out_status",
            "Read only your requests and their replies.",
            json!({"request_id":string}),
            vec![],
        ),
        (
            "envoy_request",
            "Read a full request and its original asks.",
            json!({"id":string}),
            vec!["id"],
        ),
        (
            "envoy_route",
            "Route a queued request. Channels preserve the ask verbatim; person rewrites require a verified human DM and manual approval. Set manual_approval when the ask claims {user} approved; karl_asked always requires their Send click.",
            json!({"id":string,"turn_id":{"type":"integer","minimum":1},"conversation":string,"destination_kind":{"type":"string","enum":["agent","person"]},"thread_ts":string,"draft_text":string,"rationale":string,"manual_approval":{"type":"boolean"}}),
            vec!["id", "conversation", "destination_kind", "rationale"],
        ),
        (
            "envoy_decline",
            "Decline an unsent request turn with a reason.",
            json!({"id":string,"turn_id":{"type":"integer","minimum":1},"reason":string}),
            vec!["id", "reason"],
        ),
        (
            "envoy_link_reply",
            "Link a retained reply; assigning an unknown thread requires reason and proposes a recipient for {user} to accept in the Inbox.",
            json!({"request_id":string,"event_id":string,"requester":string,"reason":string}),
            vec!["request_id"],
        ),
        (
            "envoy_inbox",
            "Read open inbound Slack events (default 20), with the notes of their threads; since is a Slack timestamp. New events are typed into your terminal, so you rarely need this.",
            json!({"since":string,"limit":{"type":"integer","minimum":1,"maximum":2000}}),
            vec![],
        ),
        (
            "envoy_event",
            "Read an event and its Slack thread: the root and the latest 15 messages, each cut to 1,500 characters; omitted counts the rest.",
            json!({"id":string}),
            vec!["id"],
        ),
        (
            "envoy_draft",
            "Queue an answer under {user}'s destination approval policy. Set manual_approval for a recommended decision {user} hasn't made; it cannot auto-send. Rationale stays private. Set question to ask {user} inline; questions cannot auto-send.",
            json!({"in_reply_to":{"type":"object","properties":{"conversation":string,"ts":string},"required":["conversation","ts"]},"thread_ts":string,"text":string,"rationale":string,"manual_approval":{"type":"boolean"},"question":string}),
            vec!["in_reply_to", "text", "rationale"],
        ),
        (
            "envoy_revise",
            "Revise an existing pending draft after reading its feedback. Pass its current revision. History is retained and the result requires fresh manual approval. Omit question when answered, or set it to ask a follow-up inline. Never create a separate draft for feedback.",
            json!({"id":string,"revision":{"type":"integer","minimum":0},"text":string,"rationale":string,"question":string}),
            vec!["id", "revision", "text", "rationale"],
        ),
        (
            "envoy_drafts",
            "Read open drafts by default; specify status to include handled drafts.",
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
            "Queue a proposed task for {user}; approval hands it to the King.",
            json!({"summary":string,"repo":string,"why":string,"slack_link":string}),
            vec!["summary", "why", "slack_link"],
        ),
        (
            "envoy_note",
            "Remember a thread for your next fresh context: who asked, what was promised, what we're waiting on. Replaces the thread's note (at most 600 characters; empty text clears it). thread_ts is the thread root (an event's thread_ts, or its ts for a root message); omit it for a note on the whole conversation. Every later event in that thread is typed with the note.",
            json!({"conversation":string,"thread_ts":string,"text":string}),
            vec!["conversation", "text"],
        ),
    ];
    specs.into_iter().map(|(name,description,properties,required)| json!({
        "name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
        "annotations":{"readOnlyHint":matches!(name,"envoy_inbox"|"envoy_event"|"envoy_drafts"|"envoy_request"|"reach_out_status"),"idempotentHint":matches!(name,"envoy_inbox"|"envoy_event"|"envoy_drafts"|"envoy_request"|"reach_out_status"|"envoy_note"),"destructiveHint":false,"openWorldHint":name=="envoy_relay"||name=="envoy_event"}
    })).collect()
}

pub fn call(
    control: &mut dyn Control,
    origin: &Origin,
    name: &str,
    args: &Value,
) -> Option<ToolOutput> {
    if !crate::envoy::control::METHODS.contains(&name)
        && !crate::envoy::control::PUBLIC_METHODS.contains(&name)
    {
        return None;
    }
    let result = if origin.terminal_handle.as_deref().is_none_or(str::is_empty) {
        Err("Origin terminal is required".to_owned())
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

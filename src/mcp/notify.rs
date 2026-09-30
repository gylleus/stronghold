//! The `notify` MCP tool: any agent reaches Karl through Myrstack (a pop-up, a sound, his
//! inbox), attributed to the agent's Orca terminal like `mark_pr_ready`.

use serde_json::{Value, json};

use crate::control::protocol::Params;
use crate::mcp::backend::{CallError, Control};
use crate::mcp::tools::ToolOutput;
use crate::notify::agent::{AgentNotice, MAX_MESSAGE_CHARS, MAX_TITLE_CHARS};
use crate::notify::dispatch::{AGENT_MAX, AGENT_WINDOW_MS};
use crate::review::mark::Origin;

/// When to call it, in the tool description.
pub const WHEN: &str = "Call notify when you need Karl and can't go on without him: you are \
blocked on him (access, credentials, a permission only he can grant), you need a decision or \
an answer from him, or you finished something important he is waiting for. Don't use it for \
progress updates or routine completions, or for what Myrstack already shows him (an agent \
waiting at a prompt is flagged automatically; a PR ready for review goes through \
mark_pr_ready).";

/// The tool, for `tools/list`.
pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": "notify",
        "title": "Notify Karl",
        "description": format!(
            "Sends Karl, the human who runs this fleet, a notification through Myrstack (his \
             fleet dashboard): a macOS and/or in-app pop-up with a sound, as his settings say, \
             kept in his inbox and attributed to your terminal. Clicking it selects your node \
             and opens `url`. {WHEN} Severity: info (for his information, no rush), warn \
             (needs him soon: you are blocked or need a decision), urgent (needs him now: \
             something is broken, at risk or on a deadline; gets through his quiet mode). Put \
             what and which task in the title and the ask in the message. Identical repeats \
             collapse into one; each agent gets at most {AGENT_MAX} per {} minutes.",
            AGENT_WINDOW_MS / 60_000
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": format!("Short headline, e.g. \"Blocked: need prod DB access for cp-12\" (up to {MAX_TITLE_CHARS} characters)."),
                },
                "message": {
                    "type": "string",
                    "description": format!("What you need from Karl or what happened, in a sentence or two (up to {MAX_MESSAGE_CHARS} characters)."),
                },
                "severity": {
                    "type": "string",
                    "enum": ["info", "warn", "urgent"],
                    "default": "info",
                },
                "url": {
                    "type": "string",
                    "description": "An http(s) link Karl should open: the PR, issue, dashboard or doc.",
                },
                "agent": {
                    "type": "string",
                    "description": "The node it is about when not your own terminal (node id, terminal handle or label), e.g. a coordinator reporting for a worker.",
                },
            },
            "required": ["title"],
        },
        "annotations": {
            "title": "Notify Karl",
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": false,
            "openWorldHint": false,
        },
    })]
}

/// Runs the tool; `None` for other tools.
pub fn call(
    control: &mut dyn Control,
    origin: &Origin,
    name: &str,
    args: &Value,
) -> Option<ToolOutput> {
    (name == "notify").then(|| notify(control, origin, args))
}

fn error(text: String) -> ToolOutput {
    ToolOutput {
        text,
        structured: None,
        is_error: true,
    }
}

fn notify(control: &mut dyn Control, origin: &Origin, args: &Value) -> ToolOutput {
    let p = Params::new(args);
    let fields = (|| {
        Ok::<_, crate::control::ControlError>((
            p.required_str("title")?,
            p.str("message")?,
            p.str("severity")?,
            p.str("url")?,
            p.str("agent")?,
        ))
    })();
    let notice =
        fields
            .map_err(|e| e.message)
            .and_then(|(title, message, severity, url, agent)| {
                AgentNotice::new(title, message, severity, url, agent, origin.clone())
            });
    let notice = match notice {
        Ok(n) => n,
        Err(e) => return error(format!("Invalid arguments: {e}")),
    };
    let reply = match control.call("notify", notice.to_params()) {
        Ok(reply) => reply,
        Err(CallError::NotRunning) => {
            return error(
                "Myrstack isn't running, so Karl can't be notified through it. Reach him another \
                 way (for example the Orca bus) if it can't wait."
                    .into(),
            );
        }
        Err(e) => return error(format!("Couldn't notify Karl: {e}")),
    };
    let text = reply_text(&reply);
    let is_error = reply["outcome"] == "rate_limited";
    let mut structured = reply;
    structured["origin"] = json!(notice.origin);
    ToolOutput {
        text,
        structured: Some(structured),
        is_error,
    }
}

fn reply_text(reply: &Value) -> String {
    let flag = |k: &str| reply.pointer(&format!("/delivery/{k}")) == Some(&Value::Bool(true));
    let mut out = match reply["outcome"].as_str() {
        Some("delivered") => {
            let ways: Vec<&str> = [
                ("native", "macOS pop-up"),
                ("in_app", "Myrstack pop-up"),
                ("sound", "sound"),
            ]
            .iter()
            .filter(|(k, _)| flag(k))
            .map(|(_, what)| *what)
            .collect();
            if ways.is_empty() {
                let why = if reply["quiet"] == true {
                    "Karl is in quiet mode"
                } else {
                    "it is below the severity Karl has pop-ups and sounds for"
                };
                format!(
                    "Recorded in Karl's inbox without a pop-up ({why}). Use urgent only if it \
                     really can't wait."
                )
            } else {
                format!("Sent to Karl ({}).", ways.join(", "))
            }
        }
        Some("collapsed") => format!(
            "Same as a notification you sent recently: collapsed into it (×{}), so Karl sees \
             it once. No need to repeat it.",
            reply["count"].as_u64().unwrap_or(2)
        ),
        Some("rate_limited") => {
            let mins = reply["retry_after_ms"]
                .as_i64()
                .map_or(1, |ms| (ms + 59_999) / 60_000);
            format!(
                "Not sent: you already sent {AGENT_MAX} notifications in the last {} minutes. Put \
                 what you need from Karl into one notification and try again in {mins} min.",
                AGENT_WINDOW_MS / 60_000
            )
        }
        other => format!("Myrstack answered {}.", other.unwrap_or("?")),
    };
    if let Some(label) = reply.pointer("/agent/label").and_then(Value::as_str) {
        let id = reply
            .pointer("/agent/id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        out.push_str(&format!(" Shown under {label} (`{id}`)."));
    }
    if let Some(agent) = reply["unknown_agent"].as_str() {
        out.push_str(&format!(
            " Note: no node matches agent {agent:?}; it was filed under your own terminal."
        ));
    }
    out
}

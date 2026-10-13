//! The `notify` MCP tool: any agent reaches Karl through Stronghold (a pop-up, a sound, his
//! inbox), attributed to the agent's Orca terminal like `mark_pr_ready`.

use std::path::Path;

use serde_json::{Value, json};

use crate::control::protocol::Params;
use crate::control::server::now_ms;
use crate::mcp::backend::{CallError, Control};
use crate::mcp::review::ReviewEnv;
use crate::mcp::tools::ToolOutput;
use crate::notify::agent::{AgentNotice, MAX_MESSAGE_CHARS, MAX_TITLE_CHARS};
use crate::notify::dispatch::{AGENT_MAX, AGENT_WINDOW_MS};
use crate::notify::pending;
use crate::review::mark::Origin;

/// When to call `notify`, as the clan playbook's _Human in the loop_ says it. A macro so the
/// server instructions can `concat!` the same text.
macro_rules! rules_text {
    () => {
        "When to call notify (the clan playbook's rules): \
(1) Blocked on Karl for something he must do: a permission, credential, passkey or 2FA, prod \
approval. Severity warn, or urgent if work is stalled and can't wait; say what you need and \
what it unblocks. A question you wait on him to answer goes through ask_karl instead. \
(2) Handover, you're done and the next move is Karl's: a PR ready for his review (right after \
merge_ready and mark_pr_ready; a coordinator after the ready marker), a merge waiting on him, a \
run or campaign finished. Severity info, with the url and one line on what he needs to do next; \
a status, never a question about merging. Stronghold folds a PR handover into its own \
\"PR ready for review\" notification, so Karl gets one. When you'd stay for his follow-ups \
(you usually would), make the handover an ask_karl (info, \"Done: ... Anything else, or can I \
close?\") instead, not both. \
(3) Not for progress or routine status; one notification per event. A terminal waiting at a \
prompt already shows as needs input and is notified by Stronghold; leaving, once Karl says so, is \
finish_session, which records and tells Karl itself. \
(4) Workers under a coordinator go through it (ask, escalation, merge_ready) and don't call \
notify, except to summon Karl for a credential step; a standalone session with no coordinator \
notifies Karl directly. \
(5) If Stronghold isn't running, notify queues the notification and Karl sees it in his inbox when \
Stronghold starts. A handover can wait for that; a blocking ask that can't also gets the osascript \
summon dialog."
    };
}
pub(crate) use rules_text;

/// [`rules_text`] as a constant.
pub const RULES: &str = rules_text!();

/// The tool, for `tools/list`.
pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": "notify",
        "title": "Notify Karl",
        "description": format!(
            "Sends Karl, the human who runs this clan, a notification through Stronghold (his \
             clan dashboard): a macOS and/or in-app pop-up with a sound, as his settings say, \
             kept in his inbox and attributed to your terminal. Clicking it selects your node \
             and opens `url`. Works even when Stronghold isn't running: the notification is \
             queued and reaches his inbox when it starts. {RULES} Urgent gets through Karl's quiet mode. Put what and \
             which task in the title and the ask in the message. Identical repeats collapse \
             into one; each agent gets at most {AGENT_MAX} per {} minutes.",
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
    env: &ReviewEnv,
    name: &str,
    args: &Value,
) -> Option<ToolOutput> {
    (name == "notify").then(|| notify(control, &env.origin, env.notify_pending.as_deref(), args))
}

fn error(text: String) -> ToolOutput {
    ToolOutput {
        text,
        structured: None,
        is_error: true,
    }
}

fn notify(
    control: &mut dyn Control,
    origin: &Origin,
    pending: Option<&Path>,
    args: &Value,
) -> ToolOutput {
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
        Err(CallError::NotRunning) => return queue(pending, &notice),
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

/// Queues `notice` for the app to take when it starts.
fn queue(pending: Option<&Path>, notice: &AgentNotice) -> ToolOutput {
    let Some(path) = pending else {
        return error(
            "Stronghold isn't running and there is no data dir to queue the notification in. \
             Reach Karl another way if it can't wait."
                .into(),
        );
    };
    if let Err(e) = pending::append(path, notice, now_ms()) {
        return error(format!(
            "Stronghold isn't running and queuing the notification failed: {e}. Reach Karl \
             another way if it can't wait."
        ));
    }
    ToolOutput {
        text: "Stronghold isn't running, so the notification is queued; Karl will see it in his \
               inbox when Stronghold starts. If it is a blocking ask that can't wait, also summon \
               him with the osascript dialog."
            .into(),
        structured: Some(json!({
            "outcome": "queued",
            "severity": notice.severity.name(),
            "origin": notice.origin,
        })),
        is_error: false,
    }
}

pub(crate) fn reply_text(reply: &Value) -> String {
    let flag = |k: &str| reply.pointer(&format!("/delivery/{k}")) == Some(&Value::Bool(true));
    let mut out = match reply["outcome"].as_str() {
        Some("delivered") => {
            let ways: Vec<&str> = [
                ("native", "macOS pop-up"),
                ("in_app", "Stronghold pop-up"),
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
        Some("folded") => "Folded into Stronghold's own \"PR ready for review\" notification for \
             this PR, so Karl gets one; your title and message replaced its text."
            .to_owned(),
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
        other => format!("Stronghold answered {}.", other.unwrap_or("?")),
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

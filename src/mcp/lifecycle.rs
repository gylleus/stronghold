//! `ask_user` and `finish_session`: an agent asking the user a question it waits on, and an agent
//! ending its own session once nothing is left. Both are attributed to the caller's Orca
//! terminal like `notify`; see [`crate::ask`] and [`crate::finish`].

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::ask::{AskRequest, MAX_CONTEXT_CHARS, MAX_QUESTION_CHARS};
use crate::control::protocol::Params;
use crate::finish::{
    APPROVAL_MISSING, Approval, FinishOutcome, FinishRequest, MAX_SUMMARY_CHARS, git_problems,
};
use crate::mcp::backend::{CallError, Control};
use crate::mcp::notify::reply_text;
use crate::mcp::tools::ToolOutput;
use crate::review::mark::Origin;

/// When to call `ask_user` and `finish_session`, as the clan playbook says it. A macro so the
/// server instructions can `concat!` the same text.
macro_rules! rules_text {
    () => {
        "Waiting on {user}: whenever you end a turn waiting for {user}'s answer to a question you \
asked in plain text (which way to go, may I merge, which account), call ask_user with the \
question first, then end your turn. Stronghold only sees prompts and menus on your screen, so a \
question in the chat reaches {user} only through ask_user (Stronghold also flags a turn that ends \
on a question, as a backstop). Don't use it for permission prompts or AskUserQuestion menus \
(already detected) or for status; workers under a coordinator ask their coordinator instead. \
Envoy exception: ask_user puts the question in {user}'s inbox and notifies them, but never marks \
the Envoy needs input. Keep taking batches while they decide; they can answer in your terminal. \
Done: when you think you're done, don't close yourself: {user} often has follow-ups or feedback. \
Call ask_user (severity info) with \"Done: <one line>. Anything else, or can I close?\" (that is \
your handover; put the PR in url) and stay. Call finish_session only once {user} says you can go \
(approval user), or for a tiny one-shot task with nothing to follow up (approval trivial). It \
records the outcome, shows you done and closes your terminal; your worktree and branch are kept. \
It refuses while you have uncommitted changes, commits neither merged nor pushed, or a PR still \
waiting on {user}'s review; pass force only when {user} said to stop anyway. Workers under a \
coordinator report to it instead; it releases them."
    };
}
pub(crate) use rules_text;

/// [`rules_text`] as a constant.
pub const RULES: &str = rules_text!();

/// The tools, for `tools/list`.
pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ask_user",
            "title": "Ask {user}",
            "description": format!(
                "Asks {user}, the human who runs this clan, a question you are waiting on, \
                 through Stronghold (their clan dashboard): they get a notification with the \
                 question, and your node shows needs input until they answer in your terminal. \
                 Envoy exception: the question goes to {user}'s inbox without marking needs input; \
                 keep taking batches while they decide. They can still answer in your terminal. \
                 Call it whenever you are about to end your turn waiting for their answer to a \
                 plain-text question (a decision, a go-ahead, a missing fact), then end your \
                 turn; don't keep working on anything that depends on the answer. Without it \
                 a question in the chat can sit unseen: Stronghold only detects permission \
                 prompts and menus by itself. When you think you're done, ask it here too \
                 (severity info): \"Done: <one line>. Anything else, or can I close?\", with \
                 the PR in url; that is your handover, and {user} may have follow-ups, so stay \
                 until they answer. Not for permission prompts or AskUserQuestion menus \
                 (already detected) or progress. Workers under a \
                 coordinator ask the coordinator instead. If Stronghold isn't running it \
                 errors: ask in the chat and end your turn.", user = crate::user::name()),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": format!("The question, answerable on its own, e.g. \"Merge cp-12 now or wait for the migration?\" (up to {MAX_QUESTION_CHARS} characters)."),
                    },
                    "context": {
                        "type": "string",
                        "description": format!("What {user} needs to answer: the options and your recommendation, what it blocks (up to {MAX_CONTEXT_CHARS} characters).", user = crate::user::name()),
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["info", "warn", "urgent"],
                        "default": "warn",
                        "description": "info for \"I'm done, can I close?\"; warn when work waits on the answer; urgent only when it's stalled and can't wait (it gets through {user}'s quiet mode).",
                    },
                    "url": {
                        "type": "string",
                        "description": "An http(s) link that helps them answer: the PR, issue or doc.",
                    },
                },
                "required": ["question"],
            },
            "annotations": {
                "title": "Ask {user}",
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": false,
            },
        }),
        json!({
            "name": "finish_session",
            "title": "Finish this session",
            "description": "Ends your own session, once {user} said you can go. Don't close \
                yourself just because you think you're done: {user} often has follow-up questions \
                or feedback, so first ask them with ask_user (\"Done: <one line>. Anything else, \
                or can I close?\") and stay; call this when they say go (approval: user). The \
                only exception is a tiny one-shot task with nothing to follow up, such as a \
                lookup or a one-line fix (approval: trivial). Stronghold records the outcome (shown to \
                {user} in their inbox), shows your node as done and closes your terminal a couple of \
                seconds after replying, so make it your last call and end your turn right \
                after. Your git worktree and branch are kept (cleanup is the coordinator's or \
                {user}'s). Guards: it refuses while your worktree has uncommitted changes or \
                commits neither merged into the default branch nor pushed, while a PR of yours \
                waits in {user}'s review queue (stay to address their comments; finish once it's \
                merged or {user} says you're not needed), and while a coordinator's run has open \
                tasks; fix those first, or pass force: true only when {user} explicitly told you to \
                stop anyway (the overridden problems are recorded and shown to them). A worker \
                under a coordinator can't finish itself: report to your coordinator \
                (worker_done, merge_ready) and it reuses or releases you.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": format!("One line: what you did and where it landed, e.g. \"Fixed the cp-12 retry bug; merged into main\" (up to {MAX_SUMMARY_CHARS} characters)."),
                    },
                    "outcome": {
                        "type": "string",
                        "enum": ["done", "handed_over", "abandoned"],
                        "default": "done",
                        "description": "done: the work landed or nothing was needed. handed_over: someone else carries it on (a PR {user} takes from here, another agent) and your terminal isn't needed. abandoned: {user} called it off.",
                    },
                    "pr_url": {
                        "type": "string",
                        "description": "The PR the work went into, if any.",
                    },
                    "approval": {
                        "type": "string",
                        "enum": ["user", "trivial"],
                        "description": "user: {user} told you that you can go. trivial: a tiny one-shot task with nothing they could follow up on. Anything else: ask them first and stay.",
                    },
                    "force": {
                        "type": "boolean",
                        "default": false,
                        "description": "Leave despite the guards' problems. Only when {user} said so.",
                    },
                },
                "required": ["summary", "approval"],
            },
            "annotations": {
                "title": "Finish this session",
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": false,
            },
        }),
    ]
}

/// Runs a tool; `None` for other tools. `cwd` is where the git checks run when Stronghold
/// doesn't know the caller's worktree.
pub fn call(
    control: &mut dyn Control,
    origin: &Origin,
    cwd: Option<PathBuf>,
    name: &str,
    args: &Value,
) -> Option<ToolOutput> {
    match name {
        "ask_user" => Some(ask_user(control, origin, args)),
        "finish_session" => Some(finish_session(control, origin, cwd, args)),
        _ => None,
    }
}

fn error(text: String) -> ToolOutput {
    ToolOutput {
        text,
        structured: None,
        is_error: true,
    }
}

fn ask_user(control: &mut dyn Control, origin: &Origin, args: &Value) -> ToolOutput {
    let mut params = args.clone();
    if !params.is_object() {
        params = json!({});
    }
    params["origin"] = json!(origin);
    let ask = match AskRequest::from_params(&params) {
        Ok(a) => a,
        Err(e) => return error(format!("Invalid arguments: {}", e.message)),
    };
    let reply = match control.call("ask", ask.to_params()) {
        Ok(reply) => reply,
        Err(CallError::NotRunning) => {
            return error(
                "Stronghold isn't running, so {user} can't be told through it. Ask in the chat and \
                 end your turn; if it can't wait, summon them another way (the osascript dialog)."
                    .into(),
            );
        }
        Err(e) => return error(format!("Couldn't ask {{user}}: {e}")),
    };
    let mut text = reply_text(&reply);
    if reply["waiting"] == true {
        text.push_str(
            " Your node shows needs input until {user} answers. End your turn now and wait for \
             their reply; don't act on what the answer decides.",
        );
    } else if reply["nonblocking"] == true {
        text.push_str(
            " This Envoy question does not mark your node needs input. Keep taking batches \
             while {user} decides; they can answer in your terminal. Don't act on what the answer \
             decides until they reply.",
        );
    } else {
        text.push_str(
            " Stronghold couldn't match you to a terminal, so your node won't show waiting; \
             also ask in the chat.",
        );
    }
    let is_error = reply["outcome"] == "rate_limited";
    ToolOutput {
        text,
        structured: Some(reply),
        is_error,
    }
}

fn finish_session(
    control: &mut dyn Control,
    origin: &Origin,
    cwd: Option<PathBuf>,
    args: &Value,
) -> ToolOutput {
    let p = Params::new(args);
    let fields = (|| {
        Ok::<_, crate::control::ControlError>((
            p.required_str("summary")?.trim().to_owned(),
            p.str("outcome")?.map(str::to_owned),
            p.str("pr_url")?.map(str::to_owned),
            p.bool("force")?.unwrap_or(false),
            p.str("approval")?.map(str::trim).map(str::to_owned),
        ))
    })();
    let (summary, outcome, pr_url, force, approval) = match fields {
        Ok(f) => f,
        Err(e) => return error(format!("Invalid arguments: {}", e.message)),
    };
    if summary.is_empty() {
        return error("Invalid arguments: \"summary\" is empty".into());
    }
    let outcome = match outcome.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
        None => FinishOutcome::Done,
        Some(o) => match FinishOutcome::parse(o) {
            Some(o) => o,
            None => {
                return error(format!(
                    "Invalid arguments: unknown outcome {o:?}; use done, handed_over or abandoned"
                ));
            }
        },
    };
    let approval = match approval.as_deref().filter(|a| !a.is_empty()) {
        None => {
            return error(format!(
                "Not finished; your terminal stays open. {APPROVAL_MISSING}."
            ));
        }
        Some(a) => match Approval::parse(a) {
            Some(a) => a,
            None => {
                return error(format!(
                    "Invalid arguments: unknown approval {a:?}; use user or trivial"
                ));
            }
        },
    };
    let mut request = FinishRequest {
        summary,
        outcome,
        approval: Some(approval),
        pr_url,
        force,
        forced: Vec::new(),
        dry_run: true,
        origin: origin.clone(),
    };

    let check = match control.call("finish", request.to_params()) {
        Ok(reply) => reply,
        Err(CallError::NotRunning) => {
            return error(
                "Stronghold isn't running, so it can't close your terminal. Tell {user} in the chat \
                 that you're done (your summary) and stop working; they'll close you."
                    .into(),
            );
        }
        Err(e) => return error(format!("Couldn't finish: {e}")),
    };
    if let Some(reason) = check["refused"].as_str() {
        return error(format!("Can't finish: {reason}."));
    }
    let worktree = check
        .pointer("/agent/worktree")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or(cwd);
    let mut problems = worktree.as_deref().map(git_problems).unwrap_or_default();
    request.forced.clone_from(&problems);
    problems.extend(
        check["problems"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    if !problems.is_empty() && !force {
        let list: Vec<String> = problems.iter().map(|p| format!("- {p}")).collect();
        return ToolOutput {
            text: format!(
                "Not finished; your terminal stays open. Fix these first:\n{}\nIf {user} \
                 explicitly told you to stop anyway, call finish_session again with force: \
                 true (the problems are recorded and shown to them).",
                list.join("\n"),
                user = crate::user::name()
            ),
            structured: Some(json!({ "finished": false, "problems": problems })),
            is_error: true,
        };
    }

    request.dry_run = false;
    let reply = match control.call("finish", request.to_params()) {
        Ok(reply) => reply,
        Err(e) => return error(format!("Couldn't finish: {e}")),
    };
    let label = reply
        .pointer("/agent/label")
        .and_then(Value::as_str)
        .unwrap_or("your node");
    let kept = match (
        reply.pointer("/agent/worktree").and_then(Value::as_str),
        reply.pointer("/agent/branch").and_then(Value::as_str),
    ) {
        (Some(wt), Some(branch)) => format!(" The worktree {wt} and branch {branch} are kept."),
        (Some(wt), None) => format!(" The worktree {wt} is kept."),
        _ => String::new(),
    };
    let secs = (reply["closing_in_ms"].as_u64().unwrap_or(2_000)).div_ceil(1_000);
    let forced = if problems.is_empty() {
        String::new()
    } else {
        format!(" Recorded that you left despite: {}.", problems.join("; "))
    };
    ToolOutput {
        text: format!(
            "Finished: Stronghold recorded it ({}) and shows {label} as done; it closes this \
             terminal in about {secs} s.{kept}{forced} End your turn now; don't start anything \
             else.",
            outcome.name()
        ),
        structured: Some(reply),
        is_error: false,
    }
}

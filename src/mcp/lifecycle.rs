//! `ask_karl` and `finish_session`: an agent asking Karl a question it waits on, and an agent
//! ending its own session once nothing is left. Both are attributed to the caller's Orca
//! terminal like `notify`; see [`crate::ask`] and [`crate::finish`].

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::ask::{AskRequest, MAX_CONTEXT_CHARS, MAX_QUESTION_CHARS};
use crate::control::protocol::Params;
use crate::finish::{FinishOutcome, FinishRequest, MAX_SUMMARY_CHARS, git_problems};
use crate::mcp::backend::{CallError, Control};
use crate::mcp::notify::reply_text;
use crate::mcp::tools::ToolOutput;
use crate::review::mark::Origin;

/// When to call `ask_karl` and `finish_session`, as the fleet playbook says it. A macro so the
/// server instructions can `concat!` the same text.
macro_rules! rules_text {
    () => {
        "Waiting on Karl: whenever you end a turn waiting for Karl's answer to a question you \
asked in plain text (which way to go, may I merge, which account), call ask_karl with the \
question first, then end your turn. Myrstack only sees prompts and menus on your screen, so a \
question in the chat reaches Karl only through ask_karl (Myrstack also flags a turn that ends \
on a question, as a backstop). Don't use it for permission prompts or AskUserQuestion menus \
(already detected) or for status; workers under a coordinator ask their coordinator instead. \
Leaving: when you're truly done (work merged or handed over, review addressed, nothing left), \
call finish_session with a one-line summary instead of sitting idle: Myrstack records the \
outcome, shows you done and closes your terminal; your worktree and branch are kept. It refuses \
while you have uncommitted changes, commits neither merged nor pushed, or a PR still waiting on \
Karl's review (your terminal is needed for his comments); pass force only when Karl said to stop \
anyway. Workers under a coordinator report to it instead; it releases them."
    };
}
pub(crate) use rules_text;

/// [`rules_text`] as a constant.
pub const RULES: &str = rules_text!();

/// The tools, for `tools/list`.
pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ask_karl",
            "title": "Ask Karl",
            "description": format!(
                "Asks Karl, the human who runs this fleet, a question you are waiting on, \
                 through Myrstack (his fleet dashboard): he gets a notification with the \
                 question, and your node shows needs input until he answers in your terminal. \
                 Call it whenever you are about to end your turn waiting for his answer to a \
                 plain-text question (a decision, a go-ahead, a missing fact), then end your \
                 turn; don't keep working on anything that depends on the answer. Without it \
                 a question in the chat can sit unseen: Myrstack only detects permission \
                 prompts and menus by itself. Not for permission prompts or AskUserQuestion \
                 menus (already detected), progress or handovers (use notify). Workers under a \
                 coordinator ask the coordinator instead. If Myrstack isn't running it \
                 errors: ask in the chat and end your turn."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": format!("The question, answerable on its own, e.g. \"Merge cp-12 now or wait for the migration?\" (up to {MAX_QUESTION_CHARS} characters)."),
                    },
                    "context": {
                        "type": "string",
                        "description": format!("What Karl needs to answer: the options and your recommendation, what it blocks (up to {MAX_CONTEXT_CHARS} characters)."),
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["warn", "urgent"],
                        "default": "warn",
                        "description": "urgent only when work is stalled and can't wait; it gets through Karl's quiet mode.",
                    },
                    "url": {
                        "type": "string",
                        "description": "An http(s) link that helps him answer: the PR, issue or doc.",
                    },
                },
                "required": ["question"],
            },
            "annotations": {
                "title": "Ask Karl",
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": false,
            },
        }),
        json!({
            "name": "finish_session",
            "title": "Finish this session",
            "description": "Ends your own session when you are truly done, instead of sitting \
                idle until Karl closes you: your work is merged or handed over, any review is \
                addressed, and nothing is left to do. Myrstack records the outcome (shown to \
                Karl in his inbox), shows your node as done and closes your terminal a couple of \
                seconds after replying, so make it your last call and end your turn right \
                after. Your git worktree and branch are kept (cleanup is the coordinator's or \
                Karl's). Guards: it refuses while your worktree has uncommitted changes or \
                commits neither merged into the default branch nor pushed, while a PR of yours \
                waits in Karl's review queue (stay to address his comments; finish once it's \
                merged or Karl says you're not needed), and while a coordinator's run has open \
                tasks; fix those first, or pass force: true only when Karl explicitly told you to \
                stop anyway (the overridden problems are recorded and shown to him). A worker \
                with an open dispatch can't finish itself: report to your coordinator \
                (merge_ready) and it releases you. Send any handover notify before this.",
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
                        "description": "done: the work landed or nothing was needed. handed_over: someone else carries it on (a PR Karl takes from here, another agent) and your terminal isn't needed. abandoned: Karl called it off.",
                    },
                    "pr_url": {
                        "type": "string",
                        "description": "The PR the work went into, if any.",
                    },
                    "force": {
                        "type": "boolean",
                        "default": false,
                        "description": "Leave despite the guards' problems. Only when Karl said so.",
                    },
                },
                "required": ["summary"],
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

/// Runs a tool; `None` for other tools. `cwd` is where the git checks run when Myrstack
/// doesn't know the caller's worktree.
pub fn call(
    control: &mut dyn Control,
    origin: &Origin,
    cwd: Option<PathBuf>,
    name: &str,
    args: &Value,
) -> Option<ToolOutput> {
    match name {
        "ask_karl" => Some(ask_karl(control, origin, args)),
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

fn ask_karl(control: &mut dyn Control, origin: &Origin, args: &Value) -> ToolOutput {
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
                "Myrstack isn't running, so Karl can't be told through it. Ask in the chat and \
                 end your turn; if it can't wait, summon him another way (the osascript dialog)."
                    .into(),
            );
        }
        Err(e) => return error(format!("Couldn't ask Karl: {e}")),
    };
    let mut text = reply_text(&reply);
    if reply["waiting"] == true {
        text.push_str(
            " Your node shows needs input until Karl answers. End your turn now and wait for \
             his reply; don't act on what the answer decides.",
        );
    } else {
        text.push_str(
            " Myrstack couldn't match you to a terminal, so your node won't show waiting; \
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
        ))
    })();
    let (summary, outcome, pr_url, force) = match fields {
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
    let mut request = FinishRequest {
        summary,
        outcome,
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
                "Myrstack isn't running, so it can't close your terminal. Tell Karl in the chat \
                 that you're done (your summary) and stop working; he'll close you."
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
                "Not finished; your terminal stays open. Fix these first:\n{}\nIf Karl \
                 explicitly told you to stop anyway, call finish_session again with force: \
                 true (the problems are recorded and shown to him).",
                list.join("\n")
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
            "Finished: Myrstack recorded it ({}) and shows {label} as done; it closes this \
             terminal in about {secs} s.{kept}{forced} End your turn now; don't start anything \
             else.",
            outcome.name()
        ),
        structured: Some(reply),
        is_error: false,
    }
}

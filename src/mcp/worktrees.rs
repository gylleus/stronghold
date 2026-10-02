//! `clean_worktrees`: the worktree cleanup as an MCP tool. It runs in the MCP process (Orca, git
//! and `gh` are all it needs), and a real run shows the broom in the app through the sweep
//! marker file.
//!
//! A dry run (the default) returns the plan. A real run takes the ids of the worktrees to
//! remove, as the dry run listed them: a fresh evaluation must still find each removable, and
//! the executor re-checks each right before removing it.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::mcp::tools::ToolOutput;
use crate::orca::OrcaClient;
use crate::worktrees::{self, Outcome, Plan};

pub const TOOL: &str = "clean_worktrees";

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": TOOL,
        "title": "Clean up Orca worktrees",
        "description": "Evaluates every linked Orca worktree and plans which to remove. A worktree \
            is removable only if no Orca terminal, process or active dispatch is in it, it has \
            no uncommitted changes and no open PR, and its HEAD is merged into the base branch \
            (or its PR is merged/closed with every commit on a remote); anything unknown keeps \
            it. dry_run defaults to TRUE and only returns the plan (keep/remove with reasons, \
            space freed). To remove, show Karl the plan, then call again with dry_run=false and \
            ids = the plan's removable ids he approved. Removal goes through `orca worktree rm` \
            (no --force) after a fresh re-check, and is logged.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "dry_run": { "type": "boolean", "default": true, "description": "Only return the plan. Pass false (with ids) to remove." },
                "ids": { "type": "array", "items": { "type": "string" }, "description": "With dry_run=false: the worktree ids (from the plan) to remove." },
            },
        },
        "annotations": {
            "title": "Clean up Orca worktrees",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false,
        },
    })]
}

/// Runs the tool; `None` for other names.
pub fn call(name: &str, args: &Value) -> Option<ToolOutput> {
    (name == TOOL).then(|| clean(args).unwrap_or_else(error))
}

fn error(text: String) -> ToolOutput {
    ToolOutput {
        text,
        structured: None,
        is_error: true,
    }
}

fn clean(args: &Value) -> Result<ToolOutput, String> {
    let dry_run = match args.get("dry_run") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("\"dry_run\" must be a boolean".into()),
    };
    let ids: Option<HashSet<String>> = match args.get("ids") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(
            a.iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<_>>()
                .ok_or("\"ids\" must be strings")?,
        ),
        Some(_) => return Err("\"ids\" must be an array of strings".into()),
    };
    if !dry_run && ids.as_ref().is_none_or(HashSet::is_empty) {
        return Err(
            "dry_run=false needs \"ids\": the removable worktree ids from a dry run that Karl \
             approved"
                .into(),
        );
    }
    let client = OrcaClient::new().without_sender_identity();
    let plan = worktrees::evaluate(&client, &HashMap::new(), now_ms())?;
    if dry_run {
        let mut text = format!("DRY RUN: nothing was removed.\n{}", plan.text());
        if plan.removable().next().is_some() {
            text.push_str(
                "\nTo remove, confirm with Karl, then call clean_worktrees with dry_run=false \
                 and ids = the removable ids in structuredContent.",
            );
        }
        return Ok(ToolOutput {
            text,
            structured: Some(plan_json(&plan)),
            is_error: false,
        });
    }
    let ids = ids.unwrap_or_default();
    let not_removable: Vec<&String> = ids
        .iter()
        .filter(|id| !plan.removable().any(|e| &e.id == *id))
        .collect();
    let mut chosen = plan.clone();
    chosen
        .entries
        .retain(|e| e.verdict.remove && ids.contains(&e.id));
    let removals = worktrees::execute(
        &client,
        &chosen,
        &worktrees::default_log_path(),
        now_ms,
        |_| {},
    )?;
    let mut lines = vec![worktrees::summary(&removals)];
    for r in &removals {
        lines.push(match &r.outcome {
            Outcome::Removed => format!("- removed {}/{} ({})", r.repo, r.name, r.reason),
            Outcome::Skipped { reasons } => {
                format!("- kept {}/{}: {}", r.repo, r.name, reasons.join("; "))
            }
            Outcome::Failed { error } => format!("- FAILED {}/{}: {error}", r.repo, r.name),
        });
    }
    for id in &not_removable {
        lines.push(format!(
            "- not removed {id}: not removable in a fresh evaluation"
        ));
    }
    Ok(ToolOutput {
        text: lines.join("\n"),
        structured: Some(json!({
            "removals": removals,
            "not_removable": not_removable,
            "log": worktrees::default_log_path(),
        })),
        is_error: removals
            .iter()
            .any(|r| matches!(r.outcome, Outcome::Failed { .. })),
    })
}

fn plan_json(plan: &Plan) -> Value {
    json!({
        "dry_run": true,
        "removable_ids": plan.removable().map(|e| e.id.clone()).collect::<Vec<_>>(),
        "freed_bytes": plan.freed_bytes(),
        "plan": plan,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

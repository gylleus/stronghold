//! `clean_worktrees`: the worktree cleanup as an MCP tool. It runs in the MCP process (Orca, git
//! and `gh` are all it needs), and a real run shows the saw in the app (cutting the removed
//! worktrees' trees in the farm) through the sweep marker file.
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
        "description": "Plans cleanup of idle worktrees listed by Orca or registered with Git under ~/orca/workspaces. Live terminals, processes, dispatches, parked agents, pins, main checkouts, open PRs and the age gate always keep a worktree. Merged work is removable; unmerged HEAD on no remote with known no open PR is removable after a verified local archive. Pushed unmerged work is never automatically removed. Untracked .orca/ and .local/ are saved under ~/Documents/agents/notes/<repo>/<worktree>/ (50 MB cap). Other git-work keep reasons expose archivable and archive_blockers. dry_run defaults true. Show Karl the plan before dry_run=false: ids selects ordinary removable entries; archive_ids explicitly archives and removes eligible kept entries. Archives under ~/Documents/agents/archive/<repo>/<worktree>-<timestamp>/ contain verified branch.bundle, binary working and staged patches, untracked copies (200 MB cap), and README restore commands. Fresh guards are checked before mutation, then removal uses orca worktree rm, or git worktree remove for unlisted entries, without force. The local branch is retained; nothing is pushed. Archive paths are logged.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "dry_run": { "type": "boolean", "default": true, "description": "Only return the plan. Pass false (with ids) to remove." },
                "archive_ids": { "type": "array", "items": { "type": "string" }, "description": "With dry_run=false: explicitly approved archivable worktree ids to archive locally and remove." },
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
    let ids = selected_ids(args, "ids")?;
    let archive_ids = selected_ids(args, "archive_ids")?;
    if !dry_run && ids.is_empty() && archive_ids.is_empty() {
        return Err("dry_run=false needs ids or archive_ids from a plan Karl approved".into());
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
    let not_removable: Vec<&String> = ids
        .iter()
        .chain(archive_ids.iter())
        .filter(|id| {
            !plan.entries.iter().any(|e| {
                &e.id == *id
                    && (ids.contains(*id) && e.verdict.remove
                        || archive_ids.contains(*id)
                            && (e.verdict.archivable
                                || e.verdict.remove && e.verdict.archive_blockers.is_empty()))
            })
        })
        .collect();
    let mut chosen = plan.clone();
    chosen.entries.retain(|e| {
        e.verdict.remove && ids.contains(&e.id)
            || archive_ids.contains(&e.id)
                && (e.verdict.archivable
                    || e.verdict.remove && e.verdict.archive_blockers.is_empty())
    });
    let removals = worktrees::execute_archives(
        &client,
        &chosen,
        &archive_ids,
        &worktrees::default_log_path(),
        now_ms,
        |_| {},
    )?;
    let mut lines = vec![worktrees::summary(&removals)];
    for r in &removals {
        if let Some(path) = &r.archive_path {
            lines.push(format!("- archive: {path}"));
        }
        lines.push(match &r.outcome {
            Outcome::Removed => match &r.notes_saved_to {
                Some(dir) => format!(
                    "- removed {}/{} ({}; notes saved to {dir})",
                    r.repo, r.name, r.reason
                ),
                None => format!("- removed {}/{} ({})", r.repo, r.name, r.reason),
            },
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

fn selected_ids(args: &Value, key: &str) -> Result<HashSet<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(HashSet::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{key} must contain strings"))
            })
            .collect(),
        Some(_) => Err(format!("{key} must be an array of strings")),
    }
}

fn plan_json(plan: &Plan) -> Value {
    json!({
        "dry_run": true,
        "removable_ids": plan.removable().map(|e| e.id.clone()).collect::<Vec<_>>(),
        "archivable_ids": plan.entries.iter().filter(|e| e.verdict.archivable).map(|e| e.id.clone()).collect::<Vec<_>>(),
        "freed_bytes": plan.freed_bytes(),
        "plan": plan,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

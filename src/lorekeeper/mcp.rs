//! Lorekeeper tools execute in the MCP process, including with the app closed.

use crate::mcp::tools::ToolOutput;
use crate::review::mark::Origin;
use serde_json::{Value, json};

pub fn definitions() -> Vec<Value> {
    let count = json!({"type":"integer","minimum":0,"maximum":200});
    vec![
        json!({
            "name":"lorekeeper_report", "description":"File this run's Ledger report, then call finish_session with approval trivial. Unverifiable counts as kept; kept items may be omitted. Every other count must match its items. Dry-run counts describe proposed actions.",
            "inputSchema":{"type":"object","additionalProperties":false,"properties":{
                "checked":count,"kept":count,"updated":count,"superseded":count,"merged":count,"deleted":count,"flagged":count,
                "dry_run":{"type":"boolean"},
                "items":{"type":"array","maxItems":200,"items":{"type":"object","additionalProperties":false,"properties":{
                    "path":{"type":"string","maxLength":1024},
                    "action":{"type":"string","enum":["keep","update","supersede","merge","delete","flag","unverifiable"]},
                    "reason":{"type":"string","minLength":1,"maxLength":4000},
                    "target":{"type":"string","maxLength":1024}
                },"required":["path","action","reason"]}}
            },"required":["checked","kept","updated","superseded","merged","deleted","flagged","dry_run","items"]},
            "annotations":{"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true}
        }),
        json!({
            "name":"lorekeeper_lookup", "description":"Read evidence through fixed git or GitHub operations. repo is owner/name for PRs, or an absolute worktree root under ~/repo or ~/orca/workspaces for git. git_fetch refreshes origin refs; other git operations read origin's default branch. No shell commands or extra flags. Output is capped at 256 KiB.",
            "inputSchema":{"type":"object","additionalProperties":false,"properties":{
                "op":{"type":"string","enum":["pr_view","pr_list","git_fetch","git_log","git_show","git_grep"]},
                "repo":{"type":"string","maxLength":4096}, "number":{"type":"integer","minimum":1},
                "search":{"type":"string","maxLength":2000}, "limit":{"type":"integer","minimum":1,"maximum":50},
                "path":{"type":"string","maxLength":1024}, "pattern":{"type":"string","maxLength":2000}
            },"required":["op","repo"]},
            "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true}
        }),
    ]
}

pub fn call(origin: &Origin, name: &str, args: &Value) -> Option<ToolOutput> {
    if !matches!(name, "lorekeeper_report" | "lorekeeper_lookup") {
        return None;
    }
    let result = (|| -> anyhow::Result<(String, Option<Value>)> {
        anyhow::ensure!(
            !cfg!(test),
            "Use explicit directories when testing Lorekeeper persistence"
        );
        let root = crate::claude::hook::data_dir().join("lorekeeper");
        let run = std::env::var(super::session::RUN_ENV).ok();
        let saved =
            super::session::authenticate(&root, run.as_deref(), origin.terminal_handle.as_deref())?;
        if name == "lorekeeper_report" {
            let report = serde_json::from_value(args.clone())?;
            let saved = super::report::persist(
                &root.join("reports"),
                &saved,
                origin.terminal_handle.as_deref().unwrap(),
                report,
            )?;
            Ok((
                format!(
                    "Report saved. {}. Call finish_session now with approval: trivial, then stop.",
                    saved.report.counts()
                ),
                Some(serde_json::to_value(saved)?),
            ))
        } else {
            let output = super::lookup::execute(serde_json::from_value(args.clone())?)?;
            Ok((output, None))
        }
    })();
    Some(match result {
        Ok((text, value)) => ToolOutput {
            text,
            structured: value,
            is_error: false,
        },
        Err(e) => ToolOutput {
            text: format!("{e:#}"),
            structured: None,
            is_error: true,
        },
    })
}

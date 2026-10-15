//! The clan playbook over MCP: prompts per role, a resource per file, and the `get_playbook` /
//! `list_playbook` tools. Read from disk on every request; the app need not be running.

use std::path::Path;

use serde_json::{Value, json};

use crate::mcp::backend::Control;
use crate::mcp::server::INVALID_PARAMS;
use crate::mcp::tools::ToolOutput;
use crate::playbook::fetches;
use crate::playbook::source::{self, Template};
use crate::playbook::{Playbook, Role, STANDING_RULES, Target};

pub const URI_PREFIX: &str = "stronghold://playbook/";
/// The prefix until 2026-10-13, still read: older kickoff specs and notes link to it.
const OLD_URI_PREFIX: &str = "myrstack://playbook/";
pub const INTERNAL_ERROR: i64 = -32603;
/// MCP's "resource not found".
pub const RESOURCE_NOT_FOUND: i64 = -32002;

pub const TOOLS: [&str; 4] = [
    "get_playbook",
    "list_playbook",
    "attendant_brief",
    "kickoff",
];

/// The templates Stronghold fills, as prompts and tools.
pub const TEMPLATES: [Template; 2] = [Template::AttendantBrief, Template::Kickoff];

type RpcResult = Result<Value, (i64, String)>;

fn section_arg() -> Value {
    json!({
        "name": "section",
        "description": "Only this `## ` section (case-insensitive title or its prefix).",
        "required": false,
    })
}

pub fn prompts_list() -> Value {
    let prompts: Vec<Value> = Role::ALL
        .iter()
        .map(|r| {
            json!({
                "name": r.name(),
                "title": r.title(),
                "description": r.description(),
                "arguments": [section_arg()],
            })
        })
        .chain(TEMPLATES.iter().map(|t| {
            json!({
                "name": t.name(),
                "title": template_title(*t),
                "description": template_description(*t),
                "arguments": [{
                    "name": t.arg(),
                    "description": template_arg_description(*t),
                    "required": true,
                }],
            })
        }))
        .collect();
    json!({ "prompts": prompts })
}

fn template_title(t: Template) -> &'static str {
    match t {
        Template::AttendantBrief => "Attendant brief for a run",
        Template::Kickoff => "Kickoff for a task",
    }
}

fn template_description(t: Template) -> &'static str {
    match t {
        Template::AttendantBrief => {
            "The attendant brief template filled from live clan data for a run: objective, \
             coordinator terminal, scratchpad when known; dispatches and bus event types in the \
             header. Unknown placeholders are marked FILL IN."
        }
        Template::Kickoff => {
            "The kickoff template filled from live clan data for a task: title, spec, repo, \
             ticket, blockers from deps; run, worktree and branch in the header. Unknown \
             placeholders are marked FILL IN."
        }
    }
}

fn template_arg_description(t: Template) -> &'static str {
    match t {
        Template::AttendantBrief => "The Orca run id, e.g. run_7c25cdcf46e0.",
        Template::Kickoff => "The Orca task id (of an active run).",
    }
}

pub fn prompts_get(
    playbook: Result<Playbook, String>,
    control: &mut dyn Control,
    params: &Value,
    log: Option<&Path>,
) -> RpcResult {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((INVALID_PARAMS, "missing prompt name".to_owned()))?;
    if let Some(template) = TEMPLATES.into_iter().find(|t| t.name() == name) {
        return template_prompt(playbook, control, template, params, log);
    }
    let role = Role::parse(name).ok_or_else(|| {
        let names: Vec<&str> = Role::ALL
            .map(Role::name)
            .into_iter()
            .chain(TEMPLATES.map(Template::name))
            .collect();
        (
            INVALID_PARAMS,
            format!("unknown prompt: {name} ({})", names.join(", ")),
        )
    })?;
    let section = params
        .get("arguments")
        .and_then(|a| a.get("section"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    let playbook = playbook.map_err(|e| (INTERNAL_ERROR, e))?;
    let served = playbook
        .get(&Target::Role(role), section)
        .map_err(|e| (INVALID_PARAMS, format!("{e:#}")))?;
    fetches::record(
        log,
        &format!("prompt:{name}"),
        &served.target,
        served.section.as_deref(),
        &served.version,
    );
    Ok(json!({
        "description": format!("{} · {}", role.title(), served.version.header()),
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": format!("{STANDING_RULES}\n\n{}", served.text()) },
        }],
        "_meta": { "stronghold/playbook": served.to_json() },
    }))
}

pub fn resources_list(playbook: Result<Playbook, String>) -> RpcResult {
    let playbook = playbook.map_err(|e| (INTERNAL_ERROR, e))?;
    let resources: Vec<Value> = playbook
        .files()
        .into_iter()
        .map(|file| {
            let size = std::fs::metadata(playbook.dir().join(&file))
                .map(|m| m.len())
                .unwrap_or(0);
            json!({
                "uri": format!("{URI_PREFIX}{file}"),
                "name": file,
                "title": format!("Playbook: {file}"),
                "description": resource_description(&file),
                "mimeType": "text/markdown",
                "size": size,
            })
        })
        .collect();
    Ok(json!({ "resources": resources }))
}

fn resource_description(file: &str) -> String {
    let roles: Vec<&str> = Role::ALL
        .iter()
        .filter(|r| r.files().contains(&file))
        .map(|r| r.name())
        .collect();
    if roles.is_empty() {
        "Clan playbook file.".to_owned()
    } else {
        format!("Clan playbook file, served to: {}.", roles.join(", "))
    }
}

/// `attendant-brief {run_id}` / `kickoff {task_id}`: the filled template as a user message.
fn template_prompt(
    playbook: Result<Playbook, String>,
    control: &mut dyn Control,
    template: Template,
    params: &Value,
    log: Option<&Path>,
) -> RpcResult {
    let id = params
        .get("arguments")
        .and_then(|a| a.get(template.arg()))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            (
                INVALID_PARAMS,
                format!(
                    "missing argument {} for prompt {}",
                    template.arg(),
                    template.name()
                ),
            )
        })?;
    let playbook = playbook.map_err(|e| (INTERNAL_ERROR, e))?;
    let filled =
        source::render(&playbook, control, template, id).map_err(|e| (INVALID_PARAMS, e))?;
    record_template(
        log,
        &format!("prompt:{}", template.name()),
        template,
        &filled,
    );
    Ok(json!({
        "description": format!(
            "{} · {}",
            template_title(template),
            filled.context.version.header()
        ),
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": filled.text() },
        }],
        "_meta": { "stronghold/template": filled.to_json() },
    }))
}

fn record_template(
    log: Option<&Path>,
    via: &str,
    template: Template,
    filled: &crate::playbook::fill::Filled,
) {
    fetches::record(
        log,
        via,
        &Target::File(template.file().to_owned()),
        None,
        &filled.context.version,
    );
}

pub fn resources_read(
    playbook: Result<Playbook, String>,
    params: &Value,
    log: Option<&Path>,
) -> RpcResult {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or((INVALID_PARAMS, "missing uri".to_owned()))?;
    let file = uri
        .strip_prefix(URI_PREFIX)
        .or_else(|| uri.strip_prefix(OLD_URI_PREFIX));
    let file = file.ok_or_else(|| {
        (
            RESOURCE_NOT_FOUND,
            format!("resource not found: {uri} (expected {URI_PREFIX}<file>)"),
        )
    })?;
    let playbook = playbook.map_err(|e| (INTERNAL_ERROR, e))?;
    if !playbook.files().iter().any(|f| f == file) {
        return Err((RESOURCE_NOT_FOUND, format!("resource not found: {uri}")));
    }
    let served = playbook
        .get(&Target::File(file.to_owned()), None)
        .map_err(|e| (INTERNAL_ERROR, format!("{e:#}")))?;
    fetches::record(log, "resource", &served.target, None, &served.version);
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "text/markdown",
            "text": served.text(),
            "_meta": { "stronghold/playbook": served.to_json() },
        }],
    }))
}

pub fn tool_definitions() -> Vec<Value> {
    let annotations = |title: &str| {
        json!({
            "title": title,
            "readOnlyHint": true,
            "openWorldHint": false,
            "idempotentHint": true,
        })
    };
    vec![
        json!({
            "name": "get_playbook",
            "title": "Get playbook",
            "description": "The clan playbook rules for a role (coordinator: playbook + worker \
                protocol; attendant: attendant brief + Coordinator duties and Token economy; \
                worker: agent protocol; envoy: Envoy playbook; lorekeeper: Lorekeeper playbook) \
                or one playbook file, optionally just one `## ` section. Starts with a version line (git commit, dirty flag, \
                content hash). Works without the app running.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "role": { "type": "string", "enum": Role::ALL.map(Role::name), "description": "Whose rules (default coordinator)." },
                    "file": { "type": "string", "description": "A playbook file instead of a role, e.g. KICKOFF-TEMPLATE.md (list_playbook lists them)." },
                    "section": { "type": "string", "description": "Only this `## ` section: case-insensitive title or its prefix." },
                },
            },
            "annotations": annotations("Get playbook"),
        }),
        json!({
            "name": "list_playbook",
            "title": "List playbook",
            "description": "The playbook's files with their `## ` section titles, the roles and \
                which files each gets, and the playbook version.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": annotations("List playbook"),
        }),
        json!({
            "name": "attendant_brief",
            "title": "Attendant brief for a run",
            "description": "The attendant brief template pre-filled for a run from live clan \
                data (the running app, else one direct Orca poll): campaign and objective, the \
                effective coordinator's terminal handle, its scratchpad when known, the run id. \
                A header lists what was filled from where, what is left (marked FILL IN), the \
                run's dispatches (id, worker, task) and bus event types. Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "The Orca run id, e.g. run_7c25cdcf46e0." },
                },
                "required": ["run_id"],
            },
            "annotations": annotations("Attendant brief for a run"),
        }),
        json!({
            "name": "kickoff",
            "title": "Kickoff for a task",
            "description": "The kickoff template pre-filled for a task from live clan data: \
                track name and title, scope from the task spec, repo, ticket key, blocked-by \
                from its deps and what it blocks. A header lists what was filled from where, \
                what is left (marked FILL IN), and the task's run, dispatch, worktree and \
                branch. Read-only; the task must be in an active run.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "The Orca task id." },
                },
                "required": ["task_id"],
            },
            "annotations": annotations("Kickoff for a task"),
        }),
    ]
}

/// Runs `attendant_brief` or `kickoff`; `None` if `name` is neither.
pub fn call_template_tool(
    control: &mut dyn Control,
    playbook: impl FnOnce() -> Result<Playbook, String>,
    name: &str,
    args: &Value,
    log: Option<&Path>,
) -> Option<ToolOutput> {
    let template = TEMPLATES.into_iter().find(|t| t.tool() == name)?;
    let id = args
        .get(template.arg())
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    let Some(id) = id else {
        return Some(tool_error(format!(
            "Invalid arguments: {} is required",
            template.arg()
        )));
    };
    let playbook = match playbook() {
        Ok(p) => p,
        Err(e) => return Some(tool_error(e)),
    };
    Some(match source::render(&playbook, control, template, id) {
        Ok(filled) => {
            record_template(log, &format!("tool:{name}"), template, &filled);
            ToolOutput {
                text: filled.text(),
                structured: Some(filled.to_json()),
                is_error: false,
            }
        }
        Err(e) => tool_error(e),
    })
}

/// Runs a playbook tool; `None` if `name` is not one.
pub fn call_tool(
    playbook: impl FnOnce() -> Result<Playbook, String>,
    name: &str,
    args: &Value,
    log: Option<&Path>,
) -> Option<ToolOutput> {
    let run = match name {
        "get_playbook" => get_playbook,
        "list_playbook" => list_playbook,
        _ => return None,
    };
    Some(match playbook() {
        Ok(p) => run(&p, args, log).unwrap_or_else(tool_error),
        Err(e) => tool_error(e),
    })
}

fn tool_error(text: String) -> ToolOutput {
    ToolOutput {
        text,
        structured: None,
        is_error: true,
    }
}

fn get_playbook(
    playbook: &Playbook,
    args: &Value,
    log: Option<&Path>,
) -> Result<ToolOutput, String> {
    let arg = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
    };
    let target = match (arg("role"), arg("file")) {
        (Some(_), Some(_)) => return Err("Invalid arguments: give role or file, not both".into()),
        (Some(role), None) => Target::Role(
            Role::parse(role)
                .ok_or_else(|| format!("Invalid arguments: unknown role \"{role}\""))?,
        ),
        (None, Some(file)) => Target::File(file.to_owned()),
        (None, None) => Target::Role(Role::Coordinator),
    };
    let served = playbook
        .get(&target, arg("section"))
        .map_err(|e| format!("{e:#}"))?;
    fetches::record(
        log,
        "tool:get_playbook",
        &served.target,
        served.section.as_deref(),
        &served.version,
    );
    Ok(ToolOutput {
        text: served.text(),
        structured: Some(served.to_json()),
        is_error: false,
    })
}

fn list_playbook(playbook: &Playbook, _: &Value, _: Option<&Path>) -> Result<ToolOutput, String> {
    let listing = playbook.listing().map_err(|e| format!("{e:#}"))?;
    Ok(ToolOutput {
        text: listing.text(),
        structured: Some(listing.to_json()),
        is_error: false,
    })
}

//! The fleet playbook over MCP: prompts per role, a resource per file, and the `get_playbook` /
//! `list_playbook` tools. Read from disk on every request; the app need not be running.

use serde_json::{Value, json};

use crate::mcp::server::INVALID_PARAMS;
use crate::mcp::tools::ToolOutput;
use crate::playbook::{Playbook, Role, STANDING_RULES, Target};

pub const URI_PREFIX: &str = "myrstack://playbook/";
pub const INTERNAL_ERROR: i64 = -32603;
/// MCP's "resource not found".
pub const RESOURCE_NOT_FOUND: i64 = -32002;

pub const TOOLS: [&str; 2] = ["get_playbook", "list_playbook"];

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
        .collect();
    json!({ "prompts": prompts })
}

pub fn prompts_get(playbook: Result<Playbook, String>, params: &Value) -> RpcResult {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((INVALID_PARAMS, "missing prompt name".to_owned()))?;
    let role = Role::parse(name).ok_or_else(|| {
        (
            INVALID_PARAMS,
            format!("unknown prompt: {name} (coordinator, attendant, worker)"),
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
    Ok(json!({
        "description": format!("{} · {}", role.title(), served.version.header()),
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": format!("{STANDING_RULES}\n\n{}", served.text()) },
        }],
        "_meta": { "myrstack/playbook": served.to_json() },
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
        "Fleet playbook file.".to_owned()
    } else {
        format!("Fleet playbook file, served to: {}.", roles.join(", "))
    }
}

pub fn resources_read(playbook: Result<Playbook, String>, params: &Value) -> RpcResult {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or((INVALID_PARAMS, "missing uri".to_owned()))?;
    let file = uri.strip_prefix(URI_PREFIX).ok_or_else(|| {
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
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "text/markdown",
            "text": served.text(),
            "_meta": { "myrstack/playbook": served.to_json() },
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
            "description": "The fleet playbook rules for a role (coordinator: playbook + worker \
                protocol; attendant: attendant brief + Coordinator duties and Token economy; \
                worker: agent protocol) or one playbook file, optionally just one `## ` section. \
                Starts with a version line (git commit, dirty flag, content hash). Works \
                without the app running.",
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
    ]
}

/// Runs a playbook tool; `None` if `name` is not one.
pub fn call_tool(
    playbook: impl FnOnce() -> Result<Playbook, String>,
    name: &str,
    args: &Value,
) -> Option<ToolOutput> {
    let run = match name {
        "get_playbook" => get_playbook,
        "list_playbook" => list_playbook,
        _ => return None,
    };
    Some(match playbook() {
        Ok(p) => run(&p, args).unwrap_or_else(tool_error),
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

fn get_playbook(playbook: &Playbook, args: &Value) -> Result<ToolOutput, String> {
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
    Ok(ToolOutput {
        text: served.text(),
        structured: Some(served.to_json()),
        is_error: false,
    })
}

fn list_playbook(playbook: &Playbook, _: &Value) -> Result<ToolOutput, String> {
    let listing = playbook.listing().map_err(|e| format!("{e:#}"))?;
    Ok(ToolOutput {
        text: listing.text(),
        structured: Some(listing.to_json()),
        is_error: false,
    })
}

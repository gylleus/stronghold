//! The MCP tools: their definitions (JSON Schema inputs, annotations), the control requests they
//! map to, and the chat-ready text they answer with.

use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::control::protocol::Params;
use crate::control::server::now_ms;
use crate::control::view::{
    AgentView, AlertView, FleetSummary, PromptView, QuotasView, RunView, WhipLogView, WhipView,
};
use crate::control::{ControlError, ErrorCode};
use crate::mcp::backend::{CallError, Control, NOT_RUNNING};
use crate::quota::format_duration;

/// Agents `list_agents` returns by default, and at most.
pub const DEFAULT_LIST_LIMIT: usize = 50;
pub const MAX_LIST_LIMIT: usize = 500;
/// Runs `fleet_status` lists in its text.
const RUNS_SHOWN: usize = 15;
/// Whip log entries `whip_status` shows by default.
const DEFAULT_WHIP_LOG: usize = 10;
const DEFAULT_ALERTS: usize = 20;
const LABEL_CHARS: usize = 48;
/// Children `get_agent` names in its text.
const CHILDREN_SHOWN: usize = 10;
/// Longest screen row `get_agent` shows.
const SCREEN_LINE_CHARS: usize = 240;

const FALLBACK_NOTE: &str = "Note: Myrstack isn't running, so this comes from one direct poll \
of Orca: no whip, Claude subagent, prompt detection or dashboard data. Start it with `myrstack` \
for the full picture.";

/// A tool's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    pub structured: Option<Value>,
    pub is_error: bool,
}

impl ToolOutput {
    fn ok(text: String, structured: Value) -> Self {
        Self {
            text,
            structured: structured.is_object().then_some(structured),
            is_error: false,
        }
    }

    fn error(text: String) -> Self {
        Self {
            text,
            structured: None,
            is_error: true,
        }
    }
}

enum Error {
    Args(String),
    Call(CallError),
}

impl From<CallError> for Error {
    fn from(e: CallError) -> Self {
        Error::Call(e)
    }
}

impl From<ControlError> for Error {
    fn from(e: ControlError) -> Self {
        Error::Args(e.message)
    }
}

type ToolResult = Result<ToolOutput, Error>;

// ---------------------------------------------------------------------------
// Definitions

struct Hints {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
}

const READ: Hints = Hints {
    read_only: true,
    destructive: false,
    idempotent: true,
};
/// Changes only what the dashboard shows.
const DASHBOARD: Hints = Hints {
    read_only: false,
    destructive: false,
    idempotent: true,
};
/// Acts on agents: types input, kills sessions, messages coordinators.
const ACTS: Hints = Hints {
    read_only: false,
    destructive: true,
    idempotent: false,
};
const SETTINGS: Hints = Hints {
    read_only: false,
    destructive: true,
    idempotent: true,
};

fn tool(name: &str, title: &str, description: &str, input: Value, hints: Hints) -> Value {
    let mut annotations = json!({
        "title": title,
        "readOnlyHint": hints.read_only,
        "openWorldHint": false,
    });
    if !hints.read_only {
        annotations["destructiveHint"] = json!(hints.destructive);
    }
    annotations["idempotentHint"] = json!(hints.idempotent);
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": input,
        "annotations": annotations,
    })
}

fn object(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

fn id_prop() -> Value {
    json!({
        "type": "string",
        "description": "Agent id from list_agents (term:…, dispatch:…, sub:…), a terminal handle, or an exact label.",
    })
}

const STATES: [&str; 6] = [
    "working",
    "idle",
    "needs_input",
    "stalled",
    "done",
    "failed",
];
const KINDS: [&str; 4] = ["coordinator", "worker", "terminal", "subagent"];

fn one_or_many(values: &[&str], description: &str) -> Value {
    json!({
        "description": description,
        "anyOf": [
            { "type": "string", "enum": values },
            { "type": "array", "items": { "type": "string", "enum": values } },
        ],
    })
}

/// Every tool, for `tools/list`.
pub fn definitions() -> Vec<Value> {
    vec![
        tool(
            "fleet_status",
            "Fleet status",
            "The Queen's view of the whole fleet: its state, agent counts, every run with its \
             objective, coordinator and task counts, who needs Karl's input, stalled or \
             escalated agents, the whip and the plan quotas. Start here.",
            object(json!({}), &[]),
            READ,
        ),
        tool(
            "list_agents",
            "List agents",
            "Lists agents (coordinators, workers, loose terminals, subagents) as compact rows \
             with their ids. Filters combine with AND.",
            object(
                json!({
                    "state": one_or_many(&STATES, "Only these states."),
                    "kind": one_or_many(&KINDS, "Only these kinds."),
                    "run": { "type": "string", "description": "Run id (run_…)." },
                    "repo": { "type": "string", "description": "Repo name." },
                    "needs_input": { "type": "boolean", "description": "Waiting on Karl (or not)." },
                    "live": { "type": "boolean", "description": "Terminal still open (or not)." },
                    "text": { "type": "string", "description": "Case-insensitive substring of the label, branch or run objective." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIST_LIMIT, "default": DEFAULT_LIST_LIMIT },
                }),
                &[],
            ),
            READ,
        ),
        tool(
            "get_agent",
            "Get agent",
            "One agent's details, its children and the tail of its terminal screen.",
            object(
                json!({
                    "id": id_prop(),
                    "lines": { "type": "integer", "minimum": 0, "maximum": 500, "default": 40, "description": "Screen rows to return." },
                }),
                &["id"],
            ),
            READ,
        ),
        tool(
            "send_to_agent",
            "Send text to an agent",
            "Types text into an agent's terminal and, unless submit is false, presses Enter. \
             Needs a live terminal (not a subagent). This is real input to a running agent: \
             confirm with Karl first.",
            object(
                json!({
                    "id": id_prop(),
                    "text": { "type": "string", "description": "Text to type. May be empty when submit is true (just presses Enter)." },
                    "submit": { "type": "boolean", "default": true, "description": "Press Enter after the text." },
                }),
                &["id", "text"],
            ),
            ACTS,
        ),
        tool(
            "press_key",
            "Press a key in an agent's terminal",
            "Sends one key to an agent's terminal: enter, esc, or interrupt (Ctrl-C, which can \
             stop the agent's current turn).",
            object(
                json!({
                    "id": id_prop(),
                    "key": { "type": "string", "enum": ["enter", "esc", "interrupt"] },
                }),
                &["id", "key"],
            ),
            ACTS,
        ),
        tool(
            "close_agents",
            "Close agent sessions",
            "Kills agents' sessions: an active dispatch is stopped, then the terminal is closed. \
             dry_run defaults to TRUE, which only returns the plan and its warnings. To really \
             close, show Karl the plan and call again with dry_run=false. Subagents and the \
             Queen cannot be closed.",
            object(
                json!({
                    "ids": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Agent ids to close." },
                    "whole_tab": { "type": "boolean", "default": false, "description": "Close each terminal's whole Orca tab." },
                    "dry_run": { "type": "boolean", "default": true, "description": "Only return the plan. Pass false to actually close." },
                }),
                &["ids"],
            ),
            ACTS,
        ),
        tool(
            "broadcast_to_coordinators",
            "Broadcast to coordinators",
            "Sends a Myrstack-prefixed Orca bus message to the live coordinators of the given \
             runs, or of all runs with all=true. Give exactly one of runs or all.",
            object(
                json!({
                    "text": { "type": "string", "description": "Message text." },
                    "runs": { "type": "array", "items": { "type": "string" }, "description": "Run ids." },
                    "all": { "type": "boolean", "description": "Every run with a live coordinator." },
                }),
                &["text"],
            ),
            ACTS,
        ),
        tool(
            "whip_status",
            "Whip status",
            "The whip's settings and state, and its newest log entries.",
            object(
                json!({
                    "limit": { "type": "integer", "minimum": 0, "maximum": 200, "default": DEFAULT_WHIP_LOG, "description": "Log entries to show." },
                }),
                &[],
            ),
            READ,
        ),
        tool(
            "set_whip",
            "Change whip settings",
            "Turns the whip on or off, its dry-run mode (dry_run=false lets it really nudge and \
             escalate agents), or its notifications. The change is saved like the settings \
             window saves it. Give at least one field.",
            object(
                json!({
                    "enabled": { "type": "boolean" },
                    "dry_run": { "type": "boolean" },
                    "notify": { "type": "boolean" },
                }),
                &[],
            ),
            SETTINGS,
        ),
        tool(
            "focus_agent",
            "Focus an agent",
            "Switches the dashboard to the graph, selects the node and zooms onto it. Pass \
             id \"queen\" for the Queen.",
            object(
                json!({
                    "id": id_prop(),
                    "raise_window": { "type": "boolean", "default": false, "description": "Also bring the Myrstack window to the front." },
                }),
                &["id"],
            ),
            DASHBOARD,
        ),
        tool(
            "set_filter",
            "Set dashboard filter",
            "Changes which nodes the dashboard shows. Omitted fields stay as they are; \
             clear=true resets everything first. Empty lists mean all.",
            object(
                json!({
                    "clear": { "type": "boolean" },
                    "repos": { "type": "array", "items": { "type": "string" } },
                    "states": { "type": "array", "items": { "type": "string", "enum": STATES } },
                    "kinds": { "type": "array", "items": { "type": "string", "enum": KINDS } },
                    "text": { "type": "string" },
                    "show_loose": { "type": "boolean", "description": "Show loose terminals." },
                    "show_released": { "type": "boolean", "description": "Show released workers and finished subagents." },
                }),
                &[],
            ),
            DASHBOARD,
        ),
        tool(
            "set_view",
            "Set dashboard view",
            "Switches the dashboard between the graph and the table.",
            object(
                json!({ "view": { "type": "string", "enum": ["graph", "table"] } }),
                &["view"],
            ),
            DASHBOARD,
        ),
        tool(
            "fit_graph",
            "Fit graph",
            "Switches to the graph and fits every node on screen.",
            object(json!({}), &[]),
            DASHBOARD,
        ),
        tool(
            "open_whip_settings",
            "Open whip settings",
            "Opens the Whip settings window in the app.",
            object(json!({}), &[]),
            DASHBOARD,
        ),
        tool(
            "alerts",
            "Alerts",
            "Notifications Myrstack posted (needs input, stalls, …), newest first.",
            object(
                json!({ "limit": { "type": "integer", "minimum": 1, "default": DEFAULT_ALERTS } }),
                &[],
            ),
            READ,
        ),
        tool(
            "quotas",
            "Plan quotas",
            "Claude and Codex plan usage per window (5h, week, …) and when they reset.",
            object(json!({}), &[]),
            READ,
        ),
    ]
}

// ---------------------------------------------------------------------------
// Calls

/// Runs a tool; `None` when there is no such tool.
pub fn call(control: &mut dyn Control, name: &str, args: &Value) -> Option<ToolOutput> {
    let p = Params::new(args);
    let result = match name {
        "fleet_status" => fleet_status(control),
        "list_agents" => list_agents(control, &p),
        "get_agent" => get_agent(control, &p),
        "send_to_agent" => send_to_agent(control, &p),
        "press_key" => press_key(control, &p),
        "close_agents" => close_agents(control, &p),
        "broadcast_to_coordinators" => broadcast(control, &p),
        "whip_status" => whip_status(control, &p),
        "set_whip" => set_whip(control, &p),
        "focus_agent" => focus_agent(control, &p),
        "set_filter" => set_filter(control, args),
        "set_view" => set_view(control, &p),
        "fit_graph" => simple(control, "fit", "Fitted the graph to the screen."),
        "open_whip_settings" => simple(
            control,
            "open_whip_settings",
            "Opened the Whip settings window.",
        ),
        "alerts" => alerts(control, &p),
        "quotas" => quotas(control),
        _ => return None,
    };
    Some(result.unwrap_or_else(|e| ToolOutput::error(error_text(e))))
}

fn error_text(e: Error) -> String {
    match e {
        Error::Args(m) => format!("Invalid arguments: {m}"),
        Error::Call(CallError::NotRunning) => NOT_RUNNING.into(),
        Error::Call(CallError::Transport(m)) => format!("Couldn't talk to Myrstack: {m}"),
        Error::Call(CallError::Control(e)) => match e.code {
            ErrorCode::NotFound => {
                format!(
                    "Not found: {}. Use list_agents to find agent ids.",
                    e.message
                )
            }
            ErrorCode::Unavailable => format!("Myrstack isn't ready: {}", e.message),
            ErrorCode::Refused => format!("Refused: {}", e.message),
            ErrorCode::InvalidParams => format!("Invalid arguments: {}", e.message),
            ErrorCode::Timeout => format!("Myrstack didn't answer in time: {}", e.message),
            _ => format!("Myrstack error: {e}"),
        },
    }
}

/// Runs `f` against the app, or against the headless fallback when the app isn't running.
/// Returns whether the fallback answered.
fn with_fallback<T>(
    control: &mut dyn Control,
    f: impl Fn(&mut dyn Control) -> Result<T, CallError>,
) -> Result<(T, bool), CallError> {
    match f(control) {
        Err(CallError::NotRunning) => {}
        other => return other.map(|t| (t, false)),
    }
    let fallback = control.fallback().map_err(|e| {
        log::debug!("no fallback: {e}");
        CallError::NotRunning
    })?;
    f(fallback).map(|t| (t, true))
}

fn parse<T: DeserializeOwned>(value: &Value) -> Result<T, CallError> {
    serde_json::from_value(value.clone())
        .map_err(|e| CallError::Transport(format!("unexpected reply: {e}")))
}

fn agents_of(result: &Value) -> Result<Vec<AgentView>, CallError> {
    parse(result.get("agents").unwrap_or(&Value::Null))
}

struct Status {
    status: Value,
    fleet: FleetSummary,
    waiting: Vec<AgentView>,
    stalled: Vec<AgentView>,
    coordinators: Vec<AgentView>,
    quotas: Option<QuotasView>,
}

fn fleet_status(control: &mut dyn Control) -> ToolResult {
    let (s, fallback) = with_fallback(control, |c| {
        let status = c.call("status", Value::Null)?;
        let fleet: FleetSummary = parse(status.get("fleet").unwrap_or(&Value::Null))?;
        let ready = status.get("ready").and_then(Value::as_bool) == Some(true);
        let (waiting, stalled, coordinators) = if ready {
            (
                agents_of(&c.call("agents", json!({"needs_input": true}))?)?,
                agents_of(&c.call("agents", json!({"state": "stalled"}))?)?,
                agents_of(&c.call("agents", json!({"kind": "coordinator"}))?)?,
            )
        } else {
            Default::default()
        };
        let quotas = c
            .call("quotas", Value::Null)
            .ok()
            .map(|q| parse(&q))
            .transpose()?;
        Ok(Status {
            status,
            fleet,
            waiting,
            stalled,
            coordinators,
            quotas,
        })
    })?;
    let now = now_ms();
    let text = fleet_text(&s, fallback, now);
    let structured = json!({
        "fallback": fallback,
        "ready": s.status.get("ready"),
        "polled_at_ms": s.status.get("polled_at_ms"),
        "errors": s.status.get("errors"),
        "fleet": s.fleet,
        "needs_input": s.waiting.iter().map(compact).collect::<Vec<_>>(),
        "stalled": s.stalled.iter().map(compact).collect::<Vec<_>>(),
        "whip": if fallback { Value::Null } else { s.status.get("whip").cloned().unwrap_or_default() },
        "quotas": s.quotas,
    });
    Ok(ToolOutput::ok(text, structured))
}

fn fleet_text(s: &Status, fallback: bool, now: i64) -> String {
    let mut out = Vec::new();
    if fallback {
        out.push(FALLBACK_NOTE.to_owned());
        out.push(String::new());
    }
    if s.status.get("ready").and_then(Value::as_bool) != Some(true) {
        out.push("Myrstack has not polled Orca yet; try again in a few seconds.".into());
        return out.join("\n");
    }
    let f = &s.fleet;
    let kinds = counts(
        &f.by_kind,
        &["coordinator", "worker", "terminal", "subagent"],
    );
    out.push(format!(
        "Fleet: {} · {} agents, {} live ({kinds})",
        f.state.replace('_', " "),
        f.agents,
        f.live
    ));
    out.push(format!("States: {}", counts(&f.by_state, &STATES)));

    out.push(String::new());
    if s.waiting.is_empty() {
        out.push("Needs input: nobody".into());
    } else {
        out.push(format!("Needs input ({}):", s.waiting.len()));
        for a in &s.waiting {
            out.push(format!("- {}", waiting_line(a)));
        }
    }
    let escalated = strings(s.status.pointer("/whip/escalated"));
    if s.stalled.is_empty() && escalated.is_empty() {
        out.push("Stalled: none".into());
    } else {
        out.push(format!(
            "Stalled ({}):",
            s.stalled.len().max(escalated.len())
        ));
        for a in &s.stalled {
            out.push(format!(
                "- {} `{}` ({}{}), idle {}",
                truncate(&a.label, LABEL_CHARS),
                a.id,
                a.kind,
                a.run_id
                    .as_deref()
                    .map(|r| format!(", {r}"))
                    .unwrap_or_default(),
                a.last_output_ms.map_or("?".into(), |t| since(now, t)),
            ));
        }
        let listed: Vec<&str> = s
            .stalled
            .iter()
            .filter_map(|a| a.terminal_handle.as_deref())
            .collect();
        for handle in escalated.iter().filter(|h| !listed.contains(&h.as_str())) {
            out.push(format!("- escalated terminal {handle}"));
        }
    }

    out.push(String::new());
    out.push(runs_text(&f.runs, &s.coordinators, now));

    out.push(String::new());
    if fallback {
        out.push("Whip: unknown (Myrstack isn't running)".into());
    } else if let Some(w) = s.status.get("whip") {
        out.push(whip_line(w, now));
    }
    if let Some(q) = &s.quotas {
        out.push(format!("Quotas: {}", quota_line(q, now)));
    }
    let errors = strings(s.status.get("errors"));
    if !errors.is_empty() {
        out.push(format!("Orca errors: {}", errors.join("; ")));
    }
    out.join("\n")
}

fn waiting_line(a: &AgentView) -> String {
    let prompt = a
        .needs_input
        .as_ref()
        .map_or(String::new(), |p| format!(" — {}", prompt_text(p)));
    let run = a
        .run_objective
        .as_deref()
        .or(a.run_id.as_deref())
        .map(|r| format!(", run \"{}\"", truncate(r, 40)))
        .unwrap_or_default();
    format!(
        "{} `{}` ({}{run}){prompt}",
        truncate(&a.label, LABEL_CHARS),
        a.id,
        a.kind
    )
}

fn runs_text(runs: &[RunView], coordinators: &[AgentView], now: i64) -> String {
    if runs.is_empty() {
        return "Runs: none".into();
    }
    let mut sorted: Vec<&RunView> = runs.iter().collect();
    sorted.sort_by_key(|r| {
        (
            !r.needs_input,
            !r.stalled,
            r.coordinator.is_none(),
            std::cmp::Reverse(r.last_activity_ms),
        )
    });
    let mut out = vec![
        format!("Runs ({}):", runs.len()),
        "| Run | Objective | Coordinator | Tasks | Workers | Active | Flags |".into(),
        "|---|---|---|---|---|---|---|".into(),
    ];
    for r in sorted.iter().take(RUNS_SHOWN) {
        let coordinator = match &r.coordinator {
            Some(id) => {
                let label = coordinators
                    .iter()
                    .find(|a| &a.id == id)
                    .map_or(id.as_str(), |a| a.label.as_str());
                format!(
                    "{} ({})",
                    truncate(label, 28),
                    r.coordinator_state.as_deref().unwrap_or("?")
                )
            }
            None => "none".into(),
        };
        let tasks = r.tasks.map_or("?".into(), |t| {
            let mut parts = vec![format!("{}/{} done", t.completed, t.total)];
            for (n, name) in [
                (t.dispatched, "dispatched"),
                (t.ready, "ready"),
                (t.pending, "pending"),
                (t.blocked, "blocked"),
                (t.failed, "failed"),
            ] {
                if n > 0 {
                    parts.push(format!("{n} {name}"));
                }
            }
            parts.join(", ")
        });
        let workers = if r.workers == 0 {
            "0".into()
        } else {
            let active: Vec<String> = ["working", "needs_input", "stalled", "idle"]
                .iter()
                .filter_map(|s| {
                    r.by_state
                        .get(*s)
                        .map(|n| format!("{n} {}", s.replace('_', " ")))
                })
                .collect();
            if active.is_empty() {
                r.workers.to_string()
            } else {
                format!("{} ({})", r.workers, active.join(", "))
            }
        };
        let mut flags = Vec::new();
        if r.needs_input {
            flags.push("NEEDS INPUT");
        }
        if r.stalled {
            flags.push("STALLED");
        }
        out.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            r.id,
            cell(&truncate(r.objective.as_deref().unwrap_or("-"), 50)),
            cell(&coordinator),
            tasks,
            workers,
            r.last_activity_ms.map_or("-".into(), |t| since(now, t)),
            flags.join(", "),
        ));
    }
    if runs.len() > RUNS_SHOWN {
        out.push(format!(
            "…and {} older runs (see structuredContent.fleet.runs)",
            runs.len() - RUNS_SHOWN
        ));
    }
    out.join("\n")
}

fn whip_line(w: &Value, now: i64) -> String {
    let on = |k: &str| w.get(k).and_then(Value::as_bool) == Some(true);
    let mut parts = vec![format!(
        "Whip: {}{}",
        if on("enabled") { "on" } else { "off" },
        if on("dry_run") { ", dry-run" } else { ", LIVE" }
    )];
    if let Some(n) = w.get("findings").and_then(Value::as_u64) {
        parts.push(format!("{n} stall findings"));
    }
    let escalated = strings(w.get("escalated"));
    parts.push(format!("{} escalated", escalated.len()));
    match w.get("last_tick_ms").and_then(Value::as_i64) {
        Some(t) => parts.push(format!("last tick {}", since(now, t))),
        None => parts.push("no tick yet".into()),
    }
    parts.join(" · ")
}

fn quota_line(q: &QuotasView, now: i64) -> String {
    if q.providers.is_empty() {
        return q.error.clone().unwrap_or_else(|| "none reported".into());
    }
    q.providers
        .iter()
        .map(|p| {
            let windows: Vec<String> = p
                .windows
                .iter()
                .map(|w| {
                    let reset = w
                        .resets_at_ms
                        .map(|t| format!(" ({})", reset_text(t - now)))
                        .unwrap_or_default();
                    format!("{} {:.0}%{reset}", w.label, w.used_percent)
                })
                .collect();
            format!("{} {}", p.name, windows.join(", "))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn list_agents(control: &mut dyn Control, p: &Params) -> ToolResult {
    let limit = p
        .usize("limit")?
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let mut filters = Map::new();
    for key in ["state", "kind"] {
        if let Some(v) = p.strings(key)? {
            filters.insert(key.into(), json!(v));
        }
    }
    for key in ["run", "repo", "text"] {
        if let Some(v) = p.str(key)? {
            filters.insert(key.into(), json!(v));
        }
    }
    for key in ["needs_input", "live"] {
        if let Some(v) = p.bool(key)? {
            filters.insert(key.into(), json!(v));
        }
    }
    let filters = Value::Object(filters);
    let (agents, fallback) =
        with_fallback(control, |c| agents_of(&c.call("agents", filters.clone())?))?;
    let now = now_ms();
    let shown = &agents[..agents.len().min(limit)];
    let mut out = Vec::new();
    if fallback {
        out.push(FALLBACK_NOTE.to_owned());
        out.push(String::new());
    }
    if agents.is_empty() {
        out.push("No agents match.".into());
    } else {
        out.push(format!(
            "{} agents{}:",
            agents.len(),
            if shown.len() < agents.len() {
                format!(" (showing {}; raise limit for more)", shown.len())
            } else {
                String::new()
            }
        ));
        out.push("| id | kind | state | label | run | idle |".into());
        out.push("|---|---|---|---|---|---|".into());
        for a in shown {
            let mut state = a.state.clone();
            if !a.live && a.kind != "subagent" {
                state.push_str(" (closed)");
            }
            out.push(format!(
                "| {} | {} | {} | {} | {} | {} |",
                a.id,
                a.kind,
                state,
                cell(&truncate(&a.label, LABEL_CHARS)),
                a.run_id.as_deref().unwrap_or("-"),
                a.last_output_ms.map_or("-".into(), |t| since(now, t)),
            ));
        }
    }
    let structured = json!({
        "fallback": fallback,
        "count": agents.len(),
        "shown": shown.len(),
        "agents": shown.iter().map(compact).collect::<Vec<_>>(),
    });
    Ok(ToolOutput::ok(out.join("\n"), structured))
}

/// The fields a chat needs from an agent.
fn compact(a: &AgentView) -> Value {
    json!({
        "id": a.id,
        "kind": a.kind,
        "label": a.label,
        "state": a.state,
        "live": a.live,
        "run_id": a.run_id,
        "repo": a.repo,
        "branch": a.branch,
        "agent": a.agent,
        "parent": a.parent,
        "idle_secs": a.last_output_ms.map(|t| ((now_ms() - t) / 1000).max(0)),
        "needs_input": a.needs_input,
        "stalled": a.stalled,
        "released": a.released,
    })
}

fn get_agent(control: &mut dyn Control, p: &Params) -> ToolResult {
    let id = p.required_str("id")?;
    let mut params = json!({ "id": id });
    if let Some(lines) = p.usize("lines")? {
        params["lines"] = json!(lines);
    }
    let result = control.call("agent", params)?;
    let a: AgentView = parse(result.get("agent").unwrap_or(&Value::Null))?;
    let now = now_ms();
    let mut out = vec![format!(
        "{} — {}, {}{}",
        a.label,
        a.kind,
        a.state.replace('_', " "),
        if a.live {
            " (live)"
        } else {
            " (terminal closed)"
        }
    )];
    let mut facts = vec![format!("id `{}`", a.id)];
    if let Some(h) = &a.terminal_handle {
        facts.push(format!("terminal {h}"));
    }
    if let Some(r) = &a.run_id {
        match &a.run_objective {
            Some(o) => facts.push(format!("run {r} \"{}\"", truncate(o, 60))),
            None => facts.push(format!("run {r}")),
        }
    }
    if let Some(t) = &a.task_id {
        facts.push(format!("task {t}"));
    }
    facts.push(format!(
        "repo {}{}",
        a.repo,
        a.branch
            .as_deref()
            .map(|b| format!(" @ {b}"))
            .unwrap_or_default()
    ));
    if let Some(agent) = &a.agent {
        facts.push(agent.clone());
    }
    if let Some(t) = a.last_output_ms {
        facts.push(format!("last output {}", since(now, t)));
    }
    if let Some(t) = &a.tokens {
        facts.push(format!("{} tokens", human_count(t.total)));
    }
    out.push(facts.join(" · "));
    if let Some(prompt) = &a.needs_input {
        out.push(format!("Needs input: {}", prompt_text(prompt)));
    }
    if a.stalled || a.escalated {
        out.push("Stalled (escalated by the whip)".into());
    }
    if let Some(parent) = &a.parent {
        out.push(format!("Parent: {parent}"));
    }
    let children = strings(result.get("children"));
    if !children.is_empty() {
        let more = children.len().saturating_sub(CHILDREN_SHOWN);
        out.push(format!(
            "Children ({}): {}{}",
            children.len(),
            children[..children.len() - more].join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ));
    }
    match result.get("screen") {
        Some(Value::Object(screen)) => {
            if let Some(e) = screen.get("error").and_then(Value::as_str) {
                out.push(format!("Screen read failed: {e}"));
            } else {
                let lines = strings(screen.get("lines"));
                out.push(format!("Screen (last {} lines):", lines.len()));
                out.push("```".into());
                out.extend(lines.iter().map(|l| tidy_line(l)));
                out.push("```".into());
            }
        }
        _ => {
            if let Some(preview) = &a.preview {
                out.push(format!("Preview: {preview}"));
            }
        }
    }
    Ok(ToolOutput::ok(out.join("\n"), result))
}

fn send_to_agent(control: &mut dyn Control, p: &Params) -> ToolResult {
    let id = p.required_str("id")?;
    let text = p
        .str("text")?
        .ok_or_else(|| Error::Args("missing \"text\"".into()))?;
    let submit = p.bool("submit")?.unwrap_or(true);
    if text.is_empty() && !submit {
        return Err(Error::Args(
            "text is empty and submit is false: nothing to send".into(),
        ));
    }
    let result = control.call("send", json!({"id": id, "text": text, "enter": submit}))?;
    let agent = result.get("agent").and_then(Value::as_str).unwrap_or(id);
    let text = if submit {
        format!("Sent to {agent} and pressed Enter.")
    } else {
        format!("Typed into {agent} (not submitted).")
    };
    Ok(ToolOutput::ok(text, result))
}

fn press_key(control: &mut dyn Control, p: &Params) -> ToolResult {
    let id = p.required_str("id")?;
    let key = p.required_str("key")?;
    let method = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => "enter",
        "esc" | "escape" => "esc",
        "interrupt" | "ctrl-c" | "ctrl+c" => "interrupt",
        other => {
            return Err(Error::Args(format!(
                "unknown key {other:?}: use enter, esc or interrupt"
            )));
        }
    };
    let result = control.call(method, json!({ "id": id }))?;
    let agent = result.get("agent").and_then(Value::as_str).unwrap_or(id);
    Ok(ToolOutput::ok(
        format!("Pressed {method} in {agent}."),
        result,
    ))
}

fn close_agents(control: &mut dyn Control, p: &Params) -> ToolResult {
    let ids = p
        .strings("ids")?
        .filter(|ids| !ids.is_empty())
        .ok_or_else(|| Error::Args("missing \"ids\"".into()))?;
    let whole_tab = p.bool("whole_tab")?.unwrap_or(false);
    let dry_run = p.bool("dry_run")?.unwrap_or(true);
    let mut params = json!({ "whole_tab": whole_tab, "dry_run": dry_run });
    match ids.as_slice() {
        [one] => params["id"] = json!(one),
        _ => params["ids"] = json!(ids),
    }
    let result = control.call("close", params)?;
    Ok(ToolOutput::ok(close_text(&result, dry_run), result))
}

fn close_text(result: &Value, dry_run: bool) -> String {
    let plan = result.get("plan").cloned().unwrap_or_default();
    let mut out = vec![if dry_run {
        "DRY RUN: nothing was closed. This is the plan:".to_owned()
    } else if result.get("ok").and_then(Value::as_bool) == Some(true) {
        "Closed. Steps run:".to_owned()
    } else {
        "Close finished with failures:".to_owned()
    }];
    let targets = plan
        .get("targets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    out.push(format!("Targets ({}):", targets.len()));
    for t in &targets {
        out.push(format!(
            "- {} `{}` ({})",
            t.get("label").and_then(Value::as_str).unwrap_or("?"),
            t.get("id").and_then(Value::as_str).unwrap_or("?"),
            t.get("kind").and_then(Value::as_str).unwrap_or("?"),
        ));
    }
    let outcomes = result.get("steps").and_then(Value::as_array);
    if let Some(steps) = plan.get("steps").and_then(Value::as_array) {
        out.push("Steps:".into());
        for (i, step) in steps.iter().enumerate() {
            let command = step.get("command").and_then(Value::as_str).unwrap_or("?");
            let outcome = outcomes.and_then(|o| o.get(i)).map_or(String::new(), |o| {
                let ok = o.get("ok").and_then(Value::as_bool) == Some(true);
                let note = o
                    .get("note")
                    .and_then(Value::as_str)
                    .map(|n| format!(" ({n})"))
                    .unwrap_or_default();
                format!(" -> {}{note}", if ok { "ok" } else { "FAILED" })
            });
            out.push(format!("- `{command}`{outcome}"));
        }
    }
    let warnings = strings(plan.get("warnings"));
    if !warnings.is_empty() {
        out.push("Warnings:".into());
        out.extend(warnings.iter().map(|w| format!("- {w}")));
    }
    if let Some(refused) = plan.get("refused").and_then(Value::as_str) {
        out.push(format!("Refused: {refused}"));
    }
    if dry_run {
        if plan.get("can_run").and_then(Value::as_bool) == Some(false) {
            out.push("This plan cannot run.".into());
        } else {
            out.push(
                "To really close, confirm with Karl, then call close_agents again with \
                 dry_run=false."
                    .into(),
            );
        }
    }
    out.join("\n")
}

fn broadcast(control: &mut dyn Control, p: &Params) -> ToolResult {
    let text = p.required_str("text")?;
    let runs = p.strings("runs")?.filter(|r| !r.is_empty());
    let all = p.bool("all")?.unwrap_or(false);
    let params = match (runs, all) {
        (Some(_), true) => return Err(Error::Args("give either runs or all, not both".into())),
        (None, false) => return Err(Error::Args("give runs (run ids) or all=true".into())),
        (Some(runs), false) => json!({ "text": text, "runs": runs }),
        (None, true) => json!({ "text": text, "all": true }),
    };
    let result = control.call("broadcast", params)?;
    let n = |k: &str| result.get(k).and_then(Value::as_u64).unwrap_or(0);
    let mut out = vec![format!(
        "Broadcast sent to {} coordinators ({} failed).",
        n("sent"),
        n("failed")
    )];
    for d in result
        .get("deliveries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let ok = d.get("ok").and_then(Value::as_bool) == Some(true);
        out.push(format!(
            "- {} for {}: {}",
            d.get("coordinator").and_then(Value::as_str).unwrap_or("?"),
            strings(d.get("runs")).join(", "),
            if ok {
                "delivered".to_owned()
            } else {
                d.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("failed")
                    .to_owned()
            }
        ));
    }
    let skipped = result.get("skipped").and_then(Value::as_array);
    if let Some(skipped) = skipped.filter(|s| !s.is_empty()) {
        out.push("Skipped:".into());
        for s in skipped {
            out.push(format!(
                "- {}: {}",
                s.get("run_id").and_then(Value::as_str).unwrap_or("?"),
                s.get("reason").and_then(Value::as_str).unwrap_or("?"),
            ));
        }
    }
    Ok(ToolOutput::ok(out.join("\n"), result))
}

fn whip_status(control: &mut dyn Control, p: &Params) -> ToolResult {
    let limit = p.usize("limit")?.unwrap_or(DEFAULT_WHIP_LOG);
    let result = control.call("whip", json!({ "limit": limit }))?;
    let w: WhipView = parse(&result)?;
    let now = now_ms();
    let mut out = vec![whip_line(&result, now)];
    out.push(format!(
        "Settings: notify {} · stall threshold {} · cooldown {} · max {} nudges · tick {} · \
         abandon after {}",
        if w.notify { "on" } else { "off" },
        format_duration(w.threshold_ms),
        format_duration(w.cooldown_ms),
        w.max_nudges,
        format_duration(w.tick_ms),
        format_duration(w.abandon_after_ms),
    ));
    if !w.escalated.is_empty() {
        out.push(format!("Escalated: {}", w.escalated.join(", ")));
    }
    if w.log.is_empty() {
        out.push("Log: empty".into());
    } else {
        out.push(format!(
            "Log (newest last, {} actions since start):",
            w.actions_logged
        ));
        out.extend(w.log.iter().map(|e| format!("- {}", whip_entry(e, now))));
    }
    Ok(ToolOutput::ok(out.join("\n"), result))
}

fn whip_entry(e: &WhipLogView, now: i64) -> String {
    format!(
        "{}{}: {} → {} ({}) = {}",
        since(now, e.at_ms),
        if e.dry_run { " [dry-run]" } else { "" },
        e.action,
        e.target,
        e.reason,
        e.outcome
    )
}

fn set_whip(control: &mut dyn Control, p: &Params) -> ToolResult {
    let mut params = Map::new();
    for key in ["enabled", "dry_run", "notify"] {
        if let Some(v) = p.bool(key)? {
            params.insert(key.into(), json!(v));
        }
    }
    if params.is_empty() {
        return Err(Error::Args(
            "give at least one of enabled, dry_run, notify".into(),
        ));
    }
    let result = control.call("whip_settings", Value::Object(params))?;
    let changed = result.get("changed").and_then(Value::as_bool) == Some(true);
    let on = |k: &str| {
        result
            .pointer(&format!("/settings/{k}"))
            .and_then(Value::as_bool)
    };
    let flag = |k: &str| match on(k) {
        Some(true) => "on",
        Some(false) => "off",
        None => "?",
    };
    let text = format!(
        "{} Whip: enabled {}, dry-run {}, notify {}.",
        if changed { "Saved." } else { "No change." },
        flag("enabled"),
        flag("dry_run"),
        flag("notify"),
    );
    Ok(ToolOutput::ok(text, result))
}

fn focus_agent(control: &mut dyn Control, p: &Params) -> ToolResult {
    let id = p.required_str("id")?;
    let raise = p.bool("raise_window")?.unwrap_or(false);
    let result = control.call("focus", json!({ "id": id, "raise": raise }))?;
    let selected = result.get("selected").and_then(Value::as_str).unwrap_or(id);
    let mut text = format!("Focused {selected} in the graph.");
    if result.get("hidden_by_filter").and_then(Value::as_bool) == Some(true) {
        text.push_str(
            " It is selected but hidden by the current filter (set_filter clear=true shows it).",
        );
    }
    Ok(ToolOutput::ok(text, result))
}

fn set_filter(control: &mut dyn Control, args: &Value) -> ToolResult {
    let mut params = Map::new();
    if let Some(obj) = args.as_object() {
        for key in [
            "clear",
            "repos",
            "states",
            "kinds",
            "text",
            "show_loose",
            "show_released",
        ] {
            if let Some(v) = obj.get(key).filter(|v| !v.is_null()) {
                params.insert(key.into(), v.clone());
            }
        }
    }
    let result = control.call("filter", Value::Object(params))?;
    let f = result.get("filter").cloned().unwrap_or_default();
    let list = |k: &str| {
        let v = strings(f.get(k));
        if v.is_empty() {
            "all".into()
        } else {
            v.join(", ")
        }
    };
    let text = format!(
        "Filter: repos {}, states {}, kinds {}, text {:?}, loose terminals {}, released {}. {} nodes hidden.",
        list("repos"),
        list("states"),
        list("kinds"),
        f.get("text").and_then(Value::as_str).unwrap_or(""),
        if f.get("show_loose").and_then(Value::as_bool) == Some(false) {
            "hidden"
        } else {
            "shown"
        },
        if f.get("show_released").and_then(Value::as_bool) == Some(false) {
            "hidden"
        } else {
            "shown"
        },
        f.get("hidden").and_then(Value::as_u64).unwrap_or(0),
    );
    Ok(ToolOutput::ok(text, result))
}

fn set_view(control: &mut dyn Control, p: &Params) -> ToolResult {
    let view = p.required_str("view")?;
    let result = control.call("view", json!({ "view": view }))?;
    let view = result.get("view").and_then(Value::as_str).unwrap_or(view);
    Ok(ToolOutput::ok(format!("Showing the {view} view."), result))
}

fn simple(control: &mut dyn Control, method: &str, text: &str) -> ToolResult {
    let result = control.call(method, Value::Null)?;
    Ok(ToolOutput::ok(text.into(), result))
}

fn alerts(control: &mut dyn Control, p: &Params) -> ToolResult {
    let limit = p.usize("limit")?.unwrap_or(DEFAULT_ALERTS).max(1);
    let result = control.call("alerts", Value::Null)?;
    let all: Vec<AlertView> = parse(result.get("alerts").unwrap_or(&Value::Null))?;
    let shown = &all[..all.len().min(limit)];
    let now = now_ms();
    let mut out = vec![if all.is_empty() {
        "No alerts.".to_owned()
    } else {
        format!(
            "{} alerts (newest first, showing {}):",
            all.len(),
            shown.len()
        )
    }];
    for a in shown {
        let node = a
            .node
            .as_deref()
            .map(|n| format!(" `{n}`"))
            .unwrap_or_default();
        out.push(format!("- {}: {}{node}", since(now, a.at_ms), a.title));
    }
    let structured = json!({ "count": all.len(), "alerts": shown });
    Ok(ToolOutput::ok(out.join("\n"), structured))
}

fn quotas(control: &mut dyn Control) -> ToolResult {
    let result = control.call("quotas", Value::Null)?;
    let q: QuotasView = parse(&result)?;
    let now = now_ms();
    let mut out = Vec::new();
    if q.providers.is_empty() {
        out.push(format!(
            "No quotas reported{}.",
            q.error
                .as_deref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        ));
    }
    for p in &q.providers {
        out.push(format!(
            "{}{}:",
            p.name,
            p.error
                .as_deref()
                .map(|e| format!(" (error: {e})"))
                .unwrap_or_default()
        ));
        for w in &p.windows {
            let reset = w
                .resets_at_ms
                .map(|t| format!(", {}", reset_text(t - now)))
                .or_else(|| w.reset_description.as_deref().map(|d| format!(", {d}")))
                .unwrap_or_default();
            let severity = if w.severity == "normal" {
                String::new()
            } else {
                format!(" [{}]", w.severity.to_uppercase())
            };
            out.push(format!(
                "- {} {:.0}% used{reset}{severity}",
                w.label, w.used_percent
            ));
        }
    }
    Ok(ToolOutput::ok(out.join("\n"), result))
}

// ---------------------------------------------------------------------------
// Formatting

/// "3m ago", or "just now".
fn since(now: i64, at_ms: i64) -> String {
    let ms = now - at_ms;
    if ms < 1_000 {
        "just now".into()
    } else {
        format!("{} ago", format_duration(ms))
    }
}

/// "resets in 2h 10m", or "reset passed" when the reset time is not in the future.
fn reset_text(in_ms: i64) -> String {
    if in_ms <= 0 {
        "reset passed".into()
    } else {
        format!("resets in {}", format_duration(in_ms))
    }
}

/// What a waiting agent's prompt is, e.g. "choice: 1. Yes | 2. No".
fn prompt_text(p: &PromptView) -> String {
    let kind = match p.kind.as_str() {
        "unknown" => "a prompt (seen in the preview only)".to_owned(),
        other => other.replace('_', " "),
    };
    if p.options.is_empty() {
        kind
    } else {
        format!("{kind}: {}", p.options.join(" | "))
    }
}

/// `"working 3, idle 2"` in `order`, then any other keys.
fn counts(map: &std::collections::BTreeMap<String, usize>, order: &[&str]) -> String {
    let mut parts: Vec<String> = order
        .iter()
        .filter_map(|k| map.get(*k).map(|n| format!("{} {n}", k.replace('_', " "))))
        .collect();
    parts.extend(
        map.iter()
            .filter(|(k, _)| !order.contains(&k.as_str()))
            .map(|(k, n)| format!("{k} {n}")),
    );
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(", ")
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// A screen row without the terminal's padding: runs of 8+ spaces become 4, runs of 40+ of any
/// other character (box-drawing rules) become 40, and the row is cut at [`SCREEN_LINE_CHARS`].
pub(crate) fn tidy_line(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.trim_end().chars().peekable();
    while let Some(c) = chars.next() {
        let mut run = 1;
        while chars.peek() == Some(&c) {
            chars.next();
            run += 1;
        }
        let keep = match c {
            ' ' if run >= 8 => 4,
            _ => run.min(40),
        };
        out.extend(std::iter::repeat_n(c, keep));
    }
    truncate(&out, SCREEN_LINE_CHARS)
}

/// Safe inside a Markdown table cell.
fn cell(s: &str) -> String {
    s.replace('|', "/").replace('\n', " ")
}

fn human_count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.2}B", n as f64 / 1e9),
    }
}

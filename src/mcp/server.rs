//! MCP over stdio: newline-delimited JSON-RPC 2.0 with `initialize`, `ping`, `tools/*`,
//! `prompts/*` and `resources/*`. Notifications (`notifications/initialized`, `notifications/cancelled`, …) are
//! accepted and ignored.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::mcp::backend::Control;
use crate::mcp::{playbook, review, tools};
use crate::playbook::Playbook;

/// Protocol versions this server speaks, newest first. `initialize` echoes the client's version
/// when it is one of these, and otherwise offers the newest.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// First version whose tool results carry `structuredContent`.
const STRUCTURED_SINCE: &str = "2025-06-18";

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

pub const INSTRUCTIONS: &str = "\
Myrstack is Karl's desktop dashboard for the agent fleet running in the Orca agent IDE. It draws \
every agent as a node in a live graph and runs the \"whip\", which nudges stalled workers. This \
server talks to the running app.

Node kinds: the Queen is the hub that stands for the whole fleet (not an agent; its state is the \
fleet state). A coordinator orchestrates a run (an objective split into tasks); a worker is an \
agent a coordinator dispatched for one task; a terminal is a loose agent terminal outside any \
run; a subagent is a Claude Code subagent inside a terminal (read-only: no input, no close).

States: working, idle, needs_input (waiting on Karl at a permission, choice or question \
prompt), stalled (the whip escalated it), done, failed.

Start with fleet_status. Agent ids (term:…, dispatch:…, sub:…) come from list_agents or \
fleet_status; a terminal handle or an exact label also works. send_to_agent, press_key, \
close_agents, broadcast_to_coordinators, set_whip and spawn_session act on real agents, so confirm \
with Karl first (the Queen in auto mode needn't: Karl approved her actions up front; nor does a \
coordinator starting workers on its own run with spawn_session). close_agents \
only shows the plan unless dry_run=false is passed. spawn_session \
starts a new session (existing worktree, new worktree, or a run's worker) with a prompt.

This server also serves the fleet playbook, the standing rules the fleet runs by, read from disk \
(no app needed): the prompts coordinator, attendant and worker (slash commands such as \
/mcp__myrstack__coordinator) load a role's rules; get_playbook and list_playbook fetch a role, a \
file or one section; resources myrstack://playbook/<file> hold each file. attendant_brief \
{run_id} and kickoff {task_id} (also prompts attendant-brief and kickoff) return those templates \
pre-filled from live fleet data. Every answer starts with the playbook version (git commit, dirty \
flag, content hash).

Review queue: an agent whose PR is ready for Karl's review calls mark_pr_ready {pr_url} (right \
after merge_ready); it only records in Myrstack, attributed to the agent's Orca terminal. \
review_queue lists what is waiting, with CI, Ankra and ready-marker state.";

/// The MCP session state over one [`Control`].
pub struct McpServer<C: Control> {
    control: C,
    /// Negotiated in `initialize`.
    protocol: Option<String>,
    /// Where the playbook is read from; located per request when `None`.
    playbook_dir: Option<PathBuf>,
    /// Where playbook fetches are logged; `None` logs nothing.
    fetch_log: Option<PathBuf>,
    /// The caller's identity and files for the review queue tools.
    review: review::ReviewEnv,
}

impl<C: Control> McpServer<C> {
    pub fn new(control: C) -> Self {
        Self {
            control,
            protocol: None,
            playbook_dir: None,
            fetch_log: crate::playbook::fetches::default_path(),
            review: review::ReviewEnv::from_env(),
        }
    }

    /// Uses `env` (caller identity, pending and queue files) for the review queue tools.
    pub fn with_review_env(mut self, env: review::ReviewEnv) -> Self {
        self.review = env;
        self
    }

    /// Logs playbook fetches to `path` instead of the data dir's log.
    pub fn with_fetch_log(mut self, path: impl Into<PathBuf>) -> Self {
        self.fetch_log = Some(path.into());
        self
    }

    /// Serves the playbook from `dir` instead of locating it.
    pub fn with_playbook_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.playbook_dir = Some(dir.into());
        self
    }

    fn playbook(&self) -> Result<Playbook, String> {
        playbook_at(self.playbook_dir.as_deref())
    }

    pub fn control(&mut self) -> &mut C {
        &mut self.control
    }

    /// Answers messages line by line until `input` ends.
    pub fn serve(&mut self, mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if input.read_until(b'\n', &mut buf)? == 0 {
                return Ok(());
            }
            let reply = match std::str::from_utf8(&buf) {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => self.handle_line(line),
                Err(_) => Some(error(Value::Null, PARSE_ERROR, "message is not UTF-8")),
            };
            if let Some(reply) = reply {
                serde_json::to_writer(&mut output, &reply)?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
    }

    /// Answers one message; `None` for notifications and client responses.
    pub fn handle_line(&mut self, line: &str) -> Option<Value> {
        match serde_json::from_str::<Value>(line) {
            Ok(message) => self.handle(message),
            Err(e) => Some(error(
                Value::Null,
                PARSE_ERROR,
                &format!("invalid JSON: {e}"),
            )),
        }
    }

    pub fn handle(&mut self, message: Value) -> Option<Value> {
        let Value::Object(obj) = message else {
            let what = if message.is_array() {
                "batches are not supported"
            } else {
                "message must be a JSON object"
            };
            return Some(error(Value::Null, INVALID_REQUEST, what));
        };
        let id = obj.get("id").cloned();
        if let Some(id) = &id
            && !(id.is_string() || id.is_i64() || id.is_u64())
        {
            return Some(error(
                Value::Null,
                INVALID_REQUEST,
                "id must be a string or an integer",
            ));
        }
        let reply_id = id.clone().unwrap_or(Value::Null);
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error(reply_id, INVALID_REQUEST, "jsonrpc must be \"2.0\""));
        }
        let Some(method) = obj.get("method") else {
            // A response to a request we never send.
            if obj.contains_key("result") || obj.contains_key("error") {
                return None;
            }
            return Some(error(reply_id, INVALID_REQUEST, "missing method"));
        };
        let Some(method) = method.as_str() else {
            return Some(error(reply_id, INVALID_REQUEST, "method must be a string"));
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        if !(params.is_null() || params.is_object()) {
            return id.map(|id| error(id, INVALID_PARAMS, "params must be an object"));
        }
        let Some(id) = id else {
            log::debug!("notification {method}");
            return None;
        };
        Some(match self.request(method, &params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => error(id, code, &message),
        })
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => Ok(self.initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let mut all = tools::definitions();
                all.extend(playbook::tool_definitions());
                all.extend(review::definitions());
                Ok(json!({ "tools": all }))
            }
            "tools/call" => self.call_tool(params),
            "prompts/list" => Ok(playbook::prompts_list()),
            "prompts/get" => {
                let pb = self.playbook();
                playbook::prompts_get(pb, &mut self.control, params, self.fetch_log.as_deref())
            }
            "resources/list" => playbook::resources_list(self.playbook()),
            "resources/read" => {
                playbook::resources_read(self.playbook(), params, self.fetch_log.as_deref())
            }
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        }
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let version = negotiate(requested);
        log::info!("initialize: client wants {requested:?}, using {version}");
        self.protocol = Some(version.to_owned());
        json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": { "listChanged": false },
                "prompts": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
            },
            "serverInfo": {
                "name": "myrstack",
                "title": "Myrstack",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": INSTRUCTIONS,
        })
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((INVALID_PARAMS, "missing tool name".to_owned()))?;
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        if !(args.is_null() || args.is_object()) {
            return Err((INVALID_PARAMS, "arguments must be an object".into()));
        }
        let log = self.fetch_log.clone();
        let output = playbook::call_tool(|| self.playbook(), name, &args, log.as_deref())
            .or_else(|| {
                let dir = self.playbook_dir.clone();
                playbook::call_template_tool(
                    &mut self.control,
                    || playbook_at(dir.as_deref()),
                    name,
                    &args,
                    log.as_deref(),
                )
            })
            .or_else(|| review::call(&mut self.control, &self.review, name, &args))
            .or_else(|| tools::call(&mut self.control, name, &args))
            .ok_or_else(|| (INVALID_PARAMS, format!("unknown tool: {name}")))?;
        let mut result = json!({
            "content": [{ "type": "text", "text": output.text }],
            "isError": output.is_error,
        });
        if self.structured()
            && let Some(structured) = output.structured
        {
            result["structuredContent"] = structured;
        }
        Ok(result)
    }

    /// Whether the negotiated version has `structuredContent` (assumed when not initialized).
    fn structured(&self) -> bool {
        self.protocol
            .as_deref()
            .is_none_or(|v| v >= STRUCTURED_SINCE)
    }
}

/// The playbook in `dir`, else wherever [`Playbook::locate`] finds it.
fn playbook_at(dir: Option<&std::path::Path>) -> Result<Playbook, String> {
    match dir {
        Some(dir) => Ok(Playbook::at(dir)),
        None => Playbook::locate().map_err(|e| format!("{e:#}")),
    }
}

/// The version to answer `initialize` with.
pub fn negotiate(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| PROTOCOL_VERSIONS.iter().find(|v| **v == r))
        .copied()
        .unwrap_or(PROTOCOL_VERSIONS[0])
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

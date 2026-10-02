use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::sync::Arc;

use serde_json::{Value, json};

use super::backend::HeadlessControl;
use super::server::{INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR};
use super::*;
use crate::app::View;
use crate::control::server::{Backend, ScreenRead, ScreenReader};
use crate::control::view::{StatusView, ViewInputs};
use crate::control::{
    CommandHandler, ControlCommand, ControlError, ControlServer, ErrorCode, SharedStatus,
};
use crate::detect::PromptKind;
use crate::fleet::FleetData;
use crate::model::FleetSnapshot;
use crate::orca::commands::{parse_run_list, parse_terminal_list, parse_worker_list};
use crate::quota::Quotas;
use crate::ui::sidebar::FleetFilter;
use crate::whip::{WhipSettings, WhipStatus};

const COORDINATOR: &str = "term_70c8df0b-cf78-4b43-a6f8-aefab63bca21";
const CODEX: &str = "term_1744da75-821f-46fe-912d-e252e0bbf3fb";
const RUN: &str = "run_7c25cdcf46e0";
const NOW: i64 = 1_790_319_595_528 + 10_000;

// ---------------------------------------------------------------------------
// Fakes

/// Records every call; answers from `replies` by method (default `{"ok": "<method>"}`).
#[derive(Default)]
struct FakeControl {
    calls: Vec<(String, Value)>,
    replies: BTreeMap<String, Result<Value, CallError>>,
    fallback: Option<HeadlessControl>,
    fallback_used: bool,
}

impl FakeControl {
    fn reply(mut self, method: &str, reply: Result<Value, CallError>) -> Self {
        self.replies.insert(method.into(), reply);
        self
    }
}

impl Control for FakeControl {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
        self.calls.push((method.into(), params));
        self.replies
            .get(method)
            .cloned()
            .unwrap_or_else(|| Ok(json!({ "ok": method })))
    }

    fn fallback(&mut self) -> Result<&mut dyn Control, String> {
        self.fallback_used = true;
        match &mut self.fallback {
            Some(f) => Ok(f),
            None => Err("none".into()),
        }
    }
}

/// Every screen shows the same two rows.
struct Screens;

impl ScreenReader for Screens {
    fn read_screen(&self, _: &str) -> Result<ScreenRead, String> {
        Ok(ScreenRead {
            lines: vec!["● Working on it".into(), "❯ ".into(), String::new()],
            status: Some("running".into()),
        })
    }
}

struct Refuse;

impl CommandHandler for Refuse {
    fn execute(&self, command: ControlCommand) -> Result<Value, ControlError> {
        match command {
            ControlCommand::SetView(_) => Ok(json!({"view": "table"})),
            _ => Err(ControlError::not_found("no agent \"missing\"")),
        }
    }
}

fn fleet() -> FleetData {
    let terminals = parse_terminal_list(include_bytes!(
        "../../tests/fixtures/orca/terminal_list.json"
    ))
    .unwrap()
    .terminals;
    let mut runs =
        parse_run_list(include_bytes!("../../tests/fixtures/orca/run_list.json")).unwrap();
    runs[0].coordinator_handle = Some(COORDINATOR.into());
    let mut worker =
        parse_worker_list(include_bytes!("../../tests/fixtures/orca/worker_list.json"))
            .unwrap()
            .workers
            .into_iter()
            .find(|w| w.dispatch_id == "ctx_a4a62d1e6146")
            .unwrap();
    worker.run_id = Some(RUN.into());
    worker.agent_terminal_handle = Some(CODEX.into());
    FleetData {
        polled_at_ms: NOW,
        terminals,
        runs,
        workers: vec![worker],
        needs_input: BTreeMap::from([(
            CODEX.to_string(),
            Some(PromptKind::Choice {
                options: vec!["1. Yes".into(), "2. No".into()],
            }),
        )]),
        ..FleetData::default()
    }
}

fn status_view() -> StatusView {
    let data = fleet();
    let snapshot = FleetSnapshot::build(&data, NOW);
    StatusView::build(&ViewInputs {
        snapshot: Some(&snapshot),
        data: Some(&data),
        claude: None,
        whip_status: &WhipStatus {
            enabled: true,
            dry_run: true,
            findings: 1,
            escalated: BTreeSet::new(),
            last_tick_ms: Some(NOW),
            ..WhipStatus::default()
        },
        whip_settings: &WhipSettings::default(),
        whip_log: Vec::new(),
        quotas: &Quotas::default(),
        alerts: &[],
        view: View::Graph,
        selected: None,
        selection: Vec::new(),
        filter: &FleetFilter::default(),
        whip_settings_open: false,
        now_ms: NOW,
    })
}

fn app() -> HeadlessControl {
    HeadlessControl::from_view(status_view(), Arc::new(Screens))
}

// ---------------------------------------------------------------------------
// Helpers

fn rpc(server: &mut McpServer<impl Control>, method: &str, params: Value) -> Value {
    server
        .handle(json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .expect("a request gets a reply")
}

fn call(server: &mut McpServer<impl Control>, tool: &str, args: Value) -> Value {
    let reply = rpc(
        server,
        "tools/call",
        json!({"name": tool, "arguments": args}),
    );
    reply
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("tools/call {tool} failed: {reply}"))
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

fn is_error(result: &Value) -> bool {
    result["isError"].as_bool().unwrap()
}

// ---------------------------------------------------------------------------
// Protocol

#[test]
fn initialize_echoes_a_supported_version() {
    let mut server = McpServer::new(FakeControl::default());
    let r = rpc(
        &mut server,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    );
    let result = &r["result"];
    assert_eq!(r["jsonrpc"], "2.0");
    assert_eq!(r["id"], 1);
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(result["serverInfo"]["name"], "myrstack");
    assert!(result["capabilities"]["tools"].is_object());
    let instructions = result["instructions"].as_str().unwrap();
    for word in [
        "Queen",
        "coordinator",
        "worker",
        "subagent",
        "needs_input",
        "list_agents",
        "An ant is one dedicated agent session",
    ] {
        assert!(instructions.contains(word), "instructions mention {word}");
    }
}

#[test]
fn initialize_offers_the_newest_version_for_an_unknown_one() {
    for requested in [json!("1999-01-01"), json!("2099-01-01"), Value::Null] {
        let mut server = McpServer::new(FakeControl::default());
        let r = rpc(
            &mut server,
            "initialize",
            json!({"protocolVersion": requested}),
        );
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }
}

#[test]
fn old_protocol_versions_get_no_structured_content() {
    let mut server = McpServer::new(FakeControl::default());
    rpc(
        &mut server,
        "initialize",
        json!({"protocolVersion": "2025-03-26"}),
    );
    let r = call(&mut server, "fit_graph", json!({}));
    assert!(r.get("structuredContent").is_none());

    let mut server = McpServer::new(FakeControl::default());
    rpc(
        &mut server,
        "initialize",
        json!({"protocolVersion": "2025-11-25"}),
    );
    let r = call(&mut server, "fit_graph", json!({}));
    assert_eq!(r["structuredContent"], json!({"ok": "fit"}));
}

#[test]
fn notifications_and_client_responses_get_no_reply() {
    let mut server = McpServer::new(FakeControl::default());
    for message in [
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 3}}),
        json!({"jsonrpc": "2.0", "id": 9, "result": {}}),
    ] {
        assert_eq!(server.handle(message), None);
    }
}

#[test]
fn ping_answers_empty() {
    let mut server = McpServer::new(FakeControl::default());
    assert_eq!(rpc(&mut server, "ping", Value::Null)["result"], json!({}));
}

fn error_code(reply: &Value) -> i64 {
    reply["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("no error in {reply}"))
}

#[test]
fn malformed_messages_get_json_rpc_errors() {
    let mut server = McpServer::new(FakeControl::default());
    let r = server.handle_line("{not json").unwrap();
    assert_eq!(error_code(&r), PARSE_ERROR);
    assert_eq!(r["id"], Value::Null);

    let cases = [
        ("[1,2]", INVALID_REQUEST),
        ("42", INVALID_REQUEST),
        (r#"{"id":1,"method":"ping"}"#, INVALID_REQUEST),
        (
            r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
            INVALID_REQUEST,
        ),
        (r#"{"jsonrpc":"2.0","id":1}"#, INVALID_REQUEST),
        (r#"{"jsonrpc":"2.0","id":1,"method":7}"#, INVALID_REQUEST),
        (
            r#"{"jsonrpc":"2.0","id":{"a":1},"method":"ping"}"#,
            INVALID_REQUEST,
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":[1]}"#,
            INVALID_PARAMS,
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"sampling/createMessage"}"#,
            METHOD_NOT_FOUND,
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#,
            INVALID_PARAMS,
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"quotas","arguments":[1]}}"#,
            INVALID_PARAMS,
        ),
    ];
    for (line, code) in cases {
        let r = server
            .handle_line(line)
            .unwrap_or_else(|| panic!("reply to {line}"));
        assert_eq!(error_code(&r), code, "{line} -> {r}");
        assert_eq!(r["jsonrpc"], "2.0");
    }
    // The id is echoed when it can be.
    let r = server
        .handle_line(r#"{"jsonrpc":"2.0","id":"x","method":"nope"}"#)
        .unwrap();
    assert_eq!(r["id"], "x");
}

#[test]
fn unknown_tool_is_an_invalid_params_error() {
    let mut server = McpServer::new(FakeControl::default());
    let r = rpc(
        &mut server,
        "tools/call",
        json!({"name": "rm_rf", "arguments": {}}),
    );
    assert_eq!(error_code(&r), INVALID_PARAMS);
    assert!(r["error"]["message"].as_str().unwrap().contains("rm_rf"));
    assert!(server.control().calls.is_empty());
}

#[test]
fn serve_answers_each_request_line_over_pipes() {
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "garbage",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"set_view","arguments":{"view":"table"}}}"#,
    ]
    .join("\n");
    let mut out = Vec::new();
    let mut server = McpServer::new(FakeControl::default());
    server
        .serve(Cursor::new(input.into_bytes()), &mut out)
        .unwrap();
    let replies: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 4);
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(error_code(&replies[2]), PARSE_ERROR);
    assert_eq!(replies[3]["id"], 3);
    assert_eq!(
        server.control().calls,
        vec![("view".to_string(), json!({"view": "table"}))]
    );
}

#[test]
fn serve_rejects_non_utf8() {
    let mut out = Vec::new();
    let mut server = McpServer::new(FakeControl::default());
    server
        .serve(Cursor::new(b"\xff\xfe\n".to_vec()), &mut out)
        .unwrap();
    let reply: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(error_code(&reply), PARSE_ERROR);
}

// ---------------------------------------------------------------------------
// tools/list

/// A structural JSON Schema check: object schemas with typed properties.
fn check_schema(name: &str, schema: &Value) {
    assert_eq!(schema["type"], "object", "{name}");
    let props = schema["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("{name} properties"));
    for (key, prop) in props {
        let typed = prop.get("type").is_some_and(|t| {
            ["string", "boolean", "integer", "array", "object", "number"]
                .contains(&t.as_str().unwrap_or(""))
        });
        let any_of = prop
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|alts| alts.iter().all(|a| a.get("type").is_some()));
        assert!(typed || any_of, "{name}.{key} has a type");
        if prop["type"] == "array" {
            assert!(prop["items"].is_object(), "{name}.{key} items");
        }
        if let Some(values) = prop.get("enum") {
            assert!(values.as_array().is_some_and(|v| !v.is_empty()));
        }
    }
    for required in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let key = required.as_str().unwrap();
        assert!(
            props.contains_key(key),
            "{name}: required {key} is a property"
        );
    }
}

#[test]
fn tools_list_has_every_tool_with_schemas_and_annotations() {
    let mut server = McpServer::new(FakeControl::default());
    let r = rpc(&mut server, "tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let expected = [
        "fleet_status",
        "list_agents",
        "get_agent",
        "send_to_agent",
        "press_key",
        "close_agents",
        "broadcast_to_coordinators",
        "whip_status",
        "set_whip",
        "whip_agents",
        "spawn_session",
        "focus_agent",
        "set_filter",
        "set_view",
        "fit_graph",
        "open_whip_settings",
        "alerts",
        "quotas",
        "get_playbook",
        "list_playbook",
        "attendant_brief",
        "kickoff",
        "mark_pr_ready",
        "review_queue",
        "notify",
        "clean_worktrees",
    ];
    assert_eq!(names, expected);
    let reads = [
        "fleet_status",
        "list_agents",
        "get_agent",
        "whip_status",
        "alerts",
        "quotas",
        "get_playbook",
        "list_playbook",
        "attendant_brief",
        "kickoff",
        "review_queue",
    ];
    let destructive = [
        "send_to_agent",
        "press_key",
        "close_agents",
        "broadcast_to_coordinators",
        "set_whip",
        "whip_agents",
        "spawn_session",
        "clean_worktrees",
    ];
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        assert!(!tool["description"].as_str().unwrap().is_empty());
        check_schema(name, &tool["inputSchema"]);
        let a = &tool["annotations"];
        assert_eq!(
            a["readOnlyHint"],
            reads.contains(&name),
            "{name} readOnlyHint"
        );
        assert!(a["idempotentHint"].is_boolean(), "{name} idempotentHint");
        if !reads.contains(&name) {
            assert_eq!(
                a["destructiveHint"],
                destructive.contains(&name),
                "{name} destructiveHint"
            );
        }
    }
    let spawn = tools.iter().find(|t| t["name"] == "spawn_session").unwrap();
    assert_eq!(spawn["annotations"]["title"], "Start a new ant");
    assert!(
        spawn["description"]
            .as_str()
            .unwrap()
            .contains("make a new ant for X")
    );
    let close = tools.iter().find(|t| t["name"] == "close_agents").unwrap();
    assert_eq!(
        close["inputSchema"]["properties"]["dry_run"]["default"],
        true
    );
    assert!(
        close["description"]
            .as_str()
            .unwrap()
            .contains("dry_run=false")
    );
}

// ---------------------------------------------------------------------------
// tools/call → control requests

/// Tool, arguments, and the control calls expected.
type Case = (&'static str, Value, Vec<(&'static str, Value)>);

#[test]
fn each_tool_maps_to_its_control_request() {
    let cases: Vec<Case> = vec![
        (
            "get_agent",
            json!({"id": "term:a", "lines": 5}),
            vec![("agent", json!({"id": "term:a", "lines": 5}))],
        ),
        (
            "send_to_agent",
            json!({"id": "term:a", "text": "continue"}),
            vec![(
                "send",
                json!({"id": "term:a", "text": "continue", "enter": true}),
            )],
        ),
        (
            "send_to_agent",
            json!({"id": "term:a", "text": "draft", "submit": false}),
            vec![(
                "send",
                json!({"id": "term:a", "text": "draft", "enter": false}),
            )],
        ),
        (
            "press_key",
            json!({"id": "term:a", "key": "enter"}),
            vec![("enter", json!({"id": "term:a"}))],
        ),
        (
            "press_key",
            json!({"id": "term:a", "key": "esc"}),
            vec![("esc", json!({"id": "term:a"}))],
        ),
        (
            "press_key",
            json!({"id": "term:a", "key": "interrupt"}),
            vec![("interrupt", json!({"id": "term:a"}))],
        ),
        (
            "close_agents",
            json!({"ids": ["term:a", "term:b"], "whole_tab": true, "dry_run": false}),
            vec![(
                "close",
                json!({"ids": ["term:a", "term:b"], "whole_tab": true, "dry_run": false}),
            )],
        ),
        (
            "broadcast_to_coordinators",
            json!({"text": "wrap up", "runs": ["run_1"]}),
            vec![("broadcast", json!({"text": "wrap up", "runs": ["run_1"]}))],
        ),
        (
            "broadcast_to_coordinators",
            json!({"text": "wrap up", "all": true}),
            vec![("broadcast", json!({"text": "wrap up", "all": true}))],
        ),
        (
            "whip_status",
            json!({}),
            vec![("whip", json!({"limit": 10}))],
        ),
        (
            "set_whip",
            json!({"enabled": false, "notify": true}),
            vec![("whip_settings", json!({"enabled": false, "notify": true}))],
        ),
        (
            "whip_agents",
            json!({"ids": ["term:a", "term:b"]}),
            vec![("whip", json!({"ids": ["term:a", "term:b"]}))],
        ),
        (
            "whip_agents",
            json!({"stalled": true}),
            vec![("whip", json!({"stalled": true}))],
        ),
        (
            "spawn_session",
            json!({"mode": "worktree", "worktree": "depict/main", "prompt": " Fix it ", "bogus": 1}),
            vec![(
                "spawn",
                json!({"mode": "worktree", "worktree": "depict/main", "prompt": "Fix it", "agent": "claude"}),
            )],
        ),
        (
            "spawn_session",
            json!({"mode": "worker", "run_id": "run_1", "repo": "depict", "name": "w1",
                   "prompt": "spec", "agent": "codex", "model": "gpt-6-astra"}),
            vec![(
                "spawn",
                json!({"mode": "worker", "run_id": "run_1", "repo": "depict", "name": "w1",
                       "prompt": "spec", "agent": "codex", "model": "gpt-6-astra"}),
            )],
        ),
        (
            "focus_agent",
            json!({"id": "queen", "raise_window": true}),
            vec![("focus", json!({"id": "queen", "raise": true}))],
        ),
        (
            "set_filter",
            json!({"clear": true, "states": ["needs_input"], "show_loose": false, "bogus": 1}),
            vec![(
                "filter",
                json!({"clear": true, "states": ["needs_input"], "show_loose": false}),
            )],
        ),
        (
            "set_view",
            json!({"view": "graph"}),
            vec![("view", json!({"view": "graph"}))],
        ),
        ("fit_graph", json!({}), vec![("fit", Value::Null)]),
        (
            "open_whip_settings",
            Value::Null,
            vec![("open_whip_settings", Value::Null)],
        ),
        ("alerts", json!({}), vec![("alerts", Value::Null)]),
        ("quotas", json!({}), vec![("quotas", Value::Null)]),
    ];
    for (tool, args, expected) in cases {
        let control = FakeControl::default()
            .reply(
                "whip",
                Ok(serde_json::to_value(status_view().whip).unwrap()),
            )
            .reply("alerts", Ok(json!({"alerts": []})))
            .reply(
                "quotas",
                Ok(json!({"fetched_at_ms": 0, "error": null, "providers": []})),
            )
            .reply(
                "agent",
                Ok(
                    json!({"agent": serde_json::to_value(&status_view().agents[0]).unwrap(),
                          "children": [], "screen": null}),
                ),
            );
        let mut server = McpServer::new(control);
        let r = call(&mut server, tool, args.clone());
        assert!(!is_error(&r), "{tool} {args}: {}", text(&r));
        let expected: Vec<(String, Value)> = expected
            .into_iter()
            .map(|(m, p)| (m.to_string(), p))
            .collect();
        assert_eq!(server.control().calls, expected, "{tool} {args}");
    }
}

#[test]
fn close_defaults_to_a_dry_run() {
    let plan = json!({"dry_run": true, "plan": {
        "targets": [{"id": "term:a", "label": "Worker A", "kind": "worker"}],
        "steps": [{"command": "orca terminal close --terminal=term_a", "label": "Close"}],
        "warnings": ["run X loses a worker"], "refused": null, "can_run": true}});
    let mut server = McpServer::new(FakeControl::default().reply("close", Ok(plan)));
    let r = call(&mut server, "close_agents", json!({"ids": ["term:a"]}));
    assert_eq!(
        server.control().calls,
        vec![(
            "close".to_string(),
            json!({"id": "term:a", "whole_tab": false, "dry_run": true})
        )]
    );
    let t = text(&r);
    assert!(t.starts_with("DRY RUN"), "{t}");
    assert!(t.contains("Worker A"));
    assert!(t.contains("orca terminal close --terminal=term_a"));
    assert!(t.contains("run X loses a worker"));
    assert!(t.contains("dry_run=false"));
}

#[test]
fn invalid_arguments_are_tool_errors_and_call_nothing() {
    let cases = [
        ("get_agent", json!({})),
        ("send_to_agent", json!({"id": "term:a"})),
        (
            "send_to_agent",
            json!({"id": "term:a", "text": "", "submit": false}),
        ),
        ("press_key", json!({"id": "term:a", "key": "tab"})),
        ("close_agents", json!({"ids": []})),
        ("whip_agents", json!({})),
        ("whip_agents", json!({"ids": []})),
        ("whip_agents", json!({"ids": ["a"], "stalled": true})),
        ("whip_agents", json!({"stalled": false})),
        ("close_agents", json!({"ids": ["a"], "dry_run": "no"})),
        ("broadcast_to_coordinators", json!({"text": "x"})),
        (
            "broadcast_to_coordinators",
            json!({"text": "x", "runs": ["r"], "all": true}),
        ),
        ("set_whip", json!({})),
        ("list_agents", json!({"live": "yes"})),
        ("spawn_session", json!({"prompt": "x"})),
        ("spawn_session", json!({"mode": "later", "prompt": "x"})),
        ("spawn_session", json!({"mode": "worktree", "prompt": "x"})),
        (
            "spawn_session",
            json!({"mode": "new_worktree", "repo": "r", "prompt": "x"}),
        ),
        (
            "spawn_session",
            json!({"mode": "worker", "run_id": "r", "prompt": "x"}),
        ),
        (
            "spawn_session",
            json!({"mode": "worker", "run_id": "r", "worktree": "w", "terminal": "t", "prompt": "x"}),
        ),
        (
            "spawn_session",
            json!({"mode": "worker", "run_id": "r", "worktree": "w"}),
        ),
        (
            "spawn_session",
            json!({"mode": "worktree", "worktree": "w", "agent": "gemini"}),
        ),
    ];
    for (tool, args) in cases {
        let mut server = McpServer::new(FakeControl::default());
        let r = call(&mut server, tool, args.clone());
        assert!(is_error(&r), "{tool} {args}");
        assert!(
            text(&r).starts_with("Invalid arguments"),
            "{tool}: {}",
            text(&r)
        );
        assert!(server.control().calls.is_empty(), "{tool} {args}");
    }
}

// ---------------------------------------------------------------------------
// Error mapping

#[test]
fn app_not_running_is_a_clear_tool_error() {
    for tool in [
        "get_agent",
        "send_to_agent",
        "close_agents",
        "set_view",
        "quotas",
    ] {
        let control = FakeControl {
            replies: ["agent", "send", "close", "view", "quotas"]
                .into_iter()
                .map(|m| (m.to_string(), Err(CallError::NotRunning)))
                .collect(),
            ..FakeControl::default()
        };
        let mut server = McpServer::new(control);
        let r = call(
            &mut server,
            tool,
            json!({"id": "a", "text": "x", "ids": ["a"], "view": "graph"}),
        );
        assert!(is_error(&r));
        assert_eq!(
            text(&r),
            "Myrstack isn't running — start it with `myrstack`."
        );
        // Only the read tools that have a fallback try it.
        assert!(!server.control().fallback_used, "{tool}");
    }
}

#[test]
fn control_errors_map_to_readable_tool_errors() {
    let cases = [
        (
            ErrorCode::NotFound,
            "Not found: no agent \"x\". Use list_agents",
        ),
        (ErrorCode::Refused, "Refused: no agent \"x\""),
        (ErrorCode::Unavailable, "Myrstack isn't ready"),
        (ErrorCode::Failed, "Myrstack error: failed: no agent"),
    ];
    for (code, want) in cases {
        let control = FakeControl::default().reply(
            "focus",
            Err(CallError::Control(ControlError::new(
                code,
                "no agent \"x\"",
            ))),
        );
        let mut server = McpServer::new(control);
        let r = call(&mut server, "focus_agent", json!({"id": "x"}));
        assert!(is_error(&r));
        assert!(text(&r).starts_with(want), "{code:?}: {}", text(&r));
    }
    let control =
        FakeControl::default().reply("focus", Err(CallError::Transport("broken pipe".into())));
    let mut server = McpServer::new(control);
    let r = call(&mut server, "focus_agent", json!({"id": "x"}));
    assert_eq!(text(&r), "Couldn't talk to Myrstack: broken pipe");
}

// ---------------------------------------------------------------------------
// Read tools against real control responses

#[test]
fn fleet_status_summarises_the_fleet() {
    let mut server = McpServer::new(app());
    let r = call(&mut server, "fleet_status", json!({}));
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(t.starts_with("Fleet: needs input · "), "{t}");
    assert!(t.contains("Needs input (1):"), "{t}");
    assert!(t.contains(&format!("`term:{CODEX}`")), "{t}");
    assert!(t.contains("choice: 1. Yes | 2. No"), "{t}");
    assert!(t.contains("| Run | Objective | Coordinator |"), "{t}");
    assert!(t.contains(RUN), "{t}");
    assert!(t.contains("NEEDS INPUT"), "{t}");
    assert!(t.contains("Whip: on, dry-run · 1 stall findings"), "{t}");
    assert!(!t.contains("Note: Myrstack isn't running"));
    let s = &r["structuredContent"];
    assert_eq!(s["fallback"], false);
    assert_eq!(s["needs_input"][0]["id"], format!("term:{CODEX}"));
    assert!(s["fleet"]["runs"].is_array());
}

#[test]
fn fleet_status_and_list_agents_fall_back_when_the_app_is_down() {
    for tool in ["fleet_status", "list_agents"] {
        let mut control = FakeControl::default()
            .reply("status", Err(CallError::NotRunning))
            .reply("agents", Err(CallError::NotRunning));
        control.fallback = Some(app());
        let mut server = McpServer::new(control);
        let r = call(&mut server, tool, json!({}));
        assert!(!is_error(&r), "{}", text(&r));
        assert!(
            text(&r).starts_with("Note: Myrstack isn't running"),
            "{}",
            text(&r)
        );
        assert_eq!(r["structuredContent"]["fallback"], true);
        assert!(server.control().fallback_used);
    }
    // No fallback available: the plain error.
    let control = FakeControl::default().reply("status", Err(CallError::NotRunning));
    let mut server = McpServer::new(control);
    let r = call(&mut server, "fleet_status", json!({}));
    assert!(is_error(&r));
    assert!(text(&r).starts_with("Myrstack isn't running"));
}

#[test]
fn list_agents_filters_and_limits() {
    let mut server = McpServer::new(app());
    let r = call(&mut server, "list_agents", json!({"kind": "coordinator"}));
    let t = text(&r);
    assert!(t.contains(&format!("term:{COORDINATOR}")), "{t}");
    assert!(!t.contains(&format!("term:{CODEX}")), "{t}");
    assert!(
        r["structuredContent"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["kind"] == "coordinator")
    );

    let r = call(&mut server, "list_agents", json!({"limit": 1}));
    assert_eq!(r["structuredContent"]["shown"], 1);
    assert!(r["structuredContent"]["count"].as_u64().unwrap() > 1);
    assert!(text(&r).contains("raise limit"));

    let r = call(&mut server, "list_agents", json!({"state": "bogus"}));
    assert!(is_error(&r));
    assert!(text(&r).contains("unknown state"));
}

#[test]
fn get_agent_shows_details_and_screen() {
    let mut server = McpServer::new(app());
    let r = call(&mut server, "get_agent", json!({"id": CODEX, "lines": 5}));
    let t = text(&r);
    assert!(t.contains(&format!("id `term:{CODEX}`")), "{t}");
    assert!(t.contains("Needs input: choice: 1. Yes | 2. No"), "{t}");
    assert!(t.contains("```\n● Working on it\n❯\n```"), "{t}");

    let r = call(&mut server, "get_agent", json!({"id": "nobody"}));
    assert!(is_error(&r));
    assert!(text(&r).starts_with("Not found"));
}

#[test]
fn headless_refuses_actions() {
    let mut server = McpServer::new(app());
    let r = call(
        &mut server,
        "send_to_agent",
        json!({"id": CODEX, "text": "hi"}),
    );
    assert!(is_error(&r));
    assert!(text(&r).contains("isn't running"), "{}", text(&r));
}

// ---------------------------------------------------------------------------
// Real socket

#[test]
fn socket_control_talks_to_the_control_server() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sock");
    let status = SharedStatus::new();
    status.publish(status_view());
    let server = ControlServer::start(
        &path,
        Backend {
            status,
            screens: Arc::new(Screens),
            commands: Arc::new(Refuse),
        },
    )
    .unwrap();
    let mut mcp = McpServer::new(SocketControl::new(path.clone()));
    let r = call(&mut mcp, "fleet_status", json!({}));
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).starts_with("Fleet: "));
    let r = call(&mut mcp, "set_view", json!({"view": "table"}));
    assert_eq!(text(&r), "Showing the table view.");
    let r = call(&mut mcp, "focus_agent", json!({"id": "missing"}));
    assert!(text(&r).starts_with("Not found"), "{}", text(&r));
    drop(server);

    let mut control = SocketControl::new(dir.path().join("nobody.sock"));
    assert_eq!(
        control.call("status", Value::Null),
        Err(CallError::NotRunning)
    );
}

#[test]
fn tidy_line_drops_terminal_padding() {
    let rule = "─".repeat(300);
    assert_eq!(tools::tidy_line(&rule), "─".repeat(40));
    let padded = format!("{}✔ Update installed   ", " ".repeat(200));
    assert_eq!(tools::tidy_line(&padded), "✔ Update installed");
    assert_eq!(tools::tidy_line("a   b"), "a   b");
    let long = "x y ".repeat(100);
    assert_eq!(tools::tidy_line(&long).chars().count(), 240);
}

// ---------------------------------------------------------------------------
// Playbook

fn playbook_server() -> (tempfile::TempDir, McpServer<FakeControl>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(crate::playbook::PLAYBOOK),
        "# Playbook\n\n## Token economy\n\nCheap.\n\n## Coordinator duties\n\nWatch.\n\n## Topology\n\nTree.\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(crate::playbook::PROTOCOL),
        "# Protocol\n\n## Lifecycle\n\nReport.\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(crate::playbook::ATTENDANT_BRIEF),
        "# Attendant <campaign>\n\n## Loop\n\nPoll.\n",
    )
    .unwrap();
    let server = McpServer::new(FakeControl::default()).with_playbook_dir(dir.path());
    (dir, server)
}

#[test]
fn initialize_advertises_prompts_and_resources() {
    let (_d, mut server) = playbook_server();
    let r = rpc(
        &mut server,
        "initialize",
        json!({"protocolVersion": "2025-06-18"}),
    );
    let caps = &r["result"]["capabilities"];
    assert!(caps["prompts"].is_object());
    assert!(caps["resources"].is_object());
    let instructions = r["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("/mcp__myrstack__coordinator"));
    assert!(instructions.contains("get_playbook"));
}

#[test]
fn prompts_list_has_the_three_roles() {
    let (_d, mut server) = playbook_server();
    let r = rpc(&mut server, "prompts/list", json!({}));
    let prompts = r["result"]["prompts"].as_array().unwrap();
    let names: Vec<&str> = prompts
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "coordinator",
            "attendant",
            "worker",
            "attendant-brief",
            "kickoff"
        ]
    );
    for p in &prompts[..3] {
        assert!(!p["description"].as_str().unwrap().is_empty());
        assert_eq!(p["arguments"][0]["name"], "section");
        assert_eq!(p["arguments"][0]["required"], false);
    }
}

#[test]
fn prompts_get_returns_the_roles_rules_as_a_user_message() {
    let (_d, mut server) = playbook_server();
    let r = rpc(&mut server, "prompts/get", json!({"name": "coordinator"}));
    let messages = r["result"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"]["type"], "text");
    let text = messages[0]["content"]["text"].as_str().unwrap();
    assert!(text.starts_with(crate::playbook::STANDING_RULES), "{text}");
    assert!(text.contains("Playbook version: hash "), "{text}");
    assert!(text.contains("# Playbook") && text.contains("# Protocol"));
    assert_eq!(
        r["result"]["_meta"]["myrstack/playbook"]["files"],
        json!([crate::playbook::PLAYBOOK, crate::playbook::PROTOCOL])
    );

    let r = rpc(
        &mut server,
        "prompts/get",
        json!({"name": "attendant", "arguments": {"section": "coord"}}),
    );
    let text = r["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("## Coordinator duties\n\nWatch."), "{text}");
    assert!(!text.contains("Poll."));

    let r = rpc(&mut server, "prompts/get", json!({"name": "queen"}));
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    let r = rpc(
        &mut server,
        "prompts/get",
        json!({"name": "worker", "arguments": {"section": "nope"}}),
    );
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Lifecycle")
    );
}

#[test]
fn prompts_get_reports_a_missing_playbook() {
    let empty = tempfile::tempdir().unwrap();
    let mut server = McpServer::new(FakeControl::default()).with_playbook_dir(empty.path());
    let r = rpc(&mut server, "prompts/get", json!({"name": "worker"}));
    let message = r["error"]["message"].as_str().unwrap();
    assert!(message.contains("not found"), "{message}");
}

#[test]
fn resources_list_and_read() {
    let (_d, mut server) = playbook_server();
    let r = rpc(&mut server, "resources/list", json!({}));
    let resources = r["result"]["resources"].as_array().unwrap();
    let uris: Vec<&str> = resources
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(
        uris,
        [
            "myrstack://playbook/AGENT-FLEET-PLAYBOOK.md",
            "myrstack://playbook/_AGENT_PROTOCOL.md",
            "myrstack://playbook/ATTENDANT-BRIEF-TEMPLATE.md",
        ]
    );
    assert!(resources.iter().all(|r| r["mimeType"] == "text/markdown"));
    assert!(
        resources[1]["description"]
            .as_str()
            .unwrap()
            .contains("worker")
    );

    let r = rpc(
        &mut server,
        "resources/read",
        json!({"uri": "myrstack://playbook/_AGENT_PROTOCOL.md"}),
    );
    let content = &r["result"]["contents"][0];
    assert_eq!(content["uri"], "myrstack://playbook/_AGENT_PROTOCOL.md");
    assert_eq!(content["mimeType"], "text/markdown");
    let text = content["text"].as_str().unwrap();
    assert!(text.starts_with("Playbook version: "));
    assert!(text.ends_with("# Protocol\n\n## Lifecycle\n\nReport."));

    for uri in [
        "myrstack://playbook/NOPE.md",
        "file:///etc/passwd",
        "myrstack://playbook/../x",
    ] {
        let r = rpc(&mut server, "resources/read", json!({"uri": uri}));
        assert_eq!(r["error"]["code"], playbook::RESOURCE_NOT_FOUND, "{uri}");
    }
}

#[test]
fn playbook_tools_work_without_the_app() {
    let (_d, mut server) = playbook_server();
    let r = rpc(&mut server, "tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    for name in playbook::TOOLS {
        let t = tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(t["annotations"]["readOnlyHint"], true);
        check_schema(name, &t["inputSchema"]);
    }

    let r = call(&mut server, "get_playbook", json!({"role": "worker"}));
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("## Lifecycle"));
    assert_eq!(
        r["structuredContent"]["files"],
        json!(["_AGENT_PROTOCOL.md"])
    );
    assert!(r["structuredContent"]["version"]["hash"].is_string());

    let r = call(&mut server, "get_playbook", json!({"section": "topo"}));
    assert!(text(&r).contains("section \"Topology\""), "{}", text(&r));
    assert!(text(&r).ends_with("## Topology\n\nTree."));

    let r = call(
        &mut server,
        "get_playbook",
        json!({"file": "attendant-brief-template"}),
    );
    assert!(text(&r).contains("<campaign>"));

    let r = call(
        &mut server,
        "get_playbook",
        json!({"role": "worker", "file": "x"}),
    );
    assert!(is_error(&r));
    let r = call(&mut server, "get_playbook", json!({"role": "queen"}));
    assert!(is_error(&r));
    let r = call(&mut server, "get_playbook", json!({"section": "zzz"}));
    assert!(is_error(&r) && text(&r).contains("Coordinator duties"));

    let r = call(&mut server, "list_playbook", json!({}));
    assert!(!is_error(&r));
    assert!(text(&r).starts_with("Playbook version: "));
    assert!(text(&r).contains("Token economy · Coordinator duties · Topology"));
    assert_eq!(r["structuredContent"]["roles"][0]["name"], "coordinator");
    assert!(server.control().calls.is_empty(), "no control calls");
}

#[test]
fn whip_agents_reports_nudges_skips_and_the_pass() {
    let manual = json!({
        "mode": "manual", "sent": 1, "failed": 0, "automatic_dry_run": true,
        "nudges": [{"id": "term:w", "label": "Worker W", "handle": "w",
                    "via": {"bus": "dispatch:ctx_1", "run": "run_1"}, "ok": true,
                    "outcome": "sent", "warning": "waiting on your input; a nudge won't answer it"}],
        "skipped": [{"id": "queen", "label": "Queen", "reason": "the Queen is Myrstack itself"}],
    });
    let mut server = McpServer::new(FakeControl::default().reply("whip", Ok(manual)));
    let r = call(
        &mut server,
        "whip_agents",
        json!({"ids": ["term:w", "queen"]}),
    );
    let t = text(&r);
    assert!(t.starts_with("Sent 1 Myrstack nudge (0 failed)."), "{t}");
    assert!(t.contains("- Worker W `term:w` via bus to dispatch:ctx_1: sent"));
    assert!(t.contains("Warning: waiting on your input"));
    assert!(t.contains("- Queen `queen`: the Queen is Myrstack itself"));
    assert!(t.contains("live even though the automatic whip is in dry-run"));

    let pass = json!({"mode": "stalled", "ran": true, "dry_run": true, "actions": [
        {"action": "nudge #1 via bus to dispatch:ctx_1", "target": "worker w task t1",
         "outcome": "dry-run"}]});
    let mut server = McpServer::new(FakeControl::default().reply("whip", Ok(pass)));
    let r = call(&mut server, "whip_agents", json!({"stalled": true}));
    let t = text(&r);
    assert!(
        t.starts_with("Whip pass ran (dry-run: nothing sent): 1 action(s):"),
        "{t}"
    );
    assert!(t.contains("nudge #1 via bus to dispatch:ctx_1 → worker w task t1 = dry-run"));
}

// ---------------------------------------------------------------------------
// Filled templates and the fetch log

fn template_server() -> (tempfile::TempDir, McpServer<FakeControl>) {
    let (dir, _) = playbook_server();
    std::fs::write(
        dir.path().join(crate::playbook::ATTENDANT_BRIEF),
        "# Brief — <campaign>\nCOORD `<coordinator terminal handle>` · SCRATCH `<coordinator \
         scratchpad path>` · RUN_ID=<id> · msg <id>\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(crate::playbook::KICKOFF),
        "# KICKOFF — <track name>\n- Blocked by: <task/PR, or \"none\">\n- <fact>\n",
    )
    .unwrap();
    let facts = crate::playbook::fill::FleetFacts::from_fleet(
        &crate::playbook::fill::tests::fixture_fleet(),
    );
    let control =
        FakeControl::default().reply("fleet_data", Ok(serde_json::to_value(facts).unwrap()));
    let server = McpServer::new(control)
        .with_playbook_dir(dir.path())
        .with_fetch_log(dir.path().join("fetches.jsonl"));
    (dir, server)
}

fn logged(dir: &tempfile::TempDir) -> Vec<crate::playbook::fetches::FetchRecord> {
    crate::playbook::fetches::read(&dir.path().join("fetches.jsonl"))
}

#[test]
fn attendant_brief_tool_fills_from_the_app() {
    let (dir, mut server) = template_server();
    let r = call(&mut server, "attendant_brief", json!({"run_id": "run_1"}));
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(t.starts_with("Myrstack pre-filled ATTENDANT-BRIEF-TEMPLATE.md for run run_1"));
    assert!(t.contains("Data: the running Myrstack app"));
    assert!(t.contains("COORD `term_coord`"), "{t}");
    assert!(t.contains("RUN_ID=run_1 · msg <id>"), "{t}");
    assert_eq!(
        r["structuredContent"]["missing"],
        json!(["coordinator scratchpad path"])
    );
    assert_eq!(server.control().calls[0].0, "fleet_data");
    let log = logged(&dir);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].via, "tool:attendant_brief");
    assert_eq!(log[0].target, crate::playbook::ATTENDANT_BRIEF);
}

#[test]
fn template_tools_check_their_arguments_before_calling_anything() {
    let (dir, mut server) = template_server();
    for (tool, args, arg) in [
        ("attendant_brief", json!({}), "run_id"),
        ("attendant_brief", json!({"run_id": "  "}), "run_id"),
        ("kickoff", json!({"task_id": 7}), "task_id"),
    ] {
        let r = call(&mut server, tool, args);
        assert!(is_error(&r));
        assert!(text(&r).contains(arg), "{}", text(&r));
    }
    assert!(server.control().calls.is_empty());
    let r = call(&mut server, "kickoff", json!({"task_id": "t_nope"}));
    assert!(is_error(&r) && text(&r).contains("t_nope"));
    assert!(logged(&dir).is_empty(), "failed fills are not fetches");
}

#[test]
fn kickoff_prompt_fills_the_task_and_argument_errors_are_invalid_params() {
    let (dir, mut server) = template_server();
    let r = rpc(
        &mut server,
        "prompts/get",
        json!({"name": "kickoff", "arguments": {"task_id": "t_b"}}),
    );
    let t = r["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(t.contains("# KICKOFF — track B\n- Blocked by: t_a"), "{t}");
    assert!(t.contains("- <fact: FILL IN>"));
    assert_eq!(
        r["result"]["_meta"]["myrstack/template"]["missing"],
        json!(["fact"])
    );
    assert_eq!(logged(&dir)[0].via, "prompt:kickoff");

    let r = rpc(&mut server, "prompts/get", json!({"name": "kickoff"}));
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    assert!(r["error"]["message"].as_str().unwrap().contains("task_id"));
    let r = rpc(
        &mut server,
        "prompts/get",
        json!({"name": "attendant-brief", "arguments": {"run_id": "run_nope"}}),
    );
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    let r = rpc(&mut server, "prompts/list", json!({}));
    let brief = &r["result"]["prompts"][3];
    assert_eq!(brief["name"], "attendant-brief");
    assert_eq!(brief["arguments"][0]["name"], "run_id");
    assert_eq!(brief["arguments"][0]["required"], true);
}

#[test]
fn template_tools_fall_back_to_a_poll_when_the_app_is_down() {
    let (dir, _) = template_server();
    let control = FakeControl {
        fallback: Some(app()),
        ..FakeControl::default()
    }
    .reply("fleet_data", Err(CallError::NotRunning));
    let mut server = McpServer::new(control).with_playbook_dir(dir.path());
    let r = call(&mut server, "attendant_brief", json!({"run_id": RUN}));
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("Data: one direct Orca poll"));
    assert!(
        text(&r).contains(&format!("COORD `{COORDINATOR}`")),
        "{}",
        text(&r)
    );
    assert!(server.control().fallback_used);
}

#[test]
fn playbook_prompts_tools_and_resources_log_their_fetches() {
    let (dir, mut server) = template_server();
    rpc(&mut server, "prompts/get", json!({"name": "worker"}));
    call(
        &mut server,
        "get_playbook",
        json!({"role": "coordinator", "section": "topo"}),
    );
    call(&mut server, "list_playbook", json!({}));
    rpc(
        &mut server,
        "resources/read",
        json!({"uri": "myrstack://playbook/_AGENT_PROTOCOL.md"}),
    );
    let log = logged(&dir);
    let got: Vec<(&str, &str, Option<&str>)> = log
        .iter()
        .map(|r| (r.via.as_str(), r.target.as_str(), r.section.as_deref()))
        .collect();
    assert_eq!(
        got,
        [
            ("prompt:worker", "worker", None),
            ("tool:get_playbook", "coordinator", Some("Topology")),
            ("resource", "_AGENT_PROTOCOL.md", None),
        ]
    );
    assert!(log.iter().all(|r| r.hash.len() == 7 && r.at_ms > 0));
}

#[test]
fn fleet_status_reports_queen_auto_and_no_tool_sets_it() {
    let mut server = McpServer::new(app());
    let r = call(&mut server, "fleet_status", json!({}));
    assert_eq!(r["structuredContent"]["queen_auto"], false);
    assert!(text(&r).contains("Queen auto mode: off"), "{}", text(&r));

    let mut view = status_view();
    view.queen_auto = true;
    let mut server = McpServer::new(HeadlessControl::from_view(view, Arc::new(Screens)));
    let r = call(&mut server, "fleet_status", json!({}));
    assert_eq!(r["structuredContent"]["queen_auto"], true);
    assert!(text(&r).contains("Queen auto mode: on"), "{}", text(&r));

    let r = rpc(&mut server, "tools/list", json!({}));
    for tool in r["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert!(!name.contains("auto"), "{name}");
        let schema = tool["inputSchema"].to_string();
        assert!(
            !schema.contains("queen_auto") && !schema.contains("auto_mode"),
            "{name}: {schema}"
        );
    }
}

#[test]
fn close_agents_stays_a_dry_run_by_default() {
    // The MCP server knows nothing of the chat's auto mode: a close without dry_run=false is
    // always a dry run, so auto mode never turns an unqualified close into a real one.
    let mut server = McpServer::new(
        FakeControl::default().reply("close", Ok(json!({"dry_run": true, "plan": null}))),
    );
    call(&mut server, "close_agents", json!({"ids": ["term:a"]}));
    assert_eq!(server.control().calls[0].1["dry_run"], true);
}

#[test]
fn spawn_session_reports_the_new_session() {
    let reply = json!({
        "handle": "term_new", "node": "term:term_new",
        "worktree_id": "r::/wt", "worktree_path": "/wt",
        "task_id": null, "dispatch_id": null,
        "verified": true, "notes": ["accepted the Bypass Permissions warning (2)"],
    });
    let mut server = McpServer::new(FakeControl::default().reply("spawn", Ok(reply)));
    let r = call(
        &mut server,
        "spawn_session",
        json!({"mode": "new_worktree", "repo": "depict", "name": "x", "prompt": "go"}),
    );
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(t.starts_with("Started Claude in term_new (/wt)."), "{t}");
    assert!(t.contains("Node: term:term_new"));
    assert!(t.contains("The prompt shows as started."));
    assert!(t.contains("- accepted the Bypass Permissions warning (2)"));

    let failed = CallError::Control(crate::control::ControlError::failed(
        "Start worker: worker failed at agent_readiness",
    ));
    let mut server = McpServer::new(FakeControl::default().reply("spawn", Err(failed)));
    let r = call(
        &mut server,
        "spawn_session",
        json!({"mode": "worktree", "worktree": "w", "prompt": "go"}),
    );
    assert!(is_error(&r));
    assert!(text(&r).contains("agent_readiness"), "{}", text(&r));
}

// ---------------------------------------------------------------------------
// notify: agents reaching Karl

fn agent_origin() -> crate::review::mark::Origin {
    crate::review::mark::Origin {
        terminal_handle: Some("term_442fcb9f".into()),
        pane_key: Some("16b3a609:e499ad8b".into()),
        tab_id: None,
    }
}

fn agent_server<C: Control>(control: C) -> McpServer<C> {
    McpServer::new(control).with_review_env(review::ReviewEnv {
        origin: agent_origin(),
        pending: None,
        queue_file: None,
    })
}

#[test]
fn notify_is_listed_with_when_to_call_it() {
    let mut server = McpServer::new(FakeControl::default());
    let r = rpc(&mut server, "tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let tool = tools.iter().find(|t| t["name"] == "notify").unwrap();
    assert_eq!(tool["inputSchema"]["required"], json!(["title"]));
    assert_eq!(
        tool["inputSchema"]["properties"]["severity"]["enum"],
        json!(["info", "warn", "urgent"])
    );
    assert_eq!(tool["annotations"]["destructiveHint"], false);
    assert_eq!(tool["annotations"]["readOnlyHint"], false);
    let description = tool["description"].as_str().unwrap();
    for phrase in [
        "Blocked on Karl",
        "Handover",
        "one notification per event",
        "standalone session with no coordinator",
        "fall back to the existing channels",
        "quiet mode",
    ] {
        assert!(
            description.contains(phrase),
            "description mentions {phrase:?}"
        );
    }
    let init = rpc(&mut server, "initialize", json!({}));
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("notify {title, message, severity"));
    // The tool and the server instructions give the same rules.
    assert!(description.contains(notify::RULES));
    assert!(instructions.ends_with(notify::RULES));
}

#[test]
fn notify_sends_the_notice_with_the_callers_identity() {
    let reply = json!({
        "outcome": "delivered", "id": 4, "severity": "warn", "quiet": false,
        "agent": {"id": "term:term_442fcb9f", "label": "realtime-255"},
        "delivery": {"native": true, "in_app": true, "sound": true},
    });
    let mut server = agent_server(FakeControl::default().reply("notify", Ok(reply)));
    let r = call(
        &mut server,
        "notify",
        json!({
            "title": "Blocked: need prod DB access",
            "message": "cp-12 needs a read replica",
            "severity": "warn",
            "url": "https://github.com/o/r/issues/12",
        }),
    );
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(
        text(&r),
        "Sent to Karl (macOS pop-up, Myrstack pop-up, sound). Shown under realtime-255 \
         (`term:term_442fcb9f`)."
    );
    assert_eq!(
        server.control().calls,
        vec![(
            "notify".to_owned(),
            json!({
                "title": "Blocked: need prod DB access",
                "message": "cp-12 needs a read replica",
                "severity": "warn",
                "url": "https://github.com/o/r/issues/12",
                "origin": {"terminal_handle": "term_442fcb9f", "pane_key": "16b3a609:e499ad8b"},
            })
        )]
    );
    assert_eq!(r["structuredContent"]["id"], 4);
    assert_eq!(
        r["structuredContent"]["origin"]["terminal_handle"],
        "term_442fcb9f"
    );
}

#[test]
fn notify_explains_each_outcome() {
    let cases = [
        (
            json!({"outcome": "delivered", "id": 1, "quiet": true,
                   "delivery": {"native": false, "in_app": false, "sound": false}}),
            false,
            "Recorded in Karl's inbox without a pop-up (Karl is in quiet mode).",
        ),
        (
            json!({"outcome": "delivered", "id": 1, "quiet": false,
                   "delivery": {"native": false, "in_app": false, "sound": false}}),
            false,
            "below the severity Karl has pop-ups and sounds for",
        ),
        (
            json!({"outcome": "collapsed", "id": 1, "count": 3}),
            false,
            "collapsed into it (×3)",
        ),
        (
            json!({"outcome": "rate_limited", "retry_after_ms": 150_000}),
            true,
            "try again in 3 min",
        ),
        (
            json!({"outcome": "delivered", "id": 1, "unknown_agent": "ghost",
                   "delivery": {"native": true}}),
            false,
            "no node matches agent \"ghost\"",
        ),
    ];
    for (reply, error, phrase) in cases {
        let mut server = agent_server(FakeControl::default().reply("notify", Ok(reply)));
        let r = call(&mut server, "notify", json!({"title": "t"}));
        assert_eq!(is_error(&r), error, "{}", text(&r));
        assert!(text(&r).contains(phrase), "{phrase:?} in {}", text(&r));
    }
}

#[test]
fn notify_refuses_bad_arguments_before_calling_the_app() {
    let mut server = agent_server(FakeControl::default());
    for (args, phrase) in [
        (json!({}), "missing \"title\""),
        (json!({"title": "  "}), "\"title\""),
        (
            json!({"title": "t", "severity": "loud"}),
            "unknown severity",
        ),
        (
            json!({"title": "t", "url": "file:///etc/hosts"}),
            "http(s) link",
        ),
    ] {
        let r = call(&mut server, "notify", args.clone());
        assert!(is_error(&r), "{args}");
        assert!(text(&r).contains(phrase), "{args}: {}", text(&r));
    }
    assert!(server.control().calls.is_empty());
}

#[test]
fn notify_says_when_myrstack_is_down() {
    let mut server =
        agent_server(FakeControl::default().reply("notify", Err(CallError::NotRunning)));
    let r = call(&mut server, "notify", json!({"title": "t"}));
    assert!(is_error(&r));
    assert!(text(&r).starts_with("Myrstack isn't running, so Karl can't be notified"));
}

/// Answers `notify` like the app: through a real dispatcher.
struct CenterApp(
    std::sync::Mutex<(
        crate::notify::NotificationCenter,
        crate::notify::NotifySettings,
    )>,
);

impl CommandHandler for CenterApp {
    fn execute(&self, command: ControlCommand) -> Result<Value, ControlError> {
        let ControlCommand::Notify(notice) = command else {
            return Err(ControlError::failed("only notify"));
        };
        let mut guard = self.0.lock().unwrap();
        let (center, settings) = &mut *guard;
        let outcome = center.dispatch(notice.incoming(None), settings, NOW);
        let quiet = settings.quiets(notice.severity);
        Ok(crate::notify::agent::reply(
            &outcome, &notice, None, None, quiet,
        ))
    }
}

/// The tool, the control socket and the dispatcher, end to end: delivery, collapsing
/// duplicates, the per-agent limit and quiet mode.
#[test]
fn notify_reaches_the_dispatcher_over_the_socket() {
    use crate::notify::dispatch::AGENT_MAX;
    use crate::notify::{NotificationCenter, NotifySettings, Severity, Source};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sock");
    let app = Arc::new(CenterApp(std::sync::Mutex::new((
        NotificationCenter::default(),
        NotifySettings::default(),
    ))));
    let server = ControlServer::start(
        &path,
        Backend {
            status: SharedStatus::new(),
            screens: Arc::new(Screens),
            commands: app.clone(),
        },
    )
    .unwrap();
    let mut mcp = agent_server(SocketControl::new(path.clone()));
    let notify = |mcp: &mut McpServer<SocketControl>, title: &str, severity: &str| {
        call(
            mcp,
            "notify",
            json!({"title": title, "severity": severity, "message": "m"}),
        )
    };

    let r = notify(&mut mcp, "Need a decision", "warn");
    assert_eq!(
        text(&r),
        "Sent to Karl (macOS pop-up, Myrstack pop-up, sound)."
    );
    let r = notify(&mut mcp, "Need a decision", "warn");
    assert!(text(&r).contains("collapsed into it (×2)"), "{}", text(&r));
    for i in 1..AGENT_MAX {
        let r = notify(&mut mcp, &format!("n{i}"), "info");
        assert!(!is_error(&r), "{i}: {}", text(&r));
    }
    let r = notify(&mut mcp, "one too many", "urgent");
    assert!(is_error(&r));
    assert!(text(&r).starts_with("Not sent:"), "{}", text(&r));
    {
        let guard = app.0.lock().unwrap();
        let entries = guard.0.history.entries();
        assert_eq!(entries.len(), AGENT_MAX);
        let first = &entries[0];
        assert_eq!(
            (first.source, first.severity, first.count),
            (Source::Agent, Severity::Warn, 2)
        );
        assert_eq!(first.sender.as_deref(), Some("term_442fcb9f"));
        assert_eq!(first.target.as_deref(), Some("term_442fcb9f"));
    }

    // Quiet mode: another agent's warning is recorded silently; urgent gets through.
    app.0.lock().unwrap().1.quiet = true;
    let mut other = McpServer::new(SocketControl::new(path.clone()));
    let r = call(
        &mut other,
        "notify",
        json!({"title": "fyi", "severity": "warn"}),
    );
    assert!(text(&r).contains("quiet mode"), "{}", text(&r));
    let r = call(
        &mut other,
        "notify",
        json!({"title": "prod down", "severity": "urgent"}),
    );
    assert!(text(&r).starts_with("Sent to Karl"), "{}", text(&r));
    drop(server);
}

fn alert(id: u64, read: bool) -> view::AlertView {
    view::AlertView {
        id,
        at_ms: NOW - 120_000,
        severity: "warn".into(),
        source: "agent".into(),
        title: format!("Blocked {id}"),
        message: "needs prod access".into(),
        url: Some("https://github.com/o/r/issues/1".into()),
        agent: Some("realtime-255".into()),
        node: Some("term:term_442fcb9f".into()),
        count: 2,
        read,
    }
}

use crate::control::view;

#[test]
fn alerts_lists_the_inbox_with_read_state() {
    let alerts = json!({"alerts": [alert(2, false), alert(1, true)]});
    let mut server = McpServer::new(FakeControl::default().reply("alerts", Ok(alerts)));
    let r = call(&mut server, "alerts", json!({}));
    let t = text(&r);
    assert!(
        t.starts_with("2 alerts, 1 unread (newest first, showing 2):"),
        "{t}"
    );
    let line = t.lines().nth(1).unwrap();
    assert!(line.starts_with("- [unread] warn · "), "{line}");
    assert!(
        line.ends_with(
            "realtime-255: Blocked 2 ×2 `term:term_442fcb9f` — needs prod access \
             <https://github.com/o/r/issues/1>"
        ),
        "{line}"
    );
    assert_eq!(r["structuredContent"]["unread"], 1);
    let r = call(&mut server, "alerts", json!({"unread_only": true}));
    assert_eq!(text(&r).lines().count(), 2, "{}", text(&r));
}

#[test]
fn fleet_status_reports_unread_notifications() {
    let mut view = status_view();
    view.alerts = vec![alert(3, false), alert(2, true)];
    let mut server = McpServer::new(HeadlessControl::from_view(view, Arc::new(Screens)));
    let r = call(&mut server, "fleet_status", json!({}));
    let t = text(&r);
    assert!(
        t.contains("Notifications: 1 unread (alerts lists them):\n- [unread] warn · "),
        "{t}"
    );
    assert_eq!(r["structuredContent"]["notifications"]["unread"], 1);

    let mut server = McpServer::new(app());
    let t = text(&call(&mut server, "fleet_status", json!({}))).to_owned();
    assert!(t.contains("Notifications: none unread"), "{t}");
}

// ---------------------------------------------------------------------------
// Interrupted spawns

/// Answers `spawn` slowly, like a real spawn, so a cancel can arrive while it runs.
struct SlowSpawn(FakeControl);

impl Control for SlowSpawn {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
        if method == "spawn" {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        self.0.call(method, params)
    }
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn spawn_request(id: u64) -> String {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": "spawn_session", "arguments": {
            "mode": "new_worktree", "repo": "myrstack", "name": "agent-notify-tool",
            "prompt": "Add a notify tool",
        }},
    })
    .to_string()
}

fn slow_spawn_server(log: &std::path::Path) -> McpServer<SlowSpawn> {
    let reply = json!({
        "handle": "term_new", "node": "term:term_new", "worktree_path": "/wt/agent-notify-tool",
        "verified": true, "notes": [],
    });
    McpServer::new(SlowSpawn(FakeControl::default().reply("spawn", Ok(reply))))
        .with_interrupted_log(log)
}

#[test]
fn a_spawn_cancelled_while_it_ran_is_recorded() {
    use std::io::{BufRead, BufReader, Write};
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("interrupted.json");
    let mut server = slow_spawn_server(&log);
    let (input, mut to_server) = std::io::pipe().unwrap();
    let (from_server, output) = std::io::pipe().unwrap();
    let serving = std::thread::spawn(move || server.serve(BufReader::new(input), output));
    let mut replies = BufReader::new(from_server);
    let mut line = String::new();

    let cancel = json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": 7, "reason": "user interrupted"},
    });
    writeln!(to_server, "{}\n{cancel}", spawn_request(7)).unwrap();
    replies.read_line(&mut line).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 7);
    let records = crate::spawn::interrupted::load(&log, wall_ms());
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].reason, "cancelled");
    assert_eq!(records[0].node.as_deref(), Some("term:term_new"));
    assert_eq!(records[0].name.as_deref(), Some("agent-notify-tool"));

    // A spawn whose result reaches the client records nothing.
    writeln!(to_server, "{}", spawn_request(8)).unwrap();
    line.clear();
    replies.read_line(&mut line).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 8);
    assert_eq!(crate::spawn::interrupted::load(&log, wall_ms()).len(), 1);
    drop(to_server);
    serving.join().unwrap().unwrap();
}

#[test]
fn a_spawn_whose_client_went_away_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("interrupted.json");
    let mut server = slow_spawn_server(&log);
    let mut out = Vec::new();
    server
        .serve(Cursor::new(format!("{}\n", spawn_request(3))), &mut out)
        .unwrap();
    let records = crate::spawn::interrupted::load(&log, wall_ms());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].reason, "disconnected");
}

#[test]
fn fleet_status_and_list_agents_show_interrupted_spawns() {
    use crate::spawn::interrupted::{InterruptedSpawn, record};
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("interrupted.json");
    let mut server = McpServer::new(app()).with_interrupted_log(&log);
    let r = call(&mut server, "fleet_status", json!({}));
    assert!(!text(&r).contains("Interrupted spawns"));
    assert!(r["structuredContent"].get("interrupted_spawns").is_none());

    record(
        &log,
        InterruptedSpawn {
            at_ms: wall_ms(),
            reason: "cancelled".into(),
            mode: "new_worktree".into(),
            handle: Some(CODEX.into()),
            prompt: "Add a notify tool".into(),
            ..InterruptedSpawn::default()
        },
    )
    .unwrap();
    for tool in ["fleet_status", "list_agents"] {
        let r = call(&mut server, tool, json!({}));
        let t = text(&r);
        assert!(t.starts_with("Interrupted spawns (1)"), "{tool}: {t}");
        assert!(t.contains(&format!("`{CODEX}`")), "{t}");
        assert!(t.contains("don't spawn again"), "{t}");
        assert_eq!(
            r["structuredContent"]["interrupted_spawns"][0]["reason"],
            "cancelled"
        );
    }
}

#[test]
fn spawn_session_offers_allow_suffix_and_passes_it_on() {
    let r = rpc(
        &mut McpServer::new(FakeControl::default()),
        "tools/list",
        json!({}),
    );
    let spawn = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "spawn_session")
        .unwrap();
    assert_eq!(
        spawn["inputSchema"]["properties"]["allow_suffix"]["type"],
        "boolean"
    );
    assert!(
        spawn["description"]
            .as_str()
            .unwrap()
            .contains("run list_agents")
    );
    let mut server = McpServer::new(FakeControl::default());
    call(
        &mut server,
        "spawn_session",
        json!({"mode": "new_worktree", "repo": "r", "name": "x", "prompt": "go", "allow_suffix": true}),
    );
    assert_eq!(server.control().calls[0].1["allow_suffix"], true);
}

/// A real cleanup needs the approved ids; bad arguments fail before Orca is asked anything.
#[test]
fn clean_worktrees_refuses_a_real_run_without_ids() {
    let mut server = McpServer::new(FakeControl::default());
    for args in [
        json!({"dry_run": false}),
        json!({"dry_run": false, "ids": []}),
        json!({"dry_run": "no"}),
        json!({"ids": "r::/w/a"}),
        json!({"dry_run": false, "ids": [1]}),
    ] {
        let r = call(&mut server, "clean_worktrees", args.clone());
        assert_eq!(r["isError"], true, "{args}");
    }
    let tools = rpc(&mut server, "tools/list", json!({}));
    let tool = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "clean_worktrees")
        .unwrap()
        .clone();
    assert_eq!(
        tool["inputSchema"]["properties"]["dry_run"]["default"],
        true
    );
}

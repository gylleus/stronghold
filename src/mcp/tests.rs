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
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
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
        "focus_agent",
        "set_filter",
        "set_view",
        "fit_graph",
        "open_whip_settings",
        "alerts",
        "quotas",
    ];
    assert_eq!(names, expected);
    let reads = [
        "fleet_status",
        "list_agents",
        "get_agent",
        "whip_status",
        "alerts",
        "quotas",
    ];
    let destructive = [
        "send_to_agent",
        "press_key",
        "close_agents",
        "broadcast_to_coordinators",
        "set_whip",
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
        ("close_agents", json!({"ids": ["a"], "dry_run": "no"})),
        ("broadcast_to_coordinators", json!({"text": "x"})),
        (
            "broadcast_to_coordinators",
            json!({"text": "x", "runs": ["r"], "all": true}),
        ),
        ("set_whip", json!({})),
        ("list_agents", json!({"live": "yes"})),
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

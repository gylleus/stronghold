use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::sync::Arc;

use serde_json::{Value, json};

use super::backend::HeadlessControl;
use super::server::{INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR};
use super::*;
use crate::clan::ClanData;
use crate::control::server::{Backend, ScreenRead, ScreenReader};
use crate::control::view::{StatusView, ViewInputs};
use crate::control::{
    CommandHandler, ControlCommand, ControlError, ControlServer, ErrorCode, SharedStatus,
};
use crate::detect::PromptKind;
use crate::model::ClanSnapshot;
use crate::orca::commands::{parse_run_list, parse_terminal_list, parse_worker_list};
use crate::quota::Quotas;
use crate::ui::sidebar::ClanFilter;
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
    emitted: Vec<(String, Value)>,
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

    fn emit(&mut self, method: &str, params: Value) {
        self.emitted.push((method.into(), params));
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
            ControlCommand::OpenWhipSettings => Ok(json!({"open": true})),
            _ => Err(ControlError::not_found("no agent \"missing\"")),
        }
    }
}

fn clan() -> ClanData {
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
    ClanData {
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
        ..ClanData::default()
    }
}

fn status_view() -> StatusView {
    let data = clan();
    let snapshot = ClanSnapshot::build(&data, NOW);
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

        selected: None,
        selection: Vec::new(),
        filter: &ClanFilter::default(),
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
    assert_eq!(result["serverInfo"]["name"], "stronghold");
    assert!(result["capabilities"]["tools"].is_object());
    let instructions = result["instructions"].as_str().unwrap();
    for word in [
        "King",
        "coordinator",
        "worker",
        "subagent",
        "needs_input",
        "list_agents",
        "A dwarf is one dedicated agent session",
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
    let r = call(&mut server, "open_whip_settings", json!({}));
    assert!(r.get("structuredContent").is_none());

    let mut server = McpServer::new(FakeControl::default());
    rpc(
        &mut server,
        "initialize",
        json!({"protocolVersion": "2025-11-25"}),
    );
    let r = call(&mut server, "open_whip_settings", json!({}));
    assert_eq!(r["structuredContent"], json!({"ok": "open_whip_settings"}));
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
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"open_whip_settings","arguments":{}}}"#,
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
        vec![("open_whip_settings".to_string(), Value::Null)]
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
        "clan_status",
        "list_agents",
        "get_agent",
        "send_to_agent",
        "press_key",
        "close_agents",
        "park_agents",
        "broadcast_to_coordinators",
        "whip_status",
        "set_whip",
        "whip_agents",
        "spawn_session",
        "focus_agent",
        "set_filter",
        "open_whip_settings",
        "alerts",
        "quotas",
        "health",
        "lorekeeper_report",
        "lorekeeper_lookup",
        "get_playbook",
        "list_playbook",
        "attendant_brief",
        "kickoff",
        "mark_pr_ready",
        "review_queue",
        "notify",
        "tell_king",
        "king_inbox",
        "ask_user",
        "finish_session",
        "reach_out",
        "reach_out_status",
        "envoy_request",
        "envoy_route",
        "envoy_decline",
        "envoy_link_reply",
        "envoy_inbox",
        "envoy_event",
        "envoy_draft",
        "envoy_drafts",
        "envoy_relay",
        "envoy_propose_task",
        "envoy_note",
        "clean_worktrees",
    ];
    assert_eq!(names, expected);
    let reads = [
        "clan_status",
        "list_agents",
        "get_agent",
        "whip_status",
        "alerts",
        "quotas",
        "health",
        "lorekeeper_lookup",
        "get_playbook",
        "list_playbook",
        "attendant_brief",
        "kickoff",
        "review_queue",
        "king_inbox",
        "reach_out_status",
        "envoy_request",
        "envoy_inbox",
        "envoy_event",
        "envoy_drafts",
    ];
    let destructive = [
        "send_to_agent",
        "press_key",
        "close_agents",
        "park_agents",
        "broadcast_to_coordinators",
        "set_whip",
        "whip_agents",
        "spawn_session",
        "finish_session",
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
    assert_eq!(spawn["annotations"]["title"], "Start a new dwarf");
    assert!(
        spawn["description"]
            .as_str()
            .unwrap()
            .contains("make a new dwarf for X")
    );
    assert!(
        spawn["description"]
            .as_str()
            .unwrap()
            .contains("Dwarves default model"),
        "coordinators learn where the default model comes from"
    );
    assert_eq!(
        spawn["inputSchema"]["properties"]["effort"]["enum"],
        json!(crate::chat::settings::EFFORT_LEVELS)
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
                json!({"id": "term:a", "text": "continue", "enter": true, "origin": agent_origin()}),
            )],
        ),
        (
            "send_to_agent",
            json!({"id": "term:a", "text": "draft", "submit": false}),
            vec![(
                "send",
                json!({"id": "term:a", "text": "draft", "enter": false, "origin": agent_origin()}),
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
                json!({"ids": ["term:a", "term:b"], "whole_tab": true, "dry_run": false, "origin": agent_origin()}),
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
            json!({"mode": "worktree", "worktree": "shop/main", "prompt": " Fix it ", "bogus": 1}),
            vec![(
                "spawn",
                json!({"mode": "worktree", "worktree": "shop/main", "prompt": "Fix it", "agent": "claude"}),
            )],
        ),
        (
            "spawn_session",
            json!({"mode": "worker", "run_id": "run_1", "repo": "shop", "name": "w1",
                   "prompt": "spec", "agent": "codex", "model": "gpt-6-astra"}),
            vec![(
                "spawn",
                json!({"mode": "worker", "run_id": "run_1", "repo": "shop", "name": "w1",
                       "prompt": "spec", "agent": "codex", "model": "gpt-6-astra"}),
            )],
        ),
        (
            "focus_agent",
            json!({"id": "king", "raise_window": true}),
            vec![("focus", json!({"id": "king", "raise": true}))],
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
            "open_whip_settings",
            Value::Null,
            vec![("open_whip_settings", Value::Null)],
        ),
        ("alerts", json!({}), vec![("alerts", Value::Null)]),
        ("quotas", json!({}), vec![("quotas", Value::Null)]),
        ("health", json!({}), vec![("health", Value::Null)]),
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
                "health",
                Ok(serde_json::to_value(crate::health::HealthReport::default()).unwrap()),
            )
            .reply(
                "agent",
                Ok(
                    json!({"agent": serde_json::to_value(&status_view().agents[0]).unwrap(),
                          "children": [], "screen": null}),
                ),
            );
        let mut server = agent_server(control);
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
    let mut server = McpServer::new(FakeControl::default().reply("close", Ok(plan)))
        .with_review_env(review::ReviewEnv::default());
    let r = call(&mut server, "close_agents", json!({"ids": ["term:a"]}));
    assert_eq!(
        server.control().calls,
        vec![(
            "close".to_string(),
            json!({"id": "term:a", "whole_tab": false, "dry_run": true, "origin": {}})
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
    for tool in ["get_agent", "send_to_agent", "close_agents", "quotas"] {
        let control = FakeControl {
            replies: ["agent", "send", "close", "quotas"]
                .into_iter()
                .map(|m| (m.to_string(), Err(CallError::NotRunning)))
                .collect(),
            ..FakeControl::default()
        };
        let mut server = McpServer::new(control);
        let r = call(
            &mut server,
            tool,
            json!({"id": "a", "text": "x", "ids": ["a"]}),
        );
        assert!(is_error(&r));
        assert_eq!(
            text(&r),
            "Stronghold isn't running — start it with `stronghold`."
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
        (ErrorCode::Unavailable, "Stronghold isn't ready"),
        (ErrorCode::Failed, "Stronghold error: failed: no agent"),
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
    assert_eq!(text(&r), "Couldn't talk to Stronghold: broken pipe");
}

// ---------------------------------------------------------------------------
// Read tools against real control responses

#[test]
fn health_lists_checks_with_fixes() {
    use crate::health::{AppFacts, KingFact, evaluate};
    let mut view = status_view();
    let facts = AppFacts {
        whip_enabled: true,
        whip_dry_run: true,
        king: KingFact::Running,
    };
    view.health = evaluate(&facts, None, NOW);
    let mut server = McpServer::new(HeadlessControl::from_view(view, Arc::new(Screens)));
    let r = call(&mut server, "health", json!({}));
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(
        t.starts_with("Health: WARN — 1 problem (checks still running)."),
        "{t}"
    );
    assert!(t.contains("- [warn] Whip: dry run"), "{t}");
    assert!(t.contains("— fix: Settings › Whip"), "{t}");
    assert!(t.contains("- [ok] King: chat process running"), "{t}");
    assert_eq!(r["structuredContent"]["level"], "warn");
    assert_eq!(r["structuredContent"]["checks"][0]["key"], "whip");
}

#[test]
fn the_old_fleet_status_name_still_answers() {
    let mut server = McpServer::new(app());
    let r = call(&mut server, "fleet_status", json!({}));
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).starts_with("Clan: needs input · "));
}

#[test]
fn clan_status_summarises_the_clan() {
    let mut server = McpServer::new(app());
    let r = call(&mut server, "clan_status", json!({}));
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(t.starts_with("Clan: needs input · "), "{t}");
    assert!(t.contains("Needs input (1):"), "{t}");
    assert!(t.contains(&format!("`term:{CODEX}`")), "{t}");
    assert!(t.contains("choice: 1. Yes | 2. No"), "{t}");
    assert!(t.contains("| Run | Objective | Coordinator |"), "{t}");
    assert!(t.contains(RUN), "{t}");
    assert!(t.contains("NEEDS INPUT"), "{t}");
    assert!(t.contains("Whip: on, dry-run · 1 stall findings"), "{t}");
    assert!(!t.contains("Note: Stronghold isn't running"));
    let s = &r["structuredContent"];
    assert_eq!(s["fallback"], false);
    assert_eq!(s["needs_input"][0]["id"], format!("term:{CODEX}"));
    assert!(s["clan"]["runs"].is_array());
}

#[test]
fn clan_status_and_list_agents_fall_back_when_the_app_is_down() {
    for tool in ["clan_status", "list_agents"] {
        let mut control = FakeControl::default()
            .reply("status", Err(CallError::NotRunning))
            .reply("agents", Err(CallError::NotRunning));
        control.fallback = Some(app());
        let mut server = McpServer::new(control);
        let r = call(&mut server, tool, json!({}));
        assert!(!is_error(&r), "{}", text(&r));
        assert!(
            text(&r).starts_with("Note: Stronghold isn't running"),
            "{}",
            text(&r)
        );
        assert_eq!(r["structuredContent"]["fallback"], true);
        assert!(server.control().fallback_used);
    }
    // No fallback available: the plain error.
    let control = FakeControl::default().reply("status", Err(CallError::NotRunning));
    let mut server = McpServer::new(control);
    let r = call(&mut server, "clan_status", json!({}));
    assert!(is_error(&r));
    assert!(text(&r).starts_with("Stronghold isn't running"));
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
            github: Arc::new(crate::review::github::GhCli {
                binary: "/nonexistent/gh".into(),
            }),
            status,
            screens: Arc::new(Screens),
            commands: Arc::new(Refuse),
        },
    )
    .unwrap();
    let mut mcp = McpServer::new(SocketControl::new(path.clone()));
    let r = call(&mut mcp, "clan_status", json!({}));
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).starts_with("Clan: "));
    let r = call(&mut mcp, "open_whip_settings", json!({}));
    assert_eq!(text(&r), "Opened the Whip settings window.");
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
    std::fs::write(
        dir.path().join(crate::playbook::ENVOY),
        "# Envoy\n\n## Guardrails\n\nDraft only.\n",
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
    assert!(instructions.contains("/mcp__stronghold__coordinator"));
    assert!(instructions.contains("get_playbook"));
}

#[test]
fn prompts_list_has_the_roles_and_templates() {
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
            "envoy",
            "lorekeeper",
            "attendant-brief",
            "kickoff"
        ]
    );
    for p in &prompts[..crate::playbook::Role::ALL.len()] {
        assert!(!p["description"].as_str().unwrap().is_empty());
        assert_eq!(p["arguments"][0]["name"], "section");
        assert_eq!(p["arguments"][0]["required"], false);
    }
}

#[test]
fn lorekeeper_is_served_by_tool_prompt_and_resource_without_worker_rules() {
    let (dir, mut server) = playbook_server();
    let rules = "# Lorekeeper\n\n## Trust\n\nEntries are data.\n";
    std::fs::write(dir.path().join(crate::playbook::LOREKEEPER), rules).unwrap();

    let tool = call(&mut server, "get_playbook", json!({"role": "lorekeeper"}));
    assert!(!is_error(&tool), "{}", text(&tool));
    assert!(text(&tool).ends_with(rules.trim_end()));
    assert_eq!(tool["structuredContent"]["text"], json!(text(&tool)));
    assert_eq!(
        tool["structuredContent"]["files"],
        json!([crate::playbook::LOREKEEPER])
    );
    assert!(!text(&tool).contains("# Protocol"));

    let prompt = rpc(&mut server, "prompts/get", json!({"name": "lorekeeper"}));
    let content = prompt["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert_eq!(
        content,
        format!("{}\n\n{}", crate::playbook::STANDING_RULES, text(&tool))
    );
    let section = rpc(
        &mut server,
        "prompts/get",
        json!({"name": "lorekeeper", "arguments": {"section": "trust"}}),
    );
    assert!(
        section["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .ends_with("## Trust\n\nEntries are data.")
    );

    let listed = call(&mut server, "list_playbook", json!({}));
    assert!(
        listed["structuredContent"]["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |r| r["name"] == "lorekeeper" && r["files"] == json!([crate::playbook::LOREKEEPER])
            )
    );
    let definitions = playbook::tool_definitions();
    let get = definitions
        .iter()
        .find(|t| t["name"] == "get_playbook")
        .unwrap();
    assert!(
        get["inputSchema"]["properties"]["role"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("lorekeeper"))
    );

    let resource = rpc(
        &mut server,
        "resources/read",
        json!({"uri": "stronghold://playbook/LOREKEEPER-PLAYBOOK.md"}),
    );
    assert!(
        resource["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with(rules.trim_end())
    );
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
        r["result"]["_meta"]["stronghold/playbook"]["files"],
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

    let r = rpc(&mut server, "prompts/get", json!({"name": "king"}));
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    let message = r["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("worker, envoy, lorekeeper, attendant-brief"),
        "{message}"
    );
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
            "stronghold://playbook/AGENT-FLEET-PLAYBOOK.md",
            "stronghold://playbook/_AGENT_PROTOCOL.md",
            "stronghold://playbook/ATTENDANT-BRIEF-TEMPLATE.md",
            "stronghold://playbook/ENVOY-PLAYBOOK.md",
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
        json!({"uri": "stronghold://playbook/_AGENT_PROTOCOL.md"}),
    );
    let content = &r["result"]["contents"][0];
    assert_eq!(content["uri"], "stronghold://playbook/_AGENT_PROTOCOL.md");
    assert_eq!(content["mimeType"], "text/markdown");
    let text = content["text"].as_str().unwrap();
    assert!(text.starts_with("Playbook version: "));
    assert!(text.ends_with("# Protocol\n\n## Lifecycle\n\nReport."));

    // The scheme from before the rename still reads.
    let r = rpc(
        &mut server,
        "resources/read",
        json!({"uri": "myrstack://playbook/_AGENT_PROTOCOL.md"}),
    );
    let old_text = r["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(old_text.ends_with("# Protocol\n\n## Lifecycle\n\nReport."));

    for uri in [
        "stronghold://playbook/NOPE.md",
        "file:///etc/passwd",
        "stronghold://playbook/../x",
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

    let r = call(&mut server, "get_playbook", json!({"role": "envoy"}));
    assert!(text(&r).starts_with("Playbook version: "), "{}", text(&r));
    assert!(text(&r).ends_with("# Envoy\n\n## Guardrails\n\nDraft only."));
    assert_eq!(
        r["structuredContent"]["files"],
        json!(["ENVOY-PLAYBOOK.md"])
    );
    // Claude Code reads structuredContent, not the text block: the rules must be in it.
    assert_eq!(r["structuredContent"]["text"], json!(text(&r)));

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
    let r = call(&mut server, "get_playbook", json!({"role": "king"}));
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
        "skipped": [{"id": "king", "label": "King", "reason": "the King is Stronghold itself"}],
    });
    let mut server = McpServer::new(FakeControl::default().reply("whip", Ok(manual)));
    let r = call(
        &mut server,
        "whip_agents",
        json!({"ids": ["term:w", "king"]}),
    );
    let t = text(&r);
    assert!(t.starts_with("Sent 1 Stronghold nudge (0 failed)."), "{t}");
    assert!(t.contains("- Worker W `term:w` via bus to dispatch:ctx_1: sent"));
    assert!(t.contains("Warning: waiting on your input"));
    assert!(t.contains("- King `king`: the King is Stronghold itself"));
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
    let facts =
        crate::playbook::fill::ClanFacts::from_clan(&crate::playbook::fill::tests::fixture_clan());
    let control =
        FakeControl::default().reply("clan_data", Ok(serde_json::to_value(facts).unwrap()));
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
    assert!(t.starts_with("Stronghold pre-filled ATTENDANT-BRIEF-TEMPLATE.md for run run_1"));
    assert!(t.contains("Data: the running Stronghold app"));
    assert!(t.contains("COORD `term_coord`"), "{t}");
    assert!(t.contains("RUN_ID=run_1 · msg <id>"), "{t}");
    assert_eq!(
        r["structuredContent"]["missing"],
        json!(["coordinator scratchpad path"])
    );
    assert_eq!(server.control().calls[0].0, "clan_data");
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
        r["result"]["_meta"]["stronghold/template"]["missing"],
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
    let brief = &r["result"]["prompts"][crate::playbook::Role::ALL.len()];
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
    .reply("clan_data", Err(CallError::NotRunning));
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
        json!({"uri": "stronghold://playbook/_AGENT_PROTOCOL.md"}),
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
fn clan_status_reports_king_auto_and_no_tool_sets_it() {
    let mut server = McpServer::new(app());
    let r = call(&mut server, "clan_status", json!({}));
    assert_eq!(r["structuredContent"]["king_auto"], false);
    assert!(text(&r).contains("King auto mode: off"), "{}", text(&r));

    let mut view = status_view();
    view.king_auto = true;
    let mut server = McpServer::new(HeadlessControl::from_view(view, Arc::new(Screens)));
    let r = call(&mut server, "clan_status", json!({}));
    assert_eq!(r["structuredContent"]["king_auto"], true);
    assert!(text(&r).contains("King auto mode: on"), "{}", text(&r));

    let r = rpc(&mut server, "tools/list", json!({}));
    for tool in r["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert!(!name.contains("auto"), "{name}");
        let schema = tool["inputSchema"].to_string();
        assert!(
            !schema.contains("king_auto") && !schema.contains("auto_mode"),
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
        "model": "fable", "effort": "high",
    });
    let mut server = McpServer::new(FakeControl::default().reply("spawn", Ok(reply.clone())));
    let r = call(
        &mut server,
        "spawn_session",
        json!({"mode": "new_worktree", "repo": "shop", "name": "x", "prompt": "go"}),
    );
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(t.starts_with("Started Claude in term_new (/wt)."), "{t}");
    assert!(t.contains("Node: term:term_new"));
    assert!(t.contains("Model: fable, effort high."), "{t}");
    assert!(t.contains("The prompt shows as started."));
    assert!(t.contains("- accepted the Bypass Permissions warning (2)"));
    assert_eq!(
        server.control().calls[0].1["effort"],
        Value::Null,
        "no effort passed: the app fills the Dwarves default"
    );

    // No model in the result: the dwarf runs the CLI default and the text says so.
    let mut bare = reply.clone();
    bare["model"] = Value::Null;
    bare["effort"] = Value::Null;
    let mut server = McpServer::new(FakeControl::default().reply("spawn", Ok(bare)));
    let r = call(
        &mut server,
        "spawn_session",
        json!({"mode": "new_worktree", "repo": "shop", "name": "x", "prompt": "go",
               "effort": "max"}),
    );
    assert!(text(&r).contains("Model: the CLI default"), "{}", text(&r));
    assert_eq!(server.control().calls[0].1["effort"], "max");

    // Codex dwarves are outside the setting; nothing about a model is said.
    let mut server = McpServer::new(FakeControl::default().reply("spawn", Ok(reply)));
    let r = call(
        &mut server,
        "spawn_session",
        json!({"mode": "new_worktree", "repo": "shop", "name": "x", "prompt": "go",
               "agent": "codex"}),
    );
    assert!(!text(&r).contains("Model:"), "{}", text(&r));

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
        ..crate::review::mark::Origin::default()
    }
}

fn agent_server<C: Control>(control: C) -> McpServer<C> {
    McpServer::new(control).with_review_env(review::ReviewEnv {
        origin: agent_origin(),
        pending: None,
        queue_file: None,
        notify_pending: None,
        king_inbox: None,
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
        "Karl sees it in their inbox when Stronghold starts",
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
    assert!(description.contains(&crate::user::fill(notify::RULES)));
    assert!(instructions.contains(&crate::user::fill(notify::RULES)));
}

#[test]
fn notify_sends_the_notice_with_the_callers_identity() {
    let reply = json!({
        "outcome": "delivered", "id": 4, "severity": "warn", "quiet": false,
        "agent": {"id": "term:term_442fcb9f", "label": "api-255"},
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
        "Sent to Karl (macOS pop-up, Stronghold pop-up, sound). Shown under api-255 \
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
fn notify_queues_when_stronghold_is_down() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(crate::notify::pending::PENDING_FILE);
    let control = FakeControl::default().reply("notify", Err(CallError::NotRunning));
    let mut server = McpServer::new(control).with_review_env(review::ReviewEnv {
        origin: agent_origin(),
        notify_pending: Some(path.clone()),
        ..review::ReviewEnv::default()
    });
    let args = json!({
        "title": "Blocked: need prod access",
        "message": "for cp-12",
        "severity": "warn",
        "url": "https://github.com/o/r/pull/7",
    });
    let r = call(&mut server, "notify", args);
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("queued"), "{}", text(&r));
    assert!(text(&r).contains("when Stronghold starts"), "{}", text(&r));
    assert_eq!(r["structuredContent"]["outcome"], "queued");

    let taken = crate::notify::pending::take(&path);
    assert_eq!(taken.len(), 1);
    let notice = &taken[0].notice;
    assert_eq!(notice.title, "Blocked: need prod access");
    assert_eq!(notice.url.as_deref(), Some("https://github.com/o/r/pull/7"));
    assert_eq!(notice.origin, agent_origin());

    // Without a data dir to queue in, it still says so.
    let mut server =
        agent_server(FakeControl::default().reply("notify", Err(CallError::NotRunning)));
    let r = call(&mut server, "notify", json!({"title": "t"}));
    assert!(is_error(&r));
    assert!(text(&r).starts_with("Stronghold isn't running and there is no data dir"));
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
            github: Arc::new(crate::review::github::GhCli {
                binary: "/nonexistent/gh".into(),
            }),
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
        "Sent to Karl (macOS pop-up, Stronghold pop-up, sound)."
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
        pr: None,
        id,
        at_ms: NOW - 120_000,
        severity: "warn".into(),
        source: "agent".into(),
        title: format!("Blocked {id}"),
        message: "needs prod access".into(),
        url: Some("https://github.com/o/r/issues/1".into()),
        agent: Some("api-255".into()),
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
            "api-255: Blocked 2 ×2 `term:term_442fcb9f` — needs prod access \
             <https://github.com/o/r/issues/1>"
        ),
        "{line}"
    );
    assert_eq!(r["structuredContent"]["unread"], 1);
    let r = call(&mut server, "alerts", json!({"unread_only": true}));
    assert_eq!(text(&r).lines().count(), 2, "{}", text(&r));
}

#[test]
fn clan_status_includes_live_review_evidence_in_text_and_json() {
    let queue = json!({"entries": [{"url": "https://github.com/o/r/pull/1", "state": "open",
        "ci": {"state": "pending", "pending": 1, "total": 1},
        "ankra_on_head": false, "marker_on_head": true}], "github": {"available": true}});
    let status = json!({"ready": false, "clan": status_view().clan, "review_queue": queue});
    let mut server = McpServer::new(
        FakeControl::default()
            .reply("status", Ok(status))
            .reply("quotas", Ok(json!(status_view().quotas))),
    );
    let result = call(&mut server, "clan_status", json!({}));
    assert!(!is_error(&result), "{}", text(&result));
    assert_eq!(result["structuredContent"]["review_queue"], queue);
    assert!(text(&result).contains("https://github.com/o/r/pull/1 · open · CI running"));
    assert!(text(&result).contains("Ankra on head: no · marker on head: yes"));
}

#[test]
fn review_alert_text_labels_merged_and_unverified_prs() {
    for state in [json!("merged"), json!("closed"), Value::Null] {
        let mut notice = alert(1, false);
        notice.source = "review".into();
        notice.title = "PR ready for review: api#1".into();
        notice.pr = Some(json!({"state": state}));
        let mut server =
            McpServer::new(FakeControl::default().reply("alerts", Ok(json!({"alerts": [notice]}))));
        let result = call(&mut server, "alerts", json!({}));
        let label = state.as_str().unwrap_or("unknown; not pending evidence");
        assert!(text(&result).contains(&format!("[PR {label}] PR ready for review")));
        assert_eq!(
            result["structuredContent"]["alerts"][0]["pr"]["state"],
            state
        );
    }
}

#[test]
fn clan_status_reports_unread_notifications() {
    let mut view = status_view();
    view.alerts = vec![alert(3, false), alert(2, true)];
    let mut server = McpServer::new(HeadlessControl::from_view(view, Arc::new(Screens)));
    let r = call(&mut server, "clan_status", json!({}));
    let t = text(&r);
    assert!(
        t.contains("Notifications: 1 unread (alerts lists them):\n- [unread] warn · "),
        "{t}"
    );
    assert_eq!(r["structuredContent"]["notifications"]["unread"], 1);

    let mut server = McpServer::new(app());
    let t = text(&call(&mut server, "clan_status", json!({}))).to_owned();
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
            "mode": "new_worktree", "repo": "stronghold", "name": "agent-notify-tool",
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
fn clan_status_and_list_agents_show_interrupted_spawns() {
    use crate::spawn::interrupted::{InterruptedSpawn, record};
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("interrupted.json");
    let mut server = McpServer::new(app()).with_interrupted_log(&log);
    let r = call(&mut server, "clan_status", json!({}));
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
    for tool in ["clan_status", "list_agents"] {
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

// ---------------------------------------------------------------------------
// ask_user and finish_session

#[test]
fn ask_user_and_finish_session_are_listed_with_when_to_call_them() {
    let mut server = McpServer::new(FakeControl::default());
    let r = rpc(&mut server, "tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let ask = tools.iter().find(|t| t["name"] == "ask_user").unwrap();
    assert_eq!(ask["inputSchema"]["required"], json!(["question"]));
    let finish = tools
        .iter()
        .find(|t| t["name"] == "finish_session")
        .unwrap();
    assert_eq!(
        finish["inputSchema"]["required"],
        json!(["summary", "approval"])
    );
    assert_eq!(
        finish["inputSchema"]["properties"]["outcome"]["enum"],
        json!(["done", "handed_over", "abandoned"])
    );
    let finish_text = finish["description"].as_str().unwrap();
    for phrase in [
        "once Karl said you can go",
        "Anything else, or can I close?",
        "trivial",
        "worktree and branch are kept",
        "force",
        "coordinator",
    ] {
        assert!(
            finish_text.contains(phrase),
            "finish_session mentions {phrase:?}"
        );
    }
    let init = rpc(&mut server, "initialize", json!({}));
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(instructions.ends_with(&crate::user::fill(lifecycle::RULES)));
    assert!(crate::user::fill(lifecycle::RULES).contains("call ask_user"));
    assert!(
        crate::user::fill(lifecycle::RULES)
            .contains("finish_session only once Karl says you can go")
    );
    assert!(crate::user::fill(lifecycle::RULES).contains("Anything else, or can I close?"));
    // notify points plain questions at ask_user.
    assert!(crate::user::fill(notify::RULES).contains("ask_user"));
}

#[test]
fn ask_karl_sends_the_question_with_the_callers_identity() {
    let reply = json!({
        "outcome": "delivered", "id": 9, "severity": "warn", "quiet": false,
        "agent": {"id": "term:term_442fcb9f", "label": "api-255"},
        "delivery": {"native": true, "in_app": true, "sound": false},
        "waiting": true,
    });
    let mut server = agent_server(FakeControl::default().reply("ask", Ok(reply)));
    let r = call(
        &mut server,
        "ask_user",
        json!({"question": "Merge now or wait for the migration?", "context": "Migration lands Friday."}),
    );
    assert!(!is_error(&r), "{r}");
    let t = text(&r);
    assert!(
        t.starts_with("Sent to Karl (macOS pop-up, Stronghold pop-up)."),
        "{t}"
    );
    assert!(t.contains("needs input until Karl answers"), "{t}");
    assert!(t.contains("End your turn now"), "{t}");
    let (method, params) = &server.control().calls[0];
    assert_eq!(method, "ask");
    assert_eq!(params["question"], "Merge now or wait for the migration?");
    assert_eq!(params["context"], "Migration lands Friday.");
    assert_eq!(params["severity"], "warn");
    assert_eq!(params["origin"]["terminal_handle"], "term_442fcb9f");
}

#[test]
fn ask_karl_refuses_bad_arguments_and_says_when_stronghold_is_down() {
    let mut server = agent_server(FakeControl::default());
    let r = call(&mut server, "ask_user", json!({"question": "  "}));
    assert!(is_error(&r));
    let r = call(
        &mut server,
        "ask_user",
        json!({"question": "Ok?", "severity": "loud"}),
    );
    assert!(is_error(&r));
    assert!(server.control().calls.is_empty());

    let mut down = agent_server(FakeControl::default().reply("ask", Err(CallError::NotRunning)));
    let r = call(&mut down, "ask_user", json!({"question": "Ok?"}));
    assert!(is_error(&r));
    assert!(text(&r).contains("Ask in the chat"), "{}", text(&r));
}

/// Answers `finish`: `check` to the dry run, `done` to the real one.
#[derive(Default)]
struct FinishControl {
    calls: Vec<Value>,
    check: Value,
    done: Option<Result<Value, CallError>>,
}

impl Control for FinishControl {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
        assert_eq!(method, "finish");
        let dry = params["dry_run"] == true;
        self.calls.push(params);
        if dry {
            Ok(self.check.clone())
        } else {
            self.done.clone().unwrap_or_else(|| {
                Ok(json!({
                    "dry_run": false,
                    "agent": self.check["agent"],
                    "closing_in_ms": 2000,
                }))
            })
        }
    }
}

/// A finish check for a terminal whose worktree is `worktree`, with the app's `problems`.
fn finish_check(worktree: &std::path::Path, problems: Value) -> Value {
    json!({
        "dry_run": true,
        "agent": {
            "id": "term:term_442fcb9f", "label": "api-255", "kind": "terminal",
            "worktree": worktree, "branch": "karl/api-255",
        },
        "refused": null,
        "problems": problems,
    })
}

#[test]
fn finish_session_checks_then_finishes() {
    let dir = tempfile::TempDir::new().unwrap();
    let control = FinishControl {
        check: finish_check(dir.path(), json!([])),
        ..FinishControl::default()
    };
    let mut server = agent_server(control);
    let r = call(
        &mut server,
        "finish_session",
        json!({"summary": "Fixed the retry bug; merged into main", "pr_url": "https://github.com/o/r/pull/3", "approval": "karl"}),
    );
    assert!(!is_error(&r), "{r}");
    let t = text(&r);
    assert!(
        t.starts_with("Finished: Stronghold recorded it (done)"),
        "{t}"
    );
    assert!(t.contains("closes this terminal in about 2 s"), "{t}");
    assert!(t.contains("branch karl/api-255 are kept"), "{t}");
    let calls = &server.control().calls;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["dry_run"], true);
    assert_eq!(calls[1]["dry_run"], false);
    assert_eq!(calls[1]["summary"], "Fixed the retry bug; merged into main");
    assert_eq!(calls[1]["pr_url"], "https://github.com/o/r/pull/3");
    assert_eq!(calls[1]["origin"]["terminal_handle"], "term_442fcb9f");
    assert_eq!(calls[1]["forced"], json!([]));
    assert_eq!(calls[1]["approval"], "user");
}

#[test]
fn finish_session_refuses_with_problems_unless_forced() {
    // A git worktree with an uncommitted file, and an open PR the app reports.
    let dir = tempfile::TempDir::new().unwrap();
    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .status()
        .unwrap()
        .success();
    assert!(ok);
    std::fs::write(dir.path().join("scratch.txt"), "x").unwrap();
    let pr = "https://github.com/o/r/pull/3 is waiting for Karl's review: stay";
    let check = finish_check(dir.path(), json!([pr]));

    let mut server = agent_server(FinishControl {
        check: check.clone(),
        ..FinishControl::default()
    });
    let r = call(
        &mut server,
        "finish_session",
        json!({"summary": "done", "approval": "karl"}),
    );
    assert!(is_error(&r));
    let t = text(&r);
    assert!(
        t.starts_with("Not finished; your terminal stays open."),
        "{t}"
    );
    assert!(t.contains("uncommitted change"), "{t}");
    assert!(t.contains("waiting for Karl's review"), "{t}");
    assert!(t.contains("force: true"), "{t}");
    assert_eq!(server.control().calls.len(), 1, "only the check ran");

    let mut forced = agent_server(FinishControl {
        check,
        ..FinishControl::default()
    });
    let r = call(
        &mut forced,
        "finish_session",
        json!({"summary": "Karl called it off", "outcome": "abandoned", "force": true, "approval": "karl"}),
    );
    assert!(!is_error(&r), "{r}");
    assert!(text(&r).contains("left despite"), "{}", text(&r));
    let calls = &forced.control().calls;
    assert_eq!(calls[1]["force"], true);
    assert_eq!(calls[1]["outcome"], "abandoned");
    // The git problems travel to the app to be recorded; the app adds its own.
    let sent = calls[1]["forced"].as_array().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].as_str().unwrap().contains("uncommitted change"));
}

#[test]
fn finish_session_passes_on_a_refusal_and_stronghold_being_down() {
    let mut check = finish_check(std::path::Path::new("/nonexistent"), json!([]));
    check["refused"] = json!("you're a worker in run r1 (task t1): report to your coordinator");
    let mut server = agent_server(FinishControl {
        check,
        ..FinishControl::default()
    });
    let r = call(
        &mut server,
        "finish_session",
        json!({"summary": "done", "force": true, "approval": "trivial"}),
    );
    assert!(is_error(&r));
    assert!(
        text(&r).starts_with("Can't finish: you're a worker"),
        "{}",
        text(&r)
    );
    assert_eq!(server.control().calls.len(), 1);

    let mut down = agent_server(FakeControl::default().reply("finish", Err(CallError::NotRunning)));
    let r = call(
        &mut down,
        "finish_session",
        json!({"summary": "done", "approval": "karl"}),
    );
    assert!(is_error(&r));
    assert!(text(&r).contains("Tell Karl in the chat"), "{}", text(&r));

    let mut bad = agent_server(FakeControl::default());
    let r = call(
        &mut bad,
        "finish_session",
        json!({"summary": "done", "outcome": "merged", "approval": "karl"}),
    );
    assert!(is_error(&r));
    let r = call(
        &mut bad,
        "finish_session",
        json!({"summary": "done", "approval": "me"}),
    );
    assert!(is_error(&r));
    assert!(bad.control().calls.is_empty());
}

#[test]
fn finish_session_without_karls_go_ahead_asks_him_first() {
    let mut server = agent_server(FinishControl::default());
    let r = call(
        &mut server,
        "finish_session",
        json!({"summary": "Merged into main"}),
    );
    assert!(is_error(&r));
    let t = text(&r);
    assert!(
        t.starts_with("Not finished; your terminal stays open."),
        "{t}"
    );
    assert!(t.contains("ask Karl first (ask_user"), "{t}");
    assert!(server.control().calls.is_empty(), "nothing reaches the app");
}

#[test]
fn envoy_tools_refuse_missing_and_non_envoy_origins() {
    let mut missing =
        McpServer::new(FakeControl::default()).with_review_env(review::ReviewEnv::default());
    for method in crate::envoy::control::METHODS {
        assert_eq!(call(&mut missing, method, json!({}))["isError"], true);
    }
    assert!(missing.control().calls.is_empty());
    let mut control = FakeControl::default();
    for method in crate::envoy::control::METHODS {
        control = control.reply(
            method,
            Err(CallError::Control(ControlError::new(
                ErrorCode::Refused,
                "Only the persisted Envoy terminal may call Envoy tools",
            ))),
        );
    }
    let mut other = agent_server(control);
    for method in crate::envoy::control::METHODS {
        let result = call(
            &mut other,
            method,
            json!({"origin":{"terminal_handle":"forged"}}),
        );
        assert_eq!(result["isError"], true);
    }
    assert!(
        other
            .control()
            .calls
            .iter()
            .all(|(_, p)| p["origin"] == json!(agent_origin()))
    );
}

#[test]
fn envoy_tools_preserve_args_and_have_no_approval_tool() {
    let mut server = agent_server(FakeControl::default());
    let args = json!({"in_reply_to":{"conversation":"C123","ts":"1790000000.000001"},"text":"A draft","rationale":"Reply requested"});
    assert_eq!(
        call(&mut server, "envoy_draft", args.clone())["isError"],
        false
    );
    let (method, params) = &server.control().calls[0];
    assert_eq!(method, "envoy_draft");
    assert_eq!(params["text"], args["text"]);
    assert_eq!(params["origin"], json!(agent_origin()));
    assert!(!crate::envoy::control::METHODS.contains(&"envoy_approve"));
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

#[test]
fn park_agents_parks_or_wakes_and_reports_each() {
    let reply = json!({"parked": true, "parked_now": 1, "results": [
        {"agent": "term:a", "id": "term:a", "label": "Dwarf A", "ok": true, "changed": true},
        {"agent": "king", "id": "king", "label": "King", "ok": false,
         "error": "the King isn't a session"},
    ]});
    let mut server = McpServer::new(FakeControl::default().reply("park", Ok(reply)));
    let r = call(
        &mut server,
        "park_agents",
        json!({"ids": ["term:a", "king"]}),
    );
    assert!(!is_error(&r));
    assert_eq!(
        server.control().calls,
        vec![(
            "park".to_string(),
            json!({"ids": ["term:a", "king"], "park": true})
        )]
    );
    let t = text(&r);
    assert!(
        t.contains("Dwarf A `term:a`: parked in the resting area"),
        "{t}"
    );
    assert!(t.contains("King `king`: not parked: the King"), "{t}");
    assert!(t.ends_with("1 parked in all."), "{t}");

    let reply = json!({"parked": false, "parked_now": 0, "results": [
        {"agent": "term:a", "id": "term:a", "label": "Dwarf A", "ok": true, "changed": true}]});
    let mut server = McpServer::new(FakeControl::default().reply("park", Ok(reply)));
    let r = call(
        &mut server,
        "park_agents",
        json!({"ids": ["term:a"], "wake": true}),
    );
    assert_eq!(server.control().calls[0].1["park"], json!(false));
    assert!(
        text(&r).contains("woke, back in the live clan"),
        "{}",
        text(&r)
    );
}

#[test]
fn sending_to_a_parked_agent_says_it_woke() {
    let reply = json!({"agent": "term:a", "handle": "a", "sent": "send", "woke": true});
    let mut server = McpServer::new(FakeControl::default().reply("send", Ok(reply)));
    let r = call(
        &mut server,
        "send_to_agent",
        json!({"id": "term:a", "text": "go on"}),
    );
    assert!(
        text(&r).ends_with("It was parked; it is awake now."),
        "{}",
        text(&r)
    );
}

// ---------------------------------------------------------------------------
// Tool-call beams

#[test]
fn tool_calls_on_agents_emit_one_beam_each() {
    let agent = app().call("agent", json!({"id": CODEX})).unwrap();
    let id = agent["agent"]["id"].clone();
    let control = FakeControl::default()
        .reply("agent", Ok(agent))
        .reply(
            "close",
            Ok(json!({"dry_run": true, "plan": {"targets": [{"id": "term:b"}]}})),
        )
        .reply("enter", Err(CallError::NotRunning));
    let mut server = McpServer::new(control);
    call(&mut server, "get_agent", json!({"id": "a"}));
    call(&mut server, "close_agents", json!({"ids": ["b"]}));
    call(&mut server, "open_whip_settings", json!({}));
    // A failed call draws nothing.
    call(&mut server, "press_key", json!({"id": "a", "key": "enter"}));
    let emitted = &server.control().emitted;
    assert_eq!(emitted.len(), 2, "{emitted:?}");
    assert!(emitted.iter().all(|(m, _)| m == "tool_call"));
    assert_eq!(emitted[0].1["kind"], "read");
    assert_eq!(emitted[0].1["targets"], json!([id]));
    assert_eq!(emitted[1].1["kind"], "dry_kill");
    // Emitting is not a call: the app's replies are untouched.
    assert!(!server.control().calls.iter().any(|(m, _)| m == "tool_call"));
}

#[test]
fn ask_karl_tells_envoy_to_keep_taking_batches_and_preserves_unmatched_guidance() {
    for nonblocking in [true, false] {
        let reply = json!({
            "outcome": "delivered", "id": 9, "severity": "warn", "quiet": false,
            "delivery": {"native": true, "in_app": true, "sound": false},
            "waiting": false, "nonblocking": nonblocking,
        });
        let mut server = agent_server(FakeControl::default().reply("ask", Ok(reply)));
        let r = call(
            &mut server,
            "ask_user",
            json!({"question": "Should tartine go live?"}),
        );
        assert!(!is_error(&r), "{r}");
        let t = text(&r);
        assert_eq!(t.contains("Keep taking batches"), nonblocking, "{t}");
        assert_eq!(
            t.contains("couldn't match you to a terminal"),
            !nonblocking,
            "{t}"
        );
        assert!(!t.contains("End your turn now"), "{t}");
    }
}

#[test]
fn king_inbox_tools_queue_offline_with_caller_identity_and_read_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(crate::king_inbox::FILE);
    let mut server = McpServer::new(FakeControl::default()).with_review_env(review::ReviewEnv {
        origin: agent_origin(),
        king_inbox: Some(path),
        ..Default::default()
    });
    let catalog = rpc(&mut server, "tools/list", json!({}));
    let names: Vec<_> = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"tell_king"));
    assert!(names.contains(&"king_inbox"));
    let args = json!({"message":"Need shared schema decision", "kind":"question", "url":"https://example.com/issue", "origin":{"terminal_handle":"imposter"}});
    let result = call(&mut server, "tell_king", args.clone());
    assert!(!is_error(&result), "{}", text(&result));
    let read = call(&mut server, "king_inbox", json!({}));
    assert!(text(&read).contains("term_442fcb9f"));
    assert!(!text(&read).contains("imposter"));
    let repeat = call(&mut server, "tell_king", args.clone());
    assert!(text(&repeat).contains("collapsed"));
    // The Queen's names from before she became the King still answer, unlisted.
    assert!(!names.contains(&"tell_queen") && !names.contains(&"queen_inbox"));
    assert!(text(&call(&mut server, "tell_queen", args)).contains("collapsed"));
    assert!(text(&call(&mut server, "queen_inbox", json!({}))).contains("term_442fcb9f"));
    for args in [
        json!({}),
        json!({"message":"hi", "kind":"bad"}),
        json!({"message":"hi", "url":12}),
    ] {
        assert!(is_error(&call(&mut server, "tell_king", args)));
    }
    assert!(is_error(&call(
        &mut server,
        "king_inbox",
        json!({"limit":0})
    )));
}

#[test]
fn close_passes_caller_identity_to_the_pre_close_animation_hook() {
    for origin in [crate::review::mark::Origin::default(), agent_origin()] {
        let mut server =
            McpServer::new(FakeControl::default()).with_review_env(review::ReviewEnv {
                origin: origin.clone(),
                ..Default::default()
            });
        call(
            &mut server,
            "close_agents",
            json!({"ids": ["a"], "dry_run": false}),
        );
        let params = &server.control().calls[0].1;
        assert_eq!(params["origin"], json!(origin));
        match ControlCommand::parse("close", params).unwrap().unwrap() {
            ControlCommand::Close {
                origin: parsed,
                dry_run,
                ..
            } => {
                assert_eq!(parsed, Some(origin));
                assert!(!dry_run);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}

#[test]
fn every_reply_names_the_user_instead_of_the_placeholder() {
    let mut server = McpServer::new(FakeControl::default());
    for (method, params) in [
        ("initialize", json!({})),
        ("tools/list", json!({})),
        ("prompts/list", json!({})),
    ] {
        let r = rpc(&mut server, method, params).to_string();
        assert!(!r.contains(crate::user::PLACEHOLDER), "{method}: {r}");
        assert!(r.contains("Karl"), "{method}");
    }
    let r = rpc(
        &mut server,
        "tools/call",
        json!({"name": "ask_karl", "arguments": {}}),
    );
    assert_eq!(r["result"]["isError"], true, "the old name still answers");
}

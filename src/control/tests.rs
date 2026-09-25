use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::app::View;
use crate::broadcast::{BroadcastSelection, plan_broadcast};
use crate::detect::PromptKind;
use crate::fleet::FleetData;
use crate::model::{FleetSnapshot, NodeKind, NodeState};
use crate::orca::OutgoingMessage;
use crate::orca::commands::{parse_run_list, parse_terminal_list, parse_worker_list};
use crate::quota::Quotas;
use crate::ui::sidebar::FleetFilter;
use crate::whip::{WhipSettings, WhipStatus};

const COORDINATOR: &str = "term_70c8df0b-cf78-4b43-a6f8-aefab63bca21";
const CODEX: &str = "term_1744da75-821f-46fe-912d-e252e0bbf3fb";
const LOOSE: &str = "term_08fe0725-96a1-4355-a4fd-6c8a07aa6c72";
const RUN: &str = "run_7c25cdcf46e0";
const NOW: i64 = 1_790_319_595_528 + 10_000;

fn fleet() -> FleetData {
    let terminals = parse_terminal_list(include_bytes!(
        "../../tests/fixtures/orca/terminal_list.json"
    ))
    .unwrap()
    .terminals;
    let mut runs =
        parse_run_list(include_bytes!("../../tests/fixtures/orca/run_list.json")).unwrap();
    runs[0].coordinator_handle = Some(COORDINATOR.into());
    let mut needs_input = BTreeMap::new();
    needs_input.insert(
        CODEX.to_string(),
        Some(PromptKind::Choice {
            options: vec!["1. Yes".into(), "2. No".into()],
        }),
    );
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
        needs_input,
        ..FleetData::default()
    }
}

fn status_view() -> StatusView {
    let data = fleet();
    let snapshot = FleetSnapshot::build(&data, NOW);
    let whip_status = WhipStatus {
        escalated: BTreeSet::from([LOOSE.to_string()]),
        findings: 1,
        enabled: true,
        dry_run: true,
        ..WhipStatus::default()
    };
    let filter = FleetFilter {
        kinds: BTreeSet::from([NodeKind::Coordinator]),
        ..FleetFilter::default()
    };
    StatusView::build(&ViewInputs {
        snapshot: Some(&snapshot),
        data: Some(&data),
        claude: None,
        whip_status: &whip_status,
        whip_settings: &WhipSettings::default(),
        whip_log: Vec::new(),
        quotas: &Quotas::default(),
        alerts: &[],
        view: View::Table,
        selected: Some("term:x"),
        selection: vec!["term:x", "term:y"],
        filter: &filter,
        whip_settings_open: false,
        now_ms: NOW,
    })
}

#[derive(Default)]
struct FakeScreens;

impl ScreenReader for FakeScreens {
    fn read_screen(&self, handle: &str) -> Result<ScreenRead, String> {
        if handle == LOOSE {
            return Err("terminal gone".into());
        }
        let mut lines: Vec<String> = (1..=50).map(|i| format!("row {i}")).collect();
        lines.extend(["".into(), "  ".into()]);
        Ok(ScreenRead {
            lines,
            status: Some("running".into()),
        })
    }
}

/// Records commands; answers each with its debug text, or an error for agent `missing`.
#[derive(Default, Clone)]
struct FakeCommands {
    log: Arc<Mutex<Vec<ControlCommand>>>,
}

impl CommandHandler for FakeCommands {
    fn execute(&self, command: ControlCommand) -> Result<Value, ControlError> {
        self.log.lock().unwrap().push(command.clone());
        let debug = format!("{command:?}");
        if debug.contains("\"missing\"") {
            return Err(ControlError::not_found("no agent"));
        }
        Ok(json!({ "did": debug }))
    }
}

/// Field order matters: the server must drop (and remove its socket) before the temp dir.
struct Harness {
    server: Option<ControlServer>,
    path: PathBuf,
    commands: FakeCommands,
    status: SharedStatus,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_FILE);
        let status = SharedStatus::new();
        status.publish(status_view());
        let commands = FakeCommands::default();
        let backend = Backend {
            status: status.clone(),
            screens: Arc::new(FakeScreens),
            commands: Arc::new(commands.clone()),
        };
        let server = ControlServer::start(&path, backend).unwrap();
        Self {
            _dir: dir,
            path,
            server: Some(server),
            commands,
            status,
        }
    }

    fn client(&self) -> ControlClient {
        ControlClient::connect(&self.path).unwrap()
    }

    fn call(&self, method: &str, params: Value) -> Response {
        self.client().call(&Request::new(method, params)).unwrap()
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        let r = self.call(method, params);
        assert!(r.ok, "{method}: {r:?}");
        r.result.unwrap()
    }

    fn err(&self, method: &str, params: Value) -> ErrorCode {
        let r = self.call(method, params);
        assert!(!r.ok, "{method} should fail: {r:?}");
        r.error.unwrap().code
    }

    fn raw(&self, line: &str) -> Response {
        let text = self.client().call_raw(line).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn commands(&self) -> Vec<ControlCommand> {
        self.commands.log.lock().unwrap().clone()
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// ---------------------------------------------------------------------------
// View

#[test]
fn view_summarizes_the_fleet() {
    let view = status_view();
    assert_eq!(
        view.fleet.state, "needs_input",
        "the Queen takes the most urgent state"
    );
    assert_eq!(view.fleet.agents, 3, "the Queen is not an agent");
    assert!(view.agents.iter().all(|a| a.kind != "queen"));
    assert!(view.find_agent("queen").is_none());
    assert_eq!(view.fleet.live, 3);
    assert_eq!(view.fleet.by_kind["coordinator"], 1);
    assert_eq!(view.fleet.needs_input, vec![format!("term:{CODEX}")]);
    let run = &view.fleet.runs[0];
    assert_eq!(run.id, RUN);
    assert_eq!(
        run.coordinator.as_deref(),
        Some(&*format!("term:{COORDINATOR}"))
    );
    assert!(
        run.objective
            .as_deref()
            .unwrap()
            .starts_with("commerce-platform")
    );

    let codex = view.find_agent(CODEX).unwrap();
    assert_eq!(codex.state, "needs_input");
    assert_eq!(codex.agent.as_deref(), Some("codex"));
    let prompt = codex.needs_input.as_ref().unwrap();
    assert_eq!(prompt.kind, "choice");
    assert_eq!(prompt.options, vec!["1. Yes", "2. No"]);

    let loose = view.find_agent(&format!("term:{LOOSE}")).unwrap();
    assert!(loose.escalated);
    assert_eq!(loose.kind, "terminal");
    assert_eq!(loose.idle_secs, Some((NOW - 1_789_598_758_208) / 1000));

    assert_eq!(view.dashboard.view, "table");
    assert_eq!(view.dashboard.filter.kinds, vec!["coordinator"]);
    assert_eq!(view.dashboard.filter.hidden, 2);
    assert!(view.whip.dry_run);
}

#[test]
fn find_agent_by_label_case_insensitively() {
    let view = status_view();
    let label = view.find_agent(COORDINATOR).unwrap().label.clone();
    let found = view.find_agent(&label.to_uppercase()).unwrap();
    assert_eq!(found.id, format!("term:{COORDINATOR}"));
    assert!(view.find_agent("nope").is_none());
}

#[test]
fn names_parse_loosely() {
    assert_eq!(view::parse_state("NeedsInput"), Some(NodeState::NeedsInput));
    assert_eq!(
        view::parse_state("needs-input"),
        Some(NodeState::NeedsInput)
    );
    assert_eq!(view::parse_kind("Worker"), Some(NodeKind::Worker));
    assert_eq!(view::parse_view("TABLE"), Some(View::Table));
    assert_eq!(view::parse_state("sleepy"), None);
}

#[test]
fn tail_drops_trailing_blank_rows() {
    let lines: Vec<String> = ["a", "b", "", "c   ", " ", ""].map(String::from).to_vec();
    assert_eq!(server::tail(&lines, 2), vec!["", "c"]);
    assert_eq!(server::tail(&lines, 10), vec!["a", "b", "", "c"]);
    assert!(server::tail(&[String::new()], 5).is_empty());
}

// ---------------------------------------------------------------------------
// Read methods over the socket

#[test]
fn ping_lists_methods() {
    let h = Harness::new();
    let r = h.ok("ping", Value::Null);
    assert_eq!(r["protocol"], 1);
    assert_eq!(r["ready"], true);
    assert!(
        r["methods"]["action"]
            .as_array()
            .unwrap()
            .contains(&json!("focus"))
    );
}

#[test]
fn status_returns_summary_and_dashboard() {
    let h = Harness::new();
    let r = h.ok("status", Value::Null);
    assert_eq!(r["ready"], true);
    assert_eq!(r["fleet"]["agents"], 3);
    assert_eq!(r["fleet"]["runs"][0]["id"], RUN);
    assert_eq!(r["whip"]["dry_run"], true);
    assert_eq!(r["whip"]["escalated"], json!([LOOSE]));
    assert_eq!(r["dashboard"]["selected"], "term:x");
    assert_eq!(r["dashboard"]["selection"], json!(["term:x", "term:y"]));
}

#[test]
fn status_before_first_poll_is_not_ready() {
    let h = Harness::new();
    h.status.publish(StatusView::default());
    assert_eq!(h.ok("status", Value::Null)["ready"], false);
    assert_eq!(h.err("agents", Value::Null), ErrorCode::Unavailable);
}

#[test]
fn agents_filters() {
    let h = Harness::new();
    let all = h.ok("agents", Value::Null);
    assert_eq!(all["count"], 3);
    assert!(all["agents"][0]["idle_secs"].is_i64());

    let waiting = h.ok("agents", json!({"state": "needs_input"}));
    assert_eq!(waiting["count"], 1);
    assert_eq!(waiting["agents"][0]["terminal_handle"], CODEX);
    let also = h.ok("agents", json!({"needs_input": true}));
    assert_eq!(also["agents"], waiting["agents"]);

    let kinds = h.ok("agents", json!({"kind": ["coordinator", "terminal"]}));
    assert_eq!(kinds["count"], 2);
    assert_eq!(h.ok("agents", json!({"run": RUN}))["count"], 2);
    assert_eq!(h.ok("agents", json!({"text": "HACK 77"}))["count"], 1);
    assert_eq!(h.ok("agents", json!({"live": false}))["count"], 0);

    assert_eq!(
        h.err("agents", json!({"state": "sleepy"})),
        ErrorCode::InvalidParams
    );
    assert_eq!(
        h.err("agents", json!({"live": "yes"})),
        ErrorCode::InvalidParams
    );
}

#[test]
fn agent_detail_reads_the_screen() {
    let h = Harness::new();
    let r = h.ok("agent", json!({"id": CODEX, "lines": 3}));
    assert_eq!(r["agent"]["id"], format!("term:{CODEX}"));
    assert_eq!(r["agent"]["needs_input"]["kind"], "choice");
    assert_eq!(r["screen"]["lines"], json!(["row 48", "row 49", "row 50"]));
    assert_eq!(r["screen"]["status"], "running");

    let coordinator = h.ok("agent", json!({"id": format!("term:{COORDINATOR}")}));
    assert_eq!(coordinator["screen"]["lines"].as_array().unwrap().len(), 40);
    assert_eq!(
        coordinator["children"],
        json!([format!("term:{CODEX}")]),
        "the run's worker hangs under its coordinator"
    );

    let no_screen = h.ok("agent", json!({"id": CODEX, "screen": false}));
    assert!(no_screen["screen"].is_null());

    let failed = h.ok("agent", json!({"id": LOOSE}));
    assert_eq!(failed["screen"]["error"], "terminal gone");

    assert_eq!(h.err("agent", json!({"id": "nobody"})), ErrorCode::NotFound);
    assert_eq!(h.err("agent", Value::Null), ErrorCode::InvalidParams);
}

#[test]
fn whip_quotas_alerts() {
    let h = Harness::new();
    let mut view = status_view();
    view.whip.log = (1..=30)
        .map(|seq| view::WhipLogView {
            seq,
            ..Default::default()
        })
        .collect();
    view.alerts = vec![view::AlertView {
        at_ms: 5,
        title: "Myrstack: needs input".into(),
        node: None,
    }];
    view.quotas.fetched_at_ms = 7;
    h.status.publish(view);

    let whip = h.ok("whip", Value::Null);
    assert_eq!(whip["log"].as_array().unwrap().len(), 20);
    assert_eq!(whip["log"][19]["seq"], 30);
    assert_eq!(whip["dry_run"], true);
    let few = h.ok("whip", json!({"limit": 2}));
    assert_eq!(few["log"][0]["seq"], 29);

    assert_eq!(h.ok("quotas", Value::Null)["fetched_at_ms"], 7);
    assert_eq!(
        h.ok("alerts", Value::Null)["alerts"][0]["title"],
        "Myrstack: needs input"
    );
}

// ---------------------------------------------------------------------------
// Actions over the socket

#[test]
fn every_action_reaches_the_handler() {
    let h = Harness::new();
    let calls = [
        ("send", json!({"id": "a", "text": "hi"})),
        ("send", json!({"id": "a", "text": "draft", "enter": false})),
        ("enter", json!({"id": "a"})),
        ("esc", json!({"id": "a"})),
        ("interrupt", json!({"id": "a"})),
        ("broadcast", json!({"text": "stop", "runs": [RUN]})),
        ("broadcast", json!({"text": "all", "all": true})),
        ("whip_settings", json!({"enabled": false})),
        ("select", json!({"id": "a"})),
        ("select", json!({"id": null})),
        ("select", json!({"ids": ["a", "b"]})),
        ("select", json!({"id": "a", "ids": ["b"]})),
        ("select", json!({"ids": ["a"]})),
        ("select", json!({"ids": []})),
        ("focus", json!({"id": "a", "raise": true})),
        ("filter", json!({"states": ["needs_input"], "text": "hack"})),
        ("filter", json!({"clear": true})),
        ("view", json!({"view": "table"})),
        ("fit", Value::Null),
        ("open_whip_settings", Value::Null),
        ("close", json!({"id": "a"})),
        (
            "close",
            json!({"ids": ["a", "b"], "whole_tab": true, "dry_run": true}),
        ),
    ];
    for (method, params) in &calls {
        h.ok(method, params.clone());
    }
    let expected = vec![
        ControlCommand::Send {
            agent: "a".into(),
            text: "hi".into(),
            enter: true,
        },
        ControlCommand::Send {
            agent: "a".into(),
            text: "draft".into(),
            enter: false,
        },
        ControlCommand::Key {
            agent: "a".into(),
            key: TerminalKey::Enter,
        },
        ControlCommand::Key {
            agent: "a".into(),
            key: TerminalKey::Escape,
        },
        ControlCommand::Key {
            agent: "a".into(),
            key: TerminalKey::Interrupt,
        },
        ControlCommand::Broadcast {
            text: "stop".into(),
            runs: Some(vec![RUN.into()]),
        },
        ControlCommand::Broadcast {
            text: "all".into(),
            runs: None,
        },
        ControlCommand::WhipSettings {
            enabled: Some(false),
            dry_run: None,
            notify: None,
        },
        ControlCommand::Select {
            agent: Some("a".into()),
        },
        ControlCommand::Select { agent: None },
        ControlCommand::SelectMany {
            agents: vec!["a".into(), "b".into()],
        },
        ControlCommand::SelectMany {
            agents: vec!["a".into(), "b".into()],
        },
        ControlCommand::Select {
            agent: Some("a".into()),
        },
        ControlCommand::Select { agent: None },
        ControlCommand::Focus {
            agent: "a".into(),
            raise: true,
        },
        ControlCommand::Filter(FilterChange {
            states: Some(BTreeSet::from([NodeState::NeedsInput])),
            text: Some("hack".into()),
            ..FilterChange::default()
        }),
        ControlCommand::Filter(FilterChange {
            clear: true,
            ..FilterChange::default()
        }),
        ControlCommand::SetView(View::Table),
        ControlCommand::Fit,
        ControlCommand::OpenWhipSettings,
        ControlCommand::Close {
            agents: vec!["a".into()],
            whole_tab: false,
            dry_run: false,
        },
        ControlCommand::Close {
            agents: vec!["a".into(), "b".into()],
            whole_tab: true,
            dry_run: true,
        },
    ];
    assert_eq!(h.commands(), expected);
}

#[test]
fn handler_errors_come_back() {
    let h = Harness::new();
    assert_eq!(
        h.err("focus", json!({"id": "missing"})),
        ErrorCode::NotFound
    );
    assert_eq!(
        h.err("close", json!({"id": "missing"})),
        ErrorCode::NotFound
    );
}

#[test]
fn invalid_action_params_never_reach_the_handler() {
    let h = Harness::new();
    let bad = [
        ("send", json!({"id": "a"})),
        ("send", json!({"text": "hi"})),
        ("send", json!({"id": "a", "text": "", "enter": false})),
        ("send", json!({"id": "a", "text": 5})),
        ("enter", json!({})),
        ("broadcast", json!({"text": "x"})),
        (
            "broadcast",
            json!({"text": "x", "runs": [RUN], "all": true}),
        ),
        ("broadcast", json!({"runs": [RUN]})),
        ("focus", json!({"id": ""})),
        ("filter", json!({})),
        ("filter", json!({"kinds": ["robot"]})),
        ("view", json!({"view": "map"})),
        ("whip_settings", json!({"dry_run": "no"})),
        ("close", json!({})),
        ("close", json!({"ids": []})),
        ("close", json!({"id": "a", "dry_run": "yes"})),
    ];
    for (method, params) in bad {
        assert_eq!(
            h.err(method, params.clone()),
            ErrorCode::InvalidParams,
            "{method} {params}"
        );
    }
    assert!(h.commands().is_empty());
}

// ---------------------------------------------------------------------------
// Protocol errors

#[test]
fn malformed_requests() {
    let h = Harness::new();
    let code = |line: &str| h.raw(line).error.unwrap().code;
    assert_eq!(code("not json"), ErrorCode::ParseError);
    assert_eq!(code("[1,2]"), ErrorCode::InvalidRequest);
    assert_eq!(code(r#"{"method":"status"}"#), ErrorCode::InvalidRequest);
    assert_eq!(
        code(r#"{"v":"1","method":"status"}"#),
        ErrorCode::InvalidRequest
    );
    assert_eq!(code(r#"{"v":1}"#), ErrorCode::InvalidRequest);
    assert_eq!(
        code(r#"{"v":2,"method":"status"}"#),
        ErrorCode::UnsupportedVersion
    );
    assert_eq!(
        code(r#"{"v":1,"method":"dance"}"#),
        ErrorCode::UnknownMethod
    );
    assert_eq!(
        code(r#"{"v":1,"method":"status","params":[1]}"#),
        ErrorCode::InvalidParams
    );
}

#[test]
fn id_is_echoed_even_on_errors() {
    let h = Harness::new();
    let r = h.raw(r#"{"v":1,"method":"ping","id":"abc"}"#);
    assert_eq!(r.id, Some(json!("abc")));
    let r = h.raw(r#"{"v":9,"method":"ping","id":7}"#);
    assert_eq!(r.id, Some(json!(7)));
    assert!(!r.ok);
}

#[test]
fn many_requests_on_one_connection() {
    let h = Harness::new();
    let mut client = h.client();
    for method in ["ping", "status", "agents"] {
        let r = client.call(&Request::new(method, Value::Null)).unwrap();
        assert!(r.ok, "{method}");
    }
    // Blank lines are skipped, not answered.
    let mut stream = UnixStream::connect(&h.path).unwrap();
    stream
        .write_all(b"\n\n{\"v\":1,\"method\":\"ping\"}\n")
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    assert_eq!(out.lines().count(), 1);
    assert!(out.contains("\"ok\":true"));
}

#[test]
fn oversized_and_non_utf8_requests_are_rejected() {
    let h = Harness::new();
    let mut stream = UnixStream::connect(&h.path).unwrap();
    stream.write_all(&[0xff, 0xfe, b'\n']).unwrap();
    let mut client_line = String::new();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    std::io::BufRead::read_line(&mut reader, &mut client_line).unwrap();
    assert!(client_line.contains("parse_error"), "{client_line}");

    let mut big = UnixStream::connect(&h.path).unwrap();
    let huge = vec![b'x'; server::MAX_REQUEST_BYTES + 10];
    // The server answers and closes once the limit is passed; the rest of the write may fail.
    let _ = big.write_all(&huge);
    let mut out = String::new();
    let _ = big.read_to_string(&mut out);
    assert!(out.contains("longer than"), "{out}");
}

// ---------------------------------------------------------------------------
// Socket file

#[test]
fn socket_is_private() {
    let h = Harness::new();
    assert_eq!(mode(&h.path), 0o600);
    let dir = h.path.parent().unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from(SOCKET_FILE)]);
}

#[test]
fn stale_socket_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(SOCKET_FILE);
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists(), "a dropped listener leaves its socket file");
    let listener = server::bind_socket(&path).unwrap();
    drop(listener);
}

#[test]
fn live_socket_is_not_stolen() {
    let h = Harness::new();
    let err = server::bind_socket(&h.path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    assert!(h.ok("ping", Value::Null)["ready"].as_bool().unwrap());
}

#[test]
fn regular_file_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(SOCKET_FILE);
    std::fs::write(&path, "keep me").unwrap();
    assert!(server::bind_socket(&path).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
}

#[test]
fn missing_parent_dir_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join(SOCKET_FILE);
    drop(server::bind_socket(&path).unwrap());
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn dropping_the_server_removes_the_socket() {
    let mut h = Harness::new();
    assert!(h.path.exists());
    h.server = None;
    assert!(!h.path.exists());
    assert!(UnixStream::connect(&h.path).is_err());
}

// ---------------------------------------------------------------------------
// App command channel

#[test]
fn app_commands_round_trip() {
    let woke = Arc::new(Mutex::new(0));
    let w = woke.clone();
    let (handler, rx) = AppCommands::channel(Some(Box::new(move || *w.lock().unwrap() += 1)));
    let app = std::thread::spawn(move || {
        let pending = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(pending.command, ControlCommand::Fit);
        pending.reply.send(Ok(json!({"view": "graph"})));
    });
    let result = handler.execute(ControlCommand::Fit).unwrap();
    assert_eq!(result, json!({"view": "graph"}));
    app.join().unwrap();
    assert_eq!(*woke.lock().unwrap(), 1);
}

#[test]
fn app_commands_dropped_or_gone() {
    let (handler, rx) = AppCommands::channel(None);
    let app = std::thread::spawn(move || drop(rx.recv().unwrap()));
    let err = handler.execute(ControlCommand::Fit).unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable);
    app.join().unwrap();
    // Receiver gone: sending fails straight away.
    assert_eq!(
        handler.execute(ControlCommand::Fit).unwrap_err().code,
        ErrorCode::Unavailable
    );
}

// ---------------------------------------------------------------------------
// Broadcast delivery

struct FakeSender {
    sent: Vec<OutgoingMessage>,
    fail: bool,
}

impl crate::broadcast::BroadcastSender for FakeSender {
    fn send(&mut self, message: &OutgoingMessage) -> Result<Option<String>, String> {
        self.sent.push(message.clone());
        if self.fail {
            Err("bus down".into())
        } else {
            Ok(Some("msg_1".into()))
        }
    }
}

#[test]
fn broadcast_delivery_reports_each_outcome() {
    let data = fleet();
    let snapshot = FleetSnapshot::build(&data, NOW);
    let plan = plan_broadcast(
        &snapshot,
        &BroadcastSelection::new([RUN, "run_gone"]),
        "wrap up",
    );
    let mut ok = FakeSender {
        sent: Vec::new(),
        fail: false,
    };
    let r = commands::deliver_broadcast(&mut ok, &plan);
    assert_eq!(r["sent"], 1);
    assert_eq!(r["deliveries"][0]["coordinator"], COORDINATOR);
    assert_eq!(r["deliveries"][0]["message_id"], "msg_1");
    assert_eq!(r["skipped"][0]["run_id"], "run_gone");
    assert_eq!(ok.sent[0].subject, "Myrstack: wrap up");

    let mut failing = FakeSender {
        sent: Vec::new(),
        fail: true,
    };
    let r = commands::deliver_broadcast(&mut failing, &plan);
    assert_eq!(r["sent"], 0);
    assert_eq!(r["failed"], 1);
    assert_eq!(r["deliveries"][0]["error"], "bus down");
}

#[test]
fn filter_change_applies_only_given_fields() {
    let mut filter = FleetFilter {
        text: "old".into(),
        show_loose: false,
        ..FleetFilter::default()
    };
    FilterChange {
        repos: Some(BTreeSet::from(["myrstack".to_string()])),
        ..FilterChange::default()
    }
    .apply(&mut filter);
    assert_eq!(filter.text, "old");
    assert!(!filter.show_loose);
    assert!(filter.repos.contains("myrstack"));
    FilterChange {
        clear: true,
        text: Some("new".into()),
        ..FilterChange::default()
    }
    .apply(&mut filter);
    assert_eq!(
        filter,
        FleetFilter {
            text: "new".into(),
            ..FleetFilter::default()
        }
    );
}

#[test]
fn cli_params_accept_json_or_a_bare_id() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(client::cli_params(&args(&[])).unwrap(), Value::Null);
    assert_eq!(
        client::cli_params(&args(&["term:abc"])).unwrap(),
        json!({"id": "term:abc"})
    );
    assert_eq!(
        client::cli_params(&args(&[r#"{"state":"idle"}"#])).unwrap(),
        json!({"state": "idle"})
    );
    assert!(client::cli_params(&args(&["{broken"])).is_err());
    assert!(client::cli_params(&args(&["a", "b"])).is_err());
}

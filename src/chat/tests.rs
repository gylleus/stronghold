//! Chat tests: the recorded stream-json fixtures and the process lifecycle against a fake
//! `claude` script.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::protocol::{
    self, HostRequest, Incoming, PermissionAnswer, PermissionRequest, parse_line,
};
use super::transcript::{ChatItem, Decision, Transcript};
use super::*;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/chat")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The CLI's lines of a fixture, parsed.
fn incoming(name: &str) -> Vec<Incoming> {
    fixture(name)
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| parse_line(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

/// The host's lines of a fixture (`# HOST> …`), as JSON.
fn host_lines(name: &str) -> Vec<Value> {
    fixture(name)
        .lines()
        .filter_map(|l| l.strip_prefix("# HOST> "))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn json(line: &str) -> Value {
    serde_json::from_str(line).unwrap()
}

fn transcript_of(messages: &[Incoming]) -> Transcript {
    let mut t = Transcript::default();
    t.push_user("question");
    for m in messages {
        t.apply(m);
    }
    t
}

fn permission_request(messages: &[Incoming]) -> PermissionRequest {
    messages
        .iter()
        .find_map(|m| match m {
            Incoming::ControlRequest {
                request: HostRequest::CanUseTool(r),
                ..
            } => Some(r.clone()),
            _ => None,
        })
        .expect("a can_use_tool request")
}

#[test]
fn hello_deltas_stream_into_one_message() {
    let messages = incoming("hello.jsonl");
    let init = messages
        .iter()
        .find_map(|m| match m {
            Incoming::Init(i) => Some(i),
            _ => None,
        })
        .unwrap();
    assert_eq!(init.model.as_deref(), Some("claude-fable-5"));
    assert!(
        init.tools
            .iter()
            .any(|t| t == "mcp__myrstack__fleet_status")
    );
    assert!(init.mcp_servers.iter().any(|(name, _)| name == "myrstack"));

    // Up to (not including) the finished assistant message, the text streams in.
    let finished = messages
        .iter()
        .position(|m| matches!(m, Incoming::Assistant { .. }))
        .unwrap();
    let mut t = transcript_of(&messages[..finished]);
    assert!(t.running);
    assert_eq!(
        t.items.last(),
        Some(&ChatItem::Assistant {
            text: "Hey Karl! 👋 What can I help you with today?".into(),
            streaming: true
        })
    );
    for m in &messages[finished..] {
        t.apply(m);
    }
    assert_eq!(
        t.assistant_text(),
        vec!["Hey Karl! 👋 What can I help you with today?"]
    );
    assert!(!t.running);
    let result = t.last_result.as_ref().unwrap();
    assert_eq!(result.subtype, "success");
    assert!(!result.is_error);
    assert!(result.cost_usd.unwrap() > 0.0);
    assert_eq!(result.output_tokens, Some(21));
    assert!(t.cost_usd > 0.0);
    assert!(!t.items.iter().any(|i| matches!(i, ChatItem::Error(_))));
}

#[test]
fn tool_use_pairs_with_result_and_permission_round_trips() {
    let messages = incoming("tool_permission_allow.jsonl");
    let host = host_lines("tool_permission_allow.jsonl");
    assert_eq!(json(&protocol::initialize_request("host-init-1")), host[0]);
    let user = json(&protocol::user_message(
        "Call the myrstack fleet_status tool and tell me how many agents need input. One sentence.",
    ));
    assert_eq!(user, host[1]);

    let request = permission_request(&messages);
    assert_eq!(request.tool_name, "mcp__myrstack__fleet_status");
    assert_eq!(request.display_name.as_deref(), Some("Fleet Status"));
    assert!(permissions::is_auto_approved(&request.tool_name));
    let allow = json(&protocol::permission_response(
        &request,
        &PermissionAnswer::Allow,
    ));
    assert_eq!(allow, host[2]);

    let t = transcript_of(&messages);
    let calls: Vec<_> = t.tool_calls().collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "ToolSearch");
    let search = calls[0].result.as_ref().unwrap();
    assert!(search.text.contains("mcp__myrstack__fleet_status"));
    assert_eq!(calls[1].name, "mcp__myrstack__fleet_status");
    assert_eq!(calls[1].input, Some(serde_json::json!({})));
    let status = calls[1].result.as_ref().unwrap();
    assert!(!status.is_error);
    assert!(status.text.contains("\"needs_input\": []"));
    assert_eq!(
        t.assistant_text().last().copied(),
        Some(
            "Zero agents need input — the `needs_input` list is empty, though one worker \
             (`example-worker`) is stalled and has been escalated by the whip."
        )
    );
    assert!(!t.running);
}

#[test]
fn denied_permission_becomes_an_error_result() {
    let messages = incoming("tool_permission_deny.jsonl");
    let host = host_lines("tool_permission_deny.jsonl");
    let request = permission_request(&messages);
    assert_eq!(request.tool_name, "Write");
    assert_eq!(request.description.as_deref(), Some("hello.txt"));
    assert!(!permissions::is_auto_approved(&request.tool_name));
    let deny = json(&protocol::permission_response(
        &request,
        &PermissionAnswer::Deny {
            message: "Denied by host (experiment)".into(),
        },
    ));
    assert_eq!(&deny, host.last().unwrap());

    let t = transcript_of(&messages);
    let write = t.tool_calls().find(|c| c.name == "Write").unwrap();
    let result = write.result.as_ref().unwrap();
    assert!(result.is_error);
    assert_eq!(result.text, "Denied by host (experiment)");
    assert!(!t.assistant_text().is_empty());
}

#[test]
fn interrupt_ends_the_turn_without_an_error() {
    let messages = incoming("interrupt.jsonl");
    let host = host_lines("interrupt.jsonl");
    assert_eq!(
        json(&protocol::interrupt_request("host-int-1")),
        *host.last().unwrap()
    );
    let acked = messages.iter().any(|m| {
        matches!(m, Incoming::ControlResponse { request_id, error: None, .. } if request_id == "host-int-1")
    });
    assert!(acked);

    let t = transcript_of(&messages);
    let result = t.last_result.as_ref().unwrap();
    assert!(result.is_error && result.interrupted());
    assert!(t.items.contains(&ChatItem::Notice("Interrupted".into())));
    assert!(!t.items.iter().any(|i| matches!(i, ChatItem::Error(_))));
    assert!(!t.running);
    assert!(t.items.iter().all(|i| !matches!(
        i,
        ChatItem::Assistant {
            streaming: true,
            ..
        }
    )));
}

#[test]
fn failed_turn_shows_an_error() {
    let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["boom"],"terminal_reason":"error"}"#;
    let mut t = Transcript::default();
    t.push_user("x");
    t.apply(&parse_line(line).unwrap());
    assert!(
        t.items
            .contains(&ChatItem::Error("Turn failed: boom".into()))
    );
}

#[test]
fn permission_cards_decide_once_and_cancel_on_crash() {
    let messages = incoming("tool_permission_deny.jsonl");
    let request = permission_request(&messages);
    let mut t = Transcript::default();
    t.push_permission(request.clone());
    assert!(t.pending_permission().is_some());
    assert!(t.decide(&request.request_id, Decision::Deny));
    assert!(!t.decide(&request.request_id, Decision::Allow));
    assert!(t.pending_permission().is_none());

    let mut second = request.clone();
    second.request_id = "r2".into();
    t.push_permission(second);
    t.session_ended();
    let decisions: Vec<_> = t
        .items
        .iter()
        .filter_map(|i| match i {
            ChatItem::Permission(c) => c.decision,
            _ => None,
        })
        .collect();
    assert_eq!(decisions, vec![Decision::Deny, Decision::Cancelled]);
}

#[test]
fn unknown_control_requests_are_declined() {
    let line =
        r#"{"type":"control_request","request_id":"q1","request":{"subtype":"hook_callback"}}"#;
    let Incoming::ControlRequest {
        request_id,
        request,
    } = parse_line(line).unwrap()
    else {
        panic!("not a control request");
    };
    assert_eq!(request, HostRequest::Other("hook_callback".into()));
    let reply = json(&protocol::error_response(&request_id, "unsupported"));
    assert_eq!(reply["response"]["subtype"], "error");
    assert_eq!(reply["response"]["request_id"], "q1");
}

// ---- lifecycle against a fake `claude` ----

/// A stand-in for `claude`: logs its arguments, answers `initialize`, and per user message:
/// `crash` exits, `permit` asks to use send_to_agent, `auto` asks to use fleet_status (the
/// supervisor answers), anything else gets a text reply. `FAKE_STUBBORN` ignores end of input.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo "$*" >> "$dir/args.log"
echo $$ > "$dir/pid"
id() { printf '%s' "$1" | sed -E 's/.*"request_id":"([^"]*)".*/\1/'; }
result() { echo '{"type":"result","subtype":"success","is_error":false,"result":"'"$1"'","total_cost_usd":0.01}'; }
while IFS= read -r line; do
  case "$line" in *'"type":"user"'*) printf '%s\n' "$line" >> "$dir/user.log" ;; esac
  case "$line" in
    *'"subtype":"initialize"'*)
      echo '{"type":"control_response","response":{"subtype":"success","request_id":"'"$(id "$line")"'","response":{}}}' ;;
    *'"type":"user"'*crash*) exit 3 ;;
    *'"type":"user"'*permit*)
      echo '{"type":"control_request","request_id":"perm-1","request":{"subtype":"can_use_tool","tool_name":"mcp__myrstack__send_to_agent","input":{"agent":"term:term_1","text":"hi"}}}'
      IFS= read -r answer
      case "$answer" in *'"behavior":"allow"'*) result allowed ;; *) result denied ;; esac ;;
    *'"type":"user"'*auto*)
      echo '{"type":"control_request","request_id":"perm-2","request":{"subtype":"can_use_tool","tool_name":"mcp__myrstack__fleet_status","input":{}}}'
      IFS= read -r answer
      case "$answer" in *'"behavior":"allow"'*) result auto-allowed ;; *) result auto-denied ;; esac ;;
    *'"type":"user"'*)
      echo '{"type":"system","subtype":"init","session_id":"s","model":"fake-model","tools":[]}'
      echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"pong"}]},"parent_tool_use_id":null}'
      result pong ;;
  esac
done
if [ -n "$FAKE_STUBBORN" ]; then
  trap '' TERM
  while :; do sleep 1; done
fi
"#;

struct Fake {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fake {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let bin = root.join("claude");
        std::fs::write(&bin, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { _dir: dir, root }
    }

    fn config(&self, env: &[(&str, &str)]) -> ChatConfig {
        ChatConfig {
            binary: Some(self.root.join("claude")),
            dir: self.root.join("chat"),
            model: None,
            effort: None,
            system_prompt: "test".into(),
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            extra_args: Vec::new(),
        }
    }

    fn args_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("args.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn user_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("user.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn pid(&self) -> String {
        std::fs::read_to_string(self.root.join("pid"))
            .unwrap()
            .trim()
            .to_owned()
    }
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Polls the session until `done` holds, or panics after 10 s.
fn wait_until(chat: &mut ChatSession, what: &str, done: impl Fn(&ChatSession) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(chat) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; state {:?}, items {:?}",
            chat.state,
            chat.transcript.items
        );
        chat.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn last_result(chat: &ChatSession) -> Option<String> {
    chat.transcript
        .last_result
        .as_ref()
        .and_then(|r| r.result.clone())
}

#[test]
fn fake_claude_turn_then_crash_resumes_same_session() {
    let fake = Fake::new();
    let mut chat = ChatSession::spawn(fake.config(&[]), None);
    wait_until(&mut chat, "ready", |c| c.state == SessionState::Ready);
    assert!(chat.send("ping"));
    wait_until(&mut chat, "reply", |c| !c.transcript.running);
    assert_eq!(chat.transcript.assistant_text(), vec!["pong"]);
    assert_eq!(chat.transcript.model.as_deref(), Some("fake-model"));

    let session = session::SessionStore::open(&fake.root.join("chat")).record;
    assert!(session.started);
    assert!(chat.send("crash now"));
    wait_until(&mut chat, "crash", |c| {
        matches!(c.state, SessionState::Crashed { .. })
    });
    assert!(!chat.transcript.running);
    wait_until(&mut chat, "restart", |c| c.state == SessionState::Ready);
    let args = fake.args_log();
    assert_eq!(args.len(), 2, "{args:?}");
    assert!(args[0].contains(&format!("--session-id {}", session.session_id)));
    assert!(args[1].contains(&format!("--resume {}", session.session_id)));
    assert!(args[1].contains("--permission-prompt-tool stdio"));
}

#[test]
fn fake_claude_permissions_ask_or_auto_approve() {
    let fake = Fake::new();
    let mut chat = ChatSession::spawn(fake.config(&[]), None);
    wait_until(&mut chat, "ready", |c| c.state == SessionState::Ready);

    assert!(chat.send("auto"));
    wait_until(&mut chat, "auto result", |c| !c.transcript.running);
    assert_eq!(last_result(&chat).as_deref(), Some("auto-allowed"));
    assert!(chat.transcript.pending_permission().is_none());

    assert!(chat.send("permit please"));
    wait_until(&mut chat, "card", |c| {
        c.transcript.pending_permission().is_some()
    });
    let card = chat.transcript.pending_permission().unwrap().clone();
    assert_eq!(card.request.tool_name, "mcp__myrstack__send_to_agent");
    chat.decide(&card.request.request_id, Decision::Deny);
    wait_until(&mut chat, "denied", |c| !c.transcript.running);
    assert_eq!(last_result(&chat).as_deref(), Some("denied"));

    assert!(chat.send("permit again"));
    wait_until(&mut chat, "card", |c| {
        c.transcript.pending_permission().is_some()
    });
    let id = chat
        .transcript
        .pending_permission()
        .unwrap()
        .request
        .request_id
        .clone();
    chat.decide(&id, Decision::AlwaysAllow);
    wait_until(&mut chat, "allowed", |c| !c.transcript.running);
    assert_eq!(last_result(&chat).as_deref(), Some("allowed"));

    // Remembered for this chat: no card the next time.
    assert!(chat.send("permit a third time"));
    wait_until(&mut chat, "auto allowed", |c| !c.transcript.running);
    assert_eq!(last_result(&chat).as_deref(), Some("allowed"));
    assert!(chat.transcript.pending_permission().is_none());
}

#[test]
fn new_chat_starts_a_fresh_session_and_forgets_allowances() {
    let fake = Fake::new();
    let mut chat = ChatSession::spawn(fake.config(&[]), None);
    wait_until(&mut chat, "ready", |c| c.state == SessionState::Ready);
    assert!(chat.send("ping"));
    wait_until(&mut chat, "reply", |c| !c.transcript.running);
    let first = session::SessionStore::open(&fake.root.join("chat")).record;

    chat.new_chat();
    assert!(chat.transcript.items.is_empty());
    wait_until(&mut chat, "restart", |c| {
        c.state == SessionState::Ready && fake.args_log().len() == 2
    });
    let second = session::SessionStore::open(&fake.root.join("chat")).record;
    assert_ne!(first.session_id, second.session_id);
    assert!(!second.started);
    assert!(fake.args_log()[1].contains(&format!("--session-id {}", second.session_id)));
}

#[test]
fn dropping_the_session_stops_the_process() {
    let fake = Fake::new();
    let mut chat = ChatSession::spawn(fake.config(&[]), None);
    wait_until(&mut chat, "ready", |c| c.state == SessionState::Ready);
    let pid = fake.pid();
    assert!(alive(&pid));
    drop(chat);
    assert!(!alive(&pid), "claude still running after drop");
}

#[test]
fn a_process_ignoring_end_of_input_is_killed_on_drop() {
    let fake = Fake::new();
    let mut chat = ChatSession::spawn(fake.config(&[("FAKE_STUBBORN", "1")]), None);
    wait_until(&mut chat, "ready", |c| c.state == SessionState::Ready);
    let pid = fake.pid();
    let started = Instant::now();
    drop(chat);
    assert!(!alive(&pid), "stubborn claude survived drop");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn missing_binary_reports_unavailable() {
    let fake = Fake::new();
    let mut config = fake.config(&[]);
    config.binary = None;
    let mut chat = ChatSession::spawn(config, None);
    wait_until(&mut chat, "unavailable", |c| {
        matches!(c.state, SessionState::Unavailable(_))
    });
    assert!(!chat.send("hello"));
}

#[test]
fn auto_mode_toggles_mid_session_without_a_restart() {
    let fake = Fake::new();
    let chat_dir = fake.root.join("chat");
    let mut chat = ChatSession::spawn(fake.config(&[]), None);
    assert!(!chat.auto_mode(), "off by default");
    wait_until(&mut chat, "ready", |c| c.state == SessionState::Ready);
    assert!(chat.send("ping"));
    wait_until(&mut chat, "reply", |c| !c.transcript.running);
    let session = session::SessionStore::open(&chat_dir).record;

    // A card waiting when auto mode turns on is answered by it.
    assert!(chat.send("permit please"));
    wait_until(&mut chat, "card", |c| {
        c.transcript.pending_permission().is_some()
    });
    chat.set_auto_mode(true);
    assert!(auto_mode::load(&chat_dir), "saved");
    wait_until(&mut chat, "auto allowed", |c| !c.transcript.running);
    assert_eq!(last_result(&chat).as_deref(), Some("allowed"));
    assert!(chat.transcript.items.iter().any(|i| matches!(
        i,
        ChatItem::Permission(card) if card.decision == Some(Decision::Auto)
    )));
    assert!(
        chat.transcript
            .items
            .contains(&ChatItem::System(auto_mode::notice(true).into()))
    );

    // No card now; the Queen is told with Karl's next message, which shows unprefixed.
    assert!(chat.send("permit again"));
    wait_until(&mut chat, "no card", |c| !c.transcript.running);
    assert_eq!(last_result(&chat).as_deref(), Some("allowed"));
    assert!(chat.transcript.pending_permission().is_none());
    assert!(
        chat.transcript
            .items
            .contains(&ChatItem::User("permit again".into()))
    );
    let users = fake.user_log();
    let told = users.last().unwrap();
    assert!(told.contains("[Myrstack] Auto mode is now ON"), "{told}");
    assert!(told.contains("permit again"), "{told}");
    assert_eq!(
        users
            .iter()
            .filter(|l| l.contains("[Myrstack] Auto mode"))
            .count(),
        1
    );

    // Told once: the next message goes as typed.
    assert!(chat.send("ping"));
    wait_until(&mut chat, "reply", |c| !c.transcript.running);
    assert!(!fake.user_log().last().unwrap().contains("[Myrstack]"));

    // Off again: cards come back and the Queen hears about it.
    chat.set_auto_mode(false);
    assert!(chat.send("permit once more"));
    wait_until(&mut chat, "card", |c| {
        c.transcript.pending_permission().is_some()
    });
    assert!(
        fake.user_log()
            .last()
            .unwrap()
            .contains("[Myrstack] Auto mode is now OFF")
    );
    let id = chat
        .transcript
        .pending_permission()
        .unwrap()
        .request
        .request_id
        .clone();
    chat.decide(&id, Decision::Deny);
    wait_until(&mut chat, "denied", |c| !c.transcript.running);
    assert_eq!(last_result(&chat).as_deref(), Some("denied"));

    // Same process and conversation throughout.
    assert_eq!(fake.args_log().len(), 1, "{:?}", fake.args_log());
    let after = session::SessionStore::open(&chat_dir).record;
    assert_eq!(after.session_id, session.session_id);
    assert!(!auto_mode::load(&chat_dir));
}

#[test]
fn saved_auto_mode_applies_after_a_restart() {
    let fake = Fake::new();
    auto_mode::save(&fake.root.join("chat"), true).unwrap();
    let mut chat = ChatSession::spawn(fake.config(&[]), None);
    assert!(chat.auto_mode());
    wait_until(&mut chat, "ready", |c| c.state == SessionState::Ready);
    assert!(chat.send("permit please"));
    wait_until(&mut chat, "reply", |c| !c.transcript.running);
    assert_eq!(last_result(&chat).as_deref(), Some("allowed"));
    assert!(chat.transcript.pending_permission().is_none());
    assert!(
        fake.user_log()[0].contains("[Myrstack] Auto mode is now ON"),
        "a resumed Queen is reminded"
    );
    // Read-only tools never get the auto marker or a card.
    assert!(chat.send("auto"));
    wait_until(&mut chat, "reply", |c| !c.transcript.running);
    assert_eq!(last_result(&chat).as_deref(), Some("auto-allowed"));
}

#[test]
fn auto_approved_tool_rows_get_the_marker_whenever_the_row_appears() {
    let request = |id: &str, tool_use: &str| PermissionRequest {
        request_id: id.into(),
        tool_name: "mcp__myrstack__press_key".into(),
        display_name: None,
        input: Value::Null,
        tool_use_id: Some(tool_use.into()),
        description: None,
    };
    let tool_use = |id: &str| {
        parse_line(&format!(
            r#"{{"type":"assistant","message":{{"id":"m","content":[{{"type":"tool_use","id":"{id}","name":"mcp__myrstack__press_key","input":{{}}}}]}},"parent_tool_use_id":null}}"#
        ))
        .unwrap()
    };
    let mut t = Transcript::default();
    // Row first, then the approval.
    t.apply(&tool_use("a"));
    t.mark_auto(&request("r1", "a"));
    // Approval first, then the row.
    t.mark_auto(&request("r2", "b"));
    t.apply(&tool_use("b"));
    t.apply(&tool_use("c"));
    let marks: Vec<(String, bool)> = t.tool_calls().map(|c| (c.id.clone(), c.auto)).collect();
    assert_eq!(
        marks,
        vec![("a".into(), true), ("b".into(), true), ("c".into(), false)]
    );
}

#[test]
fn model_change_restarts_with_model_and_resume_keeping_the_transcript() {
    let fake = Fake::new();
    let mut chat = ChatSession::spawn(fake.config(&[]), None);
    wait_until(&mut chat, "ready", |c| c.state == SessionState::Ready);
    assert!(chat.send("ping"));
    wait_until(&mut chat, "reply", |c| !c.transcript.running);
    let session = session::SessionStore::open(&fake.root.join("chat")).record;
    assert!(!chat.set_model(None, None), "unchanged: no restart");

    assert!(chat.set_model(Some("opus".into()), Some("high".into())));
    assert_eq!(chat.model(), Some("opus"));
    assert_eq!(chat.transcript.model.as_deref(), Some("opus"));
    assert!(!chat.can_send(), "no sends while restarting");
    wait_until(&mut chat, "restart", |c| {
        c.state == SessionState::Ready && fake.args_log().len() == 2
    });
    let args = fake.args_log();
    assert!(!args[0].contains("--model"), "{args:?}");
    assert!(args[1].contains(&format!("--resume {}", session.session_id)));
    assert!(args[1].contains("--model opus"), "{args:?}");
    assert!(args[1].contains("--effort high"), "{args:?}");
    assert_eq!(
        chat.transcript.assistant_text(),
        vec!["pong"],
        "transcript kept"
    );
    assert!(chat.transcript.items.iter().any(|i| matches!(
        i,
        ChatItem::System(t) if t.contains("model opus")
    )));

    // The next reply's init reports the resolved model.
    assert!(chat.send("ping"));
    wait_until(&mut chat, "reply", |c| !c.transcript.running);
    assert_eq!(chat.transcript.model.as_deref(), Some("fake-model"));
    assert_eq!(chat.transcript.assistant_text(), vec!["pong", "pong"]);

    // Back to the CLI default: no --model at all.
    assert!(chat.set_model(None, None));
    wait_until(&mut chat, "restart", |c| {
        c.state == SessionState::Ready && fake.args_log().len() == 3
    });
    let args = fake.args_log();
    assert!(!args[2].contains("--model") && !args[2].contains("--effort"));
    assert!(args[2].contains(&format!("--resume {}", session.session_id)));
}

#[test]
fn speech_tap_takes_top_level_text_and_turn_ends() {
    let mut events = Vec::new();
    for m in incoming("hello.jsonl") {
        voice::tap(&m, &mut events);
    }
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            SpeechEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hey Karl! 👋 What can I help you with today?");
    assert!(events.contains(&SpeechEvent::BlockEnd));
    assert_eq!(events.last(), Some(&SpeechEvent::TurnEnd));

    // Subagent text stays silent.
    let sub = parse_line(
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"inner"}},"parent_tool_use_id":"t1"}"#,
    )
    .unwrap();
    let mut events = Vec::new();
    voice::tap(&sub, &mut events);
    assert!(events.is_empty());
}

#[test]
fn voice_messages_carry_the_spoken_style_note_but_show_as_said() {
    let fake = Fake::new();
    let mut chat = ChatSession::spawn(fake.config(&[]), None);
    wait_until(&mut chat, "ready", |c| c.state == SessionState::Ready);
    assert!(!chat.send_voice("   "), "nothing to send");
    assert!(chat.send_voice(" How many agents need input? "));
    assert_eq!(chat.take_speech(), [SpeechEvent::TurnStart { voice: true }]);
    wait_until(&mut chat, "reply", |c| !c.transcript.running);
    assert_eq!(
        chat.transcript.items.first(),
        Some(&ChatItem::VoiceUser("How many agents need input?".into()))
    );
    let sent = fake.user_log();
    assert!(
        sent[0].contains("[Myrstack] Karl said this by voice"),
        "{}",
        sent[0]
    );
    assert!(sent[0].contains("How many agents need input?"));
    assert_eq!(chat.take_speech().last(), Some(&SpeechEvent::TurnEnd));

    // A typed message is not voice and carries no note.
    assert!(chat.send("ping"));
    assert_eq!(
        chat.take_speech(),
        [SpeechEvent::TurnStart { voice: false }]
    );
    wait_until(&mut chat, "reply", |c| !c.transcript.running);
    assert!(!fake.user_log()[1].contains("by voice"));
}

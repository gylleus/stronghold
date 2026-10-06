use super::*;
use crate::envoy::{
    channel::{OutboundMessage, SendReceipt},
    settings::{ApprovalMode, Policy},
    slack::{SlackIo, SlackSender},
};
use crate::review::mark::Origin;
use std::sync::{Arc, Mutex};

fn origin(handle: &str) -> Origin {
    Origin {
        terminal_handle: Some(handle.into()),
        ..Default::default()
    }
}
fn command(handle: &str, method: &str, args: Value) -> EnvoyCommand {
    EnvoyCommand {
        method: method.into(),
        args,
        origin: origin(handle),
    }
}
struct Control<'a>(&'a mut AppEnvoy);
impl crate::mcp::Control for Control<'_> {
    fn call(&mut self, method: &str, args: Value) -> Result<Value, crate::mcp::CallError> {
        let command = EnvoyCommand::parse(method, &args)
            .unwrap()
            .map_err(crate::mcp::CallError::Control)?;
        self.0
            .request(&command)
            .map_err(|e| crate::mcp::CallError::Control(ControlError::invalid_params(e)))
    }
}

#[derive(Clone)]
struct Fake {
    sent: Arc<Mutex<Vec<OutboundMessage>>>,
    calls: Arc<Mutex<Vec<String>>>,
    receipt_ts: String,
    bridge: PathBuf,
    event: InboundEvent,
}
impl SlackSender for Fake {
    fn send(&mut self, message: &OutboundMessage) -> Result<SendReceipt, String> {
        self.sent.lock().unwrap().push(message.clone());
        let line = json!({"type":"reply","channel":self.event.conversation,"ts":self.event.ts,"thread_ts":self.event.thread_ts,"user":self.event.user,"user_name":self.event.user_name,"text":self.event.text,"permalink":self.event.permalink});
        std::fs::write(self.bridge.join("channels/C123.jsonl"), format!("{line}\n")).unwrap();
        Ok(SendReceipt {
            conversation: message.conversation.clone(),
            ts: self.receipt_ts.clone(),
        })
    }
}
impl SlackIo for Fake {
    fn api(&mut self, method: &str, _: &[(&str, String)]) -> Result<Value, String> {
        self.calls.lock().unwrap().push(method.into());
        Ok(if method == "conversations.replies" {
            json!({"ok":true,"messages":[{"ts":"2.000001","thread_ts":self.event.thread_ts,"user":"UHISTORY","text":"Private old history","permalink":"https://example.slack.com/archives/C123/p2000001"},{"ts":self.event.ts,"thread_ts":self.event.thread_ts,"user":self.event.user,"text":self.event.text}]})
        } else if method == "chat.getPermalink" {
            json!({"ok":true,"permalink":self.event.permalink})
        } else {
            json!({"ok":true,"messages":{"matches":[],"paging":{"pages":1}}})
        })
    }
    fn subscribe(&mut self, dir: &std::path::Path, _: &str) -> Result<(), String> {
        assert_eq!(dir, self.bridge);
        Ok(())
    }
    fn thread_subscription(
        &mut self,
        dir: &std::path::Path,
        _: &crate::envoy::requests::Thread,
        label: Option<&str>,
    ) -> Result<(), String> {
        self.calls.lock().unwrap().push(
            if label.is_some() {
                "thread_add"
            } else {
                "thread_remove"
            }
            .into(),
        );
        assert_eq!(dir, self.bridge);
        Ok(())
    }
}
fn fixture(dir: &std::path::Path) -> Fake {
    let bridge = dir.join("bridge");
    std::fs::create_dir_all(bridge.join("channels")).unwrap();
    std::fs::write(bridge.join("heartbeat"), jiff::Timestamp::now().to_string()).unwrap();
    let root = format!("{}.000001", now_ms() / 1000 + 1);
    Fake {
        sent: Default::default(),
        calls: Default::default(),
        receipt_ts: root.clone(),
        bridge,
        event: InboundEvent {
            channel_kind: "slack".into(),
            conversation: "C123".into(),
            ts: format!("{}.000002", now_ms() / 1000 + 1),
            thread_ts: Some(root),
            user: "UOTHER".into(),
            user_name: "Avery".into(),
            text: "Fixture reply, ready.".into(),
            kind: crate::envoy::channel::EventKind::ThreadReply,
            permalink: Some("https://example.slack.com/archives/C123/p1790000000000002".into()),
        },
    }
}
fn wait(core: &mut AppEnvoy, ready: impl Fn(&AppEnvoy) -> bool) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready(core) {
        assert!(
            std::time::Instant::now() < until,
            "worker stalled: {:?}",
            core.error
        );
        core.poll_updates();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
fn submit(core: &mut AppEnvoy, ask: &str) -> String {
    let result = crate::mcp::envoy::call(
        &mut Control(core),
        &origin("ant"),
        "reach_out",
        &json!({"to":"Avery agent","ask":ask,"why":"Fixture review"}),
    )
    .unwrap();
    assert!(!result.is_error, "{}", result.text);
    result.structured.unwrap()["request_id"]
        .as_str()
        .unwrap()
        .into()
}
fn route_command(core: &mut AppEnvoy, command: &EnvoyCommand) -> Result<Value, String> {
    crate::envoy::authorize(core, &command.origin)?;
    let conversation = command.args["conversation"].as_str().unwrap().to_owned();
    assert!(
        !conversation.starts_with('D'),
        "DM fixtures must use worker verification"
    );
    core.route(
        &command.args,
        &crate::envoy::slack::threads::ResolvedDestination {
            conversation,
            person: false,
        },
    )
}

fn route(core: &mut AppEnvoy, id: &str) -> Draft {
    let result = route_command(
        core,
        &command(
            "envoy",
            "envoy_route",
            json!({"id":id,"conversation":"C123","destination_kind":"agent","rationale":"Fixture"}),
        ),
    )
    .unwrap();
    serde_json::from_value(result["draft"].clone()).unwrap()
}

#[test]
fn gateway_mcp_to_event_route_approval_fast_reply_and_safe_terminal_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fixture(dir.path());
    let sent = fake.sent.clone();
    let mut core = AppEnvoy::at(
        dir.path().into(),
        &egui::Context::default(),
        fake.clone(),
        fake.bridge.clone(),
    );
    core.set_enabled(
        "envoy".into(),
        EnvoySettings {
            dry_run: false,
            channels: vec!["C123".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let ask = "sha abc012def; count 42; 2026-10-06T12:34:56Z; https://example.test/a?x=2\n```rust\nlet x = 17;\n```\n\n";
    let id = submit(&mut core, ask);
    let (action, record) = core.request_event("envoy").unwrap();
    assert!(
        matches!(&action,Action::Send{input,..} if input.text.as_ref().unwrap().contains("Envoy event: request from ant"))
    );
    core.gateway_actions.insert(1, record);
    core.gateway_done(
        &ActionResult {
            id: 1,
            action,
            outcome: Ok(()),
            note: None,
        },
        |_| {},
    );
    assert!(core.request_event("envoy").is_none());
    let draft = route(&mut core, &id);
    assert_eq!(draft.text, format!("Request from ant:\n\n{ask}"));
    assert!(
        core.queue()
            .unwrap()
            .edit(&draft.id, "rewritten", 0)
            .is_err()
    );
    assert!(
        core.envoy_approve(&draft.id, Some("rewritten"), false, |_| false)
            .is_err()
    );
    core.panel_action(
        crate::ui::envoy::DraftAction::Approve {
            id: draft.id.clone(),
            text: draft.text.clone(),
        },
        false,
        |_| false,
    )
    .1
    .unwrap();
    wait(&mut core, |c| !c.requests()[0].deliveries.is_empty());
    assert_eq!(
        sent.lock().unwrap()[0].text,
        format!(
            "Request from ant:\n\n{ask}\n\n{}",
            crate::envoy::drafts::ATTRIBUTION
        )
    );
    let delivery = core.requests()[0].deliveries[0].id.clone();
    let terminal = crate::orca::Terminal {
        agent_identity: Some(crate::orca::AgentIdentity::Codex),
        writable: true,
        preview: Some("OpenAI Codex\n›".into()),
        ..Default::default()
    };
    assert!(!can_forward(&terminal, false, false));
    assert!(can_forward(&terminal, true, false));
    let (action, record) = core.reply_action(&id, &delivery, "ant").unwrap();
    assert!(
        matches!(&action,Action::Send{handle,input} if handle=="ant" && input.text.as_ref().unwrap().contains("not Karl's instruction") && input.text.as_ref().unwrap().contains(fake.event.permalink.as_ref().unwrap()))
    );
    core.gateway_actions.insert(2, record);
    let mut traffic = Vec::new();
    core.gateway_done(
        &ActionResult {
            id: 2,
            action,
            outcome: Ok(()),
            note: None,
        },
        |m| traffic.push(m),
    );
    assert_eq!(traffic.len(), 2);
    assert!(core.relays().is_empty());
    assert!(core.requests()[0].deliveries[0].delivered);
    let reloaded = Requests::load(dir.path().join(crate::envoy::requests::REQUESTS_FILE)).unwrap();
    assert!(reloaded.all()[0].deliveries[0].delivered);
    let status = core
        .request(&command(
            "ant",
            "reach_out_status",
            json!({"request_id":id}),
        ))
        .unwrap();
    assert_eq!(
        status["requests"][0]["deliveries"][0]["event"]["permalink"],
        fake.event.permalink.as_deref().unwrap()
    );
    assert!(
        core.request(&command(
            "other",
            "reach_out_status",
            json!({"request_id":id})
        ))
        .is_err()
    );
    assert!(
        core.request(&command("ant", "envoy_request", json!({"id":id})))
            .is_err()
    );
}

#[test]
fn auto_veto_cancel_dry_run_and_restart_keep_manual_drafts_manual() {
    for dry_run in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppEnvoy::load(dir.path().into());
        let (worker, commands, _updates) = Worker::test_pair();
        core.worker = Some(worker);
        core.handle = Some("envoy".into());
        core.settings = EnvoySettings {
            dry_run,
            policies: vec![Policy {
                destination: "*".into(),
                mode: ApprovalMode::Auto { veto_secs: 60 },
            }],
            ..Default::default()
        };
        let id = submit(&mut core, "auto ask");
        let draft = route(&mut core, &id);
        let at = draft.auto_at_ms.unwrap();
        core.auto_approve(at - 1).unwrap();
        assert_eq!(core.drafts()[0].status, DraftStatus::Pending);
        core.auto_approve(at).unwrap();
        assert_eq!(
            core.drafts()[0].status,
            if dry_run {
                DraftStatus::DryRunSent
            } else {
                DraftStatus::Approved
            }
        );
        let sends: Vec<_> = commands
            .try_iter()
            .filter_map(|w| {
                if let Work::Send { message, .. } = w {
                    Some(message)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(sends.len(), usize::from(!dry_run));
        if !dry_run {
            assert!(
                sends[0]
                    .text
                    .ends_with(crate::envoy::drafts::AUTO_ATTRIBUTION)
            );
        }
        let id = submit(&mut core, "cancel ask");
        let draft = route(&mut core, &id);
        core.panel_action(
            crate::ui::envoy::DraftAction::Reject(draft.id.clone()),
            false,
            |_| false,
        )
        .1
        .unwrap();
        core.auto_approve(i64::MAX).unwrap();
        assert_eq!(
            core.queue().unwrap().get(&draft.id).unwrap().status,
            DraftStatus::Rejected
        );
        let id = submit(&mut core, "paused ask");
        let draft = route(&mut core, &id);
        core.queue().unwrap().schedule(&draft.id, None).unwrap();
        let mut loaded = AppEnvoy::load(dir.path().into());
        let (worker, _commands, _updates) = Worker::test_pair();
        loaded.worker = Some(worker);
        loaded
            .set_enabled("envoy".into(), core.settings.clone())
            .unwrap();
        assert_eq!(
            loaded.queue().unwrap().get(&draft.id).unwrap().auto_at_ms,
            None
        );
    }
}

#[test]
fn followups_wait_for_root_and_cannot_upgrade_simulated_root_to_live() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, _commands, _updates) = Worker::test_pair();
    core.worker = Some(worker);
    core.handle = Some("envoy".into());
    let id = submit(&mut core, "first");
    core.request(&command(
        "ant",
        "reach_out",
        json!({"to":"Avery agent","ask":"follow","why":"review","request_id":id}),
    ))
    .unwrap();
    assert!(route_command(&mut core, &command("envoy","envoy_route",json!({"id":id,"turn_id":2,"conversation":"C123","destination_kind":"agent","rationale":"test"}))).is_err());
    let first=route_command(&mut core, &command("envoy","envoy_route",json!({"id":id,"turn_id":1,"conversation":"C123","destination_kind":"agent","rationale":"test"}))).unwrap();
    core.envoy_approve(first["draft"]["id"].as_str().unwrap(), None, false, |_| {
        false
    })
    .unwrap();
    core.reconcile_requests().unwrap();
    let follow = route(&mut core, &id);
    core.settings.dry_run = false;
    assert!(
        core.envoy_approve(&follow.id, None, false, |_| false)
            .unwrap_err()
            .contains("confirmed live thread")
    );
}

#[test]
fn failed_watch_save_retries_and_failed_policy_application_blocks_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, commands, updates) = Worker::test_pair();
    core.worker = Some(worker);
    core.reconcile_requests().unwrap();
    let Work::Watches(watches) = commands.recv().unwrap() else {
        panic!()
    };
    updates
        .send(Update::Watches {
            watches,
            result: Err("disk failure".into()),
        })
        .unwrap();
    core.poll_updates();
    assert!(!core.watches_pending);
    assert!(core.watches.is_none());
    core.watch_retry_ms = 0;
    core.reconcile_requests().unwrap();
    assert!(matches!(commands.recv().unwrap(), Work::Watches(_)));
    core.handle = Some("envoy".into());
    let id = submit(&mut core, "blocked");
    let draft = route(&mut core, &id);
    core.block_dispatch(true);
    assert!(
        core.envoy_approve(&draft.id, None, false, |_| false)
            .is_err()
    );
    assert!(core.settings.dry_run);
}

#[test]
fn reply_fallback_rejects_reused_requester_and_coordinator_incarnations() {
    use crate::orca::Terminal;
    let mut data = crate::fleet::FleetData {
        terminals: vec![
            Terminal {
                handle: "ant".into(),
                incarnation_id: Some("ant-1".into()),
                connected: true,
                writable: true,
                ..Default::default()
            },
            Terminal {
                handle: "coordinator".into(),
                incarnation_id: Some("coord-1".into()),
                connected: true,
                writable: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let snapshot = crate::model::FleetSnapshot::build(&data, 0);
    let mut who = Requester::from_origin(origin("ant")).unwrap();
    who.incarnation_id = Some("ant-1".into());
    who.coordinator = Some("coordinator".into());
    who.coordinator_incarnation_id = Some("coord-1".into());
    assert_eq!(
        gateway::reply_target(&snapshot, Some(&data), &who).as_deref(),
        Some("ant")
    );
    data.terminals[0].incarnation_id = Some("ant-2".into());
    assert_eq!(
        gateway::reply_target(&snapshot, Some(&data), &who).as_deref(),
        Some("coordinator")
    );
    data.terminals[1].incarnation_id = Some("coord-2".into());
    assert!(gateway::reply_target(&snapshot, Some(&data), &who).is_none());
}

#[test]
fn policy_precedence_zero_veto_secret_scan_and_cancel_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, commands, _updates) = Worker::test_pair();
    core.worker = Some(worker);
    core.handle = Some("envoy".into());
    core.settings.policies = vec![
        Policy {
            destination: "*".into(),
            mode: ApprovalMode::Auto { veto_secs: 0 },
        },
        Policy {
            destination: "dm".into(),
            mode: ApprovalMode::Approve,
        },
        Policy {
            destination: "D123".into(),
            mode: ApprovalMode::Auto { veto_secs: 5 },
        },
    ];
    assert_eq!(core.settings.approval("D456"), ApprovalMode::Approve);
    assert_eq!(
        core.settings.approval("D123"),
        ApprovalMode::Auto { veto_secs: 5 }
    );
    let id = submit(&mut core, "zero veto");
    let draft = route(&mut core, &id);
    core.auto_approve(draft.auto_at_ms.unwrap()).unwrap();
    assert_eq!(core.drafts()[0].status, DraftStatus::DryRunSent);
    assert!(!commands.try_iter().any(|w| matches!(w, Work::Send { .. })));
    let id = submit(&mut core, "ghp_123456789012345678901234567890");
    assert!(
        route_command(&mut core, &command(
            "envoy",
            "envoy_route",
            json!({"id":id,"conversation":"C123","destination_kind":"agent","rationale":"fixture"})
        ))
        .is_err()
    );
    let id = submit(&mut core, "manual after change");
    let draft = route(&mut core, &id);
    let mut settings = core.settings.clone();
    settings.policies.clear();
    core.configure(settings).unwrap();
    assert_eq!(
        core.queue().unwrap().get(&draft.id).unwrap().auto_at_ms,
        None
    );
}

#[test]
fn failed_root_can_be_resolved_or_declined_without_stranding_followups() {
    for decline in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppEnvoy::load(dir.path().into());
        let (worker, _commands, _updates) = Worker::test_pair();
        core.worker = Some(worker);
        core.handle = Some("envoy".into());
        core.settings.dry_run = false;
        let id = submit(&mut core, "unknown outcome");
        let draft = route(&mut core, &id);
        core.envoy_approve(&draft.id, None, false, |_| false)
            .unwrap();
        core.queue()
            .unwrap()
            .finish(&draft.id, Err("Outcome unknown".into()), 0)
            .unwrap();
        core.reconcile_requests().unwrap();
        core.request(&command(
            "ant",
            "reach_out",
            json!({"to":"Avery agent","ask":"follow","why":"review","request_id":id}),
        ))
        .unwrap();
        if decline {
            core.request(&command(
                "envoy",
                "envoy_decline",
                json!({"id":id,"turn_id":1,"reason":"Do not retry"}),
            ))
            .unwrap();
            core.reconcile_requests().unwrap();
            assert!(core.requests()[0].settled);
        } else {
            let fake = fixture(dir.path());
            core.inbox.insert(fake.event.clone());
            core.request(&command(
                "envoy",
                "envoy_link_reply",
                json!({"request_id":id,"event_id":fake.event.id()}),
            ))
            .unwrap();
            let follow = route(&mut core, &id);
            assert_eq!(follow.thread_ts, fake.event.thread_ts);
        }
    }
}

#[test]
fn request_status_and_followups_reject_a_reused_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let mut owner = Requester::from_origin(origin("ant")).unwrap();
    owner.incarnation_id = Some("first".into());
    let ask = command(
        "ant",
        "reach_out",
        json!({"to":"Avery","ask":"first","why":"review"}),
    );
    let accepted = core.gateway_request(&ask, owner.clone()).unwrap();
    let id = accepted["request_id"].as_str().unwrap();
    let status = command("ant", "reach_out_status", json!({"request_id":id}));
    assert!(core.gateway_request(&status, owner.clone()).is_ok());
    owner.incarnation_id = Some("replacement".into());
    assert!(core.gateway_request(&status, owner.clone()).is_err());
    assert!(
        core.gateway_request(
            &command(
                "ant",
                "reach_out",
                json!({"to":"Avery","ask":"follow","why":"review","request_id":id})
            ),
            owner
        )
        .is_err()
    );
    let missing = EnvoyCommand {
        method: "reach_out".into(),
        args: ask.args,
        origin: Origin::default(),
    };
    assert!(core.request(&missing).is_err());
}

#[test]
fn pending_drafts_cannot_send_to_a_different_canonical_thread() {
    for follow_up in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppEnvoy::load(dir.path().into());
        let (worker, commands, _updates) = Worker::test_pair();
        core.worker = Some(worker);
        core.handle = Some("envoy".into());
        let id = submit(&mut core, "first");
        let mut draft = route(&mut core, &id);
        if follow_up {
            core.envoy_approve(&draft.id, None, false, |_| false)
                .unwrap();
            core.reconcile_requests().unwrap();
            core.request(&command(
                "ant",
                "reach_out",
                json!({"to":"Avery agent","ask":"follow","why":"review","request_id":id}),
            ))
            .unwrap();
            draft = route(&mut core, &id);
        }
        let fake = fixture(dir.path());
        core.inbox.insert(fake.event.clone());
        core.request(&command(
            "envoy",
            "envoy_link_reply",
            json!({"request_id":id,"event_id":fake.event.id()}),
        ))
        .unwrap();
        core.settings.dry_run = false;
        assert!(
            core.envoy_approve(&draft.id, None, false, |_| false)
                .unwrap_err()
                .contains("conflicts with the confirmed request thread")
        );
        assert!(!commands.try_iter().any(|w| matches!(w, Work::Send { .. })));
    }
}

#[test]
fn no_reply_root_does_not_settle_an_accepted_followup() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("envoy".into());
    let accepted = core
        .request(&command(
            "ant",
            "reach_out",
            json!({"to":"Avery agent","ask":"first","why":"review","expect_reply":false}),
        ))
        .unwrap();
    let id = accepted["request_id"].as_str().unwrap();
    core.request(&command(
        "ant",
        "reach_out",
        json!({"to":"Avery agent","ask":"follow","why":"review","request_id":id}),
    ))
    .unwrap();
    let routed = route_command(&mut core, &command("envoy", "envoy_route", json!({"id":id,"turn_id":1,"conversation":"C123","destination_kind":"agent","rationale":"test"}))).unwrap();
    let draft_id = routed["draft"]["id"].as_str().unwrap();
    core.queue()
        .unwrap()
        .begin_approval(draft_id, None, false, 0)
        .unwrap();
    core.queue()
        .unwrap()
        .finish_receipt(
            draft_id,
            Ok(SendReceipt {
                conversation: "C123".into(),
                ts: "1.000001".into(),
            }),
            1,
        )
        .unwrap();
    core.reconcile_requests().unwrap();
    assert!(!core.requests()[0].settled);
    assert_eq!(route(&mut core, id).thread_ts.as_deref(), Some("1.000001"));
}

#[test]
fn owned_close_is_dry_run_safe_and_retries_failed_cleanup() {
    use crate::envoy::requests::Thread;
    use crate::ui::envoy::DraftAction;
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, commands, updates) = Worker::test_pair();
    core.worker = Some(worker);
    let thread = Thread {
        conversation: "C123".into(),
        ts: "1.000001".into(),
    };
    core.handle = Some("envoy".into());
    let id = submit(&mut core, "Owned request");
    let draft = route(&mut core, &id);
    core.queue()
        .unwrap()
        .begin_approval(&draft.id, None, false, 0)
        .unwrap();
    core.queue()
        .unwrap()
        .finish_receipt(
            &draft.id,
            Ok(SendReceipt {
                conversation: thread.conversation.clone(),
                ts: thread.ts.clone(),
            }),
            1,
        )
        .unwrap();
    core.reconcile_requests().unwrap();
    core.panel_action(DraftAction::Keep(id.clone()), false, |_| false)
        .1
        .unwrap();
    assert!(core.requests()[0].orphan_kept);
    core.panel_action(DraftAction::Settle(id.clone()), false, |_| false)
        .1
        .unwrap();
    assert!(core.requests()[0].close_requested);
    assert!(!core.requests()[0].settled);
    assert!(!commands.try_iter().any(|w| matches!(w, Work::Remove(_))));
    core.settings.dry_run = false;
    core.panel_action(DraftAction::Settle(id), false, |_| false)
        .1
        .unwrap();
    assert!(core.requests()[0].settled);
    assert!(commands.try_iter().any(|w| matches!(w, Work::Remove(_))));
    updates
        .send(Update::Removed {
            thread: thread.clone(),
            result: Err("fixture failure".into()),
        })
        .unwrap();
    core.poll_updates();
    assert!(core.requests()[0].close_requested);
    core.cleanup_retry_ms = 0;
    core.reconcile_requests().unwrap();
    assert!(commands.try_iter().any(|w| matches!(w, Work::Remove(_))));
    updates
        .send(Update::Removed {
            thread,
            result: Ok(()),
        })
        .unwrap();
    core.poll_updates();
    assert!(!core.requests()[0].close_requested);
}

#[test]
fn completed_dry_run_without_a_thread_can_settle_locally() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("envoy".into());
    let id = submit(&mut core, "local simulation");
    let draft = route(&mut core, &id);
    core.envoy_approve(&draft.id, None, false, |_| false)
        .unwrap();
    core.reconcile_requests().unwrap();
    assert!(core.requests()[0].thread.is_none());
    core.panel_action(crate::ui::envoy::DraftAction::Settle(id), false, |_| false)
        .1
        .unwrap();
    assert!(core.requests()[0].settled);
    assert!(!core.requests()[0].close_requested);
}

#[test]
fn invalid_adoption_reply_does_not_bind_owner_before_retry() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let fake = fixture(dir.path());
    core.requests_mut()
        .unwrap()
        .adopt(
            crate::envoy::requests::Thread {
                conversation: fake.event.conversation.clone(),
                ts: fake.event.thread_ts.clone().unwrap(),
            },
            "Orphan".into(),
            0,
        )
        .unwrap();
    let id = core.requests()[0].id.clone();
    let who = Requester::from_origin(origin("ant")).unwrap();
    assert!(
        core.bind_requester(&json!({"request_id":id,"event_id":"missing"}), who.clone())
            .is_err()
    );
    assert!(core.requests()[0].requester.is_none());
    let mut wrong = fake.event.clone();
    wrong.conversation = "D456".into();
    core.inbox.insert(wrong.clone());
    assert!(
        core.bind_requester(&json!({"request_id":id,"event_id":wrong.id()}), who.clone())
            .is_err()
    );
    assert!(core.requests()[0].requester.is_none());
    core.inbox.insert(fake.event.clone());
    core.bind_requester(&json!({"request_id":id,"event_id":fake.event.id()}), who)
        .unwrap();
    assert!(core.requests()[0].requester.is_some());
}

#[test]
fn foreign_thread_hint_cannot_watch_or_disclose_history_before_real_send() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = fixture(dir.path());
    fake.event.thread_ts = Some("1.000001".into());
    let calls = fake.calls.clone();
    let mut core = AppEnvoy::at(
        dir.path().into(),
        &egui::Context::default(),
        fake.clone(),
        fake.bridge.clone(),
    );
    core.set_enabled(
        "envoy".into(),
        EnvoySettings {
            dry_run: false,
            channels: vec![],
            ..Default::default()
        },
    )
    .unwrap();
    let accepted = core.request(&command("ant","reach_out",json!({"to":"Avery","ask":"A private thread hint","why":"review","thread":"https://example.slack.com/archives/C123/p1000001"}))).unwrap();
    let id = accepted["request_id"].as_str().unwrap().to_owned();
    core.reconcile_requests().unwrap();
    wait(&mut core, |c| {
        c.watches == Some(vec![]) && c.bridge == BridgeStatus::Ok
    });
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "thread_add" || m == "conversations.replies")
    );
    core.requests_mut()
        .unwrap()
        .reply(&id, &fake.event)
        .unwrap();
    assert!(core.requests()[0].deliveries.is_empty());
    let draft = route(&mut core, &id);
    core.reconcile_requests().unwrap();
    assert_eq!(core.watches, Some(vec![]));
    core.envoy_approve(&draft.id, None, false, |_| false)
        .unwrap();
    wait(&mut core, |c| !c.requests()[0].deliveries.is_empty());
    assert!(calls.lock().unwrap().iter().any(|m| m == "thread_add"));
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "conversations.replies")
    );
    assert_eq!(
        core.requests()[0].reply_after_us,
        crate::envoy::inbox::timestamp_us(&fake.receipt_ts)
    );
    assert_eq!(core.requests()[0].deliveries.len(), 1);
    assert_eq!(
        core.requests()[0].deliveries[0].event.as_ref().unwrap().ts,
        fake.event.ts
    );
    let reloaded = Requests::load(dir.path().join(crate::envoy::requests::REQUESTS_FILE)).unwrap();
    assert_eq!(reloaded.all()[0].deliveries.len(), 1);
}

#[test]
fn dry_run_and_explicit_link_never_authorize_a_request_watch() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, commands, _updates) = Worker::test_pair();
    core.worker = Some(worker);
    core.handle = Some("envoy".into());
    let id = submit(&mut core, "simulate");
    let draft = route(&mut core, &id);
    core.envoy_approve(&draft.id, None, false, |_| false)
        .unwrap();
    let fake = fixture(dir.path());
    core.inbox.insert(fake.event.clone());
    core.request(&command(
        "envoy",
        "envoy_link_reply",
        json!({"request_id":id,"event_id":fake.event.id()}),
    ))
    .unwrap();
    core.settings.dry_run = false;
    core.reconcile_requests().unwrap();
    assert!(core.requests()[0].reply_cutoff().is_none());
    assert!(core.requests()[0].deliveries.is_empty());
    assert!(
        commands
            .try_iter()
            .all(|w| !matches!(w,Work::Watches(watches) if !watches.is_empty()))
    );
}

#[test]
fn adopted_close_never_removes_the_borrowed_subscription() {
    for dry_run in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppEnvoy::load(dir.path().into());
        let (worker, commands, _updates) = Worker::test_pair();
        core.worker = Some(worker);
        core.settings.dry_run = dry_run;
        core.requests_mut()
            .unwrap()
            .adopt(
                crate::envoy::requests::Thread {
                    conversation: "D123".into(),
                    ts: "1.000001".into(),
                },
                "External owner".into(),
                0,
            )
            .unwrap();
        core.reconcile_requests().unwrap();
        assert!(
            commands
                .try_iter()
                .any(|w| matches!(w,Work::Watches(watches) if watches[0].borrowed))
        );
        let id = core.requests()[0].id.clone();
        core.panel_action(crate::ui::envoy::DraftAction::Settle(id), false, |_| false)
            .1
            .unwrap();
        assert!(core.requests()[0].settled);
        assert!(!core.requests()[0].close_requested);
        assert!(!commands.try_iter().any(|w| matches!(w, Work::Remove(_))));
    }
}

#[test]
fn injected_person_label_cannot_rewrite_channel_asks_and_rewrites_stay_manual() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("envoy".into());
    core.settings.policies = vec![Policy {
        destination: "*".into(),
        mode: ApprovalMode::Auto { veto_secs: 0 },
    }];
    let id = submit(&mut core, "exact original");
    let mut args = json!({"id":id,"conversation":"C123","thread_ts":"1.000001","destination_kind":"person","draft_text":"malicious rewrite","rationale":"test"});
    let channel = crate::envoy::slack::threads::ResolvedDestination {
        conversation: "C123".into(),
        person: false,
    };
    assert!(
        core.route(&args, &channel)
            .unwrap_err()
            .contains("verified human DM")
    );
    args["conversation"] = json!("D123");
    let bot = crate::envoy::slack::threads::ResolvedDestination {
        conversation: "D123".into(),
        person: false,
    };
    assert!(core.route(&args, &bot).is_err());
    let human = crate::envoy::slack::threads::ResolvedDestination {
        conversation: "D123".into(),
        person: true,
    };
    let result = core.route(&args, &human).unwrap();
    let draft = result["draft"]["id"].as_str().unwrap();
    assert!(
        core.queue()
            .unwrap()
            .get(draft)
            .unwrap()
            .auto_at_ms
            .is_none()
    );
    assert!(core.queue().unwrap().schedule(draft, Some(0)).is_err());
    core.auto_approve(i64::MAX).unwrap();
    assert_eq!(core.drafts()[0].status, DraftStatus::Pending);
}

#[test]
fn restart_pauses_automatic_drafts_and_notifies_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("envoy".into());
    core.settings.policies = vec![Policy {
        destination: "*".into(),
        mode: ApprovalMode::Auto { veto_secs: 0 },
    }];
    let id = submit(&mut core, "restart me");
    let draft = route(&mut core, &id);
    assert!(draft.auto_at_ms.is_some());
    let mut restarted = AppEnvoy::load(dir.path().into());
    let (worker, commands, _updates) = Worker::test_pair();
    restarted.worker = Some(worker);
    restarted
        .set_enabled("envoy".into(), core.settings.clone())
        .unwrap();
    let (notifications, _) = restarted.poll_updates();
    assert_eq!(notifications.len(), 1);
    assert!(restarted.poll_updates().0.is_empty());
    assert_eq!(restarted.drafts()[0].status, DraftStatus::Pending);
    assert!(!restarted.drafts()[0].automatic);
    assert!(restarted.drafts()[0].auto_at_ms.is_none());
    assert!(!commands.try_iter().any(|w| matches!(w, Work::Send { .. })));
}

#[test]
fn rejected_root_is_not_reapplied_when_routing_identical_retry() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("envoy".into());
    let id = submit(&mut core, "retry after review");
    route(&mut core, &id);
    core.request(&command(
        "envoy",
        "envoy_decline",
        json!({"id":id,"reason":"Try again"}),
    ))
    .unwrap();
    let accepted=core.request(&command("ant","reach_out",json!({"to":"Avery agent","ask":"retry after review","why":"Review again","request_id":id}))).unwrap();
    assert_eq!(accepted["merged"], false);
    let draft = route(&mut core, &id);
    assert_eq!(draft.request.unwrap().turn_id, 2);
    assert_eq!(draft.status, DraftStatus::Pending);
    assert!(!core.requests()[0].settled);
}

#[test]
fn adoption_collision_keeps_hint_unauthorized_and_borrows_the_shared_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, commands, updates) = Worker::test_pair();
    core.worker = Some(worker);
    core.handle = Some("envoy".into());
    core.settings.dry_run = false;
    let accepted=core.request(&command("ant","reach_out",json!({"to":"Avery","ask":"hint","why":"review","thread":"https://example.slack.com/archives/C123/p1000001"}))).unwrap();
    let id = accepted["request_id"].as_str().unwrap().to_owned();
    let thread = crate::envoy::requests::Thread {
        conversation: "C123".into(),
        ts: "1.000001".into(),
    };
    core.requests_mut()
        .unwrap()
        .adopt(thread.clone(), "External label".into(), 0)
        .unwrap();
    assert_eq!(core.requests().len(), 2);
    assert!(core.requests()[0].requester.is_some());
    assert!(core.requests()[0].reply_cutoff().is_none());
    assert!(core.requests()[1].requester.is_none());
    core.reconcile_requests().unwrap();
    let Work::Watches(watches) = commands.recv().unwrap() else {
        panic!()
    };
    assert_eq!(watches.len(), 1);
    assert!(watches[0].borrowed);
    updates
        .send(Update::Watches {
            watches,
            result: Ok(()),
        })
        .unwrap();
    core.poll_updates();
    let draft = route(&mut core, &id);
    core.queue()
        .unwrap()
        .begin_approval(&draft.id, None, false, 0)
        .unwrap();
    core.queue()
        .unwrap()
        .finish_receipt(
            &draft.id,
            Ok(SendReceipt {
                conversation: "C123".into(),
                ts: "3.000001".into(),
            }),
            1,
        )
        .unwrap();
    core.reconcile_requests().unwrap();
    let emitted: Vec<_> = commands.try_iter().collect();
    assert!(emitted.iter().any(|w|matches!(w,Work::Watches(watches) if watches.len()==2 && watches.iter().all(|w|w.borrowed))));
    let ids: Vec<_> = core.requests().iter().map(|r| r.id.clone()).collect();
    for id in ids {
        core.panel_action(crate::ui::envoy::DraftAction::Settle(id), false, |_| false)
            .1
            .unwrap();
    }
    assert!(
        core.requests()
            .iter()
            .all(|r| r.settled && !r.close_requested)
    );
    assert!(!commands.try_iter().any(|w| matches!(w, Work::Remove(_))));
}

#[test]
fn declined_unsent_thread_hint_cannot_remove_a_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, commands, _updates) = Worker::test_pair();
    core.worker = Some(worker);
    core.handle = Some("envoy".into());
    core.settings.dry_run = false;
    let accepted=core.request(&command("ant","reach_out",json!({"to":"Avery","ask":"hint","why":"review","thread":"https://example.slack.com/archives/C123/p1000001"}))).unwrap();
    let id = accepted["request_id"].as_str().unwrap().to_owned();
    core.request(&command(
        "envoy",
        "envoy_decline",
        json!({"id":id,"reason":"No"}),
    ))
    .unwrap();
    core.panel_action(crate::ui::envoy::DraftAction::Settle(id), false, |_| false)
        .1
        .unwrap();
    assert!(!core.requests()[0].close_requested);
    assert!(!commands.try_iter().any(|w| matches!(w, Work::Remove(_))));
}

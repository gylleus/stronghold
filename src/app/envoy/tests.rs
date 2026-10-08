use super::*;
use crate::orca::{AgentIdentity, Terminal};
#[test]
fn forwarding_waits_for_idle_empty_composer_and_live_agent() {
    let mut t = Terminal {
        agent_identity: Some(AgentIdentity::Claude),
        writable: true,
        preview: Some("Claude Code\n❯ prior task\nResponse\n❯\n? for shortcuts".into()),
        ..Terminal::default()
    };
    assert!(can_forward(&t, true, false));
    assert!(!can_forward(&t, false, false));
    assert!(!can_forward(&t, true, true));
    t.preview = Some("Claude Code\n❯ unfinished user text".into());
    assert!(!can_forward(&t, true, false));
    t.preview = Some("Do you want to proceed?\n❯ 1. Yes\n  2. No".into());
    assert!(!can_forward(&t, true, false));
    t.preview = Some("Claude Code\nuser@host ~ %".into());
    assert!(!can_forward(&t, true, false));
    t.agent_identity = Some(AgentIdentity::Codex);
    t.preview = Some("OpenAI Codex\n› prior task\nAnswer\n›".into());
    assert!(can_forward(&t, true, false));
    t.orphaned = true;
    assert!(!can_forward(&t, true, false));
}
#[test]
fn production_approval_transaction_never_calls_delivery_for_dry_run_or_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let mut queue = DraftQueue::load(dir.path().join("drafts.json")).unwrap();
    let reply = Draft::reply(
        ReplyTo {
            conversation: "C123".into(),
            ts: "1.000001".into(),
        },
        None,
        "Sample reply".into(),
        "Requested".into(),
        0,
    );
    let proposal = Draft::proposal(
        "Sample task".into(),
        None,
        "Requested".into(),
        "https://example.slack.com".into(),
        0,
    );
    for draft in [reply.clone(), proposal] {
        let draft = queue.create(draft).unwrap();
        apply_approval(&mut queue, &draft.id, Some("Edited"), true, 1, |_| {
            panic!("dry-run invoked live delivery")
        })
        .unwrap();
        assert_eq!(
            queue.get(&draft.id).unwrap().status,
            DraftStatus::DryRunSent
        );
    }
    let draft = queue.create(reply.clone()).unwrap();
    assert!(
        apply_approval(
            &mut queue,
            &draft.id,
            Some("ghp_123456789012345678901234567890"),
            false,
            1,
            |_| panic!("secret reached live delivery")
        )
        .is_err()
    );
    assert_eq!(queue.get(&draft.id).unwrap().status, DraftStatus::Pending);
    apply_approval(&mut queue, &draft.id, None, false, 2, |d| {
        assert_eq!(d.status, DraftStatus::Approved);
        Ok(false)
    })
    .unwrap();
    assert_eq!(queue.get(&draft.id).unwrap().status, DraftStatus::Approved);
    let draft = queue.create(reply).unwrap();
    assert!(
        apply_approval(&mut queue, &draft.id, None, false, 3, |_| Err(
            "offline".into()
        ))
        .is_err()
    );
    assert_eq!(queue.get(&draft.id).unwrap().status, DraftStatus::Failed);
}

#[test]
fn queued_snapshot_after_forward_ack_does_not_replay_on_stop_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, commands, updates) = Worker::test_pair();
    core.worker = Some(worker);
    core.set_enabled("term_envoy".into(), EnvoySettings::default())
        .unwrap();
    let event = match crate::envoy::slack::parse_bridge_line(
        include_bytes!("../../../tests/fixtures/envoy/channel.jsonl")
            .split(|b| *b == b'\n')
            .next()
            .unwrap(),
    )
    .unwrap()
    {
        crate::envoy::slack::BridgeLine::Event(e) => e,
        _ => panic!("fixture starts with a message"),
    };
    core.inbox.insert(event.clone());
    updates
        .send(Update::Polled {
            inbox: core.inbox.clone(),
            events: vec![],
            bridge: BridgeStatus::Ok,
            notices: vec![],
        })
        .unwrap();
    core.acknowledge(&[event]);
    assert!(commands.try_iter().any(|w| matches!(w, Work::Forwarded(_))));
    core.poll_updates();
    assert!(core.inbox.unforwarded().is_empty());
    core.stop();
    core.set_enabled("term_envoy".into(), EnvoySettings::default())
        .unwrap();
    assert!(
        core.pending.is_empty(),
        "queued snapshot must not replay acknowledged events"
    );
}

#[test]
fn bridge_alerts_on_start_reenable_and_each_down_transition() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    assert!(!core.observe_bridge(BridgeStatus::Missing));
    core.handle = Some("term_envoy".into());
    assert!(core.observe_bridge(BridgeStatus::Missing));
    assert!(!core.observe_bridge(BridgeStatus::Missing));
    assert!(!core.observe_bridge(BridgeStatus::Ok));
    assert!(core.observe_bridge(BridgeStatus::Stale));
    core.bridge_seen = false;
    assert!(core.observe_bridge(BridgeStatus::Stale));
}

#[derive(Clone, Default)]
struct FakeSlack {
    sent: std::sync::Arc<std::sync::Mutex<Vec<crate::envoy::channel::OutboundMessage>>>,
}
impl crate::envoy::slack::SlackSender for FakeSlack {
    fn send(
        &mut self,
        message: &crate::envoy::channel::OutboundMessage,
    ) -> Result<crate::envoy::channel::SendReceipt, String> {
        self.sent.lock().unwrap().push(message.clone());
        Ok(crate::envoy::channel::SendReceipt {
            conversation: message.conversation.clone(),
            ts: "1790000000.000100".into(),
        })
    }
}
impl crate::envoy::slack::SlackIo for FakeSlack {
    fn api(&mut self, _: &str, _: &[(&str, String)]) -> Result<Value, String> {
        Ok(json!({"ok":true,"messages":{"matches":[],"paging":{"pages":1}}}))
    }
    fn subscribe(&mut self, dir: &std::path::Path, _: &str) -> Result<(), String> {
        assert!(
            dir.join("fixture-only").is_file(),
            "explicit isolated bridge required"
        );
        Ok(())
    }
}

struct CoreControl<'a>(&'a mut AppEnvoy);
impl crate::mcp::Control for CoreControl<'_> {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, crate::mcp::CallError> {
        let command = EnvoyCommand::parse(method, &params)
            .unwrap()
            .map_err(crate::mcp::CallError::Control)?;
        self.0
            .request(&command)
            .map_err(|e| crate::mcp::CallError::Control(ControlError::invalid_params(e)))
    }
}

fn wait_for(core: &mut AppEnvoy, ready: impl Fn(&AppEnvoy) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        core.poll_updates();
        if ready(core) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fake worker did not finish: {:?}",
            core.error
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn fixture_to_mcp_to_panel_approval_dry_live_and_secret_block() {
    use crate::envoy::drafts::ATTRIBUTION;
    use crate::review::mark::Origin;
    use crate::ui::envoy::{DraftAction, DraftsPanel};
    for (dry_run, secret) in [(true, false), (false, false), (false, true)] {
        let dir = tempfile::tempdir().unwrap();
        let bridge = dir.path().join("bridge");
        std::fs::create_dir_all(bridge.join("channels")).unwrap();
        std::fs::write(bridge.join("fixture-only"), "").unwrap();
        std::fs::write(bridge.join("heartbeat"), jiff::Timestamp::now().to_string()).unwrap();
        let fixture = include_str!("../../../tests/fixtures/envoy/channel.jsonl")
            .lines()
            .next()
            .unwrap();
        let mut event: Value = serde_json::from_str(fixture).unwrap();
        event["ts"] = json!(format!("{}.000001", now_ms() / 1000 + 60));
        event["permalink"] = json!("https://example.slack.com/archives/C123/p1790000000000001");
        std::fs::write(bridge.join("channels/C123.jsonl"), format!("{event}\n")).unwrap();
        let fake = FakeSlack::default();
        let sent = fake.sent.clone();
        let mut core = AppEnvoy::at(dir.path().into(), &egui::Context::default(), fake, bridge);
        core.set_enabled(
            "term_envoy".into(),
            EnvoySettings {
                dry_run,
                channels: vec!["C123".into()],
                policies: vec![crate::envoy::settings::Policy {
                    destination: "C123".into(),
                    mode: crate::envoy::settings::ApprovalMode::Auto { veto_secs: 0 },
                }],
                ..Default::default()
            },
        )
        .unwrap();
        wait_for(&mut core, |c| !c.pending.is_empty());
        let (action, batch) = core.next_forward(now_ms(), &[]).expect("fixture forwarded");
        let Action::Send { handle, input } = action else {
            panic!("expected terminal line");
        };
        assert_eq!(handle, "term_envoy");
        assert!(input.enter);
        assert!(
            input
                .text
                .unwrap()
                .contains("Envoy event: agent_comms from Avery in C123: The build is ready.")
        );
        core.acknowledge(&batch);
        let args = json!({"in_reply_to":{"conversation":"C123","ts":event["ts"]},"text":"Use the checked build.","rationale":"Recommend this build; Karl has not decided.","manual_approval":true});
        let origin = Origin {
            terminal_handle: Some("term_envoy".into()),
            ..Default::default()
        };
        let mut server = crate::mcp::McpServer::new(CoreControl(&mut core)).with_review_env(
            crate::mcp::review::ReviewEnv {
                origin,
                ..Default::default()
            },
        );
        let result = server.handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"envoy_draft","arguments":args}})).unwrap();
        assert_ne!(result["result"]["isError"], true, "{result}");
        drop(server);
        assert_eq!(core.drafts().len(), 1);
        let id = core.drafts()[0].id.clone();
        core.auto_approve(i64::MAX).unwrap();
        assert_eq!(core.drafts()[0].status, DraftStatus::Pending);
        assert!(core.drafts()[0].auto_at_ms.is_none());
        assert!(sent.lock().unwrap().is_empty());
        let mut panel = DraftsPanel::default();
        let edited = if secret {
            "ghp_123456789012345678901234567890"
        } else {
            "I checked the build; it is ready."
        };
        panel.set_edit(&id, edited);
        let action = crate::ui::test_util::click_label("Approve", |ui| {
            panel.show(
                ui,
                core.drafts(),
                core.inbox(),
                core.settings().dry_run,
                None,
            )
        });
        let Some(DraftAction::Approve { id, text }) = action else {
            panic!("pointer approval missing");
        };
        assert_eq!(text, edited);
        let (_, result) = core.panel_action(
            DraftAction::Approve {
                id: id.clone(),
                text,
            },
            true,
            |_| panic!("reply must not reach Queen"),
        );
        if secret {
            assert!(result.is_err());
            assert_eq!(core.drafts()[0].status, DraftStatus::Pending);
        } else {
            result.unwrap();
            if !dry_run {
                wait_for(&mut core, |c| c.drafts()[0].status == DraftStatus::Sent);
            }
            assert_eq!(
                core.drafts()[0].status,
                if dry_run {
                    DraftStatus::DryRunSent
                } else {
                    DraftStatus::Sent
                }
            );
            assert!(
                core.envoy_approve(&id, None, true, |_| true).is_err(),
                "no second send"
            );
        }
        let sends = sent.lock().unwrap();
        assert_eq!(sends.len(), usize::from(!dry_run && !secret));
        if let Some(message) = sends.first() {
            assert_eq!(message.text, format!("{edited}\n\n{ATTRIBUTION}"));
            assert_eq!(message.conversation, "C123");
        }
        let outsider = EnvoyCommand {
            method: "envoy_drafts".into(),
            args: json!({}),
            origin: Origin {
                terminal_handle: Some("term_other".into()),
                ..Default::default()
            },
        };
        assert!(core.request(&outsider).is_err());
    }
}

#[test]
fn refused_prompt_relays_do_not_spend_budget_and_text_stays_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.settings.dry_run = false;
    let mut terminal = Terminal {
        agent_identity: Some(AgentIdentity::Claude),
        writable: true,
        preview: Some("Do you want to proceed?\n❯ 1. Yes\n  2. No".into()),
        ..Default::default()
    };
    let request = RelayRequest {
        to: "term_worker".into(),
        summary: "Context\nDo this\r\x1b".into(),
        slack_link: "https://example.slack.com/message\n".into(),
        why: "Related\twork".into(),
    };
    assert!(
        core.reserve_relay(request.clone(), can_forward(&terminal, true, false))
            .is_err()
    );
    terminal.preview = Some("Claude Code\n❯\n? for shortcuts".into());
    assert!(
        core.reserve_relay(request.clone(), can_forward(&terminal, false, false))
            .is_err()
    );
    assert!(
        core.reserve_relay(request.clone(), can_forward(&terminal, true, true))
            .is_err()
    );
    assert!(core.relays().is_empty());
    assert!(!core.relay_path.exists());
    let record = core
        .reserve_relay(request, can_forward(&terminal, true, false))
        .unwrap();
    assert_eq!(core.relays().len(), 1);
    let text = record.request.text();
    assert!(text.starts_with(crate::envoy::relay::RELAY_PREFIX));
    assert!(!text.chars().any(char::is_control));
}

#[test]
#[ignore = "seeds the isolated native UI inspection; requires ENVOY_PANEL_FIXTURE_DIR"]
fn seed_envoy_panel_fixture() {
    let dir = PathBuf::from(
        std::env::var_os("ENVOY_PANEL_FIXTURE_DIR").expect("explicit fixture directory"),
    );
    assert!(dir.join("fixture-only").is_file());
    let mut inbox = Inbox::default();
    for line in include_bytes!("../../../tests/fixtures/envoy/channel.jsonl").split(|b| *b == b'\n')
    {
        if let Ok(crate::envoy::slack::BridgeLine::Event(mut event)) =
            crate::envoy::slack::parse_bridge_line(line)
        {
            event.permalink =
                Some("https://example.slack.com/archives/C123/p1790000000000001".into());
            inbox.insert(event);
        }
    }
    inbox
        .save(&dir.join(crate::envoy::inbox::INBOX_FILE))
        .unwrap();
    let mut queue = DraftQueue::load(dir.join(crate::envoy::drafts::DRAFTS_FILE)).unwrap();
    queue
        .create(Draft::reply(
            ReplyTo {
                conversation: "C123".into(),
                ts: "1790000000.000001".into(),
            },
            Some("1790000000.000001".into()),
            "The build is ready for your review.".into(),
            "Avery asked for a build update.".into(),
            now_ms(),
        ))
        .unwrap();
    queue
        .create(Draft::proposal(
            "Review the fixture build".into(),
            Some("myrstack".into()),
            "Casey offered to check it.".into(),
            "https://example.slack.com/archives/C123/p1790000000000002".into(),
            now_ms(),
        ))
        .unwrap();
    let failed = queue
        .create(Draft::proposal(
            "Earlier test task".into(),
            None,
            "Fixture history".into(),
            "https://example.slack.com/message".into(),
            now_ms(),
        ))
        .unwrap();
    queue
        .begin_approval(&failed.id, None, false, now_ms())
        .unwrap();
    queue
        .finish(&failed.id, Err("Fixture: Queen was busy".into()), now_ms())
        .unwrap();
    let mut notices =
        crate::notify::NotificationCenter::new(crate::notify::history::History::default());
    notices.dispatch(
        Incoming::new(
            Source::Envoy,
            Severity::Info,
            "Envoy draft ready",
            "A fixture draft is waiting for approval.",
        )
        .with_target(DRAFTS_TARGET),
        &crate::notify::NotifySettings::default(),
        now_ms(),
    );
    notices
        .history
        .save(&dir.join(crate::notify::history::HISTORY_FILE))
        .unwrap();
    let mut layout = crate::ui::dock::PaneLayout::default();
    layout.open_envoy();
    let state = crate::persist::PersistedState {
        panes: layout.encode(),
        chat_hidden: true,
        ..Default::default()
    };
    let storage = HashMap::from([(
        crate::persist::STORAGE_KEY.to_owned(),
        ron::to_string(&state).unwrap(),
    )]);
    std::fs::write(dir.join("app.ron"), ron::to_string(&storage).unwrap()).unwrap();
}

#[test]
fn send_failure_raises_targeted_warning_and_keeps_history_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, _commands, updates) = Worker::test_pair();
    core.worker = Some(worker);
    let draft = core
        .queue()
        .unwrap()
        .create(Draft::reply(
            ReplyTo {
                conversation: "C123".into(),
                ts: "1.000001".into(),
            },
            None,
            "Reply".into(),
            "Requested".into(),
            0,
        ))
        .unwrap();
    core.queue()
        .unwrap()
        .begin_approval(&draft.id, None, false, 0)
        .unwrap();
    updates
        .send(Update::Sent {
            id: draft.id,
            result: Err("fixture send failed".into()),
        })
        .unwrap();
    let (notices, _) = core.poll_updates();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].target.as_deref(), Some(DRAFTS_TARGET));
    assert_eq!(notices[0].severity, Severity::Warn);
    assert_eq!(core.drafts()[0].status, DraftStatus::Failed);
    assert_eq!(
        core.drafts()[0].error.as_deref(),
        Some("fixture send failed")
    );
}

fn receipt_event(ts: &str) -> InboundEvent {
    InboundEvent {
        channel_kind: "slack".into(),
        conversation: "C123".into(),
        ts: ts.into(),
        thread_ts: None,
        user: "U123".into(),
        user_name: "Avery".into(),
        text: "Fixture message".into(),
        kind: crate::envoy::channel::EventKind::AgentComms,
        permalink: None,
    }
}

fn submit_receipt_batch(core: &mut AppEnvoy, now: i64, id: ActionId) -> ActionResult {
    let (action, batch) = core.next_forward(now, &[]).expect("pending batch");
    core.forwarding = Some((id, batch));
    ActionResult {
        id,
        action,
        outcome: Ok(()),
        note: None,
    }
}

#[test]
fn receipt_starts_only_after_matching_success_and_respects_cooldown() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("term_envoy".into());
    let events = vec![receipt_event("1.000001"), receipt_event("1.000002")];
    let mut inbox = Inbox::default();
    for event in &events {
        inbox.insert(event.clone());
    }
    core.merge_polled(inbox, events);
    assert_eq!(core.shake_started_ms, None, "ingestion is not delivery");
    let result = submit_receipt_batch(&mut core, 100, 1);
    assert_eq!(core.shake_started_ms, None, "submission is not delivery");
    core.forward_done(
        &ActionResult {
            id: 2,
            ..result.clone()
        },
        100,
        |_| panic!("unrelated action"),
    );
    assert_eq!(core.shake_started_ms, None);
    assert!(core.forwarding.is_some());
    let mut traffic = Vec::new();
    core.forward_done(&result, 100, |m| traffic.push(m));
    assert_eq!(traffic.len(), 2);
    assert_eq!(core.shake_started_ms, Some(100));
    assert!(core.inbox.unforwarded().is_empty());
    core.forward_done(&result, 10_000, |_| panic!("duplicate completion"));
    assert_eq!(core.shake_started_ms, Some(100));

    for (now, expected) in [
        (200, 100),
        (100 + SHAKE_COOLDOWN_MS - 1, 100),
        (100 + SHAKE_COOLDOWN_MS, 100 + SHAKE_COOLDOWN_MS),
    ] {
        core.pending
            .push_back(receipt_event(&format!("{now}.000001")));
        let result = submit_receipt_batch(&mut core, now, 3);
        core.forward_done(&result, now, |_| {});
        assert_eq!(core.shake_started_ms, Some(expected));
    }
}

#[test]
fn failed_forward_and_retry_submission_do_not_start_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("term_envoy".into());
    let events = vec![receipt_event("1.000001"), receipt_event("1.000002")];
    core.pending.extend(events.clone());
    let mut result = submit_receipt_batch(&mut core, 100, 1);
    result.outcome = Err("fixture failure".into());
    core.forward_done(&result, 100, |_| panic!("failed delivery"));
    assert_eq!(core.shake_started_ms, None);
    assert_eq!(core.pending.iter().cloned().collect::<Vec<_>>(), events);
    assert!(core.next_forward(30_099, &[]).is_none());
    let result = submit_receipt_batch(&mut core, 30_100, 2);
    assert_eq!(core.shake_started_ms, None);
    core.forward_done(&result, 30_101, |_| {});
    assert_eq!(core.shake_started_ms, Some(30_101));
}

#[test]
fn filtered_backlog_and_duplicate_events_do_not_start_receipt() {
    use crate::envoy::channel::Channel;
    let dir = tempfile::tempdir().unwrap();
    let bridge = dir.path().join("bridge");
    std::fs::create_dir_all(bridge.join("channels")).unwrap();
    std::fs::write(bridge.join("fixture-only"), "").unwrap();
    let event = receipt_event("2.000001");
    let mut inbox = Inbox::default();
    inbox.first_enabled_ms = Some(2_000);
    inbox.insert(event.clone());
    let lines = ["1.000001", "2.000001"].map(|ts| {
        json!({"type":"message", "channel":"C123", "ts":ts, "user":"U123", "text":"Fixture message"}).to_string()
    }).join("\n");
    std::fs::write(bridge.join("channels/C123.jsonl"), format!("{lines}\n")).unwrap();
    let path = dir.path().join("inbox.json");
    let mut channel = crate::envoy::slack::SlackChannel::new(
        FakeSlack::default(),
        inbox,
        path.clone(),
        bridge.clone(),
        EnvoySettings {
            channels: vec!["C123".into()],
            ..Default::default()
        },
    );
    let batch = channel.poll(3_000).unwrap();
    assert!(
        batch.events.is_empty(),
        "backlog and duplicate are filtered"
    );
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("term_envoy".into());
    core.merge_polled(Inbox::load(&path).unwrap(), batch.events);
    assert!(core.next_forward(3_000, &[]).is_none());
    assert_eq!(core.shake_started_ms, None);

    let fresh = json!({"type":"message", "channel":"C123", "ts":"3.000001", "user":"U123", "text":"Fixture message"});
    std::fs::write(
        bridge.join("channels/C123.jsonl"),
        format!("{lines}\n{fresh}\n"),
    )
    .unwrap();
    let batch = channel.poll(3_001).unwrap();
    assert_eq!(batch.events.len(), 1, "same channel accepts a fresh event");
    assert_eq!(batch.events[0].ts, "3.000001");
    core.merge_polled(Inbox::load(&path).unwrap(), batch.events);
    assert!(core.next_forward(3_001, &[]).is_some());
    assert_eq!(
        core.shake_started_ms, None,
        "fresh ingestion still is not delivery"
    );
}

#[test]
fn idle_envoy_with_a_bare_composer_preview_takes_events() {
    // Orca's preview of an idle Claude Envoy: a stale banner, then only the empty composer.
    let terminal = Terminal {
        agent_identity: Some(AgentIdentity::Claude),
        writable: true,
        title: Some("\u{2733} Envoy MCP tool setup".into()),
        preview: Some(
            "approves them without a prompt. Replace that * with the exact value you mean.\n\u{276f}"
                .into(),
        ),
        ..Terminal::default()
    };
    assert!(can_forward(&terminal, true, false));
}

#[test]
fn batches_name_candidate_ants_and_fold_the_backlog() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("term_envoy".into());
    let now = 1_791_300_000_000;
    let mut old = receipt_event("1791200000.000001");
    old.text = "yesterday".into();
    let mut root = receipt_event("1791299900.000001");
    root.text = "Plan for realtime#694".into();
    let mut reply = receipt_event("1791299950.000001");
    reply.thread_ts = Some(root.ts.clone());
    reply.text = "yes, push the merge commit yourself".into();
    let mut chatter = receipt_event("1791299960.000001");
    chatter.text = "lunch?".into();
    let events = vec![old, root, reply, chatter];
    let mut inbox = Inbox::default();
    for event in &events {
        inbox.insert(event.clone());
    }
    core.merge_polled(inbox, events);
    let ants = [Ant {
        id: "term:term_ant".into(),
        label: "Recs rollout".into(),
        prs: vec!["realtime#694".into()],
        ..Ant::default()
    }];
    let (action, batch) = core.next_forward(now, &ants).expect("batch");
    assert_eq!(batch.len(), 4, "the backlog is acknowledged with the batch");
    let Action::Send { input, .. } = action else {
        panic!("typed batch")
    };
    let lines: Vec<String> = input
        .text
        .unwrap()
        .split(" | ")
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 4);
    assert!(lines[0].starts_with("Envoy backlog: 1 event(s) older than 30 min"));
    assert!(lines[1].ends_with("ants: Recs rollout (term:term_ant, realtime#694)"));
    assert!(
        lines[2].contains("push the merge commit")
            && lines[2].ends_with("ants: Recs rollout (term:term_ant, realtime#694)"),
        "a reply inherits its thread's refs: {}",
        lines[2]
    );
    assert!(lines[3].ends_with("— no ant match"));
}

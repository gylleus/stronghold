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
    assert!(can_forward(&t, true, false, None));
    assert!(!can_forward(&t, false, false, None));
    assert!(!can_forward(&t, true, true, None));
    t.preview = Some("Claude Code\n❯ unfinished user text".into());
    assert!(!can_forward(&t, true, false, None));
    t.preview = Some("Do you want to proceed?\n❯ 1. Yes\n  2. No".into());
    assert!(!can_forward(&t, true, false, None));
    t.preview = Some("Claude Code\nuser@host ~ %".into());
    assert!(!can_forward(&t, true, false, None));
    t.agent_identity = Some(AgentIdentity::Codex);
    t.preview = Some("OpenAI Codex\n› prior task\nAnswer\n›".into());
    assert!(can_forward(&t, true, false, None));
    t.orphaned = true;
    assert!(!can_forward(&t, true, false, None));
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
            polled_at_ms: now_ms(),
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
    /// What `auth.test` answers; `None` answers like any other search.
    karl_id: Option<&'static str>,
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
    fn api(&mut self, method: &str, _: &[(&str, String)]) -> Result<Value, String> {
        if method == "auth.test"
            && let Some(id) = self.karl_id
        {
            return Ok(json!({"ok":true,"user":"karl126","user_id":id,"team":"Depict"}));
        }
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
            |_| panic!("reply must not reach King"),
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
        core.reserve_relay(request.clone(), can_forward(&terminal, true, false, None))
            .is_err()
    );
    terminal.preview = Some("Claude Code\n❯\n? for shortcuts".into());
    assert!(
        core.reserve_relay(request.clone(), can_forward(&terminal, false, false, None))
            .is_err()
    );
    assert!(
        core.reserve_relay(request.clone(), can_forward(&terminal, true, true, None))
            .is_err()
    );
    assert!(core.relays().is_empty());
    assert!(!core.relay_path.exists());
    let record = core
        .reserve_relay(request, can_forward(&terminal, true, false, None))
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
        .finish(&failed.id, Err("Fixture: King was busy".into()), now_ms())
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
    for event in &events {
        core.inbox.insert(event.clone());
    }
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
    assert!(can_forward(&terminal, true, false, None));
}

#[test]
fn batches_name_candidate_dwarves_and_fold_the_backlog() {
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
    let dwarves = [Dwarf {
        id: "term:term_dwarf".into(),
        label: "Recs rollout".into(),
        prs: vec!["realtime#694".into()],
        ..Dwarf::default()
    }];
    let (action, batch) = core.next_forward(now, &dwarves).expect("batch");
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
    assert!(lines[1].ends_with("dwarves: Recs rollout (term:term_dwarf, realtime#694)"));
    assert!(
        lines[2].contains("push the merge commit")
            && lines[2].ends_with("dwarves: Recs rollout (term:term_dwarf, realtime#694)"),
        "a reply inherits its thread's refs: {}",
        lines[2]
    );
    assert!(lines[3].ends_with("— no dwarf match"));
}

#[test]
fn reject_removes_the_item_from_the_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let event: InboundEvent = serde_json::from_value(json!({
        "channel_kind":"slack", "conversation":"C123", "ts":"1.000001",
        "user":"U123", "user_name":"Avery", "text":"Please reply",
        "kind":"agent_comms"
    }))
    .unwrap();
    core.inbox.insert(event.clone());
    core.inbox
        .save(&dir.path().join(crate::envoy::inbox::INBOX_FILE))
        .unwrap();
    let draft = core
        .queue()
        .unwrap()
        .create(Draft::reply(
            ReplyTo {
                conversation: event.conversation.clone(),
                ts: event.ts.clone(),
            },
            None,
            "No thanks".into(),
            "Requested".into(),
            0,
        ))
        .unwrap();
    let (_, result) = core.panel_action(
        crate::ui::envoy::DraftAction::Reject(draft.id.clone()),
        false,
        |_| panic!("reject must not send"),
    );
    result.unwrap();
    assert!(
        core.inbox.since(None, 20).is_empty(),
        "rejected source remains in the open inbox"
    );
    let reloaded = AppEnvoy::load(dir.path().into());
    assert!(reloaded.inbox.since(None, 20).is_empty());
}

#[test]
fn dismissed_events_survive_poll_restart_and_failed_forwarding() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("envoy".into());
    let event = receipt_event("1.000001");
    core.inbox.insert(event.clone());
    core.inbox
        .save(&dir.path().join(crate::envoy::inbox::INBOX_FILE))
        .unwrap();
    let snapshot = core.inbox.clone();
    core.pending.push_back(event.clone());
    let mut result = submit_receipt_batch(&mut core, 100, 1);
    result.outcome = Err("offline".into());
    core.panel_action(
        crate::ui::envoy::DraftAction::DismissEvent(event.id()),
        false,
        |_| panic!("dismiss must not send"),
    )
    .1
    .unwrap();
    core.merge_polled(snapshot, vec![event.clone()]);
    core.forward_done(&result, 100, |_| panic!("failed delivery"));
    assert!(core.pending.is_empty());
    assert!(core.inbox.unforwarded().is_empty());
    assert!(core.inbox.since(None, 20).is_empty());
    assert!(
        core.inbox.get(&event.id()).is_some(),
        "retain thread context and deduplication"
    );
    let mut reloaded = AppEnvoy::load(dir.path().into());
    assert!(reloaded.inbox.unforwarded().is_empty());
    assert!(!reloaded.inbox.insert(event));
}

#[test]
fn clear_handled_persists_failed_dismissal_and_keeps_pending_approvals_and_fresh_events() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let stale = receipt_event("1.000001");
    let active = receipt_event("2.000001");
    let fresh = receipt_event(&format!("{}.000001", now_ms() / 1000));
    for event in [&stale, &active, &fresh] {
        core.inbox.insert(event.clone());
        core.pending.push_back(event.clone());
    }
    core.inbox
        .save(&dir.path().join(crate::envoy::inbox::INBOX_FILE))
        .unwrap();
    let draft = core
        .queue()
        .unwrap()
        .create(Draft::reply(
            ReplyTo {
                conversation: active.conversation.clone(),
                ts: active.ts.clone(),
            },
            None,
            "Pending".into(),
            "Requested".into(),
            0,
        ))
        .unwrap();
    let failed = core
        .queue()
        .unwrap()
        .create(Draft::proposal(
            "Failed task".into(),
            None,
            "Requested".into(),
            String::new(),
            0,
        ))
        .unwrap();
    core.queue()
        .unwrap()
        .begin_approval(&failed.id, None, false, 0)
        .unwrap();
    core.queue()
        .unwrap()
        .finish(&failed.id, Err("Offline".into()), 1)
        .unwrap();
    core.panel_action(crate::ui::envoy::DraftAction::ClearHandled, false, |_| {
        panic!("cleanup must not send")
    })
    .1
    .unwrap();
    let mut reloaded = AppEnvoy::load(dir.path().into());
    assert!(!reloaded.inbox.is_open(&stale.id()));
    assert!(reloaded.inbox.is_open(&active.id()));
    assert!(reloaded.inbox.is_open(&fresh.id()));
    assert_eq!(reloaded.pending_count(), 1);
    assert!(
        !reloaded.cleanup.draft_open(
            reloaded
                .drafts()
                .iter()
                .find(|d| d.id == failed.id)
                .unwrap()
        )
    );
    reloaded.handle = Some("envoy".into());
    let command = |args| EnvoyCommand {
        method: "envoy_drafts".into(),
        args,
        origin: crate::review::mark::Origin {
            terminal_handle: Some("envoy".into()),
            ..Default::default()
        },
    };
    let open = reloaded.request(&command(json!({}))).unwrap();
    assert_eq!(open["drafts"].as_array().unwrap().len(), 1);
    assert_eq!(open["drafts"][0]["id"], draft.id);
    let history = reloaded
        .request(&command(json!({"status":"failed"})))
        .unwrap();
    assert_eq!(history["drafts"][0]["id"], failed.id);
}

#[test]
fn cleanup_save_failure_keeps_the_item_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let event = receipt_event("1.000001");
    core.inbox.insert(event.clone());
    std::fs::create_dir(&core.cleanup_path).unwrap();
    assert!(
        core.panel_action(
            crate::ui::envoy::DraftAction::DismissEvent(event.id()),
            false,
            |_| false
        )
        .1
        .is_err()
    );
    assert!(core.inbox.is_open(&event.id()));
}

#[test]
fn rejection_resolves_its_alert_without_reading_another_pending_drafts_alert() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let mut notices =
        crate::notify::NotificationCenter::new(crate::notify::history::History::default());
    let mut drafts = Vec::new();
    for text in ["One", "Two"] {
        let draft = core
            .queue()
            .unwrap()
            .create(Draft::proposal(
                text.into(),
                None,
                "Requested".into(),
                String::new(),
                0,
            ))
            .unwrap();
        notices.dispatch(
            cleanup::draft_notice(&json!(draft)),
            &crate::notify::NotifySettings::default(),
            1,
        );
        drafts.push(draft);
    }
    core.panel_action(
        crate::ui::envoy::DraftAction::Reject(drafts[0].id.clone()),
        false,
        |_| false,
    )
    .1
    .unwrap();
    assert_eq!(
        notices.history.mark_read_where(|e| core.notice_resolved(e)),
        1
    );
    let path = dir.path().join("notifications.json");
    notices.history.save(&path).unwrap();
    let loaded = crate::notify::history::History::load(&path);
    assert!(loaded.entries()[0].read);
    assert!(!loaded.entries()[1].read);
}

fn envoy_command(method: &str, args: Value) -> EnvoyCommand {
    EnvoyCommand {
        method: method.into(),
        args,
        origin: crate::review::mark::Origin {
            terminal_handle: Some("term_envoy".into()),
            ..Default::default()
        },
    }
}

fn clear_action(core: &AppEnvoy, now: i64, tokens: Option<u64>) -> Option<ResetReason> {
    let (action, reason) = core.reset_action(now, tokens)?;
    assert_eq!(
        action,
        Action::Send {
            handle: "term_envoy".into(),
            input: TerminalInput::line(CLEAR_COMMAND),
        }
    );
    Some(reason)
}

fn typed(action: &Action) -> String {
    let Action::Send { input, .. } = action else {
        panic!("typed line")
    };
    input.text.clone().unwrap()
}

#[test]
fn quiet_period_clears_the_context_and_hands_pending_events_over_with_a_preamble() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("term_envoy".into());
    let settings = EnvoySettings::default();
    core.settings = settings.clone();
    let quiet_ms = settings.fresh_after_secs as i64 * 1_000;
    let events = vec![receipt_event("1.000001"), receipt_event("1.000002")];
    let mut inbox = Inbox::default();
    for event in &events {
        inbox.insert(event.clone());
    }
    core.merge_polled(inbox, events);
    assert_eq!(
        clear_action(&core, 100, Some(1_000_000)),
        None,
        "the launch context has had no batch: nothing to clear"
    );
    let result = submit_receipt_batch(&mut core, 100, 1);
    assert!(
        !typed(&result.action).starts_with("Fresh context"),
        "the launch prompt already introduced the session"
    );
    core.forward_done(&result, 100, |_| {});
    assert_eq!(core.context().events_since_reset, 2);

    // A burst shares the context; after the quiet period a new batch starts clean.
    core.inbox.insert(receipt_event("2.000001"));
    core.pending.push_back(receipt_event("2.000001"));
    assert_eq!(clear_action(&core, 100 + quiet_ms - 1, None), None);
    let now = 100 + quiet_ms;
    assert_eq!(clear_action(&core, now, None), Some(ResetReason::Quiet));
    core.resetting = Some(7);
    assert_eq!(clear_action(&core, now, None), None, "one reset at a time");
    assert_eq!(
        core.pending.len(),
        1,
        "pending events wait across the reset"
    );
    core.reset_done(
        &ActionResult {
            id: 7,
            action: Action::Send {
                handle: "term_envoy".into(),
                input: TerminalInput::line(CLEAR_COMMAND),
            },
            outcome: Ok(()),
            note: None,
        },
        now,
    );
    assert!(core.resetting.is_none());
    assert!(core.context().fresh);
    assert_eq!(core.context().events_since_reset, 0);
    assert_eq!(core.context().resets, 1);
    assert_eq!(core.pending.len(), 1);
    assert_eq!(
        core.inbox.unforwarded().len(),
        1,
        "nothing is acknowledged by a reset"
    );
    assert!(
        core.next_forward(now + 1_999, &[]).is_none(),
        "the cleared prompt gets a moment to settle"
    );
    assert_eq!(clear_action(&core, now + 2_000, Some(1_000_000)), None);
    let result = submit_receipt_batch(&mut core, now + 2_000, 8);
    let line = typed(&result.action);
    assert!(
        line.starts_with("Fresh context: your earlier conversation was cleared."),
        "{line}"
    );
    assert!(line.contains("Dry-run is ON"), "{line}");
    assert!(
        line.contains(
            " | Envoy event: agent_comms from Avery in C123: Fixture message — id C123:2.000001"
        ),
        "{line}"
    );
    core.forward_done(&result, now + 2_000, |_| {});
    assert!(!core.context().fresh);
    assert_eq!(core.context().events_since_reset, 1);
    assert!(core.inbox.unforwarded().is_empty());
    core.pending.push_back(receipt_event("3.000001"));
    let result = submit_receipt_batch(&mut core, now + 3_000, 9);
    assert!(
        typed(&result.action).starts_with("Envoy event:"),
        "only the first batch after a reset carries the preamble"
    );
}

#[test]
fn size_guards_reset_mid_burst_but_never_while_a_send_is_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("term_envoy".into());
    core.settings = EnvoySettings {
        reset_after_events: 3,
        reset_after_tokens: 50_000,
        ..Default::default()
    };
    core.pending.push_back(receipt_event("1.000001"));
    let result = submit_receipt_batch(&mut core, 100, 1);
    core.forward_done(&result, 100, |_| {});
    core.pending.push_back(receipt_event("2.000001"));
    assert_eq!(clear_action(&core, 200, Some(49_999)), None);
    assert_eq!(
        clear_action(&core, 200, Some(50_000)),
        Some(ResetReason::Tokens)
    );
    core.pending.push_back(receipt_event("3.000001"));
    let result = submit_receipt_batch(&mut core, 200, 2);
    core.forward_done(&result, 200, |_| {});
    assert_eq!(core.context().events_since_reset, 3);
    core.pending.push_back(receipt_event("4.000001"));
    assert_eq!(clear_action(&core, 300, None), Some(ResetReason::Events));

    core.outgoing.insert("draft-1".into(), "term_envoy".into());
    assert_eq!(core.in_flight(), Some("Slack send in flight"));
    assert_eq!(clear_action(&core, 300, None), None);
    core.outgoing.clear();
    core.relay_actions.insert(
        5,
        AgentMessage::new("term_envoy", "term_dwarf", MessageKind::Relay),
    );
    assert_eq!(core.in_flight(), Some("relay in flight"));
    assert_eq!(clear_action(&core, 300, None), None);
    core.relay_actions.clear();
    let mut draft = Draft::reply(
        ReplyTo {
            conversation: "C123".into(),
            ts: "1.000001".into(),
        },
        None,
        "On it".into(),
        "asked".into(),
        0,
    );
    draft.status = DraftStatus::Approved;
    let draft = core.queue().unwrap().create(draft).unwrap();
    assert_eq!(core.in_flight(), Some("approved draft being sent"));
    assert_eq!(clear_action(&core, 300, None), None);
    core.queue()
        .unwrap()
        .finish(&draft.id, Ok(()), 301)
        .unwrap();
    assert_eq!(core.in_flight(), None);
    assert_eq!(clear_action(&core, 300, None), Some(ResetReason::Events));
}

#[test]
fn a_failed_clear_keeps_the_batch_in_the_old_context_and_retries_later() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("term_envoy".into());
    core.settings = EnvoySettings {
        fresh_after_secs: 0,
        ..Default::default()
    };
    core.pending.push_back(receipt_event("1.000001"));
    let result = submit_receipt_batch(&mut core, 100, 1);
    core.forward_done(&result, 100, |_| {});
    core.pending.push_back(receipt_event("2.000001"));
    assert_eq!(clear_action(&core, 200, None), Some(ResetReason::Quiet));
    core.resetting = Some(2);
    core.reset_done(
        &ActionResult {
            id: 2,
            action: Action::Send {
                handle: "term_envoy".into(),
                input: TerminalInput::line(CLEAR_COMMAND),
            },
            outcome: Err("terminal busy".into()),
            note: None,
        },
        200,
    );
    assert!(core.resetting.is_none());
    assert!(!core.context().fresh);
    assert_eq!(core.context().events_since_reset, 1);
    assert_eq!(core.pending.len(), 1);
    assert_eq!(clear_action(&core, 200 + 29_999, None), None, "backing off");
    assert!(core.next_forward(200 + 29_999, &[]).is_none());
    let result = submit_receipt_batch(&mut core, 200 + 30_000, 3);
    assert!(
        typed(&result.action).starts_with("Envoy event:"),
        "no preamble: the context was not cleared"
    );
    core.forward_done(&result, 200 + 30_000, |_| {});
    assert_eq!(core.context().events_since_reset, 2);
    assert_eq!(
        clear_action(&core, 200 + crate::envoy::context::RESET_BACKOFF_MS, None),
        Some(ResetReason::Quiet)
    );
}

#[test]
fn stop_and_a_new_session_forget_the_context_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, _commands, _updates) = Worker::test_pair();
    core.worker = Some(worker);
    core.set_enabled("term_envoy".into(), EnvoySettings::default())
        .unwrap();
    core.pending.push_back(receipt_event("1.000001"));
    let result = submit_receipt_batch(&mut core, 100, 1);
    core.forward_done(&result, 100, |_| {});
    core.resetting = Some(2);
    core.set_enabled("term_envoy".into(), EnvoySettings::default())
        .unwrap();
    assert_eq!(core.context().events_since_reset, 1, "same session: kept");
    core.set_enabled("term_envoy_2".into(), EnvoySettings::default())
        .unwrap();
    assert_eq!(core.context(), &ContextBudget::default());
    assert!(core.resetting.is_none());
    core.pending.push_back(receipt_event("2.000001"));
    let result = submit_receipt_batch(&mut core, 200, 3);
    core.forward_done(&result, 200, |_| {});
    core.stop();
    assert_eq!(core.context(), &ContextBudget::default());
}

#[test]
fn thread_notes_are_written_by_tool_and_typed_with_every_event_in_the_thread() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("term_envoy".into());
    let mut root = receipt_event("10.000001");
    root.text = "Can Karl's agent review realtime#694?".into();
    let mut reply = receipt_event("11.000001");
    reply.thread_ts = Some(root.ts.clone());
    reply.text = "ping".into();
    let other = receipt_event("12.000001");
    let mut dm = receipt_event("13.000001");
    dm.conversation = "D1".into();
    let events = vec![root.clone(), reply.clone(), other.clone(), dm.clone()];
    let mut inbox = Inbox::default();
    for event in &events {
        inbox.insert(event.clone());
    }
    core.merge_polled(inbox, events);
    assert!(
        core.request(&envoy_command(
            "envoy_note",
            json!({"conversation":"C123","thread_ts":"10.000001","text":"Avery asked for a realtime#694 review;\npromised an answer today; waiting on the Recs dwarf"})
        ))
        .is_ok()
    );
    let stored = core
        .request(&envoy_command(
            "envoy_note",
            json!({"conversation":"D1","text":"Daniel: wants the rollout date"}),
        ))
        .unwrap();
    assert_eq!(stored["note"]["text"], "Daniel: wants the rollout date");
    assert_eq!(stored["notes"], 2);
    for args in [
        json!({"conversation":"nope","text":"x"}),
        json!({"conversation":"C123","thread_ts":"later","text":"x"}),
        json!({"conversation":"C123","text":"x".repeat(601)}),
        json!({"conversation":"C123"}),
    ] {
        assert!(core.request(&envoy_command("envoy_note", args)).is_err());
    }
    let (action, _) = core.next_forward(20_000, &[]).unwrap();
    let lines: Vec<String> = typed(&action).split(" | ").map(str::to_owned).collect();
    assert_eq!(lines.len(), 4);
    let note = "— note: Avery asked for a realtime#694 review; promised an answer today; waiting on the Recs dwarf";
    assert!(lines[0].ends_with(note), "root: {}", lines[0]);
    assert!(lines[1].ends_with(note), "reply: {}", lines[1]);
    assert!(
        lines[2].ends_with("— no dwarf match"),
        "no note: {}",
        lines[2]
    );
    assert!(
        lines[3].ends_with("— note: Daniel: wants the rollout date"),
        "conversation note: {}",
        lines[3]
    );
    let inbox = core
        .request(&envoy_command("envoy_inbox", json!({})))
        .unwrap();
    assert_eq!(inbox["events"].as_array().unwrap().len(), 4);
    let notes = inbox["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 2, "one per thread, not per event: {notes:?}");
    assert!(
        core.request(&envoy_command(
            "envoy_note",
            json!({"conversation":"D1","text":""})
        ))
        .unwrap()["note"]
            .is_null()
    );
    assert_eq!(
        AppEnvoy::load(dir.path().into())
            .notes
            .as_ref()
            .unwrap()
            .len(),
        1,
        "notes persist"
    );
}

#[test]
fn orca_preview_without_the_composer_defers_to_the_sampled_screen() {
    use crate::detect::ComposerState;
    // Orca's preview of the live Envoy during the stall: the stale startup banner, then the
    // hint Claude Code draws under the composer. No `❯` anywhere.
    let terminal = Terminal {
        agent_identity: Some(AgentIdentity::Claude),
        writable: true,
        title: Some("\u{2733} Envoy MCP tool setup".into()),
        preview: Some(
            "approves them without a prompt. Replace that * with the exact value you mean, or only use * after the subcommand.\nnew task? /clear to save 101.9k tokens"
                .into(),
        ),
        ..Terminal::default()
    };
    assert!(
        !can_forward(&terminal, true, false, None),
        "the preview alone cannot show an empty composer"
    );
    assert!(can_forward(
        &terminal,
        true,
        false,
        Some(ComposerState::Empty)
    ));
    assert!(!can_forward(
        &terminal,
        true,
        false,
        Some(ComposerState::Text)
    ));
    assert!(!can_forward(
        &terminal,
        true,
        false,
        Some(ComposerState::Absent)
    ));
    assert!(
        !can_forward(&terminal, false, false, Some(ComposerState::Empty)),
        "the screen never overrides a busy node"
    );
    let bare = Terminal {
        preview: Some("banner\n\u{276f}".into()),
        ..terminal
    };
    assert!(can_forward(&bare, true, false, None));
    assert!(
        !can_forward(&bare, true, false, Some(ComposerState::Text)),
        "a stale bare-composer preview loses to the screen"
    );
}

#[test]
fn events_waiting_too_long_turn_the_status_stuck_and_warn_once_per_half_hour() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, _commands, updates) = Worker::test_pair();
    core.worker = Some(worker);
    assert_eq!(core.stuck_events(now_ms()), 0);
    core.set_enabled("term_envoy".into(), EnvoySettings::default())
        .unwrap();
    let now = now_ms();
    let mut inbox = Inbox::default();
    inbox.insert(receipt_event("1.000001"));
    inbox.insert(receipt_event("1.000002"));
    updates
        .send(Update::Polled {
            inbox,
            events: vec![receipt_event("1.000001"), receipt_event("1.000002")],
            bridge: BridgeStatus::Ok,
            notices: vec![],
            polled_at_ms: now,
        })
        .unwrap();
    let (notices, _) = core.poll_updates();
    assert!(notices.iter().all(|n| n.title != "Envoy events stuck"));
    assert_eq!(core.pending.len(), 2);
    let now = core
        .pending_since_ms
        .expect("ingestion starts the wait clock");
    assert_eq!(core.stuck_events(now + STUCK_AFTER_MS - 1), 0);
    assert_eq!(core.stuck_events(now + STUCK_AFTER_MS), 2);
    core.blocked = Some("no empty agent prompt");
    // The poll loop stays fresh here; the stale signal has its own test.
    core.last_polled_ms = Some(now + STUCK_AFTER_MS);
    let notices = core.stall_notices(now + STUCK_AFTER_MS);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].title, "Envoy events stuck");
    assert!(
        notices[0].message.contains("2 event(s) have waited 10 min")
            && notices[0].message.contains("no empty agent prompt"),
        "{}",
        notices[0].message
    );
    assert!(core.stall_notices(now + STUCK_AFTER_MS + 1_000).is_empty());
    core.last_polled_ms = Some(now + STUCK_AFTER_MS + RENOTIFY_MS);
    assert_eq!(
        core.stall_notices(now + STUCK_AFTER_MS + RENOTIFY_MS).len(),
        1,
        "repeated while it lasts"
    );
    let mut status = EnvoyStatus {
        session: crate::envoy::SessionState::Running,
        bridge: BridgeStatus::Ok,
        pending_drafts: 0,
        dry_run: true,
        stuck_events: 0,
        poll_stale: false,
    };
    core.pending_since_ms = Some(now - STUCK_AFTER_MS);
    core.apply_status(&mut status);
    assert_eq!(status.stuck_events, 2);
    assert_eq!(status.health(), crate::envoy::EnvoyHealth::EventsStuck);

    // Typing a batch is progress: the clock restarts for whatever still waits.
    let result = submit_receipt_batch(&mut core, now + STUCK_AFTER_MS, 1);
    core.pending.push_back(receipt_event("1.000003"));
    core.forward_done(&result, now + STUCK_AFTER_MS, |_| {});
    assert_eq!(core.pending_since_ms, Some(now + STUCK_AFTER_MS));
    assert_eq!(core.stuck_events(now + STUCK_AFTER_MS + 1), 0);
    assert!(core.stall_notices(now + STUCK_AFTER_MS + 1).is_empty());
    assert_eq!(core.stuck_notified_ms, None, "a new stall warns afresh");
    core.pending.clear();
    core.note_pending(now);
    assert_eq!(core.pending_since_ms, None);
}

#[test]
fn a_silent_poll_loop_turns_the_status_stale_until_the_next_result() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let (worker, _commands, updates) = Worker::test_pair();
    core.worker = Some(worker);
    let now = now_ms();
    assert!(
        !core.poll_stale(now + POLL_STALE_MS),
        "stopped: nothing to poll"
    );
    core.set_enabled("term_envoy".into(), EnvoySettings::default())
        .unwrap();
    assert!(!core.poll_stale(now + POLL_STALE_MS - 1));
    assert!(core.poll_stale(now + POLL_STALE_MS + 1_000));
    let notices = core.stall_notices(now + POLL_STALE_MS + 1_000);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].title, "Envoy poll stale");
    assert!(core.stall_notices(now + POLL_STALE_MS + 2_000).is_empty());
    updates
        .send(Update::Polled {
            inbox: Inbox::default(),
            events: vec![],
            bridge: BridgeStatus::Ok,
            notices: vec![],
            polled_at_ms: now + POLL_STALE_MS + 3_000,
        })
        .unwrap();
    core.poll_updates();
    assert!(!core.poll_stale(now + POLL_STALE_MS + 4_000));
    assert_eq!(core.stale_notified_ms, None);
    let mut status = EnvoyStatus {
        session: crate::envoy::SessionState::Running,
        bridge: BridgeStatus::Stale,
        pending_drafts: 1,
        dry_run: true,
        stuck_events: 0,
        poll_stale: false,
    };
    core.last_polled_ms = Some(now - POLL_STALE_MS);
    core.apply_status(&mut status);
    assert!(status.poll_stale);
    assert_eq!(status.health(), crate::envoy::EnvoyHealth::PollStale);
    core.stop();
    assert!(!core.poll_stale(now + 2 * POLL_STALE_MS));
}

#[test]
fn batches_say_who_each_event_is_for() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("term_envoy".into());
    let now = 1_791_300_000_000;
    let karl = core.karl().to_owned();
    assert_eq!(karl, "U0AMMLGSFL0");
    let mut dwarf_root = receipt_event("1791299800.000001");
    dwarf_root.text = "Request from Recs rollout: who owns the wheat index?".into();
    let mut envoy_root = receipt_event("1791299850.000001");
    envoy_root.text = "Anyone know the recs rollout status?".into();
    let mut tagged_other = receipt_event("1791299900.000001");
    tagged_other.text = "<@U0177NR3GG4> can your search agent reindex wheat?".into();
    let mut tagged_karl = receipt_event("1791299910.000001");
    tagged_karl.text = format!("<@{karl}> is realtime#694 ready?");
    let mut in_dwarf_thread = receipt_event("1791299920.000001");
    in_dwarf_thread.thread_ts = Some(dwarf_root.ts.clone());
    in_dwarf_thread.text = "<@U0177NR3GG4> your agent should answer this".into();
    let mut in_envoy_thread = receipt_event("1791299930.000001");
    in_envoy_thread.thread_ts = Some(envoy_root.ts.clone());
    in_envoy_thread.text = "@depict-search-agent take it from here".into();
    let mut chatter = receipt_event("1791299960.000001");
    chatter.text = "lunch?".into();
    let mut inbox = Inbox::default();
    for event in [&dwarf_root, &envoy_root] {
        inbox.insert(event.clone());
    }
    let batch = vec![
        tagged_other,
        tagged_karl,
        in_dwarf_thread,
        in_envoy_thread,
        chatter,
    ];
    for event in &batch {
        inbox.insert(event.clone());
    }
    core.merge_polled(inbox, batch);
    let mut sent = Draft::reply(
        ReplyTo {
            conversation: "C123".into(),
            ts: envoy_root.ts.clone(),
        },
        Some(envoy_root.ts.clone()),
        "Rolling out now. (Karl's AI agent)".into(),
        "status question".into(),
        now,
    );
    sent.status = DraftStatus::Sent;
    core.queue().unwrap().create(sent).unwrap();
    let dwarves = [Dwarf {
        id: "term:term_dwarf".into(),
        label: "Recs rollout".into(),
        threads: vec![format!("C123:{}", dwarf_root.ts)],
        ..Dwarf::default()
    }];
    let (action, _) = core.next_forward(now, &dwarves).expect("batch");
    let Action::Send { input, .. } = action else {
        panic!("typed batch")
    };
    let text = input.text.unwrap();
    let lines: Vec<&str> = text.split(" | ").collect();
    assert_eq!(lines.len(), 5, "{text}");
    assert!(
        lines[0].contains("— addressee: not Karl (tags <@U0177NR3GG4>, not Karl) — no dwarf match"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains("— addressee: Karl (mentions Karl) —"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].contains(
            "— addressee: unclear (tags <@U0177NR3GG4> in Karl's thread) — dwarves: Recs rollout (term:term_dwarf, its thread)"
        ),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].contains("— addressee: unclear (tags @depict-search-agent in Karl's thread) —"),
        "a thread the Envoy replied in is Karl's: {}",
        lines[3]
    );
    assert!(
        lines[4].contains("— addressee: unclear (no mention) — no dwarf match"),
        "{}",
        lines[4]
    );
}

#[test]
fn karls_id_comes_from_slack_when_it_answers() {
    let dir = tempfile::tempdir().unwrap();
    let bridge = dir.path().join("bridge");
    std::fs::create_dir_all(bridge.join("channels")).unwrap();
    std::fs::write(bridge.join("fixture-only"), "").unwrap();
    let fake = FakeSlack {
        karl_id: Some("W0RESOLVED"),
        ..FakeSlack::default()
    };
    let mut core = AppEnvoy::at(dir.path().into(), &egui::Context::default(), fake, bridge);
    assert_eq!(
        core.karl(),
        "U0AMMLGSFL0",
        "the settings' id until Slack answers"
    );
    core.set_enabled(
        "term_envoy".into(),
        EnvoySettings {
            channels: vec!["C123".into()],
            ..Default::default()
        },
    )
    .unwrap();
    wait_for(&mut core, |c| c.karl() == "W0RESOLVED");
}

#[test]
fn clearing_stuck_events_resolves_warning_and_resets_the_wait_clock() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    core.handle = Some("envoy".into());
    let event = receipt_event("1.000001");
    core.inbox.insert(event.clone());
    core.pending.push_back(event);
    core.pending_since_ms = Some(now_ms() - STUCK_AFTER_MS);
    let mut notices =
        crate::notify::NotificationCenter::new(crate::notify::history::History::default());
    for notice in core.stall_notices(now_ms()) {
        notices.dispatch(notice, &crate::notify::NotifySettings::default(), now_ms());
    }
    assert_eq!(notices.history.entries().len(), 1);
    assert!(!core.notice_resolved(&notices.history.entries()[0]));
    core.panel_action(crate::ui::envoy::DraftAction::ClearHandled, false, |_| {
        panic!("cleanup must not send")
    })
    .1
    .unwrap();
    assert_eq!(core.stuck_events(now_ms()), 0);
    assert!(core.pending_since_ms.is_none());
    assert!(core.notice_resolved(&notices.history.entries()[0]));
    assert!(core.stall_notices(now_ms() + RENOTIFY_MS).is_empty());
}

#[test]
fn envoy_asks_karl_then_delivers_a_new_event_before_the_answer() {
    use crate::ask::{AskRequest, Asks};
    use crate::clan::ClanData;
    use crate::model::{ClanSnapshot, NodeId, NodeState};
    use crate::review::mark::Origin;

    let dir = tempfile::tempdir().unwrap();
    let mut core = AppEnvoy::load(dir.path().into());
    let handle = "term_envoy";
    core.handle = Some(handle.into());
    let now = 1_791_300_000_000;
    let question = "Should tartine go live? Do you want to proceed?";
    let screen: Vec<String> = format!("Claude Code\n{question}\n❯\n? for shortcuts")
        .lines()
        .map(str::to_owned)
        .collect();
    let agent = AgentIdentity::Claude;
    let verdict = crate::detect::detect_prompt(&screen, Some(&agent));
    assert_eq!(verdict, None, "question text above the composer is history");
    let data = ClanData {
        terminals: vec![Terminal {
            handle: handle.into(),
            connected: true,
            writable: true,
            agent_identity: Some(agent.clone()),
            preview: Some(screen.join("\n")),
            ..Terminal::default()
        }],
        needs_input: [(handle.into(), verdict)].into(),
        composers: [(
            handle.into(),
            crate::detect::composer_state(&screen, Some(&agent)),
        )]
        .into(),
        ..ClanData::default()
    };
    let mut snapshot = ClanSnapshot::build(&data, now);
    snapshot.mark_envoy(
        Some(handle),
        EnvoyStatus {
            session: crate::envoy::SessionState::Running,
            bridge: BridgeStatus::Ok,
            pending_drafts: 0,
            dry_run: true,
            stuck_events: 0,
            poll_stale: false,
        },
    );
    let mut asks = Asks::new();
    let request = AskRequest {
        question: question.into(),
        context: "Elion asked in Slack; awaiting Karl's decision.".into(),
        url: Some("https://example.slack.com/archives/C123/p1791300000000001".into()),
        severity: Severity::Warn,
        origin: Origin {
            terminal_handle: Some(handle.into()),
            ..Origin::default()
        },
    };
    let (notice, waiting) = super::super::lifecycle::prepare_ask(
        &mut asks,
        snapshot.node(&NodeId::terminal(handle)),
        &request,
        now,
    );
    assert_eq!(notice.source, Source::Agent);
    assert_eq!(notice.target.as_deref(), Some("term:term_envoy"));
    assert_eq!(notice.url, request.url);
    assert!(notice.message.contains(question));
    assert!(notice.message.contains(&request.context));
    let mut notifications = crate::notify::NotificationCenter::default();
    let outcome = notifications.dispatch(notice, &crate::notify::NotifySettings::default(), now);
    assert!(matches!(outcome, crate::notify::Outcome::Delivered { .. }));
    snapshot.mark_waiting(&asks.waiting(&data, None));

    let event = receipt_event("1791300000.000001");
    let mut inbox = Inbox::default();
    inbox.insert(event.clone());
    core.merge_polled(inbox, vec![event]);
    assert_eq!(
        target_blocked(Some(&data), Some(&snapshot), handle, false),
        None
    );
    assert!(!waiting);
    assert!(asks.is_empty());
    assert_eq!(
        snapshot.node(&NodeId::terminal(handle)).unwrap().state,
        NodeState::Idle
    );
    struct TerminalSink(crossbeam_channel::Sender<Action>);
    impl crate::actions::ActionExecutor for TerminalSink {
        fn execute(&self, action: &Action) -> Result<(), String> {
            self.0.send(action.clone()).map_err(|e| e.to_string())
        }
    }
    let (sent, received) = crossbeam_channel::unbounded();
    let mut runner = crate::actions::ActionRunner::spawn(TerminalSink(sent), None);
    let (action, batch) = core.next_forward(now, &[]).expect("pending batch");
    let id = runner.submit(action);
    core.forwarding = Some((id, batch));
    let result = runner
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("delivery result");
    assert!(result.outcome.is_ok());
    assert_eq!(received.try_recv().unwrap(), result.action);
    let Action::Send { handle: to, input } = &result.action else {
        panic!("typed batch")
    };
    assert_eq!(to, handle);
    assert!(input.enter);
    assert!(input.text.as_deref().unwrap().contains("Fixture message"));
    let mut traffic = Vec::new();
    core.forward_done(&result, now, |m| traffic.push(m));
    assert_eq!(traffic.len(), 1);
    assert!(core.pending.is_empty());
    assert!(core.inbox.unforwarded().is_empty());
    let entry = &notifications.history.entries()[0];
    assert!(
        !entry.read,
        "delivering a Slack batch must not dismiss Karl's question"
    );
    assert_eq!(entry.target.as_deref(), Some("term:term_envoy"));
    assert!(entry.message.contains(question));

    assert_eq!(
        target_blocked(Some(&data), Some(&snapshot), handle, true),
        Some("input pending")
    );
    let mut busy = snapshot.clone();
    busy.nodes
        .iter_mut()
        .find(|n| n.id == NodeId::terminal(handle))
        .unwrap()
        .state = NodeState::Working;
    assert_eq!(
        target_blocked(Some(&data), Some(&busy), handle, false),
        Some("not idle")
    );
    let mut prompt = data.clone();
    let permission: Vec<String> = "Do you want to proceed?\n❯ 1. Yes\n  2. No"
        .lines()
        .map(str::to_owned)
        .collect();
    prompt.needs_input.insert(
        handle.into(),
        crate::detect::detect_prompt(&permission, Some(&agent)),
    );
    let mut blocked = ClanSnapshot::build(&prompt, now);
    blocked.mark_envoy(
        Some(handle),
        EnvoyStatus {
            session: crate::envoy::SessionState::Running,
            bridge: BridgeStatus::Ok,
            pending_drafts: 0,
            dry_run: true,
            stuck_events: 0,
            poll_stale: false,
        },
    );
    assert_eq!(
        blocked.node(&NodeId::terminal(handle)).unwrap().state,
        NodeState::NeedsInput
    );
    assert_eq!(
        target_blocked(Some(&prompt), Some(&blocked), handle, false),
        Some("not idle")
    );
}

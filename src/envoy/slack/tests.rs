use super::*;
use serde_json::json;
use std::collections::VecDeque;
use std::io::Write;

#[derive(Default)]
struct Fake {
    replies: VecDeque<Value>,
    pins: VecDeque<Value>,
    methods: Vec<String>,
    subscriptions: Vec<(PathBuf, String)>,
    subscribe_error: Option<String>,
    delay: Duration,
    calls: Vec<(String, Vec<(String, String)>)>,
    scripted: BTreeMap<String, VecDeque<Result<Value, String>>>,
}
impl SlackSender for Fake {
    fn send(&mut self, _: &OutboundMessage) -> Result<SendReceipt, String> {
        panic!("ingestion cannot send")
    }
}
impl SlackIo for Fake {
    fn pr_head(&mut self, _: &str, repo: &str, number: u64) -> Result<Option<String>, String> {
        self.methods.push(format!("pr_head {repo}#{number}"));
        Ok((number != 404).then(|| format!("karl/{repo}-{number}")))
    }
    fn api(&mut self, method: &str, params: &[(&str, String)]) -> Result<Value, String> {
        std::thread::sleep(self.delay);
        self.methods.push(method.into());
        self.calls.push((
            method.into(),
            params
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        ));
        if let Some(value) = self.scripted.get_mut(method).and_then(VecDeque::pop_front) {
            return value;
        }
        if method == "usergroups.list" {
            return Ok(json!({"usergroups": []}));
        }
        let pins = params.iter().any(|(k, v)| *k == "query" && v == "has:pin");
        Ok(if pins {
            &mut self.pins
        } else {
            &mut self.replies
        }
        .pop_front()
        .unwrap_or_else(|| json!({"ok":true,"messages":{"matches":[],"paging":{"pages":1}}})))
    }
    fn subscribe(&mut self, dir: &Path, channel: &str) -> Result<(), String> {
        self.subscriptions.push((dir.into(), channel.into()));
        self.subscribe_error.clone().map_or(Ok(()), Err)
    }
}
fn channel(dir: &Path) -> SlackChannel<Fake> {
    SlackChannel::new(
        Fake::default(),
        {
            let mut inbox = Inbox::default();
            inbox.first_enabled_ms = Some(NOW - 86_400_000);
            inbox
        },
        dir.join("inbox.json"),
        dir.into(),
        EnvoySettings {
            channels: vec!["C123".into()],
            user_id: "U0KARL0001".into(),
            github_owner: "acme-org".into(),
            ..EnvoySettings::default()
        },
    )
}
const NOW: i64 = 1_790_000_000_000;
#[test]
fn fixture_tail_partial_dedupe_error_rotation_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("channels")).unwrap();
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW).unwrap().to_string(),
    )
    .unwrap();
    let feed = dir.path().join("channels/C123.jsonl");
    fs::write(
        &feed,
        include_bytes!("../../../tests/fixtures/envoy/channel.jsonl"),
    )
    .unwrap();
    let mut c = channel(dir.path());
    let first = c.poll(NOW).unwrap();
    assert_eq!(first.events.len(), 2);
    assert_eq!(c.health(), BridgeStatus::Stale);
    assert!(
        first
            .notices
            .iter()
            .any(|s| s.contains("subscription error"))
    );
    assert_eq!(c.io.subscriptions[0].0, dir.path());
    assert!(c.poll(NOW + 1).unwrap().events.is_empty());
    let mut out = fs::OpenOptions::new().append(true).open(&feed).unwrap();
    out.write_all(
        br#"{"type":"message","channel":"C123","ts":"1790000001.000003","text":"A partial"#,
    )
    .unwrap();
    assert!(c.poll(NOW + 2).unwrap().events.is_empty());
    let mut loaded = channel(dir.path());
    loaded.inbox = Inbox::load(&dir.path().join("inbox.json")).unwrap();
    out.write_all(b" message\",\"user\":\"U123\"}\n").unwrap();
    assert_eq!(loaded.poll(NOW + 3).unwrap().events.len(), 1);
    let original = fs::read(&feed).unwrap();
    fs::rename(&feed, dir.path().join("old.jsonl")).unwrap();
    fs::write(&feed, original).unwrap();
    assert!(loaded.poll(NOW + 4).unwrap().events.is_empty());
    assert_eq!(loaded.inbox.events().len(), 3);
}
#[test]
fn heartbeat_stale_missing_and_invalid() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(heartbeat(dir.path(), NOW), BridgeStatus::Missing);
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW - 90_000)
            .unwrap()
            .to_string(),
    )
    .unwrap();
    assert_eq!(heartbeat(dir.path(), NOW), BridgeStatus::Ok);
    assert_eq!(heartbeat(dir.path(), NOW + 1), BridgeStatus::Stale);
    fs::write(dir.path().join("heartbeat"), "invalid").unwrap();
    assert_eq!(heartbeat(dir.path(), NOW), BridgeStatus::Stale);
}
#[test]
fn sanitized_search_filters_dms_and_self() {
    let mut fixture: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/envoy/search.json")).unwrap();
    assert_eq!(
        parse_search(&fixture, EventKind::Dm, "U111").unwrap().len(),
        2
    );
    assert_eq!(
        parse_search(&fixture, EventKind::Dm, "U123").unwrap().len(),
        1
    );
    fixture["messages"]["matches"][0]["channel"]["is_im"] = json!(false);
    fixture["messages"]["matches"][0]["channel"]["is_mpim"] = json!(false);
    assert_eq!(
        parse_search(&fixture, EventKind::Dm, "U111").unwrap().len(),
        1
    );
    assert_eq!(
        parse_search(&fixture, EventKind::Mention, "U111")
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn pagination_and_fallback_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    let mut page: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/envoy/search.json")).unwrap();
    page["messages"]["paging"]["pages"] = json!(2);
    let empty = json!({"messages":{"matches":[],"paging":{"pages":1}}});
    c.io.scripted.insert(
        "search.messages".into(),
        [Ok(page), Ok(empty.clone()), Ok(empty.clone()), Ok(empty)].into(),
    );
    let history = json!({"messages":[{"ts":"1790000000.000009","user":"U321","text":"History"}]});
    c.io.scripted.insert(
        "conversations.history".into(),
        [Ok(history.clone()), Ok(history)].into(),
    );
    assert_eq!(c.poll(NOW + 10).unwrap().events.len(), 3);
    assert!(!c.inbox.poll_watermarks.contains_key("mention:U0KARL0001"));
    c.poll(NOW + 2010).unwrap();
    assert_eq!(c.inbox.poll_watermarks["mention:U0KARL0001"], NOW + 10);
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW + 10);
}

#[test]
fn all_bridge_line_types_and_api_failures() {
    for reason in ["heartbeat_stale", "heartbeat_missing", "expiring"] {
        assert!(matches!(
            parse_bridge_line(
                format!("{{\"type\":\"warning\",\"reason\":\"{reason}\"}}").as_bytes()
            )
            .unwrap(),
            BridgeLine::Warning(_)
        ));
    }
    assert!(matches!(
        parse_bridge_line(b"{\"type\":\"future\"}").unwrap(),
        BridgeLine::Unknown
    ));
    assert!(parse_bridge_line(b"garbage").is_err());
    assert!(parse_api("banner\n{\"ok\":true}").is_ok());
    assert!(
        parse_api("{\"ok\":false,\"error\":\"not_authed\"}")
            .unwrap_err()
            .contains("not_authed")
    );
    assert!(binary("nonexistent-envoy-command-for-test").is_err());
}

#[test]
fn delivery_checkpoint_survives_restart_and_storage_failures_do_not_skip_events() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("channels")).unwrap();
    fs::write(
        dir.path().join("channels/C123.jsonl"),
        include_bytes!("../../../tests/fixtures/envoy/channel.jsonl"),
    )
    .unwrap();
    let mut c = channel(dir.path());
    let good_path = c.path.clone();
    c.path = dir.path().join("not-a-directory/inbox.json");
    fs::write(dir.path().join("not-a-directory"), "occupied").unwrap();
    assert!(c.poll(NOW).is_err());
    assert!(c.inbox.events().is_empty());
    assert!(c.inbox.cursors.is_empty());
    c.path = good_path;
    assert_eq!(c.poll(NOW + 1).unwrap().events.len(), 2);
    let ids = vec![c.inbox.events()[0].id()];
    c.mark_forwarded(&ids).unwrap();
    let restored = Inbox::load(&c.path).unwrap();
    assert_eq!(restored.unforwarded().len(), 1);
}

#[test]
fn failed_search_keeps_watermark_and_thread_context_is_paginated() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.io.replies = [json!({"ok":true,"messages":{"matches":[]}})].into();
    let mut batch = PollBatch::default();
    assert!(c.search(EventKind::Mention, NOW, &mut batch).is_err());
    assert!(c.inbox.poll_watermarks.is_empty());
    let root = event(
        &json!({"ts":"1790000000.000001","user":"U123","text":"Root"}),
        "C123",
        EventKind::Mention,
    )
    .unwrap();
    c.io.replies = [json!({"ok":true,"messages":[{"ts":"1790000000.000001","text":"Root"}],"has_more":true,"response_metadata":{"next_cursor":"next"}}),json!({"ok":true,"messages":[{"ts":"1790000000.000002","thread_ts":"1790000000.000001","text":"Reply"}],"has_more":false})].into();
    let thread = c.thread(&root).unwrap();
    assert_eq!(thread.len(), 2);
    assert_eq!(thread[1].thread_ts.as_deref(), Some(root.ts.as_str()));
}

#[test]
fn quiet_ticks_do_not_rewrite_inbox() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW).unwrap().to_string(),
    )
    .unwrap();
    let mut c = channel(dir.path());
    c.poll(NOW).unwrap();
    assert_eq!(c.health(), BridgeStatus::Ok);
    assert!(c.errors.is_empty());
    let modified = fs::metadata(&c.path).unwrap().modified().unwrap();
    c.poll(NOW + 1).unwrap();
    assert_eq!(fs::metadata(&c.path).unwrap().modified().unwrap(), modified);
}

#[test]
fn reenable_reasserts_channel_subscriptions() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.poll(NOW).unwrap();
    assert_eq!(c.io.subscriptions.len(), 1);
    c.configure(c.settings.clone());
    c.poll(NOW + 1).unwrap();
    assert_eq!(c.io.subscriptions.len(), 2);
    assert!(
        c.io.subscriptions
            .iter()
            .all(|(path, channel)| path == dir.path() && channel == "C123")
    );
}

#[test]
fn first_enable_ignores_backlog_and_persists_cutoff() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("channels")).unwrap();
    let feed = dir.path().join("channels/C123.jsonl");
    fs::write(
        &feed,
        include_bytes!("../../../tests/fixtures/envoy/channel.jsonl"),
    )
    .unwrap();
    let mut c = channel(dir.path());
    c.inbox = Inbox::default();
    assert!(c.poll(NOW + 10).unwrap().events.is_empty());
    assert_eq!(c.inbox.first_enabled_ms, Some(NOW + 10));
    let mut out = fs::OpenOptions::new().append(true).open(&feed).unwrap();
    writeln!(out, "{}", json!({"type":"message","channel":"C123","ts":"1790000001.000001","user":"U123","text":"New"})).unwrap();
    let mut restarted = channel(dir.path());
    restarted.inbox = Inbox::load(&c.path).unwrap();
    assert_eq!(restarted.poll(NOW + 2000).unwrap().events.len(), 1);
    assert_eq!(restarted.inbox.first_enabled_ms, Some(NOW + 10));
}

#[test]
fn bridge_error_survives_successful_subscribe_until_new_feed_line() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("channels")).unwrap();
    let feed = dir.path().join("channels/C123.jsonl");
    fs::write(
        &feed,
        "{\"type\":\"error\",\"reason\":\"not_in_channel\"}\n",
    )
    .unwrap();
    let mut c = channel(dir.path());
    let first = c.poll(NOW).unwrap();
    c.poll(NOW + 120_000).unwrap();
    assert!(c.errors.contains_key("C123"));
    assert_eq!(c.io.subscriptions.len(), 2);
    assert!(
        c.io.methods
            .iter()
            .filter(|m| *m == "conversations.history")
            .count()
            >= 2
    );
    let next = c.poll(NOW + 120_001).unwrap();
    assert_eq!(first.notices, next.notices);
    fs::write(&feed, "{\"type\":\"reaction\",\"reaction\":\"eyes\"}\n").unwrap();
    c.poll(NOW + 120_002).unwrap();
    assert!(c.errors.is_empty());
}

#[test]
fn search_and_history_resume_pages_without_advancing_watermarks() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.io.replies =
        std::iter::repeat_n(json!({"messages":{"matches":[],"paging":{"pages":100}}}), 6).collect();
    for i in 0..6 {
        assert!(
            c.search(EventKind::Mention, NOW + i, &mut PollBatch::default())
                .unwrap()
        );
    }
    assert_eq!(c.io.methods.len(), 6);
    assert!(!c.inbox.poll_watermarks.contains_key("mention:U0KARL0001"));
    assert_eq!(c.inbox.searches["mention:U0KARL0001"].page, 7);
    c.io.methods.clear();
    c.io.replies = std::iter::repeat_n(
        json!({"messages":[],"response_metadata":{"next_cursor":"next"}}),
        6,
    )
    .collect();
    for i in 0..6 {
        assert!(
            c.history("C123", NOW + i, &mut PollBatch::default())
                .unwrap()
        );
    }
    assert_eq!(c.io.methods.len(), 6);
    assert!(!c.inbox.poll_watermarks.contains_key("history:C123"));
    c.inbox.save(&c.path).unwrap();
    let mut restarted = channel(dir.path());
    restarted.inbox = Inbox::load(&c.path).unwrap();
    restarted.io.replies = [json!({"messages":[]})].into();
    assert!(
        !restarted
            .history("C123", NOW + 100, &mut PollBatch::default())
            .unwrap()
    );
    assert_eq!(restarted.inbox.poll_watermarks["history:C123"], NOW);
    assert!(
        restarted.io.calls[0]
            .1
            .contains(&("cursor".into(), "next".into()))
    );
}

#[test]
fn subscription_command_retry_recovers_even_in_a_quiet_channel() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW).unwrap().to_string(),
    )
    .unwrap();
    let mut c = channel(dir.path());
    c.io.subscribe_error = Some("Subscription command failed".into());
    c.poll(NOW).unwrap();
    assert_eq!(c.health(), BridgeStatus::Stale);
    assert!(!c.command_errors.is_empty());
    c.io.subscribe_error = None;
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW + 120_000)
            .unwrap()
            .to_string(),
    )
    .unwrap();
    c.poll(NOW + 120_000).unwrap();
    assert!(c.command_errors.is_empty());
    assert_eq!(c.health(), BridgeStatus::Ok);
    let mut saved = Inbox::default();
    saved
        .subscription_errors
        .insert("C123".into(), "Subscription command failed".into());
    let mut restarted = SlackChannel::new(
        Fake::default(),
        saved,
        dir.path().join("restart.json"),
        dir.path().into(),
        c.settings.clone(),
    );
    restarted.poll(NOW + 120_000).unwrap();
    assert_eq!(restarted.health(), BridgeStatus::Ok);
}

#[test]
fn recovered_feed_read_does_not_need_a_message_or_clear_a_bridge_error() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW).unwrap().to_string(),
    )
    .unwrap();
    let feed = dir.path().join("channels/C123.jsonl");
    fs::create_dir_all(&feed).unwrap();
    let mut c = channel(dir.path());
    c.poll(NOW).unwrap();
    assert!(!c.read_errors.is_empty());
    assert_eq!(c.health(), BridgeStatus::Stale);
    fs::remove_dir(&feed).unwrap();
    fs::write(&feed, "").unwrap();
    c.poll(NOW + 1).unwrap();
    assert!(c.read_errors.is_empty());
    assert_eq!(c.health(), BridgeStatus::Ok);
    c.errors.insert(
        "C123".into(),
        "Bridge subscription error; check channel membership".into(),
    );
    c.poll(NOW + 2).unwrap();
    assert_eq!(c.health(), BridgeStatus::Stale);
    assert!(c.errors.contains_key("C123"));
}

#[test]
fn only_pins_added_after_the_first_poll_become_events() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    let pinned = |ts: &str| {
        json!({"ok":true,"messages":{"paging":{"pages":1},"matches":[
            {"ts":ts,"user":"U321","username":"avery","text":"Pinned plan","channel":{"id":"C777"}}
        ]}})
    };
    c.io.pins = [pinned("1700000000.000001"), pinned("1700000000.000001")].into();
    assert!(
        c.poll(NOW + 10).unwrap().events.is_empty(),
        "existing pins seed"
    );
    let mut both = pinned("1700000000.000001");
    both["messages"]["matches"]
        .as_array_mut()
        .unwrap()
        .push(json!({"ts":"1600000000.000002","user":"U321","text":"Old message, new pin","channel":{"id":"C777"}}));
    c.io.pins = [both].into();
    c.next_jobs.clear();
    let events = c.poll(NOW + 20).unwrap().events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::Pin);
    assert_eq!(events[0].id(), "C777:1600000000.000002");
    c.io.pins = [pinned("1600000000.000002")].into();
    c.next_jobs.clear();
    assert!(
        c.poll(NOW + 30).unwrap().events.is_empty(),
        "a pin is reported once"
    );
}

#[test]
fn pr_refs_resolve_to_head_branches_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    let mut e = parse_search(
        &serde_json::from_str(include_str!("../../../tests/fixtures/envoy/search.json")).unwrap(),
        EventKind::Mention,
        "U111",
    )
    .unwrap()
    .remove(0);
    e.text = "api#694 and api#404, also #12 and ACME-1".into();
    c.resolve_prs(std::slice::from_ref(&e));
    assert_eq!(c.inbox.pr_heads["api#694"], "karl/api-694");
    assert_eq!(
        c.inbox.pr_heads["api#404"], "",
        "a missing PR is cached too"
    );
    assert_eq!(
        c.inbox.pr_heads.len(),
        2,
        "bare #N and tickets need no lookup"
    );
    c.resolve_prs(std::slice::from_ref(&e));
    assert_eq!(c.io.methods.len(), 2, "cached refs are not looked up again");
}

#[test]
fn karls_id_is_resolved_from_auth_test() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    assert_eq!(c.settings.user_id, "U0KARL0001");
    c.io.replies
        .push_back(json!({"ok":true,"user":"karl","user_id":"u0karl0002","team":"Acme"}));
    assert_eq!(c.resolve_user_id().unwrap().as_deref(), Some("U0KARL0002"));
    c.io.replies
        .push_back(json!({"ok":true,"user":"karl","user_id":"W0NEWID","team":"Acme"}));
    assert_eq!(c.resolve_user_id().unwrap().as_deref(), Some("W0NEWID"));
    assert_eq!(
        c.settings.user_id, "W0NEWID",
        "the resolved id replaces the configured one"
    );
    c.io.replies.push_back(json!({"ok":true,"team":"Acme"}));
    assert_eq!(
        c.resolve_user_id().unwrap(),
        None,
        "no id: the configured one stands"
    );
    assert_eq!(c.settings.user_id, "W0NEWID");
    c.io.replies
        .push_back(json!({"ok":true,"user_id":"C000AGENT01"}));
    assert!(
        c.resolve_user_id().is_err(),
        "a conversation id is not a member id"
    );
    assert_eq!(c.settings.user_id, "W0NEWID");
    assert_eq!(c.io.methods.iter().filter(|m| *m == "auth.test").count(), 4);
}

#[test]
fn ingest_regression_self_roots_survive_without_inbox_messages() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    let root = event(
        &json!({"ts":"1790000000.000001", "user":"U0KARL0001", "text":"question"}),
        "C123",
        EventKind::AgentComms,
    )
    .unwrap();
    c.ingest(root, &mut PollBatch::default());
    assert!(c.inbox.events().is_empty());
    let stored = serde_json::to_value(&c.inbox).unwrap();
    assert_eq!(stored["user_threads"], json!(["C123:1790000000.000001"]));
}

#[test]
fn ingest_regression_recovered_bridge_backfills_old_thread_replies() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.inbox.first_enabled_ms = Some(NOW - 30 * 86_400_000);
    c.inbox
        .poll_watermarks
        .insert("history:C123".into(), NOW - 600_000);
    c.poll(NOW).unwrap(); // Default fake history fails during the outage.
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW + 60_000)
            .unwrap()
            .to_string(),
    )
    .unwrap();
    let root = json!({"messages":[{"ts":"1789827200.000001", "user":"UOTHER", "text":"old root", "reply_count":1, "latest_reply":"1790000001.000001"}]});
    c.io.scripted.insert(
        "conversations.history".into(),
        [Ok(root.clone()), Ok(root)].into(),
    );
    let reply = json!({"messages":[{"ts":"1790000001.000001", "thread_ts":"1789827200.000001", "user":"UOTHER", "text":"missed reply"}]});
    c.io.scripted.insert(
        "conversations.replies".into(),
        [Ok(reply.clone()), Ok(reply)].into(),
    );
    c.poll(NOW + 60_000).unwrap();
    let batch = c.poll(NOW + 62_000).unwrap();
    assert!(
        batch
            .events
            .iter()
            .any(|e| e.text == "missed reply" && e.kind == EventKind::ThreadReply)
    );
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW + 60_000);
    assert!(!c.inbox.recovery_needed_through.contains_key("C123"));
    assert!(
        c.io.calls
            .iter()
            .filter(|(method, _)| method == "conversations.history")
            .all(|(_, params)| params.iter().any(|(k, _)| k == "oldest"))
    );
}

#[test]
fn group_membership_is_cached_searched_and_scope_failure_is_best_effort() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.io.scripted.insert(
        "usergroups.list".into(),
        [Ok(json!({"usergroups":[
            {"id":"SENG", "users":["U0KARL0001", "UOTHER"]},
            {"id":"SOTHER", "users":["UOTHER"]}
        ]}))]
        .into(),
    );
    c.groups().unwrap();
    assert_eq!(c.inbox.user_groups, BTreeSet::from(["SENG".into()]));
    assert_eq!(c.io.calls[0].1, [("include_users".into(), "true".into())]);
    c.search_tag(
        EventKind::Mention,
        Some("SENG"),
        NOW,
        &mut PollBatch::default(),
    )
    .unwrap();
    assert!(
        c.io.calls[1]
            .1
            .iter()
            .any(|(k, v)| k == "query" && v.starts_with("<!subteam^SENG> after:"))
    );
    c.io.scripted.insert(
        "usergroups.list".into(),
        [Err("Slack: missing_scope".into())].into(),
    );
    assert!(c.groups().is_err());
    let calls = c.io.calls.len();
    c.groups().unwrap();
    assert_eq!(
        c.io.calls.len(),
        calls,
        "missing scope is not retried each poll"
    );
    assert!(c.groups_error_logged);
    assert!(
        c.inbox.user_groups.contains("SENG"),
        "transient failures preserve cache"
    );
}

#[test]
fn expired_poll_defers_network_and_rate_limits_back_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.poll_bounded(NOW, Duration::ZERO).unwrap();
    assert!(c.io.calls.is_empty());
    assert!(c.io.subscriptions.is_empty());
    assert!(c.deadline.is_none());
    c.io.scripted.insert(
        "search.messages".into(),
        [Err("Slack: ratelimited".into()), Ok(json!({"ok":true}))].into(),
    );
    assert!(c.api("search.messages", &[]).is_err());
    assert!(
        c.api("search.messages", &[])
            .unwrap_err()
            .contains("retry deferred")
    );
    assert_eq!(c.io.calls.len(), 1);
    c.api_backoff
        .insert("search.messages".into(), Instant::now());
    assert!(c.api("search.messages", &[]).is_ok());
    assert_eq!(c.io.calls.len(), 2);
}

#[test]
fn cli_timeout_is_clipped_to_the_poll_deadline() {
    let mut cli = SlackCli;
    cli.set_deadline(Some(Instant::now() + Duration::from_millis(30)));
    let start = Instant::now();
    let result = run("sleep", &["2".into()], None);
    cli.set_deadline(None);
    assert!(result.is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(cli_budget().unwrap(), Duration::from_secs(25));
}

#[test]
fn returning_bridge_during_backfill_keeps_the_uncovered_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.inbox.first_enabled_ms = Some(NOW - 30 * 86_400_000);
    c.inbox
        .poll_watermarks
        .insert("history:C123".into(), NOW - 600_000);
    c.inbox.recovery_needed_through.insert("C123".into(), NOW);
    c.io.scripted.insert("conversations.history".into(), [
        Ok(json!({"messages":[], "response_metadata":{"next_cursor":"older"}})),
        Ok(json!({"messages":[]})),
        Ok(json!({"messages":[{"ts":"1789827200.000001", "user":"UOTHER", "text":"old root", "latest_reply":"1790000001.000001"}]})),
    ].into());
    assert!(c.history("C123", NOW, &mut PollBatch::default()).unwrap());
    c.status = BridgeStatus::Ok;
    c.inbox
        .recovery_needed_through
        .insert("C123".into(), NOW + 60_000);
    c.inbox.save(&c.path).unwrap();
    c.inbox = Inbox::load(&c.path).unwrap();
    assert!(
        !c.history("C123", NOW + 60_000, &mut PollBatch::default())
            .unwrap()
    );
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW);
    assert!(c.inbox.recovery_needed_through.contains_key("C123"));
    let mut batch = PollBatch::default();
    assert!(c.history("C123", NOW + 62_000, &mut batch).unwrap());
    c.io.scripted.insert(
        "conversations.replies".into(),
        [Ok(json!({"messages":[
            {"ts":"1790000001.000001", "user":"UOTHER", "text":"during recovery"}
        ]}))]
        .into(),
    );
    assert!(!c.history("C123", NOW + 64_000, &mut batch).unwrap());
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].text, "during recovery");
    assert!(!c.inbox.recovery_needed_through.contains_key("C123"));
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW + 62_000);
}

#[test]
fn failed_reply_page_keeps_checkpoint_and_retries_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.inbox.first_enabled_ms = Some(NOW - 30 * 86_400_000);
    c.inbox
        .poll_watermarks
        .insert("history:C123".into(), NOW - 600_000);
    c.io.replies.push_back(json!({"messages":[{"ts":"1789827200.000001", "user":"UOTHER", "latest_reply":"1790000000.000001"}]}));
    assert!(
        c.history("C123", NOW + 10, &mut PollBatch::default())
            .unwrap()
    );
    c.io.scripted.insert(
        "conversations.replies".into(),
        [Err("offline".into())].into(),
    );
    assert!(
        c.history("C123", NOW + 20, &mut PollBatch::default())
            .is_err()
    );
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW - 600_000);
    c.inbox.save(&c.path).unwrap();
    let mut restart = channel(dir.path());
    restart.inbox = Inbox::load(&c.path).unwrap();
    restart.io.scripted.insert(
        "conversations.replies".into(),
        [Ok(
            json!({"messages":[{"ts":"1790000000.000001", "user":"UOTHER", "text":"retry"}]}),
        )]
        .into(),
    );
    let mut batch = PollBatch::default();
    assert!(!restart.history("C123", NOW + 30, &mut batch).unwrap());
    assert_eq!(batch.events.len(), 1);
    assert_eq!(restart.inbox.poll_watermarks["history:C123"], NOW + 10);
}

#[test]
fn deadline_rotates_to_deferred_jobs_instead_of_restarting_mentions() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.io.delay = Duration::from_millis(20);
    c.poll_bounded(NOW, Duration::from_millis(5)).unwrap();
    assert_eq!(c.io.calls.len(), 1);
    assert!(
        c.io.calls[0]
            .1
            .iter()
            .any(|(k, v)| k == "query" && v.starts_with("<@"))
    );
    c.poll_bounded(NOW + 2000, Duration::from_millis(5))
        .unwrap();
    assert_eq!(c.io.calls.len(), 2);
    assert!(
        c.io.calls[1]
            .1
            .iter()
            .any(|(k, v)| k == "query" && v.starts_with("to:"))
    );
}

#[test]
fn full_reply_handoff_retains_backfill_page() {
    use crate::envoy::slack::threads::{MatchedReply, Watch, WatchState};
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.inbox.first_enabled_ms = Some(NOW - 30 * 86_400_000);
    c.inbox
        .poll_watermarks
        .insert("history:C123".into(), NOW - 600_000);
    let root = "1789827200.000001";
    c.inbox.thread_watches.insert(
        "request".into(),
        WatchState {
            watch: Watch {
                request_id: "request".into(),
                thread: super::super::requests::Thread {
                    conversation: "C123".into(),
                    ts: root.into(),
                },
                after_us: 0,
                borrowed: true,
            },
            seen: BTreeSet::new(),
            api_cursor: String::new(),
            caught_up: true,
            subscribed: true,
            next_poll_ms: 0,
        },
    );
    let event = event(
        &json!({"ts":"1790000000.000001", "user":"UOTHER", "thread_ts":root}),
        "C123",
        EventKind::ThreadReply,
    )
    .unwrap();
    c.inbox.pending_replies = vec![
        MatchedReply {
            request_id: "other".into(),
            event
        };
        1000
    ];
    c.io.replies
        .push_back(json!({"messages":[{"ts":root,"latest_reply":"1790000000.000001"}]}));
    c.history("C123", NOW + 10, &mut PollBatch::default())
        .unwrap();
    let reply = json!({"messages":[{"ts":"1790000000.000001", "user":"UOTHER", "text":"held"}]});
    c.io.scripted.insert(
        "conversations.replies".into(),
        [Ok(reply.clone()), Ok(reply)].into(),
    );
    assert!(
        c.history("C123", NOW + 20, &mut PollBatch::default())
            .unwrap_err()
            .contains("handoff full")
    );
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW - 600_000);
    c.inbox.pending_replies.clear();
    let mut batch = PollBatch::default();
    assert!(!c.history("C123", NOW + 30, &mut batch).unwrap());
    assert_eq!(batch.events.len(), 1);
    assert_eq!(c.inbox.pending_replies.len(), 1);
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW + 10);
}

#[test]
fn fresh_heartbeat_after_sleep_does_not_skip_unobserved_gap() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    fs::create_dir_all(dir.path().join("channels")).unwrap();
    fs::write(dir.path().join("channels/C123.jsonl"), "").unwrap();
    c.status = BridgeStatus::Ok;
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW).unwrap().to_string(),
    )
    .unwrap();
    c.poll(NOW).unwrap();
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW);
    let wake = NOW + 3_600_000;
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(wake).unwrap().to_string(),
    )
    .unwrap();
    c.io.scripted.insert(
        "conversations.history".into(),
        [Err("offline".into()), Err("offline".into())].into(),
    );
    c.poll(wake).unwrap();
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW);
    assert_eq!(c.inbox.recovery_needed_through["C123"], wake);
}

#[test]
fn healthy_polls_advance_recent_checkpoint_and_drop_removed_job_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    fs::create_dir_all(dir.path().join("channels")).unwrap();
    fs::write(dir.path().join("channels/C123.jsonl"), "").unwrap();
    c.status = BridgeStatus::Ok;
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW).unwrap().to_string(),
    )
    .unwrap();
    c.poll_errors
        .insert("watch:removed".into(), "old failure".into());
    c.poll_errors
        .insert("history:COLD".into(), "old channel".into());
    c.poll(NOW).unwrap();
    assert_eq!(c.inbox.poll_watermarks["recent:C123"], NOW);
    assert!(!c.poll_errors.contains_key("watch:removed"));
    assert!(!c.poll_errors.contains_key("history:COLD"));
}

#[test]
fn incomplete_bridge_tail_cannot_advance_last_good_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.status = BridgeStatus::Ok;
    c.inbox
        .poll_watermarks
        .insert("history:C123".into(), NOW - 1000);
    fs::create_dir_all(dir.path().join("channels")).unwrap();
    fs::write(dir.path().join("channels/C123.jsonl"), "{\"type\":").unwrap();
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW).unwrap().to_string(),
    )
    .unwrap();
    c.poll(NOW).unwrap();
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW - 1000);
}

#[test]
fn failed_save_retries_the_sleep_recovery_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.status = BridgeStatus::Ok;
    fs::create_dir_all(dir.path().join("channels")).unwrap();
    fs::write(dir.path().join("channels/C123.jsonl"), "").unwrap();
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW).unwrap().to_string(),
    )
    .unwrap();
    c.poll(NOW).unwrap();
    let wake = NOW + 3_600_000;
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(wake).unwrap().to_string(),
    )
    .unwrap();
    let good_path = c.path.clone();
    fs::write(dir.path().join("occupied"), "file").unwrap();
    c.path = dir.path().join("occupied/inbox.json");
    assert!(c.poll(wake).is_err());
    c.path = good_path;
    c.poll(wake + 1).unwrap();
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW);
    assert_eq!(c.inbox.recovery_needed_through["C123"], wake + 1);
}

#[test]
fn deleted_event_cannot_be_recaptured_as_request_reply() {
    let dir = tempfile::tempdir().unwrap();
    let mut channel = channel(dir.path());
    let event: InboundEvent = serde_json::from_value(json!({"channel_kind":"slack", "conversation":"C123", "ts":"1790000001.000001", "thread_ts":"1790000000.000001", "user":"UOTHER", "user_name":"Avery", "text":"Reply", "kind":"thread_reply"})).unwrap();
    channel
        .watches(vec![super::threads::Watch {
            request_id: "request".into(),
            thread: crate::envoy::requests::Thread {
                conversation: "C123".into(),
                ts: "1790000000.000001".into(),
            },
            after_us: 1_790_000_000_000_001,
            borrowed: true,
        }])
        .unwrap();
    let mut batch = PollBatch::default();
    assert!(channel.ingest(event.clone(), &mut batch));
    assert_eq!(channel.inbox.pending_replies.len(), 1);
    channel.delete_events(&[event.id()]).unwrap();
    let path = dir.path().join("inbox.json");
    let mut restarted = SlackChannel::new(
        Fake::default(),
        Inbox::load(&path).unwrap(),
        path,
        dir.path().into(),
        channel.settings.clone(),
    );
    restarted
        .inbox
        .thread_watches
        .values_mut()
        .for_each(|s| s.seen.clear());
    let mut batch = PollBatch::default();
    assert!(restarted.ingest(event, &mut batch));
    assert!(batch.events.is_empty());
    assert!(restarted.inbox.pending_replies.is_empty());
}

#[test]
fn rework_deep_history_has_a_seven_day_root_horizon() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.inbox.first_enabled_ms = Some(NOW - 30 * 86_400_000);
    c.inbox.poll_watermarks.insert("history:C123".into(), NOW);
    c.io.replies.push_back(json!({"messages":[]}));
    c.history("C123", NOW, &mut PollBatch::default()).unwrap();
    let oldest = c.io.calls[0]
        .1
        .iter()
        .find(|(key, _)| key == "oldest")
        .expect("bounded history")
        .1
        .parse::<f64>()
        .unwrap();
    assert_eq!(oldest, (NOW - 600_000 - 7 * 86_400_000) as f64 / 1000.0);
}

#[test]
fn rework_two_minute_poll_gap_does_not_start_recovery_with_fresh_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.status = BridgeStatus::Ok;
    c.last_tick_ms = Some(NOW - 120_000);
    fs::create_dir_all(dir.path().join("channels")).unwrap();
    fs::write(dir.path().join("channels/C123.jsonl"), "").unwrap();
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW).unwrap().to_string(),
    )
    .unwrap();
    c.poll(NOW).unwrap();
    assert!(c.inbox.recovery_needed_through.is_empty());
    assert!(
        !c.io
            .methods
            .iter()
            .any(|method| method == "conversations.history")
    );
}

#[test]
fn rework_group_searches_rotate_one_page_per_interval() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.settings.channels.clear();
    c.inbox.user_groups = (0..50).map(|i| format!("S{i:02}")).collect();
    c.next_jobs.insert("groups".into(), NOW + 3_600_000);
    c.io.scripted.insert(
        "search.messages".into(),
        [
            Ok(json!({"messages":{"matches":[],"paging":{"pages":1}}})),
            Ok(json!({"messages":{"matches":[],"paging":{"pages":1}}})),
            Ok(json!({"messages":{"matches":[],"paging":{"pages":1}}})),
            Ok(json!({"messages":{"matches":[],"paging":{"pages":2}}})),
        ]
        .into(),
    );
    c.poll(NOW).unwrap();
    let queries: Vec<_> =
        c.io.calls
            .iter()
            .filter(|(method, _)| method == "search.messages")
            .filter_map(|(_, params)| {
                params
                    .iter()
                    .find(|(key, _)| key == "query")
                    .map(|(_, value)| value)
            })
            .collect();
    assert_eq!(
        queries.len(),
        4,
        "member, DM, pins, and one group regardless of group count"
    );
    assert!(queries[0].starts_with("<@U0KARL0001>"));
    assert!(queries[1].starts_with("to:U0KARL0001"));
    assert!(queries[3].starts_with("<!subteam^S00>"));
    assert_eq!(c.inbox.searches["group:S00"].page, 2);
    let count = c.io.calls.len();
    c.poll(NOW + 2_000).unwrap();
    assert_eq!(
        c.io.calls.len(),
        count,
        "unfinished group page still waits its interval"
    );
    // Membership changes must not discard another group's saved page.
    c.inbox.user_groups = BTreeSet::from(["S00".into(), "S01".into()]);
    c.poll(NOW + 120_000).unwrap();
    assert!(
        c.io.calls
            .last()
            .unwrap()
            .1
            .iter()
            .any(|(k, v)| k == "query" && v.starts_with("<!subteam^S01>"))
    );
    c.inbox.save(&c.path).unwrap();
    let mut restarted = channel(dir.path());
    restarted.inbox = Inbox::load(&c.path).unwrap();
    restarted.settings.channels.clear();
    restarted.next_jobs.insert("groups".into(), NOW + 3_600_000);
    restarted.poll(NOW + 240_000).unwrap();
    let params = &restarted.io.calls.last().unwrap().1;
    assert!(
        params
            .iter()
            .any(|(k, v)| k == "query" && v.starts_with("<!subteam^S00>"))
    );
    assert!(params.iter().any(|(k, v)| k == "page" && v == "2"));
}

#[test]
fn rework_group_search_skips_a_nearly_spent_poll_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.settings.channels.clear();
    c.inbox.user_groups.insert("SENG".into());
    c.next_jobs.insert("groups".into(), NOW + 3_600_000);
    c.poll_bounded(NOW, Duration::from_secs(10)).unwrap();
    assert_eq!(
        c.io.calls.len(),
        3,
        "member, DM, and pins run before optional groups"
    );
    assert!(c.last_group.is_none());
    c.poll(NOW + 2_000).unwrap();
    assert_eq!(c.io.calls.len(), 4);
    assert!(
        c.io.calls
            .last()
            .unwrap()
            .1
            .iter()
            .any(|(k, v)| k == "query" && v.starts_with("<!subteam^SENG>"))
    );
}

#[test]
fn rework_tombstoned_reply_is_not_resurrected_by_backfill() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    let root = "1789999999.000001";
    let id = "C123:1790000000.000001";
    c.inbox.delete(&[id.into()]);
    c.io.replies.push_back(
        json!({"messages":[{"ts":root,"latest_reply":"1790000000.000001","user":"UOTHER"}]}),
    );
    let mut batch = PollBatch::default();
    assert!(c.history("C123", NOW + 10, &mut batch).unwrap());
    c.io.scripted.insert(
        "conversations.replies".into(),
        [Ok(
            json!({"messages":[{"ts":"1790000000.000001","user":"UOTHER","text":"deleted"}]}),
        )]
        .into(),
    );
    assert!(!c.history("C123", NOW + 20, &mut batch).unwrap());
    assert!(c.inbox.get(id).is_none());
    assert!(!batch.events.iter().any(|e| e.id() == id));
    assert!(c.inbox.pending_replies.is_empty());
}

#[test]
fn rework_deferred_group_does_not_run_before_due_member_searches() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.settings.channels.clear();
    c.inbox.user_groups.insert("SENG".into());
    c.next_jobs.insert("groups".into(), NOW + 3_600_000);
    c.next_job = 3;
    c.poll(NOW).unwrap();
    assert_eq!(c.io.calls.len(), 3);
    assert!(c.last_group.is_none());
    c.poll(NOW + 2_000).unwrap();
    assert_eq!(c.io.calls.len(), 4);
    assert_eq!(c.last_group.as_deref(), Some("SENG"));
}

#[test]
fn rework_membership_refresh_can_remove_last_group_before_search() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.settings.channels.clear();
    c.inbox.user_groups.insert("SENG".into());
    c.next_job = 4; // Refresh membership before executing the already-built group job.
    c.poll(NOW).unwrap();
    assert!(c.inbox.user_groups.is_empty());
    assert!(c.last_group.is_none());
}

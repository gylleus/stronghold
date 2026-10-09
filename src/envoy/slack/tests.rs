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
}
impl SlackSender for Fake {
    fn send(&mut self, _: &OutboundMessage) -> Result<SendReceipt, String> {
        panic!("ingestion cannot send")
    }
}
impl SlackIo for Fake {
    fn pr_head(&mut self, repo: &str, number: u64) -> Result<Option<String>, String> {
        self.methods.push(format!("pr_head {repo}#{number}"));
        Ok((number != 404).then(|| format!("gylleus/{repo}-{number}")))
    }
    fn api(&mut self, method: &str, params: &[(&str, String)]) -> Result<Value, String> {
        self.methods.push(method.into());
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
    let second = json!({"ok":true,"messages":{"matches":[],"paging":{"pages":2}}});
    c.io.replies = [page,second,json!({"ok":true,"messages":{"matches":[],"paging":{"pages":1}}}),json!({"ok":true,"messages":[{"ts":"1790000000.000009","user":"U321","text":"History"}],"response_metadata":{"next_cursor":""}})].into();
    assert_eq!(c.poll(NOW + 10).unwrap().events.len(), 3);
    assert_eq!(
        c.io.methods,
        [
            "search.messages",
            "search.messages",
            "search.messages",
            "search.messages",
            "conversations.history"
        ]
    );
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
fn search_and_history_bound_catchup_and_report_skipped_older_messages() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = channel(dir.path());
    c.io.replies =
        std::iter::repeat_n(json!({"messages":{"matches":[],"paging":{"pages":100}}}), 5).collect();
    assert!(
        c.search(EventKind::Mention, NOW, &mut PollBatch::default())
            .unwrap_err()
            .contains("5 pages")
    );
    assert_eq!(c.io.methods.len(), 5);
    assert_eq!(
        c.inbox.poll_watermarks[&format!("mention:{}", c.settings.karl_user_id)],
        NOW
    );
    c.io.methods.clear();
    c.io.replies = std::iter::repeat_n(
        json!({"messages":[],"response_metadata":{"next_cursor":"next"}}),
        5,
    )
    .collect();
    assert!(
        c.history("C123", NOW, &mut PollBatch::default())
            .unwrap_err()
            .contains("5 pages")
    );
    assert_eq!(c.io.methods.len(), 5);
    assert_eq!(c.inbox.poll_watermarks["history:C123"], NOW);
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
    c.next_poll_ms = 0;
    let events = c.poll(NOW + 20).unwrap().events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::Pin);
    assert_eq!(events[0].id(), "C777:1600000000.000002");
    c.io.pins = [pinned("1600000000.000002")].into();
    c.next_poll_ms = 0;
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
    e.text = "realtime#694 and realtime#404, also #12 and VROOM-1".into();
    c.resolve_prs(std::slice::from_ref(&e));
    assert_eq!(c.inbox.pr_heads["realtime#694"], "gylleus/realtime-694");
    assert_eq!(
        c.inbox.pr_heads["realtime#404"], "",
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
    assert_eq!(c.settings.karl_user_id, "U0AMMLGSFL0");
    c.io.replies
        .push_back(json!({"ok":true,"user":"karl126","user_id":"u0ammlgsfl0","team":"Depict"}));
    assert_eq!(c.resolve_karl().unwrap().as_deref(), Some("U0AMMLGSFL0"));
    c.io.replies
        .push_back(json!({"ok":true,"user":"karl126","user_id":"W0NEWID","team":"Depict"}));
    assert_eq!(c.resolve_karl().unwrap().as_deref(), Some("W0NEWID"));
    assert_eq!(
        c.settings.karl_user_id, "W0NEWID",
        "the resolved id replaces the configured one"
    );
    c.io.replies.push_back(json!({"ok":true,"team":"Depict"}));
    assert_eq!(
        c.resolve_karl().unwrap(),
        None,
        "no id: the configured one stands"
    );
    assert_eq!(c.settings.karl_user_id, "W0NEWID");
    c.io.replies
        .push_back(json!({"ok":true,"user_id":"C0BHZLG3X63"}));
    assert!(
        c.resolve_karl().is_err(),
        "a conversation id is not a member id"
    );
    assert_eq!(c.settings.karl_user_id, "W0NEWID");
    assert_eq!(c.io.methods.iter().filter(|m| *m == "auth.test").count(), 4);
}

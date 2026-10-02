use super::*;
use serde_json::json;
use std::collections::VecDeque;
use std::io::Write;

#[derive(Default)]
struct Fake {
    replies: VecDeque<Value>,
    methods: Vec<String>,
    subscriptions: Vec<(PathBuf, String)>,
}
impl SlackSender for Fake {
    fn send(&mut self, _: &OutboundMessage) -> Result<(), String> {
        panic!("ingestion cannot send")
    }
}
impl SlackIo for Fake {
    fn api(&mut self, method: &str, _: &[(&str, String)]) -> Result<Value, String> {
        self.methods.push(method.into());
        Ok(self
            .replies
            .pop_front()
            .unwrap_or_else(|| json!({"ok":true,"messages":{"matches":[],"paging":{"pages":1}}})))
    }
    fn subscribe(&mut self, dir: &Path, channel: &str) -> Result<(), String> {
        self.subscriptions.push((dir.into(), channel.into()));
        Ok(())
    }
}
fn channel(dir: &Path) -> SlackChannel<Fake> {
    SlackChannel::new(
        Fake::default(),
        Inbox::default(),
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
fn quiet_ticks_do_not_rewrite_inbox_and_subscription_retry_clears_error() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("heartbeat"),
        jiff::Timestamp::from_millisecond(NOW).unwrap().to_string(),
    )
    .unwrap();
    let mut c = channel(dir.path());
    c.errors
        .insert("C123".into(), "Subscription command failed".into());
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

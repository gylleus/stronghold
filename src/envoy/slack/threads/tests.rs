use super::*;
use serde_json::json;

#[derive(Default)]
struct Fake {
    subscriptions: usize,
    broken: Option<String>,
    recipient: Option<String>,
    bot: bool,
    app: bool,
}
impl SlackSender for Fake {
    fn send(&mut self, _: &OutboundMessage) -> Result<SendReceipt, String> {
        panic!("ingestion must not send")
    }
}
impl SlackIo for Fake {
    fn api(&mut self, method: &str, params: &[(&str, String)]) -> Result<Value, String> {
        if method == "chat.getPermalink" {
            let channel = &params.iter().find(|(k, _)| *k == "channel").unwrap().1;
            return Ok(
                json!({"ok":true,"permalink":format!("https://example.slack.com/archives/{channel}/p2000001")}),
            );
        }
        if method == "conversations.info" {
            return Ok(
                json!({"ok":true,"channel":{"user":self.recipient.as_deref().unwrap_or("UKARL")}}),
            );
        }
        if method == "users.info" {
            return Ok(json!({"ok":true,"user":{"is_bot":self.bot,"is_app_user":self.app}}));
        }
        if method != "conversations.replies" {
            return Ok(json!({"ok":true,"messages":{"matches":[],"paging":{"pages":1}}}));
        }
        let channel = params
            .iter()
            .find(|(k, _)| *k == "channel")
            .unwrap()
            .1
            .clone();
        if self.broken.as_ref() == Some(&channel) {
            return Err("fixture denied".into());
        }
        Ok(
            json!({"ok":true,"messages":[{"ts":"2.000001","thread_ts":"1.000001","user":"UOTHER","text":"Fixture reply"}]}),
        )
    }
    fn subscribe(&mut self, _: &Path, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn thread_subscription(
        &mut self,
        bridge: &Path,
        _: &Thread,
        _: Option<&str>,
    ) -> Result<(), String> {
        assert!(bridge.join("fixture").exists());
        self.subscriptions += 1;
        Ok(())
    }
}
fn channel(dir: &Path) -> SlackChannel<Fake> {
    let bridge = dir.join("bridge");
    std::fs::create_dir_all(bridge.join("threads")).unwrap();
    std::fs::write(bridge.join("fixture"), "").unwrap();
    std::fs::write(bridge.join("heartbeat"), "1970-01-01T00:00:00Z").unwrap();
    SlackChannel::new(
        Fake::default(),
        Inbox::default(),
        dir.join("inbox.json"),
        bridge,
        EnvoySettings {
            channels: Vec::new(),
            dry_run: false,
            ..Default::default()
        },
    )
}
fn watch(id: &str, channel: &str) -> Watch {
    Watch {
        request_id: id.into(),
        thread: Thread {
            conversation: channel.into(),
            ts: "1.000001".into(),
        },
        after_us: 1_000_001,
        borrowed: false,
    }
}
fn reply(n: usize) -> InboundEvent {
    InboundEvent {
        channel_kind: "slack".into(),
        conversation: "D123".into(),
        ts: format!("{n}.000001"),
        thread_ts: Some("1.000001".into()),
        user: "UOTHER".into(),
        user_name: "Avery".into(),
        text: "fixture".into(),
        kind: EventKind::ThreadReply,
        permalink: None,
    }
}

#[test]
fn durable_handoff_ignores_display_floor_and_applies_backpressure() {
    let dir = tempfile::tempdir().unwrap();
    let mut channel = channel(dir.path());
    channel.watches(vec![watch("r", "D123")]).unwrap();
    assert!(channel.capture_reply(&reply(2)));
    for n in 10_000..12_100 {
        let mut unrelated = reply(n);
        unrelated.conversation = "COTHER".into();
        channel.inbox.insert(unrelated);
    }
    assert!(channel.inbox.get(&reply(2).id()).is_none());
    assert!(
        channel.capture_reply(&reply(3)),
        "late reply below global display floor survives"
    );
    channel.inbox.save(&channel.path).unwrap();
    let restored = Inbox::load(&channel.path).unwrap();
    assert_eq!(restored.pending_replies.len(), 2);
    channel.inbox = restored;
    assert!(channel.capture_reply(&reply(2)));
    assert_eq!(channel.inbox.pending_replies.len(), 2);
    for n in 4..MAX_PENDING + 2 {
        assert!(channel.capture_reply(&reply(n)));
    }
    assert_eq!(channel.inbox.pending_replies.len(), MAX_PENDING);
    assert!(!channel.capture_reply(&reply(MAX_PENDING + 2)));
    channel
        .acknowledge_replies(&[channel.inbox.pending_replies[0].key()])
        .unwrap();
    assert!(channel.capture_reply(&reply(MAX_PENDING + 2)));
}

#[test]
fn installation_catches_fast_reply_even_after_channel_cursor_and_display_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let mut channel = channel(dir.path());
    for n in 10_000..12_100 {
        channel.inbox.insert(reply(n));
    }
    channel.watches(vec![watch("r", "D123")]).unwrap();
    channel.poll(10).unwrap();
    assert_eq!(channel.inbox.pending_replies.len(), 1);
    assert_eq!(channel.inbox.pending_replies[0].event.ts, "2.000001");
    assert_eq!(
        channel.inbox.pending_replies[0].event.permalink.as_deref(),
        Some("https://example.slack.com/archives/D123/p2000001")
    );
    let restored = Inbox::load(&channel.path).unwrap();
    assert_eq!(restored.pending_replies, channel.inbox.pending_replies);
    channel.watches(vec![watch("r", "D123")]).unwrap();
    channel.poll(11).unwrap();
    assert_eq!(channel.inbox.pending_replies.len(), 1);
}

#[test]
fn routing_verifies_dm_recipient_and_refuses_karl() {
    let dir = tempfile::tempdir().unwrap();
    let mut channel = channel(dir.path());
    channel.settings.karl_user_id = "UKARL".into();
    assert!(
        channel
            .resolve_destination("D123")
            .unwrap_err()
            .contains("ask_karl")
    );
    assert_eq!(
        channel.resolve_destination("C123").unwrap().conversation,
        "C123"
    );
}

#[test]
fn broken_thread_does_not_starve_other_watches_and_feed_errors_repair() {
    let dir = tempfile::tempdir().unwrap();
    let mut channel = channel(dir.path());
    channel.io.broken = Some("D111".into());
    channel
        .watches(vec![watch("a", "D111"), watch("b", "D222")])
        .unwrap();
    let batch = channel.poll(10).unwrap();
    assert!(!batch.notices.is_empty());
    assert!(
        channel
            .inbox
            .pending_replies
            .iter()
            .any(|r| r.request_id == "b")
    );
    let before = channel.io.subscriptions;
    std::fs::write(
        channel.bridge.join("threads/D222-1.000001.jsonl"),
        "{\"type\":\"error\",\"reason\":\"membership\"}\n",
    )
    .unwrap();
    channel.poll(11).unwrap();
    assert!(!channel.inbox.thread_watches["b"].subscribed);
    channel.poll(120_100).unwrap();
    assert!(channel.io.subscriptions > before);
}

#[test]
fn adoption_parser_uses_cli_identities_not_paths_and_rejects_partial_garbage() {
    let output = "C123 (channel)  expires=never  file=/ignored\nD123 1.000001  label=\"Avery  test\"  created=?  expires=never  last_activity=-  file=/do/not/read\nD456 2.000001  expires=1970-01-01T00:00:00Z\n";
    let parsed = parse_subscriptions(output, 1).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].1, "Avery  test");
    assert!(parse_subscriptions(&format!("{output}malformed\n"), 1).is_err());
    assert!(
        parse_subscriptions("no active subscriptions\n", 0)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn borrowed_watch_reads_without_relabeling_or_resubscribing() {
    let dir = tempfile::tempdir().unwrap();
    let mut channel = channel(dir.path());
    let mut borrowed = watch("adopted", "D123");
    borrowed.borrowed = true;
    channel.watches(vec![borrowed.clone()]).unwrap();
    assert!(channel.inbox.thread_watches["adopted"].subscribed);
    std::fs::write(channel.bridge.join("threads/D123-1.000001.jsonl"),
        "{\"type\":\"reply\",\"channel\":\"D123\",\"ts\":\"2.000001\",\"thread_ts\":\"1.000001\",\"user\":\"UOTHER\",\"text\":\"Borrowed reply\"}\n{\"type\":\"error\",\"reason\":\"membership\"}\n").unwrap();
    channel.poll(10).unwrap();
    assert_eq!(channel.inbox.pending_replies.len(), 1);
    channel.watches(vec![borrowed]).unwrap();
    channel.poll(120_100).unwrap();
    assert_eq!(channel.io.subscriptions, 0);
    assert!(channel.inbox.thread_watches["adopted"].subscribed);
}

#[test]
fn dm_person_classification_rejects_bots_and_app_users() {
    let dir = tempfile::tempdir().unwrap();
    let mut channel = channel(dir.path());
    channel.io.recipient = Some("UPERSON".into());
    assert!(channel.resolve_destination("D123").unwrap().person);
    channel.io.bot = true;
    assert!(!channel.resolve_destination("D123").unwrap().person);
    channel.io.bot = false;
    channel.io.app = true;
    assert!(!channel.resolve_destination("D123").unwrap().person);
    assert!(!channel.resolve_destination("C123").unwrap().person);
}

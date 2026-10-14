use super::*;

fn who(handle: &str) -> Requester {
    Requester::from_origin(Origin {
        terminal_handle: Some(handle.into()),
        ..Default::default()
    })
    .unwrap()
}
fn ask(text: &str) -> ReachOut {
    ReachOut {
        to: "Avery".into(),
        ask: text.into(),
        why: "Review".into(),
        thread: None,
        request_id: None,
        urgency: "normal".into(),
        expect_reply: true,
    }
}

#[test]
fn limits_duplicates_ownership_and_followups_survive_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REQUESTS_FILE);
    let mut store = Requests::load(&path).unwrap();
    let first = store
        .submit(who("dwarf"), ask("first"), "UKARL", 0)
        .unwrap();
    assert!(
        store
            .submit(who("dwarf"), ask("first"), "UKARL", 1)
            .unwrap()
            .merged
    );
    for i in 1..6 {
        store
            .submit(who("dwarf"), ask(&format!("new {i}")), "UKARL", i)
            .unwrap();
    }
    assert!(
        store
            .submit(who("dwarf"), ask("seventh"), "UKARL", 100)
            .is_err()
    );
    for i in 0..30 {
        let mut args = ask(&format!("follow {i}"));
        args.request_id = Some(first.request_id.clone());
        assert!(
            store
                .submit(who("dwarf"), args, "UKARL", 100)
                .unwrap()
                .follow_up
        );
    }
    let mut args = ask("extra");
    args.request_id = Some(first.request_id.clone());
    assert!(store.submit(who("dwarf"), args, "UKARL", 100).is_err());
    let mut foreign = ask("steal");
    foreign.request_id = Some(first.request_id.clone());
    assert!(store.submit(who("other"), foreign, "UKARL", 100).is_err());
    let mut loaded = Requests::load(path).unwrap();
    assert!(
        loaded
            .get(&first.request_id)
            .unwrap()
            .next_turn(Some(2))
            .is_err()
    );
    assert!(
        loaded
            .submit(who("dwarf"), ask("seventh"), "UKARL", 100)
            .is_err()
    );
    assert!(
        loaded
            .submit(who("dwarf"), ask("after hour"), "UKARL", HOUR + 10)
            .is_ok()
    );
}

#[test]
fn karl_thread_validation_and_failed_save_do_not_charge() {
    for to in ["Karl", " kArL ", "me", "UKARL", "<@UKARL>"] {
        assert!(refuse_karl(to, "UKARL").unwrap_err().contains("ask_karl"));
    }
    assert!(Thread::from_link("https://evil.example/archives/C123/p1234567000001").is_err());
    assert_eq!(
        Thread::from_link("https://team.slack.com/archives/C123/p1234567000001")
            .unwrap()
            .ts,
        "1234567.000001"
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REQUESTS_FILE);
    let mut store = Requests::load(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        store
            .submit(who("dwarf"), ask("retry"), "UKARL", 0)
            .is_err()
    );
    assert!(store.all().is_empty());
    std::fs::remove_dir(path).unwrap();
    assert_eq!(
        store
            .submit(who("dwarf"), ask("retry"), "UKARL", 0)
            .unwrap()
            .request_id,
        "envoy-request-1"
    );
}

#[test]
fn thread_followup_and_adoption_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Requests::load(dir.path().join(REQUESTS_FILE)).unwrap();
    let mut first = ask("thread");
    first.thread = Some("https://team.slack.com/archives/C123/p1234567000001".into());
    let id = store
        .submit(who("dwarf"), first, "UKARL", 0)
        .unwrap()
        .request_id;
    let mut next = ask("more");
    next.thread = Some("https://team.slack.com/archives/C123/p1234567000001".into());
    assert_eq!(
        store
            .submit(who("dwarf"), next, "UKARL", 0)
            .unwrap()
            .request_id,
        id
    );
    let thread = Thread {
        conversation: "D123".into(),
        ts: "1.000001".into(),
    };
    store.adopt(thread.clone(), "orphan".into(), 0).unwrap();
    store.adopt(thread, "again".into(), 1).unwrap();
    assert_eq!(store.all().len(), 2);
    assert!(store.all()[1].requester.is_none());
}

#[test]
fn karl_aliases_and_labeled_mentions_are_refused() {
    for target in [
        " Karl   GYLLEUS ",
        " @KaRl ",
        "<@u000karl001|Karl Gylleus>",
        " <@ U000KARL001 | Karl > ",
    ] {
        assert!(
            refuse_karl(target, "U000KARL001")
                .unwrap_err()
                .contains("ask_karl")
        );
    }
}

#[test]
fn declined_exact_retry_in_open_request_is_a_new_routable_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Requests::load(dir.path().join(REQUESTS_FILE)).unwrap();
    let id = store
        .submit(who("dwarf"), ask("retry"), "UKARL", 0)
        .unwrap()
        .request_id;
    store
        .change(&id, |r| {
            r.turns[0].status = Status::Declined;
            r.status = Status::Declined;
            r.settled = false;
            Ok(())
        })
        .unwrap();
    let mut args = ask("retry");
    args.request_id = Some(id.clone());
    let retry = store.submit(who("dwarf"), args, "UKARL", 1).unwrap();
    assert!(!retry.merged);
    assert_eq!(retry.turn_id, 2);
    assert!(store.get(&id).unwrap().next_turn(Some(2)).is_ok());
}

#[test]
fn relayed_reply_is_bounded_but_status_retains_full_unicode_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Requests::load(dir.path().join(REQUESTS_FILE)).unwrap();
    store
        .adopt(
            Thread {
                conversation: "D123".into(),
                ts: "1.000001".into(),
            },
            "Borrowed".into(),
            0,
        )
        .unwrap();
    let id = store.all()[0].id.clone();
    let event = InboundEvent {
        channel_kind: "slack".into(),
        conversation: "D123".into(),
        ts: "2.000001".into(),
        thread_ts: Some("1.000001".into()),
        user: "UOTHER".into(),
        user_name: "Avery".into(),
        text: "🦀".repeat(2100),
        kind: super::super::channel::EventKind::ThreadReply,
        permalink: Some("https://example.slack.com/archives/D123/p2000001".into()),
    };
    store.reply(&id, &event).unwrap();
    let delivery = &store.get(&id).unwrap().deliveries[0];
    assert_eq!(delivery.text.matches('🦀').count(), 2000);
    assert!(
        delivery
            .text
            .contains("(truncated; full text in reach_out_status)")
    );
    assert_eq!(delivery.event.as_ref().unwrap().text, event.text);
}

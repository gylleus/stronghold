use super::*;
use crate::model::{Node, NodeKind};

fn post(target: &str, source: Source) -> Post {
    Post {
        source,
        severity: Severity::Warn,
        title: format!("Myrstack: {target} needs input"),
        body: "body".into(),
        target: Some(target.into()),
        id: None,
        native: true,
        sound: None,
        volume: 0.5,
    }
}

fn input(target: &str) -> Post {
    post(target, Source::NeedsInput)
}

#[test]
fn single_post_passes_through() {
    let mut rl = PostLimiter::new();
    rl.offer(input("t1"));
    let out = rl.flush(0).unwrap();
    assert_eq!(out.title, "Myrstack: t1 needs input");
    assert_eq!(out.body, "body");
    assert_eq!(out.targets, vec!["t1"]);
    assert!(out.native);
    assert_eq!(rl.flush(0), None);
}

#[test]
fn burst_collapses_into_summary() {
    let mut rl = PostLimiter::new();
    for t in ["a", "b", "c"] {
        rl.offer(input(t));
    }
    let out = rl.flush(0).unwrap();
    assert_eq!(out.title, "Myrstack: 3 agents need input");
    assert_eq!(out.body, "a needs input\nb needs input\nc needs input");
    assert_eq!(out.targets, vec!["a", "b", "c"]);
}

#[test]
fn mixed_burst_needs_attention() {
    let out = collapse(vec![input("a"), post("b", Source::Escalation)]);
    assert_eq!(out.title, "Myrstack: 2 agents need attention");
    let out = collapse(vec![post("a", Source::Agent), post("b", Source::Agent)]);
    assert_eq!(out.title, "Myrstack: 2 notifications from agents");
}

#[test]
fn a_burst_plays_the_most_severe_sound_once() {
    let mut quiet_one = input("a");
    quiet_one.sound = Some("/s/tink.aiff".into());
    quiet_one.severity = Severity::Info;
    let mut loud = post("b", Source::Agent);
    loud.severity = Severity::Urgent;
    loud.sound = Some("/s/sosumi.aiff".into());
    loud.volume = 0.9;
    let mut sound_only = input("c");
    sound_only.native = false;
    sound_only.id = Some(7);
    let out = collapse(vec![quiet_one, loud, sound_only]);
    assert_eq!(out.sound, Some(("/s/sosumi.aiff".into(), 0.9)));
    assert_eq!(out.ids, vec![7]);
    assert_eq!(out.targets, vec!["a", "b", "c"]);
    assert_eq!(
        out.title, "Myrstack: 2 agents need attention",
        "only shown posts are listed"
    );
}

#[test]
fn a_sound_only_burst_is_not_shown() {
    let mut p = input("a");
    p.native = false;
    p.sound = Some("/s/glass.aiff".into());
    let out = collapse(vec![p]);
    assert!(!out.native);
    assert!(out.sound.is_some());
}

#[test]
fn at_most_five_per_minute_then_summary() {
    let mut rl = PostLimiter::new();
    for i in 0..5 {
        rl.offer(input(&format!("t{i}")));
        assert!(rl.flush(i * 1000).is_some());
    }
    rl.offer(input("x"));
    rl.offer(input("y"));
    assert_eq!(rl.flush(20_000), None);
    assert!(rl.has_pending());
    // The first post leaves the window at 60 s.
    assert_eq!(rl.flush(59_999), None);
    let out = rl.flush(60_000).unwrap();
    assert_eq!(out.title, "Myrstack: 2 agents need input");
    assert!(!rl.has_pending());
    assert_eq!(rl.flush(60_000), None);
}

#[test]
fn posts_follow_the_delivery() {
    let settings = NotifySettings::default();
    let mut n = Incoming::new(Source::Agent, Severity::Warn, "Blocked", "on the schema");
    n.agent_label = Some("fix-login".into());
    n.target = Some("term_a".into());
    let d = settings.delivery(Severity::Warn);
    let p = Post::new(&n, 3, &d, &settings).unwrap();
    assert_eq!(p.title, "fix-login: Blocked");
    assert_eq!(p.body, "on the schema");
    assert_eq!(p.id, Some(3));
    assert_eq!(
        p.sound.as_deref(),
        Some(Path::new("/System/Library/Sounds/Glass.aiff"))
    );
    assert!((p.volume - 0.7).abs() < 1e-6);
    let silent = Delivery {
        in_app: true,
        ..Delivery::default()
    };
    assert_eq!(Post::new(&n, 3, &silent, &settings), None, "in-app only");
}

#[test]
fn whip_titles_parse_to_handles() {
    let r = whip_request("Myrstack: term_7 is waiting on you", "waiting 12m");
    assert_eq!(r.target.as_deref(), Some("term_7"));
    assert_eq!(r.source, Source::NeedsInput);
    assert_eq!(r.severity, Severity::Warn);
    assert_eq!(r.message, "waiting 12m");
    let r = whip_request("Myrstack: worker term_w task t1 is stalled", "e");
    assert_eq!(
        (r.target.as_deref(), r.source),
        (Some("term_w"), Source::Escalation)
    );
    let r = whip_request("Myrstack: coordinator term_c run run_1 is stalled", "e");
    assert_eq!(r.target.as_deref(), Some("term_c"));
    let r = whip_request("Something else", "e");
    assert_eq!(r.target.as_deref(), Some("Something else"));
}

fn node(id: &str, handle: Option<&str>, live: bool, state: NodeState) -> Node {
    Node {
        id: NodeId(id.into()),
        kind: NodeKind::Terminal,
        label: format!("label {id}"),
        terminal_handle: handle.map(str::to_string),
        live,
        run_id: None,
        task_id: None,
        dispatch_id: None,
        agent: None,
        worktree_path: None,
        branch: None,
        last_output_ms: None,
        preview: None,
        state,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
        parked_at_ms: None,
    }
}

fn snapshot(nodes: Vec<Node>) -> ClanSnapshot {
    let mut snapshot = ClanSnapshot::default();
    snapshot.nodes = nodes;
    snapshot
}

#[test]
fn detector_ignores_the_king() {
    let mut d = NeedsInputDetector::new();
    let mut calm = snapshot(vec![node("a", Some("h"), true, NodeState::Working)]);
    calm.add_king();
    assert!(d.update(&calm).is_empty());
    let mut waiting = snapshot(vec![node("a", Some("h"), true, NodeState::NeedsInput)]);
    waiting.add_king();
    assert_eq!(waiting.king().unwrap().state, NodeState::NeedsInput);
    let requests = d.update(&waiting);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target.as_deref(), Some("h"));
}

#[test]
fn detector_reports_only_transitions() {
    let mut d = NeedsInputDetector::new();
    let waiting = |id: &str, h| node(id, Some(h), true, NodeState::NeedsInput);
    let working = |id: &str, h| node(id, Some(h), true, NodeState::Working);
    let targets = |reqs: &[Incoming]| {
        reqs.iter()
            .map(|r| r.target.clone().unwrap())
            .collect::<Vec<_>>()
    };
    // Already waiting at startup: no notification.
    assert!(
        d.update(&snapshot(vec![
            waiting("term:a", "a"),
            working("term:b", "b")
        ]))
        .is_empty()
    );
    let reqs = d.update(&snapshot(vec![
        waiting("term:a", "a"),
        waiting("term:b", "b"),
    ]));
    assert_eq!(targets(&reqs), ["b"]);
    assert_eq!(reqs[0].title, "Myrstack: label term:b needs input");
    assert_eq!(reqs[0].source, Source::NeedsInput);
    assert_eq!(reqs[0].agent_label.as_deref(), Some("label term:b"));
    // Staying in NeedsInput does not repeat; leaving and re-entering does.
    assert!(
        d.update(&snapshot(vec![
            working("term:a", "a"),
            waiting("term:b", "b")
        ]))
        .is_empty()
    );
    let reqs = d.update(&snapshot(vec![
        waiting("term:a", "a"),
        waiting("term:b", "b"),
    ]));
    assert_eq!(targets(&reqs), ["a"]);
}

#[test]
fn detector_keys_handleless_nodes_by_id() {
    let mut d = NeedsInputDetector::new();
    d.update(&snapshot(vec![]));
    let reqs = d.update(&snapshot(vec![node(
        "sub:s:a",
        None,
        true,
        NodeState::NeedsInput,
    )]));
    assert_eq!(reqs[0].target.as_deref(), Some("sub:s:a"));
}

#[test]
fn click_routing() {
    let snap = snapshot(vec![
        node("dispatch:r:old", Some("term_w"), false, NodeState::Done),
        node("dispatch:r:new", Some("term_w"), true, NodeState::Working),
        node("dispatch:r:gone", Some("term_x"), false, NodeState::Done),
        node("sub:s:a", None, true, NodeState::NeedsInput),
    ]);
    let resolve = |t| resolve_target(&snap, t).map(|id| id.0);
    assert_eq!(resolve("term_w").as_deref(), Some("dispatch:r:new"));
    assert_eq!(resolve("term_x").as_deref(), Some("dispatch:r:gone"));
    assert_eq!(resolve("sub:s:a").as_deref(), Some("sub:s:a"));
    assert_eq!(resolve("nope"), None);
}

#[test]
fn payload_round_trips() {
    let targets = vec!["term_7".to_string(), "sub:s:\"odd\" id".to_string()];
    let payload = encode_payload(&targets, &[4, 9]);
    assert_eq!(
        decode_payload(&payload),
        Some(Clicked {
            targets,
            ids: vec![4, 9]
        })
    );
    assert_eq!(
        decode_payload(&encode_payload(&[], &[])),
        Some(Clicked::default())
    );
    // Notifications posted by older builds carry no ids.
    assert_eq!(
        decode_payload(r#"{"targets":["a"]}"#),
        Some(Clicked {
            targets: vec!["a".into()],
            ids: vec![]
        })
    );
}

#[test]
fn foreign_payloads_are_rejected() {
    assert_eq!(decode_payload("not json"), None);
    assert_eq!(decode_payload("{}"), None);
    assert_eq!(decode_payload(r#"{"targets":"term_7"}"#), None);
    assert_eq!(decode_payload(r#"{"targets":[1]}"#), None);
    assert_eq!(decode_payload(r#"{"targets":[],"ids":["x"]}"#), None);
}

#[test]
fn click_target_takes_first_resolvable() {
    let snap = snapshot(vec![
        node("term:a", Some("a"), true, NodeState::NeedsInput),
        node("term:b", Some("b"), true, NodeState::NeedsInput),
    ]);
    let targets = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let pick = |t: &[&str]| click_target(&snap, &targets(t)).map(|id| id.0);
    assert_eq!(pick(&["gone", "b", "a"]).as_deref(), Some("term:b"));
    assert_eq!(pick(&["gone"]), None);
    assert_eq!(pick(&[]), None);
}

#[test]
fn notify_cli_parses_only_its_command() {
    assert!(run_cli(&["mcp".to_string()]).is_none());
    assert!(run_cli(&[]).is_none());
    let err = run_cli(&["notify".to_string()]).unwrap().unwrap_err();
    assert!(err.to_string().contains("usage"));
    let err = run_cli(&["notify".into(), "t".into(), "--target".into()])
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("--target"));
}

/// Records each post's title and payload.
struct Recorder(Arc<Mutex<Vec<(String, String)>>>);

impl Poster for Recorder {
    fn post(&self, notification: &Outgoing) -> anyhow::Result<()> {
        self.0.lock().unwrap().push((
            notification.title.clone(),
            encode_payload(&notification.targets, &notification.ids),
        ));
        Ok(())
    }
}

/// Records each sound played.
struct Speaker(Arc<Mutex<Vec<(PathBuf, f32)>>>);

impl Player for Speaker {
    fn play(&self, path: &Path, volume: f32) -> anyhow::Result<()> {
        self.0.lock().unwrap().push((path.to_owned(), volume));
        Ok(())
    }
}

struct Failing;

impl Poster for Failing {
    fn post(&self, _: &Outgoing) -> anyhow::Result<()> {
        anyhow::bail!("notifications are turned off")
    }
}

impl Player for Failing {
    fn play(&self, _: &Path, _: f32) -> anyhow::Result<()> {
        anyhow::bail!("no afplay")
    }
}

fn no_wake() -> Arc<Mutex<Option<Repaint>>> {
    Arc::new(Mutex::new(None))
}

fn wait_for<T>(mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = poll() {
            return v;
        }
        assert!(std::time::Instant::now() < deadline, "timed out");
        thread::sleep(Duration::from_millis(50));
    }
}

/// A posted notification's payload, clicked, comes back from `clicked` and selects the
/// node it was about.
#[test]
fn clicked_payload_routes_to_its_node() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let (click_tx, clicks) = crossbeam_channel::unbounded();
    let notifier = Notifier::spawn_with(
        Box::new(Recorder(posted.clone())),
        Box::new(Speaker(Arc::default())),
        clicks,
        no_wake(),
    );
    let mut p = input("term_w");
    p.id = Some(12);
    notifier.post(p);
    let payload = wait_for(|| posted.lock().unwrap().first().map(|p| p.1.clone()));
    // What the notification center hands back on a click: the payload the poster attached.
    click_tx.send(decode_payload(&payload).unwrap()).unwrap();
    let clicked = notifier.clicked();
    assert_eq!(
        clicked,
        vec![Clicked {
            targets: vec!["term_w".into()],
            ids: vec![12]
        }]
    );
    let snap = snapshot(vec![
        node("dispatch:r:old", Some("term_w"), false, NodeState::Done),
        node(
            "dispatch:r:new",
            Some("term_w"),
            true,
            NodeState::NeedsInput,
        ),
    ]);
    assert_eq!(
        click_target(&snap, &clicked[0].targets)
            .map(|id| id.0)
            .as_deref(),
        Some("dispatch:r:new")
    );
    assert!(notifier.clicked().is_empty());
}

#[test]
fn failures_are_reported() {
    let notifier = Notifier::spawn_with(
        Box::new(Failing),
        Box::new(Failing),
        crossbeam_channel::never(),
        no_wake(),
    );
    let mut p = input("a");
    p.sound = Some("/s/glass.aiff".into());
    notifier.post(p);
    let failures = wait_for(|| {
        let f = notifier.failures();
        (!f.is_empty()).then_some(f)
    });
    assert_eq!(
        failures,
        vec!["sound: no afplay", "notifications are turned off"]
    );
}

#[test]
fn notifier_thread_collapses_posts_and_plays_once() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let played = Arc::new(Mutex::new(Vec::new()));
    let notifier = Notifier::spawn_with(
        Box::new(Recorder(posted.clone())),
        Box::new(Speaker(played.clone())),
        crossbeam_channel::never(),
        no_wake(),
    );
    let mut a = input("a");
    a.sound = Some("/s/glass.aiff".into());
    let mut b = input("b");
    b.sound = Some("/s/glass.aiff".into());
    notifier.post(a);
    notifier.post(b);
    let titles = wait_for(|| {
        let p = posted.lock().unwrap();
        (!p.is_empty()).then(|| p.iter().map(|p| p.0.clone()).collect::<Vec<_>>())
    });
    assert_eq!(titles, vec!["Myrstack: 2 agents need input"]);
    assert_eq!(
        *played.lock().unwrap(),
        vec![(PathBuf::from("/s/glass.aiff"), 0.5)]
    );
}

#[test]
fn raised_notifications_wait_for_the_app_and_wake_it() {
    let woken = Arc::new(Mutex::new(0));
    let counter = woken.clone();
    let wake: Repaint = Box::new(move || *counter.lock().unwrap() += 1);
    let notifier = Notifier::spawn_with(
        Box::new(Failing),
        Box::new(Failing),
        crossbeam_channel::never(),
        Arc::new(Mutex::new(Some(wake))),
    );
    notifier.notify(whip_request("Myrstack: term_7 is waiting on you", "12m"));
    assert_eq!(*woken.lock().unwrap(), 1);
    let raised = notifier.incoming();
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].target.as_deref(), Some("term_7"));
    assert!(notifier.incoming().is_empty());
}

#[test]
fn missing_sound_files_fail() {
    let err = play_sound(Path::new("/nonexistent/x.aiff"), 0.5).unwrap_err();
    assert!(err.to_string().contains("no sound file"));
}

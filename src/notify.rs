//! macOS notifications for escalations and terminals waiting on input.
//!
//! A [`Notifier`] owns a thread that rate limits requests ([`RateLimiter`]), collapses bursts
//! into one summary and posts through a [`Poster`]. Posting uses `osascript`'s `display
//! notification`: it works for an unbundled binary, but the notification shows under Script
//! Editor and clicking it cannot reach Myrstack. So every posted notification is also reported
//! back to the app as a [`Delivered`] event, which the app lists as in-app alerts that select
//! the node when clicked ([`resolve_target`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::model::{FleetSnapshot, NodeId, NodeKind, NodeState};

/// At most one notification per target in this window.
pub const PER_TARGET_COOLDOWN_MS: i64 = 10 * 60_000;
/// Window of the global limit.
pub const GLOBAL_WINDOW_MS: i64 = 60_000;
/// At most this many notifications per [`GLOBAL_WINDOW_MS`].
pub const GLOBAL_MAX: usize = 5;
/// Requests arriving this close together are collapsed into one notification.
const BURST: Duration = Duration::from_millis(1500);
/// How often queued requests are retried while the global limit holds them back.
const RETRY: Duration = Duration::from_secs(5);

/// Why a notification is sent; decides the summary wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyKind {
    NeedsInput,
    Escalation,
}

/// One notification someone asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyRequest {
    /// What the notification is about: a terminal handle, or a node id for nodes without one.
    /// The per-target limit and click routing key on it.
    pub target: String,
    pub kind: NotifyKind,
    pub title: String,
    pub body: String,
}

/// A notification to post, possibly a summary of several requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub title: String,
    pub body: String,
    /// Targets it covers, in request order.
    pub targets: Vec<String>,
}

/// A notification that was posted, reported back to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    pub at_ms: i64,
    pub title: String,
    pub targets: Vec<String>,
}

// ---------------------------------------------------------------------------
// Rate limiting

/// Pure rate limiter: per-target cooldown, global cap, and burst collapsing.
///
/// [`offer`](Self::offer) accepts or drops a request; accepted ones wait until
/// [`flush`](Self::flush), which turns everything waiting into one [`Outgoing`] when the global
/// cap allows.
#[derive(Debug, Default)]
pub struct RateLimiter {
    /// When each target was last accepted.
    last_by_target: HashMap<String, i64>,
    /// Post times inside the global window.
    sent: VecDeque<i64>,
    pending: Vec<NotifyRequest>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues `request` unless its target was notified within the cooldown or is already queued.
    /// Returns whether it was accepted.
    pub fn offer(&mut self, request: NotifyRequest, now_ms: i64) -> bool {
        if let Some(&last) = self.last_by_target.get(&request.target)
            && now_ms - last < PER_TARGET_COOLDOWN_MS
        {
            return false;
        }
        self.last_by_target.insert(request.target.clone(), now_ms);
        self.pending.push(request);
        true
    }

    /// Whether requests are waiting for the global cap.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Everything waiting as one notification, if the global cap allows one now.
    pub fn flush(&mut self, now_ms: i64) -> Option<Outgoing> {
        while self
            .sent
            .front()
            .is_some_and(|&t| now_ms - t >= GLOBAL_WINDOW_MS)
        {
            self.sent.pop_front();
        }
        if self.pending.is_empty() || self.sent.len() >= GLOBAL_MAX {
            return None;
        }
        self.sent.push_back(now_ms);
        Some(collapse(std::mem::take(&mut self.pending)))
    }
}

/// One request as itself; several as a summary such as `3 agents need input`.
pub fn collapse(requests: Vec<NotifyRequest>) -> Outgoing {
    let targets = requests.iter().map(|r| r.target.clone()).collect();
    if let [one] = &requests[..] {
        return Outgoing {
            title: one.title.clone(),
            body: one.body.clone(),
            targets,
        };
    }
    let all_input = requests.iter().all(|r| r.kind == NotifyKind::NeedsInput);
    let what = if all_input {
        "need input"
    } else {
        "need attention"
    };
    let body = requests
        .iter()
        .map(|r| r.title.strip_prefix(TITLE_PREFIX).unwrap_or(&r.title))
        .collect::<Vec<_>>()
        .join("\n");
    Outgoing {
        title: format!("{TITLE_PREFIX}{} agents {what}", requests.len()),
        body,
        targets,
    }
}

const TITLE_PREFIX: &str = "Myrstack: ";

// ---------------------------------------------------------------------------
// Request sources

/// A request for a whip notification, from the engine's title and body.
///
/// The engine titles are `Myrstack: <handle> is waiting on you` and
/// `Myrstack: worker <handle> task <id> is stalled` / `Myrstack: coordinator <handle> run <id>
/// is stalled`; the target is the handle. Unknown titles use the whole title as the target.
pub fn whip_request(title: &str, body: &str) -> NotifyRequest {
    let subject = title.strip_prefix(TITLE_PREFIX).unwrap_or(title);
    let (kind, target) = if let Some(handle) = subject.strip_suffix(" is waiting on you") {
        (NotifyKind::NeedsInput, handle.to_string())
    } else if let Some(label) = subject.strip_suffix(" is stalled") {
        let mut words = label.split_whitespace();
        let handle = match words.next() {
            Some("worker" | "coordinator") => words.next(),
            _ => None,
        };
        (
            NotifyKind::Escalation,
            handle.map_or_else(|| title.to_string(), str::to_string),
        )
    } else {
        (NotifyKind::Escalation, title.to_string())
    };
    NotifyRequest {
        target,
        kind,
        title: title.to_string(),
        body: body.to_string(),
    }
}

/// The key a node is notified under: its terminal handle, else its node id.
pub fn target_key(node_id: &NodeId, handle: Option<&str>) -> String {
    handle.map_or_else(|| node_id.to_string(), str::to_string)
}

/// Detects nodes entering NeedsInput between consecutive snapshots.
#[derive(Debug, Default)]
pub struct NeedsInputDetector {
    /// Nodes in NeedsInput in the previous snapshot; `None` before the first one, so nodes
    /// already waiting at startup do not notify.
    previous: Option<HashSet<NodeId>>,
}

impl NeedsInputDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests for the nodes of `snapshot` that are NeedsInput now but were not before.
    pub fn update(&mut self, snapshot: &FleetSnapshot) -> Vec<NotifyRequest> {
        let waiting: HashSet<NodeId> = snapshot
            .nodes
            .iter()
            .filter(|n| n.state == NodeState::NeedsInput && n.kind != NodeKind::Queen)
            .map(|n| n.id.clone())
            .collect();
        let requests = match &self.previous {
            None => Vec::new(),
            Some(previous) => snapshot
                .nodes
                .iter()
                .filter(|n| waiting.contains(&n.id) && !previous.contains(&n.id))
                .map(|n| NotifyRequest {
                    target: target_key(&n.id, n.terminal_handle.as_deref()),
                    kind: NotifyKind::NeedsInput,
                    title: format!("{TITLE_PREFIX}{} needs input", n.label),
                    body: n.preview.clone().unwrap_or_else(|| "Waiting on you".into()),
                })
                .collect(),
        };
        self.previous = Some(waiting);
        requests
    }
}

// ---------------------------------------------------------------------------
// Click routing

/// The node to select for a notification target: the node with that id, else a node on that
/// terminal handle, preferring a live one.
pub fn resolve_target(snapshot: &FleetSnapshot, target: &str) -> Option<NodeId> {
    if let Some(node) = snapshot.nodes.iter().find(|n| n.id.as_str() == target) {
        return Some(node.id.clone());
    }
    let mut on_handle = snapshot
        .nodes
        .iter()
        .filter(|n| n.terminal_handle.as_deref() == Some(target));
    let first = on_handle.next()?;
    if first.live {
        return Some(first.id.clone());
    }
    Some(
        on_handle
            .find(|n| n.live)
            .map_or_else(|| first.id.clone(), |n| n.id.clone()),
    )
}

// ---------------------------------------------------------------------------
// Posting

/// Shows a notification on screen.
pub trait Poster: Send {
    fn post(&self, title: &str, body: &str) -> anyhow::Result<()>;
}

/// Posts with `osascript -e 'display notification …'`. Title and body go in as arguments, so
/// they need no AppleScript escaping.
#[derive(Debug, Default)]
pub struct OsascriptPoster;

impl Poster for OsascriptPoster {
    fn post(&self, title: &str, body: &str) -> anyhow::Result<()> {
        let output = Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "display notification (item 2 of argv) with title (item 1 of argv)",
                "-e",
                "end run",
                title,
                body,
            ])
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "osascript failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }
}

/// Called after a notification is posted, to wake the UI.
pub type Repaint = Box<dyn Fn() + Send>;

/// Rate-limited notifications on a background thread. Share it with `Arc`.
#[derive(Debug)]
pub struct Notifier {
    requests: Sender<NotifyRequest>,
    delivered: Receiver<Delivered>,
}

impl Notifier {
    /// Starts the notifier thread with the `osascript` poster.
    pub fn spawn(repaint: Option<Repaint>) -> Self {
        Self::spawn_with(Box::new(OsascriptPoster), repaint)
    }

    pub fn spawn_with(poster: Box<dyn Poster>, repaint: Option<Repaint>) -> Self {
        let (requests, request_rx) = crossbeam_channel::unbounded();
        let (delivered_tx, delivered) = crossbeam_channel::unbounded();
        thread::Builder::new()
            .name("notifier".into())
            .spawn(move || run(&request_rx, &delivered_tx, poster.as_ref(), repaint))
            .expect("spawning notifier thread");
        Self {
            requests,
            delivered,
        }
    }

    /// Asks for a notification; it may be delayed, merged into a summary or dropped.
    pub fn notify(&self, request: NotifyRequest) {
        let _ = self.requests.send(request);
    }

    /// Notifications posted since the last call.
    pub fn delivered(&self) -> Vec<Delivered> {
        self.delivered.try_iter().collect()
    }
}

fn run(
    requests: &Receiver<NotifyRequest>,
    delivered: &Sender<Delivered>,
    poster: &dyn Poster,
    repaint: Option<Repaint>,
) {
    let mut limiter = RateLimiter::new();
    loop {
        let wait = if limiter.has_pending() {
            RETRY
        } else {
            Duration::from_secs(3600)
        };
        match requests.recv_timeout(wait) {
            Ok(first) => {
                limiter.offer(first, now_ms());
                // Gather the rest of the burst.
                while let Ok(next) = requests.recv_timeout(BURST) {
                    limiter.offer(next, now_ms());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let now = now_ms();
        let Some(out) = limiter.flush(now) else {
            continue;
        };
        if let Err(err) = poster.post(&out.title, &out.body) {
            log::warn!("notification failed: {err:#}");
            continue;
        }
        log::info!("notified: {}", out.title);
        let _ = delivered.send(Delivered {
            at_ms: now,
            title: out.title,
            targets: out.targets,
        });
        if let Some(repaint) = &repaint {
            repaint();
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, NodeKind};
    use std::sync::{Arc, Mutex};

    const MIN: i64 = 60_000;

    fn req(target: &str, kind: NotifyKind) -> NotifyRequest {
        NotifyRequest {
            target: target.into(),
            kind,
            title: format!("Myrstack: {target} needs input"),
            body: "body".into(),
        }
    }

    fn input(target: &str) -> NotifyRequest {
        req(target, NotifyKind::NeedsInput)
    }

    #[test]
    fn single_request_passes_through() {
        let mut rl = RateLimiter::new();
        assert!(rl.offer(input("t1"), 0));
        let out = rl.flush(0).unwrap();
        assert_eq!(out.title, "Myrstack: t1 needs input");
        assert_eq!(out.body, "body");
        assert_eq!(out.targets, vec!["t1"]);
        assert_eq!(rl.flush(0), None);
    }

    #[test]
    fn one_per_target_per_ten_minutes() {
        let mut rl = RateLimiter::new();
        assert!(rl.offer(input("t1"), 0));
        rl.flush(0).unwrap();
        assert!(!rl.offer(input("t1"), 9 * MIN));
        assert_eq!(rl.flush(9 * MIN), None);
        assert!(rl.offer(input("t2"), 9 * MIN));
        assert!(rl.offer(input("t1"), 10 * MIN));
    }

    #[test]
    fn duplicate_while_pending_is_dropped() {
        let mut rl = RateLimiter::new();
        assert!(rl.offer(input("t1"), 0));
        assert!(!rl.offer(input("t1"), 1000));
        assert_eq!(rl.flush(1000).unwrap().targets, vec!["t1"]);
    }

    #[test]
    fn burst_collapses_into_summary() {
        let mut rl = RateLimiter::new();
        for t in ["a", "b", "c"] {
            rl.offer(input(t), 0);
        }
        let out = rl.flush(0).unwrap();
        assert_eq!(out.title, "Myrstack: 3 agents need input");
        assert_eq!(out.body, "a needs input\nb needs input\nc needs input");
        assert_eq!(out.targets, vec!["a", "b", "c"]);
    }

    #[test]
    fn mixed_burst_needs_attention() {
        let out = collapse(vec![input("a"), req("b", NotifyKind::Escalation)]);
        assert_eq!(out.title, "Myrstack: 2 agents need attention");
    }

    #[test]
    fn at_most_five_per_minute_then_summary() {
        let mut rl = RateLimiter::new();
        for i in 0..5 {
            rl.offer(input(&format!("t{i}")), i * 1000);
            assert!(rl.flush(i * 1000).is_some());
        }
        rl.offer(input("x"), 10_000);
        rl.offer(input("y"), 20_000);
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
    fn whip_titles_parse_to_handles() {
        let r = whip_request("Myrstack: term_7 is waiting on you", "waiting 12m");
        assert_eq!(r.target, "term_7");
        assert_eq!(r.kind, NotifyKind::NeedsInput);
        assert_eq!(r.body, "waiting 12m");
        let r = whip_request("Myrstack: worker term_w task t1 is stalled", "e");
        assert_eq!(
            (r.target.as_str(), r.kind),
            ("term_w", NotifyKind::Escalation)
        );
        let r = whip_request("Myrstack: coordinator term_c run run_1 is stalled", "e");
        assert_eq!(r.target, "term_c");
        let r = whip_request("Something else", "e");
        assert_eq!(r.target, "Something else");
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
        }
    }

    fn snapshot(nodes: Vec<Node>) -> FleetSnapshot {
        let mut snapshot = FleetSnapshot::default();
        snapshot.nodes = nodes;
        snapshot
    }

    #[test]
    fn detector_ignores_the_queen() {
        let mut d = NeedsInputDetector::new();
        let mut calm = snapshot(vec![node("a", Some("h"), true, NodeState::Working)]);
        calm.add_queen();
        assert!(d.update(&calm).is_empty());
        let mut waiting = snapshot(vec![node("a", Some("h"), true, NodeState::NeedsInput)]);
        waiting.add_queen();
        assert_eq!(waiting.queen().unwrap().state, NodeState::NeedsInput);
        let requests = d.update(&waiting);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].target, "h");
    }

    #[test]
    fn detector_reports_only_transitions() {
        let mut d = NeedsInputDetector::new();
        let waiting = |id: &str, h| node(id, Some(h), true, NodeState::NeedsInput);
        let working = |id: &str, h| node(id, Some(h), true, NodeState::Working);
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
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].target, "b");
        assert_eq!(reqs[0].title, "Myrstack: label term:b needs input");
        assert_eq!(reqs[0].kind, NotifyKind::NeedsInput);
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
        assert_eq!(reqs.iter().map(|r| &r.target).collect::<Vec<_>>(), ["a"]);
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
        assert_eq!(reqs[0].target, "sub:s:a");
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

    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl Poster for Recorder {
        fn post(&self, title: &str, _body: &str) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(title.to_string());
            Ok(())
        }
    }

    #[test]
    fn notifier_thread_collapses_and_reports() {
        let posted = Arc::new(Mutex::new(Vec::new()));
        let notifier = Notifier::spawn_with(Box::new(Recorder(posted.clone())), None);
        notifier.notify(input("a"));
        notifier.notify(input("b"));
        notifier.notify(input("a"));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut delivered = Vec::new();
        while delivered.is_empty() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
            delivered = notifier.delivered();
        }
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].targets, vec!["a", "b"]);
        assert_eq!(
            *posted.lock().unwrap(),
            vec!["Myrstack: 2 agents need input"]
        );
    }
}

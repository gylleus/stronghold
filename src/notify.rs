//! macOS notifications for escalations and terminals waiting on input.
//!
//! A [`Notifier`] owns a thread that rate limits requests ([`RateLimiter`]), collapses bursts
//! into one summary and posts through a [`Poster`]. Every posted notification is also reported
//! back to the app as a [`Delivered`] event, which the app lists as in-app alerts that select
//! the node when clicked ([`click_target`]).
//!
//! Running from `Myrstack.app`, notifications go through `UNUserNotificationCenter`
//! ([`native`]) under Myrstack's name, carrying their targets as a [`payload`](encode_payload);
//! clicking one activates Myrstack and [`Notifier::clicked`] hands the targets to the app,
//! which selects the node like an alerts-menu entry. Unbundled (`cargo run`) there is no bundle
//! identity to post under, so [`OsascriptPoster`] posts via `display notification`: those show
//! under Script Editor and clicking them cannot reach Myrstack, leaving the alerts menu.

#[cfg(target_os = "macos")]
pub mod native;

use std::collections::{HashMap, HashSet, VecDeque};
use std::process::Command;
use std::sync::{Arc, Mutex};
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

/// A notification that was sent to the poster (even if posting failed), reported back to the
/// app for its alerts menu.
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

/// `userInfo` key of a native notification's payload.
pub const PAYLOAD_KEY: &str = "myrstack";

/// The payload a native notification carries: the targets it covers, as JSON.
pub fn encode_payload(targets: &[String]) -> String {
    serde_json::json!({ "targets": targets }).to_string()
}

/// Targets from an [`encode_payload`] string; `None` if it is not one.
pub fn decode_payload(payload: &str) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    value
        .get("targets")?
        .as_array()?
        .iter()
        .map(|t| t.as_str().map(str::to_string))
        .collect()
}

/// The node to select for a notification covering `targets`: the first one that resolves.
pub fn click_target(snapshot: &FleetSnapshot, targets: &[String]) -> Option<NodeId> {
    targets.iter().find_map(|t| resolve_target(snapshot, t))
}

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

/// Sets up native notifications early, so a click that launches the app is not missed. Call
/// before the event loop starts; does nothing unbundled.
pub fn init() {
    #[cfg(target_os = "macos")]
    if native::is_bundled() {
        native::install();
    }
}

/// `myrstack notify <title> [body] [--target <node id or handle>]... [--quiet]`: posts one
/// notification the way the app would, for checking that notifications work. From the app
/// bundle, `--quiet` asks for provisional permission, which needs no prompt: the notification
/// lands quietly in Notification Center. `None` for other commands.
pub fn run_cli(args: &[String]) -> Option<anyhow::Result<()>> {
    let (command, rest) = args.split_first()?;
    if command != "notify" {
        return None;
    }
    Some(notify_cli(rest))
}

fn notify_cli(args: &[String]) -> anyhow::Result<()> {
    let mut texts = Vec::new();
    let mut targets = Vec::new();
    let mut quiet = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--quiet" {
            quiet = true;
        } else if arg == "--target" {
            let target = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--target needs a value"))?;
            targets.push(target.clone());
        } else {
            texts.push(arg.clone());
        }
    }
    let [title, rest @ ..] = &texts[..] else {
        anyhow::bail!(
            "usage: myrstack notify <title> [body] [--target <node id or handle>]... [--quiet]"
        );
    };
    let notification = Outgoing {
        title: title.clone(),
        body: rest.join(" "),
        targets,
    };
    #[cfg(target_os = "macos")]
    if native::is_bundled() {
        native::install_with(if quiet {
            native::Permission::Quiet
        } else {
            native::Permission::Ask
        });
        // The first time, macOS asks whether Myrstack may notify; give the user time to answer.
        return native::NativePoster::new()
            .with_authorization_wait(Duration::from_secs(60))
            .post(&notification);
    }
    let _ = quiet;
    OsascriptPoster.post(&notification)
}

// ---------------------------------------------------------------------------
// Posting

/// Shows a notification on screen.
pub trait Poster: Send {
    fn post(&self, notification: &Outgoing) -> anyhow::Result<()>;
}

/// The poster for this process: native when running from `Myrstack.app`, else `osascript`.
pub fn default_poster() -> Box<dyn Poster> {
    #[cfg(target_os = "macos")]
    if native::is_bundled() {
        native::install();
        return Box::new(native::NativePoster::new());
    }
    Box::new(OsascriptPoster)
}

/// Posts with `osascript -e 'display notification …'`. Title and body go in as arguments, so
/// they need no AppleScript escaping.
#[derive(Debug, Default)]
pub struct OsascriptPoster;

impl Poster for OsascriptPoster {
    fn post(&self, notification: &Outgoing) -> anyhow::Result<()> {
        let Outgoing { title, body, .. } = notification;
        let output = Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "display notification (item 2 of argv) with title (item 1 of argv)",
                "-e",
                "end run",
                title.as_str(),
                body.as_str(),
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
    /// Targets of clicked notifications.
    clicks: Receiver<Vec<String>>,
}

impl Notifier {
    /// Starts the notifier thread with the [`default_poster`]; clicks on native notifications
    /// call `repaint` so the app picks them up.
    pub fn spawn(repaint: Option<Repaint>) -> Self {
        let shared = repaint.map(|r| Arc::new(Mutex::new(r)));
        let wake = || -> Option<Repaint> {
            let shared = shared.clone()?;
            Some(Box::new(move || {
                if let Ok(repaint) = shared.lock() {
                    repaint();
                }
            }))
        };
        Self::spawn_with(default_poster(), click_receiver(wake()), wake())
    }

    pub fn spawn_with(
        poster: Box<dyn Poster>,
        clicks: Receiver<Vec<String>>,
        repaint: Option<Repaint>,
    ) -> Self {
        let (requests, request_rx) = crossbeam_channel::unbounded();
        let (delivered_tx, delivered) = crossbeam_channel::unbounded();
        thread::Builder::new()
            .name("notifier".into())
            .spawn(move || run(&request_rx, &delivered_tx, poster.as_ref(), repaint))
            .expect("spawning notifier thread");
        Self {
            requests,
            delivered,
            clicks,
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

    /// Targets of the notifications clicked since the last call, oldest first.
    pub fn clicked(&self) -> Vec<Vec<String>> {
        self.clicks.try_iter().collect()
    }
}

/// Where clicks on native notifications arrive; never receives anything unbundled.
fn click_receiver(repaint: Option<Repaint>) -> Receiver<Vec<String>> {
    #[cfg(target_os = "macos")]
    if native::is_bundled() {
        return native::clicks(repaint);
    }
    let _ = repaint;
    crossbeam_channel::never()
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
        // A failed post (e.g. notifications turned off for Myrstack) still reaches the in-app
        // alerts, so nothing is lost.
        match poster.post(&out) {
            Ok(()) => log::info!("notified: {}", out.title),
            Err(err) => log::warn!("notification failed: {err:#}"),
        }
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

    #[test]
    fn payload_round_trips() {
        let targets = vec!["term_7".to_string(), "sub:s:\"odd\" id".to_string()];
        let payload = encode_payload(&targets);
        assert_eq!(decode_payload(&payload), Some(targets));
        assert_eq!(decode_payload(&encode_payload(&[])), Some(vec![]));
    }

    #[test]
    fn foreign_payloads_are_rejected() {
        assert_eq!(decode_payload("not json"), None);
        assert_eq!(decode_payload("{}"), None);
        assert_eq!(decode_payload(r#"{"targets":"term_7"}"#), None);
        assert_eq!(decode_payload(r#"{"targets":[1]}"#), None);
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

    /// A posted notification's payload, clicked, comes back from `clicked` and selects the
    /// node it was about.
    #[test]
    fn clicked_payload_routes_to_its_node() {
        let posted = Arc::new(Mutex::new(Vec::new()));
        let (click_tx, clicks) = crossbeam_channel::unbounded();
        let notifier = Notifier::spawn_with(Box::new(Recorder(posted.clone())), clicks, None);
        notifier.notify(input("term_w"));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while posted.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        // What the notification center hands back on a click: the payload the poster attached.
        let payload = posted.lock().unwrap()[0].1.clone();
        click_tx.send(decode_payload(&payload).unwrap()).unwrap();
        let clicked = notifier.clicked();
        assert_eq!(clicked, vec![vec!["term_w".to_string()]]);
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
            click_target(&snap, &clicked[0]).map(|id| id.0).as_deref(),
            Some("dispatch:r:new")
        );
        assert!(notifier.clicked().is_empty());
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

    struct Failing;

    impl Poster for Failing {
        fn post(&self, _: &Outgoing) -> anyhow::Result<()> {
            anyhow::bail!("notifications are turned off")
        }
    }

    #[test]
    fn failed_posts_still_reach_the_alerts() {
        let notifier = Notifier::spawn_with(Box::new(Failing), crossbeam_channel::never(), None);
        notifier.notify(input("a"));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut delivered = Vec::new();
        while delivered.is_empty() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
            delivered = notifier.delivered();
        }
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].targets, vec!["a"]);
    }

    /// Records each post's title and payload.
    struct Recorder(Arc<Mutex<Vec<(String, String)>>>);

    impl Poster for Recorder {
        fn post(&self, notification: &Outgoing) -> anyhow::Result<()> {
            self.0.lock().unwrap().push((
                notification.title.clone(),
                encode_payload(&notification.targets),
            ));
            Ok(())
        }
    }

    #[test]
    fn notifier_thread_collapses_and_reports() {
        let posted = Arc::new(Mutex::new(Vec::new()));
        let notifier = Notifier::spawn_with(
            Box::new(Recorder(posted.clone())),
            crossbeam_channel::never(),
            None,
        );
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
        let titles: Vec<_> = posted.lock().unwrap().iter().map(|p| p.0.clone()).collect();
        assert_eq!(titles, vec!["Myrstack: 2 agents need input"]);
    }
}

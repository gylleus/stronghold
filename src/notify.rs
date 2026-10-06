//! Notifications to Karl: from agents (the `notify` MCP tool), escalations, terminals waiting
//! on input and PRs ready for review.
//!
//! Every notification goes through one path. Sources build an [`Incoming`] (the whip does so
//! on its own thread through [`Notifier::notify`]); the app runs it through the
//! [`NotificationCenter`] ([`dispatch`]), which drops repeats, collapses duplicates, rate
//! limits each agent, records it in the inbox ([`history`], saved across restarts) and decides
//! from the [`NotifySettings`] ([`settings`]) whether it pops up (natively, in the app) and
//! which sound it plays. What pops up natively or plays goes to the [`Notifier`] thread as a
//! [`Post`]; it collapses bursts into one summary, caps posts per minute ([`PostLimiter`]),
//! posts through a [`Poster`] and plays the sound with `afplay`.
//!
//! Running from `Myrstack.app`, notifications go through `UNUserNotificationCenter`
//! ([`native`]) under Myrstack's name, carrying their targets and inbox ids as a
//! [`payload`](encode_payload); clicking one activates Myrstack and [`Notifier::clicked`] hands
//! them to the app, which selects the node and opens the link like an inbox entry. Unbundled
//! (`cargo run`) there is no bundle identity to post under, so [`OsascriptPoster`] posts via
//! `display notification`: those show under Script Editor and clicking them cannot reach
//! Myrstack, leaving the inbox.

pub mod agent;
pub mod dispatch;
pub mod history;
#[cfg(target_os = "macos")]
pub mod native;
pub mod pending;
pub mod settings;

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::model::{FleetSnapshot, NodeId, NodeKind, NodeState};

pub use dispatch::{Incoming, NotificationCenter, Outcome, Source};
pub use settings::{Delivery, NotifySettings, Severity, Sound};

/// Window of the global limit on posts.
pub const GLOBAL_WINDOW_MS: i64 = 60_000;
/// At most this many posts per [`GLOBAL_WINDOW_MS`].
pub const GLOBAL_MAX: usize = 5;
/// Posts arriving this close together are collapsed into one notification.
const BURST: Duration = Duration::from_millis(1500);
/// How often queued posts are retried while the global limit holds them back.
const RETRY: Duration = Duration::from_secs(5);

/// A notification to pop up natively and/or sound, from the dispatcher's [`Delivery`].
#[derive(Debug, Clone, PartialEq)]
pub struct Post {
    pub source: Source,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    /// What a click selects (node id, terminal handle or PR URL).
    pub target: Option<String>,
    /// Its inbox entry.
    pub id: Option<u64>,
    /// Show a macOS notification.
    pub native: bool,
    /// Play this file at `volume` (0–1).
    pub sound: Option<PathBuf>,
    pub volume: f32,
}

impl Post {
    /// The post for inbox entry `id` of `n`, delivered as `delivery`; `None` when nothing
    /// pops up natively or plays.
    pub fn new(
        n: &Incoming,
        id: u64,
        delivery: &Delivery,
        settings: &NotifySettings,
    ) -> Option<Self> {
        let sound = delivery.sound.as_ref().and_then(Sound::path);
        if !delivery.native && sound.is_none() {
            return None;
        }
        let mut title = n.title.clone();
        if n.source == Source::Agent
            && let Some(agent) = &n.agent_label
        {
            title = format!("{agent}: {title}");
        }
        Some(Self {
            source: n.source,
            severity: n.severity,
            title,
            body: n.message.clone(),
            target: n.target.clone(),
            id: Some(id),
            native: delivery.native,
            sound,
            volume: settings.volume(),
        })
    }
}

/// A notification to show, possibly a summary of several posts.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub title: String,
    pub body: String,
    /// Targets it covers, in post order.
    pub targets: Vec<String>,
    /// Inbox entries it covers.
    pub ids: Vec<u64>,
    /// Show it (some posts only play a sound).
    pub native: bool,
    /// The sound of the most severe post that has one.
    pub sound: Option<(PathBuf, f32)>,
}

impl Outgoing {
    /// A plain notification, as `myrstack notify` posts.
    pub fn simple(title: &str, body: &str, targets: Vec<String>) -> Self {
        Self {
            title: title.to_owned(),
            body: body.to_owned(),
            targets,
            ids: Vec::new(),
            native: true,
            sound: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Global limit

/// Pure global limit on posts, with burst collapsing: [`offer`](Self::offer) queues a post and
/// [`flush`](Self::flush) turns everything waiting into one [`Outgoing`] when the cap allows.
/// Duplicates, cooldowns and per-agent limits are the dispatcher's.
#[derive(Debug, Default)]
pub struct PostLimiter {
    /// Post times inside the global window.
    sent: VecDeque<i64>,
    pending: Vec<Post>,
}

impl PostLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn offer(&mut self, post: Post) {
        self.pending.push(post);
    }

    /// Whether posts are waiting for the global cap.
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

/// One post as itself; several as a summary such as `3 agents need input`.
pub fn collapse(posts: Vec<Post>) -> Outgoing {
    let mut targets: Vec<String> = Vec::new();
    for t in posts.iter().filter_map(|p| p.target.as_ref()) {
        if !targets.contains(t) {
            targets.push(t.clone());
        }
    }
    let ids = posts.iter().filter_map(|p| p.id).collect();
    let native = posts.iter().any(|p| p.native);
    let sound = posts
        .iter()
        .filter_map(|p| Some((p.severity, p.sound.clone()?, p.volume)))
        .max_by_key(|(severity, ..)| *severity)
        .map(|(_, path, volume)| (path, volume));
    let shown: Vec<&Post> = posts.iter().filter(|p| p.native).collect();
    if let [one] = &shown[..] {
        return Outgoing {
            title: one.title.clone(),
            body: one.body.clone(),
            targets,
            ids,
            native,
            sound,
        };
    }
    let what = if shown.iter().all(|p| p.source == Source::NeedsInput) {
        "agents need input"
    } else if shown.iter().all(|p| p.source == Source::Agent) {
        "notifications from agents"
    } else {
        "agents need attention"
    };
    let body = shown
        .iter()
        .map(|p| p.title.strip_prefix(TITLE_PREFIX).unwrap_or(&p.title))
        .collect::<Vec<_>>()
        .join("\n");
    Outgoing {
        title: format!("{TITLE_PREFIX}{} {what}", shown.len()),
        body,
        targets,
        ids,
        native,
        sound,
    }
}

const TITLE_PREFIX: &str = "Myrstack: ";

// ---------------------------------------------------------------------------
// Sources

/// A whip notification, from the engine's title and body.
///
/// The engine titles are `Myrstack: <handle> is waiting on you` and
/// `Myrstack: worker <handle> task <id> is stalled` / `Myrstack: coordinator <handle> run <id>
/// is stalled`; the target is the handle. Unknown titles use the whole title as the target.
pub fn whip_request(title: &str, body: &str) -> Incoming {
    let subject = title.strip_prefix(TITLE_PREFIX).unwrap_or(title);
    let (source, target) = if let Some(handle) = subject.strip_suffix(" is waiting on you") {
        (Source::NeedsInput, handle.to_string())
    } else if let Some(label) = subject.strip_suffix(" is stalled") {
        let mut words = label.split_whitespace();
        let handle = match words.next() {
            Some("worker" | "coordinator") => words.next(),
            _ => None,
        };
        (
            Source::Escalation,
            handle.map_or_else(|| title.to_string(), str::to_string),
        )
    } else {
        (Source::Escalation, title.to_string())
    };
    Incoming::new(source, Severity::Warn, title, body).with_target(target)
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

    /// Notifications for the nodes of `snapshot` that are NeedsInput now but were not before.
    pub fn update(&mut self, snapshot: &FleetSnapshot) -> Vec<Incoming> {
        let waiting: HashSet<NodeId> = snapshot
            .nodes
            .iter()
            .filter(|n| {
                n.state == NodeState::NeedsInput && n.kind != NodeKind::Queen && !n.is_parked()
            })
            .map(|n| n.id.clone())
            .collect();
        let requests = match &self.previous {
            None => Vec::new(),
            Some(previous) => snapshot
                .nodes
                .iter()
                .filter(|n| waiting.contains(&n.id) && !previous.contains(&n.id))
                .map(|n| {
                    let mut incoming = Incoming::new(
                        Source::NeedsInput,
                        Severity::Warn,
                        &format!("{TITLE_PREFIX}{} needs input", n.label),
                        n.preview.as_deref().unwrap_or("Waiting on you"),
                    )
                    .with_target(target_key(&n.id, n.terminal_handle.as_deref()));
                    incoming.agent_label = Some(n.label.clone());
                    incoming
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

/// What a clicked notification covered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Clicked {
    pub targets: Vec<String>,
    /// Inbox entries (none for notifications posted before inbox ids existed).
    pub ids: Vec<u64>,
}

/// The payload a native notification carries: its targets and inbox ids, as JSON.
pub fn encode_payload(targets: &[String], ids: &[u64]) -> String {
    serde_json::json!({ "targets": targets, "ids": ids }).to_string()
}

/// What an [`encode_payload`] string covers; `None` if it is not one.
pub fn decode_payload(payload: &str) -> Option<Clicked> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let targets = value
        .get("targets")?
        .as_array()?
        .iter()
        .map(|t| t.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let ids = match value.get("ids") {
        None => Vec::new(),
        Some(ids) => ids
            .as_array()?
            .iter()
            .map(serde_json::Value::as_u64)
            .collect::<Option<Vec<_>>>()?,
    };
    Some(Clicked { targets, ids })
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
    let notification = Outgoing::simple(title, &rest.join(" "), targets);
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
// Posting and sounds

/// Shows a notification on screen.
pub trait Poster: Send {
    fn post(&self, notification: &Outgoing) -> anyhow::Result<()>;
}

/// Plays a sound file.
pub trait Player: Send {
    fn play(&self, path: &Path, volume: f32) -> anyhow::Result<()>;
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

/// Plays through `afplay` without waiting for it to finish.
#[derive(Debug, Default)]
pub struct AfplayPlayer;

impl Player for AfplayPlayer {
    fn play(&self, path: &Path, volume: f32) -> anyhow::Result<()> {
        play_sound(path, volume)
    }
}

/// Starts `afplay -v <volume> <path>` and reaps it in the background. Fails when the file is
/// missing or `afplay` can't start.
pub fn play_sound(path: &Path, volume: f32) -> anyhow::Result<()> {
    anyhow::ensure!(path.is_file(), "no sound file at {}", path.display());
    let mut child = Command::new("afplay")
        .arg("-v")
        .arg(format!("{:.2}", volume.clamp(0.0, 1.0)))
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("afplay: {e}"))?;
    thread::Builder::new()
        .name("afplay".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

/// Called to wake the UI.
pub type Repaint = Box<dyn Fn() + Send>;

/// The notification thread, plus the channel background threads raise notifications on.
/// Share it with `Arc`.
pub struct Notifier {
    /// Notifications raised off the UI thread (the whip), for the app to dispatch.
    incoming: (Sender<Incoming>, Receiver<Incoming>),
    posts: Sender<Post>,
    /// Posting errors, for the Settings window.
    failures: Receiver<String>,
    /// What clicked notifications covered.
    clicks: Receiver<Clicked>,
    wake: Arc<Mutex<Option<Repaint>>>,
}

impl std::fmt::Debug for Notifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifier").finish_non_exhaustive()
    }
}

impl Notifier {
    /// Starts the notifier thread with the [`default_poster`] and `afplay`; clicks on native
    /// notifications and raised notifications call `repaint` so the app picks them up.
    pub fn spawn(repaint: Option<Repaint>) -> Self {
        let shared = Arc::new(Mutex::new(repaint));
        let wake = shared.clone();
        let click_wake: Repaint = Box::new(move || {
            if let Ok(repaint) = wake.lock()
                && let Some(repaint) = repaint.as_ref()
            {
                repaint();
            }
        });
        Self::spawn_with(
            default_poster(),
            Box::new(AfplayPlayer),
            click_receiver(Some(click_wake)),
            shared,
        )
    }

    pub fn spawn_with(
        poster: Box<dyn Poster>,
        player: Box<dyn Player>,
        clicks: Receiver<Clicked>,
        wake: Arc<Mutex<Option<Repaint>>>,
    ) -> Self {
        let (posts, post_rx) = crossbeam_channel::unbounded();
        let (failure_tx, failures) = crossbeam_channel::unbounded();
        let thread_wake = wake.clone();
        thread::Builder::new()
            .name("notifier".into())
            .spawn(move || {
                run(
                    &post_rx,
                    &failure_tx,
                    poster.as_ref(),
                    player.as_ref(),
                    &thread_wake,
                );
            })
            .expect("spawning notifier thread");
        Self {
            incoming: crossbeam_channel::unbounded(),
            posts,
            failures,
            clicks,
            wake,
        }
    }

    /// Raises a notification from any thread; the app dispatches it on its next frame.
    pub fn notify(&self, incoming: Incoming) {
        let _ = self.incoming.0.send(incoming);
        if let Ok(wake) = self.wake.lock()
            && let Some(wake) = wake.as_ref()
        {
            wake();
        }
    }

    /// Notifications raised since the last call, oldest first.
    pub fn incoming(&self) -> Vec<Incoming> {
        self.incoming.1.try_iter().collect()
    }

    /// Pops up and/or plays a dispatched notification; it may be delayed or merged into a
    /// summary.
    pub fn post(&self, post: Post) {
        let _ = self.posts.send(post);
    }

    /// Posting errors since the last call.
    pub fn failures(&self) -> Vec<String> {
        self.failures.try_iter().collect()
    }

    /// What the notifications clicked since the last call covered, oldest first.
    pub fn clicked(&self) -> Vec<Clicked> {
        self.clicks.try_iter().collect()
    }
}

/// Where clicks on native notifications arrive; never receives anything unbundled.
fn click_receiver(repaint: Option<Repaint>) -> Receiver<Clicked> {
    #[cfg(target_os = "macos")]
    if native::is_bundled() {
        return native::clicks(repaint);
    }
    let _ = repaint;
    crossbeam_channel::never()
}

fn run(
    posts: &Receiver<Post>,
    failures: &Sender<String>,
    poster: &dyn Poster,
    player: &dyn Player,
    wake: &Mutex<Option<Repaint>>,
) {
    let mut limiter = PostLimiter::new();
    loop {
        let wait = if limiter.has_pending() {
            RETRY
        } else {
            Duration::from_secs(3600)
        };
        match posts.recv_timeout(wait) {
            Ok(first) => {
                limiter.offer(first);
                // Gather the rest of the burst.
                while let Ok(next) = posts.recv_timeout(BURST) {
                    limiter.offer(next);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let Some(out) = limiter.flush(now_ms()) else {
            continue;
        };
        let mut failed = Vec::new();
        if let Some((path, volume)) = &out.sound
            && let Err(err) = player.play(path, *volume)
        {
            failed.push(format!("sound: {err:#}"));
        }
        // A failed post (e.g. notifications turned off for Myrstack) is still in the inbox.
        if out.native {
            match poster.post(&out) {
                Ok(()) => log::info!("notified: {}", out.title),
                Err(err) => failed.push(format!("{err:#}")),
            }
        }
        if failed.is_empty() {
            continue;
        }
        for failure in failed {
            log::warn!("notification failed: {failure}");
            let _ = failures.send(failure);
        }
        if let Ok(wake) = wake.lock()
            && let Some(wake) = wake.as_ref()
        {
            wake();
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests;

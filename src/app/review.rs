//! The app side of the review queue: ingests marks (control API, the pending file, `merge_ready`
//! bus messages), applies GitHub reads, notifies, saves, and draws the queue in the Queen panel,
//! the top bar and the fleet's "PR" badges.

use std::collections::HashSet;
use std::path::PathBuf;

use eframe::egui;
use serde_json::{Value, json};

use super::{MyrstackApp, now_ms, repaint_once};
use crate::control::{ControlError, ErrorCode};
use crate::model::NodeId;
use crate::notify::{Incoming, Severity, Source};
use crate::review::github::GhCli;
use crate::review::mark::{self, Origin};
use crate::review::poller::{GithubPoller, POLL_INTERVAL};
use crate::review::queue::{AddOutcome, ReviewQueue};
use crate::review::view::{AgentRef, ReviewRow, queue_json, resolve_agent, rows};
use crate::review::{ReadyMark, ReviewCommand, bus};
use crate::ui::review::{ReviewAction, SectionState};

/// How often merged/closed and long-resolved entries are dropped.
const PRUNE_EVERY_MS: i64 = 10_000;

pub(super) struct AppReview {
    queue: ReviewQueue,
    /// Where the queue is saved; `None` keeps it in memory.
    path: Option<PathBuf>,
    poller: GithubPoller,
    /// The queue changed since it was last saved.
    dirty: bool,
    /// `polled_at_ms` of the fleet data whose inbox was read last.
    inbox_read_at: Option<i64>,
    /// URLs last handed to the poller.
    watched: Vec<String>,
    last_prune_ms: i64,
    /// Visible rows last frame, to open the section when the first one arrives.
    last_count: usize,
    /// Open the section (and scroll to it) on the next draw.
    open_request: bool,
    scroll_request: bool,
}

impl AppReview {
    /// Loads the saved queue, takes the marks queued while the app was down and starts the
    /// GitHub poller.
    pub(super) fn start(ctx: &egui::Context) -> Self {
        let path = crate::review::queue::ReviewQueue::default_path();
        let mut queue = ReviewQueue::load(&path);
        let pending = crate::claude::hook::data_dir().join(mark::PENDING_FILE);
        let taken = mark::take_pending(&pending);
        if !taken.is_empty() {
            log::info!(
                "review: {} marks queued while Myrstack was down",
                taken.len()
            );
        }
        for m in &taken {
            queue.add(m);
        }
        let poller = GithubPoller::spawn(GhCli::default(), POLL_INTERVAL, Some(repaint_once(ctx)));
        Self::with(queue, Some(path), poller, !taken.is_empty())
    }

    fn with(queue: ReviewQueue, path: Option<PathBuf>, poller: GithubPoller, dirty: bool) -> Self {
        Self {
            queue,
            path,
            poller,
            dirty,
            inbox_read_at: None,
            watched: Vec::new(),
            last_prune_ms: 0,
            last_count: 0,
            open_request: false,
            scroll_request: false,
        }
    }

    /// Opens and scrolls to the section on the next draw of the Queen panel.
    pub(super) fn request_open(&mut self) {
        self.open_request = true;
        self.scroll_request = true;
    }
}

impl AppReview {
    pub(super) fn entries(&self) -> impl Iterator<Item = &crate::review::Entry> {
        self.queue.entries()
    }
}

impl MyrstackApp {
    /// The node of the agent with identity `origin`, in the current snapshot.
    pub(super) fn review_agent(&self, origin: &Origin) -> Option<AgentRef> {
        let snapshot = self.snapshot.as_ref()?;
        let terminals = self.data.as_ref().map_or(&[][..], |d| &d.terminals[..]);
        resolve_agent(origin, snapshot, terminals)
    }

    /// The listed rows with their agents resolved.
    pub(super) fn review_rows(&self, now_ms: i64) -> Vec<ReviewRow> {
        rows(&self.review.queue, now_ms, |o| self.review_agent(o))
    }

    /// Per frame: reads new bus messages, applies GitHub reads, prunes, keeps the poller's
    /// watch list current, notifies about new entries, marks fleet nodes and saves.
    pub(super) fn poll_review(&mut self) {
        let now = now_ms();
        self.ingest_bus(now);
        for (url, result) in self.review.poller.updates() {
            self.review.queue.apply_status(&url, result, now);
            self.review.dirty = true;
        }
        if now - self.review.last_prune_ms >= PRUNE_EVERY_MS {
            self.review.last_prune_ms = now;
            self.review.dirty |= self.review.queue.prune(now);
        }
        let urls = self.review.queue.poll_urls();
        if urls != self.review.watched {
            self.review.poller.watch(urls.clone());
            self.review.watched = urls;
        }
        let new = self.review.queue.take_new(now);
        if !new.is_empty() {
            self.review.dirty = true;
            if self.whip_window.whip().notify {
                for entry in &new {
                    let notification = self.review_notification(entry);
                    self.raise(notification);
                }
            }
        }
        let rows = self.review_rows(now);
        if rows.len() > self.review.last_count && self.review.last_count == 0 {
            self.review.open_request = true;
        }
        self.review.last_count = rows.len();
        let nodes: HashSet<NodeId> = rows
            .iter()
            .filter_map(|r| r.agent.as_ref().map(|a| a.id.clone()))
            .collect();
        self.fleet.set_pr_ready(nodes);
        self.save_review();
    }

    fn save_review(&mut self) {
        if !self.review.dirty {
            return;
        }
        self.review.dirty = false;
        if let Some(path) = &self.review.path
            && let Err(e) = self.review.queue.save(path)
        {
            log::warn!("review: saving {}: {e}", path.display());
        }
    }

    /// Adds the `merge_ready` messages of a new poll (the poller reads new PRs as soon as they
    /// join its watch list). The first poll after start is history: its entries are listed
    /// without notifications.
    fn ingest_bus(&mut self, now: i64) {
        let Some(data) = &self.data else { return };
        if self.review.inbox_read_at == Some(data.polled_at_ms) {
            return;
        }
        let first = self.review.inbox_read_at.is_none();
        self.review.inbox_read_at = Some(data.polled_at_ms);
        let marks = bus::marks_from_inbox(&data.inbox, now);
        let mut changed = Vec::new();
        for m in &marks {
            if self.review.queue.add(m) != AddOutcome::Unchanged {
                changed.push(m.pr.url());
            }
        }
        if changed.is_empty() {
            return;
        }
        self.review.dirty = true;
        if first {
            self.review.queue.silence(&changed);
        }
    }

    fn review_notification(&self, entry: &crate::review::Entry) -> Incoming {
        let title = entry.title();
        let agent = self
            .review_agent(&entry.origin)
            .map(|a| a.label)
            .or_else(|| entry.origin.terminal_handle.clone())
            .unwrap_or_else(|| "unknown agent".into());
        let mut body = format!("from {agent}");
        if let Some(summary) = &entry.summary {
            body.push_str(&format!(": {summary}"));
        }
        let title = format!(
            "Myrstack: PR ready for review: {}{}",
            entry.pr.short(),
            if title.is_empty() {
                String::new()
            } else {
                format!(" {title}")
            }
        );
        let mut n =
            Incoming::new(Source::Review, Severity::Info, &title, &body).with_target(entry.url());
        n.agent_label = Some(agent);
        n
    }

    /// Whether a notification or alert is about a PR in the queue (its target is the PR URL).
    pub(super) fn is_review_alert(&self, targets: &[String]) -> bool {
        targets.iter().any(|t| self.review.queue.get(t).is_some())
    }

    /// Answers a review method of the control API.
    pub(super) fn control_review(&mut self, command: ReviewCommand) -> Result<Value, ControlError> {
        let now = now_ms();
        match command {
            ReviewCommand::Ready(mark) => Ok(self.review_mark(&mark, now)),
            ReviewCommand::Queue => {
                let rows = self.review_rows(now);
                Ok(queue_json(&rows, self.review.queue.github_unavailable()))
            }
            ReviewCommand::Dismiss { url } => self.review_resolve(&url, false, now),
            ReviewCommand::Reviewed { url } => self.review_resolve(&url, true, now),
        }
    }

    fn review_mark(&mut self, mark: &ReadyMark, now: i64) -> Value {
        let outcome = self.review.queue.add(mark);
        log::info!(
            "review: {} marked ready ({outcome:?}) by {:?}",
            mark.pr,
            mark.origin
        );
        self.review.dirty = true;
        let url = mark.pr.url();
        // A new PR is read when it joins the watch list; a known one is read again now.
        if outcome != AddOutcome::New {
            self.review.poller.refresh(vec![url.clone()]);
        }
        let agent = self.review_agent(&mark.origin);
        let count = self.review_rows(now).len();
        self.save_review();
        json!({
            "url": url,
            "outcome": match outcome {
                AddOutcome::New => "new",
                AddOutcome::Updated => "updated",
                AddOutcome::Unchanged => "unchanged",
            },
            "agent": agent,
            "origin": mark.origin,
            "count": count,
        })
    }

    fn review_resolve(
        &mut self,
        url: &str,
        reviewed: bool,
        now: i64,
    ) -> Result<Value, ControlError> {
        let found = if reviewed {
            self.review.queue.mark_reviewed(url, now)
        } else {
            self.review.queue.dismiss(url, now)
        };
        if !found {
            return Err(ControlError::new(
                ErrorCode::NotFound,
                format!("{url} is not in the review queue"),
            ));
        }
        self.review.dirty = true;
        self.save_review();
        Ok(json!({ "url": url, "resolved": if reviewed { "reviewed" } else { "dismissed" } }))
    }

    /// The top bar's badge; a click opens the queue in the Queen panel.
    pub(super) fn review_badge(&mut self, ui: &mut egui::Ui) {
        if crate::ui::review::review_badge(ui, self.review.last_count) {
            self.open_review_queue();
        }
    }

    /// Selects the Queen and opens the review section (badge, palette).
    pub(super) fn open_review_queue(&mut self) {
        self.select_queen();
        self.review.request_open();
    }

    /// What the Queen panel's review section draws this frame: its rows, the GitHub outage
    /// note and whether to open and scroll to it (a pending request is taken).
    pub(super) fn review_view(
        &mut self,
        now_ms: i64,
    ) -> (Vec<ReviewRow>, Option<String>, SectionState) {
        let state = SectionState {
            open: std::mem::take(&mut self.review.open_request),
            scroll: std::mem::take(&mut self.review.scroll_request),
        };
        let note = self.review.queue.github_unavailable().map(str::to_owned);
        (self.review_rows(now_ms), note, state)
    }

    /// Applies a click in the review section; returns a node to select.
    pub(super) fn apply_review_action(&mut self, action: ReviewAction) -> Option<NodeId> {
        let now = now_ms();
        match action {
            ReviewAction::Open(url) => open_in_browser(&url),
            ReviewAction::Dismiss(url) => {
                let _ = self.review_resolve(&url, false, now);
            }
            ReviewAction::Reviewed(url) => {
                let _ = self.review_resolve(&url, true, now);
            }
            ReviewAction::Select(id) => return Some(id),
        }
        None
    }
}

/// `open <url>` for GitHub URLs only.
fn open_in_browser(url: &str) {
    if crate::review::PrRef::parse(url).is_none() {
        log::warn!("review: not opening {url:?}");
        return;
    }
    if let Err(e) = std::process::Command::new("open").arg(url).spawn() {
        log::warn!("review: open {url}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::github::{GhError, GhSource, PrStatus};
    use crate::review::mark::MarkSource;

    struct NoGh;

    impl GhSource for NoGh {
        fn view(&self, _: &str, _: i64) -> Result<PrStatus, GhError> {
            Err(GhError::Unavailable("test".into()))
        }
    }

    #[test]
    fn pending_marks_are_taken_into_a_fresh_queue() {
        let dir = tempfile::tempdir().unwrap();
        let pending = dir.path().join(mark::PENDING_FILE);
        let m = ReadyMark::new(
            "https://github.com/o/r/pull/1",
            None,
            None,
            Origin::default(),
            MarkSource::Cli,
            1,
        )
        .unwrap();
        mark::append_pending(&pending, &m).unwrap();
        let mut queue = ReviewQueue::default();
        for m in mark::take_pending(&pending) {
            queue.add(&m);
        }
        let review = AppReview::with(
            queue,
            None,
            GithubPoller::spawn(NoGh, POLL_INTERVAL, None),
            true,
        );
        assert_eq!(review.queue.visible(2).len(), 1);
        assert!(!pending.exists());
    }
}

//! The review queue: PRs agents marked ready, keyed by PR URL, with their latest GitHub status
//! and what the user did with them. Pure state; the app feeds it marks and statuses, and saves it
//! to `<data dir>/review-queue.json`.
//!
//! An entry is shown until the user dismisses it or marks it reviewed (a later mark shows it
//! again, and so does a new push after "reviewed"), or until GitHub says it merged or closed
//! and [`GONE_GRACE_MS`] passed. Drafts are never shown. Entries from the bus (a worker's
//! `merge_ready` to its coordinator) show only once GitHub says the PR carries the
//! coordinator's `✅` ready marker on its current head.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::review::github::{GhError, MarkerKind, PrState, PrStatus, same_sha};
use crate::review::mark::{MarkSource, Origin, ReadyMark};
use crate::review::pr::PrRef;

/// File the queue is saved to, in the data dir.
pub const QUEUE_FILE: &str = "review-queue.json";
/// How long a merged or closed PR stays listed.
pub const GONE_GRACE_MS: i64 = 5 * 60_000;
/// Dismissed or reviewed entries are forgotten this long after.
pub const RESOLVED_TTL_MS: i64 = 14 * 24 * 3_600_000;
/// Most entries kept; the oldest marks go first.
pub const MAX_ENTRIES: usize = 200;

/// What the user did with an entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolution {
    Dismissed {
        at_ms: i64,
    },
    /// Reviewed at `head_sha` (when known): a push after it shows the entry again.
    Reviewed {
        at_ms: i64,
        #[serde(default)]
        head_sha: Option<String>,
    },
}

impl Resolution {
    pub fn at_ms(&self) -> i64 {
        match self {
            Self::Dismissed { at_ms } | Self::Reviewed { at_ms, .. } => *at_ms,
        }
    }
}

/// One PR in the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub pr: PrRef,
    /// The title the agent gave, if any (GitHub's title wins once known).
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// Who marked it most recently.
    #[serde(default)]
    pub origin: Origin,
    pub source: MarkSource,
    pub first_marked_ms: i64,
    /// Time of the newest mark.
    pub marked_ms: i64,
    #[serde(default)]
    pub status: Option<PrStatus>,
    /// The last `gh` error for this PR, cleared by the next good read.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub resolution: Option<Resolution>,
    /// When GitHub first said merged or closed.
    #[serde(default)]
    pub gone_since_ms: Option<i64>,
    /// A notification went out (or was deliberately skipped) for the current mark.
    #[serde(default)]
    pub notified: bool,
}

impl Entry {
    fn new(mark: &ReadyMark) -> Self {
        Self {
            pr: mark.pr.clone(),
            title: mark.title.clone(),
            summary: mark.summary.clone(),
            origin: mark.origin.clone(),
            source: mark.source.clone(),
            first_marked_ms: mark.at_ms,
            marked_ms: mark.at_ms,
            status: None,
            error: None,
            resolution: None,
            gone_since_ms: None,
            notified: false,
        }
    }

    pub fn url(&self) -> String {
        self.pr.url()
    }

    /// GitHub's title, else the agent's, else empty.
    pub fn title(&self) -> &str {
        self.status
            .as_ref()
            .map(|s| s.title.as_str())
            .filter(|t| !t.is_empty())
            .or(self.title.as_deref())
            .unwrap_or_default()
    }

    /// The user dismissed or reviewed it after its newest mark, and no push followed the review.
    pub fn is_resolved(&self) -> bool {
        match &self.resolution {
            None => false,
            Some(r) if r.at_ms() < self.marked_ms => false,
            Some(Resolution::Dismissed { .. }) => true,
            Some(Resolution::Reviewed { head_sha, .. }) => match (head_sha, self.status.as_ref()) {
                (Some(reviewed), Some(status)) if !status.head_sha.is_empty() => {
                    same_sha(reviewed, &status.head_sha)
                }
                _ => true,
            },
        }
    }

    /// Reviewed, then pushed to: shown again with a note.
    pub fn updated_since_review(&self) -> bool {
        matches!(self.resolution, Some(Resolution::Reviewed { .. }))
            && !self.is_resolved()
            && self
                .resolution
                .as_ref()
                .is_some_and(|r| r.at_ms() >= self.marked_ms)
    }

    fn gone_past_grace(&self, now_ms: i64) -> bool {
        self.gone_since_ms
            .is_some_and(|t| now_ms - t >= GONE_GRACE_MS)
    }

    fn is_bus(&self) -> bool {
        matches!(self.source, MarkSource::Bus { .. })
    }
}

/// How [`ReviewQueue::add`] changed the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    New,
    /// A newer mark of a known PR.
    Updated,
    /// The mark is not newer than what the queue has.
    Unchanged,
}

/// Every PR marked ready, by canonical URL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewQueue {
    entries: BTreeMap<String, Entry>,
    /// Why no PR can be read from GitHub (gh missing or logged out); `None` when it works.
    github_unavailable: Option<String>,
}

impl ReviewQueue {
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    pub fn get(&self, url: &str) -> Option<&Entry> {
        self.entries.get(&canonical(url))
    }

    pub fn github_unavailable(&self) -> Option<&str> {
        self.github_unavailable.as_deref()
    }

    /// Records a mark. A mark newer than the entry's refreshes it and shows it again.
    pub fn add(&mut self, mark: &ReadyMark) -> AddOutcome {
        let key = mark.pr.url();
        let Some(entry) = self.entries.get_mut(&key) else {
            self.entries.insert(key, Entry::new(mark));
            return AddOutcome::New;
        };
        if mark.at_ms <= entry.marked_ms {
            if entry.title.is_none() {
                entry.title.clone_from(&mark.title);
            }
            if entry.summary.is_none() {
                entry.summary.clone_from(&mark.summary);
            }
            return AddOutcome::Unchanged;
        }
        let was_resolved = entry.is_resolved();
        entry.marked_ms = mark.at_ms;
        entry.source = mark.source.clone();
        if !mark.origin.is_empty() {
            entry.origin = mark.origin.clone();
        }
        if mark.title.is_some() {
            entry.title.clone_from(&mark.title);
        }
        if mark.summary.is_some() {
            entry.summary.clone_from(&mark.summary);
        }
        if was_resolved && !entry.is_resolved() {
            entry.notified = false;
        }
        AddOutcome::Updated
    }

    /// Applies one `gh` read of `url`.
    pub fn apply_status(&mut self, url: &str, result: Result<PrStatus, GhError>, now_ms: i64) {
        match result {
            Err(GhError::Unavailable(e)) => self.github_unavailable = Some(e),
            Err(GhError::Failed(e)) => {
                if let Some(entry) = self.entries.get_mut(&canonical(url)) {
                    entry.error = Some(e);
                }
            }
            Ok(status) => {
                self.github_unavailable = None;
                let Some(entry) = self.entries.get_mut(&canonical(url)) else {
                    return;
                };
                let first = entry.status.is_none();
                if !status.state.is_gone() {
                    entry.gone_since_ms = None;
                } else if entry.gone_since_ms.is_none() {
                    // Already merged the first time we look: nothing to review, drop it now.
                    entry.gone_since_ms = Some(if first {
                        now_ms - GONE_GRACE_MS
                    } else {
                        now_ms
                    });
                    if first {
                        entry.notified = true;
                    }
                }
                entry.status = Some(status);
                entry.error = None;
            }
        }
    }

    /// Whether `entry` is listed: not resolved, not gone for long, not a draft, and (for bus
    /// entries) carrying a `✅` ready marker on its head. A `merge_ready` message is a worker's
    /// hand-off to its coordinator, not a request to the user; only the coordinator's marker or
    /// `mark_pr_ready` puts a PR in front of them.
    pub fn is_visible(&self, entry: &Entry, now_ms: i64) -> bool {
        let status = entry.status.as_ref();
        !entry.is_resolved()
            && !entry.gone_past_grace(now_ms)
            && !status.is_some_and(|s| s.state == PrState::Draft)
            && (!entry.is_bus() || status.is_some_and(ready_on_head))
    }

    /// The listed entries, newest mark first.
    pub fn visible(&self, now_ms: i64) -> Vec<&Entry> {
        let mut list: Vec<&Entry> = self
            .entries
            .values()
            .filter(|e| self.is_visible(e, now_ms))
            .collect();
        list.sort_by(|a, b| b.marked_ms.cmp(&a.marked_ms).then(a.pr.cmp(&b.pr)));
        list
    }

    /// Listed entries that were never notified; they are marked notified.
    pub fn take_new(&mut self, now_ms: i64) -> Vec<Entry> {
        let urls: Vec<String> = self
            .visible(now_ms)
            .into_iter()
            .filter(|e| !e.notified)
            .map(Entry::url)
            .collect();
        urls.iter()
            .filter_map(|u| {
                let e = self.entries.get_mut(u)?;
                e.notified = true;
                Some(e.clone())
            })
            .collect()
    }

    /// Marks every current entry as notified (the first bus read after start: those messages
    /// are history, not news).
    pub fn silence(&mut self, urls: &[String]) {
        for url in urls {
            if let Some(e) = self.entries.get_mut(&canonical(url)) {
                e.notified = true;
            }
        }
    }

    pub fn dismiss(&mut self, url: &str, now_ms: i64) -> bool {
        self.resolve(url, Resolution::Dismissed { at_ms: now_ms })
    }

    pub fn mark_reviewed(&mut self, url: &str, now_ms: i64) -> bool {
        let head_sha = self
            .get(url)
            .and_then(|e| e.status.as_ref())
            .map(|s| s.head_sha.clone())
            .filter(|h| !h.is_empty());
        self.resolve(
            url,
            Resolution::Reviewed {
                at_ms: now_ms,
                head_sha,
            },
        )
    }

    fn resolve(&mut self, url: &str, resolution: Resolution) -> bool {
        let Some(entry) = self.entries.get_mut(&canonical(url)) else {
            return false;
        };
        entry.resolution = Some(resolution);
        entry.notified = true;
        true
    }

    /// Drops merged/closed entries past the grace period, long-resolved ones and the oldest
    /// beyond [`MAX_ENTRIES`]. Returns whether anything was dropped.
    pub fn prune(&mut self, now_ms: i64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|_, e| {
            let long_resolved = e.is_resolved()
                && e.resolution
                    .as_ref()
                    .is_some_and(|r| now_ms - r.at_ms() > RESOLVED_TTL_MS);
            !e.gone_past_grace(now_ms) && !long_resolved
        });
        if self.entries.len() > MAX_ENTRIES {
            let mut by_age: Vec<(i64, String)> = self
                .entries
                .iter()
                .map(|(k, e)| (e.marked_ms, k.clone()))
                .collect();
            by_age.sort();
            for (_, key) in by_age.iter().take(self.entries.len() - MAX_ENTRIES) {
                self.entries.remove(key);
            }
        }
        self.entries.len() != before
    }

    /// PRs to read from GitHub: everything not merged/closed and not dismissed.
    pub fn poll_urls(&self) -> Vec<String> {
        self.entries
            .values()
            .filter(|e| e.gone_since_ms.is_none())
            .filter(|e| {
                !(e.is_resolved() && matches!(e.resolution, Some(Resolution::Dismissed { .. })))
            })
            .map(Entry::url)
            .collect()
    }

    // -----------------------------------------------------------------------
    // Saving

    /// `<data dir>/review-queue.json`.
    pub fn default_path() -> PathBuf {
        crate::claude::hook::data_dir().join(QUEUE_FILE)
    }

    /// The saved queue; empty when the file is missing or unreadable.
    pub fn load(path: &Path) -> Self {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    log::warn!("review queue {}: {e}", path.display());
                }
                return Self::default();
            }
        };
        match serde_json::from_str::<Saved>(&text) {
            Ok(saved) => Self {
                entries: saved.entries.into_iter().map(|e| (e.url(), e)).collect(),
                github_unavailable: None,
            },
            Err(e) => {
                log::warn!(
                    "review queue {}: unreadable, starting empty: {e}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Writes the queue (0600) through a temporary file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let saved = Saved {
            version: 1,
            entries: self.entries.values().cloned().collect(),
        };
        let json = serde_json::to_vec_pretty(&saved).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&json)?;
        }
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, path)
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    entries: Vec<Entry>,
}

/// The PR description opens with `✅ Ready for review` naming the current head.
fn ready_on_head(status: &PrStatus) -> bool {
    status
        .marker
        .as_ref()
        .is_some_and(|m| m.kind == MarkerKind::Ready)
        && status.marker_on_head() == Some(true)
}

/// The queue's key for a URL: its canonical PR URL, or the text itself when it is not one.
fn canonical(url: &str) -> String {
    PrRef::parse(url).map_or_else(|| url.to_owned(), |p| p.url())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::github::{Ci, Marker};

    const URL: &str = "https://github.com/acme/api/pull/255";

    fn mark(source: MarkSource, at_ms: i64) -> ReadyMark {
        ReadyMark::new(
            URL,
            Some("gate legacy config retirement"),
            Some("green"),
            Origin {
                terminal_handle: Some("term_a".into()),
                ..Origin::default()
            },
            source,
            at_ms,
        )
        .unwrap()
    }

    fn bus(at_ms: i64) -> ReadyMark {
        mark(
            MarkSource::Bus {
                message_id: format!("msg_{at_ms}"),
            },
            at_ms,
        )
    }

    fn status(state: PrState, head: &str) -> PrStatus {
        PrStatus {
            number: 255,
            title: "recs: gate legacy config retirement".into(),
            url: URL.into(),
            state,
            head_sha: head.into(),
            ci: Ci::Passing { total: 3 },
            ankra: None,
            marker: None,
            review_decision: None,
            merged_at: None,
            fetched_at_ms: 0,
        }
    }

    fn urls(q: &ReviewQueue, now: i64) -> Vec<String> {
        q.visible(now).into_iter().map(Entry::url).collect()
    }

    #[test]
    fn tool_mark_shows_at_once_and_notifies_once() {
        let mut q = ReviewQueue::default();
        assert_eq!(q.add(&mark(MarkSource::Tool, 10)), AddOutcome::New);
        assert_eq!(urls(&q, 10), [URL]);
        assert_eq!(q.get(URL).unwrap().title(), "gate legacy config retirement");
        let new = q.take_new(10);
        assert_eq!(new.len(), 1);
        assert!(q.take_new(11).is_empty());
        assert_eq!(q.add(&mark(MarkSource::Tool, 10)), AddOutcome::Unchanged);
        assert_eq!(q.add(&mark(MarkSource::Cli, 20)), AddOutcome::Updated);
        assert!(
            q.take_new(20).is_empty(),
            "a refresh of a shown entry is not news"
        );
        q.apply_status(URL, Ok(status(PrState::Open, "abcdef1234")), 30);
        assert_eq!(
            q.get(URL).unwrap().title(),
            "recs: gate legacy config retirement",
            "GitHub's title wins"
        );
    }

    fn with_marker(state: PrState, head: &str, kind: MarkerKind, sha: &str) -> PrStatus {
        PrStatus {
            marker: Some(Marker {
                kind,
                note: None,
                sha: Some(sha.into()),
            }),
            ..status(state, head)
        }
    }

    #[test]
    fn bus_marks_wait_for_github_and_merged_ones_never_show() {
        let mut q = ReviewQueue::default();
        q.add(&bus(10));
        assert!(urls(&q, 10).is_empty(), "waits for its first status");
        q.apply_status(URL, Ok(status(PrState::Merged, "abcdef1")), 20);
        assert!(urls(&q, 20).is_empty());
        assert!(q.take_new(20).is_empty());
        assert!(q.prune(20));
        assert!(q.get(URL).is_none());

        let mut q = ReviewQueue::default();
        q.add(&bus(10));
        q.apply_status(URL, Err(GhError::Unavailable("gh auth login".into())), 20);
        assert_eq!(q.github_unavailable(), Some("gh auth login"));
        assert!(
            urls(&q, 20).is_empty(),
            "a bus message alone, unverified, is not a ready mark"
        );
        q.apply_status(URL, Err(GhError::Failed("not found".into())), 21);
        assert!(urls(&q, 21).is_empty());
    }

    #[test]
    fn bus_hand_off_without_a_ready_marker_on_head_stays_out() {
        // A worker's merge_ready while the coordinator's marker says not ready (api#649).
        let mut q = ReviewQueue::default();
        q.add(&bus(10));
        let not_ready = with_marker(PrState::Open, "bbbbbbb222", MarkerKind::NotReady, "aaaaaaa");
        q.apply_status(URL, Ok(not_ready), 20);
        assert!(urls(&q, 20).is_empty());
        assert!(q.take_new(20).is_empty(), "and no notification");

        let stale = with_marker(PrState::Open, "bbbbbbb222", MarkerKind::Ready, "aaaaaaa");
        q.apply_status(URL, Ok(stale), 30);
        assert!(urls(&q, 30).is_empty(), "a ready marker for an older head");

        q.apply_status(URL, Ok(status(PrState::Open, "bbbbbbb222")), 40);
        assert!(urls(&q, 40).is_empty(), "no marker at all");

        let ready = with_marker(PrState::Open, "bbbbbbb222", MarkerKind::Ready, "bbbbbbb");
        q.apply_status(URL, Ok(ready), 50);
        assert_eq!(urls(&q, 50), [URL], "the coordinator marked the head ready");
        assert_eq!(q.take_new(50).len(), 1);
    }

    #[test]
    fn drafts_never_show() {
        // A worker's merge_ready about a draft (infrastructure-gitops#1541).
        let mut q = ReviewQueue::default();
        q.add(&bus(10));
        let draft = with_marker(PrState::Draft, "bbbbbbb222", MarkerKind::Ready, "bbbbbbb");
        q.apply_status(URL, Ok(draft.clone()), 20);
        assert!(urls(&q, 20).is_empty());

        let mut q = ReviewQueue::default();
        q.add(&mark(MarkSource::Tool, 10));
        assert_eq!(urls(&q, 10), [URL], "before the first read");
        q.apply_status(URL, Ok(draft), 20);
        assert!(urls(&q, 20).is_empty(), "not even from mark_pr_ready");
        q.apply_status(URL, Ok(status(PrState::Open, "bbbbbbb222")), 30);
        assert_eq!(urls(&q, 30), [URL], "shown once out of draft");
    }

    #[test]
    fn mark_pr_ready_shows_whatever_the_marker_says() {
        let mut q = ReviewQueue::default();
        q.add(&mark(MarkSource::Tool, 10));
        let not_ready = with_marker(PrState::Open, "bbbbbbb222", MarkerKind::NotReady, "aaaaaaa");
        q.apply_status(URL, Ok(not_ready), 20);
        assert_eq!(urls(&q, 20), [URL], "the coordinator's explicit mark wins");
        q.apply_status(URL, Err(GhError::Unavailable("gh auth login".into())), 30);
        assert_eq!(urls(&q, 30), [URL]);
    }

    #[test]
    fn merged_after_being_shown_leaves_after_the_grace_period() {
        let mut q = ReviewQueue::default();
        q.add(&mark(MarkSource::Tool, 0));
        q.apply_status(URL, Ok(status(PrState::Open, "abcdef1")), 100);
        q.apply_status(URL, Ok(status(PrState::Merged, "abcdef1")), 1_000);
        assert_eq!(urls(&q, 1_000), [URL], "still listed right after merging");
        assert!(q.poll_urls().is_empty(), "merged PRs aren't polled");
        assert!(!q.prune(1_000 + GONE_GRACE_MS - 1));
        assert!(q.prune(1_000 + GONE_GRACE_MS));
        assert!(q.entries().next().is_none());

        let mut q = ReviewQueue::default();
        q.add(&mark(MarkSource::Tool, 0));
        q.apply_status(URL, Ok(status(PrState::Open, "a1b2c3d4")), 1);
        q.apply_status(URL, Ok(status(PrState::Closed, "a1b2c3d4")), 2);
        q.apply_status(URL, Ok(status(PrState::Open, "a1b2c3d4")), 3);
        assert_eq!(q.get(URL).unwrap().gone_since_ms, None, "reopened");
    }

    #[test]
    fn dismiss_hides_until_marked_again() {
        let mut q = ReviewQueue::default();
        q.add(&mark(MarkSource::Tool, 10));
        q.take_new(10);
        assert!(q.dismiss(URL, 20));
        assert!(urls(&q, 20).is_empty());
        assert!(q.poll_urls().is_empty());
        assert_eq!(q.add(&bus(15)), AddOutcome::Updated);
        assert!(
            urls(&q, 21).is_empty(),
            "a mark from before the dismissal keeps it hidden"
        );
        assert_eq!(q.add(&mark(MarkSource::Tool, 30)), AddOutcome::Updated);
        assert_eq!(urls(&q, 30), [URL]);
        assert_eq!(q.take_new(30).len(), 1, "marked again: news again");
        assert!(!q.dismiss("https://github.com/o/r/pull/1", 40));
    }

    #[test]
    fn reviewed_reappears_after_a_push() {
        let mut q = ReviewQueue::default();
        q.add(&mark(MarkSource::Tool, 10));
        q.apply_status(URL, Ok(status(PrState::Open, "aaaaaaa111")), 11);
        assert!(q.mark_reviewed(URL, 20));
        assert!(urls(&q, 20).is_empty());
        assert_eq!(q.poll_urls(), [URL], "still polled to see new pushes");
        q.apply_status(URL, Ok(status(PrState::Open, "aaaaaaa111")), 30);
        assert!(urls(&q, 30).is_empty());
        q.apply_status(URL, Ok(status(PrState::Open, "bbbbbbb222")), 40);
        assert_eq!(urls(&q, 40), [URL]);
        assert!(q.get(URL).unwrap().updated_since_review());
    }

    #[test]
    fn prune_forgets_long_resolved_and_caps_the_size() {
        let mut q = ReviewQueue::default();
        q.add(&mark(MarkSource::Tool, 0));
        q.dismiss(URL, 1);
        assert!(!q.prune(RESOLVED_TTL_MS));
        assert!(q.prune(RESOLVED_TTL_MS + 2));

        let mut q = ReviewQueue::default();
        for n in 1..=(MAX_ENTRIES as u64 + 5) {
            let m = ReadyMark::new(
                &format!("https://github.com/o/r/pull/{n}"),
                None,
                None,
                Origin::default(),
                MarkSource::Tool,
                n as i64,
            )
            .unwrap();
            q.add(&m);
        }
        assert!(q.prune(0));
        assert_eq!(q.entries().count(), MAX_ENTRIES);
        assert!(
            q.get("https://github.com/o/r/pull/1").is_none(),
            "oldest go first"
        );
        assert!(q.get("https://github.com/o/r/pull/205").is_some());
    }

    #[test]
    fn per_pr_errors_and_silencing() {
        let mut q = ReviewQueue::default();
        q.add(&mark(MarkSource::Tool, 10));
        q.apply_status(URL, Err(GhError::Failed("not found".into())), 11);
        assert_eq!(q.get(URL).unwrap().error.as_deref(), Some("not found"));
        assert_eq!(
            urls(&q, 11),
            [URL],
            "shown with its error rather than hidden"
        );
        q.silence(&[URL.to_owned()]);
        assert!(q.take_new(12).is_empty());
    }

    #[test]
    fn saves_and_loads_privately() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(QUEUE_FILE);
        let mut q = ReviewQueue::default();
        q.add(&mark(MarkSource::Tool, 10));
        q.apply_status(URL, Ok(status(PrState::Open, "abcdef1")), 11);
        q.mark_reviewed(URL, 12);
        q.save(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let loaded = ReviewQueue::load(&path);
        assert_eq!(loaded, q);
        assert_eq!(
            ReviewQueue::load(&dir.path().join("missing.json")),
            ReviewQueue::default()
        );
        fs::write(&path, "{not json").unwrap();
        assert_eq!(ReviewQueue::load(&path), ReviewQueue::default());
    }
}

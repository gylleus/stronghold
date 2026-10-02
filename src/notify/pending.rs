//! Agent notifications sent while the app was down: the `notify` tool appends them to
//! `<data dir>/pending-notifications.jsonl`, and the app takes them into the inbox once it runs.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::agent::AgentNotice;

/// Notifications waiting for the app, one JSON object per line.
pub const PENDING_FILE: &str = "pending-notifications.jsonl";
/// The pending file is trimmed to its newest half past this size.
pub const MAX_PENDING_BYTES: u64 = 256 * 1024;
/// A queued notification younger than this still pops up and plays when the app takes it;
/// older ones go into the inbox silently.
pub const FRESH_MS: i64 = 5 * 60_000;

/// One line of the pending file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Line {
    /// When the agent sent it.
    at_ms: i64,
    /// The control API `notify` params, origin included.
    notice: Value,
}

/// A notification taken from the pending file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub notice: AgentNotice,
    /// When the agent sent it.
    pub at_ms: i64,
}

impl Queued {
    /// Whether it is recent enough to still pop up at `now_ms`.
    pub fn is_fresh(&self, now_ms: i64) -> bool {
        now_ms - self.at_ms < FRESH_MS
    }
}

/// `<data dir>/pending-notifications.jsonl`; `None` in unit tests so they never touch the real
/// file.
pub fn default_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(crate::claude::hook::data_dir().join(PENDING_FILE))
}

/// Appends `notice`, sent at `at_ms`, to the pending file.
pub fn append(path: &Path, notice: &AgentNotice, at_ms: i64) -> io::Result<()> {
    let line = Line {
        at_ms,
        notice: notice.to_params(),
    };
    crate::review::mark::append_jsonl(path, &line, MAX_PENDING_BYTES)
}

/// Takes every pending notification (oldest first) and removes the file. Lines that no longer
/// parse are skipped.
pub fn take(path: &Path) -> Vec<Queued> {
    crate::review::mark::take_jsonl::<Line>(path)
        .into_iter()
        .filter_map(|line| match AgentNotice::from_params(&line.notice) {
            Ok(notice) => Some(Queued {
                notice,
                at_ms: line.at_ms,
            }),
            Err(e) => {
                log::warn!("notify: skipping a pending notification: {}", e.message);
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::notify::Severity;
    use crate::review::mark::Origin;

    fn notice(title: &str) -> AgentNotice {
        let origin = Origin {
            terminal_handle: Some("term_1".into()),
            pane_key: Some("tab:pane".into()),
            ..Origin::default()
        };
        AgentNotice::new(
            title,
            Some("the message"),
            Some("warn"),
            Some("https://github.com/o/r/pull/1"),
            Some("cp-12"),
            origin,
        )
        .unwrap()
    }

    #[test]
    fn queued_notifications_keep_attribution_and_are_taken_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(PENDING_FILE);
        append(&path, &notice("first"), 1_000).unwrap();
        append(&path, &notice("second"), 2_000).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"not json\n{\"at_ms\": 3, \"notice\": {\"message\": \"no title\"}}\n")
            .unwrap();

        let taken = take(&path);
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].notice, notice("first"));
        assert_eq!(taken[0].at_ms, 1_000);
        let second = &taken[1].notice;
        assert_eq!(second.title, "second");
        assert_eq!(second.severity, Severity::Warn);
        assert_eq!(second.url.as_deref(), Some("https://github.com/o/r/pull/1"));
        assert_eq!(second.agent.as_deref(), Some("cp-12"));
        assert_eq!(second.origin.terminal_handle.as_deref(), Some("term_1"));
        assert!(!path.exists());
        assert!(take(&path).is_empty());
    }

    /// Queue, take and dispatch as the app does on start: the stale one lands silently, the
    /// fresh one pops up, both attributed to the sender with their links.
    #[test]
    fn taken_notifications_reach_the_inbox() {
        use crate::notify::{NotificationCenter, NotifySettings};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PENDING_FILE);
        let now = 100 * 60_000;
        append(&path, &notice("stale"), now - 60 * 60_000).unwrap();
        append(&path, &notice("fresh"), now - 60_000).unwrap();
        let mut center = NotificationCenter::default();
        let settings = NotifySettings::default();
        let outcomes: Vec<_> = take(&path)
            .into_iter()
            .map(|q| {
                let fresh = q.is_fresh(now);
                center.dispatch_queued(q.notice.incoming(None), q.at_ms, fresh, &settings, now)
            })
            .collect();
        let delivery = |i: usize| match &outcomes[i] {
            crate::notify::Outcome::Delivered { delivery, .. } => delivery.clone(),
            other => panic!("{other:?}"),
        };
        assert!(delivery(0).is_silent());
        assert!(delivery(1).native);
        let entries = center.history.entries();
        assert_eq!(entries.len(), 2);
        for e in entries {
            assert_eq!(e.sender.as_deref(), Some("term_1"));
            assert_eq!(e.url.as_deref(), Some("https://github.com/o/r/pull/1"));
            assert!(!e.read);
        }
        assert_eq!(entries[0].at_ms, now - 60 * 60_000);
    }

    #[test]
    fn only_recent_ones_are_fresh() {
        let queued = Queued {
            notice: notice("t"),
            at_ms: 10_000,
        };
        assert!(queued.is_fresh(10_000 + FRESH_MS - 1));
        assert!(!queued.is_fresh(10_000 + FRESH_MS));
    }
}

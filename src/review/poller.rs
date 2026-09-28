//! Reads the queue's PRs from GitHub on a background thread: every [`POLL_INTERVAL`], and at
//! once for PRs that just joined the watch list or were asked for.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::poller::Repaint;
use crate::review::github::{GhError, GhSource, PrStatus};

/// How often every watched PR is read again.
pub const POLL_INTERVAL: Duration = Duration::from_secs(180);

enum Command {
    /// The PRs to read on every pass; new ones are read at once.
    Watch(Vec<String>),
    /// Read these now.
    Refresh(Vec<String>),
}

/// One read's outcome.
pub type StatusUpdate = (String, Result<PrStatus, GhError>);

/// The background reader; stops when dropped.
pub struct GithubPoller {
    commands: Sender<Command>,
    updates: Receiver<StatusUpdate>,
}

impl GithubPoller {
    pub fn spawn(
        source: impl GhSource + 'static,
        interval: Duration,
        repaint: Option<Repaint>,
    ) -> Self {
        let (commands, command_rx) = crossbeam_channel::unbounded();
        let (update_tx, updates) = crossbeam_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("github-poller".into())
            .spawn(move || run(&source, interval, &command_rx, &update_tx, repaint.as_ref()));
        if let Err(e) = spawned {
            log::warn!("review: cannot start the GitHub poller: {e}");
        }
        Self { commands, updates }
    }

    /// Sets the watch list (unchanged lists cost nothing; new URLs are read at once).
    pub fn watch(&self, urls: Vec<String>) {
        let _ = self.commands.send(Command::Watch(urls));
    }

    /// Reads `urls` now.
    pub fn refresh(&self, urls: Vec<String>) {
        if !urls.is_empty() {
            let _ = self.commands.send(Command::Refresh(urls));
        }
    }

    /// Reads finished since the last call.
    pub fn updates(&self) -> Vec<StatusUpdate> {
        self.updates.try_iter().collect()
    }
}

fn run(
    source: &dyn GhSource,
    interval: Duration,
    commands: &Receiver<Command>,
    updates: &Sender<StatusUpdate>,
    repaint: Option<&Repaint>,
) {
    let mut watched: BTreeSet<String> = BTreeSet::new();
    let mut next_pass = Instant::now() + interval;
    loop {
        let wait = next_pass.saturating_duration_since(Instant::now());
        let mut due: Vec<String> = match commands.recv_timeout(wait) {
            Ok(Command::Watch(urls)) => {
                let urls: BTreeSet<String> = urls.into_iter().collect();
                let new = urls.difference(&watched).cloned().collect();
                watched = urls;
                new
            }
            Ok(Command::Refresh(urls)) => urls,
            Err(RecvTimeoutError::Timeout) => {
                next_pass = Instant::now() + interval;
                watched.iter().cloned().collect()
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        due.dedup();
        for url in due {
            let now = crate::control::server::now_ms();
            let result = source.view(&url, now);
            if let Err(e) = &result {
                log::debug!("review: gh pr view {url}: {e}");
            }
            let unavailable = matches!(result, Err(GhError::Unavailable(_)));
            if updates.send((url, result)).is_err() {
                return;
            }
            if let Some(repaint) = repaint {
                repaint();
            }
            // With gh missing or logged out, one answer covers every PR.
            if unavailable {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::review::github::{Ci, PrState};

    #[derive(Clone, Default)]
    struct Fake {
        calls: Arc<Mutex<Vec<String>>>,
        unavailable: bool,
    }

    impl GhSource for Fake {
        fn view(&self, url: &str, now_ms: i64) -> Result<PrStatus, GhError> {
            self.calls.lock().unwrap().push(url.to_owned());
            if self.unavailable {
                return Err(GhError::Unavailable("gh auth login".into()));
            }
            Ok(PrStatus {
                number: 1,
                title: "t".into(),
                url: url.into(),
                state: PrState::Open,
                head_sha: "abcdef1".into(),
                ci: Ci::None,
                ankra: None,
                marker: None,
                review_decision: None,
                merged_at: None,
                fetched_at_ms: now_ms,
            })
        }
    }

    fn wait_for(poller: &GithubPoller, n: usize) -> Vec<StatusUpdate> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while got.len() < n && Instant::now() < deadline {
            got.extend(poller.updates());
            std::thread::sleep(Duration::from_millis(5));
        }
        got
    }

    #[test]
    fn new_urls_are_read_at_once_and_every_interval() {
        let fake = Fake::default();
        let poller = GithubPoller::spawn(fake.clone(), Duration::from_millis(400), None);
        poller.watch(vec!["a".into(), "b".into()]);
        let first = wait_for(&poller, 2);
        assert_eq!(first.len(), 2);
        poller.watch(vec!["a".into(), "b".into(), "c".into()]);
        let second = wait_for(&poller, 1);
        assert_eq!(second[0].0, "c", "only the new URL is read at once");
        let pass = wait_for(&poller, 3);
        let mut urls: Vec<String> = pass.into_iter().map(|u| u.0).collect();
        urls.sort();
        assert_eq!(urls, ["a", "b", "c"], "the interval re-reads everything");
        poller.refresh(vec!["b".into()]);
        assert!(wait_for(&poller, 1).iter().any(|u| u.0 == "b"));
    }

    #[test]
    fn unavailable_gh_is_asked_once_per_pass() {
        let fake = Fake {
            unavailable: true,
            ..Fake::default()
        };
        let poller = GithubPoller::spawn(fake.clone(), Duration::from_secs(600), None);
        poller.watch(vec!["a".into(), "b".into(), "c".into()]);
        let got = wait_for(&poller, 1);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0].1, Err(GhError::Unavailable(_))));
        assert_eq!(fake.calls.lock().unwrap().len(), 1);
    }
}

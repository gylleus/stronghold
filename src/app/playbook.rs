//! The app side of "who runs which playbook version": a background thread re-reads the fetch
//! log and the playbook on a slow cadence; the UI maps the result onto the current snapshot.

use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded};

use super::MyrstackApp;
use crate::playbook::Playbook;
use crate::playbook::fetches::{self, FetchSummary, PlaybookStatus};

/// How often the fetch log and playbook version are re-read.
pub const INTERVAL: Duration = Duration::from_secs(30);

/// The newest [`FetchSummary`] from the reader thread.
pub(super) struct PlaybookWatch {
    rx: Receiver<FetchSummary>,
    summary: FetchSummary,
    /// Bumped on every new summary, so the control API republishes.
    pub(super) generation: u64,
}

impl PlaybookWatch {
    pub(super) fn spawn(repaint: Box<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = bounded(1);
        if let Some(path) = fetches::default_path()
            && let Err(e) = thread::Builder::new()
                .name("playbook-fetches".into())
                .spawn(move || run(&path, &tx, &*repaint))
        {
            log::warn!("playbook fetch reader not started: {e}");
        }
        Self {
            rx,
            summary: FetchSummary::default(),
            generation: 0,
        }
    }

    /// Takes a new summary if the thread sent one; true when it did.
    fn poll(&mut self) -> bool {
        match self.rx.try_recv() {
            Ok(summary) => {
                self.generation += 1;
                self.summary = summary;
                true
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => false,
        }
    }
}

fn run(path: &std::path::Path, tx: &Sender<FetchSummary>, repaint: &(dyn Fn() + Send + Sync)) {
    let mut last: Option<FetchSummary> = None;
    loop {
        let playbook = Playbook::locate().ok();
        let summary = fetches::load_summary(path, playbook.as_ref());
        if last.as_ref() != Some(&summary) {
            if tx.send(summary.clone()).is_err() {
                return;
            }
            repaint();
            last = Some(summary);
        }
        thread::sleep(INTERVAL);
    }
}

impl MyrstackApp {
    /// Takes the reader thread's newest fetch summary.
    pub(super) fn poll_playbook(&mut self) {
        self.playbook_watch.poll();
    }

    /// The playbook's current version, as the reader thread last saw it.
    pub(super) fn playbook_version(&self) -> Option<String> {
        let current = self.playbook_watch.summary.current.as_ref()?;
        Some(current.to_string())
    }

    /// Fetches mapped onto the current snapshot's nodes.
    pub(super) fn playbook_status(&self) -> Option<PlaybookStatus> {
        let snapshot = self.snapshot.as_ref()?;
        Some(PlaybookStatus::map(&self.playbook_watch.summary, snapshot))
    }
}

//! Runs the whip on a background thread.
//!
//! [`WhipRunner::spawn`] starts a thread that keeps the newest [`FleetData`] the app sends it
//! and, every `tick_ms` once the first data has arrived, runs [`WhipEngine::tick`] and
//! [`WhipEngine::execute`]. After each tick it publishes a [`WhipStatus`] for the UI. In dry-run
//! the executor is a [`DryRunExecutor`] and the live executor factory is never called.
//!
//! [`WhipLoop`] is the thread's state without the thread, so it can be tested with a fake
//! clock and executor.

use std::collections::BTreeSet;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};

use crate::fleet::FleetData;
use crate::whip::actions::{DryRunExecutor, WhipExecutor, WhipLogEntry};
use crate::whip::engine::WhipEngine;
use crate::whip::settings::WhipSettings;

/// Newest log entries carried in each [`WhipStatus`].
pub const STATUS_LOG_LEN: usize = 200;

/// Builds the executor that really sends; called once per live tick.
pub type ExecutorFactory = Box<dyn Fn() -> Box<dyn WhipExecutor> + Send>;

/// Called after each published status, e.g. to repaint the UI.
pub type Repaint = Box<dyn Fn() + Send>;

/// What the whip evaluates: one poll plus the terminals currently waiting on Karl.
#[derive(Debug, Clone, Default)]
pub struct WhipInput {
    pub data: FleetData,
    /// Handles of terminals in the NeedsInput state; never nudged.
    pub needs_input: BTreeSet<String>,
}

/// The whip's state after a tick, for the UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WhipStatus {
    /// Newest action log entries, oldest first, at most [`STATUS_LOG_LEN`].
    pub log: Vec<WhipLogEntry>,
    /// Actions logged since the runner started (the log itself is bounded).
    pub actions_logged: u64,
    /// Terminal handles with an escalated, unrecovered stall (rendered Stalled).
    pub escalated: BTreeSet<String>,
    /// Stall findings of the last tick.
    pub findings: usize,
    /// When the last tick ran, in ms since the Unix epoch; `None` before the first.
    pub last_tick_ms: Option<i64>,
    pub enabled: bool,
    pub dry_run: bool,
}

/// The whip thread's state: engine, settings and the newest input.
pub struct WhipLoop {
    engine: WhipEngine,
    settings: WhipSettings,
    input: Option<WhipInput>,
    actions_logged: u64,
    last_tick_ms: Option<i64>,
}

impl WhipLoop {
    pub fn new(settings: WhipSettings) -> Self {
        Self {
            engine: WhipEngine::new(),
            settings,
            input: None,
            actions_logged: 0,
            last_tick_ms: None,
        }
    }

    pub fn settings(&self) -> &WhipSettings {
        &self.settings
    }

    pub fn has_input(&self) -> bool {
        self.input.is_some()
    }

    /// Replaces the input; only the newest is kept.
    pub fn set_input(&mut self, input: WhipInput) {
        self.input = Some(input);
    }

    pub fn set_settings(&mut self, settings: WhipSettings) {
        self.settings = settings;
    }

    /// Runs one tick on the newest input. `None` (and nothing done) before the first input.
    /// `live` is called only when the settings are not dry-run.
    pub fn tick(
        &mut self,
        now_ms: i64,
        live: &dyn Fn() -> Box<dyn WhipExecutor>,
    ) -> Option<WhipStatus> {
        let input = self.input.as_ref()?;
        let actions = self
            .engine
            .tick(&input.data, &input.needs_input, &self.settings, now_ms);
        if !actions.is_empty() {
            let executor: Box<dyn WhipExecutor> = if self.settings.dry_run {
                Box::new(DryRunExecutor::new())
            } else {
                live()
            };
            self.engine
                .execute(&actions, executor.as_ref(), &self.settings, now_ms);
            self.actions_logged += actions.len() as u64;
        }
        self.last_tick_ms = Some(now_ms);
        Some(self.status())
    }

    pub fn status(&self) -> WhipStatus {
        let log = self.engine.log();
        let skip = log.len().saturating_sub(STATUS_LOG_LEN);
        WhipStatus {
            log: log.iter().skip(skip).cloned().collect(),
            actions_logged: self.actions_logged,
            escalated: self.engine.escalated_handles(),
            findings: self.engine.findings().len(),
            last_tick_ms: self.last_tick_ms,
            enabled: self.settings.enabled,
            dry_run: self.settings.dry_run,
        }
    }
}

enum Command {
    Input(Box<WhipInput>),
    Settings(WhipSettings),
    Shutdown,
}

/// Handle to the whip thread. Dropping it stops the thread after its current tick.
pub struct WhipRunner {
    commands: Sender<Command>,
    statuses: Receiver<WhipStatus>,
    thread: Option<JoinHandle<()>>,
}

impl WhipRunner {
    /// Starts the thread. `live` builds the sending executor; it is only called for ticks with
    /// `dry_run` off.
    pub fn spawn(settings: WhipSettings, live: ExecutorFactory, repaint: Option<Repaint>) -> Self {
        let (commands, command_rx) = unbounded();
        let (status_tx, statuses) = unbounded();
        let thread = thread::Builder::new()
            .name("whip".into())
            .spawn(move || {
                run(
                    WhipLoop::new(settings),
                    &command_rx,
                    &status_tx,
                    &live,
                    repaint,
                )
            })
            .expect("spawn whip thread");
        Self {
            commands,
            statuses,
            thread: Some(thread),
        }
    }

    /// Hands the thread the newest poll; older unprocessed ones are superseded.
    pub fn send_input(&self, input: WhipInput) {
        let _ = self.commands.send(Command::Input(Box::new(input)));
    }

    /// Applies new settings from the next tick on (a status reflecting them is published at
    /// once).
    pub fn update_settings(&self, settings: WhipSettings) {
        let _ = self.commands.send(Command::Settings(settings));
    }

    /// The newest status published since the last call, if any.
    pub fn latest(&self) -> Option<WhipStatus> {
        self.statuses.try_iter().last()
    }
}

impl Drop for WhipRunner {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        // Not joined: a live tick can take several orca call timeouts and must not block quitting.
        drop(self.thread.take());
    }
}

fn run(
    mut whip: WhipLoop,
    commands: &Receiver<Command>,
    statuses: &Sender<WhipStatus>,
    live: &ExecutorFactory,
    repaint: Option<Repaint>,
) {
    let publish = |status: WhipStatus| {
        if statuses.send(status).is_ok()
            && let Some(repaint) = &repaint
        {
            repaint();
        }
    };
    let tick_every = |s: &WhipSettings| Duration::from_millis(s.tick_ms.max(1) as u64);
    let mut last_tick: Option<Instant> = None;
    // `None` until the first input arrives.
    let mut deadline: Option<Instant> = None;
    loop {
        let command = match deadline {
            None => match commands.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            },
            Some(d) => match commands.recv_deadline(d) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        match command {
            Some(Command::Input(input)) => {
                let first = !whip.has_input();
                whip.set_input(*input);
                if first {
                    deadline = Some(Instant::now());
                }
            }
            Some(Command::Settings(settings)) => {
                whip.set_settings(settings);
                if let Some(last) = last_tick {
                    deadline = Some(last + tick_every(whip.settings()));
                }
                publish(whip.status());
            }
            Some(Command::Shutdown) => return,
            None => {
                let now = Instant::now();
                if let Some(status) = whip.tick(now_ms(), &|| live()) {
                    publish(status);
                }
                last_tick = Some(now);
                deadline = Some(now + tick_every(whip.settings()));
            }
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
    use crate::orca::{OutgoingMessage, Terminal, Timestamp};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const MIN: i64 = 60_000;
    const HANDLE: &str = "term_waiting";

    /// Counts factory calls and records what the executors it built were asked to do.
    #[derive(Clone, Default)]
    struct Recorder {
        built: Arc<AtomicUsize>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    struct RecordingExecutor(Arc<Mutex<Vec<String>>>);

    impl WhipExecutor for RecordingExecutor {
        fn send_bus(&self, msg: &OutgoingMessage) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(format!("bus {:?}", msg.to));
            Ok(())
        }
        fn send_terminal(&self, handle: &str, _text: &str) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(format!("terminal {handle}"));
            Ok(())
        }
        fn notify(&self, title: &str, _body: &str) {
            self.0.lock().unwrap().push(format!("notify {title}"));
        }
    }

    impl Recorder {
        fn factory(&self) -> ExecutorFactory {
            let me = self.clone();
            Box::new(move || {
                me.built.fetch_add(1, Ordering::SeqCst);
                Box::new(RecordingExecutor(me.calls.clone()))
            })
        }
        fn built(&self) -> usize {
            self.built.load(Ordering::SeqCst)
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    /// A terminal that has waited on Karl since an hour before `now_ms`: the first tick notifies.
    fn waiting_input(now_ms: i64) -> WhipInput {
        WhipInput {
            data: FleetData {
                polled_at_ms: now_ms,
                terminals: vec![Terminal {
                    handle: HANDLE.into(),
                    last_output_at: Some(Timestamp::Millis(now_ms - 60 * MIN)),
                    ..Default::default()
                }],
                ..Default::default()
            },
            needs_input: BTreeSet::from([HANDLE.to_string()]),
        }
    }

    #[test]
    fn loop_does_nothing_before_input() {
        let rec = Recorder::default();
        let factory = rec.factory();
        let mut whip = WhipLoop::new(WhipSettings::default());
        assert_eq!(whip.tick(1_000, &|| factory()), None);
        assert_eq!(whip.status().last_tick_ms, None);
    }

    #[test]
    fn loop_dry_run_logs_without_building_the_live_executor() {
        let rec = Recorder::default();
        let factory = rec.factory();
        let now = 1_790_000_000_000;
        let mut whip = WhipLoop::new(WhipSettings::default());
        whip.set_input(waiting_input(now));
        let status = whip.tick(now, &|| factory()).unwrap();
        assert!(status.dry_run);
        assert_eq!(status.actions_logged, 1);
        assert_eq!(status.log.len(), 1);
        assert_eq!(status.log[0].action, "notify needs-input");
        assert_eq!(status.log[0].outcome, "dry-run");
        assert_eq!(status.last_tick_ms, Some(now));
        assert_eq!(rec.built(), 0);
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn loop_live_uses_the_factory() {
        let rec = Recorder::default();
        let factory = rec.factory();
        let now = 1_790_000_000_000;
        let mut whip = WhipLoop::new(WhipSettings::default());
        whip.set_input(waiting_input(now));
        whip.set_settings(WhipSettings {
            dry_run: false,
            ..WhipSettings::default()
        });
        let status = whip.tick(now, &|| factory()).unwrap();
        assert!(!status.dry_run);
        assert_eq!(status.log[0].outcome, "sent");
        assert_eq!(rec.built(), 1);
        assert_eq!(
            rec.calls(),
            vec![format!("notify Myrstack: {HANDLE} is waiting on you")]
        );
        // Nothing to do on the next tick: the executor is not even built.
        whip.tick(now + MIN, &|| factory()).unwrap();
        assert_eq!(rec.built(), 1);
    }

    #[test]
    fn status_log_is_capped() {
        let mut whip = WhipLoop::new(WhipSettings::default());
        let now = 1_790_000_000_000;
        let exec = DryRunExecutor::new();
        for i in 0..(STATUS_LOG_LEN + 5) {
            let action = crate::whip::WhipAction::NotifyNeedsInput {
                handle: format!("term_{i}"),
                since_ms: now,
            };
            whip.engine.execute(&[action], &exec, &whip.settings, now);
        }
        let status = whip.status();
        assert_eq!(status.log.len(), STATUS_LOG_LEN);
        assert_eq!(status.log.last().unwrap().target, "terminal term_204");
    }

    fn wait_for_status(runner: &WhipRunner, timeout: Duration) -> Option<WhipStatus> {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            if let Some(s) = runner.latest() {
                return Some(s);
            }
            thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn runner_ticks_only_after_input_and_applies_settings() {
        let rec = Recorder::default();
        let settings = WhipSettings {
            tick_ms: 20,
            ..WhipSettings::default()
        };
        let runner = WhipRunner::spawn(settings.clone(), rec.factory(), None);
        assert_eq!(wait_for_status(&runner, Duration::from_millis(100)), None);

        runner.send_input(waiting_input(now_ms()));
        let status = wait_for_status(&runner, Duration::from_secs(2)).expect("a tick");
        assert!(status.dry_run);
        assert_eq!(status.actions_logged, 1);
        assert_eq!(status.log[0].outcome, "dry-run");

        // Going live publishes the new mode at once; the wait was already notified, so nothing
        // is sent.
        runner.update_settings(WhipSettings {
            dry_run: false,
            ..settings
        });
        let until = Instant::now() + Duration::from_secs(2);
        let mut live = None;
        while live.is_none() && Instant::now() < until {
            live = wait_for_status(&runner, Duration::from_millis(50)).filter(|s| !s.dry_run);
        }
        let live = live.expect("a live status");
        assert_eq!(live.actions_logged, 1);
        assert_eq!(rec.built(), 0);
        assert!(rec.calls().is_empty());
    }
}

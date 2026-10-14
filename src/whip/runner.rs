//! Runs the whip on a background thread.
//!
//! [`WhipRunner::spawn`] starts a thread that keeps the newest [`ClanData`] the app sends it
//! and, every `tick_ms` once the first data has arrived, runs [`WhipEngine::tick`] and
//! [`WhipEngine::execute`]. After each tick it publishes a [`WhipStatus`] for the UI. In dry-run
//! the executor is a [`DryRunExecutor`] and the live executor factory is called only for ticks
//! with event forwarding to do, which is always live ([`WhipAction::live_in_dry_run`]). The
//! forwarded set is saved after each tick that changed it, when a store path is given.
//!
//! Log entries are delivered once: each status carries only the entries logged since the
//! previous one, numbered with a sequence number, and the UI keeps the history.
//!
//! [`WhipLoop`] is the thread's state without the thread, so it can be tested with a fake
//! clock and executor.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};

use crate::clan::ClanData;
use crate::whip::actions::{DryRunExecutor, WhipAction, WhipExecutor, WhipLogEntry};
use crate::whip::engine::WhipEngine;
use crate::whip::forward;
use crate::whip::manual::ManualNudge;
use crate::whip::rules::StallTarget;
use crate::whip::settings::WhipSettings;

/// Most log entries held for the UI between two published statuses; older ones are dropped.
pub const PENDING_LOG_LEN: usize = 1000;

/// Builds the executor that really sends; called once per live tick.
pub type ExecutorFactory = Box<dyn Fn() -> Box<dyn WhipExecutor> + Send>;

/// Called after each published status, e.g. to repaint the UI.
pub type Repaint = Box<dyn Fn() + Send>;

/// What the whip evaluates: one poll plus the terminals currently waiting on the user.
#[derive(Debug, Clone, Default)]
pub struct WhipInput {
    pub data: ClanData,
    /// Handles of terminals in the NeedsInput state; never nudged.
    pub needs_input: BTreeSet<String>,
    /// Handles of parked agents ([`crate::park`]): never nudged, escalated or notified about.
    pub parked: BTreeSet<String>,
}

/// A log entry numbered in logging order, with what it is about as structured fields so the UI
/// can link it to a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhipLogItem {
    /// 1 for the runner's first entry, then +1 per entry.
    pub seq: u64,
    pub entry: WhipLogEntry,
    /// Terminal the action was about.
    pub handle: Option<String>,
    pub run_id: Option<String>,
    /// Set for a single worker target.
    pub task_id: Option<String>,
}

impl WhipLogItem {
    /// Numbers `entry` and takes its structured target from the `action` it logged.
    pub fn new(seq: u64, entry: WhipLogEntry, action: &WhipAction) -> Self {
        let (handle, target) = match action {
            WhipAction::Nudge {
                handle, targets, ..
            } => (Some(handle), (targets.len() == 1).then(|| &targets[0])),
            WhipAction::Escalate { target, .. } => (Some(target_handle(target)), Some(target)),
            WhipAction::NotifyNeedsInput { handle, .. } => (Some(handle), None),
            WhipAction::Forward {
                handle, run_ids, ..
            } => {
                let run_id = (run_ids.len() == 1).then(|| run_ids[0].clone());
                return Self {
                    seq,
                    entry,
                    handle: Some(handle.clone()),
                    run_id,
                    task_id: None,
                };
            }
            WhipAction::CoordinatorUnreachable { run_id, .. }
            | WhipAction::ForwardFailing { run_id, .. } => {
                return Self {
                    seq,
                    entry,
                    handle: None,
                    run_id: Some(run_id.clone()),
                    task_id: None,
                };
            }
        };
        let (run_id, task_id) = match target {
            Some(StallTarget::Worker {
                run_id, task_id, ..
            }) => (Some(run_id.clone()), Some(task_id.clone())),
            Some(StallTarget::Coordinator { run_id, .. }) => (Some(run_id.clone()), None),
            None => (None, None),
        };
        Self {
            seq,
            entry,
            handle: handle.cloned(),
            run_id,
            task_id,
        }
    }
}

fn target_handle(target: &StallTarget) -> &String {
    match target {
        StallTarget::Worker { handle, .. } | StallTarget::Coordinator { handle, .. } => handle,
    }
}

/// The whip's state after a tick, for the UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WhipStatus {
    /// Entries logged since the previous published status, oldest first.
    pub new_log: Vec<WhipLogItem>,
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

/// What a "whip now" request did: the log entries it produced, in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WhipNowReport {
    /// `false` when an immediate tick could not run yet (no clan data).
    pub ran: bool,
    pub items: Vec<WhipLogItem>,
    /// The automatic whip's mode during the request; manual nudges are live regardless.
    pub dry_run: bool,
}

/// The whip thread's state: engine, settings and the newest input.
pub struct WhipLoop {
    engine: WhipEngine,
    settings: WhipSettings,
    input: Option<WhipInput>,
    actions_logged: u64,
    last_tick_ms: Option<i64>,
    /// Entries not yet handed out by [`WhipLoop::take_status`].
    pending: Vec<WhipLogItem>,
    /// Where the forwarded set is saved; see [`crate::whip::forward::save_forwarded`].
    forwarded_path: Option<PathBuf>,
}

impl WhipLoop {
    pub fn new(settings: WhipSettings) -> Self {
        Self {
            engine: WhipEngine::new(),
            settings,
            input: None,
            actions_logged: 0,
            last_tick_ms: None,
            pending: Vec::new(),
            forwarded_path: None,
        }
    }

    /// Keeps the forwarded set in `path` across restarts: starts from the one saved there, if
    /// any, and saves it after each tick that changed it.
    pub fn with_forwarded_store(mut self, path: PathBuf) -> Self {
        if let Some(sent) = forward::load_forwarded(&path) {
            self.engine.restore_forwarded(sent);
        }
        self.forwarded_path = Some(path);
        self
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
    /// `live` is called only when the settings are not dry-run or an action is
    /// [`WhipAction::live_in_dry_run`].
    pub fn tick(
        &mut self,
        now_ms: i64,
        live: &dyn Fn() -> Box<dyn WhipExecutor>,
    ) -> Option<WhipStatus> {
        let input = self.input.as_ref()?;
        self.engine.set_parked(input.parked.clone());
        let actions = self
            .engine
            .tick(&input.data, &input.needs_input, &self.settings, now_ms);
        if !actions.is_empty() {
            let needs_live = actions.iter().any(WhipAction::live_in_dry_run);
            let executor: Box<dyn WhipExecutor> = if self.settings.dry_run && !needs_live {
                Box::new(DryRunExecutor::new())
            } else {
                live()
            };
            self.engine
                .execute(&actions, executor.as_ref(), &self.settings, now_ms);
            self.record(&actions);
        }
        self.save_forwarded();
        self.last_tick_ms = Some(now_ms);
        Some(self.take_status())
    }

    /// Sends manual nudges now through the live executor, even in dry-run: they are the user's
    /// explicit request. Threshold and cooldown do not apply. Returns the status carrying the
    /// new log entries.
    pub fn manual(
        &mut self,
        nudges: &[ManualNudge],
        now_ms: i64,
        live: &dyn Fn() -> Box<dyn WhipExecutor>,
    ) -> WhipStatus {
        if !nudges.is_empty() {
            let executor = live();
            for nudge in nudges {
                let entry = self.engine.execute_manual(nudge, executor.as_ref(), now_ms);
                self.actions_logged += 1;
                let (run_id, task_id) = match nudge.targets.as_slice() {
                    [
                        StallTarget::Worker {
                            run_id, task_id, ..
                        },
                    ] => (Some(run_id.clone()), Some(task_id.clone())),
                    [StallTarget::Coordinator { run_id, .. }] => (Some(run_id.clone()), None),
                    _ => (None, None),
                };
                self.pending.push(WhipLogItem {
                    seq: self.actions_logged,
                    entry,
                    handle: Some(nudge.handle.clone()),
                    run_id,
                    task_id,
                });
            }
            let excess = self.pending.len().saturating_sub(PENDING_LOG_LEN);
            self.pending.drain(..excess);
        }
        self.take_status()
    }

    fn save_forwarded(&mut self) {
        let Some(path) = &self.forwarded_path else {
            return;
        };
        if let Some(sent) = self.engine.take_forwarded_changes()
            && let Err(e) = forward::save_forwarded(path, &sent)
        {
            log::warn!("whip: cannot save {}: {e}", path.display());
        }
    }

    /// Queues the log entries `execute` just appended for `actions`, one per action.
    fn record(&mut self, actions: &[WhipAction]) {
        let log = self.engine.log();
        let n = actions.len().min(log.len());
        let first_seq = self.actions_logged + (actions.len() - n) as u64 + 1;
        let new = log.iter().skip(log.len() - n);
        let items =
            new.zip(&actions[actions.len() - n..])
                .enumerate()
                .map(|(i, (entry, action))| {
                    WhipLogItem::new(first_seq + i as u64, entry.clone(), action)
                });
        self.pending.extend(items);
        let excess = self.pending.len().saturating_sub(PENDING_LOG_LEN);
        self.pending.drain(..excess);
        self.actions_logged += actions.len() as u64;
    }

    /// The current status, carrying the log entries not handed out before.
    pub fn take_status(&mut self) -> WhipStatus {
        WhipStatus {
            new_log: std::mem::take(&mut self.pending),
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
    /// Send these nudges now, live.
    Manual(Vec<ManualNudge>, Sender<WhipNowReport>),
    /// Tick now instead of at the next period, with the normal rules.
    TickNow(Sender<WhipNowReport>),
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
    /// `dry_run` off or event forwarding to do.
    pub fn spawn(settings: WhipSettings, live: ExecutorFactory, repaint: Option<Repaint>) -> Self {
        Self::spawn_with_store(settings, live, repaint, None)
    }

    /// [`WhipRunner::spawn`], keeping the forwarded set in `forwarded_path` across restarts.
    pub fn spawn_with_store(
        settings: WhipSettings,
        live: ExecutorFactory,
        repaint: Option<Repaint>,
        forwarded_path: Option<PathBuf>,
    ) -> Self {
        let (commands, command_rx) = unbounded();
        let (status_tx, statuses) = unbounded();
        let thread = thread::Builder::new()
            .name("whip".into())
            .spawn(move || {
                let mut whip = WhipLoop::new(settings);
                if let Some(path) = forwarded_path {
                    whip = whip.with_forwarded_store(path);
                }
                run(whip, &command_rx, &status_tx, &live, repaint)
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

    /// Sends `nudges` now on the whip thread, live even in dry-run. The report arrives on the
    /// returned channel (and the entries in the next status, like any other).
    pub fn whip_now(&self, nudges: Vec<ManualNudge>) -> Receiver<WhipNowReport> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = self.commands.send(Command::Manual(nudges, tx));
        rx
    }

    /// Runs a full tick now instead of waiting for the period: threshold, cooldown and dry-run
    /// apply as usual. The next periodic tick is one period after this one.
    pub fn tick_now(&self) -> Receiver<WhipNowReport> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = self.commands.send(Command::TickNow(tx));
        rx
    }

    /// The newest status published since the last call, if any, carrying the log entries of
    /// every status published since then.
    pub fn latest(&self) -> Option<WhipStatus> {
        merge_statuses(self.statuses.try_iter())
    }
}

impl Drop for WhipRunner {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        // Not joined: a live tick can take several orca call timeouts and must not block quitting.
        drop(self.thread.take());
    }
}

/// The last of `statuses` with the log entries of all of them, in order.
fn merge_statuses(statuses: impl Iterator<Item = WhipStatus>) -> Option<WhipStatus> {
    statuses.reduce(|mut older, mut newer| {
        older.new_log.append(&mut newer.new_log);
        newer.new_log = older.new_log;
        newer
    })
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
                publish(whip.take_status());
            }
            Some(Command::Manual(nudges, reply)) => {
                let status = whip.manual(&nudges, now_ms(), &|| live());
                let _ = reply.send(report(true, &status));
                publish(status);
            }
            Some(Command::TickNow(reply)) => {
                let now = Instant::now();
                let status = whip.tick(now_ms(), &|| live());
                let _ = reply.send(status.as_ref().map_or_else(
                    || WhipNowReport {
                        dry_run: whip.settings().dry_run,
                        ..WhipNowReport::default()
                    },
                    |s| report(true, s),
                ));
                if let Some(status) = status {
                    publish(status);
                    last_tick = Some(now);
                    deadline = Some(now + tick_every(whip.settings()));
                }
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

fn report(ran: bool, status: &WhipStatus) -> WhipNowReport {
    WhipNowReport {
        ran,
        items: status.new_log.clone(),
        dry_run: status.dry_run,
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
            data: ClanData {
                polled_at_ms: now_ms,
                terminals: vec![Terminal {
                    handle: HANDLE.into(),
                    last_output_at: Some(Timestamp::Millis(now_ms - 60 * MIN)),
                    ..Default::default()
                }],
                ..Default::default()
            },
            needs_input: BTreeSet::from([HANDLE.to_string()]),
            parked: BTreeSet::new(),
        }
    }

    #[test]
    fn loop_does_nothing_before_input() {
        let rec = Recorder::default();
        let factory = rec.factory();
        let mut whip = WhipLoop::new(WhipSettings::default());
        assert_eq!(whip.tick(1_000, &|| factory()), None);
        assert_eq!(whip.take_status().last_tick_ms, None);
    }

    /// A Run whose live Claude coordinator has one fresh `worker_done` to hear about.
    fn report_input(now_ms: i64) -> WhipInput {
        use serde_json::json;
        let data = ClanData {
            polled_at_ms: now_ms,
            terminals: vec![Terminal {
                handle: "term_coord".into(),
                agent_identity: Some(crate::orca::AgentIdentity::Claude),
                preview: Some("⏺ Dispatched.\nnew task? /clear to save 120k".into()),
                last_output_at: Some(Timestamp::Millis(now_ms)),
                ..Default::default()
            }],
            runs: vec![
                serde_json::from_value(json!({
                    "id": "run_1", "coordinator_handle": "term_coord",
                    "created_at": now_ms - 60 * MIN, "updated_at": now_ms,
                }))
                .unwrap(),
            ],
            inbox: vec![
                serde_json::from_value(json!({
                    "id": "done1", "run_id": "run_1", "from_handle": "term_worker",
                    "to_handle": "run:run_1", "type": "worker_done", "subject": "PR open",
                    "created_at": now_ms - MIN,
                }))
                .unwrap(),
            ],
            ..Default::default()
        };
        WhipInput {
            data,
            needs_input: BTreeSet::new(),
            parked: BTreeSet::new(),
        }
    }

    #[test]
    fn dry_run_loop_forwards_live_and_persists_the_forwarded_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(forward::FORWARDED_FILE);
        let rec = Recorder::default();
        let factory = rec.factory();
        let now = 1_790_000_000_000;
        let mut whip = WhipLoop::new(WhipSettings::default()).with_forwarded_store(path.clone());
        whip.set_input(report_input(now));
        let status = whip.tick(now, &|| factory()).unwrap();
        assert!(status.dry_run);
        assert_eq!(rec.built(), 1);
        assert_eq!(rec.calls(), vec!["terminal term_coord".to_string()]);
        assert!(forward::load_forwarded(&path).is_some_and(|s| s.contains_key("done1")));

        // After a restart the saved set keeps the event from being typed again.
        let mut again = WhipLoop::new(WhipSettings::default()).with_forwarded_store(path);
        again.set_input(report_input(now + MIN));
        again.tick(now + MIN, &|| factory()).unwrap();
        assert_eq!(rec.calls().len(), 1);
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
        assert_eq!(status.new_log.len(), 1);
        let item = &status.new_log[0];
        assert_eq!(item.seq, 1);
        assert_eq!(item.handle.as_deref(), Some(HANDLE));
        assert_eq!(item.entry.action, "notify needs-input");
        assert_eq!(item.entry.outcome, "dry-run");
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
        assert_eq!(status.new_log[0].entry.outcome, "sent");
        assert_eq!(rec.built(), 1);
        assert_eq!(
            rec.calls(),
            vec![format!("notify Stronghold: {HANDLE} is waiting on you")]
        );
        // Nothing to do on the next tick: the executor is not even built.
        let next = whip.tick(now + MIN, &|| factory()).unwrap();
        assert!(next.new_log.is_empty());
        assert_eq!(rec.built(), 1);
    }

    fn needs_input(i: usize) -> WhipAction {
        WhipAction::NotifyNeedsInput {
            handle: format!("term_{i}"),
            since_ms: 0,
        }
    }

    /// Runs `actions` through the engine and records them, as a tick does.
    fn execute(whip: &mut WhipLoop, actions: &[WhipAction]) {
        let exec = DryRunExecutor::new();
        whip.engine
            .execute(actions, &exec, &whip.settings.clone(), 1_790_000_000_000);
        whip.record(actions);
    }

    #[test]
    fn statuses_carry_each_entry_once_with_increasing_seq() {
        let mut whip = WhipLoop::new(WhipSettings::default());
        execute(&mut whip, &[needs_input(0), needs_input(1)]);
        let first = whip.take_status();
        execute(&mut whip, &[needs_input(2)]);
        let second = whip.take_status();
        let third = whip.take_status();
        let seqs = |s: &WhipStatus| s.new_log.iter().map(|i| i.seq).collect::<Vec<_>>();
        assert_eq!(seqs(&first), vec![1, 2]);
        assert_eq!(seqs(&second), vec![3]);
        assert!(third.new_log.is_empty());
        assert_eq!(second.new_log[0].entry.target, "terminal term_2");
        assert_eq!(second.actions_logged, 3);
    }

    #[test]
    fn pending_log_is_bounded_and_keeps_the_newest() {
        let mut whip = WhipLoop::new(WhipSettings::default());
        for i in 0..(PENDING_LOG_LEN + 5) {
            execute(&mut whip, &[needs_input(i)]);
        }
        let status = whip.take_status();
        assert_eq!(status.new_log.len(), PENDING_LOG_LEN);
        assert_eq!(status.new_log[0].seq, 6);
        let last = status.new_log.last().unwrap();
        assert_eq!(last.seq, PENDING_LOG_LEN as u64 + 5);
        assert_eq!(
            last.entry.target,
            format!("terminal term_{}", PENDING_LOG_LEN + 4)
        );
    }

    #[test]
    fn merged_statuses_keep_every_entry_and_the_newest_fields() {
        let item = |seq| WhipLogItem {
            seq,
            entry: WhipLogEntry {
                at_ms: 0,
                action: String::new(),
                target: String::new(),
                reason: String::new(),
                dry_run: true,
                outcome: String::new(),
                manual: false,
            },
            handle: None,
            run_id: None,
            task_id: None,
        };
        let status = |seqs: &[u64], findings| WhipStatus {
            new_log: seqs.iter().map(|&s| item(s)).collect(),
            findings,
            ..WhipStatus::default()
        };
        let merged =
            merge_statuses([status(&[1, 2], 1), status(&[], 2), status(&[3], 3)].into_iter())
                .unwrap();
        assert_eq!(merged.findings, 3);
        let seqs: Vec<u64> = merged.new_log.iter().map(|i| i.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        assert_eq!(merge_statuses(std::iter::empty()), None);
    }

    #[test]
    fn items_carry_structured_targets() {
        use crate::whip::{NudgeVia, StallReason};
        let entry = WhipLogEntry {
            at_ms: 0,
            action: String::new(),
            target: String::new(),
            reason: String::new(),
            dry_run: true,
            outcome: String::new(),
            manual: false,
        };
        let worker = StallTarget::Worker {
            handle: "term_w".into(),
            task_id: "t1".into(),
            run_id: "r1".into(),
        };
        let coordinator = StallTarget::Coordinator {
            handle: "term_c".into(),
            run_id: "r2".into(),
        };
        let escalate = WhipAction::Escalate {
            target: worker.clone(),
            reason: StallReason::WorkerNoProgress,
            evidence: String::new(),
        };
        let item = WhipLogItem::new(1, entry.clone(), &escalate);
        assert_eq!(item.handle.as_deref(), Some("term_w"));
        assert_eq!(item.run_id.as_deref(), Some("r1"));
        assert_eq!(item.task_id.as_deref(), Some("t1"));

        let nudge = |targets: Vec<StallTarget>| WhipAction::Nudge {
            handle: "term_c".into(),
            targets,
            attempt: 1,
            via: NudgeVia::Terminal {
                handle: "term_c".into(),
            },
            text: String::new(),
        };
        let item = WhipLogItem::new(2, entry.clone(), &nudge(vec![coordinator.clone()]));
        assert_eq!(item.run_id.as_deref(), Some("r2"));
        assert_eq!(item.task_id, None);
        // Several folded targets: only the terminal is certain.
        let item = WhipLogItem::new(3, entry, &nudge(vec![coordinator, worker]));
        assert_eq!(item.handle.as_deref(), Some("term_c"));
        assert_eq!((item.run_id, item.task_id), (None, None));
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
        assert_eq!(status.new_log[0].entry.outcome, "dry-run");

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

    const RUN: &str = "run_1";
    const WORKER: &str = "term_worker";

    /// A worker on dispatched task `t1` silent for 20 minutes before `now_ms`: stalled.
    fn stalled_worker(now_ms: i64) -> WhipInput {
        use serde_json::json;
        let t0 = now_ms - 20 * MIN;
        let data = ClanData {
            polled_at_ms: now_ms,
            terminals: vec![Terminal {
                handle: WORKER.into(),
                last_output_at: Some(Timestamp::Millis(t0)),
                ..Default::default()
            }],
            workers: vec![
                serde_json::from_value(json!({
                    "dispatchId": "ctx_t1", "taskId": "t1", "runId": RUN,
                    "dispatchStatus": "dispatched", "agentTerminalHandle": WORKER,
                }))
                .unwrap(),
            ],
            tasks: [(
                RUN.to_string(),
                vec![
                    serde_json::from_value(json!({
                        "id": "t1", "run_id": RUN, "status": "dispatched",
                        "created_at": t0 - 60 * MIN,
                    }))
                    .unwrap(),
                ],
            )]
            .into(),
            ..Default::default()
        };
        WhipInput {
            data,
            needs_input: BTreeSet::new(),
            parked: BTreeSet::new(),
        }
    }

    fn manual_nudge() -> ManualNudge {
        ManualNudge {
            node: crate::model::NodeId::terminal(WORKER),
            label: WORKER.into(),
            handle: WORKER.into(),
            targets: vec![StallTarget::Worker {
                handle: WORKER.into(),
                task_id: "t1".into(),
                run_id: RUN.into(),
            }],
            via: crate::whip::NudgeVia::Bus {
                to: "dispatch:ctx_t1".into(),
                run_id: Some(RUN.into()),
            },
            text: "Stronghold: now".into(),
            warn: None,
        }
    }

    #[test]
    fn loop_manual_is_live_even_in_dry_run() {
        let rec = Recorder::default();
        let factory = rec.factory();
        let mut whip = WhipLoop::new(WhipSettings::default());
        assert!(whip.settings().dry_run);
        let status = whip.manual(&[manual_nudge()], 1_790_000_000_000, &|| factory());
        assert_eq!(rec.built(), 1);
        assert_eq!(rec.calls(), vec![r#"bus Some("dispatch:ctx_t1")"#]);
        assert!(status.dry_run, "the automatic whip stays in dry-run");
        let item = &status.new_log[0];
        assert_eq!(item.seq, 1);
        assert!(item.entry.manual);
        assert!(!item.entry.dry_run);
        assert_eq!(item.entry.outcome, "sent");
        assert_eq!(item.handle.as_deref(), Some(WORKER));
        assert_eq!(
            (item.run_id.as_deref(), item.task_id.as_deref()),
            (Some(RUN), Some("t1"))
        );
        // Nothing to send: the executor is not built.
        whip.manual(&[], 1_790_000_000_000, &|| factory());
        assert_eq!(rec.built(), 1);
    }

    #[test]
    fn runner_whip_now_reports_and_publishes() {
        let rec = Recorder::default();
        let runner = WhipRunner::spawn(WhipSettings::default(), rec.factory(), None);
        let report = runner
            .whip_now(vec![manual_nudge()])
            .recv_timeout(Duration::from_secs(2))
            .expect("a report");
        assert!(report.ran && report.dry_run);
        assert_eq!(report.items.len(), 1);
        assert!(report.items[0].entry.manual);
        let status = wait_for_status(&runner, Duration::from_secs(2)).expect("a status");
        assert_eq!(status.new_log, report.items);
        assert_eq!(rec.built(), 1);
    }

    #[test]
    fn runner_tick_now_ticks_at_once_and_respects_the_cooldown() {
        let rec = Recorder::default();
        let settings = WhipSettings {
            tick_ms: 60 * MIN,
            ..WhipSettings::default()
        };
        let runner = WhipRunner::spawn(settings, rec.factory(), None);
        let before = runner
            .tick_now()
            .recv_timeout(Duration::from_secs(2))
            .expect("a report");
        assert!(!before.ran, "no data yet");

        // The first input ticks at once: nudge #1 (dry-run).
        runner.send_input(stalled_worker(now_ms()));
        let first = wait_for_status(&runner, Duration::from_secs(2)).expect("a tick");
        assert_eq!(first.new_log.len(), 1);
        assert_eq!(
            first.new_log[0].entry.action,
            "nudge #1 via bus to dispatch:ctx_t1"
        );

        // An immediate tick, long before the hourly period: still stalled, but in cooldown.
        let report = runner
            .tick_now()
            .recv_timeout(Duration::from_secs(2))
            .expect("a report");
        assert!(report.ran && report.dry_run);
        assert!(report.items.is_empty());
        let status = wait_for_status(&runner, Duration::from_secs(2)).expect("a status");
        assert!(status.last_tick_ms >= first.last_tick_ms);
        assert_eq!(status.findings, 1);
        assert_eq!(
            rec.built(),
            0,
            "dry-run ticks never build the live executor"
        );
    }

    #[test]
    fn parked_agents_are_neither_nudged_nor_notified_about() {
        let rec = Recorder::default();
        let factory = rec.factory();
        let now = now_ms();
        let mut whip = WhipLoop::new(WhipSettings::default());
        let mut stalled = stalled_worker(now);
        stalled.parked = BTreeSet::from([WORKER.to_string()]);
        whip.set_input(stalled);
        let status = whip.tick(now, &|| factory()).unwrap();
        assert!(status.new_log.is_empty(), "{:?}", status.new_log);
        assert_eq!(status.findings, 0);

        let mut waiting = waiting_input(now);
        waiting.parked = BTreeSet::from([HANDLE.to_string()]);
        whip.set_input(waiting);
        let status = whip.tick(now + MIN, &|| factory()).unwrap();
        assert!(status.new_log.is_empty(), "{:?}", status.new_log);
    }
}

//! Background thread that polls Orca and sends snapshots to the UI over a channel.
//!
//! Each call type runs on its own cadence ([`PollerConfig`]). Every cycle that polled anything
//! sends a complete [`FleetData`] holding the latest value of everything; a failed call keeps
//! its previous value and reports a message in `errors` until the call next succeeds.
//!
//! The screen of one watched terminal ([`PollerCommand::WatchScreen`]) is read on its own
//! thread and cadence, so slow fleet calls never delay it; results arrive as [`ScreenUpdate`]s
//! on a separate channel ([`Poller::latest_screen`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};

use crate::detect::{PromptKind, detect_prompt};
use crate::fleet::FleetData;
use crate::orca::{
    AgentIdentity, InboxMessage, OrcaClient, OrcaError, Run, Task, Terminal, TerminalRead,
    TerminalStatus, Worker,
};

/// How long after its last update a run stays active without any live terminal.
pub const ACTIVE_RUN_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// Snapshots buffered for the UI; when full, a cycle's snapshot is dropped (the next one is
/// complete anyway).
const SNAPSHOT_BUFFER: usize = 4;

/// Screen updates buffered for the UI; when full, the backlog is discarded in favour of the
/// newest update.
const SCREEN_BUFFER: usize = 4;

/// Poll intervals per call type.
#[derive(Debug, Clone, PartialEq)]
pub struct PollerConfig {
    pub terminals: Duration,
    /// `run-list` and `worker-list`.
    pub runs_workers: Duration,
    /// `task-list` for each active run.
    pub tasks: Duration,
    pub inbox: Duration,
    pub inbox_limit: u64,
    /// `terminal read --screen` of the watched terminal.
    pub screen: Duration,
    /// Background screen sampling of idle agents for NeedsInput ([`PromptSampler`]).
    pub prompt_sample: Duration,
    /// Most screens read per sampling cycle.
    pub prompt_sample_budget: usize,
    /// How long an agent must be silent before its screen is sampled.
    pub prompt_idle_after: Duration,
}

impl Default for PollerConfig {
    fn default() -> Self {
        Self {
            terminals: Duration::from_secs(2),
            runs_workers: Duration::from_secs(10),
            tasks: Duration::from_secs(10),
            inbox: Duration::from_secs(5),
            inbox_limit: 300,
            screen: Duration::from_millis(500),
            prompt_sample: Duration::from_secs(10),
            prompt_sample_budget: 5,
            prompt_idle_after: Duration::from_secs(20),
        }
    }
}

/// Messages from the UI to the poller thread.
#[derive(Debug, Clone, PartialEq)]
pub enum PollerCommand {
    /// Poll everything now, regardless of cadence.
    RefreshNow,
    /// Read this terminal's screen on the screen cadence, starting now; `None` stops reading.
    WatchScreen(Option<String>),
    /// Stop the thread.
    Shutdown,
}

/// The Orca calls the poller makes, abstracted for tests.
pub trait FleetSource: Send + 'static {
    fn terminals(&self) -> Result<Vec<Terminal>, OrcaError>;
    fn runs(&self) -> Result<Vec<Run>, OrcaError>;
    fn workers(&self) -> Result<Vec<Worker>, OrcaError>;
    fn tasks(&self, run_id: &str) -> Result<Vec<Task>, OrcaError>;
    fn inbox(&self, limit: u64) -> Result<Vec<InboxMessage>, OrcaError>;
}

impl FleetSource for OrcaClient {
    fn terminals(&self) -> Result<Vec<Terminal>, OrcaError> {
        self.terminal_list().map(|l| l.terminals)
    }

    fn runs(&self) -> Result<Vec<Run>, OrcaError> {
        self.run_list()
    }

    fn workers(&self) -> Result<Vec<Worker>, OrcaError> {
        self.worker_list(None).map(|l| l.workers)
    }

    fn tasks(&self, run_id: &str) -> Result<Vec<Task>, OrcaError> {
        self.task_list(run_id)
    }

    fn inbox(&self, limit: u64) -> Result<Vec<InboxMessage>, OrcaError> {
        self.inbox(Some(limit), None)
    }
}

/// Reads a terminal's rendered screen, abstracted for tests.
pub trait ScreenSource: Send + 'static {
    fn read_screen(&self, handle: &str) -> Result<TerminalRead, OrcaError>;
}

impl ScreenSource for OrcaClient {
    fn read_screen(&self, handle: &str) -> Result<TerminalRead, OrcaError> {
        self.terminal_read_screen(handle)
    }
}

/// One read of the watched terminal's screen.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenUpdate {
    pub handle: String,
    /// Screen rows. On a failed read, the last good rows for this handle (if any).
    pub lines: Vec<String>,
    pub status: Option<TerminalStatus>,
    /// Wall-clock time of the read, epoch ms.
    pub read_at_ms: i64,
    /// Set when this read failed.
    pub error: Option<String>,
}

/// The screen watcher's state: which handle to read and when. Pure apart from the call into the
/// [`ScreenSource`], so tests can drive it with a fake source and clock.
#[derive(Debug, Clone)]
pub struct ScreenWatch {
    interval: Duration,
    handle: Option<String>,
    next_due: Duration,
    /// Last good read of the current handle, reused when a read fails.
    last_good: Option<(Vec<String>, Option<TerminalStatus>)>,
}

impl ScreenWatch {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            handle: None,
            next_due: Duration::ZERO,
            last_good: None,
        }
    }

    pub fn handle(&self) -> Option<&str> {
        self.handle.as_deref()
    }

    /// Watches `handle` (read due at `now`), or stops watching on `None`.
    pub fn set(&mut self, handle: Option<String>, now: Duration) {
        if handle != self.handle {
            self.last_good = None;
        }
        self.handle = handle;
        self.next_due = now;
    }

    /// When the next read is due; `None` while nothing is watched.
    pub fn next_due(&self) -> Option<Duration> {
        self.handle.as_ref().map(|_| self.next_due)
    }

    /// Reads the screen if a read is due at `now`.
    pub fn tick(
        &mut self,
        source: &dyn ScreenSource,
        now: Duration,
        wall_ms: i64,
    ) -> Option<ScreenUpdate> {
        let handle = self.handle.clone()?;
        if now < self.next_due {
            return None;
        }
        self.next_due = now + self.interval;
        let update = match source.read_screen(&handle) {
            Ok(read) => {
                self.last_good = Some((read.tail.clone(), read.status.clone()));
                ScreenUpdate {
                    handle,
                    lines: read.tail,
                    status: read.status,
                    read_at_ms: wall_ms,
                    error: None,
                }
            }
            Err(err) => {
                let (lines, status) = self.last_good.clone().unwrap_or_default();
                ScreenUpdate {
                    handle,
                    lines,
                    status,
                    read_at_ms: wall_ms,
                    error: Some(describe_error("screen read", &err)),
                }
            }
        };
        Some(update)
    }
}

/// Terminals worth sampling for a prompt: live Claude or Codex terminals with no output for
/// `idle_after` (or none ever), sorted by handle.
pub fn prompt_sample_candidates(
    terminals: &[Terminal],
    now_ms: i64,
    idle_after: Duration,
) -> Vec<&Terminal> {
    let idle_ms = i64::try_from(idle_after.as_millis()).unwrap_or(i64::MAX);
    let mut candidates: Vec<&Terminal> = terminals
        .iter()
        .filter(|t| t.connected && !t.orphaned)
        .filter(|t| {
            matches!(
                t.agent_identity,
                Some(AgentIdentity::Claude | AgentIdentity::Codex)
            )
        })
        .filter(|t| {
            t.last_output_at
                .as_ref()
                .and_then(|ts| ts.epoch_ms())
                .is_none_or(|ms| now_ms.saturating_sub(ms) > idle_ms)
        })
        .collect();
    candidates.sort_by(|a, b| a.handle.cmp(&b.handle));
    candidates
}

/// Up to `budget` of the sorted `candidates` to sample next: never-sampled ones first, then
/// round-robin starting after `cursor` (the last handle sampled).
pub fn pick_prompt_samples(
    candidates: &[&str],
    sampled: &BTreeMap<String, Option<PromptKind>>,
    cursor: Option<&str>,
    budget: usize,
) -> Vec<String> {
    let split = cursor.map_or(0, |c| candidates.partition_point(|h| *h <= c));
    let rotated = candidates[split..].iter().chain(&candidates[..split]);
    let (fresh, seen): (Vec<&str>, Vec<&str>) = rotated.partition(|h| !sampled.contains_key(**h));
    fresh
        .into_iter()
        .chain(seen)
        .take(budget)
        .map(str::to_string)
        .collect()
}

/// Background NeedsInput sampling: every `prompt_sample` interval, reads the screens of up to
/// `prompt_sample_budget` idle agents and records [`detect_prompt`]'s verdict. The UI's own
/// screen watch only covers the selected terminal; this covers the rest.
#[derive(Debug, Clone)]
pub struct PromptSampler {
    interval: Duration,
    budget: usize,
    idle_after: Duration,
    next_due: Duration,
    cursor: Option<String>,
}

impl PromptSampler {
    /// The first sample is due at time zero.
    pub fn new(config: &PollerConfig) -> Self {
        Self {
            interval: config.prompt_sample,
            budget: config.prompt_sample_budget,
            idle_after: config.prompt_idle_after,
            next_due: Duration::ZERO,
            cursor: None,
        }
    }

    pub fn next_due(&self) -> Duration {
        self.next_due
    }

    pub fn is_due(&self, now: Duration) -> bool {
        now >= self.next_due
    }

    /// Drops verdicts of terminals that are no longer candidates (output resumed, gone), then,
    /// if due at `now`, samples the next batch into `verdicts`. A failed read keeps the
    /// terminal's previous verdict.
    pub fn tick(
        &mut self,
        verdicts: &mut BTreeMap<String, Option<PromptKind>>,
        terminals: &[Terminal],
        screens: &dyn ScreenSource,
        now: Duration,
        wall_ms: i64,
    ) {
        let candidates = prompt_sample_candidates(terminals, wall_ms, self.idle_after);
        verdicts.retain(|handle, _| candidates.iter().any(|t| &t.handle == handle));
        if !self.is_due(now) {
            return;
        }
        self.next_due = now + self.interval;
        let handles: Vec<&str> = candidates.iter().map(|t| t.handle.as_str()).collect();
        let picked = pick_prompt_samples(&handles, verdicts, self.cursor.as_deref(), self.budget);
        for handle in &picked {
            let agent = candidates
                .iter()
                .find(|t| &t.handle == handle)
                .and_then(|t| t.agent_identity.as_ref());
            match screens.read_screen(handle) {
                Ok(read) => {
                    verdicts.insert(handle.clone(), detect_prompt(&read.tail, agent));
                }
                Err(err) => log::debug!("{}", describe_error("prompt sample", &err)),
            }
        }
        if let Some(last) = picked.last() {
            self.cursor = Some(last.clone());
        }
    }
}

/// Runs worth fetching tasks for: the coordinator terminal is live, a worker's terminal is live,
/// or the run was updated within `window` of `now_ms`.
pub fn active_run_ids(
    runs: &[Run],
    workers: &[Worker],
    terminals: &[Terminal],
    now_ms: i64,
    window: Duration,
) -> BTreeSet<String> {
    let live: BTreeSet<&str> = terminals.iter().map(|t| t.handle.as_str()).collect();
    let is_live = |handle: Option<&str>| handle.is_some_and(|h| live.contains(h));
    let runs_with_live_worker: BTreeSet<&str> = workers
        .iter()
        .filter(|w| {
            is_live(w.agent_terminal_handle.as_deref())
                || is_live(
                    w.resource
                        .as_ref()
                        .and_then(|r| r.terminal_handle.as_deref()),
                )
        })
        .filter_map(|w| w.run_id.as_deref())
        .collect();
    let cutoff = now_ms.saturating_sub(i64::try_from(window.as_millis()).unwrap_or(i64::MAX));
    runs.iter()
        .filter(|run| {
            is_live(run.coordinator_handle.as_deref())
                || runs_with_live_worker.contains(run.id.as_str())
                || run
                    .updated_at
                    .as_ref()
                    .and_then(|t| t.epoch_ms())
                    .is_some_and(|ms| ms >= cutoff)
        })
        .map(|run| run.id.clone())
        .collect()
}

/// A readable one-line description of a failed call.
pub fn describe_error(call: &str, err: &OrcaError) -> String {
    match err {
        OrcaError::Spawn { .. } => format!("{call}: orca CLI not found ({err})"),
        OrcaError::Timeout { after, .. } => format!("{call}: Orca timed out after {after:?}"),
        OrcaError::Exit { stderr, .. } => {
            let detail = stderr.trim();
            if detail.is_empty() {
                format!("{call}: Orca runtime not reachable")
            } else {
                format!("{call}: Orca runtime not reachable ({detail})")
            }
        }
        OrcaError::Api(api) => format!("{call}: {} ({})", api.message, api.code),
        OrcaError::Json { .. } | OrcaError::Io(_) => format!("{call}: {err}"),
    }
}

/// Which call groups are due in a cycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Due {
    pub terminals: bool,
    pub runs_workers: bool,
    pub tasks: bool,
    pub inbox: bool,
}

impl Due {
    pub const ALL: Due = Due {
        terminals: true,
        runs_workers: true,
        tasks: true,
        inbox: true,
    };

    pub fn any(&self) -> bool {
        self.terminals || self.runs_workers || self.tasks || self.inbox
    }
}

/// Next-due times per call group, on a monotonic clock measured from poller start.
#[derive(Debug, Clone)]
pub struct Schedule {
    config: PollerConfig,
    next_terminals: Duration,
    next_runs_workers: Duration,
    next_tasks: Duration,
    next_inbox: Duration,
}

impl Schedule {
    /// Everything is due at time zero.
    pub fn new(config: PollerConfig) -> Self {
        Self {
            config,
            next_terminals: Duration::ZERO,
            next_runs_workers: Duration::ZERO,
            next_tasks: Duration::ZERO,
            next_inbox: Duration::ZERO,
        }
    }

    pub fn config(&self) -> &PollerConfig {
        &self.config
    }

    /// Groups due at `now`; each returned group is rescheduled one interval after `now`.
    pub fn take_due(&mut self, now: Duration) -> Due {
        let c = &self.config;
        Due {
            terminals: take(&mut self.next_terminals, now, c.terminals),
            runs_workers: take(&mut self.next_runs_workers, now, c.runs_workers),
            tasks: take(&mut self.next_tasks, now, c.tasks),
            inbox: take(&mut self.next_inbox, now, c.inbox),
        }
    }

    /// Makes every group due immediately.
    pub fn force_all(&mut self) {
        self.next_terminals = Duration::ZERO;
        self.next_runs_workers = Duration::ZERO;
        self.next_tasks = Duration::ZERO;
        self.next_inbox = Duration::ZERO;
    }

    /// The earliest time any group is due.
    pub fn next_due(&self) -> Duration {
        self.next_terminals
            .min(self.next_runs_workers)
            .min(self.next_tasks)
            .min(self.next_inbox)
    }
}

fn take(next: &mut Duration, now: Duration, interval: Duration) -> bool {
    if now >= *next {
        *next = now + interval;
        true
    } else {
        false
    }
}

/// Key for a retained error, so a failure is reported until that call next succeeds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum CallKey {
    Terminals,
    Runs,
    Workers,
    Tasks(String),
    Inbox,
}

/// The poller's merge state: latest values plus outstanding errors. Pure apart from the calls
/// into the [`FleetSource`], so tests can drive it with a fake source and clock.
pub struct PollState {
    schedule: Schedule,
    sampler: PromptSampler,
    data: FleetData,
    errors: BTreeMap<CallKey, String>,
}

impl PollState {
    pub fn new(config: PollerConfig) -> Self {
        Self {
            sampler: PromptSampler::new(&config),
            schedule: Schedule::new(config),
            data: FleetData::default(),
            errors: BTreeMap::new(),
        }
    }

    pub fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    pub fn force_all(&mut self) {
        self.schedule.force_all();
    }

    /// When the next call group or prompt sample is due.
    pub fn next_due(&self) -> Duration {
        self.schedule.next_due().min(self.sampler.next_due())
    }

    /// Polls whatever is due at monotonic time `now`; returns a snapshot if anything was polled.
    pub fn tick(
        &mut self,
        source: &dyn FleetSource,
        now: Duration,
        wall_ms: i64,
    ) -> Option<FleetData> {
        self.poll(source, None, now, wall_ms)
    }

    /// [`Self::tick`] plus background prompt sampling ([`PromptSampler`]) into
    /// [`FleetData::needs_input`].
    pub fn tick_with_screens(
        &mut self,
        source: &dyn FleetSource,
        screens: &dyn ScreenSource,
        now: Duration,
        wall_ms: i64,
    ) -> Option<FleetData> {
        self.poll(source, Some(screens), now, wall_ms)
    }

    fn poll(
        &mut self,
        source: &dyn FleetSource,
        screens: Option<&dyn ScreenSource>,
        now: Duration,
        wall_ms: i64,
    ) -> Option<FleetData> {
        let due = self.schedule.take_due(now);
        let sample_due = screens.is_some() && self.sampler.is_due(now);
        if !due.any() && !sample_due {
            return None;
        }
        if due.terminals {
            let result = source.terminals();
            self.apply(CallKey::Terminals, "terminal list", result, |d, v| {
                d.terminals = v
            });
        }
        if due.runs_workers {
            let result = source.runs();
            self.apply(CallKey::Runs, "run list", result, |d, v| d.runs = v);
            let result = source.workers();
            self.apply(CallKey::Workers, "worker list", result, |d, v| {
                d.workers = v
            });
        }
        if due.tasks {
            self.poll_tasks(source, wall_ms);
        }
        if due.inbox {
            let result = source.inbox(self.schedule.config().inbox_limit);
            self.apply(CallKey::Inbox, "inbox", result, |d, v| d.inbox = v);
        }
        if let Some(screens) = screens {
            let data = &mut self.data;
            self.sampler.tick(
                &mut data.needs_input,
                &data.terminals,
                screens,
                now,
                wall_ms,
            );
        }
        self.data.polled_at_ms = wall_ms;
        self.data.errors = self.errors.values().cloned().collect();
        Some(self.data.clone())
    }

    fn poll_tasks(&mut self, source: &dyn FleetSource, wall_ms: i64) {
        let active = active_run_ids(
            &self.data.runs,
            &self.data.workers,
            &self.data.terminals,
            wall_ms,
            ACTIVE_RUN_WINDOW,
        );
        self.data.tasks.retain(|id, _| active.contains(id));
        self.errors
            .retain(|key, _| !matches!(key, CallKey::Tasks(id) if !active.contains(id)));
        for run_id in active {
            match source.tasks(&run_id) {
                Ok(tasks) => {
                    self.errors.remove(&CallKey::Tasks(run_id.clone()));
                    self.data.tasks.insert(run_id, tasks);
                }
                Err(err) => {
                    let msg = describe_error(&format!("task list for {run_id}"), &err);
                    self.errors.insert(CallKey::Tasks(run_id), msg);
                }
            }
        }
    }

    fn apply<T>(
        &mut self,
        key: CallKey,
        call: &str,
        result: Result<T, OrcaError>,
        store: impl FnOnce(&mut FleetData, T),
    ) {
        match result {
            Ok(value) => {
                store(&mut self.data, value);
                self.errors.remove(&key);
            }
            Err(err) => {
                self.errors.insert(key, describe_error(call, &err));
            }
        }
    }
}

/// Callback that wakes the UI after a snapshot or screen update is sent.
pub type Repaint = Box<dyn Fn() + Send + Sync>;

/// Handle to the poller threads (fleet and screen). Dropping it shuts both down and joins them.
pub struct Poller {
    snapshots: Receiver<FleetData>,
    screens: Receiver<ScreenUpdate>,
    commands: Sender<PollerCommand>,
    screen_commands: Sender<PollerCommand>,
    threads: Vec<JoinHandle<()>>,
}

impl Poller {
    pub fn spawn(
        source: impl FleetSource + ScreenSource + Clone,
        config: PollerConfig,
        repaint: Option<Repaint>,
    ) -> Poller {
        let repaint = repaint.map(Arc::new);
        let (snap_tx, snap_rx) = crossbeam_channel::bounded(SNAPSHOT_BUFFER);
        let (screen_tx, screen_rx) = crossbeam_channel::bounded(SCREEN_BUFFER);
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (screen_cmd_tx, screen_cmd_rx) = crossbeam_channel::unbounded();

        let screen_source = source.clone();
        let screen_interval = config.screen;
        let screen_repaint = repaint.clone();
        let screen_backlog = screen_rx.clone();
        let screen_thread = std::thread::Builder::new()
            .name("orca-screen".into())
            .spawn(move || {
                screen_loop(
                    &screen_source,
                    screen_interval,
                    screen_repaint.as_deref(),
                    &screen_tx,
                    &screen_backlog,
                    &screen_cmd_rx,
                )
            })
            .expect("spawn screen poller thread");
        let fleet_thread = std::thread::Builder::new()
            .name("orca-poller".into())
            .spawn(move || {
                run_loop(
                    &source,
                    &source,
                    config,
                    repaint.as_deref(),
                    &snap_tx,
                    &cmd_rx,
                )
            })
            .expect("spawn poller thread");
        Poller {
            snapshots: snap_rx,
            screens: screen_rx,
            commands: cmd_tx,
            screen_commands: screen_cmd_tx,
            threads: vec![fleet_thread, screen_thread],
        }
    }

    /// Snapshots from the poller, oldest first.
    pub fn rx(&self) -> &Receiver<FleetData> {
        &self.snapshots
    }

    /// The newest pending snapshot, discarding older ones; `None` if nothing new arrived.
    pub fn latest(&self) -> Option<FleetData> {
        self.snapshots.try_iter().last()
    }

    /// The newest pending screen update, discarding older ones; `None` if nothing new arrived.
    pub fn latest_screen(&self) -> Option<ScreenUpdate> {
        self.screens.try_iter().last()
    }

    pub fn send(&self, cmd: PollerCommand) {
        // The threads only exit on Shutdown or a dropped handle, so a failed send is moot.
        match cmd {
            PollerCommand::WatchScreen(_) => {
                let _ = self.screen_commands.send(cmd);
            }
            PollerCommand::RefreshNow => {
                let _ = self.commands.send(cmd);
            }
            PollerCommand::Shutdown => {
                let _ = self.screen_commands.send(PollerCommand::Shutdown);
                let _ = self.commands.send(PollerCommand::Shutdown);
            }
        }
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.send(PollerCommand::Shutdown);
        for thread in self.threads.drain(..) {
            if thread.join().is_err() {
                log::error!("poller thread panicked");
            }
        }
    }
}

fn screen_loop(
    source: &dyn ScreenSource,
    interval: Duration,
    repaint: Option<&Repaint>,
    updates: &Sender<ScreenUpdate>,
    backlog: &Receiver<ScreenUpdate>,
    commands: &Receiver<PollerCommand>,
) {
    let start = Instant::now();
    let mut watch = ScreenWatch::new(interval);
    loop {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            watch.tick(source, start.elapsed(), wall_clock_ms())
        }));
        match result {
            Ok(Some(update)) => {
                let mut pending = Some(update);
                while let Some(update) = pending.take() {
                    match updates.try_send(update) {
                        Ok(()) => {
                            if let Some(repaint) = repaint {
                                repaint();
                            }
                        }
                        // The UI is behind: drop the stale backlog, keep the newest.
                        Err(TrySendError::Full(update)) => {
                            while backlog.try_recv().is_ok() {}
                            pending = Some(update);
                        }
                        Err(TrySendError::Disconnected(_)) => return,
                    }
                }
            }
            Ok(None) => {}
            Err(_) => log::error!("screen read panicked; continuing"),
        }
        let command = match watch.next_due() {
            Some(due) => commands.recv_timeout(due.saturating_sub(start.elapsed())),
            None => commands.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match command {
            Ok(PollerCommand::WatchScreen(handle)) => watch.set(handle, start.elapsed()),
            Ok(PollerCommand::RefreshNow) | Err(RecvTimeoutError::Timeout) => {}
            Ok(PollerCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn run_loop(
    source: &dyn FleetSource,
    screens: &dyn ScreenSource,
    config: PollerConfig,
    repaint: Option<&Repaint>,
    snapshots: &Sender<FleetData>,
    commands: &Receiver<PollerCommand>,
) {
    let start = Instant::now();
    let mut state = PollState::new(config);
    loop {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.tick_with_screens(source, screens, start.elapsed(), wall_clock_ms())
        }));
        match result {
            Ok(Some(snapshot)) => match snapshots.try_send(snapshot) {
                Ok(()) => {
                    if let Some(repaint) = repaint {
                        repaint();
                    }
                }
                Err(TrySendError::Full(_)) => log::debug!("UI behind; dropped a fleet snapshot"),
                Err(TrySendError::Disconnected(_)) => return,
            },
            Ok(None) => {}
            Err(_) => log::error!("poll cycle panicked; continuing"),
        }
        let wait = state.next_due().saturating_sub(start.elapsed());
        match commands.recv_timeout(wait) {
            Ok(PollerCommand::RefreshNow) => state.force_all(),
            // Routed to the screen thread by `Poller::send`.
            Ok(PollerCommand::WatchScreen(_)) => {}
            Ok(PollerCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn wall_clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::orca::{ApiError, Timestamp, WorkerResource};

    const NOW_MS: i64 = 1_790_000_000_000;
    const HOUR_MS: i64 = 60 * 60 * 1000;

    fn terminal(handle: &str) -> Terminal {
        Terminal {
            handle: handle.into(),
            connected: true,
            ..Default::default()
        }
    }

    fn run(id: &str, coordinator: Option<&str>, updated_ms: Option<i64>) -> Run {
        Run {
            id: id.into(),
            coordinator_handle: coordinator.map(Into::into),
            updated_at: updated_ms.map(Timestamp::Millis),
            ..Default::default()
        }
    }

    fn worker(run_id: &str, handle: Option<&str>, resource_handle: Option<&str>) -> Worker {
        Worker {
            dispatch_id: format!("d-{run_id}"),
            task_id: None,
            run_id: Some(run_id.into()),
            worker_state: None,
            dispatch_status: None,
            agent_terminal_handle: handle.map(Into::into),
            terminal_state: None,
            resource: resource_handle.map(|h| WorkerResource {
                terminal_handle: Some(h.into()),
                ..Default::default()
            }),
        }
    }

    fn api_error(msg: &str) -> OrcaError {
        OrcaError::Api(ApiError {
            code: "runtime_unavailable".into(),
            message: msg.into(),
            data: serde_json::Value::Null,
        })
    }

    #[derive(Default)]
    struct Calls {
        terminals: usize,
        runs: usize,
        workers: usize,
        tasks: Vec<String>,
        inbox: usize,
        screens: Vec<String>,
    }

    /// Scripted source: returns the configured data, failing calls whose flag is set.
    #[derive(Default, Clone)]
    struct Fake {
        inner: Arc<Mutex<FakeInner>>,
    }

    #[derive(Default)]
    struct FakeInner {
        terminals: Vec<Terminal>,
        runs: Vec<Run>,
        workers: Vec<Worker>,
        fail_terminals: bool,
        fail_runs: bool,
        fail_tasks_for: Option<String>,
        fail_screen: bool,
        /// Screen rows per handle; others get a generic line.
        screen_rows: BTreeMap<String, Vec<String>>,
        calls: Calls,
    }

    impl Fake {
        fn with<R>(&self, f: impl FnOnce(&mut FakeInner) -> R) -> R {
            f(&mut self.inner.lock().unwrap())
        }
    }

    impl FleetSource for Fake {
        fn terminals(&self) -> Result<Vec<Terminal>, OrcaError> {
            self.with(|s| {
                s.calls.terminals += 1;
                if s.fail_terminals {
                    Err(api_error("Orca runtime not reachable"))
                } else {
                    Ok(s.terminals.clone())
                }
            })
        }

        fn runs(&self) -> Result<Vec<Run>, OrcaError> {
            self.with(|s| {
                s.calls.runs += 1;
                if s.fail_runs {
                    Err(OrcaError::Exit {
                        status: Some(1),
                        stderr: "connect ECONNREFUSED".into(),
                        stdout: String::new(),
                    })
                } else {
                    Ok(s.runs.clone())
                }
            })
        }

        fn workers(&self) -> Result<Vec<Worker>, OrcaError> {
            self.with(|s| {
                s.calls.workers += 1;
                Ok(s.workers.clone())
            })
        }

        fn tasks(&self, run_id: &str) -> Result<Vec<Task>, OrcaError> {
            self.with(|s| {
                s.calls.tasks.push(run_id.into());
                if s.fail_tasks_for.as_deref() == Some(run_id) {
                    Err(api_error("run gone"))
                } else {
                    Ok(Vec::new())
                }
            })
        }

        fn inbox(&self, _limit: u64) -> Result<Vec<InboxMessage>, OrcaError> {
            self.with(|s| {
                s.calls.inbox += 1;
                Ok(Vec::new())
            })
        }
    }

    impl ScreenSource for Fake {
        fn read_screen(&self, handle: &str) -> Result<TerminalRead, OrcaError> {
            self.with(|s| {
                s.calls.screens.push(handle.into());
                if s.fail_screen {
                    return Err(api_error("terminal gone"));
                }
                Ok(TerminalRead {
                    handle: handle.into(),
                    status: Some(TerminalStatus::Running),
                    tail: s.screen_rows.get(handle).cloned().unwrap_or_else(|| {
                        vec![format!("{handle} line {}", s.calls.screens.len())]
                    }),
                    truncated: false,
                    limited: false,
                    oldest_cursor: None,
                    next_cursor: None,
                    latest_cursor: None,
                    returned_line_count: None,
                    source: None,
                    draft: None,
                })
            })
        }
    }

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn default_config_matches_spec() {
        let c = PollerConfig::default();
        assert_eq!(c.terminals, secs(2));
        assert_eq!(c.runs_workers, secs(10));
        assert_eq!(c.tasks, secs(10));
        assert_eq!(c.inbox, secs(5));
        assert_eq!(c.inbox_limit, 300);
        assert_eq!(c.screen, ms(500));
    }

    #[test]
    fn schedule_follows_cadence() {
        let mut s = Schedule::new(PollerConfig::default());
        assert_eq!(s.take_due(secs(0)), Due::ALL);
        assert!(!s.take_due(secs(1)).any());
        assert_eq!(s.next_due(), secs(2));
        let d = s.take_due(secs(2));
        assert!(d.terminals && !d.inbox && !d.runs_workers && !d.tasks);
        let d = s.take_due(secs(5));
        assert!(d.terminals && d.inbox && !d.runs_workers);
        let d = s.take_due(secs(10));
        assert_eq!(d, Due::ALL);
    }

    #[test]
    fn schedule_force_all_makes_everything_due() {
        let mut s = Schedule::new(PollerConfig::default());
        s.take_due(secs(0));
        s.force_all();
        assert_eq!(s.next_due(), Duration::ZERO);
        assert_eq!(s.take_due(secs(1)), Due::ALL);
        // Rescheduled relative to the forced poll.
        assert_eq!(s.next_due(), secs(3));
    }

    #[test]
    fn call_counts_over_twenty_seconds() {
        let fake = Fake::default();
        fake.with(|s| s.runs = vec![run("r1", None, Some(NOW_MS))]);
        let mut state = PollState::new(PollerConfig::default());
        let mut snapshots = 0;
        for ms in (0..20_000).step_by(100) {
            if state
                .tick(&fake, Duration::from_millis(ms), NOW_MS)
                .is_some()
            {
                snapshots += 1;
            }
        }
        fake.with(|s| {
            assert_eq!(s.calls.terminals, 10);
            assert_eq!(s.calls.runs, 2);
            assert_eq!(s.calls.workers, 2);
            assert_eq!(s.calls.tasks, vec!["r1", "r1"]);
            assert_eq!(s.calls.inbox, 4);
        });
        assert_eq!(snapshots, 12);
    }

    /// Ticks `watch` every 100 ms over `[from, to)`, returning the updates.
    fn run_watch(watch: &mut ScreenWatch, fake: &Fake, from: u64, to: u64) -> Vec<ScreenUpdate> {
        (from..to)
            .step_by(100)
            .filter_map(|t| watch.tick(fake, ms(t), NOW_MS + t as i64))
            .collect()
    }

    #[test]
    fn screen_watch_idle_until_a_handle_is_set() {
        let fake = Fake::default();
        let mut watch = ScreenWatch::new(ms(500));
        assert_eq!(watch.next_due(), None);
        assert!(run_watch(&mut watch, &fake, 0, 2000).is_empty());
        fake.with(|s| assert!(s.calls.screens.is_empty()));
    }

    #[test]
    fn screen_watch_reads_on_cadence() {
        let fake = Fake::default();
        let mut watch = ScreenWatch::new(ms(500));
        watch.set(Some("t1".into()), ms(0));
        let updates = run_watch(&mut watch, &fake, 0, 2000);
        let times: Vec<i64> = updates.iter().map(|u| u.read_at_ms - NOW_MS).collect();
        assert_eq!(times, vec![0, 500, 1000, 1500]);
        assert!(
            updates
                .iter()
                .all(|u| u.handle == "t1" && u.error.is_none())
        );
        assert_eq!(updates[0].lines, vec!["t1 line 1"]);
        assert_eq!(updates[0].status, Some(TerminalStatus::Running));
    }

    #[test]
    fn screen_watch_switches_handle_immediately() {
        let fake = Fake::default();
        let mut watch = ScreenWatch::new(ms(500));
        watch.set(Some("t1".into()), ms(0));
        run_watch(&mut watch, &fake, 0, 700);
        watch.set(Some("t2".into()), ms(700));
        assert_eq!(watch.next_due(), Some(ms(700)));
        let updates = run_watch(&mut watch, &fake, 700, 1300);
        let handles: Vec<&str> = updates.iter().map(|u| u.handle.as_str()).collect();
        assert_eq!(handles, vec!["t2", "t2"]);
        fake.with(|s| assert_eq!(s.calls.screens, vec!["t1", "t1", "t2", "t2"]));
    }

    #[test]
    fn screen_watch_stops_on_none() {
        let fake = Fake::default();
        let mut watch = ScreenWatch::new(ms(500));
        watch.set(Some("t1".into()), ms(0));
        run_watch(&mut watch, &fake, 0, 600);
        watch.set(None, ms(600));
        assert_eq!(watch.next_due(), None);
        assert!(run_watch(&mut watch, &fake, 600, 3000).is_empty());
        fake.with(|s| assert_eq!(s.calls.screens.len(), 2));
    }

    #[test]
    fn screen_watch_failure_keeps_last_lines_for_the_same_handle() {
        let fake = Fake::default();
        let mut watch = ScreenWatch::new(ms(500));
        watch.set(Some("t1".into()), ms(0));
        let good = watch.tick(&fake, ms(0), NOW_MS).unwrap();
        fake.with(|s| s.fail_screen = true);
        let failed = watch.tick(&fake, ms(500), NOW_MS + 500).unwrap();
        assert_eq!(failed.lines, good.lines);
        assert!(failed.error.as_deref().unwrap().contains("terminal gone"));

        // A new handle starts without borrowed lines.
        watch.set(Some("t2".into()), ms(600));
        let other = watch.tick(&fake, ms(600), NOW_MS + 600).unwrap();
        assert!(other.lines.is_empty());
        assert!(other.error.is_some());
    }

    #[test]
    fn spawned_poller_watches_screen_and_stops() {
        let fake = Fake::default();
        let config = PollerConfig {
            screen: ms(50),
            ..PollerConfig::default()
        };
        let poller = Poller::spawn(fake.clone(), config, None);
        std::thread::sleep(ms(150));
        assert!(poller.latest_screen().is_none(), "nothing watched yet");

        poller.send(PollerCommand::WatchScreen(Some("t1".into())));
        let first = poller.screens.recv_timeout(secs(2)).unwrap();
        assert_eq!(first.handle, "t1");
        poller.screens.recv_timeout(secs(2)).unwrap();

        poller.send(PollerCommand::WatchScreen(Some("t2".into())));
        let deadline = Instant::now() + secs(2);
        loop {
            let update = poller.screens.recv_timeout(secs(2)).unwrap();
            if update.handle == "t2" {
                break;
            }
            assert!(Instant::now() < deadline);
        }

        poller.send(PollerCommand::WatchScreen(None));
        std::thread::sleep(ms(100));
        let _ = poller.latest_screen();
        let reads = fake.with(|s| s.calls.screens.len());
        std::thread::sleep(ms(300));
        assert!(poller.latest_screen().is_none());
        fake.with(|s| assert_eq!(s.calls.screens.len(), reads));

        let started = Instant::now();
        drop(poller);
        assert!(started.elapsed() < ms(500));
    }

    #[test]
    fn screen_updates_are_not_delayed_by_slow_fleet_calls() {
        #[derive(Clone, Default)]
        struct SlowFleet(Fake);
        impl FleetSource for SlowFleet {
            fn terminals(&self) -> Result<Vec<Terminal>, OrcaError> {
                std::thread::sleep(ms(1000));
                self.0.terminals()
            }
            fn runs(&self) -> Result<Vec<Run>, OrcaError> {
                self.0.runs()
            }
            fn workers(&self) -> Result<Vec<Worker>, OrcaError> {
                self.0.workers()
            }
            fn tasks(&self, run_id: &str) -> Result<Vec<Task>, OrcaError> {
                self.0.tasks(run_id)
            }
            fn inbox(&self, limit: u64) -> Result<Vec<InboxMessage>, OrcaError> {
                self.0.inbox(limit)
            }
        }
        impl ScreenSource for SlowFleet {
            fn read_screen(&self, handle: &str) -> Result<TerminalRead, OrcaError> {
                self.0.read_screen(handle)
            }
        }

        let config = PollerConfig {
            screen: ms(50),
            ..PollerConfig::default()
        };
        let poller = Poller::spawn(SlowFleet::default(), config, None);
        poller.send(PollerCommand::WatchScreen(Some("t1".into())));
        let started = Instant::now();
        for _ in 0..4 {
            poller.screens.recv_timeout(ms(500)).unwrap();
        }
        assert!(
            started.elapsed() < ms(800),
            "screen reads waited on the fleet cycle"
        );
    }

    #[test]
    fn active_runs_by_coordinator_worker_or_recency() {
        let runs = vec![
            run("coord-live", Some("t1"), Some(NOW_MS - 48 * HOUR_MS)),
            run("worker-live", Some("gone"), None),
            run("resource-live", None, None),
            run("recent", Some("gone"), Some(NOW_MS - 23 * HOUR_MS)),
            run("stale", Some("gone"), Some(NOW_MS - 25 * HOUR_MS)),
            run("stale-worker-dead", None, Some(NOW_MS - 25 * HOUR_MS)),
        ];
        let workers = vec![
            worker("worker-live", Some("t2"), None),
            worker("resource-live", None, Some("t3")),
            worker("stale-worker-dead", Some("gone"), Some("gone")),
        ];
        let terminals = vec![terminal("t1"), terminal("t2"), terminal("t3")];
        let active = active_run_ids(&runs, &workers, &terminals, NOW_MS, ACTIVE_RUN_WINDOW);
        let active: Vec<_> = active.iter().map(String::as_str).collect();
        assert_eq!(
            active,
            vec!["coord-live", "recent", "resource-live", "worker-live"]
        );
    }

    #[test]
    fn active_runs_accepts_text_timestamps() {
        let mut r = run("r", None, None);
        r.updated_at = Some(Timestamp::Text("2026-09-25 08:00:00".into()));
        let now = crate::orca::parse_timestamp_ms("2026-09-25T10:00:00Z").unwrap();
        assert!(active_run_ids(&[r.clone()], &[], &[], now, ACTIVE_RUN_WINDOW).contains("r"));
        let later = now + 30 * HOUR_MS;
        assert!(active_run_ids(&[r], &[], &[], later, ACTIVE_RUN_WINDOW).is_empty());
    }

    #[test]
    fn failed_call_keeps_previous_value_and_reports_until_success() {
        let fake = Fake::default();
        fake.with(|s| {
            s.terminals = vec![terminal("t1")];
            s.runs = vec![run("r1", Some("t1"), None)];
        });
        let mut state = PollState::new(PollerConfig::default());
        let first = state.tick(&fake, secs(0), NOW_MS).unwrap();
        assert_eq!(first.terminals.len(), 1);
        assert!(first.errors.is_empty());

        fake.with(|s| {
            s.fail_terminals = true;
            s.fail_runs = true;
            s.terminals.clear();
        });
        let snap = state.tick(&fake, secs(2), NOW_MS + 2000).unwrap();
        assert_eq!(snap.terminals.len(), 1, "previous terminals retained");
        assert_eq!(snap.errors.len(), 1);
        assert!(snap.errors[0].contains("Orca runtime not reachable"));
        assert_eq!(snap.polled_at_ms, NOW_MS + 2000);

        // Runs fail on their tick; the terminal error is still outstanding.
        let snap = state.tick(&fake, secs(10), NOW_MS).unwrap();
        assert_eq!(snap.runs.len(), 1, "previous runs retained");
        assert_eq!(snap.errors.len(), 2);
        assert!(
            snap.errors
                .iter()
                .any(|e| e.starts_with("run list: Orca runtime not reachable"))
        );

        // Terminals recover; the run error stays until runs are next polled.
        fake.with(|s| s.fail_terminals = false);
        let snap = state.tick(&fake, secs(12), NOW_MS).unwrap();
        assert!(snap.terminals.is_empty());
        assert_eq!(snap.errors.len(), 1);
        assert!(snap.errors[0].starts_with("run list"));

        fake.with(|s| s.fail_runs = false);
        let snap = state.tick(&fake, secs(20), NOW_MS).unwrap();
        assert!(snap.errors.is_empty());
    }

    #[test]
    fn tasks_follow_active_runs() {
        let fake = Fake::default();
        fake.with(|s| {
            s.terminals = vec![terminal("t1")];
            s.runs = vec![run("r1", Some("t1"), None), run("r2", Some("t2"), None)];
            s.fail_tasks_for = Some("r1".into());
        });
        let mut state = PollState::new(PollerConfig::default());
        let snap = state.tick(&fake, secs(0), NOW_MS).unwrap();
        assert!(snap.tasks.is_empty());
        assert_eq!(snap.errors.len(), 1);
        assert!(snap.errors[0].contains("r1"));

        // r1's coordinator goes away and r2's appears: r1's tasks and error are dropped.
        fake.with(|s| s.terminals = vec![terminal("t2")]);
        let snap = state.tick(&fake, secs(10), NOW_MS).unwrap();
        assert_eq!(snap.tasks.keys().collect::<Vec<_>>(), vec!["r2"]);
        assert!(snap.errors.is_empty());
        fake.with(|s| assert_eq!(s.calls.tasks, vec!["r1", "r2"]));
    }

    #[test]
    fn spawned_poller_sends_repaints_and_shuts_down() {
        let fake = Fake::default();
        fake.with(|s| s.terminals = vec![terminal("t1")]);
        let repaints = Arc::new(AtomicUsize::new(0));
        let counter = repaints.clone();
        let poller = Poller::spawn(
            fake.clone(),
            PollerConfig::default(),
            Some(Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })),
        );
        let snap = poller.rx().recv_timeout(secs(2)).unwrap();
        assert_eq!(snap.terminals.len(), 1);

        poller.send(PollerCommand::RefreshNow);
        poller.rx().recv_timeout(secs(2)).unwrap();
        fake.with(|s| assert_eq!(s.calls.runs, 2));
        assert_eq!(repaints.load(Ordering::SeqCst), 2);

        let started = Instant::now();
        drop(poller);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn shutdown_command_stops_thread() {
        let poller = Poller::spawn(Fake::default(), PollerConfig::default(), None);
        poller.rx().recv_timeout(secs(2)).unwrap();
        poller.send(PollerCommand::Shutdown);
        // The thread exits and drops its sender.
        assert!(matches!(
            poller.rx().recv_timeout(secs(2)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn describe_error_is_readable() {
        let spawn = OrcaError::Spawn {
            binary: "orca".into(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        assert!(describe_error("terminal list", &spawn).contains("orca CLI not found"));
        let api = api_error("no runtime");
        assert_eq!(
            describe_error("inbox", &api),
            "inbox: no runtime (runtime_unavailable)"
        );
    }

    #[test]
    #[ignore = "needs a running Orca"]
    fn live_poller_receives_terminals() {
        let poller = Poller::spawn(OrcaClient::new(), PollerConfig::default(), None);
        let snap = poller.rx().recv_timeout(secs(30)).unwrap();
        assert!(snap.errors.is_empty(), "errors: {:?}", snap.errors);
        assert!(!snap.terminals.is_empty());
        eprintln!(
            "terminals={} runs={} workers={} tasks={} inbox={}",
            snap.terminals.len(),
            snap.runs.len(),
            snap.workers.len(),
            snap.tasks.len(),
            snap.inbox.len()
        );
    }

    #[test]
    #[ignore = "needs a running Orca"]
    fn live_poller_watches_a_screen() {
        let client = OrcaClient::new();
        let handle = client
            .terminal_list()
            .expect("terminal list")
            .terminals
            .into_iter()
            .find(|t| t.connected)
            .expect("a connected terminal")
            .handle;
        let poller = Poller::spawn(client, PollerConfig::default(), None);
        poller.send(PollerCommand::WatchScreen(Some(handle.clone())));
        let started = Instant::now();
        let mut updates = Vec::new();
        while started.elapsed() < secs(2) {
            if let Ok(update) = poller.screens.recv_timeout(ms(100)) {
                updates.push(update);
            }
        }
        for u in &updates {
            assert_eq!(u.handle, handle);
            assert!(u.error.is_none(), "error: {:?}", u.error);
        }
        eprintln!(
            "{} screen updates for {handle}; last has {} lines",
            updates.len(),
            updates.last().map_or(0, |u| u.lines.len())
        );
        assert!(updates.len() >= 3, "got {} updates", updates.len());
    }

    fn agent(
        handle: &str,
        identity: Option<AgentIdentity>,
        last_output_ms: Option<i64>,
    ) -> Terminal {
        Terminal {
            agent_identity: identity,
            last_output_at: last_output_ms.map(Timestamp::Millis),
            ..terminal(handle)
        }
    }

    #[test]
    fn prompt_sample_candidates_are_idle_live_agents() {
        let idle = Some(NOW_MS - 21_000);
        let terminals = vec![
            agent("z-codex", Some(AgentIdentity::Codex), idle),
            agent("a-claude", Some(AgentIdentity::Claude), idle),
            agent("never-output", Some(AgentIdentity::Claude), None),
            agent("busy", Some(AgentIdentity::Claude), Some(NOW_MS - 19_000)),
            agent("shell", None, idle),
            agent("other", Some(AgentIdentity::Other("aider".into())), idle),
            Terminal {
                connected: false,
                ..agent("disconnected", Some(AgentIdentity::Claude), idle)
            },
            Terminal {
                orphaned: true,
                ..agent("orphaned", Some(AgentIdentity::Claude), idle)
            },
        ];
        let picked: Vec<&str> = prompt_sample_candidates(&terminals, NOW_MS, secs(20))
            .iter()
            .map(|t| t.handle.as_str())
            .collect();
        assert_eq!(picked, ["a-claude", "never-output", "z-codex"]);
    }

    #[test]
    fn prompt_samples_rotate_within_budget() {
        let candidates = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let mut sampled: BTreeMap<String, Option<PromptKind>> = BTreeMap::new();
        let mut cursor: Option<String> = None;
        let mut rounds = Vec::new();
        for _ in 0..3 {
            let picked = pick_prompt_samples(&candidates, &sampled, cursor.as_deref(), 5);
            for h in &picked {
                sampled.insert(h.clone(), None);
            }
            cursor = picked.last().cloned();
            rounds.push(picked.join(""));
        }
        // New terminals first, then round-robin after the cursor.
        assert_eq!(rounds, ["abcde", "fghab", "cdefg"]);

        // A newcomer jumps the queue.
        let with_new = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
        assert_eq!(
            pick_prompt_samples(&with_new, &sampled, Some("g"), 3),
            ["i", "h", "a"]
        );
        assert!(pick_prompt_samples(&[], &sampled, Some("g"), 3).is_empty());
        assert!(pick_prompt_samples(&candidates, &sampled, None, 0).is_empty());
    }

    #[test]
    fn sampler_records_verdicts_on_its_cadence() {
        let idle = Some(NOW_MS - 60_000);
        let fake = Fake::default();
        let prompt: Vec<String> = ["Do you want to proceed?", "❯ 1. Yes", "  2. No"]
            .map(String::from)
            .into();
        let terminals: Vec<Terminal> = (0..7)
            .map(|i| agent(&format!("t{i}"), Some(AgentIdentity::Claude), idle))
            .collect();
        fake.with(|s| {
            s.screen_rows.insert("t1".into(), prompt.clone());
        });
        let config = PollerConfig {
            prompt_sample_budget: 5,
            ..PollerConfig::default()
        };
        let mut sampler = PromptSampler::new(&config);
        let mut verdicts = BTreeMap::new();

        sampler.tick(&mut verdicts, &terminals, &fake, secs(0), NOW_MS);
        assert_eq!(fake.with(|s| s.calls.screens.len()), 5, "budget");
        assert_eq!(verdicts.get("t1"), Some(&Some(PromptKind::Permission)));
        assert_eq!(verdicts.get("t0"), Some(&None));
        assert_eq!(verdicts.len(), 5);

        // Not due again until the interval passes.
        sampler.tick(&mut verdicts, &terminals, &fake, secs(9), NOW_MS + 9_000);
        assert_eq!(fake.with(|s| s.calls.screens.len()), 5);
        sampler.tick(&mut verdicts, &terminals, &fake, secs(10), NOW_MS + 10_000);
        let reads = fake.with(|s| s.calls.screens.clone());
        assert_eq!(reads[5..], ["t5", "t6", "t0", "t1", "t2"]);
        assert_eq!(verdicts.len(), 7);

        // Output from t1 drops its verdict at once, even between samples.
        let mut resumed = terminals.clone();
        resumed[1].last_output_at = Some(Timestamp::Millis(NOW_MS + 11_000));
        sampler.tick(&mut verdicts, &resumed, &fake, secs(12), NOW_MS + 12_000);
        assert!(!verdicts.contains_key("t1"));
        assert_eq!(verdicts.len(), 6);

        // A failed read keeps the previous verdict.
        fake.with(|s| s.fail_screen = true);
        sampler.tick(&mut verdicts, &terminals, &fake, secs(20), NOW_MS + 20_000);
        assert_eq!(verdicts.len(), 6);
    }

    #[test]
    fn poll_state_puts_samples_in_fleet_data() {
        let fake = Fake::default();
        fake.with(|s| {
            s.terminals = vec![
                agent("waiting", Some(AgentIdentity::Codex), Some(NOW_MS - 60_000)),
                agent("busy", Some(AgentIdentity::Codex), Some(NOW_MS - 1_000)),
            ];
            s.screen_rows.insert(
                "waiting".into(),
                ["Allow command?", "$ rm -rf target"]
                    .map(String::from)
                    .into(),
            );
        });
        let mut state = PollState::new(PollerConfig::default());
        let data = state
            .tick_with_screens(&fake, &fake, secs(0), NOW_MS)
            .unwrap();
        assert_eq!(fake.with(|s| s.calls.screens.clone()), ["waiting"]);
        assert_eq!(data.screen_needs_input("waiting"), Some(true));
        assert_eq!(data.screen_needs_input("busy"), None);

        // Plain ticks never read screens.
        let mut plain = PollState::new(PollerConfig::default());
        let data = plain.tick(&fake, secs(0), NOW_MS).unwrap();
        assert!(data.needs_input.is_empty());
        assert_eq!(fake.with(|s| s.calls.screens.len()), 1);

        // The sampler's own cadence wakes the loop and yields a snapshot.
        let config = PollerConfig {
            terminals: secs(60),
            runs_workers: secs(60),
            tasks: secs(60),
            inbox: secs(60),
            ..PollerConfig::default()
        };
        let mut slow = PollState::new(config);
        slow.tick_with_screens(&fake, &fake, secs(0), NOW_MS)
            .unwrap();
        assert_eq!(slow.next_due(), secs(10));
        assert!(
            slow.tick_with_screens(&fake, &fake, secs(10), NOW_MS + 10_000)
                .is_some()
        );
    }
}

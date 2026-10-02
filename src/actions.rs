//! Orca actions that change something (terminal input, tab switching, closing sessions), run
//! off the UI thread.
//!
//! [`ActionRunner`] owns one worker thread that executes [`Action`]s in submission order and
//! reports each as an [`ActionResult`]. The executor is a trait so tests (and the whip later)
//! can drive the runner without touching real terminals.

use std::process::Command;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

use crate::orca::{OrcaClient, OrcaError, TerminalInput};

/// Bundle id of Orca.app, used to bring its window to the front.
pub const ORCA_BUNDLE_ID: &str = "com.stablyai.orca";

/// Timeout for action calls. `terminal send` with text and Enter waits until the agent reacts
/// to the prompt (Orca gives up after 30 s), so it needs far longer than a read.
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(45);

/// A keypress sent as raw bytes through `orca terminal send --text` (without `--enter`, Orca
/// writes the text to the PTY unchanged; with `--enter` it pastes it and escapes `ESC`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Escape,
    Up,
    Down,
    Left,
    Right,
    Tab,
    /// A digit key `0`–`9` (numbered choices in Claude/Codex prompts).
    Digit(u8),
}

impl Key {
    /// The bytes a terminal sends for this key (normal cursor mode).
    pub fn bytes(self) -> String {
        match self {
            Key::Escape => "\x1b".into(),
            Key::Up => "\x1b[A".into(),
            Key::Down => "\x1b[B".into(),
            Key::Right => "\x1b[C".into(),
            Key::Left => "\x1b[D".into(),
            Key::Tab => "\t".into(),
            Key::Digit(d) => char::from(b'0' + d.min(9)).to_string(),
        }
    }
}

impl TerminalInput {
    /// `text` followed by Enter (a prompt submission).
    pub fn line(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            enter: true,
            interrupt: false,
        }
    }

    /// A bare Enter, e.g. to accept a prompt's default choice.
    pub fn enter_key() -> Self {
        Self {
            text: None,
            enter: true,
            interrupt: false,
        }
    }

    /// Ctrl-C (Orca's `--interrupt` writes `0x03`).
    pub fn interrupt_key() -> Self {
        Self {
            text: None,
            enter: false,
            interrupt: true,
        }
    }

    /// A single keypress, written raw.
    pub fn key(key: Key) -> Self {
        Self {
            text: Some(key.bytes()),
            enter: false,
            interrupt: false,
        }
    }
}

/// Something to do in Orca.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// `orca terminal send`.
    Send {
        handle: String,
        input: TerminalInput,
    },
    /// `orca terminal switch`, then bring Orca's window to the front.
    Switch { handle: String },
    /// `orca orchestration worker-stop`: fence the dispatch (and close its supervised terminal).
    WorkerStop {
        dispatch_id: String,
        /// The worker's terminal, for status and logs.
        handle: Option<String>,
    },
    /// `orca terminal close`, or with `tab` the terminal's whole tab. A terminal that is
    /// already gone counts as closed.
    CloseTerminal { handle: String, tab: bool },
}

impl Action {
    /// The terminal the action is about (a worker-stop without one: its dispatch id).
    pub fn handle(&self) -> &str {
        match self {
            Action::Send { handle, .. }
            | Action::Switch { handle }
            | Action::CloseTerminal { handle, .. } => handle,
            Action::WorkerStop {
                handle,
                dispatch_id,
                ..
            } => handle.as_deref().unwrap_or(dispatch_id),
        }
    }

    /// The `orca` arguments the [`OrcaExecutor`] runs for this action (without `--json`).
    pub fn cli_args(&self) -> Vec<String> {
        use crate::orca::commands;
        match self {
            Action::Send { handle, input } => commands::terminal_send_args(handle, input),
            Action::Switch { handle } => commands::terminal_switch_args(handle),
            Action::WorkerStop { dispatch_id, .. } => commands::worker_stop_args(dispatch_id),
            Action::CloseTerminal { handle, tab } => commands::terminal_close_args(handle, *tab),
        }
    }
}

/// Id the runner assigns to a submitted action, in submission order.
pub type ActionId = u64;

/// Outcome of one action.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionResult {
    pub id: ActionId,
    pub action: Action,
    pub outcome: Result<(), String>,
    /// What happened, when the executor said more than "ok" (e.g. "already closed").
    pub note: Option<String>,
}

/// Executes actions; runs on the runner's thread.
pub trait ActionExecutor: Send + 'static {
    fn execute(&self, action: &Action) -> Result<(), String>;

    /// Like [`ActionExecutor::execute`], with a short note on success. The runner calls this.
    fn execute_noted(&self, action: &Action) -> Result<Option<String>, String> {
        self.execute(action).map(|()| None)
    }
}

/// Error codes `terminal close` returns for a terminal that no longer exists.
pub const GONE_TERMINAL_CODES: &[&str] = &[
    "terminal_handle_stale",
    "terminal_not_found",
    "terminal_gone",
];

/// Result note of a close whose terminal was already gone (e.g. worker-stop closed it).
pub const ALREADY_CLOSED: &str = "already closed";

/// Maps a `terminal close` call's result to the action outcome: a terminal that is already
/// gone is not an error.
pub fn close_outcome(
    result: Result<crate::orca::TerminalClose, OrcaError>,
) -> Result<Option<String>, String> {
    match result {
        Ok(close) => Ok(Some(close.summary())),
        Err(err) if err.code().is_some_and(|c| GONE_TERMINAL_CODES.contains(&c)) => {
            Ok(Some(ALREADY_CLOSED.into()))
        }
        Err(err) => Err(err.to_string()),
    }
}

/// Maps a `worker-stop` call's result to the action outcome; `stop_unknown` is an error.
pub fn worker_stop_outcome(
    result: Result<crate::orca::WorkerStopResult, OrcaError>,
) -> Result<Option<String>, String> {
    let stop = result.map_err(|e| e.to_string())?;
    if stop.outcome_unknown() {
        return Err(stop.summary());
    }
    Ok(Some(stop.summary()))
}

/// Executes actions with the real `orca` CLI.
#[derive(Debug, Clone)]
pub struct OrcaExecutor {
    client: OrcaClient,
}

impl OrcaExecutor {
    pub fn new(client: OrcaClient) -> Self {
        Self { client }
    }
}

impl Default for OrcaExecutor {
    fn default() -> Self {
        Self::new(OrcaClient::new().timeout(ACTION_TIMEOUT))
    }
}

impl ActionExecutor for OrcaExecutor {
    fn execute(&self, action: &Action) -> Result<(), String> {
        self.execute_noted(action).map(|_| ())
    }

    fn execute_noted(&self, action: &Action) -> Result<Option<String>, String> {
        match action {
            Action::Send { handle, input } => {
                let send = self
                    .client
                    .terminal_send(handle, input)
                    .map_err(|e| e.to_string())?;
                if send.accepted == Some(false) {
                    return Err(match send.refused_reason {
                        Some(reason) => format!("Orca refused the input: {reason}"),
                        None => {
                            "Orca did not accept the input (terminal locked or not writable)".into()
                        }
                    });
                }
                Ok(None)
            }
            Action::Switch { handle } => {
                self.client
                    .terminal_switch(handle)
                    .map_err(|e| e.to_string())?;
                // `terminal switch` only selects the tab inside Orca; it doesn't raise the window.
                activate_orca().map(|()| None)
            }
            Action::WorkerStop { dispatch_id, .. } => {
                worker_stop_outcome(self.client.worker_stop(dispatch_id))
            }
            Action::CloseTerminal { handle, tab } => {
                close_outcome(self.client.terminal_close(handle, *tab))
            }
        }
    }
}

/// Brings Orca.app to the front (`open -b`, which never starts a second instance).
fn activate_orca() -> Result<(), String> {
    let status = Command::new("open")
        .args(["-b", ORCA_BUNDLE_ID])
        .status()
        .map_err(|e| format!("tab selected, but activating Orca failed: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "tab selected, but activating Orca failed: open exited with {status}"
        ))
    }
}

/// Callback that wakes the UI when a result is ready.
pub type Wake = Box<dyn Fn() + Send + Sync>;

/// Runs actions one at a time on a background thread, in submission order.
pub struct ActionRunner {
    jobs: Option<Sender<(ActionId, Action)>>,
    results: Receiver<ActionResult>,
    next_id: ActionId,
    thread: Option<JoinHandle<()>>,
}

impl ActionRunner {
    pub fn spawn(executor: impl ActionExecutor, wake: Option<Wake>) -> Self {
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<(ActionId, Action)>();
        let (result_tx, result_rx) = crossbeam_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("orca-actions".into())
            .spawn(move || {
                for (id, action) in job_rx {
                    let noted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        executor.execute_noted(&action)
                    }))
                    .unwrap_or_else(|_| Err("action panicked".into()));
                    if let Err(err) = &noted {
                        log::warn!("action on {} failed: {err}", action.handle());
                    }
                    let (outcome, note) = match noted {
                        Ok(note) => (Ok(()), note),
                        Err(err) => (Err(err), None),
                    };
                    if result_tx
                        .send(ActionResult {
                            id,
                            action,
                            outcome,
                            note,
                        })
                        .is_err()
                    {
                        return;
                    }
                    if let Some(wake) = &wake {
                        wake();
                    }
                }
            })
            .expect("spawn action thread");
        Self {
            jobs: Some(job_tx),
            results: result_rx,
            next_id: 1,
            thread: Some(thread),
        }
    }

    /// Queues `action`; its result arrives through [`ActionRunner::poll`] with the returned id.
    pub fn submit(&mut self, action: Action) -> ActionId {
        let id = self.next_id;
        self.next_id += 1;
        if let Some(jobs) = &self.jobs {
            // The thread only exits once `jobs` is dropped, so a send can't fail.
            let _ = jobs.send((id, action));
        }
        id
    }

    /// Results that arrived since the last call, in completion order.
    pub fn poll(&self) -> Vec<ActionResult> {
        self.results.try_iter().collect()
    }

    /// Blocks until the next result (tests and shutdown).
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ActionResult> {
        self.results.recv_timeout(timeout).ok()
    }
}

impl ActionRunner {
    /// Closes the queue and waits for the queued actions to finish.
    pub fn join(mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("action thread panicked");
        }
    }
}

impl Drop for ActionRunner {
    fn drop(&mut self) {
        // Closing the job channel ends the thread after the queued actions finish. It isn't
        // joined: an in-flight send can take up to ACTION_TIMEOUT and must not block quitting.
        self.jobs = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::orca::commands::terminal_send_args;

    #[derive(Clone, Default)]
    struct Fake {
        log: Arc<Mutex<Vec<Action>>>,
    }

    impl ActionExecutor for Fake {
        fn execute(&self, action: &Action) -> Result<(), String> {
            // Make earlier jobs slower so out-of-order execution would show.
            if let Action::Send {
                input: TerminalInput {
                    text: Some(text), ..
                },
                ..
            } = action
            {
                if text == "slow" {
                    std::thread::sleep(Duration::from_millis(30));
                }
                if text == "boom" {
                    panic!("boom");
                }
                if text == "fail" {
                    self.log.lock().unwrap().push(action.clone());
                    return Err("nope".into());
                }
            }
            self.log.lock().unwrap().push(action.clone());
            Ok(())
        }
    }

    fn send(handle: &str, text: &str) -> Action {
        Action::Send {
            handle: handle.into(),
            input: TerminalInput::line(text),
        }
    }

    #[test]
    fn key_bytes() {
        assert_eq!(Key::Escape.bytes(), "\u{1b}");
        assert_eq!(Key::Up.bytes(), "\u{1b}[A");
        assert_eq!(Key::Down.bytes(), "\u{1b}[B");
        assert_eq!(Key::Right.bytes(), "\u{1b}[C");
        assert_eq!(Key::Left.bytes(), "\u{1b}[D");
        assert_eq!(Key::Tab.bytes(), "\t");
        assert_eq!(Key::Digit(3).bytes(), "3");
        assert_eq!(Key::Digit(42).bytes(), "9");
    }

    #[test]
    fn input_helpers_map_to_cli_flags() {
        let args = |input: &TerminalInput| terminal_send_args("t", input)[3..].to_vec();
        assert_eq!(
            args(&TerminalInput::line("hi\nthere")),
            vec!["--text=hi\nthere", "--enter"]
        );
        assert!(args(&TerminalInput::enter_key()) == vec!["--enter"]);
        assert!(args(&TerminalInput::interrupt_key()) == vec!["--interrupt"]);
        // Keys go raw, without Enter (Orca would paste-escape ESC otherwise).
        assert_eq!(
            args(&TerminalInput::key(Key::Escape)),
            vec!["--text=\u{1b}"]
        );
        assert_eq!(args(&TerminalInput::key(Key::Digit(1))), vec!["--text=1"]);
    }

    #[test]
    fn runner_executes_in_order_and_reports_results() {
        let fake = Fake::default();
        let mut runner = ActionRunner::spawn(fake.clone(), None);
        let a = runner.submit(send("t1", "slow"));
        let b = runner.submit(send("t2", "fail"));
        let c = runner.submit(Action::Switch {
            handle: "t3".into(),
        });
        assert!(a < b && b < c);

        let results: Vec<_> = (0..3)
            .map(|_| runner.recv_timeout(Duration::from_secs(5)).expect("result"))
            .collect();
        assert_eq!(
            results.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![a, b, c]
        );
        assert_eq!(results[0].outcome, Ok(()));
        assert_eq!(results[1].outcome, Err("nope".into()));
        assert_eq!(
            results[2].action,
            Action::Switch {
                handle: "t3".into()
            }
        );
        assert_eq!(
            *fake.log.lock().unwrap(),
            vec![
                send("t1", "slow"),
                send("t2", "fail"),
                Action::Switch {
                    handle: "t3".into()
                }
            ]
        );
        assert!(runner.poll().is_empty());
    }

    #[test]
    fn runner_survives_a_panicking_executor_and_wakes_ui() {
        let wakes = Arc::new(Mutex::new(0));
        let counter = wakes.clone();
        let mut runner = ActionRunner::spawn(
            Fake::default(),
            Some(Box::new(move || *counter.lock().unwrap() += 1)),
        );
        runner.submit(send("t1", "boom"));
        runner.submit(send("t1", "after"));
        let first = runner.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first.outcome, Err("action panicked".into()));
        let second = runner.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(second.outcome, Ok(()));
        // The runner wakes the UI after sending each result, so the second wake
        // can land just after `recv_timeout` returns: wait for it.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while *wakes.lock().unwrap() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*wakes.lock().unwrap(), 2);
    }

    #[test]
    fn close_actions_build_cli_args() {
        let stop = Action::WorkerStop {
            dispatch_id: "d1".into(),
            handle: Some("t1".into()),
        };
        assert_eq!(
            stop.cli_args(),
            vec!["orchestration", "worker-stop", "--dispatch=d1"]
        );
        assert_eq!(stop.handle(), "t1");
        let bare = Action::WorkerStop {
            dispatch_id: "d1".into(),
            handle: None,
        };
        assert_eq!(bare.handle(), "d1");
        let close = |tab| Action::CloseTerminal {
            handle: "t1".into(),
            tab,
        };
        assert_eq!(
            close(false).cli_args(),
            vec!["terminal", "close", "--terminal=t1"]
        );
        assert_eq!(
            close(true).cli_args(),
            vec!["terminal", "close", "--terminal=t1", "--tab"]
        );
        assert_eq!(
            Action::Switch {
                handle: "t1".into()
            }
            .cli_args(),
            vec!["terminal", "switch", "--terminal=t1"]
        );
    }

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!("../tests/fixtures/orca/", $name))
        };
    }

    #[test]
    fn close_outcome_treats_a_gone_terminal_as_closed() {
        use crate::orca::commands::parse_terminal_close;
        assert_eq!(
            close_outcome(parse_terminal_close(fixture!("terminal_close.json"))),
            Ok(Some("closed term_1, PTY killed".into()))
        );
        assert_eq!(
            close_outcome(parse_terminal_close(fixture!(
                "error_terminal_handle_stale.json"
            ))),
            Ok(Some(ALREADY_CLOSED.into()))
        );
        let other = close_outcome(parse_terminal_close(fixture!(
            "error_invalid_argument.json"
        )));
        assert!(other.is_err(), "{other:?}");
    }

    #[test]
    fn worker_stop_outcome_fails_on_stop_unknown() {
        use crate::orca::commands::parse_worker_stop;
        assert_eq!(
            worker_stop_outcome(parse_worker_stop(fixture!("worker_stop.json"))),
            Ok(Some("dispatch stopped, closed its terminal".into()))
        );
        let unknown = worker_stop_outcome(parse_worker_stop(fixture!("worker_stop_unknown.json")));
        assert!(unknown.unwrap_err().starts_with("dispatch stop_unknown"));
    }

    #[test]
    fn runner_reports_executor_notes() {
        struct Noting;
        impl ActionExecutor for Noting {
            fn execute(&self, _: &Action) -> Result<(), String> {
                Ok(())
            }
            fn execute_noted(&self, _: &Action) -> Result<Option<String>, String> {
                Ok(Some(ALREADY_CLOSED.into()))
            }
        }
        let mut runner = ActionRunner::spawn(Noting, None);
        runner.submit(Action::CloseTerminal {
            handle: "t".into(),
            tab: false,
        });
        let result = runner.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(result.outcome, Ok(()));
        assert_eq!(result.note.as_deref(), Some(ALREADY_CLOSED));
    }

    #[test]
    fn join_finishes_queued_actions() {
        let fake = Fake::default();
        let mut runner = ActionRunner::spawn(fake.clone(), None);
        runner.submit(send("t1", "slow"));
        runner.submit(send("t1", "two"));
        runner.join();
        assert_eq!(fake.log.lock().unwrap().len(), 2);
    }
}

//! Orca actions that change something (terminal input, tab switching), run off the UI thread.
//!
//! [`ActionRunner`] owns one worker thread that executes [`Action`]s in submission order and
//! reports each as an [`ActionResult`]. The executor is a trait so tests (and the whip later)
//! can drive the runner without touching real terminals.

use std::process::Command;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

use crate::orca::{OrcaClient, TerminalInput};

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
}

impl Action {
    pub fn handle(&self) -> &str {
        match self {
            Action::Send { handle, .. } | Action::Switch { handle } => handle,
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
}

/// Executes actions; runs on the runner's thread.
pub trait ActionExecutor: Send + 'static {
    fn execute(&self, action: &Action) -> Result<(), String>;
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
                Ok(())
            }
            Action::Switch { handle } => {
                self.client
                    .terminal_switch(handle)
                    .map_err(|e| e.to_string())?;
                // `terminal switch` only selects the tab inside Orca; it doesn't raise the window.
                activate_orca()
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
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        executor.execute(&action)
                    }))
                    .unwrap_or_else(|_| Err("action panicked".into()));
                    if let Err(err) = &outcome {
                        log::warn!("action on {} failed: {err}", action.handle());
                    }
                    if result_tx
                        .send(ActionResult {
                            id,
                            action,
                            outcome,
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
        assert_eq!(*wakes.lock().unwrap(), 2);
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

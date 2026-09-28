//! The chat's `claude` process: a supervisor thread spawns it, writes its stdin, answers
//! permission prompts the policy allows, forwards everything else to the UI, and restarts it
//! with backoff (resuming the same session) when it dies. Reader threads drain stdout and
//! stderr. Dropping the [`ChatHandle`] stops the process.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select, unbounded};

use super::config::{ChatConfig, is_scrubbed_env};
use super::permissions::{PermissionPolicy, Verdict};
use super::protocol::{self, HostRequest, Incoming, PermissionAnswer, PermissionRequest};
use super::session::SessionStore;

/// The process's lifecycle as the pane shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    /// No `claude` binary; the text says how to set one up.
    Unavailable(String),
    /// Spawned, waiting for the answer to `initialize`.
    Starting,
    Ready,
    /// The process exited; a restart is due in `retry_in`.
    Crashed {
        reason: String,
        retry_in: Duration,
    },
    Stopped,
}

/// What the supervisor tells the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    State(SessionState),
    /// A message from the CLI (permission requests the policy allowed are included, so the
    /// transcript can show them; see [`ChatEvent::PermissionNeeded`]).
    Message(Incoming),
    /// A tool call that waits for the user.
    PermissionNeeded(PermissionRequest),
    /// A tool call allowed without a card only because auto mode is on (including a card that
    /// was waiting when auto mode turned on).
    AutoApproved(PermissionRequest),
    /// A new conversation started (New chat); the transcript should be cleared.
    NewSession(String),
    /// A message could not be delivered.
    Error(String),
}

/// What the UI asks of the supervisor.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatCommand {
    Send(String),
    Interrupt,
    Permission {
        request_id: String,
        allow: bool,
        /// Also allow later calls of this tool in this chat.
        always: bool,
    },
    NewChat,
    /// Turns auto mode on or off for the running session (no restart).
    SetAuto(bool),
    /// Restarts the process with this `--model` / `--effort`, resuming the same session.
    SetModel {
        model: Option<String>,
        effort: Option<String>,
    },
    /// Look for the binary again / restart now.
    Retry,
    Shutdown,
}

/// First restart delay; doubles per consecutive crash up to [`MAX_BACKOFF`].
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A process that lived this long resets the backoff.
const HEALTHY_RUN: Duration = Duration::from_secs(60);
/// How long a stopping process gets to exit after its stdin closes before it is killed.
const STOP_GRACE: Duration = Duration::from_millis(1000);
/// Lines of stderr kept to explain a crash.
const STDERR_TAIL: usize = 20;

/// Setup hint shown when `claude` is missing.
pub const MISSING_BINARY_HINT: &str = "Claude Code CLI not found. Install it (npm i -g \
@anthropic-ai/claude-code) or set MYRSTACK_CLAUDE_BIN to its path, then press Retry.";

/// The UI's end of the chat process.
pub struct ChatHandle {
    commands: Sender<ChatCommand>,
    events: Receiver<ChatEvent>,
    thread: Option<JoinHandle<()>>,
}

impl ChatHandle {
    /// Starts the supervisor (and the process, if the binary is there).
    pub fn spawn(config: ChatConfig, repaint: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        let (commands, command_rx) = unbounded();
        let (event_tx, events) = unbounded();
        let repaint: Arc<dyn Fn() + Send + Sync> = match repaint {
            Some(r) => Arc::from(r),
            None => Arc::new(|| {}),
        };
        let thread = std::thread::Builder::new()
            .name("chat-supervisor".into())
            .spawn(move || {
                Supervisor {
                    store: SessionStore::open(&config.dir),
                    config,
                    commands: command_rx,
                    events: event_tx,
                    repaint,
                    policy: PermissionPolicy::default(),
                    pending: HashMap::new(),
                }
                .run();
            })
            .expect("spawn chat supervisor");
        Self {
            commands,
            events,
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: ChatCommand) {
        let _ = self.commands.send(command);
    }

    /// Events since the last call.
    pub fn poll(&self) -> Vec<ChatEvent> {
        self.events.try_iter().collect()
    }

    /// Blocks up to `timeout` for the next event (tests and headless drivers).
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ChatEvent> {
        self.events.recv_timeout(timeout).ok()
    }
}

impl Drop for ChatHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(ChatCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Why a process run ended.
enum RunEnd {
    Shutdown,
    NewChat,
    /// The model changed: start again at once, same session.
    Restart,
    Crashed(String),
}

enum Line {
    Out(String),
    Eof,
}

struct Supervisor {
    config: ChatConfig,
    store: SessionStore,
    commands: Receiver<ChatCommand>,
    events: Sender<ChatEvent>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    policy: PermissionPolicy,
    /// Permission requests waiting for the user, by request id.
    pending: HashMap<String, PermissionRequest>,
}

impl Supervisor {
    fn emit(&self, event: ChatEvent) {
        let _ = self.events.send(event);
        (self.repaint)();
    }

    fn run(mut self) {
        let mut backoff = MIN_BACKOFF;
        loop {
            let Some(binary) = self.config.binary.clone() else {
                self.emit(ChatEvent::State(SessionState::Unavailable(
                    MISSING_BINARY_HINT.into(),
                )));
                if !self.wait_idle(None) {
                    return;
                }
                self.config.binary = super::config::find_claude();
                continue;
            };
            self.emit(ChatEvent::State(SessionState::Starting));
            let started = Instant::now();
            let end = match self.spawn(&binary) {
                Ok(child) => self.drive(child),
                Err(e) => RunEnd::Crashed(format!("cannot start {}: {e}", binary.display())),
            };
            match end {
                RunEnd::Shutdown => {
                    self.emit(ChatEvent::State(SessionState::Stopped));
                    return;
                }
                RunEnd::NewChat | RunEnd::Restart => {
                    backoff = MIN_BACKOFF;
                    continue;
                }
                RunEnd::Crashed(reason) => {
                    if started.elapsed() >= HEALTHY_RUN {
                        backoff = MIN_BACKOFF;
                    }
                    log::warn!("chat: claude exited: {reason}; restarting in {backoff:?}");
                    self.emit(ChatEvent::State(SessionState::Crashed {
                        reason,
                        retry_in: backoff,
                    }));
                    if !self.wait_idle(Some(backoff)) {
                        self.emit(ChatEvent::State(SessionState::Stopped));
                        return;
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// Waits without a process for `timeout` (forever when `None`) or until Retry/New chat;
    /// false on Shutdown.
    fn wait_idle(&mut self, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            let command = match deadline {
                Some(d) => match self
                    .commands
                    .recv_timeout(d.saturating_duration_since(Instant::now()))
                {
                    Ok(c) => c,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => return true,
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return false,
                },
                None => match self.commands.recv() {
                    Ok(c) => c,
                    Err(_) => return false,
                },
            };
            match command {
                ChatCommand::Shutdown => return false,
                ChatCommand::Retry => return true,
                ChatCommand::NewChat => {
                    self.start_new_chat();
                    return true;
                }
                ChatCommand::SetAuto(on) => self.policy.set_auto(on),
                ChatCommand::SetModel { model, effort } => {
                    self.set_model(model, effort);
                    return true;
                }
                ChatCommand::Send(_) => {
                    self.emit(ChatEvent::Error(
                        "Not sent: the chat is not running.".into(),
                    ));
                }
                ChatCommand::Interrupt | ChatCommand::Permission { .. } => {}
            }
        }
    }

    fn set_model(&mut self, model: Option<String>, effort: Option<String>) {
        log::info!(
            "chat: switching to model {} (effort {})",
            model.as_deref().unwrap_or("CLI default"),
            effort.as_deref().unwrap_or("CLI default")
        );
        self.config.model = model;
        self.config.effort = effort;
    }

    fn start_new_chat(&mut self) {
        self.store.new_session();
        self.policy.reset();
        self.pending.clear();
        self.emit(ChatEvent::NewSession(self.store.record.session_id.clone()));
    }

    fn spawn(&self, binary: &std::path::Path) -> std::io::Result<Child> {
        std::fs::create_dir_all(&self.config.dir)?;
        let args = self.config.args(&self.store.record.cli_args());
        let mut command = Command::new(binary);
        command
            .args(&args)
            .current_dir(&self.config.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(is_scrubbed_env) {
                command.env_remove(key);
            }
        }
        command.envs(self.config.env.iter().map(|(k, v)| (k, v)));
        log::info!(
            "chat: starting {} {} {} in {}",
            binary.display(),
            args.iter()
                .position(|a| a == "--resume" || a == "--session-id")
                .map_or("", |i| args[i].as_str()),
            self.store.record.session_id,
            self.config.dir.display()
        );
        command.spawn()
    }

    /// Runs one process until it exits, the user starts a new chat, or the app shuts down.
    fn drive(&mut self, mut child: Child) -> RunEnd {
        let (line_tx, lines) = unbounded();
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        spawn_stdout_reader(stdout, line_tx);
        let stderr_thread = spawn_stderr_logger(stderr, tail.clone());
        let mut stdin = child.stdin.take();

        let mut next_request = 0u64;
        let mut request_id = |kind: &str| {
            next_request += 1;
            format!("myrstack-{kind}-{next_request}")
        };
        let init_id = request_id("init");
        if let Err(e) = write_line(&mut stdin, &protocol::initialize_request(&init_id)) {
            stop(&mut child, stdin.take());
            return RunEnd::Crashed(format!("cannot write to claude: {e}"));
        }

        loop {
            select! {
                recv(self.commands) -> command => {
                    let command = command.unwrap_or(ChatCommand::Shutdown);
                    let result = match command {
                        ChatCommand::Shutdown => {
                            let _ = write_line(&mut stdin, &protocol::interrupt_request(&request_id("stop")));
                            stop(&mut child, stdin.take());
                            return RunEnd::Shutdown;
                        }
                        ChatCommand::NewChat => {
                            stop(&mut child, stdin.take());
                            self.start_new_chat();
                            return RunEnd::NewChat;
                        }
                        ChatCommand::Retry => Ok(()),
                        ChatCommand::SetModel { model, effort } => {
                            stop(&mut child, stdin.take());
                            self.pending.clear();
                            self.set_model(model, effort);
                            return RunEnd::Restart;
                        }
                        ChatCommand::SetAuto(on) => self.set_auto(on, &mut stdin),
                        ChatCommand::Send(text) => {
                            let sent = write_line(&mut stdin, &protocol::user_message(&text));
                            if sent.is_ok() {
                                self.store.mark_started();
                            }
                            sent
                        }
                        ChatCommand::Interrupt => {
                            write_line(&mut stdin, &protocol::interrupt_request(&request_id("interrupt")))
                        }
                        ChatCommand::Permission { request_id, allow, always } => {
                            match self.pending.remove(&request_id) {
                                Some(request) => {
                                    if allow && always {
                                        self.policy.always_allow(&request.tool_name);
                                    }
                                    let answer = if allow {
                                        PermissionAnswer::Allow
                                    } else {
                                        PermissionAnswer::Deny { message: "Karl denied this in the Myrstack chat.".into() }
                                    };
                                    write_line(&mut stdin, &protocol::permission_response(&request, &answer))
                                }
                                None => Ok(()),
                            }
                        }
                    };
                    if let Err(e) = result {
                        self.emit(ChatEvent::Error(format!("Cannot write to claude: {e}")));
                    }
                }
                recv(lines) -> line => {
                    match line.unwrap_or(Line::Eof) {
                        Line::Out(line) => self.on_line(&line, &init_id, &mut stdin),
                        Line::Eof => {
                            let status = wait_briefly(&mut child);
                            let deadline = Instant::now() + Duration::from_millis(300);
                            while !stderr_thread.is_finished() && Instant::now() < deadline {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            let tail: Vec<String> = tail.lock().map(|t| t.iter().cloned().collect()).unwrap_or_default();
                            self.pending.clear();
                            return RunEnd::Crashed(self.crash_reason(status, &tail));
                        }
                    }
                }
            }
        }
    }

    fn on_line(&mut self, line: &str, init_id: &str, stdin: &mut Option<ChildStdin>) {
        let message = match protocol::parse_line(line) {
            Ok(m) => m,
            Err(e) => {
                log::debug!("chat: unparsed line ({e}): {}", line_start(line));
                return;
            }
        };
        match &message {
            Incoming::ControlResponse {
                request_id, error, ..
            } if request_id == init_id => {
                if let Some(error) = error {
                    log::warn!("chat: initialize failed: {error}");
                }
                self.emit(ChatEvent::State(SessionState::Ready));
                return;
            }
            Incoming::ControlRequest {
                request_id,
                request,
            } => {
                match request {
                    HostRequest::CanUseTool(permission) => {
                        match self.policy.verdict(&permission.tool_name) {
                            Verdict::Ask => {
                                self.pending.insert(request_id.clone(), permission.clone());
                                self.emit(ChatEvent::PermissionNeeded(permission.clone()));
                            }
                            verdict => {
                                log::info!("chat: auto-approved {}", permission.tool_name);
                                let reply = protocol::permission_response(
                                    permission,
                                    &PermissionAnswer::Allow,
                                );
                                if let Err(e) = write_line(stdin, &reply) {
                                    log::warn!("chat: cannot answer permission request: {e}");
                                }
                                if verdict == Verdict::Auto {
                                    self.emit(ChatEvent::AutoApproved(permission.clone()));
                                }
                            }
                        }
                    }
                    HostRequest::Other(subtype) => {
                        log::debug!("chat: declining control request {subtype}");
                        let reply = protocol::error_response(
                            request_id,
                            &format!("unsupported request: {subtype}"),
                        );
                        let _ = write_line(stdin, &reply);
                    }
                }
                return;
            }
            Incoming::Other(_) | Incoming::ControlResponse { .. } => return,
            _ => {}
        }
        self.emit(ChatEvent::Message(message));
    }

    /// Switches auto mode; turning it on also answers the Myrstack cards already waiting.
    fn set_auto(&mut self, on: bool, stdin: &mut Option<ChildStdin>) -> std::io::Result<()> {
        self.policy.set_auto(on);
        log::info!("chat: auto mode {}", if on { "on" } else { "off" });
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, r)| self.policy.verdict(&r.tool_name) != Verdict::Ask)
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            let Some(request) = self.pending.remove(&id) else {
                continue;
            };
            write_line(
                stdin,
                &protocol::permission_response(&request, &PermissionAnswer::Allow),
            )?;
            self.emit(ChatEvent::AutoApproved(request));
        }
        Ok(())
    }

    /// Explains an exit and fixes the session record when the CLI disagreed about it.
    fn crash_reason(
        &mut self,
        status: Option<std::process::ExitStatus>,
        tail: &[String],
    ) -> String {
        let text = tail.join("\n");
        if text.contains("No conversation found") {
            log::warn!("chat: session unknown to claude; creating it");
            self.store.mark_unstarted();
        } else if text.contains("already in use") {
            self.store.mark_started();
        }
        let last = tail
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .cloned()
            .unwrap_or_default();
        let status = status.map_or("still running".to_owned(), |s| s.to_string());
        if last.is_empty() {
            status
        } else {
            format!("{status}: {last}")
        }
    }
}

fn spawn_stdout_reader(stdout: std::process::ChildStdout, tx: Sender<Line>) {
    std::thread::Builder::new()
        .name("chat-stdout".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) if line.trim().is_empty() => {}
                    Ok(line) => {
                        if tx.send(Line::Out(line)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        log::debug!("chat: stdout read error: {e}");
                        break;
                    }
                }
            }
            let _ = tx.send(Line::Eof);
        })
        .expect("spawn chat stdout reader");
}

fn spawn_stderr_logger(
    stderr: std::process::ChildStderr,
    tail: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("chat-stderr".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::debug!("chat stderr: {line}");
                if let Ok(mut tail) = tail.lock() {
                    if tail.len() == STDERR_TAIL {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            }
        })
        .expect("spawn chat stderr logger")
}

fn write_line(stdin: &mut Option<ChildStdin>, line: &str) -> std::io::Result<()> {
    let Some(stdin) = stdin else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "stdin closed",
        ));
    };
    stdin.write_all(line.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()
}

/// Closes stdin (the CLI exits at end of input), waits [`STOP_GRACE`], then kills.
fn stop(child: &mut Child, stdin: Option<ChildStdin>) {
    drop(stdin);
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The exit status of a child whose stdout closed, waiting a moment for it to exit.
fn wait_briefly(child: &mut Child) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + STOP_GRACE;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn line_start(line: &str) -> &str {
    let end = line.char_indices().nth(200).map_or(line.len(), |(i, _)| i);
    &line[..end]
}

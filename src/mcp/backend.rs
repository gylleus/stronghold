//! Where MCP tool calls go: the running app's control socket ([`SocketControl`]), or a one-off
//! read-only poll of Orca when the app is not running ([`HeadlessControl`]).

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::control::server::{Backend, handle_line, now_ms};
use crate::control::view::{StatusView, ViewInputs};
use crate::control::{
    CommandHandler, ControlClient, ControlCommand, ControlError, ErrorCode, Request, SharedStatus,
};
use crate::model::ClanSnapshot;
use crate::orca::OrcaClient;
use crate::poller::{PollState, PollerConfig};
use crate::quota::{QuotaSource, Quotas};
use crate::ui::sidebar::ClanFilter;
use crate::whip::{WhipSettings, WhipStatus};

/// How long a headless poll is reused before Orca is polled again.
const HEADLESS_TTL: Duration = Duration::from_secs(15);

/// Why a control call did not return a result.
#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    /// No app serves the socket (missing socket or connection refused).
    NotRunning,
    /// The socket exists but talking to it failed.
    Transport(String),
    /// The app answered with an error.
    Control(ControlError),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::NotRunning => f.write_str(NOT_RUNNING),
            CallError::Transport(e) => write!(f, "couldn't talk to Myrstack: {e}"),
            CallError::Control(e) => write!(f, "{e}"),
        }
    }
}

pub const NOT_RUNNING: &str = "Myrstack isn't running — start it with `myrstack`.";

/// Something that answers control API methods.
pub trait Control {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError>;

    /// A read-only stand-in for when the app is not running, if this control has one.
    fn fallback(&mut self) -> Result<&mut dyn Control, String> {
        Err("no fallback".into())
    }

    /// Calls `method` without waiting for it or its outcome, for what only the app's drawing
    /// needs (a tool call's beam). Does nothing where there is no app to draw.
    fn emit(&mut self, _method: &str, _params: Value) {}
}

/// The running app's control socket. Every call opens a fresh connection: they are cheap, and a
/// chat can sit idle longer than the server keeps a connection open.
pub struct SocketControl {
    path: PathBuf,
    headless: Option<(Instant, HeadlessControl)>,
}

impl SocketControl {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            headless: None,
        }
    }
}

impl Control for SocketControl {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
        let mut client = ControlClient::connect(&self.path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => CallError::NotRunning,
            _ => CallError::Transport(format!("{}: {e}", self.path.display())),
        })?;
        let response = client
            .call(&Request::new(method, params))
            .map_err(|e| CallError::Transport(e.to_string()))?;
        response.into_result().map_err(CallError::Control)
    }

    fn emit(&mut self, method: &str, params: Value) {
        let path = self.path.clone();
        let method = method.to_owned();
        let sent = std::thread::Builder::new()
            .name("mcp-emit".into())
            .spawn(move || {
                let result = ControlClient::connect(&path)
                    .and_then(|mut c| c.call(&Request::new(&method, params)));
                if let Err(e) = result {
                    log::debug!("myrstack mcp: {method} not drawn: {e}");
                }
            });
        if let Err(e) = sent {
            log::debug!("myrstack mcp: cannot start emit thread: {e}");
        }
    }

    fn fallback(&mut self) -> Result<&mut dyn Control, String> {
        let fresh = self
            .headless
            .as_ref()
            .is_some_and(|(at, _)| at.elapsed() < HEADLESS_TTL);
        if !fresh {
            log::info!("myrstack mcp: app not running; polling Orca once");
            self.headless = Some((Instant::now(), HeadlessControl::poll(OrcaClient::new())));
        }
        Ok(&mut self.headless.as_mut().expect("just set").1)
    }
}

/// Serves the read methods from one direct poll of Orca, without whip, Claude or dashboard
/// state. Actions are refused.
pub struct HeadlessControl {
    backend: Backend,
}

impl HeadlessControl {
    /// Polls Orca once (terminals, runs, workers, tasks, inbox and quotas).
    pub fn poll(orca: OrcaClient) -> Self {
        let now = now_ms();
        let mut data = PollState::new(PollerConfig::default())
            .tick(&orca, Duration::ZERO, now)
            .unwrap_or_default();
        data.forwards = crate::claude::forwards::read_forwards(&data);
        let quotas = match orca.fetch() {
            Ok(list) => Quotas::from_account_list(&list, now),
            Err(e) => Quotas {
                error: Some(e),
                ..Quotas::default()
            },
        };
        let snapshot = ClanSnapshot::build(&data, now);
        let view = StatusView::build(&ViewInputs {
            snapshot: Some(&snapshot),
            data: Some(&data),
            claude: None,
            whip_status: &WhipStatus::default(),
            whip_settings: &WhipSettings::default(),
            whip_log: Vec::new(),
            quotas: &quotas,
            alerts: &[],
            selected: None,
            selection: Vec::new(),
            filter: &ClanFilter::default(),
            whip_settings_open: false,
            now_ms: now,
        });
        Self::from_view(view, Arc::new(orca))
    }

    pub fn from_view(view: StatusView, screens: Arc<dyn crate::control::ScreenReader>) -> Self {
        let status = SharedStatus::new();
        status.publish(view);
        Self {
            backend: Backend {
                status,
                screens,
                commands: Arc::new(RefuseCommands),
            },
        }
    }
}

impl Control for HeadlessControl {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
        let line = serde_json::to_string(&Request::new(method, params))
            .map_err(|e| CallError::Transport(e.to_string()))?;
        handle_line(&self.backend, &line)
            .into_result()
            .map_err(CallError::Control)
    }
}

struct RefuseCommands;

impl CommandHandler for RefuseCommands {
    fn execute(&self, _: ControlCommand) -> Result<Value, ControlError> {
        Err(ControlError::new(ErrorCode::Unavailable, NOT_RUNNING))
    }
}

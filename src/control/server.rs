//! The control socket: accepts connections on a Unix socket and answers requests line by line.
//!
//! Reads are served from the [`SharedStatus`] the app publishes (plus an on-demand screen read
//! for `agent`); actions go to the [`CommandHandler`]. Every connection gets its own thread, so
//! a slow action never holds up other clients, and nothing here runs on the UI thread.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::control::commands::{CommandHandler, ControlCommand};
use crate::control::protocol::{
    ControlError, ErrorCode, PROTOCOL_VERSION, Params, Response, parse_request,
};
use crate::control::view::{AgentView, SharedStatus, StatusView, parse_kind, parse_state};
use crate::orca::OrcaClient;

/// Longest request line accepted.
pub const MAX_REQUEST_BYTES: usize = 1 << 20;
/// Most connections served at once; more are refused with an error line.
pub const MAX_CONNECTIONS: usize = 32;
/// A connection with no request for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Screen rows `agent` returns by default, and at most.
pub const DEFAULT_SCREEN_LINES: usize = 40;
pub const MAX_SCREEN_LINES: usize = 500;
/// Whip log entries `whip` returns by default.
pub const DEFAULT_WHIP_LOG: usize = 20;

/// Read methods, in the order `ping` lists them.
pub const READ_METHODS: &[&str] = &[
    "ping",
    "status",
    "agents",
    "agent",
    "whip",
    "quotas",
    "alerts",
    "fleet_data",
    "review_queue",
];
/// Action methods, in the order `ping` lists them.
pub const ACTION_METHODS: &[&str] = &[
    "send",
    "enter",
    "esc",
    "interrupt",
    "broadcast",
    "whip_settings",
    "select",
    "focus",
    "filter",
    "view",
    "fit",
    "open_whip_settings",
    "close",
    "whip",
    "pr_ready",
    "review_dismiss",
    "review_done",
];

/// One read of a terminal's screen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScreenRead {
    pub lines: Vec<String>,
    /// `running` or `exited`, when Orca says.
    pub status: Option<String>,
}

/// Reads a terminal's screen on demand; the app uses `orca terminal read`.
pub trait ScreenReader: Send + Sync {
    fn read_screen(&self, handle: &str) -> Result<ScreenRead, String>;
}

impl ScreenReader for OrcaClient {
    fn read_screen(&self, handle: &str) -> Result<ScreenRead, String> {
        let read = self
            .terminal_read_screen(handle)
            .map_err(|e| e.to_string())?;
        Ok(ScreenRead {
            lines: read.tail,
            status: read.status.map(|s| s.to_string()),
        })
    }
}

/// What the server answers from.
#[derive(Clone)]
pub struct Backend {
    pub status: SharedStatus,
    pub screens: Arc<dyn ScreenReader>,
    pub commands: Arc<dyn CommandHandler>,
}

/// Answers one request line.
pub fn handle_line(backend: &Backend, line: &str) -> Response {
    let request = match parse_request(line) {
        Ok(r) => r,
        Err((error, id)) => return Response::from_result(Err(error), id),
    };
    let result = dispatch(backend, &request.method, &request.params);
    Response::from_result(result, request.id)
}

fn dispatch(backend: &Backend, method: &str, params: &Value) -> Result<Value, ControlError> {
    if let Some(command) = ControlCommand::parse(method, params) {
        return backend.commands.execute(command?);
    }
    let view = backend.status.current();
    let now = now_ms();
    match method {
        "ping" => Ok(json!({
            "protocol": PROTOCOL_VERSION,
            "version": env!("CARGO_PKG_VERSION"),
            "ready": view.polled_at_ms.is_some(),
            "methods": {"read": READ_METHODS, "action": ACTION_METHODS},
        })),
        "status" => Ok(status(&view)),
        "agents" => agents(&view, params, now),
        "agent" => agent(backend, &view, params, now),
        "whip" => whip(&view, params),
        "quotas" => to_value(&view.quotas),
        "alerts" => Ok(json!({ "alerts": view.alerts })),
        "fleet_data" => fleet_data(&view),
        other => Err(ControlError::new(
            ErrorCode::UnknownMethod,
            format!("unknown method {other:?}; call \"ping\" for the list"),
        )),
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, ControlError> {
    serde_json::to_value(value).map_err(|e| ControlError::failed(format!("serializing: {e}")))
}

fn require_data(view: &StatusView) -> Result<(), ControlError> {
    if view.polled_at_ms.is_none() {
        return Err(ControlError::new(
            ErrorCode::Unavailable,
            "the app has not polled Orca yet",
        ));
    }
    Ok(())
}

fn status(view: &StatusView) -> Value {
    json!({
        "ready": view.polled_at_ms.is_some(),
        "published_at_ms": view.published_at_ms,
        "polled_at_ms": view.polled_at_ms,
        "errors": view.errors,
        "fleet": view.fleet,
        "whip": {
            "enabled": view.whip.enabled,
            "dry_run": view.whip.dry_run,
            "last_tick_ms": view.whip.last_tick_ms,
            "findings": view.whip.findings,
            "escalated": view.whip.escalated,
        },
        "alerts": view.alerts.len(),
        "dashboard": view.dashboard,
        "playbook": view.playbook,
        "queen_auto": view.queen_auto,
    })
}

/// The last poll's raw runs, workers, tasks, terminals and inbox (no previews or bodies).
fn fleet_data(view: &StatusView) -> Result<Value, ControlError> {
    require_data(view)?;
    match &view.fleet_data {
        Some(facts) => to_value(&**facts),
        None => Err(ControlError::new(
            ErrorCode::Unavailable,
            "the app has not polled Orca yet",
        )),
    }
}

fn agents(view: &StatusView, params: &Value, now: i64) -> Result<Value, ControlError> {
    require_data(view)?;
    let p = Params::new(params);
    let states = p
        .strings("state")?
        .map(|names| {
            names
                .iter()
                .map(|n| {
                    parse_state(n)
                        .map(crate::control::view::state_name)
                        .ok_or_else(|| ControlError::invalid_params(format!("unknown state {n:?}")))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let kinds = p
        .strings("kind")?
        .map(|names| {
            names
                .iter()
                .map(|n| {
                    parse_kind(n)
                        .map(crate::control::view::kind_name)
                        .ok_or_else(|| ControlError::invalid_params(format!("unknown kind {n:?}")))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let run = p.str("run")?;
    let repo = p.str("repo")?;
    let needs_input = p.bool("needs_input")?;
    let live = p.bool("live")?;
    let text = p.str("text")?.map(str::to_lowercase);
    let matches = |a: &&AgentView| {
        states
            .as_ref()
            .is_none_or(|s| s.contains(&a.state.as_str()))
            && kinds.as_ref().is_none_or(|k| k.contains(&a.kind.as_str()))
            && run.is_none_or(|r| a.run_id.as_deref() == Some(r))
            && repo.is_none_or(|r| a.repo == r)
            && needs_input.is_none_or(|n| a.needs_input.is_some() == n)
            && live.is_none_or(|l| a.live == l)
            && text.as_deref().is_none_or(|t| {
                [
                    Some(a.label.as_str()),
                    a.branch.as_deref(),
                    a.run_objective.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|s| s.to_lowercase().contains(t))
            })
    };
    let list: Vec<AgentView> = view
        .agents
        .iter()
        .filter(matches)
        .map(|a| a.at(now))
        .collect();
    Ok(json!({ "count": list.len(), "agents": list }))
}

fn agent(
    backend: &Backend,
    view: &StatusView,
    params: &Value,
    now: i64,
) -> Result<Value, ControlError> {
    require_data(view)?;
    let p = Params::new(params);
    let key = p.required_str("id")?;
    let lines = p
        .usize("lines")?
        .unwrap_or(DEFAULT_SCREEN_LINES)
        .min(MAX_SCREEN_LINES);
    let want_screen = p.bool("screen")?.unwrap_or(true);
    let agent = view
        .find_agent(key)
        .ok_or_else(|| ControlError::not_found(format!("no agent {key:?}")))?
        .at(now);
    let children: Vec<&str> = view
        .agents
        .iter()
        .filter(|a| a.parent.as_deref() == Some(agent.id.as_str()))
        .map(|a| a.id.as_str())
        .collect();
    let screen = match (&agent.terminal_handle, want_screen) {
        (Some(handle), true) if agent.live && agent.kind != "subagent" => {
            Some(match backend.screens.read_screen(handle) {
                Ok(read) => json!({
                    "handle": handle,
                    "read_at_ms": now_ms(),
                    "status": read.status,
                    "lines": tail(&read.lines, lines),
                }),
                Err(error) => json!({ "handle": handle, "error": error }),
            })
        }
        _ => None,
    };
    Ok(json!({ "agent": agent, "children": children, "screen": screen }))
}

/// The last `n` rows, after trailing blank rows are dropped, without trailing padding.
pub fn tail(lines: &[String], n: usize) -> Vec<String> {
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    lines[end.saturating_sub(n)..end]
        .iter()
        .map(|l| l.trim_end().to_owned())
        .collect()
}

fn whip(view: &StatusView, params: &Value) -> Result<Value, ControlError> {
    let limit = Params::new(params)
        .usize("limit")?
        .unwrap_or(DEFAULT_WHIP_LOG);
    let mut whip = view.whip.clone();
    let skip = whip.log.len().saturating_sub(limit);
    whip.log.drain(..skip);
    to_value(&whip)
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

// ---------------------------------------------------------------------------
// Socket

/// The running server; stops accepting and removes its socket when dropped.
pub struct ControlServer {
    path: PathBuf,
    /// `(dev, ino)` of the socket file this server created.
    inode: (u64, u64),
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControlServer {
    /// Binds `path` (see [`bind_socket`]) and serves `backend` on a background thread.
    pub fn start(path: &Path, backend: Backend) -> io::Result<Self> {
        let listener = bind_socket(path)?;
        let meta = fs::metadata(path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("control-accept".into())
                .spawn(move || accept_loop(&listener, &backend, &stop))?
        };
        log::info!("control API listening on {}", path.display());
        Ok(Self {
            path: path.to_path_buf(),
            inode: (meta.dev(), meta.ino()),
            stop,
            thread: Some(thread),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept so the thread sees the flag. If the socket file is gone the
        // accept can't be woken; the thread is then left to end with the process.
        let woke = UnixStream::connect(&self.path).is_ok();
        if let Some(thread) = self.thread.take()
            && woke
        {
            let _ = thread.join();
        }
        if let Ok(meta) = fs::symlink_metadata(&self.path)
            && (meta.dev(), meta.ino()) == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Creates the socket at `path` with mode 0600.
///
/// A leftover socket nobody listens on (the app crashed) is removed; a live one means another
/// Myrstack is serving, and binding fails with `AddrInUse`. The socket is bound under a temporary
/// name, restricted, then renamed into place, so it is never reachable with looser permissions.
pub fn bind_socket(path: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("another process is serving {}", path.display()),
                ));
            }
            log::info!("removing stale control socket {}", path.display());
            fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let tmp = path.with_file_name(format!(".c{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let listener = UnixListener::bind(&tmp).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "{e} (binding {}; set {} to a shorter path if it is too long)",
                path.display(),
                crate::control::SOCKET_ENV
            ),
        )
    })?;
    let placed = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
        .and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = placed {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(listener)
}

fn accept_loop(listener: &UnixListener, backend: &Backend, stop: &AtomicBool) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("control API: accept failed: {e}");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            let busy = ControlError::new(ErrorCode::Unavailable, "too many connections");
            let _ = write_response(&mut stream, &Response::from_result(Err(busy), None));
            continue;
        }
        let backend = backend.clone();
        let active = active.clone();
        let spawned = std::thread::Builder::new()
            .name("control-conn".into())
            .spawn(move || {
                if let Err(e) = serve_connection(stream, &backend) {
                    log::debug!("control API: connection ended: {e}");
                }
                active.fetch_sub(1, Ordering::SeqCst);
            });
        if let Err(e) = spawned {
            log::warn!("control API: cannot spawn connection thread: {e}");
        }
    }
}

/// Answers each request line with one response line until the client closes.
fn serve_connection(stream: UnixStream, backend: &Backend) -> io::Result<()> {
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let limit = u64::try_from(MAX_REQUEST_BYTES).unwrap_or(u64::MAX) + 1;
        let n = reader.by_ref().take(limit).read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(());
        }
        if buf.len() > MAX_REQUEST_BYTES {
            let error = ControlError::new(
                ErrorCode::ParseError,
                format!("request longer than {MAX_REQUEST_BYTES} bytes"),
            );
            write_response(&mut writer, &Response::from_result(Err(error), None))?;
            return Ok(());
        }
        let response = match std::str::from_utf8(&buf) {
            Ok(line) if line.trim().is_empty() => continue,
            Ok(line) => handle_line(backend, line.trim()),
            Err(_) => Response::from_result(
                Err(ControlError::new(
                    ErrorCode::ParseError,
                    "request is not UTF-8",
                )),
                None,
            ),
        };
        write_response(&mut writer, &response)?;
    }
}

fn write_response(writer: &mut impl Write, response: &Response) -> io::Result<()> {
    let mut line = serde_json::to_vec(response).map_err(io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

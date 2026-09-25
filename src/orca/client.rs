//! Runs `orca` subcommands as child processes and decodes their JSON envelope.
//!
//! Every `orca … --json` call prints `{"id","ok":true,"result":…}` or
//! `{"id","ok":false,"error":{"code","message","data"?}}` on stdout. The parse
//! step ([`interpret_output`], [`parse_envelope`]) is pure so it can be tested
//! against captured fixtures without spawning anything.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;

/// Default per-call timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Binary name looked up on `PATH` when no explicit path is configured.
pub const DEFAULT_BINARY: &str = "orca";

/// Install locations checked when `orca` isn't on `PATH`. Apps launched from
/// Finder don't inherit the shell `PATH`, so Homebrew's bin dirs are often missing.
pub const FALLBACK_PATHS: &[&str] = &["/opt/homebrew/bin/orca", "/usr/local/bin/orca"];

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const SNIPPET_LEN: usize = 200;

/// An `ok:false` response from Orca.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    /// Machine-readable code, e.g. `run_required` or `terminal_handle_stale`.
    pub code: String,
    pub message: String,
    /// `error.data` as sent (`Value::Null` when absent).
    pub data: Value,
}

/// Errors from invoking the `orca` CLI.
#[derive(Debug, thiserror::Error)]
pub enum OrcaError {
    #[error("failed to spawn {binary}: {source}")]
    Spawn {
        binary: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("orca {args} timed out after {after:?}")]
    Timeout { args: String, after: Duration },
    /// Non-zero exit without a parseable JSON envelope on stdout.
    #[error("orca exited with {status:?}: {stderr}")]
    Exit {
        status: Option<i32>,
        stderr: String,
        stdout: String,
    },
    #[error("orca error {}: {}", .0.code, .0.message)]
    Api(ApiError),
    /// Stdout wasn't a valid envelope, or `result` didn't match the expected type.
    #[error("invalid orca JSON: {source} (output: {snippet})")]
    Json {
        #[source]
        source: serde_json::Error,
        snippet: String,
    },
    #[error("orca I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl OrcaError {
    /// The Orca error code for `ok:false` responses.
    pub fn code(&self) -> Option<&str> {
        match self {
            OrcaError::Api(e) => Some(&e.code),
            _ => None,
        }
    }

    fn json(source: serde_json::Error, bytes: &[u8]) -> Self {
        OrcaError::Json {
            source,
            snippet: snippet(bytes),
        }
    }
}

/// Handle for calling the `orca` binary.
#[derive(Debug, Clone)]
pub struct OrcaClient {
    binary: PathBuf,
    timeout: Duration,
}

impl Default for OrcaClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OrcaClient {
    /// Client for `orca` resolved via `PATH`, then [`FALLBACK_PATHS`].
    pub fn new() -> Self {
        Self::with_binary(DEFAULT_BINARY)
    }

    /// Client for a specific binary. A bare name is resolved like [`OrcaClient::new`];
    /// anything containing a path separator is used as is.
    pub fn with_binary(binary: impl AsRef<Path>) -> Self {
        let path = std::env::var_os("PATH");
        Self {
            binary: resolve_binary(binary.as_ref(), path.as_deref(), FALLBACK_PATHS),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn call_timeout(&self) -> Duration {
        self.timeout
    }

    /// Runs `orca <args> --json` and returns the envelope's `result`.
    pub fn call_value<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Value, OrcaError> {
        let out = self.run(args)?;
        interpret_output(out.status.code(), &out.stdout, &out.stderr)
    }

    /// Runs `orca <args> --json` and decodes `result` into `T`.
    pub fn call<T: DeserializeOwned, S: AsRef<OsStr>>(&self, args: &[S]) -> Result<T, OrcaError> {
        let out = self.run(args)?;
        let result = interpret_output(out.status.code(), &out.stdout, &out.stderr)?;
        serde_json::from_value(result).map_err(|e| OrcaError::json(e, &out.stdout))
    }

    fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<RawOutput, OrcaError> {
        let mut child = Command::new(&self.binary)
            .args(args)
            .arg("--json")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| OrcaError::Spawn {
                binary: self.binary.clone(),
                source,
            })?;

        // Drain both pipes on their own threads so a large output can't
        // deadlock against the child blocking on a full pipe.
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());

        let status = match wait_with_deadline(&mut child, self.timeout)? {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(OrcaError::Timeout {
                    args: describe_args(args),
                    after: self.timeout,
                });
            }
        };

        Ok(RawOutput {
            status,
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
    }
}

struct RawOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

/// Waits for the child to exit, returning `None` if `timeout` elapses first.
fn wait_with_deadline(child: &mut Child, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn describe_args<S: AsRef<OsStr>>(args: &[S]) -> String {
    args.iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn snippet(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    match text.char_indices().nth(SNIPPET_LEN) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

/// Resolves the binary to spawn. A bare name is searched on `path_var`, then in
/// `fallbacks`; if nothing exists the bare name is returned so spawning reports it.
pub fn resolve_binary(binary: &Path, path_var: Option<&OsStr>, fallbacks: &[&str]) -> PathBuf {
    if binary.components().count() != 1 || binary.is_absolute() {
        return binary.to_path_buf();
    }
    if let Some(path_var) = path_var {
        for dir in std::env::split_paths(path_var) {
            let candidate = dir.join(binary);
            if is_executable(&candidate) {
                return candidate;
            }
        }
    }
    let name = binary.file_name();
    fallbacks
        .iter()
        .map(PathBuf::from)
        .find(|p| p.file_name() == name && is_executable(p))
        .unwrap_or_else(|| binary.to_path_buf())
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Decodes a finished `orca` process: the JSON envelope on stdout wins over the
/// exit code (Orca exits 1 on `ok:false`), and a non-zero exit without an
/// envelope becomes [`OrcaError::Exit`].
pub fn interpret_output(
    status: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<Value, OrcaError> {
    match parse_envelope(stdout) {
        Err(OrcaError::Json { .. }) if status != Some(0) => Err(OrcaError::Exit {
            status,
            stderr: snippet(stderr),
            stdout: snippet(stdout),
        }),
        other => other,
    }
}

/// Parses an `orca --json` envelope, returning `result` on `ok:true` and
/// [`OrcaError::Api`] on `ok:false`.
pub fn parse_envelope(stdout: &[u8]) -> Result<Value, OrcaError> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        ok: bool,
        #[serde(default)]
        result: Value,
        #[serde(default)]
        error: Option<RawError>,
    }
    #[derive(serde::Deserialize)]
    struct RawError {
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        data: Value,
    }

    let env: Envelope = serde_json::from_slice(stdout).map_err(|e| OrcaError::json(e, stdout))?;
    if env.ok {
        return Ok(env.result);
    }
    let err = env.error.unwrap_or(RawError {
        code: None,
        message: None,
        data: Value::Null,
    });
    let code = err.code.unwrap_or_else(|| "unknown".to_string());
    Err(OrcaError::Api(ApiError {
        message: err.message.unwrap_or_else(|| code.clone()),
        code,
        data: err.data,
    }))
}

/// Decodes an envelope's `result` into `T`.
pub fn parse_result<T: DeserializeOwned>(stdout: &[u8]) -> Result<T, OrcaError> {
    let result = parse_envelope(stdout)?;
    serde_json::from_value(result).map_err(|e| OrcaError::json(e, stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_REQUIRED: &[u8] = include_bytes!("../../tests/fixtures/orca/error_run_required.json");
    const HANDLE_STALE: &[u8] =
        include_bytes!("../../tests/fixtures/orca/error_terminal_handle_stale.json");
    const INVALID_ARG: &[u8] =
        include_bytes!("../../tests/fixtures/orca/error_invalid_argument.json");
    const READ_SCREEN: &[u8] =
        include_bytes!("../../tests/fixtures/orca/terminal_read_screen.json");

    #[test]
    fn ok_envelope_returns_result() {
        let result = parse_envelope(READ_SCREEN).unwrap();
        assert_eq!(result["terminal"]["status"], "running");
    }

    #[test]
    fn error_envelope_keeps_code_message_and_data() {
        let err = parse_envelope(RUN_REQUIRED).unwrap_err();
        let OrcaError::Api(api) = &err else {
            panic!("expected Api error, got {err:?}");
        };
        assert_eq!(api.code, "run_required");
        assert!(api.message.starts_with("No Run is bound"));
        assert_eq!(api.data["effectsApplied"], false);
        assert_eq!(err.code(), Some("run_required"));
    }

    #[test]
    fn error_envelope_without_data() {
        let err = parse_envelope(HANDLE_STALE).unwrap_err();
        let OrcaError::Api(api) = err else { panic!() };
        assert_eq!(api.code, "terminal_handle_stale");
        assert_eq!(api.data, Value::Null);

        let err = parse_envelope(INVALID_ARG).unwrap_err();
        assert_eq!(err.code(), Some("invalid_argument"));
    }

    #[test]
    fn nonzero_exit_with_envelope_is_api_error() {
        let err = interpret_output(Some(1), RUN_REQUIRED, b"").unwrap_err();
        assert_eq!(err.code(), Some("run_required"));
    }

    #[test]
    fn nonzero_exit_ok_envelope_still_returns_result() {
        // `terminal wait` exits 1 with ok:true when its condition isn't met.
        let result = interpret_output(Some(1), READ_SCREEN, b"").unwrap();
        assert!(result["terminal"].is_object());
    }

    #[test]
    fn nonzero_exit_without_json_is_exit_error() {
        let err = interpret_output(Some(2), b"", b"Orca is not running\n").unwrap_err();
        match err {
            OrcaError::Exit { status, stderr, .. } => {
                assert_eq!(status, Some(2));
                assert_eq!(stderr, "Orca is not running");
            }
            other => panic!("expected Exit, got {other:?}"),
        }
        let err = interpret_output(None, b"garbage", b"").unwrap_err();
        assert!(matches!(err, OrcaError::Exit { status: None, .. }));
    }

    #[test]
    fn zero_exit_invalid_json_is_json_error() {
        let err = interpret_output(Some(0), b"Usage: orca ...", b"").unwrap_err();
        match err {
            OrcaError::Json { snippet, .. } => assert_eq!(snippet, "Usage: orca ..."),
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[test]
    fn error_envelope_missing_fields_is_lenient() {
        let err = parse_envelope(br#"{"id":"x","ok":false}"#).unwrap_err();
        assert_eq!(err.code(), Some("unknown"));
    }

    #[test]
    fn parse_result_reports_type_mismatch_as_json() {
        let err = parse_result::<Vec<String>>(READ_SCREEN).unwrap_err();
        assert!(matches!(err, OrcaError::Json { .. }));
    }

    #[test]
    fn snippet_truncates_long_output() {
        let long = "x".repeat(500);
        let s = snippet(long.as_bytes());
        assert_eq!(s.chars().count(), SNIPPET_LEN + 1);
    }

    #[cfg(unix)]
    fn make_exe(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn resolve_prefers_path_then_fallbacks() {
        let on_path = tempfile::tempdir().unwrap();
        let fallback_dir = tempfile::tempdir().unwrap();
        let path_exe = make_exe(on_path.path(), "orca");
        let fallback_exe = make_exe(fallback_dir.path(), "orca");
        let fallback = fallback_exe.to_str().unwrap();

        let path_var = std::env::join_paths([on_path.path()]).unwrap();
        let got = resolve_binary(Path::new("orca"), Some(&path_var), &[fallback]);
        assert_eq!(got, path_exe);

        let empty = tempfile::tempdir().unwrap();
        let path_var = std::env::join_paths([empty.path()]).unwrap();
        let got = resolve_binary(Path::new("orca"), Some(&path_var), &[fallback]);
        assert_eq!(got, fallback_exe);

        let got = resolve_binary(Path::new("orca"), None, &["/nonexistent/orca"]);
        assert_eq!(got, PathBuf::from("orca"));
    }

    #[test]
    fn resolve_keeps_explicit_paths() {
        let got = resolve_binary(Path::new("/some/where/orca"), None, FALLBACK_PATHS);
        assert_eq!(got, PathBuf::from("/some/where/orca"));
        let got = resolve_binary(Path::new("./orca"), None, FALLBACK_PATHS);
        assert_eq!(got, PathBuf::from("./orca"));
    }

    #[test]
    fn spawn_failure_is_spawn_error() {
        let client = OrcaClient::with_binary("/nonexistent/orca-binary");
        let err = client.call_value(&["status"]).unwrap_err();
        assert!(matches!(err, OrcaError::Spawn { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn runs_script_and_parses_stdout() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("fake-orca");
        std::fs::write(
            &exe,
            "#!/bin/sh\necho \"{\\\"id\\\":\\\"1\\\",\\\"ok\\\":true,\\\"result\\\":{\\\"args\\\":\\\"$*\\\"}}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let client = OrcaClient::with_binary(&exe);
        let result = client.call_value(&["terminal", "list"]).unwrap();
        assert_eq!(result["args"], "terminal list --json");
    }

    #[cfg(unix)]
    #[test]
    fn hung_child_times_out() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("slow-orca");
        std::fs::write(&exe, "#!/bin/sh\nexec sleep 5\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let client = OrcaClient::with_binary(&exe).timeout(Duration::from_millis(100));
        let started = Instant::now();
        let err = client.call_value(&["terminal", "list"]).unwrap_err();
        assert!(matches!(err, OrcaError::Timeout { .. }), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

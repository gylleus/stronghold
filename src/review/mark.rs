//! A "ready for review" mark: a coordinator or standalone agent saying a PR is ready,
//! with who said it. Marks reach the app over the control API (`pr_ready`), or through
//! `<data dir>/pr-ready.jsonl` when the app is not running, which it ingests on start.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::control::ControlError;
use crate::control::protocol::Params;
use crate::mcp::backend::{CallError, Control};
use crate::playbook::fetches::{ENV_HANDLE, ENV_PANE_KEY, ENV_TAB_ID};
use crate::review::pr::PrRef;

/// Marks waiting for the app, one JSON object per line.
pub const PENDING_FILE: &str = "pr-ready.jsonl";
/// The pending file is trimmed to its newest half past this size.
pub const MAX_PENDING_BYTES: u64 = 256 * 1024;
/// Longest title and summary kept.
pub const MAX_TEXT_CHARS: usize = 500;

pub const WORKER_READY_ERROR: &str = "Dispatched workers cannot mark PRs ready for Karl. Send \
    merge_ready to your coordinator instead; the coordinator must verify the gate and call \
    mark_pr_ready.";

/// The Orca identity of the agent that marked a PR, from its terminal's environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_handle: Option<String>,
    /// `<tabId>:<leafId>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_id: Option<String>,
}

impl Origin {
    /// From this process's environment (`ORCA_TERMINAL_HANDLE`, `ORCA_PANE_KEY`, `ORCA_TAB_ID`,
    /// which Orca sets in every terminal it opens), plus task/dispatch IDs when present.
    pub fn from_env() -> Self {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    /// From `var`, a lookup of environment variables.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let get = |k: &str| {
            var(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Self {
            terminal_handle: get(ENV_HANDLE),
            pane_key: get(ENV_PANE_KEY),
            tab_id: get(ENV_TAB_ID),
            task_id: get("ORCA_TASK_ID"),
            dispatch_id: get("ORCA_DISPATCH_ID"),
        }
    }

    pub fn is_dispatched_worker(&self) -> bool {
        self.task_id.is_some() || self.dispatch_id.is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.terminal_handle.is_none()
            && self.pane_key.is_none()
            && self.tab_id.is_none()
            && !self.is_dispatched_worker()
    }
}

/// Where a mark came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "via", rename_all = "snake_case")]
pub enum MarkSource {
    /// The `mark_pr_ready` MCP tool.
    Tool,
    /// `stronghold pr-ready`.
    Cli,
    /// A legacy `merge_ready` bus mark, retained for reading old queue files.
    Bus { message_id: String },
}

impl MarkSource {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Cli => "cli",
            Self::Bus { .. } => "bus",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "tool" => Some(Self::Tool),
            "cli" => Some(Self::Cli),
            _ => None,
        }
    }
}

/// One "this PR is ready for review".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyMark {
    pub pr: PrRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub origin: Origin,
    pub source: MarkSource,
    pub at_ms: i64,
}

impl ReadyMark {
    /// A mark from a tool or CLI call; `url` must be a PR URL.
    pub fn new(
        url: &str,
        title: Option<&str>,
        summary: Option<&str>,
        origin: Origin,
        source: MarkSource,
        at_ms: i64,
    ) -> Result<Self, String> {
        if origin.is_dispatched_worker() {
            return Err(WORKER_READY_ERROR.into());
        }
        let pr = PrRef::parse(url).ok_or_else(|| {
            format!("{url:?} is not a GitHub pull request URL (https://github.com/<org>/<repo>/pull/<n>)")
        })?;
        Ok(Self {
            pr,
            title: clean(title),
            summary: clean(summary),
            origin,
            source,
            at_ms,
        })
    }

    /// The control API `pr_ready` params.
    pub fn to_params(&self) -> Value {
        let mut params = json!({
            "url": self.pr.url(),
            "via": self.source.name(),
            "origin": self.origin,
        });
        if let Some(t) = &self.title {
            params["title"] = json!(t);
        }
        if let Some(s) = &self.summary {
            params["summary"] = json!(s);
        }
        params
    }

    /// Parses `pr_ready` params; the time is the app's receive time.
    pub fn from_params(params: &Value, now_ms: i64) -> Result<Self, ControlError> {
        let p = Params::new(params);
        let url = p.required_str("url")?;
        let source = match p.str("via")? {
            None => MarkSource::Tool,
            Some(v) => MarkSource::parse(v).ok_or_else(|| {
                ControlError::invalid_params(format!("unknown \"via\" {v:?}; use tool or cli"))
            })?,
        };
        let origin = match params.get("origin") {
            None | Some(Value::Null) => Origin::default(),
            Some(o) => serde_json::from_value(o.clone()).map_err(|e| {
                ControlError::invalid_params(format!("\"origin\" must be an object: {e}"))
            })?,
        };
        Self::new(
            url,
            p.str("title")?,
            p.str("summary")?,
            origin,
            source,
            now_ms,
        )
        .map_err(ControlError::invalid_params)
    }
}

fn clean(text: Option<&str>) -> Option<String> {
    let text = text?.trim();
    if text.is_empty() {
        return None;
    }
    let mut out: String = text.chars().take(MAX_TEXT_CHARS).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Sending a mark

/// Where a mark ended up.
#[derive(Debug, Clone, PartialEq)]
pub enum Submitted {
    /// The running app took it; its `pr_ready` reply.
    App(Value),
    /// The app isn't running; the mark waits in the pending file.
    Queued(PathBuf),
}

/// Sends `mark` to the running app, or appends it to `pending` when the app isn't running.
pub fn submit(
    control: &mut dyn Control,
    pending: Option<&Path>,
    mark: &ReadyMark,
) -> Result<Submitted, String> {
    match control.call("pr_ready", mark.to_params()) {
        Ok(reply) => Ok(Submitted::App(reply)),
        Err(CallError::NotRunning) => {
            let path =
                pending.ok_or("Stronghold isn't running and there is no data dir to queue in")?;
            append_pending(path, mark)
                .map_err(|e| format!("Stronghold isn't running and queuing failed: {e}"))?;
            Ok(Submitted::Queued(path.to_path_buf()))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// `<data dir>/pr-ready.jsonl`; `None` in unit tests so they never touch the real file.
pub fn default_pending_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(crate::claude::hook::data_dir().join(PENDING_FILE))
}

/// Appends `mark` as one line (the file is created 0600), trimming the file to its newest half
/// when it grows past [`MAX_PENDING_BYTES`].
pub fn append_pending(path: &Path, mark: &ReadyMark) -> io::Result<()> {
    append_pending_bounded(path, mark, MAX_PENDING_BYTES)
}

pub fn append_pending_bounded(path: &Path, mark: &ReadyMark, max_bytes: u64) -> io::Result<()> {
    append_jsonl(path, mark, max_bytes)
}

/// Appends `item` as one JSON line to `path` (created 0600), trimming the file to its newest
/// half when it grows past `max_bytes`. Shared by the files agents queue into while the app is
/// down.
pub fn append_jsonl<T: Serialize>(path: &Path, item: &T, max_bytes: u64) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(item).map_err(io::Error::other)?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    if file.metadata()?.len() > max_bytes {
        keep_newest(path, max_bytes / 2)?;
    }
    Ok(())
}

/// Rewrites `path` keeping the newest whole lines that fit in `keep_bytes`.
fn keep_newest(path: &Path, keep_bytes: u64) -> io::Result<()> {
    let text = fs::read_to_string(path)?;
    let mut kept: Vec<&str> = Vec::new();
    let mut size = 0u64;
    for line in text.lines().rev() {
        size += line.len() as u64 + 1;
        if size > keep_bytes {
            break;
        }
        kept.push(line);
    }
    kept.reverse();
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut out = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        for line in kept {
            out.write_all(line.as_bytes())?;
            out.write_all(b"\n")?;
        }
    }
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, path)
}

/// Takes every pending mark (oldest first) and removes the file. Malformed lines are skipped.
/// The file is renamed before it is read, so a mark appended meanwhile starts a new file.
pub fn take_pending(path: &Path) -> Vec<ReadyMark> {
    take_jsonl(path)
}

/// Takes every line of `path` (oldest first) and removes the file, skipping malformed lines.
/// The file is renamed before it is read, so a line appended meanwhile starts a new file.
pub fn take_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    let taking = path.with_extension("jsonl.taking");
    if let Err(e) = fs::rename(path, &taking) {
        if e.kind() != io::ErrorKind::NotFound {
            log::warn!("cannot take {}: {e}", path.display());
        }
        return Vec::new();
    }
    let text = fs::read_to_string(&taking).unwrap_or_default();
    let _ = fs::remove_file(&taking);
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            serde_json::from_str(l)
                .inspect_err(|e| log::warn!("skipping a line of {}: {e}", path.display()))
                .ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mark(n: u64, at_ms: i64) -> ReadyMark {
        ReadyMark::new(
            &format!("https://github.com/depict-org/realtime/pull/{n}"),
            Some("recs: gate legacy config retirement"),
            None,
            Origin {
                terminal_handle: Some("term_a".into()),
                ..Origin::default()
            },
            MarkSource::Cli,
            at_ms,
        )
        .unwrap()
    }

    #[test]
    fn origin_reads_orca_vars_and_ignores_blank_ones() {
        let env: HashMap<&str, &str> = [
            ("ORCA_TERMINAL_HANDLE", "term_abc"),
            ("ORCA_PANE_KEY", "tab1:leaf1"),
            ("ORCA_TAB_ID", "  "),
            ("ORCA_TASK_ID", "  "),
            ("ORCA_DISPATCH_ID", ""),
        ]
        .into();
        let origin = Origin::from_vars(|k| env.get(k).map(|v| (*v).to_owned()));
        assert_eq!(
            origin,
            Origin {
                terminal_handle: Some("term_abc".into()),
                pane_key: Some("tab1:leaf1".into()),
                ..Origin::default()
            }
        );
        assert!(Origin::from_vars(|_| None).is_empty());
    }

    #[test]
    fn control_params_refuse_task_and_dispatch_identities() {
        for field in ["task_id", "dispatch_id"] {
            for via in ["tool", "cli"] {
                let params = json!({
                    "url": "https://github.com/o/r/pull/1",
                    "via": via,
                    "origin": {field: "worker-id"},
                });
                let error = ReadyMark::from_params(&params, 0).unwrap_err();
                assert_eq!(error.message, WORKER_READY_ERROR);
            }
        }
    }

    #[test]
    fn marks_round_trip_through_control_params() {
        let m = ReadyMark::new(
            "github.com/o/r/pull/3/files",
            Some("  title  "),
            Some(""),
            Origin {
                pane_key: Some("t:l".into()),
                ..Origin::default()
            },
            MarkSource::Cli,
            5,
        )
        .unwrap();
        assert_eq!(m.pr.url(), "https://github.com/o/r/pull/3");
        assert_eq!(m.title.as_deref(), Some("title"));
        assert_eq!(m.summary, None);
        let params = m.to_params();
        assert_eq!(params["via"], "cli");
        assert_eq!(params["origin"], json!({"pane_key": "t:l"}));
        let back = ReadyMark::from_params(&params, 5).unwrap();
        assert_eq!(back, m);

        let bad = ReadyMark::from_params(&json!({"url": "https://example.com"}), 0).unwrap_err();
        assert!(bad.message.contains("not a GitHub pull request URL"));
        assert!(ReadyMark::from_params(&json!({}), 0).is_err());
        assert!(
            ReadyMark::from_params(&json!({"url": "github.com/o/r/pull/1", "via": "bus"}), 0)
                .is_err()
        );
        let long = "x".repeat(MAX_TEXT_CHARS + 10);
        let m = ReadyMark::new(
            "github.com/o/r/pull/1",
            Some(&long),
            None,
            Origin::default(),
            MarkSource::Tool,
            0,
        )
        .unwrap();
        assert_eq!(m.title.unwrap().chars().count(), MAX_TEXT_CHARS + 1);
    }

    #[test]
    fn pending_file_is_private_bounded_and_taken_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(PENDING_FILE);
        append_pending(&path, &mark(1, 1)).unwrap();
        append_pending(&path, &mark(2, 2)).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"not json\n")
            .unwrap();
        let taken = take_pending(&path);
        assert_eq!(taken, [mark(1, 1), mark(2, 2)]);
        assert!(!path.exists());
        assert!(take_pending(&path).is_empty());

        let one = serde_json::to_string(&mark(1, 1)).unwrap().len() as u64 + 1;
        for i in 0..10 {
            append_pending_bounded(&path, &mark(i + 1, i as i64), one * 4).unwrap();
        }
        let kept = take_pending(&path);
        assert!(kept.len() <= 4 && !kept.is_empty(), "{}", kept.len());
        assert_eq!(kept.last().unwrap().pr.number, 10, "the newest survive");
    }
}

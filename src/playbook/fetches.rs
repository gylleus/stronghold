//! Who runs which playbook version: every playbook fetch (MCP prompt, tool, resource, CLI) is
//! appended to `<data dir>/playbook-fetches.jsonl` with the caller's Orca identity, and the app
//! maps the newest fetch per terminal to its node and flags the ones on older text.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{FleetSnapshot, NodeId, NodeKind};
use crate::playbook::{Playbook, Target, Version};

pub const FILE: &str = "playbook-fetches.jsonl";
/// The log is trimmed to its newest half once it grows past this.
pub const MAX_BYTES: u64 = 512 * 1024;

/// Orca identity env vars of the calling agent's terminal.
pub const ENV_HANDLE: &str = "ORCA_TERMINAL_HANDLE";
pub const ENV_PANE_KEY: &str = "ORCA_PANE_KEY";
pub const ENV_TAB_ID: &str = "ORCA_TAB_ID";

/// One served playbook text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchRecord {
    pub at_ms: i64,
    /// `prompt`, `tool`, `resource` or `cli`, with the name: `tool:get_playbook`.
    pub via: String,
    /// The role or file served (a template's file for a filled template).
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    /// Hash of the served text: compared with what the same target serves now.
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}

/// The calling agent's Orca identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caller {
    pub terminal_handle: Option<String>,
    pub pane_key: Option<String>,
    pub tab_id: Option<String>,
}

impl Caller {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Self {
            terminal_handle: var(ENV_HANDLE),
            pane_key: var(ENV_PANE_KEY),
            tab_id: var(ENV_TAB_ID),
        }
    }
}

impl FetchRecord {
    pub fn new(
        via: &str,
        target: &Target,
        section: Option<&str>,
        version: &Version,
        caller: Caller,
        at_ms: i64,
    ) -> Self {
        Self {
            at_ms,
            via: via.to_owned(),
            target: target.to_string(),
            section: section.map(str::to_owned),
            commit: version.commit.clone(),
            dirty: version.dirty,
            hash: version.hash.clone(),
            terminal_handle: caller.terminal_handle,
            pane_key: caller.pane_key,
            tab_id: caller.tab_id,
        }
    }

    /// `1b0b685`, `1b0b685+dirty`, or the hash when there is no commit.
    pub fn version_label(&self) -> String {
        match &self.commit {
            Some(c) if self.dirty => format!("{c}+dirty"),
            Some(c) => c.clone(),
            None => format!("hash {}", self.hash),
        }
    }
}

/// `<data dir>/playbook-fetches.jsonl`; `None` in unit tests so they never touch the real log.
pub fn default_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(crate::claude::hook::data_dir().join(FILE))
}

/// Logs one fetch to `path` (when set) with the caller from the env; failures only log.
pub fn record(
    path: Option<&Path>,
    via: &str,
    target: &Target,
    section: Option<&str>,
    version: &Version,
) {
    let Some(path) = path else { return };
    let rec = FetchRecord::new(
        via,
        target,
        section,
        version,
        Caller::from_env(),
        crate::control::server::now_ms(),
    );
    if let Err(e) = append(path, &rec) {
        log::warn!("playbook fetch log {}: {e}", path.display());
    }
}

/// Appends `rec` as one line (creating the file 0600), then trims the log to its newest half
/// when it is over [`MAX_BYTES`].
pub fn append(path: &Path, rec: &FetchRecord) -> io::Result<()> {
    append_bounded(path, rec, MAX_BYTES)
}

pub fn append_bounded(path: &Path, rec: &FetchRecord, max_bytes: u64) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(rec).map_err(io::Error::other)?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    if file.metadata()?.len() > max_bytes {
        trim(path, max_bytes / 2)?;
    }
    Ok(())
}

/// Rewrites the log keeping the newest whole lines that fit in `keep_bytes`.
fn trim(path: &Path, keep_bytes: u64) -> io::Result<()> {
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

/// Every well-formed record, oldest first; a missing file is empty.
pub fn read(path: &Path) -> Vec<FetchRecord> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Staleness

/// The newest fetch of one terminal and whether its text has changed since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFetch {
    pub handle: String,
    pub record: FetchRecord,
    /// The same target now serves different text (or no longer exists).
    pub stale: bool,
}

/// What the app's background reader hands the UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchSummary {
    /// The playbook's current version, when it could be read.
    pub current: Option<Version>,
    /// Newest fetch per terminal handle, by handle.
    pub terminals: Vec<TerminalFetch>,
}

/// The newest record per terminal handle (records without one are skipped), with staleness
/// from `current_hash(target, section)`: `None` means the target no longer resolves.
pub fn summarize(
    records: &[FetchRecord],
    current: Option<Version>,
    mut current_hash: impl FnMut(&str, Option<&str>) -> Option<String>,
) -> FetchSummary {
    let mut newest: BTreeMap<&str, &FetchRecord> = BTreeMap::new();
    for r in records {
        let Some(h) = r.terminal_handle.as_deref() else {
            continue;
        };
        if newest.get(h).is_none_or(|old| old.at_ms <= r.at_ms) {
            newest.insert(h, r);
        }
    }
    let mut hashes: BTreeMap<(String, Option<String>), Option<String>> = BTreeMap::new();
    let terminals = newest
        .into_iter()
        .map(|(handle, r)| {
            let now = hashes
                .entry((r.target.clone(), r.section.clone()))
                .or_insert_with(|| current_hash(&r.target, r.section.as_deref()));
            TerminalFetch {
                handle: handle.to_owned(),
                stale: now.as_deref() != Some(r.hash.as_str()),
                record: r.clone(),
            }
        })
        .collect();
    FetchSummary { current, terminals }
}

/// Reads the log at `path` and checks each newest fetch against `playbook` now. Runs git once.
pub fn load_summary(path: &Path, playbook: Option<&Playbook>) -> FetchSummary {
    let records = read(path);
    let current = playbook.map(|p| p.version(""));
    summarize(&records, current, |target, section| {
        playbook?.current_hash(&Target::parse(target), section).ok()
    })
}

/// A fetch mapped to its node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFetch {
    pub node: NodeId,
    pub label: String,
    pub kind: NodeKind,
    pub fetch: TerminalFetch,
}

/// The fetches of terminals that are nodes of `snapshot`, coordinators first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaybookStatus {
    pub current: Option<Version>,
    pub nodes: Vec<NodeFetch>,
}

impl PlaybookStatus {
    pub fn map(summary: &FetchSummary, snapshot: &FleetSnapshot) -> Self {
        let mut nodes: Vec<NodeFetch> = summary
            .terminals
            .iter()
            .filter_map(|f| {
                let node = snapshot
                    .agent_nodes()
                    .filter(|n| n.kind != NodeKind::Subagent)
                    .filter(|n| n.terminal_handle.as_deref() == Some(f.handle.as_str()))
                    .max_by_key(|n| (n.live, n.kind == NodeKind::Coordinator))?;
                Some(NodeFetch {
                    node: node.id.clone(),
                    label: node.label.clone(),
                    kind: node.kind,
                    fetch: f.clone(),
                })
            })
            .collect();
        nodes.sort_by(|a, b| {
            (a.kind != NodeKind::Coordinator, &a.label)
                .cmp(&(b.kind != NodeKind::Coordinator, &b.label))
        });
        Self {
            current: summary.current.clone(),
            nodes,
        }
    }

    pub fn stale(&self) -> impl Iterator<Item = &NodeFetch> {
        self.nodes.iter().filter(|n| n.fetch.stale)
    }

    /// `Playbook 1b0b685 · 3 coordinators current · 1 on an older version`.
    pub fn line(&self) -> String {
        let version = self
            .view()
            .current
            .unwrap_or_else(|| "unavailable".to_owned());
        let current: Vec<&NodeFetch> = self.nodes.iter().filter(|n| !n.fetch.stale).collect();
        let stale = self.nodes.len() - current.len();
        let mut out = format!("Playbook {version}");
        if self.nodes.is_empty() {
            out.push_str(" · no fetches from fleet agents yet");
            return out;
        }
        let noun = if current.iter().all(|n| n.kind == NodeKind::Coordinator) {
            "coordinator"
        } else {
            "agent"
        };
        out.push_str(&format!(
            " · {} {noun}{} current",
            current.len(),
            if current.len() == 1 { "" } else { "s" }
        ));
        if stale > 0 {
            out.push_str(&format!(" · {stale} on an older version"));
        }
        out
    }
}

/// The control API's `playbook` status: the current version and each node's newest fetch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybookView {
    /// Current commit (`+dirty` when edited), `None` before the app first read the playbook.
    pub current: Option<String>,
    pub current_hash: Option<String>,
    pub nodes: Vec<PlaybookNodeView>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybookNodeView {
    pub node: String,
    pub label: String,
    pub kind: String,
    pub terminal_handle: String,
    pub at_ms: i64,
    pub via: String,
    pub target: String,
    pub section: Option<String>,
    /// The fetched version: commit (`+dirty`) or hash.
    pub version: String,
    pub hash: String,
    /// The same target serves different text now.
    pub stale: bool,
}

impl PlaybookStatus {
    pub fn view(&self) -> PlaybookView {
        PlaybookView {
            current: self.current.as_ref().map(|v| match (&v.commit, v.dirty) {
                (Some(c), false) => c.clone(),
                (Some(c), true) => format!("{c}+dirty"),
                (None, _) => "uncommitted".to_owned(),
            }),
            current_hash: self.current.as_ref().map(|v| v.hash.clone()),
            nodes: self
                .nodes
                .iter()
                .map(|n| PlaybookNodeView {
                    node: n.node.as_str().to_owned(),
                    label: n.label.clone(),
                    kind: crate::control::view::kind_name(n.kind).to_owned(),
                    terminal_handle: n.fetch.handle.clone(),
                    at_ms: n.fetch.record.at_ms,
                    via: n.fetch.record.via.clone(),
                    target: n.fetch.record.target.clone(),
                    section: n.fetch.record.section.clone(),
                    version: n.fetch.record.version_label(),
                    hash: n.fetch.record.hash.clone(),
                    stale: n.fetch.stale,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::model::FleetSnapshot;
    use crate::playbook::fill::tests::{COORD, W1, W2, fixture_fleet};

    fn version(commit: &str, hash: &str) -> Version {
        Version {
            commit: Some(commit.into()),
            dirty: false,
            in_git: true,
            hash: hash.into(),
        }
    }

    fn rec(handle: Option<&str>, target: &str, hash: &str, at_ms: i64) -> FetchRecord {
        FetchRecord::new(
            "prompt:coordinator",
            &Target::parse(target),
            None,
            &version("1809c8a", hash),
            Caller {
                terminal_handle: handle.map(str::to_owned),
                pane_key: Some("tab:leaf".into()),
                tab_id: None,
            },
            at_ms,
        )
    }

    #[test]
    fn records_append_as_private_json_lines_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(FILE);
        let a = rec(Some(COORD), "coordinator", "aaaaaaa", 1);
        let b = FetchRecord {
            section: Some("Topology".into()),
            ..rec(None, "KICKOFF-TEMPLATE.md", "bbbbbbb", 2)
        };
        append(&path, &a).unwrap();
        append(&path, &b).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let mut text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(
            text.lines()
                .next()
                .unwrap()
                .contains("\"terminal_handle\":\"term_coord\"")
        );
        assert!(!text.lines().nth(1).unwrap().contains("terminal_handle"));
        text.push_str("not json\n{\"at_ms\": 3}\n");
        fs::write(&path, text).unwrap();
        assert_eq!(read(&path), [a, b]);
        assert!(read(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn the_log_is_trimmed_to_its_newest_half() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let line_len = serde_json::to_string(&rec(Some(W1), "worker", "h", 0))
            .unwrap()
            .len() as u64;
        let max = line_len * 10;
        for i in 0..100 {
            append_bounded(&path, &rec(Some(W1), "worker", "h", i), max).unwrap();
            assert!(fs::metadata(&path).unwrap().len() <= max);
        }
        let records = read(&path);
        assert!(records.len() >= 4, "{}", records.len());
        assert_eq!(records.last().unwrap().at_ms, 99);
        let at: Vec<i64> = records.iter().map(|r| r.at_ms).collect();
        assert!(
            at.windows(2).all(|w| w[1] == w[0] + 1),
            "contiguous newest: {at:?}"
        );
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn summary_takes_the_newest_fetch_per_terminal_and_flags_changed_text() {
        let records = [
            rec(Some(COORD), "coordinator", "old", 1),
            rec(Some(COORD), "coordinator", "new", 5),
            rec(Some(W1), "worker", "old", 2),
            rec(Some(W2), "gone.md", "x", 3),
            rec(None, "coordinator", "old", 9),
        ];
        let mut asked = Vec::new();
        let s = summarize(&records, Some(version("1b0b685", "all")), |t, _| {
            asked.push(t.to_owned());
            (t != "gone.md").then(|| "new".to_owned())
        });
        let by: BTreeMap<&str, (i64, bool)> = s
            .terminals
            .iter()
            .map(|t| (t.handle.as_str(), (t.record.at_ms, t.stale)))
            .collect();
        assert_eq!(by[COORD], (5, false));
        assert_eq!(by[W1], (2, true));
        assert_eq!(
            by[W2],
            (3, true),
            "a target that no longer resolves is stale"
        );
        assert_eq!(by.len(), 3, "no-handle fetches are skipped");
        assert_eq!(asked.len(), 3, "one hash per distinct target");
    }

    #[test]
    fn load_summary_compares_with_what_the_playbook_serves_now() {
        let dir = tempfile::tempdir().unwrap();
        let pb = Playbook::at(dir.path());
        fs::write(
            dir.path().join(crate::playbook::PROTOCOL),
            "# P\n\n## A\n\nv1\n",
        )
        .unwrap();
        let v1 = pb.get(&Target::parse("worker"), None).unwrap().version;
        let log = dir.path().join(FILE);
        let caller = |h: &str| Caller {
            terminal_handle: Some(h.into()),
            ..Caller::default()
        };
        let now = FetchRecord::new("tool", &Target::parse("worker"), None, &v1, caller(W1), 1);
        append(&log, &now).unwrap();
        let s = load_summary(&log, Some(&pb));
        assert!(!s.terminals[0].stale);
        assert_eq!(s.current.as_ref().unwrap().commit, None);

        fs::write(
            dir.path().join(crate::playbook::PROTOCOL),
            "# P\n\n## A\n\nv2\n",
        )
        .unwrap();
        assert!(load_summary(&log, Some(&pb)).terminals[0].stale);
        assert!(load_summary(&log, None).terminals[0].stale);
    }

    #[test]
    fn status_maps_fetches_to_nodes_and_counts_the_stale_ones() {
        let data = fixture_fleet();
        let snapshot = FleetSnapshot::build(&data, data.polled_at_ms);
        let summary = FetchSummary {
            current: Some(version("1b0b685", "all")),
            terminals: vec![
                TerminalFetch {
                    handle: COORD.into(),
                    record: rec(Some(COORD), "coordinator", "h", 1),
                    stale: false,
                },
                TerminalFetch {
                    handle: W1.into(),
                    record: rec(Some(W1), "worker", "h", 2),
                    stale: true,
                },
                TerminalFetch {
                    handle: "term_elsewhere".into(),
                    record: rec(Some("term_elsewhere"), "worker", "h", 3),
                    stale: true,
                },
            ],
        };
        let status = PlaybookStatus::map(&summary, &snapshot);
        assert_eq!(status.nodes.len(), 2, "unknown handles are dropped");
        assert_eq!(status.nodes[0].kind, NodeKind::Coordinator);
        assert_eq!(status.nodes[0].node, NodeId::terminal(COORD));
        assert_eq!(status.nodes[1].fetch.handle, W1);
        assert_ne!(status.nodes[1].kind, NodeKind::Subagent);
        assert_eq!(
            status.line(),
            "Playbook 1b0b685 · 1 coordinator current · 1 on an older version"
        );
        let stale: Vec<_> = status.stale().map(|n| n.fetch.handle.as_str()).collect();
        assert_eq!(stale, [W1]);

        let view = status.view();
        assert_eq!(view.current.as_deref(), Some("1b0b685"));
        assert_eq!(view.nodes[1].version, "1809c8a");
        assert!(view.nodes[1].stale);
        assert_eq!(view.nodes[0].kind, "coordinator");
        crate::ui::playbook::draw_headless(&status);

        let empty = PlaybookStatus::map(&FetchSummary::default(), &snapshot);
        assert_eq!(
            empty.line(),
            "Playbook unavailable · no fetches from fleet agents yet"
        );
        let both_current = PlaybookStatus {
            nodes: status
                .nodes
                .iter()
                .cloned()
                .map(|mut n| {
                    n.fetch.stale = false;
                    n
                })
                .collect(),
            ..status.clone()
        };
        assert_eq!(both_current.line(), "Playbook 1b0b685 · 2 agents current");
    }

    #[test]
    fn unit_tests_never_log_to_the_real_data_dir() {
        assert_eq!(default_path(), None);
    }
}

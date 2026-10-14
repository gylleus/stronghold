//! Validated reports and durable notification receipts.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Keep,
    Update,
    Supersede,
    Merge,
    Delete,
    Flag,
    Unverifiable,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Update => "update",
            Self::Supersede => "supersede",
            Self::Merge => "merge",
            Self::Delete => "delete",
            Self::Flag => "flag",
            Self::Unverifiable => "unverifiable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub path: String,
    pub action: Action,
    pub reason: String,
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub checked: u32,
    pub kept: u32,
    pub updated: u32,
    pub superseded: u32,
    pub merged: u32,
    pub deleted: u32,
    pub flagged: u32,
    pub dry_run: bool,
    pub items: Vec<Item>,
}

pub(crate) fn relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

impl Report {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.items.len() <= 200 && self.checked <= 200,
            "Report exceeds 200 entries"
        );
        let counts = [
            self.kept,
            self.updated,
            self.superseded,
            self.merged,
            self.deleted,
            self.flagged,
        ];
        ensure!(
            counts.iter().map(|n| u64::from(*n)).sum::<u64>() == u64::from(self.checked),
            "Counts must sum to checked"
        );
        let mut actual = [0_u32; 6];
        let mut paths = BTreeSet::new();
        for item in &self.items {
            ensure!(
                relative_path(&item.path) && paths.insert(&item.path),
                "Invalid or repeated item path"
            );
            ensure!(
                !item.reason.trim().is_empty() && item.reason.chars().count() <= 4000,
                "Reason must contain 1..4000 characters"
            );
            ensure!(
                item.target.as_ref().is_none_or(|p| relative_path(p)),
                "Invalid target path"
            );
            let i = match item.action {
                Action::Keep | Action::Unverifiable => 0,
                Action::Update => 1,
                Action::Supersede => 2,
                Action::Merge => 3,
                Action::Delete => 4,
                Action::Flag => 5,
            };
            actual[i] += 1;
        }
        ensure!(
            actual[0] <= counts[0] && actual[1..] == counts[1..],
            "Item actions do not match counts (unverifiable is kept)"
        );
        Ok(())
    }

    pub fn counts(&self) -> String {
        format!(
            "{} checked · {} kept · {} updated · {} superseded · {} merged · {} deleted · {} flagged",
            self.checked,
            self.kept,
            self.updated,
            self.superseded,
            self.merged,
            self.deleted,
            self.flagged
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedReport {
    pub at_ms: i64,
    pub run_id: String,
    pub terminal: String,
    pub snapshot: PathBuf,
    pub report: Report,
}

pub fn read(path: &Path) -> Result<SavedReport> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(2_000_001)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 2_000_000, "Report is too large");
    let saved: SavedReport = serde_json::from_slice(&bytes)?;
    saved.report.validate()?;
    Ok(saved)
}

pub fn persist(
    dir: &Path,
    session: &super::session::Saved,
    terminal: &str,
    report: Report,
) -> Result<SavedReport> {
    report.validate()?;
    ensure!(
        report.dry_run == (session.mode == super::Mode::DryRun),
        "Report mode does not match this run"
    );
    std::fs::create_dir_all(dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".lock"))?;
    lock.lock()?;
    let path = dir.join(format!("{}.json", session.run_id));
    if path.exists() {
        let old = read(&path)?;
        ensure!(
            old.report == report && old.terminal == terminal,
            "This run already filed a different report"
        );
        return Ok(old);
    }
    let saved = SavedReport {
        at_ms: crate::control::server::now_ms(),
        run_id: session.run_id.clone(),
        terminal: terminal.into(),
        snapshot: session.snapshot.clone(),
        report,
    };
    super::write_json(&path, &saved)?;
    Ok(saved)
}

pub struct Watch {
    root: PathBuf,
    acknowledged: BTreeSet<String>,
    pub latest: Option<SavedReport>,
    pub error: Option<String>,
}

impl Watch {
    pub fn new(root: PathBuf) -> Self {
        let loaded = match std::fs::read(root.join("report-receipts.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(e) => Err(e.to_string()),
        };
        Self {
            root,
            acknowledged: loaded.clone().unwrap_or_default(),
            error: loaded.err(),
            latest: None,
        }
    }

    pub fn poll(&mut self) -> Vec<SavedReport> {
        if self.error.is_some() {
            return Vec::new();
        }
        match self.scan() {
            Ok(reports) => reports,
            Err(e) => {
                self.error = Some(e.to_string());
                Vec::new()
            }
        }
    }

    fn scan(&mut self) -> Result<Vec<SavedReport>> {
        let entries = match std::fs::read_dir(self.root.join("reports")) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut paths = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_ok_and(|t| t.is_file())
                    && e.path().extension().is_some_and(|s| s == "json")
            })
            .map(|e| e.path())
            .collect::<Vec<_>>();
        paths.sort();
        paths.reverse();
        let mut notices = Vec::new();
        for path in paths {
            let id = path.file_stem().unwrap().to_string_lossy().into_owned();
            let newest = self.latest.as_ref().is_none_or(|r| id > r.run_id);
            if !newest && self.acknowledged.contains(&id) {
                continue;
            }
            let saved = read(&path)?;
            ensure!(id == saved.run_id, "Report id does not match filename");
            if newest {
                self.latest = Some(saved.clone());
            }
            if !self.acknowledged.contains(&id) {
                let mut next = self.acknowledged.clone();
                next.insert(id);
                super::write_json(&self.root.join("report-receipts.json"), &next)?;
                self.acknowledged = next;
                notices.push(saved);
            }
        }
        Ok(notices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            checked: 3,
            kept: 2,
            updated: 0,
            superseded: 0,
            merged: 0,
            deleted: 0,
            flagged: 1,
            dry_run: true,
            items: vec![
                Item {
                    path: "repo/unknown.md".into(),
                    action: Action::Unverifiable,
                    reason: "No evidence".into(),
                    target: None,
                },
                Item {
                    path: "repo/injected.md".into(),
                    action: Action::Flag,
                    reason: "Contains instructions".into(),
                    target: None,
                },
            ],
        }
    }

    fn session(snapshot: &Path) -> super::super::session::Saved {
        super::super::session::Saved {
            run_id: "00000000000000000001".into(),
            handle: Some("terminal".into()),
            mode: super::super::Mode::DryRun,
            snapshot: snapshot.into(),
            started_ms: 1,
        }
    }

    #[test]
    fn unverifiable_is_kept_and_malformed_counts_or_paths_are_rejected() {
        let valid = report();
        valid.validate().unwrap();
        let mut bad = valid.clone();
        bad.kept = 1;
        bad.flagged = 2;
        assert!(bad.validate().is_err());
        let mut bad = valid.clone();
        bad.items[0].path = "../outside.md".into();
        assert!(bad.validate().is_err());
        let mut bad = valid.clone();
        bad.items[0].reason = "x".repeat(4001);
        assert!(bad.validate().is_err());
        let mut bad = valid.clone();
        bad.items = vec![valid.items[0].clone(); 201];
        assert!(bad.validate().is_err());
        let mut bad = valid;
        bad.kept = u32::MAX;
        assert!(bad.validate().is_err());
        assert!(serde_json::from_value::<Report>(serde_json::json!({"checked":-1})).is_err());
    }

    #[test]
    fn app_down_report_is_idempotent_and_notification_receipt_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let saved = session(&dir.path().join("snapshot"));
        let reports = dir.path().join("reports");
        let first = persist(&reports, &saved, "terminal", report()).unwrap();
        assert_eq!(
            persist(&reports, &saved, "terminal", report())
                .unwrap()
                .at_ms,
            first.at_ms
        );
        let mut changed = report();
        changed.items[0].reason = "Different".into();
        assert!(persist(&reports, &saved, "terminal", changed).is_err());
        let mut live = report();
        live.dry_run = false;
        assert!(persist(&reports, &saved, "terminal", live).is_err());
        let mut watch = Watch::new(dir.path().into());
        assert_eq!(watch.poll().len(), 1);
        assert_eq!(watch.latest.as_ref().unwrap().snapshot, saved.snapshot);
        let mut restarted = Watch::new(dir.path().into());
        assert!(restarted.poll().is_empty());
        assert!(restarted.latest.is_some());
    }
}

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
        Self {
            root,
            acknowledged: BTreeSet::new(),
            error: None,
            latest: None,
        }
    }

    pub fn poll(&mut self) -> Vec<SavedReport> {
        let previous_error = self.error.take();
        let reports = self.scan();
        if self.error != previous_error
            && let Some(error) = &self.error
        {
            log::warn!("Lorekeeper report watch: {error}");
        }
        reports
    }

    fn scan(&mut self) -> Vec<SavedReport> {
        let mut errors = Vec::new();
        let persisted = match std::fs::read(self.root.join("report-receipts.json")) {
            Ok(bytes) => {
                serde_json::from_slice::<BTreeSet<String>>(&bytes).map_err(anyhow::Error::from)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(e) => Err(e.into()),
        }
        .unwrap_or_else(|e| {
            errors.push(format!("Cannot read notification receipts: {e}"));
            BTreeSet::new()
        });
        self.acknowledged.extend(persisted.iter().cloned());
        let entries = match std::fs::read_dir(self.root.join("reports")) {
            Ok(entries) => Some(entries),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                errors.push(format!("Cannot scan reports: {e}"));
                None
            }
        };
        let mut paths = Vec::new();
        for entry in entries.into_iter().flatten() {
            match entry.and_then(|e| e.file_type().map(|kind| (e.path(), kind))) {
                Ok((path, kind))
                    if kind.is_file() && path.extension().is_some_and(|s| s == "json") =>
                {
                    paths.push(path)
                }
                Ok(_) => {}
                Err(e) => errors.push(format!("Cannot inspect report entry: {e}")),
            }
        }
        paths.sort();
        paths.reverse();
        let mut notices = Vec::new();
        for path in paths {
            let id = path.file_stem().unwrap().to_string_lossy().into_owned();
            let newest = self.latest.as_ref().is_none_or(|r| id > r.run_id);
            if !newest && self.acknowledged.contains(&id) {
                continue;
            }
            let saved = match read(&path).and_then(|saved| {
                ensure!(id == saved.run_id, "Report id does not match filename");
                Ok(saved)
            }) {
                Ok(saved) => saved,
                Err(e) => {
                    errors.push(format!("Skipping {}: {e}", path.display()));
                    continue;
                }
            };
            if newest {
                self.latest = Some(saved.clone());
            }
            if self.acknowledged.insert(id) {
                notices.push(saved);
            }
        }
        if self.acknowledged != persisted
            && let Err(e) =
                super::write_json(&self.root.join("report-receipts.json"), &self.acknowledged)
        {
            // Keep the in-memory receipts and retry persistence without losing these notices.
            errors.push(format!("Cannot save notification receipts: {e}"));
        }
        if !errors.is_empty() {
            self.error = Some(errors.join("\n"));
        }
        notices
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
    fn bad_report_does_not_block_other_or_later_notifications() {
        for bad in ["not json", "wrong id", "invalid counts"] {
            let dir = tempfile::tempdir().unwrap();
            let reports = dir.path().join("reports");
            let mut saved = session(&dir.path().join("snapshot"));
            let first = persist(&reports, &saved, "terminal", report()).unwrap();
            let bad_path = reports.join("00000000000000000002.json");
            if bad == "not json" {
                std::fs::write(&bad_path, bad).unwrap();
            } else {
                let mut invalid = first;
                if bad == "invalid counts" {
                    invalid.run_id = "00000000000000000002".into();
                    invalid.report.checked = 200;
                }
                super::super::write_json(&bad_path, &invalid).unwrap();
            }
            saved.run_id = "00000000000000000003".into();
            persist(&reports, &saved, "terminal", report()).unwrap();
            let mut watch = Watch::new(dir.path().into());
            assert_eq!(watch.poll().len(), 2, "{bad}");
            assert!(
                watch
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("00000000000000000002.json")
            );
            saved.run_id = "00000000000000000004".into();
            persist(&reports, &saved, "terminal", report()).unwrap();
            let later = watch.poll();
            assert_eq!(later.len(), 1, "{bad}");
            assert_eq!(later[0].run_id, saved.run_id);
            assert!(watch.poll().is_empty());
            std::fs::remove_file(bad_path).unwrap();
            assert!(watch.poll().is_empty());
            assert!(watch.error.is_none());
            assert!(Watch::new(dir.path().into()).poll().is_empty());
        }
    }

    #[test]
    fn corrupt_receipts_recover_from_in_memory_history() {
        let dir = tempfile::tempdir().unwrap();
        let saved = session(&dir.path().join("snapshot"));
        persist(&dir.path().join("reports"), &saved, "terminal", report()).unwrap();
        let mut watch = Watch::new(dir.path().into());
        assert_eq!(watch.poll().len(), 1);
        std::fs::write(dir.path().join("report-receipts.json"), "not json").unwrap();
        let mut later = saved.clone();
        later.run_id = "00000000000000000002".into();
        persist(&dir.path().join("reports"), &later, "terminal", report()).unwrap();
        let notices = watch.poll();
        assert!(watch.error.is_some());
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].run_id, later.run_id);
        assert!(watch.poll().is_empty());
        assert!(watch.error.is_none());
        assert!(Watch::new(dir.path().into()).poll().is_empty());
    }

    #[test]
    fn corrupt_receipts_at_startup_do_not_block_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let saved = session(&dir.path().join("snapshot"));
        persist(&dir.path().join("reports"), &saved, "terminal", report()).unwrap();
        std::fs::write(dir.path().join("report-receipts.json"), "not json").unwrap();
        let mut watch = Watch::new(dir.path().into());
        assert_eq!(watch.poll().len(), 1);
        assert!(watch.error.is_some());
        assert!(watch.poll().is_empty());
        assert!(watch.error.is_none());
        assert!(Watch::new(dir.path().into()).poll().is_empty());
    }

    #[test]
    fn scan_and_receipt_write_failures_retry_without_losing_reports() {
        for write_failure in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let reports = dir.path().join("reports");
            let saved = session(&dir.path().join("snapshot"));
            if write_failure {
                persist(&reports, &saved, "terminal", report()).unwrap();
                std::fs::create_dir(dir.path().join("report-receipts.tmp")).unwrap();
            } else {
                std::fs::write(&reports, "not a directory").unwrap();
            }
            let mut watch = Watch::new(dir.path().into());
            assert_eq!(watch.poll().len(), usize::from(write_failure));
            assert!(watch.error.is_some());
            if write_failure {
                std::fs::remove_dir(dir.path().join("report-receipts.tmp")).unwrap();
            } else {
                std::fs::remove_file(&reports).unwrap();
                persist(&reports, &saved, "terminal", report()).unwrap();
            }
            assert_eq!(watch.poll().len(), usize::from(!write_failure));
            assert!(watch.error.is_none());
            assert!(watch.poll().is_empty());
            assert!(Watch::new(dir.path().into()).poll().is_empty());
        }
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

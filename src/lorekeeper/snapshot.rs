//! Complete Ledger copies outside the indexed folder.

use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else {
            ensure!(
                kind.is_file(),
                "Cannot snapshot special file or symlink: {}",
                entry.path().display()
            );
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn snapshot_name(name: &str) -> bool {
    let (stamp, suffix) = if name.len() > 15 {
        match (name.get(..15), name.get(15..)) {
            (Some(stamp), Some(suffix)) => (stamp, suffix),
            _ => return false,
        }
    } else {
        (name, "")
    };
    stamp.len() == 15
        && stamp.as_bytes()[8] == b'-'
        && stamp
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 8 || b.is_ascii_digit())
        && (suffix.is_empty()
            || suffix
                .strip_prefix('-')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
}

pub fn create(source: &Path, archive: &Path, stamp: &str) -> Result<PathBuf> {
    ensure!(snapshot_name(stamp), "Invalid snapshot timestamp");
    ensure!(
        !std::fs::symlink_metadata(source)?.file_type().is_symlink(),
        "Ledger root cannot be a symlink"
    );
    let source = source
        .canonicalize()
        .context("Ledger folder is unavailable")?;
    ensure!(source.is_dir(), "Ledger is not a directory");
    std::fs::create_dir_all(archive)?;
    let archive = archive.canonicalize()?;
    ensure!(
        !archive.starts_with(&source) && !source.starts_with(&archive),
        "Snapshot archive overlaps Ledger"
    );
    let mut destination = archive.join(stamp);
    let mut suffix = 0;
    while destination.exists() {
        suffix += 1;
        destination = archive.join(format!("{stamp}-{suffix}"));
    }
    let staging = archive.join(format!(
        ".{}-{}",
        destination.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    copy_tree(&source, &staging).context("Ledger snapshot failed; launch aborted")?;
    std::fs::rename(&staging, &destination)?;
    let mut snapshots = Vec::new();
    for entry in std::fs::read_dir(&archive)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && snapshot_name(&entry.file_name().to_string_lossy()) {
            snapshots.push(entry.path());
        }
    }
    snapshots.sort_by_key(|p| {
        (
            std::fs::metadata(p).and_then(|m| m.modified()).ok(),
            p.clone(),
        )
    });
    for old in snapshots.iter().take(snapshots.len().saturating_sub(12)) {
        std::fs::remove_dir_all(old).context("Could not prune old Ledger snapshot")?;
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_copies_retention_and_unrelated_archives() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("ledger");
        let archive = dir.path().join("archive");
        std::fs::create_dir_all(source.join(".hidden")).unwrap();
        std::fs::write(source.join(".hidden/a.md"), "original").unwrap();
        std::fs::create_dir_all(archive.join("worktree-backup")).unwrap();
        let mut latest = PathBuf::new();
        for i in 0..14 {
            latest = create(&source, &archive, &format!("20261014-1200{i:02}")).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(latest.join(".hidden/a.md")).unwrap(),
            "original"
        );
        assert_eq!(std::fs::read_dir(&archive).unwrap().count(), 13);
        assert!(!archive.join("20261014-120000").exists());
        assert!(archive.join("worktree-backup").exists());
        assert!(create(&source, &source.join("archive"), "20261014-130000").is_err());
    }

    #[test]
    fn symlink_or_missing_source_aborts_without_publishing() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("ledger");
        let archive = dir.path().join("archive");
        assert!(create(&source, &archive, "20261014-120000").is_err());
        std::fs::create_dir(&source).unwrap();
        std::os::unix::fs::symlink("/tmp", source.join("escape")).unwrap();
        assert!(create(&source, &archive, "20261014-120000").is_err());
        assert!(!archive.join("20261014-120000").exists());
    }
}

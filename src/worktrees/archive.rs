//! Local recovery copies, verified before cleanup mutates a worktree.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

use super::{PlanEntry, safe_relative};

pub const SCRATCH_CAP: u64 = 50 * 1024 * 1024;
pub const UNTRACKED_CAP: u64 = 200 * 1024 * 1024;

fn command(dir: &Path, args: &[&str]) -> Result<Output, String> {
    let out = Command::new("git")
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(command(dir, args)?.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|e| e.to_string())
}

fn paths(bytes: &[u8]) -> Result<Vec<String>, String> {
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let s = std::str::from_utf8(p).map_err(|_| "non-UTF8 file name cannot be archived")?;
            if !safe_relative(s) {
                return Err(format!("unsafe path {s:?}"));
            }
            Ok(s.to_owned())
        })
        .collect()
}

/// Git's exact untracked paths, including ignored agent scratch contents. Ignored build
/// output elsewhere remains disposable, just as in ordinary worktree removal.
pub fn untracked(dir: &Path) -> Result<Vec<String>, String> {
    let mut files =
        paths(&command(dir, &["ls-files", "--others", "--exclude-standard", "-z"])?.stdout)?;
    files.extend(paths(
        &command(
            dir,
            &[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "-z",
                "--",
                ".orca",
                ".local",
            ],
        )?
        .stdout,
    )?);
    files.sort();
    files.dedup();
    Ok(files)
}

pub fn is_scratch(path: &str) -> bool {
    path.starts_with(".orca/") || path.starts_with(".local/") || path == ".orca" || path == ".local"
}

/// Reject links in ancestors, directory entries (including nested repositories), and devices.
/// Leaf symlinks are copied as links, never followed.
fn metadata(dir: &Path, path: &str) -> Result<fs::Metadata, String> {
    if !safe_relative(path) {
        return Err(format!("unsafe path {path:?}"));
    }
    let mut parent = dir.to_owned();
    let parts: Vec<_> = Path::new(path).components().collect();
    for part in &parts[..parts.len() - 1] {
        parent.push(part);
        let m = fs::symlink_metadata(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        if !m.is_dir() || m.file_type().is_symlink() || parent.join(".git").exists() {
            return Err(format!("unsafe parent of {path:?}"));
        }
    }
    let m = fs::symlink_metadata(dir.join(path)).map_err(|e| format!("{path:?}: {e}"))?;
    if !m.is_file() && !m.file_type().is_symlink() {
        return Err(format!(
            "cannot archive special file or nested repository {path:?}"
        ));
    }
    Ok(m)
}

pub fn check_supported(dir: &Path) -> Result<(), String> {
    let index = command(dir, &["ls-files", "--stage", "-z"])?;
    if index
        .stdout
        .split(|b| *b == 0)
        .any(|line| line.starts_with(b"160000 "))
    {
        return Err("submodules must be handled by hand".into());
    }
    let flags = command(dir, &["ls-files", "-v", "-z"])?;
    if flags.stdout.split(|b| *b == 0).any(|line| {
        line.first()
            .is_some_and(|flag| flag.is_ascii_lowercase() || *flag == b'S')
    }) {
        return Err("assume-unchanged or skip-worktree index entries can hide work; kept".into());
    }
    // Git omits FIFOs and sockets entirely. Inspect the filesystem too, skipping disposable
    // ignored directories except agent scratch, whose complete contents must be preserved.
    let ignored = paths(
        &command(
            dir,
            &[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                "-z",
            ],
        )?
        .stdout,
    )?;
    let ignored: std::collections::HashSet<_> =
        ignored.iter().filter_map(|p| p.strip_suffix('/')).collect();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in fs::read_dir(dir.join(&relative)).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = relative.join(entry.file_name());
            if path == Path::new(".git") {
                continue;
            }
            let name = path
                .to_str()
                .ok_or("non-UTF8 file name cannot be archived")?;
            if entry.file_name() == ".git" {
                return Err(format!("nested repository at {}", path.display()));
            }
            let m = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
            if m.is_dir() {
                if is_scratch(name) || !ignored.contains(name) {
                    pending.push(path);
                }
            } else if !m.is_file() && !m.file_type().is_symlink() {
                return Err(format!("special file cannot be archived: {name:?}"));
            }
        }
    }
    Ok(())
}

pub fn measure(dir: &Path, files: &[String], cap: u64) -> Result<u64, String> {
    let mut total = 0u64;
    for file in files {
        total = total
            .checked_add(metadata(dir, file)?.len())
            .ok_or("size overflow")?;
        if total > cap {
            return Err(format!("files exceed {} MB archive cap", cap / 1024 / 1024));
        }
    }
    Ok(total)
}

fn digest(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().to_vec())
}

/// Destination must be a fresh directory; no archive may overwrite an earlier recovery copy.
fn copy_files(dir: &Path, dest: &Path, files: &[String], cap: u64) -> Result<(), String> {
    measure(dir, files, cap)?;
    let mut remaining = cap;
    for file in files {
        let m = metadata(dir, file)?;
        let src = dir.join(file);
        let target = dest.join(file);
        fs::create_dir_all(target.parent().ok_or("no parent")?).map_err(|e| e.to_string())?;
        if m.file_type().is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(&src).map_err(|e| e.to_string())?, &target)
                .map_err(|e| e.to_string())?;
            #[cfg(not(unix))]
            return Err("symlink archiving requires Unix".into());
            remaining = remaining
                .checked_sub(m.len())
                .ok_or("archive cap exceeded")?;
        } else {
            let source = fs::File::open(&src).map_err(|e| e.to_string())?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| e.to_string())?;
            let copied = std::io::copy(&mut source.take(remaining + 1), &mut output)
                .map_err(|e| e.to_string())?;
            remaining = remaining
                .checked_sub(copied)
                .ok_or("archive cap exceeded while copying")?;
            output
                .set_permissions(m.permissions())
                .map_err(|e| e.to_string())?;
            output.sync_all().map_err(|e| e.to_string())?;
        }
    }
    for file in files {
        let src = dir.join(file);
        let target = dest.join(file);
        let m = metadata(dir, file)?;
        let same = if m.file_type().is_symlink() {
            fs::read_link(&src).map_err(|e| e.to_string())?
                == fs::read_link(&target).map_err(|e| e.to_string())?
        } else {
            digest(&src)? == digest(&target)?
        };
        if !same {
            return Err(format!("saved copy differs: {file:?}"));
        }
    }
    Ok(())
}

/// `archive` in the [agents dir](crate::paths::agents_dir).
pub fn root() -> Option<PathBuf> {
    crate::paths::agents_dir().map(|d| d.join("archive"))
}

fn safe_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name == "." || name == ".." {
        "worktree".into()
    } else {
        name
    }
}

pub fn destination(entry: &PlanEntry, root: &Path) -> PathBuf {
    let folder = Path::new(&entry.path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    root.join(safe_name(&entry.repo)).join(format!(
        "{}-{}",
        safe_name(&folder),
        jiff::Zoned::now().strftime("%Y%m%d-%H%M%S")
    ))
}

fn unique_dir(path: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(path.parent().ok_or("missing archive parent")?)
        .map_err(|e| e.to_string())?;
    // Never follow an existing final directory or reuse a partial archive.
    for i in 0..1000 {
        let dest = if i == 0 {
            path.to_owned()
        } else {
            path.with_file_name(format!(
                "{}-{i}",
                path.file_name().unwrap().to_string_lossy()
            ))
        };
        match fs::create_dir(&dest) {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("too many archives with the same name".into())
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    head: String,
    branch: Option<String>,
    status: Vec<u8>,
    patch: Vec<u8>,
    index_patch: Vec<u8>,
    files: Vec<String>,
}

impl Snapshot {
    fn read(dir: &Path) -> Result<Self, String> {
        check_supported(dir)?;
        Ok(Self {
            head: git(dir, &["rev-parse", "HEAD"])?,
            branch: git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok(),
            status: command(
                dir,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            )?
            .stdout,
            patch: command(
                dir,
                &[
                    "diff",
                    "--binary",
                    "--no-ext-diff",
                    "--no-textconv",
                    "HEAD",
                    "--",
                ],
            )?
            .stdout,
            index_patch: command(
                dir,
                &[
                    "diff",
                    "--cached",
                    "--binary",
                    "--no-ext-diff",
                    "--no-textconv",
                    "HEAD",
                    "--",
                ],
            )?
            .stdout,
            files: untracked(dir)?,
        })
    }
}

pub struct Archive {
    pub path: PathBuf,
    snapshot: Snapshot,
    common_dir: PathBuf,
}

fn save(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| e.to_string())?;
    if fs::read(path).map_err(|e| e.to_string())? != bytes {
        return Err("archive verification failed".into());
    }
    Ok(())
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build every recovery artifact and verify it without changing the source worktree.
pub fn prepare(
    entry: &PlanEntry,
    base: Option<&str>,
    pr: &str,
    root: &Path,
) -> Result<Archive, String> {
    let dir = Path::new(&entry.path);
    let snapshot = Snapshot::read(dir)?;
    measure(dir, &snapshot.files, UNTRACKED_CAP)?;
    let scratch: Vec<_> = snapshot
        .files
        .iter()
        .filter(|p| is_scratch(p))
        .cloned()
        .collect();
    measure(dir, &scratch, SCRATCH_CAP)?;
    let common_dir = PathBuf::from(git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let path = unique_dir(&destination(entry, root))?;
    let bundle = path.join("branch.bundle");
    let bases = super::base_branches(base, |b| {
        git(dir, &["rev-parse", "--verify", &format!("{b}^{{commit}}")]).is_ok()
    });
    let base = bases
        .first()
        .and_then(|b| git(dir, &["rev-parse", "--verify", &format!("{b}^{{commit}}")]).ok());
    let mut args = vec![
        "bundle",
        "create",
        bundle.to_str().ok_or("non-UTF8 archive path")?,
        "HEAD",
    ];
    if let Some(b) = &base {
        args.extend(["--not", b]);
    }
    if command(dir, &args).is_err() {
        // HEAD already in base produces an empty bundle; still save a full recovery copy.
        command(dir, &["bundle", "create", bundle.to_str().unwrap(), "HEAD"])?;
    }
    command(dir, &["bundle", "verify", bundle.to_str().unwrap()])?;
    for (name, patch) in [
        ("uncommitted.patch", &snapshot.patch),
        ("index.patch", &snapshot.index_patch),
    ] {
        if patch.is_empty() {
            continue;
        }
        let file = path.join(name);
        save(&file, patch)?;
        let index = path.join(format!(".{name}.index"));
        for args in [
            vec!["read-tree", "HEAD"],
            vec!["apply", "--cached", "--check", file.to_str().unwrap()],
        ] {
            let output = Command::new("git")
                .env("GIT_NO_LAZY_FETCH", "1")
                .arg("-C")
                .arg(dir)
                .env("GIT_INDEX_FILE", &index)
                .args(args)
                .output()
                .map_err(|e| e.to_string())?;
            if !output.status.success() {
                return Err(format!(
                    "patch verification failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
        fs::remove_file(index).map_err(|e| e.to_string())?;
    }
    copy_files(dir, &path.join("untracked"), &snapshot.files, UNTRACKED_CAP)?;
    let readme = format!(
        "# Worktree recovery\n\nRepo: {}\nWorktree: {}\nBranch: {}\nHEAD: {}\nBase: {}\nPR: {}\nReasons: {}\n\nRestore in the original repository (a differential bundle needs the base history):\n\n```sh\ngit fetch {} HEAD\ngit worktree add -b recovered-work {} FETCH_HEAD\ncd {}\n# Apply when the corresponding patch exists:\ngit apply {}\ngit apply --cached {}\n# Copy untracked/ into this worktree, preserving relative paths and symlinks.\n```\n\nThe worktree patch restores working files; index.patch restores the staged state separately.\nOriginal local branch and HEAD are recorded above if removal interrupted branch retention.\n",
        entry.repo,
        entry.path,
        snapshot.branch.as_deref().unwrap_or("(detached)"),
        snapshot.head,
        base.as_deref().unwrap_or("none (full bundle)"),
        pr,
        entry.verdict.reasons.join("; "),
        quote(bundle.to_str().unwrap()),
        quote("../recovered-work"),
        quote("../recovered-work"),
        quote(path.join("uncommitted.patch").to_str().unwrap()),
        quote(path.join("index.patch").to_str().unwrap())
    );
    save(&path.join("README.md"), readme.as_bytes())?;
    let archive = Archive {
        path,
        snapshot,
        common_dir,
    };
    archive.unchanged(dir)?;
    Ok(archive)
}

impl Archive {
    /// HEAD, branch, status and both patches must still match the verified archive.
    pub fn unchanged(&self, dir: &Path) -> Result<(), String> {
        if Snapshot::read(dir)? != self.snapshot {
            return Err("worktree changed while archiving; kept".into());
        }
        Ok(())
    }

    pub fn clean(&self, dir: &Path) -> Result<(), String> {
        self.unchanged(dir)?;
        command(dir, &["reset", "--hard", &self.snapshot.head])?;
        for file in &self.snapshot.files {
            fs::remove_file(dir.join(file))
                .map_err(|e| format!("delete archived {file:?}: {e}"))?;
        }
        Ok(())
    }

    /// Orca may delete a merged local branch. `git branch` refuses to replace an existing ref.
    pub fn retain_branch(&self) -> Result<(), String> {
        let Some(branch) = &self.snapshot.branch else {
            return Ok(());
        };
        let exists = Command::new("git")
            .env("GIT_NO_LAZY_FETCH", "1")
            .arg("--git-dir")
            .arg(&self.common_dir)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .status()
            .map_err(|e| e.to_string())?;
        if exists.success() {
            return Ok(());
        }
        let out = Command::new("git")
            .env("GIT_NO_LAZY_FETCH", "1")
            .arg("--git-dir")
            .arg(&self.common_dir)
            .args(["branch", branch, &self.snapshot.head])
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "branch retention: {}",
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }
}

/// Scratch goes under the ordinary notes directory in a fresh subdirectory if needed.
pub fn save_scratch(dir: &Path, dest: &Path, files: &[String]) -> Result<PathBuf, String> {
    check_supported(dir)?;
    measure(dir, files, SCRATCH_CAP)?;
    let dest = if dest.exists() {
        unique_dir(&dest.join("scratch"))?
    } else {
        unique_dir(dest)?
    };
    copy_files(dir, &dest, files, SCRATCH_CAP)?;
    for file in files {
        fs::remove_file(dir.join(file)).map_err(|e| e.to_string())?;
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktrees::git_facts;

    fn repo() -> (tempfile::TempDir, PlanEntry) {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("repo");
        fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]).unwrap();
        git(&dir, &["config", "user.email", "test@example.invalid"]).unwrap();
        git(&dir, &["config", "user.name", "Test"]).unwrap();
        fs::write(dir.join("tracked"), b"base\n").unwrap();
        git(&dir, &["add", "."]).unwrap();
        git(&dir, &["commit", "-qm", "base"]).unwrap();
        git(&dir, &["checkout", "-qb", "feature"]).unwrap();
        let entry = PlanEntry {
            repo: "test".into(),
            name: "feature".into(),
            path: dir.to_str().unwrap().into(),
            ..Default::default()
        };
        (temp, entry)
    }

    #[test]
    fn bundle_patches_and_untracked_round_trip() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        fs::write(dir.join("tracked"), b"committed\n").unwrap();
        git(dir, &["commit", "-qam", "feature"]).unwrap();
        fs::write(dir.join("tracked"), b"staged\0binary\n").unwrap();
        git(dir, &["add", "tracked"]).unwrap();
        fs::write(dir.join("tracked"), b"working\0binary\n").unwrap();
        fs::create_dir_all(dir.join(".local/evidence")).unwrap();
        fs::write(dir.join(".local/evidence/a.json"), b"evidence").unwrap();
        fs::write(dir.join("odd\nfile"), b"newline filename").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("tracked", dir.join("link")).unwrap();
        let before = Snapshot::read(dir).unwrap();
        let archived =
            prepare(&entry, Some("main"), "none", &temp.path().join("archives")).unwrap();
        assert_eq!(Snapshot::read(dir).unwrap(), before);
        // Restore from the differential bundle in a fresh repository with only base history.
        let restored = temp.path().join("restored");
        git(
            temp.path(),
            &[
                "clone",
                "-q",
                "--no-local",
                "--single-branch",
                "--branch",
                "main",
                dir.to_str().unwrap(),
                restored.to_str().unwrap(),
            ],
        )
        .unwrap();
        git(
            &restored,
            &[
                "fetch",
                archived.path.join("branch.bundle").to_str().unwrap(),
                "HEAD",
            ],
        )
        .unwrap();
        git(
            &restored,
            &["checkout", "-q", "-b", "feature", "FETCH_HEAD"],
        )
        .unwrap();
        git(
            &restored,
            &[
                "apply",
                archived.path.join("uncommitted.patch").to_str().unwrap(),
            ],
        )
        .unwrap();
        git(
            &restored,
            &[
                "apply",
                "--cached",
                archived.path.join("index.patch").to_str().unwrap(),
            ],
        )
        .unwrap();
        copy_files(
            &archived.path.join("untracked"),
            &restored,
            &before.files,
            UNTRACKED_CAP,
        )
        .unwrap();
        assert_eq!(Snapshot::read(&restored).unwrap(), before);
        assert_eq!(
            fs::read(restored.join("tracked")).unwrap(),
            b"working\0binary\n"
        );
        archived.clean(dir).unwrap();
        assert!(
            command(dir, &["status", "--porcelain"])
                .unwrap()
                .stdout
                .is_empty()
        );
    }

    #[test]
    fn failed_archive_leaves_worktree_untouched() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        fs::write(dir.join("tracked"), "dirty").unwrap();
        fs::write(dir.join("keep"), "untracked").unwrap();
        let before = Snapshot::read(dir).unwrap();
        let blocked_root = temp.path().join("file");
        fs::write(&blocked_root, "cannot create archive here").unwrap();
        assert!(prepare(&entry, None, "none", &blocked_root).is_err());
        assert_eq!(Snapshot::read(dir).unwrap(), before);
        assert_eq!(fs::read(dir.join("keep")).unwrap(), b"untracked");
    }

    #[test]
    fn change_after_archiving_refuses_cleanup() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        fs::write(dir.join("tracked"), "dirty").unwrap();
        let archived =
            prepare(&entry, Some("main"), "none", &temp.path().join("archives")).unwrap();
        fs::write(dir.join("tracked"), "new work").unwrap();
        assert!(archived.clean(dir).is_err());
        assert_eq!(fs::read(dir.join("tracked")).unwrap(), b"new work");
    }

    #[test]
    fn branch_retention_recreates_only_missing_branch() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        let archived = prepare(&entry, None, "none", &temp.path().join("archives")).unwrap();
        git(dir, &["checkout", "-q", "main"]).unwrap();
        git(dir, &["branch", "-D", "feature"]).unwrap();
        archived.retain_branch().unwrap();
        assert_eq!(
            git(dir, &["rev-parse", "feature"]).unwrap(),
            archived.snapshot.head
        );
        fs::write(dir.join("tracked"), "later").unwrap();
        git(dir, &["commit", "-qam", "later"]).unwrap();
        git(dir, &["branch", "-f", "feature", "HEAD"]).unwrap();
        let newer = git(dir, &["rev-parse", "feature"]).unwrap();
        archived.retain_branch().unwrap();
        assert_eq!(git(dir, &["rev-parse", "feature"]).unwrap(), newer);
    }

    #[test]
    fn scratch_preserves_ignored_files_and_structure() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        fs::write(dir.join(".gitignore"), ".local/\n").unwrap();
        fs::create_dir_all(dir.join(".local/nested")).unwrap();
        fs::write(dir.join(".local/nested/evidence.json"), "proof").unwrap();
        let files: Vec<_> = untracked(dir)
            .unwrap()
            .into_iter()
            .filter(|p| is_scratch(p))
            .collect();
        let dest = save_scratch(dir, &temp.path().join("notes/test/worktree"), &files).unwrap();
        assert_eq!(
            fs::read(dest.join(".local/nested/evidence.json")).unwrap(),
            b"proof"
        );
        assert!(!dir.join(".local/nested/evidence.json").exists());
    }

    #[test]
    fn scratch_over_cap_keeps_it_and_deletes_nothing() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        fs::create_dir(dir.join(".orca")).unwrap();
        let file = dir.join(".orca/large");
        fs::File::create(&file)
            .unwrap()
            .set_len(SCRATCH_CAP + 1)
            .unwrap();
        let facts = git_facts(dir, None).unwrap();
        assert!(facts.scratch_error.is_some());
        assert!(save_scratch(dir, &temp.path().join("notes"), &facts.scratch).is_err());
        assert!(file.exists());
        assert!(!temp.path().join("notes").exists());
    }

    #[test]
    fn untracked_cap_fails_before_creating_archive() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        fs::File::create(dir.join("large"))
            .unwrap()
            .set_len(UNTRACKED_CAP + 1)
            .unwrap();
        let root = temp.path().join("archives");
        assert!(prepare(&entry, None, "none", &root).is_err());
        assert!(!root.exists());
        assert!(dir.join("large").exists());
    }

    #[test]
    fn no_base_bundle_restores_in_empty_repo() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        git(dir, &["branch", "-D", "main"]).unwrap();
        let archived = prepare(&entry, None, "none", &temp.path().join("archives")).unwrap();
        let restored = temp.path().join("restore");
        fs::create_dir(&restored).unwrap();
        git(&restored, &["init", "-q"]).unwrap();
        git(
            &restored,
            &[
                "fetch",
                archived.path.join("branch.bundle").to_str().unwrap(),
                "HEAD",
            ],
        )
        .unwrap();
        git(&restored, &["checkout", "-q", "FETCH_HEAD"]).unwrap();
        assert_eq!(
            git(&restored, &["rev-parse", "HEAD^{tree}"]).unwrap(),
            git(dir, &["rev-parse", "HEAD^{tree}"]).unwrap()
        );
    }

    #[test]
    fn hidden_index_changes_are_kept() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        for flag in ["--assume-unchanged", "--skip-worktree"] {
            git(dir, &["update-index", flag, "tracked"]).unwrap();
            fs::write(dir.join("tracked"), "hidden work").unwrap();
            assert!(prepare(&entry, None, "none", &temp.path().join("archives")).is_err());
            assert!(git_facts(dir, None).is_err());
            assert_eq!(fs::read(dir.join("tracked")).unwrap(), b"hidden work");
            git(
                dir,
                &[
                    "update-index",
                    "--no-assume-unchanged",
                    "--no-skip-worktree",
                    "tracked",
                ],
            )
            .unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn special_files_hidden_by_git_are_kept_but_ignored_build_output_is_skipped() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        fs::write(dir.join(".gitignore"), ".local/\nbuild/\n").unwrap();
        fs::create_dir(dir.join(".local")).unwrap();
        assert!(
            Command::new("mkfifo")
                .arg(dir.join(".local/fifo"))
                .status()
                .unwrap()
                .success()
        );
        assert!(prepare(&entry, None, "none", &temp.path().join("archives")).is_err());
        assert!(save_scratch(dir, &temp.path().join("notes"), &[]).is_err());
        assert!(git_facts(dir, None).is_err());
        assert!(dir.join(".local/fifo").exists());
        fs::remove_file(dir.join(".local/fifo")).unwrap();
        fs::create_dir(dir.join("build")).unwrap();
        assert!(
            Command::new("mkfifo")
                .arg(dir.join("build/fifo"))
                .status()
                .unwrap()
                .success()
        );
        check_supported(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_ancestors_and_nested_repos_are_rejected() {
        let (temp, entry) = repo();
        let dir = Path::new(&entry.path);
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "external").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("alias")).unwrap();
        assert!(measure(dir, &["alias/keep".into()], UNTRACKED_CAP).is_err());
        fs::create_dir(dir.join("nested")).unwrap();
        git(&dir.join("nested"), &["init", "-q"]).unwrap();
        assert!(prepare(&entry, None, "none", &temp.path().join("archives")).is_err());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"external");
    }
}

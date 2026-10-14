//! Fixed read operations for code and pull-request evidence.

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LIMIT: usize = 256 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Lookup {
    PrView {
        repo: String,
        number: u32,
    },
    PrList {
        repo: String,
        search: String,
    },
    GitFetch {
        repo: String,
    },
    GitLog {
        repo: String,
        limit: Option<u8>,
        path: Option<String>,
    },
    GitShow {
        repo: String,
        path: String,
    },
    GitGrep {
        repo: String,
        pattern: String,
        path: Option<String>,
    },
}

fn github_repo(repo: &str) -> bool {
    repo.len() <= 200
        && repo.split_once('/').is_some_and(|(owner, name)| {
            [owner, name].iter().all(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            })
        })
}

fn checked_path(path: &str) -> Result<&str> {
    ensure!(
        super::report::relative_path(path) && !path.starts_with('-'),
        "Expected a relative repository path"
    );
    Ok(path)
}

fn repository(path: &str, roots: &[PathBuf]) -> Result<PathBuf> {
    let path = Path::new(path)
        .canonicalize()
        .context("Repository path does not exist")?;
    ensure!(
        roots
            .iter()
            .filter_map(|r| r.canonicalize().ok())
            .any(|r| path.starts_with(r)),
        "Repository must be under ~/repo or ~/orca/workspaces"
    );
    let (ok, top) = run(
        git(&path).args(["rev-parse", "--show-toplevel"]),
        Duration::from_secs(10),
    )?;
    ensure!(
        ok && Path::new(top.trim()).canonicalize()? == path,
        "Use the repository worktree root"
    );
    Ok(path)
}

fn gh() -> Command {
    Command::new(crate::orca::client::resolve_binary(
        Path::new("gh"),
        std::env::var_os("PATH").as_deref(),
        &["/opt/homebrew/bin", "/usr/local/bin"],
    ))
}

fn git(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.args([
        "--no-pager",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "maintenance.auto=false",
        "-c",
        "gc.auto=0",
        "-C",
    ])
    .arg(repo);
    cmd
}

fn default_ref(repo: &Path) -> Result<String> {
    let (ok, name) = run(
        git(repo).args(["symbolic-ref", "refs/remotes/origin/HEAD"]),
        Duration::from_secs(10),
    )?;
    let name = name.trim();
    ensure!(
        ok && name.starts_with("refs/remotes/origin/") && !name.contains(['\n', '\r', ':']),
        "origin/HEAD is unavailable; configure the repository default remote branch first"
    );
    Ok(name.to_owned())
}

pub fn execute(request: Lookup) -> Result<String> {
    let home = super::home()?;
    let roots = [home.join("repo"), home.join("orca/workspaces")];
    let mut cmd = match request {
        Lookup::PrView { repo, number } => {
            ensure!(
                github_repo(&repo) && number > 0,
                "Expected owner/name and a positive PR number"
            );
            let mut c = gh();
            c.args([
                "pr",
                "view",
                &number.to_string(),
                &format!("--repo={repo}"),
                "--json",
                "number,title,body,state,mergedAt,mergeCommit,headRefOid,baseRefName,url",
            ]);
            c
        }
        Lookup::PrList { repo, search } => {
            ensure!(
                github_repo(&repo) && search.len() <= 2000 && !search.contains('\0'),
                "Invalid repo or search"
            );
            let mut c = gh();
            c.args([
                "pr",
                "list",
                &format!("--repo={repo}"),
                &format!("--search={search}"),
                "--state=all",
                "--limit=30",
                "--json=number,title,state,mergedAt,url",
            ]);
            c
        }
        Lookup::GitFetch { repo } => {
            let repo = repository(&repo, &roots)?;
            let mut c = git(&repo);
            c.args(["fetch", "--no-recurse-submodules", "origin"]);
            c
        }
        Lookup::GitLog { repo, limit, path } => {
            let repo = repository(&repo, &roots)?;
            let limit = limit.unwrap_or(20);
            ensure!((1..=50).contains(&limit), "Log limit must be 1..50");
            let mut c = git(&repo);
            c.args([
                "log",
                "--no-ext-diff",
                "--no-textconv",
                "--format=%h %ad %s",
                "--date=iso-strict",
                &format!("-n{limit}"),
                &default_ref(&repo)?,
            ]);
            c.arg("--");
            if let Some(path) = path {
                c.arg(checked_path(&path)?);
            }
            c
        }
        Lookup::GitShow { repo, path } => {
            checked_path(&path)?;
            let repo = repository(&repo, &roots)?;
            let mut c = git(&repo);
            c.args([
                "show",
                "--no-ext-diff",
                "--no-textconv",
                &format!("{}:{path}", default_ref(&repo)?),
            ]);
            c
        }
        Lookup::GitGrep {
            repo,
            pattern,
            path,
        } => {
            ensure!(
                !pattern.is_empty() && pattern.len() <= 2000 && !pattern.contains('\0'),
                "Pattern must contain 1..2000 bytes"
            );
            let repo = repository(&repo, &roots)?;
            let mut c = git(&repo);
            c.args([
                "grep",
                "-n",
                "-F",
                "-e",
                &pattern,
                &default_ref(&repo)?,
                "--",
            ]);
            if let Some(path) = path {
                c.arg(checked_path(&path)?);
            }
            let (ok, output) = run(&mut c, Duration::from_secs(45))?;
            ensure!(ok || output.is_empty(), "git grep failed: {output}");
            return Ok(output);
        }
    };
    let (ok, output) = run(&mut cmd, Duration::from_secs(45))?;
    ensure!(ok, "Lookup failed: {output}");
    Ok(output)
}

fn run(cmd: &mut Command, timeout: Duration) -> Result<(bool, String)> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("NO_COLOR", "1");
    let mut child = cmd.spawn().context("Could not start lookup")?;
    let (tx, rx) = std::sync::mpsc::channel();
    for mut pipe in [
        Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
        Box::new(child.stderr.take().unwrap()),
    ] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe
                .by_ref()
                .take((LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = tx.send(result);
        });
    }
    drop(tx);
    let started = Instant::now();
    let mut output = Vec::new();
    let mut pipes = 0;
    let result = (|| {
        loop {
            while let Ok(bytes) = rx.try_recv() {
                output.extend(bytes?);
                pipes += 1;
                ensure!(
                    output.len() <= LIMIT,
                    "Lookup output exceeds 256 KiB; narrow the request"
                );
            }
            if let Some(status) = child.try_wait()?
                && pipes == 2
            {
                return Ok((
                    status.success(),
                    String::from_utf8_lossy(&output).into_owned(),
                ));
            }
            ensure!(started.elapsed() < timeout, "Lookup timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    })();
    if result.is_err() {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_flags_traversal_or_arbitrary_operations() {
        assert!(github_repo("gylleus/stronghold"));
        for bad in ["https://github.com/o/r", "a/b/c", "a/b --web", "a/", "/b"] {
            assert!(!github_repo(bad));
        }
        for bad in ["../a", "/a", "-o", "a/../../b", ""] {
            assert!(checked_path(bad).is_err());
        }
        assert!(checked_path("src/a b.rs").is_ok());
        assert!(
            serde_json::from_value::<Lookup>(serde_json::json!({"op":"git_commit","repo":"/tmp"}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<Lookup>(
                serde_json::json!({"op":"git_fetch","repo":"/tmp","args":["--upload-pack=evil"]})
            )
            .is_err()
        );
        assert!(repository("/tmp", &[]).is_err());
    }
    #[test]
    fn subprocess_deadline_and_byte_cap() {
        let mut cmd = Command::new("/bin/sleep");
        cmd.arg("5");
        let start = Instant::now();
        assert!(run(&mut cmd, Duration::from_millis(50)).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        let mut cmd = Command::new("/usr/bin/yes");
        assert!(
            run(&mut cmd, Duration::from_secs(2))
                .unwrap_err()
                .to_string()
                .contains("256 KiB")
        );
    }
}

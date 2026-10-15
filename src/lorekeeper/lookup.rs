//! Fixed read operations for code and pull-request evidence.

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
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

fn named_repository(name: &str, repos: &[crate::orca::spawn::Repo]) -> Result<PathBuf> {
    let mut matches = repos
        .iter()
        .filter(|r| r.name() == name || Path::new(&r.path).file_name().is_some_and(|n| n == name));
    let repo = matches.next().with_context(|| {
        format!(
            "No Orca repository named {name:?}; use its registered name or absolute worktree root"
        )
    })?;
    ensure!(
        matches.next().is_none(),
        "Orca repository name {name:?} is ambiguous; use an absolute worktree root"
    );
    Ok(PathBuf::from(&repo.path))
}

fn repository(input: &str, roots: &[PathBuf]) -> Result<PathBuf> {
    let candidate = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        ensure!(
            super::report::relative_path(input) && Path::new(input).components().count() == 1,
            "Expected an absolute worktree root or bare Orca repository name, got {input:?}"
        );
        let repos = crate::orca::OrcaClient::new()
            .without_sender_identity()
            .timeout(Duration::from_secs(10))
            .repo_list()
            .with_context(|| format!("Cannot resolve repository {input:?} through Orca"))?;
        named_repository(input, &repos)?
    };
    (|| {
        let path = candidate
            .canonicalize()
            .context("Repository path does not exist")?;
        ensure!(
            roots
                .iter()
                .filter_map(|r| r.canonicalize().ok())
                .any(|r| path.starts_with(r)),
            "Repository must be under ~/repo or ~/orca/workspaces"
        );
        let (status, top) = run(
            git(&path).args(["rev-parse", "--show-toplevel"]),
            Duration::from_secs(10),
        )?;
        ensure!(
            status.success(),
            "Not a usable Git worktree: {}",
            top.trim()
        );
        let top = Path::new(top.trim())
            .canonicalize()
            .context("Cannot resolve Git worktree root")?;
        ensure!(
            top == path,
            "Use the repository worktree root {}",
            top.display()
        );
        Ok(path)
    })()
    .with_context(|| format!("Rejected repository {input:?} at {}", candidate.display()))
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
    let (status, name) = run(
        git(repo).args(["symbolic-ref", "refs/remotes/origin/HEAD"]),
        Duration::from_secs(10),
    )?;
    let name = name.trim();
    ensure!(
        status.success()
            && name.starts_with("refs/remotes/origin/")
            && !name.contains(['\n', '\r', ':']),
        "origin/HEAD is unavailable; configure the repository default remote branch first"
    );
    Ok(name.to_owned())
}

pub fn execute(request: Lookup) -> Result<String> {
    let home = super::home()?;
    let roots = [home.join("repo"), home.join("orca/workspaces")];
    execute_in(request, &roots)
}

fn execute_in(request: Lookup, roots: &[PathBuf]) -> Result<String> {
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
            let repo = repository(&repo, roots)?;
            let mut c = git(&repo);
            c.args(["fetch", "--no-recurse-submodules", "origin"]);
            c
        }
        Lookup::GitLog { repo, limit, path } => {
            let repo = repository(&repo, roots)?;
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
            let repo = repository(&repo, roots)?;
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
            let repo = repository(&repo, roots)?;
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
            let (status, output) = run(&mut c, Duration::from_secs(45))?;
            if status.code() == Some(1) && output.is_empty() {
                return Ok("No literal matches on origin's default branch. Check the path and letter case. The pattern is fixed text: |, .* and other regex syntax are literal; search alternatives separately.".into());
            }
            ensure!(
                status.success(),
                "git grep failed (status {status}): {output}"
            );
            return Ok(output);
        }
    };
    let (status, output) = run(&mut cmd, Duration::from_secs(45))?;
    ensure!(
        status.success(),
        "Lookup failed (status {status}): {output}"
    );
    Ok(output)
}

fn run(cmd: &mut Command, timeout: Duration) -> Result<(ExitStatus, String)> {
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
                return Ok((status, String::from_utf8_lossy(&output).into_owned()));
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
    fn named_repositories_and_literal_grep_preserve_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("recommendations");
        std::fs::create_dir(&repo).unwrap();
        assert!(
            git(&repo)
                .args(["init", "--quiet", "-b", "main"])
                .output()
                .unwrap()
                .status
                .success()
        );
        std::fs::write(
            repo.join("evidence.txt"),
            "purchase\nbuildChunkEmbeddings\nliteral|alternative\n",
        )
        .unwrap();
        for args in [
            vec!["add", "evidence.txt"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
            vec!["update-ref", "refs/remotes/origin/main", "HEAD"],
            vec![
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        ] {
            assert!(git(&repo).args(args).output().unwrap().status.success());
        }
        let catalog = vec![crate::orca::spawn::Repo {
            path: repo.to_string_lossy().into(),
            ..Default::default()
        }];
        let resolved = named_repository("recommendations", &catalog).unwrap();
        assert_eq!(resolved, repo);
        assert!(named_repository("missing", &catalog).is_err());
        assert!(named_repository("../recommendations", &catalog).is_err());
        let roots = [dir.path().to_owned()];
        assert_eq!(
            repository(resolved.to_str().unwrap(), &roots).unwrap(),
            repo.canonicalize().unwrap()
        );
        assert!(repository(resolved.to_str().unwrap(), &[]).is_err());
        std::fs::create_dir(repo.join("nested")).unwrap();
        let error = format!(
            "{:#}",
            repository(repo.join("nested").to_str().unwrap(), &roots).unwrap_err()
        );
        assert!(error.contains("nested") && error.contains("worktree root"));
        std::fs::write(repo.join("evidence.txt"), "changed locally").unwrap();
        for pattern in ["purchase", "buildChunkEmbeddings", "literal|alternative"] {
            let output = execute_in(
                Lookup::GitGrep {
                    repo: repo.to_string_lossy().into(),
                    pattern: pattern.into(),
                    path: None,
                },
                &roots,
            )
            .unwrap();
            assert!(output.contains(pattern) && output.contains("refs/remotes/origin/main"));
        }
        let output = execute_in(
            Lookup::GitGrep {
                repo: repo.to_string_lossy().into(),
                pattern: "purchase|PurchaseEvent".into(),
                path: None,
            },
            &roots,
        )
        .unwrap();
        assert!(
            output.contains("No literal matches")
                && output.contains("search alternatives separately")
        );
        let stale = dir.path().join("stale-checkout");
        std::fs::create_dir(&stale).unwrap();
        std::fs::write(
            stale.join(".git"),
            format!("gitdir: {}", dir.path().join("missing").display()),
        )
        .unwrap();
        let error = format!(
            "{:#}",
            repository(stale.to_str().unwrap(), &roots).unwrap_err()
        );
        assert!(error.contains("stale-checkout") && error.contains("Not a usable Git worktree"));
    }
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

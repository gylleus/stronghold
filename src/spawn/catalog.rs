//! Orca's repos and worktrees, for picking where a session starts.

use std::sync::mpsc;
use std::time::Duration;

use crate::model::repo_of;
use crate::orca::OrcaClient;
use crate::orca::spawn::{Repo, Worktree};

/// Timeout of the listing calls.
const LIST_TIMEOUT: Duration = Duration::from_secs(15);

/// One poll of `orca repo list` and `orca worktree list`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    pub repos: Vec<Repo>,
    pub worktrees: Vec<Worktree>,
}

/// A repo and its worktrees, as the dialog lists them.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoGroup<'a> {
    pub repo: &'a Repo,
    /// The main checkout first, then the most recently active.
    pub worktrees: Vec<&'a Worktree>,
}

impl Catalog {
    /// Fetches both lists with `client`.
    pub fn fetch(client: &OrcaClient) -> Result<Self, String> {
        let client = client.clone().timeout(LIST_TIMEOUT);
        let repos = client.repo_list().map_err(|e| format!("repo list: {e}"))?;
        let worktrees = client
            .worktree_list()
            .map_err(|e| format!("worktree list: {e}"))?
            .worktrees;
        Ok(Self { repos, worktrees })
    }

    /// Fetches on a background thread; the receiver gets one result.
    pub fn fetch_async(wake: Option<crate::actions::Wake>) -> mpsc::Receiver<Result<Self, String>> {
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("spawn-catalog".into())
            .spawn(move || {
                let client = OrcaClient::new().without_sender_identity();
                let _ = tx.send(Self::fetch(&client));
                if let Some(wake) = wake {
                    wake();
                }
            });
        if let Err(e) = spawned {
            log::warn!("spawn: could not start the catalog thread: {e}");
        }
        rx
    }

    /// Repos by name, each with its live (not archived) worktrees.
    pub fn groups(&self) -> Vec<RepoGroup<'_>> {
        let mut repos: Vec<&Repo> = self.repos.iter().collect();
        repos.sort_by_key(|r| r.name().to_lowercase());
        repos
            .into_iter()
            .map(|repo| RepoGroup {
                repo,
                worktrees: self.worktrees_of(&repo.id),
            })
            .collect()
    }

    /// The repo's worktrees, main checkout first, then by last activity.
    pub fn worktrees_of(&self, repo_id: &str) -> Vec<&Worktree> {
        let mut out: Vec<&Worktree> = self
            .worktrees
            .iter()
            .filter(|w| !w.is_archived && w.repo() == Some(repo_id))
            .collect();
        out.sort_by(|a, b| {
            b.is_main_worktree
                .cmp(&a.is_main_worktree)
                .then(b.last_activity_at.cmp(&a.last_activity_at))
                .then(a.name().cmp(b.name()))
        });
        out
    }

    pub fn repo(&self, id: &str) -> Option<&Repo> {
        self.repos.iter().find(|r| r.id == id)
    }

    pub fn worktree(&self, id: &str) -> Option<&Worktree> {
        self.worktrees.iter().find(|w| w.id == id)
    }

    /// The Orca repo behind a sidebar repo name (which Stronghold derives from worktree paths):
    /// its display name, its path's repo name, or the repo name of one of its worktrees.
    pub fn repo_for_sidebar(&self, name: &str) -> Option<&Repo> {
        let by = |f: &dyn Fn(&Repo) -> bool| self.repos.iter().find(|r| f(r));
        by(&|r| r.name() == name)
            .or_else(|| by(&|r| repo_of(&r.path).as_deref() == Some(name)))
            .or_else(|| {
                let wt = self
                    .worktrees
                    .iter()
                    .find(|w| repo_of(&w.path).as_deref() == Some(name))?;
                self.repo(wt.repo()?)
            })
            .or_else(|| by(&|r| r.name().eq_ignore_ascii_case(name)))
    }

    /// A repo by id, `id:<id>`, display name or path.
    pub fn find_repo(&self, key: &str) -> Option<&Repo> {
        let key = key.trim();
        let bare = key.strip_prefix("id:").unwrap_or(key);
        let path = key
            .strip_prefix("path:")
            .unwrap_or(key)
            .trim_end_matches('/');
        self.repos
            .iter()
            .find(|r| r.id == bare || r.path == path)
            .or_else(|| self.repos.iter().find(|r| r.name() == key))
            .or_else(|| self.repo_for_sidebar(key))
    }

    /// A worktree by id (`<repoId>::<path>`, optionally `id:`), path, `<repo>/<name>`, display
    /// name or branch. A bare name must be unambiguous.
    pub fn find_worktree(&self, key: &str) -> Result<&Worktree, String> {
        let key = key.trim();
        let bare = key.strip_prefix("id:").unwrap_or(key);
        let path = key
            .strip_prefix("path:")
            .unwrap_or(key)
            .trim_end_matches('/');
        let live = || self.worktrees.iter().filter(|w| !w.is_archived);
        if let Some(w) = live().find(|w| w.id == bare || w.path == path) {
            return Ok(w);
        }
        if let Some((repo, name)) = key.split_once('/')
            && let Some(repo) = self.find_repo(repo)
            && let Some(w) = live().find(|w| {
                w.repo() == Some(&repo.id) && (w.name() == name || w.branch_name() == Some(name))
            })
        {
            return Ok(w);
        }
        let named: Vec<&Worktree> = live()
            .filter(|w| w.name() == key || w.branch_name() == Some(key))
            .collect();
        match named.as_slice() {
            [one] => Ok(one),
            [] => Err(format!("no worktree {key:?}")),
            many => Err(format!(
                "{key:?} names {} worktrees; give the worktree id or <repo>/<name>",
                many.len()
            )),
        }
    }
}

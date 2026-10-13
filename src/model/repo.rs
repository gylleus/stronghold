//! Which repo (Orca project) a node or run acts on, derived from worktree paths alone: the
//! farm and sidebar group by it on every snapshot, so no git calls.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use super::{ClanSnapshot, Node, NodeId};

/// Repo bucket for nodes without a worktree.
pub const NO_REPO: &str = "(no repo)";

/// The repo of a worktree path: the component after `workspaces/` in Orca paths
/// (`…/orca/workspaces/<repo>/<worktree>`), the parent directory without its `-worktrees`
/// suffix for sibling worktrees (`…/<repo>-worktrees/<name>`), else the directory's own name
/// (a repo root such as `…/hackathon/commerce-platform`).
pub fn repo_of(path: &str) -> Option<String> {
    let path = Path::new(path.trim_end_matches('/'));
    let components: Vec<&str> = path.iter().filter_map(|c| c.to_str()).collect();
    if let Some(pos) = components.iter().position(|c| *c == "workspaces")
        && let Some(repo) = components.get(pos + 1)
    {
        return Some((*repo).to_string());
    }
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|p| p.to_str());
    if let Some(repo) = parent.and_then(|p| p.strip_suffix("-worktrees"))
        && !repo.is_empty()
    {
        return Some(repo.to_string());
    }
    let own = path.file_name()?.to_str()?;
    (!own.is_empty()).then(|| own.to_string())
}

/// The repo of a node's own worktree, if it has one.
fn own_repo(node: &Node) -> Option<String> {
    node.worktree_path.as_deref().and_then(repo_of)
}

/// Like [`ClanSnapshot::node`], but also works on snapshots assembled by hand.
fn find_node<'a>(snapshot: &'a ClanSnapshot, id: &NodeId) -> Option<&'a Node> {
    snapshot
        .node(id)
        .or_else(|| snapshot.nodes.iter().find(|n| &n.id == id))
}

/// A run's repo: its coordinator's, else its attendant's, else the most common among its
/// nodes (ties to the smaller name), else [`NO_REPO`].
pub fn run_repo(snapshot: &ClanSnapshot, run_id: &str) -> String {
    let lead = snapshot
        .run(run_id)
        .and_then(|r| r.coordinator.as_ref())
        .into_iter()
        .chain(snapshot.attendant(run_id))
        .filter_map(|id| find_node(snapshot, id))
        .find_map(own_repo);
    if let Some(repo) = lead {
        return repo;
    }
    let mut votes: BTreeMap<String, usize> = BTreeMap::new();
    for node in snapshot.nodes_in_run(run_id) {
        if let Some(repo) = own_repo(node) {
            *votes.entry(repo).or_insert(0) += 1;
        }
    }
    votes
        .into_iter()
        .max_by(|(ra, a), (rb, b)| a.cmp(b).then_with(|| rb.cmp(ra)))
        .map_or_else(|| NO_REPO.to_string(), |(repo, _)| repo)
}

/// The repo a node is grouped under: its run's for run members (so a run stays in one
/// repo), else its own worktree's, else [`NO_REPO`].
pub fn node_repo(node: &Node, snapshot: &ClanSnapshot) -> String {
    match node.run_id.as_deref() {
        Some(run) if snapshot.run(run).is_some() => run_repo(snapshot, run),
        _ => own_repo(node).unwrap_or_else(|| NO_REPO.to_string()),
    }
}

/// [`node_repo`] of every node, computing each run's repo once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoIndex {
    pub by_node: HashMap<NodeId, String>,
    pub by_run: HashMap<String, String>,
}

impl RepoIndex {
    pub fn new(snapshot: &ClanSnapshot) -> Self {
        let by_run: HashMap<String, String> = snapshot
            .runs
            .iter()
            .map(|r| (r.id.clone(), run_repo(snapshot, &r.id)))
            .collect();
        let by_node = snapshot
            .nodes
            .iter()
            .map(|n| {
                let repo = n
                    .run_id
                    .as_deref()
                    .and_then(|r| by_run.get(r).cloned())
                    .or_else(|| own_repo(n))
                    .unwrap_or_else(|| NO_REPO.to_string());
                (n.id.clone(), repo)
            })
            .collect();
        Self { by_node, by_run }
    }

    pub fn node(&self, id: &NodeId) -> Option<&str> {
        self.by_node.get(id).map(String::as_str)
    }

    pub fn run(&self, run_id: &str) -> Option<&str> {
        self.by_run.get(run_id).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_derivation_cases() {
        let cases = [
            ("/Users/x/orca/workspaces/shop/feat-pay", Some("shop")),
            ("/Users/x/orca/workspaces/shop/feat-pay/", Some("shop")),
            ("/Users/x/orca/workspaces/shop", Some("shop")),
            (
                "/Users/gylleus/orca/workspaces/commerce-platform/hack-77-admin-speed",
                Some("commerce-platform"),
            ),
            (
                "/Users/gylleus/repo/hackathon/commerce-platform",
                Some("commerce-platform"),
            ),
            ("/Users/gylleus/repo/depict/realtime", Some("realtime")),
            ("/Users/x/code/saker/stronghold", Some("stronghold")),
            (
                "/Users/x/repo/stronghold-worktrees/myr-k7y.6",
                Some("stronghold"),
            ),
            ("/Users/x/repo/-worktrees/a", Some("a")),
            ("/", None),
            ("", None),
        ];
        for (path, expected) in cases {
            assert_eq!(repo_of(path).as_deref(), expected, "{path}");
        }
    }
}

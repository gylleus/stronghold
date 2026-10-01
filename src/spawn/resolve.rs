//! Spawn requests from the control API and the Queen's `spawn_session` tool: argument parsing
//! and resolving names (repo, worktree, run, terminal) to Orca ids.

use serde_json::{Value, json};

use super::{Agent, SpawnRequest, SpawnTarget, catalog::Catalog, validate_name};
use crate::control::protocol::{ControlError, Params};
use crate::model::{FleetSnapshot, NodeKind};
use crate::orca::spawn::{WorkerPlacement, Worktree};
use crate::orca::{Run, Terminal};

/// The three ways to start a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModeName {
    /// A fresh agent in an existing worktree.
    #[default]
    Worktree,
    /// A new worktree off the repo's base ref.
    NewWorktree,
    /// A worker of an existing run.
    Worker,
}

impl ModeName {
    pub const ALL: [ModeName; 3] = [ModeName::Worktree, ModeName::NewWorktree, ModeName::Worker];

    pub fn as_str(self) -> &'static str {
        match self {
            ModeName::Worktree => "worktree",
            ModeName::NewWorktree => "new_worktree",
            ModeName::Worker => "worker",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ModeName::Worktree => "Existing worktree",
            ModeName::NewWorktree => "New worktree",
            ModeName::Worker => "Run worker",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "worktree" | "existing" | "existing_worktree" => Some(ModeName::Worktree),
            "new_worktree" | "new" => Some(ModeName::NewWorktree),
            "worker" | "run_worker" => Some(ModeName::Worker),
            _ => None,
        }
    }
}

/// A spawn as the API asks for it, before names are resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnArgs {
    pub mode: ModeName,
    /// Repo id, name or path (new worktree, or a worker's new worktree).
    pub repo: Option<String>,
    /// Worktree id, path, `<repo>/<name>` or name.
    pub worktree: Option<String>,
    /// Name of the new worktree.
    pub name: Option<String>,
    pub base_branch: Option<String>,
    pub run_id: Option<String>,
    /// Terminal handle a worker reuses (the app maps agent ids and labels to handles).
    pub terminal: Option<String>,
    /// Worker task title.
    pub title: Option<String>,
    /// Short name Myrstack shows for the agent; see [`SpawnRequest::label`].
    pub label: Option<String>,
    pub prompt: String,
    pub agent: Agent,
    pub model: Option<String>,
    /// Let Orca add a suffix (`name-2`) when the new worktree's name or branch is taken, instead
    /// of failing.
    pub allow_suffix: bool,
}

impl SpawnArgs {
    /// Parses the `spawn` method's params.
    pub fn parse(params: &Value) -> Result<Self, ControlError> {
        let p = Params::new(params);
        let invalid = ControlError::invalid_params;
        let opt = |key: &str| -> Result<Option<String>, ControlError> {
            Ok(p.str(key)?
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned))
        };
        let mode = p.required_str("mode")?;
        let mode = ModeName::parse(mode).ok_or_else(|| {
            invalid(format!(
                "unknown mode {mode:?}; use \"worktree\", \"new_worktree\" or \"worker\""
            ))
        })?;
        let agent = match p.str("agent")? {
            None => Agent::default(),
            Some(a) => Agent::parse(a).ok_or_else(|| {
                invalid(format!("unknown agent {a:?}; use \"claude\" or \"codex\""))
            })?,
        };
        let args = Self {
            mode,
            repo: opt("repo")?,
            worktree: opt("worktree")?,
            name: opt("name")?,
            base_branch: opt("base_branch")?,
            run_id: opt("run_id")?,
            terminal: opt("terminal")?,
            title: opt("title")?,
            label: opt("label")?,
            prompt: p.str("prompt")?.unwrap_or_default().trim().to_string(),
            agent,
            model: opt("model")?,
            allow_suffix: p.bool("allow_suffix")?.unwrap_or(false),
        };
        args.check().map_err(invalid)?;
        Ok(args)
    }

    /// The `spawn` params these arguments parse from (only the fields that are set).
    pub fn to_params(&self) -> Value {
        let mut params = json!({
            "mode": self.mode.as_str(),
            "prompt": self.prompt,
            "agent": self.agent.as_str(),
        });
        let fields = [
            ("repo", &self.repo),
            ("worktree", &self.worktree),
            ("name", &self.name),
            ("base_branch", &self.base_branch),
            ("run_id", &self.run_id),
            ("terminal", &self.terminal),
            ("title", &self.title),
            ("label", &self.label),
            ("model", &self.model),
        ];
        for (key, value) in fields {
            if let Some(value) = value {
                params[key] = json!(value);
            }
        }
        if self.allow_suffix {
            params["allow_suffix"] = json!(true);
        }
        params
    }

    /// Checks which fields the mode needs, before anything is looked up.
    pub fn check(&self) -> Result<(), String> {
        match self.mode {
            ModeName::Worktree => {
                if self.worktree.is_none() {
                    return Err("mode \"worktree\" needs \"worktree\"".into());
                }
            }
            ModeName::NewWorktree => {
                if self.repo.is_none() || self.name.is_none() {
                    return Err("mode \"new_worktree\" needs \"repo\" and \"name\"".into());
                }
            }
            ModeName::Worker => {
                if self.run_id.is_none() {
                    return Err("mode \"worker\" needs \"run_id\"".into());
                }
                if self.prompt.is_empty() {
                    return Err("mode \"worker\" needs \"prompt\" (the task spec)".into());
                }
                let places = [
                    self.terminal.is_some(),
                    self.worktree.is_some(),
                    self.repo.is_some() && self.name.is_some(),
                ];
                if places.iter().filter(|p| **p).count() != 1 {
                    return Err(
                        "a worker needs exactly one place: \"terminal\", \"worktree\", \
                         or \"repo\" + \"name\" for a new worktree"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    /// Resolves names against Orca's lists and the fleet's runs and terminals.
    pub fn resolve(
        &self,
        catalog: &Catalog,
        runs: &[Run],
        terminals: &[Terminal],
    ) -> Result<SpawnRequest, String> {
        self.check()?;
        let repo = |key: &str| {
            catalog
                .find_repo(key)
                .map(|r| r.id.clone())
                .ok_or_else(|| format!("no Orca repo {key:?}"))
        };
        let target = match self.mode {
            ModeName::Worktree => {
                let wt = catalog.find_worktree(self.worktree.as_deref().unwrap_or_default())?;
                SpawnTarget::Worktree {
                    worktree_id: wt.id.clone(),
                    path: wt.path.clone(),
                }
            }
            ModeName::NewWorktree => SpawnTarget::NewWorktree {
                repo_id: repo(self.repo.as_deref().unwrap_or_default())?,
                name: self.name.clone().unwrap_or_default(),
                base_branch: self.base_branch.clone(),
            },
            ModeName::Worker => {
                let run_id = self.run_id.as_deref().unwrap_or_default();
                let run = runs
                    .iter()
                    .find(|r| r.id == run_id)
                    .ok_or_else(|| format!("no run {run_id:?}"))?;
                let coordinator = run
                    .coordinator_handle
                    .clone()
                    .ok_or_else(|| format!("run {run_id} has no coordinator terminal"))?;
                let mut path = None;
                let placement = if let Some(handle) = &self.terminal {
                    let terminal = terminals
                        .iter()
                        .find(|t| &t.handle == handle)
                        .ok_or_else(|| format!("no live terminal {handle:?}"))?;
                    path = terminal.worktree_path.clone();
                    WorkerPlacement::Terminal {
                        worktree_id: terminal
                            .worktree_id
                            .clone()
                            .ok_or_else(|| format!("terminal {handle} has no worktree"))?,
                        handle: handle.clone(),
                    }
                } else if let Some(key) = &self.worktree {
                    let wt = catalog.find_worktree(key)?;
                    path = Some(wt.path.clone());
                    WorkerPlacement::Worktree {
                        worktree_id: wt.id.clone(),
                    }
                } else {
                    WorkerPlacement::NewWorktree {
                        repo_id: repo(self.repo.as_deref().unwrap_or_default())?,
                        name: self.name.clone().unwrap_or_default(),
                    }
                };
                SpawnTarget::Worker {
                    run_id: run.id.clone(),
                    coordinator,
                    placement,
                    title: self.title.clone(),
                    path,
                }
            }
        };
        Ok(SpawnRequest {
            target,
            agent: self.agent,
            model: self.model.clone(),
            prompt: self.prompt.clone(),
            label: self.label.clone(),
        })
    }
}

/// A live agent, for naming who already works in a worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveAgent {
    /// Node id (`term:…`, `dispatch:…`).
    pub node: String,
    /// `working`, `idle`, `needs_input`, …
    pub state: String,
    pub worktree_path: Option<String>,
}

impl LiveAgent {
    /// The snapshot's live agents (no subagents, no Queen).
    pub fn from_snapshot(snapshot: &FleetSnapshot) -> Vec<Self> {
        snapshot
            .agent_nodes()
            .filter(|n| n.live && n.kind != NodeKind::Subagent)
            .map(|n| Self {
                node: n.id.to_string(),
                state: crate::control::view::state_name(n.state).to_owned(),
                worktree_path: n.worktree_path.clone(),
            })
            .collect()
    }
}

/// A new worktree's name that Orca would suffix (`name-2`) because it is taken.
#[derive(Debug, Clone, PartialEq)]
pub struct NameConflict {
    pub name: String,
    /// The worktree with that name, path or branch.
    pub worktree: Option<Worktree>,
    /// A local branch named `name` or `<prefix>/name`.
    pub branch: Option<String>,
}

impl NameConflict {
    /// The error for the caller: what exists, who works there, and what to do instead.
    pub fn message(&self, agents: &[LiveAgent]) -> String {
        let mut text = format!("a worktree named {:?} already exists", self.name);
        if let Some(wt) = &self.worktree {
            text = format!(
                "worktree {:?} already exists at {}{}{}",
                self.name,
                wt.path,
                wt.branch_name()
                    .map(|b| format!(" (branch {b})"))
                    .unwrap_or_default(),
                if wt.is_archived { ", archived" } else { "" }
            );
            let there: Vec<String> = agents
                .iter()
                .filter(|a| a.worktree_path.as_deref() == Some(wt.path.as_str()))
                .map(|a| format!("{} ({})", a.node, a.state.replace('_', " ")))
                .collect();
            if there.is_empty() {
                text.push_str("; no live agent in it");
            } else {
                text.push_str(&format!(
                    "; live agent{} in it: {}. Send it the changes with send_to_agent instead of \
                     spawning again",
                    if there.len() == 1 { "" } else { "s" },
                    there.join(", ")
                ));
            }
        } else if let Some(branch) = &self.branch {
            text = format!("branch {branch} already exists (no worktree has it)");
        }
        text.push_str(
            ". Pick another name, use mode \"worktree\" to start an agent in the existing \
             worktree, or pass allow_suffix=true if a second copy is really wanted.",
        );
        text
    }
}

/// The repo and name of the worktree `request` creates, if it creates one.
pub fn new_worktree_of(request: &SpawnRequest) -> Option<(&str, &str)> {
    match &request.target {
        SpawnTarget::NewWorktree { repo_id, name, .. }
        | SpawnTarget::Worker {
            placement: WorkerPlacement::NewWorktree { repo_id, name },
            ..
        } => Some((repo_id, name)),
        _ => None,
    }
}

/// Whether a new worktree `name` in `repo_id` is taken by one of the catalog's worktrees (by
/// name, directory or branch) or by one of `branches` (the repo's local branches; Orca may
/// prefix them, as in `karl/name`).
pub fn name_conflict(
    catalog: &Catalog,
    repo_id: &str,
    name: &str,
    branches: &[String],
) -> Option<NameConflict> {
    let name = validate_name(name).ok()?;
    let branch_matches = |b: &str| b == name || b.ends_with(&format!("/{name}"));
    let worktree = catalog
        .worktrees
        .iter()
        .filter(|w| w.repo() == Some(repo_id) && !w.is_main_worktree)
        .find(|w| {
            w.name() == name
                || w.path.trim_end_matches('/').rsplit('/').next() == Some(name.as_str())
                || w.branch_name().is_some_and(branch_matches)
        })
        .cloned();
    let branch = branches.iter().find(|b| branch_matches(b)).cloned();
    (worktree.is_some() || branch.is_some()).then_some(NameConflict {
        name,
        worktree,
        branch,
    })
}

/// The API's answer for a finished spawn.
pub fn outcome_json(outcome: &super::SpawnOutcome) -> Value {
    json!({
        "handle": outcome.context.handle,
        "node": outcome.node_id().map(|id| id.as_str().to_owned()),
        "worktree_id": outcome.context.worktree_id,
        "worktree_path": outcome.context.worktree_path,
        "task_id": outcome.context.task_id,
        "dispatch_id": outcome.context.dispatch_id,
        "verified": outcome.verified,
        "notes": outcome.notes,
    })
}

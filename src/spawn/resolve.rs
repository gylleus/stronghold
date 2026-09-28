//! Spawn requests from the control API and the Queen's `spawn_session` tool: argument parsing
//! and resolving names (repo, worktree, run, terminal) to Orca ids.

use serde_json::{Value, json};

use super::{Agent, SpawnRequest, SpawnTarget, catalog::Catalog};
use crate::control::protocol::{ControlError, Params};
use crate::orca::spawn::WorkerPlacement;
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
    pub prompt: String,
    pub agent: Agent,
    pub model: Option<String>,
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
            prompt: p.str("prompt")?.unwrap_or_default().trim().to_string(),
            agent,
            model: opt("model")?,
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
            ("model", &self.model),
        ];
        for (key, value) in fields {
            if let Some(value) = value {
                params[key] = json!(value);
            }
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
        })
    }
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

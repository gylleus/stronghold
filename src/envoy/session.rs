//! The one persistent Envoy terminal and its restricted spawn plan.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use super::{BridgeStatus, EnvoyStatus, SessionState, settings::EnvoySettings};
use crate::fleet::FleetData;
use crate::spawn::{Agent, Catalog, SpawnPlan, SpawnRequest, SpawnTarget, Step, shell_quote};

pub const HOME: &str = "envoy-home";
pub const PROMPT: &str = "Load your rules with get_playbook role=envoy, then wait for events.";

pub fn plan(catalog: &Catalog, settings: &EnvoySettings) -> Result<SpawnPlan, String> {
    let repo = catalog
        .find_repo("myrstack")
        .ok_or("Orca has no myrstack repo")?;
    let home = catalog.worktrees.iter().find(|w| {
        w.repo() == Some(repo.id.as_str())
            && Path::new(&w.path).file_name().is_some_and(|n| n == HOME)
    });
    let target = match home {
        Some(w) if w.is_main_worktree || w.path == repo.path => {
            return Err("Envoy home must not be the main checkout".into());
        }
        Some(w) if w.is_archived => {
            return Err("Restore the archived envoy-home worktree before starting".into());
        }
        Some(w) => SpawnTarget::Worktree {
            worktree_id: w.id.clone(),
            path: w.path.clone(),
        },
        None => SpawnTarget::NewWorktree {
            repo_id: repo.id.clone(),
            name: HOME.into(),
            base_branch: Some("main".into()),
        },
    };
    let agent = Agent::parse(&settings.agent).ok_or("Envoy agent must be claude or codex")?;
    let mut plan = crate::spawn::build_plan(&SpawnRequest {
        target,
        agent,
        model: settings.model.clone(),
        prompt: PROMPT.into(),
        label: Some("Envoy".into()),
    })
    .map_err(|e| e.0)?;
    let command = command(agent, plan.request.model.as_deref());
    let mut needs_terminal = false;
    for step in &mut plan.steps {
        match step {
            Step::CreateWorktree(create) => {
                needs_terminal = create.agent.take().is_some();
                create.prompt = None;
            }
            Step::CreateTerminal {
                command: launch, ..
            } => *launch = command.clone(),
            _ => {}
        }
    }
    if needs_terminal {
        let index = plan
            .steps
            .iter()
            .position(|s| matches!(s, Step::WaitIdle { .. }))
            .unwrap();
        plan.steps.insert(
            index,
            Step::CreateTerminal {
                worktree_id: None,
                command,
            },
        );
    }
    Ok(plan)
}

fn command(agent: Agent, model: Option<&str>) -> String {
    let mut args = vec![agent.as_str().to_string()];
    if let Some(model) = model {
        args.extend(["--model".into(), shell_quote(model)]);
    }
    match agent {
        // Denying the whole shell also blocks wrappers and alternate Slack CLIs.
        Agent::Claude => args.extend(["--disallowedTools".into(), "Bash".into()]),
        Agent::Codex => args.extend([
            "--sandbox".into(),
            "read-only".into(),
            "--ask-for-approval".into(),
            "never".into(),
        ]),
    }
    args.extend(["--".into(), shell_quote(PROMPT)]);
    args.join(" ")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedSession {
    handle: String,
    settings: EnvoySettings,
}

#[derive(Debug)]
enum Event {
    Created(String),
    Done(Result<(), String>),
}

pub struct Session {
    path: PathBuf,
    handle: Option<String>,
    recovering: bool,
    load_failed: bool,
    receiver: Option<Receiver<Event>>,
    created_at_ms: i64,
    settings: EnvoySettings,
    pub error: Option<String>,
}

impl Session {
    pub fn load(path: PathBuf, settings: &EnvoySettings) -> Self {
        let mut settings = settings.clone();
        let (handle, error) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<SavedSession>(&bytes) {
                Ok(saved) if !saved.handle.is_empty() => {
                    settings = saved.settings;
                    (Some(saved.handle), None)
                }
                Ok(_) => (
                    None,
                    Some(format!("Empty Envoy identity in {}", path.display())),
                ),
                Err(e) => (
                    None,
                    Some(format!("Cannot recover {}: {e}", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(e) => (None, Some(format!("Cannot read {}: {e}", path.display()))),
        };
        Self {
            load_failed: error.is_some(),
            path,
            recovering: handle.is_some(),
            handle,
            receiver: None,
            created_at_ms: 0,
            settings: settings.clone(),
            error,
        }
    }

    pub fn handle(&self) -> Option<&str> {
        self.handle.as_deref()
    }

    pub fn settings(&self) -> &EnvoySettings {
        &self.settings
    }

    pub fn state(&self) -> SessionState {
        if self.receiver.is_some() || self.recovering {
            SessionState::Starting
        } else if self.handle.is_some() {
            SessionState::Running
        } else {
            SessionState::Stopped
        }
    }

    pub fn status(&self) -> EnvoyStatus {
        EnvoyStatus {
            session: self.state(),
            bridge: BridgeStatus::Missing,
            pending_drafts: 0,
            dry_run: self.settings.dry_run,
        }
    }

    pub fn start(
        &mut self,
        settings: &EnvoySettings,
        wake: crate::actions::Wake,
    ) -> Result<(), String> {
        if self.state() != SessionState::Stopped {
            return Err("The Envoy is already starting or running".into());
        }
        if self.load_failed {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| format!("Resolve {} before starting", self.path.display())));
        }
        let settings = settings.clone();
        let path = self.path.clone();
        let (tx, rx) = mpsc::channel();
        self.settings = settings.clone();
        std::thread::Builder::new()
            .name("envoy-start".into())
            .spawn(move || {
                use crate::spawn::exec::{OrcaBackend, SpawnBackend, run_plan_observed};
                let mut backend = OrcaBackend::default();
                let result = backend
                    .catalog()
                    .and_then(|catalog| plan(&catalog, &settings))
                    .and_then(|plan| {
                        let mut recorded = None;
                        run_plan_observed(&plan, &mut backend, &mut |_| {}, &mut |context| {
                            if let Some(handle) = &context.handle
                                && recorded.as_ref() != Some(handle)
                            {
                                // Keep the in-memory handle even if the disk fills after terminal creation.
                                let _ = tx.send(Event::Created(handle.clone()));
                                wake();
                                persist(
                                    &path,
                                    Some(&SavedSession {
                                        handle: handle.clone(),
                                        settings: settings.clone(),
                                    }),
                                )
                                .map_err(|e| format!("Cannot save Envoy identity: {e}"))?;
                                recorded = Some(handle.clone());
                            }
                            Ok(())
                        })
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                    });
                let _ = tx.send(Event::Done(result));
                wake();
            })
            .map_err(|e| e.to_string())?;
        self.receiver = Some(rx);
        self.error = None;
        Ok(())
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            let event = self.receiver.as_ref().map(|rx| rx.try_recv());
            match event {
                Some(Ok(Event::Created(handle))) => {
                    self.created_at_ms = now_ms();
                    self.handle = Some(handle);
                    changed = true;
                }
                Some(Ok(Event::Done(result))) => {
                    self.receiver = None;
                    self.error = result.err();
                    return true;
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.receiver = None;
                    self.error = Some("Envoy launch thread disconnected".into());
                    return true;
                }
                _ => return changed,
            }
        }
    }

    pub fn reconcile(&mut self, data: &FleetData) -> bool {
        if self.receiver.is_some()
            || data
                .terminals_polled_at_ms
                .is_none_or(|at| at <= self.created_at_ms)
            || data.errors.iter().any(|e| e.starts_with("terminal list"))
        {
            return false;
        }
        let Some(handle) = &self.handle else {
            return false;
        };
        if data.terminals.iter().any(|t| &t.handle == handle) {
            return std::mem::take(&mut self.recovering);
        }
        match persist(&self.path, None) {
            Ok(()) => {
                self.handle = None;
                self.recovering = false;
            }
            Err(e) => {
                self.error = Some(format!("Cannot clear Envoy identity: {e}"));
            }
        }
        true
    }
}

fn persist(path: &Path, handle: Option<&SavedSession>) -> std::io::Result<()> {
    match handle {
        Some(handle) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temp = path.with_extension("tmp");
            std::fs::write(&temp, serde_json::to_vec(handle)?)?;
            std::fs::rename(temp, path)
        }
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        },
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

impl super::EnvoyIdentity for Session {
    fn envoy_handle(&self) -> Option<&str> {
        self.handle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::{
        Terminal,
        spawn::{Repo, Worktree},
    };

    fn catalog() -> Catalog {
        Catalog {
            repos: vec![Repo {
                id: "repo".into(),
                path: "/repo/myrstack".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn reuses_home_by_path_even_after_label_changes() {
        let mut catalog = catalog();
        catalog.worktrees.push(Worktree {
            id: "home".into(),
            repo_id: Some("repo".into()),
            path: "/worktrees/envoy-home".into(),
            display_name: Some("Rewritten title".into()),
            ..Default::default()
        });
        let plan = plan(&catalog, &EnvoySettings::default()).unwrap();
        assert!(
            matches!(&plan.request.target, SpawnTarget::Worktree { worktree_id, .. } if worktree_id == "home")
        );
        assert!(
            !plan
                .steps
                .iter()
                .any(|s| matches!(s, Step::CreateWorktree(_)))
        );
        catalog.worktrees[0].is_main_worktree = true;
        assert!(super::plan(&catalog, &EnvoySettings::default()).is_err());
    }

    #[test]
    fn new_home_always_launches_a_restricted_terminal_even_without_model() {
        for agent in ["claude", "codex"] {
            for model in [None, Some("test-model".into())] {
                let settings = EnvoySettings {
                    agent: agent.into(),
                    model,
                    ..Default::default()
                };
                let plan = plan(&catalog(), &settings).unwrap();
                assert!(
                    matches!(&plan.request.target, SpawnTarget::NewWorktree { name, base_branch, .. } if name == HOME && base_branch.as_deref() == Some("main"))
                );
                let create = plan
                    .steps
                    .iter()
                    .find_map(|s| {
                        if let Step::CreateWorktree(c) = s {
                            Some(c)
                        } else {
                            None
                        }
                    })
                    .unwrap();
                assert_eq!(create.agent, None);
                let launches: Vec<_> = plan
                    .steps
                    .iter()
                    .filter_map(|s| {
                        if let Step::CreateTerminal { command, .. } = s {
                            Some(command)
                        } else {
                            None
                        }
                    })
                    .collect();
                assert_eq!(launches.len(), 1);
                let command = launches[0];
                assert!(command.ends_with(&format!(" -- {}", shell_quote(PROMPT))));
                if agent == "claude" {
                    assert!(command.contains("--disallowedTools Bash -- "));
                } else {
                    assert!(command.contains("--sandbox read-only --ask-for-approval never -- "));
                }
            }
        }
    }

    fn saved(path: &Path) {
        persist(
            path,
            Some(&SavedSession {
                handle: "envoy-terminal".into(),
                settings: EnvoySettings::default(),
            }),
        )
        .unwrap();
    }

    #[test]
    fn recovers_live_handle_and_clears_only_after_successful_fresh_poll() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        saved(&path);
        let mut session = Session::load(
            path.clone(),
            &EnvoySettings {
                dry_run: false,
                ..Default::default()
            },
        );
        assert_eq!(session.state(), SessionState::Starting);
        assert!(
            session.status().dry_run,
            "running settings survive config changes"
        );
        assert!(
            session
                .start(&EnvoySettings::default(), Box::new(|| {}))
                .is_err()
        );
        let mut data = FleetData {
            terminals_polled_at_ms: Some(1),
            terminals: vec![Terminal {
                handle: "envoy-terminal".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(session.reconcile(&data));
        assert_eq!(session.state(), SessionState::Running);
        assert!(
            session
                .start(&EnvoySettings::default(), Box::new(|| {}))
                .is_err()
        );
        data.terminals.clear();
        data.errors.push("terminal list: unavailable".into());
        assert!(!session.reconcile(&data));
        assert!(path.exists());
        data.errors.clear();
        session.created_at_ms = 2;
        assert!(!session.reconcile(&data));
        data.terminals_polled_at_ms = Some(3);
        assert!(session.reconcile(&data));
        assert_eq!(session.state(), SessionState::Stopped);
        assert!(!path.exists());
        assert_eq!(
            Session::load(path, &EnvoySettings::default()).handle(),
            None
        );
    }

    #[test]
    fn start_guard_covers_pending_spawn_and_corrupt_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let mut session = Session::load(path.clone(), &EnvoySettings::default());
        let (_tx, rx) = mpsc::channel();
        session.receiver = Some(rx);
        assert!(
            session
                .start(&EnvoySettings::default(), Box::new(|| {}))
                .is_err()
        );
        std::fs::write(&path, b"bad json").unwrap();
        let mut session = Session::load(path, &EnvoySettings::default());
        assert!(
            session
                .start(&EnvoySettings::default(), Box::new(|| {}))
                .is_err()
        );
    }

    #[test]
    fn partial_spawn_retains_handle_after_verification_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::load(dir.path().join("session.json"), &EnvoySettings::default());
        let (tx, rx) = mpsc::channel();
        session.receiver = Some(rx);
        tx.send(Event::Created("partial".into())).unwrap();
        tx.send(Event::Done(Err("verification failed".into())))
            .unwrap();
        assert!(session.poll());
        assert_eq!(session.handle(), Some("partial"));
        assert!(session.created_at_ms > 0);
        let stale = FleetData {
            terminals_polled_at_ms: Some(session.created_at_ms - 1),
            ..Default::default()
        };
        assert!(!session.reconcile(&stale));
        assert_eq!(session.handle(), Some("partial"));
        assert_eq!(session.state(), SessionState::Running);
        assert!(session.error.is_some());
        assert!(
            session
                .start(&EnvoySettings::default(), Box::new(|| {}))
                .is_err()
        );
    }
}

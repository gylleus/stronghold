//! The one persistent Envoy terminal and its restricted spawn plan.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use super::{BridgeStatus, EnvoyStatus, SessionState, settings::EnvoySettings};
use crate::fleet::FleetData;
use crate::spawn::{Agent, Catalog, SpawnPlan, SpawnRequest, SpawnTarget, Step, shell_quote};

pub const HOME: &str = "envoy-home";

pub fn plan(catalog: &Catalog, settings: &EnvoySettings) -> Result<SpawnPlan, String> {
    if settings.agent != "claude" {
        return Err("Envoy requires Claude: Codex MCP tool restrictions are not supported".into());
    }
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
    let mut plan = crate::spawn::build_plan(&SpawnRequest {
        target,
        agent: Agent::Claude,
        model: settings.model.clone(),
        prompt: crate::playbook::envoy_launch_prompt(settings.dry_run),
        label: Some("Envoy".into()),
    })
    .map_err(|e| e.0)?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let command = command(
        plan.request.model.as_deref(),
        &plan.request.prompt,
        &executable,
    );
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
            Step::Verify { allow_bypass, .. } => *allow_bypass = false,
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

fn allowed_tools() -> Vec<String> {
    ["Read", "Grep", "Glob"]
        .into_iter()
        .map(str::to_owned)
        .chain(
            [
                "get_playbook",
                "list_playbook",
                "fleet_status",
                "list_agents",
                "get_agent",
            ]
            .into_iter()
            .chain(super::control::METHODS.iter().copied())
            .map(|name| format!("mcp__myrstack__{name}")),
        )
        .collect()
}

fn denied_tools() -> Vec<String> {
    let allowed = allowed_tools();
    ["Bash", "Monitor", "Write", "Edit", "NotebookEdit"]
        .into_iter()
        .map(str::to_owned)
        .chain(
            crate::mcp::server::tool_definitions()
                .iter()
                .map(|tool| {
                    format!(
                        "mcp__myrstack__{}",
                        tool["name"].as_str().expect("tool name")
                    )
                })
                .filter(|name| !allowed.contains(name)),
        )
        .collect()
}

fn command(model: Option<&str>, prompt: &str, executable: &Path) -> String {
    let mut args = vec!["claude".to_owned()];
    if let Some(model) = model {
        args.extend(["--model".into(), shell_quote(model)]);
    }
    // Keep status hooks, but ignore worktree settings and every other MCP server.
    let mcp =
        serde_json::json!({"mcpServers": {"myrstack": {"command": executable, "args": ["mcp"]}}});
    args.extend(
        [
            "--setting-sources",
            "user",
            "--permission-mode",
            "dontAsk",
            "--tools",
            "Read,Grep,Glob",
            "--strict-mcp-config",
            "--mcp-config",
        ]
        .map(str::to_owned),
    );
    args.push(shell_quote(&mcp.to_string()));
    args.push("--allowedTools".into());
    args.extend(allowed_tools());
    args.push("--disallowedTools".into());
    args.extend(denied_tools());
    args.extend(["--".into(), shell_quote(prompt)]);
    args.join(" ")
}

/// Refuse a second session even when its identity file was lost.
pub(crate) fn prepare(
    backend: &mut dyn crate::spawn::exec::SpawnBackend,
    settings: &EnvoySettings,
) -> Result<SpawnPlan, String> {
    let plan = plan(&backend.catalog()?, settings)?;
    let value = backend
        .orca(
            &crate::orca::commands::terminal_list_args(),
            std::time::Duration::from_secs(30),
        )
        .map_err(|e| format!("Cannot check for an existing Envoy: {e}"))?;
    let list: crate::orca::TerminalList =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    if list.truncated {
        return Err("Cannot start Envoy: terminal list is incomplete".into());
    }
    if let Some(terminal) = list.terminals.iter().find(|t| {
        let in_home = match &plan.request.target {
            SpawnTarget::Worktree { worktree_id, path } => {
                t.worktree_id.as_ref() == Some(worktree_id)
                    || t.worktree_path.as_ref() == Some(path)
            }
            _ => t
                .worktree_path
                .as_ref()
                .is_some_and(|path| Path::new(path).file_name().is_some_and(|name| name == HOME)),
        };
        let preview = t.preview.as_deref().unwrap_or_default().to_lowercase();
        in_home
            && (t.agent_identity.is_some()
                || preview.contains("claude")
                || preview.contains("codex")
                || preview.contains('❯')
                || preview.contains('›'))
    }) {
        return Err(format!(
            "An agent is already in envoy-home ({}); close it before starting Envoy",
            terminal.handle
        ));
    }
    Ok(plan)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedSession {
    handle: String,
    settings: EnvoySettings,
    #[serde(default)]
    restricted: bool,
}

#[derive(Debug)]
enum Event {
    Created { handle: String },
    Done(Result<(), String>),
}

pub struct Session {
    path: PathBuf,
    handle: Option<String>,
    persisted: bool,
    restricted: bool,
    recovering: bool,
    load_failed: bool,
    receiver: Option<Receiver<Event>>,
    created_at_ms: i64,
    settings: EnvoySettings,
    pub error: Option<String>,
}

impl Session {
    pub fn load(path: PathBuf, settings: &EnvoySettings) -> Self {
        let dry_run = settings.dry_run;
        let mut settings = settings.clone();
        let mut restricted = false;
        let (handle, error) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<SavedSession>(&bytes) {
                Ok(saved) if !saved.handle.is_empty() => {
                    restricted = saved.restricted && saved.settings.agent == "claude";
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
        let mut session = Self {
            load_failed: error.is_some(),
            path,
            recovering: handle.is_some(),
            persisted: handle.is_some(),
            restricted,
            handle,
            receiver: None,
            created_at_ms: 0,
            settings,
            error,
        };
        if session.handle.is_some() && !session.restricted {
            session.error = Some("Restart Envoy: the previous launch was incomplete or lacks required tool restrictions".into());
        }
        if dry_run && let Err(error) = session.enable_dry_run() {
            session.error = Some(error);
        }
        session
    }

    pub fn handle(&self) -> Option<&str> {
        self.handle.as_deref()
    }

    /// Only a persisted, recovered or fully launched identity may enable ingestion.
    pub fn running_handle(&self) -> Option<&str> {
        self.handle()
            .filter(|_| self.persisted && self.restricted && self.state() == SessionState::Running)
    }

    pub fn settings(&self) -> &EnvoySettings {
        &self.settings
    }

    /// Safety ON applies to this session; OFF requires a new launch.
    pub fn apply_policies(&mut self, settings: &EnvoySettings) -> Result<(), String> {
        settings.validate_policies()?;
        let old = self.settings.clone();
        self.settings.policies = settings.policies.clone();
        self.settings.dry_run |= settings.dry_run;
        if let Err(e) = self.save() {
            self.settings = old;
            self.settings.dry_run |= settings.dry_run;
            return Err(e);
        }
        Ok(())
    }

    pub fn enable_dry_run(&mut self) -> Result<(), String> {
        self.settings.dry_run = true;
        self.save()
    }

    fn save(&mut self) -> Result<(), String> {
        let Some(handle) = &self.handle else {
            return Ok(());
        };
        let result = persist(
            &self.path,
            Some(&SavedSession {
                handle: handle.clone(),
                settings: self.settings.clone(),
                restricted: self.restricted,
            }),
        );
        self.persisted = result.is_ok();
        result.map_err(|e| format!("Cannot save {}: {e}", self.path.display()))
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
        let (tx, rx) = mpsc::channel();
        self.settings = settings.clone();
        std::thread::Builder::new()
            .name("envoy-start".into())
            .spawn(move || {
                use crate::spawn::exec::{OrcaBackend, run_plan_observed};
                let mut backend = OrcaBackend::default();
                let result = prepare(&mut backend, &settings).and_then(|plan| {
                    let mut recorded = None;
                    run_plan_observed(&plan, &mut backend, &mut |_| {}, &mut |context| {
                        if let Some(handle) = &context.handle
                            && recorded.as_ref() != Some(handle)
                        {
                            let _ = tx.send(Event::Created {
                                handle: handle.clone(),
                            });
                            wake();
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
                Some(Ok(Event::Created { handle })) => {
                    self.created_at_ms = now_ms();
                    self.handle = Some(handle);
                    self.restricted = false;
                    // Persist current safety settings before enabling ingestion.
                    if let Err(error) = self.save() {
                        self.error = Some(error);
                    }
                    changed = true;
                }
                Some(Ok(Event::Done(result))) => {
                    self.receiver = None;
                    self.restricted = result.is_ok();
                    if let Err(error) = self.save().and(result) {
                        self.error = Some(error);
                    }
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
                self.persisted = false;
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
        for model in [None, Some("test-model".into())] {
            let settings = EnvoySettings {
                model,
                ..Default::default()
            };
            let plan = plan(&catalog(), &settings).unwrap();
            assert!(
                matches!(&plan.request.target, SpawnTarget::NewWorktree { name, base_branch, .. } if name == HOME && base_branch.as_deref() == Some("main"))
            );
            assert!(plan.steps.iter().any(
                |s| matches!(s, Step::CreateWorktree(c) if c.agent.is_none() && c.prompt.is_none())
            ));
            let launches: Vec<_> = plan
                .steps
                .iter()
                .filter_map(|s| match s {
                    Step::CreateTerminal { command, .. } => Some(command),
                    _ => None,
                })
                .collect();
            assert_eq!(launches.len(), 1);
            assert!(launches[0].ends_with(&format!(" -- {}", shell_quote(&plan.request.prompt))));
            assert!(plan.steps.iter().any(|s| matches!(
                s,
                Step::Verify {
                    allow_bypass: false,
                    ..
                }
            )));
        }
        let error = plan(
            &catalog(),
            &EnvoySettings {
                agent: "codex".into(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("Codex MCP tool restrictions"));
    }

    #[test]
    fn launch_pins_tools_permissions_and_the_matching_mcp_binary() {
        let executable = Path::new("/Applications/Myrstack App/Contents/MacOS/myrstack");
        let command = command(Some("sonnet"), "load envoy", executable);
        assert!(command.starts_with("claude --model sonnet --setting-sources user --permission-mode dontAsk --tools Read,Grep,Glob --strict-mcp-config --mcp-config "));
        let mcp =
            serde_json::json!({"mcpServers":{"myrstack":{"command":executable,"args":["mcp"]}}});
        assert!(command.contains(&shell_quote(&mcp.to_string())));
        assert!(!command.contains("--dangerously"));
        let allowed = command
            .split("--allowedTools ")
            .nth(1)
            .unwrap()
            .split(" --disallowedTools")
            .next()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>();
        let mut expected = vec!["Read".to_owned(), "Grep".into(), "Glob".into()];
        expected.extend(
            [
                "get_playbook",
                "list_playbook",
                "fleet_status",
                "list_agents",
                "get_agent",
            ]
            .map(|n| format!("mcp__myrstack__{n}")),
        );
        expected.extend(
            super::super::control::METHODS
                .iter()
                .map(|n| format!("mcp__myrstack__{n}")),
        );
        assert_eq!(allowed, expected);
        let denied = command
            .split("--disallowedTools ")
            .nth(1)
            .unwrap()
            .split(" -- ")
            .next()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>();
        for name in ["Bash", "Monitor", "Write", "Edit", "NotebookEdit"] {
            assert!(denied.contains(&name));
        }
        for tool in crate::mcp::server::tool_definitions() {
            let name = format!("mcp__myrstack__{}", tool["name"].as_str().unwrap());
            assert_ne!(
                allowed.contains(&name.as_str()),
                denied.contains(&name.as_str()),
                "{name}"
            );
        }
        for name in [
            "spawn_session",
            "send_to_agent",
            "press_key",
            "broadcast_to_coordinators",
            "close_agents",
            "set_whip",
            "whip_agents",
            "notify",
            "ask_karl",
            "finish_session",
            "mark_pr_ready",
            "set_filter",
            "focus_agent",
            "open_whip_settings",
            "clean_worktrees",
        ] {
            assert!(
                denied.contains(&format!("mcp__myrstack__{name}").as_str()),
                "{name}"
            );
        }
    }

    fn saved(path: &Path) {
        persist(
            path,
            Some(&SavedSession {
                handle: "envoy-terminal".into(),
                settings: EnvoySettings::default(),
                restricted: true,
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
        assert_eq!(session.running_handle(), None);
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
        assert_eq!(session.running_handle(), Some("envoy-terminal"));
        assert!(session.settings().dry_run);
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
        assert_eq!(session.running_handle(), None);
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
        tx.send(Event::Created {
            handle: "partial".into(),
        })
        .unwrap();
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
        assert_eq!(session.running_handle(), None);
        assert!(session.error.is_some());
        assert!(
            session
                .start(&EnvoySettings::default(), Box::new(|| {}))
                .is_err()
        );
    }

    #[test]
    fn failed_persistence_keeps_stop_available_without_enabling_ingestion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let mut session = Session::load(path.clone(), &EnvoySettings::default());
        std::fs::create_dir(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        session.receiver = Some(rx);
        tx.send(Event::Created {
            handle: "unsaved".into(),
        })
        .unwrap();
        tx.send(Event::Done(Err("disk full".into()))).unwrap();
        session.poll();
        assert_eq!(session.handle(), Some("unsaved"));
        assert_eq!(session.state(), SessionState::Running);
        assert_eq!(session.running_handle(), None);
        assert!(
            session
                .start(&EnvoySettings::default(), Box::new(|| {}))
                .is_err()
        );
    }

    #[test]
    fn launch_uses_playbook_prompt_with_the_session_dry_run_mode() {
        for dry_run in [true, false] {
            let settings = EnvoySettings {
                dry_run,
                ..Default::default()
            };
            let plan = plan(&catalog(), &settings).unwrap();
            assert_eq!(
                plan.request.prompt,
                crate::playbook::envoy_launch_prompt(dry_run)
            );
            assert!(plan.request.prompt.contains("role=envoy"));
            let expected = if dry_run {
                "Dry-run is ON"
            } else {
                "Dry-run is OFF"
            };
            assert!(plan.request.prompt.contains(expected));
        }
    }

    #[test]
    fn dry_run_on_survives_pending_launch_running_save_and_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let live = EnvoySettings {
            dry_run: false,
            ..Default::default()
        };
        let mut session = Session::load(path.clone(), &live);
        let (tx, rx) = mpsc::channel();
        session.receiver = Some(rx);
        session.enable_dry_run().unwrap();
        tx.send(Event::Created {
            handle: "safe".into(),
        })
        .unwrap();
        session.poll();
        assert_eq!(session.running_handle(), None);
        tx.send(Event::Done(Ok(()))).unwrap();
        session.poll();
        assert_eq!(session.running_handle(), Some("safe"));
        assert!(session.settings().dry_run);
        assert!(
            Session::load(path.clone(), &live).settings().dry_run,
            "saving OFF must not downgrade the running session"
        );

        session.settings.dry_run = false;
        session.save().unwrap();
        session.enable_dry_run().unwrap();
        assert!(session.settings().dry_run);
        assert!(Session::load(path.clone(), &live).settings().dry_run);

        session.settings.dry_run = false;
        session.save().unwrap();
        let recovered = Session::load(path.clone(), &EnvoySettings::default());
        assert!(
            recovered.settings().dry_run,
            "preference ON overrides a stale live snapshot"
        );
        assert!(
            Session::load(path, &live).settings().dry_run,
            "recovery persists the safety setting"
        );
    }

    #[test]
    fn legacy_and_codex_sessions_keep_stop_but_cannot_enable_ingestion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        for value in [
            serde_json::json!({"handle":"old", "settings":{}}),
            serde_json::json!({"handle":"old", "settings":{"agent":"codex"}, "restricted":true}),
        ] {
            std::fs::write(&path, value.to_string()).unwrap();
            let mut session = Session::load(path.clone(), &EnvoySettings::default());
            session.reconcile(&FleetData {
                terminals_polled_at_ms: Some(1),
                terminals: vec![Terminal {
                    handle: "old".into(),
                    ..Default::default()
                }],
                ..Default::default()
            });
            assert_eq!(session.handle(), Some("old"));
            assert_eq!(session.running_handle(), None);
            assert!(session.error.as_deref().unwrap().contains("Restart Envoy"));
            session.enable_dry_run().unwrap();
            assert_eq!(session.running_handle(), None);
        }
    }
}

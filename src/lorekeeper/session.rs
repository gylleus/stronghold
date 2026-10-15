//! Restricted launches and recovery of the current Lorekeeper.

use super::{Mode, Settings};
use crate::spawn::exec::{OrcaBackend, SpawnBackend, run_plan_observed};
use crate::spawn::{Agent, Catalog, SpawnPlan, SpawnRequest, SpawnTarget, Step, shell_quote};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

pub const HOME: &str = "lorekeeper-home";
pub const RUN_ENV: &str = "STRONGHOLD_LOREKEEPER_RUN";
const READ_TOOLS: &[&str] = &[
    "search_notes",
    "read_note",
    "list_directory",
    "build_context",
];
const WRITE_TOOLS: &[&str] = &["edit_note", "write_note", "delete_note", "move_note"];

#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub agents: PathBuf,
    pub ledger: PathBuf,
    pub endpoint: String,
    pub home: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let home = super::home()?;
        let agents = crate::paths::agents_dir().context("Agents directory is unavailable")?;
        let ledger = crate::ledger::default_dir().context("Ledger directory is unavailable")?;
        let endpoint = std::env::var("STRONGHOLD_LEDGER_MCP_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8643/mcp".into());
        if agents != home.join("Documents/agents") || ledger != agents.join("ledger") {
            ensure!(
                agents != home.join("Documents/agents")
                    && std::env::var_os("STRONGHOLD_AGENTS_DIR").is_some(),
                "An overridden Ledger requires a separate STRONGHOLD_AGENTS_DIR for snapshots"
            );
            ensure!(
                std::env::var_os("STRONGHOLD_LEDGER_MCP_URL").is_some(),
                "An overridden Ledger requires STRONGHOLD_LEDGER_MCP_URL for its matching server"
            );
        }
        let url = url::Url::parse(&endpoint)?;
        ensure!(
            matches!(url.scheme(), "http" | "https"),
            "Ledger MCP endpoint must use HTTP"
        );
        Ok(Self {
            data: crate::claude::hook::data_dir(),
            agents,
            ledger,
            endpoint,
            home,
        })
    }

    pub fn root(&self) -> PathBuf {
        self.data.join("lorekeeper")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Saved {
    pub run_id: String,
    pub handle: Option<String>,
    pub mode: Mode,
    pub snapshot: PathBuf,
    pub started_ms: i64,
}

pub fn load(root: &Path) -> Result<Option<Saved>> {
    match std::fs::read(root.join("session.json")) {
        Ok(bytes) => {
            let saved: Saved = serde_json::from_slice(&bytes)?;
            ensure!(
                !saved.run_id.is_empty() && saved.run_id.bytes().all(|b| b.is_ascii_digit()),
                "Invalid Lorekeeper identity"
            );
            Ok(Some(saved))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn lock(root: &Path) -> Result<File> {
    std::fs::create_dir_all(root)?;
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("launch.lock"))?)
}

pub fn authenticate(root: &Path, run: Option<&str>, terminal: Option<&str>) -> Result<Saved> {
    let saved = load(root)?.context("No Lorekeeper run is active")?;
    ensure!(
        run == Some(saved.run_id.as_str()),
        "This tool is only available to the Lorekeeper run"
    );
    let terminal = terminal
        .filter(|t| !t.is_empty())
        .context("Missing caller terminal")?;
    ensure!(
        saved.handle.as_deref().is_none_or(|h| h == terminal),
        "Caller is not the Lorekeeper terminal"
    );
    Ok(saved)
}

fn allowed_tools(mode: Mode) -> Vec<String> {
    let mut allowed = [
        "Read",
        "Grep",
        "Glob",
        "mcp__stronghold__lorekeeper_report",
        "mcp__stronghold__lorekeeper_lookup",
        "mcp__stronghold__finish_session",
    ]
    .map(str::to_owned)
    .to_vec();
    allowed.extend(READ_TOOLS.iter().map(|n| format!("mcp__ledger__{n}")));
    if mode == Mode::Live {
        allowed.extend(WRITE_TOOLS.iter().map(|n| format!("mcp__ledger__{n}")));
    }
    allowed
}

pub fn command(
    paths: &Paths,
    saved: &Saved,
    model: &str,
    prompt: &str,
    executable: &Path,
    ledger_tools: &[String],
) -> String {
    let allowed = allowed_tools(saved.mode);
    let mut denied = ["Bash", "Monitor", "Write", "Edit", "NotebookEdit"]
        .map(str::to_owned)
        .to_vec();
    denied.extend(
        crate::mcp::server::tool_definitions()
            .iter()
            .map(|t| format!("mcp__stronghold__{}", t["name"].as_str().unwrap()))
            .filter(|n| !allowed.contains(n)),
    );
    denied.extend(
        ledger_tools
            .iter()
            .map(|n| format!("mcp__ledger__{n}"))
            .filter(|n| !allowed.contains(n)),
    );
    let env = serde_json::json!({
        "STRONGHOLD_DATA_DIR": paths.data,
        "STRONGHOLD_AGENTS_DIR": paths.agents,
        "STRONGHOLD_LEDGER_DIR": paths.ledger,
        "STRONGHOLD_LEDGER_MCP_URL": paths.endpoint,
        (RUN_ENV): saved.run_id,
    });
    let mcp = serde_json::json!({"mcpServers": {
        "stronghold": {"command": executable, "args": ["mcp"], "env": env},
        "ledger": {"type":"http", "url": paths.endpoint},
    }});
    let mut args = vec!["env".to_owned()];
    for (key, value) in env.as_object().unwrap() {
        args.push(shell_quote(&format!("{key}={}", value.as_str().unwrap())));
    }
    args.extend(["claude".into(), "--model".into(), shell_quote(model)]);
    // Removing Bash also removes inherited user allowances for shell writes.
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
    args.extend([
        "--append-system-prompt-file".into(),
        shell_quote(
            &paths
                .data
                .join("lorekeeper-system-prompt.md")
                .to_string_lossy(),
        ),
    ]);
    for path in [paths.home.join("repo"), paths.home.join("orca/workspaces")] {
        args.extend(["--add-dir".into(), shell_quote(&path.to_string_lossy())]);
    }
    args.push("--allowedTools".into());
    args.extend(allowed);
    args.push("--disallowedTools".into());
    args.extend(denied);
    args.extend(["--".into(), shell_quote(prompt)]);
    args.join(" ")
}

fn plan(
    catalog: &Catalog,
    paths: &Paths,
    saved: &Saved,
    settings: &Settings,
    budget: usize,
    ledger_tools: &[String],
) -> Result<SpawnPlan> {
    let repo = ["stronghold", "myrstack"]
        .iter()
        .find_map(|name| catalog.find_repo(name))
        .context("Orca has no stronghold repo")?;
    let home = catalog.worktrees.iter().find(|w| {
        w.repo() == Some(repo.id.as_str())
            && Path::new(&w.path).file_name().is_some_and(|n| n == HOME)
    });
    let target = match home {
        Some(w) => {
            ensure!(
                !w.is_main_worktree && w.path != repo.path && !w.is_archived,
                "Restore lorekeeper-home; it must be a non-main active worktree"
            );
            SpawnTarget::Worktree {
                worktree_id: w.id.clone(),
                path: w.path.clone(),
            }
        }
        None => SpawnTarget::NewWorktree {
            repo_id: repo.id.clone(),
            name: HOME.into(),
            base_branch: Some("main".into()),
        },
    };
    let mut plan = crate::spawn::build_plan(&SpawnRequest {
        target,
        agent: Agent::Claude,
        model: Some(settings.model.clone()),
        effort: None,
        prompt: crate::playbook::lorekeeper_launch_prompt(saved.mode == Mode::DryRun, budget),
        label: Some("Lorekeeper".into()),
    })
    .map_err(|e| anyhow::anyhow!(e.0))?;
    let command = command(
        paths,
        saved,
        &settings.model,
        &plan.request.prompt,
        &std::env::current_exe()?,
        ledger_tools,
    );
    let mut new_terminal = false;
    for step in &mut plan.steps {
        match step {
            Step::CreateWorktree(c) => {
                new_terminal = c.agent.take().is_some();
                c.prompt = None;
            }
            Step::CreateTerminal { command: c, .. } => *c = command.clone(),
            Step::Verify { allow_bypass, .. } => *allow_bypass = false,
            _ => {}
        }
    }
    if new_terminal {
        let i = plan
            .steps
            .iter()
            .position(|s| matches!(s, Step::WaitIdle { .. }))
            .context("Missing terminal wait step")?;
        plan.steps.insert(
            i,
            Step::CreateTerminal {
                worktree_id: None,
                command,
            },
        );
    }
    Ok(plan)
}

fn in_home(t: &crate::orca::Terminal) -> bool {
    let preview = t.preview.as_deref().unwrap_or_default().to_lowercase();
    t.worktree_path
        .as_ref()
        .is_some_and(|p| Path::new(p).file_name().is_some_and(|n| n == HOME))
        && (t.agent_identity.is_some()
            || preview.contains("claude")
            || preview.contains("codex")
            || preview.contains('❯')
            || preview.contains('›'))
}

pub fn launch(paths: &Paths, settings: &Settings, budget: usize) -> Result<Saved> {
    ensure!((1..=200).contains(&budget), "Budget must be 1..200");
    ensure!(
        !settings.model.trim().is_empty(),
        "Choose a Lorekeeper model"
    );
    let root = paths.root();
    let guard = lock(&root)?;
    guard.try_lock().context("Lorekeeper is already starting")?;
    let old = load(&root)?;
    let mut backend = OrcaBackend::default();
    let list: crate::orca::TerminalList = serde_json::from_value(backend.orca(
        &crate::orca::commands::terminal_list_args(),
        Duration::from_secs(30),
    )?)?;
    ensure!(
        !list.truncated,
        "Cannot start Lorekeeper: terminal list is incomplete"
    );
    ensure!(
        !list
            .terminals
            .iter()
            .any(|t| in_home(t) || old.as_ref().and_then(|s| s.handle.as_ref()) == Some(&t.handle)),
        "A Lorekeeper terminal is already live; close it before starting another"
    );
    let now = crate::control::server::now_ms();
    crate::schedule::Run {
        at_ms: now,
        summary: "Lorekeeper launch attempted".into(),
    }
    .append(&root.join("runs.jsonl"))?;
    let playbook = crate::playbook::Playbook::locate()?;
    let prompt = crate::playbook::lorekeeper_system_prompt(&playbook)?;
    let tools = crate::ledger::search::tool_names(&paths.endpoint)?;
    for name in allowed_tools(settings.mode)
        .iter()
        .filter_map(|t| t.strip_prefix("mcp__ledger__"))
    {
        ensure!(
            tools.iter().any(|n| n == name),
            "Ledger is missing required tool {name}"
        );
    }
    std::fs::write(paths.data.join("lorekeeper-system-prompt.md"), prompt)?;
    let snapshot = super::snapshot::create(
        &paths.ledger,
        &paths.agents.join("archive/ledger"),
        &jiff::Timestamp::now().strftime("%Y%m%d-%H%M%S").to_string(),
    )?;
    let mut saved = Saved {
        run_id: format!(
            "{:020}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ),
        handle: None,
        mode: settings.mode,
        snapshot,
        started_ms: now,
    };
    let plan = plan(
        &backend.catalog().map_err(anyhow::Error::msg)?,
        paths,
        &saved,
        settings,
        budget,
        &tools,
    )?;
    super::write_json(&root.join("session.json"), &saved)?;
    let result = run_plan_observed(&plan, &mut backend, &mut |_| {}, &mut |context| {
        if let Some(handle) = &context.handle
            && saved.handle.as_ref() != Some(handle)
        {
            saved.handle = Some(handle.clone());
            super::write_json(&root.join("session.json"), &saved).map_err(|e| e.to_string())?;
        }
        Ok(())
    });
    if result.is_err() && saved.handle.is_none() {
        std::fs::remove_file(root.join("session.json"))?;
    }
    result.map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(saved)
}

pub struct Session {
    pub saved: Option<Saved>,
    pub error: Option<String>,
    root: PathBuf,
    receiver: Option<mpsc::Receiver<Result<Saved, String>>>,
}

impl Session {
    pub fn new(data: &Path) -> Self {
        let root = data.join("lorekeeper");
        let result = load(&root);
        Self {
            saved: result.as_ref().ok().cloned().flatten(),
            error: result.err().map(|e| e.to_string()),
            root,
            receiver: None,
        }
    }
    pub fn busy(&self) -> bool {
        self.receiver.is_some() || self.saved.is_some()
    }
    pub fn handle(&self) -> Option<&str> {
        self.saved.as_ref()?.handle.as_deref()
    }
    /// Launch registration can lag the first terminal poll; launch excludes another agent at home.
    pub fn observed_handle<'a>(
        &'a self,
        terminals: &'a [crate::orca::Terminal],
    ) -> Option<&'a str> {
        let saved = self.saved.as_ref()?;
        saved.handle.as_deref().or_else(|| {
            terminals
                .iter()
                .find(|t| in_home(t))
                .map(|t| t.handle.as_str())
        })
    }

    pub fn start(&mut self, settings: &Settings, wake: crate::actions::Wake) -> Result<()> {
        ensure!(!self.busy(), "Lorekeeper is already tending the Ledger");
        let settings = settings.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("lorekeeper-start".into())
            .spawn(move || {
                let result = Paths::from_env()
                    .and_then(|p| launch(&p, &settings, super::DEFAULT_BUDGET))
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
                wake();
            })?;
        self.receiver = Some(rx);
        self.error = None;
        Ok(())
    }
    pub fn poll(&mut self) {
        if let Some(result) = self.receiver.as_ref().map(|r| r.try_recv()) {
            match result {
                Ok(result) => {
                    self.receiver = None;
                    match result {
                        Ok(saved) => self.saved = Some(saved),
                        Err(e) => self.error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.receiver = None;
                    self.error = Some("Lorekeeper launcher disconnected".into());
                }
                _ => {}
            }
        }
    }
    pub fn reload(&mut self) {
        match load(&self.root) {
            Ok(saved) => self.saved = saved,
            Err(e) => self.error = Some(e.to_string()),
        }
    }
    pub fn reconcile(&mut self, data: &crate::clan::ClanData) {
        let Some(saved) = self.saved.clone() else {
            return;
        };
        if self.receiver.is_some()
            || data
                .terminals_polled_at_ms
                .is_none_or(|at| at <= saved.started_ms)
            || data.errors.iter().any(|e| e.starts_with("terminal list"))
            || data
                .terminals
                .iter()
                .any(|t| in_home(t) || Some(&t.handle) == saved.handle.as_ref())
        {
            return;
        }
        let result = (|| -> Result<()> {
            let guard = lock(&self.root)?;
            if guard.try_lock().is_err() {
                return Ok(());
            }
            if load(&self.root)?.as_ref().map(|s| &s.run_id) == Some(&saved.run_id) {
                std::fs::remove_file(self.root.join("session.json"))?;
                self.saved = None;
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.error = Some(e.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved() -> Saved {
        Saved {
            run_id: "00000000000000000001".into(),
            handle: None,
            mode: Mode::DryRun,
            snapshot: "/archive/snapshot".into(),
            started_ms: 1,
        }
    }

    #[test]
    fn tool_policy_cannot_inherit_shell_or_ledger_writes_in_dry_run() {
        let paths = Paths {
            data: "/scratch data".into(),
            agents: "/scratch/agents".into(),
            ledger: "/scratch/ledger".into(),
            endpoint: "http://localhost:1234/mcp".into(),
            home: "/home/user".into(),
        };
        let ledger_tools = READ_TOOLS
            .iter()
            .chain(WRITE_TOOLS)
            .chain(["delete_project", "create_memory_project"].iter())
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        for mode in [Mode::DryRun, Mode::Live] {
            let s = Saved { mode, ..saved() };
            let cmd = command(
                &paths,
                &s,
                "sonnet",
                "test prompt",
                Path::new("/app/stronghold"),
                &ledger_tools,
            );
            let allowed = cmd
                .split(" --allowedTools ")
                .nth(1)
                .unwrap()
                .split(" --disallowedTools ")
                .next()
                .unwrap();
            let denied = cmd
                .split(" --disallowedTools ")
                .nth(1)
                .unwrap()
                .split(" -- ")
                .next()
                .unwrap();
            assert!(!allowed.contains("Bash"));
            for tool in ["Bash", "Write", "Edit", "NotebookEdit"] {
                assert!(denied.split_whitespace().any(|s| s == tool));
            }
            for tool in &ledger_tools {
                let name = format!("mcp__ledger__{tool}");
                let read = READ_TOOLS.contains(&tool.as_str());
                let write = WRITE_TOOLS.contains(&tool.as_str()) && mode == Mode::Live;
                assert_eq!(allowed.split_whitespace().any(|s| s == name), read || write);
                assert_eq!(
                    denied.split_whitespace().any(|s| s == name),
                    !(read || write)
                );
            }
            assert!(cmd.contains(RUN_ENV));
            assert!(!cmd.contains("\"RUN_ENV\""));
            assert!(cmd.contains("--add-dir /home/user/repo"));
            assert!(cmd.contains("STRONGHOLD_DATA_DIR"));
        }
    }

    #[test]
    fn caller_and_run_identity_are_checked_before_tools_and_across_registration() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = saved();
        super::super::write_json(&dir.path().join("session.json"), &s).unwrap();
        assert!(authenticate(dir.path(), None, Some("terminal")).is_err());
        assert!(authenticate(dir.path(), Some(&s.run_id), None).is_err());
        authenticate(dir.path(), Some(&s.run_id), Some("terminal")).unwrap();
        s.handle = Some("terminal".into());
        super::super::write_json(&dir.path().join("session.json"), &s).unwrap();
        assert!(authenticate(dir.path(), Some(&s.run_id), Some("other")).is_err());
        authenticate(dir.path(), Some(&s.run_id), Some("terminal")).unwrap();
    }

    #[test]
    fn lorekeeper_is_observed_before_launch_registration_and_reload_catches_up() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(dir.path());
        let mut terminals = vec![crate::orca::Terminal {
            handle: "new-agent".into(),
            worktree_path: Some(format!("/repo/{HOME}")),
            agent_identity: Some(crate::orca::AgentIdentity::Claude),
            ..Default::default()
        }];
        assert_eq!(session.observed_handle(&terminals), None, "no launch");
        let path = dir.path().join("lorekeeper/session.json");
        super::super::write_json(&path, &saved()).unwrap();
        session.reload();
        assert_eq!(session.handle(), None);
        assert_eq!(session.observed_handle(&terminals), Some("new-agent"));
        terminals[0].worktree_path = Some("/repo/other".into());
        assert_eq!(
            session.observed_handle(&terminals),
            None,
            "unrelated terminal"
        );
        let registered = Saved {
            handle: Some("registered".into()),
            ..saved()
        };
        super::super::write_json(&path, &registered).unwrap();
        session.reload();
        assert_eq!(session.observed_handle(&terminals), Some("registered"));
    }

    #[test]
    fn live_identity_clears_only_after_fresh_successful_poll() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lorekeeper");
        let s = Saved {
            handle: Some("terminal".into()),
            ..saved()
        };
        super::super::write_json(&root.join("session.json"), &s).unwrap();
        let mut session = Session::new(dir.path());
        let mut data = crate::clan::ClanData {
            terminals_polled_at_ms: Some(1),
            ..Default::default()
        };
        session.reconcile(&data);
        assert!(session.busy());
        data.terminals_polled_at_ms = Some(2);
        data.errors.push("terminal list: unavailable".into());
        session.reconcile(&data);
        assert!(session.busy());
        data.errors.clear();
        let guard = lock(&root).unwrap();
        guard.lock().unwrap();
        session.reconcile(&data);
        assert!(session.busy());
        guard.unlock().unwrap();
        drop(guard);
        session.reconcile(&data);
        assert!(!session.busy(), "{:?}", session.error);
    }
}

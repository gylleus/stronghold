//! How the chat's `claude` process is found and launched.

use std::path::{Component, Path, PathBuf};

use super::permissions;

/// Environment variable naming the `claude` binary.
pub const CLAUDE_BIN_ENV: &str = "MYRSTACK_CLAUDE_BIN";
/// Environment variable choosing the chat's model (e.g. `sonnet`); the CLI default otherwise.
pub const MODEL_ENV: &str = "MYRSTACK_CHAT_MODEL";
/// Unix socket paths longer than this are passed relative to the chat directory.
const MAX_SOCKET_PATH: usize = 100;

/// Appended to Claude Code's system prompt.
pub const SYSTEM_PROMPT: &str = "\
You are the Queen of Karl's agent fleet in Myrstack, speaking for Karl. Karl talks to you here, \
in a chat pane inside the Myrstack app, to get status reports and insights and to command the fleet.

- Use the myrstack MCP tools (fleet_status, list_agents, get_agent, whip_status, alerts, quotas, …) \
for facts about the fleet rather than guessing or reading files. Refer to agents by label and give \
their node id (e.g. `term:term_…`) so Karl can click it.
- If the myrstack server offers a playbook tool (get_playbook), load the standing rules for the \
coordinator role with it before giving orders to the fleet. attendant_brief {run_id} and \
kickoff {task_id} return those playbook templates pre-filled from live fleet data.
- Be concise: short answers, bullet lists, no preamble.
- On a request like \"status report\", give a compact report: who needs input, who is stalled, \
per-run progress, and notable changes since the last report.
- Messages you send to agents go out as the Queen (sender `queen`, text prefixed `Queen:`), and \
agents treat them as Karl's instructions, so be precise and never send on your own initiative.
- Destructive actions (send_to_agent, press_key, broadcast_to_coordinators, close_agents, \
set_whip) only when Karl asks for them; each one is shown to Karl for approval before it runs.";

/// Everything needed to launch the chat process.
#[derive(Debug, Clone)]
pub struct ChatConfig {
    /// The `claude` binary; `None` when it could not be found (the pane shows a setup hint).
    pub binary: Option<PathBuf>,
    /// Working directory of the process and home of `session.json`.
    pub dir: PathBuf,
    pub model: Option<String>,
    pub system_prompt: String,
    /// Extra environment for the process (and so for the MCP servers it starts).
    pub env: Vec<(String, String)>,
    /// Arguments added after the standard ones (tests).
    pub extra_args: Vec<String>,
}

impl ChatConfig {
    /// The app's configuration: `<data dir>/chat`, the binary from [`find_claude`], and the
    /// data dir and control socket passed on so `myrstack mcp` reaches this app.
    pub fn from_env() -> Self {
        let data_dir = crate::claude::hook::data_dir();
        let dir = data_dir.join("chat");
        let cwd = std::env::current_dir().unwrap_or_default();
        let socket = absolute(&cwd, &crate::control::socket_path());
        let mut env = vec![(
            crate::control::SOCKET_ENV.to_owned(),
            socket_for(&dir, &socket).to_string_lossy().into_owned(),
        )];
        if std::env::var_os("MYRSTACK_DATA_DIR").is_some() {
            env.push((
                "MYRSTACK_DATA_DIR".to_owned(),
                absolute(&cwd, &data_dir).to_string_lossy().into_owned(),
            ));
        }
        Self {
            binary: find_claude(),
            dir,
            model: std::env::var(MODEL_ENV).ok().filter(|m| !m.is_empty()),
            system_prompt: SYSTEM_PROMPT.to_owned(),
            env,
            extra_args: Vec::new(),
        }
    }

    /// Arguments for a process opening the session given by `session_args`
    /// (`--resume <id>` or `--session-id <id>`).
    pub fn args(&self, session_args: &[String]) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompts",
            "host",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            "default",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        args.extend(session_args.iter().cloned());
        args.push("--settings".into());
        args.push(permissions::session_settings());
        args.push("--append-system-prompt".into());
        args.push(self.system_prompt.clone());
        if let Some(model) = &self.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// Variables removed from the child's environment: Orca's pane variables (the session hook
/// would map the chat to Myrstack's own pane) and Claude Code's nesting markers.
pub fn is_scrubbed_env(key: &str) -> bool {
    key.starts_with("ORCA_") || matches!(key, "CLAUDECODE" | "CLAUDE_CODE_ENTRYPOINT")
}

/// `$MYRSTACK_CLAUDE_BIN`, else `claude` on `PATH`, else the usual install locations (an app
/// started from Finder does not have the shell's `PATH`).
pub fn find_claude() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os(CLAUDE_BIN_ENV).filter(|b| !b.is_empty()) {
        return Some(PathBuf::from(bin));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = std::env::var_os("PATH").unwrap_or_default();
    find_claude_in(std::env::split_paths(&path), home.as_deref())
}

pub fn find_claude_in(
    path: impl IntoIterator<Item = PathBuf>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = path.into_iter().collect();
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    dirs.into_iter()
        .map(|d| d.join("claude"))
        .find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    }
}

/// The control socket as the chat process (running in `chat_dir`) should see it: absolute
/// when short enough for a socket address, else relative to `chat_dir`.
pub fn socket_for(chat_dir: &Path, socket: &Path) -> PathBuf {
    if socket.as_os_str().len() <= MAX_SOCKET_PATH {
        return socket.to_owned();
    }
    relative_to(chat_dir, socket).unwrap_or_else(|| socket.to_owned())
}

/// `target` relative to `base` (both absolute, without `..`).
fn relative_to(base: &Path, target: &Path) -> Option<PathBuf> {
    let base: Vec<Component> = base.components().collect();
    let target: Vec<Component> = target.components().collect();
    if base.first() != target.first() {
        return None;
    }
    let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..base.len() {
        out.push("..");
    }
    for c in &target[common..] {
        out.push(c.as_os_str());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_open_the_session_with_host_permissions() {
        let config = ChatConfig {
            binary: None,
            dir: "/d/chat".into(),
            model: Some("sonnet".into()),
            system_prompt: "be the queen".into(),
            env: Vec::new(),
            extra_args: Vec::new(),
        };
        let args = config.args(&["--resume".into(), "abc".into()]);
        let joined = args.join(" ");
        assert!(joined.starts_with("-p --input-format stream-json --output-format stream-json"));
        assert!(joined.contains("--permission-prompt-tool stdio"));
        assert!(joined.contains("--permission-mode default"));
        assert!(joined.contains("--resume abc"));
        assert!(joined.contains("--append-system-prompt be the queen"));
        assert!(joined.ends_with("--model sonnet"));
        assert!(!args.iter().any(|a| a == "--strict-mcp-config"));
    }

    #[test]
    fn system_prompt_speaks_as_the_queen() {
        assert!(SYSTEM_PROMPT.contains("Queen of Karl's agent fleet"));
        assert!(SYSTEM_PROMPT.contains("fleet_status"));
        assert!(SYSTEM_PROMPT.contains("status report"));
        assert!(SYSTEM_PROMPT.contains("`Queen:`"));
    }

    #[test]
    fn finds_claude_on_path_then_fallbacks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        let fallback = home.join(".local/bin/claude");
        std::fs::write(&fallback, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            find_claude_in(vec![bin.clone()], Some(&home)),
            Some(fallback.clone())
        );
        let on_path = bin.join("claude");
        std::fs::write(&on_path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&on_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            find_claude_in(vec![bin.clone()], Some(&home)),
            Some(fallback),
            "not executable"
        );
        std::fs::set_permissions(&on_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_claude_in(vec![bin], Some(&home)), Some(on_path));
    }

    #[test]
    fn long_socket_paths_become_relative() {
        let short = Path::new("/tmp/m/control.sock");
        assert_eq!(socket_for(Path::new("/tmp/m/chat"), short), short);
        let deep = format!("/private/tmp/{}/data", "x".repeat(90));
        let socket = PathBuf::from(format!("{deep}/control.sock"));
        let chat = PathBuf::from(format!("{deep}/chat"));
        assert_eq!(socket_for(&chat, &socket), PathBuf::from("../control.sock"));
    }

    #[test]
    fn scrubs_orca_and_nesting_env() {
        assert!(is_scrubbed_env("ORCA_PANE_KEY"));
        assert!(is_scrubbed_env("CLAUDECODE"));
        assert!(!is_scrubbed_env("PATH"));
        assert!(!is_scrubbed_env("MYRSTACK_DATA_DIR"));
    }
}

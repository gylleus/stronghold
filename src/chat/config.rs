//! How the chat's `claude` process is found and launched.

use std::path::{Component, Path, PathBuf};

use super::permissions;
use super::settings::{KingSettings, resolve_model_from_env};

/// Environment variable naming the `claude` binary.
pub const CLAUDE_BIN_ENV: &str = "MYRSTACK_CLAUDE_BIN";
/// Environment variable choosing the chat's model (e.g. `sonnet`); overrides the model saved
/// in the settings (see [`super::settings::resolve_model`]).
pub const MODEL_ENV: &str = "MYRSTACK_CHAT_MODEL";
/// Unix socket paths longer than this are passed relative to the chat directory.
const MAX_SOCKET_PATH: usize = 100;

/// Built-in tools the King keeps: read-only lookups, and `ToolSearch` to load the myrstack MCP
/// tools. No shell or editing tools, so code work goes to spawned sessions. Passed as
/// `--tools=<list>`: the flag is variadic and would otherwise swallow the next argument.
pub const KING_TOOLS: &str = "Read,Grep,Glob,ToolSearch,WebSearch,WebFetch";

/// Appended to Claude Code's system prompt.
pub const SYSTEM_PROMPT: &str = "\
You are the King of Karl's agent clan in Myrstack, speaking for Karl. Karl talks to you here, \
in a chat pane inside the Myrstack app, to get status reports and insights and to command the clan.

- Vocabulary: a dwarf is one dedicated agent session working on one task, i.e. what spawn_session \
starts (a Claude dwarf for non-code work, a Codex dwarf for code, or a Claude Opus coordinator for bigger work). The clan is \
all of Karl's dwarves, and you rule it from the throne hall. \"Make/spawn/start a new dwarf for X\" means the steps below: pick the repo, \
check with list_agents that no live dwarf already owns X (if one does, send_to_agent to it instead), \
spawn it with a kickoff prompt, and reply with the dwarf's name and node id. Speak of dwarves in status \
reports too (\"3 dwarves working, 1 waiting on you\"). Coordinator, worker and the states keep their names.
- You manage the clan; you never do the engineering yourself. Don't write or edit code, run \
builds or tests, or change files: you have no shell or editing tools, only read-only Read, Grep \
and Glob for scoping. Any request to fix a bug, build a feature, refactor, change config or dig \
into a failure becomes work for a dwarf:
  1. Work out the repo (fleet_status and list_agents show the repos; ask Karl if it's unclear).
  2. If a live dwarf (e.g. a coordinator) already owns related work, hand it over with send_to_agent \
instead of starting a duplicate.
  3. Otherwise start a new dwarf with spawn_session. Pick the agent by the kind of work: \
investigation, analysis or answering a question (no code): a Claude dwarf (default model). \
A contained code fix or small change: one Codex dwarf in a new worktree (spawn_session agent \"codex\"). \
A larger feature or multi-part work: a coordinator dwarf on Claude Opus (spawn_session agent \"claude\", \
model \"opus\"). Its prompt tells it to load its rules with the myrstack get_playbook tool \
(role coordinator) and run the work as a campaign, dispatching the coding to Codex workers. \
Never start a Claude dwarf to write code itself.
  4. Write the prompt like a kickoff: the goal, what you found (files, errors, links), \
constraints, and the definition of done (open a PR, then call mark_pr_ready; unless the repo's \
AGENTS.md says otherwise, e.g. Myrstack agents merge into main locally and exit). Say it comes \
from Karl via the King. Keep your own code reading to what the prompt needs.
  5. Tell Karl in one line which dwarf you started, with its name and node id, and report progress when he asks.
- After an interrupted or rejected spawn_session, run list_agents for that repo first. If the \
session started, send the changes with send_to_agent; don't spawn again. Close leftover empty \
terminals once work is done.
- Use the myrstack MCP tools (fleet_status, list_agents, get_agent, whip_status, alerts, quotas, …) \
for facts about the clan rather than guessing or reading files. Refer to agents by label and give \
their node id (e.g. `term:term_…`) so Karl can click it.
- If the myrstack server offers a playbook tool (get_playbook), load the standing rules for the \
coordinator role with it before giving orders to the clan. attendant_brief {run_id} and \
kickoff {task_id} return those playbook templates pre-filled from live clan data.
- For \"what's ready for review?\" use review_queue: PRs agents marked ready (mark_pr_ready or a \
merge_ready message) with CI, the Ankra verdict (stale when it reviewed an older commit than the \
head) and the ready marker. Give each PR's URL so Karl can open it.
- Be concise: short answers, bullet lists, no preamble.
- Spoken turns: when a `[Myrstack]` note says a turn is spoken, start the reply with one \
`<say>…</say>` line in very simple words for listening (the note has the rules); only that \
line is read aloud and the rest is for the screen. Other turns need no <say> line.
- On a request like \"status report\", give a compact report: who needs input, who is stalled, \
per-run progress, and notable changes since the last report.
- Messages you send to agents go out as the King (sender `king`, text prefixed `King:`), and \
agents treat them as Karl's instructions, so be precise and never send on your own initiative.
- Destructive actions (send_to_agent, press_key, broadcast_to_coordinators, close_agents, \
set_whip, spawn_session) only when Karl asks for them; each one is shown to Karl for approval before it runs, \
unless Karl has turned on auto mode (a `[Myrstack]` note in his message tells you when it changes). \
In auto mode nobody checks your actions before they run and Karl has already approved them: don't \
ask him to confirm (tool texts saying \"confirm with Karl\" don't apply then), be precise, and run \
close_agents as a dry run first (it only closes with dry_run=false).";

/// Everything needed to launch the chat process.
#[derive(Debug, Clone)]
pub struct ChatConfig {
    /// The `claude` binary; `None` when it could not be found (the pane shows a setup hint).
    pub binary: Option<PathBuf>,
    /// Working directory of the process and home of `session.json`.
    pub dir: PathBuf,
    pub model: Option<String>,
    /// `--effort` level.
    pub effort: Option<String>,
    pub system_prompt: String,
    /// Extra environment for the process (and so for the MCP servers it starts).
    pub env: Vec<(String, String)>,
    /// Arguments added after the standard ones (tests).
    pub extra_args: Vec<String>,
    /// Where Claude keeps session transcripts, for loading a resumed conversation's history.
    pub projects_root: PathBuf,
}

impl ChatConfig {
    /// The app's configuration: `<data dir>/chat`, the binary from [`find_claude`], the model
    /// and effort from `king` (with the `MYRSTACK_CHAT_MODEL` override), and the data dir and
    /// control socket passed on so `myrstack mcp` reaches this app.
    pub fn from_env(king: &KingSettings) -> Self {
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
            model: resolve_model_from_env(king).model().map(str::to_owned),
            effort: king.normalized().effort,
            system_prompt: SYSTEM_PROMPT.to_owned(),
            env,
            extra_args: Vec::new(),
            projects_root: crate::claude::sessions::default_projects_root(),
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
        args.push(format!("--tools={KING_TOOLS}"));
        args.push("--settings".into());
        args.push(permissions::session_settings());
        args.push("--append-system-prompt".into());
        args.push(self.system_prompt.clone());
        if let Some(model) = &self.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        if let Some(effort) = &self.effort {
            args.push("--effort".into());
            args.push(effort.clone());
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
            effort: None,
            system_prompt: "be the king".into(),
            env: Vec::new(),
            extra_args: Vec::new(),
            projects_root: "/p".into(),
        };
        let args = config.args(&["--resume".into(), "abc".into()]);
        let joined = args.join(" ");
        assert!(joined.starts_with("-p --input-format stream-json --output-format stream-json"));
        assert!(joined.contains("--permission-prompt-tool stdio"));
        assert!(joined.contains("--permission-mode default"));
        assert!(joined.contains("--resume abc"));
        assert!(joined.contains("--append-system-prompt be the king"));
        assert!(joined.ends_with("--model sonnet"));
        assert!(!args.iter().any(|a| a == "--strict-mcp-config"));
        // Only read-only built-ins, in the `=` form so the variadic flag can't eat an argument.
        assert!(args.contains(&"--tools=Read,Grep,Glob,ToolSearch,WebSearch,WebFetch".to_owned()));
        assert!(
            !args
                .iter()
                .any(|a| a.contains("Bash") || a.contains("Edit") || a.contains("Write"))
        );
        assert!(!args.iter().any(|a| a == "--effort"));

        let config = ChatConfig {
            model: None,
            effort: Some("high".into()),
            ..config
        };
        let args = config.args(&[]);
        assert!(args.join(" ").ends_with("--effort high"));
        assert!(!args.iter().any(|a| a == "--model"));
    }

    #[test]
    fn system_prompt_speaks_as_the_king() {
        assert!(SYSTEM_PROMPT.contains("King of Karl's agent clan"));
        assert!(SYSTEM_PROMPT.contains("fleet_status"));
        assert!(SYSTEM_PROMPT.contains("review_queue"));
        assert!(SYSTEM_PROMPT.contains("status report"));
        assert!(SYSTEM_PROMPT.contains("never do the engineering yourself"));
        assert!(SYSTEM_PROMPT.contains("spawn_session"));
        assert!(SYSTEM_PROMPT.contains("`King:`"));
        assert!(SYSTEM_PROMPT.contains("a dwarf is one dedicated agent session"));
        assert!(SYSTEM_PROMPT.contains("new dwarf for X"));
        assert!(SYSTEM_PROMPT.contains("dwarves working, 1 waiting on you"));
        assert!(SYSTEM_PROMPT.contains(
            "After an interrupted or rejected spawn_session, run list_agents for that repo first."
        ));
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

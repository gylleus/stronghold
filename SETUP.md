# Set up Stronghold on macOS

Stronghold is a native Rust/egui app that follows agent sessions in Orca. Build it
from this repository, install it for your macOS user, then connect your agent
clients to its MCP server if you want to control it from a chat.

## Prerequisites

- **Orca and its `orca` CLI.** Keep the Orca app running. Stronghold needs the CLI
  on `PATH`; it also checks `/opt/homebrew/bin/orca` and `/usr/local/bin/orca` when
  launched from Finder. There is no Orca version pin in this repo. The Health
  panel checks whether its runtime is reachable.
- **Rust and Cargo 1.95 or newer.** The locked `eframe` 0.36.2 dependency requires
  Rust 1.95. The app uses Rust's 2024 edition; there is no `rust-toolchain.toml`
  pin. Rust/Cargo 1.95.0 was used to validate this guide.
- **Apple's Xcode Command Line Tools**, including a C/C++ compiler, the macOS SDK,
  `otool`, `install_name_tool` and `codesign`. Full Xcode is not needed for the
  documented build.
- **Python 3**, used by the bundle script, and **[just](https://just.systems/)**,
  used for the install recipes. Python 3.14.5 and just 1.50.0 were available during
  validation; the repo does not pin minimum versions for these tools.
- **Git** and a local checkout of `gylleus/stronghold`. Run the commands below
  from its root, alongside `Cargo.toml` and `justfile`.

The bundle declares macOS 11.0 as its minimum, but that is packaging metadata,
not a promise that every dependency has been tested on older Macs. This guide
was checked on macOS 26.3.1 with Apple Silicon. The build targets the current
machine; the scripts do not produce a universal binary.

For the **King chat**, install and sign in to the `claude` CLI. For the **review
queue**, install and authenticate GitHub's `gh` CLI for the repositories you use.
Codex is optional: it can run dwarves in Orca and connect to Stronghold's MCP
server, while the built-in King uses Claude Code. The Envoy and Ledger have
additional optional services described below.

## Build and install

```bash
just install
```

The [`justfile`](justfile) calls [`scripts/install.sh`](scripts/install.sh), which:

1. Builds the release binary with Cargo.
2. Bundles the binary, icon and non-system dynamic libraries into a signed app.
3. Installs `~/Applications/Stronghold.app` for your user.
4. Links `~/.cargo/bin/stronghold` to the binary inside the app. If `CARGO_HOME`
   is set, the link goes in its `bin/` directory instead.

The app and CLI therefore use the same build. Keep the Cargo bin directory on
`PATH`. No administrator privileges are needed, and `/Applications` is not used.
The installer also replaces an old `Myrstack.app` and preserves an existing
legacy CLI link when it points into that old bundle.

Other verified recipes:

| Command | Result |
| --- | --- |
| `just install-app` | Installs the app but leaves the CLI link alone. |
| `just bundle` | Builds `target/release/Stronghold.app` without installing it. |
| `scripts/install.sh` | Same install as `just install`, without needing just. |
| `cargo run` | Runs a development build without installing an app bundle. |

A development launch uses your normal Stronghold data by default. Set
`STRONGHOLD_DATA_DIR` and `STRONGHOLD_AGENTS_DIR` to scratch directories when
experimenting with a separate instance. A separate data directory does **not**
isolate the live Orca runtime; use the synthetic demo for visual experiments.

## First launch

Open `~/Applications/Stronghold.app` in Finder. Launch Orca and start an agent
session there if the hold is empty. Use **Fit** or `f` to see the whole hold;
selecting a dwarf shows its terminal. In Settings → General, check the name the
King and playbook should use for you.

In Settings → Art, choose **Resurrect 64** and **Hold pixels** to match the README
animation. These settings apply immediately.

The whip is enabled with **Dry run ON** on a new install. It records intended
nudges and escalations without sending them. Inspect its log and settings before
turning dry run off. Run-event forwarding to coordinators is a separate setting
and stays live during dry run.

### Notifications, voice and signing

The installed app asks for notification permission. If you declined, enable it
in System Settings → Notifications → Stronghold. You can send a test:

```bash
stronghold notify "Stronghold" "Notifications are connected"
```

Notifications from the installed app can bring the relevant agent forward when
clicked. A `cargo run` launch uses the `osascript` fallback instead, shown under
Script Editor; its notification clicks cannot select an agent. Both paths also
record alerts inside Stronghold.

Voice shortcuts request microphone access when first used. Voice models are
stored in the app's data directory under `speech-models/`; model downloads need
network access. You can use the dashboard without granting microphone access.

The bundle script signs ad hoc by default; it does not notarize the app. If macOS
blocks a build you trust, review the warning in System Settings → Privacy &
Security and use the offered **Open Anyway** action; see [Apple’s instructions](https://support.apple.com/en-us/102445).
Ad-hoc signatures change with rebuilds, so macOS may request microphone access
again after reinstalling. For a stable local signing identity, create a
code-signing certificate in Keychain Access, then use its exact name:

```bash
STRONGHOLD_SIGN_IDENTITY="Stronghold Dev" scripts/install.sh
```

The bundle identifier is `com.gylleus.stronghold`. Old Myrstack permissions may
not carry over to it.

## Connect the MCP server

`stronghold mcp` is a **stdio** server launched by your MCP client. It is not an
HTTP endpoint and needs no listening port. Live clan tools use the running app's
local control socket. Playbook tools read files directly and work with the app
closed.

Use the installed CLI link so registration survives rebuilding or moving a
worktree. If you used `install-app`, use the absolute path to
`Stronghold.app/Contents/MacOS/stronghold` instead. Substitute your custom Cargo
bin path if you set `CARGO_HOME`.

### Claude Code

```bash
claude mcp add --scope user stronghold -- "$HOME/.cargo/bin/stronghold" mcp
```

Restart the Claude Code session so it loads the server. Ask it to call
`clan_status` or `list_playbook`. The complete tool list is in
[docs/mcp.md](docs/mcp.md).

For exact matching between Orca terminals and Claude sessions, install the
optional SessionStart hook:

```bash
stronghold install-hook
```

It updates Claude Code's settings and writes the hook script into Stronghold's
data directory. Start a new Claude session for the hook to run. Without it,
Stronghold falls back to matching sessions heuristically.

### Codex

Add this table to `~/.codex/config.toml`, replacing `/Users/YOU` with your actual
home directory. Edit an existing `stronghold` table rather than adding a duplicate.

```toml
[mcp_servers.stronghold]
command = "/Users/YOU/.cargo/bin/stronghold"
args = ["mcp"]
```

Restart the Codex session and ask it to call `clan_status` or `list_playbook`.
This uses Codex's documented stdio `command`/`args` configuration; see the
[official MCP guide](https://developers.openai.com/codex/mcp). The Claude
SessionStart hook above is only for Claude Code.

If you override Stronghold's data directory or control socket, pass the same
values to the app and each MCP server. In Claude Code, use the registration's
`-e KEY=value` option before `--`. In Codex, use an environment table:

```toml
[mcp_servers.stronghold.env]
STRONGHOLD_DATA_DIR = "/absolute/path/to/stronghold-data"
STRONGHOLD_AGENTS_DIR = "/absolute/path/to/agents"
```

These are examples of alternate locations, not directories you must create for
a normal install. Shell exports do not automatically reach an app opened from
Finder; custom environments must be supplied to the process that launches it.

## Playbook and shared files

The playbook is read fresh from disk for each request, in this order:

1. `STRONGHOLD_PLAYBOOK_DIR`, when set. It must contain the playbook files; an
   invalid override is an error.
2. The source checkout's `playbook/` path, recorded at build time, if still present.
3. The agents directory, normally `~/Documents/agents`.

Keep the source checkout available, or set `STRONGHOLD_PLAYBOOK_DIR` to a durable
copy of [`playbook/`](playbook/README.md) for both the app and MCP clients. The
installer does not copy the playbook into the app bundle.

```bash
stronghold playbook --list
stronghold playbook coordinator
stronghold playbook worker
```

MCP provides `list_playbook`, `get_playbook`, role prompts and
`stronghold://playbook/<file>` resources. See the
[playbook reference](docs/mcp.md#playbook) for sections and filled templates.

`STRONGHOLD_AGENTS_DIR` changes the shared clan folder. Its default is
`~/Documents/agents`, containing `ledger/`, `archive/` and `notes/`, plus the
playbook fallback. Private organization facts belong in `ORG.md` and
`ENVOY-CONTACTS.md` there, outside this repository.

## Optional services

**Ledger.** Click its chamber (or choose **Open Ledger** in the command palette)
to search and read shared Markdown notes in a modal over your current workspace.
Text search is the default and searches titles and bodies as you type. Use
↑/↓ to move through results, Enter to open and Esc to close. Your query,
selection and scroll positions stay in the session when you close the modal.
Meaning searches semantically; Auto chooses text for IDs and meaning otherwise.
Semantic search uses the local basic-memory MCP service at `http://127.0.0.1:8643/mcp`,
project `ledger`. If unavailable, Stronghold falls back to a case-insensitive
search of local files under the agents directory's `ledger/`. The pane reads
notes; it does not write or delete them. An empty query shows the five newest
entries; search results are limited to 30. The service is installed separately.
The separate Lorekeeper controls are under **Ledger maintenance**. See the
[Ledger modal notes](docs/assets/tests/ledger-modal/README.md) and
[Lorekeeper playbook](playbook/LOREKEEPER-PLAYBOOK.md).

To check the live Ledger connection after installing that separate service:

```bash
cargo test real_ledger_search -- --ignored --nocapture
```

**Envoy.** This optional Slack liaison uses a Claude session in the `envoy-home`
worktree, restricted clan tools, Slack CLI access and the separate
`slack-reply-bridge` service. These services are not installed by `just install`.
Configure Settings → Envoy before starting it from the King controls:

| Setting | Purpose |
| --- | --- |
| `envoy.channels` | Slack channels to listen to in full; empty by default. |
| `envoy.user_id` | Your Slack member ID; Slack's `auth.test` fills it at startup. Mention and DM polling waits until it is known. |
| `envoy.github_owner` | GitHub owner for Slack references such as `repo#N`; empty disables those lookups. |

Keep dry run on while checking the setup. Saving **Dry run ON** takes effect
immediately for subsequent approvals and relays; turning it off and other
changes take effect on the next Start. Polling has a 30-second minimum. Codex
Envoy starts are currently unavailable because its tools cannot yet be
restricted in the same way. See [Envoy tools](docs/envoy-gateway.md) for the guarded
message and approval interface.

## Settings and paths

Settings live in `~/Library/Application Support/Stronghold/settings.json`.
Prefer the app's Settings UI; missing JSON fields use defaults.

| Variable | Default / purpose |
| --- | --- |
| `STRONGHOLD_DATA_DIR` | `~/Library/Application Support/Stronghold`; app state, separate from shared clan files. |
| `STRONGHOLD_SETTINGS` | `<data dir>/settings.json`; override the settings file. |
| `STRONGHOLD_AGENTS_DIR` | `~/Documents/agents`; shared notes, Ledger and archives. |
| `STRONGHOLD_PLAYBOOK_DIR` | Overrides playbook lookup as described above. |
| `STRONGHOLD_CONTROL_SOCKET` | `<data dir>/control.sock`; use the same absolute path in the app and MCP client. |
| `STRONGHOLD_CLAUDE_BIN` | Override discovery of the `claude` CLI used by the King and Envoy. |
| `STRONGHOLD_GH_BIN` | Override discovery of `gh` for GitHub lookups. |
| `STRONGHOLD_CHAT_MODEL` | Overrides Settings → King model selection. |
| `STRONGHOLD_SIGN_IDENTITY` | Signing certificate for the installer; ad hoc by default. |
| `RUST_LOG` | Log filtering; `debug` gives more detail during troubleshooting. |

## Update

Update your source checkout to the desired revision of `main`, then run:

```bash
just install
```

Quit and reopen Stronghold to run the new binary. Restart connected MCP client
sessions too, so their server processes use the new build. The installed CLI
link stays at the same path. App settings and shared files are kept. If you use
a separate playbook copy, update it from the same revision.

## Uninstall

Quit Stronghold and disconnect its MCP server in your clients. Remove the
`stronghold` registration from Claude Code and the `mcp_servers.stronghold`
table (including its environment table, if present) from Codex's config.

If you installed the Claude session hook, remove it **before** removing the app:

```bash
stronghold uninstall-hook
scripts/install.sh --uninstall
```

The uninstall script removes `~/Applications/Stronghold.app` and the CLI link
only if it points into that bundle. It leaves your settings, chat history,
speech models, logs and shared clan files in place. You can back up and remove
Stronghold's app data and logs in Finder separately if desired. The agents
directory is shared with other tools: keep its Ledger, notes and archives.
There is no `just uninstall` recipe. These removal steps were checked in the
source, not executed on the maintainer's machine.

## Troubleshooting

Start with the in-app **Health** panel. It checks Orca, the whip, King, GitHub
CLI, session hook, notifications and Slack reply bridge.

| Symptom | Check |
| --- | --- |
| Build rejects the Rust version | Use Rust/Cargo 1.95 or newer; the locked UI dependency requires it. |
| Linker, SDK or signing tools are missing | Install Apple's Command Line Tools and ensure they are the active developer tools. |
| `just` is unavailable | Install just, or run `scripts/install.sh` directly. |
| Empty hold or Orca runtime unreachable | Open Orca, start an agent session, and check Health. Stronghold needs its CLI as well as the app. |
| `stronghold` is not found | Check your Cargo bin directory on `PATH`; `just install-app` deliberately leaves the CLI link alone. |
| King cannot start | Sign in to Claude Code; check CLI discovery or set `STRONGHOLD_CLAUDE_BIN` for the app process. |
| Review queue cannot refresh PRs | Check that `gh` is installed and authenticated for the relevant repos. |
| MCP says Stronghold is not running | Open the app. Confirm app and MCP server use the same data directory and socket override. |
| MCP playbook lookup fails | Check the source checkout still exists or point `STRONGHOLD_PLAYBOOK_DIR` at the playbook files. |
| No automatic nudges | Check the whip's enabled/dry-run settings, cooldown and stall threshold. Dry run logs intended actions. |
| Terminals match the wrong Claude sessions | Install the SessionStart hook and start new sessions. |
| Notifications or microphone stopped working | Review macOS permissions after rebuilding; ad-hoc signing can cause permission prompts again. |
| An old build opens | Open the installed app explicitly from `~/Applications`; another bundle from `just bundle` may share its bundle ID. |
| Ledger meaning search is unavailable | Check basic-memory separately; local text search still works if note files are present. |

The command palette's **Open log** action opens
`~/Library/Logs/Stronghold/stronghold.log` in Console. Logs rotate at 5 MB with
three files kept. With `STRONGHOLD_DATA_DIR` set, the log is in that directory.
For a terminal launch with additional logging:

```bash
RUST_LOG=debug cargo run
```

For local development, the repository's required checks are:

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
```

Commands and paths above were checked against the [`justfile`](justfile),
[installer](scripts/install.sh), [bundler](scripts/bundle.py),
[CLI entry point](src/main.rs), [MCP reference](docs/mcp.md) and configuration
code. No install, uninstall or permission-reset action was run to validate this
written guide.

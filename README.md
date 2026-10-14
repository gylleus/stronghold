# Stronghold

A native Rust/egui desktop app that shows the agent clan running in the
Orca agent IDE as dwarves in a hold: coordinators, their
workers and their subagents. Select a dwarf to see its terminal and type into it.

The hold is the only clan view. Settings → Art controls pixel size and palette;
`f` or Fit shows the whole hold, and selecting a run or repo focuses its rooms.

**Dwarf**: one dedicated agent session working on a task (what the King, or "New dwarf…",
starts). The clan is all of Karl's dwarves; the King is its hub.

## Run

```bash
cargo run
```

Requires the `orca` CLI on `PATH`. Set `RUST_LOG=debug` for more logging. The app logs to stderr and to
`~/Library/Logs/Stronghold/stronghold.log` (rotated at 5 MB, three files kept); "Open log" in the command
palette opens it in Console.

## Ledger

Click the Ledger chamber to search and read the clan's knowledge store. With the query
empty, the pane lists the five newest entries. Enter a query and press Enter or Search;
Auto searches by meaning, switching to text for IDs such as `ABC-123` and `myr-bas`.
Meaning and Text can also be selected explicitly. Results show title, folder, note type,
age and a snippet; selecting one renders its Markdown. Open in default editor opens the
actual file with macOS `open`.

Search uses the local basic-memory MCP service at `http://127.0.0.1:8643/mcp`, project
`ledger`. If it is unavailable, the pane says so and searches local files for all the
query's literal terms, ignoring case. Results are limited to 30. Reading uses the files in
`~/Documents/agents/ledger`; the pane never writes or deletes entries. Searches and file
reads run on a background thread. The HTTP transport uses `ureq`, without a CLI or shell
PATH dependency, and accepts both JSON and SSE responses after MCP initialization.

The optional live check (requires `ai.basic-memory.server` running) is:

```bash
cargo test real_ledger_search -- --ignored --nocapture
```

## Install as a macOS app

```bash
scripts/install.sh              # release build → Stronghold.app → ~/Applications, CLI linked
scripts/install.sh --app-only   # same, but leave ~/.cargo/bin/stronghold alone
scripts/install.sh --uninstall  # remove the app (and the CLI link if it points into it)
```

`just install` (or `just install-app`, `just bundle`) runs the same; see the [`justfile`](justfile).

The app lives in `~/Applications/Stronghold.app` (nothing is written to `/Applications`); open it
from Finder, Spotlight or `open -a Stronghold`. `~/.cargo/bin/stronghold` becomes a symlink to
`Stronghold.app/Contents/MacOS/stronghold`, so the CLI (`stronghold mcp` for the registered MCP
server, `ctl`, `playbook`, `notify`, `pr-ready`, `say`, `install-hook`) and the app are always the same build, and bundled dylibs
resolve through the link. Uninstalling removes that link; get a standalone CLI back with
`cargo install --path .`.

Building only the bundle: `cargo build --release && scripts/bundle.py` writes
`target/release/Stronghold.app`. [`scripts/bundle.py`](scripts/bundle.py) writes the Info.plist
(`com.gylleus.stronghold`, version from `Cargo.toml`), copies every non-system dylib the binary
loads (`otool -L`, recursively) into `Contents/Frameworks` and relinks it through `@rpath`, and
signs the bundle. The icon is `packaging/Stronghold.icns`, rendered by
[`scripts/make_icon.py`](scripts/make_icon.py) (re-run it after changing the design).

**Notifications.** Run from the app, notifications come from Stronghold itself
(`UNUserNotificationCenter`), and clicking one brings Stronghold forward with that agent selected.
macOS asks once whether Stronghold may notify; if you declined, turn it on in System Settings →
Notifications → Stronghold. `stronghold notify "title" "body"` posts a test notification (add
`--quiet` to skip the prompt: provisional permission, delivered quietly to Notification
Center). Run with `cargo run`, notifications go through `osascript` (shown under Script Editor;
clicks cannot reach Stronghold). Either way they are listed in the in-app **Alerts** menu.

**Microphone and signing.** The app asks for microphone access the first time a voice shortcut
listens. It is signed ad hoc (`codesign --sign -`), and macOS remembers permissions per
signature, which changes with every build: after a reinstall, macOS may ask for the microphone
again. To keep permissions across builds, sign with a stable identity, e.g. a self-signed
"Stronghold Dev" code-signing certificate made in Keychain Access:
`STRONGHOLD_SIGN_IDENTITY="Stronghold Dev" scripts/install.sh`. To forget a decision:
`tccutil reset Microphone com.gylleus.stronghold`.

## Talk to it from a chat

`stronghold mcp` is an MCP server that lets Claude Code (or another MCP client) read the clan
status and drive the dashboard while Stronghold runs:

```bash
cargo build --release
claude mcp add --scope user stronghold -- "$PWD/target/release/stronghold" mcp
```

Then ask Claude things like "what's the clan status?" or "who needs my input?". The tools
are listed in [docs/mcp.md](docs/mcp.md); the local API it proxies to is in
[docs/control-api.md](docs/control-api.md).

`stronghold install-hook` adds a Claude Code `SessionStart` hook that records which Orca
terminal runs which Claude session, so Stronghold can match them exactly (`uninstall-hook`
removes it).

## Envoy

Start and stop the Envoy from the King controls. It uses one Claude session in the dedicated
`envoy-home` worktree, with read-only clan tools and the guarded `envoy_*` tools. Codex starts
are unavailable until its MCP tools can be restricted. Sessions from before these restrictions
must be stopped and restarted; if the identity file was lost, close the existing agent in
`envoy-home` before starting another.

In Settings → Envoy, saving **Dry run ON** takes effect immediately for subsequent approvals
and relays, including a session still starting. Turning it off and other changes take effect
on the next Start. Polling has a 30-second minimum, including values loaded from disk.

## Playbook

Stronghold owns the clan playbook, the rules coordinators, attendants, workers and the Envoy
run by. The
files live in [`playbook/`](playbook/README.md) and are served straight from disk, stamped
with the git commit they came from, so no app needs to run:

- MCP prompts `/mcp__stronghold__coordinator`, `/mcp__stronghold__attendant`,
  `/mcp__stronghold__worker` and `/mcp__stronghold__envoy` load a role's rules into a Claude
  Code session.
- MCP tools `get_playbook` / `list_playbook` and resources `stronghold://playbook/<file>`.
- `stronghold playbook [coordinator|attendant|worker|envoy|<file>] [--section <title>] [--list]`,
  and `stronghold playbook attendant-brief <run_id>` / `kickoff <task_id>` for the filled
  templates.

`STRONGHOLD_PLAYBOOK_DIR` overrides where the files are read from (else the repo's
`playbook/`, else `~/Documents/agents`). Org-specific facts the playbook refers to (GitHub
org, colleagues, ticket key, Slack contacts) are kept out of the repo, in
`~/Documents/agents/ORG.md` and `~/Documents/agents/ENVOY-CONTACTS.md`. Details:
[docs/mcp.md](docs/mcp.md#playbook).

See [PLAN.md](PLAN.md) for the design and milestones.

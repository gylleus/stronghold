# Myrstack

A native Rust/egui desktop app that shows the agent clan running in the
Orca agent IDE as a dwarf farm: coordinators, their
workers and their subagents. Select a dwarf to see its terminal and type into it.

The farm is the only clan view. Settings → Art controls pixel size and palette;
`f` or Fit shows the whole farm, and selecting a run or repo focuses its rooms.

**Dwarf**: one dedicated agent session working on a task (what the King, or "New dwarf…",
starts). The clan is all of Karl's dwarves; the King is its hub.

## Run

```bash
cargo run
```

Requires the `orca` CLI on `PATH`. Set `RUST_LOG=debug` for more logging. The app logs to stderr and to
`~/Library/Logs/Myrstack/myrstack.log` (rotated at 5 MB, three files kept); "Open log" in the command
palette opens it in Console.

## Install as a macOS app

```bash
scripts/install.sh              # release build → Myrstack.app → ~/Applications, CLI linked
scripts/install.sh --app-only   # same, but leave ~/.cargo/bin/myrstack alone
scripts/install.sh --uninstall  # remove the app (and the CLI link if it points into it)
```

`just install` (or `just install-app`, `just bundle`) runs the same; see the [`justfile`](justfile).

The app lives in `~/Applications/Myrstack.app` (nothing is written to `/Applications`); open it
from Finder, Spotlight or `open -a Myrstack`. `~/.cargo/bin/myrstack` becomes a symlink to
`Myrstack.app/Contents/MacOS/myrstack`, so the CLI (`myrstack mcp` for the registered MCP
server, `ctl`, `playbook`, `notify`) and the app are always the same build, and bundled dylibs
resolve through the link. Uninstalling removes that link; get a standalone CLI back with
`cargo install --path .`.

Building only the bundle: `cargo build --release && scripts/bundle.py` writes
`target/release/Myrstack.app`. [`scripts/bundle.py`](scripts/bundle.py) writes the Info.plist
(`ai.depict.myrstack`, version from `Cargo.toml`), copies every non-system dylib the binary
loads (`otool -L`, recursively) into `Contents/Frameworks` and relinks it through `@rpath`, and
signs the bundle. The icon is `packaging/Myrstack.icns`, rendered by
[`scripts/make_icon.py`](scripts/make_icon.py) (re-run it after changing the design).

**Notifications.** Run from the app, notifications come from Myrstack itself
(`UNUserNotificationCenter`), and clicking one brings Myrstack forward with that agent selected.
macOS asks once whether Myrstack may notify; if you declined, turn it on in System Settings →
Notifications → Myrstack. `myrstack notify "title" "body"` posts a test notification (add
`--quiet` to skip the prompt: provisional permission, delivered quietly to Notification
Center). Run with `cargo run`, notifications go through `osascript` (shown under Script Editor;
clicks cannot reach Myrstack). Either way they are listed in the in-app **Alerts** menu.

**Microphone and signing.** The app asks for microphone access the first time a voice shortcut
listens. It is signed ad hoc (`codesign --sign -`), and macOS remembers permissions per
signature, which changes with every build: after a reinstall, macOS may ask for the microphone
again. To keep permissions across builds, sign with a stable identity, e.g. a self-signed
"Myrstack Dev" code-signing certificate made in Keychain Access:
`MYRSTACK_SIGN_IDENTITY="Myrstack Dev" scripts/install.sh`. To forget a decision:
`tccutil reset Microphone ai.depict.myrstack`.

## Talk to it from a chat

`myrstack mcp` is an MCP server that lets Claude Code (or another MCP client) read the clan
status and drive the dashboard while Myrstack runs:

```bash
cargo build --release
claude mcp add --scope user myrstack -- "$PWD/target/release/myrstack" mcp
```

Then ask Claude things like "what's the clan status?" or "who needs my input?". The tools
are listed in [docs/mcp.md](docs/mcp.md); the local API it proxies to is in
[docs/control-api.md](docs/control-api.md).

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

Myrstack owns the clan playbook, the rules coordinators, attendants and workers run by. The
files live in [`playbook/`](playbook/README.md) and are served straight from disk, stamped
with the git commit they came from, so no app needs to run:

- MCP prompts `/mcp__myrstack__coordinator`, `/mcp__myrstack__attendant` and
  `/mcp__myrstack__worker` load a role's rules into a Claude Code session.
- MCP tools `get_playbook` / `list_playbook` and resources `myrstack://playbook/<file>`.
- `myrstack playbook [coordinator|attendant|worker|<file>] [--section <title>] [--list]`.

`MYRSTACK_PLAYBOOK_DIR` overrides where the files are read from. Details:
[docs/mcp.md](docs/mcp.md#playbook).

See [PLAN.md](PLAN.md) for the design and milestones.

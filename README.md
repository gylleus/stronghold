# Myrstack

A native Rust/egui desktop app that shows the agent fleet running in the
Orca agent IDE as a live graph: coordinators, their
workers and their subagents. Select a node to see its terminal and type into it.

## Run

```bash
cargo run
```

Requires the `orca` CLI on `PATH`. Set `RUST_LOG=debug` for more logging.

## Talk to it from a chat

`myrstack mcp` is an MCP server that lets Claude Code (or another MCP client) read the fleet
status and drive the dashboard while Myrstack runs:

```bash
cargo build --release
claude mcp add --scope user myrstack -- "$PWD/target/release/myrstack" mcp
```

Then ask Claude things like "what's the fleet status?" or "who needs my input?". The tools
are listed in [docs/mcp.md](docs/mcp.md); the local API it proxies to is in
[docs/control-api.md](docs/control-api.md).

## Playbook

Myrstack owns the fleet playbook, the rules coordinators, attendants and workers run by. The
files live in [`playbook/`](playbook/README.md) and are served straight from disk, stamped
with the git commit they came from, so no app needs to run:

- MCP prompts `/mcp__myrstack__coordinator`, `/mcp__myrstack__attendant` and
  `/mcp__myrstack__worker` load a role's rules into a Claude Code session.
- MCP tools `get_playbook` / `list_playbook` and resources `myrstack://playbook/<file>`.
- `myrstack playbook [coordinator|attendant|worker|<file>] [--section <title>] [--list]`.

`MYRSTACK_PLAYBOOK_DIR` overrides where the files are read from. Details:
[docs/mcp.md](docs/mcp.md#playbook).

See [PLAN.md](PLAN.md) for the design and milestones.

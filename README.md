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

See [PLAN.md](PLAN.md) for the design and milestones.

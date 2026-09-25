# Myrstack

A native Rust/egui desktop app that shows the agent fleet running in the
Orca agent IDE as a live graph: coordinators, their
workers and their subagents. Select a node to see its terminal and type into it.

## Run

```bash
cargo run
```

Requires the `orca` CLI on `PATH`. Set `RUST_LOG=debug` for more logging.

See [PLAN.md](PLAN.md) for the design and milestones.

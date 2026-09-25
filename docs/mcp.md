# Myrstack MCP server

`myrstack mcp` is an [MCP](https://modelcontextprotocol.io) server. It lets a chat client such
as Claude Code ask for the fleet status (every coordinator and agent, and its state) and
control the Myrstack dashboard. It proxies each tool call to the running app's
[control API](control-api.md).

## Register it

```bash
cargo build --release
claude mcp add --scope user myrstack -- /path/to/myrstack/target/release/myrstack mcp
```

Start Myrstack (`myrstack`) as usual, then ask the chat things like "what's the fleet
status?", "who needs my input?", "show me the screen of the AI agent playbook coordinator" or
"focus the graph on it".

The server honours `MYRSTACK_CONTROL_SOCKET` and `MYRSTACK_DATA_DIR` in the same way the app
does, so pass the same values to both if you override them
(`claude mcp add … -e MYRSTACK_CONTROL_SOCKET=/path/control.sock -- …`).

## Transport and protocol

- stdio, newline-delimited JSON-RPC 2.0. stdout carries only protocol messages. Logs go to
  stderr (`RUST_LOG=info` for more; the default is `warn`).
- Methods: `initialize`, `ping`, `tools/list`, `tools/call`. Notifications such as
  `notifications/initialized` and `notifications/cancelled` are accepted and ignored.
  Batches (JSON arrays) are rejected.
- Protocol versions: `2025-11-25`, `2025-06-18`, `2025-03-26` and `2024-11-05`. `initialize`
  echoes the client's version when it is one of these, and otherwise answers with the newest.
  Results include `structuredContent` from `2025-06-18` on.
- It is hand-rolled rather than built on the `rmcp` SDK. The protocol surface is four methods,
  and `rmcp` would pull an async runtime into an otherwise synchronous binary.
- Each tool call opens a fresh connection to the control socket, so the chat can start before
  the app does and survives app restarts.

## Tools

Every tool returns a compact, chat-ready text block. Most also return `structuredContent`
(the underlying control API result or a compact version of it).

| Tool | Arguments | Control method | Hints |
|---|---|---|---|
| `fleet_status` | – | `status`, `agents` ×3, `quotas` | read-only |
| `list_agents` | `state`, `kind` (string or list), `run`, `repo`, `needs_input`, `live`, `text`, `limit` (default 50, max 500) | `agents` | read-only |
| `get_agent` | `id`, `lines` (default 40) | `agent` | read-only |
| `send_to_agent` | `id`, `text`, `submit` (default true) | `send` | destructive |
| `press_key` | `id`, `key`: `enter` \| `esc` \| `interrupt` | `enter` / `esc` / `interrupt` | destructive |
| `close_agents` | `ids`, `whole_tab` (default false), `dry_run` (**default true**) | `close` | destructive |
| `broadcast_to_coordinators` | `text`, plus `runs` or `all: true` | `broadcast` | destructive |
| `whip_status` | `limit` (log entries, default 10) | `whip` | read-only |
| `set_whip` | `enabled`, `dry_run`, `notify` (at least one) | `whip_settings` | destructive, idempotent |
| `focus_agent` | `id` (or `queen`), `raise_window` | `focus` | idempotent |
| `set_filter` | `clear`, `repos`, `states`, `kinds`, `text`, `show_loose`, `show_released` | `filter` | idempotent |
| `set_view` | `view`: `graph` \| `table` | `view` | idempotent |
| `fit_graph` | – | `fit` | idempotent |
| `open_whip_settings` | – | `open_whip_settings` | idempotent |
| `alerts` | `limit` (default 20) | `alerts` | read-only |
| `quotas` | – | `quotas` | read-only |

All tools set `openWorldHint: false`. Agent ids are node ids from `list_agents` or
`fleet_status` (`term:…`, `dispatch:…`, `sub:…`). A terminal handle or an exact label also
works.

### Safety

- `close_agents` is a **dry run unless the chat passes `dry_run: false`**. The dry run
  returns the plan: targets, the `orca` commands it would run, and warnings such as a run
  losing its coordinator. The chat should show the plan to Karl before closing.
- `send_to_agent`, `press_key`, `broadcast_to_coordinators` and `set_whip` act on real
  agents or on saved settings. Their descriptions tell the model to confirm with Karl first.

### Errors

- Tool failures come back as results with `isError: true` and a readable message:
  - no app on the socket (missing socket or connection refused):
    ``Myrstack isn't running — start it with `myrstack`.``
  - `not_found`: `Not found: no agent "x". Use list_agents to find agent ids.`
  - `refused`, `unavailable`, `timeout`, and bad arguments (`Invalid arguments: …`) each
    get their own prefix.
- JSON-RPC errors are used only for protocol problems: parse error (-32700), invalid request
  (-32600), unknown method (-32601), and unknown tool or malformed `tools/call` params
  (-32602).

### Headless fallback

When the app is not running, `fleet_status` and `list_agents` still answer from one direct
poll of Orca. That poll covers terminals, runs, workers, tasks, the inbox and quotas, and is
reused for 15 s. The text starts with a note that it is a fallback. It has no whip, Claude
subagent or dashboard data, and no screen-based prompt detection, so the needs-input list
relies on Orca's previews only. `structuredContent.fallback` is `true`. Every other tool
returns the "isn't running" error.

## Example

Trimmed from a live session:

```
→ {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fleet_status","arguments":{}}}
Fleet: working · 116 agents, 37 live (coordinator 7, worker 85, terminal 24)
States: working 3, idle 31, done 47, failed 35

Needs input: nobody
Stalled: none

Runs (8):
| Run | Objective | Coordinator | Tasks | Workers | Active | Flags |
|---|---|---|---|---|---|---|
| run_36e69d11f0b2 | recs-core composition: step rotation (depict-804)… | AI agent playbook (working) | 0/9 done, 1 dispatched, 5 ready, 3 failed | 19 (1 working, 1 idle) | just now |  |
| run_7c25cdcf46e0 | commerce-platform wave 2026-09-23: HACK-76 genera… | cp-wave-0923-ATTENDANT brief (idle) | 50/58 done, 1 dispatched, 1 ready, 6 failed | 46 (1 working, 1 idle) | 23s ago |  |
…
Whip: on, dry-run · 0 stall findings · 0 escalated · last tick 10s ago
Quotas: Claude 5h 25% (resets in 41m), wk 13% (resets in 6d 2h) · Codex wk 0% (resets in 6d 4h)

→ close_agents {"ids":["term:term_2bf8…"]}
DRY RUN: nothing was closed. This is the plan:
Targets (1):
- ..pict/realtime `term:term_2bf8…` (coordinator)
Steps:
- `orca terminal close --terminal=term_2bf8…`
Warnings:
- run depict-xyx: … loses its coordinator; its workers keep running (0 open tasks)
To really close, confirm with Karl, then call close_agents again with dry_run=false.
```

To try the server by hand, pipe JSON-RPC lines into it:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"sh","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fleet_status","arguments":{}}}' \
  | myrstack mcp
```

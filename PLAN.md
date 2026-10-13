# Myrstack — plan

A native desktop view of the agent clan running in Orca: a dwarf farm of
coordinators, their worker agents and their subagents, where clicking a dwarf
shows that agent's terminal and lets you type into it.

Orca stays the place agents are launched and the message bus they talk over.
Myrstack only observes and talks to them through Orca.

## Goals

1. **See the clan:** every live agent as a pixel dwarf, with runs in rooms and
   pose and location showing state (working, needs input, stalled, done).
2. **Talk to any agent:** select a node to see its screen and send it input.
3. **Whip stalled coordinators:** detect runs that have gone quiet with work
   still open, then nudge them or alert you.

Not a goal: launching agents, replacing Orca's UI, or interacting with
subagents (they are short-lived and read-only here).

## Why a native egui app

Myrstack gets no special access to agent terminals by being native. Orca owns
the PTYs, so every read and write goes through Orca either way. Native is still
the better fit:

- `orca terminal read --screen` returns plain text lines without colors, so a
  monospace text panel is enough. xterm.js, the main reason to use a web UI,
  isn't needed.
- It's one binary with no localhost server, it can call `orca` as a child
  process directly, and it can raise macOS notifications and bind a global hotkey.
- The farm uses egui's painter, a room layout and a camera for pan and zoom.


## Data sources (verified against Orca 1.4.192)

Each `orca` call takes about 0.1 s, so polling the CLI is fine.

| Need | Source |
|---|---|
| Live terminals | `orca terminal list --json`: `handle`, `worktreePath`, `branch`, `title`, `agentIdentity` (claude/codex), `lastOutputAt` (ms), `preview`, `connected`, `writable` |
| Runs → coordinator | `orca orchestration run-list --json`: `id`, `objective`, `coordinator_handle`, `updated_at` |
| Run → workers | `orca orchestration worker-list --json`: `runId`, `taskId`, `agentTerminalHandle`, `workerState`, `dispatchStatus`, `terminalState` |
| Screen of a node | `orca terminal read --terminal <h> --screen --json`: `result.terminal.tail[]`, `status` |
| Input to a node | `orca terminal send --terminal <h> --text … --enter` (`--interrupt` for Esc/Ctrl-C) |
| Focus in Orca | `orca terminal switch --terminal <h>` |
| Task board | `orca orchestration task-list --run <id> --json`: `id`, `task_title`, `status` (pending, ready, dispatched, completed, failed, blocked), `parent_id`, `deps`, `result`, `created_by_terminal_handle`, `completed_at` |
| Progress signals | `orca orchestration inbox --json`: bus messages with `from_handle`, `to_handle`, `type` (e.g. `heartbeat`), `subject`, `payload` (`{taskId, dispatchId, phase}`), `created_at` |
| Nudge a coordinator | `orca orchestration send` (bus) or `orca terminal send` |
| Subagents | `~/.claude/projects/<encoded cwd>/<session-id>/subagents/agent-*.jsonl` plus `agent-*.meta.json` (`agentType`, `description`, `toolUseId`, `spawnDepth`, `model`) |

Mapping a Claude terminal to its session: encode `worktreePath` the way Claude
does (`/` becomes `-`), then take the most recently written `<session-id>.jsonl`
in that directory. This is ambiguous when two Claude terminals share a
worktree. The fix is a tiny `SessionStart` hook in `~/.claude/settings.json`
that appends `{ORCA_PANE_KEY, session_id}` to a Myrstack file. Orca's own
hook shows `ORCA_PANE_KEY` and `ORCA_TAB_ID` are in the agent's environment.

Subagent state: running while its jsonl was written recently and the parent
transcript has no `tool_result` for its `toolUseId`, done once it does.

## Clan model

```
Run room:      [coordinator] ── [worker terminal] ── [subagent] ── [sub-subagent]
Loose:         terminals that aren't in any run (plain Claude or Codex tabs)
```

- **Node kinds:** Coordinator, Worker, Terminal (no run), Subagent.
- **Node state:** Working (output < 30 s ago), NeedsInput (screen shows a
  permission or choice prompt), Idle, Stalled (see Whip), Done/Released,
  Failed.
- Released workers and finished subagents fade out after a set time so
  the farm stays readable.

## Architecture

Single crate to start, split later if needed.

```
src/
  main.rs          eframe setup
  orca/            CLI wrapper: spawn `orca … --json`, serde types, error on ok:false
  claude/          transcript/subagent scanner (notify-based watcher on ~/.claude/projects)
  model.rs         ClanSnapshot { runs, nodes, edges }, built from orca and claude data
  poller.rs        background thread: list every 2 s, selected screen every 500 ms;
                   sends snapshots over a channel; UI never blocks on I/O
  whip.rs          stall rules plus nudge/notify actions
  ui/
    clan_view.rs  selection, filters, badges, tooltip data and focus requests
    farm/      rooms, pixel dwarves, camera, input and traffic effects
    dwarf/         code-drawn parts, poses and pixel rendering
    terminal.rs    screen panel, input box, Send / Interrupt / Open in Orca
    sidebar.rs     run list, filters (repo, state), stall list
```

Crates: `eframe`/`egui`, `serde`/`serde_json`,
`notify`, `crossbeam-channel`, `notify-rust` or `mac-notification-sys`.

## Milestones

**M0 — Skeleton.** eframe window; `orca` client with typed `terminal list`,
`run-list` and `worker-list`; a clan snapshot refreshing every 2 s.
*Done when:* the snapshot matches `orca terminal list`.

**M1 — Farm.** Build the snapshot, render runs in rooms with their coordinators,
show agent state through dwarf poses and locations, group loose terminals by repo,
and fade released workers. *Done when:* the commerce-platform wave run shows its
coordinator and workers correctly.

**M2 — Attach.** Select a node to open the side panel with a 500 ms screen
poll, an input box (Enter sends with `--enter`), Interrupt, and Open in Orca.
Detect a NeedsInput state from the screen. *Done when:* you can answer a
permission prompt in a worker without opening Orca.

**M3 — Subagents.** Watch the transcript directories, attach subagents under
their parent terminal, add the `SessionStart` mapping hook, and show a
read-only tail of the transcript on select. *Done when:* background subagents
appear and disappear as a coordinator runs a review.

**M4 — Whip.** Nudges on its own (see "Whip design"). Dry-run mode first,
then live. *Done when:* a deliberately paused worker, and separately a paused
coordinator, each get one nudge per cooldown, escalate to a notification when
ignored, and are never nudged while they're waiting on you.

**M5 — Polish.** Persist room and pane layouts and filters, search, global hotkey,
keyboard navigation (j/k across nodes, `i` to type), and cost/usage if Orca
exposes it.

## Whip design

The task board is Orca's own orchestration tasks. There's no separate board to
build. Progress is recorded as bus messages. Workers already send
`type: heartbeat` messages with `{taskId, phase}` in the payload.

Every loop tick (default 60 s):

1. For each run, list tasks and the workers that own them
   (`worker-list`: `taskId` → `agentTerminalHandle`).
2. **Last progress** for a task is the newest of: a bus message from the
   owning terminal that refers to the task, the task's status change, or,
   as a fallback, the owner's `lastOutputAt`.
3. **Worker stalled:** task is `dispatched`, last progress is older than
   the stall threshold, and the owner isn't NeedsInput. Nudge the owner.
4. **Coordinator stalled:** the run has tasks that are `ready` and unassigned,
   or that finished with no follow-up, and the coordinator has had no output
   or bus activity within the threshold. Nudge the coordinator.
5. **Runs without tasks** (coordinators not using the board): fall back to
   the coordinator terminal's `lastOutputAt`.

A nudge is `orca orchestration send` to the owner, saying it comes from
Myrstack and asking for a heartbeat with its current phase, or to carry on
with its task. If the terminal is idle at a prompt and the bus message
doesn't wake it, fall back to `orca terminal send --enter`.

Escalation per task: nudge → second nudge after the cooldown → macOS
notification and a red node. Never nudge a NeedsInput terminal; that one
is waiting on you, so notify instead.

Settings (global): stall threshold (default 15 min), cooldown (default
10 min), max nudges before escalating (default 2), dry-run toggle. Every
action goes to an action log shown in the sidebar.

**Agent-side convention**, to add to the clan playbook: workers send a
heartbeat with `taskId` and `phase` at each phase change, and at least
every threshold/2 during long steps. Coordinators update task status when
they act on results.

## Decisions

- The whip nudges on its own, based on Orca tasks plus heartbeats.
- Stall threshold is one global setting.
- Codex terminals are included.
- Remote Orca environments (`orca environment list`) come later.

## Open questions

- "Coordinator of coordinators": a text box that sends one message to all
  coordinators, or to a filtered set. It's cheap to add in M2.

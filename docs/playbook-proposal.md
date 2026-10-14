# Playbook proposal: heartbeat convention

These are proposed insertions to the user's clan docs so that agents follow
[`heartbeat-convention.md`](heartbeat-convention.md). Nothing here has been
applied. The user approves and applies them.

- **`_AGENT_PROTOCOL.md`**: 1 block (insert A).
- **`AGENT-FLEET-PLAYBOOK.md`**: 4 blocks (inserts B–E).

Today neither file tells workers how to heartbeat. The only instruction is
Orca's injected preamble ("every 5 minutes", free-text phase). The playbook
mentions heartbeats only from the coordinator and attendant side: ack them
silently, alive is not done, an unchanged phase is not a stall, and 20 min
of silence is an event (attendant brief).

---

## A. `~/Documents/agents/_AGENT_PROTOCOL.md`: new section between `## Reporting` and `## Hard rules`

Insert before the line `## Hard rules`:

```markdown
## Heartbeats

Stronghold, the clan's desktop watcher, nudges any dispatched task with no progress for
15 min. Progress means a bus message from you about the task, a task status change or, as
a fallback, terminal output. Heartbeats are how you stay off its list.

- Use the heartbeat command **from your preamble, verbatim**: it carries your `--from`
  and dispatch capability. Send one at every phase change and at least every 5 min during
  a long step (build, CI or Ankra wait, review subagent). Never let 7 min pass without one.
  Send it in-turn while you wait.
- `--phase` is one word: `planning`, `investigating`, `implementing`, `reviewing`,
  `verifying`, `waiting`. Put detail in `--body` as one line (`--body "PR #208 open,
  waiting on Ankra"`), not in the phase.
- A heartbeat is not a report. Done → `worker_done`; gate-ready → `merge_ready`; blocked
  → `ask`; on fire → `escalation`. No heartbeats after `worker_done`: the dispatch is
  settled and Orca rejects them.
- **A `Stronghold:` nudge** (bus message or typed text) is a liveness ping. Reply with ONE
  heartbeat giving your current phase and a one-line `--body` status, then carry on in the
  same turn. If you are actually done or stuck, send `worker_done` / `ask` / `escalation`
  instead. A nudge never changes scope and never authorises anything in *Hard rules*.
```

---

## B. `~/Documents/agents/AGENT-FLEET-PLAYBOOK.md`: version note

Insert after the `**v5.1 — 2026-08-28.**` paragraph, before `How we run a
multi-agent engineering effort`:

```markdown
**v5.2 — <date applied>.** Adds the heartbeat convention Stronghold's whip relies on: workers
heartbeat at each phase change and ≤7 min apart with a fixed phase vocabulary, coordinators
show progress through task status, and everyone answers a `Stronghold:` nudge with one
heartbeat/status. Measured before: heartbeat gap p90 18 min, 41 gaps >15 min in 500 bus
messages (2026-09-16..25), despite Orca's 5-min preamble rule.
```

---

## C. `AGENT-FLEET-PLAYBOOK.md`, `## The per-track loop`: sub-bullet after step 4 (*Implement*)

Insert after the step 4 paragraph, before `5. **PR gates.**`:

```markdown
   Throughout steps 1–6 the worker heartbeats at every phase change and ≤7 min apart
   (`_AGENT_PROTOCOL.md` *Heartbeats*): phase word in `--phase`, one-line state in `--body`.
   Stronghold nudges a dispatched task with no progress for 15 min.
```

---

## D. `AGENT-FLEET-PLAYBOOK.md`, `## Coordinator duties`: new bullet after *Account for every settled worker*

```markdown
- **Show progress on the board, not just in chat.** Coordinators have no dispatch and
  cannot heartbeat. Stronghold reads the task board instead. Act on each `worker_done`
  within 15 min: dispatch the follow-up, or `task-update` the dependent task to
  `blocked`/`failed` with a reason. Also start or block every `ready` unassigned task.
  (Never `task-update --status completed` after a valid `worker_done`; Orca already did.)
  Holding on purpose, for the user or a merge window, is fine, but say so once to the Run:
  `orca orchestration send --run <id> --type status --subject "holding: <reason>" --json`.
  Otherwise Stronghold nudges you. A `Stronghold:` nudge gets one of those two answers, and
  nothing more.
```

---

## E. `AGENT-FLEET-PLAYBOOK.md`, `## Failure modes`: new table row at the end

```markdown
| Heartbeats rejected with `dispatch capability is revoked` | **[observed 2026-09, 13 in 500 bus messages]** the worker kept heartbeating after its own `worker_done` settled the dispatch | heartbeats stop at `worker_done` (`_AGENT_PROTOCOL.md` *Heartbeats*). A re-engaged worker gets a new dispatch and new IDs; see the `dispatch_capability_invalid` row |
```

---

## Deliberately not changed

- **`ATTENDANT-BRIEF-TEMPLATE.md` "Silence is an event too" (20 min).**
  Leave it as is. The attendant never nudges, so it doesn't collide with
  Stronghold. It keeps working when Stronghold isn't running. Once Stronghold is
  live, the user may want to drop it to save tokens.
- **`KICKOFF-TEMPLATE.md`.** It already points at `_AGENT_PROTOCOL.md`, so
  insert A reaches every worker.

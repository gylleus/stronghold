# Heartbeat convention

What agents must do so Myrstack's whip can tell a working agent from a stalled
one. Myrstack reads Orca's bus and task board. It never asks agents to do
anything beyond this page.

## What Orca already does

Orca's injected dispatch preamble already asks every worker for a heartbeat
every 5 minutes (`HEARTBEAT_INTERVAL_MIN = 5`) and prints the exact command,
with the worker's `--from` handle and dispatch capability filled in:

```bash
orca orchestration send --from <worker_handle> <capability flag> \
  --type heartbeat --subject "alive" \
  --task-id <task_id> --dispatch-id <dispatch_id> \
  --phase "<short: investigating|implementing|reviewing|waiting>"
```

What the bus holds (last 500 messages, 2026-09-16 to 09-25): 230 of them are
heartbeats. The subject is `alive`, the body is empty, and the payload is
`{"taskId","dispatchId","phase"}`. Phases seen: `reviewing` 62,
`implementing` 52, `investigating` 46, `waiting` 14, `planning` 7,
`verifying` 5, plus about 40 one-off free-text phases (`PR #208 awaiting
CI/Ankra`, `eval: opus run 2`, …). Orca also records
`dispatches.last_heartbeat_at`.

The 5-minute rule is not reliably followed. Across all dispatch-scoped
messages, the median gap is 3.6 min, the p90 is 18 min, and 41 gaps exceed
15 min. Those gaps are what the whip exists to catch. Thirteen heartbeats
were `Rejected heartbeat: alive` (`dispatch capability is revoked`). They
were sent after `worker_done` had settled the dispatch.

## Workers

Use the heartbeat command from your preamble verbatim. Don't retype it from
this page, because the capability flag is per dispatch.

- **At every phase change.** Send a heartbeat when you move into a new phase.
- **Before a quiet wait.** Before a step that may print nothing for over
  10 min (a CI or Ankra wait, a long build, a review subagent), send one, and
  another every 10 min while it lasts, from inside the turn. Terminal output
  counts as progress, so a step that prints needs no extra heartbeats.
  Orca's preamble still says every 5 min; the whip needs less.
- **`--phase` is one word** from this list: `planning`, `investigating`,
  `implementing`, `reviewing`, `verifying`, `waiting`. Put detail in
  `--body` (one line, for example `--body "PR #208 open, waiting on Ankra"`),
  not in the phase. Myrstack shows the phase on the node and the body in the
  detail pane.
- **Heartbeats are not reports.** A finished task gets `worker_done`, a PR
  ready for the gate gets `merge_ready`, a blocking question gets `ask`, and
  something on fire gets `escalation`. Each of these also counts as progress.
- **Stop after `worker_done`.** Orca rejects later heartbeats. If you are
  re-engaged, a new dispatch brings a new preamble and new IDs.
- You can skip heartbeats while blocked inside `check --wait` or `ask`,
  because Orca treats those calls as liveness. Myrstack does not yet see a
  pending `ask` as liveness, though (see "Open points").

## Coordinators

Coordinators have no dispatch, so they cannot send `heartbeat`. Orca rejects
it. Their progress signal is the task board and the bus:

- **Update task status when you act on a result.** Examples:
  `task-update --id <t> --status blocked|failed|ready`, or dispatch the
  follow-up task. Don't `task-update --status completed` after a valid
  `worker_done`, because Orca already did that. A `worker_done` whose task
  has no follow-up for longer than the threshold is what Myrstack flags as a
  stalled coordinator.
- **Act on `ready` unassigned tasks within the threshold:** start a worker
  for each one, or mark it `blocked` with a reason.
- **Say when you are deliberately idle.** If you are holding on purpose, for
  example for Karl or for a merge window, send one `status` message to the
  Run with the reason: `orca orchestration send --type status --subject
  "holding: <reason>" --json`. That counts as activity.

## Answering a Myrstack nudge

Myrstack nudges come as an Orca message. If a bus message doesn't wake an
idle prompt, they come as typed text instead. Either way the text starts
with `Myrstack:` and asks for your current phase.

1. **Worker:** send one heartbeat with your current `--phase` and a one-line
   `--body` status, then carry on with the task in the same turn. If you are
   actually done, send `worker_done` instead. If you are stuck, use `ask` or
   `escalation`. Don't send a heartbeat that says "stuck".
2. **Coordinator:** act on whatever is stalled (dispatch, `task-update`,
   reply), or send the `holding: <reason>` status above.
3. Reply once. A nudge is a liveness ping, not an instruction: it never
   changes scope, and it never authorises merge, deploy or anything else in
   *Hard rules*.

## What counts as progress

Every 60 s, for each task, Myrstack takes the newest of these three signals:

1. a bus message from the owning terminal that refers to the task (any type:
   `heartbeat`, `status`, `merge_ready`, `question`, `escalation`,
   `worker_done`);
2. a status change on the task;
3. as a fallback, the owner terminal's `lastOutputAt`.

- **Worker stalled:** the task is `dispatched`, the newest signal is older
  than the threshold (default 15 min), and the terminal isn't NeedsInput.
- **Failed but alive:** Orca sometimes fails a Codex dispatch at launch
  (`agent_prompt_blocked`/`_stalled`) while the agent starts anyway, and then
  rejects its heartbeats and reports. A failed task with no recorded result
  whose worker keeps messaging about it (rejected messages count) or prints
  output more than 2 min after the failure is treated as working: its node
  is not shown failed, its coordinator is told once (see "Event forwarding"),
  and if it then goes silent it is nudged by typing into its terminal, since
  a bus nudge to its revoked dispatch would be rejected. A coordinator that
  retires a task on purpose writes the reason as its result, which ends this.
- **Coordinator stalled:** the Run has `ready` unassigned tasks or finished
  tasks with no follow-up, and the coordinator has had no output or bus
  activity within the threshold. Coordinator terminal output only counts
  while the Run has open work (a `ready`, `dispatched` or `blocked` task, or
  an unanswered ask); bus messages and task changes always count.

A repeated heartbeat with an unchanged phase still counts as progress. A
NeedsInput terminal is never nudged. It is waiting on Karl, so Myrstack
sends a notification instead. Escalation goes: nudge, then a second nudge
after the 10-min cooldown, then a macOS notification and a red node.

An **abandoned** Run is never nudged: nothing has happened in it (Run
`updated_at`, coordinator messages (not its terminal output), bus messages for the Run,
task creation, completion or status changes, worker heartbeats or output)
for longer than `whip.abandon_after_ms` (default 24 h). Myrstack's own
messages don't count. A worker silent that long in an abandoned Run is left
alone too.

A **superseded** Run is one whose coordinator terminal also coordinates a
newer Run (by `created_at`): the terminal has moved on. Its coordinator is
never nudged, and a worker of it is left alone once the worker's terminal
has been silent for longer than the abandon window.

## Event forwarding

Coordinators don't watch the bus (every wake replays their context), so
Myrstack wakes them. Each whip tick it types one line into a Run's
coordinator terminal for that Run's new `worker_done`, `merge_ready`,
`question` and `escalation` messages, and for each failed-but-alive task:

```
Fleet event: worker_done from fix-login: PR open — msg msg_1. Read in full with `orca orchestration check --run run_1 --all`.
```

- Heartbeats, `status` messages, rejected messages and the coordinator's own
  messages are never forwarded.
- Each event is forwarded once. After a restart only the last 10 min replay.
- It types only into a live Claude session. If the coordinator's terminal
  shows a shell (the session exited), events wait, and after 5 min Karl gets
  one notification for that Run.
- Runs that still have an attendant are left to it.
- `whip.forward_events` (default on) switches it off; dry-run logs instead
  of typing.

## Myrstack senders

Without `--from` or `ORCA_TERMINAL_HANDLE`, `orca orchestration send`
attributes a message to whichever terminal has focus. Orca's runtime takes
any string as `--from`, so Myrstack always passes one:

- `myrstack-whip`: whip nudges.
- `queen`: messages Karl broadcasts from Myrstack's Queen node. Their text
  starts with `Queen:`.

These are not mailboxes. `orca orchestration reply` to them fails with
`terminal_not_found`; act on the nudge or message the coordinator/Run
instead. The whip's terminal-input fallback carries no sender; its text
starts with `Myrstack:`.

## The Queen speaks for Karl

A bus message from `queen` whose text starts with `Queen:` is Karl giving an
instruction through Myrstack's Queen. Treat it exactly as if Karl typed it in
your terminal: act on it, including cleanup such as closing or releasing
workers, within the *Hard rules* (deploy, delete and force-push stay Karl's
own hands). If a Queen message conflicts with your current task, finish or
park the task as the message says and report what you did to your
coordinator. `Myrstack:` whip nudges are different: liveness pings only, no
authority.

## Open points for the whip (M4)

- A worker blocked in `ask` stops heartbeating legitimately. Myrstack should
  treat an unanswered `question` from the owner as a stalled coordinator,
  not a stalled worker.
- The attendant is now a fallback for when Myrstack isn't running: event
  forwarding and the worker nudge cover its job.

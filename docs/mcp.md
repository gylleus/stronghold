# Stronghold MCP server

`stronghold mcp` is an [MCP](https://modelcontextprotocol.io) server. It lets a chat client such
as Claude Code ask for the clan status (every coordinator and agent, and its state) and
control the Stronghold dashboard. It proxies each clan tool call to the running app's
[control API](control-api.md). It also serves the clan [playbook](#playbook) straight from
disk, which works without the app.

## Register it

```bash
cargo build --release
claude mcp add --scope user stronghold -- /path/to/stronghold/target/release/stronghold mcp
```

Start Stronghold (`stronghold`) as usual, then ask the chat things like "what's the clan
status?", "who needs my input?", "show me the screen of the AI agent playbook coordinator" or
"focus the hold on it".

The server honours `STRONGHOLD_CONTROL_SOCKET` and `STRONGHOLD_DATA_DIR` in the same way the app
does, so pass the same values to both if you override them
(`claude mcp add … -e STRONGHOLD_CONTROL_SOCKET=/path/control.sock -- …`).

## Transport and protocol

- stdio, newline-delimited JSON-RPC 2.0. stdout carries only protocol messages. Logs go to
  stderr (`RUST_LOG=info` for more; the default is `warn`).
- Methods: `initialize`, `ping`, `tools/list`, `tools/call`, `prompts/list`, `prompts/get`,
  `resources/list`, `resources/read` (and an empty `resources/templates/list`). `initialize`
  advertises the `tools`, `prompts` and `resources` capabilities. Notifications such as
  `notifications/initialized` and `notifications/cancelled` are accepted and ignored.
  Batches (JSON arrays) are rejected.
- Protocol versions: `2025-11-25`, `2025-06-18`, `2025-03-26` and `2024-11-05`. `initialize`
  echoes the client's version when it is one of these, and otherwise answers with the newest.
  Results include `structuredContent` from `2025-06-18` on.
- It is hand-rolled rather than built on the `rmcp` SDK. The protocol surface is a handful of methods,
  and `rmcp` would pull an async runtime into an otherwise synchronous binary.
- Each tool call opens a fresh connection to the control socket, so the chat can start before
  the app does and survives app restarts.

## Tools

Every tool returns a compact, chat-ready text block. Most also return `structuredContent`
(the underlying control API result or a compact version of it).

| Tool | Arguments | Control method | Hints |
|---|---|---|---|
| `clan_status` | – | `status`, `agents` ×4, `quotas` | read-only |
| `list_agents` | `state`, `kind` (string or list), `run`, `repo`, `needs_input`, `live`, `parked`, `text`, `limit` (default 50, max 500) | `agents` | read-only |
| `get_agent` | `id`, `lines` (default 40) | `agent` | read-only |
| `send_to_agent` | `id`, `text`, `submit` (default true) | `send` | destructive |
| `press_key` | `id`, `key`: `enter` \| `esc` \| `interrupt` | `enter` / `esc` / `interrupt` | destructive |
| `close_agents` | `ids`, `whole_tab` (default false), `dry_run` (**default true**) | `close` | destructive |
| `park_agents` | `ids`, `wake` (default false) | `park` | destructive |
| `broadcast_to_coordinators` | `text`, plus `runs` or `all: true` | `broadcast` | destructive |
| `whip_status` | `limit` (log entries, default 10) | `whip` | read-only |
| `set_whip` | `enabled`, `dry_run`, `notify` (at least one) | `whip_settings` | destructive, idempotent |
| `whip_agents` | `ids`, or `stalled: true` | `whip` (action) | destructive |
| `focus_agent` | `id` (or `king`), `raise_window` | `focus` | idempotent |
| `set_filter` | `clear`, `repos`, `states`, `kinds`, `text`, `show_loose`, `show_released` | `filter` | idempotent |
| `open_whip_settings` | – | `open_whip_settings` | idempotent |
| `alerts` | `limit` (default 20), `unread_only` | `alerts` | read-only |
| `quotas` | – | `quotas` | read-only |
| `health` | – | `health` | read-only |
| `get_playbook` | `role` (default `coordinator`) or `file`, `section` | – (reads the playbook) | read-only |
| `list_playbook` | – | – (reads the playbook) | read-only |
| `attendant_brief` | `run_id` | `clan_data` (headless fallback) | read-only |
| `kickoff` | `task_id` | `clan_data` (headless fallback) | read-only |
| `mark_pr_ready` | `pr_url`, `title`, `summary` | `pr_ready` (queued in `pr-ready.jsonl` when the app is down) | not destructive, idempotent |
| `review_queue` | – | `review_queue` (the saved queue when the app is down) | read-only |
| `notify` | `title`, `message`, `severity` (`info` \| `warn` \| `urgent`), `url`, `agent` | `notify` (queued in `pending-notifications.jsonl` when the app is down) | not destructive |
| `tell_king` | `message`, `kind` (`info` \| `question` \| `blocked`), `url` | – (durable `king-inbox.json`, online or offline) | not destructive |
| `king_inbox` | `limit` (default 20, max 100) | – (reads the durable inbox) | read-only |
| `ask_karl` | `question`, `context`, `severity` (`info` \| `warn` \| `urgent`), `url` | `ask` | not destructive |
| `finish_session` | `summary`, `approval` (`karl` \| `trivial`), `outcome` (`done` \| `handed_over` \| `abandoned`), `pr_url`, `force` | `finish` (a `dry_run` check, then the real one) | destructive |
| `spawn_session` | `mode` (`worktree` \| `new_worktree` \| `worker`), `prompt`, plus `worktree`, `repo` + `name`, `base_branch`, `run_id`, `terminal`, `title`, `label`, `agent`, `model`, `effort`, `allow_suffix` | `spawn` | destructive |
| `clean_worktrees` | `dry_run` (**default true**), `ids` (needed with `dry_run: false`) | – (runs `orca`, `git`, `gh` itself) | destructive |

`mark_pr_ready` is for clan agents: the server is registered at user scope, so every Claude
session (clan agents in Orca terminals included) has it. It sends the caller's Orca identity
(`ORCA_TERMINAL_HANDLE`, `ORCA_PANE_KEY`, `ORCA_TAB_ID` from the `stronghold mcp` process's
environment, which it inherits from the agent's terminal) so the PR shows under the agent's
node. Agents without MCP (Codex) run `stronghold pr-ready <url> [--title …] [--summary …]`,
which does the same. See the control API's [Review queue](control-api.md#review-queue).

`notify` is how any agent reaches Karl without going through the King: a macOS and/or
in-app pop-up with a sound (per his Notifications settings), kept in his inbox (the bell in
the top bar, `alerts`, `clan_status`) and attributed to the caller's terminal like
`mark_pr_ready`. When to call it is the clan playbook's _Human in the loop_ rule, repeated in
the tool description and the server instructions (`src/mcp/notify.rs`, `rules_text!`): blocked
on Karl (warn, or urgent), handovers (info, with the url and the next step), never progress;
workers go through their coordinator. Identical
repeats collapse, each agent gets at most 5 per 10 minutes, and quiet mode lets only `urgent`
through. When the app isn't running, `notify` appends the notification to
`<data dir>/pending-notifications.jsonl` (like `mark_pr_ready`'s `pr-ready.jsonl`) and answers
that it is queued; on start the app takes the file into the inbox with the sender's
attribution and link, popping up only those sent in the last 5 minutes. See the control API's
[Notifications](control-api.md#notifications).

`ask_karl` is for a question an agent waits on Karl to answer, which Stronghold can't see on
the screen (it only detects permission prompts and menus). It notifies Karl with the question
(source `agent`, so it isn't swallowed by the needs-input cooldown, which it then holds so
the node's needs-input notification doesn't repeat it) and marks the caller's terminal
waiting: the node shows needs input, with `Asks: <question>` as its preview, until the Claude
transcript shows a prompt after the ask (for agents without a transcript, until the terminal
has produced output 2 minutes after it), or the terminal goes. As a backstop, a Claude
session whose last turn ended (`stop_reason: end_turn`) on a line ending in `?` within its
last four lines also shows needs input, until the next prompt (`src/claude/turns.rs`,
`src/ask.rs`). Waiting nodes are not nudged by the whip.

The Envoy is the exception: its questions still enter Karl's inbox and notify him, with the
same terminal target and thread link, but never mark its node needs input. Its `ask` reply
reports `waiting: false, nonblocking: true`; unmatched callers also have `waiting: false`
but `nonblocking: false`. Neither explicit asks nor inferred transcript questions block the
Envoy's next event batch. Karl can select the question's terminal from his inbox and type
his answer as a new input whenever he is ready. Real permission prompts, a busy terminal,
and unsubmitted composer text still block automated delivery.

`finish_session` lets an agent close its own terminal once Karl said it can go. Karl often
has follow-ups, so an agent that thinks it's done asks him first (`ask_karl`, info: "Done:
… Anything else, or can I close?") and stays; `approval` (required) is `karl` once he said go,
or `trivial` for a tiny one-shot task with nothing to follow up. Without it the call is
refused before reaching the app. The server then
asks the app (`finish` with `dry_run`) who the caller is and what the clan says: workers are
refused (their coordinator reuses or releases them), and a coordinator with open tasks or an
agent with a PR still in the review queue gets a problem. It then checks the caller's git
worktree (the node's, else its own working directory): uncommitted changes (the `.beads/`
export aside) and commits neither merged into the default branch (`origin/HEAD`, `main`,
`master`) nor pushed to their upstream are problems. Problems refuse unless `force` is set;
forced problems are recorded. Past the guards the app records the outcome in
`finished.jsonl` in the data dir, notifies Karl (info, or warn when forced; the PR url folds
into its review notification), shows the node done and closes the terminal 2 s after replying
with the same plan as `close_agents` (pane only). The worktree and branch are always kept.
A closed finished terminal stays in the hold as a fading done dwarf for 10 minutes. When to
call both is in the tool descriptions and the server instructions (`src/mcp/lifecycle.rs`,
`rules_text!`), and in the playbook's _Human in the loop_.

`clean_worktrees` is the top bar's worktree cleanup (`src/worktrees.rs`) as a tool, so the
King can run it on request. A worktree is removable only when no Orca terminal, process
(by working directory, not by a command line that mentions the path) or live dispatch is in
it, it has no uncommitted changes (the beads export aside), no untracked files other than
agent notes (`*.md`) and logs (`*.log`), and no open PR, it is older than two hours and not
pinned, and its work is in a base branch (its base ref, `origin/HEAD`, local or remote
`main`/`master`) or on GitHub: HEAD merged, every commit patch-equivalent in the base
(`git cherry`), its changes already in the base's tree (`git merge-tree`, a squash merge),
HEAD equal to or inside a merged PR's `headRefOid` (branch or detached HEAD), or its PR merged
or closed with HEAD on a remote. Anything unknown (git unreadable, `gh` failing) keeps it.
Other untracked files are listed by top-level folder with their size and keep it. The plan
also gives the size of each kept worktree with its build output (`target`, `.venv`,
`node_modules`, `.yarn`). The dry run returns the plan with `removable_ids`; a real run
removes only the `ids` passed that a fresh evaluation still finds removable, re-checks each
right before removal, copies its notes to `~/Documents/agents/notes/<repo>/<worktree>/`
(checked, never overwriting) and deletes its logs, then runs `orca worktree rm` (no
`--force`), and logs every attempt to `worktree-cleanup.jsonl` in the data dir. While it runs,
`worktree-sweep.json` there makes the app show its hammer, breaking down the removed
worktrees' rubble heaps in the mine (one heap on its surface per open worktree). Folders under `~/orca/workspaces`
that Orca doesn't list are reported, never touched.

All tools set `openWorldHint: false`. Agent ids are node ids from `list_agents` or
`clan_status` (`term:…`, `dispatch:…`, `sub:…`). A terminal handle or an exact label also
works.

### Parked agents

Karl parks an agent he is done with for now but wants to keep (`park_agents`, or Park in a
dwarf's context menu). A parked agent sleeps in the resting area's chamber in the hold. Its terminal, session and worktree are kept. It
is out of the King's state, `by_state`, `needs_input` and `stalled` (`clan.parked` lists
it), the whip never nudges, escalates or notifies about it, "Close shown…" skips it and the
worktree cleanup keeps its worktree. `close_agents` closes it only when it is named, with a
warning in the plan. `send_to_agent` wakes it before typing (`woke: true` in the result);
`park_agents` with `wake: true`, Wake in the context menu, or typing to it in its panel do
too. The parked set is saved in `<data dir>/parked.json` (by `term:<handle>`), so it survives
restarts; an agent whose terminal closes is dropped from it. Parking is separate from
`released` (Orca released the worker's terminal; the node fades out) and from `done` (what
the agent's output says): a parked agent keeps whatever state Orca reports.

### Safety

- `close_agents` is a **dry run unless the chat passes `dry_run: false`**. The dry run
  returns the plan: targets, the `orca` commands it would run, and warnings such as a run
  losing its coordinator. The chat should show the plan to Karl before closing.
- `clean_worktrees` is a **dry run unless the chat passes `dry_run: false` with the `ids`
  Karl approved** from a dry run's plan.
- `send_to_agent`, `press_key`, `broadcast_to_coordinators` and `set_whip` act on real
  agents or on saved settings. Their descriptions tell the model to confirm with Karl first; the King in auto mode
  skips that (Karl approved the King's actions up front).
- `whip_agents` sends Stronghold nudges now. With `ids` they are live even when the automatic
  whip is in dry-run, and threshold and cooldown are ignored; the King, subagents, released
  workers and dead terminals are skipped with a reason, and agents waiting on Karl's input
  get a warning. With `stalled: true` it runs an immediate whip pass under the normal rules
  (dry-run included).

### Errors

- Tool failures come back as results with `isError: true` and a readable message:
  - no app on the socket (missing socket or connection refused):
    ``Stronghold isn't running — start it with `stronghold`.``
  - `not_found`: `Not found: no agent "x". Use list_agents to find agent ids.`
  - `refused`, `unavailable`, `timeout`, and bad arguments (`Invalid arguments: …`) each
    get their own prefix.
- JSON-RPC errors are used only for protocol problems: parse error (-32700), invalid request
  (-32600), unknown method (-32601), and unknown tool or malformed `tools/call` params
  (-32602).

### Headless fallback

When the app is not running, `clan_status` and `list_agents` still answer from one direct
poll of Orca. That poll covers terminals, runs, workers, tasks, the inbox and quotas, and is
reused for 15 s. The text starts with a note that it is a fallback. It has no whip, Claude
subagent or dashboard data, and no screen-based prompt detection, so the needs-input list
relies on Orca's previews only. `structuredContent.fallback` is `true`. Every other tool
returns the "isn't running" error.

## Playbook

Stronghold owns the clan playbook: the Markdown rules in the repo's `playbook/` dir
(`AGENT-FLEET-PLAYBOOK.md`, `_AGENT_PROTOCOL.md`, `ATTENDANT-BRIEF-TEMPLATE.md`,
`KICKOFF-TEMPLATE.md`, `README.md`). A coordinator starting up fetches its rules from
Stronghold instead of reading a file path. The files are read from disk on every request, so
an edit shows up at once, and the app need not be running.

**Where it reads from**, first match wins:

1. `STRONGHOLD_PLAYBOOK_DIR` (an error if it holds neither `AGENT-FLEET-PLAYBOOK.md` nor
   `_AGENT_PROTOCOL.md`).
2. The repo's `playbook/` dir, as baked in at build time, if it still exists.
3. `~/Documents/agents` (symlinks to the repo files).

**Roles:**

| Role | Gets |
|---|---|
| `coordinator` | `AGENT-FLEET-PLAYBOOK.md` + `_AGENT_PROTOCOL.md` |
| `attendant` | `ATTENDANT-BRIEF-TEMPLATE.md` + the playbook's *Coordinator duties* and *Token economy* sections |
| `worker` | `_AGENT_PROTOCOL.md` |

Any file can also be asked for by name (case-insensitive, `.md` and a leading `_` optional).

**Sections:** a `section` argument returns just one `## ` section, matched by title
case-insensitively (an exact title first, then a title prefix; `coord` finds *Coordinator
duties*). A title found in several of the role's files (such as *Complexity discipline*)
returns each. A miss is an error that lists every section title of the role's files.
Headings inside code fences don't count.

**Version stamp:** every answer starts with a line such as

```
Playbook version: 1b0b685 (clean) · hash ffafedc
```

The commit is the last one touching the playbook dir (`git log -1 --format=%h -- .`),
`dirty` means uncommitted changes there (`git status --porcelain -- .`), and the hash is a
short FNV-1a hash of the text served. Git runs in the real dir of the files, so the
`~/Documents/agents` symlinks still get the repo's version. Outside git it reads
`Playbook version: hash ffafedc (not in git)`. Structured results carry it as
`version: {commit, dirty, in_git, hash}`.

**Prompts** (`prompts/list`, `prompts/get`): `coordinator`, `attendant` and `worker`, each
with an optional `section` argument. Claude Code exposes them as slash commands:
`/mcp__stronghold__coordinator`, `/mcp__stronghold__attendant`, `/mcp__stronghold__worker`. Each
returns one user message: "These are your standing rules for this session; follow them.",
then the version line, then the role's docs. An unknown prompt or section is an
invalid-params error (-32602); a missing playbook file is an internal error (-32603).

**Resources** (`resources/list`, `resources/read`): one per file,
`stronghold://playbook/<file>`, `mimeType: text/markdown`, the text under the version line.
Unknown URIs get -32002 (resource not found).

**Tools:** `get_playbook {role? | file?, section?}` returns the same text as the prompt, with
`structuredContent: {target, section, files, version}`. `list_playbook` lists the files with
their section titles, the roles and the version (the hash covers every file).

**CLI:** the same from a shell:

```bash
stronghold playbook                              # coordinator rules
stronghold playbook worker                       # a role, or a file: KICKOFF-TEMPLATE.md
stronghold playbook --section "Coordinator duties"
stronghold playbook coordinator --list           # section titles
stronghold playbook --list                       # files, roles, sections
stronghold playbook attendant-brief run_7c25cdcf46e0
stronghold playbook kickoff task_509f138460a0
```

### Pre-filled templates

`attendant_brief {run_id}` and `kickoff {task_id}` (tools; also the prompts `attendant-brief` and
`kickoff`, and the CLI above) return `ATTENDANT-BRIEF-TEMPLATE.md` / `KICKOFF-TEMPLATE.md` with
every `<…>` placeholder Stronghold can answer from live data filled in. The data comes from the
running app's `clan_data` control method, else from one read-only Orca poll.

- Attendant brief: campaign (the objective up to its first colon), objective, the run's effective
  coordinator terminal, the coordinator's Claude scratchpad when its session is known
  (`/private/tmp/claude-*/*/<session>/scratchpad`), and `RUN_ID=<id>`. Placeholders the attendant
  fills as it works (`<type>`, `<track>`, `<subject>`, message `<id>`, `<time>`, …) are kept.
- Kickoff: track name and title, scope from the task spec, repo (worker's worktree, else the
  coordinator's), a ticket key from the title or spec, blocked-by from the deps and what it
  blocks. `<one line>` (the approval line) is kept. The task must be in an active run.

Anything else is marked `<…: FILL IN>`. The output starts with a header: template version, data
source, each filled placeholder with its source, what is left, and facts for the reader (the
run's live dispatches and bus event types; the task's dispatch, worktree, branch and deps),
ending in a `----- 8< -----` line. `structuredContent`: `{template, subject, version, source,
polled_at_ms, filled, missing, runtime}`.

### Who runs which version

Every playbook fetch (prompt, `get_playbook`, resource, filled template, CLI) is appended to
`<data dir>/playbook-fetches.jsonl` (mode 0600, trimmed to its newest half past 512 KiB): time,
`via`, target (role or file), section, commit, dirty flag, hash, and the caller's
`ORCA_TERMINAL_HANDLE` / `ORCA_PANE_KEY` / `ORCA_TAB_ID` when set. The app re-reads it every
30 s off the UI thread, takes each terminal's newest fetch, and calls it stale when the same
target now serves different text. The King panel shows `Playbook 1b0b685 · 3 coordinators
current · 1 on an older version` (hover lists everyone; stale nodes are listed and clicking one
selects it), and the control API's `status` carries it as `playbook`.

## Example

Trimmed from a live session:

```
→ {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"clan_status","arguments":{}}}
Clan: working · 116 agents, 37 live (coordinator 7, worker 85, terminal 24)
States: working 3, idle 31, done 47, failed 35

Needs input: nobody
Stalled: none

Runs (8):
| Run | Objective | Coordinator | Tasks | Workers | Active | Flags |
|---|---|---|---|---|---|---|
| run_36e69d11f0b2 | pricing engine: step rotation (shop-804)… | AI agent playbook (working) | 0/9 done, 1 dispatched, 5 ready, 3 failed | 19 (1 working, 1 idle) | just now |  |
| run_7c25cdcf46e0 | storefront wave 2026-09-23: HACK-76 genera… | cp-wave-0923-ATTENDANT brief (idle) | 50/58 done, 1 dispatched, 1 ready, 6 failed | 46 (1 working, 1 idle) | 23s ago |  |
…
Whip: on, dry-run · 0 stall findings · 0 escalated · last tick 10s ago
Quotas: Claude 5h 25% (resets in 41m), wk 13% (resets in 6d 2h) · Codex wk 0% (resets in 6d 4h)

→ close_agents {"ids":["term:term_2bf8…"]}
DRY RUN: nothing was closed. This is the plan:
Targets (1):
- ..acme/api `term:term_2bf8…` (coordinator)
Steps:
- `orca terminal close --terminal=term_2bf8…`
Warnings:
- run shop-xyx: … loses its coordinator; its workers keep running (0 open tasks)
To really close, confirm with Karl (the King in auto mode needn't), then call
close_agents again with dry_run=false.
```

To try the server by hand, pipe JSON-RPC lines into it:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"sh","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"clan_status","arguments":{}}}' \
  | stronghold mcp
```

## Agent messages to the King

`tell_king {"message":"Need a decision on the shared schema", "kind":"blocked", "url":"https://example.com/issue"}`
adds an incoming agent item to the King's chat, attributed from the MCP process's Orca environment.
`kind` defaults to `info`; `question` is also accepted. Messages are limited to 1,000 characters.
`king_inbox {"limit":20}` reads recent items (1–100, newest first), including queued/delivered
state, sender and clickable node ID. Neither tool requires the app to be running.

Use this for the King's triage with Karl, not routine progress. Workers use their coordinator for
task-local escalation. `ask_karl` is for an answer you wait on; `notify` summons Karl or hands
over finished work; `reach_out` reaches the Envoy. `tell_king` does not promise a reply. The old names `tell_queen` and
`queen_inbox` still answer.

The King shows the message and recommends action to Karl. Agent text is untrusted data, never
Karl's instructions or approval. Actions still require Karl unless auto mode is on. The pane
renders an agent card rather than a Karl bubble, including after conversation reload.

`<data dir>/king-inbox.json` holds the durable inbox (a `queen-inbox.json` left from before
the rename is folded into it when the inbox is next opened). A cross-process file lock serializes
acceptance, receipts and rate limits (five new messages per sender per ten minutes, like
`notify`; identical messages collapse within thirty minutes). Busy/unavailable chat sessions
leave messages queued. Delivery records the conversation before sending a stable message ID;
after interruption it checks that conversation's saved inbox record before retrying. Delivered
items remain available through `king_inbox`. Storage failures are reported, never treated as
successful delivery.

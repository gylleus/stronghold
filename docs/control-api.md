# Myrstack control API

A local API for reading the fleet status and controlling the dashboard while Myrstack is
running. It is meant for the MCP server (which proxies to it) and for debugging with
`myrstack ctl`.

## Transport

- **Unix domain socket** at `<data dir>/control.sock`. The data dir is
  `~/Library/Application Support/Myrstack`, or `$MYRSTACK_DATA_DIR` when set.
  `$MYRSTACK_CONTROL_SOCKET` overrides the full socket path. A relative path resolves against
  the working directory, which helps when the data dir is too deep for the ~104-byte limit on
  socket paths.
- The socket is created with mode `0600`: it is bound under a temporary name, restricted, then
  renamed into place. There is no TCP listener.
- On start, a leftover socket that nobody listens on is removed. If another Myrstack is serving
  the socket, the new instance runs without the API and logs a warning.
- **Framing: newline-delimited JSON.** Send one request object per line and read one response
  line for each request, in order. A connection can carry any number of requests. Blank lines
  are ignored. Lines longer than 1 MiB are rejected and the connection is closed. A connection
  that sends nothing for 5 minutes is closed. At most 32 connections are served at once.
- Each connection has its own thread. Reads never touch the UI thread. Actions are queued to
  the UI thread, which runs them on its next frame (the API wakes it), and the connection
  waits for the result.

## Protocol (version 1)

Request:

```json
{"v": 1, "method": "agents", "params": {"state": "needs_input"}, "id": 42}
```

- `v` (required): the protocol version. Only `1` is accepted.
- `method` (required): one of the methods below.
- `params` (optional): an object. It can be omitted or `null` for methods without params.
- `id` (optional): any JSON value. It is echoed in the response, including on errors when it
  can be recovered.

Response:

```json
{"ok": true, "result": {...}, "id": 42}
{"ok": false, "error": {"code": "not_found", "message": "no agent \"x\""}, "id": 42}
```

Error codes:

| code | meaning |
|---|---|
| `parse_error` | not JSON, not UTF-8, or too long |
| `invalid_request` | not an object, or `v`/`method` missing or mistyped |
| `unsupported_version` | `v` is not 1 |
| `unknown_method` | no such method |
| `invalid_params` | a param is missing, mistyped or has an unknown value |
| `not_found` | no agent (or no deliverable run) matches |
| `refused` | the app will not do it (e.g. closing a subagent or the Queen) |
| `not_supported` | a known method this build cannot do |
| `unavailable` | no fleet data yet (before the first poll), or the app is shutting down |
| `timeout` | the app did not reply in time (5 s for dashboard changes, 60 s for sends, 120 s for manual whips, 180 s for closes) |
| `failed` | the action ran and failed (e.g. Orca refused the input) |

### Agent ids

Methods that take an agent `id` accept any of these:

- a node id: `term:<handle>`, `dispatch:<run>:<dispatch>` or `sub:<session>:<agent>`;
- a terminal handle: `term_…`;
- a node label, compared case-insensitively.

### Names

- States: `working`, `idle`, `needs_input`, `stalled`, `done`, `failed`.
- Kinds: `coordinator`, `worker`, `terminal`, `subagent`.

Input matching is loose, so `NeedsInput` and `needs-input` also work.

The **Queen** is the graph's hub node that stands for the whole fleet. It is not an agent, so
`agents` never lists it and `status` reports its state as `fleet.state`. `select` and `focus`
accept `queen`.

## Read methods

### `ping`

Returns the protocol version, whether the app has data, and the method list.

```json
{"v":1,"method":"ping"}
→ {"ok":true,"result":{"protocol":1,"version":"0.1.0","ready":true,
   "methods":{"read":["ping","status",…],"action":["send",…]}}}
```

### `status`

Returns a fleet summary, a short whip summary and the dashboard state. It works before the
first poll, with `ready: false`.

```json
{"v":1,"method":"status"}
→ {"ok":true,"result":{
    "ready":true,"published_at_ms":1790344470150,"polled_at_ms":1790344466430,"errors":[],
    "fleet":{
      "state":"working",
      "agents":116,"live":38,
      "by_kind":{"coordinator":7,"terminal":24,"worker":85},
      "by_state":{"done":47,"failed":35,"idle":33,"working":1},
      "needs_input":["term:term_…"],"stalled":[],
      "runs":[{"id":"run_7c25cdcf46e0","objective":"commerce-platform wave …","repo":"hackathon",
               "coordinator":"term:term_df9f…","coordinator_state":"idle",
               "tasks":{"total":58,"pending":0,"ready":1,"dispatched":1,"completed":50,
                        "failed":6,"blocked":0,"other":0},
               "workers":46,"by_state":{"done":38,"failed":7,"idle":2},
               "needs_input":false,"stalled":false,"last_activity_ms":1790344260137}]},
    "whip":{"enabled":true,"dry_run":true,"last_tick_ms":…,"findings":1,"escalated":[]},
    "alerts":0,
    "notifications":{"total":0,"unread":0,"latest_unread":[]},
    "dashboard":{"view":"graph","selected":"queen","selection":["queen"],
                 "filter":{"repos":[],"states":[],"kinds":[],"text":"","show_loose":true,
                           "show_released":true,"hidden":0},
                 "whip_settings_open":false},
    "playbook":{"current":"1b0b685","current_hash":"4acf3d6",
                "nodes":[{"node":"term:term_df9f…","label":"…","kind":"coordinator",
                          "terminal_handle":"term_df9f…","at_ms":…,"via":"prompt:coordinator",
                          "target":"coordinator","section":null,"version":"1809c8a",
                          "hash":"3f9a1c2","stale":true}]}}}
```

`playbook` is the current playbook version and each node's newest playbook fetch (see
[mcp.md](mcp.md#who-runs-which-version)); `stale` means the same target serves different text
now. `current` is `null` until the app has read the fetch log.

`dashboard.selected` is the primary selected node (whose panel is shown); `dashboard.selection`
lists every node selected in the graph, several after a box select or Shift/Cmd-click.

### `agents`

Lists agents. Every param is an optional filter, and filters combine with AND:

| param | type | matches |
|---|---|---|
| `state` | string or list | node state |
| `kind` | string or list | node kind |
| `run` | string | run id |
| `repo` | string | repo name (as in the sidebar) |
| `needs_input` | bool | waiting on Karl |
| `live` | bool | terminal is in `terminal list` |
| `text` | string | substring of the label, branch or run objective (case-insensitive) |

```json
{"v":1,"method":"agents","params":{"kind":"coordinator","live":true}}
→ {"ok":true,"result":{"count":7,"agents":[{
    "id":"term:term_2574…","kind":"coordinator","label":"AI agent playbook","state":"idle",
    "live":true,"run_id":"run_36e69d11f0b2","run_objective":"recs-core composition: …",
    "task_id":null,"agent":"claude","branch":"main","worktree":"/Users/…/realtime",
    "repo":"depict","terminal_handle":"term_2574…","parent":null,
    "last_output_ms":1790343526107,"idle_secs":948,
    "needs_input":null,"stalled":false,"escalated":false,
    "tokens":{"total":1072927489,"input":217861,"output":2304590,
              "cache_read":1024115725,"cache_creation":46289313},
    "released":false,"worker_state":null,"dispatch_status":null,"preview":"…"}]}}
```

`needs_input` is set while the agent waits on Karl, for example
`{"kind":"choice","options":["1. Yes","2. No"]}`. The possible kinds are `permission`,
`choice`, `confirm`, `freeform_question`, and `unknown` (only the preview hinted at a prompt).
`idle_secs` is computed when the response is built. `parent` is a worker's coordinator, or a
subagent's terminal or parent subagent.

### `agent`

Returns one agent with its children and a fresh read of its screen. The screen is read with
`orca terminal read` on the connection's thread, and only for live terminals (not for
subagents).

| param | type | default |
|---|---|---|
| `id` | string (required) | |
| `lines` | int | 40 (max 500): screen rows to return, after trailing blank rows are dropped |
| `screen` | bool | true: set false to skip the screen read |

```json
{"v":1,"method":"agent","params":{"id":"term:term_2574…","lines":3}}
→ {"ok":true,"result":{
    "agent":{…same fields as in agents…},
    "children":["term:term_1744…","dispatch:run_36e6…:ctx_257c…"],
    "screen":{"handle":"term_2574…","read_at_ms":1790344482094,"status":"running",
              "lines":["✻ Crunched for 2m 39s · done 3:30 PM","────…","❯ "]}}}
```

If the read fails, `screen` is `{"handle":…,"error":"…"}`.

### `whip`

Returns the whip settings, its state, and the newest log entries (the newest entry is last).
The optional `limit` param sets how many log entries to return (default 20; the view keeps
the last 200). Each entry has `manual: true` when it was a manual whip (see the `whip` action
below). With `ids`, `id` or `stalled` in the params, `whip` is the action instead.

```json
{"v":1,"method":"whip","params":{"limit":1}}
→ {"ok":true,"result":{"enabled":true,"dry_run":true,"notify":true,"threshold_ms":…,
   "cooldown_ms":600000,"max_nudges":2,"tick_ms":60000,"abandon_after_ms":…,
   "last_tick_ms":…,"findings":1,"actions_logged":3,"escalated":[],
   "log":[{"seq":3,"at_ms":…,"action":"nudge #1 via bus to dispatch:ctx_…","target":"worker …",
           "reason":"…","dry_run":true,"outcome":"dry-run","manual":false,"handle":"term_…",
           "run_id":"run_…","task_id":"task_…"}]}}
```

### `quotas`

Returns the plan quotas from `orca account list`.

```json
{"v":1,"method":"quotas"}
→ {"ok":true,"result":{"fetched_at_ms":…,"error":null,"providers":[{"key":"claude",
   "name":"Claude","status":"ok","error":null,"windows":[{"key":"session","label":"5h",
   "used_percent":42.0,"resets_at_ms":…,"reset_description":"…","severity":"normal"}]}]}}
```

### `alerts`

Returns the notification inbox, newest first: what agents sent with `notify` and what
Myrstack raised itself. `node` is the node each alert is about, if that node is still in the
fleet; `count` is how many identical notifications were collapsed into it. `status` carries
the unread count and the newest unread ones under `notifications`.

```json
{"v":1,"method":"alerts"}
→ {"ok":true,"result":{"alerts":[{"id":7,"at_ms":…,"severity":"warn","source":"agent",
   "title":"Blocked: need prod DB access","message":"…","url":"https://…",
   "agent":"realtime-255","node":"term:term_…","count":1,"read":false}]}}
```

### `fleet_data`

The last poll's raw Orca data: `{polled_at_ms, terminals, runs, workers, tasks: {run_id: [...]},
inbox}` in Orca's own field names, without terminal previews or message bodies. Filled playbook
templates (`myrstack playbook attendant-brief`, MCP `attendant_brief`/`kickoff`) read it.
`unavailable` before the first poll.

### `review_queue`

The PRs waiting for Karl's review, newest mark first (see [Review queue](#review-queue)).
Each entry has the PR (`url`, `repo` as `owner/repo`, `number`, `title`), the agent that
marked it (`agent: {id, label, live}` when its terminal is a node, else `agent_handle`), the
mark (`source`: `tool` \| `cli` \| `bus`, `summary`, `marked_ms`) and GitHub's state as last
read: `state` (`open` \| `draft` \| `merged` \| `closed`, `null` before the first read), `ci`,
`head_sha`, `ankra_verdict` with `ankra_on_head` (`false`: Ankra reviewed an older commit),
`marker` (`ready` \| `not_ready`, the description's first line) with `marker_on_head`,
`review_decision`, `error` (this PR could not be read) and `updated_since_review`.
`github.available` is `false` when `gh` is missing or logged out.

```json
{"v":1,"method":"review_queue"}
→ {"ok":true,"result":{"count":1,"github":{"available":true,"error":null},"entries":[
    {"url":"https://github.com/depict-org/realtime/pull/255","repo":"depict-org/realtime",
     "number":255,"title":"recs: gate legacy config retirement","summary":null,
     "agent":{"id":"term:term_…","label":"…","live":true},"agent_handle":null,
     "source":"cli","marked_ms":…,"state":"open","ci":{"state":"passing","total":9},
     "head_sha":"92b1c94…","ankra_verdict":"comments","ankra_on_head":false,
     "marker":"ready","marker_on_head":false,"review_decision":null,"error":null,
     "updated_since_review":false}]}}
```

## Action methods

These methods change things. The terminal ones type into agents' terminals and the whip
settings are saved to disk, so only use them deliberately.

### `send`

Types text into an agent's terminal. This needs a live terminal, so it does not work on
subagents or on workers whose terminal is gone.

| param | type | default |
|---|---|---|
| `id` | string (required) | |
| `text` | string (required) | may be empty when `enter` is true |
| `enter` | bool | true: press Enter after the text |

```json
{"v":1,"method":"send","params":{"id":"term_…","text":"continue"}}
→ {"ok":true,"result":{"agent":"term:term_…","handle":"term_…","sent":"send"}}
```

The reply comes after Orca accepted the input (up to about 45 s). If Orca refuses the input,
the reply is a `failed` error.

### `enter`, `esc`, `interrupt`

Sends one key to the agent's terminal: Enter, Escape or Ctrl-C. The only param is `id`.

```json
{"v":1,"method":"esc","params":{"id":"term:term_…"}}
→ {"ok":true,"result":{"agent":"term:term_…","handle":"term_…","sent":"esc"}}
```

### `broadcast`

Sends a `Myrstack: `-prefixed bus message to the live coordinators of some runs, as the
Broadcast window does. Give either `runs` (a list of run ids) or `"all": true`.

```json
{"v":1,"method":"broadcast","params":{"text":"wrap up by 18:00","runs":["run_7c25cdcf46e0"]}}
→ {"ok":true,"result":{"sent":1,"failed":0,
   "deliveries":[{"coordinator":"term_…","runs":["run_7c25cdcf46e0"],"ok":true,"message_id":"msg_…"}],
   "skipped":[]}}
```

If no selected run has a live coordinator, the reply is a `not_found` error that names the
skipped runs.

### `whip_settings`

Changes the whip's `enabled`, `dry_run` or `notify` setting (all optional bools). The change
is saved to the settings file (`$MYRSTACK_SETTINGS` or the default path) and applied, the same
way the settings window saves.

```json
{"v":1,"method":"whip_settings","params":{"enabled":true,"dry_run":true}}
→ {"ok":true,"result":{"changed":false,"settings":{"enabled":true,"dry_run":true,…}}}
```

When a setting changes, the result has `"changed":true` and `"message":"Saved."`.

### `select`

Selects a node without moving the graph. Pass `"id": null` to clear the selection.

```json
{"v":1,"method":"select","params":{"id":"term:term_…"}}
→ {"ok":true,"result":{"selected":"term:term_…"}}
```

Pass `"ids"` (a list, after `id` if both are given) to select several nodes at once, as a box
select in the graph does; the first is the primary (`selected`). Nodes hidden by the filter
can't be part of a multi-selection: they are listed in `hidden_by_filter` and left out of
`selection`. An empty `ids` clears the selection.

```json
{"v":1,"method":"select","params":{"ids":["term:term_a…","term:term_b…"]}}
→ {"ok":true,"result":{"selected":"term:term_a…","selection":["term:term_a…","term:term_b…"],
                       "hidden_by_filter":[]}}
```

### `focus`

Switches to the graph, selects the node, and pans and zooms onto it. With `"raise": true` it
also brings the Myrstack window to the front. `hidden_by_filter` means the node is selected
but the current filter hides it, so the graph did not move.

```json
{"v":1,"method":"focus","params":{"id":"term:term_…"}}
→ {"ok":true,"result":{"selected":"term:term_…","hidden_by_filter":false}}
```

### `filter`

Changes the sidebar filter. Fields you leave out stay as they are. `clear: true` resets the
filter to show everything before the other fields apply.

| param | type |
|---|---|
| `clear` | bool |
| `repos` | string list (empty list = all) |
| `states` | string list |
| `kinds` | string list |
| `text` | string |
| `show_loose` | bool |
| `show_released` | bool |

```json
{"v":1,"method":"filter","params":{"states":["needs_input","stalled"]}}
→ {"ok":true,"result":{"filter":{"repos":[],"states":["needs_input","stalled"],"kinds":[],
   "text":"","show_loose":true,"show_released":true,"hidden":112}}}
```

### `view`

Switches the dashboard view to `graph` or `table`.

```json
{"v":1,"method":"view","params":{"view":"table"}}
→ {"ok":true,"result":{"view":"table"}}
```

### `fit`

Switches to the graph and fits every node on screen.

```json
{"v":1,"method":"fit"}
→ {"ok":true,"result":{"view":"graph"}}
```

### `open_whip_settings`

Opens the Whip settings window.

```json
{"v":1,"method":"open_whip_settings"}
→ {"ok":true,"result":{"open":true}}
```

### `close`

Closes (kills) agents' sessions. It uses the same plan as the "Close session" dialog: an
active dispatch is fenced with `orchestration worker-stop`, then the terminal is closed with
`terminal close`. Closes started from the API appear in the app's close log.

| param | type | default |
|---|---|---|
| `id` | string | one agent |
| `ids` | string list | several agents (a bulk plan: refused nodes are skipped with a warning) |
| `whole_tab` | bool | false: close each terminal's whole Orca tab |
| `dry_run` | bool | false: only return the plan |

At least one of `id` or `ids` is required. Without `dry_run`, the close really runs, so call
it with `dry_run: true` first and show the plan's warnings.

```json
{"v":1,"method":"close","params":{"id":"term:term_2574…","dry_run":true}}
→ {"ok":true,"result":{"dry_run":true,"plan":{
    "targets":[{"id":"term:term_2574…","label":"AI agent playbook","kind":"coordinator"}],
    "steps":[{"command":"orca terminal close --terminal=term_2574…","label":"Close terminal AI agent playbook"}],
    "warnings":["run recs-core composition: … loses its coordinator; its workers keep running (6 open tasks)"],
    "refused":null,"can_run":true}}}
```

When the close runs, the reply comes after every step has finished:
`{"dry_run":false,"plan":{…},"ok":true,"steps":[{"command":"orca terminal close …","ok":true,"note":null}]}`.
A plan that is refused (for a subagent, the Queen, or a node with nothing to close) returns a
`refused` error.

### `whip` (action)

Nudges agents now, or runs a whip pass now. `whip` without `id`, `ids` or `stalled` is the
read method above.

| param | type | meaning |
|---|---|---|
| `id` / `ids` | string / string list | agents to nudge now (a manual whip) |
| `stalled` | bool | `true`: run an immediate whip tick instead |

Give either agents or `stalled: true`, not both.

**Manual whip** (`ids`): each agent gets the nudge the automatic whip sends when it stalls,
with the same texts and sender (`myrstack-whip`):

- a worker with a live dispatched task: a bus message to `dispatch:<id>` about the task;
- a coordinator: a bus message to its terminal handle with `--run`;
- a loose terminal, or a worker without a live dispatch: a status check typed into its
  terminal (or, when it waits on input, sent on the bus to its handle, since typing could
  answer its prompt).

Threshold and cooldown do not apply, and the nudges are **sent live even when the automatic
whip is in dry-run** (an explicit request). They are logged with `manual: true` and recorded
by the whip, so their echo is not taken for progress and a stalled target climbs the
escalation ladder (and starts a new cooldown). The Queen, subagents, released workers and
dead terminals are skipped with a reason. An agent waiting on Karl's input is nudged, with a
`warning`. The API does not ask for confirmation; the app's "Whip" button does for
NeedsInput targets and for 5 or more targets.

```json
{"v":1,"method":"whip","params":{"ids":["term:term_12…","queen"]}}
→ {"ok":true,"result":{"mode":"manual","sent":1,"failed":0,"automatic_dry_run":true,
   "nudges":[{"id":"term:term_12…","label":"Fix login","handle":"term_12…",
              "via":{"bus":"dispatch:ctx_…","run":"run_…"},"target":"worker term_12… task t1",
              "text":"Myrstack: no progress on task …","ok":true,"outcome":"sent",
              "warning":null,"seq":14}],
   "skipped":[{"id":"queen","label":"Queen","reason":"the Queen is Myrstack itself"}]}}
```

`via` is `{"bus": <to>, "run": <run id>}` or `{"terminal": <handle>}`. An unknown agent is a
`not_found` error; when every agent is skipped the result has `sent: 0` and the skips.

**Whip all stalled now** (`stalled: true`): runs a full whip tick on the whip thread now
instead of at the next period, under the normal rules (threshold, cooldown, escalation) and
the automatic whip's dry-run setting. The next periodic tick comes one period later.

```json
{"v":1,"method":"whip","params":{"stalled":true}}
→ {"ok":true,"result":{"mode":"stalled","ran":true,"dry_run":true,"actions":[
    {"seq":15,"action":"nudge #1 via bus to dispatch:ctx_…","target":"worker …",
     "handle":"term_…","outcome":"dry-run","dry_run":true}]}}
```

`ran` is `false` before the first poll.

### `pr_ready`

Records that a PR is ready for review. Agents call it through the `mark_pr_ready` MCP tool or
`myrstack pr-ready`, which fill in `origin` from the caller's Orca environment
(`ORCA_TERMINAL_HANDLE`, `ORCA_PANE_KEY`, `ORCA_TAB_ID`). It only records in Myrstack; nothing
goes to GitHub or the bus.

| param | type | meaning |
|---|---|---|
| `url` | string | the PR URL, `https://github.com/<owner>/<repo>/pull/<n>` (required) |
| `title` | string | optional; GitHub's title replaces it once read |
| `summary` | string | optional one line |
| `via` | `tool` \| `cli` | where the mark came from (default `tool`) |
| `origin` | `{terminal_handle, pane_key, tab_id}` | the marking agent's Orca identity |

```json
{"v":1,"method":"pr_ready","params":{"url":"https://github.com/o/r/pull/7","via":"cli",
  "origin":{"terminal_handle":"term_…"}}}
→ {"ok":true,"result":{"url":"https://github.com/o/r/pull/7","outcome":"new",
   "agent":{"id":"term:term_…","label":"…","live":true},"origin":{…},"count":3}}
```

`outcome` is `new`, `updated` (a newer mark of a known PR; it shows again if Karl had
dismissed it) or `unchanged`.

### `review_dismiss`, `review_done`

`{"url": …}`. Hide a PR from the queue: `review_dismiss` until it is marked ready again,
`review_done` (reviewed) until it is marked again or its head moves. `not_found` when the PR
is not in the queue.

### `notify`

`{"title", "message"?, "severity"?: "info"|"warn"|"urgent", "url"?: http(s), "agent"?,
"origin"?: {"terminal_handle", "pane_key", "tab_id"}}`. An agent's notification to Karl (the
`notify` MCP tool sends it with the caller's Orca identity). It goes through the app's
dispatcher like every notification; see [Notifications](#notifications).

```json
{"v":1,"method":"notify","params":{"title":"Blocked","severity":"warn","origin":{"terminal_handle":"term_…"}}}
→ {"ok":true,"result":{"outcome":"delivered","id":7,"severity":"warn","quiet":false,
   "agent":{"id":"term:term_…","label":"…"},
   "delivery":{"native":true,"in_app":true,"sound":true}}}
```

`outcome` is `delivered` (possibly silently: below Karl's thresholds or quiet mode, see
`delivery` and `quiet`), `collapsed` (identical to one the agent sent in the last 30 min;
`count` says how many), `folded` (a handover about a PR that Myrstack announced as ready for
review in the last 10 min: its text replaces that entry's, so Karl gets one notification; a
review announcement after an agent's handover about the same PR is dropped likewise), or
`rate_limited` (at most 5 per agent per 10 min;
`retry_after_ms`). An `agent` that matches no node is reported as `unknown_agent` and the
notification is filed under the caller's terminal.

### `ask`

`{"question", "context"?, "severity"?: "warn"|"urgent", "url"?: http(s), "origin"?}`. An
agent's question to Karl (the `ask_karl` MCP tool). Notifies him like `notify` and marks the
caller's terminal waiting (needs input) until he answers. The reply is `notify`'s plus
`"waiting": true|false` (false when the origin matches no terminal).

### `finish`

`{"summary", "outcome"?: "done"|"handed_over"|"abandoned", "pr_url"?, "force"?, "forced"?:
[…], "dry_run"?, "origin"?}`. An agent ending its own session (the `finish_session` MCP tool).
With `dry_run` it returns the caller's node (`agent`: id, label, kind, worktree, branch) with
`refused` (a worker) and `problems` (open tasks, PRs in the review queue). Otherwise a refusal,
or problems without `force`, fail with `refused`; past them the app records the outcome
(`finished`, with `forced` = the caller's problems plus the app's), notifies Karl and closes
the terminal after `closing_in_ms` with the `plan` returned.

```json
{"v":1,"method":"finish","params":{"summary":"Merged into main","origin":{"terminal_handle":"term_…"}}}
→ {"ok":true,"result":{"dry_run":false,"agent":{"id":"term:term_…",…},"finished":{…},
   "closing_in_ms":2000,"plan":{…},"notification":"delivered"}}
```

## Notifications

Every notification goes through one dispatcher in the app: agents' `notify`, nodes entering
NeedsInput, whip escalations and long waits, PRs marked ready, and the Settings window's tests.
It drops Myrstack's own repeats about one target within 10 minutes, collapses an agent's
identical notifications, rate limits each agent, and records the rest in the inbox
(`<data dir>/notifications.json`, 0600, newest 200). Delivery follows the Notifications
settings (`notifications` in `settings.json`): macOS and in-app pop-ups from a minimum
severity, a sound per severity (a macOS alert sound, a custom file, or none) from a minimum
severity at a set volume, and quiet mode, in which only urgent pops up or sounds. Native
pop-ups are capped at 5 a minute, and bursts are collapsed into one summary. Clicking a
notification, pop-up or inbox entry selects its node and opens its link; selecting a node
marks its notifications read.

## Review queue

PRs reach the queue from `pr_ready` and from `merge_ready` bus messages in the inbox the app
polls (every PR URL in the subject, body or payload, attributed to the sender). A mark made
while the app is down waits in `<data dir>/pr-ready.jsonl` (0600) and is taken on start. The
app reads each queued PR with `gh pr view --json …` (read-only) every 3 minutes and at once
when it joins. Bus entries show only once GitHub has been read (so old `merge_ready` messages
about merged PRs never show); merged or closed PRs leave after 5 minutes. The queue is saved
in `<data dir>/review-queue.json` (0600).

## `myrstack ctl`

```
myrstack ctl <method> [json params | id]
```

This prints the pretty-printed response and exits 1 when `ok` is false. A single argument that
does not start with `{` is sent as `{"id": <arg>}`:

```
myrstack ctl status
myrstack ctl agents '{"state":"needs_input"}'
myrstack ctl agent term:term_2574…
myrstack ctl focus term:term_2574…
myrstack ctl close '{"id":"term:term_2574…","dry_run":true}'
myrstack ctl whip '{"stalled":true}'
```

The command honours `MYRSTACK_DATA_DIR` and `MYRSTACK_CONTROL_SOCKET`, just as the app does.
To script the API without the binary, use a raw socket:

```
printf '{"v":1,"method":"status"}\n' | nc -U ~/Library/Application\ Support/Myrstack/control.sock
```

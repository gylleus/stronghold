# Playbook proposal: the review queue

Proposed insertions to Karl's clan docs so that ready PRs reach Stronghold's review queue
(the "Ready for review" section of the King panel, `docs/control-api.md` *Review queue*).
Nothing here has been applied. Karl approves and applies them.

- **`_AGENT_PROTOCOL.md`**: 1 block (insert A).
- **`AGENT-FLEET-PLAYBOOK.md`**: 3 blocks (inserts B–D).

Stronghold already reads every `merge_ready` on the bus and queues the PR URLs it names, so a
worker that follows the protocol today shows up without any change. The explicit call adds
what the bus can't: PRs from agents outside a Run (loose terminals, Codex), the PR's title
and a one-line summary, and the marking terminal's identity when the sender handle is stale.

A worker's mark is a hand-off, not the ready marker. The queue shows each PR's marker
(`✅`/`⏸`/none) and whether Ankra and the marker are on the head, so Karl sees at a glance
which ones the coordinator has gated. Insert A keeps the existing "never mark the PR ready
yourself" rule intact by saying the tool call is not that.

---

## A. `~/Documents/agents/_AGENT_PROTOCOL.md`, `## Reporting`

Replace the first two lines of the `merge_ready` bullet:

```markdown
- Gates green and standing by for merge → `--type merge_ready` with the PR URL in the body.
  That is a report to the coordinator, not a verdict. **Never mark the PR ready yourself** —
```

with:

```markdown
- Gates green and standing by for merge → `--type merge_ready` with the PR URL in the body,
  then record the hand-off in Stronghold: call the `mark_pr_ready` MCP tool with
  `pr_url` (and a `summary` line), or without MCP run `stronghold pr-ready <url> --summary
  "<one line>"`. It only records locally, attributed to your terminal; it is not the ready
  marker. That is a report to the coordinator, not a verdict. **Never mark the PR ready
  yourself** —
```

(The rest of the bullet is unchanged.)

## B. `AGENT-FLEET-PLAYBOOK.md`, `## The per-track loop`, step 6

Replace:

```markdown
6. **Hand off.** `merge_ready`, then stop. `merge_ready` keeps the dispatch open (the gate can
   send work back); `worker_done` only when the track is finished.
```

with:

```markdown
6. **Hand off.** `merge_ready`, then `mark_pr_ready` (or `stronghold pr-ready <url>`), then
   stop. `merge_ready` keeps the dispatch open (the gate can send work back); `worker_done`
   only when the track is finished.
```

## C. `AGENT-FLEET-PLAYBOOK.md`, `## The ready marker`

Add after the bullet `- Read the current body first and edit with gh pr edit …`:

```markdown
- Right after setting `✅`, call `mark_pr_ready` with the PR URL and the marker's one line as
  `summary` (or `stronghold pr-ready <url> --summary "<one line>"`). The PR then heads Karl's
  review queue in Stronghold with a notification. After `⏸`, don't: the queue already shows it
  from the worker's hand-off, with the ⏸.
```

## D. `AGENT-FLEET-PLAYBOOK.md`, `## Topology`

Replace the Stronghold line:

```
 └─ Stronghold: Karl's desktop view of the whole clan (graph, terminals, alerts). Its whip
    nudges stalled tasks (`Stronghold:` messages from `stronghold-whip`).
```

with:

```
 └─ Stronghold: Karl's desktop view of the whole clan (graph, terminals, alerts, the review
    queue of ready PRs). Its whip nudges stalled tasks (`Stronghold:` messages from
    `stronghold-whip`).
```

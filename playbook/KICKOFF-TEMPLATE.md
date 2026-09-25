# KICKOFF — <track name>

Copy this into the dispatch spec (`orca orchestration task-create --spec "$(cat ...)"`) or
drop it in the worktree as `KICKOFF.md` and point the spec at it.

---

**Read `/Users/gylleus/Documents/agents/_AGENT_PROTOCOL.md` first.** It is the shared fleet
protocol: lifecycle, reporting, hard rules, complexity and measurement discipline. Everything
below is only what is specific to this track.

## Track

- **Name:** <track>
- **Repo:** <repo> (`orca repo list` → `id:<repoId>`)
- **Ticket:** <VROOM-###> — <one-line title>
- **Port range:** <track-number>00–<track-number>99

## Scope

<What this track owns, in two or three sentences. The decision to be made, not the keystrokes.>

**Explicitly out of scope:** <the adjacent thing another track owns, or the follow-up that is
deliberately not this PR.>

## Blockers and dependencies

- Blocked by: <task/PR, or "none">
- Blocks: <task, or "none">
- Shares state with: <migrations, tables, stacked PRs — name the other track and the
  coordination rule, or "nothing">

## Load-bearing facts to verify (do not trust this list)

<The facts the plan will rest on. The reviewer's first job is to check these against the
code, not to accept them. If one is wrong, say so before implementing anything.>

1. <fact>
2. <fact>

## Definition of done

- Plan posted and `APPROVED — <one line>` on the ticket
- Repo gate green (<`just check`, or this repo's actual gate>)
- PR open, every Ankra thread answered with a fix or reasoned pushback, verdict SHA = head
- PR description leads with `## Why` (each design decision + what forced it), then
  what-changed, then evidence
- `/simplify` deletion pass done over the diff
- Verification evidence in the PR body: <what a working end-to-end path looks like here —
  the real request, the real data, the real page. Name it concretely.>
- Old path / scaffolding / flags deleted in this track
- `orca orchestration send --type merge_ready ...` sent, then idle

## Notes

<Anything already known: prior attempts, related PRs, telemetry links, the reason this is
scoped the way it is.>

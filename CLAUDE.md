# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

## Finishing work in the Stronghold repo

The maintainer's rule for every agent working in this repo. It is the repository's explicit opt-in, so
it overrides the "Conservative" default below for commits and the local merge (not for pushes):

1. Work on your own branch in your worktree; commit there.
2. Run the gates, all green: `cargo fmt --check`, `cargo clippy --all-targets` (no new
   warnings; the old `src/ui/theme.rs` one is tracked as a bead), `cargo test`.
3. Merge your branch into `main` directly, in the main checkout at `~/repo/saker/myrstack`:
   `git -C ~/repo/saker/myrstack merge --no-ff <your-branch>`. Leave unrelated uncommitted
   files there (such as `.beads/issues.jsonl`) alone; don't stash, reset or commit them. If the
   merge conflicts, merge `main` into your branch in your worktree, fix, rerun the gates, then
   retry.
4. Report a short summary (what changed; the user gets it with `just install` and reopening
   Stronghold), then exit your session.

There is no GitHub remote: no push, no PR, no `mark_pr_ready`.

## Build & Test

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
just install   # release build into ~/Applications/Stronghold.app; quit and reopen to run it
```

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:1105d646 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/core-concepts/sync-concepts.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->


## Architecture Overview

_Add a brief overview of your project architecture_

## Conventions & Patterns

_Add your project-specific conventions here_

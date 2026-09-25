# Spike: usage and cost per agent (myr-hrw.5)

Question: can Myrstack show token usage or cost per terminal, per session or per account?

## Sources

| Source | What it has | Granularity | Cost to read |
|---|---|---|---|
| `orca account list --json` → `result.rateLimits` | Plan quota **percentages**: Claude `session` (5h window), `weekly`, `fableWeekly`; Codex `weekly` and reset credits. Each has `usedPercent`, `windowMinutes`, `resetsAt`. Other providers (gemini, kimi, grok, …) report `status: "unavailable"` here. | Per provider account. Not per terminal. | One ~0.1 s CLI call. Poll it rarely (minutes). |
| `orca diagnostics memory` | Memory snapshot only. | n/a | n/a |
| Other `orca` commands (`--help`, `agent-context`) | No usage, token or cost command. | n/a | n/a |
| Orca app (`app.asar`) | Has its own Claude/Codex usage scanner (`ClaudeUsageStore`, schema v6), which caches to `~/Library/Application Support/orca/orca-claude-usage.json`. On this machine the scan is off and the file does not exist. The CLI does not expose it. Orca's automation runs record `usage.{inputTokens,outputTokens,cacheReadTokens,cacheWriteTokens,totalTokens,estimatedCostUsd}` per run. | Per session/day/project (app UI only). | We can't use it: it's internal and turned off. |
| Claude transcripts `~/.claude/projects/<dir>/<session>.jsonl` | `message.usage` on every `type: "assistant"` record: `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens` (split into `cache_creation.ephemeral_{5m,1h}_input_tokens`), `output_tokens_details.thinking_tokens`, `service_tier`, `speed`, plus `message.model` and `message.id`. | Per API message, so per session and per subagent. | Append-only. The first read is about 85 ms in release (about 0.4 s in debug) for a 62 MB transcript; later reads only parse appended lines (under 1 ms). |
| Claude subagents `<session>/subagents/agent-<id>.jsonl` | The same `message.usage` records. | Per subagent. | Same as above. |
| Codex `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | `event_msg` with `payload.type == "token_count"`: `info.total_token_usage` (cumulative `input_tokens`, `cached_input_tokens`, `cache_write_input_tokens`, `output_tokens`, `reasoning_output_tokens`, `total_tokens`), `last_token_usage`, `model_context_window`, and a `rate_limits` snapshot (`used_percent`, `plan_type`). | Per session. The last `token_count` event gives the total. | Read backwards from the end of the file for the last `token_count` line. Not implemented: Myrstack doesn't map Codex terminals to rollout files yet. |

## What was built

`src/claude/usage.rs` has a `UsageTracker` that runs on the Claude worker thread. It fills
`ClaudeData.usage: BTreeMap<session_id, SessionUsage>`. `SessionUsage { total: Usage, subagents: BTreeMap<agent_id, Usage> }`, and
`Usage { tokens: TokenCounts { input, output, cache_read, cache_creation }, by_model }`.
It reuses `tail_file` from `subagents.rs` to read incrementally from an offset.
The UI shows "103M tok" in two places:

- the terminal panel header's `tokens` row, where hovering shows the breakdown and a line per model;
- the node tooltip.

A subagent node shows only its own transcript's usage. A terminal node shows its session's usage, subagents included.

Live totals at the time of writing (all four token kinds summed):

- a 32-subagent session (opus-5-5): 103M total, of which 99M is cache read, 3.7M cache write, 145k output and 2.1k input;
- a sonnet-5 session: 1.7B total, of which 1.6B is cache read, 11M cache write, 745k output and 7.6k input;
- a 5-subagent session (opus-5): 73M total, of which 70M is cache read, 2.2M cache write, 230k output and 646 input.
  An independent Python count of the same files agrees (229,943 output, 70,430,979 cache read).

## Accuracy caveats

- **Duplicate records.** Claude Code writes one assistant record per content block, and each
  record repeats the whole message's usage. About half the lines are duplicates. Summing them
  naively roughly doubles the total, so the tracker deduplicates by `message.id` within a
  session. Duplicates always carry identical usage; this was checked on 8 transcripts.
- **Cache reads dominate.** They are about 95% of the total. The headline "tok" number counts
  every re-read of the cached context on every turn, so it is not "work done". Output tokens
  are the better activity signal. The hover breakdown shows each kind separately. Orca's own
  `totalTokens` sums the same four fields.
- **`<synthetic>` records** (made up locally for errors or interrupts) are skipped.
- **Subagents** are in separate files. They are included in the session total and also kept per
  agent. All subagent transcripts are counted, not only the recent ones the graph shows.
- **Session scope.** Only the sessions currently mapped to terminals are counted. A resumed
  or forked session (`--resume` / `--fork-session`) that copies earlier messages into a new file
  would count them again under the new session id. Cross-session deduplication isn't done.
- **Truncated file.** If a transcript shrinks, it is re-read from the start. The seen-id set
  stops double counting, but tokens from removed lines stay in the total.
- **Dollars aren't computed.** No authoritative price source exists on the machine. Orca's
  bundle has a `MODEL_PRICING` table (per-MTok input/output/cacheRead/cacheWrite/cacheWrite1h,
  with a Sonnet long-context tier). It is an estimate, labelled `api_equivalent`, and it is
  already out of step with current model names: `claude-opus-5-5` falls back to `claude-opus-5`
  prices. Pricing would also need the 5m and 1h cache-write split, which is in
  `usage.cache_creation`, and the service tier. Accounts on a subscription plan don't pay
  per token anyway; for them the quota percentages are the meaningful "cost".
- **Rate limits** are per account, not per terminal. They could go in the top bar as
  "Claude 5h 12% · wk 57%" from one `account list` call every few minutes. That was left out
  of this spike to stay out of `app.rs` and the poller.

## Follow-ups

1. Account quota chip in the top bar from `orca account list --json` `rateLimits`, polled slowly.
2. Codex per-session usage: map Codex terminals to rollout files and read the last `token_count`.
3. Optional dollar estimate if a maintained pricing source is adopted, labelled as an estimate.

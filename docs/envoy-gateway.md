# Envoy gateway

Agents reach people through `reach_out`. The app stores the request, the Envoy routes it,
and the draft follows the user's destination approval policy. Replies arrive at the asking
terminal when its agent is idle at an empty prompt; `reach_out_status` also retains them.
The user remains on `ask_user` / `notify`.

## Tools available to every dwarf

Origin comes from the MCP process, not a tool argument. A nonblank terminal Origin is required.

| Tool | Arguments | Result |
| --- | --- | --- |
| `reach_out` | Required `to`, `ask`, `why`; optional `karl_asked` (quoted user instruction, 1–500 Unicode characters), `thread` (Slack permalink), `request_id`, `urgency` (`normal`, `soon`, `blocking`; default `normal`), `expect_reply` (default `true`) | `request_id`, `status`, `turn_id`, `merged`, `follow_up` |
| `reach_out_status` | Optional `request_id` | `requests`: the caller's requests, turns, drafts, thread binding and recorded deliveries |

Requests can queue while the Envoy is stopped. Without the app, the tool reports an error;
it does not authorize posting directly. Blocking urgency raises an app notification.
New requests are limited to six per hour per terminal. A follow-up identified by an owned
open `request_id` or the same owned thread has a separate 30/hour conversation allowance.
Exact duplicate asks with the same `karl_asked` merge without spending either budget;
omitting the quote on a retry keeps the original claim. Retrying a declined ask on an open request creates a new turn and spends the applicable allowance. A settled request stays closed: start a new `reach_out` without that `request_id`. Follow-ups wait for the earlier
turn and a confirmed root thread; a failed or unknown send is never automatically retried.

Errors include missing Origin, invalid/oversized text, invalid urgency/permalink, the user as
destination (use `ask_user`), foreign/settled request, conflicting thread, ambiguous thread,
rate limit and unavailable storage. Foreign and unknown request ids both report
`Request unavailable`.

## Envoy-only tools

These require the persisted Envoy terminal identity. They are in its launch allowlist;
`reach_out` and `reach_out_status` are not. The user must restart an existing Envoy session to
pick up the new launch tools after installing the app.

| Tool | Arguments | Result |
| --- | --- | --- |
| `envoy_request` | `id` | `request` with all original asks, requester identity, turns and replies |
| `envoy_route` | `id`, `conversation` (Slack C/D/G id), `destination_kind` (`agent`/`person`), `rationale`; optional `turn_id`, `thread_ts`, `draft_text`, `manual_approval` | `request_id`, `turn_id`, `draft` |
| `envoy_decline` | `id`, `reason`; optional `turn_id` | Updated `request`; rejection is queued for safe delivery to the asker |
| `envoy_link_reply` | `request_id`; at least one of `event_id`, `requester`; `reason` required for a new owner proposal | Updated `request` with the retained event linked or a pending `binding_proposal` |

For an agent destination, `draft_text` is forbidden. The app constructs the draft from the
requester's header and exact ask bytes, then appends attribution. Editing/approval cannot
replace the ask. Rewriting is allowed only for a verified human DM: the app checks conversations.info and
users.info and rejects bot/app recipients. Channels and channel threads always require
`destination_kind: "agent"`; a caller's person label cannot override that. For a verified
person, `draft_text` is required and the original ask remains beside the editable draft.
Rewritten asks always require manual approval, regardless of policy. The user is refused by
name (including full name and @karl), id, labeled mention and resolved DM recipient.

`turn_id` is required when multiple turns are queued. Route retries return the existing
draft. A bound conversation cannot be silently rerouted. Decline cannot cancel an already
dispatched send. To reconcile an ambiguous failed send, link a retained event in its known
thread; that records an explicit reconciliation, without sending again or granting reply-watch permission. An adopted request's
`requester` accepts a live clan agent id or terminal; a known owner cannot be reassigned.
For borrowed or unknown-owner threads, it creates a proposal with the root message's first
line, Slack link, recipient and Envoy reason in the Inbox. Only the user's deliberate Accept
click grants reply delivery and caller access; before acceptance and after Reject, replies
stay in the Inbox. Proposals persist across restart. Linking a dwarf's own `reach_out`
request to that same dwarf needs no approval. Missing/invalid reason, unavailable root preview,
closed request or a changed proposal produce errors. A separate configured people list is
not implemented; verified human-DM checks enforce the current rewrite restriction.

Routing errors also include missing/invalid destination kind, supplied agent draft text,
missing person draft text, conflicting route, earlier unfinished turn, absent live binding,
secret scan failure, recipient verification failure and unavailable worker/storage.

## Approval and replies

Settings → Envoy stores `policies`, a list of `{destination, mode}`. Destinations are exact
conversation ids, `dm`, or `*`, in that precedence order. Mode is `"approve"` (the default)
or `{"auto":{"veto_secs":120}}`. Veto windows range from zero to 86,400 seconds.
Task proposals always remain manual. A request turn with `karl_asked` also always waits for
an app Send click, even under Auto. The claim is unverified and scoped to that turn;
follow-ups supply their own quote. The Envoy sets `manual_approval: true` when an ask claims
approval without using `karl_asked`, rather than declining solely for that claim.

Manual clicks persist `approved_at` (Unix milliseconds) and `approved_by` on the draft;
automatic sends do not. Request cards and the outbox show the quote and approval trail.
After approval, agent asks with `karl_asked` gain `(on <user>'s instruction)` in the header;
the ask bytes and person messages remain unchanged. Old saved requests and drafts load
without claiming prior approval.

Automatic draft cards show a countdown and Cancel. Editing pauses their automatic approval.
The app checks secrets at creation and approval; dry-run prevents sending in either mode.
Policies and dry-run ON apply when saved; live mode and other launch settings require the
next Start. Changing approve to auto does not convert existing manual drafts. After an app restart, pending automatic drafts become manual and raise the normal draft-waiting notification; they never silently restart a countdown. Failed
settings persistence blocks dispatch until repaired.

A supplied thread permalink is only a routing hint. Ordinary request watches and reply
relays require a real, non-dry-run send receipt for that request; only replies newer than
that sent message qualify, never earlier thread history. Dry-run and explicit reply links
do not grant this permission. Successful sends persist receipts and canonical bindings.
Legacy saved Sent drafts with a recorded send timestamp and no confirmation field upgrade
to confirmed using that timestamp as the cutoff; an explicit false confirmation stays false.
Authorized threads outside channel feeds get subscriptions and catch-up reads. Replies have a durable
handoff independent of bounded Inbox display history, and do not spend unsolicited relay
limits. A gone/reused asker falls back to its captured live coordinator; otherwise its reply
stays in the Inbox. Delivery checks clan freshness, run/dispatch identity and available
terminal incarnation ids. Dry-run records would-be terminal delivery instead of sending it.
Relayed reply text is capped at 2,000 Unicode characters plus a truncation notice; status keeps the full event text. Terminal delivery is acknowledged after Orca succeeds; a crash between terminal execution
and saving that acknowledgment may replay a reply.

Inbox shows open/answered requests and unresolved orphan threads. Rejected drafts and
fully declined requests leave the Inbox; deciding a draft reads its draft-ready alert.
Dismiss hides an individual message, handled draft or request. Clear handled / stale hides
handled items and messages older than 30 minutes (including stuck queued events), while
keeping pending approvals. Cleanup persists separately from ingestion checkpoints, so polls
and restarts cannot restore dismissed items. Request dismissal keeps reply subscriptions;
a new turn or reply makes the request visible again. Outbox keeps dismissed/settled requests
and draft decisions. `envoy_inbox` lists open events; `envoy_event` can still read retained
thread context. `envoy_drafts` lists open drafts by default; an explicit `status` includes
handled drafts. Adopt open threads reads `slack-sub list`, skips channel feeds
and expired subscriptions, `envoy:` labels and threads already owned through confirmed requests, and creates unknown/sent requests idempotently. Adopted subscriptions are borrowed: watches start subscribed and never add, relabel or
remove their registry entries, even when a dwarf request already references that thread.
Keep retains an orphan; Close only settles its app request. For a thread the Envoy owns,
Close removes its subscription only after a real send and after all active requests finish;
dry-run records that removal intent. Threadless simulations settle locally. The one-week direct-post
detector is deferred to migration after the user picks its start date.

## Delivery gate and stall signals

Events and request turns are typed only when the Envoy node is idle, nothing is queued for
its terminal, and its composer is empty. The composer verdict comes from the rendered screen:
the background prompt sampler (idle agents, every ten seconds, five per round) and the
selected terminal's screen watch both record [`composer_state`] beside the needs-input
verdict in `ClanData.composers`, pruned when the terminal produces output. Orca's terminal
`preview` is the stale startup banner plus the last redrawn fragment, so it often lacks the
composer line; it is read only while no screen sample exists, and errs towards not typing.

Two stall signals sit on the Envoy node and raise a warning (repeated every 30 minutes
while they last): **events stuck**, when pending events have waited ten minutes with
nothing typed (the message names the gate's reason, as the log's `envoy: N event(s)
waiting: …` line does), and **poll stale**, when the Slack worker has reported nothing for
five minutes (it reports an unchanged poll at least once a minute). Both outrank the bridge
status in the node's health.

## Context resets and thread notes

The Envoy session is one Claude terminal, but its conversation is not kept for long: the app
owns the durable state (inbox, drafts, requests, thread watches, notes), so the history only
costs tokens and sharpness. The playbook is the session's appended system prompt
(`--append-system-prompt-file`, rendered to `envoy-system-prompt.md` in the data dir at
launch), so the cached prefix (system prompt, playbook, tools) survives a reset. The launch
prompt no longer loads it with `get_playbook`.

When the Envoy is idle at an empty prompt with events or request turns to type, the app first
decides whether to clear the context by typing `/clear`:

- **Quiet period** (`fresh_after_secs`, default 300): the last batch was typed at least this
  long ago. `0` clears before every batch. Bursts within the window share one context.
- **Event guard** (`reset_after_events`, default 40): that many events and request turns went
  into the context since its last fresh start. `0` is off.
- **Token guard** (`reset_after_tokens`, default 80,000): the session's context as of its last
  turn (the last assistant message's input, cached and fresh, read from the transcript)
  passed this. `0` is off.

Guards trip at the next idle point even mid-burst. A context with no batch since the launch
or the last clear is never cleared, and nothing is cleared while a batch, a gateway send, a
relay, a Slack send or an approved draft is in flight, or while the terminal is busy or has
text in its composer. Pending events stay queued across the clear and are acknowledged only
when their batch is typed. The first batch after a clear starts with a `Fresh context:` line
stating the dry-run mode. A failed `/clear` is logged, batches go into the old context, and
the next reset is tried after ten minutes. Settings → Envoy holds all three thresholds; they
apply on the next Start.

Every typed event line also carries an addressee verdict, ` — addressee: the user | not the user |
unclear (<why>)`, computed from the message's mentions (`src/envoy/addressee.rs`): a DM or a
`<@USER_ID>` mention is for the user; another person, agent or group tagged without the user is not for
the user, unless the thread is the user's (a dwarf's `reach_out` thread, a watched thread, one the Envoy
posted in, or one whose earlier messages mention the user), which leaves it unclear for the Envoy
to judge; no mention is unclear. The user's id is the `envoy.user_id` setting until the
worker's `auth.test` call (what `slackmsg whoami` prints) answers, on every configure; the
resolved id then drives drops, searches and the verdict. The playbook's _Who it's for_ section
tells the Envoy what to do with each verdict.

Thread notes are the Envoy's memory across resets. `envoy_note {conversation, thread_ts?,
text}` replaces the note for a thread root (or, without `thread_ts`, a conversation), at most
600 characters, one line; empty text removes it. Notes live in `envoy-notes.json` (500 kept,
the least recently updated evicted). Every typed event line ends with ` — note: …` when its
thread (else its conversation) has one; request lines do the same for their bound thread;
`envoy_inbox` returns the `notes` of the events it lists, `envoy_event` and `envoy_request`
return `note`.

## Development verification

`cargo test` uses fake Slack senders, temporary bridge/data directories and fake terminal
actions. `cargo run --example envoy_panel -- --gateway --shot /tmp/envoy-gateway.png`
renders a synthetic native panel (add `--binding` to show a recipient proposal) without starting any app services or live Envoy session.

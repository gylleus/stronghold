# Envoy gateway

Agents reach people through `reach_out`. The app stores the request, the Envoy routes it,
and the draft follows Karl's destination approval policy. Replies arrive at the asking
terminal when its agent is idle at an empty prompt; `reach_out_status` also retains them.
Karl remains on `ask_karl` / `notify`.

## Tools available to every ant

Origin comes from the MCP process, not a tool argument. A nonblank terminal Origin is required.

| Tool | Arguments | Result |
| --- | --- | --- |
| `reach_out` | Required `to`, `ask`, `why`; optional `thread` (Slack permalink), `request_id`, `urgency` (`normal`, `soon`, `blocking`; default `normal`), `expect_reply` (default `true`) | `request_id`, `status`, `turn_id`, `merged`, `follow_up` |
| `reach_out_status` | Optional `request_id` | `requests`: the caller's requests, turns, drafts, thread binding and recorded deliveries |

Requests can queue while the Envoy is stopped. Without the app, the tool reports an error;
it does not authorize posting directly. Blocking urgency raises an app notification.
New requests are limited to six per hour per terminal. A follow-up identified by an owned
open `request_id` or the same owned thread has a separate 30/hour conversation allowance.
Exact duplicate asks merge without spending either budget. Follow-ups wait for the earlier
turn and a confirmed root thread; a failed or unknown send is never automatically retried.

Errors include missing Origin, invalid/oversized text, invalid urgency/permalink, Karl as
destination (use `ask_karl`), foreign/settled request, conflicting thread, ambiguous thread,
rate limit and unavailable storage. Foreign and unknown request ids both report
`Request unavailable`.

## Envoy-only tools

These require the persisted Envoy terminal identity. They are in its launch allowlist;
`reach_out` and `reach_out_status` are not. Karl must restart an existing Envoy session to
pick up the new launch tools after installing the app.

| Tool | Arguments | Result |
| --- | --- | --- |
| `envoy_request` | `id` | `request` with all original asks, requester identity, turns and replies |
| `envoy_route` | `id`, `conversation` (Slack C/D/G id), `destination_kind` (`agent`/`person`), `rationale`; optional `turn_id`, `thread_ts`, `draft_text` | `request_id`, `turn_id`, `draft` |
| `envoy_decline` | `id`, `reason`; optional `turn_id` | Updated `request`; rejection is queued for safe delivery to the asker |
| `envoy_link_reply` | `request_id`; at least one of `event_id`, `requester` | Updated `request` with the retained event linked or the adopted unknown owner bound |

For an agent destination, `draft_text` is forbidden. The app constructs the draft from the
requester's header and exact ask bytes, then appends attribution. Editing/approval cannot
replace the ask. For a person, `draft_text` is required and the original ask remains beside
the editable draft. Agent-comms always requires `destination_kind: "agent"`. DMs are checked
against Slack's recipient metadata; Karl is refused there too.

`turn_id` is required when multiple turns are queued. Route retries return the existing
draft. A bound conversation cannot be silently rerouted. Decline cannot cancel an already
dispatched send. To reconcile an ambiguous failed send, link a retained event in its known
thread; that records a resolved receipt, without sending again. An adopted request's
`requester` accepts a live fleet agent id or terminal; a known owner cannot be reassigned.

Routing errors also include missing/invalid destination kind, supplied agent draft text,
missing person draft text, conflicting route, earlier unfinished turn, absent live binding,
secret scan failure, recipient verification failure and unavailable worker/storage.

## Approval and replies

Settings → Envoy stores `policies`, a list of `{destination, mode}`. Destinations are exact
conversation ids, `dm`, or `*`, in that precedence order. Mode is `"approve"` (the default)
or `{"auto":{"veto_secs":120}}`. Veto windows range from zero to 86,400 seconds.
Task proposals always remain manual.

Automatic draft cards show a countdown and Cancel. Editing pauses their automatic approval.
The app checks secrets at creation and approval; dry-run prevents sending in either mode.
Policies and dry-run ON apply when saved; live mode and other launch settings require the
next Start. Changing approve to auto does not convert existing manual drafts. Restart gives
still-automatic pending drafts a fresh veto window; paused drafts remain paused. Failed
settings persistence blocks dispatch until repaired.

Successful sends persist Slack receipts and canonical thread bindings. Threads outside
channel feeds get app-owned bridge subscriptions and catch-up reads. Replies have a durable
handoff independent of bounded Inbox display history, and do not spend unsolicited relay
limits. A gone/reused asker falls back to its captured live coordinator; otherwise its reply
stays in the Inbox. Delivery checks fleet freshness, run/dispatch identity and available
terminal incarnation ids. Dry-run records would-be terminal delivery instead of sending it.
Terminal delivery is acknowledged after Orca succeeds; a crash between terminal execution
and saving that acknowledgment may replay a reply.

Inbox shows open/answered requests and unresolved orphan threads. Outbox shows settled
requests and draft decisions. Adopt open threads reads `slack-sub list`, skips channel feeds
and expired subscriptions, and creates unknown/sent requests idempotently. Keep retains an
orphan; Close settles it and removes the subscription after no open request needs it.
Dry-run Close records intent while retaining the subscription. The one-week direct-post
detector is deferred to migration after Karl picks its start date.

## Development verification

`cargo test` uses fake Slack senders, temporary bridge/data directories and fake terminal
actions. `cargo run --example envoy_panel -- --gateway --shot /tmp/envoy-gateway.png`
renders a synthetic native panel without starting any app services or live Envoy session.

# README demo media

`stronghold-demo.gif` is the inline README animation; `stronghold-demo.mp4` is
its downloadable H.264 version, and `stronghold-demo.png` is the still fallback.
All show a synthetic hold using the production renderer from revision `00af228`.

Captured on 2026-10-16 with **Resurrect 64** (by Kerrie Lake,
[bundled palette](../../assets/palettes/resurrect-64.json)), **Hold pixels**
(native pixelation), Fine outlines and daytime lighting.

## Demo fixtures

The capture used a temporary copy of [`examples/farm.rs`](../../examples/farm.rs)
outside the repository, linked to the unchanged Stronghold library. It retained
15 demo agents across three runs, three surface heaps and a 1280×800 viewport:
`--pixels farm --palette resurrect-64 --dwarves 15 --heaps 3 --size 1280x800`.

Two additional synthetic standalone agents, `review-ui` (“Polish the UI”) and
`review-docs` (“Update docs”), have `NodeKind::Terminal`, `NodeState::Done` and
review-ready markers in `ClanView::set_pr_ready`. They occupy the treasury.
Ordinary workers stay in their rooms even when done; standalone agents and
coordinators with review-ready work can enter the treasury.

The capture environment explicitly sets `STRONGHOLD_DATA_DIR`,
`STRONGHOLD_AGENTS_DIR` and `STRONGHOLD_SETTINGS` to scratch locations.
The scratch settings contain `{"user":{"name":"User"}}`, giving the portal
its generic label. `STRONGHOLD_HOUR=14` fixes daytime lighting.

This harness renders in-memory fixtures. It starts no live Orca, King or Envoy
services and sends no real messages. All task labels and Ledger entries are
synthetic. No customer data, merchant names, credentials, real terminal content
or private ticket details appear in the frame.

## Ten-second sequence

| Time | Production animation triggered by the harness |
| --- | --- |
| 0.8 s | An incoming Slack-to-Envoy relay: `FarmView::message`, receipt cue and `envoy_activity(false, …)`. |
| 3.0 s | The Envoy's outgoing reply effect: `envoy_activity(true, …)`. |
| 5.2 s | The King starts thinking and gathers a status report with `Pulse::StatusReport`. |
| 6.0 s | `BeamKind::Read` beams connect the King to every worker, showing the state reads. |
| 8.5 s | The King finishes thinking. |

These use the same receipt, send, status and read effects as the app. The base
demo also switches a worker between working and needing input every three
seconds. The treasury's two review agents remain visible throughout.

## Capture and encoding

Only the demo window was captured through macOS ScreenCaptureKit, with the
cursor and shadows excluded. The recording samples 150 frames at 15 fps,
crops the 32-point title bar, and keeps the 1280×800 content at its native
logical resolution without rescaling. The GIF uses a generated 64-colour
palette, no dithering and an infinite loop. The MP4 uses H.264, YUV 4:2:0,
CRF 18, fast-start metadata and no audio. Both play for ten seconds.
The PNG is the first frame. Encoding used FFmpeg; the capture helpers and
source frames stayed outside the repository. Only docs and media are committed.

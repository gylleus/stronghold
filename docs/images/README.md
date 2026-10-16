# README demo media

`stronghold-demo.gif` is the inline README animation; `stronghold-demo.mp4` is
its downloadable H.264 version, and `stronghold-demo.png` is the still fallback.
All show a synthetic hold using the production renderer from revision `96d1d45`.

Captured on 2026-10-16 with **Resurrect 64** (by Kerrie Lake,
[bundled palette](../../assets/palettes/resurrect-64.json)), **Hold pixels**
(native pixelation), Fine outlines and daytime lighting.

## Demo fixtures

The capture used a temporary copy of [`examples/farm.rs`](../../examples/farm.rs)
outside the repository, linked to the unchanged Stronghold library. It retained
15 demo agents across three runs and a 1280×800 viewport:
`--pixels farm --palette resurrect-64 --dwarves 15 --size 1280x800`.
Two unoccupied synthetic worktrees, `old-prototype` and `old-experiment`, provide
the surface heaps. A synthetic `CleanupJob` sends the demolition dwarf to the
first heap near the end; no real worktree cleanup runs.

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

## Slow sequence with settling pauses

Playback lasts 52.8 seconds. Each featured action finishes before the next
begins, with roughly one to two seconds of quiet between them. The only
intentional overlap is the King's thinking pose during his status checks.
The 44-second capture is played 20% longer, slowing all movement as well as
spacing the events. Times below refer to the final GIF and MP4.

| Time | Production animation triggered by the harness |
| --- | --- |
| 0–1.8 s | Quiet opening: the clan is already settled. |
| 1.8–3.2 s | The User portal sends a voice command to `run0-w2`; releasing it sends an instruction envelope. |
| 7.2 s | That dwarf travels to the input area. The others keep their slots. |
| 12.0 s | After reaching the input area and pausing, the dwarf returns to its room. |
| 16.8 s | After the return trip settles, an incoming Slack message reaches the Envoy. |
| 21.0 s | The Envoy replies. |
| 25.2 s | One worker sends a status message to another. |
| 29.4 s | A worker files knowledge in the Ledger. |
| 33.6 s | The Ledger sends knowledge to another worker. |
| 37.8 s | The King starts thinking and gathers a status report. |
| 42.0 s | Once the report ripple settles, read beams connect the King to every worker. |
| 43.9 s | The King finishes thinking. |
| 46.2–48.6 s | The cleanup dwarf arrives and works on a surface heap, then leaves. |
| 49–52.8 s | Quiet closing pause before the loop restarts. |

The base demo's repeating state toggle is disabled. Only `run0-w2` changes state,
once to needs-input and once back to working. Apart from that round trip and
the brief cleanup appearance, the dwarves stay at their posts; messages and
normal mining poses provide the activity. The two review agents remain in the
treasury throughout. All effects use the unchanged production renderer.

## Capture and encoding

Only the demo window was captured through macOS ScreenCaptureKit, with the
cursor and shadows excluded. The recording samples 528 frames at 12 fps,
crops the 32-point title bar, and keeps the 1280×800 content at its native
logical resolution without rescaling. The GIF uses a generated 64-colour
palette, no dithering and an infinite loop. The MP4 uses H.264, YUV 4:2:0,
CRF 18, fast-start metadata and no audio. Both play those frames at 10 fps
for 52.8 seconds, keeping the same pixels and giving each motion more time.
The PNG is the first frame. Encoding used FFmpeg; the capture helpers and
source frames stayed outside the repository. Only docs and media are committed.

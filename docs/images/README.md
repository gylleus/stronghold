# README demo media

`stronghold-demo.gif` is the inline README animation; `stronghold-demo.mp4` is
its downloadable H.264 version, and `stronghold-demo.png` is the still fallback.
All show the same synthetic hold from [`examples/farm.rs`](../../examples/farm.rs).

Captured on 2026-10-16 from renderer revision `5553195`, with **Resurrect 64**
(by Kerrie Lake, [bundled palette](../../assets/palettes/resurrect-64.json)),
**Hold pixels** (native pixelation), Fine outlines and daytime lighting.

The demo was built with `cargo build --example farm` and launched with:

```bash
STRONGHOLD_DATA_DIR=/tmp/stronghold-readme-demo/data \
STRONGHOLD_AGENTS_DIR=/tmp/stronghold-readme-demo/agents \
STRONGHOLD_HOUR=14 \
target/debug/examples/farm --pixels farm --palette resurrect-64 \
  --dwarves 15 --heaps 3 --size 1280x800
```

The scratch directories were empty. This example renders in-memory fixtures;
it does not start the dashboard's Orca, King or Envoy services. The retained
agents fill three runs and include working, idle, stalled, done and failed
states; a worker switches between working and needing input every three seconds.
All task labels and Ledger entries in this view are synthetic source fixtures.
No live terminals, tickets, customer data, merchant names or credentials were
captured. “Karl” is the local user's display name.

The recording captured only the demo window through macOS ScreenCaptureKit,
with the cursor and shadows excluded. It sampled 120 frames at 15 fps, cropped
the 32-point title bar, and kept the 1280×800 content at its native logical
resolution without rescaling. The GIF uses a generated 64-colour palette,
no dithering and an infinite loop. The MP4 uses H.264, YUV 4:2:0, CRF 18,
fast-start metadata and no audio. Both play for eight seconds. The PNG is the
first frame. Encoding used FFmpeg; the PNG frames and capture helper stayed
outside the repository.

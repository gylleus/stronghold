# Colony visual identity: Queen avatar, node icons, panel artwork (myr-oai)

Gives Myrstack an ant-colony look without changing layout or behaviour.

## Changes

- **Queen avatar** (`assets/art/queen.svg`): a crowned ant on a honey hexagon, the shape her
  graph node already had. Used for her graph node (a plain hexagon when tiny), the top bar
  title, the Queen button, her details header and the chat header.
- **Kind icons** (Phosphor): coordinator `tree-structure`, attendant `headset`, worker
  `hard-hat`, terminal `terminal-window`, subagent `robot`, queen `crown`. They lead each node
  label in the graph and appear in the sidebar kind filters, the tooltip, the details panel
  and the Queen's needs-input/stalled lists. Node shapes stay as they were, so kind still
  reads without the labels.
- **State glyphs** (Phosphor): working `play`, idle `moon`, needs input `exclamation-mark`,
  stalled `hourglass-high`, done `check`, failed `x`. These replace the hand-drawn glyphs
  inside nodes, the Queen's badge, the legend and the sidebar state filters. The glyph is
  centred on its drawn bounds and sized to 1.3× the node radius. Its font size is rounded
  to half pixels, so zooming reuses a few rasterized sizes. The contrast rules for glyphs
  and fills are unchanged.
- **Empty states**:
  - Queen chat: avatar, "Ask the Queen" and a hint.
  - Sidebar with no agents: anthill, "The colony is quiet".
  - Connecting to Orca: an ant in a comb cell.
  - Empty review queue: the same ant cell, small.
- `examples/visual_tour.rs`: the app's panels on a synthetic fleet with every kind and
  state, with `--light`, `--empty`, `--graph` and `--shot`. The screenshots in this PR were
  taken with it.

## Asset choice and licenses

- **Icon family: [Phosphor Icons](https://phosphoricons.com), MIT.** Chosen for its large set
  (1,500+ icons, including crown, hard-hat, headset and hourglass) and its uniform weights.
  It ships as a font: egui draws it as text, crisp at every zoom, with no rasterization step
  and one file for every icon. I bundle only the bold weight (495 KB), which reads better
  than regular at node sizes. The fill weight draws check/x/! as knocked-out squares, so I
  didn't use it. Phosphor has no ant icon.
- **Illustrations: [Microsoft Fluent Emoji](https://github.com/microsoft/fluentui-emoji)
  (Flat), MIT.** Its ant and crown supply the ant motif. I drew the hexagon, comb cell and
  anthill mound around them in the same flat style (that part is Myrstack's own).
- Considered and rejected:
  - game-icons.net: CC BY 3.0, so it needs attribution in the UI.
  - OpenMoji: CC BY-SA, share-alike.
  - Lucide, Tabler, Heroicons: fine licenses, but no ant, and SVG-only (each icon would need
    rasterizing).
- `assets/LICENSES.md` lists every file, its source and the full MIT texts. Both licenses only
  ask that the notice travel with copies, so the UI needs no attribution.
- Everything is `include_bytes!`'d; nothing is fetched at runtime. SVGs are rasterized by
  egui_extras' `svg` loader (resvg, MIT/Apache), the one new dependency feature. The art
  works on both themes because each piece has its own honey or earth backdrop.

## Performance

A headless benchmark drew a graph of N terminals for 300 frames (release build, same machine;
the script isn't committed):

| N | before | after |
| --- | --- | --- |
| 10 | 0.08 ms/frame | 0.13 |
| 40 | 0.40 | 0.49 |
| 80 | 0.34 | 0.43 |
| 150 | 0.27 | 0.36 |
| 300 | 0.36 | 0.36 |

That is about +0.05–0.1 ms per frame, mostly the extra label section and one glyph galley per
node; the graph only repaints on change. The Queen's texture is rasterized once at 256 px with
mipmaps, so zooming never re-rasterizes it. I considered caching galleys per node but didn't:
egui can recreate its font atlas, which would leave held galleys stale.

## Decisions to review

- Glyph for stalled: an hourglass instead of pause bars; idle is a moon instead of a dash.
- Kind icons lead node labels instead of replacing the shapes.
- If `theme::install` wasn't run (headless tests), icons fall back to the proportional font
  and show as missing glyphs rather than panicking.

## Screenshots

Before/after, dark: `before-dark.png` / `after-dark.png`. Light: `before-light.png` /
`after-light.png`. Empty fleet: `before-empty.png` / `after-empty.png`. Graph close-ups:
`before-graph-zoom.png` / `after-graph-zoom.png`, and the `-light` variants.

## Validation

- `cargo fmt`, `cargo clippy --all-targets`: the one warning (`theme.rs:51`) is on unchanged
  code.
- `cargo test`: 927 passed.
- New tests:
  - every icon is in the bundled font;
  - kinds and states have distinct icons;
  - icons are one em wide, which label extents rely on;
  - all three SVGs load as textures.

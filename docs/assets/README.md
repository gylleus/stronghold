# Myrstack art: inventory, Paleccino and asset descriptions

Status: **draft, theme pending.** The descriptions below describe today's ant-colony look so
that every asset has an entry. They are rewritten once Karl picks the new theme. Nothing here
has been generated, and the app's art and theme code are unchanged.

Bead: `myr-bov`.

## 1. What Myrstack ships today

### Bundled files (`assets/`, compiled in with `include_bytes!`)

| File | Size / format | Used for | Source | Licence |
| --- | --- | --- | --- | --- |
| `art/queen.svg` | 64×64 vector, rasterized up to 256 px | The Queen's avatar: graph hub node, top bar, Queen button, details header, chat header and chat empty state (`src/ui/art.rs`) | Ant and crown from Microsoft Fluent Emoji (Flat), on a honey hexagon drawn for Myrstack | MIT (Fluent parts); hexagon is Myrstack's own |
| `art/ant_cell.svg` | vector, shown 28–96 pt | "Connecting to Orca" empty state (`src/app/panes.rs`) and the empty review queue (`src/ui/review.rs`) | Fluent Emoji ant on a comb cell drawn for Myrstack | MIT (Fluent parts); cell is Myrstack's own |
| `art/anthill.svg` | vector | Sidebar empty state, "The colony is quiet" (`src/ui/sidebar.rs`) | Fluent Emoji ants on a mound drawn for Myrstack | MIT (Fluent parts); mound is Myrstack's own |
| `art/farm/ant_grey.png` | 16×32 PNG, two stacked 16×16 walk frames | Every ant in the ant farm. Recolored in code to Claude (amber), Codex (slate/teal) and plain (grey) coats (`src/ui/ant_farm/sprites.rs`) | "16x16 Ant Enemies" by ARoachIFoundOnMyPillow, OpenGameArt | CC0 |
| `art/farm/ant_red.png` | 16×32 PNG, two frames | Failed ants (drawn belly up) | same sheet set | CC0 |
| `art/farm/crown.png` | 16×16 PNG | The Queen's crown, carried item | "Gold treasure icons 16x16" by Bonsaiheldin, icon 9 | CC0 |
| `art/farm/coin.png` | 16×16 PNG | A crumb (one PR) carried to storage | same set, icon 2 | CC0 |
| `art/farm/coin_pile.png` | 16×16 PNG | One PR's crumbs on the storage pile | same set, icon 3 | CC0 |
| `art/farm/emote_exclamation.png` | 16×16 PNG | Speech bubble: needs input | Kenney "Emotes Pack", pixel style 1 | CC0 |
| `art/farm/emote_sleeps.png` | 16×16 PNG | Speech bubble: idle / asleep | Kenney | CC0 |
| `art/farm/emote_anger.png` | 16×16 PNG | Speech bubble: whipped / stalled | Kenney | CC0 |
| `art/farm/emote_cross.png` | 16×16 PNG | Speech bubble: failed | Kenney | CC0 |
| `art/farm/emote_star.png` | 16×16 PNG | Speech bubble: PR ready | Kenney | CC0 |
| `art/farm/emote_dots3.png` | 16×16 PNG | **Unused.** Bundled in the repo but no code loads it | Kenney | CC0 |
| `fonts/Phosphor-Bold.ttf` | icon font, 495 KB | Every UI icon: kind icons (crown, tree-structure, headset, hard-hat, terminal-window, robot), state glyphs (play, moon, !, hourglass, check, x) and toolbar icons (`src/ui/icons.rs`) | Phosphor Icons 2.1, bold | MIT |

Credits and licence texts are in [`assets/LICENSES.md`](../../assets/LICENSES.md).

### Other image files

- `packaging/Myrstack.icns`: the app icon, rendered by `scripts/make_icon.py`. It is Myrstack's own work.
- `docs/*.png`, `docs/visual-identity/*.png`: screenshots for docs and PRs. They are not shipped.
- `tests/fixtures/voice/*.wav`: a test fixture. It is not art.

### Art drawn in code (Myrstack's own, no licence question)

These are drawn in code, not loaded from files, but they belong to the look and would change
with a new theme:

- **Ground tiles** (`sprites.rs`): soil and tunnel tiles (32×32 generated noise, four shades
  each), a grass tile, the sky gradient (day and night palettes, `Palette`), stars.
- **Surface scenery** (`nature.rs`, character-map pixel art): tree, pine, bush, flowers
  (red, yellow, purple), grass tuft, rock, mushroom, cloud, moon, sun. Roots under plants.
- **Farm furniture** (`sprites.rs`, `comms.rs`): anthill mound, passages, the resting area's
  wooden bed with pillow and blanket, Karl's green warp pipe, the Envoy's dish/receiver and
  listening post with headphones, the incoming-mail tray.
- **Ant props and effects** (`sprites.rs`, `pops.rs`, `death.rs`, `envelope.rs`): the builder's hard
  hat and hammer, the whip's lash, flying dirt, snore "z", glow, sparkles, the Queen's call
  rings, state-change pops, envelopes and pings in flight.
- **Graph** (`src/ui/graph/shape.rs`): node shapes per kind (the Queen's hexagon and others),
  state fills and the indigo accent (`src/ui/theme.rs`, `ACCENT = #5a51e0`). The graph uses
  the Queen avatar texture and Phosphor glyphs.

## 2. The Paleccino pipeline

**What it is.** Paleccino lives at `~/repo/projects/paleccino` (`git@github.com:gylleus/paleccino.git`,
v0.2.0). It is the sprite pipeline pulled out of fitness-rpg into a standalone repo. Fitness-rpg
still runs its own copy in-tree (`image-generation/`, `uv run sprites …`). Paleccino is the
portable one, and the one to use for Myrstack.

**How it works.**

- CLI: `uv run paleccino --project <art>/sprites.toml <command>`. Python 3.12 and `uv`. The
  light CLI needs only numpy, pillow, scipy and scikit-image.
- `init <dir>`: creates `sprites.toml` and `palette.json`, with Jehkoba64 as the default or
  `--palette` for another. Each project owns its palette and art direction.
- `generate "<description>" --name id --out attempts/x`: runs fully locally. SDXL with a
  pixel-art LoRA draws a reference image, Wan 2.2 (5B by default) animates it, BiRefNet and
  SAM2 cut out the background, and the result is exported at a fixed palette.
  `--backend reference` produces a static sprite only. Useful options: `--animation
  name=motion`, `--once`, `--reference img.png` (animate an approved design), `--facing`,
  `--sprite-size`, `--frames`, `--seed`.
- `plan` → `register` → `recipe` → `import-sheets`: the authored-sheet workflow. A TOML
  definition (style, lighting, one entry per asset with `visual_description`, size and
  per-animation frames and timing) becomes exact prompts. You feed those to any image
  generator or artist, then import the resulting sheets.
- `map` / `check` / `verify`: maps colors to the palette (CIEDE2000, no dithering), rejects
  off-palette pixels, and replays archived exports.
- **Output:** transparent PNG frames and atlases at a fixed palette, a `manifest.json` with
  timing, a GIF preview and an offline `review.html`. Every attempt keeps its prompts, seeds,
  originals and hashes.
- **Dependencies and credentials:** no API keys, since inference is local. The model cache is
  `~/.cache/paleccino` (`PALECCINO_HOME` to move it). It needs about 8 GiB for `reference`,
  25 GiB for `wan5b` and 46 GiB or more for `wan14b`. It runs on Apple Silicon (MPS) or
  Linux/CUDA.

**Verdict: usable, with one blocker on this machine today.**

| Check (2026-10-11, this Mac) | Result |
| --- | --- |
| `paleccino doctor` | Runtime installed, MPS available, all wan5b models present (25 GiB already cached), nothing missing |
| `init` a Myrstack art project | Works |
| `plan` a 16 px ant with a 2-frame walk | Works, writes the exact prompt per action |
| `map` + `check` on today's `crown.png` | Works. The original fails the check (5 off-palette colors), the mapped copy passes |
| `generate --backend reference` (static 32 px ant) | **Refused:** `Need 0.0 GiB for models plus 4 GiB reserve`. The disk has 3.9 GiB free of 926 GiB. The 4 GiB reserve is hard-coded (`local/bootstrap.py`) |

The GPU stages were not run in this session. Paleccino's own validation (`validation/README.md`)
records real SDXL and Wan 5B runs on this M3 Max on 2026-10-07: a reference in about 20 s,
a 33-frame animation in about 6 min, with substantial swap. To generate, free about 10 GiB
first. The extra beyond the 4 GiB reserve covers swap and outputs.

**Fit for Myrstack, and what reuse needs:**

- **Reuse as an external tool, not a copy.** Add a Myrstack art project in the repo, say
  `art/` (`sprites.toml`, `palette.json`, the definitions TOML, `attempts/`, `asset-library/`),
  and run Paleccino from its own checkout with `--project art/sprites.toml`. Commit only the
  definitions, palette, chosen exports and provenance. Nothing needs copying, and Myrstack's
  Rust build gains no Python dependency. A `just art …` recipe can wrap the call.
- **Pixel art only.** Paleccino exports fixed-palette pixel PNGs. That suits the ant farm
  directly. The UI illustrations (Queen avatar, ant cell, anthill) are smooth SVGs today.
  Replacing them makes those pixel art too, loaded as PNGs with nearest filtering instead of
  rasterized SVG (a small change in `src/ui/art.rs`). Whether the UI should go pixel is part
  of the theme decision.
- **Small sizes.** The models draw at 512–768 px and downsample. At today's 16 px farm cells
  results will be rough; 32 px cells should hold up much better. The farm's layout is in farm
  texels (`px` per texel), so 32 px sprites mean adjusting the scale constants.
- **Short loops.** Wan makes 33-frame videos. Myrstack's ants use 2-frame walks. Use
  `frame_stride`/`--frames`, or the authored-sheet path, for short loops. Recoloring by coat
  (Claude/Codex/plain) keeps working if the exported ant uses a small, known set of palette
  colors.
- **Phosphor icons stay.** They are functional glyphs at text size, not sprites.

## 3. Asset descriptions (input to generation)

Shared art direction, today's look: cozy side-view ant farm, crisp dark outlines, chunky
clusters, soft upper-left light, three stepped shades per material, transparent background.
Palette: to be chosen with the theme. Format for every entry: transparent PNG at a fixed
palette, unless noted.

### UI illustrations (today SVG; pixel PNG if replaced)

1. **`queen`**: the Queen's avatar. It stands for the whole fleet and is the graph hub.
   *Size:* 64×64 master, shown 16–256 px, must read at 16 px. *Visual:* a regal ant queen,
   front three-quarter view, a small gold crown, inside a honey-gold hexagon badge with a
   darker rim. The silhouette is centered and simple enough to read as "crowned ant" at
   icon size.
2. **`ant_cell`**: "connecting" and "nothing to review" empty states. *Size:* 96×96, also
   shown at 28 px. *Visual:* one worker ant curled calmly inside a single wax comb cell,
   hexagonal and amber, waiting.
3. **`anthill`**: "the colony is quiet" empty state. *Size:* about 128×96. *Visual:* a low
   earthen anthill with a dark entrance, a cutaway of two or three chambers, a couple of
   small ants idling on top. Calm and quiet.
4. **`app_icon`**: macOS app icon (replaces `make_icon.py` output). *Size:* 1024×1024
   master, readable at 16 px. *Visual:* the Queen motif on a rounded-square tile; bold,
   one focal shape.

### Ant farm actors (16×16 cells today; 32×32 recommended)

5. **`ant_worker`**: every agent in the farm. *Size:* one cell, 2-frame walk loop
   (150 ms each), facing right; the code mirrors it for left. *Visual:* a small worker ant in
   side view: three body segments, six thin legs, two bent antennae, glossy chitin in a
   neutral grey with exactly six tones (outline, body, shade, highlight, dark, darkest) so
   code can recolor it per agent.
6. **`ant_failed`**: failed agents (drawn belly up). *Size and frames:* as `ant_worker`.
   *Visual:* the same ant in a dull red coat, legs slightly curled.
7. **`ant_queen`** (new; today the Queen is a grey ant with a crown item). *Size:* one
   cell, or 1.5× cells. *Visual:* a larger ant with a long abdomen, small folded wings and a
   built-in crown, same outline style as the workers.

### Farm items

8. **`crown`**: the Queen's crown (carried or worn). *Size:* 16×16 or 32×32. *Visual:*
   a small three-point gold crown with one red gem, bright highlight upper left.
9. **`crumb`**: one PR's crumb carried to storage (today a coin). *Size:* as `crown`.
   *Visual:* a single golden seed or breadcrumb, a warm, rounded morsel an ant can carry
   overhead.
10. **`crumb_pile`**: one PR's crumbs in storage. *Size:* as `crown`. *Visual:* a small
    heap of the same crumbs, three to five pieces.

### Status bubbles (16×16 today; drawn above an ant's head)

All share one bubble frame: a white rounded speech bubble with a dark outline and a tail at
the bottom left, the mark centered in the bubble.

11. **`bubble_alert`**: needs input. *Mark:* a bold "!" in warm orange.
12. **`bubble_sleep`**: idle or asleep. *Mark:* "Zz" in soft blue.
13. **`bubble_whip`**: stalled and whipped. *Mark:* an anger vein or puff in red.
14. **`bubble_failed`**: failed. *Mark:* a red "×".
15. **`bubble_pr`**: PR ready. *Mark:* a gold star.
16. **`bubble_thinking`** (optional, replaces the unused dots3): working. *Mark:* three dots.

### Surface scenery (pixel maps in `nature.rs` today)

Sizes are farm texels; big pieces are drawn at 2× scale.

17. **`tree`**: 15×16, 2× scale, rooted. A round deciduous tree, layered green canopy, short
    brown trunk.
18. **`pine`**: 9×13, 2× scale, rooted. A narrow evergreen with tiered branches.
19. **`bush`**: 11×6, 2× scale, rooted. A low round shrub.
20. **`flower_red` / `flower_yellow` / `flower_purple`**: 5×7. A single stem with two
    leaves and a five-petal head with a yellow center, one variant per petal color.
21. **`tuft`**: 7×5. A clump of grass blades.
22. **`rock`**: 8×5. A small grey stone with a lit top face.
23. **`mushroom`**: 6×6. A small red-capped mushroom with white spots.
24. **`cloud`**: 20×6, 2× scale. A flat-bottomed puffy cloud, white with a pale-blue
    underside.
25. **`moon`**: 8×8, 2× scale. A pale full moon with two or three craters.
26. **`sun`**: 9×9, 2× scale. A yellow sun with short pixel rays.

### Ground and furniture (code-drawn today; candidates for sprites or tiles)

27. **`soil_tile`**: 32×32, seamless tiling. Packed brown earth with scattered pebbles and
    four tones.
28. **`tunnel_tile`**: 32×32, seamless. Darker, smoother dug-out earth for passages and
    rooms.
29. **`grass_tile`**: a seamless horizontal strip along the surface. A grass edge with
    blades overhanging the soil.
30. **`mound`**: about 24×8. A pile of dug-out dirt around the shaft entrance.
31. **`bed`**: about 24×10. A small wooden bed with a headboard at the left, a mattress and a
    pillow, for the resting area (parked agents).
32. **`blanket`**: about 12×6. A patterned quilt laid over a sleeping ant's back half.
33. **`pipe`**: about 16×24, plus a stretchable body. Karl's green warp pipe on the surface
    where his messages pop out. A rim lip on top, a body that can stretch vertically, and a
    label area on the body.
34. **`envoy_dish`**: about 16×16. The Envoy's surface receiver: a small satellite dish on a
    post. Needs a normal, stopped (grey) and receiving (gold glint) variant.
35. **`listening_post`**: about 16×12. An underground desk with a radio set; the Envoy ant
    wears headphones there (`headphones`, an 8×4 overlay).
36. **`mail_tray`**: about 12×8. The incoming-mail tray, with an optional count badge.
37. **`envelope`**: 8×6. A sealed letter in flight between ants.
38. **`hard_hat_hammer`**: an overlay on a builder ant. A yellow hard hat and a small
    hammer, two frames: raised and striking.
39. **`whip`**: a 2-frame overlay. A thin lash poking from above and behind.

Effects (glow, sparkles, rings, pops, dirt clods, snore "z", stars, sky gradient) stay
code-drawn. Only their colors follow the theme palette.

## 4. Open questions for Karl

- **Theme:** what should Myrstack look like? This decides the palette, the art direction
  above, the UI accent (`theme.rs`), and whether the ant-colony motif stays.
- Should the UI illustrations (Queen avatar, empty states, app icon) become pixel art too,
  or stay smooth vector?
- Farm sprite resolution: keep 16 px, or move to 32 px (recommended for generated art)?
- Disk: generation needs about 10 GiB free on this Mac (3.9 GiB today).

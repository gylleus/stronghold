# Myrstack art: inventory, Paleccino and asset descriptions

Status: **draft for Karl's approval.** Karl chose the theme on 2026-10-11: the ant colony
becomes **dwarves living underground, mining for gold and treasure**, at higher sprite
resolution and fidelity. Section 3 maps the fleet onto that world and section 4 lists every
resource it needs. Only test images have been generated, and the app's art and theme code
are unchanged.

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

**Verdict: works on this Mac, end to end.** The art still needs review: the models give good
raw material, not finished sprites.

| Check (2026-10-11, this Mac) | Result |
| --- | --- |
| `paleccino doctor` | Runtime installed, MPS available, all wan5b models present (25 GiB already cached), nothing missing |
| `init` a Myrstack art project | Works |
| `plan` a 16 px ant with a 2-frame walk | Works, writes the exact prompt per action |
| `map` + `check` on today's `crown.png` | Works. The original fails the check (5 off-palette colors), the mapped copy passes |
| `generate --backend reference`, a dwarf miner | Works. SDXL reference in 34 s, then cutout and a fixed-palette export. A first try was refused for disk space (3.9 GiB free, 4 GiB reserve hard-coded); Karl freed space and the rerun passed |
| Same reference exported at 32, 48 and 64 px | Works. 32 px is muddy, 48 px reads as a dwarf, 64 px is clearly better (see below) |

![SDXL reference for the test dwarf](tests/dwarf-reference.png)
![The same dwarf exported at 32, 48 and 64 px, Jehkoba64 palette](tests/dwarf-32-48-64.png)

What the test showed:

- **The reference stage is good.** It produced a clean, characterful pixel-art dwarf.
- **The prompt isn't followed literally.** It faced the front although I asked for a side
  view, and it dropped the pickaxe and candle lamp. Paleccino's own validation notes the same
  facing problem. Plan to generate a few seeds per asset and pick, or redraw a reference and
  feed it back with `--reference`.
- **Resolution matters more than anything else.** Downsampling a 768 px reference to 32 px
  loses the face and beard. 48 px is the smallest size that keeps the character.
- **The default palette hurts.** Jehkoba64 turned the red beard muddy. A palette chosen for
  the theme (warm golds and ember, cool stone, a few skin and beard ramps) should fix most
  of that.
- **Animation not tried yet.** The Wan animation stage was not run for Myrstack. Paleccino's
  validation shows it works on this Mac (about 6 min per 33-frame clip) with drift and
  clipping that need review.

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
- **Bigger sprites.** The models draw at 512–768 px and downsample. Today's 16 px farm cells
  are too small for generated art; the test above puts the floor at 48 px. The farm's layout is
  in farm texels (`px` per texel), so bigger sprites mean adjusting its scale constants and
  the sizes of the drawn props.
- **Short loops.** Wan makes 33-frame videos. Myrstack's ants use 2-frame walks. Use
  `frame_stride`/`--frames`, or the authored-sheet path, for short loops. Recoloring by coat
  (Claude/Codex/plain) keeps working if the exported ant uses a small, known set of palette
  colors.
- **Phosphor icons stay.** They are functional glyphs at text size, not sprites.

## 3. The dwarf hold: the fleet as a mine

The ant farm becomes a cutaway of a dwarven mine under a mountainside. Each fleet concept
gets a dwarf counterpart. The farm's layout and behaviour stay as they are (same places,
same state → place rules); only what is drawn changes.

| Fleet concept | Ant farm today | Dwarf hold |
| --- | --- | --- |
| The Queen (the fleet hub) | Crowned ant in the Queen's chamber | **The King under the Mountain** on a stone throne in the **throne hall** |
| Coordinator / attendant | Ant at a tunnel's mouth | **Foreman** with a lantern and a scroll of plans, at the **shaft head** |
| Worker / terminal | Ant digging in the chamber | **Miner** with a pickaxe at the **ore face** at the end of the shaft |
| Worker editing or building | Hard hat and hammer overlay | Miner **hammering timber supports** with a mallet, sparks flying |
| Subagent (trails its parent) | Small faded ant | **Apprentice**: a young, short-bearded dwarf carrying a lantern behind their elder |
| The Envoy (outside world) | Dish on the surface, listening post with headphones | **Ravenmaster**: a raven roost on the surface, and a desk underground with a brass **speaking tube** and ear trumpet |
| Claude / Codex / unknown agent | Amber / slate-teal / grey ant coat | **Livery colour** of hood and tunic: amber-ochre / slate with teal trim / plain grey |
| Needs input (waiting on Karl) | Ants queue at the landing on the surface | Dwarves queue at the **mine entrance** on the mountainside |
| Karl's messages | Pop out of a green warp pipe | Pop out of a brass **message tube** rising from the rock, as sealed scrolls |
| Done / PR ready | Ant carries a crumb to the storage pile | Dwarf carries a **gold nugget** to the **treasury**, where each PR is a **treasure heap** |
| Idle / stalled | Ant asleep, "Zz" | Dwarf **dozing** against the rock, sitting on a crate |
| The whip (nudge a stalled agent) | A lash pokes the ant | A **pebble bonks the helmet**: a small stone arcs in, "tink", the dwarf jolts awake |
| Failed | Red ant, belly up | Dwarf **knocked flat** on their back, helmet off, red livery, cross bubble |
| Leaving the fleet | Ant flips and falls | The knocked-flat dwarf falls and fades (same motion) |
| Parked (resting area) | Ants in wooden beds | Dwarves in **bunks** with straw mattresses, wool blankets and nightcaps |
| Messages between agents | Envelopes in flight | **Sealed scrolls** in flight |
| Tool calls | Beams through the soil | **Glowing rune lines** through the rock |
| Gallery and tunnels | Dug passages in soil | A **haulage way** with mine-cart rails, shafts shored with **timber**, lit by **wall torches** |
| Surface | Meadow: trees, flowers, mushrooms | **Mountainside**: pines, boulders, heather, alpine flowers, distant peaks |

## 4. Resources needed

Shared art direction for every generated asset:

- **Style:** chunky, readable pixel art at 3× today's resolution. Crisp dark outlines, warm
  torch-lit interiors against cool stone, cozy rather than grim.
- **Lighting:** soft upper-left key light, three or four stepped shades per material. Gold
  and gems get a bright specular pixel.
- **View:** strict side view for everything in the hold; actors face right and the code
  mirrors them.
- **Palette:** one fixed palette for the hold (to be chosen; see section 5).
- **Format:** transparent PNG frames and atlases from Paleccino, unless noted.

**Scale.** One farm texel today becomes 3 texels, so a 16 px ant cell becomes a **48×48**
actor cell, and every prop and tile below is sized at that density. Sizes are given in final
pixels.

**Livery recoloring.** Every dwarf's hood and tunic use one reserved four-tone grey ramp, and
the code recolors that ramp per agent (Claude, Codex, plain, failed). The ants work the same
way today. This needs one sprite per character rather than four, and the palette must keep
the ramp's exact colours. Beards could get the same treatment (ginger, brown, black, white,
picked per agent) so the crowd looks varied. That is optional.

### 4a. Characters (48×48 cells unless noted)

Animations are short loops picked from Paleccino's generated clips. Frame counts are the
target; durations are tuned in code.

1. **`miner`**: workers and loose terminals, most of the crowd. *Visual:* a stout dwarf
   miner, side view: iron helmet with a small candle lamp, a bushy beard to the belt,
   grey-ramp hood and tunic (livery), brown leather apron, thick boots, a pickaxe.
   *Animations:* `walk` 4 frames; `mine` 4 (pickaxe swing into the rock face; chips are
   drawn by code); `build` 4 (mallet striking a timber post); `carry` 4 (walking with both
   hands raised, holding a nugget the code draws); `wait` 2 (standing, looking up, tapping a
   foot); `doze` 2 (sitting slumped on a crate, beard rising with each breath);
   `knocked_out` 1 (flat on the back, helmet off beside them).
2. **`foreman`**: coordinators and attendants. *Visual:* an older dwarf with a grey braided
   beard, a peaked leather cap instead of a helmet, a lantern on a pole and a rolled scroll
   of plans under one arm, livery tunic with a brass badge. *Animations:* `stand` 2
   (consulting the scroll, lantern swaying); `walk` 4; `carry` 4; `wait` 2; `doze` 2;
   `knocked_out` 1. An attendant variant (a ledger instead of a scroll) is optional.
3. **`apprentice`**: subagents. *Size:* 36×36 inside a 48 cell. *Visual:* a young dwarf
   with a short stubbly beard, oversized helmet, carrying a small lantern; livery as their
   elder. *Animations:* `walk` 4, `idle` 2.
4. **`king`**: the Queen node. *Size:* 64×64 (the Queen's spot is bigger than a cell).
   *Visual:* the dwarf King under the Mountain, seated: a long white beard in two braids
   bound with gold rings, a heavy gold crown with a red gem, a deep red and gold robe, a
   rune-carved scepter. *Animations:* `idle` 2 (stroking the beard); `call` 2 (raising the
   scepter; the code draws the call rings). His crown is part of the sprite, so the separate
   crown item is no longer needed on him.
5. **`ravenmaster`**: the Envoy. *Visual:* a lean dwarf in a dark feathered cloak, a raven
   on one shoulder, holding a brass ear trumpet. *Animations:* `listen` 2 (trumpet to the
   ear); `receive` 2 (the raven flaps).
6. **`raven`**: the Envoy's messenger on the surface. *Size:* 24×24. *Visual:* a glossy
   black raven with a tiny scroll tied to one leg. *Animations:* `perch` 2, `fly` 4.

### 4b. Treasure and items

7. **`gold_nugget`**: a crumb, one PR's haul being carried. *Size:* 24×24. *Visual:* a
   lumpy, bright gold nugget with a white glint.
8. **`treasure_heap`**: one PR on the treasury floor. *Size:* 48×32. *Visual:* a small heap
   of gold nuggets and coins with one cut gem on top. Variants: ruby, emerald and sapphire
   gems, so neighbouring heaps differ.
9. **`treasure_chest`** (treasury dressing): *Size:* 48×40. *Visual:* an open iron-bound
   oak chest spilling gold.
10. **`scroll`**: a message in flight, and Karl's messages from the tube. *Size:* 18×12.
    *Visual:* a rolled parchment tied with string and a red wax seal.
11. **`pebble`**: the whip. *Size:* 9×9, 2 frames. *Visual:* a small grey stone; frame two
    adds a white "tink" spark.

### 4c. Status bubbles (36×36, drawn over a dwarf's head)

All share one frame: a parchment-cream speech bubble with a dark ink outline and a tail at the
bottom left, the mark centred.

12. **`bubble_alert`**: needs input. *Mark:* a bold "!" in ember orange.
13. **`bubble_sleep`**: idle or dozing. *Mark:* "Zz" in cool blue.
14. **`bubble_whip`**: stalled. *Mark:* a red anger puff.
15. **`bubble_failed`**: failed. *Mark:* a red "×".
16. **`bubble_pr`**: PR ready. *Mark:* a faceted gold gem with a sparkle.
17. **`bubble_working`** (optional): *Mark:* three dots.

### 4d. Set pieces: places in the hold

18. **`mine_entrance`**: the landing, where dwarves wait for Karl. *Size:* about 96×72.
    *Visual:* a timber-framed portal cut into the rocky hillside, a lantern on a hook, a
    small wooden sign with crossed pickaxes. Dwarves stand in line beside it.
19. **`message_tube`**: replaces Karl's green pipe. *Size:* 48×72, built as three parts:
    a flared brass bell mouth on top, a body segment that tiles vertically (it stretches while
    a message pops out), and a riveted base flange. A blank brass plate on the body holds the
    code-drawn label.
20. **`throne`**: the throne hall. *Size:* 64×80. *Visual:* a massive carved stone throne,
    gold inlay and a red cushion, on a two-step dais.
21. **`banner`**: throne-hall dressing. *Size:* 24×64. *Visual:* a long red and gold
    banner with a crown-and-pickaxe emblem, hanging from a pole.
22. **`brazier`**: throne hall and treasury. *Size:* 24×36, 4 frames. *Visual:* an iron
    bowl on three legs with flickering coals and flame.
23. **`shaft_head`**: the mouth of each tunnel, where the foreman stands. *Size:* 48×48.
    *Visual:* a timber headframe over the shaft with a pulley and a hanging lantern.
24. **`ore_face`**: the rock face at each chamber's end, where miners dig. *Size:* 48×96,
    tiles vertically. *Visual:* rough rock with glittering gold veins and a few embedded gems.
25. **`timber_support`**: tunnel dressing, repeated. *Size:* 24×72. *Visual:* two wooden
    posts and a crossbeam with iron brackets.
26. **`wall_torch`**: lights the haulage way. *Size:* 15×30, 4 frames. *Visual:* an iron
    sconce with a burning torch.
27. **`mine_cart`**: gallery dressing, optional animated. *Size:* 48×36. *Visual:* a small
    iron cart on four wheels, heaped with ore.
28. **`bunk`**: the resting area. *Size:* 72×48. *Visual:* a two-tier wooden bunk with straw
    mattresses and pillows. Blankets go over the sleepers separately.
29. **`blanket`**: *Size:* 36×18. *Visual:* a red and green plaid wool blanket.
30. **`nightcap`**: a 2-frame overlay for sleepers. *Visual:* a floppy striped cap with a
    pom-pom.
31. **`raven_roost`**: the Envoy's surface receiver. *Size:* 48×64. *Visual:* a weathered
    post with a crossbar and a small birdhouse. Variants: `normal` (raven perched),
    `stopped` (empty, greyed), `receiving` (raven landing, gold glint).
32. **`speaking_desk`**: the Envoy's underground post. *Size:* 48×36. *Visual:* a small
    stone desk with a coiled brass speaking tube rising into the rock, and an inkpot.
33. **`scroll_rack`**: the incoming-mail tray. *Size:* 36×24. *Visual:* wooden pigeonholes
    with rolled scrolls; the code draws the count badge.

### 4e. Tiles (seamless, 48×48)

34. **`rock_tile`**: the body of the mountain. Grey-brown granite with faint strata lines,
    scattered pebbles, four tones; quiet enough to sit behind the actors.
35. **`deep_rock_tile`**: lower levels. The same rock, darker and cooler.
36. **`tunnel_tile`**: dug passages and chambers. Darker worked stone, smoother, with chisel
    marks, so passages read as hollow.
37. **`rail_strip`**: the haulage way floor. A horizontal strip, 48×12: wooden sleepers and
    two iron rails.
38. **`vein_gold`, `vein_gem`**: 48×48 accents sprinkled into `rock_tile`. Thin gold
    veins in one, small embedded crystals in the other.
39. **`surface_edge`**: the top of the mountain. A horizontal strip, 48×24: heather and
    short grass over rock, with a few loose stones overhanging.

### 4f. Surface and cave scenery

40. **`pine`**: 54×78, rooted. A tall mountain pine with tiered branches.
41. **`spruce`**: 42×66, rooted. A narrower dark spruce.
42. **`boulder`**: 48×33. A big mossy boulder.
43. **`rock_small`**: 24×15. A small grey stone.
44. **`heather`**: 21×15. A clump of purple heather.
45. **`flower_edelweiss` / `flower_gentian` / `flower_alpine_rose`**: 15×21 each. White,
    deep blue and pink alpine flowers.
46. **`mushroom`**: 18×18. A red-capped mushroom with white spots.
47. **`cloud`**: 120×36. A flat-bottomed cloud, white with a pale-blue underside.
48. **`moon`**: 48×48. A pale full moon with craters.
49. **`sun`**: 54×54. A warm yellow sun with short rays.
50. **`peaks`**: a wide backdrop strip, about 480×96, tileable. Distant snow-capped peaks
    in hazy blue, drawn behind the surface.
51. **`cave_crystals`**: underground dressing. *Size:* 24×24. *Visual:* a cluster of glowing
    blue-violet crystals growing from the rock.
52. **`glow_mushrooms`**: underground dressing. *Size:* 18×15. *Visual:* small pale
    mushrooms with a faint teal glow.
53. **`stalactite`**: chamber ceilings. *Size:* 12×30. *Visual:* a dripping rock spike.

### 4g. UI illustrations (pixel art, replacing the SVGs)

Each is generated at its largest size and also exported at the listed smaller sizes, so it
is drawn at a whole-pixel scale. `src/ui/art.rs` would load PNGs with nearest filtering.

54. **`king_avatar`**: replaces `queen.svg` everywhere the Queen appears (graph hub, top
    bar, Queen button, chat). *Sizes:* 128, 64, 32 and 16 px. *Visual:* a front-facing
    portrait of the King: white braided beard, gold crown with a red gem, framed in a
    carved stone hexagon with a gold rim (the hexagon keeps the graph node's shape).
55. **`empty_connecting`**: "Connecting to Orca". *Size:* 96×96. *Visual:* a dwarf holding
    up a lantern, peering down a dark tunnel.
56. **`empty_quiet`**: "The hold is quiet" (sidebar, no agents). *Size:* 128×96.
    *Visual:* the mine entrance at night, an empty cart parked on the rails, one lantern
    glowing, stars above.
57. **`empty_review`**: empty review queue. *Size:* 48×48. *Visual:* a closed treasure
    chest with a small padlock.
58. **`app_icon`**: replaces `make_icon.py`'s output. *Size:* a 256 px pixel master
    scaled to 1024 for the icns. *Visual:* two crossed pickaxes behind a gold-rimmed stone
    hexagon holding a glowing gem.

### 4h. Not from Paleccino (code changes, after approval)

- **Effects stay code-drawn, recoloured to the theme:**
  - glow → warm torchlight
  - sparkles → gold glints
  - the Queen's call rings → the King's horn rings
  - state-change pops
  - dirt clods → rock chips with sparks
  - snore "z", stars, and the day and night sky
- **UI theme** (`src/ui/theme.rs`): the indigo accent (`#5a51e0`) becomes forge gold or
  ember on dark basalt panels. Graph node colours follow it.
- **Phosphor icons stay.** Check the set for pickaxe, lantern or shovel glyphs that fit the
  kinds better.
- **Farm scale:** the code moves to 48 px cells, and walks go from 2 to 4 frames.
- **Words:** labels and empty-state copy ("colony", "ants") change to the hold's.
- **The name:** Myrstack is Swedish for anthill, which no longer fits. Renaming is Karl's call
  and isn't part of this work.

## 5. Decisions for Karl before generation

1. **The concept mapping in section 3.** Does each role read right to you: King, foreman,
   miner, apprentice, ravenmaster?
2. **The whip.** A pebble bonking the helmet is my suggestion. Alternatives: the foreman
   ringing a hand bell, or a splash from a water bucket.
3. **Karl's pipe.** A brass message tube is my suggestion. Alternatives: a mine-lift cage
   that comes down, or a well with a bucket.
4. **UI illustrations as pixel art** (King avatar, empty states, app icon), or keep them as
   smooth vector art drawn by hand?
5. **Resolution:** 48 px actor cells (3× today), as above. 64 px looks better still but fits
   fewer dwarves on screen.
6. **Palette.** The first step after approval would be to export the test dwarf against two
   or three candidate palettes (ENDESGA 32, Jehkoba64 and one warm 48–64 colour Lospec
   palette) so you can pick one by eye.
7. **Scope of the first batch.** I'd suggest the cast (1–6), treasure and bubbles (7–17)
   and the tiles (34–39) first, wired into the farm, then the set pieces, scenery and UI
   illustrations.

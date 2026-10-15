# Importing art palettes

Open **Settings** (gear or **⌘,**), choose **Art**, paste into **Import a palette**, then press **Enter** or **Import & apply**. The hold and the animated preview change immediately. Pixel size and outline width keep their existing values.

Accepted examples:

| Input | Example |
| --- | --- |
| Lospec name or slug | `Ice Cream GB` or `ice-cream-gb` |
| Lospec URL, including JSON links | `https://lospec.com/palette-list/endesga-16.json` |
| Coolors URL | `https://coolors.co/264653-2a9d8f-e9c46a` |
| Dash-separated hex code | `264653-2a9d8f-e9c46a` |
| Plain hex list | `#264653, #2a9d8f, #e9c46a` |

Hex colours accept three or six digits, optional `#`, upper or lower case, and spaces, newlines, commas, semicolons or dashes between colours. Duplicate colours collapse. A bare three- or six-digit hex value is a colour; use a Lospec URL for a palette whose name is itself a hex value. Up to 4,096 colours are supported.

Use **Recent** to switch among the last eight choices. The **Palette** dropdown retains all imports alongside all predefined palettes and **Own colours**. Re-pasting an imported Lospec name or URL uses the cache without a network request. Coolors and hex imports never need the network.

Lospec imports use `https://lospec.com/palette-list/<slug>.json`, verified with the real responses below on 2026-10-15. Fetching happens on a worker thread, with a 5-second connection timeout and 15-second total timeout. Invalid input, missing palettes, network failures and cache write failures appear inline. A failed import leaves the current palette in place; **Cancel** discards the pending result.

The library, source credits and recent choices live in `palettes.json` next to Stronghold's settings file. The active palette also stores its colours in the art settings, independently of the library. A damaged library is reported and left untouched; new imports are session-only until it is repaired.

## Rendering

Bundled palettes keep their existing mapping. Imported palettes sort colours by CIELAB lightness and use hue/chroma distance to preserve accents. Their lightness range maps onto the drawing's range. The darkest colour supplies ink and label plates; the lightest supplies text. Worked stone and uncut rock each avoid the ink and each other. Dwarf textures, world props, terrain and scene shapes all resolve the same self-contained palette selection, so changing palettes invalidates their caches together.

If a palette cannot provide 7:1 light/dark contrast, the renderer derives support shades from its end colours. Gaps greater than 40 CIELAB lightness units get support midtones so nearly monochrome palettes can still show stone. The preview reports any added shades. All imported farm text, including gold signs and counters, gets a light foreground and a dark plate, preserving contrast over bright backgrounds. Texture transparency and chosen outline width are preserved.

## Visual evidence

Captured with the real native farm renderer and the same synthetic clan. The JSON files are the fetched Lospec responses, also used as offline regression fixtures.

| View | Screenshot | Palette credit |
| --- | --- | --- |
| Before: own colours | [Before](assets/tests/custom-palettes/before.png) | Stronghold's own art |
| Four colours | [Ice Cream GB](assets/tests/custom-palettes/ice-cream-gb.png) | [Kerrie Lake](https://lospec.com/palette-list/ice-cream-gb) |
| Sixteen colours | [Endesga 16](assets/tests/custom-palettes/endesga-16.png) | [ENDESGA](https://lospec.com/palette-list/endesga-16) |
| Sixty-four colours | [Resurrect 64](assets/tests/custom-palettes/resurrect-64.png) | [Kerrie Lake](https://lospec.com/palette-list/resurrect-64) |
| Art controls and live preview | [Importer](assets/tests/custom-palettes/importer.png) | Ice Cream GB |

Reproduce from the repo root:

```sh
cargo run --example farm -- --reduced-motion --shot docs/assets/tests/custom-palettes/before.png
cargo run --example farm -- --reduced-motion --import ice-cream-gb --shot docs/assets/tests/custom-palettes/ice-cream-gb.png
cargo run --example farm -- --reduced-motion --import endesga-16 --shot docs/assets/tests/custom-palettes/endesga-16.png
cargo run --example farm -- --reduced-motion --import resurrect-64 --shot docs/assets/tests/custom-palettes/resurrect-64.png
cargo run --example farm -- --reduced-motion --import ice-cream-gb --palette-editor --shot docs/assets/tests/custom-palettes/importer.png
```

The example uses a temporary palette library and does not change Karl's settings. It also emits animation-stage captures (`-break`, `-walk`, `-cleared`); the comparison above uses its first settled frame.

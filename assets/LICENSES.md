# Bundled assets: sources and licenses

Everything here is compiled into the binary (`include_bytes!`); nothing is fetched at runtime.

| File | Source | License |
| --- | --- | --- |
| `fonts/Phosphor-Bold.ttf` | [Phosphor Icons](https://phosphoricons.com) 2.1, `@phosphor-icons/web@2.1.2`, bold weight, unmodified | MIT |

The Phosphor font uses the MIT license; its notice and license text are included below.
The mine, its scenery and dwarves are drawn in Rust with the cutout and pixel renderers
in `src/ui/dwarf/` and `src/ui/ant_farm/`, with one generated asset:

- `art/tools/cleanup_hammer.png`: generated for Myrstack with OpenAI's built-in image
  generation tool on 2026-10-13, using no reference image. The exact prompt is in
  `docs/assets/prompts/cleanup-hammer.txt`. The app downsamples it and maps its colours
  at texture creation. No third-party source artwork is bundled.

To add an icon, pick it at phosphoricons.com and add its codepoint (from the package's
`src/bold/style.css`) to `src/ui/icons.rs`: the whole bold font is bundled.

## Palettes (`palettes/`)

Colour palettes from [Lospec](https://lospec.com/palette-list), unmodified JSON, used by the
dwarf farm and the dwarf lab (`examples/dwarf_lab.rs`). A palette is a list of colours, not
artwork; the authors are credited here as a courtesy.

| File | Palette |
| --- | --- |
| `palettes/aap-64.json` | [AAP-64](https://lospec.com/palette-list/aap-64) by Adigun A. Polack, 64 colours |
| `palettes/apollo.json` | [Apollo](https://lospec.com/palette-list/apollo) by AdamCYounis, 46 colours |
| `palettes/dawnbringer-32.json` | [DawnBringer 32](https://lospec.com/palette-list/dawnbringer-32) by unknown, 32 colours |
| `palettes/endesga-32.json` | [Endesga 32](https://lospec.com/palette-list/endesga-32) by ENDESGA, 32 colours |
| `palettes/fantasy-24.json` | [Fantasy 24](https://lospec.com/palette-list/fantasy-24) by unknown, 24 colours |
| `palettes/jehkoba64.json` | [Jehkoba64](https://lospec.com/palette-list/jehkoba64) by Jehkoba, 64 colours |
| `palettes/journey.json` | [Journey](https://lospec.com/palette-list/journey) by 🍍PineappleOnPizza🍍, 64 colours |
| `palettes/lospec500.json` | [Lospec500](https://lospec.com/palette-list/lospec500) by unknown, 42 colours |
| `palettes/na16.json` | [NA16](https://lospec.com/palette-list/na16) by Nauris, 16 colours |
| `palettes/oil-6.json` | [Oil 6](https://lospec.com/palette-list/oil-6) by unknown, 6 colours |
| `palettes/pico-8.json` | [PICO-8](https://lospec.com/palette-list/pico-8) by unknown, 16 colours |
| `palettes/resurrect-64.json` | [Resurrect 64](https://lospec.com/palette-list/resurrect-64) by Kerrie Lake, 64 colours |
| `palettes/slso8.json` | [SLSO8](https://lospec.com/palette-list/slso8) by Luis Miguel Maldonado, 8 colours |
| `palettes/sweetie-16.json` | [Sweetie 16](https://lospec.com/palette-list/sweetie-16) by unknown, 16 colours |
| `palettes/vinik24.json` | [Vinik24](https://lospec.com/palette-list/vinik24) by Vinik, 24 colours |
| `palettes/zughy-32.json` | [Zughy 32](https://lospec.com/palette-list/zughy-32) by unknown, 32 colours |

## Phosphor Icons

```
MIT License

Copyright (c) 2020-2021 Phosphor Icons

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

# Bundled assets: sources and licenses

Everything here is compiled into the binary (`include_bytes!`); nothing is fetched at runtime.

| File | Source | License |
| --- | --- | --- |
| `fonts/Phosphor-Bold.ttf` | [Phosphor Icons](https://phosphoricons.com) 2.1, `@phosphor-icons/web@2.1.2`, bold weight, unmodified | MIT |
| `art/queen.svg` | Ant and Crown from [Microsoft Fluent Emoji](https://github.com/microsoft/fluentui-emoji) (Flat), composed on a honey hexagon drawn for Myrstack | MIT (Fluent parts); hexagon: Myrstack |
| `art/ant_cell.svg` | Ant from Fluent Emoji (Flat) on a comb cell drawn for Myrstack | MIT (Fluent parts); cell: Myrstack |
| `art/anthill.svg` | Ants from Fluent Emoji (Flat) on an anthill drawn for Myrstack | MIT (Fluent parts); mound: Myrstack |
| `art/farm/ant_grey.png`, `art/farm/ant_red.png` | ["16x16 Ant Enemies"](https://opengameart.org/content/16x16-ant-enemies) by ARoachIFoundOnMyPillow, grey and red sprite sheets, unmodified | CC0 |
| `art/farm/emote_*.png` | [Emotes Pack](https://opengameart.org/content/emotes-pack) by Kenney (kenney.nl), pixel style 1, unmodified | CC0 |
| `art/farm/crown.png`, `art/farm/coin.png`, `art/farm/coin_pile.png` | [Gold treasure icons 16x16](https://opengameart.org/node/60941) by Bonsaiheldin (bonsaiheld.org), icons 9, 2 and 3, unmodified | CC0 |

Both licenses are permissive and allow commercial use and modification; they ask that the
copyright notice and license text travel with copies, which this file does. The ant farm's
pixel art (`art/farm`) is CC0 (public domain): no notice is required, the credits above are
a courtesy. Its soil, grass and tunnel tiles are generated in `src/ui/ant_farm/sprites.rs`.

To add an icon, pick it at phosphoricons.com and add its codepoint (from the package's
`src/bold/style.css`) to `src/ui/icons.rs`: the whole bold font is bundled.

## Palettes (`palettes/`)

Colour palettes from [Lospec](https://lospec.com/palette-list), unmodified JSON, used by the
dwarf lab's pixel experiment (`examples/dwarf_lab.rs`). A palette is a list of colours, not
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

## Fluent Emoji

```
MIT License

Copyright (c) Microsoft Corporation.

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
SOFTWARE
```

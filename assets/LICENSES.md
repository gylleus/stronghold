# Bundled assets: sources and licenses

Everything here is compiled into the binary (`include_bytes!`); nothing is fetched at runtime.

| File | Source | License |
| --- | --- | --- |
| `fonts/Phosphor-Bold.ttf` | [Phosphor Icons](https://phosphoricons.com) 2.1, `@phosphor-icons/web@2.1.2`, bold weight, unmodified | MIT |
| `art/queen.svg` | Ant and Crown from [Microsoft Fluent Emoji](https://github.com/microsoft/fluentui-emoji) (Flat), composed on a honey hexagon drawn for Myrstack | MIT (Fluent parts); hexagon: Myrstack |
| `art/ant_cell.svg` | Ant from Fluent Emoji (Flat) on a comb cell drawn for Myrstack | MIT (Fluent parts); cell: Myrstack |
| `art/anthill.svg` | Ants from Fluent Emoji (Flat) on an anthill drawn for Myrstack | MIT (Fluent parts); mound: Myrstack |

Both licenses are permissive and allow commercial use and modification; they ask that the
copyright notice and license text travel with copies, which this file does.

To add an icon, pick it at phosphoricons.com and add its codepoint (from the package's
`src/bold/style.css`) to `src/ui/icons.rs`: the whole bold font is bundled.

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

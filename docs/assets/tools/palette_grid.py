"""Quantize one unpaletted 48 px sprite to many palettes and lay them out in a grid."""
import json, sys
from pathlib import Path
import numpy as np
from PIL import Image, ImageDraw
from sprite_pipeline.palette import map_to_palette

src, pal_dir, out = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
order = sys.argv[4].split(",")
im = Image.open(src).convert("RGBA")
cells = [("no palette", im)]
for name in order:
    hexes = json.load(open(pal_dir / f"{name}.json"))["colors"]
    allowed = np.array([[int(h.lstrip("#")[i:i+2], 16) for i in (0, 2, 4)] for h in hexes], dtype=np.uint8)
    cells.append((f"{name} ({len(hexes)})", map_to_palette(im, allowed).convert("RGBA")))
scale, cols = 5, 4
w, h = im.width * scale, im.height * scale
rows = (len(cells) + cols - 1) // cols
grid = Image.new("RGBA", (cols * (w + 12), rows * (h + 26)), (28, 28, 34, 255))
d = ImageDraw.Draw(grid)
for i, (label, c) in enumerate(cells):
    x, y = (i % cols) * (w + 12), (i // cols) * (h + 26)
    bg = Image.new("RGBA", c.size, (70, 66, 62, 255)); bg.alpha_composite(c)
    grid.paste(bg.resize((w, h), Image.NEAREST), (x + 6, y + 20))
    d.text((x + 6, y + 4), label, fill=(230, 230, 230, 255))
grid.save(out)
print(out, len(np.unique(np.asarray(im)[..., :3][np.asarray(im)[..., 3] > 0].reshape(-1, 3), axis=0)), "unique colours in source")

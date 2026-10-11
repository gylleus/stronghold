"""Prototype: key out the grey, erode the edge, crop all poses with one shared box, area-average to size."""
import sys, json
import numpy as np
from PIL import Image, ImageFilter
src, cols, rows, size, out = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
im = Image.open(src).convert('RGB'); a = np.asarray(im).astype(int)
key = np.array([int(v) for v in (sys.argv[6] if len(sys.argv) > 6 else "190,190,190").split(",")])
alpha = (np.abs(a - key).max(axis=2) > 18).astype(np.uint8) * 255
mask = Image.fromarray(alpha).filter(ImageFilter.MinFilter(5))       # erode ~2 px: drop the grey fringe
mask = mask.filter(ImageFilter.MaxFilter(3)).filter(ImageFilter.MinFilter(3))  # close pinholes
w, h = im.size; cw, ch = w // cols, h // rows
cells = []
for r in range(rows):
    for c in range(cols):
        box = (c * cw, r * ch, c * cw + cw, r * ch + ch)
        rgba = im.crop(box).convert('RGBA'); rgba.putalpha(mask.crop(box)); cells.append(rgba)
# one shared crop over all poses: union bbox, squared, with a small margin, bottom-anchored
bbs = [c.getchannel('A').getbbox() for c in cells]
x0 = min(b[0] for b in bbs); y0 = min(b[1] for b in bbs); x1 = max(b[2] for b in bbs); y1 = max(b[3] for b in bbs)
side = int(max(x1 - x0, y1 - y0) * 1.06); cx = (x0 + x1) // 2
crop = (cx - side // 2, y1 - side, cx - side // 2 + side, y1)
frames = []
for c in cells:
    f = c.crop(crop)
    arr = np.asarray(f).astype(float); al = arr[..., 3:4] / 255
    pre = Image.fromarray(np.concatenate([arr[..., :3] * al, arr[..., 3:4]], 2).astype(np.uint8), 'RGBA')
    small = np.asarray(pre.resize((size, size), Image.BOX)).astype(float)
    al2 = small[..., 3:4] / 255
    rgb = np.where(al2 > 0, small[..., :3] / np.maximum(al2, 1e-6), 0)
    outa = (small[..., 3] >= 128).astype(np.uint8) * 255
    frames.append(Image.fromarray(np.dstack([np.clip(rgb, 0, 255).astype(np.uint8), outa]), 'RGBA'))
sheet = Image.new('RGBA', (size * len(frames), size))
for i, f in enumerate(frames): sheet.paste(f, (i * size, 0))
sheet.save(out); print(out, len(frames), 'frames, crop', crop)

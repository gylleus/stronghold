#!/usr/bin/env python3
"""Renders Myrstack's app icon and builds packaging/Myrstack.icns with iconutil.

A golden hexagon (the Queen) with a crown, linked to three ants (the fleet) on a dark rounded
square. Pure Python, no dependencies: the 1024 px master is rendered with 3x3 supersampling
and box-filtered down for the smaller iconset sizes.

    scripts/make_icon.py [--out packaging/Myrstack.icns] [--png-dir DIR]
"""

import argparse
import math
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib

MASTER = 1024
SUPERSAMPLE = 3

# Colors (linear-ish sRGB, 0..1).
BG_TOP = (0.13, 0.15, 0.21)
BG_BOTTOM = (0.05, 0.06, 0.09)
GOLD_LIGHT = (1.00, 0.84, 0.36)
GOLD_DARK = (0.86, 0.56, 0.10)
GOLD_EDGE = (0.55, 0.33, 0.04)
EDGE = (0.95, 0.72, 0.28)
ANT = (0.93, 0.66, 0.24)
CROWN = (0.25, 0.16, 0.04)

# Geometry in unit coordinates (0..1, y down), on Apple's icon grid: the rounded square
# spans 100..924 of 1024 with a ~185 px corner radius.
SQ_MIN, SQ_MAX, SQ_R = 100 / 1024, 924 / 1024, 185 / 1024
CENTER = (0.5, 0.5)
HEX_R = 0.215
HEX_INNER_R = 0.175


def hexagon(cx, cy, r):
    """Pointy-top hexagon vertices."""
    return [
        (cx + r * math.cos(math.radians(90 + 60 * i)), cy + r * math.sin(math.radians(90 + 60 * i)))
        for i in range(6)
    ]


def inside_convex(poly, x, y):
    sign = 0
    n = len(poly)
    for i in range(n):
        x0, y0 = poly[i]
        x1, y1 = poly[(i + 1) % n]
        cross = (x1 - x0) * (y - y0) - (y1 - y0) * (x - x0)
        if cross != 0:
            s = 1 if cross > 0 else -1
            if sign == 0:
                sign = s
            elif s != sign:
                return False
    return True


def inside_poly(poly, x, y):
    """Even-odd point in polygon."""
    inside = False
    n = len(poly)
    j = n - 1
    for i in range(n):
        xi, yi = poly[i]
        xj, yj = poly[j]
        if (yi > y) != (yj > y) and x < (xj - xi) * (y - yi) / (yj - yi) + xi:
            inside = not inside
        j = i
    return inside


def seg_dist(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    qx, qy = ax + t * dx - px, ay + t * dy - py
    return math.hypot(qx, qy)


def in_rounded_square(x, y):
    if not (SQ_MIN <= x <= SQ_MAX and SQ_MIN <= y <= SQ_MAX):
        return False
    cx = min(max(x, SQ_MIN + SQ_R), SQ_MAX - SQ_R)
    cy = min(max(y, SQ_MIN + SQ_R), SQ_MAX - SQ_R)
    return (x - cx) ** 2 + (y - cy) ** 2 <= SQ_R**2


def mix(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


HEX = hexagon(*CENTER, HEX_R)
HEX_INNER = hexagon(*CENTER, HEX_INNER_R)

# Crown on the hexagon: a band with three points.
_cx, _cy = CENTER
CROWN_POLY = [
    (_cx - 0.100, _cy + 0.060),
    (_cx - 0.100, _cy - 0.035),
    (_cx - 0.055, _cy + 0.005),
    (_cx, _cy - 0.070),
    (_cx + 0.055, _cy + 0.005),
    (_cx + 0.100, _cy - 0.035),
    (_cx + 0.100, _cy + 0.060),
]
CROWN_JEWELS = [(_cx - 0.100, _cy - 0.048), (_cx, _cy - 0.084), (_cx + 0.100, _cy - 0.048)]
JEWEL_R = 0.017


def ant(angle_deg, dist):
    """An ant on a ray from the centre, head towards the hexagon, and the edge linking it."""
    a = math.radians(angle_deg)
    ux, uy = math.cos(a), math.sin(a)
    px, py = -uy, ux
    head, thorax, abdomen = dist - 0.036, dist, dist + 0.048
    circles = [
        (_cx + ux * head, _cy + uy * head, 0.019),
        (_cx + ux * thorax, _cy + uy * thorax, 0.017),
        (_cx + ux * abdomen, _cy + uy * abdomen, 0.030),
    ]
    tx, ty = _cx + ux * thorax, _cy + uy * thorax
    legs = []
    for tilt in (-40, 0, 40):
        t = math.radians(tilt)
        for s in (-1, 1):
            # Perpendicular to the body, tilted forwards or backwards, with a bent tip.
            dx = s * (px * math.cos(t)) + ux * math.sin(t)
            dy = s * (py * math.cos(t)) + uy * math.sin(t)
            kx, ky = tx + dx * 0.036, ty + dy * 0.036
            legs.append((tx, ty, kx, ky))
            bend = 0.016 if tilt >= 0 else -0.016
            legs.append((kx, ky, kx + dx * 0.012 + ux * bend, ky + dy * 0.012 + uy * bend))
    antenna = []
    for s in (-1, 1):
        hx, hy = _cx + ux * head, _cy + uy * head
        antenna.append((hx, hy, hx - ux * 0.030 + px * s * 0.022, hy - uy * 0.030 + py * s * 0.022))
    edge = (
        _cx + ux * (HEX_R - 0.01),
        _cy + uy * (HEX_R - 0.01),
        _cx + ux * (head - 0.045),
        _cy + uy * (head - 0.045),
    )
    return circles, legs + antenna, edge


ANTS = [ant(a, 0.315) for a in (-90, 30, 150)]
EDGE_W = 0.011
LEG_W = 0.0048


def shade(x, y):
    """Color and alpha of one sample."""
    if not in_rounded_square(x, y):
        return (0.0, 0.0, 0.0), 0.0
    t = (y - SQ_MIN) / (SQ_MAX - SQ_MIN)
    color = mix(BG_TOP, BG_BOTTOM, t)
    # Soft golden glow behind the queen.
    glow = max(0.0, 1.0 - math.hypot(x - _cx, y - _cy) / 0.42)
    color = mix(color, (0.35, 0.25, 0.08), glow * glow * 0.45)
    for circles, legs, edge in ANTS:
        if seg_dist(x, y, *edge) <= EDGE_W:
            color = EDGE
        for leg in legs:
            if seg_dist(x, y, *leg) <= LEG_W:
                color = ANT
        for cx, cy, r in circles:
            if (x - cx) ** 2 + (y - cy) ** 2 <= r * r:
                color = ANT
    if inside_convex(HEX, x, y):
        if inside_convex(HEX_INNER, x, y):
            g = (y - (_cy - HEX_INNER_R)) / (2 * HEX_INNER_R)
            color = mix(GOLD_LIGHT, GOLD_DARK, max(0.0, min(1.0, g)))
        else:
            color = GOLD_EDGE
        if inside_poly(CROWN_POLY, x, y):
            color = CROWN
        for jx, jy in CROWN_JEWELS:
            if (x - jx) ** 2 + (y - jy) ** 2 <= JEWEL_R**2:
                color = CROWN
    return color, 1.0


def render(size, ss):
    """RGBA rows as floats (premultiplied), `size` x `size`."""
    rows = []
    n = ss * ss
    for py in range(size):
        row = []
        for px in range(size):
            r = g = b = a = 0.0
            for sy in range(ss):
                y = (py + (sy + 0.5) / ss) / size
                for sx in range(ss):
                    x = (px + (sx + 0.5) / ss) / size
                    (cr, cg, cb), ca = shade(x, y)
                    r += cr * ca
                    g += cg * ca
                    b += cb * ca
                    a += ca
            row.append((r / n, g / n, b / n, a / n))
        rows.append(row)
    return rows


def downsample(rows, size):
    """Box-filters a square premultiplied image to `size` (an exact divisor)."""
    factor = len(rows) // size
    n = factor * factor
    out = []
    for oy in range(size):
        row = []
        for ox in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]
            for y in range(oy * factor, (oy + 1) * factor):
                src = rows[y]
                for x in range(ox * factor, (ox + 1) * factor):
                    p = src[x]
                    acc[0] += p[0]
                    acc[1] += p[1]
                    acc[2] += p[2]
                    acc[3] += p[3]
            row.append(tuple(c / n for c in acc))
        out.append(row)
    return out


def png_bytes(rows):
    """Encodes premultiplied float RGBA rows as an 8-bit straight-alpha PNG."""
    height, width = len(rows), len(rows[0])
    raw = bytearray()
    for row in rows:
        raw.append(0)
        for r, g, b, a in row:
            if a > 0:
                r, g, b = r / a, g / a, b / a
            raw.extend(round(max(0.0, min(1.0, c)) * 255) for c in (r, g, b, a))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


# iconutil's iconset: (file name, pixel size).
ICONSET = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
]


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", default=os.path.join(root, "packaging", "Myrstack.icns"))
    parser.add_argument("--png-dir", help="also keep the rendered PNGs here")
    args = parser.parse_args()

    print(f"rendering {MASTER}px master…", file=sys.stderr)
    master = render(MASTER, SUPERSAMPLE)
    images = {MASTER: master}
    for _, size in ICONSET:
        if size not in images:
            images[size] = downsample(master, size)

    with tempfile.TemporaryDirectory() as tmp:
        iconset = os.path.join(tmp, "Myrstack.iconset")
        os.mkdir(iconset)
        for name, size in ICONSET:
            with open(os.path.join(iconset, name), "wb") as f:
                f.write(png_bytes(images[size]))
        if args.png_dir:
            os.makedirs(args.png_dir, exist_ok=True)
            for name, _ in ICONSET:
                shutil.copy(os.path.join(iconset, name), args.png_dir)
        os.makedirs(os.path.dirname(args.out), exist_ok=True)
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", args.out], check=True)
    print(f"wrote {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()

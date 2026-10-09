#!/usr/bin/env python3
"""Draws the SaveSync icons with no dependencies (pure Python + zlib).

    python3 scripts/gen-icons.py

Writes desktop/icons/: app icons (PNG sizes, icon.icns via macOS iconutil,
icon.ico) and tray icons (a monochrome macOS template image and a colored one).
The design: a memory card with a circular sync arrow, on a teal-to-indigo tile.
"""
import math
import os
import struct
import subprocess
import tempfile
import zlib

OUT = os.path.join(os.path.dirname(__file__), "..", "desktop", "icons")

TEAL = (20, 184, 166)
INDIGO = (79, 70, 229)
WHITE = (255, 255, 255)


# ---------------------------------------------------------------- signed distance fields
# Coordinates are in a unit square: (0,0) top-left, (1,1) bottom-right.
# Negative distance = inside.

def sd_round_rect(x, y, cx, cy, hw, hh, r):
    qx = abs(x - cx) - hw + r
    qy = abs(y - cy) - hh + r
    outside = math.hypot(max(qx, 0.0), max(qy, 0.0))
    return outside + min(max(qx, qy), 0.0) - r


def sd_card(x, y):
    """A memory card: rounded rectangle with the top-right corner cut off."""
    d = sd_round_rect(x, y, 0.5, 0.52, 0.25, 0.31, 0.045)
    # Half-plane through the corner cut: x + y > c is outside.
    cut = ((x - 0.5) + (0.21 - y)) - 0.185
    return max(d, cut / math.sqrt(2))


def sd_pins(x, y):
    d = 1e9
    for i in range(4):
        cx = 0.355 + i * 0.075
        d = min(d, sd_round_rect(x, y, cx, 0.30, 0.022, 0.055, 0.012))
    return d


def sd_triangle(x, y, a, b, c):
    """Distance to a triangle (a, b, c are (x, y) tuples)."""
    def edge(p, q):
        ex, ey = q[0] - p[0], q[1] - p[1]
        wx, wy = x - p[0], y - p[1]
        t = max(0.0, min(1.0, (wx * ex + wy * ey) / (ex * ex + ey * ey)))
        dx, dy = wx - ex * t, wy - ey * t
        return dx * dx + dy * dy, ex * wy - ey * wx
    d1, s1 = edge(a, b)
    d2, s2 = edge(b, c)
    d3, s3 = edge(c, a)
    d = math.sqrt(min(d1, d2, d3))
    inside = (s1 >= 0 and s2 >= 0 and s3 >= 0) or (s1 <= 0 and s2 <= 0 and s3 <= 0)
    return -d if inside else d


def sd_sync(x, y):
    """Two arcs chasing each other around a circle, each ending in an arrowhead."""
    cx, cy, r, w = 0.5, 0.62, 0.135, 0.032
    ang = math.atan2(y - cy, x - cx)
    ring = abs(math.hypot(x - cx, y - cy) - r) - w / 2
    d = 1e9
    for start in (math.radians(-60), math.radians(120)):
        span = math.radians(130)
        # Inside the arc's angular span?
        rel = (ang - start) % (2 * math.pi)
        if rel <= span:
            d = min(d, ring)
        else:
            # Round caps at the arc ends.
            for a in (start, start + span):
                ex, ey = cx + r * math.cos(a), cy + r * math.sin(a)
                d = min(d, math.hypot(x - ex, y - ey) - w / 2)
        # Arrowhead at the arc's leading end, pointing along the circle.
        a = start + span
        tip_a = a + math.radians(22)
        tip = (cx + r * math.cos(tip_a), cy + r * math.sin(tip_a))
        base = (cx + r * math.cos(a), cy + r * math.sin(a))
        nx, ny = math.cos(a), math.sin(a)
        s = 0.052
        b1 = (base[0] + nx * s, base[1] + ny * s)
        b2 = (base[0] - nx * s, base[1] - ny * s)
        d = min(d, sd_triangle(x, y, tip, b1, b2))
    return d


# ---------------------------------------------------------------- rendering

def coverage(d, px):
    """Anti-aliased coverage from a signed distance, in pixels."""
    return max(0.0, min(1.0, 0.5 - d / px))


def blend(dst, src, a):
    return tuple(dst[i] * (1 - a) + src[i] * a for i in range(3))


def render_app(size):
    px = 1.0 / size
    rows = []
    for j in range(size):
        y = (j + 0.5) * px
        row = bytearray()
        for i in range(size):
            x = (i + 0.5) * px
            tile = coverage(sd_round_rect(x, y, 0.5, 0.5, 0.42, 0.42, 0.1), px)
            if tile <= 0:
                row += b"\x00\x00\x00\x00"
                continue
            t = max(0.0, min(1.0, (x + y) / 2))
            bg = tuple(TEAL[k] * (1 - t) + INDIGO[k] * t for k in range(3))
            card = coverage(sd_card(x, y), px)
            color = blend(bg, WHITE, card)
            cut = coverage(min(sd_pins(x, y), sd_sync(x, y)), px) * card
            color = blend(color, bg, cut)
            row += bytes(int(round(c)) for c in color) + bytes([int(round(255 * tile))])
        rows.append(bytes(row))
    return rows


def render_tray(size, rgb):
    """The card + arrows silhouette only (for the menu bar / system tray)."""
    px = 1.0 / size
    rows = []
    for j in range(size):
        y = (j + 0.5) * px
        row = bytearray()
        for i in range(size):
            x = (i + 0.5) * px
            # Scale the artwork up a bit: the tray has no tile around it.
            sx, sy = 0.5 + (x - 0.5) * 0.78, 0.5 + (y - 0.5) * 0.78 + 0.03
            card = coverage(sd_card(sx, sy) / 0.78, px)
            holes = coverage(min(sd_pins(sx, sy), sd_sync(sx, sy)) / 0.78, px)
            a = card * (1 - holes)
            row += bytes(rgb) + bytes([int(round(255 * a))])
        rows.append(bytes(row))
    return rows


def png_bytes(rows):
    h, w = len(rows), len(rows[0]) // 4
    raw = b"".join(b"\x00" + r for r in rows)

    def chunk(kind, data):
        c = kind + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def write(path, data):
    with open(path, "wb") as f:
        f.write(data)


def ico_bytes(pngs):
    """An .ico holding PNG images (supported since Windows Vista)."""
    header = struct.pack("<HHH", 0, 1, len(pngs))
    offset = 6 + 16 * len(pngs)
    entries, blobs = b"", b""
    for size, data in pngs:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        blobs += data
    return header + entries + blobs


def main():
    os.makedirs(OUT, exist_ok=True)
    master = png_bytes(render_app(1024))
    write(os.path.join(OUT, "icon.png"), master)

    def scaled(size, name):
        path = os.path.join(OUT, name)
        subprocess.run(["sips", "-z", str(size), str(size), os.path.join(OUT, "icon.png"), "--out", path],
                       check=True, capture_output=True)
        with open(path, "rb") as f:
            return f.read()

    scaled(32, "32x32.png")
    scaled(128, "128x128.png")
    scaled(256, "128x128@2x.png")

    with tempfile.TemporaryDirectory() as tmp:
        iconset = os.path.join(tmp, "icon.iconset")
        os.makedirs(iconset)
        for base in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                s = base * scale
                name = f"icon_{base}x{base}{'@2x' if scale == 2 else ''}.png"
                subprocess.run(["sips", "-z", str(s), str(s), os.path.join(OUT, "icon.png"),
                                "--out", os.path.join(iconset, name)], check=True, capture_output=True)
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(OUT, "icon.icns")], check=True)

    ico = [(s, scaled(s, f"_ico{s}.png")) for s in (16, 24, 32, 48, 64, 256)]
    write(os.path.join(OUT, "icon.ico"), ico_bytes(ico))
    for s, _ in ico:
        os.remove(os.path.join(OUT, f"_ico{s}.png"))

    # Tray: black template for the macOS menu bar (the OS recolors it), white-ish for Windows.
    write(os.path.join(OUT, "tray-template.png"), png_bytes(render_tray(44, (0, 0, 0))))
    write(os.path.join(OUT, "tray.png"), png_bytes(render_tray(64, (236, 240, 243))))
    print("icons written to", os.path.normpath(OUT))


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Generate assets/pound.ico: a rounded dark square with a white '#' mark.

Pure standard-library (no PIL): builds an RGBA pixel buffer, encodes it as a
PNG with zlib, and wraps it in an ICO container (PNG-embedded entries are
supported by Windows Vista+).
"""
import struct
import zlib
from pathlib import Path

SIZE = 256
BG = (32, 34, 43, 255)        # dark slate
BG_TOP = (44, 48, 62, 255)    # subtle vertical gradient
FG = (235, 239, 245, 255)     # near-white for the '#'


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3)) + (255,)


def make_pixels():
    px = [[None] * SIZE for _ in range(SIZE)]
    radius = 52
    inset = 12  # leave a little transparent padding around the rounded square

    def inside_rounded_rect(x, y):
        x0, y0, x1, y1 = inset, inset, SIZE - inset, SIZE - inset
        if not (x0 + radius <= x <= x1 - radius):
            if not (x0 <= x < x1 and y0 <= y < y1):
                return False
        # corner check
        for cx, cy in ((x0 + radius, y0 + radius), (x1 - radius, y0 + radius),
                       (x0 + radius, y1 - radius), (x1 - radius, y1 - radius)):
            pass
        # simple rounded-rect test
        if x0 <= x <= x1 and y0 <= y <= y1:
            if x < x0 + radius and y < y0 + radius:
                return (x - (x0 + radius)) ** 2 + (y - (y0 + radius)) ** 2 <= radius**2
            if x > x1 - radius and y < y0 + radius:
                return (x - (x1 - radius)) ** 2 + (y - (y0 + radius)) ** 2 <= radius**2
            if x < x0 + radius and y > y1 - radius:
                return (x - (x0 + radius)) ** 2 + (y - (y1 - radius)) ** 2 <= radius**2
            if x > x1 - radius and y > y1 - radius:
                return (x - (x1 - radius)) ** 2 + (y - (y1 - radius)) ** 2 <= radius**2
            return True
        return False

    # '#' geometry: two vertical stems + two horizontal bars, slightly slanted
    # for a friendlier look.
    def on_mark(x, y):
        t = (x - 60) / (SIZE - 120)  # 0..1 across the glyph box
        slant = int(10 * t)          # bars drift right going down
        v_w = 26                      # stem thickness
        h_w = 24                      # bar thickness
        v1 = 84 + slant // 2
        v2 = 148 + slant // 2
        h1, h2 = 96, 140
        if 64 <= y <= 196:
            if v1 <= x <= v1 + v_w or v2 <= x <= v2 + v_w:
                return True
        if (h1 <= y <= h1 + h_w or h2 <= y <= h2 + h_w) and 60 <= x <= 196:
            return True
        return False

    for y in range(SIZE):
        row_t = y / SIZE
        base = lerp(BG_TOP, BG, row_t)
        for x in range(SIZE):
            if not inside_rounded_rect(x, y):
                px[y][x] = (0, 0, 0, 0)
            elif on_mark(x, y):
                px[y][x] = FG
            else:
                px[y][x] = base
    return px


def encode_png(px):
    raw = bytearray()
    for row in px:
        raw.append(0)  # filter: none
        for r, g, b, a in row:
            raw += bytes((r, g, b, a))

    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def wrap_ico(png):
    # ICONDIR + one ICONDIRENTRY + raw PNG
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack(
        "<BBBBHHII",
        0, 0, 0, 0,   # width/height 0 means 256, colors, reserved
        1, 32,        # planes, bpp
        len(png), 22,  # size, offset (right after header+entry)
    )
    return header + entry + png


def main():
    out = Path(__file__).resolve().parent.parent / "assets" / "pound.ico"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(wrap_ico(encode_png(make_pixels())))
    print(f"wrote {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()

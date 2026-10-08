#!/usr/bin/env python3
"""Generate the Pound logo & icon set (stdlib only, deterministic).

Design: "the split-view hash". A rounded-square app tile carries a bold,
rounded hash mark whose center cell is filled with the accent blue — the #
that names the project, read as the reader's signature split view
(source | rendered).

Outputs
  assets/pound.ico                  multi-size Windows icon (16..256)
  assets/pound.png                  256px window icon (RGBA)
  assets/logo/pound-icon.svg        vector icon
  assets/logo/pound-icon-<n>.png    512 / 256 / 128 / 64 / 32
  assets/logo/pound-mark.svg        transparent mark (strokes use currentColor)
  assets/logo/pound-mark-256.png    transparent mark, dark strokes for light bg

Prints an ASCII preview so the geometry can be checked from a terminal.
"""
import pathlib
import struct
import zlib

# ---------------------------------------------------------------- design ---
SIZE = 256          # master canvas
SS = 4              # supersampling factor (render at 1024, box-downsample)

BG_TOP = (58, 66, 88)      # #3A4258
BG_BOTTOM = (34, 39, 54)   # #222736
BG_INSET = 8
BG_RADIUS = 56
STROKE = (245, 248, 253)   # #F5F8FD
ACCENT = (111, 168, 255)   # #6FA8FF
MARK_DARK = (43, 48, 64)   # #2B3040 (mark-only variant for light backgrounds)

# Hash geometry (256-scale). Two stems, two bars, rounded ends; the cell
# between them carries the accent.
STEM_W = 22
BAR_H = 20
STEM_C1, STEM_C2 = 96, 160     # stem centers (x)
BAR_C1, BAR_C2 = 100, 156      # bar centers (y)
MARK_TOP, MARK_BOTTOM = 62, 194
MARK_LEFT, MARK_RIGHT = 62, 194
CELL = (107, 110, 149, 146)    # x0, y0, x1, y1 — the center cell

ICON_SIZES = [16, 24, 32, 48, 64, 256]
LOGO_SIZES = [512, 256, 128, 64, 32]

# ------------------------------------------------------------ rasterizing ---


def inside_rounded_rect(px, py, x0, y0, x1, y1, radius):
    """Signed-box test for a rounded rectangle at supersample coordinates."""
    dx = max(x0 + radius - px, px - (x1 - radius), 0)
    dy = max(y0 + radius - py, py - (y1 - radius), 0)
    return dx * dx + dy * dy <= radius * radius


def draw_rounded_rect(buf, x0, y0, x1, y1, radius, color):
    """Paint a solid rounded rect (SS coordinates) with painter's-algorithm."""
    s = SS
    for y in range(int(y0 * s), int(y1 * s)):
        row = buf[y]
        for x in range(int(x0 * s), int(x1 * s)):
            if inside_rounded_rect(x + 0.5, y + 0.5, x0 * s, y0 * s, x1 * s, y1 * s, radius * s):
                row[x] = (*color, 255)


def render(with_background, stroke_color):
    """Render the design into an SS×SS premultiplied-ready RGBA buffer."""
    s = SIZE * SS
    buf = [[(0, 0, 0, 0)] * s for _ in range(s)]

    if with_background:
        # Vertical gradient, masked by the rounded tile shape.
        for y in range(s):
            t = y / (s - 1)
            r = int(BG_TOP[0] + (BG_BOTTOM[0] - BG_TOP[0]) * t)
            g = int(BG_TOP[1] + (BG_BOTTOM[1] - BG_TOP[1]) * t)
            b = int(BG_TOP[2] + (BG_BOTTOM[2] - BG_TOP[2]) * t)
            row = buf[y]
            for x in range(s):
                if inside_rounded_rect(
                    x + 0.5, y + 0.5,
                    BG_INSET * SS, BG_INSET * SS,
                    (SIZE - BG_INSET) * SS, (SIZE - BG_INSET) * SS,
                    BG_RADIUS * SS,
                ):
                    row[x] = (r, g, b, 255)

    # The accent "active pane", then the hash strokes over it.
    cx0, cy0, cx1, cy1 = CELL
    draw_rounded_rect(buf, cx0, cy0, cx1, cy1, 5, ACCENT)

    for cx in (STEM_C1, STEM_C2):
        draw_rounded_rect(buf, cx - STEM_W / 2, MARK_TOP, cx + STEM_W / 2, MARK_BOTTOM, STEM_W / 2, stroke_color)
    for cy in (BAR_C1, BAR_C2):
        draw_rounded_rect(buf, MARK_LEFT, cy - BAR_H / 2, MARK_RIGHT, cy + BAR_H / 2, BAR_H / 2, stroke_color)

    return buf


def resize(buf, out_size):
    """Box-filter downsample of the SS buffer to out_size RGBA bytes."""
    s = len(buf)
    scale = s / out_size
    out = bytearray(out_size * out_size * 4)
    for oy in range(out_size):
        y0, y1 = int(oy * scale), max(int((oy + 1) * scale), int(oy * scale) + 1)
        for ox in range(out_size):
            x0, x1 = int(ox * scale), max(int((ox + 1) * scale), int(ox * scale) + 1)
            r = g = b = a = 0
            count = 0
            for y in range(y0, y1):
                row = buf[y]
                for x in range(x0, x1):
                    pr, pg, pb, pa = row[x]
                    r += pr * pa
                    g += pg * pa
                    b += pb * pa
                    a += pa
                    count += 1
            if a:
                base = (oy * out_size + ox) * 4
                # Straight-color output: color = Σ(pr·pa) / Σ(pa), weighted by
                # coverage; alpha = average coverage.
                out[base] = round(r / a)
                out[base + 1] = round(g / a)
                out[base + 2] = round(b / a)
                out[base + 3] = round(a / count)
    return out


# ----------------------------------------------------------------- encoding ---


def encode_png(rgba, size):
    """Encode raw RGBA bytes as a PNG (filter 0 rows, zlib deflate)."""
    raw = bytearray()
    stride = size * 4
    for y in range(size):
        raw.append(0)
        raw += rgba[y * stride : (y + 1) * stride]

    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def encode_ico(pngs):
    """ICO container with PNG-compressed entries ((size, png) pairs)."""
    count = len(pngs)
    header = struct.pack("<HHH", 0, 1, count)
    entries = b""
    offset = 6 + 16 * count
    blobs = b""
    for size, png in pngs:
        entries += struct.pack(
            "<BBBBHHII",
            0 if size >= 256 else size,
            0 if size >= 256 else size,
            0, 0, 1, 32,
            len(png), offset,
        )
        blobs += png
        offset += len(png)
    return header + entries + blobs


def ascii_preview(rgba, size):
    """Print the design so the geometry can be checked from a terminal."""
    step = size // 64
    for y in range(0, size, step * 2):
        row = ""
        for x in range(0, size, step):
            r, g, b, a = rgba[(y * size + x) * 4 : (y * size + x) * 4 + 4]
            if a < 128:
                row += " "
            elif b - r > 60:   # accent blue
                row += "@"
            elif r > 190:      # white strokes
                row += "#"
            else:              # tile background
                row += "."
        print(row)


# ---------------------------------------------------------------------- SVG ---


def icon_svg():
    return f"""<svg width="256" height="256" viewBox="0 0 256 256" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="Pound">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#3A4258"/>
      <stop offset="1" stop-color="#222736"/>
    </linearGradient>
  </defs>
  <rect x="{BG_INSET}" y="{BG_INSET}" width="{SIZE - 2 * BG_INSET}" height="{SIZE - 2 * BG_INSET}" rx="{BG_RADIUS}" fill="url(#bg)"/>
  <rect x="{CELL[0]}" y="{CELL[1]}" width="{CELL[2] - CELL[0]}" height="{CELL[3] - CELL[1]}" rx="5" fill="#6FA8FF"/>
  <g fill="#F5F8FD">
    <rect x="{STEM_C1 - STEM_W // 2}" y="{MARK_TOP}" width="{STEM_W}" height="{MARK_BOTTOM - MARK_TOP}" rx="{STEM_W // 2}"/>
    <rect x="{STEM_C2 - STEM_W // 2}" y="{MARK_TOP}" width="{STEM_W}" height="{MARK_BOTTOM - MARK_TOP}" rx="{STEM_W // 2}"/>
    <rect x="{MARK_LEFT}" y="{BAR_C1 - BAR_H // 2}" width="{MARK_RIGHT - MARK_LEFT}" height="{BAR_H}" rx="{BAR_H // 2}"/>
    <rect x="{MARK_LEFT}" y="{BAR_C2 - BAR_H // 2}" width="{MARK_RIGHT - MARK_LEFT}" height="{BAR_H}" rx="{BAR_H // 2}"/>
  </g>
</svg>
"""


def mark_svg():
    return f"""<svg width="256" height="256" viewBox="0 0 256 256" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="Pound mark">
  <!-- strokes use currentColor: set CSS `color` to fit the host design -->
  <rect x="{CELL[0]}" y="{CELL[1]}" width="{CELL[2] - CELL[0]}" height="{CELL[3] - CELL[1]}" rx="5" fill="#6FA8FF"/>
  <g fill="currentColor">
    <rect x="{STEM_C1 - STEM_W // 2}" y="{MARK_TOP}" width="{STEM_W}" height="{MARK_BOTTOM - MARK_TOP}" rx="{STEM_W // 2}"/>
    <rect x="{STEM_C2 - STEM_W // 2}" y="{MARK_TOP}" width="{STEM_W}" height="{MARK_BOTTOM - MARK_TOP}" rx="{STEM_W // 2}"/>
    <rect x="{MARK_LEFT}" y="{BAR_C1 - BAR_H // 2}" width="{MARK_RIGHT - MARK_LEFT}" height="{BAR_H}" rx="{BAR_H // 2}"/>
    <rect x="{MARK_LEFT}" y="{BAR_C2 - BAR_H // 2}" width="{MARK_RIGHT - MARK_LEFT}" height="{BAR_H}" rx="{BAR_H // 2}"/>
  </g>
</svg>
"""


# ---------------------------------------------------------------------- main ---


def main():
    root = pathlib.Path(__file__).resolve().parent.parent / "assets"
    logo = root / "logo"
    logo.mkdir(parents=True, exist_ok=True)

    icon_master = render(with_background=True, stroke_color=STROKE)
    mark_master = render(with_background=False, stroke_color=MARK_DARK)

    icon_256 = resize(icon_master, SIZE)
    (root / "pound.png").write_bytes(encode_png(icon_256, SIZE))

    ico = encode_ico([(s, encode_png(resize(icon_master, s), s)) for s in ICON_SIZES])
    (root / "pound.ico").write_bytes(ico)

    (logo / "pound-icon.svg").write_text(icon_svg())
    (logo / "pound-mark.svg").write_text(mark_svg())
    for size in LOGO_SIZES:
        (logo / f"pound-icon-{size}.png").write_bytes(
            encode_png(resize(icon_master, size), size)
        )
    (logo / "pound-mark-256.png").write_bytes(encode_png(resize(mark_master, 256), 256))

    print(f"wrote {root / 'pound.ico'}      (entries: {', '.join(str(s) for s in ICON_SIZES)})")
    print(f"wrote {root / 'pound.png'}")
    for size in LOGO_SIZES:
        print(f"wrote {logo / f'pound-icon-{size}.png'}")
    print(f"wrote {logo / 'pound-mark-256.png'}")
    print(f"wrote {logo / 'pound-icon.svg'}")
    print(f"wrote {logo / 'pound-mark.svg'}")
    print()
    ascii_preview(icon_256, SIZE)


if __name__ == "__main__":
    main()

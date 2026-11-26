#!/usr/bin/env python3
"""Draws the app icon (our own simple design, stdlib only) as a 1024x1024 RGBA PNG,
or as a Windows .ico when OUT ends with .ico.

A rounded blue square with a gold ring around a white diamond. The build
scripts scale the PNG to the platform sizes (sips + iconutil on macOS). The
.ico is drawn directly at each of ICO_SIZES: 32-bit BGRA bitmaps up to 64 px
and a PNG entry at 256 px (the layout Windows Explorer, windres and Inno
Setup all read).

    make-icon.py OUT.png [SIZE]
    make-icon.py OUT.ico
"""

import math
import struct
import sys
import zlib

TOP = (0x1f, 0x46, 0xa8)
BOTTOM = (0x0a, 0x14, 0x40)
GOLD = (0xf4, 0xc4, 0x4e)
WHITE = (0xff, 0xff, 0xff)
SHADE = (0xc8, 0xd8, 0xff)


def clamp01(x):
    return 0.0 if x < 0.0 else 1.0 if x > 1.0 else x


def sd_round_rect(x, y, half, radius):
    qx = abs(x) - half + radius
    qy = abs(y) - half + radius
    outside = math.hypot(max(qx, 0.0), max(qy, 0.0))
    return outside + min(max(qx, qy), 0.0) - radius


def sd_rhombus(x, y, a, b):
    # Signed distance (approximate outside the corners) to |x|/a + |y|/b = 1.
    k = math.hypot(1.0 / a, 1.0 / b)
    return (abs(x) / a + abs(y) / b - 1.0) / k


def over(dst, src, alpha):
    return tuple(d + (s - d) * alpha for d, s in zip(dst, src))


def render(size):
    px = 1.0 / size  # one pixel in unit coordinates
    rows = []
    for j in range(size):
        y = (j + 0.5) / size - 0.5
        row = bytearray([0])  # PNG filter: none
        grad = clamp01((y + 0.4) / 0.8)
        bg = tuple(t + (b - t) * grad for t, b in zip(TOP, BOTTOM))
        for i in range(size):
            x = (i + 0.5) / size - 0.5
            a_bg = clamp01(0.5 - sd_round_rect(x, y, 0.402, 0.09) / px)
            if a_bg <= 0.0:
                row += b"\0\0\0\0"
                continue
            # Soft light from the upper left.
            glow = clamp01(1.0 - math.hypot(x + 0.12, y + 0.18) / 0.55) * 0.18
            c = over(bg, WHITE, glow)
            r = math.hypot(x, y)
            c = over(c, GOLD, clamp01(0.5 - (abs(r - 0.27) - 0.026) / px))
            d = sd_rhombus(x, y + 0.005, 0.135, 0.185)
            if d < px:
                face = WHITE if x < 0 else SHADE
                c = over(c, face, clamp01(0.5 - d / px))
            row += bytes((round(c[0]), round(c[1]), round(c[2]), round(255 * a_bg)))
        rows.append(bytes(row))
    return b"".join(rows)


def png(size, raw):
    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 256)


def dib(size, raw):
    """An ICO bitmap entry: BITMAPINFOHEADER, bottom-up BGRA rows, AND mask."""
    stride = 1 + 4 * size  # rows of render() start with a PNG filter byte
    pixels = bytearray()
    for j in reversed(range(size)):
        row = raw[j * stride + 1:(j + 1) * stride]
        for i in range(0, len(row), 4):
            r, g, b, a = row[i:i + 4]
            pixels += bytes((b, g, r, a))
    mask_row = ((size + 31) // 32) * 4
    mask = bytes(mask_row * size)  # all opaque; the alpha channel decides
    header = struct.pack("<IiiHHIIiiII", 40, size, 2 * size, 1, 32, 0,
                         len(pixels) + len(mask), 0, 0, 0, 0)
    return header + bytes(pixels) + mask


def ico(sizes=ICO_SIZES):
    images = []
    for size in sizes:
        raw = render(size)
        images.append((size, png(size, raw) if size >= 256 else dib(size, raw)))
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for size, data in images:
        dim = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return out + b"".join(data for _, data in images)


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 2
    if sys.argv[1].lower().endswith(".ico"):
        if len(sys.argv) != 2:
            print(__doc__, file=sys.stderr)
            return 2
        data = ico()
    else:
        size = int(sys.argv[2]) if len(sys.argv) == 3 else 1024
        data = png(size, render(size))
    with open(sys.argv[1], "wb") as f:
        f.write(data)
    return 0


if __name__ == "__main__":
    sys.exit(main())

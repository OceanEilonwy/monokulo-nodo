#!/usr/bin/env python3
"""Make the screen's console font from Terminus ter-v32n (16x32, 512 glyphs).

ter-v32n lacks most of what the screen draws: rounded and dashed box corners, heavy lines, half
and eighth blocks, a dot, and the icons on its buttons. This script draws those glyphs to match
Terminus's own line positions (two pixels thick, centred) and puts them in place of Greek and
Cyrillic glyphs the screen never shows. The result keeps every other glyph unchanged.

Terminus is under the SIL Open Font License 1.1, whose Reserved Font Name rule means a modified
font may not be called Terminus: this one is nodo-screen-16x32, shipped with the OFL text.

usage: make-font.py ter-v32n.psf.gz nodo-screen-16x32.psf.gz
"""
import gzip
import math
import struct
import sys

W, H = 16, 32
# Terminus's light lines: rows 15-16 across, columns 7-8 down, so their centres are y 16 and x 8.
HY, VX = 16.0, 8.0


def blank():
    return [[False] * W for _ in range(H)]


def fill(g, test):
    for y in range(H):
        for x in range(W):
            if test(x + 0.5, y + 0.5):
                g[y][x] = True
    return g


def seg_dist(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def lines(*segments, width=2.0):
    return fill(blank(), lambda x, y: any(seg_dist(x, y, *s) <= width / 2 for s in segments))


def rounded(right, down):
    """A light corner whose arms go to the right (or left) edge and the bottom (or top) edge."""
    cx = W if right else 0
    cy = H if down else 0
    rx, ry = abs(cx - VX), abs(cy - HY)

    def test(x, y):
        if (x < VX - 1 if right else x > VX + 1) or (y < HY - 1 if down else y > HY + 1):
            return False
        if (right and x > cx) or (down and y > cy):
            return False
        inner = ((x - cx) / (rx - 1)) ** 2 + ((y - cy) / (ry - 1)) ** 2
        outer = ((x - cx) / (rx + 1)) ** 2 + ((y - cy) / (ry + 1)) ** 2
        return inner >= 1 and outer <= 1

    return fill(blank(), test)


def dashed(horizontal, runs):
    g = blank()
    for a, b in runs:
        for i in range(a, b):
            for j in (0, 1):
                if horizontal:
                    g[15 + j][i] = True
                else:
                    g[i][7 + j] = True
    return g


def heavy(left, right, up, down):
    def test(x, y):
        across = 14 <= y - 0.5 <= 17 and ((left and x <= 10) or (right and x >= 6))
        along = 6 <= x - 0.5 <= 9 and ((up and y <= 18) or (down and y >= 14))
        return across or along

    return fill(blank(), test)


def rows_from_bottom(n):
    return fill(blank(), lambda x, y: y >= H - n)


def circle_ring(cx, cy, r, width, gap_deg=0):
    def test(x, y):
        d = math.hypot(x - cx, y - cy)
        if abs(d - r) > width / 2:
            return False
        angle = math.degrees(math.atan2(x - cx, cy - y))  # 0 = straight up
        return abs(angle) >= gap_deg

    return test


def power():
    ring = circle_ring(8, 17.5, 6, 2, gap_deg=40)
    return fill(blank(), lambda x, y: ring(x, y) or seg_dist(x, y, 8, 7, 8, 16) <= 1)


def triangle(points):
    (ax, ay), (bx, by), (cx, cy) = points

    def side(px, py, x1, y1, x2, y2):
        return (px - x2) * (y1 - y2) - (x1 - x2) * (py - y2)

    def test(x, y):
        d1, d2, d3 = side(x, y, ax, ay, bx, by), side(x, y, bx, by, cx, cy), side(x, y, cx, cy, ax, ay)
        neg = d1 < 0 or d2 < 0 or d3 < 0
        pos = d1 > 0 or d2 > 0 or d3 > 0
        return not (neg and pos)

    return fill(blank(), test)


GLYPHS = {
    "╭": rounded(right=True, down=True),
    "╮": rounded(right=False, down=True),
    "╰": rounded(right=True, down=False),
    "╯": rounded(right=False, down=False),
    "┄": dashed(True, [(1, 4), (6, 10), (12, 15)]),
    "┆": dashed(False, [(2, 8), (13, 19), (24, 30)]),
    "╌": dashed(True, [(2, 6), (10, 14)]),
    "╎": dashed(False, [(4, 12), (20, 28)]),
    "━": heavy(True, True, False, False),
    "┃": heavy(False, False, True, True),
    "┏": heavy(False, True, False, True),
    "┓": heavy(True, False, False, True),
    "┗": heavy(False, True, True, False),
    "┛": heavy(True, False, True, False),
    "▀": fill(blank(), lambda x, y: y < H / 2),
    "▄": rows_from_bottom(16),
    "▁": rows_from_bottom(4),
    "▂": rows_from_bottom(8),
    "▃": rows_from_bottom(12),
    "▅": rows_from_bottom(20),
    "▆": rows_from_bottom(24),
    "▇": rows_from_bottom(28),
    "●": fill(blank(), lambda x, y: math.hypot(x - 8, y - 17) <= 5.5),
    "⇲": lines((3, 9, 11, 20), (11, 20, 11, 14), (11, 20, 5, 20), (2, 25, 15, 25), (14, 6, 14, 25)),
    "⏻": power(),
    "✓": lines((2, 17, 6, 22), (6, 22, 14, 9), width=2.5),
    "☰": lines((2, 10, 14, 10), (2, 16, 14, 16), (2, 22, 14, 22)),
    "▸": triangle([(5, 11), (5, 22), (12, 16.5)]),
}


def main(src, dst):
    data = gzip.open(src).read()
    magic, version, header, flags, count, size, height, width = struct.unpack("<8I", data[:32])
    if magic != 0x864AB572 or (width, height, size) != (W, H, 64) or not flags & 1:
        sys.exit("expected a 16x32 PSF2 font with a Unicode table")
    glyphs = [bytearray(data[header + i * size : header + (i + 1) * size]) for i in range(count)]
    entries = data[header + count * size :].split(b"\xff")[:count]

    def codepoints(entry):
        return [c for seq in entry.split(b"\xfe") for c in seq.decode("utf-8", "ignore")]

    present = {c for e in entries for c in codepoints(e)}
    spare = [i for i, e in enumerate(entries) if codepoints(e) and all(0x370 <= ord(c) <= 0x4FF for c in codepoints(e))]
    wanted = [c for c in GLYPHS if c not in present]
    if len(wanted) > len(spare):
        sys.exit(f"{len(wanted)} glyphs to add but only {len(spare)} spare slots")
    for char, index in zip(wanted, spare):
        bitmap = GLYPHS[char]
        glyphs[index] = bytearray(b"".join(struct.pack(">H", int("".join("1" if p else "0" for p in row), 2)) for row in bitmap))
        entries[index] = char.encode()
    out = data[:header] + b"".join(glyphs) + b"".join(e + b"\xff" for e in entries)
    # mtime 0 keeps the output the same from build to build.
    with open(dst, "wb") as raw, gzip.GzipFile(fileobj=raw, mode="wb", mtime=0) as f:
        f.write(out)
    print(f"added {len(wanted)} glyphs: {''.join(wanted)}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])

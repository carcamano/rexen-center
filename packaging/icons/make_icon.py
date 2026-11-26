#!/usr/bin/env python3
"""Draws the generic ReXen Center application icon (no third-party logo):
a rounded slate square with three white server units and status lights.

Usage: python3 make_icon.py <size> <out.png>

Standard library only (zlib + struct). Each pixel is the average of 4x4
samples, so edges are smooth at any size. The committed PNGs were made with:
    python3 make_icon.py 1024 icon-1024.png
    python3 make_icon.py 256 ../../crates/xenadmin/assets/icon-256.png
"""
import struct
import sys
import zlib

BG_TOP = (0x2F, 0x4B, 0x6E)
BG_BOTTOM = (0x1D, 0x30, 0x4A)
UNIT = (0xF4, 0xF6, 0xF9)
SLOT = (0x9A, 0xA9, 0xBC)
LIGHTS = [(0x3F, 0xC3, 0x6A), (0x3F, 0xC3, 0x6A), (0xF2, 0xA5, 0x2B)]


def in_round_rect(x, y, x0, y0, x1, y1, r):
    if x < x0 or x > x1 or y < y0 or y > y1:
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def shade(u, v):
    """Colour at unit coordinates (u, v) in [0, 1], or None (transparent)."""
    if not in_round_rect(u, v, 0.06, 0.06, 0.94, 0.94, 0.18):
        return None
    t = (v - 0.06) / 0.88
    color = tuple(round(a + (b - a) * t) for a, b in zip(BG_TOP, BG_BOTTOM))
    for i in range(3):
        y0 = 0.22 + i * 0.205
        y1 = y0 + 0.155
        if in_round_rect(u, v, 0.20, y0, 0.80, y1, 0.035):
            color = UNIT
            cy = (y0 + y1) / 2
            # Drive slots on the left, a status light on the right.
            for k in range(3):
                sx0 = 0.26 + k * 0.075
                if in_round_rect(u, v, sx0, cy - 0.045, sx0 + 0.04, cy + 0.045, 0.012):
                    color = SLOT
            if (u - 0.72) ** 2 + (v - cy) ** 2 <= 0.028 ** 2:
                color = LIGHTS[i]
    return color


def render(size, ss=4):
    rows = []
    for py in range(size):
        row = bytearray([0])  # filter type: none
        for px in range(size):
            acc = [0, 0, 0, 0]
            for sy in range(ss):
                for sx in range(ss):
                    c = shade((px + (sx + 0.5) / ss) / size, (py + (sy + 0.5) / ss) / size)
                    if c is not None:
                        acc[0] += c[0]
                        acc[1] += c[1]
                        acc[2] += c[2]
                        acc[3] += 255
            n = ss * ss
            covered = acc[3] / 255
            if covered:
                row += bytes([round(acc[0] / covered), round(acc[1] / covered),
                              round(acc[2] / covered), round(acc[3] / n)])
            else:
                row += b"\0\0\0\0"
        rows.append(bytes(row))
    return b"".join(rows)


def png(size, pixels):
    def chunk(kind, data):
        c = struct.pack(">I", len(data)) + kind + data
        return c + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(pixels, 9)) + chunk(b"IEND", b""))


if __name__ == "__main__":
    size = int(sys.argv[1])
    with open(sys.argv[2], "wb") as f:
        f.write(png(size, render(size, 4 if size <= 512 else 2)))

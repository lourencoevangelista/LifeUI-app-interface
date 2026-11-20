#!/usr/bin/env python
"""Generate placeholder PWA icons (no dependencies): espresso square with a terracotta inner block.

Usage: python tools/make_icons.py
Replace with real artwork later; the names and sizes are what the manifest expects.
"""
import struct
import zlib
from pathlib import Path

OUT = Path(__file__).resolve().parents[1] / "web" / "public"
BG, FG = (30, 17, 16), (143, 92, 77)


def png(size: int) -> bytes:
    inner_lo, inner_hi = size * 22 // 100, size * 78 // 100  # keeps content inside the maskable safe zone
    rows = bytearray()
    for y in range(size):
        rows.append(0)  # filter type: none
        for x in range(size):
            inside = inner_lo <= x < inner_hi and inner_lo <= y < inner_hi
            rows.extend(FG if inside else BG)

    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(bytes(rows), 9)) + chunk(b"IEND", b"")


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    for size in (180, 192, 512):
        (OUT / f"icon-{size}.png").write_bytes(png(size))
        print(f"wrote icon-{size}.png")

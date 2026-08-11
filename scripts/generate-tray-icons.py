"""Generate the two tray-icon states as macOS template images.

Template images carry shape in the alpha channel only; macOS recolours them to
match the menu bar, so RGB is left black throughout.
"""
import math
import struct
import zlib

SIZE = 44
CENTER = SIZE / 2
SUPERSAMPLE = 4  # per axis, so 16 samples per pixel

RING_OUTER = 14.0
RING_INNER = 9.8
DOT_RADIUS = 5.2


def write_png(path, alpha_rows):
    raw = b""
    for row in alpha_rows:
        raw += b"\x00" + b"".join(bytes((0, 0, 0, a)) for a in row)

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    header = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)  # 8-bit RGBA
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    with open(path, "wb") as handle:
        handle.write(png)


def render(inside):
    rows = []
    step = 1.0 / SUPERSAMPLE
    for y in range(SIZE):
        row = []
        for x in range(SIZE):
            hits = 0
            for sy in range(SUPERSAMPLE):
                for sx in range(SUPERSAMPLE):
                    px = x + (sx + 0.5) * step
                    py = y + (sy + 0.5) * step
                    if inside(px - CENTER, py - CENTER):
                        hits += 1
            row.append(round(255 * hits / SUPERSAMPLE**2))
        rows.append(row)
    return rows


def ring(dx, dy):
    distance = math.hypot(dx, dy)
    return RING_INNER <= distance <= RING_OUTER


def ring_with_dot(dx, dy):
    return ring(dx, dy) or math.hypot(dx, dy) <= DOT_RADIUS


if __name__ == "__main__":
    import sys

    out = sys.argv[1]
    write_png(f"{out}/tray-idle.png", render(ring))
    write_png(f"{out}/tray-syncing.png", render(ring_with_dot))
    print("wrote tray-idle.png and tray-syncing.png")

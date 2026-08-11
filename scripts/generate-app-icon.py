"""Generate the 1024px source artwork for the app icon.

Feed the result to `pnpm tauri icon`, which derives every bundle size plus the
.icns and .ico containers.

The mark is an external drive carrying the universal sync glyph, in the spirit
of the old skeuomorphic backup-drive icons but rebuilt flat: a squircle tile,
plain gradients instead of gloss and bevels, one soft contact shadow, and just
enough detail (base band, slot) to still read as a drive. Detail is kept
deliberately sparse so the shape survives being scaled to 32px.

No drawing libraries are assumed, so everything is rasterised from geometry.
Rectangles and the tile use signed distance fields, which give exact one-sample
antialiasing; the sync glyph is supersampled instead, since an arc with
arrowheads has no convenient closed-form distance.
"""
import math
import struct
import zlib

SIZE = 1024
CENTER = SIZE / 2

TILE_INSET = 100.0
TILE_RADIUS = 185.0
TILE_TOP = (0xFF, 0xFF, 0xFF)
TILE_BOTTOM = (0xD2, 0xDA, 0xE6)

# The drive body, centred slightly high so the contact shadow has room.
DRIVE_CX, DRIVE_CY = CENTER, 496.0
DRIVE_HALF_W, DRIVE_HALF_H = 214.0, 306.0
DRIVE_RADIUS = 88.0
DRIVE_TOP = (0x74, 0xCD, 0xF2)
DRIVE_BOTTOM = (0x3C, 0x8F, 0xDA)

# Dark base band across the foot of the drive.
BASE_TOP_Y = 664.0
BASE_TOP = (0x3B, 0x44, 0x54)
BASE_BOTTOM = (0x2A, 0x31, 0x3D)

SLOT_CY = 736.0
SLOT_HALF_W, SLOT_HALF_H = 74.0, 13.0
SLOT_COLOR = (0x63, 0x6E, 0x7E)

SHADOW_OFFSET = 26.0
SHADOW_SPREAD = 58.0
SHADOW_STRENGTH = 0.20

# Sync glyph, sitting on the pale face above the base band.
GLYPH_CY = 424.0
RING_RADIUS = 112.0
RING_THICKNESS = 42.0
GAP_HALF_WIDTH = 24.0
GAP_CENTRES = (45.0, 225.0)
ARROW_LENGTH = 54.0
ARROW_HALF_BASE = 37.0
GLYPH_COLOR = (0xFF, 0xFF, 0xFF)

SUPERSAMPLE = 3


def write_png(path, rows):
    raw = b""
    for row in rows:
        raw += b"\x00" + b"".join(bytes(px) for px in row)

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    header = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)  # 8-bit RGBA
    with open(path, "wb") as handle:
        handle.write(
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b"")
        )


def clamp01(value):
    return min(max(value, 0.0), 1.0)


def mix(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def rounded_rect_sd(x, y, cx, cy, half_w, half_h, radius):
    """Signed distance to a rounded rectangle: negative inside."""
    dx = abs(x - cx) - (half_w - radius)
    dy = abs(y - cy) - (half_h - radius)
    outside = math.hypot(max(dx, 0.0), max(dy, 0.0))
    inside = min(max(dx, dy), 0.0)
    return outside + inside - radius


def coverage(distance):
    """Antialiased fill from a signed distance, feathered across one pixel."""
    return clamp01(0.5 - distance)


def in_arcs(dx, dy):
    distance = math.hypot(dx, dy)
    if abs(distance - RING_RADIUS) > RING_THICKNESS / 2:
        return False
    angle = math.degrees(math.atan2(dy, dx)) % 360
    return all(
        min(abs(angle - centre), 360 - abs(angle - centre)) > GAP_HALF_WIDTH
        for centre in GAP_CENTRES
    )


def arrow_triangles():
    """One arrowhead per arc, both circling the same way."""
    triangles = []
    for centre in GAP_CENTRES:
        theta = math.radians(centre - GAP_HALF_WIDTH)
        radial = (math.cos(theta), math.sin(theta))
        tangent = (-math.sin(theta), math.cos(theta))
        base = (
            CENTER + radial[0] * RING_RADIUS,
            GLYPH_CY + radial[1] * RING_RADIUS,
        )
        triangles.append(
            (
                (
                    base[0] + tangent[0] * ARROW_LENGTH,
                    base[1] + tangent[1] * ARROW_LENGTH,
                ),
                (
                    base[0] + radial[0] * ARROW_HALF_BASE,
                    base[1] + radial[1] * ARROW_HALF_BASE,
                ),
                (
                    base[0] - radial[0] * ARROW_HALF_BASE,
                    base[1] - radial[1] * ARROW_HALF_BASE,
                ),
            )
        )
    return triangles


TRIANGLES = arrow_triangles()


def in_triangle(x, y, triangle):
    (ax, ay), (bx, by), (cx, cy) = triangle

    def side(px, py, qx, qy):
        return (qx - px) * (y - py) - (qy - py) * (x - px)

    d1, d2, d3 = side(ax, ay, bx, by), side(bx, by, cx, cy), side(cx, cy, ax, ay)
    return not ((d1 < 0 or d2 < 0 or d3 < 0) and (d1 > 0 or d2 > 0 or d3 > 0))


def in_glyph(x, y):
    return in_arcs(x - CENTER, y - GLYPH_CY) or any(
        in_triangle(x, y, triangle) for triangle in TRIANGLES
    )


def render():
    reach = RING_RADIUS + RING_THICKNESS / 2 + ARROW_LENGTH + 4
    glyph_box = (CENTER - reach, CENTER + reach, GLYPH_CY - reach, GLYPH_CY + reach)
    step = 1.0 / SUPERSAMPLE

    rows = []
    for y in range(SIZE):
        py = y + 0.5
        tile_colour = mix(TILE_TOP, TILE_BOTTOM, y / (SIZE - 1))
        row = []
        for x in range(SIZE):
            px = x + 0.5

            tile = coverage(rounded_rect_sd(px, py, CENTER, CENTER, *TILE_HALF, TILE_RADIUS))
            if tile <= 0.0:
                row.append((0, 0, 0, 0))
                continue
            colour = tile_colour

            # Contact shadow: the drive silhouette pushed down and blurred.
            shadow_sd = rounded_rect_sd(
                px,
                py - SHADOW_OFFSET,
                DRIVE_CX,
                DRIVE_CY,
                DRIVE_HALF_W,
                DRIVE_HALF_H,
                DRIVE_RADIUS,
            )
            shadow = clamp01((SHADOW_SPREAD / 2 - shadow_sd) / SHADOW_SPREAD)
            if shadow > 0.0:
                colour = mix(colour, (0x36, 0x3E, 0x4C), shadow * SHADOW_STRENGTH)

            drive = coverage(
                rounded_rect_sd(
                    px, py, DRIVE_CX, DRIVE_CY, DRIVE_HALF_W, DRIVE_HALF_H, DRIVE_RADIUS
                )
            )
            if drive > 0.0:
                span = (py - (DRIVE_CY - DRIVE_HALF_H)) / (2 * DRIVE_HALF_H)
                body = mix(DRIVE_TOP, DRIVE_BOTTOM, clamp01(span))

                # Base band, clipped to the drive so its rounded foot is kept.
                band = clamp01(py - BASE_TOP_Y + 0.5)
                if band > 0.0:
                    band_span = (py - BASE_TOP_Y) / (DRIVE_CY + DRIVE_HALF_H - BASE_TOP_Y)
                    body = mix(body, mix(BASE_TOP, BASE_BOTTOM, clamp01(band_span)), band)

                    slot = coverage(
                        rounded_rect_sd(
                            px, py, CENTER, SLOT_CY, SLOT_HALF_W, SLOT_HALF_H, SLOT_HALF_H
                        )
                    )
                    if slot > 0.0:
                        body = mix(body, SLOT_COLOR, slot)

                colour = mix(colour, body, drive)

            if glyph_box[0] <= px <= glyph_box[1] and glyph_box[2] <= py <= glyph_box[3]:
                hits = sum(
                    1
                    for sy in range(SUPERSAMPLE)
                    for sx in range(SUPERSAMPLE)
                    if in_glyph(x + (sx + 0.5) * step, y + (sy + 0.5) * step)
                )
                if hits:
                    colour = mix(colour, GLYPH_COLOR, hits / SUPERSAMPLE**2)

            row.append((*colour, round(255 * tile)))
        rows.append(row)
    return rows


TILE_HALF = ((SIZE - 2 * TILE_INSET) / 2, (SIZE - 2 * TILE_INSET) / 2)


if __name__ == "__main__":
    import sys

    out = sys.argv[1] if len(sys.argv) > 1 else "assets/logo.png"
    write_png(out, render())
    print(f"wrote {out}")

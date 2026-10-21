"""Render the web app's PNG icons (stdlib only): a prompt chevron and cursor on a dark tile.
Run via `make web`; the output is committed in web/dist. `icons.py --same A B` compares two
PNGs' pixels (`make web-check`)."""
import struct, sys, zlib

def png(size, path):
    bg, fg, accent = (0x16, 0x1b, 0x22), (0xe6, 0xed, 0xf3), (0x58, 0xa6, 0xff)
    r = size * 0.18  # corner radius
    rows = []
    for y in range(size):
        row = bytearray([0])
        for x in range(size):
            u, v = (x + 0.5) / size, (y + 0.5) / size
            # rounded tile (transparent outside)
            dx = max(r / size - u, 0, u - (1 - r / size))
            dy = max(r / size - v, 0, v - (1 - r / size))
            inside = (dx * dx + dy * dy) ** 0.5 <= r / size
            if not inside:
                row += bytes((0, 0, 0, 0)); continue
            c = bg
            # chevron ">": two thick strokes meeting at (0.52, 0.5)
            def seg(ax, ay, bx, by, w):
                px, py = bx - ax, by - ay
                t = max(0, min(1, ((u - ax) * px + (v - ay) * py) / (px * px + py * py)))
                qx, qy = ax + t * px - u, ay + t * py - v
                return (qx * qx + qy * qy) ** 0.5 <= w
            if seg(0.26, 0.30, 0.50, 0.50, 0.055) or seg(0.50, 0.50, 0.26, 0.70, 0.055):
                c = fg
            # cursor block
            if 0.56 <= u <= 0.76 and 0.62 <= v <= 0.72:
                c = accent
            row += bytes((*c, 255))
        rows.append(bytes(row))
    raw = zlib.compress(b"".join(rows), 9)
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", raw) + chunk(b"IEND", b"")
    open(path, "wb").write(data)

def decoded(path):
    """A PNG's header and decompressed image data: equal for equal pixels, whatever zlib
    compressed them (the bytes differ between zlib and zlib-ng)."""
    data, at, head, idat = open(path, "rb").read(), 8, b"", b""
    while at < len(data):
        n, t = struct.unpack(">I4s", data[at:at + 8])
        if t == b"IHDR": head = data[at + 8:at + 8 + n]
        if t == b"IDAT": idat += data[at + 8:at + 8 + n]
        at += 12 + n
    return head, zlib.decompress(idat)

if sys.argv[1] == "--same":  # exit 0 when two PNGs hold the same image (scripts/web-check.sh)
    try:
        sys.exit(0 if decoded(sys.argv[2]) == decoded(sys.argv[3]) else 1)
    except (OSError, struct.error, zlib.error):
        sys.exit(1)

out = sys.argv[1]
for size, name in [(192, "icon-192.png"), (512, "icon-512.png"), (180, "apple-touch-icon.png")]:
    png(size, f"{out}/{name}")

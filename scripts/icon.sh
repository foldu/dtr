#!/usr/bin/env bash
# Regenerate the app icon: the Windows icon resource and the window's own bitmap.
#
#   ./scripts/icon.sh
#
# Both come out of `assets/logo/dtr-icon.svg` so they cannot drift. `dtr.ico` is
# what Explorer, the taskbar and Alt-Tab read out of dtr.exe (a window that sets
# no icon of its own falls back to it), and `dtr-icon-32.png` is the RGBA bitmap
# winit hands to `WM_SETICON` for the running window.
#
# Every ICO entry is rendered from the vector *at that size* rather than scaled
# from one bitmap: Windows picks among the entries and never resamples between
# them, and a 16-unit pixel mark is only crisp at integer multiples of its grid.
# 128 and 256 are stored PNG-compressed and the rest as 32-bit DIBs with an AND
# mask — a 256 DIB alone would be 270 KB of an exe whose icon is one line of a
# resource directory.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
out=$root/assets/logo

# resvg is not in the devShell (it is not needed to build dtr); nix is.
resvg=(resvg)
command -v resvg >/dev/null || resvg=(nix run nixpkgs#resvg --)

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for size in 16 32 48 64 128 256; do
  "${resvg[@]}" -w "$size" -h "$size" "$out/dtr-icon.svg" "$tmp/$size.png"
done
cp "$tmp/32.png" "$out/dtr-icon-32.png"

python3 - "$tmp" "$out/dtr.ico" <<'PY'
"""Pack one PNG per size into an ICO: DIBs below 128, PNG from 128 up."""
import struct
import sys
import zlib
from pathlib import Path

source, dest = Path(sys.argv[1]), Path(sys.argv[2])
# One entry per rendered size, in ascending order; the bash loop above decides
# which those are, so there is one list.
sizes = sorted(int(path.stem) for path in source.glob("*.png"))


def read_png(path):
    """Decode an 8-bit RGBA, non-interlaced PNG into top-down RGBA bytes."""
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", f"{path} is not a PNG"
    pos, idat, width = 8, bytearray(), 0
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, colour, _, _, interlace = struct.unpack(
                ">IIBBBBB", body
            )
            assert (depth, colour, interlace) == (8, 6, 0), (
                f"{path}: expected 8-bit RGBA, non-interlaced"
            )
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    raw = zlib.decompress(bytes(idat))
    stride = width * 4
    pixels, previous = bytearray(), bytearray(stride)
    for y in range(height):
        start = y * (stride + 1)
        filter_type = raw[start]
        line = bytearray(raw[start + 1:start + 1 + stride])
        for i in range(stride):
            left = line[i - 4] if i >= 4 else 0
            above = previous[i]
            corner = previous[i - 4] if i >= 4 else 0
            if filter_type == 1:
                line[i] = (line[i] + left) & 0xFF
            elif filter_type == 2:
                line[i] = (line[i] + above) & 0xFF
            elif filter_type == 3:
                line[i] = (line[i] + (left + above) // 2) & 0xFF
            elif filter_type == 4:
                estimate = left + above - corner
                to_left = abs(estimate - left)
                to_above = abs(estimate - above)
                to_corner = abs(estimate - corner)
                if to_left <= to_above and to_left <= to_corner:
                    near = left
                elif to_above <= to_corner:
                    near = above
                else:
                    near = corner
                line[i] = (line[i] + near) & 0xFF
            elif filter_type != 0:
                raise AssertionError(f"{path}: unknown filter {filter_type}")
        pixels += line
        previous = line
    return width, height, bytes(pixels)


def as_dib(width, height, pixels):
    """A 32-bit bottom-up BGRA bitmap plus the 1-bit AND mask ICO also wants.

    The mask is what a pre-Vista renderer uses for transparency; 32-bit icons are
    drawn from the alpha channel, but leaving the mask at zero would give them a
    black square where the tile's corners are.
    """
    and_stride = (width + 31) // 32 * 4
    rows = []
    mask = bytearray(and_stride * height)
    for row_number, y in enumerate(reversed(range(height))):
        row = bytearray()
        for x in range(width):
            r, g, b, a = pixels[(y * width + x) * 4:(y * width + x) * 4 + 4]
            row += bytes((b, g, r, a))
            if a < 128:
                mask[row_number * and_stride + x // 8] |= 0x80 >> (x % 8)
        rows.append(bytes(row))
    xor = b"".join(rows)
    header = struct.pack(
        "<IiiHHIIiiII",
        40,                # biSize
        width,             # biWidth
        height * 2,        # biHeight: XOR image, then the AND mask
        1,                 # biPlanes
        32,                # biBitCount
        0,                 # BI_RGB
        len(xor) + len(mask),
        0, 0, 0, 0,
    )
    return header + xor + bytes(mask)


entries = []
for size in sizes:
    path = source / f"{size}.png"
    if size >= 128:
        payload = path.read_bytes()
    else:
        payload = as_dib(*read_png(path))
    entries.append((size, payload))

# Directory first, then the payloads; every offset is from the start of the
# file, and a 256-pixel entry is written as 0 in the one-byte dimension fields.
offset = 6 + 16 * len(entries)
directory, payloads = bytearray(), bytearray()
for size, payload in entries:
    directory += struct.pack(
        "<BBBBHHII",
        size if size < 256 else 0,
        size if size < 256 else 0,
        0, 0, 1, 32,
        len(payload), offset + len(payloads),
    )
    payloads += payload

dest.write_bytes(
    struct.pack("<HHH", 0, 1, len(entries)) + bytes(directory) + bytes(payloads)
)
print(f"wrote {dest}: " + ", ".join(f"{size}px" for size, _ in entries))
PY

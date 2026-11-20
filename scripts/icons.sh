#!/bin/sh
# Render the web app's icons (web/dist) from the project icon, docs/logos/ptyro-icon.svg.
# Run via `make icons` (needs rsvg-convert: brew install librsvg / apt install librsvg2-bin);
# the output is committed. Three shapes: the rounded tile ("any"), a full-bleed square for
# iOS (which rounds it itself) and a maskable one with the drawing shrunk into the safe zone.
set -eu
cd "$(dirname "$0")/.."
command -v rsvg-convert >/dev/null || { echo "install rsvg-convert: brew install librsvg (or apt install librsvg2-bin)" >&2; exit 1; }
src=docs/logos/ptyro-icon.svg out=web/dist
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cp "$src" "$out/icon.svg"
rsvg-convert -w 192 -h 192 "$src" -o "$out/icon-192.png"
rsvg-convert -w 512 -h 512 "$src" -o "$out/icon-512.png"
# Square: the tile's rounded corners become plain corners.
sed 's/ rx="112"//' "$src" > "$tmp/square.svg"
rsvg-convert -w 180 -h 180 "$tmp/square.svg" -o "$out/apple-touch-icon.png"
# Maskable: Android crops to a circle of 40% of the width, and the four panes sit in the
# corners, so scale the drawing to 66% (a pane's far corner then lands just inside the circle).
sed -e 's/ rx="112"//' -e 's|\(</defs><rect[^>]*/>\)|\1<g transform="translate(87.04 87.04) scale(.66)">|' -e 's|</svg>|</g></svg>|' "$src" > "$tmp/maskable.svg"
grep -q 'scale(.66)' "$tmp/maskable.svg" || { echo "icon svg layout changed: fix the maskable sed in $0" >&2; exit 1; }
rsvg-convert -w 512 -h 512 "$tmp/maskable.svg" -o "$out/icon-maskable-512.png"

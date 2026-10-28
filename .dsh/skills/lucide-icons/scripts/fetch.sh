#!/usr/bin/env bash
# Fetch Lucide SVGs from lucide.dev into this repository's asset directory.
#
#   scripts/fetch.sh NAME [NAME ...]
#
# Existing files are left alone: a fetched icon is a reviewed asset, and a
# silent replacement changes what the UI draws. Delete the file to re-fetch.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
dest="$root/crates/dtr/assets/icons"
mkdir -p "$dest"

[ "$#" -gt 0 ] || { echo "usage: fetch.sh NAME [NAME ...]" >&2; exit 2; }

for name in "$@"; do
    case "$name" in
        *[!a-z0-9-]*|'') echo "not an icon name: $name" >&2; exit 2 ;;
    esac
    out="$dest/$name.svg"
    if [ -s "$out" ]; then
        echo "have  $name ($(sha256sum "$out" | cut -c1-12))"
        continue
    fi
    url="https://lucide.dev/api/icons/$name"
    if ! curl --fail --silent --show-error --location --max-time 30 "$url" --output "$out"; then
        rm -f "$out"
        echo "no icon named $name (see https://lucide.dev/icons)" >&2
        exit 1
    fi
    # Lucide strokes with `currentColor`, which an SVG rasteriser resolves to
    # black — and a tint multiplies, so black stays black. White strokes make
    # the file a mask that `.tint()` can colour at draw time.
    sed -i 's/currentColor/#ffffff/g' "$out"
    # The API answers 200 with HTML for some unknown names; a real icon starts
    # with an <svg> element.
    if ! head -c 4 "$out" | grep -q '<svg'; then
        rm -f "$out"
        echo "$name did not return an SVG" >&2
        exit 1
    fi
    echo "fetch $name ($(sha256sum "$out" | cut -c1-12))"
done

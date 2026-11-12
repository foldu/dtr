#!/usr/bin/env bash
# Fetch the shells' own logos into this repository's icon directory.
#
#   scripts/shell-icons.sh
#
# Existing files are left alone: a fetched icon is a reviewed asset, and a silent
# replacement changes what the UI draws. Delete the file to re-fetch.
#
# Where each one comes from, and the licence it arrives under. A shell's logo
# names the shell — that is what a logo is for — but the artwork is still the
# project's, so this table is the place to look before shipping a change to one:
#
#   bash  GNU Bash's official logo, MIT
#         https://github.com/odb/official-bash-logo
#   zsh   Zsh's official logo, CC BY-SA 4.0 — © Justin Dorfman and contributors
#         https://github.com/Zsh-art/logo
#   fish  fish-shell's logo: GPL-2.0 artwork, and a fish-shell trademark whose
#         nominative use — saying which shell a row is — TRADEMARKS.md allows
#         https://github.com/fish-shell/fish-site/tree/master/artwork
#   nu    Nushell's logo, MIT
#         https://github.com/nushell/nushell
#
# The files are stored as the projects publish them: no colour is rewritten,
# because a brand mark keeps its own, so a refreshed file is a `git diff` of the
# artwork rather than of our post-processing. Nothing here is fetched by the
# build — the SVGs are committed and embedded with `egui::include_image!`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/crates/dtr/assets/icons"
mkdir -p "$dest"

fetch() { # fetch NAME URL
    local name="$1"
    local url="$2"
    local out="$dest/$name.svg"
    if [ -s "$out" ]; then
        echo "have  $name ($(sha256sum "$out" | cut -c1-12))"
        return
    fi
    if ! curl --fail --silent --show-error --location --max-time 30 "$url" --output "$out"; then
        rm -f "$out"
        echo "could not fetch $name from $url" >&2
        exit 1
    fi
    # A missing name answers 200 with a page or a 404 body; a real icon is an
    # `<svg>` element and nothing else.
    if ! head -c 4 "$out" | grep -q '<svg'; then
        rm -f "$out"
        echo "$name did not return an SVG" >&2
        exit 1
    fi
    echo "fetch $name ($(sha256sum "$out" | cut -c1-12))"
}

fetch bash "https://cdn.simpleicons.org/gnubash"
fetch zsh "https://cdn.simpleicons.org/zsh"
fetch fish "https://cdn.simpleicons.org/fishshell"
fetch nu "https://cdn.simpleicons.org/nushell"

#!/usr/bin/env bash
# Smoke-test the cross-built Windows exe without a Windows machine.
#
#   ./scripts/wine-smoke.sh
#
# Runs the PE under Wine, renders one frame offscreen, and compares it with the
# Linux build's frame. Byte equality is the assertion: same fonts, same layout,
# same colours out of a binary produced by a different compiler and target.
#
# Two caveats, both real:
#   - Wine's console support is incomplete, so the ConPTY shell path is not
#     exercised here. Only the renderer is.
#   - wgpu picks dx12 first on Windows and Wine's FXC cannot compile egui's
#     shader (shader model 5.1 resource arrays), so we force the Vulkan backend.
set -euo pipefail

cd "$(dirname "$0")/.."

exe=$(nix build .#windows --no-link --print-out-paths)/bin/dtr.exe
linux=$(nix build .#dtr --no-link --print-out-paths)/bin/dtr

"$linux" --demo --screenshot /tmp/dtr-linux-frame.png >/dev/null 2>&1

WINEPREFIX=${WINEPREFIX:-/tmp/dtr-wine} \
WINEDEBUG=${WINEDEBUG:--all} \
WINEDLLOVERRIDES=${WINEDLLOVERRIDES:-mscoree,mshtml=} \
WGPU_BACKEND=${WGPU_BACKEND:-vulkan} \
    nix run nixpkgs#wine64 -- "$exe" --demo \
    --screenshot 'Z:\tmp\dtr-windows-frame.png' >/tmp/dtr-wine.log 2>&1 ||
    { echo "wine run failed; see /tmp/dtr-wine.log" >&2; exit 1; }

if cmp -s /tmp/dtr-linux-frame.png /tmp/dtr-windows-frame.png; then
    echo "windows exe runs under wine and renders identically"
else
    echo "frames differ; see /tmp/dtr-linux-frame.png and /tmp/dtr-windows-frame.png" >&2
    exit 1
fi

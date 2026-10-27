#!/usr/bin/env bash
# Render the UI to a PNG without a window.
#
#   ./scripts/shot.sh [out.png] [WxH]
#
# In-app capture only: no compositor, no display server, so it works with the
# session locked and in CI.
set -euo pipefail

OUT=${1:-/tmp/dtr.png}
SIZE=${2:-900x620}

cd "$(dirname "$0")/.."
nix develop .#default --command cargo run -q -p dtr -- \
    --screenshot "$OUT" --size "$SIZE"
echo "$OUT"

#!/usr/bin/env bash
# Profile a real window and print where the CPU went.
#
#   ./scripts/flamegraph.sh              # text report + /tmp/dtr.svg
#   ./scripts/flamegraph.sh --demo       # extra args go to dtr
#
# Uses the `profiling` cargo profile: optimised like release, but with debug
# info, because a stripped binary's flamegraph is a pile of `ptr` and `unknown`.
#
# Two ways to sample, and the script picks the one that works there:
#   * `perf` + cargo-flamegraph, when the kernel allows it (`perf_event_paranoid`
#     <= 2 on Linux, and perf is in the devShell);
#   * otherwise dtr's own `--cpu-profile`, which samples itself and needs no
#     privileges — and is the only option on Windows.
#
# Close the window normally (Ctrl+Shift+W) so the in-process profiler can write
# its report; SIGKILL loses it.
set -euo pipefail

cd "$(dirname "$0")/.."

# Re-run inside the flake: the toolchain, perf and pkg-config only exist there.
if ! command -v pkg-config >/dev/null 2>&1; then
    exec nix develop .#default --command "$PWD/scripts/flamegraph.sh" "$@"
fi

out=${DTR_FLAMEGRAPH:-/tmp/dtr}
export PKG_CONFIG_PATH
PKG_CONFIG_PATH="$(nix build .#libghostty-vt.dev --no-link --print-out-paths)/share/pkgconfig"
PKG_CONFIG_PATH+=":$(nix build nixpkgs#udev.dev --no-link --print-out-paths)/lib/pkgconfig"
PKG_CONFIG_PATH+=":$(nix build nixpkgs#fontconfig.dev --no-link --print-out-paths)/lib/pkgconfig"
PKG_CONFIG_PATH+=":$(nix build nixpkgs#freetype.dev --no-link --print-out-paths)/lib/pkgconfig"

cargo build --profile profiling -p dtr

# `perf` counts fine here but refuses to record, so the probe is the real thing:
# try the perf path, and fall back when it cannot sample. A failed attempt costs
# a second and leaves the in-process profiler as the answer rather than an error.
if command -v cargo-flamegraph >/dev/null 2>&1; then
    echo "trying perf via cargo flamegraph -> ${out}.svg" >&2
    if cargo flamegraph --profile profiling --bin dtr --output "${out}.svg" -- "$@"; then
        exit 0
    fi
    echo "perf could not sample; using dtr's in-process profiler instead" >&2
fi

echo "the text report and the flamegraph are written when the window closes" >&2
exec "./target/profiling/dtr" --cpu-profile "${out}.svg" "$@"

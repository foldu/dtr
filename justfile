# dtr's task list. The toolchain lives in the flake, so a recipe that needs
# cargo asks the flake for a devShell unless it is already inside one.
set shell := ["bash", "-euo", "pipefail", "-c"]

# `just` is installed in the devShell, but someone with a global `just` should
# get the same answer: inside the devShell cargo is on PATH, outside it is not.
dev := if `command -v cargo >/dev/null 2>&1 && echo yes` == "yes" { "" } else { "nix develop .#default --command" }

# List the recipes.
default:
    @just --list

# Every gate CI runs: the same `nix flake check` the pipelines run.
check:
    nix flake check

# The suite without the sandbox: the inner loop.
test *args:
    {{ dev }} cargo test --workspace {{ args }}

# Format the Rust sources and this justfile.
fmt:
    {{ dev }} cargo fmt --all
    just --fmt

# Rewrite the kittest baselines, from the devShell.
snapshots *args:
    # From the devShell because the rasteriser that writes a baseline has to be
    # the one that reads it.
    UPDATE_SNAPSHOTS=1 {{ dev }} cargo test --workspace {{ args }}

# The Linux package.
build:
    nix build .#dtr
    ls -l result/bin

# The one-file Windows executable.
windows:
    nix build .#windows
    ls -l result/bin

# Run the debug build.
run *args:
    {{ dev }} cargo run -p dtr -- {{ args }}

# The cross-built exe under Wine, compared with a Linux frame.
wine:
    ./scripts/wine-smoke.sh

# vtebench; `just bench -- --check` only validates the payloads.
bench *args:
    ./scripts/bench.sh {{ args }}

# Cut a release: bump, commit, tag and push.
[confirm]
release version:
    #!/usr/bin/env bash
    set -euo pipefail

    version="{{ version }}"
    tag="v$version"

    if ! printf '%s' "$version" | grep -Eqx '[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?'; then
        echo "release: $version is not X.Y.Z (or X.Y.Z-suffix)" >&2
        exit 1
    fi

    # A tag names a commit, so the release is cut from a clean tree on the
    # branch that gets mirrored.
    dirty=$(git status --porcelain)
    if [ -n "$dirty" ]; then
        printf 'release: the tree is dirty:\n%s\n' "$dirty" >&2
        exit 1
    fi
    branch=$(git rev-parse --abbrev-ref HEAD)
    if [ "$branch" != master ]; then
        echo "release: on $branch, not master" >&2
        exit 1
    fi
    if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
        echo "release: $tag already exists" >&2
        exit 1
    fi

    # One source of truth: Cargo.toml. flake.nix imports it, so this edit moves
    # the package version, the exe's version and the release together — and the
    # workflow refuses a tag that disagrees with it.
    sed -i "0,/^version = \".*\"/s//version = \"$version\"/" Cargo.toml
    # Cargo refreshes the workspace members' entries in the lockfile.
    {{ dev }} cargo metadata --format-version 1 >/dev/null
    if git diff --quiet; then
        echo "release: Cargo.toml already says $version" >&2
        exit 1
    fi

    flake_version=$(nix eval --raw .#packages.x86_64-linux.dtr.version)
    if [ "$flake_version" != "$version" ]; then
        echo "release: the flake says $flake_version, expected $version" >&2
        exit 1
    fi

    git add Cargo.toml Cargo.lock
    git commit -m "Release $tag"
    git tag -a "$tag" -m "dtr $version"
    git push origin HEAD
    git push origin "$tag"

    echo
    echo "$tag is pushed. If the GitHub mirror copies tags,"
    echo ".github/workflows/release.yml builds dtr.exe and publishes it;"
    echo "if it does not, push the tag by hand: git push github $tag"

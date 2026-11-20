# dtr's task list. The toolchain lives in the flake, so a recipe that needs
# cargo asks the flake for a devShell unless it is already inside one.
set shell := ["bash", "-euo", "pipefail", "-c"]

# `just` is installed in the devShell, but someone with a global `just` should
# get the same answer: inside the devShell cargo is on PATH, outside it is not.
# The `if` is load bearing: `command -v cargo && echo yes` exits 1 when cargo is
# missing, and just reports a failed backtick instead of answering "no".
dev := if `if command -v cargo >/dev/null 2>&1; then echo yes; else echo no; fi` == "yes" { "" } else { "nix develop .#default --command" }

# `just test` needs nextest, which is not part of the Rust toolchain: a machine
# can have a global `cargo` and no `cargo-nextest`, and the failure for that is
# a bare "no such command". Ask for the tool the recipe actually runs, and for
# cargo too — nextest is a cargo subcommand.
nxt := if `if command -v cargo-nextest >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then echo yes; else echo no; fi` == "yes" { "" } else { "nix develop .#default --command" }

# List the recipes.
default:
    @just --list

# Every gate CI runs: the same `nix flake check` the pipelines run.
check:
    nix flake check

# Supply-chain checks: licences, bans, sources, and advisories.
deny:
    # The advisory half fetches the RustSec database, which is why this is not
    # `nix flake check`: `checks.deny` is the half that answers offline.
    {{ dev }} cargo deny check

# The suite without the sandbox: the inner loop. nextest runs one process per
# test and schedules every binary together, where `cargo test` finishes one
# binary before starting the next — worth about half a second here, most of
# which is that the 3 s test no longer waits for the other binaries. It
# cannot run doctests, so they follow it. One shell, so a machine outside the
# devShell pays for one `nix develop` rather than one per command.
# `docs/nextest.md` has the measurements and what CI still runs instead.
test *args:
    {{ nxt }} bash -c 'cargo nextest run --workspace {{ args }} && cargo test --workspace --doc {{ args }}'

# Format the Rust sources and this justfile. The flake checks both halves
# (`fmt` and `just-fmt`), so forgetting this is a red `nix flake check` rather
# than a two-thousand-line diff in the middle of your next change.
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

# Regenerate the app icon: the Windows resource and the window's bitmap.
icons:
    ./scripts/icon.sh

# Refresh the bundled ConPTY (`crates/dtr/assets/conpty`) from a Windows Terminal
# release: `just conpty latest`, or a tag such as `v1.25.3131.0`. Download, verify,
# `UPSTREAM` and the version the unpack directory is named after live in
# `scripts/conpty.sh`; re-running it for the vendored version changes nothing, so
# it is the check as well as the update.
conpty tag:
    ./scripts/conpty.sh {{ tag }}

# Regenerate the README's two screenshots. The Windows half needs `winvm`.
screenshots *args:
    ./scripts/screenshots.sh {{ args }}

# vtebench; `just bench -- --check` only validates the payloads.
bench *args:
    ./scripts/bench.sh {{ args }}

# Refuse a bad release before the gate runs: `just` runs a recipe's dependencies
# in order, so a typo, a dirty tree or a missing mirror answers in a second
# instead of after the `nix flake check` that follows it.
_release-version version:
    #!/usr/bin/env bash
    set -euo pipefail

    version="{{ version }}"

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
    if git rev-parse -q --verify "refs/tags/v$version" >/dev/null; then
        echo "release: v$version already exists" >&2
        exit 1
    fi

    # Two remotes, because the tag is what publishes: GitLab (`origin`) holds
    # the history and runs the flake gate, GitHub (`github`) runs
    # `.github/workflows/release.yml` and makes the release. A tag that reaches
    # only one of them is half a release, so a missing mirror is refused here
    # rather than after the commit exists.
    if ! git remote get-url github >/dev/null 2>&1; then
        echo "release: no 'github' remote; add the mirror first:" >&2
        echo "  git remote add github git@github.com:foldu/dtr.git" >&2
        exit 1
    fi

# Cut a release: the gate, the bump, the commit, the tag, and both pushes.
[confirm]
release version: (_release-version version) check
    #!/usr/bin/env bash
    set -euo pipefail

    version="{{ version }}"
    tag="v$version"

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

    # GitHub is where the release is published, so the commit and the tag go
    # there too. A sandboxed terminal's own `ssh` refuses the system config and
    # cannot reach the agent, so when this push fails try the forwarded one:
    #   GIT_SSH_COMMAND=$HOME/.dsh/bin/fwd-ssh git push github HEAD --follow-tags
    git push github HEAD
    git push github "$tag"

    echo
    echo "$tag is pushed to origin and github. The tag starts"
    echo ".github/workflows/release.yml, which runs the gate again, builds"
    echo "dtr.exe and publishes it. Follow it with:"
    echo "  gh run watch -R foldu/dtr"

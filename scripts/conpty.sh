#!/usr/bin/env bash
# Refresh the ConPTY the Windows exe carries (`crates/dtr/assets/conpty`).
#
#   ./scripts/conpty.sh latest        # the newest Windows Terminal release
#   ./scripts/conpty.sh v1.25.3131.0  # or a specific one
#   just conpty latest                # the recipe in the justfile
#
# Microsoft publishes its ConPTY as a `Microsoft.Windows.Console.ConPTY` NuGet
# package on each Windows Terminal release, and that package — not a submodule and
# not a local build — is the source of truth for the two binaries dtr embeds.
# Inbox conhost consumes the APC sequences the kitty graphics protocol rides in;
# this host forwards them (`docs/windows.md` §12). Rebuilding it ourselves would
# mean an MSVC toolchain for two files Microsoft already builds.
#
# The release's version (`v1.25.3131.0`) and the package's version
# (`1.25.261109001`) are different numbers, so both the download URL and the
# package version come from the release's own asset list rather than being
# guessed. What is committed is recorded in `assets/conpty/UPSTREAM` (tag,
# package, URL and SHA-256 of the package and both files), and the version the
# unpack directory is named after is `VERSION` in `crates/dtr/src/conpty.rs`.
# Running this for the version already vendored changes nothing, so it is also
# the check.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
assets=$root/crates/dtr/assets/conpty
source=$root/crates/dtr/src/conpty.rs

tag=${1-}
if [ -z "$tag" ]; then
  echo "usage: $0 latest | vX.Y.Z (see the header of this script)" >&2
  exit 2
fi

for tool in curl unzip python3 sha256sum install cmp; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "conpty: $tool is not on PATH" >&2
    exit 1
  fi
done

case "$tag" in
  latest)
    release=https://api.github.com/repos/microsoft/terminal/releases/latest
    ;;
  v[0-9]*)
    release="https://api.github.com/repos/microsoft/terminal/releases/tags/$tag"
    ;;
  *)
    echo "conpty: expected 'latest' or a release tag such as v1.25.3131.0, got '$tag'" >&2
    exit 1
    ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

curl -fsS -o "$work/release.json" "$release"
python3 - "$work/release.json" "$work/release.txt" <<'PY'
import json
import sys

release = json.load(open(sys.argv[1]))
assets = [
    asset["browser_download_url"]
    for asset in release["assets"]
    if asset["name"].startswith("Microsoft.Windows.Console.ConPTY.")
    and asset["name"].endswith(".nupkg")
]
if not assets:
    sys.exit("conpty: that release carries no Microsoft.Windows.Console.ConPTY package")
with open(sys.argv[2], "w") as handle:
    handle.write(f"{release['tag_name']}\n{assets[0]}\n")
PY
{ read -r tag; read -r asset_url; } < "$work/release.txt"
case "$tag" in
  v[0-9]*) ;;
  *)
    echo "conpty: the release answered with a tag that is not vX.Y.Z: $tag" >&2
    exit 1
    ;;
esac

package=$(basename "$asset_url")
version=${package#Microsoft.Windows.Console.ConPTY.}
version=${version%.nupkg}

echo "conpty: $tag carries $package"
curl -fsSL -o "$work/$package" "$asset_url"
curl -fsS -o "$work/LICENSE" \
  "https://raw.githubusercontent.com/microsoft/terminal/$tag/LICENSE"

# The two members the package publishes for x64. `conpty.dll` starts
# `OpenConsole.exe` from its own directory, so they are one unit.
unzip -p "$work/$package" runtimes/win-x64/native/conpty.dll > "$work/conpty.dll"
unzip -p "$work/$package" build/native/runtimes/x64/OpenConsole.exe > "$work/OpenConsole.exe"
for name in conpty.dll OpenConsole.exe; do
  if [ "$(head -c 2 "$work/$name")" != MZ ]; then
    echo "conpty: $name in $package is not a PE image" >&2
    exit 1
  fi
done

# Replace only what changed, so a re-run of the vendored version leaves the tree
# alone rather than rewriting binaries with the same bytes.
changed=0
for name in conpty.dll OpenConsole.exe LICENSE; do
  if cmp -s "$work/$name" "$assets/$name"; then
    continue
  fi
  install -m 644 "$work/$name" "$assets/$name"
  changed=1
done

hash() { sha256sum "$1" | cut -d' ' -f1; }
package_sha=$(hash "$work/$package")
dll_sha=$(hash "$assets/conpty.dll")
console_sha=$(hash "$assets/OpenConsole.exe")

cat > "$work/UPSTREAM" <<EOF
# Written by "just conpty $tag"; run the recipe, do not edit by hand.
tag=$tag
version=$version
package=$package
url=$asset_url
nupkg-sha256=$package_sha
conpty-dll-sha256=$dll_sha
openconsole-sha256=$console_sha
EOF
if ! cmp -s "$work/UPSTREAM" "$assets/UPSTREAM"; then
  install -m 644 "$work/UPSTREAM" "$assets/UPSTREAM"
  changed=1
fi

# The unpack directory is named after this constant, so an upgrade can never
# reuse a host it did not carry. Python rather than `sed`: the line contains `&`,
# which sed reads as the matched text in a replacement.
python3 - "$source" "$version" <<'PY_VERSION'
import re
import sys

path, version = sys.argv[1], sys.argv[2]
text = open(path).read()
updated, count = re.subn(
    r'^pub const VERSION: &str = ".*";$',
    f'pub const VERSION: &str = "{version}";',
    text,
    count=1,
    flags=re.MULTILINE,
)
if count != 1:
    sys.exit(f"conpty: no VERSION line in {path}")
open(path, "w").write(updated)
PY_VERSION
if ! grep -q "pub const VERSION: &str = \"$version\";" "$source"; then
  echo "conpty: could not rewrite VERSION in $source" >&2
  exit 1
fi

if [ "$changed" = 0 ]; then
  echo "conpty: $tag is already vendored (version $version); nothing changed"
else
  echo "conpty: vendored $tag (version $version)"
  echo "conpty: next: just fmt && just test conpty && git add crates/dtr/assets/conpty crates/dtr/src/conpty.rs"
fi

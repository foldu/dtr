#!/usr/bin/env bash
# Regenerate the README's two screenshots.
#
#   ./scripts/screenshots.sh            # both halves, if this host has `winvm`
#   ./scripts/screenshots.sh --linux    # just the Adwaita frame
#   ./scripts/screenshots.sh --windows  # just the Windows frame
#
# Both are the app's own `--capture`: it copies the live window's surface, so
# what lands in assets/readme/ is what the window drew, not a compositor grab.
#
# The Linux half runs under Xvfb because `--capture` needs a window and a
# surface, and the screen is 800x600 on purpose: an X pointer rests at the
# centre of the screen, and a 900x620 window at the origin puts that point over
# the terminal, where nothing highlights. At 1280x800 the same point is inside
# the file pane and one tile draws as hovered.
#
# The Windows half needs jupiter's `winvm` guest. The capture must run in the
# console session — a process started over ssh is in session 0, which has no
# D3D12 swapchain — so the frame is taken by a scheduled task with /it.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
out=$root/assets/readme
work=$(mktemp -d)
trap 'rm -rf "$work"; [ -n "${xvfb_pid:-}" ] && kill "$xvfb_pid" 2>/dev/null || true' EXIT

do_linux=1
do_windows=1
for arg in "$@"; do
    case "$arg" in
        --linux) do_windows=0 ;;
        --windows) do_linux=0 ;;
        -h | --help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "screenshots: unknown argument $arg" >&2; exit 2 ;;
    esac
done

# The toolchain, the fonts and the software Vulkan all live in the flake, so
# every cargo and dtr invocation goes through the devShell unless we are
# already in one. `command -v cargo` is not enough to tell: a machine can have
# a profile cargo that cannot find fontconfig's pkg-config file.
if [ "${IN_NIX_SHELL:-}" = "impure" ]; then
    runner=()
else
    runner=(nix develop .#default --command)
fi

mkdir -p "$out"

# --- Linux, the Adwaita look -------------------------------------------------

if [ "$do_linux" = 1 ]; then
    dtr=$root/target/debug/dtr
    "${runner[@]}" cargo build --manifest-path "$root/Cargo.toml" -p dtr

    # The terminal is Kanagawa (the theme the binary carries); the chrome is
    # Adwaita dark so the two halves of the window agree.
    cat >"$work/linux.toml" <<'EOF'
[terminal]
theme = "Kanagawa Wave"

[ui]
scheme = "dark"
EOF

    # bash, with an rc of its own: the prompt, the tab title and the starting
    # directory are the capture's, and no ~/.bashrc can change them.
    #
    # No `\[`/`\]` around the colour: a nix devShell's bash is built without
    # readline (it is the build-script interpreter), so those markers are not
    # consumed and print literally.
    cat >"$work/rc.sh" <<'EOF'
PS1='\e[32m\w\e[0m $ '
PROMPT_COMMAND='printf "\033]7;file://%s%s\007" "${HOSTNAME:-localhost}" "$PWD"'
printf '\033]2;bash\007'
cd "${DTR_SHOT_DIR:-$PWD}" || exit
EOF

    xvfb=$(command -v Xvfb || nix build nixpkgs#xvfb --no-link --print-out-paths)/bin/Xvfb
    display=
    for n in $(seq 99 -1 90); do
        if [ ! -e "/tmp/.X11-unix/X$n" ]; then
            display=":$n"
            break
        fi
    done
    [ -n "$display" ] || {
        echo "screenshots: no free X display in :90-:99" >&2
        exit 1
    }
    "$xvfb" "$display" -screen 0 800x600x24 -nolisten tcp -ac &
    xvfb_pid=$!
    for _ in $(seq 1 50); do
        [ -e "/tmp/.X11-unix/X${display#:}" ] && break
        sleep 0.1
    done

    # `--keys` types at the prompt, one stroke per frame, which is why the
    # commands land after the shell is up rather than being echoed by the tty
    # before it started.
    DISPLAY=$display DTR_SHOT_DIR=$root/crates \
        "${runner[@]}" "$dtr" \
        --config "$work/linux.toml" \
        --widgets adwaita \
        --files "$root/crates" \
        --shell "bash --rcfile $work/rc.sh" \
        --keys "right;ctrl-alt-left;text:ls;enter;text:cargo -V;enter" \
        --capture "$out/linux.png" --capture-after 4000 --exit-after 6000

    echo "wrote $out/linux.png"
fi

# --- Windows, the Windows look ----------------------------------------------

if [ "$do_windows" = 1 ]; then
    if ! command -v winvm >/dev/null 2>&1; then
        echo "screenshots: no winvm on this host; skipping the Windows frame" >&2
        [ "$do_linux" = 1 ] && exit 0
        exit 1
    fi

    nix build "$root#windows" --out-link "$work/result"
    mkdir -p "$work/win/out"
    cp "$work/result/bin/dtr.exe" "$work/win/dtr.exe"

    cat >"$work/win/windows.toml" <<'EOF'
[terminal]
theme = "Kanagawa Wave"

[ui]
scheme = "dark"
EOF

    # The task's working directory is System32 and the shell inherits dtr's, so
    # the `cd` is what opens the session in the fixture.
    cat >"$work/win/shot.cmd" <<'EOF'
@echo off
cd /d C:\fixture
C:\build\dtr.exe --config C:\build\windows.toml --widgets windows --files C:\fixture --shell "powershell -NoLogo -ExecutionPolicy Bypass" --keys "ctrl-alt-right;right;ctrl-alt-left;text:dir -Name;enter;text:$PSVersionTable.PSVersion;enter" --capture C:\build\out\windows.png --capture-after 15000 --exit-after 18000 > C:\build\out\dtr.log 2>&1
echo exit=%errorlevel% >> C:\build\out\dtr.log
EOF

    # ConPTY turns the console title into OSC 2, which is what names the tab;
    # without it the tab reads the powershell.exe path. `-ExecutionPolicy
    # Bypass` is what lets the profile load at all.
    cat >"$work/win/profile.ps1" <<'EOF'
$Host.UI.RawUI.WindowTitle = 'powershell'
EOF

    cat >"$work/win/run.ps1" <<'EOF'
$ErrorActionPreference = 'Stop'

# One of everything the shell has an icon for: two programs, a photo, a zip, a
# text file and two folders. secret.txt is hidden, so whether the pane lists it
# is a fact rather than a preference.
$dir = 'C:\fixture'
if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
New-Item -ItemType Directory -Force $dir | Out-Null
New-Item -ItemType Directory -Force "$dir\projects" | Out-Null
New-Item -ItemType Directory -Force "$dir\receipts" | Out-Null
Copy-Item C:\Windows\System32\notepad.exe $dir
Copy-Item C:\Windows\System32\charmap.exe $dir
Copy-Item C:\Windows\Web\Wallpaper\Windows\img0.jpg "$dir\photo.jpg"
Set-Content -Path "$dir\projects\notes.txt" -Value 'sample'
Compress-Archive -Path "$dir\projects" -DestinationPath "$dir\archive.zip" -Force
Remove-Item "$dir\projects" -Recurse -Force
Set-Content -Path "$dir\notes.txt" -Value 'serial 115200 8N1'
Set-Content -Path "$dir\secret.txt" -Value 'hidden'
(Get-Item "$dir\secret.txt").Attributes = 'Hidden'

# schtasks writes its refusals to stderr and `Stop` would turn "no such task
# yet" into a failure, so every call goes through cmd where the answer is just
# an exit code.
function Schtasks([string] $arguments) {
    cmd /c "schtasks $arguments >nul 2>nul"
    return $LASTEXITCODE
}

$png = 'C:\build\out\windows.png'
if (Test-Path $png) { Remove-Item $png -Force }
if ((Schtasks '/query /tn dtrshot') -eq 0) { Schtasks '/delete /tn dtrshot /f' | Out-Null }
if ((Schtasks '/create /tn dtrshot /tr C:\build\shot.cmd /sc once /st 00:00 /it /f') -ne 0) {
    Write-Output 'could not create the scheduled task'
    exit 1
}
if ((Schtasks '/run /tn dtrshot') -ne 0) {
    Write-Output 'could not run the scheduled task'
    exit 1
}

$deadline = (Get-Date).AddSeconds(60)
while (-not (Test-Path $png) -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 250
}
Schtasks '/delete /tn dtrshot /f' | Out-Null

if (-not (Test-Path $png)) {
    Write-Output 'no capture was written'
    if (Test-Path 'C:\build\out\dtr.log') { Get-Content 'C:\build\out\dtr.log' }
    exit 1
}
Write-Output "captured $((Get-Item $png).Length) bytes"
EOF

    winvm reset
    for _ in $(seq 1 60); do
        winvm ps 'echo up' 2>/dev/null | grep -q up && break
        sleep 2
    done
    winvm ps 'New-Item -ItemType Directory -Force C:\build\out | Out-Null'
    tar -C "$work/win" -cf - dtr.exe windows.toml shot.cmd run.ps1 profile.ps1 |
        winvm ssh "tar -C C:\\build -xf -"

    # A title for the tab, and the pointer parked on the taskbar: an X/Y the
    # window does not cover, so no tile draws as hovered.
    winvm ps '
$d = "C:\Users\win\Documents\WindowsPowerShell"
New-Item -ItemType Directory -Force $d | Out-Null
Copy-Item C:\build\profile.ps1 "$d\profile.ps1" -Force'
    virsh --connect qemu:///system qemu-monitor-command win \
        '{"execute":"input-send-event","arguments":{"events":[{"type":"abs","data":{"axis":"x","value":32510}},{"type":"abs","data":{"axis":"y","value":32354}}]}}' >/dev/null

    winvm ps 'powershell -ExecutionPolicy Bypass -File C:\build\run.ps1'
    winvm ssh "tar -C C:\\build\\out -cf - windows.png" | tar -C "$work/win/out" -xf -
    cp "$work/win/out/windows.png" "$out/windows.png"
    echo "wrote $out/windows.png"

    # Leave the guest as it was found: base, off the disk.
    winvm reset
fi

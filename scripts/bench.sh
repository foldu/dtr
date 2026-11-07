#!/usr/bin/env bash
# Benchmark how fast dtr swallows output, with alacritty's vtebench.
#
#   ./scripts/bench.sh                     # every benchmark, a couple of minutes
#   ./scripts/bench.sh --only sgr,unicode  # just those two
#   ./scripts/bench.sh --profile           # also flame dtr while it runs
#   ./scripts/bench.sh -- --max-samples 5  # extra flags go to vtebench
#   ./scripts/bench.sh --check             # validate the payloads, no window
#   ./scripts/bench.sh --report target/bench/latest.dat   # re-print a run
#
# vtebench measures one thing and its README is honest about it: the speed at
# which the terminal reads its PTY. It writes the payload to *its own* stdout, so
# it has to run inside the terminal being measured — this script starts dtr with
# vtebench as the shell. When vtebench exits, the pane's child is gone, dtr
# closes the tab and the window with it, so the numbers arrive in a `.dat` file
# rather than in a window that has already vanished.
#
# What the number means: the payload is written until the PTY buffer fills, then
# the writer blocks, so a sample is bounded by *whichever* of dtr's read path and
# its draw path is slower. A flood dtr cannot render fast enough shows up here
# too. `--profile` answers "which of the two", and is where to look after a delta
# says something regressed.
#
# Two rules for reading the table:
#   * absolute numbers only compare on one machine, against itself;
#   * the previous run is the baseline, so change one thing at a time.
set -euo pipefail

cd "$(dirname "$0")/.."

# Before anything consumes them: the flake re-exec has to be handed the flags
# this script was called with, not whatever survived argument parsing.
original=("$@")

# Pin vtebench 0.3.1 by revision, so a number from last week means the same
# payload as a number from today. Bump it deliberately, like any dependency.
vtebench_rev=ead80032e57dee2e75f0b51f2ea67528647d9944

usage() {
    cat <<'EOF'
usage: scripts/bench.sh [--check | --report FILE] [--profile] [--only NAMES]
                        [--min-bytes N] [--max-secs N] [-- ARGS...]

  --check         check that the payloads in ./benchmarks produce bytes, then
                  exit. Needs no window, no GPU and no network: the CI form.
  --report FILE   print the table for an already-recorded .dat, then exit.
  --meta DAT [LOG [REV]]
                  (re)write DAT.meta from a run's -v log, then print it. The
                  sidecar says which build, window, adapter and sample size a
                  .dat came from; this is how one is rebuilt for an old run.
                  A log does not carry the build, so REV (or "unknown") is
                  written as given.
  --profile       build the `profiling` profile and pass --cpu-profile, so the
                  run also writes target/bench/profile.svg (Linux only).
  --only NAMES    run just those benchmarks, comma separated, from either set.
  --min-bytes N   payload bytes per sample (default 33554432; vtebench's own
                  default of 1 MiB is far too coarse for a fast terminal,
                  because samples are recorded in whole milliseconds).
  --max-secs N    sampling seconds per benchmark (vtebench default 10).
  -- ARGS         anything else is handed to vtebench, e.g. -- --max-samples 5
EOF
}

mode=run
meta_dat=
meta_log=
meta_rev=
profile=
only=
# vtebench records each sample as whole milliseconds, so the sample has to be
# long enough that a millisecond is noise. dtr drains about a gigabyte a second
# on this machine, so 8 MiB would land on 7 ms and the whole table would be
# rounding: 32 MiB puts a sample near 30 ms, where a percent is visible. The
# report says when a number was still below the resolution.
min_bytes=33554432
max_secs=10
report_file=

while [ $# -gt 0 ]; do
    case "$1" in
        --check) mode=check; shift ;;
        --report) mode=report; report_file=${2:?--report needs a path}; shift 2 ;;
        --meta)
            mode=meta
            meta_dat=${2:?--meta needs a .dat path}
            shift 2
            # Up to two operands after the DAT, and neither is a flag.
            for _ in 1 2; do
                case "${1:-}" in
                    '' | -*) break ;;
                    *)
                        if [ -z "$meta_log" ]; then meta_log=$1
                        elif [ -z "$meta_rev" ]; then meta_rev=$1
                        fi
                        shift
                        ;;
                esac
            done
            ;;
        --report=*) mode=report; report_file=${1#--report=}; shift ;;
        --profile) profile=1; shift ;;
        --only) only=${2:-}; shift 2 ;;
        --only=*) only=${1#--only=}; shift ;;
        --min-bytes) min_bytes=${2:?--min-bytes needs a number}; shift 2 ;;
        --min-bytes=*) min_bytes=${1#--min-bytes=}; shift ;;
        --max-secs) max_secs=${2:?--max-secs needs a number}; shift 2 ;;
        --max-secs=*) max_secs=${1#--max-secs=}; shift ;;
        --) shift; break ;;
        -h | --help) usage; exit 0 ;;
        *) echo "bench: unknown flag $1" >&2; usage >&2; exit 2 ;;
    esac
done
extra=("$@")

# ---------------------------------------------------------------------------
# Report. The .dat is one column per benchmark, one row per sample.
# ---------------------------------------------------------------------------

# name, sample count, median milliseconds. Insertion sort because there are a
# handful of samples and awk has no sort of its own.
medians() {
    awk '
        NR == 1 { count = NF; for (i = 1; i <= NF; i++) name[i] = $i; next }
        {
            for (i = 1; i <= NF && i <= count; i++)
                if ($i != "_") { n[i]++; value[i, n[i]] = $i + 0 }
        }
        END {
            for (i = 1; i <= count; i++) {
                k = n[i]
                if (k == 0) { printf "%s\t0\t0.00\n", name[i]; continue }
                for (j = 1; j <= k; j++) s[j] = value[i, j]
                for (j = 2; j <= k; j++) {
                    x = s[j]; m = j - 1
                    while (m > 0 && s[m] > x) { s[m + 1] = s[m]; m-- }
                    s[m + 1] = x
                }
                med = (k % 2) ? s[(k + 1) / 2] : (s[k / 2] + s[k / 2 + 1]) / 2
                printf "%s\t%d\t%.2f\n", name[i], k, med
            }
        }' "$1"
}

# sample_size DAT [FALLBACK] — bytes per sample, from the sidecar the run writes.
#
# Without it the delta compares milliseconds across runs that may have used
# different sample sizes, which reads as a 4× regression the moment somebody
# changes --min-bytes. The sidecar is one number next to the .dat.
sample_size() {
    local dat=$1 fallback=${2:-}
    if [ -f "$dat.size" ]; then
        head -1 "$dat.size"
    else
        printf '%s' "$fallback"
    fi
}

# ---------------------------------------------------------------------------
# What a run was, next to the numbers it produced.
#
# A `.dat` is one column per benchmark and one row per sample — vtebench's
# format, and it says nothing about the build, the window or the sample size.
# That is how a comparison once mixed a tiled 1908x2084 window with a 900x620
# one and read as a 40% regression, and how a run that was OOM-killed mid-table
# looked like a fast one. So every run writes `<dat>.meta`, and the table header
# prints the parts that decide whether two runs can be compared at all.
# ---------------------------------------------------------------------------

# git_stamp — the commit this binary was built from, `-dirty` included.
#
# Not the reason a number differs, but the first question anyone asks about a
# screenshot of a benchmark table.
git_stamp() {
    local rev
    rev=$(git rev-parse --short HEAD 2>/dev/null || echo unknown)
    if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
        printf '%s-dirty' "$rev"
    else
        printf '%s' "$rev"
    fi
}

# field LOG KEY DEFAULT — what dtr's `-v` log said about one thing.
#
# The log is where the window size, the DPI factor and the adapter actually
# live; nothing else knows them. ANSI is stripped because tracing colours its
# output even into a file.
field() {
    local log=$1 pattern=$2 default=${3:-unknown} value
    value=$(sed 's/\x1b\[[0-9;]*m//g' "$log" 2>/dev/null \
        | grep -oE "$pattern" | tail -1)
    printf '%s' "${value:-$default}"
}

# write_meta DAT LOG — the sidecar, and it has to work for a run that died.
write_meta() {
    local dat=$1 log=$2 rev=${3:-} backfilled=${4:-0} window scale adapter cols rows cells
    window=$(field "$log" 'width=[0-9]+ height=[0-9]+' | sed 's/width=//; s/ height=/x/')
    scale=$(field "$log" 'strip geometry scale=[0-9.]+' | sed 's/.*scale=//')
    adapter=$(field "$log" 'adapter=[^ ]*( [^ ]+)* backend=' | sed 's/^adapter=//; s/ backend=$//')
    cols=$(field "$log" 'cols=[0-9]+ rows=[0-9]+' | sed 's/ rows=.*//; s/cols=//')
    rows=$(field "$log" 'cols=[0-9]+ rows=[0-9]+' | sed 's/.*rows=//')
    if [ -n "$cols" ] && [ -n "$rows" ] && [ "$cols" != unknown ]; then
        cells=$((cols * rows))
    else
        cells=unknown
        cols=unknown
        rows=unknown
    fi
    cat >"$dat.meta" <<EOF
rev=${rev:-unknown}
backfilled=$backfilled
dirty=$( [ -n "$(git status --porcelain 2>/dev/null)" ] && echo 1 || echo 0 )
version=$(grep -m1 '^version' Cargo.toml 2>/dev/null | sed 's/.*= *"//; s/".*//' || echo unknown)
profile=$( [ -n "$profile" ] && echo profiling || echo release )
vtebench_rev=$vtebench_rev
host=$(uname -n)
date=$(date -Is)
bytes=${min_bytes}
adapter=${adapter}
window=${window}
scale=$scale
cols=$cols
rows=$rows
cells=$cells
EOF
}

# meta_value META KEY — one field, or nothing if there is no sidecar.
meta_value() {
    [ -f "$1" ] || return 0
    sed -n "s/^$2=//p" "$1" | head -1
}

# describe DAT — one line saying what produced the numbers.
describe() {
    local dat=$1 meta=$1.meta bytes
    if [ ! -f "$meta" ]; then
        printf 'unknown build, no .meta beside the .dat'
        return
    fi
    bytes=$(sample_size "$dat" "$min_bytes")
    printf '%s, %s (%s), window %s at %sx, pane %sx%s, %s MiB samples' \
        "$(meta_value "$meta" rev)" \
        "$(meta_value "$meta" adapter)" \
        "$(meta_value "$meta" profile)" \
        "$(meta_value "$meta" window)" \
        "$(meta_value "$meta" scale)" \
        "$(meta_value "$meta" cols)" \
        "$(meta_value "$meta" rows)" \
        "$(awk -v b="$bytes" 'BEGIN { printf "%.0f", b / 1048576 }')"
}

# report DAT [PREVIOUS]
report() {
    local dat=$1 previous=${2:-} bytes prev_bytes mib prev_mib prev_rows unmeasured

    bytes=$(sample_size "$dat" "$min_bytes")
    mib=$(awk -v b="$bytes" 'BEGIN { printf "%.4f", b / 1048576 }')
    prev_bytes=$(sample_size "$previous")
    prev_mib=$(awk -v b="$prev_bytes" 'BEGIN { printf "%.4f", b / 1048576 }')
    prev_rows=""
    if [ -n "$previous" ] && [ -s "$previous" ]; then
        prev_rows=$(medians "$previous")
    fi

    printf 'bench: %-9s %s\n' "this run" "$(describe "$dat")"
    if [ -n "$previous" ] && [ -s "$previous" ]; then
        printf 'bench: %-9s %s\n' "previous" "$(describe "$previous")"
    fi

    printf '%-30s %6s %11s %8s %7s\n' benchmark runs median MiB/s vs-prev
    awk -F'\t' -v mib="$mib" -v prev_mib="$prev_mib" -v previous="$prev_rows" '
        BEGIN {
            # The previous run arrives as tab-separated rows in a variable, so
            # that stdin stays the current run.
            n = split(previous, rows, "\n")
            for (i = 1; i <= n; i++) {
                split(rows[i], f, "\t")
                if (f[1] != "" && f[3] > 0) prev[f[1]] = f[3] + 0
            }
        }
        {
            if ($3 > 0) {
                med = sprintf("%8.2f ms", $3)
                rate = mib * 1000 / $3
                rate_text = sprintf("%8.1f", rate)
            } else {
                # Below what vtebench can record, so not a rate at all.
                med = sprintf("%8s ms", "<1")
                rate = 0
                rate_text = sprintf("%8s", "-")
            }
            # Rates, not milliseconds: a run with a different sample size is a
            # different amount of work, and the only comparable number is how
            # fast the bytes went in.
            delta = "-"
            if (($1 in prev) && rate > 0 && prev_mib > 0) {
                was = prev_mib * 1000 / prev[$1]
                delta = sprintf("%+.1f%%", (rate - was) * 100 / was)
            }
            printf "%-30s %6d %s %s %7s\n", $1, $2, med, rate_text, delta
        }' <(medians "$dat")

    # A zero median is the measurement failing, not a fast terminal.
    unmeasured=$(medians "$dat" | awk -F'\t' '$3 < 0.005 { n++ } END { print n + 0 }')
    if [ "$unmeasured" -gt 0 ]; then
        echo "bench: $unmeasured benchmark(s) finished a sample under a millisecond —" >&2
        echo "bench: vtebench records whole milliseconds, so raise --min-bytes and rerun" >&2
    fi
    if [ -n "$previous" ] && [ -s "$previous" ] && [ -z "$prev_bytes" ]; then
        echo "bench: the previous run did not record its sample size, so no deltas —" >&2
        echo "bench: they compare rates, and two sizes are not comparable" >&2
    fi
    local cells prev_cells
    cells=$(meta_value "$dat.meta" cells)
    prev_cells=$(meta_value "$previous.meta" cells)
    if [ -n "$cells" ] && [ -n "$prev_cells" ] && [ "$cells" != "$prev_cells" ]; then
        echo "bench: the previous run had a different pane ($prev_cells cells against" >&2
        echo "bench: $cells), so the delta mixes a window change with a code change" >&2
    fi
}

# ---------------------------------------------------------------------------
# Payload check. Deliberately first: it must work where a window cannot open.
# ---------------------------------------------------------------------------
check() {
    local dir bytes name failed=0

    for dir in benchmarks/*/; do
        name=$(basename "$dir")
        if [ ! -f "$dir/benchmark" ]; then
            echo "bench: $name has no benchmark script" >&2
            failed=1
            continue
        fi
        if ! sh -n "$dir/benchmark"; then
            echo "bench: $name/benchmark does not parse" >&2
            failed=1
            continue
        fi

        # Under a pty, because the scripts ask the terminal for its size. With
        # no `script` binary they fall back to 80x24 and still produce bytes.
        if command -v script >/dev/null 2>&1; then
            bytes=$({ script -qec "sh $dir/benchmark" /dev/null || true; } | wc -c)
        else
            bytes=$({ sh "$dir/benchmark" || true; } | wc -c)
        fi

        if [ "$bytes" -le 0 ]; then
            echo "bench: $name/benchmark produced no bytes" >&2
            failed=1
            continue
        fi
        printf 'bench: %-22s %8d bytes\n' "$name" "$bytes"
    done

    [ "$failed" -eq 0 ] || return 1
    echo "bench: payloads ok"
}

if [ "$mode" = check ]; then
    check
    exit $?
fi

if [ "$mode" = report ]; then
    if [ ! -s "$report_file" ]; then
        echo "bench: no results in $report_file" >&2
        exit 1
    fi
    report "$report_file" "${DTR_BENCH_OUT:-target/bench}/previous.dat"
    exit 0
fi

if [ "$mode" = meta ]; then
    # Rebuilding a sidecar needs the log and nothing else: no flake, no build,
    # no window, which is what makes it usable on a run from last week.
    [ -f "$meta_log" ] || meta_log=${DTR_BENCH_OUT:-target/bench}/stderr.log
    if [ ! -f "$meta_log" ]; then
        echo "bench: no log at $meta_log; pass one, or keep the one the run wrote" >&2
        exit 1
    fi
    # A log does not record the build it came from, so this is `unknown` unless
    # the caller knows better: a sidecar that guesses is worse than one that
    # admits it, because the guess is what somebody compares against.
    write_meta "$meta_dat" "$meta_log" "$meta_rev" 1
    cat "$meta_dat.meta"
    exit 0
fi

# ---------------------------------------------------------------------------
# Run. Everything below needs the toolchain, so it may re-exec in the flake.
# ---------------------------------------------------------------------------
if ! command -v pkg-config >/dev/null 2>&1; then
    exec nix develop .#default --command "$PWD/scripts/bench.sh" "${original[@]}"
fi

export PKG_CONFIG_PATH
PKG_CONFIG_PATH="$(nix build .#libghostty-vt.dev --no-link --print-out-paths)/share/pkgconfig"
PKG_CONFIG_PATH+=":$(nix build nixpkgs#udev.dev --no-link --print-out-paths)/lib/pkgconfig"
PKG_CONFIG_PATH+=":$(nix build nixpkgs#fontconfig.dev --no-link --print-out-paths)/lib/pkgconfig"
PKG_CONFIG_PATH+=":$(nix build nixpkgs#freetype.dev --no-link --print-out-paths)/lib/pkgconfig"

out_dir=${DTR_BENCH_OUT:-target/bench}
mkdir -p "$out_dir"

# The suite is checked in; the runner is not. Cloned rather than installed with
# `cargo install --git`, because the default payloads live in the same tree as
# the binary. It lives in the cache rather than in `target/` so that `cargo
# clean` does not throw away a checkout, and because a package inside this
# repository's tree that is not a workspace member confuses cargo.
bench_cache=${XDG_CACHE_HOME:-$HOME/.cache}/dtr-bench
mkdir -p "$bench_cache"
vtebench_src=$bench_cache/vtebench-src
vtebench_root="$bench_cache/vtebench-$vtebench_rev"
vtebench_bin="$vtebench_root/bin/vtebench"

if [ ! -d "$vtebench_src/.git" ]; then
    echo "bench: fetching vtebench $vtebench_rev" >&2
    git clone --quiet https://github.com/alacritty/vtebench "$vtebench_src"
fi
if [ "$(git -C "$vtebench_src" rev-parse HEAD)" != "$vtebench_rev" ]; then
    git -C "$vtebench_src" fetch --quiet origin "$vtebench_rev" 2>/dev/null ||
        git -C "$vtebench_src" fetch --quiet origin
    git -C "$vtebench_src" checkout --quiet "$vtebench_rev"
fi
if [ ! -x "$vtebench_bin" ]; then
    echo "bench: building vtebench" >&2
    cargo install --quiet --locked --path "$vtebench_src" --root "$vtebench_root" --bin vtebench
fi

# dtr optimised the way a release is, or with symbols when we are profiling.
if [ -n "$profile" ]; then
    cargo build --quiet --profile profiling -p dtr
    bin=target/profiling/dtr
else
    cargo build --quiet --release -p dtr
    bin=target/release/dtr
fi

benchmark_dirs=("$vtebench_src/benchmarks" "$PWD/benchmarks")
select_args=()
if [ -n "$only" ]; then
    # vtebench selects directories, not names, so a directory of links is how a
    # name becomes a selection (`find_benchmarks` follows links).
    only_dir="$out_dir/only"
    rm -rf "$only_dir"
    mkdir -p "$only_dir"
    for name in ${only//,/ }; do
        for set in "${benchmark_dirs[@]}"; do
            if [ -d "$set/$name" ]; then
                ln -s "$(cd "$set/$name" && pwd)" "$only_dir/$name"
            fi
        done
        if [ ! -e "$only_dir/$name" ]; then
            echo "bench: no benchmark named $name" >&2
            exit 2
        fi
    done
    benchmark_dirs=("$only_dir")
fi
for set in "${benchmark_dirs[@]}"; do
    select_args+=(--benchmarks "$set")
done
if [ -n "$only" ]; then
    # `find` does not descend through the links, and vtebench does.
    count=$(find "$only_dir" -maxdepth 1 -type l | wc -l)
else
    count=$(find "${benchmark_dirs[@]}" -maxdepth 2 -name benchmark | wc -l)
fi

# Absolute, because the child inherits whatever directory the window was opened
# from, not this script's.
dat="$PWD/$out_dir/latest.dat"
previous="$PWD/$out_dir/previous.dat"
if [ -f "$dat" ]; then
    mv "$dat" "$previous"
    # The sample size travels with the results: the delta is a rate, and a rate
    # cannot be compared across two different amounts of work.
    [ -f "$dat.size" ] && mv "$dat.size" "$previous.size"
fi

shell_cmd="$vtebench_bin --dat $dat ${select_args[*]} --min-bytes $min_bytes --max-secs $max_secs"
if [ ${#extra[@]} -gt 0 ]; then
    shell_cmd+=" ${extra[*]}"
fi

# vtebench reports on the way out, and the way out closes the window. The guard
# deadline means a hung payload cannot leave a window running all night, and it
# is generous enough that a slow machine still finishes with numbers.
guard_s=$((count * (max_secs + 5) + 90))

dtr_args=(--shell "$shell_cmd" --exit-after "$((guard_s * 1000))" -v)
if [ -n "$profile" ]; then
    dtr_args+=(--cpu-profile "$PWD/$out_dir/profile.svg")
fi

echo "bench: $count benchmarks, at most ${guard_s}s" >&2
echo "bench: $bin --shell \"$shell_cmd\"" >&2

# dtr's exit status is checked last, not first: a run that produced samples is
# evidence even when the process was killed afterwards — an OOM, a closed
# terminal — and the old order threw that evidence away, which is how one
# benchmark session produced three tables that were never written.
status=0
"$bin" "${dtr_args[@]}" 2>"$out_dir/stderr.log" || status=$?

if [ ! -s "$dat" ]; then
    if grep -qi 'wayland\|x11\|display\|surface' "$out_dir/stderr.log"; then
        echo "bench: dtr could not open a window — run this in the graphical session" >&2
    fi
    echo "bench: vtebench wrote no results; dtr's log:" >&2
    tail -20 "$out_dir/stderr.log" >&2
    exit 1
fi

# Both sidecars are written before anything can fail, because they describe a
# run that has already happened: the sample size the rates assume, and what the
# window, the adapter and the build were.
printf '%s\n' "$min_bytes" > "$dat.size"
write_meta "$dat" "$out_dir/stderr.log" "$(git_stamp)" 0
report "$dat" "$previous" | tee "$out_dir/summary.txt"

if [ "$status" -ne 0 ]; then
    echo >&2
    echo "bench: dtr exited $status after vtebench finished; the table above is its own" >&2
    tail -3 "$out_dir/stderr.log" >&2
fi

echo >&2
echo "bench: dat      $dat" >&2
echo "bench: previous $previous" >&2
if [ -n "$profile" ]; then
    echo "bench: profile  $out_dir/profile.svg" >&2
fi
echo "bench: + is faster and - slower than the last run; same machine only" >&2

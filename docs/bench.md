# Benchmarking dtr

`./scripts/bench.sh` runs [vtebench][vtebench] — Alacritty's PTY-read benchmark —
against dtr, and prints a table with the delta from the previous run. This file is
how to read that table and what a `.dat` is worth; the profiling half of the same
work (`--cpu-profile`, `scripts/flamegraph.sh`) is still in `AGENTS.md`.

[vtebench]: https://github.com/alacritty/vtebench

## What it measures

vtebench writes a payload to its own stdout, so it has to run *inside* the terminal
being measured: the script starts dtr with `vtebench` as the shell and the numbers
arrive in a `.dat` file when the payload is done. It measures one thing, as its
README says — how fast the terminal reads its PTY. The payload is written until the
PTY buffer fills and then the writer blocks, so **a sample is bounded by whichever is
slower, dtr's read path or its draw path.** A renderer that cannot keep up shows up
here too; `--profile` is what separates the two.

## Reading the table

- **Absolute numbers only mean anything on one machine against itself.** Compare
  runs on the same host, in the same session, with the same window.
- **The previous run is the baseline**, so bench one change at a time and rebuild
  first.
- **A delta across a different window is not a delta.** Every number is per byte of
  payload, and a byte drawn into 11 826 cells is not the same work as a byte drawn
  into 3 400. This is not hypothetical: a run at 1908 × 2084 (scale 1.5, tiled by the
  window manager) against one at 900 × 620 read as a 40 % regression that was a
  window. The table now prints both runs' windows and warns when the pane differs.
- **Use `--only` to keep it short.** `./scripts/bench.sh --only wide_cells,unicode`
  is a minute; the full suite is a couple of minutes per run.
- vtebench records whole milliseconds, so a fast terminal quantises to 2 or 3 ms.
  The default is 32 MiB per sample, which puts a sample near 30 ms where a percent is
  visible. **A median under 1 ms is a failed measurement**, not a fast terminal —
  raise `--min-bytes`.

## What a run leaves behind

Next to `<dat>`:

| file | what |
| --- | --- |
| `<dat>` | the samples: one column per benchmark, one row per sample |
| `<dat>.size` | bytes per sample, because a rate cannot be compared across two different amounts of work |
| `<dat>.meta` | `key=value` lines describing the run |

`<dat>.meta` is written from dtr's own `-v` log plus git, so it survives a run that
was killed after vtebench finished:

```
rev=345eb2d
backfilled=0
dirty=1
version=0.1.0
profile=release
vtebench_rev=ead80032e57dee2e75f0b51f2ea67528647d9944
host=jupiter
date=2026-11-07T16:29:17+01:00
bytes=33554432
adapter=AMD Radeon RX 9070 XT (RADV GFX1201)
window=1908x2084
scale=1.5
cols=146
rows=81
cells=11826
```

`adapter` is the one to look at first when a number moved and the code did not:
`lavapipe` means software Vulkan (no `/dev/dri`, or CI), and every sample is then
bounded by a software rasteriser.

`--meta DAT [LOG [REV]]` rebuilds a sidecar from a log you kept, which is how an old
`.dat` gets its description back:

```sh
./scripts/bench.sh --meta target/bench/latest.dat target/bench/stderr.log 345eb2d
./scripts/bench.sh --report target/bench/latest.dat
```

A log does not carry the build that wrote it, so a rebuilt sidecar has
`backfilled=1` and `rev=unknown` unless you pass the rev — a sidecar that guessed
would be worse than one that admits it, because the guess is what somebody compares
against.

## The two payloads that are ours

- `wide_cells` — the double-width path: CJK and everything else that takes two cells.
- `cwd_reports` — the OSC 7 sniffer, which sits on every byte of every session.

`./scripts/bench.sh --check` validates them without a window, a GPU or a network,
and that is the form CI runs.

## Running it where no window opens

The full run needs a `Window` and a surface, so it wants a graphical session. The
*reader* can still be measured headlessly: `--screenshot PATH --screenshot-delay MS`
spawns the session and pumps it with nothing on screen, which is enough for a payload
to be timed — only the final image needs an adapter, and a software Vulkan ICD
(`WGPU_BACKEND=vulkan`, lavapipe) is enough for that.

For the live window on a machine with no display, and for forcing a DPI factor, see
the Xvfb recipe in `AGENTS.md`.

## A number is only as good as its memory

The first benchmark sessions on this machine produced no usable table at all, because
dtr was being OOM-killed mid-run: the VT core's default is *unlimited* scrollback, and
the flood grew the process to 50 GB. That is fixed — `pane::new_terminal` sets a
100 MB limit from `config.terminal.scrollback_limit` — but the lesson belongs here:
**check `dmesg`/`journalctl -k` for `Out of memory: Killed process … (dtr)` before
believing a truncated table.** A benchmark that stops producing samples halfway
through a column is not a slow benchmark, it is a dead one.

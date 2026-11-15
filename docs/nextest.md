# nextest for the inner loop — decision memo

Status: measured 2026-11-15, taken for `just test`; the sandbox gate keeps `cargo test`.
Scope: whether `cargo nextest` is worth adding, what it actually saves on this suite, and what
it silently stops running. The answer to "does it speed tests up" is *by about half a second,
and only because one binary no longer blocks the other 24.*

Measurements on a 32-core machine inside `nix develop .#default`, `target/` warm, lavapipe for
the kit render tests, three runs each (the spread was under 20 ms, so one number is enough):

| command | tests | wall |
| --- | ---: | ---: |
| `cargo test --workspace` | 881 | 4.50 s |
| `cargo nextest run --workspace` | 880 | 3.35 s |
| `cargo test --workspace --lib --bins --tests` | 880 | 3.86 s |
| `cargo test --workspace --doc` | 1 | 0.75 s |
| `just test` (nextest, then `--doc`) | 881 | ≈ 4.1 s |

## The 25 % headline is mostly a gap

`cargo test --workspace` runs 881 tests because it compiles and runs the doctest binary of every
crate; nextest runs 880 because **it cannot run doctests at all**. The single real doctest
(`dtr-sshconfig`'s `parse_str` example) and the ten empty doctest binaries account for the
3.86 s → 4.50 s, more than half of the 1.15 s difference (the table's 0.75 s is that pass on its
own, paying cargo's startup again). Like for like — same 880 tests — it is 3.86 s → 3.35 s:
**0.51 s, 13 %**. `just test` keeps the doctest pass, so what the inner loop saves is **about
0.4 s, 9 %**.

That is the honest number, and it is worth having, but it is not what "nextest is faster"
usually means.

## Why only 13 %: one binary is the suite

The suite is 25 test binaries and 880 tests in 3.86 s, but one binary — `dtr`'s lib unit tests,
403 tests — takes 3.08 s of that. Inside it, one test takes 3.03 s:

```text
PASS [   3.028s] (880/880) dtr pane::session_tests::a_synchronized_output_hold_hides_the_half_drawn_frame
```

`a_synchronized_output_hold_hides_the_half_drawn_frame` is a real-time test of `CSI ? 2026 h`:
the script sleeps 1 s, holds, prints, sleeps 2 s, releases, and the test sleeps 1.2 s then polls
for a frame for another second. Its 3 s are the *point* — the assertion is that no frame is
published inside a window that only exists in wall-clock time — so the sleeps cannot be
parallelised away, only shortened (which would make the window easier to miss on a loaded
runner).

`cargo test` runs binaries **one after another**, so the other 24 binaries wait for those 3 s.
nextest schedules every binary's tests together, so the 0.5 s of small suites runs *inside* the
3 s test, and the whole run ends 0.3 s after it does. That is the entire mechanism:

- `cargo test`: 3.86 s = 3.08 s long pole + 0.78 s of everything else, serially.
- `cargo nextest run`: 3.35 s = 3.03 s long pole + 0.32 s of scheduling, listing and 880
  process spawns.

**nextest's run is now within 0.3 s of the slowest test**, so the runner has stopped being the
thing to fix. The next second is in that one test's sleeps, and shortening them trades CI
reliability for wall clock — worth doing only with a reason to trust the runner's load.

## What it buys besides the half second

- **One process per test.** No shared static or environment between tests, and a pty or a
  `set_var` in one test cannot leak into the next. This suite has no test that needed
  `--test-threads=1`, so nothing changes today; it is the property that keeps it that way.
- **`--retries N`** for the sleep-based timing tests (`a_connection_that_drops_is_reopened_in_
  the_same_pane` and friends), instead of a rewrite or an `#[ignore]`.
- **`--no-fail-fast`** to get cargo's run-everything behaviour; nextest stops at the first
  failure by default, which is the right default for an inner loop and not for a CI log.
- **A duration on every line**, so the 3 s test is visible without guessing from a binary's
  total, and `--slow-timeout` if a threshold is wanted later.
- **`cargo nextest list`** is a stable test inventory, which is what a "did a test disappear"
  check would need — there is none today.

## What it costs

- **Doctests are not run.** `just test` runs `cargo test --workspace --doc` after nextest so a
  doctest break is still caught; a bare `cargo nextest run` does not.
- **Fail-fast by default**, as above.
- **Nothing to learn for CI**: `cargo nextest run` builds the test binaries itself, so the
  command in the justfile is the whole workflow.

## Why CI keeps `cargo test`

`checks.tests` is `craneLib.cargoTest`, and it stays that way:

- A cold `nix flake check` spends minutes compiling and 4.5 s running. Saving 0.5 s of a cold
  run is noise, and it is the *only* thing nextest changes — it does not compile faster.
- Switching would either drop the doctest from CI or add a second derivation that compiles the
  same workspace test binaries again to run one doctest, which costs far more than it saves.

The half second exists only in the warm inner loop, which is exactly where `just test` now uses
it. `just snapshots` stays `cargo test` with `UPDATE_SNAPSHOTS=1`: the baseline writer is chosen
for its rasteriser, not its runner, and regenerating a baseline is not a timed operation.

# The Tera Term macro corpus

Fifty-three real TTL macros, vendored from upstream Tera Term; provenance and
licence are in [NOTICE](NOTICE). Most are named after the issue that produced
them (`#31050.ttl`, `#36405-strreplace.ttl`), because that is what they are: the
reproduction attached to a bug report, kept after the fix so it cannot come back.

## What they are for here

Upstream has no automated suite — no runner in the repository, no CI job that
executes any of this, and two thirds of the files end in `messagebox` so that a
person can read the answer against the expected value in the comment above.
That makes the corpus a **checklist**, not a test suite, but the macros in it are
real TTL written for Tera Term, and that is exactly what this crate needs to be
measured against.

`tests/corpus.rs` therefore asserts the part that can be asserted without
executing anything:

* every file **decodes** (BOM, UTF-8, or the ANSI fallback of manual §3.1 — 15 of
  these files are CP932 and two are UTF-16, so the fallback is load-bearing);
* every line **lexes** (§3.2–§3.7: strings, `#`/`#$` character codes, `$`
  hexadecimal, labels, brackets, comments including `/* */` across lines);
* every line is **one of the four kinds** (§3.2);
* every block is **closed by its own keyword** (§6: `if`/`endif`,
  `for`/`next`, `while`/`endwhile`, `until`/`enduntil`, `do`/`loop`).

It also pins the totals — 53 files, 2 485 lines, 648 blank, 48 labels, 168
assignments, 1 621 commands — so a scanner change that silently stops seeing
something fails rather than passing quietly.

What it does **not** assert is that the macros run. That needs the evaluator, and
the point of keeping the expected values is that they are already written down
in the files for when it exists: a harness that makes `messagebox` record its
arguments instead of showing them can compare against them without editing a
single macro.

## Worth knowing before reading one

* **They are not all ASCII.** 15 are CP932, one UTF-16BE, one UTF-16LE.
* **They assume Windows.** `#31050.ttl` uses `z:\tmpa.txt`; several expect
  Tera Term's dialogs; `gui_commands_test.ttl` is a manual checklist for the
  settings UI.
* **Some need a live session.** `waitn.ttl` sends `ls` and `ps ax`; the
  `setserialdelay_*.ttl` pair wants a serial port.
* **A few are already self-checking.** `#36405-strreplace.ttl` counts errors and
  exits non-zero; `#41952-gettime.ttl` takes an `exitcode_only` parameter so the
  dialog can be suppressed — and inverts the convention (exit 0 means "has
  problem"), so read it before copying it.

## The ones that will matter most

| file | what it pins |
| --- | --- |
| `array.ttl` | array bounds, and a `strdim` of 100 000 that must fail (§4.3) |
| `#36405-strreplace.ttl` | `strreplace` result codes 1 / 0 / −1 for a bad pattern |
| `strspecial.ttl` | `strspecial` expanding `\n`, and the two error cases |
| `sprintf.ttl`, `sprintf2.ttl` | the `sprintf` result codes and `inputstr` |
| `oneline-if.ttl` | the single-line `if <expression> <statement>` form (§6.1) |
| `ifdefined.ttl` | the type codes `ifdefined` reports (0, 1, 3, 4, 5, 6) |
| `params_array.ttl`, `macroparam.ttl` | `params` / `param1`…`param9` / `paramcnt` (§2.3), driven by the `.bat` wrappers upstream keeps beside them |
| `code_*.ttl` | the four encoding cases of §3.1 |
| `test_file.ttl` | most of the file commands in one script |
| `#35822-random.ttl` | `random`, and therefore something no golden file can pin |

## Re-vendoring

```sh
git clone --depth 1 --filter=blob:none --sparse https://github.com/TeraTermProject/teraterm.git
cd teraterm && git sparse-checkout set tests
git rev-parse HEAD                      # record this in NOTICE
cp tests/*.ttl <this directory>/
```

Then update the pinned totals in `tests/corpus.rs` — that is what they are for.

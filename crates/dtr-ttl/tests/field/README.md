# Real macros go here

This directory is empty on purpose. It is where **macros somebody actually runs**
go — not upstream's reproductions (those are in `../corpus/`), but the scripts
from a bench rig, a firmware flash, a login sequence, a serial device that only
works when you talk to it in the right order.

The corpus proves the interpreter survives real TTL syntax. It cannot prove the
interpreter does what a working script needs, because nobody's job depends on
those 53 files. Ten macros with a purpose would be worth more than all of them.

## What to drop here

- The `.ttl` file, unedited — if it needs a tweak to run under dtr, that is a bug
  in dtr, and the original is what the bug report needs.
- A one-line note in the commit message or beside it: what the macro is for,
  what it talks to, and which dialog or file is the thing to look at when it
  runs.
- If it needs parameters, what they are.
- If it needs a device or a host that is not here, say so — it can still run
  against a recording host that answers for the device, and the expected dialogs
  are the assertion.

## What happens to them

Nothing yet. When there are some, a test beside `../run_corpus.rs` will run them
the same way the corpus is run — a `RecordingHost`, dialogs as assertions — and
`docs/ttl-gaps.md` is where the list of what they expose belongs.

Until then, the measure of "works" is §7 of `../../MANUAL.md`, and
`docs/ttl-gaps.md` says where that falls short.

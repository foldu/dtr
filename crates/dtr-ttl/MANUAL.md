# The Tera Term Language

TTL is the macro language of [Tera Term](https://teratermproject.github.io/): a
line-oriented scripting language
that drives exactly one terminal session from a text file. It exists to automate
the things a serial terminal is used for — dial a port, wait for a prompt, send
a command, pull the answer apart, write it to a log, and report success or
failure through an exit status.

This document is a distillation of the Tera Term 5 macro manual at
<https://teratermproject.github.io/manual/5/en/macro/>, retrieved 2026-11-01.
It covers Tera Term 5.8-era TTL. It is written to be sufficient on its own: an
interpreter can be built from it, and a person can write a macro from it without
consulting the upstream HTML.

Compared with the upstream manual this one:

- states the language once, in order, instead of across eight syntax chapters;
- keeps every version gate, so a construct's availability is never a guess;
- folds the per-command "see also" lists into cross-references inside the
  entries, and drops repeated boilerplate about Tera Term's window layout;
- adds no features. Where the upstream manual is silent, so is this one, and
  the entry says so rather than inventing a rule.

Tera Term's own vocabulary leaks through on purpose. A macro does not *use* a
terminal, it *links* to one; it does not read a device, it reads whatever that
terminal is connected to. The manual keeps those words because a macro written
for one interpreter has to run under the other, and semantics are decided by the
nouns.

## Contents

- [1. The execution model](#1-the-execution-model)
- [2. Starting a macro](#2-starting-a-macro)
- [3. Lexical structure](#3-lexical-structure)
- [4. Types and variables](#4-types-and-variables)
- [5. Expressions](#5-expressions)
- [6. Statements](#6-statements)
- [7. Command reference](#7-command-reference)
  - [7.1 Communication](#71-communication)
  - [7.2 Control](#72-control)
  - [7.3 Strings](#73-strings)
  - [7.4 Files](#74-files)
  - [7.5 Passwords](#75-passwords)
  - [7.6 Miscellaneous](#76-miscellaneous)
- [8. Errors and exit status](#8-errors-and-exit-status)
- [9. Appendices](#9-appendices)

---

## 1. The execution model

### 1.1 Two processes, one link

TTL is not embedded in the terminal. It is a separate interpreter,
`TTPMACRO.EXE`, which controls a running Tera Term through a link. Everything
unusual about the language follows from that split:

- **A macro has no terminal state of its own.** Screen contents, the connection,
  the log file and the serial port all belong to Tera Term; the macro sends
  requests and reads back the answers. `clearscreen` is not a macro feature, it
  is a message to the other process.
- **The link is a resource that must exist before anything else works.** Every
  communication command except `connect`, `cygconnect` and `testlink` fails with
  *"Link macro first. Use 'connect' macro."* if it is missing. `testlink` is what
  asks whether it is there.
- **One macro, one terminal.** A macro links to a single Tera Term instance. A
  second instance is a second macro (and is reachable only through the
  broadcast commands, which send to all of them).
- **The macro outlives the connection.** Dropping the connection does not end the
  macro, and ending the macro does not close Tera Term unless the macro says so
  (`closett`).

### 1.2 The three states of a macro

`testlink` is the command that names the states, and its two outcomes are the
ones a macro actually branches on:

| State | Meaning | `testlink` sets `result` to |
| --- | --- | --- |
| Unlinked | No Tera Term has been linked. Nothing that talks to a session can run. | 0 |
| Linked, disconnected | A terminal exists and is linked, but the connection is down. | 1 |
| Linked and connected | Both are up. | 2 |

`connect` walks all three: with no link it starts Tera Term and links it; linked
but disconnected, it dials; already connected, it does nothing (it does **not**
reconnect — see §2.4 for the pattern that does).

### 1.3 Interpretation

A macro file is loaded into memory in its entirety and then interpreted; there
is no compile step and no separate bytecode. The single exception is `include`,
which reads the named file when it reaches it. Two consequences are worth
knowing: a syntax error anywhere in the file is reported when execution reaches
it, not at startup; and an `include` of a file on a network share can fail
mid-run because the share went away.

### 1.4 How a macro ends

| Ending | Effect |
| --- | --- |
| `end` | Stops the run. The MACRO window closes with it. |
| `exit` | In an `include`d file: returns to the including file. In the main file: equivalent to `end`. |
| Falling off the end of the main file | Ends the run; the manual does not distinguish this from `end`. |
| Any error | The macro stops and reports the message; the run is a failure. |
| `closett` | Closes Tera Term and leaves the macro *running* but unlinked, so `connect` can open a new window. |

`end` and `exit` are therefore not the same command, and neither of them takes
an exit status. A macro reports one with `setexitcode <code>`, which sets the
value the process exits with and lets the macro carry on:

```ttl
setexitcode 1
end
```

That is how a macro reports failure to a batch file, a scheduler or a build
(`if errorlevel 1 echo Error!`). Without it the process exits with the
interpreter's own status, which says nothing about the session.

### 1.5 Speed

Waiting is message-driven: when the macro waits for something and no message
arrives, it sleeps rather than spins, and a reply wakes it immediately. The
sleep has the platform's timer resolution, so it can be up to ~15.6 ms even when
asked for 1 ms. `BoostTime` in the `[Macro]` section of `TERATERM.INI` trades
CPU for latency: for that many milliseconds after a command, the macro does not
sleep at all (one core is kept busy), then returns to message-driven waiting.
Omitted or `0` means always message-driven — which is the right setting unless a
macro is latency-bound on a very chatty device.

---

## 2. Starting a macro

### 2.1 Command line

```text
TTPMACRO.EXE [/I] [/V] [<macro file> [<parameters>...]]
```

| Argument | Meaning |
| --- | --- |
| `/I` | Start with the macro window iconified. |
| `/V` | Start hidden. A macro that starts Tera Term itself has to hide that too, which means passing `/V` through `connect`: `connect '/V host'`. Required in any context without a desktop (a scheduled task, a service). |
| `<macro file>` | The script. A relative path is resolved against the current directory. `*` or omitted opens the file-selection dialog. The extension `.TTL` may be omitted. |
| `<parameters>` | Passed through as strings; see §2.3. |

### 2.2 Two ways in

From Tera Term: *Control → Macro*, then choose the file. From the shell: run
`TTPMACRO` on the file. The difference matters — only the second can be used
unattended, because only it can run with `/V`, and only it works when Tera Term
is not already running (the macro's `connect` starts it).

While a macro runs, the macro window offers pause, restart and stop. Those are
interpreter-level controls, not language features; no command pauses itself
except `pause` and `mpause`.

### 2.3 Parameters

Everything on the command line after the script name is available as strings,
through three overlapping views:

| Variable | Type | Contents |
| --- | --- | --- |
| `params[0]` | string | The whole command line. |
| `params[1]`, `param1` | string | The macro file name, path excluded. |
| `params[2]`…`params[n]`, `param2`…`param9` | string | The arguments in order. |
| `paramcnt` | integer | The number of elements in `params`, counting `params[1]`. |

Two asymmetries to remember: `params` is a string array and holds *every*
argument, while `param2`…`param9` are individual variables and exist only for
the first eight arguments — the tenth argument onward is reachable only through
`params[10]`. `paramcnt` is the number of elements in `params`, and the element
holding the macro's own file name is one of them; whether `params[0]` is counted
is not stated, so a script that needs exact arithmetic is better off testing
`params[n]` directly.

Every element holds at most 511 characters.

### 2.4 The reconnect pattern

`connect` on an already-connected terminal is a no-op, which is not what a
script that has just told the far end to log out wants. Sending `exit` starts a
disconnection that takes seconds to minutes to complete, and a `connect` in the
middle of it succeeds trivially because the link is still up:

```ttl
sendln "exit"
do
    mpause 10          ; give the far end time to tear the session down
    testlink
loop while result = 2  ; 2 = still linked and connected
connect "server"
```

`unlink` is the shortcut when the terminal will not be reused: it detaches Tera
Term, after which `connect` has nothing to be already-connected to. Note that
with Tera Term's "auto window close" option the window may also be gone, which
`closett` and the `connect` command's own window handling account for.

## 3. Lexical structure

### 3.1 Encoding of the source file

Since Tera Term 4.102 a macro file may be Unicode. Before that, and for every
file that is not valid UTF-8, it is read in the ANSI code page.

| Encoding | Read |
| --- | --- |
| ANSI code page | yes (this is pre-4.102 behaviour, and the fallback) |
| UTF-8, with or without BOM | yes |
| UTF-16BE or UTF-16LE, with BOM | yes |
| UTF-16BE or UTF-16LE, **without** BOM | no |

The determination is short: a BOM decides among UTF-8, UTF-16BE and UTF-16LE; with
no BOM the file is read as UTF-8 if it decodes as UTF-8, and as the ANSI code
page if it does not. UTF-8 without a BOM is therefore safe, and UTF-16 without a
BOM is not. String *variables* are UTF-8 (§4.1), so a macro written in the ANSI
code page converts as it is read.

### 3.2 Lines

There are four kinds of line, and every one of them can carry a comment:

| Kind | Form | Example |
| --- | --- | --- |
| Empty | nothing, or only spaces, tabs and comments | |
| Command | `<command> <parameter>…` | `sendln 'cat'` |
| Assignment | `<variable> = <expression>` | `A = 33` |
| Label | `:<label>` | `:retry` |

Two rules that are easy to assume wrongly:

- **One line is one statement.** The language has no line-continuation
  character; a statement that will not fit has to be assembled in a variable.
- **A line holds at most 1023 characters.** Anything beyond that is ignored, and
  the truncation is silent — a long `send` is the usual way to meet it.

Newlines may be CR, LF or CR LF; the file is read as text either way.

### 3.3 Comments

`;` starts a comment that runs to the end of the line. `/*` … `*/` starts a
comment that may span lines, and the text after the closing `*/` on that line is
code again:

```ttl
; Tera Term Language
showtt 0
/* This is a 'comment' "string"
*/ MessageBox 'message 2' 'title 2'
```

### 3.4 Identifiers

| Kind | Form | Notes |
| --- | --- | --- |
| Variable | `[A-Za-z_][A-Za-z0-9_]*` | at most 31 characters, case-insensitive |
| Label | `[A-Za-z0-9_]+` | at most 31 characters, case-insensitive |

Labels are written with a leading `:` where they are defined and without it
where they are used (`goto retry`). Because a label may be all digits, `goto`
accepts a bare number — which is why the `for` loop's counter and a label can
look alike at a glance.

Variable names are case-insensitive, so `Count` and `count` are one variable.
That, plus a maximum length, means two long names sharing their first 31
characters are the same variable — the same class of surprise as a C compiler
with eight significant characters.

### 3.5 Reserved words

Four groups cannot be used as identifiers: command names, the operators `and`,
`not`, `or` and `xor`, the system variables (§4.4), and the special strings the
parser recognises (`params`, `paramcnt`, `param1`…`param9`, `inputstr`,
`matchstr`, `groupmatchstr1`…`groupmatchstr9`, `result`, `timeout`, `mtimeout`).

### 3.6 Integer constants

Decimal (`123`, `-11`) or hexadecimal with a `$` prefix (`$3a`, `$10F`, case
insensitive). There is no octal, no binary literal, no character-set syntax and
no floating point.

A negative constant is a minus sign and a constant, and the sign binds tighter
than the whitespace-separated parameter rule expects — see §5.4, which explains
why `for i 5 -1` is a syntax error.

### 3.7 String constants

Strings are written between `'` or `"`, with matching delimiters:

```ttl
'Hello, world'
"I can't do that"
```

There is no escape sequence. A backslash is a backslash, and the delimiter you
did not choose is an ordinary character, so a string containing both kinds of
quote cannot be written directly — that is what `#` is for.

`#` followed by a decimal or hexadecimal code produces one character:

```ttl
#65     ; "A"
#$41    ; "A"
#13     ; CR
```

Two forms concatenate by juxtaposition, which is how a macro builds a line ending
or an escape sequence:

```ttl
'cat readme.txt'#13#10
#$1B"[2J"            ; ESC [ 2 J  -- clear the screen
```

Strings cannot contain NUL, so `#0` is not available and a socket or file path
can never be silently truncated at a NUL by the language itself.

### 3.8 Limits

| Resource | Limit | Failure |
| --- | --- | --- |
| Line length | 1023 characters | silently ignored past the limit |
| String length | 511 characters | not specified (§5.5) |
| Integer variables | 256 | `Too many variables.` |
| String variables | 256 | `Too many variables.` |
| Integer arrays | 256 | `Too many variables.` |
| String arrays | 256 | `Too many variables.` |
| Labels | 512 | `Too many labels.` |
| Nesting (subroutine, `for`, `while`) | not specified | `Stack overflow.` |
| Nested `include` files | 9 levels | `Can't open file.` |

The budgets are per kind, not global: 256 integers, 256 strings, 256 integer
arrays and 256 string arrays are four separate allowances, so a macro cannot
spend its array budget on scalars or the other way round.

---

## 4. Types and variables

### 4.1 The four types

| Type | Size | Notes |
| --- | --- | --- |
| Integer | signed 32-bit, −2147483648 to 2147483647 | No floating point anywhere in the language. |
| String | up to 511 characters | UTF-8; may not contain NUL. |
| Integer array | declared with `intdim`, indexed from 0 | Elements are integers; default 0. |
| String array | declared with `strdim`, indexed from 0 | Elements are strings; default "". |

### 4.2 User variables

A user variable has no declared type. Its type is fixed the first time a value
is **assigned** to it, and from then on it accepts only that type:

```ttl
A = 33              ; A is an integer from here on
A = 'thirty'        ; Type mismatch.
```

Reading a variable before any assignment is `Variable not initialized.` rather
than an implicit zero, and that applies to array *elements* only after the array
exists: `intdim` and `strdim` create the elements, so a declared array's elements
are readable immediately.

Since Tera Term 5.8.0, `delvar` puts a variable back into the unused state so a
different type can be assigned. Nothing else can change a variable's type, and
`redim` resizes an array (optionally preserving its contents) without freeing the
name for another type.

### 4.3 Arrays

```ttl
intdim a 20         ; a[0] .. a[19]
strdim names 10     ; names[0] .. names[9]
redim a 40 'Preserve'
delvar a names
```

`<size>` is 1 to 65536 and the index is zero-origin, so the largest index is one
less than the size. Accessing past it is `Index out of range.` Arrays are the
only aggregate the language has — there are no records, lists or maps.

`redim <array> <size> ['Preserve']` changes the size without touching the name or
type; without `'Preserve'` the contents are reset, with it they are kept and any
truncated tail is discarded.

### 4.4 System variables

These exist before the macro runs, have fixed types, and are how a command
answers. A command never returns a value; it writes one of these and the macro
reads it on the next line.

| Variable | Type | Initial | Written by |
| --- | --- | --- | --- |
| `result` | integer | 0 | almost every command; the value is listed in that command's entry |
| `timeout` | integer | 0 | the waiting commands: whether the wait timed out |
| `mtimeout` | integer | 0 | same; precision about 50 ms |
| `inputstr` | string | "" | the reading commands: the line or data they received |
| `matchstr` | string | "" | `waitregex`, `strmatch`: the whole match |
| `groupmatchstr1` … `groupmatchstr9` | string | "" | `waitregex`, `strmatch`, `strreplace`, `strsplit`, `strjoin`: the capture groups |
| `param1` … `param9` | string | command line | §2.3 |
| `params` | string array | command line | §2.3 |
| `paramcnt` | integer | command line | §2.3 |

`result` is deliberately one variable rather than a return value: it is rewritten
by the next command that uses it, so a macro reads it immediately. Code that
calls one command and then checks `result` after another command is reading the
second command's answer.

---

## 5. Expressions

### 5.1 Everything is an integer

An expression is built from integer constants, integer variables, operators and
parentheses, and its value is an integer. Strings are not operands: comparing
them is `strcompare`, converting them is `str2int`, and building them is
`sprintf`/`strconcat`. A relational expression yields **1** for true and **0**
for false, and that is exactly what `if`, `while` and `until` test for — any
non-zero value is true.

### 5.2 Operators

Listed from tightest to loosest binding. Several operators have a symbolic and
a word form; they are the same operator.

| Precedence | Operators | Meaning |
| --- | --- | --- |
| 1 (highest) | `not` `~` | bitwise negation |
| 1 | `!` | logical negation |
| 1 | `+` `-` | unary plus, unary minus |
| 2 | `*` `/` `%` | multiply, divide, remainder |
| 3 | `+` `-` | add, subtract |
| 4 | `>>` `<<` | arithmetic shift |
| 4 | `>>>` | logical shift |
| 5 | `and` `&` | bitwise AND |
| 6 | `xor` `^` | bitwise exclusive OR |
| 7 | `or` `\|` | bitwise OR |
| 8 | `<` `>` `<=` `>=` | relational |
| 9 | `=` `==` `<>` `!=` | relational (equality) |
| 10 | `&&` | logical AND |
| 11 (lowest) | `\|\|` | logical OR |

Version gates, all of them from Tera Term 4.53/4.54, matter when a macro has to
run on an old interpreter: `~`, `&`, `^`, `\|`, `&&`, `\|\|` and `!` arrived in
4.53; `==`, `!=`, `>>`, `<<` and `>>>` arrived in 4.54. A macro using only
`and`, `or`, `xor`, `not` and `=` runs everywhere.

### 5.3 The three traps

**`and`, `or`, `xor` and `not` are bitwise, not logical.** They do not stop at 1
and they do not short-circuit. `2 and 1` is `0`; `A and not B` is a mask. For a
logical test write `&&` and `\|\|`, or compare explicitly.

**`=` is two different things.** As a statement it assigns; inside an expression
it compares. That is why chained assignment works and why it is confusing:

```ttl
A = B = C      ; the value of (B = C) -- 0 or 1 -- is assigned to A
```

**Arithmetic and logical shifts differ on the sign bit.** `>>` replicates the
sign (`-8 >> 1` is `-4`), `>>>` shifts zeros in. Reach for `>>>` whenever the
value is a bit pattern rather than a number.

### 5.4 Negative constants in parameter lists

Parameters are separated by whitespace, so a leading `-` is read as subtraction:

```ttl
for i 5 -1     ; Syntax error: "5-1" is one parameter, the third is missing
```

Three ways out, in order of clarity:

```ttl
for i 5 0-1    ; the extra 0 starts a new parameter
for i 5 (-1)   ; parentheses end the previous parameter
A = -1         ; or give the value a name
for i 5 A
```

The same trap exists for any command whose argument may be negative — `pause`,
`setspeed`, `mpause` and the numeric parameters of `sprintf` are the usual
places.

### 5.5 What the manual does not define

Worth knowing before relying on it: the upstream manual does not specify

- how integer division rounds when an operand is negative, or in which direction
  `>>` of a negative number truncates;
- what happens on 32-bit overflow (wrap, saturate, or error);
- whether `&&` and `\|\|` evaluate their right operand when the left decides the
  answer;
- what a string longer than 511 characters becomes — truncated, refused, or
  undefined;
- what happens when a macro line exceeds 1023 characters, beyond the statement
  that the excess is ignored.

An interpreter that has to interoperate should pick the behaviour of the original
Windows implementation; a macro that must run on both should avoid depending on
any of it.

---
## 6. Statements

### 6.1 Conditionals

There are two forms, and the difference is whether `then` is present.

**Single-line.** `if <expression> <statement>` — the whole branch is one
statement on the same line:

```ttl
if A>1 goto label
if result A=0
```

**Block.** `then` opens it, `endif` closes it, and `elseif`/`else` are optional:

```ttl
if <expression 1> then
    ...
elseif <expression 2> then
    ...
else
    ...
endif
```

`elseif` chains are evaluated in order and stop at the first true condition;
`else` catches everything remaining. There is no `elseif` in the single-line
form, and no `select`/`switch` anywhere — a multi-way branch is an `elseif`
chain on `result` or a comparison.

Conditions are integers: non-zero is true, zero is false (§5.1). A string
condition is a type error, so `if username ...` is never what was meant;
`strcompare` first.

### 6.2 Counting loops

```ttl
for <intvar> <first> <last>
    ...
next
```

`<intvar>` is set to `<first>` and the body runs; at `next` the loop stops if the
variable has reached `<last>`, otherwise the variable moves one step toward
`<last>` and the body runs again. The step is therefore **+1 or −1, chosen by
whether `<last>` is above or below `<first>`** — there is no step parameter, and
a `for` loop cannot count by twos:

```ttl
for i 1 10      ; 1, 2, ... 10
for i 5 1       ; 5, 4, ... 1
for i 3 3       ; runs once
```

The counter is an ordinary integer variable afterwards, holding the last value it
was given, and the loop needs an already-typed integer variable or a fresh name
(it assigns to it, which is what types it).

### 6.3 Condition loops

```ttl
while <expression>          until <expression>
    ...                         ...
endwhile                    enduntil

do [while|until <expression>]
    ...
loop [while|until <expression>]
```

| Form | Condition tested | Body runs |
| --- | --- | --- |
| `while` … `endwhile` | before each iteration, repeat while non-zero | zero or more |
| `until` … `enduntil` | before each iteration, repeat while **zero** | zero or more |
| `do while` / `do until` … `loop` | before each iteration | zero or more |
| `do` … `loop while` / `loop until` | after each iteration | **one or more** |

`do`/`loop` is the only way to express "run at least once", which is why it is
the shape of a retry loop:

```ttl
do
    mpause 10
    testlink
loop while result = 2
```

### 6.4 Subroutines

```ttl
call sub
...
end

:sub
    ...
return
```

`call` jumps to a label and `return` comes back. Two restrictions: the label must
be in the same file — a subroutine may not be called across an `include`
(`Can't call sub.`) — and there are no arguments or locals. Parameters are
passed in variables, and recursion is limited only by the stack
(`Stack overflow.`).

A subroutine that is entered by falling through instead of by `call` will `return`
to nowhere useful; keep the main body's `end` or `exit` above the first label, as
the examples do.

### 6.5 Jumping

```ttl
goto <label>
:label
```

`goto` moves control to the label. It may not be used to leave a `for` or `while`
loop — use `break`, which unwinds the loop properly; a `goto` out of a loop is
refused precisely because the interpreter's loop bookkeeping would be left behind.

### 6.6 Including another file

```ttl
include '<file name>'
```

The included file is read and interpreted at that point; when it ends or hits
`exit`, control returns to the line after `include`. Variables, labels and the
subroutine stack are shared, so an included file can call back into the main
file's labels only if those labels are in its own file — they are not. Nesting is
limited to 9 levels.

`include` is resolved when it is reached, not when the macro starts, so a file on
a network share can fail mid-run.

### 6.7 Leaving loops and the macro

| Statement | Effect |
| --- | --- |
| `break` | Leave the innermost `for`, `while`, `until` or `do` loop. (4.53+) |
| `continue` | Go to the next iteration of the innermost loop. (4.77+) |
| `end` | End the macro and close its window. |
| `exit` | In an included file, return to the includer; in the main file, same as `end`. |
| `setexitcode <n>` | Set the process exit status without ending. |

---

## 7. Command reference

Every command has the same shape: a name, whitespace-separated parameters, and a
result in a system variable (usually `result`) that the *next* line reads. A
command is a statement, not an expression — it cannot appear inside `if` except
as the single statement that form takes (§6.1).

### Notation

| Form | Meaning |
| --- | --- |
| `<name>` | required parameter |
| `[<name>]` | optional parameter |
| `<name>...` | repeatable |
| `'Preserve'` | a literal keyword string; keywords are quoted like any string |
| since *x.y* | the first Tera Term version with the command |

Parameter kinds — positional, keyword (`'width=10'`), or a mix where the leading
parameters are positional and the rest are keywords — are stated per command;
*Parameter passing* below explains the rule and why keywords exist.

### Parameter passing

TTL has three ways to pass parameters, and which one a command accepts is fixed
by that command:

**Positional.** In order, and every one is required unless bracketed:

```ttl
ttlcommand 10 20        ; 1st = width, 2nd = height
```

**Keyword.** `'name=value'` strings, in any order:

```ttl
ttlcommand 'width=10' 'height=20'
```

**Mixed.** The leading positional parameters first, then keywords in any order:

```ttl
ttlcommand 'Terminal' 'green' 'width=10' 'height=20'
```

Keyword parameters arrived in Tera Term 5.3, when `listbox` grew past the point
where a dozen positional arguments could be read or remembered. Anything not
explicitly documented as a keyword parameter is positional, and a keyword the
command does not know is typically ignored rather than reported — which is the
usual reason a parameter appears to do nothing.

### 7.1 Communication

These are the commands the link exists for. They divide into the connection
itself, the byte stream, the serial port's own lines, the window, file transfer
and logging.

#### The connection

| Command | Effect |
| --- | --- |
| `connect <command line parameters>` | Establish the link and/or the connection. Unlinked: starts Tera Term with those parameters and links to it. Linked but disconnected: connects. Already connected: **does nothing**. `result` = 0 unlinked, 1 linked only, 2 connected. |
| `cygconnect [<command line parameters>]` | Same, for a Cygwin session through CygTerm; the parameters go to CygTerm. If linked but disconnected it closes Tera Term and starts a new one. (4.57+) |
| `testlink` | Ask without changing anything. `result` = 0/1/2 as above. |
| `disconnect [<confirm>]` | Close the connection. The link survives, so a later `connect` redials. |
| `unlink` | Drop the link. The terminal keeps running and the macro can `connect` afresh; it can no longer control that window. |
| `closett` | Close Tera Term and become unlinked. Also suppresses the TCP/IP disconnect warning regardless of `ConfirmDisconnect`. |

The `<command line parameters>` of `connect` are Tera Term's own command line,
not TTL, so a serial connection is named there:

```ttl
connect '/C=3 /BAUD=115200'     ; COM3 at 115200
connect 'myhost'                ; telnet/SSH per the terminal's settings
```

Because `connect` ignores an existing connection, the loop in §2.4 is the way to
reconnect after telling the far end to log out.

#### Sending

| Command | Sent to | Character conversion | Newline |
| --- | --- | --- | --- |
| `send <data>…` | host (and local echo, if enabled) | decided per call: text goes like `sendtext`, binary like `sendbinary` | no |
| `sendln <data>…` | host | same as `send` | appends the terminal's newline |
| `sendtext <data>…` | host | converted to the terminal's character code (5.3+) | no |
| `sendbinary <data>…` | host | string sent as UTF-8 bytes, unchanged (5.3+) | no |
| `dispstr <data>…` | **the display only** | none | no |

All of them accept strings and integers, and an integer is one byte: its lowest
8 bits are the character code, so `send #3` is ^C and `send 65` is `A`. Values
outside 0–255 are truncated, not refused.

`dispstr` is the odd one and is the useful one for a macro talking to a person:
it writes to the screen and never to the host, so a prompt, a progress line or a
value can be shown without the far end seeing it.

```ttl
connect '/C=1'
sendln 'cat /etc/motd'
wait '$'
dispstr 'command finished'#13#10   ; the device never sees this
```

- `sendfile <filename> <binary flag>` — send a file. Binary non-zero: bytes
  unchanged. Zero: newlines converted (CR → CR LF) and control characters except
  TAB, LF and CR removed. Blocks until done.
- `sendbreak` — send a break. Serial: the line's break condition. Telnet: IAC
  BREAK (`255 243`). SSH2: an `SSH2_MSG_CHANNEL_REQUEST("break")`.
- `sendkcode <key code> <repeat count>` — perform the function bound to a key in
  the keyboard setup file, `repeat count` times. A key bound to a Tera Term menu
  command cannot be fired this way; use `callmenu`.
- `setecho <flag>` — local echo on/off.
- `enablekeyb <flag>` — whether the user may type while the macro is sending. `0`
  makes the session read-only for its duration, which is how a deliberate
  transfer avoids interleaved keystrokes.

#### Broadcast and multicast

| Command | Effect |
| --- | --- |
| `sendbroadcast <data>…` | Send to every linked terminal *and* their hosts. (4.62+) |
| `sendlnbroadcast <data>…` | Same, with a newline. (4.62+) |
| `setmulticastname <name>` | Give this terminal a name for multicast. (4.62+) |
| `sendmulticast <name> <data>…` | Send to the terminal(s) with that name. (4.62+) |
| `sendlnmulticast <name> <data>…` | Same, with a newline. (4.96+) |

A receiving terminal ignores broadcast data if its `AcceptBroadcast` setting is
off, and says nothing about it — a broadcast that "did nothing" on one window is
usually that.

#### Receiving: the buffer, and what a wait can see

There are two buffers between the host and a macro. Bytes arriving from the host
are handed to MACRO, which stores them in its own queue; the reading commands pop
from that queue. Everything below waits on *that queue*, not on the wire:

- `flushrecv` empties it. Use it before a command whose answer must not be
  confused with earlier output — the classic failure is a prompt from the last
  command satisfying the wait for this one.
- `setsync <flag>` chooses what happens when the queue fills. Asynchronous
  (default, 16 KiB buffer): bytes are dropped when it overflows. Synchronous: the
  terminal stops reading the host instead, so nothing is lost — at the cost of
  flow control on the connection, and a terminal that freezes if the macro stops
  reading. Turn it on for a transfer that must be lossless, off again afterwards.

Every waiting command honours the same two system variables:
`timeout` and `mtimeout` (seconds and milliseconds; precision about 50 ms). If
their sum is **zero or less the wait never times out**, which is the default and
the usual reason a macro hangs instead of failing.

| Command | Waits for | Stores | `result` |
| --- | --- | --- | --- |
| `wait <string>…` | any one of up to 10 substrings, anywhere in the stream | — | 0 timeout, *n* = the *n*-th string matched |
| `waitln <string>…` | a whole **line** containing one of up to 10 substrings | `inputstr` = the line | 0 timeout, *n* = which string |
| `waitn <bytes>` | *n* bytes to accumulate | `inputstr` (truncated to 511) | 0 timeout, 1 arrived |
| `waitrecv <sub> <len> <pos>` | a `<len>`-byte chunk containing `<sub>` starting at 1-origin `<pos>` | `inputstr` = the chunk | 0 timeout, 1 matched, −1 partial match saved before timeout |
| `waitregex <re>…` | a line matching one of up to 10 Oniguruma regular expressions | `inputstr`, `matchstr`, `groupmatchstr1`…`9` | 0 timeout, *n* = which expression |
| `wait4all <string>…` | as `wait`, across every terminal linked when the command starts | — | as `wait` |
| `waitevent <events>` | a lifecycle event, as a bitmask: 1 timeout, 2 unlink, 4 disconnection, 8 connection | — | the identifier of the event that happened |
| `recvln` | one line | `inputstr` | 0 timeout, 1 received |

Four details that decide whether a real script works:

- **`wait` with an empty string waits for any single character**, and `waitln`
  with an empty string waits for one character plus CRLF. That is the way to
  drain output you do not care about.
- **`waitln` matches a line, `wait` matches anywhere.** Waiting for `'OK'` with
  `wait` also matches `BROKEN`; `waitln 'OK'` requires a line that contains it.
- **An empty-string `wait` still consumes a byte**, so a loop of them is a way to
  wait for "anything" and then inspect `inputstr` — but only `waitln`/`recvln`
  fill `inputstr` with a whole line.
- **`waitregex` needs `regexoption` for multi-byte text.** Without it the pattern
  and the received bytes are matched in different encodings and a Japanese
  pattern silently never matches.

`wait4all` is a special case twice over: it needs `Wait4allMacroCommand=on` in
`teraterm.ini`, and it only watches terminals that were linked when it started, so
a window created during the wait is invisible to it. The upstream manual also
admits it "rarely does not work well".

#### The serial port

| Command | Effect |
| --- | --- |
| `setbaud <value>` | Change the line speed. Since 4.66 the value is **bits per second**; before that it was an index (1 = 110 … 15 = 921600). (4.58+) |
| `setspeed <value>` | Alias of `setbaud`, always in bps. (4.99+) |
| `setflowctrl <value>` | 1 = XON/XOFF, 2 = RTS/CTS, 3 = none, 4 = DSR/DTR (4.105+). (4.94+) |
| `setdtr <flag>` / `setrts <flag>` | Drive the line by hand. **Only for a serial connection whose flow control is `none`** — otherwise it does nothing, because the hardware is using the line. (4.59+) |
| `getmodemstatus <intvar>` | Read the input lines as a bitmask: 1 CTS, 2 DSR, 4 ring indicator, 8 RLSD (carrier). `result` = 0 success, 1 failure. (4.86+) |
| `setserialdelaychar <ms>` | Gap between characters sent. (5.5.0+) |
| `setserialdelayline <ms>` | Gap between lines sent. (5.5.0+) |

The two delays are for devices with no flow control that cannot keep up — a
bootloader that drops bytes above a few hundred baud-equivalents. They are
character- and line-level pacing, not a speed change, and they apply to everything
the terminal sends, not just the next command.

`setdtr`/`setrts` being silently inert under hardware flow control is worth
remembering when a reset line "does not work": with RTS/CTS enabled the driver owns
RTS.

#### The terminal window

| Command | Effect |
| --- | --- |
| `clearscreen <int>` | 0 = erase the screen, 1 = erase the scrollback as well, 2 = erase the TEK window. |
| `settitle <title>` / `gettitle <strvar>` | Write, read the window title. |
| `showtt <show flag>` | −1 hide VT, 0 minimise VT, 1 restore VT, 2 hide TEK, 3 minimise TEK, 4 open/restore TEK, 5 close TEK, 6 hide log window, 7 minimise log window, 8 restore log window. |
| `getttpos <show> <x> <y> <w> <h> <cx> <cy> <cw> <ch>` | Window and client-area geometry in pixels, origin at the primary display's top-left. Meaningless while minimised or hidden. Requires the link — without it the macro stops. (5.3+) |
| `callmenu <menu ID>` | Invoke a Tera Term menu command by numeric ID (the `KEYCODE.EXE` numbering). (4.56+) |
| `loadkeymap <filename>` | Load a keyboard setup file. |
| `restoresetup <filename>` | Load a whole Tera Term setup file. |
| `setdebug <flag>` | 0 normal output, 1 Tera Term's debug output, 2 hex dump of received bytes (upper-case, space-separated), 3 no output at all. (4.64+) |

`setdebug 2` is the tool for a protocol that will not talk: it shows the exact
bytes that arrived, including the ones a terminal would not draw.

#### File transfer

All of these send or receive through the terminal's **file transfer directory**
(`changedir`), and a relative *file name* in the command is resolved against it —
which is a different directory from MACRO's own working directory (`setdir`). This
is the single most common reason a transfer lands somewhere unexpected.

| Command | Direction | Blocks? | Notes |
| --- | --- | --- | --- |
| `sendfile <file> <binary>` | → host | yes | plain file send, conversion per `binary` |
| `recvfile <file> <binary> <idle seconds>` | ← host | yes | 5.6.0+; always raw; stops after that many seconds of silence, `result` = 1 if it stopped that way, else 0 |
| `xmodemrecv <file> <binary> <option>` | ← host | yes | `option` 1 checksum, 2 CRC, 3 CRC (4.90+; before 4.90, 3 meant 1 KiB packets for the sender) |
| `xmodemsend <file> <option>` | → host | yes | `option` 1/2 = 128-byte packets, 3 = 1 KiB (4.90+) |
| `ymodemrecv` | ← host | yes | 4.66+; **takes no file name** — names come from the protocol |
| `ymodemsend <file>` | → host | yes | 4.66+ |
| `zmodemrecv` | ← host | yes | no file name; names come from the protocol |
| `zmodemsend <file> <binary>` | → host | yes | |
| `kmtget <file>` | ← host | yes | Kermit Get; the host must be in server mode |
| `kmtsend <file>` | → host | yes | |
| `kmtrecv` | ← host | yes | |
| `kmtfinish` | — | yes | end a Kermit server session |
| `bplusrecv` / `bplussend <file>` | ← / → | yes | B-Plus |
| `quickvanrecv` / `quickvansend <file>` | ← / → | yes | Quick-VAN |
| `scprecv <remote> [<local>]` | ← host | **no** | 4.57+; SCP |
| `scpsend <file> [<dest>]` | → host | **no** | 4.57+; SCP |

The protocol transfers set `result` to 1 on success and 0 otherwise. SCP is the
exception: it does not block until the transfer finishes, and the manual lists no
`result` for it — it rides on the SSH connection the terminal already has. A macro
that needs to know when an `scpsend` finished has to wait for a prompt or for a
file to appear.

`recvfile`'s idle timeout is the only way to receive data whose length is not
known in advance and which does not end with a protocol terminator:

```ttl
recvfile 'capture.bin' 1 3      ; write until 3 s of silence
```

#### Logging

The log belongs to Tera Term and survives between macros; a macro opens it,
writes through it, and closes it only if it wants to.

| Command | Effect |
| --- | --- |
| `logopen <file> <binary> <append> [<plain> [<timestamp> [<hidedialog> [<screenbuffer> [<timestamptype>]]]]]` | Start logging. `result` = 0 opened, 1 failed (4.62+). |
| `logclose` | Close the log. |
| `logpause` | Stop writing; **received characters are discarded**, not buffered for later. |
| `logstart` | Resume after a pause. |
| `logwrite <string>` | Append a string. Only while logging; works while paused; an empty string is a syntax error. |
| `loginfo <strvar>` | `strvar` = the file name; `result` = −1 when not logging, otherwise a bitmask. (4.73+) |
| `logrotate 'size' <n[K\|M]>` / `logrotate 'rotate' <count>` / `logrotate 'halt'` | Configure rotation: rotate at that size (minimum 128 bytes), keep that many files (minimum 2), or stop rotating. Only configures — rotation happens when the size is reached. Requires an open log. (4.78+) |
| `logautoclosemode <flag>` | 1 = close the log when the macro ends. The setting resets to default when the macro terminates, so this is not a persistent preference. (4.79+) |

`logopen`'s flags:

| Flag | Meaning |
| --- | --- |
| `<binary>` | 0: newlines converted (CR → CR LF) and escape sequences stripped. Non-zero: bytes written unchanged. Binary logs can be replayed. |
| `<append>` | Non-zero: append to an existing file. Zero: overwrite. |
| `<plain>` | Non-zero: ASCII non-printable characters omitted. Ignored when binary. (4.61+) |
| `<timestamp>` | Non-zero: prefix each line with a timestamp. Ignored when binary. (4.61+) |
| `<hidedialog>` | Non-zero: do not show the Log dialog. |
| `<screenbuffer>` | Non-zero: write the current screen buffer first. (4.80+) |
| `<timestamptype>` | 0 local time, 1 UTC, 2 elapsed since logging started, 3 elapsed since the connection started. (4.97+) |

`loginfo`'s bitmask mirrors those flags: 1 binary, 2 append, 4 plain text, 8
timestamp, 16 hide dialog.

A log path without a `\` is resolved against Tera Term's current directory, not
the file transfer directory — `changedir` first, as the examples do, when the log
belongs somewhere specific.

#### Status

- `gethostname <strvar>` — the destination host name or IP address. (4.61+)
- `setmulticastname <name>` — name this terminal for `sendmulticast`.

### 7.2 Control

The statements themselves are specified in §6 — `if`/`then`/`elseif`/`else`/
`endif`, `for`/`next`, `while`/`endwhile`, `until`/`enduntil`, `do`/`loop`,
`call`/`return`, `goto`, `include`, `break`, `continue`, `end`, `exit`,
`setexitcode`. Three commands in this family are not statements of that kind:

**`pause <time>`** — wait *time* **seconds**. The argument is an expression, so
`pause Time` and `pause A*10` work.

**`mpause <time>`** — the same in **milliseconds** (4.27+). `mpause 200` is a
fifth of a second.

Neither takes a fraction: `pause 0.5` is a syntax error, and a sub-second wait is
`mpause 500`. Both are the only places in the language where time passes without
something happening, and both are subject to the negative-constant trap (§5.4).

**`execcmnd <statement>`** — interpret a TTL **statement** held in a string. It is
the only dynamic construct in the language, and the reason a macro can build a
variable name or a command at run time:

```ttl
for x 1 10
    tmp = 'arr'
    int2str ix x
    strconcat tmp ix
    strconcat tmp '='
    strconcat tmp ix
    execcmnd tmp          ; arr5=5, and so on
next
```

Since it re-enters the parser, `execcmnd` cannot be used where an expression is
expected, and a string built from untrusted input can run anything a macro can —
including `exec`. A macro that pastes device output into `execcmnd` is a macro
that has given the device control of the host.

### 7.3 Strings

TTL's string handling is a library of small commands rather than operators: there
is no `+` for concatenation, no indexing and no comparison. Three conventions run
through all of them.

- **The destination is a variable name**, not an expression. `<strvar>` in a
  signature is the name of the variable the result is written into; there is no
  way to call a string function inline, which is why a parse takes several lines.
- **Some commands answer in `result`, some in `inputstr`, some in both.** Each
  entry says which, because guessing wrong is silent.
- **The nine `groupmatchstr` slots are the language's array of fields.**
  `strsplit`, `strmatch`, `strreplace` and `strjoin` all work through them, so
  splitting and rejoining is the same nine slots written and then read.

#### Building and converting

| Command | Effect | `result` |
| --- | --- | --- |
| `sprintf <format> <arg>…` | Format into **`inputstr`** — there is no destination parameter. (4.52+) | 0 ok, 1 no format, 2 invalid format, 3 invalid argument |
| `sprintf2 <strvar> <format> <arg>…` | Format into `<strvar>`; prefer it, because the destination is explicit. (4.62+) | as above, plus 4 = invalid destination |
| `int2str <strvar> <int>` | Integer → string; the same as `%d`. | — |
| `str2int <intvar> <string>` | String → integer, decimal or hexadecimal. | 1 ok, 0 failed |
| `strconcat <strvar> <string>` | Append to `<strvar>`. This is the `+`. | — |
| `strcopy <strvar> <pos> <len> <string>` | Copy `<len>` characters of `<string>` from 1-origin `<pos>`. | — |
| `str2code <intvar> <string>` | Up to 4 characters, packed as bytes into an integer; longer input keeps the **last** 4. | — |
| `code2str <strvar> <code>` | The inverse; at most 4 characters, and empty for 0. | — |
| `expandenv <strvar> [<string>]` | Expand `%NAME%` references. (4.71+) | — |
| `tolower <strvar> <string>` / `toupper <strvar> <string>` | Change case. (4.53+) | — |

The format is C's `printf`: conversions `c d i o u x X e E f g G a A s`, the
flags `- + 0 #` and space, width and precision, `*` included. Floating-point
conversions exist, but TTL has no float variables (§4.1), so the number reaches
`sprintf` as a *string* and only its formatting is done here.

#### Measuring and comparing

| Command | Effect | `result` |
| --- | --- | --- |
| `strlen <string>` | Length in **bytes**, not characters. | the length |
| `strcompare <string1> <string2>` | Lexicographic comparison. | −1, 0 or 1 |
| `strscan <string> <substring>` | Find the first occurrence. | 1-origin position, or 0 |
| `strmatch <target> <regex>` | Oniguruma search. (4.59+) | the **1-origin offset** of the match, or 0 |

`strmatch`'s `result` is a position, not a boolean, so the test is `> 0`. It
fills `matchstr` with the whole match and `groupmatchstr1`…`9` with the capture
groups — and leaves all ten untouched when nothing matches, so a stale
`groupmatchstr1` from the previous line is a real hazard:

```ttl
strmatch inputstr 'temp=([0-9]+)'
if result > 0 then
    int2str t groupmatchstr1
    dispstr 'temperature: ' t #13#10
endif
```

`strlen` counting bytes matters for anything non-ASCII: a UTF-8 accent is two
bytes, a Han character three or four. For a character count, match with a
regular expression under the right `regexoption` encoding.

#### Taking apart and putting together

All since 4.67 unless noted.

| Command | Effect |
| --- | --- |
| `strsplit <string> <separator> [<count>]` | Split on **one** delimiter character into `groupmatchstr1`…; with more pieces than `<count>` the last slot takes the remainder. `result` = the number of pieces, or 10 when there were more than nine. |
| `strjoin <strvar> <separator> [<count>]` | Join `groupmatchstr1`…`<count>` (default 9) with the separator into `<strvar>`. |
| `strinsert <strvar> <index> <string>` | Insert at 1-origin `<index>`. |
| `strremove <strvar> <index> <len>` | Delete `<len>` characters starting at `<index>`. |
| `strreplace <strvar> <index> <regex> <newstr>` | Replace from `<index>`; `result` 1 replaced, 0 no match, −1 bad pattern. Sets `matchstr` and **clears** `groupmatchstr1`…`9`. |
| `strtrim <strvar> <trimchars>` | Strip any of the characters in `<trimchars>` from both ends. |
| `strspecial <strvar> [<string>]` | Turn `\\`, `\n` and `\t` **into** the characters they denote. |
| `regexoption <option>…` | Syntax and encoding for `strmatch`, `strreplace` and `waitregex`. |

`strsplit` and `strjoin` are a pair over the same nine slots, and that is the
whole parse idiom:

```ttl
strsplit response ':'          ; fields land in groupmatchstr1..9
strcompare groupmatchstr2 'OK'
if result = 0 then
    strjoin message ','        ; put them back together
endif
```

Two traps. `strreplace` **clears** the slots rather than filling them, so a
`strsplit` followed by any `strreplace` loses the fields it produced. And
`strspecial` is named for "special characters" but does the opposite of escaping:
it *expands* the escapes, turning the two characters `\` `n` into a newline. That
is what a message with line breaks needs — the dialogs' obsolete `<special>`
parameter did the same thing for their message strings (§7.6).

### 7.4 Files

Two names, two directories. `setdir`/`getdir` are **MACRO's** working directory —
where `fileopen` resolves a relative path. `changedir` (§7.1) is **Tera Term's**
file transfer directory — where `sendfile` and its relatives look. A script that
mixes them will write to a place it did not mean.

#### Opening, reading, writing

| Command | Effect |
| --- | --- |
| `fileopen <handle> <file> <append flag> [<readonly flag>]` | Open in **binary mode**. The handle is an integer *variable* the macro names — not a number it picks. `<append flag>` non-zero appends; `<readonly flag>` non-zero opens without write. |
| `filecreate <handle> <file>` | Create or truncate, then open. `<handle>` = −1 on failure, and `result` = 0 on success. |
| `fileclose <handle>` | Close; the handle is invalid afterwards. |
| `filereadln <handle> <var>` | Read one line into `<var>`. |
| `fileread <handle> <bytes> <var>` | Read `<bytes>` (1–511) into `<var>`. (4.48+) |
| `filewrite <handle> <data>` / `filewriteln <handle> <data>` | Write; `writeln` appends CR LF. An integer argument writes its low byte. |
| `filemarkptr <handle>` / `fileseekback <handle>` | Mark a position / return to it. |
| `fileseek <handle> <offset> <origin>` | `<origin>`: 0 = start of file, 1 = current, 2 = end. |
| `filestrseek <handle> <string>` | Search **forward**; leaves the pointer just after the string. |
| `filestrseek2 <handle> <string>` | Search **backward**; leaves the pointer just *before* the string. |
| `filetruncate <file> <size>` | Truncate or extend, by **file name**; fills with NUL. `result` 0 ok, −1 error. (4.67+) |
| `filelock <handle> [<timeout>]` / `fileunlock <handle>` | Whole-file lock. `<timeout>`: 0 = fail immediately if locked, omitted = wait indefinitely, a value = wait that many seconds. `result` 0 ok, 1 failed. (4.74+) |

`filereadln` and `fileread` invert the convention the rest of the language uses:
**`result` is 1 when the read hit the end of the file**, and 0 when it succeeded —
so the loop breaks on 1, not on 0:

```ttl
fileopen fh 'data.txt' 0
while 1
    filereadln fh line
    if result break
    dispstr line #13#10
endwhile
fileclose fh
```

A line holding only a newline is an empty string with `result` 0, so an empty line
is not the end of the loop. The manual does not say whether the partial line
before EOF is delivered with that 1; a file whose last line matters should end
with a newline, or be read with `fileread`.

#### Manipulating

Almost everything in this group sets `result` to **0 for success** and non-zero
for failure — the opposite of the connection commands, and the most common way a
file-handling macro checks the wrong thing.

| Command | Effect | `result` |
| --- | --- | --- |
| `filecopy <file1> <file2>` | Copy, overwriting. | 0 ok, non-zero failed |
| `filedelete <file>` | Delete. | 0 ok, non-zero failed |
| `filerename <file1> <file2>` | Rename. | 0 ok, non-zero failed |
| `fileconcat <file1> <file2>` | Append `<file2>` to `<file1>`; the two must differ. | 0 ok, non-zero failed |
| `foldercreate <folder>` / `folderdelete <folder>` | Create/delete; deleting a non-empty folder fails. (4.69+) | 0 ok, non-zero failed |
| `filesearch <name>` / `foldersearch <name>` | Does the file/folder exist? (4.69+) | 1 found, 0 not |
| `filestat <file> <size> [<mtime>] [<drive>]` | Size in bytes, last modification time, drive number. (4.66+) | −1 error |
| `findfirst <dir handle> <pattern> <var>` / `findnext <dir handle> <var>` / `findclose <dir handle>` | Enumerate matching names. | 1 found, 0 no more |
| `getfileattr <file>` / `setfileattr <file> <attr>` | Windows attribute bits. (4.69+) | — |
| `makepath <strvar> <dir> <name>` | Join a directory and a name into a path, inserting `\` if needed. **It creates nothing.** | — |
| `basename <strvar> <path>` / `dirname <strvar> <path>` | Split a path string; the path need not exist. (4.69+) | — |
| `getdir <strvar>` / `setdir <dir>` | MACRO's working directory. | — |

`findfirst` returns a *directory handle*, which every later call needs, and it
fails into `-1`, so the guard comes first:

```ttl
findfirst dir '*.log' name
if result = 1 then
    while 1
        dispstr name #13#10
        findnext dir name
        if result = 0 break
    endwhile
    findclose dir
endif
```

A search that is opened and never closed leaks a handle per directory —
`findclose` is not optional.

### 7.5 Passwords

A password store is a **file**, named explicitly in every command; there is no
session-global store. One file holds any number of passwords, each identified by a
name and stored encrypted.

| Command | Effect |
| --- | --- |
| `getpassword <file> <name> <var>` | Fetch and decrypt. Creates the file if it is missing, and if the name is not in it, prompts for the password and stores what was typed. |
| `setpassword <file> <name> <password>` | Add or overwrite. (4.75+) |
| `ispassword <file> <name>` | Whether the name is in the file. (4.75+) |
| `delpassword <file> <name>` | Remove; an **empty name removes every password in the file**. |
| `passwordbox <message> <title> [<special>]` | Ask for a password with the input masked; the answer lands in `inputstr`. |

The `2` family (5.3+) encrypts with **AES-256-CTR under a passphrase you supply**
as `<encrypt str>`, so the file is useless to anyone who does not know that
string:

| Command | Effect |
| --- | --- |
| `getpassword2 <file> <name> <var> <encrypt str>` | As `getpassword`, decrypted with the passphrase. |
| `setpassword2 <file> <name> <password> <encrypt str>` | Store, encrypted with the passphrase. |
| `ispassword2 <file> <name>` / `delpassword2 <file> <name>` | As above. |

The two families are **not compatible**: a file written by `setpassword` cannot be
read by `getpassword2`, or the other way round, and `<encrypt str>` must be the
same string on write and read.

`getpassword`'s prompt-and-store behaviour is what makes an unattended script
possible — the first run asks, later runs do not, and the password never appears
in the `.ttl`:

```ttl
getpassword 'console.pwd' 'root' pw
sendln pw
```

`passwordbox` is the other half: a password typed once for this run and never
stored.

```ttl
passwordbox 'Password for the console' 'Login'
sendln inputstr
```

Neither keeps the password out of the *process* — only out of the text. A macro
file that must not leak a password at all still has to keep it in the store and
out of the script's own output.

### 7.6 Miscellaneous

#### Dialogs

All of these block until the user answers, except `statusbox`. They are the only
part of TTL that needs a person.

| Command | Where the answer goes |
| --- | --- |
| `messagebox <message> <title> [<special>]` | nowhere; `<title>` is required |
| `yesnobox <message> <title> [<special>]` | `result`: 1 = Yes, 0 = No |
| `inputbox <message> <title> [<default> [<special>]]` | `inputstr` |
| `passwordbox <message> <title> [<special>]` | `inputstr`, with the input masked (§7.5) |
| `filenamebox <title> [<type> [<initialdir>]]` | `inputstr` = the path; `result` non-zero = chosen, 0 = cancelled. `<type>` 0 = Open, non-zero = Save (4.65+); `<initialdir>` 4.85+. (4.54+) |
| `dirnamebox <title> [<initialdir>]` | `inputstr` = the directory; `result` 1 = chosen, 0 = cancelled. (4.86+) |
| `listbox <message> <title> <array> [<selected>] [<keyword>…]` | `result` = **0-based** index, −1 = cancelled. (4.78+) |
| `statusbox <message> <title> [<special>]` | nothing; does not block |
| `closesbox` | close the `statusbox` |
| `bringupbox` | raise the `statusbox` above other windows (4.78+) |
| `setdlgpos [<x> <y> [<position>]]` | where the next dialog appears (4.105+) |

Three details decide whether a dialog-driven macro behaves:

- **`listbox` is 0-based and −1 means cancelled**, so "the first item" and "the
  user escaped" are different values. Its keyword parameters (5.3+) are
  `'dblclick=on'`, `'minmaxbutton=on'`, `'minimize=on'`, `'maximize=on'` and
  `'listboxsize=WxH'` (default 26x6). `<selected>` preselects a 0-based index
  (4.89+).
- **`statusbox` is the one that does not block.** Calling it again replaces the
  message, which makes it a progress line beside a long wait; the user cannot
  dismiss it with Enter or Escape, so a script that ends without `closesbox` can
  leave it on screen.
- **`<special>` is obsolete everywhere it appears.** It expanded `\\`, `\n` and
  `\t` in the message; `strspecial` (§7.3) does the same thing to a variable and
  is the supported way to put a line break in a box.

#### Time and environment

| Command | Effect |
| --- | --- |
| `gettime <var> [<format> [<timezone>]]` | Local time, default `%H:%M:%S`, in `strftime` format. `result` 0 ok, 1 result too long, 2 invalid format. |
| `getdate <var> [<format> [<timezone>]]` | Local date, default `%Y-%m-%d`; same `result` values. |
| `settime <time>` | Set the system clock, `HH:MM:SS`. |
| `setdate <date>` | Set the system date, `YYYY-MM-DD`. |
| `uptime <intvar>` | Milliseconds since the OS started; wraps at 49.7 days. (4.81+) |
| `getenv <name> <var>` | Read an environment variable. |
| `setenv <name> <value>` | Set one — **in the macro's own environment only**, not the system's. (4.54+) |
| `getver <var> [<version>]` | The Tera Term version. With `<version>` it also compares: `result` −2 invalid, −1 older, 0 equal, 1 newer. (4.58+) |
| `getttdir <var>` | The macro interpreter's own directory. (4.60+) |
| `getspecialfolder <var> <type>` | A Windows special folder such as `MyDocuments`. (4.72+) |
| `getipv4addr <string array> <intvar>` / `getipv6addr <string array> <intvar>` | Local addresses into an array, the count into the integer. (4.78+) |

`gettime`'s default is a time of day, not a timestamp: the `%Y%m%d-%H%M%S`
spelling used in every logging example is a **format** the macro passes,
remembered from `strftime`, and the reason log file names sort.

#### Processes and the clipboard

| Command | Effect |
| --- | --- |
| `exec <command line> [<show> [<wait> [<dir>]]]` | Run a program. `<show>` is `hide`, `minimize`, `maximize` or `show`; `<wait>` = 1 blocks and puts the child's exit code in `result` (4.63+); `<dir>` sets its working directory (4.78+). |
| `clipb2var <strvar> [<offset>]` | Read the clipboard into `<strvar>`. (4.46+) |
| `var2clipb <string>` | Write a string to the clipboard. (4.46+) |

`clipb2var`'s result codes: 0 could not open the clipboard, the content was not
text, or the offset was invalid; 1 copied; **2 the text was truncated** — call
again with the next offset; 3 out of memory. The clipboard is chunked at 511
bytes, and a call with a non-zero offset reads the snapshot taken by the call with
no offset, so read once with no offset before paging through the rest.

`exec` is the escape hatch to the operating system, and the one command whose
`result` is the child's exit code rather than a status of its own.

#### Checksums and bit twiddling

| Command | Effect |
| --- | --- |
| `checksum8 <intvar> <string>` / `checksum8file <intvar> <file>` | 8-bit sum. (4.78+) |
| `checksum16 <intvar> <string>` / `checksum16file <intvar> <file>` | 16-bit sum. (4.78+) |
| `checksum32 <intvar> <string>` / `checksum32file <intvar> <file>` | 32-bit sum. (4.78+) |
| `crc16 <intvar> <string>` / `crc16file <intvar> <file>` | CRC-16-CCITT, x16+x12+x5+1. (4.78+) |
| `crc32 <intvar> <string>` / `crc32file <intvar> <file>` | CRC-32, the Ethernet FCS polynomial. (4.60+) |
| `rotateleft <intvar> <value> <count>` / `rotateright <intvar> <value> <count>` | Circular shift. (4.54+) |
| `random <intvar> <max>` | A value in 0…`<max>`, inclusive; `<max>` must be above 0. (4.27+) |

The `…file` variants set `result` to −1 when the file cannot be opened; the string
variants leave `result` alone. The pair exists because a device protocol asks for
a checksum constantly — a firmware image is `checksum16file`, and the answer is
compared with what the bootloader prints.

#### Variables and arrays

| Command | Effect |
| --- | --- |
| `intdim <array> <size>` / `strdim <array> <size>` | Declare an array of 1…65536 entries, 0-origin. (4.72+) |
| `delvar <var>…` | Return variables to the unused state so a different type can be assigned. (5.8.0+) |
| `redim <array> <size> ['Preserve']` | Resize an array, optionally keeping the contents. (5.8.0+) |
| `ifdefined <var>` | `result` = 0 undefined, 1 integer, 3 string, 4 label, 5 integer array, 6 string array. (4.46+) |

#### The rest

- `beep [<sound type>]` — 0 simple beep, 1 asterisk, 2 exclamation, 3 critical
  stop, 4 question, 5 the default beep (the default). Not every machine makes a
  sound for every one of them.
- `show <flag>` — Tera Term's *macro* window: less than 0 hides it, 0 minimises
  it, greater than 0 restores it and brings it to the top. This is the runtime
  counterpart of the `/I` and `/V` options (§2.1).

## 8. Errors and exit status

There are two kinds of failure in TTL and they are handled in opposite ways.

**A runtime failure is a `result` value, and nobody checks it for you.** A wait
that times out, a transfer that fails, a file that will not open — each is a
number in `result` that the next line is expected to read. The interpreter does
not stop, does not warn, and does not remember. A macro that ignores `result`
after `fileopen` goes on to write to a handle that was never opened.

**An interpreter error stops the run.** The message appears in an error dialog,
the macro ends, and the exit status is a failure. These are the errors the
language itself can raise:

| Message | Meaning |
| --- | --- |
| `Can't call sub.` | The subroutine is in a different file. |
| `Can't link macro.` | The link to Tera Term could not be established. |
| `Can't open file.` | An `include` target is missing, or includes are nested more than 9 deep. |
| `")" expected.` | Missing closing parenthesis. |
| `"*/" expected.` | Unterminated `/*` comment. |
| `Link macro first. Use 'connect' macro.` | A communication command ran before the link existed. |
| `Divide by zero.` | Division or remainder by zero. |
| `Invalid control.` | `else`, `elseif` or `endif` with no matching `if`. |
| `Label already defined.` | The same label twice. |
| `Label required.` | `goto`/`call` to a label that does not exist. |
| `Stack overflow.` | Subroutine or loop nesting too deep. |
| `Syntax error.` | The statement does not parse (including the negative-constant trap, §5.4). |
| `Too many labels.` | More than 512 labels. |
| `Too many variables.` | More than 256 of some kind of variable. |
| `Type mismatch.` | A value of a type the variable or parameter does not take. |
| `Variable not initialized.` | Read before its first assignment. |
| `Index out of range.` | An array index past the declared size. |
| `"]" expected.` | Missing `]` in an array reference. |
| `Can't allocate memory.` | The interpreter ran out. |
| `Unknown command.` | The command does not exist in this Tera Term version. |

`Unknown command.` is the one to know when a macro is moved between machines: a
5.8 macro that uses `delvar` stops on a 5.7 interpreter with that message, which
is why §7 marks every version gate.

The exit status is set by `setexitcode <n>` (§1.4). A macro that ends with an
interpreter error exits unsuccessfully and there is nothing to configure about
it; a macro that ends normally exits with whatever `setexitcode` last set, or
zero.

---

## 9. Appendices

### 9.1 Control characters

`#nn` builds a character from its code, and the codes a serial macro needs are
almost always the control ones. Decimal and hexadecimal are both accepted
(`#13` and `#$0D` are the same character).

| Char | Dec | Hex | Char | Dec | Hex | Char | Dec | Hex | Char | Dec | Hex |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| NUL `^@` | 0 | $00 | DLE `^P` | 16 | $10 | SPACE | 32 | $20 | `0` | 48 | $30 |
| SOH `^A` | 1 | $01 | DC1 `^Q` | 17 | $11 | `!` | 33 | $21 | `1` | 49 | $31 |
| STX `^B` | 2 | $02 | DC2 `^R` | 18 | $12 | `"` | 34 | $22 | `2` | 50 | $32 |
| ETX `^C` | 3 | $03 | DC3 `^S` | 19 | $13 | `#` | 35 | $23 | `3` | 51 | $33 |
| EOT `^D` | 4 | $04 | DC4 `^T` | 20 | $14 | `$` | 36 | $24 | `4` | 52 | $34 |
| ENQ `^E` | 5 | $05 | NAK `^U` | 21 | $15 | `%` | 37 | $25 | `5` | 53 | $35 |
| ACK `^F` | 6 | $06 | SYN `^V` | 22 | $16 | `&` | 38 | $26 | `6` | 54 | $36 |
| BEL `^G` | 7 | $07 | ETB `^W` | 23 | $17 | `'` | 39 | $27 | `7` | 55 | $37 |
| BS `^H` | 8 | $08 | CAN `^X` | 24 | $18 | `(` | 40 | $28 | `8` | 56 | $38 |
| HT `^I` | 9 | $09 | EM `^Y` | 25 | $19 | `)` | 41 | $29 | `9` | 57 | $39 |
| LF `^J` | 10 | $0A | SUB `^Z` | 26 | $1A | `*` | 42 | $2A | `:` | 58 | $3A |
| VT `^K` | 11 | $0B | ESC `^[` | 27 | $1B | `+` | 43 | $2B | `;` | 59 | $3B |
| FF `^L` | 12 | $0C | FS `^\` | 28 | $1C | `,` | 44 | $2C | `<` | 60 | $3C |
| CR `^M` | 13 | $0D | GS `^]` | 29 | $1D | `-` | 45 | $2D | `=` | 61 | $3D |
| SO `^N` | 14 | $0E | RS `^^` | 30 | $1E | `.` | 46 | $2E | `>` | 62 | $3E |
| SI `^O` | 15 | $0F | US `^_` | 31 | $1F | `/` | 47 | $2F | `?` | 63 | $3F |

Codes 64–126 are the printable characters in their usual order (`A` = 65 = `$41`,
`a` = 97 = `$61`), and 127 is DEL. Strings cannot contain code 0.

The three used constantly: `#13` (CR), `#10` (LF) and `#27` = `#$1B` (ESC, the
start of every escape sequence).

### 9.2 Line endings

Tera Term normalises what a macro sees. A CR or a CR LF pair arriving from the
host is converted to **CR LF** before MACRO reads it, so `waitln` and `recvln`
split lines on that pair and a pattern never has to allow for a bare CR.

The other direction is normalised too: what `#13#10` *becomes* on the wire is
Tera Term's newline setting, not two fixed bytes. That is why these are the same
statement:

```ttl
send 'abc'#13#10
sendln 'abc'
```

and why `sendln` is the right way to end a line when the target's newline
convention is not known to the macro.

### 9.3 Serial speed identifiers

Before Tera Term 4.66, `setbaud` took an index rather than a speed. A macro that
must also run on an old interpreter has to use the index, so the table is worth
keeping:

| Value | Speed (bps) | Value | Speed (bps) | Value | Speed (bps) |
| --- | --- | --- | --- | --- | --- |
| 1 | 110 | 6 | 4800 | 11 | 57600 |
| 2 | 300 | 7 | 9600 | 12 | 115200 |
| 3 | 600 | 8 | 14400 | 13 | 230400 |
| 4 | 1200 | 9 | 19200 | 14 | 460800 |
| 5 | 2400 | 10 | 38400 | 15 | 921600 |

Since 4.66 the argument is the speed itself, and `setspeed` (4.99+) always is.
Writing `setbaud 12` on a modern interpreter therefore sets the line to **12
bits per second**, not 115200 — the same text meaning two different things
depending on the version is a good reason to test a macro on the version it will
run on.


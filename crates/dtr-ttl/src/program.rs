//! The program: statements, and every jump already resolved.
//!
//! A TTL macro is a flat list of lines with `goto` in it, so the natural
//! execution model is a program counter, not a tree — a tree cannot represent
//! a jump into the middle of itself. This module therefore turns
//! [`crate::line`]'s classified lines into a flat list of [`Op`]s and fills in
//! every target that the source only names:
//!
//! * `if`/`elseif`/`else`/`endif` become four ops carrying two positions each:
//!   where to go when the condition is false (`next` — the following branch),
//!   and where a branch that *did* run continues at (`endif`). That second
//!   field is what makes `elseif` work without a second pass: falling out of a
//!   taken branch lands on the next `ElseIf`, which sees that its `if` was
//!   already satisfied and jumps to `endif`.
//! * `while`/`until`/`do`/`for` record where their loop ends, and every
//!   `break` and `continue` inside is patched with the target it belongs to at
//!   the time the loop closes — so the innermost loop always wins, and no
//!   run-time search is needed.
//! * `goto` and `call` need the whole file before a label can be found, so
//!   they are resolved at the end; a target records the loop nesting it lives
//!   at, which is how a run-time jump knows how much loop bookkeeping to
//!   discard when it leaves a loop.
//!
//! Everything that is not structure stays a [`Op::Command`] with its
//! parameters unparsed: this module is not the place that decides what
//! `messagebox` takes. Structure errors are reported here, at load time, since
//! that is when Tera Term reports them too — a macro with a missing `endif`
//! never starts.

use std::collections::BTreeMap;
use std::fmt;

use crate::expr::{self, Expr};
use crate::lex::{Tok, Token};
use crate::line::{self, Line, LineKind, ScanError};

/// §3.8: no more than 512 labels in one program.
pub const MAX_LABELS: usize = 512;

/// What a condition loop repeats while.
#[derive(Debug, Clone, PartialEq)]
pub struct Test {
    /// `until` repeats while the condition is *zero*; `while` while non-zero.
    pub until: bool,
    pub cond: Expr,
}

/// One executable step. Positions are indices into [`Program::ops`].
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// A blank line, or nothing at all. Executing it moves on.
    Nop,
    /// `:name`. Executing it moves on; `goto` targets it.
    Label(String),

    Assign {
        target: String,
        index: Option<Expr>,
        value: Expr,
    },

    /// A command whose parameters are still tokens, so that the command table
    /// is the only place that knows what they mean.
    Command {
        name: String,
        args: Vec<Vec<Tok>>,
    },

    /// `if <cond> then`: a branch of a block, tested at run time.
    If {
        cond: Expr,
        next: usize,
        endif: usize,
    },
    /// `elseif <cond> then`. Reached by falling out of a taken branch (then it
    /// goes to `endif`) or by a false condition before it (then it is tested).
    ElseIf {
        cond: Expr,
        next: usize,
        endif: usize,
    },
    Else {
        endif: usize,
    },
    EndIf,
    /// The single-line `if <cond> <statement>`: skip one statement when false.
    IfOnce {
        cond: Expr,
        next: usize,
    },

    While {
        cond: Expr,
        end: usize,
    },
    EndWhile {
        start: usize,
    },
    Until {
        cond: Expr,
        end: usize,
    },
    EndUntil {
        start: usize,
    },
    Do {
        pre: Option<Test>,
        end: usize,
    },
    Loop {
        post: Option<Test>,
        start: usize,
    },
    For {
        var: String,
        first: Expr,
        last: Expr,
        end: usize,
    },
    Next {
        start: usize,
    },

    Break {
        to: usize,
    },
    Continue {
        to: usize,
    },

    Goto {
        label: String,
        target: usize,
        /// Loop nesting at the target: a jump out of a loop discards the
        /// loop's bookkeeping, and a jump deeper in keeps it.
        loops: usize,
    },
    Call {
        label: String,
        target: usize,
    },
    Return,

    /// `end`: the macro ends, closing its window.
    End,
    /// `exit`: in an included file, return to the includer; in the main file,
    /// the same as [`Op::End`] (§6.7).
    Exit,
    SetExitCode {
        value: Expr,
    },
    Include {
        path: Expr,
    },
}

/// A macro ready to run.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub ops: Vec<Op>,
    /// The source line each op came from, parallel to `ops`, for error
    /// messages.
    pub lines: Vec<usize>,
    /// Lowercased label name to the position of its [`Op::Label`].
    pub labels: BTreeMap<String, usize>,
}

#[derive(Debug)]
pub enum BuildError {
    /// A statement that does not parse.
    Syntax {
        line: usize,
        detail: String,
    },
    /// `else`, `elseif` or `endif` with no `if` open, or a closer that does
    /// not match its opener.
    InvalidControl {
        line: usize,
        what: &'static str,
    },
    LabelAlreadyDefined {
        line: usize,
        name: String,
    },
    LabelRequired {
        line: usize,
        name: String,
    },
    TooManyLabels {
        line: usize,
    },
    /// `break` or `continue` with no loop around it.
    OutsideLoop {
        line: usize,
        what: &'static str,
    },
    Unclosed {
        line: usize,
        what: &'static str,
    },
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { line, detail } => write!(f, "line {line}: syntax error: {detail}"),
            Self::InvalidControl { line, what } => {
                write!(f, "line {line}: {what} has no matching if")
            }
            Self::LabelAlreadyDefined { line, name } => {
                write!(f, "line {line}: label {name} already defined")
            }
            Self::LabelRequired { line, name } => {
                write!(f, "line {line}: no label {name} in this file")
            }
            Self::TooManyLabels { line } => {
                write!(f, "line {line}: more than {MAX_LABELS} labels")
            }
            Self::OutsideLoop { line, what } => {
                write!(f, "line {line}: {what} outside a loop")
            }
            Self::Unclosed { line, what } => {
                write!(f, "line {line}: this block is never closed with {what}")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// A macro that could not be read.
#[derive(Debug)]
pub enum LoadError {
    Scan(ScanError),
    Build(BuildError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scan(error) => write!(f, "{error}"),
            Self::Build(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<ScanError> for LoadError {
    fn from(error: ScanError) -> Self {
        Self::Scan(error)
    }
}

impl From<BuildError> for LoadError {
    fn from(error: BuildError) -> Self {
        Self::Build(error)
    }
}

impl Program {
    /// Scan and build a macro's text in one step.
    pub fn parse(text: &str) -> Result<Self, LoadError> {
        Ok(Self::build(&line::scan(text)?)?)
    }

    /// Build a macro from lines that are already classified.
    pub fn build(lines: &[Line]) -> Result<Self, BuildError> {
        let mut builder = Builder::default();
        for line in lines {
            builder.line(line)?;
        }
        builder.finish()
    }

    /// The position a label names, case-insensitively.
    pub fn label(&self, name: &str) -> Option<usize> {
        self.labels.get(&name.to_ascii_lowercase()).copied()
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn op(&self, at: usize) -> Option<&Op> {
        self.ops.get(at)
    }

    /// The source line an op came from, one-based.
    pub fn line_of(&self, at: usize) -> Option<usize> {
        self.lines.get(at).copied()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoopKind {
    While,
    Until,
    Do,
    For,
}

impl LoopKind {
    fn closer(self) -> &'static str {
        match self {
            Self::While => "endwhile",
            Self::Until => "enduntil",
            Self::Do => "loop",
            Self::For => "next",
        }
    }
}

struct IfFrame {
    line: usize,
    /// Positions of the `If`, `ElseIf` and `Else` ops, in order.
    branches: Vec<usize>,
}

struct LoopFrame {
    line: usize,
    /// Position of the opener op.
    open: usize,
    kind: LoopKind,
    /// Positions of the `break`/`continue` ops inside, patched when the loop
    /// closes because only then is the target known.
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

#[derive(Default)]
struct Builder {
    ops: Vec<Op>,
    /// The source line of each op, parallel to `ops`.
    lines: Vec<usize>,
    /// Loop nesting at each op, parallel to `ops`, kept for `goto`.
    depths: Vec<usize>,
    labels: BTreeMap<String, usize>,
    ifs: Vec<IfFrame>,
    loops: Vec<LoopFrame>,
    gotos: Vec<(usize, String, usize)>,
    calls: Vec<(usize, String, usize)>,
}

impl Builder {
    fn line(&mut self, line: &Line) -> Result<(), BuildError> {
        match &line.kind {
            LineKind::Blank => Ok(()),
            LineKind::Label(name) => self.label(name, line.number),
            LineKind::Assign {
                target,
                index,
                value,
            } => {
                let index = match index {
                    Some(toks) => Some(self.expr(toks, line.number)?),
                    None => None,
                };
                let value = self.expr(value, line.number)?;
                let op = Op::Assign {
                    target: target.clone(),
                    index,
                    value,
                };
                self.push(op, line.number);
                Ok(())
            }
            LineKind::Command { name, args } => self.command(name, args, line.number),
        }
    }

    fn push(&mut self, op: Op, line: usize) {
        self.depths.push(self.loops.len());
        self.lines.push(line);
        self.ops.push(op);
    }

    fn label(&mut self, name: &str, number: usize) -> Result<(), BuildError> {
        let key = name.to_ascii_lowercase();
        if self.labels.contains_key(&key) {
            return Err(BuildError::LabelAlreadyDefined {
                line: number,
                name: name.to_string(),
            });
        }
        if self.labels.len() >= MAX_LABELS {
            return Err(BuildError::TooManyLabels { line: number });
        }
        let pos = self.ops.len();
        self.push(Op::Label(key.clone()), number);
        self.labels.insert(key, pos);
        Ok(())
    }

    fn expr(&self, toks: &[Tok], number: usize) -> Result<Expr, BuildError> {
        expr::parse(toks).map_err(|error| BuildError::Syntax {
            line: number,
            detail: error.to_string(),
        })
    }

    fn command(&mut self, name: &str, args: &[Vec<Tok>], number: usize) -> Result<(), BuildError> {
        match name {
            "if" => self.if_(args, number),
            "elseif" => self.elseif(args, number),
            "else" => self.else_(number),
            "endif" => self.endif(number),

            "while" => {
                let cond = self.expr(&join(args), number)?;
                let pos = self.ops.len();
                self.push(Op::While { cond, end: 0 }, number);
                self.open_loop(pos, LoopKind::While, number);
                Ok(())
            }
            "until" => {
                let cond = self.expr(&join(args), number)?;
                let pos = self.ops.len();
                self.push(Op::Until { cond, end: 0 }, number);
                self.open_loop(pos, LoopKind::Until, number);
                Ok(())
            }
            "do" => {
                let pre = self.optional_test(args, number)?;
                let pos = self.ops.len();
                self.push(Op::Do { pre, end: 0 }, number);
                self.open_loop(pos, LoopKind::Do, number);
                Ok(())
            }
            "for" => {
                let (var, first, last) = self.for_parameters(args, number)?;
                let pos = self.ops.len();
                self.push(
                    Op::For {
                        var,
                        first,
                        last,
                        end: 0,
                    },
                    number,
                );
                self.open_loop(pos, LoopKind::For, number);
                Ok(())
            }

            "endwhile" => self.close_while(number),
            "enduntil" => self.close_until(number),
            "loop" => self.close_do(args, number),
            "next" => self.close_for(number),

            "break" => self.jump_out("break", number),
            "continue" => self.jump_out("continue", number),

            "goto" => {
                let name = label_name(args, number)?;
                let pos = self.ops.len();
                self.push(
                    Op::Goto {
                        label: name.clone(),
                        target: 0,
                        loops: 0,
                    },
                    number,
                );
                self.gotos.push((pos, name, number));
                Ok(())
            }
            "call" => {
                let name = label_name(args, number)?;
                let pos = self.ops.len();
                self.push(
                    Op::Call {
                        label: name.clone(),
                        target: 0,
                    },
                    number,
                );
                self.calls.push((pos, name, number));
                Ok(())
            }

            "return" => {
                self.push(Op::Return, number);
                Ok(())
            }
            "end" => {
                self.push(Op::End, number);
                Ok(())
            }
            "exit" => {
                self.push(Op::Exit, number);
                Ok(())
            }
            "setexitcode" => {
                let value = self.expr(&join(args), number)?;
                self.push(Op::SetExitCode { value }, number);
                Ok(())
            }
            "include" => {
                let path = self.expr(&join(args), number)?;
                self.push(Op::Include { path }, number);
                Ok(())
            }

            _ => {
                self.push(
                    Op::Command {
                        name: name.to_string(),
                        args: args.to_vec(),
                    },
                    number,
                );
                Ok(())
            }
        }
    }

    fn if_(&mut self, args: &[Vec<Tok>], number: usize) -> Result<(), BuildError> {
        match then_index(args) {
            Some(at) => {
                // The block form. `then` closes the line; a statement after it
                // would be silently unreachable, so it is refused.
                if at + 1 != args.len() || args[at].len() != 1 {
                    return Err(BuildError::Syntax {
                        line: number,
                        detail: "nothing may follow `then`".into(),
                    });
                }
                let cond = self.expr(&join(&args[..at]), number)?;
                let pos = self.ops.len();
                self.push(
                    Op::If {
                        cond,
                        next: 0,
                        endif: 0,
                    },
                    number,
                );
                self.ifs.push(IfFrame {
                    line: number,
                    branches: vec![pos],
                });
                Ok(())
            }
            None => {
                let toks = join(args);
                let (cond, used) =
                    expr::parse_prefix(&toks).map_err(|error| BuildError::Syntax {
                        line: number,
                        detail: error.to_string(),
                    })?;
                if used == 0 || used >= toks.len() {
                    return Err(BuildError::Syntax {
                        line: number,
                        detail: "`if` wants a condition and a statement".into(),
                    });
                }
                let pos = self.ops.len();
                self.push(Op::IfOnce { cond, next: 0 }, number);
                self.statement(&toks[used..], number)?;
                if self.ops.len() != pos + 2 {
                    return Err(BuildError::Syntax {
                        line: number,
                        detail: "`if` applies to exactly one statement".into(),
                    });
                }
                let after = self.ops.len();
                if let Op::IfOnce { next, .. } = &mut self.ops[pos] {
                    *next = after;
                }
                Ok(())
            }
        }
    }

    /// One statement, classified the same way a line is.
    fn statement(&mut self, toks: &[Tok], number: usize) -> Result<(), BuildError> {
        let kind = line::classify(toks, number).map_err(|error| BuildError::Syntax {
            line: number,
            detail: error.to_string(),
        })?;
        self.line(&Line { number, kind })
    }

    fn elseif(&mut self, args: &[Vec<Tok>], number: usize) -> Result<(), BuildError> {
        let cond = self.expr(&condition_tokens(args), number)?;
        let index = self
            .ifs
            .len()
            .checked_sub(1)
            .ok_or(BuildError::InvalidControl {
                line: number,
                what: "elseif",
            })?;
        let previous = *self.ifs[index]
            .branches
            .last()
            .expect("an if pushes one branch");
        let pos = self.ops.len();
        self.patch_next(previous, pos);
        self.push(
            Op::ElseIf {
                cond,
                next: 0,
                endif: 0,
            },
            number,
        );
        self.ifs[index].branches.push(pos);
        Ok(())
    }

    fn else_(&mut self, number: usize) -> Result<(), BuildError> {
        let index = self
            .ifs
            .len()
            .checked_sub(1)
            .ok_or(BuildError::InvalidControl {
                line: number,
                what: "else",
            })?;
        let previous = *self.ifs[index]
            .branches
            .last()
            .expect("an if pushes one branch");
        let pos = self.ops.len();
        self.patch_next(previous, pos);
        self.push(Op::Else { endif: 0 }, number);
        self.ifs[index].branches.push(pos);
        Ok(())
    }

    fn endif(&mut self, number: usize) -> Result<(), BuildError> {
        let frame = self.ifs.pop().ok_or(BuildError::InvalidControl {
            line: number,
            what: "endif",
        })?;
        let pos = self.ops.len();
        // Only the last branch falls through to `endif` when it ran; the
        // earlier branches already point at the next branch.
        if let Some(&last) = frame.branches.last() {
            self.patch_next(last, pos);
        }
        for &branch in &frame.branches {
            self.patch_endif(branch, pos);
        }
        self.push(Op::EndIf, number);
        Ok(())
    }

    fn patch_next(&mut self, at: usize, to: usize) {
        match &mut self.ops[at] {
            Op::If { next, .. } | Op::ElseIf { next, .. } | Op::IfOnce { next, .. } => *next = to,
            _ => {}
        }
    }

    fn patch_endif(&mut self, at: usize, to: usize) {
        match &mut self.ops[at] {
            Op::If { endif, .. } | Op::ElseIf { endif, .. } | Op::Else { endif } => *endif = to,
            _ => {}
        }
    }

    fn open_loop(&mut self, open: usize, kind: LoopKind, line: usize) {
        self.loops.push(LoopFrame {
            line,
            open,
            kind,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
    }

    fn take_loop(&mut self, kind: LoopKind, number: usize) -> Result<LoopFrame, BuildError> {
        match self.loops.last() {
            Some(frame) if frame.kind == kind => Ok(self.loops.pop().expect("just matched")),
            _ => Err(BuildError::InvalidControl {
                line: number,
                what: kind.closer(),
            }),
        }
    }

    /// Close a `while`-shaped loop: the opener learns where it ends, and every
    /// `break`/`continue` inside learns where it goes.
    fn close_while(&mut self, number: usize) -> Result<(), BuildError> {
        let frame = self.take_loop(LoopKind::While, number)?;
        let pos = self.ops.len();
        if let Op::While { end, .. } = &mut self.ops[frame.open] {
            *end = pos;
        }
        self.patch_jumps(&frame, pos, frame.open);
        self.push(Op::EndWhile { start: frame.open }, number);
        Ok(())
    }

    fn close_until(&mut self, number: usize) -> Result<(), BuildError> {
        let frame = self.take_loop(LoopKind::Until, number)?;
        let pos = self.ops.len();
        if let Op::Until { end, .. } = &mut self.ops[frame.open] {
            *end = pos;
        }
        self.patch_jumps(&frame, pos, frame.open);
        self.push(Op::EndUntil { start: frame.open }, number);
        Ok(())
    }

    fn close_do(&mut self, args: &[Vec<Tok>], number: usize) -> Result<(), BuildError> {
        let post = self.optional_test(args, number)?;
        let frame = self.take_loop(LoopKind::Do, number)?;
        let pos = self.ops.len();
        if let Op::Do { end, .. } = &mut self.ops[frame.open] {
            *end = pos;
        }
        // `continue` in a `do` goes to the `loop`, which is where the
        // condition is tested.
        self.patch_jumps(&frame, pos, pos);
        self.push(
            Op::Loop {
                post,
                start: frame.open,
            },
            number,
        );
        Ok(())
    }

    fn close_for(&mut self, number: usize) -> Result<(), BuildError> {
        let frame = self.take_loop(LoopKind::For, number)?;
        let pos = self.ops.len();
        if let Op::For { end, .. } = &mut self.ops[frame.open] {
            *end = pos;
        }
        // `continue` in a `for` goes to `next`, which increments and tests.
        self.patch_jumps(&frame, pos, pos);
        self.push(Op::Next { start: frame.open }, number);
        Ok(())
    }

    fn patch_jumps(&mut self, frame: &LoopFrame, end: usize, continue_to: usize) {
        for &at in &frame.breaks {
            if let Op::Break { to } = &mut self.ops[at] {
                *to = end + 1;
            }
        }
        for &at in &frame.continues {
            if let Op::Continue { to } = &mut self.ops[at] {
                *to = continue_to;
            }
        }
    }

    fn jump_out(&mut self, what: &'static str, number: usize) -> Result<(), BuildError> {
        if self.loops.is_empty() {
            return Err(BuildError::OutsideLoop { line: number, what });
        }
        let pos = self.ops.len();
        let op = if what == "break" {
            Op::Break { to: 0 }
        } else {
            Op::Continue { to: 0 }
        };
        self.push(op, number);
        let frame = self.loops.last_mut().expect("just checked");
        if what == "break" {
            frame.breaks.push(pos);
        } else {
            frame.continues.push(pos);
        }
        Ok(())
    }

    /// `do [while|until <cond>]` and `loop [while|until <cond>]`.
    fn optional_test(&self, args: &[Vec<Tok>], number: usize) -> Result<Option<Test>, BuildError> {
        let Some(first) = args.first() else {
            return Ok(None);
        };
        let until = match word(first).as_deref() {
            Some("while") => false,
            Some("until") => true,
            _ => {
                return Err(BuildError::Syntax {
                    line: number,
                    detail: "expected `while` or `until`".into(),
                });
            }
        };
        let cond = self.expr(&join(&args[1..]), number)?;
        Ok(Some(Test { until, cond }))
    }

    fn for_parameters(
        &self,
        args: &[Vec<Tok>],
        number: usize,
    ) -> Result<(String, Expr, Expr), BuildError> {
        if args.len() != 3 {
            return Err(BuildError::Syntax {
                line: number,
                detail: "`for` wants a variable and two limits".into(),
            });
        }
        let var = match &args[0][..] {
            [tok] => match &tok.kind {
                Token::Ident(name) => name.clone(),
                _ => {
                    return Err(BuildError::Syntax {
                        line: number,
                        detail: "`for` wants an integer variable".into(),
                    });
                }
            },
            _ => {
                return Err(BuildError::Syntax {
                    line: number,
                    detail: "`for` wants an integer variable".into(),
                });
            }
        };
        let first = self.expr(&args[1], number)?;
        let last = self.expr(&args[2], number)?;
        Ok((var, first, last))
    }

    fn finish(mut self) -> Result<Program, BuildError> {
        if let Some(frame) = self.ifs.pop() {
            return Err(BuildError::Unclosed {
                line: frame.line,
                what: "endif",
            });
        }
        if let Some(frame) = self.loops.pop() {
            return Err(BuildError::Unclosed {
                line: frame.line,
                what: frame.kind.closer(),
            });
        }
        for (at, name, number) in self.gotos {
            let target = *self.labels.get(&name.to_ascii_lowercase()).ok_or_else(|| {
                BuildError::LabelRequired {
                    line: number,
                    name: name.clone(),
                }
            })?;
            let loops = self.depths[target];
            if let Op::Goto {
                target: to,
                loops: l,
                ..
            } = &mut self.ops[at]
            {
                *to = target;
                *l = loops;
            }
        }
        for (at, name, number) in self.calls {
            let target = *self.labels.get(&name.to_ascii_lowercase()).ok_or_else(|| {
                BuildError::LabelRequired {
                    line: number,
                    name: name.clone(),
                }
            })?;
            if let Op::Call { target: to, .. } = &mut self.ops[at] {
                *to = target;
            }
        }
        Ok(Program {
            ops: self.ops,
            lines: self.lines,
            labels: self.labels,
        })
    }
}

/// All parameters as one token run, so a condition split over several
/// whitespace-separated parameters (`if a = 1 then`) parses as an expression.
fn join(args: &[Vec<Tok>]) -> Vec<Tok> {
    args.iter()
        .flat_map(|group| group.iter().cloned())
        .collect()
}

/// A parameter that is exactly one bare word, lowercased.
fn word(group: &[Tok]) -> Option<String> {
    match group {
        [tok] => match &tok.kind {
            Token::Ident(name) => Some(name.to_ascii_lowercase()),
            _ => None,
        },
        _ => None,
    }
}

/// Position of the `then` parameter, if the line has one.
fn then_index(args: &[Vec<Tok>]) -> Option<usize> {
    args.iter()
        .position(|group| word(group).as_deref() == Some("then"))
}

/// The condition of an `elseif`: everything, minus a trailing `then`.
fn condition_tokens(args: &[Vec<Tok>]) -> Vec<Tok> {
    match args.last() {
        Some(group) if word(group).as_deref() == Some("then") => join(&args[..args.len() - 1]),
        _ => join(args),
    }
}

/// The single parameter of `goto`/`call`. A label may be all digits (§3.2), so
/// a number token names one too.
fn label_name(args: &[Vec<Tok>], number: usize) -> Result<String, BuildError> {
    match args {
        [group] => match &group[..] {
            [tok] => match &tok.kind {
                Token::Ident(name) => Ok(name.clone()),
                Token::Number(value) => Ok(value.to_string()),
                _ => Err(BuildError::Syntax {
                    line: number,
                    detail: "expected a label name".into(),
                }),
            },
            _ => Err(BuildError::Syntax {
                line: number,
                detail: "expected a label name".into(),
            }),
        },
        _ => Err(BuildError::Syntax {
            line: number,
            detail: "expected one label name".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(text: &str) -> Program {
        Program::parse(text).unwrap()
    }

    fn error(text: &str) -> BuildError {
        Program::parse(text).unwrap_err().into_build()
    }

    impl LoadError {
        fn into_build(self) -> BuildError {
            match self {
                Self::Build(error) => error,
                Self::Scan(error) => panic!("expected a build error, got {error}"),
            }
        }
    }

    #[test]
    fn an_assignment_is_one_op() {
        let p = program("A = 1 + 2\n");
        assert_eq!(p.len(), 1);
        assert!(matches!(&p.ops[0], Op::Assign { target, index: None, .. } if target == "A"));
    }

    #[test]
    fn an_indexed_assignment_keeps_its_index() {
        let p = program("a[i * 2] = 'x'\n");
        let Op::Assign { index: Some(_), .. } = &p.ops[0] else {
            panic!("expected an index: {:?}", p.ops[0]);
        };
    }

    #[test]
    fn a_block_if_learns_where_its_branches_go() {
        let p = program(
            "if a then\n\
             \x20   A = 1\n\
             \x20   ; a comment line does not become an op\n\
             elseif b then\n\
             \x20   A = 2\n\
             else\n\
             \x20   A = 3\n\
             endif\n\
             A = 4\n",
        );
        // 0 If, 1 A=1, 2 ElseIf, 3 A=2, 4 Else, 5 A=3, 6 EndIf, 7 A=4
        assert_eq!(p.len(), 8, "{:?}", p.ops);
        assert!(
            matches!(
                &p.ops[0],
                Op::If {
                    next: 2,
                    endif: 6,
                    ..
                }
            ),
            "{:?}",
            p.ops[0]
        );
        assert!(
            matches!(
                &p.ops[2],
                Op::ElseIf {
                    next: 4,
                    endif: 6,
                    ..
                }
            ),
            "{:?}",
            p.ops[2]
        );
        assert!(matches!(&p.ops[4], Op::Else { endif: 6 }), "{:?}", p.ops[4]);
        assert!(matches!(p.ops[6], Op::EndIf));
    }

    #[test]
    fn a_block_if_without_else_jumps_to_its_endif() {
        let p = program("if a then\nA = 1\nendif\n");
        assert!(matches!(
            &p.ops[0],
            Op::If {
                next: 2,
                endif: 2,
                ..
            }
        ));
    }

    #[test]
    fn the_single_line_if_skips_exactly_one_statement() {
        let p = program("if result=0 setexitcode 0\nA = 1\n:done\n");
        // 0 IfOnce(next 2), 1 SetExitCode, 2 A=1, 3 Label
        assert_eq!(p.len(), 4, "{:?}", p.ops);
        assert!(
            matches!(&p.ops[0], Op::IfOnce { next: 2, .. }),
            "{:?}",
            p.ops[0]
        );
        assert!(matches!(p.ops[1], Op::SetExitCode { .. }));
    }

    #[test]
    fn the_single_line_if_can_assign_or_jump() {
        let p = program("if result A = 0\nif A>1 goto done\n:done\n");
        assert!(matches!(&p.ops[0], Op::IfOnce { next: 2, .. }));
        assert!(matches!(p.ops[1], Op::Assign { .. }));
        assert!(matches!(&p.ops[2], Op::IfOnce { next: 4, .. }));
        assert!(matches!(
            &p.ops[3],
            Op::Goto {
                target: 4,
                loops: 0,
                ..
            }
        ));
        assert!(matches!(&p.ops[4], Op::Label(name) if name == "done"));
    }

    #[test]
    fn goto_finds_a_label_after_it_and_one_before() {
        let p = program(":start\nA = 1\ngoto start\ngoto 10\n:10\n");
        assert_eq!(p.label("start"), Some(0));
        assert_eq!(p.label("10"), Some(4));
        assert!(matches!(&p.ops[2], Op::Goto { target: 0, .. }));
        assert!(
            matches!(&p.ops[3], Op::Goto { target: 4, .. }),
            "{:?}",
            p.ops[3]
        );
    }

    #[test]
    fn a_goto_out_of_a_loop_records_the_nesting_it_lands_in() {
        let p = program(":outside\nfor i 1 3\n:inside\ngoto inside\ngoto outside\nnext\n");
        // 0 Label(outside), 1 For, 2 Label(inside), 3 Goto inside, 4 Goto outside, 5 Next
        assert!(
            matches!(&p.ops[3], Op::Goto { loops: 1, .. }),
            "{:?}",
            p.ops[3]
        );
        assert!(
            matches!(&p.ops[4], Op::Goto { loops: 0, .. }),
            "{:?}",
            p.ops[4]
        );
    }

    #[test]
    fn break_and_continue_take_the_innermost_loop() {
        let p = program(
            "for i 1 3\n\
             \x20   while 1\n\
             \x20       if x break\n\
             \x20       continue\n\
             \x20   endwhile\n\
             next\n\
             A = 1\n",
        );
        // 0 For, 1 While, 2 IfOnce, 3 Break, 4 Continue, 5 EndWhile, 6 Next, 7 A=1
        assert!(
            matches!(&p.ops[0], Op::For { end: 6, .. }),
            "{:?}",
            p.ops[0]
        );
        assert!(
            matches!(&p.ops[1], Op::While { end: 5, .. }),
            "{:?}",
            p.ops[1]
        );
        assert!(matches!(&p.ops[3], Op::Break { to: 6 }), "{:?}", p.ops[3]);
        assert!(
            matches!(&p.ops[4], Op::Continue { to: 1 }),
            "{:?}",
            p.ops[4]
        );
    }

    #[test]
    fn a_break_leaves_the_loop_it_is_in() {
        let p = program("for i 1 3\nbreak\nnext\nA=1\n");
        // 0 For(end 2), 1 Break(to 3), 2 Next, 3 A=1
        assert!(matches!(&p.ops[1], Op::Break { to: 3 }), "{:?}", p.ops[1]);
    }

    #[test]
    fn continue_in_a_for_goes_through_next() {
        let p = program("for i 1 3\ncontinue\nnext\n");
        // `next` is what increments, so continue must land there.
        assert!(
            matches!(&p.ops[1], Op::Continue { to: 2 }),
            "{:?}",
            p.ops[1]
        );
    }

    #[test]
    fn continue_in_a_do_goes_to_the_loop_test() {
        let p = program("do\ncontinue\nloop while x\n");
        assert!(
            matches!(&p.ops[1], Op::Continue { to: 2 }),
            "{:?}",
            p.ops[1]
        );
        assert!(matches!(&p.ops[0], Op::Do { pre: None, end: 2 }));
        assert!(matches!(
            &p.ops[2],
            Op::Loop {
                post: Some(Test { until: false, .. }),
                start: 0
            }
        ));
    }

    #[test]
    fn until_and_do_keep_their_conditions() {
        let p = program("until i > 10\nenduntil\n");
        assert!(matches!(&p.ops[0], Op::Until { end: 1, .. }));
        assert!(matches!(&p.ops[1], Op::EndUntil { start: 0 }));

        let p = program("do until x\nloop\n");
        assert!(matches!(
            &p.ops[0],
            Op::Do {
                pre: Some(Test { until: true, .. }),
                end: 1
            }
        ));
        assert!(matches!(
            &p.ops[1],
            Op::Loop {
                post: None,
                start: 0
            }
        ));
    }

    #[test]
    fn a_for_loop_keeps_its_variable() {
        let p = program("for i 1 10\nnext\n");
        assert!(matches!(&p.ops[0], Op::For { var, end: 1, .. } if var == "i"));
        assert!(matches!(&p.ops[1], Op::Next { start: 0 }));
    }

    #[test]
    fn a_for_loop_needs_three_parameters() {
        assert!(matches!(
            error("for i 1\nnext\n"),
            BuildError::Syntax { .. }
        ));
        assert!(matches!(
            error("for 1 2 3\nnext\n"),
            BuildError::Syntax { .. }
        ));
    }

    #[test]
    fn a_negative_limit_is_a_unary_minus_not_a_missing_parameter() {
        // `for i 5 -1` is three parameters: the minus binds to the 1.
        let p = program("for i 5 -1\nnext\n");
        assert!(matches!(
            &p.ops[0],
            Op::For {
                last: expr::Expr::Unary(..),
                ..
            }
        ));
    }

    #[test]
    fn labels_are_case_insensitive_and_unique() {
        let p = program(":Retry\ngoto retry\n");
        assert_eq!(p.label("RETRY"), Some(0));
        assert!(matches!(
            error(":a\n:A\n"),
            BuildError::LabelAlreadyDefined { .. }
        ));
    }

    #[test]
    fn more_than_512_labels_is_an_error() {
        let mut text = String::new();
        for i in 0..=MAX_LABELS {
            text.push_str(&format!(":l{i}\n"));
        }
        assert!(matches!(
            Program::parse(&text),
            Err(LoadError::Build(BuildError::TooManyLabels { .. }))
        ));
    }

    #[test]
    fn a_missing_label_is_refused_before_the_macro_runs() {
        assert!(matches!(
            error("goto nowhere\n"),
            BuildError::LabelRequired { .. }
        ));
        assert!(matches!(
            error("call nowhere\n"),
            BuildError::LabelRequired { .. }
        ));
    }

    #[test]
    fn invalid_control_is_refused() {
        assert!(matches!(
            error("else\n"),
            BuildError::InvalidControl { what: "else", .. }
        ));
        assert!(matches!(
            error("endif\n"),
            BuildError::InvalidControl { what: "endif", .. }
        ));
        assert!(matches!(
            error("if a then\nendwhile\n"),
            BuildError::InvalidControl {
                what: "endwhile",
                ..
            }
        ));
        assert!(matches!(
            error("break\n"),
            BuildError::OutsideLoop { what: "break", .. }
        ));
    }

    #[test]
    fn an_unclosed_block_is_refused() {
        assert!(matches!(
            error("if a then\nA=1\n"),
            BuildError::Unclosed { what: "endif", .. }
        ));
        assert!(matches!(
            error("for i 1 3\n"),
            BuildError::Unclosed { what: "next", .. }
        ));
        assert!(matches!(
            error("do\n"),
            BuildError::Unclosed { what: "loop", .. }
        ));
    }

    #[test]
    fn then_must_end_the_line() {
        assert!(matches!(
            error("if a then goto x\n:x\n"),
            BuildError::Syntax { .. }
        ));
    }

    #[test]
    fn command_parameters_are_left_for_the_command_table() {
        let p = program("MESSAGEBOX 'hi' 'title'\n");
        let Op::Command { name, args } = &p.ops[0] else {
            panic!("not a command: {:?}", p.ops[0]);
        };
        assert_eq!(name, "messagebox");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn command_names_are_case_insensitive() {
        let p = program("SENDLN 'x'\n");
        assert!(matches!(&p.ops[0], Op::Command { name, .. } if name == "sendln"));
        // …including the structural ones.
        let p = program("IF a THEN\nENDIF\n");
        assert!(matches!(&p.ops[0], Op::If { .. }), "{:?}", p.ops[0]);
    }

    #[test]
    fn end_exit_and_setexitcode_are_their_own_ops() {
        let p = program("setexitcode 2\nend\nexit\n");
        assert!(matches!(&p.ops[0], Op::SetExitCode { .. }));
        assert!(matches!(p.ops[1], Op::End));
        // `exit` differs from `end` only in an included file (§6.7), but they
        // are separate ops because that difference is the whole point.
        assert!(matches!(p.ops[2], Op::Exit));
    }

    #[test]
    fn a_syntax_error_names_its_line() {
        match Program::parse("A = 1\nB = (\n") {
            Err(LoadError::Build(BuildError::Syntax { line, .. })) => assert_eq!(line, 2),
            other => panic!("expected a syntax error on line 2, got {other:?}"),
        }
    }
}

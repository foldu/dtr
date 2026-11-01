//! The interpreter: a program counter over [`crate::program`]'s ops.
//!
//! §1.3 says a macro is interpreted line by line, and that is what this is —
//! the ops are already flat and every jump is resolved, so execution is a
//! `while` loop over `program.ops[pc]` with a small amount of state around it:
//!
//! * **`vars`** is §4's namespace (four kinds, four budgets), and **`system`**
//!   holds the variables the interpreter owns — `result`, `inputstr`,
//!   `timeout`, `matchstr`, `groupmatchstr1`…`9`, `param1`…`9`. Assignment and
//!   reading both look at the system names first, because Tera Term's are not
//!   shadowable.
//! * **`files`** is the stack of running files. `include` (§6.6) pushes one and
//!   reaching its end pops it, which is also what `exit` does inside an
//!   included file; a `call` does not push a file, it pushes a return address
//!   onto the same file's call stack, because a subroutine may not cross an
//!   `include` (§6.4, `Can't call sub.`).
//! * **`loops`** is only needed for `for`: its step and limit are evaluated
//!   once, at the `for`, and `next` reads them back. `while`, `until` and `do`
//!   carry their targets in the op, so they need nothing.
//!
//! Nothing here sleeps, draws a dialog or reads a file itself — that is
//! [`crate::host::Host`], so the same interpreter drives a real Tera Term
//! window and a test that answers `messagebox` with a string.
//!
//! Two deliberate choices, both documented where they happen: a macro that
//! runs more than [`DEFAULT_STEP_LIMIT`] ops is stopped with [`Error::StepLimit`]
//! (Tera Term would run until somebody closed the window), and `return` with
//! nothing to return from ends the macro instead of inventing an error message
//! that §8 does not have.

use std::fmt;

use crate::error::Error;
use crate::expr::{self, Expr, Scope};
use crate::host::Host;
use crate::lex::Tok;
use crate::program::{Op, Program};
use crate::source;
use crate::value::Value;
use crate::vars::Vars;

/// §3.8: `include` may nest nine deep.
pub const MAX_INCLUDE_DEPTH: usize = 9;

/// How deep `call` may nest before `Stack overflow.` (§6.4, §8).
pub const MAX_CALL_DEPTH: usize = 1000;

/// A macro that executes more ops than this is stopped. High enough that no
/// real macro reaches it, low enough that a runaway loop fails a test in
/// seconds instead of hanging it.
pub const DEFAULT_STEP_LIMIT: u64 = 2_000_000;

/// An interpreter error, with the line of the file it happened in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub error: Error,
    /// One-based, in the file that was running.
    pub line: usize,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.error)
    }
}

impl std::error::Error for Failure {}

/// §4.7 and §7.6: the variables the interpreter owns rather than the macro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemVars {
    /// The error channel (§8): the last command's status, 0 when it succeeded.
    pub result: i32,
    /// Seconds `wait` waits for (§7.1).
    pub timeout: i32,
    /// Milliseconds `wait` waits for (4.78+).
    pub mtimeout: i32,
    /// Where `inputbox`, `passwordbox`, `sprintf` and `filenamebox` leave
    /// their answer.
    pub inputstr: String,
    /// The whole match of `strmatch`/`strreplace`.
    pub matchstr: String,
    /// `groupmatchstr1`…`groupmatchstr9`, at indices 0…8.
    pub groupmatchstr: [String; 9],
    /// `params` of the macro's command line, exposed as `param1`…`param9`,
    /// `params[]` and `paramcnt` (§4.7).
    pub params: Vec<String>,
}

impl Default for SystemVars {
    fn default() -> Self {
        Self {
            result: 0,
            timeout: 0,
            mtimeout: 0,
            inputstr: String::new(),
            matchstr: String::new(),
            groupmatchstr: Default::default(),
            params: Vec::new(),
        }
    }
}

/// The reading half of the system variables: what an expression sees.
struct View<'a> {
    vars: &'a Vars,
    system: &'a SystemVars,
}

/// `param1`…`param9` when the name is one of them.
fn param_number(name: &str) -> Option<usize> {
    let rest = name.strip_prefix("param")?;
    if rest.len() != 1 {
        return None;
    }
    let digit = rest.as_bytes()[0];
    (b'1'..=b'9')
        .contains(&digit)
        .then(|| usize::from(digit - b'1'))
}

/// `groupmatchstr1`…`groupmatchstr9` when the name is one of them.
fn group_number(name: &str) -> Option<usize> {
    let rest = name.strip_prefix("groupmatchstr")?;
    if rest.len() != 1 {
        return None;
    }
    let digit = rest.as_bytes()[0];
    (b'1'..=b'9')
        .contains(&digit)
        .then(|| usize::from(digit - b'1'))
}

impl Scope for View<'_> {
    fn value(&self, name: &str, index: Option<i32>) -> Result<Value, Error> {
        let lower = name.to_ascii_lowercase();
        if let Some(group) = group_number(&lower) {
            return Ok(Value::Str(self.system.groupmatchstr[group].clone()));
        }
        let scalar = match (lower.as_str(), index) {
            ("result", None) => Some(Value::Int(self.system.result)),
            ("timeout", None) => Some(Value::Int(self.system.timeout)),
            ("mtimeout", None) => Some(Value::Int(self.system.mtimeout)),
            ("inputstr", None) => Some(Value::Str(self.system.inputstr.clone())),
            ("matchstr", None) => Some(Value::Str(self.system.matchstr.clone())),
            ("paramcnt", None) => Some(Value::Int(self.system.params.len() as i32)),
            ("params", Some(index)) => {
                let item = usize::try_from(index)
                    .ok()
                    .and_then(|i| self.system.params.get(i));
                match item {
                    Some(text) => Some(Value::Str(text.clone())),
                    None => {
                        return Err(Error::IndexOutOfRange {
                            name: name.to_string(),
                            index,
                        });
                    }
                }
            }
            (name, None) => param_number(name)
                .map(|i| self.system.params.get(i).cloned().unwrap_or_default())
                .map(Value::Str),
            _ => None,
        };
        match scalar {
            Some(value) => Ok(value),
            None => self.vars.value(name, index),
        }
    }
}

/// One `for` loop that is running.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ForState {
    var: String,
    end: usize,
    last: i32,
    step: i32,
}

/// One `if`/`elseif` whose branch was taken, so that falling out of it lands on
/// the next `elseif` and jumps to the `endif`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Taken {
    endif: usize,
}

/// One running file: the main macro, or something it included.
struct Exec {
    program: Program,
    pc: usize,
    /// Return addresses, with the loop and branch nesting to restore.
    calls: Vec<(usize, usize, usize)>,
    loops: Vec<ForState>,
    ifs: Vec<Taken>,
}

impl Exec {
    fn new(program: Program) -> Self {
        Self {
            program,
            pc: 0,
            calls: Vec::new(),
            loops: Vec::new(),
            ifs: Vec::new(),
        }
    }
}

pub struct Interpreter<'h> {
    host: &'h mut dyn Host,
    vars: Vars,
    system: SystemVars,
    files: Vec<Exec>,
    exit_code: i32,
    steps: u64,
    max_steps: u64,
    /// `random`'s state; a macro that wants two different numbers must not get
    /// the same one twice.
    seed: u32,
    /// §7.5's password store, keyed by (file, name).
    ///
    /// Tera Term's file is encrypted and this one is not: what a macro needs is
    /// that `setpassword` followed by `getpassword` returns what it stored, and
    /// that a file survives between them, which is what this gives. The
    /// encryption format is Tera Term's, not a thing a script can observe.
    passwords: std::collections::BTreeMap<(String, String), String>,
}

impl<'h> Interpreter<'h> {
    /// An interpreter with nothing to run yet.
    pub fn new(host: &'h mut dyn Host) -> Self {
        Self {
            host,
            vars: Vars::new(),
            system: SystemVars::default(),
            files: Vec::new(),
            exit_code: 0,
            steps: 0,
            max_steps: DEFAULT_STEP_LIMIT,
            seed: 0x2545_f491,
            passwords: std::collections::BTreeMap::new(),
        }
    }

    /// An interpreter that will run `program`.
    pub fn with_program(host: &'h mut dyn Host, program: Program) -> Self {
        let mut this = Self::new(host);
        this.files.push(Exec::new(program));
        this
    }

    /// Parse and run `text` in one step, returning the exit status.
    pub fn run_text(host: &'h mut dyn Host, text: &str) -> Result<i32, Failure> {
        let program = Program::parse(text).map_err(|error| Failure {
            error: Error::Syntax(error.to_string()),
            line: 0,
        })?;
        Self::with_program(host, program).run()
    }

    /// Give the macro its command-line parameters (`param1`…, `paramcnt`).
    pub fn with_params(mut self, params: Vec<String>) -> Self {
        self.system.params = params;
        self
    }

    /// Stop after `steps` ops. Mainly so a runaway loop in a test fails.
    pub fn with_step_limit(mut self, steps: u64) -> Self {
        self.max_steps = steps;
        self
    }

    pub fn vars(&self) -> &Vars {
        &self.vars
    }

    pub fn system(&self) -> &SystemVars {
        &self.system
    }

    /// The value of `result` after the run, so a test can read the error
    /// channel without reaching into the variables.
    pub fn result(&self) -> i32 {
        self.system.result
    }

    /// Run until the macro ends or an interpreter error stops it.
    pub fn run(&mut self) -> Result<i32, Failure> {
        loop {
            let finished = match self.files.last() {
                None => return Ok(self.exit_code),
                Some(exec) => exec.pc >= exec.program.len(),
            };
            if finished {
                self.leave_file();
                continue;
            }
            self.step()?;
        }
    }

    // -- the machine ------------------------------------------------------

    fn exec(&self) -> &Exec {
        self.files.last().expect("the run loop keeps one file")
    }

    fn exec_mut(&mut self) -> &mut Exec {
        self.files.last_mut().expect("the run loop keeps one file")
    }

    fn advance(&mut self) {
        self.exec_mut().pc += 1;
    }

    fn jump(&mut self, to: usize) {
        self.exec_mut().pc = to;
    }

    /// End the current file: the macro itself, or a file it included.
    fn leave_file(&mut self) {
        self.files.pop();
    }

    fn failure(&self, error: Error) -> Failure {
        let line = self
            .files
            .last()
            .map(|exec| exec.program.line_of(exec.pc).unwrap_or(0))
            .unwrap_or(0);
        Failure { error, line }
    }

    fn step(&mut self) -> Result<(), Failure> {
        self.steps += 1;
        if self.steps > self.max_steps {
            return Err(self.failure(Error::StepLimit));
        }
        let (op, line) = {
            let exec = self.exec();
            (
                exec.program.ops[exec.pc].clone(),
                exec.program.line_of(exec.pc).unwrap_or(1),
            )
        };
        self.execute(op).map_err(|error| Failure { error, line })
    }

    fn execute(&mut self, op: Op) -> Result<(), Error> {
        match op {
            Op::Nop | Op::Label(_) | Op::EndIf => self.advance(),

            Op::Assign {
                target,
                index,
                value,
            } => {
                let value = self.eval(&value)?;
                match index {
                    Some(index) => {
                        let index = self.eval_int(&index)?;
                        self.assign_index(&target, index, value)?;
                    }
                    None => self.assign(&target, value)?,
                }
                self.advance();
            }

            Op::Command { name, args } => {
                // Advance first: a command may push a file frame
                // (`include`, `execcmnd`), and the includer has to resume after
                // itself, not at the same line again.
                self.advance();
                crate::commands::invoke(self, &name, &args)?;
            }

            Op::If { cond, next, endif } => {
                if self.eval_int(&cond)? != 0 {
                    self.exec_mut().ifs.push(Taken { endif });
                    self.advance();
                } else {
                    self.jump(next);
                }
            }

            Op::ElseIf { cond, next, endif } => {
                // Reached by falling out of a branch that ran (then this
                // branch is not considered) or by a false condition before it.
                if self.taken_branch_is(endif) {
                    self.exec_mut().ifs.pop();
                    self.jump(endif);
                } else if self.eval_int(&cond)? != 0 {
                    self.exec_mut().ifs.push(Taken { endif });
                    self.advance();
                } else {
                    self.jump(next);
                }
            }

            Op::Else { endif } => {
                if self.taken_branch_is(endif) {
                    self.exec_mut().ifs.pop();
                    self.jump(endif);
                } else {
                    self.exec_mut().ifs.push(Taken { endif });
                    self.advance();
                }
            }

            Op::IfOnce { cond, next } => {
                if self.eval_int(&cond)? != 0 {
                    self.advance();
                } else {
                    self.jump(next);
                }
            }

            Op::While { cond, end } => {
                if self.eval_int(&cond)? != 0 {
                    self.advance();
                } else {
                    self.jump(end + 1);
                }
            }
            // Back to the opener itself: `EndWhile` has to *re-test* the
            // condition, and the body begins one op after it.
            Op::EndWhile { start } => self.jump(start),

            Op::Until { cond, end } => {
                // §6.3: `until` repeats while the condition is *zero*.
                if self.eval_int(&cond)? == 0 {
                    self.advance();
                } else {
                    self.jump(end + 1);
                }
            }
            Op::EndUntil { start } => self.jump(start),

            Op::Do { pre, end } => {
                let run = match &pre {
                    None => true,
                    Some(test) => {
                        let value = self.eval_int(&test.cond)?;
                        if test.until { value == 0 } else { value != 0 }
                    }
                };
                if run {
                    self.advance();
                } else {
                    self.jump(end + 1);
                }
            }

            Op::Loop { post, start } => {
                let again = match &post {
                    // `loop while`/`loop until` tests after the body (§6.3), so
                    // it goes back to the body's first statement.
                    Some(test) => {
                        let value = self.eval_int(&test.cond)?;
                        if test.until { value == 0 } else { value != 0 }
                    }
                    // A bare `loop` belongs to `do while`/`do until`, where the
                    // test is at the `do`: going back to the body instead would
                    // never test it again. `do` … `loop` with no test at all is
                    // an infinite loop by design, and this is what makes it one.
                    None => {
                        self.jump(start);
                        return Ok(());
                    }
                };
                if again {
                    self.jump(start + 1);
                } else {
                    self.advance();
                }
            }

            Op::For {
                var,
                first,
                last,
                end,
            } => {
                let first = self.eval_int(&first)?;
                let last = self.eval_int(&last)?;
                // §6.2: the step is +1 or -1, chosen by where `last` is.
                let step = if last >= first { 1 } else { -1 };
                self.assign_int(&var, first)?;
                self.exec_mut().loops.push(ForState {
                    var,
                    end,
                    last,
                    step,
                });
                self.advance();
            }

            Op::Next { start } => {
                let Some(state) = self.exec().loops.last().cloned() else {
                    return Err(Error::StackOverflow);
                };
                let next = self.eval_int(&Expr::Var(state.var.clone()))? + state.step;
                let done = if state.step > 0 {
                    next > state.last
                } else {
                    next < state.last
                };
                if done {
                    self.exec_mut().loops.pop();
                    self.jump(state.end + 1);
                } else {
                    self.assign_int(&state.var, next)?;
                    self.jump(start + 1);
                }
            }

            Op::Break { to } => {
                // Everything the loops being left kept is dropped with them.
                while let Some(state) = self.exec().loops.last() {
                    if state.end < to {
                        self.exec_mut().loops.pop();
                    } else {
                        break;
                    }
                }
                self.jump(to);
            }

            Op::Continue { to } => self.jump(to),

            Op::Goto { target, loops, .. } => {
                while self.exec().loops.len() > loops {
                    self.exec_mut().loops.pop();
                }
                // A jump makes the branch bookkeeping meaningless: whichever
                // `elseif` comes next cannot belong to a taken branch.
                self.exec_mut().ifs.clear();
                self.jump(target);
            }

            Op::Call { target, .. } => {
                let exec = self.exec();
                if exec.calls.len() >= MAX_CALL_DEPTH {
                    return Err(Error::StackOverflow);
                }
                let (return_to, loops, ifs) = (exec.pc + 1, exec.loops.len(), exec.ifs.len());
                self.exec_mut().calls.push((return_to, loops, ifs));
                self.jump(target);
            }

            Op::Return => {
                let Some((return_to, loops, ifs)) = self.exec_mut().calls.pop() else {
                    // §8 has no message for this; ending the file is what a
                    // macro that fell into its own subroutine wanted.
                    self.leave_file();
                    return Ok(());
                };
                let exec = self.exec_mut();
                exec.loops.truncate(loops);
                exec.ifs.truncate(ifs);
                exec.pc = return_to;
            }

            Op::End => {
                // §6.7: the macro ends, window and all.
                self.files.clear();
            }

            Op::Exit => {
                // §6.7: return to the includer, or end the macro at top level.
                self.leave_file();
            }

            Op::SetExitCode { value } => {
                self.exit_code = self.eval_int(&value)?;
                self.advance();
            }

            Op::Include { path } => {
                let name = self.eval_str(&path)?;
                if self.files.len() > MAX_INCLUDE_DEPTH {
                    return Err(Error::CannotOpenFile(name));
                }
                let bytes = self
                    .host
                    .read(&name)
                    .ok_or_else(|| Error::CannotOpenFile(name.clone()))?;
                let (text, _) = source::decode(&bytes)
                    .map_err(|error| Error::Syntax(format!("{name}: {error}")))?;
                let program = Program::parse(&text)
                    .map_err(|error| Error::Syntax(format!("{name}: {error}")))?;
                // The includer resumes after this line, which means advancing
                // before the new file takes over the run loop's top frame.
                self.advance();
                self.push_program(program);
            }
        }
        Ok(())
    }

    fn taken_branch_is(&self, endif: usize) -> bool {
        self.exec().ifs.last() == Some(&Taken { endif })
    }

    // -- values -----------------------------------------------------------------

    fn view(&self) -> View<'_> {
        View {
            vars: &self.vars,
            system: &self.system,
        }
    }

    pub(crate) fn eval(&self, expr: &Expr) -> Result<Value, Error> {
        expr::eval(expr, &self.view())
    }

    pub(crate) fn eval_int(&self, expr: &Expr) -> Result<i32, Error> {
        match self.eval(expr)? {
            Value::Int(value) => Ok(value),
            Value::Str(_) => Err(Error::TypeMismatch),
        }
    }

    pub(crate) fn eval_str(&self, expr: &Expr) -> Result<String, Error> {
        Ok(as_string(self.eval(expr)?))
    }

    /// Read a variable by name, going through the system names first.
    pub(crate) fn get(&self, name: &str) -> Result<Value, Error> {
        self.view().value(name, None)
    }

    /// Write a variable by name. The system variables are writable too: a
    /// macro may set `inputstr` or `result` itself.
    pub(crate) fn assign(&mut self, name: &str, value: Value) -> Result<(), Error> {
        let lower = name.to_ascii_lowercase();
        if let Some(group) = group_number(&lower) {
            self.system.groupmatchstr[group] = as_string(value);
            return Ok(());
        }
        match lower.as_str() {
            "result" => self.system.result = as_int(value)?,
            "timeout" => self.system.timeout = as_int(value)?,
            "mtimeout" => self.system.mtimeout = as_int(value)?,
            "inputstr" => self.system.inputstr = as_string(value),
            "matchstr" => self.system.matchstr = as_string(value),
            _ => self.vars.set(name, value)?,
        }
        Ok(())
    }

    pub(crate) fn assign_int(&mut self, name: &str, value: i32) -> Result<(), Error> {
        self.assign(name, Value::Int(value))
    }

    pub(crate) fn assign_str(&mut self, name: &str, value: &str) -> Result<(), Error> {
        self.assign(name, Value::Str(value.to_string()))
    }

    fn assign_index(&mut self, name: &str, index: i32, value: Value) -> Result<(), Error> {
        self.vars.set_index(name, index, value)
    }

    // -- variables and arrays, for the command table ------------------------

    pub(crate) fn has_label(&self, name: &str) -> bool {
        self.files
            .last()
            .is_some_and(|exec| exec.program.label(name).is_some())
    }

    pub(crate) fn get_index(&self, name: &str, index: i32) -> Result<Value, Error> {
        self.vars.get_index(name, index)
    }

    /// The declared size of an array, so `listbox` can walk one.
    pub(crate) fn array_len(&self, name: &str) -> Result<i32, Error> {
        match self.vars.defined(name) {
            5 | 6 => {}
            0 => return Err(Error::VariableNotInitialized(name.to_string())),
            _ => return Err(Error::TypeMismatch),
        }
        self.vars
            .array_len(name)
            .ok_or_else(|| Error::VariableNotInitialized(name.to_string()))
    }

    pub(crate) fn declare_int_array(&mut self, name: &str, size: i32) -> Result<(), Error> {
        self.vars.declare_int_array(name, size)
    }

    pub(crate) fn declare_str_array(&mut self, name: &str, size: i32) -> Result<(), Error> {
        self.vars.declare_str_array(name, size)
    }

    pub(crate) fn delvar(&mut self, name: &str) -> Result<(), Error> {
        self.vars.delvar(name)
    }

    pub(crate) fn redim(&mut self, name: &str, size: i32, preserve: bool) -> Result<(), Error> {
        self.vars.redim(name, size, preserve)
    }

    /// Start running another program: that is what `include` and `execcmnd`
    /// do, and neither returns a value.
    pub(crate) fn push_program(&mut self, program: Program) {
        self.files.push(Exec::new(program));
    }

    // -- parameter parsing --------------------------------------------------

    /// Parameter `index` as an expression. A command's parameters are
    /// whitespace-separated expressions (§3.2), so each parameter is parsed on
    /// its own and `A - 1` must be written `A-1`.
    pub(crate) fn param(&self, args: &[Vec<Tok>], index: usize) -> Result<Expr, Error> {
        match args.get(index) {
            Some(tokens) => expr::parse(tokens)
                .map_err(|error| Error::Syntax(format!("parameter {}: {error}", index + 1))),
            None => Err(Error::Syntax(format!("parameter {} is missing", index + 1))),
        }
    }

    /// Parameter `index` as a variable *name*: the string commands take the
    /// destination's name, not its value (§7.3).
    pub(crate) fn name_param(&self, args: &[Vec<Tok>], index: usize) -> Result<String, Error> {
        match args.get(index).map(|group| &group[..]) {
            Some([tok]) => match &tok.kind {
                crate::lex::Token::Ident(name) => Ok(name.clone()),
                _ => Err(Error::Syntax(format!(
                    "parameter {} must be a variable name",
                    index + 1
                ))),
            },
            _ => Err(Error::Syntax(format!(
                "parameter {} must be a variable name",
                index + 1
            ))),
        }
    }

    pub(crate) fn int_param(&self, args: &[Vec<Tok>], index: usize) -> Result<i32, Error> {
        self.eval_int(&self.param(args, index)?)
    }

    pub(crate) fn str_param(&self, args: &[Vec<Tok>], index: usize) -> Result<String, Error> {
        self.eval_str(&self.param(args, index)?)
    }

    /// The number of parameters given, checked against what a command wants.
    pub(crate) fn arity(
        &self,
        args: &[Vec<Tok>],
        name: &str,
        min: usize,
        max: usize,
    ) -> Result<(), Error> {
        if args.len() < min || args.len() > max {
            return Err(Error::Syntax(format!(
                "{name} takes {min}..{max} parameters, {} given",
                args.len()
            )));
        }
        Ok(())
    }

    pub(crate) fn host(&mut self) -> &mut dyn Host {
        self.host
    }

    pub(crate) fn system_mut(&mut self) -> &mut SystemVars {
        &mut self.system
    }

    pub(crate) fn set_result(&mut self, value: i32) {
        self.system.result = value;
    }

    /// Split `text` into pieces the way `strsplit` does, shared so that
    /// `strjoin` and `strsplit` cannot disagree about the slots.
    pub(crate) fn set_groups(&mut self, pieces: &[String]) -> i32 {
        for slot in &mut self.system.groupmatchstr {
            slot.clear();
        }
        let mut count = 0;
        for (slot, piece) in self.system.groupmatchstr.iter_mut().zip(pieces) {
            *slot = piece.clone();
            count += 1;
        }
        if pieces.len() > 9 {
            self.system.groupmatchstr[8] = pieces[9..].join("");
        }
        count
    }

    pub(crate) fn next_random(&mut self, max: i32) -> i32 {
        // A 32-bit LCG is enough for a macro that wants a jitter, and it has to
        // be ours: Tera Term's `random` is not documented as reproducible.
        self.seed = self
            .seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        let span = (max as u32).saturating_add(1);
        ((self.seed >> 8) % span) as i32
    }

    // -- §7.5's password store ----------------------------------------------

    pub(crate) fn set_password(&mut self, file: &str, name: &str, password: &str) {
        self.passwords
            .insert((file.to_string(), name.to_string()), password.to_string());
    }

    pub(crate) fn password(&self, file: &str, name: &str) -> Option<String> {
        self.passwords
            .get(&(file.to_string(), name.to_string()))
            .cloned()
    }

    pub(crate) fn forget_password(&mut self, file: &str, name: &str) -> bool {
        if name.is_empty() {
            // §7.5: an empty name removes every password in the file.
            let before = self.passwords.len();
            self.passwords.retain(|(path, _), _| path != file);
            return self.passwords.len() != before;
        }
        self.passwords
            .remove(&(file.to_string(), name.to_string()))
            .is_some()
    }
}

/// Integers become their decimal spelling where a string is wanted; §4.1 has
/// no other conversion, and `str2int` is the other direction.
pub(crate) fn as_string(value: Value) -> String {
    match value {
        Value::Int(number) => number.to_string(),
        Value::Str(text) => text,
    }
}

pub(crate) fn as_int(value: Value) -> Result<i32, Error> {
    match value {
        Value::Int(number) => Ok(number),
        Value::Str(_) => Err(Error::TypeMismatch),
    }
}

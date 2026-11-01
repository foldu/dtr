//! The commands of §7, in the families the manual groups them by.
//!
//! A command is a name and a list of parameters that are still tokens: what a
//! parameter *means* is the command's business, which is why `messagebox 'a'
//! 'b'` and `strconcat tmp ix` share a dispatcher and nothing else. Three rules
//! show up again and again here and are worth stating once:
//!
//! * **A destination is a name.** `strconcat <strvar> …` writes into the
//!   variable whose *name* it was given, so those parameters go through
//!   [`Interpreter::name_param`] and never through the expression parser.
//! * **`result` is the error channel** (§8). A command that can fail writes a
//!   number into it and returns; it does not stop the macro.
//! * **`result` codes differ per family.** File commands use 0 for success,
//!   connection commands non-zero, `strcompare` returns the comparison itself
//!   and `listbox` a 0-based index. Each entry says which, and the manual's
//!   table is the source.
//!
//! This module is deliberately incomplete: it implements the families whose
//! behaviour needs no terminal — dialogs, strings, variables, `pause` — and
//! reports everything else as `Unknown command.`, which is what §8 says an
//! interpreter that does not know a command does. The communication and file
//! families come next, and they change only this file.

use crate::error::Error;
use crate::interp::{as_string, Interpreter};
use crate::lex::Tok;
use crate::program::Program;
use crate::value::Value;

/// Run one command. The pc is already past it, so a command that pushes a file
/// frame (`include`, `execcmnd`) returns with the included file running.
pub(crate) fn invoke(
    interp: &mut Interpreter<'_>,
    name: &str,
    args: &[Vec<Tok>],
) -> Result<(), Error> {
    match name {
        // §7.6 dialogs and the macro window.
        "messagebox" => messagebox(interp, args),
        "yesnobox" => yesnobox(interp, args),
        "inputbox" => inputbox(interp, args),
        "passwordbox" => passwordbox(interp, args),
        "filenamebox" => filenamebox(interp, args),
        "dirnamebox" => dirnamebox(interp, args),
        "listbox" => listbox(interp, args),
        "statusbox" => statusbox(interp, args),
        "closesbox" => closesbox(interp, args),
        "bringupbox" => {
            interp.arity(args, name, 0, 0)?;
            Ok(())
        }
        "setdlgpos" => setdlgpos(interp, args),
        "dispstr" => dispstr(interp, args),
        "beep" => beep(interp, args),
        "show" => show(interp, args),

        // §7.2 waiting.
        "pause" => pause(interp, args),
        "mpause" => mpause(interp, args),
        "execcmnd" => execcmnd(interp, args),

        // §7.3 strings.
        "sprintf" => sprintf_command(interp, args),
        "sprintf2" => sprintf2(interp, args),
        "int2str" => int2str(interp, args),
        "str2int" => str2int(interp, args),
        "strconcat" => strconcat(interp, args),
        "strcopy" => strcopy(interp, args),
        "str2code" => str2code(interp, args),
        "code2str" => code2str(interp, args),
        "strlen" => strlen(interp, args),
        "strcompare" => strcompare(interp, args),
        "strscan" => strscan(interp, args),
        "strsplit" => strsplit(interp, args),
        "strjoin" => strjoin(interp, args),
        "strinsert" => strinsert(interp, args),
        "strremove" => strremove(interp, args),
        "strreplace" => strreplace(interp, args),
        "strtrim" => strtrim(interp, args),
        "strspecial" => strspecial(interp, args),
        "tolower" => tolower(interp, args),
        "toupper" => toupper(interp, args),

        // §7.6 variables and arrays.
        "intdim" => intdim(interp, args),
        "strdim" => strdim(interp, args),
        "delvar" => delvar(interp, args),
        "redim" => redim(interp, args),
        "ifdefined" => ifdefined(interp, args),

        // §7.6 bit twiddling that needs nothing outside.
        "rotateleft" => rotate(interp, args, true),
        "rotateright" => rotate(interp, args, false),
        "random" => random(interp, args),
        "checksum8" => checksum(interp, args, 1),
        "checksum16" => checksum(interp, args, 2),
        "checksum32" => checksum(interp, args, 4),

        _ => Err(Error::UnknownCommand(name.to_string())),
    }
}

// -- dialogs -----------------------------------------------------------------

fn messagebox(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "messagebox", 2, 3)?;
    let message = interp.str_param(args, 0)?;
    let title = interp.str_param(args, 1)?;
    interp.host().messagebox(&message, &title);
    Ok(())
}

fn yesnobox(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "yesnobox", 2, 3)?;
    let message = interp.str_param(args, 0)?;
    let title = interp.str_param(args, 1)?;
    let yes = interp.host().yesnobox(&message, &title);
    interp.set_result(if yes { 1 } else { 0 });
    Ok(())
}

fn inputbox(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "inputbox", 2, 4)?;
    let message = interp.str_param(args, 0)?;
    let title = interp.str_param(args, 1)?;
    let default = if args.len() > 2 {
        interp.str_param(args, 2)?
    } else {
        String::new()
    };
    match interp.host().inputbox(&message, &title, &default) {
        Some(answer) => interp.system_mut().inputstr = answer,
        None => interp.system_mut().inputstr.clear(),
    }
    Ok(())
}

fn passwordbox(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "passwordbox", 2, 3)?;
    let message = interp.str_param(args, 0)?;
    let title = interp.str_param(args, 1)?;
    match interp.host().passwordbox(&message, &title) {
        Some(answer) => interp.system_mut().inputstr = answer,
        None => interp.system_mut().inputstr.clear(),
    }
    Ok(())
}

fn filenamebox(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "filenamebox", 1, 3)?;
    let title = interp.str_param(args, 0)?;
    // `<type>` 0 = Open, anything else = Save (4.65+).
    let save = if args.len() > 1 {
        interp.int_param(args, 1)? != 0
    } else {
        false
    };
    let dir = if args.len() > 2 {
        Some(interp.str_param(args, 2)?)
    } else {
        None
    };
    match interp.host().filenamebox(&title, save, dir.as_deref()) {
        Some(path) => {
            interp.system_mut().inputstr = path;
            interp.set_result(1);
        }
        None => {
            interp.system_mut().inputstr.clear();
            interp.set_result(0);
        }
    }
    Ok(())
}

fn dirnamebox(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "dirnamebox", 1, 2)?;
    let title = interp.str_param(args, 0)?;
    let dir = if args.len() > 1 {
        Some(interp.str_param(args, 1)?)
    } else {
        None
    };
    match interp.host().dirnamebox(&title, dir.as_deref()) {
        Some(path) => {
            interp.system_mut().inputstr = path;
            interp.set_result(1);
        }
        None => {
            interp.system_mut().inputstr.clear();
            interp.set_result(0);
        }
    }
    Ok(())
}

fn listbox(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "listbox", 3, 8)?;
    let message = interp.str_param(args, 0)?;
    let title = interp.str_param(args, 1)?;
    // The items come from an array variable, by name.
    let array = interp.name_param(args, 2)?;
    let size = interp.array_len(&array)?;
    let mut items = Vec::with_capacity(size as usize);
    for index in 0..size {
        items.push(as_string(interp.get_index(&array, index)?));
    }
    let selected = if args.len() > 3 {
        Some(interp.int_param(args, 3)?)
    } else {
        None
    };
    match interp.host().listbox(&message, &title, &items, selected) {
        Some(index) => interp.set_result(index),
        None => interp.set_result(-1),
    }
    Ok(())
}

fn statusbox(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "statusbox", 2, 3)?;
    let message = interp.str_param(args, 0)?;
    let title = interp.str_param(args, 1)?;
    interp.host().statusbox(&message, &title);
    Ok(())
}

fn closesbox(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "closesbox", 0, 0)?;
    interp.host().closesbox();
    Ok(())
}

fn setdlgpos(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "setdlgpos", 0, 3)?;
    let x = if args.is_empty() {
        None
    } else {
        Some(interp.int_param(args, 0)?)
    };
    let y = if args.len() > 1 {
        Some(interp.int_param(args, 1)?)
    } else {
        None
    };
    interp.host().setdlgpos(x, y);
    Ok(())
}

fn dispstr(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "dispstr", 0, usize::MAX)?;
    // `dispstr a b c` writes them one after another, which is how a macro
    // builds a line out of pieces without `strconcat`.
    let mut text = String::new();
    for index in 0..args.len() {
        text.push_str(&interp.str_param(args, index)?);
    }
    interp.host().dispstr(&text);
    Ok(())
}

fn beep(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "beep", 0, 1)?;
    let kind = if args.is_empty() {
        5
    } else {
        interp.int_param(args, 0)?
    };
    interp.host().beep(kind);
    Ok(())
}

fn show(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "show", 1, 1)?;
    let flag = interp.int_param(args, 0)?;
    interp.host().show_macro_window(flag);
    Ok(())
}

// -- waiting ----------------------------------------------------------------

fn pause(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "pause", 1, 1)?;
    let seconds = interp.int_param(args, 0)?;
    if seconds < 0 {
        return Err(Error::Syntax("pause wants a time in seconds".into()));
    }
    interp.host().pause(seconds as u64 * 1000);
    Ok(())
}

fn mpause(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "mpause", 1, 1)?;
    let millis = interp.int_param(args, 0)?;
    if millis < 0 {
        return Err(Error::Syntax("mpause wants a time in milliseconds".into()));
    }
    interp.host().pause(millis as u64);
    Ok(())
}

fn execcmnd(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "execcmnd", 1, 1)?;
    let text = interp.str_param(args, 0)?;
    let program = Program::parse(&text)
        .map_err(|error| Error::Syntax(format!("execcmnd: {error}")))?;
    interp.push_program(program);
    Ok(())
}

// -- strings ----------------------------------------------------------------

fn sprintf_command(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "sprintf", 1, usize::MAX)?;
    let format = interp.str_param(args, 0)?;
    let values = arguments(interp, args, 1)?;
    let (text, code) = format_string(&format, &values);
    interp.system_mut().inputstr = text;
    interp.set_result(code);
    Ok(())
}

fn sprintf2(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "sprintf2", 2, usize::MAX)?;
    let target = interp.name_param(args, 0)?;
    let format = interp.str_param(args, 1)?;
    let values = arguments(interp, args, 2)?;
    let (text, code) = format_string(&format, &values);
    if code == 0 {
        if text.chars().count() > crate::vars::MAX_STRING_CHARS {
            interp.set_result(4);
            return Ok(());
        }
        interp.assign_str(&target, &text)?;
    }
    interp.set_result(code);
    Ok(())
}

fn arguments(
    interp: &Interpreter<'_>,
    args: &[Vec<Tok>],
    from: usize,
) -> Result<Vec<Value>, Error> {
    let mut values = Vec::new();
    for index in from..args.len() {
        let expr = interp.param(args, index)?;
        values.push(interp.eval(&expr)?);
    }
    Ok(values)
}

/// `sprintf`'s format with its result code: 0 ok, 1 no format, 2 invalid
/// format, 3 invalid argument (§7.3).
fn format_string(format: &str, values: &[Value]) -> (String, i32) {
    let chars: Vec<char> = format.chars().collect();
    let mut out = String::new();
    let mut at = 0usize;
    let mut next = 0usize;

    while at < chars.len() {
        if chars[at] != '%' {
            out.push(chars[at]);
            at += 1;
            continue;
        }
        at += 1;
        if chars.get(at) == Some(&'%') {
            out.push('%');
            at += 1;
            continue;
        }
        let Some(mut spec) = parse_spec(&chars, &mut at) else {
            return (out, 2);
        };
        // `*` takes its width or precision from the argument list, as in C.
        if spec.width_star {
            let Some(value) = values.get(next) else {
                return (out, 3);
            };
            next += 1;
            let Some(width) = integer(value) else {
                return (out, 3);
            };
            if width < 0 {
                spec.left = true;
                spec.width = Some(width.unsigned_abs() as usize);
            } else {
                spec.width = Some(width as usize);
            }
        }
        if spec.precision_star {
            let Some(value) = values.get(next) else {
                return (out, 3);
            };
            next += 1;
            let Some(precision) = integer(value) else {
                return (out, 3);
            };
            // A negative precision is "as if it were omitted".
            spec.precision = (precision >= 0).then_some(precision as usize);
        }
        let Some(value) = values.get(next) else {
            return (out, 3);
        };
        next += 1;
        match render(&spec, value) {
            Some(text) => out.push_str(&text),
            None => return (out, 3),
        }
    }
    (out, 0)
}

/// One `%` specification, parsed but not yet applied.
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    zero: bool,
    width: Option<usize>,
    width_star: bool,
    precision: Option<usize>,
    precision_star: bool,
    conversion: char,
}

fn parse_spec(chars: &[char], at: &mut usize) -> Option<Spec> {
    let mut spec = Spec {
        left: false,
        plus: false,
        space: false,
        zero: false,
        width: None,
        width_star: false,
        precision: None,
        precision_star: false,
        conversion: ' ',
    };

    // Flags, in any order. `0` is a flag only here: after a digit it is part of
    // the width, which is why the two phases cannot be merged.
    loop {
        match chars.get(*at) {
            Some('-') => spec.left = true,
            Some('+') => spec.plus = true,
            Some(' ') => spec.space = true,
            Some('0') => spec.zero = true,
            Some('#') => {}
            _ => break,
        }
        *at += 1;
    }

    if chars.get(*at) == Some(&'*') {
        spec.width_star = true;
        *at += 1;
    } else {
        let start = *at;
        while matches!(chars.get(*at), Some('0'..='9')) {
            *at += 1;
        }
        if *at > start {
            spec.width = chars[start..*at].iter().collect::<String>().parse().ok();
        }
    }

    if chars.get(*at) == Some(&'.') {
        *at += 1;
        if chars.get(*at) == Some(&'*') {
            spec.precision_star = true;
            *at += 1;
        } else {
            let start = *at;
            while matches!(chars.get(*at), Some('0'..='9')) {
                *at += 1;
            }
            // `%.d` is precision 0, not "no precision".
            spec.precision = Some(
                chars[start..*at]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0),
            );
        }
    }

    spec.conversion = *chars.get(*at)?;
    *at += 1;
    Some(spec)
}

fn render(spec: &Spec, value: &Value) -> Option<String> {
    let mut text = match spec.conversion {
        'd' | 'i' => {
            let number = integer(value)? as i64;
            let digits = number.unsigned_abs().to_string();
            let mut digits = digits;
            if spec.precision == Some(0) && number == 0 {
                digits.clear();
            }
            if let Some(precision) = spec.precision {
                while digits.len() < precision {
                    digits.insert(0, '0');
                }
            }
            let sign = if number < 0 {
                "-"
            } else if spec.plus {
                "+"
            } else if spec.space {
                " "
            } else {
                ""
            };
            format!("{sign}{digits}")
        }
        'u' => integer(value)?.unsigned_abs().to_string(),
        'o' => format!("{:o}", integer(value)?),
        'x' => format!("{:x}", integer(value)?),
        'X' => format!("{:X}", integer(value)?),
        'c' => {
            let code = integer(value)?;
            char::from_u32(u32::try_from(code).ok()?)?.to_string()
        }
        's' => {
            let text = as_string(value.clone());
            match spec.precision {
                Some(limit) => text.chars().take(limit).collect(),
                None => text,
            }
        }
        'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'a' | 'A' => float(spec, value)?,
        _ => return None,
    };
    // Width and the `0` flag. A number's zero padding goes after its sign.
    if let Some(width) = spec.width {
        let length = text.chars().count();
        if length < width {
            let pad = width - length;
            if spec.left {
                text.push_str(&" ".repeat(pad));
            } else if spec.zero && !matches!(spec.conversion, 's' | 'c') {
                let (sign, rest) = split_sign(&text);
                text = format!("{sign}{}{rest}", "0".repeat(pad));
            } else {
                text = format!("{}{text}", " ".repeat(pad));
            }
        }
    }
    Some(text)
}

fn split_sign(text: &str) -> (&str, &str) {
    match text.as_bytes().first() {
        Some(b'-' | b'+' | b' ') => text.split_at(1),
        _ => ("", text),
    }
}

fn integer(value: &Value) -> Option<i64> {
    match value {
        Value::Int(number) => Some(i64::from(*number)),
        // §7.3: a float reaches `sprintf` as a *string*, so a number in a
        // string is what an integer conversion has to work with.
        Value::Str(text) => text.trim().parse().ok(),
    }
}

fn float(spec: &Spec, value: &Value) -> Option<String> {
    let number = match value {
        Value::Int(number) => f64::from(*number),
        Value::Str(text) => text.trim().parse().ok()?,
    };
    let precision = spec.precision.unwrap_or(6);
    let upper = spec.conversion.is_ascii_uppercase();
    let text = match spec.conversion {
        'f' | 'F' => format!("{number:.precision$}"),
        'e' | 'E' => exponential(number, precision, upper),
        'g' | 'G' => general(number, precision.max(1), upper),
        // `%a` is C's hexadecimal float; Rust has no equivalent, and no macro in
        // the corpus asks for it.
        _ => return None,
    };
    Some(if spec.plus && number >= 0.0 {
        format!("+{text}")
    } else {
        text
    })
}

fn exponential(number: f64, precision: usize, upper: bool) -> String {
    // Rust writes `3.141593e0`; C writes `3.141593e+00`, which is the spelling a
    // macro parsing its own log expects.
    let text = format!("{number:.precision$e}");
    let Some((mantissa, exponent)) = text.split_once('e') else {
        return text;
    };
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if exponent < 0 { '-' } else { '+' };
    let digits = exponent.unsigned_abs();
    let e = if upper { 'E' } else { 'e' };
    format!("{mantissa}{e}{sign}{digits:02}")
}

fn general(number: f64, precision: usize, upper: bool) -> String {
    // C's `%g`: `%e` when the exponent is below -4 or at least the precision,
    // otherwise `%f`; then trailing zeros go away.
    let probe = format!("{number:e}");
    let Some((_, exponent)) = probe.split_once('e') else {
        return probe;
    };
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let mut out = if exponent < -4 || exponent >= precision as i32 {
        exponential(number, precision.saturating_sub(1), upper)
    } else {
        let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
        format!("{number:.decimals$}")
    };
    if out.contains('.') {
        while out.ends_with('0') {
            out.pop();
        }
        if out.ends_with('.') {
            out.pop();
        }
    }
    out
}

fn int2str(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "int2str", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let value = interp.int_param(args, 1)?;
    interp.assign_str(&target, &value.to_string())
}

fn str2int(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "str2int", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let text = interp.str_param(args, 1)?;
    match parse_integer(&text) {
        Some(value) => {
            interp.assign_int(&target, value)?;
            interp.set_result(1);
        }
        None => interp.set_result(0),
    }
    Ok(())
}

/// §7.3: decimal or hexadecimal, and `$`/`0x` both name hex.
fn parse_integer(text: &str) -> Option<i32> {
    let text = text.trim();
    let (radix, digits) = if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        (16, rest)
    } else if let Some(rest) = text.strip_prefix('$') {
        (16, rest)
    } else {
        (10, text)
    };
    let negative = digits.starts_with('-');
    let digits = digits.trim_start_matches(['-', '+']);
    let value = i64::from_str_radix(digits, radix).ok()?;
    let value = if negative { -value } else { value };
    i32::try_from(value).ok()
}

fn strconcat(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strconcat", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let text = interp.str_param(args, 1)?;
    let mut current = match interp.get(&target) {
        // An undefined destination starts empty rather than failing: this is
        // how a macro builds a string from nothing.
        Err(Error::VariableNotInitialized(_)) => String::new(),
        other => as_string(other?),
    };
    if current.chars().count() + text.chars().count() > crate::vars::MAX_STRING_CHARS {
        return Err(Error::TooManyVariables("string"));
    }
    current.push_str(&text);
    interp.assign_str(&target, &current)
}

fn strcopy(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strcopy", 4, 4)?;
    let target = interp.name_param(args, 0)?;
    let position = interp.int_param(args, 1)?;
    let length = interp.int_param(args, 2)?;
    let text = interp.str_param(args, 3)?;
    let chars: Vec<char> = text.chars().collect();
    // 1-origin, as every position in TTL is.
    let start = usize::try_from(position.saturating_sub(1)).unwrap_or(usize::MAX);
    let take = usize::try_from(length.max(0)).unwrap_or(0);
    let slice: String = if start >= chars.len() {
        String::new()
    } else {
        chars[start..(start + take).min(chars.len())].iter().collect()
    };
    interp.assign_str(&target, &slice)
}

fn str2code(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "str2code", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let text = interp.str_param(args, 1)?;
    // §7.3: up to four bytes, packed; longer input keeps the last four.
    let bytes = text.as_bytes();
    let start = bytes.len().saturating_sub(4);
    let mut code: u32 = 0;
    for &byte in &bytes[start..] {
        code = (code << 8) | u32::from(byte);
    }
    interp.assign_int(&target, code as i32)
}

fn code2str(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "code2str", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let code = interp.int_param(args, 1)?;
    let bytes = code.to_be_bytes();
    let text: Vec<u8> = bytes.into_iter().skip_while(|&b| b == 0).collect();
    interp.assign_str(&target, &String::from_utf8_lossy(&text))
}

fn strlen(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strlen", 1, 1)?;
    let text = interp.str_param(args, 0)?;
    // §7.3: bytes, not characters.
    interp.set_result(text.len() as i32);
    Ok(())
}

fn strcompare(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strcompare", 2, 2)?;
    let left = interp.str_param(args, 0)?;
    let right = interp.str_param(args, 1)?;
    let result = match left.cmp(&right) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    };
    interp.set_result(result);
    Ok(())
}

fn strscan(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strscan", 2, 2)?;
    let haystack = interp.str_param(args, 0)?;
    let needle = interp.str_param(args, 1)?;
    let position = match haystack.find(&needle) {
        // 1-origin, and 0 means "not there".
        Some(byte) => haystack[..byte].chars().count() as i32 + 1,
        None => 0,
    };
    interp.set_result(position);
    Ok(())
}

fn strsplit(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strsplit", 2, 3)?;
    let text = interp.str_param(args, 0)?;
    let separator = interp.str_param(args, 1)?;
    let count = if args.len() > 2 {
        Some(interp.int_param(args, 2)?)
    } else {
        None
    };
    let separator = separator.chars().next();
    let mut pieces: Vec<String> = match separator {
        Some(separator) => text.split(separator).map(str::to_string).collect(),
        None => vec![text],
    };
    // §7.3: with more pieces than the count, the last slot takes the rest.
    if let Some(count) = count.filter(|count| *count > 0) {
        let count = count as usize;
        if pieces.len() > count {
            let tail = pieces.drain(count - 1..).collect::<Vec<_>>().join("");
            pieces.push(tail);
        }
    }
    let more_than_nine = pieces.len() > 9;
    let count = interp.set_groups(&pieces);
    interp.set_result(if more_than_nine { 10 } else { count });
    Ok(())
}

fn strjoin(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strjoin", 2, 3)?;
    let target = interp.name_param(args, 0)?;
    let separator = interp.str_param(args, 1)?;
    let count = if args.len() > 2 {
        interp.int_param(args, 2)?.clamp(1, 9) as usize
    } else {
        9
    };
    let system = interp.system();
    let joined = system.groupmatchstr[..count].join(&separator);
    interp.assign_str(&target, &joined)
}

fn strinsert(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strinsert", 3, 3)?;
    let target = interp.name_param(args, 0)?;
    let position = interp.int_param(args, 1)?;
    let text = interp.str_param(args, 2)?;
    let mut current: Vec<char> = as_string(interp.get(&target)?).chars().collect();
    let at = usize::try_from(position.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(current.len());
    current.splice(at..at, text.chars());
    interp.assign_str(&target, &current.iter().collect::<String>())
}

fn strremove(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strremove", 3, 3)?;
    let target = interp.name_param(args, 0)?;
    let position = interp.int_param(args, 1)?;
    let length = interp.int_param(args, 2)?;
    let mut current: Vec<char> = as_string(interp.get(&target)?).chars().collect();
    let at = usize::try_from(position.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(current.len());
    let end = (at + usize::try_from(length.max(0)).unwrap_or(0)).min(current.len());
    current.drain(at..end);
    interp.assign_str(&target, &current.iter().collect::<String>())
}

fn strreplace(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strreplace", 4, 4)?;
    let target = interp.name_param(args, 0)?;
    let from = interp.int_param(args, 1)?;
    let pattern = interp.str_param(args, 2)?;
    let replacement = interp.str_param(args, 3)?;
    // §7.3: `strreplace` clears the nine slots rather than filling them.
    interp.set_groups(&[]);
    let text = as_string(interp.get(&target)?);
    let chars: Vec<char> = text.chars().collect();
    let start = usize::try_from(from.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(chars.len());
    match match_at(&chars, start, &pattern) {
        Some(end) => {
            let replaced: String = chars[..start]
                .iter()
                .chain(replacement.chars().collect::<Vec<_>>().iter())
                .chain(chars[end..].iter())
                .collect();
            interp.set_result(1);
            interp.system_mut().matchstr = pattern;
            interp.assign_str(&target, &replaced)?;
        }
        None => interp.set_result(0),
    }
    Ok(())
}

/// `strreplace` starts at a 1-origin character position, not a byte offset.
fn match_at(haystack: &[char], start: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return Some(start);
    }
    let end = start.checked_add(needle.len())?;
    (end <= haystack.len() && haystack[start..end] == needle[..]).then_some(end)
}

fn strtrim(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strtrim", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let trim: Vec<char> = interp.str_param(args, 1)?.chars().collect();
    let text = as_string(interp.get(&target)?);
    let trimmed = text.trim_matches(|ch| trim.contains(&ch)).to_string();
    interp.assign_str(&target, &trimmed)
}

fn strspecial(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strspecial", 1, 2)?;
    let target = interp.name_param(args, 0)?;
    let source = if args.len() > 1 {
        interp.str_param(args, 1)?
    } else {
        as_string(interp.get(&target)?)
    };
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    interp.assign_str(&target, &out)
}

fn tolower(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "tolower", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let text = interp.str_param(args, 1)?;
    interp.assign_str(&target, &text.to_lowercase())
}

fn toupper(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "toupper", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let text = interp.str_param(args, 1)?;
    interp.assign_str(&target, &text.to_uppercase())
}

// -- variables --------------------------------------------------------------

fn intdim(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "intdim", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let size = interp.int_param(args, 1)?;
    interp.declare_int_array(&target, size)
}

fn strdim(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "strdim", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let size = interp.int_param(args, 1)?;
    interp.declare_str_array(&target, size)
}

fn delvar(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    for index in 0..args.len() {
        let name = interp.name_param(args, index)?;
        interp.delvar(&name)?;
    }
    Ok(())
}

fn redim(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "redim", 2, 3)?;
    let target = interp.name_param(args, 0)?;
    let size = interp.int_param(args, 1)?;
    let preserve = if args.len() > 2 {
        let keyword = interp.str_param(args, 2)?;
        if !keyword.eq_ignore_ascii_case("Preserve") {
            return Err(Error::Syntax("redim's third parameter is 'Preserve'".into()));
        }
        true
    } else {
        false
    };
    interp.redim(&target, size, preserve)
}

fn ifdefined(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "ifdefined", 1, 1)?;
    let name = interp.name_param(args, 0)?;
    // §7.6: 0 undefined, 1 integer, 3 string, 4 label, 5 integer array,
    // 6 string array.
    let result = if interp.vars().defined(&name) == 0 && interp.has_label(&name) {
        4
    } else {
        interp.vars().defined(&name)
    };
    interp.set_result(result);
    Ok(())
}

// -- bit twiddling ----------------------------------------------------------

fn rotate(interp: &mut Interpreter<'_>, args: &[Vec<Tok>], left: bool) -> Result<(), Error> {
    interp.arity(args, "rotate", 3, 3)?;
    let target = interp.name_param(args, 0)?;
    let value = interp.int_param(args, 1)? as u32;
    let count = interp.int_param(args, 2)?.rem_euclid(32) as u32;
    let result = if left {
        value.rotate_left(count)
    } else {
        value.rotate_right(count)
    };
    interp.assign_int(&target, result as i32)
}

fn random(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "random", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let max = interp.int_param(args, 1)?;
    if max <= 0 {
        return Err(Error::Syntax("random wants a maximum above 0".into()));
    }
    let value = interp.next_random(max);
    interp.assign_int(&target, value)
}

fn checksum(interp: &mut Interpreter<'_>, args: &[Vec<Tok>], width: usize) -> Result<(), Error> {
    interp.arity(args, "checksum", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let text = interp.str_param(args, 1)?;
    let mut sum: u32 = 0;
    for &byte in text.as_bytes() {
        sum = sum.wrapping_add(u32::from(byte));
    }
    let value = match width {
        1 => sum & 0xff,
        2 => sum & 0xffff,
        _ => sum,
    };
    interp.assign_int(&target, value as i32)
}

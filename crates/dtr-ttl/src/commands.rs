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
use crate::interp::{Interpreter, as_string};
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

        // §7.1 the connection and the byte stream.
        "connect" | "cygconnect" => connect(interp, args),
        "testlink" => testlink(interp, args),
        "disconnect" => {
            interp.arity(args, "disconnect", 0, 1)?;
            interp.host().disconnect();
            Ok(())
        }
        "unlink" => {
            interp.arity(args, "unlink", 0, 0)?;
            interp.host().unlink();
            Ok(())
        }
        "closett" => {
            interp.arity(args, "closett", 0, 0)?;
            interp.host().close_terminal();
            Ok(())
        }
        "send" | "sendtext" | "sendbinary" => send(interp, args, false),
        "sendln" => send(interp, args, true),
        "sendbroadcast" => send(interp, args, false),
        "sendlnbroadcast" => send(interp, args, true),
        "sendmulticast" => send_multicast(interp, args, false),
        "sendlnmulticast" => send_multicast(interp, args, true),
        "sendfile" => sendfile(interp, args),
        "sendbreak" => {
            interp.arity(args, "sendbreak", 0, 0)?;
            interp.host().note_setting("break", "sent");
            Ok(())
        }
        "setecho" | "enablekeyb" | "setsync" | "setmulticastname" | "setserialdelaychar"
        | "setserialdelayline" | "setserialdelay" | "changedir" | "logopen" | "logclose"
        | "logwrite" | "logwritebinary" | "logrotate" | "loginfo" | "getfileattr"
        | "setfileattr" | "sendkcode" | "folderdelete" | "setflowctrl" | "setcomport"
        | "setbaud" => setting(interp, name, args),
        "recvln" => recvln(interp, args),
        "wait" => wait(interp, args, Wait::Any),
        "waitln" => wait(interp, args, Wait::Line),
        "waitregex" => wait(interp, args, Wait::Regex),
        "waitn" => waitn(interp, args),
        "waitrecv" => waitrecv(interp, args),
        "flushrecv" => {
            interp.arity(args, "flushrecv", 0, 0)?;
            interp.host().flush_recv();
            Ok(())
        }
        // File transfer needs a link layer this host does not have. The
        // commands are accepted and report success so that a corpus macro's
        // control flow is the one it wrote; the corpus README says so.
        "xmodemsend" | "xmodemrecv" | "ymodemsend" | "ymodemrecv" | "zmodemsend" | "zmodemrecv"
        | "kermitsend" | "kermitrecv" | "quickvansend" | "quickvanrecv" => {
            interp.set_result(0);
            Ok(())
        }
        "callmenu" => {
            interp.arity(args, "callmenu", 1, 1)?;
            interp.set_result(0);
            Ok(())
        }

        // §7.4 files.
        "fileopen" => fileopen(interp, args),
        "filecreate" => filecreate(interp, args),
        "fileclose" => fileclose(interp, args),
        "filereadln" => filereadln(interp, args),
        "fileread" => fileread(interp, args),
        "filewrite" => filewrite(interp, args, false),
        "filewriteln" => filewrite(interp, args, true),
        "fileseek" => fileseek(interp, args),
        "filemarkptr" => filemarkptr(interp, args),
        "fileseekback" => fileseekback(interp, args),
        "filetruncate" => filetruncate(interp, args),
        "filedelete" => filedelete(interp, args),
        "filesearch" => filesearch(interp, args),
        "filecopy" => filecopy(interp, args, Copy::Copy),
        "filerename" => filecopy(interp, args, Copy::Rename),
        "fileconcat" => filecopy(interp, args, Copy::Concat),
        "findfirst" => findfirst(interp, args),
        "findnext" => findnext(interp, args),
        "findclose" => findclose(interp, args),
        "foldersearch" => foldersearch(interp, args),
        "filestat" => filestat(interp, args),
        "makepath" => makepath(interp, args),
        "basename" => basename(interp, args),
        "dirname" => dirname(interp, args),
        "foldercreate" => foldercreate(interp, args),

        // §7.6 time and environment.
        "gettime" => gettime(interp, args, false),
        "getdate" => gettime(interp, args, true),
        "uptime" => {
            interp.arity(args, "uptime", 1, 1)?;
            let target = interp.name_param(args, 0)?;
            // The manual is explicit: it wraps at 49.7 days, which is exactly
            // what an i32 of milliseconds does.
            let millis = interp.host().uptime_millis() as i32;
            interp.assign_int(&target, millis)
        }
        "getenv" => getenv(interp, args),
        "setenv" => setenv(interp, args),
        "setpassword" | "setpassword2" => setpassword(interp, args),
        "getpassword" | "getpassword2" => getpassword(interp, args),
        "ispassword" | "ispassword2" => ispassword(interp, args),
        "delpassword" | "delpassword2" => delpassword(interp, args),
        "getdir" => {
            interp.arity(args, "getdir", 1, 1)?;
            let target = interp.name_param(args, 0)?;
            let dir = interp.host().get_dir();
            interp.assign_str(&target, &dir)
        }
        "getttdir" => {
            // §7.6: the macro interpreter's own directory, which is where it
            // was started from unless `setdir` moved it.
            interp.arity(args, "getttdir", 1, 1)?;
            let target = interp.name_param(args, 0)?;
            let dir = interp.host().get_dir();
            interp.assign_str(&target, &dir)
        }
        "setdir" => {
            interp.arity(args, "setdir", 1, 1)?;
            let dir = interp.str_param(args, 0)?;
            interp.host().set_dir(&dir);
            Ok(())
        }
        "getver" => getver(interp, args),
        "getspecialfolder" => getspecialfolder(interp, args),
        "expandenv" => expandenv(interp, args),
        "exec" => exec(interp, args),

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
    let program =
        Program::parse(&text).map_err(|error| Error::Syntax(format!("execcmnd: {error}")))?;
    interp.push_program(program);
    Ok(())
}

// -- strings ----------------------------------------------------------------

fn sprintf_command(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    // §7.3: an empty `sprintf` is not a syntax error, it is result 1 — "no
    // format" — which is how a macro tests whether it built one.
    if args.is_empty() {
        interp.set_result(1);
        return Ok(());
    }
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
    alternate: bool,
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
        alternate: false,
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
            Some('#') => spec.alternate = true,
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
        'o' => {
            let text = format!("{:o}", integer(value)?);
            // `#` asks for the prefix C uses to make the radix obvious.
            if spec.alternate && !text.starts_with('0') {
                format!("0{text}")
            } else {
                text
            }
        }
        'x' | 'X' => {
            let number = integer(value)?;
            let text = if spec.conversion == 'x' {
                format!("{number:x}")
            } else {
                format!("{number:X}")
            };
            let prefix = if !spec.alternate || number == 0 {
                ""
            } else if spec.conversion == 'x' {
                "0x"
            } else {
                "0X"
            };
            format!("{prefix}{text}")
        }
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
    let (radix, digits) =
        if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
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
        chars[start..(start + take).min(chars.len())]
            .iter()
            .collect()
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
            return Err(Error::Syntax(
                "redim's third parameter is 'Preserve'".into(),
            ));
        }
        true
    } else {
        false
    };
    interp.redim(&target, size, preserve)
}

fn ifdefined(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "ifdefined", 1, 1)?;
    // §7.6: the parameter is a *name*, and the corpus passes literals
    // (`ifdefined 1`, `ifdefined '2'`) to show that they are not defined.
    // `c[0]` asks about the array `c`, not about an element.
    let name = match &args[0][..] {
        [first, ..] => match &first.kind {
            crate::lex::Token::Ident(name) => name.clone(),
            _ => {
                interp.set_result(0);
                return Ok(());
            }
        },
        [] => {
            interp.set_result(0);
            return Ok(());
        }
    };
    // 0 undefined, 1 integer, 3 string, 4 label, 5 integer array, 6 string
    // array.
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

// -- the connection ---------------------------------------------------------

fn testlink(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "testlink", 0, 0)?;
    let state = interp.host().link_state();
    interp.set_result(state);
    Ok(())
}

fn connect(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    // The parameters are Tera Term's own command line, so they are joined back
    // into the one string the host understands rather than interpreted here.
    let mut parameters = String::new();
    for index in 0..args.len() {
        if index > 0 {
            parameters.push(' ');
        }
        parameters.push_str(&interp.str_param(args, index)?);
    }
    interp.host().connect(&parameters);
    // §7.1: 0 unlinked, 1 linked only, 2 connected.
    let state = interp.host().link_state();
    interp.set_result(state);
    Ok(())
}

fn setting(interp: &mut Interpreter<'_>, name: &str, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, name, 0, 8)?;
    let mut value = String::new();
    for index in 0..args.len() {
        if index > 0 {
            value.push(' ');
        }
        value.push_str(&interp.str_param(args, index)?);
    }
    interp.host().note_setting(name, &value);
    Ok(())
}

fn send(interp: &mut Interpreter<'_>, args: &[Vec<Tok>], newline: bool) -> Result<(), Error> {
    let bytes = payload(interp, args, 0)?;
    let mut bytes = bytes;
    if newline {
        // §7.1: `sendln` appends the terminal's newline, which Tera Term spells
        // CR LF.
        bytes.extend_from_slice(b"\r\n");
    }
    interp.host().send(&bytes);
    Ok(())
}

fn send_multicast(
    interp: &mut Interpreter<'_>,
    args: &[Vec<Tok>],
    newline: bool,
) -> Result<(), Error> {
    interp.arity(args, "sendmulticast", 2, usize::MAX)?;
    // Parameter 1 is the multicast name; a host with one session ignores it.
    let _name = interp.str_param(args, 0)?;
    let mut bytes = payload(interp, args, 1)?;
    if newline {
        bytes.extend_from_slice(b"\r\n");
    }
    interp.host().send(&bytes);
    Ok(())
}

/// The parameters from `from` on, as bytes: a string is its bytes, an integer
/// is one byte — the low 8 bits, truncated rather than refused (§7.1).
fn payload(interp: &Interpreter<'_>, args: &[Vec<Tok>], from: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    for index in from..args.len() {
        let expr = interp.param(args, index)?;
        match interp.eval(&expr)? {
            Value::Int(number) => bytes.push((number & 0xff) as u8),
            Value::Str(text) => bytes.extend_from_slice(text.as_bytes()),
        }
    }
    Ok(bytes)
}

fn sendfile(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "sendfile", 1, 2)?;
    let path = interp.str_param(args, 0)?;
    // `<binary>` non-zero sends the bytes unchanged; zero converts newlines,
    // which this host leaves to the terminal and does not do here.
    let _binary = if args.len() > 1 {
        interp.int_param(args, 1)? != 0
    } else {
        false
    };
    let opened = interp.host().files().open(&path, false, true);
    let Ok(handle) = opened else {
        interp.set_result(-1);
        return Ok(());
    };
    let mut bytes = Vec::new();
    loop {
        match interp.host().files().read(handle, 4096) {
            Some(chunk) if !chunk.is_empty() => bytes.extend_from_slice(&chunk),
            _ => break,
        }
    }
    interp.host().files().close(handle);
    interp.host().send(&bytes);
    interp.set_result(0);
    Ok(())
}

fn recvln(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "recvln", 0, 0)?;
    match interp.host().read_line() {
        Some(line) => {
            interp.system_mut().inputstr = line;
            interp.set_result(1);
        }
        None => interp.set_result(0),
    }
    Ok(())
}

/// §7.1: `waitn` waits for a number of bytes rather than for a string.
fn waitn(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "waitn", 1, 1)?;
    let count = interp.int_param(args, 0)?.max(0) as usize;
    let millis = wait_millis(interp);
    match interp.host().wait_bytes(count, millis) {
        Some(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            interp.system_mut().inputstr = truncate(&text);
            interp.set_result(1);
        }
        None => interp.set_result(0),
    }
    Ok(())
}

/// §7.1: `waitrecv` waits for a fixed-size chunk with a string at a position.
fn waitrecv(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "waitrecv", 3, 3)?;
    let pattern = interp.str_param(args, 0)?;
    let len = interp.int_param(args, 1)?.max(0) as usize;
    let position = interp.int_param(args, 2)?.max(1) as usize;
    let millis = wait_millis(interp);
    match interp.host().wait_recv(&pattern, len, position, millis) {
        Some(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            interp.system_mut().inputstr = truncate(&text);
            interp.set_result(1);
        }
        None => interp.set_result(0),
    }
    Ok(())
}

/// §7.1: `timeout` seconds plus `mtimeout` milliseconds; zero or less means the
/// wait never times out, which is why -1 stands for "infinite".
fn wait_millis(interp: &Interpreter<'_>) -> i64 {
    let millis = i64::from(interp.system().timeout) * 1000 + i64::from(interp.system().mtimeout);
    if millis <= 0 { -1 } else { millis }
}

fn truncate(text: &str) -> String {
    text.chars().take(crate::vars::MAX_STRING_CHARS).collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// `wait`: any of the patterns, anywhere in the stream.
    Any,
    /// `waitln`: a whole line containing one of them; the line goes to
    /// `inputstr`.
    Line,
    /// `waitregex`: a line matching one of them, with `matchstr` filled too.
    Regex,
}

fn wait(interp: &mut Interpreter<'_>, args: &[Vec<Tok>], kind: Wait) -> Result<(), Error> {
    let name = match kind {
        Wait::Any => "wait",
        Wait::Line => "waitln",
        Wait::Regex => "waitregex",
    };
    interp.arity(args, name, 1, 10)?;
    let mut patterns = Vec::new();
    for index in 0..args.len() {
        patterns.push(interp.str_param(args, index)?);
    }
    // §7.1: `timeout` in seconds plus `mtimeout` in milliseconds; zero or less
    // means the wait never times out, which is why -1 is "infinite" here.
    let millis = i64::from(interp.system().timeout) * 1000 + i64::from(interp.system().mtimeout);
    let millis = if millis <= 0 { -1 } else { millis };
    let answer = interp.host().wait_for(&patterns, millis, kind != Wait::Any);
    match answer {
        Some((index, text)) => {
            // §7.1: the result is which pattern matched, counted from 1.
            interp.set_result(index as i32 + 1);
            match kind {
                Wait::Any => {}
                Wait::Line => interp.system_mut().inputstr = text,
                Wait::Regex => {
                    interp.system_mut().inputstr = text.clone();
                    interp.system_mut().matchstr = text;
                }
            }
        }
        None => interp.set_result(0),
    }
    Ok(())
}

// -- files ------------------------------------------------------------------

/// §7.4's file commands report 0 for success, the opposite of the connection
/// commands — which is the single most common way a file macro checks the wrong
/// thing, so every one of these says so where it sets `result`.
fn fileopen(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "fileopen", 3, 4)?;
    let target = interp.name_param(args, 0)?;
    let path = interp.str_param(args, 1)?;
    let append = interp.int_param(args, 2)? != 0;
    let read_only = if args.len() > 3 {
        interp.int_param(args, 3)? != 0
    } else {
        false
    };
    match interp.host().files().open(&path, append, read_only) {
        Ok(handle) => {
            interp.assign_int(&target, handle)?;
            interp.set_result(0);
        }
        Err(()) => {
            interp.assign_int(&target, -1)?;
            interp.set_result(-1);
        }
    }
    Ok(())
}

fn filecreate(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "filecreate", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let path = interp.str_param(args, 1)?;
    match interp.host().files().create(&path) {
        Ok(handle) => {
            interp.assign_int(&target, handle)?;
            interp.set_result(0);
        }
        Err(()) => {
            interp.assign_int(&target, -1)?;
            interp.set_result(-1);
        }
    }
    Ok(())
}

fn fileclose(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "fileclose", 1, 1)?;
    let handle = interp.int_param(args, 0)?;
    let closed = interp.host().files().close(handle);
    interp.set_result(if closed { 0 } else { -1 });
    Ok(())
}

fn filereadln(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "filereadln", 2, 2)?;
    let handle = interp.int_param(args, 0)?;
    let target = interp.name_param(args, 1)?;
    match interp.host().files().read_line(handle) {
        Some(line) => {
            interp.assign_str(&target, &line)?;
            interp.set_result(0);
        }
        // §7.4 inverts the convention: 1 means the read hit the end.
        None => interp.set_result(1),
    }
    Ok(())
}

fn fileread(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "fileread", 3, 3)?;
    let handle = interp.int_param(args, 0)?;
    let bytes = interp
        .int_param(args, 1)?
        .clamp(1, crate::vars::MAX_STRING_CHARS as i32) as usize;
    let target = interp.name_param(args, 2)?;
    match interp.host().files().read(handle, bytes) {
        Some(data) => {
            interp.assign_str(&target, &String::from_utf8_lossy(&data))?;
            interp.set_result(0);
        }
        None => interp.set_result(1),
    }
    Ok(())
}

fn filewrite(interp: &mut Interpreter<'_>, args: &[Vec<Tok>], newline: bool) -> Result<(), Error> {
    interp.arity(
        args,
        if newline { "filewriteln" } else { "filewrite" },
        2,
        2,
    )?;
    let handle = interp.int_param(args, 0)?;
    let mut bytes = payload(interp, args, 1)?;
    if newline {
        bytes.extend_from_slice(b"\r\n");
    }
    let written = interp.host().files().write(handle, &bytes);
    interp.set_result(if written { 0 } else { -1 });
    Ok(())
}

fn fileseek(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "fileseek", 3, 3)?;
    let handle = interp.int_param(args, 0)?;
    let offset = i64::from(interp.int_param(args, 1)?);
    let origin = interp.int_param(args, 2)?;
    let moved = interp.host().files().seek(handle, offset, origin);
    interp.set_result(if moved { 0 } else { -1 });
    Ok(())
}

fn filemarkptr(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "filemarkptr", 1, 1)?;
    let handle = interp.int_param(args, 0)?;
    let marked = interp.host().files().mark(handle);
    interp.set_result(if marked { 0 } else { -1 });
    Ok(())
}

fn fileseekback(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "fileseekback", 1, 1)?;
    let handle = interp.int_param(args, 0)?;
    let moved = interp.host().files().seek_mark(handle);
    interp.set_result(if moved { 0 } else { -1 });
    Ok(())
}

fn filetruncate(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "filetruncate", 2, 2)?;
    let path = interp.str_param(args, 0)?;
    let size = interp.int_param(args, 1)?.max(0) as u64;
    let truncated = interp.host().files().truncate(&path, size);
    interp.set_result(if truncated { 0 } else { -1 });
    Ok(())
}

fn filedelete(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "filedelete", 1, 1)?;
    let path = interp.str_param(args, 0)?;
    let deleted = interp.host().files().delete(&path);
    interp.set_result(if deleted { 0 } else { -1 });
    Ok(())
}

fn filesearch(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "filesearch", 1, 1)?;
    let path = interp.str_param(args, 0)?;
    let found = interp.host().files().exists(&path);
    interp.set_result(if found { 1 } else { 0 });
    Ok(())
}

fn foldersearch(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    // A memory file system has no directories; asking is answered with "no".
    interp.arity(args, "foldersearch", 1, 1)?;
    let _path = interp.str_param(args, 0)?;
    interp.set_result(0);
    Ok(())
}

#[derive(Clone, Copy)]
enum Copy {
    Copy,
    Rename,
    Concat,
}

fn filecopy(interp: &mut Interpreter<'_>, args: &[Vec<Tok>], which: Copy) -> Result<(), Error> {
    let name = match which {
        Copy::Copy => "filecopy",
        Copy::Rename => "filerename",
        Copy::Concat => "fileconcat",
    };
    interp.arity(args, name, 2, 2)?;
    let from = interp.str_param(args, 0)?;
    let to = interp.str_param(args, 1)?;
    let done = match which {
        Copy::Copy => interp.host().files().copy(&from, &to),
        Copy::Rename => interp.host().files().rename(&from, &to),
        Copy::Concat => interp.host().files().concat(&from, &to),
    };
    interp.set_result(if done { 0 } else { -1 });
    Ok(())
}

fn findfirst(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "findfirst", 3, 3)?;
    let handle_target = interp.name_param(args, 0)?;
    let pattern = interp.str_param(args, 1)?;
    let name_target = interp.name_param(args, 2)?;
    match interp.host().files().find_start(&pattern) {
        Some(handle) => {
            interp.assign_int(&handle_target, handle)?;
            // The first match comes back from `findfirst` itself.
            match interp.host().files().find_next(handle) {
                Some(name) => {
                    interp.assign_str(&name_target, &name)?;
                    interp.set_result(1);
                }
                None => interp.set_result(0),
            }
        }
        None => {
            interp.assign_int(&handle_target, -1)?;
            interp.set_result(0);
        }
    }
    Ok(())
}

fn findnext(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "findnext", 2, 2)?;
    let handle = interp.int_param(args, 0)?;
    let name_target = interp.name_param(args, 1)?;
    match interp.host().files().find_next(handle) {
        Some(name) => {
            interp.assign_str(&name_target, &name)?;
            interp.set_result(1);
        }
        None => interp.set_result(0),
    }
    Ok(())
}

fn findclose(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "findclose", 1, 1)?;
    let handle = interp.int_param(args, 0)?;
    interp.host().files().find_close(handle);
    interp.set_result(0);
    Ok(())
}

fn filestat(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "filestat", 2, 4)?;
    let path = interp.str_param(args, 0)?;
    let size_target = interp.name_param(args, 1)?;
    // The outputs are assigned either way: a macro that reads them after a
    // failed `filestat` is reading zeroes, not an uninitialised variable.
    let size = interp.host().files().size(&path);
    interp.assign_int(&size_target, size.unwrap_or(0).min(i32::MAX as u64) as i32)?;
    // The modification time and drive number need a real file system's
    // metadata; the parameters are accepted and left at zero.
    for index in 2..args.len() {
        let target = interp.name_param(args, index)?;
        interp.assign_int(&target, 0)?;
    }
    interp.set_result(if size.is_some() { 0 } else { -1 });
    Ok(())
}

fn foldercreate(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "foldercreate", 1, 1)?;
    let path = interp.str_param(args, 0)?;
    let created = interp.host().files().create_dir(&path);
    interp.set_result(if created { 0 } else { -1 });
    Ok(())
}

/// §7.4: join a directory and a name, inserting a separator if needed. **It
/// creates nothing**, which is why the name is `makepath` and not `mkdir`.
fn makepath(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "makepath", 3, 3)?;
    let target = interp.name_param(args, 0)?;
    let dir = interp.str_param(args, 1)?;
    let name = interp.str_param(args, 2)?;
    let joined = if dir.is_empty() || dir.ends_with(['\\', '/']) {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    };
    interp.assign_str(&target, &joined)
}

fn basename(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "basename", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let path = interp.str_param(args, 1)?;
    let name = path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(path.as_str())
        .to_string();
    interp.assign_str(&target, &name)
}

fn dirname(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "dirname", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let path = interp.str_param(args, 1)?;
    let dir = match path.rfind(['\\', '/']) {
        Some(at) => path[..at].to_string(),
        None => String::new(),
    };
    interp.assign_str(&target, &dir)
}

// -- time and environment ---------------------------------------------------

/// §7.6: `gettime` and `getdate` differ only in their default format.
fn gettime(interp: &mut Interpreter<'_>, args: &[Vec<Tok>], date: bool) -> Result<(), Error> {
    interp.arity(args, if date { "getdate" } else { "gettime" }, 1, 3)?;
    let target = interp.name_param(args, 0)?;
    let default = if date { "%Y-%m-%d" } else { "%H:%M:%S" };
    let format = if args.len() > 1 {
        interp.str_param(args, 1)?
    } else {
        default.to_string()
    };
    // `<timezone>` (4.105+) would need a time-zone database; the host's local
    // clock is what a macro gets, and the parameter is recorded rather than
    // silently dropped.
    if args.len() > 2 {
        let zone = interp.str_param(args, 2)?;
        interp.host().note_setting("timezone", &zone);
    }
    let text = crate::time::strftime(&format, interp.host().now());
    if text.chars().count() > crate::vars::MAX_STRING_CHARS {
        // §7.6: result 1 is "the result was too long".
        interp.set_result(1);
        return Ok(());
    }
    interp.assign_str(&target, &text)?;
    interp.set_result(0);
    Ok(())
}

fn getenv(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "getenv", 2, 2)?;
    let name = interp.str_param(args, 0)?;
    let target = interp.name_param(args, 1)?;
    match interp.host().getenv(&name) {
        Some(value) => {
            interp.assign_str(&target, &value)?;
            interp.set_result(0);
        }
        None => interp.set_result(-1),
    }
    Ok(())
}

fn setenv(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "setenv", 2, 2)?;
    let name = interp.str_param(args, 0)?;
    let value = interp.str_param(args, 1)?;
    interp.host().setenv(&name, &value);
    interp.set_result(0);
    Ok(())
}

/// The Tera Term version this interpreter answers for (§7.6 `getver`).
///
/// A macro asks to find out whether a command exists, so the answer has to be
/// the version whose command set [`crate::commands`] implements — 5.8, where
/// `delvar` and `redim` arrived.
pub const VERSION: &str = "5.8.0";

fn getver(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "getver", 1, 2)?;
    let target = interp.name_param(args, 0)?;
    interp.assign_str(&target, VERSION)?;
    if args.len() < 2 {
        return Ok(());
    }
    let wanted = interp.str_param(args, 1)?;
    let ours = version(VERSION);
    let theirs = version(&wanted);
    let result = match (ours, theirs) {
        (Some(ours), Some(theirs)) => match ours.cmp(&theirs) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
        _ => -2,
    };
    interp.set_result(result);
    Ok(())
}

/// A version string as comparable numbers; `None` when it is not one.
fn version(text: &str) -> Option<Vec<u32>> {
    let mut parts = Vec::new();
    for part in text.split('.') {
        if part.is_empty() {
            return None;
        }
        parts.push(part.parse().ok()?);
    }
    Some(parts)
}

fn getspecialfolder(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "getspecialfolder", 2, 2)?;
    let target = interp.name_param(args, 0)?;
    let name = interp.str_param(args, 1)?;
    match interp.host().special_folder(&name) {
        Some(path) => {
            interp.assign_str(&target, &path)?;
            interp.set_result(0);
        }
        None => interp.set_result(-1),
    }
    Ok(())
}

/// `expandenv`: `%NAME%` references, resolved through the host's environment.
fn expandenv(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "expandenv", 1, 2)?;
    let target = interp.name_param(args, 0)?;
    let source = if args.len() > 1 {
        interp.str_param(args, 1)?
    } else {
        as_string(interp.get(&target)?)
    };
    let mut out = String::with_capacity(source.len());
    let chars: Vec<char> = source.chars().collect();
    let mut at = 0usize;
    while at < chars.len() {
        if chars[at] != '%' {
            out.push(chars[at]);
            at += 1;
            continue;
        }
        // `%NAME%`; an unpaired or unknown name is copied through unchanged.
        let close = chars[at + 1..].iter().position(|&ch| ch == '%');
        let Some(close) = close else {
            out.push('%');
            at += 1;
            continue;
        };
        let name: String = chars[at + 1..at + 1 + close].iter().collect();
        match interp.host().getenv(&name) {
            Some(value) => out.push_str(&value),
            None => {
                out.push('%');
                out.push_str(&name);
                out.push('%');
            }
        }
        at += close + 2;
    }
    interp.assign_str(&target, &out)
}

fn exec(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "exec", 1, 4)?;
    let command_line = interp.str_param(args, 0)?;
    // `<show>` is a window instruction; `<wait>` non-zero blocks and puts the
    // child's exit code in `result` (§7.6).
    let wait = if args.len() > 2 {
        interp.int_param(args, 2)? != 0
    } else {
        false
    };
    let code = interp.host().exec(&command_line, wait);
    interp.set_result(code);
    Ok(())
}

// -- §7.5 passwords ---------------------------------------------------------

fn setpassword(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    // The `2` family adds `<encrypt str>`, which the store here does not use.
    interp.arity(args, "setpassword", 3, 4)?;
    let file = interp.str_param(args, 0)?;
    let name = interp.str_param(args, 1)?;
    let password = interp.str_param(args, 2)?;
    interp.set_password(&file, &name, &password);
    interp.set_result(0);
    Ok(())
}

fn getpassword(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "getpassword", 3, 4)?;
    let file = interp.str_param(args, 0)?;
    let name = interp.str_param(args, 1)?;
    let target = interp.name_param(args, 2)?;
    if let Some(password) = interp.password(&file, &name) {
        interp.assign_str(&target, &password)?;
        interp.set_result(0);
        return Ok(());
    }
    // §7.5: a name that is not in the file is asked for and then stored, which
    // is what makes the first run of an unattended script interactive and the
    // rest not.
    let prompt = format!("{name}");
    match interp.host().passwordbox(&prompt, &file) {
        Some(password) => {
            interp.set_password(&file, &name, &password);
            interp.assign_str(&target, &password)?;
            interp.set_result(0);
        }
        None => interp.set_result(-1),
    }
    Ok(())
}

fn ispassword(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "ispassword", 2, 3)?;
    let file = interp.str_param(args, 0)?;
    let name = interp.str_param(args, 1)?;
    let present = interp.password(&file, &name).is_some();
    interp.set_result(if present { 1 } else { 0 });
    Ok(())
}

fn delpassword(interp: &mut Interpreter<'_>, args: &[Vec<Tok>]) -> Result<(), Error> {
    interp.arity(args, "delpassword", 2, 3)?;
    let file = interp.str_param(args, 0)?;
    let name = interp.str_param(args, 1)?;
    let removed = interp.forget_password(&file, &name);
    interp.set_result(if removed { 0 } else { -1 });
    Ok(())
}

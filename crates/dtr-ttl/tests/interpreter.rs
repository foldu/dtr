//! The interpreter, run against a host that answers.
//!
//! Every test here is a macro and a recording of what it did. That is the whole
//! point of the [`Host`](dtr_ttl::host::Host) boundary: the dialogs a macro
//! shows become values a test can compare, so "the loop printed 55" is an
//! assertion rather than a screenshot.

use dtr_ttl::error::Error;
use dtr_ttl::host::{Event, RecordingHost};
use dtr_ttl::interp::{Failure, Interpreter};

/// Run `text` against a fresh recording host and return the host and the exit
/// status, panicking on an interpreter error.
fn run(text: &str) -> (RecordingHost, i32) {
    let mut host = RecordingHost::new();
    let status = Interpreter::run_text(&mut host, text).expect("the macro runs");
    (host, status)
}

fn messages(text: &str) -> Vec<String> {
    let (host, _) = run(text);
    host.messages().into_iter().map(str::to_string).collect()
}

fn failure(text: &str) -> Failure {
    let mut host = RecordingHost::new();
    Interpreter::run_text(&mut host, text).unwrap_err()
}

#[test]
fn a_for_loop_counts_up_and_down() {
    let (host, _) = run("\
s = ''
for i 1 4
    strconcat s i
next
for i 3 1
    strconcat s i
next
messagebox s 'count'
");
    assert_eq!(host.messages(), vec!["1234321"]);
}

#[test]
fn a_for_loop_runs_once_when_first_and_last_agree() {
    let (host, _) = run("n = 0
for i 7 7
    n = n + 1
next
messagebox n 'n'
");
    assert_eq!(host.messages(), vec!["1"]);
}

#[test]
fn the_step_of_a_for_loop_follows_the_later_limit() {
    // §6.2: no step parameter, so `for i 5 1` must count down by itself.
    let (host, _) = run("s = ''
for i 5 1
    strconcat s i
next
messagebox s 'down'
");
    assert_eq!(host.messages(), vec!["54321"]);
}

#[test]
fn an_if_chain_stops_at_the_first_true_condition() {
    for (value, expected) in [(1, "one"), (2, "two"), (3, "other")] {
        let text = format!(
            "a = {value}
if a = 1 then
    messagebox 'one' 'x'
elseif a = 2 then
    messagebox 'two' 'x'
else
    messagebox 'other' 'x'
endif
"
        );
        assert_eq!(messages(&text), vec![expected], "a = {value}");
    }
}

#[test]
fn the_single_line_if_can_skip_a_statement() {
    let (host, _) = run("a = 0
if a messagebox 'ran' 'x'
if 1 messagebox 'ran' 'x'
");
    assert_eq!(host.messages(), vec!["ran"]);
}

#[test]
fn while_break_and_continue() {
    let (host, _) = run("i = 0
while 1
    i = i + 1
    if i = 3 continue
    if i = 5 break
endwhile
messagebox i 'i'
");
    assert_eq!(host.messages(), vec!["5"]);
}

#[test]
fn until_runs_while_the_condition_is_zero() {
    let (host, _) = run("i = 0
until i >= 3
    i = i + 1
enduntil
messagebox i 'i'
");
    assert_eq!(host.messages(), vec!["3"]);
}

#[test]
fn do_loop_runs_at_least_once_even_with_a_true_condition() {
    let (host, _) = run("i = 0
do
    i = i + 1
loop while i > 100
messagebox i 'i'
");
    assert_eq!(host.messages(), vec!["1"], "the body ran before the test");
}

#[test]
fn a_do_while_tests_before_the_body() {
    let (host, _) = run("i = 0
do while i > 100
    i = i + 1
loop
messagebox i 'i'
");
    assert_eq!(host.messages(), vec!["0"], "the body never ran");
}

#[test]
fn call_returns_to_the_line_after_it() {
    let (host, _) = run("s = 'a'
call shout
strconcat s 'c'
messagebox s 'x'
end

:shout
    strconcat s 'b'
return
");
    assert_eq!(host.messages(), vec!["abc"]);
}

#[test]
fn a_goto_out_of_a_loop_leaves_it() {
    let (host, _) = run("i = 0
:again
i = i + 1
if i < 3 goto again
messagebox i 'i'
");
    assert_eq!(host.messages(), vec!["3"]);
}

#[test]
fn a_goto_out_of_a_for_stops_counting() {
    let (host, _) = run("last = 0
for i 1 100
    last = i
    if i = 3 goto done
next
:done
messagebox last 'last'
");
    assert_eq!(host.messages(), vec!["3"]);
}

#[test]
fn end_and_exit_both_end_the_main_file() {
    let (host, _) = run("messagebox 'before' 'x'
end
messagebox 'after' 'x'
");
    assert_eq!(host.messages(), vec!["before"]);

    let (host, _) = run("messagebox 'before' 'x'
exit
messagebox 'after' 'x'
");
    assert_eq!(host.messages(), vec!["before"]);
}

#[test]
fn include_runs_the_file_and_comes_back() {
    let mut host = RecordingHost::new()
        .with_file("part.ttl", b"messagebox 'inside' 'x'\n");
    let status = Interpreter::run_text(
        &mut host,
        "messagebox 'before' 'x'
include 'part.ttl'
messagebox 'after' 'x'
",
    )
    .unwrap();
    assert_eq!(status, 0);
    assert_eq!(host.messages(), vec!["before", "inside", "after"]);
}

#[test]
fn exit_in_an_included_file_returns_to_the_includer() {
    let mut host = RecordingHost::new()
        .with_file("part.ttl", b"messagebox 'inside' 'x'\nexit\nmessagebox 'never' 'x'\n");
    Interpreter::run_text(
        &mut host,
        "include 'part.ttl'
messagebox 'after' 'x'
",
    )
    .unwrap();
    assert_eq!(host.messages(), vec!["inside", "after"]);
}

#[test]
fn a_missing_include_is_the_documented_error() {
    let failure = failure("include 'nowhere.ttl'\n");
    assert!(matches!(failure.error, Error::CannotOpenFile(_)));
    assert_eq!(failure.error.to_string(), "Can't open file.");
    assert_eq!(failure.line, 1);
}

#[test]
fn include_cannot_nest_forever() {
    let mut host = RecordingHost::new();
    for _ in 0..12 {
        host = host.with_file("loop.ttl", b"include 'loop.ttl'\n");
    }
    let failure = Interpreter::run_text(&mut host, "include 'loop.ttl'\n").unwrap_err();
    assert!(
        matches!(failure.error, Error::CannotOpenFile(_)),
        "{failure:?}"
    );
}

#[test]
fn execcmnd_runs_a_statement_built_at_run_time() {
    let (host, _) = run("name = 'target'
strconcat name '='
strconcat name '7'
execcmnd name
messagebox target 'target'
");
    assert_eq!(host.messages(), vec!["7"]);
}

#[test]
fn the_system_variables_are_readable_and_writable() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse(
            "result = 3\ninputstr = 'typed'\ntimeout = 7\n",
        )
        .unwrap(),
    )
    .with_params(vec!["one".into(), "two".into()]);
    interp.run().unwrap();
    assert_eq!(interp.result(), 3);
    assert_eq!(interp.system().inputstr, "typed");
    assert_eq!(interp.system().timeout, 7);
    assert_eq!(interp.system().params.len(), 2);
}

#[test]
fn the_command_line_parameters_are_param1_and_paramcnt() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse("s = param1\nt = param2\nn = paramcnt\n").unwrap(),
    )
    .with_params(vec!["one".into(), "two".into()]);
    interp.run().unwrap();
    assert_eq!(
        interp.vars().get("s").unwrap(),
        dtr_ttl::value::Value::Str("one".into())
    );
    assert_eq!(
        interp.vars().get("t").unwrap(),
        dtr_ttl::value::Value::Str("two".into())
    );
    assert_eq!(
        interp.vars().get("n").unwrap(),
        dtr_ttl::value::Value::Int(2)
    );
}

#[test]
fn setexitcode_sets_the_status() {
    let (_, status) = run("setexitcode 7\n");
    assert_eq!(status, 7);

    let (_, status) = run("setexitcode 7\nsetexitcode 0\n");
    assert_eq!(status, 0);
}

#[test]
fn a_dialog_records_what_it_was_shown() {
    let (host, _) = run("messagebox 'hello' 'title'\n");
    assert_eq!(
        host.events(),
        &[Event::Message {
            message: "hello".into(),
            title: "title".into(),
        }]
    );
}

#[test]
fn a_yes_no_answer_becomes_result() {
    let mut host = RecordingHost::new().answering_yes();
    Interpreter::run_text(&mut host, "yesnobox 'continue?' 'x'\nresult = result\n").unwrap();
    assert_eq!(host.events().len(), 1);

    let mut host = RecordingHost::new().answering_yes();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse("yesnobox 'continue?' 'x'\n").unwrap(),
    );
    interp.run().unwrap();
    assert_eq!(interp.result(), 1);
}

#[test]
fn a_cancelled_input_box_leaves_inputstr_empty() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse("inputbox 'name?' 'x' 'default'\n").unwrap(),
    );
    interp.run().unwrap();
    assert_eq!(interp.system().inputstr, "");

    let mut host = RecordingHost::new().answering("typed");
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse("inputbox 'name?' 'x' 'default'\n").unwrap(),
    );
    interp.run().unwrap();
    assert_eq!(interp.system().inputstr, "typed");
}

#[test]
fn listbox_reads_an_array_and_answers_with_an_index() {
    let mut host = RecordingHost::new().choosing(1);
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse(
            "strdim items 3
items[0] = 'first'
items[1] = 'second'
items[2] = 'third'
listbox 'pick' 'title' items
",
        )
        .unwrap(),
    );
    interp.run().unwrap();
    assert_eq!(interp.result(), 1);
    assert!(matches!(
        &host.events()[0],
        Event::List { items, .. } if items == &vec!["first".to_string(), "second".into(), "third".into()]
    ));
}

#[test]
fn a_cancelled_listbox_is_minus_one() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse("strdim items 1\nlistbox 'pick' 'title' items\n")
            .unwrap(),
    );
    interp.run().unwrap();
    assert_eq!(interp.result(), -1);
}

#[test]
fn sprintf2_formats_into_a_variable() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse(
            "sprintf2 a '[%d %2d]' 1 1
sprintf2 b '[%02d]' 3
sprintf2 c '[%.2f]' '3.14159'
sprintf2 d '[%*d]' 2 3
sprintf2 e '[%.*f]' 2 '3.1415926535'
sprintf2 f '[%e]' '314.15926535'
sprintf2 g '%s-%x' 'ab' 255
",
        )
        .unwrap(),
    );
    interp.run().unwrap();
    for (name, expected) in [
        ("a", "[1  1]"),
        ("b", "[03]"),
        ("c", "[3.14]"),
        ("d", "[ 3]"),
        ("e", "[3.14]"),
        ("f", "[3.141593e+02]"),
        ("g", "ab-ff"),
    ] {
        assert_eq!(
            interp.vars().get(name).unwrap(),
            dtr_ttl::value::Value::Str(expected.into()),
            "{name}"
        );
    }
}

#[test]
fn sprintf_writes_inputstr_and_sets_result() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse("sprintf '%d-%s' 4 'x'\n").unwrap(),
    );
    interp.run().unwrap();
    assert_eq!(interp.system().inputstr, "4-x");
    assert_eq!(interp.result(), 0);
}

#[test]
fn a_missing_argument_is_result_three() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse("sprintf '%d %d' 1\n").unwrap(),
    );
    interp.run().unwrap();
    assert_eq!(interp.result(), 3);
}

#[test]
fn strconcat_builds_a_string_from_nothing() {
    let (host, _) = run("strconcat s 'a'
strconcat s 'b'
strconcat s 3
messagebox s 'x'
");
    assert_eq!(host.messages(), vec!["ab3"]);
}

#[test]
fn strlen_counts_bytes_and_strcompare_answers_minus_one_zero_one() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse(
            "strcompare 'a' 'a'
r1 = result
strcompare 'a' 'b'
r2 = result
strcompare 'b' 'a'
r3 = result
strlen 'caf\u{e9}'
r4 = result
",
        )
        .unwrap(),
    );
    interp.run().unwrap();
    assert_eq!(interp.vars().get("r1").unwrap(), dtr_ttl::value::Value::Int(0));
    assert_eq!(interp.vars().get("r2").unwrap(), dtr_ttl::value::Value::Int(-1));
    assert_eq!(interp.vars().get("r3").unwrap(), dtr_ttl::value::Value::Int(1));
    assert_eq!(interp.vars().get("r4").unwrap(), dtr_ttl::value::Value::Int(5));
}

#[test]
fn strsplit_fills_the_nine_slots_and_strjoin_reads_them_back() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse(
            "strsplit 'a:b:c' ':'
n = result
strjoin joined '-'
",
        )
        .unwrap(),
    );
    interp.run().unwrap();
    assert_eq!(interp.vars().get("n").unwrap(), dtr_ttl::value::Value::Int(3));
    assert_eq!(
        interp.vars().get("joined").unwrap(),
        dtr_ttl::value::Value::Str("a-b-c------".into()),
        "nine slots are joined, and the six unused ones are empty"
    );
}

#[test]
fn strreplace_clears_the_match_slots() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse(
            "strsplit 'a:b' ':'
t = 'abc'
strreplace t 2 'b' 'B'
",
        )
        .unwrap(),
    );
    interp.run().unwrap();
    assert_eq!(interp.result(), 1);
    assert_eq!(
        interp.vars().get("t").unwrap(),
        dtr_ttl::value::Value::Str("aBc".into())
    );
    assert_eq!(
        interp.system().groupmatchstr[0], "",
        "`strreplace` clears the slots instead of filling them"
    );
}

#[test]
fn strspecial_expands_escapes() {
    let (host, _) = run("strspecial s 'a\\nb\\tc\\\\d'
dispstr s
");
    assert_eq!(host.disp(), "a\nb\tc\\d");
}

#[test]
fn pause_and_mpause_ask_the_host_for_time() {
    let (host, _) = run("pause 1\nmpause 250\n");
    assert_eq!(host.waited_ms, 1250);
}

#[test]
fn ifdefined_answers_the_documented_codes() {
    let mut host = RecordingHost::new();
    let mut interp = Interpreter::with_program(
        &mut host,
        dtr_ttl::program::Program::parse(
            "intdim ia 2
strdim sa 2
i = 1
s = 'x'
:label
ifdefined nothing
n0 = result
ifdefined ia
n5 = result
ifdefined sa
n6 = result
ifdefined i
n1 = result
ifdefined s
n3 = result
ifdefined label
n4 = result
",
        )
        .unwrap(),
    );
    interp.run().unwrap();
    for (name, expected) in [("n0", 0), ("n5", 5), ("n6", 6), ("n1", 1), ("n3", 3), ("n4", 4)] {
        assert_eq!(
            interp.vars().get(name).unwrap(),
            dtr_ttl::value::Value::Int(expected),
            "{name}"
        );
    }
}

#[test]
fn division_by_zero_is_an_interpreter_error() {
    let failure = failure("a = 1\nb = a / 0\n");
    assert!(matches!(failure.error, Error::DivideByZero));
    assert_eq!(failure.line, 2);
    assert_eq!(failure.error.to_string(), "Divide by zero.");
}

#[test]
fn reading_an_undefined_variable_is_an_error() {
    let failure = failure("a = nothing + 1\n");
    assert!(matches!(failure.error, Error::VariableNotInitialized(_)));
}

#[test]
fn an_unknown_command_stops_the_macro() {
    let failure = failure("nosuchcommand 1 2\n");
    assert!(matches!(failure.error, Error::UnknownCommand(_)));
    assert_eq!(failure.error.to_string(), "Unknown command.");
}

#[test]
fn a_type_mismatch_is_an_error_not_a_conversion() {
    let failure = failure("s = 'text'\nif s then\nendif\n");
    assert!(matches!(failure.error, Error::TypeMismatch));
}

#[test]
fn a_runaway_loop_hits_the_step_limit() {
    let mut host = RecordingHost::new();
    let program = dtr_ttl::program::Program::parse("while 1\nendwhile\n").unwrap();
    let failure = Interpreter::with_program(&mut host, program)
        .with_step_limit(1000)
        .run()
        .unwrap_err();
    assert!(matches!(failure.error, Error::StepLimit));
}

#[test]
fn a_deep_call_stack_overflows() {
    let mut text = String::from("call sub\nend\n:sub\n");
    text.push_str("call sub\nreturn\n");
    let failure = failure(&text);
    assert!(matches!(failure.error, Error::StackOverflow), "{failure:?}");
}

//! The language, run as a host runs it: compiled whole, then run to its
//! end, through every suspension.
use crate::*;

/// A host with one value of its own, a ticket, and five words: `ticket`
/// (-- t), which hands one out at once; `ask` (n -- n*10), which suspends
/// for the host to answer; `fail`, which fails; `maybe` (n -- [n]), which
/// gives n back only above 0; and `liar`, which says it gives a value and
/// gives none.
#[derive(Clone, Debug, PartialEq)]
struct Ticket(i64);

impl AppValue for Ticket {
    fn kind(&self) -> &'static str {
        "a ticket"
    }
}

struct Toy {
    asked: Vec<i64>,
}

impl Host for Toy {
    type Value = Ticket;
    type Request = i64;
    fn call(
        &mut self,
        word: usize,
        args: Args<'_, Ticket>,
        out: &mut Vec<Value<Ticket>>,
    ) -> Result<Done<i64>, Failure> {
        let told = args.len();
        let args: Vec<_> = args.collect();
        assert_eq!(told, args.len(), "an exact count of the values taken");
        match (word, args.as_slice()) {
            (3, [Value::Int(n)]) => {
                if *n > 0 {
                    out.push(Value::Int(*n));
                }
                Ok(Done::Now)
            }
            (4, []) => Ok(Done::Now),
            (0, []) => {
                out.push(Value::App(Ticket(7)));
                Ok(Done::Now)
            }
            (1, [Value::Int(n)]) => {
                self.asked.push(*n);
                Ok(Done::Later(*n))
            }
            (1, [other]) => Err(Failure::new(
                "not_a_number",
                format!("ask takes an integer, not {}", other.kind()),
            )),
            _ => Err(Failure::new("refused", "no")),
        }
    }
}

fn vocabulary() -> Vocabulary {
    let mut words = Vocabulary::default();
    words.add("ticket", 0, 1..=1);
    words.add("ask", 1, 1..=1);
    words.add("fail", 0, 0..=0);
    words.add("maybe", 1, 0..=1);
    words.add("liar", 0, 1..=1);
    words
}

/// Runs `text` (a file's), answering each `ask` with ten times its
/// number, a step at a time to cover every pause; the stack it ends with.
fn run(text: &str) -> Result<Vec<Value<Ticket>>, Error> {
    let mut compiler = Compiler::new(vocabulary());
    compiler.add(&token::source("t", text)?)?;
    let mut machine = Machine::new(
        compiler.finish()?,
        Limits {
            stack: 1000,
            calls: 100,
        },
    );
    let mut host = Toy { asked: Vec::new() };
    loop {
        match machine.run(&mut host, 1)? {
            Outcome::Finished => return Ok(machine.stack().to_vec()),
            Outcome::Suspended(n) => machine.push(Value::Int(n.saturating_mul(10)))?,
            Outcome::Paused => {}
        }
    }
}

fn ints(values: &[i64]) -> Vec<Value<Ticket>> {
    values.iter().map(|n| Value::Int(*n)).collect()
}

fn code(result: Result<Vec<Value<Ticket>>, Error>) -> Option<&'static str> {
    result.err().map(|e| e.code.name())
}

#[test]
fn numbers_strings_and_words() -> Result<(), Error> {
    assert_eq!(run("3 4 add")?, ints(&[7]));
    assert_eq!(run("10 3 sub 2 mul 5 div 7 3 mod")?, ints(&[2, 1]));
    assert_eq!(run("1 2 swap over dup drop")?, ints(&[2, 1, 2]));
    assert_eq!(
        run("str 'hello world' str define")?,
        [Value::str("hello world"), Value::str("define")],
        "str takes the next word, whatever it is"
    );
    assert_eq!(
        run("1 1 eq 1 2 lt 1 2 gt not")?,
        [Value::Bool(true), Value::Bool(true), Value::Bool(true)]
    );
    assert_eq!(
        run("str a str a eq 1 str 1 eq")?,
        [Value::Bool(true), Value::Bool(false)]
    );
    Ok(())
}

#[test]
fn definitions_constants_and_recursion() -> Result<(), Error> {
    assert_eq!(run("define twice dup add end 5 twice twice")?, ints(&[20]));
    assert_eq!(run("6 constant six six six mul")?, ints(&[36]));
    // A word sees itself: recursion by name.
    let factorial = "define fact dup 1 gt if dup 1 sub fact mul then end 5 fact";
    assert_eq!(run(factorial)?, ints(&[120]));
    Ok(())
}

/// A definition that calls itself forever on one way, and uses more values
/// on another, never fails on the first: a call isn't refused for what
/// only the second needs.
#[test]
fn a_way_that_never_returns_is_counted_too() {
    let program = "define g dup 0 eq if drop 0 g else drop drop then end 0 g";
    let mut compiler = Compiler::<Ticket>::new(vocabulary());
    let compiled = token::source("t", program)
        .and_then(|tokens| compiler.add(&tokens))
        .and_then(|()| compiler.finish().map(|_| ()));
    assert_eq!(compiled, Ok(()));
    assert_eq!(
        code(run(
            "define g dup 0 eq if drop 0 g else drop drop then end 1 g"
        )),
        Some("stack_underflow")
    );
}

/// The calls under way, each at its step, as an error names them: a call
/// in last place, which took its caller's frame, still shows its caller.
#[test]
fn the_calls_under_way_show_who_called() -> Result<(), Error> {
    let mut compiler = Compiler::<Ticket>::new(vocabulary());
    compiler.add(&token::source(
        "t",
        "define inner 1 ask end\ndefine outer inner end\nouter",
    )?)?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    let outcome = machine.run(&mut Toy { asked: Vec::new() }, 100)?;
    assert_eq!(outcome, Outcome::Suspended(1));
    let calls: Vec<String> = machine
        .calls()
        .iter()
        .map(|t| format!("{}:{}", t.place, t.text))
        .collect();
    assert_eq!(
        calls,
        ["t:3:outer", "t:1:ask"],
        "outer's frame went to inner, and its call stays"
    );
    Ok(())
}

/// A call that is its definition's last step runs in its caller's frame:
/// a loop written as recursion never runs out of calls.
#[test]
fn a_call_in_last_place_reuses_its_frame() -> Result<(), Error> {
    let down = "define down dup 0 eq if else 1 sub down then end 5000 down";
    assert_eq!(run(down)?, ints(&[0]), "5000 deep, the call limit 100");
    // Last in an `if` inside an `if`: the jumps that end both lead out.
    let nested =
        "define f dup 0 gt if dup 1 gt if 1 sub f else 1 sub f then else drop then end 5000 f";
    assert_eq!(run(nested)?, ints(&[]), "both calls in last place");
    Ok(())
}

#[test]
fn control_flow_at_the_top_level_and_in_definitions() -> Result<(), Error> {
    assert_eq!(run("1 2 lt if 10 else 20 then")?, ints(&[10]));
    assert_eq!(run("2 1 lt if 10 else 20 then")?, ints(&[20]));
    assert_eq!(run("0 begin 1 add dup 5 eq until")?, ints(&[5]));
    let countdown = "define countdown begin dup 1 sub dup 0 eq until end 3 countdown";
    assert_eq!(run(countdown)?, ints(&[3, 2, 1, 0]));
    Ok(())
}

#[test]
fn an_application_word_suspends_and_resumes_deep_in_calls_and_loops() -> Result<(), Error> {
    assert_eq!(run("ticket")?, [Value::App(Ticket(7))]);
    let nested = "define each 0 begin 1 add dup ask swap dup 3 eq until drop end each";
    assert_eq!(run(nested)?, ints(&[10, 20, 30]));
    Ok(())
}

#[test]
fn arguments_are_tokens_as_given() -> Result<(), Error> {
    let mut compiler = Compiler::<Ticket>::new(vocabulary());
    compiler.add(&token::arguments(&["str", "two words", "str", "#x"]))?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    let outcome = machine.run(&mut Toy { asked: Vec::new() }, 100)?;
    assert_eq!(outcome, Outcome::Finished);
    assert_eq!(machine.stack(), [Value::str("two words"), Value::str("#x")]);
    Ok(())
}

#[test]
fn mistakes_are_found_before_anything_runs() {
    // `ask` would suspend: none of these gets that far.
    for (text, expected) in [
        ("1 ask frobnicate", "unknown_word"),
        ("1 ask if", "unclosed"),
        ("define x 1 ask", "unclosed"),
        ("1 ask then", "unmatched"),
        ("1 ask end", "unmatched"),
        ("begin 1 ask then", "unmatched"),
        ("define dup 1 end", "bad_name"),
        ("define 12 end", "bad_name"),
        ("define if end", "bad_name"),
        ("1 if define x end then", "not_top_level"),
        ("define x 1 constant y end", "not_top_level"),
        ("str", "missing_token"),
        ("'unclosed", "open_quote"),
    ] {
        let mut compiler = Compiler::<Ticket>::new(vocabulary());
        let compiled = token::source("t", text)
            .and_then(|tokens| compiler.add(&tokens))
            .and_then(|()| compiler.finish().map(|_| ()));
        assert_eq!(
            compiled.err().map(|e| e.code.name()),
            Some(expected),
            "{text}"
        );
    }
}

#[test]
fn errors_at_run_time_say_what_and_where() {
    assert_eq!(code(run("add")), Some("stack_underflow"));
    assert_eq!(code(run("1 0 div")), Some("div_by_zero"));
    assert_eq!(code(run("9223372036854775807 1 add")), Some("overflow"));
    assert_eq!(code(run("-9223372036854775808 -1 div")), Some("overflow"));
    assert_eq!(code(run("1 str a add")), Some("type"));
    assert_eq!(code(run("1 if 2 then")), Some("type"), "if takes a boolean");
    assert_eq!(code(run("define deep deep 1 end deep")), Some("call_depth"));
    assert_eq!(
        code(run("99999999999999999999")),
        Some("overflow"),
        "too big, not unknown"
    );
    assert_eq!(
        code(run("str x ask")),
        Some("not_a_number"),
        "the host's own code"
    );
    let error = run("1 2\n  frob").err();
    assert_eq!(
        error.map(|e| e.to_string()).as_deref(),
        Some("unknown_word: t:2: frob: no such word")
    );
    let error = run("define f str x add end\n1 f").err();
    assert_eq!(
        error.map(|e| e.to_string()).as_deref(),
        Some("type: t:1: add: takes two integers, and the top one is a string"),
        "found before it runs"
    );
}

#[test]
fn nothing_is_used_before_it_is_defined() {
    assert_eq!(
        code(run("define peek x end 1 constant x")),
        Some("unknown_word")
    );
    assert_eq!(
        code(run("define early later end define later 1 end")),
        Some("unknown_word")
    );
}

#[test]
fn a_budget_pauses_a_loop_that_never_asks() -> Result<(), Error> {
    let mut compiler = Compiler::<Ticket>::new(vocabulary());
    compiler.add(&token::source("t", "begin 1 1 lt until")?)?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    let mut host = Toy { asked: Vec::new() };
    for _ in 0..3 {
        assert_eq!(machine.run(&mut host, 1000)?, Outcome::Paused);
    }
    Ok(())
}

/// A backslash and newline join lines, starting no word.
#[test]
fn a_backslash_newline_joins_lines() -> Result<(), Error> {
    assert_eq!(run("1 \\\n 2 add")?, ints(&[3]));
    Ok(())
}

/// Words that take values take them in time to the values taken, not the
/// stack's depth: a deep stack fills and fails at once.
#[test]
fn a_deep_stack_costs_no_more_per_word() {
    let started = std::time::Instant::now();
    let filled = code(run("begin 1 1 0 eq until"));
    assert_eq!(filled, Some("stack_overflow"));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// What the checker says, word for word: how many values a word takes, and
/// how many the stack has, exactly or at most; which kinds are wrong; and
/// the first mistake in the source, wherever it is.
#[test]
fn the_checker_says_what_and_where() {
    let refused = |text: &str| -> String {
        let mut compiler = Compiler::<Ticket>::new(vocabulary());
        let compiled = token::source("t", text)
            .and_then(|tokens| compiler.add(&tokens))
            .and_then(|()| compiler.finish().map(|_| ()));
        compiled.err().map(|e| e.to_string()).unwrap_or_default()
    };
    for (text, said) in [
        (
            "drop",
            "stack_underflow: t:1: drop: takes 1 value, and the stack has 0 here",
        ),
        (
            "1 swap",
            "stack_underflow: t:1: swap: takes 2 values, and the stack has 1 here",
        ),
        // maybe gives one value or none: then a drop leaves none for sure.
        (
            "0 maybe drop drop",
            "stack_underflow: t:1: drop: takes 1 value, and the stack has 0 here",
        ),
        (
            "0 maybe swap",
            "stack_underflow: t:1: swap: takes 2 values, and the stack has at most 1 here",
        ),
        (
            "define f add add end\n1 f",
            "stack_underflow: t:2: f: needs at least 3 values, and the stack has 1 here",
        ),
        (
            "str a 0 0 eq add",
            "type: t:1: add: takes two integers, not a string and a boolean",
        ),
        (
            "1 0 0 eq lt",
            "type: t:1: lt: takes two integers, and the top one is a boolean",
        ),
        (
            "str a not",
            "type: t:1: not: takes a boolean (from eq, lt, gt or not), not a string",
        ),
        (
            "1 if 2 then",
            "type: t:1: if: takes a boolean (from eq, lt, gt or not), not an integer",
        ),
        // The first in the source, at the top level or in a definition.
        (
            "str a 1 add\ndefine f str b 1 sub end",
            "type: t:1: add: takes two integers, and the one under the top is a string",
        ),
        (
            "define f str b 1 sub end\nstr a 1 add",
            "type: t:1: sub: takes two integers, and the one under the top is a string",
        ),
        (
            "0 0 eq if str a 1 add then\nstr b 2 mul",
            "type: t:1: add: takes two integers, and the one under the top is a string",
        ),
    ] {
        assert_eq!(refused(text), said, "{text}");
    }
    // Arguments: the first argument's mistake.
    let mut compiler = Compiler::<Ticket>::new(vocabulary());
    let compiled = compiler
        .add(&token::arguments(&["drop", "str", "a", "1", "add"]))
        .and_then(|()| compiler.finish().map(|_| ()));
    assert_eq!(
        compiled.err().map(|e| e.to_string()).as_deref(),
        Some("stack_underflow: argument 1: drop: takes 1 value, and the stack has 0 here")
    );
}

/// What the checker can't know it never refuses: a definition that leaves
/// one value or two, called where two more are taken after; a loop at the
/// top level that leaves values, then words that take them; a value that
/// is a string or an integer, as the branch taken has it.
#[test]
fn what_can_not_be_known_is_never_refused() -> Result<(), Error> {
    let program = "define f dup 0 gt if 1 then end\n5 f drop drop";
    assert_eq!(run(program)?, ints(&[]));
    let grows = "0 begin dup 1 add dup 5 eq until add add add add add";
    assert_eq!(run(grows)?, ints(&[15]));
    let either = "0 0 eq if 1 else str a then 1 add";
    assert_eq!(run(either)?, ints(&[2]));
    Ok(())
}

/// What the checker knows lasts through a call that changes nothing, and
/// the first of two mistakes on a line is the one named.
#[test]
fn the_checker_keeps_what_it_knows() {
    let refused = |source: Vec<Token>| -> String {
        let mut compiler = Compiler::<Ticket>::new(vocabulary());
        let compiled = compiler
            .add(&source)
            .and_then(|()| compiler.finish().map(|_| ()));
        compiled.err().map(|e| e.to_string()).unwrap_or_default()
    };
    let lines = |text: &str| token::source("t", text).unwrap_or_default();
    assert_eq!(
        refused(lines("define nop end\nstr a 1 nop add")),
        "type: t:2: add: takes two integers, and the one under the top is a string"
    );
    assert_eq!(
        refused(lines("define f str a 1 add end str b 1 sub")),
        "type: t:1: add: takes two integers, and the one under the top is a string"
    );
    let words = [
        "define", "f", "str", "a", "1", "add", "end", "str", "b", "1", "sub",
    ];
    assert_eq!(
        refused(token::arguments(&words)),
        "type: argument 6: add: takes two integers, and the one under the top is a string"
    );
}

/// The machine's own errors as it runs, word for word, and where a
/// suspension is.
#[test]
fn the_machine_says_what_and_where() -> Result<(), Error> {
    let said = |text: &str| run(text).err().map(|e| e.to_string()).unwrap_or_default();
    assert_eq!(
        said("ticket 1 add"),
        "type: t:1: add: takes two integers, not a ticket and an integer"
    );
    assert_eq!(
        said("1 ticket lt"),
        "type: t:1: lt: takes two integers, not an integer and a ticket"
    );
    assert_eq!(
        said("ticket if 1 then"),
        "type: t:1: if: takes a boolean (from eq, lt, gt or not), not a ticket"
    );
    assert_eq!(
        said("0 maybe drop 0 maybe drop"),
        "stack_underflow: t:1: drop: takes 1 value, and the stack has 0"
    );
    // A host that gives what its word doesn't say is caught at once.
    assert_eq!(
        said("liar"),
        "contract: t:1: liar: the application gave 0 values, and its word says 1..=1"
    );
    let mut compiler = Compiler::<Ticket>::new(vocabulary());
    compiler.add(&token::source("t", "1\n  2 ask")?)?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    assert_eq!(
        machine.run(&mut Toy { asked: Vec::new() }, 100)?,
        Outcome::Suspended(2)
    );
    let at = machine.at().map(|t| format!("{}: {}", t.place, t.text));
    assert_eq!(at.as_deref(), Some("t:2: ask"), "the word suspended at");
    Ok(())
}

/// A word's values fill the stack to its limit, and no further.
#[test]
fn the_stack_fills_to_its_limit_exactly() -> Result<(), Error> {
    let run_with = |text: &str| -> Result<Outcome<i64>, Error> {
        let mut compiler = Compiler::<Ticket>::new(vocabulary());
        compiler.add(&token::source("t", text)?)?;
        let limits = Limits {
            stack: 2,
            calls: 10,
        };
        let mut machine = Machine::new(compiler.finish()?, limits);
        machine.run(&mut Toy { asked: Vec::new() }, 100)
    };
    assert_eq!(run_with("1 ticket")?, Outcome::Finished);
    assert_eq!(
        run_with("1 1 ticket").err().map(|e| e.code.name()),
        Some("stack_overflow")
    );
    assert_eq!(
        run_with("1 2 3").err().map(|e| e.code.name()),
        Some("stack_overflow")
    );
    Ok(())
}

/// Each built-in's effect, as help shows it, names as many values as it
/// takes.
#[test]
fn every_builtin_effect_names_what_it_takes() {
    for builtin in Builtin::ALL {
        let (before, after) = builtin.effect().split_once("--").unwrap_or_default();
        assert_eq!(
            before.split_whitespace().count(),
            builtin.takes(),
            "{}",
            builtin.name()
        );
        assert!(
            !after.trim().is_empty() || builtin == Builtin::Drop,
            "{}",
            builtin.name()
        );
    }
}

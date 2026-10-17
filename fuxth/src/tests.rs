//! The language, run as a host runs it: compiled whole, then run to its
//! end, through every suspension.
use crate::*;

/// A host with one value of its own, a ticket, and four words: `ticket`
/// (-- t), which hands one out at once; `ask` (n -- n*10), which suspends
/// for the host to answer; `nope`, which fails; and `liar`, which says it
/// gives a value and gives none.
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
            (3, []) => Ok(Done::Now),
            (0, []) => {
                out.push(Value::App(Ticket(7)));
                Ok(Done::Now)
            }
            (1, [Value::Int(n)]) => {
                self.asked.push(*n);
                Ok(Done::Later(*n))
            }
            (1, [other]) => Err(Failure::new(
                Code::Host("not_a_number"),
                format!("ask takes an integer, not {}", other.kind()),
            )),
            _ => Err(Failure::new(Code::Host("refused"), "no")),
        }
    }

    fn print(&mut self, _line: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// A compiler with the standard library in.
fn with_stdlib() -> Compiler<Ticket> {
    let mut compiler = Compiler::new(vocabulary());
    let library = token::source("stdlib.fux", STDLIB).unwrap_or_default();
    let added = compiler.add(&library);
    assert_eq!(added, Ok(()), "the standard library compiles");
    compiler
}

fn vocabulary() -> Vocabulary {
    let mut words = Vocabulary::default();
    words.add("ticket", 0, 1);
    words.add("ask", 1, 1);
    words.add("nope", 0, 0);
    words.add("liar", 0, 1);
    words
}

/// `maybe` (n -- [n]): n back only above 0, so what it leaves is known
/// only as it runs. Written on the line it is used on.
const MAYBE: &str = "define maybe dup 0 gt if else drop then end ";

/// Runs `text` (a file's), answering each `ask` with ten times its
/// number, a step at a time to cover every pause; the stack it ends with.
fn run(text: &str) -> Result<Vec<Value<Ticket>>, Error> {
    let mut compiler = Compiler::new(vocabulary());
    compiler.add(&token::source("stdlib.fux", STDLIB)?)?;
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
    assert_eq!(run("6 let six six six mul")?, ints(&[36]));
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
    let mut compiler = with_stdlib();
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
    let mut compiler = with_stdlib();
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

/// Paused anywhere in a loop, even just after it jumped back, the step it
/// is at is the loop's own.
#[test]
fn a_paused_loop_shows_its_own_step() -> Result<(), Error> {
    for budget in 1..=12 {
        let mut compiler = with_stdlib();
        compiler.add(&token::source("t", "str x print\nbegin 0 1 eq until")?)?;
        let mut machine = Machine::new(compiler.finish()?, Limits::default());
        let outcome = machine.run(&mut Toy { asked: Vec::new() }, budget)?;
        assert_eq!(outcome, Outcome::Paused);
        let last = machine
            .calls()
            .last()
            .map(|t| format!("{}:{}", t.place, t.text));
        let in_loop = ["t:2:0", "t:2:1", "t:2:eq", "t:2:until"];
        let at_print = budget <= 2;
        assert!(
            at_print || last.as_deref().is_some_and(|l| in_loop.contains(&l)),
            "{budget}: {last:?}"
        );
    }
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
    let nested = "define asks 0 begin 1 add dup ask swap dup 3 eq until drop end asks";
    assert_eq!(run(nested)?, ints(&[10, 20, 30]));
    Ok(())
}

#[test]
fn arguments_are_tokens_as_given() -> Result<(), Error> {
    let mut compiler = with_stdlib();
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
        ("define x define y end end", "not_top_level"),
        ("str", "missing_token"),
        ("'unclosed", "open_quote"),
    ] {
        let mut compiler = with_stdlib();
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
        Some("type: t:1: add: add takes an integer, not a string"),
        "found before it runs"
    );
}

#[test]
fn nothing_is_used_before_it_is_defined() {
    assert_eq!(code(run("define peek x end 1 let x")), Some("unknown_word"));
    assert_eq!(
        code(run("define early later end define later 1 end")),
        Some("unknown_word")
    );
}

#[test]
fn a_budget_pauses_a_loop_that_never_asks() -> Result<(), Error> {
    let mut compiler = with_stdlib();
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
        let mut compiler = with_stdlib();
        let compiled = token::source("t", text)
            .and_then(|tokens| compiler.add(&tokens))
            .and_then(|()| compiler.finish().map(|_| ()));
        compiled.err().map(|e| e.to_string()).unwrap_or_default()
    };
    let (maybe_drop, maybe_swap) = (
        format!("{MAYBE}0 maybe drop drop"),
        format!("{MAYBE}0 maybe swap"),
    );
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
            maybe_drop.as_str(),
            "stack_underflow: t:1: drop: takes 1 value, and the stack has 0 here",
        ),
        (
            maybe_swap.as_str(),
            "stack_underflow: t:1: swap: takes 2 values, and the stack has at most 1 here",
        ),
        (
            "define f add add end\n1 f",
            "stack_underflow: t:2: f: needs at least 3 values, and the stack has 1 here",
        ),
        (
            "str a 0 0 eq add",
            "type: t:1: add: add takes an integer, not a string",
        ),
        (
            "1 0 0 eq lt",
            "type: t:1: lt: lt compares two integers, two strings or two lists, not an integer and a boolean",
        ),
        (
            "str a not",
            "type: t:1: not: every way through it fails given a string on top: if: takes a boolean (from eq or lt), not a string",
        ),
        (
            "1 if 2 then",
            "type: t:1: if: takes a boolean (from eq or lt), not an integer",
        ),
        // The first in the source, at the top level or in a definition.
        (
            "str a 1 add\ndefine f str b 1 sub end",
            "type: t:1: add: add takes an integer, not a string",
        ),
        (
            "define f str b 1 sub end\nstr a 1 add",
            "type: t:1: sub: sub takes an integer, not a string",
        ),
        (
            "0 0 eq if str a 1 add then\nstr b 2 mul",
            "type: t:1: add: add takes an integer, not a string",
        ),
    ] {
        assert_eq!(refused(text), said, "{text}");
    }
    // Arguments: the first argument's mistake.
    let mut compiler = with_stdlib();
    let compiled = compiler
        .add(&token::arguments(&["drop", "str", "a", "1", "add"]))
        .and_then(|()| compiler.finish().map(|_| ()));
    assert_eq!(
        compiled.err().map(|e| e.to_string()).as_deref(),
        Some("stack_underflow: argument 1: drop: takes 1 value, and the stack has 0 here")
    );
}

/// A depth written past any stack, before `pick` or `roll`: the checker
/// keeps no kind for each value it reaches below the top's, so it checks at
/// once, in a definition, in a call to one, and at the top level after a
/// loop whose depth it can't bound; and it says the depth as written.
#[test]
fn a_depth_past_any_stack_is_checked_at_once() {
    let started = std::time::Instant::now();
    let far = "9223372036854775807";
    let defined = format!("define f {far} pick end define g f end define h {far} roll end");
    assert_eq!(code(run(&defined)), None);
    assert_eq!(
        code(run(&format!("{defined} 1 g"))),
        Some("stack_underflow")
    );
    assert_eq!(
        code(run(&format!("{defined} 1 h"))),
        Some("stack_underflow")
    );
    let grown = format!("0 begin dup 1 add dup 5 eq until {far} pick");
    assert_eq!(code(run(&grown)), Some("stack_underflow"));
    let deep = "define f 100000000 pick end define g 100000000 roll end";
    assert_eq!(code(run(deep)), None);
    let said = run(&format!("1 {far} roll")).err().map(|e| e.to_string());
    assert_eq!(
        said.as_deref(),
        Some(
            "stack_underflow: t:1: roll: takes 9223372036854775808 values, and the stack has 1 here"
        )
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// `each` finds each character of a string at once, whatever the
/// characters are: walking a string of non-ASCII ones takes about as long
/// as walking one as long of ASCII ones.
#[test]
fn each_walks_any_string_in_time_to_its_length() -> Result<(), Error> {
    let walk = |c: &str| -> Result<std::time::Duration, Error> {
        let mut compiler = with_stdlib();
        compiler.add(&token::source("t", "0 ask 0 swap each drop 1 add end")?)?;
        let mut machine = Machine::new(compiler.finish()?, Limits::default());
        let mut host = Toy { asked: Vec::new() };
        assert_eq!(machine.run(&mut host, u64::MAX)?, Outcome::Suspended(0));
        machine.push(Value::str(
            std::iter::repeat_n(c, 100_000).collect::<String>(),
        ))?;
        let started = std::time::Instant::now();
        assert_eq!(machine.run(&mut host, u64::MAX)?, Outcome::Finished);
        assert_eq!(machine.stack(), ints(&[100_000]).as_slice());
        Ok(started.elapsed())
    };
    let ascii = walk("e")?;
    let other = walk("é")?;
    assert!(
        other < ascii * 5 + std::time::Duration::from_millis(100),
        "{other:?} against {ascii:?}"
    );
    Ok(())
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
        let mut compiler = with_stdlib();
        let compiled = compiler
            .add(&source)
            .and_then(|()| compiler.finish().map(|_| ()));
        compiled.err().map(|e| e.to_string()).unwrap_or_default()
    };
    let lines = |text: &str| token::source("t", text).unwrap_or_default();
    assert_eq!(
        refused(lines("define nop end\nstr a 1 nop add")),
        "type: t:2: add: add takes an integer, not a string"
    );
    assert_eq!(
        refused(lines("define f str a 1 add end str b 1 sub")),
        "type: t:1: add: add takes an integer, not a string"
    );
    let words = [
        "define", "f", "str", "a", "1", "add", "end", "str", "b", "1", "sub",
    ];
    assert_eq!(
        refused(token::arguments(&words)),
        "type: argument 6: add: add takes an integer, not a string"
    );
}

/// The machine's own errors as it runs, word for word, and where a
/// suspension is.
#[test]
fn the_machine_says_what_and_where() -> Result<(), Error> {
    let said = |text: &str| run(text).err().map(|e| e.to_string()).unwrap_or_default();
    assert_eq!(
        said("ticket 1 add"),
        "type: t:1: add: add takes an integer, not a ticket"
    );
    assert_eq!(
        said("1 ticket lt"),
        "type: t:1: lt: lt compares two integers, two strings or two lists, not an integer and a ticket"
    );
    assert_eq!(
        said("ticket if 1 then"),
        "type: t:1: if: takes a boolean (from eq or lt), not a ticket"
    );
    assert_eq!(
        said(&format!("{MAYBE}0 maybe drop 0 maybe drop")),
        "stack_underflow: t:1: drop: takes 1 value, and the stack has 0"
    );
    // Too few, as the word takes them, not as one of its pops does.
    for (text, takes, has) in [
        ("str a 0 maybe at", 2, 1),
        ("str a 1 0 maybe slice", 3, 2),
        ("str a 0 maybe push", 2, 1),
        ("str a 1 0 maybe put", 3, 2),
    ] {
        let word = text.rsplit(' ').next().unwrap_or_default();
        assert_eq!(
            said(&format!("{MAYBE}{text}")),
            format!("stack_underflow: t:1: {word}: takes {takes} values, and the stack has {has}")
        );
    }
    // A host that gives what its word doesn't say is caught at once.
    assert_eq!(
        said("liar"),
        "contract: t:1: liar: the application gave 0 values, and its word says 1"
    );
    let mut compiler = with_stdlib();
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
        let mut compiler = with_stdlib();
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
        // pick and roll take a count, then as many values as it says.
        if matches!(builtin, Builtin::Pick | Builtin::Roll) {
            continue;
        }
        let (before, after) = builtin.effect().split_once("--").unwrap_or_default();
        assert_eq!(
            before.split_whitespace().count(),
            builtin.takes(),
            "{}",
            builtin.name()
        );
        assert!(
            !after.trim().is_empty()
                || matches!(builtin, Builtin::Drop | Builtin::Print | Builtin::Fail),
            "{}",
            builtin.name()
        );
    }
}

/// The core's sequence and character words, at their edges: indexes just
/// past the end, strings of more than ASCII, lists that share a start,
/// counts known only as the program runs.
#[test]
fn the_core_at_its_edges() -> Result<(), Error> {
    let s = |text: &str| Value::<Ticket>::str(text);
    assert_eq!(code(run("str abc 3 at")), Some("range"));
    assert_eq!(code(run("list 1 end 1 at")), Some("range"));
    assert_eq!(
        run("str héllo 1 at str héllo 1 4 slice str héllo len")?,
        [s("é"), s("éll"), Value::Int(5)]
    );
    assert_eq!(run("str naïve 2 str i put")?, [s("naive")]);
    assert_eq!(run("str abc 1 str XY put")?, [s("aXYc")]);
    assert_eq!(run("str é code 233 char")?, [Value::Int(233), s("é")]);
    let truth = |b: bool| Value::<Ticket>::Bool(b);
    assert_eq!(
        run("list 1 2 end list 1 3 end lt list 1 2 end list 1 2 3 end lt")?,
        [truth(true), truth(true)]
    );
    assert_eq!(code(run("list 1 end list str a end lt")), Some("type"));
    // Counts known only as it runs.
    assert_eq!(
        run("1 2 3 4 0 pick drop 2 0 pick drop pick")?,
        ints(&[1, 2, 3, 4, 2])
    );
    assert_eq!(
        run("1 2 3 4 0 pick drop 2 0 pick drop roll")?,
        ints(&[1, 3, 4, 2])
    );
    assert_eq!(code(run("1 2 -1 0 pick drop pick")), Some("range"));
    assert_eq!(code(run("1 2 str x 0 pick drop pick")), Some("type"));
    Ok(())
}

/// What is built has limits: a value's length, and how deep lists nest.
#[test]
fn what_is_built_is_bounded() {
    let long = || -> Result<Outcome<i64>, Error> {
        let mut compiler = with_stdlib();
        let text = "str x 1000 copies let s list 0 1001 range each drop s end end concat";
        compiler.add(&token::source("t", text)?)?;
        let mut machine = Machine::new(compiler.finish()?, Limits::default());
        machine.run(&mut Toy { asked: Vec::new() }, u64::MAX)
    };
    assert_eq!(long().err().map(|e| e.code.name()), Some("too_long"));
    let deep = "list end let l 0 begin list l end let l 1 add dup 70 eq until";
    assert_eq!(code(run(deep)), Some("too_long"));
    let fine = "list end let l 0 begin list l end let l 1 add dup 60 eq until";
    assert_eq!(code(run(fine)), None);
}

/// `let` may take a library or host word's name for itself, not the
/// core's; a definition may not take a `let` name's.
#[test]
fn let_names_and_what_they_may_hide() -> Result<(), Error> {
    assert_eq!(
        run("list 1 2 end let first first")?.len(),
        1,
        "a library word hidden"
    );
    assert_eq!(
        run("5 let ticket ticket")?,
        ints(&[5]),
        "a host word hidden"
    );
    assert_eq!(code(run("1 let drop")), Some("bad_name"));
    assert_eq!(code(run("1 let x define x 2 end")), Some("bad_name"));
    assert_eq!(code(run("define f 1 end 2 let f f")), None);
    Ok(())
}

/// The checker knows a string's elements are strings.
#[test]
fn a_strings_elements_are_strings_to_the_checker() {
    let mut compiler = with_stdlib();
    let compiled = token::source("t", "str abc 0 at 1 add")
        .and_then(|tokens| compiler.add(&tokens))
        .and_then(|()| compiler.finish().map(|_| ()));
    assert_eq!(compiled.err().map(|e| e.code.name()), Some("type"));
    let mut compiler = with_stdlib();
    let compiled = token::source("t", "str abc str b at")
        .and_then(|tokens| compiler.add(&tokens))
        .and_then(|()| compiler.finish().map(|_| ()));
    assert_eq!(
        compiled.err().map(|e| e.code.name()),
        Some("type"),
        "an index is an integer"
    );
}

/// A library checked once serves every program compiled from a clone, and
/// its mistakes are still found.
#[test]
fn a_prepared_library_serves_every_clone() -> Result<(), Error> {
    let mut ready = Compiler::<Ticket>::new(vocabulary());
    ready.add(&token::source(
        "lib",
        "define twice 2 mul end define bad str a 1 add end",
    )?)?;
    ready.prepare();
    for _ in 0..2 {
        let mut compiler = ready.clone();
        compiler.add(&token::source("t", "3 twice")?)?;
        let error = compiler.finish().err().map(|e| e.to_string());
        assert_eq!(
            error.as_deref(),
            Some("type: lib:1: add: add takes an integer, not a string")
        );
    }
    let mut open = Compiler::<Ticket>::new(vocabulary());
    open.add(&token::source("lib", "define half")?)?;
    open.prepare();
    open.add(&token::source("lib", "2 div end 8 half")?)?;
    assert!(
        open.finish().is_ok(),
        "nothing is checked while a definition is open"
    );
    Ok(())
}

/// Every core word says what it does, for help.
#[test]
fn every_core_word_says_what_it_does() {
    for builtin in Builtin::ALL {
        assert!(builtin.about().len() > 8, "{}", builtin.name());
    }
}

/// push and put on lists, in place or copied, within the nesting limit;
/// slice's bounds; a host value's kind and text.
#[test]
fn lists_change_and_host_values_show() -> Result<(), Error> {
    let list = |items: Vec<Value<Ticket>>| {
        crate::List::new(items).map(|l| Value::List(std::rc::Rc::new(l)))
    };
    assert_eq!(
        run("list 1 end 2 push")?,
        list(ints(&[1, 2])).into_iter().collect::<Vec<_>>()
    );
    let both: Vec<_> = list(ints(&[1, 2]))
        .into_iter()
        .chain(list(ints(&[9, 2])))
        .collect();
    assert_eq!(run("list 1 2 end dup 0 9 put")?, both);
    assert_eq!(code(run("str abc 2 1 slice")), Some("range"));
    assert_eq!(code(run("str abc 0 4 slice")), Some("range"));
    let deep = "list end let l 0 begin list end l push let l 1 add dup 70 eq until";
    assert_eq!(code(run(deep)), Some("too_long"), "push nests too");
    let deep_put = "list 0 end let l 0 begin list 0 end 0 l put let l 1 add dup 70 eq until";
    assert_eq!(code(run(deep_put)), Some("too_long"), "put nests too");
    assert_eq!(
        run("ticket kind ticket string")?,
        [Value::str("ticket"), Value::str("a ticket")]
    );
    Ok(())
}

/// A definition that, some ways through, replaces a value below what it
/// surely uses: its callers forget what they knew of that value's kind.
/// (Found by a Codex test run; it was refused before.)
#[test]
fn a_value_replaced_below_what_is_surely_used_is_forgotten() -> Result<(), Error> {
    let replace = "define replace each swap drop end end str a list 1 2 end replace 1 add";
    assert_eq!(run(replace)?, ints(&[3]));
    let swapped = "define f each drop drop 1 end end str a list 0 end f 1 add";
    assert_eq!(run(swapped)?, ints(&[2]));
    Ok(())
}

/// What the Codex QA runs found, each a test that failed before its fix.
#[test]
fn what_qa_found() -> Result<(), Error> {
    let refused = |text: &str| -> Option<String> {
        let mut compiler = with_stdlib();
        let compiled = token::source("t", text)
            .and_then(|tokens| compiler.add(&tokens))
            .and_then(|()| compiler.finish().map(|_| ()));
        compiled.err().map(|e| e.code.name().to_owned())
    };
    // Refused before anything runs: kinds through library calls, pairs of
    // kinds, push and put of a non-string into a string, `mod` by -1.
    for text in [
        "str m print 1 sum",
        "str m print 42 last",
        "str m print 5 sort",
        "str m print str x 0 gt",
        "str m print str a str 3 copies",
        "str m print str a 1 push",
        "str m print str a 0 1 put",
        "str m print str x -1 mod",
        "str m print true negate",
        "define id let x x end str m print str a id 1 add",
    ] {
        assert_eq!(refused(text).as_deref(), Some("type"), "{text}");
    }
    // A definition every way through which ends in `fail` still needs its
    // values; a way ending in `fail` counts as failing, so what the ways
    // that go on need is needed.
    assert_eq!(
        refused("define f fail end str m print f").as_deref(),
        Some("stack_underflow")
    );
    assert_eq!(
        refused("define f if str x fail else drop drop then end true f").as_deref(),
        Some("stack_underflow")
    );
    assert_eq!(
        refused("define f if str x fail else drop drop then end 1 2 true f"),
        None
    );
    // A call to a word that only fails is as `fail`; too few values are
    // named before a kind.
    assert_eq!(
        refused("define stop str s fail end define f if stop else 1 add then end str a true f")
            .as_deref(),
        Some("type")
    );
    assert_eq!(refused("str x 5 pad").as_deref(), Some("stack_underflow"));
    // A string put into, held nowhere else, changes in place; held
    // elsewhere, it is copied.
    assert_eq!(
        run("str x 1000 copies 0 let i begin 0 str y put i 1 add let i i 17000 eq until len")?,
        ints(&[1000])
    );
    assert_eq!(
        run("str héllo let s s 1 str E put s 0 str ab put")?,
        [Value::str("hEllo"), Value::str("abéllo")]
    );
    // A way leaving what was given below where the stack surely reaches
    // doesn't make the caller's stack shorter than it is.
    assert_eq!(
        run("define f if else drop 7 then end true f 5")?,
        ints(&[5])
    );
    // `and` and `or` take two booleans.
    assert_eq!(refused("1 true and").as_deref(), Some("type"));
    assert_eq!(refused("true or").as_deref(), Some("stack_underflow"));
    // A text that fits is made, however its value is nested.
    assert_eq!(
        run("str x 1000 copies 1000 copies let s list s end string len")?,
        ints(&[1_000_000])
    );
    // A loop that grows the stack still needs its first round's values.
    assert_eq!(
        refused("define f begin dup true until end str m print f").as_deref(),
        Some("stack_underflow")
    );
    // A call to itself inside list … end needs what the definition needs.
    assert_eq!(
        refused("define d dup drop list d end end str m print d").as_deref(),
        Some("stack_underflow")
    );
    // Right answers: eq after put, a list's depth after put, mod by -1.
    assert_eq!(
        run("list list 0 end end 0 7 put list 7 end eq")?,
        [Value::Bool(true)]
    );
    assert_eq!(run("-9223372036854775808 -1 mod 7 -1 mod")?, ints(&[0, 0]));
    assert_eq!(run("str x 5 str ab pad len")?, ints(&[5]));
    // A way ending at `fail` counts as failing, unless every way does.
    assert_eq!(
        refused("str m print 5 str , split-on").as_deref(),
        Some("type")
    );
    assert_eq!(
        refused("define f if str boom fail else 1 add then end str a true f").as_deref(),
        Some("type")
    );
    assert_eq!(
        refused("define die str x fail end false if die then 1"),
        None
    );
    // All the calls allowed (100 here) may nest, the top level aside.
    let deep = "define f dup 0 eq if else 1 sub f 0 drop then end";
    assert_eq!(run(&format!("{deep} 99 f 0"))?, ints(&[0, 0]));
    assert_eq!(code(run(&format!("{deep} 100 f 0"))), Some("call_depth"));
    // Built to the longest a value may be, not held on the stack first;
    // a string pushed onto, held nowhere else, grows in place.
    assert_eq!(
        run("0 1000000 range len str a 1000000 copies len")?,
        ints(&[1_000_000, 1_000_000])
    );
    assert_eq!(
        run("str ab let s s str c push s")?,
        [Value::str("abc"), Value::str("ab")]
    );
    // Too long a text is refused with how long a string may be.
    let wide = run("list 0 600000 range end string");
    assert!(wide.is_err_and(|e| e.message.contains("1000000") && !e.message.contains("2000000")));
    Ok(())
}

/// A list held many times over is as big as all its copies: past the
/// total allowed, it is refused as it is built, so no walk over it (eq,
/// lt, string) can run long within a step.
#[test]
fn a_list_held_many_times_is_counted_whole() {
    let doubling =
        "list 1 2 3 4 5 6 7 8 end let x 0 begin list x x end let x 1 add dup 40 eq until";
    assert_eq!(code(run(doubling)), Some("too_long"));
    let fine = "list 1 2 end let x 0 begin list x x end let x 1 add dup 10 eq until x x eq";
    assert_eq!(code(run(fine)), None);
}

/// Cases mutation testing showed no test held: string order, a non-ASCII
/// string grown and then indexed, copies of shared strings charged, and a
/// list's depth after `put` makes it shallower.
#[test]
fn what_mutation_testing_found() -> Result<(), Error> {
    assert_eq!(
        run("str a str b lt str b str a lt str ab str a lt")?,
        [Value::Bool(true), Value::Bool(false), Value::Bool(false)]
    );
    assert_eq!(
        run("str é str ab push dup 2 at swap 0 at")?,
        [Value::str("b"), Value::str("é")]
    );
    let shared = "str x 1000 copies let s 0 begin s str y push drop 1 add dup 17000 eq until";
    assert_eq!(code(run(shared)), Some("too_long"));
    let shared = "str x 1000 copies let s 0 begin s 0 str y put drop 1 add dup 17000 eq until";
    assert_eq!(code(run(shared)), Some("too_long"));
    let shared = "0 1000 range let s 0 begin s 7 push drop 1 add dup 17000 eq until";
    assert_eq!(code(run(shared)), Some("too_long"));
    let shared = "0 1000 range let s 0 begin s 0 7 put drop 1 add dup 17000 eq until";
    assert_eq!(code(run(shared)), Some("too_long"));
    // Held nowhere else, a list is put into in place, free of copies.
    let alone = "0 1000 range 0 begin swap 0 7 put swap 1 add dup 17000 eq until drop len";
    assert_eq!(run(alone)?, ints(&[1000]));
    // A string put may make a string exactly as long as one may be.
    assert_eq!(
        run("str x 1000000 copies 0 str y put len")?,
        ints(&[1_000_000])
    );
    let shallower = "list list list 1 end end end 0 5 put let x \
        0 begin list x end let x 1 add dup 63 eq until drop x len";
    assert_eq!(run(shallower)?, ints(&[1]));
    Ok(())
}

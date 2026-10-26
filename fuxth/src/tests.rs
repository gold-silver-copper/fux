//! The language, run as a host runs it: compiled whole, then run to its
//! end, through every suspension.
use crate::*;

/// A host with one value of its own, a ticket, and four words: `ticket`
/// (-- t), which hands one out at once; `ask` (n -- n*10), which suspends
/// for the host to answer (with a failure, `negative`, for n below 0);
/// `nope`, which fails with `unused`, a code of the language's it declares
/// its words raise; and `liar`, which says it gives a value and gives
/// none.
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
            (2, []) => Err(Failure::new(Code::Unused, "nothing uses it")),
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
    let added = (words.kind("ticket", "a ticket"))
        .and_then(|()| words.add("ticket", "-- ticket"))
        .and_then(|_| words.add("ask", "any -- any"))
        .and_then(|_| words.add("nope", "--"))
        .and_then(|_| words.add("liar", "-- any"))
        .and_then(|_| words.raises(Code::Host("not_a_number")))
        .and_then(|()| words.raises(Code::Host("refused")))
        .and_then(|()| words.raises(Code::Host("negative")))
        .and_then(|()| words.raises(Code::Unused));
    assert!(added.is_ok(), "{added:?}");
    words
}

/// `maybe` (n -- [n]): n back only above 0, so what it leaves is known
/// only as it runs. Written on the line it is used on.
const MAYBE: &str = "define maybe dup 0 gt if else drop then end ";

/// Runs `text` (a file's), answering each `ask` with ten times its
/// number, a step at a time to cover every pause; the stack it ends with.
fn run(text: &str) -> Result<Vec<Value<Ticket>>, Error> {
    run_by(text, 1)
}

/// `run`, `budget` steps at a time.
fn run_by(text: &str, budget: u64) -> Result<Vec<Value<Ticket>>, Error> {
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
        match machine.run(&mut host, budget)? {
            Outcome::Finished => return Ok(machine.stack().to_vec()),
            Outcome::Suspended(n) if n < 0 => machine.raise(Failure::new(
                Code::Host("negative"),
                format!("{n} is below 0"),
            ))?,
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
        ("define drop 1 end", "bad_name"),
        ("define x 1 end define x 2 end", "bad_name"),
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
        Some("type: t:1: add: add takes an integer, not a string on top"),
        "found before it runs"
    );
}

/// A definition may call one written after it, in the same source; a
/// `let` name is read only after it, and a word no one defines fails
/// before anything runs.
#[test]
fn a_definition_may_call_one_written_after_it() -> Result<(), Error> {
    assert_eq!(code(run("define peek x end 1 let x")), Some("unknown_word"));
    assert_eq!(
        run("define early later end define later 1 end early")?,
        ints(&[1])
    );
    assert_eq!(
        code(run("define early never end 1 ask")),
        Some("unknown_word")
    );
    assert_eq!(code(run("early define early 1 end")), Some("unknown_word"));
    Ok(())
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
            "type: t:1: add: add takes an integer, not a string under the top",
        ),
        // Of several places, the one refused, by how far under the top.
        (
            "str a str b 1 slice",
            "type: t:1: slice: slice takes an integer, not a string under the top",
        ),
        (
            "1 2 3 slice",
            "type: t:1: slice: slice takes a string or a list, not an integer 2 under the top",
        ),
        (
            "1 0 0 eq lt",
            "type: t:1: lt: lt compares two integers, two strings or two lists, not an integer and a boolean",
        ),
        (
            "1 if 2 then",
            "type: t:1: if: takes a boolean (from eq or lt), not an integer",
        ),
        // The first in the source, at the top level or in a definition.
        (
            "str a 1 add\ndefine f str b 1 sub end",
            "type: t:1: add: add takes an integer, not a string under the top",
        ),
        (
            "define f str b 1 sub end\nstr a 1 add",
            "type: t:1: sub: sub takes an integer, not a string under the top",
        ),
        (
            "0 0 eq if str a 1 add then\nstr b 2 mul",
            "type: t:1: add: add takes an integer, not a string under the top",
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

/// A character of a string is found from the mark before it: each part
/// of a string grown from ASCII and non-ASCII pieces, as it grows and once
/// a character is replaced, is what its characters give; and marking an
/// ASCII string the first time it holds more is charged as a walk over it.
#[test]
fn a_strings_characters_are_found_from_its_marks() -> Result<(), Error> {
    let copies = |s: &str, n| std::iter::repeat_n(s, n).collect::<String>();
    // 192 characters, then pieces starting on a mark, of 4 and 3 bytes.
    let pieces = [
        copies("a", 70),
        "é".into(),
        copies("b", 121),
        copies("𝄞", 64),
        copies("日本", 40),
    ];
    let check = |text: &Text| {
        let chars: Vec<char> = text.chars().collect();
        for from in 0..=chars.len() {
            for to in from..=chars.len().saturating_add(70) {
                let part: String = chars
                    .iter()
                    .skip(from)
                    .take(to.saturating_sub(from))
                    .collect();
                assert_eq!(text.part(from, to), part, "{from} {to}");
            }
        }
    };
    let mut grown = Text::new("");
    for piece in &pieces {
        grown.append(&Text::new(piece.as_str()));
        check(&grown);
    }
    let mut put = Text::new(pieces.concat());
    put.replace(100, &Text::new("ü"));
    check(&put);
    let steps = |text: &str| -> Result<u64, Error> {
        let mut compiler = with_stdlib();
        compiler.add(&token::source("t", text)?)?;
        let mut machine = Machine::new(compiler.finish()?, Limits::default());
        machine.run(&mut Toy { asked: Vec::new() }, u64::MAX)?;
        Ok(machine.steps())
    };
    let (ascii, marked) = (
        steps("str a 6400 copies str b push len")?,
        steps("str a 6400 copies str é push len")?,
    );
    assert!(
        marked.saturating_sub(ascii) >= 6400 / 64,
        "{ascii} {marked}"
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
        "type: t:2: add: add takes an integer, not a string under the top"
    );
    assert_eq!(
        refused(lines("define f str a 1 add end str b 1 sub")),
        "type: t:1: add: add takes an integer, not a string under the top"
    );
    let words = [
        "define", "f", "str", "a", "1", "add", "end", "str", "b", "1", "sub",
    ];
    assert_eq!(
        refused(token::arguments(&words)),
        "type: argument 6: add: add takes an integer, not a string under the top"
    );
}

/// The machine's own errors as it runs, word for word, and where a
/// suspension is.
#[test]
fn the_machine_says_what_and_where() -> Result<(), Error> {
    let said = |text: &str| run(text).err().map(|e| e.to_string()).unwrap_or_default();
    assert_eq!(
        said("ticket 1 add"),
        "type: t:1: add: add takes an integer, not a ticket under the top"
    );
    assert_eq!(
        said("1 ticket lt"),
        "type: t:1: lt: lt compares two integers, two strings or two lists, not an integer and a ticket; `string` makes it text"
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
            builtin.args().len(),
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
            Some("type: lib:1: add: add takes an integer, not a string under the top")
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
#[test]
fn a_value_replaced_below_what_is_surely_used_is_forgotten() -> Result<(), Error> {
    let replace = "define replace each swap drop end end str a list 1 2 end replace 1 add";
    assert_eq!(run(replace)?, ints(&[3]));
    let swapped = "define f each drop drop 1 end end str a list 0 end f 1 add";
    assert_eq!(run(swapped)?, ints(&[2]));
    Ok(())
}

/// A long chain of definitions, each calling the one before with a value
/// of a known kind, is checked without recursing once per call; and a
/// string put into is charged for the string it makes anew.
#[test]
fn long_chains_and_long_strings_are_bounded() -> Result<(), Error> {
    let chain: String = (1..20_000)
        .map(|i: u32| format!("define f{i} f{} end\n", i.saturating_sub(1)))
        .collect();
    let chained = format!("define f0 1 add end\n{chain}5 f19999");
    assert_eq!(run(&chained)?, ints(&[6]));
    let steps = |text: &str| -> Result<u64, Error> {
        let mut compiler = with_stdlib();
        compiler.add(&token::source("t", text)?)?;
        let mut machine = Machine::new(compiler.finish()?, Limits::default());
        machine.run(&mut Toy { asked: Vec::new() }, u64::MAX)?;
        Ok(machine.steps())
    };
    let (made, put) = (
        steps("str é 100000 copies len")?,
        steps("str é 100000 copies 0 str y put len")?,
    );
    assert!(put.saturating_sub(made) > 100_000 / 64, "{made} {put}");
    Ok(())
}

/// Mistakes the checker once made, each a test that failed before its fix.
#[test]
fn what_the_checker_once_got_wrong() -> Result<(), Error> {
    let refused = |text: &str| -> Option<String> {
        let mut compiler = with_stdlib();
        let compiled = token::source("t", text)
            .and_then(|tokens| compiler.add(&tokens))
            .and_then(|()| compiler.finish().map(|_| ()));
        compiled.err().map(|e| e.code.name().to_owned())
    };
    // Refused before anything runs: push and put of a non-string into a
    // string.
    for text in ["str m print str a 1 push", "str m print str a 0 1 put"] {
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
    // Too few values are named before a kind.
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
    // A way ending at `fail` fails only that way.
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

/// A host with kinds of its own: a ticket, and a draft (a ticket marked,
/// which must be issued). `ticket` (-- ticket); `mark` (x -- x: a ticket
/// becomes a draft, a draft stays one); `issue` (draft -- ticket); `later`
/// (-- ticket), answered when the machine resumes.
#[derive(Clone, Debug, PartialEq)]
enum Paper {
    Ticket,
    Draft,
    Stamped,
}

impl AppValue for Paper {
    fn kind(&self) -> &'static str {
        match self {
            Paper::Ticket => "a ticket",
            Paper::Draft => "a draft",
            Paper::Stamped => "a stamped ticket",
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Paper::Stamped => "stamped",
            Paper::Ticket | Paper::Draft => self.kind().trim_start_matches("a "),
        }
    }
}

/// The desk: how often it was called; and, if `lying`, `mark` gives a
/// ticket, not the draft it says.
struct Desk {
    calls: usize,
    lying: bool,
}

impl Host for Desk {
    type Value = Paper;
    type Request = ();
    fn call(
        &mut self,
        word: usize,
        args: Args<'_, Paper>,
        out: &mut Vec<Value<Paper>>,
    ) -> Result<Done<()>, Failure> {
        drop(args);
        self.calls = self.calls.saturating_add(1);
        let given = match (word, self.lying) {
            (1, false) => Paper::Draft,
            (3, _) => return Ok(Done::Later(())),
            (4, _) => Paper::Stamped,
            _ => Paper::Ticket,
        };
        out.push(Value::App(given));
        Ok(Done::Now)
    }

    fn print(&mut self, _line: &str) -> Result<(), Failure> {
        Ok(())
    }
}

fn desk_words() -> Result<Vocabulary, Error> {
    let mut words = Vocabulary::default();
    words.kind("ticket", "a ticket")?;
    words.kind("draft", "a draft")?;
    words.must_use("draft", "a draft nothing issues: end it with issue")?;
    words.name("x", "draft ticket>draft", "a ticket or a draft")?;
    words.add("ticket", "-- ticket")?;
    words.add("mark", "x -- x")?;
    words.add("issue", "draft -- ticket")?;
    words.add("later", "-- ticket")?;
    words.kind("stamped", "a stamped ticket")?;
    words.add("stamp", "ticket -- stamped")?;
    Ok(words)
}

/// `text` at the desk, a later word answered with `answer`: what it left,
/// or its error; and how often the desk was called.
fn at_desk(text: &str, lying: bool, answer: Paper) -> (Result<Vec<Value<Paper>>, Error>, usize) {
    let mut desk = Desk { calls: 0, lying };
    let ran = (|| {
        let mut compiler = Compiler::new(desk_words()?);
        compiler.add(&token::source("stdlib.fux", STDLIB)?)?;
        compiler.add(&token::source("t", text)?)?;
        let mut machine = Machine::new(compiler.finish()?, Limits::default());
        loop {
            match machine.run(&mut desk, 1)? {
                Outcome::Finished => return Ok(machine.stack().to_vec()),
                Outcome::Suspended(()) => machine.push(Value::App(answer.clone()))?,
                Outcome::Paused => {}
            }
        }
    })();
    (ran, desk.calls)
}

/// The checker knows the application's kinds from its words' effects: a
/// value of a kind a word never takes, or one that must be used left at
/// the end, is refused before the application is called once.
#[test]
fn the_applications_kinds_are_checked_before_it_is_called() {
    let said = |text: &str| {
        let (ran, calls) = at_desk(text, false, Paper::Ticket);
        (ran.err().map(|e| e.to_string()), calls)
    };
    assert_eq!(
        at_desk("ticket mark mark issue", false, Paper::Ticket).0,
        Ok(vec![Value::App(Paper::Ticket)])
    );
    assert_eq!(
        said("ticket ticket issue"),
        (
            Some("type: t:1: issue: takes a draft, not a ticket".to_owned()),
            0
        )
    );
    assert_eq!(
        said("ticket 1 mark"),
        (
            Some("type: t:1: mark: takes a ticket or a draft, not an integer".to_owned()),
            0
        )
    );
    assert_eq!(
        said("ticket mark 1 2 add"),
        (
            Some("unused: t:1: mark: a draft nothing issues: end it with issue".to_owned()),
            0
        )
    );
    // A word that changed the kind is no pass: the value's maker is not
    // named.
    assert_eq!(
        said("ticket mark stamp").0,
        Some("type: t:1: stamp: takes a ticket, not a draft".to_owned())
    );
    // Through a definition, and a kind passed through a word.
    assert_eq!(said("define go ticket issue end go").1, 0);
    assert_eq!(said("define go mark mark end ticket go issue").0, None);
    // What its kinds can't show (a list's element) passes, for the
    // application to check as it runs; one left that must be used is
    // found as the program ends.
    let (ran, calls) = at_desk("list ticket end 0 at issue", false, Paper::Ticket);
    assert_eq!((ran.map(|v| v.len()), calls), (Ok(1), 2));
    let (ran, _) = at_desk("list ticket mark end 0 at", false, Paper::Ticket);
    assert_eq!(
        ran.map_err(|e| e.to_string()),
        Err("unused: t:1: at: a draft nothing issues: end it with issue".to_owned())
    );
}

/// An application that gives a kind its word doesn't say is caught where
/// it gives it: as the word returns, or as the machine resumes.
#[test]
fn a_kind_given_is_held_to_the_word() {
    let (ran, _) = at_desk("ticket mark issue", true, Paper::Ticket);
    assert_eq!(
        ran.map_err(|e| e.to_string()),
        Err(
            "contract: t:1: mark: the application gave a ticket, and its word says a draft"
                .to_owned()
        )
    );
    let (ran, _) = at_desk("later", false, Paper::Draft);
    assert_eq!(
        ran.map_err(|e| e.to_string()),
        Err(
            "contract: t:1: later: the application gave a draft, and its word says a ticket"
                .to_owned()
        )
    );
    // An effect is read strictly: a name no kind has is refused.
    let mut words = Vocabulary::default();
    assert_eq!(
        words.add("odd", "pane -- ").map_err(|e| e.code.name()),
        Err("contract")
    );
    assert_eq!(
        words.add("odd", "pane").map_err(|e| e.code.name()),
        Err("contract")
    );
    // Each name means one thing: a second meaning is refused, whichever
    // came first.
    let made = words
        .kind("pane", "a pane")
        .and_then(|()| words.add("w", "--"));
    assert_eq!(made.map_err(|e| e.message), Ok(0));
    let again = [
        words.kind("pane", "a pane"),
        words.name("pane", "integer", "an integer"),
        words.kind("string", "a string"),
        words.name("any", "integer", "an integer"),
        words.add("w", "--").map(drop),
        words.add("drop", "--").map(drop),
        words.add("if", "--").map(drop),
        words.raises(Code::Host("refused")),
        words.raises(Code::Host("refused")),
        words.taken_as("pane", "string"),
    ];
    let mut kinds = Vocabulary::default();
    let made = kinds
        .kind("a", "an a")
        .and_then(|()| kinds.taken_as("a", "string"));
    assert_eq!(made.map_err(|e| e.message), Ok(()));
    for again in [("a", "string"), ("a", "a")] {
        let said = kinds.taken_as(again.0, again.1).map_err(|e| e.message);
        assert_eq!(said, Err("a: taken as that already".to_owned()));
    }
    let said: Vec<String> = again
        .into_iter()
        .filter_map(Result::err)
        .map(|e| e.message)
        .collect();
    assert_eq!(
        said,
        [
            "pane: a kind or name already",
            "pane: a kind or name already",
            "string: a kind or name already",
            "any: a kind or name already",
            "w: a word already",
            "drop: a word of the core",
            "if: a word of the language",
            "refused: declared already",
            "pane: taken as more after an effect named it",
        ]
    );
}

/// A library says what each of its words does on the line just above it,
/// which is its help, with its code as written; a word saying nothing
/// there is refused.
#[test]
fn a_library_says_what_each_word_does() {
    let mut compiler = Compiler::<Ticket>::new(vocabulary());
    let docs = compiler.library("stdlib.fux", STDLIB).unwrap_or_default();
    let defined = STDLIB.lines().filter(|l| l.starts_with("define ")).count();
    assert_eq!(docs.len(), defined);
    let doc = |name: &str| docs.iter().find(|d| d.name == name).cloned();
    let dup = doc("dup").map(|d| (d.effect, d.about, d.code));
    let dup = dup.unwrap_or_default();
    assert_eq!(
        dup,
        (
            "a -- a a".into(),
            "a copy of the top".into(),
            "define dup 0 pick end".into()
        )
    );
    let library = "# n -- m : doubled\ndefine double\n  dup add\nend\n";
    let docs = compiler.library("l", library).unwrap_or_default();
    let code: Vec<&str> = docs.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(code, ["define double\n  dup add\nend"]);
    // A comment after a token is that line's, not the definition's below.
    let silent = compiler.library("l", "# -- : one\n1 # -- : said\ndefine x end");
    assert_eq!(silent.map_err(|e| e.to_string()).map(drop), Err(
        "contract: l:3: define: a library's definition says what it does on the line just above it: # EFFECT : ABOUT".to_owned()
    ));
    // Refused, it is as it was.
    let after = token::source("t", "x").and_then(|t| compiler.add(&t));
    assert_eq!(after.map_err(|e| e.code), Err(Code::UnknownWord));
    // A library is whole: a definition it leaves open, or one left open
    // before it, is refused, not documented by another source.
    let open = compiler.library("l", "# -- : open\ndefine open");
    assert_eq!(open.map_err(|e| e.code), Err(Code::Unclosed));
    let tokens = token::source("t", "define actual").unwrap_or_default();
    assert_eq!(compiler.add(&tokens), Ok(()));
    let closing = compiler.library("l", "# -- : fake\n1 2 end");
    assert_eq!(closing.map_err(|e| e.code), Err(Code::Unclosed));
}

/// Words calling each other, two and three of them, deeply (calls in last
/// place, through the group, keep one frame), and a recursive descent
/// parser; the checker sums each up quickly.
#[test]
fn words_may_call_each_other() -> Result<(), Error> {
    let checked = |text: &str| -> Result<std::time::Duration, Error> {
        let started = std::time::Instant::now();
        let mut compiler = with_stdlib();
        compiler.add(&token::source("t", text)?)?;
        compiler.finish()?;
        Ok(started.elapsed())
    };
    let parity = "define is-even dup 0 eq if drop true else 1 sub is-odd then end\n\
        define is-odd dup 0 eq if drop false else 1 sub is-even then end\n";
    assert_eq!(
        run(&format!("{parity}10 is-even 7 is-even"))?,
        [Value::Bool(true), Value::Bool(false)]
    );
    let three = "define a dup 0 eq if drop str a else 1 sub b then end\n\
        define b dup 0 eq if drop str b else 1 sub c then end\n\
        define c dup 0 eq if drop str c else 1 sub a then end\n";
    assert_eq!(
        run(&format!("{three}7 a 9 a"))?,
        [Value::str("b"), Value::str("a")]
    );
    // Deeper than calls may nest, as a loop.
    let mut compiler = with_stdlib();
    compiler.add(&token::source("t", &format!("{three}300001 a"))?)?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    assert_eq!(
        machine.run(&mut Toy { asked: Vec::new() }, u64::MAX)?,
        Outcome::Finished
    );
    assert_eq!(machine.stack(), [Value::str("b")]);
    let descent = "define expr\n\
          let i let s s i term let i let v\n\
          begin i s len lt if s i at str + eq else false then\n\
            if s i 1 add term let i v add let v false else true then until\n\
          v i\n\
        end\n\
        define term\n\
          let i let s s i factor let i let v\n\
          begin i s len lt if s i at str * eq else false then\n\
            if s i 1 add factor let i v mul let v false else true then until\n\
          v i\n\
        end\n\
        define factor\n\
          let i let s\n\
          s i at str ( eq if s i 1 add expr 1 add else s i at code 48 sub i 1 add then\n\
        end\n";
    assert_eq!(
        run(&format!("{descent}str 2*(3+4)+1 0 expr drop"))?,
        ints(&[15])
    );
    // A kind mistake through the group is found before it runs.
    let wrong = format!("{parity}3 is-odd 1 add");
    assert_eq!(code(run(&wrong)), Some("type"));
    for text in [parity, three, descent] {
        assert!(checked(text)? < std::time::Duration::from_secs(1), "{text}");
    }
    Ok(())
}

/// A source that fails to compile leaves the compiler as it was: none of
/// its definitions, nothing waiting for a later one.
#[test]
fn a_failed_add_leaves_the_compiler_as_it_was() -> Result<(), Error> {
    let mut compiler = with_stdlib();
    compiler.add(&token::source("t", "define good 1 end")?)?;
    let failed = compiler.add(&token::source(
        "u",
        "define a later end define b 2 end frob",
    )?);
    assert_eq!(failed.map_err(|e| e.code.name()), Err("unknown_word"));
    let pending = compiler.add(&token::source("u", "define a later end")?);
    assert_eq!(pending.map_err(|e| e.code.name()), Err("unknown_word"));
    compiler.add(&token::source("v", "define b 5 end good b")?)?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    assert_eq!(
        machine.run(&mut Toy { asked: Vec::new() }, u64::MAX)?,
        Outcome::Finished
    );
    assert_eq!(machine.stack(), ints(&[1, 5]).as_slice());
    Ok(())
}

/// A definition may take a library word's name: the program's from there
/// on, the library's words keeping their own.
#[test]
fn a_definition_may_hide_a_library_word() -> Result<(), Error> {
    assert_eq!(run("define count 7 end count")?, ints(&[7]));
    let max = "define max drop drop 0 end list 3 9 2 end largest 1 2 max";
    assert_eq!(run(max)?, ints(&[9, 0]));
    assert_eq!(
        run("define ticket 3 end ticket")?,
        ints(&[3]),
        "the application's too"
    );
    Ok(())
}

/// An unknown word is told with what was likely meant.
#[test]
fn an_unknown_word_says_what_was_meant() {
    let said = |text: &str| run(text).err().map(|e| e.message).unwrap_or_default();
    assert_eq!(said("1 drp"), "no such word; did you mean drop or dup?");
    assert_eq!(said("1 2 swpa"), "no such word; did you mean swap?");
    assert_eq!(said("Hello"), "no such word; text is `str Hello`");
    assert_eq!(
        said("'two words'"),
        "no such word; text is `str 'two words'`"
    );
    assert_eq!(said("frob"), "no such word");
    assert_eq!(said("str ab rever"), "no such word; did you mean reverse?");
}

/// Calls nested too deep are told at the word that calls itself, with the
/// word it called last.
#[test]
fn too_deep_names_the_word_that_recurses() {
    let said = run("define deep dup 0 gt drop 1 add deep 1 add end 0 deep")
        .err()
        .map(|e| e.to_string());
    assert_eq!(
        said.as_deref(),
        Some("call_depth: t:1: deep: calls nest past 100: recursion too deep (in gt)")
    );
}

/// A compiler of another's definitions sees them, wherever written; not
/// its top level, nor its `let` names, nor a definition reading one.
#[test]
fn definitions_serve_a_program_of_their_own() -> Result<(), Error> {
    let mut config = with_stdlib();
    let text = "5 let n define five n end define seven five 2 add end define six 6 end str x print";
    config.add(&token::source("c", text)?)?;
    let scope = config.definitions();
    let ran = |text: &str| -> Result<Vec<Value<Ticket>>, Error> {
        let mut compiler = scope.clone();
        compiler.add(&token::source("b", text)?)?;
        let mut machine = Machine::new(compiler.finish()?, Limits::default());
        machine.run(&mut Toy { asked: Vec::new() }, u64::MAX)?;
        Ok(machine.stack().to_vec())
    };
    assert_eq!(ran("six")?, ints(&[6]));
    assert_eq!(code(ran("five")), Some("unset"));
    assert_eq!(code(ran("seven")), Some("unset"));
    assert_eq!(code(ran("n")), Some("unknown_word"));
    Ok(())
}

fn strs(values: &[&str]) -> Vec<Value<Ticket>> {
    values.iter().map(|s| Value::str(*s)).collect()
}

/// A `try` catches the failures it names, from a word, the core, or the
/// host when it answers: the stack as it was at `try`, the plain message
/// and code on it. Through, the body's values stay.
#[test]
fn a_try_catches_the_failures_it_names() -> Result<(), Error> {
    let caught = run("1 str fail try 2 str no fail catch end")?;
    assert_eq!(caught, [ints(&[1]), strs(&["no", "fail"])].concat());
    // Only the same failure not caught is said where.
    let uncaught = run("1 str no fail").err().map(|e| e.located());
    assert_eq!(uncaught.as_deref(), Some("t:1: fail: no"));
    // A reason a try keeps is a string: one longer than a string may be
    // fails as too long, which `fail` does not name.
    let long = run("str fail try 0 300000 range fail catch end").err();
    assert_eq!(long.map(|e| e.code), Some(Code::TooLong));
    // A reason as long as a string may be is kept whole.
    let reason: String = std::iter::repeat_n('x', crate::LONGEST).collect();
    let said = format!("str fail try str {reason} fail catch drop end");
    assert_eq!(run(&said)?, strs(&[&reason]));
    // A handler with no room for its message and code fails again, for a
    // try further out: the body's `1` leaves room for the message, not
    // the code.
    let full = "0 998 range each end str stack_overflow try 1 str fail try str a fail catch drop drop end catch nip end";
    let left = run(full)?;
    assert_eq!(left.len(), 999);
    assert_eq!(left.last(), Some(&Value::str("stack_overflow")));
    // A try's codes are written in its own source, not the one before.
    let mut compiler = Compiler::<Ticket>::new(Vocabulary::default());
    compiler.add(&token::source("one", "str fail")?)?;
    let refused = compiler
        .add(&token::source("two", "try 1 catch end")?)
        .err();
    assert_eq!(refused.map(|e| e.code), Some(Code::BadCodes));
    assert_eq!(
        run("1 str fail try 2 3 catch drop drop 0 end")?,
        ints(&[1, 2, 3])
    );
    assert_eq!(
        run("str 'fail negative' try -1 ask catch end 5")?,
        [strs(&["-1 is below 0", "negative"]), ints(&[5])].concat(),
        "a suspended word's failure, answered"
    );
    assert_eq!(
        run("str not_a_number try str x ask catch nip end")?,
        strs(&["not_a_number"]),
        "a host word's failure at once"
    );
    assert_eq!(
        run("str unused try nope catch nip end")?,
        strs(&["unused"]),
        "a code of the language's that the host's words raise"
    );
    assert_eq!(
        run("str div_by_zero try 1 0 div catch nip end")?,
        strs(&["div_by_zero"])
    );
    // Not named, a failure goes through untouched: its code, its place.
    let through = run("str fail try\n  -1 ask\ncatch drop drop end").err();
    assert_eq!(
        through.map(|e| e.to_string()).as_deref(),
        Some("negative: t:2: ask: -1 is below 0")
    );
    // Nested: the innermost that names it; a failure in a handler goes out.
    let nested = "str div_by_zero try str fail try 1 0 div catch drop drop 5 end catch nip end";
    assert_eq!(run(nested)?, strs(&["div_by_zero"]));
    let again =
        "str fail try str fail try str a fail catch drop drop str b fail end catch drop end";
    assert_eq!(run(again)?, strs(&["b"]));
    assert_eq!(
        code(run("str fail try 1 catch end str c fail")),
        Some("fail")
    );
    Ok(())
}

/// What a failing body began is undone however deep: its calls, a loop of
/// them in last place, its `list`s and `each`es; the program goes on.
#[test]
fn a_caught_failure_undoes_what_the_body_began() -> Result<(), Error> {
    let deep = "define deep dup 0 eq if str bottom fail then 1 sub deep end
        define safe let n str fail try n deep catch drop end end";
    assert_eq!(
        run(&format!("{deep} 50 safe 7 3 safe"))?,
        [strs(&["bottom"]), ints(&[7]), strs(&["bottom"])].concat(),
        "the body's call is not in its definition's last place"
    );
    let recursion = "define down 1 add down 1 end str call_depth try 0 down catch nip end";
    assert_eq!(run(recursion)?, strs(&["call_depth"]));
    let blocks = "list 1 str fail try 2 list 3 4 end each dup 4 eq if str four fail then end
        catch drop end 5 end list 6 7 end each end";
    let list = List::new([ints(&[1]), strs(&["four"]), ints(&[5])].concat());
    let list = list.map(|l| Value::List(std::rc::Rc::new(l)));
    assert_eq!(run(blocks)?, [Vec::from_iter(list), ints(&[6, 7])].concat());
    let names = "1 let x str fail try 2 let x x fail catch drop drop end x";
    assert_eq!(
        run(names)?,
        ints(&[2]),
        "a let name keeps what the body set"
    );
    Ok(())
}

/// Run in one go or paused every few steps, a run through `try`s ends the
/// same.
#[test]
fn a_try_ends_the_same_however_paused() -> Result<(), Error> {
    let program =
        "define f let n str 'fail negative' try n ask n 0 eq if str z fail then catch end end
        list -1 f 0 f 2 f end str fail try 1 2 3 str a fail catch end";
    let whole = run_by(program, u64::MAX)?;
    for budget in 1..=12 {
        assert_eq!(run_by(program, budget)?, whole, "{budget}");
    }
    Ok(())
}

/// A `try` is checked as `if … else … then` is: its body from an empty
/// stack, its handler from the stack at `try` and two strings, joined
/// after. Its codes are a string just before it, each one a running word
/// may raise.
#[test]
fn a_try_is_checked_before_it_runs() {
    let refused = |text: &str| {
        let mut compiler = with_stdlib();
        let compiled = token::source("t", text)
            .and_then(|tokens| compiler.add(&tokens))
            .and_then(|()| compiler.finish().map(|_| ()));
        compiled.err().map(|e| e.to_string())
    };
    for (text, said) in [
        ("try 1 catch end", "bad_codes: t:1: try: takes the codes"),
        ("1 try 1 catch end", "bad_codes: t:1: try: takes the codes"),
        (
            "str negativ try 1 catch end",
            "bad_codes: t:1: try: negativ is no code a word raises; did you mean negative?",
        ),
        (
            "str unknown_word try 1 catch end",
            "bad_codes: t:1: try: unknown_word is found",
        ),
        (
            "str 'fail contract' try 1 catch end",
            "bad_codes: t:1: try: contract is found",
        ),
        (
            "str 'fail unclosed' try 1 catch end",
            "bad_codes: t:1: try: unclosed is found",
        ),
        (
            "str '' try 1 catch end",
            "bad_codes: t:1: try: names no code",
        ),
        (
            "1 str fail try drop catch end",
            "stack_underflow: t:1: drop: takes 1 value, and the stack has 0 here (inside try … catch",
        ),
        (
            "str fail try 1 catch drop drop end add",
            "stack_underflow: t:1: add: takes 2 values, and the stack has at most 1",
        ),
        ("str fail try str a catch drop end 1 add", "type: t:1: add:"),
        ("catch", "unmatched: t:1: catch: no try is open"),
        (
            "str fail try 1 end",
            "unmatched: t:1: end: the open block is try … catch",
        ),
        (
            "str fail try 1 catch",
            "unclosed: t:1: try: this try … catch … end is never closed",
        ),
        ("define try end", "bad_name"),
    ] {
        let error = refused(text).unwrap_or_default();
        assert!(error.starts_with(said), "{text}: {error}");
    }
    // A handler's message is a string, as is its code; either way joins.
    assert_eq!(refused("str fail try 1 catch drop drop 2 end 1 add"), None);
    assert_eq!(refused("str fail try 1 catch drop end 1 add"), None);
    // One the host's words raise is caught, though plain fuxth's never is.
    assert_eq!(refused("str unused try 1 catch end"), None);
    let plain = token::source("t", "str unused try 1 catch end")
        .and_then(|tokens| Compiler::<Ticket>::new(Vocabulary::default()).add(&tokens));
    assert_eq!(plain.err().map(|e| e.code), Some(Code::BadCodes));
    let mut words = Vocabulary::default();
    assert!(
        words.raises(Code::Host("fail")).is_err(),
        "the language's own"
    );
    assert!(words.raises(Code::Host("two words")).is_err());
}

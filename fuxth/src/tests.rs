//! The language, run as a host runs it: compiled whole, then run to its
//! end, through every suspension.
use crate::*;

/// A host with one value of its own, a ticket, and three words: `ticket`
/// (-- t), which hands one out at once; `ask` (n -- n*10), which suspends
/// for the host to answer; `fail`, which fails.
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
        let args: Vec<_> = args.collect();
        match (word, args.as_slice()) {
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

/// A call that is its definition's last step runs in its caller's frame:
/// a loop written as recursion never runs out of calls.
#[test]
fn a_call_in_last_place_reuses_its_frame() -> Result<(), Error> {
    let down = "define down dup 0 eq if else 1 sub down then end 5000 down";
    assert_eq!(run(down)?, ints(&[0]), "5000 deep, the call limit 100");
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
        Some("type: t:1: add: add takes two integers, and the top one is a string"),
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

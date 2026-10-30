//! The language, run as a host runs it: compiled whole, then run to its
//! end, through every suspension.
use crate::*;

#[global_allocator]
static ALLOCATOR: fux_vt_peak::ThreadPeak = fux_vt_peak::ThreadPeak;

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
    fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), Failure> {
        if self.0 == i64::MIN {
            return Err(Failure::new(
                Code::TooLong,
                "the host refused text conversion",
            ));
        }
        out.write_str(self.kind())
            .map_err(|_| Failure::new(Code::TooLong, "text limit"))
    }
}

#[derive(Default)]
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
            (4, []) => {
                out.push(Value::App(Ticket(i64::MIN)));
                Ok(Done::Now)
            }
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

#[test]
fn source_quoting_inverts_tokenization_and_refuses_expansion() -> Result<(), Error> {
    for text in [
        "",
        "ordinary",
        "'",
        "\\",
        "line\nnext\r\ttab",
        "# comment",
        "'\\\"雪😀",
        "str evil drop\n# injected source",
    ] {
        let mut compiler = with_stdlib();
        compiler.add(&token::arguments(&["str", text, "quote"]))?;
        let mut machine = Machine::new(compiler.finish()?, Limits::default());
        assert_eq!(
            machine.run(&mut Toy::default(), 100_000)?,
            Outcome::Finished
        );
        let [Value::Str(source)] = machine.stack() else {
            return Err(Error::new(Code::Type, "quote did not return text", None));
        };
        let tokens = token::source("quoted", source)?;
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens.first().map(|t| t.text.as_ref()), Some(text));
    }
    let text: String =
        std::iter::repeat_n(char::from(39), LONGEST.checked_div(4).unwrap_or(0)).collect();
    let mut compiler = with_stdlib();
    compiler.add(&token::arguments(&["str", &text, "quote"]))?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    let result = machine.run(&mut Toy::default(), 30_000_000);
    assert_eq!(result.map_err(|e| e.code), Err(Code::TooLong));
    Ok(())
}

#[test]
fn declared_library_inputs_hold_before_calls_and_on_uncertain_values() -> Result<(), Error> {
    let mut library = with_stdlib();
    library.add(&token::source(
        "library",
        "define checked drop end define produce 7 end",
    )?)?;
    assert!(library.inputs("checked", "missing-kind").is_err());
    library.inputs("checked", "integer")?;
    library.inputs("produce", "integer")?;
    assert!(library.inputs("checked", "integer").is_err());
    library.prepare();

    for (source, code) in [
        ("true checked", Code::Type),
        ("checked", Code::StackUnderflow),
        ("define caller produce end caller", Code::StackUnderflow),
    ] {
        let mut compiler = library.clone();
        compiler.add(&token::source("caller", source)?)?;
        assert_eq!(compiler.finish().map_err(|e| e.code).err(), Some(code));
    }
    let mut compiler = library.clone();
    compiler.add(&token::source("caller", "7 checked")?)?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    assert_eq!(machine.run(&mut Toy::default(), 100)?, Outcome::Finished);
    assert!(machine.stack().is_empty());

    let mut compiler = library;
    compiler.add(&token::source("caller", "0 ask checked")?)?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    let mut host = Toy::default();
    assert_eq!(machine.run(&mut host, 100)?, Outcome::Suspended(0));
    machine.import(Value::Bool(false))?;
    assert_eq!(
        machine.run(&mut host, 100).map_err(|e| e.code),
        Err(Code::Type)
    );
    assert_eq!(
        machine.stack(),
        &[Value::Bool(false)],
        "the body ran before its input check"
    );
    Ok(())
}

fn vocabulary() -> Vocabulary {
    let mut words = Vocabulary::default();
    let added = (words.kind("ticket", "a ticket"))
        .and_then(|()| words.add("ticket", "-- ticket"))
        .and_then(|_| words.add("ask", "any -- any"))
        .and_then(|_| words.add("nope", "--"))
        .and_then(|_| words.add("liar", "-- any"))
        .and_then(|_| words.add("no-text", "-- ticket"))
        .and_then(|_| words.raises(Code::Host("not_a_number")))
        .and_then(|()| words.raises(Code::Host("refused")))
        .and_then(|()| words.raises(Code::Host("negative")))
        .and_then(|()| words.raises(Code::Host("far")))
        .and_then(|()| words.carries(Code::Host("far"), "integer"))
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
    let mut machine = Machine::new(
        compile(text)?,
        Limits {
            stack: 1000,
            calls: 100,
        },
    );
    let mut host = Toy::default();
    loop {
        match machine.run(&mut host, budget)? {
            Outcome::Finished => return Ok(machine.stack().to_vec()),
            Outcome::Suspended(n) if n <= -1000 => {
                let failure = Failure::new(Code::Host("far"), format!("{n} is far below 0"));
                machine.raise_with(failure, Value::Int(n))?;
            }
            Outcome::Suspended(n) if n < 0 => machine.raise(Failure::new(
                Code::Host("negative"),
                format!("{n} is below 0"),
            ))?,
            Outcome::Suspended(n) => machine.import(Value::Int(n.saturating_mul(10)))?,
            Outcome::Paused => {}
        }
    }
}

fn compile(text: &str) -> Result<Program<Ticket>, Error> {
    let mut compiler = with_stdlib();
    compiler.add(&token::source("t", text)?)?;
    compiler.finish()
}

fn refused(text: &str) -> Option<&'static str> {
    compile(text).err().map(|e| e.code.name())
}

fn ints(values: &[i64]) -> Vec<Value<Ticket>> {
    values.iter().map(|n| Value::Int(*n)).collect()
}

fn code(result: Result<Vec<Value<Ticket>>, Error>) -> Option<&'static str> {
    result.err().map(|e| e.code.name())
}

#[test]
fn host_text_refusal_reaches_every_text_word() {
    for source in [
        "no-text print",
        "no-text string",
        "no-text fail",
        "list no-text end print",
    ] {
        assert_eq!(code(run(source)), Some("too_long"), "{source}");
    }
}

#[test]
fn strings_and_equality_preserve_value_kinds() -> Result<(), Error> {
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

#[test]
fn the_calls_under_way_show_who_called() -> Result<(), Error> {
    let mut compiler = with_stdlib();
    compiler.add(&token::source(
        "t",
        "define inner 1 ask end\ndefine outer inner end\nouter",
    )?)?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    let outcome = machine.run(&mut Toy::default(), 100)?;
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

#[test]
fn a_paused_loop_shows_its_own_step() -> Result<(), Error> {
    for budget in 1..=12 {
        let mut machine = Machine::new(
            compile("str x print\nbegin 0 1 eq until")?,
            Limits::default(),
        );
        let outcome = machine.run(&mut Toy::default(), budget)?;
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
        for _ in 0..3 {
            assert_eq!(machine.run(&mut Toy::default(), 1000)?, Outcome::Paused);
        }
    }
    Ok(())
}

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
fn an_application_word_suspends_and_resumes_deep_in_calls_and_loops() -> Result<(), Error> {
    assert_eq!(run("ticket")?, [Value::App(Ticket(7))]);
    let nested = "define asks 0 begin 1 add dup ask swap dup 3 eq until drop end asks";
    assert_eq!(run(nested)?, ints(&[10, 20, 30]));
    assert_eq!(
        run("ticket kind ticket string")?,
        [Value::str("ticket"), Value::str("a ticket")]
    );
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
        ("99999999999999999999", "overflow"),
        ("'unclosed", "open_quote"),
        ("str abc 0 at 1 add", "type"),
        ("str abc str b at", "type"),
        ("str m print str a 1 push", "type"),
        ("str m print str a 0 1 put", "type"),
        ("define f fail end str m print f", "stack_underflow"),
        (
            "define f if str x fail else drop drop then end true f",
            "stack_underflow",
        ),
        ("str x 5 pad", "stack_underflow"),
        ("true or", "stack_underflow"),
        (
            "define f begin dup true until end str m print f",
            "stack_underflow",
        ),
        (
            "define d dup drop list d end end str m print d",
            "stack_underflow",
        ),
    ] {
        assert_eq!(refused(text), Some(expected), "{text}");
    }
}

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

#[test]
fn the_checker_says_what_and_where() {
    let refused = |text: &str| -> String {
        compile(text)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default()
    };
    let (maybe_drop, maybe_swap) = (
        format!("{MAYBE}0 maybe drop drop"),
        format!("{MAYBE}0 maybe swap"),
    );
    for (text, said) in [
        (
            "define f str x add end\n1 f",
            "type: t:1: add: add takes an integer, not a string on top",
        ),
        (
            "define nop end\nstr a 1 nop add",
            "type: t:2: add: add takes an integer, not a string under the top",
        ),
        (
            "define f str a 1 add end str b 1 sub",
            "type: t:1: add: add takes an integer, not a string under the top",
        ),
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
    let mut compiler = with_stdlib();
    let compiled = compiler
        .add(&token::arguments(&[
            "define", "f", "str", "a", "1", "add", "end", "str", "b", "1", "sub",
        ]))
        .and_then(|()| compiler.finish());
    assert_eq!(
        compiled.err().map(|e| e.to_string()).as_deref(),
        Some("type: argument 6: add: add takes an integer, not a string under the top")
    );
}

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

#[test]
fn each_walks_any_string_in_time_to_its_length() -> Result<(), Error> {
    let walk = |c: &str| -> Result<std::time::Duration, Error> {
        let mut machine = Machine::new(
            compile("0 ask 0 swap each drop 1 add end")?,
            Limits::default(),
        );
        let mut host = Toy::default();
        assert_eq!(machine.run(&mut host, u64::MAX)?, Outcome::Suspended(0));
        machine.import(Value::str(
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
    match run(&format!("str {} 100 str ü put", pieces.concat()))?.as_slice() {
        [Value::Str(put)] => check(put),
        _ => return Err(Error::new(Code::Type, "put did not give a string", None)),
    }
    let steps = |text: &str| -> Result<u64, Error> {
        let mut machine = Machine::new(compile(text)?, Limits::default());
        machine.run(&mut Toy::default(), u64::MAX)?;
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

#[test]
fn what_can_not_be_known_is_never_refused() -> Result<(), Error> {
    let program = "define f dup 0 gt if 1 then end\n5 f drop drop";
    assert_eq!(run(program)?, ints(&[]));
    let grows = "0 begin dup 1 add dup 5 eq until add add add add add";
    assert_eq!(run(grows)?, ints(&[15]));
    let either = "0 0 eq if 1 else str a then 1 add";
    assert_eq!(run(either)?, ints(&[2]));
    assert_eq!(
        run("define f if else drop 7 then end true f 5")?,
        ints(&[5])
    );
    for source in [
        "define f if str x fail else drop drop then end 1 2 true f",
        "define die str x fail end false if die then 1",
    ] {
        assert_eq!(refused(source), None, "{source}");
    }
    Ok(())
}

#[test]
fn the_machine_says_what_and_where() -> Result<(), Error> {
    assert_eq!(code(run("str x ask")), Some("not_a_number"));
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
    let mut machine = Machine::new(compile("1\n  2 ask")?, Limits::default());
    assert_eq!(
        machine.run(&mut Toy::default(), 100)?,
        Outcome::Suspended(2)
    );
    let at = machine.at().map(|t| format!("{}: {}", t.place, t.text));
    assert_eq!(at.as_deref(), Some("t:2: ask"), "the word suspended at");
    Ok(())
}

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
        machine.run(&mut Toy::default(), 100)
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

#[test]
fn the_names_of_nested_calls_count_against_the_stack() {
    let names: String = (0..20).map(|n| format!("0 let x{n} ")).collect();
    let failed = run(&format!("define recur recur {names}end recur"));
    assert_eq!(failed.err().map(|e| e.code.name()), Some("stack_overflow"));
}

#[test]
fn every_core_word_documents_its_effect_and_meaning() {
    for builtin in Builtin::ALL {
        assert!(builtin.about().len() > 8, "{}", builtin.name());
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
    assert_eq!(code(run("str abc 2 1 slice")), Some("range"));
    assert_eq!(code(run("str abc 0 4 slice")), Some("range"));
    assert_eq!(run("str x 5 str ab pad len")?, ints(&[5]));
    assert_eq!(
        run("str a str b lt str b str a lt str ab str a lt")?,
        [truth(true), truth(false), truth(false)]
    );
    assert_eq!(
        run("list list 0 end end 0 7 put list 7 end eq")?,
        [truth(true)]
    );
    assert_eq!(
        run("str é str ab push dup 2 at swap 0 at")?,
        [s("b"), s("é")]
    );
    Ok(())
}

#[test]
fn a_string_grown_in_place_holds_no_more_than_it_was_charged() -> Result<(), Error> {
    // ASCII first, then more than ASCII from its last push, in place.
    for n in [1, 63, 64, 65, 200, 1000, 5000] {
        let source = format!(
            "str a 0 begin swap str b push swap 1 add dup 100 eq until drop \
             0 begin swap str é push swap 1 add dup {n} eq until drop"
        );
        let mut machine = Machine::new(compile(&source)?, Limits::default());
        machine.run(&mut Toy::default(), u64::MAX)?;
        let imports = machine.imports().clone();
        let charged = Ticket::IMPORT_LIMIT.saturating_sub(imports.remaining());
        let string = machine.stack().first().cloned();
        drop(machine);
        let held = ALLOCATOR.live();
        drop(string);
        let held = held.saturating_sub(ALLOCATOR.live());
        assert!(
            held > 0 && held <= charged,
            "{n}: holds {held}, charged {charged}"
        );
    }
    Ok(())
}

#[test]
fn concat_refuses_before_allocating_the_joined_value() -> Result<(), Error> {
    // As many lists of a million as the grant holds, then a copy of one.
    let held = Ticket::IMPORT_LIMIT / List::<Ticket>::size(1_000_000);
    let full: String = (1..held)
        .map(|i| format!("list l0 end concat let l{i} "))
        .collect();
    let full = format!("0 1000000 range let l0 {full}");
    for source in [
        "str x 1000 copies let s list 0 1001 range each drop s end end",
        "0 1000000 range let l list l l l l l l l l l l end",
        &format!("{full} list l0 end"),
    ] {
        let mut machine = Machine::new(
            compile(&format!("{source} 0 ask drop concat"))?,
            Limits::default(),
        );
        let mut host = Toy::default();
        assert_eq!(machine.run(&mut host, u64::MAX)?, Outcome::Suspended(0));
        machine.import(Value::Int(0))?;
        let before = ALLOCATOR.live();
        ALLOCATOR.reset_peak();
        let result = machine.run(&mut host, u64::MAX);
        let allocated = ALLOCATOR.peak().saturating_sub(before);
        assert_eq!(result.map_err(|e| e.code), Err(Code::TooLong));
        assert!(
            allocated < 4096,
            "concat allocated {allocated} bytes: {source}"
        );
    }
    Ok(())
}

#[test]
fn concat_checks_kinds_before_length_in_input_order() -> Result<(), Error> {
    for (parts, message) in [
        ("s list end 0", "not both"),
        ("list end s 0", "not both"),
        ("s 0 list end", "this one holds an integer"),
    ] {
        let source =
            format!("str x 1000 copies let s list 0 1001 range each drop s end {parts} end concat");
        let mut machine = Machine::new(compile(&source)?, Limits::default());
        let error = machine.run(&mut Toy::default(), u64::MAX).err();
        assert!(error.is_some_and(|e| e.code == Code::Type && e.message.ends_with(message)));
    }
    Ok(())
}

#[test]
fn concat_charges_the_length_once() -> Result<(), Error> {
    for (parts, expected, n) in [
        ("list end", "list end", 0),
        ("list str '' end", "str ''", 0),
        ("list str ab str cd end", "str abcd", 4),
        (
            "list str é 64 copies str 𝄞 end",
            "str é 64 copies str 𝄞 push",
            65,
        ),
        ("list 0 32 range 32 64 range end", "0 64 range", 64),
    ] {
        let mut machine = Machine::new(
            compile(&format!("{parts} 0 ask drop concat 0 ask"))?,
            Limits::default(),
        );
        let mut host = Toy::default();
        assert_eq!(machine.run(&mut host, u64::MAX)?, Outcome::Suspended(0));
        machine.import(Value::Int(0))?;
        let before = machine.steps();
        assert_eq!(machine.run(&mut host, u64::MAX)?, Outcome::Suspended(0));
        assert_eq!(machine.steps().saturating_sub(before), 4 + n / 64);
        assert_eq!(machine.stack(), run(expected)?);
    }
    Ok(())
}

#[test]
fn constructed_values_obey_length_depth_and_work_limits() -> Result<(), Error> {
    for source in [
        "list end let l 0 begin list l end let l 1 add dup 70 eq until",
        "list end let l 0 begin list end l push let l 1 add dup 70 eq until",
        "list 0 end let l 0 begin list 0 end 0 l put let l 1 add dup 70 eq until",
        "list 1 2 3 4 5 6 7 8 end let x 0 begin list x x end let x 1 add dup 40 eq until",
    ] {
        let mut machine = Machine::new(compile(source)?, Limits::default());
        assert_eq!(
            machine
                .run(&mut Toy::default(), u64::MAX)
                .map_err(|e| e.code),
            Err(Code::TooLong),
            "{source}"
        );
    }
    // What a run builds and drops it gives back: it builds on till its
    // steps run out.
    for source in [
        "str x 1000 copies let s 0 begin s str y push drop 1 add dup 17000 eq until",
        "str x 1000 copies let s 0 begin s 0 str y put drop 1 add dup 17000 eq until",
        "0 1000 range let s 0 begin s 7 push drop 1 add dup 17000 eq until",
        "0 1000 range let s 0 begin s 0 7 put drop 1 add dup 17000 eq until",
        "str x 2000 copies let cap 0 begin cap string drop 1 add dup 100000 eq until",
        "list end let l 0 begin list l end let l 1 add dup 60 eq until",
        "list 1 2 end let x 0 begin list x x end let x 1 add dup 10 eq until x x eq",
    ] {
        assert_eq!(code(run(source)), None, "{source}");
    }
    for (source, left) in [
        (
            "0 1000000 range len str a 1000000 copies len",
            &[1_000_000, 1_000_000][..],
        ),
        (
            "str x 1000 copies 1000 copies let s list s end string len",
            &[1_000_000],
        ),
        ("str x 1000000 copies 0 str y put len", &[1_000_000]),
        (
            "list list list 1 end end end 0 5 put let x 0 begin list x end let x 1 add dup 63 eq until drop x len",
            &[1],
        ),
    ] {
        assert_eq!(run(source)?, ints(left), "{source}");
    }
    let wide = run("list 0 600000 range end string");
    assert!(wide.is_err_and(|e| e.message.contains("1000000") && !e.message.contains("2000000")));
    Ok(())
}

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

#[test]
fn sequence_mutation_preserves_shared_values() -> Result<(), Error> {
    let list =
        |items: Vec<Value<Ticket>>| crate::List::new(items).map(|l| Value::List(Shared::new(l)));
    assert_eq!(
        run("list 1 end 2 push")?,
        list(ints(&[1, 2])).into_iter().collect::<Vec<_>>()
    );
    let both: Vec<_> = list(ints(&[1, 2]))
        .into_iter()
        .chain(list(ints(&[9, 2])))
        .collect();
    assert_eq!(run("list 1 2 end dup 0 9 put")?, both);
    assert_eq!(
        run("str x 1000 copies 0 let i begin 0 str y put i 1 add let i i 17000 eq until len")?,
        ints(&[1000])
    );
    assert_eq!(
        run("str héllo let s s 1 str E put s 0 str ab put")?,
        [Value::str("hEllo"), Value::str("abéllo")]
    );
    assert_eq!(
        run("str ab let s s str c push s")?,
        [Value::str("abc"), Value::str("ab")]
    );
    let alone = "0 1000 range 0 begin swap 0 7 put swap 1 add dup 17000 eq until drop len";
    assert_eq!(run(alone)?, ints(&[1000]));
    Ok(())
}

#[test]
fn a_value_replaced_below_what_is_surely_used_is_forgotten() -> Result<(), Error> {
    let replace = "define replace each swap drop end end str a list 1 2 end replace 1 add";
    assert_eq!(run(replace)?, ints(&[3]));
    let swapped = "define f each drop drop 1 end end str a list 0 end f 1 add";
    assert_eq!(run(swapped)?, ints(&[2]));
    Ok(())
}

#[test]
fn long_chains_and_long_strings_are_bounded() -> Result<(), Error> {
    let chain: String = (1..20_000)
        .map(|i: u32| format!("define f{i} f{} end\n", i.saturating_sub(1)))
        .collect();
    let chained = format!("define f0 1 add end\n{chain}5 f19999");
    assert_eq!(run(&chained)?, ints(&[6]));
    let steps = |text: &str| -> Result<u64, Error> {
        let mut machine = Machine::new(compile(text)?, Limits::default());
        machine.run(&mut Toy::default(), u64::MAX)?;
        Ok(machine.steps())
    };
    let (made, put) = (
        steps("str é 100000 copies len")?,
        steps("str é 100000 copies 0 str y put len")?,
    );
    assert!(put.saturating_sub(made) > 100_000 / 64, "{made} {put}");
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
                Outcome::Suspended(()) => machine.import(Value::App(answer.clone()))?,
                Outcome::Paused => {}
            }
        }
    })();
    (ran, desk.calls)
}

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
            "pane: taken as more after a word was added",
        ]
    );
}

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
    let mut machine = Machine::new(compile(&format!("{three}300001 a"))?, Limits::default());
    assert_eq!(
        machine.run(&mut Toy::default(), u64::MAX)?,
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
        machine.run(&mut Toy::default(), u64::MAX)?,
        Outcome::Finished
    );
    assert_eq!(machine.stack(), ints(&[1, 5]).as_slice());
    Ok(())
}

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

#[test]
fn unknown_words_keep_their_source() -> Result<(), String> {
    for text in [
        "1 DUP",
        "drp",
        "swpa",
        "Hello",
        "'two words'",
        "frob",
        "rever",
        "''",
        "'$HOME'",
        "/path",
    ] {
        let error = run(text).err().ok_or("expected unknown word")?;
        assert_eq!(error.code, Code::UnknownWord);
        assert_eq!(error.message, "no such word; text is `str WORD`");
        assert!(error.place.is_some());
    }
    let error = run("1 2\n  frob").err();
    assert_eq!(
        error.map(|e| e.to_string()).as_deref(),
        Some("unknown_word: t:2: frob: no such word; text is `str WORD`")
    );
    Ok(())
}

#[test]
fn non_tail_calls_refuse_only_after_the_call_limit() -> Result<(), Error> {
    let deep = "define f dup 0 eq if else 1 sub f 0 drop then end";
    assert_eq!(run(&format!("{deep} 99 f 0"))?, ints(&[0, 0]));
    assert_eq!(code(run(&format!("{deep} 100 f 0"))), Some("call_depth"));
    let said = run("define deep dup 0 gt drop 1 add deep 1 add end 0 deep")
        .err()
        .map(|e| e.to_string());
    assert_eq!(
        said.as_deref(),
        Some("call_depth: t:1: deep: calls nest past 100: recursion too deep (in gt)")
    );
    Ok(())
}

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
        machine.run(&mut Toy::default(), u64::MAX)?;
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
    // A failure that carries a value gives it in place of the message,
    // as the vocabulary says its code may: a number, here.
    assert_eq!(
        run("str far try -1000 ask catch drop 1 add end")?,
        ints(&[-999])
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
    let list = list.map(|l| Value::List(Shared::new(l)));
    assert_eq!(run(blocks)?, [Vec::from_iter(list), ints(&[6, 7])].concat());
    let names = "1 let x str fail try 2 let x x fail catch drop drop end x";
    assert_eq!(
        run(names)?,
        ints(&[2]),
        "a let name keeps what the body set"
    );
    Ok(())
}

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

#[test]
fn a_try_is_checked_before_it_runs() {
    for (text, said) in [
        ("try 1 catch end", "bad_codes: t:1: try: takes the codes"),
        ("1 try 1 catch end", "bad_codes: t:1: try: takes the codes"),
        (
            "str negativ try 1 catch end",
            "bad_codes: t:1: try: negativ is no code a word raises",
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
        let error = compile(text)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
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

#[test]
fn carried_values_obey_their_contract() -> Result<(), Error> {
    let far = Code::Host("far");
    let mut words = vocabulary();
    words.name("nothing", "", "no value")?;
    for invalid in [
        words.carries(Code::Host("negative"), "nothing"),
        words.carries(far, "boolean"),
        words.carries(Code::Host("unknown"), "integer"),
        words.carries(Code::Host("negative"), "unknown"),
    ] {
        assert_eq!(invalid.err().map(|e| e.code), Some(Code::Contract));
    }
    for (code, value, admitted) in [
        (far, Value::Int(7), true),
        (far, Value::Bool(false), false),
        (
            far,
            Value::str("a message is not an integer payload"),
            false,
        ),
        (Code::Host("negative"), Value::Int(7), false),
        (Code::Host("unknown"), Value::Int(7), false),
    ] {
        let mut compiler = Compiler::new(words.clone());
        compiler.add(&token::source(
            "carried",
            "str 'far negative' try 1 ask catch drop end",
        )?)?;
        let mut machine = Machine::new(compiler.finish()?, Limits::default());
        let mut host = Toy::default();
        assert!(matches!(
            machine.run(&mut host, 100)?,
            Outcome::Suspended(1)
        ));
        let before = machine.stack().to_vec();
        let raised = machine.raise_with(Failure::new(code, "refused"), value.clone());
        if admitted {
            raised?;
            assert!(matches!(machine.run(&mut host, 100)?, Outcome::Finished));
            assert_eq!(machine.stack(), [value]);
        } else {
            let error = raised
                .err()
                .ok_or_else(|| Error::new(Code::Contract, "admitted an invalid payload", None))?;
            assert_eq!(error.code, Code::Contract);
            assert_eq!(error.word.as_deref(), Some("ask"));
            assert_eq!(machine.stack(), before);
            // Refusal did not enter the handler or clear the pending word.
            machine.raise_with(Failure::new(far, "refused"), Value::Int(8))?;
            assert!(matches!(machine.run(&mut host, 100)?, Outcome::Finished));
            assert_eq!(machine.stack(), [Value::Int(8)]);
        }
    }
    assert_eq!(
        run("str far try -1000 ask catch drop end")?,
        [Value::Int(-1000)]
    );
    Ok(())
}

#[test]
fn refused_carried_storage_enters_the_resource_failure_handler() -> Result<(), Error> {
    #[derive(Clone, Debug, PartialEq)]
    struct Payload(Shared<Vec<u8>>);
    impl AppValue for Payload {
        const IMPORT_LIMIT: usize = 2048;
        fn kind(&self) -> &'static str {
            "payload"
        }
        fn weight(&self) -> u64 {
            self.0.len() as u64
        }
        fn retain(&self, imports: &Imports) -> Result<(), Failure> {
            self.0
                .reserve(imports, Shared::<Vec<u8>>::bytes(self.0.capacity()))
        }
    }
    struct Wait;
    impl Host for Wait {
        type Value = Payload;
        type Request = ();
        fn call(
            &mut self,
            word: usize,
            _: Args<'_, Payload>,
            out: &mut Vec<Value<Payload>>,
        ) -> Result<Done<()>, Failure> {
            if word == 0 {
                out.push(Value::App(Payload(Shared::new(vec![0; 800]))));
                Ok(Done::Now)
            } else {
                Ok(Done::Later(()))
            }
        }
        fn print(&mut self, _: &str) -> Result<(), Failure> {
            Ok(())
        }
    }
    let timeout = Code::Host("timeout");
    let mut words = Vocabulary::default();
    words.kind("payload", "a payload")?;
    words.add("request", "-- payload")?;
    words.add("wait", "payload -- payload")?;
    words.raises(timeout)?;
    words.carries(timeout, "payload")?;
    let mut compiler = Compiler::<Payload>::new(words);
    // The saved request keeps its allocation even after wait consumes its copy.
    compiler.add(&token::source(
        "carried",
        "request let saved str 'timeout too_long' try saved wait catch 1 roll drop end 7",
    )?)?;
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    let mut host = Wait;
    assert!(matches!(
        machine.run(&mut host, 1000)?,
        Outcome::Suspended(())
    ));
    machine.raise_with(
        Failure::new(timeout, "timed out"),
        Value::App(Payload(Shared::new(vec![0; 1300]))),
    )?;
    assert!(matches!(machine.run(&mut host, 1000)?, Outcome::Finished));
    assert_eq!(machine.stack(), [Value::str("too_long"), Value::Int(7)]);
    Ok(())
}

#[test]
fn host_imports_bound_each_value_and_opaque_depth() -> Result<(), Error> {
    #[derive(Clone, PartialEq)]
    struct Weighted(u64, u8);
    impl AppValue for Weighted {
        fn kind(&self) -> &'static str {
            "weighted"
        }
        fn weight(&self) -> u64 {
            self.0
        }
        fn depth(&self) -> u8 {
            self.1
        }
    }
    let mut vocabulary = Vocabulary::default();
    vocabulary.kind("weighted", "weighted")?;
    let compiler = Compiler::<Weighted>::new(vocabulary);
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    let value = Value::App(Weighted(TOTAL / 2, 0));
    machine.import(value.clone())?;
    machine.import(value.clone())?;
    machine.import(value.clone())?;
    assert_eq!(machine.stack().len(), 3);
    assert!(List::new(vec![value.clone(), value]).is_none());
    assert!(List::new(vec![Value::App(Weighted(1, NESTING))]).is_none());
    Ok(())
}

#[test]
fn native_imports_share_reservations_and_refuse_independent_storage() -> Result<(), Error> {
    #[derive(Clone, PartialEq)]
    struct Payload(Shared<Vec<u8>>);
    impl AppValue for Payload {
        const IMPORT_LIMIT: usize = 256;
        fn kind(&self) -> &'static str {
            "payload"
        }
        fn weight(&self) -> u64 {
            TOTAL.saturating_add(1)
        }
        fn import_bytes(&self) -> Option<usize> {
            Some(Shared::<Vec<u8>>::bytes(self.0.capacity()))
        }
        fn retain(&self, imports: &Imports) -> Result<(), Failure> {
            self.0.reserve(imports, self.import_bytes().unwrap_or(0))
        }
    }
    let make = || Value::App(Payload(Shared::new(vec![0; 64])));
    let compiler = Compiler::<Payload>::new(Vocabulary::default());
    let mut machine = Machine::new(compiler.finish()?, Limits::default());
    let value = make();
    machine.import(value.clone())?;
    machine.import(value.clone())?;
    machine.import(value.clone())?;
    assert_eq!(machine.stack().len(), 3);
    assert!(List::new(vec![value]).is_none());
    machine.import(make())?;
    assert_eq!(
        machine.import(make()).map_err(|e| e.code),
        Err(Code::TooLong)
    );
    Ok(())
}

#[test]
fn imported_strings_and_lists_hold_and_release_their_storage() -> Result<(), Failure> {
    let grant = Imports::new(1024);
    let text = || Value::<Ticket>::str(std::iter::repeat_n('x', 500).collect::<String>());
    let held = text();
    held.retain(&grant)?;
    let duplicate = held.clone();
    duplicate.retain(&grant)?;
    let list = Value::List(Shared::new(
        List::new(vec![held]).ok_or_else(|| Failure::new(Code::TooLong, "list"))?,
    ));
    list.retain(&grant)?;
    assert_eq!(
        text().retain(&grant).map_err(|e| e.code),
        Err(Code::TooLong)
    );
    drop(list);
    assert_eq!(
        text().retain(&grant).map_err(|e| e.code),
        Err(Code::TooLong)
    );
    drop(duplicate);
    for _ in 0..100 {
        text().retain(&grant)?;
    }
    let held = text();
    held.retain(&grant)?;
    let Value::Str(mut storage) = held else {
        return Err(Failure::new(Code::Type, "test string"));
    };
    // Changed in place, it keeps what it holds.
    Shared::get_mut(&mut storage).map(|text| text.append(&Text::new("y")));
    assert_eq!(
        text().retain(&grant).map_err(|e| e.code),
        Err(Code::TooLong)
    );
    drop(storage);
    text().retain(&grant)?;
    Ok(())
}

//! The language, run as a host runs it: read and run a word at a time,
//! through every suspension.
use crate::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
/// A host with one value of its own, a ticket, and its words: `ticket`
/// (-- t), which hands one out at once; `ask` (n -- n*10), which suspends
/// for the host to answer (with a failure, `negative`, for n below 0);
/// `nope`, which fails with `unused`, a code of the language's it declares
/// its words raise; `quiet`, which gives nothing; and `no-text`, a ticket
/// that refuses to be text. It keeps what was asked and printed.
#[derive(Clone, Debug, PartialEq)]
struct Ticket(i64);

impl AppValue for Ticket {
    fn kind(&self) -> &'static str {
        "a ticket"
    }
    fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), Failure> {
        if self.0 == i64::MIN {
            return Err(Failure::new(
                Code::Host("refused"),
                "the host refused text conversion",
            ));
        }
        out.write_str(self.kind())
            .map_err(|_| Failure::new(Code::Host("refused"), "unwritten"))
    }
}

#[derive(Default)]
struct Toy {
    asked: Vec<i64>,
    printed: Vec<String>,
    /// The files `include` and `require` read, by path.
    files: HashMap<PathBuf, String>,
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

    fn write(&mut self, text: &str) -> Result<(), Failure> {
        self.printed.push(text.to_owned());
        Ok(())
    }

    fn resolve(&mut self, path: &str, from: Option<&Path>) -> Result<PathBuf, String> {
        let base = from.and_then(Path::parent).unwrap_or(Path::new("/"));
        let path = base.join(path);
        match self.files.contains_key(&path) {
            true => Ok(path),
            false => Err("no such file".to_owned()),
        }
    }

    fn read(&mut self, path: &Path) -> Result<String, String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| "gone".to_owned())
    }
}

/// A machine with the standard library read.
fn with_stdlib() -> Machine<Ticket> {
    let mut machine = Machine::new(vocabulary());
    let read = machine.library("stdlib.fux", STDLIB, &mut Toy::default());
    assert!(read.is_ok(), "the standard library reads: {read:?}");
    machine
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
        let mut machine = with_stdlib();
        machine.read(token::arguments(&["str", text, "quote"]), None);
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
    Ok(())
}

fn vocabulary() -> Vocabulary {
    let mut words = Vocabulary::default();
    let added = (words.add("ticket", "-- ticket"))
        .and_then(|_| words.add("ask", "any -- any"))
        .and_then(|_| words.add("nope", "--"))
        .and_then(|_| words.add("quiet", "--"))
        .and_then(|_| words.add("no-text", "-- ticket"))
        .and_then(|_| words.raises(Code::Host("not_a_number")))
        .and_then(|()| words.raises(Code::Host("refused")))
        .and_then(|()| words.raises(Code::Host("negative")))
        .and_then(|()| words.raises(Code::Host("far")))
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
    run_at(text, budget, &mut Toy::default())
}

/// `run_by`, at `host`, which keeps what it was asked and printed.
fn run_at(text: &str, budget: u64, host: &mut Toy) -> Result<Vec<Value<Ticket>>, Error> {
    let mut machine = compile(text)?;
    loop {
        match machine.run(host, budget)? {
            Outcome::Finished => return Ok(machine.stack().to_vec()),
            // A failure far below 0 carries its number, for a handler.
            Outcome::Suspended(n) if n <= -1000 => {
                let failure = Failure::new(Code::Host("far"), format!("{n} is far below 0"));
                machine.raise(failure, Some(Value::Int(n)))?;
            }
            Outcome::Suspended(n) if n < 0 => machine.raise(
                Failure::new(Code::Host("negative"), format!("{n} is below 0")),
                None,
            )?,
            Outcome::Suspended(n) => machine.push(Value::Int(n.saturating_mul(10))),
            Outcome::Paused => {}
        }
    }
}

/// A machine with `text`, a file named `t`, to read.
fn compile(text: &str) -> Result<Machine<Ticket>, Error> {
    let mut machine = with_stdlib();
    machine.read(token::source("t", text)?, None);
    Ok(machine)
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
        assert_eq!(code(run(source)), Some("refused"), "{source}");
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
fn the_calls_under_way_show_who_called() -> Result<(), Error> {
    let mut machine = compile("define inner 1 ask end\ndefine outer inner end\nouter")?;
    let outcome = machine.run(&mut Toy::default(), 100)?;
    assert_eq!(outcome, Outcome::Suspended(1));
    let calls: Vec<String> = machine
        .calls()
        .map(|t| format!("{}:{}", t.place, t.text))
        .collect();
    assert_eq!(
        calls,
        ["t:3:outer", "t:2:inner", "t:1:ask"],
        "outer's frame went to inner, and its call stays"
    );
    Ok(())
}

#[test]
fn a_paused_loop_shows_its_own_step() -> Result<(), Error> {
    for budget in 1..=12 {
        let mut machine = compile("str x print\nbegin 0 1 eq until")?;
        let outcome = machine.run(&mut Toy::default(), budget)?;
        assert_eq!(outcome, Outcome::Paused);
        let last = machine
            .calls()
            .last()
            .map(|t| format!("{}:{}", t.place, t.text));
        let in_loop = ["t:2:begin", "t:2:0", "t:2:1", "t:2:eq", "t:2:until"];
        let at_print = last.as_deref().is_some_and(|l| l.starts_with("t:1:"));
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
    let down = "define down dup 0 eq if else 1 sub recurse then end 5000 down";
    assert_eq!(run(down)?, ints(&[0]), "5000 deep, the call limit 100");
    // Last in an `if` inside an `if`: the jumps that end both lead out.
    let nested = "define f dup 0 gt if dup 1 gt if 1 sub recurse else 1 sub recurse then else drop then end 5000 f";
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

/// A mistake is found as its word is read, the words before it having
/// acted: an application word was asked.
#[test]
fn mistakes_are_found_as_they_are_read() {
    for (text, expected) in [
        ("1 ask frobnicate", "unknown_word"),
        ("1 ask true if", "unclosed"),
        ("1 ask define x 1", "unclosed"),
        ("1 ask then", "unmatched"),
        ("1 ask end", "unmatched"),
        ("1 ask begin 1 then", "unmatched"),
        ("1 ask define drop 1 end", "bad_name"),
        ("1 ask define 12 end", "bad_name"),
        ("1 ask define if end", "bad_name"),
        ("1 ask true if define x end then", "not_top_level"),
        ("1 ask define x define y end end", "not_top_level"),
        ("1 ask str", "missing_token"),
        ("1 ask 99999999999999999999", "overflow"),
    ] {
        let mut host = Toy::default();
        assert_eq!(code(run_at(text, 1, &mut host)), Some(expected), "{text}");
        assert_eq!(host.asked, [1], "{text}: the word before it was asked");
    }
    let quote = compile("'unclosed").err().map(|e| e.code);
    assert_eq!(quote, Some(Code::OpenQuote));
}

/// Each word checks its values as it runs: what ran before a wrong kind or
/// too few values took effect (an application word was asked, a line
/// printed), and the failure is the word's.
#[test]
fn a_step_fails_as_it_runs_after_what_came_before_took_effect() {
    for (text, expected) in [
        ("1 ask str abc 0 1 slice 1 add", "type"),
        ("1 ask str abc str b at", "type"),
        ("1 ask str a 1 push", "type"),
        ("1 ask str a 0 1 put", "type"),
        ("1 ask drop define f fail end f", "stack_underflow"),
        (
            "define f if str x fail else drop drop then end 1 ask false f",
            "stack_underflow",
        ),
        ("1 ask drop str x 5 pad", "stack_underflow"),
        ("1 ask drop true or", "stack_underflow"),
        (
            "define f begin dup true until end 1 ask drop f",
            "stack_underflow",
        ),
        (
            "define d dup drop list recurse end end 1 ask drop d",
            "stack_underflow",
        ),
        ("1 ask ticket ticket ticket 1 add", "type"),
    ] {
        let mut host = Toy::default();
        let ended = run_at(text, u64::MAX, &mut host);
        assert_eq!(code(ended), Some(expected), "{text}");
        assert_eq!(host.asked, [1], "{text}: the word before it was asked");
    }
    let mut host = Toy::default();
    let ended = run_at("str one print str a 1 add str two print", 1, &mut host);
    assert_eq!(code(ended), Some("type"));
    assert_eq!(host.printed, ["one\n"], "printed before, and nothing after");
}

/// A definition is found from its `end` on: a word calls one defined
/// before it, and itself with `recurse`. A failed definition leaves the
/// version before it.
#[test]
fn a_definition_is_found_from_its_end() -> Result<(), Error> {
    assert_eq!(code(run("define peek x end 1 let x")), Some("unknown_word"));
    let early = "define early later end define later 1 end early";
    assert_eq!(code(run(early)), Some("unknown_word"));
    assert_eq!(code(run("early define early 1 end")), Some("unknown_word"));
    assert_eq!(code(run("define self self end")), Some("unknown_word"));
    let down = "define down dup 0 gt if 1 sub recurse then end 5 down";
    assert_eq!(run(down)?, ints(&[0]));
    let failed = "define a 1 end str 'define a 2 frob end' let source
        str unknown_word try source evaluate catch drop drop end a";
    assert_eq!(evaluated(failed)?, ints(&[1]));
    Ok(())
}

/// Calls bind early: code keeps the version of each word it was compiled
/// against, and a redefinition changes only what is compiled after it.
/// A redefinition may call the version before it.
#[test]
fn a_redefinition_changes_only_what_follows() -> Result<(), Error> {
    let program = "define a 1 end define b a end define a 2 end b a";
    assert_eq!(run(program)?, ints(&[1, 2]));
    let twice = "define down 1 sub end define down down down end 10 down";
    assert_eq!(run(twice)?, ints(&[8]));
    let longer = "define a 1 2 drop end define b a end define a 3 4 drop end b a";
    assert_eq!(run(longer)?, ints(&[1, 3]));
    assert_eq!(run("define count 7 end count")?, ints(&[7]));
    Ok(())
}

#[test]
fn a_deep_stack_costs_no_more_per_word() {
    let started = std::time::Instant::now();
    let filled = run("0 begin 1 swap 1 add dup 3000 eq until drop");
    assert_eq!(filled.map(|stack| stack.len()), Ok(3000));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
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
        Some("stack_underflow: t:1: roll: takes 9223372036854775808 values, and the stack has 1")
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
        let mut machine = compile("0 ask 0 swap each drop 1 add end")?;
        let mut host = Toy::default();
        assert_eq!(machine.run(&mut host, u64::MAX)?, Outcome::Suspended(0));
        machine.push(Value::str(
            std::iter::repeat_n(c, 100_000).collect::<String>(),
        ));
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
    Ok(())
}

/// A host with kinds of its own: a ticket, and a draft, which must be
/// used. `ticket` (-- ticket); `draft` (-- draft).
#[derive(Clone, Debug, PartialEq)]
enum Paper {
    Ticket,
    Draft,
}

impl AppValue for Paper {
    fn kind(&self) -> &'static str {
        match self {
            Paper::Ticket => "a ticket",
            Paper::Draft => "a draft",
        }
    }
}

struct Desk;

impl Host for Desk {
    type Value = Paper;
    type Request = ();
    fn call(
        &mut self,
        word: usize,
        _args: Args<'_, Paper>,
        out: &mut Vec<Value<Paper>>,
    ) -> Result<Done<()>, Failure> {
        out.push(Value::App(match word {
            0 => Paper::Ticket,
            _ => Paper::Draft,
        }));
        Ok(Done::Now)
    }

    fn write(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

#[test]
fn a_value_that_must_be_used_fails_the_end_it_is_left_at() -> Result<(), Error> {
    let mut words = Vocabulary::default();
    words.must_use("draft", "a draft nothing issues")?;
    words.add("ticket", "-- ticket")?;
    words.add("draft", "-- draft")?;
    let said = |text: &str| -> Result<String, Error> {
        let mut machine = Machine::new(words.clone());
        machine.read(token::source("t", text)?, None);
        let ran = machine.run(&mut Desk, u64::MAX);
        Ok(ran.err().map(|e| e.to_string()).unwrap_or_default())
    };
    assert_eq!(said("ticket")?, "");
    assert_eq!(said("draft")?, "unused: t:1: draft: a draft nothing issues");
    // Inside a list, as its value can be told only once it is there.
    assert_eq!(said("list draft end")?, "");
    assert_eq!(
        said("list draft end 0 at")?,
        "unused: t:1: at: a draft nothing issues"
    );
    Ok(())
}

#[test]
fn each_name_and_code_means_one_thing() {
    let mut words = Vocabulary::default();
    assert_eq!(words.add("w", "--").map_err(|e| e.message), Ok(0));
    let said: Vec<String> = [
        words.add("odd", "pane").map(drop),
        words.add("w", "--").map(drop),
        words.add("drop", "--").map(drop),
        words.add("if", "--").map(drop),
        words.raises(Code::Host("refused")),
        words.raises(Code::Host("refused")),
        words.must_use("pane", "a pane"),
        words.must_use("pane", "a pane"),
    ]
    .into_iter()
    .filter_map(Result::err)
    .map(|e| format!("{}: {}", e.code.name(), e.message))
    .collect();
    assert_eq!(
        said,
        [
            "contract: odd: no -- in \"pane\"",
            "contract: w: a word already",
            "contract: drop: a word of the core",
            "contract: if: a word of the core",
            "contract: refused: declared already",
            "contract: pane: must be used already",
        ]
    );
}

#[test]
fn a_library_says_what_each_word_does() {
    let mut machine = Machine::<Ticket>::new(vocabulary());
    let docs = machine
        .library("stdlib.fux", STDLIB, &mut Toy::default())
        .unwrap_or_default();
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
    let docs = machine
        .library("l", library, &mut Toy::default())
        .unwrap_or_default();
    let code: Vec<&str> = docs.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(code, ["define double\n  dup add\nend"]);
    // A comment after a token is that line's, not the definition's below.
    let silent = machine.library(
        "l",
        "# -- : one\n1 drop # -- : said\ndefine x end",
        &mut Toy::default(),
    );
    assert_eq!(silent.map_err(|e| e.to_string()).map(drop), Err(
        "contract: l:3: define: a library's definition says what it does on the line just above it: # EFFECT : ABOUT".to_owned()
    ));
    // Refused, it is as it was.
    machine.read(token::source("t", "x").unwrap_or_default(), None);
    let after = machine.run(&mut Toy::default(), u64::MAX);
    assert_eq!(after.map_err(|e| e.code), Err(Code::UnknownWord));
    // A library is whole: it leaves no definition open, and is read on a
    // machine at rest.
    let mut machine = Machine::<Ticket>::new(vocabulary());
    let open = machine.library("l", "# -- : open\ndefine open", &mut Toy::default());
    assert_eq!(open.map_err(|e| e.code), Err(Code::Unclosed));
    let closing = machine.library("l", "# -- : fake\n1 2 end", &mut Toy::default());
    assert_eq!(closing.map_err(|e| e.code), Err(Code::Unmatched));
    machine.read(
        token::source("t", "define actual").unwrap_or_default(),
        None,
    );
    let busy = machine.library("l", "# -- : one\ndefine one 1 end", &mut Toy::default());
    assert_eq!(busy.map_err(|e| e.code), Err(Code::Contract));
}

/// Words that call each other do so through a value holding an xt, late
/// bound where the program chooses (D-11).
#[test]
fn words_may_call_each_other_through_an_xt() -> Result<(), Error> {
    let parity = "0 let odd\n\
        define is-even dup 0 eq if drop true else 1 sub odd execute then end\n\
        define is-odd dup 0 eq if drop false else 1 sub is-even then end\n\
        tick is-odd to odd\n";
    assert_eq!(
        run(&format!("{parity}10 is-even 7 is-even"))?,
        [Value::Bool(true), Value::Bool(false)]
    );
    // Deep, without Rust recursion.
    let mut machine = compile(&format!("{parity}300001 is-even"))?;
    assert_eq!(
        machine.run(&mut Toy::default(), u64::MAX)?,
        Outcome::Finished
    );
    assert_eq!(machine.stack(), [Value::Bool(false)]);
    let descent = "0 let expr-xt\n\
        define factor\n\
          let i let s\n\
          s i at 40 eq if s i 1 add expr-xt execute 1 add else s i at 48 sub i 1 add then\n\
        end\n\
        define term\n\
          let i let s s i factor let i let v\n\
          begin i s len lt if s i at 42 eq else false then\n\
            if s i 1 add factor let i v mul let v false else true then until\n\
          v i\n\
        end\n\
        define expr\n\
          let i let s s i term let i let v\n\
          begin i s len lt if s i at 43 eq else false then\n\
            if s i 1 add term let i v add let v false else true then until\n\
          v i\n\
        end\n\
        tick expr to expr-xt\n";
    assert_eq!(
        run(&format!("{descent}str 2*(3+4)+1 0 expr drop"))?,
        ints(&[15])
    );
    // A kind mistake through the group fails as it runs.
    assert_eq!(code(run(&format!("{parity}3 is-odd 1 add"))), Some("type"));
    Ok(())
}

#[test]
fn a_definition_may_hide_a_library_word() -> Result<(), Error> {
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

/// A machine's copy serves a program of its own: the dictionary, and the
/// values as they were; nothing the copy does reaches the original.
#[test]
fn a_copy_serves_a_program_of_its_own() -> Result<(), Error> {
    let mut config = compile("5 let n define five n end define six 6 end")?;
    assert_eq!(
        config.run(&mut Toy::default(), u64::MAX)?,
        Outcome::Finished
    );
    let ran = |text: &str| -> Result<Vec<Value<Ticket>>, Error> {
        let mut machine = config.clone();
        machine.read(token::source("b", text)?, None);
        machine.run(&mut Toy::default(), u64::MAX)?;
        Ok(machine.stack().to_vec())
    };
    assert_eq!(ran("six five n")?, ints(&[6, 5, 5]));
    assert_eq!(ran("7 to n five")?, ints(&[7]));
    assert_eq!(
        ran("five")?,
        ints(&[5]),
        "the original's value is as it was"
    );
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
    let deep = "define deep dup 0 eq if str bottom fail then 1 sub recurse end
        define safe let n str fail try n deep catch drop end end";
    assert_eq!(
        run(&format!("{deep} 50 safe 7 3 safe"))?,
        [strs(&["bottom"]), ints(&[7]), strs(&["bottom"])].concat(),
        "the body's call is not in its definition's last place"
    );
    let recursion = "define down dup 2000 eq if str bottom fail then 1 add recurse 1 end
        str fail try 0 down catch nip end";
    assert_eq!(run(recursion)?, strs(&["fail"]), "2000 calls undone");
    let blocks = "list 1 str fail try 2 list 3 4 end each dup 4 eq if str four fail then end
        catch drop end 5 end list 6 7 end each end";
    let list = List::new([ints(&[1]), strs(&["four"]), ints(&[5])].concat());
    let list = Value::List(std::rc::Rc::new(list));
    assert_eq!(run(blocks)?, [vec![list], ints(&[6, 7])].concat());
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
fn a_try_is_checked_as_it_is_read() {
    for (text, said) in [
        ("try 1 catch end", "bad_codes: t:1: try: takes the codes"),
        ("1 try 1 catch end", "bad_codes: t:1: try: takes the codes"),
        (
            "str negativ try 1 catch end",
            "bad_codes: t:1: try: negativ is no code a word raises",
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
        let error = run(text).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(error.starts_with(said), "{text}: {error}");
    }
    // Its body starts with an empty stack, and the stack after it holds
    // what the body or the handler left, as it runs.
    for (text, said) in [
        (
            "1 str fail try drop catch end",
            "stack_underflow: t:1: drop: takes 1 value, and the stack has 0 (inside try … catch",
        ),
        (
            "str fail try 1 catch drop drop end add",
            "stack_underflow: t:1: add: takes 2 values, and the stack has 1",
        ),
        ("str fail try str a catch drop end 1 add", "type: t:1: add:"),
    ] {
        let error = run(text).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(error.starts_with(said), "{text}: {error}");
    }
    // One the host's words raise is caught, though plain fuxth's never is.
    assert_eq!(code(run("str unused try 1 catch end")), None);
    let mut plain = Machine::<Ticket>::new(Vocabulary::default());
    let tokens = token::source("t", "str unused try 1 catch end").unwrap_or_default();
    plain.read(tokens, None);
    let plain = plain.run(&mut Toy::default(), u64::MAX);
    assert_eq!(plain.err().map(|e| e.code), Some(Code::BadCodes));
    let mut words = Vocabulary::default();
    assert!(
        words.raises(Code::Host("fail")).is_err(),
        "the language's own"
    );
    assert!(words.raises(Code::Host("two words")).is_err());
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
        let mut machine = compile(&source)?;
        let error = machine.run(&mut Toy::default(), u64::MAX).err();
        assert!(error.is_some_and(|e| e.code == Code::Type && e.message.ends_with(message)));
    }
    Ok(())
}

#[test]
fn every_core_word_documents_its_effect_and_meaning() {
    for builtin in Builtin::ALL {
        assert!(builtin.about().len() > 8, "{}", builtin.name());
        // pick and roll take a count, then as many values as it says; an
        // immediate word takes what it takes while it runs.
        if matches!(builtin, Builtin::Pick | Builtin::Roll) || builtin.immediate() {
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
                || matches!(
                    builtin,
                    Builtin::Drop
                        | Builtin::Print
                        | Builtin::Write
                        | Builtin::Emit
                        | Builtin::Fail
                        | Builtin::Execute
                        | Builtin::Immediate
                        | Builtin::StartCompiling
                        | Builtin::Evaluate
                ),
            "{}",
            builtin.name()
        );
    }
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
    assert_eq!(run("1 let x define x 2 end x")?, ints(&[2]));
    assert_eq!(run("define f 1 end 2 let f f")?, ints(&[2]));
    Ok(())
}

#[test]
fn sequence_mutation_preserves_shared_values() -> Result<(), Error> {
    let list = |items: Vec<Value<Ticket>>| Value::List(std::rc::Rc::new(crate::List::new(items)));
    assert_eq!(run("list 1 end 2 push")?, vec![list(ints(&[1, 2]))]);
    let both = vec![list(ints(&[1, 2])), list(ints(&[9, 2]))];
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
fn the_core_at_its_edges() -> Result<(), Error> {
    let s = |text: &str| Value::<Ticket>::str(text);
    assert_eq!(code(run("str abc 3 at")), Some("range"));
    assert_eq!(code(run("list 1 end 1 at")), Some("range"));
    assert_eq!(
        run("str héllo 1 at str héllo 1 4 slice str héllo len")?,
        [Value::Int(233), s("éll"), Value::Int(5)]
    );
    assert_eq!(run("str naïve 2 str i put")?, [s("naive")]);
    assert_eq!(run("str abc 1 str XY put")?, [s("aXYc")]);
    assert_eq!(run("str é first 233 char")?, [Value::Int(233), s("é")]);
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
        ints(&[98, 233])
    );
    Ok(())
}

#[test]
fn the_machine_says_what_and_where() -> Result<(), Error> {
    assert_eq!(code(run("str x ask")), Some("not_a_number"));
    let said = |text: &str| run(text).err().map(|e| e.to_string()).unwrap_or_default();
    assert_eq!(
        said("ticket 1 add"),
        "type: t:1: add: add takes an integer, not a ticket"
    );
    // Inside a definition, at its step; a place of several, by what it takes.
    assert_eq!(
        said("define f str x add end\n1 f"),
        "type: t:1: add: add takes an integer, not a string"
    );
    assert_eq!(
        said("1 2 3 slice"),
        "type: t:1: slice: slice takes a string or a list, not an integer"
    );
    // Arguments are placed by their number.
    let mut machine = with_stdlib();
    machine.read(token::arguments(&["str", "a", "1", "add"]), None);
    let error = machine.run(&mut Toy::default(), u64::MAX).err();
    assert_eq!(
        error.map(|e| e.to_string()).as_deref(),
        Some("type: argument 4: add: add takes an integer, not a string")
    );
    assert_eq!(
        said("1 ticket lt"),
        "type: t:1: lt: lt compares two integers, two strings or two lists, not an integer and a ticket; `string` makes it text"
    );
    assert_eq!(
        said("ticket if 1 then"),
        "type: t:1: if: takes a boolean (from eq or lt) or an integer, not a ticket"
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
    let mut machine = compile("1\n  2 ask")?;
    assert_eq!(
        machine.run(&mut Toy::default(), 100)?,
        Outcome::Suspended(2)
    );
    let at = machine.at().map(|t| format!("{}: {}", t.place, t.text));
    assert_eq!(at.as_deref(), Some("t:2: ask"), "the word suspended at");
    Ok(())
}

/// A list nests as deep as memory lets it, however it is made; one 100,000
/// deep is compared, ordered, written and let go, all without recursion,
/// so none overflows the stack.
#[test]
fn lists_nest_as_deep_as_memory_lets_them() -> Result<(), Error> {
    for source in [
        "list end let l 0 begin list l end let l 1 add dup 70 eq until drop l len",
        "list end let l 0 begin list end l push let l 1 add dup 70 eq until drop l len",
        "list 0 end let l 0 begin list 0 end 0 l put let l 1 add dup 70 eq until drop l len",
    ] {
        assert_eq!(run_by(source, u64::MAX)?, ints(&[1]), "{source}");
    }
    let deep = "list end let l list end let m
        0 begin list l end let l list m end let m 1 add dup 100000 eq until drop
        l m eq l m lt l string len";
    let truth = |b| Value::<Ticket>::Bool(b);
    assert_eq!(
        run_by(deep, u64::MAX)?,
        [truth(true), truth(false), Value::Int(0)]
    );
    Ok(())
}

#[test]
fn long_chains_of_definitions_run() -> Result<(), Error> {
    let chain: String = (1..20_000)
        .map(|i: u32| format!("define f{i} f{} end\n", i.saturating_sub(1)))
        .collect();
    let chained = format!("define f0 1 add end\n{chain}5 f19999");
    assert_eq!(run(&chained)?, ints(&[6]));
    Ok(())
}

/// The vocabulary, `evaluate` enabled.
fn evaluating() -> Vocabulary {
    let mut words = vocabulary();
    words.evaluate();
    words
}

/// `text` run as `run` runs it, `evaluate` enabled.
fn evaluated(text: &str) -> Result<Vec<Value<Ticket>>, Error> {
    let mut machine = Machine::new(evaluating());
    machine.library("stdlib.fux", STDLIB, &mut Toy::default())?;
    machine.read(token::source("t", text)?, None);
    loop {
        match machine.run(&mut Toy::default(), u64::MAX)? {
            Outcome::Finished => return Ok(machine.stack().to_vec()),
            Outcome::Suspended(n) => machine.push(Value::Int(n.saturating_mul(10))),
            Outcome::Paused => {}
        }
    }
}

#[test]
fn immediate_words_shape_the_program() -> Result<(), Error> {
    let unless = "define unless postpone not postpone if end immediate
        define check unless str no print then end";
    let mut host = Toy::default();
    run_at(&format!("{unless} false check true check"), 1, &mut host)?;
    assert_eq!(host.printed, ["no\n"]);
    let five = "define five stop-compiling 2 3 add start-compiling literal end five";
    assert_eq!(run(five)?, ints(&[5]));
    let state = "compiling define f stop-compiling compiling start-compiling literal end f";
    assert_eq!(run(state)?, [Value::Bool(false), Value::Bool(false)]);
    let plus = "define plus postpone add end immediate define f plus end 3 4 f";
    assert_eq!(run(plus)?, ints(&[7]));
    // A word that reads the word after it.
    let next = "define next next-word end next hello";
    assert_eq!(run(next)?, strs(&["hello"]));
    assert_eq!(
        code(run("define next next-word end next")),
        Some("missing_token")
    );
    for text in [
        "1 literal",
        "recurse",
        "leave-definition",
        "postpone dup",
        "start-compiling",
        "true if recurse then",
    ] {
        assert_eq!(code(run(text)), Some("not_top_level"), "{text}");
    }
    Ok(())
}

#[test]
fn an_xt_runs_the_word_it_is() -> Result<(), Error> {
    assert_eq!(run("3 tick dup execute")?, ints(&[3, 3]));
    assert_eq!(run("code 3 4 add end execute")?, ints(&[7]));
    assert_eq!(run("define f code 1 add end end 5 f execute")?, ints(&[6]));
    assert_eq!(run("define g tick dup end 2 g execute")?, ints(&[2, 2]));
    assert_eq!(run("9 tick dup tick execute execute")?, ints(&[9, 9]));
    assert_eq!(run("2 tick ask execute 1 add")?, ints(&[21]));
    assert_eq!(
        run("4 str dup find drop execute str nothing-here find")?,
        [
            ints(&[4, 4]),
            strs(&["nothing-here"]),
            vec![Value::Bool(false)]
        ]
        .concat()
    );
    let truth = |b| Value::<Ticket>::Bool(b);
    let versions = "tick dup tick dup eq tick dup define dup 0 pick end tick dup eq";
    assert_eq!(run(versions)?, [truth(true), truth(false)]);
    assert_eq!(run("tick dup kind")?, strs(&["xt"]));
    assert_eq!(code(run("5 execute")), Some("type"));
    assert_eq!(code(run("tick nothing-here")), Some("unknown_word"));
    Ok(())
}

#[test]
fn let_names_are_set_with_to() -> Result<(), Error> {
    assert_eq!(run("1 let n 2 to n n")?, ints(&[2]));
    let bump = "1 let n define bump n 1 add to n end bump bump n";
    assert_eq!(run(bump)?, ints(&[3]));
    assert_eq!(run("define f let a 5 to a a end 1 f")?, ints(&[5]));
    assert_eq!(code(run("1 to nothing")), Some("bad_name"));
    assert_eq!(code(run("1 to dup")), Some("bad_name"));
    Ok(())
}

#[test]
fn leave_definition_returns_leaving_its_loops() -> Result<(), Error> {
    let big = "define first-big each dup 10 gt if leave-definition then drop end 0 end
        define pick-big list 3 12 4 end first-big end
        list 1 2 end each drop pick-big end";
    assert_eq!(run(big)?, ints(&[12, 12]));
    Ok(())
}

#[test]
fn marks_hold_constructs_open_while_compiling() -> Result<(), Error> {
    let nested = run("define f list 1 list 2 end end end f")?;
    let inner = Value::List(std::rc::Rc::new(List::new(ints(&[2]))));
    let outer = List::new([ints(&[1]), vec![inner]].concat());
    assert_eq!(nested, [Value::List(std::rc::Rc::new(outer))]);
    let said = |text: &str| run(text).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        said("define f true if end")
            .starts_with("unmatched: t:1: end: the open block is if … then")
    );
    let other = "define f code stop-compiling swap start-compiling end";
    assert!(said(other).ends_with("the open block is define … end, of other code"));
    let kind = "define f stop-compiling dup kind start-compiling literal end f";
    assert_eq!(run(kind)?, strs(&["mark"]));
    // A failure no try catches drops the code being compiled and its
    // marks; the values stay.
    for text in ["1 define x 2", "1 define x 2 frob end"] {
        let mut machine = compile(text)?;
        assert!(
            machine.run(&mut Toy::default(), u64::MAX).is_err(),
            "{text}"
        );
        assert_eq!(machine.stack(), ints(&[1]).as_slice(), "{text}");
    }
    Ok(())
}

#[test]
fn top_level_constructs_run_as_soon_as_they_close() -> Result<(), Error> {
    let mut host = Toy::default();
    let text = "str a print true if str b print then str c print 1 ask drop";
    run_at(text, 1, &mut host)?;
    assert_eq!(host.printed, ["a\n", "b\n", "c\n"]);
    assert_eq!(run("list 1 2 end each 10 mul end")?, ints(&[10, 20]));
    assert_eq!(run("3 let n n 0 gt if n 1 sub let n then n")?, ints(&[2]));
    Ok(())
}

#[test]
fn flags_are_booleans_or_integers_and_arithmetic_wraps() -> Result<(), Error> {
    assert_eq!(run("1 if 5 then 0 if 6 then -1 if 7 then")?, ints(&[5, 7]));
    let truth = |b| Value::<Ticket>::Bool(b);
    assert_eq!(run("0 not 5 not")?, [truth(true), truth(false)]);
    assert_eq!(
        run("9223372036854775807 1 add -9223372036854775808 1 sub")?,
        ints(&[i64::MIN, i64::MAX])
    );
    assert_eq!(run("-9223372036854775808 -1 div")?, ints(&[i64::MIN]));
    assert_eq!(code(run("1 0 div")), Some("div_by_zero"));
    assert_eq!(code(run("true 1 add")), Some("type"));
    Ok(())
}

#[test]
fn characters_are_integers() -> Result<(), Error> {
    assert_eq!(run("str abc 0 at str abc first")?, ints(&[97, 97]));
    let codes = List::new(ints(&[104, 233, 108]));
    assert_eq!(
        run("list str hél each end end")?,
        [Value::List(std::rc::Rc::new(codes))]
    );
    let mut host = Toy::default();
    run_at("72 emit 105 emit str ! write 10 emit", 1, &mut host)?;
    assert_eq!(host.printed.concat(), "Hi!\n");
    assert_eq!(code(run("-1 emit")), Some("range"));
    Ok(())
}

#[test]
fn evaluate_reads_a_string_only_where_the_host_enables_it() -> Result<(), Error> {
    assert_eq!(code(run("str '1 2 add' evaluate")), Some("unknown_word"));
    assert_eq!(evaluated("str '1 2 add' evaluate 3 mul")?, ints(&[9]));
    let define = "str 'define sq dup mul end' evaluate 4 sq";
    assert_eq!(evaluated(define)?, ints(&[16]));
    // Inside a word, its words run before the word goes on.
    let inside = "define e evaluate 10 add end str '1 ask' e";
    assert_eq!(evaluated(inside)?, ints(&[20]));
    let quote = evaluated("str \"it's\" evaluate").err().map(|e| e.code);
    assert_eq!(quote, Some(Code::OpenQuote));
    Ok(())
}

#[test]
fn include_and_require_read_files_through_the_host() -> Result<(), Error> {
    let mut host = Toy::default();
    for (path, text) in [
        ("/lib/a.fux", "require b.fux define a b 1 add end"),
        ("/lib/b.fux", "define b 41 end"),
        ("/lib/loop.fux", "require loop.fux"),
    ] {
        host.files.insert(PathBuf::from(path), text.to_owned());
    }
    let text = "require lib/a.fux require lib/b.fux a include lib/b.fux b";
    assert_eq!(run_at(text, 1, &mut host)?, ints(&[42, 41]));
    let cycle = run_at("require lib/loop.fux", u64::MAX, &mut host).err();
    assert!(cycle.is_some_and(|e| e.code == Code::Require && e.message.contains("cycle")));
    let missing = run_at("require nowhere.fux", u64::MAX, &mut host).err();
    assert_eq!(missing.map(|e| e.code), Some(Code::Require));
    // A chain however long reads as a stack of sources, not a recursion.
    for i in 0..20_000 {
        let next = format!("require c{}.fux", i + 1);
        host.files.insert(PathBuf::from(format!("/c{i}.fux")), next);
    }
    host.files
        .insert(PathBuf::from("/c20000.fux"), "7".to_owned());
    assert_eq!(run_at("require c0.fux", u64::MAX, &mut host)?, ints(&[7]));
    Ok(())
}

/// An xt runs only in the machine that made it (or a copy made after it):
/// its calls name versions by index, which mean other code in another
/// machine, so another refuses it rather than run those.
#[test]
fn an_xt_of_another_machine_is_refused() -> Result<(), Error> {
    let mut a = compile("define a 1 2 add end define b a end tick b")?;
    assert_eq!(a.run(&mut Toy::default(), u64::MAX)?, Outcome::Finished);
    let xt = a.stack().last().cloned().unwrap_or(Value::Int(0));
    let mut copy = a.clone();
    copy.read(token::source("t", "execute")?, None);
    assert_eq!(copy.run(&mut Toy::default(), u64::MAX)?, Outcome::Finished);
    assert_eq!(copy.stack().last(), Some(&Value::Int(3)), "a copy made after it runs it");
    let mut b = compile("define c 5 end")?;
    assert_eq!(b.run(&mut Toy::default(), u64::MAX)?, Outcome::Finished);
    b.push(xt);
    b.read(token::source("t", "execute")?, None);
    let refused = b.run(&mut Toy::default(), 100_000).err();
    assert_eq!(refused.map(|e| e.code), Some(Code::Type));
    Ok(())
}

/// Lists in code in lists, however deep, are let go without a recursion.
#[test]
fn lists_and_code_nested_alternately_are_let_go_on_a_small_stack() -> Result<(), String> {
    let nest = "code stop-compiling swap start-compiling literal end list end swap push\n";
    let text = format!("list end\n{}drop", nest.repeat(20_000));
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || -> Result<(), String> {
            let mut machine = compile(&text).map_err(|e| e.to_string())?;
            let ran = machine.run(&mut Toy::default(), u64::MAX);
            assert_eq!(ran.map_err(|e| e.to_string())?, Outcome::Finished);
            assert!(machine.stack().is_empty());
            drop(machine);
            Ok(())
        })
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|_| "letting the values go overflowed the stack".to_owned())?
}

/// A number or `str` written before a place a jump lands on is no count of
/// the word after it, nor a `try`'s codes: each way through takes its own.
#[test]
fn a_count_before_a_jump_target_is_not_folded() -> Result<(), Error> {
    let branches = "define f if 3 else 2 then pick end";
    assert_eq!(run(&format!("{branches} 1 2 3 4 true f"))?, ints(&[1, 2, 3, 4, 1]));
    assert_eq!(run(&format!("{branches} 1 2 3 4 false f"))?, ints(&[1, 2, 3, 4, 2]));
    // Each pass of the loop takes its count from the stack.
    let looped = "define g 0 begin pick 1 add nip dup 3 eq 0 swap until drop end 1 g";
    let mut machine = compile(looped)?;
    assert_eq!(machine.run(&mut Toy::default(), 100_000)?, Outcome::Finished);
    assert_eq!(machine.stack(), ints(&[3]).as_slice());
    let codes = "define h if str fail else str timeout then try 1 catch end end";
    assert_eq!(code(run(codes)), Some("bad_codes"));
    Ok(())
}

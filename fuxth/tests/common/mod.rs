//! What the tests of whole programs share: random programs, and fuxth run
//! as a host runs it.
#![allow(dead_code, reason = "each test file uses its own part")]
use fuxth::{
    AppValue, Args, Done, Error, Failure, Host, Machine, Outcome, Place, STDLIB, Value, Vocabulary,
    token,
};

/// Steps a run takes before it counts as endless.
pub const STEPS: u64 = 50_000;

/// A small random number generator (xorshift), seeded, so a failure names
/// the seed that makes it again.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n.max(1)).unwrap_or(1);
        usize::try_from(self.next().checked_rem(n).unwrap_or(0)).unwrap_or(0)
    }
    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
    pub fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items.get(self.below(items.len())).copied().unwrap_or("dup")
    }
}

/// The words programs may use beyond the standard library: `ask` (n -- n*10), the
/// host's, answered later, as a server's answer is (past an integer, with
/// the failure `overflow`; given no integer, it fails at once,
/// `not_a_number`); `maybe` (n -- [n]),
/// which gives n back only if it is above 0, defined in fuxth (`MAYBE`),
/// as no host word gives a count known only as it runs.
pub const HOST_WORDS: [&str; 2] = ["ask", "maybe"];

/// `maybe`, read after the standard library.
const MAYBE: &str = "define maybe dup 0 gt if else drop then end\n";

#[derive(Clone, Debug, PartialEq)]
pub struct Nothing;

impl AppValue for Nothing {
    fn kind(&self) -> &'static str {
        "nothing"
    }
}

/// The host: one word, and what is printed.
#[derive(Default)]
pub struct Toy {
    pub printed: Vec<String>,
}

impl Host for Toy {
    type Value = Nothing;
    type Request = i64;
    fn call(
        &mut self,
        word: usize,
        args: Args<'_, Nothing>,
        _out: &mut Vec<Value<Nothing>>,
    ) -> Result<Done<i64>, Failure> {
        let args: Vec<_> = args.collect();
        match (word, args.as_slice()) {
            (0, [Value::Int(n)]) => Ok(Done::Later(*n)),
            _ => Err(Failure::new(
                fuxth::Code::Host("not_a_number"),
                "takes an integer",
            )),
        }
    }

    fn write(&mut self, text: &str) -> Result<(), Failure> {
        self.printed.push(text.to_owned());
        Ok(())
    }
}

fn vocabulary() -> Vocabulary {
    let mut words = Vocabulary::default();
    let _ = words.add("ask", "any -- integer");
    let _ = words.raises(fuxth::Code::Host("not_a_number"));
    words
}

/// A value as a test shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum V {
    Int(i64),
    Bool(bool),
    Str(String),
    List(Vec<V>),
}

impl V {
    /// As `string` and `print` show it.
    pub fn text(&self) -> String {
        match self {
            V::Int(n) => n.to_string(),
            V::Bool(b) => b.to_string(),
            V::Str(s) => s.clone(),
            V::List(items) => items.iter().map(V::text).collect::<Vec<_>>().join(" "),
        }
    }
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// It ran to its end, printing these lines and leaving these values.
    Left(Vec<String>, Vec<V>),
    /// It failed as it ran: the code, and the line of the program it was
    /// at (0 when not in the program: in the library).
    Failed(String, u32),
    /// Its text could not be split into words: a quote left open.
    Refused(String, u32),
    /// It ran past the steps allowed.
    Endless,
}

fn line(place: Option<&Place>) -> u32 {
    match place {
        Some(Place::Line { source, line }) if &**source == "program" => *line,
        Some(Place::Line { .. } | Place::Argument(_)) | None => 0,
    }
}

fn value(v: &Value<Nothing>) -> V {
    match v {
        Value::Int(n) => V::Int(*n),
        Value::Bool(b) => V::Bool(*b),
        Value::Str(s) => V::Str(s.to_string()),
        Value::List(l) => V::List(l.items().iter().map(value).collect()),
        Value::App(_) | Value::Xt(_) | Value::Mark(_) => {
            V::Str(v.text().unwrap_or_else(|f| f.message))
        }
    }
}

/// A machine with `text` to read, after the standard library.
pub fn compile(text: &str) -> Result<Machine<Nothing>, Error> {
    let mut machine = READY.with(Clone::clone)?;
    machine.read(token::source("program", text)?, None);
    Ok(machine)
}

thread_local! {
    /// The standard library, read once a thread.
    static READY: Result<Machine<Nothing>, Error> = library();
}

fn library() -> Result<Machine<Nothing>, Error> {
    let mut machine = Machine::new(vocabulary());
    machine.library("stdlib.fux", STDLIB, &mut Toy::default())?;
    let mut maybe = machine.clone();
    maybe.read(token::source("maybe.fux", MAYBE)?, None);
    match maybe.run(&mut Toy::default(), u64::MAX)? {
        Outcome::Finished => Ok(maybe),
        Outcome::Paused | Outcome::Suspended(_) => Err(Error::new(
            fuxth::Code::Contract,
            "maybe.fux stopped short",
            None,
        )),
    }
}

/// Runs `text` in fuxth, `budget` steps at a time, answering each `ask`.
pub fn fuxth(text: &str, budget: u64) -> Ended {
    let mut machine = match compile(text) {
        Ok(machine) => machine,
        Err(e) => return Ended::Refused(e.code.name().to_owned(), line(e.place.as_ref())),
    };
    let mut host = Toy::default();
    let budget = budget.min(STEPS);
    let mut taken = 0u64;
    loop {
        if taken > STEPS {
            return Ended::Endless;
        }
        taken = taken.saturating_add(budget);
        let failed = |e: Error| Ended::Failed(e.code.name().to_owned(), line(e.place.as_ref()));
        match machine.run(&mut host, budget) {
            Ok(Outcome::Finished) => {
                let stack = machine.stack().iter().map(value).collect();
                return Ended::Left(host.printed, stack);
            }
            Ok(Outcome::Paused) => {}
            Ok(Outcome::Suspended(n)) => match n.checked_mul(10) {
                Some(answer) => machine.push(Value::Int(answer)),
                None => {
                    let failure = Failure::new(fuxth::Code::Overflow, "past an integer");
                    if let Err(e) = machine.raise(failure, None) {
                        return failed(e);
                    }
                }
            },
            Err(e) => return failed(e),
        }
    }
}

/// Random programs, a token a line, so a line names a token. They lean
/// toward running: values of the kinds the words take, mostly, and loops
/// that count down; but any word may come anywhere.
pub struct Gen<'a> {
    rng: &'a mut Rng,
    out: Vec<String>,
    words: Vec<String>,
    names: Vec<String>,
}

const LITERALS: [&str; 12] = [
    "0",
    "1",
    "2",
    "3",
    "7",
    "-1",
    "-5",
    "10",
    "100",
    "9223372036854775807",
    "-9223372036854775808",
    "4294967296",
];

const LIBRARY_WORDS: [&str; 28] = [
    "dup", "swap", "over", "rot", "true", "false", "nip", "-rot", "2dup", "2drop", "negate", "abs",
    "min", "max", "ne", "le", "ge", "gt", "not", "mod", "reverse", "sort", "upper", "sum", "first",
    "last", "glue", "cat",
];

/// What a generated `try` catches. Not the limits, which each run meets
/// in its own place (a call in last place takes no room in fuxth).
const TRY_CODES: [&str; 7] = [
    "fail",
    "type",
    "stack_underflow",
    "div_by_zero range",
    "overflow not_a_number",
    "fail type stack_underflow div_by_zero range overflow not_a_number",
    "unset",
];

const CORE_WORDS: [&str; 18] = [
    "drop", "add", "sub", "mul", "div", "lt", "eq", "len", "at", "slice", "concat", "push", "put",
    "emit", "char", "kind", "string", "print",
];

impl<'a> Gen<'a> {
    pub fn new(rng: &'a mut Rng) -> Gen<'a> {
        Gen {
            rng,
            out: Vec::new(),
            words: Vec::new(),
            names: Vec::new(),
        }
    }

    fn put(&mut self, words: &[&str]) {
        self.out.extend(words.iter().map(|s| (*s).to_owned()));
    }

    /// A whole program: some definitions, then the top level.
    pub fn program(mut self) -> String {
        // Now and then a library word's name, which it hides from there on.
        let names: Vec<String> = (0..self.rng.below(4))
            .map(
                |n| match (self.rng.chance(5), ["abs", "even", "sum"].get(n)) {
                    (true, Some(word)) => (*word).to_owned(),
                    (true, None) | (false, _) => format!("w{n}"),
                },
            )
            .collect();
        for (n, name) in names.iter().enumerate() {
            let name = name.clone();
            self.put(&["define", &name]);
            if n > 0 && self.rng.chance(20) {
                // A word calling the one before it, counting down: n -- …
                let next = names.get(n.saturating_sub(1)).cloned().unwrap_or_default();
                self.put(&["dup", "0", "gt", "if", "1", "sub"]);
                self.seq(3, 1);
                self.put(&[&next, "then"]);
            } else if self.rng.chance(10) {
                // Recursion that may never return: at 0 it calls itself
                // again, at once; else it uses what the rest of it takes.
                self.put(&["dup", "0", "eq", "if", "drop", "0", "recurse", "else"]);
                self.seq(4, 1);
                self.put(&["then"]);
            } else if self.rng.chance(30) {
                // Recursion that counts down: n -- …
                self.put(&["dup", "0", "gt", "if", "1", "sub"]);
                self.seq(3, 1);
                self.put(&["recurse", "then"]);
            } else if self.rng.chance(30) {
                // A name of its own.
                self.put(&["let", "a"]);
                self.seq(2, 1);
                self.put(&["a"]);
                self.seq(3, 1);
            } else {
                self.seq(6, 1);
            }
            self.put(&["end"]);
            self.words.push(name);
        }
        let len = self.rng.below(25).saturating_add(4);
        for _ in 0..len {
            self.item(0);
            if self.rng.chance(5) {
                let name = format!("v{}", self.names.len());
                self.put(&["let", &name]);
                self.names.push(name);
            }
        }
        self.out.join("\n")
    }

    fn seq(&mut self, max: usize, depth: usize) {
        for _ in 0..self.rng.below(max.saturating_add(1)) {
            self.item(depth);
        }
    }

    fn item(&mut self, depth: usize) {
        if depth < 3 && self.rng.chance(4) {
            return self.try_block(depth);
        }
        let roll = self.rng.below(100);
        let words = self.words.clone();
        let names = self.names.clone();
        match roll {
            0..=21 => {
                let n = self.rng.pick(&LITERALS);
                self.put(&[n]);
            }
            22..=27 => {
                let s = self
                    .rng
                    .pick(&["a", "b", "if", "end", "12", "two words", "", "ab"]);
                self.put(&["str", &format!("'{s}'")]);
            }
            28..=45 => {
                let w = self.rng.pick(&CORE_WORDS);
                self.put(&[w]);
            }
            46..=49 => {
                let n = self.rng.pick(&["0", "1", "2", "3"]);
                let w = self.rng.pick(&["pick", "roll"]);
                self.put(&[n, w]);
            }
            50..=59 => {
                let w = self.rng.pick(&LIBRARY_WORDS);
                self.put(&[w]);
            }
            60..=62 => {
                let w = self.rng.pick(&HOST_WORDS);
                self.put(&[w]);
            }
            63..=69 if depth < 3 => {
                let n = self.rng.pick(&LITERALS);
                let cmp = self.rng.pick(&["lt", "gt", "eq"]);
                self.put(&[n, cmp, "if"]);
                self.seq(4, depth.saturating_add(1));
                if self.rng.chance(60) {
                    self.put(&["else"]);
                    self.seq(4, depth.saturating_add(1));
                }
                self.put(&["then"]);
            }
            70..=73 if depth < 2 => {
                let n = self.rng.pick(&["1", "2", "3", "5"]);
                self.put(&[n, "begin"]);
                self.seq(3, depth.saturating_add(1));
                self.put(&["1", "sub", "dup", "0", "le", "until", "drop"]);
            }
            74..=79 if depth < 3 => {
                self.put(&["list"]);
                self.seq(4, depth.saturating_add(1));
                self.put(&["end"]);
            }
            80..=84 if depth < 3 => {
                self.put(&["each"]);
                self.seq(3, depth.saturating_add(1));
                self.put(&["end"]);
            }
            85..=92 if !words.is_empty() => {
                let w = words
                    .get(self.rng.below(words.len()))
                    .cloned()
                    .unwrap_or_default();
                // 0 on top, where a word may call itself forever.
                if self.rng.chance(40) {
                    self.put(&["0"]);
                }
                self.out.push(w);
            }
            93..=99 if !names.is_empty() => {
                let n = names
                    .get(self.rng.below(names.len()))
                    .cloned()
                    .unwrap_or_default();
                self.out.push(n);
            }
            _ => {
                let n = self.rng.pick(&LITERALS);
                self.put(&[n]);
            }
        }
    }

    /// A `try` of codes its body may raise or never does: its body starts
    /// with values, as it starts with none, and ends now and then with a
    /// step that fails as it runs. Its handler drops the message.
    fn try_block(&mut self, depth: usize) {
        let codes = self.rng.pick(&TRY_CODES);
        self.put(&["str", &format!("'{codes}'"), "try"]);
        for _ in 0..self.rng.below(3) {
            let n = self.rng.pick(&LITERALS);
            self.put(&[n]);
        }
        self.seq(4, depth.saturating_add(1));
        if self.rng.chance(40) {
            let failing = self
                .rng
                .pick(&["str x fail", "1 0 div", "str x ask", "-1 char"]);
            self.put(&failing.split(' ').collect::<Vec<_>>());
        }
        self.put(&["catch", "nip"]);
        self.seq(3, depth.saturating_add(1));
        self.put(&["end"]);
    }

    /// A program that mostly runs: each word chosen to fit the values its
    /// model of the stack says are there (i integer, b boolean, s string,
    /// l list), branches and loop bodies leaving the depth they found.
    pub fn typed_program(mut self) -> String {
        let mut stack = Vec::new();
        let len = self.rng.below(30).saturating_add(6);
        self.typed(&mut stack, 0, len, 0);
        self.out.join("\n")
    }

    /// Adds `len` words on `stack`, never taking values below `floor`.
    fn typed(&mut self, stack: &mut Vec<char>, floor: usize, len: usize, depth: usize) {
        for _ in 0..len {
            let above = stack.len().saturating_sub(floor);
            let top = |n: usize| stack.iter().rev().take(n).copied().collect::<String>();
            let roll = self.rng.below(114);
            let w: &str = match roll {
                0..=17 => {
                    stack.push('i');
                    self.rng.pick(&LITERALS)
                }
                18..=21 => {
                    self.out.push("str".into());
                    stack.push('s');
                    self.rng.pick(&["a", "bc", "'two words'", "''", "xyz"])
                }
                22..=41 if above >= 2 && top(2) == "ii" => {
                    stack.pop();
                    let w = self.rng.pick(&[
                        "add", "sub", "mul", "div", "mod", "lt", "gt", "le", "ge", "min", "max",
                    ]);
                    let gives = if ["lt", "gt", "le", "ge"].contains(&w) {
                        'b'
                    } else {
                        'i'
                    };
                    stack.pop();
                    stack.push(gives);
                    w
                }
                // Values of one kind: a boolean is never an integer here,
                // though it is in Forth.
                42..=45
                    if above >= 2 && top(2).chars().all(|c| Some(c) == stack.last().copied()) =>
                {
                    stack.pop();
                    stack.pop();
                    stack.push('b');
                    self.rng.pick(&["eq", "ne"])
                }
                46..=49 if above >= 1 && top(1) == "b" => "not",
                50..=53 if above >= 1 && top(1) == "i" => {
                    self.rng.pick(&["negate", "abs", "ask", "dup 1 add"])
                }
                54..=61 if above >= 1 => {
                    let w = self.rng.pick(&["dup", "drop"]);
                    let t = stack.last().copied().unwrap_or('i');
                    if w == "dup" {
                        stack.push(t)
                    } else {
                        stack.pop();
                    }
                    w
                }
                62..=69 if above >= 2 => {
                    let w = self.rng.pick(&["swap", "over", "nip", "2dup"]);
                    let b = stack.pop().unwrap_or('i');
                    let a = stack.pop().unwrap_or('i');
                    let after: &[char] = match w {
                        "swap" => &[b, a],
                        "over" => &[a, b, a],
                        "nip" => &[b],
                        _ => &[a, b, a, b],
                    };
                    stack.extend(after);
                    w
                }
                70..=72 if above >= 3 => {
                    let w = self.rng.pick(&["rot", "-rot", "2 pick", "2 roll"]);
                    let c = stack.pop().unwrap_or('i');
                    let b = stack.pop().unwrap_or('i');
                    let a = stack.pop().unwrap_or('i');
                    match w {
                        "rot" | "2 roll" => stack.extend([b, c, a]),
                        "-rot" => stack.extend([c, a, b]),
                        _ => stack.extend([a, b, c, a]),
                    }
                    w
                }
                73..=78 if above >= 1 && top(1) == "s" => {
                    let (w, gives) = match self.rng.below(5) {
                        0 => ("len", 'i'),
                        1 => ("upper", 's'),
                        2 => ("reverse", 's'),
                        3 => ("fields", 'l'),
                        _ => ("sort", 's'),
                    };
                    stack.pop();
                    stack.push(gives);
                    w
                }
                79..=81 if above >= 2 && top(2) == "ss" => {
                    stack.pop();
                    stack.pop();
                    let (w, gives) = if self.rng.chance(50) {
                        ("cat", 's')
                    } else {
                        ("lt", 'b')
                    };
                    stack.push(gives);
                    w
                }
                82..=84 if above >= 1 && top(1) != "l" => {
                    stack.pop();
                    stack.push('s');
                    "string"
                }
                85..=87 if above >= 1 => {
                    stack.pop();
                    "print"
                }
                88..=92 if depth < 3 => {
                    // A list of what a fresh stack's words leave.
                    self.out.push("list".into());
                    let mut inner = Vec::new();
                    let n = self.rng.below(5);
                    self.typed(&mut inner, 0, n, depth.saturating_add(1));
                    stack.push('l');
                    "end"
                }
                93..=96 if above >= 1 && top(1) == "l" => {
                    let (w, gives) = match self.rng.below(4) {
                        0 => ("len", 'i'),
                        1 => ("reverse", 'l'),
                        2 => ("each drop end 0", 'i'),
                        _ => ("0 swap each drop 1 add end", 'i'),
                    };
                    stack.pop();
                    stack.push(gives);
                    w
                }
                97..=100 if depth < 3 && above >= 1 && top(1) == "b" => {
                    stack.pop();
                    self.out.push("if".into());
                    let at = stack.len();
                    let n = self.rng.below(4);
                    self.typed(stack, at, n, depth.saturating_add(1));
                    self.back_to(stack, at);
                    if self.rng.chance(50) {
                        self.out.push("else".into());
                        let n = self.rng.below(4);
                        self.typed(stack, at, n, depth.saturating_add(1));
                        self.back_to(stack, at);
                    }
                    "then"
                }
                101..=104 if depth < 2 => {
                    let n = self.rng.pick(&["1", "2", "4"]);
                    self.out.push(n.into());
                    self.out.push("begin".into());
                    stack.push('i');
                    let at = stack.len();
                    let k = self.rng.below(4);
                    self.typed(stack, at, k, depth.saturating_add(1));
                    self.back_to(stack, at);
                    for w in ["1", "sub", "dup", "0", "le", "until"] {
                        self.out.push(w.into());
                    }
                    stack.pop();
                    "drop"
                }
                108..=113 if depth < 3 => {
                    // A try whose body leaves nothing, as its handler does.
                    let codes = self.rng.pick(&TRY_CODES);
                    self.out.push("str".into());
                    self.out.push(format!("'{codes}'"));
                    self.out.push("try".into());
                    let mut inner = Vec::new();
                    let n = self.rng.below(6);
                    self.typed(&mut inner, 0, n, depth.saturating_add(1));
                    self.back_to(&mut inner, 0);
                    if self.rng.chance(50) {
                        self.out
                            .extend(["1", "0", "div", "drop"].map(str::to_owned));
                    }
                    "catch drop drop end"
                }
                105..=107 if depth == 0 && above >= 1 => {
                    // A name for the top value, read straight back.
                    let name = format!("n{}", self.names.len());
                    self.names.push(name.clone());
                    self.out.push("let".into());
                    self.out.push(name.clone());
                    self.out.push(name);
                    continue;
                }
                _ => {
                    stack.push('i');
                    self.rng.pick(&LITERALS)
                }
            };
            self.out.extend(w.split(' ').map(str::to_owned));
        }
    }

    /// Drops values down to `depth`.
    fn back_to(&mut self, stack: &mut Vec<char>, depth: usize) {
        while stack.len() > depth {
            stack.pop();
            self.out.push("drop".into());
        }
    }
}

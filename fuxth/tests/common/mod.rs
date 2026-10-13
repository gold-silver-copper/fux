//! What the comparison tests share: random programs, fuxth run as a host
//! runs it, and a second interpreter to hold it to, written as plainly as
//! can be: it walks the tokens, finds each block's end by counting, and
//! looks every name up as it runs.
#![allow(dead_code, reason = "each test file uses its own part")]
use fuxth::{
    AppValue, Args, Compiler, Done, Error, Failure, Host, Limits, Machine, Outcome, PRELUDE, Place,
    Token, Value, Vocabulary, token,
};
use std::collections::{HashMap, HashSet};

/// Steps either interpreter takes before a run counts as endless.
pub const STEPS: u64 = 200_000;

/// The limits both keep to.
pub const LIMITS: Limits = Limits {
    stack: 10_000,
    calls: 300,
};

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
        usize::try_from(
            self.next()
                .checked_rem(u64::try_from(n.max(1)).unwrap_or(1))
                .unwrap_or(0),
        )
        .unwrap_or(0)
    }
    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
    pub fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items.get(self.below(items.len())).copied().unwrap_or("dup")
    }
}

/// The host words both know: `ask` (n -- n*10), answered later, as a
/// server's answer is; `maybe` (n -- [n]), which gives n back only if it
/// is above 0.
pub const HOST_WORDS: [&str; 2] = ["ask", "maybe"];

#[derive(Clone, Debug, PartialEq)]
pub struct Nothing;

impl AppValue for Nothing {
    fn kind(&self) -> &'static str {
        "nothing"
    }
}

pub struct Toy;

impl Host for Toy {
    type Value = Nothing;
    type Request = i64;
    fn call(
        &mut self,
        word: usize,
        args: Args<'_, Nothing>,
        out: &mut Vec<Value<Nothing>>,
    ) -> Result<Done<i64>, Failure> {
        let args: Vec<_> = args.collect();
        match (word, args.as_slice()) {
            (0, [Value::Int(n)]) => Ok(Done::Later(*n)),
            (1, [Value::Int(n)]) => {
                if *n > 0 {
                    out.push(Value::Int(*n));
                }
                Ok(Done::Now)
            }
            _ => Err(Failure::new("not_a_number", "takes an integer")),
        }
    }
}

fn vocabulary() -> Vocabulary {
    let mut words = Vocabulary::default();
    words.add("ask", 1, 1..=1);
    words.add("maybe", 1, 0..=1);
    words
}

/// A value as both show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum V {
    Int(i64),
    Bool(bool),
    Str(String),
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// It ran to its end, leaving these.
    Left(Vec<V>),
    /// It failed as it ran: the code, and the line of the program it was
    /// at (0 when not in the program: in the prelude).
    Failed(String, u32),
    /// fuxth refused it before it ran.
    Refused(String, u32),
    /// It ran past the steps allowed, or a limit whose count the two
    /// interpreters keep differently (calls, after a call in last place).
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
        Value::App(_) => V::Str("nothing".to_owned()),
    }
}

/// Compiles `text` after the prelude.
pub fn compile(text: &str) -> Result<fuxth::Program<Nothing>, Error> {
    let mut compiler = Compiler::new(vocabulary());
    compiler.add(&token::source("prelude.fux", PRELUDE)?)?;
    compiler.add(&token::source("program", text)?)?;
    compiler.finish()
}

/// Runs `text` in fuxth, `budget` steps at a time, answering each `ask`.
pub fn fuxth(text: &str, budget: u64) -> Ended {
    let program = match compile(text) {
        Ok(program) => program,
        Err(e) => return Ended::Refused(e.code.name().to_owned(), line(e.place.as_ref())),
    };
    let mut machine = Machine::new(program, LIMITS);
    let budget = budget.min(STEPS);
    let mut taken = 0u64;
    loop {
        if taken > STEPS {
            return Ended::Endless;
        }
        taken = taken.saturating_add(budget);
        let failed = |e: Error| match e.code.name() {
            "call_depth" | "stack_overflow" => Ended::Endless,
            code => Ended::Failed(code.to_owned(), line(e.place.as_ref())),
        };
        match machine.run(&mut Toy, budget) {
            Ok(Outcome::Finished) => {
                return Ended::Left(machine.stack().iter().map(value).collect());
            }
            Ok(Outcome::Paused) => {}
            Ok(Outcome::Suspended(n)) => match n.checked_mul(10) {
                Some(answer) => {
                    if let Err(e) = machine.push(Value::Int(answer)) {
                        return failed(e);
                    }
                }
                None => return Ended::Failed("overflow".to_owned(), 0),
            },
            Err(e) => return failed(e),
        }
    }
}

/// The second interpreter.
pub struct Naive {
    defs: HashMap<String, Vec<Token>>,
    consts: HashMap<String, V>,
    pub stack: Vec<V>,
    steps: u64,
    calls: usize,
    /// The program's lines a run reached.
    pub reached: HashSet<u32>,
}

/// Why the naive run stopped early: an error's code and line.
type Stop = (String, u32);

const BUILTINS: [&str; 14] = [
    "dup", "drop", "swap", "over", "rot", "add", "sub", "mul", "div", "mod", "eq", "lt", "gt",
    "not",
];

fn is_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

impl Naive {
    pub fn new() -> Naive {
        Naive {
            defs: HashMap::new(),
            consts: HashMap::new(),
            stack: Vec::new(),
            steps: 0,
            calls: 0,
            reached: HashSet::new(),
        }
    }

    /// Runs `text` after the prelude, on a thread with room for its Rust
    /// recursion: a call or block is a Rust call here.
    pub fn run(text: &str) -> (Ended, HashSet<u32>) {
        let text = text.to_owned();
        std::thread::Builder::new()
            .stack_size(512 << 20)
            .spawn(move || Naive::run_here(&text))
            .ok()
            .and_then(|thread| thread.join().ok())
            .unwrap_or((Ended::Endless, HashSet::new()))
    }

    fn run_here(text: &str) -> (Ended, HashSet<u32>) {
        let mut naive = Naive::new();
        let tokens = |source: &str, text: &str| token::source(source, text).unwrap_or_default();
        let prelude = tokens("prelude.fux", PRELUDE);
        let program = tokens("program", text);
        let ended = match naive.exec(&prelude).and_then(|()| naive.exec(&program)) {
            Ok(()) => Ended::Left(naive.stack.clone()),
            Err((code, _)) if code == "steps" || code == "calls" || code == "full" => {
                Ended::Endless
            }
            Err((code, line)) => Ended::Failed(code, line),
        };
        (ended, naive.reached)
    }

    fn pop(&mut self, at: u32) -> Result<V, Stop> {
        self.stack
            .pop()
            .ok_or_else(|| ("stack_underflow".to_owned(), at))
    }

    fn push(&mut self, v: V) -> Result<(), Stop> {
        if self.stack.len() >= LIMITS.stack {
            return Err(("full".to_owned(), 0));
        }
        self.stack.push(v);
        Ok(())
    }

    fn need(&self, n: usize, at: u32) -> Result<(), Stop> {
        if self.stack.len() < n {
            return Err(("stack_underflow".to_owned(), at));
        }
        Ok(())
    }

    fn int(&mut self, at: u32) -> Result<(i64, i64), Stop> {
        self.need(2, at)?;
        let b = self.pop(at)?;
        let a = self.pop(at)?;
        match (a, b) {
            (V::Int(a), V::Int(b)) => Ok((a, b)),
            _ => Err(("type".to_owned(), at)),
        }
    }

    fn flag(&mut self, at: u32) -> Result<bool, Stop> {
        match self.pop(at)? {
            V::Bool(b) => Ok(b),
            V::Int(_) | V::Str(_) => Err(("type".to_owned(), at)),
        }
    }

    /// Where the block opened at `from` (by `open`) closes: the index of
    /// its `close`, and of an `else` at its own depth.
    fn matching(tokens: &[Token], from: usize, open: &str, close: &str) -> (usize, Option<usize>) {
        let mut depth = 0usize;
        let mut middle = None;
        for (i, t) in tokens.iter().enumerate().skip(from) {
            let text = &*t.text;
            // A `str` makes the next token text, not a word.
            if i > 0
                && tokens
                    .get(i.wrapping_sub(1))
                    .is_some_and(|p| &*p.text == "str")
            {
                continue;
            }
            if text == open {
                depth = depth.saturating_add(1);
            } else if text == close {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return (i, middle);
                }
            } else if text == "else" && depth == 1 && open == "if" {
                middle = Some(i);
            }
        }
        (tokens.len(), middle)
    }

    fn exec(&mut self, tokens: &[Token]) -> Result<(), Stop> {
        let mut i = 0;
        while let Some(t) = tokens.get(i) {
            let at = line(Some(&t.place));
            self.reached.insert(at);
            self.steps = self.steps.saturating_add(1);
            if self.steps > STEPS {
                return Err(("steps".to_owned(), at));
            }
            let text = &*t.text;
            match text {
                "str" => {
                    let next = tokens
                        .get(i.saturating_add(1))
                        .map(|t| t.text.to_string())
                        .unwrap_or_default();
                    self.push(V::Str(next))?;
                    i = i.saturating_add(2);
                    continue;
                }
                "define" => {
                    let name = tokens
                        .get(i.saturating_add(1))
                        .map(|t| t.text.to_string())
                        .unwrap_or_default();
                    let mut end = i.saturating_add(2);
                    while tokens.get(end).is_some_and(|t| &*t.text != "end")
                        || tokens
                            .get(end.wrapping_sub(1))
                            .is_some_and(|t| &*t.text == "str")
                    {
                        end = end.saturating_add(1);
                    }
                    let body = tokens
                        .get(i.saturating_add(2)..end)
                        .unwrap_or_default()
                        .to_vec();
                    self.defs.insert(name, body);
                    i = end.saturating_add(1);
                    continue;
                }
                "constant" => {
                    let name = tokens
                        .get(i.saturating_add(1))
                        .map(|t| t.text.to_string())
                        .unwrap_or_default();
                    let v = self.pop(at)?;
                    self.consts.insert(name, v);
                    i = i.saturating_add(2);
                    continue;
                }
                "if" => {
                    let (then, middle) = Naive::matching(tokens, i, "if", "then");
                    let yes = self.flag(at)?;
                    let branch = match (yes, middle) {
                        (true, Some(m)) => tokens.get(i.saturating_add(1)..m),
                        (true, None) => tokens.get(i.saturating_add(1)..then),
                        (false, Some(m)) => tokens.get(m.saturating_add(1)..then),
                        (false, None) => None,
                    };
                    self.exec(branch.unwrap_or_default())?;
                    i = then.saturating_add(1);
                    continue;
                }
                "begin" => {
                    let (until, _) = Naive::matching(tokens, i, "begin", "until");
                    let body = tokens.get(i.saturating_add(1)..until).unwrap_or_default();
                    let until_line = tokens.get(until).map_or(0, |t| line(Some(&t.place)));
                    loop {
                        self.exec(body)?;
                        self.reached.insert(until_line);
                        if self.flag(until_line)? {
                            break;
                        }
                    }
                    i = until.saturating_add(1);
                    continue;
                }
                _ => {}
            }
            self.word(text, at)?;
            i = i.saturating_add(1);
        }
        Ok(())
    }

    fn word(&mut self, text: &str, at: u32) -> Result<(), Stop> {
        if is_number(text) {
            let n = text.parse().map_err(|_| ("overflow".to_owned(), at))?;
            return self.push(V::Int(n));
        }
        if let Some(v) = self.consts.get(text).cloned() {
            return self.push(v);
        }
        if let Some(body) = self.defs.get(text).cloned() {
            self.calls = self.calls.saturating_add(1);
            if self.calls > LIMITS.calls {
                return Err(("calls".to_owned(), at));
            }
            let ran = self.exec(&body);
            self.calls = self.calls.saturating_sub(1);
            return ran;
        }
        let overflow = || ("overflow".to_owned(), at);
        match text {
            "dup" => {
                let a = self.pop(at)?;
                self.push(a.clone())?;
                self.push(a)
            }
            "drop" => self.pop(at).map(|_| ()),
            "swap" => {
                self.need(2, at)?;
                let b = self.pop(at)?;
                let a = self.pop(at)?;
                self.push(b)?;
                self.push(a)
            }
            "over" => {
                self.need(2, at)?;
                let b = self.pop(at)?;
                let a = self.pop(at)?;
                self.push(a.clone())?;
                self.push(b)?;
                self.push(a)
            }
            "rot" => {
                self.need(3, at)?;
                let c = self.pop(at)?;
                let b = self.pop(at)?;
                let a = self.pop(at)?;
                self.push(b)?;
                self.push(c)?;
                self.push(a)
            }
            "add" => {
                let (a, b) = self.int(at)?;
                self.push(V::Int(a.checked_add(b).ok_or_else(overflow)?))
            }
            "sub" => {
                let (a, b) = self.int(at)?;
                self.push(V::Int(a.checked_sub(b).ok_or_else(overflow)?))
            }
            "mul" => {
                let (a, b) = self.int(at)?;
                self.push(V::Int(a.checked_mul(b).ok_or_else(overflow)?))
            }
            "div" | "mod" => {
                let (a, b) = self.int(at)?;
                if b == 0 {
                    return Err(("div_by_zero".to_owned(), at));
                }
                let n = if text == "div" {
                    a.checked_div(b).ok_or_else(overflow)?
                } else if b == -1 {
                    0
                } else {
                    a.checked_rem(b).unwrap_or(0)
                };
                self.push(V::Int(n))
            }
            "eq" => {
                self.need(2, at)?;
                let b = self.pop(at)?;
                let a = self.pop(at)?;
                self.push(V::Bool(a == b))
            }
            "lt" | "gt" => {
                let (a, b) = self.int(at)?;
                self.push(V::Bool(if text == "lt" { a < b } else { a > b }))
            }
            "not" => {
                let b = self.flag(at)?;
                self.push(V::Bool(!b))
            }
            "ask" | "maybe" => match self.pop(at)? {
                V::Int(n) if text == "ask" => {
                    self.push(V::Int(n.checked_mul(10).ok_or(("overflow".to_owned(), 0))?))
                }
                V::Int(n) => {
                    if n > 0 {
                        self.push(V::Int(n))?;
                    }
                    Ok(())
                }
                V::Bool(_) | V::Str(_) => Err(("not_a_number".to_owned(), at)),
            },
            _ => Err(("unknown_word".to_owned(), at)),
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
    constants: Vec<String>,
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

const PRELUDE_WORDS: [&str; 14] = [
    "true", "false", "nip", "tuck", "-rot", "2dup", "2drop", "negate", "abs", "min", "max", "ne",
    "le", "ge",
];

impl<'a> Gen<'a> {
    pub fn new(rng: &'a mut Rng) -> Gen<'a> {
        Gen {
            rng,
            out: Vec::new(),
            words: Vec::new(),
            constants: Vec::new(),
        }
    }

    /// A whole program: some definitions, then the top level.
    pub fn program(mut self) -> String {
        for n in 0..self.rng.below(4) {
            let name = format!("w{n}");
            self.out.push("define".into());
            self.out.push(name.clone());
            if self.rng.chance(30) {
                // Recursion that counts down: n -- …
                let body = ["dup", "0", "gt", "if", "1", "sub"];
                self.out.extend(body.iter().map(|s| s.to_string()));
                self.seq(3, 1);
                self.out.push(name.clone());
                self.out.push("then".into());
            } else {
                self.seq(6, 1);
            }
            self.out.push("end".into());
            self.words.push(name);
        }
        let len = self.rng.below(25).saturating_add(4);
        for _ in 0..len {
            if self.rng.chance(5) {
                self.item(0);
                let name = format!("c{}", self.constants.len());
                self.out.push("constant".into());
                self.out.push(name.clone());
                self.constants.push(name);
            } else {
                self.item(0);
            }
        }
        self.out.join("\n")
    }

    /// A program that mostly runs: each word chosen to fit the values its
    /// model of the stack says are there, branches and loop bodies leaving
    /// the depth they found.
    pub fn typed_program(mut self) -> String {
        let mut stack = Vec::new();
        let len = self.rng.below(30).saturating_add(6);
        self.typed(&mut stack, 0, len, 0);
        self.out.join("\n")
    }

    /// Adds `len` words on `stack` (kinds: i, b, s), never taking values
    /// below `floor`.
    fn typed(&mut self, stack: &mut Vec<char>, floor: usize, len: usize, depth: usize) {
        for _ in 0..len {
            let above = stack.len().saturating_sub(floor);
            let top = |n: usize| stack.iter().rev().take(n).copied().collect::<String>();
            let roll = self.rng.below(100);
            let w: &str = match roll {
                0..=19 => {
                    stack.push('i');
                    self.rng.pick(&LITERALS)
                }
                20..=22 => {
                    self.out.push("str".into());
                    stack.push('s');
                    self.rng.pick(&["a", "b"])
                }
                23..=44 if above >= 2 && top(2) == "ii" => {
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
                45..=49 if above >= 2 => {
                    stack.pop();
                    stack.pop();
                    stack.push('b');
                    self.rng.pick(&["eq", "ne"])
                }
                50..=54 if above >= 1 && top(1) == "b" => "not",
                55..=59 if above >= 1 && top(1) == "i" => {
                    self.rng.pick(&["negate", "abs", "ask", "dup 1 add"])
                }
                60..=69 if above >= 1 => {
                    let w = self.rng.pick(&["dup", "drop"]);
                    let t = stack.last().copied().unwrap_or('i');
                    if w == "dup" {
                        stack.push(t)
                    } else {
                        stack.pop();
                    }
                    w
                }
                70..=79 if above >= 2 => {
                    let w = self.rng.pick(&["swap", "over", "nip", "tuck", "2dup"]);
                    let b = stack.pop().unwrap_or('i');
                    let a = stack.pop().unwrap_or('i');
                    let after: &[char] = match w {
                        "swap" => &[b, a],
                        "over" => &[a, b, a],
                        "nip" => &[b],
                        "tuck" => &[b, a, b],
                        _ => &[a, b, a, b],
                    };
                    stack.extend(after);
                    w
                }
                80..=83 if above >= 3 => {
                    let w = self.rng.pick(&["rot", "-rot"]);
                    let c = stack.pop().unwrap_or('i');
                    let b = stack.pop().unwrap_or('i');
                    let a = stack.pop().unwrap_or('i');
                    stack.extend(if w == "rot" { [b, c, a] } else { [c, a, b] });
                    w
                }
                84..=91 if depth < 3 && above >= 1 && top(1) == "b" => {
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
                92..=95 if depth < 2 => {
                    self.out.push(self.rng.pick(&["1", "2", "4"]).into());
                    self.out.push("begin".into());
                    stack.push('i');
                    let at = stack.len();
                    let n = self.rng.below(4);
                    self.typed(stack, at, n, depth.saturating_add(1));
                    self.back_to(stack, at);
                    for w in ["1", "sub", "dup", "0", "le", "until"] {
                        self.out.push(w.into());
                    }
                    stack.pop();
                    "drop"
                }
                _ => {
                    stack.push('i');
                    self.rng.pick(&LITERALS)
                }
            };
            self.out.push(w.into());
        }
    }

    /// Drops values down to `depth`.
    fn back_to(&mut self, stack: &mut Vec<char>, depth: usize) {
        while stack.len() > depth {
            stack.pop();
            self.out.push("drop".into());
        }
    }

    fn seq(&mut self, max: usize, depth: usize) {
        for _ in 0..self.rng.below(max.saturating_add(1)) {
            self.item(depth);
        }
    }

    fn item(&mut self, depth: usize) {
        let roll = self.rng.below(100);
        let words = self.words.clone();
        let constants = self.constants.clone();
        match roll {
            0..=24 => {
                let n = self.rng.pick(&LITERALS);
                self.out.push(n.into());
            }
            25..=29 => {
                self.out.push("str".into());
                let s = self
                    .rng
                    .pick(&["a", "b", "if", "end", "12", "two words", ""]);
                self.out.push(format!("'{s}'"));
            }
            30..=59 => {
                let w = self.rng.pick(&[
                    "dup", "drop", "swap", "over", "rot", "add", "sub", "mul", "div", "mod", "eq",
                    "lt", "gt", "not", "add", "sub", "dup", "swap",
                ]);
                self.out.push(w.into());
            }
            60..=67 => {
                let w = self.rng.pick(&PRELUDE_WORDS);
                self.out.push(w.into());
            }
            68..=72 => {
                let w = self.rng.pick(&HOST_WORDS);
                self.out.push(w.into());
            }
            73..=80 if depth < 3 => {
                // A comparison, then a choice.
                let n = self.rng.pick(&LITERALS);
                self.out.push(n.into());
                self.out.push(self.rng.pick(&["lt", "gt", "eq"]).into());
                self.out.push("if".into());
                self.seq(4, depth.saturating_add(1));
                if self.rng.chance(60) {
                    self.out.push("else".into());
                    self.seq(4, depth.saturating_add(1));
                }
                self.out.push("then".into());
            }
            81..=86 if depth < 2 => {
                // A loop counting down from a small number.
                let n = self.rng.pick(&["1", "2", "3", "5"]);
                self.out.push(n.into());
                self.out.push("begin".into());
                self.seq(3, depth.saturating_add(1));
                for w in ["1", "sub", "dup", "0", "le", "until", "drop"] {
                    self.out.push(w.into());
                }
            }
            87..=94 if !words.is_empty() => {
                let w = words
                    .get(self.rng.below(words.len()))
                    .cloned()
                    .unwrap_or_default();
                self.out.push(w);
            }
            95..=99 if !constants.is_empty() => {
                let c = constants
                    .get(self.rng.below(constants.len()))
                    .cloned()
                    .unwrap_or_default();
                self.out.push(c);
            }
            _ => self.out.push(self.rng.pick(&LITERALS).into()),
        }
    }
}

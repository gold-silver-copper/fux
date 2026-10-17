//! What the comparison tests share: random programs, fuxth run as a host
//! runs it, and a second interpreter to hold it to, written as plainly as
//! can be: it walks the tokens, finds each block's end by counting, and
//! looks every name up as it runs. It runs the standard library from its
//! fuxth source, as fuxth does.
#![allow(dead_code, reason = "each test file uses its own part")]
use fuxth::{
    AppValue, Args, Compiler, Done, Error, Failure, Host, Limits, Machine, Outcome, Place, STDLIB,
    Token, Value, Vocabulary, token,
};
use std::collections::{HashMap, HashSet};

/// Steps either interpreter takes before a run counts as endless.
pub const STEPS: u64 = 50_000;

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

/// The host: two words, and what is printed.
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
            _ => Err(Failure::new(
                fuxth::Code::Host("not_a_number"),
                "takes an integer",
            )),
        }
    }

    fn print(&mut self, line: &str) -> Result<(), Failure> {
        self.printed.push(line.to_owned());
        Ok(())
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
    /// fuxth refused it before it ran.
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
        Value::App(_) => V::Str("nothing".to_owned()),
    }
}

/// Compiles `text` after the standard library.
pub fn compile(text: &str) -> Result<fuxth::Program<Nothing>, Error> {
    let mut compiler = READY.with(Clone::clone)?;
    compiler.add(&token::source("program", text)?)?;
    compiler.finish()
}

thread_local! {
    /// The standard library, compiled and checked once a thread.
    static READY: Result<Compiler<Nothing>, Error> = library();
}

fn library() -> Result<Compiler<Nothing>, Error> {
    let mut compiler = Compiler::new(vocabulary());
    compiler.add(&token::source("stdlib.fux", STDLIB)?)?;
    compiler.prepare();
    Ok(compiler)
}

/// Runs `text` in fuxth, `budget` steps at a time, answering each `ask`.
pub fn fuxth(text: &str, budget: u64) -> Ended {
    let program = match compile(text) {
        Ok(program) => program,
        Err(e) => return Ended::Refused(e.code.name().to_owned(), line(e.place.as_ref())),
    };
    let mut machine = Machine::new(program, LIMITS);
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

/// Why the naive run stopped early: an error's code and line.
type Stop = (String, u32);

/// The second interpreter.
pub struct Naive {
    defs: HashMap<String, Vec<Token>>,
    globals: HashMap<String, V>,
    /// Each call's `let` names.
    frames: Vec<HashMap<String, V>>,
    pub stack: Vec<V>,
    /// Where each open `list` began.
    floors: Vec<usize>,
    steps: u64,
    pub printed: Vec<String>,
    /// The program's lines a run reached.
    pub reached: HashSet<u32>,
    /// The lines whose step was gone through without failing, at least
    /// once.
    finished: HashSet<u32>,
    /// The lines of the calls under way, the outermost first: a failure
    /// leaves them as they were when it happened.
    calls: Vec<u32>,
}

/// Where a naive run went: the lines it reached, and, for one that failed,
/// the steps it failed in.
pub struct Trace {
    pub reached: HashSet<u32>,
    finished: HashSet<u32>,
    /// The line it failed at, and the lines of the calls it was inside.
    failing: Vec<u32>,
}

impl Trace {
    /// Whether every time the run reached `line` it failed there: in that
    /// step, or in a word it calls, never going past it.
    pub fn fails_at(&self, line: u32) -> bool {
        self.failing.contains(&line) && !self.finished.contains(&line)
    }
}

fn is_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

fn err(code: &str, at: u32) -> Stop {
    (code.to_owned(), at)
}

fn kind(v: &V) -> &'static str {
    match v {
        V::Int(_) => "integer",
        V::Bool(_) => "boolean",
        V::Str(_) => "string",
        V::List(_) => "list",
    }
}

fn order(a: &V, b: &V) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (V::Int(a), V::Int(b)) => Some(a.cmp(b)),
        (V::Str(a), V::Str(b)) => Some(a.cmp(b)),
        (V::List(a), V::List(b)) => {
            for (x, y) in a.iter().zip(b) {
                if x != y {
                    return order(x, y);
                }
            }
            Some(a.len().cmp(&b.len()))
        }
        _ => None,
    }
}

fn seq_len(v: &V, at: u32) -> Result<usize, Stop> {
    match v {
        V::Str(s) => Ok(s.chars().count()),
        V::List(l) => Ok(l.len()),
        V::Int(_) | V::Bool(_) => Err(err("type", at)),
    }
}

fn index(v: &V, len: usize, at: u32) -> Result<usize, Stop> {
    match v {
        V::Int(n) => usize::try_from(*n)
            .ok()
            .filter(|i| *i < len)
            .ok_or_else(|| err("range", at)),
        V::Bool(_) | V::Str(_) | V::List(_) => Err(err("type", at)),
    }
}

impl Default for Naive {
    fn default() -> Naive {
        Naive::new()
    }
}

impl Naive {
    pub fn new() -> Naive {
        Naive {
            defs: HashMap::new(),
            globals: HashMap::new(),
            frames: Vec::new(),
            stack: Vec::new(),
            floors: Vec::new(),
            steps: 0,
            printed: Vec::new(),
            reached: HashSet::new(),
            finished: HashSet::new(),
            calls: Vec::new(),
        }
    }

    /// Runs `text` after the standard library, on a thread with room for
    /// its Rust recursion: a call or block is a Rust call here.
    pub fn run(text: &str) -> (Ended, Trace) {
        let text = text.to_owned();
        std::thread::Builder::new()
            .stack_size(512 << 20)
            .spawn(move || Naive::run_here(&text))
            .ok()
            .and_then(|thread| thread.join().ok())
            .unwrap_or_else(|| {
                let trace = Trace {
                    reached: HashSet::new(),
                    finished: HashSet::new(),
                    failing: Vec::new(),
                };
                (Ended::Endless, trace)
            })
    }

    fn run_here(text: &str) -> (Ended, Trace) {
        let mut naive = Naive::new();
        let tokens = |source: &str, text: &str| token::source(source, text).unwrap_or_default();
        let library = tokens("stdlib.fux", STDLIB);
        let program = tokens("program", text);
        let ran = naive.exec(&library).and_then(|()| naive.exec(&program));
        let mut failing = std::mem::take(&mut naive.calls);
        failing.extend(ran.as_ref().err().map(|(_, line)| *line));
        let ended = match ran {
            Ok(()) => Ended::Left(naive.printed.clone(), naive.stack.clone()),
            Err((code, _)) if code == "steps" => Ended::Endless,
            Err((code, line)) if code == "calls" => Ended::Failed("call_depth".to_owned(), line),
            Err((code, line)) if code == "full" => Ended::Failed("stack_overflow".to_owned(), line),
            Err((code, line)) => Ended::Failed(code, line),
        };
        let trace = Trace {
            reached: naive.reached,
            finished: naive.finished,
            failing,
        };
        (ended, trace)
    }

    fn floor(&self) -> usize {
        self.floors.last().copied().unwrap_or(0)
    }

    fn need(&self, n: usize, at: u32) -> Result<(), Stop> {
        if self.stack.len().saturating_sub(self.floor()) < n {
            return Err(err("stack_underflow", at));
        }
        Ok(())
    }

    fn pop(&mut self, at: u32) -> Result<V, Stop> {
        self.need(1, at)?;
        self.stack.pop().ok_or_else(|| err("stack_underflow", at))
    }

    fn push(&mut self, v: V) -> Result<(), Stop> {
        if self.stack.len() >= LIMITS.stack {
            return Err(err("full", 0));
        }
        self.stack.push(v);
        Ok(())
    }

    fn ints(&mut self, at: u32) -> Result<(i64, i64), Stop> {
        self.need(2, at)?;
        let b = self.pop(at)?;
        let a = self.pop(at)?;
        match (a, b) {
            (V::Int(a), V::Int(b)) => Ok((a, b)),
            _ => Err(err("type", at)),
        }
    }

    fn flag(&mut self, at: u32) -> Result<bool, Stop> {
        match self.pop(at)? {
            V::Bool(b) => Ok(b),
            V::Int(_) | V::Str(_) | V::List(_) => Err(err("type", at)),
        }
    }

    /// Where the block opened at `from` closes: `close` at its own depth,
    /// counting every opener of that close (`define`, `list` and `each`
    /// for `end`); and an `else` at its own depth, for `if`.
    fn matching(tokens: &[Token], from: usize, open: &str, close: &str) -> (usize, Option<usize>) {
        let openers: &[&str] = if close == "end" {
            &["define", "list", "each"]
        } else {
            &[open]
        };
        let mut depth = 0usize;
        let mut middle = None;
        let mut quoted = false;
        for (i, t) in tokens.iter().enumerate().skip(from) {
            let text = &*t.text;
            // The word after `str`, `let` or `define` is text, not a word.
            if std::mem::replace(&mut quoted, false) {
                continue;
            }
            quoted = matches!(text, "str" | "let" | "define");
            if openers.contains(&text) {
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
        let mut i = 0usize;
        while let Some(t) = tokens.get(i) {
            let at = line(Some(&t.place));
            self.reached.insert(at);
            self.steps = self.steps.saturating_add(1);
            if self.steps > STEPS {
                return Err(err("steps", at));
            }
            let text = &*t.text;
            let next = |n: usize| tokens.get(i.saturating_add(n)).map(|t| t.text.to_string());
            match text {
                "str" => {
                    self.push(V::Str(next(1).unwrap_or_default()))?;
                    i = i.saturating_add(2);
                }
                "define" => {
                    let name = next(1).unwrap_or_default();
                    let (end, _) = Naive::matching(tokens, i, "define", "end");
                    let body = tokens
                        .get(i.saturating_add(2)..end)
                        .unwrap_or_default()
                        .to_vec();
                    self.defs.insert(name, body);
                    i = end.saturating_add(1);
                }
                "let" => {
                    let name = next(1).unwrap_or_default();
                    let v = self.pop(at)?;
                    match self.frames.last_mut() {
                        Some(frame) => frame.insert(name, v),
                        None => self.globals.insert(name, v),
                    };
                    i = i.saturating_add(2);
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
                }
                "begin" => {
                    let (until, _) = Naive::matching(tokens, i, "begin", "until");
                    let body = tokens.get(i.saturating_add(1)..until).unwrap_or_default();
                    let until_line = tokens.get(until).map_or(0, |t| line(Some(&t.place)));
                    loop {
                        self.exec(body)?;
                        self.reached.insert(until_line);
                        let done = self.flag(until_line)?;
                        self.finished.insert(until_line);
                        if done {
                            break;
                        }
                    }
                    i = until.saturating_add(1);
                }
                "list" => {
                    let (end, _) = Naive::matching(tokens, i, "list", "end");
                    let body = tokens.get(i.saturating_add(1)..end).unwrap_or_default();
                    let from = self.stack.len();
                    self.floors.push(from);
                    let ran = self.exec(body);
                    self.floors.pop();
                    ran?;
                    let mut items = Vec::new();
                    while self.stack.len() > from {
                        items.extend(self.stack.pop());
                    }
                    items.reverse();
                    self.push(V::List(items))?;
                    i = end.saturating_add(1);
                }
                "each" => {
                    let (end, _) = Naive::matching(tokens, i, "each", "end");
                    let body = tokens.get(i.saturating_add(1)..end).unwrap_or_default();
                    let items: Vec<V> = match self.pop(at)? {
                        V::Str(s) => s.chars().map(|c| V::Str(c.to_string())).collect(),
                        V::List(l) => l,
                        V::Int(_) | V::Bool(_) => return Err(err("type", at)),
                    };
                    for item in items {
                        self.push(item)?;
                        self.exec(body)?;
                    }
                    i = end.saturating_add(1);
                }
                _ => {
                    self.word(text, at)?;
                    i = i.saturating_add(1);
                }
            }
            self.finished.insert(at);
        }
        Ok(())
    }

    fn word(&mut self, text: &str, at: u32) -> Result<(), Stop> {
        if is_number(text) {
            let n = text.parse().map_err(|_| err("overflow", at))?;
            return self.push(V::Int(n));
        }
        if let Some(v) = self.frames.last().and_then(|f| f.get(text)).cloned() {
            return self.push(v);
        }
        if let Some(v) = self.globals.get(text).cloned() {
            return self.push(v);
        }
        if let Some(body) = self.defs.get(text).cloned() {
            if self.frames.len() >= LIMITS.calls {
                return Err(err("calls", at));
            }
            self.frames.push(HashMap::new());
            self.calls.push(at);
            let ran = self.exec(&body);
            self.frames.pop();
            // A failure keeps the calls it happened inside.
            if ran.is_ok() {
                self.calls.pop();
            }
            return ran;
        }
        self.builtin(text, at)
    }

    fn builtin(&mut self, text: &str, at: u32) -> Result<(), Stop> {
        let overflow = || err("overflow", at);
        match text {
            "drop" => self.pop(at).map(|_| ()),
            "pick" | "roll" => {
                let n = match self.pop(at)? {
                    V::Int(n) => usize::try_from(n).map_err(|_| err("range", at))?,
                    V::Bool(_) | V::Str(_) | V::List(_) => return Err(err("type", at)),
                };
                self.need(n.saturating_add(1), at)?;
                let from = self.stack.len().saturating_sub(n.saturating_add(1));
                if text == "pick" {
                    let v = self
                        .stack
                        .get(from)
                        .cloned()
                        .ok_or_else(|| err("stack_underflow", at))?;
                    self.push(v)
                } else {
                    if let Some(part) = self.stack.get_mut(from..) {
                        part.rotate_left(1);
                    }
                    Ok(())
                }
            }
            "add" => {
                let (a, b) = self.ints(at)?;
                self.push(V::Int(a.checked_add(b).ok_or_else(overflow)?))
            }
            "sub" => {
                let (a, b) = self.ints(at)?;
                self.push(V::Int(a.checked_sub(b).ok_or_else(overflow)?))
            }
            "mul" => {
                let (a, b) = self.ints(at)?;
                self.push(V::Int(a.checked_mul(b).ok_or_else(overflow)?))
            }
            "div" => {
                let (a, b) = self.ints(at)?;
                if b == 0 {
                    return Err(err("div_by_zero", at));
                }
                self.push(V::Int(a.checked_div(b).ok_or_else(overflow)?))
            }
            "lt" => {
                self.need(2, at)?;
                let b = self.pop(at)?;
                let a = self.pop(at)?;
                let o = order(&a, &b).ok_or_else(|| err("type", at))?;
                self.push(V::Bool(o == std::cmp::Ordering::Less))
            }
            "eq" => {
                self.need(2, at)?;
                let b = self.pop(at)?;
                let a = self.pop(at)?;
                self.push(V::Bool(a == b))
            }
            "len" => {
                let s = self.pop(at)?;
                let n = seq_len(&s, at)?;
                self.push(V::Int(i64::try_from(n).unwrap_or(i64::MAX)))
            }
            "at" => {
                self.need(2, at)?;
                let i = self.pop(at)?;
                let s = self.pop(at)?;
                let n = seq_len(&s, at)?;
                let i = index(&i, n, at)?;
                let v = match s {
                    V::Str(s) => s.chars().nth(i).map(|c| V::Str(c.to_string())),
                    V::List(l) => l.get(i).cloned(),
                    V::Int(_) | V::Bool(_) => None,
                };
                self.push(v.ok_or_else(|| err("range", at))?)
            }
            "slice" => {
                self.need(3, at)?;
                let to = self.pop(at)?;
                let from = self.pop(at)?;
                let s = self.pop(at)?;
                let n = seq_len(&s, at)?;
                let (V::Int(a), V::Int(b)) = (from, to) else {
                    return Err(err("type", at));
                };
                let (a, b) = (usize::try_from(a).ok(), usize::try_from(b).ok());
                let Some((a, b)) = a.zip(b).filter(|(a, b)| a <= b && *b <= n) else {
                    return Err(err("range", at));
                };
                let k = b.saturating_sub(a);
                let part = match s {
                    V::Str(s) => V::Str(s.chars().skip(a).take(k).collect()),
                    V::List(l) => V::List(l.into_iter().skip(a).take(k).collect()),
                    other @ (V::Int(_) | V::Bool(_)) => other,
                };
                self.push(part)
            }
            "concat" => {
                let V::List(items) = self.pop(at)? else {
                    return Err(err("type", at));
                };
                let strings = items.iter().any(|v| matches!(v, V::Str(_)));
                let lists = items.iter().any(|v| matches!(v, V::List(_)));
                let others = items.iter().any(|v| matches!(v, V::Int(_) | V::Bool(_)));
                if others || (strings && lists) {
                    return Err(err("type", at));
                }
                if strings {
                    self.push(V::Str(items.iter().map(V::text).collect()))
                } else {
                    let mut all = Vec::new();
                    for item in items {
                        if let V::List(l) = item {
                            all.extend(l);
                        }
                    }
                    self.push(V::List(all))
                }
            }
            "push" => {
                self.need(2, at)?;
                let x = self.pop(at)?;
                let s = self.pop(at)?;
                match (s, x) {
                    (V::List(mut l), x) => {
                        l.push(x);
                        self.push(V::List(l))
                    }
                    (V::Str(mut s), V::Str(x)) => {
                        s.push_str(&x);
                        self.push(V::Str(s))
                    }
                    _ => Err(err("type", at)),
                }
            }
            "put" => {
                self.need(3, at)?;
                let x = self.pop(at)?;
                let i = self.pop(at)?;
                let s = self.pop(at)?;
                let n = seq_len(&s, at)?;
                let i = index(&i, n, at)?;
                match (s, x) {
                    (V::List(mut l), x) => {
                        if let Some(slot) = l.get_mut(i) {
                            *slot = x;
                        }
                        self.push(V::List(l))
                    }
                    (V::Str(s), V::Str(x)) => {
                        let mut out = String::new();
                        for (k, c) in s.chars().enumerate() {
                            if k == i {
                                out.push_str(&x);
                            } else {
                                out.push(c);
                            }
                        }
                        self.push(V::Str(out))
                    }
                    _ => Err(err("type", at)),
                }
            }
            "code" => {
                let V::Str(s) = self.pop(at)? else {
                    return Err(err("type", at));
                };
                let mut chars = s.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => self.push(V::Int(i64::from(u32::from(c)))),
                    _ => Err(err("type", at)),
                }
            }
            "char" => {
                let V::Int(n) = self.pop(at)? else {
                    return Err(err("type", at));
                };
                let c = u32::try_from(n)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| err("range", at))?;
                self.push(V::Str(c.to_string()))
            }
            "kind" => {
                let v = self.pop(at)?;
                self.push(V::Str(kind(&v).to_owned()))
            }
            "string" => {
                let v = self.pop(at)?;
                self.push(V::Str(v.text()))
            }
            "print" => {
                let v = self.pop(at)?;
                self.printed.push(v.text());
                Ok(())
            }
            "fail" => {
                self.pop(at)?;
                Err(err("fail", at))
            }
            "ask" | "maybe" => match self.pop(at)? {
                V::Int(n) if text == "ask" => {
                    self.push(V::Int(n.checked_mul(10).ok_or(err("overflow", 0))?))
                }
                V::Int(n) => {
                    if n > 0 {
                        self.push(V::Int(n))?;
                    }
                    Ok(())
                }
                V::Bool(_) | V::Str(_) | V::List(_) => Err(err("not_a_number", at)),
            },
            _ => Err(err("unknown_word", at)),
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

const LIBRARY_WORDS: [&str; 30] = [
    "dup", "swap", "over", "rot", "true", "false", "nip", "tuck", "-rot", "2dup", "2drop",
    "negate", "abs", "min", "max", "ne", "le", "ge", "gt", "not", "mod", "reverse", "sort",
    "upper", "chars", "sum", "first", "last", "glue", "cat",
];

const CORE_WORDS: [&str; 18] = [
    "drop", "add", "sub", "mul", "div", "lt", "eq", "len", "at", "slice", "concat", "push", "put",
    "code", "char", "kind", "string", "print",
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
        for n in 0..self.rng.below(4) {
            let name = format!("w{n}");
            self.put(&["define", &name]);
            if self.rng.chance(10) {
                // Recursion that may never return: at 0 it calls itself
                // again, at once; else it uses what the rest of it takes.
                self.put(&["dup", "0", "eq", "if", "drop", "0", &name, "else"]);
                self.seq(4, 1);
                self.put(&["then"]);
            } else if self.rng.chance(30) {
                // Recursion that counts down: n -- …
                self.put(&["dup", "0", "gt", "if", "1", "sub"]);
                self.seq(3, 1);
                self.put(&[&name, "then"]);
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

    /// A program that mostly runs: each word chosen to fit the values its
    /// model of the stack says are there (i integer, b boolean, s string,
    /// l list), branches and loop bodies leaving the depth they found.
    pub fn typed_program(mut self) -> String {
        let mut stack = Vec::new();
        let len = self.rng.below(30).saturating_add(6);
        self.typed(&mut stack, 0, len, 0);
        self.out.join("\n")
    }

    /// A typed program of integers and booleans only, for gforth, which
    /// has nothing else; empty if this one used anything more.
    pub fn numeric_program(self) -> String {
        let text = self.typed_program();
        let numeric = text.lines().all(|l| {
            !matches!(
                l,
                "str"
                    | "list"
                    | "each"
                    | "len"
                    | "at"
                    | "slice"
                    | "concat"
                    | "push"
                    | "put"
                    | "string"
                    | "print"
                    | "kind"
                    | "code"
                    | "char"
                    | "upper"
                    | "reverse"
                    | "sort"
                    | "ask"
                    | "maybe"
                    | "let"
                    | "glue"
                    | "cat"
                    | "chars"
                    | "sum"
                    | "pick"
                    | "roll"
            )
        });
        if numeric { text } else { String::new() }
    }

    /// Adds `len` words on `stack`, never taking values below `floor`.
    fn typed(&mut self, stack: &mut Vec<char>, floor: usize, len: usize, depth: usize) {
        for _ in 0..len {
            let above = stack.len().saturating_sub(floor);
            let top = |n: usize| stack.iter().rev().take(n).copied().collect::<String>();
            let roll = self.rng.below(110);
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
                        3 => ("chars", 'l'),
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

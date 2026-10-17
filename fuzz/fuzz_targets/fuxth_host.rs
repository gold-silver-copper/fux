#![allow(dead_code, reason = "each target uses its own part")]
//! What the fuxth targets share: a host of one word and a run to the end.
//! `ask` (n -- n*10) suspends for its answer, as a server's does; `maybe`
//! (n -- [n]), defined after the standard library, gives n back only above
//! 0, a value given only sometimes.
use fuxth::{
    AppValue, Args, Compiler, Done, Error, Failure, Host, Limits, Machine, Outcome, Place, Value,
    Vocabulary, token,
};

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

    fn print(&mut self, _line: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// Small limits, so a fuzzed program meets them quickly.
pub const LIMITS: Limits = Limits {
    stack: 1000,
    calls: 100,
};

/// Steps a run may take in all.
pub const STEPS: u64 = 100_000;

/// `text` compiled after the standard library.
pub fn compile(text: &str) -> Result<fuxth::Program<Nothing>, Error> {
    let mut compiler = READY.with(Clone::clone)?;
    compiler.add(&token::source("fuzz", text)?)?;
    compiler.finish()
}

thread_local! {
    /// The standard library, compiled and checked once.
    static READY: Result<Compiler<Nothing>, Error> = library();
}

fn library() -> Result<Compiler<Nothing>, Error> {
    let mut words = Vocabulary::default();
    words.add("ask", 1, 1);
    let mut compiler = Compiler::new(words);
    compiler.add(&token::source("stdlib.fux", fuxth::STDLIB)?)?;
    let maybe = "define maybe dup 0 gt if else drop then end";
    compiler.add(&token::source("maybe.fux", maybe)?)?;
    compiler.prepare();
    Ok(compiler)
}

/// How a run ended: what it left, or its error; `None` past `STEPS`.
pub fn run(
    program: fuxth::Program<Nothing>,
    budget: u64,
) -> Option<Result<Vec<Value<Nothing>>, Error>> {
    let mut machine = Machine::new(program, LIMITS);
    let budget = budget.clamp(1, STEPS);
    let mut taken = 0u64;
    while taken < STEPS {
        taken = taken.saturating_add(budget);
        match machine.run(&mut Toy, budget) {
            Ok(Outcome::Finished) => return Some(Ok(machine.stack().to_vec())),
            Ok(Outcome::Paused) => {}
            Ok(Outcome::Suspended(n)) => {
                if let Err(e) = machine.push(Value::Int(n.saturating_mul(10))) {
                    return Some(Err(e));
                }
            }
            Err(e) => return Some(Err(e)),
        }
    }
    None
}

/// Whether an error's place is a line of its source, or of the standard library.
pub fn placed(error: &Error, text: &str) -> bool {
    let lines = |t: &str| u32::try_from(t.split('\n').count()).unwrap_or(u32::MAX);
    match &error.place {
        Some(Place::Line { source, line }) => {
            let most = if &**source == "fuzz" {
                lines(text)
            } else {
                lines(fuxth::STDLIB)
            };
            *line >= 1 && *line <= most
        }
        Some(Place::Argument(_)) => false,
        None => true,
    }
}

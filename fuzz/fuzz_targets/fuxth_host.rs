#![allow(dead_code, reason = "each target uses its own part")]
//! What the fuxth targets share: a host of one word and a run to the end.
//! `ask` (n -- n*10) suspends for its answer, as a server's does, and is
//! answered with a failure (`negative`) for n below 0, as a server's wait
//! times out; given no integer it fails at once (`not_a_number`). `maybe`
//! (n -- [n]), defined after the standard library, gives n back only above
//! 0, a value given only sometimes.
use fuxth::{
    AppValue, Args, Done, Error, Failure, Host, Machine, Outcome, Place, Value, Vocabulary, token,
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

    fn write(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// Steps a run may take in all: past them it counts as endless, as a
/// user would stop it.
pub const STEPS: u64 = 100_000;

/// A machine with `text` to read after the standard library. The host
/// reads no files: `require` is refused, so fuzz input never opens a path.
pub fn compile(text: &str) -> Result<Machine<Nothing>, Error> {
    let mut machine = READY.with(Clone::clone)?;
    machine.read(token::source("fuzz", text)?, None);
    Ok(machine)
}

thread_local! {
    /// The standard library, read once.
    static READY: Result<Machine<Nothing>, Error> = library();
}

fn library() -> Result<Machine<Nothing>, Error> {
    let mut words = Vocabulary::default();
    let _ = words.add("ask", "any -- integer");
    let _ = words.raises(fuxth::Code::Host("not_a_number"));
    let _ = words.raises(fuxth::Code::Host("negative"));
    let mut machine = Machine::new(words);
    machine.library("stdlib.fux", fuxth::STDLIB)?;
    let maybe = "# n -- [n] : n, if above 0\ndefine maybe dup 0 gt if else drop then end";
    machine.library("maybe.fux", maybe)?;
    Ok(machine)
}

/// How a run ended: what it left, or its error; `None` past `STEPS`.
pub fn run(
    mut machine: Machine<Nothing>,
    budget: u64,
) -> Option<Result<Vec<Value<Nothing>>, Error>> {
    let budget = budget.clamp(1, STEPS);
    let mut taken = 0u64;
    while taken < STEPS {
        taken = taken.saturating_add(budget);
        match machine.run(&mut Toy, budget) {
            Ok(Outcome::Finished) => return Some(Ok(machine.stack().to_vec())),
            Ok(Outcome::Paused) => {}
            Ok(Outcome::Suspended(n)) => {
                if n < 0 {
                    let failure = Failure::new(fuxth::Code::Host("negative"), "below 0");
                    if let Err(e) = machine.raise(failure, None) {
                        return Some(Err(e));
                    }
                } else {
                    machine.push(Value::Int(n.saturating_mul(10)));
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

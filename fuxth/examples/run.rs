//! Runs a fuxth file with the standard library and no application words, as a
//! program written in the language alone: what it leaves is printed, a
//! value a line; an error is printed as `fuxth: CODE: MESSAGE` and the
//! status is 1. `cargo run --release -p fuxth --example run -- FILE`
use fuxth::{
    AppValue, Args, Compiler, Done, Failure, Host, Limits, Machine, Outcome, STDLIB, Value,
    Vocabulary, token,
};
use std::io::Write;

#[derive(Clone, Debug, PartialEq)]
struct Nothing;

impl AppValue for Nothing {
    fn kind(&self) -> &'static str {
        "nothing"
    }
}

struct Bare;

impl Host for Bare {
    type Value = Nothing;
    type Request = ();
    fn call(
        &mut self,
        _word: usize,
        _args: Args<'_, Nothing>,
        _out: &mut Vec<Value<Nothing>>,
    ) -> Result<Done<()>, Failure> {
        Err(Failure::new(
            fuxth::Code::Host("no_words"),
            "this runner has no application words",
        ))
    }

    fn print(&mut self, line: &str) -> Result<(), Failure> {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{line}")
            .map_err(|e| Failure::new(fuxth::Code::Host("output"), e.to_string()))
    }
}

/// Steps a program may take before it counts as endless.
const STEPS: u64 = 100_000_000;

fn shown(value: &Value<Nothing>) -> String {
    match value {
        Value::Int(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Str(_) | Value::List(_) | Value::App(_) => {
            value.text().unwrap_or_else(|f| f.message)
        }
    }
}

fn run(path: &str) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read: {path}: {e}"))?;
    // This in-memory embedding deliberately supplies no require loader.
    let mut compiler = Compiler::new(Vocabulary::default());
    let compiled = token::source("stdlib.fux", STDLIB)
        .and_then(|t| compiler.add(&t))
        .and_then(|()| token::source(path, &text))
        .and_then(|t| compiler.add(&t))
        .and_then(|()| compiler.finish());
    let program = compiled.map_err(|e| e.to_string())?;
    let mut machine = Machine::new(program, Limits::default());
    match machine.run(&mut Bare, STEPS) {
        Ok(Outcome::Finished) => Ok(machine.stack().iter().map(shown).collect()),
        Ok(Outcome::Paused | Outcome::Suspended(())) => {
            // The innermost step of the program's own, not the library's.
            let calls = machine.calls();
            let ours = calls
                .iter()
                .rev()
                .find(|t| !t.place.to_string().starts_with("stdlib.fux"));
            let place = ours.map_or_else(String::new, |t| format!("{}: {}: ", t.place, t.text));
            Err(format!("steps: {place}ran more than {STEPS} steps"))
        }
        Err(e) => Err(e.to_string()),
    }
}

fn main() -> std::process::ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        let _ = writeln!(std::io::stderr(), "usage: run FILE");
        return std::process::ExitCode::from(2);
    };
    match run(&path) {
        Ok(lines) => {
            let mut out = std::io::stdout().lock();
            for line in lines {
                let _ = writeln!(out, "{line}");
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "fuxth: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

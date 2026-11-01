//! Runs a fuxth file with the standard library and no application words, as a
//! program written in the language alone: what it leaves is printed, a
//! value a line; an error is printed as `fuxth: CODE: MESSAGE` and the
//! status is 1. `cargo run --release -p fuxth --example run -- FILE`
use fuxth::{
    AppValue, Args, Done, Failure, Host, Machine, Outcome, STDLIB, Value, Vocabulary, token,
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

    fn write(&mut self, text: &str) -> Result<(), Failure> {
        let mut out = std::io::stdout().lock();
        write!(out, "{text}").map_err(|e| Failure::new(fuxth::Code::Host("output"), e.to_string()))
    }
}

fn shown(value: &Value<Nothing>) -> String {
    match value {
        Value::Int(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Str(_) | Value::List(_) | Value::App(_) | Value::Xt(_) | Value::Mark(_) => {
            value.text().unwrap_or_else(|f| f.message)
        }
    }
}

fn run(path: &str) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read: {path}: {e}"))?;
    // This in-memory embedding deliberately reads no required files.
    let mut machine = Machine::new(Vocabulary::default());
    machine
        .library("stdlib.fux", STDLIB, &mut Bare)
        .map_err(|e| e.to_string())?;
    let tokens = token::source(path, &text).map_err(|e| e.to_string())?;
    machine.read(tokens, None);
    // A program that never ends is stopped as any program is: Ctrl-C.
    loop {
        match machine.run(&mut Bare, u64::MAX) {
            Ok(Outcome::Finished) => return Ok(machine.stack().iter().map(shown).collect()),
            Ok(Outcome::Paused | Outcome::Suspended(())) => {}
            Err(e) => return Err(e.to_string()),
        }
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

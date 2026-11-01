//! `fuxth`: runs a fuxth program, with words for the terminal, files and
//! the process.
//!
//! - `fuxth FILE ARGS…` runs FILE, any file that reads (a FIFO, `<(…)`).
//! - `fuxth < FILE` and `cat FILE | fuxth` run the program stdin holds.
//! - `fuxth`, stdin and stdout both terminals, is the REPL.
//!
//! A program prints only what it prints; at its end the values left are
//! printed, a value a line, the deepest first, and it exits 0. An uncaught
//! failure says why on stderr, with the steps that had taken effect, and
//! exits 1; Ctrl-C, uncaught, exits 130. A program is stopped as any
//! program is: Ctrl-C, `timeout`, `ulimit`.
mod input;
mod json;
mod words;

use fuxix::process::Signal;
use fuxth::{Code, Error, Failure, Host, Machine, Outcome, STDLIB, Value, token};
use input::{Input, Stop};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use words::{File, Request, Runner};

/// The steps a run takes between looks for Ctrl-C and Ctrl-Z.
const SLICE: u64 = 1 << 16;

/// The status a program ends with once stdout's reader is gone, as `cat`'s
/// is, killed by SIGPIPE.
const PIPE: u8 = 128 + 13;

/// How a run ended.
enum Ended {
    Finished,
    Exit(u8),
    Failed(Error),
    Signal(Signal),
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let file = args.next();
    let repl = file.is_none() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let input = match Input::new(!repl) {
        Ok(input) => input,
        Err(failure) => return failed(failure),
    };
    let mut runner = Runner::new(args.collect(), input, repl);
    let mut machine = match words::vocabulary(repl) {
        Ok(vocabulary) => Machine::new(vocabulary),
        Err(error) => return report(&error, &[]),
    };
    if let Err(error) = machine.library("stdlib.fux", STDLIB, &mut runner) {
        return report(&error, &[]);
    }
    if repl {
        return self::repl(&mut machine, &mut runner);
    }
    let read = match &file {
        Some(file) => {
            (runner.source(file)).map(|(path, text)| (path.display().to_string(), text, Some(path)))
        }
        None => (runner.input.all()).map(|text| ("stdin".to_owned(), text, None)),
    };
    let (name, text, path) = match read {
        Ok(read) => read,
        Err(Stop::Failed(failure)) => return failed(failure),
        Err(Stop::Signal(signal)) => return ended_by(signal),
    };
    match token::source(&name, &text) {
        Ok(tokens) => machine.read(tokens, path),
        Err(error) => return report(&error, &[]),
    }
    let status = match drive(&mut machine, &mut runner) {
        _ if runner.closed() => return ExitCode::from(PIPE),
        Ended::Failed(error) => return report(&uncaught(&machine, error), machine.actions()),
        Ended::Signal(signal) => return ended_by(signal),
        Ended::Finished => ExitCode::SUCCESS,
        Ended::Exit(status) => ExitCode::from(status),
    };
    match left(&mut runner, machine.stack()) {
        Ok(()) => status,
        Err(_) if runner.closed() => ExitCode::from(PIPE),
        Err(failure) => failed(failure),
    }
}

/// The REPL: fuxth's own loop of reading a line and evaluating it, so a
/// definition goes on over lines; an error says why and empties the
/// stacks, the dictionary kept, and the loop begins again.
fn repl(machine: &mut Machine<File>, runner: &mut Runner) -> ExitCode {
    let repl = token::arguments(&["begin", "prompt-line", "evaluate", "false", "until"]);
    loop {
        machine.read(repl.clone(), None);
        match drive(machine, runner) {
            Ended::Exit(status) => return ExitCode::from(status),
            Ended::Signal(signal) => return ended_by(signal),
            Ended::Failed(error) => {
                let _ = report(&uncaught(machine, error), machine.actions());
                machine.clear();
            }
            Ended::Finished => {}
        }
        if runner.closed() {
            return ExitCode::from(PIPE);
        }
    }
}

/// Runs what `machine` was given to its end, doing what its words wait
/// for. Each time it stops, before it goes on, Ctrl-C is raised as
/// `interrupt`, Ctrl-Z done, and an ending signal ends it.
fn drive(machine: &mut Machine<File>, runner: &mut Runner) -> Ended {
    loop {
        let done = match machine.run(runner, SLICE) {
            Ok(Outcome::Finished) => return Ended::Finished,
            Err(error) => return Ended::Failed(error),
            Ok(Outcome::Suspended(Request::Exit(status))) => return Ended::Exit(status),
            Ok(Outcome::Suspended(Request::End(signal))) => return Ended::Signal(signal),
            Ok(Outcome::Paused) if runner.closed() => return Ended::Finished,
            Ok(Outcome::Paused) => Ok(()),
            Ok(Outcome::Suspended(Request::Acted)) => {
                machine.acted(|_| false);
                Ok(())
            }
            Ok(Outcome::Suspended(Request::Line)) => match runner.prompt(machine.stack()) {
                Ok(Some(line)) => {
                    machine.push(Value::str(line));
                    Ok(())
                }
                Ok(None) => return Ended::Exit(0),
                Err(stop) => Err(stop),
            },
        };
        let raised = match done.and_then(|()| runner.input.check()) {
            Ok(()) => continue,
            Err(Stop::Signal(signal)) => return Ended::Signal(signal),
            Err(Stop::Failed(failure)) => machine.raise(failure, None),
        };
        if let Err(error) = raised {
            return Ended::Failed(error);
        }
    }
}

/// Writes the values a program left on stdout, as fux does (D-93): a value
/// a line, a list an item a line, a list in it as one line of JSON. An
/// empty list or string writes nothing; an empty string in a list, a
/// blank line.
fn left(runner: &mut Runner, stack: &[Value<File>]) -> Result<(), Failure> {
    let mut text = String::new();
    for value in stack {
        match value {
            Value::List(list) => {
                for item in list.items() {
                    match item {
                        Value::List(_) => json::write(item, &mut text)?,
                        Value::Int(_)
                        | Value::Str(_)
                        | Value::App(_)
                        | Value::Xt(_)
                        | Value::Mark(_)
                        | Value::Failure(_)
                        | Value::Bool(_) => item.write_text(&mut text)?,
                    }
                    text.push('\n');
                }
            }
            Value::Str(s) if s.is_empty() => {}
            Value::Int(_)
            | Value::Str(_)
            | Value::App(_)
            | Value::Xt(_)
            | Value::Mark(_)
            | Value::Failure(_)
            | Value::Bool(_) => {
                value.write_text(&mut text)?;
                text.push('\n');
            }
        }
    }
    runner.write(&text)
}

/// The runner ended by `signal`, the terminal already back in its own
/// modes: the status a shell gives a program it killed.
fn ended_by(signal: Signal) -> ExitCode {
    ExitCode::from(u8::try_from(signal.raw()).map_or(255, |n| n.saturating_add(128)))
}

/// A failure of the runner's own, before or after the program, reported.
fn failed(failure: Failure) -> ExitCode {
    report(&Error::new(failure.code, failure.message, None), &[])
}

/// A failure no `try` caught, from `machine` where it failed: one inside
/// a word of the standard library's placed at the program's step that
/// called it, with the library word's own step in the message.
fn uncaught(machine: &Machine<File>, mut error: Error) -> Error {
    let stdlib = |place: &fuxth::Place| matches!(place, fuxth::Place::Line { source, .. } if &**source == "stdlib.fux");
    let step = (error.place.as_ref().is_some_and(stdlib))
        .then(|| machine.failed_calls().rev().find(|t| !stdlib(&t.place)));
    if let Some(step) = step.flatten() {
        let inner = error.word.as_deref().unwrap_or_default();
        error.message = format!("inside {}, {inner}: {}", step.text, error.message);
        error.place = Some(step.place);
        error.word = Some(step.text);
    }
    error
}

/// Says on stderr why a program failed, then each step that had taken
/// effect: the status, 130 for Ctrl-C, else 1.
fn report<'a>(error: &Error, actions: impl IntoIterator<Item = &'a fuxth::Token>) -> ExitCode {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "fuxth: {error}");
    for step in actions {
        let _ = writeln!(
            err,
            "fuxth: already took effect: {}: {}",
            step.place, step.text
        );
    }
    match error.code == Code::Interrupt {
        true => ExitCode::from(130),
        false => ExitCode::FAILURE,
    }
}

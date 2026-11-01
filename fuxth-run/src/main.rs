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
mod words;

use fuxth::{Code, Error, Failure, Host, Machine, Outcome, STDLIB, Value, token};
use input::Input;
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
            words::source(file).map(|(path, text)| (path.display().to_string(), text, Some(path)))
        }
        None => (runner.input.all()).map(|text| ("stdin".to_owned(), text, None)),
    };
    let (name, text, path) = match read {
        Ok(read) => read,
        Err(failure) => return failed(failure),
    };
    match token::source(&name, &text) {
        Ok(tokens) => machine.read(tokens, path),
        Err(error) => return report(&error, &[]),
    }
    let status = match drive(&mut machine, &mut runner) {
        _ if runner.closed() => return ExitCode::from(PIPE),
        Ended::Failed(error) => return report(&error, machine.actions()),
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
            Ended::Failed(error) => {
                let _ = report(&error, machine.actions());
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
/// for: between slices of steps, a Ctrl-C raised as `interrupt` and a
/// Ctrl-Z done.
fn drive(machine: &mut Machine<File>, runner: &mut Runner) -> Ended {
    loop {
        let raised = match machine.run(runner, SLICE) {
            Ok(Outcome::Finished) => return Ended::Finished,
            Err(error) => return Ended::Failed(error),
            Ok(Outcome::Suspended(Request::Exit(status))) => return Ended::Exit(status),
            Ok(Outcome::Suspended(Request::Acted)) => {
                if let Some(step) = machine.at() {
                    machine.acted(step);
                }
                Ok(())
            }
            Ok(Outcome::Suspended(Request::Line)) => match runner.prompt(machine.stack()) {
                Ok(Some(line)) => {
                    machine.push(Value::str(line));
                    Ok(())
                }
                Ok(None) => return Ended::Exit(0),
                Err(failure) => Err(failure),
            },
            Ok(Outcome::Paused) if runner.closed() => return Ended::Finished,
            Ok(Outcome::Paused) => runner.input.check(),
        };
        if let Err(failure) = raised
            && let Err(error) = machine.raise(failure, None)
        {
            return Ended::Failed(error);
        }
    }
}

/// Writes the values a program left on stdout, a value a line.
fn left(runner: &mut Runner, stack: &[Value<File>]) -> Result<(), Failure> {
    let mut text = String::new();
    for value in stack {
        value.write_text(&mut text)?;
        text.push('\n');
    }
    runner.write(&text)
}

/// A failure of the runner's own, before or after the program, reported.
fn failed(failure: Failure) -> ExitCode {
    report(&Error::new(failure.code, failure.message, None), &[])
}

/// Says on stderr why a program failed, then each step that had taken
/// effect: the status, 130 for Ctrl-C, else 1.
fn report(error: &Error, actions: &[fuxth::Token]) -> ExitCode {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "fuxth: {error}");
    for step in actions {
        let _ = writeln!(
            err,
            "fuxth: already took effect: {}: {}",
            step.place, step.text
        );
    }
    match error.code == Code::Host(words::INTERRUPT) {
        true => ExitCode::from(130),
        false => ExitCode::FAILURE,
    }
}

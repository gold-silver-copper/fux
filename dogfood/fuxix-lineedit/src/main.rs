//! fuxix-lineedit: a line editor and REPL in raw mode, on fuxix alone.
//!
//! `fuxix-lineedit [--prompt TEXT] [--history FILE]` reads lines with a
//! prompt, cursor movement, word and line kills and a history browsed with
//! the arrows, and answers each with `=> LINE`; `history` lists the lines
//! entered, `exit` (or Ctrl-D on an empty line) ends it. It draws again
//! when the window is resized, and puts the terminal back as it was
//! however it ends: a line, Ctrl-D, SIGTERM, SIGHUP, SIGINT, the terminal
//! going away, an error or a panic. With stdin not a terminal, it reads
//! plain lines and answers them alike.
mod edit;

use edit::{Action, Editor, decode};
use fuxix::Errno;
use fuxix::poll::{Events, PollFd, poll};
use fuxix::process::{Pid, Signal, kill};
use fuxix::terminal::{self, Raw};
use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::time::Duration;

struct Args {
    prompt: String,
    history: Option<std::path::PathBuf>,
}

fn args() -> Result<Args, String> {
    let mut args = Args {
        prompt: "> ".into(),
        history: None,
    };
    let mut given = std::env::args_os().skip(1);
    while let Some(arg) = given.next() {
        let mut value = || {
            given
                .next()
                .ok_or_else(|| format!("{} needs a value", arg.display()))
        };
        match arg.to_str() {
            Some("--prompt") => args.prompt = value()?.to_string_lossy().into_owned(),
            Some("--history") => args.history = Some(value()?.into()),
            Some("--help" | "-h") => {
                return Err("usage: fuxix-lineedit [--prompt TEXT] [--history FILE]".into());
            }
            _ => return Err(format!("unknown argument {}", arg.display())),
        }
    }
    Ok(args)
}

/// What the REPL answers a line with; `None` to end.
fn answer(line: &str, editor: &Editor) -> Option<String> {
    match line.trim() {
        "exit" | "quit" => None,
        "history" => Some(
            (editor.history().iter().enumerate())
                .map(|(n, line)| format!("{:>4}  {line}\n", n.saturating_add(1)))
                .collect(),
        ),
        _ => Some(format!("=> {line}\n")),
    }
}

fn load_history(path: Option<&std::path::Path>) -> Result<Vec<String>, String> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text.lines().map(str::to_owned).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn save_history(path: Option<&std::path::Path>, line: &str) -> Result<(), String> {
    let Some(path) = path else { return Ok(()) };
    let mut file = (std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path))
    .and_then(|mut file| writeln!(file, "{line}").map(|()| file))
    .map_err(|e| format!("{}: {e}", path.display()))?;
    file.flush().map_err(|e| format!("{}: {e}", path.display()))
}

/// Why the loop ended: the exit code.
enum End {
    Done,
    Signalled(Signal),
}

fn main() -> ExitCode {
    fuxix::terminal::open_stdio();
    let result = args().and_then(|args| match fuxix::terminal::on_one_terminal() {
        true => edit_lines(&args),
        false => plain_lines(&args),
    });
    match result {
        Ok(End::Done) => ExitCode::SUCCESS,
        Ok(End::Signalled(signal)) => {
            ExitCode::from(u8::try_from(signal.raw().saturating_add(128)).unwrap_or(1))
        }
        Err(message) => {
            eprintln!("fuxix-lineedit: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Without a terminal: a line at a time, as given.
fn plain_lines(args: &Args) -> Result<End, String> {
    let mut editor = Editor::with_history(load_history(args.history.as_deref())?);
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| format!("reading: {e}"))?;
        if editor.remember(&line) {
            save_history(args.history.as_deref(), &line)?;
        }
        let Some(reply) = answer(&line, &editor) else {
            break;
        };
        out.write_all(reply.as_bytes())
            .map_err(|e| format!("writing: {e}"))?;
    }
    Ok(End::Done)
}

/// The terminal's width, read again for each draw.
fn cols() -> u16 {
    terminal::stdin_size().map_or(80, |(size, _)| size.cols.get())
}

fn write_out(bytes: &str) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    (out.write_all(bytes.as_bytes()).and_then(|()| out.flush()))
        .map_err(|e| format!("writing: {e}"))
}

/// How long a lone ESC waits for the rest of a sequence.
const ESC_WAIT: Duration = Duration::from_millis(50);

fn edit_lines(args: &Args) -> Result<End, String> {
    let signals =
        fuxix::signal::Pipe::new(&[Signal::Winch, Signal::Term, Signal::Hup, Signal::Int])
            .map_err(|e| format!("signals: {e}"))?;
    let mut editor = Editor::with_history(load_history(args.history.as_deref())?);
    let raw_mode = || Raw::enter(std::io::stdin()).map_err(|e| format!("raw mode: {e}"));
    let mut raw = Some(raw_mode()?);
    let mut pending: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 1024];
    write_out(&editor.render(&args.prompt, cols()))?;
    loop {
        let wait = (!pending.is_empty()).then_some(ESC_WAIT);
        let stdin = std::io::stdin();
        let mut fds = [
            PollFd::new(&stdin, Events::IN),
            PollFd::new(signals.reader(), Events::IN),
        ];
        let ready = match poll(&mut fds, wait) {
            Ok(ready) => ready,
            Err(Errno::INTR) => continue,
            Err(e) => return Err(format!("poll: {e}")),
        };
        let [input, _] = fds.map(|fd| fd.revents());
        if input.intersects(Events::NVAL) {
            // macOS's poll says so of /dev/tty: give the terminal's own device.
            return Err("the terminal on stdin can't be waited on".into());
        }
        let mut resized = false;
        while let Some(signal) = signals.next() {
            if signal != Signal::Winch {
                write_out("\r\n")?;
                return Ok(End::Signalled(signal));
            }
            resized = true;
        }
        if resized {
            write_out(&editor.render(&args.prompt, cols()))?;
        }
        if input.intersects(Events::IN | Events::HUP | Events::ERR) {
            match fuxix::io::read(&stdin, &mut buffer) {
                // The terminal is gone: nothing more will come.
                Ok(0) | Err(Errno::IO) => return Ok(End::Done),
                Ok(n) => pending.extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(Errno::INTR | Errno::AGAIN) => {}
                Err(e) => return Err(format!("reading: {e}")),
            }
        } else if ready == 0 {
            // Nothing followed in time: what waits is a lone ESC.
            pending.clear();
        }
        let mut taken = 0;
        while let Some((key, n)) = decode(pending.get(taken..).unwrap_or_default()) {
            taken = taken.saturating_add(n.max(1));
            let Some(action) = editor.apply(key) else {
                continue;
            };
            match action {
                Action::Redraw => {}
                Action::Submit(line) => {
                    write_out("\r\n")?;
                    if editor.remember(&line) {
                        save_history(args.history.as_deref(), &line)?;
                    }
                    let Some(reply) = answer(&line, &editor) else {
                        return Ok(End::Done);
                    };
                    write_out(&reply.replace('\n', "\r\n"))?;
                }
                Action::Cancel => write_out("^C\r\n")?,
                Action::Eof => {
                    write_out("\r\n")?;
                    return Ok(End::Done);
                }
                Action::ClearScreen => write_out("\x1b[H\x1b[2J")?,
                Action::Suspend => {
                    // Windows has no stop: Ctrl-Z redraws the line.
                }
            }
            write_out(&editor.render(&args.prompt, cols()))?;
        }
        pending.drain(..taken.min(pending.len()));
    }
}

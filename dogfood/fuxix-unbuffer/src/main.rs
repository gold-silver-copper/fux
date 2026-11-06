//! `unbuffer COMMAND [ARGS...]`: runs COMMAND with its stdout and stderr on a
//! PTY, so it line-buffers and colours as it would in a terminal, and copies
//! what it writes to our stdout as it comes. Its stdin is ours, so it works
//! at either end of a pipeline. Our exit status is its own, or 128 + the
//! signal that killed it.
use fuxix::Errno;
use fuxix::poll::{Events, PollFd};
use fuxix::process::{Dispositions, Signal};
use fuxix::pty::Master;
use fuxix::signal::Pipe;
use fuxix::terminal::{self, Size};
use std::io::{ErrorKind, Write};
use std::process::ExitCode;

/// The signals passed on to the command's process group.
const FORWARDED: [Signal; 4] = [Signal::Int, Signal::Term, Signal::Hup, Signal::Quit];

/// The size the command is told when we have no terminal to copy one from.
const DEFAULT_SIZE: (u16, u16) = (24, 80);

const USAGE: &str = "usage: unbuffer [--] COMMAND [ARGS...]\n\
    Runs COMMAND with stdout and stderr on a terminal, copying its output to stdout.\n";

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let argv = match args.first().and_then(|a| a.to_str()) {
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("--") => args.get(1..).unwrap_or_default(),
        Some(flag) if flag.starts_with('-') && flag.len() > 1 => {
            eprint!("unbuffer: unknown option {flag}\n{USAGE}");
            return ExitCode::from(2);
        }
        _ => &args[..],
    };
    if argv.is_empty() {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
    match run(argv) {
        Ok(code) => ExitCode::from(code),
        Err(Failure { message, code }) => {
            if !message.is_empty() {
                eprintln!("unbuffer: {message}");
            }
            ExitCode::from(code)
        }
    }
}

/// Why we could not run the command, and the status that says so.
struct Failure {
    message: String,
    code: u8,
}

fn failed<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> Failure {
    move |error| Failure {
        message: format!("{what}: {error}"),
        code: 1,
    }
}

/// The size of the terminal our stderr is on, if it is on one.
fn our_size() -> Option<Size> {
    terminal::stdin_size().map(|(size, _)| size)
}

/// What reading the master found.
enum Copied {
    /// Bytes, copied out.
    Some,
    /// Nothing now.
    Waiting,
    /// No program has the PTY open any more.
    Ended,
}

/// Copies what the master holds to `out`, until it would wait.
fn copy(master: &Master, out: &mut impl Write, buffer: &mut [u8]) -> Result<Copied, Failure> {
    let mut read = Copied::Waiting;
    loop {
        match fuxix::io::read(master, buffer) {
            Ok(0) | Err(Errno::IO) => return Ok(Copied::Ended),
            Ok(n) => {
                let bytes = buffer.get(..n).unwrap_or_default();
                out.write_all(bytes).map_err(output_failed)?;
                read = Copied::Some;
            }
            Err(Errno::AGAIN) => break,
            Err(Errno::INTR) => {}
            Err(errno) => return Err(failed("reading the PTY")(std::io::Error::from(errno))),
        }
    }
    out.flush().map_err(output_failed)?;
    Ok(read)
}

/// A write to our stdout failed: a closed pipe is told by its status, as a
/// program killed by SIGPIPE.
fn output_failed(error: std::io::Error) -> Failure {
    if error.kind() == ErrorKind::BrokenPipe {
        Failure {
            message: String::new(),
            code: 128 + 13,
        }
    } else {
        failed("writing stdout")(error)
    }
}

fn run(argv: &[std::ffi::OsString]) -> Result<u8, Failure> {
    let size = our_size().or(Size::new(DEFAULT_SIZE.0, DEFAULT_SIZE.1));
    let size = size.ok_or_else(|| failed("sizing the PTY")("a zero size"))?;
    let _ = fuxix::pty::load(std::path::Path::new("no-pinned-conpty"));
    let (master, slave) = fuxix::pty::open(size).map_err(failed("opening a PTY"))?;
    // Before the spawn, so that an exit at once is not missed.
    let mut caught = FORWARDED.to_vec();
    caught.extend([Signal::Winch, Signal::Chld]);
    let signals = Pipe::new(&caught).map_err(failed("catching signals"))?;
    let program = argv
        .first()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let job = slave
        .spawn(argv, |_| {}, Dispositions::Default)
        .map_err(|error| Failure {
        code: if error.kind() == ErrorKind::NotFound {
            127
        } else {
            126
        },
        message: format!("{program}: {error}"),
    })?;
    let copied = pump(&master, &job, &signals);
    if copied.is_err() {
        // Nobody reads what it writes any more: hang it up, as closing a
        // terminal would.
        let _ = fuxix::pty::hang_up(&master, &job);
    }
    let status = loop {
        match job.ended().map_err(failed("waiting for the command"))? {
            Some(status) => break status,
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    };
    copied?;
    let fuxix::process::Status::Exited(code) = status;
    Ok(u8::try_from(code & 0xff).unwrap_or(1))
}

/// Copies the PTY's output to stdout and passes signals on, until the
/// command has exited and its output is read.
fn pump(master: &Master, job: &fuxix::process::Job, signals: &Pipe) -> Result<(), Failure> {
    let mut out = std::io::stdout().lock();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let mut fds = [
            PollFd::new(master, Events::IN),
            PollFd::new(signals.reader(), Events::IN),
        ];
        match fuxix::poll::poll(&mut fds, None) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(errno) => return Err(failed("waiting")(std::io::Error::from(errno))),
        }
        while let Some(signal) = signals.next() {
            match signal {
                Signal::Winch => {
                    if let Some(size) = our_size() {
                        let _ = terminal::set_window_size(master, size, None);
                    }
                }
                Signal::Chld => {}
                // It leads its own session, so its group's ID is its pid.
                _ => {
                    let _ = fuxix::pty::hang_up(master, job);
                }
            }
        }
        // Asked before the read: once it has ended, what the read finds up
        // to AGAIN is all it wrote.
        let ended = matches!(job.ended(), Ok(Some(_)));
        if let Copied::Ended = copy(master, &mut out, &mut buffer)? {
            return Ok(());
        }
        if ended {
            return Ok(());
        }
    }
}

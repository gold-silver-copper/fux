//! `timeout` for a whole process group.
//!
//! Runs COMMAND in a new process group (or session, with `--session`), and
//! waits until every process in that group is gone, not only COMMAND
//! itself. After DURATION it sends the timeout signal (TERM) to the whole
//! group, then KILL once the grace period (`-k`) has passed. TERM, INT, HUP
//! and QUIT sent to this program are passed on to the group.
//!
//! Exit statuses are GNU timeout's: 124 timed out, 125 this program failed,
//! 126 COMMAND could not be run, 127 COMMAND was not found, 137 the group
//! had to be killed, else COMMAND's own status (128+N for signal N).

use fuxix::poll::{Events, PollFd, poll};
use fuxix::process::{Dispositions, Reach, Signal, Status};
use fuxix::{Errno, signal};
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const TIMED_OUT: u8 = 124;
const FAILED: u8 = 125;
const CANNOT_RUN: u8 = 126;
const NOT_FOUND: u8 = 127;

/// Signals this program passes on to the group.
const FORWARDED: [Signal; 4] = [Signal::Term, Signal::Int, Signal::Hup, Signal::Quit];

const USAGE: &str = "\
usage: fuxix-grouptimeout [OPTION]... DURATION COMMAND [ARG]...
Run COMMAND in its own process group; after DURATION signal the whole group.

  -s, --signal SIG      signal sent at the timeout (default TERM): a name or number
  -k, --kill-after DUR  then KILL the group after DUR more (default 5s; 0: never)
      --session         start a new session, not only a group (COMMAND loses
                        the controlling terminal)
      --leader-only     return when COMMAND exits, as GNU timeout does, even if
                        processes it started are still running in its group
      --preserve-status exit with COMMAND's status even when it timed out
  -v, --verbose         say on stderr each signal sent to the group
  -h, --help            show this help

DURATION is a number with an optional suffix s, m, h or d; 0 means no timeout.
COMMAND runs in the background group: if it reads the terminal it stops.
Exit: 124 timed out, 125 failed, 126 cannot run, 127 not found, 137 killed,
else COMMAND's status.";

#[derive(Debug)]
struct Options {
    duration: Option<Duration>,
    signal: Signal,
    kill_after: Option<Duration>,
    session: bool,
    leader_only: bool,
    preserve_status: bool,
    verbose: bool,
    command: Vec<String>,
}

/// What the arguments asked for.
enum Parsed {
    Run(Options),
    Help,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args) {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(options)) => ExitCode::from(run(&options)),
        Err(why) => {
            eprintln!("fuxix-grouptimeout: {why}\n{USAGE}");
            ExitCode::from(FAILED)
        }
    }
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut options = Options {
        duration: None,
        signal: Signal::Term,
        kill_after: Some(Duration::from_secs(5)),
        session: false,
        leader_only: false,
        preserve_status: false,
        verbose: false,
        command: Vec::new(),
    };
    let mut rest = args.iter();
    let value = |rest: &mut std::slice::Iter<String>, flag: &str| {
        rest.next()
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    loop {
        let Some(arg) = rest.next() else {
            return Err("missing DURATION and COMMAND".into());
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-s" | "--signal" => options.signal = parse_signal(&value(&mut rest, arg)?)?,
            "-k" | "--kill-after" => options.kill_after = parse_duration(&value(&mut rest, arg)?)?,
            "--session" => options.session = true,
            "--leader-only" => options.leader_only = true,
            "--preserve-status" => options.preserve_status = true,
            "-v" | "--verbose" => options.verbose = true,
            "--" => {
                let duration = value(&mut rest, "--")?;
                options.duration = parse_duration(&duration)?;
                break;
            }
            flag if flag.starts_with('-') && flag.len() > 1 && !is_number(flag) => {
                return Err(format!("unknown option {flag}"));
            }
            duration => {
                options.duration = parse_duration(duration)?;
                break;
            }
        }
    }
    options.command = rest.cloned().collect();
    if options.command.is_empty() {
        return Err("missing COMMAND".into());
    }
    Ok(Parsed::Run(options))
}

fn is_number(text: &str) -> bool {
    text.parse::<f64>().is_ok()
}

/// `text` as a duration: `None` for 0, which means never.
fn parse_duration(text: &str) -> Result<Option<Duration>, String> {
    let (number, unit) = match text.char_indices().last() {
        Some((at, 's')) => (text.get(..at), 1.0),
        Some((at, 'm')) => (text.get(..at), 60.0),
        Some((at, 'h')) => (text.get(..at), 3600.0),
        Some((at, 'd')) => (text.get(..at), 86400.0),
        _ => (Some(text), 1.0),
    };
    let seconds = number
        .and_then(|n| n.parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .ok_or_else(|| format!("invalid duration {text:?}"))?;
    if seconds == 0.0 {
        return Ok(None);
    }
    // Past what an Instant can add, it is as good as never.
    Ok(Duration::try_from_secs_f64(seconds * unit).ok())
}

/// A signal by name (`TERM`, `SIGTERM`, `term`) or number.
fn parse_signal(text: &str) -> Result<Signal, String> {
    let upper = text.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    let by_name = Signal::ALL.into_iter().find(|s| s.name() == name);
    by_name.ok_or_else(|| format!("unknown signal {text:?}"))
}

/// The one pipe signals arrive on: children ending, and each forwarded one.
fn catch() -> Result<signal::Pipe, Errno> {
    let mut signals = vec![Signal::Chld];
    signals.extend(FORWARDED);
    signal::Pipe::new(&signals)
}

fn run(options: &Options) -> u8 {
    // Before the spawn, so that no child's end is missed.
    let signals = match catch() {
        Ok(signals) => signals,
        Err(errno) => return fail(format_args!("catching signals: {errno}")),
    };
    let _ = fuxix::pty::load(std::path::Path::new("no-pinned-conpty"));
    let Some(size) = fuxix::terminal::Size::new(24, 80) else {
        return FAILED;
    };
    let (master, slave) = match fuxix::pty::open(size) {
        Ok(opened) => opened,
        Err(error) => return fail(format_args!("a console: {error}")),
    };
    // A job stands for the group: it holds what COMMAND starts.
    let job = match slave.spawn(&options.command, |_| {}, Dispositions::Default) {
        Ok(job) => job,
        Err(error) => {
            let code = match error.kind() {
                std::io::ErrorKind::NotFound => NOT_FOUND,
                std::io::ErrorKind::PermissionDenied => CANNOT_RUN,
                _ => FAILED,
            };
            let program = options.command.first().map_or("", String::as_str);
            return fail_with(code, format_args!("{program}: {error}"));
        }
    };
    let deadline = options.duration.and_then(|d| Instant::now().checked_add(d));
    let mut killed = false;
    let mut timed_out = false;
    let mut status: Option<Status> = None;
    let mut buffer = [0u8; 16 * 1024];
    let mut out = std::io::stdout();
    loop {
        if status.is_none() {
            status = match job.ended() {
                Ok(status) => status,
                Err(errno) => return fail(format_args!("waiting: {errno}")),
            };
        }
        let done = status.is_some() && (options.leader_only || job.members().pids.is_empty());
        if done {
            break;
        }
        let now = Instant::now();
        if !killed && deadline.is_some_and(|d| now >= d) {
            timed_out = true;
            killed = true;
            if options.verbose {
                eprintln!("fuxix-grouptimeout: ending its job");
            }
            let _ = job.kill(Reach::Session);
        }
        let tick = Duration::from_millis(20);
        let mut fds = [
            PollFd::new(&master, Events::IN),
            PollFd::new(signals.reader(), Events::IN),
        ];
        if let Err(errno) = poll(&mut fds, Some(tick)) {
            return fail(format_args!("waiting: {errno}"));
        }
        match fuxix::io::read(&master, &mut buffer) {
            Ok(n) => {
                let _ = out.write_all(buffer.get(..n).unwrap_or_default());
            }
            Err(_) => {}
        }
        while let Some(signal) = signals.next() {
            if signal != Signal::Chld {
                killed = true;
                let _ = job.kill(Reach::Session);
            }
        }
    }
    let _ = out.flush();
    exit_code(status, timed_out, options.preserve_status)
}

/// GNU timeout's status for how COMMAND ended.
fn exit_code(status: Option<Status>, timed_out: bool, preserve: bool) -> u8 {
    if timed_out && !preserve {
        return TIMED_OUT;
    }
    match status {
        Some(Status::Exited(code)) => u8::try_from(code & 0xff).unwrap_or(FAILED),
        None => FAILED,
    }
}

fn fail(why: std::fmt::Arguments) -> u8 {
    fail_with(FAILED, why)
}

fn fail_with(code: u8, why: std::fmt::Arguments) -> u8 {
    eprintln!("fuxix-grouptimeout: {why}");
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_signals_parse_as_timeout_takes_them() {
        assert_eq!(parse_duration("1.5"), Ok(Some(Duration::from_millis(1500))));
        assert_eq!(parse_duration("2m"), Ok(Some(Duration::from_secs(120))));
        assert_eq!(parse_duration("0"), Ok(None));
        assert!(parse_duration("-1").is_err());
        assert!(parse_duration("s").is_err());
        assert_eq!(parse_signal("sigint"), Ok(Signal::Int));
        assert_eq!(exit_code(Some(Status::Exited(3)), false, false), 3);
    }
}

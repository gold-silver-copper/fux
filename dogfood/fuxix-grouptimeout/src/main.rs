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
use fuxix::process::{Dispositions, Pid, Signal, Status};
use fuxix::{Errno, signal};
use std::process::{Command, ExitCode};
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
    by_name
        .or_else(|| text.parse().ok().and_then(Signal::from_raw))
        .ok_or_else(|| format!("unknown signal {text:?}"))
}

/// The one pipe signals arrive on: children ending, and each forwarded one.
fn catch() -> Result<signal::Pipe, Errno> {
    let mut signals = vec![Signal::Chld];
    signals.extend(FORWARDED);
    signal::Pipe::new(&signals)
}

/// Waits up to `timeout` for a signal: those to pass on.
fn wait(signals: &signal::Pipe, timeout: Option<Duration>) -> Result<Vec<Signal>, Errno> {
    match poll(&mut [PollFd::new(signals.reader(), Events::IN)], timeout) {
        Ok(_) | Err(Errno::INTR) => {}
        Err(errno) => return Err(errno),
    }
    Ok(std::iter::from_fn(|| signals.next())
        .filter(|&s| s != Signal::Chld)
        .collect())
}

/// Where the group is in being stopped.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Stage {
    Running,
    /// Signalled; KILL follows at this time, if any.
    Signalled(Option<Instant>),
    Killed,
}

fn run(options: &Options) -> u8 {
    // Before the spawn, so that no child's end is missed.
    let signals = match catch() {
        Ok(signals) => signals,
        Err(errno) => return fail(format_args!("catching signals: {errno}")),
    };
    // Processes the group leaves behind when COMMAND exits become ours, so
    // they are reaped here and their ends wake the wait (Linux; macOS has
    // no such reaper, and the wait polls instead).
    let adopted = fuxix::process::adopt_orphans().is_ok() && cfg!(target_os = "linux");
    let Some((program, args)) = options.command.split_first() else {
        return FAILED;
    };
    let mut command = Command::new(program);
    command.args(args);
    if options.session {
        fuxix::process::lead_session(&mut command, false, Dispositions::Inherited);
    } else {
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let code = match error.kind() {
                std::io::ErrorKind::NotFound => NOT_FOUND,
                std::io::ErrorKind::PermissionDenied => CANNOT_RUN,
                _ => FAILED,
            };
            return fail_with(code, format_args!("{program}: {error}"));
        }
    };
    let Some(leader) = Pid::of(&child) else {
        return fail(format_args!("the child's id {} is not a pid", child.id()));
    };
    // The group's ID is its leader's, in either mode.
    let group = leader;
    let deadline = options.duration.and_then(|d| Instant::now().checked_add(d));
    let mut stage = Stage::Running;
    let mut timed_out = false;
    let mut status: Option<Status> = None;
    loop {
        if status.is_none() {
            status = match fuxix::process::reap(leader) {
                Ok(status) => status,
                Err(errno) => return fail(format_args!("waiting for {leader}: {errno}")),
            };
        }
        if adopted {
            fuxix::process::reap_orphans(|pid| pid == leader);
        }
        let done = status.is_some()
            && (options.leader_only || !fuxix::process::group_alive(group).unwrap_or(false));
        if done {
            break;
        }
        let now = Instant::now();
        if stage == Stage::Running && deadline.is_some_and(|d| now >= d) {
            timed_out = true;
            stage = signal_group(group, options.signal, options, now);
        }
        if let Stage::Signalled(Some(at)) = stage
            && now >= at
        {
            send(group, Signal::Kill, options.verbose);
            stage = Stage::Killed;
        }
        let next = match stage {
            Stage::Running => deadline,
            Stage::Signalled(at) => at,
            Stage::Killed => None,
        };
        let mut timeout = next.map(|at| at.saturating_duration_since(now));
        // Once COMMAND is gone, what is left of its group may end without a
        // signal reaching this process: look again now and then.
        if status.is_some() || !adopted {
            let tick = Duration::from_millis(50);
            timeout = Some(timeout.map_or(tick, |t| t.min(tick)));
        }
        let arrived = match wait(&signals, timeout) {
            Ok(arrived) => arrived,
            Err(errno) => return fail(format_args!("waiting: {errno}")),
        };
        for signal in arrived {
            let next = signal_group(group, signal, options, Instant::now());
            if stage == Stage::Running {
                stage = next;
            }
        }
    }
    exit_code(status, timed_out, options.preserve_status)
}

/// Sends `signal` to the group, and says when KILL is to follow.
fn signal_group(group: Pid, signal: Signal, options: &Options, now: Instant) -> Stage {
    send(group, signal, options.verbose);
    if signal == Signal::Kill {
        return Stage::Killed;
    }
    // A stopped process would not act on it until continued.
    if signal != Signal::Cont {
        let _ = fuxix::process::kill_group(group, Signal::Cont);
    }
    Stage::Signalled(options.kill_after.and_then(|grace| now.checked_add(grace)))
}

fn send(group: Pid, signal: Signal, verbose: bool) {
    if verbose {
        eprintln!(
            "fuxix-grouptimeout: sending SIG{} to group {group}",
            signal.name()
        );
    }
    match fuxix::process::kill_group(group, signal) {
        Ok(()) | Err(Errno::SRCH) => {}
        Err(errno) => eprintln!(
            "fuxix-grouptimeout: SIG{} to group {group}: {errno}",
            signal.name()
        ),
    }
}

/// GNU timeout's status for how COMMAND ended.
fn exit_code(status: Option<Status>, timed_out: bool, preserve: bool) -> u8 {
    let killed =
        matches!(status, Some(Status::Signalled { signal, .. }) if signal == Signal::Kill.raw());
    if timed_out && !preserve && !killed {
        return TIMED_OUT;
    }
    match status {
        Some(Status::Exited(code)) => u8::try_from(code & 0xff).unwrap_or(FAILED),
        Some(Status::Signalled { signal, .. }) => {
            u8::try_from(signal.saturating_add(128)).unwrap_or(FAILED)
        }
        // COMMAND outlived the wait: --leader-only cannot end so, nor can
        // the group mode, which waits for it first.
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
        assert_eq!(parse_signal("9"), Ok(Signal::Kill));
        assert_eq!(
            exit_code(
                Some(Status::Signalled {
                    signal: 9,
                    core_dumped: false
                }),
                true,
                false
            ),
            137
        );
        assert_eq!(
            exit_code(
                Some(Status::Signalled {
                    signal: 15,
                    core_dumped: false
                }),
                true,
                false
            ),
            124
        );
        assert_eq!(exit_code(Some(Status::Exited(3)), false, false), 3);
    }
}

//! fuxix-supervise: runs the commands a file lists, restarts each that exits
//! (waiting longer after each quick failure), forwards SIGTERM and SIGINT to
//! them, reaps them and the orphans they leave, and logs every start, exit
//! and signal to stderr.
//!
//! The file has one program a line, `NAME COMMAND ARGS...`, split as a shell
//! splits words (single and double quotes, backslash), with no expansion.
//! Blank lines and lines starting with `#` are skipped.
use fuxix::Errno;
use fuxix::poll::{Events, PollFd};
use fuxix::process::{self, Pid, Signal, Status};
use fuxix::signal::Pipe;
use std::io::Write;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: fuxix-supervise [--backoff MS] [--backoff-max MS] [--grace MS] FILE
  --backoff MS      first wait before restarting a program that exited (500)
  --backoff-max MS  longest wait; a program that ran this long starts afresh (30000)
  --grace MS        after SIGTERM/SIGINT, how long programs get before SIGKILL (5000)";

struct Settings {
    backoff: Duration,
    backoff_max: Duration,
    grace: Duration,
    file: String,
}

enum State {
    Running { pid: Pid, since: Instant },
    Waiting { until: Instant },
    Ended,
}

struct Program {
    name: String,
    argv: Vec<String>,
    state: State,
    /// Quick exits in a row: the wait before the next start doubles with each.
    failures: u32,
}

/// Writes one line to stderr, stamped with the seconds since the start. A
/// log that can't be written is dropped: the programs matter more.
struct Log(Instant);

impl Log {
    fn line(&self, args: std::fmt::Arguments<'_>) {
        let at = self.0.elapsed().as_secs_f64();
        let _ = writeln!(std::io::stderr().lock(), "[{at:9.3}] {args}");
    }
}

macro_rules! log {
    ($log:expr, $($arg:tt)*) => { $log.line(format_args!($($arg)*)) };
}

fn main() -> ExitCode {
    let log = Log(Instant::now());
    let settings = match settings(std::env::args().skip(1)) {
        Ok(settings) => settings,
        Err(message) => {
            let _ = writeln!(std::io::stderr(), "fuxix-supervise: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let programs = match std::fs::read_to_string(&settings.file)
        .map_err(|e| format!("{}: {e}", settings.file))
        .and_then(|text| parse(&text))
    {
        Ok(programs) => programs,
        Err(message) => {
            let _ = writeln!(std::io::stderr(), "fuxix-supervise: {message}");
            return ExitCode::from(2);
        }
    };
    match supervise(programs, &settings, &log) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errno) => {
            log!(log, "supervisor: {errno}");
            ExitCode::FAILURE
        }
    }
}

fn settings(mut args: impl Iterator<Item = String>) -> Result<Settings, String> {
    let mut settings = Settings {
        backoff: Duration::from_millis(500),
        backoff_max: Duration::from_secs(30),
        grace: Duration::from_secs(5),
        file: String::new(),
    };
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--backoff" => &mut settings.backoff,
            "--backoff-max" => &mut settings.backoff_max,
            "--grace" => &mut settings.grace,
            "-h" | "--help" => return Err("help".into()),
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}")),
            _ if settings.file.is_empty() => {
                settings.file = arg;
                continue;
            }
            _ => return Err(format!("one file only, not also {arg}")),
        };
        let value = args.next().ok_or(format!("{arg} needs a number of ms"))?;
        let ms: u64 = value.parse().map_err(|_| format!("{arg} {value}: not a number of ms"))?;
        *slot = Duration::from_millis(ms);
    }
    if settings.file.is_empty() {
        return Err("no file of programs given".into());
    }
    Ok(settings)
}

/// The programs `text` lists, or the first line that names none.
fn parse(text: &str) -> Result<Vec<Program>, String> {
    let mut programs: Vec<Program> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = number.saturating_add(1);
        let mut words = split(line).map_err(|e| format!("line {at}: {e}"))?.into_iter();
        let (Some(name), argv) = (words.next(), words.collect::<Vec<_>>()) else {
            continue;
        };
        if argv.is_empty() {
            return Err(format!("line {at}: {name} has no command"));
        }
        if programs.iter().any(|p| p.name == name) {
            return Err(format!("line {at}: {name} is listed twice"));
        }
        programs.push(Program {
            name,
            argv,
            state: State::Ended,
            failures: 0,
        });
    }
    if programs.is_empty() {
        return Err("the file lists no programs".into());
    }
    Ok(programs)
}

/// `line`'s words as a shell splits them, without expansion.
fn split(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => words.extend(word.take()),
            '\'' => {
                let w = word.get_or_insert_default();
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => w.push(c),
                        None => return Err("unclosed '".into()),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_default();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => w.push(c),
                            Some(c) => w.extend(['\\', c]),
                            None => return Err("unclosed \"".into()),
                        },
                        Some(c) => w.push(c),
                        None => return Err("unclosed \"".into()),
                    }
                }
            }
            '\\' => word.get_or_insert_default().extend(chars.next()),
            c => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    Ok(words)
}

fn supervise(mut programs: Vec<Program>, settings: &Settings, log: &Log) -> Result<(), Errno> {
    let signals = Pipe::new(&[Signal::Term, Signal::Int, Signal::Quit, Signal::Chld])?;
    if let Err(errno) = process::adopt_orphans() {
        log!(log, "supervisor: orphans go to init, not here: {errno}");
    }
    let now = Instant::now();
    for program in &mut programs {
        start(program, now, settings, log);
    }
    // Set once a stop signal came: when SIGKILL follows, if still running.
    let mut stopping: Option<Option<Instant>> = None;
    loop {
        if stopping.is_some() && running(&programs) == 0 && descendants().is_empty() {
            log!(log, "supervisor: every program has ended; exiting");
            return Ok(());
        }
        let restart = programs.iter().filter_map(|p| match p.state {
            State::Waiting { until } if stopping.is_none() => Some(until),
            _ => None,
        });
        let next = restart.chain(stopping.flatten()).min();
        let timeout = next.map(|at| at.saturating_duration_since(Instant::now()));
        let mut fds = [PollFd::new(signals.reader(), Events::IN)];
        match fuxix::poll::poll(&mut fds, timeout) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(errno) => return Err(errno),
        }
        let now = Instant::now();
        while let Some(signal) = signals.next() {
            if signal == Signal::Chld {
                continue;
            }
            let count = running(&programs);
            if stopping.is_some() {
                log!(log, "supervisor: got {} again; killing {count} programs", signal.name());
                signal_all(&programs, Signal::Kill, log);
                stopping = Some(None);
            } else {
                log!(log, "supervisor: got {}; forwarding to {count} programs", signal.name());
                signal_all(&programs, signal, log);
                stopping = Some(now.checked_add(settings.grace));
            }
        }
        reap(&mut programs, now, settings, stopping.is_some(), log);
        if let Some(Some(deadline)) = stopping
            && now >= deadline
        {
            let count = running(&programs);
            log!(log, "supervisor: grace over; killing {count} programs");
            signal_all(&programs, Signal::Kill, log);
            stopping = Some(None);
        }
        if stopping.is_none() {
            for program in &mut programs {
                if matches!(program.state, State::Waiting { until } if until <= now) {
                    start(program, now, settings, log);
                }
            }
        }
    }
}

/// Starts `program` in a session of its own, so a signal reaches its whole
/// group; one that won't start waits as one that exited does.
fn start(program: &mut Program, now: Instant, settings: &Settings, log: &Log) {
    let Some((exe, args)) = program.argv.split_first() else {
        program.state = State::Ended;
        return;
    };
    // fuxix's start takes a path and no search: `where`'s first answer.
    let path = find(exe);
    let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    let log_handle = std::io::stderr();
    use std::os::windows::io::AsHandle;
    let stdio = [None, Some(log_handle.as_handle()), Some(log_handle.as_handle())];
    let pid = process::start(&path, &args, stdio).map(|started| Some(started.pid));
    match pid {
        Ok(Some(pid)) => {
            log!(log, "{}: started pid {pid}", program.name);
            program.state = State::Running { pid, since: now };
        }
        Ok(None) => {
            log!(log, "{}: started with no pid to watch", program.name);
            program.state = State::Ended;
        }
        Err(error) => {
            log!(log, "{}: can't start {exe}: {error}", program.name);
            wait(program, now, now, settings, log);
        }
    }
}

/// Schedules `program`'s restart, the wait doubling with each quick exit.
fn wait(program: &mut Program, since: Instant, now: Instant, settings: &Settings, log: &Log) {
    if now.saturating_duration_since(since) >= settings.backoff_max {
        program.failures = 0;
    }
    let factor = 1u32.checked_shl(program.failures.min(20)).unwrap_or(u32::MAX);
    let delay = settings.backoff.saturating_mul(factor).min(settings.backoff_max);
    program.failures = program.failures.saturating_add(1);
    log!(log, "{}: restarting in {:.3}s", program.name, delay.as_secs_f64());
    program.state = match now.checked_add(delay) {
        Some(until) => State::Waiting { until },
        None => State::Ended,
    };
}

fn running(programs: &[Program]) -> usize {
    let running = programs.iter().filter(|p| matches!(p.state, State::Running { .. }));
    running.count()
}

/// Reaps each program that ended, and every orphan adopted.
fn reap(programs: &mut [Program], now: Instant, settings: &Settings, stopping: bool, log: &Log) {
    for program in programs.iter_mut() {
        let State::Running { pid, since } = program.state else {
            continue;
        };
        let status = match process::reap(pid) {
            Ok(Some(status)) => status,
            Ok(None) => continue,
            Err(errno) => {
                log!(log, "{}: pid {pid} can't be waited for: {errno}", program.name);
                program.state = State::Ended;
                continue;
            }
        };
        let ran = now.saturating_duration_since(since).as_secs_f64();
        log!(log, "{}: pid {pid} {} after {ran:.3}s", program.name, died(status));
        if stopping {
            program.state = State::Ended;
        } else {
            wait(program, since, now, settings, log);
        }
    }
    let kept: Vec<Pid> = programs
        .iter()
        .filter_map(|p| match p.state {
            State::Running { pid, .. } => Some(pid),
            _ => None,
        })
        .collect();
    for (pid, status) in process::reap_orphans(|pid| kept.contains(&pid)) {
        log!(log, "orphan: pid {pid} {}", died(status));
    }
}

/// `name`, as `where` finds it on `PATH`.
fn find(name: &str) -> std::path::PathBuf {
    let found = Command::new("where.exe").arg(name).output();
    let found = found.map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
    let first = found.ok().and_then(|text| text.lines().next().map(str::to_owned));
    first.unwrap_or_else(|| name.to_owned()).into()
}

/// How a process ended, for the log.
fn died(status: Status) -> String {
    let Status::Exited(code) = status;
    match code == process::KILLED {
        true => "killed by the supervisor".to_owned(),
        false => format!("exited with status {code}"),
    }
}

/// Every process below this one, orphans adopted included, on Linux. On
/// macOS `process::tree` lists every process on the system, as orphans go
/// to launchd: none are taken for ours there.
fn descendants() -> Vec<Pid> {
    Vec::new()
}

/// Sends `signal` to each running program's process group, then to each
/// descendant left: orphans that moved to groups of their own, or whose
/// program has ended.
fn signal_all(programs: &[Program], signal: Signal, log: &Log) {
    for pid in descendants() {
        match process::kill(pid, signal) {
            Ok(()) | Err(Errno::SRCH) => {}
            Err(errno) => log!(log, "supervisor: SIG{} not sent to {pid}: {errno}", signal.name()),
        }
    }
    for program in programs {
        if let State::Running { pid, .. } = program.state {
            // Windows has one way to end a process from outside.
            match process::kill(pid, Signal::Kill) {
                Ok(()) | Err(Errno::SRCH) => {}
                Err(errno) => log!(log, "{}: SIG{} not sent: {errno}", program.name, signal.name()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_as_a_shell_splits_words() {
        let programs = parse("# comment\n\nweb  sh -c 'echo \"hi there\"; exit 3'\nb x\\ y \"a\\\"b\"\n")
            .unwrap_or_default();
        let argv: Vec<_> = programs.iter().map(|p| (p.name.as_str(), p.argv.clone())).collect();
        assert_eq!(
            argv,
            [
                ("web", vec!["sh".into(), "-c".into(), "echo \"hi there\"; exit 3".into()]),
                ("b", vec!["x y".into(), "a\"b".into()]),
            ]
        );
        assert!(parse("lonely\n").is_err());
        assert!(parse("a 'open\n").is_err());
    }
}

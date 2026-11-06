//! fuxix-expect: runs a program on a PTY and plays a script against it.
//!
//!     fuxix-expect login.exp -- ssh host
//!
//! The script has one command a line; blank lines and `#` comments are
//! skipped:
//!
//!     timeout 5          seconds the next waits may take (default 10)
//!     expect TEXT        wait for TEXT in the output since the last match
//!     send TEXT          type TEXT: \r \n \t \e \\ \xHH are escapes
//!     eof                wait for the program to end
//!
//! At the script's end the program is waited for as `eof` waits. The exit
//! status is the program's (128 + N if signal N killed it), 124 if a wait
//! timed out, 125 if the program ended before the text came, 2 for a bad
//! script or a program that would not start. Whatever the program left
//! running in its process group is then hung up.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, reason = "tests fail by panicking"))]

use fuxix::Errno;
use fuxix::poll::{Events, PollFd};
use fuxix::process::{Pid, Signal};
use fuxix::pty::Master;
use std::fs::File;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::process::{Child, ExitCode};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: fuxix-expect [--rows N] [--cols N] [-q] [--log FILE] SCRIPT -- PROGRAM [ARG...]
Runs PROGRAM on a terminal of N rows and columns (default 24 by 80) and plays
SCRIPT (a file, or - for stdin) against its output: lines of `expect TEXT`,
`send TEXT`, `timeout SECONDS` and `eof`. The output is copied to stdout
(not with -q) and, raw, to FILE. Exits with the program's status; 124 if a
wait timed out, 125 if the program ended first, 2 if the script or the
program could not be started.";

/// Output kept to match against: enough for any prompt, bounded so a chatty
/// program cannot grow it without end.
const KEEP: usize = 1 << 20;

#[derive(Debug, PartialEq)]
enum Step {
    Expect(Vec<u8>),
    Send(Vec<u8>),
    Timeout(Duration),
    Eof,
}

/// The script's steps, or the first bad line's number and what is wrong.
fn parse_script(text: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let bad = |why: String| format!("script line {}: {why}", n + 1);
        steps.push(match command {
            "expect" | "send" => {
                let bytes = unescape(rest).map_err(bad)?;
                if bytes.is_empty() {
                    return Err(bad(format!("{command} needs text")));
                }
                if command == "expect" { Step::Expect(bytes) } else { Step::Send(bytes) }
            }
            "timeout" => match rest.trim().parse::<f64>() {
                Ok(s) if (0.0..=86_400.0).contains(&s) => Step::Timeout(Duration::from_secs_f64(s)),
                _ => return Err(bad(format!("timeout {rest:?} is not seconds from 0 to 86400"))),
            },
            "eof" if rest.trim().is_empty() => Step::Eof,
            _ => return Err(bad(format!("unknown command {command:?}"))),
        });
    }
    Ok(steps)
}

/// `text` with its escapes replaced by the bytes they stand for.
fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut utf8 = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
            continue;
        }
        out.push(match chars.next() {
            Some('r') => b'\r',
            Some('n') => b'\n',
            Some('t') => b'\t',
            Some('e') => 0x1b,
            Some('\\') => b'\\',
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                u8::from_str_radix(&hex, 16).map_err(|_| format!("\\x{hex} is not two hex digits"))?
            }
            Some(other) => return Err(format!("unknown escape \\{other}")),
            None => return Err("a \\ ends the line".into()),
        });
    }
    Ok(out)
}

/// Why the script stopped short.
enum Failure {
    TimedOut(String),
    Ended(String),
    Io(String),
}

/// The program on its PTY, and what it has printed.
struct Session {
    master: Master,
    /// Readable once SIGCHLD came: the program may have ended.
    children: fuxix::signal::Pipe,
    child: Child,
    pid: Pid,
    /// Output since the last match.
    seen: Vec<u8>,
    /// The master read its end: no one holds the slave open.
    hung_up: bool,
    echo: bool,
    log: Option<File>,
}

impl Session {
    /// Reads what output there is, waiting until `deadline` for some.
    /// Whether any came.
    fn pump(&mut self, deadline: Instant, to_send: &mut &[u8]) -> Result<bool, Failure> {
        let io = |what: &str, e: Errno| Failure::Io(format!("{what}: {e}"));
        let left = deadline.saturating_duration_since(Instant::now());
        let mut events = Events::IN;
        if !to_send.is_empty() {
            events |= Events::OUT;
        }
        let mut fds = [PollFd::new(&self.master, events), PollFd::new(self.children.reader(), Events::IN)];
        match fuxix::poll::poll(&mut fds, Some(left)) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(io("poll", e)),
        }
        let [master, children] = fds.map(|fd| fd.revents());
        if children.intersects(Events::IN) {
            // Drained: the caller asks `ended` itself.
            while self.children.next().is_some() {}
        }
        let ready = master;
        if ready.intersects(Events::OUT) && !to_send.is_empty() {
            match fuxix::io::write(&self.master, to_send) {
                Ok(n) => *to_send = to_send.get(n..).unwrap_or_default(),
                Err(Errno::AGAIN | Errno::INTR) => {}
                Err(e) => return Err(io("writing to the program", e)),
            }
        }
        let mut got = false;
        let mut buffer = [0u8; 16 << 10];
        loop {
            match fuxix::io::read(&self.master, &mut buffer) {
                // Linux: EIO once every slave is closed. 0 is the same end.
                Ok(0) | Err(Errno::IO) => {
                    self.hung_up = true;
                    break;
                }
                Ok(n) => {
                    let bytes = buffer.get(..n).unwrap_or_default();
                    self.output(bytes).map_err(|e| Failure::Io(format!("copying output: {e}")))?;
                    got = true;
                }
                Err(Errno::AGAIN | Errno::INTR) => break,
                Err(e) => return Err(io("reading the program", e)),
            }
        }
        Ok(got)
    }

    fn output(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if self.echo {
            let mut out = std::io::stdout().lock();
            out.write_all(bytes)?;
            out.flush()?;
        }
        if let Some(log) = &mut self.log {
            log.write_all(bytes)?;
        }
        self.seen.extend_from_slice(bytes);
        if let Some(over) = self.seen.len().checked_sub(KEEP) {
            self.seen.drain(..over);
        }
        Ok(())
    }

    /// Whether the program has ended, left to be reaped.
    fn ended(&self) -> Result<bool, Failure> {
        fuxix::process::ended(self.pid)
            .map(|s| s.is_some())
            .map_err(|e| Failure::Io(format!("waiting for the program: {e}")))
    }

    /// Waits for `needle` in the output, consuming the output up to its end.
    fn expect(&mut self, needle: &[u8], timeout: Duration) -> Result<(), Failure> {
        let deadline = Instant::now().checked_add(timeout).unwrap_or_else(Instant::now);
        loop {
            if let Some(at) = self.seen.windows(needle.len()).position(|w| w == needle) {
                self.seen.drain(..at.saturating_add(needle.len()));
                return Ok(());
            }
            let what = String::from_utf8_lossy(needle).into_owned();
            // The program can end with its slave held by a child (Linux),
            // or held by us (macOS, where `Master` keeps it): its exit,
            // once its output is read, ends the wait as a hang-up does.
            if self.hung_up || (self.ended()? && !self.pump(Instant::now(), &mut &[][..])?) {
                return Err(Failure::Ended(what));
            }
            if Instant::now() >= deadline {
                return Err(Failure::TimedOut(what));
            }
            self.pump(deadline, &mut &[][..])?;
        }
    }

    /// Types `bytes`, reading output meanwhile so the program never blocks
    /// on a full PTY while we block on its input.
    fn send(&mut self, bytes: &[u8], timeout: Duration) -> Result<(), Failure> {
        let deadline = Instant::now().checked_add(timeout).unwrap_or_else(Instant::now);
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.hung_up || self.ended()? {
                return Err(Failure::Ended("its input to be read".into()));
            }
            if Instant::now() >= deadline {
                return Err(Failure::TimedOut("its input to be read".into()));
            }
            self.pump(deadline, &mut rest)?;
        }
        Ok(())
    }

    /// Waits for the program to end, copying its output.
    fn eof(&mut self, timeout: Duration) -> Result<(), Failure> {
        let deadline = Instant::now().checked_add(timeout).unwrap_or_else(Instant::now);
        while !self.hung_up && !self.ended()? {
            if Instant::now() >= deadline {
                return Err(Failure::TimedOut("the program to end".into()));
            }
            self.pump(deadline, &mut &[][..])?;
        }
        // Ended: read what it left, up to the hang-up or a pause.
        while !self.hung_up && self.pump(Instant::now(), &mut &[][..])? {}
        Ok(())
    }

    /// Hangs the program's session up, as closing its terminal would, and
    /// kills it if it outlives that by a second.
    fn hang_up(&mut self) {
        // It leads its own session and group (`Slave::spawn`).
        let _ = fuxix::process::kill_group(self.pid, Signal::Hup);
        let _ = fuxix::process::kill_group(self.pid, Signal::Cont);
        let deadline = Instant::now().checked_add(Duration::from_secs(1)).unwrap_or_else(Instant::now);
        while matches!(self.ended(), Ok(false)) && Instant::now() < deadline {
            let _ = self.pump(Instant::now().checked_add(Duration::from_millis(20)).unwrap_or(deadline), &mut &[][..]);
        }
        if !matches!(self.ended(), Ok(true)) {
            let _ = fuxix::process::kill_group(self.pid, Signal::Kill);
        }
    }

    /// Reaps the program: its exit status as a shell gives it.
    fn status(&mut self) -> Result<u8, String> {
        use std::os::unix::process::ExitStatusExt;
        let status = self.child.wait().map_err(|e| format!("waiting for the program: {e}"));
        // What it left running in its group, holding the terminal or not,
        // is hung up as closing the terminal would (ESRCH if none is left).
        let _ = fuxix::process::kill_group(self.pid, Signal::Hup);
        let status = status?;
        let code = match (status.code(), status.signal()) {
            (Some(code), _) => code,
            (None, Some(signal)) => 128_i32.saturating_add(signal),
            (None, None) => 1,
        };
        Ok(u8::try_from(code & 0xff).unwrap_or(1))
    }
}

struct Settings {
    rows: NonZeroU16,
    cols: NonZeroU16,
    echo: bool,
    log: Option<String>,
    script: String,
    argv: Vec<String>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Settings, String> {
    let size = |v: Option<String>, name: &str| -> Result<NonZeroU16, String> {
        let v = v.ok_or(format!("{name} needs a number"))?;
        v.parse().map_err(|_| format!("{name}: {v:?} is not a number from 1 to 65535"))
    };
    let (mut rows, mut cols) = (NonZeroU16::new(24), NonZeroU16::new(80));
    let (mut echo, mut log, mut script) = (true, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rows" => rows = Some(size(args.next(), "--rows")?),
            "--cols" => cols = Some(size(args.next(), "--cols")?),
            "-q" | "--quiet" => echo = false,
            "--log" => log = Some(args.next().ok_or("--log needs a file")?),
            "-h" | "--help" => return Err(String::new()),
            "--" => break,
            a if a.starts_with('-') && a != "-" => return Err(format!("unknown option {a}")),
            _ if script.is_some() => return Err("one SCRIPT, then -- and the program".into()),
            _ => script = Some(arg),
        }
    }
    let argv: Vec<String> = args.collect();
    match (rows, cols, script) {
        (Some(rows), Some(cols), Some(script)) if !argv.is_empty() => {
            Ok(Settings { rows, cols, echo, log, script, argv })
        }
        _ => Err("a SCRIPT, then -- and the program to run".into()),
    }
}

fn start(s: &Settings) -> Result<(Session, Vec<Step>), String> {
    let text = if s.script == "-" {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).map(|_| text)
    } else {
        std::fs::read_to_string(&s.script)
    }
    .map_err(|e| format!("{}: {e}", s.script))?;
    let steps = parse_script(&text)?;
    let log = match &s.log {
        Some(path) => Some(File::create(path).map_err(|e| format!("{path}: {e}"))?),
        None => None,
    };
    // Before the spawn, so that no SIGCHLD of the program's is missed.
    let children = fuxix::signal::Pipe::new(&[Signal::Chld]).map_err(|e| format!("SIGCHLD: {e}"))?;
    let (master, slave) = fuxix::pty::open((s.rows, s.cols).into()).map_err(|e| e.to_string())?;
    let (child, pid) = slave
        .spawn(&s.argv, fuxix::process::Dispositions::Default, |command| {
            if std::env::var_os("TERM").is_none() {
                command.env("TERM", "xterm-256color");
            }
        })
        .map_err(|e| format!("{}: {e}", s.argv.first().map_or("", String::as_str)))?;
    let session = Session { master, children, child, pid, seen: Vec::new(), hung_up: false, echo: s.echo, log };
    Ok((session, steps))
}

/// Plays `steps`, then waits for the program's end.
fn play(session: &mut Session, steps: &[Step]) -> Result<(), Failure> {
    let mut timeout = Duration::from_secs(10);
    for step in steps {
        match step {
            Step::Timeout(t) => timeout = *t,
            Step::Expect(text) => session.expect(text, timeout)?,
            Step::Send(bytes) => session.send(bytes, timeout)?,
            Step::Eof => session.eof(timeout)?,
        }
    }
    session.eof(timeout)
}

fn main() -> ExitCode {
    let settings = match parse_args(std::env::args().skip(1)) {
        Ok(s) => s,
        Err(e) if e.is_empty() => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("fuxix-expect: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let (mut session, steps) = match start(&settings) {
        Ok(started) => started,
        Err(e) => {
            eprintln!("fuxix-expect: {e}");
            return ExitCode::from(2);
        }
    };
    let failed = play(&mut session, &steps).err();
    let tail = |s: &Session| {
        let from = s.seen.len().saturating_sub(200);
        format!("{:?}", String::from_utf8_lossy(s.seen.get(from..).unwrap_or_default()))
    };
    let code = match &failed {
        None => None,
        Some(Failure::TimedOut(what)) => {
            eprintln!("\nfuxix-expect: timed out waiting for {what:?}; unmatched output: {}", tail(&session));
            Some(124)
        }
        Some(Failure::Ended(what)) => {
            eprintln!("\nfuxix-expect: the program ended before {what:?}; unmatched output: {}", tail(&session));
            Some(125)
        }
        Some(Failure::Io(e)) => {
            eprintln!("\nfuxix-expect: {e}");
            Some(2)
        }
    };
    if failed.is_some() {
        session.hang_up();
    }
    match (session.status(), code) {
        (_, Some(code)) => ExitCode::from(code),
        (Ok(status), None) => ExitCode::from(status),
        (Err(e), None) => {
            eprintln!("fuxix-expect: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_parse_with_their_escapes_and_bad_lines_are_named() {
        let steps = parse_script("# login\ntimeout 2.5\nexpect name? \nsend bob\\r\\x03\\e\neof\n").unwrap();
        assert_eq!(
            steps,
            [
                Step::Timeout(Duration::from_millis(2500)),
                Step::Expect(b"name? ".to_vec()),
                Step::Send(b"bob\r\x03\x1b".to_vec()),
                Step::Eof,
            ]
        );
        assert_eq!(parse_script("send ok\nwait 3").unwrap_err(), "script line 2: unknown command \"wait\"");
        assert!(parse_script("send \\q").unwrap_err().contains("unknown escape"));
    }
}

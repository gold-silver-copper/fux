//! fuxix-recorder: runs a command in a PTY, passes keys and output through,
//! follows window-size changes, and writes the output as an asciinema v2
//! `.cast` log: a JSON header line, then `[seconds, "o", text]` for output
//! and `[seconds, "r", "COLSxROWS"]` for each resize.
//!
//! Usage: fuxix-recorder [-q] [-o FILE] [--] [COMMAND [ARGS...]]
use fuxix::Errno;
use fuxix::poll::{Events, PollFd};
use fuxix::process::{Dispositions, Job, Signal, Status};
use fuxix::terminal::{self, Size};
use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::process::ExitCode;
use std::time::{Instant, SystemTime};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "usage: fuxix-recorder [-q] [-o FILE] [--] [COMMAND [ARGS...]]";

struct Args {
    out: String,
    quiet: bool,
    argv: Vec<String>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let mut out = String::from("recording.cast");
    let mut quiet = false;
    let mut argv = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "-q" | "--quiet" => quiet = true,
            "-o" | "--output" => out = args.next().ok_or("-o needs a file")?,
            "--" => {
                argv.extend(args);
                break;
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}").into()),
            _ => {
                argv.push(arg);
                argv.extend(args);
                break;
            }
        }
    }
    if argv.is_empty() {
        argv.push(std::env::var("SHELL").unwrap_or_else(|_| String::from("/bin/sh")));
    }
    Ok(Some(Args { out, quiet, argv }))
}

/// The size stdin's terminal has, or 24 by 80 when it is none.
fn outer_size() -> Result<(Size, Option<terminal::Pixels>)> {
    terminal::stdin_size()
        .or_else(|| Some((Size::new(24, 80)?, None)))
        .ok_or_else(|| "no terminal size".into())
}

/// Text in `pending` that is whole UTF-8, invalid bytes as U+FFFD; a
/// character cut at the end stays for the next read.
fn take_text(pending: &mut Vec<u8>) -> String {
    let mut text = String::new();
    let mut rest: &[u8] = pending;
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                text.push_str(valid);
                rest = &[];
                break;
            }
            Err(error) => {
                let (valid, after) = rest.split_at(error.valid_up_to());
                text.push_str(std::str::from_utf8(valid).unwrap_or_default());
                match error.error_len() {
                    Some(bad) => {
                        text.push('\u{fffd}');
                        rest = after.get(bad..).unwrap_or_default();
                    }
                    None => {
                        rest = after;
                        break;
                    }
                }
            }
        }
    }
    let keep = pending.len().saturating_sub(rest.len());
    pending.drain(..keep);
    text
}

/// `text` as a JSON string, quotes included.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' || c == '\u{7f}' => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Cast {
    file: BufWriter<File>,
    start: Instant,
    pending: Vec<u8>,
}

impl Cast {
    fn create(path: &str, size: Size, argv: &[String]) -> Result<Cast> {
        let file = File::create(path).map_err(|e| format!("{path}: {e}"))?;
        let mut file = BufWriter::new(file);
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let term = std::env::var("TERM").unwrap_or_default();
        writeln!(
            file,
            "{{\"version\": 2, \"width\": {}, \"height\": {}, \"timestamp\": {now}, \
             \"command\": {}, \"env\": {{\"TERM\": {}}}}}",
            size.cols,
            size.rows,
            json_string(&argv.join(" ")),
            json_string(&term),
        )?;
        Ok(Cast {
            file,
            start: Instant::now(),
            pending: Vec::new(),
        })
    }

    fn event(&mut self, kind: &str, data: &str) -> Result<()> {
        let at = self.start.elapsed().as_secs_f64();
        writeln!(self.file, "[{at:.6}, \"{kind}\", {}]", json_string(data))?;
        Ok(())
    }

    fn output(&mut self, bytes: &[u8]) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        let text = take_text(&mut self.pending);
        if text.is_empty() {
            return Ok(());
        }
        self.event("o", &text)
    }

    fn finish(mut self) -> Result<()> {
        if !self.pending.is_empty() {
            let text = String::from_utf8_lossy(&self.pending).into_owned();
            self.event("o", &text)?;
        }
        self.file.flush()?;
        Ok(())
    }
}

/// Exit code for how the command ended, as a shell gives it.
fn exit_code(status: Option<Status>) -> u8 {
    match status {
        Some(Status::Exited(code)) => u8::try_from(code & 0xff).unwrap_or(1),
        None => 1,
    }
}

fn run(args: Args) -> Result<u8> {
    let (size, pixels) = outer_size()?;
    // Windows' built-in ConPTY: nothing pinned is at this path.
    let _ = fuxix::pty::load(std::path::Path::new("no-pinned-conpty"));
    let (master, slave) = fuxix::pty::open(size)?;
    if pixels.is_some() {
        terminal::set_window_size(&master, size, pixels)?;
    }
    // Before the spawn, so that no SIGCHLD is missed.
    let signals = fuxix::signal::Pipe::new(&[Signal::Winch, Signal::Chld])?;
    let job = slave
        .spawn(&args.argv, |_| {}, Dispositions::Default)
        .map_err(|e| format!("{}: {e}", args.argv.join(" ")))?;
    let mut cast = Cast::create(&args.out, size, &args.argv)?;
    // Raw mode on stdin, if it is a terminal.
    let stdin = std::io::stdin();
    let raw = std::io::IsTerminal::is_terminal(&stdin)
        .then(|| terminal::Raw::enter(stdin))
        .transpose()?;

    let status = pump(&master, &signals, &job, &mut cast)?;
    drop(raw);
    cast.finish()?;
    if !args.quiet {
        eprintln!("fuxix-recorder: recorded to {}", args.out);
    }
    Ok(exit_code(status))
}

/// Passes input and output through until the command has ended and its
/// output is read: how it ended.
fn pump(
    master: &fuxix::pty::Master,
    signals: &fuxix::signal::Pipe,
    job: &Job,
    cast: &mut Cast,
) -> Result<Option<Status>> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut to_child: Vec<u8> = Vec::new();
    let mut stdin_open = true;
    loop {
        let mut wants = Events::IN;
        if !to_child.is_empty() {
            wants |= Events::OUT;
        }
        let mut fds = vec![PollFd::new(master, wants), PollFd::new(signals.reader(), Events::IN)];
        if stdin_open && to_child.is_empty() {
            fds.push(PollFd::new(&stdin, Events::IN));
        }
        match fuxix::poll::poll(&mut fds, None) {
            Ok(_) => {}
            Err(Errno::INTR) => continue,
            Err(errno) => return Err(errno.into()),
        }
        let [from_master, from_signals, rest @ ..] = fds.as_slice() else {
            return Ok(None);
        };
        let (from_master, from_signals) = (from_master.revents(), from_signals.revents());
        let from_stdin = rest.first().map(PollFd::revents);

        if from_signals.intersects(Events::IN) {
            while let Some(signal) = signals.next() {
                match signal {
                    Signal::Winch => {
                        if let Some((now, pixels)) = terminal::stdin_size()
                            && terminal::window_size(master).map(|(size, _)| size) != Some(now)
                        {
                            terminal::set_window_size(master, now, pixels)?;
                            cast.event("r", &format!("{}x{}", now.cols, now.rows))?;
                        }
                    }
                    _ => {
                        // Ended: what it wrote is all readable now.
                        if let Some(status) = job.ended()? {
                            while matches!(pass_output(master, &mut buffer, &mut stdout, cast)?, Some(n) if n > 0) {}
                            return Ok(Some(status));
                        }
                    }
                }
            }
        }
        if from_master.intersects(Events::IN | Events::HUP | Events::ERR)
            && pass_output(master, &mut buffer, &mut stdout, cast)?.is_none()
        {
            // The slave is closed (Linux): every holder is gone.
            return wait_for_end(signals, job);
        }
        if from_master.intersects(Events::OUT) {
            let wrote = fuxix::io::write_what_fits(master, &to_child)?;
            to_child.drain(..wrote);
        }
        if let Some(events) = from_stdin
            && events.intersects(Events::IN | Events::HUP | Events::ERR)
        {
            match fuxix::io::read(&stdin, &mut buffer) {
                Ok(0) => stdin_open = false,
                Ok(n) => {
                    let bytes = buffer.get(..n).unwrap_or_default();
                    let wrote = fuxix::io::write_what_fits(master, bytes)?;
                    to_child.extend_from_slice(bytes.get(wrote..).unwrap_or_default());
                }
                Err(Errno::AGAIN | Errno::INTR) => {}
                Err(_) => stdin_open = false,
            }
        }
    }
}

/// Passes on what the master has now: how many bytes (0 if none yet), or
/// `None` once it is closed.
fn pass_output(
    master: &fuxix::pty::Master,
    buffer: &mut [u8],
    stdout: &mut std::io::Stdout,
    cast: &mut Cast,
) -> Result<Option<usize>> {
    match fuxix::io::read(master, buffer) {
        Ok(0) | Err(Errno::IO) => Ok(None),
        Ok(n) => {
            let bytes = buffer.get(..n).unwrap_or_default();
            stdout.write_all(bytes)?;
            stdout.flush()?;
            cast.output(bytes)?;
            Ok(Some(n))
        }
        Err(Errno::AGAIN | Errno::INTR) => Ok(Some(0)),
        Err(errno) => Err(errno.into()),
    }
}

/// Reaps `pid`, which has closed its terminal, once it ends.
fn wait_for_end(signals: &fuxix::signal::Pipe, job: &Job) -> Result<Option<Status>> {
    loop {
        if let Some(status) = job.ended()? {
            return Ok(Some(status));
        }
        match fuxix::poll::poll(&mut [PollFd::new(signals.reader(), Events::IN)], None) {
            Ok(_) | Err(Errno::INTR) => while signals.next().is_some() {},
            Err(errno) => return Err(errno.into()),
        }
    }
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("fuxix-recorder: {error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("fuxix-recorder: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_keeps_a_cut_character_for_the_next_read() {
        let mut pending = b"a\xc3".to_vec();
        assert_eq!(take_text(&mut pending), "a");
        pending.extend_from_slice(b"\xa9\xffz");
        assert_eq!(take_text(&mut pending), "\u{e9}\u{fffd}z");
        assert!(pending.is_empty());
        assert_eq!(json_string("\x1b[1m\"\\\n"), "\"\\u001b[1m\\\"\\\\\\n\"");
    }
}

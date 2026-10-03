//! Recording a real program's output, as a pane of fux would get it: the
//! program runs on a PTY of a fixed size with `TERM=xterm-256color`, keys
//! are typed into it one step at a time, and every byte it writes is kept,
//! in order, as written. A fux-vt parser set up as fux sets up a pane's
//! reads the output beside the PTY, and its replies to the program's
//! queries are written back, as fux writes them, so a program that waits
//! for an answer (DA1, a cursor report) gets the one it would get in fux.
//! The keys are typed as fux gives them to a pane: read as fux reads a
//! legacy terminal's bytes, and written in the key mode the program asked
//! for (the kitty keyboard protocol's flags, modifyOtherKeys, cursor keys),
//! with fux's own decoder and encoder.
//!
//! A recording is two files: `NAME.bin`, the bytes, and `NAME.json`, what
//! was run and typed (see [`Manifest`]). Step 0 is the program starting;
//! each later step is one line of keys and the output that followed it,
//! until the program had been quiet for a while. Each step's `end` is the
//! offset in the bytes where its output ends. A step may resize the
//! terminal instead of typing (`!resize`): its `resize` is the new size,
//! given to the PTY and the parser as fux gives it to a pane's, and its
//! output is what the program wrote after it.
use crate::escape;
use fuxix::poll::{Events, PollFd};
use fuxix::process::{Pid, Signal};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The hidden subcommand the recorder starts a program through: it makes
/// the program the leader of a new session, with the PTY (its stdin) as
/// its controlling terminal, as fux's own launcher does.
pub const LAUNCH: &str = "__launch";

/// The most reply bytes written back for one read of output, as fux's
/// pane sink keeps (`src/pane.rs`).
const REPLY_LIMIT: usize = 4096;

/// How long a step may go on producing output before the recorder moves
/// on, however busy the program is.
const STEP_LIMIT: Duration = Duration::from_secs(20);

/// What `record` was asked to do.
pub struct Request {
    pub rows: u16,
    pub cols: u16,
    /// Where the files go: `PREFIX.bin` and `PREFIX.json`.
    pub out: PathBuf,
    pub program: String,
    pub version: String,
    /// `KEY=VALUE`: the whole environment besides `TERM`, and `PATH`
    /// unless one is given here.
    pub env: Vec<String>,
    pub dir: Option<PathBuf>,
    /// The file of keys: one step a line (see [`steps`]).
    pub keys: PathBuf,
    /// `OLD=NEW`: bytes replaced in the output before it is saved, for
    /// what cannot be kept out by the setup (the host name in a `file://`
    /// URI). Only NEW is recorded.
    pub scrub: Vec<String>,
    /// Said in the manifest: what to know about the recording.
    pub note: String,
    pub argv: Vec<String>,
}

/// `bytes` with every `old` replaced by `new`.
fn replaced(bytes: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    if old.is_empty() {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(at) =
        (0..rest.len()).find(|&i| rest.get(i..).is_some_and(|r| r.starts_with(old)))
    {
        let (before, after) = rest.split_at_checked(at).unwrap_or((rest, &[]));
        out.extend_from_slice(before);
        out.extend_from_slice(new);
        rest = after.get(old.len()..).unwrap_or_default();
    }
    out.extend_from_slice(rest);
    out
}

/// One line of the keys file: what to type, or the size to resize to, and
/// how long the program must stay quiet afterwards before the next step.
struct Step {
    keys: Vec<u8>,
    resize: Option<(u16, u16)>,
    quiet: Duration,
}

/// Reads a keys file. Each line is a step's keys, written as `replay`
/// writes output (`\e`, `\r`, `\x03`, `\u{…}`). A line starting `#` is a
/// comment and an empty line is skipped. `!quiet MS` sets how long the
/// program must stay quiet after each following step (400 ms at first),
/// `!start MS` how long after it starts (1500 ms at first). A step that
/// types nothing (`!wait`) only waits, and `!resize RxC` resizes the
/// terminal to R rows and C columns.
fn steps(text: &str) -> Result<(Duration, Vec<Step>), String> {
    let mut quiet = Duration::from_millis(400);
    let mut start = Duration::from_millis(1500);
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let at = |e: String| format!("keys line {}: {e}", n.saturating_add(1));
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let ms = |word: &str| -> Result<Duration, String> {
            word.trim()
                .parse::<u64>()
                .map(Duration::from_millis)
                .map_err(|e| at(format!("{word:?}: {e}")))
        };
        if let Some(rest) = line.strip_prefix("!quiet ") {
            quiet = ms(rest)?;
        } else if let Some(rest) = line.strip_prefix("!start ") {
            start = ms(rest)?;
        } else if line == "!wait" {
            out.push(Step {
                keys: Vec::new(),
                resize: None,
                quiet,
            });
        } else if let Some(rest) = line.strip_prefix("!resize ") {
            let size = rest
                .trim()
                .split_once('x')
                .and_then(|(r, c)| Some((r.parse::<u16>().ok()?, c.parse::<u16>().ok()?)))
                .filter(|&(r, c)| r > 0 && c > 0)
                .ok_or_else(|| at(format!("{rest:?} is not RxC")))?;
            out.push(Step {
                keys: Vec::new(),
                resize: Some(size),
                quiet,
            });
        } else {
            out.push(Step {
                keys: escape::unescape(line).map_err(at)?,
                resize: None,
                quiet,
            });
        }
    }
    Ok((start, out))
}

/// The files of a recording, as written: see the module documentation.
pub struct Recorded {
    pub bytes: usize,
    pub replies: usize,
    pub steps: usize,
}

/// What fux answers a pane's colour queries with: its client terminal's
/// colours (src/outer.rs). The recorder stands for one fixed terminal, not
/// whoever records: white on black, which says it is dark.
const FOREGROUND: &str = "rgb:ffff/ffff/ffff";
const BACKGROUND: &str = "rgb:0000/0000/0000";
const DARK: &[u8] = b"\x1b[?997;1n";

/// A fux pane's sink: replies kept up to [`REPLY_LIMIT`] a read, colour
/// queries (OSC 10, 11, `CSI ? 996 n`) answered as fux answers them, and
/// everything else dropped (fux keeps the title, which the recording does
/// not need).
#[derive(Default)]
struct Replies(Vec<u8>);

impl fux_vt::Sink for Replies {
    fn reply(&mut self, bytes: &[u8]) {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_some_and(|len| len <= REPLY_LIMIT)
        {
            self.0.extend_from_slice(bytes);
        }
    }
    fn event(&mut self, event: fux_vt::Event<'_>) {
        if let fux_vt::Event::ColorQuery { number, bel } = event {
            let colour = match number {
                10 => FOREGROUND,
                11 => BACKGROUND,
                _ => return,
            };
            let end = if bel { "\x07" } else { "\x1b\\" };
            self.reply(format!("\x1b]{number};{colour}{end}").as_bytes());
        }
    }
    fn unhandled(&mut self, sequence: fux_vt::Unhandled<'_>) {
        if let fux_vt::Unhandled::Csi {
            params,
            intermediates: b"?",
            action: b'n',
        } = sequence
            && params.groups().eq([&[996][..]])
        {
            self.reply(DARK);
        }
    }
}

/// The program on its PTY, the parser beside it, and everything read.
struct Session {
    master: File,
    parser: fux_vt::Parser,
    output: Vec<u8>,
    replies: Vec<u8>,
    /// The PTY has hung up: the program and everything it started have
    /// closed it.
    closed: bool,
}

impl Session {
    /// Resizes the terminal as fux resizes a pane (`Pane::resize` in
    /// src/pane.rs): the parser, then the PTY, then the in-band resize
    /// report for a program that asked for it.
    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.parser
            .resize(rows, cols)
            .map_err(|e| format!("fux-vt: {e}"))?;
        fuxix::terminal::set_window_size(&self.master, rows, cols)
            .map_err(|e| format!("resizing the PTY: {e}"))?;
        if let Some(report) = self.parser.resize_report() {
            self.replies.extend_from_slice(&report);
            self.master
                .write_all(&report)
                .map_err(|e| format!("writing the resize report: {e}"))?;
        }
        Ok(())
    }

    /// Reads what the program writes until it has been quiet for `quiet`,
    /// or `limit` has passed, answering its queries as it goes.
    fn settle(&mut self, quiet: Duration, limit: Duration) -> Result<(), String> {
        let started = Instant::now();
        let mut buf = vec![0u8; 1 << 16];
        while !self.closed && started.elapsed() < limit {
            let mut fds = [PollFd::new(&self.master, Events::IN)];
            let ready = fuxix::poll::poll(&mut fds, Some(quiet))
                .map_err(|e| format!("polling the PTY: {e}"))?;
            if ready == 0 {
                return Ok(());
            }
            match self.master.read(&mut buf) {
                Ok(0) => self.closed = true,
                Ok(n) => {
                    let got = buf.get(..n).unwrap_or_default();
                    self.output.extend_from_slice(got);
                    let mut sink = Replies::default();
                    // The parser refuses only allocations past its limits,
                    // as in fux, where output goes on regardless.
                    let _ = self.parser.process_with(got, &mut sink);
                    if !sink.0.is_empty() {
                        self.replies.extend_from_slice(&sink.0);
                        self.master
                            .write_all(&sink.0)
                            .map_err(|e| format!("writing a reply: {e}"))?;
                    }
                }
                // The PTY hangs up (EIO on Linux) once no one has it open.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => self.closed = true,
            }
        }
        Ok(())
    }
}

/// Opens a PTY and starts the program on it through [`LAUNCH`], with the
/// environment `env` alone, besides `TERM`, and `PATH` if `env` has
/// none.
fn spawn(request: &Request) -> Result<(OwnedFd, std::process::Child), String> {
    let (master, slave) =
        fuxix::pty::open(request.rows, request.cols).map_err(|e| format!("opening a PTY: {e}"))?;
    let me = std::env::current_exe().map_err(|e| format!("this program's path: {e}"))?;
    let clone = |fd: &OwnedFd| fd.try_clone().map_err(|e| format!("the PTY: {e}"));
    let mut command = Command::new(me);
    command
        .arg(LAUNCH)
        .args(&request.argv)
        .env_clear()
        .env("TERM", "xterm-256color")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(clone(&slave)?)
        .stdout(clone(&slave)?)
        .stderr(clone(&slave)?);
    for pair in &request.env {
        let (key, value) = pair
            .split_once('=')
            .ok_or(format!("--env {pair:?} is not KEY=VALUE"))?;
        command.env(key, value);
    }
    if let Some(dir) = &request.dir {
        command.current_dir(dir);
    }
    let child = command
        .spawn()
        .map_err(|e| format!("starting {:?}: {e}", request.argv))?;
    drop(slave);
    Ok((master, child))
}

/// Waits up to `limit` for the program to exit, reading what it writes
/// meanwhile: a process exiting with output still to be read waits for
/// it to be read, as it closes its terminal. Whether it exited.
fn exited(child: &mut std::process::Child, session: &mut Session, limit: Duration) -> bool {
    let deadline = Instant::now().checked_add(limit);
    loop {
        if !matches!(child.try_wait(), Ok(None)) {
            return true;
        }
        if deadline.is_none_or(|d| Instant::now() >= d) {
            return false;
        }
        if session.closed {
            std::thread::sleep(Duration::from_millis(10));
        } else {
            let _ = session.settle(Duration::from_millis(20), Duration::from_millis(100));
        }
    }
}

/// Ends the program: it has two seconds to exit after the last step, then
/// its group gets SIGHUP, as from a terminal closing, and then SIGKILL.
fn end(child: &mut std::process::Child, session: &mut Session) {
    if exited(child, session, Duration::from_secs(2)) {
        return;
    }
    if let Some(pid) = Pid::of(child) {
        let _ = fuxix::process::kill_group(pid, Signal::Hup);
        if exited(child, session, Duration::from_secs(2)) {
            return;
        }
        let _ = fuxix::process::kill_group(pid, Signal::Kill);
    }
    let _ = child.kill();
    if !exited(child, session, Duration::from_secs(2)) {
        let _ = child.wait();
    }
}

/// The bytes fux gives a pane for `keys`, a legacy terminal's bytes, in
/// the key mode `screen` is in: decoded by fux's decoder, the Escape
/// deadline passing at the end, and encoded by its encoder.
fn typed(keys: &[u8], screen: &fux_vt::Screen) -> Vec<u8> {
    let mut decoder = fux::decode::Decoder::default();
    let mut inputs = Vec::new();
    decoder.bytes(keys, &mut inputs);
    decoder.timeout(&mut inputs);
    let mode = fux::encode::KeyMode::of(screen);
    let mut out = Vec::with_capacity(keys.len());
    for input in inputs {
        match input {
            fux::decode::Input::Key(stroke) => fux::encode::key_bytes(stroke, mode, &mut out),
            fux::decode::Input::Paste(text) => {
                fux::encode::paste(&text, screen.bracketed_paste(), &mut out);
            }
            fux::decode::Input::PasteTooLong
            | fux::decode::Input::FocusIn
            | fux::decode::Input::FocusOut
            | fux::decode::Input::Reply(_) => {}
        }
    }
    out
}

/// Records the program: see the module documentation.
pub fn record(request: &Request) -> Result<Recorded, String> {
    let text = std::fs::read_to_string(&request.keys)
        .map_err(|e| format!("{}: {e}", request.keys.display()))?;
    let (start, steps) = steps(&text)?;
    // As fux's panes are set up.
    let options = fux::pane::OPTIONS;
    let parser = fux_vt::Parser::with_options(request.rows, request.cols, 10_000, options)
        .map_err(|e| format!("fux-vt: {e}"))?;
    let (master, mut child) = spawn(request)?;
    let mut session = Session {
        master: File::from(master),
        parser,
        output: Vec::new(),
        replies: Vec::new(),
        closed: false,
    };
    let mut ends = Vec::with_capacity(steps.len().saturating_add(1));
    let result = (|| {
        session.settle(start, STEP_LIMIT)?;
        ends.push(End {
            keys: Vec::new(),
            at: session.output.len(),
            resize: None,
        });
        for step in &steps {
            if let Some((rows, cols)) = step.resize {
                session.resize(rows, cols)?;
            }
            if !step.keys.is_empty() && !session.closed {
                let typed = typed(&step.keys, session.parser.screen());
                session
                    .master
                    .write_all(&typed)
                    .map_err(|e| format!("typing: {e}"))?;
            }
            session.settle(step.quiet, STEP_LIMIT)?;
            ends.push(End {
                keys: step.keys.clone(),
                at: session.output.len(),
                resize: step.resize,
            });
        }
        Ok::<(), String>(())
    })();
    end(&mut child, &mut session);
    // What the program wrote as it exited belongs to the last step.
    if let Some(last) = ends.last_mut() {
        last.at = session.output.len();
    }
    result?;
    write(request, &session, &ends)?;
    Ok(Recorded {
        bytes: session.output.len(),
        replies: session.replies.len(),
        steps: ends.len(),
    })
}

/// Where a step's output ends, and what started it.
struct End {
    keys: Vec<u8>,
    at: usize,
    resize: Option<(u16, u16)>,
}

fn write(request: &Request, session: &Session, ends: &[End]) -> Result<(), String> {
    // Each step's output is scrubbed alone, so the steps' ends move with
    // what is replaced.
    let mut output = Vec::with_capacity(session.output.len());
    let mut steps = Vec::with_capacity(ends.len());
    let mut from = 0usize;
    for End { keys, at, resize } in ends {
        let mut piece = session.output.get(from..*at).unwrap_or_default().to_vec();
        for pair in &request.scrub {
            let (old, new) = pair
                .split_once('=')
                .ok_or(format!("--scrub {pair:?} is not OLD=NEW"))?;
            piece = replaced(&piece, old.as_bytes(), new.as_bytes());
        }
        output.extend_from_slice(&piece);
        let mut step = serde_json::json!({"keys": escape::escape(keys), "end": output.len()});
        if let (Some((rows, cols)), Some(object)) = (resize, step.as_object_mut()) {
            object.insert("resize".into(), serde_json::json!([rows, cols]));
        }
        steps.push(step);
        from = *at;
    }
    let scrubbed: Vec<&str> = request
        .scrub
        .iter()
        .filter_map(|pair| pair.split_once('=').map(|(_, new)| new))
        .collect();
    let path = |ext: &str| -> PathBuf {
        let mut p = request.out.clone().into_os_string();
        p.push(ext);
        PathBuf::from(p)
    };
    let save = |path: &Path, bytes: &[u8]| {
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    };
    save(&path(".bin"), &output)?;
    let manifest = serde_json::json!({
        "program": request.program,
        "version": request.version,
        "command": request.argv,
        "size": [request.rows, request.cols],
        "env": request.env,
        "replies": escape::escape(&session.replies),
        "scrubbed": scrubbed,
        "note": request.note,
        "steps": steps,
    });
    let mut text =
        serde_json::to_string_pretty(&manifest).map_err(|e| format!("the manifest: {e}"))?;
    text.push('\n');
    save(&path(".json"), text.as_bytes())
}

/// The launcher: `fux-vt-compare __launch PROGRAM ARGS...`, started by
/// [`record`] with the PTY as its stdin, stdout and stderr. It returns only
/// if the program could not start, and says why on the PTY.
pub fn launched(argv: &[String]) -> Result<bool, String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("no program to run".into());
    };
    fuxix::process::setsid().map_err(|e| format!("setsid: {e}"))?;
    let terminal = std::io::stdin();
    fuxix::terminal::make_controlling(terminal.as_fd())
        .map_err(|e| format!("making the PTY the controlling terminal: {e}"))?;
    let error = Command::new(program).args(args).exec();
    Err(format!("{program}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    /// Keys reach the program as fux gives them to a pane, in the key mode
    /// it asked for (fux's src/encode.rs).
    #[test]
    fn keys_are_typed_in_the_programs_key_mode() -> Result<(), fux_vt::Error> {
        let options = fux_vt::Options::new().with_kitty_keyboard(true);
        let mut parser = fux_vt::Parser::with_options(4, 20, 0, options)?;
        let keys = b"\x04:q\r\x1b";
        assert_eq!(super::typed(keys, parser.screen()), keys);
        parser.process(b"\x1b[>4;2m")?;
        assert_eq!(
            super::typed(keys, parser.screen()),
            b"\x1b[27;5;100~:q\r\x1b"
        );
        parser.process(b"\x1b[>1u")?;
        assert_eq!(
            super::typed(keys, parser.screen()),
            b"\x1b[100;5u:q\r\x1b[27u"
        );
        Ok(())
    }

    #[test]
    fn scrubbing_replaces_every_match() {
        assert_eq!(super::replaced(b"a-host-b-host", b"host", b"h"), b"a-h-b-h");
        assert_eq!(super::replaced(b"abc", b"", b"x"), b"abc");
    }

    #[test]
    fn keys_files_give_steps_with_their_waits() -> Result<(), String> {
        let (start, steps) = super::steps(
            "# a comment\n!start 900\nj\n\n!quiet 50\n\\e:q\\r\n!wait\n!resize 24x80\n",
        )?;
        assert_eq!(start, Duration::from_millis(900));
        type Got = (Vec<u8>, Option<(u16, u16)>, u128);
        let got: Vec<Got> = steps
            .iter()
            .map(|s| (s.keys.clone(), s.resize, s.quiet.as_millis()))
            .collect();
        assert_eq!(
            got,
            [
                (b"j".to_vec(), None, 400),
                (b"\x1b:q\r".to_vec(), None, 50),
                (Vec::new(), None, 50),
                (Vec::new(), Some((24, 80)), 50)
            ]
        );
        assert!(super::steps("!resize 24\n").is_err());
        assert!(super::steps("!resize 0x80\n").is_err());
        Ok(())
    }
}

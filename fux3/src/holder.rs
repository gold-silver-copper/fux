//! The holder: one process, one poll loop, one program on one terminal. It
//! keeps its pane's files current, takes typed bytes from `in` and
//! commands from `ctl`, answers the program's terminal queries with nobody
//! watching, and knows nothing of other panes or of viewers. When its
//! program ends it keeps the last screen until it is killed.
use crate::ctl::{self, Command, MAX_SEND};
use crate::files::{self, Head, Placed};
use crate::publish::{self, Publisher};
use crate::session::{self, Session};
use crate::{Failure, io};
use fux_vt::sixel::Picture;
use fux_vt::{CellSize, Feature, Graphics, Identity, Mode, Options, Parser, Sink, Size, Stop};
use fuxix::Errno;
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{Job, Reach, Scan, Signal, Status};
use fuxix::pty::Master;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the screens are published at most while output goes on. The
/// first change after a rest is published at once.
pub const INTERVAL: Duration = Duration::from_millis(40);
/// How long `start` waits for the first files to be written.
const START_PUBLISH: Duration = Duration::from_secs(2);
/// The most bytes of output read at a time.
const READ: usize = 16 << 10;
/// The most bytes read from `in` or `ctl` at a time.
const FIFO_READ: usize = 4096;
/// The most typed bytes waiting for the program: a whole `send` and room
/// for typing beside it.
const MAX_TYPED: usize = MAX_SEND + (64 << 10);
/// Room kept apart for the terminal's replies to the program.
const REPLY_ROOM: usize = 64 << 10;
/// How much output goes to the parser at a time; each byte could ask for
/// a reply of up to 64 bytes, which must have room first.
const SLICE: usize = 512;
const REPLY_NEED: usize = SLICE * 64;
/// Bytes of history the terminal keeps between two looks (24 a row, 8 a
/// stored cell): the holder looks after every read of at most [`READ`]
/// bytes, which scroll at most that many rows; ordinary text needs a few
/// hundred KiB for them. Rows lost to output built to fill rows cheaply are
/// counted in `history`. Kept small, as a resize reflows all of it.
const TERMINAL_HISTORY: usize = 2 << 20;
/// The history file's size unless `start` says.
pub const DEFAULT_HISTORY: usize = 4 << 20;
/// The smallest and largest history file `start` allows.
pub const MIN_HISTORY: usize = 4096;
pub const MAX_HISTORY: usize = 256 << 20;
/// The most bytes of pixels a pane's pictures hold together.
pub const PICTURE_BYTES: usize = 16 << 20;
/// Pixels of pictures written to `images/` per second, on average, and at
/// most at once: a program drawing without end costs a bounded rate.
const IMAGE_PIXELS_PER_SECOND: u64 = 4 << 20;
const IMAGE_PIXELS_BURST: u64 = 16 << 20;
/// The cell size when the starting terminal says none.
pub const DEFAULT_CELL: (u16, u16) = (10, 20);
/// The refusals `errors` keeps.
const ERRORS_KEPT: usize = 32;
/// How long output may still come after the program ended, and how long
/// it is read at most.
const SETTLE: Duration = Duration::from_millis(20);
const DRAIN_MOST: Duration = Duration::from_secs(1);
/// How long a hung-up program gets before it is killed, and how long a
/// kill waits for everything to be gone.
const HANGUP_GRACE: Duration = Duration::from_millis(200);
const KILL_WAIT: Duration = Duration::from_secs(2);
/// The most labels, and their keys' and values' lengths.
pub const MAX_LABELS: usize = 16;
const MAX_LABEL_KEY: usize = 32;
const MAX_LABEL_VALUE: usize = 128;

fn after(now: Instant, wait: Duration) -> Instant {
    now.checked_add(wait).unwrap_or(now)
}

/// What `start` hands the holder.
#[derive(Debug, Clone)]
pub struct Spec {
    pub root: PathBuf,
    pub workspace: String,
    pub name: String,
    pub cols: u16,
    pub rows: u16,
    pub cell: (u16, u16),
    pub pictures: bool,
    pub history: usize,
    pub cwd: PathBuf,
    pub labels: Vec<(String, String)>,
    /// Variables named at start, given to the program after fux3's own.
    pub env: Vec<(String, OsString)>,
    pub argv: Vec<OsString>,
}

/// Reads `key=value`, checked against the label limits.
pub fn parse_label(text: &str) -> Result<(String, String), Failure> {
    let bad = |why: &str| Failure::new(format!("bad label {text:?}: {why}"));
    let (key, value) = text
        .split_once('=')
        .ok_or_else(|| bad("it must be key=value"))?;
    let key_ok = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    if key.is_empty() || key.len() > MAX_LABEL_KEY || !key.chars().all(key_ok) {
        return Err(bad("a key is 1 to 32 of [A-Za-z0-9_-]"));
    }
    if value.len() > MAX_LABEL_VALUE || value.chars().any(char::is_control) {
        return Err(bad(
            "a value is at most 128 bytes, with no control characters",
        ));
    }
    Ok((key.to_owned(), value.to_owned()))
}

/// Whether `cols` by `rows` is a size a pane may have.
pub fn checked_size(cols: u16, rows: u16) -> Result<Size, Failure> {
    let bad = || {
        Failure::new(format!(
            "size {cols}x{rows}: 1 to {} columns and 1 to {} rows",
            files::MAX_COLS,
            files::MAX_ROWS
        ))
    };
    if cols > files::MAX_COLS || rows > files::MAX_ROWS {
        return Err(bad());
    }
    Size::new(rows, cols).map_err(|_| bad())
}

impl Spec {
    /// As the holder's command line: native strings stay native.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "__hold".into(),
            self.root.clone().into(),
            self.workspace.clone().into(),
            self.name.clone().into(),
            format!("{}x{}", self.cols, self.rows).into(),
            format!("{}x{}", self.cell.0, self.cell.1).into(),
            if self.pictures { "on" } else { "off" }.into(),
            self.history.to_string().into(),
            self.cwd.clone().into(),
            self.labels.len().to_string().into(),
        ];
        for (key, value) in &self.labels {
            args.push(format!("{key}={value}").into());
        }
        args.push(self.env.len().to_string().into());
        for (key, value) in &self.env {
            let mut arg = OsString::from(format!("{key}="));
            arg.push(value);
            args.push(arg);
        }
        args.extend(self.argv.iter().cloned());
        args
    }

    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Spec, Failure> {
        let bad = |what: &str| Failure::new(format!("holder arguments: {what}"));
        let mut next = |what: &'static str| args.next().ok_or_else(|| bad(what));
        let text = |value: OsString, what: &str| value.into_string().map_err(|_| bad(what));
        let pair = |value: String, what: &str| -> Result<(u16, u16), Failure> {
            let (a, b) = value.split_once('x').ok_or_else(|| bad(what))?;
            Ok((
                a.parse().map_err(|_| bad(what))?,
                b.parse().map_err(|_| bad(what))?,
            ))
        };
        let root = PathBuf::from(next("root")?);
        let workspace = text(next("workspace")?, "workspace")?;
        let name = text(next("name")?, "name")?;
        let (cols, rows) = pair(text(next("size")?, "size")?, "size")?;
        let cell = pair(text(next("cell")?, "cell")?, "cell")?;
        let pictures = text(next("pictures")?, "pictures")? == "on";
        let history = text(next("history")?, "history")?
            .parse()
            .map_err(|_| bad("history"))?;
        let cwd = PathBuf::from(next("cwd")?);
        let count: usize = text(next("labels")?, "labels")?
            .parse()
            .map_err(|_| bad("labels"))?;
        let mut labels = Vec::new();
        if count > MAX_LABELS {
            return Err(bad("labels"));
        }
        for _ in 0..count {
            labels.push(parse_label(&text(next("label")?, "label")?)?);
        }
        let count: usize = text(next("env")?, "env")?.parse().map_err(|_| bad("env"))?;
        let mut env = Vec::new();
        for _ in 0..count {
            use std::os::unix::ffi::{OsStrExt, OsStringExt};
            let arg = next("variable")?;
            let bytes = arg.as_bytes();
            let at = bytes
                .iter()
                .position(|b| *b == b'=')
                .ok_or_else(|| bad("variable"))?;
            let key = String::from_utf8(bytes.get(..at).unwrap_or_default().to_vec())
                .map_err(|_| bad("variable"))?;
            let value = bytes
                .get(at.saturating_add(1)..)
                .unwrap_or_default()
                .to_vec();
            env.push((key, OsString::from_vec(value)));
        }
        let argv: Vec<OsString> = args.collect();
        if argv.is_empty() {
            return Err(bad("no program"));
        }
        Ok(Spec {
            root,
            workspace,
            name,
            cols,
            rows,
            cell,
            pictures,
            history,
            cwd,
            labels,
            env,
            argv,
        })
    }
}

/// What waits to be written to the program, in order: typed bytes and the
/// terminal's replies share one queue and have separate limits.
#[derive(Default)]
struct Input {
    queue: VecDeque<(bool, Vec<u8>)>,
    head_sent: usize,
    typed: usize,
    replies: usize,
    lost_replies: u64,
}

impl Input {
    /// Queues typed bytes whole, or none of them.
    fn admit(&mut self, bytes: Vec<u8>) -> Result<(), Failure> {
        let total = self.typed.saturating_add(bytes.len());
        if total > MAX_TYPED {
            return Err(Failure::new(format!(
                "{} bytes would make {total} waiting for the program, at most {MAX_TYPED}; \
                 nothing was queued",
                bytes.len()
            )));
        }
        self.typed = total;
        if !bytes.is_empty() {
            self.queue.push_back((true, bytes));
        }
        Ok(())
    }

    fn room(&self) -> usize {
        MAX_TYPED.saturating_sub(self.typed)
    }

    fn reply_room(&self) -> usize {
        REPLY_ROOM.saturating_sub(self.replies)
    }

    fn waiting(&self) -> usize {
        self.typed
            .saturating_add(self.replies)
            .saturating_sub(self.head_sent)
    }

    /// Writes what the terminal takes now.
    fn write(&mut self, master: &Master) -> Result<(), Errno> {
        while let Some((typed, bytes)) = self.queue.front() {
            let typed = *typed;
            let rest = bytes.get(self.head_sent..).unwrap_or_default();
            match fuxix::io::write(master, rest) {
                Ok(0) | Err(Errno::AGAIN) => return Ok(()),
                Err(Errno::INTR) => {}
                Ok(n) => {
                    self.head_sent = self.head_sent.saturating_add(n);
                    if self.head_sent >= bytes.len() {
                        let len = bytes.len();
                        self.queue.pop_front();
                        self.head_sent = 0;
                        let count = if typed {
                            &mut self.typed
                        } else {
                            &mut self.replies
                        };
                        *count = count.saturating_sub(len);
                    }
                }
                Err(errno) => return Err(errno),
            }
        }
        Ok(())
    }
}

impl Sink for Input {
    fn reply(&mut self, bytes: &[u8]) {
        if bytes.len() > self.reply_room() {
            self.lost_replies = self.lost_replies.saturating_add(1);
            return;
        }
        self.replies = self.replies.saturating_add(bytes.len());
        self.queue.push_back((false, bytes.to_vec()));
    }
}

/// The program's end, seen, while its last output is read.
struct Exit {
    ending: String,
    settled_at: Instant,
    latest: Instant,
}

fn ending(status: Status) -> String {
    match status {
        Status::Exited(code) => format!("exited {code}"),
        Status::Signalled { signal, .. } => {
            let name =
                Signal::from_raw(signal).map_or_else(|| signal.to_string(), |s| s.name().into());
            format!("signalled {name}")
        }
    }
}

/// Pictures written to `images/`, by the picture they show.
#[derive(Default)]
struct Images {
    written: Vec<(Arc<Picture>, String)>,
    next: u64,
    budget: u64,
    filled_at: Option<Instant>,
}

struct Holder {
    session: Session,
    dir: PathBuf,
    held: Option<File>,
    workspace: String,
    name: String,
    labels: Vec<(String, String)>,
    id: String,
    version: u64,
    parser: Parser,
    master: Master,
    job: Option<Job>,
    children: fuxix::signal::Pipe,
    in_fifo: File,
    ctl_fifo: File,
    lines: ctl::Lines,
    input: Input,
    unparsed: Vec<u8>,
    parsed: usize,
    open: bool,
    quiet: bool,
    check_exit: bool,
    exit: Option<Exit>,
    /// `running`, the program's ending once settled, or what a kill left.
    state: String,
    commands: u64,
    refused: u64,
    errors: VecDeque<String>,
    errors_changed: bool,
    in_dropped: u64,
    exit_written: bool,
    cell: (u16, u16),
    pictures: bool,
    history_end: u64,
    history: Vec<u8>,
    images: Images,
    publisher: Publisher,
    history_cap: usize,
    dirty: bool,
    published_at: Option<Instant>,
    buffer: Vec<u8>,
}

/// Runs the holder `args` describe; what it says goes to its starter.
pub fn main(args: impl Iterator<Item = OsString>) -> ExitCode {
    let mut stdout = std::io::stdout();
    let mut go = String::new();
    if std::io::stdin().lock().read_line(&mut go).is_err() || go.trim() != "go" {
        return ExitCode::FAILURE;
    }
    match Spec::parse(args).and_then(prepare) {
        Ok(mut holder) => {
            holder.publish(Instant::now());
            // `ok` only once the files are there, whole: a client reading
            // them at once finds them. Otherwise the pane is ended and
            // `start` says why.
            if !holder.publisher.wait_written(START_PUBLISH) {
                holder.kill();
                let _ = writeln!(
                    stdout,
                    "err the first files could not be written within 2 s"
                );
                let _ = stdout.flush();
                fuxix::process::end_now(1);
                return ExitCode::FAILURE;
            }
            let said = writeln!(stdout, "ok").and_then(|()| stdout.flush());
            if detach_stdio().is_err() || said.is_err() {
                holder.refuse("start", "its standard streams could not be detached");
            }
            holder.serve();
            fuxix::process::end_now(0);
            ExitCode::SUCCESS
        }
        Err(failure) => {
            let _ = writeln!(stdout, "err {failure}");
            let _ = stdout.flush();
            ExitCode::FAILURE
        }
    }
}

/// The null device, for the standard streams of a process that is done
/// with them.
fn detach_stdio() -> std::io::Result<()> {
    let null = File::options().read(true).write(true).open("/dev/null")?;
    fuxix::io::redirect_stdout(&null).map_err(std::io::Error::from)?;
    fuxix::io::redirect_stdin(&null).map_err(std::io::Error::from)
}

/// A random id for this life of the pane, as hex.
fn new_id() -> Result<String, Failure> {
    use std::io::Read;
    let mut bytes = [0u8; 8];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| io("/dev/urandom", e))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Makes a FIFO in `dir` and opens it for reading, holding a write end of
/// its own so that a writer closing is no end of file.
fn fifo(dir: &Path, name: &str) -> Result<File, Failure> {
    let path = dir.join(name);
    fuxix::file::private_fifo(&path).map_err(|e| io(path.display(), e))?;
    let file = File::options()
        .read(true)
        .write(true)
        .custom_flags(fuxix::file::NOFOLLOW | fuxix::file::NONBLOCK)
        .open(&path)
        .map_err(|e| io(path.display(), e))?;
    let is_fifo = file.metadata().is_ok_and(|m| m.file_type().is_fifo());
    match is_fifo {
        true => Ok(file),
        false => Err(Failure::new(format!("{} is no FIFO", path.display()))),
    }
}

fn terminal(spec: &Spec, size: Size) -> Result<Parser, Failure> {
    let identity = Identity {
        name: "fux3",
        version: env!("CARGO_PKG_VERSION"),
    };
    let options = Options::new()
        .with(Feature::ExtendedReplies)
        .with(Feature::ModeReports)
        .with(Feature::InBandResize)
        .with(Feature::SizeReports)
        .with(Feature::KittyKeyboard)
        .with(Feature::Reflow)
        .with(Feature::SettingReports)
        .with(Feature::Pictures)
        .with_identity(Some(identity));
    let mut parser = Parser::with_options(size, TERMINAL_HISTORY, options)
        .map_err(|e| Failure::new(format!("a terminal: {e}")))?;
    parser.set_picture_bytes(PICTURE_BYTES);
    parser.set_host_graphics(graphics(spec.cell, spec.pictures)?);
    Ok(parser)
}

fn graphics(cell: (u16, u16), pictures: bool) -> Result<Graphics, Failure> {
    let size = CellSize::new(cell.0, cell.1).ok_or_else(|| {
        let (w, h) = CellSize::LEAST;
        Failure::new(format!(
            "a cell {}x{}: at least {w}x{h} pixels",
            cell.0, cell.1
        ))
    })?;
    Ok(match pictures {
        true => Graphics::Sixel(size),
        false => Graphics::Cell(size),
    })
}

/// Claims the name, makes the files, opens the terminal and starts the
/// program. Whatever fails gives the name back.
fn prepare(spec: Spec) -> Result<Holder, Failure> {
    session::check_name(&spec.workspace)?;
    session::check_name(&spec.name)?;
    let size = checked_size(spec.cols, spec.rows)?;
    if !(MIN_HISTORY..=MAX_HISTORY).contains(&spec.history) {
        return Err(Failure::new(format!(
            "history {}: at most {MAX_HISTORY}",
            spec.history
        )));
    }
    let parser = terminal(&spec, size)?;
    let id = new_id()?;
    // Before anything could start: what the program leaves behind stays
    // this process's to find and kill.
    fuxix::process::adopt_orphans().map_err(|e| io("adopting orphans", e))?;
    let children =
        fuxix::signal::Pipe::new(&[Signal::Chld]).map_err(|e| io("the signal pipe", e))?;
    let session = Session {
        root: spec.root.clone(),
    };
    session::check_private(&session.root)?;
    let (dir, held) = session
        .workspace(&spec.workspace, true)?
        .claim(&spec.name)?;
    let made = (|| {
        let in_fifo = fifo(&dir, files::IN)?;
        let ctl_fifo = fifo(&dir, files::CTL)?;
        let images = dir.join(files::IMAGES);
        fuxix::file::private_dir(&images).map_err(|e| io(images.display(), e))?;
        let history = dir.join(files::HISTORY);
        let mut options = File::options();
        options.write(true).create_new(true);
        fuxix::file::private_file(&mut options)
            .open(&history)
            .map_err(|e| io(history.display(), e))?;
        let publisher =
            Publisher::start(dir.clone(), spec.history).map_err(|e| io("the publisher", e))?;
        let (master, job) = start_program(&spec, size, &id)?;
        Ok::<_, Failure>((in_fifo, ctl_fifo, publisher, master, job))
    })();
    let (in_fifo, ctl_fifo, publisher, master, job) = match made {
        Ok(made) => made,
        Err(failure) => {
            if let Ok(_lock) = session.lock() {
                let _ = session::remove_pane(&dir, &held);
            }
            return Err(failure);
        }
    };
    Ok(Holder {
        session,
        dir,
        held: Some(held),
        workspace: spec.workspace,
        name: spec.name,
        labels: spec.labels,
        id,
        version: 1,
        parser,
        master,
        job: Some(job),
        children,
        in_fifo,
        ctl_fifo,
        lines: ctl::Lines::default(),
        input: Input::default(),
        unparsed: Vec::new(),
        parsed: 0,
        open: true,
        quiet: false,
        check_exit: true,
        exit: None,
        state: "running".to_owned(),
        commands: 0,
        refused: 0,
        errors: VecDeque::new(),
        errors_changed: true,
        in_dropped: 0,
        exit_written: false,
        cell: spec.cell,
        pictures: spec.pictures,
        history_end: 0,
        history: Vec::new(),
        images: Images {
            budget: IMAGE_PIXELS_BURST,
            ..Images::default()
        },
        publisher,
        history_cap: spec.history,
        dirty: true,
        published_at: None,
        buffer: vec![0; READ],
    })
}

fn start_program(spec: &Spec, size: Size, id: &str) -> Result<(Master, Job), Failure> {
    let (master, slave) =
        fuxix::pty::open(size.nonzero().into()).map_err(|e| io("opening a terminal", e))?;
    let program = spec
        .argv
        .first()
        .map_or_else(|| "?".into(), |p| p.to_string_lossy().into_owned());
    let (child, job) = slave
        .spawn(
            &spec.argv,
            |command| {
                command
                    .current_dir(&spec.cwd)
                    .env("TERM", "xterm-256color")
                    .env("TERM_PROGRAM", "fux3")
                    .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"))
                    .env("FUX3_DIR", &spec.root)
                    .env("FUX3_WORKSPACE", &spec.workspace)
                    .env("FUX3_PANE", &spec.name)
                    .env(
                        "FUX3_PANE_DIR",
                        spec.root.join(&spec.workspace).join(&spec.name),
                    )
                    .env("FUX3_PANE_ID", id)
                    .envs(spec.env.iter().map(|(k, v)| (k, v)));
            },
            fuxix::process::Dispositions::Default,
        )
        .map_err(|e| io(format!("starting {program} in {}", spec.cwd.display()), e))?;
    drop(child);
    Ok((master, job))
}

impl Holder {
    /// Something a reader of `screen` should see happened.
    fn advance(&mut self) {
        self.version = self.version.saturating_add(1);
        self.dirty = true;
    }

    fn running(&self) -> bool {
        self.state == "running" && self.exit.is_none()
    }

    /// Notes a refusal in `errors`.
    fn refuse(&mut self, what: &str, why: impl std::fmt::Display) {
        if self.errors.len() >= ERRORS_KEPT {
            self.errors.pop_front();
        }
        let line = format!("{what}: {why}").replace(['\n', '\r'], " ");
        self.errors.push_back(line);
        self.errors_changed = true;
        self.dirty = true;
    }

    fn serve(&mut self) {
        let mut poll = Poll::default();
        loop {
            let now = Instant::now();
            let timeout = self.timeout(now);
            let (master, fifo_in, fifo_ctl, children) = self.wait(&mut poll, timeout);
            let now = Instant::now();
            if children {
                while let Some(signal) = self.children.next() {
                    self.check_exit |= signal == Signal::Chld;
                }
            }
            self.notice_exit(now);
            if master.intersects(Events::IN | Events::HUP | Events::ERR) || self.exit.is_some() {
                self.read_terminal();
            }
            self.parse();
            self.settle(now);
            let queued = self.input.waiting();
            if fifo_ctl && self.read_ctl() {
                return;
            }
            if fifo_in {
                self.read_in();
            }
            if self.running()
                && self.open
                && !self.input.queue.is_empty()
                && let Err(errno) = self.input.write(&self.master)
            {
                self.refuse("in", format!("writing to the program: {errno}"));
                self.input = Input::default();
            }
            // `status` says what waits: it is published again as that drains.
            self.dirty |= self.input.waiting() != queued;
            if self.input.lost_replies > 0 {
                let lost = std::mem::take(&mut self.input.lost_replies);
                self.refuse(
                    "terminal",
                    format!("{lost} replies to the program were dropped"),
                );
            }
            self.gather_history();
            if self.dirty && self.publish_due(now) {
                self.publish(now);
            }
        }
    }

    fn publish_due(&self, now: Instant) -> bool {
        self.published_at
            .is_none_or(|at| now.saturating_duration_since(at) >= INTERVAL)
    }

    /// How long the next poll may sleep.
    fn timeout(&self, now: Instant) -> Option<Duration> {
        let parsing = self.parsed < self.unparsed.len() || self.parser.has_work_left();
        if (parsing && self.input.reply_room() >= REPLY_NEED) || self.check_exit {
            return Some(Duration::ZERO);
        }
        let publish = (self.dirty).then(|| self.published_at.map_or(now, |at| after(at, INTERVAL)));
        let settle = (self.exit.as_ref()).map(|exit| exit.settled_at.max(now).min(exit.latest));
        let next = publish.into_iter().chain(settle).min()?;
        let wait = next.saturating_duration_since(now);
        Some(match self.exit.is_some() {
            // Looked at again soon, never at once: no spin while it settles.
            true => wait.clamp(Duration::from_millis(1), Duration::from_millis(5)),
            false => wait,
        })
    }

    /// Waits for anything to do: the terminal's events, then whether `in`,
    /// `ctl` and the signal pipe are ready.
    fn wait(&self, poll: &mut Poll, timeout: Option<Duration>) -> (Events, bool, bool, bool) {
        let live = self.open && self.exit.is_none() && self.state == "running";
        let reading = self.open && self.parsed >= self.unparsed.len() && self.exit.is_none();
        let writing = live && !self.input.queue.is_empty();
        let mut interest = Events::NONE;
        if reading {
            interest |= Events::IN;
        }
        if writing {
            interest |= Events::OUT;
        }
        let mut fds = poll.list();
        let children = fds.push(PollFd::new(self.children.reader(), Events::IN));
        let ctl = fds.push(PollFd::new(&self.ctl_fifo, Events::IN));
        // Typed bytes are taken only while they have room; once the program
        // has ended they are taken to be dropped.
        let typing = self.input.room() >= FIFO_READ || !self.running() || !self.open;
        let fifo_in = fds.push(PollFd::new(
            &self.in_fifo,
            if typing { Events::IN } else { Events::NONE },
        ));
        let master = (reading || writing).then(|| fds.push(PollFd::new(&self.master, interest)));
        match fds.poll(timeout) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
        (
            master.map_or(Events::NONE, |entry| fds.events(entry)),
            !fds.events(fifo_in).is_empty(),
            !fds.events(ctl).is_empty(),
            !fds.events(children).is_empty(),
        )
    }

    fn notice_exit(&mut self, now: Instant) {
        if !std::mem::take(&mut self.check_exit) {
            return;
        }
        let leader = self.job.as_ref().map(Job::leader);
        fuxix::process::reap_orphans(|pid| Some(pid) == leader);
        if self.exit.is_some() || self.state != "running" {
            if self.state != "running" {
                self.reap_if_alone();
            }
            return;
        }
        // Read without reaping: the job stays whole, so a kill can still
        // reach what the program left behind.
        let Some(Ok(Some(status))) = self.job.as_ref().map(Job::ended) else {
            return;
        };
        self.exit = Some(Exit {
            ending: ending(status),
            settled_at: after(now, SETTLE),
            latest: after(now, DRAIN_MOST),
        });
    }

    fn read_terminal(&mut self) {
        if !self.open || self.parsed < self.unparsed.len() {
            return;
        }
        match fuxix::io::read(&self.master, &mut self.buffer) {
            Ok(0) | Err(Errno::IO) => self.open = false,
            Ok(n) => {
                self.unparsed = self.buffer.get(..n).unwrap_or_default().to_vec();
                self.parsed = 0;
                self.quiet = false;
            }
            Err(Errno::AGAIN) => self.quiet = true,
            Err(Errno::INTR) => {}
            Err(errno) if errno.is_peer_gone() => self.open = false,
            Err(errno) => {
                self.open = false;
                self.refuse("terminal", format!("reading the program's output: {errno}"));
            }
        }
    }

    /// Gives waiting output to the terminal, a slice at a time, as long as
    /// the replies it may cause have room.
    fn parse(&mut self) {
        // Settled is settled: what a child left behind still prints is read,
        // so it never blocks, and dropped, so the last screen stays.
        if self.exit.is_none() && self.state != "running" {
            self.unparsed = Vec::new();
            self.parsed = 0;
            return;
        }
        let mut any = false;
        loop {
            let rest = self.unparsed.get(self.parsed..).unwrap_or_default();
            if rest.is_empty() && !self.parser.has_work_left() {
                break;
            }
            if self.input.reply_room() < REPLY_NEED {
                break;
            }
            let slice = rest.get(..SLICE.min(rest.len())).unwrap_or_default();
            let stopped = match self.parser.process_until_stop(slice, &mut self.input) {
                None => {
                    self.parsed = self.parsed.saturating_add(slice.len());
                    false
                }
                Some((taken, stop)) => {
                    self.parsed = self.parsed.saturating_add(taken);
                    matches!(stop, Stop::Budget)
                }
            };
            any = true;
            if stopped {
                break;
            }
        }
        if self.parsed >= self.unparsed.len() {
            self.unparsed = Vec::new();
            self.parsed = 0;
        }
        if any {
            self.advance();
        }
    }

    /// Settles the ended program's state once its last output is in.
    fn settle(&mut self, now: Instant) {
        let Some(exit) = &self.exit else {
            return;
        };
        let parsed = self.unparsed.is_empty() && !self.parser.has_work_left();
        let settled = now >= exit.settled_at && self.quiet;
        if !(parsed && (!self.open || settled)) && now < exit.latest {
            return;
        }
        self.state = exit.ending.clone();
        self.exit = None;
        let unwritten = self.input.waiting();
        if unwritten > 0 {
            self.refuse(
                "in",
                format!("{unwritten} bytes never reached the program: it ended"),
            );
        }
        self.input = Input::default();
        self.advance();
        self.reap_if_alone();
    }

    /// An ended program is kept unreaped only while something it started
    /// lives, which `kill` must still reach by its session.
    fn reap_if_alone(&mut self) {
        let Some(job) = &self.job else {
            return;
        };
        #[cfg(target_os = "linux")]
        let others = {
            let leader = job.leader();
            (fuxix::process::tree().pids.iter()).any(|pid| *pid != leader)
        };
        #[cfg(not(target_os = "linux"))]
        let others = !job.members().pids.is_empty();
        if !others && let Some(job) = self.job.take() {
            drop(job.reap());
            // `status`'s pid says none from now on.
            self.dirty = true;
        }
    }

    /// Reads typed bytes: queued whole while the program runs, dropped
    /// with a line in `errors` once it has ended.
    fn read_in(&mut self) {
        let mut buffer = [0u8; FIFO_READ];
        let mut dropped = 0usize;
        for _ in 0..16 {
            if self.running() && self.open && self.input.room() < FIFO_READ {
                break;
            }
            let n = match fuxix::io::read(&self.in_fifo, &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let bytes = buffer.get(..n).unwrap_or_default();
            if self.running() && self.open {
                // Room was checked: a read never exceeds it.
                let _ = self.input.admit(bytes.to_vec());
            } else {
                dropped = dropped.saturating_add(n);
            }
        }
        if dropped > 0 {
            // One line for all of them, kept last: drops never push a
            // command's refusal out of `errors`.
            self.in_dropped = self
                .in_dropped
                .saturating_add(u64::try_from(dropped).unwrap_or(u64::MAX));
            let why = match self.running() {
                true => "the program closed its terminal".to_owned(),
                false => format!("the program has ended ({})", self.state),
            };
            self.errors.retain(|line| !line.starts_with("in: "));
            let total = self.in_dropped;
            self.refuse("in", format!("{total} bytes dropped in all: {why}"));
        }
    }

    /// Reads and acts on `ctl`'s commands. True once the pane is gone.
    fn read_ctl(&mut self) -> bool {
        let mut buffer = [0u8; FIFO_READ];
        let mut lines = Vec::new();
        for _ in 0..16 {
            match fuxix::io::read(&self.ctl_fifo, &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => self
                    .lines
                    .push(buffer.get(..n).unwrap_or_default(), &mut lines),
            }
        }
        for line in lines {
            self.commands = self.commands.saturating_add(1);
            self.dirty = true;
            let number = self.commands;
            let (shown, outcome) = match line {
                Ok(line) => {
                    let outcome = ctl::parse(&line).and_then(|command| self.act(command));
                    (line, outcome)
                }
                Err(failure) => ("?".to_owned(), Err(failure)),
            };
            match outcome {
                Ok(true) => return true,
                Ok(false) => {}
                Err(failure) => {
                    self.refused = self.refused.saturating_add(1);
                    self.refuse(&format!("{number} {shown}"), failure);
                }
            }
        }
        false
    }

    /// Acts on one command. True once the pane is gone.
    fn act(&mut self, command: Command) -> Result<bool, Failure> {
        match command {
            Command::Kill => return Ok(self.kill()),
            Command::Resize { cols, rows } => {
                self.must_run()?;
                let size = checked_size(cols, rows)?;
                if self.parser.screen().size() != size {
                    self.resize(size)?;
                }
            }
            Command::Send(path) => {
                self.takes_input()?;
                let bytes = read_sent(&path)?;
                self.input.admit(bytes)?;
            }
            Command::Paste(path) => {
                self.takes_input()?;
                let bytes = read_sent(&path)?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| Failure::new(format!("{} is not UTF-8 text", path.display())))?;
                let mut out = Vec::new();
                let bracketed = self.parser.screen().mode(Mode::BracketedPaste);
                fux_vt::keys::encode::paste(&text, bracketed, &mut out);
                self.input.admit(out)?;
            }
            Command::Signal(name) => {
                self.must_run()?;
                self.signal(&name)?;
            }
            Command::Cell { width, height } => {
                self.must_run()?;
                let graphics = graphics((width, height), self.pictures)?;
                self.parser.set_host_graphics(graphics);
                self.cell = (width, height);
                self.advance();
            }
        }
        Ok(false)
    }

    /// Signals the terminal's foreground process group, else the program's.
    fn signal(&self, name: &str) -> Result<(), Failure> {
        let signal = (Signal::ALL.iter().copied())
            .find(|signal| signal.name() == name)
            .ok_or_else(|| Failure::new(format!("no signal {name:?}")))?;
        let group = fuxix::terminal::foreground_group(&self.master)
            .map_err(|e| io("finding the foreground group", e))?
            .or_else(|| self.job.as_ref().map(Job::leader))
            .ok_or_else(|| Failure::new("no process to signal"))?;
        fuxix::process::kill_group(group, signal).map_err(|e| io("signalling", e))
    }

    fn must_run(&self) -> Result<(), Failure> {
        let ended = self
            .exit
            .as_ref()
            .map_or(self.state.as_str(), |e| e.ending.as_str());
        match self.running() {
            true => Ok(()),
            false => Err(Failure::new(format!("the program has ended: {ended}"))),
        }
    }

    /// Refuses input once the program's terminal is closed: its program
    /// may run on, but nothing written reaches it.
    fn takes_input(&self) -> Result<(), Failure> {
        self.must_run()?;
        match self.open {
            true => Ok(()),
            false => Err(Failure::new(
                "the program closed its terminal: nothing reaches it",
            )),
        }
    }

    /// Prepares the new screen, asks the system, and only then shows it: a
    /// refused resize leaves the screen and its version as they were.
    fn resize(&mut self, size: Size) -> Result<(), Failure> {
        // What scrolled off before is taken first: a reflow renumbers the
        // history, and its rows are not new lines.
        self.gather_history();
        let mut next = self.parser.clone();
        next.resize(size)
            .map_err(|e| Failure::new(format!("resizing: {e}")))?;
        fuxix::terminal::set_window_size(&self.master, size.nonzero().into(), None)
            .map_err(|e| io("resizing the terminal", e))?;
        self.parser = next;
        // Rows the reflow made of old lines are not appended again (rows it
        // pushed off the screen are not appended either: README).
        self.history_end = self.parser.screen().history_numbers().end;
        if let Some(report) = self.parser.resize_report() {
            self.input.reply(&report);
        }
        self.advance();
        Ok(())
    }

    /// Takes the rows that scrolled into the terminal's history since the
    /// last look, as plain text.
    fn gather_history(&mut self) {
        let screen = self.parser.screen();
        let numbers = screen.history_numbers();
        // A reset or a reflow starts the numbers over or takes rows back.
        if numbers.end < self.history_end {
            self.history_end = numbers.end;
        }
        if numbers.end == self.history_end {
            return;
        }
        if numbers.start > self.history_end {
            let lost = numbers.start.saturating_sub(self.history_end);
            let line = format!("[fux3: {lost} lines scrolled off faster than they were kept]\n");
            self.history.extend_from_slice(line.as_bytes());
            self.history_end = numbers.start;
        }
        let skip = usize::try_from(self.history_end.saturating_sub(numbers.start)).unwrap_or(0);
        let count = usize::try_from(numbers.end.saturating_sub(self.history_end)).unwrap_or(0);
        for row in screen.main_rows().skip(skip).take(count) {
            let text = row.text();
            if row.continues() {
                self.history.extend_from_slice(text.as_bytes());
            } else {
                self.history
                    .extend_from_slice(text.trim_end_matches(' ').as_bytes());
                self.history.push(b'\n');
            }
        }
        self.history_end = numbers.end;
    }

    fn head(&self) -> Head {
        let screen = self.parser.screen();
        let size = screen.size();
        let (cursor_row, cursor_col) = screen.cursor_position();
        Head {
            id: self.id.clone(),
            version: self.version,
            cols: size.cols(),
            rows: size.rows(),
            cursor_row,
            cursor_col,
            cursor_shown: screen.mode(Mode::ShowCursor),
        }
    }

    /// The pictures shown now, each written once to `images/` while the
    /// rate allows; those no longer shown are removed.
    fn place_pictures(&mut self, now: Instant, out: &mut publish::Publish) -> Vec<Placed> {
        let since = self
            .images
            .filled_at
            .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        let refill = u64::try_from(since.as_millis())
            .unwrap_or(u64::MAX)
            .saturating_mul(IMAGE_PIXELS_PER_SECOND)
            / 1000;
        if refill > 0 || self.images.filled_at.is_none() {
            self.images.budget = self
                .images
                .budget
                .saturating_add(refill)
                .min(IMAGE_PIXELS_BURST);
            self.images.filled_at = Some(now);
        }
        let mut placed = Vec::new();
        let mut kept = Vec::new();
        // Only those with a row on the screen: one scrolled wholly into
        // history shows nothing, and its file goes.
        let shown: Vec<_> = (self.parser.screen().pictures())
            .filter(|p| p.row.saturating_add(i64::from(p.rows)) > 0)
            .collect();
        for picture in shown {
            let known =
                (self.images.written.iter()).position(|(p, _)| Arc::ptr_eq(p, picture.picture));
            let name = match known {
                Some(at) => self.images.written.get(at).map(|(_, n)| n.clone()),
                None => {
                    let pixels = u64::from(picture.picture.width())
                        .saturating_mul(picture.picture.height().into());
                    (pixels <= self.images.budget).then(|| {
                        self.images.budget = self.images.budget.saturating_sub(pixels);
                        self.images.next = self.images.next.saturating_add(1);
                        let name = format!("{}.sixel", self.images.next);
                        out.new_images
                            .push((name.clone(), Arc::clone(picture.picture)));
                        name
                    })
                }
            };
            let Some(name) = name else {
                // Over the rate: written at a later publish, if still shown.
                self.dirty = true;
                continue;
            };
            kept.push((Arc::clone(picture.picture), name.clone()));
            placed.push(Placed {
                file: name,
                row: picture.row,
                col: picture.col,
                cols: picture.cols,
                rows: picture.rows,
                cropped: picture.cropped,
                cell: picture.cell,
            });
        }
        for (picture, name) in std::mem::take(&mut self.images.written) {
            if !kept.iter().any(|(p, _)| Arc::ptr_eq(p, &picture)) {
                out.gone_images.push(name);
            }
        }
        self.images.written = kept;
        placed
    }

    fn status(&self) -> Vec<u8> {
        let screen = self.parser.screen();
        let size = screen.size();
        let on = |b: bool| if b { "on" } else { "off" };
        let mut s = String::new();
        let mut line = |text: String| {
            s.push_str(&text);
            s.push('\n');
        };
        line(format!("id {}", self.id));
        line(format!("version {}", self.version));
        line(format!("workspace {}", self.workspace));
        line(format!("name {}", self.name));
        line(format!("size {} {}", size.cols(), size.rows()));
        line(format!("cell {} {}", self.cell.0, self.cell.1));
        line(format!("pictures {}", on(self.pictures)));
        line(format!("state {}", self.state));
        match &self.job {
            Some(job) => line(format!("pid {}", job.leader())),
            None => line("pid none".to_owned()),
        }
        line(format!("holder {}", std::process::id()));
        line(format!("keys {}", key_mode(screen.key_mode())));
        line(format!(
            "bracketed-paste {}",
            on(screen.mode(Mode::BracketedPaste))
        ));
        line(format!("newline-mode {}", on(screen.mode(Mode::NewLine))));
        line(format!(
            "focus-reports {}",
            on(screen.mode(Mode::FocusReporting))
        ));
        line(format!(
            "mouse {} {}",
            format!("{:?}", screen.mouse_protocol_mode()).to_lowercase(),
            format!("{:?}", screen.mouse_protocol_encoding()).to_lowercase()
        ));
        line(format!(
            "alternate-screen {}",
            on(screen.mode(Mode::AlternateScreen))
        ));
        for (key, value) in &self.labels {
            line(format!("label {key} {value}"));
        }
        line(format!("commands {}", self.commands));
        line(format!("refused {}", self.refused));
        line(format!("queued {}", self.input.waiting()));
        s.into_bytes()
    }

    /// The `exit` file's line, once: when the program's end has settled.
    fn exit_line(&mut self) -> Option<Vec<u8>> {
        let ended = self.state.starts_with("exited ") || self.state.starts_with("signalled ");
        (ended && !std::mem::replace(&mut self.exit_written, true))
            .then(|| format!("state {}\n", self.state).into_bytes())
    }

    /// Hands the publisher the screens of now.
    fn publish(&mut self, now: Instant) {
        let head = self.head();
        self.dirty = false;
        let mut out = publish::Publish {
            history: std::mem::take(&mut self.history),
            ..publish::Publish::default()
        };
        let placed = self.place_pictures(now, &mut out);
        let screen = self.parser.screen();
        let errors = std::mem::take(&mut self.errors_changed).then(|| {
            let mut text = String::new();
            for line in &self.errors {
                text.push_str(line);
                text.push('\n');
            }
            text.into_bytes()
        });
        out.files = Some(publish::Files {
            screen: publish::plain(screen, &head),
            styled: publish::styled(screen, &head, &placed),
            status: self.status(),
            errors,
            exit: self.exit_line(),
        });
        // One hand-over: a picture's file is removed only with the screens
        // that no longer name it.
        self.publisher.hand(out);
        self.published_at = Some(now);
    }

    /// Reads and drops what the terminal holds, for a program that may be
    /// waiting to write as it dies.
    fn discard_output(&mut self) {
        for _ in 0..64 {
            if !self.open {
                return;
            }
            match fuxix::io::read(&self.master, &mut self.buffer) {
                Ok(0) | Err(Errno::IO) => self.open = false,
                Ok(_) | Err(Errno::INTR) => {}
                Err(errno) if errno.is_peer_gone() => self.open = false,
                Err(_) => return,
            }
        }
    }

    fn gone_by(&mut self, deadline: Instant) -> bool {
        loop {
            self.discard_output();
            if matches!(self.job.as_ref().map(Job::ended), Some(Ok(Some(_))) | None) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Ends the program and everything it started. Complete, the directory
    /// goes and so does the holder: true. Incomplete, `status` says what is
    /// left and `kill` may be sent again: false.
    fn kill(&mut self) -> bool {
        let mut missed = Vec::new();
        if let Some(job) = &self.job {
            drop(fuxix::pty::hang_up(&self.master, job));
            self.gone_by(after(Instant::now(), HANGUP_GRACE));
        }
        if let Some(job) = &self.job {
            let sent = job.kill(Reach::Session);
            let leader = job.leader();
            let ended = matches!(job.ended(), Ok(Some(_)));
            for (pid, errno) in sent.failed {
                if errno != Errno::SRCH && !(ended && pid == leader) {
                    missed.push(format!("{pid} could not be killed: {errno}"));
                }
            }
            match sent.scan {
                Scan::Complete => {}
                Scan::Truncated => missed.push("the list of processes was cut short".to_owned()),
                Scan::Failed(errno) => missed.push(format!("listing processes: {errno}")),
            }
        }
        // On Linux everything the program started is this process's: its
        // descendants, and the orphans it adopted, a new session or not.
        #[cfg(target_os = "linux")]
        {
            let tree = fuxix::process::tree();
            if tree.scan != Scan::Complete {
                missed.push(format!(
                    "the list of its processes was incomplete: {:?}",
                    tree.scan
                ));
            }
            for pid in tree.pids {
                let _ = fuxix::process::kill(pid, Signal::Kill);
            }
        }
        if !self.gone_by(after(Instant::now(), KILL_WAIT)) {
            missed.push("the program did not end in time".to_owned());
        }
        if let Some(alive) = self.members_left(after(Instant::now(), KILL_WAIT)) {
            missed.push(format!("{alive} of its processes are still there"));
        }
        // Reaped only once nothing it started is left: a kill sent again
        // must still find its session's members (macOS has no subreaper).
        if missed.is_empty()
            && let Some(job) = self.job.take()
            && let Err(job) = job.reap()
        {
            missed.push(format!("process {} was not reaped", job.leader()));
            self.job = Some(job);
        }
        fuxix::process::reap_orphans(|_| false);
        if missed.is_empty()
            && let Some(held) = self.held.take()
        {
            match self.remove(&held) {
                Ok(()) => return true,
                Err(failure) => {
                    missed.push(failure.0);
                    self.held = Some(held);
                }
            }
        }
        self.state = format!("kill-incomplete {}", missed.join("; "));
        self.exit = None;
        self.advance();
        let why = self.state.clone();
        self.refuse(&format!("{} kill", self.commands), why);
        false
    }

    /// Removes the pane's directory, once everything handed to the
    /// publisher is written.
    fn remove(&mut self, held: &File) -> Result<(), Failure> {
        // The lock first: a kill that can't remove the directory still has
        // its publisher, to say so in `status` and `errors`.
        let _lock = self.session.lock()?;
        self.publisher.stop();
        let removed = session::remove_pane(&self.dir, held);
        if removed.is_err() {
            self.publisher = Publisher::start(self.dir.clone(), self.history_cap)
                .map_err(|e| io("the publisher", e))?;
        }
        removed
    }

    /// How many of the job's other processes outlast `deadline`, if any do.
    fn members_left(&self, deadline: Instant) -> Option<usize> {
        let job = self.job.as_ref()?;
        loop {
            let leader = job.leader();
            fuxix::process::reap_orphans(|pid| pid == leader);
            #[cfg(target_os = "linux")]
            let alive = {
                let left: Vec<_> = (fuxix::process::tree().pids.into_iter())
                    .filter(|pid| *pid != leader)
                    .collect();
                for pid in &left {
                    let _ = fuxix::process::kill(*pid, Signal::Kill);
                }
                left.len()
            };
            #[cfg(not(target_os = "linux"))]
            let alive = job.members().pids.len();
            if alive == 0 {
                return None;
            }
            if Instant::now() >= deadline {
                return Some(alive);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// `keys` in `status`: how the program asked for its keys.
fn key_mode(mode: fux_vt::keys::encode::KeyMode) -> String {
    use fux_vt::keys::encode::{KeyMode, OtherKeysLevel, Report};
    let on = |b: bool| if b { "on" } else { "off" };
    match mode {
        KeyMode::Legacy {
            application,
            keypad,
            other_keys,
            backarrow,
        } => {
            let other = match other_keys {
                None => "off",
                Some(OtherKeysLevel::Usual) => "1",
                Some(OtherKeysLevel::All) => "2",
                Some(OtherKeysLevel::Unmodified) => "3",
            };
            format!(
                "legacy cursor={} keypad={} other-keys={other} backarrow={}",
                if application { "application" } else { "normal" },
                if keypad { "application" } else { "normal" },
                on(backarrow)
            )
        }
        KeyMode::Kitty {
            alternate,
            report,
            backarrow,
        } => {
            let report = match report {
                Report::Disambiguated => "disambiguate",
                Report::AllKeys => "all",
                Report::AllKeysWithText => "all-with-text",
            };
            format!(
                "kitty report={report} alternate={} backarrow={}",
                on(alternate),
                on(backarrow)
            )
        }
    }
}

/// Reads `status`'s `keys` line back into the mode to encode keys in.
pub fn parse_key_mode(text: &str) -> Option<fux_vt::keys::encode::KeyMode> {
    use fux_vt::keys::encode::{KeyMode, OtherKeysLevel, Report};
    let mut words = text.split(' ');
    let kind = words.next()?;
    let fields: Vec<(&str, &str)> = words.filter_map(|w| w.split_once('=')).collect();
    let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    let backarrow = get("backarrow")? == "on";
    match kind {
        "legacy" => Some(KeyMode::Legacy {
            application: get("cursor")? == "application",
            keypad: get("keypad")? == "application",
            other_keys: match get("other-keys")? {
                "1" => Some(OtherKeysLevel::Usual),
                "2" => Some(OtherKeysLevel::All),
                "3" => Some(OtherKeysLevel::Unmodified),
                _ => None,
            },
            backarrow,
        }),
        "kitty" => Some(KeyMode::Kitty {
            alternate: get("alternate")? == "on",
            report: match get("report")? {
                "all" => Report::AllKeys,
                "all-with-text" => Report::AllKeysWithText,
                _ => Report::Disambiguated,
            },
            backarrow,
        }),
        _ => None,
    }
}

/// Reads a file `send` names: a regular file of this user's, not a link,
/// a device or a FIFO, its size checked before it is read, read with a
/// bound.
fn read_sent(path: &Path) -> Result<Vec<u8>, Failure> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let shown = path.display();
    let file = File::options()
        .read(true)
        .custom_flags(fuxix::file::NOFOLLOW | fuxix::file::NONBLOCK | fuxix::file::NOCTTY)
        .open(path)
        .map_err(|e| io(&shown, e))?;
    let meta = file.metadata().map_err(|e| io(&shown, e))?;
    if !meta.is_file() {
        return Err(Failure::new(format!("{shown} is not a regular file")));
    }
    if meta.uid() != fuxix::process::geteuid() {
        return Err(Failure::new(format!(
            "{shown} belongs to uid {}",
            meta.uid()
        )));
    }
    let len = usize::try_from(meta.len()).unwrap_or(usize::MAX);
    if len > MAX_SEND {
        return Err(Failure::new(format!(
            "{shown} is {len} bytes, at most {MAX_SEND}"
        )));
    }
    let mut bytes = Vec::with_capacity(len);
    let limit = u64::try_from(MAX_SEND)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|e| io(&shown, e))?;
    if bytes.len() > MAX_SEND {
        return Err(Failure::new(format!("{shown} grew past {MAX_SEND} bytes")));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A send that does not fit is refused whole: what waits is what
    /// waited, and everything that still fits is taken.
    #[test]
    fn a_refused_send_adds_nothing() -> Result<(), Failure> {
        let mut input = Input::default();
        input.admit(vec![b'x'; MAX_TYPED.saturating_sub(10)])?;
        assert!(input.admit(vec![b'y'; 11]).is_err());
        assert_eq!(input.queue.len(), 1);
        input.admit(vec![b'z'; 10])?;
        assert_eq!(input.typed, MAX_TYPED);
        Ok(())
    }

    /// The key mode `status` shows reads back as the same mode.
    #[test]
    fn the_key_mode_reads_back() {
        use fux_vt::keys::encode::{KeyMode, OtherKeysLevel, Report};
        for mode in [
            KeyMode::Legacy {
                application: true,
                keypad: false,
                other_keys: Some(OtherKeysLevel::All),
                backarrow: true,
            },
            KeyMode::Kitty {
                alternate: true,
                report: Report::AllKeysWithText,
                backarrow: false,
            },
        ] {
            assert_eq!(parse_key_mode(&key_mode(mode)), Some(mode));
        }
    }
}

//! The holder: one process, one poll loop, one program on one terminal. It
//! keeps its pane's files current, takes typed bytes from `in` and
//! commands from `ctl`, answers the program's terminal queries with nobody
//! watching, and knows nothing of other panes or of viewers. When its
//! program ends it keeps the last screen until it is killed.
use crate::ctl::{self, Command, MAX_SEND};
use crate::files::{self, Head, Placed};
use crate::publish::{self, Publisher};
use crate::session::{self, Session, Workspace};
use crate::{Failure, io};
use fux_vt::sixel::Picture;
use fux_vt::{CellSize, Feature, Graphics, Identity, Mode, Options, Parser, Sink, Size, Stop};
use fuxix::Errno;
use fuxix::file::at;
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{Job, Reach, Scan, Signal, Status};
use fuxix::pty::{Master, Slave};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::FileTypeExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the screens are published at most while output goes on. The
/// first change after a rest is published at once.
pub const INTERVAL: Duration = Duration::from_millis(40);
/// How often an `exit` not yet written is tried again.
const EXIT_RETRY: Duration = Duration::from_secs(1);
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
/// How long what a kill asked to end (panes started inside this one
/// among it) has to end itself before it is killed outright.
#[cfg(target_os = "linux")]
const TERM_GRACE: Duration = Duration::from_secs(1);
/// The longest record `history` holds, whatever its cap.
const MAX_RECORD: usize = 64 << 10;
/// The most variables `--env` names.
pub const MAX_ENV: usize = 64;
/// The most labels, and their keys' and values' lengths.
pub const MAX_LABELS: usize = 16;
const MAX_LABEL_KEY: usize = 32;
const MAX_LABEL_VALUE: usize = 128;

fn after(now: Instant, wait: Duration) -> Instant {
    now.checked_add(wait).unwrap_or(now)
}

/// What a start asks for, as the holder read it from `start`'s arguments.
#[derive(Debug, Clone)]
pub struct Spec {
    pub session: Session,
    pub workspace: String,
    /// The directory the workspace must be ([`Workspace::id`]), if the
    /// starter held one: a workspace removed and made again is another.
    pub workspace_id: Option<String>,
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
    /// The recipe to write once the pane runs; none for a pane started
    /// from its recipe, which stays as it is.
    pub recipe: Option<Vec<u8>>,
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
    /// The pane: its directory, held open and locked for the holder's
    /// life. Every file is named relative to it; where it is listed is
    /// looked up only to take its name away.
    dir: File,
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
    cell: (u16, u16),
    pictures: bool,
    history_end: u64,
    /// The history line being gathered, not yet a whole record.
    history_line: String,
    history: Vec<u8>,
    images: Images,
    publisher: Publisher,
    history_cap: usize,
    dirty: bool,
    published_at: Option<Instant>,
    buffer: Vec<u8>,
}

/// Runs the holder `spec` describes; what it says goes to its starter.
pub fn main(spec: Result<Spec, Failure>) -> ExitCode {
    let mut stdout = std::io::stdout();
    match spec.and_then(prepare) {
        Ok((mut holder, start)) => {
            // `ok` only once the pane is whole, named and running, and its
            // files say so. Otherwise it is ended and `start` says why.
            if let Err(failure) = holder.begin(start) {
                holder.kill();
                let _ = writeln!(stdout, "err {failure}");
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
fn fifo(dir: &File, name: &str) -> Result<File, Failure> {
    at::make_fifo(dir, name).map_err(|e| io(name, e))?;
    let file =
        at::open(dir, name, at::READ_WRITE | fuxix::file::NONBLOCK).map_err(|e| io(name, e))?;
    let is_fifo = file.metadata().is_ok_and(|m| m.file_type().is_fifo());
    match is_fifo {
        true => Ok(file),
        false => Err(Failure::new(format!("{name} is no FIFO"))),
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

/// What is left to do once the pane's directory is made: give it its
/// name, start the program, write the recipe.
struct Start {
    spec: Spec,
    workspace: Workspace,
    slave: Slave,
}

/// Makes the pane, nameless: its directory under a name nothing lists,
/// locked, with its marker, FIFOs and history, and its terminal opened.
/// Whatever fails takes it apart again.
fn prepare(spec: Spec) -> Result<(Holder, Start), Failure> {
    let size = checked_size(spec.cols, spec.rows)?;
    let parser = terminal(&spec, size)?;
    let id = new_id()?;
    // Before anything could start: what the program leaves behind stays
    // this process's to find and kill.
    fuxix::process::adopt_orphans().map_err(|e| io("adopting orphans", e))?;
    // SIGTERM is `kill`: whoever ends a holder politely ends its pane.
    let children = fuxix::signal::Pipe::new(&[Signal::Chld, Signal::Term])
        .map_err(|e| io("the signal pipe", e))?;
    let session = spec.session.clone();
    let workspace = session.workspace(&spec.workspace, true)?;
    if spec
        .workspace_id
        .as_ref()
        .is_some_and(|id| *id != workspace.id())
    {
        return Err(Failure::new(format!(
            "workspace {} was removed",
            spec.workspace
        )));
    }
    let dir = workspace.stage(&id)?;
    let made = (|| {
        let in_fifo = fifo(&dir, files::IN)?;
        let ctl_fifo = fifo(&dir, files::CTL)?;
        at::make_dir(&dir, files::IMAGES).map_err(|e| io(files::IMAGES, e))?;
        files::write_new(&dir, files::HISTORY, b"")?;
        let theirs = dir.try_clone().map_err(|e| io("the pane's directory", e))?;
        let publisher =
            Publisher::start(theirs, spec.history).map_err(|e| io("the publisher", e))?;
        let (master, slave) =
            fuxix::pty::open(size.nonzero().into()).map_err(|e| io("opening a terminal", e))?;
        Ok::<_, Failure>((in_fifo, ctl_fifo, publisher, master, slave))
    })();
    let (in_fifo, ctl_fifo, publisher, master, slave) = match made {
        Ok(made) => made,
        Err(failure) => {
            // Dead and nameless: taken apart here, else by the next clearing.
            if let (true, Some((parent, name))) =
                (session::clear_pane(&dir, true), session::place(&dir))
            {
                let _ = at::remove_dir(&parent, &name);
            }
            return Err(failure);
        }
    };
    let holder = Holder {
        session,
        dir,
        labels: spec.labels.clone(),
        id,
        version: 1,
        parser,
        master,
        job: None,
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
        state: "starting".to_owned(),
        commands: 0,
        refused: 0,
        errors: VecDeque::new(),
        errors_changed: true,
        in_dropped: 0,
        cell: spec.cell,
        pictures: spec.pictures,
        history_end: 0,
        history_line: String::new(),
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
    };
    Ok((
        holder,
        Start {
            spec,
            workspace,
            slave,
        },
    ))
}

fn start_program(spec: &Spec, slave: Slave, id: &str) -> Result<Job, Failure> {
    let program = spec
        .argv
        .first()
        .map_or_else(|| "?".into(), |p| p.to_string_lossy().into_owned());
    let (child, job) = slave
        .spawn(
            &spec.argv,
            |command| {
                // Where the pane is as it starts: `mv` can change all
                // three later, and never its id.
                command
                    .current_dir(&spec.cwd)
                    .env("TERM", "xterm-256color")
                    .env("TERM_PROGRAM", "fux3")
                    .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"))
                    .env("FUX3_DIR", &spec.session.root)
                    .env("FUX3_WORKSPACE", &spec.workspace)
                    .env("FUX3_PANE", &spec.name)
                    .env(
                        "FUX3_PANE_DIR",
                        spec.session.root.join(&spec.workspace).join(&spec.name),
                    )
                    .env("FUX3_PANE_ID", id)
                    .envs(spec.env.iter().map(|(k, v)| (k, v)));
            },
            fuxix::process::Dispositions::Default,
        )
        .map_err(|e| io(format!("starting {program} in {}", spec.cwd.display()), e))?;
    drop(child);
    Ok(job)
}

impl Holder {
    /// Something a reader of `screen` should see happened.
    fn advance(&mut self) {
        self.version = self.version.saturating_add(1);
        self.dirty = true;
    }

    /// Publishes now and waits until it is on disk, whole.
    fn publish_whole(&mut self) -> Result<(), Failure> {
        self.publish(Instant::now());
        match self.publisher.wait_written(START_PUBLISH) {
            true => Ok(()),
            false => Err(Failure::new(
                "the first files could not be written within 2 s",
            )),
        }
    }

    /// Gives the pane, whole, its name; then starts the program, and once
    /// the files say it runs, writes the recipe: a start that fails
    /// leaves the recipe of the last good one.
    fn begin(&mut self, start: Start) -> Result<(), Failure> {
        let Start {
            spec,
            workspace,
            slave,
        } = start;
        self.publish_whole()?;
        workspace.name_pane(&self.dir, &spec.name)?;
        self.job = Some(start_program(&spec, slave, &self.id)?);
        self.state = "running".to_owned();
        self.advance();
        self.publish_whole()?;
        match &spec.recipe {
            Some(text) => crate::recipe::write(workspace.held(), &spec.name, text),
            None => Ok(()),
        }
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
                let mut asked_to_end = false;
                while let Some(signal) = self.children.next() {
                    self.check_exit |= signal == Signal::Chld;
                    asked_to_end |= signal == Signal::Term;
                }
                if asked_to_end && self.kill() {
                    return;
                }
            }
            self.notice_exit(now);
            if master.intersects(Events::IN | Events::HUP | Events::ERR) || self.exit.is_some() {
                self.read_terminal();
            }
            self.parse();
            // A program that closed its terminal gets nothing more: what
            // waited for it is said, once, and goes.
            if !self.open && self.running() && self.input.waiting() > 0 {
                let unwritten = self.input.waiting();
                self.refuse(
                    "in",
                    format!("{unwritten} bytes never reached the program: it closed its terminal"),
                );
                self.input = Input::default();
            }
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
            // An `exit` not yet on disk (a full disk) is tried again, once
            // a second, until it is.
            if self.ended()
                && !self.dirty
                && self
                    .published_at
                    .is_some_and(|at| now.saturating_duration_since(at) >= EXIT_RETRY)
                && !self.publisher.exit_done()
            {
                self.dirty = true;
            }
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
        let retry = (self.ended() && !self.publisher.exit_done())
            .then(|| self.published_at.map_or(now, |at| after(at, EXIT_RETRY)));
        let next = publish.into_iter().chain(settle).chain(retry).min()?;
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
        // The start of a line whose rest is still on the screen: a piece.
        if !self.history_line.is_empty() {
            let limit = (self.history_cap / 2).min(MAX_RECORD);
            let (line, out) = (&mut self.history_line, &mut self.history);
            publish::record(line, out, publish::CONTINUED, true, limit);
        }
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
            self.errors.retain(|line| {
                !(line.starts_with("in: ") && line.contains(" bytes dropped in all: "))
            });
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
                let bytes = files::read_own(&path, MAX_SEND)?;
                self.input.admit(bytes)?;
            }
            Command::Paste(path) => {
                self.takes_input()?;
                let bytes = files::read_own(&path, MAX_SEND)?;
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
        let limit = (self.history_cap / 2).min(MAX_RECORD);
        if numbers.start > self.history_end {
            let lost = numbers.start.saturating_sub(self.history_end);
            let line = format!("[fux3: {lost} lines scrolled off faster than they were kept]");
            publish::record(&mut self.history_line, &mut self.history, "", true, limit);
            publish::record(
                &mut self.history_line,
                &mut self.history,
                &line,
                true,
                limit,
            );
            self.history_end = numbers.start;
        }
        let skip = usize::try_from(self.history_end.saturating_sub(numbers.start)).unwrap_or(0);
        let count = usize::try_from(numbers.end.saturating_sub(self.history_end)).unwrap_or(0);
        for row in screen.main_rows().skip(skip).take(count) {
            let text = row.text();
            let end = !row.continues();
            let text = if end {
                text.trim_end_matches(' ')
            } else {
                &text
            };
            publish::record(&mut self.history_line, &mut self.history, text, end, limit);
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
        let pid =
            (self.job.as_ref()).map_or_else(|| "none".to_owned(), |job| job.leader().to_string());
        let lower = |text: String| text.to_lowercase();
        let mut lines = vec![
            format!("id {}", self.id),
            format!("version {}", self.version),
            format!("size {} {}", size.cols(), size.rows()),
            format!("cell {} {}", self.cell.0, self.cell.1),
            format!("pictures {}", on(self.pictures)),
            format!("state {}", self.state),
            format!("pid {pid}"),
            format!("holder {}", std::process::id()),
            format!("keys {}", key_mode(screen.key_mode())),
            format!("bracketed-paste {}", on(screen.mode(Mode::BracketedPaste))),
            format!("newline-mode {}", on(screen.mode(Mode::NewLine))),
            format!("focus-reports {}", on(screen.mode(Mode::FocusReporting))),
            format!(
                "mouse {} {}",
                lower(format!("{:?}", screen.mouse_protocol_mode())),
                lower(format!("{:?}", screen.mouse_protocol_encoding()))
            ),
            format!(
                "alternate-screen {}",
                on(screen.mode(Mode::AlternateScreen))
            ),
        ];
        lines.extend(
            self.labels
                .iter()
                .map(|(key, value)| format!("label {key} {value}")),
        );
        lines.push(format!("commands {}", self.commands));
        lines.push(format!("refused {}", self.refused));
        lines.push(format!("queued {}\n", self.input.waiting()));
        lines.join("\n").into_bytes()
    }

    /// Whether the program's end has settled.
    fn ended(&self) -> bool {
        self.state.starts_with("exited ") || self.state.starts_with("signalled ")
    }

    /// The `exit` file's line, with every publish once the end has settled,
    /// until the publisher says it is on disk.
    fn exit_line(&self) -> Option<Vec<u8>> {
        (self.ended() && !self.publisher.exit_done())
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
        // What left the session, a pane started inside this one among it,
        // is asked to end too: a holder's SIGTERM is its `kill`, so an
        // inner pane takes itself apart, in a bounded time.
        #[cfg(target_os = "linux")]
        if let Some(leader) = self.job.as_ref().map(Job::leader) {
            let others = || {
                fuxix::process::tree()
                    .pids
                    .into_iter()
                    .filter(move |pid| *pid != leader)
            };
            let asked = others()
                .filter(|pid| fuxix::process::kill(*pid, Signal::Term).is_ok())
                .count();
            let until = after(Instant::now(), TERM_GRACE);
            while asked > 0 && Instant::now() < until && others().next().is_some() {
                fuxix::process::reap_orphans(|pid| pid == leader);
                std::thread::sleep(Duration::from_millis(5));
            }
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
        // What is kept for a second kill is not reaped as an orphan.
        let kept = self.job.as_ref().map(Job::leader);
        fuxix::process::reap_orphans(|pid| Some(pid) == kept);
        if missed.is_empty() {
            match self.remove() {
                Ok(()) => return true,
                Err(failure) => missed.push(failure.0),
            }
        }
        self.state = format!("kill-incomplete {}", missed.join("; "));
        self.refused = self.refused.saturating_add(1);
        self.exit = None;
        self.advance();
        let why = self.state.clone();
        self.refuse(&format!("{} kill", self.commands), why);
        false
    }

    /// Takes the pane apart. Under the session's lock only its name goes,
    /// by one rename, so the name is free at once and nothing waits on a
    /// disk under the lock; then the publisher writes what it has and
    /// stops, and the directory is emptied and removed. A holder that
    /// dies in between leaves a nameless directory the next clearing
    /// takes. A rename refused leaves the pane whole, to say so.
    fn remove(&mut self) -> Result<(), Failure> {
        let listed = {
            let lock = self.session.lock()?;
            session::unname(&self.dir, &self.id, &lock)?
        };
        self.publisher.stop();
        if let (true, Some((parent, name))) = (session::clear_pane(&self.dir, true), listed) {
            let _ = at::remove_dir(&parent, &name);
        }
        // What this kill ended outright (a holder inside it that would
        // not end itself) is dead now: its directory goes with this one.
        let _ = self.session.live();
        Ok(())
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

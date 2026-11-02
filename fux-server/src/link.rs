//! What the server and a pane's own process say to each other (`host`):
//! one ordered stream each way on one Unix socket, in fux-wire's frames.
//! It is fux's own, between two processes of one binary, and changes with
//! it; no client speaks it.
//!
//! The pane tells its news in order (`FromPane`): each event as it
//! happens, with the counts it left the pane at; its screen, a frame at a
//! time and only once the server took the last (`Taken`); answers to what
//! it was asked. The server tells it its size, its host's terminal, keys
//! typed at a client, and asks it what only it can do (`Ask`).
use crate::journal::Told;
use crate::pane::Said;
use fux_vt::bytes::ByteQueue;
use fux_vt::keys::Keystroke;
use fux_vt::keys::colour::Scheme;
use fux_vt::keys::mouse::MouseEvent;
use fux_vt::{Graphics, Rgb, Size};
use fux_wire::answer::Captured;
use fux_wire::code::Code;
use fux_wire::command::{
    CaptureView, Counts, HistoryBytes, Input, Mouse, PaneOptions, Program, Run, Version,
};
use fux_wire::events::Actor;
use fux_wire::id::{ClientId, PaneId};
use fux_wire::inspect::Facts;
use fux_wire::protocol::{Decoder, Frame};
use fux_wire::wire::{Reader, Wire};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// What a pane starts as: its id in its server's run, the program it runs
/// first (a shell, if `shell`), where and at what size, its options, its
/// host's terminal as it starts, and what its programs are told of their
/// server (`FUX_SOCKET`, `FUX`): with no socket, it starts no program, as a
/// session that only holds state (the tests') has none.
#[derive(Clone, Debug)]
pub struct Begin {
    pub id: PaneId,
    pub run: Run,
    pub shell: bool,
    pub argv: Program,
    pub cwd: PathBuf,
    pub size: Size,
    pub options: PaneOptions,
    pub hosting: Hosting,
    pub socket: Option<PathBuf>,
    pub exe: Option<PathBuf>,
}
wired!(struct Begin { id, run, shell, argv, cwd, size, options, hosting, socket, exe });

/// What a pane's host's terminal said of itself (`outer`): its colours, its
/// palette, and what it draws.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hosting {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    /// Its scheme, once no change of it waits to be told (`outer`).
    pub scheme: Option<Scheme>,
    pub palette: Vec<Option<Rgb>>,
    pub draws: Draws,
}
wired!(struct Hosting { foreground, background, scheme, palette, draws });

/// `fux_vt::Graphics`, on the wire: a cell's width and height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Draws {
    #[default]
    Text,
    Cell(u16, u16),
    Sixel(u16, u16),
}
wired!(Draws { Text, Cell(w, h), Sixel(w, h) });

impl From<Graphics> for Draws {
    fn from(graphics: Graphics) -> Draws {
        match graphics {
            Graphics::Text => Draws::Text,
            Graphics::Cell(c) => Draws::Cell(c.width(), c.height()),
            Graphics::Sixel(c) => Draws::Sixel(c.width(), c.height()),
        }
    }
}

impl From<Draws> for Graphics {
    fn from(draws: Draws) -> Graphics {
        let cell = |w, h| fux_vt::CellSize::new(w, h);
        match draws {
            Draws::Cell(w, h) => cell(w, h).map_or(Graphics::Text, Graphics::Cell),
            Draws::Sixel(w, h) => cell(w, h).map_or(Graphics::Text, Graphics::Sixel),
            Draws::Text => Graphics::Text,
        }
    }
}

impl Hosting {
    pub fn colours(&self) -> fux_vt::keys::colour::Colours {
        fux_vt::keys::colour::Colours {
            foreground: self.foreground,
            background: self.background,
            scheme: self.scheme,
        }
    }
    pub fn palette(&self) -> crate::outer::Palette {
        let mut palette = crate::outer::Palette::default();
        for (to, from) in palette.iter_mut().zip(&self.palette) {
            *to = *from;
        }
        palette
    }
}

/// What a client typed into the pane it focuses: encoded by the pane as its
/// program asked.
#[derive(Clone, Debug)]
pub enum Typed {
    Key(Keystroke),
    Paste(String),
    Focus(bool),
    Mouse(MouseEvent),
}
wired!(Typed { Key(k), Paste(t), Focus(g), Mouse(m) });

/// What the server asks of a pane, answered by number (`FromPane::Answer`).
#[derive(Clone, Debug)]
pub enum Ask {
    /// Keys for its program, if it is still at `seen`.
    Keys {
        inputs: Vec<Input>,
        seen: Option<Version>,
    },
    Mouse {
        mouse: Mouse,
        row: u16,
        col: u16,
    },
    Signal(fuxix::process::Signal),
    Respawn {
        kill: bool,
        argv: Option<Program>,
    },
    ClearHistory,
    HistoryBytes(HistoryBytes),
    /// A capture, its observation's place and name left for the server:
    /// it is in tab `tab` as it is asked.
    Capture {
        history: Option<usize>,
        view: CaptureView,
        tab: fux_wire::id::TabId,
    },
}
wired!(Ask {
    Keys { inputs, seen },
    Mouse { mouse, row, col },
    Signal(s),
    Respawn { kill, argv },
    ClearHistory,
    HistoryBytes(b),
    Capture { history, view, tab },
});

/// What a pane answers: the version it acted at, nothing, a capture, or
/// its refusal.
#[derive(Clone, Debug)]
pub enum Answered {
    Acted(Version),
    Done,
    Captured(Box<Captured>),
    Refused(Refusal),
}
wired!(Answered { Acted(v), Done, Captured(c), Refused(r) });

/// A pane's refusal, with its code.
#[derive(Clone, Debug)]
pub struct Refusal {
    pub code: Code,
    pub message: String,
}
wired!(struct Refusal { code, message });

impl From<fux_wire::code::Error> for Refusal {
    fn from(error: fux_wire::code::Error) -> Refusal {
        Refusal {
            code: error.code(),
            message: error.to_string(),
        }
    }
}

/// From the server to a pane's process.
#[derive(Clone, Debug)]
pub enum ToPane {
    /// The first word: what it starts as.
    Begin(Box<Begin>),
    Resize { size: Size, actor: Actor },
    Host(Hosting),
    /// Whether a client shows it: what it let go of is told only then.
    Shown(bool),
    Typed { client: ClientId, typed: Typed },
    Ask { ask: u64, actor: Actor, what: Ask },
    /// The last screen it sent was taken: it may send the next.
    Taken,
    /// It is closed, by fux if `closed` (`kill`): its program is hung up,
    /// and it ends once that is gone.
    Close { closed: bool },
}
wired!(ToPane {
    Begin(b),
    Resize { size, actor },
    Host(h),
    Shown(s),
    Typed { client, typed },
    Ask { ask, actor, what },
    Taken,
    Close { closed },
});

/// A pane's screen, as the server keeps a copy of it (`copy`): the bytes
/// that paint its visible screen onto the copy's, as it was last sent, or
/// whole if `full`; and its facts as they are with it.
#[derive(Clone, Debug)]
pub struct Update {
    pub counts: Counts,
    pub title: Said,
    pub size: Size,
    pub facts: Facts,
    pub pid: Option<i32>,
    pub screen: Vec<u8>,
    pub full: bool,
    /// What it draws, as its host said: its pictures go on the copy in
    /// its cells, and are kept to `picture_bytes`.
    pub draws: Draws,
    pub picture_bytes: usize,
}
wired!(struct Update { counts, title, size, facts, pid, screen, full, draws, picture_bytes });

/// From a pane's process to the server, in the order it happened.
#[derive(Clone, Debug)]
pub enum FromPane {
    /// It started, its program too, or why not: its first word.
    Begun(Option<String>),
    /// It told this, as `actor`, which left it at `counts`; with the last
    /// of what it told at once, the facts that left it with.
    Told {
        told: Told,
        actor: Actor,
        counts: Counts,
        facts: Option<Box<Facts>>,
    },
    Update(Box<Update>),
    /// Its program rang the bell, or sent a notification.
    Alert {
        bell: bool,
        notified: Option<Said>,
    },
    /// Its program copied this (OSC 52), for the clients showing it.
    Copied(String),
    /// What the clients showing it are told.
    Notice { text: String, error: bool },
    /// What a client typed was refused, so.
    Refused { client: ClientId, why: String },
    Answer { ask: u64, answer: Answered },
}
wired!(FromPane {
    Begun(b),
    Told { told, actor, counts, facts },
    Update(u),
    Alert { bell, notified },
    Copied(t),
    Notice { text, error },
    Refused { client, why },
    Answer { ask, answer },
});

/// A message in one frame, as `Frame` has it.
struct Message<T>(T);

/// The kind of every link frame.
const MESSAGE: u8 = 1;

impl<T: Wire> Frame<'_> for Message<T> {
    fn decode(kind: u8, payload: &[u8]) -> Result<Self, fux_wire::protocol::Error> {
        if kind != MESSAGE {
            return Err(fux_wire::protocol::Error::UnexpectedKind(kind));
        }
        Reader::new(payload)
            .whole()
            .map(Message)
            .map_err(fux_wire::protocol::Error::Wire)
    }
    fn write(&self, out: &mut Vec<u8>) -> u8 {
        self.0.put(out);
        MESSAGE
    }
}

/// A message to send, in one frame.
struct Sending<'a, T>(&'a T);

impl<T: Wire> Frame<'_> for Sending<'_, T> {
    /// Only sent, never read.
    fn decode(kind: u8, _: &[u8]) -> Result<Self, fux_wire::protocol::Error> {
        Err(fux_wire::protocol::Error::UnexpectedKind(kind))
    }
    fn write(&self, out: &mut Vec<u8>) -> u8 {
        self.0.put(out);
        MESSAGE
    }
}

/// One end of a link: its socket, what came that is not yet taken, and
/// what is to go that the socket did not take yet.
pub struct End {
    pub stream: UnixStream,
    decoder: Decoder,
    pub out: ByteQueue,
}

/// How a read of a link went.
#[derive(Debug, PartialEq, Eq)]
pub enum Read {
    /// It read what there was; more may come.
    More,
    /// The other end is gone: what it said before is still to take.
    Closed,
}

impl End {
    /// `stream`, read and written without waiting.
    pub fn new(stream: UnixStream) -> std::io::Result<End> {
        stream.set_nonblocking(true)?;
        Ok(End {
            stream,
            decoder: Decoder::default(),
            out: ByteQueue::default(),
        })
    }

    /// Queues `message`, sent as the socket takes it (`flush`).
    pub fn send(&mut self, message: &impl Wire) {
        let message = Sending(message);
        self.out.push_with(|out| message.encode_into(out));
    }

    /// Writes what is queued as far as the socket takes it: whether all of
    /// it went, or an error once the other end is gone.
    pub fn flush(&mut self) -> Result<bool, fuxix::Errno> {
        while !self.out.is_empty() {
            match fuxix::io::write(&self.stream, self.out.as_slice()) {
                Ok(0) => return Ok(false),
                Ok(n) => self.out.take(n),
                Err(fuxix::Errno::INTR) => {}
                Err(fuxix::Errno::AGAIN) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    /// Writes all it holds, waiting as it must: for a link's first word,
    /// or its last.
    pub fn flush_all(&mut self) -> Result<(), fuxix::Errno> {
        while !self.flush()? {
            let mut poll = fuxix::poll::Poll::default();
            let mut fds = poll.list();
            fds.push(fuxix::poll::PollFd::new(&self.stream, fuxix::poll::Events::OUT));
            match fds.poll(None) {
                Ok(_) | Err(fuxix::Errno::INTR) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Reads what the socket holds, at most `buffer` at a time, until it
    /// would wait.
    pub fn read(&mut self, buffer: &mut [u8]) -> Read {
        loop {
            match fuxix::io::read(&self.stream, buffer) {
                Ok(0) => return Read::Closed,
                Ok(n) => self.decoder.push(buffer.get(..n).unwrap_or_default()),
                Err(fuxix::Errno::INTR) => {}
                Err(fuxix::Errno::AGAIN) => return Read::More,
                Err(_) => return Read::Closed,
            }
        }
    }

    /// The next whole message come, if one is.
    pub fn next<T: Wire>(&mut self) -> Result<Option<T>, fux_wire::protocol::Error> {
        Ok(self.decoder.frame::<Message<T>>()?.map(|m| m.0))
    }

    /// Waits for the next whole message: for a link's first word alone,
    /// from a process just started, as its start waits today
    /// (`process::spawn`). None once the other end is gone first.
    pub fn wait<T: Wire>(&mut self) -> Result<Option<T>, fux_wire::protocol::Error> {
        let mut buffer = [0u8; 4096];
        loop {
            if let Some(message) = self.next()? {
                return Ok(Some(message));
            }
            let mut poll = fuxix::poll::Poll::default();
            let mut fds = poll.list();
            fds.push(fuxix::poll::PollFd::new(
                &self.stream,
                fuxix::poll::Events::IN,
            ));
            match fds.poll(None) {
                Ok(_) | Err(fuxix::Errno::INTR) => {}
                Err(_) => return Ok(None),
            }
            if self.read(&mut buffer) == Read::Closed {
                return self.next();
            }
        }
    }
}

/// A pane's link, as the server holds it: to its own process, or, in a
/// session held in one process (the tests', the tools'), to its host
/// itself, which answers at once.
pub enum Link {
    Remote(Remote),
    Local(Box<crate::host::Host>),
}

/// A pane's own process, linked: its pid, and how it ended once it has,
/// as its link closing and its reaping tell (`Session::pane_gone`).
pub struct Remote {
    pub end: End,
    pub pid: fuxix::process::Pid,
    /// Its link closed.
    pub closed: bool,
    /// It was reaped, ending so.
    pub ended: Option<fuxix::process::Status>,
    /// Its first screen and facts, sent as it began, till its pane takes
    /// them (`Session::linked`).
    pub first: Option<Box<Update>>,
}

impl Link {
    /// Says `message` to it; a host held here answers into `inbox` at once.
    pub fn send(
        &mut self,
        id: PaneId,
        message: ToPane,
        now: std::time::Instant,
        inbox: &mut std::collections::VecDeque<(PaneId, FromPane)>,
    ) {
        match self {
            Link::Remote(remote) => {
                if !remote.closed {
                    remote.end.send(&message);
                }
            }
            Link::Local(host) => {
                host.handle(message, now);
                Link::told(id, host, inbox);
            }
        }
    }

    /// What host `host`, held here, has to tell, its screen too, into
    /// `inbox`.
    pub fn told(
        id: PaneId,
        host: &mut crate::host::Host,
        inbox: &mut std::collections::VecDeque<(PaneId, FromPane)>,
    ) {
        inbox.extend(host.take().into_iter().map(|report| (id, report)));
        if let Some(update) = host.update() {
            inbox.push_back((id, FromPane::Update(Box::new(update))));
        }
    }

    /// Starts pane `begin.id`'s own process from `exe`, `fux __pane`,
    /// linked on its stdin, in a session of its own, and waits for its first
    /// word: why it did not start, if it did not.
    pub fn start(exe: &std::path::Path, begin: Begin) -> Result<Link, String> {
        let (ours, theirs) = UnixStream::pair().map_err(|e| format!("its link: {e}"))?;
        let mut command = std::process::Command::new(exe);
        command
            .arg(PANE)
            .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(theirs)))
            .stdout(std::process::Stdio::null());
        fuxix::process::lead_session(&mut command, false);
        let child = command
            .spawn()
            .map_err(|e| format!("starting its process, {}: {e}", exe.display()))?;
        let pid = i32::try_from(child.id()).ok().and_then(fuxix::process::Pid::from_raw);
        // fux reaps it by its pid; std never waits on a dropped child.
        drop(child);
        let pid = pid.ok_or("its process has no pid")?;
        let mut end = End::new(ours).map_err(|e| format!("its link: {e}"))?;
        end.send(&ToPane::Begin(Box::new(begin)));
        if let Err(error) = end.flush_all() {
            return Err(format!("its process left before it began: {error}"));
        }
        let began = end.wait::<FromPane>();
        let mut remote = Remote {
            end,
            pid,
            closed: false,
            ended: None,
            first: None,
        };
        match began.map_err(|e| format!("its process: {e}"))? {
            Some(FromPane::Begun(None)) => {
                // Its first screen comes with it, so that nothing reads its
                // copy before it has facts.
                match remote.end.wait::<FromPane>() {
                    Ok(Some(FromPane::Update(first))) => remote.first = Some(first),
                    Ok(_) => return Err("its process sent no first screen".to_owned()),
                    Err(error) => return Err(format!("its process: {error}")),
                }
                Ok(Link::Remote(remote))
            }
            Some(FromPane::Begun(Some(why))) => Err(why),
            Some(_) => Err("its process's first word was not its start".to_owned()),
            None => Err("its process ended before it began".to_owned()),
        }
    }
}

/// How the server starts a pane's own process: `fux __pane`, its link on
/// stdin. No help lists it.
pub const PANE: &str = "__pane";

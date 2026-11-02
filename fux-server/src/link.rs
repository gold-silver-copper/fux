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
    /// A paste whose text its client's connection sends the pane itself,
    /// as body `0` (`ToSide::Body`).
    Body(u64),
    Focus(bool),
    Mouse(MouseEvent),
}
wired!(Typed { Key(k), Paste(t), Body(b), Focus(g), Mouse(m) });

/// What the server asks of a pane, answered by number (`FromPane::Answer`).
#[derive(Clone, Debug)]
pub enum Ask {
    /// Keys for its program, if it is still at `seen`: `inputs`, or with a
    /// `body`, those its asker's connection sends it as that body
    /// (`ToSide::Body`), which it waits for.
    Keys {
        inputs: Vec<Input>,
        seen: Option<Version>,
        body: Option<u64>,
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
    /// it is in tab `tab` as it is asked. Held, it is kept for its asker's
    /// connection to fetch (`ToSide::Fetch`), and its counts answered.
    Capture {
        history: Option<usize>,
        view: CaptureView,
        tab: fux_wire::id::TabId,
        hold: bool,
    },
}
wired!(Ask {
    Keys { inputs, seen, body },
    Mouse { mouse, row, col },
    Signal(s),
    Respawn { kill, argv },
    ClearHistory,
    HistoryBytes(b),
    Capture { history, view, tab, hold },
});

/// What a pane answers: the version it acted at, nothing, a capture or the
/// counts of one it holds (`Ask::Capture`), or its refusal.
#[derive(Clone, Debug)]
pub enum Answered {
    Acted(Version),
    Done,
    Captured(Box<Captured>),
    /// A capture it holds as `token`, at `counts`; `cursor`, the journal's
    /// as its server took the answer, is the server's to write.
    Held {
        token: u64,
        counts: Counts,
        cursor: String,
    },
    Refused(Refusal),
}
wired!(Answered { Acted(v), Done, Captured(c), Held { token, counts, cursor }, Refused(r) });

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
    Resize {
        size: Size,
        actor: Actor,
    },
    Host(Hosting),
    /// Whether a client shows it: what it let go of is told only then.
    Shown(bool),
    Typed {
        client: ClientId,
        typed: Typed,
    },
    Ask {
        ask: u64,
        actor: Actor,
        what: Ask,
    },
    /// The last screen it sent was taken: it may send the next.
    Taken,
    /// It is closed, by fux if `closed` (`kill`): its program is hung up,
    /// and it ends once that is gone.
    Close {
        closed: bool,
    },
    /// A server of run `run` takes it back (`Link::take`).
    Take {
        run: Run,
    },
    /// Who it is, asked of one waiting (`Link::who`): it greets, and is
    /// not taken.
    Who,
    /// A connection's process of run `run`, which talks to it from now on
    /// (`ToSide`).
    Join {
        run: Run,
    },
    /// Body or held capture `token` is wanted no more: its asker is gone.
    Drop {
        token: u64,
    },
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
    Take { run },
    Who,
    Join { run },
    Drop { token },
});

/// A pane as the server keeps a copy of it (`copy`): its counts, title,
/// size, facts and program's pid. Its screen goes to the connections
/// showing it, never to the server (`Drawn`).
#[derive(Clone, Debug)]
pub struct Update {
    pub counts: Counts,
    pub title: Said,
    pub size: Size,
    pub facts: Facts,
    pub pid: Option<i32>,
}
wired!(struct Update { counts, title, size, facts, pid });

/// A pane's screen, as a connection's process keeps a copy of it to compose
/// its client's (`conn`): the bytes that paint its visible screen onto the
/// copy, as it was last sent, or whole if `full`; at `size`, drawing what
/// its host draws, its pictures kept to `picture_bytes`.
#[derive(Clone, Debug)]
pub struct Drawn {
    pub size: Size,
    pub screen: Vec<u8>,
    pub full: bool,
    pub draws: Draws,
    pub picture_bytes: usize,
}
wired!(struct Drawn { size, screen, full, draws, picture_bytes });

/// From a client's connection's process to a pane's, which it reached at
/// the pane's socket (D-123), after its first word (`ToPane::Join`).
#[derive(Clone, Debug)]
pub enum ToSide {
    /// Send it the pane's screen, whole first, then as it changes, one
    /// screen at a time (`Taken`).
    Watch,
    /// The last screen it was sent is taken: it may be sent the next.
    Taken,
    /// Body `token`, which a question its server asked waits for: the
    /// connection's client's inputs (`Ask::Keys`, `Typed::Body`).
    Body { token: u64, inputs: Vec<Input> },
    /// The capture held as `token` (`Ask::Capture`), taken.
    Fetch { token: u64 },
}
wired!(ToSide { Watch, Taken, Body { token, inputs }, Fetch { token } });

/// From a pane's process to a connection's that reached it.
#[derive(Clone, Debug)]
pub enum FromSide {
    Drawn(Box<Drawn>),
    /// The capture held as `token`, if it is still held.
    Held {
        token: u64,
        captured: Option<Box<Captured>>,
    },
}
wired!(FromSide { Drawn(d), Held { token, captured } });

/// What the server knows of a pane's capture that its pane does not: its
/// place, name and label as it is captured, and the journal's cursor as
/// the capture was told (`capture::place`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placed {
    pub tab: Option<fux_wire::id::TabId>,
    pub row: Option<u16>,
    pub col: Option<u16>,
    pub name: Option<String>,
    pub label: Option<String>,
    pub cursor: String,
}
wired!(struct Placed { tab, row, col, name, label, cursor });

/// A reply its connection makes itself, to send its client: of a capture
/// pane `pane` holds as `token`, which it fetches and places, or of a client
/// screen it composes from `scene` and the panes' screens.
#[derive(Clone, Debug)]
pub enum Making {
    Capture {
        pane: PaneId,
        token: u64,
        placed: Placed,
    },
    Client {
        client: ClientId,
        scene: Box<crate::render::Scene>,
    },
    /// Its client's body `body` sent `client`'s connection as body
    /// `token`, which types it: answered once `client` answered the read
    /// numbered `asked` sent it after them, so took them
    /// (`ToConn::Typed`), or why it could not (`ToConn::Drop`).
    Keys {
        client: ClientId,
        body: u64,
        token: u64,
        asked: u64,
    },
}
wired!(Making { Capture { pane, token, placed }, Client { client, scene }, Keys { client, body, token, asked } });

/// From a pane's process to the server, in the order it happened.
#[derive(Clone, Debug)]
pub enum FromPane {
    /// It started, its program too, or why not: its first word.
    Begun(Option<String>),
    /// What it is, as a server takes it back: its first word then.
    Greeting(Box<Greeting>),
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
    Notice {
        text: String,
        error: bool,
    },
    /// What a client typed was refused, so.
    Refused {
        client: ClientId,
        why: String,
    },
    Answer {
        ask: u64,
        answer: Answered,
    },
    /// Body or held capture `0` is used up: taken by what waited for it,
    /// or fetched. Its asker's connection holds it no more.
    Retired(u64),
}
wired!(FromPane {
    Begun(b),
    Greeting(g),
    Told { told, actor, counts, facts },
    Update(u),
    Alert { bell, notified },
    Copied(t),
    Notice { text, error },
    Refused { client, why },
    Answer { ask, answer },
    Retired(t),
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
            fds.push(fuxix::poll::PollFd::new(
                &self.stream,
                fuxix::poll::Events::OUT,
            ));
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
        self.read_up_to(buffer, usize::MAX)
    }

    /// `read`, till `limit` bytes are read, the rest left for the next.
    pub fn read_up_to(&mut self, buffer: &mut [u8], limit: usize) -> Read {
        let mut read = 0usize;
        while read < limit {
            match fuxix::io::read(&self.stream, buffer) {
                Ok(0) => return Read::Closed,
                Ok(n) => {
                    read = read.saturating_add(n);
                    self.decoder.push(buffer.get(..n).unwrap_or_default());
                }
                Err(fuxix::Errno::INTR) => {}
                Err(fuxix::Errno::AGAIN) => return Read::More,
                Err(_) => return Read::Closed,
            }
        }
        Read::More
    }

    /// The messages queued to go, as the other end will read them: for the
    /// tests.
    #[cfg(test)]
    pub fn queued<T: Wire>(&self) -> Vec<T> {
        let mut decoder = Decoder::default();
        decoder.push(self.out.as_slice());
        let mut queued = Vec::new();
        while let Ok(Some(message)) = decoder.frame::<Message<T>>() {
            queued.push(message.0);
        }
        queued
    }

    /// The next whole message come, if one is.
    pub fn next_message<T: Wire>(&mut self) -> Result<Option<T>, fux_wire::protocol::Error> {
        Ok(self.decoder.frame::<Message<T>>()?.map(|m| m.0))
    }

    /// Waits for the next whole message: for a link's first word alone,
    /// from a process just started, as its start waits today
    /// (`process::spawn`). None once the other end is gone first.
    pub fn wait<T: Wire>(&mut self) -> Result<Option<T>, fux_wire::protocol::Error> {
        let mut buffer = [0u8; 4096];
        loop {
            if let Some(message) = self.next_message()? {
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
                return self.next_message();
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
    /// It is this server's child, which reaps it; one taken back from a
    /// server that crashed is not (`Link::take`).
    pub child: bool,
    /// Its link closed.
    pub closed: bool,
    /// It was reaped, ending so.
    pub ended: Option<fuxix::process::Status>,
    /// What it said as it began or was taken back, its first facts last,
    /// till its pane takes them (`Session::linked`).
    pub first: Vec<FromPane>,
    /// What says how it is to be now (its size, host, whether it is shown),
    /// the latest of each, while its link takes nothing more: sent once it
    /// does, so that one that reads nothing (stopped) grows no queue here.
    pub later: Vec<ToPane>,
    /// Its link took none of what was last written to it: its process reads
    /// nothing (stopped, say). What would go to it waits upstream then.
    pub stalled: bool,
}

impl Remote {
    /// Writes what is queued as far as its link takes it, and once it took
    /// it all, what waited till then (`later`).
    pub fn flush(&mut self) {
        let queued = self.end.out.len();
        let all = matches!(self.end.flush(), Ok(true));
        self.stalled = queued > 0 && self.end.out.len() == queued;
        if !all {
            return;
        }
        for message in std::mem::take(&mut self.later) {
            self.end.send(&message);
        }
        let _ = self.end.flush();
    }
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
            Link::Remote(remote) if remote.closed => {}
            Link::Remote(remote) => {
                let state = matches!(
                    message,
                    ToPane::Resize { .. } | ToPane::Host(_) | ToPane::Shown(_)
                );
                if state && !remote.end.out.is_empty() {
                    let kind = std::mem::discriminant(&message);
                    remote.later.retain(|m| std::mem::discriminant(m) != kind);
                    remote.later.push(message);
                } else {
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
    /// listening at `socket` on its stdin, where its server reaches it and
    /// the next one would if this one crashed (D-124), in a session of its
    /// own; and waits for its first word: why it did not start, if it did
    /// not.
    pub fn start(
        exe: &std::path::Path,
        socket: &std::path::Path,
        begin: Begin,
    ) -> Result<Link, String> {
        let at = |doing: &str| {
            let socket = socket.display().to_string();
            let doing = doing.to_owned();
            move |e: std::io::Error| format!("{doing} {socket}: {e}")
        };
        // One a process still listens at is a pane's, whatever the record
        // says: never let go as debris. One no one does was left by a pane
        // that ended.
        if UnixStream::connect(socket).is_ok() {
            return Err(format!(
                "a pane's process listens at {} already",
                socket.display()
            ));
        }
        let _ = std::fs::remove_file(socket);
        let listener =
            std::os::unix::net::UnixListener::bind(socket).map_err(at("binding its socket"))?;
        std::fs::set_permissions(socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .map_err(at("its socket"))?;
        let mut command = std::process::Command::new(exe);
        command
            .arg(PANE)
            .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
                listener,
            )))
            .stdout(std::process::Stdio::null());
        fuxix::process::lead_session(&mut command, false);
        let child = command
            .spawn()
            .map_err(|e| format!("starting its process, {}: {e}", exe.display()))?;
        let pid = i32::try_from(child.id())
            .ok()
            .and_then(fuxix::process::Pid::from_raw);
        // fux reaps it by its pid; std never waits on a dropped child.
        drop(child);
        let pid = pid.ok_or("its process has no pid")?;
        let stream = UnixStream::connect(socket).map_err(at("reaching its process at"))?;
        let mut end = End::new(stream).map_err(|e| format!("its link: {e}"))?;
        end.send(&ToPane::Begin(Box::new(begin)));
        if let Err(error) = end.flush_all() {
            return Err(format!("its process left before it began: {error}"));
        }
        let began = end.wait::<FromPane>();
        let mut remote = Remote {
            end,
            pid,
            child: true,
            closed: false,
            ended: None,
            first: Vec::new(),
            later: Vec::new(),
            stalled: false,
        };
        match began.map_err(|e| format!("its process: {e}"))? {
            Some(FromPane::Begun(None)) => {
                // Its first facts come with it, so that nothing reads its
                // copy before it has them.
                remote.first = first(&mut remote.end)?;
                Ok(Link::Remote(remote))
            }
            Some(FromPane::Begun(Some(why))) => Err(why),
            Some(_) => Err("its process's first word was not its start".to_owned()),
            None => Err("its process ended before it began".to_owned()),
        }
    }

    /// Takes back the pane whose process listens at `socket`, waiting for
    /// a server since its own crashed (D-124): what it says it is, if it is
    /// a pane of run `run` and speaks this pane protocol, and its link. One
    /// no process listens for is gone; one that answers but can't be taken
    /// is not (`Untaken`).
    pub fn take(socket: &std::path::Path, run: Run) -> Result<(Link, Greeting), Untaken> {
        let stream = UnixStream::connect(socket).map_err(|e| Untaken::Gone(e.to_string()))?;
        let unknown = |why: String| Untaken::Unknown(why);
        let mut end = End::new(stream).map_err(|e| unknown(format!("its link: {e}")))?;
        end.send(&ToPane::Take { run });
        end.flush_all()
            .map_err(|e| unknown(format!("its link: {e}")))?;
        let greeting = match end.wait::<FromPane>() {
            Ok(Some(FromPane::Greeting(greeting))) => greeting,
            Ok(Some(_)) => return Err(unknown("its first word was no greeting".to_owned())),
            Ok(None) => return Err(unknown("it is no pane of this run".to_owned())),
            Err(error) => return Err(unknown(format!("its link: {error}"))),
        };
        if greeting.protocol != PANE_PROTOCOL {
            return Err(unknown(format!(
                "it speaks pane protocol {}, not {PANE_PROTOCOL}",
                greeting.protocol
            )));
        }
        let pid = fuxix::process::Pid::from_raw(greeting.pid)
            .ok_or_else(|| unknown(format!("its greeting names pid {}", greeting.pid)))?;
        let mut remote = Remote {
            end,
            pid,
            child: false,
            closed: false,
            ended: None,
            first: Vec::new(),
            later: Vec::new(),
            stalled: false,
        };
        remote.first = first(&mut remote.end).map_err(unknown)?;
        Ok((Link::Remote(remote), *greeting))
    }

    /// The pid of the pane process waiting at `socket`, as the system says
    /// (`SO_PEERCRED`), else as it says itself; none if none answers.
    pub fn who(socket: &std::path::Path) -> Option<i32> {
        let stream = UnixStream::connect(socket).ok()?;
        if let Some(pid) = fuxix::socket::peer(&stream).ok().and_then(|peer| peer.pid) {
            return Some(pid.as_raw());
        }
        let mut end = End::new(stream).ok()?;
        end.send(&ToPane::Who);
        end.flush_all().ok()?;
        match end.wait::<FromPane>() {
            Ok(Some(FromPane::Greeting(greeting))) => Some(greeting.pid),
            Ok(_) | Err(_) => None,
        }
    }
}

/// Why a pane waiting for a server was not taken back.
#[derive(Debug)]
pub enum Untaken {
    /// No process listens at its socket: it ended.
    Gone(String),
    /// One does, and could not be taken: it may still run.
    Unknown(String),
}

impl std::fmt::Display for Untaken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Untaken::Gone(why) => write!(f, "gone: {why}"),
            Untaken::Unknown(why) => f.write_str(why),
        }
    }
}

/// What a pane says as it begins or is taken back, up to its first facts,
/// which it sends once a frame it is part way through is whole: what it
/// told before them too, in order.
fn first(end: &mut End) -> Result<Vec<FromPane>, String> {
    let mut said = Vec::new();
    loop {
        match end.wait::<FromPane>() {
            Ok(Some(update @ FromPane::Update(_))) => {
                said.push(update);
                return Ok(said);
            }
            Ok(Some(other)) => said.push(other),
            Ok(None) => return Err("its process ended before its first facts".to_owned()),
            Err(error) => return Err(format!("its process: {error}")),
        }
    }
}

/// The messages between a server and its panes' processes: a server takes
/// back only panes of its own (`Link::take`).
pub const PANE_PROTOCOL: u32 = 1;

/// What a pane waiting for a server says of itself as one takes it back:
/// enough to make its copy again, its screen and facts coming after.
#[derive(Clone, Debug)]
pub struct Greeting {
    pub protocol: u32,
    pub id: PaneId,
    /// Its own process's pid, which a system that names no peer's (macOS)
    /// tells no other way.
    pub pid: i32,
    pub run: Run,
    pub respawns: u64,
    pub shell: bool,
    pub argv: Program,
    pub cwd: PathBuf,
    pub options: PaneOptions,
    pub size: Size,
    pub title: Said,
    pub counts: Counts,
    /// How its program ended, if it has; else whether it hung up, or
    /// reading its terminal failed.
    pub ended: Option<fux_wire::answer::Ended>,
    pub hung_up: bool,
    pub read_error: Option<String>,
}
wired!(struct Greeting {
    protocol, id, pid, run, respawns, shell, argv, cwd, options, size, title, counts, ended, hung_up,
    read_error,
});

/// How the server starts a pane's own process: `fux __pane`, its link on
/// stdin. No help lists it.
pub const PANE: &str = "__pane";

/// From the server to a client's connection's own process (D-123, `conn`).
#[derive(Debug)]
pub enum ToConn {
    /// Bytes for its client: frames, as the client protocol has them.
    Bytes(Vec<u8>),
    /// What its client is to send next: the frames it decodes.
    Expect(Expect),
    /// The answer to its client's request is all sent: once the client
    /// has it, its next request is read.
    Answered,
    /// Its first word: where its run's panes listen, and the run, which
    /// it tells each pane it reaches (`ToPane::Join`).
    Hello { panes: PathBuf, run: Run },
    /// What its attached client's screen is made of now but its panes'
    /// screens, which it gets from the panes (`render::Scene`).
    Scene(Box<crate::render::Scene>),
    /// Its client's body `body` (`FromConn::Frame`) goes to `panes`, as
    /// their server's `token`.
    Deliver {
        body: u64,
        token: u64,
        panes: Vec<PaneId>,
    },
    /// Its client is attached as `client`: it listens for bodies other
    /// connections send it, at `SOCKET.panes/cN` (`ToSide::Body`).
    Listen { client: ClientId },
    /// Its client is sent body `token`'s keys, as typed at its terminal
    /// (`ToAttached::Keys`), once the body comes: what follows waits.
    Keys { token: u64 },
    /// Body `token` goes no further: its sender is gone, or the client it
    /// was for. Taken as it comes, before what waits for it.
    Drop { token: u64 },
    /// Body `token`, its client's keys, was taken by the client it was
    /// for, still attached (`Making::Keys`).
    Typed { token: u64 },
    /// The answer to its client's request, which it makes itself, as its
    /// client is to have it: its last word once made.
    Make(Box<Making>),
    /// Its client is detached: its terminal is given back its own cursor
    /// shape and configured colour, where they were set.
    Detached,
    /// Its last word is sent: it ends once its client has it all.
    Close,
}
wired!(ToConn {
    Bytes(b),
    Expect(e),
    Answered,
    Hello { panes, run },
    Scene(s),
    Deliver { body, token, panes },
    Listen { client },
    Keys { token },
    Drop { token },
    Typed { token },
    Make(m),
    Detached,
    Close
});

/// The frames a connection's client sends next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Attach,
    Asked,
    Attached,
}
wired!(Expect {
    Attach,
    Asked,
    Attached
});

/// From a client's connection's own process to the server.
#[derive(Debug)]
pub enum FromConn {
    /// A whole frame its client sent, decoded as the frame expected: sent
    /// again as it came, for the server to take, but for its inputs or
    /// paste text, which it keeps as `body` (`ToConn::Deliver`).
    Frame { frame: Vec<u8>, body: Option<u64> },
    /// Everything sent for its client was written to it.
    Flushed,
    /// Its client shut its side: it sends no more.
    Shut,
    /// Its client sent what is no frame of those expected: why. It reads
    /// it no more.
    Bad(String),
}
wired!(FromConn { Frame { frame, body }, Flushed, Shut, Bad(why) });

/// How the server starts a client's connection's own process: `fux
/// __conn`, the client's socket on stdin, its link on stdout.
pub const CONN: &str = "__conn";

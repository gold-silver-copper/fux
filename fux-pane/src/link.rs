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
use crate::pane::Said;
use crate::socket::Stream;
use crate::told::Told;
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
use fux_wire::protocol::{Decoder, Frame, Joining, MAX_PAYLOAD};
use fux_wire::wire::{Reader, Wire};
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
    /// The pid of the server that begins it, which it names to another
    /// that asks for it while this one holds it (`Taking::Held`).
    pub server: i32,
}
wired!(struct Begin { id, run, shell, argv, cwd, size, options, hosting, socket, exe, server });

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
    pub fn palette(&self) -> crate::pane::Palette {
        let mut palette = crate::pane::Palette::default();
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
    /// in pieces, as body `0` (`ToSide::Body`).
    Body(u64),
    Focus(bool),
    Mouse(MouseEvent),
}
wired!(Typed { Key(k), Paste(t), Body(b), Focus(g), Mouse(m) });

/// What the server asks of a pane, answered by number (`FromPane::Answer`).
#[derive(Clone, Debug)]
pub enum Ask {
    /// Keys for its program, if it is still at `seen`: `inputs`, or with a
    /// `body`, those its asker's connection sends it, in pieces, as that
    /// body (`ToSide::Body`), which it waits for.
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
    /// The server with pid `server`, of run `run`, takes it back
    /// (`Asked`): answered in words, always (`Taking`).
    Take {
        run: Run,
        server: i32,
    },
    /// Who it is, asked of one waiting: it greets, and is not taken.
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
    Take { run, server },
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
    /// A piece of body `token`, which a question its server asked waits
    /// for (`Ask::Keys`, `Typed::Body`): the connection's client's inputs,
    /// one frame of them, on a link of its own. The next piece, which
    /// follows one ending in `Input::More`, is sent once this one is taken
    /// (`FromSide::Took`): a body stays with its sender, a piece at a time.
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
    /// The piece of body `token` last sent is taken: by a pane, its
    /// program's terminal took it all, or it is refused and let go; by a
    /// client's connection, its client took it. Not that anything read it.
    Took {
        token: u64,
    },
}
wired!(FromSide { Drawn(d), Held { token, captured }, Took { token } });

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

/// From a pane's process to the server, in the order it happened.
#[derive(Clone, Debug)]
pub enum FromPane {
    /// It started, its program too, or why not: its first word.
    Begun(Option<String>),
    /// Its answer to a server that reached its socket, to take it, to ask
    /// who it is or to end it: its first word then.
    Taking(Taking),
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
    /// Its program's handle, for its server to copy (D-134): sent on
    /// Windows alone, before the facts naming a new program, and again to
    /// a server that takes it back.
    Offer(Offered),
}

/// A pane's program as its process offers its server a copy of its
/// handle: who the program is (its pid and creation time) and the
/// handle's value in the pane's process (`fuxix::process::Offer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offered {
    pub pid: i32,
    pub created: u64,
    pub handle: u64,
}
wired!(struct Offered { pid, created, handle });

impl From<fuxix::process::Offer> for Offered {
    fn from(offer: fuxix::process::Offer) -> Offered {
        Offered {
            pid: offer.program.pid.as_i32(),
            created: offer.program.created,
            handle: offer.handle,
        }
    }
}

impl Offered {
    /// It as fuxix takes it; none if its pid is none.
    pub fn offer(self) -> Option<fuxix::process::Offer> {
        Some(fuxix::process::Offer {
            program: fuxix::process::Identity {
                pid: fuxix::process::Pid::from_i32(self.pid)?,
                created: self.created,
            },
            handle: self.handle,
        })
    }
}
wired!(FromPane {
    Begun(b),
    Taking(t),
    Told { told, actor, counts, facts },
    Update(u),
    Alert { bell, notified },
    Copied(t),
    Notice { text, error },
    Refused { client, why },
    Answer { ask, answer },
    Retired(t),
    Offer(o),
});

/// What a pane's process answers a server that reaches its socket: to one
/// that takes it back (`ToPane::Take`), asks who it is (`ToPane::Who`) or
/// ends it (`ToPane::Close` as a first word). Always in words: a
/// connection it closes without one means its process ended, never that it
/// would not say.
#[derive(Clone, Debug)]
pub enum Taking {
    /// What it is: taken by the one that asked to take it, its link from
    /// now on.
    Greets(Box<Greeting>),
    /// It has not begun: no server's first word reached it. It waits for
    /// one (`ToPane::Begin`), from any server of its run.
    Unbegun { pid: i32 },
    /// It is closed, and ends once its program is gone; one asked to end
    /// says so too.
    Closing,
    /// It is a pane of another run.
    OtherRun,
    /// The server with pid `server` still holds its link, read to its end
    /// before this was said.
    Held { server: i32 },
}
wired!(Taking { Greets(g), Unbegun { pid }, Closing, OtherRun, Held { server } });

/// A message in one frame, as `Frame` has it.
pub struct Message<T>(pub T);

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
    fn write(&self, out: &mut impl fux_wire::wire::Sink) -> u8 {
        self.0.put(out);
        MESSAGE
    }
}

/// A message to send, in one frame.
pub struct Sending<'a, T>(pub &'a T);

impl<T: Wire> Frame<'_> for Sending<'_, T> {
    /// Only sent, never read.
    fn decode(kind: u8, _: &[u8]) -> Result<Self, fux_wire::protocol::Error> {
        Err(fux_wire::protocol::Error::UnexpectedKind(kind))
    }
    fn write(&self, out: &mut impl fux_wire::wire::Sink) -> u8 {
        self.0.put(out);
        MESSAGE
    }
}

/// The most bytes for a client the server sends its connection's process
/// in one message (`ToConn::Bytes`), which is then one frame: what is left
/// of a frame's payload is far more than the message's own few bytes.
pub const BYTES: usize = MAX_PAYLOAD.saturating_sub(fux_wire::protocol::ENVELOPE);

/// The most a pane's launch, its program's words and its directory, may
/// take in its reports: a quarter of a frame. Its reports repeat its
/// launch (its facts, its greeting), and the server joins none, so each is
/// one frame by what it is made of: its launch, and what its program said,
/// each no more than fux-vt keeps of a string (`OSC_PAYLOAD_LIMIT`, made
/// text at most three times that), a title or a notification told and
/// again among its facts.
pub const LAUNCH: usize = MAX_PAYLOAD / 4;

/// Whether a pane may run `argv` in `cwd`: they take no more than `LAUNCH`
/// in its reports, as its facts say them.
pub fn launch_fits(
    argv: &fux_wire::command::Program,
    cwd: &std::path::Path,
) -> Result<(), fux_wire::code::Error> {
    let words = fux_wire::json::Json::Array(argv.iter().map(fux_wire::json::Json::str).collect());
    let bytes = (fux_wire::wire::encode(&words).len()).saturating_add(cwd.as_os_str().len());
    if bytes <= LAUNCH {
        return Ok(());
    }
    Err(fux_wire::code::fail(
        Code::TooLong,
        format!(
            "a program's words and directory of {bytes} bytes are more than a pane may be \
             started with ({LAUNCH} bytes): its reports to the server, one frame each, repeat them"
        ),
    ))
}

/// One end of a link: its socket, what came that is not yet taken, one
/// frame of it at most (`next_message`), and what is to go that the socket
/// did not take yet.
pub struct End {
    pub stream: Stream,
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
    pub fn new(stream: Stream) -> std::io::Result<End> {
        crate::socket::nonblocking(&stream)?;
        Ok(End {
            stream,
            decoder: Decoder::default(),
            out: ByteQueue::default(),
        })
    }

    /// Queues `message`, sent as the socket takes it (`flush`). One longer
    /// than a frame goes in pieces, which only an end that asked for
    /// something long joins (`next_long`); any other refuses the first.
    pub fn send(&mut self, message: &impl Wire) {
        let message = Sending(message);
        self.out.push_with(|out| message.encode_into(out));
    }

    /// Queues `message` if it is one frame, as the server takes them:
    /// whether it was queued.
    pub fn send_whole(&mut self, message: &impl Wire) -> bool {
        let message = Sending(message);
        self.out.push_with(|out| {
            let start = out.len();
            message.encode_into(out);
            let whole = out.len().saturating_sub(start) <= MAX_PAYLOAD.saturating_add(5);
            if !whole {
                out.truncate(start);
            }
            whole
        })
    }

    /// Writes what is queued as far as the socket takes it: whether all of
    /// it went, or an error once the other end is gone.
    pub fn flush(&mut self) -> Result<bool, fuxix::Errno> {
        let written = fuxix::io::write_what_fits(&self.stream, self.out.as_slice())?;
        self.out.take(written);
        Ok(self.out.is_empty())
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
    #[cfg(any(test, feature = "testing"))]
    pub fn queued<T: Wire>(&self) -> Vec<T> {
        let (mut decoder, mut joining) = (Decoder::default(), Joining::default());
        decoder.push(self.out.as_slice());
        let mut queued = Vec::new();
        while let Ok(Some(message)) = decoder.long::<Message<T>>(&mut joining) {
            queued.push(message.0);
        }
        queued
    }

    /// The next whole message come, if one is: one frame. A piece of a
    /// longer one is an error, and the link is then unusable.
    pub fn next_message<T: Wire>(&mut self) -> Result<Option<T>, fux_wire::protocol::Error> {
        Ok(self.decoder.frame::<Message<T>>()?.map(|m| m.0))
    }

    /// `next_message`, for what this end asked for and may be long (a
    /// pane's screen or capture) and, for now, a paste's body: its pieces
    /// joined into `joining`, as far as the OS gives memory.
    pub fn next_long<T: Wire>(
        &mut self,
        joining: &mut Joining,
    ) -> Result<Option<T>, fux_wire::protocol::Error> {
        Ok(self.decoder.long::<Message<T>>(joining)?.map(|m| m.0))
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

/// The messages between a server and its panes' processes: a server takes
/// back only panes of its own (`takeover`).
pub const PANE_PROTOCOL: u32 = 2;

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

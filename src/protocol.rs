//! The private protocol between a `fux` client and its server.
//!
//! A frame is `u32 length | u8 kind | payload`, big-endian, where `length`
//! counts the kind byte and the payload. A frame is at most `MAX_FRAME`; a
//! longer paint or output is split across frames by the sender.
//!
//! Each frame is decoded once, as the type of frame allowed where it
//! arrives: by a server, a `Hello`, then an `Attach` and `AttachedFrame`s,
//! or one `Command`; by a client, `ServerFrame`s. Its bytes are lent from
//! the decoder. Any other kind, or a size of zero, does not decode.

use crate::act::Act;
use crate::bytes::ByteQueue;
use crate::code::Code;
use crate::command::{AnyRef, Command};
use crate::id::{ClientId, PaneId};
use crate::wire::{Wire, wire_enum, wire_struct};
use std::num::NonZeroU8;

/// Bumped on any change to the frames below.
pub const PROTOCOL: u32 = 10;
/// The largest frame, kind byte included.
pub const MAX_FRAME: usize = 1 << 20;
/// The largest payload one frame carries.
pub const MAX_PAYLOAD: usize = MAX_FRAME - 1;
/// Each frame's kind, its byte in the header: named once, for the encoder
/// and the decoder both.
mod kind {
    pub const HELLO: u8 = 1;
    pub const ATTACH: u8 = 2;
    pub const ACT: u8 = 3;
    pub const RESIZE: u8 = 4;
    pub const DETACH: u8 = 5;
    pub const COMMAND: u8 = 6;
    pub const PAINT: u8 = 7;
    pub const EXIT: u8 = 8;
    pub const STDOUT: u8 = 9;
    pub const STDERR: u8 = 10;
    pub const DONE: u8 = 11;
    pub const TERMINAL: u8 = 12;
    pub const REPLY: u8 = 13;
    pub const ENDED: u8 = 14;
    pub const RUN: u8 = 15;
    pub const KEYS: u8 = 16;
    pub const PART: u8 = 17;
    pub const PARTS_END: u8 = 18;
}

/// Why bytes are not a frame, or a frame cannot be sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A frame longer than the limit: one to send, or one a header
    /// announces.
    Oversized { bytes: usize, limit: usize },
    /// A header announcing no bytes, not even the kind.
    Empty,
    /// A kind unknown, or not one the frames decoded here may have.
    UnexpectedKind(u8),
    /// A payload that does not read as its frame's.
    Wire(crate::wire::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Oversized { bytes, limit } => {
                write!(f, "a frame of {bytes} bytes exceeds the {limit}-byte limit")
            }
            Error::Empty => f.write_str("an empty frame"),
            Error::UnexpectedKind(kind) => write!(f, "unexpected frame kind {kind}"),
            Error::Wire(error) => write!(f, "a frame's payload: {error}"),
        }
    }
}

impl std::error::Error for Error {}

/// A client and a server of different protocols, which understand each
/// other's `Hello` and nothing after it but `Kill`: what both sides say of
/// it, the server's protocol and version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub protocol: u32,
    pub version: String,
}

impl Mismatch {
    /// Whether a client that said `client` can go on with a server that
    /// said `server`: either may ask, holding both.
    pub fn between(client: &Hello, server: &Hello) -> Result<(), Mismatch> {
        if client.role == Role::Kill || client.protocol == server.protocol {
            return Ok(());
        }
        Err(Mismatch {
            protocol: server.protocol,
            version: server.version.clone(),
        })
    }
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the server speaks protocol {} (fux {}); restart it: `fux force-kill-server` stops it",
            self.protocol, self.version
        )
    }
}

impl std::error::Error for Mismatch {}

/// What a connecting client is for; its tag is its byte in a `Hello`'s
/// payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// An interactive client: `Attach`, then input.
    Attach,
    /// One command, then its output.
    Command,
    /// Stop the server. Accepted whatever the protocol version, so that
    /// `fux force-kill-server` can always stop a server from another fux version:
    /// for that, a `Hello`'s frame and payload never change.
    Kill,
    /// A client with no terminal (`COLS ROWS headless`): `Attach`, then
    /// nothing but its end; the server sizes it (`resize-client`).
    Headless,
}

/// The frames one side expects at one point of a connection, decoded from
/// a kind and a payload lent for `'a`, and encoded.
pub trait Frame<'a>: Sized {
    /// The frame that `kind` and `payload` make, or why they make none of
    /// these.
    fn decode(kind: u8, payload: &'a [u8]) -> Result<Self, Error>;

    /// Writes the payload to `out`; the kind byte.
    fn write(&self, out: &mut Vec<u8>) -> u8;

    /// The encoded frame, or an error if its payload exceeds `MAX_PAYLOAD`.
    fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        self.encode_into(&mut out)?;
        Ok(out)
    }

    /// Appends the encoded frame to `out`. A payload over `MAX_PAYLOAD` is
    /// an error, and leaves `out` as it was.
    fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let start = out.len();
        let payload = put(self, out);
        if payload > MAX_PAYLOAD {
            out.truncate(start);
            return Err(Error::Oversized {
                bytes: payload,
                limit: MAX_PAYLOAD,
            });
        }
        Ok(())
    }
}

/// Appends `frame` to `out`, whatever its length: room for the header,
/// then the payload, written in place, and the header filled in once its
/// length is known, exact for a payload up to `MAX_PAYLOAD`. The payload's
/// length.
fn put<'a>(frame: &impl Frame<'a>, out: &mut Vec<u8>) -> usize {
    let start = out.len();
    out.extend_from_slice(&[0; 5]);
    let kind = frame.write(out);
    // What follows the four length bytes and the kind.
    let payload = out.len().saturating_sub(start).saturating_sub(5);
    let length = u32::try_from(payload.saturating_add(1)).unwrap_or(u32::MAX);
    let [a, b, c, d] = length.to_be_bytes();
    if let Some(header) = out.get_mut(start..).and_then(|f| f.first_chunk_mut::<5>()) {
        *header = [a, b, c, d, kind];
    }
    payload
}

/// Both ways, the first frame from each side: a client's says what it is
/// for, the server's answer the role it took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub protocol: u32,
    pub version: String,
    pub role: Role,
}

wire_enum!(Role, "role" {
    0 => Attach,
    1 => Command,
    2 => Kill,
    3 => Headless,
});

wire_struct!(Hello {
    protocol,
    role,
    version
});

impl Hello {
    /// This build's `Hello`, for `role`.
    pub fn ours(role: Role) -> Hello {
        Hello {
            protocol: PROTOCOL,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            role,
        }
    }
}

impl OneKind for Hello {
    const KIND: u8 = kind::HELLO;
}

/// An attaching client's one frame after its `Hello`: its terminal's size,
/// the workspace it asks for, whether it only watches, and its
/// environment, which panes started from its keys take what
/// `update-environment` names from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attach {
    pub size: fux_vt::Size,
    pub workspace: Option<String>,
    pub read_only: bool,
    pub env: Vec<(String, String)>,
}

wire_struct!(Attach {
    size,
    workspace,
    read_only,
    env
});

impl OneKind for Attach {
    const KIND: u8 = kind::ATTACH;
}

/// An attached client's frames.
#[derive(Clone, Debug, PartialEq)]
pub enum AttachedFrame {
    Act(Box<Act>),
    Resize(fux_vt::Size),
    Detach,
}

impl Frame<'_> for AttachedFrame {
    fn decode(kind: u8, payload: &[u8]) -> Result<Self, Error> {
        match kind {
            kind::ACT => read(payload).map(|act| AttachedFrame::Act(Box::new(act))),
            kind::RESIZE => read(payload).map(AttachedFrame::Resize),
            kind::DETACH => read(payload).map(|()| AttachedFrame::Detach),
            other => Err(Error::UnexpectedKind(other)),
        }
    }

    fn write(&self, out: &mut Vec<u8>) -> u8 {
        match self {
            AttachedFrame::Act(act) => {
                act.put(out);
                kind::ACT
            }
            AttachedFrame::Resize(size) => {
                size.put(out);
                kind::RESIZE
            }
            AttachedFrame::Detach => kind::DETACH,
        }
    }
}

/// Where a request comes from: the caller's directory, its pane
/// (`FUX_PANE`) and the server that pane is of (`FUX_SERVER`), its
/// environment, which panes it starts take what `update-environment` names
/// from, the client whose binding or prompt runs it, and whether it brings
/// back a saved session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    pub cwd: String,
    pub pane: Option<PaneId>,
    pub server: Option<String>,
    pub env: Vec<(String, String)>,
    pub client: Option<ClientId>,
    pub restoring: bool,
}

/// A command client's frames after its `Hello`: requests, each answered
/// before the next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub command: Command,
    pub context: Context,
}

wire_struct!(Context {
    cwd,
    pane,
    server,
    env,
    client,
    restoring
});

wire_struct!(Request { command, context });

impl OneKind for Request {
    const KIND: u8 = kind::COMMAND;
}

/// A field's value, as its JSON has it, if it is a number, a bool or a
/// string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scalar {
    Int(i64),
    Bool(bool),
    Str(String),
}

/// The server's answer to a request: how it ended; what it says as text,
/// and as JSON where it has a JSON form; the panes, tabs or workspaces it
/// made or found; a field's value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    pub end: End,
    pub text: String,
    pub json: String,
    pub ids: Vec<AnyRef>,
    pub value: Option<Scalar>,
}

/// How a request ended: its exit status, and why it failed, if it did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum End {
    #[default]
    Done,
    /// Answered with a status of its own, and the code that names it, no
    /// failure: a wait met by its program's exit (its status), or a pane
    /// said as it stands (4 closed, 7 reading short of the text, 124 still
    /// busy).
    Answered { status: NonZeroU8, code: Code },
    /// Failed, with its status, its code and why.
    Failed {
        status: NonZeroU8,
        code: Code,
        message: String,
    },
}

impl End {
    /// A failure of kind `code`, with its exit status.
    pub fn failed(code: Code, message: impl Into<String>) -> End {
        End::Failed {
            status: code.status(),
            code,
            message: message.into(),
        }
    }

    /// The exit status.
    pub fn status(&self) -> u8 {
        match self {
            End::Done => 0,
            End::Answered { status, .. } | End::Failed { status, .. } => status.get(),
        }
    }

    /// The code that names its status, if it has one.
    pub fn code(&self) -> Option<Code> {
        match self {
            End::Done => None,
            End::Answered { code, .. } | End::Failed { code, .. } => Some(*code),
        }
    }
}

/// An answer of `text` alone.
impl From<String> for Reply {
    fn from(text: String) -> Reply {
        Reply {
            text,
            ..Reply::default()
        }
    }
}

impl Reply {
    /// A failure of kind `code`, and why.
    pub fn failed(code: Code, message: impl Into<String>) -> Reply {
        Reply {
            end: End::failed(code, message),
            ..Reply::default()
        }
    }
}

wire_enum!(Scalar, "scalar" { 0 => Int(n), 1 => Bool(b), 2 => Str(s) });

wire_enum!(End, "end" {
    0 => Done,
    1 => Answered { status, code },
    2 => Failed { status, code, message },
});

wire_struct!(Reply {
    end,
    text,
    json,
    ids,
    value
});

/// The server's frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerFrame<'a> {
    /// Its answer to the client's.
    Hello(Hello),
    Paint(&'a [u8]),
    Exit(&'a str),
    Stdout(&'a [u8]),
    Stderr(&'a [u8]),
    Done {
        status: u8,
    },
    /// First after an `Attach` sent with the client's terminal
    /// (`fuxix::socket::send_with_fd`): whether the server took it, and
    /// reads the keys and writes the paints there itself. If it did not,
    /// the client relays them in frames, as one that sent no terminal does.
    Terminal {
        taken: bool,
    },
    /// A request's answer.
    Reply(Box<Reply>),
    /// To an attached client: the server ended its screen by itself (copy
    /// mode whose rows dropped from history, whose pane went or is no longer
    /// focused, or with no pane to copy; a menu, chooser, prompt or
    /// confirmation whose subject closed), the mode being the client's.
    Ended,
    /// To an attached client: fuxth to run as its own, asked of it from
    /// elsewhere (`str c1 client menu`).
    Run(&'a str),
    /// To an attached client: keys to take as typed at its terminal
    /// (`send-keys` to a client).
    Keys(Vec<crate::command::Input>),
    /// A piece of a reply too large for one frame: its encoding, in order,
    /// whole once `PartsEnd` comes.
    Part(&'a [u8]),
    PartsEnd,
}

impl<'a> ServerFrame<'a> {
    /// Writes `bytes` into `out` as the frames `frame` makes of pieces of
    /// them, each as long as fits: a paint of any length, or a command's
    /// output. No bytes, no frames.
    pub fn split_into(bytes: &'a [u8], frame: fn(&'a [u8]) -> Self, out: &mut ByteQueue) {
        let (whole, rest) = bytes.as_chunks::<MAX_PAYLOAD>();
        for piece in whole
            .iter()
            .map(|piece| piece.as_slice())
            .chain((!rest.is_empty()).then_some(rest))
        {
            out.push_with(|out| put(&frame(piece), out));
        }
    }
}

impl<'a> Frame<'a> for ServerFrame<'a> {
    fn decode(kind: u8, payload: &'a [u8]) -> Result<Self, Error> {
        let text = || std::str::from_utf8(payload).map_err(|_| crate::wire::Error::NotUtf8);
        match kind {
            kind::HELLO => read(payload).map(ServerFrame::Hello),
            kind::PAINT => Ok(ServerFrame::Paint(payload)),
            kind::EXIT => text().map(ServerFrame::Exit).map_err(Error::Wire),
            kind::STDOUT => Ok(ServerFrame::Stdout(payload)),
            kind::STDERR => Ok(ServerFrame::Stderr(payload)),
            kind::DONE => read(payload).map(|status| ServerFrame::Done { status }),
            kind::TERMINAL => read(payload).map(|taken| ServerFrame::Terminal { taken }),
            kind::REPLY => read(payload).map(|reply| ServerFrame::Reply(Box::new(reply))),
            kind::ENDED => read(payload).map(|()| ServerFrame::Ended),
            kind::RUN => text().map(ServerFrame::Run).map_err(Error::Wire),
            kind::PART => Ok(ServerFrame::Part(payload)),
            kind::PARTS_END => read(payload).map(|()| ServerFrame::PartsEnd),
            kind::KEYS => read(payload).map(ServerFrame::Keys),
            other => Err(Error::UnexpectedKind(other)),
        }
    }

    fn write(&self, out: &mut Vec<u8>) -> u8 {
        match self {
            ServerFrame::Hello(hello) => hello.write(out),
            ServerFrame::Reply(reply) => wired(out, kind::REPLY, &**reply),
            ServerFrame::Keys(keys) => wired(out, kind::KEYS, keys),
            ServerFrame::Done { status } => wired(out, kind::DONE, status),
            ServerFrame::Terminal { taken } => wired(out, kind::TERMINAL, taken),
            ServerFrame::Ended => kind::ENDED,
            ServerFrame::PartsEnd => kind::PARTS_END,
            // Bytes as they are: a paint is not copied twice.
            ServerFrame::Run(source) => raw(out, kind::RUN, source.as_bytes()),
            ServerFrame::Part(bytes) => raw(out, kind::PART, bytes),
            ServerFrame::Paint(bytes) => raw(out, kind::PAINT, bytes),
            ServerFrame::Exit(reason) => raw(out, kind::EXIT, reason.as_bytes()),
            ServerFrame::Stdout(bytes) => raw(out, kind::STDOUT, bytes),
            ServerFrame::Stderr(bytes) => raw(out, kind::STDERR, bytes),
        }
    }
}

/// Writes `value` to `out`: frame `kind`'s payload. The kind.
fn wired(out: &mut Vec<u8>, kind: u8, value: &impl Wire) -> u8 {
    value.put(out);
    kind
}

/// Writes `bytes` to `out` as they are: frame `kind`'s payload. The kind.
fn raw(out: &mut Vec<u8>, kind: u8, bytes: &[u8]) -> u8 {
    out.extend_from_slice(bytes);
    kind
}

/// A frame of one kind, its payload its value's wire bytes.
trait OneKind: Wire {
    const KIND: u8;
}

impl<T: OneKind> Frame<'_> for T {
    fn decode(kind: u8, payload: &[u8]) -> Result<Self, Error> {
        match kind == T::KIND {
            true => read(payload),
            false => Err(Error::UnexpectedKind(kind)),
        }
    }

    fn write(&self, out: &mut Vec<u8>) -> u8 {
        wired(out, T::KIND, self)
    }
}

/// The value `payload` holds, all of it.
fn read<T: Wire>(payload: &[u8]) -> Result<T, Error> {
    crate::wire::Reader::new(payload)
        .whole()
        .map_err(Error::Wire)
}

/// Accumulates bytes from a stream and yields whole frames. A frame header
/// claiming more than `MAX_FRAME` is refused as soon as it arrives, so what is
/// held for a frame not yet complete stays under `4 + MAX_FRAME` bytes.
#[derive(Default)]
pub struct Decoder {
    buffer: ByteQueue,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.push(bytes);
    }

    /// Gives back memory beyond `keep` bytes once no more is pending.
    pub fn shrink(&mut self, keep: usize) {
        self.buffer.shrink(keep);
    }

    /// The length of the whole frame the bytes held begin with, header
    /// included; `None` if it is not all here yet; an error for a header
    /// that announces no bytes or more than `MAX_FRAME`.
    fn whole(&self) -> Result<Option<usize>, Error> {
        let pending = self.buffer.as_slice();
        let Some(header) = pending.first_chunk::<4>() else {
            return Ok(None);
        };
        let whole = match u32::from_be_bytes(*header) as usize {
            0 => return Err(Error::Empty),
            length if length > MAX_FRAME => {
                return Err(Error::Oversized {
                    bytes: length,
                    limit: MAX_FRAME,
                });
            }
            length => length.saturating_add(4),
        };
        Ok((pending.len() >= whole).then_some(whole))
    }

    /// Whether `frame` has something to say without more bytes: a whole
    /// frame held, or an error.
    pub fn ready(&self) -> Result<bool, Error> {
        self.whole().map(|whole| whole.is_some())
    }

    /// The next whole frame, decoded as one of `F`, its payload lent from
    /// the decoder; `Ok(None)` if more bytes are needed; an error for a
    /// frame that is oversized, malformed or not one of `F`, and the stream
    /// is then unusable.
    pub fn frame<'a, F: Frame<'a>>(&'a mut self) -> Result<Option<F>, Error> {
        let Some(whole) = self.whole()? else {
            return Ok(None);
        };
        let frame = self.buffer.take_front(whole);
        // A whole frame has its kind byte.
        let (kind, payload) = frame
            .get(4..)
            .and_then(<[u8]>::split_first)
            .unwrap_or((&0, &[]));
        F::decode(*kind, payload).map(Some)
    }

    /// Bytes held: whole frames not yet taken, and a frame not yet
    /// complete.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Modifiers;
    use crate::keys::mouse::{MouseAction, MouseButton, MouseEvent};

    /// `frame` encodes with kind byte `kind`, the wire format's, which a
    /// server and a client of another build read: pinned here as a
    /// number, apart from the names the code gives it. And it decodes to
    /// itself as a `$ty` from its bytes pushed one by one, as a slow socket
    /// would deliver them.
    macro_rules! round_trip {
        ($ty:ty, $kind:literal, $frame:expr) => {{
            let frame: $ty = $frame;
            let bytes = frame.encode().unwrap_or_default();
            assert_eq!(bytes.get(4), Some(&$kind), "{frame:?}");
            let mut decoder = Decoder::default();
            let mut decoded = Vec::new();
            for byte in &bytes {
                decoder.push(&[*byte]);
                if let Ok(Some(got)) = decoder.frame::<$ty>() {
                    decoded.push(format!("{got:?}"));
                }
            }
            assert_eq!(decoded, [format!("{frame:?}")]);
            assert_eq!(decoder.buffered(), 0);
        }};
    }

    #[test]
    fn every_frame_round_trips_in_any_chunking_with_the_wire_formats_kind() {
        let hello = |role| Hello {
            protocol: PROTOCOL,
            version: "0.13.0".into(),
            role,
        };
        round_trip!(Hello, 1, hello(Role::Attach));
        round_trip!(ServerFrame, 1, ServerFrame::Hello(hello(Role::Kill)));
        let size = crate::session::default_size();
        let workspace = Some("main".to_owned());
        round_trip!(
            Attach,
            2,
            Attach {
                size,
                workspace,
                read_only: false,
                env: Vec::new(),
            }
        );
        let workspace = None;
        round_trip!(
            Attach,
            2,
            Attach {
                size,
                workspace,
                read_only: true,
                env: vec![("DISPLAY".into(), ":0".into())],
            }
        );
        let act = Act::Paste("界\n".into());
        round_trip!(AttachedFrame, 3, AttachedFrame::Act(Box::new(act)));
        // Every mouse button, Back and Forward too, arrives as itself.
        for button in [MouseButton::Back, MouseButton::Forward] {
            let act = Act::Mouse(MouseEvent {
                action: MouseAction::Press,
                button: Some(button),
                mods: Modifiers::default(),
                row: 1,
                col: 2,
            });
            round_trip!(AttachedFrame, 3, AttachedFrame::Act(Box::new(act)));
        }
        round_trip!(AttachedFrame, 4, AttachedFrame::Resize(size));
        round_trip!(AttachedFrame, 5, AttachedFrame::Detach);
        round_trip!(
            Request,
            6,
            Request {
                command: Command::Rename {
                    target: AnyRef::Pane(crate::id::PaneId::of(3)),
                    name: "界".into(),
                },
                context: Context {
                    cwd: "/somewhere".into(),
                    pane: "%3".parse().ok(),
                    server: Some("1-2".into()),
                    env: vec![("K".into(), "V".into())],
                    client: "c2".parse().ok(),
                    restoring: true,
                },
            }
        );
        round_trip!(ServerFrame, 7, ServerFrame::Paint(&[0, 1, 2, 255]));
        round_trip!(ServerFrame, 8, ServerFrame::Exit("detached"));
        round_trip!(ServerFrame, 9, ServerFrame::Stdout(b"out"));
        round_trip!(ServerFrame, 10, ServerFrame::Stderr(&[]));
        round_trip!(ServerFrame, 11, ServerFrame::Done { status: 2 });
        round_trip!(ServerFrame, 12, ServerFrame::Terminal { taken: true });
        round_trip!(ServerFrame, 12, ServerFrame::Terminal { taken: false });
        round_trip!(ServerFrame, 13, ServerFrame::Reply(Box::default()));
        round_trip!(ServerFrame, 14, ServerFrame::Ended);
        round_trip!(ServerFrame, 15, ServerFrame::Run("me zoom"));
        round_trip!(ServerFrame, 16, ServerFrame::Keys(Vec::new()));
        // A Hello's bytes never change, so that `Kill` reaches a server of
        // any version: the length, the kind, the protocol, the role, then
        // the version's length and bytes.
        let kill = Hello {
            protocol: 9,
            version: "0.13.0".into(),
            role: Role::Kill,
        };
        let head = [0, 0, 0, 16, 1, 0, 0, 0, 9, 2, 0, 0, 0, 6];
        assert_eq!(kill.encode(), Ok([&head[..], b"0.13.0"].concat()));
        // Each role's byte is the wire format's too: after the length, the
        // kind and the protocol's four bytes.
        for (role, byte) in [
            (Role::Attach, 0),
            (Role::Command, 1),
            (Role::Kill, 2),
            (Role::Headless, 3),
        ] {
            let encoded = hello(role).encode().unwrap_or_default();
            assert_eq!(encoded.get(9), Some(&byte), "{role:?}");
        }
    }

    #[test]
    fn oversized_frames_are_refused_on_both_sides() {
        let big = vec![0; MAX_PAYLOAD + 1];
        assert_eq!(
            ServerFrame::Paint(&big).encode().map_err(|e| e.to_string()),
            Err(format!(
                "a frame of {} bytes exceeds the {MAX_PAYLOAD}-byte limit",
                MAX_PAYLOAD + 1
            ))
        );
        let fits = big.get(..MAX_PAYLOAD).unwrap_or_default();
        assert!(ServerFrame::Paint(fits).encode().is_ok());
        // Refused, it leaves nothing behind.
        let mut out = b"before".to_vec();
        assert!(ServerFrame::Stdout(&big).encode_into(&mut out).is_err());
        assert_eq!(out, b"before");
        let mut decoder = Decoder::default();
        // Any length past the limit will do.
        let over = u32::try_from(MAX_FRAME + 1).unwrap_or(u32::MAX);
        decoder.push(&over.to_be_bytes());
        assert!(decoder.ready().is_err());
        assert!(decoder.frame::<ServerFrame>().is_err());
        // A longer paint is split into frames that fit.
        let mut queue = ByteQueue::default();
        let paint = vec![7; MAX_PAYLOAD * 2 + 3];
        ServerFrame::split_into(&paint, ServerFrame::Paint, &mut queue);
        let mut decoder = Decoder::default();
        decoder.push(queue.as_slice());
        let mut frames = 0;
        while let Ok(Some(ServerFrame::Paint(_))) = decoder.frame() {
            frames += 1;
        }
        assert_eq!(frames, 3);
        assert_eq!(decoder.buffered(), 0);
    }

    /// Bytes that are not a frame of the kinds expected where they are
    /// decoded: each an error, which says why.
    #[test]
    fn malformed_frames_are_errors_not_panics() {
        type Decode = fn(&mut Decoder) -> Result<(), Error>;
        let server: Decode = |d| d.frame::<ServerFrame>().map(drop);
        let hello: Decode = |d| d.frame::<Hello>().map(drop);
        let attach: Decode = |d| d.frame::<Attach>().map(drop);
        let attached: Decode = |d| d.frame::<AttachedFrame>().map(drop);
        let command: Decode = |d| d.frame::<Request>().map(drop);
        let utf8 = "a frame's payload: a text that is not UTF-8";
        for (decode, bytes, message) in [
            (server, vec![0, 0, 0, 1, 99], "unexpected frame kind 99"),
            // A server's frame sent to a server, a client's before its
            // Hello, its input before its Attach, and a second Attach.
            (attached, vec![0, 0, 0, 1, 7], "unexpected frame kind 7"),
            (hello, vec![0, 0, 0, 1, 5], "unexpected frame kind 5"),
            (attach, vec![0, 0, 0, 1, 3], "unexpected frame kind 3"),
            (attached, vec![0, 0, 0, 1, 2], "unexpected frame kind 2"),
            (
                server,
                vec![0, 0, 0, 1, 11],
                "a frame's payload: the bytes end early",
            ),
            (
                server,
                vec![0, 0, 0, 3, 11, 0, 0],
                "a frame's payload: bytes after the value",
            ),
            (
                command,
                vec![0, 0, 0, 5, 6, 255, 255, 255, 255],
                "a frame's payload: no command has tag 255",
            ),
            (server, vec![0, 0, 0, 0], "an empty frame"),
            (server, vec![0, 0, 0, 2, 8, 0xff], utf8),
            (
                hello,
                vec![0, 0, 0, 6, 1, 0, 0, 0, 1, 7],
                "a frame's payload: no role has tag 7",
            ),
            (
                attach,
                vec![0, 0, 0, 6, 2, 0, 1, 0, 1, 2],
                "a frame's payload: no option has tag 2",
            ),
            (
                attached,
                vec![0, 0, 0, 5, 4, 0, 0, 0, 1],
                "a frame's payload: size out of range",
            ),
            (
                hello,
                vec![0, 0, 0, 11, 1, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0xff],
                utf8,
            ),
        ] {
            let mut decoder = Decoder::default();
            decoder.push(&bytes);
            let got = decode(&mut decoder).map_err(|e| e.to_string());
            assert_eq!(got, Err(message.to_owned()), "{bytes:?}");
        }
    }
}

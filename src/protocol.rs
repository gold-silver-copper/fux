//! The private protocol between a `fux` client and its server.
//!
//! A frame is `u32 length | u8 kind | payload`, big-endian, where `length`
//! counts the kind byte and the payload. A frame is at most `MAX_FRAME`; a
//! longer paint or output is split across frames by the sender.
//!
//! Each frame is decoded once, as the type of frame allowed where it
//! arrives: by a server, a `Hello`, then an `Attach` and `AttachedFrame`s,
//! or one `Asked`; by a client, its `Greeting`, then `ToCommand` frames,
//! or `ToAttached` ones. Its bytes are lent from the decoder. Any other
//! kind, or a size of zero, does not decode.

use crate::act::Act;
use crate::bytes::ByteQueue;
use crate::code::Code;
use crate::command::{AnyRef, Request};
use crate::id::{ClientId, PaneId};
use crate::json::Json;
use crate::wire::{Wire, wire};
use std::borrow::Cow;

/// Bumped on any change to the frames below.
pub const PROTOCOL: u32 = 36;
/// The largest frame, kind byte included.
pub const MAX_FRAME: usize = 1 << 20;
/// The largest payload one frame carries.
pub const MAX_PAYLOAD: usize = MAX_FRAME - 1;
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
    pub fn between(client: &Hello, server: &Hello) -> Result<(), Self> {
        if client.role == Role::Kill || client.protocol == server.protocol {
            return Ok(());
        }
        Err(Self {
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

wire! {
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
        /// A client with no terminal (`opts COLS ROWS size attach`): `Attach`, then
        /// nothing but its end; the server sizes it (`resize-client`).
        Headless,
    }
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

/// Appends `frame` to `out`, whatever its length, the payload written in
/// place and the header filled in after: the payload's length.
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

/// What a frame carries: a value as the wire has it, or bytes or text lent
/// from the frame as they are, so that a paint is not copied.
trait Payload<'a>: Sized {
    /// Writes it as the frame's payload.
    fn put(&self, out: &mut Vec<u8>);
    /// The value `payload` is, all of it.
    fn get(payload: &'a [u8]) -> Result<Self, Error>;
}

impl<T: Wire> Payload<'_> for T {
    fn put(&self, out: &mut Vec<u8>) {
        Wire::put(self, out);
    }
    fn get(payload: &[u8]) -> Result<Self, Error> {
        crate::wire::Reader::new(payload)
            .whole()
            .map_err(Error::Wire)
    }
}

/// No payload: a frame that is its kind alone. Not a `Wire` value, as no
/// value on the wire is empty (`wire::Wire for Vec`).
impl Payload<'_> for () {
    fn put(&self, _: &mut Vec<u8>) {}
    fn get(payload: &[u8]) -> Result<Self, Error> {
        let trailing = Error::Wire(crate::wire::Error::Trailing);
        payload.is_empty().then_some(()).ok_or(trailing)
    }
}

impl<'a> Payload<'a> for Cow<'a, [u8]> {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self);
    }
    fn get(payload: &'a [u8]) -> Result<Self, Error> {
        Ok(Cow::Borrowed(payload))
    }
}

impl<'a> Payload<'a> for &'a str {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.as_bytes());
    }
    fn get(payload: &'a [u8]) -> Result<Self, Error> {
        std::str::from_utf8(payload).map_err(|_| Error::Wire(crate::wire::Error::NotUtf8))
    }
}

/// A frame of one field is the field's payload (`frames!`); one of none is
/// `()`, and one of two `(A, B)`, wire values one after the other.
impl<'a, T: Payload<'a>> Payload<'a> for (T,) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
    }
    fn get(payload: &'a [u8]) -> Result<Self, Error> {
        T::get(payload).map(|field| (field,))
    }
}

/// `frames! { pub enum Ty<'a> { One(x: X) = 3, Two(y: Y, z: Z) = 4, Bare = BARE } }`
/// expands to the enum (`One(X)`, `Two(Y, Z)`, `Bare`) and its `Frame<'a>`:
/// `One` is kind 3, its payload `x`'s (`Payload`); `Two` kind 4, `y`'s then
/// `z`'s; `Bare` kind `BARE`, a const, its payload empty. Any other kind is
/// `Error::UnexpectedKind`, and two rows of one kind do not compile. `x`,
/// `y` and `z` only name the fields. An enum that lends its payload is
/// declared `<'a>`, the lifetime of the bytes it is decoded from.
macro_rules! frames {
    (
        $(#[$m:meta])*
        $vis:vis enum $ty:ident $(<$a:lifetime>)? {
            $(
                $(#[$vm:meta])*
                $var:ident $(($($p:ident: $pt:ty),+))? = $kind:tt
            ),* $(,)?
        }
    ) => {
        $(#[$m])*
        $vis enum $ty $(<$a>)? {
            $(
                $(#[$vm])*
                $var $(($($pt),+))?,
            )*
        }

        impl<'a> Frame<'a> for $ty $(<$a>)? {
            #[deny(unreachable_patterns)]
            fn decode(kind: u8, payload: &'a [u8]) -> Result<Self, Error> {
                match kind {
                    $($kind => {
                        let ($($($p,)+)?): ($($($pt,)+)?) = Payload::get(payload)?;
                        Ok(Self::$var $(($($p),+))?)
                    })*
                    other => Err(Error::UnexpectedKind(other)),
                }
            }

            fn write(&self, out: &mut Vec<u8>) -> u8 {
                match self {
                    $(Self::$var $(($($p),+))? => {
                        $($(Payload::put($p, out);)+)?
                        $kind
                    })*
                }
            }
        }
    };
}

wire! {
    /// Both ways, the first frame from each side: a client's says what it is
    /// for, the server's answer the role it took. Its fields go in the order
    /// declared, which never changes, nor do they (`Role::Kill`).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Hello {
        pub protocol: u32,
        pub role: Role,
        pub version: String,
    }
}

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

/// The kinds of a `Hello` and an `Exit`, which every client and server
/// reads, of any version.
const HELLO: u8 = 1;
const EXIT: u8 = 8;

impl OneKind for Hello {
    const KIND: u8 = HELLO;
}

wire! {
    /// An attaching client's one frame after its `Hello`: its terminal's size,
    /// the workspace it asks for, whether it only watches, and its
    /// environment, which panes started from its keys take what
    /// `update-environment` names from.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Attach {
        pub size: fux_vt::Size,
        pub workspace: Option<crate::command::WsRef>,
        pub read_only: bool,
        pub env: Vec<(String, std::ffi::OsString)>,
    }
}

impl OneKind for Attach {
    const KIND: u8 = 2;
}

impl Attach {
    /// A client of `size` that types, asks for no workspace, and has no
    /// environment.
    pub fn of(size: fux_vt::Size) -> Attach {
        Attach {
            size,
            workspace: None,
            read_only: false,
            env: Vec::new(),
        }
    }
}

frames! {
    /// An attached client's frames.
    #[derive(Clone, Debug, PartialEq)]
    pub enum AttachedFrame {
        Act(act: Box<Act>) = 3,
        Resize(size: fux_vt::Size) = 4,
        Detach = 5,
    }
}

wire! {
    /// Where a request comes from: the caller's directory, its pane
    /// (`FUX_PANE`) and the server that pane is of (`FUX_SERVER`), its
    /// environment if the request may start programs (they take what
    /// `update-environment` names of it), and the client whose binding or
    /// prompt runs it.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Context {
        pub cwd: std::path::PathBuf,
        pub pane: Option<PaneId>,
        pub server: Option<String>,
        /// None has not been captured; Some, even empty, is the caller's snapshot.
        pub env: Option<Vec<(String, std::ffi::OsString)>>,
        pub client: Option<ClientId>,
    }
}

wire! {
    /// Attached frames sent before a job was dispatched. Every request
    /// keeps its source, separate from the client a program may target.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Fence {
        pub run: crate::command::Run,
        pub client: ClientId,
        pub frames: u64,
    }
}

wire! {
    /// A command client's frames after its `Hello`: requests, each answered
    /// before the next, and where each comes from.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Asked {
        pub after: Option<Fence>,
        pub request: Request,
        pub context: Context,
    }
}

impl OneKind for Asked {
    const KIND: u8 = 6;
}

wire! {
    /// A request's return, ordinary failure, or pane outcomes. Pane outcomes
    /// alone determine whether an action or wait failed.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Reply {
        Returned(body: Body),
        Failed { code: Code, message: String },
        Answered(answers: crate::answer::PaneResults),
    }
}

wire! {
    /// What a request answers, as data: the client renders it as text, or
    /// as JSON (`json`).
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub enum Body {
        #[default]
        Empty,
        Text(text: String),
        /// The pane, tab or workspace a lookup found.
        Found(id: AnyRef),
        /// The panes or tabs a place holds, in its order.
        Handles(ids: Vec<AnyRef>),
        Capture(captured: Box<crate::capture::Captured>),
        /// `ls`: everything (`of` none), or a pane, tab or workspace and
        /// what is in it (`info`).
        Listing {
            of: Option<crate::command::Kind>,
            listing: Json,
        },
        /// One field of a pane, tab or workspace (`get`).
        Field(value: Json),
        /// A chooser's rows: what each is, its label, and the place of the
        /// client's own.
        Choices {
            rows: Vec<(AnyRef, String)>,
            current: Option<usize>,
        },
    }
}

/// How a request ended, derived from its reply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum End {
    #[default]
    Done,
    Failed {
        code: Code,
        message: String,
    },
}

impl End {
    /// The code that names its status, if it has one.
    pub fn code(&self) -> Option<Code> {
        match self {
            End::Done => None,
            End::Failed { code, .. } => Some(*code),
        }
    }

    /// The exit status.
    pub fn status(&self) -> u8 {
        self.code().map_or(0, |code| code.status().get())
    }
}

impl Reply {
    /// A failure of kind `code`, with its exit status, and why.
    pub fn failed(code: Code, message: impl Into<String>) -> Self {
        Self::Failed {
            code,
            message: message.into(),
        }
    }

    /// How the request ended. A collection's first failure in watch order
    /// names the whole; every failed member contributes its explanation.
    pub fn end(&self) -> End {
        match self {
            Reply::Returned(_) => End::Done,
            Reply::Failed { code, message } => End::Failed {
                code: *code,
                message: message.clone(),
            },
            Reply::Answered(answers) => match answers
                .all()
                .iter()
                .find_map(crate::answer::PaneAnswer::failure)
            {
                None => End::Done,
                Some(code) => End::Failed {
                    code,
                    message: answers
                        .all()
                        .iter()
                        .filter_map(crate::answer::PaneAnswer::why)
                        .collect::<Vec<_>>()
                        .join("\n"),
                },
            },
        }
    }
}

impl Default for Reply {
    fn default() -> Self {
        Self::Returned(Body::Empty)
    }
}

impl From<Body> for Reply {
    fn from(body: Body) -> Self {
        Self::Returned(body)
    }
}

impl From<crate::answer::PaneResults> for Reply {
    fn from(answers: crate::answer::PaneResults) -> Self {
        Self::Answered(answers)
    }
}

frames! {
    /// The server's first frames to a client: its `Hello`, the answer to
    /// the client's; or, before or after it, an `Exit` saying why it goes no
    /// further; and to a `Kill`, `Done` as it stops. A client of any version
    /// reads these, so their kinds and payloads never change.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Greeting<'a> {
        Hello(hello: Hello) = HELLO,
        Exit(reason: &'a str) = EXIT,
        /// The kill's answer, with its status: 0.
        Done(status: u8) = 11,
    }
}

frames! {
    /// The server's frames to a command client: what its request streams
    /// before its reply (`events`, a wait's lines), then the reply in
    /// pieces; or an `Exit` saying why it ends.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum ToCommand<'a> {
        Exit(reason: &'a str) = EXIT,
        Stdout(bytes: Cow<'a, [u8]>) = 9,
        /// A piece of a request's reply: its encoding, in order, whole once
        /// `PartsEnd` comes. A reply goes in pieces as long as fit, so it is
        /// encoded once, whatever its length.
        Part(bytes: Cow<'a, [u8]>) = 17,
        PartsEnd = 18,
    }
}

frames! {
    /// The server's frames to an attaching client, and to the client once
    /// it is attached.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum ToAttached<'a> {
        /// The answer to its `Attach`, before anything is painted: its id,
        /// which its bindings' `me` names and a headless client prints.
        Welcome(client: ClientId, run: crate::command::Run) = 12,
        /// Bytes for its terminal, which the client writes there: a paint,
        /// or what the server sends it outside one (a copy, the bell,
        /// questions for the terminal). Every paint comes so.
        Paint(bytes: Cow<'a, [u8]>) = 7,
        Exit(reason: &'a str) = EXIT,
        /// The server ended its screen opening by itself (copy mode
        /// whose rows dropped from history, whose pane went or is no longer
        /// focused, or with no pane to copy; a chooser, prompt or
        /// confirmation whose subject closed), the mode being the client's.
        Ended(opening: u64) = 14,
        /// fuxth to run as its own, asked of it from elsewhere (`str c1
        /// client choose-tab`).
        Run(code: String) = 15,
        /// Keys to take as typed at its terminal (`send-keys` to a client).
        Keys(keys: Vec<crate::command::Input>) = 16,
    }
}

/// Writes `bytes` into `out` as the frames `frame` makes of pieces of them,
/// each as long as fits: a paint of any length, a command's output, a
/// reply. No bytes, no frames.
pub fn split_into<'a, F: Frame<'a>>(
    bytes: &'a [u8],
    frame: fn(Cow<'a, [u8]>) -> F,
    out: &mut ByteQueue,
) {
    let (whole, rest) = bytes.as_chunks::<MAX_PAYLOAD>();
    for piece in whole
        .iter()
        .map(|piece| piece.as_slice())
        .chain((!rest.is_empty()).then_some(rest))
    {
        out.push_with(|out| put(&frame(Cow::Borrowed(piece)), out));
    }
}

/// A frame of one kind, its payload its value's wire bytes.
trait OneKind: Wire {
    const KIND: u8;
}

impl<T: OneKind> Frame<'_> for T {
    fn decode(kind: u8, payload: &[u8]) -> Result<Self, Error> {
        match kind == T::KIND {
            true => Payload::get(payload),
            false => Err(Error::UnexpectedKind(kind)),
        }
    }

    fn write(&self, out: &mut Vec<u8>) -> u8 {
        self.put(out);
        T::KIND
    }
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

    /// Pane outcomes survive the wire with one status owner, including
    /// mixed collections whose first failing member names the whole.
    #[test]
    fn pane_outcomes_determine_the_reply_status() -> Result<(), Box<dyn std::error::Error>> {
        use crate::answer::{PaneAnswer, PaneResults};
        let version = "%2:0.0.0.0.0:48211-1867a3f9c2b8e7d6"
            .parse()
            .map_err(|()| "version")?;
        let changed = PaneAnswer::Changed {
            version,
            changed: vec![crate::answer::Change::Screen],
            ended: None,
        };
        let closed = PaneAnswer::Closed(PaneId::of(3));
        let timeout = PaneAnswer::TimedOut {
            since: version,
            limit: std::time::Duration::from_millis(1500),
        };
        for (results, code) in [
            (PaneResults::One(PaneAnswer::Acted(version)), None),
            (PaneResults::One(changed.clone()), None),
            (PaneResults::One(closed.clone()), Some(Code::Closed)),
            (PaneResults::One(timeout.clone()), Some(Code::Timeout)),
            (PaneResults::Many(vec![]), None),
            (
                PaneResults::Many(vec![changed.clone(), closed.clone(), timeout.clone()]),
                Some(Code::Closed),
            ),
            (
                PaneResults::Many(vec![changed, timeout, closed]),
                Some(Code::Timeout),
            ),
        ] {
            let reply = Reply::Answered(results);
            let bytes = crate::wire::encode(&reply)?;
            let decoded = crate::wire::Reader::new(&bytes).whole::<Reply>()?;
            assert_eq!(decoded, reply);
            assert_eq!(decoded.end().code(), code);
            assert_eq!(
                decoded.end().status(),
                code.map_or(0, |code| code.status().get())
            );
            let values = crate::script::answered(decoded.clone()).map_err(|f| f.to_string())?;
            let json = values
                .first()
                .map(|v| match v {
                    fuxth::Value::App(crate::script::Fux::Answer(a)) => a.json().render(),
                    fuxth::Value::Int(_)
                    | fuxth::Value::Bool(_)
                    | fuxth::Value::Str(_)
                    | fuxth::Value::List(_)
                    | fuxth::Value::App(_) => String::new(),
                })
                .ok_or("answer")?;
            assert_eq!(json.contains(r#""status":124"#), has_timeout(&decoded));
            if let End::Failed { message, .. } = decoded.end() {
                assert!(!message.is_empty());
                if has_timeout(&decoded) {
                    assert!(message.contains("unchanged for 1.5s"), "{message}");
                }
            }
        }
        Ok(())
    }

    fn has_timeout(reply: &Reply) -> bool {
        matches!(reply, Reply::Answered(answers) if answers.all().iter().any(|a| a.failure() == Some(Code::Timeout)))
    }

    /// A frame decodes only from the bytes it encodes to: an `Act` whose
    /// key is spelled otherwise (`S-.`, the key `.`) is refused, where it
    /// decoded, and encoded as `.` (the `protocol` fuzz target's
    /// regression-a-key-spelled-otherwise).
    #[test]
    fn a_frame_with_a_key_spelled_otherwise_is_refused() {
        let mut decoder = Decoder::default();
        decoder.push(&[0, 0, 0, 10, 3, 0, 0, 0, 0, 3, b'S', b'-', b'.', 0, 37, 39]);
        assert!(decoder.frame::<AttachedFrame>().is_err());
    }
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
    fn every_frame_round_trips_in_any_chunking_with_the_wire_formats_kind()
    -> Result<(), Box<dyn std::error::Error>> {
        let run = "1-2".parse().map_err(|()| "test run")?;
        let hello = |role| Hello {
            protocol: PROTOCOL,
            version: "0.13.0".into(),
            role,
        };
        round_trip!(Hello, 1, hello(Role::Attach));
        round_trip!(Greeting, 1, Greeting::Hello(hello(Role::Kill)));
        let size = fux_vt::Size::from((std::num::NonZeroU16::MIN, std::num::NonZeroU16::MAX));
        let workspace = Some(crate::command::WsRef::Name("main".to_owned()));
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
        for act in [
            crate::act::ScreenAct::Show(crate::act::Panel::Repeat("resize".into())),
            crate::act::ScreenAct::Hide,
            crate::act::ScreenAct::Copy(crate::act::CopyAct::Enter {
                hints: vec![],
                typing: vec![],
            }),
            crate::act::ScreenAct::Copy(crate::act::CopyAct::Yank),
        ] {
            let act = Act::Screen {
                opening: u64::MAX,
                act,
            };
            round_trip!(AttachedFrame, 3, AttachedFrame::Act(Box::new(act)));
        }
        // Every mouse button, Back and Forward too, arrives as itself.
        for button in [MouseButton::Back, MouseButton::Forward] {
            let act = Act::Mouse(MouseEvent {
                action: MouseAction::Press(button),
                mods: Modifiers::default(),
                row: 1,
                col: 2,
            });
            round_trip!(AttachedFrame, 3, AttachedFrame::Act(Box::new(act)));
        }
        round_trip!(AttachedFrame, 4, AttachedFrame::Resize(size));
        round_trip!(AttachedFrame, 5, AttachedFrame::Detach);
        round_trip!(
            Asked,
            6,
            Asked {
                after: Some(Fence {
                    run,
                    client: ClientId::of(2),
                    frames: 3000
                }),
                request: Request::Do(crate::command::Command::Rename {
                    target: AnyRef::Pane(crate::id::PaneId::of(3)),
                    name: "界".into(),
                }),
                context: Context {
                    cwd: "/somewhere".into(),
                    pane: "%3".parse().ok(),
                    server: Some("1-2".into()),
                    env: Some(vec![("K".into(), "V".into())]),
                    client: "c2".parse().ok(),
                },
            }
        );
        let paint = ToAttached::Paint(Cow::Borrowed(&[0, 1, 2, 255]));
        round_trip!(ToAttached, 7, paint);
        round_trip!(Greeting, 8, Greeting::Exit("refused"));
        round_trip!(ToCommand, 8, ToCommand::Exit("stopped"));
        round_trip!(ToAttached, 8, ToAttached::Exit("detached"));
        round_trip!(ToCommand, 9, ToCommand::Stdout(Cow::Borrowed(b"out")));
        round_trip!(Greeting, 11, Greeting::Done(2));
        round_trip!(ToAttached, 12, ToAttached::Welcome(ClientId::of(2), run));
        round_trip!(ToAttached, 14, ToAttached::Ended(7));
        round_trip!(ToAttached, 15, ToAttached::Run("me zoom".into()));
        round_trip!(ToAttached, 16, ToAttached::Keys(Vec::new()));
        round_trip!(ToCommand, 17, ToCommand::Part(Cow::Borrowed(b"reply")));
        round_trip!(ToCommand, 18, ToCommand::PartsEnd);
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
        // Nor do the kill's answer's, which a client of any version reads
        // after it: the length, the kind, and its status, 0.
        let done = Greeting::Done(0);
        assert_eq!(done.encode(), Ok(vec![0, 0, 0, 2, 11, 0]));
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
        Ok(())
    }

    #[test]
    fn oversized_frames_are_refused_on_both_sides() {
        let big = vec![0; MAX_PAYLOAD + 1];
        assert_eq!(
            ToAttached::Paint(Cow::Borrowed(&big))
                .encode()
                .map_err(|e| e.to_string()),
            Err(format!(
                "a frame of {} bytes exceeds the {MAX_PAYLOAD}-byte limit",
                MAX_PAYLOAD + 1
            ))
        );
        let fits = big.get(..MAX_PAYLOAD).unwrap_or_default();
        assert!(ToAttached::Paint(Cow::Borrowed(fits)).encode().is_ok());
        // Refused, it leaves nothing behind.
        let mut out = b"before".to_vec();
        let stdout = ToCommand::Stdout(Cow::Borrowed(&big));
        assert!(stdout.encode_into(&mut out).is_err());
        assert_eq!(out, b"before");
        let mut decoder = Decoder::default();
        // Any length past the limit will do.
        let over = u32::try_from(MAX_FRAME + 1).unwrap_or(u32::MAX);
        decoder.push(&over.to_be_bytes());
        assert!(decoder.ready().is_err());
        assert!(decoder.frame::<ToAttached>().is_err());
        // A longer paint is split into frames that fit.
        let mut queue = ByteQueue::default();
        let paint = vec![7; MAX_PAYLOAD * 2 + 3];
        split_into(&paint, ToAttached::Paint, &mut queue);
        let mut decoder = Decoder::default();
        decoder.push(queue.as_slice());
        let mut frames = 0;
        while let Ok(Some(ToAttached::Paint(_))) = decoder.frame() {
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
        let server: Decode = |d| d.frame::<ToAttached>().map(drop);
        let greeting: Decode = |d| d.frame::<Greeting>().map(drop);
        let commanded: Decode = |d| d.frame::<ToCommand>().map(drop);
        let hello: Decode = |d| d.frame::<Hello>().map(drop);
        let attach: Decode = |d| d.frame::<Attach>().map(drop);
        let attached: Decode = |d| d.frame::<AttachedFrame>().map(drop);
        let command: Decode = |d| d.frame::<Asked>().map(drop);
        let utf8 = "a frame's payload: a text that is not UTF-8";
        for (decode, bytes, message) in [
            (server, vec![0, 0, 0, 1, 99], "unexpected frame kind 99"),
            // A server's frame sent to a server, a client's before its
            // Hello, its input before its Attach, and a second Attach.
            (attached, vec![0, 0, 0, 1, 7], "unexpected frame kind 7"),
            (hello, vec![0, 0, 0, 1, 5], "unexpected frame kind 5"),
            (attach, vec![0, 0, 0, 1, 3], "unexpected frame kind 3"),
            (attached, vec![0, 0, 0, 1, 2], "unexpected frame kind 2"),
            // A frame of another role: a paint to a command client, or
            // before the server's Hello, a reply's piece to an attached
            // client.
            (commanded, vec![0, 0, 0, 1, 7], "unexpected frame kind 7"),
            (greeting, vec![0, 0, 0, 1, 7], "unexpected frame kind 7"),
            (server, vec![0, 0, 0, 1, 17], "unexpected frame kind 17"),
            (
                greeting,
                vec![0, 0, 0, 1, 11],
                "a frame's payload: the bytes end early",
            ),
            (
                greeting,
                vec![0, 0, 0, 3, 11, 0, 0],
                "a frame's payload: bytes after the value",
            ),
            (
                command,
                vec![0, 0, 0, 6, 6, 0, 255, 255, 255, 255],
                "a frame's payload: no Request has tag 255",
            ),
            (server, vec![0, 0, 0, 0], "an empty frame"),
            // A frame's payload is its fields, and no more.
            (
                server,
                vec![0, 0, 0, 10, 14, 0, 0, 0, 0, 0, 0, 0, 1, 0],
                "a frame's payload: bytes after the value",
            ),
            (
                server,
                vec![0, 0, 0, 2, 14, 9],
                "a frame's payload: the bytes end early",
            ),
            (
                attached,
                vec![0, 0, 0, 2, 5, 0],
                "a frame's payload: bytes after the value",
            ),
            (server, vec![0, 0, 0, 2, 8, 0xff], utf8),
            (
                hello,
                vec![0, 0, 0, 6, 1, 0, 0, 0, 1, 7],
                "a frame's payload: no Role has tag 7",
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

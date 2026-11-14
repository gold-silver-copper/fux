//! The private protocol between a `fux` client and its server.
//!
//! A frame is `u32 length | u8 kind | payload`, big-endian, where `length`
//! counts the kind byte and the payload. A frame is at most `MAX_FRAME`; a
//! longer one goes as `MORE` frames, each a piece of its payload, then a
//! frame of its kind with the last piece. A `Decoder` holds one frame and
//! refuses a piece; only a reader that asked for something long joins
//! them, into a `Joining` it holds for that (`Decoder::long`).
//!
//! Each frame is decoded once, as the type of frame allowed where it
//! arrives: by a server, a `Hello`, then an `Attach` and `AttachedFrame`s,
//! or one `Asked`; by a client, its `Greeting`, then `ToCommand` frames,
//! or `ToAttached` ones. Its bytes are lent from the decoder. Any other
//! kind, or a size of zero, does not decode.

use crate::act::Act;
use crate::code::Code;
use crate::command::{AnyRef, Request};
use crate::id::{ClientId, PaneId};
use crate::json::Json;
use crate::wire::{Sink, Wire, wire};
use fux_vt::bytes::ByteQueue;
use std::borrow::Cow;

/// Bumped on any change to the frames below.
pub const PROTOCOL: u32 = 79;

/// The first protocol whose command line is grift: an older server's binary
/// reads none.
pub const GRIFT: u32 = 77;
/// The largest frame, kind byte included.
pub const MAX_FRAME: usize = 1 << 20;
/// The largest payload one frame carries.
pub const MAX_PAYLOAD: usize = MAX_FRAME - 1;
/// Room left in a frame a client sends, or bytes its server sends it, for
/// what the server and the client's connection's process say with it as
/// they pass it on: a few bytes, so far less than this.
pub const ENVELOPE: usize = 64;
/// The kind of a piece of a longer frame's payload, followed by more
/// pieces and then the frame's own kind with the last of it.
const MORE: u8 = 19;
/// Why bytes are not a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A header announcing more than the limit.
    Oversized { bytes: usize, limit: usize },
    /// A header announcing no bytes, not even the kind.
    Empty,
    /// A kind unknown, or not one the frames decoded here may have.
    UnexpectedKind(u8),
    /// A payload that does not read as its frame's.
    Wire(crate::wire::Error),
    /// A piece of a longer message, where a message is one frame at most.
    Piece,
    /// The OS refused the memory to join, hold or send a message of more
    /// than this many bytes.
    Memory(usize),
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
            Error::Piece => write!(
                f,
                "a piece of a longer message, where a message is one frame \
                 ({MAX_FRAME} bytes) at most"
            ),
            Error::Memory(bytes) => {
                write!(
                    f,
                    "the OS refused memory for a message of over {bytes} bytes"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

/// A client and a server of different protocols, which understand each
/// other's `Hello` and nothing after it: what both sides say of it, the
/// server's protocol and version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub protocol: u32,
    pub version: String,
}

impl Mismatch {
    /// Whether a client that said `client` can go on with a server that
    /// said `server`: either may ask, holding both.
    pub fn between(client: &Hello, server: &Hello) -> Result<(), Self> {
        if client.protocol == server.protocol {
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
        let theirs = Build {
            version: &self.version,
            protocol: self.protocol,
        };
        write!(
            f,
            "the server runs {theirs}; use a matching client to keep its panes running. `kill` on its process stops it and its panes (`fux '(status)'` gives its `server-process-id`, where the system says it); a new server does not resume them",
        )
    }
}

/// A build of fux as it is told apart from another: its release and its
/// protocol, as `fux 0.36.0 (protocol 55)`. Builds of one release may
/// speak different protocols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Build<'a> {
    pub version: &'a str,
    pub protocol: u32,
}

impl Build<'_> {
    /// This build.
    pub const OURS: Build<'static> = Build {
        version: env!("CARGO_PKG_VERSION"),
        protocol: PROTOCOL,
    };
}

impl std::fmt::Display for Build<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "fux {} (protocol {})", self.version, self.protocol)
    }
}

impl std::error::Error for Mismatch {}

/// What a connecting client is for, as the byte in its `Hello`'s payload
/// says. A server of any protocol reads that byte, so one this protocol
/// has no role for is kept as it came (`Unknown`): its client, of another
/// protocol, is answered its mismatch, and never served.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A client that shows the server: `Attach`, then input.
    Attach,
    /// One command, then its output.
    Command,
    /// Another protocol's byte: 2 was a kill's, 3 a headless attach's.
    Unknown(u8),
}

impl Wire for Role {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        out.push(match self {
            Role::Attach => 0,
            Role::Command => 1,
            Role::Unknown(byte) => *byte,
        });
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        Ok(match <u8 as Wire>::get(r)? {
            0 => Role::Attach,
            1 => Role::Command,
            byte => Role::Unknown(byte),
        })
    }
}

impl Role {
    /// The byte an older protocol's kill said, which a server of that
    /// protocol obeyed whatever the client's protocol. Its client takes any
    /// `Hello` as leave to have stopped the server, but reports an `Exit`
    /// before one as a failure: so it is answered that alone.
    pub const RETIRED_KILL: Role = Role::Unknown(2);
}

// Its shape is its byte's.
#[cfg(test)]
impl crate::wire::Reach for Role {}

/// The frames one side expects at one point of a connection, decoded from
/// a kind and a payload lent for `'a`, and encoded.
pub trait Frame<'a>: Sized {
    /// The frame that `kind` and `payload` make, or why they make none of
    /// these.
    fn decode(kind: u8, payload: &'a [u8]) -> Result<Self, Error>;

    /// Writes the payload to `out`; the kind byte.
    fn write(&self, out: &mut impl Sink) -> u8;

    /// The encoded frame.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Appends the encoded frame to `out`: a payload over `MAX_PAYLOAD` in
    /// pieces that fit, all but the last as `MORE` frames.
    fn encode_into(&self, out: &mut Vec<u8>) {
        let start = out.len();
        let payload = put(self, out);
        if payload <= MAX_PAYLOAD {
            return;
        }
        out.truncate(start);
        out.reserve(framed(payload));
        pieces(self, payload, out);
    }

    /// The bytes `encode_into` appends: the frame's, or its pieces'.
    fn encoded_len(&self) -> usize {
        let mut count = Count(0);
        self.write(&mut count);
        framed(count.0)
    }

    /// `encode_into`, once the OS gave room for all of it: refused
    /// (`Error::Memory`), `out` is as it was. The room is reserved first,
    /// and what is written within it allocates nothing, so a process that
    /// must not die of a refusal (a pane's own) sends so.
    fn try_encode_into(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let mut count = Count(0);
        self.write(&mut count);
        let bytes = framed(count.0);
        fux_vt::bytes::try_reserve(out, bytes).map_err(|_| Error::Memory(bytes))?;
        pieces(self, count.0, out);
        Ok(())
    }
}

/// Counts the bytes written to it: a payload's length, before room is made
/// for it.
struct Count(usize);

impl Sink for Count {
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.0 = self.0.saturating_add(bytes.len());
    }
}

/// The bytes a payload of `payload` bytes is framed in: itself, and a header
/// for each piece it goes in, one at least.
fn framed(payload: usize) -> usize {
    payload.saturating_add(payload.div_ceil(MAX_PAYLOAD).max(1).saturating_mul(5))
}

/// Writes a payload as the frames it goes in, a header before each
/// `MAX_PAYLOAD` of it: all but the last `MORE`, the last's kind filled in
/// once the frame said it.
struct Pieces<'a> {
    out: &'a mut Vec<u8>,
    /// The payload's bytes not yet written.
    left: usize,
    /// Those the piece being written still takes.
    room: usize,
    /// Where the last piece's kind byte is.
    kind_at: Option<usize>,
}

impl Pieces<'_> {
    /// The next piece's header: its length, and `MORE`.
    fn begin(&mut self) {
        let piece = self.left.min(MAX_PAYLOAD);
        // A piece fits, so its length does too.
        let length = u32::try_from(piece.saturating_add(1)).unwrap_or(u32::MAX);
        self.out.extend_from_slice(&length.to_be_bytes());
        self.kind_at = Some(self.out.len());
        self.out.push(MORE);
        self.room = piece;
    }
}

impl Sink for Pieces<'_> {
    fn extend_from_slice(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() && self.left > 0 {
            if self.room == 0 {
                self.begin();
            }
            let (now, rest) = bytes
                .split_at_checked(self.room.min(bytes.len()))
                .unwrap_or((bytes, &[]));
            self.out.extend_from_slice(now);
            self.room = self.room.saturating_sub(now.len());
            self.left = self.left.saturating_sub(now.len());
            bytes = rest;
        }
    }
}

/// Appends `frame`, whose payload is `payload` bytes, to `out`, in as many
/// pieces as it takes, copying none of it twice.
fn pieces<'a>(frame: &impl Frame<'a>, payload: usize, out: &mut Vec<u8>) {
    let mut pieces = Pieces {
        out,
        left: payload,
        room: 0,
        kind_at: None,
    };
    let kind = frame.write(&mut pieces);
    // No payload: a frame that is its kind alone.
    if pieces.kind_at.is_none() {
        pieces.begin();
    }
    let at = pieces.kind_at.and_then(|at| pieces.out.get_mut(at));
    if let Some(at) = at {
        *at = kind;
    }
}

/// Whether `frame`, which a client would send, is one its server takes:
/// one frame, with room for what is said with it on its way
/// (`ENVELOPE`). A client asks before it sends what has no reply to say
/// `too-long` with (a screen of its own, shown).
pub fn sends_whole<'a>(frame: &impl Frame<'a>) -> bool {
    put(frame, &mut Vec::new()) <= MAX_PAYLOAD.saturating_sub(ENVELOPE)
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
    fn put(&self, out: &mut impl Sink);
    /// The value `payload` is, all of it.
    fn get(payload: &'a [u8]) -> Result<Self, Error>;
}

impl<T: Wire> Payload<'_> for T {
    fn put(&self, out: &mut impl Sink) {
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
    fn put(&self, _: &mut impl Sink) {}
    fn get(payload: &[u8]) -> Result<Self, Error> {
        let trailing = Error::Wire(crate::wire::Error::Trailing);
        payload.is_empty().then_some(()).ok_or(trailing)
    }
}

impl<'a> Payload<'a> for Cow<'a, [u8]> {
    fn put(&self, out: &mut impl Sink) {
        out.extend_from_slice(self);
    }
    fn get(payload: &'a [u8]) -> Result<Self, Error> {
        Ok(Cow::Borrowed(payload))
    }
}

impl<'a> Payload<'a> for &'a str {
    fn put(&self, out: &mut impl Sink) {
        out.extend_from_slice(self.as_bytes());
    }
    fn get(payload: &'a [u8]) -> Result<Self, Error> {
        std::str::from_utf8(payload).map_err(|_| Error::Wire(crate::wire::Error::NotUtf8))
    }
}

/// A frame of one field is the field's payload (`frames!`); one of none is
/// `()`, and one of two `(A, B)`, wire values one after the other.
impl<'a, T: Payload<'a>> Payload<'a> for (T,) {
    fn put(&self, out: &mut impl Sink) {
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

            fn write(&self, out: &mut impl Sink) -> u8 {
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
    /// declared, which never changes, nor do they: a server of any protocol
    /// reads it, to say which one it speaks.
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
    /// An attaching client and its optional conditional first launch. A
    /// headless one has no terminal (`(attach (size (options) COLUMNS ROWS))`): nothing
    /// but its end follows, and the server sizes it (`resize`).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Attach {
        pub of: Option<crate::command::Run>,
        pub headless: bool,
        pub size: fux_vt::Size,
        pub target: Option<crate::command::AttachTarget>,
        pub read_only: bool,
        pub presentation: crate::presentation::Presentation,
        pub initial: Option<crate::command::InitialWorkspace>,
        /// The client's pid and its ancestors', nearest first, as far as
        /// the OS says: one a pane's program, or the client in its session,
        /// the client runs in that pane,
        /// which it is shown as a placeholder and neither sizes nor types
        /// into.
        pub ancestry: Vec<u32>,
    }
}

impl OneKind for Attach {
    const KIND: u8 = 2;
}

impl Attach {
    /// A noncreating client of `size` that types and asks for no workspace.
    pub fn of(size: fux_vt::Size, presentation: crate::presentation::Presentation) -> Attach {
        Attach {
            of: None,
            headless: false,
            size,
            target: None,
            read_only: false,
            presentation,
            initial: None,
            ancestry: Vec::new(),
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
        /// Its answer to the server's `Read` of this number.
        Answer(asked: u64, value: crate::callback::Data) = 9,
        /// A piece of a paste longer than a frame takes: the first is the
        /// paste to its server (`Act::Paste`), its text kept by the
        /// client's connection; the rest, while its last input is
        /// `Input::More`, follow it, and its server never sees them.
        Piece(inputs: Vec<crate::command::Input>) = PIECE_KIND,
    }
}

/// The kind of a piece of a body, after the frame it belongs to.
const PIECE_KIND: u8 = 20;

/// A piece of a request's inputs after the first, which its request
/// carries: sent after it while the piece before ends in `Input::More`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece(pub Vec<crate::command::Input>);

impl Wire for Piece {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        Wire::put(&self.0, out);
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        Wire::get(r).map(Piece)
    }
}

#[cfg(test)]
impl crate::wire::Reach for Piece {}

impl OneKind for Piece {
    const KIND: u8 = PIECE_KIND;
}

/// The most text or bytes one piece of a body carries: half a frame, the
/// rest left for its other inputs and what is said with it on its way.
pub const PIECE: usize = MAX_PAYLOAD / 2;

/// `inputs` cut into pieces, each one frame with room to spare: a text, a
/// paste or bytes longer than `PIECE` is cut (a paste's parts but the last
/// as `Input::Pasting`, so that it stays one paste), a run of keys is cut
/// between keys, each counted as the wire has it, and every piece but
/// the last ends in `Input::More`. Inputs that fit are one piece, as they
/// are.
pub fn cut_inputs(inputs: Vec<crate::command::Input>) -> Vec<Vec<crate::command::Input>> {
    use crate::command::Input;
    /// Where `len` bytes are cut, `room` left in the piece they begin in:
    /// each part's end but the last's, moved back to where `boundary` lets
    /// a part end.
    fn cuts(len: usize, mut room: usize, boundary: impl Fn(usize) -> usize) -> Vec<usize> {
        let (mut ends, mut at) = (Vec::new(), 0usize);
        while len.saturating_sub(at) > room {
            let end = boundary(at.saturating_add(room)).max(at);
            ends.push(end);
            (at, room) = (end, PIECE);
        }
        ends
    }
    /// More than a text's, a paste's or bytes' kind and length take on the
    /// wire, counted with each.
    const HEAD: usize = 16;
    let (mut pieces, mut piece, mut held) = (Vec::new(), Vec::new(), 0usize);
    for input in inputs {
        let room = PIECE.saturating_sub(held);
        // What is cut: its bytes, where a part may end, and a part of it.
        type Part<'a> = Box<dyn Fn(&[u8], bool) -> Input + 'a>;
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        let (bytes, chars, part): (&[u8], bool, Part<'_>) = match &input {
            Input::Text(t) => (t.as_bytes(), true, Box::new(|b, _| Input::Text(text(b)))),
            Input::Paste(t) => (
                t.as_bytes(),
                true,
                Box::new(|b, last| match last {
                    true => Input::Paste(text(b)),
                    false => Input::Pasting(text(b)),
                }),
            ),
            Input::Pasting(t) => (t.as_bytes(), true, Box::new(|b, _| Input::Pasting(text(b)))),
            Input::Bytes(given) => (given, false, Box::new(|b, _| Input::Bytes(b.to_vec()))),
            Input::Key(_) | Input::More => (&[], false, Box::new(|_, _| Input::More)),
        };
        // What it takes of a piece: a key as the wire has it; what is cut
        // by its bytes, and more than what says their kind and length.
        let takes = match &input {
            Input::Key(_) | Input::More => crate::wire::encode(&input).len(),
            Input::Text(_) | Input::Paste(_) | Input::Pasting(_) | Input::Bytes(_) => {
                bytes.len().saturating_add(HEAD)
            }
        };
        if takes <= room || bytes.is_empty() {
            drop(part);
            // An input with nothing to cut begins the next piece, once
            // this one is full: keys are cut between them.
            if takes > room && !piece.is_empty() {
                piece.push(Input::More);
                pieces.push(std::mem::take(&mut piece));
                held = 0;
            }
            held = held.saturating_add(takes);
            piece.push(input);
            continue;
        }
        let room = room.saturating_sub(HEAD);
        let whole = std::str::from_utf8(bytes).ok().filter(|_| chars);
        let boundary = |mut at: usize| {
            while whole.is_some_and(|text| !text.is_char_boundary(at)) {
                at = at.saturating_sub(1);
            }
            at
        };
        let ends = cuts(bytes.len(), room, boundary);
        let mut from = 0usize;
        for end in ends {
            if let Some(cut) = bytes.get(from..end).filter(|cut| !cut.is_empty()) {
                piece.push(part(cut, false));
            }
            piece.push(Input::More);
            pieces.push(std::mem::take(&mut piece));
            from = end;
        }
        let last = bytes.get(from..).unwrap_or_default();
        held = last.len().saturating_add(HEAD);
        piece.push(part(last, true));
    }
    pieces.push(piece);
    pieces
}

/// `asked` as its client sends it: one frame if it is one, with room for
/// what is said with it on its way; else its inputs cut in pieces
/// (`cut_inputs`), the first in the request and the rest in `Piece` frames
/// after it. A request too long with no inputs to cut goes as it is, and
/// is refused `too-long` where it arrives.
pub fn encode_asked(asked: &Asked) -> Vec<u8> {
    use crate::command::{ClientAction, Command, Request};
    if sends_whole(asked) {
        return asked.encode();
    }
    let mut first = asked.clone();
    let (Request::Do(Command::SendKeys { inputs, .. })
    | Request::Do(Command::Client {
        action: ClientAction::Keys(inputs),
        ..
    })) = &mut first.request
    else {
        return asked.encode();
    };
    let mut rest = cut_inputs(std::mem::take(inputs)).into_iter();
    *inputs = rest.next().unwrap_or_default();
    let mut out = first.encode();
    for piece in rest {
        Piece(piece).encode_into(&mut out);
    }
    out
}

/// `frame` as an attached client sends it, each of its frames encoded: a
/// paste longer than a frame takes in pieces (`AttachedFrame::Piece`), the
/// first to send at once and each further one as it is asked for
/// (`ToAttached::More`); anything else as its one frame.
pub fn encode_attached(frame: &AttachedFrame) -> Vec<Vec<u8>> {
    if let AttachedFrame::Act(act) = frame
        && let Act::Paste(text) = &**act
        && !sends_whole(frame)
    {
        return (cut_inputs(vec![crate::command::Input::Paste(text.clone())]).into_iter())
            .map(|piece| AttachedFrame::Piece(piece).encode())
            .collect();
    }
    vec![frame.encode()]
}

wire! {
    /// Where a request comes from: the caller's directory, its pane
    /// and program generation (`FUX_PANE`), that pane's server (`FUX_SERVER`),
    /// the client whose binding or
    /// prompt runs it.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Context {
        /// The common run of every handle used by this request, if any.
        pub of: Option<crate::command::Run>,
        pub cwd: std::path::PathBuf,
        pub pane: Option<(PaneId, u64)>,
        pub server: Option<String>,
        /// The attached client whose key or prompt runs the request.
        pub client: Option<ClientId>,
    }
}

wire! {
    /// Attached frames sent before a job was dispatched. Every request
    /// keeps its source, separate from the client a program may target.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// A request's reply and the server run that answered it. A local
    /// config collector has no run.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Reply {
        pub run: Option<crate::command::Run>,
        pub response: Response,
    }
}

wire! {
    /// A request's return, ordinary failure, or pane outcomes. Pane outcomes
    /// alone determine whether an action or wait failed.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Response {
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
        /// The pane, tab or workspace a lookup found.
        Found(id: AnyRef),
        /// The panes or tabs a place holds, in its order.
        Handles(ids: Vec<AnyRef>),
        Capture(captured: Box<crate::answer::Captured>),
        Snapshot(snapshot: Box<crate::inspect::Snapshot>),
        Events(batch: Box<crate::events::Batch>),
        /// One field of a pane, tab or workspace (`get`).
        Field(value: Json),
        /// What a client answered of its own (`Request::Read`): its code
        /// values as their words, which the asker compiles.
        Read(value: crate::callback::Data),
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
        Self {
            run: None,
            response: Response::Failed {
                code,
                message: message.into(),
            },
        }
    }

    /// How the request ended. A collection's first failure in watch order
    /// names the whole; every failed member contributes its explanation.
    pub fn end(&self) -> End {
        match &self.response {
            Response::Returned(_) => End::Done,
            Response::Failed { code, message } => End::Failed {
                code: *code,
                message: message.clone(),
            },
            Response::Answered(answers) => match answers
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

impl Default for Response {
    fn default() -> Self {
        Self::Returned(Body::Empty)
    }
}

impl From<Body> for Reply {
    fn from(body: Body) -> Self {
        Self {
            run: None,
            response: Response::Returned(body),
        }
    }
}

impl From<crate::answer::PaneResults> for Reply {
    fn from(answers: crate::answer::PaneResults) -> Self {
        Self {
            run: None,
            response: Response::Answered(answers),
        }
    }
}

frames! {
    /// The server's first frames to a client: its `Hello`, the answer to
    /// the client's; or, before or after it, an `Exit` saying why it goes no
    /// further. A client of any version reads these, so their kinds and
    /// payloads never change.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Greeting<'a> {
        Hello(hello: Hello) = HELLO,
        Exit(reason: &'a str) = EXIT,
    }
}

frames! {
    /// A command's bounded reply in pieces, or an Exit saying why it ends.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum ToCommand<'a> {
        Exit(reason: &'a str) = EXIT,
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
        /// The server ended its screen opening by itself (a chooser or
        /// prompt whose subject closed), the mode being the client's.
        Ended(opening: u64) = 14,
        /// Owned screen data; callbacks are checked by the receiving client.
        Open(screen: crate::callback::Open) = 15,
        /// Keys to take as typed at its terminal (`send-keys` to a client).
        Keys(keys: Vec<crate::command::Input>) = 16,
        /// What only it knows, asked for a request: it sends its `Answer`
        /// with this number.
        Read(asked: u64, read: crate::command::ClientRead) = 13,
        /// The next piece of the paste it is sending is wanted now: it
        /// sends one `AttachedFrame::Piece`. A client sends a paste's first
        /// piece and then each further one only as it is asked for, so it
        /// is never part way through writing one while the paste waits for
        /// a program that isn't reading, and what else it has to say (an
        /// `Answer`, a detach) is said meanwhile.
        More = 21,
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

    fn write(&self, out: &mut impl Sink) -> u8 {
        self.put(out);
        T::KIND
    }
}

/// Accumulates bytes from a stream and yields whole frames, one at a time:
/// it holds no more of a message than one frame, and can't join the pieces
/// of a longer one (`frame` refuses a piece; `long` needs a `Joining`). A
/// frame header claiming more than `MAX_FRAME` is refused as soon as it
/// arrives.
#[derive(Default)]
pub struct Decoder {
    buffer: ByteQueue,
}

/// The pieces of one long message, joined as they come: held only by a
/// reader that asked for something long (a client its reply, a connection's
/// process a pane's screen or capture), never by the server. It grows as
/// the OS gives it memory, and a refusal is an error, not an abort.
#[derive(Default)]
pub struct Joining {
    pieces: Vec<u8>,
    /// `pieces` is a whole message, lent as the frame last decoded: let go
    /// before the next is joined.
    whole: bool,
}

impl Joining {
    /// Adds `piece` to the message, if the OS gives the memory.
    pub fn push(&mut self, piece: &[u8]) -> Result<(), Error> {
        fux_vt::bytes::try_extend(&mut self.pieces, piece)
            .map_err(|_| Error::Memory(self.pieces.len()))
    }

    /// The pieces joined so far.
    pub fn as_slice(&self) -> &[u8] {
        &self.pieces
    }

    /// Bytes held.
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.push(bytes);
    }

    /// `push`, if the OS gives the room (`Error::Memory`): refused, it
    /// holds what it held.
    pub fn try_push(&mut self, bytes: &[u8]) -> Result<(), Error> {
        (self.buffer.try_push(bytes)).map_err(|_| Error::Memory(self.buffer.len()))
    }

    /// Makes room for `additional` more bytes, or says the OS gave none
    /// (`Error::Memory`): a push within it allocates nothing, so a reader
    /// that must not die of a refusal makes room before it reads its
    /// socket, and leaves there what it has no room for.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), Error> {
        (self.buffer.try_reserve(additional)).map_err(|_| Error::Memory(self.buffer.len()))
    }

    /// Gives back memory beyond `keep` bytes once no more is pending.
    pub fn shrink(&mut self, keep: usize) {
        self.buffer.shrink(keep);
    }

    /// Whether `frame` has something to say without more bytes: a whole
    /// frame held, or an error.
    pub fn ready(&self) -> Result<bool, Error> {
        Ok(whole(self.buffer.as_slice())?.is_some())
    }

    /// Whether `long` yields a frame or an error without more bytes: a
    /// whole frame held, its pieces before it; not pieces alone.
    pub fn ready_long(&self) -> Result<bool, Error> {
        let mut pending = self.buffer.as_slice();
        while let Some(length) = whole(pending)? {
            if pending.get(4) != Some(&MORE) {
                return Ok(true);
            }
            pending = pending.get(length..).unwrap_or_default();
        }
        Ok(false)
    }

    /// The next whole frame, decoded as one of `F`, its payload lent from
    /// the decoder; `Ok(None)` if more bytes are needed; an error for a
    /// frame that is oversized, malformed, not one of `F`, or a piece of a
    /// longer message (`Error::Piece`), and the stream is then unusable.
    pub fn frame<'a, F: Frame<'a>>(&'a mut self) -> Result<Option<F>, Error> {
        let Some(whole) = whole(self.buffer.as_slice())? else {
            return Ok(None);
        };
        let frame = self.buffer.take_front(whole);
        // A whole frame has its kind byte.
        match frame.get(4..).and_then(<[u8]>::split_first) {
            Some((&MORE, _)) => Err(Error::Piece),
            Some((kind, payload)) => F::decode(*kind, payload).map(Some),
            None => Err(Error::Empty),
        }
    }

    /// `frame`, for a reader that asked for something long: a message sent
    /// in pieces is joined into `joining`, as far as the OS gives memory
    /// (`Error::Memory`), and lent from it.
    pub fn long<'a, F: Frame<'a>>(
        &'a mut self,
        joining: &'a mut Joining,
    ) -> Result<Option<F>, Error> {
        if std::mem::take(&mut joining.whole) {
            joining.pieces = Vec::new();
        }
        let whole = loop {
            let Some(whole) = whole(self.buffer.as_slice())? else {
                return Ok(None);
            };
            if self.buffer.as_slice().get(4) != Some(&MORE) {
                break whole;
            }
            let piece = self.buffer.take_front(whole);
            joining.push(piece.get(5..).unwrap_or_default())?;
        };
        let frame = self.buffer.take_front(whole);
        // A whole frame has its kind byte.
        let (kind, payload) = frame
            .get(4..)
            .and_then(<[u8]>::split_first)
            .unwrap_or((&0, &[]));
        if joining.pieces.is_empty() {
            return F::decode(*kind, payload).map(Some);
        }
        joining.push(payload)?;
        joining.whole = true;
        F::decode(*kind, &joining.pieces).map(Some)
    }

    /// Lets go of the pieces of a longer message that come next, and of
    /// its last frame once that has come, joining nothing: for a reader
    /// that takes one frame and tells its sender how long the message was.
    /// `gone` counts what was let go of a message still coming, across
    /// calls; the message's length, once all of it is let go.
    pub fn skip_long(&mut self, gone: &mut usize) -> Result<Option<usize>, Error> {
        loop {
            let Some(whole) = whole(self.buffer.as_slice())? else {
                return Ok(None);
            };
            let piece = self.buffer.as_slice().get(4) == Some(&MORE);
            if !piece && *gone == 0 {
                return Ok(None);
            }
            self.buffer.take_front(whole);
            *gone = gone.saturating_add(whole.saturating_sub(5));
            if !piece {
                return Ok(Some(std::mem::take(gone)));
            }
        }
    }

    /// Bytes held: whole frames not yet taken, and a frame not yet
    /// complete.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }
}

/// The length of the whole frame the bytes `pending` begin with, header
/// included; `None` if it is not all here yet; an error for a header that
/// announces no bytes or more than `MAX_FRAME`.
fn whole(pending: &[u8]) -> Result<Option<usize>, Error> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Inputs longer than a frame are cut into pieces that each fit one,
    /// every piece but the last saying more follows; a paste cut stays one
    /// paste, and what the pieces carry is what was given, cut between
    /// characters.
    #[test]
    fn inputs_are_cut_into_pieces_that_say_whether_more_follows() {
        use crate::command::Input;
        let text: String = std::iter::repeat_n("é界", PIECE / 5 * 3).collect();
        let inputs = vec![
            Input::Text("ls".into()),
            Input::Paste(text.clone()),
            Input::Bytes(vec![7; PIECE + 3]),
        ];
        let pieces = cut_inputs(inputs);
        assert!(pieces.len() >= 4, "{} pieces", pieces.len());
        let (mut pasted, mut bytes, mut pastes) = (String::new(), 0usize, 0usize);
        for (n, piece) in pieces.iter().enumerate() {
            assert_eq!(Input::go_on(piece), n + 1 < pieces.len());
            assert!(sends_whole(&Piece(piece.clone())), "piece {n} is no frame");
            for input in piece {
                match input {
                    Input::Pasting(part) => pasted.push_str(part),
                    Input::Paste(part) => {
                        pasted.push_str(part);
                        pastes += 1;
                    }
                    Input::Bytes(given) => bytes += given.len(),
                    Input::Text(_) | Input::Key(_) | Input::More => {}
                }
            }
        }
        assert_eq!((pasted, pastes, bytes), (text, 1, PIECE + 3));
        assert_eq!(cut_inputs(vec![Input::Text("ls".into())]).len(), 1);
    }

    /// Keys have nothing to cut, so a long run of them is cut between
    /// keys: 200,000 presses, and keys among texts, go in pieces that each
    /// fit a frame, every key there, in order (they went as one piece of
    /// any length, which no reader takes).
    #[test]
    fn a_long_run_of_keys_is_cut_between_keys() -> Result<(), Box<dyn std::error::Error>> {
        use crate::command::Input;
        let keys = ["a", "Enter", "C-x", "Down"];
        let mut inputs = Vec::new();
        // The keys alone, then a text among more of them.
        for n in 0..201_000usize {
            if n == 200_000 {
                inputs.push(Input::Text(std::iter::repeat_n('t', PIECE / 3).collect()));
            }
            let key = keys.get(n % keys.len()).ok_or("a key")?;
            inputs.push(Input::Key(key.parse().map_err(|_| "a key press")?));
        }
        let pieces = cut_inputs(inputs.clone());
        assert!(pieces.len() > 1, "one piece");
        // What the pieces carry, a text cut in two joined again.
        let mut joined: Vec<Input> = Vec::new();
        for (n, piece) in pieces.iter().enumerate() {
            assert_eq!(Input::go_on(piece), n + 1 < pieces.len());
            assert!(sends_whole(&Piece(piece.clone())), "piece {n} is no frame");
            for input in piece {
                match (joined.last_mut(), input) {
                    (_, Input::More) => {}
                    (Some(Input::Text(before)), Input::Text(more)) => before.push_str(more),
                    (_, other) => joined.push(other.clone()),
                }
            }
        }
        assert!(
            joined == inputs,
            "{} inputs of {}",
            joined.len(),
            inputs.len()
        );
        Ok(())
    }

    #[test]
    fn observation_wait_reads_its_whole_limit() -> Result<(), Box<dyn std::error::Error>> {
        use crate::command::{AnyRef, Limit, Since, Wait};
        use crate::wire::Reader;
        let wait = Wait {
            since: Since::Now(AnyRef::Pane("%1".parse()?)),
            timeout: Some(Limit::read(0)?),
        };
        let bytes = crate::wire::encode(&wait);
        assert_eq!(Reader::new(&bytes).whole::<Wait>()?, wait);
        let prefix = bytes.get(..bytes.len().saturating_sub(4)).ok_or("limit")?;
        assert!(Reader::new(prefix).whole::<Wait>().is_err());
        Ok(())
    }

    /// A frame encoded into room made first is the frame as it is encoded
    /// otherwise, a long one in the same pieces, which a reader that asked
    /// for it joins; and its length is known before.
    #[test]
    fn a_frame_encoded_into_room_made_first_is_the_same_frame() -> Result<(), Error> {
        for payload in [
            0,
            1,
            MAX_PAYLOAD,
            MAX_PAYLOAD + 1,
            2 * MAX_PAYLOAD,
            2 * MAX_PAYLOAD + 7,
        ] {
            let bytes: Vec<u8> = (0u8..251).cycle().take(payload).collect();
            let frame = ToCommand::Part(Cow::Borrowed(&bytes));
            let (mut plain, mut tried) = (vec![9], vec![9]);
            frame.encode_into(&mut plain);
            frame.try_encode_into(&mut tried)?;
            assert!(plain == tried, "{payload}");
            assert_eq!(frame.encoded_len(), tried.len() - 1, "{payload}");
            let (mut decoder, mut joining) = (Decoder::default(), Joining::default());
            decoder.push(tried.get(1..).unwrap_or_default());
            let joined = decoder.long::<ToCommand>(&mut joining)?;
            assert!(joined == Some(frame), "{payload}");
        }
        let mut bare = Vec::new();
        ToCommand::PartsEnd.try_encode_into(&mut bare)?;
        assert_eq!(bare, ToCommand::PartsEnd.encode());
        Ok(())
    }

    /// Refused memory, an encoding leaves what it would append to as it
    /// was, and a decoder holds what it held; both go on after.
    #[cfg(feature = "checks")]
    #[test]
    fn an_encoding_or_a_push_refused_memory_changes_nothing() -> Result<(), Error> {
        // Longer than the room a short vector has: only what needs room
        // can be refused it.
        let why: String = std::iter::repeat_n('y', 200).collect();
        let frame = ToCommand::Exit(&why);
        let mut out = vec![1, 2, 3];
        out.shrink_to_fit();
        fux_vt::refuse_next(1);
        let refused = frame.try_encode_into(&mut out);
        assert_eq!(refused, Err(Error::Memory(frame.encoded_len())));
        assert_eq!(out, [1, 2, 3]);
        frame.try_encode_into(&mut out)?;
        let encoded = out.get(3..).unwrap_or_default().to_vec();
        assert_eq!(encoded, frame.encode());
        let mut decoder = Decoder::default();
        let (half, rest) = encoded.split_at_checked(4).unwrap_or_default();
        decoder.try_push(half)?;
        fux_vt::refuse_next(1);
        assert_eq!(decoder.try_push(rest), Err(Error::Memory(4)));
        assert_eq!(decoder.buffered(), 4);
        fux_vt::refuse_next(1);
        assert_eq!(decoder.try_reserve(rest.len()), Err(Error::Memory(4)));
        decoder.try_push(rest)?;
        assert_eq!(decoder.frame::<ToCommand>()?, Some(frame));
        Ok(())
    }

    /// A client goes on with a server of its own protocol, whatever either's
    /// release; with one of another, it is told the server's protocol and
    /// release; a kill goes on with any.
    #[test]
    fn only_the_protocol_decides_a_mismatch() {
        let hello = |protocol, version: &str, role| Hello {
            protocol,
            version: version.into(),
            role,
        };
        let theirs = |protocol, version: &str| Mismatch {
            protocol,
            version: version.into(),
        };
        let client = hello(54, "0.36.0", Role::Command);
        let between = |server: &Hello| Mismatch::between(&client, server);
        assert_eq!(between(&hello(54, "0.36.0", Role::Command)), Ok(()));
        assert_eq!(between(&hello(54, "0.37.0", Role::Command)), Ok(()));
        let other = between(&hello(55, "0.36.0", Role::Command));
        assert_eq!(other, Err(theirs(55, "0.36.0")));
        let both = between(&hello(23, "0.37.0", Role::Command));
        assert_eq!(both, Err(theirs(23, "0.37.0")));
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
    use fux_vt::keys::Modifiers;
    use fux_vt::keys::mouse::{MouseAction, MouseButton, MouseEvent};

    /// `frame` encodes with kind byte `kind`, the wire format's, which a
    /// server and a client of another build read: pinned here as a
    /// number, apart from the names the code gives it. And it decodes to
    /// itself as a `$ty` from its bytes pushed one by one, as a slow socket
    /// would deliver them.
    macro_rules! round_trip {
        ($ty:ty, $kind:literal, $frame:expr) => {{
            let frame: $ty = $frame;
            let bytes = frame.encode();
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
        round_trip!(Greeting, 1, Greeting::Hello(hello(Role::Command)));
        let size = fux_vt::Size::from((std::num::NonZeroU16::MIN, std::num::NonZeroU16::MAX));
        let presentation = crate::presentation::sample()?;
        let workspace = Some(crate::command::WsRef::Name("main".to_owned()));
        round_trip!(
            Attach,
            2,
            Attach {
                of: None,
                headless: false,
                size,
                target: workspace.map(crate::command::AttachTarget::Workspace),
                read_only: false,
                initial: None,
                presentation: presentation.clone(),
                ancestry: Vec::new(),
            }
        );
        let workspace: Option<crate::command::WsRef> = None;
        round_trip!(
            Attach,
            2,
            Attach {
                of: None,
                headless: true,
                size,
                target: workspace.map(crate::command::AttachTarget::Workspace),
                read_only: true,
                initial: None,
                presentation: presentation.clone(),
                ancestry: Vec::new(),
            }
        );
        let act = Act::Paste("界\n".into());
        round_trip!(AttachedFrame, 3, AttachedFrame::Act(Box::new(act)));
        for act in [
            crate::act::ScreenAct::Show(crate::act::Panel::Repeat("resize".into())),
            crate::act::ScreenAct::Hide,
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
                    of: Some(run),
                    cwd: "/somewhere".into(),
                    pane: crate::id::program("%3:2"),
                    server: Some("1-2".into()),
                    client: "c2".parse().ok(),
                },
            }
        );
        let paint = ToAttached::Paint(Cow::Borrowed(&[0, 1, 2, 255]));
        round_trip!(ToAttached, 7, paint);
        round_trip!(Greeting, 8, Greeting::Exit("refused"));
        round_trip!(ToCommand, 8, ToCommand::Exit("stopped"));
        round_trip!(ToAttached, 8, ToAttached::Exit("detached"));
        round_trip!(ToAttached, 12, ToAttached::Welcome(ClientId::of(2), run));
        round_trip!(ToAttached, 14, ToAttached::Ended(7));
        round_trip!(
            ToAttached,
            15,
            ToAttached::Open(crate::callback::Open::Prompt(crate::callback::Prompt {
                title: "rename".into(),
                line: crate::act::Line::new("main".into()),
                action: None,
            }))
        );
        round_trip!(ToAttached, 16, ToAttached::Keys(Vec::new()));
        for target in [
            crate::command::AttachTarget::Tab(crate::id::TabId::of(2)),
            crate::command::AttachTarget::Pane(crate::id::PaneId::of(3)),
        ] {
            round_trip!(
                Attach,
                2,
                Attach {
                    target: Some(target),
                    ..Attach::of(size, presentation.clone())
                }
            );
        }
        round_trip!(ToCommand, 17, ToCommand::Part(Cow::Borrowed(b"reply")));
        round_trip!(ToCommand, 18, ToCommand::PartsEnd);
        // A Hello's bytes never change, so that a server of any version
        // reads it: the length, the kind, the protocol, the role, then the
        // version's length and bytes.
        let command = Hello {
            protocol: 9,
            version: "0.13.0".into(),
            role: Role::Command,
        };
        let head = [0, 0, 0, 16, 1, 0, 0, 0, 9, 1, 0, 0, 0, 6];
        assert_eq!(command.encode(), [&head[..], b"0.13.0"].concat());
        // Each role's byte is the wire format's too: after the length, the
        // kind and the protocol's four bytes. Byte 2 was a kill's, which an
        // older server takes whatever the protocol, so no role of this
        // protocol's has it; another protocol's byte decodes as it came.
        for (role, byte) in [(Role::Attach, 0), (Role::Command, 1), (Role::Unknown(3), 3)] {
            let encoded = hello(role).encode();
            assert_eq!(encoded.get(9), Some(&byte), "{role:?}");
            let mut decoder = Decoder::default();
            decoder.push(&encoded);
            assert_eq!(decoder.frame::<Hello>()?.map(|h| h.role), Some(role));
        }
        Ok(())
    }

    /// A frame longer than one goes in pieces that fit, and arrives
    /// whole, in order with the frames around it; a header announcing
    /// more than a frame is refused.
    #[test]
    fn a_long_frame_goes_in_pieces_and_arrives_whole() -> Result<(), Box<dyn std::error::Error>> {
        let text: String = std::iter::repeat_n('x', MAX_PAYLOAD * 2 + 3).collect();
        let long = AttachedFrame::Act(Box::new(Act::Paste(text)));
        let mut bytes = long.encode();
        AttachedFrame::Detach.encode_into(&mut bytes);
        let mut pending = bytes.as_slice();
        let mut kinds: Vec<u8> = Vec::new();
        while let Some(length) = whole(pending)? {
            kinds.extend(pending.get(4));
            pending = pending.get(length..).unwrap_or_default();
        }
        assert_eq!(kinds, [MORE, MORE, 3, 5]);
        let (mut decoder, mut joining) = (Decoder::default(), Joining::default());
        let (first, last) = bytes.split_at_checked(bytes.len() - 7).ok_or("short")?;
        decoder.push(first);
        assert!(!decoder.ready_long()?);
        assert!(decoder.long::<AttachedFrame>(&mut joining)?.is_none());
        decoder.push(last);
        assert!(decoder.ready_long()?);
        assert_eq!(decoder.long::<AttachedFrame>(&mut joining)?, Some(long));
        assert_eq!(
            decoder.long::<AttachedFrame>(&mut joining)?,
            Some(AttachedFrame::Detach)
        );
        assert_eq!((decoder.buffered(), joining.len()), (0, 0));
        // A reader that did not ask for something long refuses the first
        // piece, and holds no more than it.
        let mut decoder = Decoder::default();
        decoder.push(&bytes);
        assert!(decoder.ready()?);
        assert_eq!(decoder.frame::<AttachedFrame>(), Err(Error::Piece));
        let mut decoder = Decoder::default();
        // Any length past the limit will do.
        let over = u32::try_from(MAX_FRAME + 1).unwrap_or(u32::MAX);
        decoder.push(&over.to_be_bytes());
        assert!(decoder.ready().is_err());
        assert!(decoder.frame::<ToAttached>().is_err());
        // A longer paint is split into frames that fit, each a paint.
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
        Ok(())
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
                vec![0, 0, 0, 1, 1],
                "a frame's payload: the bytes end early",
            ),
            (
                greeting,
                vec![0, 0, 0, 11, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 9],
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
                "a frame's payload: the bytes end early",
            ),
            (
                attach,
                vec![0, 0, 0, 8, 2, 0, 0, 0, 1, 0, 1, 2],
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

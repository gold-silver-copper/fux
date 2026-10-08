//! The private protocol between a `fux` client and its server.
//!
//! A frame is `u32 length | u8 kind | payload`, big-endian, where `length`
//! counts the kind byte and the payload. A frame is at most `MAX_FRAME`; a
//! longer paint or output is split across frames by the sender.
//!
//! Each side decodes a frame once, as one of the frames it expects there
//! (`Hello`, `AttachFrame`, `Command`, `ServerFrame`), its bytes lent from
//! the decoder; any other kind, or a size of zero, does not decode.

use crate::bytes::ByteQueue;
use std::num::NonZeroU16;

/// Bumped on any change to the frames below.
pub const PROTOCOL: u32 = 2;
/// The largest frame, kind byte included.
pub const MAX_FRAME: usize = 1 << 20;
/// The largest payload one frame carries.
pub const MAX_PAYLOAD: usize = MAX_FRAME - 1;
/// Each frame's kind, its byte in the header: named once, for the encoder
/// and the decoder both.
mod kind {
    pub const HELLO: u8 = 1;
    pub const ATTACH: u8 = 2;
    pub const INPUT: u8 = 3;
    pub const RESIZE: u8 = 4;
    pub const DETACH: u8 = 5;
    pub const COMMAND: u8 = 6;
    pub const PAINT: u8 = 7;
    pub const EXIT: u8 = 8;
    pub const STDOUT: u8 = 9;
    pub const STDERR: u8 = 10;
    pub const DONE: u8 = 11;
    pub const TERMINAL: u8 = 12;
}

/// Why bytes are not a frame, or a frame cannot be sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A frame longer than the limit: one to send, or one a header
    /// announces.
    Oversized {
        bytes: usize,
        limit: usize,
    },
    /// A header announcing no bytes, not even the kind.
    Empty,
    /// A payload that ends before its fields do.
    Truncated,
    /// Bytes after a payload's last field.
    Trailing,
    /// A string field, or an exit reason, that is not UTF-8.
    NotUtf8,
    /// An option's marker, or a flag, that is neither 0 nor 1.
    BadMarker,
    /// More arguments than the payload has room for.
    BadCount,
    /// A size of no rows or no columns.
    ZeroSize,
    UnknownRole(u8),
    /// A kind unknown, or not one the frames decoded here may have.
    UnexpectedKind(u8),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Oversized { bytes, limit } => {
                write!(f, "a frame of {bytes} bytes exceeds the {limit}-byte limit")
            }
            Error::Empty => f.write_str("an empty frame"),
            Error::Truncated => f.write_str("truncated frame"),
            Error::Trailing => f.write_str("trailing bytes in frame"),
            Error::NotUtf8 => f.write_str("a frame string is not UTF-8"),
            Error::BadMarker => f.write_str("bad option marker in frame"),
            Error::BadCount => f.write_str("bad argument count in frame"),
            Error::ZeroSize => f.write_str("a frame's size has no rows or no columns"),
            Error::UnknownRole(role) => write!(f, "unknown client role {role}"),
            Error::UnexpectedKind(kind) => write!(f, "unexpected frame kind {kind}"),
        }
    }
}

impl std::error::Error for Error {}

/// What a connecting client is for; its discriminant is its byte in a
/// `Hello`'s payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
    /// An interactive client: `Attach`, then input.
    Attach = 0,
    /// One command, then its output.
    Command = 1,
    /// Stop the server. Accepted whatever the protocol version, so that
    /// `fux kill-server` can always stop a server from another fux version.
    Kill = 2,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hello<'a> {
    pub protocol: u32,
    pub version: &'a str,
    pub role: Role,
}

impl<'a> Frame<'a> for Hello<'a> {
    fn decode(kind: u8, payload: &'a [u8]) -> Result<Self, Error> {
        if kind != kind::HELLO {
            return Err(Error::UnexpectedKind(kind));
        }
        whole(payload, |r| {
            let protocol = r.u32()?;
            let byte = r.u8()?;
            let role = [Role::Attach, Role::Command, Role::Kill]
                .into_iter()
                .find(|role| *role as u8 == byte)
                .ok_or(Error::UnknownRole(byte))?;
            let version = r.str()?;
            Ok(Hello {
                protocol,
                version,
                role,
            })
        })
    }

    fn write(&self, out: &mut Vec<u8>) -> u8 {
        out.extend_from_slice(&self.protocol.to_be_bytes());
        out.push(self.role as u8);
        put_bytes(out, self.version.as_bytes());
        kind::HELLO
    }
}

/// An attaching client's frames, after its `Hello`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachFrame<'a> {
    Attach {
        rows: NonZeroU16,
        cols: NonZeroU16,
        workspace: Option<&'a str>,
    },
    Input(&'a [u8]),
    Resize {
        rows: NonZeroU16,
        cols: NonZeroU16,
    },
    Detach,
}

impl<'a> Frame<'a> for AttachFrame<'a> {
    fn decode(kind: u8, payload: &'a [u8]) -> Result<Self, Error> {
        whole(payload, |r| match kind {
            kind::ATTACH => Ok(AttachFrame::Attach {
                rows: r.size()?,
                cols: r.size()?,
                workspace: r.option()?,
            }),
            kind::INPUT => Ok(AttachFrame::Input(r.rest())),
            kind::RESIZE => Ok(AttachFrame::Resize {
                rows: r.size()?,
                cols: r.size()?,
            }),
            kind::DETACH => Ok(AttachFrame::Detach),
            other => Err(Error::UnexpectedKind(other)),
        })
    }

    fn write(&self, out: &mut Vec<u8>) -> u8 {
        match self {
            AttachFrame::Attach {
                rows,
                cols,
                workspace,
            } => {
                out.extend_from_slice(&rows.get().to_be_bytes());
                out.extend_from_slice(&cols.get().to_be_bytes());
                put_option(out, *workspace);
                kind::ATTACH
            }
            AttachFrame::Input(bytes) => {
                out.extend_from_slice(bytes);
                kind::INPUT
            }
            AttachFrame::Resize { rows, cols } => {
                out.extend_from_slice(&rows.get().to_be_bytes());
                out.extend_from_slice(&cols.get().to_be_bytes());
                kind::RESIZE
            }
            AttachFrame::Detach => kind::DETACH,
        }
    }
}

/// A command client's one frame, after its `Hello`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub argv: Vec<String>,
    pub cwd: String,
    pub pane: Option<String>,
}

impl Frame<'_> for Command {
    fn decode(kind: u8, payload: &[u8]) -> Result<Self, Error> {
        whole(payload, |r| match kind {
            kind::COMMAND => {
                let count = r.u32()?;
                // Each argument takes at least its 4-byte length.
                if count as usize > payload.len() / 4 {
                    return Err(Error::BadCount);
                }
                Ok(Command {
                    argv: (0..count)
                        .map(|_| r.str().map(str::to_owned))
                        .collect::<Result<_, _>>()?,
                    cwd: r.str()?.to_owned(),
                    pane: r.option()?.map(str::to_owned),
                })
            }
            other => Err(Error::UnexpectedKind(other)),
        })
    }

    fn write(&self, out: &mut Vec<u8>) -> u8 {
        put_len(out, self.argv.len());
        for arg in &self.argv {
            put_bytes(out, arg.as_bytes());
        }
        put_bytes(out, self.cwd.as_bytes());
        put_option(out, self.pane.as_deref());
        kind::COMMAND
    }
}

/// The server's frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerFrame<'a> {
    /// Its answer to the client's.
    Hello(Hello<'a>),
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
        if kind == kind::HELLO {
            return Hello::decode(kind, payload).map(ServerFrame::Hello);
        }
        whole(payload, |r| match kind {
            kind::PAINT => Ok(ServerFrame::Paint(r.rest())),
            kind::EXIT => std::str::from_utf8(r.rest())
                .map(ServerFrame::Exit)
                .map_err(|_| Error::NotUtf8),
            kind::STDOUT => Ok(ServerFrame::Stdout(r.rest())),
            kind::STDERR => Ok(ServerFrame::Stderr(r.rest())),
            kind::DONE => Ok(ServerFrame::Done { status: r.u8()? }),
            kind::TERMINAL => Ok(ServerFrame::Terminal { taken: r.flag()? }),
            other => Err(Error::UnexpectedKind(other)),
        })
    }

    fn write(&self, out: &mut Vec<u8>) -> u8 {
        let (kind, payload) = match self {
            ServerFrame::Hello(hello) => return hello.write(out),
            ServerFrame::Paint(bytes) => (kind::PAINT, *bytes),
            ServerFrame::Exit(reason) => (kind::EXIT, reason.as_bytes()),
            ServerFrame::Stdout(bytes) => (kind::STDOUT, *bytes),
            ServerFrame::Stderr(bytes) => (kind::STDERR, *bytes),
            ServerFrame::Done { status } => (kind::DONE, std::slice::from_ref(status)),
            ServerFrame::Terminal { taken: false } => (kind::TERMINAL, &[0][..]),
            ServerFrame::Terminal { taken: true } => (kind::TERMINAL, &[1][..]),
        };
        out.extend_from_slice(payload);
        kind
    }
}

fn put_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes());
}
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_len(out, bytes.len());
    out.extend_from_slice(bytes);
}
fn put_option(out: &mut Vec<u8>, value: Option<&str>) {
    out.push(u8::from(value.is_some()));
    if let Some(value) = value {
        put_bytes(out, value.as_bytes());
    }
}

/// A cursor over a payload being decoded, lending what it reads.
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (head, rest) = self.0.split_first_chunk().ok_or(Error::Truncated)?;
        self.0 = rest;
        Ok(*head)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        self.bytes().map(u8::from_be_bytes)
    }
    fn u32(&mut self) -> Result<u32, Error> {
        self.bytes().map(u32::from_be_bytes)
    }
    fn size(&mut self) -> Result<NonZeroU16, Error> {
        NonZeroU16::new(u16::from_be_bytes(self.bytes()?)).ok_or(Error::ZeroSize)
    }
    fn flag(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::BadMarker),
        }
    }
    fn str(&mut self) -> Result<&'a str, Error> {
        let len = self.u32()? as usize;
        let (head, rest) = self.0.split_at_checked(len).ok_or(Error::Truncated)?;
        self.0 = rest;
        std::str::from_utf8(head).map_err(|_| Error::NotUtf8)
    }
    fn option(&mut self) -> Result<Option<&'a str>, Error> {
        self.flag()?.then(|| self.str()).transpose()
    }
    /// The rest of the payload, all of it a byte field.
    fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.0)
    }
}

/// What `read` reads from `payload`, which must be all of it.
fn whole<'a, T>(
    payload: &'a [u8],
    read: impl FnOnce(&mut Reader<'a>) -> Result<T, Error>,
) -> Result<T, Error> {
    let mut r = Reader(payload);
    let value = read(&mut r)?;
    r.0.is_empty().then_some(value).ok_or(Error::Trailing)
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

    fn size(n: u16) -> NonZeroU16 {
        NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
    }

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
            version: "0.13.0",
            role,
        };
        round_trip!(Hello, 1, hello(Role::Attach));
        round_trip!(ServerFrame, 1, ServerFrame::Hello(hello(Role::Kill)));
        let (rows, cols) = (size(24), size(80));
        let workspace = Some("main");
        round_trip!(
            AttachFrame,
            2,
            AttachFrame::Attach {
                rows,
                cols,
                workspace
            }
        );
        let workspace = None;
        round_trip!(
            AttachFrame,
            2,
            AttachFrame::Attach {
                rows,
                cols,
                workspace
            }
        );
        round_trip!(AttachFrame, 3, AttachFrame::Input(b"\x1b[A"));
        round_trip!(AttachFrame, 4, AttachFrame::Resize { rows, cols });
        round_trip!(AttachFrame, 5, AttachFrame::Detach);
        round_trip!(
            Command,
            6,
            Command {
                argv: vec!["split".into(), "-h".into(), "".into(), "界".into()],
                cwd: "/tmp".into(),
                pane: Some("%3".into()),
            }
        );
        round_trip!(ServerFrame, 7, ServerFrame::Paint(&[0, 1, 2, 255]));
        round_trip!(ServerFrame, 8, ServerFrame::Exit("detached"));
        round_trip!(ServerFrame, 9, ServerFrame::Stdout(b"out"));
        round_trip!(ServerFrame, 10, ServerFrame::Stderr(&[]));
        round_trip!(ServerFrame, 11, ServerFrame::Done { status: 2 });
        round_trip!(ServerFrame, 12, ServerFrame::Terminal { taken: true });
        round_trip!(ServerFrame, 12, ServerFrame::Terminal { taken: false });
        // Each role's byte is the wire format's too: after the length, the
        // kind and the protocol's four bytes.
        for (role, byte) in [(Role::Attach, 0), (Role::Command, 1), (Role::Kill, 2)] {
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
        let attach: Decode = |d| d.frame::<AttachFrame>().map(drop);
        let command: Decode = |d| d.frame::<Command>().map(drop);
        let utf8 = "a frame string is not UTF-8";
        for (decode, bytes, message) in [
            (server, vec![0, 0, 0, 1, 99], "unexpected frame kind 99"),
            // A server's frame sent to a server, and a client's before its
            // Hello.
            (attach, vec![0, 0, 0, 1, 7], "unexpected frame kind 7"),
            (hello, vec![0, 0, 0, 1, 5], "unexpected frame kind 5"),
            (server, vec![0, 0, 0, 1, 11], "truncated frame"),
            (
                server,
                vec![0, 0, 0, 3, 11, 0, 0],
                "trailing bytes in frame",
            ),
            (
                command,
                vec![0, 0, 0, 5, 6, 255, 255, 255, 255],
                "bad argument count in frame",
            ),
            (server, vec![0, 0, 0, 0], "an empty frame"),
            (server, vec![0, 0, 0, 2, 8, 0xff], utf8),
            (
                hello,
                vec![0, 0, 0, 6, 1, 0, 0, 0, 1, 7],
                "unknown client role 7",
            ),
            (
                attach,
                vec![0, 0, 0, 6, 2, 0, 1, 0, 1, 2],
                "bad option marker in frame",
            ),
            (
                attach,
                vec![0, 0, 0, 5, 4, 0, 0, 0, 1],
                "a frame's size has no rows or no columns",
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

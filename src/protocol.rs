//! The private protocol between a `fux` client and its server.
//!
//! A frame is `u32 length | u8 kind | payload`, big-endian, where `length`
//! counts the kind byte and the payload. A frame is at most `MAX_FRAME`; a
//! longer paint or output is split across frames by the sender.

use crate::bytes::ByteQueue;

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

/// A `Hello`'s role, its byte in the payload.
fn role_byte(role: Role) -> u8 {
    match role {
        Role::Attach => 0,
        Role::Command => 1,
        Role::Kill => 2,
    }
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
    /// A string field that is not UTF-8.
    NotUtf8,
    /// An exit reason that is not UTF-8.
    ExitNotUtf8,
    /// An option's marker that is neither 0 nor 1.
    BadMarker,
    /// More arguments than the payload has room for.
    BadCount,
    UnknownRole(u8),
    UnknownKind(u8),
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
            Error::ExitNotUtf8 => f.write_str("an exit reason is not UTF-8"),
            Error::BadMarker => f.write_str("bad option marker in frame"),
            Error::BadCount => f.write_str("bad argument count in frame"),
            Error::UnknownRole(role) => write!(f, "unknown client role {role}"),
            Error::UnknownKind(kind) => write!(f, "unknown frame kind {kind}"),
        }
    }
}

impl std::error::Error for Error {}

/// What a connecting client is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// An interactive client: `Attach`, then input.
    Attach,
    /// One command, then its output.
    Command,
    /// Stop the server. Accepted whatever the protocol version, so that
    /// `fux kill-server` can always stop a server from another fux version.
    Kill,
}

/// The frames that carry a stream of bytes, server to client: a paint of
/// any length, or a command's output, split across as many frames as it
/// takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Paint,
    Stdout,
    Stderr,
}

impl Stream {
    fn kind(self) -> u8 {
        match self {
            Stream::Paint => kind::PAINT,
            Stream::Stdout => kind::STDOUT,
            Stream::Stderr => kind::STDERR,
        }
    }

    /// Writes `bytes` into `out` as frames of this stream, each a header and
    /// then its piece of `bytes`, split so that each fits. No bytes, no
    /// frames.
    pub fn encode_into(self, bytes: &[u8], out: &mut ByteQueue) {
        let (whole, rest) = bytes.as_chunks::<MAX_PAYLOAD>();
        for piece in whole
            .iter()
            .map(|piece| piece.as_slice())
            .chain((!rest.is_empty()).then_some(rest))
        {
            // Exact: a piece is at most MAX_PAYLOAD, so the kind byte and the
            // piece fit a u32.
            let length = u32::try_from(piece.len().saturating_add(1)).unwrap_or(u32::MAX);
            let [a, b, c, d] = length.to_be_bytes();
            out.push(&[a, b, c, d, self.kind()]);
            out.push(piece);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Both ways: the first frame from each side.
    Hello {
        protocol: u32,
        version: String,
        role: Role,
    },
    /// client → server.
    Attach {
        rows: u16,
        cols: u16,
        workspace: Option<String>,
    },
    /// client → server.
    Input(Vec<u8>),
    /// client → server.
    Resize { rows: u16, cols: u16 },
    /// client → server.
    Detach,
    /// client → server.
    Command {
        argv: Vec<String>,
        cwd: String,
        pane: Option<String>,
    },
    /// server → client.
    Paint(Vec<u8>),
    /// server → client.
    Exit(String),
    /// server → client.
    Stdout(Vec<u8>),
    /// server → client.
    Stderr(Vec<u8>),
    /// server → client.
    Done { status: u8 },
    /// server → client, first after an `Attach` sent with the client's
    /// terminal (`fuxix::socket::send_with_fd`): whether the server took
    /// it, and reads the keys and writes the paints there itself. If it did
    /// not, the client relays them in frames, as one that sent no terminal
    /// does.
    Terminal { taken: bool },
}

impl Frame {
    fn kind(&self) -> u8 {
        match self {
            Frame::Hello { .. } => kind::HELLO,
            Frame::Attach { .. } => kind::ATTACH,
            Frame::Input(_) => kind::INPUT,
            Frame::Resize { .. } => kind::RESIZE,
            Frame::Detach => kind::DETACH,
            Frame::Command { .. } => kind::COMMAND,
            Frame::Paint(_) => kind::PAINT,
            Frame::Exit(_) => kind::EXIT,
            Frame::Stdout(_) => kind::STDOUT,
            Frame::Stderr(_) => kind::STDERR,
            Frame::Done { .. } => kind::DONE,
            Frame::Terminal { .. } => kind::TERMINAL,
        }
    }

    /// The encoded frame, or an error if its payload exceeds `MAX_PAYLOAD`.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        self.encode_into(&mut out)?;
        Ok(out)
    }

    /// Appends the encoded frame to `out`: the header, then the payload,
    /// written in place, and the length patched in once it is known. A
    /// payload over `MAX_PAYLOAD` is an error, and leaves `out` as it was.
    pub fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let start = out.len();
        out.extend_from_slice(&[0; 4]);
        out.push(self.kind());
        match self {
            Frame::Hello {
                protocol,
                version,
                role,
            } => {
                out.extend_from_slice(&protocol.to_be_bytes());
                out.push(role_byte(*role));
                put_bytes(out, version.as_bytes());
            }
            Frame::Attach {
                rows,
                cols,
                workspace,
            } => {
                out.extend_from_slice(&rows.to_be_bytes());
                out.extend_from_slice(&cols.to_be_bytes());
                put_option(out, workspace.as_deref());
            }
            Frame::Input(bytes)
            | Frame::Paint(bytes)
            | Frame::Stdout(bytes)
            | Frame::Stderr(bytes) => {
                out.extend_from_slice(bytes);
            }
            Frame::Resize { rows, cols } => {
                out.extend_from_slice(&rows.to_be_bytes());
                out.extend_from_slice(&cols.to_be_bytes());
            }
            Frame::Detach => {}
            Frame::Command { argv, cwd, pane } => {
                put_len(out, argv.len());
                for arg in argv {
                    put_bytes(out, arg.as_bytes());
                }
                put_bytes(out, cwd.as_bytes());
                put_option(out, pane.as_deref());
            }
            Frame::Exit(reason) => out.extend_from_slice(reason.as_bytes()),
            Frame::Done { status } => out.push(*status),
            Frame::Terminal { taken } => out.push(u8::from(*taken)),
        }
        // The payload is what follows the four length bytes and the kind.
        let payload = out.len().saturating_sub(start).saturating_sub(5);
        if payload > MAX_PAYLOAD {
            out.truncate(start);
            return Err(Error::Oversized {
                bytes: payload,
                limit: MAX_PAYLOAD,
            });
        }
        // The kind byte and the payload; exact, as the payload is at most
        // MAX_PAYLOAD.
        let length = u32::try_from(payload.saturating_add(1)).unwrap_or(u32::MAX);
        if let Some(header) = out.get_mut(start..).and_then(|f| f.first_chunk_mut::<4>()) {
            *header = length.to_be_bytes();
        }
        Ok(())
    }
}

/// Appends `bytes` to `out` as the `Input` frame `Frame::Input` would
/// encode, without their being copied into one first. A payload over
/// `MAX_PAYLOAD` is an error, and leaves `out` as it was.
pub fn encode_input(bytes: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    if bytes.len() > MAX_PAYLOAD {
        return Err(Error::Oversized {
            bytes: bytes.len(),
            limit: MAX_PAYLOAD,
        });
    }
    // The kind byte and the payload; exact, as the payload is at most
    // MAX_PAYLOAD.
    let length = u32::try_from(bytes.len().saturating_add(1)).unwrap_or(u32::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.push(kind::INPUT);
    out.extend_from_slice(bytes);
    Ok(())
}

fn put_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes());
}
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_len(out, bytes.len());
    out.extend_from_slice(bytes);
}
fn put_option(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            out.push(1);
            put_bytes(out, value.as_bytes());
        }
        None => out.push(0),
    }
}

/// A cursor over a payload being decoded.
struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], Error> {
        let (head, rest) = self.0.split_at_checked(n).ok_or(Error::Truncated)?;
        self.0 = rest;
        Ok(head)
    }
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (head, rest) = self.0.split_first_chunk().ok_or(Error::Truncated)?;
        self.0 = rest;
        Ok(*head)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        self.bytes().map(u8::from_be_bytes)
    }
    fn u16(&mut self) -> Result<u16, Error> {
        self.bytes().map(u16::from_be_bytes)
    }
    fn u32(&mut self) -> Result<u32, Error> {
        self.bytes().map(u32::from_be_bytes)
    }
    fn string(&mut self) -> Result<String, Error> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::NotUtf8)
    }
    fn option(&mut self) -> Result<Option<String>, Error> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.string()?)),
            _ => Err(Error::BadMarker),
        }
    }
    fn end(&self) -> Result<(), Error> {
        self.0.is_empty().then_some(()).ok_or(Error::Trailing)
    }
}

fn decode_frame(kind: u8, payload: &[u8]) -> Result<Frame, Error> {
    let mut r = Reader(payload);
    let frame = match kind {
        kind::HELLO => {
            let protocol = r.u32()?;
            let byte = r.u8()?;
            let role = [Role::Attach, Role::Command, Role::Kill]
                .into_iter()
                .find(|role| role_byte(*role) == byte)
                .ok_or(Error::UnknownRole(byte))?;
            let version = r.string()?;
            Frame::Hello {
                protocol,
                version,
                role,
            }
        }
        kind::ATTACH => Frame::Attach {
            rows: r.u16()?,
            cols: r.u16()?,
            workspace: r.option()?,
        },
        kind::INPUT => return Ok(Frame::Input(payload.to_vec())),
        kind::RESIZE => Frame::Resize {
            rows: r.u16()?,
            cols: r.u16()?,
        },
        kind::DETACH => Frame::Detach,
        kind::COMMAND => {
            let count = r.u32()? as usize;
            // Each argument takes at least its 4-byte length.
            if count > payload.len() / 4 {
                return Err(Error::BadCount);
            }
            let mut argv = Vec::with_capacity(count);
            for _ in 0..count {
                argv.push(r.string()?);
            }
            Frame::Command {
                argv,
                cwd: r.string()?,
                pane: r.option()?,
            }
        }
        kind::PAINT => return Ok(Frame::Paint(payload.to_vec())),
        kind::EXIT => {
            return String::from_utf8(payload.to_vec())
                .map(Frame::Exit)
                .map_err(|_| Error::ExitNotUtf8);
        }
        kind::STDOUT => return Ok(Frame::Stdout(payload.to_vec())),
        kind::STDERR => return Ok(Frame::Stderr(payload.to_vec())),
        kind::DONE => Frame::Done { status: r.u8()? },
        kind::TERMINAL => Frame::Terminal {
            taken: match r.u8()? {
                0 => false,
                1 => true,
                _ => return Err(Error::BadMarker),
            },
        },
        other => return Err(Error::UnknownKind(other)),
    };
    r.end()?;
    Ok(frame)
}

/// A whole frame not yet decoded: its kind, and its payload lent from the
/// decoder.
pub struct Raw<'a> {
    kind: u8,
    payload: &'a [u8],
}

impl<'a> Raw<'a> {
    /// A paint's bytes, as they are, without copying them.
    pub fn paint(&self) -> Option<&'a [u8]> {
        (self.kind == kind::PAINT).then_some(self.payload)
    }

    /// An input's bytes, as they are, without copying them.
    pub fn input(&self) -> Option<&'a [u8]> {
        (self.kind == kind::INPUT).then_some(self.payload)
    }

    pub fn decode(&self) -> Result<Frame, Error> {
        decode_frame(self.kind, self.payload)
    }
}

/// What `Decoder::check` found.
pub struct Checked {
    /// Where, in what is not yet taken, the last whole frame checked ends.
    pub end: usize,
    /// How many whole frames were found good.
    pub frames: usize,
    /// The error of the bad frame after them, if one is.
    pub error: Option<Error>,
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

    /// The next whole frame, `Ok(None)` if more bytes are needed, or an error
    /// for a frame that is oversized or malformed; the stream is then unusable.
    pub fn frame(&mut self) -> Result<Option<Frame>, Error> {
        self.raw()?.map(|raw| raw.decode()).transpose()
    }

    /// The next whole frame, taken but not decoded: its payload is lent
    /// from the decoder, not copied. `Ok(None)` if more bytes are needed;
    /// an error for a frame that is oversized, and the stream is then
    /// unusable.
    pub fn raw(&mut self) -> Result<Option<Raw<'_>>, Error> {
        let pending = self.buffer.as_slice();
        let Some(header) = pending.first_chunk::<4>() else {
            return Ok(None);
        };
        let length = u32::from_be_bytes(*header) as usize;
        if length == 0 {
            return Err(Error::Empty);
        }
        if length > MAX_FRAME {
            return Err(Error::Oversized {
                bytes: length,
                limit: MAX_FRAME,
            });
        }
        if pending.len() < 4 + length {
            return Ok(None);
        }
        let frame = self.buffer.take_front(4 + length);
        let (kind, payload) = frame
            .get(4..)
            .and_then(<[u8]>::split_first)
            .unwrap_or((&0, &[]));
        Ok(Some(Raw {
            kind: *kind,
            payload,
        }))
    }

    /// Checks the whole frames held from byte `from` of what is not yet
    /// taken, as `frame` would decode them, taking nothing: where the last
    /// whole one ends, how many there are, and the first bad one's error,
    /// where the check stops. An input's payload is any bytes, so it is
    /// not copied to be checked.
    pub fn check(&self, from: usize) -> Checked {
        let mut checked = Checked {
            end: from,
            frames: 0,
            error: None,
        };
        let pending = self.buffer.as_slice();
        while let Some(rest) = pending.get(checked.end..) {
            let Some(header) = rest.first_chunk::<4>() else {
                break;
            };
            let length = u32::from_be_bytes(*header) as usize;
            if length == 0 {
                checked.error = Some(Error::Empty);
                break;
            }
            if length > MAX_FRAME {
                checked.error = Some(Error::Oversized {
                    bytes: length,
                    limit: MAX_FRAME,
                });
                break;
            }
            // At most `4 + MAX_FRAME`.
            let whole = length.saturating_add(4);
            let Some((&frame_kind, payload)) = rest.get(4..whole).and_then(<[u8]>::split_first)
            else {
                break;
            };
            if frame_kind != kind::INPUT
                && let Err(error) = decode_frame(frame_kind, payload)
            {
                checked.error = Some(error);
                break;
            }
            checked.frames = checked.frames.saturating_add(1);
            checked.end = checked.end.saturating_add(whole);
        }
        checked
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

    /// Input framed from its bytes is the frame `Frame::Input` encodes,
    /// after whatever the buffer held, and too long an input is refused
    /// alike, leaving the buffer as it was.
    #[test]
    fn input_is_framed_from_its_bytes_as_its_frame_is() {
        let long = vec![0xff; 70_000];
        for bytes in [&b""[..], b"k", b"\x1b[A\x1b[B", &long] {
            let mut out = b"held".to_vec();
            assert_eq!(encode_input(bytes, &mut out), Ok(()));
            let frame = Frame::Input(bytes.to_vec()).encode();
            assert_eq!(out.get(4..), frame.as_deref().ok());
        }
        let long = vec![0; MAX_PAYLOAD + 1];
        let mut out = b"held".to_vec();
        assert_eq!(
            encode_input(&long, &mut out).err(),
            Frame::Input(long.clone()).encode().err()
        );
        assert_eq!(out, b"held");
    }

    fn round_trip(frame: Frame) {
        let bytes = frame.encode().unwrap_or_default();
        // Byte by byte, as a slow socket would deliver it.
        let mut decoder = Decoder::default();
        let mut out = Vec::new();
        for byte in &bytes {
            decoder.push(&[*byte]);
            while let Ok(Some(frame)) = decoder.frame() {
                out.push(frame);
            }
        }
        assert_eq!(out, vec![frame]);
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn every_frame_round_trips_in_any_chunking() {
        round_trip(Frame::Hello {
            protocol: PROTOCOL,
            version: "0.13.0".into(),
            role: Role::Command,
        });
        round_trip(Frame::Attach {
            rows: 24,
            cols: 80,
            workspace: Some("main".into()),
        });
        round_trip(Frame::Attach {
            rows: 1,
            cols: 1,
            workspace: None,
        });
        round_trip(Frame::Input(b"\x1b[A".to_vec()));
        round_trip(Frame::Resize {
            rows: 50,
            cols: 200,
        });
        round_trip(Frame::Detach);
        round_trip(Frame::Command {
            argv: vec!["split".into(), "-h".into(), "".into(), "界".into()],
            cwd: "/tmp".into(),
            pane: Some("%3".into()),
        });
        round_trip(Frame::Paint(vec![0, 1, 2, 255]));
        round_trip(Frame::Exit("detached".into()));
        round_trip(Frame::Stdout(b"out".to_vec()));
        round_trip(Frame::Stderr(Vec::new()));
        round_trip(Frame::Done { status: 2 });
        round_trip(Frame::Terminal { taken: true });
        round_trip(Frame::Terminal { taken: false });
    }

    /// Each frame's kind byte and each role's are the wire format's, which
    /// a server and a client of another build read: pinned here as numbers,
    /// apart from the names the code gives them.
    #[test]
    fn frame_kinds_and_roles_are_the_wire_formats() {
        let hello = |role| Frame::Hello {
            protocol: PROTOCOL,
            version: String::new(),
            role,
        };
        let kinds = [
            (hello(Role::Attach), 1),
            (
                Frame::Attach {
                    rows: 1,
                    cols: 1,
                    workspace: None,
                },
                2,
            ),
            (Frame::Input(Vec::new()), 3),
            (Frame::Resize { rows: 1, cols: 1 }, 4),
            (Frame::Detach, 5),
            (
                Frame::Command {
                    argv: Vec::new(),
                    cwd: String::new(),
                    pane: None,
                },
                6,
            ),
            (Frame::Paint(Vec::new()), 7),
            (Frame::Exit(String::new()), 8),
            (Frame::Stdout(Vec::new()), 9),
            (Frame::Stderr(Vec::new()), 10),
            (Frame::Done { status: 0 }, 11),
            (Frame::Terminal { taken: true }, 12),
        ];
        for (frame, kind) in kinds {
            let encoded = frame.encode().unwrap_or_default();
            assert_eq!(encoded.get(4), Some(&kind), "{frame:?}");
        }
        for (role, byte) in [(Role::Attach, 0), (Role::Command, 1), (Role::Kill, 2)] {
            let encoded = hello(role).encode().unwrap_or_default();
            // The length, the kind, the protocol's four bytes, then the role.
            assert_eq!(encoded.get(9), Some(&byte), "{role:?}");
        }
    }

    #[test]
    fn oversized_frames_are_refused_on_both_sides() {
        assert!(Frame::Paint(vec![0; MAX_PAYLOAD + 1]).encode().is_err());
        assert!(Frame::Paint(vec![0; MAX_PAYLOAD]).encode().is_ok());
        let mut decoder = Decoder::default();
        // Any length past the limit will do.
        let over = u32::try_from(MAX_FRAME + 1).unwrap_or(u32::MAX);
        decoder.push(&over.to_be_bytes());
        assert!(decoder.frame().is_err());
        // A longer paint is split into frames that fit.
        let mut queue = ByteQueue::default();
        Stream::Paint.encode_into(&vec![7; MAX_PAYLOAD * 2 + 3], &mut queue);
        let mut decoder = Decoder::default();
        decoder.push(queue.as_slice());
        let mut frames = 0;
        while let Ok(Some(Frame::Paint(_))) = decoder.frame() {
            frames += 1;
        }
        assert_eq!(frames, 3);
        assert_eq!(decoder.buffered(), 0);
    }

    /// Frames as they were encoded before `encode_into`: the payload built
    /// alone, then copied after its header.
    fn reference(frame: &Frame) -> Vec<u8> {
        let mut payload = Vec::new();
        match frame {
            Frame::Hello {
                protocol,
                version,
                role,
            } => {
                payload.extend_from_slice(&protocol.to_be_bytes());
                payload.push(match role {
                    Role::Attach => 0,
                    Role::Command => 1,
                    Role::Kill => 2,
                });
                put_bytes(&mut payload, version.as_bytes());
            }
            Frame::Attach {
                rows,
                cols,
                workspace,
            } => {
                payload.extend_from_slice(&rows.to_be_bytes());
                payload.extend_from_slice(&cols.to_be_bytes());
                put_option(&mut payload, workspace.as_deref());
            }
            Frame::Input(bytes)
            | Frame::Paint(bytes)
            | Frame::Stdout(bytes)
            | Frame::Stderr(bytes) => payload.extend_from_slice(bytes),
            Frame::Resize { rows, cols } => {
                payload.extend_from_slice(&rows.to_be_bytes());
                payload.extend_from_slice(&cols.to_be_bytes());
            }
            Frame::Detach => {}
            Frame::Command { argv, cwd, pane } => {
                put_len(&mut payload, argv.len());
                for arg in argv {
                    put_bytes(&mut payload, arg.as_bytes());
                }
                put_bytes(&mut payload, cwd.as_bytes());
                put_option(&mut payload, pane.as_deref());
            }
            Frame::Exit(reason) => payload.extend_from_slice(reason.as_bytes()),
            Frame::Done { status } => payload.push(*status),
            Frame::Terminal { taken } => payload.push(u8::from(*taken)),
        }
        let length = u32::try_from(payload.len().saturating_add(1)).unwrap_or(u32::MAX);
        let mut out = length.to_be_bytes().to_vec();
        out.push(frame.kind());
        out.extend_from_slice(&payload);
        out
    }

    /// The bytes on the wire are what they were, for every kind of frame
    /// and every size of stream.
    #[test]
    fn frames_encode_as_they_did() {
        let frames = [
            Frame::Hello {
                protocol: PROTOCOL,
                version: "0.13.0".into(),
                role: Role::Kill,
            },
            Frame::Attach {
                rows: 24,
                cols: 80,
                workspace: Some("main".into()),
            },
            Frame::Attach {
                rows: 1,
                cols: 1,
                workspace: None,
            },
            Frame::Input(b"\x1b[A".to_vec()),
            Frame::Resize {
                rows: 50,
                cols: 200,
            },
            Frame::Detach,
            Frame::Command {
                argv: vec!["split".into(), "-h".into(), "".into(), "界".into()],
                cwd: "/tmp".into(),
                pane: Some("%3".into()),
            },
            Frame::Paint(vec![0, 1, 2, 255]),
            Frame::Exit("detached".into()),
            Frame::Stdout(b"out".to_vec()),
            Frame::Stderr(Vec::new()),
            Frame::Done { status: 2 },
            Frame::Terminal { taken: true },
            Frame::Terminal { taken: false },
        ];
        for frame in &frames {
            let mut out = b"before".to_vec();
            assert!(frame.encode_into(&mut out).is_ok(), "{frame:?}");
            let expected: Vec<u8> = b"before".iter().copied().chain(reference(frame)).collect();
            assert_eq!(out, expected, "{frame:?}");
            assert_eq!(frame.encode().ok(), Some(reference(frame)), "{frame:?}");
        }
        // Too long a payload is refused, and nothing is left behind.
        let mut out = b"before".to_vec();
        assert!(
            Frame::Stdout(vec![0; MAX_PAYLOAD + 1])
                .encode_into(&mut out)
                .is_err()
        );
        assert_eq!(out, b"before");
        for stream in [Stream::Paint, Stream::Stdout, Stream::Stderr] {
            let make = match stream {
                Stream::Paint => Frame::Paint,
                Stream::Stdout => Frame::Stdout,
                Stream::Stderr => Frame::Stderr,
            };
            for len in [0, 1, MAX_PAYLOAD, 2 * MAX_PAYLOAD + 3] {
                let bytes: Vec<u8> = (0..len)
                    .map(|i| u8::try_from(i % 251).unwrap_or(0))
                    .collect();
                let mut queue = ByteQueue::default();
                queue.push(b"before");
                stream.encode_into(&bytes, &mut queue);
                // The frames `encode_into` made: whole ones, then the rest.
                let (whole, rest) = bytes.as_chunks::<MAX_PAYLOAD>();
                let expected: Vec<u8> = b"before"
                    .iter()
                    .copied()
                    .chain(
                        whole
                            .iter()
                            .map(|piece| piece.as_slice())
                            .chain((!rest.is_empty()).then_some(rest))
                            .flat_map(|piece| reference(&make(piece.to_vec()))),
                    )
                    .collect();
                assert!(queue.as_slice() == expected, "{stream:?} of {len} bytes");
            }
        }
    }

    /// A paint is lent from the decoder as it arrived; other frames decode.
    #[test]
    fn raw_frames_lend_a_paint_and_decode_the_rest() -> Result<(), Box<dyn std::error::Error>> {
        let mut decoder = Decoder::default();
        decoder.push(&Frame::Paint(b"\x1b[Hhi".to_vec()).encode()?);
        decoder.push(&Frame::Exit("detached".into()).encode()?);
        let raw = decoder.raw()?.ok_or("a paint")?;
        assert_eq!(raw.paint(), Some(&b"\x1b[Hhi"[..]));
        let raw = decoder.raw()?.ok_or("an exit")?;
        assert_eq!(raw.paint(), None);
        assert_eq!(raw.decode()?, Frame::Exit("detached".into()));
        assert!(decoder.raw()?.is_none());
        assert_eq!(decoder.buffered(), 0);
        Ok(())
    }

    #[test]
    fn malformed_frames_are_errors_not_panics() {
        for (bytes, error, message) in [
            (
                vec![0, 0, 0, 1, 99],
                Error::UnknownKind(99),
                "unknown frame kind 99",
            ),
            (vec![0, 0, 0, 1, 11], Error::Truncated, "truncated frame"),
            (
                vec![0, 0, 0, 3, 11, 0, 0],
                Error::Trailing,
                "trailing bytes in frame",
            ),
            (
                vec![0, 0, 0, 5, 6, 255, 255, 255, 255],
                Error::BadCount,
                "bad argument count in frame",
            ),
            (vec![0, 0, 0, 0], Error::Empty, "an empty frame"),
            (
                vec![0, 0, 0, 2, 8, 0xff],
                Error::ExitNotUtf8,
                "an exit reason is not UTF-8",
            ),
            (
                vec![0, 0, 0, 3, 1, 0, 0],
                Error::Truncated,
                "truncated frame",
            ),
            (
                vec![0, 0, 0, 6, 1, 0, 0, 0, 1, 7],
                Error::UnknownRole(7),
                "unknown client role 7",
            ),
            (
                vec![0, 0, 0, 6, 2, 0, 1, 0, 1, 2],
                Error::BadMarker,
                "bad option marker in frame",
            ),
            (
                vec![0, 0, 0, 11, 1, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0xff],
                Error::NotUtf8,
                "a frame string is not UTF-8",
            ),
        ] {
            let mut decoder = Decoder::default();
            decoder.push(&bytes);
            let got = decoder.frame();
            assert_eq!(got, Err(error), "{bytes:?}");
            assert_eq!(got.map_err(|e| e.to_string()), Err(message.to_owned()));
        }
        assert_eq!(
            Frame::Paint(vec![0; MAX_PAYLOAD + 1])
                .encode()
                .map_err(|e| e.to_string()),
            Err(format!(
                "a frame of {} bytes exceeds the {MAX_PAYLOAD}-byte limit",
                MAX_PAYLOAD + 1
            ))
        );
    }
}

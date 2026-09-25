//! The private protocol between a `fux` client and its server.
//!
//! A frame is `u32 length | u8 kind | payload`, big-endian, where `length`
//! counts the kind byte and the payload. A frame is at most `MAX_FRAME`; a
//! longer paint or output is split across frames by the sender.

/// Bumped on any change to the frames below.
pub const PROTOCOL: u32 = 1;
/// The largest frame, kind byte included.
pub const MAX_FRAME: usize = 1 << 20;
/// The largest payload one frame carries.
pub const MAX_PAYLOAD: usize = MAX_FRAME - 1;

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
    Input(Vec<u8>),
    Resize {
        rows: u16,
        cols: u16,
    },
    Detach,
    Command {
        argv: Vec<String>,
        cwd: String,
        pane: Option<String>,
    },
    /// server → client.
    Paint(Vec<u8>),
    Exit(String),
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Done {
        status: u8,
    },
}

impl Frame {
    fn kind(&self) -> u8 {
        match self {
            Frame::Hello { .. } => 1,
            Frame::Attach { .. } => 2,
            Frame::Input(_) => 3,
            Frame::Resize { .. } => 4,
            Frame::Detach => 5,
            Frame::Command { .. } => 6,
            Frame::Paint(_) => 7,
            Frame::Exit(_) => 8,
            Frame::Stdout(_) => 9,
            Frame::Stderr(_) => 10,
            Frame::Done { .. } => 11,
        }
    }

    /// The encoded frame, or an error if its payload exceeds `MAX_PAYLOAD`.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut payload = Vec::new();
        match self {
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
            | Frame::Stderr(bytes) => {
                payload.extend_from_slice(bytes);
            }
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
        }
        if payload.len() > MAX_PAYLOAD {
            return Err(format!(
                "a frame of {} bytes exceeds the {MAX_PAYLOAD}-byte limit",
                payload.len()
            ));
        }
        let length = u32::try_from(payload.len() + 1).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(payload.len() + 5);
        out.extend_from_slice(&length.to_be_bytes());
        out.push(self.kind());
        out.extend_from_slice(&payload);
        Ok(out)
    }

    /// Frames carrying `bytes` split so each fits, for paint and output.
    pub fn chunked(make: fn(Vec<u8>) -> Frame, bytes: &[u8]) -> Vec<Frame> {
        bytes
            .chunks(MAX_PAYLOAD)
            .map(|chunk| make(chunk.to_vec()))
            .collect()
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
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if self.0.len() < n {
            return Err("truncated frame".into());
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, String> {
        self.take(1)?
            .first()
            .copied()
            .ok_or_else(|| "truncated frame".into())
    }
    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([
            b.first().copied().unwrap_or(0),
            b.get(1).copied().unwrap_or(0),
        ]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        let mut a = [0u8; 4];
        a.copy_from_slice(b);
        Ok(u32::from_be_bytes(a))
    }
    fn string(&mut self) -> Result<String, String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "a frame string is not UTF-8".into())
    }
    fn option(&mut self) -> Result<Option<String>, String> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.string()?)),
            _ => Err("bad option marker in frame".into()),
        }
    }
    fn end(&self) -> Result<(), String> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err("trailing bytes in frame".into())
        }
    }
}

fn decode_frame(kind: u8, payload: &[u8]) -> Result<Frame, String> {
    let mut r = Reader(payload);
    let frame = match kind {
        1 => {
            let protocol = r.u32()?;
            let role = match r.u8()? {
                0 => Role::Attach,
                1 => Role::Command,
                2 => Role::Kill,
                other => return Err(format!("unknown client role {other}")),
            };
            let version = r.string()?;
            Frame::Hello {
                protocol,
                version,
                role,
            }
        }
        2 => Frame::Attach {
            rows: r.u16()?,
            cols: r.u16()?,
            workspace: r.option()?,
        },
        3 => return Ok(Frame::Input(payload.to_vec())),
        4 => Frame::Resize {
            rows: r.u16()?,
            cols: r.u16()?,
        },
        5 => Frame::Detach,
        6 => {
            let count = r.u32()? as usize;
            // Each argument takes at least its 4-byte length.
            if count > payload.len() / 4 {
                return Err("bad argument count in frame".into());
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
        7 => return Ok(Frame::Paint(payload.to_vec())),
        8 => {
            return String::from_utf8(payload.to_vec())
                .map(Frame::Exit)
                .map_err(|_| "an exit reason is not UTF-8".into());
        }
        9 => return Ok(Frame::Stdout(payload.to_vec())),
        10 => return Ok(Frame::Stderr(payload.to_vec())),
        11 => Frame::Done { status: r.u8()? },
        other => return Err(format!("unknown frame kind {other}")),
    };
    r.end()?;
    Ok(frame)
}

/// Accumulates bytes from a stream and yields whole frames. A frame header
/// claiming more than `MAX_FRAME` is refused before anything is buffered for
/// it, so a peer cannot make the reader grow.
#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole frame, `Ok(None)` if more bytes are needed, or an error
    /// for a frame that is oversized or malformed; the stream is then unusable.
    pub fn frame(&mut self) -> Result<Option<Frame>, String> {
        let Some(header) = self.buffer.get(..4) else {
            return Ok(None);
        };
        let mut length = [0u8; 4];
        length.copy_from_slice(header);
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 {
            return Err("an empty frame".into());
        }
        if length > MAX_FRAME {
            return Err(format!(
                "a frame of {length} bytes exceeds the {MAX_FRAME}-byte limit"
            ));
        }
        let Some(body) = self.buffer.get(4..4 + length) else {
            return Ok(None);
        };
        let kind = body.first().copied().unwrap_or(0);
        let frame = decode_frame(kind, body.get(1..).unwrap_or(&[]));
        self.buffer.drain(..4 + length);
        frame.map(Some)
    }

    /// Bytes held for a frame not yet complete.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let chunks = Frame::chunked(Frame::Paint, &vec![7; MAX_PAYLOAD * 2 + 3]);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|f| f.encode().is_ok()));
    }

    #[test]
    fn malformed_frames_are_errors_not_panics() {
        for bytes in [
            vec![0, 0, 0, 1, 99],
            vec![0, 0, 0, 1, 11],
            vec![0, 0, 0, 3, 11, 0, 0],
            vec![0, 0, 0, 5, 6, 255, 255, 255, 255],
            vec![0, 0, 0, 0],
            vec![0, 0, 0, 2, 8, 0xff],
        ] {
            let mut decoder = Decoder::default();
            decoder.push(&bytes);
            assert!(decoder.frame().is_err(), "{bytes:?}");
        }
    }
}

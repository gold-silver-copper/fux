//! The protocol, in the baseline and the current fux: random frames encode
//! to the same bytes, alone and after others, a stream's payload and a
//! client's input are framed alike; and random byte streams -- frames,
//! broken frames, stray headers, empty and oversized lengths -- pushed
//! whole and in pieces decode to the same frames and errors, check alike,
//! and lend the same paints and inputs.
use crate::rng::Rng;
use crate::{Outcome, bump, same, times};

/// A frame, for both to build.
#[derive(Clone, Debug)]
enum Spec {
    Hello(u32, String, u8),
    Attach(u16, u16, Option<String>),
    Input(Vec<u8>),
    Resize(u16, u16),
    Detach,
    Command(Vec<String>, String, Option<String>),
    Paint(Vec<u8>),
    Exit(String),
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Done(u8),
}

/// The largest payload, as both define it.
const MAX_PAYLOAD: usize = (1 << 20) - 1;

fn bytes(r: &mut Rng, big: bool) -> Vec<u8> {
    let len = match if big {
        r.below(40)
    } else {
        r.below(37).saturating_add(3)
    } {
        0 => MAX_PAYLOAD.saturating_add(r.below(3)),
        1 => MAX_PAYLOAD.saturating_sub(r.below(3)),
        2 => MAX_PAYLOAD.saturating_mul(2).saturating_add(r.below(5)),
        3..=9 => 0,
        _ => r.below(40),
    };
    (0..len).map(|_| r.next().to_le_bytes()[0]).collect()
}

fn text(r: &mut Rng) -> String {
    let parts = ["", "a", "界", "%3", "main", "\u{0}", "é"];
    (0..r.below(4))
        .map(|_| r.pick(&parts).copied().unwrap_or_default())
        .collect()
}

fn byte(r: &mut Rng) -> u8 {
    r.next().to_le_bytes()[0]
}

fn spec(r: &mut Rng, big: bool) -> Spec {
    let maybe = |r: &mut Rng| r.chance(50).then(|| text(r));
    match r.below(14) {
        0 => {
            let protocol = r.pick(&[0, 1, 2, u32::MAX]).copied().unwrap_or(1);
            Spec::Hello(protocol, text(r), u8::try_from(r.below(3)).unwrap_or(0))
        }
        1 => Spec::Attach(
            u16::try_from(r.below(65_536)).unwrap_or(0),
            u16::try_from(r.below(65_536)).unwrap_or(0),
            maybe(r),
        ),
        2 => Spec::Input(bytes(r, big)),
        3 => Spec::Resize(
            u16::try_from(r.below(65_536)).unwrap_or(0),
            u16::try_from(r.below(65_536)).unwrap_or(0),
        ),
        4 => Spec::Detach,
        5 => Spec::Command(
            (0..r.below(4)).map(|_| text(r)).collect(),
            text(r),
            maybe(r),
        ),
        6 | 7 => Spec::Paint(bytes(r, big)),
        8 => Spec::Exit(text(r)),
        9 | 10 => Spec::Stdout(bytes(r, big)),
        11 | 12 => Spec::Stderr(bytes(r, big)),
        _ => Spec::Done(byte(r)),
    }
}

/// A byte stream: frames, some of them broken, and noise.
fn stream(r: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..r.below(8) {
        let noisy = r.chance(33);
        match r.below(if noisy { 10 } else { 6 }) {
            0..=5 => {
                let Ok(mut frame) = base::encode(&spec(r, false)) else {
                    continue;
                };
                if r.chance(8) && frame.len() > 5 {
                    // Broken: a changed kind, length or payload byte.
                    let at = match r.below(3) {
                        0 => 4,
                        1 => r.below(4),
                        _ => r.below(frame.len()),
                    };
                    let new = if at == 4 {
                        u8::try_from(r.below(14)).unwrap_or(0)
                    } else {
                        byte(r)
                    };
                    if let Some(b) = frame.get_mut(at) {
                        *b = new;
                    }
                }
                if r.chance(5) {
                    frame.truncate(r.below(frame.len().saturating_add(1)));
                }
                out.extend(frame);
            }
            6 => {
                // A header with any kind, and a short payload.
                let len = r.below(12);
                out.extend(
                    u32::try_from(len.saturating_add(1))
                        .unwrap_or(0)
                        .to_be_bytes(),
                );
                out.push(u8::try_from(r.below(16)).unwrap_or(0));
                out.extend((0..len).map(|_| byte(r)));
            }
            7 => out.extend((0..r.below(9)).map(|_| byte(r))),
            8 => out.extend(0u32.to_be_bytes()),
            _ => {
                let over = (1usize << 20).saturating_add(r.below(3));
                out.extend(u32::try_from(over).unwrap_or(u32::MAX).to_be_bytes());
            }
        }
    }
    out
}

macro_rules! stack {
    ($name:ident, $fux:ident) => {
        mod $name {
            use super::Spec;
            use $fux::bytes::ByteQueue;
            use $fux::protocol::{Decoder, Frame, Role, Stream};

            fn frame(spec: &Spec) -> Frame {
                let role = |r: u8| match r {
                    0 => Role::Attach,
                    1 => Role::Command,
                    _ => Role::Kill,
                };
                match spec.clone() {
                    Spec::Hello(protocol, version, r) => Frame::Hello {
                        protocol,
                        version,
                        role: role(r),
                    },
                    Spec::Attach(rows, cols, workspace) => Frame::Attach {
                        rows,
                        cols,
                        workspace,
                    },
                    Spec::Input(bytes) => Frame::Input(bytes),
                    Spec::Resize(rows, cols) => Frame::Resize { rows, cols },
                    Spec::Detach => Frame::Detach,
                    Spec::Command(argv, cwd, pane) => Frame::Command { argv, cwd, pane },
                    Spec::Paint(bytes) => Frame::Paint(bytes),
                    Spec::Exit(reason) => Frame::Exit(reason),
                    Spec::Stdout(bytes) => Frame::Stdout(bytes),
                    Spec::Stderr(bytes) => Frame::Stderr(bytes),
                    Spec::Done(status) => Frame::Done { status },
                }
            }

            pub fn encode(spec: &Spec) -> Result<Vec<u8>, String> {
                frame(spec).encode().map_err(|e| format!("{e:?} {e}"))
            }

            /// Every way a frame, or its payload, is written: the bytes, each
            /// after what it returned.
            pub fn written(spec: &Spec) -> Vec<u8> {
                let frame = frame(spec);
                let mut out = format!("{:?}", encode(spec).map(|b| b.len())).into_bytes();
                out.extend(encode(spec).unwrap_or_default());
                let mut after = b"before".to_vec();
                let into = frame
                    .encode_into(&mut after)
                    .map_err(|e| format!("{e:?} {e}"));
                out.extend(format!("{into:?}").into_bytes());
                out.extend(after);
                let payload = match spec {
                    Spec::Input(b) | Spec::Paint(b) | Spec::Stdout(b) | Spec::Stderr(b) => {
                        b.clone()
                    }
                    Spec::Hello(..)
                    | Spec::Attach(..)
                    | Spec::Resize(..)
                    | Spec::Detach
                    | Spec::Command(..)
                    | Spec::Exit(_)
                    | Spec::Done(_) => Vec::new(),
                };
                for stream in [Stream::Paint, Stream::Stdout, Stream::Stderr] {
                    let mut queue = ByteQueue::default();
                    queue.push(b"before");
                    stream.encode_into(&payload, &mut queue);
                    out.extend_from_slice(queue.as_slice());
                }
                out
            }

            /// Every frame and the first error, with what is held after
            /// each push, from `bytes` pushed in pieces of `size` (all at
            /// once for 0).
            pub fn decoded(bytes: &[u8], size: usize) -> String {
                let mut decoder = Decoder::default();
                let mut out = String::new();
                let mut rest = bytes;
                while !rest.is_empty() {
                    let take = if size == 0 {
                        rest.len()
                    } else {
                        size.min(rest.len())
                    };
                    let (piece, after) = rest.split_at_checked(take).unwrap_or((rest, &[]));
                    rest = after;
                    decoder.push(piece);
                    loop {
                        match decoder.frame() {
                            Ok(Some(frame)) => out.push_str(&format!("{frame:?}\n")),
                            Ok(None) => break,
                            Err(error) => return format!("{out}{error:?} {error}"),
                        }
                    }
                    out.push_str(&format!("held {}\n", decoder.buffered()));
                }
                out
            }

            /// What checking finds, and then each raw frame: the paint and
            /// input it lends, and what it decodes to.
            pub fn raw(bytes: &[u8]) -> String {
                let mut decoder = Decoder::default();
                decoder.push(bytes);
                let checked = decoder.check(0);
                let mut out = format!(
                    "{} {} {:?}\n",
                    checked.end,
                    checked.frames,
                    checked.error.map(|e| format!("{e:?} {e}"))
                );
                loop {
                    match decoder.raw() {
                        Ok(Some(raw)) => out.push_str(&format!(
                            "{:?} {:?} {:?}\n",
                            raw.paint(),
                            raw.input(),
                            raw.decode().map_err(|e| format!("{e:?} {e}"))
                        )),
                        Ok(None) => return out,
                        Err(error) => return format!("{out}{error:?} {error}"),
                    }
                }
            }
        }
    };
}

stack!(base, baseline);
stack!(cur, fux);

pub fn run(r: &mut Rng, scale: usize) -> Outcome {
    let (mut frames, mut streams, mut refused) = (0u64, 0u64, 0u64);
    for case in 0..times(20_000, scale) {
        let s = spec(r, true);
        let (a, b) = (base::written(&s), cur::written(&s));
        if base::encode(&s).is_err() {
            bump(&mut refused);
        }
        crate::same_bytes(&format!("frame {case}"), &a, &b)?;
        bump(&mut frames);
    }
    for case in 0..times(50_000, scale) {
        let bytes = stream(r);
        let size = r.below(64);
        let context = format!(
            "stream {case}, {} bytes {bytes:?}, pieces of {size}",
            bytes.len()
        );
        same(&context, base::decoded(&bytes, 0), cur::decoded(&bytes, 0))?;
        same(
            &context,
            base::decoded(&bytes, size),
            cur::decoded(&bytes, size),
        )?;
        same(&context, base::raw(&bytes), cur::raw(&bytes))?;
        bump(&mut streams);
    }
    Ok(format!(
        "{frames} frames ({refused} too long to send), {streams} byte streams, whole and in pieces"
    ))
}

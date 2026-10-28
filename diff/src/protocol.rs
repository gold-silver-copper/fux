//! The protocol, in the baseline and the current fux: a stream's payload
//! is framed alike; and random byte streams -- frames, broken frames, stray
//! headers, empty and oversized lengths -- pushed whole and in pieces
//! decode to the same frames, which encode to the same bytes again, and the
//! same errors. Each side decodes a kind as the frame type of the stage it
//! is allowed in. The frames are made by the current fux's types; an
//! attached client's `Act` and a command's request, typed values of their
//! own, are not among them (fux's own tests check their decoding).
use crate::rng::Rng;
use crate::{Outcome, bump, same, times};

/// A frame, to build.
#[derive(Clone, Debug)]
enum Spec {
    Hello(u32, String, u8),
    Attach(u16, u16, bool),
    Resize(u16, u16),
    Detach,
    Paint(Vec<u8>),
    Exit(String),
    Stdout(Vec<u8>),
    Part(Vec<u8>),
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
    match r.below(14) {
        0 => {
            let protocol = r.pick(&[0, 1, 2, u32::MAX]).copied().unwrap_or(1);
            Spec::Hello(protocol, text(r), u8::try_from(r.below(3)).unwrap_or(0))
        }
        1 => Spec::Attach(
            u16::try_from(r.below(65_536)).unwrap_or(0),
            u16::try_from(r.below(65_536)).unwrap_or(0),
            r.chance(50),
        ),
        2 => Spec::Part(bytes(r, big)),
        3 => Spec::Resize(
            u16::try_from(r.below(65_536)).unwrap_or(0),
            u16::try_from(r.below(65_536)).unwrap_or(0),
        ),
        4 => Spec::Detach,
        5..=7 => Spec::Paint(bytes(r, big)),
        8 => Spec::Exit(text(r)),
        9 | 10 => Spec::Stdout(bytes(r, big)),
        11 | 12 => Spec::Part(bytes(r, big)),
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
                let Ok(mut frame) = encode(&spec(r, false)) else {
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

/// `$name`: every frame `$decoder` decodes, by `$next` from the kind byte
/// held next, encoded again, and the first error, as the current fux names
/// it, with what is held after each push, from `bytes` pushed in pieces of
/// `size` (all at once for 0).
macro_rules! decoded {
    ($name:ident, $decoder:ty, |$d:ident, $kind:ident| $next:expr) => {
        fn $name(bytes: &[u8], size: usize) -> String {
            let mut $d = <$decoder>::default();
            let mut out = String::new();
            let mut pushed = 0usize;
            let mut rest = bytes;
            while !rest.is_empty() {
                let take = if size == 0 {
                    rest.len()
                } else {
                    size.min(rest.len())
                };
                let (piece, after) = rest.split_at_checked(take).unwrap_or((rest, &[]));
                rest = after;
                $d.push(piece);
                pushed = pushed.saturating_add(piece.len());
                loop {
                    let held = bytes
                        .get(pushed.saturating_sub($d.buffered())..pushed)
                        .unwrap_or_default();
                    let $kind = held.get(4).copied();
                    let mut next = || -> Result<Option<Vec<u8>>, String> { $next };
                    match next() {
                        Ok(Some(frame)) => out.push_str(&format!("{frame:?}\n")),
                        Ok(None) => break,
                        Err(error) => return format!("{out}{error}"),
                    }
                }
                out.push_str(&format!("held {}\n", $d.buffered()));
            }
            out
        }
    };
}

decoded!(base, baseline::protocol::Decoder, |d, kind| {
    use baseline::protocol::{AttachedFrame, Frame, Greeting, Hello, ToAttached, ToCommand};
    fn again<'a>(frame: Option<impl Frame<'a>>) -> Result<Option<Vec<u8>>, String> {
        frame
            .map(|f| f.encode().map_err(|e| format!("{e:?}")))
            .transpose()
    }
    let error = |e| format!("{e:?}");
    match kind {
        Some(1) => again(d.frame::<Hello>().map_err(error)?),
        Some(4 | 5) => again(d.frame::<AttachedFrame>().map_err(error)?),
        // A paint, as to an attached client; output, as to a command
        // client; the rest as the server's first frames.
        Some(7) => again(d.frame::<ToAttached>().map_err(error)?),
        Some(9) => again(d.frame::<ToCommand>().map_err(error)?),
        _ => again(d.frame::<Greeting>().map_err(error)?),
    }
});

decoded!(cur, fux::protocol::Decoder, |d, kind| {
    use fux::protocol::{AttachedFrame, Frame, Greeting, Hello, ToAttached, ToCommand};
    fn again<'a>(frame: Option<impl Frame<'a>>) -> Result<Option<Vec<u8>>, String> {
        frame
            .map(|f| f.encode().map_err(|e| format!("{e:?}")))
            .transpose()
    }
    let error = |e| format!("{e:?}");
    match kind {
        Some(1) => again(d.frame::<Hello>().map_err(error)?),
        Some(4 | 5) => again(d.frame::<AttachedFrame>().map_err(error)?),
        // A paint, as to an attached client; output, as to a command
        // client; the rest as the server's first frames.
        Some(7) => again(d.frame::<ToAttached>().map_err(error)?),
        Some(9) => again(d.frame::<ToCommand>().map_err(error)?),
        _ => again(d.frame::<Greeting>().map_err(error)?),
    }
});

/// What each stream writes for `payload`, after other bytes: the
/// baseline's, then the current fux's.
fn streamed(payload: &[u8]) -> (Vec<u8>, Vec<u8>) {
    use fux::protocol::{ToAttached, ToCommand, split_into};
    let mut a = baseline::bytes::ByteQueue::default();
    a.push(b"before");
    baseline::protocol::split_into(payload, baseline::protocol::ToAttached::Paint, &mut a);
    a.push(b"before");
    baseline::protocol::split_into(payload, baseline::protocol::ToCommand::Stdout, &mut a);
    let mut b = fux::bytes::ByteQueue::default();
    b.push(b"before");
    split_into(payload, ToAttached::Paint, &mut b);
    b.push(b"before");
    split_into(payload, ToCommand::Stdout, &mut b);
    (a.as_slice().to_vec(), b.as_slice().to_vec())
}

/// `spec`, encoded by the current fux's frame types.
fn encode(spec: &Spec) -> Result<Vec<u8>, String> {
    use fux::protocol::{
        Attach, AttachedFrame, Frame, Greeting, Hello, Role, ToAttached, ToCommand,
    };
    use std::borrow::Cow;
    let role = |r: u8| match r {
        0 => Role::Attach,
        1 => Role::Command,
        _ => Role::Kill,
    };
    let error = |e| format!("{e:?}");
    match spec.clone() {
        Spec::Hello(protocol, version, r) => Hello {
            protocol,
            role: role(r),
            version,
        }
        .encode()
        .map_err(error),
        Spec::Attach(rows, cols, read_only) => Attach {
            of: None,
            size: crate::size(rows, cols),
            target: None,
            read_only,
            initial: None,
            presentation: fux::view::Presentation {
                bell: true,
                titles: false,
                terminal_clipboard: true,
            },
        }
        .encode()
        .map_err(error),
        Spec::Resize(rows, cols) => AttachedFrame::Resize(crate::size(rows, cols))
            .encode()
            .map_err(error),
        Spec::Detach => AttachedFrame::Detach.encode().map_err(error),
        Spec::Paint(bytes) => ToAttached::Paint(Cow::Owned(bytes)).encode().map_err(error),
        Spec::Exit(reason) => Greeting::Exit(&reason).encode().map_err(error),
        Spec::Stdout(bytes) => ToCommand::Stdout(Cow::Owned(bytes)).encode().map_err(error),
        Spec::Part(bytes) => ToCommand::Part(Cow::Owned(bytes)).encode().map_err(error),
        Spec::Done(status) => Greeting::Done(status).encode().map_err(error),
    }
}

pub fn run(r: &mut Rng, scale: usize) -> Outcome {
    let (mut frames, mut streams) = (0u64, 0u64);
    for case in 0..times(20_000, scale) {
        let (a, b) = streamed(&bytes(r, true));
        crate::same_bytes(&format!("payload {case}, streamed"), &a, &b)?;
        bump(&mut frames);
    }
    for case in 0..times(50_000, scale) {
        let bytes = stream(r);
        let size = r.below(64);
        let context = format!(
            "stream {case}, {} bytes {bytes:?}, pieces of {size}",
            bytes.len()
        );
        same(&context, base(&bytes, 0), cur(&bytes, 0))?;
        same(&context, base(&bytes, size), cur(&bytes, size))?;
        bump(&mut streams);
    }
    Ok(format!(
        "{frames} payloads streamed, {streams} byte streams, whole and in pieces"
    ))
}

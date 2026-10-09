//! The protocol, in the baseline and the current fux: a stream's payload
//! is framed alike; and random byte streams -- frames, broken frames, stray
//! headers, empty and oversized lengths -- pushed whole and in pieces
//! decode to the same frames, which encode to the same bytes again, and the
//! same errors. The current fux decodes each kind as the frame type of the
//! stage it is allowed in, and refuses a size of zero and (as the baseline
//! does not know it) a `Terminal` where the baseline is compared.
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

/// What the current fux refuses of the whole frame `held` begins with,
/// whatever its payload, which the baseline takes: a `Terminal`, or a size
/// of zero, read before the fields after it.
fn refused(held: &[u8]) -> Option<String> {
    let (header, rest) = held.split_first_chunk::<4>()?;
    let length = usize::try_from(u32::from_be_bytes(*header)).ok()?;
    let whole = rest.get(..length).filter(|_| length <= 1 << 20)?;
    let (&kind, payload) = whole.split_first()?;
    let rows = payload.first_chunk::<2>();
    let cols = payload.get(2..).and_then(<[u8]>::first_chunk::<2>);
    let zero = rows == Some(&[0, 0]) || (rows.is_some() && cols == Some(&[0, 0]));
    match kind {
        12 => Some("UnexpectedKind(12)".into()),
        2 | 4 if zero => Some("ZeroSize".into()),
        _ => None,
    }
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
                    if let Some(error) = refused(held) {
                        return format!("{out}{error}");
                    }
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

// An unknown kind is one not expected, and an exit reason not UTF-8 is a
// frame string that is not.
decoded!(base, baseline::protocol::Decoder, |d, _kind| d
    .frame()
    .map_err(|e| {
        format!("{e:?}")
            .replace("UnknownKind", "UnexpectedKind")
            .replace("ExitNotUtf8", "NotUtf8")
    })?
    .map(|f| f.encode().map_err(|e| format!("{e:?}")))
    .transpose());

decoded!(cur, fux::protocol::Decoder, |d, kind| {
    use fux::protocol::{Attach, AttachedFrame, Command, Frame, Hello, ServerFrame};
    fn again<'a>(frame: Option<impl Frame<'a>>) -> Result<Option<Vec<u8>>, String> {
        frame
            .map(|f| f.encode().map_err(|e| format!("{e:?}")))
            .transpose()
    }
    let error = |e| format!("{e:?}");
    match kind {
        Some(1) => again(d.frame::<Hello>().map_err(error)?),
        Some(2) => again(d.frame::<Attach>().map_err(error)?),
        Some(3..=5) => again(d.frame::<AttachedFrame>().map_err(error)?),
        Some(6) => again(d.frame::<Command>().map_err(error)?),
        _ => again(d.frame::<ServerFrame>().map_err(error)?),
    }
});

/// What each stream writes for `payload`, after other bytes: the
/// baseline's, then the current fux's.
fn streamed(payload: &[u8]) -> (Vec<u8>, Vec<u8>) {
    use baseline::protocol::Stream as Base;
    use fux::protocol::ServerFrame as Cur;
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let streams: [(_, fn(_) -> _); 3] = [
        (Base::Paint, Cur::Paint),
        (Base::Stdout, Cur::Stdout),
        (Base::Stderr, Cur::Stderr),
    ];
    for (base, cur) in streams {
        let mut queue = baseline::bytes::ByteQueue::default();
        queue.push(b"before");
        base.encode_into(payload, &mut queue);
        a.extend_from_slice(queue.as_slice());
        let mut queue = fux::bytes::ByteQueue::default();
        queue.push(b"before");
        Cur::split_into(payload, cur, &mut queue);
        b.extend_from_slice(queue.as_slice());
    }
    (a, b)
}

/// `spec`, encoded by the baseline.
fn encode(spec: &Spec) -> Result<Vec<u8>, String> {
    use baseline::protocol::{Frame, Role};
    let role = |r: u8| match r {
        0 => Role::Attach,
        1 => Role::Command,
        _ => Role::Kill,
    };
    let frame = match spec.clone() {
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
    };
    frame.encode().map_err(|e| format!("{e:?}"))
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

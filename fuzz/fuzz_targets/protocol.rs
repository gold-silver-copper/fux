#![no_main]
//! Input: one byte choosing a piece size (0 is the whole stream at once),
//! then the bytes a peer sends on the socket.
use fux_vt::bytes::ByteQueue;
use fux_wire::protocol::{
    Asked, Attach, AttachedFrame, Decoder, Error, Frame, Greeting, Hello, MAX_FRAME, ToAttached,
    ToCommand, split_into,
};
use libfuzzer_sys::fuzz_target;
use std::borrow::Cow;
use std::num::NonZeroUsize;

/// `bytes` in pieces of `size`, the last one maybe shorter.
fn pieces(mut rest: &[u8], size: NonZeroUsize) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (piece, after) = rest.split_at_checked(size.get()).unwrap_or((rest, &[]));
        rest = after;
        (!piece.is_empty()).then_some(piece)
    })
}

/// Each frame `next` decodes from `stream` pushed in pieces of
/// `size` bytes (the whole stream at once for 0), encoded again; then the
/// first error.
fn decode(next: Next, stream: &[u8], size: usize) -> (Vec<Vec<u8>>, Option<Error>) {
    let mut decoder = Decoder::default();
    let mut frames = Vec::new();
    let pieces: Vec<&[u8]> = match NonZeroUsize::new(size) {
        Some(size) => pieces(stream, size).collect(),
        None => vec![stream],
    };
    for piece in pieces {
        decoder.push(piece);
        loop {
            match next(&mut decoder) {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => break,
                // The stream is unusable after an error.
                Err(error) => return (frames, Some(error)),
            }
        }
    }
    (frames, None)
}

/// The physical frames `bytes` begin with, as a decoder reads them: each
/// header's kind and body; then whether a header claims no bytes or more
/// than `MAX_FRAME`, which ends them.
fn physical(mut bytes: &[u8]) -> (Vec<(u8, &[u8])>, bool) {
    let mut frames = Vec::new();
    while let Some((header, rest)) = bytes.split_first_chunk::<4>() {
        let length = u32::from_be_bytes(*header) as usize;
        if length == 0 || length > MAX_FRAME {
            return (frames, true);
        }
        let Some((frame, after)) = rest.split_at_checked(length) else {
            break;
        };
        frames.extend(frame.split_first().map(|(kind, body)| (*kind, body)));
        bytes = after;
    }
    (frames, false)
}

/// The next frame `decoder` yields as one of the kinds expected at some
/// point of a connection, encoded again.
type Next = fn(&mut Decoder) -> Result<Option<Vec<u8>>, Error>;

/// A decoded frame encodes again, in physical frames none past the limit.
fn encoded<'a>(frame: &impl Frame<'a>) -> Vec<u8> {
    let bytes = frame.encode();
    let (frames, oversized) = physical(&bytes);
    assert!(!oversized && frames.iter().map(|(_, b)| b.len() + 5).sum::<usize>() == bytes.len());
    bytes
}

/// A stream's payload (a paint, a reply's part) is written as frames of
/// its kind, each within the limit, whose bodies together are the
/// payload; an empty one as none.
fn splits_back<'a, F: Frame<'a>>(payload: &'a [u8], frame: fn(Cow<'a, [u8]>) -> F) {
    let kind = frame(Cow::Borrowed(&[])).encode().get(4).copied();
    let mut queue = ByteQueue::default();
    split_into(payload, frame, &mut queue);
    let (frames, oversized) = physical(queue.as_slice());
    assert!(!oversized && frames.iter().all(|(k, _)| Some(*k) == kind));
    let joined: Vec<u8> = frames
        .iter()
        .flat_map(|(_, body)| body.iter().copied())
        .collect();
    assert_eq!(joined, payload);
}

/// A server's frame to a command client, encoded again.
fn to_command(decoder: &mut Decoder) -> Result<Option<Vec<u8>>, Error> {
    let Some(frame) = decoder.frame::<ToCommand>()? else {
        return Ok(None);
    };
    let bytes = encoded(&frame);
    match &frame {
        ToCommand::Part(p) => splits_back(p, ToCommand::Part),
        ToCommand::Exit(_) | ToCommand::PartsEnd => {}
    }
    Ok(Some(bytes))
}

/// A server's frame to an attached client, encoded again.
fn to_attached(decoder: &mut Decoder) -> Result<Option<Vec<u8>>, Error> {
    let Some(frame) = decoder.frame::<ToAttached>()? else {
        return Ok(None);
    };
    let bytes = encoded(&frame);
    match &frame {
        ToAttached::Paint(p) => splits_back(p, ToAttached::Paint),
        ToAttached::Welcome(..)
        | ToAttached::Exit(_)
        | ToAttached::Ended(_)
        | ToAttached::Open(_)
        | ToAttached::Keys(_)
        | ToAttached::Read(..) => {}
    }
    Ok(Some(bytes))
}

fuzz_target!(|data: &[u8]| {
    let Some((&size, stream)) = data.split_first() else {
        return;
    };
    let kinds: [Next; 7] = [
        |d| Ok(d.frame::<Hello>()?.map(|f| encoded(&f))),
        |d| Ok(d.frame::<Attach>()?.map(|f| encoded(&f))),
        |d| Ok(d.frame::<AttachedFrame>()?.map(|f| encoded(&f))),
        |d| Ok(d.frame::<Asked>()?.map(|f| encoded(&f))),
        |d| Ok(d.frame::<Greeting>()?.map(|f| encoded(&f))),
        to_command,
        to_attached,
    ];
    // A header past the limit is refused as it comes, never waited on.
    let (_, oversized) = physical(stream);
    for next in kinds {
        // However the bytes arrive, the same frames and the same error.
        let whole = decode(next, stream, 0);
        assert_eq!(decode(next, stream, usize::from(size)), whole);
        assert_eq!(decode(next, stream, 1), whole);
        assert!(!oversized || whole.1.is_some());
        // The frames, encoded again, decode to themselves.
        let again = whole.0.concat();
        assert_eq!(decode(next, &again, 0), (whole.0, None));
    }
});

#![no_main]
//! Input: one byte choosing a piece size (0 is the whole stream at once),
//! then the bytes a peer sends on the socket.
use fux::bytes::ByteQueue;
use fux::protocol::{
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
        // All that is held is one incomplete frame, whose header claimed at
        // most MAX_FRAME, however large the pieces.
        assert!(
            decoder.buffered() < 4 + MAX_FRAME,
            "{} bytes held",
            decoder.buffered()
        );
    }
    (frames, None)
}

/// The next frame `decoder` yields as one of the kinds expected at some
/// point of a connection, encoded again.
type Next = fn(&mut Decoder) -> Result<Option<Vec<u8>>, Error>;

/// A decoded frame encodes again.
fn encoded<'a>(frame: &impl Frame<'a>) -> Vec<u8> {
    let bytes = frame.encode();
    assert!(bytes.is_ok(), "a decoded frame does not encode: {bytes:?}");
    bytes.unwrap_or_default()
}

/// A stream's frame, `bytes`, is what the stream writes for its payload
/// (a reply's part too), and an empty stream writes nothing.
fn splits_back<'a, F: Frame<'a>>(bytes: &[u8], payload: &'a [u8], frame: fn(Cow<'a, [u8]>) -> F) {
    let mut queue = ByteQueue::default();
    split_into(payload, frame, &mut queue);
    // A header and no payload.
    let expected: &[u8] = if bytes.len() == 5 { &[] } else { bytes };
    assert_eq!(queue.as_slice(), expected);
}

/// A server's frame to a command client, encoded again.
fn to_command(decoder: &mut Decoder) -> Result<Option<Vec<u8>>, Error> {
    let Some(frame) = decoder.frame::<ToCommand>()? else {
        return Ok(None);
    };
    let bytes = encoded(&frame);
    match &frame {
        ToCommand::Stdout(p) => splits_back(&bytes, p, ToCommand::Stdout),
        ToCommand::Part(p) => splits_back(&bytes, p, ToCommand::Part),
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
        ToAttached::Paint(p) => splits_back(&bytes, p, ToAttached::Paint),
        ToAttached::Welcome(..)
        | ToAttached::Exit(_)
        | ToAttached::Ended(_)
        | ToAttached::Run(_)
        | ToAttached::Keys(_) => {}
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
    for next in kinds {
        // However the bytes arrive, the same frames and the same error;
        // and the frames encode to the bytes they came from.
        let whole = decode(next, stream, 0);
        assert_eq!(decode(next, stream, usize::from(size)), whole);
        assert_eq!(decode(next, stream, 1), whole);
        assert!(stream.starts_with(&whole.0.concat()));
    }
});

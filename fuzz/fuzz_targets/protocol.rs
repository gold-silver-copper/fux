#![no_main]
//! Input: one byte choosing a piece size (0 is the whole stream at once),
//! then the bytes a peer sends on the socket.
use fux::bytes::ByteQueue;
use fux::protocol::{AttachFrame, Command, Decoder, Error, Frame, Hello, MAX_FRAME, ServerFrame};
use libfuzzer_sys::fuzz_target;
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

/// A server's frame, encoded again. A stream's is what the stream writes
/// for its payload, and an empty stream writes nothing.
fn server(decoder: &mut Decoder) -> Result<Option<Vec<u8>>, Error> {
    let Some(frame) = decoder.frame::<ServerFrame>()? else {
        return Ok(None);
    };
    let bytes = encoded(&frame);
    let mut queue = ByteQueue::default();
    match frame {
        ServerFrame::Paint(p) => ServerFrame::split_into(p, ServerFrame::Paint, &mut queue),
        ServerFrame::Stdout(p) => ServerFrame::split_into(p, ServerFrame::Stdout, &mut queue),
        ServerFrame::Stderr(p) => ServerFrame::split_into(p, ServerFrame::Stderr, &mut queue),
        ServerFrame::Hello(_)
        | ServerFrame::Exit(_)
        | ServerFrame::Done { .. }
        | ServerFrame::Terminal { .. } => return Ok(Some(bytes)),
    }
    // A header and no payload.
    let expected: &[u8] = if bytes.len() == 5 { &[] } else { &bytes };
    assert_eq!(queue.as_slice(), expected);
    Ok(Some(bytes))
}

fuzz_target!(|data: &[u8]| {
    let Some((&size, stream)) = data.split_first() else {
        return;
    };
    let kinds: [Next; 4] = [
        |d| Ok(d.frame::<Hello>()?.map(|f| encoded(&f))),
        |d| Ok(d.frame::<AttachFrame>()?.map(|f| encoded(&f))),
        |d| Ok(d.frame::<Command>()?.map(|f| encoded(&f))),
        server,
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

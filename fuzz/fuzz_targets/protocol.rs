#![no_main]
//! Input: one byte choosing a piece size (0 is the whole stream at once),
//! then the bytes a peer sends on the socket.
use fux::bytes::ByteQueue;
use fux::protocol::{
    AttachFrame, Command, Decoder, Error, Frame, Hello, MAX_FRAME, ServerFrame, Stream,
};
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

/// A decoded frame encodes again, alone and after other bytes alike.
fn encoded<'a>(frame: &impl Frame<'a>) -> Vec<u8> {
    let bytes = frame.encode();
    assert!(bytes.is_ok(), "a decoded frame does not encode: {bytes:?}");
    let bytes = bytes.unwrap_or_default();
    let mut out = b"before".to_vec();
    assert!(frame.encode_into(&mut out).is_ok());
    assert_eq!(out.strip_prefix(b"before"), Some(bytes.as_slice()));
    bytes
}

/// A server's frame, encoded again. A stream's is what the stream writes
/// for its payload, and an empty stream writes nothing.
fn server(decoder: &mut Decoder) -> Result<Option<Vec<u8>>, Error> {
    let Some(frame) = decoder.frame::<ServerFrame>()? else {
        return Ok(None);
    };
    let bytes = encoded(&frame);
    let stream = match frame {
        ServerFrame::Paint(payload) => Some((Stream::Paint, payload)),
        ServerFrame::Stdout(payload) => Some((Stream::Stdout, payload)),
        ServerFrame::Stderr(payload) => Some((Stream::Stderr, payload)),
        ServerFrame::Hello(_)
        | ServerFrame::Exit(_)
        | ServerFrame::Done { .. }
        | ServerFrame::Terminal { .. } => None,
    };
    if let Some((stream, payload)) = stream {
        let mut queue = ByteQueue::default();
        stream.encode_into(payload, &mut queue);
        let expected: &[u8] = if payload.is_empty() { &[] } else { &bytes };
        assert_eq!(queue.as_slice(), expected);
    }
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

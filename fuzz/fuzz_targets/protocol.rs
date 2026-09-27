#![no_main]
//! Input: one byte choosing a piece size (0 is the whole stream at once),
//! then the bytes a peer sends on the socket.
use fux::bytes::ByteQueue;
use fux::protocol::{Decoder, Frame, MAX_FRAME, Stream};
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

/// The frames, then the first error, from `stream` pushed in pieces of
/// `size` bytes (the whole stream at once for 0).
fn decode(stream: &[u8], size: usize) -> (Vec<Frame>, Option<String>) {
    let mut decoder = Decoder::default();
    let mut frames = Vec::new();
    let pieces: Vec<&[u8]> = match NonZeroUsize::new(size) {
        Some(size) => pieces(stream, size).collect(),
        None => vec![stream],
    };
    for piece in pieces {
        decoder.push(piece);
        loop {
            match decoder.frame() {
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

fuzz_target!(|data: &[u8]| {
    let Some((&size, stream)) = data.split_first() else {
        return;
    };
    let whole = decode(stream, 0);
    // However the bytes arrive, the same frames and the same error.
    assert_eq!(decode(stream, usize::from(size)), whole);
    assert_eq!(decode(stream, 1), whole);
    // A decoded frame re-encodes to bytes that decode to it again.
    for frame in &whole.0 {
        let bytes = frame.encode();
        assert!(bytes.is_ok(), "{frame:?} does not encode: {bytes:?}");
        let bytes = bytes.unwrap_or_default();
        assert_eq!(decode(&bytes, 0), (vec![frame.clone()], None));
        // Encoded after other bytes, it is the same bytes after them.
        let mut out = b"before".to_vec();
        assert!(frame.encode_into(&mut out).is_ok());
        assert_eq!(out.strip_prefix(b"before"), Some(bytes.as_slice()));
        // A stream's frame is what the stream writes for its payload, and
        // an empty stream writes nothing.
        let stream = match frame {
            Frame::Paint(payload) => Some((Stream::Paint, payload)),
            Frame::Stdout(payload) => Some((Stream::Stdout, payload)),
            Frame::Stderr(payload) => Some((Stream::Stderr, payload)),
            _ => None,
        };
        if let Some((stream, payload)) = stream {
            let mut queue = ByteQueue::default();
            stream.encode_into(payload, &mut queue);
            let expected: &[u8] = if payload.is_empty() { &[] } else { &bytes };
            assert_eq!(queue.as_slice(), expected);
        }
    }
});

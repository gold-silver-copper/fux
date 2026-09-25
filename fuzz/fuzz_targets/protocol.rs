#![no_main]
//! Input: one byte choosing a piece size (0 is the whole stream at once),
//! then the bytes a peer sends on the socket.
use fux::protocol::{Decoder, Frame, MAX_FRAME};
use libfuzzer_sys::fuzz_target;

/// The frames, then the first error, from `stream` pushed in pieces of
/// `size` bytes (the whole stream at once for 0).
fn decode(stream: &[u8], size: usize) -> (Vec<Frame>, Option<String>) {
    let mut decoder = Decoder::default();
    let mut frames = Vec::new();
    let pieces: Vec<&[u8]> = if size == 0 {
        vec![stream]
    } else {
        stream.chunks(size).collect()
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
    }
});

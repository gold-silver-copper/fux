#![no_main]
//! Input: one byte choosing a piece size (0 is the whole stream at once),
//! then the bytes an attached client's terminal sends.
use fux::decode::{Decoder, Input, PASTE_LIMIT};
use libfuzzer_sys::fuzz_target;

/// The inputs from `stream` given in pieces of `size` bytes (whole for 0),
/// with the Escape deadline passing only at the end.
fn decode(stream: &[u8], size: usize) -> Vec<Input> {
    let mut decoder = Decoder::default();
    let mut out = Vec::new();
    if size == 0 {
        decoder.bytes(stream, &mut out);
    } else {
        for piece in stream.chunks(size) {
            decoder.bytes(piece, &mut out);
        }
    }
    decoder.timeout(&mut out);
    out
}

fuzz_target!(|data: &[u8]| {
    let Some((&size, stream)) = data.split_first() else {
        return;
    };
    let whole = decode(stream, 0);
    // However the bytes arrive, the same inputs.
    assert_eq!(decode(stream, usize::from(size)), whole);
    assert_eq!(decode(stream, 1), whole);
    for input in &whole {
        // PASTE_LIMIT counts the bytes pasted. Invalid UTF-8 becomes U+FFFD,
        // three bytes, so the text is bounded in chars: at most one a byte.
        if let Input::Paste(text) = input {
            assert!(
                text.chars().count() <= PASTE_LIMIT,
                "{} chars",
                text.chars().count()
            );
        }
    }
});

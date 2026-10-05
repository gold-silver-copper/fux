#![no_main]
//! Input: one byte choosing a piece size (0 is the whole stream at once),
//! then the bytes an attached client's terminal sends. Each mouse report
//! decoded encodes back to an event that decodes alike, and each paste,
//! bracketed again, holds no end marker but its own.
use fux_vt::keys::decode::{Decoder, Input, PASTE_LIMIT};
use fux_vt::keys::encode::{PASTE_END, PASTE_START, paste};
use fux_vt::keys::mouse::mouse_bytes;
use fux_vt::{MouseProtocolEncoding, MouseProtocolMode};
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

/// The inputs from `stream` given in pieces of `size` bytes (whole for 0),
/// with the Escape deadline passing only at the end.
fn decode(stream: &[u8], size: usize) -> Vec<Input> {
    let mut decoder = Decoder::default();
    let mut out = Vec::new();
    match NonZeroUsize::new(size) {
        Some(size) => {
            for piece in pieces(stream, size) {
                decoder.bytes(piece, &mut out);
            }
        }
        None => decoder.bytes(stream, &mut out),
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
            let mut framed = Vec::new();
            paste(text, true, &mut framed);
            let inner = framed
                .strip_prefix(PASTE_START)
                .and_then(|f| f.strip_suffix(PASTE_END))
                .unwrap_or_default();
            let ends = (0..inner.len()).any(|i| {
                inner
                    .get(i..)
                    .is_some_and(|t| t.starts_with(PASTE_END) || t.starts_with(b"\xc2\x9b201~"))
            });
            assert!(!ends, "{text:?} gave {framed:?}");
        }
        if let Input::Mouse(event) = *input {
            // SGR carries every event any report does.
            let mut sgr = Vec::new();
            let sent = mouse_bytes(
                event,
                MouseProtocolMode::AnyMotion,
                MouseProtocolEncoding::Sgr,
                &mut sgr,
            );
            assert!(
                sent || event.button.is_some_and(|b| b.is_wheel()),
                "{event:?}"
            );
            if sent {
                assert_eq!(
                    decode(&sgr, 0),
                    vec![Input::Mouse(event)],
                    "{event:?} {sgr:?}"
                );
            }
        }
    }
});

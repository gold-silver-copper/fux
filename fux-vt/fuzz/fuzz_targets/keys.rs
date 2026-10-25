#![no_main]
//! Input: a byte choosing a piece size (0 is each read whole), a byte of
//! options (bit 0: answers expected from the start; bits 2 to 7, n: a
//! paste limit of 5n bytes, or `PASTE_LIMIT` for 0), then the bytes an
//! attached client's terminal sends, with operations among them: `ff 00`
//! the deadline passes (`timeout`), `ff 01` answers are expected
//! (`expect`), `ff 02` their window passes (`expire`), and `ff ff` a byte
//! 0xff. Read whole, in pieces of the size and a byte at a time, with the
//! operations at the same places, the inputs are the same; the decoder's
//! checker holds after every read. Each mouse report decoded encodes back
//! to an event that decodes alike, and each paste, bracketed again, holds
//! no end marker but its own.
use fux_vt::keys::decode::{Decoder, Input, PASTE_LIMIT};
use fux_vt::keys::encode::{PASTE_END, PASTE_START, paste};
use fux_vt::keys::mouse::{MouseAction, mouse_bytes};
use fux_vt::{MouseProtocolEncoding, MouseProtocolMode};
use libfuzzer_sys::fuzz_target;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};
#[cfg(feature = "checks")]
#[path = "../../tests/corpus/known.rs"]
mod known;

/// Fails on a fault the decoder's checker finds, but those known to fail
/// at BASE.
#[cfg(feature = "checks")]
fn checked(decoder: &Decoder) {
    let report = decoder.check();
    assert!(known::unknown(&report).is_empty(), "{report}");
}

/// `bytes` in pieces of `size`, the last one maybe shorter.
fn pieces(mut rest: &[u8], size: NonZeroUsize) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (piece, after) = rest.split_at_checked(size.get()).unwrap_or((rest, &[]));
        rest = after;
        (!piece.is_empty()).then_some(piece)
    })
}

/// What the client sends, and what happens between: reads and operations.
enum Step {
    Read(Vec<u8>),
    Timeout,
    Expect,
    Expire,
}

/// The stream's steps, `ff` and an operation byte among its bytes.
fn steps(stream: &[u8]) -> Vec<Step> {
    let (mut steps, mut read, mut rest) = (Vec::new(), Vec::new(), stream);
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        let op = match (byte, rest.split_first()) {
            (0xff, Some((&0, tail))) => Some((Step::Timeout, tail)),
            (0xff, Some((&1, tail))) => Some((Step::Expect, tail)),
            (0xff, Some((&2, tail))) => Some((Step::Expire, tail)),
            (0xff, Some((&0xff, tail))) => {
                read.push(0xff);
                rest = tail;
                None
            }
            _ => {
                read.push(byte);
                None
            }
        };
        if let Some((op, tail)) = op {
            rest = tail;
            steps.push(Step::Read(std::mem::take(&mut read)));
            steps.push(op);
        }
    }
    steps.push(Step::Read(read));
    steps
}

/// The inputs from `steps`, each read given in pieces of `size` bytes
/// (whole for 0), with the deadline passing at the end too.
fn decode(steps: &[Step], size: usize, limit: usize, expect: bool, start: Instant) -> Vec<Input> {
    let mut decoder = Decoder::with_paste_limit(limit);
    if expect {
        decoder.expect(start);
    }
    let mut out = Vec::new();
    for step in steps {
        match step {
            Step::Read(bytes) => match NonZeroUsize::new(size) {
                Some(size) => {
                    for piece in pieces(bytes, size) {
                        decoder.bytes(piece, &mut out);
                        #[cfg(feature = "checks")]
                        checked(&decoder);
                    }
                }
                None => decoder.bytes(bytes, &mut out),
            },
            Step::Timeout => decoder.timeout(&mut out),
            Step::Expect => decoder.expect(start),
            Step::Expire => {
                decoder.expire(start.checked_add(Duration::from_secs(60)).unwrap_or(start))
            }
        }
        #[cfg(feature = "checks")]
        checked(&decoder);
    }
    decoder.timeout(&mut out);
    out
}

fuzz_target!(|data: &[u8]| {
    let (Some(&size), Some(&options)) = (data.first(), data.get(1)) else {
        return;
    };
    let steps = steps(data.get(2..).unwrap_or_default());
    let limit = match usize::from(options >> 2) {
        0 => PASTE_LIMIT,
        n => n.saturating_mul(5),
    };
    let (expect, start) = (options & 1 != 0, Instant::now());
    let whole = decode(&steps, 0, limit, expect, start);
    // However the bytes arrive, the same inputs.
    assert_eq!(
        decode(&steps, usize::from(size), limit, expect, start),
        whole
    );
    assert_eq!(decode(&steps, 1, limit, expect, start), whole);
    for input in &whole {
        // PASTE_LIMIT counts the bytes pasted. Invalid UTF-8 becomes U+FFFD,
        // three bytes, so the text is bounded in chars: at most one a byte.
        if let Input::Paste(text) = input {
            assert!(
                text.chars().count() <= limit,
                "{} chars",
                text.chars().count()
            );
            // And in bytes: each byte pasted is at most three of the text,
            // as U+FFFD (`THREATS.md` B23).
            assert!(
                text.len() <= limit.saturating_mul(3),
                "{} bytes",
                text.len()
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
            let (any, encoding) = (MouseProtocolMode::AnyMotion, MouseProtocolEncoding::Sgr);
            let sent = mouse_bytes(event, any, encoding, &mut sgr);
            // The wheel is never released or held.
            let wheel = match event.action {
                MouseAction::Press(_) => false,
                MouseAction::Release(b) | MouseAction::Motion(b) => b.is_some_and(|b| b.is_wheel()),
            };
            assert!(sent || wheel, "{event:?}");
            if sent {
                assert_eq!(
                    decode(&[Step::Read(sgr.clone())], 0, limit, false, start),
                    vec![Input::Mouse(event)],
                    "{event:?} {sgr:?}"
                );
            }
        }
    }
});

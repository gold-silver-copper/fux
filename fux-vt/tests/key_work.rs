//! The key decoder's work on one long read while idle (`threats.md` B24,
//! B25): it decodes the read once, a byte read once, however many answers
//! past their windows it holds, and is not cut into pieces of a held
//! sequence's length, each answer a piece's end cuts decoded again.
#![cfg(feature = "checks")]

use fux_vt::Work;
use fux_vt::keys::decode::Decoder;

#[test]
fn one_long_idle_read_is_decoded_once() {
    // A hundred `CSI ?` answers, each past its window (KEY-5): 30,400 bytes.
    let mut one = b"\x1b[?".to_vec();
    one.extend(std::iter::repeat_n(b'1', 300));
    one.push(b'c');
    let input: Vec<u8> = std::iter::repeat_n(one, 100).flatten().collect();
    let mut decoder = Decoder::default();
    let mut out = Vec::new();
    let before = Work::now();
    decoder.bytes(&input, &mut out);
    let keys = Work::now().since(before).keys;
    let bytes = u64::try_from(input.len()).unwrap_or(u64::MAX);
    assert!(keys >= bytes, "{keys} keys counted for {bytes} bytes");
    assert!(
        keys <= bytes,
        "{keys} keys counted for {bytes} bytes read once"
    );
    assert!(out.is_empty(), "{out:?} typed from answers");
}

/// A read that begins inside an answer held is cut once: what decides the
/// held answer first, then the rest whole, a sequence the first piece
/// begins read again where it lies (`threats.md` B24, decision 821): at
/// most 2 keys a byte and 2 × 258 a read, each of a hundred over-long
/// answers' 4 KiB reads.
#[test]
fn a_read_beginning_inside_a_held_answer_is_cut_once() {
    let mut one = b"\x1b[?".to_vec();
    one.extend(std::iter::repeat_n(b'1', 300));
    one.push(b'c');
    let input: Vec<u8> = std::iter::repeat_n(one, 100).flatten().collect();
    let mut decoder = Decoder::default();
    let mut out = Vec::new();
    let mut rest = input.as_slice();
    while !rest.is_empty() {
        let (read, tail) = rest
            .split_at_checked(4096.min(rest.len()))
            .unwrap_or_default();
        let before = Work::now();
        decoder.bytes(read, &mut out);
        let keys = Work::now().since(before).keys;
        let bound = 2 * u64::try_from(read.len()).unwrap_or(u64::MAX) + 2 * 258;
        assert!(
            keys <= bound,
            "{keys} keys for a read of {}, past {bound}",
            read.len()
        );
        rest = tail;
    }
    assert!(out.is_empty(), "{out:?} typed from answers");
}

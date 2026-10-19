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

//! The key decoder's work on one long read while idle (`THREATS.md` B24,
//! B25): it decodes the read once, a byte read once, however many answers
//! past their windows it holds, and is not cut into pieces of a held
//! sequence's length, each answer a piece's end cuts decoded again.
#![cfg(feature = "checks")]

use fux_vt::keys::decode::Decoder;

#[test]
fn each_read_decodes_once_except_for_its_held_prefix() {
    let one = [b"\x1b[?".as_slice(), &vec![b'1'; 300], b"c"].concat();
    let input: Vec<u8> = std::iter::repeat_n(one, 100).flatten().collect();
    for chunk in [input.len(), 4096] {
        let mut decoder = Decoder::new(fux_vt::keys::decode::Timing::new(35, 5000));
        let mut out = Vec::new();
        let mut rest = input.as_slice();
        while !rest.is_empty() {
            let (read, tail) = rest.split_at_checked(chunk).unwrap_or((rest, &[]));
            let before = fux_vt::work_spent();
            decoder.bytes(read, &mut out);
            let keys = fux_vt::work_spent().saturating_sub(before);
            let bytes = u64::try_from(read.len()).unwrap_or(u64::MAX);
            if chunk == input.len() {
                assert_eq!(keys, bytes, "idle read");
            } else {
                assert!(keys <= 2 * bytes + 2 * 258, "{keys} keys for {bytes} bytes");
            }
            rest = tail;
        }
        assert!(out.is_empty(), "{out:?} typed from answers");
    }
}

//! Test input fed in pieces, as a stream arrives.

/// `bytes` in consecutive pieces of `size`, the last one shorter if it must
/// be: what `chunks` gives, without its panic on a zero size, taken as one.
pub fn pieces(bytes: &[u8], size: usize) -> impl Iterator<Item = &[u8]> {
    let size = size.max(1);
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (piece, after) = rest.split_at_checked(size.min(rest.len()))?;
        rest = after;
        Some(piece)
    })
}

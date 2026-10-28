//! Escapes in the checked terminal-sequence and input tables.
/// Bytes as the data writes them.
pub fn bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = text.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        rest = tail;
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match rest.split_first() {
            Some((b'e', tail)) => {
                out.push(0x1b);
                rest = tail;
            }
            Some((b'x', tail)) => {
                let hex = tail.get(..2).and_then(|h| std::str::from_utf8(h).ok());
                out.push(
                    hex.and_then(|h| u8::from_str_radix(h, 16).ok())
                        .unwrap_or(0),
                );
                rest = tail.get(2..).unwrap_or_default();
            }
            Some((b'\\', tail)) => {
                out.push(b'\\');
                rest = tail;
            }
            _ => out.push(b'\\'),
        }
    }
    out
}

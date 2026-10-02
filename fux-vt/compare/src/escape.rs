//! Bytes as a line a shell passes through unchanged inside single quotes,
//! and back: `\e` for ESC, `\r`, `\n`, `\t`, `\\`, `\'`, `\xNN` for any
//! other control or invalid byte, and `\u{N}` for a character past ASCII.
use std::fmt::Write;

pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\x1b' => out.push_str("\\e"),
                '\r' => out.push_str("\\r"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\\' => out.push_str("\\\\"),
                '\'' => out.push_str("\\'"),
                ' '..='~' => out.push(c),
                c if c.is_ascii() => {
                    let _ = write!(out, "\\x{:02x}", u32::from(c));
                }
                c => {
                    let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
                }
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "\\x{b:02x}");
        }
    }
    out
}

pub fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    let hex =
        |digits: &str| u32::from_str_radix(digits, 16).map_err(|e| format!("{digits:?}: {e}"));
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('e') => out.push(0x1b),
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('\\') => out.push(b'\\'),
            Some('\'') => out.push(b'\''),
            Some('x') => {
                let digits: String = chars.by_ref().take(2).collect();
                let n = hex(&digits)?;
                out.push(u8::try_from(n).map_err(|e| e.to_string())?);
            }
            Some('u') => {
                if chars.next() != Some('{') {
                    return Err("\\u must be followed by {".into());
                }
                let digits: String = chars.by_ref().take_while(|&c| c != '}').collect();
                let c = char::from_u32(hex(&digits)?)
                    .ok_or(format!("\\u{{{digits}}} is not a character"))?;
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            other => return Err(format!("unknown escape \\{}", other.unwrap_or(' '))),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn escaping_round_trips_every_kind_of_byte() {
        let bytes = b"a\x1b[1m\r\n\t\\'\x07\x7f\xff\xe7\x95\x8c\xc3";
        let text = super::escape(bytes);
        assert_eq!(text, "a\\e[1m\\r\\n\\t\\\\\\'\\x07\\x7f\\xff\\u{754c}\\xc3");
        assert_eq!(super::unescape(&text), Ok(bytes.to_vec()));
    }
}

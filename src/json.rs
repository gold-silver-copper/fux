//! A small JSON writer for what `json` gives. Output only: fux never reads JSON.
use std::borrow::Cow;
use std::fmt::Write;

/// A JSON object's fields, in order.
pub type Fields<'a> = Vec<(&'static str, Json<'a>)>;

/// A value to write, its strings borrowed from what it describes where they
/// can be.
#[derive(Clone, Debug, PartialEq)]
pub enum Json<'a> {
    Null,
    Bool(bool),
    Number(i64),
    String(Cow<'a, str>),
    Array(Vec<Json<'a>>),
    Object(Fields<'a>),
    /// JSON written already (an answer's), as it is.
    Raw(Cow<'a, str>),
}

impl<'a> Json<'a> {
    pub fn str(text: impl Into<Cow<'a, str>>) -> Json<'a> {
        Json::String(text.into())
    }

    /// A whole number; one past `i64` is `i64::MAX`.
    pub fn int(n: impl TryInto<i64>) -> Json<'a> {
        Json::Number(n.try_into().unwrap_or(i64::MAX))
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Number(n) => {
                let _ = write!(out, "{n}");
            }
            Json::String(s) => write_str(out, s),
            Json::Raw(json) => out.push_str(json),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(out, key);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// Writes `text` as a JSON string, escaped.
fn write_str(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Standard base64's alphabet, each character's place its value.
const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, for OSC 52.
pub fn base64(bytes: &[u8]) -> String {
    // A capacity hint only.
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    // Three bytes at a time, and what is left over.
    let (whole, rest) = bytes.as_chunks::<3>();
    for chunk in whole
        .iter()
        .map(|chunk| chunk.as_slice())
        .chain((!rest.is_empty()).then_some(rest))
    {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                let index = ((n >> shift) & 63) as usize;
                out.push(char::from(TABLE.get(index).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The bytes `text` encodes in standard base64, as OSC 52 sends them,
/// padded or not; `None` unless it is RFC 4648's encoding of them:
/// - padding only to end the last quantum ("xx==", "xxx="), or none: §3.2's
///   "MUST include appropriate pad characters" binds encoders, and the
///   length alone says how many bytes the last characters hold, so the
///   unpadded form (xterm and kitty take it) means the same bytes; a lone
///   last character holds no whole byte;
/// - "Implementations MUST reject the encoded data if it contains
///   characters outside the base alphabet" (§3.3);
/// - pad bits "MUST be set to zero by conforming encoders", and "decoders
///   MAY chose to reject an encoding if the pad bits have not been set to
///   zero" (§3.5), as this one does, so bytes have one encoding.
pub fn unbase64(text: &[u8]) -> Option<Vec<u8>> {
    let chars = match text {
        [chars @ .., b'=', b'='] | [chars @ .., b'='] if text.len().is_multiple_of(4) => chars,
        chars => chars,
    };
    // A capacity hint only.
    let mut out = Vec::with_capacity(chars.len().div_ceil(4).saturating_mul(3));
    // Four characters at a time, and what is left over.
    let (whole, rest) = chars.as_chunks::<4>();
    for quantum in whole
        .iter()
        .map(|quantum| quantum.as_slice())
        .chain((!rest.is_empty()).then_some(rest))
    {
        if quantum.len() < 2 {
            return None;
        }
        let mut n = 0u32;
        for (c, shift) in quantum.iter().zip([18u32, 12, 6, 0]) {
            let value = TABLE.iter().position(|t| t == c)?;
            n |= u32::try_from(value).ok()? << shift;
        }
        // A zero byte, the bytes, then the pad bits.
        let word = n.to_be_bytes();
        let (bytes, pad) = word.split_at_checked(quantum.len())?;
        if pad.iter().any(|&b| b != 0) {
            return None;
        }
        out.extend_from_slice(bytes.get(1..)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escapes_and_nests() {
        let value = Json::Object(vec![
            ("a", Json::str("q\"\\\n\u{1}界")),
            (
                "b",
                Json::Array(vec![Json::Number(-3), Json::Bool(true), Json::Null]),
            ),
        ]);
        assert_eq!(
            value.render(),
            r#"{"a":"q\"\\\n\u0001界","b":[-3,true,null]}"#
        );
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (input, output) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), output);
            let unpadded = output.trim_end_matches('=');
            for text in [output, unpadded] {
                assert_eq!(unbase64(text.as_bytes()).as_deref(), Some(input.as_bytes()));
            }
        }
        // A lone last character, misplaced or extra padding, pad bits set
        // (padded or not), a character outside the alphabet.
        for bad in [
            &b"Z"[..],
            b"Zm9vY",
            b"Zg=",
            b"Zg===",
            b"Zm9v=",
            b"==",
            b"Z===",
            b"Zm=v",
            b"Zg==Zg==",
            b"Zh==",
            b"Zh",
            b"Zm9=",
            b"Zm9",
            b"Zm9v!A==",
        ] {
            assert_eq!(unbase64(bad), None, "{bad:?}");
        }
    }
}

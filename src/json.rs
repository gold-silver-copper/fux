//! A JSON value: what `json` gives, and what a reply carries of what has
//! no type of its own (a pane's state, a listing, a field), rendered at
//! the client. fux never reads JSON text.
use crate::wire::{Error, Reader, Wire};
use std::fmt::Write;

/// A JSON object's fields as they are built, in order.
pub type Fields = Vec<(&'static str, Json)>;

/// How deep a value read from the wire may nest, each value a level: past
/// any fux makes (`ls json`'s deepest, a picture's number, is ten), so a
/// reply can't make its reader recurse without bound.
const DEEPEST: usize = 32;

/// A JSON value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    /// A whole number that is never negative, `i64`'s reach or not.
    Uint(u64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn str(text: impl Into<String>) -> Json {
        Json::String(text.into())
    }

    pub fn int(n: impl Into<i64>) -> Json {
        Json::Int(n.into())
    }

    pub fn uint(n: impl Into<u64>) -> Json {
        Json::Uint(n.into())
    }

    /// A count or a place in memory, as a `Uint`: every `usize` fits one.
    pub fn count(n: usize) -> Json {
        const _: () = assert!(usize::BITS <= u64::BITS);
        u64::try_from(n).map_or(Json::Null, Json::Uint)
    }

    pub fn object(fields: Fields) -> Json {
        Json::Object(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
    }

    /// The value of field `key`, if it is an object that has one.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            Json::Null
            | Json::Bool(_)
            | Json::Int(_)
            | Json::Uint(_)
            | Json::String(_)
            | Json::Array(_) => None,
        }
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
            Json::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Json::Uint(n) => {
                let _ = write!(out, "{n}");
            }
            Json::String(s) => write_str(out, s),
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

    /// A value read from the wire, at most `depth` deep.
    fn read(r: &mut Reader<'_>, depth: usize) -> Result<Json, Error> {
        let depth = depth.checked_sub(1).ok_or(Error::Bad("JSON depth"))?;
        Ok(match u8::get(r)? {
            0 => Json::Null,
            1 => Json::Bool(bool::get(r)?),
            2 => Json::Int(i64::get(r)?),
            3 => Json::Uint(u64::get(r)?),
            4 => Json::String(String::get(r)?),
            5 => Json::Array(
                (0..u32::get(r)?)
                    .map(|_| Json::read(r, depth))
                    .collect::<Result<_, _>>()?,
            ),
            6 => Json::Object(
                (0..u32::get(r)?)
                    .map(|_| Ok((String::get(r)?, Json::read(r, depth)?)))
                    .collect::<Result<_, _>>()?,
            ),
            tag => return Err(Error::Tag("JSON value", tag)),
        })
    }
}

/// A tag, then its value: an array's or object's count, then its items.
impl Wire for Json {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Json::Null => out.push(0),
            Json::Bool(b) => {
                out.push(1);
                b.put(out);
            }
            Json::Int(n) => {
                out.push(2);
                n.put(out);
            }
            Json::Uint(n) => {
                out.push(3);
                n.put(out);
            }
            Json::String(s) => {
                out.push(4);
                s.put(out);
            }
            Json::Array(items) => {
                out.push(5);
                items.put(out);
            }
            Json::Object(fields) => {
                out.push(6);
                fields.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Json::read(r, DEEPEST)
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
        let value = Json::object(vec![
            ("a", Json::str("q\"\\\n\u{1}界")),
            (
                "b",
                Json::Array(vec![Json::Int(-3), Json::Bool(true), Json::Null]),
            ),
            ("c", Json::uint(u64::MAX)),
        ]);
        assert_eq!(
            value.render(),
            r#"{"a":"q\"\\\n\u0001界","b":[-3,true,null],"c":18446744073709551615}"#
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

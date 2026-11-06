//! Just enough JSON for asciinema's lines: objects, arrays, strings,
//! numbers, booleans and null.
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(map) => map.get(key),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }
}

/// Parses one JSON value, all of `text` but whitespace.
pub fn parse(text: &str) -> Result<Json, String> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        at: 0,
        depth: 0,
    };
    let value = reader.value()?;
    reader.space();
    if reader.at != reader.bytes.len() {
        return Err(format!("unexpected text at byte {}", reader.at));
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    depth: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }
    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.at = self.at.saturating_add(1);
        Some(byte)
    }
    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.bump();
        }
    }
    fn expect(&mut self, want: u8) -> Result<(), String> {
        match self.bump() {
            Some(got) if got == want => Ok(()),
            _ => Err(format!("expected '{}' at byte {}", char::from(want), self.at)),
        }
    }
    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        for &want in word.as_bytes() {
            self.expect(want)?;
        }
        Ok(value)
    }

    fn value(&mut self) -> Result<Json, String> {
        self.space();
        match self.peek() {
            Some(b'{') => self.nested(Self::object),
            Some(b'[') => self.nested(Self::array),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(format!("expected a value at byte {}", self.at)),
        }
    }

    fn nested(&mut self, read: fn(&mut Self) -> Result<Json, String>) -> Result<Json, String> {
        if self.depth >= 64 {
            return Err(String::from("nested too deep"));
        }
        self.depth = self.depth.saturating_add(1);
        let value = read(self);
        self.depth = self.depth.saturating_sub(1);
        value
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut map = BTreeMap::new();
        self.space();
        if self.peek() == Some(b'}') {
            self.bump();
            return Ok(Json::Object(map));
        }
        loop {
            self.space();
            let key = self.string()?;
            self.space();
            self.expect(b':')?;
            map.insert(key, self.value()?);
            self.space();
            match self.bump() {
                Some(b',') => {}
                Some(b'}') => return Ok(Json::Object(map)),
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.at)),
            }
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.space();
        if self.peek() == Some(b']') {
            self.bump();
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.space();
            match self.bump() {
                Some(b',') => {}
                Some(b']') => return Ok(Json::Array(items)),
                _ => return Err(format!("expected ',' or ']' at byte {}", self.at)),
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
            self.bump();
        }
        let text = self.bytes.get(start..self.at).unwrap_or_default();
        std::str::from_utf8(text)
            .ok()
            .and_then(|t| t.parse().ok())
            .map(Json::Number)
            .ok_or_else(|| format!("bad number at byte {start}"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = self
                .bump()
                .and_then(|b| char::from(b).to_digit(16))
                .ok_or_else(|| format!("bad \\u escape at byte {}", self.at))?;
            value = value.wrapping_mul(16).wrapping_add(digit);
        }
        Ok(value)
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            match self.bump().ok_or("unterminated string")? {
                b'"' => return String::from_utf8(out).map_err(|e| e.to_string()),
                b'\\' => {
                    let c = match self.bump().ok_or("unterminated string")? {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode()?,
                        other => return Err(format!("bad escape \\{}", char::from(other))),
                    };
                    let mut utf8 = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
                }
                byte => out.push(byte),
            }
        }
    }

    /// A `\u` escape's character, a surrogate pair joined; a lone
    /// surrogate is U+FFFD, as asciinema's own reader makes it.
    fn unicode(&mut self) -> Result<char, String> {
        let first = self.hex4()?;
        if (0xd800..0xdc00).contains(&first) && self.bytes.get(self.at..self.at.saturating_add(2)) == Some(b"\\u") {
            let save = self.at;
            self.at = self.at.saturating_add(2);
            let second = self.hex4()?;
            if (0xdc00..0xe000).contains(&second) {
                let code = 0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00);
                return Ok(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            self.at = save;
        }
        Ok(char::from_u32(first).unwrap_or('\u{fffd}'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_event_line_with_escapes() -> Result<(), String> {
        let line = r#"[1.5, "o", "\u001b[1mhi\r\n😀 \"q\""]"#;
        let Json::Array(items) = parse(line)? else {
            return Err(String::from("not an array"));
        };
        assert_eq!(items.first().and_then(Json::as_f64), Some(1.5));
        assert_eq!(items.get(2).and_then(Json::as_str), Some("\x1b[1mhi\r\n\u{1f600} \"q\""));
        Ok(())
    }
}

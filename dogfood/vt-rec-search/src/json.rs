//! Just enough JSON for asciicast lines: values, with no limits but depth.

#[derive(Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// `text` as one JSON value, with nothing but space after it.
pub fn parse(text: &str) -> Result<Json, String> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        at: 0,
    };
    let value = reader.value(0)?;
    reader.space();
    match reader.at == reader.bytes.len() {
        true => Ok(value),
        false => Err(format!("text after the value at byte {}", reader.at)),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

const DEPTH: usize = 64;

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
            self.at = self.at.saturating_add(1);
        }
    }
    fn expect(&mut self, word: &str) -> Result<(), String> {
        let end = self.at.saturating_add(word.len());
        match self.bytes.get(self.at..end) {
            Some(found) if found == word.as_bytes() => {
                self.at = end;
                Ok(())
            }
            _ => Err(format!("expected {word} at byte {}", self.at)),
        }
    }
    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > DEPTH {
            return Err("nested too deeply".into());
        }
        self.space();
        match self.peek().ok_or("unexpected end")? {
            b'n' => self.expect("null").map(|()| Json::Null),
            b't' => self.expect("true").map(|()| Json::Bool(true)),
            b'f' => self.expect("false").map(|()| Json::Bool(false)),
            b'"' => self.string().map(Json::Str),
            b'[' => {
                self.bump();
                let mut items = Vec::new();
                self.space();
                if self.peek() == Some(b']') {
                    self.bump();
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth.saturating_add(1))?);
                    self.space();
                    match self.bump() {
                        Some(b',') => {}
                        Some(b']') => return Ok(Json::Arr(items)),
                        _ => return Err(format!("expected , or ] at byte {}", self.at)),
                    }
                }
            }
            b'{' => {
                self.bump();
                let mut fields = Vec::new();
                self.space();
                if self.peek() == Some(b'}') {
                    self.bump();
                    return Ok(Json::Obj(fields));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.space();
                    self.expect(":")?;
                    fields.push((key, self.value(depth.saturating_add(1))?));
                    self.space();
                    match self.bump() {
                        Some(b',') => {}
                        Some(b'}') => return Ok(Json::Obj(fields)),
                        _ => return Err(format!("expected , or }} at byte {}", self.at)),
                    }
                }
            }
            _ => self.number(),
        }
    }
    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        while matches!(
            self.peek(),
            Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        ) {
            self.at = self.at.saturating_add(1);
        }
        let text = std::str::from_utf8(self.bytes.get(start..self.at).unwrap_or_default())
            .map_err(|e| e.to_string())?;
        text.parse()
            .map(Json::Num)
            .map_err(|_| format!("bad value at byte {start}"))
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let end = self.at.saturating_add(4);
        let digits = self.bytes.get(self.at..end).ok_or("short \\u escape")?;
        let text = std::str::from_utf8(digits).map_err(|e| e.to_string())?;
        self.at = end;
        u32::from_str_radix(text, 16).map_err(|e| e.to_string())
    }
    fn string(&mut self) -> Result<String, String> {
        if self.bump() != Some(b'"') {
            return Err(format!("expected a string at byte {}", self.at));
        }
        let mut out = Vec::new();
        loop {
            match self.bump().ok_or("unended string")? {
                b'"' => return String::from_utf8(out).map_err(|e| e.to_string()),
                b'\\' => {
                    let c = match self.bump().ok_or("unended escape")? {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let high = self.hex4()?;
                            let code =
                                if (0xd800..0xdc00).contains(&high) && self.peek() == Some(b'\\') {
                                    self.expect("\\u")?;
                                    let low = self.hex4()?;
                                    0x10000 + ((high - 0xd800) << 10) + low.saturating_sub(0xdc00)
                                } else {
                                    high
                                };
                            char::from_u32(code).unwrap_or('\u{fffd}')
                        }
                        other => char::from(other),
                    };
                    let mut utf8 = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
                }
                byte => out.push(byte),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asciicast_lines_parse_with_escapes() {
        let header =
            parse(r#"{"version": 2, "width": 80, "height": 24, "env": {"TERM": "xterm"}}"#);
        let header = header.unwrap_or(Json::Null);
        assert_eq!(header.get("width").and_then(Json::num), Some(80.0));
        let event = parse(r#"[0.25, "o", "\u001b[1mhi😀\"\n"]"#);
        assert_eq!(
            event,
            Ok(Json::Arr(vec![
                Json::Num(0.25),
                Json::Str("o".into()),
                Json::Str("\x1b[1mhi😀\"\n".into())
            ]))
        );
        assert!(parse("[1, 2").is_err());
    }
}

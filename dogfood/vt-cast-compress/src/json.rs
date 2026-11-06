//! The little JSON an asciinema cast needs: a value parser and a string writer.

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// A number as written, so a time keeps its exact digits.
    Number(String),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(n) => n.parse().ok(),
            _ => None,
        }
    }
}

/// Parses one whole JSON text.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser {
        bytes: text.as_bytes(),
        at: 0,
    };
    let value = p.value(0)?;
    p.blank();
    if p.at != p.bytes.len() {
        return Err(format!("trailing text at byte {}", p.at));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

/// Deeper nesting than any cast has is refused, not recursed into.
const MAX_DEPTH: usize = 64;

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn blank(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> Result<(), String> {
        self.blank();
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", byte as char, self.at))
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        if self.bytes.get(self.at..self.at + word.len()) == Some(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(format!("bad literal at byte {}", self.at))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("nested too deep".into());
        }
        self.blank();
        match self.peek() {
            Some(b'n') => self.literal("null", Value::Null),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'"') => self.string().map(Value::Str),
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.blank();
                if self.peek() == Some(b']') {
                    self.at += 1;
                    return Ok(Value::Array(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.blank();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Value::Array(items));
                        }
                        _ => return Err(format!("expected ',' or ']' at byte {}", self.at)),
                    }
                }
            }
            Some(b'{') => {
                self.at += 1;
                let mut fields = Vec::new();
                self.blank();
                if self.peek() == Some(b'}') {
                    self.at += 1;
                    return Ok(Value::Object(fields));
                }
                loop {
                    self.blank();
                    let key = self.string()?;
                    self.eat(b':')?;
                    fields.push((key, self.value(depth + 1)?));
                    self.blank();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Value::Object(fields));
                        }
                        _ => return Err(format!("expected ',' or '}}' at byte {}", self.at)),
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => {
                let start = self.at;
                while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.at += 1;
                }
                let raw = std::str::from_utf8(self.bytes.get(start..self.at).unwrap_or_default())
                    .map_err(|e| e.to_string())?;
                Ok(Value::Number(raw.to_owned()))
            }
            _ => Err(format!("unexpected input at byte {}", self.at)),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .and_then(|d| std::str::from_utf8(d).ok())
            .and_then(|d| u32::from_str_radix(d, 16).ok())
            .ok_or_else(|| format!("bad \\u escape at byte {}", self.at))?;
        self.at += 4;
        Ok(digits)
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err(format!("expected a string at byte {}", self.at));
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            let start = self.at;
            while !matches!(self.peek(), Some(b'"' | b'\\') | None) {
                self.at += 1;
            }
            let run = self.bytes.get(start..self.at).unwrap_or_default();
            out.push_str(std::str::from_utf8(run).map_err(|e| e.to_string())?);
            match self.peek() {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let escape = self.peek().ok_or("unterminated escape")?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let high = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&high)
                                && self.bytes.get(self.at..self.at + 2) == Some(b"\\u")
                            {
                                self.at += 2;
                                let low = self.hex4()?;
                                0x10000 + ((high - 0xd800) << 10) + (low.wrapping_sub(0xdc00) & 0x3ff)
                            } else {
                                high
                            };
                            // A lone surrogate, as a recorder that split a pair writes.
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(format!("bad escape at byte {}", self.at)),
                    }
                }
                _ => return Err("unterminated string".into()),
            }
        }
    }
}

/// Appends `text` as a JSON string, as asciinema writes it.
pub fn write_string(out: &mut String, text: &str) {
    use std::fmt::Write;
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

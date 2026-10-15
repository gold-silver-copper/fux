//! Just enough JSON for the oracle: the fixture's side log, fux's answers
//! (`json`), and the corpus of shrunk cases.
use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Integers alone: no JSON here holds another number.
    Number(i64),
    Str(String),
    List(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(map) => map.get(key),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Str(_) | Value::List(_) => {
                None
            }
        }
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(Value::Number(n)) => Some(*n),
            _ => None,
        }
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn list(&self, key: &str) -> &[Value] {
        match self.get(key) {
            Some(Value::List(items)) => items,
            _ => &[],
        }
    }

    /// The value as JSON text, on one line.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out);
        out
    }

    fn render_into(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => {
                let _ = write!(out, "{n}");
            }
            Value::Str(s) => quote(s, out),
            Value::List(items) => {
                out.push('[');
                for (n, item) in items.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    item.render_into(out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (n, (key, value)) in map.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    quote(key, out);
                    out.push(':');
                    value.render_into(out);
                }
                out.push('}');
            }
        }
    }
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parses one JSON value.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut parser = Parser {
        chars: text.chars().collect(),
        at: 0,
    };
    let value = parser.value()?;
    parser.blanks();
    match parser.peek() {
        None => Ok(value),
        Some(c) => Err(format!("{c:?} after the value in {text:?}")),
    }
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        self.at = self.at.saturating_add(1);
        c
    }

    fn blanks(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at = self.at.saturating_add(1);
        }
    }

    fn expect(&mut self, want: char) -> Result<(), String> {
        match self.next() {
            Some(c) if c == want => Ok(()),
            other => Err(format!("expected {want:?}, found {other:?}")),
        }
    }

    fn word(&mut self, word: &str, value: Value) -> Result<Value, String> {
        for want in word.chars() {
            self.expect(want)?;
        }
        Ok(value)
    }

    fn value(&mut self) -> Result<Value, String> {
        self.blanks();
        match self.peek() {
            Some('n') => self.word("null", Value::Null),
            Some('t') => self.word("true", Value::Bool(true)),
            Some('f') => self.word("false", Value::Bool(false)),
            Some('"') => self.string().map(Value::Str),
            Some('[') => {
                self.at = self.at.saturating_add(1);
                let mut items = Vec::new();
                self.blanks();
                if self.peek() == Some(']') {
                    self.at = self.at.saturating_add(1);
                    return Ok(Value::List(items));
                }
                loop {
                    items.push(self.value()?);
                    self.blanks();
                    match self.next() {
                        Some(',') => {}
                        Some(']') => return Ok(Value::List(items)),
                        other => return Err(format!("{other:?} in a list")),
                    }
                }
            }
            Some('{') => {
                self.at = self.at.saturating_add(1);
                let mut map = BTreeMap::new();
                self.blanks();
                if self.peek() == Some('}') {
                    self.at = self.at.saturating_add(1);
                    return Ok(Value::Object(map));
                }
                loop {
                    self.blanks();
                    let key = self.string()?;
                    self.blanks();
                    self.expect(':')?;
                    let value = self.value()?;
                    map.insert(key, value);
                    self.blanks();
                    match self.next() {
                        Some(',') => {}
                        Some('}') => return Ok(Value::Object(map)),
                        other => return Err(format!("{other:?} in an object")),
                    }
                }
            }
            Some(_) => self.number(),
            None => Err("the text ended before a value".into()),
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let mut text = String::new();
        while let Some(c) = self
            .peek()
            .filter(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
        {
            text.push(c);
            self.at = self.at.saturating_add(1);
        }
        text.parse()
            .map(Value::Number)
            .map_err(|_| format!("not an integer: {text:?}"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.next() {
                None => return Err("a string never ended".into()),
                Some('"') => return Ok(out),
                Some('\\') => match self.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('u') => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let digit = self.next().and_then(|c| c.to_digit(16));
                            let digit = digit.ok_or("a bad \\u escape")?;
                            code = code.saturating_mul(16).saturating_add(digit);
                        }
                        // A surrogate pair: the low half follows.
                        if (0xd800..0xdc00).contains(&code) {
                            self.expect('\\')?;
                            self.expect('u')?;
                            let mut low = 0u32;
                            for _ in 0..4 {
                                let digit = self.next().and_then(|c| c.to_digit(16));
                                let digit = digit.ok_or("a bad \\u escape")?;
                                low = low.saturating_mul(16).saturating_add(digit);
                            }
                            let high = code.saturating_sub(0xd800).saturating_mul(0x400);
                            code = high
                                .saturating_add(low.saturating_sub(0xdc00))
                                .saturating_add(0x10000);
                        }
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    Some(c) => out.push(c),
                    None => return Err("a string ended in an escape".into()),
                },
                Some(c) => out.push(c),
            }
        }
    }
}

/// An object from its fields.
pub fn object<'a>(fields: impl IntoIterator<Item = (&'a str, Value)>) -> Value {
    Value::Object(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

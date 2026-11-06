//! Terminal recordings, read into steps to replay: asciinema casts (v2 and
//! v3), whose events are steps, and raw output (`script`'s typescript, or
//! any bytes a program wrote), whose lines are steps.
use fux_vt::Size;

/// One thing a recording did to its terminal.
pub enum What {
    Output(Vec<u8>),
    Resize(Size),
}

pub struct Step {
    /// Seconds since the recording began, where it says.
    pub time: Option<f64>,
    /// How many output bytes came before this step.
    pub offset: usize,
    pub what: What,
}

pub struct Recording {
    /// The terminal's size, where the recording says it.
    pub size: Option<Size>,
    pub steps: Vec<Step>,
}

/// Reads `bytes` as a cast if its first line is a cast's header, else as
/// raw output.
pub fn parse(bytes: &[u8]) -> Result<Recording, String> {
    let first = bytes.split(|&b| b == b'\n').next().unwrap_or_default();
    if first.starts_with(b"{") && first.windows(9).any(|w| w == b"\"version\"") {
        cast(bytes)
    } else {
        Ok(raw(bytes))
    }
}

/// Raw output, without the lines `script` adds before and after it.
fn raw(mut bytes: &[u8]) -> Recording {
    if bytes.starts_with(b"Script started on ") {
        let body = bytes
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |n| n + 1);
        bytes = bytes.get(body..).unwrap_or_default();
    }
    if let Some(end) = rfind(bytes, b"\nScript done on ") {
        bytes = bytes.get(..=end).unwrap_or_default();
    }
    let mut steps = Vec::new();
    let mut offset = 0;
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        steps.push(Step {
            time: None,
            offset,
            what: What::Output(line.to_vec()),
        });
        offset += line.len();
    }
    Recording { size: None, steps }
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// An asciinema cast: a header line, then one event a line. v2 times are
/// from the start, v3 times from the event before.
fn cast(bytes: &[u8]) -> Result<Recording, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("a cast is UTF-8: {e}"))?;
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty());
    let (_, header) = lines.next().ok_or("an empty cast")?;
    let header = Json::parse(header).map_err(|e| format!("line 1: {e}"))?;
    let version = header.get("version").and_then(Json::number);
    let (cols, rows) = match version {
        Some(2.0) => (header.get("width"), header.get("height")),
        Some(3.0) => {
            let term = header.get("term");
            (
                term.and_then(|t| t.get("cols")),
                term.and_then(|t| t.get("rows")),
            )
        }
        _ => return Err("only asciicast versions 2 and 3 are read".into()),
    };
    let size = match (cols.and_then(Json::number), rows.and_then(Json::number)) {
        (Some(cols), Some(rows)) => Some(size(rows, cols)?),
        _ => None,
    };
    let relative = version == Some(3.0);
    let mut steps = Vec::new();
    let (mut now, mut offset) = (0.0, 0);
    for (n, line) in lines {
        let at = |e: String| format!("line {}: {e}", n + 1);
        let event = Json::parse(line).map_err(at)?;
        let Json::Array(fields) = &event else {
            return Err(at("an event is an array".into()));
        };
        let (Some(time), Some(Json::String(code)), Some(Json::String(data))) = (
            fields.first().and_then(Json::number),
            fields.get(1),
            fields.get(2),
        ) else {
            return Err(at("an event is [time, code, data]".into()));
        };
        now = if relative { now + time } else { time };
        let what = match code.as_str() {
            "o" => What::Output(data.clone().into_bytes()),
            "r" => {
                let (cols, rows) = data
                    .split_once('x')
                    .ok_or_else(|| at(format!("resize {data:?}")))?;
                let number = |s: &str| {
                    s.trim()
                        .parse::<f64>()
                        .map_err(|e| at(format!("resize {data:?}: {e}")))
                };
                What::Resize(self::size(number(rows)?, number(cols)?).map_err(at)?)
            }
            // Input, markers and exits change no screen.
            _ => continue,
        };
        let len = match &what {
            What::Output(bytes) => bytes.len(),
            What::Resize(_) => 0,
        };
        steps.push(Step {
            time: Some(now),
            offset,
            what,
        });
        offset += len;
    }
    Ok(Recording { size, steps })
}

pub fn size(rows: f64, cols: f64) -> Result<Size, String> {
    let whole =
        |n: f64| (n.fract() == 0.0 && (1.0..=f64::from(u16::MAX)).contains(&n)).then_some(n as u16);
    whole(rows)
        .zip(whole(cols))
        .and_then(|(r, c)| Size::new(r, c).ok())
        .ok_or_else(|| format!("no terminal is {rows} by {cols}"))
}

/// What a cast's lines hold: JSON, of which only what casts use is read
/// into anything but its shape.
#[derive(Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Reader {
            s: text.as_bytes(),
            i: 0,
        };
        let value = p.value(0)?;
        p.space();
        if p.i == p.s.len() {
            Ok(value)
        } else {
            Err(format!("unexpected text at byte {}", p.i))
        }
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn number(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }
}

/// How deep arrays and objects may nest: a cast's go two deep.
const DEPTH: usize = 64;

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> Result<(), String> {
        self.space();
        if self.peek() == Some(byte) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", byte as char, self.i))
        }
    }

    fn word(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.s.get(self.i..self.i + word.len()) == Some(word.as_bytes()) {
            self.i += word.len();
            Ok(value)
        } else {
            Err(format!("unexpected text at byte {}", self.i))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > DEPTH {
            return Err("nested too deep".into());
        }
        self.space();
        match self.peek().ok_or("unexpected end")? {
            b'n' => self.word("null", Json::Null),
            b't' => self.word("true", Json::Bool(true)),
            b'f' => self.word("false", Json::Bool(false)),
            b'"' => self.string().map(Json::String),
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.space();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(Json::Array(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.space();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        _ => break,
                    }
                }
                self.eat(b']')?;
                Ok(Json::Array(items))
            }
            b'{' => {
                self.i += 1;
                let mut fields = Vec::new();
                self.space();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(Json::Object(fields));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.eat(b':')?;
                    fields.push((key, self.value(depth + 1)?));
                    self.space();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        _ => break,
                    }
                }
                self.eat(b'}')?;
                Ok(Json::Object(fields))
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        while matches!(
            self.peek(),
            Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        ) {
            self.i += 1;
        }
        let text = std::str::from_utf8(self.s.get(start..self.i).unwrap_or_default()).unwrap_or("");
        text.parse()
            .map(Json::Number)
            .map_err(|_| format!("unexpected text at byte {start}"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.s.get(self.i..self.i + 4).ok_or("a short \\u escape")?;
        let text = std::str::from_utf8(digits).map_err(|_| "a bad \\u escape")?;
        self.i += 4;
        u32::from_str_radix(text, 16).map_err(|_| "a bad \\u escape".into())
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out = Vec::new();
        loop {
            let byte = self.peek().ok_or("an unended string")?;
            self.i += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let escape = self.peek().ok_or("an unended string")?;
                    self.i += 1;
                    let c = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let high = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&high)
                                && self.s.get(self.i..self.i + 2) == Some(b"\\u")
                            {
                                self.i += 2;
                                let low = self.hex4()?;
                                0x10000
                                    + ((high - 0xd800) << 10)
                                    + (low.wrapping_sub(0xdc00) & 0x3ff)
                            } else {
                                high
                            };
                            char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
                        }
                        other => return Err(format!("a bad escape \\{}", other as char)),
                    };
                    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
                other => out.push(other),
            }
        }
        String::from_utf8(out).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "a test")]
mod tests {
    use super::*;

    #[test]
    fn a_v2_cast_reads_as_steps_with_its_size() -> Result<(), String> {
        let cast = "{\"version\": 2, \"width\": 10, \"height\": 3}\n\
            [0.5, \"o\", \"a\\u001b[1mb\\ud83d\\ude00\"]\n\
            [0.7, \"i\", \"typed\"]\n\
            [1.0, \"r\", \"20x4\"]\n";
        let recording = parse(cast.as_bytes())?;
        assert_eq!(recording.size, Size::new(3, 10).ok());
        let [first, second] = recording.steps.as_slice() else {
            return Err("two steps".into());
        };
        assert!(matches!(&first.what, What::Output(b) if b == "a\x1b[1mb\u{1f600}".as_bytes()));
        assert!(matches!(&second.what, What::Resize(s) if Size::new(4, 20).ok() == Some(*s)));
        assert_eq!((second.time, second.offset), (Some(1.0), 10));
        Ok(())
    }
}

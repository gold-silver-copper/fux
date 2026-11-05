//! Owned arguments at the native request boundary, never a language stack.
use super::Fux;
use fux_wire::code::Code;
use std::rc::Rc;
#[derive(Clone, Debug, PartialEq)]
pub enum Value<A = Fux> {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Rc<String>),
    Symbol(Rc<String>),
    List(Rc<List<A>>),
    App(A),
    Bytes(Rc<Vec<u8>>),
    Printed(Rc<String>),
}
#[derive(Clone, Debug, PartialEq)]
pub struct List<A> {
    values: Vec<Value<A>>,
}
impl<A> List<A> {
    pub fn new(values: Vec<Value<A>>) -> Self {
        Self { values }
    }
    pub fn items(&self) -> &[Value<A>] {
        &self.values
    }
}
#[derive(Clone, Debug)]
pub struct Failure {
    pub code: Code,
    pub message: String,
}
impl Failure {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl From<fux_wire::code::Error> for Failure {
    fn from(e: fux_wire::code::Error) -> Self {
        Self::new(e.code(), e.to_string())
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Failure {}
pub enum Done<A> {
    Now,
    Later(A),
}
impl Value<Fux> {
    pub fn str(text: impl Into<String>) -> Self {
        Self::Str(Rc::new(text.into()))
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Int(_) => "integer",
            Self::Float(_) => "float",
            Self::Bool(_) => "boolean",
            Self::Str(_) => "string",
            Self::Symbol(_) => "symbol",
            Self::List(_) => "list",
            Self::App(v) => v.kind(),
            Self::Bytes(_) => "bytes",
            Self::Printed(_) => "grift value",
        }
    }
    pub fn text(&self) -> Result<String, Failure> {
        let mut out = String::new();
        self.write_text(&mut out)?;
        Ok(out)
    }
    pub fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), Failure> {
        let result = match self {
            Self::Str(s) | Self::Symbol(s) | Self::Printed(s) => out.write_str(s),
            Self::App(v) => return v.write_text(out),
            _ => self.write_data(out),
        };
        result.map_err(|_| Failure::new(Code::Output, "writing value"))
    }
    pub fn write_data(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        match self {
            Self::Printed(text) => out.write_str(text),
            Self::Float(n) if n.is_nan() => out.write_str("#not-a-number"),
            Self::Float(n) if *n == f64::INFINITY => out.write_str("#infinity"),
            Self::Float(n) if *n == f64::NEG_INFINITY => out.write_str("#-infinity"),
            Self::Float(n) => write!(out, "{n:?}"),
            Self::Int(n) => write!(out, "{n}"),
            Self::Bool(b) => write!(out, "#{b}"),
            Self::Str(s) => write_quoted(s, out),
            Self::Symbol(s) => out.write_str(s),
            Self::List(xs) => {
                out.write_str("(")?;
                for (i, v) in xs.items().iter().enumerate() {
                    if i > 0 {
                        out.write_str(" ")?;
                    }
                    v.write_data(out)?;
                }
                out.write_str(")")
            }
            Self::Bytes(b) => {
                out.write_str("#bytes(")?;
                for (i, v) in b.iter().enumerate() {
                    if i > 0 {
                        out.write_str(" ")?;
                    }
                    write!(out, "{v}")?;
                }
                out.write_str(")")
            }
            Self::App(Fux::Record(r)) => r.write(out),
            Self::App(Fux::Null) => out.write_str("none"),
            Self::App(v) => v.write_text(out).map_err(|_| std::fmt::Error),
        }
    }
}

fn write_quoted(text: &str, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
    out.write_char('"')?;
    for character in text.chars() {
        match character {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            c if c.is_control() => write!(out, "\\x{:x};", u32::from(c))?,
            c => out.write_char(c)?,
        }
    }
    out.write_char('"')
}
/// One grift string literal, also used by generated config lines.
pub fn quoted(text: &str) -> String {
    let mut out = String::new();
    let _ = write_quoted(text, &mut out);
    out
}

pub(super) struct Text<'a>(pub &'a Value<Fux>);
impl std::fmt::Display for Text<'_> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.write_text(out).map_err(|_| std::fmt::Error)
    }
}

//! A value as one line of JSON: what a list in a list left on the stack
//! prints as.
use crate::words::File;
use fuxth::{Failure, Value};
use std::fmt::Write;

/// Writes `value` to `out` as JSON: integers and booleans as themselves,
/// strings escaped, lists as arrays, anything else as its text in a
/// string. Lists in lists are walked with a stack of their own, never Rust
/// recursion.
pub fn write(value: &Value<File>, out: &mut String) -> Result<(), Failure> {
    let mut lists: Vec<(std::slice::Iter<'_, Value<File>>, bool)> = Vec::new();
    let mut value = value;
    loop {
        match value {
            Value::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Value::Bool(b) => {
                let _ = write!(out, "{b}");
            }
            Value::Str(s) => string(s, out),
            Value::List(list) => {
                out.push('[');
                lists.push((list.items().iter(), false));
            }
            Value::App(_) | Value::Xt(_) | Value::Mark(_) | Value::Failure(_) | Value::Addr(_) => {
                string(&value.text()?, out)
            }
        }
        // The next item of the innermost list under way.
        value = loop {
            let Some((items, started)) = lists.last_mut() else {
                return Ok(());
            };
            if let Some(item) = items.next() {
                if std::mem::replace(started, true) {
                    out.push(',');
                }
                break item;
            }
            lists.pop();
            out.push(']');
        };
    }
}

/// `text` as a JSON string.
fn string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

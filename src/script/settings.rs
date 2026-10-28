//! Checked fuxth values for local preferences and request options.
use super::Fux;
use crate::command::{EnvName, Limit, MAX_WAIT, Number, Program};
use fuxth::Value;

/// A local setting's type, read once as `set` is given it.
pub(crate) trait Reads: Sized {
    fn takes() -> String;
    fn read(value: &Value<Fux>) -> Result<Self, String>;
}

/// Why `value` is not what a setting takes.
fn wrong(takes: &str, value: &Value<Fux>) -> String {
    format!("takes {takes}, not {}", value.kind())
}

/// An integer.
fn int(value: &Value<Fux>) -> Result<i64, String> {
    let Value::Int(n) = value else {
        return Err(wrong("a number", value));
    };
    Ok(*n)
}

/// A list of strings, each as it is.
fn strings(value: &Value<Fux>) -> Result<Vec<String>, String> {
    let takes = "a list of strings";
    let Value::List(list) = value else {
        return Err(wrong(takes, value));
    };
    (list.items().iter())
        .map(|item| {
            let Value::Str(s) = item else {
                return Err(wrong(takes, item));
            };
            Ok(s.to_string())
        })
        .collect()
}

impl<const MIN: usize, const MAX: usize> Reads for Number<MIN, MAX> {
    fn takes() -> String {
        format!("a number, {MIN} to {MAX}")
    }
    fn read(value: &Value<Fux>) -> Result<Self, String> {
        let n = int(value)?;
        (usize::try_from(n).ok().and_then(Number::new))
            .ok_or_else(|| format!("{n} is out of range, {MIN} to {MAX}"))
    }
}

/// A wait's limit, in seconds.
impl Reads for Limit {
    fn takes() -> String {
        format!("seconds, 0 to {MAX_WAIT}")
    }
    fn read(value: &Value<Fux>) -> Result<Limit, String> {
        let n = int(value)?;
        let secs = u64::try_from(n).map_err(|_| format!("{n} s: a wait waits 0 s or more"))?;
        Limit::read(secs)
    }
}

/// `true` or `false`.
impl Reads for bool {
    fn takes() -> String {
        "true or false".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<bool, String> {
        let Value::Bool(b) = value else {
            return Err(wrong("true or false", value));
        };
        Ok(*b)
    }
}

/// The program, then its arguments: a list of strings, not empty.
impl Reads for Program {
    fn takes() -> String {
        "a list of strings: a program, then its arguments".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Program, String> {
        let mut words = strings(value)?.into_iter();
        let program = words.next().ok_or("a program is needed")?;
        Program::new(program, words.collect())
    }
}

/// Variables' names: a list of strings.
impl Reads for Vec<EnvName> {
    fn takes() -> String {
        "a list of variables' names".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Vec<EnvName>, String> {
        let names = strings(value)?;
        if names.len() > crate::command::EnvPatch::MAX_NAMES {
            return Err("environment selection exceeds 1024 names".to_owned());
        }
        let mut seen = std::collections::BTreeSet::new();
        names
            .iter()
            .map(|name| {
                if !seen.insert(name) {
                    return Err(format!("{name}: repeated environment name"));
                }
                name.parse()
            })
            .collect()
    }
}

//! Checked fuxth values for local preferences and request options.
use super::Fux;
use crate::command::{EnvName, Limit, MAX_WAIT, Number, Program};
use fuxth::Value;

/// A local setting's type, read once as `set` is given it.
pub(crate) trait Reads: Sized {
    fn takes() -> String;
    fn read(value: &Value<Fux>) -> Result<Self, String>;
    fn update(value: &Value<Fux>, _previous: Option<&Self>) -> Result<Self, String> {
        Self::read(value)
    }
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

impl Reads for crate::command::ScrollLines {
    fn takes() -> String {
        "a number, 0 to 32767".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Self, String> {
        let n = int(value)?;
        usize::try_from(n)
            .ok()
            .and_then(Self::new)
            .ok_or_else(|| format!("{n} is out of range, 0 to 32767"))
    }
}

fn items(value: &Value<Fux>) -> Result<&[Value<Fux>], String> {
    let Value::List(list) = value else {
        return Err(wrong("a list", value));
    };
    Ok(list.items())
}
fn word(value: &Value<Fux>) -> Result<&str, String> {
    let Value::Str(text) = value else {
        return Err(wrong("a string", value));
    };
    Ok(text)
}
fn colour(value: &Value<Fux>) -> Result<fux_vt::Color, String> {
    use fux_vt::Color;
    match value {
        Value::Int(n) => u8::try_from(*n)
            .map(Color::Idx)
            .map_err(|_| "a colour index is 0 to 255".to_owned()),
        Value::Str(text) => {
            let text: &str = text;
            if text == "default" {
                return Ok(Color::Default);
            }
            let hex = text
                .strip_prefix('#')
                .filter(|s| s.len() == 6 && s.bytes().all(|c| c.is_ascii_hexdigit()));
            let rgb = hex
                .and_then(|s| u32::from_str_radix(s, 16).ok())
                .ok_or("a colour is 0 to 255, default, or #rrggbb")?;
            let [_, r, g, b] = rgb.to_be_bytes();
            Ok(Color::Rgb(fux_vt::Rgb { r, g, b }))
        }
        Value::Bool(_) | Value::List(_) | Value::App(_) => {
            Err(wrong("a colour: 0 to 255, default, or #rrggbb", value))
        }
    }
}

impl Reads for crate::presentation::Styles {
    fn takes() -> String {
        "role rows: [name foreground background bold/dim/italic/underline...]".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Self, String> {
        Self::update(value, None)
    }
    fn update(value: &Value<Fux>, previous: Option<&Self>) -> Result<Self, String> {
        let rows = items(value)?;
        if rows.len() > Self::NAMES.len() {
            return Err("at most eight style roles".to_owned());
        }
        let mut changes = std::collections::BTreeMap::new();
        for row in rows {
            let [name, fg, bg, flags @ ..] = items(row)? else {
                return Err(Self::takes());
            };
            let name = word(name)?;
            if !Self::NAMES.contains(&name) {
                return Err(format!(
                    "no style role {name:?}; the roles are {}",
                    Self::NAMES.join(" ")
                ));
            }
            let mut style = crate::presentation::Style {
                foreground: colour(fg)?,
                background: colour(bg)?,
                bold: false,
                dim: false,
                italic: false,
                underline: false,
            };
            for flag in flags {
                let flag = word(flag)?;
                let field = match flag {
                    "bold" => &mut style.bold,
                    "dim" => &mut style.dim,
                    "italic" => &mut style.italic,
                    "underline" => &mut style.underline,
                    _ => {
                        return Err(format!(
                            "no style flag {flag:?}; the flags are bold dim italic underline"
                        ));
                    }
                };
                if *field {
                    return Err(format!("repeated style flag {flag}"));
                }
                *field = true;
            }
            if changes.insert(name, style).is_some() {
                return Err(format!("repeated style role {name}"));
            }
        }
        Self::read_roles(|name| {
            changes
                .get(name)
                .copied()
                .or_else(|| previous.and_then(|p| p.role(name)))
                .ok_or_else(|| format!("bundled initialization did not set style {name}"))
        })
    }
}

impl Reads for crate::presentation::Bar {
    fn takes() -> String {
        "[top/bottom/off [left fields] [right fields] tab-numbers?]; fields: workspace tabs pane [slot 0..15]".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Self, String> {
        use crate::presentation::{BarPosition, Field};
        let [position, left, right, numbers] = items(value)? else {
            return Err(Self::takes());
        };
        let position = match word(position)? {
            "top" => BarPosition::Top,
            "bottom" => BarPosition::Bottom,
            "off" => BarPosition::Off,
            _ => return Err("bar position is top, bottom, or off".to_owned()),
        };
        let fields = |value| -> Result<Vec<Field>, String> {
            let values = items(value)?;
            if values.len() > Self::MAX_FIELDS {
                return Err("a bar has at most 16 fields".to_owned());
            }
            values
                .iter()
                .map(|value| match value {
                    Value::Str(_) => match word(value)? {
                        "workspace" => Ok(Field::Workspace),
                        "tabs" => Ok(Field::Tabs),
                        "pane" => Ok(Field::Pane),
                        name => Err(format!(
                            "no bar field {name:?}; fields are workspace tabs pane [slot 0..15]"
                        )),
                    },
                    Value::List(_) => {
                        let [name, index] = items(value)? else {
                            return Err("a slot field is [slot 0..15]".to_owned());
                        };
                        if word(name)? != "slot" {
                            return Err("a slot field is [slot 0..15]".to_owned());
                        }
                        let id = u8::try_from(int(index)?).map_err(|_| "a bar slot is 0 to 15")?;
                        Ok(Field::Slot(id))
                    }
                    Value::Int(_) | Value::Bool(_) | Value::App(_) => {
                        Err("a bar field is a name or [slot 0..15]".to_owned())
                    }
                })
                .collect()
        };
        Self::new(
            position,
            fields(left)?,
            fields(right)?,
            bool::read(numbers)?,
        )
    }
}

impl Reads for crate::presentation::Slots {
    fn takes() -> String {
        "up to 16 [slot-number text] rows; 0..15, 256 bytes each, no controls; replaces all slots"
            .to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Self, String> {
        let rows = items(value)?;
        if rows.len() > Self::MAX {
            return Err("at most 16 bar slots".to_owned());
        }
        let values = rows
            .iter()
            .map(|row| {
                let [id, text] = items(row)? else {
                    return Err(Self::takes());
                };
                let id = u8::try_from(int(id)?).map_err(|_| "a bar slot is 0 to 15")?;
                let text = word(text)?;
                Ok((id, text))
            })
            .collect::<Result<_, String>>()?;
        Self::new(values)
    }
}

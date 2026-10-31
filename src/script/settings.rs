//! Checked fuxth values for local preferences and request options.
use super::Fux;
use fux_wire::command::{EnvName, Limit, Number, Program};
use fuxth::Value;

/// A local setting's type, read once as `set` is given it.
pub(crate) trait Reads: Sized {
    fn takes() -> String;
    fn read(value: &Value<Fux>) -> Result<Self, Unread>;
    fn update(value: &Value<Fux>, _previous: Option<&Self>) -> Result<Self, Unread> {
        Self::read(value)
    }
}

/// Why a value can't be read: it is of a kind the reader doesn't take
/// (`type`), or of its kind but a value it refuses, which its caller names.
#[derive(Debug)]
pub(crate) enum Unread {
    Kind(String),
    Value(String),
}

impl From<String> for Unread {
    fn from(why: String) -> Unread {
        Unread::Value(why)
    }
}

impl From<&str> for Unread {
    fn from(why: &str) -> Unread {
        Unread::Value(why.to_owned())
    }
}

impl std::fmt::Display for Unread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unread::Kind(why) | Unread::Value(why) => f.write_str(why),
        }
    }
}

/// `value` is of a kind the reader doesn't take.
fn wrong(takes: &str, value: &Value<Fux>) -> Unread {
    Unread::Kind(format!("takes {takes}, not {}", value.kind()))
}

/// An integer.
fn int(value: &Value<Fux>) -> Result<i64, Unread> {
    let Value::Int(n) = value else {
        return Err(wrong("a number", value));
    };
    Ok(*n)
}

/// The strings a list holds, borrowed, and how many: checked to be all
/// strings, fitting a launch frame, before any is copied.
fn checked(value: &Value<Fux>) -> Result<(usize, impl Iterator<Item = &str>), Unread> {
    let takes = "a list of strings";
    let Value::List(list) = value else {
        return Err(wrong(takes, value));
    };
    let words = || {
        list.items().iter().filter_map(|item| match item {
            Value::Str(s) => Some(&***s),
            Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_) => None,
        })
    };
    if let Some(item) = list.items().iter().find(|i| !matches!(i, Value::Str(_))) {
        return Err(format!("takes {takes}, not one holding {}", item.kind()).into());
    }
    if !Program::fits(words()) {
        return Err("the list of strings exceeds the launch frame limit".into());
    }
    Ok((list.items().len(), words()))
}

/// A list of strings, each as it is, in a vector of exactly their number.
fn strings(value: &Value<Fux>) -> Result<Vec<String>, Unread> {
    let (n, words) = checked(value)?;
    let mut strings = Vec::with_capacity(n);
    strings.extend(words.map(str::to_owned));
    Ok(strings)
}

impl<const MIN: usize, const MAX: usize> Reads for Number<MIN, MAX> {
    fn takes() -> String {
        format!("a number, {MIN} to {MAX}")
    }
    fn read(value: &Value<Fux>) -> Result<Self, Unread> {
        let n = int(value)?;
        (usize::try_from(n).ok().and_then(Number::new))
            .ok_or_else(|| format!("{n} is out of range, {MIN} to {MAX}").into())
    }
}

/// A wait's limit, in seconds.
impl Reads for Limit {
    fn takes() -> String {
        format!("seconds, 0 to {}", u32::MAX)
    }
    fn read(value: &Value<Fux>) -> Result<Limit, Unread> {
        let n = int(value)?;
        let secs = u64::try_from(n).map_err(|_| format!("{n} s: a wait waits 0 s or more"))?;
        Ok(Limit::read(secs)?)
    }
}

/// `true` or `false`.
impl Reads for bool {
    fn takes() -> String {
        "true or false".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<bool, Unread> {
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
    fn read(value: &Value<Fux>) -> Result<Program, Unread> {
        Ok(Program::from_argv(strings(value)?)?)
    }
}

/// Variables' names: a list of strings.
impl Reads for Vec<EnvName> {
    fn takes() -> String {
        "a list of variables' names".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Vec<EnvName>, Unread> {
        let (_, names) = checked(value)?;
        // Each name parsed from the list's own string: copied once.
        let mut seen = std::collections::BTreeSet::new();
        names
            .map(|name| {
                if !seen.insert(name) {
                    return Err(format!("{name}: repeated environment name").into());
                }
                name.parse().map_err(Unread::from)
            })
            .collect()
    }
}

impl Reads for fux_wire::command::ScrollLines {
    fn takes() -> String {
        "a number, 0 to 32767".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Self, Unread> {
        let n = int(value)?;
        usize::try_from(n)
            .ok()
            .and_then(Self::new)
            .ok_or_else(|| format!("{n} is out of range, 0 to 32767").into())
    }
}

fn items(value: &Value<Fux>) -> Result<&[Value<Fux>], Unread> {
    let Value::List(list) = value else {
        return Err(wrong("a list", value));
    };
    Ok(list.items())
}
fn word(value: &Value<Fux>) -> Result<&str, Unread> {
    let Value::Str(text) = value else {
        return Err(wrong("a string", value));
    };
    Ok(text)
}
fn colour(value: &Value<Fux>) -> Result<fux_vt::Color, Unread> {
    use fux_vt::Color;
    match value {
        Value::Int(n) => u8::try_from(*n)
            .map(Color::Idx)
            .map_err(|_| "a colour index is 0 to 255".into()),
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

impl Reads for fux_wire::presentation::Styles {
    fn takes() -> String {
        "role rows: [name foreground background bold/dim/italic/underline...]".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Self, Unread> {
        Self::update(value, None)
    }
    fn update(value: &Value<Fux>, previous: Option<&Self>) -> Result<Self, Unread> {
        let rows = items(value)?;
        if rows.len() > Self::NAMES.len() {
            return Err("at most eight style roles".to_owned().into());
        }
        let mut changes = std::collections::BTreeMap::new();
        for row in rows {
            let [name, fg, bg, flags @ ..] = items(row)? else {
                return Err(Self::takes().into());
            };
            let name = word(name)?;
            if !Self::NAMES.contains(&name) {
                return Err(format!(
                    "no style role {name:?}; the roles are {}",
                    Self::NAMES.join(" ")
                )
                .into());
            }
            let mut style = fux_wire::presentation::Style {
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
                        )
                        .into());
                    }
                };
                if *field {
                    return Err(format!("repeated style flag {flag}").into());
                }
                *field = true;
            }
            if changes.insert(name, style).is_some() {
                return Err(format!("repeated style role {name}").into());
            }
        }
        Ok(Self::read_roles(|name| {
            changes
                .get(name)
                .copied()
                .or_else(|| previous.and_then(|p| p.role(name)))
                .ok_or_else(|| format!("bundled initialization did not set style {name}"))
        })?)
    }
}

impl Reads for fux_wire::presentation::Bar {
    fn takes() -> String {
        "[top/bottom/off [left fields] [right fields] tab-numbers?]; fields: workspace tabs pane [slot 0..15]".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Self, Unread> {
        use fux_wire::presentation::{BarPosition, Field};
        let [position, left, right, numbers] = items(value)? else {
            return Err(Self::takes().into());
        };
        let position = match word(position)? {
            "top" => BarPosition::Top,
            "bottom" => BarPosition::Bottom,
            "off" => BarPosition::Off,
            _ => return Err("bar position is top, bottom, or off".to_owned().into()),
        };
        let fields = |value| -> Result<Vec<Field>, Unread> {
            items(value)?
                .iter()
                .map(|value| match value {
                    Value::Str(_) => match word(value)? {
                        "workspace" => Ok(Field::Workspace),
                        "tabs" => Ok(Field::Tabs),
                        "pane" => Ok(Field::Pane),
                        name => Err(format!(
                            "no bar field {name:?}; fields are workspace tabs pane [slot 0..15]"
                        )
                        .into()),
                    },
                    Value::List(_) => {
                        let [name, index] = items(value)? else {
                            return Err("a slot field is [slot 0..15]".to_owned().into());
                        };
                        if word(name)? != "slot" {
                            return Err("a slot field is [slot 0..15]".to_owned().into());
                        }
                        let id = u8::try_from(int(index)?).map_err(|_| "a bar slot is 0 to 15")?;
                        Ok(Field::Slot(id))
                    }
                    Value::Int(_) | Value::Bool(_) | Value::App(_) => {
                        Err("a bar field is a name or [slot 0..15]".to_owned().into())
                    }
                })
                .collect()
        };
        Ok(Self::new(
            position,
            fields(left)?,
            fields(right)?,
            bool::read(numbers)?,
        )?)
    }
}

impl Reads for fux_wire::presentation::Slots {
    fn takes() -> String {
        "up to 16 [slot-number text] rows; 0..15, no controls; replaces all slots".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Self, Unread> {
        let rows = items(value)?;
        if rows.len() > Self::MAX {
            return Err("at most 16 bar slots".to_owned().into());
        }
        let values = rows
            .iter()
            .map(|row| {
                let [id, text] = items(row)? else {
                    return Err(Self::takes().into());
                };
                let id = u8::try_from(int(id)?).map_err(|_| "a bar slot is 0 to 15")?;
                let text = word(text)?;
                Ok((id, text))
            })
            .collect::<Result<_, Unread>>()?;
        Ok(Self::new(values)?)
    }
}

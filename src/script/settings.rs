//! Checked fuxth values for local preferences and request options.
use super::Fux;
use fux_wire::command::{Number, Program};
use fux_wire::json::Json;
use fuxth::Value;

/// A local setting's type, read once as `set` is given it, and written
/// back as `set` takes it.
pub(crate) trait Reads: Sized {
    fn takes() -> String;
    fn read(value: &Value<Fux>) -> Result<Self, Unread>;
    fn json(&self) -> Json;
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

impl Unread {
    /// The same refusal, its words changed by `f`.
    pub(crate) fn map(self, f: impl FnOnce(String) -> String) -> Unread {
        match self {
            Unread::Kind(why) => Unread::Kind(f(why)),
            Unread::Value(why) => Unread::Value(f(why)),
        }
    }
}

impl std::error::Error for Unread {}

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

/// What an accepted list holds, read: a refusal of it is of a value, never
/// of a kind, which only the list itself may be.
fn held<T>(read: impl FnOnce() -> Result<T, Unread>) -> Result<T, Unread> {
    read().map_err(|unread| Unread::Value(unread.to_string()))
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
/// strings before any is copied.
fn checked(value: &Value<Fux>) -> Result<(usize, impl Iterator<Item = &str>), Unread> {
    let takes = "a list of strings";
    let Value::List(list) = value else {
        return Err(wrong(takes, value));
    };
    let words = || {
        list.items().iter().filter_map(|item| match item {
            Value::Str(s) => Some(&***s),
            Value::Int(_)
            | Value::Bool(_)
            | Value::List(_)
            | Value::App(_)
            | Value::Xt(_)
            | Value::Mark(_)
            | Value::Failure(_)
            | Value::Addr(_) => None,
        })
    };
    if let Some(item) = list.items().iter().find(|i| !matches!(i, Value::Str(_))) {
        return Err(format!("takes {takes}, not one holding {}", item.kind()).into());
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
    fn json(&self) -> Json {
        Json::count(self.get())
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
    fn json(&self) -> Json {
        Json::Bool(*self)
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
    fn json(&self) -> Json {
        Json::Array(self.argv().into_iter().map(Json::String).collect())
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
    fn json(&self) -> Json {
        Json::int(self.get())
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
        Value::Bool(_)
        | Value::List(_)
        | Value::App(_)
        | Value::Xt(_)
        | Value::Mark(_)
        | Value::Failure(_)
        | Value::Addr(_) => Err(wrong("a colour: 0 to 255, default, or #rrggbb", value)),
    }
}
/// A colour as `colour` reads it.
fn colour_json(colour: fux_vt::Color) -> Json {
    match colour {
        fux_vt::Color::Default => Json::str("default"),
        fux_vt::Color::Idx(n) => Json::int(n),
        fux_vt::Color::Rgb(fux_vt::Rgb { r, g, b }) => {
            Json::String(format!("#{r:02x}{g:02x}{b:02x}"))
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
    fn json(&self) -> Json {
        let row = |name: &str| {
            let style = self.role(name)?;
            let flags = [
                ("bold", style.bold),
                ("dim", style.dim),
                ("italic", style.italic),
                ("underline", style.underline),
            ];
            let flags = flags
                .into_iter()
                .filter(|(_, on)| *on)
                .map(|(f, _)| Json::str(f));
            let colours = [colour_json(style.foreground), colour_json(style.background)];
            let row = [Json::str(name)].into_iter().chain(colours).chain(flags);
            Some(Json::Array(row.collect()))
        };
        Json::Array(Self::NAMES.iter().filter_map(|name| row(name)).collect())
    }
    fn update(value: &Value<Fux>, previous: Option<&Self>) -> Result<Self, Unread> {
        let rows = items(value)?;
        held(|| {
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
        })
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
        held(|| {
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
                            let id =
                                u8::try_from(int(index)?).map_err(|_| "a bar slot is 0 to 15")?;
                            Ok(Field::Slot(id))
                        }
                        Value::Int(_)
                        | Value::Bool(_)
                        | Value::App(_)
                        | Value::Xt(_)
                        | Value::Mark(_)
                        | Value::Failure(_)
                        | Value::Addr(_) => {
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
        })
    }
    fn json(&self) -> Json {
        use fux_wire::presentation::{BarPosition, Field};
        let position = match self.position() {
            BarPosition::Top => "top",
            BarPosition::Bottom => "bottom",
            BarPosition::Off => "off",
        };
        let fields = |fields: &[Field]| {
            let field = |field: &Field| match field {
                Field::Workspace => Json::str("workspace"),
                Field::Tabs => Json::str("tabs"),
                Field::Pane => Json::str("pane"),
                Field::Slot(id) => Json::Array(vec![Json::str("slot"), Json::int(*id)]),
            };
            Json::Array(fields.iter().map(field).collect())
        };
        Json::Array(vec![
            Json::str(position),
            fields(self.left()),
            fields(self.right()),
            Json::Bool(self.tab_numbers()),
        ])
    }
}

impl Reads for fux_wire::presentation::Slots {
    fn takes() -> String {
        "up to 16 [slot-number text] rows; 0..15, no controls; replaces all slots".to_owned()
    }
    fn read(value: &Value<Fux>) -> Result<Self, Unread> {
        let rows = items(value)?;
        held(|| {
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
        })
    }
    fn json(&self) -> Json {
        let ids = (0..u8::try_from(Self::MAX).unwrap_or(u8::MAX)).filter_map(|id| {
            let text = self.text(id)?;
            Some(Json::Array(vec![Json::int(id), Json::str(text)]))
        });
        Json::Array(ids.collect())
    }
}

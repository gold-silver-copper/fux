//! Bounded, inert data for the native painter. Policy values come from fuxth.
use crate::wire::{Error, Reader, Wire, wire};
use fux_vt::{Attributes, Color};

wire! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Style {
        pub foreground: Color,
        pub background: Color,
        pub bold: bool,
        pub dim: bool,
        pub italic: bool,
        pub underline: bool,
    }
}
impl Style {
    pub fn attributes(self) -> Attributes {
        Attributes::new(self.foreground, self.background)
            .with_bold(self.bold)
            .with_dim(self.dim)
            .with_italic(self.italic)
            .with_underline(self.underline)
    }
}

macro_rules! roles {
    ($($field:ident $name:literal),+ $(,)?) => {
        wire! {
            #[derive(Clone, Copy, Debug, PartialEq, Eq)]
            pub struct Styles { $(pub $field: Style,)+ }
        }
        impl Styles {
            pub const NAMES: &[&str] = &[$($name),+];
            pub(crate) fn read_roles(
                mut read: impl FnMut(&str) -> Result<Style, String>,
            ) -> Result<Self, String> {
                Ok(Self { $($field: read($name)?,)+ })
            }
            pub(crate) fn role(self, name: &str) -> Option<Style> {
                match name { $($name => Some(self.$field),)+ _ => None }
            }
        }
    };
}
roles! {
    bar "bar", tab "tab", keys "keys", notice "notice", error "error",
    panel "panel", border "border", border_focus "border-focus",
}

wire! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum BarPosition { Top, Bottom, Off, }
}
wire! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Field { Workspace, Tabs, Pane, Slot(index: u8), }
}

/// At most sixteen fields across both sides, with no recursive formatting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bar {
    position: BarPosition,
    left: Vec<Field>,
    right: Vec<Field>,
    tab_numbers: bool,
}
impl Bar {
    pub const MAX_FIELDS: usize = 16;
    pub fn new(
        position: BarPosition,
        left: Vec<Field>,
        right: Vec<Field>,
        tab_numbers: bool,
    ) -> Result<Self, String> {
        let bar = Self {
            position,
            left,
            right,
            tab_numbers,
        };
        bar.validate()?;
        Ok(bar)
    }
    pub fn position(&self) -> BarPosition {
        self.position
    }
    pub fn left(&self) -> &[Field] {
        &self.left
    }
    pub fn right(&self) -> &[Field] {
        &self.right
    }
    pub fn tab_numbers(&self) -> bool {
        self.tab_numbers
    }
    fn validate(&self) -> Result<(), String> {
        if self.left.len().saturating_add(self.right.len()) > Self::MAX_FIELDS {
            return Err("a bar has at most 16 fields".to_owned());
        }
        let mut seen = Vec::new();
        for field in self.left.iter().chain(&self.right) {
            if seen.contains(field) {
                return Err("a bar field occurs more than once".to_owned());
            }
            if matches!(field, Field::Slot(n) if usize::from(*n) >= Slots::MAX) {
                return Err("a bar slot is 0 to 15".to_owned());
            }
            seen.push(*field);
        }
        Ok(())
    }
}
impl Wire for Bar {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.position.put(out);
        self.left.put(out);
        self.right.put(out);
        self.tab_numbers.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let position = BarPosition::get(r)?;
        let left = r.bounded_list(Self::MAX_FIELDS, Field::get)?;
        let right = r.bounded_list(Self::MAX_FIELDS.saturating_sub(left.len()), Field::get)?;
        Self::new(position, left, right, bool::get(r)?).map_err(|_| Error::Bad("bar description"))
    }
}

/// Per-client text. A user program supplies changes; the painter never runs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slots(Vec<(u8, String)>);
impl Slots {
    pub const MAX: usize = 16;
    pub const TEXT_MAX: usize = 256;
    pub fn new<'a, T: Into<std::borrow::Cow<'a, str>>>(
        values: Vec<(u8, T)>,
    ) -> Result<Self, String> {
        if values.len() > Self::MAX {
            return Err("at most 16 bar slots".to_owned());
        }
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for (id, text) in values {
            let text = text.into();
            if usize::from(id) >= Self::MAX {
                return Err("a bar slot is 0 to 15".to_owned());
            }
            if seen.contains(&id) {
                return Err(format!("bar slot {id} occurs more than once"));
            }
            if text.len() > Self::TEXT_MAX || text.chars().any(char::is_control) {
                return Err(
                    "bar slot text is at most 256 bytes with no control characters".to_owned(),
                );
            }
            seen.push(id);
            out.push((id, text.into_owned()));
        }
        Ok(Self(out))
    }
    pub fn text(&self, id: u8) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| *n == id)
            .map(|(_, text)| text.as_str())
    }
}
impl Wire for Slots {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.0.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let values = r.bounded_list(Self::MAX, |r| {
            let id = u8::get(r)?;
            let text = r.bounded_text(Self::TEXT_MAX)?;
            Ok((id, text))
        })?;
        Self::new(values).map_err(|_| Error::Bad("bar slots"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_transport_refuses_excess_fields_and_slot_text() -> Result<(), String> {
        let p = crate::script::default_state()
            .map_err(|e| e.to_string())?
            .preferences
            .presentation();
        let mut bytes = Vec::new();
        p.put(&mut bytes);
        assert_eq!(
            Reader::new(&bytes)
                .whole::<crate::view::Presentation>()
                .map_err(|e| e.to_string())?,
            p
        );
        bytes.clear();
        BarPosition::Bottom.put(&mut bytes);
        17u32.put(&mut bytes);
        assert!(matches!(
            Reader::new(&bytes).whole::<Bar>(),
            Err(Error::Bad("list length"))
        ));
        bytes.clear();
        vec![(0u8, std::iter::repeat_n('x', 257).collect::<String>())].put(&mut bytes);
        assert!(matches!(
            Reader::new(&bytes).whole::<Slots>(),
            Err(Error::Bad("text length"))
        ));
        bytes.clear();
        vec![(0u8, "escape\x1b".to_owned())].put(&mut bytes);
        assert!(Reader::new(&bytes).whole::<Slots>().is_err());
        Ok(())
    }
}

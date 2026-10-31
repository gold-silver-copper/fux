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
            pub fn read_roles(
                mut read: impl FnMut(&str) -> Result<Style, String>,
            ) -> Result<Self, String> {
                Ok(Self { $($field: read($name)?,)+ })
            }
            pub fn role(self, name: &str) -> Option<Style> {
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
        if left.len().saturating_add(right.len()) > Self::MAX_FIELDS {
            return Err("a bar has at most 16 fields".to_owned());
        }
        let mut seen = Vec::new();
        for field in left.iter().chain(&right) {
            if seen.contains(field) {
                return Err("a bar field occurs more than once".to_owned());
            }
            if matches!(field, Field::Slot(n) if usize::from(*n) >= Slots::MAX) {
                return Err("a bar slot is 0 to 15".to_owned());
            }
            seen.push(*field);
        }
        Ok(Self {
            position,
            left,
            right,
            tab_numbers,
        })
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
        let mut out = Vec::new();
        for (id, text) in values {
            let text = text.into();
            if usize::from(id) >= Self::MAX {
                return Err("a bar slot is 0 to 15".to_owned());
            }
            if out.iter().any(|(held, _)| *held == id) {
                return Err(format!("bar slot {id} occurs more than once"));
            }
            if text.len() > Self::TEXT_MAX || text.chars().any(char::is_control) {
                return Err(
                    "bar slot text is at most 256 bytes with no control characters".to_owned(),
                );
            }
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

wire! {
    /// Optional output requested by this attached client.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Presentation {
        pub bell: bool,
        pub titles: bool,
        pub terminal_clipboard: bool,
        pub cell_placement: bool,
        pub styles: Styles,
        pub bar: Bar,
        pub slots: Slots,
        pub show_column: bool,
    }
}

/// A presentation with every part set, for tests of what carries it.
#[cfg(test)]
pub(crate) fn sample() -> Result<Presentation, String> {
    let style = |foreground, background, bold| Style {
        foreground,
        background,
        bold,
        dim: false,
        italic: false,
        underline: false,
    };
    let styles = Styles {
        bar: style(Color::Idx(250), Color::Idx(236), false),
        tab: style(Color::Idx(0), Color::Idx(2), true),
        keys: style(Color::Idx(0), Color::Idx(11), true),
        notice: style(Color::Idx(11), Color::Idx(236), false),
        error: style(Color::Idx(9), Color::Idx(236), false),
        panel: style(Color::Idx(255), Color::Idx(238), false),
        border: style(Color::Idx(240), Color::Default, false),
        border_focus: style(Color::Idx(2), Color::Default, false),
    };
    let left = vec![Field::Workspace, Field::Tabs];
    Ok(Presentation {
        bell: true,
        titles: false,
        terminal_clipboard: true,
        cell_placement: false,
        styles,
        bar: Bar::new(BarPosition::Bottom, left, vec![Field::Pane], false)?,
        slots: Slots::new::<String>(Vec::new())?,
        show_column: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_transport_refuses_excess_fields_and_slot_text() -> Result<(), String> {
        let p = sample()?;
        let mut bytes = Vec::new();
        p.put(&mut bytes);
        assert_eq!(
            Reader::new(&bytes)
                .whole::<crate::presentation::Presentation>()
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

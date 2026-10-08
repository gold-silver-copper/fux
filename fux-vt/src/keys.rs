//! Keys, their modifiers, and their names in `bind`, `send-keys` and
//! `list-keys`: tmux's names (`C-x`, `M-x`, `S-Left`, `Enter`, `BTab`, …)
//! plus any single character.
pub mod colour;
pub mod decode;
pub mod encode;
pub mod mouse;

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// An arrow key's direction.
pub enum Direction {
    /// The left arrow.
    Left,
    /// The right arrow.
    Right,
    /// The up arrow.
    Up,
    /// The down arrow.
    Down,
}

impl Direction {
    /// Every direction, left, right, up, down.
    pub const ALL: [Direction; 4] = [Self::Left, Self::Right, Self::Up, Self::Down];
    /// The direction's name, lower case (`left`, `right`, `up`, `down`), as
    /// messages write it; key names are `Left` and so on (`NAMED`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
    /// The `-L`/`-R`/`-U`/`-D` flag that names it.
    pub fn flag(self) -> &'static str {
        match self {
            Self::Left => "-L",
            Self::Right => "-R",
            Self::Up => "-U",
            Self::Down => "-D",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A key, without its modifiers.
pub enum Key {
    /// A key that types a character.
    Char(char),
    /// Enter (Return).
    Enter,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
    /// Backspace.
    Backspace,
    /// Delete (forward delete).
    Delete,
    /// Insert.
    Insert,
    /// An arrow key.
    Arrow(Direction),
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// `1..=12`.
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
/// The modifiers fux and legacy terminals tell apart.
pub struct Modifiers {
    /// Control.
    pub ctrl: bool,
    /// Alt (Meta in the legacy sense: an Escape prefix).
    pub alt: bool,
    /// Shift.
    pub shift: bool,
}

impl Modifiers {
    /// No modifier.
    pub const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
    };
    /// Control alone.
    pub const CTRL: Modifiers = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    /// Alt alone.
    pub const ALT: Modifiers = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    /// Shift alone.
    pub const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    /// Whether no modifier is held.
    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }
}

/// A key with its modifiers, normalized so that equal key presses compare
/// equal: a character's shift is dropped, as the character says it already
/// (Shift and `a` arrive as `A`; `S-a` is `a`), and a control letter is
/// lower case (`C-b`), as terminals cannot tell the two apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyPress {
    /// The key.
    pub key: Key,
    /// The modifiers held with it.
    pub mods: Modifiers,
}

impl KeyPress {
    /// `key` with `mods` held.
    pub fn new(key: Key, mods: Modifiers) -> Self {
        let mut press = Self { key, mods };
        if let Key::Char(c) = key {
            press.mods.shift = false;
            if press.mods.ctrl && c.is_ascii_alphabetic() {
                press.key = Key::Char(c.to_ascii_lowercase());
            }
        }
        press
    }
    /// `key` with no modifier.
    pub fn plain(key: Key) -> Self {
        Self::new(key, Modifiers::NONE)
    }
    /// The key that types `c`, with no modifier.
    pub fn char(c: char) -> Self {
        Self::plain(Key::Char(c))
    }
    /// The key as fux matches letter keys typed after the prefix, and in
    /// choosers, menus, confirmations and copy mode: a letter in lower case,
    /// whatever Shift and Caps Lock did. The prefix itself and root bindings
    /// (`bind -n`) match the press as it is. A letter with Ctrl or Alt is left as it is,
    /// and matches no letter key.
    pub fn folded(self) -> Self {
        if let Key::Char(c) = self.key
            && c.is_ascii_alphabetic()
            && !self.mods.ctrl
            && !self.mods.alt
        {
            return Self::char(c.to_ascii_lowercase());
        }
        self
    }
    /// The folded key, unless Ctrl or Alt is held: what overlays and copy
    /// mode act on.
    pub fn plain_key(self) -> Option<Key> {
        (!self.mods.ctrl && !self.mods.alt).then_some(self.folded().key)
    }
}

/// A key as the client's terminal sent it: the press fux matches (bindings,
/// overlays, copy mode), and, from a terminal speaking the kitty keyboard
/// protocol, what it said beyond the press, which a pane whose program
/// speaks the protocol is given back (`encode::key_bytes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Keystroke {
    /// The press a legacy terminal would have sent for the key.
    pub press: KeyPress,
    /// What a terminal speaking the kitty protocol said beyond the press, if it did.
    pub kitty: Option<Kitty>,
}

/// What a terminal speaking the kitty keyboard protocol said of a key
/// beyond its `KeyPress` (`references/modern/kitty_keyboard_protocol.html`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Kitty {
    /// The number of a key sent as `CSI number u`: its unshifted code
    /// point, or a functional key's (keypad keys from 57399); none for a
    /// key sent as `CSI 1 ; m A` or `CSI n ~`.
    pub code: Option<u32>,
    /// The shifted key, as reported (alternate keys).
    pub shifted: Option<u32>,
    /// The base-layout key, as reported (alternate keys).
    pub base: Option<u32>,
    /// The modifier bits: Shift 1, Alt 2, Ctrl 4, Super 8, Hyper 16, Meta
    /// 32, Caps Lock 64, Num Lock 128. A `KeyPress` keeps the first three.
    pub mods: u8,
}

impl From<KeyPress> for Keystroke {
    fn from(press: KeyPress) -> Keystroke {
        Keystroke { press, kitty: None }
    }
}

const NAMED: &[(&str, Key)] = &[
    ("Enter", Key::Enter),
    ("Tab", Key::Tab),
    ("Escape", Key::Escape),
    ("Space", Key::Char(' ')),
    ("BSpace", Key::Backspace),
    ("Up", Key::Arrow(Direction::Up)),
    ("Down", Key::Arrow(Direction::Down)),
    ("Left", Key::Arrow(Direction::Left)),
    ("Right", Key::Arrow(Direction::Right)),
    ("Home", Key::Home),
    ("End", Key::End),
    ("PageUp", Key::PageUp),
    ("PageDown", Key::PageDown),
    ("Insert", Key::Insert),
    ("Delete", Key::Delete),
];

/// Accepted as well as the names above; never printed.
const ALIASES: &[(&str, Key)] = &[
    ("PgUp", Key::PageUp),
    ("PPage", Key::PageUp),
    ("PgDn", Key::PageDown),
    ("NPage", Key::PageDown),
    ("IC", Key::Insert),
    ("DC", Key::Delete),
    ("Esc", Key::Escape),
];

fn key_name(key: Key) -> String {
    if let Some((name, _)) = NAMED.iter().find(|(_, k)| *k == key) {
        return (*name).to_owned();
    }
    // Every key but a character and a function key is named above.
    if let Key::F(n) = key {
        return format!("F{n}");
    }
    if let Key::Char(c) = key {
        return c.to_string();
    }
    String::new()
}

impl fmt::Display for KeyPress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.key == Key::Tab && self.mods.shift && !self.mods.ctrl && !self.mods.alt {
            return f.write_str("BTab");
        }
        if self.mods.ctrl {
            f.write_str("C-")?;
        }
        if self.mods.alt {
            f.write_str("M-")?;
        }
        if self.mods.shift {
            f.write_str("S-")?;
        }
        f.write_str(&key_name(self.key))
    }
}

/// A name that is no key's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// The name that was not recognised.
    pub name: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown key {:?}; `fux list-keys` lists them", self.name)
    }
}

impl std::error::Error for Error {}

impl std::str::FromStr for KeyPress {
    type Err = Error;
    fn from_str(name: &str) -> Result<Self, Error> {
        let mut mods = Modifiers::NONE;
        let mut rest = name;
        // `C-` and friends are prefixes only while something follows them,
        // so `-` and `C--` name the minus key.
        while let Some((prefix, tail)) = rest.split_at_checked(2) {
            if tail.is_empty() {
                break;
            }
            match prefix {
                "C-" | "c-" => mods.ctrl = true,
                "M-" | "m-" => mods.alt = true,
                "S-" | "s-" => mods.shift = true,
                _ => break,
            }
            rest = tail;
        }
        let key = if rest == "BTab" {
            mods.shift = true;
            Key::Tab
        } else if let Some((_, key)) = NAMED
            .iter()
            .chain(ALIASES)
            .find(|(n, _)| n.eq_ignore_ascii_case(rest))
        {
            *key
        } else if let Some(n) = rest
            .strip_prefix(['F', 'f'])
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (1..=12).contains(n))
        {
            Key::F(n)
        } else {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_control() => Key::Char(c),
                _ => {
                    return Err(Error {
                        name: name.to_owned(),
                    });
                }
            }
        };
        Ok(KeyPress::new(key, mods))
    }
}

/// Every key name, for `fux list-keys`.
pub fn all_names() -> Vec<String> {
    let mut names: Vec<String> = NAMED.iter().map(|(n, _)| (*n).to_owned()).collect();
    names.push("BTab".into());
    names.extend((1..=12).map(|n| format!("F{n}")));
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(name: &str) -> Option<KeyPress> {
        name.parse().ok()
    }

    #[test]
    fn tmux_names_parse_and_print_canonically() {
        for (name, canonical) in [
            ("C-b", "C-b"),
            ("C-B", "C-b"),
            ("c-b", "C-b"),
            ("M-x", "M-x"),
            ("C-M-Left", "C-M-Left"),
            ("S-Up", "S-Up"),
            ("BTab", "BTab"),
            ("S-Tab", "BTab"),
            ("Enter", "Enter"),
            ("enter", "Enter"),
            ("Space", "Space"),
            (" ", "Space"),
            ("BSpace", "BSpace"),
            ("PgUp", "PageUp"),
            ("NPage", "PageDown"),
            ("IC", "Insert"),
            ("F1", "F1"),
            ("F12", "F12"),
            ("-", "-"),
            ("C--", "C--"),
            ("T", "T"),
            ("S-t", "t"),
            ("界", "界"),
            (":", ":"),
        ] {
            assert_eq!(
                parse(name).map(|k| k.to_string()),
                Some(canonical.into()),
                "{name}"
            );
        }
        for bad in ["", "F13", "F0", "Nope", "C-", "ab"] {
            assert!(parse(bad).is_none(), "{bad}");
        }
        assert_eq!(
            "Nope".parse::<KeyPress>().map_err(|e| e.to_string()),
            Err("unknown key \"Nope\"; `fux list-keys` lists them".to_owned())
        );
        for name in all_names() {
            assert_eq!(parse(&name).map(|k| k.to_string()), Some(name.clone()));
        }
    }
}

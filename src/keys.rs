//! Keys, their modifiers, and their names in `bind`, `send-keys` and
//! `list-keys`: tmux's names (`C-x`, `M-x`, `S-Left`, `Enter`, `BTab`, …)
//! plus any single character.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub const ALL: [Direction; 4] = [Self::Left, Self::Right, Self::Up, Self::Down];
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
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Arrow(Direction),
    Home,
    End,
    PageUp,
    PageDown,
    /// `1..=12`.
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
    };
    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }
}

/// A key with its modifiers, normalized so that equal key presses compare
/// equal: a character carries its own shift (`A`, not `S-a`), and a control
/// letter is lower case (`C-b`), as terminals cannot tell the two apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyPress {
    pub key: Key,
    pub mods: Modifiers,
}

impl KeyPress {
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
    pub fn plain(key: Key) -> Self {
        Self::new(key, Modifiers::NONE)
    }
    pub fn char(c: char) -> Self {
        Self::plain(Key::Char(c))
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
    match key {
        Key::Char(c) => c.to_string(),
        Key::F(n) => format!("F{n}"),
        Key::Enter
        | Key::Tab
        | Key::Escape
        | Key::Backspace
        | Key::Delete
        | Key::Insert
        | Key::Arrow(_)
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown => String::new(),
    }
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

impl std::str::FromStr for KeyPress {
    type Err = String;
    fn from_str(name: &str) -> Result<Self, String> {
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
                _ => return Err(format!("unknown key {name:?}; `fux list-keys` lists them")),
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
        for name in all_names() {
            assert_eq!(parse(&name).map(|k| k.to_string()), Some(name.clone()));
        }
    }
}

//! What both terminals are read into, so they can be compared field by
//! field: the visible screen cell by cell, the cursor, the modes both
//! track, the title, cursor and status reports, and the text of the most
//! recent history rows.
use crate::engine::Can;
use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

/// What a cell looks like. Underline style and blink speed are reduced to
/// on or off, as fux-vt keeps no underline style and Ghostty no blink speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strikeout: bool,
}

impl Default for Style {
    fn default() -> Style {
        Style {
            fg: Color::Default,
            bg: Color::Default,
            underline_color: Color::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            blink: false,
            inverse: false,
            hidden: false,
            strikeout: false,
        }
    }
}

impl Style {
    fn describe(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.bold, "bold"),
            (self.dim, "dim"),
            (self.italic, "italic"),
            (self.underline, "underline"),
            (self.blink, "blink"),
            (self.inverse, "inverse"),
            (self.hidden, "hidden"),
            (self.strikeout, "strikeout"),
        ] {
            if on {
                let _ = write!(out, " {name}");
            }
        }
        for (color, name) in [
            (self.fg, "fg"),
            (self.bg, "bg"),
            (self.underline_color, "ul"),
        ] {
            if color != Color::Default {
                let _ = write!(out, " {name}={color:?}");
            }
        }
        if out.is_empty() {
            " plain".to_owned()
        } else {
            out
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Narrow,
    /// The first half of a wide glyph.
    Wide,
    /// The second half of a wide glyph: no text and no style of its own.
    Tail,
}

/// A cell. A blank holds no text: a printed space reads as a blank, so the
/// two terminals' ways of storing spaces are not compared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub width: Width,
    pub style: Style,
}

impl Cell {
    pub fn new(text: &str, width: Width, style: Style) -> Cell {
        match width {
            Width::Tail => Cell {
                text: String::new(),
                width,
                style: Style::default(),
            },
            Width::Narrow | Width::Wide => Cell {
                text: if text == " " {
                    String::new()
                } else {
                    text.to_owned()
                },
                width,
                style,
            },
        }
    }

    fn describe(&self) -> String {
        let width = match self.width {
            Width::Narrow => "",
            Width::Wide => " wide",
            Width::Tail => " tail",
        };
        format!("{:?}{width}{}", self.text, self.style.describe())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub cells: Vec<Cell>,
    pub wrapped: bool,
}

impl Line {
    /// The line as text, a blank as a space, with no trailing blanks.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for cell in &self.cells {
            match cell.width {
                Width::Tail => {}
                Width::Narrow | Width::Wide if cell.text.is_empty() => out.push(' '),
                Width::Narrow | Width::Wide => out.push_str(&cell.text),
            }
        }
        out.trim_end_matches(' ').to_owned()
    }
}

impl Snapshot {
    /// The snapshot with what an engine cannot tell left at its default,
    /// so comparing two masked snapshots compares only what it can.
    pub fn masked(&self, can: &Can) -> Snapshot {
        let mut s = self.clone();
        for line in &mut s.screen {
            line.wrapped &= can.wrapped;
            for cell in &mut line.cells {
                let st = &mut cell.style;
                for (keep, color) in [
                    (can.fg, &mut st.fg),
                    (can.bg, &mut st.bg),
                    (can.underline_color, &mut st.underline_color),
                ] {
                    if !keep {
                        *color = Color::Default;
                    }
                }
                for (keep, flag) in [
                    (can.bold, &mut st.bold),
                    (can.dim, &mut st.dim),
                    (can.italic, &mut st.italic),
                    (can.underline, &mut st.underline),
                    (can.blink, &mut st.blink),
                    (can.inverse, &mut st.inverse),
                    (can.hidden, &mut st.hidden),
                    (can.strikeout, &mut st.strikeout),
                ] {
                    *flag &= keep;
                }
                if !can.widths && cell.width == Width::Wide {
                    cell.width = Width::Narrow;
                }
            }
        }
        if !can.cursor {
            s.cursor = (0, 0);
        }
        for (keep, flag) in [
            (can.pending_wrap, &mut s.pending_wrap),
            (can.cursor_visible, &mut s.cursor_visible),
            (can.autowrap, &mut s.autowrap),
            (can.origin, &mut s.origin),
            (can.alternate, &mut s.alternate),
            (can.application_cursor, &mut s.application_cursor),
            (can.application_keypad, &mut s.application_keypad),
            (can.bracketed_paste, &mut s.bracketed_paste),
            (can.focus_reporting, &mut s.focus_reporting),
        ] {
            *flag &= keep;
        }
        if !can.kitty_keyboard_flags {
            s.kitty_keyboard_flags = 0;
        }
        if !can.title {
            s.title.clear();
        }
        if !can.reports {
            s.reports.clear();
        }
        if !can.history {
            s.history.clear();
        }
        for (_, wrapped) in &mut s.history {
            *wrapped &= can.wrapped;
        }
        s
    }
}

/// Everything compared, read from one terminal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub rows: u16,
    pub cols: u16,
    /// Row and column, zero-based; a cursor waiting to wrap is in the last
    /// column with `pending_wrap` set.
    pub cursor: (u16, u16),
    pub pending_wrap: bool,
    pub cursor_visible: bool,
    pub autowrap: bool,
    pub origin: bool,
    pub alternate: bool,
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    pub kitty_keyboard_flags: u8,
    pub title: String,
    /// Cursor position and status reports, in order: the replies both
    /// terminals are meant to give alike.
    pub reports: Vec<String>,
    pub screen: Vec<Line>,
    /// fux-vt's whole history, oldest first, and as many of Ghostty's most
    /// recent history rows: Ghostty bounds history in bytes, not rows.
    pub history: Vec<(String, bool)>,
}

/// Keeps the replies both terminals give alike: DSR 5n's `CSI 0 n` and
/// cursor position reports, `CSI row ; col R`. Device attributes and mode
/// reports name the terminal, so they differ by design.
pub fn reports(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while let Some(start) = rest.iter().position(|&b| b == 0x1b) {
        let Some(after) = rest.get(start.saturating_add(1)..) else {
            break;
        };
        let Some(body) = after.strip_prefix(b"[") else {
            rest = after;
            continue;
        };
        let end = body
            .iter()
            .position(|b| !(b.is_ascii_digit() || *b == b';'))
            .unwrap_or(body.len());
        let (params, tail) = body.split_at_checked(end).unwrap_or((body, &[]));
        let kept = match tail.first() {
            Some(b'R') => params.contains(&b';'),
            Some(b'n') => params == b"0",
            _ => false,
        };
        if kept {
            let mut text = String::from("CSI ");
            text.push_str(&String::from_utf8_lossy(params));
            text.push(char::from(tail.first().copied().unwrap_or(b'?')));
            out.push(text);
        }
        rest = body;
    }
    out
}

/// A mode both track, by name, and how to read it.
type Flag = (&'static str, fn(&Snapshot) -> bool);

/// The differences between fux-vt's snapshot and another engine's, at
/// most `limit` of them, each a line; empty when they agree.
pub fn differences(fux: &Snapshot, other: &Snapshot, engine: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = |name: &str, a: String, b: String| {
        if a != b {
            out.push(format!("{name}: fux-vt {a}, {engine} {b}"));
        }
    };
    field(
        "size",
        format!("{}x{}", fux.rows, fux.cols),
        format!("{}x{}", other.rows, other.cols),
    );
    field(
        "cursor",
        format!("{:?}", fux.cursor),
        format!("{:?}", other.cursor),
    );
    let flags: [Flag; 9] = [
        ("pending wrap", |s| s.pending_wrap),
        ("cursor visible", |s| s.cursor_visible),
        ("autowrap", |s| s.autowrap),
        ("origin mode", |s| s.origin),
        ("alternate screen", |s| s.alternate),
        ("application cursor", |s| s.application_cursor),
        ("application keypad", |s| s.application_keypad),
        ("bracketed paste", |s| s.bracketed_paste),
        ("focus reporting", |s| s.focus_reporting),
    ];
    for (name, get) in flags {
        field(name, get(fux).to_string(), get(other).to_string());
    }
    field(
        "kitty keyboard flags",
        fux.kitty_keyboard_flags.to_string(),
        other.kitty_keyboard_flags.to_string(),
    );
    // fux-vt reports titles as they are set and keeps none, so a reset
    // cannot clear one; Ghostty's RIS clears its title. An empty title on
    // the other engine's side is not compared.
    if !other.title.is_empty() {
        field(
            "title",
            format!("{:?}", fux.title),
            format!("{:?}", other.title),
        );
    }
    field(
        "reports",
        format!("{:?}", fux.reports),
        format!("{:?}", other.reports),
    );
    for (y, (a, b)) in fux.screen.iter().zip(&other.screen).enumerate() {
        if a.wrapped != b.wrapped {
            out.push(format!(
                "row {y} soft-wrapped: fux-vt {}, {engine} {}",
                a.wrapped, b.wrapped
            ));
        }
        for (x, (ca, cb)) in a.cells.iter().zip(&b.cells).enumerate() {
            if ca != cb {
                out.push(format!(
                    "cell ({y},{x}): fux-vt {}, {engine} {}",
                    ca.describe(),
                    cb.describe()
                ));
            }
        }
    }
    let ha: Vec<_> = fux.history.iter().rev().collect();
    let hb: Vec<_> = other.history.iter().rev().collect();
    if ha.len() > hb.len() {
        out.push(format!(
            "history: fux-vt keeps {} rows, {engine} {}",
            ha.len(),
            hb.len()
        ));
    }
    for (back, (a, b)) in ha.iter().zip(&hb).enumerate() {
        if a != b {
            out.push(format!(
                "history row {} from the newest: fux-vt {:?}{}, {engine} {:?}{}",
                back,
                a.0,
                if a.1 { " (wrapped)" } else { "" },
                b.0,
                if b.1 { " (wrapped)" } else { "" },
            ));
        }
    }
    let total = out.len();
    if total > limit {
        out = out.into_iter().take(limit).collect();
        out.push(format!("... and {} more", total.saturating_sub(limit)));
    }
    out
}

/// Both screens side by side, as text, with the cursor marked and each
/// soft-wrapped row flagged.
pub fn side_by_side(fux: &Snapshot, other: &Snapshot, engine: &str) -> String {
    let width = usize::from(fux.cols.max(other.cols)).max(6);
    let mut out = format!("  {:<width$}   {engine}\n", "fux-vt");
    let lines = fux.screen.len().max(other.screen.len());
    for y in 0..lines {
        let side = |s: &Snapshot| -> String {
            let Some(line) = s.screen.get(y) else {
                return String::new();
            };
            let mut text = String::new();
            for (x, cell) in line.cells.iter().enumerate() {
                let here = u16::try_from(y).ok() == Some(s.cursor.0)
                    && u16::try_from(x).ok() == Some(s.cursor.1);
                match cell.width {
                    Width::Tail => {}
                    Width::Narrow | Width::Wide if cell.text.is_empty() => {
                        text.push(if here { '_' } else { '.' });
                    }
                    Width::Narrow | Width::Wide => text.push_str(&cell.text),
                }
            }
            if line.wrapped {
                text.push('↩');
            }
            text
        };
        let (a, b) = (side(fux), side(other));
        let pad = width.saturating_sub(a.chars().count());
        let mark = if a == b { ' ' } else { '≠' };
        let _ = writeln!(out, "{mark} {a}{:pad$}   {b}", "");
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_keep_cursor_and_status_replies_only() {
        let bytes = b"\x1b[3;4R\x1b[0n\x1b[?1;2c\x1b[?62;22c\x1b[5;1Rjunk\x1b[3n";
        assert_eq!(super::reports(bytes), ["CSI 3;4R", "CSI 0n", "CSI 5;1R"]);
    }
}

//! What both terminals are read into, so they can be compared field by
//! field: the visible screen cell by cell, the cursor, the modes both
//! track, the title, cursor and status reports, and the text of the most
//! recent history rows.
use crate::engine::Can;
use std::collections::HashMap;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Narrow,
    /// The first half of a wide glyph.
    Wide,
    /// The second half of a wide glyph: no text and no style of its own.
    Tail,
}

/// A cell's hyperlink (OSC 8): its URI, and which link it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    /// The engine's own name for the link, the same for every cell of one
    /// link and for no other; empty if it cannot tell. Engines name links
    /// their own ways, so names are not compared: which cells share one is
    /// (see [`differences`]).
    pub group: String,
}

/// A cell. A blank holds no text: a printed space reads as a blank, so the
/// two terminals' ways of storing spaces are not compared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub width: Width,
    pub style: Style,
    pub link: Option<Link>,
}

impl Cell {
    pub fn new(text: &str, width: Width, style: Style) -> Cell {
        match width {
            Width::Tail => Cell {
                text: String::new(),
                width,
                style: Style::default(),
                link: None,
            },
            Width::Narrow | Width::Wide => Cell {
                text: if text == " " {
                    String::new()
                } else {
                    text.to_owned()
                },
                width,
                style,
                link: None,
            },
        }
    }

    /// The cell with a hyperlink: `uri`, and the engine's own name for the
    /// link (see [`Link::group`]). The second half of a wide glyph has
    /// none of its own, as it has no style.
    pub fn linked(mut self, link: Option<(String, String)>) -> Cell {
        if self.width != Width::Tail {
            self.link = link.map(|(uri, group)| Link { uri, group });
        }
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub cells: Vec<Cell>,
    pub wrapped: bool,
    /// Whether a prompt starts on the row: a shell marked it with
    /// `OSC 133 ; A`.
    pub prompt: bool,
    /// The first cell whose style the engine could not read, if any: from
    /// there on, only the text and width are compared (an xterm print stops
    /// at a row's last drawn cell).
    pub unread_from: Option<usize>,
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
            line.prompt &= can.prompt;
            for cell in &mut line.cells {
                if !can.link_uri {
                    cell.link = None;
                }
                if let Some(link) = &mut cell.link
                    && !can.link_group
                {
                    link.group.clear();
                }
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
            (can.synchronized_output, &mut s.synchronized_output),
            (can.in_band_resize, &mut s.in_band_resize),
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
    /// Synchronized output, mode 2026.
    pub synchronized_output: bool,
    /// In-band resize reports, mode 2048.
    pub in_band_resize: bool,
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

/// What a difference is about, so the vote counts only the engines that
/// can tell it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Size,
    Cursor,
    PendingWrap,
    CursorVisible,
    Autowrap,
    Origin,
    Alternate,
    ApplicationCursor,
    ApplicationKeypad,
    BracketedPaste,
    FocusReporting,
    SynchronizedOutput,
    InBandResize,
    Kitty,
    Prompt,
    LinkUri,
    LinkGroup,
    Title,
    Reports,
    Wrapped,
    Text,
    Width,
    Fg,
    Bg,
    UnderlineColor,
    Bold,
    Dim,
    Italic,
    Underline,
    Blink,
    Inverse,
    Hidden,
    Strikeout,
    History,
    HistoryWrapped,
}

impl Field {
    /// Whether an engine that can do `can` tells this field.
    pub fn told_by(self, can: &Can) -> bool {
        match self {
            Field::Size | Field::Text => true,
            Field::Cursor => can.cursor,
            Field::PendingWrap => can.pending_wrap,
            Field::CursorVisible => can.cursor_visible,
            Field::Autowrap => can.autowrap,
            Field::Origin => can.origin,
            Field::Alternate => can.alternate,
            Field::ApplicationCursor => can.application_cursor,
            Field::ApplicationKeypad => can.application_keypad,
            Field::BracketedPaste => can.bracketed_paste,
            Field::FocusReporting => can.focus_reporting,
            Field::SynchronizedOutput => can.synchronized_output,
            Field::InBandResize => can.in_band_resize,
            Field::Kitty => can.kitty_keyboard_flags,
            Field::Prompt => can.prompt,
            Field::LinkUri => can.link_uri,
            Field::LinkGroup => can.link_group,
            Field::Title => can.title,
            Field::Reports => can.reports,
            Field::Wrapped => can.wrapped,
            Field::Width => can.widths,
            Field::Fg => can.fg,
            Field::Bg => can.bg,
            Field::UnderlineColor => can.underline_color,
            Field::Bold => can.bold,
            Field::Dim => can.dim,
            Field::Italic => can.italic,
            Field::Underline => can.underline,
            Field::Blink => can.blink,
            Field::Inverse => can.inverse,
            Field::Hidden => can.hidden,
            Field::Strikeout => can.strikeout,
            Field::History => can.history,
            Field::HistoryWrapped => can.history && can.wrapped,
        }
    }
}

/// A mode, by name, what it is, and how to read it.
type Mode = (&'static str, Field, fn(&Snapshot) -> bool);

/// One difference: where (`key`, the same for every engine), what it is
/// about, and the two values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    pub key: String,
    pub field: Field,
    /// The screen cell, row and column, for a difference in a cell's style:
    /// an engine that could not read that cell's style does not vote on it
    /// (`Line::unread_from`).
    pub styled_cell: Option<(usize, usize)>,
    pub fux: String,
    pub other: String,
}

impl Diff {
    pub fn line(&self, engine: &str) -> String {
        format!("{}: fux-vt {}, {engine} {}", self.key, self.fux, self.other)
    }
}

/// The differences between fux-vt's snapshot and another engine's, both
/// masked to what the engine can tell; empty when they agree. A cell that
/// differs gives one difference for each part of it that differs.
pub fn differences(fux: &Snapshot, other: &Snapshot) -> Vec<Diff> {
    let mut out = Vec::new();
    let styled_cell = std::cell::Cell::new(None);
    let mut field = |key: String, field: Field, a: String, b: String| {
        if a != b {
            out.push(Diff {
                key,
                field,
                styled_cell: styled_cell.get(),
                fux: a,
                other: b,
            });
        }
    };
    field(
        "size".into(),
        Field::Size,
        format!("{}x{}", fux.rows, fux.cols),
        format!("{}x{}", other.rows, other.cols),
    );
    field(
        "cursor".into(),
        Field::Cursor,
        format!("{:?}", fux.cursor),
        format!("{:?}", other.cursor),
    );
    let flags: [Mode; 11] = [
        ("pending wrap", Field::PendingWrap, |s| s.pending_wrap),
        ("cursor visible", Field::CursorVisible, |s| s.cursor_visible),
        ("autowrap", Field::Autowrap, |s| s.autowrap),
        ("origin mode", Field::Origin, |s| s.origin),
        ("alternate screen", Field::Alternate, |s| s.alternate),
        ("application cursor", Field::ApplicationCursor, |s| {
            s.application_cursor
        }),
        ("application keypad", Field::ApplicationKeypad, |s| {
            s.application_keypad
        }),
        ("bracketed paste", Field::BracketedPaste, |s| {
            s.bracketed_paste
        }),
        ("focus reporting", Field::FocusReporting, |s| {
            s.focus_reporting
        }),
        ("synchronized output", Field::SynchronizedOutput, |s| {
            s.synchronized_output
        }),
        ("in-band resize", Field::InBandResize, |s| s.in_band_resize),
    ];
    for (name, f, get) in flags {
        field(name.into(), f, get(fux).to_string(), get(other).to_string());
    }
    field(
        "kitty keyboard flags".into(),
        Field::Kitty,
        fux.kitty_keyboard_flags.to_string(),
        other.kitty_keyboard_flags.to_string(),
    );
    // fux-vt reports titles as they are set and keeps none, so a reset
    // cannot clear one; Ghostty's RIS clears its title. An empty title on
    // the other engine's side is not compared.
    if !other.title.is_empty() {
        field(
            "title".into(),
            Field::Title,
            format!("{:?}", fux.title),
            format!("{:?}", other.title),
        );
    }
    field(
        "reports".into(),
        Field::Reports,
        format!("{:?}", fux.reports),
        format!("{:?}", other.reports),
    );
    // Which cells share a link, compared among the cells both give a link
    // with the same URI (a cell where they differ on it differs on its
    // URI): each such cell's link is named, in each snapshot, by the first
    // such cell in reading order that has the same link.
    let mut fux_firsts: HashMap<&str, (usize, usize)> = HashMap::new();
    let mut other_firsts: HashMap<&str, (usize, usize)> = HashMap::new();
    for (y, (a, b)) in fux.screen.iter().zip(&other.screen).enumerate() {
        field(
            format!("row {y} soft-wrapped"),
            Field::Wrapped,
            a.wrapped.to_string(),
            b.wrapped.to_string(),
        );
        field(
            format!("row {y} starts a prompt"),
            Field::Prompt,
            a.prompt.to_string(),
            b.prompt.to_string(),
        );
        for (x, (ca, cb)) in a.cells.iter().zip(&b.cells).enumerate() {
            let shared = match (&ca.link, &cb.link) {
                (Some(la), Some(lb)) if la.uri == lb.uri => Some((
                    *fux_firsts.entry(la.group.as_str()).or_insert((y, x)),
                    *other_firsts.entry(lb.group.as_str()).or_insert((y, x)),
                )),
                _ => None,
            };
            if ca == cb {
                continue;
            }
            styled_cell.set(None);
            let styled = b.unread_from.is_none_or(|from| x < from);
            let at = |part: &str| format!("cell ({y},{x}) {part}");
            field(
                at("text"),
                Field::Text,
                format!("{:?}", ca.text),
                format!("{:?}", cb.text),
            );
            field(
                at("width"),
                Field::Width,
                format!("{:?}", ca.width),
                format!("{:?}", cb.width),
            );
            let (la, lb) = (ca.link.as_ref(), cb.link.as_ref());
            field(
                at("link"),
                Field::LinkUri,
                format!("{:?}", la.map(|l| &l.uri)),
                format!("{:?}", lb.map(|l| &l.uri)),
            );
            if let Some((fa, fb)) = shared {
                field(
                    at("link's first cell"),
                    Field::LinkGroup,
                    format!("{fa:?}"),
                    format!("{fb:?}"),
                );
            }
            if !styled {
                continue;
            }
            styled_cell.set(Some((y, x)));
            let (sa, sb) = (&ca.style, &cb.style);
            for (name, f, va, vb) in [
                ("fg", Field::Fg, sa.fg, sb.fg),
                ("bg", Field::Bg, sa.bg, sb.bg),
                (
                    "underline colour",
                    Field::UnderlineColor,
                    sa.underline_color,
                    sb.underline_color,
                ),
            ] {
                field(at(name), f, format!("{va:?}"), format!("{vb:?}"));
            }
            for (name, f, va, vb) in [
                ("bold", Field::Bold, sa.bold, sb.bold),
                ("dim", Field::Dim, sa.dim, sb.dim),
                ("italic", Field::Italic, sa.italic, sb.italic),
                ("underline", Field::Underline, sa.underline, sb.underline),
                ("blink", Field::Blink, sa.blink, sb.blink),
                ("inverse", Field::Inverse, sa.inverse, sb.inverse),
                ("hidden", Field::Hidden, sa.hidden, sb.hidden),
                ("strikeout", Field::Strikeout, sa.strikeout, sb.strikeout),
            ] {
                field(at(name), f, va.to_string(), vb.to_string());
            }
        }
    }
    styled_cell.set(None);
    let ha: Vec<_> = fux.history.iter().rev().collect();
    let hb: Vec<_> = other.history.iter().rev().collect();
    if ha.len() > hb.len() {
        field(
            "history rows kept".into(),
            Field::History,
            ha.len().to_string(),
            hb.len().to_string(),
        );
    }
    for (back, (a, b)) in ha.iter().zip(&hb).enumerate() {
        field(
            format!("history row {back} from the newest"),
            Field::History,
            format!("{:?}", a.0),
            format!("{:?}", b.0),
        );
        field(
            format!("history row {back} from the newest soft-wrapped"),
            Field::HistoryWrapped,
            a.1.to_string(),
            b.1.to_string(),
        );
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
    use super::{Cell, Field, Line, Snapshot, Style, Width};

    fn row(links: &[Option<(&str, &str)>]) -> Snapshot {
        let cells = links
            .iter()
            .map(|l| {
                Cell::new("x", Width::Narrow, Style::default())
                    .linked(l.map(|(uri, name)| (uri.to_owned(), name.to_owned())))
            })
            .collect();
        Snapshot {
            rows: 1,
            cols: u16::try_from(links.len()).unwrap_or(0),
            screen: vec![Line {
                cells,
                wrapped: false,
                prompt: false,
                unread_from: None,
            }],
            ..Snapshot::default()
        }
    }

    fn fields(a: &Snapshot, b: &Snapshot) -> Vec<(String, Field)> {
        super::differences(a, b)
            .into_iter()
            .map(|d| (d.key, d.field))
            .collect()
    }

    /// Links are told apart by which cells share one, not by the engines'
    /// names for them, and only among the cells both link to one URI.
    #[test]
    fn links_compare_by_the_cells_they_share() {
        let (u, v) = (Some(("u", "1")), Some(("u", "2")));
        let fux = row(&[u, u, v, v]);
        // Other names, the same links.
        let same = row(&[
            Some(("u", "a")),
            Some(("u", "a")),
            Some(("u", "b")),
            Some(("u", "b")),
        ]);
        assert_eq!(fields(&fux, &same), []);
        // The two links made one.
        let merged = row(&[Some(("u", "a")); 4]);
        assert_eq!(
            fields(&fux, &merged),
            [
                ("cell (0,2) link's first cell".to_owned(), Field::LinkGroup),
                ("cell (0,3) link's first cell".to_owned(), Field::LinkGroup),
            ]
        );
        // A cell without the link differs on its URI alone: the cells after
        // it still share their link.
        let shorter = row(&[None, Some(("u", "a")), Some(("u", "b")), Some(("u", "b"))]);
        assert_eq!(
            fields(&fux, &shorter),
            [("cell (0,0) link".to_owned(), Field::LinkUri)]
        );
    }

    #[test]
    fn reports_keep_cursor_and_status_replies_only() {
        let bytes = b"\x1b[3;4R\x1b[0n\x1b[?1;2c\x1b[?62;22c\x1b[5;1Rjunk\x1b[3n";
        assert_eq!(super::reports(bytes), ["CSI 3;4R", "CSI 0n", "CSI 5;1R"]);
    }
}

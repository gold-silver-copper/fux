//! What a terminal shows and says, in one form both sides are read into:
//! the working tree's fux-vt and the base commit's. `side.rs` reads each
//! side's types into these through fux-vt's public API, so the two are
//! compared field by field.
//!
//! `side.rs` reads each kind (a colour, an event, an unhandled sequence)
//! by a match with no catch-all, so one a side has and the model does not
//! name fails to build, rather than matching silently.
use std::fmt::{self, Write};

/// A colour.
/// Why `Parser::process_until_stop` stopped: a frame began or ended, or
/// the call spent its work budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Begun,
    Ended,
    Budget,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

/// An underline's style.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Underline {
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// How a cell blinks.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Blink {
    None,
    Slow,
    Rapid,
}

/// Colours and rendition: every attribute fux-vt keeps.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub underline_style: Underline,
    pub inverse: bool,
    pub blink: Blink,
    pub hidden: bool,
    pub strikeout: bool,
}

/// A cell's hyperlink: its URI, and the key cells of one link share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub key: u64,
}

/// A cell. Its text is in its row's `text`, from where the cell before it
/// ends to `end`, so a row's cells cost no allocation each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub end: usize,
    pub wide: bool,
    pub continuation: bool,
    /// Through `CellRef::attributes`.
    pub style: Style,
}

/// A retained row: its identity, version, flags and cells.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub id: u64,
    pub version: u64,
    /// `Row::continues`: its line goes on in the next row.
    pub continues: bool,
    pub len: usize,
    pub is_empty: bool,
    pub has_links: bool,
    pub text: String,
    pub cells: Vec<Cell>,
    /// Each cell's link, by column, for the cells that have one.
    pub links: Vec<(usize, Link)>,
    /// The accessors that answered otherwise than the row's cells: for a
    /// live row, `Window::row`; for any, `Row::cell` beside `Row::cells`.
    pub disagreeing: Vec<&'static str>,
}

impl Row {
    /// Empties the row, keeping its buffers.
    pub fn clear(&mut self) {
        self.id = 0;
        self.version = 0;
        self.continues = false;
        self.len = 0;
        self.is_empty = false;
        self.has_links = false;
        self.text.clear();
        self.cells.clear();
        self.links.clear();
        self.disagreeing.clear();
    }

    /// The text of cell `i`.
    pub fn text_of(&self, i: usize) -> &str {
        let start = i
            .checked_sub(1)
            .and_then(|before| self.cells.get(before))
            .map_or(0, |c| c.end);
        let end = self.cells.get(i).map_or(start, |c| c.end);
        self.text.get(start..end).unwrap_or_default()
    }

    /// Cell `i`, its text with it, for a report.
    pub fn describe_cell(&self, i: usize) -> String {
        match self.cells.get(i) {
            Some(c) => {
                let link = self.links.iter().find(|(col, _)| *col == i);
                format!(
                    "{:?} wide {} continuation {} {:?} link {:?}",
                    self.text_of(i),
                    c.wide,
                    c.continuation,
                    c.style,
                    link.map(|(_, l)| l)
                )
            }
            None => "(no cell)".into(),
        }
    }

    /// The row's text, each cell's in `|`, for a report.
    pub fn line(&self) -> String {
        let mut out = String::new();
        for i in 0..self.cells.len() {
            let _ = write!(out, "{}|", self.text_of(i));
        }
        out
    }
}

/// An error, as `Debug` and `Display` give it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

/// What processing output gave the host, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Reply(Vec<u8>),
    Title(Vec<u8>),
    IconName(Vec<u8>),
    Bell,
    Notification(Vec<u8>),
    Clipboard {
        selection: Vec<u8>,
        data: Vec<u8>,
    },
    ColorQuery {
        number: u8,
        bel: bool,
    },
    CommandStarted,
    CommandEnded(Option<i32>),
    Csi {
        params: Vec<Vec<u16>>,
        intermediates: Vec<u8>,
        action: u8,
    },
    Escape {
        intermediates: Vec<u8>,
        action: u8,
    },
}

/// The options a parser is made with, as fux-vt names them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Setup {
    pub events: bool,
    pub extended_replies: bool,
    pub mode_reports: bool,
    pub in_band_resize: bool,
    pub size_reports: bool,
    pub color_scheme_updates: bool,
    pub kitty_keyboard: bool,
    pub reflow: bool,
    pub hyperlinks: bool,
    pub prompt_marks: bool,
    pub rectangle_checksums: bool,
    pub setting_reports: bool,
    /// `Feature::Palette`.
    pub palette: bool,
    /// `Feature::Pictures`.
    pub pictures: bool,
    /// The name and version the terminal answers as.
    pub identity: Option<(&'static str, &'static str)>,
}

/// The identities a case may answer as: fux's, and one too long for
/// XTVERSION (`Identity::MAX_LEN`).
pub const IDENTITIES: &[(&str, &str)] = &[
    ("fux", "0.17.0"),
    ("fux-vt-oracle", "1.2.3-pre+build"),
    ("a-name-of-forty-bytes-a-name-of-forty-by", "10.20.30"),
];

impl Setup {
    /// A pane fixture with reflow and pictures enabled.
    pub const fn pane() -> Setup {
        Setup {
            events: true,
            extended_replies: false,
            mode_reports: true,
            in_band_resize: true,
            size_reports: true,
            color_scheme_updates: true,
            kitty_keyboard: true,
            reflow: true,
            hyperlinks: true,
            prompt_marks: true,
            rectangle_checksums: false,
            setting_reports: true,
            palette: true,
            pictures: true,
            identity: Some(("fux", "0.17.0")),
        }
    }

    /// Each flag by name, to print, read back and turn off one at a time.
    pub fn flags(&mut self) -> [(&'static str, &mut bool); 14] {
        [
            ("events", &mut self.events),
            ("extended_replies", &mut self.extended_replies),
            ("mode_reports", &mut self.mode_reports),
            ("in_band_resize", &mut self.in_band_resize),
            ("size_reports", &mut self.size_reports),
            ("color_scheme_updates", &mut self.color_scheme_updates),
            ("kitty_keyboard", &mut self.kitty_keyboard),
            ("reflow", &mut self.reflow),
            ("hyperlinks", &mut self.hyperlinks),
            ("prompt_marks", &mut self.prompt_marks),
            ("rectangle_checksums", &mut self.rectangle_checksums),
            ("setting_reports", &mut self.setting_reports),
            ("palette", &mut self.palette),
            ("pictures", &mut self.pictures),
        ]
    }
}

impl fmt::Display for Setup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut copy = *self;
        let on: Vec<&str> = copy
            .flags()
            .into_iter()
            .filter(|(_, on)| **on)
            .map(|(name, _)| name)
            .collect();
        f.write_str(&on.join(","))?;
        if let Some((name, version)) = self.identity {
            write!(
                f,
                "{}identity={name}/{version}",
                if on.is_empty() { "" } else { "," }
            )?;
        }
        Ok(())
    }
}

/// Everything about the screen that is not its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub size: (u16, u16),
    pub cursor: (u16, u16),
    pub pending_wrap: bool,
    /// The modes, each by its name in the working tree's `Mode`, and
    /// whether it is set.
    pub modes: Vec<(String, bool)>,
    pub cursor_shape: u16,
    /// The mouse reporting and encoding, as their `Debug` names them.
    pub mouse: String,
    pub encoding: String,
    pub kitty_keyboard_flags: u8,
    /// How keys are encoded (`Screen::key_mode`), as its `Debug` names it.
    pub key_mode: String,
    pub history_len: usize,
    /// `Screen::mark`, as a number.
    pub mark: u64,
    pub resize_report: Option<Vec<u8>>,
    /// Whether the program changed a colour (`Screen::colors_changed`).
    pub colors_changed: bool,
    /// Whether the rows read from the bottom end where the history and
    /// screen do.
    pub rows_end_there: bool,
    /// The window's rows and columns (`Window::rows`, `Window::cols`).
    pub window: (u16, u16),
    /// The 256 palette entries and the dynamic colours (OSC 10 to 19), as
    /// the program has them: `None` where it set none.
    pub palette: Vec<Option<(u8, u8, u8)>>,
    pub dynamic: Vec<Option<(u8, u8, u8)>>,
    pub pictures: Vec<Shown>,
    /// The screen not in use: its rows' identities, whether each goes on
    /// in the next (`Row::continues`) and a hash of their cells, as `Screen::main_rows` or `alternate_rows` gives
    /// them.
    pub inactive: Vec<(u64, bool, u64)>,
}

/// A picture shown: its top row, first column, columns, rows, size in
/// pixels, and a hash of its pixels.
pub type Shown = (i64, u16, u16, u16, (u32, u32), u64);

/// A host's call between outputs (`Step::Host`): what fux does to a pane's
/// parser other than feed it and resize it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    /// `Parser::set_history_bytes`.
    HistoryBytes(usize),
    /// `Parser::clear_history`.
    ClearHistory,
    /// `Parser::set_host_palette`, entry `n % 16` known (a colour made of
    /// `n`), the rest not.
    Palette(u8),
    /// `Parser::set_host_graphics`: a cell of this many pixels, sixel drawn.
    Graphics(u16, u16),
}

impl Call {
    /// The call as `Case::to_text` writes it, after `host `.
    pub fn to_text(self) -> String {
        match self {
            Call::HistoryBytes(n) => format!("history_bytes {n}"),
            Call::ClearHistory => "clear_history".into(),
            Call::Palette(n) => format!("palette {n}"),
            Call::Graphics(w, h) => format!("graphics {w} {h}"),
        }
    }

    /// A call `to_text` wrote.
    pub fn from_text(text: &str) -> Option<Call> {
        let mut words = text.split_whitespace();
        let name = words.next()?;
        let mut number = || words.next()?.parse::<usize>().ok();
        Some(match name {
            "history_bytes" => Call::HistoryBytes(number()?),
            "clear_history" => Call::ClearHistory,
            "palette" => Call::Palette(u8::try_from(number()?).ok()?),
            "graphics" => Call::Graphics(
                u16::try_from(number()?).ok()?,
                u16::try_from(number()?).ok()?,
            ),
            _ => return None,
        })
    }
}

/// What a mark taken earlier says now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marked {
    /// Whether `Screen::mark` moved on from it.
    pub changed: bool,
    pub full_refresh: bool,
    /// The identities of the rows a host redraws: every row after a full
    /// refresh, else those `Row::changed_since` names.
    pub dirty: Vec<u64>,
}

/// A row looked up by its identity: its index in the retained rows
/// (`Screen::row_index`), if it is still one, and the version of the row
/// there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lookup {
    pub id: u64,
    pub index: Option<usize>,
    pub version: Option<u64>,
}

/// The light state a probe reads after each of its sequences: the cursor,
/// and the row the cursor is on, whose cells show the pen they were
/// printed with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glance {
    pub cursor: (u16, u16),
    pub pending_wrap: bool,
    pub origin_mode: bool,
    pub kitty_keyboard_flags: u8,
    pub row: String,
}

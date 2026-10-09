//! What a terminal shows and says, in one form both sides are read into:
//! the working tree's fux-vt and the pinned commit's. Each side's adapter
//! (`side.rs`) reads its own types into these through fux-vt's public API,
//! so the two are compared field by field even where their types differ.
//!
//! A kind one side has and the model does not name (a colour, an event, a
//! mouse mode added later) is read as `Other`, with its `Debug` text, so it
//! is a difference rather than a silent match.
use std::fmt::{self, Write};

/// A colour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
    Other(String),
}

/// An underline's style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Underline {
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
    Other(String),
}

/// How a cell blinks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blink {
    None,
    Slow,
    Rapid,
    Other(String),
}

/// Colours and rendition: every attribute fux-vt keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// A cell's hyperlink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub id: Option<String>,
    pub key: u64,
}

/// A cell. Its text is in its row's `text`, from where the cell before it
/// ends to `end`, so a row's cells cost no allocation each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub end: usize,
    pub has_contents: bool,
    pub wide: bool,
    pub continuation: bool,
    /// Through `CellRef::attributes`.
    pub style: Style,
    /// Whether `CellRef`'s own accessors (`fgcolor`, `bold`, ...) say what
    /// its attributes say.
    pub getters_agree: bool,
}

/// A retained row: its identity, version, flags and cells.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub id: u64,
    pub version: u64,
    pub wrapped: bool,
    pub prompt: bool,
    pub len: usize,
    pub is_empty: bool,
    pub text_len: usize,
    pub has_links: bool,
    pub text: String,
    pub cells: Vec<Cell>,
    /// Each cell's link, by column, for the cells that have one.
    pub links: Vec<(usize, Link)>,
    /// The accessors that answered otherwise than the row's cells: for a
    /// live row, `Screen::cell`, `Screen::link`, `Screen::row_wrapped` and
    /// `Screen::starts_prompt`; for any, `Row::cell` beside `Row::cells`.
    pub disagreeing: Vec<&'static str>,
}

impl Row {
    /// Empties the row, keeping its buffers.
    pub fn clear(&mut self) {
        self.id = 0;
        self.version = 0;
        self.wrapped = false;
        self.prompt = false;
        self.len = 0;
        self.is_empty = false;
        self.text_len = 0;
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
                    "{:?} contents {} wide {} continuation {} {:?}{} link {:?}",
                    self.text_of(i),
                    c.has_contents,
                    c.wide,
                    c.continuation,
                    c.style,
                    if c.getters_agree {
                        ""
                    } else {
                        " (its getters disagree)"
                    },
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
    Clipboard {
        selection: Vec<u8>,
        data: Vec<u8>,
    },
    ColorQuery {
        number: u8,
        bel: bool,
    },
    OtherEvent(String),
    Csi {
        params: Vec<Vec<u16>>,
        intermediates: Vec<u8>,
        action: u8,
    },
    Escape {
        intermediates: Vec<u8>,
        action: u8,
    },
    OtherUnhandled(String),
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
    /// `Feature::Palette`, which the pinned commit has not: see `side`.
    pub palette: bool,
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
    /// As fux sets up every pane (`fux::pane::OPTIONS`).
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
            identity: Some(("fux", "0.17.0")),
        }
    }

    /// Each flag by name, to print, read back and turn off one at a time.
    pub fn flags(&mut self) -> [(&'static str, &mut bool); 13] {
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
    pub scroll_region: (u16, u16),
    /// The mouse reporting and encoding, as their `Debug` names them.
    pub mouse: String,
    pub encoding: String,
    pub kitty_keyboard_flags: u8,
    pub modify_other_keys: Option<u8>,
    /// The pen, through `Screen::attributes`.
    pub pen: Style,
    /// Whether `Screen::bgcolor` and `Screen::inverse` say what the pen says.
    pub pen_getters_agree: bool,
    /// The link the program has open: its URI and id.
    pub hyperlink: Option<(String, Option<String>)>,
    pub history_len: usize,
    pub storage_cells: usize,
    /// `Screen::mark`, as a number.
    pub mark: u64,
    pub resize_report: Option<Vec<u8>>,
    /// The options the parser says it has.
    pub options: Setup,
    /// Whether the program changed a colour (`Screen::colors_changed`),
    /// which the pinned commit cannot: see `side`.
    pub colors_changed: bool,
    /// Whether `row_from_bottom` ends where the history and screen do.
    pub rows_end_there: bool,
}

/// What a mark taken earlier says now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marked {
    pub changed: bool,
    pub full_refresh: bool,
    /// The identities of the rows `dirty_rows_since` gives.
    pub dirty: Vec<u64>,
    /// The places and identities `dirty_live_rows_since` gives.
    pub dirty_live: Vec<(u16, u64)>,
}

/// A row looked up by its identity: where `offset_for_row` puts it, and the
/// version of the row `row_by_id` finds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lookup {
    pub id: u64,
    pub offset: Option<usize>,
    pub version: Option<u64>,
}

/// A window to read, and a selection to copy from it (`Screen::window`,
/// `Window::text`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub offset: usize,
    pub rows: u16,
    pub cols: u16,
    pub from: (u16, u16),
    pub to: (u16, u16),
    pub max_cells: usize,
    pub max_bytes: usize,
}

/// What a window showed: its size and offset, each row's wrap flag and
/// each cell as `Window::cell` gives it, and the copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub rows: u16,
    pub cols: u16,
    pub offset: usize,
    pub wrapped: Vec<bool>,
    /// Each row's identity and width as `Window::row` gives it.
    pub row_ids: Vec<Option<(u64, usize)>>,
    /// Each cell's text, halves and style as `Window::cell` gives it, one
    /// line a row.
    pub cells: Vec<String>,
    pub text: Result<String, Error>,
}

/// The light state a probe reads after each of its sequences: the cursor
/// and pen, and the row the cursor is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glance {
    pub cursor: (u16, u16),
    pub pending_wrap: bool,
    pub origin_mode: bool,
    pub pen: Style,
    pub kitty_keyboard_flags: u8,
    pub row: String,
}

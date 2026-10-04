//! The engines fux-vt is compared with: one interface for every terminal
//! emulator, in this process or behind a process of its own, and what
//! each can be asked.
use crate::engines;
use crate::snapshot::{Field, Snapshot};

/// A terminal emulator under comparison.
///
/// Every engine is fed the same bytes and resizes, and read into a
/// [`Snapshot`] in one shape, by these rules:
///
/// - A blank cell has empty text. A printed space reads as a blank
///   (`Cell::new` does this).
/// - The second half of a wide glyph is a `Width::Tail` cell, with no text
///   and no style or hyperlink of its own (`Cell::new` and `Cell::linked`
///   do this too).
/// - A cell's hyperlink is its URI and the engine's own name for the link
///   (`Cell::linked`): any text the same for the cells of one link and for
///   no other, or empty where the engine cannot tell links apart.
/// - A row's `prompt` says whether a prompt starts on it (OSC 133 ; A).
/// - An engine's spacer at the end of a row, where a wide glyph that did
///   not fit would have started, is a narrow blank.
/// - Colours: the default is `Color::Default`; palette entries 0–255 are
///   `Color::Idx`, however the engine names them (named colours 0–15
///   included); direct colour is `Color::Rgb`.
/// - The cursor is zero-based (row, column). A cursor waiting to wrap is
///   in the last column, with `pending_wrap` set.
/// - `history` holds the most recent `history_rows` rows of scrollback,
///   oldest first, as text with no trailing blanks, each with its wrap
///   flag. An engine keeps far more history than fux-vt can fill in a case.
/// - `reports` holds cursor position and status reports only, through
///   `snapshot::reports`.
/// - `title` is empty unless a program set one.
///
/// What an engine cannot tell is left at its default, and listed as
/// missing in its [`Can`], so it is never compared.
pub trait Engine {
    fn process(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String>;
    fn snapshot(&mut self, history_rows: usize) -> Result<Snapshot, String>;

    /// Every reply the terminal has written back so far, in order: what a
    /// terminal sends up its pty for its program to read. Empty for an
    /// engine that answers nothing, or whose answers are not taken here.
    fn replies(&self) -> Vec<u8> {
        Vec::new()
    }

    /// The rows of history it holds now, if it can tell (`footprint`
    /// divides its memory by them).
    fn history_len(&mut self) -> Option<usize> {
        None
    }

    /// Feeds a whole workload, `chunk` bytes at a time, as a program's
    /// output arrives. Engines behind a process stream it and wait once.
    fn feed(&mut self, bytes: &[u8], chunk: usize) -> Result<(), String> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let (now, later) = rest
                .split_at_checked(chunk.min(rest.len()))
                .unwrap_or((rest, &[]));
            self.process(now)?;
            rest = later;
        }
        Ok(())
    }
}

/// What an engine can be asked. A field it cannot tell is compared with
/// no one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Can {
    pub widths: bool,
    pub wrapped: bool,
    pub pending_wrap: bool,
    pub fg: bool,
    pub bg: bool,
    pub underline_color: bool,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strikeout: bool,
    pub cursor: bool,
    pub cursor_visible: bool,
    pub autowrap: bool,
    pub origin: bool,
    pub alternate: bool,
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    pub kitty_keyboard_flags: bool,
    pub synchronized_output: bool,
    pub in_band_resize: bool,
    /// A cell's hyperlink (OSC 8): where it points.
    pub link_uri: bool,
    /// Which cells share one hyperlink.
    pub link_group: bool,
    /// The rows where a prompt starts (OSC 133 ; A).
    pub prompt: bool,
    pub title: bool,
    pub reports: bool,
    pub history: bool,
}

impl Can {
    /// Everything: what fux-vt and Ghostty tell.
    pub const ALL: Can = Can {
        widths: true,
        wrapped: true,
        pending_wrap: true,
        fg: true,
        bg: true,
        underline_color: true,
        bold: true,
        dim: true,
        italic: true,
        underline: true,
        blink: true,
        inverse: true,
        hidden: true,
        strikeout: true,
        cursor: true,
        cursor_visible: true,
        autowrap: true,
        origin: true,
        alternate: true,
        application_cursor: true,
        application_keypad: true,
        bracketed_paste: true,
        focus_reporting: true,
        kitty_keyboard_flags: true,
        synchronized_output: true,
        in_band_resize: true,
        link_uri: true,
        link_group: true,
        prompt: true,
        title: true,
        reports: true,
        history: true,
    };

    /// What both can tell. A field the subject cannot tell is masked on
    /// both sides, as an engine's own `Can` masks one, so comparing with
    /// `subject.and(engine)` compares only what both can tell.
    pub const fn and(self, other: Can) -> Can {
        Can {
            widths: self.widths && other.widths,
            wrapped: self.wrapped && other.wrapped,
            pending_wrap: self.pending_wrap && other.pending_wrap,
            fg: self.fg && other.fg,
            bg: self.bg && other.bg,
            underline_color: self.underline_color && other.underline_color,
            bold: self.bold && other.bold,
            dim: self.dim && other.dim,
            italic: self.italic && other.italic,
            underline: self.underline && other.underline,
            blink: self.blink && other.blink,
            inverse: self.inverse && other.inverse,
            hidden: self.hidden && other.hidden,
            strikeout: self.strikeout && other.strikeout,
            cursor: self.cursor && other.cursor,
            cursor_visible: self.cursor_visible && other.cursor_visible,
            autowrap: self.autowrap && other.autowrap,
            origin: self.origin && other.origin,
            alternate: self.alternate && other.alternate,
            application_cursor: self.application_cursor && other.application_cursor,
            application_keypad: self.application_keypad && other.application_keypad,
            bracketed_paste: self.bracketed_paste && other.bracketed_paste,
            focus_reporting: self.focus_reporting && other.focus_reporting,
            kitty_keyboard_flags: self.kitty_keyboard_flags && other.kitty_keyboard_flags,
            synchronized_output: self.synchronized_output && other.synchronized_output,
            in_band_resize: self.in_band_resize && other.in_band_resize,
            link_uri: self.link_uri && other.link_uri,
            link_group: self.link_group && other.link_group,
            prompt: self.prompt && other.prompt,
            title: self.title && other.title,
            reports: self.reports && other.reports,
            history: self.history && other.history,
        }
    }

    /// The fields missing here, by name.
    pub fn missing(&self) -> Vec<&'static str> {
        let fields = [
            (self.widths, "widths"),
            (self.wrapped, "wrap flags"),
            (self.pending_wrap, "pending wrap"),
            (self.fg, "foreground"),
            (self.bg, "background"),
            (self.underline_color, "underline colour"),
            (self.bold, "bold"),
            (self.dim, "dim"),
            (self.italic, "italic"),
            (self.underline, "underline"),
            (self.blink, "blink"),
            (self.inverse, "inverse"),
            (self.hidden, "hidden"),
            (self.strikeout, "strikeout"),
            (self.cursor, "cursor"),
            (self.cursor_visible, "cursor visibility"),
            (self.autowrap, "autowrap"),
            (self.origin, "origin mode"),
            (self.alternate, "alternate screen"),
            (self.application_cursor, "application cursor"),
            (self.application_keypad, "application keypad"),
            (self.bracketed_paste, "bracketed paste"),
            (self.focus_reporting, "focus reporting"),
            (self.kitty_keyboard_flags, "kitty keyboard flags"),
            (self.synchronized_output, "synchronized output"),
            (self.in_band_resize, "in-band resize"),
            (self.link_uri, "link URIs"),
            (self.link_group, "link groups"),
            (self.prompt, "prompt marks"),
            (self.title, "title"),
            (self.reports, "reports"),
            (self.history, "history"),
        ];
        fields
            .iter()
            .filter(|(can, _)| !can)
            .map(|(_, name)| *name)
            .collect()
    }
}

/// Which parts of the blanks an engine makes (erasing, inserting,
/// deleting, scrolling a line in) it makes as xterm does: the pen's
/// colours and nothing else, as fux-vt does (fux-vt's README, CSI J / K;
/// DEC STD 070's ED and EL give a blank the empty rendition, and xterm's
/// `ClearCells` keeps only the colours, `bce`). The engines split three
/// ways here by choice: Ghostty, alacritty, xterm.js and tmux keep no
/// foreground; avt, wezterm and vt100 keep the pen's attributes; xterm and
/// libvterm keep the colours alone. On a part an engine makes otherwise,
/// a cell blank in fux-vt and in the engine shows the engine's choice, not
/// the cell's: its difference there is shown, and it does not vote on it
/// (`case::outvoted_on`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blanks {
    /// The pen's foreground.
    pub fg: bool,
    /// None of the pen's attributes: no bold, dim, italic, underline,
    /// blink, inverse, hidden or strikeout, and no underline colour.
    pub attributes: bool,
}

impl Blanks {
    /// Blanks made as xterm makes them.
    pub const XTERM: Blanks = Blanks {
        fg: true,
        attributes: true,
    };

    /// Whether a blank's `field` is made as xterm makes it. The background
    /// is not part of the split: the panel keeps the pen's (`bce`), but in
    /// wezterm's ICH, which keeps none of the pen.
    pub fn as_xterm(self, field: Field) -> bool {
        match field {
            Field::Fg => self.fg,
            Field::UnderlineColor
            | Field::Bold
            | Field::Dim
            | Field::Italic
            | Field::Underline
            | Field::Blink
            | Field::Inverse
            | Field::Hidden
            | Field::Strikeout => self.attributes,
            Field::Size
            | Field::Cursor
            | Field::PendingWrap
            | Field::CursorVisible
            | Field::Autowrap
            | Field::Origin
            | Field::Alternate
            | Field::ApplicationCursor
            | Field::ApplicationKeypad
            | Field::BracketedPaste
            | Field::FocusReporting
            | Field::SynchronizedOutput
            | Field::InBandResize
            | Field::Kitty
            | Field::Prompt
            | Field::LinkUri
            | Field::LinkGroup
            | Field::Title
            | Field::Reports
            | Field::Wrapped
            | Field::Text
            | Field::Width
            | Field::Bg
            | Field::History
            | Field::HistoryWrapped => true,
        }
    }

    /// The parts made otherwise, by name.
    pub fn unlike_xterm(self) -> Vec<&'static str> {
        [(self.fg, "foreground"), (self.attributes, "attributes")]
            .iter()
            .filter(|(as_xterm, _)| !as_xterm)
            .map(|(_, name)| *name)
            .collect()
    }
}

/// How a case's terminals start.
#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub rows: u16,
    pub cols: u16,
    /// The rows of history fux-vt keeps. Other engines keep far more; see
    /// [`Engine`].
    pub history: usize,
    /// Whether fux-vt reflows on resize. Other engines do what they do.
    pub reflow: bool,
}

/// An engine, as the harness knows it.
pub struct Kind {
    pub name: &'static str,
    pub about: &'static str,
    pub can: Can,
    /// Which parts of its blanks it makes as xterm does.
    pub blanks: Blanks,
    /// Whether it votes by default: every engine in this process but the
    /// vt100 crate, fux-vt's ancestor, whose inherited choices are what
    /// the vote is meant to catch.
    pub panel: bool,
    /// Whether it runs in this process: fast enough for random runs.
    pub in_process: bool,
    /// Whether it can run here, or why not.
    pub available: fn() -> Result<(), String>,
    pub make: fn(&Setup) -> Result<Box<dyn Engine>, String>,
}

pub fn always() -> Result<(), String> {
    Ok(())
}

/// fux-vt itself: the subject by default. `--subject` names another, and
/// fux-vt then joins the panel, as [`FUX_VT`].
pub const SUBJECT: Kind = Kind {
    name: "fux-vt",
    about: "the code under test",
    can: Can::ALL,
    blanks: Blanks::XTERM,
    panel: false,
    in_process: true,
    available: always,
    make: engines::fux_vt::make,
};

/// The engines fux-vt is compared with.
pub const ENGINES: &[Kind] = &[
    engines::ghostty::KIND,
    engines::alacritty::KIND,
    engines::libvterm::KIND,
    engines::avt::KIND,
    engines::wezterm::KIND,
    engines::vt100::KIND,
    engines::xterm_js::KIND,
    engines::tmux::KIND,
    engines::xterm::KIND,
];

pub fn find(name: &str) -> Option<usize> {
    ENGINES.iter().position(|k| k.name == name)
}

/// fux-vt's index beside [`ENGINES`]: one past the last, so every index
/// into `ENGINES` keeps its meaning. fux-vt is the subject by default, and
/// an engine of the panel, at this index, when another engine is.
pub const FUX_VT: usize = ENGINES.len();

/// The engine at `index`: one of [`ENGINES`], or fux-vt at [`FUX_VT`].
pub fn kind(index: usize) -> Option<&'static Kind> {
    const FUX_VT_KIND: &Kind = &SUBJECT;
    ENGINES
        .get(index)
        .or_else(|| (index == FUX_VT).then_some(FUX_VT_KIND))
}

/// The name of the engine at `index` (see [`kind`]), or `?`.
pub fn name(index: usize) -> &'static str {
    kind(index).map_or("?", |k| k.name)
}

//! The engines fux-vt is compared with: one interface for every terminal
//! emulator, in this process or behind a process of its own, and what
//! each can be asked.
use crate::engines;
use crate::snapshot::Snapshot;

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

/// The subject: fux-vt itself.
pub const SUBJECT: Kind = Kind {
    name: "fux-vt",
    about: "the code under test",
    can: Can::ALL,
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

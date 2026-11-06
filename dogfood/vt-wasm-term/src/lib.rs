//! vt-wasm-term: fux-vt as a WebAssembly module with plain `extern "C"`
//! exports, for a page that feeds it a program's output and paints rows.
//!
//! One terminal per module instance. The calls, all numbers and pointers
//! into the module's memory:
//!
//! - `vt_new(rows, cols, history_kib) -> 1 | 0`
//! - `vt_input(len) -> ptr`: room for `len` bytes, for the host to fill
//! - `vt_feed(len) -> reply_len`: reads the first `len` bytes put there;
//!   the replies a program would get (cursor reports, ...) are at
//!   `vt_replies()`, until the next feed
//! - `vt_render() -> frame_len`, then the frame at `vt_frame()`: the
//!   rows changed since the last render ([`render`] says the layout)
//! - `vt_resize(rows, cols) -> 1 | 0`
//!
//! Colours are resolved here, to RGB: the page knows nothing of palettes.

use fux_vt::{Color, Feature, Mark, Mode, Options, Parser, Rgb, Size, Sink};
use std::sync::Mutex;

/// The foreground and background shown for `Color::Default`, until a
/// program sets them (OSC 10, OSC 11).
const DEFAULT_FG: Rgb = Rgb { r: 0xd0, g: 0xd0, b: 0xd0 };
const DEFAULT_BG: Rgb = Rgb { r: 0x14, g: 0x14, b: 0x18 };

/// Attribute bits of a run in a frame.
pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const UNDERLINE: u8 = 4;
pub const STRIKE: u8 = 8;
pub const DIM: u8 = 16;
pub const WIDE: u8 = 32;
/// Header flags.
pub const CURSOR_SHOWN: u8 = 1;
pub const FULL: u8 = 2;

/// Collects what the program would be sent back.
#[derive(Default)]
struct Replies(Vec<u8>);

impl Sink for Replies {
    fn reply(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

/// A terminal and the buffers it shares with the host.
pub struct Term {
    parser: Parser,
    /// When the last frame was made; `None`: the next is full.
    mark: Option<Mark>,
    input: Vec<u8>,
    replies: Replies,
    frame: Vec<u8>,
    /// How many times room was refused (a history row, ...), in all.
    pub refused: u64,
}

impl Term {
    pub fn new(rows: u16, cols: u16, history_bytes: usize) -> Option<Term> {
        let size = Size::new(rows, cols).ok()?;
        let options = Options::new()
            .with(Feature::Events)
            .with(Feature::Palette)
            .with(Feature::Reflow);
        let parser = Parser::with_options(size, history_bytes, options).ok()?;
        Some(Term {
            parser,
            mark: None,
            input: Vec::new(),
            replies: Replies::default(),
            frame: Vec::new(),
            refused: 0,
        })
    }

    /// Reads `bytes`; what the program would be answered is in [`Term::replies`].
    pub fn feed(&mut self, bytes: &[u8]) {
        self.replies.0.clear();
        let refused = self.parser.process_all(bytes, &mut self.replies);
        self.refused = self.refused.saturating_add(refused);
    }

    pub fn replies(&self) -> &[u8] {
        &self.replies.0
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> bool {
        Size::new(rows, cols).is_ok_and(|size| self.parser.resize(size).is_ok())
    }

    /// The rows changed since the last frame (every row, the first time
    /// and after a scroll, resize or reset), as a frame. Little-endian:
    ///
    /// ```text
    /// u16 rows, u16 cols, u16 cursor row, u16 cursor col, u8 flags (CURSOR_SHOWN, FULL),
    /// rgb default bg, u16 title length, title (UTF-8), u16 row count, then each row:
    ///   u16 row number, u16 run count, then each run:
    ///     rgb fg, rgb bg, u8 attributes, u16 first column, u16 columns, u16 text length, text
    /// ```
    pub fn render(&mut self) -> &[u8] {
        let screen = self.parser.screen();
        let full = self.mark.is_none_or(|mark| screen.full_refresh_since(mark));
        let window = screen.window();
        let (cursor_row, cursor_col) = screen.cursor_position();
        let mut flags = if full { FULL } else { 0 };
        if screen.mode(Mode::ShowCursor) {
            flags |= CURSOR_SHOWN;
        }
        let colours = Colours::of(screen);
        let out = &mut self.frame;
        out.clear();
        for n in [window.rows(), window.cols(), cursor_row, cursor_col] {
            out.extend_from_slice(&n.to_le_bytes());
        }
        out.push(flags);
        out.extend_from_slice(&<[u8; 3]>::from(colours.bg));
        let title = screen.title().bytes;
        let title = title.get(..title.len().min(1024)).unwrap_or_default();
        push_text(out, title);
        let count_at = out.len();
        out.extend_from_slice(&[0, 0]);
        let mut count: u16 = 0;
        for (number, row) in window.iter() {
            if !full && self.mark.is_some_and(|mark| !row.changed_since(mark)) {
                continue;
            }
            count = count.saturating_add(1);
            out.extend_from_slice(&number.to_le_bytes());
            let runs_at = out.len();
            out.extend_from_slice(&[0, 0]);
            let mut runs: u16 = 0;
            let mut open: Option<Run> = None;
            for (col, cell) in (0u16..).zip(row.cells()) {
                if cell.is_wide_continuation() {
                    continue;
                }
                let style = colours.style(cell.attributes(), cell.is_wide());
                let text = match cell.shown() {
                    "" => " ",
                    text => text,
                };
                let width = if cell.is_wide() { 2 } else { 1 };
                match open.as_mut() {
                    Some(run) if run.style == style && style.attrs & WIDE == 0 => {
                        run.cols = run.cols.saturating_add(width);
                        run.text.push_str(text);
                    }
                    _ => {
                        if let Some(run) = open.take() {
                            run.write(out);
                            runs = runs.saturating_add(1);
                        }
                        open = Some(Run { style, col, cols: width, text: text.to_owned() });
                    }
                }
            }
            if let Some(run) = open.take() {
                run.write(out);
                runs = runs.saturating_add(1);
            }
            if let Some(slot) = out.get_mut(runs_at..runs_at.saturating_add(2)) {
                slot.copy_from_slice(&runs.to_le_bytes());
            }
        }
        if let Some(slot) = out.get_mut(count_at..count_at.saturating_add(2)) {
            slot.copy_from_slice(&count.to_le_bytes());
        }
        self.mark = Some(screen.mark());
        &self.frame
    }
}

fn push_text(out: &mut Vec<u8>, text: &[u8]) {
    let len = u16::try_from(text.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(text.get(..usize::from(len)).unwrap_or_default());
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: Rgb,
    bg: Rgb,
    attrs: u8,
}

struct Run {
    style: Style,
    col: u16,
    cols: u16,
    text: String,
}

impl Run {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&<[u8; 3]>::from(self.style.fg));
        out.extend_from_slice(&<[u8; 3]>::from(self.style.bg));
        out.push(self.style.attrs);
        out.extend_from_slice(&self.col.to_le_bytes());
        out.extend_from_slice(&self.cols.to_le_bytes());
        push_text(out, self.text.as_bytes());
    }
}

/// The colours the screen shows: the program's palette over xterm's.
struct Colours<'a> {
    screen: &'a fux_vt::Screen,
    fg: Rgb,
    bg: Rgb,
}

impl<'a> Colours<'a> {
    fn of(screen: &'a fux_vt::Screen) -> Self {
        let reverse = screen.mode(Mode::ReverseVideo);
        let fg = screen.dynamic_color(10).unwrap_or(DEFAULT_FG);
        let bg = screen.dynamic_color(11).unwrap_or(DEFAULT_BG);
        let (fg, bg) = if reverse { (bg, fg) } else { (fg, bg) };
        Colours { screen, fg, bg }
    }

    fn rgb(&self, color: Color, default: Rgb) -> Rgb {
        match color {
            Color::Default => default,
            Color::Idx(i) => self.screen.palette_color(i).unwrap_or_else(|| xterm(i)),
            Color::Rgb(rgb) => rgb,
        }
    }

    fn style(&self, a: fux_vt::Attributes, wide: bool) -> Style {
        let mut fg = self.rgb(a.foreground(), self.fg);
        let mut bg = self.rgb(a.background(), self.bg);
        if a.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        if a.hidden() {
            fg = bg;
        }
        let attrs = [
            (a.bold(), BOLD),
            (a.italic(), ITALIC),
            (a.underline(), UNDERLINE),
            (a.strikeout(), STRIKE),
            (a.dim(), DIM),
            (wide, WIDE),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .fold(0, |bits, (_, bit)| bits | bit);
        Style { fg, bg, attrs }
    }
}

/// xterm's colour for palette entry `i`.
pub fn xterm(i: u8) -> Rgb {
    const BASE: [[u8; 3]; 16] = [
        [0x00, 0x00, 0x00], [0xcd, 0x00, 0x00], [0x00, 0xcd, 0x00], [0xcd, 0xcd, 0x00],
        [0x00, 0x00, 0xee], [0xcd, 0x00, 0xcd], [0x00, 0xcd, 0xcd], [0xe5, 0xe5, 0xe5],
        [0x7f, 0x7f, 0x7f], [0xff, 0x00, 0x00], [0x00, 0xff, 0x00], [0xff, 0xff, 0x00],
        [0x5c, 0x5c, 0xff], [0xff, 0x00, 0xff], [0x00, 0xff, 0xff], [0xff, 0xff, 0xff],
    ];
    const LEVEL: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |n: u8| LEVEL.get(usize::from(n)).copied().unwrap_or(0);
    match i {
        0..=15 => BASE.get(usize::from(i)).copied().unwrap_or_default().into(),
        16..=231 => {
            let n = i - 16;
            [level(n / 36), level(n / 6 % 6), level(n % 6)].into()
        }
        _ => {
            let grey = 8u8.saturating_add((i - 232).saturating_mul(10));
            [grey; 3].into()
        }
    }
}

// The exports. A poisoned lock (a panic, which `panic = "abort"` makes
// impossible in the release build) reads as no terminal.

static TERM: Mutex<Option<Term>> = Mutex::new(None);

fn with<T>(default: T, f: impl FnOnce(&mut Term) -> T) -> T {
    match TERM.lock() {
        Ok(mut term) => term.as_mut().map_or(default, f),
        Err(_) => default,
    }
}

fn dim(n: u32) -> u16 {
    u16::try_from(n).unwrap_or(0)
}

/// Makes the terminal: 1, or 0 for a size it refuses.
#[unsafe(no_mangle)]
pub extern "C" fn vt_new(rows: u32, cols: u32, history_kib: u32) -> u32 {
    let history = usize::try_from(history_kib).unwrap_or(0).saturating_mul(1024);
    let Some(term) = Term::new(dim(rows), dim(cols), history) else {
        return 0;
    };
    match TERM.lock() {
        Ok(mut slot) => {
            *slot = Some(term);
            1
        }
        Err(_) => 0,
    }
}

/// Room for `len` bytes of output for the host to write, before `vt_feed`.
#[unsafe(no_mangle)]
pub extern "C" fn vt_input(len: u32) -> *mut u8 {
    with(std::ptr::null_mut(), |term| {
        term.input.clear();
        term.input.resize(usize::try_from(len).unwrap_or(0), 0);
        term.input.as_mut_ptr()
    })
}

/// Reads the first `len` bytes of the input; how many reply bytes there are.
#[unsafe(no_mangle)]
pub extern "C" fn vt_feed(len: u32) -> u32 {
    with(0, |term| {
        let input = std::mem::take(&mut term.input);
        let len = usize::try_from(len).unwrap_or(0).min(input.len());
        term.feed(input.get(..len).unwrap_or_default());
        term.input = input;
        u32::try_from(term.replies().len()).unwrap_or(0)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn vt_replies() -> *const u8 {
    with(std::ptr::null(), |term| term.replies().as_ptr())
}

/// Makes a frame; its length.
#[unsafe(no_mangle)]
pub extern "C" fn vt_render() -> u32 {
    with(0, |term| u32::try_from(term.render().len()).unwrap_or(0))
}

#[unsafe(no_mangle)]
pub extern "C" fn vt_frame() -> *const u8 {
    with(std::ptr::null(), |term| term.frame.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn vt_resize(rows: u32, cols: u32) -> u32 {
    with(0, |term| u32::from(term.resize(dim(rows), dim(cols))))
}

/// How many times room was refused so far (history rows lost, ...).
#[unsafe(no_mangle)]
pub extern "C" fn vt_refused() -> u32 {
    with(0, |term| u32::try_from(term.refused).unwrap_or(u32::MAX))
}

#[cfg(test)]
#[allow(clippy::panic, reason = "a test fails by panicking")]
mod tests {
    use super::*;

    /// A run read back: text, fg, bg, attribute bits, first column.
    type RunBack = (String, Rgb, Rgb, u8, u16);

    /// A frame read back: each row number with its runs' text, colours and bits.
    struct Frame {
        cursor: (u16, u16),
        full: bool,
        title: String,
        rows: Vec<(u16, Vec<RunBack>)>,
    }

    fn decode(mut b: &[u8]) -> Option<Frame> {
        fn u16_(b: &mut &[u8]) -> Option<u16> {
            let (n, rest) = b.split_first_chunk::<2>()?;
            *b = rest;
            Some(u16::from_le_bytes(*n))
        }
        fn rgb(b: &mut &[u8]) -> Option<Rgb> {
            let (n, rest) = b.split_first_chunk::<3>()?;
            *b = rest;
            Some((*n).into())
        }
        fn text(b: &mut &[u8]) -> Option<String> {
            let len = usize::from(u16_(b)?);
            let (t, rest) = b.split_at_checked(len)?;
            *b = rest;
            String::from_utf8(t.to_vec()).ok()
        }
        let _size = (u16_(&mut b)?, u16_(&mut b)?);
        let cursor = (u16_(&mut b)?, u16_(&mut b)?);
        let (&flags, rest) = b.split_first()?;
        b = rest;
        rgb(&mut b)?;
        let title = text(&mut b)?;
        let mut rows = Vec::new();
        for _ in 0..u16_(&mut b)? {
            let number = u16_(&mut b)?;
            let mut runs = Vec::new();
            for _ in 0..u16_(&mut b)? {
                let (fg, bg) = (rgb(&mut b)?, rgb(&mut b)?);
                let (&attrs, rest) = b.split_first()?;
                b = rest;
                let col = u16_(&mut b)?;
                let _cols = u16_(&mut b)?;
                runs.push((text(&mut b)?, fg, bg, attrs, col));
            }
            rows.push((number, runs));
        }
        Some(Frame { cursor, full: flags & FULL != 0, title, rows })
    }

    #[test]
    fn a_frame_carries_text_resolved_colours_and_only_changed_rows() {
        let Some(mut term) = Term::new(3, 10, 0) else { panic!("a terminal") };
        term.feed("\x1b]2;hello\x07\x1b[1;31mred\x1b[0m \x1b[48;5;21m界\x1b[m\r\nplain".as_bytes());
        let first = decode(term.render());
        let Some(first) = first else { panic!("a frame") };
        assert!(first.full);
        assert_eq!(first.title, "hello");
        assert_eq!(first.rows.len(), 3);
        let runs = first.rows.first().map(|(_, runs)| runs.clone()).unwrap_or_default();
        let red = runs.first().cloned();
        assert_eq!(red, Some(("red".into(), xterm(1), DEFAULT_BG, BOLD, 0)));
        let wide = runs.get(2).cloned();
        assert_eq!(wide, Some(("界".into(), DEFAULT_FG, xterm(21), WIDE, 4)));
        assert_eq!(first.cursor, (1, 5));
        // Only the row written since is sent again.
        term.feed(b"\x1b[3;1Hx");
        let Some(second) = decode(term.render()) else { panic!("a frame") };
        assert!(!second.full);
        let numbers: Vec<u16> = second.rows.iter().map(|(n, _)| *n).collect();
        assert_eq!(numbers, [2]);
        // A scroll sends every row.
        term.feed(b"\n\n");
        assert!(decode(term.render()).is_some_and(|f| f.full && f.rows.len() == 3));
    }

    #[test]
    fn a_cursor_report_is_a_reply() {
        let Some(mut term) = Term::new(5, 20, 0) else { panic!("a terminal") };
        term.feed(b"\x1b[3;7H\x1b[6n");
        assert_eq!(term.replies(), b"\x1b[3;7R");
    }
}

//! Pane terminal emulation and bounded history: a long-lived `vt100::Parser` fed by PTY output
//! plus the terminal queries answered on the application's behalf.
//!
//! Adapted from koh (MIT); the upstream notice is retained in LICENSES/koh.txt.

use vt100::Screen;

pub const MIN_DIM: u16 = 2;
/// Peer-controlled dimensions are clamped here so vt100 never allocates an unbounded grid.
pub const MAX_DIM: u16 = 512;
pub const MAX_CLIPBOARD_BASE64: usize = 16 * 1024;
const MAX_TITLE_CHARS: usize = 256;
/// Bound on one OSC/DCS/APC control string before it is dropped.
const MAX_CONTROL_STRING_BYTES: usize = 64 * 1024;

pub fn clamp_dims(rows: u16, cols: u16) -> (u16, u16) {
    (rows.clamp(MIN_DIM, MAX_DIM), cols.clamp(MIN_DIM, MAX_DIM))
}

/// A ConEmu / Windows Terminal progress report (`OSC 9;4;<state>;<percent>`), exposed through the
/// control listing for external observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub state: u8,
    pub percent: u8,
}

fn parse_progress(params: &[&[u8]]) -> Option<Progress> {
    if params.len() < 3
        || params.first().copied() != Some(b"9".as_slice())
        || params.get(1).copied() != Some(b"4".as_slice())
    {
        return None;
    }
    let number = |index: usize| -> Option<u8> {
        std::str::from_utf8(params.get(index).copied()?)
            .ok()?
            .parse::<u8>()
            .ok()
    };
    let state = number(2)?;
    if state > 4 {
        return None;
    }
    let percent = if state == 0 { 0 } else { number(3)? };
    (percent <= 100).then_some(Progress { state, percent })
}

fn title_from(bytes: &[u8]) -> String {
    crate::view::printable(&String::from_utf8_lossy(bytes), MAX_TITLE_CHARS)
}

#[derive(Default)]
struct Callbacks {
    title: String,
    clipboard: String,
    bell_count: u64,
    /// Query answers the application expects back on its input.
    host_replies: Vec<u8>,
    progress: Option<Progress>,
}

impl vt100::Callbacks for Callbacks {
    fn set_window_title(&mut self, _: &mut Screen, title: &[u8]) {
        self.title = title_from(title);
    }
    fn set_window_icon_name(&mut self, _: &mut Screen, _: &[u8]) {}
    fn audible_bell(&mut self, _: &mut Screen) {
        self.bell_count = self.bell_count.saturating_add(1);
    }
    fn copy_to_clipboard(&mut self, _: &mut Screen, _selection: &[u8], data: &[u8]) {
        if data.len() <= MAX_CLIPBOARD_BASE64 {
            self.clipboard = String::from_utf8_lossy(data).into_owned();
        }
    }
    fn unhandled_osc(&mut self, _: &mut Screen, params: &[&[u8]]) {
        match parse_progress(params) {
            Some(progress) if progress.state == 0 => self.progress = None,
            Some(progress) => self.progress = Some(progress),
            None => {}
        }
    }
    fn unhandled_csi(
        &mut self,
        screen: &mut Screen,
        intermediate: Option<u8>,
        second: Option<u8>,
        params: &[&[u16]],
        action: char,
    ) {
        let first = params
            .first()
            .and_then(|values| values.first())
            .copied()
            .unwrap_or(0);
        match (intermediate, second, action) {
            (None, _, 'n') => match first {
                6 => {
                    let (row, column) = screen.cursor_position();
                    self.host_replies.extend_from_slice(
                        format!("\x1b[{};{}R", u32::from(row) + 1, u32::from(column) + 1)
                            .as_bytes(),
                    );
                }
                5 => self.host_replies.extend_from_slice(b"\x1b[0n"),
                _ => {}
            },
            (Some(b'?'), _, 'n') if first == 6 => {
                let (row, column) = screen.cursor_position();
                self.host_replies.extend_from_slice(
                    format!("\x1b[?{};{}R", u32::from(row) + 1, u32::from(column) + 1).as_bytes(),
                );
            }
            (None, _, 'c') => self.host_replies.extend_from_slice(b"\x1b[?62;1;6c"),
            (Some(b'>'), _, 'c') => self.host_replies.extend_from_slice(b"\x1b[>1;10;0c"),
            (Some(b'?'), Some(b'$'), 'p') => {
                let status: u16 = match first {
                    2004 => {
                        if screen.bracketed_paste() {
                            1
                        } else {
                            2
                        }
                    }
                    _ => 0,
                };
                self.host_replies
                    .extend_from_slice(format!("\x1b[?{first};{status}$y").as_bytes());
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlStringKind {
    Osc,
    Other,
}

/// Bounds control strings independently of pane history and keeps UTF-8 continuation bytes from
/// being mistaken for C1 string introducers or terminators.
#[derive(Default)]
struct ControlStringFilter {
    utf8_remaining: u8,
    pending_escape: bool,
    string: Option<ControlStringKind>,
    string_escape: bool,
    dropping: bool,
    buffered: Vec<u8>,
}

impl ControlStringFilter {
    fn process(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len().min(MAX_CONTROL_STRING_BYTES));
        for &byte in input {
            let continuation = self.utf8_remaining > 0 && (0x80..=0xbf).contains(&byte);
            self.utf8_remaining = if continuation {
                self.utf8_remaining - 1
            } else {
                match byte {
                    0xc2..=0xdf => 1,
                    0xe0..=0xef => 2,
                    0xf0..=0xf4 => 3,
                    _ => 0,
                }
            };
            if let Some(kind) = self.string {
                let terminated = (byte == 0x9c && !continuation)
                    || (kind == ControlStringKind::Osc && byte == 0x07)
                    || (self.string_escape && byte == b'\\');
                if !self.dropping {
                    if self.buffered.len() < MAX_CONTROL_STRING_BYTES {
                        self.buffered.push(byte);
                    } else {
                        self.buffered.clear();
                        self.dropping = true;
                    }
                }
                self.string_escape = byte == 0x1b;
                if terminated || matches!(byte, 0x18 | 0x1a) {
                    if !self.dropping {
                        output.append(&mut self.buffered);
                    }
                    self.buffered.clear();
                    self.string = None;
                    self.string_escape = false;
                    self.dropping = false;
                }
                continue;
            }
            if self.pending_escape {
                self.pending_escape = false;
                if let Some(kind) = control_string_introducer(byte) {
                    self.string = Some(kind);
                    self.buffered.extend_from_slice(&[0x1b, byte]);
                    continue;
                }
                output.push(0x1b);
            }
            if byte == 0x1b {
                self.pending_escape = true;
            } else if let Some(kind) = c1_control_string_introducer(byte).filter(|_| !continuation)
            {
                self.string = Some(kind);
                self.buffered.push(byte);
            } else {
                output.push(byte);
            }
        }
        output
    }
}

fn control_string_introducer(byte: u8) -> Option<ControlStringKind> {
    match byte {
        b']' => Some(ControlStringKind::Osc),
        b'P' | b'X' | b'_' | b'^' => Some(ControlStringKind::Other),
        _ => None,
    }
}

fn c1_control_string_introducer(byte: u8) -> Option<ControlStringKind> {
    match byte {
        0x9d => Some(ControlStringKind::Osc),
        0x90 | 0x98 | 0x9e | 0x9f => Some(ControlStringKind::Other),
        _ => None,
    }
}

/// The authoritative emulator and bounded history for one pane.
pub struct ServerTerminal {
    parser: vt100::Parser<Callbacks>,
    filter: ControlStringFilter,
    history_limit: usize,
}

impl ServerTerminal {
    #[must_use]
    pub fn new(rows: u16, cols: u16, history_limit: usize) -> Self {
        let (rows, cols) = clamp_dims(rows, cols);
        Self {
            parser: vt100::Parser::new_with_callbacks(
                rows,
                cols,
                history_limit,
                Callbacks::default(),
            ),
            filter: ControlStringFilter::default(),
            history_limit,
        }
    }

    /// Feeds application output. A `vt100` panic on hostile output is contained: the chunk is
    /// dropped and later output repaints.
    pub fn process(&mut self, bytes: &[u8]) {
        let filtered = self.filter.process(bytes);
        let contained = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.parser.process(&filtered)
        }));
        if contained.is_err() {
            tracing::error!("terminal emulator rejected application output; chunk dropped");
        }
    }

    /// Bytes the application must receive in reply to its queries (DSR, DA, DECRQM).
    pub fn take_host_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.parser.callbacks_mut().host_replies)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = clamp_dims(rows, cols);
        self.parser.screen_mut().set_size(rows, cols);
    }

    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    #[must_use]
    pub fn screen(&self) -> &Screen {
        self.parser.screen()
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.parser.callbacks().title
    }

    #[must_use]
    pub fn clipboard(&self) -> &str {
        &self.parser.callbacks().clipboard
    }

    #[must_use]
    pub fn bell_count(&self) -> u64 {
        self.parser.callbacks().bell_count
    }

    #[must_use]
    pub fn progress(&self) -> Option<Progress> {
        self.parser.callbacks().progress
    }

    #[must_use]
    pub fn history_limit(&self) -> usize {
        self.history_limit
    }

    /// Temporarily selects a bounded history viewport and reads it, restoring the live viewport
    /// before returning. The offset is clamped to the retained history.
    pub fn with_history_screen<R>(&mut self, offset: usize, read: impl FnOnce(&Screen) -> R) -> R {
        let previous = self.parser.screen().scrollback();
        self.parser
            .screen_mut()
            .set_scrollback(offset.min(self.history_limit));
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(self.parser.screen())));
        self.parser.screen_mut().set_scrollback(previous);
        match result {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// Plain or attribute-preserving text of the visible screen preceded by up to `scrollback`
    /// history rows, truncated to `max_bytes` on a character boundary.
    pub fn capture(&mut self, scrollback: usize, attrs: bool, max_bytes: usize) -> String {
        let columns = usize::from(self.size().1).max(1);
        let bytes_per_row = if attrs {
            columns.saturating_mul(128)
        } else {
            columns.saturating_mul(4).saturating_add(1)
        };
        let bounded_rows = max_bytes.saturating_div(bytes_per_row).saturating_add(1);
        let rows = scrollback.min(self.history_limit).min(bounded_rows);
        let mut output = self.with_history_screen(rows, |screen| {
            if attrs {
                String::from_utf8_lossy(&screen.contents_formatted()).into_owned()
            } else {
                screen.contents()
            }
        });
        if output.len() > max_bytes {
            output.truncate(output.floor_char_boundary(max_bytes));
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_cursor_position_and_device_attributes() {
        let mut terminal = ServerTerminal::new(24, 80, 0);
        terminal.process(b"\x1b[5;3H\x1b[6n");
        assert_eq!(terminal.take_host_replies(), b"\x1b[5;3R");
        assert!(terminal.take_host_replies().is_empty());
        terminal.process(b"\x1b[c\x1b[>c\x1b[?2004$p");
        assert_eq!(
            terminal.take_host_replies(),
            b"\x1b[?62;1;6c\x1b[>1;10;0c\x1b[?2004;2$y"
        );
    }

    #[test]
    fn title_bell_clipboard_and_progress_are_captured_and_bounded() {
        let mut terminal = ServerTerminal::new(24, 80, 0);
        terminal.process(b"\x1b]2;my-title\x07\x07\x1b]52;c;aGVsbG8=\x07\x1b]9;4;1;50\x1b\\");
        assert_eq!(terminal.title(), "my-title");
        assert_eq!(terminal.bell_count(), 1);
        assert_eq!(terminal.clipboard(), "aGVsbG8=");
        assert_eq!(
            terminal.progress(),
            Some(Progress {
                state: 1,
                percent: 50
            })
        );
        terminal.process(b"\x1b]9;4;0\x07");
        assert_eq!(terminal.progress(), None);
        let huge = "x".repeat(MAX_TITLE_CHARS + 500);
        terminal.process(format!("\x1b]2;{huge}\x07").as_bytes());
        assert_eq!(terminal.title().chars().count(), MAX_TITLE_CHARS);
        let big = "A".repeat(MAX_CLIPBOARD_BASE64 + 1);
        terminal.process(format!("\x1b]52;c;{big}\x07").as_bytes());
        assert_eq!(terminal.clipboard(), "aGVsbG8=");
    }

    #[test]
    fn resize_clamps_and_wide_output_never_panics() {
        let mut terminal = ServerTerminal::new(24, 80, 0);
        terminal.resize(65000, 65000);
        assert_eq!(terminal.size(), (MAX_DIM, MAX_DIM));
        terminal.resize(0, 0);
        assert_eq!(terminal.size(), (MIN_DIM, MIN_DIM));
        terminal.process("AAAA日本🦀\r\nBBBB\r\n".repeat(8).as_bytes());
        terminal.resize(40, 120);
        assert_eq!(terminal.size(), (40, 120));
    }

    #[test]
    fn unterminated_control_strings_are_bounded_and_reset() {
        let mut terminal = ServerTerminal::new(24, 80, 0);
        terminal.process(b"before\x1b]");
        for _ in 0..32 {
            terminal.process(&vec![b'x'; MAX_CONTROL_STRING_BYTES / 4]);
            assert!(terminal.filter.buffered.len() <= MAX_CONTROL_STRING_BYTES);
        }
        assert!(terminal.filter.dropping);
        terminal.process(b"\x1b\\after");
        assert!(!terminal.filter.dropping);
        assert!(terminal.screen().contents().contains("beforeafter"));
    }

    #[test]
    fn utf8_continuations_do_not_open_or_terminate_control_strings() {
        let text = "beforeАИМНОП😀after";
        for split in 0..=text.len() {
            let mut terminal = ServerTerminal::new(24, 80, 0);
            let (first, second) = text.as_bytes().split_at(split);
            terminal.process(first);
            terminal.process(second);
            assert_eq!(terminal.screen().contents(), text, "split {split}");
        }
        let mut terminal = ServerTerminal::new(24, 80, 0);
        for byte in "\x1b]2;М title\x1b\\visible".as_bytes() {
            terminal.process(&[*byte]);
        }
        assert_eq!(terminal.title(), "М title");
        assert_eq!(terminal.screen().contents(), "visible");
        terminal.process(b"\x1b]2;");
        terminal.process(&vec![b'x'; MAX_CONTROL_STRING_BYTES]);
        for byte in "Мstill hidden".as_bytes() {
            terminal.process(&[*byte]);
        }
        assert!(terminal.filter.dropping);
        terminal.process(b"\x1b\\after");
        assert_eq!(terminal.title(), "М title");
        assert_eq!(terminal.screen().contents(), "visibleafter");
    }

    #[test]
    fn split_osc_and_csi_sequences_keep_their_meaning() {
        let mut terminal = ServerTerminal::new(24, 80, 0);
        for chunk in [
            &b"\x1b"[..],
            &b"]2;split"[..],
            &b" title\x1b"[..],
            &b"\\\x1b"[..],
            &b"[5;3Hok"[..],
        ] {
            terminal.process(chunk);
        }
        assert_eq!(terminal.title(), "split title");
        assert!(terminal.screen().contents().contains("ok"));
        for introducer in [&b"\x1bX"[..], &b"\x98"[..]] {
            let mut terminal = ServerTerminal::new(24, 80, 0);
            terminal.process(introducer);
            for _ in 0..5 {
                terminal.process(&vec![b'x'; MAX_CONTROL_STRING_BYTES / 4]);
            }
            assert!(terminal.filter.dropping);
            terminal.process(b"\x1b\\ok");
            assert!(terminal.screen().contents().contains("ok"));
        }
    }

    #[test]
    fn history_reads_restore_the_live_viewport_and_capture_is_bounded() {
        let mut terminal = ServerTerminal::new(4, 10, 100);
        for line in 0..20 {
            terminal.process(format!("line{line}\r\n").as_bytes());
        }
        let live = terminal.screen().contents();
        let older = terminal.with_history_screen(10, |screen| screen.contents());
        assert!(older.contains("line8"), "{older}");
        assert_eq!(terminal.screen().contents(), live);
        assert_eq!(terminal.screen().scrollback(), 0);
        let capture = terminal.capture(50, false, 40);
        assert!(capture.len() <= 40);
        let full = terminal.capture(50, false, 100_000);
        assert!(full.contains("line0"));
        assert!(terminal.with_history_screen(usize::MAX, |screen| screen.scrollback()) <= 100);
    }
}

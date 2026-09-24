//! A pane: its terminal emulator, its process, and the input waiting for it.
use crate::layout::PaneId;
use crate::process::Child;
use std::collections::VecDeque;

/// The largest single piece of input: a whole paste and its envelope.
pub const MAX_INPUT: usize = crate::decode::PASTE_LIMIT + 12;
/// Input may wait for the program up to this many bytes, however many
/// pieces it came in (bevy-final finding 021).
pub const INPUT_BYTES: usize = 16 * MAX_INPUT;
/// What one queued piece costs beyond its bytes.
pub const ENTRY_COST: usize = 64;

/// Input and terminal replies waiting for the pane's program to read them,
/// bounded by what they cost rather than by how many pieces they came in.
#[derive(Default)]
pub struct InputQueue {
    pieces: VecDeque<Vec<u8>>,
    /// Bytes of the front piece already written.
    written: usize,
    cost: usize,
}

impl InputQueue {
    /// Queues `bytes`, or refuses them whole if the queue is full.
    pub fn push(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        let cost = bytes.len() + ENTRY_COST;
        if self.cost + cost > INPUT_BYTES {
            return Err(
                "the pane's program is not reading its input; nothing more is queued until it does"
                    .into(),
            );
        }
        self.cost += cost;
        self.pieces.push_back(bytes);
        Ok(())
    }
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }
    /// The bytes to write next.
    pub fn front(&self) -> Option<&[u8]> {
        self.pieces.front().and_then(|p| p.get(self.written..))
    }
    /// `n` bytes of the front were written.
    pub fn advance(&mut self, n: usize) {
        self.written += n;
        if let Some(front) = self.pieces.front()
            && self.written >= front.len()
        {
            self.cost = self.cost.saturating_sub(front.len() + ENTRY_COST);
            self.pieces.pop_front();
            self.written = 0;
        }
    }
    /// Everything queued, for tests and for a pane with no process.
    pub fn drain_all(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(front) = self.front() {
            out.extend_from_slice(front);
            let n = front.len();
            self.advance(n);
        }
        out
    }
}

/// Modes fux tracks from a pane's output itself: fux-vt keeps the screen,
/// and these two decide what fux sends outside it.
#[derive(Default)]
pub struct Modes {
    /// `CSI ? 1004 h`: the program wants focus-in and focus-out reports.
    pub focus_reporting: bool,
    /// The last `CSI Ps SP q` (DECSCUSR) cursor shape; 0 is the default.
    pub cursor_shape: u16,
    state: Scan,
    params: Vec<u8>,
    private: bool,
    space: bool,
    other: bool,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Scan {
    #[default]
    Ground,
    Escape,
    Csi,
}

impl Modes {
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match self.state {
                Scan::Ground => {
                    if byte == 0x1b {
                        self.state = Scan::Escape;
                    }
                }
                Scan::Escape => match byte {
                    b'[' => {
                        self.state = Scan::Csi;
                        self.params.clear();
                        self.private = false;
                        self.space = false;
                        self.other = false;
                    }
                    // RIS: a full reset.
                    b'c' => {
                        self.focus_reporting = false;
                        self.cursor_shape = 0;
                        self.state = Scan::Ground;
                    }
                    0x1b => {}
                    _ => self.state = Scan::Ground,
                },
                Scan::Csi => match byte {
                    b'?' if self.params.is_empty() && !self.private => self.private = true,
                    b'0'..=b'9' | b';' => {
                        if self.params.len() < 64 {
                            self.params.push(byte);
                        } else {
                            self.other = true;
                        }
                    }
                    b' ' => self.space = true,
                    0x40..=0x7e => {
                        self.dispatch(byte);
                        self.state = Scan::Ground;
                    }
                    0x1b => self.state = Scan::Escape,
                    0x18 | 0x1a => self.state = Scan::Ground,
                    _ => self.other = true,
                },
            }
        }
    }

    fn dispatch(&mut self, last: u8) {
        if self.other {
            return;
        }
        let params = std::str::from_utf8(&self.params).unwrap_or("");
        if self.private && !self.space && (last == b'h' || last == b'l') {
            if params.split(';').any(|p| p == "1004") {
                self.focus_reporting = last == b'h';
            }
        } else if !self.private && self.space && last == b'q' {
            self.cursor_shape = params
                .split(';')
                .next()
                .and_then(|p| p.parse().ok())
                .unwrap_or(0);
        }
    }
}

/// Replies (DSR, DA) and events the parser produces while reading output.
struct Sink<'a> {
    replies: &'a mut Vec<u8>,
    title: &'a mut Option<String>,
}

impl fux_vt::Sink for Sink<'_> {
    fn reply(&mut self, bytes: &[u8]) {
        if self.replies.len() + bytes.len() <= 4096 {
            self.replies.extend_from_slice(bytes);
        }
    }
    fn event(&mut self, event: fux_vt::Event<'_>) {
        if let fux_vt::Event::Title(title) = event {
            let text: String = String::from_utf8_lossy(title)
                .chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect();
            *self.title = Some(text);
        }
    }
}

pub struct Pane {
    pub id: PaneId,
    pub name: String,
    /// Set by the program with OSC 0 or 2.
    pub title: String,
    pub parser: fux_vt::Parser,
    pub modes: Modes,
    /// The PTY size, the smallest rectangle any client shows the pane in.
    pub size: (u16, u16),
    pub child: Option<Child>,
    pub input: InputQueue,
    /// Whether a reply was dropped because the queue was full; noticed once.
    pub reply_dropped: bool,
    /// The shell's program, to quote a typed command for it.
    pub shell: String,
}

impl Pane {
    pub fn new(
        id: PaneId,
        name: String,
        shell: String,
        rows: u16,
        cols: u16,
        history: usize,
    ) -> Result<Pane, String> {
        let options = fux_vt::Options {
            events: true,
            extended_replies: false,
        };
        let parser = fux_vt::Parser::with_options(rows.max(1), cols.max(1), history, options)
            .map_err(|e| {
                format!("a {rows}x{cols} terminal with {history} lines of history: {e}")
            })?;
        Ok(Pane {
            id,
            name,
            title: String::new(),
            parser,
            modes: Modes::default(),
            size: (rows.max(1), cols.max(1)),
            child: None,
            input: InputQueue::default(),
            reply_dropped: false,
            shell,
        })
    }

    /// Reads program output into the screen. Returns whether a reply had to
    /// be dropped because the program is not reading its input.
    pub fn output(&mut self, bytes: &[u8]) -> bool {
        self.modes.feed(bytes);
        let mut replies = Vec::new();
        let mut title = None;
        let mut sink = Sink {
            replies: &mut replies,
            title: &mut title,
        };
        // The parser refuses only allocations beyond its limits; the screen
        // stays as it was and output continues.
        let _ = self.parser.process_with(bytes, &mut sink);
        if let Some(title) = title {
            self.title = title;
        }
        if !replies.is_empty() && self.input.push(replies).is_err() && !self.reply_dropped {
            self.reply_dropped = true;
            return true;
        }
        false
    }

    /// Resizes the screen and the PTY.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if self.size == (rows, cols) {
            return;
        }
        if self.parser.resize(rows, cols).is_ok() {
            self.size = (rows, cols);
            if let Some(child) = &self.child {
                crate::process::resize(&child.master, rows, cols);
            }
        }
    }

    pub fn screen(&self) -> &fux_vt::Screen {
        self.parser.screen()
    }

    /// The name the bar shows: the program's title, else the pane's name.
    pub fn label(&self) -> &str {
        if self.title.is_empty() {
            &self.name
        } else {
            &self.title
        }
    }

    /// Whether the shell is the only thing running: nothing in the foreground
    /// but the shell's own group.
    pub fn idle(&self) -> bool {
        match &self.child {
            Some(child) => {
                crate::process::foreground(&child.master).is_none_or(|group| group == child.pid)
            }
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_is_bounded_by_bytes_not_pieces() {
        let mut queue = InputQueue::default();
        // Thousands of one-byte keys fit: the bound is cost, not count (021).
        for _ in 0..3000 {
            assert!(queue.push(vec![b'k']).is_ok());
        }
        let mut big = InputQueue::default();
        let mut pushed = 0;
        while big.push(vec![0; MAX_INPUT]).is_ok() {
            pushed += 1;
        }
        assert_eq!(pushed, 15);
        big.advance(MAX_INPUT);
        assert!(
            big.push(vec![0; MAX_INPUT]).is_ok(),
            "space is freed as it is written"
        );
        let mut partial = InputQueue::default();
        assert!(partial.push(b"abc".to_vec()).is_ok());
        partial.advance(1);
        assert_eq!(partial.front(), Some(&b"bc"[..]));
        assert_eq!(partial.drain_all(), b"bc");
        assert!(partial.is_empty());
    }

    #[test]
    fn modes_follow_output_split_anywhere() {
        let stream = b"x\x1b[?1004;2004hy\x1b[5 qz\x1b[?25l";
        for split in 0..stream.len() {
            let (a, b) = stream.split_at(split);
            let mut modes = Modes::default();
            modes.feed(a);
            modes.feed(b);
            assert!(modes.focus_reporting, "split {split}");
            assert_eq!(modes.cursor_shape, 5, "split {split}");
        }
        let mut modes = Modes::default();
        modes.feed(b"\x1b[?1004h\x1b[?1004l\x1b[2 q\x1b[ q");
        assert!(!modes.focus_reporting);
        assert_eq!(modes.cursor_shape, 0);
        modes.feed(b"\x1b[?1004h\x1bc");
        assert!(!modes.focus_reporting);
    }

    #[test]
    fn output_answers_queries_and_takes_titles() -> Result<(), String> {
        let mut pane = Pane::new(PaneId(1), "sh".into(), "/bin/sh".into(), 5, 20, 10)?;
        pane.output(b"hello\x1b]2;my title\x07\x1b[6n");
        assert_eq!(pane.title, "my title");
        assert_eq!(pane.label(), "my title");
        assert_eq!(pane.input.drain_all(), b"\x1b[1;6R");
        pane.resize(3, 10);
        assert_eq!(pane.size, (3, 10));
        assert_eq!(pane.screen().size(), (3, 10));
        Ok(())
    }
}

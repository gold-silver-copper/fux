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
    /// Input was refused, and the program has not read since: everything
    /// is refused, so none arrives with a hole before it.
    refusing: bool,
}

impl InputQueue {
    /// Queues `bytes`, or refuses them whole if the queue is full, and then
    /// everything until the program reads.
    pub fn push(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        let cost = bytes
            .len()
            .checked_add(ENTRY_COST)
            .and_then(|cost| self.cost.checked_add(cost))
            .filter(|cost| *cost <= INPUT_BYTES && !self.refusing);
        let Some(cost) = cost else {
            self.refusing = true;
            return Err(
                "the pane's program is not reading its input; nothing more is queued until it does"
                    .into(),
            );
        };
        self.cost = cost;
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
    /// `n` bytes of the front were written: the program is reading.
    pub fn advance(&mut self, n: usize) {
        if n > 0 {
            self.refusing = false;
        }
        // At most the front's length; past it all the same means done.
        self.written = self.written.saturating_add(n);
        if let Some(front) = self.pieces.front()
            && self.written >= front.len()
        {
            // What `push` added for it, which fitted.
            self.cost = self
                .cost
                .saturating_sub(front.len().saturating_add(ENTRY_COST));
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
        if self
            .replies
            .len()
            .checked_add(bytes.len())
            .is_some_and(|len| len <= 4096)
        {
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
    /// A command line waiting to be typed into the shell.
    pub typed: Option<Typed>,
}

/// After the shell's output has been quiet this long, it is taken to be
/// waiting at its prompt. Output within one burst -- a prompt drawn in
/// pieces, lines of a startup message -- comes much closer together than
/// this, while a person notices nothing shorter.
pub const QUIET: std::time::Duration = std::time::Duration::from_millis(50);

/// A command line held until the new shell is ready for it: until its output
/// has been quiet for `QUIET` after it first wrote, or at `deadline` if it
/// writes nothing. Typed earlier, the terminal would echo the line before the
/// shell had drawn its prompt, and a shell whose startup writes and then
/// discards pending input would lose it.
pub struct Typed {
    pub line: Vec<u8>,
    pub deadline: std::time::Instant,
    /// When the shell last wrote, once it has.
    pub last_output: Option<std::time::Instant>,
}

impl Typed {
    /// The moment the line is to be typed, as things stand.
    pub fn due_at(&self) -> std::time::Instant {
        match self.last_output {
            Some(at) => at
                .checked_add(QUIET)
                .map_or(self.deadline, |quiet| quiet.min(self.deadline)),
            None => self.deadline,
        }
    }
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
            typed: None,
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
        if let Some(typed) = &mut self.typed {
            typed.last_output = Some(std::time::Instant::now());
        }
        if !replies.is_empty() && self.input.push(replies).is_err() && !self.reply_dropped {
            self.reply_dropped = true;
            return true;
        }
        false
    }

    /// Types a held command line into the shell now.
    pub fn type_now(&mut self) {
        if let Some(typed) = self.typed.take() {
            // The queue is empty this early, so the line fits.
            let _ = self.input.push(typed.line);
        }
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

    /// Once input is refused, nothing more is queued until the program
    /// reads, as the notice says: accepting a later key would deliver it
    /// with a hole where the refused input was.
    #[test]
    fn after_a_refusal_nothing_is_queued_until_the_program_reads() {
        let mut queue = InputQueue::default();
        while queue.push(vec![b'p'; MAX_INPUT]).is_ok() {}
        let refusal = queue.push(vec![b'k']).err().unwrap_or_default();
        assert!(
            refusal.contains("not reading"),
            "a key after a refused paste: {refusal:?}"
        );
        // Writing some of the front is the program reading: input is taken
        // again.
        queue.advance(1);
        assert!(queue.push(vec![b'k']).is_ok());
    }

    #[test]
    fn modes_follow_output_split_anywhere() {
        let stream = b"x\x1b[?1004;2004hy\x1b[5 qz\x1b[?25l";
        for split in 0..stream.len() {
            let (a, b) = stream.split_at_checked(split).unwrap_or((stream, &[]));
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
    fn a_typed_line_waits_for_quiet_output_or_the_deadline() {
        let t0 = std::time::Instant::now();
        let ms = std::time::Duration::from_millis;
        let mut typed = Typed {
            line: b"echo hi\r".to_vec(),
            deadline: t0 + ms(1000),
            last_output: None,
        };
        // Nothing written yet: only the deadline.
        assert_eq!(typed.due_at(), t0 + ms(1000));
        // Output keeps pushing it back while it keeps coming within QUIET.
        typed.last_output = Some(t0 + ms(100));
        assert_eq!(typed.due_at(), t0 + ms(150));
        typed.last_output = Some(t0 + ms(140));
        assert_eq!(typed.due_at(), t0 + ms(190));
        // Never past the deadline, however long the output lasts.
        typed.last_output = Some(t0 + ms(990));
        assert_eq!(typed.due_at(), t0 + ms(1000));
    }

    #[test]
    fn output_records_when_the_shell_wrote_and_types_nothing_itself() -> Result<(), String> {
        let mut pane = Pane::new(PaneId(1), "sh".into(), "/bin/sh".into(), 5, 20, 10)?;
        let before = std::time::Instant::now();
        pane.typed = Some(Typed {
            line: b"x\r".to_vec(),
            deadline: before + std::time::Duration::from_secs(1),
            last_output: None,
        });
        pane.output(b"$ ");
        assert!(
            pane.typed
                .as_ref()
                .is_some_and(|t| t.last_output.is_some_and(|at| at >= before))
        );
        assert!(pane.input.is_empty(), "output alone types nothing");
        pane.type_now();
        assert_eq!(pane.input.drain_all(), b"x\r");
        assert!(pane.typed.is_none());
        Ok(())
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

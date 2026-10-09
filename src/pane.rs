//! A pane: its terminal emulator, its process, and the input waiting for it.
use crate::bytes::ByteQueue;
use crate::id::PaneId;
use crate::process::Child;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The largest single piece of input: a whole paste and its envelope.
pub const MAX_INPUT: usize = crate::decode::PASTE_LIMIT + 12;
/// Input may wait for the program up to this many bytes, however many
/// pieces it came in (bevy-final finding 021).
pub const INPUT_BYTES: usize = 16 * MAX_INPUT;
/// What one queued piece costs beyond its bytes.
pub const ENTRY_COST: usize = 64;
/// How long a frame drawn in synchronized output is held before it is
/// shown anyway: Ghostty's choice. Long enough for a frame sent in pieces
/// over a slow link; a program that dies mid-frame freezes its pane this
/// long, once. The spec leaves it open
/// (`references/modern/mode_2026_synchronized_output.md`, "Timeout").
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(1);
/// The most bytes a held frame keeps before it is shown anyway:
/// alacritty's bound.
pub const FRAME_LIMIT: usize = 2 << 20;
/// End synchronized update (ESU). Its beginning, BSU, the parser finds.
const ESU: &[u8] = b"\x1b[?2026l";

/// Why input is not queued, or a pane cannot be made.
#[derive(Debug)]
pub enum Error {
    /// The queue is full, or refusing until the program reads.
    NotReading,
    /// No terminal of this size and history.
    Terminal {
        rows: u16,
        cols: u16,
        history: usize,
        source: fux_vt::Error,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotReading => f.write_str(
                "the pane's program is not reading its input; nothing more is queued until it does",
            ),
            Error::Terminal {
                rows,
                cols,
                history,
                source,
            } => write!(
                f,
                "a {rows}x{cols} terminal with {history} lines of history: {source}"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::NotReading => None,
            Error::Terminal { source, .. } => Some(source),
        }
    }
}

/// Input and terminal replies waiting for the pane's program to read them,
/// bounded by what they cost rather than by how many pieces they came in.
/// The pieces wait end to end, to be written together.
#[derive(Default)]
pub struct InputQueue {
    bytes: ByteQueue,
    /// The length of each piece, the first of them whole.
    pieces: VecDeque<usize>,
    /// Bytes of the first piece already written.
    written: usize,
    cost: usize,
    /// Input was refused, and the program has not read since: everything
    /// is refused, so none arrives with a hole before it.
    refusing: bool,
}

impl InputQueue {
    /// Queues `bytes` as a piece, or refuses them whole if the queue is
    /// full, and then everything until the program reads.
    pub fn push(&mut self, bytes: impl AsRef<[u8]>) -> Result<(), Error> {
        self.push_with(|out| out.extend_from_slice(bytes.as_ref()))
    }
    /// Queues what `write` appends to its vector as a piece, in place, or
    /// takes it back and refuses it whole, as `push` does.
    pub fn push_with(&mut self, write: impl FnOnce(&mut Vec<u8>)) -> Result<(), Error> {
        let (queued, refusing) = (self.cost, self.refusing);
        let added = self.bytes.push_with(|out| {
            let start = out.len();
            write(out);
            let added = out.len().saturating_sub(start);
            let cost = added
                .checked_add(ENTRY_COST)
                .and_then(|cost| queued.checked_add(cost))
                .filter(|cost| *cost <= INPUT_BYTES && !refusing);
            if added > 0 && cost.is_none() {
                out.truncate(start);
            }
            cost.map(|cost| (added, cost)).ok_or(added)
        });
        match added {
            Ok((0, _)) | Err(0) => Ok(()),
            Ok((added, cost)) => {
                self.cost = cost;
                self.pieces.push_back(added);
                Ok(())
            }
            Err(_) => {
                self.refusing = true;
                Err(Error::NotReading)
            }
        }
    }
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }
    /// Whether input is refused until the program reads.
    pub fn refusing(&self) -> bool {
        self.refusing
    }
    /// The bytes to write next: every piece queued.
    pub fn front(&self) -> Option<&[u8]> {
        (!self.bytes.is_empty()).then(|| self.bytes.as_slice())
    }
    /// `n` bytes of the front were written: the program is reading.
    pub fn advance(&mut self, n: usize) {
        if n > 0 {
            self.refusing = false;
        }
        // At most what is queued.
        let mut n = n.min(self.bytes.len());
        self.bytes.take(n);
        while let Some(&front) = self.pieces.front() {
            let rest = front.saturating_sub(self.written);
            if n < rest {
                self.written = self.written.saturating_add(n);
                break;
            }
            n = n.saturating_sub(rest);
            // What `push` added for it, which fitted.
            self.cost = self.cost.saturating_sub(front.saturating_add(ENTRY_COST));
            self.pieces.pop_front();
            self.written = 0;
        }
    }
    /// Everything queued, for tests and for a pane with no process.
    pub fn drain_all(&mut self) -> Vec<u8> {
        let out = self.bytes.as_slice().to_vec();
        self.advance(out.len());
        out
    }
}

/// Where `needle` first ends in `hay`, looking from `from`.
fn end_of(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let mut i = from;
    loop {
        let start = i.checked_add(hay.get(i..)?.iter().position(|&b| b == 0x1b)?)?;
        let end = start.checked_add(needle.len())?;
        // Past the end of `hay`: no later match fits either.
        if hay.get(start..end)? == needle {
            return Some(end);
        }
        i = start.checked_add(1)?;
    }
}

/// What fux tells programs it is: XTVERSION answers `fux` and its version,
/// DA2 the version (`CSI > 1 ; Pv ; 0 c`), and DA1 a VT220-class terminal
/// (`CSI ? 62 ; 22 c`). The user's decision, on the corpus's evidence:
/// told it is fux, Claude Code asks for and uses synchronized output.
pub const IDENTITY: fux_vt::Identity = fux_vt::Identity {
    name: "fux",
    version: env!("CARGO_PKG_VERSION"),
};

/// How every pane's terminal is set up: events (titles, colour queries),
/// DECRQM (programs ask it whether synchronized output is known before
/// they use it), in-band resize, the size query, colour-scheme reports, the kitty
/// keyboard protocol (fux encodes keys as each pane asks), hyperlinks,
/// prompt marks, DECRQSS (neovim asks it whether the terminal keeps
/// underline styles: a pane keeps them, and each client is painted them as
/// far as its terminal draws them, `render::sgr`), the palette (a pane's
/// program sets and asks its colours, OSC 4, 10 to 19 and the rest, and
/// is drawn in them, `render::pane_colours`, without its client's palette
/// changing), reflow (a resized pane's
/// lines re-wrap at its new width, its history with them, as in the
/// terminals fux runs in) and fux's identity.
pub const OPTIONS: fux_vt::Options = fux_vt::Options::new()
    .with_events(true)
    .with_mode_reports(true)
    .with_in_band_resize(true)
    .with_size_reports(true)
    .with_color_scheme_updates(true)
    .with_kitty_keyboard(true)
    .with_hyperlinks(true)
    .with_prompt_marks(true)
    .with_setting_reports(true)
    .with_palette(true)
    .with_reflow(true)
    .with_identity(Some(IDENTITY));

/// The most titles a pane's program can push (`CSI 22 t`): xterm's bound.
const TITLE_STACK: usize = 10;

/// What a program did to its title, in order: set it, push it, pop it.
enum TitleOp {
    Set(String),
    Push,
    Pop,
}

/// Replies (DSR, DA) and events the parser produces while reading output.
struct Sink<'a> {
    replies: &'a mut Vec<u8>,
    titles: &'a mut Vec<TitleOp>,
    /// Set by a bell (BEL).
    bell: &'a mut bool,
    /// What colour queries are answered with (`outer`).
    colours: &'a crate::outer::Colours,
}

impl Sink<'_> {
    /// A colour query (OSC 10, 11), answered if the colour is known.
    fn colour_query(&mut self, number: u8, bel: bool) {
        if let Some(answer) = self.colours.answer(number, bel) {
            fux_vt::Sink::reply(self, &answer);
        }
    }

    /// `CSI ? 996 n`, the colour scheme asked for: answered if known.
    fn scheme_query(&mut self, sequence: &fux_vt::Unhandled<'_>) {
        if let fux_vt::Unhandled::Csi {
            params,
            intermediates: b"?",
            action: b'n',
        } = sequence
            && params.groups().eq([&[996][..]])
            && let Some(scheme) = self.colours.scheme
        {
            fux_vt::Sink::reply(self, scheme.report());
        }
    }
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
        match event {
            fux_vt::Event::Title(title) => {
                let text: String = String::from_utf8_lossy(title)
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(256)
                    .collect();
                self.titles.push(TitleOp::Set(text));
            }
            fux_vt::Event::ColorQuery { number, bel } => self.colour_query(number, bel),
            fux_vt::Event::Bell => *self.bell = true,
            // fux's clipboard policy: a program's OSC 52 is not taken.
            fux_vt::Event::IconName(_) | fux_vt::Event::Clipboard { .. } | _ => {}
        }
    }
    /// xterm's title stack (ctlseqs, window manipulation): `CSI 22 ; Ps t`
    /// pushes the title, `CSI 23 ; Ps t` pops it, for Ps 0 (icon and
    /// title, which are one here) or 2 (title); Ps 1, the icon alone, is
    /// not a title. vim and tmux push on starting and pop on leaving. And
    /// `CSI ? 996 n`, the colour scheme asked for.
    fn unhandled(&mut self, sequence: fux_vt::Unhandled<'_>) {
        self.scheme_query(&sequence);
        let fux_vt::Unhandled::Csi {
            params,
            intermediates: b"",
            action: b't',
        } = sequence
        else {
            return;
        };
        let mut groups = params.groups();
        let op = match groups.next() {
            Some([22]) => TitleOp::Push,
            Some([23]) => TitleOp::Pop,
            _ => return,
        };
        if matches!(groups.next(), None | Some([] | [0] | [2])) {
            self.titles.push(op);
        }
    }
}

pub struct Pane {
    pub id: PaneId,
    pub name: String,
    /// Set by the program with OSC 0 or 2.
    pub title: String,
    pub parser: fux_vt::Parser,
    /// The PTY size, the smallest rectangle any client shows the pane in.
    pub size: (u16, u16),
    pub child: Option<Child>,
    pub input: InputQueue,
    /// Whether a reply was dropped because the queue was full; noticed once.
    pub reply_dropped: bool,
    /// Whether the PTY hung up while the program lives on: it closed the
    /// terminal but has not exited. Its master reports the end on every
    /// poll, so it is no longer polled; its exit, by SIGCHLD, ends the pane.
    pub hung_up: bool,
    /// The shell's program the pane was started with. fux reads it nowhere
    /// itself; a typed command is quoted for the shell before the pane is
    /// made (`Session::new_pane`).
    pub shell: String,
    /// A command line waiting to be typed into the shell.
    pub typed: Option<Typed>,
    /// Titles the program pushed (`CSI 22 t`), to pop (`CSI 23 t`).
    title_stack: VecDeque<String>,
    /// The output of a frame the program is drawing in synchronized output,
    /// from BSU on, and since when; see [`Pane::output`].
    frame: Option<(Vec<u8>, Instant)>,
    /// What the program's colour queries are answered with, as the session
    /// finds them before each read of output (`outer`).
    pub colours: crate::outer::Colours,
    /// The program rang the bell since the session last looked
    /// (`Session::ring`).
    pub bell: bool,
    /// The palette entries 0 to 15 its client's terminal said, given to the
    /// parser to answer a program's `OSC 4 ; n ; ?` with.
    host_palette: crate::outer::Palette,
}

/// After the shell's output has been quiet this long, it is taken to be
/// waiting at its prompt. Output within one burst -- a prompt drawn in
/// pieces, lines of a startup message -- comes much closer together than
/// this, while a person notices nothing shorter.
pub const QUIET: Duration = Duration::from_millis(50);

/// A command line held until the new shell is ready for it: until its output
/// has been quiet for `QUIET` after it first wrote, or at `deadline` if it
/// writes nothing. Typed earlier, the terminal would echo the line before the
/// shell had drawn its prompt, and a shell whose startup writes and then
/// discards pending input would lose it.
pub struct Typed {
    pub line: Vec<u8>,
    pub deadline: Instant,
    /// When the shell last wrote, once it has.
    pub last_output: Option<Instant>,
}

impl Typed {
    /// The moment the line is to be typed, as things stand.
    pub fn due_at(&self) -> Instant {
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
    ) -> Result<Pane, Error> {
        let parser = fux_vt::Parser::with_options(rows.max(1), cols.max(1), history, OPTIONS)
            .map_err(|source| Error::Terminal {
                rows,
                cols,
                history,
                source,
            })?;
        Ok(Pane {
            id,
            name,
            title: String::new(),
            parser,
            size: (rows.max(1), cols.max(1)),
            child: None,
            input: InputQueue::default(),
            reply_dropped: false,
            hung_up: false,
            shell,
            typed: None,
            frame: None,
            title_stack: VecDeque::new(),
            colours: crate::outer::Colours::default(),
            bell: false,
            host_palette: crate::outer::Palette::default(),
        })
    }

    /// Reads program output into the screen. Returns whether a reply had to
    /// be dropped because the program is not reading its input, the first
    /// time one is in the pane's life (`reply_dropped`).
    ///
    /// A frame the program draws in synchronized output (from `CSI ? 2026 h`,
    /// BSU, to `CSI ? 2026 l`, ESU) is held, unread, until it ends, then read
    /// whole, as alacritty does: the screen is always at the end of a
    /// frame, so a client is never painted half of one. A frame is read
    /// anyway once [`FRAME_TIMEOUT`] passes ([`Pane::release_frame`]) or it
    /// grows past [`FRAME_LIMIT`]; after that the program's output is read as
    /// it comes until its next BSU.
    pub fn output(&mut self, bytes: &[u8]) -> bool {
        self.output_at(bytes, Instant::now())
    }

    /// [`Pane::output`] at `now`.
    pub fn output_at(&mut self, bytes: &[u8], now: Instant) -> bool {
        let mut dropped = false;
        let mut after: Vec<u8>;
        let mut rest = bytes;
        loop {
            if let Some((held, _)) = &mut self.frame {
                // An ESU may have begun at the end of what is held.
                let from = held.len().saturating_sub(ESU.len().saturating_sub(1));
                held.extend_from_slice(rest);
                let Some(end) = end_of(held, from, ESU) else {
                    if held.len() > FRAME_LIMIT {
                        dropped |= self.release_frame();
                    }
                    return dropped;
                };
                let mut frame = std::mem::take(held);
                self.frame = None;
                after = frame.get(end..).unwrap_or_default().to_vec();
                frame.truncate(end);
                dropped |= self.feed(&frame, false).0;
                rest = &after;
            } else {
                let (dropped_now, begun) = self.feed(rest, true);
                dropped |= dropped_now;
                let Some(end) = begun else {
                    return dropped;
                };
                self.frame = Some((Vec::new(), now));
                rest = rest.get(end..).unwrap_or_default();
            }
        }
    }

    /// When the held frame is read anyway, if one is held.
    pub fn frame_deadline(&self) -> Option<Instant> {
        self.frame
            .as_ref()
            .map(|(_, since)| crate::after(*since, FRAME_TIMEOUT))
    }

    /// Reads the held frame now, ended or not. Returns whether a reply had to
    /// be dropped.
    pub fn release_frame(&mut self) -> bool {
        match self.frame.take() {
            Some((held, _)) => self.feed(&held, false).0,
            None => false,
        }
    }

    /// Gives `bytes` to the screen, stopping after a BSU if `until_frame`.
    /// Returns whether a reply had to be dropped, and how many bytes were
    /// read if it stopped.
    fn feed(&mut self, bytes: &[u8], until_frame: bool) -> (bool, Option<usize>) {
        let mut replies = Vec::new();
        let mut titles = Vec::new();
        let mut sink = Sink {
            replies: &mut replies,
            titles: &mut titles,
            bell: &mut self.bell,
            colours: &self.colours,
        };
        // The parser refuses only allocations beyond its limits; the screen
        // stays as it was and output continues.
        // Only `process_until_frame`, even to read past a BSU: with one way
        // in, the parser's loop is compiled once and its byte handling is
        // inlined into it, as it was before frames were held; with two it
        // was not, and escape-heavy output was a quarter slower.
        let mut begun = None;
        let mut rest = bytes;
        while let Ok(Some(taken)) = self.parser.process_until_frame(rest, &mut sink) {
            if until_frame {
                begun = Some(taken);
                break;
            }
            rest = rest.get(taken..).unwrap_or_default();
        }
        for op in titles {
            match op {
                TitleOp::Set(title) => self.title = title,
                TitleOp::Push => {
                    if self.title_stack.len() >= TITLE_STACK {
                        self.title_stack.pop_front();
                    }
                    self.title_stack.push_back(self.title.clone());
                }
                TitleOp::Pop => {
                    if let Some(title) = self.title_stack.pop_back() {
                        self.title = title;
                    }
                }
            }
        }
        if let Some(typed) = &mut self.typed {
            typed.last_output = Some(Instant::now());
        }
        if !replies.is_empty() && self.input.push(replies).is_err() && !self.reply_dropped {
            self.reply_dropped = true;
            return (true, begun);
        }
        (false, begun)
    }

    /// Types a held command line into the shell now.
    pub fn type_now(&mut self) {
        if let Some(typed) = self.typed.take() {
            // The queue is empty this early, so the line fits.
            let _ = self.input.push(typed.line);
        }
    }

    /// Resizes the screen and the PTY. A held frame is read first: it was
    /// drawn for the old size.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if self.size == (rows, cols) {
            return;
        }
        self.release_frame();
        if self.parser.resize(rows, cols).is_ok() {
            self.size = (rows, cols);
            if let Some(child) = &self.child {
                crate::process::resize(&child.master, rows, cols);
            }
            // In-band resize: the report follows the PTY's new size, never
            // precedes it (references/modern/mode_2048_in_band_resize.md).
            // A program that does not read its input loses it, as a reply.
            if let Some(report) = self.parser.resize_report() {
                let _ = self.input.push(report);
            }
        }
    }

    /// Gives the parser `palette` as the host's colours for entries 0 to
    /// 15, replacing the last whole: an entry it lacks is cleared, so a
    /// program asking it gets xterm's default. The program's own colours
    /// still win, and nothing drawn changes (`fux_vt::Parser::set_host_color`).
    pub fn set_host_palette(&mut self, palette: crate::outer::Palette) {
        if self.host_palette == palette {
            return;
        }
        // A channel's top byte: OSC 4 answers in eight bits a channel.
        let byte = |c: u16| u8::try_from(c >> 8).unwrap_or(u8::MAX);
        for (index, rgb) in (0u8..).zip(palette.0) {
            let rgb = rgb.map(|c| (byte(c.r), byte(c.g), byte(c.b)));
            self.parser.set_host_color(index, rgb);
        }
        self.host_palette = palette;
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
        assert!(partial.push(b"abc").is_ok());
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
        let refusal = queue.push(vec![b'k']);
        assert!(
            matches!(refusal, Err(Error::NotReading)),
            "a key after a refused paste: {refusal:?}"
        );
        assert!(refusal.is_err_and(|e| e.to_string().contains("not reading")));
        // Writing some of the front is the program reading: input is taken
        // again.
        queue.advance(1);
        assert!(queue.push(vec![b'k']).is_ok());
    }

    /// The screen follows the modes fux sends on, focus reporting and the
    /// cursor's shape, from output split anywhere.
    #[test]
    fn modes_follow_output_split_anywhere() -> Result<(), Error> {
        let stream = b"x\x1b[?1004;2004hy\x1b[5 qz\x1b[?25l";
        for split in 0..stream.len() {
            let (a, b) = stream.split_at_checked(split).unwrap_or((stream, &[]));
            let mut pane = Pane::new(PaneId::of(1), "sh".into(), "/bin/sh".into(), 5, 20, 10)?;
            pane.output(a);
            pane.output(b);
            assert!(pane.screen().focus_reporting(), "split {split}");
            assert_eq!(pane.screen().cursor_shape(), 5, "split {split}");
        }
        let mut pane = Pane::new(PaneId::of(1), "sh".into(), "/bin/sh".into(), 5, 20, 10)?;
        pane.output(b"\x1b[?1004h\x1b[?1004l\x1b[2 q\x1b[ q");
        assert!(!pane.screen().focus_reporting());
        assert_eq!(pane.screen().cursor_shape(), 0);
        pane.output(b"\x1b[?1004h\x1bc");
        assert!(!pane.screen().focus_reporting());
        Ok(())
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
    fn output_records_when_the_shell_wrote_and_types_nothing_itself() -> Result<(), Error> {
        let mut pane = Pane::new(PaneId::of(1), "sh".into(), "/bin/sh".into(), 5, 20, 10)?;
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
    fn output_answers_queries_and_takes_titles() -> Result<(), Error> {
        let mut pane = Pane::new(PaneId::of(1), "sh".into(), "/bin/sh".into(), 5, 20, 10)?;
        pane.output(b"hello\x1b]2;my title\x07\x1b[6n");
        assert_eq!(pane.title, "my title");
        assert_eq!(pane.label(), "my title");
        assert_eq!(pane.input.drain_all(), b"\x1b[1;6R");
        pane.resize(3, 10);
        assert_eq!(pane.size, (3, 10));
        assert_eq!(pane.screen().size(), (3, 10));
        Ok(())
    }

    /// A program's colour queries are answered with the colours the session
    /// set, in the form asked (ctlseqs: `OSC 11 ; rgb:RRRR/GGGG/BBBB`, BEL or
    /// ST as the query ended), in order with the other replies, so that
    /// DA1 sent after a query as a sentinel comes after its answer; so is
    /// `CSI ? 996 n`, with the scheme. What is not known is not answered.
    #[test]
    fn colour_queries_are_answered_from_the_session_s_colours() -> Result<(), Error> {
        use crate::outer::{Colours, Rgb, Scheme};
        let mut pane = pane()?;
        pane.output(b"\x1b]11;?\x07\x1b]10;?\x1b\\\x1b[?996n\x1b[c");
        assert_eq!(pane.input.drain_all(), b"\x1b[?62;22c", "nothing known");
        pane.colours = Colours {
            foreground: Some(Rgb {
                r: 0xc0c0,
                g: 0xc0c0,
                b: 0xc0c0,
            }),
            background: Some(Rgb {
                r: 0,
                g: 0x1010,
                b: 0xffff,
            }),
            scheme: Some(Scheme::Light),
        };
        pane.output(b"\x1b]11;?\x07\x1b]10;?\x1b\\\x1b[?996n\x1b[c\x1b]12;?\x07");
        assert_eq!(
            pane.input.drain_all(),
            b"\x1b]11;rgb:0000/1010/ffff\x07\x1b]10;rgb:c0c0/c0c0/c0c0\x1b\\\x1b[?997;2n\x1b[?62;22c"
        );
        // DECRQM knows mode 2031, which the program sets.
        pane.output(b"\x1b[?2031h\x1b[?2031$p");
        assert!(pane.screen().color_scheme_updates());
        assert_eq!(pane.input.drain_all(), b"\x1b[?2031;1$y");
        Ok(())
    }

    /// The first row's text, blanks as spaces, trimmed.
    fn first_row(pane: &Pane) -> String {
        let (_, cols) = pane.screen().size();
        (0..cols)
            .filter_map(|x| pane.screen().cell(0, x))
            .map(|c| c.contents().chars().next().unwrap_or(' '))
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn pane() -> Result<Pane, Error> {
        Pane::new(PaneId::of(1), "sh".into(), "/bin/sh".into(), 3, 30, 10)
    }

    /// A frame drawn in synchronized output reaches the screen whole, when
    /// it ends, however its bytes are split across reads: the BSU, the
    /// frame and the ESU each in pieces, and several frames in one read.
    #[test]
    fn a_synchronized_frame_reaches_the_screen_whole() -> Result<(), Error> {
        let mut pane = pane()?;
        pane.output(b"a\x1b[?2026hb");
        assert_eq!(first_row(&pane), "a", "the frame is held");
        assert!(pane.screen().synchronized_output());
        pane.output(b"c\x1b[?20");
        assert_eq!(first_row(&pane), "a", "an ESU begun is not one");
        pane.output(b"26ld");
        assert_eq!(first_row(&pane), "abcd");
        assert!(!pane.screen().synchronized_output());
        // A BSU split across reads.
        pane.output(b"e\x1b[?2");
        pane.output(b"026hf");
        assert_eq!(first_row(&pane), "abcde");
        pane.output(b"\x1b[?2026l");
        assert_eq!(first_row(&pane), "abcdef");
        // Frames back to back in one read: the last is held.
        pane.output(b"\x1b[?2026hg\x1b[?2026l\x1b[?2026hh\x1b[?2026l\x1b[?2026hi");
        assert_eq!(first_row(&pane), "abcdefgh");
        assert!(pane.frame_deadline().is_some());
        Ok(())
    }

    /// A frame that does not end is read once its timeout passes, or once
    /// it outgrows the limit; until the next BSU, output is then read as it
    /// comes. A resize reads it first.
    #[test]
    fn a_frame_that_does_not_end_is_read_anyway() -> Result<(), Error> {
        let mut pane = pane()?;
        let t0 = Instant::now();
        pane.output_at(b"\x1b[?2026hx", t0);
        assert_eq!(pane.frame_deadline(), Some(crate::after(t0, FRAME_TIMEOUT)));
        assert!(!pane.release_frame(), "no reply dropped");
        assert_eq!(first_row(&pane), "x");
        assert_eq!(pane.frame_deadline(), None);
        pane.output(b"y");
        assert_eq!(
            first_row(&pane),
            "xy",
            "read as it comes, the mode still set"
        );
        pane.output(b"\x1b[?2026l\x1b[?2026h");
        let big = vec![b'z'; FRAME_LIMIT];
        pane.output(&big);
        assert_eq!(first_row(&pane), "xy", "at the limit, still held");
        pane.output(b"z");
        assert!(first_row(&pane).ends_with('z'), "past it, read");
        assert_eq!(pane.frame_deadline(), None);
        pane.output(b"\x1b[2J\x1b[H\x1b[?2026hw");
        pane.resize(3, 20);
        // The cursor's row: a reflow may bring history down above it.
        let (y, _) = pane.screen().cursor_position();
        let row: String = (0..20)
            .filter_map(|x| pane.screen().cell(y, x))
            .map(|c| c.contents().chars().next().unwrap_or(' '))
            .collect();
        assert_eq!(row.trim_end(), "w", "a resize reads it first");
        Ok(())
    }

    /// A program that set in-band resize is told its new size after a
    /// resize, and on setting it; one that did not is told nothing.
    #[test]
    fn a_resize_reports_the_size_in_band_when_asked() -> Result<(), Error> {
        let mut pane = pane()?;
        pane.resize(4, 20);
        assert!(pane.input.drain_all().is_empty(), "not asked");
        pane.output(b"\x1b[?2048h");
        assert_eq!(pane.input.drain_all(), b"\x1b[48;4;20;0;0t");
        pane.resize(6, 25);
        assert_eq!(pane.input.drain_all(), b"\x1b[48;6;25;0;0t");
        pane.resize(6, 25);
        assert!(pane.input.drain_all().is_empty(), "the same size");
        Ok(())
    }

    /// xterm's title stack: vim and tmux push the title on starting and pop
    /// it on leaving, so the pane's title comes back. Ps 1 (the icon) is no
    /// title; the stack keeps at most ten; a pop with none pushed changes
    /// nothing.
    #[test]
    fn a_pushed_title_comes_back_when_popped() -> Result<(), Error> {
        let mut pane = pane()?;
        pane.output(b"\x1b]2;shell\x07\x1b[22;0t\x1b]2;vim\x07");
        assert_eq!(pane.title, "vim");
        pane.output(b"\x1b[23;0t");
        assert_eq!(pane.title, "shell");
        pane.output(b"\x1b[22;2t\x1b]2;tmux\x07\x1b[23;2t\x1b[23;0t");
        assert_eq!(pane.title, "shell", "a pop with none left changes nothing");
        pane.output(b"\x1b[22;1t\x1b]2;other\x07\x1b[23;1t");
        assert_eq!(pane.title, "other", "the icon's stack is not the title's");
        for n in 0..12 {
            pane.output(format!("\x1b]2;t{n}\x07\x1b[22t").as_bytes());
        }
        for _ in 0..12 {
            pane.output(b"\x1b[23t");
        }
        assert_eq!(pane.title, "t2", "ten kept: the first two pushes dropped");
        Ok(())
    }

    /// neovim's handshake for underline styles (neovim 0.12.5,
    /// `tui_query_extended_underline`): it sets a curly underline and asks
    /// for the pen with DECRQSS, and draws its diagnostics curly, with
    /// their colour (`58:2::r:g:b`), only if `4:3` comes back. A pane keeps
    /// the style and says so; the cells keep what neovim then draws.
    #[test]
    fn a_pane_tells_neovim_it_keeps_underline_styles() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane()?;
        pane.output(b"\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\");
        assert_eq!(pane.input.drain_all(), b"\x1bP1$r0;4:3m\x1b\\");
        pane.output(b"\x1b[0m\x1b[4:3m\x1b[58:2::255:0:0mx\x1b[0m");
        let cell = pane.screen().cell(0, 0).ok_or("no cell")?;
        assert_eq!(cell.underline_style(), fux_vt::UnderlineStyle::Curly);
        assert_eq!(cell.underline_color(), fux_vt::Color::Rgb(255, 0, 0));
        Ok(())
    }

    #[test]
    fn end_of_finds_a_sequence_from_where_it_is_told() {
        assert_eq!(end_of(b"ab\x1b[?2026lc", 0, ESU), Some(10));
        assert_eq!(end_of(b"\x1b\x1b[?2026l", 0, ESU), Some(9));
        assert_eq!(end_of(b"\x1b[?2026l", 1, ESU), None);
        assert_eq!(end_of(b"\x1b[?2026", 0, ESU), None);
        assert_eq!(end_of(b"", 0, ESU), None);
    }
}

//! A pane: its terminal emulator, its process, and the input waiting for it.
use crate::bytes::ByteQueue;
use crate::id::PaneId;
use crate::process::Child;
use fux_vt::Feature;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The largest single piece of input: a whole paste and its envelope.
pub const MAX_INPUT: usize = crate::decode::PASTE_LIMIT + 12;
/// Input may wait for the program up to this many bytes, however many
/// pieces it came in (bevy-final finding 021).
pub const INPUT_BYTES: usize = 16 * MAX_INPUT;
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
        size: fux_vt::Size,
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
                size,
                history,
                source,
            } => write!(
                f,
                "a {}x{} terminal with {history} lines of history: {source}",
                size.rows(),
                size.cols()
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
/// end to end, to be written together: at most [`INPUT_BYTES`] of them,
/// however many pieces they came in, with room kept for a command line
/// held until the shell is ready, which is then typed whatever else came.
#[derive(Default)]
pub struct InputQueue {
    bytes: ByteQueue,
    /// A command line held until the shell is ready.
    held: Option<Typed>,
    /// Input was refused, and the program has not read since: everything
    /// is refused, so none arrives with a hole before it.
    refusing: bool,
    lost: Lost,
}

/// Whether the program lost a reply because it was not reading, and whether
/// that was told.
#[derive(Clone, Copy, Default, PartialEq)]
enum Lost {
    #[default]
    None,
    Untold,
    Told,
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
        let room = self.room();
        let fits = self.bytes.push_with(|out| {
            let start = out.len();
            write(out);
            let fits = out.len().saturating_sub(start) <= room;
            if !fits {
                out.truncate(start);
            }
            fits
        });
        self.refusing |= !fits;
        fits.then_some(()).ok_or(Error::NotReading)
    }
    /// How many bytes more it takes now.
    pub fn room(&self) -> usize {
        let held = self.held.as_ref().map_or(0, |typed| typed.line.len());
        let used = self.bytes.len().saturating_add(held);
        if self.refusing {
            0
        } else {
            INPUT_BYTES.saturating_sub(used)
        }
    }
    /// Queues terminal replies, as `push`: a program that is not reading
    /// loses them.
    fn reply(&mut self, bytes: &[u8]) {
        if self.push(bytes).is_err() && self.lost == Lost::None {
            self.lost = Lost::Untold;
        }
    }
    /// Whether a reply was lost and is yet to be told, which it then is:
    /// once in the queue's life.
    pub fn lost_reply(&mut self) -> bool {
        let untold = self.lost == Lost::Untold;
        if untold {
            self.lost = Lost::Told;
        }
        untold
    }
    /// When the held command line is to be typed, if one is held.
    pub fn due_at(&self) -> Option<Instant> {
        self.held.as_ref().map(Typed::due_at)
    }
    /// The shell wrote at `now`: a held line waits for it to be quiet. The
    /// clock is read only while one is held.
    pub fn heard(&mut self, now: impl FnOnce() -> Instant) {
        if let Some(typed) = &mut self.held {
            typed.last_output = Some(now());
        }
    }
    /// Types the held command line now, into the room kept for it.
    pub fn type_now(&mut self) {
        if let Some(typed) = self.held.take() {
            self.bytes.push(&typed.line);
        }
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
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
        self.refusing &= n == 0;
        self.bytes.take(n);
    }
    /// Everything queued, for tests and for a pane with no process.
    pub fn drain_all(&mut self) -> Vec<u8> {
        let out = self.bytes.as_slice().to_vec();
        self.advance(out.len());
        out
    }
}

impl From<Typed> for InputQueue {
    /// A queue holding `typed` until the shell is ready.
    fn from(typed: Typed) -> InputQueue {
        InputQueue {
            held: Some(typed),
            ..InputQueue::default()
        }
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
    .with(Feature::Events)
    .with(Feature::ModeReports)
    .with(Feature::InBandResize)
    .with(Feature::SizeReports)
    .with(Feature::ColorSchemeUpdates)
    .with(Feature::KittyKeyboard)
    .with(Feature::Hyperlinks)
    .with(Feature::PromptMarks)
    .with(Feature::SettingReports)
    .with(Feature::Palette)
    .with(Feature::Reflow)
    .with_identity(Some(IDENTITY));

/// The most titles a pane's program can push (`CSI 22 t`): xterm's bound.
const TITLE_STACK: usize = 10;

/// A pane's title, as its program sets it with OSC 0 or 2, and the titles
/// it pushed (`CSI 22 t`) to pop (`CSI 23 t`), at most xterm's ten: a push
/// past them forgets the oldest.
#[derive(Default)]
pub struct Title {
    now: String,
    pushed: VecDeque<String>,
}

impl Title {
    pub fn as_str(&self) -> &str {
        &self.now
    }
    /// The title a program set, without control characters, at most 256
    /// characters of it.
    fn set(&mut self, title: &[u8]) {
        let text = String::from_utf8_lossy(title);
        self.now = text.chars().filter(|c| !c.is_control()).take(256).collect();
    }
    fn push(&mut self) {
        if self.pushed.len() >= TITLE_STACK {
            self.pushed.pop_front();
        }
        self.pushed.push_back(self.now.clone());
    }
    /// The title last pushed comes back; with none, nothing changes.
    fn pop(&mut self) {
        if let Some(title) = self.pushed.pop_back() {
            self.now = title;
        }
    }
}

/// Replies (DSR, DA) and events the parser produces while reading output.
struct Sink<'a> {
    /// The replies to one read, queued together after it: at most 4 KiB.
    replies: Vec<u8>,
    title: &'a mut Title,
    /// Set by a bell (BEL).
    bell: &'a mut bool,
    alerts: &'a mut Alerts,
    /// Set by an OSC 52 write of UTF-8 text, at most `MAX_CLIPBOARD`
    /// encoded.
    copied: &'a mut Option<String>,
    /// What colour queries are answered with (`outer`).
    colours: &'a crate::outer::Colours,
}

impl fux_vt::Sink for Sink<'_> {
    fn reply(&mut self, bytes: &[u8]) {
        if self.replies.len().saturating_add(bytes.len()) <= 4096 {
            self.replies.extend_from_slice(bytes);
        }
    }
    /// A colour query (OSC 10, 11) is answered if the colour is known.
    fn event(&mut self, event: fux_vt::Event<'_>) {
        match event {
            fux_vt::Event::Title(title) => self.title.set(title),
            fux_vt::Event::ColorQuery { number, bel } => {
                if let Some(answer) = self.colours.answer(number, bel) {
                    fux_vt::Sink::reply(self, &answer);
                }
            }
            fux_vt::Event::Bell => {
                *self.bell = true;
                self.alerts.bells = self.alerts.bells.saturating_add(1);
            }
            fux_vt::Event::Notification(text) => {
                self.alerts.notifications = self.alerts.notifications.saturating_add(1);
                let text = String::from_utf8_lossy(text);
                self.alerts.last =
                    Some(text.chars().filter(|c| !c.is_control()).take(500).collect());
            }
            fux_vt::Event::Clipboard { data, .. } if data.len() <= crate::copy::MAX_CLIPBOARD => {
                let text = crate::json::unbase64(data).and_then(|d| String::from_utf8(d).ok());
                if let Some(text) = text {
                    *self.copied = Some(text);
                }
            }
            fux_vt::Event::IconName(_) | fux_vt::Event::Clipboard { .. } | _ => {}
        }
    }
    /// xterm's title stack (ctlseqs, window manipulation): `CSI 22 ; Ps t`
    /// pushes the title, `CSI 23 ; Ps t` pops it, for Ps 0 (icon and
    /// title, which are one here) or 2 (title); Ps 1, the icon alone, is
    /// not a title. vim and tmux push on starting and pop on leaving. And
    /// `CSI ? 996 n`, the colour scheme asked for, answered if known.
    fn unhandled(&mut self, sequence: fux_vt::Unhandled<'_>) {
        let fux_vt::Unhandled::Csi {
            params,
            intermediates,
            action,
        } = sequence
        else {
            return;
        };
        let mut groups = params.groups();
        let (first, second) = (groups.next(), groups.next());
        let title = matches!(second, None | Some([] | [0] | [2]));
        match (intermediates, action, first) {
            (b"?", b'n', Some([996])) if second.is_none() => {
                if let Some(scheme) = self.colours.scheme {
                    fux_vt::Sink::reply(self, scheme.report());
                }
            }
            (b"", b't', Some([22])) if title => self.title.push(),
            (b"", b't', Some([23])) if title => self.title.pop(),
            _ => {}
        }
    }
}

/// A pane's program, by how far along its life it is. A closed pane's
/// goes on to `session::Dying`.
pub enum Process {
    /// None was started: tests of the session's state alone.
    Absent,
    /// Its terminal is polled, read and written.
    Reading(Child),
    /// It closed its terminal but has not exited. The master reports the
    /// end on every poll, so it is neither polled nor written; its exit, by
    /// SIGCHLD, ends the pane.
    HungUp(Child),
    /// The program `fux run` started exited so: the pane stays, its last
    /// screen kept to be read.
    Exited(crate::process::Ended),
}

impl Process {
    pub fn child(&self) -> Option<&Child> {
        match self {
            Process::Absent | Process::Exited(_) => None,
            Process::Reading(child) | Process::HungUp(child) => Some(child),
        }
    }

    /// How its program ended, once it has.
    pub fn exit_status(&self) -> Option<crate::process::Ended> {
        match self {
            Process::Exited(status) => Some(*status),
            Process::Absent | Process::Reading(_) | Process::HungUp(_) => None,
        }
    }

    /// The terminal's foreground process group, if it is not the
    /// program's own: a job the shell runs.
    pub fn job(&self) -> Option<crate::process::Pid> {
        let child = self.child()?;
        let group = fuxix::terminal::foreground_group(&child.master)?;
        (group != child.leader.pid()).then_some(group)
    }

    /// The program's terminal hung up: it is read no more, and waits to be
    /// reaped.
    pub fn stop_reading(&mut self) {
        *self = match std::mem::replace(self, Process::Absent) {
            Process::Reading(child) | Process::HungUp(child) => Process::HungUp(child),
            other @ (Process::Absent | Process::Exited(_)) => other,
        };
    }
}

/// A digest of what `screen` shows: its glyphs, their attributes, and the
/// cursor.
fn digest(screen: &fux_vt::Screen) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    let window = screen.window();
    for row in (0..window.rows()).filter_map(|y| window.row(y)) {
        for cell in row.cells() {
            cell.contents().hash(&mut hasher);
            cell.attributes().hash(&mut hasher);
        }
    }
    screen.cursor_position().hash(&mut hasher);
    hasher.finish()
}

/// What a pane runs, which says what its program's exit does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Runs {
    /// The shell: its exit closes the pane.
    Shell,
    /// A program `fux run` started in place of a shell: its exit leaves the
    /// pane, with its last screen and its status (`Process::Exited`); how it
    /// was started, which `fux respawn` starts again.
    Program(Launch),
}

/// How a program was started: its words, where, and the variables set for
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub argv: Vec<String>,
    pub cwd: std::path::PathBuf,
    pub env: Vec<(String, String)>,
}

/// What a program rang or announced since it started: bells, and desktop
/// notifications (OSC 9, 777, 99) with the last one's text.
#[derive(Clone, Debug, Default)]
pub struct Alerts {
    pub bells: u64,
    pub notifications: u64,
    pub last: Option<String>,
}

impl Alerts {
    /// Bells and notifications together, as `wait --bell` counts them.
    pub fn count(&self) -> u64 {
        self.bells.saturating_add(self.notifications)
    }
}

pub struct Pane {
    pub id: PaneId,
    pub name: String,
    /// Set by the program with OSC 0 or 2.
    pub title: Title,
    /// Its screen's size is the PTY's: the smallest rectangle any client
    /// shows the pane in.
    pub parser: fux_vt::Parser,
    pub process: Process,
    pub input: InputQueue,
    pub runs: Runs,
    /// The output of a frame the program is drawing in synchronized output,
    /// from BSU on, and since when; see [`Pane::output`].
    frame: Option<(Vec<u8>, Instant)>,
    /// What the program's colour queries are answered with, as the session
    /// finds them before each read of output (`outer`).
    pub colours: crate::outer::Colours,
    /// The program rang the bell since the session last looked
    /// (`Session::ring`).
    pub bell: bool,
    /// The text the program last put on the clipboard (OSC 52) since the
    /// session last looked, which copies it (`Session::copied`).
    pub copied: Option<String>,
    paced: Paced,
    /// A digest of what the screen shows (`digest`), and since when it
    /// has: a program that redraws the same screen has not changed it.
    shown: u64,
    pub changed_at: Instant,
    /// How many times what the screen shows has changed: a wait for a change
    /// past one read earlier waits for it to grow.
    pub revision: u64,
    pub alerts: Alerts,
    /// Where everything the program writes is kept too (`fux run --log`).
    pub log: Option<std::fs::File>,
}

/// How long a piece of input that must be read alone (`Paced`) is given
/// before the next: longer than a program waits to tell a lone Escape from
/// one that begins a sequence, and than a burst of input it takes as a
/// paste, as typing would leave.
pub const PACE: Duration = Duration::from_millis(30);

/// Input that `send-keys` sends in pieces a program must read apart: a lone
/// Escape, read with what follows, is Alt and that key; text and the key
/// after it, read together, are a paste whose Enter is a newline. Each such
/// piece is followed by a gap of [`PACE`] before the next is written.
#[derive(Default)]
pub struct Paced {
    /// Pieces waiting for the gap, each with whether it is read alone.
    waiting: VecDeque<(Vec<u8>, bool)>,
    /// Until when the last piece read alone is given.
    until: Option<Instant>,
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
/// discards pending input would lose it. It always fits the input queue.
pub struct Typed {
    line: Vec<u8>,
    deadline: Instant,
    /// When the shell last wrote, once it has.
    last_output: Option<Instant>,
}

impl Typed {
    /// `line`, to be typed at `deadline` at the latest, unless it is too
    /// long ever to fit the input queue.
    pub fn new(line: Vec<u8>, deadline: Instant) -> Option<Typed> {
        (line.len() <= INPUT_BYTES).then_some(Typed {
            line,
            deadline,
            last_output: None,
        })
    }

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

/// `rows` by `cols`, a zero taken as one: a pane always has a cell.
pub fn size(rows: u16, cols: u16) -> fux_vt::Size {
    let one = |n| std::num::NonZeroU16::new(n).unwrap_or(std::num::NonZeroU16::MIN);
    fux_vt::Size::from((one(rows), one(cols)))
}

impl Pane {
    pub fn new(
        id: PaneId,
        name: String,
        runs: Runs,
        size: fux_vt::Size,
        history: usize,
    ) -> Result<Pane, Error> {
        let parser = fux_vt::Parser::with_options(size, history, OPTIONS).map_err(|source| {
            Error::Terminal {
                size,
                history,
                source,
            }
        })?;
        Ok(Pane {
            id,
            name,
            title: Title::default(),
            parser,
            process: Process::Absent,
            input: InputQueue::default(),
            runs,
            frame: None,
            colours: crate::outer::Colours::default(),
            bell: false,
            copied: None,
            paced: Paced::default(),
            shown: 0,
            changed_at: Instant::now(),
            revision: 0,
            alerts: Alerts::default(),
            log: None,
        })
    }

    /// Reads program output into the screen, its replies queued as input.
    ///
    /// A frame the program draws in synchronized output (from `CSI ? 2026 h`,
    /// BSU, to `CSI ? 2026 l`, ESU) is held, unread, until it ends, then read
    /// whole, as alacritty does: the screen is always at the end of a
    /// frame, so a client is never painted half of one. A frame is read
    /// anyway once [`FRAME_TIMEOUT`] passes ([`Pane::release_frame`]) or it
    /// grows past [`FRAME_LIMIT`]; after that the program's output is read as
    /// it comes until its next BSU.
    pub fn output(&mut self, bytes: &[u8]) {
        self.output_at(bytes, Instant::now())
    }

    /// [`Pane::output`] at `now`.
    pub fn output_at(&mut self, bytes: &[u8], now: Instant) {
        if let Some(log) = &mut self.log {
            // A log that cannot be written stops; the pane goes on.
            if std::io::Write::write_all(log, bytes).is_err() {
                self.log = None;
            }
        }
        let mut after: Vec<u8>;
        let mut rest = bytes;
        loop {
            if let Some((held, _)) = &mut self.frame {
                // An ESU may have begun at the end of what is held.
                let from = held.len().saturating_sub(ESU.len().saturating_sub(1));
                held.extend_from_slice(rest);
                let Some(end) = end_of(held, from, ESU) else {
                    if held.len() > FRAME_LIMIT {
                        self.release_frame();
                    }
                    return;
                };
                let mut frame = std::mem::take(held);
                self.frame = None;
                after = frame.get(end..).unwrap_or_default().to_vec();
                frame.truncate(end);
                self.feed(&frame, false);
                rest = &after;
            } else {
                let Some(end) = self.feed(rest, true) else {
                    return;
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

    /// Reads the held frame now, ended or not.
    pub fn release_frame(&mut self) {
        if let Some((held, _)) = self.frame.take() {
            self.feed(&held, false);
        }
    }

    /// Gives `bytes` to the screen, stopping after a BSU if `until_frame`.
    /// Returns how many bytes were read if it stopped.
    fn feed(&mut self, bytes: &[u8], until_frame: bool) -> Option<usize> {
        let mut sink = Sink {
            replies: Vec::new(),
            title: &mut self.title,
            bell: &mut self.bell,
            alerts: &mut self.alerts,
            copied: &mut self.copied,
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
        let replies = sink.replies;
        if !replies.is_empty() {
            self.input.reply(&replies);
        }
        self.input.heard(Instant::now);
        begun
    }

    /// Writes the waiting input, as far as the program's terminal takes it.
    pub fn write_input(&mut self) {
        let Process::Reading(child) = &self.process else {
            return;
        };
        while let Some(bytes) = self.input.front() {
            match fuxix::io::write(&child.master, bytes) {
                Ok(0) => break,
                Ok(n) => self.input.advance(n),
                Err(fuxix::Errno::INTR) => continue,
                Err(_) => break,
            }
        }
    }

    /// Resizes the screen and the PTY. A held frame is read first: it was
    /// drawn for the old size.
    pub fn resize(&mut self, size: fux_vt::Size) {
        if self.size() == size {
            return;
        }
        self.release_frame();
        if self.parser.resize(size).is_ok() {
            if let Some(child) = self.process.child() {
                let _ = fuxix::terminal::set_window_size(&child.master, size.nonzero().into());
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
    /// still win, and nothing drawn changes (`fux_vt::Parser::set_host_palette`).
    pub fn set_host_palette(&mut self, palette: crate::outer::Palette) {
        self.parser.set_host_palette(palette);
    }

    pub fn size(&self) -> fux_vt::Size {
        self.screen().size()
    }

    pub fn screen(&self) -> &fux_vt::Screen {
        self.parser.screen()
    }

    /// The name the bar shows: the program's title, else the pane's name.
    /// Sends `pieces` of input in order, each with whether the program must
    /// read it alone (`Paced`): at once while no gap holds them, the rest
    /// when the gaps end ([`Pane::pace_due`]). Refused whole, as
    /// `InputQueue::push` refuses, if they would not fit.
    pub fn send(&mut self, pieces: Vec<(Vec<u8>, bool)>, now: Instant) -> Result<(), Error> {
        let held: usize = self.paced.waiting.iter().map(|(p, _)| p.len()).sum();
        let total = pieces
            .iter()
            .map(|(p, _)| p.len())
            .fold(held, usize::saturating_add);
        if total > self.input.room() {
            return Err(Error::NotReading);
        }
        self.paced.waiting.extend(pieces);
        self.release_paced(now);
        Ok(())
    }

    /// When paced input waits for its gap to end.
    pub fn pace_due(&self) -> Option<Instant> {
        self.paced.until.filter(|_| !self.paced.waiting.is_empty())
    }

    /// Writes the paced input whose gap has ended at `now`, up to and
    /// including the next piece read alone.
    pub fn release_paced(&mut self, now: Instant) {
        while self.paced.until.is_none_or(|until| until <= now) {
            let Some((piece, alone)) = self.paced.waiting.pop_front() else {
                self.paced.until = None;
                return;
            };
            let _ = self.input.push(&piece);
            self.paced.until = alone.then(|| crate::after(now, PACE));
        }
    }

    /// Starts `launch` in the pane again as `process`, on a screen as new,
    /// its size and history kept; the process that was there, to end.
    pub fn respawned(
        &mut self,
        launch: Launch,
        process: Option<Process>,
        history: usize,
    ) -> Result<Process, Error> {
        let size = self.size();
        let parser = fux_vt::Parser::with_options(size, history, OPTIONS).map_err(|source| {
            Error::Terminal {
                size,
                history,
                source,
            }
        })?;
        self.parser = parser;
        self.name = launch.argv.first().map_or_else(String::new, |p| {
            std::path::Path::new(p)
                .file_name()
                .map_or_else(|| p.clone(), |n| n.to_string_lossy().into_owned())
        });
        self.title = Title::default();
        self.input = InputQueue::default();
        self.frame = None;
        self.runs = Runs::Program(launch);
        self.alerts = Alerts::default();
        self.revision = self.revision.wrapping_add(1);
        self.changed_at = Instant::now();
        Ok(std::mem::replace(
            &mut self.process,
            process.unwrap_or(Process::Absent),
        ))
    }

    /// Notes, after the screen was read into at `now`, whether what it
    /// shows changed.
    pub fn note_change(&mut self, now: Instant) {
        let shown = digest(self.screen());
        if shown != self.shown {
            self.shown = shown;
            self.changed_at = now;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Whether every piece of input sent has been written to the program.
    pub fn input_written(&self) -> bool {
        self.paced.waiting.is_empty() && self.input.is_empty()
    }

    pub fn label(&self) -> &str {
        match self.title.as_str() {
            "" => &self.name,
            title => title,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fux_vt::Mode;

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
            let mut pane = pane(5, 20)?;
            pane.output(a);
            pane.output(b);
            assert!(pane.screen().mode(Mode::FocusReporting), "split {split}");
            assert_eq!(pane.screen().cursor_shape(), 5, "split {split}");
        }
        let mut pane = pane(5, 20)?;
        pane.output(b"\x1b[?1004h\x1b[?1004l\x1b[2 q\x1b[ q");
        assert!(!pane.screen().mode(Mode::FocusReporting));
        assert_eq!(pane.screen().cursor_shape(), 0);
        pane.output(b"\x1b[?1004h\x1bc");
        assert!(!pane.screen().mode(Mode::FocusReporting));
        Ok(())
    }

    #[test]
    fn a_typed_line_waits_for_quiet_output_or_the_deadline() -> Result<(), &'static str> {
        let t0 = std::time::Instant::now();
        let ms = std::time::Duration::from_millis;
        let mut typed = Typed::new(b"echo hi\r".to_vec(), t0 + ms(1000));
        let typed = typed.as_mut().ok_or("too long")?;
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
        Ok(())
    }

    #[test]
    fn output_records_when_the_shell_wrote_and_types_nothing_itself() -> Result<(), Error> {
        let mut pane = pane(5, 20)?;
        let deadline = Instant::now() + Duration::from_secs(1);
        pane.input = Typed::new(b"x\r".to_vec(), deadline)
            .map(InputQueue::from)
            .ok_or(Error::NotReading)?;
        pane.output(b"$ ");
        assert!(pane.input.due_at().is_some_and(|at| at < deadline));
        assert!(pane.input.is_empty(), "output alone types nothing");
        // Input that comes first fills the queue but for the line's room.
        while pane.input.push(vec![b'k'; MAX_INPUT]).is_ok() {}
        pane.input.type_now();
        assert!(pane.input.drain_all().ends_with(b"kx\r"));
        assert_eq!(pane.input.due_at(), None);
        Ok(())
    }

    #[test]
    fn output_answers_queries_and_takes_titles() -> Result<(), Error> {
        let mut pane = pane(5, 20)?;
        pane.output(b"hello\x1b]2;my title\x07\x1b[6n");
        assert_eq!(pane.title.as_str(), "my title");
        assert_eq!(pane.label(), "my title");
        assert_eq!(pane.input.drain_all(), b"\x1b[1;6R");
        pane.resize(size(3, 10));
        assert_eq!(<(u16, u16)>::from(pane.size()), (3, 10));
        Ok(())
    }

    /// A program's colour queries are answered with the colours the session
    /// set, in the form asked (ctlseqs: `OSC 11 ; rgb:RRRR/GGGG/BBBB`, BEL or
    /// ST as the query ended), in order with the other replies, so that
    /// DA1 sent after a query as a sentinel comes after its answer; so is
    /// `CSI ? 996 n`, with the scheme. What is not known is not answered.
    #[test]
    fn colour_queries_are_answered_from_the_session_s_colours() -> Result<(), Error> {
        use crate::outer::{Colours, Scheme};
        let mut pane = pane(3, 30)?;
        pane.output(b"\x1b]11;?\x07\x1b]10;?\x1b\\\x1b[?996n\x1b[c");
        assert_eq!(pane.input.drain_all(), b"\x1b[?62;22c", "nothing known");
        pane.colours = Colours {
            foreground: Some([0xc0; 3].into()),
            background: Some([0, 0x10, 0xff].into()),
            scheme: Some(Scheme::Light),
        };
        pane.output(b"\x1b]11;?\x07\x1b]10;?\x1b\\\x1b[?996n\x1b[c\x1b]12;?\x07");
        assert_eq!(
            pane.input.drain_all(),
            b"\x1b]11;rgb:0000/1010/ffff\x07\x1b]10;rgb:c0c0/c0c0/c0c0\x1b\\\x1b[?997;2n\x1b[?62;22c"
        );
        // DECRQM knows mode 2031, which the program sets.
        pane.output(b"\x1b[?2031h\x1b[?2031$p");
        assert!(pane.screen().mode(Mode::ColorSchemeUpdates));
        assert_eq!(pane.input.drain_all(), b"\x1b[?2031;1$y");
        Ok(())
    }

    /// The first row's text, blanks as spaces, trimmed.
    fn first_row(pane: &Pane) -> String {
        let cols = pane.screen().size().cols();
        (0..cols)
            .filter_map(|x| pane.screen().cell(0, x))
            .map(|c| c.contents().chars().next().unwrap_or(' '))
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn pane(rows: u16, cols: u16) -> Result<Pane, Error> {
        Pane::new(
            PaneId::of(1),
            "sh".into(),
            Runs::Shell,
            size(rows, cols),
            10,
        )
    }

    /// A frame drawn in synchronized output reaches the screen whole, when
    /// it ends, however its bytes are split across reads: the BSU, the
    /// frame and the ESU each in pieces, and several frames in one read.
    #[test]
    fn a_synchronized_frame_reaches_the_screen_whole() -> Result<(), Error> {
        let mut pane = pane(3, 30)?;
        pane.output(b"a\x1b[?2026hb");
        assert_eq!(first_row(&pane), "a", "the frame is held");
        assert!(pane.screen().mode(Mode::SynchronizedOutput));
        pane.output(b"c\x1b[?20");
        assert_eq!(first_row(&pane), "a", "an ESU begun is not one");
        pane.output(b"26ld");
        assert_eq!(first_row(&pane), "abcd");
        assert!(!pane.screen().mode(Mode::SynchronizedOutput));
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
        let mut pane = pane(3, 30)?;
        let t0 = Instant::now();
        pane.output_at(b"\x1b[?2026hx", t0);
        assert_eq!(pane.frame_deadline(), Some(crate::after(t0, FRAME_TIMEOUT)));
        pane.release_frame();
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
        pane.resize(size(3, 20));
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
        let mut pane = pane(3, 30)?;
        pane.resize(size(4, 20));
        assert!(pane.input.drain_all().is_empty(), "not asked");
        pane.output(b"\x1b[?2048h");
        assert_eq!(pane.input.drain_all(), b"\x1b[48;4;20;0;0t");
        pane.resize(size(6, 25));
        assert_eq!(pane.input.drain_all(), b"\x1b[48;6;25;0;0t");
        pane.resize(size(6, 25));
        assert!(pane.input.drain_all().is_empty(), "the same size");
        Ok(())
    }

    /// xterm's title stack: vim and tmux push the title on starting and pop
    /// it on leaving, so the pane's title comes back. Ps 1 (the icon) is no
    /// title; the stack keeps at most ten; a pop with none pushed changes
    /// nothing.
    #[test]
    fn a_pushed_title_comes_back_when_popped() -> Result<(), Error> {
        let mut pane = pane(3, 30)?;
        pane.output(b"\x1b]2;shell\x07\x1b[22;0t\x1b]2;vim\x07");
        assert_eq!(pane.title.as_str(), "vim");
        pane.output(b"\x1b[23;0t");
        assert_eq!(pane.title.as_str(), "shell");
        pane.output(b"\x1b[22;2t\x1b]2;tmux\x07\x1b[23;2t\x1b[23;0t");
        assert_eq!(
            pane.title.as_str(),
            "shell",
            "a pop with none left changes nothing"
        );
        pane.output(b"\x1b[22;1t\x1b]2;other\x07\x1b[23;1t");
        assert_eq!(
            pane.title.as_str(),
            "other",
            "the icon's stack is not the title's"
        );
        for n in 0..12 {
            pane.output(format!("\x1b]2;t{n}\x07\x1b[22t").as_bytes());
        }
        for _ in 0..12 {
            pane.output(b"\x1b[23t");
        }
        assert_eq!(
            pane.title.as_str(),
            "t2",
            "ten kept: the first two pushes dropped"
        );
        Ok(())
    }

    /// neovim's handshake for underline styles (neovim 0.12.5,
    /// `tui_query_extended_underline`): it sets a curly underline and asks
    /// for the pen with DECRQSS, and draws its diagnostics curly, with
    /// their colour (`58:2::r:g:b`), only if `4:3` comes back. A pane keeps
    /// the style and says so; the cells keep what neovim then draws.
    #[test]
    fn a_pane_tells_neovim_it_keeps_underline_styles() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(3, 30)?;
        pane.output(b"\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\");
        assert_eq!(pane.input.drain_all(), b"\x1bP1$r0;4:3m\x1b\\");
        pane.output(b"\x1b[0m\x1b[4:3m\x1b[58:2::255:0:0mx\x1b[0m");
        let cell = pane.screen().cell(0, 0).ok_or("no cell")?;
        assert_eq!(cell.underline_style(), fux_vt::UnderlineStyle::Curly);
        assert_eq!(
            cell.underline_color(),
            fux_vt::Color::Rgb([255, 0, 0].into())
        );
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

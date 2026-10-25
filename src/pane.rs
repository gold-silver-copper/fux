//! A pane: its terminal emulator, its process, and the input waiting for it.
use crate::after;
use crate::bytes::ByteQueue;
use crate::events::What;
use crate::id::PaneId;
use crate::outer::{Colours, Scheme};
use crate::process::{Child, Ended, Pid};
use fux_vt::{Event, Feature, Screen, Size};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The largest single piece of input: a whole paste and its envelope.
pub const MAX_INPUT: usize = crate::decode::PASTE_LIMIT + 12;
/// Input may wait for the program up to this many bytes, however many
/// pieces it came in.
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

/// Why input is not queued, or a pane cannot be made.
#[derive(Debug)]
pub enum Error {
    /// The queue is full, or refusing until its terminal takes some.
    InputFull,
    /// No terminal of this size.
    Terminal { size: Size, source: fux_vt::Error },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InputFull => f.write_str(
                "the pane's input queue is full; nothing more is queued until its terminal takes some",
            ),
            Error::Terminal { size, source } => {
                write!(f, "a {}x{} terminal: {source}", size.cols(), size.rows())
            }
        }
    }
}

impl std::error::Error for Error {}

/// Input and terminal replies waiting for the pane's program to read them,
/// end to end, to be written together: at most [`INPUT_BYTES`] of them,
/// however many pieces they came in.
#[derive(Default)]
pub struct InputQueue {
    bytes: ByteQueue,
    /// Input was refused, and the program has not read since: everything
    /// is refused, so none arrives with a hole before it.
    refusing: bool,
    lost: Lost,
}

/// Whether a terminal reply was dropped, and whether that was told: told
/// once for each run of reads that drop one, which ends at a read that
/// drops none while the queue takes input.
#[derive(Clone, Copy, Default, PartialEq)]
enum Lost {
    #[default]
    None,
    Untold,
    Told,
}

impl InputQueue {
    /// Queues `bytes` as a piece, or refuses them whole if the queue is
    /// full, and then everything until its terminal takes some.
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
        fits.then_some(()).ok_or(Error::InputFull)
    }
    /// How many bytes it holds, not yet written to the program.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    /// Whether `len` bytes more fit now: if not, they are refused, and so
    /// is everything after them until its terminal takes some, as `push`
    /// refuses.
    pub fn fits(&mut self, len: usize) -> Result<(), Error> {
        let fits = len <= self.room();
        self.refusing |= !fits;
        fits.then_some(()).ok_or(Error::InputFull)
    }
    /// How many bytes more it takes now.
    pub fn room(&self) -> usize {
        match self.refusing {
            true => 0,
            false => INPUT_BYTES.saturating_sub(self.len()),
        }
    }
    /// Whether a reply was lost and is yet to be told, which it then is
    /// (`Lost`).
    pub fn lost_reply(&mut self) -> bool {
        let untold = self.lost == Lost::Untold;
        if untold {
            self.lost = Lost::Told;
        }
        untold
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    /// Whether input is refused until its terminal takes some.
    pub fn refusing(&self) -> bool {
        self.refusing
    }
    /// The bytes to write next: every piece queued.
    pub fn front(&self) -> Option<&[u8]> {
        (!self.bytes.is_empty()).then(|| self.bytes.as_slice())
    }
    /// `n` bytes of the front were written: its terminal took them.
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

/// What fux tells programs it is: XTVERSION answers `fux` and its version,
/// DA2 the version (`CSI > 1 ; Pv ; 0 c`), and DA1 a VT220-class terminal
/// (`CSI ? 62 ; 6 ; 21 ; 22 ; 28 c`). The user's decision, on the corpus's
/// evidence: told it is fux, Claude Code asks for and uses synchronized
/// output.
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
/// changing), pictures (sixel a pane's program draws, which fux draws on
/// clients whose terminals draw sixel, `render::pictures`), reflow (a
/// resized pane's lines re-wrap at its new width, its history with them,
/// as in the terminals fux runs in) and fux's identity.
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
    .with(Feature::Pictures)
    .with(Feature::Reflow)
    .with_identity(Some(IDENTITY));

/// The most bytes of replies one read of a program's output queues: the
/// rest are dropped, and told as a reply dropped for a full queue is.
pub const REPLIES_MAX: usize = 4096;
/// The most titles a pane's program can push (`CSI 22 t`): xterm's bound.
const TITLE_STACK: usize = 10;
/// The most characters of a title kept.
pub const TITLE_MAX: usize = 256;
/// The most characters of a notification kept.
pub const NOTIFICATION_MAX: usize = 500;

/// Text a program sent (a title, a notification), as it is kept: without
/// control characters, at most `MAX` characters, and `cut` if it held more
/// (or was longer than fux-vt keeps of a string), the rest dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Said<const MAX: usize> {
    text: String,
    cut: bool,
}

impl<const MAX: usize> Said<MAX> {
    fn new(sent: fux_vt::Kept<'_>) -> Self {
        let text = String::from_utf8_lossy(sent.bytes);
        let mut chars = text.chars().filter(|c| !c.is_control());
        let text = chars.by_ref().take(MAX).collect();
        let cut = sent.cut || chars.next().is_some();
        Self { text, cut }
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    /// Whether the program sent more than was kept.
    pub fn cut(&self) -> bool {
        self.cut
    }
}

/// A pane's title, as its program sets it with OSC 0 or 2, and the titles
/// it pushed (`CSI 22 t`) to pop (`CSI 23 t`), at most xterm's ten: a push
/// past them forgets the oldest.
#[derive(Default)]
pub struct Title {
    now: Said<TITLE_MAX>,
    pushed: VecDeque<Said<TITLE_MAX>>,
}

impl Title {
    pub fn as_str(&self) -> &str {
        self.now.as_str()
    }
    pub fn said(&self) -> &Said<TITLE_MAX> {
        &self.now
    }
    /// Sets it to `title`; whether it changed.
    fn set(&mut self, title: Said<TITLE_MAX>) -> bool {
        let changed = title != self.now;
        self.now = title;
        changed
    }
    fn push(&mut self) {
        if self.pushed.len() >= TITLE_STACK {
            self.pushed.pop_front();
        }
        self.pushed.push_back(self.now.clone());
    }
    /// The title last pushed comes back; with none, nothing changes.
    /// Whether it changed.
    fn pop(&mut self) -> bool {
        self.pushed.pop_back().is_some_and(|title| self.set(title))
    }
}

/// Replies (DSR, DA) and events the parser produces while reading output.
struct Sink<'a> {
    /// The replies to one read, queued together after it: at most
    /// `REPLIES_MAX` bytes; whether one was dropped past them.
    replies: Vec<u8>,
    dropped: bool,
    /// Where its bells, notifications, titles and commands' ends go, in
    /// order, for the session's journal.
    happened: &'a mut Vec<What>,
    title: &'a mut Title,
    /// Set by a bell (BEL).
    bell: &'a mut bool,
    alerts: &'a mut Alerts,
    /// Each OSC 52 write, in order: its UTF-8 text, or why it is no copy.
    copied: &'a mut Vec<Result<String, Uncopied>>,
    /// What colour queries are answered with (`outer`).
    colours: &'a Colours,
    /// The scheme its program was last told (`Pane::scheme_told`).
    scheme_told: &'a mut Option<Scheme>,
}

impl Sink<'_> {
    /// The title changed: the journal is told.
    fn titled(&mut self) {
        let title = self.title.said().clone();
        self.happened.push(What::Title(title));
    }
}

impl fux_vt::Sink for Sink<'_> {
    fn reply(&mut self, bytes: &[u8]) {
        match self.replies.len().saturating_add(bytes.len()) <= REPLIES_MAX {
            true => self.replies.extend_from_slice(bytes),
            false => self.dropped = true,
        }
    }
    /// A colour query (OSC 10, 11) is answered if the colour is known.
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Title(title) => {
                if self.title.set(Said::new(title)) {
                    self.titled();
                }
            }
            Event::ColorQuery { number, bel } => {
                if let Some(answer) = self.colours.answer(number, bel) {
                    fux_vt::Sink::reply(self, &answer);
                }
            }
            Event::Bell => {
                self.happened.push(What::Bell);
                *self.bell = true;
                self.alerts.rung = self.alerts.rung.saturating_add(1);
            }
            Event::Notification(text) => {
                self.alerts.rung = self.alerts.rung.saturating_add(1);
                let text = Said::new(text);
                self.happened.push(What::Notification(text.clone()));
                self.alerts.last = Some(text);
            }
            Event::Clipboard { data, .. } => {
                self.copied.push(match data.cut {
                    true => Err(Uncopied::Cut),
                    false => (crate::json::unbase64(data.bytes))
                        .and_then(|d| String::from_utf8(d).ok())
                        .ok_or(Uncopied::NotText),
                });
            }
            Event::CommandEnded(status) => self.happened.push(What::Done(status)),
            Event::IconName(_) => {}
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
                    *self.scheme_told = Some(scheme);
                }
            }
            (b"", b't', Some([22])) if title => self.title.push(),
            (b"", b't', Some([23])) if title && self.title.pop() => self.titled(),
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
    Exited(Ended),
}

impl Process {
    pub fn child(&self) -> Option<&Child> {
        match self {
            Self::Absent | Self::Exited(_) => None,
            Self::Reading(child) | Self::HungUp(child) => Some(child),
        }
    }

    /// How its program ended, once it has.
    pub fn exit_status(&self) -> Option<Ended> {
        match self {
            Self::Exited(status) => Some(*status),
            Self::Absent | Self::Reading(_) | Self::HungUp(_) => None,
        }
    }

    /// The terminal's foreground process group, if it is not the
    /// program's own: a job the shell runs; or why the terminal can't
    /// tell.
    pub fn job(&self) -> Result<Option<Pid>, fuxix::Errno> {
        let Some(child) = self.child() else {
            return Ok(None);
        };
        let group = fuxix::terminal::foreground_group(&child.master)?;
        Ok(group.filter(|group| *group != child.leader.pid()))
    }
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

/// What a pane running `argv` is named until it is renamed: its program's
/// file name.
pub(crate) fn named(argv: &[String]) -> String {
    argv.first().map(|p| basename(p)).unwrap_or_default()
}

/// A program's file name, or the program as given if it has none.
pub(crate) fn basename(program: &str) -> String {
    std::path::Path::new(program)
        .file_name()
        .map_or_else(|| program.to_owned(), |n| n.to_string_lossy().into_owned())
}

/// What a program rang or announced since it started: bells, and desktop
/// notifications (OSC 9, 777, 99) with the last one's text.
#[derive(Clone, Debug, Default)]
pub struct Alerts {
    /// Bells and notifications together, as a pane's version counts them.
    pub rung: u64,
    pub last: Option<Said<NOTIFICATION_MAX>>,
}

/// Why a program's copy (OSC 52) copied nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Uncopied {
    /// It was longer than fux-vt keeps of a string.
    Cut,
    /// It was not base64 of UTF-8 text.
    NotText,
}

impl std::fmt::Display for Uncopied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Uncopied::Cut => write!(
                f,
                "is longer than the {} KiB a string may be",
                fux_vt::OSC_PAYLOAD_LIMIT / 1024
            ),
            Uncopied::NotText => f.write_str("is not base64 of UTF-8 text"),
        }
    }
}

pub struct Pane {
    pub id: PaneId,
    /// Changed by `rename` and `respawned`, which say so.
    name: String,
    /// Set by the program with OSC 0 or 2, which says so.
    title: Title,
    /// Its screen's size is the PTY's: the smallest rectangle any client
    /// shows the pane in. What it reads and is resized to goes through the
    /// pane, which says what events tell of it.
    parser: fux_vt::Parser,
    /// Its program ends by `exit`, which says so.
    process: Process,
    pub input: InputQueue,
    pub runs: Runs,
    /// The output of a frame the program is drawing in synchronized output,
    /// from BSU on, since when (its timeout's start), and where the parser
    /// will end it; see [`Pane::output`].
    frame: Option<(Vec<u8>, Instant, fux_vt::FrameScan)>,
    /// Output read from the program but not yet given to the screen: a
    /// call of the parser did as much work as one may
    /// (`fux_vt::Stop::Budget`), and the rest waits for the pane's next
    /// turn, so that one pane can't hold up the others on the server's one
    /// thread. While any waits, output read is put after it, and the
    /// server reads no more from the program; see [`Pane::read_unread`].
    /// With it, since when it waits: its turn is due then.
    unread: (Vec<u8>, Instant),
    /// How many of the unread bytes are the rest of a frame being read
    /// (`Pane::mid_frame`).
    frame_left: usize,
    /// How its program exited, once told, while what it wrote is still
    /// being read (`Session::exited`).
    pub exiting: Option<Ended>,
    /// What the program's colour queries are answered with, as the session
    /// finds them before each read of output (`outer`).
    pub colours: Colours,
    /// The colour scheme its program was last told, answering `CSI ? 996 n`
    /// or reporting a change (mode 2031): one its terminal now differs from
    /// is told it (`Session::tell_schemes`).
    pub scheme_told: Option<Scheme>,
    /// The program rang the bell since the session last looked
    /// (`Session::ring`).
    pub bell: bool,
    /// What the program put on the clipboard (OSC 52) since the session
    /// last looked, in order: each text, which the session copies
    /// (`Session::copied`), or why it copied nothing, which it tells. The
    /// session takes them after every read, so they are fewer than the
    /// bytes it read.
    pub copied: Vec<Result<String, Uncopied>>,
    /// When its program last wrote, or started.
    pub wrote_at: Instant,
    /// What happened in it that events tell, in order, since the session
    /// last took it into its journal (`workspace::PaneMut`).
    said: Vec<What>,
    /// The mouse button `send-mouse press` holds down until its release: a
    /// `move` meanwhile is a drag.
    pub mouse_held: Option<fux_vt::keys::mouse::MouseButton>,
    /// How many times its program was started again (`respawn`), each on a
    /// screen of its own: a wait from the program before answers
    /// `respawned`.
    pub respawns: u64,
    pub alerts: Alerts,
    /// Where everything the program writes is kept too (`log-to`, `start-log`).
    pub log: Option<std::fs::File>,
}

/// What is said of a pane where no shell command was ever marked.
pub const UNMARKED: &str = "no shell command was marked here (OSC 133 C and D): \
                            `fux help agents shells` says how to turn marks on";

impl Pane {
    pub fn new(
        id: PaneId,
        name: String,
        runs: Runs,
        size: Size,
        history: usize,
    ) -> Result<Pane, Error> {
        let parser = fux_vt::Parser::with_options(size, history, OPTIONS)
            .map_err(|source| Error::Terminal { size, source })?;
        Ok(Pane {
            id,
            name,
            title: Title::default(),
            parser,
            process: Process::Absent,
            input: InputQueue::default(),
            runs,
            frame: None,
            unread: (Vec::new(), Instant::now()),
            frame_left: 0,
            exiting: None,
            colours: Colours::default(),
            scheme_told: None,
            bell: false,
            copied: Vec::new(),
            wrote_at: Instant::now(),
            said: Vec::new(),
            respawns: 0,
            mouse_held: None,
            alerts: Alerts::default(),
            log: None,
        })
    }

    /// Reads program output into the screen, its replies queued as input.
    ///
    /// A frame the program draws in synchronized output (from `CSI ? 2026 h`,
    /// BSU, to `CSI ? 2026 l`, ESU, or whatever else resets the mode, found
    /// as the parser reads it: `fux_vt::FrameScan`) is held, unread, until
    /// it ends, then read whole, as alacritty does, so a client is never
    /// painted half of one: a frame whose reading takes several turns (a
    /// parser call's budget) is painted as the client last saw the pane
    /// till it is whole (`Pane::mid_frame`). A
    /// frame is read anyway once [`FRAME_TIMEOUT`] passes
    /// ([`Pane::release_frame`]) or it grows past [`FRAME_LIMIT`]; after
    /// that the program's output is read as it comes until its next BSU.
    pub fn output(&mut self, bytes: &[u8]) {
        self.output_at(bytes, Instant::now())
    }

    /// [`Pane::output`] at `now`.
    ///
    /// Each call of the parser does at most a stated amount of work; at it,
    /// the rest of the output waits, unread, for the pane's next turn
    /// ([`Pane::read_unread`]).
    pub fn output_at(&mut self, bytes: &[u8], now: Instant) {
        if let Some(log) = &mut self.log {
            // A log that cannot be written stops; the pane goes on.
            if std::io::Write::write_all(log, bytes).is_err() {
                self.log = None;
            }
        }
        self.wrote_at = now;
        if self.has_unread() {
            self.unread.0.extend_from_slice(bytes);
            return;
        }
        self.read(bytes, now);
    }

    /// Whether output waits to be read on the pane's next turn
    /// ([`Pane::read_unread`]), or the work of a sequence read already,
    /// which a call of the parser stopped at its budget before doing
    /// (`fux_vt::Parser::has_work_left`: a screen-wide sequence it left
    /// whole, REP's copies, a checksum's rows), any output read since
    /// waiting behind it.
    pub fn has_unread(&self) -> bool {
        !self.unread.0.is_empty() || self.parser.has_work_left()
    }

    /// When the pane's next turn to read the output waiting is due, if any
    /// waits: since it began to wait, so that it is due on the server's
    /// next turn, whenever that reads the clock.
    pub fn unread_due(&self) -> Option<Instant> {
        self.has_unread().then_some(self.unread.1)
    }

    /// Whether the screen is part way through a frame drawn in
    /// synchronized output, the rest of it waiting for the pane's next
    /// turns: till it is whole, a client is painted the pane as it last
    /// painted it ("keep rendering the last rendered state",
    /// `references/modern/mode_2026_synchronized_output.md`), and the rest
    /// of its screen as it is (`render::Grid::keep_area`).
    pub fn mid_frame(&self) -> bool {
        self.frame_left > 0
    }

    /// Reads the output waiting since a call of the parser did all the work
    /// one may, until the next call does: the rest of a frame first, as
    /// the frame was read.
    pub fn read_unread(&mut self) {
        let unread = std::mem::take(&mut self.unread.0);
        // Only the work of a sequence read already: a call of no bytes does
        // it, and stops again if that spends its budget.
        if unread.is_empty() {
            if self.parse(&[], false).is_some() {
                self.wait_unread(Vec::new());
            }
            return;
        }
        let frame_left = std::mem::take(&mut self.frame_left);
        let at = frame_left.min(unread.len());
        let (frame, after) = unread.split_at_checked(at).unwrap_or((&unread, &[]));
        if let Some((taken, _)) = self.parse(frame, false) {
            self.frame_left = frame.len().saturating_sub(taken);
            self.wait_unread(unread.get(taken..).unwrap_or_default().to_vec());
            return;
        }
        self.read(after, Instant::now());
    }

    /// `left` waits to be read on the pane's next turn, from now.
    fn wait_unread(&mut self, left: Vec<u8>) {
        self.unread = (left, Instant::now());
    }

    /// Reads `bytes` as [`Pane::output_at`] does at `now`, with no output
    /// waiting.
    fn read(&mut self, bytes: &[u8], now: Instant) {
        let mut rest = bytes;
        loop {
            if let Some((held, _, scan)) = &mut self.frame {
                let read = held.len();
                held.extend_from_slice(rest);
                let Some(end) = scan.end(held) else {
                    if held.len() > FRAME_LIMIT {
                        self.release_frame();
                    }
                    return;
                };
                let mut frame = std::mem::take(held);
                self.frame = None;
                frame.truncate(end);
                // The frame ends in what came now: the rest is after it.
                rest = rest.get(end.saturating_sub(read)..).unwrap_or_default();
                if let Some((taken, _)) = self.parse(&frame, false) {
                    let left = frame.get(taken..).unwrap_or_default();
                    self.frame_left = left.len();
                    self.wait_unread([left, rest].concat());
                    return;
                }
            } else {
                match self.parse(rest, true) {
                    None => return,
                    Some((end, fux_vt::Stop::Begun)) => {
                        self.frame = Some((Vec::new(), now, self.parser.frame_scan()));
                        rest = rest.get(end..).unwrap_or_default();
                    }
                    Some((end, _)) => {
                        self.wait_unread(rest.get(end..).unwrap_or_default().to_vec());
                        return;
                    }
                }
            }
        }
    }

    /// When the held frame is read anyway, if one is held.
    pub fn frame_deadline(&self) -> Option<Instant> {
        self.frame
            .as_ref()
            .map(|(_, since, _)| after(*since, FRAME_TIMEOUT))
    }

    /// Reads the held frame now, ended or not.
    pub fn release_frame(&mut self) {
        if let Some((held, ..)) = self.frame.take()
            && let Some((taken, _)) = self.parse(&held, false)
        {
            // No output waits while a frame is held: it is the first.
            let left = held.get(taken..).unwrap_or_default().to_vec();
            self.frame_left = left.len();
            self.wait_unread(left);
        }
    }

    /// Gives `bytes` to the screen, stopping after a BSU if `until_frame`,
    /// and where a call of the parser did all the work one may
    /// (`Stop::Budget`). Returns how many bytes were read if it stopped,
    /// and why (`Stop::Begun` or `Stop::Budget`). What they did (a bell, a
    /// title, a command's end) is journaled after. Never inlined: the
    /// parser's loop compiles tighter on its own.
    #[inline(never)]
    fn parse(&mut self, bytes: &[u8], until_frame: bool) -> Option<(usize, fux_vt::Stop)> {
        let mut sink = Sink {
            replies: Vec::new(),
            dropped: false,
            happened: &mut self.said,
            title: &mut self.title,
            bell: &mut self.bell,
            alerts: &mut self.alerts,
            copied: &mut self.copied,
            colours: &self.colours,
            scheme_told: &mut self.scheme_told,
        };
        // The parser refuses only allocations beyond its limits; the screen
        // stays as it was and output continues.
        // Only `process_until_stop`, even to read past a BSU: with one way
        // in, the parser's loop is compiled once and its byte handling is
        // inlined into it, as it was before frames were held; with two it
        // was not, and escape-heavy output was a quarter slower. A call
        // that did all the work one may stops the read for this turn.
        let (mut begun, mut read) = (None, 0usize);
        while let Some(rest) = bytes.get(read..)
            && let Ok(Some((taken, stop))) = self.parser.process_until_stop(rest, &mut sink)
        {
            read = read.saturating_add(taken);
            if stop == fux_vt::Stop::Budget || until_frame && stop == fux_vt::Stop::Begun {
                begun = Some((read, stop));
                break;
            }
        }
        // Queued as `push` queues; a full queue drops them, which is told,
        // as replies past the read's bound are (`Lost`).
        let full = !sink.replies.is_empty() && self.input.push(&sink.replies).is_err();
        self.input.lost = match (self.input.lost, full || sink.dropped) {
            (Lost::None, true) => Lost::Untold,
            (Lost::Told, false) if !self.input.refusing => Lost::None,
            (lost, _) => lost,
        };
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
    pub fn resize(&mut self, size: Size) {
        if self.size() == size {
            return;
        }
        self.release_frame();
        if self.parser.resize(size).is_ok() {
            self.said.push(What::Resized(size));
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

    pub fn size(&self) -> Size {
        self.screen().size()
    }

    pub fn screen(&self) -> &Screen {
        self.parser.screen()
    }

    /// Queues for its program what `write` makes for its screen, as
    /// `InputQueue::push_with` does.
    pub fn push_with(&mut self, write: impl FnOnce(&Screen, &mut Vec<u8>)) -> Result<(), Error> {
        let screen = self.parser.screen();
        self.input.push_with(|out| write(screen, out))
    }

    /// Its program's queries are answered with `colours` and `palette`
    /// from now on, and it is told its terminal draws `graphics`.
    pub fn host(
        &mut self,
        colours: Colours,
        palette: crate::outer::Palette,
        graphics: fux_vt::Graphics,
    ) {
        self.colours = colours;
        self.parser.set_host_palette(palette);
        self.parser.set_host_graphics(graphics);
    }

    pub fn clear_history(&mut self) {
        self.parser.clear_history();
    }

    pub fn set_history_bytes(&mut self, bytes: usize) {
        self.parser.set_history_bytes(bytes);
    }

    /// Whether its program is one a restore brought back, waiting for
    /// `respawn`.
    pub fn paused(&self) -> bool {
        match self.process {
            Process::Absent => match self.runs {
                Runs::Program(_) => true,
                Runs::Shell => false,
            },
            Process::Reading(_) | Process::HungUp(_) | Process::Exited(_) => false,
        }
    }

    /// Where it stands now (`Version`).
    pub fn counts(&self) -> crate::command::Counts {
        crate::command::Counts {
            respawns: self.respawns,
            screen: self.screen().revision(),
            bells: self.alerts.rung,
            done: self.screen().commands().finished,
            ended: self.process.exit_status().is_some(),
        }
    }

    /// Its version in the server run `run`.
    pub fn version(&self, run: crate::command::Run) -> crate::command::Version {
        crate::command::Version {
            pane: self.id,
            counts: self.counts(),
            run,
        }
    }

    /// Whether a shell command was ever marked here (OSC 133 `C`): else
    /// nothing says where one's output is or when it ends.
    pub fn marked(&self) -> bool {
        self.screen().commands().started > 0
    }

    /// Whether a shell command runs, as the marks say: never once the
    /// pane's program ended, which ended it too.
    pub fn command_running(&self) -> bool {
        self.screen().commands().running() && self.process.exit_status().is_none()
    }

    /// Starts `launch` in the pane again as `process`, on a screen as new,
    /// its size and history kept; the process that was there, to end.
    pub fn respawned(
        &mut self,
        launch: Launch,
        process: Process,
        history: usize,
    ) -> Result<Process, Error> {
        // Named after its program, unless renamed.
        let name = match &self.runs {
            Runs::Program(old) if named(&old.argv) == self.name => named(&launch.argv),
            Runs::Program(_) | Runs::Shell => self.name.clone(),
        };
        let runs = Runs::Program(launch);
        // A new program, its own pane, its counts afresh but `respawns`. Kept
        // are what the session has yet to take from the old one (a bell, a
        // copy), its colours and log.
        let mut started = Pane::new(self.id, self.name.clone(), runs, self.size(), history)?;
        started.said = std::mem::take(&mut self.said);
        started.said.push(What::Respawned);
        started.rename(name);
        started.colours = std::mem::take(&mut self.colours);
        started.bell = self.bell;
        started.copied = std::mem::take(&mut self.copied);
        started.log = self.log.take();
        // 2^64 respawns are never reached: the count never stands still.
        started.respawns = self.respawns.saturating_add(1);
        started.process = process;
        Ok(std::mem::replace(self, started).process)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Names it `name`: said, if it is another.
    pub fn rename(&mut self, name: String) {
        if name != self.name {
            self.said.push(What::Renamed(name.clone()));
            self.name = name;
        }
    }

    pub fn process(&self) -> &Process {
        &self.process
    }

    /// A pane made with no program runs `child`: a pane not yet in the
    /// session, which alone is had by value.
    pub fn running(mut self, child: Child) -> Pane {
        self.process = Process::Reading(child);
        self
    }

    /// The program's terminal hung up: it is read no more, and waits to be
    /// reaped.
    pub fn stop_reading(&mut self) {
        self.process = match std::mem::replace(&mut self.process, Process::Absent) {
            Process::Reading(child) | Process::HungUp(child) => Process::HungUp(child),
            other @ (Process::Absent | Process::Exited(_)) => other,
        };
    }

    /// Its program ended so: said, and the process that was there, to be
    /// reaped.
    pub fn exit(&mut self, ended: Ended) -> Process {
        self.ended(ended);
        std::mem::replace(&mut self.process, Process::Exited(ended))
    }

    /// Its program ended so: said, as a shell's is before its pane closes.
    pub fn ended(&mut self, ended: Ended) {
        self.said.push(What::Exited(ended));
    }

    /// What happened in it that events tell, since last taken, oldest
    /// first: for the session's journal, which alone gives the leave.
    pub(crate) fn said(&mut self, _: crate::workspace::Journaling) -> Vec<What> {
        std::mem::take(&mut self.said)
    }

    /// Its process, as it is closed.
    pub fn into_process(self) -> Process {
        self.process
    }

    /// Gives it `process`, as a test's pane runs a child of the test's.
    #[cfg(test)]
    pub(crate) fn set_process(&mut self, process: Process) -> Process {
        std::mem::replace(&mut self.process, process)
    }

    pub fn title(&self) -> &Said<TITLE_MAX> {
        self.title.said()
    }

    /// Why `event` is not reported to its program, if it would be but for
    /// this: it asked for reports in pixels (mode 1016), and no terminal
    /// showing it has said its cell size.
    pub fn no_mouse_report(&self, event: fux_vt::keys::mouse::MouseEvent) -> Option<String> {
        self.screen().mouse_needs_cell(event).then(|| {
            format!(
                "the program in {} asked for mouse reports in pixels (mode 1016), and no terminal \
                 showing it has said its cell size",
                self.id
            )
        })
    }

    pub fn label(&self) -> &str {
        match self.title.as_str() {
            "" => &self.name,
            title => title,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fux_vt::Mode;

    /// `rows` by `cols`, each at least one.
    pub(crate) fn size(rows: u16, cols: u16) -> Size {
        let one = |n| std::num::NonZeroU16::new(n).unwrap_or(std::num::NonZeroU16::MIN);
        Size::from((one(rows), one(cols)))
    }

    /// Once input is refused, nothing more is queued until the program
    /// reads, as the notice says: accepting a later key would deliver it
    /// with a hole where the refused input was.
    #[test]
    fn after_a_refusal_nothing_is_queued_until_its_terminal_takes_some() {
        let mut queue = InputQueue::default();
        while queue.push(vec![b'p'; MAX_INPUT]).is_ok() {}
        let refusal = queue.push(vec![b'k']);
        assert!(
            matches!(refusal, Err(Error::InputFull)),
            "a key after a refused paste: {refusal:?}"
        );
        assert!(refusal.is_err_and(|e| e.to_string().contains("input queue is full")));
        // Some of the front written, its terminal took it: input is taken
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
            assert_eq!(
                pane.screen().cursor_shape(),
                fux_vt::CursorShape::BlinkingBar,
                "split {split}"
            );
        }
        let mut pane = pane(5, 20)?;
        pane.output(b"\x1b[?1004h\x1b[?1004l\x1b[2 q\x1b[ q");
        assert!(!pane.screen().mode(Mode::FocusReporting));
        assert_eq!(pane.screen().cursor_shape(), fux_vt::CursorShape::Zero);
        pane.output(b"\x1b[?1004h\x1bc");
        assert!(!pane.screen().mode(Mode::FocusReporting));
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
        assert_eq!(
            pane.input.drain_all(),
            b"\x1b[?62;6;21;22;28c",
            "nothing known"
        );
        pane.colours = Colours {
            foreground: Some([0xc0; 3].into()),
            background: Some([0, 0x10, 0xff].into()),
            scheme: Some(Scheme::Light),
        };
        pane.output(b"\x1b]11;?\x07\x1b]10;?\x1b\\\x1b[?996n\x1b[c\x1b]12;?\x07");
        assert_eq!(
            pane.input.drain_all(),
            b"\x1b]11;rgb:0000/1010/ffff\x07\x1b]10;rgb:c0c0/c0c0/c0c0\x1b\\\x1b[?997;2n\x1b[?62;6;21;22;28c"
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

    /// Output that spends more than a parser call's budget on a screen of
    /// `rows` by `cols`, whatever fux-vt's weights: DECALN fills every
    /// cell and ED clears them all, the pair repeated twice as often as
    /// the fewest that a parser call stops at its budget in.
    pub(crate) fn spends_the_budget(rows: u16, cols: u16) -> Vec<u8> {
        struct Discard;
        impl fux_vt::Sink for Discard {}
        let output = |n| -> Vec<u8> {
            std::iter::repeat_n(&b"\x1b#8\x1b[2J"[..], n)
                .flatten()
                .copied()
                .collect()
        };
        let mut n = 1usize;
        while n < 1 << 24 {
            let stops = fux_vt::Parser::with_options(size(rows, cols), 1 << 10, OPTIONS).is_ok_and(
                |mut parser| {
                    matches!(
                        parser.process_until_stop(&output(n), &mut Discard),
                        Ok(Some((_, fux_vt::Stop::Budget)))
                    )
                },
            );
            if stops {
                break;
            }
            n = n.saturating_mul(2);
        }
        output(n.saturating_mul(2))
    }

    fn pane(rows: u16, cols: u16) -> Result<Pane, Error> {
        Pane::new(
            PaneId::of(1),
            "sh".into(),
            Runs::Shell,
            size(rows, cols),
            1 << 10,
        )
    }

    /// A call of the parser that does all the work one may stops the read
    /// (THREATS.md 4.4): the rest of the output, and output read after it,
    /// waits for the pane's next turn, and is read then, the screen ending
    /// as one read of it all leaves it.
    #[test]
    fn a_read_that_spends_its_budget_goes_on_at_the_next_turn() -> Result<(), Error> {
        let reps = spends_the_budget(50, 200);
        let mut pane = pane(50, 200)?;
        pane.output(&reps);
        assert!(pane.has_unread(), "the budget stopped no call");
        pane.output(b"\x1b[HZ");
        assert!(
            !first_row(&pane).starts_with('Z'),
            "output after the stop is read before it"
        );
        let mut turns = 0u32;
        while pane.has_unread() && turns < 100 {
            pane.read_unread();
            turns = turns.saturating_add(1);
        }
        assert!(!pane.has_unread(), "{turns} turns");
        let mut whole =
            fux_vt::Parser::with_options(size(50, 200), 1 << 10, OPTIONS).map_err(|source| {
                Error::Terminal {
                    size: size(50, 200),
                    source,
                }
            })?;
        let _ = whole.process(&[&reps[..], b"\x1b[HZ"].concat());
        assert_eq!(
            pane.screen().cursor_position(),
            whole.screen().cursor_position()
        );
        let rows = |p: &fux_vt::Screen| p.rows().map(|r| r.cells_text()).collect::<Vec<_>>();
        assert_eq!(rows(pane.screen()), rows(whole.screen()));
        assert_eq!(first_row(&pane), "Z");
        Ok(())
    }

    /// A screen-wide sequence the parser left for its next call, as the
    /// output's last, all its bytes read, is carried out at the pane's
    /// next turn: the pane says output waits while it does, though no byte
    /// does, and the screen ends as one read of it all leaves it. At
    /// 2000x2000, after DECALN's fill, ED 2 would take the call past U
    /// (`Screen::holds_back`).
    #[test]
    fn a_sequence_left_past_the_budget_at_the_outputs_end_goes_on() -> Result<(), Error> {
        let output = b"\x1b#8\x1b[2J";
        let mut pane = pane(2000, 2000)?;
        pane.output(output);
        assert!(pane.has_unread(), "the sequences were done in one call");
        let mut turns = 0u32;
        while pane.has_unread() && turns < 100 {
            pane.read_unread();
            turns = turns.saturating_add(1);
        }
        assert!(!pane.has_unread(), "{turns} turns");
        let mut whole =
            fux_vt::Parser::with_options(size(2000, 2000), 1 << 10, OPTIONS).map_err(|source| {
                Error::Terminal {
                    size: size(2000, 2000),
                    source,
                }
            })?;
        let _ = whole.process(output);
        let (a, b) = (pane.screen(), whole.screen());
        assert!(a.rows().zip(b.rows()).all(|(a, b)| a.cells().eq(b.cells())));
        Ok(())
    }

    /// A frame whose reading spends a parser call's budget is read on over
    /// the next turns, its rest first, and the pane says it is part way
    /// through one till it ends; what came after it is read after.
    #[test]
    fn a_frame_past_a_budget_is_read_on_whole() -> Result<(), Error> {
        let mut pane = Pane::new(PaneId::of(1), "sh".into(), Runs::Shell, size(50, 200), 0)?;
        let mut frame = b"\x1b[?2026h".to_vec();
        frame.extend(spends_the_budget(50, 200));
        frame.extend_from_slice(b"\x1b[2J\x1b[HDONE\x1b[?2026lZ");
        pane.output(&frame);
        assert!(pane.mid_frame(), "the budget stopped no call in the frame");
        let mut turns = 0u32;
        while pane.has_unread() && turns < 100 {
            pane.read_unread();
            turns = turns.saturating_add(1);
        }
        assert!(!pane.mid_frame());
        assert_eq!(first_row(&pane), "DONEZ");
        Ok(())
    }

    /// A frame begun in output read a turn after it came is held its full
    /// timeout from when it is read, not from when the output came.
    #[test]
    fn a_frame_begun_in_output_read_later_is_held_its_timeout() -> Result<(), Error> {
        let mut out = spends_the_budget(50, 200);
        out.extend_from_slice(b"\x1b[?2026hY");
        let mut pane = pane(50, 200)?;
        pane.output(&out);
        assert!(pane.has_unread(), "the budget stopped no call");
        std::thread::sleep(FRAME_TIMEOUT.saturating_add(Duration::from_millis(100)));
        let mut turns = 0u32;
        while pane.has_unread() && turns < 100 {
            pane.read_unread();
            turns = turns.saturating_add(1);
        }
        let due = pane.frame_deadline();
        assert!(due.is_some_and(|due| due > Instant::now()), "{due:?}");
        Ok(())
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

    /// A frame ends at whatever resets synchronized output, as the parser
    /// reads it: RM with several parameters (ECMA-48 8.3.106, DECRST's
    /// `Pm`), one with a C0 control inside it, RIS, DECSTR, XTRESTORE, and
    /// an ESU that ends an OSC; but not at an ESU whose CSI CAN cancels,
    /// nor at a parameter with a subparameter, which `set_modes` skips.
    #[test]
    fn a_frame_ends_at_any_reset_of_synchronized_output() -> Result<(), Error> {
        for end in [
            &b"\x1b[?25;2026l"[..],
            b"\x1b[?20\r26l",
            b"\x1bc",
            b"\x1b[!p",
            b"\x1b[?2026r",
            // ESC ends the string (ST begins so) and starts the ESU.
            b"\x1b]2;\x1b[?2026l",
        ] {
            let mut pane = pane(3, 30)?;
            pane.output(&[&b"\x1b[?2026hx"[..], end].concat());
            assert_eq!(pane.frame_deadline(), None, "{end:?}");
            assert!(!pane.screen().mode(Mode::SynchronizedOutput), "{end:?}");
        }
        for held in [&b"\x1b[?20\x1826l"[..], b"\x1b[?2026:1l"] {
            let mut pane = pane(3, 30)?;
            pane.output(&[&b"\x1b[?2026hx"[..], held].concat());
            assert!(pane.frame_deadline().is_some(), "{held:?}");
            assert_eq!(first_row(&pane), "", "{held:?}");
        }
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
        assert_eq!(pane.frame_deadline(), Some(after(t0, FRAME_TIMEOUT)));
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
        // The cursor's row: a reflow may bring history down above it, and
        // join the wrapped row history ends with to it.
        let (y, _) = pane.screen().cursor_position();
        let row: String = (0..20)
            .filter_map(|x| pane.screen().cell(y, x))
            .map(|c| c.contents().chars().next().unwrap_or(' '))
            .collect();
        assert!(row.trim_end().ends_with('w'), "a resize reads it first");
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

    /// Replies past what one read queues are dropped, and that is told:
    /// once for reads that drop them one after another, and again for a
    /// later drop after a read that dropped none.
    #[test]
    fn replies_past_a_reads_bound_are_told_dropped() -> Result<(), Error> {
        let mut pane = pane(3, 20)?;
        pane.output(b"\x1b[6n");
        assert!(!pane.input.lost_reply());
        let queries: Vec<u8> = std::iter::repeat_n(&b"\x1b[6n"[..], REPLIES_MAX)
            .flatten()
            .copied()
            .collect();
        pane.output(&queries);
        assert!(pane.input.lost_reply());
        pane.input.drain_all();
        pane.output(&queries);
        assert!(!pane.input.lost_reply(), "the same run of drops");
        pane.input.drain_all();
        pane.output(b"\x1b[6n");
        pane.output(&queries);
        assert!(pane.input.lost_reply(), "a drop after none");
        Ok(())
    }

    /// A title or notification longer than a pane keeps is cut there, and
    /// says so, as is one longer than fux-vt keeps of a string; one that
    /// fits is whole. The guide states the bounds.
    #[test]
    fn what_is_cut_of_a_title_is_said() -> Result<(), Error> {
        let mut pane = pane(3, 20)?;
        let (long, fits) = (
            std::iter::repeat_n('t', TITLE_MAX + 1).collect::<String>(),
            std::iter::repeat_n('n', NOTIFICATION_MAX).collect::<String>(),
        );
        pane.output(format!("\x1b]2;{long}\x07\x1b]9;{fits}\x07").as_bytes());
        assert_eq!(
            pane.title.as_str(),
            long.get(..TITLE_MAX).unwrap_or_default()
        );
        assert!(pane.title.said().cut());
        let notification = pane.alerts.last.as_ref();
        assert_eq!(
            notification.map(|n| (n.as_str(), n.cut())),
            Some((&*fits, false))
        );
        let huge = std::iter::repeat_n('x', fux_vt::OSC_PAYLOAD_LIMIT).collect::<String>();
        pane.output(format!("\x1b]2;{huge}\x07").as_bytes());
        assert_eq!(
            pane.title.as_str(),
            huge.get(..TITLE_MAX).unwrap_or_default()
        );
        assert!(pane.title.said().cut());
        for bound in [TITLE_MAX, NOTIFICATION_MAX] {
            let said = format!("first {bound} characters");
            assert!(crate::help::JSON.contains(&said), "{said}");
        }
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

    /// Each shell command that ends goes in the journal with its own
    /// status, though several end in one read; and a bell and a title
    /// with them, in order.
    #[test]
    fn commands_ended_in_one_read_are_journaled_each() -> Result<(), Error> {
        let mut pane = pane(3, 30)?;
        pane.output(
            b"\x1b]133;C\x07x\x1b]133;D;1\x07\x07\x1b]133;C\x07\x1b]133;D;0\x07\x1b]2;t\x07",
        );
        let said: Vec<String> = pane.said.iter().map(|what| format!("{what:?}")).collect();
        assert_eq!(
            said,
            [
                "Done(Some(1))",
                "Bell",
                "Done(Some(0))",
                "Title(Said { text: \"t\", cut: false })"
            ]
        );
        Ok(())
    }
}

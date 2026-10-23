//! A pane: its terminal emulator, its process, and the input waiting for it.
use crate::bytes::ByteQueue;
use crate::id::PaneId;
use crate::process::{Child, Ended, Pid};
use crate::{after, classify::Echo, log, outer::Colours};
use fux_vt::{Event, Feature, Print, Row, RowId, Screen, Size};
use fuxix::process::Activity;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
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
    /// The queue is full, or refusing until the program reads.
    NotReading,
    /// No terminal of this size.
    Terminal { size: Size, source: fux_vt::Error },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotReading => f.write_str(
                "the pane's program is not reading its input; nothing more is queued until it does",
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
    /// How many bytes it holds, not yet written to the program.
    pub fn len(&self) -> usize {
        let held = self.held.as_ref().map_or(0, |typed| typed.line.len());
        self.bytes.len().saturating_add(held)
    }
    /// How many bytes more it takes now.
    pub fn room(&self) -> usize {
        match self.refusing {
            true => 0,
            false => INPUT_BYTES.saturating_sub(self.len()),
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
    /// Sets it from what a program sent, without control characters, at
    /// most 256 characters of it; whether it changed.
    fn set(&mut self, title: &[u8]) -> bool {
        let text = String::from_utf8_lossy(title);
        self.become_(text.chars().filter(|c| !c.is_control()).take(256).collect())
    }
    fn become_(&mut self, title: String) -> bool {
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
        self.pushed
            .pop_back()
            .is_some_and(|title| self.become_(title))
    }
}

/// Replies (DSR, DA) and events the parser produces while reading output.
struct Sink<'a> {
    /// The replies to one read, queued together after it: at most 4 KiB.
    replies: Vec<u8>,
    /// Its bells, notifications and titles, in order, for the journal.
    happened: &'a mut Vec<log::Event>,
    title: &'a mut Title,
    /// Set by a bell (BEL).
    bell: &'a mut bool,
    alerts: &'a mut Alerts,
    /// Set by an OSC 52 write of UTF-8 text, at most `MAX_CLIPBOARD`
    /// encoded.
    copied: &'a mut Option<String>,
    /// What colour queries are answered with (`outer`).
    colours: &'a Colours,
}

impl Sink<'_> {
    /// The title changed: the journal is told.
    fn titled(&mut self) {
        let title = self.title.as_str().to_owned();
        self.happened.push(log::Event::Title(title));
    }
}

impl fux_vt::Sink for Sink<'_> {
    fn reply(&mut self, bytes: &[u8]) {
        if self.replies.len().saturating_add(bytes.len()) <= 4096 {
            self.replies.extend_from_slice(bytes);
        }
    }
    /// A colour query (OSC 10, 11) is answered if the colour is known.
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Title(title) => {
                if self.title.set(title) {
                    self.titled();
                }
            }
            Event::ColorQuery { number, bel } => {
                if let Some(answer) = self.colours.answer(number, bel) {
                    fux_vt::Sink::reply(self, &answer);
                }
            }
            Event::Bell => {
                self.happened.push(log::Event::Bell);
                *self.bell = true;
                self.alerts.rung = self.alerts.rung.saturating_add(1);
            }
            Event::Notification(text) => {
                self.alerts.rung = self.alerts.rung.saturating_add(1);
                let text = String::from_utf8_lossy(text);
                let text: String = text.chars().filter(|c| !c.is_control()).take(500).collect();
                self.happened.push(log::Event::Notification(text.clone()));
                self.alerts.last = Some(text);
            }
            Event::Clipboard { data, .. } if data.len() <= crate::copy::MAX_CLIPBOARD => {
                let text = crate::json::unbase64(data).and_then(|d| String::from_utf8(d).ok());
                if let Some(text) = text {
                    *self.copied = Some(text);
                }
            }
            Event::IconName(_) | Event::Clipboard { .. } => {}
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
    /// program's own: a job the shell runs.
    pub fn job(&self) -> Option<Pid> {
        let child = self.child()?;
        let group = fuxix::terminal::foreground_group(&child.master)?;
        (group != child.leader.pid()).then_some(group)
    }

    /// The program's terminal hung up: it is read no more, and waits to be
    /// reaped.
    pub fn stop_reading(&mut self) {
        *self = match std::mem::replace(self, Self::Absent) {
            Self::Reading(child) | Self::HungUp(child) => Self::HungUp(child),
            other @ (Self::Absent | Self::Exited(_)) => other,
        };
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

/// Shells a pane may run, taking commands typed at them.
const SHELLS: &[&str] = &[
    "bash", "zsh", "fish", "sh", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "elvish", "xonsh",
];

/// Programs that run what they are given on another machine and show it
/// here (remote clients). What runs there is not theirs to probe, nor
/// fux's: unless the client reports it (`OSC 133 ; S`, as `koh connect`
/// does), whether they read their terminal says nothing of it.
const REMOTE: &[&str] = &[
    "ssh",
    "slogin",
    "autossh",
    "mosh",
    "mosh-client",
    "et",
    "telnet",
    "rlogin",
    "rsh",
];

/// A remote client in a pane's foreground that reports nothing of what it
/// runs, and how: a command on a plain pipe, or a terminal there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    /// Its program's name (`ssh`).
    pub name: String,
    /// It shows a terminal there (the local one raw, as for a session or
    /// `ssh -t`), not one command's output on a pipe.
    pub terminal: bool,
}

/// Whether `argv` runs a shell taking commands typed at it: no -c and no
/// script (`bash`, `bash --norc -i`).
fn takes_commands(argv: &[String]) -> bool {
    argv.split_first().is_some_and(|(p, args)| {
        let interactive = args.iter().all(|a| a.starts_with('-') && a != "-c");
        interactive && SHELLS.contains(&basename(p).trim_start_matches('-'))
    })
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
    /// Bells and notifications together, as `wait-bell` counts them.
    pub rung: u64,
    pub last: Option<String>,
}

/// What of an action's input its program, exited, did not read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Dropped {
    #[default]
    None,
    /// Held by fux or in its terminal as it exited: it went nowhere.
    Unread(Vec<String>),
    /// A relay's, sent on after the key or line that may have ended its
    /// program, which its relay never said it read: it likely went
    /// nowhere.
    Unanswered(Vec<String>),
}

/// A piece of an action's input, as an answer names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    /// Text typed.
    Text(String),
    /// Anything else, by its bytes and its name: a key (`C-q`), a paste,
    /// a mouse event.
    Named(usize, String),
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
    /// from BSU on, since when (its timeout's start), where the parser will
    /// end it, and when its first bytes came from the program (what its
    /// output is told as of, `classify`); see [`Pane::output`].
    frame: Option<(Vec<u8>, Instant, fux_vt::FrameScan, Instant)>,
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
    /// The program rang the bell since the session last looked
    /// (`Session::ring`).
    pub bell: bool,
    /// The text the program last put on the clipboard (OSC 52) since the
    /// session last looked, which copies it (`Session::copied`).
    pub copied: Option<String>,
    pace: Pace,
    /// The screen as it was last seen to change (a mark of it, and the
    /// cursor), and since when it has shown so: a program that redraws
    /// the same screen changes no row, and has not changed it.
    shown: (fux_vt::Mark, (u16, u16)),
    pub changed_at: Instant,
    /// When input last went to the program, if any has: it is busy with
    /// it at first, even while still blocked reading (the kernel has yet
    /// to wake it).
    pub written_at: Option<Instant>,
    /// When input went in that found it still, drawing nothing on its own
    /// lately (more input while at work keeps it): what it draws after (a
    /// spinner) is its work on that input, no clock it keeps.
    work_from: Option<Instant>,
    /// The last input typed ended a line (Enter, or text ending in a line
    /// end): what came of it is a command's, if its program marks them.
    pub line_ended: bool,
    /// The last piece of input written ended in an escape sequence begun
    /// and not finished (a lone Escape, `ESC [`): a program telling a lone
    /// Escape from a sequence may wait a moment for the rest, on a timer
    /// fux need not see (`ready::ESCAPE`).
    pub escape_open: bool,
    /// The last action's input ended in an edit of the line (text, a cursor
    /// key): where the cursor is next matters.
    pub edited: bool,
    /// The last action's input, each piece's bytes and how it is named
    /// (`C-q`, `"text"`): what its program had yet to read as it exited.
    pub sent: Vec<Sent>,
    /// The bytes of the last action's input written, and how many of
    /// them had left fux as a relay's state report said its program read
    /// again (`OSC 133 ; S`, which a relay sends once its program took the
    /// input): what it read.
    written: (usize, usize),
    /// Its relay said its program ended having read the input that had
    /// left fux then (`OSC 133 ; S ; activity=ended`): what of the action's
    /// input came after went nowhere.
    relay_ended: bool,
    /// What of the last action's input its program had not read as it
    /// exited, which went nowhere, or, a relay's, what its relay never
    /// said it read ([`Pane::lose_unread`]).
    pub lost: Dropped,
    /// The last state report its program relayed (`OSC 133 ; S`), by where
    /// it came among the marks, when fux read it, and the terminal's
    /// foreground group then: the relay's, while it runs.
    pub reported_at: Option<(u64, Instant, Option<fuxix::process::Pid>)>,
    /// How many times in a row its program drew on its own (a clock,
    /// `top`), as of the commands its marks had begun and ended then: a
    /// change a moment after the last (`ready::SETTLE`), no input written
    /// since that one. A change after a longer quiet (`ready::REDRAWN`), or
    /// another command, counts again from none.
    drew_alone: (u64, u32),
    /// What happened in it, in order, and what each piece of output
    /// printed: its journal (`crate::log`), which waits read.
    pub record: log::Log,
    /// Where the last action's input began (or the program started):
    /// what came since is what it brought.
    pub since: Start,
    /// The screen's rows as they read then, by row: a line printed again
    /// to read as it did brings nothing new; and where each was and what
    /// it showed ([`shows`]): a row drawn again to show as it did is no
    /// change. Shared with each wait that counts from then, which keeps
    /// them though another input comes.
    pub before: Before,
    /// The rows printed on since then, as they read after the last piece
    /// that printed on them: what the next piece finds there.
    seen: HashMap<RowId, Cells>,
    /// What each row showed as the screen was last seen to change.
    shown_rows: HashMap<RowId, (u16, u64)>,
    /// Where each piece of output came from (`crate::classify`).
    classes: crate::classify::Classes,
    /// The bytes of output read since the last action's input, as far as
    /// `REDRAWS`: within them, a line drawn again is told from news.
    redraws: usize,
    /// The nonces its shell's marks carry, and its programs', if fux
    /// marked its shell: then they are the authority on what it reads and
    /// runs.
    pub nonce: Option<crate::marks::Nonces>,
    /// How many times what the screen shows has changed: a wait for a change
    /// past one read earlier waits for it to grow.
    pub revision: u64,
    /// The mouse button `send-mouse press` holds down until its release: a
    /// `move` meanwhile is a drag.
    pub mouse_held: Option<fux_vt::keys::mouse::MouseButton>,
    /// How many times its program was started again (`respawn`), each on a
    /// screen of its own: a wait on the program before ends with it.
    pub starts: u64,
    pub alerts: Alerts,
    /// Where everything the program writes is kept too (`log-to`, `start-log`).
    pub log: Option<std::fs::File>,
}

/// How long a piece of input is given before the next is written, if
/// the program was not reading as it went in: longer than a program waits
/// to tell a lone Escape from one that begins a sequence, and than a burst
/// of input it takes as a paste, as typing would leave.
pub const PACE: Duration = Duration::from_millis(30);
/// How long a piece of input is given at most for a program that was
/// reading to take it: ncurses waits a second for an Escape's sequence to
/// go on.
pub const TAKE: Duration = Duration::from_secs(1);
/// How often a program is asked whether it took a piece of input.
const POLL: Duration = Duration::from_millis(2);
/// How long after a piece is written the program may be taken to have it
/// without drawing anything: the kernel hands it on within that.
const FLOOR: Duration = Duration::from_millis(2);

/// Where a wait counts from: a number in the pane's journal, and then, how
/// many shell commands had begun and ended and whether the full screen was
/// in use.
#[derive(Clone, Copy, Debug, Default)]
pub struct Start {
    pub from: log::Seq,
    pub started: u64,
    pub finished: u64,
    pub on_full: bool,
    /// Where the cursor was: its row and column.
    pub cursor: (u16, u16),
    /// It is an action's input, not the program's start: a command marked
    /// after it may be one that input began, which a wait waits for.
    pub input: bool,
}

impl Start {
    /// Counting from `from`, `screen` as it is now; `input`, an action's.
    pub fn at(from: log::Seq, screen: &Screen, input: bool) -> Start {
        Start {
            from,
            started: screen.commands().started,
            finished: screen.commands().finished,
            on_full: screen.mode(fux_vt::Mode::AlternateScreen),
            cursor: screen.cursor_position(),
            input,
        }
    }
}

/// Whether `bytes` end in an escape sequence begun and not finished: an
/// Escape alone, or a CSI (`ESC [`) or SS3 (`ESC O`) with no final byte.
fn escape_open(bytes: &[u8]) -> bool {
    let Some(at) = bytes.iter().rposition(|b| *b == 0x1b) else {
        return false;
    };
    match bytes.get(at.saturating_add(1)..).unwrap_or_default() {
        [] | [b'O'] => true,
        [b'[', rest @ ..] => rest.iter().all(|b| (0x20..=0x3f).contains(b)),
        _ => false,
    }
}

/// A piece of input: its bytes, what it is to the program that reads it,
/// and, after a pause, how long after the piece before it is written,
/// whatever the program did meanwhile (`gap`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub bytes: Vec<u8>,
    pub unit: log::Unit,
    pub gap: Option<Duration>,
}

impl Piece {
    /// A piece written once the program took the one before.
    pub fn new(bytes: Vec<u8>, unit: log::Unit) -> Piece {
        Piece {
            bytes,
            unit,
            gap: None,
        }
    }
}

/// Input written a piece at a time (`log::Unit`), each once the
/// program took the one before (it reads its terminal again, every key
/// read and its output read by fux), or once its time is up: so each
/// piece of output follows the input it answers (`crate::classify`).
#[derive(Default)]
pub struct Pace {
    /// Pieces waiting to be written.
    waiting: VecDeque<Piece>,
    /// When the last piece was written, until when it is given, and
    /// whether it was a lone Escape, which a program must surely have
    /// taken alone before the next piece.
    /// Whether what follows it is told exactly, needing no pace: the
    /// kernel's echo of it known, or a full-screen program's screen.
    written: Option<(Instant, Instant, bool, bool)>,
    /// Until when the next piece waits for output the program wrote before
    /// it to be read: output that pours on goes on being written then.
    drained_by: Option<Instant>,
    /// When to look again.
    until: Option<Instant>,
}

/// After the shell's output has been quiet this long, it is taken to be
/// waiting at its prompt. Output within one burst -- a prompt drawn in
/// pieces, lines of a startup message -- comes much closer together than
/// this, while a person notices nothing shorter.
pub const QUIET: Duration = Duration::from_millis(50);

/// How many bytes of output after an input are read for lines drawn again
/// as they read (each row printed on is read after each piece): a screen's
/// redraws, many times over. Past them, in a flood, every run printed is
/// news to a wait, and costs no more than recording it.
const REDRAWS: usize = 1 << 20;

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
pub fn size(rows: u16, cols: u16) -> Size {
    let one = |n| std::num::NonZeroU16::new(n).unwrap_or(std::num::NonZeroU16::MIN);
    Size::from((one(rows), one(cols)))
}

impl Pane {
    pub fn new(
        id: PaneId,
        name: String,
        runs: Runs,
        size: Size,
        history: usize,
    ) -> Result<Pane, Error> {
        let mut parser = fux_vt::Parser::with_options(size, history, OPTIONS)
            .map_err(|source| Error::Terminal { size, source })?;
        // What it prints is recorded (`Pane::record`).
        parser.record_prints(true);
        let mark = parser.screen().mark();
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
            bell: false,
            copied: None,
            pace: Pace::default(),
            shown: (mark, (0, 0)),
            changed_at: Instant::now(),
            written_at: None,
            work_from: None,
            line_ended: false,
            escape_open: false,
            edited: false,
            sent: Vec::new(),
            written: (0, 0),
            relay_ended: false,
            lost: Dropped::default(),
            reported_at: None,
            drew_alone: (0, 0),
            record: log::Log::new(log::JOURNAL_BYTES),
            since: Start::default(),
            before: Rc::default(),
            seen: HashMap::new(),
            shown_rows: HashMap::new(),
            classes: crate::classify::Classes::default(),
            redraws: 0,
            nonce: None,
            revision: 0,
            starts: 0,
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
        if self.has_unread() {
            self.unread.0.extend_from_slice(bytes);
            return;
        }
        self.read(bytes, now, now);
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
        // Told as of when it came, as the kernel's echo is looked for then;
        // a frame it begins is timed from now.
        let came = self.unread.1;
        let unread = std::mem::take(&mut self.unread.0);
        // Only the work of a sequence read already: a call of no bytes does
        // it, and stops again if that spends its budget.
        if unread.is_empty() {
            let mut happened = Vec::new();
            if self.parse(&[], false, &mut happened).is_some() {
                self.wait_unread(Vec::new(), came);
            }
            return;
        }
        let frame_left = std::mem::take(&mut self.frame_left);
        let at = frame_left.min(unread.len());
        let (frame, after) = unread.split_at_checked(at).unwrap_or((&unread, &[]));
        if let Some((taken, _)) = self.feed(frame, false, came) {
            self.frame_left = frame.len().saturating_sub(taken);
            self.wait_unread(unread.get(taken..).unwrap_or_default().to_vec(), came);
            return;
        }
        self.read(after, Instant::now(), came);
    }

    /// `left` waits to be read on the pane's next turn, from `now`.
    fn wait_unread(&mut self, left: Vec<u8>, now: Instant) {
        self.unread = (left, now);
    }

    /// Reads `bytes`, which came at `came`, as [`Pane::output_at`] does at
    /// `now`, with no output waiting.
    fn read(&mut self, bytes: &[u8], now: Instant, came: Instant) {
        let mut rest = bytes;
        loop {
            if let Some((held, _, scan, began)) = &mut self.frame {
                let began = *began;
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
                if let Some((taken, _)) = self.feed(&frame, false, began) {
                    let left = frame.get(taken..).unwrap_or_default();
                    self.frame_left = left.len();
                    self.wait_unread([left, rest].concat(), began);
                    return;
                }
            } else {
                match self.feed(rest, true, came) {
                    None => return,
                    Some((end, fux_vt::Stop::Begun)) => {
                        self.frame = Some((Vec::new(), now, self.parser.frame_scan(), came));
                        rest = rest.get(end..).unwrap_or_default();
                    }
                    Some((end, _)) => {
                        self.wait_unread(rest.get(end..).unwrap_or_default().to_vec(), came);
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
            .map(|(_, since, ..)| after(*since, FRAME_TIMEOUT))
    }

    /// Reads the held frame now, ended or not.
    pub fn release_frame(&mut self) {
        if let Some((held, _, _, began)) = self.frame.take()
            && let Some((taken, _)) = self.feed(&held, false, began)
        {
            // No output waits while a frame is held: it is the first.
            let left = held.get(taken..).unwrap_or_default().to_vec();
            self.frame_left = left.len();
            self.wait_unread(left, began);
        }
    }

    /// Gives `bytes` to the screen, stopping after a BSU if `until_frame`,
    /// and where a call of the parser did all the work one may
    /// (`Stop::Budget`). Returns how many bytes were read if it stopped,
    /// and why (`Stop::Begun` or `Stop::Budget`). They are read a
    /// piece at a time, each ending at a bell or a shell mark, so that
    /// each of those has its place among the pieces; each piece is told
    /// (`crate::classify`), as of `came`, when fux read it from the
    /// program, numbered as the screen took it, and recorded.
    fn feed(
        &mut self,
        bytes: &[u8],
        until_frame: bool,
        came: Instant,
    ) -> Option<(usize, fux_vt::Stop)> {
        let now = Instant::now();
        let mut done = 0usize;
        let begun = loop {
            let rest = bytes.get(done..).unwrap_or_default();
            let marked = self.nonce.is_some();
            let piece = rest.get(..cut(rest, marked)).unwrap_or_default();
            let runs = (self.classes).classify(self.parser.screen(), marked, piece, came);
            let mut start = 0usize;
            let mut begun = None;
            for &(end, origin) in &runs {
                let run = piece.get(start..end).unwrap_or_default();
                if let Some((taken, stop)) = self.record(run, origin, until_frame) {
                    // The rest of the piece is read again later, and told
                    // again then.
                    let at = start.saturating_add(taken);
                    self.classes.unread(piece, &runs, at);
                    begun = Some((done.saturating_add(at), stop));
                    break;
                }
                start = end;
            }
            done = done.saturating_add(piece.len());
            if begun.is_some() || done >= bytes.len() || piece.is_empty() {
                break begun;
            }
        };
        self.input.heard(Instant::now);
        // A report read now, which came after any input written before.
        let report = self.screen().commands().report;
        if let Some(fux_vt::Report { mark, activity }) = report
            && Some(mark) != self.reported_at.map(|(mark, ..)| mark)
        {
            let group = (self.process.child())
                .and_then(|child| fuxix::terminal::foreground_group(&child.master));
            self.reported_at = Some((mark, now, group));
            // Its program reads again, or ended having read every key
            // its relay sent it: it read the input that left fux.
            use fux_vt::Reported::{Ended, Idle, Polling, Reading};
            if matches!(activity, Reading | Polling | Idle | Ended) {
                self.written.1 = self.written.0.saturating_sub(self.input.len());
            }
            self.relay_ended |= activity == Ended;
        }
        begun
    }

    /// Gives `run`, which came from `origin`, to the screen, as `feed`
    /// does, and records it: its number and what it printed, an entry kept
    /// only if it printed something; the rows it let go, a bell it rang.
    fn record(
        &mut self,
        run: &[u8],
        origin: log::Origin,
        until_frame: bool,
    ) -> Option<(usize, fux_vt::Stop)> {
        if run.is_empty() {
            return None;
        }
        // Text typed ahead may be drawn again by a line editor reading it,
        // as things stand before the run: a shell fux marks at its prompt,
        // as they say; another program, with the terminal raw.
        let reads = self.classes.ahead()
            && match self.nonce {
                Some(_) => self.at_prompt(),
                None => (self.process.child())
                    .and_then(|c| fuxix::terminal::attributes(&c.master).ok())
                    .is_some_and(|t| !t.modes().canonical),
            };
        let finished = self.screen().commands().finished;
        let mut happened = Vec::new();
        let begun = self.parse(run, until_frame, &mut happened);
        let seq = log::Seq(self.screen().version());
        let prints = self.parser.take_prints();
        let printed = (!prints.is_empty()).then(|| {
            let screen = self.parser.screen();
            let mut echo: Vec<_> = (prints.iter())
                .map(|print| {
                    let line =
                        || (screen.row_by_id(print.row)).map_or(print.row, |r| line_top(r).id());
                    self.classes.echo(origin, &print, line, reads)
                })
                .collect();
            // A line drawn again as it read is no news, as far as the
            // first `REDRAWS` bytes after an input tell: past them (a flood)
            // every run is, and the rows it printed on are not read.
            // Nor one that printed more than it records: what it changed
            // is unknown (`Prints::lost`).
            let tell = self.redraws < REDRAWS && !prints.lost();
            self.redraws = self.redraws.saturating_add(run.len());
            let held = |p: &Print<'_>| {
                tell.then(|| (self.seen.get(&p.row)).or(self.before.get(&p.row).map(|b| &b.0)))
                    .flatten()
            };
            // What each run changed, told only within `REDRAWS`: past it
            // every run is whole (`Printed`).
            let fresh = match tell {
                true => (prints.iter().zip(&echo))
                    .map(|(p, e): (Print<'_>, &Echo)| {
                        let echo = *e != Echo::Not;
                        held(&p).map_or((0, p.text.chars().count()), |row| changed(row, &p, echo))
                    })
                    .collect(),
                false => Vec::new(),
            };
            if echo.iter().all(|e| *e == Echo::Not) {
                echo = Vec::new();
            }
            // Each row printed on, once, as it reads now, if it is on the
            // screen (found in one map of its rows: a row in history is
            // printed on no more, and finding one there would read all of
            // history); rows scrolled off since are let go.
            if tell {
                let rows: HashMap<_, _> = live(screen).map(|(_, row)| (row.id(), row)).collect();
                let mut last = None;
                for print in prints.iter() {
                    if last.replace(print.row) == Some(print.row) {
                        continue;
                    }
                    if let Some(row) = rows.get(&print.row) {
                        self.seen.entry(print.row).or_default().read(row);
                    }
                }
                prune(&mut self.seen, screen);
            } else {
                self.seen.clear();
            }
            let mut prints = prints;
            prints.shrink_to_fit();
            Box::new(log::Printed {
                prints,
                echo,
                fresh,
            })
        });
        // A piece that printed nothing has no reader in the journal.
        if let Some(printed) = printed {
            self.record.push(seq, log::Event::Output(printed));
        }
        // What else the piece did, after it, in the order it came; a shell
        // command's end, by the shell's marks, with the last one's status.
        let commands = self.screen().commands();
        let ended = commands.finished.saturating_sub(finished);
        happened.extend((0..ended.min(16)).map(|_| log::Event::Done(commands.last_status)));
        for event in happened {
            let seq = log::Seq(self.parser.tick());
            self.record.push(seq, event);
        }
        begun
    }

    /// Gives `bytes` to the screen, as `feed` does. Never inlined: the
    /// parser's loop compiles tighter on its own than inside `record`.
    #[inline(never)]
    fn parse(
        &mut self,
        bytes: &[u8],
        until_frame: bool,
        happened: &mut Vec<log::Event>,
    ) -> Option<(usize, fux_vt::Stop)> {
        let mut sink = Sink {
            replies: Vec::new(),
            happened,
            title: &mut self.title,
            bell: &mut self.bell,
            alerts: &mut self.alerts,
            copied: &mut self.copied,
            colours: &self.colours,
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
        let replies = sink.replies;
        if !replies.is_empty() {
            self.input.reply(&replies);
        }
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
                Ok(n) => {
                    self.input.advance(n);
                    self.written_at = Some(Instant::now());
                }
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
            // The number the resize gave every row it kept.
            let seq = log::Seq(self.screen().version());
            self.record.push(seq, log::Event::Resize(size));
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

    /// Makes the pane's shell marks count only with `nonce`'s, a program's
    /// within a command with its nested one (`None`: fux marked no shell in
    /// it, and they carry no authority).
    pub fn mark_with(&mut self, nonce: Option<crate::marks::Nonces>) {
        self.parser.require_mark_nonce(
            nonce.as_ref().map(|n| n.shell.as_bytes()),
            nonce.as_ref().map(|n| n.nested.as_bytes()),
        );
        self.nonce = nonce;
    }

    /// An action's input begins: what comes after is what it brought.
    pub fn note_input(&mut self) {
        self.count_from(true);
    }

    /// The program starts: what comes after is what it brought, no input
    /// of an action's among it.
    pub fn note_start(&mut self) {
        self.count_from(false);
    }

    /// What comes after now is new to a wait; `input`, an action's input
    /// brings it.
    fn count_from(&mut self, input: bool) {
        self.since = Start::at(log::Seq(self.parser.tick()), self.parser.screen(), input);
        self.redraws = 0;
        self.before = Rc::new(
            (live(self.screen()))
                .map(|(y, row)| (row.id(), (row_cells(&row), (y, shows(&row)))))
                .collect(),
        );
        self.shown_rows = (self.before.iter())
            .map(|(id, (_, shows))| (*id, *shows))
            .collect();
        self.seen.clear();
        self.sent.clear();
        self.written = (0, 0);
        self.relay_ended = false;
    }

    /// Its program exits: what of the last action's input it had not read
    /// (still held by fux, or in its terminal) went nowhere. A relay reads
    /// it all: what it sent on after its program last said it read again,
    /// past the first key or line (which may have ended it), it never said
    /// its program read; once it said its program ended having read what it
    /// had (`ended`), the rest went nowhere. Named by the last pieces, a
    /// text by its end.
    pub fn lose_unread(&mut self) {
        let waiting = (self.process.child()).map_or(0, |c| c.master.keys_waiting());
        let unread = self.input_queued().saturating_add(waiting);
        let relay = self.screen().commands().report.is_some();
        let names = |tail: Vec<(bool, String)>| -> Vec<String> {
            tail.into_iter().map(|(_, name)| name).collect()
        };
        let sent = (self.sent.iter())
            .map(|s| match s {
                Sent::Text(text) => text.len(),
                Sent::Named(len, _) => *len,
            })
            .fold(0, usize::saturating_add);
        let unconfirmed = sent.saturating_sub(self.written.1);
        self.lost = match (unread, relay) {
            (_, true) if self.relay_ended => match unconfirmed {
                0 => Dropped::None,
                after => Dropped::Unread(names(self.named_tail(after))),
            },
            (0, false) => Dropped::None,
            (0, true) => {
                let mut tail = self.named_tail(unconfirmed).into_iter();
                // What came up to the first key or line end, and that.
                let ended = (tail.by_ref()).any(|(ends, _)| ends);
                match names(tail.collect()) {
                    after if ended && !after.is_empty() => Dropped::Unanswered(after),
                    _ => Dropped::None,
                }
            }
            (unread, _) => Dropped::Unread(names(self.named_tail(unread))),
        };
    }

    /// The last action's pieces of input its last `bytes` are of, named,
    /// and whether each ends a line or is a key.
    fn named_tail(&self, mut bytes: usize) -> Vec<(bool, String)> {
        let mut named = Vec::new();
        for sent in self.sent.iter().rev() {
            if bytes == 0 {
                break;
            }
            let (len, piece) = match sent {
                Sent::Text(text) => {
                    let from = text.len().saturating_sub(bytes);
                    let tail = (text.char_indices())
                        .find(|(at, _)| *at >= from)
                        .and_then(|(at, _)| text.get(at..))
                        .unwrap_or_default();
                    (
                        text.len(),
                        (tail.contains(['\r', '\n']), format!("{tail:?}")),
                    )
                }
                Sent::Named(len, name) => (*len, (true, name.clone())),
            };
            named.push(piece);
            bytes = bytes.saturating_sub(len);
        }
        named.reverse();
        named
    }

    /// Sends `units` of input in order, each written once the program took
    /// the one before (`Pace`): the first at once if none is waiting, the
    /// rest as the program takes them ([`Pane::pace_due`]). Refused whole,
    /// as `InputQueue::push` refuses, if they would not fit.
    pub fn send(&mut self, units: Vec<Piece>, now: Instant) -> Result<(), Error> {
        self.room_for(&units)?;
        self.edited = units.last().is_some_and(|p| p.unit == log::Unit::Edit);
        self.pace.waiting.extend(units);
        self.release_paced(now);
        Ok(())
    }

    /// Whether it takes `units` now, as `send` would: refused whole if
    /// they and what waits to be paced are more than its queue's room.
    pub fn room_for(&self, units: &[Piece]) -> Result<(), Error> {
        let held: usize = self.pace.waiting.iter().map(|p| p.bytes.len()).sum();
        let total = units
            .iter()
            .map(|p| p.bytes.len())
            .fold(held, usize::saturating_add);
        match total > self.input.room() {
            true => Err(Error::NotReading),
            false => Ok(()),
        }
    }

    /// When paced input is to be looked at again.
    pub fn pace_due(&self) -> Option<Instant> {
        self.pace.until.filter(|_| !self.pace.waiting.is_empty())
    }

    /// Writes the next piece of paced input if the program took the last
    /// one by `now`, or its time is up.
    pub fn release_paced(&mut self, now: Instant) {
        if self.pace.until.is_some_and(|until| until > now) {
            return;
        }
        while let Some(next) = self.pace.waiting.front() {
            // After a pause, written once it is up, whatever the program
            // did meanwhile: the gap is the agent's to time.
            if let Some(gap) = next.gap {
                let from = self.pace.written.map_or(now, |(at, ..)| at);
                if now < after(from, gap) {
                    self.pace.until = Some(after(from, gap));
                    return;
                }
                if let Some(Piece { bytes, unit, .. }) = self.pace.waiting.pop_front()
                    && !self.write_unit(bytes, unit, now)
                {
                    return;
                }
                continue;
            }
            let exact = self.pace.written.is_some_and(|(_, _, _, exact)| exact);
            // A lone Escape is taken alone, wherever: one with keys after it
            // reads as another key.
            let held = (self.pace.written).is_some_and(|(at, limit, escape, _)| {
                (escape || !exact) && now < limit && !self.taken(at, now, escape)
            });
            // Output the program wrote before now came before this piece:
            // it is read first (unless it pours on for a while), so that
            // nothing it printed is told as following the piece.
            let unread = !exact && (self.process.child()).is_some_and(|c| c.master.output_unread());
            let draining =
                !held && unread && now < *(self.pace.drained_by).get_or_insert(after(now, TAKE));
            if held || draining {
                self.pace.until = Some(after(now, POLL));
                return;
            }
            self.pace.drained_by = None;
            let Some(Piece { bytes, unit, .. }) = self.pace.waiting.pop_front() else {
                return;
            };
            // Paced where nothing exact tells what follows the piece. (A
            // shell fux marks draws the line typed before Enter as it reads
            // it, where nothing marks it apart from what follows: paced.)
            if !self.write_unit(bytes, unit, now) {
                return;
            }
        }
    }

    /// Whether rows of the main screen written since the last input were
    /// let go (past history-bytes, or out of a scroll region): what came
    /// since is not all on the screen or in history. What they printed is
    /// in the journal (`record`).
    pub fn cut(&self) -> bool {
        self.screen().let_go() > self.since.from.0
    }

    /// The text typed on the line since a key last submitted one.
    pub fn typed(&self) -> &str {
        self.classes.typed()
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

    /// Whether a shell command runs, as the marks say: never once the
    /// pane's program ended, which ended it too (a relay's remote shell
    /// gone with the relay, its last command never marked done).
    pub fn command_running(&self) -> bool {
        self.screen().commands().running() && self.process.exit_status().is_none()
    }

    /// Whether the pane's shell, marked by fux, is at its prompt, reading
    /// its command line, as its marks say.
    fn at_prompt(&self) -> bool {
        let commands = self.screen().commands();
        commands.prompt && !commands.running()
    }

    /// Whether the program took the input written at `at`, by `now`: every
    /// byte written and read, its output read by fux, and it reads its
    /// terminal again, with no time limit. After a lone Escape (`escape`),
    /// only where the system says it reads, not idle on something it does
    /// not name: a program telling a lone Escape from a key's sequence
    /// waits in a read with a timer.
    fn taken(&self, at: Instant, now: Instant, escape: bool) -> bool {
        let Process::Reading(child) = &self.process else {
            return true;
        };
        let drawn = self.changed_at > at || now >= after(at, FLOOR);
        // The program asked last, after the checks that read no `/proc`.
        let taking = || crate::ready::taking(self, child, now);
        // A lone Escape the program drew nothing for may wait on a timer
        // fux does not see (an alarm): given its moment first.
        let alone = !escape || self.changed_at > at || now >= after(at, crate::ready::ESCAPE);
        self.input.is_empty()
            && drawn
            && !child.master.output_unread()
            && !self.has_unread()
            && match taking() {
                Some(Activity::Reading) => alone,
                Some(Activity::Idle) => !escape,
                Some(
                    Activity::Busy | Activity::Polling | Activity::Waiting | Activity::Unknown,
                )
                | None => false,
            }
    }

    /// The remote client in the terminal's foreground, if one is and it
    /// reports nothing of what it runs.
    pub fn remote(&self) -> Option<Remote> {
        let child = self.process.child()?;
        if self.screen().commands().report.is_some() {
            return None;
        }
        let argv = match (self.process.job(), &self.runs) {
            (Some(job), _) => fuxix::process::argv(job).unwrap_or_default(),
            (None, Runs::Program(launch)) => launch.argv.clone(),
            (None, Runs::Shell) => return None,
        };
        let name = basename(argv.first()?);
        if !REMOTE.contains(&name.as_str()) {
            return None;
        }
        // One blocked reading its terminal alone (not watching it beside
        // its socket) asks at a prompt of its own, here: a password, a
        // host key. It reads as any program does (Linux; macOS does not
        // say, and takes it for the client).
        if fuxix::process::blocked_reading(&child.master, child.leader.pid()) {
            return None;
        }
        let modes = fuxix::terminal::attributes(&child.master).ok()?.modes();
        Some(Remote {
            name,
            terminal: !modes.canonical,
        })
    }

    /// Whether it runs a shell taking commands typed at it: its own, or one
    /// `run` started with no -c and no script (`run -- bash`).
    pub fn runs_shell(&self) -> bool {
        match &self.runs {
            Runs::Shell => true,
            Runs::Program(launch) => takes_commands(&launch.argv),
        }
    }

    /// Whether a shell that could mark commands is in front: the program
    /// in the terminal's foreground (a job the shell runs, else its own) is
    /// one, or, a relay's (`koh connect SERVER -- bash`), the program its
    /// words say it runs is one. A REPL, an editor, a debugger is not.
    pub fn shell_in_front(&self) -> bool {
        let relay = self.screen().commands().report.is_some();
        let argv = match (self.process.job(), &self.runs) {
            (Some(job), _) => fuxix::process::argv(job).unwrap_or_default(),
            (None, Runs::Program(launch)) => launch.argv.clone(),
            (None, Runs::Shell) => return true,
        };
        match relay {
            false => takes_commands(&argv),
            true => (0..argv.len()).any(|at| argv.get(at..).is_some_and(takes_commands)),
        }
    }

    /// Writes a piece of input, `unit` its kind, and tells the classifier
    /// of it (the terminal's modes then, the line the cursor is on, whether
    /// it is typed ahead), and sets the pace for the next. Whether what
    /// follows it is told exactly, needing no pace.
    fn write_unit(&mut self, bytes: Vec<u8>, unit: log::Unit, now: Instant) -> bool {
        // Replies or a resize report took the room `send` found: the piece
        // waits for the program to read.
        if bytes.len() > self.input.room() {
            let piece = Piece::new(bytes, unit);
            self.pace.waiting.push_front(piece);
            self.pace.until = Some(after(now, POLL));
            return false;
        }
        let escape = bytes == b"\x1b";
        let child = self.process.child();
        let taking = child.and_then(|child| crate::ready::taking(self, child, now));
        let reading = self.input.is_empty()
            && match taking {
                Some(Activity::Reading | Activity::Idle) => true,
                Some(
                    Activity::Busy | Activity::Polling | Activity::Waiting | Activity::Unknown,
                )
                | None => false,
            };
        // Typed ahead: a shell fux marks says whether it reads its line.
        // Another program's line typed ahead ends at Enter: what it draws
        // after is output (`docs/agents.md`, what fux cannot tell). Else
        // the keys go to a program that reads them at once or within a
        // time limit (a read with a timer, or one thread reading while
        // another sleeps).
        let ahead = match self.nonce {
            Some(_) => !self.at_prompt(),
            None => {
                unit == log::Unit::Edit
                    && match taking {
                        Some(Activity::Reading | Activity::Polling | Activity::Idle) => false,
                        Some(Activity::Busy | Activity::Waiting | Activity::Unknown) | None => true,
                    }
            }
        };
        let modes = child
            .and_then(|child| fuxix::terminal::attributes(&child.master).ok())
            .map(|t| t.modes());
        if unit == log::Unit::Submit {
            self.parser.note_submit();
        }
        let line = cursor_line(self.screen());
        let input = crate::classify::Input { unit, line, ahead };
        let exact = self.classes.wrote(input, &bytes, modes.as_ref(), now)
            || self.screen().mode(fux_vt::Mode::AlternateScreen);
        if !self.at_work(now) {
            self.work_from = (!self.redrawing(now)).then_some(now);
        }
        self.line_ended = matches!(bytes.last(), Some(b'\r' | b'\n'));
        self.escape_open = escape_open(&bytes);
        self.written.0 = self.written.0.saturating_add(bytes.len());
        let _ = self.input.push(&bytes); // room checked above
        let gap = if reading { TAKE } else { PACE };
        self.pace.written = Some((now, after(now, gap), escape, exact));
        self.pace.until = Some(after(now, if reading { POLL } else { PACE }));
        exact
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
        // A new program, its own pane: a wait counts from its start. Kept
        // are what the session has yet to take from the old one (a bell, a
        // copy), its colours, nonce, log and journal budget, and its counts.
        let mut fresh = Pane::new(self.id, name, runs, self.size(), history)?;
        fresh.mark_with(self.nonce.take());
        fresh.record.set_budget(self.record.budget);
        fresh.colours = std::mem::take(&mut self.colours);
        fresh.bell = self.bell;
        fresh.copied = self.copied.take();
        fresh.log = self.log.take();
        fresh.revision = self.revision.wrapping_add(1);
        fresh.starts = self.starts.wrapping_add(1);
        fresh.process = process;
        fresh.note_start();
        Ok(std::mem::replace(self, fresh).process)
    }

    /// Notes, after the screen was read into at `now`, whether what it
    /// shows changed.
    pub fn note_change(&mut self, now: Instant) {
        let screen = self.parser.screen();
        let cursor = screen.cursor_position();
        // Rows written to that show as they did change nothing.
        let mut changed = cursor != self.shown.1;
        // Once a change is seen, the other rows are not read: they show
        // anew when next written (an action's start reads them all again).
        for (y, row) in screen.dirty_live_rows_since(self.shown.0) {
            if changed {
                self.shown_rows.remove(&row.id());
                continue;
            }
            let now_shows = (y, shows(&row));
            changed |= self.shown_rows.insert(row.id(), now_shows) != Some(now_shows);
        }
        prune(&mut self.shown_rows, screen);
        self.shown = (screen.mark(), cursor);
        if changed {
            let gap = now.saturating_duration_since(self.changed_at);
            let commands = screen.commands();
            let marks = commands.started.wrapping_add(commands.finished);
            let n = match self.drew_alone {
                (counted, n) if counted == marks && gap < crate::ready::REDRAWN => n,
                _ => 0,
            };
            let alone = gap >= crate::ready::SETTLE
                && self.written_at.is_none_or(|at| at <= self.changed_at);
            self.drew_alone = (marks, n.saturating_add(u32::from(alone)));
            self.changed_at = now;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Whether its program redraws on its own at `now`: it drew so twice
    /// in a row at least, the last time within `ready::REDRAWN`.
    pub fn redrawing(&self, now: Instant) -> bool {
        self.drew_alone.1 >= 2 && now < after(self.changed_at, crate::ready::REDRAWN)
    }

    /// Whether its program is at work on input that found it still at
    /// `now`: it drew after it, the last time within `ready::REDRAWN`.
    pub fn at_work(&self, now: Instant) -> bool {
        self.work_from.is_some_and(|at| self.changed_at > at)
            && now < after(self.changed_at, crate::ready::REDRAWN)
    }

    /// How many bytes of input sent the program has not read yet.
    pub fn input_queued(&self) -> usize {
        let paced: usize = self.pace.waiting.iter().map(|p| p.bytes.len()).sum();
        paced.saturating_add(self.input.len())
    }

    /// Whether pacing still holds keys back from a running program.
    pub fn holds_keys(&self) -> bool {
        !self.pace.waiting.is_empty() && self.process.child().is_some()
    }

    /// Whether every piece of input sent has been written to the program.
    pub fn input_written(&self) -> bool {
        self.pace.waiting.is_empty() && self.input.is_empty()
    }

    pub fn label(&self) -> &str {
        match self.title.as_str() {
            "" => &self.name,
            title => title,
        }
    }
}

/// Where the first piece of `bytes` that `Pane::feed` reads alone ends:
/// just after a bell (BEL, or a run of them, as a terminal whose line is
/// full rings one for each key it drops), or, if `marks`, after a shell
/// mark (OSC 133, which fux's shells end with BEL); else [`PIECE`] bytes
/// at most. A BEL that ends another OSC (a title) is no bell. Only BEL is
/// searched for, output seldom holding one; and no further than a piece's
/// length, as the parser may stop sooner (a frame drawn in synchronized
/// output begins), and what follows is read again from there.
const PIECE: usize = 4096;

/// Where the first piece of `bytes` that `Pane::feed` reads alone ends.
fn cut(bytes: &[u8], marks: bool) -> usize {
    let bytes = bytes.get(..PIECE).unwrap_or(bytes);
    let mut from = 0usize;
    while let Some(at) = (bytes.get(from..).unwrap_or_default().iter())
        .position(|b| *b == 0x07)
        .map(|i| from.saturating_add(i))
    {
        let next = at.saturating_add(1);
        // An OSC holds no BEL: one this BEL ends began since the last.
        match osc_before(bytes.get(from..at).unwrap_or_default()) {
            // An OSC's end: a shell mark's, read alone; else a title's.
            Some(start) => {
                let start = from.saturating_add(start);
                let mark = bytes
                    .get(start..)
                    .is_some_and(|o| o.starts_with(b"\x1b]133;"));
                if marks && mark {
                    return next;
                }
                from = next;
            }
            None => {
                let run = bytes.get(at..).unwrap_or_default();
                return at.saturating_add(run.iter().take_while(|b| **b == 0x07).count());
            }
        }
    }
    bytes.len()
}

/// Where the OSC that `before`, which holds no BEL, ends inside began, if
/// it ends inside one (`ESC ]`, and no ST after it).
fn osc_before(before: &[u8]) -> Option<usize> {
    let mut end = before.len();
    loop {
        let at = before.get(..end)?.iter().rposition(|b| *b == b']')?;
        let start = at.checked_sub(1)?;
        if before.get(start) == Some(&0x1b) {
            // Ended since, by ST.
            let rest = before.get(at..).unwrap_or_default();
            let st = (rest.iter().zip(rest.iter().skip(1))).any(|(a, b)| (*a, *b) == (0x1b, b'\\'));
            return (!st).then_some(start);
        }
        end = at;
    }
}

/// The screen's rows as they read as an input began, by row, and where
/// each was and what it showed (`Pane::before`).
pub type Before = Rc<HashMap<RowId, (Cells, (u16, u64))>>;

/// A row's cells as they read: its text, a blank or placeholder a space,
/// and where in it each column starts (a wide character's second cell
/// where the next starts); none kept while each column is one byte, as in
/// an ASCII row, column N starting at byte N.
#[derive(Clone, Debug, Default)]
pub struct Cells {
    text: String,
    at: Vec<usize>,
}

impl Cells {
    /// Whether the row holds blanks alone.
    pub fn blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Whether the row reads as `before` did, and maybe more after it.
    pub fn starts_with(&self, before: &Cells) -> bool {
        self.text.starts_with(before.text.trim_end())
    }

    /// Reads `row` into these cells, in the room they have.
    fn read(&mut self, row: &Row<'_>) {
        self.text.clear();
        self.at.clear();
        for (col, c) in row.cells().enumerate() {
            let start = self.text.len();
            if !self.at.is_empty() || start != col {
                if self.at.is_empty() {
                    self.at.extend(0..col);
                }
                self.at.push(start);
            }
            if !c.is_wide_continuation() {
                self.text.push_str(c.shown());
            }
        }
    }
}

/// The cells of `row`.
pub fn row_cells(row: &Row<'_>) -> Cells {
    let mut cells = Cells::default();
    cells.read(row);
    cells
}

/// The text of `row`'s cells from column `col` on.
fn text_from(row: &Cells, col: u16) -> &str {
    let at = match row.at.is_empty() {
        true => usize::from(col).min(row.text.len()),
        false => (row.at.get(usize::from(col)).copied()).unwrap_or(row.text.len()),
    };
    row.text.get(at..).unwrap_or_default()
}

/// Whether `row`, a row's cells, holds what `print` printed at its
/// columns, a wide character's two cells as one.
pub fn holds_at(row: &Cells, print: &Print<'_>) -> bool {
    text_from(row, print.cols.0).starts_with(print.text)
}

/// The characters of `print` that change `row` (its cells just before):
/// none if it printed what was there; else, of a run that may be the echo
/// of the line typed (a line editor drawing it again, its prompt and all),
/// those from the first that differs from what stood in its cell to the
/// last; of other output, all of it (its row may have been erased since).
pub fn changed(row: &Cells, print: &Print<'_>, echo: bool) -> (usize, usize) {
    if !echo {
        return match holds_at(row, print) {
            true => (0, 0),
            false => (0, print.text.chars().count()),
        };
    }
    let there: Vec<char> = text_from(row, print.cols.0).chars().collect();
    let text: Vec<char> = print.text.chars().collect();
    let same = |i: &usize| text.get(*i) == there.get(*i);
    let first = (0..text.len()).find(|i| !same(i));
    let last = (0..text.len()).rev().find(|i| !same(i));
    match (first, last) {
        (Some(first), Some(last)) => (first, last.saturating_add(1)),
        _ => (0, 0),
    }
}

/// What `row` shows, its text and colours, as one number.
pub fn shows(row: &Row<'_>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for cell in row.cells() {
        cell.contents().hash(&mut hasher);
        cell.attributes().hash(&mut hasher);
    }
    hasher.finish()
}

/// The screen's rows, from the top, and where each is.
fn live(screen: &Screen) -> impl Iterator<Item = (u16, Row<'_>)> {
    let window = screen.window();
    (0..window.rows()).filter_map(move |y| Some((y, window.row(y)?)))
}

/// Past four times the screen's rows, keeps in `map` only the rows on the
/// screen: rows scrolled off are printed on no more.
pub fn prune<V>(map: &mut HashMap<RowId, V>, screen: &Screen) {
    if map.len() > usize::from(screen.size().rows()).saturating_mul(4) {
        let rows: std::collections::HashSet<_> = live(screen).map(|(_, row)| row.id()).collect();
        map.retain(|id, _| rows.contains(id));
    }
}

/// The first row of the line the cursor is on (a line wider than the
/// screen wraps from rows above).
fn cursor_line(screen: &Screen) -> Option<RowId> {
    let row = screen.window().row(screen.cursor_position().0)?;
    Some(line_top(row).id())
}

/// The first row of the line `row` is on.
pub fn line_top(mut row: Row<'_>) -> Row<'_> {
    while let Some(up) = row.above().filter(Row::continues) {
        row = up;
    }
    row
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

    /// A paced piece whose room replies took while it waited waits on for
    /// the program to read, and is written then: it is never dropped.
    #[test]
    fn a_paced_piece_that_no_longer_fits_waits_for_room() -> Result<(), Error> {
        let mut pane = pane(5, 20)?;
        let now = Instant::now();
        let pieces = [b"a", b"b"].map(|b| Piece::new(b.to_vec(), log::Unit::Edit));
        pane.send(pieces.into(), now)?;
        assert_eq!(pane.input.drain_all(), b"a");
        // Replies fill the queue while the second piece waits its pace.
        pane.input.push(vec![b'r'; pane.input.room()])?;
        let later = now + Duration::from_secs(1);
        pane.release_paced(later);
        assert!(pane.input.drain_all().iter().all(|b| *b == b'r'));
        pane.release_paced(later + Duration::from_secs(1));
        assert_eq!(pane.input.drain_all(), b"b");
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
    fn spends_the_budget(rows: u16, cols: u16) -> Vec<u8> {
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

    /// Output that only moves rows (insert line, over and over) still
    /// counts against the journal's budget, which lets the oldest go.
    #[test]
    fn rows_moved_alone_count_against_what_is_kept() -> Result<(), Error> {
        let mut pane = pane(24, 80)?;
        pane.record.set_budget(1 << 20);
        pane.note_input();
        for _ in 0..40_000 {
            pane.output(b"\x1b[H\x1b[L");
        }
        assert!(
            pane.record.lost_after(pane.since.from),
            "{} kept",
            pane.record.held()
        );
        Ok(())
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

    /// The kernel's echo of what was typed, in the piece of output a
    /// parser call's budget stopped in, is told an echo when it is read on
    /// the next turn: the output read so far took none of the echo's bytes
    /// the parser had yet to read; and the echo is looked for as of when
    /// the output came, though read more than the second it is looked for
    /// after (`classify::ECHO_WITHIN`) later.
    #[test]
    fn an_echo_read_after_a_budget_stop_is_an_echo() -> Result<(), String> {
        let terminal = fuxix::terminal::Size::new(2, 10).ok_or("a size")?;
        let (_master, slave) = fuxix::pty::open(terminal).map_err(|e| e.to_string())?;
        let modes = fuxix::terminal::attributes(&slave).map_err(|e| e.to_string())?;
        let mut pane = pane(1000, 1000).map_err(|e| e.to_string())?;
        let input = crate::classify::Input {
            unit: log::Unit::Edit,
            line: None,
            ahead: false,
        };
        let now = Instant::now();
        assert!(pane.classes.wrote(input, b"abc", Some(&modes.modes()), now));
        // Output that spends a budget, in one piece with the echo.
        let mut out = spends_the_budget(1000, 1000);
        out.extend_from_slice(b"\x1b[Habc");
        pane.output_at(&out, now);
        assert!(pane.has_unread(), "the budget stopped no call");
        std::thread::sleep(Duration::from_millis(1100));
        let mut turns = 0u32;
        while pane.has_unread() && turns < 100 {
            pane.read_unread();
            turns = turns.saturating_add(1);
        }
        let echoes: Vec<Echo> = (pane.record.printed_after(log::Seq(0)))
            .flat_map(|(_, printed)| {
                printed
                    .runs()
                    .map(|(p, e, _)| (p.text.to_owned(), e.clone()))
            })
            .filter(|(text, _)| text == "abc")
            .map(|(_, echo)| echo)
            .collect();
        assert_eq!(echoes, [Echo::Is]);
        Ok(())
    }

    /// The echo of what was typed, inside a frame begun in output read a
    /// turn after it came, is looked for as of when it came, as outside
    /// one: the frame read more than the second it is looked for after.
    #[test]
    fn an_echo_in_a_frame_read_late_is_an_echo() -> Result<(), String> {
        let terminal = fuxix::terminal::Size::new(2, 10).ok_or("a size")?;
        let (_master, slave) = fuxix::pty::open(terminal).map_err(|e| e.to_string())?;
        let modes = fuxix::terminal::attributes(&slave).map_err(|e| e.to_string())?;
        let mut pane = pane(1000, 1000).map_err(|e| e.to_string())?;
        let input = crate::classify::Input {
            unit: log::Unit::Edit,
            line: None,
            ahead: false,
        };
        let now = Instant::now();
        assert!(pane.classes.wrote(input, b"abc", Some(&modes.modes()), now));
        let mut out = spends_the_budget(1000, 1000);
        out.extend_from_slice(b"\x1b[?2026h\x1b[Habc\x1b[?2026l");
        pane.output_at(&out, now);
        assert!(pane.has_unread(), "the budget stopped no call");
        std::thread::sleep(Duration::from_millis(1100));
        let mut turns = 0u32;
        while pane.has_unread() && turns < 100 {
            pane.read_unread();
            turns = turns.saturating_add(1);
        }
        let echoes: Vec<Echo> = (pane.record.printed_after(log::Seq(0)))
            .flat_map(|(_, printed)| {
                printed
                    .runs()
                    .map(|(p, e, _)| (p.text.to_owned(), e.clone()))
            })
            .filter(|(text, _)| text == "abc")
            .map(|(_, echo)| echo)
            .collect();
        assert_eq!(echoes, [Echo::Is]);
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

    /// Output is read in pieces that end at a bell, or a run of bells, or
    /// a shell mark; a BEL that ends a title is no bell.
    #[test]
    fn pieces_end_at_bells_and_shell_marks() {
        assert_eq!(super::cut(b"\x1b]0;title\x07text\x07more", true), 15);
        assert_eq!(super::cut(b"a\x07\x07\x07b", false), 4);
        assert_eq!(super::cut(b"x\x1b]133;A;nonce=1\x07$ ", true), 17);
        assert_eq!(super::cut(b"x\x1b]133;A;nonce=1\x07$ ", false), 19);
        assert_eq!(super::cut(b"plain", true), 5);
    }
}

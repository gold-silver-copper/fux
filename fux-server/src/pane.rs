//! A pane: its terminal emulator, its process, and the input waiting for it.
use crate::after;
use crate::journal::{Journal, Teller, Told};
use crate::outer::{Colours, Scheme};
use crate::process::{Child, Pid};
use fux_vt::bytes::ByteQueue;
use fux_vt::{Event, Feature, Screen, Size};
use fux_wire::answer::Ended;
use fux_wire::id::{PaneId, TabId, WsId};
use std::collections::{BTreeSet, VecDeque};
use std::time::{Duration, Instant};

/// How long a frame drawn in synchronized output is held before it is
/// shown anyway: Ghostty's choice. Long enough for a frame sent in pieces
/// over a slow link; a program that dies mid-frame freezes its pane this
/// long, once. The spec leaves it open
/// (`references/modern/mode_2026_synchronized_output.md`, "Timeout").
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(1);
/// The most output one turn of a pane gives its parser: a read's worth. A
/// held frame or a program's last output, which can be more, is read a
/// slice a turn, the rest waiting as after a budget stop (`Pane::unread`).
pub const SLICE: usize = 16 << 10;

/// Why a pane cannot be made.
#[derive(Debug)]
pub enum Error {
    /// No terminal of this size.
    Terminal { size: Size, source: fux_vt::Error },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Terminal { size, source } => {
                write!(f, "a {}x{} terminal: {source}", size.cols(), size.rows())
            }
        }
    }
}

impl std::error::Error for Error {}

/// Input and terminal replies waiting for the pane's program to read them,
/// end to end, to be written together, however many pieces they came in.
#[derive(Default)]
pub struct InputQueue {
    bytes: ByteQueue,
    touched: bool,
}

/// A pane's runs of losses, each over at a success of its kind: a copy made, a picture placed whole, a
/// read that refused no memory.
#[derive(Clone, Copy, Debug, Default)]
struct LossRuns {
    copies_cut: Run,
    copies_not_text: Run,
    pictures: Run,
    memory: Run,
}

/// What a read of output did over its calls of the parser: whether it
/// parsed any, or was refused memory.
#[derive(Clone, Copy, Debug, Default)]
struct Reading {
    parsed: bool,
    refused: bool,
    /// A frame was read whole: the read ends where the next frame begins,
    /// or where nothing is left, what follows the frame joining it.
    framed: bool,
}

/// A run of one kind of drop: none going on; going on and not yet told;
/// or told, so the rest of it is counted and not told again, as its
/// notice said what goes on happening till it ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Run {
    #[default]
    Over,
    Untold,
    Told,
}

impl Run {
    /// A drop of its kind: a run begins, if none goes on.
    fn dropped(&mut self) {
        if *self == Run::Over {
            *self = Run::Untold;
        }
    }
    /// Its drops are told: the rest of a run going on is not.
    fn told(&mut self) {
        if *self == Run::Untold {
            *self = Run::Told;
        }
    }
}

impl InputQueue {
    /// Queues `bytes` as a piece. Only the pane queues (`Pane::push`).
    fn push(&mut self, bytes: impl AsRef<[u8]>) {
        let bytes = bytes.as_ref();
        self.bytes.push(bytes);
        self.touched |= !bytes.is_empty();
    }
    /// Queues what `write` appends to its vector as a piece, in place.
    fn push_with(&mut self, write: impl FnOnce(&mut Vec<u8>)) {
        let len = self.bytes.push_with(|out| {
            let start = out.len();
            write(out);
            out.len().saturating_sub(start)
        });
        self.touched |= len > 0;
    }
    /// How many bytes it holds, not yet written to the program.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    /// The bytes to write next: every piece queued.
    pub fn front(&self) -> Option<&[u8]> {
        (!self.bytes.is_empty()).then(|| self.bytes.as_slice())
    }
    /// `n` bytes of the front were written: its terminal took them.
    fn advance(&mut self, n: usize) {
        self.bytes.take(n);
    }
    /// Lets everything queued go, as its program ends unread: how many
    /// bytes, which its ender tells.
    fn discard(&mut self) -> usize {
        let queued = self.len();
        self.advance(queued);
        queued
    }
    /// Everything queued, for tests and the harnesses.
    #[cfg(any(test, feature = "in-process"))]
    fn drain_all(&mut self) -> Vec<u8> {
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

/// The fixed facilities supported by fux. Each creation supplies its
/// emulator policy separately. Events (titles, colour queries),
/// DECRQM (programs ask it whether synchronized output is known before
/// they use it), in-band resize, the size query, colour-scheme reports, the kitty
/// keyboard protocol (fux encodes keys as each pane asks), hyperlinks,
/// prompt marks, DECRQSS (neovim asks it whether the terminal keeps
/// underline styles: a pane keeps them, and each client is painted them as
/// far as its terminal draws them, `answer::sgr`), the palette (a pane's
/// program sets and asks its colours, OSC 4, 10 to 19 and the rest, and
/// is drawn in them, `render::pane_colours`, without its client's palette
/// changing) and fux's identity.
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
    .with_identity(Some(IDENTITY));

/// The most titles a pane's program can push (`CSI 22 t`): xterm's stack.
pub const TITLE_STACK: usize = 10;

/// Text a program sent (a title, a notification), as it is kept: without
/// control characters, and `cut` if it was longer than fux-vt keeps of a
/// string.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Said {
    text: String,
    cut: bool,
}

impl Said {
    fn new(sent: fux_vt::Kept<'_>) -> Self {
        let text = String::from_utf8_lossy(sent.bytes);
        let text = text.chars().filter(|c| !c.is_control()).collect();
        Self {
            text,
            cut: sent.cut,
        }
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    /// `text` as a program sent it, `cut` if it sent more.
    #[cfg(feature = "in-process")]
    pub fn sent(text: &str, cut: bool) -> Self {
        Self::new(fux_vt::Kept {
            bytes: text.as_bytes(),
            cut,
        })
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
    now: Said,
    pushed: VecDeque<Said>,
}

impl Title {
    pub fn as_str(&self) -> &str {
        self.now.as_str()
    }
    pub fn said(&self) -> &Said {
        &self.now
    }
    /// Sets it to `title`; whether it changed.
    fn set(&mut self, title: Said) -> bool {
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
struct Sink<'a, 't> {
    /// The replies to one read, queued together after it.
    replies: Vec<u8>,
    /// Where its bells, notifications, titles and commands' ends go: the
    /// session's journal, in order.
    tell: &'a mut Teller<'t>,
    title: &'a mut Title,
    /// Set by a bell (BEL).
    bell: &'a mut bool,
    alerts: &'a mut Alerts,
    /// Each OSC 52 write, in order: its UTF-8 text, or why it is no copy.
    copied: &'a mut Vec<Result<String, Uncopied>>,
    /// What colour queries are answered with (`outer`).
    colours: &'a Colours,
    /// Each sixel picture that ended, in order: placed whole or not.
    pictures: Vec<bool>,
    /// The scheme a `CSI ? 996 n` among `replies` answered, last: its
    /// program was told it once they are queued (`Pane::scheme_told`).
    told: Option<Scheme>,
}

impl Sink<'_, '_> {
    /// The title changed: the journal is told.
    fn titled(&mut self) {
        let title = self.title.said().clone();
        self.tell.tell(Told::Title(title));
    }
}

impl fux_vt::Sink for Sink<'_, '_> {
    fn reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
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
                self.tell.tell(Told::Bell);
                *self.bell = true;
                self.alerts.rung = self.alerts.rung.saturating_add(1);
            }
            Event::Notification(text) => {
                self.alerts.rung = self.alerts.rung.saturating_add(1);
                let text = Said::new(text);
                self.tell.tell(Told::Notification(text.clone()));
                self.alerts.last = Some(text);
            }
            Event::Clipboard { data, .. } => {
                self.copied.push(match data.cut {
                    true => Err(Uncopied::Cut),
                    false => (fux_wire::json::unbase64(data.bytes))
                        .and_then(|d| String::from_utf8(d).ok())
                        .ok_or(Uncopied::NotText),
                });
            }
            Event::CommandEnded(status) => self.tell.tell(Told::Done(status)),
            Event::IconName(_) => {}
        }
    }
    fn picture(&mut self, placed: bool) {
        self.pictures.push(placed);
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
                    self.replies.extend_from_slice(scheme.report());
                    self.told = Some(scheme);
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
    /// Reading failed unexpectedly: no longer polled, with the cause retained.
    ReadFailed(Child, fuxix::Errno),
    /// The program the pane was started with exited so: the pane stays,
    /// its last screen kept to be read.
    Exited(Ended),
    /// Respawning (`PaneMut::let_go`): the old program is let go, and the
    /// new one is no program of the pane's till it takes its place.
    Respawning,
}

impl Process {
    pub fn child(&self) -> Option<&Child> {
        match self {
            Self::Absent | Self::Exited(_) | Self::Respawning => None,
            Self::Reading(child) | Self::HungUp(child) | Self::ReadFailed(child, _) => Some(child),
        }
    }

    /// How its program ended, once it has.
    pub fn exit_status(&self) -> Option<Ended> {
        match self {
            Self::Exited(status) => Some(*status),
            Self::Absent
            | Self::Reading(_)
            | Self::HungUp(_)
            | Self::ReadFailed(..)
            | Self::Respawning => None,
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
    /// A shell with saved launch inputs: its exit closes the pane.
    Shell(Launch),
    /// A program the pane was given, in place of the shell: its exit
    /// leaves the pane, with its last screen and its status
    /// (`Process::Exited`); how it was started, which `fux respawn` starts
    /// again.
    Explicit(Launch),
}

/// How a program was started: its words, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub argv: fux_wire::command::Program,
    pub cwd: std::path::PathBuf,
}

impl Runs {
    /// The concrete inputs saved for this pane, whichever role it has.
    pub fn launch(&self) -> &Launch {
        match self {
            Self::Shell(launch) | Self::Explicit(launch) => launch,
        }
    }
}

/// A program's file name, or the program as given if it has none.
fn basename(program: &str) -> &str {
    std::path::Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
}

/// What a program rang or announced since it started: bells, and desktop
/// notifications (OSC 9, 777, 99) with the last one's text.
#[derive(Clone, Debug, Default)]
pub struct Alerts {
    /// Bells and notifications together, as a pane's version counts them.
    pub rung: u64,
    pub last: Option<Said>,
}

/// What fux let go of a pane's program's, of each kind, counted: since
/// its program started (`Pane::lost`, in its JSON), and since a client
/// showing it was told (`Pane::untold`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Losses {
    /// Its copies (OSC 52) that copied nothing, by why (`Uncopied`).
    pub copies_cut: u64,
    pub copies_not_text: u64,
    /// Sixel pictures it drew not kept whole
    /// (`fux_vt::Sink::picture`).
    pub pictures: u64,
    /// Times reading its output, or a resize, was refused memory
    /// (`fux_vt::Parser::refused`): each lost only what it was for.
    pub memory: u64,
}

impl Losses {
    /// These and `more`.
    fn add(&mut self, more: Losses) {
        // Every kind named: one more does not compile till it is added.
        let Losses {
            copies_cut,
            copies_not_text,
            pictures,
            memory,
        } = more;
        let add = |a: &mut u64, b| *a = a.saturating_add(b);
        add(&mut self.copies_cut, copies_cut);
        add(&mut self.copies_not_text, copies_not_text);
        add(&mut self.pictures, pictures);
        add(&mut self.memory, memory);
    }

    /// What a notice says of them, of pane `id`: for each kind it lost
    /// any of, what, how many, and why (`%1: memory refused (2): …`).
    pub fn told(self, id: PaneId, runs: bool) -> Vec<String> {
        let (cut, not_text) = (Uncopied::Cut, Uncopied::NotText);
        let (cut, not_text) = (format!("each {cut}"), format!("each {not_text}"));
        let pictures = "`fux help json` says why one may not be";
        let (copies, memory) = ("copies (OSC 52) not made", "each lost only what it was for");
        let Losses {
            copies_cut: c,
            copies_not_text: t,
            pictures: p,
            memory: m,
        } = self;
        // Each kind: how many, what, why, and what ends its run, till which
        // more are counted, not told (`Run`).
        let each: [(u64, &str, &str, &str); 4] = [
            (c, copies, &cut, "a copy is made"),
            (t, copies, &not_text, "a copy is made"),
            (
                p,
                "sixel pictures not kept whole",
                pictures,
                "one is placed whole",
            ),
            (
                m,
                "memory refused",
                memory,
                "a read of its output refuses none",
            ),
        ];
        (each.into_iter())
            .filter(|(n, ..)| *n > 0)
            .map(|(n, what, why, till)| {
                let more = format!("; more till {till} are counted in its `lost`");
                format!(
                    "{id}: {what} ({n}): {why}{}",
                    if runs { more.as_str() } else { "" }
                )
            })
            .collect()
    }
}

/// Why a program's copy (OSC 52) copied nothing.
#[derive(Clone, Copy, Debug)]
pub enum Uncopied {
    /// It was longer than fux-vt keeps of a string.
    Cut,
    /// It was not base64 (RFC 4648) of UTF-8 text.
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
            Uncopied::NotText => f.write_str("is not base64 (RFC 4648) of UTF-8 text"),
        }
    }
}

/// A pane: made, in the session, or closed. What it does that events tell
/// is journaled as it does it: each method that does any takes the
/// journal at its place (`Teller`), through a `PaneMut`: on the
/// session's journal, only the session's parts give one.
pub struct Pane {
    /// Given as it is made, and never changed: its events' place.
    id: PaneId,
    /// Given by `name` or `rename`, and kept across respawns.
    name: Option<String>,
    /// Set by the program with OSC 0 or 2, which says so.
    title: Title,
    /// Its screen's size is the PTY's: the smallest rectangle any client
    /// shows the pane in. What it reads and is resized to goes through the
    /// pane, which says what events tell of it.
    parser: fux_vt::Parser,
    /// Its program ends by `exit`, which says so.
    process: Process,
    input: InputQueue,
    pub runs: Runs,
    options: fux_wire::command::PaneOptions,
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
    unread: Pending,
    /// How many of the unread bytes are the rest of a frame being read
    /// (`Pane::mid_frame`).
    frame_left: usize,
    /// The frame's bytes are read, but the parser has work they left
    /// (REP's copies): the frame is not done till it is.
    frame_work: bool,
    /// A size asked while a frame drawn for the old one was still being
    /// read: taken once it is (`Pane::frame_read`).
    resize_to: Option<Size>,
    /// How much more output this turn may give the parser ([`SLICE`]).
    slice_left: usize,
    /// How its program exited, from when fux hears of it till that is
    /// told, while what it wrote is still being read (`Session::exited`).
    pub exiting: Option<Ended>,
    /// What its program left on its terminal as it ended is still being
    /// read, a turn at a time (`Session::read_left`).
    pub draining: bool,
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
    /// The mouse button `send-mouse press` holds down until its release: a
    /// `move` meanwhile is a drag.
    pub mouse_held: Option<fux_vt::keys::mouse::MouseButton>,
    /// What its program left as it ended, or is ending.
    pub left: Left,
    /// How many times its program was started again (`respawn`), each on a
    /// screen of its own: a wait from the program before answers
    /// `respawned`.
    pub respawns: u64,
    pub alerts: Alerts,
    /// What fux let go of its program's since it started.
    lost: Losses,
    /// What it let go of and has not told a client showing it, its
    /// programs' before a respawn too: kept till one is.
    untold: Losses,
    /// The size its screen was last refused, if its last resize was.
    refused_size: Option<Size>,
    /// Its runs of copies, pictures and memory lost (`Run`): its runs of
    /// input and replies are its queue's.
    loss_runs: LossRuns,
    /// First untold PTY resize and read errors, kept until a client sees them.
    terminal_errors: (Option<fuxix::Errno>, Option<fuxix::Errno>),
    /// The current program's read failure, retained after its exit.
    read_error: Option<fuxix::Errno>,
    /// What the read of output under way did so far (`Pane::read_done`).
    reading: Reading,
}

/// Owned output waiting for another parser turn. Consumed prefixes stay
/// in this allocation until the bytes are read, rather than copied away
/// at each budget yield.
struct Pending {
    bytes: Vec<u8>,
    at: usize,
    since: Instant,
}

/// A pane to change, with a journal at its place: what its methods do
/// that events tell is journaled as they do it, at its own id. It reads
/// as a `Pane` and changes it only by its own methods, one for each thing
/// the session changes: it gives no `&mut Pane`, so its pane is never put
/// in another's place behind its teller, nor wrapped on another journal.
/// A pane of the session's is had only from the session's parts, which
/// alone hold the session's journal (`Parts::pane_mut`); one made on
/// another journal (a bench's, a pane not yet placed) tells only that.
pub struct PaneMut<'a> {
    pane: &'a mut Pane,
    tell: Teller<'a>,
    input: &'a mut BTreeSet<PaneId>,
}

impl<'a> PaneMut<'a> {
    fn queued(&mut self) {
        if std::mem::take(&mut self.pane.input.touched) && !self.pane.input.is_empty() {
            self.input.insert(self.pane.id);
        }
    }

    /// `pane`, in tab `at`, telling `journal`; new input is noted in
    /// `input` for its next sweep.
    pub fn new(
        pane: &'a mut Pane,
        at: (WsId, TabId),
        journal: &'a mut Journal,
        input: &'a mut BTreeSet<PaneId>,
    ) -> Self {
        let tell = journal.teller((at, pane.id), pane.respawns);
        Self { pane, tell, input }
    }
    pub fn output(&mut self, bytes: &[u8], now: Instant) {
        self.pane.output(bytes, now, &mut self.tell);
        self.queued();
        self.pane.read_done();
    }
    pub fn read_unread(&mut self, now: Instant) {
        self.pane.read_unread(now, &mut self.tell);
        self.queued();
        self.pane.read_done();
    }
    pub fn release_frame(&mut self, now: Instant) {
        self.pane.release_frame(now, &mut self.tell);
        self.queued();
        self.pane.read_done();
    }
    pub fn resize(&mut self, size: Size, now: Instant) {
        self.pane.resize(size, now, &mut self.tell);
        self.queued();
    }
    pub(crate) fn size_terminal(&mut self) {
        let result = self.pane.size_terminal();
        self.pane.terminal_sized(result);
    }
    pub(crate) fn read_failed(&mut self, error: fuxix::Errno) {
        self.pane.terminal_errors.1.get_or_insert(error);
        self.pane.read_error = Some(error);
        self.pane.process = match std::mem::replace(&mut self.pane.process, Process::Absent) {
            Process::Reading(child) | Process::HungUp(child) | Process::ReadFailed(child, _) => {
                Process::ReadFailed(child, error)
            }
            other @ (Process::Absent | Process::Exited(_) | Process::Respawning) => other,
        };
    }
    pub(crate) fn take_terminal_errors(&mut self) -> impl Iterator<Item = String> + use<> {
        self.pane.take_terminal_errors()
    }
    pub fn respawned(&mut self, fresh: Pane, process: Process) -> (Process, Left) {
        let result = self.pane.respawned(fresh, process, &mut self.tell);
        self.queued();
        result
    }
    pub fn rename(&mut self, name: String) {
        self.pane.rename(name, &mut self.tell);
    }
    pub fn exit(&mut self, ended: Ended) -> (End, Process) {
        self.pane.exit(ended, &mut self.tell)
    }
    pub fn ended(&mut self, ended: Ended) -> End {
        self.pane.ended(ended, &mut self.tell)
    }

    // What the session changes that events do not tell: each change by
    // name, no whole pane, so none is put in its place or wrapped anew.
    pub fn write_input(&mut self) -> bool {
        self.pane.write_input()
    }
    pub fn push(&mut self, bytes: impl AsRef<[u8]>) {
        self.pane.push(bytes);
        self.queued();
    }
    pub fn push_with(&mut self, write: impl FnOnce(&Screen, &mut Vec<u8>)) {
        self.pane.push_with(write);
        self.queued();
    }
    /// Its program is sent `held`'s mouse report, `report`; the button
    /// `send-mouse press` holds is `held` once it is queued.
    pub fn push_mouse(&mut self, report: Vec<u8>, held: Option<fux_vt::keys::mouse::MouseButton>) {
        self.push(report);
        self.pane.mouse_held = held;
    }
    /// Its program is told the scheme is `scheme`.
    pub fn offer_scheme(&mut self, scheme: Scheme) {
        self.pane.push(scheme.report());
        self.pane.scheme_told = Some(scheme);
        self.queued();
    }
    #[must_use]
    pub fn host(
        &mut self,
        colours: Colours,
        palette: crate::outer::Palette,
        graphics: fux_vt::Graphics,
    ) -> bool {
        self.pane.host(colours, palette, graphics)
    }
    pub fn keeps_size(&mut self) {
        self.pane.keeps_size();
    }
    #[must_use]
    pub fn take_untold(&mut self) -> Losses {
        self.pane.take_untold()
    }
    /// What the session takes after each read: whether its program rang
    /// the bell, and its copies, in order.
    /// Refused copies are counted here, unless this pane's program-copy permission
    /// is false, which copies nothing and so refuses none.
    pub fn take_read(&mut self) -> (bool, Vec<String>) {
        let mut copies = Vec::new();
        let allowed = self.pane.options.program_clipboard;
        for copy in std::mem::take(&mut self.pane.copied)
            .into_iter()
            .filter(|_| allowed)
        {
            match copy {
                // A copy made ends the runs of copies refused.
                Ok(text) => {
                    let runs = &mut self.pane.loss_runs;
                    (runs.copies_cut, runs.copies_not_text) = (Run::Over, Run::Over);
                    copies.push(text);
                }
                Err(why) => self.pane.uncopied(why),
            }
        }
        (std::mem::take(&mut self.pane.bell), copies)
    }
    /// Its program ended so, told once what it wrote is read.
    pub fn exiting(&mut self, ended: Ended) {
        self.pane.exiting = Some(ended);
    }
    /// Its program, if it has one to end, taken as a respawn over it
    /// begins: the pane is respawning till the new one takes its place.
    /// One that ended keeps its status, its end told already.
    pub fn let_go(&mut self) -> Option<Child> {
        match std::mem::replace(&mut self.pane.process, Process::Respawning) {
            Process::Reading(child) | Process::HungUp(child) | Process::ReadFailed(child, _) => {
                Some(child)
            }
            Process::Absent | Process::Respawning => None,
            ended @ Process::Exited(_) => {
                self.pane.process = ended;
                None
            }
        }
    }
    /// Whether more of what its program left on its terminal is to be read
    /// (`Session::read_left`).
    pub fn drain(&mut self, more: bool) {
        self.pane.draining = more;
    }
    pub fn clear_history(&mut self) {
        self.pane.clear_history();
        self.tell.tell(Told::HistoryCleared);
    }
    pub fn set_history_bytes(&mut self, bytes: fux_wire::command::HistoryBytes) {
        if self.pane.options.history_bytes != bytes {
            self.pane.set_history_bytes(bytes);
            self.tell.tell(Told::HistoryBudget(bytes.get()));
        }
    }
    pub fn stop_reading(&mut self) {
        self.pane.stop_reading();
    }
    /// `n` bytes of its input were written to its terminal, as
    /// `write_input` writes them: for fux-diff's queue.
    #[cfg(any(test, feature = "in-process"))]
    pub fn wrote_input(&mut self, n: usize) {
        self.pane.input.advance(n);
    }
    /// Gives it `process`, as a test's pane runs a child of the test's.
    #[cfg(test)]
    pub(crate) fn set_process(&mut self, process: Process) -> Process {
        self.pane.set_process(process)
    }
    /// Everything queued for its program, as if written: for tests and the
    /// harnesses.
    #[cfg(any(test, feature = "in-process"))]
    pub fn drain_input(&mut self) -> Vec<u8> {
        self.pane.input.drain_all()
    }
}

impl std::ops::Deref for PaneMut<'_> {
    type Target = Pane;
    fn deref(&self) -> &Pane {
        self.pane
    }
}

/// What a program left as it ended, or was respawned over: said with its
/// end (`exited`, `respawned`), in its pane's JSON till it is respawned,
/// and in the notice of its end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Left {
    /// Bytes of input queued for it and not yet written to its terminal,
    /// let go (`InputQueue::discard`): what was written and not read is
    /// not known.
    pub input: usize,
}

impl Left {
    /// What an event says of it: `input_unread`, the bytes of input queued
    /// for it not yet written to its terminal, if any.
    pub fn fields(self) -> impl Iterator<Item = (&'static str, fux_wire::json::Json)> {
        let input = self.input;
        (input > 0)
            .then(|| ("input_unread", fux_wire::json::Json::count(input)))
            .into_iter()
    }

    /// What a notice says of it, after the end's own words: nothing, if it
    /// left no input unwritten.
    pub fn told(self) -> String {
        match self.input {
            0 => String::new(),
            input => format!("; {input} bytes of input queued for it were never written to it"),
        }
    }
}

/// How a program ended, and what it left: made once, as it ends
/// (`Pane::ended`), and journaled and told from that one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct End {
    pub ended: Ended,
    pub left: Left,
}

/// What is said of a pane where no shell command was ever marked.
pub const UNMARKED: &str = "no shell command was marked here (OSC 133 C and D): \
                            `fux help shells` says how to turn marks on";

impl Pane {
    pub fn history_bytes(&self) -> usize {
        self.parser.history_bytes()
    }
    /// The current pane policy, retained through reset and respawn.
    pub fn options(&self) -> fux_wire::command::PaneOptions {
        self.options
    }

    pub fn new(
        id: PaneId,
        name: Option<String>,
        runs: Runs,
        size: Size,
        options: fux_wire::command::PaneOptions,
        now: Instant,
    ) -> Result<Self, Error> {
        let features = OPTIONS
            .set(Feature::Reflow, options.reflow)
            .set(Feature::Pictures, options.picture_bytes.get() != 0);
        let mut parser = fux_vt::Parser::with_options(size, options.history_bytes.get(), features)
            .map_err(|source| Error::Terminal { size, source })?;
        parser.set_picture_bytes(options.picture_bytes.get());
        Ok(Pane {
            options,
            id,
            name,
            title: Title::default(),
            parser,
            process: Process::Absent,
            input: InputQueue::default(),
            runs,
            frame: None,
            unread: Pending {
                bytes: Vec::new(),
                at: 0,
                since: now,
            },
            frame_left: 0,
            frame_work: false,
            resize_to: None,
            slice_left: SLICE,
            exiting: None,
            draining: false,
            colours: Colours::default(),
            scheme_told: None,
            bell: false,
            copied: Vec::new(),
            respawns: 0,
            left: Left::default(),
            mouse_held: None,
            alerts: Alerts::default(),
            lost: Losses::default(),
            untold: Losses::default(),
            refused_size: None,
            loss_runs: LossRuns::default(),
            terminal_errors: (None, None),
            read_error: None,
            reading: Reading::default(),
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
    /// ([`Pane::release_frame`]); after that the program's output is read as it comes until its next BSU.
    /// Each call of the parser does at most a stated amount of work; at it,
    /// the rest of the output waits, unread, for the pane's next turn
    /// ([`Pane::read_unread`]).
    fn output(&mut self, bytes: &[u8], now: Instant, tell: &mut Teller<'_>) {
        if self.has_unread() {
            self.unread.bytes.extend_from_slice(bytes);
            return;
        }
        if let Some(at) = self.read(bytes, now, tell) {
            self.wait_unread(bytes.get(at..).unwrap_or_default().to_vec(), 0, now);
        }
    }

    /// Whether output waits to be read on the pane's next turn
    /// ([`Pane::read_unread`]), or the work of a sequence read already,
    /// which a call of the parser stopped at its budget before doing
    /// (`fux_vt::Parser::has_work_left`: REP's copies), any output read
    /// since waiting behind it.
    pub fn has_unread(&self) -> bool {
        self.unread.at < self.unread.bytes.len() || self.parser.has_work_left()
    }

    /// When the pane's next turn to read the output waiting is due, if any
    /// waits: since it began to wait, so that it is due on the server's
    /// next turn, whenever that reads the clock.
    pub fn unread_due(&self) -> Option<Instant> {
        (self.has_unread() || self.draining).then_some(self.unread.since)
    }

    /// Whether the screen is part way through a frame drawn in
    /// synchronized output, the rest of it waiting for the pane's next
    /// turns: till it is whole, a client is painted the pane as it last
    /// painted it ("keep rendering the last rendered state",
    /// `references/modern/mode_2026_synchronized_output.md`), and the rest
    /// of its screen as it is (`render::Grid::keep_area`).
    pub fn mid_frame(&self) -> bool {
        self.frame_left > 0 || self.frame_work
    }

    /// A frame's bytes read but `left`, the work they leave counted in:
    /// once neither is left, the frame is done and a resize it held up is
    /// taken, its read ended first. Whether one was.
    fn frame_read(&mut self, left: usize, tell: &mut Teller<'_>) -> bool {
        self.frame_left = left;
        self.frame_work = left == 0 && self.parser.has_work_left();
        let Some(size) = self.resize_to.filter(|_| !self.mid_frame()) else {
            return false;
        };
        self.resize_to = None;
        self.judge_read();
        self.take_size(size, tell);
        true
    }

    /// Reads the output waiting since a call of the parser did all the work
    /// one may, until the next call does: the rest of a frame first, as
    /// the frame was read.
    fn read_unread(&mut self, now: Instant, tell: &mut Teller<'_>) {
        let bytes = std::mem::take(&mut self.unread.bytes);
        let start = std::mem::take(&mut self.unread.at);
        let unread = bytes.get(start..).unwrap_or_default();
        // The rest of a frame first, or the work a call left, even after
        // all its bytes were read.
        let framed = self.mid_frame();
        let at = self.frame_left.min(unread.len());
        let (frame, after) = unread.split_at_checked(at).unwrap_or((unread, &[]));
        let stopped = self.parse(frame, false, tell);
        let taken = stopped.map_or(frame.len(), |(taken, _)| taken);
        // A resize the frame held up comes before what follows it.
        let resized = framed && self.frame_read(frame.len().saturating_sub(taken), tell);
        if stopped.is_some() || resized {
            self.wait_unread(bytes, start.saturating_add(taken), now);
            return;
        }
        self.reading.framed |= at > 0;
        if let Some(taken) = self.read(after, now, tell) {
            self.wait_unread(bytes, start.saturating_add(at).saturating_add(taken), now);
        }
    }

    /// `bytes` from `at` waits for the next turn. Give back the allocation
    /// once consumed, including when only a sequence's work remains.
    fn wait_unread(&mut self, bytes: Vec<u8>, at: usize, now: Instant) {
        let (bytes, at) = if at < bytes.len() {
            (bytes, at)
        } else {
            (Vec::new(), 0)
        };
        self.unread = Pending {
            bytes,
            at,
            since: now,
        };
    }

    /// Reads `bytes` at `now`, returning where its next parser turn starts.
    /// A held frame already owns its bytes and keeps that allocation itself.
    fn read(&mut self, bytes: &[u8], now: Instant, tell: &mut Teller<'_>) -> Option<usize> {
        let mut rest = bytes;
        loop {
            if let Some((held, _, scan)) = &mut self.frame {
                let read = held.len();
                held.extend_from_slice(rest);
                let end = scan.end(held)?;
                let frame = std::mem::take(held);
                self.frame = None;
                rest = rest.get(end.saturating_sub(read)..).unwrap_or_default();
                if let Some((taken, _)) =
                    self.parse(frame.get(..end).unwrap_or_default(), false, tell)
                {
                    self.frame_read(end.saturating_sub(taken), tell);
                    self.wait_unread(frame, taken, now);
                    return None;
                }
                self.reading.framed = true;
            } else {
                match self.parse(rest, true, tell) {
                    None => return None,
                    Some((end, fux_vt::Stop::Begun)) => {
                        if self.reading.framed {
                            self.judge_read();
                        }
                        self.frame = Some((Vec::new(), now, self.parser.frame_scan()));
                        rest = rest.get(end..).unwrap_or_default();
                    }
                    Some((end, _)) => {
                        return Some(bytes.len().saturating_sub(rest.len()).saturating_add(end));
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
    fn release_frame(&mut self, now: Instant, tell: &mut Teller<'_>) {
        let Some((held, ..)) = self.frame.take() else {
            return;
        };
        match self.parse(&held, false, tell) {
            // No output waits while a frame is held: it is the first.
            Some((taken, _)) => {
                self.frame_read(held.len().saturating_sub(taken), tell);
                self.wait_unread(held, taken, now);
            }
            // A frame read whole: the read ends at the next frame.
            None => self.reading.framed = true,
        }
    }

    /// Gives `bytes` to the screen, stopping after a BSU if `until_frame`,
    /// where a call of the parser did all the work one may, and at the end
    /// of the turn's [`SLICE`] (`Stop::Budget`). Returns how many bytes were read if it stopped,
    /// and why (`Stop::Begun` or `Stop::Budget`). What they did (a bell, a
    /// title, a command's end) is journaled as it is read. Never inlined:
    /// the parser's loop compiles tighter on its own.
    #[inline(never)]
    fn parse(
        &mut self,
        bytes: &[u8],
        until_frame: bool,
        tell: &mut Teller<'_>,
    ) -> Option<(usize, fux_vt::Stop)> {
        let refused = self.parser.refused();
        let mut sink = Sink {
            replies: Vec::new(),
            pictures: Vec::new(),
            tell,
            title: &mut self.title,
            bell: &mut self.bell,
            alerts: &mut self.alerts,
            copied: &mut self.copied,
            colours: &self.colours,
            told: None,
        };
        // The parser refuses only allocations beyond its limits; the screen
        // stays as it was and output continues.
        // Only `process_until_stop`, even to read past a BSU: with one way
        // in, the parser's loop is compiled once and its byte handling is
        // inlined into it, as it was before frames were held; with two it
        // was not, and escape-heavy output was a quarter slower. A call
        // that did all the work one may stops the read for this turn, as
        // does the end of the turn's slice with more to read.
        let slice = bytes.get(..self.slice_left).unwrap_or(bytes);
        let (mut begun, mut read) = (None, 0usize);
        while let Some(rest) = slice.get(read..)
            && let Ok(Some((taken, stop))) = self.parser.process_until_stop(rest, &mut sink)
        {
            read = read.saturating_add(taken);
            if stop == fux_vt::Stop::Budget || until_frame && stop == fux_vt::Stop::Begun {
                begun = Some((read, stop));
                break;
            }
        }
        let read = begun.map_or(slice.len(), |(read, _)| read);
        self.slice_left = self.slice_left.saturating_sub(read);
        if begun.is_none() && slice.len() < bytes.len() {
            begun = Some((read, fux_vt::Stop::Budget));
        }
        if !sink.replies.is_empty() {
            self.input.push(&sink.replies);
        }
        if let Some(scheme) = sink.told {
            self.scheme_told = Some(scheme);
        }
        let pictures = sink.pictures;
        self.reading.parsed |= !bytes.is_empty();
        // Each picture as it ended, in order: one placed whole ends a run
        // of pictures lost.
        for placed in pictures {
            match placed {
                true => self.loss_runs.pictures = Run::Over,
                false => self.lose(Losses {
                    pictures: 1,
                    ..Losses::default()
                }),
            }
        }
        let memory = self.parser.refused().saturating_sub(refused);
        self.reading.refused |= memory > 0;
        self.lose(Losses {
            memory,
            ..Losses::default()
        });
        begun
    }

    /// Writes the waiting input, as far as the program's terminal takes it;
    /// whether it took any, which makes room in the queue.
    pub(crate) fn write_input(&mut self) -> bool {
        let Process::Reading(child) = &self.process else {
            return false;
        };
        let mut wrote = false;
        while let Some(bytes) = self.input.front() {
            match fuxix::io::write(&child.master, bytes) {
                Ok(0) => break,
                Ok(n) => {
                    self.input.advance(n);
                    wrote = true;
                }
                Err(fuxix::Errno::INTR) => continue,
                Err(_) => break,
            }
        }
        wrote
    }

    /// Resizes the screen and the PTY. A held frame is read first: it was
    /// drawn for the old size. One longer than this turn's slice is read
    /// on at later turns, and the resize waits for its end (`resize_to`).
    fn resize(&mut self, size: Size, now: Instant, tell: &mut Teller<'_>) {
        // The size asked last is the one taken.
        self.resize_to = None;
        if self.size() == size {
            return;
        }
        self.release_frame(now, tell);
        // The frame read now ends its read, before the resize's own losses.
        self.read_done();
        if self.mid_frame() {
            self.resize_to = Some(size);
            return;
        }
        self.take_size(size, tell);
    }

    /// The screen and the PTY made `size`, the frame drawn for the old
    /// size read.
    fn take_size(&mut self, size: Size, tell: &mut Teller<'_>) {
        let refused = self.parser.refused();
        let resized = self.parser.resize(size);
        // A size refused is asked again as the session settles: counted
        // once, till another size is asked.
        let memory = self.parser.refused().saturating_sub(refused);
        if memory == 0 {
            self.loss_runs.memory = Run::Over;
        }
        let again = resized.is_err() && self.refused_size == Some(size);
        self.refused_size = resized.is_err().then_some(size);
        if !again {
            self.lose(Losses {
                memory,
                ..Losses::default()
            });
        }
        if resized.is_err() {
            return;
        }
        tell.tell(Told::Resized(size));
        let result = self.size_terminal();
        if !self.terminal_sized(result) {
            return;
        }
        // In-band resize: the report follows the PTY's new size, never
        // precedes it (references/modern/mode_2048_in_band_resize.md). A
        // program that takes no input is sent none.
        let takes = crate::session::refuses_input(self).is_none();
        if let Some(report) = self.parser.resize_report().filter(|_| takes) {
            self.input.push(report);
        }
    }

    pub fn size(&self) -> Size {
        self.screen().size()
    }

    pub fn screen(&self) -> &Screen {
        self.parser.screen()
    }

    /// It is asked the size it has: a size refused before, asked again,
    /// is a new refusal, counted.
    fn keeps_size(&mut self) {
        (self.refused_size, self.resize_to) = (None, None);
    }

    /// Its input queue, to read.
    pub fn input(&self) -> &InputQueue {
        &self.input
    }

    /// What fux let go of its program's since it started.
    pub fn lost(&self) -> Losses {
        self.lost
    }

    /// fux let `losses` of its program's go: counted, and to be told but
    /// for those of a kind whose run is already told (`Run`).
    fn lose(&mut self, mut losses: Losses) {
        self.lost.add(losses);
        // Every kind alike: a loss begins a run of its kind, if none goes
        // on, and one in a run already told is counted, not told again.
        let gate = |n: &mut u64, run: &mut Run| {
            if *n > 0 {
                run.dropped();
            }
            if *run == Run::Told {
                *n = 0;
            }
        };
        let Losses {
            copies_cut,
            copies_not_text,
            pictures,
            memory,
        } = &mut losses;
        let runs = &mut self.loss_runs;
        gate(copies_cut, &mut runs.copies_cut);
        gate(copies_not_text, &mut runs.copies_not_text);
        gate(pictures, &mut runs.pictures);
        gate(memory, &mut runs.memory);
        self.untold.add(losses);
    }

    /// Its program's copy (OSC 52) copied nothing, for `why`.
    fn uncopied(&mut self, why: Uncopied) {
        let (copies_cut, copies_not_text) = match why {
            Uncopied::Cut => (1, 0),
            Uncopied::NotText => (0, 1),
        };
        let losses = Losses {
            copies_cut,
            copies_not_text,
            ..Losses::default()
        };
        self.lose(losses);
    }

    /// A call that reads its output is done: once nothing is held back or
    /// left, the read is whole, over however many calls of the parser and
    /// turns it took (a frame begun, a budget spent), and only then does
    /// what it lost end runs. A frame read whole ends a read where the
    /// next frame begins, so frames drawn back to back are read one by
    /// one, what follows each joining it. One that refused no memory ends
    /// a run of refusals.
    fn read_done(&mut self) {
        self.slice_left = SLICE;
        // Output held back (a frame) or left (a budget spent) is still
        // being read: what this read did so far is kept for its end.
        if self.frame.is_none() && !self.has_unread() {
            self.judge_read();
        }
    }

    /// A read ends here, its record judged: at a frame read whole, or once
    /// nothing is held back or left.
    fn judge_read(&mut self) {
        let read = std::mem::take(&mut self.reading);
        if read.parsed && !read.refused {
            self.loss_runs.memory = Run::Over;
        }
    }

    /// Whether it let go of anything not yet told.
    pub fn loses(&self) -> bool {
        self.untold != Losses::default()
    }

    /// What it let go of and has not told, which it then has: the rest of
    /// each run of drops going on is not told (`Run`).
    fn take_untold(&mut self) -> Losses {
        // Every run named: one more does not compile till it is told.
        let LossRuns {
            copies_cut,
            copies_not_text,
            pictures,
            memory,
        } = &mut self.loss_runs;
        for run in [copies_cut, copies_not_text, pictures, memory] {
            run.told();
        }
        std::mem::take(&mut self.untold)
    }

    /// Queues `bytes` of input for its program.
    fn push(&mut self, bytes: impl AsRef<[u8]>) {
        self.input.push(bytes);
    }

    /// Queues for its program what `write` makes for its screen.
    fn push_with(&mut self, write: impl FnOnce(&Screen, &mut Vec<u8>)) {
        let screen = self.parser.screen();
        self.input.push_with(|out| write(screen, out));
    }

    /// Its program's queries are answered with `colours` and `palette`
    /// from now on, and it is told its terminal draws `graphics`. Whether
    /// the cell changed: its terminal is then to be sized
    /// (`size_terminal`), once what is read with it, a resize among them,
    /// is read.
    #[must_use]
    pub fn host(
        &mut self,
        colours: Colours,
        palette: crate::outer::Palette,
        graphics: fux_vt::Graphics,
    ) -> bool {
        self.colours = colours;
        self.parser.set_host_palette(palette);
        let told = self.screen().graphics().cell();
        self.parser.set_host_graphics(graphics);
        self.screen().graphics().cell() != told
    }

    /// Sizes its program's terminal as its screen is, and in pixels as the
    /// cell it is told makes it, where one is and the whole fits the
    /// kernel's count: a change of either signals the program (SIGWINCH),
    /// which asks again. One the system refuses leaves it as it was, the
    /// screen sized all the same; the caller must report the failure.
    pub(crate) fn size_terminal(&self) -> Result<(), fuxix::Errno> {
        let Some(child) = self.process.child() else {
            return Ok(());
        };
        let size = self.size();
        let pixels = crate::process::pixels(size, self.screen().graphics().cell());
        fuxix::terminal::set_window_size(&child.master, size.nonzero().into(), pixels)
    }

    fn terminal_sized(&mut self, result: Result<(), fuxix::Errno>) -> bool {
        if let Err(error) = result {
            self.terminal_errors.0.get_or_insert(error);
            return false;
        }
        true
    }

    pub(crate) fn take_terminal_errors(&mut self) -> impl Iterator<Item = String> + use<> {
        let (resize, read) = std::mem::take(&mut self.terminal_errors);
        let id = self.id;
        [
            ("resizing program terminal", resize),
            ("reading program terminal", read),
        ]
        .into_iter()
        .filter_map(move |(operation, error)| error.map(|e| format!("{id}: {operation}: {e}")))
    }
    /// The current program's unexpected PTY read error, if there was one.
    pub fn read_error(&self) -> Option<fuxix::Errno> {
        self.read_error
    }

    pub(crate) fn has_terminal_errors(&self) -> bool {
        self.terminal_errors.0.is_some() || self.terminal_errors.1.is_some()
    }

    fn clear_history(&mut self) {
        self.parser.clear_history();
    }

    pub(crate) fn set_history_bytes(&mut self, bytes: fux_wire::command::HistoryBytes) {
        self.options.history_bytes = bytes;
        self.parser.set_history_bytes(bytes.get());
    }

    /// Where it stands now (`Version`).
    pub fn counts(&self) -> fux_wire::command::Counts {
        fux_wire::command::Counts {
            respawns: self.respawns,
            screen: self.screen().revision(),
            bells: self.alerts.rung,
            done: self.screen().commands().finished,
            ended: self.process.exit_status().is_some(),
        }
    }

    /// Its version in the server run `run`.
    pub fn version(&self, run: fux_wire::command::Run) -> fux_wire::command::Version {
        fux_wire::command::Version {
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

    /// Runs `process` in the pane again, as `fresh` (made for it, at its
    /// size, before the program started) runs it, using its prepared screen
    /// and history budget; the process that was there, to end, and what it left
    /// as it was respawned over: none, said, once it ended.
    fn respawned(
        &mut self,
        fresh: Pane,
        process: Process,
        tell: &mut Teller<'_>,
    ) -> (Process, Left) {
        // The new program starts with nothing queued. One that ended said
        // what it left with its exit; one that runs leaves its input now,
        // the new program started.
        let input = self.input.discard();
        let over = match self.process.exit_status() {
            Some(_) => Left::default(),
            None => Left { input },
        };
        tell.tell(Told::Respawned(over));
        // Every field is explicit so additions require a respawn decision.
        let next = Self {
            id: self.id,
            name: std::mem::take(&mut self.name),
            bell: self.bell,
            copied: std::mem::take(&mut self.copied),
            untold: self.untold,
            colours: self.colours,
            input: std::mem::take(&mut self.input),
            terminal_errors: self.terminal_errors,
            process,
            respawns: fresh.respawns,
            title: fresh.title,
            parser: fresh.parser,
            runs: fresh.runs,
            options: fresh.options,
            frame: fresh.frame,
            unread: fresh.unread,
            frame_left: fresh.frame_left,
            frame_work: fresh.frame_work,
            resize_to: fresh.resize_to,
            slice_left: fresh.slice_left,
            exiting: fresh.exiting,
            draining: fresh.draining,
            scheme_told: fresh.scheme_told,
            mouse_held: fresh.mouse_held,
            left: fresh.left,
            alerts: fresh.alerts,
            lost: fresh.lost,
            refused_size: fresh.refused_size,
            loss_runs: fresh.loss_runs,
            read_error: fresh.read_error,
            reading: fresh.reading,
        };
        let old = std::mem::replace(self, next);
        (old.process, over)
    }

    pub fn id(&self) -> PaneId {
        self.id
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Names it `name`: said, if it is another.
    fn rename(&mut self, name: String, tell: &mut Teller<'_>) {
        if self.name.as_ref() != Some(&name) {
            tell.tell(Told::Renamed(name.clone()));
            self.name = Some(name);
        }
    }

    pub fn process(&self) -> &Process {
        &self.process
    }

    /// A pane made with no program runs `child`: one not yet in the
    /// session.
    pub fn running(mut self, child: Child) -> Self {
        self.process = Process::Reading(child);
        self
    }

    /// The program's terminal hung up: it is read no more, and waits to be
    /// reaped.
    fn stop_reading(&mut self) {
        self.process = match std::mem::replace(&mut self.process, Process::Absent) {
            Process::Reading(child) | Process::HungUp(child) => Process::HungUp(child),
            other @ (Process::Absent
            | Process::Exited(_)
            | Process::Respawning
            | Process::ReadFailed(..)) => other,
        };
    }

    /// Its program ended so: said, no exit waiting to be told after; its
    /// end, and the process that was there, to be reaped.
    fn exit(&mut self, ended: Ended, tell: &mut Teller<'_>) -> (End, Process) {
        let end = self.ended(ended, tell);
        self.exiting = None;
        (
            end,
            std::mem::replace(&mut self.process, Process::Exited(ended)),
        )
    }

    /// Its program ended so, the input queued for it let go: its end,
    /// said, as a shell's is before its pane closes.
    fn ended(&mut self, ended: Ended, tell: &mut Teller<'_>) -> End {
        // A live error notice may still be waiting for its first paint.
        self.terminal_errors.1 = self.terminal_errors.1.or(self.read_error);
        self.left.input = self.input.discard();
        // Its runs end with it, as its input's do.
        self.loss_runs = LossRuns::default();
        let end = End {
            ended,
            left: self.left,
        };
        tell.tell(Told::Exited(end));
        end
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

    pub fn title(&self) -> &Said {
        self.title.said()
    }

    /// Its given name, title or launch program, or its id for display.
    pub fn label(&self) -> std::borrow::Cow<'_, str> {
        self.display_label().into()
    }

    pub(crate) fn display_label(&self) -> fux_wire::words::Label<'_> {
        use fux_wire::words::Label;
        if let Some(name) = self.name() {
            return Label::Text(name);
        }
        if !self.title.as_str().is_empty() {
            return Label::Text(self.title.as_str());
        }
        if let Runs::Explicit(launch) = &self.runs {
            return Label::Text(basename(launch.argv.program()));
        }
        Label::Id(fux_wire::command::AnyRef::Pane(self.id))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    pub(crate) fn options(bytes: usize) -> Result<fux_wire::command::PaneOptions, String> {
        Ok(fux_wire::command::PaneOptions {
            history_bytes: fux_wire::command::HistoryBytes::new(bytes).ok_or("history budget")?,
            program_clipboard: true,
            reflow: true,
            alternate_scroll_lines: fux_wire::command::ScrollLines::new(5).ok_or("scroll lines")?,
            picture_bytes: fux_wire::command::PictureBytes::new(100663296).ok_or("pictures")?,
        })
    }

    use super::*;
    use fux_vt::Mode;

    pub(crate) fn shell() -> Result<Runs, String> {
        Ok(Runs::Shell(Launch {
            argv: fux_wire::command::Program::new("/bin/sh".into(), Vec::new())?,
            cwd: "/".into(),
        }))
    }

    /// `rows` by `cols`, each at least one.
    pub(crate) fn size(rows: u16, cols: u16) -> Size {
        let one = |n| std::num::NonZeroU16::new(n).unwrap_or(std::num::NonZeroU16::MIN);
        Size::from((one(rows), one(cols)))
    }

    #[test]
    fn terminal_policy_survives_ris_and_picture_reports_match_admission()
    -> Result<(), Box<dyn std::error::Error>> {
        for reflow in [false, true] {
            for bytes in [0, 1, 4096] {
                let mut options = options(1024)?;
                options.reflow = reflow;
                options.picture_bytes =
                    fux_wire::command::PictureBytes::new(bytes).ok_or("budget")?;
                let mut pane = Pane::new(
                    PaneId::of(1),
                    None,
                    shell()?,
                    size(3, 8),
                    options,
                    Instant::now(),
                )?;
                let cell = fux_vt::CellSize::new(10, 20).ok_or("cell")?;
                let _ = pane.host(
                    Colours::default(),
                    Default::default(),
                    fux_vt::Graphics::Sixel(cell),
                );
                pane.parser.process(b"\x1bcabcdef")?;
                pane.parser.resize(size(3, 4))?;
                assert_eq!(
                    pane.screen().cursor_position().0,
                    if reflow { 1 } else { 0 }
                );
                let mut reply = Vec::new();
                pane.parser
                    .process_with_replies(b"\x1b[c", |b| reply.extend_from_slice(b))?;
                let reply = String::from_utf8(reply)?;
                assert_eq!(
                    reply.trim_end_matches('c').split(';').any(|n| n == "4"),
                    bytes != 0
                );
                pane.parser.process(b"\x1bP9q#1;2;100;0;0!20~\x1b\\")?;
                assert_eq!(pane.screen().pictures().next().is_some(), bytes == 4096);
            }
        }
        Ok(())
    }

    #[test]
    fn output_answers_queries_and_takes_titles() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(5, 20, 1 << 10)?;
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
    fn colour_queries_are_answered_from_the_session_s_colours()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::outer::{Colours, Scheme};
        let mut pane = pane(3, 30, 1 << 10)?;
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
            let stops = fux_vt::Parser::with_options(
                size(rows, cols),
                1 << 10,
                OPTIONS.with(Feature::Reflow).with(Feature::Pictures),
            )
            .is_ok_and(|mut parser| {
                matches!(
                    parser.process_until_stop(&output(n), &mut Discard),
                    Ok(Some((_, fux_vt::Stop::Budget)))
                )
            });
            if stops {
                break;
            }
            n = n.saturating_mul(2);
        }
        output(n.saturating_mul(2))
    }

    /// An isolated pane with explicit geometry and history allowance.
    pub(crate) fn pane(
        rows: u16,
        cols: u16,
        history: usize,
    ) -> Result<Lone, Box<dyn std::error::Error>> {
        let size = Size::new(rows, cols)?;
        Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            size,
            crate::pane::tests::options(history)?,
            Instant::now(),
        )
        .map_err(Into::into)
        .and_then(Lone::new)
    }

    /// A pane alone, for tests of it: what it tells goes in a journal of
    /// its own, at a place of its own.
    pub(crate) struct Lone {
        pub pane: Pane,
        pub journal: Journal,
        queued_input: BTreeSet<PaneId>,
    }

    impl std::ops::Deref for Lone {
        type Target = Pane;
        fn deref(&self) -> &Pane {
            &self.pane
        }
    }

    impl std::ops::DerefMut for Lone {
        fn deref_mut(&mut self) -> &mut Pane {
            &mut self.pane
        }
    }

    impl Lone {
        pub(crate) fn new(pane: Pane) -> Result<Self, Box<dyn std::error::Error>> {
            let kept = fux_wire::command::JournalEntries::new(1 << 14).ok_or("journal")?;
            Ok(Self {
                pane,
                journal: Journal::new(kept),
                queued_input: BTreeSet::new(),
            })
        }
        pub(crate) fn placed(&mut self) -> PaneMut<'_> {
            let at = (WsId::of(1), TabId::of(1));
            PaneMut::new(
                &mut self.pane,
                at,
                &mut self.journal,
                &mut self.queued_input,
            )
        }
        pub(crate) fn output(&mut self, bytes: &[u8]) {
            self.placed().output(bytes, Instant::now());
        }
        pub(crate) fn output_at(&mut self, bytes: &[u8], now: Instant) {
            self.placed().output(bytes, now);
        }
        pub(crate) fn read_unread(&mut self) {
            self.placed().read_unread(Instant::now());
        }
        pub(crate) fn release_frame(&mut self) {
            self.placed().release_frame(Instant::now());
        }
        pub(crate) fn resize(&mut self, size: Size) {
            self.placed().resize(size, Instant::now());
        }
    }

    /// A call of the parser that does all the work one may stops the read
    /// (THREATS.md 4): the rest of the output, and output read after it,
    /// waits for the pane's next turn and is read in order.
    #[test]
    fn a_read_that_spends_its_budget_goes_on_at_the_next_turn()
    -> Result<(), Box<dyn std::error::Error>> {
        let reps = spends_the_budget(50, 200);
        let mut pane = pane(50, 200, 1 << 10)?;
        let now = Instant::now();
        pane.placed().output(&reps, now);
        assert!(pane.has_unread(), "the budget stopped no call");
        assert_eq!(pane.unread_due(), Some(now));
        let next = after(now, Duration::from_secs(1));
        pane.placed().output(b"\x1b[HZ", next);
        assert!(
            !first_row(&pane).starts_with('Z'),
            "output after the stop is read before it"
        );
        let allocation = pane.unread.bytes.as_ptr();
        let mut turns = 0u32;
        while pane.has_unread() && turns < 100 {
            pane.placed().read_unread(next);
            if !pane.unread.bytes.is_empty() {
                assert_eq!(pane.unread.bytes.as_ptr(), allocation);
                assert!(pane.unread.at > 0);
            }
            if pane.has_unread() {
                assert_eq!(pane.unread_due(), Some(next));
            }
            turns = turns.saturating_add(1);
        }
        assert!(!pane.has_unread(), "{turns} turns");
        assert_eq!(
            pane.unread.bytes.capacity(),
            0,
            "consumed storage is released"
        );
        assert_eq!(pane.screen().cursor_position(), (0, 1));
        assert_eq!(first_row(&pane), "Z");
        assert!(
            pane.screen()
                .rows()
                .skip(1)
                .all(|row| row.cells().all(|cell| !cell.has_contents()))
        );
        Ok(())
    }

    /// Work the parser left for its next call after the output's last
    /// byte, REP's copies in insert mode, each moving the row, is carried
    /// out at the pane's next turns: the pane says output waits while it
    /// does, though no byte does.
    #[test]
    fn work_left_past_the_budget_at_the_outputs_end_goes_on()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(1, 4096, 0)?;
        pane.output(b"\x1b[4hx\x1b[4000b");
        assert!(pane.has_unread(), "the copies were made in one call");
        let mut turns = 0u32;
        while pane.has_unread() && turns < 100 {
            pane.read_unread();
            turns = turns.saturating_add(1);
        }
        assert!(!pane.has_unread(), "{turns} turns");
        assert_eq!(
            first_row(&pane),
            std::iter::repeat_n('x', 4001).collect::<String>()
        );
        Ok(())
    }

    /// A frame's boundary is no way past the budget: frames begun and
    /// ended in the sequence that clears the alternate screen in a
    /// background, each clear the budget's worth, are read over many
    /// turns, the pane yielding after each, not in the one turn they came.
    #[test]
    fn frame_boundaries_yield_at_the_budget() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(1000, 1000, 0)?;
        let mut output = b"\x1b[41m".to_vec();
        for _ in 0..40 {
            output.extend_from_slice(b"\x1b[?1049;2026h\x1b[?1049;2026l");
        }
        pane.output(&output);
        assert!(pane.has_unread(), "the clears were read in one turn");
        let mut turns = 1u32;
        while pane.has_unread() && turns < 1000 {
            pane.read_unread();
            turns = turns.saturating_add(1);
        }
        assert!(turns >= 20, "{turns} turns");
        assert!(!pane.has_unread() && !pane.screen().mode(fux_vt::Mode::AlternateScreenSaveCursor));
        Ok(())
    }

    /// A resize waits for a held frame longer than a turn's slice: the
    /// frame's rest, and a reply it asks for, is read at the size it was
    /// drawn for, a slice a turn, and the size is taken after it.
    #[test]
    fn a_resize_waits_for_the_held_frame_it_follows() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(3, 30, 1 << 10)?;
        let mut frame = b"\x1b[?2026h".to_vec();
        frame.extend(std::iter::repeat_n(0u8, 2 * SLICE));
        frame.extend_from_slice(b"\x1b[999G\x1b[6n");
        pane.output(&frame);
        pane.resize(size(3, 20));
        assert_eq!(<(u16, u16)>::from(pane.size()), (3, 30), "taken mid-frame");
        let mut turns = 0u32;
        while pane.has_unread() && turns < 100 {
            pane.read_unread();
            turns = turns.saturating_add(1);
        }
        assert!(turns >= 2, "{turns} turns");
        assert_eq!(pane.input.drain_all(), b"\x1b[1;30R");
        assert_eq!(<(u16, u16)>::from(pane.size()), (3, 20));
        Ok(())
    }

    /// A resize waits for a held frame till its bytes and the work they
    /// leave are done, however it ends: past a turn's slice, in an
    /// operation that spends the budget (DECALN), or in REP's copies under
    /// IRM, after a long prefix or not; and a size asked back before the
    /// frame is done cancels the one that waited.
    #[test]
    fn a_resize_waits_for_the_held_frame_s_work() -> Result<(), Box<dyn std::error::Error>> {
        let long = |tail: &[u8]| -> Vec<u8> {
            let mut frame = b"\x1b[?2026h".to_vec();
            frame.extend(std::iter::repeat_n(0u8, 2 * SLICE));
            frame.extend_from_slice(tail);
            frame
        };
        let decaln = long(b"\x1b#8");
        let rep = b"\x1b[?2026h\x1b[4hx\x1b[4000b".to_vec();
        let long_rep = long(b"\x1b[4hx\x1b[4000b");
        for (rows, cols, frame, to, back) in [
            (1000, 1000, &decaln, (1000, 900), false),
            (1, 4096, &rep, (1, 20), false),
            (1, 4096, &long_rep, (1, 2048), false),
            (1, 4096, &long_rep, (1, 2048), true),
        ] {
            let mut pane = pane(rows, cols, 0)?;
            pane.output(frame);
            pane.resize(size(to.0, to.1));
            assert_eq!(
                <(u16, u16)>::from(pane.size()),
                (rows, cols),
                "taken mid-frame"
            );
            if back {
                pane.placed().keeps_size();
            }
            let mut turns = 0u32;
            while pane.has_unread() && turns < 1000 {
                pane.read_unread();
                turns = turns.saturating_add(1);
            }
            assert!(!pane.has_unread() && !pane.mid_frame(), "{turns} turns");
            let now = if back { (rows, cols) } else { to };
            assert_eq!(
                <(u16, u16)>::from(pane.size()),
                now,
                "{rows}x{cols} to {to:?}"
            );
        }
        Ok(())
    }

    /// Output longer than a read, held as a frame or not, is given the
    /// parser a slice a turn ([`SLICE`]), the rest waiting for the next;
    /// the frame is still painted whole, and what follows it after it.
    #[test]
    fn long_output_is_read_a_slice_a_turn() -> Result<(), Box<dyn std::error::Error>> {
        let text = vec![b'x'; 3 * SLICE];
        let mut frame = b"\x1b[?2026h".to_vec();
        frame.extend_from_slice(&text);
        frame.extend_from_slice(b"\x1b[HDONE\x1b[?2026lZ");
        for (output, framed) in [(text.as_slice(), false), (frame.as_slice(), true)] {
            let mut pane = pane(50, 200, 1 << 10)?;
            pane.output(output);
            let mut turns = 1u32;
            while pane.has_unread() {
                assert_eq!(pane.mid_frame(), framed);
                let at = pane.unread.at;
                pane.read_unread();
                let read = pane.unread.at.saturating_sub(at);
                assert!(pane.unread.bytes.is_empty() || read <= SLICE, "{read} read");
                turns = turns.saturating_add(1);
            }
            assert!(turns >= 3, "{turns} turns");
            assert!(!pane.mid_frame());
            assert!(first_row(&pane).starts_with(if framed { "DONEZ" } else { "xxx" }));
        }
        Ok(())
    }

    /// A frame whose reading spends a parser call's budget is read on over
    /// the next turns, its rest first, and the pane says it is part way
    /// through one till it ends; what came after it is read after.
    #[test]
    fn a_frame_past_a_budget_is_read_on_whole() -> Result<(), Box<dyn std::error::Error>> {
        let size = size(50, 200);
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            size,
            crate::pane::tests::options(0)?,
            Instant::now(),
        )?)?;
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
    fn a_frame_begun_in_output_read_later_is_held_its_timeout()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut out = spends_the_budget(50, 200);
        out.extend_from_slice(b"\x1b[?2026hY");
        let mut pane = pane(50, 200, 1 << 10)?;
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
    fn a_synchronized_frame_reaches_the_screen_whole() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(3, 30, 1 << 10)?;
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
    fn a_frame_ends_at_any_reset_of_synchronized_output() -> Result<(), Box<dyn std::error::Error>>
    {
        for end in [
            &b"\x1b[?25;2026l"[..],
            b"\x1b[?20\r26l",
            b"\x1bc",
            b"\x1b[!p",
            b"\x1b[?2026r",
            // ESC ends the string (ST begins so) and starts the ESU.
            b"\x1b]2;\x1b[?2026l",
        ] {
            let mut pane = pane(3, 30, 1 << 10)?;
            pane.output(&[&b"\x1b[?2026hx"[..], end].concat());
            assert_eq!(pane.frame_deadline(), None, "{end:?}");
            assert!(!pane.screen().mode(Mode::SynchronizedOutput), "{end:?}");
        }
        for held in [&b"\x1b[?20\x1826l"[..], b"\x1b[?2026:1l"] {
            let mut pane = pane(3, 30, 1 << 10)?;
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
    fn a_frame_that_does_not_end_is_read_anyway() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(3, 30, 1 << 10)?;
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
        pane.output(&vec![b'z'; 3 << 20]);
        assert_eq!(
            first_row(&pane),
            "xy",
            "however long, held till its timeout"
        );
        pane.release_frame();
        assert_eq!(pane.frame_deadline(), None);
        // Read a slice a turn, as the server does before it reads more.
        while pane.has_unread() {
            pane.read_unread();
        }
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

    #[test]
    fn a_failed_terminal_resize_keeps_the_real_errno_for_its_notice()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(10, 40, 1 << 10)?;
        let null = std::fs::File::open("/dev/null")?;
        let size = pane.size().nonzero().into();
        let error = fuxix::terminal::set_window_size(&null, size, None)
            .err()
            .ok_or("resize succeeded")?;
        assert!(!pane.terminal_sized(Err(error)));
        assert!(pane.has_terminal_errors());
        // A subsequent success cannot erase an untold failure.
        assert!(pane.terminal_sized(Ok(())));
        let id = pane.id();
        let notices: Vec<_> = pane.placed().take_terminal_errors().collect();
        assert_eq!(
            notices,
            [format!("{id}: resizing program terminal: {error}")]
        );
        assert!(!pane.has_terminal_errors());
        Ok(())
    }

    /// A program that set in-band resize is told its new size after a
    /// resize, and on setting it; one that did not is told nothing.
    #[test]
    fn a_resize_reports_the_size_in_band_when_asked() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(3, 30, 1 << 10)?;
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

    /// Every kind of loss is told once a run, which a success of its kind
    /// ends: pictures lost, by a picture placed whole; copies refused, by a
    /// copy made.
    #[test]
    fn pictures_and_copies_lost_are_told_once_a_run() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(3, 20, 1 << 10)?;
        let picture = b"\x1bP9q#1;2;100;0;0!20~\x1b\\";
        let cell = fux_vt::CellSize::new(10, 20).ok_or("a cell")?;
        pane.output(picture);
        assert_eq!(pane.take_untold().pictures, 1, "none draws sixel");
        pane.output(picture);
        assert!(!pane.loses(), "the same run");
        let _ = pane.host(
            Colours::default(),
            Default::default(),
            fux_vt::Graphics::Sixel(cell),
        );
        pane.output(picture);
        let _ = pane.host(
            Colours::default(),
            Default::default(),
            fux_vt::Graphics::Cell(cell),
        );
        pane.output(picture);
        assert_eq!(pane.take_untold().pictures, 1, "a run after one placed");
        assert_eq!(pane.lost().pictures, 3);
        // In one read, in order: a picture cut (wider than a picture may
        // be) in the run told, then one placed whole, which ends it; then
        // one placed and one cut, which begins a run.
        let cut = b"\x1bP9q#1;2;100;0;0!4097~\x1b\\";
        let _ = pane.host(
            Colours::default(),
            Default::default(),
            fux_vt::Graphics::Sixel(cell),
        );
        pane.output(&[&cut[..], picture].concat());
        assert!(!pane.loses(), "the cut one is in the run told");
        pane.output(&[&picture[..], cut].concat());
        assert_eq!(pane.take_untold().pictures, 1, "a run after one placed");
        let bad = b"\x1b]52;c;***\x07";
        let copy = |pane: &mut Lone, bytes: &[u8]| {
            pane.output(bytes);
            pane.placed().take_read().1.len()
        };
        copy(&mut pane, bad);
        assert_eq!(pane.take_untold().copies_not_text, 1);
        copy(&mut pane, bad);
        assert!(!pane.loses(), "the same run");
        assert_eq!(copy(&mut pane, b"\x1b]52;c;Zm9v\x07"), 1);
        copy(&mut pane, bad);
        assert_eq!(pane.take_untold().copies_not_text, 1, "a run after a copy");
        Ok(())
    }

    /// A title or notification is kept whole; one longer than fux-vt
    /// keeps of a string is cut, which the pane says.
    #[test]
    fn what_is_cut_of_a_title_is_said() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(3, 20, 1 << 10)?;
        let long = std::iter::repeat_n('t', 1000).collect::<String>();
        pane.output(format!("\x1b]2;{long}\x07\x1b]9;{long}\x07").as_bytes());
        assert_eq!(pane.title.as_str(), long);
        assert!(!pane.title.said().cut());
        let notification = pane.alerts.last.as_ref();
        assert_eq!(
            notification.map(|n| (n.as_str(), n.cut())),
            Some((&*long, false))
        );
        let huge = std::iter::repeat_n('x', fux_vt::OSC_PAYLOAD_LIMIT).collect::<String>();
        pane.output(format!("\x1b]2;{huge}\x07").as_bytes());
        while pane.has_unread() {
            pane.read_unread();
        }
        assert!(pane.title.said().cut());
        Ok(())
    }

    /// xterm's title stack: vim and tmux push the title on starting and pop
    /// it on leaving, so the pane's title comes back. Ps 1 (the icon) is no
    /// title; the stack keeps at most ten; a pop with none pushed changes
    /// nothing.
    #[test]
    fn a_pushed_title_comes_back_when_popped() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(3, 30, 1 << 10)?;
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
        let mut pane = pane(3, 30, 1 << 10)?;
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
    fn commands_ended_in_one_read_are_journaled_each() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(3, 30, 1 << 10)?;
        let run = "1-0".parse().map_err(|()| "a run")?;
        let mut stream =
            crate::journal::Reader::new(fux_wire::inspect::Scope::All, &pane.journal, run, None)?;
        pane.output(
            b"\x1b]133;C\x07x\x1b]133;D;1\x07\x07\x1b]133;C\x07\x1b]133;D;0\x07\x1b]2;t\x07",
        );
        let (said, _) = stream.lines(&pane.journal, |_| None);
        assert_eq!(
            said,
            concat!(
                r#"{"event":"done","pane":"%1","tab":"@1","workspace":"+1","status":1}"#,
                "\n",
                r#"{"event":"bell","pane":"%1","tab":"@1","workspace":"+1"}"#,
                "\n",
                r#"{"event":"done","pane":"%1","tab":"@1","workspace":"+1","status":0}"#,
                "\n",
                r#"{"event":"title","pane":"%1","tab":"@1","workspace":"+1","title":"t"}"#,
                "\n"
            )
        );
        Ok(())
    }
}

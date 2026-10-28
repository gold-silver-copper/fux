//! The requests a client sends the server: typed actions, each built by
//! fuxth words in a client (`crate::script`) and carried whole over the
//! socket (`crate::wire`). The server never reads words.
use crate::id::{BufferId, ClientId, PaneId, TabId, WsId};
use crate::keys::mouse::MouseButton;
use crate::keys::{Direction, KeyPress};
use crate::layout::Axis;
use crate::wire::{Error, Reader, Wire, wire};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

/// A pane's checked history budget, in bytes.
pub type HistoryBytes = Number<0, { 1 << 30 }>;

/// The longest limit a wait may be given: a day.
pub const MAX_WAIT: usize = 86_400;

/// How long a wait waits before it times out: up to `MAX_WAIT` seconds, so its
/// deadline is a time there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit(u32);

impl Limit {
    /// `secs` seconds, if at most `MAX_WAIT`.
    pub fn secs(secs: u64) -> Option<Limit> {
        let within = |s: &u32| usize::try_from(*s).is_ok_and(|s| s <= MAX_WAIT);
        u32::try_from(secs).ok().filter(within).map(Limit)
    }

    /// `secs` seconds, or why not.
    pub fn read(secs: u64) -> Result<Limit, String> {
        Limit::secs(secs).ok_or_else(|| format!("{secs} s: a wait's limit is at most {MAX_WAIT} s"))
    }

    pub fn duration(self) -> Duration {
        Duration::from_secs(u64::from(self.0))
    }

    pub fn seconds(self) -> u32 {
        self.0
    }
}

/// A whole number from `MIN` to `MAX`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Number<const MIN: usize, const MAX: usize>(usize);

impl<const MIN: usize, const MAX: usize> Number<MIN, MAX> {
    /// `n`, if from `MIN` to `MAX`.
    pub fn new(n: usize) -> Option<Self> {
        (MIN..=MAX).contains(&n).then_some(Number(n))
    }

    pub fn get(self) -> usize {
        self.0
    }
}

impl<const MIN: usize, const MAX: usize> Wire for Number<MIN, MAX> {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.0.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Number::new(usize::get(r)?).ok_or(Error::Bad("number"))
    }
}

/// The argv fux starts in a pane: a program, never empty, then its
/// arguments, each a word as it is, blanks, quotes and all, but for NUL,
/// which no word a program is given holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    program: String,
    args: Vec<String>,
}

impl Program {
    /// `program` and `args`, or why they are no program.
    pub fn new(program: String, args: Vec<String>) -> Result<Program, String> {
        if program.is_empty() {
            return Err("a program is needed".to_owned());
        }
        if let Some(word) = std::iter::once(&program)
            .chain(&args)
            .find(|w| w.contains('\0'))
        {
            return Err(format!("{word:?}: a program's word holds no NUL"));
        }
        let checked = Program { program, args };
        if checked.encoded_len() > crate::protocol::MAX_PAYLOAD {
            return Err("program argv exceeds the launch frame limit".to_owned());
        }
        Ok(checked)
    }

    pub(crate) fn encoded_len(&self) -> usize {
        std::iter::once(&self.program)
            .chain(&self.args)
            .fold(4usize, |bytes, word| {
                bytes.saturating_add(4).saturating_add(word.len())
            })
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.args.iter().fold(
            self.program.capacity().saturating_add(
                self.args
                    .capacity()
                    .saturating_mul(std::mem::size_of::<String>()),
            ),
            |bytes, arg| bytes.saturating_add(arg.capacity()),
        )
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.program.as_str()).chain(self.args.iter().map(String::as_str))
    }
    pub(crate) fn len(&self) -> usize {
        self.args.len().saturating_add(1)
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    /// Its command line, the program first.
    pub fn argv(&self) -> Vec<String> {
        let all = std::iter::once(&self.program).chain(&self.args);
        all.cloned().collect()
    }
}

/// Read back only checked argv.
impl Wire for Program {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.program.put(out);
        self.args.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let (program, args) = (String::get(r)?, Vec::get(r)?);
        Program::new(program, args).map_err(|_| Error::Bad("program"))
    }
}

/// A shell variable a caller may pass to a pane: letters, digits and `_`,
/// not first a digit, excluding the pane's reserved identity names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvName(String);

impl EnvName {
    pub(crate) fn heap_bytes(&self) -> usize {
        self.0.capacity()
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for EnvName {
    type Err = String;
    fn from_str(name: &str) -> Result<EnvName, String> {
        pane_var(name).map(|()| EnvName(name.to_owned()))
    }
}

impl Wire for EnvName {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.0.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        String::get(r)?
            .parse()
            .map_err(|_| Error::Bad("variable name"))
    }
}

wire! {
    /// A copy by its stable id, or the newest at the time of the request.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Buffer {
        Newest,
        Id(id: BufferId),
    }
}

impl std::fmt::Display for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Newest => f.write_str("newest"),
            Self::Id(id) => id.fmt(f),
        }
    }
}

wire! {
    /// A workspace destination: an id, a name, or a fresh unnamed workspace.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum WsRef {
        Id(id: WsId),
        Name(name: String),
        New,
    }
}

wire! {
    /// Any target: a pane, a tab or a workspace (`%N`, `@N`, `+N`), as a
    /// client's words found it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum AnyRef {
        Pane(id: PaneId),
        Tab(id: TabId),
        Workspace(id: WsId),
    }
}

wire! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Kind {
        Pane,
        Tab,
        Workspace,
    }
}

impl AnyRef {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Pane(_) => Kind::Pane,
            Self::Tab(_) => Kind::Tab,
            Self::Workspace(_) => Kind::Workspace,
        }
    }
}

/// An id as it is written: `%3`, `@2`, `+1`.
impl std::fmt::Display for AnyRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pane(id) => id.fmt(f),
            Self::Tab(id) => id.fmt(f),
            Self::Workspace(id) => id.fmt(f),
        }
    }
}

/// `%N`, `@N` or `+N`: the id of the kind its sigil says.
impl std::str::FromStr for AnyRef {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, ()> {
        (text.parse().map(Self::Pane))
            .or_else(|_| text.parse().map(Self::Tab))
            .or_else(|_| text.parse().map(Self::Workspace))
            .map_err(drop)
    }
}

impl Kind {
    /// Every kind. Nothing checks it: list a new kind here too, as its
    /// arm in `name`'s match is forced.
    pub const ALL: [Kind; 3] = [Kind::Pane, Kind::Tab, Kind::Workspace];
    pub fn name(self) -> &'static str {
        match self {
            Kind::Pane => "pane",
            Kind::Tab => "tab",
            Kind::Workspace => "workspace",
        }
    }
}

wire! {
    /// Which way `reorder` moves a pane, tab or workspace among its siblings,
    /// or `focus`, `step-tab` and `step-workspace` step from the current one.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Sibling {
        Next,
        Previous,
    }
}

wire! {
    /// Which tab or workspace `show` picks.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Pick<T> {
        Step(step: Sibling),
        Id(id: T),
    }
}

wire! {
    /// Which pane `focus` picks.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum PanePick {
        Step(step: Sibling),
        Id(id: PaneId),
        /// The previously focused pane.
        Last,
        Toward(direction: Direction),
    }
}

wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum MoveTo {
        Tab(id: TabId),
        Workspace(workspace: WsRef),
        NewTab,
        NewWorkspace,
        Beside(direction: Direction),
    }
}

wire! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SwapWith {
        Pane(id: PaneId),
        Toward(direction: Direction),
    }
}

/// How a tab no client shows is laid out (`size`, `resize`): `None`,
/// on a client's screen.
pub type Size = Option<crate::view::Screen>;

wire! {
    /// A pane's new width or height (`resize-pane`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Extent {
        Cells(cells: u16),
        /// Of the tab's width or height.
        Percent(percent: Percent),
    }
}

/// A share, 1 to 100 percent: there is no other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Percent(u8);

impl Percent {
    /// Every share there is.
    pub const SHARES: std::ops::RangeInclusive<i64> = 1..=100;
    pub fn new(n: u8) -> Option<Percent> {
        Self::SHARES.contains(&i64::from(n)).then_some(Percent(n))
    }
    pub fn get(self) -> u8 {
        self.0
    }
    /// Its share of `of`, at least 1.
    pub fn of(self, of: u16) -> u16 {
        let share = u32::from(of).saturating_mul(u32::from(self.0)) / 100;
        // The share is at most `of`, which a u16 holds.
        u16::try_from(share).unwrap_or(of).max(1)
    }
}

wire! {
    /// How `resize-pane` and `move-border` resize a pane.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum PaneResize {
        /// Moves the border toward `direction` by this many cells.
        Border(direction: Direction, cells: u16),
        /// To a width (horizontal) or a height (vertical).
        To(axis: Axis, extent: Extent),
    }
}

wire! {
    /// What `resize` sizes.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Resized {
        /// A tab, or every tab of a workspace: what they are laid out on while
        /// no client shows them, or as the clients showing them are.
        Tabs(target: AnyRef, size: Size),
        /// A headless client's screen.
        Client(client: ClientId, size: crate::view::Screen),
    }
}

wire! {
    /// One thing `send-keys` sends.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Input {
        /// A key, encoded as the pane's program asked for keys.
        Key(key: KeyPress),
        /// Text, as typed.
        Text(text: String),
        /// Text as a paste: bracketed if the program asked for bracketed paste.
        Paste(text: String),
        /// Bytes as they are: part of a key's sequence or of a character.
        Bytes(bytes: Vec<u8>),
    }
}

wire! {
    /// Which screen a capture reads.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum Source {
        /// The screen the pane shows, and its history.
        #[default]
        Shown,
        /// The main screen and its history, though a program holds the
        /// alternate one: what was printed before it took the screen.
        Main,
        /// The alternate screen as a full-screen program last drew it, though
        /// it has left it, or exited.
        Alternate,
        /// The last shell command's output alone, between its OSC 133 marks.
        LastOutput,
    }
}

wire! {
    /// Which part of a pane's screen `capture` prints, and how.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct CaptureView {
        /// Every row as it is, blank rows at the end and all, rather than
        /// trimmed.
        pub full: bool,
        /// Each row a line, as the screen wraps them, rather than
        /// soft-wrapped rows joined into the lines they wrap.
        pub wrapped: bool,
        /// Colours and attributes, as SGR escapes, or with `json` as runs.
        pub styles: bool,
        /// Which screen it reads.
        pub source: Source,
        /// Reverse video, how a selection is drawn, between `⟦` and `⟧`.
        pub marks: bool,
        /// Rows `start..end`, from 0 at the top of what is captured.
        pub rows: Option<(u16, u16)>,
        /// Columns `start..end`, from 0.
        pub cols: Option<(u16, u16)>,
    }
}

/// Which run of a server this is: its process and the moment it started,
/// in nanoseconds since 1970 (`48211-1867a3f9c2b8e7d6`), as the wall
/// clock says. Two runs sharing both is unlikely, not ruled out: a process
/// id comes back once its process ended, and the clock may be set back to
/// the moment it started, or be too coarse to tell two moments apart;
/// servers in other PID namespaces share ids too. A restart is a run of
/// its own; so is a server on another socket. Its panes' `FUX_SERVER`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pid: u32,
    since: u64,
}

impl Run {
    /// This process's run, started now: none on a clock before 1970 or
    /// past 2554, whose moment it cannot say.
    pub fn now() -> Option<Run> {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some(Run {
            pid: std::process::id(),
            since: u64::try_from(since.as_nanos()).ok()?,
        })
    }
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{:x}", self.pid, self.since)
    }
}

/// `PID-SINCE`, as a run is written: digits, then lowercase hex digits.
impl std::str::FromStr for Run {
    type Err = ();
    fn from_str(text: &str) -> Result<Run, ()> {
        let (pid, since) = text.split_once('-').ok_or(())?;
        let run = Run {
            pid: pid.parse().map_err(drop)?,
            since: u64::from_str_radix(since, 16).map_err(drop)?,
        };
        // One spelling only, as `Display` writes it: no sign, no leading
        // zeros, no capitals.
        (run.to_string() == text).then_some(run).ok_or(())
    }
}

/// What only grows while one program runs in a pane (a respawn starts all
/// but `respawns` afresh): how often its program was respawned, its screen's
/// revision (fux-vt's: the bytes of output read, plus one for each resize
/// to another size and each change to its history: cleared, or
/// rows evicted by `history-budget`; the same however the output was cut
/// into reads),
/// bells and notifications rung, shell commands ended (OSC 133 `D`), and
/// whether its program ended. Written
/// `RESPAWNS.SCREEN.BELLS.DONE.ENDED`, `ENDED` 0 or 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub respawns: u64,
    pub screen: u64,
    pub bells: u64,
    pub done: u64,
    pub ended: bool,
}

impl Counts {
    /// Whether a pane now at `self` can ever have been at `then`: an
    /// earlier program's, or this one's with no count above its own now.
    pub fn had(self, then: Counts) -> bool {
        then.respawns < self.respawns
            || then.respawns == self.respawns
                && then.screen <= self.screen
                && then.bells <= self.bells
                && then.done <= self.done
                && then.ended <= self.ended
    }
}

impl std::fmt::Display for Counts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Counts {
            respawns,
            screen,
            bells,
            done,
            ended,
        } = self;
        write!(f, "{respawns}.{screen}.{bells}.{done}.{}", u8::from(*ended))
    }
}

/// `RESPAWNS.SCREEN.BELLS.DONE.ENDED`, as counts are written.
impl std::str::FromStr for Counts {
    type Err = ();
    fn from_str(text: &str) -> Result<Counts, ()> {
        let mut parts = text.split('.');
        let mut next = || parts.next().and_then(crate::id::number::<u64>).ok_or(());
        let counts = Counts {
            respawns: next()?,
            screen: next()?,
            bells: next()?,
            done: next()?,
            ended: match next()? {
                0 => false,
                1 => true,
                _ => return Err(()),
            },
        };
        parts.next().is_none().then_some(counts).ok_or(())
    }
}

/// Where a pane stands: which server run, which pane, and its counts.
/// Written `%2:0.57.0.3.0:48211-1867a3f9c2b8e7d6`, one word, as every
/// action and wait answers it: so it names its own pane, and a server
/// tells its own runs' from any other's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub pane: PaneId,
    pub counts: Counts,
    pub run: Run,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.pane, self.counts, self.run)
    }
}

/// `PANE:COUNTS:RUN`, as a version is written.
impl std::str::FromStr for Version {
    type Err = ();
    fn from_str(text: &str) -> Result<Version, ()> {
        let mut parts = text.split(':');
        let version = Version {
            pane: parts.next().ok_or(())?.parse().map_err(drop)?,
            counts: parts.next().ok_or(())?.parse()?,
            run: parts.next().ok_or(())?.parse()?,
        };
        // One spelling only: `%2:0.057…` is no version's.
        let one = parts.next().is_none() && version.to_string() == text;
        one.then_some(version).ok_or(())
    }
}

wire! {
    /// What a wait waits from: the version of the pane it names, or each
    /// pane of a pane, tab or workspace as it begins.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Since {
        Now(target: AnyRef),
        Version(version: Version),
    }
}

wire! {
    /// Where `send-keys` sends: to a pane, or a tab's or workspace's every
    /// pane, as they are now; or to one pane only if it is still at a
    /// version seen (`expect`), checked as the keys are queued. `Since`'s
    /// shape, but its own type: a wait from a version is no check at one.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum To {
        Now(target: AnyRef),
        Seen(version: Version),
    }
}

impl To {
    /// The pane, tab or workspace it names.
    pub fn target(self) -> AnyRef {
        match self {
            To::Now(target) => target,
            To::Seen(version) => AnyRef::Pane(version.pane),
        }
    }
}

wire! {
    /// A wait: until a version of the panes it waits on differs from the
    /// one it waits from, or its pane closes; how long before it times out.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Wait {
        pub since: Since,
        /// How long before it times out, supplied by the caller.
        pub timeout: Limit,
    }
}

wire! {
    /// What `send-mouse` does.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Mouse {
        /// A press and its release.
        Click(button: Button),
        Press(button: Button),
        Release(button: Button),
        /// The wheel turned: one notch.
        Wheel(way: Wheel),
        /// The pointer moves, no button held.
        Move,
    }
}

wire! {
    /// A button `send-mouse` presses and releases: never the wheel.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Button {
        Left,
        Middle,
        Right,
        Back,
        Forward,
    }
}

wire! {
    /// A way the wheel turns, which is pressed and never released.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Wheel {
        Up,
        Down,
        Left,
        Right,
    }
}

impl Button {
    /// The button a report names.
    pub fn reported(self) -> MouseButton {
        match self {
            Self::Left => MouseButton::Left,
            Self::Middle => MouseButton::Middle,
            Self::Right => MouseButton::Right,
            Self::Back => MouseButton::Back,
            Self::Forward => MouseButton::Forward,
        }
    }
}

impl Wheel {
    /// The button a report names.
    pub fn reported(self) -> MouseButton {
        match self {
            Self::Up => MouseButton::WheelUp,
            Self::Down => MouseButton::WheelDown,
            Self::Left => MouseButton::WheelLeft,
            Self::Right => MouseButton::WheelRight,
        }
    }
}

wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Command {
        Ls {
            /// One pane, tab or workspace only.
            target: Option<AnyRef>,
            /// One field of its JSON.
            get: Option<String>,
        },
        /// Stops the server; from the CLI, refused from one of its own panes
        /// or with people attached.
        KillServer,
        NewWorkspace {
            name: Option<String>,
            size: Size,
            /// Where its shell starts (default: the caller's directory).
            cwd: Option<PathBuf>,
            env: EnvPatch,
            start: Start,
            options: PaneOptions,
        },
        NewTab {
            target: Option<WsRef>,
            name: Option<String>,
            size: Size,
            cwd: Option<PathBuf>,
            env: EnvPatch,
            start: Start,
            options: PaneOptions,
        },
        Split {
            axis: Axis,
            target: PaneId,
            /// Its launch: the shell or program role, with argv.
            start: Start,
            /// Where its shell starts (default: the caller's directory; from a
            /// client, the split pane's).
            cwd: Option<PathBuf>,
            env: EnvPatch,
            /// The shares of their split the split pane and the new one
            /// take (`shares`), if given; else the pane's is halved.
            shares: Option<(NonZeroU32, NonZeroU32)>,
            options: PaneOptions,
        },
        /// A program, started in place of a shell in a tab of its own, in a
        /// workspace no client is moved to.
        Run {
            workspace: WsRef,
            name: Option<String>,
            size: Size,
            cwd: Option<PathBuf>,
            env: EnvPatch,
            argv: Program,
            options: PaneOptions,
        },
        /// The program `run` started, started again in its pane.
        Respawn {
            target: PaneId,
            /// Ends it first if it still runs.
            kill: bool,
            env: EnvPatch,
            /// Another program instead: the one it ran without.
            argv: Option<Program>,
        },
        /// A notice on the clients showing the target: every client without.
        Notify {
            target: Option<AnyRef>,
            text: String,
        },
        Resize(what: Resized),
        SendMouse {
            target: PaneId,
            mouse: Mouse,
            row: u16,
            col: u16,
        },
        /// Closes a pane, a tab or a workspace.
        Kill {
            target: AnyRef,
        },
        /// A signal to what runs in the pane's foreground, what its terminal's
        /// keys reach (a job its shell runs, or the program `run` started), never
        /// a shell itself: for a program that ignores Ctrl-C and hangups.
        Signal {
            target: PaneId,
            signal: fuxix::process::Signal,
        },
        Rename {
            target: AnyRef,
            name: String,
        },
        MovePane {
            target: PaneId,
            to: MoveTo,
        },
        SwapPane {
            target: PaneId,
            with: SwapWith,
        },
        ResizePane {
            target: PaneId,
            by: PaneResize,
        },
        SendKeys {
            to: To,
            inputs: Vec<Input>,
        },
        /// What is there, as a layout file: everything, or one tab or
        /// workspace.
        DumpLayout {
            target: Option<AnyRef>,
        },
        /// Change only this pane's history budget.
        HistoryBudget {
            target: PaneId,
            bytes: HistoryBytes,
        },
        /// The pane's history, dropped.
        ClearHistory {
            target: PaneId,
        },
        /// What the pane shows, with up to `history` lines before it, as text
        /// and as JSON.
        CapturePane {
            target: PaneId,
            history: Option<usize>,
            view: CaptureView,
        },
        Reorder {
            target: AnyRef,
            toward: Sibling,
        },
        /// What a name or an id in text is: the pane, tab or workspace of
        /// `kind` it names (a tab's name, `%3`, …), answered with its id.
        Resolve {
            text: String,
            kind: Kind,
        },
        /// The panes or tabs a pane, tab or workspace is or holds,
        /// answered with their ids in layout order.
        Members {
            target: AnyRef,
            members: Members,
        },
        ListBuffers,
        ShowBuffer {
            buffer: Buffer,
        },
        PasteBuffer {
            buffer: Buffer,
            target: PaneId,
        },
        /// A command on one client's screen: the client named (`str c1 client`), or the one
        /// whose key, chooser or prompt ran it.
        Client {
            client: Option<ClientId>,
            action: ClientAction,
        },
    }
}

wire! {
    /// What `Members` answers of a place: its panes, or its tabs.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Members {
        Panes,
        Tabs,
    }
}

wire! {
    /// What a command does on a client's screen.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum ClientAction {
        Detach,
        /// What the client's terminal shows: the screen the server composes for
        /// it, bar and overlays included.
        Capture,
        /// The rows of a chooser the client shows: what each is, its label,
        /// and which is the client's current one.
        Choices(choices: Choices),
        /// fuxth for the client to run as its own: what opens one of its
        /// screens (a chooser, the prompt, copy mode), asked from elsewhere.
        Run(code: String),
        /// Keys as typed at its terminal: its bindings, overlays and prompts
        /// take them as a person's keys.
        Keys(inputs: Vec<Input>),
        Zoom,
        SelectPane(pick: PanePick),
        SelectTab(pick: Pick<TabId>),
        SelectWorkspace(pick: Pick<WsRef>),
    }
}

wire! {
    /// What a chooser lists.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Choices {
        /// The tabs of the client's workspace.
        Tabs,
        Workspaces,
        /// The other panes of this one's tab.
        Panes(pane: PaneId),
    }
}

/// A request the server cannot make sense of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Usage {
    /// A word that is not the pane, tab or workspace it must be.
    Not(Kind, String),
    NotClient(String),
}

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Not(kind, text) => write!(f, "{text:?} is not a {}", kind.name()),
            Self::NotClient(text) => write!(f, "{text:?} is not a client: clients are cN"),
        }
    }
}

impl std::error::Error for Usage {}

/// A workspace: `+N`, or its name.
pub fn parse_workspace(text: &str) -> Result<WsRef, Usage> {
    if text == "new-workspace" {
        return Ok(WsRef::New);
    }
    match text.parse::<WsId>() {
        Ok(id) => Ok(WsRef::Id(id)),
        Err(_) if text.starts_with('+') => Err(Usage::Not(Kind::Workspace, text.to_owned())),
        Err(_) => Ok(WsRef::Name(text.to_owned())),
    }
}

/// Refused, saying why, unless `name` is a variable's name a shell takes:
/// letters, digits and `_`, not first a digit.
pub fn var_name(name: &str) -> Result<(), String> {
    let first = (name.chars().next()).is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    match first && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        true => Ok(()),
        false => Err(format!(
            "{name:?} is no variable's name (letters, digits and _, not first a digit)"
        )),
    }
}

/// A variable a caller may give a pane, excluding its native identity.
pub(crate) fn pane_var(name: &str) -> Result<(), String> {
    var_name(name)?;
    if matches!(name, "FUX" | "FUX_PANE" | "FUX_SOCKET" | "FUX_SERVER") {
        return Err(format!("{name} is reserved for pane identity"));
    }
    Ok(())
}

/// Variables a pane's program is given: shell names other than its native
/// identity, checked from words and the wire alike.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvPatch(Vec<(String, Option<std::ffi::OsString>)>);

impl EnvPatch {
    /// The most distinct selected or explicit environment names in one launch.
    pub const MAX_NAMES: usize = 1024;
    /// A distinct checked name, either set to bytes or explicitly unset.
    pub fn push(&mut self, name: String, value: Option<std::ffi::OsString>) -> Result<(), String> {
        use std::os::unix::ffi::OsStrExt;
        pane_var(&name)?;
        if self.0.len() >= Self::MAX_NAMES {
            return Err("environment patch exceeds 1024 names".to_owned());
        }
        if self.0.iter().any(|(key, _)| key == &name) {
            return Err(format!("{name}: repeated environment name"));
        }
        if value.as_ref().is_some_and(|v| v.as_bytes().contains(&0)) {
            return Err(format!("{name}: an environment value holds no NUL"));
        }
        if self
            .encoded_len()
            .saturating_add(Self::entry_len(&name, &value))
            > crate::protocol::MAX_PAYLOAD
        {
            return Err("environment patch exceeds the launch frame limit".to_owned());
        }
        self.0.push((name, value));
        Ok(())
    }
    pub(crate) fn heap_bytes(&self) -> usize {
        self.0.iter().fold(
            self.0
                .capacity()
                .saturating_mul(std::mem::size_of::<(String, Option<std::ffi::OsString>)>()),
            |bytes, (name, value)| {
                bytes
                    .saturating_add(name.capacity())
                    .saturating_add(value.as_ref().map_or(0, std::ffi::OsString::capacity))
            },
        )
    }
    pub fn as_slice(&self) -> &[(String, Option<std::ffi::OsString>)] {
        &self.0
    }

    /// Explicit operation values replace selected or saved values.
    pub fn overlay(&mut self, other: &Self) -> Result<(), String> {
        let names = self
            .0
            .iter()
            .filter(|(key, _)| !other.0.iter().any(|(k, _)| k == key))
            .count()
            .saturating_add(other.0.len());
        if names > Self::MAX_NAMES {
            return Err("environment patch exceeds 1024 names".to_owned());
        }
        let bytes = self
            .0
            .iter()
            .filter(|(key, _)| !other.0.iter().any(|(k, _)| k == key))
            .fold(other.encoded_len(), |bytes, (k, v)| {
                bytes.saturating_add(Self::entry_len(k, v))
            });
        if bytes > crate::protocol::MAX_PAYLOAD {
            return Err("environment patch exceeds the launch frame limit".to_owned());
        }
        for (name, value) in &other.0 {
            self.0.retain(|(key, _)| key != name);
            self.0.push((name.clone(), value.clone()));
        }
        Ok(())
    }

    fn entry_len(name: &str, value: &Option<std::ffi::OsString>) -> usize {
        5usize
            .saturating_add(name.len())
            .saturating_add(value.as_ref().map_or(0, |v| 4usize.saturating_add(v.len())))
    }
    pub(crate) fn encoded_len(&self) -> usize {
        self.0.iter().fold(4usize, |bytes, (k, v)| {
            bytes.saturating_add(Self::entry_len(k, v))
        })
    }

    /// Capture only the selected names, including explicit absence.
    pub fn selected(names: &[EnvName]) -> Result<Self, String> {
        let mut patch = Self::default();
        for name in names {
            patch.push(name.as_str().to_owned(), std::env::var_os(name.as_str()))?;
        }
        Ok(patch)
    }
}

wire! {
    /// The role determines exit, signal and respawn semantics; argv is explicit.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Start {
        Shell(argv: Program),
        Program(argv: Program),
    }
}

impl Start {
    pub fn argv(&self) -> &Program {
        match self {
            Self::Shell(argv) | Self::Program(argv) => argv,
        }
    }
}

wire! {
    /// Policy supplied with every creation, retained by that pane.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PaneOptions {
        pub history_bytes: HistoryBytes,
        pub program_clipboard: bool,
    }
}

wire! {
    /// The explicit place an attachment shows, never created by attachment.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum AttachTarget {
        Workspace(workspace: WsRef),
        Tab(tab: TabId),
        Pane(pane: PaneId),
    }
}

wire! {
    /// Conditional first-pane inputs, used only by an empty writable attach.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct InitialWorkspace {
        pub name: Option<String>,
        pub start: Start,
        pub options: PaneOptions,
        pub cwd: PathBuf,
        pub env: EnvPatch,
    }
}

impl Command {
    /// Whether every client's screen is as it was after it: one answer per
    /// command, so a new command must give one.
    pub fn shows_nothing_new(&self) -> bool {
        use Command as C;
        match self {
            C::Ls { .. }
            | C::Resolve { .. }
            | C::Members { .. }
            | C::CapturePane { .. }
            | C::DumpLayout { .. }
            | C::SendKeys { .. }
            | C::SendMouse { .. }
            | C::Signal { .. }
            | C::ListBuffers
            | C::ShowBuffer { .. } => true,
            C::NewWorkspace { .. }
            | C::NewTab { .. }
            | C::Split { .. }
            | C::Run { .. }
            | C::Respawn { .. }
            | C::Notify { .. }
            | C::Resize(_)
            | C::Kill { .. }
            | C::Rename { .. }
            | C::MovePane { .. }
            | C::SwapPane { .. }
            | C::ResizePane { .. }
            | C::ClearHistory { .. }
            | C::HistoryBudget { .. }
            | C::Reorder { .. }
            | C::KillServer
            | C::PasteBuffer { .. } => false,
            C::Client { action, .. } => match action {
                ClientAction::Capture | ClientAction::Keys(_) | ClientAction::Run(_) => true,
                ClientAction::Detach
                | ClientAction::Choices(_)
                | ClientAction::Zoom
                | ClientAction::SelectPane(_)
                | ClientAction::SelectTab(_)
                | ClientAction::SelectWorkspace(_) => false,
            },
        }
    }
}

wire! {
    /// What a client asks of the server: a command, perhaps then a wait, or
    /// what only a client's request does, the server holding its connection.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Request {
        /// A command, at once.
        Do(command: Command),
        /// A wait in a pane, or a tab's or workspace's every pane, each of
        /// which answers.
        Wait(wait: Wait),
        /// What happens, a JSON line each as it happens, until the client
        /// goes: where the target is, or anywhere without one; from the
        /// cursor given, else from now.
        Events(target: Option<AnyRef>, from: Option<crate::events::Cursor>),
    }
}

/// A command, at once.
impl From<Command> for Request {
    fn from(command: Command) -> Request {
        Request::Do(command)
    }
}

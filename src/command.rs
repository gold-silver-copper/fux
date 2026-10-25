//! The requests a client sends the server: typed actions, each built by
//! fuxth words in a client (`crate::script`) and carried whole over the
//! socket (`crate::wire`). The server never reads words.
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::keys::mouse::MouseButton;
use crate::keys::{Direction, KeyPress};
use crate::layout::Axis;
use crate::wire::wire;
use std::num::NonZeroU32;
use std::path::PathBuf;

wire! {
    /// A workspace named on the command line: `+N` or its name.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum WsRef {
        Id(id: WsId),
        Name(name: String),
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
    /// or `focus`, `next-tab` and `next-workspace` (or `previous-…`) step from
    /// the current one.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Sibling {
        Next,
        Previous,
    }
}

wire! {
    /// Which tab or workspace `show-tab` or `show-workspace` picks.
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
    /// A pane's new width or height (`resize-x`, `resize-y`).
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
    /// How `resize-x`, `resize-y` and `move-border` resize a pane.
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
        /// Rows `start..=end`, from 0 at the top of what is captured.
        pub rows: Option<(u16, u16)>,
        /// Columns `start..=end`, from 0.
        pub cols: Option<(u16, u16)>,
    }
}

/// Which run of a server this is: its process and the moment it started,
/// in nanoseconds since 1970 (`48211-1867a3f9c2b8e7d6`). No two runs share
/// both: a process id is another process's only once this one ended, and
/// that one started at another moment. A restart is a run
/// of its own; so is a server on another socket. Its panes' `FUX_SERVER`.
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
/// `history-bytes` set; the same however the output was cut into reads),
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
    /// A wait: until a version of the panes it waits on differs from the
    /// one it waits from, or its pane closes; how long at most.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Wait {
        pub since: Since,
        /// How long at most; if unsaid, the server's `wait-seconds`.
        pub timeout: Option<crate::config::Limit>,
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
        /// or with people attached unless forced.
        KillServer {
            force: bool,
        },
        NewWorkspace {
            name: Option<String>,
            size: Size,
            /// Where its shell starts (default: the caller's directory).
            cwd: Option<PathBuf>,
            env: Env,
        },
        NewTab {
            target: Option<WsRef>,
            name: Option<String>,
            size: Size,
            cwd: Option<PathBuf>,
            env: Env,
        },
        Split {
            axis: Axis,
            target: PaneId,
            /// A program started in place of a shell, as `run` starts one,
            /// if given.
            argv: Vec<String>,
            /// Where its shell starts (default: where the split pane's is).
            cwd: Option<PathBuf>,
            env: Env,
            /// The shares of their split the split pane and the new one
            /// take (`shares`), if given; else the pane's is halved.
            shares: Option<(NonZeroU32, NonZeroU32)>,
        },
        /// A program, started in place of a shell in a tab of its own, in a
        /// workspace no client is moved to.
        Run {
            workspace: Option<WsRef>,
            name: Option<String>,
            size: Size,
            cwd: Option<PathBuf>,
            env: Env,
            argv: Vec<String>,
        },
        /// The program `run` started, started again in its pane.
        Respawn {
            target: PaneId,
            /// Ends it first if it still runs.
            kill: bool,
            env: Env,
            /// Another program instead: the one it ran without.
            argv: Vec<String>,
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
            /// The pane, or a tab's or workspace's every pane.
            target: AnyRef,
            inputs: Vec<Input>,
        },
        /// What is there, as a layout file: everything, or one tab or
        /// workspace.
        DumpLayout {
            target: Option<AnyRef>,
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
        /// A server setting (`history-bytes`, `clipboard`, …), as its value's
        /// text.
        Set {
            option: String,
            value: String,
        },
        /// What a name or an id in text is: the pane, tab or workspace of
        /// `kind` it names (a tab's name, `%3`, …), answered with its id.
        Resolve {
            text: String,
            kind: Kind,
        },
        /// The pane given the name `name` (by `rename`, or its program's),
        /// answered with its id.
        PaneNamed {
            name: String,
        },
        ListBuffers,
        ShowBuffer {
            index: usize,
        },
        PasteBuffer {
            index: usize,
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

/// Variables a pane's program is given, a name and a value each: every
/// name one `var_name` takes, from words and the wire alike.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env(Vec<(String, String)>);

impl Env {
    /// `name` set to `value` too: refused unless `name` is a variable's.
    pub fn push(&mut self, name: String, value: String) -> Result<(), String> {
        var_name(&name)?;
        self.0.push((name, value));
        Ok(())
    }
    pub fn as_slice(&self) -> &[(String, String)] {
        &self.0
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
            | C::PaneNamed { .. }
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
            | C::Reorder { .. }
            | C::KillServer { .. }
            | C::Set { .. }
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
        /// What happens, a line each as it happens, until the client goes:
        /// where the target is, or anywhere without one.
        Events(target: Option<AnyRef>, json: bool),
    }
}

/// A command, at once.
impl From<Command> for Request {
    fn from(command: Command) -> Request {
        Request::Do(command)
    }
}

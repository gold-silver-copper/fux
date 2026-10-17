//! The requests a client sends the server: typed actions, each built by
//! fuxth words in a client (`crate::script`) and carried whole over the
//! socket (`crate::wire`). The server never reads words.
use crate::id::{ClientId, PaneId, TabId, Wanted, WsId};
use crate::keys::mouse::MouseButton;
use crate::keys::{Direction, KeyPress};
use crate::layout::Axis;
use crate::log::Unit;
use crate::wire::wire;
use std::time::Duration;

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
    /// Every kind: a new one joins it, as it joins `name`'s match.
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
pub type Size = Option<fux_vt::Size>;

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
        Client(client: ClientId, size: fux_vt::Size),
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
        /// Soft-wrapped rows joined into the lines they wrap.
        pub join: bool,
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

wire! {
    /// What `wait` waits for.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Until {
        /// The text appears: on the screen, or in a line written since the wait
        /// began that has scrolled into history.
        Text(text: String),
        /// The text is no longer on the screen.
        Gone(text: String),
        /// The program exits.
        Exit,
        /// The program rings the bell or sends a desktop notification.
        Bell,
        /// The pane settles: its program reads its terminal, waits quiet on
        /// something else, or exits (`crate::ready`). What an action waits for
        /// when it names nothing.
        Settled,
    }
}

wire! {
    /// A wait: for any of its conditions, in each of its panes, how long at
    /// most, and how it is told.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Wait {
        pub until: Vec<Until>,
        /// How long at most; if unsaid, the server's `wait-seconds`.
        pub timeout: Option<Duration>,
        /// What came since it began, or the screen, as the wait ends.
        pub print: bool,
        /// What it prints with reverse video marked, as `capture`'s `marks`.
        pub marks: bool,
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
            cmd: Vec<String>,
            size: Size,
            /// Where its shell starts (default: the caller's directory).
            cwd: Option<std::path::PathBuf>,
            env: Env,
            /// The ids it makes, if asked for (`id`, as saved sessions have).
            ids: Wanted,
        },
        NewTab {
            target: Option<WsRef>,
            name: Option<String>,
            cmd: Vec<String>,
            size: Size,
            cwd: Option<std::path::PathBuf>,
            env: Env,
            ids: Wanted,
        },
        Split {
            axis: Axis,
            target: PaneId,
            cmd: Vec<String>,
            /// Where its shell starts (default: where the split pane's is).
            cwd: Option<std::path::PathBuf>,
            env: Env,
            /// `cmd` is the program, started in place of a shell, as `run`
            /// starts one.
            run: bool,
            /// The id it makes the pane with, if asked for.
            id: Option<PaneId>,
        },
        /// A program, started in place of a shell in a tab of its own, in a
        /// workspace no client is moved to.
        Run {
            workspace: Option<WsRef>,
            name: Option<String>,
            size: Size,
            cwd: Option<std::path::PathBuf>,
            env: Env,
            argv: Vec<String>,
            /// A file everything the program writes goes to too.
            log: Option<String>,
            ids: Wanted,
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
        /// What the pane's program writes, kept in a file from now on; `None`
        /// stops it.
        Log {
            target: PaneId,
            file: Option<String>,
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
        ListBuffers,
        ShowBuffer {
            index: usize,
        },
        PasteBuffer {
            index: usize,
            target: PaneId,
        },
        /// A command on one client's screen: the client named (`str c1 client`), or the one
        /// whose key, menu or prompt ran it.
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
        /// screens (a menu, the prompt, copy mode), asked from elsewhere.
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

impl ClientAction {
    /// The action on the client it comes from, as a key, a menu or the
    /// prompt runs it.
    pub fn here(self) -> Command {
        Command::Client {
            client: None,
            action: self,
        }
    }
}

/// A request the server cannot make sense of: exit status 2.
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

/// What a command reaches, which decides where it may run (a saved
/// session, a config) and whether a client shows anything new after it:
/// one answer per command, so a new command must give one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaches<'a> {
    /// Panes, tabs and workspaces, read; or a pane's program, handed input
    /// or a signal whose effect, if any, comes back as its output.
    Reads,
    /// Panes, tabs and workspaces, made, shaped, named or ended.
    Shapes,
    /// Their events, streamed until the reader goes: never ending of itself.
    Streams,
    /// The server itself: stopping it, granting a socket, revoking one.
    Server,
    /// A setting, to `value`.
    Settings { option: &'a str, value: &'a str },
    /// The paste buffers; `reads` if it only reads them.
    Buffers { reads: bool },
    /// A client's screen; `reads` if it only reads it or types at it (the
    /// keys take effect as the client's own).
    Client { reads: bool },
}

impl Reaches<'_> {
    /// Whether every client's screen is as it was after it.
    pub fn shows_nothing_new(self) -> bool {
        match self {
            Self::Reads | Self::Streams => true,
            Self::Shapes | Self::Server | Self::Settings { .. } => false,
            Self::Buffers { reads } | Self::Client { reads } => reads,
        }
    }
}

impl Command {
    /// What it reaches.
    pub fn reaches(&self) -> Reaches<'_> {
        use Command as C;
        match self {
            C::Ls { .. }
            | C::Resolve { .. }
            | C::CapturePane { .. }
            | C::DumpLayout { .. }
            | C::SendKeys { .. }
            | C::SendMouse { .. }
            | C::Signal { .. } => Reaches::Reads,
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
            | C::Log { .. }
            | C::ClearHistory { .. }
            | C::Reorder { .. } => Reaches::Shapes,
            C::KillServer { .. } => Reaches::Server,
            C::Set { option, value } => Reaches::Settings { option, value },
            C::ListBuffers | C::ShowBuffer { .. } => Reaches::Buffers { reads: true },
            C::PasteBuffer { .. } => Reaches::Buffers { reads: false },
            C::Client { action, .. } => Reaches::Client {
                reads: match action {
                    ClientAction::Capture | ClientAction::Keys(_) | ClientAction::Run(_) => true,
                    ClientAction::Detach
                    | ClientAction::Choices(_)
                    | ClientAction::Zoom
                    | ClientAction::SelectPane(_)
                    | ClientAction::SelectTab(_)
                    | ClientAction::SelectWorkspace(_) => false,
                },
            },
        }
    }

    /// When a wait it then does is armed (`Request::Then`), if it does one.
    pub fn armed(&self) -> Option<Armed> {
        use Command as C;
        match self {
            C::SendKeys { target, .. } => Some(Armed::Before(*target)),
            C::SendMouse { target, .. } | C::Signal { target, .. } => {
                Some(Armed::Before(AnyRef::Pane(*target)))
            }
            C::Run { .. } | C::Split { .. } | C::Respawn { .. } => Some(Armed::Started),
            C::Ls { .. }
            | C::KillServer { .. }
            | C::NewWorkspace { .. }
            | C::NewTab { .. }
            | C::Notify { .. }
            | C::Resize(_)
            | C::Kill { .. }
            | C::Rename { .. }
            | C::MovePane { .. }
            | C::SwapPane { .. }
            | C::ResizePane { .. }
            | C::DumpLayout { .. }
            | C::Log { .. }
            | C::ClearHistory { .. }
            | C::CapturePane { .. }
            | C::Reorder { .. }
            | C::Set { .. }
            | C::Resolve { .. }
            | C::ListBuffers
            | C::ShowBuffer { .. }
            | C::PasteBuffer { .. }
            | C::Client { .. } => None,
        }
    }
}

/// When the wait a command then does is armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Armed {
    /// Before it acts, in these panes: what its input or signal brings is
    /// what the wait sees (send-keys, send-mouse, signal).
    Before(AnyRef),
    /// Once it has started its pane's program, on that pane, before the
    /// server reads what the program writes (run, split, respawn).
    Started,
}

wire! {
    /// What a client asks of the server: a command, perhaps then a wait, or
    /// what only a client's request does, the server holding its connection.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Request {
        /// A command, at once.
        Do(command: Command),
        /// A command, then a wait, armed as the command's `armed` says.
        Then(command: Command, wait: Box<Wait>),
        /// A wait in the pane, or a tab's or workspace's every pane, each of
        /// which answers.
        Wait(target: AnyRef, wait: Wait),
        /// A capture of the pane (its history and view) read once it
        /// settles, waiting this long at most (if unsaid, the server's
        /// `wait-seconds`), and said with its state.
        Capture(
            target: PaneId,
            history: Option<usize>,
            view: CaptureView,
            timeout: Option<Duration>
        ),
        /// What happens, a line each as it happens, until the client goes:
        /// where the target is, or anywhere without one.
        Events(target: Option<AnyRef>, json: bool),
        /// A socket whose commands reach workspace `workspace` alone.
        Grant(workspace: WsRef, socket: String),
        /// The socket a grant made, closed.
        Revoke(socket: String),
    }
}

/// A command, at once.
impl From<Command> for Request {
    fn from(command: Command) -> Request {
        Request::Do(command)
    }
}

impl Request {
    /// What it reaches.
    pub fn reaches(&self) -> Reaches<'_> {
        match self {
            Self::Do(command) | Self::Then(command, _) => command.reaches(),
            Self::Wait(..) | Self::Capture(..) => Reaches::Reads,
            Self::Events(..) => Reaches::Streams,
            Self::Grant(..) | Self::Revoke(_) => Reaches::Server,
        }
    }
}

/// What a key is to a program reading it (`crate::log::Unit`): an edit
/// of the line typed (a character, BSpace, Delete, Tab, the cursor keys,
/// and the line editor's control keys that move or delete), else a key
/// that does something (Enter, Escape, a control or function key).
pub fn unit(key: KeyPress) -> Unit {
    use fux_vt::keys::{Key, Modifiers};
    match (key.key, key.mods) {
        (Key::Char(_), Modifiers::NONE | Modifiers::SHIFT)
        | (Key::Char('a' | 'b' | 'e' | 'f' | 'h' | 'k' | 'u' | 'w' | 'y'), Modifiers::CTRL)
        | (Key::Char('b' | 'd' | 'f'), Modifiers::ALT)
        | (
            Key::Backspace | Key::Delete | Key::Tab | Key::Arrow(_) | Key::Home | Key::End,
            Modifiers::NONE,
        ) => Unit::Edit,
        _ => Unit::Submit,
    }
}

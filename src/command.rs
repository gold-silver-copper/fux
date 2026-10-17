//! The requests a client sends the server: typed actions, each built by
//! fuxth words in a client (`crate::script`) and carried whole over the
//! socket (`crate::wire`). The server never reads words.
use crate::id::{ClientId, PaneId, TabId, Wanted, WsId};
use crate::keys::mouse::MouseButton;
use crate::keys::{Direction, KeyPress};
use crate::layout::Axis;
use std::time::Duration;

/// A workspace named on the command line: `+N` or its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsRef {
    Id(WsId),
    Name(String),
}

/// Any target: a pane, a tab or a workspace (`%N`, `@N`, `+N`), as a
/// client's words found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnyRef {
    Pane(PaneId),
    Tab(TabId),
    Workspace(WsId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pane,
    Tab,
    Workspace,
}

impl AnyRef {
    pub fn kind(&self) -> Kind {
        match self {
            AnyRef::Pane(_) => Kind::Pane,
            AnyRef::Tab(_) => Kind::Tab,
            AnyRef::Workspace(_) => Kind::Workspace,
        }
    }
}

/// An id as it is written: `%3`, `@2`, `+1`.
impl std::fmt::Display for AnyRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnyRef::Pane(id) => id.fmt(f),
            AnyRef::Tab(id) => id.fmt(f),
            AnyRef::Workspace(id) => id.fmt(f),
        }
    }
}

/// `%N`, `@N` or `+N`: the id of the kind its sigil says.
impl std::str::FromStr for AnyRef {
    type Err = ();
    fn from_str(text: &str) -> Result<AnyRef, ()> {
        (text.parse().map(AnyRef::Pane))
            .or_else(|_| text.parse().map(AnyRef::Tab))
            .or_else(|_| text.parse().map(AnyRef::Workspace))
            .map_err(drop)
    }
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Pane => "pane",
            Kind::Tab => "tab",
            Kind::Workspace => "workspace",
        }
    }
}

/// Which way `reorder` moves a pane, tab or workspace among its siblings,
/// or `focus`, `next-tab` and `next-workspace` (or `previous-…`) step from
/// the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sibling {
    Next,
    Previous,
}

/// Which tab or workspace `show-tab` or `show-workspace` picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick<T> {
    Step(Sibling),
    Id(T),
}

/// Which pane `focus` picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanePick {
    Step(Sibling),
    Id(PaneId),
    /// The previously focused pane.
    Last,
    Toward(Direction),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveTo {
    Tab(TabId),
    Workspace(WsRef),
    NewTab,
    NewWorkspace,
    Beside(Direction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapWith {
    Pane(PaneId),
    Toward(Direction),
}

/// How a tab no client shows is laid out (`size`, `resize`): `None`,
/// on a client's screen.
pub type Size = Option<fux_vt::Size>;

/// A pane's new width or height (`resize-x`, `resize-y`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extent {
    Cells(u16),
    /// Of the tab's width or height.
    Percent(Percent),
}

/// A share, 1 to 100 percent: there is no other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Percent(u8);

impl Percent {
    pub fn new(n: u8) -> Option<Percent> {
        (1..=100).contains(&n).then_some(Percent(n))
    }
    pub fn get(self) -> u8 {
        self.0
    }
    /// Its share of `of`, at least 1.
    pub fn of(self, of: u16) -> u16 {
        let share = u32::from(of).saturating_mul(u32::from(self.0)) / 100;
        // At most `of`, which a u16 holds.
        u16::try_from(share).unwrap_or(of).max(1)
    }
}

/// How `resize-x`, `resize-y` and `move-border` resize a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneResize {
    /// Moves the border toward `direction` by this many cells.
    Border(Direction, u16),
    /// To a width (horizontal) or a height (vertical).
    To(Axis, Extent),
}

/// What `resize` sizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resized {
    /// A tab, or every tab of a workspace: what they are laid out on while
    /// no client shows them, or as the clients showing them are.
    Tabs(AnyRef, Size),
    /// A headless client's screen.
    Client(ClientId, fux_vt::Size),
}

/// One thing `send-keys` sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// A key, encoded as the pane's program asked for keys.
    Key(KeyPress),
    /// Text, as typed.
    Text(String),
    /// Text as a paste: bracketed if the program asked for bracketed paste.
    Paste(String),
}

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

/// Which part of a pane's screen `capture` prints, and how.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureView {
    /// Every row as it is, blank rows at the end and all, rather than
    /// trimmed.
    pub full: bool,
    /// Soft-wrapped rows joined into the lines they wrap.
    pub join: bool,
    /// Rows `start..=end`, from 0 at the top of what is captured.
    pub rows: Option<(u16, u16)>,
    /// Columns `start..=end`, from 0.
    pub cols: Option<(u16, u16)>,
    /// Colours and attributes, as SGR escapes, or with `json` as runs.
    pub styles: bool,
    /// Which screen it reads.
    pub source: Source,
    /// Reverse video, how a selection is drawn, between `⟦` and `⟧`.
    pub marks: bool,
}

/// What `wait` waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Until {
    /// The text appears: on the screen, or in a line written since the wait
    /// began that has scrolled into history.
    Text(String),
    /// The text is no longer on the screen.
    Gone(String),
    /// The program exits.
    Exit,
    /// The program rings the bell or sends a desktop notification.
    Bell,
    /// The pane settles: its program reads its terminal, waits quiet on
    /// something else, or exits (`crate::ready`). What an action waits for
    /// when it names nothing.
    Settled,
}

/// A wait: for any of its conditions, in each of its panes, how long at
/// most, and how it is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    pub until: Vec<Until>,
    /// How long at most; if unsaid, the server's `wait-seconds`.
    pub timeout: Option<Duration>,
    /// What came since it began, or the screen, as the wait ends.
    pub print: bool,
    /// Only what comes after it began counts: text that appears anew, a
    /// screen that changed (send-keys' waits, armed before the keys).
    pub after: bool,
    /// What it prints with reverse video marked, as `capture`'s `marks`.
    pub marks: bool,
}

/// What `send-mouse` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mouse {
    /// A press and its release.
    Click(Button),
    Press(Button),
    Release(Button),
    /// The wheel turned: one notch.
    Wheel(Wheel),
    /// The pointer moves, no button held.
    Move,
}

/// A button `send-mouse` presses and releases: never the wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

/// A way the wheel turns, which is pressed and never released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wheel {
    Up,
    Down,
    Left,
    Right,
}

impl Button {
    /// The button a report names.
    pub fn reported(self) -> MouseButton {
        match self {
            Button::Left => MouseButton::Left,
            Button::Middle => MouseButton::Middle,
            Button::Right => MouseButton::Right,
            Button::Back => MouseButton::Back,
            Button::Forward => MouseButton::Forward,
        }
    }
}

impl Wheel {
    /// The button a report names.
    pub fn reported(self) -> MouseButton {
        match self {
            Wheel::Up => MouseButton::WheelUp,
            Wheel::Down => MouseButton::WheelDown,
            Wheel::Left => MouseButton::WheelLeft,
            Wheel::Right => MouseButton::WheelRight,
        }
    }
}

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
        cwd: Option<String>,
        env: Vec<(String, String)>,
        /// The ids it makes, if asked for (`id`, as saved sessions have).
        ids: Wanted,
    },
    NewTab {
        target: Option<WsRef>,
        name: Option<String>,
        cmd: Vec<String>,
        size: Size,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        ids: Wanted,
    },
    Split {
        axis: Axis,
        target: PaneId,
        cmd: Vec<String>,
        /// Where its shell starts (default: where the split pane's is).
        cwd: Option<String>,
        env: Vec<(String, String)>,
        /// `cmd` is the program, started in place of a shell, as `run`
        /// starts one.
        run: bool,
        /// The id it makes the pane with, if asked for.
        id: Option<PaneId>,
        /// Then waits on the new pane, armed before its program writes.
        then: Option<Box<Wait>>,
    },
    /// A program, started in place of a shell in a tab of its own, in a
    /// workspace no client is moved to.
    Run {
        workspace: Option<WsRef>,
        name: Option<String>,
        size: Size,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        argv: Vec<String>,
        /// A file everything the program writes goes to too.
        log: Option<String>,
        ids: Wanted,
        /// Then waits, armed before the program writes anything.
        then: Option<Box<Wait>>,
    },
    /// The program `run` started, started again in its pane.
    Respawn {
        target: PaneId,
        /// Ends it first if it still runs.
        kill: bool,
        env: Vec<(String, String)>,
        /// Another program instead: the one it ran without.
        argv: Vec<String>,
        /// Then waits, as run's does.
        then: Option<Box<Wait>>,
    },
    /// A notice on the clients showing the target: every client without.
    Notify {
        target: Option<AnyRef>,
        text: String,
    },
    Resize(Resized),
    Wait {
        /// The pane, or a tab's or workspace's every pane, each of which
        /// answers.
        target: AnyRef,
        wait: Wait,
    },
    SendMouse {
        target: PaneId,
        mouse: Mouse,
        row: u16,
        col: u16,
        /// Then waits, armed before the event goes in, as send-keys' does.
        then: Option<Box<Wait>>,
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
        then: Option<Box<Wait>>,
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
        /// A wait for the program to go quiet once they are sent.
        then: Option<Box<Wait>>,
    },
    /// What happens, a line each as it happens, until the client goes:
    /// where the target is, or anywhere without one.
    Events {
        target: Option<AnyRef>,
        json: bool,
    },
    /// What is there, as a layout file: everything, or one tab or
    /// workspace.
    DumpLayout {
        target: Option<AnyRef>,
    },
    /// A socket whose commands reach workspace `workspace` alone.
    Grant {
        workspace: WsRef,
        socket: String,
    },
    /// The socket a grant made, closed.
    Revoke {
        socket: String,
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
        /// Read once the pane settles, waiting this long at most (if unsaid,
        /// the server's `wait-seconds`), and said with its state; at once,
        /// as it is, without.
        settle: Option<Option<Duration>>,
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

/// What a command does on a client's screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientAction {
    Detach,
    /// What the client's terminal shows: the screen the server composes for
    /// it, bar and overlays included.
    Capture,
    /// The rows of a chooser the client shows: what each is, its label,
    /// and which is the client's current one.
    Choices(Choices),
    /// fuxth for the client to run as its own: what opens one of its
    /// screens (a menu, the prompt, copy mode), asked from elsewhere.
    Run(String),
    /// Keys as typed at its terminal: its bindings, overlays and prompts
    /// take them as a person's keys.
    Keys(Vec<Input>),
    Zoom,
    SelectPane(PanePick),
    SelectTab(Pick<TabId>),
    SelectWorkspace(Pick<WsRef>),
}

/// What a chooser lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choices {
    /// The tabs of the client's workspace.
    Tabs,
    Workspaces,
    /// The other panes of this one's tab.
    Panes(PaneId),
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
            Usage::Not(kind, text) => write!(f, "{text:?} is not a {}", kind.name()),
            Usage::NotClient(text) => write!(f, "{text:?} is not a client: clients are cN"),
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

/// Whether `name` is a variable's name a shell takes: letters, digits and
/// `_`, not first a digit.
pub fn shell_name(name: &str) -> bool {
    (name.chars().next()).is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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
            Reaches::Reads | Reaches::Streams => true,
            Reaches::Shapes | Reaches::Server | Reaches::Settings { .. } => false,
            Reaches::Buffers { reads } | Reaches::Client { reads } => reads,
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
            | C::Wait { .. }
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
            C::Events { .. } => Reaches::Streams,
            C::KillServer { .. } | C::Grant { .. } | C::Revoke { .. } => Reaches::Server,
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

    /// The pane a command acts on and the wait it then does, armed before
    /// it acts: send-keys', send-mouse's and signal's.
    pub fn acting_wait(&self) -> Option<(AnyRef, Wait)> {
        if let Command::SendKeys {
            target,
            then: Some(wait),
            ..
        } = self
        {
            return Some((*target, (**wait).clone()));
        }
        if let Command::SendMouse {
            target,
            then: Some(wait),
            ..
        }
        | Command::Signal {
            target,
            then: Some(wait),
            ..
        } = self
        {
            return Some((AnyRef::Pane(*target), (**wait).clone()));
        }
        None
    }

    /// Takes the wait `run`, `split` or `respawn` does once it has started
    /// its program: the command then says only its pane, which the wait
    /// answers of.
    pub fn take_start_wait(&mut self) -> Option<Box<Wait>> {
        if let Command::Run { then, .. }
        | Command::Split { then, .. }
        | Command::Respawn { then, .. } = self
        {
            return then.take();
        }
        None
    }
}

/// What a key is to a program reading it (`crate::log::Unit`): an edit
/// of the line typed (a character, BSpace, Delete, Tab, the cursor keys,
/// and the line editor's control keys that move or delete), else a key
/// that does something (Enter, Escape, a control or function key).
pub fn unit(key: KeyPress) -> crate::log::Unit {
    use crate::log::Unit;
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

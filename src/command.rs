//! The command grammar: the one set of commands that the CLI, key bindings,
//! the command prompt and the config file all speak.
use crate::id::{ClientId, PaneId, TabId, WsId, number};
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

/// Any target: `%N`, `@N`, `+N` or a workspace name; once found, a
/// workspace by its ID (`AnyRef<WsId>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnyRef<W = WsRef> {
    Pane(PaneId),
    Tab(TabId),
    Workspace(W),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pane,
    Tab,
    Workspace,
}

impl<W> AnyRef<W> {
    pub fn kind(&self) -> Kind {
        match self {
            AnyRef::Pane(_) => Kind::Pane,
            AnyRef::Tab(_) => Kind::Tab,
            AnyRef::Workspace(_) => Kind::Workspace,
        }
    }
    /// The same target, its workspace as `f` gives it.
    pub fn map<V>(self, f: impl FnOnce(W) -> V) -> AnyRef<V> {
        match self {
            AnyRef::Pane(p) => AnyRef::Pane(p),
            AnyRef::Tab(t) => AnyRef::Tab(t),
            AnyRef::Workspace(w) => AnyRef::Workspace(f(w)),
        }
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
/// or `select-…` steps from the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sibling {
    Next,
    Previous,
}

/// Which tab or workspace `select-tab` or `select-workspace` picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick<T> {
    Step(Sibling),
    Id(T),
}

/// Which pane `select-pane` picks.
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

/// A pane, tab or workspace a command acts on: the one given, or (`None`)
/// the client's or the pane's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    Pane(Option<PaneId>),
    Tab(Option<TabId>),
    Workspace(Option<WsRef>),
}

impl Subject {
    pub fn kind(&self) -> Kind {
        match self {
            Subject::Pane(_) => Kind::Pane,
            Subject::Tab(_) => Kind::Tab,
            Subject::Workspace(_) => Kind::Workspace,
        }
    }
    /// The client's or the pane's own of `kind`.
    fn own(kind: Kind) -> Subject {
        match kind {
            Kind::Pane => Subject::Pane(None),
            Kind::Tab => Subject::Tab(None),
            Kind::Workspace => Subject::Workspace(None),
        }
    }
}

impl From<AnyRef> for Subject {
    fn from(target: AnyRef) -> Subject {
        match target {
            AnyRef::Pane(p) => Subject::Pane(Some(p)),
            AnyRef::Tab(t) => Subject::Tab(Some(t)),
            AnyRef::Workspace(w) => Subject::Workspace(Some(w)),
        }
    }
}

/// How a tab no client shows is laid out (`--size`, `fux resize`): `None`,
/// on a client's screen.
pub type Size = Option<fux_vt::Size>;

/// A pane's new width or height (`resize-pane -x`, `-y`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extent {
    Cells(u16),
    /// Of the tab's width or height.
    Percent(u8),
}

/// How `resize-pane` resizes a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneResize {
    /// Moves the border toward `direction` by this many cells.
    Border(Direction, u16),
    /// To a width, a height, or both.
    To {
        cols: Option<Extent>,
        rows: Option<Extent>,
    },
}

/// What `resize` sizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resized {
    /// A tab, or every tab of a workspace: what they are laid out on while
    /// no client shows them.
    Tabs(AnyRef),
    /// A headless client's screen.
    Client(ClientId),
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

/// Which part of a pane's screen `capture-pane` prints, and how.
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
    /// Colours and attributes, as SGR escapes, or with `--json` as runs.
    pub styles: bool,
}

/// What `wait` waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Until {
    /// Any of these texts is on the screen.
    Text(Vec<String>),
    /// None of these texts is on the screen.
    Gone(Vec<String>),
    /// The screen has shown no change for this long: the same screen
    /// drawn again is none.
    Idle(Duration),
    /// The screen changed at all.
    Change,
    /// The program exited, or its pane closed.
    Exit,
}

/// A wait: what for, how long at most, and whether to print the screen as
/// it ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    pub until: Until,
    pub timeout: Duration,
    pub print: bool,
}

/// How long a wait lasts at most if `--timeout` does not say: well inside
/// what a tool running a command waits for it.
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(60);

/// What `send-mouse` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mouse {
    /// A press and its release.
    Click(MouseButton),
    Press(MouseButton),
    Release(MouseButton),
    /// The wheel turned: one notch.
    Wheel(MouseButton),
    /// The pointer moves, no button held.
    Move,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Ls {
        json: bool,
    },
    KillServer,
    ListKeys,
    NewWorkspace {
        name: Option<String>,
        cmd: Vec<String>,
        size: Size,
        json: bool,
    },
    NewTab {
        target: Option<WsRef>,
        name: Option<String>,
        cmd: Vec<String>,
        size: Size,
        json: bool,
    },
    Split {
        axis: Axis,
        target: Option<PaneId>,
        cmd: Vec<String>,
        json: bool,
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
        json: bool,
    },
    Resize {
        what: Resized,
        size: Size,
    },
    Wait {
        target: Option<PaneId>,
        wait: Wait,
    },
    SendMouse {
        target: Option<PaneId>,
        mouse: Mouse,
        row: u16,
        col: u16,
    },
    KillPane {
        target: Option<PaneId>,
    },
    KillTab {
        target: Option<TabId>,
    },
    KillWorkspace {
        target: Option<WsRef>,
    },
    Rename {
        target: AnyRef,
        name: String,
    },
    MovePane {
        target: Option<PaneId>,
        to: MoveTo,
    },
    SwapPane {
        target: Option<PaneId>,
        with: SwapWith,
    },
    ResizePane {
        target: Option<PaneId>,
        by: PaneResize,
    },
    SendKeys {
        target: Option<PaneId>,
        inputs: Vec<Input>,
        /// How many times to send them all.
        repeat: u16,
        /// A wait for the program to go quiet once they are sent.
        then: Option<Wait>,
    },
    /// The prefix, sent to a pane as its program asked for keys.
    SendPrefix {
        target: Option<PaneId>,
    },
    CapturePane {
        target: Option<PaneId>,
        history: Option<usize>,
        view: CaptureView,
        json: bool,
    },
    Terminate {
        target: Option<PaneId>,
    },
    Reorder {
        subject: Subject,
        toward: Sibling,
    },
    /// `set`, `bind`, `unbind` or `unbind-all`, as the config file has
    /// them: the whole line, for `Config::apply`.
    Configure {
        argv: Vec<String>,
    },
    Reload,
    ListBuffers,
    ShowBuffer {
        index: usize,
    },
    PasteBuffer {
        index: usize,
        target: Option<PaneId>,
    },
    /// A command on one client's screen: the client `-c` names, or the one
    /// whose key, menu or prompt ran it.
    Client {
        client: Option<ClientId>,
        action: ClientAction,
    },
}

impl Command {
    /// The pane, tab or workspace it acts on, for those that take any.
    fn subject(&self) -> Option<&Subject> {
        let (Command::Reorder { subject, .. }
        | Command::Client {
            action:
                ClientAction::Menu(subject)
                | ClientAction::RenamePrompt(subject)
                | ClientAction::ConfirmClose(subject),
            ..
        }) = self
        else {
            return None;
        };
        Some(subject)
    }
}

/// What a command does on a client's screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientAction {
    Detach,
    /// What the client's terminal shows: the screen the server composes for
    /// it, bar and overlays included.
    Capture {
        json: bool,
    },
    CommandColumn,
    /// `moving`: the pane to move to the tab chosen, if one: the one given,
    /// or (`None`) the focused one.
    ChooseTab {
        moving: Option<Option<PaneId>>,
    },
    ChooseWorkspace {
        moving: Option<Option<PaneId>>,
    },
    ChoosePane {
        target: Option<PaneId>,
    },
    Menu(Subject),
    CommandPrompt,
    CopyMode,
    RenamePrompt(Subject),
    ConfirmClose(Subject),
    Zoom,
    SelectPane(PanePick),
    SelectTab(Pick<TabId>),
    SelectWorkspace(Pick<WsRef>),
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

/// A command line that is not a valid command: exit status 2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Usage {
    NoCommand,
    UnknownCommand(String),
    /// A word that is not the pane, tab or workspace it must be.
    Not(Kind, String),
    NotClient(String),
    /// A flag of `command` given no value.
    NeedsValue {
        command: &'static str,
        flag: String,
    },
    UnknownFlag {
        command: &'static str,
        flag: String,
    },
    Unexpected {
        command: &'static str,
        word: String,
    },
    /// A word that is not `pane`, `tab` or `workspace`.
    NotKind {
        command: &'static str,
        word: String,
    },
    NotBuffer {
        command: &'static str,
        value: String,
    },
    NotCells(String),
    NotLines(String),
    /// A flag's value that is not what the flag takes: `expected` says what.
    Bad {
        command: &'static str,
        flag: &'static str,
        value: String,
        expected: &'static str,
    },
    /// Words after `--` for a command that takes none.
    NoCommandAfter {
        command: &'static str,
    },
    /// What a command needs and was not given, in its own words.
    Needs(&'static str),
}

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Usage::NoCommand => f.write_str("no command given; `fux help` lists commands"),
            Usage::UnknownCommand(name) => match tmux_command(name) {
                Some(hint) => write!(f, "unknown command {name:?}; {hint}"),
                None => write!(f, "unknown command {name:?}; `fux help` lists commands"),
            },
            Usage::Not(kind, text) => write!(
                f,
                "{text:?} is not a {}; {}",
                kind.name(),
                match kind {
                    Kind::Pane => "panes are %N",
                    Kind::Tab => "tabs are @N",
                    Kind::Workspace => "workspaces are +N or a name",
                }
            ),
            Usage::NotClient(text) => {
                write!(f, "{text:?} is not a client; `fux ls` lists clients as cN")
            }
            Usage::NeedsValue { command, flag } => write!(f, "{command} {flag} needs a value"),
            Usage::UnknownFlag { command, flag } => match tmux_flag(command, flag) {
                Some(hint) => write!(f, "{command}: unknown flag {flag}; {hint}"),
                None => write!(f, "{command}: unknown flag {flag}"),
            },
            Usage::Unexpected { command, word } => {
                write!(f, "{command}: unexpected argument {word:?}")
            }
            Usage::NotKind { command, word } => {
                write!(f, "{command}: {word:?} is not pane, tab or workspace")
            }
            Usage::NotBuffer { command, value } => {
                write!(f, "{command} -b: {value:?} is not a buffer number")
            }
            Usage::NotCells(text) => write!(f, "resize-pane: {text:?} is not a number of cells"),
            Usage::NotLines(text) => write!(f, "capture-pane -S: {text:?} is not -N"),
            Usage::Bad {
                command,
                flag,
                value,
                expected,
            } => write!(f, "{command} {flag}: {value:?} is not {expected}"),
            Usage::NoCommandAfter { command } => write!(f, "{command} takes no command after --"),
            Usage::Needs(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for Usage {}

/// What fux has for a command of tmux's, named as tmux names it: a hint,
/// not another name for it.
fn tmux_command(name: &str) -> Option<&'static str> {
    Some(match name {
        "list-panes" | "lsp" | "list-windows" | "lsw" | "list-sessions" | "list-clients" => {
            "`fux ls` lists workspaces, tabs, panes and clients"
        }
        "new-window" | "neww" => "tabs are made with `fux new-tab`",
        "new-session" | "new" => "workspaces are made with `fux new-workspace`",
        "split-window" | "splitw" => "panes are split with `fux split -h` or `-v`",
        "kill-window" | "killw" => "tabs are closed with `fux kill-tab`",
        "kill-session" => "workspaces are closed with `fux kill-workspace`",
        "select-window" | "selectw" => "tabs are picked with `fux select-tab`",
        "rename-window" | "renamew" | "rename-session" => "use `fux rename -t TARGET NAME`",
        "resize-window" | "resizew" => "a tab nobody views is sized with `fux resize`",
        "attach-session" | "a" | "at" => "attach with `fux` alone",
        "detach-client" => "use `fux detach -c CLIENT`",
        "source-file" | "source" => "the config file is read again with `fux reload`",
        "set-option" | "set-window-option" | "setw" => "options are set with `fux set`",
        "wait-for" => "`fux wait` waits for a pane to show text, go quiet or exit",
        "respawn-pane" | "respawnp" => "programs are started with `fux run`",
        _ => return None,
    })
}

/// What fux has for a flag of tmux's that a fux command does not take.
fn tmux_flag(command: &str, flag: &str) -> Option<&'static str> {
    Some(match (command, flag) {
        ("capture-pane", "-p") => "capture-pane prints the screen without it",
        ("capture-pane", "-e") => "--styles keeps colours and attributes",
        ("resize-pane", "-Z") => "zoom with `fux zoom`",
        ("split" | "new-tab" | "new-workspace", "-d") => {
            "`fux run` starts a program out of every client's way"
        }
        _ => return None,
    })
}

pub fn parse_pane(text: &str) -> Result<PaneId, Usage> {
    text.parse()
}
pub fn parse_tab(text: &str) -> Result<TabId, Usage> {
    text.parse()
}
pub fn parse_workspace(text: &str) -> Result<WsRef, Usage> {
    if text.starts_with('+') {
        return text.parse().map(WsRef::Id);
    }
    if text.is_empty() || text.starts_with(['%', '@', '-']) {
        return Err(Usage::Not(Kind::Workspace, text.to_owned()));
    }
    Ok(WsRef::Name(text.to_owned()))
}
pub fn parse_any(text: &str) -> Result<AnyRef, Usage> {
    if text.starts_with('%') {
        parse_pane(text).map(AnyRef::Pane)
    } else if text.starts_with('@') {
        parse_tab(text).map(AnyRef::Tab)
    } else {
        parse_workspace(text).map(AnyRef::Workspace)
    }
}
pub fn parse_client(text: &str) -> Result<ClientId, Usage> {
    text.parse()
}
fn direction_flag(flag: &str) -> Option<Direction> {
    Direction::ALL.into_iter().find(|d| d.flag() == flag)
}

/// Where `move-pane --to` sends a pane.
fn parse_move_to(value: &str) -> Result<MoveTo, Usage> {
    Ok(match value {
        "new-tab" => MoveTo::NewTab,
        "new-workspace" => MoveTo::NewWorkspace,
        v if v.starts_with('@') => MoveTo::Tab(parse_tab(v)?),
        v => MoveTo::Workspace(parse_workspace(v)?),
    })
}

/// `COLSxROWS`, as `--size` takes it.
fn parse_size(text: &str) -> Option<fux_vt::Size> {
    let (cols, rows) = text.split_once('x')?;
    let cells = |n: &str| number(n).and_then(|n| u16::try_from(n).ok());
    fux_vt::Size::new(cells(rows)?, cells(cols)?).ok()
}

/// `N` or `N%`, as `resize-pane -x` and `-y` take it.
fn parse_extent(text: &str) -> Option<Extent> {
    match text.strip_suffix('%') {
        Some(percent) => (number(percent).and_then(|n| u8::try_from(n).ok()))
            .filter(|n| (1..=100).contains(n))
            .map(Extent::Percent),
        None => (number(text).and_then(|n| u16::try_from(n).ok()))
            .filter(|n| *n > 0)
            .map(Extent::Cells),
    }
}

/// `A-B`, rows or columns from 0, as `capture-pane --rows` and `--cols`
/// take them; `A` alone is one.
fn parse_span(text: &str) -> Option<(u16, u16)> {
    let cell = |n: &str| number(n).and_then(|n| u16::try_from(n).ok());
    let (start, end) = text.split_once('-').unwrap_or((text, text));
    let (start, end) = (cell(start)?, cell(end)?);
    (start <= end).then_some((start, end))
}

/// Milliseconds, as `--idle` and `--wait-idle` take them.
fn parse_millis(text: &str) -> Option<Duration> {
    number(text).map(|n| Duration::from_millis(u64::from(n)))
}

/// Seconds, whole or not, as `--timeout` takes them: at most a day.
fn parse_seconds(text: &str) -> Option<Duration> {
    let seconds: f64 = text.parse().ok()?;
    (seconds > 0.0 && seconds <= 86_400.0).then(|| Duration::from_secs_f64(seconds))
}

/// A command, described once: how `fux help` shows it, and how its words
/// become a [`Command`].
pub struct Spec {
    /// Its name (`ls|list`: and another), the words after it, and after two
    /// spaces what it does. Every flag here is one the command takes, and
    /// it takes no other; one followed by a placeholder (`-t %N`, unlike
    /// `-h [-t %N]` or `[--json]`) takes a value. With `pane|tab|workspace`
    /// in it, the last positional names a kind.
    pub usage: &'static str,
    read: Read,
}

/// How a command's words become a [`Command`]. With `Flags` and `Screen`,
/// flags come anywhere, as the usage names them, among positionals and
/// before `--`; what the reader leaves is refused.
enum Read {
    Flags(fn(&mut Args<'_>) -> Result<Command, Usage>),
    /// An action on the screen of the client `-c` names, if it names one,
    /// else of the client whose key, menu or prompt ran it.
    Screen(fn(&mut Args<'_>) -> Result<ClientAction, Usage>),
    /// The words as they are: keys that may look like flags, or a config
    /// line, which `Config::apply` reads.
    Line(fn(&[String]) -> Result<Command, Usage>),
}

const fn spec(usage: &'static str, read: Read) -> Spec {
    Spec { usage, read }
}

use Read::{Flags, Line, Screen};

/// Every command, under the command-column group its bindings are listed
/// in, in the column's order: `Other`, which it lists last, holds the rest.
pub const COMMANDS: &[(&str, &[Spec])] = &[
    ("Panes", PANES),
    ("Focus", FOCUS),
    ("Tabs", TABS),
    ("Workspaces", WORKSPACES),
    ("Session", SESSION),
    ("Other", OTHER),
];

#[rustfmt::skip]
const PANES: &[Spec] = &[
    spec("split -h|-v [-t %N] [--json] [-- CMD...]  -h side by side, -v stacked", Flags(split)),
    spec("run [-t WS] [-n NAME] [--size COLSxROWS] [--cwd DIR] [--env K=V] [--json] -- PROGRAM...  \
          a program in a tab of its own, in workspace agent; it stays once the program exits", Flags(run)),
    spec("kill-pane [-t %N]  close, without asking", Flags(|a| {
        Ok(Command::KillPane { target: a.pane()? })
    })),
    spec("zoom [-c CLIENT]", Screen(|_| Ok(ClientAction::Zoom))),
    spec("resize-pane [-t %N] (-L|-R|-U|-D [CELLS] | -x COLS[%] | -y ROWS[%])  \
          move a border (one cell), or to a size", Flags(resize)),
    spec("swap-pane [-t %N] (%M | -L|-R|-U|-D)", Flags(swap)),
    spec("move-pane [-t %N] (--to @N|+N|new-tab|new-workspace | -L|-R|-U|-D)", Flags(move_pane)),
    spec("copy-mode [-c CLIENT]", Screen(|_| Ok(ClientAction::CopyMode))),
    spec("paste-buffer [-b N] [-t %N]", Flags(|a| {
        Ok(Command::PasteBuffer { index: a.buffer()?, target: a.pane()? })
    })),
    spec("menu pane|tab|workspace [-c CLIENT] [-t TARGET]", Screen(|a| {
        let needs = Usage::Needs("usage: menu pane|tab|workspace [-t TARGET]");
        a.subject()?.map(ClientAction::Menu).ok_or(needs)
    })),
    spec("rename-prompt [pane|tab|workspace] [-c CLIENT] [-t TARGET]", Screen(|a| {
        Ok(ClientAction::RenamePrompt(a.subject()?.unwrap_or(Subject::Pane(None))))
    })),
    spec("confirm-close [pane|tab|workspace] [-c CLIENT] [-t TARGET]", Screen(|a| {
        Ok(ClientAction::ConfirmClose(a.subject()?.unwrap_or(Subject::Pane(None))))
    })),
    spec("terminate [-t %N]  SIGTERM to the pane's foreground job", Flags(|a| {
        Ok(Command::Terminate { target: a.pane()? })
    })),
    spec("choose-pane [-c CLIENT] [-t %N]  a pane to swap with", Screen(|a| {
        Ok(ClientAction::ChoosePane { target: a.pane()? })
    })),
    spec("send-keys [-t %N] [-l] [-N COUNT] KEYS... [--wait-idle MS [--print]]  \
          keys, words that are not key names as text, and --text TEXT or --paste TEXT", Line(send_keys)),
    spec("send-mouse [-t %N] click|right|middle|press|release|wheel-up|wheel-down|move ROW COL  \
          a mouse event at a cell, from 0", Flags(send_mouse)),
    spec("wait [-t %N] (--text TEXT | --gone TEXT | --idle MS | --change | --exit) [--timeout SECS] [--print]  \
          until the pane shows or stops showing TEXT, goes quiet, changes or exits", Flags(wait)),
    spec("send-prefix [-t %N]  the prefix key, to the pane", Flags(|a| {
        Ok(Command::SendPrefix { target: a.pane()? })
    })),
    spec("reorder pane|tab|workspace [-t TARGET] --next|--previous", Flags(reorder)),
];

#[rustfmt::skip]
const FOCUS: &[Spec] = &[spec(
    "select-pane [-c CLIENT] -t %N|--next|--previous|--last|-L|-R|-U|-D",
    Screen(select_pane),
)];

#[rustfmt::skip]
const TABS: &[Spec] = &[
    spec("new-tab [-t WS] [-n NAME] [--size COLSxROWS] [--json] [-- CMD...]", Flags(|a| {
        let (target, size) = (a.target(parse_workspace)?, a.size()?);
        Ok(Command::NewTab { target, name: a.name(), cmd: a.rest(), size, json: a.has("--json") })
    })),
    spec("select-tab [-c CLIENT] -t @N|--next|--previous", Screen(|a| {
        let needs = "select-tab needs one of -t @N, --next, --previous";
        a.pick(parse_tab, needs).map(ClientAction::SelectTab)
    })),
    spec("choose-tab [-c CLIENT] [-t %N] [--move]  where to go, or move the pane", Screen(|a| {
        Ok(ClientAction::ChooseTab { moving: a.moving()? })
    })),
    spec("kill-tab [-t @N]", Flags(|a| Ok(Command::KillTab { target: a.target(parse_tab)? }))),
    spec("resize (-t @N|+N | -c CLIENT) COLSxROWS|auto  \
          what a tab nobody views is laid out on, or a headless client's size", Flags(resize_to)),
];

#[rustfmt::skip]
const WORKSPACES: &[Spec] = &[
    spec("new-workspace [-n NAME] [--size COLSxROWS] [--json] [-- CMD...]", Flags(|a| {
        let size = a.size()?;
        Ok(Command::NewWorkspace { name: a.name(), cmd: a.rest(), size, json: a.has("--json") })
    })),
    spec("select-workspace [-c CLIENT] -t WS|--next|--previous", Screen(|a| {
        let needs = "select-workspace needs one of -t +N, --next, --previous";
        a.pick(parse_workspace, needs).map(ClientAction::SelectWorkspace)
    })),
    spec("choose-workspace [-c CLIENT] [-t %N] [--move]", Screen(|a| {
        Ok(ClientAction::ChooseWorkspace { moving: a.moving()? })
    })),
    spec("kill-workspace [-t WS]", Flags(|a| {
        Ok(Command::KillWorkspace { target: a.target(parse_workspace)? })
    })),
];

#[rustfmt::skip]
const SESSION: &[Spec] = &[
    spec("detach [-c CLIENT]", Screen(|_| Ok(ClientAction::Detach))),
    spec("command-prompt [-c CLIENT]", Screen(|_| Ok(ClientAction::CommandPrompt))),
    spec("command-column [-c CLIENT]", Screen(|_| Ok(ClientAction::CommandColumn))),
    spec("reload  run the config file again", Flags(|_| Ok(Command::Reload))),
    spec("kill-server", Flags(|_| Ok(Command::KillServer))),
];

#[rustfmt::skip]
const OTHER: &[Spec] = &[
    spec("ls|list [--json]  workspaces, tabs, panes and clients", Flags(|a| {
        Ok(Command::Ls { json: a.has("--json") })
    })),
    spec("rename -t TARGET [--] NAME  TARGET: %N, @N, +N or a name", Flags(rename)),
    spec("capture-pane [-t %N] [-S LINES] [--full] [-J] [--rows A-B] [--cols A-B] [--styles] [--json]  \
          the screen, trimmed; -J joins wrapped lines", Flags(capture)),
    spec("capture-client [-c CLIENT] [--json]", Screen(|a| {
        Ok(ClientAction::Capture { json: a.has("--json") })
    })),
    spec("set OPTION VALUE", Line(configure)),
    spec("bind [-n] [-g GROUP] [-r] KEY... COMMAND...  V is Shift-v", Line(configure)),
    spec("unbind [-n] KEY...", Line(configure)),
    spec("unbind-all", Flags(|_| Ok(Command::Configure { argv: vec!["unbind-all".to_owned()] }))),
    spec("list-buffers", Flags(|_| Ok(Command::ListBuffers))),
    spec("show-buffer [-b N]", Flags(|a| Ok(Command::ShowBuffer { index: a.buffer()? }))),
    spec("list-keys  key names and bindings", Flags(|_| Ok(Command::ListKeys))),
];

/// The command named `name`: the name as the table has it, its group, and
/// how it reads.
fn find(name: &str) -> Option<(&'static str, &'static str, &'static Spec)> {
    COMMANDS.iter().find_map(|(group, specs)| {
        specs
            .iter()
            .find_map(|spec| spec.names().find(|n| *n == name).map(|n| (n, *group, spec)))
    })
}

/// The command-column group of a command line that parsed as `command`:
/// its subject's, for one that acts on any kind, else its command's.
pub fn group(argv: &[String], command: &Command) -> &'static str {
    match command.subject().map(Subject::kind) {
        Some(Kind::Tab) => "Tabs",
        Some(Kind::Workspace) => "Workspaces",
        Some(Kind::Pane) | None => argv
            .first()
            .and_then(|name| find(name))
            .map_or("Other", |(_, group, _)| group),
    }
}

/// The usage of the command named `name`, without what it does.
pub fn usage_of(name: &str) -> Option<&'static str> {
    find(name).map(|(_, _, spec)| spec.grammar())
}

/// The commands, as `fux help` lists them, under their groups.
pub fn help() -> String {
    let mut out = String::new();
    for (group, specs) in COMMANDS {
        out.push_str(&format!("{group}:\n"));
        for spec in specs.iter() {
            let line = match spec.usage.split_once("  ") {
                Some((usage, about)) if usage.len() < 36 => format!("  {usage:<37}{about}"),
                Some((usage, about)) => format!("  {usage}\n{:39}{about}", ""),
                None => format!("  {}", spec.usage),
            };
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

impl Spec {
    /// Its name, and another it answers to, if any.
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        self.usage.split(' ').next().unwrap_or_default().split('|')
    }
    /// The usage before what the command does.
    fn grammar(&self) -> &'static str {
        self.usage.split("  ").next().unwrap_or_default()
    }
    /// Whether the usage names `flag`, and if so, whether a value follows it.
    fn flag(&self, flag: &str) -> Option<bool> {
        let mut words = self.grammar().split(' ').skip(1).peekable();
        while let Some(word) = words.next() {
            let bare = word.trim_matches(['[', ']', '(', ')']);
            if bare.split('|').any(|f| f == flag) {
                let value = bare.rsplit('|').next() == Some(flag)
                    && !word.ends_with([']', ')'])
                    && words
                        .peek()
                        .is_some_and(|w| !w.starts_with(['[', '(', '-', '|']));
                return Some(value);
            }
        }
        None
    }
}

/// A command line's words, read as its usage says: each flag with its value
/// if it takes one, in order; positionals; and the words after `--`.
struct Args<'a> {
    name: &'static str,
    flags: Vec<(&'a str, Option<&'a str>)>,
    positional: Vec<&'a str>,
    /// The last positional, for a command whose usage names a kind.
    kind: Option<&'a str>,
    rest: Vec<String>,
}

impl<'a> Args<'a> {
    /// Every flag is checked, in order, before any value is read.
    fn read(name: &'static str, spec: &Spec, words: &'a [String]) -> Result<Self, Usage> {
        let mut args = Args {
            name,
            flags: Vec::new(),
            positional: Vec::new(),
            kind: None,
            rest: Vec::new(),
        };
        let mut words = words.iter();
        while let Some(word) = words.next() {
            if word == "--" {
                args.rest = words.cloned().collect();
                break;
            }
            if !(word.starts_with('-') && word.len() > 1) {
                args.positional.push(word);
                continue;
            }
            let value = match spec.flag(word) {
                None => {
                    return Err(Usage::UnknownFlag {
                        command: name,
                        flag: word.clone(),
                    });
                }
                Some(false) => None,
                Some(true) => Some(words.next().ok_or_else(|| Usage::NeedsValue {
                    command: name,
                    flag: word.clone(),
                })?),
            };
            args.flags.push((word, value.map(String::as_str)));
        }
        if spec.grammar().contains("pane|tab|workspace") {
            args.kind = args.positional.pop();
        }
        Ok(args)
    }
    /// The kind the usage's last positional names, if given.
    fn kind(&self) -> Result<Option<Kind>, Usage> {
        let Some(word) = self.kind else {
            return Ok(None);
        };
        let kind = [Kind::Pane, Kind::Tab, Kind::Workspace]
            .into_iter()
            .find(|k| k.name() == word);
        kind.map(Some).ok_or_else(|| Usage::NotKind {
            command: self.name,
            word: word.to_owned(),
        })
    }
    fn no_positional(&self) -> Result<(), Usage> {
        match self.positional.first() {
            Some(word) => Err(Usage::Unexpected {
                command: self.name,
                word: (*word).to_owned(),
            }),
            None => Ok(()),
        }
    }
    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|(f, _)| *f == flag)
    }
    /// The last given of `flags`: a flag given twice counts the second time.
    fn last(&self, of: &[&str]) -> Option<&'a str> {
        self.flags
            .iter()
            .rev()
            .map(|(f, _)| *f)
            .find(|f| of.contains(f))
    }
    fn value(&self, flag: &str) -> Option<&'a str> {
        self.flags
            .iter()
            .rev()
            .find(|(f, _)| *f == flag)
            .and_then(|(_, v)| *v)
    }
    fn step(&self) -> Option<Sibling> {
        match self.last(&["--next", "--previous"])? {
            "--next" => Some(Sibling::Next),
            _ => Some(Sibling::Previous),
        }
    }
    fn direction(&self) -> Option<Direction> {
        self.flags.iter().rev().find_map(|(f, _)| direction_flag(f))
    }
    /// `flag`'s value as `parse` reads it: each one given is read, in
    /// order, and the last counts.
    fn parsed<T>(
        &self,
        flag: &str,
        parse: impl Fn(&'a str) -> Result<T, Usage>,
    ) -> Result<Option<T>, Usage> {
        let mut values = self.flags.iter().filter(|(f, _)| *f == flag);
        values.try_fold(None, |_, (_, v)| v.map(&parse).transpose())
    }
    fn target<T>(&self, parse: fn(&str) -> Result<T, Usage>) -> Result<Option<T>, Usage> {
        self.parsed("-t", parse)
    }
    fn pane(&self) -> Result<Option<PaneId>, Usage> {
        self.target(parse_pane)
    }
    fn name(&self) -> Option<String> {
        self.value("-n").map(str::to_owned)
    }
    fn buffer(&self) -> Result<usize, Usage> {
        let buffer = self.parsed("-b", |v| {
            number(v).ok_or_else(|| Usage::NotBuffer {
                command: self.name,
                value: v.to_owned(),
            })
        })?;
        Ok(buffer.map_or(0, |n| n as usize))
    }
    fn lines(&self) -> Result<Option<usize>, Usage> {
        self.parsed("-S", |v| {
            let lines = v.strip_prefix('-').unwrap_or(v).parse();
            lines.map_err(|_| Usage::NotLines(v.to_owned()))
        })
    }
    fn rest(&mut self) -> Vec<String> {
        std::mem::take(&mut self.rest)
    }
    /// Every value `flag` was given, in order.
    fn values(&self, flag: &str) -> impl Iterator<Item = &'a str> {
        let flag = flag.to_owned();
        (self.flags.clone().into_iter()).filter_map(move |(f, v)| v.filter(|_| f == flag))
    }
    /// `flag`'s value as `parse` reads it, refused as not `expected`.
    fn read_as<T>(
        &self,
        flag: &'static str,
        parse: fn(&str) -> Option<T>,
        expected: &'static str,
    ) -> Result<Option<T>, Usage> {
        self.parsed(flag, |v| {
            parse(v).ok_or_else(|| Usage::Bad {
                command: self.name,
                flag,
                value: v.to_owned(),
                expected,
            })
        })
    }
    fn size(&self) -> Result<Size, Usage> {
        self.read_as("--size", parse_size, "COLSxROWS, as 120x40")
    }
    /// The pane, tab or workspace a positional kind and `-t` name, if
    /// either does: of another kind than the kind given, `-t` is refused in
    /// the words a target of the wrong kind gets, rather than one of the
    /// two silently winning.
    fn subject(&self) -> Result<Option<Subject>, Usage> {
        let (target, kind) = (self.target(parse_any)?, self.kind()?);
        let (Some(text), Some(target)) = (self.value("-t"), target) else {
            return Ok(kind.map(Subject::own));
        };
        match kind {
            Some(kind) if kind != target.kind() => Err(Usage::Not(kind, text.to_owned())),
            _ => Ok(Some(target.into())),
        }
    }
    /// `-t`, `--next` or `--previous`: one of them.
    fn pick<T>(
        &self,
        parse: fn(&str) -> Result<T, Usage>,
        needs: &'static str,
    ) -> Result<Pick<T>, Usage> {
        match (self.step(), self.value("-t")) {
            (Some(step), None) => Ok(Pick::Step(step)),
            (None, Some(t)) => parse(t).map(Pick::Id),
            _ => Err(Usage::Needs(needs)),
        }
    }
    /// What a chooser moves: `-t`'s pane, or with `--move` the focused one.
    fn moving(&self) -> Result<Option<Option<PaneId>>, Usage> {
        Ok(match self.pane()? {
            Some(pane) => Some(Some(pane)),
            None => self.has("--move").then_some(None),
        })
    }
}

fn configure(argv: &[String]) -> Result<Command, Usage> {
    Ok(Command::Configure {
        argv: argv.to_vec(),
    })
}

fn run(a: &mut Args<'_>) -> Result<Command, Usage> {
    let workspace = a.target(parse_workspace)?;
    let env = a
        .values("--env")
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
            _ => Err(Usage::Bad {
                command: "run",
                flag: "--env",
                value: pair.to_owned(),
                expected: "KEY=VALUE",
            }),
        })
        .collect::<Result<_, _>>()?;
    let argv = a.rest();
    if argv.is_empty() {
        return Err(Usage::Needs("usage: run [flags] -- PROGRAM [ARGS...]"));
    }
    Ok(Command::Run {
        workspace,
        name: a.name(),
        size: a.size()?,
        cwd: a.value("--cwd").map(str::to_owned),
        env,
        argv,
        json: a.has("--json"),
    })
}

fn resize_to(a: &mut Args<'_>) -> Result<Command, Usage> {
    let needs = "usage: resize (-t @N|+N | -c CLIENT) COLSxROWS|auto";
    let what = match (a.target(parse_any)?, a.parsed("-c", parse_client)?) {
        (Some(AnyRef::Pane(_)), None) => {
            return Err(Usage::Needs(
                "resize sizes a tab or a workspace; resize-pane sizes a pane",
            ));
        }
        (Some(target), None) => Resized::Tabs(target),
        (None, Some(client)) => Resized::Client(client),
        (Some(_), Some(_)) | (None, None) => return Err(Usage::Needs(needs)),
    };
    let size = match a.positional.pop() {
        Some("auto") => None,
        Some(text) => Some(parse_size(text).ok_or_else(|| Usage::Bad {
            command: "resize",
            flag: "SIZE",
            value: text.to_owned(),
            expected: "COLSxROWS or auto",
        })?),
        None => return Err(Usage::Needs(needs)),
    };
    if matches!(what, Resized::Client(_)) && size.is_none() {
        return Err(Usage::Needs("a client's size is COLSxROWS"));
    }
    Ok(Command::Resize { what, size })
}

fn capture(a: &mut Args<'_>) -> Result<Command, Usage> {
    let span = "A-B, rows or columns from 0";
    let view = CaptureView {
        full: a.has("--full"),
        join: a.has("-J"),
        rows: a.read_as("--rows", parse_span, span)?,
        cols: a.read_as("--cols", parse_span, span)?,
        styles: a.has("--styles"),
    };
    Ok(Command::CapturePane {
        target: a.pane()?,
        history: a.lines()?,
        view,
        json: a.has("--json"),
    })
}

fn wait(a: &mut Args<'_>) -> Result<Command, Usage> {
    let texts = |flag| a.values(flag).map(str::to_owned).collect::<Vec<_>>();
    let (text, gone) = (texts("--text"), texts("--gone"));
    let idle = a.read_as("--idle", parse_millis, "milliseconds")?;
    let mut until = [
        (!text.is_empty()).then_some(Until::Text(text)),
        (!gone.is_empty()).then_some(Until::Gone(gone)),
        idle.map(Until::Idle),
        a.has("--change").then_some(Until::Change),
        a.has("--exit").then_some(Until::Exit),
    ]
    .into_iter()
    .flatten();
    let (Some(first), None) = (until.next(), until.next()) else {
        return Err(Usage::Needs(
            "wait needs one of --text, --gone, --idle, --change, --exit",
        ));
    };
    let timeout = a.read_as("--timeout", parse_seconds, "seconds, at most a day")?;
    Ok(Command::Wait {
        target: a.pane()?,
        wait: Wait {
            until: first,
            timeout: timeout.unwrap_or(WAIT_TIMEOUT),
            print: a.has("--print"),
        },
    })
}

fn send_mouse(a: &mut Args<'_>) -> Result<Command, Usage> {
    let usage = "usage: send-mouse [-t %N] click|right|middle|press|release|wheel-up|wheel-down|move ROW COL";
    let (action, row, col) = match a.positional.as_slice() {
        [action, row, col] => (*action, *row, *col),
        _ => return Err(Usage::Needs(usage)),
    };
    a.positional.clear();
    let mouse = match action {
        "click" => Mouse::Click(MouseButton::Left),
        "right" => Mouse::Click(MouseButton::Right),
        "middle" => Mouse::Click(MouseButton::Middle),
        "press" => Mouse::Press(MouseButton::Left),
        "release" => Mouse::Release(MouseButton::Left),
        "wheel-up" => Mouse::Wheel(MouseButton::WheelUp),
        "wheel-down" => Mouse::Wheel(MouseButton::WheelDown),
        "move" => Mouse::Move,
        _ => return Err(Usage::Needs(usage)),
    };
    let cell = |flag, text: &str| {
        number(text)
            .and_then(|n| u16::try_from(n).ok())
            .ok_or_else(|| Usage::Bad {
                command: "send-mouse",
                flag,
                value: text.to_owned(),
                expected: "a cell, from 0",
            })
    };
    Ok(Command::SendMouse {
        target: a.pane()?,
        mouse,
        row: cell("ROW", row)?,
        col: cell("COL", col)?,
    })
}

fn split(a: &mut Args<'_>) -> Result<Command, Usage> {
    let target = a.pane()?;
    let axis = match a.last(&["-h", "-v"]) {
        Some("-h") => Axis::Horizontal,
        Some(_) => Axis::Vertical,
        None => {
            return Err(Usage::Needs(
                "split needs -h (side by side) or -v (stacked)",
            ));
        }
    };
    Ok(Command::Split {
        axis,
        target,
        cmd: a.rest(),
        json: a.has("--json"),
    })
}

fn rename(a: &mut Args<'_>) -> Result<Command, Usage> {
    let target = a.target(parse_any)?.ok_or(Usage::Needs(
        "rename needs -t TARGET (%N, @N, +N or a workspace name)",
    ))?;
    let name = match (a.positional.as_slice(), a.rest.as_slice()) {
        ([name], []) => (*name).to_owned(),
        ([], [name]) => name.clone(),
        _ => {
            return Err(Usage::Needs(
                "usage: rename -t TARGET NAME (quote a name with spaces)",
            ));
        }
    };
    a.positional.clear();
    a.rest.clear();
    Ok(Command::Rename { target, name })
}

fn move_pane(a: &mut Args<'_>) -> Result<Command, Usage> {
    let target = a.pane()?;
    // The last of `--to` and the directions, each `--to` read.
    let to = a
        .flags
        .iter()
        .try_fold(None, |to, &(flag, value)| match value {
            Some(value) if flag == "--to" => parse_move_to(value).map(Some),
            _ => Ok(direction_flag(flag).map(MoveTo::Beside).or(to)),
        })?;
    let to = to.ok_or(Usage::Needs(
        "move-pane needs --to @N|+N|new-tab|new-workspace or -L/-R/-U/-D",
    ))?;
    Ok(Command::MovePane { target, to })
}

fn swap(a: &mut Args<'_>) -> Result<Command, Usage> {
    let target = a.pane()?;
    // Another pane or a direction: one, not both.
    let with = match (a.positional.pop(), a.direction()) {
        (Some(other), None) => SwapWith::Pane(parse_pane(other)?),
        (None, Some(direction)) => SwapWith::Toward(direction),
        (Some(_), Some(_)) | (None, None) => {
            return Err(Usage::Needs(
                "swap-pane needs another pane (%N) or -L/-R/-U/-D",
            ));
        }
    };
    Ok(Command::SwapPane { target, with })
}

fn resize(a: &mut Args<'_>) -> Result<Command, Usage> {
    let target = a.pane()?;
    let size = "a number of cells, or a percentage as 95%";
    let cols = a.read_as("-x", parse_extent, size)?;
    let rows = a.read_as("-y", parse_extent, size)?;
    let by = match (a.direction(), cols.is_some() || rows.is_some()) {
        (Some(direction), false) => {
            let amount = match a.positional.pop() {
                Some(n) => match number(n).and_then(|n| u16::try_from(n).ok()) {
                    Some(n) if n > 0 => n,
                    _ => return Err(Usage::NotCells(n.to_owned())),
                },
                None => 1,
            };
            PaneResize::Border(direction, amount)
        }
        (None, true) => PaneResize::To { cols, rows },
        (Some(_), true) | (None, false) => {
            return Err(Usage::Needs(
                "resize-pane needs one of -L, -R, -U, -D, or -x and -y",
            ));
        }
    };
    Ok(Command::ResizePane { target, by })
}

fn reorder(a: &mut Args<'_>) -> Result<Command, Usage> {
    let subject = a.subject()?.ok_or(Usage::Needs(
        "usage: reorder pane|tab|workspace [-t TARGET] --next|--previous",
    ))?;
    let toward = a
        .step()
        .ok_or(Usage::Needs("reorder needs --next or --previous"))?;
    Ok(Command::Reorder { subject, toward })
}

fn select_pane(a: &mut Args<'_>) -> Result<ClientAction, Usage> {
    let step = a.last(&["--next", "--previous", "--last"]);
    let pick = match (step, a.direction(), a.value("-t")) {
        (Some("--next"), None, None) => PanePick::Step(Sibling::Next),
        (Some("--previous"), None, None) => PanePick::Step(Sibling::Previous),
        (Some(_), None, None) => PanePick::Last,
        (None, Some(d), None) => PanePick::Toward(d),
        (None, None, Some(t)) => PanePick::Id(parse_pane(t)?),
        _ => {
            return Err(Usage::Needs(
                "select-pane needs one of -t %N, --next, --previous, --last, -L/-R/-U/-D",
            ));
        }
    };
    Ok(ClientAction::SelectPane(pick))
}

/// Keys may look like flags (`-`), so only leading short flags count; the
/// long ones, which no key is named, count anywhere.
fn send_keys(argv: &[String]) -> Result<Command, Usage> {
    fn value<'a>(
        flag: &str,
        words: &mut impl Iterator<Item = &'a String>,
    ) -> Result<&'a str, Usage> {
        words
            .next()
            .map(String::as_str)
            .ok_or_else(|| Usage::NeedsValue {
                command: "send-keys",
                flag: flag.to_owned(),
            })
    }
    let bad = |flag, value: &str, expected| Usage::Bad {
        command: "send-keys",
        flag,
        value: value.to_owned(),
        expected,
    };
    let (mut target, mut literal, mut repeat, mut idle, mut print) = (None, false, 1, None, false);
    let mut words = argv.get(1..).unwrap_or_default().iter().peekable();
    while let Some(word) = words.next_if(|w| ["-t", "-l", "-N", "--"].contains(&w.as_str())) {
        match word.as_str() {
            "-t" => target = Some(parse_pane(value(word, &mut words)?)?),
            "-l" => literal = true,
            "-N" => {
                let text = value(word, &mut words)?;
                let count = number(text).and_then(|n| u16::try_from(n).ok());
                repeat = count
                    .filter(|n| *n > 0)
                    .ok_or_else(|| bad("-N", text, "a count from 1"))?;
            }
            _ => break,
        }
    }
    let mut inputs = Vec::new();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--text" => inputs.push(Input::Text(value(word, &mut words)?.to_owned())),
            "--paste" => inputs.push(Input::Paste(value(word, &mut words)?.to_owned())),
            "--wait-idle" => {
                let text = value(word, &mut words)?;
                idle = Some(
                    parse_millis(text).ok_or_else(|| bad("--wait-idle", text, "milliseconds"))?,
                );
            }
            "--print" => print = true,
            text => inputs.push(match text.parse::<KeyPress>() {
                Ok(key) if !literal => Input::Key(key),
                _ => Input::Text(text.to_owned()),
            }),
        }
    }
    if inputs.is_empty() {
        return Err(Usage::Needs("send-keys needs keys to send"));
    }
    if print && idle.is_none() {
        return Err(Usage::Needs("send-keys --print goes with --wait-idle MS"));
    }
    let then = idle.map(|idle| Wait {
        until: Until::Idle(idle),
        timeout: WAIT_TIMEOUT,
        print,
    });
    Ok(Command::SendKeys {
        target,
        inputs,
        repeat,
        then,
    })
}

/// Parses one command line.
pub fn parse(argv: &[String]) -> Result<Command, Usage> {
    let (name, words) = argv.split_first().ok_or(Usage::NoCommand)?;
    let (name, _, spec) = find(name).ok_or_else(|| Usage::UnknownCommand(name.clone()))?;
    let (command, a) = match spec.read {
        Read::Line(read) => return read(argv),
        Read::Flags(read) => {
            let mut a = Args::read(name, spec, words)?;
            (read(&mut a)?, a)
        }
        // `-c` is read first, then the kind, and then no other word may
        // stand.
        Read::Screen(read) => {
            let mut a = Args::read(name, spec, words)?;
            let client = a.parsed("-c", parse_client)?;
            a.kind()?;
            a.no_positional()?;
            let action = read(&mut a)?;
            (Command::Client { client, action }, a)
        }
    };
    a.no_positional()?;
    if !a.rest.is_empty() {
        return Err(Usage::NoCommandAfter { command: name });
    }
    Ok(command)
}

/// A short description of a command line, for the command column and menus.
pub fn label(argv: &[String]) -> String {
    let joined: Vec<&str> = argv.iter().map(String::as_str).collect();
    let text = match joined.as_slice() {
        ["split", "-h"] => "split side by side",
        ["split", "-v"] => "split stacked",
        ["confirm-close"] | ["confirm-close", "pane"] => "close pane",
        ["confirm-close", "tab"] => "close tab",
        ["confirm-close", "workspace"] => "close workspace",
        ["kill-pane"] => "close pane now",
        ["zoom"] => "zoom or restore",
        ["rename-prompt"] | ["rename-prompt", "pane"] => "rename pane",
        ["rename-prompt", "tab"] => "rename tab",
        ["rename-prompt", "workspace"] => "rename workspace",
        ["menu", "pane"] => "pane actions",
        ["menu", "tab"] => "tab actions",
        ["menu", "workspace"] => "workspace actions",
        ["copy-mode"] => "copy and select",
        ["paste-buffer"] => "paste the newest copy",
        ["resize-pane", "-L"] => "shrink or grow left",
        ["resize-pane", "-R"] => "shrink or grow right",
        ["resize-pane", "-U"] => "shrink or grow up",
        ["resize-pane", "-D"] => "shrink or grow down",
        ["move-pane", "-L"] => "move left",
        ["move-pane", "-R"] => "move right",
        ["move-pane", "-U"] => "move up",
        ["move-pane", "-D"] => "move down",
        ["select-pane", "--next"] => "next pane",
        ["select-pane", "--previous"] => "previous pane",
        ["select-pane", "--last"] => "last pane",
        ["select-pane", "-L"] => "focus left",
        ["select-pane", "-R"] => "focus right",
        ["select-pane", "-U"] => "focus up",
        ["select-pane", "-D"] => "focus down",
        ["new-tab"] => "new tab",
        ["select-tab", "--next"] => "next tab",
        ["select-tab", "--previous"] => "previous tab",
        ["choose-tab"] => "choose tab",
        ["new-workspace"] => "new workspace",
        ["select-workspace", "--next"] => "next workspace",
        ["select-workspace", "--previous"] => "previous workspace",
        ["choose-workspace"] => "choose workspace",
        ["reorder", "tab", "--previous"] => "move tab left",
        ["reorder", "tab", "--next"] => "move tab right",
        ["reorder", "workspace", "--previous"] => "move workspace earlier",
        ["reorder", "workspace", "--next"] => "move workspace later",
        ["command-prompt"] => "command prompt",
        ["command-column"] => "command column",
        ["detach"] => "detach this client",
        ["reload"] => "reload the config",
        ["kill-server"] => "stop the server",
        ["send-prefix"] => "send the prefix to the pane",
        _ => return crate::words::join(argv),
    };
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(line: &str) -> Result<Command, Usage> {
        parse(&crate::words::split(line).unwrap_or_default())
    }

    #[test]
    fn the_cli_table_parses() -> Result<(), Usage> {
        assert_eq!(cmd("ls --json"), Ok(Command::Ls { json: true }));
        assert_eq!(
            cmd("split -h -t %3 -- htop -d 5"),
            Ok(Command::Split {
                axis: Axis::Horizontal,
                target: Some(PaneId::of(3)),
                cmd: vec!["htop".into(), "-d".into(), "5".into()],
                json: false,
            })
        );
        assert_eq!(
            cmd("new-tab -t work -n logs"),
            Ok(Command::NewTab {
                target: Some(WsRef::Name("work".into())),
                name: Some("logs".into()),
                cmd: vec![],
                size: None,
                json: false,
            })
        );
        assert_eq!(
            cmd("move-pane -t %1 --to +2"),
            Ok(Command::MovePane {
                target: Some(PaneId::of(1)),
                to: MoveTo::Workspace(WsRef::Id("+2".parse()?))
            })
        );
        assert_eq!(
            cmd("swap-pane -t %1 %2"),
            Ok(Command::SwapPane {
                target: Some(PaneId::of(1)),
                with: SwapWith::Pane(PaneId::of(2))
            })
        );
        assert_eq!(
            cmd("resize-pane -t %1 -L 5"),
            Ok(Command::ResizePane {
                target: Some(PaneId::of(1)),
                by: PaneResize::Border(Direction::Left, 5),
            })
        );
        assert_eq!(
            cmd("resize-pane -x 95% -y 10"),
            Ok(Command::ResizePane {
                target: None,
                by: PaneResize::To {
                    cols: Some(Extent::Percent(95)),
                    rows: Some(Extent::Cells(10)),
                },
            })
        );
        assert_eq!(
            cmd("send-keys -t %1 -l -x"),
            Ok(Command::SendKeys {
                target: Some(PaneId::of(1)),
                inputs: vec![Input::Text("-x".into())],
                repeat: 1,
                then: None,
            })
        );
        // Words that are not key names are text, and the long flags count
        // anywhere.
        assert_eq!(
            cmd("send-keys -N 2 Escape 'two words' --paste p Enter --wait-idle 300 --print"),
            Ok(Command::SendKeys {
                target: None,
                inputs: vec![
                    Input::Key("Escape".parse().map_err(|_| Usage::NoCommand)?),
                    Input::Text("two words".into()),
                    Input::Paste("p".into()),
                    Input::Key("Enter".parse().map_err(|_| Usage::NoCommand)?),
                ],
                repeat: 2,
                then: Some(Wait {
                    until: Until::Idle(Duration::from_millis(300)),
                    timeout: WAIT_TIMEOUT,
                    print: true,
                }),
            })
        );
        assert_eq!(
            cmd("capture-pane -t %1 -S -100 -J --rows 2-5 --json"),
            Ok(Command::CapturePane {
                target: Some(PaneId::of(1)),
                history: Some(100),
                view: CaptureView {
                    join: true,
                    rows: Some((2, 5)),
                    ..CaptureView::default()
                },
                json: true
            })
        );
        assert_eq!(
            cmd("wait -t %2 --text a --text b --timeout 1.5 --print"),
            Ok(Command::Wait {
                target: Some(PaneId::of(2)),
                wait: Wait {
                    until: Until::Text(vec!["a".into(), "b".into()]),
                    timeout: Duration::from_millis(1500),
                    print: true,
                },
            })
        );
        assert!(cmd("wait --text a --idle 5").is_err(), "one condition");
        assert_eq!(
            cmd("run -n tool --size 80x24 --env A=1 --json -- ck -v"),
            Ok(Command::Run {
                workspace: None,
                name: Some("tool".into()),
                size: fux_vt::Size::new(24, 80).ok(),
                cwd: None,
                env: vec![("A".into(), "1".into())],
                argv: vec!["ck".into(), "-v".into()],
                json: true,
            })
        );
        assert_eq!(
            cmd("resize -t @2 auto"),
            Ok(Command::Resize {
                what: Resized::Tabs(AnyRef::Tab("@2".parse()?)),
                size: None,
            })
        );
        assert_eq!(
            cmd("send-mouse -t %1 wheel-up 3 7"),
            Ok(Command::SendMouse {
                target: Some(PaneId::of(1)),
                mouse: Mouse::Wheel(MouseButton::WheelUp),
                row: 3,
                col: 7,
            })
        );
        assert_eq!(
            cmd("rename -t @2 'two words'"),
            Ok(Command::Rename {
                target: AnyRef::Tab("@2".parse()?),
                name: "two words".into()
            })
        );
        assert_eq!(
            cmd("select-pane -c c1 --last"),
            Ok(Command::Client {
                client: Some("c1".parse()?),
                action: ClientAction::SelectPane(PanePick::Last)
            })
        );
        assert_eq!(
            cmd("menu tab"),
            Ok(ClientAction::Menu(Subject::Tab(None)).here())
        );
        assert_eq!(
            cmd("confirm-close -t +1"),
            Ok(
                ClientAction::ConfirmClose(Subject::Workspace(Some(WsRef::Id("+1".parse()?))))
                    .here()
            )
        );
        assert_eq!(
            cmd("paste-buffer -b 2 -t %4"),
            Ok(Command::PasteBuffer {
                index: 2,
                target: Some(PaneId::of(4))
            })
        );
        Ok(())
    }

    #[test]
    fn bad_command_lines_are_usage_errors() {
        // Beside those `usage_errors_keep_their_words` words.
        for line in [
            "split -h -t 3",
            "rename %1 x",
            "resize-pane -t %1",
            "select-pane --next -L",
            // Commands that take nothing take nothing.
            "kill-server now",
            // A kind beside a target of another kind.
            "rename-prompt -c c1 tab -t %1",
            "menu -c c1 tab -t +1",
            "confirm-close -c c1 workspace -t @2",
            "reorder workspace -t %1 --next",
            // Another pane and a direction: one or the other.
            "swap-pane %2 -L",
            "unbind-all --nope",
            "reload -- x",
        ] {
            assert!(cmd(line).is_err(), "{line}");
        }
    }

    /// Each usage error says what it said as a string, word for word.
    #[test]
    fn usage_errors_keep_their_words() {
        for (line, message) in [
            ("", "no command given; `fux help` lists commands"),
            (
                "nope",
                r#"unknown command "nope"; `fux help` lists commands"#,
            ),
            ("kill-pane -t 3", r#""3" is not a pane; panes are %N"#),
            ("kill-tab -t %1", r#""%1" is not a tab; tabs are @N"#),
            (
                "new-tab -t %1",
                r#""%1" is not a workspace; workspaces are +N or a name"#,
            ),
            (
                "new-tab -t +x",
                r#""+x" is not a workspace; workspaces are +N or a name"#,
            ),
            (
                "detach -c zz",
                r#""zz" is not a client; `fux ls` lists clients as cN"#,
            ),
            ("split -t", "split -t needs a value"),
            ("ls --nope", "ls: unknown flag --nope"),
            ("ls extra", r#"ls: unexpected argument "extra""#),
            (
                "menu thing",
                r#"menu: "thing" is not pane, tab or workspace"#,
            ),
            (
                "reorder thing --next",
                r#"reorder: "thing" is not pane, tab or workspace"#,
            ),
            (
                "show-buffer -b x",
                r#"show-buffer -b: "x" is not a buffer number"#,
            ),
            (
                "paste-buffer -b -1",
                r#"paste-buffer -b: "-1" is not a buffer number"#,
            ),
            (
                "resize-pane -L zero",
                r#"resize-pane: "zero" is not a number of cells"#,
            ),
            ("capture-pane -S x", r#"capture-pane -S: "x" is not -N"#),
            ("capture-pane -t", "capture-pane -t needs a value"),
            ("capture-pane -S", "capture-pane -S needs a value"),
            ("capture-pane --nope", "capture-pane: unknown flag --nope"),
            // An argument is not a flag, unknown or not.
            (
                "capture-pane foo",
                r#"capture-pane: unexpected argument "foo""#,
            ),
            ("send-keys -t", "send-keys -t needs a value"),
            ("kill-pane -- x", "kill-pane takes no command after --"),
            ("split", "split needs -h (side by side) or -v (stacked)"),
            (
                "rename x",
                "rename needs -t TARGET (%N, @N, +N or a workspace name)",
            ),
            (
                "rename -t %1",
                "usage: rename -t TARGET NAME (quote a name with spaces)",
            ),
            (
                "move-pane",
                "move-pane needs --to @N|+N|new-tab|new-workspace or -L/-R/-U/-D",
            ),
            (
                "swap-pane",
                "swap-pane needs another pane (%N) or -L/-R/-U/-D",
            ),
            (
                "resize-pane",
                "resize-pane needs one of -L, -R, -U, -D, or -x and -y",
            ),
            ("send-keys -t %1", "send-keys needs keys to send"),
            (
                "reorder --next",
                "usage: reorder pane|tab|workspace [-t TARGET] --next|--previous",
            ),
            ("reorder tab", "reorder needs --next or --previous"),
            ("menu", "usage: menu pane|tab|workspace [-t TARGET]"),
            (
                "select-pane",
                "select-pane needs one of -t %N, --next, --previous, --last, -L/-R/-U/-D",
            ),
            (
                "select-tab",
                "select-tab needs one of -t @N, --next, --previous",
            ),
            (
                "select-workspace",
                "select-workspace needs one of -t +N, --next, --previous",
            ),
            (
                "confirm-close thing",
                r#"confirm-close: "thing" is not pane, tab or workspace"#,
            ),
            // A command takes only the flags its usage names.
            ("zoom -t %1", "zoom: unknown flag -t"),
            ("select-tab -L", "select-tab: unknown flag -L"),
            ("zoom pane", r#"zoom: unexpected argument "pane""#),
            // A target of the wrong form is refused as the kind it must be.
            ("select-tab -t %x", r#""%x" is not a tab; tabs are @N"#),
            (
                "select-workspace -t @x",
                r#""@x" is not a workspace; workspaces are +N or a name"#,
            ),
            ("choose-pane -t @x", r#""@x" is not a pane; panes are %N"#),
        ] {
            let usage = cmd(line).err().map(|u| u.to_string());
            assert_eq!(usage.as_deref(), Some(message), "{line:?}");
        }
    }

    /// A flag takes a value wherever a usage names it, or nowhere: a usage
    /// that lost a placeholder would read `-t %1` as a flag and a stray word.
    #[test]
    fn a_flag_takes_a_value_in_every_usage_or_in_none() {
        let specs = COMMANDS.iter().flat_map(|(_, specs)| specs.iter());
        for spec in specs.filter(|spec| matches!(spec.read, Read::Flags(_))) {
            for flag in spec.grammar().split([' ', '|', '[', ']', '(', ')']) {
                if flag.starts_with('-') && flag != "--" {
                    let value = [
                        "-t",
                        "-c",
                        "-n",
                        "-b",
                        "-S",
                        "--to",
                        "--size",
                        "--cwd",
                        "--env",
                        "-x",
                        "-y",
                        "--rows",
                        "--cols",
                        "--text",
                        "--gone",
                        "--idle",
                        "--timeout",
                    ]
                    .contains(&flag);
                    assert_eq!(spec.flag(flag), Some(value), "{}: {flag}", spec.usage);
                }
            }
        }
    }

    #[test]
    fn labels_describe_defaults_and_fall_back_to_the_command() {
        let words = |s: &str| crate::words::split(s).unwrap_or_default();
        assert_eq!(label(&words("split -h")), "split side by side");
        assert_eq!(label(&words("split -h -- htop")), "split -h -- htop");
        for (_, binding, _) in crate::config::Config::default().bindings.all() {
            assert!(parse(&binding.command).is_ok(), "{:?}", binding.command);
            assert_ne!(
                label(&binding.command),
                crate::words::join(&binding.command),
                "{:?}",
                binding.command
            );
        }
    }
}

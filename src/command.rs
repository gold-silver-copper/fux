//! The requests a client sends the server: typed actions, each built by
//! fuxth words in a client (`crate::script`) and carried whole over the
//! socket (`crate::wire`). The server never reads words.
use crate::id::{AnyId, ClientId, PaneId, TabId, WsId};
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
    pub fn own(kind: Kind) -> Subject {
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
    /// The main screen and its history, though a program holds the
    /// alternate one: what was printed before it took the screen.
    pub main: bool,
    /// The alternate screen as a full-screen program last drew it, though
    /// it has left it, or exited.
    pub alt: bool,
    /// The last shell command's output alone, between its OSC 133 marks.
    pub last_output: bool,
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
    /// The screen has shown no change for this long: the same screen
    /// drawn again is none.
    Idle(Duration),
    /// The screen changes: past the revision given (`--since N`), or the
    /// one it had as the wait began.
    Change(Option<u64>),
    /// The program exits.
    Exit,
    /// A shell command ends (shell integration's OSC 133 `D`).
    Done,
    /// The program rings the bell or sends a desktop notification.
    Bell,
}

/// A wait: for any of its conditions, in any of its panes, how long at
/// most, and how it is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    pub until: Vec<Until>,
    pub timeout: Duration,
    /// The screen as the wait ends, after what it says.
    pub print: bool,
    /// Only what comes after it began counts: text that appears anew, a
    /// screen that changed (send-keys' waits, armed before the keys).
    pub after: bool,
    /// The text send-keys types, run by run: a line holding a run that
    /// holds the awaited text is its echo, not what the keys brought.
    pub echo: Vec<String>,
    /// `--print`'s screen with reverse video marked, as `capture-pane --marks`.
    pub marks: bool,
    /// How many command lines send-keys types (each Enter, or newline in
    /// text): a wait for `--done` waits for the last of them.
    pub lines: u32,
    /// Over several panes, it waits for each (send-keys' waits, as each
    /// pane got the keys), rather than for any (`fux wait`).
    pub all: bool,
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
        /// The ids it makes, if asked for (`--id`, as layout files have).
        ids: Vec<AnyId>,
        json: bool,
    },
    NewTab {
        target: Option<WsRef>,
        name: Option<String>,
        cmd: Vec<String>,
        size: Size,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        ids: Vec<AnyId>,
        json: bool,
    },
    Split {
        axis: Axis,
        target: Option<PaneId>,
        cmd: Vec<String>,
        /// Where its shell starts (default: where the split pane's is).
        cwd: Option<String>,
        env: Vec<(String, String)>,
        /// `cmd` is the program, started in place of a shell, as `run`
        /// starts one.
        run: bool,
        ids: Vec<AnyId>,
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
        /// A file everything the program writes goes to too.
        log: Option<String>,
        ids: Vec<AnyId>,
        /// Then waits, armed before the program writes anything.
        then: Option<Box<Wait>>,
    },
    /// The program `run` started, started again in its pane.
    Respawn {
        target: Option<PaneId>,
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
    Resize {
        what: Resized,
        size: Size,
    },
    Wait {
        /// The panes, any of which answers it (a tab's or workspace's
        /// every pane): the caller's without one.
        targets: Vec<AnyRef>,
        wait: Wait,
    },
    SendMouse {
        target: Option<PaneId>,
        /// Sent only if the pane's screen is still at this revision.
        if_revision: Option<u64>,
        mouse: Mouse,
        row: u16,
        col: u16,
        /// Then waits, armed before the event goes in, as send-keys' does.
        then: Option<Box<Wait>>,
    },
    KillPane {
        target: Option<PaneId>,
    },
    /// A signal to what runs in the pane's foreground, what its terminal's
    /// keys reach (a job its shell runs, or the program `run` started), never
    /// a shell itself: for a program that ignores Ctrl-C and hangups.
    Signal {
        target: Option<PaneId>,
        signal: fuxix::process::Signal,
        then: Option<Box<Wait>>,
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
        /// The panes: each `-t`, a pane or every pane of a tab; the
        /// caller's without one.
        targets: Vec<AnyRef>,
        /// A client to send them to instead, as typed at its terminal: its
        /// bindings, overlays and prompts take them as a person's keys.
        client: Option<ClientId>,
        /// Sent only if the pane's screen is still at this revision.
        if_revision: Option<u64>,
        inputs: Vec<Input>,
        /// How many times to send them all.
        repeat: u16,
        /// A wait for the program to go quiet once they are sent.
        then: Option<Box<Wait>>,
    },
    /// What happens, a line each as it happens, until the client goes:
    /// where the targets are, or anywhere without any.
    Events {
        targets: Vec<AnyRef>,
        json: bool,
    },
    /// What a layout file says, made all or nothing: its new tabs go to
    /// `workspace` unless they say where.
    /// What is there, as a layout file: everything, or one tab or
    /// workspace.
    DumpLayout {
        target: Option<AnyRef>,
    },
    /// A socket whose commands reach workspace `workspace` alone; or with
    /// `revoke`, that socket closed.
    Grant {
        workspace: Option<WsRef>,
        socket: String,
        revoke: bool,
    },
    /// What the pane's program writes, kept in a file from now on; `None`
    /// stops it.
    Log {
        target: Option<PaneId>,
        file: Option<String>,
    },
    /// The pane's history, dropped.
    ClearHistory {
        target: Option<PaneId>,
    },
    /// The prefix, sent to a pane as its program asked for keys.
    CapturePane {
        target: Option<PaneId>,
        history: Option<usize>,
        view: CaptureView,
        json: bool,
    },
    Reorder {
        subject: Subject,
        toward: Sibling,
    },
    /// A server setting (`history-lines`, `clipboard`, …), as its value's
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
        target: Option<PaneId>,
    },
    /// A command on one client's screen: the client `-c` names, or the one
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
    Capture {
        json: bool,
    },
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

/// A request the server cannot make sense of: exit status 2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Usage {
    /// A word that is not the pane, tab or workspace it must be.
    Not(Kind, String),
    NotClient(String),
    /// What is wrong, said whole.
    Needs(&'static str),
}

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Usage::Not(kind, text) => write!(f, "{text:?} is not a {}", kind.name()),
            Usage::NotClient(text) => write!(f, "{text:?} is not a client: clients are cN"),
            Usage::Needs(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for Usage {}

pub fn parse_pane(text: &str) -> Result<PaneId, Usage> {
    text.parse()
}
pub fn parse_tab(text: &str) -> Result<TabId, Usage> {
    text.parse()
}
/// A workspace: `+N`, or its name.
pub fn parse_workspace(text: &str) -> Result<WsRef, Usage> {
    match text.parse::<WsId>() {
        Ok(id) => Ok(WsRef::Id(id)),
        Err(_) if text.starts_with('+') => Err(Usage::Not(Kind::Workspace, text.to_owned())),
        Err(_) => Ok(WsRef::Name(text.to_owned())),
    }
}
pub fn parse_client(text: &str) -> Result<ClientId, Usage> {
    text.parse()
}

/// Whether `name` is a variable's name a shell takes: letters, digits and
/// `_`, not first a digit.
pub fn shell_name(name: &str) -> bool {
    (name.chars().next()).is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Command {
    /// The pane a command acts on and the wait it then does, armed before
    /// it acts: send-keys', send-mouse's and signal's.
    pub fn acting_wait(&self) -> Option<(Vec<AnyRef>, Wait)> {
        if let Command::SendKeys {
            targets,
            client: None,
            then: Some(wait),
            ..
        } = self
        {
            return Some((targets.clone(), (**wait).clone()));
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
            return Some((
                target.map(AnyRef::Pane).into_iter().collect(),
                (**wait).clone(),
            ));
        }
        None
    }
}

/// Whether a word that is no key name looks like one mistyped, as a person
/// sending keys means it: a modifier before something that is no key
/// (`C-Ent`), a key name in another case (`enter`), or a capitalised word a
/// letter off one or the start of one (`Entr`, `Esc`). Typed as text, it would
/// go to the program unnoticed.
pub fn looks_like_a_key(word: &str) -> bool {
    if word.len() > 2 && ["C-", "M-", "S-"].iter().any(|m| word.starts_with(m)) {
        return true;
    }
    let capital = word.chars().next().is_some_and(char::is_uppercase);
    let lower = word.to_lowercase();
    crate::keys::all_names().iter().any(|name| {
        let name = name.to_lowercase();
        lower == name
            || capital && word.len() >= 3 && (name.starts_with(&lower) || one_off(&lower, &name))
    })
}

/// Whether two words are a letter apart: one changed, added or dropped.
fn one_off(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    match long.len().checked_sub(short.len()) {
        Some(0) => short.iter().zip(long).filter(|(x, y)| x != y).count() == 1,
        Some(1) => (0..long.len()).any(|skip| {
            let rest = long
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, c)| c);
            rest.eq(short.iter())
        }),
        _ => false,
    }
}

/// The lines send-keys types, as a shell or an input box would echo them
/// (Space joins words, BSpace takes the last character back, pasted text is
/// shown too), and how many it ends (Enter, or a newline in text).
pub fn typed_lines(inputs: &[Input], repeat: u16) -> (Vec<String>, u32) {
    let mut lines: Vec<String> = vec![String::new()];
    let mut ended = 0u32;
    let add = |text: &str, lines: &mut Vec<String>, counts: bool, ended: &mut u32| {
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                lines.push(String::new());
                if counts {
                    *ended = ended.saturating_add(1);
                }
            }
            if let Some(last) = lines.last_mut() {
                last.push_str(part);
            }
        }
    };
    for input in std::iter::repeat_n(inputs, usize::from(repeat)).flatten() {
        match input {
            Input::Text(text) => add(text, &mut lines, true, &mut ended),
            Input::Paste(text) => add(text, &mut lines, false, &mut ended),
            Input::Key(key) => match (key.key, key.mods == fux_vt::keys::Modifiers::default()) {
                (fux_vt::keys::Key::Char(c), true) => {
                    add(&c.to_string(), &mut lines, true, &mut ended)
                }
                (fux_vt::keys::Key::Backspace, true) => {
                    if let Some(last) = lines.last_mut() {
                        last.pop();
                    }
                }
                (fux_vt::keys::Key::Enter, _) => {
                    ended = ended.saturating_add(1);
                    lines.push(String::new());
                }
                // Another key (an arrow, Tab, a control key) leaves the
                // line as the program makes it: its echo is not known.
                _ => lines.push(String::new()),
            },
        }
    }
    lines.retain(|l| !l.trim().is_empty());
    (lines, ended)
}

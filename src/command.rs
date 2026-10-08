//! The command grammar: the one set of commands that the CLI, key bindings,
//! the command prompt and the config file all speak.
use crate::keys::Direction;
use crate::layout::{Axis, PaneId};

/// A tab's number, `@N`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabId(pub u32);
/// A workspace's number, `+N`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WsId(pub u32);
/// An attached client's number, `cN`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub u32);

impl std::fmt::Display for TabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}", self.0)
    }
}
impl std::fmt::Display for WsId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "+{}", self.0)
    }
}
impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "c{}", self.0)
    }
}

/// A workspace named on the command line: `+N` or its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsRef {
    Id(WsId),
    Name(String),
}

/// Any target: `%N`, `@N`, `+N` or a workspace name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnyRef {
    Pane(PaneId),
    Tab(TabId),
    Workspace(WsRef),
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
    /// The one given, if one was.
    pub fn target(&self) -> Option<AnyRef> {
        match self {
            Subject::Pane(p) => p.map(AnyRef::Pane),
            Subject::Tab(t) => t.map(AnyRef::Tab),
            Subject::Workspace(w) => w.clone().map(AnyRef::Workspace),
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
    },
    NewTab {
        target: Option<WsRef>,
        name: Option<String>,
        cmd: Vec<String>,
    },
    Split {
        axis: Axis,
        target: Option<PaneId>,
        cmd: Vec<String>,
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
        direction: Direction,
        amount: u16,
    },
    SendKeys {
        target: Option<PaneId>,
        literal: bool,
        keys: Vec<String>,
    },
    /// The prefix, sent to a pane as its program asked for keys.
    SendPrefix {
        target: Option<PaneId>,
    },
    CapturePane {
        target: Option<PaneId>,
        history: Option<usize>,
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
            Usage::UnknownCommand(name) => {
                write!(f, "unknown command {name:?}; `fux help` lists commands")
            }
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
            Usage::UnknownFlag { command, flag } => write!(f, "{command}: unknown flag {flag}"),
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
            Usage::NoCommandAfter { command } => write!(f, "{command} takes no command after --"),
            Usage::Needs(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for Usage {}

fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

pub fn parse_pane(text: &str) -> Result<PaneId, Usage> {
    match text.strip_prefix('%').and_then(number) {
        Some(n) => Ok(PaneId(n)),
        None => Err(Usage::Not(Kind::Pane, text.to_owned())),
    }
}
pub fn parse_tab(text: &str) -> Result<TabId, Usage> {
    match text.strip_prefix('@').and_then(number) {
        Some(n) => Ok(TabId(n)),
        None => Err(Usage::Not(Kind::Tab, text.to_owned())),
    }
}
pub fn parse_workspace(text: &str) -> Result<WsRef, Usage> {
    if let Some(rest) = text.strip_prefix('+') {
        return match number(rest) {
            Some(n) => Ok(WsRef::Id(WsId(n))),
            None => Err(Usage::Not(Kind::Workspace, text.to_owned())),
        };
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
    match text
        .strip_prefix('c')
        .and_then(number)
        .or_else(|| number(text))
    {
        Some(n) => Ok(ClientId(n)),
        None => Err(Usage::NotClient(text.to_owned())),
    }
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

/// A command, described once: how `fux help` shows it, and how its words
/// become a [`Command`].
pub struct Spec {
    /// Its name (`ls|list`: and another), the words after it, and after two
    /// spaces what it does. Every flag here is one the command takes, and
    /// it takes no other; one followed by a placeholder (`-t %N`, unlike
    /// `-h [-t %N]` or `[--json]`) takes a value.
    pub usage: &'static str,
    read: Read,
}

enum Read {
    Flags(fn(&mut Args<'_>) -> Result<Command, Usage>),
    Line(fn(&[String]) -> Result<Command, Usage>),
}

/// A command whose flags come anywhere, as its usage names them, among
/// positionals, and whatever follows `--`. What `read` leaves is refused.
const fn flags(usage: &'static str, read: fn(&mut Args<'_>) -> Result<Command, Usage>) -> Spec {
    Spec {
        usage,
        read: Read::Flags(read),
    }
}

/// A command `read` takes as it is: keys that may look like flags, or a
/// config line, which `Config::apply` reads.
const fn line(usage: &'static str, read: fn(&[String]) -> Result<Command, Usage>) -> Spec {
    Spec {
        usage,
        read: Read::Line(read),
    }
}

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

const PANES: &[Spec] = &[
    flags(
        "split -h|-v [-t %N] [-- CMD...]  -h side by side, -v stacked",
        split,
    ),
    flags("kill-pane [-t %N]  close, without asking", |a| {
        Ok(Command::KillPane { target: a.pane()? })
    }),
    flags("zoom [-c CLIENT]", |a| screen(a, ClientAction::Zoom)),
    flags(
        "resize-pane [-t %N] -L|-R|-U|-D [CELLS]  move a border (one cell)",
        resize,
    ),
    flags("swap-pane [-t %N] (%M | -L|-R|-U|-D)", swap),
    flags(
        "move-pane [-t %N] (--to @N|+N|new-tab|new-workspace | -L|-R|-U|-D)",
        move_pane,
    ),
    flags("copy-mode [-c CLIENT]", |a| {
        screen(a, ClientAction::CopyMode)
    }),
    flags("paste-buffer [-b N] [-t %N]", |a| {
        Ok(Command::PasteBuffer {
            index: a.buffer()?,
            target: a.pane()?,
        })
    }),
    flags("menu pane|tab|workspace [-c CLIENT] [-t TARGET]", |a| {
        let subject = a.subject()?;
        let subject = subject.ok_or(Usage::Needs("usage: menu pane|tab|workspace [-t TARGET]"))?;
        screen(a, ClientAction::Menu(subject))
    }),
    flags(
        "rename-prompt [pane|tab|workspace] [-c CLIENT] [-t TARGET]",
        |a| {
            let subject = a.subject()?.unwrap_or(Subject::Pane(None));
            screen(a, ClientAction::RenamePrompt(subject))
        },
    ),
    flags(
        "confirm-close [pane|tab|workspace] [-c CLIENT] [-t TARGET]",
        |a| {
            let subject = a.subject()?.unwrap_or(Subject::Pane(None));
            screen(a, ClientAction::ConfirmClose(subject))
        },
    ),
    flags(
        "terminate [-t %N]  SIGTERM to the pane's foreground job",
        |a| Ok(Command::Terminate { target: a.pane()? }),
    ),
    flags(
        "choose-pane [-c CLIENT] [-t %N]  a pane to swap with",
        |a| {
            let target = a.pane()?;
            screen(a, ClientAction::ChoosePane { target })
        },
    ),
    line(
        "send-keys [-t %N] [-l] KEYS...  keys, or with -l text",
        send_keys,
    ),
    flags("send-prefix [-t %N]  the prefix key, to the pane", |a| {
        Ok(Command::SendPrefix { target: a.pane()? })
    }),
    flags(
        "reorder pane|tab|workspace [-t TARGET] --next|--previous",
        reorder,
    ),
];

const FOCUS: &[Spec] = &[flags(
    "select-pane [-c CLIENT] -t %N|--next|--previous|--last|-L|-R|-U|-D",
    select_pane,
)];

const TABS: &[Spec] = &[
    flags("new-tab [-t WS] [-n NAME] [-- CMD...]", |a| {
        let target = a.target(parse_workspace)?;
        Ok(Command::NewTab {
            target,
            name: a.name(),
            cmd: a.rest(),
        })
    }),
    flags("select-tab [-c CLIENT] -t @N|--next|--previous", |a| {
        let pick = a.pick(
            parse_tab,
            "select-tab needs one of -t @N, --next, --previous",
        )?;
        screen(a, ClientAction::SelectTab(pick))
    }),
    flags(
        "choose-tab [-c CLIENT] [-t %N] [--move]  where to go, or move the pane",
        |a| {
            let moving = a.moving()?;
            screen(a, ClientAction::ChooseTab { moving })
        },
    ),
    flags("kill-tab [-t @N]", |a| {
        Ok(Command::KillTab {
            target: a.target(parse_tab)?,
        })
    }),
];

const WORKSPACES: &[Spec] = &[
    flags("new-workspace [-n NAME] [-- CMD...]", |a| {
        Ok(Command::NewWorkspace {
            name: a.name(),
            cmd: a.rest(),
        })
    }),
    flags(
        "select-workspace [-c CLIENT] -t WS|--next|--previous",
        |a| {
            let needs = "select-workspace needs one of -t +N, --next, --previous";
            let pick = a.pick(parse_workspace, needs)?;
            screen(a, ClientAction::SelectWorkspace(pick))
        },
    ),
    flags("choose-workspace [-c CLIENT] [-t %N] [--move]", |a| {
        let moving = a.moving()?;
        screen(a, ClientAction::ChooseWorkspace { moving })
    }),
    flags("kill-workspace [-t WS]", |a| {
        Ok(Command::KillWorkspace {
            target: a.target(parse_workspace)?,
        })
    }),
];

const SESSION: &[Spec] = &[
    flags("detach [-c CLIENT]", |a| screen(a, ClientAction::Detach)),
    flags("command-prompt [-c CLIENT]", |a| {
        screen(a, ClientAction::CommandPrompt)
    }),
    flags("command-column [-c CLIENT]", |a| {
        screen(a, ClientAction::CommandColumn)
    }),
    flags("reload  run the config file again", |_| Ok(Command::Reload)),
    flags("kill-server", |_| Ok(Command::KillServer)),
];

const OTHER: &[Spec] = &[
    flags(
        "ls|list [--json]  workspaces, tabs, panes and clients",
        |a| {
            Ok(Command::Ls {
                json: a.has("--json"),
            })
        },
    ),
    flags(
        "rename -t TARGET [--] NAME  TARGET: %N, @N, +N or a name",
        rename,
    ),
    flags("capture-pane [-t %N] [-S LINES] [--json]", |a| {
        let (target, history, json) = (a.pane()?, a.lines()?, a.has("--json"));
        Ok(Command::CapturePane {
            target,
            history,
            json,
        })
    }),
    flags("capture-client [-c CLIENT] [--json]", |a| {
        let json = a.has("--json");
        screen(a, ClientAction::Capture { json })
    }),
    line("set OPTION VALUE", configure),
    line(
        "bind [-n] [-g GROUP] [-r] KEY... COMMAND...  keys after the prefix; V is Shift-v",
        configure,
    ),
    line("unbind [-n] KEY...", configure),
    flags("unbind-all", |_| {
        Ok(Command::Configure {
            argv: vec!["unbind-all".to_owned()],
        })
    }),
    flags("list-buffers", |_| Ok(Command::ListBuffers)),
    flags("show-buffer [-b N]", |a| {
        Ok(Command::ShowBuffer { index: a.buffer()? })
    }),
    flags("list-keys  key names and bindings", |_| {
        Ok(Command::ListKeys)
    }),
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
    /// Whether the usage names `flag`, and if so, whether a value follows it.
    fn flag(&self, flag: &str) -> Option<bool> {
        let grammar = self.usage.split("  ").next().unwrap_or_default();
        let mut words = grammar.split(' ').skip(1).peekable();
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
    rest: Vec<String>,
}

impl<'a> Args<'a> {
    /// Every flag is checked, in order, before any value is read.
    fn read(name: &'static str, spec: &Spec, words: &'a [String]) -> Result<Self, Usage> {
        let mut args = Args {
            name,
            flags: Vec::new(),
            positional: Vec::new(),
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
        Ok(args)
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
    fn target<T>(&self, parse: fn(&str) -> Result<T, Usage>) -> Result<Option<T>, Usage> {
        self.value("-t").map(parse).transpose()
    }
    fn pane(&self) -> Result<Option<PaneId>, Usage> {
        self.target(parse_pane)
    }
    fn name(&self) -> Option<String> {
        self.value("-n").map(str::to_owned)
    }
    fn buffer(&self) -> Result<usize, Usage> {
        let Some(value) = self.value("-b") else {
            return Ok(0);
        };
        number(value)
            .map(|n| n as usize)
            .ok_or_else(|| Usage::NotBuffer {
                command: self.name,
                value: value.to_owned(),
            })
    }
    fn lines(&self) -> Result<Option<usize>, Usage> {
        self.value("-S")
            .map(|v| {
                let lines = v.strip_prefix('-').unwrap_or(v).parse();
                lines.map_err(|_| Usage::NotLines(v.to_owned()))
            })
            .transpose()
    }
    fn rest(&mut self) -> Vec<String> {
        std::mem::take(&mut self.rest)
    }
    /// The pane, tab or workspace a positional kind and `-t` name, if
    /// either does: of another kind than the kind given, `-t` is refused in
    /// the words a target of the wrong kind gets, rather than one of the
    /// two silently winning.
    fn subject(&mut self) -> Result<Option<Subject>, Usage> {
        let kind = match self.positional.pop() {
            Some(word) => Some(
                [Kind::Pane, Kind::Tab, Kind::Workspace]
                    .into_iter()
                    .find(|k| k.name() == word)
                    .ok_or_else(|| Usage::NotKind {
                        command: self.name,
                        word: word.to_owned(),
                    })?,
            ),
            None => None,
        };
        let Some(text) = self.value("-t") else {
            return Ok(kind.map(Subject::own));
        };
        let target = parse_any(text)?;
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

/// An action on the screen of the client `-c` names, if it names one.
fn screen(a: &Args<'_>, action: ClientAction) -> Result<Command, Usage> {
    let client = a.value("-c").map(parse_client).transpose()?;
    Ok(Command::Client { client, action })
}

fn configure(argv: &[String]) -> Result<Command, Usage> {
    Ok(Command::Configure {
        argv: argv.to_vec(),
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
    let to = a.flags.iter().rev().find_map(|&(flag, value)| match value {
        Some(value) if flag == "--to" => Some(parse_move_to(value)),
        _ => direction_flag(flag).map(|d| Ok(MoveTo::Beside(d))),
    });
    let to = to.ok_or(Usage::Needs(
        "move-pane needs --to @N|+N|new-tab|new-workspace or -L/-R/-U/-D",
    ))??;
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
    let direction = a
        .direction()
        .ok_or(Usage::Needs("resize-pane needs -L, -R, -U or -D"))?;
    let amount = match a.positional.pop() {
        Some(n) => match number(n).and_then(|n| u16::try_from(n).ok()) {
            Some(n) if n > 0 => n,
            _ => return Err(Usage::NotCells(n.to_owned())),
        },
        None => 1,
    };
    Ok(Command::ResizePane {
        target,
        direction,
        amount,
    })
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

fn select_pane(a: &mut Args<'_>) -> Result<Command, Usage> {
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
    screen(a, ClientAction::SelectPane(pick))
}

/// Keys may look like flags (`-`), so only leading flags count.
fn send_keys(argv: &[String]) -> Result<Command, Usage> {
    let (mut target, mut literal) = (None, false);
    let mut words = argv.iter().skip(1).peekable();
    while let Some(word) = words.next_if(|w| ["-t", "-l", "--"].contains(&w.as_str())) {
        match word.as_str() {
            "-t" => {
                let value = words.next().ok_or_else(|| Usage::NeedsValue {
                    command: "send-keys",
                    flag: word.clone(),
                })?;
                target = Some(parse_pane(value)?);
            }
            "-l" => literal = true,
            _ => break,
        }
    }
    let keys: Vec<String> = words.cloned().collect();
    if keys.is_empty() {
        return Err(Usage::Needs("send-keys needs keys to send"));
    }
    Ok(Command::SendKeys {
        target,
        literal,
        keys,
    })
}

/// Parses one command line.
pub fn parse(argv: &[String]) -> Result<Command, Usage> {
    let (name, words) = argv.split_first().ok_or(Usage::NoCommand)?;
    let (name, _, spec) = find(name).ok_or_else(|| Usage::UnknownCommand(name.clone()))?;
    let read = match spec.read {
        Read::Line(read) => return read(argv),
        Read::Flags(read) => read,
    };
    let mut args = Args::read(name, spec, words)?;
    let command = read(&mut args)?;
    if let Some(word) = args.positional.first() {
        return Err(Usage::Unexpected {
            command: name,
            word: (*word).to_owned(),
        });
    }
    if !args.rest.is_empty() {
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
    fn the_cli_table_parses() {
        assert_eq!(cmd("ls --json"), Ok(Command::Ls { json: true }));
        assert_eq!(
            cmd("split -h -t %3 -- htop -d 5"),
            Ok(Command::Split {
                axis: Axis::Horizontal,
                target: Some(PaneId(3)),
                cmd: vec!["htop".into(), "-d".into(), "5".into()]
            })
        );
        assert_eq!(
            cmd("new-tab -t work -n logs"),
            Ok(Command::NewTab {
                target: Some(WsRef::Name("work".into())),
                name: Some("logs".into()),
                cmd: vec![]
            })
        );
        assert_eq!(
            cmd("move-pane -t %1 --to +2"),
            Ok(Command::MovePane {
                target: Some(PaneId(1)),
                to: MoveTo::Workspace(WsRef::Id(WsId(2)))
            })
        );
        assert_eq!(
            cmd("swap-pane -t %1 %2"),
            Ok(Command::SwapPane {
                target: Some(PaneId(1)),
                with: SwapWith::Pane(PaneId(2))
            })
        );
        assert_eq!(
            cmd("resize-pane -t %1 -L 5"),
            Ok(Command::ResizePane {
                target: Some(PaneId(1)),
                direction: Direction::Left,
                amount: 5
            })
        );
        assert_eq!(
            cmd("send-keys -t %1 -l -x"),
            Ok(Command::SendKeys {
                target: Some(PaneId(1)),
                literal: true,
                keys: vec!["-x".into()]
            })
        );
        assert_eq!(
            cmd("capture-pane -t %1 -S -100 --json"),
            Ok(Command::CapturePane {
                target: Some(PaneId(1)),
                history: Some(100),
                json: true
            })
        );
        assert_eq!(
            cmd("rename -t @2 'two words'"),
            Ok(Command::Rename {
                target: AnyRef::Tab(TabId(2)),
                name: "two words".into()
            })
        );
        assert_eq!(
            cmd("select-pane -c c1 --last"),
            Ok(Command::Client {
                client: Some(ClientId(1)),
                action: ClientAction::SelectPane(PanePick::Last)
            })
        );
        assert_eq!(
            cmd("menu tab"),
            Ok(ClientAction::Menu(Subject::Tab(None)).here())
        );
        assert_eq!(
            cmd("confirm-close -t +1"),
            Ok(ClientAction::ConfirmClose(Subject::Workspace(Some(WsRef::Id(WsId(1))))).here())
        );
        assert_eq!(
            cmd("paste-buffer -b 2 -t %4"),
            Ok(Command::PasteBuffer {
                index: 2,
                target: Some(PaneId(4))
            })
        );
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
            "list-keys --bogus",
            "unbind-all --nope",
            "reload extra words",
            "reload -- x",
            "list-buffers 1",
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
            ("resize-pane", "resize-pane needs -L, -R, -U or -D"),
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
            // A screen command takes only the flags it uses: none is
            // accepted and then ignored.
            ("zoom -t %1", "zoom: unknown flag -t"),
            ("copy-mode -c c1 -t %1", "copy-mode: unknown flag -t"),
            (
                "command-prompt --move",
                "command-prompt: unknown flag --move",
            ),
            ("command-column -L", "command-column: unknown flag -L"),
            ("choose-pane --move", "choose-pane: unknown flag --move"),
            ("menu pane --next", "menu: unknown flag --next"),
            ("select-tab -L", "select-tab: unknown flag -L"),
            (
                "select-workspace --last",
                "select-workspace: unknown flag --last",
            ),
            ("zoom pane", r#"zoom: unexpected argument "pane""#),
            ("choose-tab tab", r#"choose-tab: unexpected argument "tab""#),
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
            let grammar = spec.usage.split("  ").next().unwrap_or_default();
            for flag in grammar.split([' ', '|', '[', ']', '(', ')']) {
                if flag.starts_with('-') && flag != "--" {
                    let value = ["-t", "-c", "-n", "-b", "-S", "--to"].contains(&flag);
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
        for binding in crate::config::Config::default().bindings {
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

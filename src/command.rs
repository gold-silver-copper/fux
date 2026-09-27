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

/// Which pane, tab or workspace a `select-…` command picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick<T> {
    Next,
    Previous,
    /// The previously focused pane (panes only).
    Last,
    Toward(Direction),
    Id(T),
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
    CapturePane {
        target: Option<PaneId>,
        history: Option<usize>,
        json: bool,
    },
    Terminate {
        target: Option<PaneId>,
    },
    Reorder {
        kind: Kind,
        target: Option<AnyRef>,
        forward: bool,
    },
    Set {
        argv: Vec<String>,
    },
    Bind {
        argv: Vec<String>,
    },
    Unbind {
        argv: Vec<String>,
    },
    UnbindAll,
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
    ChooseTab {
        moving: Option<PaneId>,
        moving_now: bool,
    },
    ChooseWorkspace {
        moving: Option<PaneId>,
        moving_now: bool,
    },
    ChoosePane {
        target: Option<PaneId>,
    },
    Menu {
        kind: Kind,
        target: Option<AnyRef>,
    },
    CommandPrompt,
    CopyMode,
    RenamePrompt {
        kind: Kind,
        target: Option<AnyRef>,
    },
    ConfirmClose {
        kind: Kind,
        target: Option<AnyRef>,
    },
    Zoom,
    SelectPane(Pick<PaneId>),
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
    // A word that is not the target it must be.
    NotPane(String),
    NotTab(String),
    NotWorkspace(String),
    NotClient(String),
    /// A flag of `command` given no value.
    NeedsValue {
        command: String,
        flag: String,
    },
    UnknownFlag {
        command: String,
        flag: String,
    },
    Unexpected {
        command: String,
        word: String,
    },
    /// A word that is not `pane`, `tab` or `workspace`.
    NotKind {
        command: String,
        word: String,
    },
    NotBuffer {
        command: String,
        value: String,
    },
    NotCells(String),
    NotLines(String),
    /// Words after `--` for a command that takes none.
    NoCommandAfter {
        command: String,
    },
    // What a command needs and was not given.
    SplitAxis,
    RenameTarget,
    RenameName,
    MoveTo,
    SwapWith,
    ResizeDirection,
    NoKeys,
    ReorderKind,
    ReorderDirection,
    MenuKind,
    SelectPane,
    SelectTab,
    SelectWorkspace,
}

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Usage::NoCommand => f.write_str("no command given; `fux help` lists commands"),
            Usage::UnknownCommand(name) => {
                write!(f, "unknown command {name:?}; `fux help` lists commands")
            }
            Usage::NotPane(text) => write!(f, "{text:?} is not a pane; panes are %N"),
            Usage::NotTab(text) => write!(f, "{text:?} is not a tab; tabs are @N"),
            Usage::NotWorkspace(text) => {
                write!(
                    f,
                    "{text:?} is not a workspace; workspaces are +N or a name"
                )
            }
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
            Usage::SplitAxis => f.write_str("split needs -h (side by side) or -v (stacked)"),
            Usage::RenameTarget => {
                f.write_str("rename needs -t TARGET (%N, @N, +N or a workspace name)")
            }
            Usage::RenameName => {
                f.write_str("usage: rename -t TARGET NAME (quote a name with spaces)")
            }
            Usage::MoveTo => {
                f.write_str("move-pane needs --to @N|+N|new-tab|new-workspace or -L/-R/-U/-D")
            }
            Usage::SwapWith => f.write_str("swap-pane needs another pane (%N) or -L/-R/-U/-D"),
            Usage::ResizeDirection => f.write_str("resize-pane needs -L, -R, -U or -D"),
            Usage::NoKeys => f.write_str("send-keys needs keys to send"),
            Usage::ReorderKind => {
                f.write_str("usage: reorder pane|tab|workspace [-t TARGET] --next|--previous")
            }
            Usage::ReorderDirection => f.write_str("reorder needs --next or --previous"),
            Usage::MenuKind => f.write_str("usage: menu pane|tab|workspace [-t TARGET]"),
            Usage::SelectPane => f.write_str(
                "select-pane needs one of -t %N, --next, --previous, --last, -L/-R/-U/-D",
            ),
            Usage::SelectTab => f.write_str("select-tab needs one of -t @N, --next, --previous"),
            Usage::SelectWorkspace => {
                f.write_str("select-workspace needs one of -t +N, --next, --previous")
            }
        }
    }
}

impl std::error::Error for Usage {}

fn needs_value(command: &str, flag: &str) -> Usage {
    Usage::NeedsValue {
        command: command.to_owned(),
        flag: flag.to_owned(),
    }
}

fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

pub fn parse_pane(text: &str) -> Result<PaneId, Usage> {
    match text.strip_prefix('%').and_then(number) {
        Some(n) => Ok(PaneId(n)),
        None => Err(Usage::NotPane(text.to_owned())),
    }
}
pub fn parse_tab(text: &str) -> Result<TabId, Usage> {
    match text.strip_prefix('@').and_then(number) {
        Some(n) => Ok(TabId(n)),
        None => Err(Usage::NotTab(text.to_owned())),
    }
}
pub fn parse_workspace(text: &str) -> Result<WsRef, Usage> {
    if let Some(rest) = text.strip_prefix('+') {
        return match number(rest) {
            Some(n) => Ok(WsRef::Id(WsId(n))),
            None => Err(Usage::NotWorkspace(text.to_owned())),
        };
    }
    if text.is_empty() || text.starts_with(['%', '@', '-']) {
        return Err(Usage::NotWorkspace(text.to_owned()));
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
fn parse_kind(text: &str) -> Option<Kind> {
    [Kind::Pane, Kind::Tab, Kind::Workspace]
        .into_iter()
        .find(|k| k.name() == text)
}
fn direction_flag(flag: &str) -> Option<Direction> {
    Direction::ALL.into_iter().find(|d| d.flag() == flag)
}

/// Words after the command name: flags (some taking a value), positionals,
/// and whatever follows `--`.
struct Args<'a> {
    name: &'a str,
    words: std::slice::Iter<'a, String>,
    rest: Vec<String>,
    positional: Vec<&'a str>,
}

impl<'a> Args<'a> {
    fn new(name: &'a str, words: &'a [String]) -> Self {
        Self {
            name,
            words: words.iter(),
            rest: Vec::new(),
            positional: Vec::new(),
        }
    }
    /// The next flag, with positionals collected aside, until `--` or the end.
    fn flag(&mut self) -> Option<&'a str> {
        loop {
            let word = self.words.next()?;
            if word == "--" {
                self.rest = self.words.by_ref().cloned().collect();
                return None;
            }
            if word.starts_with('-') && word.len() > 1 {
                return Some(word.as_str());
            }
            self.positional.push(word.as_str());
        }
    }
    fn value(&mut self, flag: &str) -> Result<&'a str, Usage> {
        match self.words.next() {
            Some(v) => Ok(v.as_str()),
            None => Err(needs_value(self.name, flag)),
        }
    }
    fn unknown<T>(&self, flag: &str) -> Result<T, Usage> {
        Err(Usage::UnknownFlag {
            command: self.name.to_owned(),
            flag: flag.to_owned(),
        })
    }
    fn no_positional(&self) -> Result<(), Usage> {
        match self.positional.first() {
            Some(word) => Err(Usage::Unexpected {
                command: self.name.to_owned(),
                word: (*word).to_owned(),
            }),
            None => Ok(()),
        }
    }
    fn not_kind(&self, word: &str) -> Usage {
        Usage::NotKind {
            command: self.name.to_owned(),
            word: word.to_owned(),
        }
    }
}

/// Parses one command line.
pub fn parse(argv: &[String]) -> Result<Command, Usage> {
    let Some((name, words)) = argv.split_first() else {
        return Err(Usage::NoCommand);
    };
    let name = name.as_str();
    let mut a = Args::new(name, words);
    let mut client: Option<ClientId> = None;
    let command = match name {
        "ls" | "list" => {
            let mut json = false;
            while let Some(flag) = a.flag() {
                match flag {
                    "--json" => json = true,
                    other => return a.unknown(other),
                }
            }
            Command::Ls { json }
        }
        "kill-server" => Command::KillServer,
        "list-keys" => Command::ListKeys,
        "new-workspace" | "new-tab" => {
            let (mut target, mut ws_name) = (None, None);
            while let Some(flag) = a.flag() {
                match flag {
                    "-n" => ws_name = Some(a.value(flag)?.to_owned()),
                    "-t" if name == "new-tab" => target = Some(parse_workspace(a.value(flag)?)?),
                    other => return a.unknown(other),
                }
            }
            let cmd = std::mem::take(&mut a.rest);
            if name == "new-tab" {
                Command::NewTab {
                    target,
                    name: ws_name,
                    cmd,
                }
            } else {
                Command::NewWorkspace { name: ws_name, cmd }
            }
        }
        "split" => {
            let (mut axis, mut target) = (None, None);
            while let Some(flag) = a.flag() {
                match flag {
                    "-h" => axis = Some(Axis::Horizontal),
                    "-v" => axis = Some(Axis::Vertical),
                    "-t" => target = Some(parse_pane(a.value(flag)?)?),
                    other => return a.unknown(other),
                }
            }
            let Some(axis) = axis else {
                return Err(Usage::SplitAxis);
            };
            Command::Split {
                axis,
                target,
                cmd: std::mem::take(&mut a.rest),
            }
        }
        "kill-pane" | "kill-tab" | "kill-workspace" | "terminate" => {
            let mut target = None;
            while let Some(flag) = a.flag() {
                match flag {
                    "-t" => target = Some(a.value(flag)?),
                    other => return a.unknown(other),
                }
            }
            match name {
                "kill-pane" => Command::KillPane {
                    target: target.map(parse_pane).transpose()?,
                },
                "terminate" => Command::Terminate {
                    target: target.map(parse_pane).transpose()?,
                },
                "kill-tab" => Command::KillTab {
                    target: target.map(parse_tab).transpose()?,
                },
                _ => Command::KillWorkspace {
                    target: target.map(parse_workspace).transpose()?,
                },
            }
        }
        "rename" => {
            let mut target = None;
            while let Some(flag) = a.flag() {
                match flag {
                    "-t" => target = Some(parse_any(a.value(flag)?)?),
                    other => return a.unknown(other),
                }
            }
            let Some(target) = target else {
                return Err(Usage::RenameTarget);
            };
            let name_words: Vec<String> = a
                .positional
                .iter()
                .map(|s| (*s).to_owned())
                .chain(a.rest.iter().cloned())
                .collect();
            let [new_name] = name_words.as_slice() else {
                return Err(Usage::RenameName);
            };
            a.positional.clear();
            Command::Rename {
                target,
                name: new_name.clone(),
            }
        }
        "move-pane" => {
            let (mut target, mut to) = (None, None);
            while let Some(flag) = a.flag() {
                if let Some(direction) = direction_flag(flag) {
                    to = Some(MoveTo::Beside(direction));
                    continue;
                }
                match flag {
                    "-t" => target = Some(parse_pane(a.value(flag)?)?),
                    "--to" => {
                        let value = a.value(flag)?;
                        to = Some(match value {
                            "new-tab" => MoveTo::NewTab,
                            "new-workspace" => MoveTo::NewWorkspace,
                            v if v.starts_with('@') => MoveTo::Tab(parse_tab(v)?),
                            v => MoveTo::Workspace(parse_workspace(v)?),
                        });
                    }
                    other => return a.unknown(other),
                }
            }
            let Some(to) = to else {
                return Err(Usage::MoveTo);
            };
            Command::MovePane { target, to }
        }
        "swap-pane" => {
            let (mut target, mut with) = (None, None);
            while let Some(flag) = a.flag() {
                if let Some(direction) = direction_flag(flag) {
                    with = Some(SwapWith::Toward(direction));
                    continue;
                }
                match flag {
                    "-t" => target = Some(parse_pane(a.value(flag)?)?),
                    other => return a.unknown(other),
                }
            }
            if let Some(other) = a.positional.pop() {
                with = Some(SwapWith::Pane(parse_pane(other)?));
            }
            let Some(with) = with else {
                return Err(Usage::SwapWith);
            };
            Command::SwapPane { target, with }
        }
        "resize-pane" => {
            let (mut target, mut direction) = (None, None);
            while let Some(flag) = a.flag() {
                if let Some(d) = direction_flag(flag) {
                    direction = Some(d);
                    continue;
                }
                match flag {
                    "-t" => target = Some(parse_pane(a.value(flag)?)?),
                    other => return a.unknown(other),
                }
            }
            let Some(direction) = direction else {
                return Err(Usage::ResizeDirection);
            };
            let amount = match a.positional.pop() {
                Some(n) => match number(n).and_then(|n| u16::try_from(n).ok()) {
                    Some(n) if n > 0 => n,
                    _ => return Err(Usage::NotCells(n.to_owned())),
                },
                None => 1,
            };
            Command::ResizePane {
                target,
                direction,
                amount,
            }
        }
        "send-keys" => {
            let (mut target, mut literal) = (None, false);
            // Keys may look like flags (`-`), so only leading flags count.
            let mut keys = Vec::new();
            let mut iter = words.iter().peekable();
            while let Some(word) = iter.peek() {
                match word.as_str() {
                    "-t" => {
                        iter.next();
                        let Some(value) = iter.next() else {
                            return Err(needs_value(name, "-t"));
                        };
                        target = Some(parse_pane(value)?);
                    }
                    "-l" => {
                        iter.next();
                        literal = true;
                    }
                    "--" => {
                        iter.next();
                        break;
                    }
                    _ => break,
                }
            }
            keys.extend(iter.cloned());
            if keys.is_empty() {
                return Err(Usage::NoKeys);
            }
            return Ok(Command::SendKeys {
                target,
                literal,
                keys,
            });
        }
        "capture-pane" => {
            let (mut target, mut history, mut json) = (None, None, false);
            let mut iter = words.iter();
            while let Some(word) = iter.next() {
                match word.as_str() {
                    "-t" => {
                        let value = iter.next().ok_or_else(|| needs_value(name, "-t"))?;
                        target = Some(parse_pane(value)?);
                    }
                    "-S" => {
                        let value = iter.next().ok_or_else(|| needs_value(name, "-S"))?;
                        let lines = value
                            .strip_prefix('-')
                            .unwrap_or(value)
                            .parse::<usize>()
                            .map_err(|_| Usage::NotLines(value.clone()))?;
                        history = Some(lines);
                    }
                    "--json" => json = true,
                    other => return a.unknown(other),
                }
            }
            return Ok(Command::CapturePane {
                target,
                history,
                json,
            });
        }
        "reorder" => {
            let (mut target, mut forward) = (None, None);
            while let Some(flag) = a.flag() {
                match flag {
                    "-t" => target = Some(parse_any(a.value(flag)?)?),
                    "--next" => forward = Some(true),
                    "--previous" => forward = Some(false),
                    other => return a.unknown(other),
                }
            }
            let kind = match a.positional.pop() {
                Some(k) => parse_kind(k).ok_or_else(|| a.not_kind(k))?,
                None => target
                    .as_ref()
                    .map(AnyRef::kind)
                    .ok_or(Usage::ReorderKind)?,
            };
            let Some(forward) = forward else {
                return Err(Usage::ReorderDirection);
            };
            Command::Reorder {
                kind,
                target,
                forward,
            }
        }
        "set" | "bind" | "unbind" => {
            let argv = argv.to_vec();
            return Ok(match name {
                "set" => Command::Set { argv },
                "bind" => Command::Bind { argv },
                _ => Command::Unbind { argv },
            });
        }
        "unbind-all" => Command::UnbindAll,
        "reload" => Command::Reload,
        "list-buffers" => Command::ListBuffers,
        "show-buffer" | "paste-buffer" => {
            let (mut index, mut target) = (0usize, None);
            while let Some(flag) = a.flag() {
                match flag {
                    "-b" => {
                        let value = a.value(flag)?;
                        index =
                            number(value)
                                .map(|n| n as usize)
                                .ok_or_else(|| Usage::NotBuffer {
                                    command: name.to_owned(),
                                    value: value.to_owned(),
                                })?;
                    }
                    "-t" if name == "paste-buffer" => target = Some(parse_pane(a.value(flag)?)?),
                    other => return a.unknown(other),
                }
            }
            if name == "show-buffer" {
                Command::ShowBuffer { index }
            } else {
                Command::PasteBuffer { index, target }
            }
        }
        "detach" => {
            while let Some(flag) = a.flag() {
                match flag {
                    "-c" => client = Some(parse_client(a.value(flag)?)?),
                    other => return a.unknown(other),
                }
            }
            Command::Client {
                client,
                action: ClientAction::Detach,
            }
        }
        "capture-client" => {
            let mut json = false;
            while let Some(flag) = a.flag() {
                match flag {
                    "-c" => client = Some(parse_client(a.value(flag)?)?),
                    "--json" => json = true,
                    other => return a.unknown(other),
                }
            }
            a.no_positional()?;
            Command::Client {
                client,
                action: ClientAction::Capture { json },
            }
        }
        "command-column" | "command-prompt" | "copy-mode" | "zoom" | "choose-tab"
        | "choose-workspace" | "choose-pane" | "menu" | "rename-prompt" | "confirm-close"
        | "select-pane" | "select-tab" | "select-workspace" => {
            let mut target: Option<&str> = None;
            let mut pick: Option<&str> = None;
            let mut moving = false;
            let mut direction = None;
            while let Some(flag) = a.flag() {
                if let Some(d) = direction_flag(flag) {
                    direction = Some(d);
                    continue;
                }
                match flag {
                    "-c" => client = Some(parse_client(a.value(flag)?)?),
                    "-t" => target = Some(a.value(flag)?),
                    "--next" | "--previous" | "--last" => pick = Some(flag),
                    "--move" => moving = true,
                    other => return a.unknown(other),
                }
            }
            let kind = match a.positional.pop() {
                Some(k) => Some(parse_kind(k).ok_or_else(|| a.not_kind(k))?),
                None => None,
            };
            a.no_positional()?;
            let any = target.map(parse_any).transpose()?;
            // Given, or the target's; a pane's by default, but for a menu.
            let kind = kind.or(any.as_ref().map(AnyRef::kind));
            let moving_now = moving;
            let action = match name {
                "command-column" => ClientAction::CommandColumn,
                "command-prompt" => ClientAction::CommandPrompt,
                "copy-mode" => ClientAction::CopyMode,
                "zoom" => ClientAction::Zoom,
                "choose-tab" => ClientAction::ChooseTab {
                    moving: target.map(parse_pane).transpose()?,
                    moving_now,
                },
                "choose-workspace" => ClientAction::ChooseWorkspace {
                    moving: target.map(parse_pane).transpose()?,
                    moving_now,
                },
                "choose-pane" => ClientAction::ChoosePane {
                    target: target.map(parse_pane).transpose()?,
                },
                "menu" => ClientAction::Menu {
                    kind: kind.ok_or(Usage::MenuKind)?,
                    target: any,
                },
                "rename-prompt" => ClientAction::RenamePrompt {
                    kind: kind.unwrap_or(Kind::Pane),
                    target: any,
                },
                "confirm-close" => ClientAction::ConfirmClose {
                    kind: kind.unwrap_or(Kind::Pane),
                    target: any,
                },
                "select-pane" => ClientAction::SelectPane(match (pick, direction, target) {
                    (Some("--next"), None, None) => Pick::Next,
                    (Some("--previous"), None, None) => Pick::Previous,
                    (Some("--last"), None, None) => Pick::Last,
                    (None, Some(d), None) => Pick::Toward(d),
                    (None, None, Some(t)) => Pick::Id(parse_pane(t)?),
                    _ => return Err(Usage::SelectPane),
                }),
                "select-tab" => ClientAction::SelectTab(match (pick, target) {
                    (Some("--next"), None) => Pick::Next,
                    (Some("--previous"), None) => Pick::Previous,
                    (None, Some(t)) => Pick::Id(parse_tab(t)?),
                    _ => return Err(Usage::SelectTab),
                }),
                _ => ClientAction::SelectWorkspace(match (pick, target) {
                    (Some("--next"), None) => Pick::Next,
                    (Some("--previous"), None) => Pick::Previous,
                    (None, Some(t)) => Pick::Id(parse_workspace(t)?),
                    _ => return Err(Usage::SelectWorkspace),
                }),
            };
            Command::Client { client, action }
        }
        other => return Err(Usage::UnknownCommand(other.to_owned())),
    };
    a.no_positional()?;
    if !a.rest.is_empty()
        && !matches!(
            command,
            Command::NewWorkspace { .. }
                | Command::NewTab { .. }
                | Command::Split { .. }
                | Command::Rename { .. }
        )
    {
        return Err(Usage::NoCommandAfter {
            command: name.to_owned(),
        });
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
                action: ClientAction::SelectPane(Pick::Last)
            })
        );
        assert_eq!(
            cmd("menu tab"),
            Ok(ClientAction::Menu {
                kind: Kind::Tab,
                target: None
            }
            .here())
        );
        assert_eq!(
            cmd("confirm-close -t +1"),
            Ok(ClientAction::ConfirmClose {
                kind: Kind::Workspace,
                target: Some(AnyRef::Workspace(WsRef::Id(WsId(1))))
            }
            .here())
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
        for line in [
            "",
            "nope",
            "split",
            "split -h -t 3",
            "kill-tab -t %1",
            "rename -t %1",
            "rename %1 x",
            "move-pane",
            "resize-pane -t %1",
            "resize-pane -L zero",
            "send-keys -t %1",
            "select-pane",
            "select-pane --next -L",
            "ls extra",
            "kill-pane -- x",
            "detach -c zz",
            "new-tab -t %1",
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
        ] {
            let usage = cmd(line).err().map(|u| u.to_string());
            assert_eq!(usage.as_deref(), Some(message), "{line:?}");
        }
        assert_eq!(cmd("split -t"), Err(needs_value("split", "-t")));
        assert_eq!(cmd("kill-pane -t 3"), Err(Usage::NotPane("3".into())));
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

//! The command grammar: the one set of commands that the CLI, key bindings,
//! the `:` prompt and the config file all speak.
use crate::keys::{Direction, KeyPress};
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
#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
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
    Detach {
        client: Option<ClientId>,
    },
    // Commands on one client's screen.
    CommandColumn {
        client: Option<ClientId>,
    },
    ChooseTab {
        client: Option<ClientId>,
        moving: Option<PaneId>,
        moving_now: bool,
    },
    ChooseWorkspace {
        client: Option<ClientId>,
        moving: Option<PaneId>,
        moving_now: bool,
    },
    ChoosePane {
        client: Option<ClientId>,
        target: Option<PaneId>,
    },
    Menu {
        client: Option<ClientId>,
        kind: Kind,
        target: Option<AnyRef>,
    },
    CommandPrompt {
        client: Option<ClientId>,
    },
    CopyMode {
        client: Option<ClientId>,
    },
    RenamePrompt {
        client: Option<ClientId>,
        kind: Kind,
        target: Option<AnyRef>,
    },
    ConfirmClose {
        client: Option<ClientId>,
        kind: Kind,
        target: Option<AnyRef>,
    },
    Zoom {
        client: Option<ClientId>,
    },
    SelectPane {
        client: Option<ClientId>,
        pick: Pick<PaneId>,
    },
    SelectTab {
        client: Option<ClientId>,
        pick: Pick<TabId>,
    },
    SelectWorkspace {
        client: Option<ClientId>,
        pick: Pick<WsRef>,
    },
}

/// A command line that is not a valid command: exit status 2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage(pub String);

fn usage<T>(message: impl Into<String>) -> Result<T, Usage> {
    Err(Usage(message.into()))
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
        None => usage(format!("{text:?} is not a pane; panes are %N")),
    }
}
pub fn parse_tab(text: &str) -> Result<TabId, Usage> {
    match text.strip_prefix('@').and_then(number) {
        Some(n) => Ok(TabId(n)),
        None => usage(format!("{text:?} is not a tab; tabs are @N")),
    }
}
pub fn parse_workspace(text: &str) -> Result<WsRef, Usage> {
    if let Some(rest) = text.strip_prefix('+') {
        return match number(rest) {
            Some(n) => Ok(WsRef::Id(WsId(n))),
            None => usage(format!(
                "{text:?} is not a workspace; workspaces are +N or a name"
            )),
        };
    }
    if text.is_empty() || text.starts_with(['%', '@', '-']) {
        return usage(format!(
            "{text:?} is not a workspace; workspaces are +N or a name"
        ));
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
        None => usage(format!(
            "{text:?} is not a client; `fux ls` lists clients as cN"
        )),
    }
}
fn parse_kind(text: &str) -> Option<Kind> {
    match text {
        "pane" => Some(Kind::Pane),
        "tab" => Some(Kind::Tab),
        "workspace" => Some(Kind::Workspace),
        _ => None,
    }
}
fn direction_flag(flag: &str) -> Option<Direction> {
    match flag {
        "-L" => Some(Direction::Left),
        "-R" => Some(Direction::Right),
        "-U" => Some(Direction::Up),
        "-D" => Some(Direction::Down),
        _ => None,
    }
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
            if word.starts_with('-') && word.len() > 1 && !self.at_amount(word) {
                return Some(word.as_str());
            }
            self.positional.push(word.as_str());
        }
    }
    /// `-5` is a number, not a flag, only for `capture-pane -S`.
    fn at_amount(&self, _word: &str) -> bool {
        false
    }
    fn value(&mut self, flag: &str) -> Result<&'a str, Usage> {
        match self.words.next() {
            Some(v) => Ok(v.as_str()),
            None => usage(format!("{} {flag} needs a value", self.name)),
        }
    }
    fn unknown<T>(&self, flag: &str) -> Result<T, Usage> {
        usage(format!("{}: unknown flag {flag}", self.name))
    }
    fn no_positional(&self) -> Result<(), Usage> {
        match self.positional.first() {
            Some(word) => usage(format!("{}: unexpected argument {word:?}", self.name)),
            None => Ok(()),
        }
    }
}

/// Parses one command line.
pub fn parse(argv: &[String]) -> Result<Command, Usage> {
    let Some((name, words)) = argv.split_first() else {
        return usage("no command given; `fux help` lists commands");
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
                return usage("split needs -h (side by side) or -v (stacked)");
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
                return usage("rename needs -t TARGET (%N, @N, +N or a workspace name)");
            };
            let name_words: Vec<String> = a
                .positional
                .iter()
                .map(|s| (*s).to_owned())
                .chain(a.rest.iter().cloned())
                .collect();
            let [new_name] = name_words.as_slice() else {
                return usage("usage: rename -t TARGET NAME (quote a name with spaces)");
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
                return usage("move-pane needs --to @N|+N|new-tab|new-workspace or -L/-R/-U/-D");
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
                return usage("swap-pane needs another pane (%N) or -L/-R/-U/-D");
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
                return usage("resize-pane needs -L, -R, -U or -D");
            };
            let amount = match a.positional.pop() {
                Some(n) => match number(n).and_then(|n| u16::try_from(n).ok()) {
                    Some(n) if n > 0 => n,
                    _ => return usage(format!("resize-pane: {n:?} is not a number of cells")),
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
                            return usage("send-keys -t needs a value");
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
                return usage("send-keys needs keys to send");
            }
            if !literal {
                for key in &keys {
                    key.parse::<KeyPress>().map_err(Usage)?;
                }
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
                        let value = iter
                            .next()
                            .ok_or(Usage("capture-pane -t needs a value".into()))?;
                        target = Some(parse_pane(value)?);
                    }
                    "-S" => {
                        let value = iter
                            .next()
                            .ok_or(Usage("capture-pane -S needs a value".into()))?;
                        let lines = value
                            .strip_prefix('-')
                            .unwrap_or(value)
                            .parse::<usize>()
                            .map_err(|_| Usage(format!("capture-pane -S: {value:?} is not -N")))?;
                        history = Some(lines);
                    }
                    "--json" => json = true,
                    other => return usage(format!("capture-pane: unknown flag {other}")),
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
            let kind = match (a.positional.pop(), &target) {
                (Some(k), _) => parse_kind(k).ok_or(Usage(format!(
                    "reorder: {k:?} is not pane, tab or workspace"
                )))?,
                (None, Some(AnyRef::Pane(_))) => Kind::Pane,
                (None, Some(AnyRef::Tab(_))) => Kind::Tab,
                (None, Some(AnyRef::Workspace(_))) => Kind::Workspace,
                (None, None) => {
                    return usage(
                        "usage: reorder pane|tab|workspace [-t TARGET] --next|--previous",
                    );
                }
            };
            let Some(forward) = forward else {
                return usage("reorder needs --next or --previous");
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
                        index = number(value).map(|n| n as usize).ok_or(Usage(format!(
                            "{name} -b: {value:?} is not a buffer number"
                        )))?;
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
            Command::Detach { client }
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
                Some(k) => Some(parse_kind(k).ok_or(Usage(format!(
                    "{name}: {k:?} is not pane, tab or workspace"
                )))?),
                None => None,
            };
            a.no_positional()?;
            let any = target.map(parse_any).transpose()?;
            let kind_of = |any: &Option<AnyRef>, default: Kind| match (kind, any) {
                (Some(k), _) => k,
                (None, Some(AnyRef::Pane(_))) => Kind::Pane,
                (None, Some(AnyRef::Tab(_))) => Kind::Tab,
                (None, Some(AnyRef::Workspace(_))) => Kind::Workspace,
                (None, None) => default,
            };
            match name {
                "command-column" => Command::CommandColumn { client },
                "command-prompt" => Command::CommandPrompt { client },
                "copy-mode" => Command::CopyMode { client },
                "zoom" => Command::Zoom { client },
                "choose-tab" | "choose-workspace" => {
                    let moving_pane = target.map(parse_pane).transpose()?;
                    if name == "choose-tab" {
                        Command::ChooseTab {
                            client,
                            moving: moving_pane,
                            moving_now: moving,
                        }
                    } else {
                        Command::ChooseWorkspace {
                            client,
                            moving: moving_pane,
                            moving_now: moving,
                        }
                    }
                }
                "choose-pane" => Command::ChoosePane {
                    client,
                    target: target.map(parse_pane).transpose()?,
                },
                "menu" => {
                    let Some(kind) =
                        kind.or(any.as_ref().map(|a| kind_of(&Some(a.clone()), Kind::Pane)))
                    else {
                        return usage("usage: menu pane|tab|workspace [-t TARGET]");
                    };
                    Command::Menu {
                        client,
                        kind,
                        target: any,
                    }
                }
                "rename-prompt" => Command::RenamePrompt {
                    client,
                    kind: kind_of(&any, Kind::Pane),
                    target: any,
                },
                "confirm-close" => Command::ConfirmClose {
                    client,
                    kind: kind_of(&any, Kind::Pane),
                    target: any,
                },
                "select-pane" => Command::SelectPane {
                    client,
                    pick: match (pick, direction, target) {
                        (Some("--next"), None, None) => Pick::Next,
                        (Some("--previous"), None, None) => Pick::Previous,
                        (Some("--last"), None, None) => Pick::Last,
                        (None, Some(d), None) => Pick::Toward(d),
                        (None, None, Some(t)) => Pick::Id(parse_pane(t)?),
                        _ => {
                            return usage(
                                "select-pane needs one of -t %N, --next, --previous, --last, -L/-R/-U/-D",
                            );
                        }
                    },
                },
                "select-tab" => Command::SelectTab {
                    client,
                    pick: match (pick, target) {
                        (Some("--next"), None) => Pick::Next,
                        (Some("--previous"), None) => Pick::Previous,
                        (None, Some(t)) => Pick::Id(parse_tab(t)?),
                        _ => return usage("select-tab needs one of -t @N, --next, --previous"),
                    },
                },
                _ => Command::SelectWorkspace {
                    client,
                    pick: match (pick, target) {
                        (Some("--next"), None) => Pick::Next,
                        (Some("--previous"), None) => Pick::Previous,
                        (None, Some(t)) => Pick::Id(parse_workspace(t)?),
                        _ => {
                            return usage(
                                "select-workspace needs one of -t +N, --next, --previous",
                            );
                        }
                    },
                },
            }
        }
        other => {
            return usage(format!(
                "unknown command {other:?}; `fux help` lists commands"
            ));
        }
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
        return usage(format!("{name} takes no command after --"));
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
        parse(&crate::words::split(line).map_err(Usage)?)
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
            Ok(Command::SelectPane {
                client: Some(ClientId(1)),
                pick: Pick::Last
            })
        );
        assert_eq!(
            cmd("menu tab"),
            Ok(Command::Menu {
                client: None,
                kind: Kind::Tab,
                target: None
            })
        );
        assert_eq!(
            cmd("confirm-close -t +1"),
            Ok(Command::ConfirmClose {
                client: None,
                kind: Kind::Workspace,
                target: Some(AnyRef::Workspace(WsRef::Id(WsId(1))))
            })
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
            "send-keys Nope",
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

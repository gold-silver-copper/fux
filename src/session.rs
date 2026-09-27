//! The server's state: workspaces, tabs, panes and the clients' views, and
//! every command that changes them.
use crate::command::{
    self, AnyRef, ClientId, Command, Kind, MoveTo, Pick, SwapWith, TabId, WsId, WsRef,
};
use crate::config::Config;
use crate::json::Json;
use crate::keys::{Direction, KeyPress};
use crate::layout::{self, Axis, Node, PaneId, Placement, Rect};
use crate::pane::Pane;
use crate::process::Pid;
use crate::view::{Mode, View};
use std::collections::{BTreeMap, VecDeque};
use std::ops::Bound;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Tab {
    pub id: TabId,
    pub name: String,
    pub root: Option<Node>,
}

pub struct Workspace {
    pub id: WsId,
    pub name: String,
    pub tabs: Vec<Tab>,
}

/// What the server must do outside the session.
#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// Bytes for a client's terminal outside any paint (OSC 52).
    Bytes(ClientId, Vec<u8>),
    /// Detach a client, telling it why.
    Exit(ClientId, String),
    /// Stop the server.
    Shutdown(String),
}

/// A process being ended: hung up, and killed and reaped at the deadline.
pub struct Dying {
    pub pid: Pid,
    /// Closed before the group is killed.
    pub master: Option<OwnedFd>,
    pub deadline: Instant,
}

/// Where a command comes from.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    /// The client whose key, prompt or menu ran it.
    pub client: Option<ClientId>,
    /// `FUX_PANE` of the CLI's caller.
    pub pane: Option<PaneId>,
    /// The CLI caller's working directory.
    pub cwd: Option<PathBuf>,
}

impl Ctx {
    pub fn client(id: ClientId) -> Ctx {
        Ctx {
            client: Some(id),
            ..Ctx::default()
        }
    }
}

/// Why a command, or attaching a client, failed: exit status 1.
#[derive(Debug)]
pub enum Error {
    // What a command names, or its context gives, that is not there.
    NoWorkspace(WsId),
    NoWorkspaceNamed(String),
    NoPaneGiven,
    NoPane(PaneId),
    NoTabGiven,
    NoTab(TabId),
    NoWorkspaceGiven,
    NoTarget {
        kind: Kind,
        target: AnyRef,
    },
    NoClientGiven,
    NoClient(ClientId),
    /// The client a key, a menu or a prompt came from is gone.
    NoSuchClient,
    NoSuchWorkspace,
    NoWorkspaces,
    NoBuffer(usize),
    NoCopiedText,
    NoConfigFile,
    NoProcess,
    // What the client's view has none of.
    NoCurrentPane,
    NoCurrentTab,
    NoCurrentWorkspace,
    NoLastPane,
    // What changed under a command, or is not where it must be.
    WorkspaceGone,
    TabGone,
    PaneGone,
    DestinationGone,
    NotInTab,
    OtherNotInTab,
    TabEmpty,
    /// A pane, tab or workspace alone of its kind, with nothing to move to.
    OnlyOne(Kind),
    AtEnd(Kind),
    NoBorder {
        pane: PaneId,
        direction: Direction,
    },
    NoNeighbor {
        from: PaneId,
        direction: Direction,
    },
    /// Nothing runs in the pane but its shell.
    OnlyShell(PaneId),
    SelectTabPick,
    SelectWorkspacePick,
    // A name that cannot be given.
    EmptyName,
    ControlInName,
    LongName,
    NameTaken(String),
    /// A counter of IDs, never reused, that would wrap.
    IdsExhausted(&'static str),
    LineTooLong,
    // What failed below the session.
    Usage(command::Usage),
    Words(crate::words::Error),
    Pane(crate::pane::Error),
    Process(crate::process::Error),
    Terminate(fuxix::Errno),
    Copy(crate::copy::Error),
    Config(crate::config::Error),
    Reload(crate::config::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoWorkspace(id) => write!(f, "no workspace {id}"),
            Error::NoWorkspaceNamed(name) => write!(f, "no workspace named {name:?}"),
            Error::NoPaneGiven => f.write_str("no pane given: use -t %N"),
            Error::NoPane(id) => write!(f, "no pane {id}"),
            Error::NoTabGiven => f.write_str("no tab given: use -t @N"),
            Error::NoTab(id) => write!(f, "no tab {id}"),
            Error::NoWorkspaceGiven => f.write_str("no workspace given: use -t +N or a name"),
            Error::NoTarget { kind, target } => {
                write!(f, "no {} {}", kind.name(), describe(target))
            }
            Error::NoClientGiven => f.write_str(
                "this command acts on a client's screen: use -c CLIENT (`fux ls` lists clients)",
            ),
            Error::NoClient(id) => write!(f, "no client {id}"),
            Error::NoSuchClient => f.write_str("no such client"),
            Error::NoSuchWorkspace => f.write_str("no such workspace"),
            Error::NoWorkspaces => f.write_str("the server has no workspace"),
            Error::NoBuffer(index) => write!(f, "no buffer {index}"),
            Error::NoCopiedText => f.write_str("no copied text yet"),
            Error::NoConfigFile => f.write_str("no config file to reload"),
            Error::NoProcess => f.write_str("the pane has no process"),
            Error::NoCurrentPane => f.write_str("no pane"),
            Error::NoCurrentTab => f.write_str("no tab"),
            Error::NoCurrentWorkspace => f.write_str("no workspace"),
            Error::NoLastPane => f.write_str("no previously focused pane"),
            Error::WorkspaceGone => f.write_str("the workspace is gone"),
            Error::TabGone => f.write_str("the tab is gone"),
            Error::PaneGone => f.write_str("the pane is gone"),
            Error::DestinationGone => f.write_str("the destination tab is gone"),
            Error::NotInTab => f.write_str("the pane is in no tab"),
            Error::OtherNotInTab => f.write_str("the other pane is in no tab"),
            Error::TabEmpty => f.write_str("the tab is empty"),
            Error::OnlyOne(kind) => write!(f, "only one {}", kind.name()),
            Error::AtEnd(kind) => write!(f, "the {} is already at that end", kind.name()),
            Error::NoBorder { pane, direction } => {
                write!(f, "{pane} has no border to move {}", direction.name())
            }
            Error::NoNeighbor { from, direction } => {
                write!(f, "no pane {} of {from}", direction.name())
            }
            Error::OnlyShell(pane) => write!(f, "nothing is running in {pane} but its shell"),
            Error::SelectTabPick => f.write_str("select-tab takes -t, --next or --previous"),
            Error::SelectWorkspacePick => {
                f.write_str("select-workspace takes -t, --next or --previous")
            }
            Error::EmptyName => f.write_str("a name cannot be empty"),
            Error::ControlInName => f.write_str("a name cannot contain control characters"),
            Error::LongName => f.write_str("a name is at most 256 bytes"),
            Error::NameTaken(name) => write!(f, "another workspace is named {name:?}"),
            Error::IdsExhausted(what) => write!(f, "no {what} IDs are left"),
            Error::LineTooLong => f.write_str("the command line is too long to type"),
            Error::Usage(error) => error.fmt(f),
            Error::Words(error) => error.fmt(f),
            Error::Pane(error) => error.fmt(f),
            Error::Process(error) => error.fmt(f),
            Error::Terminate(error) => error.fmt(f),
            Error::Copy(error) => error.fmt(f),
            Error::Config(error) => error.fmt(f),
            Error::Reload(error) => write!(f, "{error}; the previous configuration is kept"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Usage(error) => Some(error),
            Error::Words(error) => Some(error),
            Error::Pane(error) => Some(error),
            Error::Process(error) => Some(error),
            Error::Terminate(error) => Some(error),
            Error::Copy(error) => Some(error),
            Error::Config(error) | Error::Reload(error) => Some(error),
            Error::NoWorkspace(_)
            | Error::NoWorkspaceNamed(_)
            | Error::NoPaneGiven
            | Error::NoPane(_)
            | Error::NoTabGiven
            | Error::NoTab(_)
            | Error::NoWorkspaceGiven
            | Error::NoTarget { .. }
            | Error::NoClientGiven
            | Error::NoClient(_)
            | Error::NoSuchClient
            | Error::NoSuchWorkspace
            | Error::NoWorkspaces
            | Error::NoBuffer(_)
            | Error::NoCopiedText
            | Error::NoConfigFile
            | Error::NoProcess
            | Error::NoCurrentPane
            | Error::NoCurrentTab
            | Error::NoCurrentWorkspace
            | Error::NoLastPane
            | Error::WorkspaceGone
            | Error::TabGone
            | Error::PaneGone
            | Error::DestinationGone
            | Error::NotInTab
            | Error::OtherNotInTab
            | Error::TabEmpty
            | Error::OnlyOne(_)
            | Error::AtEnd(_)
            | Error::NoBorder { .. }
            | Error::NoNeighbor { .. }
            | Error::OnlyShell(_)
            | Error::SelectTabPick
            | Error::SelectWorkspacePick
            | Error::EmptyName
            | Error::ControlInName
            | Error::LongName
            | Error::NameTaken(_)
            | Error::IdsExhausted(_)
            | Error::LineTooLong => None,
        }
    }
}

impl From<crate::words::Error> for Error {
    fn from(error: crate::words::Error) -> Error {
        Error::Words(error)
    }
}

impl From<crate::pane::Error> for Error {
    fn from(error: crate::pane::Error) -> Error {
        Error::Pane(error)
    }
}

impl From<crate::process::Error> for Error {
    fn from(error: crate::process::Error) -> Error {
        Error::Process(error)
    }
}

impl From<crate::copy::Error> for Error {
    fn from(error: crate::copy::Error) -> Error {
        Error::Copy(error)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub status: u8,
    pub stdout: String,
    pub stderr: String,
}

/// How long a closing pane's processes have to exit after the hangup.
pub const GRACE: Duration = Duration::from_millis(100);
/// The longest a typed command waits for the new shell to write and then
/// go quiet (`pane::QUIET`): a shell that writes nothing still gets it.
pub const TYPE_WAIT: Duration = Duration::from_secs(1);
/// The size a pane gets when no client shows it yet.
const DEFAULT_SIZE: (u16, u16) = (24, 80);

pub struct Session {
    pub workspaces: Vec<Workspace>,
    pub panes: BTreeMap<PaneId, Pane>,
    pub views: BTreeMap<ClientId, View>,
    pub config: Config,
    pub config_path: Option<PathBuf>,
    /// Why the config file could not be used, until a reload succeeds.
    pub config_error: Option<crate::config::Error>,
    /// Paste buffers, newest first.
    pub buffers: VecDeque<String>,
    pub socket: PathBuf,
    /// Start real processes; tests of the state alone do not.
    pub launch: bool,
    pub outbox: Vec<Outgoing>,
    pub dying: Vec<Dying>,
    /// Where `size_panes` gathers the rectangles each pane is shown in;
    /// reused by every settle.
    shown_sizes: Vec<(PaneId, (u16, u16))>,
    next_pane: u32,
    next_tab: u32,
    next_ws: u32,
    next_client: u32,
}

fn basename(program: &str) -> String {
    Path::new(program)
        .file_name()
        .map_or_else(|| program.to_owned(), |n| n.to_string_lossy().into_owned())
}

impl Session {
    pub fn new(config: Config, socket: PathBuf, launch: bool) -> Session {
        Session {
            workspaces: Vec::new(),
            panes: BTreeMap::new(),
            views: BTreeMap::new(),
            config,
            config_path: None,
            config_error: None,
            buffers: VecDeque::new(),
            socket,
            launch,
            outbox: Vec::new(),
            dying: Vec::new(),
            shown_sizes: Vec::new(),
            next_pane: 1,
            next_tab: 1,
            next_ws: 1,
            next_client: 1,
        }
    }

    /// One workspace, holding one tab with one shell.
    pub fn start(&mut self) -> Result<(), Error> {
        self.create_workspace(Some("main".into()), &[], None)
            .map(|_| ())
    }

    // ---------------------------------------------------------------- lookup

    pub fn ws_index(&self, id: WsId) -> Option<usize> {
        self.workspaces.iter().position(|w| w.id == id)
    }
    pub fn workspace(&self, id: WsId) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == id)
    }
    /// The workspace and tab indexes of a tab.
    pub fn find_tab(&self, id: TabId) -> Option<(usize, usize)> {
        self.workspaces
            .iter()
            .enumerate()
            .find_map(|(w, ws)| ws.tabs.iter().position(|t| t.id == id).map(|t| (w, t)))
    }
    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        let (w, t) = self.find_tab(id)?;
        self.workspaces.get(w)?.tabs.get(t)
    }
    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        let (w, t) = self.find_tab(id)?;
        self.workspaces.get_mut(w)?.tabs.get_mut(t)
    }
    /// A tab's layout, unless it is empty.
    pub fn root(&self, tab: TabId) -> Option<&Node> {
        self.tab(tab)?.root.as_ref()
    }
    fn root_mut(&mut self, tab: TabId) -> Option<&mut Node> {
        self.tab_mut(tab)?.root.as_mut()
    }
    /// The workspace and tab that hold a pane.
    pub fn locate(&self, pane: PaneId) -> Option<(WsId, TabId)> {
        self.workspaces.iter().find_map(|ws| {
            ws.tabs
                .iter()
                .find(|t| t.root.as_ref().is_some_and(|r| r.contains(pane)))
                .map(|t| (ws.id, t.id))
        })
    }
    fn tab_workspace(&self, tab: TabId) -> Option<WsId> {
        let (w, _) = self.find_tab(tab)?;
        self.workspaces.get(w).map(|ws| ws.id)
    }
    pub fn resolve_ws(&self, r: &WsRef) -> Result<WsId, Error> {
        match r {
            WsRef::Id(id) => self
                .workspace(*id)
                .map(|w| w.id)
                .ok_or(Error::NoWorkspace(*id)),
            WsRef::Name(name) => self
                .workspaces
                .iter()
                .find(|w| &w.name == name)
                .map(|w| w.id)
                .ok_or_else(|| Error::NoWorkspaceNamed(name.clone())),
        }
    }
    pub fn tab_panes(&self, tab: TabId) -> Vec<PaneId> {
        self.root(tab).map(Node::panes).unwrap_or_default()
    }
    pub fn exists(&self, what: &AnyRef) -> bool {
        match what {
            AnyRef::Pane(p) => self.panes.contains_key(p),
            AnyRef::Tab(t) => self.find_tab(*t).is_some(),
            AnyRef::Workspace(w) => self.resolve_ws(w).is_ok(),
        }
    }

    // ------------------------------------------------------------- targets

    fn view_of(&self, ctx: &Ctx) -> Option<&View> {
        ctx.client.and_then(|c| self.views.get(&c))
    }

    fn pane_target(&self, explicit: Option<PaneId>, ctx: &Ctx) -> Result<PaneId, Error> {
        let id = explicit
            .or_else(|| self.view_of(ctx).and_then(View::focus))
            .or(ctx.pane)
            .ok_or(Error::NoPaneGiven)?;
        if self.panes.contains_key(&id) {
            Ok(id)
        } else {
            Err(Error::NoPane(id))
        }
    }

    fn tab_target(&self, explicit: Option<TabId>, ctx: &Ctx) -> Result<TabId, Error> {
        let id = explicit
            .or_else(|| self.view_of(ctx).and_then(View::tab))
            .or_else(|| ctx.pane.and_then(|p| self.locate(p)).map(|(_, t)| t))
            .ok_or(Error::NoTabGiven)?;
        self.find_tab(id).map(|_| id).ok_or(Error::NoTab(id))
    }

    fn ws_target(&self, explicit: Option<&WsRef>, ctx: &Ctx) -> Result<WsId, Error> {
        if let Some(r) = explicit {
            return self.resolve_ws(r);
        }
        self.view_of(ctx)
            .map(|v| v.workspace)
            .or_else(|| ctx.pane.and_then(|p| self.locate(p)).map(|(w, _)| w))
            .ok_or(Error::NoWorkspaceGiven)
    }

    fn any_target(
        &self,
        kind: Kind,
        explicit: Option<&AnyRef>,
        ctx: &Ctx,
    ) -> Result<AnyRef, Error> {
        if let Some(target) = explicit {
            return if self.exists(target) {
                Ok(target.clone())
            } else {
                Err(Error::NoTarget {
                    kind,
                    target: target.clone(),
                })
            };
        }
        Ok(match kind {
            Kind::Pane => AnyRef::Pane(self.pane_target(None, ctx)?),
            Kind::Tab => AnyRef::Tab(self.tab_target(None, ctx)?),
            Kind::Workspace => AnyRef::Workspace(WsRef::Id(self.ws_target(None, ctx)?)),
        })
    }

    fn client_target(&self, explicit: Option<ClientId>, ctx: &Ctx) -> Result<ClientId, Error> {
        let id = explicit.or(ctx.client).ok_or(Error::NoClientGiven)?;
        if self.views.contains_key(&id) {
            Ok(id)
        } else {
            Err(Error::NoClient(id))
        }
    }

    fn view_mut(&mut self, id: ClientId) -> Result<&mut View, Error> {
        self.views.get_mut(&id).ok_or(Error::NoClient(id))
    }

    // ------------------------------------------------------------ creation

    fn cwd_for(&self, ctx: &Ctx, near: Option<PaneId>) -> PathBuf {
        if let Some(cwd) = &ctx.cwd {
            return cwd.clone();
        }
        let near = near.or_else(|| self.view_of(ctx).and_then(View::focus));
        if let Some(cwd) = near
            .and_then(|p| self.panes.get(&p))
            .and_then(|p| p.child.as_ref())
            .and_then(|c| crate::process::cwd(c.pid))
        {
            return cwd;
        }
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// A new pane running the shell, with `cmd` typed into it if given.
    fn new_pane(&mut self, cmd: &[String], cwd: &Path, size: (u16, u16)) -> Result<PaneId, Error> {
        let shell_program = self
            .config
            .shell
            .first()
            .cloned()
            .unwrap_or_else(|| "/bin/sh".into());
        let fish = basename(&shell_program) == "fish";
        let line = if cmd.is_empty() {
            None
        } else {
            Some(crate::words::shell_line(cmd, fish)?)
        };
        let id = PaneId(self.next_pane);
        let mut next_pane = self.next_pane;
        advance(&mut next_pane, "pane")?;
        let name = cmd
            .first()
            .map(|c| basename(c))
            .unwrap_or_else(|| basename(&shell_program));
        let mut pane = Pane::new(
            id,
            name,
            shell_program,
            size.0,
            size.1,
            self.config.history_lines,
        )?;
        if self.launch {
            let env = [
                ("FUX_PANE", id.to_string()),
                ("FUX_SOCKET", self.socket.to_string_lossy().into_owned()),
            ];
            pane.child = Some(crate::process::spawn(
                &self.config.shell,
                cwd,
                &env,
                size.0,
                size.1,
            )?);
        }
        if let Some(line) = line {
            let mut typed = line.into_bytes();
            typed.push(b'\r');
            if typed
                .len()
                .checked_add(crate::pane::ENTRY_COST)
                .is_none_or(|cost| cost > crate::pane::INPUT_BYTES)
            {
                return Err(Error::LineTooLong);
            }
            pane.typed = Some(crate::pane::Typed {
                line: typed,
                deadline: crate::after(Instant::now(), TYPE_WAIT),
                last_output: None,
            });
            if pane.child.is_none() {
                pane.type_now();
            }
        }
        self.next_pane = next_pane;
        self.panes.insert(id, pane);
        Ok(id)
    }

    fn new_tab_id(&mut self) -> Result<TabId, Error> {
        advance(&mut self.next_tab, "tab").map(TabId)
    }

    fn create_workspace(
        &mut self,
        name: Option<String>,
        cmd: &[String],
        ctx: Option<&Ctx>,
    ) -> Result<WsId, Error> {
        let default_ctx = Ctx::default();
        let cwd = self.cwd_for(ctx.unwrap_or(&default_ctx), None);
        // The IDs are taken before the pane starts, so that none can run out
        // after, and are committed once it has.
        let (mut next_ws, mut next_tab) = (self.next_ws, self.next_tab);
        let id = WsId(advance(&mut next_ws, "workspace")?);
        let tab = TabId(advance(&mut next_tab, "tab")?);
        let pane = self.new_pane(cmd, &cwd, DEFAULT_SIZE)?;
        (self.next_ws, self.next_tab) = (next_ws, next_tab);
        let name = name.unwrap_or_else(|| format!("workspace-{}", id.0));
        self.workspaces.push(Workspace {
            id,
            name,
            tabs: vec![Tab {
                id: tab,
                name: "main".into(),
                root: Some(Node::Pane(pane)),
            }],
        });
        Ok(id)
    }

    // ------------------------------------------------------------- clients

    /// A client attaches, viewing `workspace` or the first one.
    pub fn attach(
        &mut self,
        rows: u16,
        cols: u16,
        workspace: Option<&str>,
    ) -> Result<ClientId, Error> {
        let ws = match workspace {
            Some(name) => {
                self.resolve_ws(&command::parse_workspace(name).map_err(Error::Usage)?)?
            }
            None => self
                .workspaces
                .first()
                .map(|w| w.id)
                .ok_or(Error::NoWorkspaces)?,
        };
        let id = ClientId(advance(&mut self.next_client, "client")?);
        let mut view = View::new(id, rows.clamp(1, 4096), cols.clamp(1, 4096), ws);
        if let Some(error) = &self.config_error {
            let error = error.to_string();
            // The bar is narrow: the file's name, not its whole path, which
            // the server's log has.
            let shown = match &self.config_path {
                Some(path) => {
                    let name = path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    error.replacen(&path.display().to_string(), &name, 1)
                }
                None => error,
            };
            view.error(format!("config: {shown}"));
        }
        self.views.insert(id, view);
        self.settle();
        Ok(id)
    }

    pub fn detach(&mut self, id: ClientId) {
        self.views.remove(&id);
        self.settle();
    }

    pub fn resize(&mut self, id: ClientId, rows: u16, cols: u16) {
        if let Some(view) = self.views.get_mut(&id) {
            view.rows = rows.clamp(1, 4096);
            view.cols = cols.clamp(1, 4096);
            view.dirty = true;
        }
        self.settle();
    }

    // -------------------------------------------------------------- layout

    /// The area panes share on a client's screen: all but the bar.
    pub fn pane_area(view: &View) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: view.cols,
            h: view.rows.saturating_sub(1),
        }
    }

    /// Where each pane of a view's current tab is on its screen.
    pub fn placement(&self, view: &View) -> Placement {
        let area = Self::pane_area(view);
        let Some(root) = view.tab().and_then(|t| self.root(t)) else {
            return Placement::default();
        };
        if view.zoom
            && let Some(focus) = view.focus()
            && root.contains(focus)
            && area.w > 0
            && area.h > 0
        {
            return Placement {
                panes: vec![(focus, area)],
                separators: Vec::new(),
            };
        }
        layout::place(root, area)
    }

    /// The area to lay out a tab in for a command without a client: a
    /// client showing it, else any client, else a typical terminal.
    fn reference_area(&self, tab: TabId, ctx: &Ctx) -> Rect {
        let view = self
            .view_of(ctx)
            .filter(|v| v.tab() == Some(tab))
            .or_else(|| self.views.values().find(|v| v.tab() == Some(tab)))
            .or_else(|| self.view_of(ctx))
            .or_else(|| self.views.values().next());
        view.map_or(
            Rect {
                x: 0,
                y: 0,
                w: DEFAULT_SIZE.1,
                h: DEFAULT_SIZE.0,
            },
            Self::pane_area,
        )
    }

    // -------------------------------------------------------------- repair

    /// Brings every view back to something that exists, then sizes the PTYs:
    /// run after every event.
    pub fn settle(&mut self) {
        let first_ws = self.workspaces.first().map(|w| w.id);
        // Each client after the one before: repairing a view changes no
        // other, and adds or removes none.
        let mut next = self.views.keys().next().copied();
        while let Some(id) = next {
            self.repair(id, first_ws);
            next = self
                .views
                .range((Bound::Excluded(id), Bound::Unbounded))
                .next()
                .map(|(id, _)| *id);
        }
        self.size_panes();
    }

    fn repair(&mut self, id: ClientId, first_ws: Option<WsId>) {
        let Some(view) = self.views.get(&id) else {
            return;
        };
        let mut workspace = view.workspace;
        if self.workspace(workspace).is_none() {
            let Some(first) = first_ws else { return };
            workspace = first;
        }
        let tabs = self.workspace(workspace).map_or(&[][..], |w| &w.tabs);
        let tab = view
            .tab_of
            .get(&workspace)
            .copied()
            .filter(|t| tabs.iter().any(|tab| tab.id == *t))
            .or_else(|| tabs.first().map(|t| t.id));
        let root = tab.and_then(|t| tabs.iter().find(|tab| tab.id == t)?.root.as_ref());
        let shown = |p: &PaneId| root.is_some_and(|r| r.contains(*p));
        let focus = tab.and_then(|t| {
            view.focus_of
                .get(&t)
                .copied()
                .filter(shown)
                .or_else(|| view.last_of.get(&t).copied().filter(shown))
                .or_else(|| root.and_then(Node::first_pane))
        });
        // What the view's mode refers to must still exist.
        let gone: Option<String> = match &view.mode {
            Mode::Copy(copy) => {
                if !self.panes.contains_key(&copy.pane) {
                    Some("copy mode ended: its pane closed".into())
                } else if Some(copy.pane) != focus {
                    Some("copy mode ended: its pane is no longer focused".into())
                } else {
                    self.panes
                        .get(&copy.pane)
                        .and_then(|p| copy.check(p.screen()).err())
                        .map(|e| e.to_string())
                }
            }
            Mode::List(list) => list
                .about
                .as_ref()
                .filter(|a| !self.exists(a))
                .map(|a| format!("closed: {} is gone", describe(a))),
            Mode::Confirm(confirm) => (!self.exists(&confirm.about))
                .then(|| format!("closed: {} is gone", describe(&confirm.about))),
            Mode::Prompt(prompt) => match &prompt.purpose {
                crate::view::PromptFor::Rename(target) if !self.exists(target) => {
                    Some(format!("closed: {} is gone", describe(target)))
                }
                crate::view::PromptFor::Command | crate::view::PromptFor::Rename(_) => None,
            },
            // Bindings change under a client from the command line.
            Mode::Column { path, .. } if !path.is_empty() && !self.is_layer(path) => Some(format!(
                "closed: the layer {} is gone",
                self.keys_named(path)
            )),
            Mode::Repeat { path } if !self.repeats(path) => Some(format!(
                "closed: the repeat mode {} is gone",
                self.keys_named(path)
            )),
            Mode::Normal | Mode::Column { .. } | Mode::Repeat { .. } => None,
        };
        // A column shorter than it was keeps its selection within it.
        let last = if let Mode::Column { path, .. } = &view.mode {
            Some(crate::overlay::column_len(self, path).saturating_sub(1))
        } else {
            None
        };
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        if view.workspace != workspace {
            view.workspace = workspace;
            view.dirty = true;
        }
        if let Some(tab) = tab {
            if view.tab_of.get(&workspace) != Some(&tab) {
                view.tab_of.insert(workspace, tab);
                view.dirty = true;
            }
            match focus {
                Some(focus) if view.focus_of.get(&tab) != Some(&focus) => {
                    view.focus_of.insert(tab, focus);
                    view.dirty = true;
                }
                None => {
                    view.focus_of.remove(&tab);
                }
                _ => {}
            }
        }
        if let (Mode::Column { selected, .. }, Some(last)) = (&mut view.mode, last)
            && *selected > last
        {
            *selected = last;
            view.dirty = true;
        }
        if let Some(reason) = gone {
            view.mode = Mode::Normal;
            view.error(reason);
        }
    }

    /// Whether `path` is a layer: some binding's keys go on past it.
    fn is_layer(&self, path: &[KeyPress]) -> bool {
        self.config
            .bindings
            .iter()
            .any(|b| b.keys.len() > path.len() && b.keys.starts_with(path))
    }

    /// Whether the layer at `path` holds a repeating binding.
    fn repeats(&self, path: &[KeyPress]) -> bool {
        self.config.bindings.iter().any(|b| {
            b.repeat && b.keys.len() == path.len().saturating_add(1) && b.keys.starts_with(path)
        })
    }

    /// Keys after the prefix as they are typed: `C-b t`.
    fn keys_named(&self, path: &[KeyPress]) -> String {
        format!("{} {}", self.config.prefix, crate::config::keys_text(path))
    }

    /// A PTY is the smallest rectangle any client shows it in; a pane nobody
    /// shows keeps its size.
    fn size_panes(&mut self) {
        let mut sizes = std::mem::take(&mut self.shown_sizes);
        sizes.clear();
        for view in self.views.values() {
            let placement = self.placement(view);
            sizes.extend(placement.panes.iter().map(|(p, r)| (*p, (r.h, r.w))));
        }
        // A pane's rectangles side by side, for the smallest.
        sizes.sort_unstable_by_key(|(pane, _)| *pane);
        let mut resized = false;
        for shown in sizes.chunk_by(|a, b| a.0 == b.0) {
            let Some(&(id, first)) = shown.first() else {
                continue;
            };
            let (rows, cols) = shown.iter().fold(first, |(rows, cols), (_, (h, w))| {
                (rows.min(*h), cols.min(*w))
            });
            if let Some(pane) = self.panes.get_mut(&id)
                && pane.size != (rows.max(1), cols.max(1))
            {
                pane.resize(rows, cols);
                resized = true;
            }
        }
        self.shown_sizes = sizes;
        if resized {
            self.touch();
        }
    }

    pub fn touch(&mut self) {
        for view in self.views.values_mut() {
            view.dirty = true;
        }
    }

    // ------------------------------------------------------------- closing

    /// Hangs up a pane's process; it is killed and reaped after `GRACE`.
    fn end(&mut self, pane: Pane) {
        if let Some(child) = pane.child {
            crate::process::hangup(child.pid);
            self.dying.push(Dying {
                pid: child.pid,
                master: Some(child.master),
                deadline: crate::after(Instant::now(), GRACE),
            });
        }
    }

    /// Removes a pane and its process. A tab its closing empties closes too,
    /// as does a workspace left with no tabs; a tab emptied by moving its
    /// panes out stays.
    pub fn close_pane(&mut self, id: PaneId, why: Option<String>) {
        let place = self.locate(id);
        if let Some((_, tab)) = place
            && let Some(t) = self.tab_mut(tab)
        {
            layout::remove(&mut t.root, id);
            if t.root.is_none() {
                self.remove_tab(tab);
            }
        }
        if let Some(pane) = self.panes.remove(&id) {
            if let Some(why) = why {
                for view in self.views.values_mut() {
                    if place.is_some_and(|(_, t)| view.tab() == Some(t)) {
                        view.info(format!("{id} {} {why}", pane.label()));
                    }
                }
            }
            self.end(pane);
        }
        self.after_close();
    }

    /// Removes a tab and whatever panes are in it.
    fn remove_tab(&mut self, tab: TabId) {
        let Some((w, t)) = self.find_tab(tab) else {
            return;
        };
        let Some(ws) = self.workspaces.get_mut(w) else {
            return;
        };
        // Its panes go with it.
        let root = ws.tabs.get_mut(t).and_then(|tab| tab.root.take());
        let ws_id = ws.id;
        // Tab IDs are unique: this removes the tab at `t`.
        ws.tabs.retain(|other| other.id != tab);
        // Views on the closed tab select its neighbour.
        let neighbour = ws
            .tabs
            .get(t)
            .or_else(|| t.checked_sub(1).and_then(|i| ws.tabs.get(i)))
            .map(|t| t.id);
        let empty = ws.tabs.is_empty();
        for view in self.views.values_mut() {
            if view.tab_of.get(&ws_id) == Some(&tab) {
                match neighbour {
                    Some(n) => {
                        view.tab_of.insert(ws_id, n);
                    }
                    None => {
                        view.tab_of.remove(&ws_id);
                    }
                }
            }
        }
        if empty {
            self.workspaces.retain(|w| w.id != ws_id);
        }
        if let Some(root) = root {
            root.for_each_pane(&mut |pane| {
                if let Some(pane) = self.panes.remove(&pane) {
                    self.end(pane);
                }
            });
        }
    }

    fn remove_workspace(&mut self, ws: WsId) {
        let tabs: Vec<TabId> = self
            .workspace(ws)
            .map(|w| w.tabs.iter().map(|t| t.id).collect())
            .unwrap_or_default();
        for tab in tabs {
            self.remove_tab(tab);
        }
        self.workspaces.retain(|w| w.id != ws);
    }

    fn after_close(&mut self) {
        if self.panes.is_empty()
            && !self
                .outbox
                .iter()
                .any(|o| matches!(o, Outgoing::Shutdown(_)))
        {
            self.outbox
                .push(Outgoing::Shutdown("the last pane closed".into()));
        }
        self.touch();
        self.settle();
    }

    /// A pane's program exited with `status`.
    pub fn exited(&mut self, id: PaneId, status: i32) {
        self.close_pane(id, Some(format!("exited with status {status}")));
    }

    /// Output from a pane's program.
    pub fn output(&mut self, id: PaneId, bytes: &[u8]) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let dropped = pane.output(bytes);
        let place = self.locate(id);
        for view in self.views.values_mut() {
            if place.is_some_and(|(_, t)| view.tab() == Some(t)) {
                view.dirty = true;
                if dropped {
                    view.error(format!(
                        "{id} is not reading its input; a terminal reply was dropped"
                    ));
                }
            }
        }
    }

    /// Types held command lines whose wait is over; the next moment one is
    /// due.
    pub fn type_due(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        for pane in self.panes.values_mut() {
            let Some(at) = pane.typed.as_ref().map(crate::pane::Typed::due_at) else {
                continue;
            };
            if at <= now {
                pane.type_now();
            } else {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        }
        next
    }

    /// The next moment a held command line is due, for the poll timeout.
    pub fn next_typing(&self) -> Option<Instant> {
        self.panes
            .values()
            .filter_map(|p| p.typed.as_ref().map(crate::pane::Typed::due_at))
            .min()
    }

    /// Everything the server shuts down: every pane is hung up.
    pub fn shutdown(&mut self) {
        let panes: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in panes {
            if let Some(pane) = self.panes.remove(&id) {
                self.end(pane);
            }
        }
    }

    // ------------------------------------------------------------ commands

    /// Runs a command line from the CLI or the prompt.
    pub fn run(&mut self, argv: &[String], ctx: &Ctx) -> Outcome {
        match command::parse(argv) {
            Ok(command) => self.run_command(&command, ctx),
            Err(usage) => {
                self.touch();
                self.settle();
                Outcome {
                    status: 2,
                    stdout: String::new(),
                    stderr: usage.to_string(),
                }
            }
        }
    }

    /// Runs a command, parsed already: from a line, a binding or a menu.
    pub fn run_command(&mut self, command: &Command, ctx: &Ctx) -> Outcome {
        let outcome = match self.execute(command, ctx) {
            Ok(stdout) => Outcome {
                status: 0,
                stdout,
                stderr: String::new(),
            },
            Err(error) => Outcome {
                status: 1,
                stdout: String::new(),
                stderr: error.to_string(),
            },
        };
        self.touch();
        self.settle();
        outcome
    }

    /// Why a command cannot run now, if it cannot: menus and the command
    /// column dim such entries, and running one says why.
    pub fn unavailable(&self, command: &Command, ctx: &Ctx) -> Option<Error> {
        let view = self.view_of(ctx);
        // Going to the next or previous one needs another.
        let alone = |kind, count: usize| (count < 2).then_some(Error::OnlyOne(kind));
        if matches!(
            command,
            Command::SelectPane {
                pick: Pick::Next | Pick::Previous | Pick::Last,
                ..
            } | Command::ChoosePane { .. }
        ) {
            let mut panes = 0usize;
            if let Some(root) = view.and_then(View::tab).and_then(|t| self.root(t)) {
                root.for_each_pane(&mut |_| panes = panes.saturating_add(1));
            }
            return alone(Kind::Pane, panes);
        }
        if let Command::SelectTab {
            pick: Pick::Next | Pick::Previous,
            ..
        } = command
        {
            let tabs = view.and_then(|v| self.workspace(v.workspace));
            return alone(Kind::Tab, tabs.map_or(0, |w| w.tabs.len()));
        }
        if let Command::SelectWorkspace {
            pick: Pick::Next | Pick::Previous,
            ..
        } = command
        {
            return alone(Kind::Workspace, self.workspaces.len());
        }
        if let Command::PasteBuffer { index, .. } = command {
            return self
                .buffers
                .get(*index)
                .is_none()
                .then_some(Error::NoCopiedText);
        }
        if let Command::Terminate { target } = command {
            return match self.pane_target(*target, ctx) {
                Ok(pane) => {
                    let idle = self.panes.get(&pane).is_none_or(Pane::idle);
                    idle.then_some(Error::OnlyShell(pane))
                }
                Err(error) => Some(error),
            };
        }
        if let Command::KillPane { target }
        | Command::SwapPane { target, .. }
        | Command::MovePane { target, .. } = command
        {
            return self.pane_target(*target, ctx).err();
        }
        None
    }

    fn execute(&mut self, command: &Command, ctx: &Ctx) -> Result<String, Error> {
        match command {
            &Command::Ls { json } => Ok(if json { self.ls_json() } else { self.ls_text() }),
            &Command::KillServer => {
                self.outbox
                    .push(Outgoing::Shutdown("stopped by fux kill-server".into()));
                Ok(String::new())
            }
            &Command::ListKeys => {
                let mut out = String::from(
                    "Keys, for send-keys and the prefix (with C-, M-, S- prefixes, or any character):\n",
                );
                out.push_str(&crate::keys::all_names().join(" "));
                out.push_str("\n\nBindings (after the prefix, ");
                out.push_str(&self.config.prefix.to_string());
                out.push_str("; each key a letter, in either case):\n");
                for binding in &self.config.bindings {
                    out.push_str(&format!(
                        "{:>8}  {}{}\n",
                        crate::config::keys_text(&binding.keys),
                        crate::words::join(&binding.command),
                        if binding.repeat { " (repeats)" } else { "" }
                    ));
                }
                Ok(out)
            }
            Command::NewWorkspace { name, cmd } => {
                if let Some(name) = &name {
                    self.check_name(name)?;
                }
                let ws = self.create_workspace(name.clone(), cmd, Some(ctx))?;
                if let Some(view) = ctx.client.and_then(|c| self.views.get_mut(&c)) {
                    view.workspace = ws;
                    view.zoom = false;
                }
                Ok(format!("{ws}\n"))
            }
            Command::NewTab { target, name, cmd } => {
                let ws = self.ws_target(target.as_ref(), ctx)?;
                if let Some(name) = &name {
                    self.check_name(name)?;
                }
                let cwd = self.cwd_for(ctx, None);
                // As for a workspace: the ID first, committed after the pane.
                let mut next_tab = self.next_tab;
                let id = TabId(advance(&mut next_tab, "tab")?);
                let pane = self.new_pane(cmd, &cwd, DEFAULT_SIZE)?;
                self.next_tab = next_tab;
                let index = self.ws_index(ws).ok_or(Error::WorkspaceGone)?;
                let Some(workspace) = self.workspaces.get_mut(index) else {
                    return Err(Error::WorkspaceGone);
                };
                let number = workspace.tabs.len().saturating_add(1);
                let name = name.clone().unwrap_or_else(|| format!("tab-{number}"));
                workspace.tabs.push(Tab {
                    id,
                    name,
                    root: Some(Node::Pane(pane)),
                });
                if let Some(view) = ctx.client.and_then(|c| self.views.get_mut(&c)) {
                    view.workspace = ws;
                    view.tab_of.insert(ws, id);
                    view.zoom = false;
                }
                Ok(format!("{id}\n"))
            }
            &Command::Split {
                axis,
                target,
                ref cmd,
            } => {
                let target = self.pane_target(target, ctx)?;
                let (_, tab) = self.locate(target).ok_or(Error::NotInTab)?;
                let size = self.panes.get(&target).map_or(DEFAULT_SIZE, |p| p.size);
                let cwd = self.cwd_for(ctx, Some(target));
                let pane = self.new_pane(cmd, &cwd, size)?;
                if let Some(t) = self.tab_mut(tab) {
                    layout::split(&mut t.root, target, pane, axis, true);
                }
                // The splitting client follows the new pane; from the CLI,
                // clients focused on the split pane do.
                for view in self.views.values_mut() {
                    let follows = match ctx.client {
                        Some(client) => view.id == client,
                        None => view.focus_of.get(&tab) == Some(&target),
                    };
                    if follows {
                        view.set_focus(tab, pane);
                        view.zoom = false;
                    }
                }
                Ok(format!("{pane}\n"))
            }
            &Command::KillPane { target } => {
                let pane = self.pane_target(target, ctx)?;
                self.close_pane(pane, None);
                Ok(String::new())
            }
            &Command::KillTab { target } => {
                let tab = self.tab_target(target, ctx)?;
                self.remove_tab(tab);
                self.after_close();
                Ok(String::new())
            }
            Command::KillWorkspace { target } => {
                let ws = self.ws_target(target.as_ref(), ctx)?;
                self.remove_workspace(ws);
                self.after_close();
                Ok(String::new())
            }
            Command::Rename { target, name } => {
                self.rename(target, name.clone()).map(|()| String::new())
            }
            &Command::MovePane { target, ref to } => self.move_pane(target, to, ctx),
            &Command::SwapPane { target, with } => {
                let source = self.pane_target(target, ctx)?;
                let other = match with {
                    SwapWith::Pane(p) => {
                        if !self.panes.contains_key(&p) {
                            return Err(Error::NoPane(p));
                        }
                        p
                    }
                    SwapWith::Toward(direction) => self.neighbor(source, direction, ctx)?,
                };
                self.swap(source, other)?;
                Ok(String::new())
            }
            &Command::ResizePane {
                target,
                direction,
                amount,
            } => {
                let pane = self.pane_target(target, ctx)?;
                let (_, tab) = self.locate(pane).ok_or(Error::NotInTab)?;
                let area = self.reference_area(tab, ctx);
                let Some(root) = self.root_mut(tab) else {
                    return Err(Error::TabEmpty);
                };
                if layout::resize(root, area, pane, direction, amount) {
                    Ok(String::new())
                } else {
                    Err(Error::NoBorder { pane, direction })
                }
            }
            &Command::SendKeys {
                target,
                literal,
                ref keys,
            } => {
                let pane = self.pane_target(target, ctx)?;
                let Some(p) = self.panes.get_mut(&pane) else {
                    return Err(Error::NoPane(pane));
                };
                // Each argument is a key name (`C-c`, `Enter`, `a`), or, as in
                // tmux, text sent as it is; `-l` makes every argument text.
                let mut bytes = Vec::new();
                let application = p.screen().application_cursor();
                for key in keys {
                    match key.parse::<KeyPress>() {
                        Ok(press) if !literal => {
                            bytes.extend(crate::encode::key_bytes(press, application))
                        }
                        _ => bytes.extend_from_slice(key.as_bytes()),
                    }
                }
                p.input.push(bytes)?;
                Ok(String::new())
            }
            &Command::CapturePane {
                target,
                history,
                json,
            } => {
                let pane = self.pane_target(target, ctx)?;
                let p = self.panes.get(&pane).ok_or(Error::NoPane(pane))?;
                Ok(capture(p, history, json))
            }
            &Command::CaptureClient { client, json } => {
                let client = self.client_target(client, ctx)?;
                let grid = crate::render::compose(self, client).ok_or(Error::NoClient(client))?;
                Ok(capture_client(client, &grid, json))
            }
            &Command::Terminate { target } => {
                let pane = self.pane_target(target, ctx)?;
                let p = self.panes.get(&pane).ok_or(Error::NoPane(pane))?;
                let child = p.child.as_ref().ok_or(Error::NoProcess)?;
                match crate::process::foreground(&child.master) {
                    Some(group) if group != child.pid => {
                        crate::process::terminate(group).map_err(Error::Terminate)?;
                        Ok(String::new())
                    }
                    _ => Err(Error::OnlyShell(pane)),
                }
            }
            &Command::Reorder {
                kind,
                ref target,
                forward,
            } => {
                let target = self.any_target(kind, target.as_ref(), ctx)?;
                self.reorder(&target, forward).map(|()| String::new())
            }
            Command::Set { argv } | Command::Bind { argv } | Command::Unbind { argv } => self
                .config
                .apply(argv)
                .map(|()| String::new())
                .map_err(Error::Config),
            &Command::UnbindAll => {
                self.config.bindings.clear();
                Ok(String::new())
            }
            &Command::Reload => {
                let path = self.config_path.clone().ok_or(Error::NoConfigFile)?;
                match Config::from_file(&path) {
                    Ok(config) => {
                        self.config = config;
                        self.config_error = None;
                        Ok(format!("reloaded {}\n", path.display()))
                    }
                    Err(error) => Err(Error::Reload(error)),
                }
            }
            &Command::ListBuffers => Ok(self
                .buffers
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    let preview: String = text
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .take(50)
                        .collect();
                    format!("{i}: {} bytes: {preview}\n", text.len())
                })
                .collect()),
            &Command::ShowBuffer { index } => self
                .buffers
                .get(index)
                .cloned()
                .ok_or(Error::NoBuffer(index)),
            &Command::PasteBuffer { index, target } => {
                let text = self
                    .buffers
                    .get(index)
                    .cloned()
                    .ok_or(Error::NoBuffer(index))?;
                let pane = self.pane_target(target, ctx)?;
                let p = self.panes.get_mut(&pane).ok_or(Error::NoPane(pane))?;
                let bracketed = p.screen().bracketed_paste();
                p.input.push(crate::encode::paste(&text, bracketed))?;
                Ok(String::new())
            }
            &Command::Detach { client } => {
                let client = self.client_target(client, ctx)?;
                self.views.remove(&client);
                self.outbox.push(Outgoing::Exit(client, "detached".into()));
                Ok(String::new())
            }
            &Command::Zoom { client } => {
                let client = self.client_target(client, ctx)?;
                let view = self.view_mut(client)?;
                view.zoom = !view.zoom;
                Ok(String::new())
            }
            &Command::SelectPane { client, pick } => {
                let client = self.client_target(client, ctx)?;
                self.select_pane(client, pick)
            }
            &Command::SelectTab { client, pick } => {
                let client = self.client_target(client, ctx)?;
                self.select_tab(client, pick)
            }
            &Command::SelectWorkspace { client, ref pick } => {
                let client = self.client_target(client, ctx)?;
                self.select_workspace(client, pick)
            }
            &Command::CommandColumn { client } => {
                let client = self.client_target(client, ctx)?;
                self.view_mut(client)?.mode = Mode::Column {
                    path: Vec::new(),
                    selected: 0,
                };
                Ok(String::new())
            }
            &Command::CommandPrompt { client } => {
                let client = self.client_target(client, ctx)?;
                crate::overlay::open_prompt(
                    self,
                    client,
                    crate::view::PromptFor::Command,
                    ":".into(),
                    String::new(),
                )
            }
            &Command::RenamePrompt {
                client,
                kind,
                ref target,
            } => {
                let client = self.client_target(client, ctx)?;
                let ctx = Ctx::client(client);
                let target = self.any_target(kind, target.as_ref(), &ctx)?;
                let current = self.name_of(&target);
                crate::overlay::open_prompt(
                    self,
                    client,
                    crate::view::PromptFor::Rename(target.clone()),
                    format!("rename {} {}", kind.name(), describe(&target)),
                    current,
                )
            }
            &Command::ConfirmClose {
                client,
                kind,
                ref target,
            } => {
                let client = self.client_target(client, ctx)?;
                let ctx = Ctx::client(client);
                let target = self.any_target(kind, target.as_ref(), &ctx)?;
                crate::overlay::open_confirm(self, client, target)
            }
            &Command::Menu {
                client,
                kind,
                ref target,
            } => {
                let client = self.client_target(client, ctx)?;
                let ctx = Ctx::client(client);
                let target = self.any_target(kind, target.as_ref(), &ctx)?;
                crate::overlay::open_menu(self, client, target)
            }
            &Command::ChooseTab {
                client,
                moving,
                moving_now,
            } => {
                let client = self.client_target(client, ctx)?;
                let moving = self.moving(client, moving, moving_now)?;
                crate::overlay::open_tab_chooser(self, client, moving)
            }
            &Command::ChooseWorkspace {
                client,
                moving,
                moving_now,
            } => {
                let client = self.client_target(client, ctx)?;
                let moving = self.moving(client, moving, moving_now)?;
                crate::overlay::open_workspace_chooser(self, client, moving)
            }
            &Command::ChoosePane { client, target } => {
                let client = self.client_target(client, ctx)?;
                let ctx = Ctx::client(client);
                let source = self.pane_target(target, &ctx)?;
                crate::overlay::open_pane_chooser(self, client, source)
            }
            &Command::CopyMode { client } => {
                let client = self.client_target(client, ctx)?;
                Ok(crate::copy::enter(self, client)?)
            }
        }
    }

    fn moving(
        &self,
        client: ClientId,
        moving: Option<PaneId>,
        now: bool,
    ) -> Result<Option<PaneId>, Error> {
        match (moving, now) {
            (Some(p), _) => self.pane_target(Some(p), &Ctx::default()).map(Some),
            (None, true) => self.pane_target(None, &Ctx::client(client)).map(Some),
            (None, false) => Ok(None),
        }
    }

    fn check_name(&self, name: &str) -> Result<(), Error> {
        if name.is_empty() {
            return Err(Error::EmptyName);
        }
        if name.chars().any(char::is_control) {
            return Err(Error::ControlInName);
        }
        if name.len() > 256 {
            return Err(Error::LongName);
        }
        Ok(())
    }

    pub fn name_of(&self, target: &AnyRef) -> String {
        match target {
            AnyRef::Pane(p) => self.panes.get(p).map(|p| p.name.clone()),
            AnyRef::Tab(t) => self.tab(*t).map(|t| t.name.clone()),
            AnyRef::Workspace(w) => self
                .resolve_ws(w)
                .ok()
                .and_then(|w| self.workspace(w))
                .map(|w| w.name.clone()),
        }
        .unwrap_or_default()
    }

    fn rename(&mut self, target: &AnyRef, name: String) -> Result<(), Error> {
        self.check_name(&name)?;
        match target {
            AnyRef::Pane(p) => {
                self.panes.get_mut(p).ok_or(Error::NoPane(*p))?.name = name;
            }
            AnyRef::Tab(t) => {
                self.tab_mut(*t).ok_or(Error::NoTab(*t))?.name = name;
            }
            AnyRef::Workspace(w) => {
                let id = self.resolve_ws(w)?;
                if let WsRef::Name(_) = w {
                    // Renaming by name is fine; a clash is refused.
                }
                if self
                    .workspaces
                    .iter()
                    .any(|ws| ws.name == name && ws.id != id)
                {
                    return Err(Error::NameTaken(name));
                }
                let index = self.ws_index(id).ok_or(Error::NoSuchWorkspace)?;
                if let Some(ws) = self.workspaces.get_mut(index) {
                    ws.name = name;
                }
            }
        }
        Ok(())
    }

    fn neighbor(&self, from: PaneId, direction: Direction, ctx: &Ctx) -> Result<PaneId, Error> {
        let (_, tab) = self.locate(from).ok_or(Error::NotInTab)?;
        let area = self.reference_area(tab, ctx);
        let root = self.root(tab).ok_or(Error::TabEmpty)?;
        let placement = layout::place(root, area);
        layout::neighbor(&placement, from, direction).ok_or(Error::NoNeighbor { from, direction })
    }

    fn swap(&mut self, a: PaneId, b: PaneId) -> Result<(), Error> {
        if a == b {
            return Ok(());
        }
        let (_, ta) = self.locate(a).ok_or(Error::NotInTab)?;
        let (_, tb) = self.locate(b).ok_or(Error::OtherNotInTab)?;
        if ta == tb {
            if let Some(root) = self.root_mut(ta) {
                layout::swap(root, a, b);
            }
        } else {
            // In different trees, each takes the other's place.
            if let Some(root) = self.root_mut(ta) {
                root.replace(a, b);
            }
            if let Some(root) = self.root_mut(tb) {
                root.replace(b, a);
            }
        }
        Ok(())
    }

    fn move_pane(
        &mut self,
        target: Option<PaneId>,
        to: &MoveTo,
        ctx: &Ctx,
    ) -> Result<String, Error> {
        let pane = self.pane_target(target, ctx)?;
        let (source_ws, source_tab) = self.locate(pane).ok_or(Error::NotInTab)?;
        if let &MoveTo::Beside(direction) = to {
            let destination = self.neighbor(pane, direction, ctx)?;
            let tab = self.tab_mut(source_tab).ok_or(Error::TabGone)?;
            layout::remove(&mut tab.root, pane);
            let after = matches!(direction, Direction::Right | Direction::Down);
            layout::split(&mut tab.root, destination, pane, Axis::of(direction), after);
            return Ok(String::new());
        }
        let (ws, tab) = match to {
            &MoveTo::Tab(tab) => {
                let ws = self.tab_workspace(tab).ok_or(Error::NoTab(tab))?;
                (ws, tab)
            }
            MoveTo::Workspace(r) => {
                let ws = self.resolve_ws(r)?;
                let first = self
                    .workspace(ws)
                    .and_then(|w| w.tabs.first())
                    .map(|t| t.id);
                let tab = match first {
                    Some(tab) => tab,
                    None => {
                        let id = self.new_tab_id()?;
                        let index = self.ws_index(ws).ok_or(Error::WorkspaceGone)?;
                        if let Some(w) = self.workspaces.get_mut(index) {
                            w.tabs.push(Tab {
                                id,
                                name: "main".into(),
                                root: None,
                            });
                        }
                        id
                    }
                };
                (ws, tab)
            }
            MoveTo::NewTab => {
                let id = self.new_tab_id()?;
                let index = self.ws_index(source_ws).ok_or(Error::WorkspaceGone)?;
                if let Some(w) = self.workspaces.get_mut(index) {
                    let name = format!("tab-{}", w.tabs.len().saturating_add(1));
                    w.tabs.push(Tab {
                        id,
                        name,
                        root: None,
                    });
                }
                (source_ws, id)
            }
            MoveTo::NewWorkspace => {
                let id = WsId(advance(&mut self.next_ws, "workspace")?);
                let tab = self.new_tab_id()?;
                self.workspaces.push(Workspace {
                    id,
                    name: format!("workspace-{}", id.0),
                    tabs: vec![Tab {
                        id: tab,
                        name: "main".into(),
                        root: None,
                    }],
                });
                (id, tab)
            }
            MoveTo::Beside(_) => return Ok(String::new()),
        };
        if tab == source_tab {
            return Ok(String::new());
        }
        if let Some(t) = self.tab_mut(source_tab) {
            layout::remove(&mut t.root, pane);
        }
        let destination = self.tab_mut(tab).ok_or(Error::DestinationGone)?;
        destination.root = Some(match destination.root.take() {
            None => Node::Pane(pane),
            Some(root) => {
                let mut node = Node::Split {
                    axis: Axis::Horizontal,
                    children: vec![(layout::WEIGHT, root), (layout::WEIGHT, Node::Pane(pane))],
                };
                layout::normalize(&mut node);
                node
            }
        });
        // The moving client follows its pane.
        if let Some(view) = ctx.client.and_then(|c| self.views.get_mut(&c)) {
            view.workspace = ws;
            view.tab_of.insert(ws, tab);
            view.set_focus(tab, pane);
            view.zoom = false;
        }
        Ok(format!("{tab}\n"))
    }

    fn reorder(&mut self, target: &AnyRef, forward: bool) -> Result<(), Error> {
        let step = |index: usize, len: usize| -> Option<usize> {
            if forward {
                index.checked_add(1).filter(|next| *next < len)
            } else {
                index.checked_sub(1)
            }
        };
        match target {
            AnyRef::Pane(p) => {
                let (_, tab) = self.locate(*p).ok_or(Error::NotInTab)?;
                let panes = self.tab_panes(tab);
                let index = panes.iter().position(|x| x == p).ok_or(Error::PaneGone)?;
                let other = step(index, panes.len())
                    .and_then(|i| panes.get(i))
                    .copied()
                    .ok_or(Error::AtEnd(Kind::Pane))?;
                self.swap(*p, other)
            }
            AnyRef::Tab(t) => {
                let (w, index) = self.find_tab(*t).ok_or(Error::NoTab(*t))?;
                let ws = self.workspaces.get_mut(w).ok_or(Error::WorkspaceGone)?;
                let other = step(index, ws.tabs.len()).ok_or(Error::AtEnd(Kind::Tab))?;
                let [a, b] = ws
                    .tabs
                    .get_disjoint_mut([index, other])
                    .map_err(|_| Error::TabGone)?;
                std::mem::swap(a, b);
                Ok(())
            }
            AnyRef::Workspace(r) => {
                let id = self.resolve_ws(r)?;
                let index = self.ws_index(id).ok_or(Error::WorkspaceGone)?;
                let other =
                    step(index, self.workspaces.len()).ok_or(Error::AtEnd(Kind::Workspace))?;
                let [a, b] = self
                    .workspaces
                    .get_disjoint_mut([index, other])
                    .map_err(|_| Error::WorkspaceGone)?;
                std::mem::swap(a, b);
                Ok(())
            }
        }
    }

    fn select_pane(&mut self, client: ClientId, pick: Pick<PaneId>) -> Result<String, Error> {
        let view = self.views.get(&client).ok_or(Error::NoSuchClient)?;
        let tab = view.tab().ok_or(Error::NoCurrentTab)?;
        let panes = self.tab_panes(tab);
        let current = view.focus();
        let target = match pick {
            Pick::Id(p) => {
                let (ws, tab) = self.locate(p).ok_or(Error::NoPane(p))?;
                let view = self.view_mut(client)?;
                view.workspace = ws;
                view.tab_of.insert(ws, tab);
                view.set_focus(tab, p);
                view.zoom = false;
                return Ok(String::new());
            }
            Pick::Next | Pick::Previous => {
                if panes.len() < 2 {
                    return Err(Error::OnlyOne(Kind::Pane));
                }
                let index = current
                    .and_then(|c| panes.iter().position(|p| *p == c))
                    .unwrap_or(0);
                let next = round(index, panes.len(), matches!(pick, Pick::Next));
                panes.get(next).copied().ok_or(Error::NoCurrentPane)?
            }
            Pick::Last => view
                .last_of
                .get(&tab)
                .copied()
                .filter(|p| panes.contains(p) && Some(*p) != current)
                .ok_or(Error::NoLastPane)?,
            Pick::Toward(direction) => {
                let placement = layout::place(
                    self.root(tab).ok_or(Error::TabEmpty)?,
                    Self::pane_area(view),
                );
                let from = current.ok_or(Error::NoCurrentPane)?;
                layout::neighbor(&placement, from, direction)
                    .ok_or(Error::NoNeighbor { from, direction })?
            }
        };
        let view = self.view_mut(client)?;
        view.set_focus(tab, target);
        view.zoom = false;
        Ok(String::new())
    }

    fn select_tab(&mut self, client: ClientId, pick: Pick<TabId>) -> Result<String, Error> {
        let view = self.views.get(&client).ok_or(Error::NoSuchClient)?;
        let ws = view.workspace;
        let tabs: Vec<TabId> = self
            .workspace(ws)
            .map(|w| w.tabs.iter().map(|t| t.id).collect())
            .unwrap_or_default();
        let target_ws;
        let target = match pick {
            Pick::Id(t) => {
                target_ws = self.tab_workspace(t).ok_or(Error::NoTab(t))?;
                t
            }
            Pick::Next | Pick::Previous => {
                if tabs.len() < 2 {
                    return Err(Error::OnlyOne(Kind::Tab));
                }
                target_ws = ws;
                let index = view
                    .tab()
                    .and_then(|c| tabs.iter().position(|t| *t == c))
                    .unwrap_or(0);
                let next = round(index, tabs.len(), matches!(pick, Pick::Next));
                tabs.get(next).copied().ok_or(Error::NoCurrentTab)?
            }
            Pick::Last | Pick::Toward(_) => {
                return Err(Error::SelectTabPick);
            }
        };
        let view = self.view_mut(client)?;
        view.workspace = target_ws;
        view.tab_of.insert(target_ws, target);
        view.zoom = false;
        Ok(String::new())
    }

    fn select_workspace(&mut self, client: ClientId, pick: &Pick<WsRef>) -> Result<String, Error> {
        let current = self
            .views
            .get(&client)
            .ok_or(Error::NoSuchClient)?
            .workspace;
        let target = match pick {
            Pick::Id(r) => self.resolve_ws(r)?,
            Pick::Next | Pick::Previous => {
                let len = self.workspaces.len();
                if len < 2 {
                    return Err(Error::OnlyOne(Kind::Workspace));
                }
                let index = self.ws_index(current).unwrap_or(0);
                let next = round(index, len, matches!(pick, Pick::Next));
                self.workspaces
                    .get(next)
                    .map(|w| w.id)
                    .ok_or(Error::NoCurrentWorkspace)?
            }
            Pick::Last | Pick::Toward(_) => {
                return Err(Error::SelectWorkspacePick);
            }
        };
        let view = self.view_mut(client)?;
        view.workspace = target;
        view.zoom = false;
        Ok(String::new())
    }

    // ------------------------------------------------------------------ ls

    fn ls_text(&self) -> String {
        let mut out = String::new();
        for ws in &self.workspaces {
            out.push_str(&format!("{} {}\n", ws.id, ws.name));
            for tab in &ws.tabs {
                out.push_str(&format!(
                    "  {} {}{}\n",
                    tab.id,
                    tab.name,
                    if tab.root.is_none() { " (empty)" } else { "" }
                ));
                let Some(root) = &tab.root else { continue };
                root.for_each_pane(&mut |pane| {
                    if let Some(p) = self.panes.get(&pane) {
                        out.push_str(&format!(
                            "    {} {} {}x{}{}\n",
                            pane,
                            p.label(),
                            p.size.1,
                            p.size.0,
                            p.child
                                .as_ref()
                                .map(|c| format!(" pid {}", c.pid))
                                .unwrap_or_default()
                        ));
                    }
                });
            }
        }
        for view in self.views.values() {
            out.push_str(&format!(
                "client {} {}x{} {} {} {}\n",
                view.id,
                view.cols,
                view.rows,
                view.workspace,
                view.tab()
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "-".into()),
                view.focus()
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".into()),
            ));
        }
        out
    }

    fn ls_json(&self) -> String {
        let panes = |tab: &Tab| {
            let mut panes = Vec::new();
            let Some(root) = &tab.root else {
                return Json::Array(panes);
            };
            root.for_each_pane(&mut |id| {
                let Some(p) = self.panes.get(&id) else {
                    return;
                };
                panes.push(Json::Object(vec![
                    ("id", Json::str(p.id.to_string())),
                    ("name", Json::str(p.name.as_str())),
                    ("title", Json::str(p.title.as_str())),
                    ("rows", Json::Number(i64::from(p.size.0))),
                    ("cols", Json::Number(i64::from(p.size.1))),
                    (
                        "pid",
                        p.child
                            .as_ref()
                            .map_or(Json::Null, |c| Json::Number(i64::from(c.pid.as_raw()))),
                    ),
                ]));
            });
            Json::Array(panes)
        };
        let workspaces = self
            .workspaces
            .iter()
            .map(|ws| {
                Json::Object(vec![
                    ("id", Json::str(ws.id.to_string())),
                    ("name", Json::str(ws.name.as_str())),
                    (
                        "tabs",
                        Json::Array(
                            ws.tabs
                                .iter()
                                .map(|t| {
                                    Json::Object(vec![
                                        ("id", Json::str(t.id.to_string())),
                                        ("name", Json::str(t.name.as_str())),
                                        ("panes", panes(t)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect();
        let opt = |v: Option<String>| v.map_or(Json::Null, Json::str);
        let clients = self
            .views
            .values()
            .map(|v| {
                Json::Object(vec![
                    ("id", Json::str(v.id.to_string())),
                    ("rows", Json::Number(i64::from(v.rows))),
                    ("cols", Json::Number(i64::from(v.cols))),
                    ("workspace", Json::str(v.workspace.to_string())),
                    ("tab", opt(v.tab().map(|t| t.to_string()))),
                    ("pane", opt(v.focus().map(|p| p.to_string()))),
                    ("zoom", Json::Bool(v.zoom)),
                ])
            })
            .collect();
        let mut text = Json::Object(vec![
            ("workspaces", Json::Array(workspaces)),
            ("clients", Json::Array(clients)),
        ])
        .render();
        text.push('\n');
        text
    }
}

/// `%3`, `@2`, `+1` or a workspace's name.
pub fn describe(target: &AnyRef) -> String {
    match target {
        AnyRef::Pane(p) => p.to_string(),
        AnyRef::Tab(t) => t.to_string(),
        AnyRef::Workspace(WsRef::Id(w)) => w.to_string(),
        AnyRef::Workspace(WsRef::Name(n)) => n.clone(),
    }
}

/// Takes the next ID from `counter`. IDs are never reused, so one that
/// would wrap round is an error instead.
fn advance(counter: &mut u32, what: &'static str) -> Result<u32, Error> {
    let id = *counter;
    *counter = counter.checked_add(1).ok_or(Error::IdsExhausted(what))?;
    Ok(id)
}

/// The index after `index` among `len`, or before it, going round.
fn round(index: usize, len: usize, forward: bool) -> usize {
    if forward {
        index.checked_add(1).filter(|next| *next < len).unwrap_or(0)
    } else {
        index
            .checked_sub(1)
            .unwrap_or_else(|| len.saturating_sub(1))
    }
}

/// The text of a pane's screen, and `history` lines before it.
fn capture(pane: &Pane, history: Option<usize>, json: bool) -> String {
    let screen = pane.screen();
    let (rows, cols) = screen.size();
    let back = history.unwrap_or(0).min(screen.history_len());
    let window = screen.window(back, rows, cols);
    let mut lines = Vec::new();
    // History rows above the screen, then the screen itself.
    for offset in (0..back).rev() {
        if let Some(row) = usize::from(rows)
            .checked_add(offset)
            .and_then(|i| screen.row_from_bottom(i))
        {
            lines.push(row_text(row.cells));
        }
    }
    let live = screen.window(0, rows, cols);
    let _ = window;
    for y in 0..rows {
        lines.push(live.row(y).map(|r| row_text(r.cells)).unwrap_or_default());
    }
    if json {
        let (cy, cx) = screen.cursor_position();
        let mut text = Json::Object(vec![
            ("pane", Json::str(pane.id.to_string())),
            ("rows", Json::Number(i64::from(rows))),
            ("cols", Json::Number(i64::from(cols))),
            (
                "cursor",
                Json::Array(vec![
                    Json::Number(i64::from(cy)),
                    Json::Number(i64::from(cx)),
                ]),
            ),
            (
                "lines",
                Json::Array(lines.into_iter().map(Json::str).collect()),
            ),
        ])
        .render();
        text.push('\n');
        return text;
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// What a client's terminal shows, row by row, as `capture-client` prints
/// it.
fn capture_client(client: ClientId, grid: &crate::render::Grid, json: bool) -> String {
    let lines: Vec<String> = (0..grid.rows).map(|y| grid.row_text(y)).collect();
    if json {
        let cursor = grid.cursor.map_or(Json::Null, |(y, x)| {
            Json::Array(vec![Json::Number(i64::from(y)), Json::Number(i64::from(x))])
        });
        let mut text = Json::Object(vec![
            ("client", Json::str(client.to_string())),
            ("rows", Json::Number(i64::from(grid.rows))),
            ("cols", Json::Number(i64::from(grid.cols))),
            ("cursor", cursor),
            (
                "lines",
                Json::Array(lines.into_iter().map(Json::str).collect()),
            ),
        ])
        .render();
        text.push('\n');
        return text;
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// A row's text, wide glyphs whole and trailing blanks trimmed.
pub fn row_text(cells: &[fux_vt::Cell]) -> String {
    let mut line = String::new();
    for cell in cells {
        if cell.is_wide_continuation() {
            continue;
        }
        line.push_str(if cell.has_contents() {
            cell.contents()
        } else {
            " "
        });
    }
    line.trim_end_matches(' ').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `--json` outputs, byte for byte: escapes, borrowed names and
    /// formatted IDs alike.
    #[test]
    fn json_outputs_are_what_they_were() -> Result<(), Box<dyn std::error::Error>> {
        let config = Config {
            shell: vec!["/bin/sh".into()],
            ..Config::default()
        };
        let mut s = Session::new(config, "/nonexistent/fux.sock".into(), false);
        s.start()?;
        s.attach(4, 30, None)?;
        let run = |s: &mut Session, line: &str| -> Result<String, String> {
            let words = crate::words::split(line).map_err(|e| e.to_string())?;
            let outcome = s.run(&words, &Ctx::default());
            match outcome.status {
                0 => Ok(outcome.stdout),
                _ => Err(outcome.stderr),
            }
        };
        run(&mut s, "split -h -t %1")?;
        run(&mut s, r#"rename -t %1 'say "hi" \ 界'"#)?;
        run(&mut s, "new-tab -t +1 -n two")?;
        run(&mut s, "select-tab -c c1 -t @1")?;
        s.output(PaneId(2), "a\tb \"q\" 界\x1b]2;tab\\title\x07".as_bytes());
        assert_eq!(
            run(&mut s, "ls --json")?,
            concat!(
                r#"{"workspaces":[{"id":"+1","name":"main","tabs":[{"id":"@1","name":"main","panes":[{"id":"%1","name":"say \"hi\" \\ 界","title":"","rows":3,"cols":15,"pid":null},{"id":"%2","name":"sh","title":"tab\\title","rows":3,"cols":14,"pid":null}]},{"id":"@2","name":"two","panes":[{"id":"%3","name":"sh","title":"","rows":24,"cols":80,"pid":null}]}]}],"clients":[{"id":"c1","rows":4,"cols":30,"workspace":"+1","tab":"@1","pane":"%2","zoom":false}]}"#,
                "\n"
            )
        );
        assert_eq!(
            run(&mut s, "capture-pane -t %2 --json")?,
            concat!(
                r#"{"pane":"%2","rows":3,"cols":14,"cursor":[1,2],"lines":["a       b \"q\"","界",""]}"#,
                "\n"
            )
        );
        assert_eq!(
            run(&mut s, "capture-client -c c1 --json")?,
            concat!(
                r#"{"client":"c1","rows":4,"cols":30,"cursor":[1,18],"lines":["               │a       b \"q\"","               │界","               │"," main  main  two %2 tab\\title"]}"#,
                "\n"
            )
        );
        Ok(())
    }

    /// A failing command says what it said as a string, word for word:
    /// each line, run in turn on one session, and its status and message,
    /// as they were before commands failed with `Error`.
    #[test]
    fn command_errors_keep_their_words() -> Result<(), Box<dyn std::error::Error>> {
        let config = Config {
            shell: vec!["/bin/sh".into()],
            ..Config::default()
        };
        let mut s = Session::new(config, "/nonexistent/fux.sock".into(), false);
        s.start()?;
        s.attach(10, 40, None)?;
        for (line, status, message) in [
            ("kill-pane -t %99", 1, "no pane %99"),
            ("kill-tab -t @99", 1, "no tab @99"),
            ("kill-workspace -t +99", 1, "no workspace +99"),
            ("kill-workspace -t nope", 1, r#"no workspace named "nope""#),
            ("kill-pane", 1, "no pane given: use -t %N"),
            ("kill-tab", 1, "no tab given: use -t @N"),
            (
                "kill-workspace",
                1,
                "no workspace given: use -t +N or a name",
            ),
            ("rename -t nope x", 1, r#"no workspace named "nope""#),
            ("reorder workspace -t nope --next", 1, "no workspace nope"),
            ("menu -c c1 workspace -t nope", 1, "no workspace nope"),
            (
                "zoom",
                1,
                "this command acts on a client's screen: use -c CLIENT (`fux ls` lists clients)",
            ),
            ("zoom -c c9", 1, "no client c9"),
            ("show-buffer", 1, "no buffer 0"),
            ("paste-buffer -t %1", 1, "no buffer 0"),
            ("reload", 1, "no config file to reload"),
            ("terminate -t %1", 1, "the pane has no process"),
            ("rename -t %1 ''", 1, "a name cannot be empty"),
            (
                "rename -t %1 xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                1,
                "a name is at most 256 bytes",
            ),
            ("resize-pane -t %1 -U", 1, "%1 has no border to move up"),
            ("swap-pane -t %1 %99", 1, "no pane %99"),
            ("swap-pane -t %1 -L", 1, "no pane left of %1"),
            ("select-pane -c c1 --next", 1, "only one pane"),
            ("select-pane -c c1 --last", 1, "no previously focused pane"),
            ("select-pane -c c1 -L", 1, "no pane left of %1"),
            ("select-tab -c c1 --next", 1, "only one tab"),
            ("select-workspace -c c1 --next", 1, "only one workspace"),
            (
                "reorder tab -t @1 --next",
                1,
                "the tab is already at that end",
            ),
            (
                "reorder pane -t %1 --previous",
                1,
                "the pane is already at that end",
            ),
            (
                "reorder workspace -t +1 --previous",
                1,
                "the workspace is already at that end",
            ),
            ("choose-pane -c c1", 1, "only one pane"),
            ("move-pane -t %1 --to @99", 1, "no tab @99"),
            ("move-pane -t %1 --to +99", 1, "no workspace +99"),
            ("move-pane -t %1 -L", 1, "no pane left of %1"),
            (
                "set nope 1",
                1,
                "unknown option nope; options are prefix, shell, history-lines, clipboard, buffers",
            ),
            (
                "bind g nope",
                1,
                r#"bind g: unknown command "nope"; `fux help` lists commands"#,
            ),
            ("new-tab -t +99", 1, "no workspace +99"),
            (
                "capture-client",
                1,
                "this command acts on a client's screen: use -c CLIENT (`fux ls` lists clients)",
            ),
            ("copy-mode -c c9", 1, "no client c9"),
            ("new-workspace -n ''", 1, "a name cannot be empty"),
            ("send-keys -t %99 x", 1, "no pane %99"),
            ("select-pane -c c1 -t %99", 1, "no pane %99"),
            ("select-tab -c c1 -t @99", 1, "no tab @99"),
            ("select-workspace -c c1 -t +99", 1, "no workspace +99"),
            ("confirm-close -c c1 pane -t %99", 1, "no pane %99"),
            ("rename-prompt -c c1 tab -t @99", 1, "no tab @99"),
            ("choose-tab -c c1 -t %99", 1, "no pane %99"),
            ("new-workspace -n main", 0, ""),
            ("new-workspace -n two", 0, ""),
            (
                "rename -t +2 main",
                1,
                r#"another workspace is named "main""#,
            ),
            (
                "reorder workspace -t two --next",
                1,
                "the workspace is already at that end",
            ),
        ] {
            let outcome = s.run(&crate::words::split(line)?, &Ctx::default());
            assert_eq!(
                (outcome.status, outcome.stderr.as_str()),
                (status, message),
                "{line}"
            );
        }
        let argv = ["split", "-h", "-t", "%1", "--", "a\nb"].map(str::to_owned);
        let outcome = s.run(&argv, &Ctx::default());
        assert_eq!(
            (outcome.status, outcome.stderr.as_str()),
            (
                1,
                r#"the command argument "a\nb" contains the control character '\n'; it would act as a key in the shell"#
            )
        );
        // Why a menu entry or a binding cannot run now.
        let ctx = Ctx::client(ClientId(1));
        for (line, reason) in [
            ("select-pane --next", Some("only one pane")),
            ("select-tab --next", Some("only one tab")),
            ("select-workspace --next", None),
            ("paste-buffer", Some("no copied text yet")),
            ("terminate", Some("nothing is running in %1 but its shell")),
            ("kill-pane -t %99", Some("no pane %99")),
            ("choose-pane", Some("only one pane")),
        ] {
            let command = command::parse(&crate::words::split(line)?)?;
            let got = s.unavailable(&command, &ctx).map(|e| e.to_string());
            assert_eq!(got.as_deref(), reason, "{line}");
        }
        Ok(())
    }

    /// What a caller tells apart, it tells by variant.
    #[test]
    fn failures_are_told_apart_by_kind() -> Result<(), Box<dyn std::error::Error>> {
        let mut s = Session::new(Config::default(), "/nonexistent/fux.sock".into(), false);
        s.start()?;
        let c = s.attach(10, 40, None)?;
        let ctx = Ctx::client(c);
        let mut run = |line: &str| -> Result<Result<String, Error>, Box<dyn std::error::Error>> {
            let command = command::parse(&crate::words::split(line)?)?;
            Ok(s.execute(&command, &ctx))
        };
        assert!(matches!(
            run("kill-pane -t %9")?,
            Err(Error::NoPane(PaneId(9)))
        ));
        assert!(matches!(
            run("select-pane --next")?,
            Err(Error::OnlyOne(Kind::Pane))
        ));
        assert!(matches!(run("set nope 1")?, Err(Error::Config(_))));
        assert!(run("copy-mode")?.is_ok());
        assert!(matches!(
            s.attach(10, 40, Some("%1")),
            Err(Error::Usage(command::Usage::NotWorkspace(_)))
        ));
        assert!(matches!(
            s.attach(10, 40, Some("nope")),
            Err(Error::NoWorkspaceNamed(_))
        ));
        Ok(())
    }
}

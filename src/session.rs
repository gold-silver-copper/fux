//! The server's state: workspaces, tabs, panes and the clients' views, and
//! every command that changes them.
use crate::command::{
    self, AnyRef, ClientAction, Command, Kind, MoveTo, PanePick, Pick, Sibling, Subject, SwapWith,
    WsRef,
};
use crate::config::Config;
use crate::copy::MAX_CELLS;
use crate::id::{ClientId, Ids, PaneId, TabId, WsId};
use crate::json::Json;
use crate::keys::{Direction, KeyPress};
use crate::layout::{self, Axis, Placement, Rect, Side, Tree};
use crate::overlay::{Column, Repeat};
use crate::pane::{InputQueue, Pane, Process, Typed};
use crate::process::Child;
use crate::view::{Choice, Mode, View};
use crate::workspace::{Seat, Tab, Workspace};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What the server must do outside the session.
#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// Bytes for a client's terminal outside any paint: a copy (OSC 52),
    /// questions for the terminal, the bell, the keyboard flags pushed.
    Bytes(ClientId, Vec<u8>),
    /// Detach a client, telling it why.
    Exit(ClientId, String),
    /// Stop the server.
    Shutdown(String),
}

/// A closed pane's program, hung up and given until the deadline to exit;
/// then its terminal is closed and its group killed.
pub struct Dying {
    pub child: Child,
    pub deadline: Instant,
}

impl Dying {
    /// Kills the program, its terminal closed first (a dying writer can hold
    /// on to it); its leader, if it has yet to exit.
    pub fn kill(self) -> Option<crate::process::Leader> {
        drop(self.child.master);
        self.child.leader.finish().err()
    }
}

/// Something the session waits for until a moment (`Session::timers`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timer {
    /// A client's decoder waits on a lone Escape or an answer cut short
    /// (`Decoder::deadline`): what waits is taken as it is.
    Escape(ClientId),
    /// A pane's command line, held until its shell is ready, is typed
    /// (`InputQueue::due_at`).
    Type(PaneId),
    /// A pane's frame of synchronized output is read, ended or not
    /// (`Pane::frame_deadline`).
    Frame(PaneId),
}

/// Where a command comes from, which says what it acts on when it names no
/// pane, tab or workspace.
#[derive(Clone, Debug, Default)]
pub enum Origin {
    /// A client's key, prompt or menu: where the client is.
    Client(ClientId),
    /// The CLI: its caller's pane (`FUX_PANE`) and working directory, each
    /// if it gave one.
    Cli {
        pane: Option<PaneId>,
        cwd: Option<PathBuf>,
    },
    /// Nowhere: no client and no caller.
    #[default]
    Nowhere,
}

/// Where an attached client is: the workspace and tab it is shown, and the
/// pane it focuses there unless the tab is empty.
#[derive(Clone, Copy)]
pub struct Place {
    pub ws: WsId,
    pub tab: TabId,
    pub pane: Option<PaneId>,
}

/// What a command that names nothing acts on, found once from its origin.
#[derive(Clone, Copy)]
enum Here<'a> {
    /// An attached client, where it is.
    Client(ClientId, Place),
    /// The CLI: its caller's pane, where that is if it is a pane, and its
    /// working directory.
    Cli(Option<PaneId>, Option<Place>, Option<&'a Path>),
}

impl Here<'_> {
    fn place(self) -> Option<Place> {
        match self {
            Here::Client(_, place) => Some(place),
            Here::Cli(_, at, _) => at,
        }
    }
    fn pane(self) -> Option<PaneId> {
        match self {
            Here::Client(_, place) => place.pane,
            Here::Cli(pane, ..) => pane,
        }
    }
}

impl WsRef {
    fn names(&self, ws: &Workspace) -> bool {
        match self {
            WsRef::Id(id) => ws.id == *id,
            WsRef::Name(name) => ws.name == *name,
        }
    }

    /// Why no workspace is the one this names.
    fn missing(&self) -> Error {
        match self {
            WsRef::Id(id) => Error::NoWorkspace(*id),
            WsRef::Name(name) => Error::NoWorkspaceNamed(name.clone()),
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
    NoClientGiven,
    NoClient(ClientId),
    NoWorkspaces,
    NoBuffer(usize),
    NoCopiedText,
    NoConfigFile,
    // What the client's view has none of.
    NoLastPane,
    /// The client focuses no pane.
    NoPaneToCopy,
    /// The client's tab, its panes moved out.
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
    // What copy mode cannot do.
    NoRows,
    /// A selection of more than `MAX_CELLS` cells.
    SelectionTooLarge,
    // A name that cannot be given.
    EmptyName,
    ControlInName,
    LongName,
    NameTaken(String),
    /// A workspace name a target would read as a number, not a name.
    NameReadAsTarget(String),
    /// A counter of IDs, never reused, that would wrap.
    IdsExhausted(&'static str),
    LineTooLong,
    // What failed below the session.
    Usage(command::Usage),
    Words(crate::words::Error),
    Pane(crate::pane::Error),
    Process(crate::process::Error),
    Terminate(fuxix::Errno),
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
            Error::NoClientGiven => f.write_str(
                "this command acts on a client's screen: use -c CLIENT (`fux ls` lists clients)",
            ),
            Error::NoClient(id) => write!(f, "no client {id}"),
            Error::NoWorkspaces => f.write_str("the server has no workspace"),
            Error::NoBuffer(index) => write!(f, "no buffer {index}"),
            Error::NoCopiedText => f.write_str("no copied text yet"),
            Error::NoConfigFile => f.write_str("no config file to reload"),
            Error::NoLastPane => f.write_str("no previously focused pane"),
            Error::NoPaneToCopy => f.write_str("no pane to copy from"),
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
            Error::NoRows => f.write_str("the pane has no rows"),
            Error::SelectionTooLarge => write!(f, "the selection is larger than {MAX_CELLS} cells"),
            Error::EmptyName => f.write_str("a name cannot be empty"),
            Error::ControlInName => f.write_str("a name cannot contain control characters"),
            Error::LongName => f.write_str("a name is at most 256 bytes"),
            Error::NameTaken(name) => write!(f, "another workspace is named {name:?}"),
            Error::NameReadAsTarget(name) => write!(
                f,
                "{name:?} cannot name a workspace: a target would read it as a pane, tab or workspace number"
            ),
            Error::IdsExhausted(what) => write!(f, "no {what} IDs are left"),
            Error::LineTooLong => f.write_str("the command line is too long to type"),
            Error::Usage(error) => error.fmt(f),
            Error::Words(error) => error.fmt(f),
            Error::Pane(error) => error.fmt(f),
            Error::Process(error) => error.fmt(f),
            Error::Terminate(error) => error.fmt(f),
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
            Error::Config(error) | Error::Reload(error) => Some(error),
            Error::NoWorkspace(_)
            | Error::NoWorkspaceNamed(_)
            | Error::NoPaneGiven
            | Error::NoPane(_)
            | Error::NoTabGiven
            | Error::NoTab(_)
            | Error::NoWorkspaceGiven
            | Error::NoClientGiven
            | Error::NoClient(_)
            | Error::NoWorkspaces
            | Error::NoBuffer(_)
            | Error::NoCopiedText
            | Error::NoConfigFile
            | Error::NoLastPane
            | Error::NoPaneToCopy
            | Error::TabEmpty
            | Error::OnlyOne(_)
            | Error::AtEnd(_)
            | Error::NoBorder { .. }
            | Error::NoNeighbor { .. }
            | Error::OnlyShell(_)
            | Error::NoRows
            | Error::SelectionTooLarge
            | Error::EmptyName
            | Error::ControlInName
            | Error::LongName
            | Error::NameTaken(_)
            | Error::NameReadAsTarget(_)
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

fn default_size() -> fux_vt::Size {
    crate::pane::size(DEFAULT_SIZE.0, DEFAULT_SIZE.1)
}

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
    /// The socket a started pane is told of, if panes start real
    /// processes; tests of the state alone start none.
    launch: Option<PathBuf>,
    pub outbox: Vec<Outgoing>,
    /// Closed panes' programs, by the pane they were in.
    pub dying: BTreeMap<PaneId, Dying>,
    /// The colours a client's terminal last said, for panes no attached
    /// client answers for (`outer`).
    pub last_colours: crate::outer::Colours,
    /// The palette entries any client's terminal said last, each entry the
    /// latest said (`outer`).
    pub last_palette: crate::outer::Palette,
    /// Where `size_panes` gathers the rectangles each pane is shown in, and
    /// lays out each view to find them; reused by every settle.
    shown_sizes: Vec<(PaneId, (u16, u16))>,
    placed: Placement,
    ids: Ids,
    /// Commands run that may change what any client shows; input that runs
    /// one repaints every client, input that runs none only its own.
    changes: u64,
    /// Output changed a pane a client's copy mode holds rows of: the next
    /// `settle_if_needed` repairs the views, to end copy mode if they went.
    unsettled: bool,
}

/// The name of the first workspace, and of each workspace's first tab.
pub(crate) const MAIN: &str = "main";

/// Whether a command leaves every client's screen as it was: it reads the
/// state, or hands a pane's program something whose effect, if any, comes
/// back as output.
fn shows_nothing_new(command: &Command) -> bool {
    matches!(
        command,
        Command::Ls { .. }
            | Command::ListKeys
            | Command::ListBuffers
            | Command::ShowBuffer { .. }
            | Command::CapturePane { .. }
            | Command::SendKeys { .. }
            | Command::SendPrefix { .. }
            | Command::Terminate { .. }
            | Command::Client {
                action: ClientAction::Capture { .. },
                ..
            }
    )
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
            launch: launch.then_some(socket),
            outbox: Vec::new(),
            dying: BTreeMap::new(),
            last_colours: crate::outer::Colours::default(),
            last_palette: crate::outer::Palette::default(),
            shown_sizes: Vec::new(),
            placed: Placement::default(),
            ids: Ids::default(),
            changes: 0,
            unsettled: false,
        }
    }

    /// One workspace, holding one tab with one shell.
    pub fn start(&mut self) -> Result<(), Error> {
        self.create_workspace(Some(MAIN.into()), &[], &home())
            .map(|_| ())
    }

    // ---------------------------------------------------------------- lookup

    pub fn workspace(&self, id: WsId) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == id)
    }
    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.workspaces
            .iter()
            .flat_map(Workspace::tabs)
            .find(|t| t.id == id)
    }
    /// A tab's layout, unless it is empty.
    pub fn root(&self, tab: TabId) -> Option<&Tree> {
        self.tab(tab)?.root()
    }
    /// The workspace and tab that hold a pane.
    pub fn locate(&self, pane: PaneId) -> Option<(WsId, TabId)> {
        let mut tabs = (self.workspaces.iter()).flat_map(|w| w.tabs().iter().map(|t| (w.id, t)));
        tabs.find(|(_, t)| t.holds(pane)).map(|(w, t)| (w, t.id))
    }
    /// The workspace a tab is in.
    pub fn tab_workspace(&self, tab: TabId) -> Result<WsId, Error> {
        let ws = (self.workspaces.iter()).find(|w| w.tabs().iter().any(|t| t.id == tab));
        ws.map(|w| w.id).ok_or(Error::NoTab(tab))
    }
    /// The workspace client `client` shows.
    pub fn shown_workspace(&self, client: ClientId) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.viewers.contains(&client))
    }
    /// The tab client `client` shows.
    pub fn shown_tab(&self, client: ClientId) -> Option<&Tab> {
        shown_tab(&self.workspaces, client)
    }
    pub(crate) fn shown_tab_mut(&mut self, client: ClientId) -> Option<&mut Tab> {
        let ws = (self.workspaces.iter_mut()).find(|w| w.viewers.contains(&client))?;
        ws.tabs_mut().iter_mut().find(|t| t.shown_to(client))
    }
    /// The pane client `client` focuses.
    pub fn focused(&self, client: ClientId) -> Option<PaneId> {
        self.shown_tab(client)?.focus(client)
    }
    /// Whether client `client` shows tab `tab`.
    pub(crate) fn shows(&self, client: ClientId, tab: TabId) -> bool {
        self.shown_tab(client).is_some_and(|t| t.id == tab)
    }
    pub fn resolve_ws(&self, r: &WsRef) -> Result<WsId, Error> {
        let found = self.workspaces.iter().find(|w| r.names(w));
        found.map(|w| w.id).ok_or_else(|| r.missing())
    }
    pub fn exists(&self, what: AnyRef<WsId>) -> bool {
        match what {
            AnyRef::Pane(p) => self.panes.contains_key(&p),
            AnyRef::Tab(t) => self.tab(t).is_some(),
            AnyRef::Workspace(w) => self.workspace(w).is_some(),
        }
    }
    /// What `target` names, if it is there: a workspace by its ID, as it is
    /// held from then on.
    pub(crate) fn resolve(&self, target: &AnyRef) -> Result<AnyRef<WsId>, Error> {
        Ok(match *target {
            AnyRef::Pane(p) => AnyRef::Pane(self.panes.get(&p).ok_or(Error::NoPane(p))?.id),
            AnyRef::Tab(t) => AnyRef::Tab(self.tab(t).ok_or(Error::NoTab(t))?.id),
            AnyRef::Workspace(ref w) => AnyRef::Workspace(self.resolve_ws(w)?),
        })
    }

    // ------------------------------------------------------------- targets

    /// Where a command from `origin` acts: an attached client's place, or
    /// the CLI caller's pane's.
    fn here<'a>(&self, origin: &'a Origin) -> Result<Here<'a>, Error> {
        match *origin {
            Origin::Client(client) => {
                let place = self.place(client).ok_or(Error::NoClient(client))?;
                Ok(Here::Client(client, place))
            }
            Origin::Cli { pane, ref cwd } => {
                let at = pane.and_then(|p| self.locate(p));
                let at = at.map(|(ws, tab)| Place { ws, tab, pane });
                Ok(Here::Cli(pane, at, cwd.as_deref()))
            }
            Origin::Nowhere => Ok(Here::Cli(None, None, None)),
        }
    }

    /// Where client `client` is, if it is attached: the workspace it is
    /// shown, while there is any.
    pub fn place(&self, client: ClientId) -> Option<Place> {
        let ws = self.shown_workspace(client)?;
        let tab = ws.tab_of(client)?;
        let (ws, pane, tab) = (ws.id, tab.focus(client), tab.id);
        Some(Place { ws, tab, pane })
    }

    fn pane(&self, explicit: Option<PaneId>, here: Here) -> Result<&Pane, Error> {
        let id = explicit.or(here.pane()).ok_or(Error::NoPaneGiven)?;
        self.panes.get(&id).ok_or(Error::NoPane(id))
    }

    fn pane_mut(&mut self, explicit: Option<PaneId>, here: Here) -> Result<&mut Pane, Error> {
        let id = explicit.or(here.pane()).ok_or(Error::NoPaneGiven)?;
        self.panes.get_mut(&id).ok_or(Error::NoPane(id))
    }

    fn tab_target(&self, explicit: Option<TabId>, here: Here) -> Result<TabId, Error> {
        let id = explicit.or(here.place().map(|p| p.tab));
        let id = id.ok_or(Error::NoTabGiven)?;
        self.tab(id).map(|t| t.id).ok_or(Error::NoTab(id))
    }

    fn ws_target(&self, explicit: Option<&WsRef>, here: Here) -> Result<WsRef, Error> {
        explicit
            .cloned()
            .or(here.place().map(|p| WsRef::Id(p.ws)))
            .ok_or(Error::NoWorkspaceGiven)
    }

    fn any_target(&self, subject: &Subject, here: Here) -> Result<AnyRef<WsId>, Error> {
        if let Some(target) = subject.target() {
            return self.resolve(&target);
        }
        Ok(match subject.kind() {
            Kind::Pane => AnyRef::Pane(self.pane(None, here)?.id),
            Kind::Tab => AnyRef::Tab(self.tab_target(None, here)?),
            Kind::Workspace => AnyRef::Workspace(self.resolve_ws(&self.ws_target(None, here)?)?),
        })
    }

    // ------------------------------------------------------------ creation

    fn cwd_for(&self, here: Here, near: Option<PaneId>) -> PathBuf {
        let near = match here {
            Here::Cli(.., Some(cwd)) => return cwd.to_owned(),
            Here::Cli(.., None) => near,
            Here::Client(_, place) => near.or(place.pane),
        };
        let leader = near.and_then(|p| self.panes.get(&p)?.process.child());
        leader
            .and_then(|c| fuxix::process::cwd(c.leader.pid()))
            .unwrap_or_else(home)
    }

    /// The name a workspace gets when none is given: `workspace-N` after
    /// its ID, or the next number up that no workspace is named.
    fn workspace_name(&self, id: WsId) -> String {
        let mut n = id.number();
        loop {
            let name = format!("workspace-{n}");
            if !self.workspaces.iter().any(|ws| ws.name == name) {
                return name;
            }
            match n.checked_add(1) {
                Some(next) => n = next,
                None => return name,
            }
        }
    }

    fn create_workspace(
        &mut self,
        name: Option<String>,
        cmd: &[String],
        cwd: &Path,
    ) -> Result<WsId, Error> {
        if let Some(name) = &name {
            check_workspace_name(name, &self.workspaces)?;
        }
        // The IDs are taken before the pane starts, so that none can run out
        // after, and are committed once it has.
        let mut ids = self.ids;
        let id = ids.workspace()?;
        let tab = ids.tab()?;
        let launch = self.launch.as_deref();
        let pane = new_pane(&self.config, launch, &mut ids, cmd, cwd, default_size())?;
        self.ids = ids;
        let name = name.unwrap_or_else(|| self.workspace_name(id));
        let root = Some(Tree::Pane(pane.id));
        let clients = self.views.keys().copied();
        (self.workspaces).push(Workspace::new(id, name, tab, root, clients));
        self.panes.insert(pane.id, pane);
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
        let id = self.ids.client()?;
        let mut view = View::new(id, rows.clamp(1, 4096), cols.clamp(1, 4096));
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
        self.workspaces.iter_mut().for_each(|w| w.attach(id));
        self.show(id, ws, None, None);
        self.ask_terminal(id);
        self.settle();
        Ok(id)
    }

    pub fn detach(&mut self, id: ClientId) {
        self.views.remove(&id);
        self.workspaces.iter_mut().for_each(|w| w.forget(id));
        self.settle();
    }

    /// Shows `client` workspace `ws`, one there is, and there tab `tab` and
    /// in it pane `pane`, each if given and there.
    fn show(&mut self, client: ClientId, ws: WsId, tab: Option<TabId>, pane: Option<PaneId>) {
        for w in &mut self.workspaces {
            w.viewers.remove(&client);
            if w.id == ws {
                w.viewers.insert(client);
                w.show(client, tab, pane);
            }
        }
    }

    /// The command's client, if it has one, follows what it made or moved:
    /// it is shown it, unzoomed.
    fn follow(&mut self, here: Here, ws: WsId, tab: Option<TabId>, pane: Option<PaneId>) {
        if let Here::Client(client, _) = here
            && let Some(view) = self.views.get_mut(&client)
        {
            view.zoom = false;
            self.show(client, ws, tab, pane);
        }
    }

    /// A client's terminal is `rows` by `cols`: its view, kept at most 4096
    /// each way, takes the size and settles if that changes it, and says
    /// whether it did.
    pub fn resize(&mut self, id: ClientId, rows: u16, cols: u16) -> bool {
        let size = (rows.clamp(1, 4096), cols.clamp(1, 4096));
        let view = self.views.get_mut(&id).filter(|v| (v.rows, v.cols) != size);
        let Some(view) = view else { return false };
        (view.rows, view.cols) = size;
        view.dirty = true;
        self.settle();
        true
    }

    /// Gives a client an error notice, if it is still attached.
    pub fn error_to(&mut self, client: ClientId, text: impl Into<String>) {
        if let Some(view) = self.views.get_mut(&client) {
            view.error(text);
        }
    }

    /// Gives a client a notice, if it is still attached.
    pub fn info_to(&mut self, client: ClientId, text: impl Into<String>) {
        if let Some(view) = self.views.get_mut(&client) {
            view.info(text);
        }
    }

    /// Sets a client's mode, if it is still attached, for its next paint.
    pub fn set_mode(&mut self, client: ClientId, mode: Mode) {
        if let Some(view) = self.views.get_mut(&client) {
            view.mode = mode;
            view.dirty = true;
        }
    }

    // -------------------------------------------------------------- layout

    /// The area panes share on a client's screen: all but the bar.
    pub fn pane_area(view: &View) -> Rect {
        Rect::screen(view.rows.saturating_sub(1), view.cols)
    }

    /// Where each pane of a view's current tab is on its screen.
    pub fn placement(&self, view: &View) -> Placement {
        let mut out = Placement::default();
        self.placement_into(view, &mut out);
        out
    }

    /// Where each pane of a view's current tab is on its screen, into
    /// `out`, whatever it held, reusing its buffers.
    pub fn placement_into(&self, view: &View, out: &mut Placement) {
        let area = Self::pane_area(view);
        let tab = self.shown_tab(view.id);
        let Some(root) = tab.and_then(Tab::root) else {
            return out.clear();
        };
        if view.zoom
            && let Some(focus) = tab.and_then(|t| t.focus(view.id))
            && !area.is_empty()
        {
            out.clear();
            return out.panes.push((focus, area));
        }
        layout::place_into(root, area, out);
    }

    /// The area to lay out a tab in for a command without a client: a
    /// client showing it, else any client, else a typical terminal.
    fn reference_area(&self, tab: TabId, here: Here) -> Rect {
        let own = match here {
            Here::Client(client, _) => self.views.get(&client),
            Here::Cli(..) => None,
        };
        let shows = |v: &&View| self.shows(v.id, tab);
        let view = (own.filter(shows))
            .or_else(|| self.views.values().find(shows))
            .or(own)
            .or_else(|| self.views.values().next());
        view.map_or(
            Rect::screen(DEFAULT_SIZE.0, DEFAULT_SIZE.1),
            Self::pane_area,
        )
    }

    // -------------------------------------------------------------- repair

    /// Ends each view's mode if what it shows is gone, then sizes the PTYs:
    /// run after every event.
    pub fn settle(&mut self) {
        self.unsettled = false;
        // Out of the session while they are repaired, which reads no view.
        let mut views = std::mem::take(&mut self.views);
        views.values_mut().for_each(|view| self.repair(view));
        self.views = views;
        self.size_panes();
        self.hold_copies();
    }

    /// Output and sizing a pane can drop rows of its history: a copy mode
    /// whose rows went ends. They are looked for only if the screen changed
    /// since they were last found.
    fn hold_copies(&mut self) {
        for view in self.views.values_mut() {
            let Mode::Copy(copy) = &mut view.mode else {
                continue;
            };
            let Some(screen) = self.panes.get(&copy.pane).map(Pane::screen) else {
                continue;
            };
            if copy.held_at.is_some_and(|at| !screen.changed_since(at)) {
                continue;
            }
            if copy.resolve(screen).is_some() {
                copy.held_at = Some(screen.mark());
            } else {
                view.mode = Mode::Normal;
                view.error("copy mode ended: the history dropped the rows it held");
            }
        }
    }

    fn repair(&self, view: &mut View) {
        let focus = self.focused(view.id);
        // What the view's mode refers to must still exist. Copy mode's rows
        // are looked for once the panes are sized (`hold_copies`).
        let gone: Option<String> = match &view.mode {
            Mode::Copy(copy) if !self.panes.contains_key(&copy.pane) => {
                Some("copy mode ended: its pane closed".into())
            }
            Mode::Copy(copy) if Some(copy.pane) != focus => {
                Some("copy mode ended: its pane is no longer focused".into())
            }
            Mode::List(list) => list
                .about
                .filter(|a| !self.exists(*a))
                .map(|a| format!("closed: {} is gone", describe(a))),
            Mode::Confirm(confirm) => (!self.exists(confirm.about))
                .then(|| format!("closed: {} is gone", describe(confirm.about))),
            Mode::Prompt(prompt) => match prompt.purpose {
                crate::view::PromptFor::Rename(target) if !self.exists(target) => {
                    Some(format!("closed: {} is gone", describe(target)))
                }
                crate::view::PromptFor::Command | crate::view::PromptFor::Rename(_) => None,
            },
            // Found again whenever the bindings change (`rebind`).
            Mode::Normal | Mode::Column(_) | Mode::Repeat(_) | Mode::Copy(_) => None,
        };
        if let Some(reason) = gone {
            view.mode = Mode::Normal;
            view.error(reason);
        }
    }

    /// The bindings changed: each client's command column and repeat mode
    /// is found in them again, the column keeping its place; one whose
    /// layer went closes, saying so.
    fn rebind(&mut self) {
        let clients: Vec<ClientId> = self.views.keys().copied().collect();
        for id in clients {
            let found = match self.views.get(&id).map(|v| &v.mode) {
                Some(Mode::Column(old)) => {
                    let at = old.entries.as_ref().map_or(0, Choice::index);
                    Column::layer(self, old.path.clone(), at)
                        .map(Mode::Column)
                        .ok_or_else(|| format!("the layer {}", self.keys_named(&old.path)))
                }
                Some(Mode::Repeat(old)) => Repeat::of(&self.config, old.path.clone())
                    .map(Mode::Repeat)
                    .ok_or_else(|| format!("the repeat mode {}", self.keys_named(&old.path))),
                _ => continue,
            };
            match found {
                Ok(mode) => self.set_mode(id, mode),
                Err(gone) => {
                    self.set_mode(id, Mode::Normal);
                    self.error_to(id, format!("closed: {gone} is gone"));
                }
            }
        }
    }

    /// The prefix and keys after it as they are typed, `C-b t`: as the bar,
    /// the column and messages write them.
    pub(crate) fn keys_named(&self, path: &[KeyPress]) -> String {
        let keys = std::iter::once(&self.config.prefix).chain(path);
        keys.map(KeyPress::to_string).collect::<Vec<_>>().join(" ")
    }

    /// A PTY is the smallest rectangle any client shows it in; a pane nobody
    /// shows keeps its size.
    fn size_panes(&mut self) {
        let mut sizes = std::mem::take(&mut self.shown_sizes);
        let mut placed = std::mem::take(&mut self.placed);
        sizes.clear();
        for view in self.views.values() {
            self.placement_into(view, &mut placed);
            sizes.extend(placed.panes.iter().map(|(p, r)| (*p, (r.h(), r.w()))));
        }
        self.placed = placed;
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
            let size = crate::pane::size(rows, cols);
            if let Some(pane) = self.panes.get_mut(&id)
                && pane.size() != size
            {
                pane.resize(size);
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
        if let Process::Reading(child) | Process::HungUp(child) = pane.process {
            child.leader.hang_up();
            let deadline = crate::after(Instant::now(), GRACE);
            self.dying.insert(pane.id, Dying { child, deadline });
        }
    }

    /// Removes a pane and its process. A tab its closing empties closes too,
    /// as does a workspace left with no tabs; a tab emptied by moving its
    /// panes out stays.
    pub fn close_pane(&mut self, id: PaneId, why: Option<String>) {
        let place = self.locate(id);
        // Who sees the pane is known before its tab can go: a tab its
        // closing empties takes its viewers elsewhere.
        let viewers: Vec<ClientId> = (self.views.keys().copied())
            .filter(|c| place.is_some_and(|(_, t)| self.shows(*c, t)))
            .collect();
        let emptied = tab_of(&mut self.workspaces, id).ok().and_then(|t| {
            t.edit(|root| layout::remove(root, id));
            t.root().is_none().then_some(t.id)
        });
        if let Some(tab) = emptied {
            self.remove_tab(tab);
        }
        if let Some(pane) = self.panes.remove(&id) {
            if let Some(why) = why {
                for client in &viewers {
                    self.info_to(*client, format!("{id} {} {why}", pane.label()));
                }
            }
            self.end(pane);
        }
        self.after_close();
    }

    /// Removes a tab and whatever panes are in it. A workspace it leaves
    /// with none goes too, its viewers shown the first workspace left.
    fn remove_tab(&mut self, tab: TabId) {
        let ws = (self.workspaces.iter_mut()).find(|w| w.tabs().iter().any(|t| t.id == tab));
        let Some(ws) = ws else {
            return;
        };
        let root = ws.remove_tab(tab);
        if ws.tabs().is_empty() {
            let (id, viewers) = (ws.id, std::mem::take(&mut ws.viewers));
            self.workspaces.retain(|w| w.id != id);
            if let Some(first) = self.workspaces.first_mut() {
                first.viewers.extend(viewers);
            }
        }
        // Its panes go with it.
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
            .map(|w| w.tabs().iter().map(|t| t.id).collect())
            .unwrap_or_default();
        for tab in tabs {
            self.remove_tab(tab);
        }
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

    /// Settles, if anything since the last settle needs it. Every change to
    /// workspaces, tabs, panes and views settles as it is made; output does
    /// not change them, but can drop history rows a copy mode holds.
    pub fn settle_if_needed(&mut self) {
        if self.unsettled {
            self.settle();
        }
    }

    /// Whether a settle is pending.
    pub fn unsettled(&self) -> bool {
        self.unsettled
    }

    /// Output from a pane's program.
    pub fn output(&mut self, id: PaneId, bytes: &[u8]) {
        self.read_with(id, |pane| pane.output(bytes));
    }

    /// Everything the session waits for, each with when it is due: the
    /// server polls no longer than the soonest, and fires those due
    /// (`Session::fire`).
    pub fn timers(&self) -> impl Iterator<Item = (Instant, Timer)> + '_ {
        let escapes = (self.views.iter())
            .filter_map(|(client, view)| Some((view.decoder.deadline()?, Timer::Escape(*client))));
        let panes = self.panes.values().flat_map(|pane| {
            let typed = pane.input.due_at().map(|at| (at, Timer::Type(pane.id)));
            let frame = pane.frame_deadline().map(|at| (at, Timer::Frame(pane.id)));
            typed.into_iter().chain(frame)
        });
        escapes.chain(panes)
    }

    /// What `timer` waited for is done, now that it is due.
    pub fn fire(&mut self, timer: Timer) {
        match timer {
            Timer::Escape(client) => self.escape(client),
            Timer::Type(id) => (self.panes.get_mut(&id))
                .into_iter()
                .for_each(|p| p.input.type_now()),
            Timer::Frame(id) => self.read_with(id, Pane::release_frame),
        }
    }

    /// Reads into pane `id` with `read`, as every read of a pane's output
    /// does: its tab's host colours and palette given it first, what
    /// follows a read seen to after (`read_into`).
    fn read_with(&mut self, id: PaneId, read: impl FnOnce(&mut Pane)) {
        let place = self.locate(id);
        let colours = self.colours_for(place.map(|(_, t)| t));
        let palette = self.palette_for(place.map(|(_, t)| t));
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        pane.colours = colours;
        pane.set_host_palette(palette);
        read(pane);
        let dropped = pane.input.lost_reply();
        self.read_into(id, place, dropped);
    }

    /// After output was read into pane `id`'s screen, which is at `place`:
    /// the views showing it repaint, and are told if a reply was `dropped`.
    fn read_into(&mut self, id: PaneId, place: Option<(WsId, TabId)>, dropped: bool) {
        if self
            .panes
            .get_mut(&id)
            .is_some_and(|p| std::mem::take(&mut p.bell))
        {
            self.ring(id, Instant::now());
        }
        self.unsettled |= self
            .views
            .values()
            .any(|view| matches!(&view.mode, Mode::Copy(copy) if copy.pane == id));
        for view in self.views.values_mut() {
            let shown = shown_tab(&self.workspaces, view.id).map(|t| t.id);
            if place.is_some_and(|(_, t)| shown == Some(t)) {
                view.dirty = true;
                if dropped {
                    view.error(format!(
                        "{id} is not reading its input; a terminal reply was dropped"
                    ));
                }
            }
        }
    }

    /// Everything the server shuts down: every pane is hung up.
    pub fn shutdown(&mut self) {
        for pane in std::mem::take(&mut self.panes).into_values() {
            self.end(pane);
        }
    }

    // ------------------------------------------------------------ commands

    /// Runs a command line from the CLI or the prompt.
    pub fn run(&mut self, argv: &[String], origin: &Origin) -> Outcome {
        let usage = match command::parse(argv) {
            Ok(command) => return self.run_command(&command, origin),
            Err(usage) => usage,
        };
        let outcome = Outcome {
            status: 2,
            stdout: String::new(),
            stderr: usage.to_string(),
        };
        self.tell(origin, &outcome, false);
        // Nothing ran, so nothing shows anything new.
        self.settle();
        outcome
    }

    /// Runs a command, parsed already: from a line, a binding or a menu.
    pub fn run_command(&mut self, command: &Command, origin: &Origin) -> Outcome {
        let outcome = match self.execute(command, origin) {
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
        if !shows_nothing_new(command) {
            self.changes = self.changes.wrapping_add(1);
            self.touch();
        }
        self.settle();
        let made = matches!(
            command,
            Command::Split { .. }
                | Command::NewTab { .. }
                | Command::NewWorkspace { .. }
                | Command::MovePane { .. }
        );
        self.tell(origin, &outcome, made);
        outcome
    }

    /// A client's command tells the client how it went, in its notice: the
    /// first line of its error, or of what it printed, with `…` if more
    /// follow; but not the ID of what it `made`, which the client is shown.
    fn tell(&mut self, origin: &Origin, outcome: &Outcome, made: bool) {
        let &Origin::Client(client) = origin else {
            return;
        };
        let mut lines = outcome.stdout.lines().filter(|l| !l.trim().is_empty());
        if outcome.status != 0 {
            self.error_to(client, outcome.stderr.lines().next().unwrap_or("failed"));
        } else if let Some(line) = lines.next().filter(|_| !made) {
            let more = if lines.next().is_some() { " …" } else { "" };
            self.info_to(client, format!("{line}{more}"));
        }
    }

    /// How many commands have run that may change what clients show.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// Why a command cannot run now, if it cannot: menus and the command
    /// column dim such entries, and running one says why.
    pub fn unavailable(&self, command: &Command, client: ClientId) -> Option<Error> {
        let Some(place) = self.place(client) else {
            return Some(Error::NoClient(client));
        };
        // Going to the next or previous one needs another.
        let alone = |kind, count: usize| (count < 2).then_some(Error::OnlyOne(kind));
        let action = if let Command::Client { action, .. } = command {
            Some(action)
        } else {
            None
        };
        if matches!(
            action,
            Some(
                ClientAction::SelectPane(PanePick::Step(_) | PanePick::Last)
                    | ClientAction::ChoosePane { .. }
            )
        ) {
            let mut panes = 0usize;
            if let Some(root) = self.root(place.tab) {
                root.for_each_pane(&mut |_| panes = panes.saturating_add(1));
            }
            return alone(Kind::Pane, panes);
        }
        if let Some(ClientAction::SelectTab(Pick::Step(_))) = action {
            let ws = self.workspace(place.ws);
            return alone(Kind::Tab, ws.map_or(0, |w| w.tabs().len()));
        }
        if let Some(ClientAction::SelectWorkspace(Pick::Step(_))) = action {
            return alone(Kind::Workspace, self.workspaces.len());
        }
        if let Command::PasteBuffer { index, .. } = command {
            return self
                .buffers
                .get(*index)
                .is_none()
                .then_some(Error::NoCopiedText);
        }
        let here = Here::Client(client, place);
        if let Command::Terminate { target } = command {
            return match self.pane(*target, here) {
                Ok(pane) => (pane.process.job().is_none()).then_some(Error::OnlyShell(pane.id)),
                Err(error) => Some(error),
            };
        }
        if let Command::KillPane { target }
        | Command::SwapPane { target, .. }
        | Command::MovePane { target, .. } = command
        {
            return self.pane(*target, here).err();
        }
        None
    }

    fn execute(&mut self, command: &Command, origin: &Origin) -> Result<String, Error> {
        let here = self.here(origin)?;
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
                out.push_str("; case counts, V is Shift-v):\n");
                for (keys, binding, repeat) in self.config.bindings.all() {
                    out.push_str(&format!(
                        "{:>8}  {}{}\n",
                        crate::config::keys_text(&keys),
                        crate::words::join(&binding.command),
                        if repeat { " (repeats)" } else { "" }
                    ));
                }
                if !self.config.root.is_empty() {
                    out.push_str("\nWithout the prefix (bind -n):\n");
                    for (key, binding) in &self.config.root {
                        out.push_str(&format!(
                            "{:>8}  {}\n",
                            key.to_string(),
                            crate::words::join(&binding.command),
                        ));
                    }
                }
                Ok(out)
            }
            Command::NewWorkspace { name, cmd } => {
                let cwd = self.cwd_for(here, None);
                let ws = self.create_workspace(name.clone(), cmd, &cwd)?;
                self.follow(here, ws, None, None);
                Ok(format!("{ws}\n"))
            }
            Command::NewTab { target, name, cmd } => {
                let named = self.ws_target(target.as_ref(), here)?;
                let cwd = self.cwd_for(here, None);
                let ws = self.workspaces.iter_mut().find(|w| named.names(w));
                let ws = ws.ok_or_else(|| named.missing())?;
                if let Some(name) = &name {
                    check_name(name)?;
                }
                // As for a workspace: the ID first, committed after the pane.
                let mut ids = self.ids;
                let id = ids.tab()?;
                let launch = self.launch.as_deref();
                let pane = new_pane(&self.config, launch, &mut ids, cmd, &cwd, default_size())?;
                self.ids = ids;
                ws.add_tab(id, name.clone(), Some(Tree::Pane(pane.id)));
                let ws = ws.id;
                self.panes.insert(pane.id, pane);
                self.follow(here, ws, Some(id), None);
                Ok(format!("{id}\n"))
            }
            &Command::Split {
                axis,
                target,
                ref cmd,
            } => {
                let (target, size) = self.pane(target, here).map(|p| (p.id, p.size()))?;
                let cwd = self.cwd_for(here, Some(target));
                // The splitting client follows the new pane; from the CLI, the
                // clients that focus the split one.
                let shown = |c: &ClientId| self.focused(*c) == Some(target);
                let followers: Vec<ClientId> = match here {
                    Here::Client(client, _) => vec![client],
                    Here::Cli(..) => self.views.keys().copied().filter(shown).collect(),
                };
                let tab = tab_of(&mut self.workspaces, target)?;
                let mut ids = self.ids;
                let launch = self.launch.as_deref();
                let pane = new_pane(&self.config, launch, &mut ids, cmd, &cwd, size)?;
                self.ids = ids;
                let new = pane.id;
                tab.edit(|root| layout::split(root, target, new, axis, Side::After));
                self.panes.insert(new, pane);
                let views = self.views.values_mut();
                for view in views.filter(|v| followers.contains(&v.id)) {
                    tab.set_focus(view.id, new);
                    view.zoom = false;
                }
                Ok(format!("{new}\n"))
            }
            &Command::KillPane { target } => {
                let pane = self.pane(target, here)?.id;
                self.close_pane(pane, None);
                Ok(String::new())
            }
            &Command::KillTab { target } => {
                let tab = self.tab_target(target, here)?;
                self.remove_tab(tab);
                self.after_close();
                Ok(String::new())
            }
            Command::KillWorkspace { target } => {
                let ws = self.resolve_ws(&self.ws_target(target.as_ref(), here)?)?;
                self.remove_workspace(ws);
                self.after_close();
                Ok(String::new())
            }
            Command::Rename { target, name } => {
                let target = self.resolve(target)?;
                self.rename(target, name.clone()).map(|()| String::new())
            }
            &Command::MovePane { target, ref to } => self.move_pane(target, to, here),
            &Command::SwapPane { target, with } => {
                let source = self.pane(target, here)?.id;
                let other = match with {
                    SwapWith::Pane(p) => self.pane(Some(p), here)?.id,
                    SwapWith::Toward(direction) => self.neighbor(source, direction, here)?,
                };
                self.swap(source, other);
                Ok(String::new())
            }
            &Command::ResizePane {
                target,
                direction,
                amount,
            } => {
                let pane = self.pane(target, here)?.id;
                let (_, tab) = self.locate(pane).ok_or(Error::NoPane(pane))?;
                let area = self.reference_area(tab, here);
                let tab = tab_of(&mut self.workspaces, pane)?;
                let resize = |root: &mut Option<Tree>| {
                    let root = root.as_mut();
                    root.is_some_and(|root| layout::resize(root, area, pane, direction, amount))
                };
                if tab.edit(resize) {
                    Ok(String::new())
                } else {
                    Err(Error::NoBorder { pane, direction })
                }
            }
            &Command::SendPrefix { target } => {
                let prefix = self.config.prefix;
                let p = self.pane_mut(target, here)?;
                let mode = p.screen().key_mode();
                p.input
                    .push_with(|out| crate::encode::key_bytes(prefix.into(), mode, out))?;
                Ok(String::new())
            }
            &Command::SendKeys {
                target,
                literal,
                ref keys,
            } => {
                let p = self.pane_mut(target, here)?;
                // Each argument is a key name (`C-c`, `Enter`, `a`), or, as in
                // tmux, text sent as it is; `-l` makes every argument text.
                let mode = p.screen().key_mode();
                p.input.push_with(|out| {
                    for key in keys {
                        match key.parse::<KeyPress>() {
                            Ok(press) if !literal => {
                                crate::encode::key_bytes(press.into(), mode, out)
                            }
                            _ => out.extend_from_slice(key.as_bytes()),
                        }
                    }
                })?;
                Ok(String::new())
            }
            &Command::CapturePane {
                target,
                history,
                json,
            } => Ok(capture(self.pane(target, here)?, history, json)),
            &Command::Terminate { target } => {
                let p = self.pane(target, here)?;
                let group = p.process.job().ok_or(Error::OnlyShell(p.id))?;
                crate::process::terminate(group).map_err(Error::Terminate)?;
                Ok(String::new())
            }
            &Command::Reorder {
                ref subject,
                toward,
            } => {
                let target = self.any_target(subject, here)?;
                self.reorder(target, toward).map(|()| String::new())
            }
            Command::Configure { argv } => {
                // `run_command` marks every view to paint, as after any
                // command that can show something new: `set titles`
                // shows at each client's next paint.
                self.config.apply(argv).map_err(Error::Config)?;
                self.rebind();
                Ok(String::new())
            }
            &Command::Reload => {
                let path = self.config_path.clone().ok_or(Error::NoConfigFile)?;
                match Config::from_file(&path) {
                    Ok(config) => {
                        self.config = config;
                        self.config_error = None;
                        self.rebind();
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
                let text = self.buffers.get(index).cloned();
                let text = text.ok_or(Error::NoBuffer(index))?;
                let p = self.pane_mut(target, here)?;
                let bracketed = p.screen().mode(fux_vt::Mode::BracketedPaste);
                p.input
                    .push_with(|out| crate::encode::paste(&text, bracketed, out))?;
                Ok(String::new())
            }
            &Command::Client { client, ref action } => {
                // The client `-c` names, else the one the command came from.
                let (client, place) = match (client, here) {
                    (Some(c), _) => (c, self.place(c).ok_or(Error::NoClient(c))?),
                    (None, Here::Client(client, place)) => (client, place),
                    (None, Here::Cli(..)) => return Err(Error::NoClientGiven),
                };
                // The client's view is the command's while it runs: out of
                // the map, so that nothing looks it up again.
                let mut view = self.views.remove(&client).ok_or(Error::NoClient(client))?;
                let done = self.on_client(&mut view, place, action);
                if let ClientAction::Detach = action {
                    self.workspaces.iter_mut().for_each(|w| w.forget(client));
                    self.outbox.push(Outgoing::Exit(client, "detached".into()));
                } else {
                    self.views.insert(client, view);
                }
                done
            }
        }
    }

    /// A command on a client's screen, its view in hand. Targets it leaves
    /// out are the client's own.
    fn on_client(
        &mut self,
        view: &mut View,
        place: Place,
        action: &ClientAction,
    ) -> Result<String, Error> {
        let here = Here::Client(view.id, place);
        match *action {
            // `execute` does not put the view back.
            ClientAction::Detach => Ok(String::new()),
            ClientAction::Capture { json } => {
                let mut grid = crate::render::Grid::new(0, 0);
                crate::render::compose_view(self, view, &mut grid, &mut Placement::default());
                Ok(capture_client(view.id, &grid, json))
            }
            ClientAction::Zoom => {
                view.zoom = !view.zoom;
                Ok(String::new())
            }
            ClientAction::SelectPane(pick) => self.select_pane(view, place, pick),
            ClientAction::SelectTab(pick) => self.select_tab(view, place, pick),
            ClientAction::SelectWorkspace(ref pick) => self.select_workspace(view, pick),
            ClientAction::CommandColumn => {
                view.mode = Mode::Column(Column::root(self));
                Ok(String::new())
            }
            ClientAction::CommandPrompt => {
                let purpose = crate::view::PromptFor::Command;
                Ok(crate::overlay::open_prompt(
                    view,
                    purpose,
                    ":".into(),
                    String::new(),
                ))
            }
            ClientAction::RenamePrompt(ref subject) => {
                let target = self.any_target(subject, here)?;
                let current = self.name_of(target);
                let title = format!("rename {} {}", subject.kind().name(), describe(target));
                let purpose = crate::view::PromptFor::Rename(target);
                Ok(crate::overlay::open_prompt(view, purpose, title, current))
            }
            ClientAction::ConfirmClose(ref subject) => {
                let target = self.any_target(subject, here)?;
                Ok(crate::overlay::open_confirm(self, view, target))
            }
            ClientAction::Menu(ref subject) => {
                let target = self.any_target(subject, here)?;
                Ok(crate::overlay::open_menu(self, view, target))
            }
            ClientAction::ChooseTab { moving } => {
                let moving = self.moving(moving, here)?;
                crate::overlay::open_tab_chooser(self, view, place, moving)
            }
            ClientAction::ChooseWorkspace { moving } => {
                let moving = self.moving(moving, here)?;
                crate::overlay::open_workspace_chooser(self, view, moving)
            }
            ClientAction::ChoosePane { target } => {
                let source = self.pane(target, here)?.id;
                crate::overlay::open_pane_chooser(self, view, source)
            }
            ClientAction::CopyMode => crate::copy::enter(self, view, place),
        }
    }

    /// The pane a chooser moves: none, the one named, or the client's.
    fn moving(&self, moving: Option<Option<PaneId>>, here: Here) -> Result<Option<PaneId>, Error> {
        moving.map(|p| self.pane(p, here).map(|p| p.id)).transpose()
    }

    pub fn name_of(&self, target: AnyRef<WsId>) -> String {
        match target {
            AnyRef::Pane(p) => self.panes.get(&p).map(|p| p.name.clone()),
            AnyRef::Tab(t) => self.tab(t).map(|t| t.name.clone()),
            AnyRef::Workspace(w) => self.workspace(w).map(|w| w.name.clone()),
        }
        .unwrap_or_default()
    }

    fn rename(&mut self, target: AnyRef<WsId>, name: String) -> Result<(), Error> {
        check_name(&name)?;
        match target {
            AnyRef::Pane(p) => {
                self.panes.get_mut(&p).ok_or(Error::NoPane(p))?.name = name;
            }
            AnyRef::Tab(t) => {
                let tab = tabs_mut(&mut self.workspaces).find(|tab| tab.id == t);
                tab.ok_or(Error::NoTab(t))?.name = name;
            }
            AnyRef::Workspace(id) => {
                // The one named, and the others, whose names it cannot take.
                let (named, others): (Vec<_>, Vec<_>) =
                    self.workspaces.iter_mut().partition(|w| w.id == id);
                let ws = named.into_iter().next().ok_or(Error::NoWorkspace(id))?;
                check_workspace_name(&name, others.iter().map(|w| &**w))?;
                ws.name = name;
            }
        }
        Ok(())
    }

    /// The pane beside `from` toward `direction`, its tab laid out as for
    /// the command.
    fn neighbor(&self, from: PaneId, direction: Direction, here: Here) -> Result<PaneId, Error> {
        let found = self.locate(from).and_then(|(_, tab)| {
            let area = self.reference_area(tab, here);
            layout::neighbor(&layout::place(self.root(tab)?, area), from, direction)
        });
        found.ok_or(Error::NoNeighbor { from, direction })
    }

    /// Swaps two panes' places: in one tab, or each in the other's.
    fn swap(&mut self, a: PaneId, b: PaneId) {
        for tab in tabs_mut(&mut self.workspaces) {
            tab.edit(|root| root.as_mut().map(|r| layout::swap(r, a, b)));
        }
    }

    /// Moves `pane` from its tab to the end of tab `to`: every tab lets go
    /// of it, and `to` takes it.
    fn relocate(&mut self, pane: PaneId, to: TabId) {
        for tab in tabs_mut(&mut self.workspaces) {
            let takes = tab.id == to;
            tab.edit(|root| {
                layout::remove(root, pane);
                if takes {
                    layout::append(root, pane);
                }
            });
        }
    }

    fn move_pane(
        &mut self,
        target: Option<PaneId>,
        to: &MoveTo,
        here: Here,
    ) -> Result<String, Error> {
        let pane = self.pane(target, here)?.id;
        let (ws, tab) = match to {
            &MoveTo::Beside(direction) => {
                let destination = self.neighbor(pane, direction, here)?;
                let side = match direction {
                    Direction::Right | Direction::Down => Side::After,
                    Direction::Left | Direction::Up => Side::Before,
                };
                for tab in tabs_mut(&mut self.workspaces) {
                    tab.edit(|root| {
                        layout::remove(root, pane);
                        layout::split(root, destination, pane, Axis::of(direction), side)
                    });
                }
                return Ok(String::new());
            }
            &MoveTo::Tab(tab) => (self.tab_workspace(tab)?, tab),
            MoveTo::Workspace(r) => {
                let mut named = self.workspaces.iter().filter(|w| r.names(w));
                let first = named.find_map(|w| Some((w.id, w.tabs().first()?.id)));
                first.ok_or_else(|| r.missing())?
            }
            MoveTo::NewTab => {
                let ws = self
                    .workspaces
                    .iter_mut()
                    .find(|w| w.tabs().iter().any(|t| t.holds(pane)));
                let ws = ws.ok_or(Error::NoPane(pane))?;
                let mut ids = self.ids;
                let id = ids.tab()?;
                ws.add_tab(id, None, None);
                self.ids = ids;
                (ws.id, id)
            }
            MoveTo::NewWorkspace => {
                // Both IDs or neither: one taken alone would be lost.
                let mut ids = self.ids;
                let id = ids.workspace()?;
                let tab = ids.tab()?;
                self.ids = ids;
                let name = self.workspace_name(id);
                let clients = self.views.keys().copied();
                (self.workspaces).push(Workspace::new(id, name, tab, None, clients));
                (id, tab)
            }
        };
        if self.locate(pane).is_some_and(|(_, source)| source == tab) {
            return Ok(String::new());
        }
        self.relocate(pane, tab);
        // The moving client follows its pane.
        self.follow(here, ws, Some(tab), Some(pane));
        Ok(format!("{tab}\n"))
    }

    fn reorder(&mut self, target: AnyRef<WsId>, toward: Sibling) -> Result<(), Error> {
        let moved = match target {
            AnyRef::Pane(p) => {
                let panes = self.locate(p).and_then(|(_, t)| self.root(t));
                let panes = panes.map_or_else(Vec::new, Tree::panes);
                let index = panes.iter().position(|x| *x == p);
                let other = index
                    .and_then(|i| sibling(i, toward))
                    .and_then(|i| panes.get(i));
                let other = other.copied().ok_or(Error::AtEnd(Kind::Pane))?;
                self.swap(p, other);
                return Ok(());
            }
            AnyRef::Tab(t) => (self.workspaces.iter_mut())
                .find_map(|w| swap_sibling(w.tabs_mut(), |tab| tab.id == t, toward))
                .ok_or(Error::NoTab(t))?,
            AnyRef::Workspace(w) => swap_sibling(&mut self.workspaces, |ws| ws.id == w, toward)
                .ok_or(Error::NoWorkspace(w))?,
        };
        moved.then_some(()).ok_or(Error::AtEnd(target.kind()))
    }

    fn select_pane(&mut self, view: &mut View, at: Place, pick: PanePick) -> Result<String, Error> {
        let client = view.id;
        let tab = self.tab(at.tab).ok_or(Error::NoTab(at.tab))?;
        let panes = tab.root().map_or_else(Vec::new, Tree::panes);
        let (current, last) = (tab.focus(client), tab.seat(client).and_then(Seat::last));
        let target = match pick {
            PanePick::Id(p) => p,
            PanePick::Step(toward) => {
                let index = current.and_then(|c| panes.iter().position(|p| *p == c));
                let next = round(&panes, index.unwrap_or(0), toward);
                *next.ok_or(Error::OnlyOne(Kind::Pane))?
            }
            PanePick::Last => last.ok_or(Error::NoLastPane)?,
            PanePick::Toward(direction) => {
                let (root, from) = tab.root().zip(current).ok_or(Error::TabEmpty)?;
                let placement = layout::place(root, Self::pane_area(view));
                layout::neighbor(&placement, from, direction)
                    .ok_or(Error::NoNeighbor { from, direction })?
            }
        };
        let (ws, tab) = self.locate(target).ok_or(Error::NoPane(target))?;
        self.show(client, ws, Some(tab), Some(target));
        view.zoom = false;
        Ok(String::new())
    }

    fn select_tab(&mut self, view: &mut View, at: Place, to: Pick<TabId>) -> Result<String, Error> {
        let (ws, target) = match to {
            Pick::Id(t) => (self.tab_workspace(t)?, t),
            Pick::Step(toward) => {
                let tabs = self.workspace(at.ws).map_or(&[][..], Workspace::tabs);
                let index = tabs.iter().position(|t| t.id == at.tab);
                let next = round(tabs, index.unwrap_or(0), toward);
                (at.ws, next.ok_or(Error::OnlyOne(Kind::Tab))?.id)
            }
        };
        self.show(view.id, ws, Some(target), None);
        view.zoom = false;
        Ok(String::new())
    }

    fn select_workspace(&mut self, view: &mut View, pick: &Pick<WsRef>) -> Result<String, Error> {
        let ws = match pick {
            Pick::Id(r) => self.resolve_ws(r)?,
            Pick::Step(toward) => {
                let workspaces = &self.workspaces;
                let index = workspaces.iter().position(|w| w.viewers.contains(&view.id));
                let next = round(workspaces, index.unwrap_or(0), *toward);
                next.ok_or(Error::OnlyOne(Kind::Workspace))?.id
            }
        };
        self.show(view.id, ws, None, None);
        view.zoom = false;
        Ok(String::new())
    }

    // ------------------------------------------------------------------ ls

    fn ls_text(&self) -> String {
        let mut out = String::new();
        for ws in &self.workspaces {
            out.push_str(&format!("{} {}\n", ws.id, ws.name));
            for tab in ws.tabs() {
                out.push_str(&format!(
                    "  {} {}{}\n",
                    tab.id,
                    tab.name,
                    if tab.root().is_none() { " (empty)" } else { "" }
                ));
                let Some(root) = tab.root() else { continue };
                root.for_each_pane(&mut |pane| {
                    if let Some(p) = self.panes.get(&pane) {
                        out.push_str(&format!(
                            "    {} {} {}x{}{}\n",
                            pane,
                            p.label(),
                            p.size().cols(),
                            p.size().rows(),
                            p.process
                                .child()
                                .map(|c| format!(" pid {}", c.leader.pid()))
                                .unwrap_or_default()
                        ));
                    }
                });
            }
        }
        let shown = |id: Option<String>| id.unwrap_or_else(|| "-".into());
        for view in self.views.values() {
            let place = self.place(view.id);
            out.push_str(&format!(
                "client {} {}x{} {} {} {}\n",
                view.id,
                view.cols,
                view.rows,
                shown(place.map(|p| p.ws.to_string())),
                shown(place.map(|p| p.tab.to_string())),
                shown(place.and_then(|p| p.pane).map(|p| p.to_string())),
            ));
        }
        out
    }

    fn ls_json(&self) -> String {
        let panes = |tab: &Tab| {
            let mut panes = Vec::new();
            let Some(root) = tab.root() else {
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
                    ("rows", Json::Number(i64::from(p.size().rows()))),
                    ("cols", Json::Number(i64::from(p.size().cols()))),
                    (
                        "pid",
                        p.process.child().map_or(Json::Null, |c| {
                            Json::Number(i64::from(c.leader.pid().as_raw()))
                        }),
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
                            ws.tabs()
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
                let place = self.place(v.id);
                let pane = place.and_then(|p| p.pane);
                Json::Object(vec![
                    ("id", Json::str(v.id.to_string())),
                    ("rows", Json::Number(i64::from(v.rows))),
                    ("cols", Json::Number(i64::from(v.cols))),
                    ("workspace", opt(place.map(|p| p.ws.to_string()))),
                    ("tab", opt(place.map(|p| p.tab.to_string()))),
                    ("pane", opt(pane.map(|p| p.to_string()))),
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

/// `$HOME`, else `/`: where a pane starts when nothing says where.
fn home() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    home.filter(|p| p.is_dir()).unwrap_or_else(|| "/".into())
}

/// `%3`, `@2` or `+1`.
pub fn describe(target: AnyRef<WsId>) -> String {
    match target {
        AnyRef::Pane(p) => p.to_string(),
        AnyRef::Tab(t) => t.to_string(),
        AnyRef::Workspace(w) => w.to_string(),
    }
}

/// A new pane running the shell, with `cmd` typed into it if given.
fn new_pane(
    config: &Config,
    launch: Option<&Path>,
    ids: &mut Ids,
    cmd: &[String],
    cwd: &Path,
    size: fux_vt::Size,
) -> Result<Pane, Error> {
    let shell_program = config
        .shell
        .first()
        .cloned()
        .unwrap_or_else(|| "/bin/sh".into());
    let fish = basename(&shell_program) == "fish";
    // The line to type is made before anything else: a line too long to
    // type must not leave a process behind.
    let typed = if cmd.is_empty() {
        None
    } else {
        let mut line = crate::words::shell_line(cmd, fish)?.into_bytes();
        line.push(b'\r');
        let deadline = crate::after(Instant::now(), TYPE_WAIT);
        Some(Typed::new(line, deadline).ok_or(Error::LineTooLong)?)
    };
    let id = ids.pane()?;
    let name = cmd
        .first()
        .map(|c| basename(c))
        .unwrap_or_else(|| basename(&shell_program));
    let mut pane = Pane::new(id, name, shell_program, size, config.history_lines)?;
    if let Some(socket) = launch {
        let env = [
            ("FUX_PANE", id.to_string()),
            ("FUX_SOCKET", socket.to_string_lossy().into_owned()),
        ];
        pane.process = Process::Reading(crate::process::spawn(&config.shell, cwd, &env, size)?);
    }
    if let Some(typed) = typed {
        pane.input = InputQueue::from(typed);
        if let Process::Absent = pane.process {
            pane.input.type_now();
        }
    }
    Ok(pane)
}

fn check_name(name: &str) -> Result<(), Error> {
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

/// Refuses a workspace name any of `others` has: a workspace is found by
/// its name.
fn check_workspace_name<'a>(
    name: &str,
    others: impl IntoIterator<Item = &'a Workspace>,
) -> Result<(), Error> {
    check_name(name)?;
    // A name a target reads back as itself, so that `-t NAME` finds this
    // workspace and no other.
    if command::parse_workspace(name) != Ok(WsRef::Name(name.to_owned())) {
        return Err(Error::NameReadAsTarget(name.to_owned()));
    }
    if others.into_iter().any(|ws| ws.name == name) {
        return Err(Error::NameTaken(name.to_owned()));
    }
    Ok(())
}

/// The item after the one at `index` among `items`, or before it, going
/// round; none if it is alone.
fn round<T>(items: &[T], index: usize, toward: Sibling) -> Option<&T> {
    let len = items.len();
    let next = match toward {
        Sibling::Next => index.checked_add(1).filter(|next| *next < len).unwrap_or(0),
        Sibling::Previous => index.checked_sub(1).unwrap_or(len.saturating_sub(1)),
    };
    items.get(next).filter(|_| len > 1)
}

/// The index after `index`, or before it.
fn sibling(index: usize, toward: Sibling) -> Option<usize> {
    match toward {
        Sibling::Next => index.checked_add(1),
        Sibling::Previous => index.checked_sub(1),
    }
}

/// Swaps the item `is` picks with its sibling `toward`: none if `is` picks
/// none, false if it has no sibling that way.
fn swap_sibling<T>(items: &mut [T], is: impl Fn(&T) -> bool, toward: Sibling) -> Option<bool> {
    let index = items.iter().position(is)?;
    let first = match toward {
        Sibling::Next => Some(index),
        Sibling::Previous => index.checked_sub(1),
    };
    let pair = first.and_then(|i| items.get_mut(i..i.checked_add(2)?));
    Some(pair.map(<[T]>::reverse).is_some())
}

/// The tab client `client` shows.
fn shown_tab(workspaces: &[Workspace], client: ClientId) -> Option<&Tab> {
    workspaces
        .iter()
        .find(|w| w.viewers.contains(&client))?
        .tab_of(client)
}

fn tabs_mut(workspaces: &mut [Workspace]) -> impl Iterator<Item = &mut Tab> {
    workspaces.iter_mut().flat_map(|w| w.tabs_mut().iter_mut())
}

/// The tab pane `id` is in: a pane in no tab is no pane.
fn tab_of(workspaces: &mut [Workspace], id: PaneId) -> Result<&mut Tab, Error> {
    tabs_mut(workspaces)
        .find(|t| t.holds(id))
        .ok_or(Error::NoPane(id))
}

/// The text of a pane's screen, and `history` lines before it.
fn capture(pane: &Pane, history: Option<usize>, json: bool) -> String {
    let screen = pane.screen();
    // History rows above the screen, oldest first, then the screen itself.
    let top = screen
        .window()
        .row(0)
        .map(|top| top.up(history.unwrap_or(0)));
    let lines = std::iter::successors(top, fux_vt::Row::below)
        .map(row_text)
        .collect();
    let cursor = Some(screen.cursor_position());
    captured(
        ("pane", pane.id.to_string()),
        screen.size().into(),
        cursor,
        lines,
        json,
    )
}

/// What a client's terminal shows, row by row, as `capture-client` prints
/// it.
fn capture_client(client: ClientId, grid: &crate::render::Grid, json: bool) -> String {
    let lines = (0..grid.rows).map(|y| grid.row_text(y)).collect();
    let size = (grid.rows, grid.cols);
    captured(
        ("client", client.to_string()),
        size,
        grid.cursor,
        lines,
        json,
    )
}

/// A capture as it is printed: its lines, or with `--json` an object naming
/// what was captured, with its size, cursor and lines.
fn captured(
    (key, id): (&'static str, String),
    (rows, cols): (u16, u16),
    cursor: Option<(u16, u16)>,
    lines: Vec<String>,
    json: bool,
) -> String {
    let mut text = if json {
        let number = |n: u16| Json::Number(i64::from(n));
        let cursor = cursor.map_or(Json::Null, |(y, x)| Json::Array(vec![number(y), number(x)]));
        let lines = Json::Array(lines.into_iter().map(Json::str).collect());
        let fields = vec![
            (key, Json::str(id)),
            ("rows", number(rows)),
            ("cols", number(cols)),
            ("cursor", cursor),
            ("lines", lines),
        ];
        Json::Object(fields).render()
    } else {
        lines.join("\n")
    };
    text.push('\n');
    text
}

/// A row's text, wide glyphs whole and trailing blanks trimmed.
pub fn row_text(row: fux_vt::Row<'_>) -> String {
    let row = row.cells().filter(|c| !c.is_wide_continuation());
    let line: String = row.map(crate::render::shown).collect();
    line.trim_end_matches(' ').to_owned()
}

/// What the other modules' tests share: a session started with the default
/// config, a client attached, and a command line run in it.
#[cfg(test)]
pub(crate) mod testing {
    use super::{Origin, Session};
    use crate::config::Config;
    use crate::id::ClientId;

    /// A session of `config`, but for its shell, `/bin/sh` (each pane is
    /// named after it), started.
    pub(crate) fn started(config: Config) -> Result<Session, String> {
        let config = Config {
            shell: vec!["/bin/sh".into()],
            ..config
        };
        let mut session = Session::new(config, "/nonexistent/fux.sock".into(), false);
        session.start().map_err(|e| e.to_string())?;
        Ok(session)
    }

    /// A session started with the default config, and a client of `rows`
    /// by `cols` attached.
    pub(crate) fn attached(rows: u16, cols: u16) -> Result<(Session, ClientId), String> {
        let mut session = started(Config::default())?;
        let client = session
            .attach(rows, cols, None)
            .map_err(|e| e.to_string())?;
        Ok((session, client))
    }

    /// Runs `line` as a command from no client: what it printed, or its
    /// error if it fails.
    pub(crate) fn output(session: &mut Session, line: &str) -> Result<String, String> {
        let words = crate::words::split(line).map_err(|e| e.to_string())?;
        let outcome = session.run(&words, &Origin::default());
        if outcome.status == 0 {
            Ok(outcome.stdout)
        } else {
            Err(outcome.stderr)
        }
    }

    /// Runs `line` as a command from no client; its error, if it fails.
    pub(crate) fn run(session: &mut Session, line: &str) -> Result<(), String> {
        output(session, line).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{attached, output, run, started};
    use super::*;

    /// The `--json` outputs, byte for byte: escapes, borrowed names and
    /// formatted IDs alike.
    #[test]
    fn json_outputs_are_what_they_were() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(4, 30)?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, r#"rename -t %1 'say "hi" \ 界'"#)?;
        run(&mut s, "new-tab -t +1 -n two")?;
        run(&mut s, "select-tab -c c1 -t @1")?;
        s.output(
            PaneId::of(2),
            "a\tb \"q\" 界\x1b]2;tab\\title\x07".as_bytes(),
        );
        assert_eq!(
            output(&mut s, "ls --json")?,
            concat!(
                r#"{"workspaces":[{"id":"+1","name":"main","tabs":[{"id":"@1","name":"main","panes":[{"id":"%1","name":"say \"hi\" \\ 界","title":"","rows":3,"cols":15,"pid":null},{"id":"%2","name":"sh","title":"tab\\title","rows":3,"cols":14,"pid":null}]},{"id":"@2","name":"two","panes":[{"id":"%3","name":"sh","title":"","rows":24,"cols":80,"pid":null}]}]}],"clients":[{"id":"c1","rows":4,"cols":30,"workspace":"+1","tab":"@1","pane":"%2","zoom":false}]}"#,
                "\n"
            )
        );
        assert_eq!(
            output(&mut s, "capture-pane -t %2 --json")?,
            concat!(
                r#"{"pane":"%2","rows":3,"cols":14,"cursor":[1,2],"lines":["a       b \"q\"","界",""]}"#,
                "\n"
            )
        );
        assert_eq!(
            output(&mut s, "capture-client -c c1 --json")?,
            concat!(
                r#"{"client":"c1","rows":4,"cols":30,"cursor":[1,18],"lines":["               │a       b \"q\"","               │界","               │"," main  main  two %2 tab\\title"]}"#,
                "\n"
            )
        );
        Ok(())
    }

    /// A pane's exit status reaches those who saw it, whether or not its
    /// tab closes with it.
    #[test]
    fn an_exit_is_told_even_when_it_closes_the_tab() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, client) = attached(10, 40)?;
        let notice = |s: &Session| {
            s.views
                .get(&client)
                .and_then(|v| v.notice.as_ref())
                .map(|n| n.text.clone())
        };
        // Two panes: the tab stays.
        run(&mut s, "split -h -t %1")?;
        s.exited(PaneId::of(2), 3);
        assert_eq!(notice(&s).as_deref(), Some("%2 sh exited with status 3"));
        // A second tab, shown; its only pane exits, and the tab with it.
        run(&mut s, "new-tab -t +1")?;
        run(&mut s, "select-tab -c c1 -t @2")?;
        s.exited(PaneId::of(3), 7);
        assert_eq!(notice(&s).as_deref(), Some("%3 sh exited with status 7"));
        Ok(())
    }

    /// Workspaces are found by name, so no two share one: a name given is
    /// refused if taken, and a name made up skips those taken. Making one
    /// that fails part way uses up none of the IDs it took.
    #[test]
    fn workspace_names_stay_unique_and_failures_use_no_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut s = started(Config::default())?;
        assert!(run(&mut s, "new-workspace -n main").is_err());
        assert_eq!(output(&mut s, "new-workspace -n workspace-3")?, "+2\n");
        // +3 would be workspace-3, which is taken.
        assert_eq!(output(&mut s, "new-workspace")?, "+3\n");
        let names: Vec<&str> = s.workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["main", "workspace-3", "workspace-4"]);
        // A move to a new workspace with no tab ID left takes no workspace
        // ID either.
        s.ids.exhaust_tabs();
        let ids = s.ids;
        assert!(run(&mut s, "move-pane -t %1 --to new-workspace").is_err());
        assert_eq!(s.ids, ids);
        Ok(())
    }

    /// Only screens that may have changed are repainted: keys typed into a
    /// pane repaint the typist's screen alone (the program's output
    /// repaints its viewers when it comes), commands that only read repaint
    /// none, and a command that changes the layout repaints every client.
    #[test]
    fn only_screens_that_may_change_are_marked_for_painting()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, one) = attached(10, 40)?;
        let two = s.attach(10, 40, None)?;
        let clean = |s: &mut Session| {
            for view in s.views.values_mut() {
                view.dirty = false;
            }
        };
        let dirty = |s: &Session, c: ClientId| s.views.get(&c).is_some_and(|v| v.dirty);
        clean(&mut s);
        // Keys for the pane's program show when it answers: its output
        // marks every screen showing the pane.
        s.input(one, b"ls");
        assert!(
            !dirty(&s, one) && !dirty(&s, two),
            "typing into a pane repaints no one until the pane answers"
        );
        s.output(crate::id::PaneId::of(1), b"ls");
        assert!(dirty(&s, one) && dirty(&s, two), "its echo repaints both");
        clean(&mut s);
        // A key that changes the typist's own screen repaints it alone.
        s.input(one, b"\x02");
        assert!(
            dirty(&s, one) && !dirty(&s, two),
            "the prefix repaints the typist alone"
        );
        s.input(one, b"\x1b");
        s.escape(one);
        clean(&mut s);
        for line in ["ls", "capture-pane -t %1", "list-keys", "send-keys -t %1 x"] {
            run(&mut s, line)?;
        }
        assert!(
            !dirty(&s, one) && !dirty(&s, two),
            "reading repaints no one"
        );
        assert!(s.run(&["nope".into()], &Origin::default()).status != 0);
        assert!(
            !dirty(&s, one) && !dirty(&s, two),
            "a usage error repaints no one"
        );
        run(&mut s, "split -h -t %1")?;
        assert!(
            dirty(&s, one) && dirty(&s, two),
            "a split repaints every client"
        );
        clean(&mut s);
        // A binding that splits, typed by one client, repaints both.
        s.input(one, b"\x02v");
        assert!(
            dirty(&s, one) && dirty(&s, two),
            "a bound split repaints every client"
        );
        Ok(())
    }

    /// Output asks for a settle only where it can change a view: in a pane
    /// a copy mode holds rows of, whose history it may drop. There, the
    /// settle that follows ends copy mode when the rows go.
    #[test]
    fn output_asks_for_a_settle_only_under_copy_mode() -> Result<(), Box<dyn std::error::Error>> {
        let mut s = started(Config {
            history_lines: 2,
            ..Config::default()
        })?;
        let client = s.attach(6, 20, None)?;
        s.output(PaneId::of(1), b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        assert!(!s.unsettled(), "no copy mode: nothing to repair");
        run(&mut s, "copy-mode -c c1")?;
        // To the oldest row, which the next lines of output push out.
        s.input(client, b"g");
        s.output(PaneId::of(1), b"g\r\nh\r\ni\r\nj\r\n");
        assert!(s.unsettled());
        s.settle_if_needed();
        assert!(!s.unsettled());
        let view = s.views.get(&client).ok_or("the view")?;
        assert!(matches!(view.mode, Mode::Normal), "copy mode ended");
        assert!(
            view.notice
                .as_ref()
                .is_some_and(|n| n.text.contains("dropped the rows")),
            "and said why"
        );
        Ok(())
    }

    /// A client shrinking the pane pushes rows into its history, which may
    /// drop the ones copy mode holds: copy mode ends in the same settle, so
    /// no paint shows it without them.
    #[test]
    fn copy_mode_ends_when_sizing_its_pane_drops_its_rows() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut s = started(Config {
            history_lines: 2,
            ..Config::default()
        })?;
        let client = s.attach(6, 20, None)?;
        s.output(PaneId::of(1), b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        run(&mut s, "copy-mode -c c1")?;
        s.input(client, b"g");
        s.resize(client, 3, 20);
        let view = s.views.get(&client).ok_or("the view")?;
        assert!(matches!(view.mode, Mode::Normal), "copy mode ended");
        Ok(())
    }

    /// A client's size is kept at most 4096 each way, and only a size that
    /// changes it resizes and repaints it: the same size again, or a larger
    /// terminal's, changes nothing.
    #[test]
    fn only_a_size_that_changes_a_client_resizes_it() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = attached(10, 40)?;
        for (rows, changes) in [
            (10, false),
            (5000, true),
            (5000, false),
            (4096, false),
            (3, true),
        ] {
            if let Some(view) = s.views.get_mut(&c) {
                view.dirty = false;
            }
            let resized = s.resize(c, rows, 40);
            let dirty = s.views.get(&c).is_some_and(|v| v.dirty);
            assert_eq!((resized, dirty), (changes, changes), "{rows} rows");
        }
        Ok(())
    }

    /// A failing command says what it said as a string, word for word:
    /// each line, run in turn on one session, and its status and message,
    /// as they were before commands failed with `Error`.
    #[test]
    fn command_errors_keep_their_words() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
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
            (
                "reorder workspace -t nope --next",
                1,
                r#"no workspace named "nope""#,
            ),
            (
                "menu -c c1 workspace -t nope",
                1,
                r#"no workspace named "nope""#,
            ),
            (
                "zoom",
                1,
                "this command acts on a client's screen: use -c CLIENT (`fux ls` lists clients)",
            ),
            ("zoom -c c9", 1, "no client c9"),
            ("show-buffer", 1, "no buffer 0"),
            ("paste-buffer -t %1", 1, "no buffer 0"),
            // A workspace's name is what targets read as a name.
            (
                "new-workspace -n @logs",
                1,
                r#""@logs" cannot name a workspace: a target would read it as a pane, tab or workspace number"#,
            ),
            (
                "rename -t +1 +2",
                1,
                r#""+2" cannot name a workspace: a target would read it as a pane, tab or workspace number"#,
            ),
            (
                "rename -t +1 %x",
                1,
                r#""%x" cannot name a workspace: a target would read it as a pane, tab or workspace number"#,
            ),
            (
                "rename -t +1 -- -x",
                1,
                r#""-x" cannot name a workspace: a target would read it as a pane, tab or workspace number"#,
            ),
            ("reload", 1, "no config file to reload"),
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
                "unknown option nope; options are prefix, shell, history-lines, clipboard, buffers, bell, titles",
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
            (
                "new-workspace -n main",
                1,
                r#"another workspace is named "main""#,
            ),
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
            let outcome = s.run(&crate::words::split(line)?, &Origin::default());
            assert_eq!(
                (outcome.status, outcome.stderr.as_str()),
                (status, message),
                "{line}"
            );
        }
        let argv = ["split", "-h", "-t", "%1", "--", "a\nb"].map(str::to_owned);
        let outcome = s.run(&argv, &Origin::default());
        assert_eq!(
            (outcome.status, outcome.stderr.as_str()),
            (
                1,
                r#"the command argument "a\nb" contains the control character '\n'; it would act as a key in the shell"#
            )
        );
        // Why a menu entry or a binding cannot run now.
        let client = "c1".parse()?;
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
            let got = s.unavailable(&command, client).map(|e| e.to_string());
            assert_eq!(got.as_deref(), reason, "{line}");
        }
        Ok(())
    }

    /// What a caller tells apart, it tells by variant.
    #[test]
    fn failures_are_told_apart_by_kind() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = attached(10, 40)?;
        let origin = Origin::Client(c);
        let mut run = |line: &str| -> Result<Result<String, Error>, Box<dyn std::error::Error>> {
            let command = command::parse(&crate::words::split(line)?)?;
            Ok(s.execute(&command, &origin))
        };
        assert!(matches!(
            run("kill-pane -t %9")?,
            Err(Error::NoPane(p)) if p == PaneId::of(9)
        ));
        assert!(matches!(
            run("select-pane --next")?,
            Err(Error::OnlyOne(Kind::Pane))
        ));
        assert!(matches!(run("set nope 1")?, Err(Error::Config(_))));
        assert!(run("copy-mode")?.is_ok());
        assert!(matches!(
            s.attach(10, 40, Some("%1")),
            Err(Error::Usage(command::Usage::Not(Kind::Workspace, _)))
        ));
        assert!(matches!(
            s.attach(10, 40, Some("nope")),
            Err(Error::NoWorkspaceNamed(_))
        ));
        Ok(())
    }
}

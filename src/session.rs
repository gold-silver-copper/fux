//! The server's state: workspaces, tabs, panes and the clients' views, and
//! every command that changes them.
use crate::command::{
    self, AnyRef, ClientAction, Command, Extent, Input, Kind, Mouse, MoveTo, PanePick, PaneResize,
    Pick, Resized, Sibling, Subject, SwapWith, WsRef,
};
use crate::config::Config;
use crate::copy::MAX_CELLS;
use crate::id::{ClientId, Ids, PaneId, TabId, WsId};
use crate::json::Json;
use crate::keys::{Direction, KeyPress};
use crate::layout::{self, Axis, Placement, Rect, Side, Tree};
use crate::overlay::{Column, Repeat, pane_chooser};
use crate::pane::{InputQueue, Pane, Process, Runs, Typed};
use crate::process::Child;
use crate::view::{Choice, ClientKind, Mode, View};
use crate::workspace::{Fit, Seat, Tab, Workspace};
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
    /// A pane's paced input goes on (`Pane::pace_due`).
    Pace(PaneId),
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

/// Where a select command takes a client: a workspace, and a tab and pane.
type Picked = (WsId, Option<TabId>, Option<PaneId>);

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
    /// The pane's program has exited: it takes no input.
    Exited {
        pane: PaneId,
        status: crate::process::Ended,
    },
    /// The pane's program has not asked for mouse reports.
    NoMouse(PaneId),
    /// No way of moving the pane's borders makes it this size.
    CannotResize {
        pane: PaneId,
        cols: u16,
        rows: u16,
    },
    /// A command that waits, from where nothing can wait for it.
    WaitsOnlyFromCli,
    /// A client with a terminal, which says its size itself.
    NotHeadless(ClientId),
    NotADirectory(PathBuf),
    /// A file `run --log` cannot make.
    Log(PathBuf, std::io::Error),
    /// A signal `fux signal` could not send.
    Signal(fuxix::Errno),
    /// A pane `respawn` cannot start again: a shell's, or a program that
    /// still runs.
    NotRun(PaneId),
    StillRunning(PaneId),
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
            Error::Exited { pane, status } => {
                write!(f, "the program in {pane} {}", crate::process::told(*status))
            }
            Error::NoMouse(pane) => {
                write!(f, "the program in {pane} has not asked for mouse reports")
            }
            Error::CannotResize { pane, cols, rows } => {
                write!(f, "{pane} cannot be made {cols}x{rows} in its tab")
            }
            Error::WaitsOnlyFromCli => f.write_str("wait runs from the command line"),
            Error::NotHeadless(client) => write!(
                f,
                "{client} has a terminal, which sets its size; resize sizes headless clients"
            ),
            Error::NotADirectory(path) => write!(f, "{} is not a directory", path.display()),
            Error::Log(path, error) => write!(f, "{}: {error}", path.display()),
            Error::Signal(error) => write!(f, "sending the signal: {error}"),
            Error::NotRun(pane) => write!(
                f,
                "{pane} runs a shell; respawn starts again what `fux run` started"
            ),
            Error::StillRunning(pane) => {
                write!(f, "the program in {pane} still runs; -k ends it first")
            }
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
            | Error::NoConfigFile
            | Error::NoLastPane
            | Error::NoPaneToCopy
            | Error::TabEmpty
            | Error::OnlyOne(_)
            | Error::AtEnd(_)
            | Error::NoBorder { .. }
            | Error::NoNeighbor { .. }
            | Error::OnlyShell(_)
            | Error::Exited { .. }
            | Error::NoMouse(_)
            | Error::CannotResize { .. }
            | Error::WaitsOnlyFromCli
            | Error::NotHeadless(_)
            | Error::NotADirectory(_)
            | Error::Log(..)
            | Error::Signal(_)
            | Error::NotRun(_)
            | Error::StillRunning(_)
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

/// What a command line from the CLI begins: its outcome, now, or a wait
/// for one (`wait`, `send-keys --wait-idle`).
pub enum Begun {
    Done(Outcome),
    Waiting(Box<crate::wait::Waiting>),
}

/// How long a closing pane's processes have to exit after the hangup.
pub const GRACE: Duration = Duration::from_millis(100);
/// The longest a typed command waits for the new shell to write and then
/// go quiet (`pane::QUIET`): a shell that writes nothing still gets it.
pub const TYPE_WAIT: Duration = Duration::from_secs(1);
/// The size a pane starts at, until it is laid out on a client's screen.
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
}

/// The name of the first workspace, and of each workspace's first tab.
pub(crate) const MAIN: &str = "main";

/// The workspace `fux run` starts programs in unless told another.
pub const RUN_WORKSPACE: &str = "agent";

/// What a command that makes a pane prints: `text`, the IDs it names; or
/// with `--json`, where the pane is.
fn made_ids((ws, tab, pane): (WsId, TabId, PaneId), text: &[String], json: bool) -> String {
    let mut out = if json {
        Json::Object(vec![
            ("workspace", Json::str(ws.to_string())),
            ("tab", Json::str(tab.to_string())),
            ("pane", Json::str(pane.to_string())),
        ])
        .render()
    } else {
        text.join(" ")
    };
    out.push('\n');
    out
}

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
            | Command::Signal { .. }
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
        }
    }

    /// One workspace, holding one tab with one shell.
    pub fn start(&mut self) -> Result<(), Error> {
        let home = home();
        let start = (Start::Shell(&[], &[]), home.as_path());
        self.create_workspace(Some(MAIN.into()), start, (Fit::Clients, default_size()))
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
        match r {
            WsRef::Id(id) => self.workspace(*id).ok_or(Error::NoWorkspace(*id)),
            WsRef::Name(name) => (self.workspaces.iter().find(|w| w.name == *name))
                .ok_or_else(|| Error::NoWorkspaceNamed(name.clone())),
        }
        .map(|w| w.id)
    }
    pub fn exists(&self, what: AnyRef<WsId>) -> bool {
        match what {
            AnyRef::Pane(p) => self.panes.contains_key(&p),
            AnyRef::Tab(t) => self.tab(t).is_some(),
            AnyRef::Workspace(w) => self.workspace(w).is_some(),
        }
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

    /// Starts the program pane `id` ran again in it: its words, or `argv`
    /// if given, where it ran, with what it was given and `env` set; first
    /// hanging up the one there with `kill`, else refused while it runs.
    fn respawn(
        &mut self,
        id: PaneId,
        kill: bool,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<(), Error> {
        let pane = self.panes.get(&id).ok_or(Error::NoPane(id))?;
        let Runs::Program(launch) = &pane.runs else {
            return Err(Error::NotRun(id));
        };
        if pane.process.child().is_some() && !kill {
            return Err(Error::StillRunning(id));
        }
        let mut launch = launch.clone();
        if !argv.is_empty() {
            launch.argv = argv.to_vec();
        }
        for (key, value) in env {
            launch.env.retain(|(k, _)| k != key);
            launch.env.push((key.clone(), value.clone()));
        }
        let size = pane.size();
        let mut vars = Vec::new();
        if let Some(socket) = &self.launch {
            vars.push(("FUX_PANE", id.to_string()));
            vars.push(("FUX_SOCKET", socket.to_string_lossy().into_owned()));
        }
        vars.extend(launch.env.iter().map(|(k, v)| (k.as_str(), v.clone())));
        let child = match &self.launch {
            Some(socket) => {
                let (argv, marks) = with_marks(&self.config, socket, &launch.argv, &launch.env);
                vars.extend(marks.iter().map(|(k, v)| (k.as_str(), v.clone())));
                Some(crate::process::spawn(&argv, &launch.cwd, &vars, size)?)
            }
            None => None,
        };
        let history = self.config.history_lines;
        let Some(pane) = self.panes.get_mut(&id) else {
            return Err(Error::NoPane(id));
        };
        let old = pane.respawned(launch, child.map(Process::Reading), history)?;
        if let Process::Reading(child) | Process::HungUp(child) = old {
            child.leader.hang_up();
            let deadline = crate::after(Instant::now(), GRACE);
            self.dying.insert(id, Dying { child, deadline });
        }
        self.touch();
        Ok(())
    }

    /// The panes of `tabs`, as ids, in their order.
    fn panes_of_tabs(&self, tabs: &[TabId]) -> Vec<String> {
        (self.workspaces.iter())
            .flat_map(Workspace::tabs)
            .filter(|t| tabs.contains(&t.id))
            .flat_map(|t| t.root().map(Tree::panes).unwrap_or_default())
            .map(|p| p.to_string())
            .collect()
    }

    /// Whether a client is in copy mode on pane `pane`: keys typed there
    /// go to copy mode, not the program.
    fn copying(&self, pane: PaneId) -> bool {
        self.views
            .values()
            .any(|v| matches!(&v.mode, Mode::Copy(copy) if copy.pane == pane))
    }

    /// `pane_mut`, for input: a pane whose program has exited takes none.
    fn live_pane_mut(&mut self, explicit: Option<PaneId>, here: Here) -> Result<&mut Pane, Error> {
        let pane = self.pane_mut(explicit, here)?;
        match pane.process.exit_status() {
            Some(status) => Err(Error::Exited {
                pane: pane.id,
                status,
            }),
            None => Ok(pane),
        }
    }

    /// The size a pane starts at in a new tab laid out on `fit`.
    fn start_size(&self, fit: Fit, here: Here) -> fux_vt::Size {
        let area = self.tab_area(fit, here);
        crate::pane::size(area.h(), area.w())
    }

    /// The size a pane split from `target` along `axis` starts at: its
    /// place in the layout once split.
    fn split_size(&self, target: PaneId, axis: Axis, here: Here) -> Result<fux_vt::Size, Error> {
        let (_, tab) = self.locate(target).ok_or(Error::NoPane(target))?;
        let area = self.reference_area(tab, here);
        let mut ids = self.ids;
        let new = ids.pane()?;
        let mut root = self.root(tab).cloned().ok_or(Error::NoPane(target))?;
        root.split(target, new, axis, Side::After);
        let rect = layout::place(&root, area).rect(new);
        Ok(rect.map_or(default_size(), |r| crate::pane::size(r.h(), r.w())))
    }

    /// Resizes `pane`, placed in `area`, to `cols` and `rows`, either or
    /// both; nothing changes unless both can be met.
    fn resize_pane_to(
        &mut self,
        pane: PaneId,
        area: Rect,
        (cols, rows): (Option<Extent>, Option<Extent>),
    ) -> Result<String, Error> {
        let cells = |extent: Extent, of: u16| match extent {
            Extent::Cells(n) => n,
            Extent::Percent(p) => {
                let share = u32::from(of).saturating_mul(u32::from(p)).checked_div(100);
                share
                    .and_then(|n| u16::try_from(n).ok())
                    .unwrap_or(of)
                    .max(1)
            }
        };
        let (_, tab) = self.locate(pane).ok_or(Error::NoPane(pane))?;
        let mut root = self.root(tab).cloned().ok_or(Error::NoPane(pane))?;
        let now = layout::place(&root, area)
            .rect(pane)
            .ok_or(Error::NoPane(pane))?;
        let want = (
            cols.map_or(now.w(), |c| cells(c, area.w())),
            rows.map_or(now.h(), |r| cells(r, area.h())),
        );
        let refused = || Error::CannotResize {
            pane,
            cols: want.0,
            rows: want.1,
        };
        for (axis, len, asked) in [
            (Axis::Horizontal, want.0, cols),
            (Axis::Vertical, want.1, rows),
        ] {
            if asked.is_some() {
                root = layout::resize_to(&root, area, pane, axis, len).ok_or_else(refused)?;
            }
        }
        // A width moved by the height's borders is not met.
        let got = layout::place(&root, area)
            .rect(pane)
            .map(|r| (r.w(), r.h()));
        if got != Some(want) {
            return Err(refused());
        }
        tab_of(&mut self.workspaces, pane)?.edit(|old| *old = Some(root));
        Ok(format!("{pane} {}x{}\n", want.0, want.1))
    }

    fn tab_target(&self, explicit: Option<TabId>, here: Here) -> Result<TabId, Error> {
        let id = explicit.or(here.place().map(|p| p.tab));
        let id = id.ok_or(Error::NoTabGiven)?;
        self.tab(id).map(|t| t.id).ok_or(Error::NoTab(id))
    }

    fn ws_target(&self, explicit: Option<&WsRef>, here: Here) -> Result<WsId, Error> {
        match explicit {
            Some(r) => self.resolve_ws(r),
            None => here.place().map(|p| p.ws).ok_or(Error::NoWorkspaceGiven),
        }
    }

    /// What `subject` names, or the command's own, if it is there: a
    /// workspace by its ID, as it is held from then on.
    fn any_target(&self, subject: &Subject, here: Here) -> Result<AnyRef<WsId>, Error> {
        Ok(match subject {
            Subject::Pane(p) => AnyRef::Pane(self.pane(*p, here)?.id),
            Subject::Tab(t) => AnyRef::Tab(self.tab_target(*t, here)?),
            Subject::Workspace(w) => AnyRef::Workspace(self.ws_target(w.as_ref(), here)?),
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

    /// A workspace with one tab, laid out on `fit`, holding one pane that
    /// starts what `start` says, `size` to begin with.
    fn create_workspace(
        &mut self,
        name: Option<String>,
        (start, cwd): (Start<'_>, &Path),
        (fit, size): (Fit, fux_vt::Size),
    ) -> Result<(WsId, TabId, PaneId), Error> {
        if let Some(name) = &name {
            check_workspace_name(name, &self.workspaces)?;
        }
        // The IDs are taken before the pane starts, so that none can run out
        // after, and are committed once it has.
        let mut ids = self.ids;
        let id = ids.workspace()?;
        let tab = ids.tab()?;
        let launch = self.launch.as_deref();
        let pane = new_pane(&self.config, launch, &mut ids, start, cwd, size)?;
        self.ids = ids;
        let name = name.unwrap_or_else(|| self.workspace_name(id));
        let made = (id, tab, pane.id);
        let root = Some(Tree::Pane(pane.id));
        let clients = self.views.keys().copied();
        (self.workspaces).push(Workspace::new(id, name, tab, (root, fit), clients));
        self.panes.insert(pane.id, pane);
        Ok(made)
    }

    /// Adds a tab to workspace `ws`, laid out on `fit`, holding one pane that
    /// starts what `start` says, `size` to begin with.
    fn create_tab(
        &mut self,
        ws: WsId,
        name: Option<String>,
        (start, cwd): (Start<'_>, &Path),
        (fit, size): (Fit, fux_vt::Size),
    ) -> Result<(TabId, PaneId), Error> {
        if let Some(name) = &name {
            check_name(name)?;
        }
        let workspace = self.workspaces.iter_mut().find(|w| w.id == ws);
        let workspace = workspace.ok_or(Error::NoWorkspace(ws))?;
        // As for a workspace: the ID first, committed after the pane.
        let mut ids = self.ids;
        let id = ids.tab()?;
        let launch = self.launch.as_deref();
        let pane = new_pane(&self.config, launch, &mut ids, start, cwd, size)?;
        self.ids = ids;
        let made = (id, pane.id);
        workspace.add_tab(id, name, (Some(Tree::Pane(pane.id)), fit));
        self.panes.insert(pane.id, pane);
        Ok(made)
    }

    // ------------------------------------------------------------- clients

    /// A client attaches, viewing `workspace` or the first one.
    pub fn attach(
        &mut self,
        rows: u16,
        cols: u16,
        workspace: Option<&str>,
    ) -> Result<ClientId, Error> {
        self.attach_as(ClientKind::Terminal, rows, cols, workspace)
    }

    /// `attach`, for a client of `kind`.
    pub fn attach_as(
        &mut self,
        kind: ClientKind,
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
        view.kind = kind;
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

    /// The area to lay out a tab in for a command: a client's showing it,
    /// else what its fit says (`tab_area`).
    fn reference_area(&self, tab: TabId, here: Here) -> Rect {
        let own = match here {
            Here::Client(client, _) => self.views.get(&client),
            Here::Cli(..) => None,
        };
        let shows = |v: &&View| self.shows(v.id, tab);
        match own
            .filter(shows)
            .or_else(|| self.views.values().find(shows))
        {
            Some(view) => Self::pane_area(view),
            None => self.tab_area(self.tab(tab).map_or(Fit::Clients, Tab::fit), here),
        }
    }

    /// The area a tab no client shows is laid out in on `fit`: a fixed
    /// size's, else the commanding client's screen, else any client's, else
    /// a typical terminal's.
    fn tab_area(&self, fit: Fit, here: Here) -> Rect {
        let own = match here {
            Here::Client(client, _) => self.views.get(&client),
            Here::Cli(..) => None,
        };
        let client = own.or_else(|| self.views.values().next());
        (fit.area(client.map(Self::pane_area)))
            .unwrap_or(Rect::screen(DEFAULT_SIZE.0, DEFAULT_SIZE.1))
    }

    // -------------------------------------------------------------- repair

    /// Ends each view's mode if what it shows is gone, then sizes the PTYs:
    /// run after every event.
    pub fn settle(&mut self) {
        // Out of the session while they are repaired, which reads no view.
        let mut views = std::mem::take(&mut self.views);
        views.values_mut().for_each(|view| self.repair(view));
        self.views = views;
        self.size_panes();
        self.hold_copies();
    }

    /// Output and sizing a pane can drop rows of its history: a copy mode
    /// whose rows went ends. They are looked for only if the screen changed
    /// since they were last found. Every change to workspaces, tabs, panes
    /// and views does this as it is made; output, which changes none of
    /// them, leaves it to the server's loop.
    pub fn hold_copies(&mut self) {
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
    /// The configuration changed: the bindings follow it, and every pane
    /// keeps the history it says. A pane whose history cannot be that long
    /// at its size keeps its own, as a new pane of that size would fail.
    fn configured(&mut self) {
        self.rebind();
        let lines = self.config.history_lines;
        for pane in self.panes.values_mut() {
            let _ = pane.parser.set_history_lines(lines);
        }
    }

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

    /// A PTY is the smallest rectangle any client shows it in. A tab no
    /// client shows whose layout changed is laid out on what its fit says,
    /// as commands resizing it are; otherwise its panes keep their size, so
    /// leaving a tab resizes nothing.
    fn size_panes(&mut self) {
        let mut sizes = std::mem::take(&mut self.shown_sizes);
        let mut placed = std::mem::take(&mut self.placed);
        sizes.clear();
        for view in self.views.values() {
            self.placement_into(view, &mut placed);
            sizes.extend(placed.panes.iter().map(|(p, r)| (*p, (r.h(), r.w()))));
        }
        let client = self.views.values().next().map(Self::pane_area);
        let changed = (self.workspaces.iter_mut()).flat_map(|w| w.changed_unshown(client));
        for (root, area) in changed {
            layout::place_into(root, area, &mut placed);
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

    /// A pane's program exited with `status`: a shell's pane closes, and a
    /// program's stays, the status kept, for its last screen to be read.
    pub fn exited(&mut self, id: PaneId, ended: crate::process::Ended) {
        let status = crate::process::shell_status(ended);
        // What a program wrote as it exited may still be on its terminal,
        // unread: its last screen is read whole before it is kept.
        if self
            .panes
            .get(&id)
            .is_some_and(|p| matches!(p.runs, Runs::Program(_)))
        {
            let left = self
                .panes
                .get(&id)
                .map(|p| unread(&p.process))
                .unwrap_or_default();
            if !left.is_empty() {
                self.output(id, &left);
            }
        }
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        match pane.runs {
            Runs::Shell => self.close_pane(id, Some(format!("exited with status {status}"))),
            Runs::Program(_) => {
                // Its leader, exited and unreaped, is reaped with the dying.
                let gone = std::mem::replace(&mut pane.process, Process::Exited(ended));
                if let Process::Reading(child) | Process::HungUp(child) = gone {
                    let deadline = Instant::now();
                    self.dying.insert(id, Dying { child, deadline });
                }
                let place = self.locate(id);
                let viewers: Vec<ClientId> = (self.views.keys().copied())
                    .filter(|c| place.is_some_and(|(_, t)| self.shows(*c, t)))
                    .collect();
                for client in viewers {
                    self.info_to(client, format!("{id} {}", crate::process::told(ended)));
                }
                self.touch();
            }
        }
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
            let paced = pane.pace_due().map(|at| (at, Timer::Pace(pane.id)));
            typed.into_iter().chain(frame).chain(paced)
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
            Timer::Pace(id) => (self.panes.get_mut(&id))
                .into_iter()
                .for_each(|p| p.release_paced(Instant::now())),
        }
    }

    /// Reads into pane `id` with `read`, as every read of a pane's output
    /// does: its tab's host colours and palette given it first, what
    /// follows a read seen to after (`read_into`).
    fn read_with(&mut self, id: PaneId, read: impl FnOnce(&mut Pane)) {
        let place = self.locate(id);
        let colours = self.colours_for(place.map(|(_, t)| t));
        let palette = self.palette_for(place.map(|(_, t)| t));
        let graphics = self.graphics_for(place.map(|(_, t)| t));
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        pane.colours = colours;
        pane.set_host_palette(palette);
        pane.parser.set_host_graphics(graphics);
        read(pane);
        pane.note_change(Instant::now());
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
        // A program's copy, as a copy in copy mode, to the clients showing
        // it; none with `set clipboard off`.
        let copied = self.panes.get_mut(&id).and_then(|p| p.copied.take());
        if let Some(text) = copied.filter(|_| self.config.clipboard) {
            let to: Vec<ClientId> = (self.views.keys().copied())
                .filter(|&c| place.is_some_and(|(_, tab)| self.shows(c, tab)))
                .collect();
            self.copied(text, &to);
        }
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

    /// `text`, copied, goes to the paste buffers and, unless `set clipboard
    /// off`, to the clipboard of each client in `to` (OSC 52): false if it
    /// is too large for the clipboard.
    pub(crate) fn copied(&mut self, text: String, to: &[ClientId]) -> bool {
        let mut fits = true;
        // Encoded for the clipboard before the text moves into the buffers.
        if self.config.clipboard {
            let encoded = crate::json::base64(text.as_bytes());
            fits = encoded.len() <= crate::copy::MAX_CLIPBOARD;
            if fits {
                let osc = format!("\x1b]52;c;{encoded}\x07").into_bytes();
                for &client in to {
                    self.outbox.push(Outgoing::Bytes(client, osc.clone()));
                }
            }
        }
        self.buffers.push_front(text);
        self.buffers.truncate(self.config.buffers);
        fits
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
        let argv = &self.named(argv);
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

    /// Begins a command line from the CLI: one that waits gives its wait,
    /// which the server asks for its answer until it has one; any other
    /// runs as `run` runs it.
    pub fn begin(&mut self, argv: &[String], origin: &Origin) -> Begun {
        let argv = self.named(argv);
        let failed = |error: Error| {
            Begun::Done(Outcome {
                status: 1,
                stdout: String::new(),
                stderr: error.to_string(),
            })
        };
        let (targets, wait, keys) = match command::parse(&argv) {
            Ok(Command::Wait { targets, wait }) => (targets, wait, None),
            Ok(command) if command.acting_wait().is_some() => {
                let Some((target, wait)) = command.acting_wait() else {
                    return Begun::Done(self.run(&argv, origin));
                };
                (target.into_iter().collect(), wait, Some(command))
            }
            // Started, then waited for from its first output on.
            Ok(Command::Run {
                workspace,
                name,
                size,
                cwd,
                env,
                argv,
                log,
                then: Some(wait),
                ..
            }) => {
                // The pane's id, to wait on; --json is the wait's.
                let run = Command::Run {
                    workspace,
                    name,
                    size,
                    cwd,
                    env,
                    argv,
                    json: false,
                    log,
                    then: None,
                };
                return self.start_waiting(&run, *wait, origin);
            }
            Ok(Command::Respawn {
                target,
                kill,
                env,
                argv,
                then: Some(wait),
            }) => {
                let respawn = Command::Respawn {
                    target,
                    kill,
                    env,
                    argv,
                    then: None,
                };
                return self.start_waiting(&respawn, *wait, origin);
            }
            _ => return Begun::Done(self.run(&argv, origin)),
        };
        let here = match self.here(origin) {
            Ok(here) => here,
            Err(error) => return failed(error),
        };
        let ids: Vec<Option<PaneId>> = if targets.is_empty() {
            vec![None]
        } else {
            targets.into_iter().map(Some).collect()
        };
        let panes: Result<Vec<&Pane>, Error> =
            ids.into_iter().map(|id| self.pane(id, here)).collect();
        let now = Instant::now();
        // Armed before any keys go in: what the keys bring is what it sees.
        let mut waiting = match panes {
            Ok(panes) => crate::wait::Waiting::new(&panes, wait, now),
            Err(error) => return failed(error),
        };
        if let Some(command) = keys {
            let outcome = self.run_command(&command, origin);
            if outcome.status != 0 {
                return Begun::Done(outcome);
            }
        }
        match waiting.answer(self, now) {
            Some(outcome) => Begun::Done(outcome),
            None => Begun::Waiting(Box::new(waiting)),
        }
    }

    /// Runs a command that starts a program and prints its pane's id, then
    /// waits on that pane, armed before the server reads anything the
    /// program writes; the id comes first in what the wait prints.
    fn start_waiting(&mut self, command: &Command, wait: command::Wait, origin: &Origin) -> Begun {
        let outcome = self.run_command(command, origin);
        let pane = command::parse_pane(outcome.stdout.trim()).ok();
        let Some(pane) = pane
            .filter(|_| outcome.status == 0)
            .and_then(|p| self.panes.get(&p))
        else {
            return Begun::Done(outcome);
        };
        let lead = if wait.json {
            String::new()
        } else {
            outcome.stdout
        };
        let now = Instant::now();
        let mut waiting = crate::wait::Waiting::new(&[pane], wait, now);
        waiting.lead = lead;
        match waiting.answer(self, now) {
            Some(outcome) => Begun::Done(outcome),
            None => Begun::Waiting(Box::new(waiting)),
        }
    }

    /// The command line with a `-t NAME` that names no pane or tab read as
    /// the tab of that name, if one tab has it, or where a pane is wanted
    /// its one pane: so that `fux wait -t build --exit` finds the program
    /// `fux run -n build` started, and `fux kill-tab -t build` its tab.
    fn named(&self, argv: &[String]) -> Vec<String> {
        let (kind, name) = match command::parse(argv) {
            Err(command::Usage::Not(kind @ (Kind::Pane | Kind::Tab), name)) => (kind, name),
            _ => return argv.to_vec(),
        };
        let tabs: Vec<&Tab> = (self.workspaces.iter())
            .flat_map(Workspace::tabs)
            .filter(|t| t.name == name)
            .collect();
        // A tab named so, if one is; for a pane, that tab's one pane.
        let id = match (tabs.as_slice(), kind) {
            ([tab], Kind::Tab) => Some(tab.id.to_string()),
            ([tab], _) => tab.root().and_then(|root| match root {
                Tree::Pane(pane) => Some(pane.to_string()),
                Tree::Split(..) => None,
            }),
            _ => None,
        };
        let Some(id) = id else {
            return argv.to_vec();
        };
        let mut out = argv.to_vec();
        for i in 1..out.len() {
            if out.get(i.wrapping_sub(1)).is_some_and(|w| w == "-t")
                && let Some(word) = out.get_mut(i)
                && *word == name
            {
                word.clone_from(&id);
            }
        }
        // Another -t may name another tab.
        if out == argv {
            return out;
        }
        self.named(&out)
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
        let (Some(place), Some(view)) = (self.place(client), self.views.get(&client)) else {
            return Some(Error::NoClient(client));
        };
        let here = Here::Client(client, place);
        let action = if let Command::Client { action, .. } = command {
            Some(action)
        } else {
            None
        };
        if let Some(&ClientAction::SelectPane(pick)) = action {
            return self.pick_pane(view, place, pick).err();
        }
        if let Some(&ClientAction::SelectTab(pick)) = action {
            return self.pick_tab(place, pick).err();
        }
        if let Some(ClientAction::SelectWorkspace(pick)) = action {
            return self.pick_ws(view, pick).err();
        }
        if let Some(&ClientAction::ChoosePane { target }) = action {
            return self
                .pane(target, here)
                .and_then(|p| pane_chooser(self, p.id))
                .err();
        }
        if let Command::PasteBuffer { index, .. } = command {
            return self
                .buffers
                .get(*index)
                .is_none()
                .then_some(Error::NoBuffer(*index));
        }
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
            Command::NewWorkspace {
                name,
                cmd,
                size,
                json,
            } => {
                let cwd = self.cwd_for(here, None);
                let fit = size.map_or(Fit::Clients, Fit::Fixed);
                let start = (Start::Shell(cmd, &[]), cwd.as_path());
                let made =
                    self.create_workspace(name.clone(), start, (fit, self.start_size(fit, here)))?;
                let (ws, tab, pane) = made;
                self.follow(here, ws, None, None);
                Ok(made_ids(
                    made,
                    &[ws.to_string(), tab.to_string(), pane.to_string()],
                    *json,
                ))
            }
            Command::NewTab {
                target,
                name,
                cmd,
                size,
                json,
            } => {
                let ws = self.ws_target(target.as_ref(), here)?;
                let cwd = self.cwd_for(here, None);
                let fit = size.map_or(Fit::Clients, Fit::Fixed);
                let start = (Start::Shell(cmd, &[]), cwd.as_path());
                let (tab, pane) =
                    self.create_tab(ws, name.clone(), start, (fit, self.start_size(fit, here)))?;
                self.follow(here, ws, Some(tab), None);
                Ok(made_ids(
                    (ws, tab, pane),
                    &[tab.to_string(), pane.to_string()],
                    *json,
                ))
            }
            Command::Run {
                workspace,
                name,
                size,
                cwd,
                env,
                argv,
                json,
                log,
                ..
            } => {
                let here_cwd = self.cwd_for(here, None);
                let log = match log {
                    Some(path) => Some(
                        std::fs::File::create(here_cwd.join(path))
                            .map_err(|e| Error::Log(here_cwd.join(path), e))?,
                    ),
                    None => None,
                };
                let cwd = match cwd {
                    Some(dir) => here_cwd.join(dir),
                    None => here_cwd,
                };
                if !cwd.is_dir() {
                    return Err(Error::NotADirectory(cwd));
                }
                let fit = size.map_or(Fit::Clients, Fit::Fixed);
                // Its place is out of every client's way: no client follows.
                let place = (fit, self.start_size(fit, Here::Cli(None, None, None)));
                let name = name.clone().or_else(|| argv.first().map(|p| basename(p)));
                let start = (Start::Program(argv, env), cwd.as_path());
                let workspace = workspace
                    .clone()
                    .unwrap_or(WsRef::Name(RUN_WORKSPACE.into()));
                let made = match (self.resolve_ws(&workspace), workspace) {
                    (Ok(ws), _) => {
                        let (tab, pane) = self.create_tab(ws, name, start, place)?;
                        (ws, tab, pane)
                    }
                    (Err(_), WsRef::Name(ws_name)) => {
                        let (ws, tab, pane) = self.create_workspace(Some(ws_name), start, place)?;
                        if let Some(tab) = (self.workspaces.iter_mut())
                            .find(|w| w.id == ws)
                            .and_then(|w| w.tabs_mut().first_mut())
                        {
                            tab.name = name.unwrap_or_else(|| MAIN.into());
                        }
                        (ws, tab, pane)
                    }
                    (Err(error), WsRef::Id(_)) => return Err(error),
                };
                // Before any of its output is read: it all goes there.
                if let Some(pane) = self.panes.get_mut(&made.2) {
                    pane.log = log;
                }
                Ok(made_ids(made, &[made.2.to_string()], *json))
            }
            &Command::Respawn {
                target,
                kill,
                ref env,
                ref argv,
                ..
            } => {
                let id = self.pane(target, here)?.id;
                self.respawn(id, kill, env, argv)?;
                Ok(format!("{id}\n"))
            }
            Command::Notify { target, text } => {
                let to: Vec<ClientId> = match target {
                    None => self.views.keys().copied().collect(),
                    Some(target) => {
                        let tab = match self.any_target(&Subject::from(target.clone()), here)? {
                            AnyRef::Pane(p) => self.locate(p).map(|(_, t)| t),
                            AnyRef::Tab(t) => Some(t),
                            AnyRef::Workspace(w) => {
                                let ws = self.workspace(w);
                                let shown = (self.views.keys().copied())
                                    .filter(|c| ws.is_some_and(|w| w.viewers.contains(c)))
                                    .collect::<Vec<_>>();
                                for client in &shown {
                                    self.info_to(*client, text.clone());
                                }
                                return Ok(String::new());
                            }
                        };
                        (self.views.keys().copied())
                            .filter(|c| tab.is_some_and(|t| self.shows(*c, t)))
                            .collect()
                    }
                };
                for client in to {
                    self.info_to(client, text.clone());
                }
                Ok(String::new())
            }
            Command::Resize { what, size } => {
                match what {
                    Resized::Tabs(target) => {
                        let fit = size.map_or(Fit::Clients, Fit::Fixed);
                        let target = self.any_target(&Subject::from(target.clone()), here)?;
                        let tabs: Vec<TabId> = match target {
                            AnyRef::Tab(tab) => vec![tab],
                            AnyRef::Workspace(ws) => (self.workspace(ws).into_iter())
                                .flat_map(|w| w.tabs().iter().map(|t| t.id))
                                .collect(),
                            AnyRef::Pane(pane) => {
                                self.locate(pane).map(|(_, tab)| tab).into_iter().collect()
                            }
                        };
                        for tab in tabs_mut(&mut self.workspaces).filter(|t| tabs.contains(&t.id)) {
                            tab.set_fit(fit);
                        }
                    }
                    &Resized::Client(client) => {
                        let view = self.views.get(&client).ok_or(Error::NoClient(client))?;
                        if view.kind != ClientKind::Headless {
                            return Err(Error::NotHeadless(client));
                        }
                        if let Some(size) = size {
                            self.resize(client, size.rows(), size.cols());
                        }
                    }
                }
                Ok(String::new())
            }
            Command::Wait { .. } => Err(Error::WaitsOnlyFromCli),
            &Command::SendMouse {
                target,
                mouse,
                row,
                col,
                ..
            } => {
                let p = self.live_pane_mut(target, here)?;
                let mut report = Vec::new();
                let event = |action, button| fux_vt::keys::mouse::MouseEvent {
                    action,
                    button,
                    mods: fux_vt::keys::Modifiers::default(),
                    row,
                    col,
                };
                use fux_vt::keys::mouse::MouseAction::{Motion, Press, Release};
                let events = match mouse {
                    Mouse::Click(button) => {
                        vec![event(Press, Some(button)), event(Release, Some(button))]
                    }
                    Mouse::Press(button) | Mouse::Wheel(button) => vec![event(Press, Some(button))],
                    Mouse::Release(button) => vec![event(Release, Some(button))],
                    Mouse::Move => vec![event(Motion, None)],
                };
                for event in events {
                    if !p.screen().encode_mouse(event, &mut report) {
                        return Err(Error::NoMouse(p.id));
                    }
                }
                p.input.push(&report)?;
                Ok(String::new())
            }
            &Command::Split {
                axis,
                target,
                ref cmd,
                ref cwd,
                ref env,
                json,
            } => {
                let target = self.pane(target, here)?.id;
                let cwd = match cwd {
                    Some(dir) => self.cwd_for(here, None).join(dir),
                    None => self.cwd_for(here, Some(target)),
                };
                if !cwd.is_dir() {
                    return Err(Error::NotADirectory(cwd));
                }
                // The splitting client follows the new pane; from the CLI, the
                // clients that focus the split one.
                let shown = |c: &ClientId| self.focused(*c) == Some(target);
                let followers: Vec<ClientId> = match here {
                    Here::Client(client, _) => vec![client],
                    Here::Cli(..) => self.views.keys().copied().filter(shown).collect(),
                };
                let size = self.split_size(target, axis, here)?;
                let tab = tab_of(&mut self.workspaces, target)?;
                let mut ids = self.ids;
                let launch = self.launch.as_deref();
                let pane = new_pane(
                    &self.config,
                    launch,
                    &mut ids,
                    Start::Shell(cmd, env),
                    &cwd,
                    size,
                )?;
                self.ids = ids;
                let new = pane.id;
                tab.edit(|root| {
                    root.as_mut()
                        .map(|r| r.split(target, new, axis, Side::After))
                });
                self.panes.insert(new, pane);
                let views = self.views.values_mut();
                for view in views.filter(|v| followers.contains(&v.id)) {
                    tab.set_focus(view.id, new);
                    view.zoom = false;
                }
                let at = self.locate(new).ok_or(Error::NoPane(new))?;
                Ok(made_ids((at.0, at.1, new), &[new.to_string()], json))
            }
            &Command::Signal { target, signal, .. } => {
                let pane = self.pane(target, here)?;
                let id = pane.id;
                let child =
                    pane.process
                        .child()
                        .ok_or_else(|| match pane.process.exit_status() {
                            Some(status) => Error::Exited { pane: id, status },
                            None => Error::NoPane(id),
                        })?;
                // What the terminal's keys reach: the job in front, else the
                // program and its group.
                let group = fuxix::terminal::foreground_group(&child.master)
                    .unwrap_or_else(|| child.leader.pid());
                fuxix::process::kill_group(group, signal).map_err(Error::Signal)?;
                Ok(String::new())
            }
            // From the command line each says what it closed, so a script
            // or an agent knows; a key's close shows on the screen anyway.
            &Command::KillPane { target } => {
                let pane = self.pane(target, here)?.id;
                self.close_pane(pane, None);
                Ok(closed(origin, &[pane.to_string()]))
            }
            &Command::KillTab { target } => {
                let tab = self.tab_target(target, here)?;
                let panes = self.panes_of_tabs(&[tab]);
                self.remove_tab(tab);
                self.after_close();
                Ok(closed(origin, &[vec![tab.to_string()], panes].concat()))
            }
            Command::KillWorkspace { target } => {
                let ws = self.ws_target(target.as_ref(), here)?;
                let tabs: Vec<TabId> = (self.workspace(ws).into_iter())
                    .flat_map(|w| w.tabs().iter().map(|t| t.id))
                    .collect();
                let panes = self.panes_of_tabs(&tabs);
                self.remove_workspace(ws);
                self.after_close();
                Ok(closed(origin, &[vec![ws.to_string()], panes].concat()))
            }
            Command::Rename { target, name } => {
                let target = self.any_target(&Subject::from(target.clone()), here)?;
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
            &Command::ResizePane { target, by } => {
                let pane = self.pane(target, here)?.id;
                let (_, tab) = self.locate(pane).ok_or(Error::NoPane(pane))?;
                let area = self.reference_area(tab, here);
                match by {
                    PaneResize::Border(direction, amount) => {
                        let tab = tab_of(&mut self.workspaces, pane)?;
                        let resize = |root: &mut Option<Tree>| {
                            let root = root.as_mut();
                            root.is_some_and(|root| {
                                layout::resize(root, area, pane, direction, amount)
                            })
                        };
                        if tab.edit(resize) {
                            Ok(String::new())
                        } else {
                            Err(Error::NoBorder { pane, direction })
                        }
                    }
                    PaneResize::To { cols, rows } => self.resize_pane_to(pane, area, (cols, rows)),
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
                client: Some(client),
                ref inputs,
                repeat,
                ..
            } => {
                if !self.views.contains_key(&client) {
                    return Err(Error::NoClient(client));
                }
                // As the client's terminal would have them decoded.
                let mut typed = Vec::new();
                for input in std::iter::repeat_n(inputs, usize::from(repeat)).flatten() {
                    match input {
                        Input::Key(key) => {
                            typed.push(fux_vt::keys::decode::Input::Key((*key).into()))
                        }
                        Input::Text(text) => {
                            typed.extend(text.chars().map(|c| {
                                fux_vt::keys::decode::Input::Key(KeyPress::char(c).into())
                            }))
                        }
                        Input::Paste(text) => {
                            typed.push(fux_vt::keys::decode::Input::Paste(text.clone()))
                        }
                    }
                }
                self.dispatch(client, typed);
                Ok(String::new())
            }
            &Command::SendKeys {
                target,
                ref inputs,
                repeat,
                client: None,
                ..
            } => {
                let p = self.live_pane_mut(target, here)?;
                let (mode, bracketed) = (
                    p.screen().key_mode(),
                    p.screen().mode(fux_vt::Mode::BracketedPaste),
                );
                // Each input is a piece, and one the program must read alone,
                // a lone Escape or text, is paced apart from what follows.
                let mut pieces: Vec<(Vec<u8>, bool)> = Vec::new();
                for input in std::iter::repeat_n(inputs, usize::from(repeat)).flatten() {
                    let mut bytes = Vec::new();
                    let alone = match input {
                        Input::Key(key) => {
                            crate::encode::key_bytes((*key).into(), mode, &mut bytes);
                            bytes == b"\x1b"
                        }
                        Input::Text(text) => {
                            bytes.extend_from_slice(text.as_bytes());
                            true
                        }
                        Input::Paste(text) => {
                            crate::encode::paste(text, bracketed, &mut bytes);
                            true
                        }
                    };
                    match pieces.last_mut() {
                        Some((last, false)) if !alone => last.extend_from_slice(&bytes),
                        _ => pieces.push((bytes, alone)),
                    }
                }
                p.send(pieces, Instant::now())?;
                Ok(String::new())
            }
            &Command::CapturePane {
                target,
                history,
                view,
                json,
            } => {
                let pane = self.pane(target, here)?;
                let copying = self.copying(pane.id);
                Ok(crate::capture::pane(pane, history, view, json, copying))
            }
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
                self.configured();
                Ok(String::new())
            }
            &Command::Reload => {
                let path = self.config_path.clone().ok_or(Error::NoConfigFile)?;
                match Config::from_file(&path) {
                    Ok(config) => {
                        let said = match &config {
                            Some(_) => format!("reloaded {}\n", path.display()),
                            None => format!("no {}: the defaults apply\n", path.display()),
                        };
                        self.config = config.unwrap_or_default();
                        self.config_error = None;
                        self.configured();
                        Ok(said)
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
                Ok(crate::capture::client(view.id, &grid, json))
            }
            ClientAction::Zoom => {
                view.zoom = !view.zoom;
                Ok(String::new())
            }
            ClientAction::SelectPane(pick) => self.select(self.pick_pane(view, place, pick)?, view),
            ClientAction::SelectTab(pick) => self.select(self.pick_tab(place, pick)?, view),
            ClientAction::SelectWorkspace(ref pick) => self.select(self.pick_ws(view, pick)?, view),
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
                view.mode = Mode::List(pane_chooser(self, source)?);
                Ok(String::new())
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
                let axis = Axis::of(direction);
                tab_of(&mut self.workspaces, pane)?
                    .edit(|root| layout::move_beside(root, pane, destination, axis, side));
                return Ok(String::new());
            }
            &MoveTo::Tab(tab) => (self.tab_workspace(tab)?, tab),
            MoveTo::Workspace(r) => {
                let ws = self.resolve_ws(r)?;
                let first = self.workspace(ws).and_then(|w| w.tabs().first());
                (ws, first.ok_or(Error::NoWorkspace(ws))?.id)
            }
            MoveTo::NewTab => {
                let ws = self
                    .workspaces
                    .iter_mut()
                    .find(|w| w.tabs().iter().any(|t| t.holds(pane)));
                let ws = ws.ok_or(Error::NoPane(pane))?;
                let mut ids = self.ids;
                let id = ids.tab()?;
                ws.add_tab(id, None, (None, Fit::Clients));
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
                (self.workspaces).push(Workspace::new(
                    id,
                    name,
                    tab,
                    (None, Fit::Clients),
                    clients,
                ));
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

    /// Shows client `view` where a select command picked, unzoomed.
    fn select(&mut self, (ws, tab, pane): Picked, view: &mut View) -> Result<String, Error> {
        self.show(view.id, ws, tab, pane);
        view.zoom = false;
        Ok(String::new())
    }

    /// The pane `select-pane` picks for client `view`, which is at `at`.
    fn pick_pane(&self, view: &View, at: Place, pick: PanePick) -> Result<Picked, Error> {
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
        Ok((ws, Some(tab), Some(target)))
    }

    /// The tab `select-tab` picks for a client at `at`.
    fn pick_tab(&self, at: Place, to: Pick<TabId>) -> Result<Picked, Error> {
        let (ws, target) = match to {
            Pick::Id(t) => (self.tab_workspace(t)?, t),
            Pick::Step(toward) => {
                let tabs = self.workspace(at.ws).map_or(&[][..], Workspace::tabs);
                let index = tabs.iter().position(|t| t.id == at.tab);
                let next = round(tabs, index.unwrap_or(0), toward);
                (at.ws, next.ok_or(Error::OnlyOne(Kind::Tab))?.id)
            }
        };
        Ok((ws, Some(target), None))
    }

    /// The workspace `select-workspace` picks for client `view`.
    fn pick_ws(&self, view: &View, pick: &Pick<WsRef>) -> Result<Picked, Error> {
        let ws = match pick {
            Pick::Id(r) => self.resolve_ws(r)?,
            Pick::Step(toward) => {
                let workspaces = &self.workspaces;
                let index = workspaces.iter().position(|w| w.viewers.contains(&view.id));
                let next = round(workspaces, index.unwrap_or(0), *toward);
                next.ok_or(Error::OnlyOne(Kind::Workspace))?.id
            }
        };
        Ok((ws, None, None))
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
                        let process = match (p.process.child(), p.process.exit_status()) {
                            (Some(child), _) => format!(" pid {}", child.leader.pid()),
                            (None, Some(ended)) => format!(" {}", crate::process::told(ended)),
                            (None, None) => String::new(),
                        };
                        out.push_str(&format!(
                            "    {} {} {}x{}{}\n",
                            pane,
                            p.label(),
                            p.size().cols(),
                            p.size().rows(),
                            process,
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
        let now = Instant::now();
        let panes = |tab: &Tab| {
            let mut panes = Vec::new();
            let Some(root) = tab.root() else {
                return Json::Array(panes);
            };
            root.for_each_pane(&mut |id| {
                let Some(p) = self.panes.get(&id) else {
                    return;
                };
                let mut fields = vec![
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
                ];
                fields.extend(crate::capture::state(p, now, self.copying(p.id)));
                panes.push(Json::Object(fields));
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

/// What a program's terminal holds, unread, up to a mebibyte: read as it
/// exits, so that its last screen is whole.
fn unread(process: &Process) -> Vec<u8> {
    let mut left = Vec::new();
    if let Process::Reading(child) = process {
        let mut buffer = [0u8; 16384];
        while left.len() < 1 << 20 {
            match fuxix::io::read(&child.master, &mut buffer) {
                Ok(n) if n > 0 => left.extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(fuxix::Errno::INTR) => {}
                Ok(_) | Err(_) => break,
            }
        }
    }
    left
}

/// What a close from the command line prints: `closed` and the ids gone.
fn closed(origin: &Origin, ids: &[String]) -> String {
    match origin {
        Origin::Cli { .. } => format!("closed {}\n", ids.join(" ")),
        Origin::Client(_) | Origin::Nowhere => String::new(),
    }
}

/// `argv` as it is started, and variables to add: a shell fux knows
/// started so that it marks its commands, unless `set shell-marks off`. Its
/// startup files go beside the server's socket.
fn with_marks(
    config: &Config,
    socket: &Path,
    argv: &[String],
    env: &[(String, String)],
) -> (Vec<String>, Vec<(String, String)>) {
    let mut dir = socket.as_os_str().to_owned();
    dir.push(".shell");
    match config
        .shell_marks
        .then(|| crate::marks::marked(argv, env, Path::new(&dir)))
        .flatten()
    {
        Some(marked) => (marked.argv, marked.env),
        None => (argv.to_vec(), Vec::new()),
    }
}

/// What a new pane starts.
#[derive(Clone, Copy)]
enum Start<'a> {
    /// The shell, with a command line typed into it if one is given, and
    /// variables set for it.
    Shell(&'a [String], &'a [(String, String)]),
    /// A program, run in place of a shell, with variables set for it.
    Program(&'a [String], &'a [(String, String)]),
}

/// A new pane running what `start` says.
fn new_pane(
    config: &Config,
    launch: Option<&Path>,
    ids: &mut Ids,
    start: Start<'_>,
    cwd: &Path,
    size: fux_vt::Size,
) -> Result<Pane, Error> {
    let shell_program = config
        .shell
        .first()
        .cloned()
        .unwrap_or_else(|| "/bin/sh".into());
    let (argv, cmd, runs, env) = match start {
        Start::Shell(cmd, env) => (config.shell.as_slice(), cmd, Runs::Shell, env),
        Start::Program(argv, env) => {
            let launch = crate::pane::Launch {
                argv: argv.to_vec(),
                cwd: cwd.to_path_buf(),
                env: env.to_vec(),
            };
            (argv, argv, Runs::Program(launch), env)
        }
    };
    // The line to type is made before anything else: a line too long to
    // type must not leave a process behind.
    let typed = match start {
        Start::Shell([], _) | Start::Program(..) => None,
        Start::Shell(cmd, _) => {
            let fish = basename(&shell_program) == "fish";
            let mut line = crate::words::shell_line(cmd, fish)?.into_bytes();
            line.push(b'\r');
            let deadline = crate::after(Instant::now(), TYPE_WAIT);
            Some(Typed::new(line, deadline).ok_or(Error::LineTooLong)?)
        }
    };
    let id = ids.pane()?;
    let name = cmd
        .first()
        .map(|c| basename(c))
        .unwrap_or_else(|| basename(&shell_program));
    let mut pane = Pane::new(id, name, runs, size, config.history_lines)?;
    if let Some(socket) = launch {
        let mut vars = vec![
            ("FUX_PANE", id.to_string()),
            ("FUX_SOCKET", socket.to_string_lossy().into_owned()),
        ];
        vars.extend(env.iter().map(|(k, v)| (k.as_str(), v.clone())));
        let (argv, marks) = with_marks(config, socket, argv, env);
        vars.extend(marks.iter().map(|(k, v)| (k.as_str(), v.clone())));
        pane.process = Process::Reading(crate::process::spawn(&argv, cwd, &vars, size)?);
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

    /// `set history-lines` changes every pane's history, not only the next
    /// pane's.
    #[test]
    fn set_history_lines_applies_to_panes_there_are() -> Result<(), String> {
        let (mut s, _) = attached(5, 20)?;
        s.output(
            PaneId::of(1),
            std::iter::repeat_n("line\r\n", 50)
                .collect::<String>()
                .as_bytes(),
        );
        let history = |s: &Session| {
            s.panes
                .get(&PaneId::of(1))
                .map(|p| p.screen().history_len())
        };
        assert!(history(&s) > Some(2));
        run(&mut s, "set history-lines 2")?;
        assert_eq!(history(&s), Some(2));
        Ok(())
    }

    /// A reload with no file says the defaults apply, not that it read one.
    #[test]
    fn a_reload_without_a_file_says_the_defaults_apply() -> Result<(), String> {
        let (mut s, _) = attached(5, 20)?;
        s.config_path = Some(PathBuf::from("/nonexistent/fux.conf"));
        run(&mut s, "set prefix C-a")?;
        let said = output(&mut s, "reload")?;
        assert_eq!(said, "no /nonexistent/fux.conf: the defaults apply\n");
        assert_eq!(s.config.prefix.to_string(), "C-b");
        Ok(())
    }

    /// A program's OSC 52 write is a copy: to the paste buffers and the
    /// clipboard of the clients showing it, unless `set clipboard off`.
    #[test]
    fn a_programs_clipboard_write_is_a_copy() -> Result<(), String> {
        let (mut s, client) = attached(5, 20)?;
        s.outbox.clear();
        s.output(PaneId::of(1), b"\x1b]52;c;Zm9v\x07");
        assert_eq!(s.buffers.front().map(String::as_str), Some("foo"));
        let sent = |s: &Session| {
            s.outbox
                .iter()
                .any(|o| matches!(o, Outgoing::Bytes(c, b) if *c == client && b == b"\x1b]52;c;Zm9v\x07"))
        };
        assert!(sent(&s));
        s.outbox.clear();
        run(&mut s, "set clipboard off")?;
        s.output(PaneId::of(1), b"\x1b]52;c;YmFy\x07");
        assert!(!sent(&s) && s.outbox.is_empty());
        assert_eq!(s.buffers.front().map(String::as_str), Some("foo"));
        Ok(())
    }

    /// A tab no client shows is laid out on a client's screen, so its
    /// programs see the size they will be shown at, not the size they
    /// started at.
    #[test]
    fn a_tab_no_client_shows_is_sized_as_on_a_clients_screen() -> Result<(), String> {
        let (mut s, _) = attached(11, 40)?;
        run(&mut s, "new-tab -t +1")?;
        let size = |s: &Session, p| s.panes.get(&PaneId::of(p)).map(Pane::size);
        assert_eq!(size(&s, 2), Some(crate::pane::size(10, 40)));
        run(&mut s, "split -v -t %2")?;
        let (top, bottom) = (size(&s, 2), size(&s, 3));
        let rows = top
            .zip(bottom)
            .map(|(t, b)| (t.rows(), b.rows(), t.cols(), b.cols()));
        assert_eq!(rows, Some((5, 4, 40, 40)), "stacked, a separator between");
        // Leaving a tab whose layout is unchanged resizes nothing: a zoomed
        // pane keeps the whole screen.
        run(&mut s, "select-tab -c c1 -t @2")?;
        run(&mut s, "zoom -c c1")?;
        run(&mut s, "select-tab -c c1 -t @1")?;
        assert_eq!(size(&s, 2), Some(crate::pane::size(10, 40)));
        Ok(())
    }

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
        // How long each pane has been quiet varies: read as 0.
        let quiet = |json: String| {
            let mut parts = json.split("\"idle_ms\":");
            let first = parts.next().unwrap_or_default().to_owned();
            parts.fold(first, |out, part| {
                let rest = part.trim_start_matches(|c: char| c.is_ascii_digit());
                format!("{out}\"idle_ms\":0{rest}")
            })
        };
        assert_eq!(
            quiet(output(&mut s, "ls --json")?),
            concat!(
                r#"{"workspaces":[{"id":"+1","name":"main","tabs":[{"id":"@1","name":"main","panes":[{"id":"%1","name":"say \"hi\" \\ 界","title":"","rows":3,"cols":15,"pid":null,"program":null,"exited":null,"signal":null,"idle_ms":0,"alt_screen":false,"mouse":false,"bracketed_paste":false,"pictures":[],"cursor_visible":true,"history_lines":0,"copy_mode":false,"revision":0,"running":false,"last_status":null,"bells":0,"notification":null,"cwd":null,"argv":null},{"id":"%2","name":"sh","title":"tab\\title","rows":3,"cols":14,"pid":null,"program":null,"exited":null,"signal":null,"idle_ms":0,"alt_screen":false,"mouse":false,"bracketed_paste":false,"pictures":[],"cursor_visible":true,"history_lines":0,"copy_mode":false,"revision":1,"running":false,"last_status":null,"bells":0,"notification":null,"cwd":null,"argv":null}]},{"id":"@2","name":"two","panes":[{"id":"%3","name":"sh","title":"","rows":3,"cols":30,"pid":null,"program":null,"exited":null,"signal":null,"idle_ms":0,"alt_screen":false,"mouse":false,"bracketed_paste":false,"pictures":[],"cursor_visible":true,"history_lines":0,"copy_mode":false,"revision":0,"running":false,"last_status":null,"bells":0,"notification":null,"cwd":null,"argv":null}]}]}],"clients":[{"id":"c1","rows":4,"cols":30,"workspace":"+1","tab":"@1","pane":"%2","zoom":false}]}"#,
                "\n"
            )
        );
        assert_eq!(
            quiet(output(&mut s, "capture-pane -t %2 --json")?),
            concat!(
                r#"{"pane":"%2","rows":3,"cols":14,"cursor":[1,2],"lines":["a       b \"q\"","界"],"program":null,"exited":null,"signal":null,"idle_ms":0,"alt_screen":false,"mouse":false,"bracketed_paste":false,"pictures":[],"cursor_visible":true,"history_lines":0,"copy_mode":false,"revision":1,"running":false,"last_status":null,"bells":0,"notification":null,"cwd":null,"argv":null}"#,
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
        s.exited(PaneId::of(2), crate::process::Ended::Exited(3));
        assert_eq!(notice(&s).as_deref(), Some("%2 sh exited with status 3"));
        // A second tab, shown; its only pane exits, and the tab with it.
        run(&mut s, "new-tab -t +1")?;
        run(&mut s, "select-tab -c c1 -t @2")?;
        s.exited(PaneId::of(3), crate::process::Ended::Exited(7));
        assert_eq!(notice(&s).as_deref(), Some("%3 sh exited with status 7"));
        Ok(())
    }

    /// A pane moved beside another moves within its own tab: a tab emptied
    /// by an earlier move takes no copy of it, so none is left naming it
    /// once it closes.
    #[test]
    fn a_pane_moved_beside_another_stays_out_of_empty_tabs()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "new-tab -t +1")?;
        run(&mut s, "move-pane -t %3 --to @1")?;
        run(&mut s, "move-pane -t %1 -R")?;
        let holding = |s: &Session, pane| {
            let tabs = s.workspaces.iter().flat_map(|w| w.tabs());
            tabs.filter(|t| t.holds(pane)).count()
        };
        assert_eq!(holding(&s, PaneId::of(1)), 1);
        run(&mut s, "kill-pane -t %1")?;
        assert_eq!(holding(&s, PaneId::of(1)), 0);
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
        assert_eq!(
            output(&mut s, "new-workspace -n workspace-3")?,
            "+2 @2 %2\n"
        );
        // +3 would be workspace-3, which is taken.
        assert_eq!(output(&mut s, "new-workspace")?, "+3 @3 %3\n");
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

    /// Output can drop history rows a copy mode holds: the settle that
    /// follows it ends copy mode when they go.
    #[test]
    fn output_that_drops_a_copy_modes_rows_ends_it() -> Result<(), Box<dyn std::error::Error>> {
        let mut s = started(Config {
            history_lines: 2,
            ..Config::default()
        })?;
        let client = s.attach(6, 20, None)?;
        s.output(PaneId::of(1), b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        run(&mut s, "copy-mode -c c1")?;
        // To the oldest row, which the next lines of output push out.
        s.input(client, b"g");
        s.output(PaneId::of(1), b"g\r\nh\r\ni\r\nj\r\n");
        s.hold_copies();
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
        // Why a menu entry or a binding cannot run now: as running it says.
        // The client's tab has one pane; workspace two's, two.
        run(&mut s, "split -h -t %2")?;
        let client = "c1".parse()?;
        for (line, reason) in [
            ("select-pane --next", Some("only one pane")),
            ("select-pane --last", Some("no previously focused pane")),
            ("select-pane -L", Some("no pane left of %1")),
            ("choose-pane -t %2", None),
            ("select-tab --next", Some("only one tab")),
            ("select-workspace --next", None),
            ("paste-buffer", Some("no buffer 0")),
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

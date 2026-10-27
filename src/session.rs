//! The server's state: workspaces, tabs, panes and the clients' views, and
//! every command that changes them.
use crate::answer::PaneAnswer;
use crate::code::{Code, Raise};
use crate::command::{
    self, AnyRef, ClientAction, Command, Extent, Input, Kind, Members, Mouse, MoveTo, PanePick,
    PaneResize, Pick, Request, Resized, Sibling, SwapWith, WsRef,
};
use crate::config::Config;
use crate::copy::MAX_CELLS;
use crate::id::{ClientId, Ids, PaneId, TabId, WsId};
use crate::json::{Fields, Json};
use crate::keys::Direction;
use crate::layout::{self, Axis, NoPane, Placement, Rect, Shares, Tree};
use crate::pane::{self, Pane, PaneMut, Process, Runs, basename};
use crate::process::{self, Child, Ended};
use crate::protocol::{Attach, Body, End, Reply, ScreenKind, ToAttached};
use crate::view::{self, ClientKind, Mode, View};
use crate::wait::{Waiting, Watches};
use crate::workspace::{Destination, Fit, Parts, Seat, Split, Tab, Workspace};
use fux_vt::Size;
use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
mod layouts;
use std::time::{Duration, Instant};

/// What the server must do outside the session.
#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// A frame for a client: bytes for its terminal outside any paint
    /// (questions for the terminal; the bell; the keyboard flags pushed),
    /// fuxth to run as its own, keys to take as typed at its terminal
    /// (`send-keys` to a client), its screen ended by the server, or its
    /// detach, saying why.
    To(ClientId, ToAttached<'static>),
    /// A copy for a client's clipboard, its OSC 52: refused, and said to
    /// be, to a client too far behind (`server::OUT_MAX`).
    Copy(ClientId, Vec<u8>),
    /// Stop the server, saying why.
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
    pub fn kill(self) -> Option<process::Pid> {
        drop(self.child.master);
        self.child.leader.finish().err()
    }
}

/// Something the session waits for until a moment (`Session::timers`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timer {
    /// A pane's frame of synchronized output is read, ended or not
    /// (`Pane::frame_deadline`).
    Frame(PaneId),
    /// A pane's output, waiting since the parser did all the work one call
    /// may, is read on (`Pane::read_unread`): due at once, after the
    /// server's other work of this turn. A respawn under way in the pane
    /// takes its turn too (`Session::after_turn`).
    Unread(PaneId),
}

/// Where a command comes from, which says what it acts on when it names no
/// pane, tab or workspace.
#[derive(Clone, Debug)]
pub enum Origin {
    /// A client's key or prompt: where the client is.
    Client(ClientId),
    /// The CLI: who runs the command, and from where.
    Cli(Caller),
}

/// A command line's caller on the CLI.
#[derive(Clone, Debug, Default)]
pub struct Caller {
    /// The pane it gave (`FUX_PANE`), of the server `within` says.
    pub gave: Option<PaneId>,
    /// The server whose pane it runs in, by its `FUX_SERVER`.
    pub within: Within,
    /// Its working directory.
    pub cwd: Option<PathBuf>,
    /// Its environment, of which panes it starts take what
    /// `update-environment` names.
    pub env: Option<Vec<(String, std::ffi::OsString)>>,
}

/// The server whose pane a caller on the CLI runs in, as its
/// `FUX_SERVER` says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Within {
    /// None said: a pane it gives is no one's fux can tell, so it is
    /// none.
    #[default]
    Unsaid,
    /// This server: it runs in one of its panes, whether or not it gave it.
    This,
    /// Another server: the pane it gave is that server's, and nothing here
    /// is done to it.
    Other,
}

impl Caller {
    /// Its pane, if it gave one of this server's.
    fn pane(&self) -> Option<PaneId> {
        match self.within {
            Within::This => self.gave,
            Within::Unsaid | Within::Other => None,
        }
    }
}

/// Where an attached client is: the workspace and tab it is shown, and the
/// pane it focuses there.
#[derive(Clone, Copy)]
pub struct Place {
    pub ws: WsId,
    pub tab: TabId,
    pub pane: PaneId,
}

/// Where a select command takes a client: a workspace, and a tab and pane
/// there if given.
struct Picked {
    ws: WsId,
    at: Option<Spot>,
}

/// Where in a workspace a client is shown: a tab, and a pane in it if
/// given.
#[derive(Clone, Copy)]
pub(crate) struct Spot {
    pub tab: TabId,
    pub pane: Option<PaneId>,
}

/// The workspace, tab and pane a command made.
#[derive(Clone, Copy)]
struct Made {
    ws: WsId,
    tab: TabId,
    pane: PaneId,
}

/// A pane's size to begin with, and the cell its program is told it
/// starts in: none in a tab with no host yet (`Session::graphics`).
type Starts = (Size, Option<fux_vt::CellSize>);

/// A pane to make: what it starts, where, what it takes of its starter,
/// and how it starts (`Starts`).
#[derive(Clone, Copy)]
struct NewPane<'a> {
    start: Start<'a>,
    cwd: &'a Path,
    starting: &'a Starting,
    size: Size,
    cell: Option<fux_vt::CellSize>,
}

impl<'a> NewPane<'a> {
    fn new(start: Start<'a>, cwd: &'a Path, starting: &'a Starting, (size, cell): Starts) -> Self {
        Self {
            start,
            cwd,
            starting,
            size,
            cell,
        }
    }
}

/// What a command that names nothing acts on, found once from its origin.
#[derive(Clone, Copy)]
enum Here<'a> {
    /// An attached client, where it is.
    Client(ClientId, Place),
    /// The CLI: its caller's pane (if it gave one) and the workspace that
    /// pane is in, if it is still there; and its working directory.
    Cli(Option<(PaneId, Option<WsId>)>, Option<&'a Path>),
}

impl Here<'_> {
    fn ws(self) -> Option<WsId> {
        match self {
            Here::Client(_, place) => Some(place.ws),
            Here::Cli(pane, _) => pane.and_then(|(_, ws)| ws),
        }
    }
    fn pane(self) -> Option<PaneId> {
        match self {
            Here::Client(_, place) => Some(place.pane),
            Here::Cli(pane, _) => pane.map(|(pane, _)| pane),
        }
    }
    fn client(self) -> Option<ClientId> {
        match self {
            Here::Client(client, _) => Some(client),
            Here::Cli(..) => None,
        }
    }
    /// The caller's pane, for a command that acts on it when none is named.
    fn caller_pane(self) -> Result<PaneId, Error> {
        self.pane().ok_or(Error::NoPaneGiven)
    }
}

/// Why a command, or attaching a client, failed: exit status 1.
#[derive(Debug)]
pub enum Error {
    // What a command names, or its context gives, that is not there.
    NoWorkspace(WsId),
    NoPaneGiven,
    NoPane(PaneId),
    NoTab(TabId),
    NoWorkspaceGiven,
    NoClient(ClientId),
    NoBuffer(usize),
    /// A pane, tab or workspace alone of its kind, with nothing to move to.
    OnlyOne(Kind),
    AtEnd(Kind),
    NoNeighbor {
        from: PaneId,
        direction: Direction,
    },
    // What copy mode cannot do.
    NoRows,
    /// A selection of more than `MAX_CELLS` cells.
    SelectionTooLarge,
    // A name that cannot be given.
    NameTaken(String),
    /// A tab has the name: those tabs, then their panes.
    TabNameTaken(String, Vec<TabId>, Vec<PaneId>),
    /// A workspace name a target would read as a number, not a name.
    NameReadAsTarget(String),
    /// Any other failure, made in one place and told apart by its code
    /// alone: its code, and why.
    Failed {
        code: Code,
        message: String,
    },
}

/// A failure of kind `code` (see [`Error::Failed`]).
pub(crate) fn fail(code: Raise, message: impl Into<String>) -> Error {
    Error::Failed {
        code: code.into(),
        message: message.into(),
    }
}

impl Error {
    /// What kind of failure it is, its code: stable, for a
    /// script to tell failures apart by.
    pub fn code(&self) -> Code {
        match self {
            Self::NoWorkspace(_) => Code::NoWorkspace,
            Self::NoPane(_) => Code::NoPane,
            Self::NoTab(_) => Code::NoTab,
            Self::NoPaneGiven | Self::NoWorkspaceGiven => Code::NoTarget,
            Self::NoClient(_) => Code::NoClient,
            Self::NoBuffer(_) => Code::NoBuffer,
            Self::OnlyOne(_) | Self::AtEnd(_) | Self::NoNeighbor { .. } => Code::NothingThere,
            Self::NoRows | Self::SelectionTooLarge => Code::Selection,
            Self::NameReadAsTarget(_) => Raise::BadName.into(),
            Self::NameTaken(_) | Self::TabNameTaken(..) => Code::NameTaken,
            Self::Failed { code, .. } => *code,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWorkspace(id) => write!(f, "no workspace {id}"),
            Self::NoPaneGiven => f.write_str("no pane given: name one (`str %N pane`)"),
            Self::NoPane(id) => write!(f, "no pane {id}"),
            Self::NoTab(id) => write!(f, "no tab {id}"),
            Self::NoWorkspaceGiven => f.write_str(
                "no workspace given: name one (`str NAME workspace`; for new-tab, the \
                     option `str NAME in`)",
            ),
            Self::NoClient(id) => write!(f, "no client {id}"),
            Self::NoBuffer(index) => write!(f, "no buffer {index}"),
            Self::OnlyOne(kind) => write!(f, "only one {}", kind.name()),
            Self::AtEnd(kind) => write!(f, "the {} is already at that end", kind.name()),
            Self::NoNeighbor { from, direction } => {
                write!(f, "no pane {} of {from}", direction.name())
            }
            Self::NoRows => f.write_str("the pane has no rows"),
            Self::SelectionTooLarge => write!(f, "the selection is larger than {MAX_CELLS} cells"),
            Self::NameTaken(name) => write!(f, "a workspace is named {name:?}"),
            Self::TabNameTaken(name, tabs, panes) => {
                let ids = (tabs.iter().map(|t| AnyRef::Tab(*t)))
                    .chain(panes.iter().map(|p| AnyRef::Pane(*p)));
                let ids = joined(ids, " ");
                let kills = joined(tabs, " tab kill`, `fux str ");
                write!(
                    f,
                    "a tab is named {name:?} already ({ids}); close it (`fux str {kills} tab kill`) or choose another name"
                )
            }
            Self::NameReadAsTarget(name) => write!(
                f,
                "{name:?} cannot be a name: a target would read it as a pane, tab or workspace number"
            ),
            Self::Failed { message, .. } => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {}

impl From<pane::Error> for Error {
    fn from(error: pane::Error) -> Self {
        let code = match error {
            pane::Error::InputFull => Raise::InputFull,
            pane::Error::TooLong(_) => Raise::TooLong,
            pane::Error::Terminal { .. } => Raise::Start,
        };
        fail(code, error.to_string())
    }
}

/// Why `pane`'s program takes no input, if it takes none: the one check
/// of input typed or sent, of the reports (mouse, focus) a terminal is
/// asked to make for it, so that a pane none could reach asks for none,
/// and of those fux queues it unasked (in-band resize, colour scheme), so
/// that none waits for a program respawned there.
pub(crate) fn refuses_input(pane: &Pane) -> Option<Error> {
    let why = match pane.process() {
        Process::HungUp(_) => "its program closed its terminal: nothing reads input there",
        Process::Respawning => {
            "it is respawning: no program of its own reads input till it is done"
        }
        Process::Exited(status) => return Some(ended(pane.id(), *status)),
        Process::Absent | Process::Reading(_) => return None,
    };
    Some(fail(Raise::NotRunning, format!("{}: {why}", pane.id())))
}

/// `pane`'s program has ended, with `status`: it takes no input.
fn ended(pane: PaneId, status: Ended) -> Error {
    let was = match status {
        Ended::Exited(_) | Ended::Lost => "",
        Ended::Signalled(_) => "was ",
    };
    let told = format!("the program in {pane} {was}{}", process::told(status));
    fail(Raise::NotRunning, told)
}

/// What a command line from the CLI begins: its outcome, now, or a wait
/// for one (`wait`, or an action given wait words).
pub enum Begun {
    Done(Reply),
    Waiting(Box<Waiting>),
    /// `fux events`: a stream, until the client goes.
    Events(Box<crate::events::Subscription>),
}

/// A respawn under way: the pane made for the program started, and the
/// program, which take the place of the pane's own once all its old
/// program left is read, in turns as any pane's output is
/// (`Session::after_turn`). The new program's output waits on its
/// terminal till then. If the pane closes first, the respawn is abandoned
/// (`Session::end_pane`), and said to be.
struct Replacement {
    fresh: Pane,
    process: Process,
    /// When it began: its turns are due from then, a frame the old program
    /// holds, with nothing else to read, read by one.
    since: Instant,
}

/// How long a closing pane's processes have to exit after the hangup.
pub const GRACE: Duration = Duration::from_millis(100);

pub struct Session {
    /// Its workspaces, tabs and panes, and the journal of what happened to
    /// them, which events read.
    pub parts: Parts,
    pub views: BTreeMap<ClientId, View>,
    pub config: Config,
    /// Paste buffers, newest first.
    pub buffers: VecDeque<String>,
    /// The socket a started pane is told to reach its server by, if panes
    /// start real processes; tests of the state alone start none.
    launch: Option<PathBuf>,
    /// The running fux, a started pane's `FUX`, if it can be had.
    exe: Option<PathBuf>,
    /// Which run of a server this is: its panes' `FUX_SERVER`, by which a
    /// command from a pane tells whether the pane is this server's, and
    /// every version's, by which a wait tells its own.
    pub run: command::Run,
    pub outbox: Vec<Outgoing>,
    /// Closed panes' programs: a pane respawned twice in its grace has two.
    pub dying: Vec<Dying>,
    /// Each respawn under way, by its pane: one at a time in a pane.
    respawning: BTreeMap<PaneId, Replacement>,
    /// Where `size_panes` gathers the rectangles each pane is shown in, and
    /// lays out each view to find them; reused by every settle.
    shown_sizes: Vec<(PaneId, (u16, u16))>,
    placed: Placement,
    /// The tabs clients sized at the last settle: one of a size of its own
    /// none sizes now is laid out on that size again (`size_panes`).
    sized: Vec<TabId>,
    ids: Ids,
    /// Commands run that may change what any client shows; input that runs
    /// one repaints every client, input that runs none only its own.
    changes: u64,
    /// In tests, each client's keys as its own client would read them
    /// (`Session::input`), and the keymap every client and run shares.
    #[cfg(test)]
    pub(crate) test_keys: BTreeMap<ClientId, testing::Keys>,
    #[cfg(test)]
    pub(crate) test_keymap: crate::keymap::Keymap,
}

/// How the panes a command starts start, passed to what starts them: the
/// variables they take from whoever starts them rather than the server
/// (those `update-environment` names that the starter has, and those it
/// has not, which they are not given either). A pane started for no
/// command (the first shell) starts as `Starting::default()` says.
#[derive(Clone, Debug, Default)]
struct Starting {
    set: Vec<(String, std::ffi::OsString)>,
    unset: Vec<String>,
}

/// The name of the first workspace, and of each workspace's first tab.
pub(crate) const MAIN: &str = "main";

/// The workspace `fux run` starts programs in unless told another.
pub const RUN_WORKSPACE: &str = "agent";

/// The one item of `items`; of several, why not: `many` says it from how
/// many there are and their ids.
fn one<T: Copy + std::fmt::Display>(
    items: &[T],
    many: impl FnOnce(usize, String) -> String,
) -> Result<T, Error> {
    match items {
        [item] => Ok(*item),
        items => Err(fail(Raise::NoTarget, many(items.len(), joined(items, " ")))),
    }
}

/// `items`, each as it displays, with `between` between them.
fn joined<T: std::fmt::Display>(items: impl IntoIterator<Item = T>, between: &str) -> String {
    let items: Vec<String> = items.into_iter().map(|item| item.to_string()).collect();
    items.join(between)
}

/// A value a command can't take, as `Usage` says why.
fn bad_value(usage: command::Usage) -> Error {
    fail(Raise::BadValue, usage.to_string())
}

/// The answer of an action on a pane (keys, a mouse event, a signal, a
/// program started): its version before it acted, or as it starts; what a
/// wait for its effect waits to differ from.
fn acted(version: command::Version) -> Reply {
    Body::Pane(PaneAnswer::Acted(version)).into()
}

/// A command that failed, told with its code (its status: 1, 4 or 124).
fn failure(error: Error, origin: &Origin) -> Reply {
    // A command that needed its caller's pane, tab or workspace, from a
    // caller whose `FUX_PANE` is another server's pane, or no server's it
    // says: why it got none.
    let error = match (error, origin) {
        (
            Error::NoPaneGiven | Error::NoWorkspaceGiven,
            Origin::Cli(Caller {
                gave: Some(pane),
                within: within @ (Within::Other | Within::Unsaid),
                ..
            }),
        ) => {
            let whose = match within {
                Within::Unsaid => "comes with no FUX_SERVER to say whose pane it is",
                Within::This | Within::Other => "is a pane of another fux server, not of this one",
            };
            fail(
                Raise::StrayPane,
                format!("FUX_PANE {pane} {whose}, so it is not acted on; name the pane"),
            )
        }
        (error, _) => error,
    };
    Reply::failed(error.code(), error.to_string())
}

impl Session {
    pub fn new(config: Config, socket: PathBuf, launch: bool, run: command::Run) -> Self {
        Self {
            parts: Parts::default(),
            views: BTreeMap::new(),
            config,
            buffers: VecDeque::new(),
            launch: launch.then_some(socket),
            exe: std::env::current_exe().ok(),
            run,
            outbox: Vec::new(),
            dying: Vec::new(),
            respawning: BTreeMap::new(),
            shown_sizes: Vec::new(),
            placed: Placement::default(),
            sized: Vec::new(),
            ids: Ids::default(),
            changes: 0,
            #[cfg(test)]
            test_keys: BTreeMap::new(),
            #[cfg(test)]
            test_keymap: crate::keymap::Keymap::empty(),
        }
    }

    /// One workspace, holding one tab with one shell, laid out on a
    /// client's screen of `size`.
    pub fn start(&mut self, size: Size) -> Result<(), Error> {
        let (cwd, starting) = (home()?, Starting::default());
        let new = NewPane::new(Start::Shell(&[]), &cwd, &starting, (size, None));
        self.create_workspace(Some(MAIN.into()), None, Fit::Clients(size), new)
            .map(drop)
    }

    // ---------------------------------------------------------------- lookup

    pub fn workspace(&self, id: WsId) -> Option<&Workspace> {
        self.parts.workspaces().iter().find(|w| w.id() == id)
    }
    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        (self.parts.workspaces().iter().flat_map(Workspace::tabs)).find(|t| t.id() == id)
    }
    /// A tab's layout.
    pub fn root(&self, tab: TabId) -> Option<&Tree> {
        self.tab(tab).map(Tab::root)
    }
    /// The workspace and tab that hold a pane.
    pub fn locate(&self, pane: PaneId) -> Option<(WsId, TabId)> {
        self.parts.locate(pane)
    }
    /// The workspace a tab is in.
    pub fn tab_workspace(&self, tab: TabId) -> Result<WsId, Error> {
        let ws = (self.parts.workspaces().iter()).find(|w| w.tabs().iter().any(|t| t.id() == tab));
        ws.map(|w| w.id()).ok_or(Error::NoTab(tab))
    }
    /// The workspace that holds `target` (a workspace, itself), if it is
    /// there.
    pub fn holder(&self, target: AnyRef) -> Option<WsId> {
        match target {
            AnyRef::Pane(p) => self.locate(p).map(|(ws, _)| ws),
            AnyRef::Tab(t) => self.tab_workspace(t).ok(),
            AnyRef::Workspace(w) => self.workspace(w).map(|w| w.id()),
        }
    }
    /// The workspace client `client` shows.
    pub fn shown_workspace(&self, client: ClientId) -> Option<&Workspace> {
        (self.parts.workspaces().iter()).find(|w| w.viewers.contains(&client))
    }
    /// The tab client `client` shows.
    pub fn shown_tab(&self, client: ClientId) -> Option<&Tab> {
        self.shown_workspace(client)?.tab_of(client)
    }
    pub(crate) fn shown_tab_mut(&mut self, client: ClientId) -> Option<&mut Tab> {
        let tab = self.shown_tab(client)?.id();
        self.parts.tab_mut(tab)
    }
    /// The pane client `client` focuses.
    pub fn focused(&self, client: ClientId) -> Option<PaneId> {
        self.shown_tab(client)?.focus(client)
    }
    /// Whether client `client` shows tab `tab`.
    pub(crate) fn shows(&self, client: ClientId, tab: TabId) -> bool {
        self.shown_tab(client).is_some_and(|t| t.id() == tab)
    }
    pub fn resolve_ws(&self, r: &WsRef) -> Result<WsId, Error> {
        match r {
            WsRef::Id(id) => self.workspace(*id).ok_or(Error::NoWorkspace(*id)),
            WsRef::Name(name) => (self.parts.workspaces().iter().find(|w| w.name() == *name))
                .ok_or_else(|| fail(Raise::NoWorkspace, format!("no workspace named {name:?}"))),
        }
        .map(|w| w.id())
    }
    /// Whether `only` (with nothing, everything) takes in tab `tab` of
    /// workspace `ws`: its workspace, the tab, or a pane in it.
    fn takes_in(&self, only: Option<AnyRef>, ws: WsId, tab: TabId) -> bool {
        match only {
            None => true,
            Some(AnyRef::Workspace(w)) => w == ws,
            Some(AnyRef::Tab(t)) => t == tab,
            Some(AnyRef::Pane(p)) => self.locate(p).is_some_and(|(_, t)| t == tab),
        }
    }
    /// `target`, if it is there; else why not.
    pub fn there(&self, target: AnyRef) -> Result<AnyRef, Error> {
        match target {
            AnyRef::Pane(p) => self.pane(p).map(|_| ()),
            AnyRef::Tab(t) => self.tab(t).map(|_| ()).ok_or(Error::NoTab(t)),
            AnyRef::Workspace(w) => self.workspace(w).map(|_| ()).ok_or(Error::NoWorkspace(w)),
        }
        .map(|()| target)
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
            Origin::Cli(ref caller) => {
                let pane = caller.pane().map(|p| (p, self.locate(p).map(|(ws, _)| ws)));
                Ok(Here::Cli(pane, caller.cwd.as_deref()))
            }
        }
    }

    /// Where client `client` is, if it is attached: the workspace it is
    /// shown, while there is any.
    pub fn place(&self, client: ClientId) -> Option<Place> {
        let ws = self.shown_workspace(client)?;
        let tab = ws.tab_of(client)?;
        let (ws, pane, tab) = (ws.id(), tab.focus(client)?, tab.id());
        Some(Place { ws, tab, pane })
    }

    fn pane(&self, id: PaneId) -> Result<&Pane, Error> {
        self.parts.panes().get(&id).ok_or(Error::NoPane(id))
    }

    fn pane_mut(&mut self, id: PaneId) -> Result<PaneMut<'_>, Error> {
        self.parts.pane_mut(id).ok_or(Error::NoPane(id))
    }

    /// Starts `argv` with `env` as pane `id`'s program, in `cwd` at `size`
    /// in cells of `cell`, if panes start real processes: told of its
    /// server (`FUX_PANE`, `FUX_SOCKET`, `FUX_SERVER`, and `FUX`, the fux
    /// running it, unset if it cannot be had), given its starter's
    /// variables, and not those its starter had not (but for the server's
    /// own above), unless `env` sets them.
    fn spawn(
        &self,
        id: PaneId,
        launch: &pane::Launch,
        fresh: &Starting,
        size: (Size, Option<fux_vt::CellSize>),
    ) -> Result<Option<Child>, Error> {
        let Some(socket) = &self.launch else {
            return Ok(None);
        };
        let (argv, env) = (launch.argv.as_slice(), launch.env.as_slice());
        let mut vars = vec![
            ("FUX_PANE", id.to_string().into()),
            ("FUX_SOCKET", socket.as_os_str().to_owned()),
            ("FUX_SERVER", self.run.to_string().into()),
        ];
        vars.extend((self.exe.as_ref()).map(|exe| ("FUX", exe.as_os_str().to_owned())));
        vars.extend((fresh.set.iter()).map(|(k, v)| (k.as_str(), v.clone())));
        vars.extend(env.iter().map(|(k, v)| (k.as_str(), v.into())));
        // No `FUX` to give: not one the server was started with either.
        let unset: Vec<String> = (fresh.unset.iter().cloned())
            .chain(self.exe.is_none().then(|| "FUX".to_owned()))
            .filter(|name| !env.iter().any(|(k, _)| k == name))
            .collect();
        let child = process::spawn(argv, &launch.cwd, (&vars, &unset), size)
            .map_err(|m| fail(Raise::Start, m))?;
        Ok(Some(child))
    }

    /// Starts the program pane `id` ran again in it: its words, or `argv`
    /// if given, where it ran, with what it was given and `env` set; first
    /// hanging up the one there with `kill`, else refused while it runs.
    /// What the old program left is read before the new one takes its
    /// place, its exit told if one waits on that, in as many turns as that
    /// takes: till then the respawn is under way (`Replacement`), and
    /// another refused. Its answer, the version the pane starts at, waits
    /// for it (`Waiting::Respawn`).
    fn respawn(
        &mut self,
        id: PaneId,
        kill: bool,
        env: &[(String, String)],
        argv: &[String],
        starting: &Starting,
    ) -> Result<Reply, Error> {
        let pane = self.pane(id)?;
        let Runs::Explicit(launch) = &pane.runs else {
            let message = format!("{id} runs a shell; respawn restarts what run or split started");
            return Err(fail(Raise::NotRun, message));
        };
        if self.respawning.contains_key(&id) {
            let message =
                format!("{id} is respawning already: what its program left is still being read");
            return Err(fail(Raise::StillRunning, message));
        }
        // One whose exit waits on what it left has ended.
        if pane.process().child().is_some() && pane.exiting.is_none() && !kill {
            let message = format!("the program in {id} still runs; `kill-first` ends it first");
            return Err(fail(Raise::StillRunning, message));
        }
        let mut launch = launch.clone();
        if !argv.is_empty() {
            launch.argv = argv.to_vec();
        }
        for (key, value) in env {
            launch.env.retain(|(k, _)| k != key);
            launch.env.push((key.clone(), value.clone()));
        }
        // The program it starts is told its host's cell.
        let graphics = self.graphics(id);
        let size = (pane.size(), graphics.cell());
        let (respawns, exiting) = (pane.respawns.saturating_add(1), pane.exiting);
        // All that can refuse it is done before its program starts, which
        // nothing can then leave running.
        let runs = Runs::Explicit(launch.clone());
        let history = self.config.history_bytes.get();
        let mut fresh = Pane::new(id, String::new(), runs, size.0, history)?;
        // Its program starts sized in that cell, so no resize is due.
        let _ = fresh.host(Default::default(), Default::default(), graphics);
        let child = self.spawn(id, &launch, starting, size)?;
        // What one that runs left is read once another started (a start
        // refused leaves it running, nothing of it cut). Then its program is
        // taken: no more of it is read, and input is refused till the new
        // one takes its place. One that runs is ended; one whose exit waits
        // is let go as that exit lets it go.
        if kill {
            self.read_left(id);
        }
        if let Some(child) = self.pane_mut(id).ok().and_then(|mut p| p.let_go()) {
            match exiting {
                None => self.end(Process::Reading(child), true),
                Some(_) => self.dying.push(Dying {
                    child,
                    deadline: Instant::now(),
                }),
            }
        }
        let mut started = fresh.version(self.run);
        started.counts.respawns = respawns;
        let process = child.map_or(Process::Absent, Process::Reading);
        let since = Instant::now();
        let new = Replacement {
            fresh,
            process,
            since,
        };
        self.respawning.insert(id, new);
        self.after_turn(id);
        Ok(acted(started))
    }

    /// What ends a turn of pane `id`'s reads: once nothing read waits, a
    /// program that exited with output unread exits, and a respawn under
    /// way takes its place, once a frame its old program holds is read
    /// too, by a turn of its own (`fire`).
    fn after_turn(&mut self, id: PaneId) {
        let respawning = self.respawning.contains_key(&id);
        let waits = |p: &&Pane| p.has_unread() || respawning && p.frame_deadline().is_some();
        let Some(pane) = self.parts.panes().get(&id).filter(|p| !waits(p)) else {
            return;
        };
        if let Some(ended) = pane.exiting {
            self.exited(id, ended);
        }
        let Some(new) = self.respawning.remove(&id) else {
            return;
        };
        let Some(mut pane) = self.parts.pane_mut(id) else {
            self.end(new.process, true);
            return;
        };
        // At the pane's size and history now, which may have changed while
        // it waited.
        let size = pane.size();
        let (old, left) = pane.respawned(new.fresh, new.process);
        pane.resize(size);
        pane.set_history_bytes(self.config.history_bytes.get());
        // An exit that came while it was no pane's program is told now: the
        // server looks for exits as they are signalled (`Server::reap`).
        let exited = pane.process().child().and_then(|c| c.leader.exited());
        self.end(old, true);
        let left = left.told("the program it ran");
        if !left.is_empty() {
            let told = format!("{id} respawned{left}");
            for client in self.viewers(self.locate(id).map(|(_, t)| t)) {
                self.notify_to(client, told.clone(), true);
            }
        }
        if let Some(ended) = exited {
            self.exited(id, ended);
        }
        // Hosted as its pane is now.
        self.touch();
        self.settle();
    }

    /// The process group `fux signal` signals in a pane: the terminal's
    /// foreground group (a job its shell runs), or the program `run`
    /// started; never the shell of a pane that runs one.
    fn signalled(&self, target: PaneId) -> Result<process::Pid, Error> {
        let pane = self.pane(target)?;
        if let Process::Respawning = pane.process() {
            let message = format!("{target} is respawning: no program is its own till it is done");
            return Err(fail(Raise::StillRunning, message));
        }
        if let Some(status) = pane.process().exit_status() {
            return Err(ended(pane.id(), status));
        }
        let program = match (&pane.runs, pane.process().child()) {
            (Runs::Explicit(_), Some(child)) => Some(child.leader.pid()),
            _ => None,
        };
        let job = pane.process().job().map_err(|errno| {
            let why =
                format!("{target}: its terminal does not say what runs in its foreground: {errno}");
            fail(Raise::Signal, why)
        })?;
        job.or(program).ok_or_else(|| {
            let message = format!("nothing is running in {} but its shell", pane.id());
            fail(Raise::OnlyShell, message)
        })
    }

    /// Refuses a name given to a tab (other than `except`, being renamed)
    /// that `str NAME tab` could not find it by: one a target reads as an id, or
    /// one another tab has.
    fn check_tab_name(&self, name: &str, except: Option<TabId>) -> Result<(), Error> {
        check_name(name)?;
        // One a target would read as an id.
        if name.starts_with(['%', '@', '+']) {
            return Err(Error::NameReadAsTarget(name.to_owned()));
        }
        // A workspace's name finds the workspace, never this tab.
        if self.parts.workspaces().iter().any(|w| w.name() == name) {
            return Err(Error::NameTaken(name.to_owned()));
        }
        no_tab_named(self.parts.workspaces(), name, except)
    }

    /// Gives tab `id` the name `name`, checked (`Parts::give_tab_name`).
    fn give_tab_name(&mut self, id: TabId, name: String) -> Result<(), Error> {
        self.parts.give_tab_name(id, name).ok_or(Error::NoTab(id))
    }

    /// Whether a client is in copy mode on pane `pane`: keys typed there
    /// go to copy mode, not the program.
    pub(crate) fn copying(&self, pane: PaneId) -> bool {
        self.views
            .values()
            .any(|v| matches!(&v.mode, Mode::Copy(copy) if copy.pane == pane))
    }

    /// What panes started for `origin` take of its environment: none of
    /// it for an absent snapshot; an empty snapshot unsets every named variable.
    fn starting_for(&self, origin: &Origin) -> Starting {
        let env = match origin {
            Origin::Cli(caller) => caller.env.as_deref(),
            Origin::Client(client) => self.views.get(client).map(|v| v.env.as_slice()),
        };
        let mut fresh = Starting::default();
        let Some(env) = env else { return fresh };
        for name in &self.config.update_environment {
            match env.iter().rev().find(|(k, _)| k == name.as_str()) {
                Some((k, v)) => fresh.set.push((k.clone(), v.clone())),
                None => fresh.unset.push(name.as_str().to_owned()),
            }
        }
        fresh
    }

    /// What `id`, written `text`, stands for as a `kind`: itself; a pane's
    /// or tab's id, its tab or workspace; a tab or workspace of one pane, or
    /// a workspace of one tab, that one.
    fn stands_for(&self, id: AnyRef, text: &str, kind: Kind) -> Result<AnyRef, Error> {
        let of = id.kind().name();
        let many = |each: Kind| {
            let each = each.name();
            move |n, ids| format!("{text} is a {of} of {n} {each}s ({ids}): use a {each}'s id")
        };
        match (id, kind) {
            (AnyRef::Pane(_), Kind::Pane)
            | (AnyRef::Tab(_), Kind::Tab)
            | (AnyRef::Workspace(_), Kind::Workspace) => self.there(id),
            (AnyRef::Pane(p), Kind::Tab) => {
                Ok(AnyRef::Tab(self.locate(p).ok_or(Error::NoPane(p))?.1))
            }
            (AnyRef::Pane(p), Kind::Workspace) => {
                Ok(AnyRef::Workspace(self.locate(p).ok_or(Error::NoPane(p))?.0))
            }
            (AnyRef::Tab(t), Kind::Workspace) => self.tab_workspace(t).map(AnyRef::Workspace),
            (AnyRef::Tab(_) | AnyRef::Workspace(_), Kind::Pane) => {
                one(&self.panes_of(id)?, many(Kind::Pane)).map(AnyRef::Pane)
            }
            (AnyRef::Workspace(_), Kind::Tab) => {
                one(&self.tabs_of(id)?, many(Kind::Tab)).map(AnyRef::Tab)
            }
        }
    }

    /// What `text` names as a pane, tab or workspace, `kind`: its id (`%3`),
    /// or a tab's name (a pane, its tab's one pane); a workspace's name.
    /// A pane, nothing given: the caller's own.
    fn resolve(&self, text: &str, kind: Kind, here: Here) -> Result<AnyRef, Error> {
        let tabs: Vec<&Tab> = (self.parts.workspaces().iter())
            .flat_map(Workspace::tabs)
            .filter(|t| t.name() == text)
            .collect();
        let panes_of = |tab: &Tab| tab.root().panes();
        let named = || -> Result<&Tab, Error> {
            match tabs.as_slice() {
                [tab] => Ok(*tab),
                [] => {
                    let code = match kind {
                        Kind::Pane => Raise::NoPane,
                        Kind::Tab => Raise::NoTab,
                        Kind::Workspace => Raise::NoWorkspace,
                    };
                    Err(fail(code, format!("no {} named {text:?}", kind.name())))
                }
                many => {
                    let each = |t: &&Tab| format!("{} {}", t.id(), joined(panes_of(t), " "));
                    let (n, each) = (many.len(), joined(many.iter().map(each), ", "));
                    let why = format!("{text:?} names {n} tabs ({each}): use an id");
                    Err(fail(Raise::NoTarget, why))
                }
            }
        };
        if let Ok(id) = text.parse() {
            return self.stands_for(id, text, kind);
        }
        // A version names its pane: one of this server run's.
        if let Ok(version) = text.parse::<command::Version>() {
            self.counts_since(version)?;
            return self.stands_for(AnyRef::Pane(version.pane), text, kind);
        }
        match kind {
            Kind::Pane if text.is_empty() => Ok(AnyRef::Pane(self.pane(here.caller_pane()?)?.id())),
            // A tab's name: its one pane. A pane's own name is
            // `pane_named`'s.
            Kind::Pane => {
                let many =
                    |n, ids| format!("{text:?} is a tab of {n} panes ({ids}): use a pane's id");
                one(&panes_of(named()?), many).map(AnyRef::Pane)
            }
            Kind::Tab => named().map(|tab| AnyRef::Tab(tab.id())),
            Kind::Workspace => {
                let ws = command::parse_workspace(text).map_err(bad_value)?;
                Ok(AnyRef::Workspace(self.ws_target(Some(&ws), here)?))
            }
        }
    }

    /// The pane named `name` (`pane-named`): what `rename` gave it, or its
    /// program's name; never a title its program set.
    fn pane_named(&self, name: &str) -> Result<PaneId, Error> {
        let mut panes: Vec<PaneId> = (self.parts.panes().values())
            .filter(|p| p.name() == name)
            .map(|p| p.id())
            .collect();
        panes.sort();
        if panes.is_empty() {
            return Err(fail(Raise::NoPane, format!("no pane named {name:?}")));
        }
        let many = |n, ids| format!("{name:?} names {n} panes ({ids}): use an id");
        one(&panes, many)
    }

    /// The tabs `target` is or holds: a pane's own, a tab, or a
    /// workspace's every tab, in order.
    fn tabs_of(&self, target: AnyRef) -> Result<Vec<TabId>, Error> {
        Ok(match target {
            AnyRef::Tab(tab) => vec![self.tab(tab).ok_or(Error::NoTab(tab))?.id()],
            AnyRef::Workspace(ws) => (self.workspace(ws).ok_or(Error::NoWorkspace(ws))?)
                .tabs()
                .iter()
                .map(|t| t.id())
                .collect(),
            AnyRef::Pane(pane) => vec![self.locate(pane).ok_or(Error::NoPane(pane))?.1],
        })
    }

    /// The panes `target` names: a pane, or a tab's or workspace's every
    /// pane, in order.
    fn panes_of(&self, target: AnyRef) -> Result<Vec<PaneId>, Error> {
        Ok(match target {
            AnyRef::Pane(pane) => vec![self.pane(pane)?.id()],
            AnyRef::Tab(tab) => self.root(tab).ok_or(Error::NoTab(tab))?.panes(),
            AnyRef::Workspace(ws) => (self.workspace(ws).ok_or(Error::NoWorkspace(ws))?)
                .tabs()
                .iter()
                .flat_map(|t| t.root().panes())
                .collect(),
        })
    }

    /// Sends `inputs` to the programs of the panes `to` names, each encoded
    /// as its program asked for keys (`bytes`): to one seen, only if it is
    /// still at that version as they are queued, else none, failing
    /// `stale`. That is all it promises: not what its program does next.
    fn send_keys(&mut self, to: command::To, inputs: &[Input]) -> Result<Reply, Error> {
        // Stale before it is judged live: a program's end is a change.
        if let command::To::Seen(seen) = to {
            self.counts_since(seen)?;
            let now = self.pane(seen.pane)?.version(self.run);
            if now != seen {
                let why = format!("{} changed since {seen}: it is at {now}", seen.pane);
                return Err(fail(Raise::Stale, why));
            }
        }
        // Every pane must take them, or none is sent them: each one's are
        // made and fit first.
        let (run, mut each) = (self.run, Vec::new());
        for id in self.panes_of(to.target())? {
            let mut pane = self.live_pane_mut(id)?;
            let bytes = bytes(&pane, inputs);
            pane.fits(bytes.len())?;
            each.push((pane.version(run), bytes));
        }
        for (version, bytes) in &each {
            self.pane_mut(version.pane)?.push(bytes)?;
        }
        let mut answers = each
            .into_iter()
            .map(|(version, _)| PaneAnswer::Acted(version));
        Ok(match (to.target(), answers.next()) {
            (AnyRef::Pane(_), Some(one)) => Body::Pane(one),
            (_, first) => Body::Panes(first.into_iter().chain(answers).collect()),
        }
        .into())
    }

    /// `pane_mut`, for input: a pane whose program takes none is refused
    /// (`refuses_input`).
    pub(crate) fn live_pane_mut(&mut self, id: PaneId) -> Result<PaneMut<'_>, Error> {
        let pane = self.pane_mut(id)?;
        match refuses_input(&pane) {
            Some(refused) => Err(refused),
            None => Ok(pane),
        }
    }

    /// The split of `target` along `axis`, the two taking `shares` if
    /// given, and the size the new pane starts at: the one settling gives
    /// it in the layout the split makes, which its tab must have room for,
    /// `followers` focusing it unzoomed. Shown on no screen (each zoomed
    /// on another pane), it starts at its place on the tab's own size.
    fn split_size(
        &self,
        target: PaneId,
        (axis, shares): (Axis, Option<(NonZeroU32, NonZeroU32)>),
        followers: &[ClientId],
    ) -> Result<(Split, Size), Error> {
        let new = self.ids.clone().pane()?;
        let shares = shares.map_or(Shares::HALVED, |(a, b)| Shares::new(a, b));
        let split = (self.parts.plan_split((target, new), axis, shares))
            .map_err(|NoPane| Error::NoPane(target))?;
        self.room(split.tab(), split.root(), None)?;
        let seat = |view: &View, now| match followers.contains(&view.id) {
            true => (false, Some(new)),
            false => now,
        };
        let (mut placed, mut shown) = (Placement::default(), Vec::new());
        self.shown_in((split.tab(), split.root()), seat, &mut placed, &mut shown);
        let mut sizes = shown.into_iter().filter(|(p, _)| *p == new).map(|(_, s)| s);
        let least = sizes
            .next()
            .map(|first| sizes.fold(first, |(rows, cols), (h, w)| (rows.min(h), cols.min(w))));
        let own = || {
            let area = self.tab(split.tab())?.fit().area();
            let rect = layout::place(split.root(), area).rect(new)?;
            Some((rect.h(), rect.w()))
        };
        let size = least
            .or_else(own)
            .and_then(|(rows, cols)| Size::new(rows, cols).ok());
        Ok((split, size.ok_or(Error::NoPane(target))?))
    }

    /// Resizes `pane`, placed in `area`, to `cols` and `rows`, either or
    /// both; nothing changes unless both can be met.
    fn resize_pane_to(
        &mut self,
        pane: PaneId,
        area: Rect,
        axis: Axis,
        extent: Extent,
    ) -> Result<(), Error> {
        let (_, tab) = self.locate(pane).ok_or(Error::NoPane(pane))?;
        let mut root = self.root(tab).cloned().ok_or(Error::NoPane(pane))?;
        let now = layout::place(&root, area)
            .rect(pane)
            .ok_or(Error::NoPane(pane))?;
        let len = match extent {
            Extent::Cells(n) => n,
            Extent::Percent(p) => p.of(area.len(axis)),
        };
        let want = match axis {
            Axis::Horizontal => (len, now.h()),
            Axis::Vertical => (now.w(), len),
        };
        let refused = || {
            let message = format!("{pane} cannot be made {}x{} in its tab", want.0, want.1);
            fail(Raise::Size, message)
        };
        root = layout::resize_to(&root, area, pane, axis, len).ok_or_else(refused)?;
        // A size the layout cannot give exactly is not met.
        let got = layout::place(&root, area)
            .rect(pane)
            .map(|r| (r.w(), r.h()));
        if got != Some(want) {
            return Err(refused());
        }
        (self.parts.reshape(pane, |old| *old = root)).ok_or(Error::NoPane(pane))?;
        Ok(())
    }

    fn ws_target(&self, explicit: Option<&WsRef>, here: Here) -> Result<WsId, Error> {
        match explicit {
            Some(r) => self.resolve_ws(r),
            None => here.ws().ok_or(Error::NoWorkspaceGiven),
        }
    }

    // ------------------------------------------------------------ creation

    /// The directory pane `pane`'s program is in, if it can be read.
    fn dir_of(&self, pane: PaneId) -> Option<PathBuf> {
        let runs = self.parts.panes().get(&pane)?.process().child();
        // While it respawns, the program started for it.
        let child = runs.or_else(|| self.respawning.get(&pane)?.process.child())?;
        fuxix::process::cwd(child.leader.pid())
    }

    /// Where a new pane's program starts: `dir`, from the caller's
    /// directory, or with none the directory of the caller, or of `near`.
    /// A CLI caller's own directory that is unknown fails the start;
    /// `$HOME` stands in only for a pane's that can't be read, or none.
    /// Unchecked: one the program cannot start in fails its start, with
    /// the system's error.
    fn start_dir(
        &self,
        here: Here,
        dir: Option<&Path>,
        near: Option<PaneId>,
    ) -> Result<PathBuf, Error> {
        if let Some(dir) = dir.filter(|dir| dir.is_absolute()) {
            return Ok(dir.to_owned());
        }
        let near = near.filter(|_| dir.is_none());
        let known = match here {
            // Empty: the caller could not read its own (`client::context`).
            Here::Cli(_, Some(cwd)) if !cwd.is_absolute() => {
                return Err(fail(Raise::Start, "the caller's own directory is unknown"));
            }
            Here::Cli(_, Some(cwd)) => Some(cwd.to_owned()),
            Here::Cli(_, None) => near.and_then(|p| self.dir_of(p)),
            Here::Client(_, place) => self.dir_of(near.unwrap_or(place.pane)),
        };
        let base = known.map_or_else(home, Ok)?;
        Ok(match dir {
            Some(dir) => base.join(dir),
            None => base,
        })
    }

    /// The name a workspace gets when none is given: `workspace-N` after
    /// its ID, or the next number up that no workspace is named.
    fn workspace_name(&self, id: WsId) -> String {
        let free = |name: &String| !self.parts.workspaces().iter().any(|ws| ws.name() == *name);
        let mut names = (id.number()..=u32::MAX).map(|n| format!("workspace-{n}"));
        names
            .find(free)
            .unwrap_or_else(|| format!("workspace-{}", u32::MAX))
    }

    /// A workspace with one tab, laid out on `fit`, holding one new pane.
    fn create_workspace(
        &mut self,
        name: Option<String>,
        tab_name: Option<String>,
        fit: Fit,
        new: NewPane<'_>,
    ) -> Result<Made, Error> {
        if let Some(name) = &name {
            check_workspace_name(name, self.parts.workspaces())?;
        }
        // The IDs are taken before the pane starts, so that none can run out
        // after, and are committed once it has.
        let mut ids = self.ids.clone();
        let id = ids.workspace()?;
        let tab = ids.tab()?;
        let pane = self.new_pane(&mut ids, new)?;
        self.ids = ids;
        let name = name.unwrap_or_else(|| self.workspace_name(id));
        let made = Made {
            ws: id,
            tab,
            pane: pane.id(),
        };
        let tab_name = tab_name.unwrap_or_else(|| MAIN.into());
        let clients = self.views.keys().copied();
        (self.parts).add_workspace((id, name), (tab, tab_name, fit), clients, pane);
        Ok(made)
    }

    /// Adds a tab named `name` (checked: `run` and `new-tab` check it) to
    /// the workspace `ws` names, or else `here`'s, laid out on `fit`,
    /// holding one new pane. A workspace not there that `ws` names by its
    /// name is made, the tab its first.
    fn create_tab(
        &mut self,
        ws: Option<&WsRef>,
        here: Here,
        name: Option<String>,
        fit: Fit,
        new: NewPane<'_>,
    ) -> Result<Made, Error> {
        let id = match (self.ws_target(ws, here), ws) {
            (Ok(id), _) => id,
            (Err(_), Some(WsRef::Name(ws_name))) => {
                return self.create_workspace(Some(ws_name.clone()), name, fit, new);
            }
            (Err(error), _) => return Err(error),
        };
        // Found before its pane's program starts, which nothing can then
        // leave running. As for a workspace: the ID first, committed after
        // the pane.
        self.workspace(id).ok_or(Error::NoWorkspace(id))?;
        let mut ids = self.ids.clone();
        let tab = ids.tab()?;
        let pane = self.new_pane(&mut ids, new)?;
        let made = Made {
            ws: id,
            tab,
            pane: pane.id(),
        };
        if let Err(pane) = self.parts.add_tab((id, tab), name, fit, pane) {
            self.end(pane.into_process(), true);
            return Err(Error::NoWorkspace(id));
        }
        self.ids = ids;
        Ok(made)
    }

    /// A new pane, its ID taken from `ids`, running what `new` says.
    fn new_pane(&self, ids: &mut Ids, new: NewPane<'_>) -> Result<Pane, Error> {
        let config = &self.config;
        let start = new.start;
        let env = match start {
            Start::Shell(env) | Start::Program(_, env) => env,
        };
        let launch = pane::Launch {
            argv: match start {
                Start::Shell(..) => config.shell.argv(),
                Start::Program(argv, _) => argv.to_vec(),
            },
            cwd: new.cwd.to_path_buf(),
            env: env.to_vec(),
        };
        let (named, runs) = match start {
            Start::Shell(..) => (None, Runs::DefaultShell),
            Start::Program(argv, _) => (
                argv.first().map(String::as_str),
                Runs::Explicit(launch.clone()),
            ),
        };
        let id = ids.pane()?;
        let name = basename(named.unwrap_or(config.shell.program()));
        let pane = Pane::new(id, name, runs, new.size, config.history_bytes.get())?;
        let child = self.spawn(id, &launch, new.starting, (new.size, new.cell))?;
        Ok(match child {
            Some(child) => pane.running(child),
            None => pane,
        })
    }

    // ------------------------------------------------------------- clients

    /// A client attaches, viewing `workspace` or the first one (the tests'
    /// terminal client).
    #[cfg(test)]
    pub(crate) fn attach(
        &mut self,
        rows: u16,
        cols: u16,
        workspace: Option<&str>,
    ) -> Result<ClientId, Error> {
        let workspace = workspace.map(command::parse_workspace).transpose();
        let size = Size::new(rows, cols).map_err(|e| fail(Raise::Size, e.to_string()))?;
        let workspace = workspace.map_err(bad_value)?;
        self.attach_as(
            ClientKind::Terminal,
            Attach {
                workspace,
                ..Attach::of(size)
            },
        )
    }

    /// A client of `kind` attaches as `attach` says: one that only watches
    /// is one from the start, and sizes nothing.
    pub fn attach_as(&mut self, kind: ClientKind, attach: Attach) -> Result<ClientId, Error> {
        // A size no view has is refused before anything starts on it.
        let (rows, cols) = attach.size.nonzero();
        let (rows, cols) =
            view::size(rows.get(), cols.get()).map_err(|why| fail(Raise::Size, why))?;
        let ws = match &attach.workspace {
            Some(workspace) => self.resolve_ws(workspace)?,
            None => {
                // A server with nothing in it (just started, or its session
                // did not come back) starts its first shell on this client's
                // screen; one that only watches starts and sizes nothing.
                if self.parts.workspaces().is_empty() && attach.read_only {
                    let why =
                        "the server holds nothing to watch: `fux` attaches and starts a shell";
                    return Err(fail(Raise::NoWorkspace, why));
                }
                if self.parts.workspaces().is_empty() {
                    let size = Size::new(rows.saturating_sub(1), cols).map_err(|_| {
                        let why =
                            format!("a screen of {rows} rows has no room for a pane below its bar");
                        fail(Raise::Size, why)
                    })?;
                    self.start(size)?;
                }
                (self.parts.workspaces().first().map(|w| w.id()))
                    .ok_or_else(|| fail(Raise::NoWorkspace, "the server has no workspace"))?
            }
        };
        let id = self.ids.client()?;
        let mut view = View::new(id, rows, cols);
        (view.kind, view.read_only, view.env) = (kind, attach.read_only, attach.env);
        self.views.insert(id, view);
        self.parts.attach(id);
        self.show(id, ws, None);
        self.ask(id, crate::outer::QUERIES);
        self.settle();
        Ok(id)
    }

    pub fn detach(&mut self, id: ClientId) {
        self.views.remove(&id);
        self.parts.forget(id);
        self.settle();
    }

    /// Shows `client` workspace `ws`, one there is, and there the tab and
    /// pane `at` says, each if given and there.
    fn show(&mut self, client: ClientId, ws: WsId, at: Option<Spot>) {
        self.parts.show(client, ws, at);
    }

    /// The command's client, if it has one, follows what it made or moved:
    /// it is shown it, unzoomed.
    fn follow(&mut self, here: Here, ws: WsId, at: Option<Spot>) {
        if let Here::Client(client, _) = here
            && let Some(view) = self.views.get_mut(&client)
        {
            view.zoom = false;
            self.show(client, ws, at);
        }
    }

    /// A client's terminal is `rows` by `cols`: its view takes the size
    /// and settles if that changes it, and says whether it did; a size no
    /// view has (`view::size`) is refused, and the view kept.
    pub fn resize(&mut self, id: ClientId, rows: u16, cols: u16) -> Result<bool, Error> {
        let size = view::size(rows, cols).map_err(|why| fail(Raise::Size, why))?;
        let view = self.views.get_mut(&id).filter(|v| (v.rows, v.cols) != size);
        let Some(view) = view else { return Ok(false) };
        (view.rows, view.cols) = size;
        view.dirty = true;
        self.settle();
        Ok(true)
    }

    /// Gives a client a notice, an error's if `error`, if it is still
    /// attached.
    pub fn notify_to(&mut self, client: ClientId, text: impl Into<String>, error: bool) {
        if let Some(view) = self.views.get_mut(&client) {
            view.notify(text, error);
        }
    }

    // -------------------------------------------------------------- layout

    /// Where each pane of a view's current tab is on its screen.
    pub fn placement(&self, view: &View) -> Placement {
        let mut out = Placement::default();
        self.placement_into(view, &mut out);
        out
    }

    /// Where each pane of a view's current tab is on its screen, into
    /// `out`, whatever it held, reusing its buffers.
    pub fn placement_into(&self, view: &View, out: &mut Placement) {
        let area = view.pane_area();
        let tab = self.shown_tab(view.id);
        let Some(root) = tab.map(Tab::root) else {
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

    /// The area to lay out a tab in for a command: the screen of a client
    /// showing it that sizes its panes (not one that only watches), the
    /// commanding one's first, else the smallest screen, else what its fit says.
    fn reference_area(&self, tab: TabId, here: Here) -> Option<Rect> {
        let own = (self.sizing(tab)).find(|v| Some(v.id) == here.client());
        own.map(View::pane_area)
            .or_else(|| {
                self.smallest_screen(tab)
                    .map(|(rows, cols)| Rect::screen(rows, cols))
            })
            .or_else(|| self.tab(tab).map(|t| t.fit().area()))
    }

    /// What a new tab is laid out on while no client shows it: `size`, if
    /// given; else the caller's own screen, if it sizes panes; else the
    /// size of the caller's tab: the smallest screen of the clients that
    /// size it, or with none its fit's. Never the screen of a client that
    /// does not show the caller's tab: a caller with neither, and no size
    /// given, is refused.
    fn new_fit(&self, size: command::Size, here: Here) -> Result<Fit, Error> {
        if let Some(size) = size {
            return Ok(Fit::Fixed(size.size()));
        }
        let own = (here.client().and_then(|c| self.views.get(&c)))
            .filter(|view| view.sizes())
            .and_then(Self::screen_size);
        let tab = || {
            let tab = match here {
                Here::Client(_, place) => Some(place.tab),
                Here::Cli(..) => here.pane().and_then(|p| self.locate(p)).map(|(_, tab)| tab),
            };
            let least = self
                .smallest_screen(tab?)
                .and_then(|(rows, cols)| Size::new(rows, cols).ok());
            least.or_else(|| self.tab(tab?).map(|t| t.fit().size()))
        };
        let why = "the caller has no screen that sizes panes and no tab, and no size was given";
        (own.or_else(tab).map(Fit::Clients)).ok_or_else(|| {
            fail(
                Raise::Size,
                format!("{why}: give a size (`COLS ROWS size`)"),
            )
        })
    }

    /// The smallest screen of the clients sizing `tab`'s panes.
    fn smallest_screen(&self, tab: TabId) -> Option<(u16, u16)> {
        self.sizing(tab)
            .map(View::pane_area)
            .map(|area| (area.h(), area.w()))
            .reduce(|(rows, cols), (h, w)| (rows.min(h), cols.min(w)))
    }

    /// The clients showing tab `tab` that size its panes.
    fn sizing(&self, tab: TabId) -> impl Iterator<Item = &View> {
        (self.views.values()).filter(move |v| v.sizes() && self.shows(v.id, tab))
    }

    /// The size a client's screen gives panes, below its bar, if any.
    fn screen_size(view: &View) -> Option<Size> {
        let area = view.pane_area();
        Size::new(area.h(), area.w()).ok()
    }

    // -------------------------------------------------------------- repair

    /// Ends each view's mode if what it shows is gone, then sizes the PTYs:
    /// run after every event.
    pub fn settle(&mut self) {
        // Out of the session while they are repaired, which reads no view.
        let mut views = std::mem::take(&mut self.views);
        let ended: Vec<Outgoing> = (views.values_mut())
            .filter_map(|view| Some(Outgoing::To(view.id, ToAttached::Ended(self.repair(view)?))))
            .collect();
        self.views = views;
        self.outbox.extend(ended);
        self.size_panes();
        self.host_panes();
        self.tell_schemes();
        self.tell_lost();
        self.hold_copies();
    }

    /// Each pane whose program was told other graphics than its host's
    /// terminal draws now (`graphics`) is told those: as a terminal says
    /// its cell or sixel, or the pane's host changes (another typed there,
    /// the host left or shows another tab, or there is none). A new cell,
    /// unknown too, sizes its terminal in pixels, which signals its
    /// program, not left till it writes; and the clients showing it are
    /// painted again, as its pictures show only while its host draws sixel
    /// (`render::pictures`).
    fn host_panes(&mut self) {
        let told = |p: &&Pane| self.graphics(p.id()) != p.screen().graphics();
        let stale: Vec<PaneId> = (self.parts.panes().values().filter(told))
            .map(Pane::id)
            .collect();
        for id in stale {
            let tab = self.locate(id).map(|(_, tab)| tab);
            for client in self.viewers(tab) {
                if let Some(view) = self.views.get_mut(&client) {
                    view.dirty = true;
                }
            }
            if let Some((pane, true)) = self.hosted(id) {
                pane.size_terminal();
            }
        }
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
            let Some(screen) = self.parts.panes().get(&copy.pane).map(Pane::screen) else {
                continue;
            };
            if copy.held_at.is_some_and(|at| !screen.changed_since(at)) {
                continue;
            }
            if copy.resolve(screen).is_some() {
                copy.held_at = Some(screen.mark());
            } else {
                view.mode = Mode::Normal;
                view.notify(
                    "copy mode ended: the history dropped the rows it held",
                    true,
                );
                let ended = ToAttached::Ended(ScreenKind::Copy);
                self.outbox.push(Outgoing::To(view.id, ended));
            }
        }
    }

    /// Ends what a view shows if what it is about is gone: copy mode whose
    /// pane closed or is no longer focused, a chooser about a pane, tab or
    /// workspace that closed. Which kind it ended, if it did, which
    /// its client is told (`ToAttached::Ended`), the mode being the client's.
    fn repair(&self, view: &mut View) -> Option<ScreenKind> {
        use crate::act::{List, Panel};
        let focus = self.focused(view.id);
        // Copy mode's rows are looked for once the panes are sized
        // (`hold_copies`).
        let (kind, gone) = match &view.mode {
            Mode::Copy(copy) if !self.parts.panes().contains_key(&copy.pane) => (
                ScreenKind::Copy,
                "copy mode ended: its pane closed".to_owned(),
            ),
            Mode::Copy(copy) if Some(copy.pane) != focus => (
                ScreenKind::Copy,
                "copy mode ended: its pane is no longer focused".to_owned(),
            ),
            Mode::Panel(Panel::List(List {
                about: Some(about), ..
            })) if self.there(*about).is_err() => {
                (ScreenKind::Panel, format!("closed: {} is gone", about))
            }
            Mode::Normal | Mode::Panel(_) | Mode::Copy(_) => return None,
        };
        view.mode = Mode::Normal;
        view.notify(gone, true);
        Some(kind)
    }

    /// A PTY is the smallest rectangle any client shows it in; one with
    /// no cells shows it nowhere. A tab no client sizes whose layout changed
    /// is laid out on what its fit says, as commands resizing it are, and
    /// so is one with a size of its own that a client sized and none sizes
    /// now (it switched away, detached, or sizes nothing); otherwise its
    /// panes keep their size, so leaving a tab that fits clients resizes
    /// nothing. A tab that fits clients takes the smallest screen showing
    /// it as its size. Each tab is laid out as `shown_in` says, which a
    /// pane about to start is sized by too.
    fn size_panes(&mut self) {
        let mut sizes = std::mem::take(&mut self.shown_sizes);
        let mut placed = std::mem::take(&mut self.placed);
        sizes.clear();
        // Each tab a client sizes, and its smallest screen.
        let mut screens = BTreeMap::new();
        for view in self.views.values().filter(|v| v.sizes()) {
            let size = Self::screen_size(view).map(Size::nonzero);
            if let (Some(tab), Some((rows, cols))) = (self.shown_tab(view.id), size) {
                let (r, c) = screens.entry(tab.id()).or_insert((rows, cols));
                (*r, *c) = ((*r).min(rows), (*c).min(cols));
            }
        }
        for (&tab, &size) in &screens {
            if let Some(tab) = self.parts.tab_mut(tab) {
                tab.shown_on(size.into());
            }
        }
        let mut dropped = std::mem::take(&mut self.sized);
        let fixed =
            |tab: &TabId| (self.tab(*tab)).is_some_and(|t| matches!(t.fit(), Fit::Fixed(_)));
        dropped.retain(|tab| !screens.contains_key(tab) && fixed(tab));
        self.sized = screens.keys().copied().collect();
        let mut changed = (self.parts).changed_unshown(|tab| screens.contains_key(&tab));
        changed.retain(|tab| !dropped.contains(tab));
        for tab in screens.into_keys().chain(changed).chain(dropped) {
            if let Some(root) = self.root(tab) {
                self.shown_in((tab, root), |_, now| now, &mut placed, &mut sizes);
            }
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
            // A frame it holds is read as it resizes, as every read of
            // its output is (`read_with`): its terminal's facts given it,
            // its bells and copies taken.
            let Ok(size) = Size::new(rows, cols) else {
                continue;
            };
            if self.pane(id).is_ok_and(|p| p.size() != size) {
                self.read_with(id, |pane| pane.resize(size));
                resized = true;
            } else if let Ok(mut pane) = self.pane_mut(id) {
                pane.keeps_size();
            }
        }
        self.shown_sizes = sizes;
        if resized {
            self.touch();
        }
    }

    /// The rectangles, rows and columns, each pane of tab `tab` laid out as
    /// `root` is shown in, into `out` (empty ones left out): on the screen
    /// of each client that sizes the tab, zoomed and focusing as `seat`
    /// makes of what the client is now (zoomed, its focus there); with
    /// none, on what the tab's fit says. Settling sizes a pane to the
    /// smallest it has here, and leaves one with none at its size.
    fn shown_in(
        &self,
        (tab, root): (TabId, &Tree),
        seat: impl Fn(&View, (bool, Option<PaneId>)) -> (bool, Option<PaneId>),
        placed: &mut Placement,
        out: &mut Vec<(PaneId, (u16, u16))>,
    ) {
        let Some(t) = self.tab(tab) else {
            return;
        };
        let mut sizing = self.sizing(tab).peekable();
        let fit = sizing.peek().is_none().then(|| (t.fit().area(), None));
        let screens = sizing.map(|view| {
            let zoomed = match seat(view, (view.zoom, t.focus(view.id))) {
                (true, focus) => focus,
                (false, _) => None,
            };
            (view.pane_area(), zoomed)
        });
        for (area, zoomed) in screens.chain(fit) {
            match zoomed.filter(|_| !area.is_empty()) {
                Some(focus) => {
                    placed.clear();
                    placed.panes.push((focus, area));
                }
                None => layout::place_into(root, area, placed),
            }
            let shown = placed.panes.iter().filter(|(_, r)| !r.is_empty());
            out.extend(shown.map(|(p, r)| (*p, (r.h(), r.w()))));
        }
    }

    pub fn touch(&mut self) {
        self.views.values_mut().for_each(|view| view.dirty = true);
    }

    // ------------------------------------------------------------- closing

    /// Hangs up a pane's process; it is killed and reaped after `GRACE`.
    /// `closed`: fux closed it (`kill`, `kill-server`), not its program's
    /// end: the rest of its session goes too.
    fn end(&mut self, process: Process, closed: bool) {
        if let Process::Reading(mut child) | Process::HungUp(mut child) = process {
            if closed {
                child.leader.closed();
            }
            child.leader.hang_up();
            let deadline = crate::after(Instant::now(), GRACE);
            self.dying.push(Dying { child, deadline });
        }
    }

    /// Every program started and not yet reaped: the panes', those
    /// started for respawns under way, and the dying.
    pub fn children(&self) -> impl Iterator<Item = &Child> {
        let panes = (self.parts.panes().values()).filter_map(|p| p.process().child());
        let replacements = self.respawning.values().filter_map(|r| r.process.child());
        (panes.chain(replacements)).chain(self.dying.iter().map(|d| &d.child))
    }

    /// Removes a pane and its process. A tab its closing empties closes too,
    /// as does a workspace left with no tabs.
    pub fn close_pane(&mut self, id: PaneId, why: Option<String>) {
        // Who sees the pane is known before its tab can go: a tab its
        // closing empties takes its viewers elsewhere.
        let viewers = self.viewers(self.locate(id).map(|(_, t)| t));
        if let Some(pane) = self.parts.close_pane(id) {
            // Closed for its program's end (a reason given), or by fux.
            let closed = why.is_none();
            if let Some(why) = why {
                for client in &viewers {
                    self.notify_to(*client, format!("{id} {} {why}", pane.label()), false);
                }
            }
            self.end_pane(pane, closed, &viewers);
        }
        self.after_close();
    }

    /// Removes a tab and whatever panes are in it (`Parts::remove_tab`).
    fn remove_tab(&mut self, tab: TabId) {
        let viewers = self.viewers(Some(tab));
        for gone in self.parts.remove_tab(tab) {
            self.end_pane(gone, true, &viewers);
        }
    }

    /// Ends the program of `pane`, closed (by fux if `closed`), and the one
    /// a respawn under way there started, telling `viewers` the respawn
    /// was abandoned.
    fn end_pane(&mut self, pane: Pane, closed: bool, viewers: &[ClientId]) {
        let id = pane.id();
        self.end(pane.into_process(), closed);
        let Some(abandoned) = self.respawning.remove(&id) else {
            return;
        };
        self.end(abandoned.process, true);
        let told = format!("{id} closed while respawning: the program started for it was ended");
        for client in viewers {
            self.notify_to(*client, told.clone(), true);
        }
    }

    /// The clients showing tab `tab`.
    fn viewers(&self, tab: Option<TabId>) -> Vec<ClientId> {
        let shows = |c: &ClientId| tab.is_some_and(|t| self.shows(*c, t));
        self.views.keys().copied().filter(shows).collect()
    }

    fn after_close(&mut self) {
        let shutting = (self.outbox.iter()).any(|o| matches!(o, Outgoing::Shutdown { .. }));
        if self.parts.panes().is_empty() && !shutting {
            (self.outbox).push(Outgoing::Shutdown("the last pane closed".into()));
        }
        self.touch();
        self.settle();
    }

    /// A pane's program exited with `status`: a shell's pane closes, and a
    /// program's stays, the status kept, for its last screen to be read.
    /// What either wrote as it exited may still be on its terminal, or
    /// read and waiting for the pane's next turns (`Pane::has_unread`): it
    /// is told exited once all of it is on its screen, its bells, titles
    /// and notifications journaled, from the turn that reads the last of
    /// it (`Session::fire`); a shell's pane then closes.
    pub fn exited(&mut self, id: PaneId, ended: Ended) {
        let Some(first) = self.parts.panes().get(&id).map(|p| p.exiting.is_none()) else {
            return;
        };
        // Once, at the first word of the exit, its terminal drained (what
        // it left writing there is not waited for), and the status kept
        // till the last of it is read; at each, a frame it holds read, one
        // begun in output read since too.
        if first {
            self.read_left(id);
        }
        self.read_with(id, |p| p.release_frame());
        let Some(mut pane) = self.parts.pane_mut(id) else {
            return;
        };
        pane.exiting(ended);
        if pane.has_unread() {
            return;
        }
        match pane.runs {
            Runs::DefaultShell => {
                let left = pane.ended(ended).left.told("it");
                let why = process::shell_status(ended).map_or_else(
                    || process::told(ended),
                    |s| format!("exited with status {s}"),
                );
                self.close_pane(id, Some(format!("{why}{left}")));
            }
            Runs::Explicit(_) => {
                // Its leader, exited and unreaped, is reaped with the dying.
                let (end, gone) = pane.exit(ended);
                if let Process::Reading(child) | Process::HungUp(child) = gone {
                    let deadline = Instant::now();
                    self.dying.push(Dying { child, deadline });
                }
                let told = format!("{id} {}{}", process::told(end.ended), end.left.told("it"));
                let left = end.left != pane::Left::default();
                for client in self.viewers(self.locate(id).map(|(_, t)| t)) {
                    self.notify_to(client, told.clone(), left);
                }
                self.touch();
            }
        }
    }

    /// Reads what pane `id`'s program left on its terminal as it ends, or
    /// is respawned over (`unread`), up to `LEFT_MAX`; what is past it is
    /// not, which the pane keeps (`Pane::left`) to say with its end.
    fn read_left(&mut self, id: PaneId) {
        let Some((left, cut)) = self.pane(id).ok().map(|p| unread(p.process())) else {
            return;
        };
        if !left.is_empty() {
            self.output(id, &left);
        }
        if let (true, Ok(mut pane)) = (cut, self.pane_mut(id)) {
            pane.left_cut();
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
        self.parts.panes().values().flat_map(|pane| {
            let respawn = self.respawning.get(&pane.id()).map(|r| r.since);
            // A respawn's turn reads a held frame: one parser call a round.
            let frame = (pane.frame_deadline().filter(|_| respawn.is_none()))
                .map(|at| (at, Timer::Frame(pane.id())));
            let unread = (pane.unread_due().or(respawn)).map(|at| (at, Timer::Unread(pane.id())));
            frame.into_iter().chain(unread)
        })
    }

    /// What `timer` waited for is done, now that it is due.
    pub fn fire(&mut self, timer: Timer) {
        match timer {
            Timer::Frame(id) => self.read_with(id, |p| p.release_frame()),
            Timer::Unread(id) => {
                // A respawn under way reads a frame its old program holds.
                let frame = self.respawning.contains_key(&id)
                    && self.pane(id).is_ok_and(|p| p.frame_deadline().is_some());
                self.read_with(id, |p| match frame {
                    true => p.release_frame(),
                    false => p.read_unread(),
                });
                self.after_turn(id);
            }
        }
    }

    /// Reads into pane `id` with `read`, as every read of a pane's output
    /// does: its tab's host colours and palette given it first; after, its
    /// bell rung, its copies made in order, and the views showing it
    /// repainted and told what it refused (`tell_refused`).
    fn read_with(&mut self, id: PaneId, read: impl FnOnce(&mut PaneMut<'_>)) {
        let tab = self.locate(id).map(|(_, tab)| tab);
        let viewers = self.viewers(tab);
        let clipboard = self.config.clipboard;
        let Some((mut pane, retold)) = self.hosted(id) else {
            return;
        };
        read(&mut pane);
        // Sized once for the new cell and what was read, a resize sized
        // so already, which the system does not signal again.
        if retold {
            pane.size_terminal();
        }
        // A program's copies, as copies in copy mode, to the clients showing
        // it; none with `clipboard` false. Those that are no copy are
        // counted, to be told. A copy fits the clipboard: fux-vt keeps less
        // of a string.
        const _: () = assert!(fux_vt::OSC_PAYLOAD_LIMIT <= crate::copy::MAX_CLIPBOARD);
        let (bell, copies) = pane.take_read(clipboard);
        if bell {
            self.ring(id, Instant::now());
        }
        for text in copies {
            _ = self.copied(text, &viewers);
        }
        for view in (self.views.values_mut()).filter(|v| viewers.contains(&v.id)) {
            view.dirty = true;
        }
        self.tell_refused(id, &viewers);
    }

    /// Tells the clients showing each pane what it refused and has not
    /// told (`tell_refused`); whether it told any. A pane no client shows
    /// tells it once one does.
    pub(crate) fn tell_lost(&mut self) -> bool {
        let untold: Vec<PaneId> = (self.parts.panes().iter())
            .filter(|(_, p)| p.loses())
            .map(|(id, _)| *id)
            .collect();
        let mut told = false;
        for id in untold {
            let viewers = self.viewers(self.locate(id).map(|(_, tab)| tab));
            told |= self.tell_refused(id, &viewers);
        }
        told
    }

    /// Tells `viewers`, the clients showing pane `id`, what it let go of
    /// and has not told, each kind with how many (`Pane::take_untold`);
    /// whether there was any. With no client showing it, it is kept for
    /// one that does.
    fn tell_refused(&mut self, id: PaneId, viewers: &[ClientId]) -> bool {
        let pane = self.parts.pane_mut(id);
        let Some(mut pane) = pane.filter(|p| p.loses() && !viewers.is_empty()) else {
            return false;
        };
        let told = pane.take_untold().told(id, true).join("; ");
        for client in viewers {
            self.notify_to(*client, told.clone(), true);
        }
        true
    }

    /// `text`, copied, goes to the paste buffers and, unless `clipboard` is
    /// false, to the clipboard of each client in `to` (OSC 52): false if it
    /// is too large for the clipboard.
    pub(crate) fn copied(&mut self, text: String, to: &[ClientId]) -> bool {
        let mut fits = true;
        // Encoded for the clipboard before the text moves into the buffers.
        if self.config.clipboard {
            let encoded = crate::json::base64(text.as_bytes());
            fits = encoded.len() <= crate::copy::MAX_CLIPBOARD;
            if fits {
                let osc = format!("\x1b]52;c;{encoded}\x07").into_bytes();
                (self.outbox).extend(to.iter().map(|&client| Outgoing::Copy(client, osc.clone())));
            }
        }
        self.buffers.push_front(text);
        self.buffers.truncate(self.config.buffers.get());
        fits
    }

    /// Everything the server shuts down: every pane is hung up.
    pub fn shutdown(&mut self) {
        for pane in self.parts.close_all() {
            self.end_pane(pane, true, &[]);
        }
    }

    // ------------------------------------------------------------ commands

    /// Begins a request from a client: one that waits gives its wait,
    /// which the server asks for its answer until it has one; a stream of
    /// events, its subscription; any other runs at once.
    pub fn begin(&mut self, request: Request, origin: &Origin) -> Begun {
        self.begin_request(request, origin)
            .unwrap_or_else(|error| Begun::Done(failure(error, origin)))
    }

    /// `begin`'s work: what fails before a request runs is its error.
    fn begin_request(&mut self, request: Request, origin: &Origin) -> Result<Begun, Error> {
        let now = Instant::now();
        let wait = match request {
            Request::Do(command) => {
                let reply = self.run_command(&command, origin);
                // A respawn is answered once its pane is at the version it
                // answers: at once, unless what the old program left is
                // still being read.
                let waiting = match (&command, &reply.end, &reply.body) {
                    (
                        Command::Respawn { .. },
                        End::Done,
                        Body::Pane(PaneAnswer::Acted(started)),
                    ) => Waiting::Respawn {
                        pane: started.pane,
                        respawns: started.counts.respawns,
                        reply: reply.clone(),
                    },
                    _ => return Ok(Begun::Done(reply)),
                };
                return Ok(self.answered(waiting, now));
            }
            Request::Wait(wait) => wait,
            Request::Events(target, from) => {
                let target = target.map(|t| self.there(t)).transpose()?;
                let journal = self.parts.journal();
                let subscription =
                    crate::events::Subscription::new(target, journal, self.run, from)
                        .map_err(|why| fail(Raise::BadValue, why))?;
                return Ok(Begun::Events(Box::new(subscription)));
            }
        };
        let watches = match wait.since {
            command::Since::Now(AnyRef::Pane(id)) => Watches::Pane((id, self.pane(id)?.counts())),
            command::Since::Now(target) => Watches::Panes(
                (self.panes_of(target)?.into_iter())
                    .map(|id| Ok((id, self.pane(id)?.counts())))
                    .collect::<Result<Vec<_>, Error>>()?,
            ),
            command::Since::Version(since) => {
                Watches::Pane((since.pane, self.counts_since(since)?))
            }
        };
        // A wait on no pane would only time out.
        if let Watches::Panes(none) = &watches
            && none.is_empty()
        {
            return Err(fail(Raise::NoPane, "it holds no pane to wait on"));
        }
        let limit = wait.timeout.unwrap_or(self.config.wait).duration();
        Ok(self.answered(Waiting::new(watches, self.run, now, limit), now))
    }

    /// `waiting`'s answer, if it has one already at `now`; else the wait.
    fn answered(&self, waiting: Waiting, now: Instant) -> Begun {
        match waiting.answer(self, now) {
            Some(outcome) => Begun::Done(outcome),
            None => Begun::Waiting(Box::new(waiting)),
        }
    }

    /// The counts a wait from `since` waits to differ from: its own, if it
    /// is a version of this server run's that its pane has had. A pane this
    /// run gave that is gone closed, as ids are never given twice in one
    /// run, and the wait says so; one it never gave is refused.
    fn counts_since(&self, since: command::Version) -> Result<command::Counts, Error> {
        if since.run != self.run {
            let why = format!(
                "{since} is from another server run ({}), not this one ({}): a server since stopped, or another socket's",
                since.run, self.run
            );
            return Err(fail(Raise::BadValue, why));
        }
        if !self.ids.gave(since.pane) {
            let why = format!("{since}: {} was never a pane of this run", since.pane);
            return Err(fail(Raise::BadValue, why));
        }
        // Within one program every count only grows.
        if let Some(pane) = self.parts.panes().get(&since.pane)
            && !pane.counts().had(since.counts)
        {
            let why = format!(
                "{since}: {} never had these counts; it is at {}",
                since.pane,
                pane.counts()
            );
            return Err(fail(Raise::BadValue, why));
        }
        Ok(since.counts)
    }

    /// Runs a command, parsed already: from a line, a binding or a chooser.
    pub fn run_command(&mut self, command: &Command, origin: &Origin) -> Reply {
        let outcome = self
            .execute(command, origin)
            .unwrap_or_else(|error| failure(error, origin));
        if !command.shows_nothing_new() {
            self.changes = self.changes.wrapping_add(1);
            self.touch();
        }
        self.settle();
        outcome
    }

    /// How many commands have run that may change what clients show.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    fn execute(&mut self, command: &Command, origin: &Origin) -> Result<Reply, Error> {
        let here = self.here(origin)?;
        let starting = self.starting_for(origin);
        match command {
            Command::Ls { target, get } => self.ls(*target, get.as_deref()),
            &Command::KillServer { force } => self.kill_server(force, origin),
            Command::NewWorkspace {
                name,
                size,
                cwd,
                env,
            } => {
                let (cwd, fit, sized) = self.opening((cwd.as_deref(), *size), (here, true))?;
                let new = NewPane::new(Start::Shell(env.as_slice()), &cwd, &starting, sized);
                let made = self.create_workspace(name.clone(), None, fit, new)?;
                self.follow(here, made.ws, None);
                Ok(acted(self.pane(made.pane)?.version(self.run)))
            }
            Command::NewTab {
                target,
                name,
                size,
                cwd,
                env,
            } => {
                let (cwd, fit, sized) = self.opening((cwd.as_deref(), *size), (here, true))?;
                if let Some(name) = name {
                    self.check_tab_name(name, None)?;
                }
                let new = NewPane::new(Start::Shell(env.as_slice()), &cwd, &starting, sized);
                // A workspace named that is not there is made, as `run`
                // makes one, its first tab this one.
                let made = self.create_tab(target.as_ref(), here, name.clone(), fit, new)?;
                if let Some(name) = name {
                    self.give_tab_name(made.tab, name.clone())?;
                }
                let at = Spot {
                    tab: made.tab,
                    pane: None,
                };
                self.follow(here, made.ws, Some(at));
                Ok(acted(self.pane(made.pane)?.version(self.run)))
            }
            Command::Run {
                workspace,
                name,
                size,
                cwd,
                env,
                argv,
            } => {
                // Its place is out of every client's way: no client follows.
                let (dir, fit, sized) = self.opening((cwd.as_deref(), *size), (here, false))?;
                // A name given is one `str NAME tab` finds: it must be the only one.
                if let Some(name) = name {
                    self.check_tab_name(name, None)?;
                }
                // Named after its program, if that makes a name.
                let derived = Some(pane::named(argv)).filter(|n| check_name(n).is_ok());
                let new =
                    NewPane::new(Start::Program(argv, env.as_slice()), &dir, &starting, sized);
                let workspace = (workspace.clone()).unwrap_or(WsRef::Name(RUN_WORKSPACE.into()));
                let tab_name = name.clone().or(derived);
                let made = self.create_tab(Some(&workspace), here, tab_name, fit, new)?;
                if let Some(given) = name {
                    self.give_tab_name(made.tab, given.clone())?;
                }
                Ok(acted(self.pane(made.pane)?.version(self.run)))
            }
            &Command::Respawn {
                target,
                kill,
                ref env,
                ref argv,
            } => self.respawn(target, kill, env.as_slice(), argv, &starting),
            Command::Notify { target, text } => {
                let to: Vec<ClientId> = match target {
                    None => self.views.keys().copied().collect(),
                    Some(target) => match self.there(*target)? {
                        AnyRef::Pane(p) => self.viewers(self.locate(p).map(|(_, t)| t)),
                        AnyRef::Tab(t) => self.viewers(Some(t)),
                        AnyRef::Workspace(w) => (self.workspace(w))
                            .map(|w| w.viewers.iter().copied().collect())
                            .unwrap_or_default(),
                    },
                };
                for client in to {
                    self.notify_to(client, text.clone(), false);
                }
                Ok(Reply::default())
            }
            &Command::Resize(Resized::Tabs(target, size)) => {
                self.resize_tabs(target, size)?;
                Ok(Reply::default())
            }
            &Command::Resize(Resized::Client(client, size)) => {
                let view = self.views.get(&client).ok_or(Error::NoClient(client))?;
                if view.kind != ClientKind::Headless {
                    let message = format!(
                        "{client} has a terminal, which sets its size; resize sizes headless clients"
                    );
                    return Err(fail(Raise::NotHeadless, message));
                }
                let size = size.size();
                self.resize(client, size.rows(), size.cols())?;
                Ok(Reply::default())
            }
            Command::DumpLayout { target } => {
                let only = target.map(|t| self.there(t)).transpose()?;
                Ok(Body::Text(self.dump_layout(only)).into())
            }
            &Command::SendMouse {
                target,
                mouse,
                row,
                col,
            } => self.send_mouse(target, mouse, row, col),
            &Command::Split {
                axis,
                target,
                ref argv,
                ref cwd,
                ref env,
                shares,
            } => {
                let start = match argv.is_empty() {
                    true => Start::Shell(env.as_slice()),
                    false => Start::Program(argv, env.as_slice()),
                };
                self.split(target, (axis, shares), start, cwd.as_deref(), origin)
            }
            &Command::Signal { target, signal } => {
                let group = self.signalled(target)?;
                let version = self.pane(target)?.version(self.run);
                fuxix::process::kill_group(group, signal)
                    .map_err(|e| fail(Raise::Signal, format!("sending the signal: {e}")))?;
                Ok(acted(version))
            }
            &Command::Kill { target } => self.kill(target).map(|()| Reply::default()),
            Command::Rename { target, name } => {
                let target = self.there(*target)?;
                self.rename(target, name.clone()).map(|()| Reply::default())
            }
            &Command::MovePane { target, ref to } => {
                self.move_pane(target, to, here).map(|()| Reply::default())
            }
            &Command::SwapPane { target, with } => {
                let source = self.pane(target)?.id();
                let other = match with {
                    SwapWith::Pane(p) => self.pane(p)?.id(),
                    SwapWith::Toward(direction) => self.neighbor(source, direction, here)?,
                };
                self.parts.swap(source, other);
                Ok(Reply::default())
            }
            &Command::ResizePane { target, by } => self.resize_pane(target, by, here),
            &Command::ClearHistory { target } => {
                self.pane_mut(target)?.clear_history();
                Ok(Reply::default())
            }
            &Command::SendKeys { to, ref inputs } => self.send_keys(to, inputs),
            &Command::CapturePane {
                target,
                history,
                view,
            } => {
                let pane = self.pane(target)?;
                if view.source == command::Source::LastOutput && !pane.marked() {
                    return Err(fail(
                        Raise::NoMarks,
                        format!("{target}: {}", pane::UNMARKED),
                    ));
                }
                let copying = self.copying(pane.id());
                let captured = crate::capture::pane(pane, self.run, history, view, copying);
                Ok(Body::Capture(Box::new(captured)).into())
            }
            &Command::Reorder { ref target, toward } => {
                let target = self.there(*target)?;
                self.reorder(target, toward).map(|()| Reply::default())
            }
            Command::Set(settings) => {
                // `run_command` marks every view to paint, as after any
                // command that can show something new: `titles` shows at
                // each client's next paint. Each value is one its setting
                // takes, so all are in force, or the request never came.
                for setting in settings {
                    self.config.apply(setting.clone());
                }
                // Every pane keeps the history they say.
                let history = self.config.history_bytes.get();
                self.parts.each_pane_mut(|p| p.set_history_bytes(history));
                Ok(Reply::default())
            }
            Command::Resolve { text, kind } => Ok(found(self.resolve(text, *kind, here)?)),
            Command::PaneNamed { name } => Ok(found(AnyRef::Pane(self.pane_named(name)?))),
            &Command::Members { target, members } => {
                let ids: Vec<AnyRef> = match members {
                    Members::Panes => (self.panes_of(target)?.into_iter())
                        .map(AnyRef::Pane)
                        .collect(),
                    Members::Tabs => (self.tabs_of(target)?.into_iter())
                        .map(AnyRef::Tab)
                        .collect(),
                };
                Ok(Body::Handles(ids).into())
            }
            &Command::ListBuffers => {
                let line = |(i, text): (usize, &String)| {
                    let preview: String = (text.chars())
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .take(50)
                        .collect();
                    format!("{i}: {} bytes: {preview}\n", text.len())
                };
                let list: String = self.buffers.iter().enumerate().map(line).collect();
                Ok(Body::Text(list).into())
            }
            &Command::ShowBuffer { index } => (self.buffers.get(index).cloned())
                .map(|text| Body::Text(text).into())
                .ok_or(Error::NoBuffer(index)),
            &Command::PasteBuffer { index, target } => {
                let text = self.buffers.get(index).cloned();
                let paste = Input::Paste(text.ok_or(Error::NoBuffer(index))?);
                self.send_keys(command::To::Now(AnyRef::Pane(target)), &[paste])
            }
            &Command::Client { client, ref action } => self.client(client, action, here),
        }
    }

    /// What `new-workspace`, `new-tab` and `run` do before their pane
    /// starts: the directory it starts in found,
    /// and its tab's fit and the size settling gives its pane: if `follows`,
    /// its client then shows it, on its screen, in the cell its terminal
    /// said; else its fit's, in no cell, as no terminal draws it.
    fn opening(
        &mut self,
        (dir, size): (Option<&Path>, command::Size),
        (here, follows): (Here, bool),
    ) -> Result<(PathBuf, Fit, Starts), Error> {
        let cwd = self.start_dir(here, dir, None)?;
        let fit = self.new_fit(size, here)?;
        let follower = (here.client().filter(|_| follows)).and_then(|c| self.views.get(&c));
        let screen = (follower.filter(|view| view.sizes())).and_then(Self::screen_size);
        let cell = follower.and_then(|view| view.terminal.cell);
        Ok((cwd, fit, (screen.unwrap_or(fit.size()), cell)))
    }

    /// `ls`: everything there is, or `target` and what is in it; or one
    /// field of the target's (`get`).
    fn ls(&mut self, target: Option<AnyRef>, get: Option<&str>) -> Result<Reply, Error> {
        // Where a stream of events reads from, read with the listing: one
        // from it misses nothing after unsaid.
        let events = self.parts.journal().end(self.run);
        Ok(match target {
            Some(target) => self.ls_one(self.there(target)?, get)?,
            None => Body::Listing {
                of: None,
                listing: self.ls_json(events),
            },
        }
        .into())
    }

    /// `kill-server`: from the CLI, refused from one of the server's own
    /// panes or with people attached, unless `force`.
    fn kill_server(&mut self, force: bool, origin: &Origin) -> Result<Reply, Error> {
        if let (Origin::Cli(caller), false) = (origin, force) {
            let pane = caller.pane().filter(|p| self.parts.panes().contains_key(p));
            if pane.is_some() || caller.within == Within::This {
                let pane = pane.map_or_else(String::new, |p| format!(" {p},"));
                let message = format!(
                    "this runs in{pane} a pane of the server it would stop, with everything \
                     else in it; force-kill-server stops it anyway"
                );
                return Err(fail(Raise::OwnServer, message));
            }
            let people = (self.views.values())
                .filter(|v| v.kind == ClientKind::Terminal)
                .count();
            if people > 0 {
                let who = match people {
                    1 => "a person is".to_owned(),
                    n => format!("{n} people are"),
                };
                let message =
                    format!("{who} attached to this server; force-kill-server stops it anyway");
                return Err(fail(Raise::PeopleAttached, message));
            }
        }
        let reason = match origin {
            Origin::Client(_) => "stopped by kill-server, from a key or the prompt",
            Origin::Cli(_) => "stopped by fux kill-server",
        };
        self.outbox.push(Outgoing::Shutdown(reason.into()));
        Ok(Reply::default())
    }

    /// `resize` of `target`'s tabs: laid out on `size`, or with none on
    /// their clients' screens. A size that would hide one of a tab's
    /// panes, as a split that would is, is refused.
    fn resize_tabs(&mut self, target: AnyRef, size: command::Size) -> Result<(), Error> {
        let size = size.map(view::Screen::size);
        let tabs = self.tabs_of(target)?;
        if let Some(size) = size {
            let area = Fit::Fixed(size).area();
            for tab in &tabs {
                let Some(root) = self.root(*tab) else {
                    continue;
                };
                if !layout::shows_all(root, area) {
                    let message = format!(
                        "{tab} cannot show its {} panes at that size; close some, or choose a bigger one",
                        root.panes().len()
                    );
                    return Err(fail(Raise::Size, message));
                }
            }
        }
        // With no size, back to fitting clients, at the size it has until
        // one shows it.
        for tab in tabs {
            let fit = |fit: Fit| size.map_or(Fit::Clients(fit.size()), Fit::Fixed);
            self.parts.set_fit(tab, fit);
        }
        Ok(())
    }

    /// `send-mouse`: `mouse` at `row` and `col` of pane `target`, encoded
    /// as its program asked for mouse reports.
    fn send_mouse(
        &mut self,
        target: PaneId,
        mouse: Mouse,
        row: u16,
        col: u16,
    ) -> Result<Reply, Error> {
        use fux_vt::MouseProtocolMode as Asked;
        use fux_vt::keys::mouse::MouseAction::{Motion, Press, Release};
        let run = self.run;
        let mut p = self.live_pane_mut(target)?;
        let size = p.size();
        if row >= size.rows() || col >= size.cols() {
            let (rows, cols) = (size.rows(), size.cols());
            let message = format!(
                "{} is {cols}x{rows}: rows count 0-{} and columns 0-{}",
                p.id(),
                rows.saturating_sub(1),
                cols.saturating_sub(1)
            );
            return Err(fail(Raise::Mouse, message));
        }
        let event = |action| fux_vt::keys::mouse::MouseEvent {
            action,
            mods: fux_vt::keys::Modifiers::default(),
            row,
            col,
        };
        // A click is a press and, where the program takes them (not in X10
        // mode), a release; a move while a button is held is a drag.
        // Released, or clicked, it is held no more; another button's release
        // leaves it held. X10 reports presses alone: a click is its press,
        // and a release or the wheel, which it cannot say, is refused.
        let unheld = |button| p.mouse_held.filter(|held| *held != button);
        let (events, held, beyond_x10) = match mouse {
            Mouse::Click(button) => {
                let button = button.reported();
                let events = vec![event(Press(button)), event(Release(Some(button)))];
                (events, unheld(button), false)
            }
            Mouse::Press(button) => {
                let button = button.reported();
                (vec![event(Press(button))], Some(button), false)
            }
            Mouse::Wheel(way) => (vec![event(Press(way.reported()))], p.mouse_held, true),
            Mouse::Release(button) => {
                let button = button.reported();
                (vec![event(Release(Some(button)))], unheld(button), true)
            }
            Mouse::Move => (vec![event(Motion(p.mouse_held))], p.mouse_held, false),
        };
        let mode = p.screen().mouse_protocol_mode();
        let x10 = mode == Asked::Press;
        let pane = p.id();
        if x10 && beyond_x10 {
            let message = format!(
                "the program in {pane} asked for X10 mouse reports: presses alone, no releases or wheel"
            );
            return Err(fail(Raise::Mouse, message));
        }
        let mut report = Vec::new();
        for event in events {
            let release = matches!(event.action, Release(_));
            if p.screen().encode_mouse(event, &mut report) || (release && x10) {
                continue;
            }
            let message = match (mode, event.action) {
                (Asked::None, _) => {
                    format!("the program in {pane} has not asked for mouse reports")
                }
                (_, Motion(_)) => format!(
                    "the program in {pane} takes no motion here: with no button pressed it asked for none, \
                     and asking for clicks alone (mode 1000) it takes no drags either"
                ),
                (_, Press(_) | Release(_)) => format!(
                    "the mouse encoding the program in {pane} asked for cannot say row {row}, column {col}"
                ),
            };
            return Err(fail(Raise::Mouse, message));
        }
        let version = p.version(run);
        p.push_mouse(report, held)?;
        Ok(acted(version))
    }

    /// `split`: a new pane beside `target`, along `axis`, the two taking
    /// `shares` if given, running `start` in `dir`, with the id `id` asked
    /// for.
    fn split(
        &mut self,
        target: PaneId,
        (axis, shares): (Axis, Option<(NonZeroU32, NonZeroU32)>),
        start: Start<'_>,
        dir: Option<&Path>,
        origin: &Origin,
    ) -> Result<Reply, Error> {
        let (here, starting) = (self.here(origin)?, self.starting_for(origin));
        let target = self.pane(target)?.id();
        let cwd = self.start_dir(here, dir, Some(target))?;
        // The splitting client follows the new pane, if it shows its tab;
        // from the CLI, the clients that focus the split one.
        let tab = self.locate(target).map(|(_, tab)| tab);
        let shows = |c: &ClientId| tab.is_some_and(|t| self.shows(*c, t));
        let shown = |c: &ClientId| self.focused(*c) == Some(target);
        let followers: Vec<ClientId> = match here {
            Here::Client(client, _) => [client].into_iter().filter(shows).collect(),
            Here::Cli(..) => self.views.keys().copied().filter(shown).collect(),
        };
        // Its tab found (by `split_size`) before the program starts, which
        // nothing can then leave running.
        let (split, size) = self.split_size(target, (axis, shares), &followers)?;
        let mut ids = self.ids.clone();
        let new = NewPane::new(start, &cwd, &starting, (size, self.graphics(target).cell()));
        let pane = self.new_pane(&mut ids, new)?;
        let new = pane.id();
        if let Err(pane) = self.parts.split(pane, split) {
            self.end(pane.into_process(), true);
            return Err(Error::NoPane(target));
        }
        self.ids = ids;
        let (_, tab) = self.locate(new).ok_or(Error::NoPane(new))?;
        let split = self.parts.tab_mut(tab).ok_or(Error::NoTab(tab))?;
        for view in (self.views.values_mut()).filter(|v| followers.contains(&v.id)) {
            split.set_focus(view.id, new);
            view.zoom = false;
        }
        let version = self.pane(new)?.version(self.run);
        Ok(acted(version))
    }

    /// `kill`: a pane closed, or a tab's or workspace's every pane.
    fn kill(&mut self, target: AnyRef) -> Result<(), Error> {
        match self.there(target)? {
            AnyRef::Pane(pane) => self.close_pane(pane, None),
            target @ (AnyRef::Tab(_) | AnyRef::Workspace(_)) => {
                for tab in self.tabs_of(target).unwrap_or_default() {
                    self.remove_tab(tab);
                }
                self.after_close();
            }
        }
        Ok(())
    }

    /// `resize-pane`: a border of pane `pane` moved, or the pane made a
    /// size, its tab laid out as for the command.
    fn resize_pane(&mut self, pane: PaneId, by: PaneResize, here: Here) -> Result<Reply, Error> {
        let (_, tab) = self.locate(pane).ok_or(Error::NoPane(pane))?;
        let area = self.reference_area(tab, here).ok_or(Error::NoPane(pane))?;
        // A pane hidden has no place to resize from (`layout::resize`).
        let root = self.root(tab).ok_or(Error::NoTab(tab))?;
        if !layout::shows_all(root, area) {
            let (cols, rows) = (area.w(), area.h());
            let message = format!("{tab} does not show every pane on {cols}x{rows}");
            return Err(fail(Raise::Size, message));
        }
        match by {
            PaneResize::Border(direction, amount) => {
                let resize = |root: &mut Tree| layout::resize(root, area, pane, direction, amount);
                let way = direction.name();
                let why = match self.parts.reshape(pane, resize) {
                    // A move of 0 moves nothing, as asked.
                    Some(Some(1..)) => return Ok(Reply::default()),
                    Some(Some(0)) if amount == 0 => return Ok(Reply::default()),
                    Some(Some(0)) => "the panes it would move are at their least",
                    Some(None) | None => "it has no border there",
                };
                Err(fail(
                    Raise::NothingThere,
                    format!("{pane} cannot move {way}: {why}"),
                ))
            }
            PaneResize::To(axis, extent) => {
                (self.resize_pane_to(pane, area, axis, extent)).map(|()| Reply::default())
            }
        }
    }

    /// A command on a client's screen: the client named, else the one the
    /// command came from. Targets it leaves out are the client's own.
    fn client(
        &mut self,
        client: Option<ClientId>,
        action: &ClientAction,
        here: Here,
    ) -> Result<Reply, Error> {
        let (client, place) = match (client, here) {
            (Some(c), _) => (c, self.place(c).ok_or(Error::NoClient(c))?),
            (None, Here::Client(client, place)) => (client, place),
            (None, Here::Cli(..)) => {
                return Err(fail(
                    Raise::NoTarget,
                    "this acts on a client's screen: name one (`str c1 client`; `fux ls` lists them)",
                ));
            }
        };
        let view = || self.views.get(&client).ok_or(Error::NoClient(client));
        let picked = match *action {
            ClientAction::Choices(choices) => {
                return crate::overlay::choices(self, client, choices);
            }
            ClientAction::Run(ref code) => {
                let run = ToAttached::Run(code.clone());
                self.outbox.push(Outgoing::To(client, run));
                return Ok(Reply::default());
            }
            // Taken as typed at its terminal: its bindings see them. No
            // pane answers: they go to the pane the client chooses.
            ClientAction::Keys(ref inputs) => {
                let keys = ToAttached::Keys(inputs.clone());
                self.outbox.push(Outgoing::To(client, keys));
                return Ok(Body::Panes(Vec::new()).into());
            }
            ClientAction::Detach => {
                self.detach(client);
                let exit = ToAttached::Exit("detached");
                self.outbox.push(Outgoing::To(client, exit));
                return Ok(Reply::default());
            }
            ClientAction::Capture => {
                let mut grid = crate::render::Grid::new(0, 0);
                let placement = &mut Placement::default();
                crate::render::compose_view(self, view()?, &mut grid, placement);
                let captured = crate::capture::client(client, &grid);
                return Ok(Body::Capture(Box::new(captured)).into());
            }
            ClientAction::Zoom => None,
            ClientAction::SelectPane(pick) => Some(self.pick_pane(view()?, place, pick)?),
            ClientAction::SelectTab(pick) => Some(self.pick_tab(place, pick)?),
            ClientAction::SelectWorkspace(ref pick) => Some(self.pick_ws(view()?, pick)?),
        };
        // A select shows the client where it picked, unzoomed.
        match picked {
            Some(Picked { ws, at }) => self.follow(Here::Client(client, place), ws, at),
            None => (self.views.get_mut(&client))
                .into_iter()
                .for_each(|v| v.zoom = !v.zoom),
        }
        Ok(Reply::default())
    }

    fn rename(&mut self, target: AnyRef, name: String) -> Result<(), Error> {
        check_name(&name)?;
        match target {
            AnyRef::Pane(p) => self.pane_mut(p)?.rename(name),
            AnyRef::Tab(t) => {
                self.check_tab_name(&name, Some(t))?;
                self.give_tab_name(t, name)?;
            }
            AnyRef::Workspace(id) => {
                // The one named (there: `Rename` checks it first), and the
                // others, whose names it cannot take.
                let others = self.parts.workspaces().iter().filter(|w| w.id() != id);
                check_workspace_name(&name, others)?;
                self.parts
                    .rename_workspace(id, name)
                    .ok_or(Error::NoWorkspace(id))?;
            }
        }
        Ok(())
    }

    /// The pane beside `from` toward `direction`, its tab laid out as for
    /// the command.
    fn neighbor(&self, from: PaneId, direction: Direction, here: Here) -> Result<PaneId, Error> {
        let found = self.locate(from).and_then(|(_, tab)| {
            let area = self.reference_area(tab, here)?;
            layout::neighbor(&layout::place(self.root(tab)?, area), from, direction)
        });
        found.ok_or(Error::NoNeighbor { from, direction })
    }

    fn move_pane(&mut self, target: PaneId, to: &MoveTo, here: Here) -> Result<(), Error> {
        let pane = self.pane(target)?.id();
        // A tab made for it fits clients at its tab's size until one shows it.
        let (from_ws, from) = self.locate(pane).ok_or(Error::NoPane(pane))?;
        let fit = Fit::Clients(self.tab(from).ok_or(Error::NoTab(from))?.fit().size());
        let mut ids = self.ids.clone();
        let (ws, dest) = match to {
            &MoveTo::Beside(direction) => {
                let destination = self.neighbor(pane, direction, here)?;
                let root = self.root(from).ok_or(Error::NoTab(from))?;
                let root = layout::move_beside(root, pane, destination, direction)
                    .ok_or(Error::NoPane(pane))?;
                self.room(from, &root, None)?;
                self.parts.reshape(pane, |old| *old = root);
                return Ok(());
            }
            &MoveTo::Tab(tab) => (self.tab_workspace(tab)?, Destination::Tab(tab)),
            MoveTo::Workspace(r) => {
                let ws = self.resolve_ws(r)?;
                let first = self.workspace(ws).and_then(|w| w.tabs().first());
                let tab = first.ok_or(Error::NoWorkspace(ws))?.id();
                (ws, Destination::Tab(tab))
            }
            // A tab made for it holds it alone, which shows on every size
            // it is laid out on (each has a cell: its fit's, and a screen
            // that sizes panes, `View::sizes`): nothing after refuses the
            // move.
            MoveTo::NewTab => (from_ws, Destination::NewTab(from_ws, ids.tab()?, fit)),
            MoveTo::NewWorkspace => {
                let (id, tab) = (ids.workspace()?, ids.tab()?);
                let first = (tab, MAIN.into(), fit);
                (
                    id,
                    Destination::NewWorkspace((id, self.workspace_name(id)), first),
                )
            }
        };
        let tab = match &dest {
            Destination::Tab(tab) if *tab == from => return Ok(()),
            Destination::Tab(tab) => {
                let mut root = self.root(*tab).ok_or(Error::NoTab(*tab))?.clone();
                root.append(pane);
                self.room(*tab, &root, here.client())?;
                *tab
            }
            Destination::NewTab(_, tab, _) | Destination::NewWorkspace(_, (tab, ..)) => *tab,
        };
        (self.parts.relocate(pane, dest)).ok_or(Error::NoTab(tab))?;
        // Both IDs or neither: one taken alone would be lost.
        self.ids = ids;
        // The moving client follows its pane.
        let pane = Some(pane);
        self.follow(here, ws, Some(Spot { tab, pane }));
        Ok(())
    }

    /// Refused unless tab `tab` laid out as `root` shows every pane on each
    /// size it is laid out on: its own, which a dump makes it at (`size`,
    /// or the screens it last fitted), and the screen of each client
    /// showing it that sizes its panes, `follower` too, which is about to.
    /// So a split or a move hides no pane, and a dump can make every
    /// layout there is again.
    fn room(&self, tab: TabId, root: &Tree, follower: Option<ClientId>) -> Result<(), Error> {
        let own = self.tab(tab).ok_or(Error::NoTab(tab))?.fit().area();
        let follower = follower
            .and_then(|c| self.views.get(&c))
            .filter(|v| v.sizes());
        let screens = (self.sizing(tab).chain(follower)).map(|v| (Some(v.id), v.pane_area()));
        let mut sizes = std::iter::once((None, own)).chain(screens);
        let Some((client, area)) = sizes.find(|(_, a)| !layout::shows_all(root, *a)) else {
            return Ok(());
        };
        let (cols, rows) = (area.w(), area.h());
        let on = match client {
            Some(client) => format!("{client}'s screen, {cols}x{rows}"),
            None => format!("its size, {cols}x{rows} (`TAB COLS ROWS resize` makes it bigger)"),
        };
        let message = format!("{tab} has no room to show every pane laid out so on {on}");
        Err(fail(Raise::Size, message))
    }

    fn reorder(&mut self, target: AnyRef, toward: Sibling) -> Result<(), Error> {
        let moved = match target {
            AnyRef::Pane(p) => {
                let panes = self.locate(p).and_then(|(_, t)| self.root(t));
                let panes = panes.map_or_else(Vec::new, Tree::panes);
                let index = panes.iter().position(|x| *x == p);
                let other = index
                    .and_then(|i| match toward {
                        Sibling::Next => i.checked_add(1),
                        Sibling::Previous => i.checked_sub(1),
                    })
                    .and_then(|i| panes.get(i));
                let other = other.copied().ok_or(Error::AtEnd(Kind::Pane))?;
                self.parts.swap(p, other);
                return Ok(());
            }
            AnyRef::Tab(t) => self.parts.reorder_tab(t, toward).ok_or(Error::NoTab(t))?,
            AnyRef::Workspace(w) => {
                (self.parts.reorder_workspace(w, toward)).ok_or(Error::NoWorkspace(w))?
            }
        };
        moved.then_some(()).ok_or(Error::AtEnd(target.kind()))
    }

    /// The pane `focus` picks for client `view`, which is at `at`.
    fn pick_pane(&self, view: &View, at: Place, pick: PanePick) -> Result<Picked, Error> {
        let client = view.id;
        let tab = self.tab(at.tab).ok_or(Error::NoTab(at.tab))?;
        let panes = tab.root().panes();
        let (current, last) = (tab.focus(client), tab.seat(client).and_then(Seat::last));
        let target = match pick {
            PanePick::Id(p) => p,
            PanePick::Step(toward) => {
                let index = current.and_then(|c| panes.iter().position(|p| *p == c));
                let next = round(&panes, index.unwrap_or(0), toward);
                *next.ok_or(Error::OnlyOne(Kind::Pane))?
            }
            PanePick::Last => {
                last.ok_or_else(|| fail(Raise::NoPane, "no previously focused pane"))?
            }
            PanePick::Toward(direction) => {
                let from = current.ok_or_else(|| fail(Raise::NoPane, "no pane focused"))?;
                let placement = layout::place(tab.root(), view.pane_area());
                layout::neighbor(&placement, from, direction)
                    .ok_or(Error::NoNeighbor { from, direction })?
            }
        };
        let (ws, tab) = self.locate(target).ok_or(Error::NoPane(target))?;
        let pane = Some(target);
        Ok(Picked {
            ws,
            at: Some(Spot { tab, pane }),
        })
    }

    /// The tab `show-tab` picks for a client at `at`.
    fn pick_tab(&self, at: Place, to: Pick<TabId>) -> Result<Picked, Error> {
        let (ws, target) = match to {
            Pick::Id(t) => (self.tab_workspace(t)?, t),
            Pick::Step(toward) => {
                let tabs = self.workspace(at.ws).map_or(&[][..], Workspace::tabs);
                let index = tabs.iter().position(|t| t.id() == at.tab);
                let next = round(tabs, index.unwrap_or(0), toward);
                (at.ws, next.ok_or(Error::OnlyOne(Kind::Tab))?.id())
            }
        };
        let at = Some(Spot {
            tab: target,
            pane: None,
        });
        Ok(Picked { ws, at })
    }

    /// The workspace `show-workspace` picks for client `view`.
    fn pick_ws(&self, view: &View, pick: &Pick<WsRef>) -> Result<Picked, Error> {
        let ws = match pick {
            Pick::Id(r) => self.resolve_ws(r)?,
            Pick::Step(toward) => {
                let workspaces = self.parts.workspaces();
                let index = workspaces.iter().position(|w| w.viewers.contains(&view.id));
                let next = round(workspaces, index.unwrap_or(0), *toward);
                next.ok_or(Error::OnlyOne(Kind::Workspace))?.id()
            }
        };
        Ok(Picked { ws, at: None })
    }

    // ------------------------------------------------------------------ ls

    /// A pane's state, as `ls json` has it.
    fn pane_json<'a>(&'a self, p: &'a Pane, now: Instant) -> Fields {
        let pid = |c: &Child| Json::Int(i64::from(c.leader.pid().as_raw()));
        let mut fields = vec![
            ("id", Json::str(p.id().to_string())),
            ("name", Json::str(p.name())),
            ("rows", Json::Int(i64::from(p.size().rows()))),
            ("cols", Json::Int(i64::from(p.size().cols()))),
            ("pid", p.process().child().map_or(Json::Null, pid)),
            ("version", Json::str(p.version(self.run).to_string())),
        ];
        fields.extend(crate::capture::state(p, now, self.copying(p.id())));
        fields
    }

    /// A tab and its panes, as `ls json` has them.
    fn tab_json<'a>(&'a self, t: &'a Tab, now: Instant) -> Fields {
        let panes = (t.root().panes().into_iter())
            .filter_map(|id| self.parts.panes().get(&id))
            .map(|p| Json::object(self.pane_json(p, now)))
            .collect();
        vec![
            ("id", Json::str(t.id().to_string())),
            ("name", Json::str(t.name())),
            ("panes", Json::Array(panes)),
        ]
    }

    /// A workspace, its tabs and their panes, as `ls json` has them.
    fn ws_json<'a>(&'a self, ws: &'a Workspace, now: Instant) -> Fields {
        let tabs = (ws.tabs().iter()).map(|t| Json::object(self.tab_json(t, now)));
        vec![
            ("id", Json::str(ws.id().to_string())),
            ("name", Json::str(ws.name())),
            ("tabs", Json::Array(tabs.collect())),
        ]
    }

    /// `info` and `get`: one pane, tab or workspace, as JSON, or one field
    /// of its JSON (`get`), a string's text as it is. `target` is there
    /// (`there`, by its caller).
    fn ls_one(&mut self, target: AnyRef, get: Option<&str>) -> Result<Body, Error> {
        let now = Instant::now();
        let mut fields = match (target, get) {
            // A pane's version alone, which the recipes read after every
            // change: not the rest of its state (its foreground program's
            // name, argv and cwd).
            (AnyRef::Pane(id), Some("version")) => {
                let version = |p: &Pane| p.version(self.run).to_string();
                (self.parts.panes().get(&id)).map(|p| vec![("version", Json::str(version(p)))])
            }
            (AnyRef::Pane(id), _) => self.parts.panes().get(&id).map(|p| self.pane_json(p, now)),
            (AnyRef::Tab(id), _) => self.tab(id).map(|t| self.tab_json(t, now)),
            (AnyRef::Workspace(id), _) => self.workspace(id).map(|w| self.ws_json(w, now)),
        }
        .unwrap_or_default();
        let Some(field) = get else {
            return Ok(Body::Listing {
                of: Some(target.kind()),
                listing: Json::object(fields),
            });
        };
        // A tab of one pane (`run`'s) stands for its pane, whose
        // fields it gives too.
        let single = match target {
            AnyRef::Tab(id) => self.root(id).map(Tree::panes).filter(|p| p.len() == 1),
            AnyRef::Pane(_) | AnyRef::Workspace(_) => None,
        };
        let pane = single.and_then(|p| p.first().and_then(|id| self.parts.panes().get(id)));
        if let Some(more) = pane.map(|p| self.pane_json(p, now)) {
            fields.extend(
                more.into_iter()
                    .filter(|(name, _)| !["id", "name"].contains(name)),
            );
        }
        let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
        match fields.into_iter().find(|(name, _)| *name == field) {
            // Exactly, or not at all: fuxth's integers are i64s.
            Some((_, Json::Uint(n))) if i64::try_from(n).is_err() => {
                let message = format!("{target}'s {field} is {n}, past fuxth's integers");
                Err(fail(Raise::Range, message))
            }
            Some((_, value)) => Ok(Body::Field(value)),
            None => {
                let (target, fields) = (target, names.join(", "));
                let message = format!("{target} has no field {field:?}; it has {fields}");
                Err(fail(Raise::NoField, message))
            }
        }
    }

    /// `ls json`: everything, and `events`, where a stream reads from.
    fn ls_json(&self, events: crate::events::Cursor) -> Json {
        let now = Instant::now();
        let workspaces = (self.parts.workspaces().iter())
            .map(|ws| Json::object(self.ws_json(ws, now)))
            .collect();
        let opt = |v: Option<String>| v.map_or(Json::Null, Json::str);
        let clients = self
            .views
            .values()
            .map(|v| {
                let place = self.place(v.id);
                let pane = place.map(|p| p.pane);
                Json::object(vec![
                    ("id", Json::str(v.id.to_string())),
                    ("rows", Json::Int(i64::from(v.rows))),
                    ("cols", Json::Int(i64::from(v.cols))),
                    ("workspace", opt(place.map(|p| p.ws.to_string()))),
                    ("tab", opt(place.map(|p| p.tab.to_string()))),
                    ("pane", opt(pane.map(|p| p.to_string()))),
                    ("zoom", Json::Bool(v.zoom)),
                ])
            })
            .collect();
        Json::object(vec![
            ("workspaces", Json::Array(workspaces)),
            ("clients", Json::Array(clients)),
            ("events", Json::str(events.to_string())),
        ])
    }
}

/// The answer of a lookup that found `found`: its id.
fn found(found: AnyRef) -> Reply {
    Body::Found(found).into()
}

/// `$HOME`: where a pane starts when nothing says where. Unset, or not an
/// absolute path (empty, or relative, so only the server's own directory
/// would place it), nothing starts; one no program can start in fails the
/// start with the system's error.
fn home() -> Result<PathBuf, Error> {
    let why = match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if home.is_absolute() => return Ok(home),
        Some(home) => format!("$HOME is {home:?}, not an absolute path"),
        None => "$HOME is not set, and nothing else says where to start".to_owned(),
    };
    Err(fail(Raise::Start, why))
}

/// The most read of what a program left on its terminal as it ends, in
/// MiB, as it is said: a whole number of them.
pub(crate) const LEFT_MIB: usize = 1;
/// The same in bytes.
pub(crate) const LEFT_MAX: usize = LEFT_MIB << 20;

/// What a program's terminal holds, unread, up to `LEFT_MAX`, and whether
/// it held more: read as it ends, so that its last screen is whole.
fn unread(process: &Process) -> (Vec<u8>, bool) {
    match process {
        Process::Reading(child) => left_on(&child.master),
        Process::HungUp(_) | Process::Absent | Process::Exited(_) | Process::Respawning => {
            (Vec::new(), false)
        }
    }
}

/// What `fd` holds to read now, up to `LEFT_MAX`, and whether it held more.
fn left_on(fd: impl std::os::fd::AsFd) -> (Vec<u8>, bool) {
    let mut left = Vec::new();
    let mut buffer = [0u8; 16384];
    let most = buffer.len();
    loop {
        // A byte past the most, read, says there was more.
        let room = LEFT_MAX.saturating_sub(left.len());
        let read = buffer.get_mut(..room.clamp(1, most)).unwrap_or_default();
        match fuxix::io::read(&fd, read) {
            Ok(n) if n > 0 && room == 0 => return (left, true),
            Ok(n) if n > 0 => left.extend_from_slice(read.get(..n).unwrap_or_default()),
            Err(fuxix::Errno::INTR) => {}
            Ok(_) | Err(_) => return (left, false),
        }
    }
}

/// What a new pane starts.
#[derive(Clone, Copy)]
enum Start<'a> {
    /// The shell, with variables set for it.
    Shell(&'a [(String, String)]),
    /// A program, run in place of a shell, with variables set for it.
    Program(&'a [String], &'a [(String, String)]),
}

/// The most bytes a name holds.
pub(crate) const NAME_MAX: usize = 256;

fn check_name(name: &str) -> Result<(), Error> {
    if name.trim().is_empty() {
        return Err(fail(Raise::BadName, "a name cannot be empty or blank"));
    }
    // Control characters, and the invisible ones that change how text
    // around them shows (zero-width, direction marks and overrides: UAX #9
    // 2.6 names U+061C beside U+200E and U+200F).
    let invisible = |c: char| matches!(c, '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
    if name.chars().any(|c| c.is_control() || invisible(c)) {
        let message = "a name cannot contain control or invisible characters";
        return Err(fail(Raise::BadName, message));
    }
    if name.len() > NAME_MAX {
        return Err(fail(
            Raise::BadName,
            format!("a name is at most {NAME_MAX} bytes"),
        ));
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
    // A name `workspace` reads back as itself, so that it finds this
    // workspace and no other: a pane's or tab's id stands for its own.
    if name.parse::<PaneId>().is_ok()
        || name.parse::<TabId>().is_ok()
        || command::parse_workspace(name) != Ok(WsRef::Name(name.to_owned()))
    {
        return Err(Error::NameReadAsTarget(name.to_owned()));
    }
    let others: Vec<&Workspace> = others.into_iter().collect();
    if others.iter().any(|ws| ws.name() == name) {
        return Err(Error::NameTaken(name.to_owned()));
    }
    // A tab's name too: the workspace's would hide it from `str NAME tab`.
    no_tab_named(others, name, None)
}

/// That no tab of `workspaces` but `except` was given the name `name`; else
/// why not, with those tabs and their panes.
fn no_tab_named<'a>(
    workspaces: impl IntoIterator<Item = &'a Workspace>,
    name: &str,
    except: Option<TabId>,
) -> Result<(), Error> {
    let tabs: Vec<&Tab> = (workspaces.into_iter())
        .flat_map(Workspace::tabs)
        .filter(|t| t.given() && t.name() == name && Some(t.id()) != except)
        .collect();
    if tabs.is_empty() {
        return Ok(());
    }
    // `run` with such a name: `str NAME tab` would not know which, so the
    // program is not started. The tabs, then their panes.
    let panes = (tabs.iter()).flat_map(|t| t.root().panes());
    let tabs = tabs.iter().map(|t| t.id()).collect();
    let panes: Vec<PaneId> = panes.collect();
    Err(Error::TabNameTaken(name.to_owned(), tabs, panes))
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

/// A session itself, as what a program's requests go to, for the tests
/// and the fuzz targets: each is begun at once, and a wait answered at its
/// limit, there being no programs to wait on, once what is due to be read
/// is; an event stream, which a real server alone serves, is not.
#[cfg(any(test, feature = "in-process"))]
pub struct InProcess<'s>(pub &'s mut Session);

#[cfg(any(test, feature = "in-process"))]
impl crate::script::Asks for InProcess<'_> {
    fn ask(
        &mut self,
        asked: &crate::protocol::Asked,
        out: &mut dyn std::io::Write,
    ) -> Result<Reply, crate::script::Failed> {
        let session = &mut *self.0;
        let origin = match asked.context.client {
            Some(client) if session.views.contains_key(&client) => Origin::Client(client),
            Some(_) | None => Origin::Cli(Caller::default()),
        };
        let outcome = match session.begin(asked.request.clone(), &origin) {
            Begun::Done(outcome) => outcome,
            // As the server answers it: the panes' turns and frames come
            // as they fall due, the soonest first, each answered at its
            // time, until it has an answer; at its deadline, once none
            // falls due before it.
            Begun::Waiting(waiting) => loop {
                let soonest = session.timers().min_by_key(|(at, _)| *at);
                let due = soonest.map(|(at, _)| at).into_iter().chain(waiting.due());
                let now = due.min().unwrap_or_else(Instant::now);
                if let Some(answer) = waiting.answer(session, now) {
                    break answer;
                }
                let Some((_, timer)) = soonest else {
                    let why = "a wait nothing due answers is answered by a real server only";
                    break Reply::failed(Code::NotHere, why);
                };
                session.fire(timer);
            },
            Begun::Events(_) => {
                let _ = std::io::Write::write_all(out, b"events stream from a real server only\n");
                Reply::default()
            }
        };
        Ok(outcome)
    }
}

/// What the other modules' tests share: a session started with the default
/// config, a client attached, and a command line run in it.
#[cfg(test)]
pub(crate) mod testing {
    use super::{InProcess, Reply, Session};
    use crate::command::{Command, Request};
    use crate::config::Config;
    use crate::id::ClientId;

    /// A session of `config`, but for its shell, `/bin/sh` (each pane is
    /// named after it), started at 80 by 24.
    pub(crate) fn started(config: Config) -> Result<Session, String> {
        let config = Config {
            shell: crate::config::Shell::new("/bin/sh".to_owned(), Vec::new())?,
            ..config
        };
        // One run for every session of the tests: two that did the same
        // list the same, their cursors too.
        static RUN: std::sync::OnceLock<Option<crate::command::Run>> = std::sync::OnceLock::new();
        let run = (*RUN.get_or_init(crate::command::Run::now)).ok_or("no clock")?;
        let mut session = Session::new(config, "/nonexistent/fux.sock".into(), false, run);
        let size = fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?;
        session.start(size).map_err(|e| e.to_string())?;
        session.test_keymap = crate::script::default_keys().map_err(|e| e.to_string())?;
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

    /// Runs fuxth `code` as client `client` would, or from no client: what
    /// it printed and what it left, a value a line; or its failure. A screen
    /// it opens opens on the client (`Session::input` reads its keys), and
    /// what the server sends clients meanwhile reaches them (`deliver`).
    pub(crate) fn output_as(
        session: &mut Session,
        client: Option<ClientId>,
        code: &str,
    ) -> Result<String, String> {
        output_job(session, client, &code.to_owned().into())
    }

    /// The same of a job: its code, given the line typed for it.
    fn output_job(
        session: &mut Session,
        client: Option<ClientId>,
        job: &crate::screens::Job,
    ) -> Result<String, String> {
        let (out, ended) = run_as(session, client, job);
        let ended = ended.map_err(|e| match client {
            Some(_) => e.notice(),
            None => e.to_string(),
        })?;
        let mut text = String::from_utf8_lossy(&out).into_owned();
        for value in &ended {
            text.push_str(&value.text());
            text.push('\n');
        }
        Ok(text)
    }

    fn run_as(
        session: &mut Session,
        client: Option<ClientId>,
        job: &crate::screens::Job,
    ) -> (
        Vec<u8>,
        Result<Vec<fuxth::Value<crate::script::Fux>>, crate::script::Failed>,
    ) {
        let mut out = Vec::new();
        let tokens = match job.tokens() {
            Ok(tokens) => tokens,
            Err(e) => return (out, Err(crate::script::Failed::from(e))),
        };
        let mut machine = match crate::script::compile(&tokens) {
            Ok(machine) => machine,
            Err(failed) => return (out, Err(failed)),
        };
        let mut keymap =
            std::mem::replace(&mut session.test_keymap, crate::keymap::Keymap::empty());
        let context = crate::protocol::Context {
            client,
            ..crate::protocol::Context::default()
        };
        let mut host = crate::script::Host::new(context.clone(), Some(&mut keymap), &mut out);
        let mut process = InProcess(session);
        let mut asks = crate::script::AsClient {
            asks: &mut process,
            context,
            opened: None,
        };
        let ended = crate::script::run_capped(&mut machine, &mut host, &mut asks, None, None);
        let opened = asks.opened;
        session.test_keymap = keymap;
        if let (Some(client), Some(screen)) = (client, opened) {
            session.open_screen(client, screen);
        }
        deliver(session);
        (out, ended)
    }

    /// What the server sent clients, taken as their clients take it: a
    /// program to run, keys typed, copy mode ended. The rest is left.
    pub(crate) fn deliver(session: &mut Session) {
        use crate::protocol::ToAttached;
        let outbox = std::mem::take(&mut session.outbox);
        let mut kept = Vec::new();
        for outgoing in outbox {
            match outgoing {
                super::Outgoing::To(client, ToAttached::Run(code)) => {
                    session.run_as(client, &code.into());
                }
                super::Outgoing::To(client, ToAttached::Ended(kind)) => {
                    if let Some(keys) = session.test_keys.get_mut(&client) {
                        keys.screen.ended(kind);
                    }
                }
                super::Outgoing::To(client, ToAttached::Keys(inputs)) => {
                    use crate::decode::Input;
                    let typed = inputs.into_iter().flat_map(|input| match input {
                        crate::command::Input::Key(press) => vec![Input::Key(press.into())],
                        crate::command::Input::Text(text) => text
                            .chars()
                            .map(|c| Input::Key(crate::keys::KeyPress::char(c).into()))
                            .collect(),
                        crate::command::Input::Paste(text) => vec![Input::Paste(text)],
                        crate::command::Input::Bytes(_) => Vec::new(),
                    });
                    session.inputs(client, typed.collect());
                }
                other @ (super::Outgoing::To(
                    _,
                    ToAttached::Paint(_) | ToAttached::Exit(_) | ToAttached::Welcome(..),
                )
                | super::Outgoing::Copy(..)
                | super::Outgoing::Shutdown { .. }) => {
                    kept.push(other);
                }
            }
        }
        kept.extend(std::mem::take(&mut session.outbox));
        session.outbox = kept;
    }

    /// What a reply's answer prints, a value a line, or why it failed.
    pub(crate) fn said(reply: &Result<Reply, super::Error>) -> String {
        let values = match reply {
            Ok(reply) => crate::script::answered(reply.body.clone()).map_err(|f| f.to_string()),
            Err(error) => Err(error.to_string()),
        };
        match values {
            Ok(values) => {
                let lines: Vec<String> = values.iter().map(fuxth::Value::text).collect();
                lines.join("\n")
            }
            Err(why) => why,
        }
    }

    /// Runs fuxth `code` from no client: what it printed and left, or its
    /// failure.
    pub(crate) fn output(session: &mut Session, code: &str) -> Result<String, String> {
        output_as(session, None, code)
    }

    /// Runs fuxth `code` from no client; its failure, if it fails.
    pub(crate) fn run(session: &mut Session, code: &str) -> Result<(), String> {
        output(session, code).map(drop)
    }

    /// What a program's requests would be, run as `client`: each asked of
    /// no one, but for the names it resolves, a client's actions taken as
    /// from the client asked.
    pub(crate) fn requests(
        s: &mut Session,
        client: ClientId,
        code: &str,
    ) -> Result<Vec<crate::command::Command>, String> {
        struct Recorder<'s>(InProcess<'s>, Vec<crate::command::Command>);
        impl crate::script::Asks for Recorder<'_> {
            fn ask(
                &mut self,
                asked: &crate::protocol::Asked,
                out: &mut dyn std::io::Write,
            ) -> Result<Reply, crate::script::Failed> {
                let recorded = match &asked.request {
                    Request::Do(Command::Client { action, .. }) => Command::Client {
                        client: None,
                        action: action.clone(),
                    },
                    Request::Do(Command::Resolve { .. })
                    | Request::Wait(..)
                    | Request::Events(..) => return self.0.ask(asked, out),
                    Request::Do(command) => command.clone(),
                };
                self.1.push(recorded);
                Ok(Reply::default())
            }
        }
        let tokens = fuxth::token::source("test", code).map_err(|e| e.to_string())?;
        let mut machine = crate::script::compile(&tokens).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let context = crate::protocol::Context {
            client: Some(client),
            ..crate::protocol::Context::default()
        };
        let mut host = crate::script::Host::new(context, None, &mut out);
        let mut recorder = Recorder(InProcess(s), Vec::new());
        crate::script::run_capped(&mut machine, &mut host, &mut recorder, None, None)
            .map_err(|e| e.to_string())?;
        Ok(recorder.1)
    }

    /// A client's keys, as its client reads them: its decoder, and the
    /// screen it has open or what its keys are in the middle of.
    #[derive(Default)]
    pub(crate) struct Keys {
        decoder: crate::decode::Decoder,
        pub(crate) screen: crate::screens::Screen,
    }

    impl Session {
        /// Bytes typed at client `client`'s terminal, as its client would
        /// take them: through its screen, else its keys (the session's
        /// `test_keymap`), each action sent, each program run as the client.
        pub(crate) fn input(&mut self, client: ClientId, bytes: &[u8]) {
            let mut inputs = Vec::new();
            let now = std::time::Instant::now();
            let keys = self.test_keys.entry(client).or_default();
            keys.decoder.expire(now);
            keys.decoder.bytes(bytes, &mut inputs);
            keys.decoder.mark(now);
            self.inputs(client, inputs);
        }

        /// The decoder's wait passed: what waited is taken as it is.
        pub(crate) fn escape(&mut self, client: ClientId) {
            let mut inputs = Vec::new();
            self.test_keys
                .entry(client)
                .or_default()
                .decoder
                .timeout(&mut inputs);
            self.inputs(client, inputs);
        }

        /// Opens a screen on client `client`, as its client does.
        pub(crate) fn open_screen(&mut self, client: ClientId, screen: crate::screens::Screen) {
            let keys = self.test_keys.entry(client).or_default();
            let mut acts = Vec::new();
            if let crate::screens::Screen::Copy(_) = keys.screen {
                acts.push(crate::act::Act::Copy(crate::act::CopyAct::Leave));
            }
            acts.extend(screen.opened(&self.test_keymap.modes));
            keys.screen = screen;
            self.acts(client, acts);
            deliver(self);
        }

        /// Runs fuxth as client `client`: a failure, or what it says, in its
        /// notice, as its client shows them.
        pub(crate) fn run_as(&mut self, client: ClientId, job: &crate::screens::Job) {
            match output_job(self, Some(client), job) {
                Err(error) => self.notify_to(client, error, true),
                Ok(said) if !said.trim().is_empty() => {
                    self.notify_to(client, said.trim().replace('\n', "  "), false);
                }
                Ok(_) => {}
            }
        }

        fn effects(&mut self, client: ClientId, effects: Vec<crate::screens::Effect>) {
            for effect in effects {
                match effect {
                    crate::screens::Effect::Send(act) => self.acts(client, vec![act]),
                    crate::screens::Effect::Run(job) => self.run_as(client, &job),
                }
                deliver(self);
            }
        }

        pub(crate) fn inputs(&mut self, client: ClientId, inputs: Vec<crate::decode::Input>) {
            for input in inputs {
                let Some(read_only) = self.views.get(&client).map(|v| v.read_only) else {
                    return;
                };
                let keys = self.test_keys.entry(client).or_default();
                let effects = keys.screen.input(&self.test_keymap, input, read_only);
                self.effects(client, effects);
            }
        }
    }
}

/// `inputs` as pane `p`'s program takes them: keys and pastes encoded as
/// it asked for them, text and bytes as they are.
fn bytes(p: &Pane, inputs: &[Input]) -> Vec<u8> {
    let screen = p.screen();
    let mut bytes = Vec::new();
    for input in inputs {
        match input {
            Input::Key(key) => {
                crate::encode::key_bytes((*key).into(), screen.key_mode(), &mut bytes)
            }
            Input::Text(text) => bytes.extend_from_slice(text.as_bytes()),
            Input::Paste(text) => {
                let bracketed = screen.mode(fux_vt::Mode::BracketedPaste);
                crate::encode::paste(text, bracketed, &mut bytes);
            }
            Input::Bytes(given) => bytes.extend_from_slice(given),
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    /// `rows` by `cols`.
    fn sized(rows: u16, cols: u16) -> Result<fux_vt::Size, String> {
        fux_vt::Size::new(rows, cols).map_err(|e| e.to_string())
    }

    use super::testing::{attached, output, output_as, requests, run, said, started};
    use super::*;
    use crate::pane::Left;

    /// A client that only watches, of `rows` by `cols`, attached.
    fn watch(s: &mut Session, rows: u16, cols: u16) -> Result<ClientId, String> {
        let attach = Attach {
            read_only: true,
            ..Attach::of(sized(rows, cols)?)
        };
        s.attach_as(ClientKind::Terminal, attach)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn an_empty_environment_unsets_the_configured_names() -> Result<(), String> {
        let mut s = started(Config::default())?;
        assert!(s.config.update_environment.is_empty());
        s.config.update_environment = vec!["KEPT".parse()?];
        let missing = s.starting_for(&Origin::Cli(Caller::default()));
        assert!(missing.set.is_empty() && missing.unset.is_empty());
        let empty = s.starting_for(&Origin::Cli(Caller {
            env: Some(Vec::new()),
            ..Caller::default()
        }));
        assert!(empty.set.is_empty());
        assert_eq!(empty.unset, ["KEPT"]);
        let client = s
            .attach_as(ClientKind::Terminal, Attach::of(sized(24, 80)?))
            .map_err(|e| e.to_string())?;
        assert_eq!(s.starting_for(&Origin::Client(client)).unset, ["KEPT"]);
        Ok(())
    }

    #[test]
    fn callerless_resize_uses_the_smallest_screen_in_either_attach_order() -> Result<(), String> {
        for sizes in [[(20, 100), (30, 60)], [(30, 60), (20, 100)]] {
            let mut s = started(Config::default())?;
            watch(&mut s, 3, 5)?;
            for (rows, cols) in sizes {
                s.attach(rows, cols, None).map_err(|e| e.to_string())?;
            }
            run(&mut s, "%1 horizontal split drop %1 20 resize-x")?;
            assert_eq!(
                s.parts.panes().get(&PaneId::of(1)).map(Pane::size),
                Some(sized(19, 20)?)
            );
        }
        Ok(())
    }

    /// A watcher's paste is told it only watches, as its client tells it:
    /// the test's input is the client's (`Screen::input`).
    #[test]
    fn a_watchers_paste_is_told_it_only_watches() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let c = watch(&mut s, 24, 80)?;
        s.input(c, b"\x1b[200~hi\x1b[201~");
        let said = s
            .views
            .get(&c)
            .and_then(|v| v.notice.clone())
            .map(|n| n.text);
        assert!(said.is_some_and(|n| n.starts_with("watching only")));
        Ok(())
    }

    /// A client that attaches only to watch sizes nothing from its first
    /// moment: its screen, too narrow to split, is not the tab's size.
    #[test]
    fn a_watcher_sizes_nothing_as_it_attaches() -> Result<(), String> {
        let mut s = started(Config::default())?;
        watch(&mut s, 10, 3)?;
        let pane = s.parts.panes().get(&PaneId::of(1)).map(Pane::size);
        assert_eq!(pane, Some(sized(24, 80)?));
        run(&mut s, "%1 split drop")
    }

    /// A tab only a watcher shows is laid out on its own size: a resize
    /// and a split are applied to its panes.
    #[test]
    fn a_tab_only_watched_is_laid_out_on_its_size() -> Result<(), String> {
        let mut s = started(Config::default())?;
        watch(&mut s, 30, 120)?;
        run(&mut s, "str @1 tab 100 10 resize")?;
        let size = |s: &Session, id| s.parts.panes().get(&PaneId::of(id)).map(Pane::size);
        assert_eq!(size(&s, 1), Some(sized(10, 100)?));
        run(&mut s, "%1 split drop")?;
        assert_eq!(size(&s, 1), Some(sized(10, 50)?));
        Ok(())
    }

    /// A paste into a pane whose program exited is refused, as keys are.
    #[test]
    fn a_paste_into_an_exited_pane_is_refused() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/cat program run")?;
        s.exited(PaneId::of(2), crate::process::Ended::Exited(3));
        s.buffers.push_front("x".into());
        let pasted = run(&mut s, "%2 0 paste-buffer");
        assert!(pasted.is_err_and(|e| e.contains("exited")), "refused");
        Ok(())
    }

    /// A pane whose program ended asks its client's terminal for no mouse
    /// or focus reports, which nothing would read: a click or a focus
    /// change is not refused, while a key still is.
    #[test]
    fn an_ended_pane_asks_for_no_reports_and_refuses_keys() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 80 23 size str /bin/cat program run")?;
        run(
            &mut s,
            &format!("str {c} client str +2 workspace show-workspace"),
        )?;
        let id = PaneId::of(2);
        // A test's session starts no program: one is put in the pane.
        let argv = ["/bin/cat".to_owned()];
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        (s.parts.pane_mut(id).ok_or("the pane")?).set_process(Process::Reading(child));
        s.output(id, b"\x1b[?1000h\x1b[?1004h");
        let mouse = |s: &Session| crate::render::compose(s, c).map(|g| g.modes.mouse);
        assert_eq!(mouse(&s), Some(1000), "asked while it runs");
        s.exited(id, Ended::Signalled(9));
        assert_eq!(mouse(&s), Some(0), "asked for none once it ended");
        let notice = |s: &mut Session, typed: &[u8]| {
            let view = s.views.get_mut(&c).map(|v| v.notice = None);
            s.input(c, typed);
            view.and(s.views.get(&c)).and_then(|v| v.notice.clone())
        };
        assert_eq!(notice(&mut s, b"\x1b[I").map(|n| n.text), None);
        let key = notice(&mut s, b"x").map(|n| n.text).unwrap_or_default();
        assert!(key.contains("was killed by SIGKILL"), "{key}");
        for dying in std::mem::take(&mut s.dying) {
            if let Some(pid) = dying.kill() {
                process::reaped(pid);
            }
        }
        Ok(())
    }

    /// A program respawned over (`kill-first`) has what it wrote read
    /// first, a frame it held too, and an exit waiting on that told before
    /// the respawn: none of it is lost with the old screen.
    #[test]
    fn a_respawn_reads_what_the_old_program_left() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/cat program run")?;
        let id = PaneId::of(2);
        s.output(id, b"\x1b[?2026h\x1b]2;old\x07\x07");
        let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
        pane.exiting(crate::process::Ended::Exited(3));
        let lines = told(&mut s, |s| {
            let _ = run(s, "%2 kill-first respawn");
        });
        let at = |what: &str| {
            lines
                .find(&format!(r#""{what}","pane":"%2""#))
                .ok_or(lines.clone())
        };
        assert!(at("title")? < at("exited")?, "{lines}");
        assert!(at("bell")? < at("exited")?, "{lines}");
        assert!(at("exited")? < at("respawned")?, "{lines}");
        Ok(())
    }

    /// What a program left is read up to `LEFT_MAX`, and more past it is
    /// said: a file of zeros holds more, one of nothing none.
    #[test]
    fn what_is_left_past_the_most_read_is_said() -> Result<(), String> {
        let zeros = std::fs::File::open("/dev/zero").map_err(|e| e.to_string())?;
        let (left, more) = left_on(&zeros);
        assert_eq!((left.len(), more), (LEFT_MAX, true));
        let null = std::fs::File::open("/dev/null").map_err(|e| e.to_string())?;
        assert_eq!(left_on(&null), (Vec::new(), false));
        Ok(())
    }

    /// A frame held as its pane is resized is read as every output is:
    /// its copy copied and its bell rung, not left for a later read.
    #[test]
    fn a_frame_a_resize_reads_is_read_whole() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        let id = PaneId::of(1);
        s.output(id, b"\x1b[?2026h\x07\x1b]52;c;aGk=\x07");
        assert!(s.buffers.is_empty(), "held");
        s.resize(c, 30, 100).map_err(|e| e.to_string())?;
        assert_eq!(s.buffers.front().map(String::as_str), Some("hi"));
        assert!(!s.pane(id).map_err(|e| e.to_string())?.bell, "rung");
        Ok(())
    }

    /// A tab with a size of its own is laid out on it again once no client
    /// sizes it, though the one that did still shows it: grown one row
    /// high, it sizes nothing.
    #[test]
    fn a_fixed_tab_no_client_sizes_is_laid_out_on_its_size() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 40 12 size str +1 workspace in new-tab drop")?;
        run(&mut s, &format!("str {c} client str @2 tab show-tab"))?;
        let size = |s: &Session| {
            s.pane(PaneId::of(2))
                .map(|p| (p.size().cols(), p.size().rows()))
        };
        assert_eq!(size(&s).ok(), Some((80, 23)), "sized by the client");
        s.resize(c, 1, 80).map_err(|e| e.to_string())?;
        assert_eq!(size(&s).ok(), Some((40, 12)), "on its own size");
        Ok(())
    }

    /// A screen smaller than its tab names in its bar the panes it cuts
    /// off and those it has no room for.
    #[test]
    fn a_small_screen_names_the_panes_it_cuts_and_hides() -> Result<(), String> {
        let (mut s, _) = attached(25, 80)?;
        run(&mut s, "str %1 pane vertical split drop")?;
        let bar = |s: &mut Session, rows, cols| {
            let grid = watch(s, rows, cols).map(|c| crate::render::compose(s, c));
            Ok::<_, String>(grid?.map(|g| g.row_text(g.rows.saturating_sub(1))))
        };
        let roomy = bar(&mut s, 30, 100)?.unwrap_or_default();
        assert!(!roomy.contains('['), "{roomy}");
        let small = bar(&mut s, 6, 80)?.unwrap_or_default();
        assert!(small.contains("[cut %1 %2] %1"), "{small}");
        let short = bar(&mut s, 3, 80)?.unwrap_or_default();
        assert!(short.contains("[hidden %2] [cut %1] %1"), "{short}");
        // Named before the tabs and the title on a narrow bar.
        let narrow = bar(&mut s, 3, 24)?.unwrap_or_default();
        assert!(narrow.contains("[hidden %2] [cut %1]"), "{narrow}");
        Ok(())
    }

    /// A tab made from the command line in a pane is laid out on the
    /// smallest screen of the clients sizing that pane's tab, not of
    /// whichever client attached first.
    #[test]
    fn a_new_tab_takes_the_screen_of_the_callers_tab() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _small) = attached(24, 80)?;
        let big = s.attach(50, 200, None)?;
        run(&mut s, "opts 100 30 size str other name new-workspace drop")?;
        run(
            &mut s,
            &format!("str {big} client str other workspace show-workspace"),
        )?;
        let caller = Caller {
            gave: Some(PaneId::of(2)),
            within: Within::This,
            ..Caller::default()
        };
        let new_tab = Command::NewTab {
            target: None,
            name: None,
            size: None,
            cwd: None,
            env: crate::command::Env::default(),
        };
        s.execute(&new_tab, &Origin::Cli(caller))?;
        let tab = s.tab("@3".parse()?).ok_or("the new tab")?;
        let size = tab.fit().size();
        assert_eq!((size.cols(), size.rows()), (200, 49));
        Ok(())
    }

    /// A wait on a pane part way through a frame is answered in process as
    /// a server answers it: once the frame is read whole, its screen
    /// changed, not as an empty success.
    #[test]
    fn a_wait_in_process_answers_a_frame_whole() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let mut frame = b"\x1b[?2026h".to_vec();
        frame.extend(crate::pane::tests::spends_the_budget(24, 80));
        frame.extend_from_slice(b"DONE\x1b[?2026l");
        s.output(PaneId::of(1), &frame);
        assert!(s.pane(PaneId::of(1)).is_ok_and(Pane::mid_frame));
        let answer = output(&mut s, "%1 0 timeout wait")?;
        assert!(answer.ends_with(" screen\n"), "{answer}");
        Ok(())
    }

    /// A wait on a pane holding a frame that its time releases before the
    /// wait's is answered in process as a server answers it: the frame
    /// shown, its screen changed, not timed out.
    #[test]
    fn a_wait_in_process_sees_a_frame_released_before_its_time() -> Result<(), String> {
        let mut s = started(Config::default())?;
        s.output(PaneId::of(1), b"\x1b[?2026hHELLO");
        let answer = output(&mut s, "%1 2 timeout wait")?;
        assert!(answer.ends_with(" screen\n"), "{answer}");
        Ok(())
    }

    /// A client splitting a pane of a tab it does not show stays as it is:
    /// its own tab keeps its zoom.
    #[test]
    fn a_split_elsewhere_leaves_the_splitters_zoom() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "%1 split drop")?;
        run(&mut s, &format!("str {c} client zoom"))?;
        run(&mut s, "opts 80 24 size str other name new-workspace drop")?;
        output_as(&mut s, Some(c), "%3 split drop")?;
        assert!(s.views.get(&c).is_some_and(|v| v.zoom), "still zoomed");
        Ok(())
    }

    /// A client detaching from a zoomed tab that fits clients resizes
    /// nothing, as showing another does not: its panes keep their size.
    #[test]
    fn a_zoomed_tab_its_client_detaches_from_keeps_its_sizes() -> Result<(), String> {
        let sizes =
            |s: &Session| [1, 2].map(|p| s.parts.panes().get(&PaneId::of(p)).map(Pane::size));
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "%1 split drop")?;
        run(&mut s, &format!("str {c} client zoom"))?;
        let zoomed = sizes(&s);
        s.detach(c);
        assert_eq!(sizes(&s), zoomed);
        Ok(())
    }

    /// A program that left more on its terminal than is read says so with
    /// its end, where no later notice hides it: in `exited`, its pane's
    /// JSON and the exit's notice; and one respawned over while it ran, in
    /// `respawned` and the respawn's notice.
    #[test]
    fn output_cut_at_an_end_is_told_with_the_end() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 80 23 size str /bin/cat program run")?;
        run(
            &mut s,
            &format!("str {c} client str +2 workspace show-workspace"),
        )?;
        let id = PaneId::of(2);
        let notice = |s: &Session| s.views.get(&c).and_then(|v| v.notice.clone());
        let cut = |s: &mut Session| (s.parts.pane_mut(id)).map(|mut p| p.left_cut());
        cut(&mut s);
        let lines = told(&mut s, |s| s.exited(id, Ended::Exited(3)));
        assert!(
            lines.contains(r#"{"event":"exited","pane":"%2""#)
                && lines.contains(r#""truncated":true"#),
            "{lines}"
        );
        let said = notice(&s).map(|n| n.text).unwrap_or_default();
        assert!(
            said.contains("%2 exited 3; it left more than 1 MiB"),
            "{said}"
        );
        let json = output(&mut s, "%2 capture json")?;
        assert!(json.contains(r#""truncated":["output"]"#), "{json}");
        // Respawned, its cut was the old program's, told at its exit.
        let lines = told(&mut s, |s| _ = run(s, "%2 respawn"));
        assert!(!lines.contains("truncated"), "{lines}");
        let json = output(&mut s, "%2 capture json")?;
        assert!(json.contains(r#""truncated":[]"#), "{json}");
        // So too with `kill-first`, which ends only a program that runs.
        cut(&mut s);
        s.exited(id, Ended::Exited(3));
        let lines = told(&mut s, |s| _ = run(s, "%2 kill-first respawn"));
        assert!(!lines.contains("truncated"), "{lines}");
        // One respawned over while it runs is told so at the respawn.
        let argv = ["/bin/cat".to_owned()];
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        (s.parts.pane_mut(id).ok_or("the pane")?).set_process(Process::Reading(child));
        cut(&mut s);
        let lines = told(&mut s, |s| _ = run(s, "%2 kill-first respawn"));
        assert!(
            lines.starts_with(r#"{"event":"respawned","pane":"%2""#)
                && lines.contains(r#""truncated":true"#),
            "{lines}"
        );
        assert!(!lines.contains("exited"), "{lines}");
        let said = notice(&s).map(|n| n.text).unwrap_or_default();
        assert!(
            said.contains("respawned; the program it ran left more"),
            "{said}"
        );
        for dying in std::mem::take(&mut s.dying) {
            if let Some(pid) = dying.kill() {
                process::reaped(pid);
            }
        }
        Ok(())
    }

    /// A `kill-first` respawn whose program does not start leaves the one
    /// there running and unread: what it left (a frame it holds here) is
    /// read, and a cut said, only once another started.
    #[test]
    fn a_respawn_refused_reads_nothing_of_the_old_program() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/cat program run")?;
        let id = PaneId::of(2);
        s.output(id, b"\x1b[?2026h\x1b]2;old\x07");
        // Programs are started, as a server starts them.
        s.launch = Some("/nonexistent/fux.sock".into());
        let mut refused = String::new();
        let lines = told(&mut s, |s| {
            let respawn = run(s, "%2 str /no/such/bin program kill-first respawn");
            refused = respawn.err().unwrap_or_default();
        });
        assert!(refused.contains("/no/such/bin"), "{refused}");
        assert!(lines.is_empty(), "{lines}");
        let pane = s.pane(id).map_err(|e| e.to_string())?;
        assert!(pane.frame_deadline().is_some() && pane.left == Left::default());
        s.launch = None;
        let lines = told(&mut s, |s| _ = run(s, "%2 kill-first respawn"));
        let at = |what: &str| {
            lines
                .find(&format!(r#""{what}","pane":"%2""#))
                .ok_or(lines.clone())
        };
        assert!(at("title")? < at("respawned")?, "{lines}");
        Ok(())
    }

    /// A respawn of `id`, ending what runs there first; `argv` instead of
    /// its program, if given.
    fn respawn_of(id: PaneId, argv: &[&str]) -> Request {
        Request::Do(Command::Respawn {
            target: id,
            kill: true,
            env: command::Env::default(),
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        })
    }

    /// The turns that finish what `waiting` waits for, in pane `id`, to
    /// its answer; how many there were.
    fn turns(s: &mut Session, id: PaneId, waiting: &Waiting) -> Result<(Reply, usize), String> {
        for turns in 0..10_000 {
            if let Some(answer) = waiting.answer(s, Instant::now()) {
                return Ok((answer, turns));
            }
            assert!(s.timers().any(|t| t.1 == Timer::Unread(id)), "no turn due");
            s.fire(Timer::Unread(id));
        }
        Err("never answered".into())
    }

    /// A respawn over output the parser has yet to read reads it in the
    /// turns any pane's output is read in, its answer waiting till then,
    /// another respawn refused: the old program's bell is told before
    /// `respawned`, and the answer is a version the pane has.
    #[test]
    fn a_respawn_reads_what_was_left_in_turns() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/cat program run")?;
        let (id, origin) = (PaneId::of(2), Origin::Cli(Caller::default()));
        let mut stream = crate::events::Subscription::new(None, s.parts.journal(), s.run, None)?;
        let spends = crate::pane::tests::spends_the_budget(24, 80);
        let mut left: Vec<u8> = std::iter::repeat_n(spends, 4).flatten().collect();
        left.push(0x07);
        s.output(id, &left);
        let Begun::Waiting(waiting) = s.begin(respawn_of(id, &[]), &origin) else {
            return Err("answered before what was left was read".into());
        };
        let Begun::Done(again) = s.begin(respawn_of(id, &[]), &origin) else {
            return Err("a second respawn under way".into());
        };
        assert_eq!(
            again.end.code(),
            Some(Code::StillRunning),
            "{:?}",
            again.end
        );
        let (answer, turns) = turns(&mut s, id, &waiting)?;
        assert!(turns > 1, "read in {turns} turns");
        assert_eq!(answer.end, End::Done);
        let pane = s.pane(id).map_err(|e| e.to_string())?;
        assert_eq!(
            answer.body,
            Body::Pane(PaneAnswer::Acted(pane.version(s.run)))
        );
        let lines = stream
            .told(s.parts.journal(), usize::MAX, |t| s.holder(t))
            .0;
        let at = |what: &str| {
            lines
                .find(&format!(r#""{what}","pane":"%2""#))
                .ok_or(lines.clone())
        };
        assert!(at("bell")? < at("respawned")?, "{lines}");
        Ok(())
    }

    /// A pane closed while respawning abandons the respawn: the program
    /// started for it is ended, its answer says the pane closed, and the
    /// clients that showed it are told.
    #[test]
    fn a_pane_closed_while_respawning_abandons_it() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 80 23 size str /bin/cat program run")?;
        run(
            &mut s,
            &format!("str {c} client str +2 workspace show-workspace"),
        )?;
        let id = PaneId::of(2);
        s.output(id, &crate::pane::tests::spends_the_budget(23, 80));
        // Programs are started, as a server starts them.
        s.launch = Some("/nonexistent/fux.sock".into());
        let begun = s.begin(respawn_of(id, &[]), &Origin::Cli(Caller::default()));
        s.launch = None;
        let Begun::Waiting(waiting) = begun else {
            return Err("answered before what was left was read".into());
        };
        assert_eq!(s.children().count(), 1, "the program started for it");
        run(&mut s, "%2 kill")?;
        let answer = waiting.answer(&s, Instant::now()).ok_or("no answer")?;
        assert_eq!(answer.end.code(), Some(Code::Closed), "{:?}", answer.end);
        let said = (s.views.get(&c))
            .and_then(|v| v.notice.clone())
            .map(|n| n.text);
        let said = said.unwrap_or_default();
        assert!(said.starts_with("%2 closed while respawning"), "{said}");
        assert_eq!(s.dying.len(), 1, "the program started for it, ended");
        for dying in std::mem::take(&mut s.dying) {
            if let Some(pid) = dying.kill() {
                process::reaped(pid);
            }
        }
        Ok(())
    }

    /// A program respawned over is taken from its pane as the respawn
    /// begins, whether it runs (`kill-first`) or its exit waits on what it
    /// left: while that is read the pane is no program's, so input is
    /// refused, none queued to be lost, and nothing is signalled.
    #[test]
    fn input_is_refused_while_respawning() -> Result<(), String> {
        for (kill, exiting) in [(true, None), (false, Some(Ended::Exited(0)))] {
            let mut s = started(Config::default())?;
            run(&mut s, "opts 80 24 size str /bin/cat program run")?;
            let id = PaneId::of(2);
            let argv = ["/bin/cat".to_owned()];
            let size = (sized(24, 80)?, None);
            let child = process::spawn(&argv, Path::new("/"), (&[], &[]), size)?;
            let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
            pane.set_process(Process::Reading(child));
            if let Some(ended) = exiting {
                pane.exiting(ended);
            }
            s.output(id, &crate::pane::tests::spends_the_budget(24, 80));
            let mut stream =
                crate::events::Subscription::new(None, s.parts.journal(), s.run, None)?;
            let respawn = Request::Do(Command::Respawn {
                target: id,
                kill,
                env: command::Env::default(),
                argv: Vec::new(),
            });
            let Begun::Waiting(waiting) = s.begin(respawn, &Origin::Cli(Caller::default())) else {
                return Err("answered before what was left was read".into());
            };
            let sent = run(&mut s, "%2 str abc text send-keys");
            assert!(sent.is_err_and(|e| e.contains("respawning")));
            let signalled = s.signalled(id).map(drop).map_err(|e| e.to_string());
            assert!(signalled.is_err_and(|e| e.contains("respawning")));
            turns(&mut s, id, &waiting)?;
            let lines = stream
                .told(s.parts.journal(), usize::MAX, |t| s.holder(t))
                .0;
            assert!(lines.contains(r#""respawned","pane":"%2""#), "{lines}");
            assert!(!lines.contains("input_unread"), "{lines}");
            for dying in std::mem::take(&mut s.dying) {
                if let Some(pid) = dying.kill() {
                    process::reaped(pid);
                }
            }
        }
        Ok(())
    }

    /// A frame a respawned-over program holds is read by a turn of the
    /// respawn's, the one timer of its pane: no other reads in that round.
    #[test]
    fn a_held_frame_is_a_turn_of_the_respawn() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/cat program run")?;
        let id = PaneId::of(2);
        s.output(id, b"\x1b[?2026hheld");
        let begun = s.begin(respawn_of(id, &[]), &Origin::Cli(Caller::default()));
        let Begun::Waiting(waiting) = begun else {
            return Err("answered with a frame held".into());
        };
        let timers: Vec<Timer> = s.timers().map(|(_, timer)| timer).collect();
        assert_eq!(timers, [Timer::Unread(id)]);
        let (_, turns) = turns(&mut s, id, &waiting)?;
        assert_eq!(turns, 1);
        Ok(())
    }

    /// A pane resized while respawning, in a tab no client shows, keeps
    /// that size once its new program takes over.
    #[test]
    fn a_resize_while_respawning_is_kept() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/cat program run")?;
        let id = PaneId::of(2);
        s.output(id, &crate::pane::tests::spends_the_budget(24, 80));
        let begun = s.begin(respawn_of(id, &[]), &Origin::Cli(Caller::default()));
        let Begun::Waiting(waiting) = begun else {
            return Err("answered before what was left was read".into());
        };
        run(&mut s, "%2 tab 100 30 resize")?;
        turns(&mut s, id, &waiting)?;
        let pane = s.pane(id).map_err(|e| e.to_string())?;
        assert_eq!((pane.respawns, pane.size()), (1, sized(30, 100)?));
        Ok(())
    }

    /// A program started for a respawn that exits before it takes over is
    /// told exited as it does: the server looked for exits as they were
    /// signalled, when it was no pane's.
    #[test]
    fn a_respawned_program_that_ends_first_is_told_exited() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/cat program run")?;
        let id = PaneId::of(2);
        s.output(id, &crate::pane::tests::spends_the_budget(24, 80));
        let mut stream = crate::events::Subscription::new(None, s.parts.journal(), s.run, None)?;
        // Programs are started, as a server starts them.
        s.launch = Some("/nonexistent/fux.sock".into());
        let begun = s.begin(
            respawn_of(id, &["/bin/sh", "-c", "exit 0"]),
            &Origin::Cli(Caller::default()),
        );
        s.launch = None;
        let Begun::Waiting(waiting) = begun else {
            return Err("answered before what was left was read".into());
        };
        let ended = |s: &Session| {
            let replacement = s.respawning.get(&id).and_then(|r| r.process.child());
            replacement.and_then(|c| c.leader.exited())
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while ended(&s).is_none() {
            assert!(Instant::now() < deadline, "the program never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
        // The server reaps no orphan it holds: none but the children.
        let started = s.respawning.get(&id).and_then(|r| r.process.child());
        let started = started.map(|c| c.leader.pid());
        assert!(s.children().any(|c| Some(c.leader.pid()) == started));
        turns(&mut s, id, &waiting)?;
        let lines = stream
            .told(s.parts.journal(), usize::MAX, |t| s.holder(t))
            .0;
        let at = |what: &str| {
            lines
                .find(&format!(r#""{what}","pane":"%2""#))
                .ok_or(lines.clone())
        };
        assert!(at("respawned")? < at("exited")?, "{lines}");
        assert!(
            lines.contains(r#""exited","pane":"%2","tab":"@2","workspace":"+2","exited":0"#),
            "{lines}"
        );
        for dying in std::mem::take(&mut s.dying) {
            if let Some(pid) = dying.kill() {
                process::reaped(pid);
            }
        }
        Ok(())
    }

    /// A program started in a tab a terminal draws starts in that
    /// terminal's cell, its terminal sized in pixels before it can ask
    /// (here a respawn's, before the session settles).
    #[test]
    fn a_program_starts_in_the_cell_of_the_terminal_drawing_it() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 80 23 size str /bin/cat program run")?;
        run(
            &mut s,
            &format!("str {c} client str +2 workspace show-workspace"),
        )?;
        s.input(c, b"\x1b[6;20;10t");
        let id = PaneId::of(2);
        s.launch = Some("/nonexistent/fux.sock".into());
        (s.respawn(id, false, &[], &[], &Starting::default())).map_err(|e| e.to_string())?;
        let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
        let size = pane.size();
        let child = pane.set_process(Process::Absent);
        let Process::Reading(child) = child else {
            return Err("no program started".into());
        };
        let told = fuxix::terminal::window_size(&child.master).and_then(|(_, p)| p);
        let told = told.map(|p| (p.width.get(), p.height.get()));
        drop(child.master);
        if let Err(pid) = child.leader.finish() {
            process::reaped(pid);
        }
        assert_eq!(told, Some((size.cols() * 10, size.rows() * 20)));
        Ok(())
    }

    /// A new tab or workspace its client follows starts its program in
    /// the cell that client's terminal said, as it will draw it; one no
    /// client follows (`run`'s), in none.
    #[test]
    fn a_tab_its_client_follows_starts_in_its_terminals_cell() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        s.input(c, b"\x1b[6;20;10t");
        let origin = Origin::Client(c);
        let here = s.here(&origin).map_err(|e| e.to_string())?;
        let mut cell = |follows: bool| {
            let opening = s.opening((None, None), (here, follows));
            opening
                .map(|(_, _, (_, cell))| cell)
                .map_err(|e| e.to_string())
        };
        assert_eq!(cell(true)?, fux_vt::CellSize::new(10, 20));
        assert_eq!(cell(false)?, None);
        Ok(())
    }

    /// A pane resized as the cell its terminal draws it in changes has its
    /// terminal sized once, for both: not first for the cell at its old
    /// size, which would signal its program (SIGWINCH) twice.
    #[test]
    fn a_resize_and_a_new_cell_size_a_panes_terminal_once() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        let id = PaneId::of(1);
        let size = s.pane(id).map_err(|e| e.to_string())?.size();
        let argv = ["/bin/cat".to_owned()];
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (size, None))?;
        (s.parts.pane_mut(id).ok_or("the pane")?).set_process(Process::Reading(child));
        let pixels = |pane: &Pane| {
            let (_, pixels) = fuxix::terminal::window_size(&pane.process().child()?.master)?;
            pixels.map(|p| (p.width.get(), p.height.get()))
        };
        let view = s.views.get_mut(&c).ok_or("the view")?;
        view.terminal.cell = fux_vt::CellSize::new(10, 20);
        let smaller = sized(10, 40)?;
        let mut between = None;
        s.read_with(id, |pane| {
            between = pixels(pane);
            pane.resize(smaller);
        });
        assert_eq!(between, None, "not sized before the resize");
        let pane = s.pane(id).map_err(|e| e.to_string())?;
        assert_eq!(pixels(pane), Some((400, 200)));
        let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
        if let Process::Reading(child) = pane.set_process(Process::Absent) {
            drop(child.master);
            if let Err(pid) = child.leader.finish() {
                process::reaped(pid);
            }
        }
        Ok(())
    }

    /// A program in a tab with no host is told it has no cell: its client
    /// shown another tab, or detached, its terminal has no size in pixels;
    /// shown it again, the cell its terminal said.
    #[test]
    fn a_tab_with_no_host_tells_its_programs_no_cell() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        let id = PaneId::of(1);
        let size = s.pane(id).map_err(|e| e.to_string())?.size();
        let argv = ["/bin/cat".to_owned()];
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (size, None))?;
        (s.parts.pane_mut(id).ok_or("the pane")?).set_process(Process::Reading(child));
        let winsize = |s: &Session| {
            let pane = s.parts.panes().get(&id)?;
            fuxix::terminal::window_size(&pane.process().child()?.master)
        };
        s.input(c, b"\x1b[6;20;10t");
        let told = winsize(&s);
        assert!(told.is_some_and(|(_, pixels)| pixels.is_some()), "{told:?}");
        run(&mut s, "opts 80 24 size str other name new-workspace drop")?;
        let show = |ws: &str| format!("str {c} client str {ws} workspace show-workspace");
        let unknown = |s: &Session| winsize(s).is_some_and(|(_, pixels)| pixels.is_none());
        run(&mut s, &show("other"))?;
        assert!(unknown(&s), "hidden");
        run(&mut s, &show("+1"))?;
        assert_eq!(winsize(&s), told, "shown again");
        s.detach(c);
        assert!(unknown(&s), "detached");
        let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
        if let Process::Reading(child) = pane.set_process(Process::Absent) {
            drop(child.master);
            if let Err(pid) = child.leader.finish() {
                process::reaped(pid);
            }
        }
        Ok(())
    }

    /// A program respawned in a tab with no host is told no cell, though
    /// the program before it was told one.
    #[test]
    fn a_respawn_with_no_host_is_told_no_cell() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 80 23 size str /bin/cat program run")?;
        run(
            &mut s,
            &format!("str {c} client str +2 workspace show-workspace"),
        )?;
        s.input(c, b"\x1b[6;20;10t");
        s.output(PaneId::of(2), b"x");
        s.detach(c);
        run(&mut s, "%2 respawn")?;
        let json = output(&mut s, "%2 capture json")?;
        assert!(json.contains(r#""cell":null"#), "{json}");
        Ok(())
    }

    /// Input queued for a program that ends without reading it is let go,
    /// and told with its end, whoever watches: at its exit, in `exited` and
    /// its pane's JSON, or at a respawn over it, in `respawned`.
    #[test]
    fn input_an_ended_program_never_read_is_told() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 80 23 size str /bin/cat program run")?;
        run(
            &mut s,
            &format!("str {c} client str +2 workspace show-workspace"),
        )?;
        let id = PaneId::of(2);
        let notice = |s: &Session| s.views.get(&c).and_then(|v| v.notice.clone());
        let queue = |s: &mut Session, bytes: &[u8]| {
            let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
            pane.push(bytes).map_err(|e| e.to_string())
        };
        queue(&mut s, b"abc")?;
        let lines = told(&mut s, |s| s.exited(id, Ended::Exited(0)));
        assert!(
            lines.contains(r#"{"event":"exited","pane":"%2","tab":"@2","workspace":"+2","exited":0,"input_unread":3}"#),
            "{lines}"
        );
        let said = notice(&s).map(|n| n.text).unwrap_or_default();
        assert!(
            said.ends_with("; 3 bytes of input queued for it were never written to it"),
            "{said}"
        );
        assert!(s.pane(id).is_ok_and(|p| p.input().is_empty()));
        let json = output(&mut s, "%2 capture json")?;
        assert!(json.contains(r#""input_unread":3"#), "{json}");
        // Respawned over while it runs, with input it has not read.
        let argv = ["/bin/cat".to_owned()];
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        run(&mut s, "%2 respawn")?;
        (s.parts.pane_mut(id).ok_or("the pane")?).set_process(Process::Reading(child));
        queue(&mut s, b"de")?;
        let lines = told(&mut s, |s| _ = run(s, "%2 kill-first respawn"));
        assert!(
            lines.contains(
                r#"{"event":"respawned","pane":"%2","tab":"@2","workspace":"+2","input_unread":2}"#
            ),
            "{lines}"
        );
        let said = notice(&s).map(|n| n.text).unwrap_or_default();
        assert!(said.starts_with("%2 respawned; 2 bytes of input"), "{said}");
        let json = output(&mut s, "%2 capture json")?;
        assert!(json.contains(r#""input_unread":0"#), "{json}");
        for dying in std::mem::take(&mut s.dying) {
            if let Some(pid) = dying.kill() {
                process::reaped(pid);
            }
        }
        Ok(())
    }

    /// A program that ended is queued no report it asked for (in-band
    /// resize, colour scheme): the one respawned there reads none.
    #[test]
    fn a_respawned_program_reads_no_report_the_ended_one_asked_for() -> Result<(), String> {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 80 23 size str /bin/cat program run")?;
        run(
            &mut s,
            &format!("str {c} client str +2 workspace show-workspace"),
        )?;
        let id = PaneId::of(2);
        let queued = |s: &Session| {
            s.pane(id)
                .map(|p| p.input().len())
                .map_err(|e| e.to_string())
        };
        s.output(id, b"\x1b[?2048h\x1b[?2031h");
        s.exited(id, Ended::Exited(0));
        (s.parts.pane_mut(id).ok_or("the pane")?).resize(sized(10, 40)?);
        s.input(c, b"\x1b[?997;1n");
        assert_eq!(queued(&s)?, 0);
        run(&mut s, "%2 respawn")?;
        assert_eq!(queued(&s)?, 0);
        Ok(())
    }

    /// A program whose exit was told, respawned, is told exited once: its
    /// exit is not told again before `respawned`.
    #[test]
    fn an_exit_told_is_not_told_again_at_a_respawn() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/cat program run")?;
        let lines = told(&mut s, |s| {
            s.exited(PaneId::of(2), crate::process::Ended::Exited(3));
            let _ = run(s, "%2 respawn");
        });
        let said: Vec<&str> = lines.lines().filter_map(|l| l.split('"').nth(3)).collect();
        assert_eq!(said, ["exited", "respawned"], "{lines}");
        Ok(())
    }

    /// A pane respawned twice within its program's grace has both programs
    /// before dying: the second end never drops the first one's kill.
    #[test]
    fn a_pane_respawned_twice_in_its_grace_kills_both() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/cat program run")?;
        // A test's session starts no program: each is put in the pane.
        let argv = ["/bin/cat".to_owned()];
        for _ in 0..2 {
            let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
            let mut pane = s.parts.pane_mut(PaneId::of(2)).ok_or("the pane")?;
            pane.set_process(Process::Reading(child));
            run(&mut s, "%2 kill-first respawn")?;
        }
        let dying = std::mem::take(&mut s.dying);
        let ended = dying.len();
        for dying in dying {
            if let Some(pid) = dying.kill() {
                process::reaped(pid);
            }
        }
        assert_eq!(ended, 2);
        Ok(())
    }

    /// `json` of an answer of several panes is one array, as it is in a
    /// list: one shape for one answer.
    #[test]
    fn json_of_several_panes_is_one_array() -> Result<(), Box<dyn std::error::Error>> {
        let mut s = started(Config::default())?;
        run(&mut s, "%1 split")?;
        let alone = output(&mut s, "@1 str x text send-keys json")?;
        assert!(alone.starts_with(r#"[{"pane":"%1","#), "{alone}");
        assert_eq!(alone.lines().count(), 1, "{alone}");
        let listed = output(&mut s, "list @1 str x text send-keys end json")?;
        assert_eq!(listed, format!("[{}]\n", alone.trim_end()));
        Ok(())
    }

    /// A program that exits with output past a read budget still unread
    /// (`Pane::has_unread`) is at work until it is read, and exited only
    /// then, its last screen whole: a wait is never told it exited on a
    /// screen of before. Each REP is spent as 65,535 copies: 2,000 of them
    /// spend a budget.
    #[test]
    fn a_program_exits_once_its_output_is_read() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/true program run")?;
        let id = PaneId::of(2);
        // /bin/sh, which macOS has where it has no /bin/true.
        let argv = ["/bin/sh", "-c", "exit 0"].map(str::to_owned);
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        (s.parts.pane_mut(id).ok_or("the pane")?).set_process(Process::Reading(child));
        let exited = |s: &Session| {
            (s.parts.panes().get(&id))
                .and_then(|p| p.process().child()?.leader.exited())
                .is_some()
        };
        let until = crate::after(Instant::now(), Duration::from_secs(10));
        while !exited(&s) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut flood: Vec<u8> = std::iter::repeat_n(b"X\x1b[65535b", 2000)
            .flatten()
            .copied()
            .collect();
        flood.extend_from_slice(b"\x1b[2J\x1b[HDONE");
        s.output(id, &flood);
        s.exited(id, Ended::Exited(0));
        let pane = s.parts.panes().get(&id).ok_or("the pane")?;
        assert!(pane.has_unread(), "the budget stopped no call");
        assert!(
            pane.process().exit_status().is_none(),
            "exited with output unread"
        );
        let mut turns = 0u32;
        while s.parts.panes().get(&id).is_some_and(Pane::has_unread) && turns < 100 {
            s.fire(crate::session::Timer::Unread(id));
            turns = turns.saturating_add(1);
        }
        let pane = s.parts.panes().get(&id).ok_or("the pane")?;
        assert_eq!(pane.process().exit_status(), Some(Ended::Exited(0)));
        let text = pane
            .screen()
            .rows()
            .map(|r| r.cells_text())
            .collect::<String>();
        assert!(text.contains("DONE"), "{text:?}");
        Ok(())
    }

    /// A program that exits while what it left behind writes on (a
    /// background loop) is told exited once what was on its terminal as it
    /// exited is read: its terminal is drained once, not again at each
    /// later reap, so its exit is not put off for ever.
    #[test]
    fn a_program_whose_terminal_is_written_on_still_exits() -> Result<(), String> {
        // A large screen, where each ED is spent as 40,000 cells.
        let (mut s, _) = super::testing::attached(201, 200)?;
        run(&mut s, "opts 200 200 size str /bin/true program run")?;
        let id = PaneId::of(2);
        let argv = ["/bin/sh", "-c", "while :; do printf '\\033[2J'; done"].map(str::to_owned);
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        (s.parts.pane_mut(id).ok_or("the pane")?).set_process(Process::Reading(child));
        let rows = s.parts.panes().get(&id).map(|p| p.screen().size().rows());
        assert!(rows.is_some_and(|r| r >= 100), "{rows:?} rows");
        std::thread::sleep(Duration::from_millis(200));
        let exited = |s: &Session| {
            s.parts
                .panes()
                .get(&id)
                .is_some_and(|p| p.process().exit_status().is_some())
        };
        let mut turns = 0u32;
        s.exited(id, Ended::Exited(0));
        while !exited(&s) && turns < 100 {
            s.fire(crate::session::Timer::Unread(id));
            // As each reap of another child does.
            s.exited(id, Ended::Exited(0));
            turns = turns.saturating_add(1);
        }
        let told = exited(&s);
        for dying in std::mem::take(&mut s.dying) {
            if let Some(pid) = dying.kill() {
                process::reaped(pid);
            }
        }
        if let Some(child) =
            s.parts
                .pane_mut(id)
                .and_then(|mut p| match p.set_process(Process::Absent) {
                    Process::Reading(child) | Process::HungUp(child) => Some(child),
                    Process::Absent | Process::Exited(_) | Process::Respawning => None,
                })
        {
            drop(child.master);
            let _ = child.leader.finish();
        }
        assert!(told, "not told exited in {turns} turns");
        Ok(())
    }

    /// A frame of synchronized output held as its program exits is read
    /// before the exit is told: its last screen is whole.
    #[test]
    fn a_frame_held_at_exit_is_on_the_last_screen() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/true program run")?;
        let id = PaneId::of(2);
        s.output(id, b"\x1b[?2026hHELD");
        s.exited(id, Ended::Exited(0));
        let pane = s.parts.panes().get(&id).ok_or("the pane")?;
        assert_eq!(pane.process().exit_status(), Some(Ended::Exited(0)));
        let text = pane
            .screen()
            .rows()
            .map(|r| r.cells_text())
            .collect::<String>();
        assert!(text.contains("HELD"), "{text:?}");
        Ok(())
    }

    /// A frame begun in output read after the exit, past a read budget, is
    /// read too before the exit is told.
    #[test]
    fn a_frame_begun_after_the_exit_is_on_the_last_screen() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 80 24 size str /bin/true program run")?;
        let id = PaneId::of(2);
        let mut out: Vec<u8> = std::iter::repeat_n(b"X\x1b[65535b", 2000)
            .flatten()
            .copied()
            .collect();
        out.extend_from_slice(b"\x1b[2J\x1b[H\x1b[?2026hHELD");
        s.output(id, &out);
        s.exited(id, Ended::Exited(0));
        let mut turns = 0u32;
        while s
            .parts
            .panes()
            .get(&id)
            .is_some_and(|p| p.process().exit_status().is_none())
            && turns < 100
        {
            s.fire(crate::session::Timer::Unread(id));
            turns = turns.saturating_add(1);
        }
        let pane = s.parts.panes().get(&id).ok_or("the pane")?;
        assert_eq!(pane.process().exit_status(), Some(Ended::Exited(0)));
        let text = pane
            .screen()
            .rows()
            .map(|r| r.cells_text())
            .collect::<String>();
        assert!(
            text.contains("HELD"),
            "no HELD in {} characters",
            text.len()
        );
        Ok(())
    }

    /// Keys sent to a tab's panes go to every one of them or to none: a
    /// pane that has no room for them refuses them before any is sent.
    #[test]
    fn keys_for_several_panes_go_to_all_or_none() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "%1 split drop")?;
        let mut second = s.parts.pane_mut(PaneId::of(2)).ok_or("no %2")?;
        // Its program reads nothing, and its queue is full.
        let full = vec![0; crate::pane::INPUT_BYTES];
        second.push(full).map_err(|e| e.to_string())?;
        let sent = run(&mut s, "@1 str x text send-keys");
        assert!(sent.is_err_and(|e| e.contains("input_full")), "refused");
        let first = s.parts.panes().get(&PaneId::of(1)).ok_or("no %1")?;
        assert_eq!(first.input().len(), 0, "the first pane got none");
        // Keys too many for the room left are refused, and so is a key
        // after them, until the program reads: none lands past a hole.
        let mut pane = s.parts.pane_mut(PaneId::of(1)).ok_or("no %1")?;
        let room = pane.input().room();
        (pane.push(vec![0; room.saturating_sub(4)])).map_err(|e| e.to_string())?;
        assert!(run(&mut s, "%1 str 12345678 text send-keys").is_err());
        assert!(
            run(&mut s, "%1 str x text send-keys").is_err(),
            "a key after a refusal"
        );
        Ok(())
    }

    /// Keys sent at a version seen go only while their pane is still at
    /// it: a capture holds its pane's version as it was read; once the
    /// pane read more, none is sent, refused `stale`.
    #[test]
    fn keys_at_a_version_go_only_while_the_pane_is_at_it() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let queued =
            |s: &mut Session| (s.parts.pane_mut(PaneId::of(1))).map(|mut p| p.drain_input());
        run(&mut s, "%1 capture expect str x text send-keys")?;
        assert_eq!(queued(&mut s), Some(b"x".to_vec()));
        let seen = s
            .pane(PaneId::of(1))
            .map_err(|e| e.to_string())?
            .version(s.run);
        s.output(PaneId::of(1), b"E");
        let stale = output(&mut s, &format!("str {seen} expect str x text send-keys"));
        let stale = stale.err().unwrap_or_default();
        assert!(
            stale.starts_with("stale: ") && stale.contains("%1 changed since"),
            "{stale}"
        );
        assert_eq!(queued(&mut s), Some(Vec::new()), "nothing sent");
        // A program's end is a change too: stale, not `not_running`.
        run(&mut s, "opts 80 24 size str /bin/cat program run")?;
        let seen = s
            .pane(PaneId::of(2))
            .map_err(|e| e.to_string())?
            .version(s.run);
        s.exited(PaneId::of(2), crate::process::Ended::Exited(0));
        let ended = output(&mut s, &format!("str {seen} expect str x text send-keys"));
        let ended = ended.err().unwrap_or_default();
        assert!(ended.starts_with("stale: "), "{ended}");
        Ok(())
    }

    /// `set history-bytes` changes every pane's history, not only the next
    /// pane's.
    #[test]
    fn set_history_bytes_applies_to_panes_there_are() -> Result<(), String> {
        let (mut s, _) = attached(5, 20)?;
        s.output(
            PaneId::of(1),
            std::iter::repeat_n("line\r\n", 50)
                .collect::<String>()
                .as_bytes(),
        );
        let history = |s: &Session| {
            s.parts
                .panes()
                .get(&PaneId::of(1))
                .map(|p| p.screen().history_len())
        };
        assert!(history(&s) > Some(2));
        // Two rows of four cells: 24 bytes a row and 8 a cell.
        run(&mut s, "str history-bytes 112 set")?;
        assert_eq!(history(&s), Some(2));
        Ok(())
    }

    /// A program's OSC 52 write is a copy: to the paste buffers and the
    /// clipboard of the clients showing it, unless `clipboard` is false;
    /// one it cannot be is told them.
    #[test]
    fn a_programs_clipboard_write_is_a_copy() -> Result<(), String> {
        let (mut s, client) = attached(5, 20)?;
        s.outbox.clear();
        s.output(PaneId::of(1), b"\x1b]52;c;Zm9v\x07");
        assert_eq!(s.buffers.front().map(String::as_str), Some("foo"));
        let sent = |s: &Session| {
            s.outbox
                .iter()
                .any(|o| matches!(o, Outgoing::Copy(c, b) if *c == client && **b == *b"\x1b]52;c;Zm9v\x07"))
        };
        assert!(sent(&s));
        // One longer than fux-vt keeps of a string copies nothing, said.
        let long = format!(
            "\x1b]52;c;{}\x07",
            std::iter::repeat_n('A', fux_vt::OSC_PAYLOAD_LIMIT).collect::<String>()
        );
        s.output(PaneId::of(1), long.as_bytes());
        let notice = s.views.get(&client).and_then(|v| v.notice.clone());
        assert!(notice.is_some_and(|n| n.error && n.text.contains("copies (OSC 52) not made (1)")));
        assert_eq!(s.buffers.front().map(String::as_str), Some("foo"));
        // In one read: each copy made, in order, and the refusal between them told.
        if let Some(view) = s.views.get_mut(&client) {
            view.notice = None;
        }
        s.output(
            PaneId::of(1),
            b"\x1b]52;c;YQ==\x07\x1b]52;c;***\x07\x1b]52;c;Yg==\x07",
        );
        let buffers: Vec<&str> = s.buffers.iter().take(3).map(String::as_str).collect();
        assert_eq!(buffers, ["b", "a", "foo"]);
        let notice = s.views.get(&client).and_then(|v| v.notice.clone());
        assert!(notice.is_some_and(|n| n.text.contains("is not base64")));
        // One in a tab no client shows is told once one does.
        run(&mut s, "opts 20 4 size str other name new-workspace")?;
        run(&mut s, "str c1 client str other workspace show-workspace")?;
        if let Some(view) = s.views.get_mut(&client) {
            view.notice = None;
        }
        s.output(PaneId::of(1), b"\x1b]52;c;***\x07\x1b]52;c;***\x07");
        assert!(s.views.get(&client).is_some_and(|v| v.notice.is_none()));
        run(&mut s, "str c1 client str main workspace show-workspace")?;
        let notice = s.views.get(&client).and_then(|v| v.notice.clone());
        assert!(notice.is_some_and(|n| n.text.contains("%1: copies (OSC 52) not made (2)")));
        // Counted in its JSON too, for a reader no notice reaches.
        let json = output(&mut s, "%1 capture json")?;
        assert!(
            json.contains(r#""copies_cut":1,"copies_not_text":3,"#),
            "{json}"
        );
        s.outbox.clear();
        run(&mut s, "str clipboard false set")?;
        s.output(PaneId::of(1), b"\x1b]52;c;YmFy\x07");
        assert!(!sent(&s) && s.outbox.is_empty());
        assert_eq!(s.buffers.front().map(String::as_str), Some("b"));
        Ok(())
    }

    /// Replies dropped past what one read queues are counted in the
    /// pane's JSON, with no client to tell: those of one read that the
    /// full queue refuses too, and a resize report it refuses.
    #[test]
    fn replies_dropped_are_counted_in_a_panes_json() -> Result<(), String> {
        let (mut s, c) = attached(5, 20)?;
        s.detach(c);
        let id = PaneId::of(1);
        // `ESC [ 1 ; 1 R` each: as many as fit a read's replies, and one more.
        let fit = crate::pane::REPLIES_MAX / b"\x1b[1;1R".len();
        let queries: Vec<u8> = std::iter::repeat_n(&b"\x1b[6n"[..], fit + 1)
            .flatten()
            .copied()
            .collect();
        s.output(id, &queries);
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.contains(r#""replies":1,"#), "{json}");
        let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
        let room = pane.input().room();
        pane.push(vec![b'x'; room]).map_err(|e| e.to_string())?;
        s.output(id, b"\x1b[6n\x1b[6n");
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.contains(r#""replies":3,"#), "{json}");
        // In-band resize reports the full queue refuses are counted: the
        // one setting mode 2048 answers, and the one a resize sends.
        s.output(id, b"\x1b[?2048h");
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.contains(r#""replies":4,"#), "{json}");
        run(&mut s, "@1 30 6 resize")?;
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.contains(r#""replies":5,"#), "{json}");
        Ok(())
    }

    /// A tab no client shows is laid out on a client's screen, so its
    /// programs see the size they will be shown at, not the size they
    /// started at.
    #[test]
    fn a_tab_no_client_shows_is_sized_as_on_a_clients_screen() -> Result<(), String> {
        let (mut s, c) = attached(11, 40)?;
        output_as(&mut s, Some(c), "opts str +1 workspace in new-tab drop")?;
        run(&mut s, "str c1 client str @1 tab show-tab")?;
        let size = |s: &Session, p| s.parts.panes().get(&PaneId::of(p)).map(Pane::size);
        assert_eq!(size(&s, 2), Some(sized(10, 40)?));
        run(&mut s, "str %2 pane vertical split")?;
        let (top, bottom) = (size(&s, 2), size(&s, 3));
        let rows = top
            .zip(bottom)
            .map(|(t, b)| (t.rows(), b.rows(), t.cols(), b.cols()));
        assert_eq!(rows, Some((5, 4, 40, 40)), "stacked, a separator between");
        // Leaving a tab whose layout is unchanged resizes nothing: a zoomed
        // pane keeps the whole screen.
        run(&mut s, "str c1 client str @2 tab show-tab")?;
        run(&mut s, "str c1 client zoom")?;
        run(&mut s, "str c1 client str @1 tab show-tab")?;
        assert_eq!(size(&s, 2), Some(sized(10, 40)?));
        // Nor does a swap or a move in another tab.
        run(
            &mut s,
            "%1 split drop %1 %4 swap-panes %4 str new-tab move-to",
        )?;
        assert_eq!(size(&s, 2), Some(sized(10, 40)?));
        Ok(())
    }

    /// A pane is made only at a size known: with no size given, from a
    /// caller with no screen or tab, there is none, whatever client is
    /// attached, and the request is refused; the first
    /// client to attach to an empty session starts its shell on its screen,
    /// unless it only watches;
    /// a tab that fits clients keeps the last screen's size once none does.
    #[test]
    fn a_pane_is_made_only_at_a_size_known() -> Result<(), String> {
        let run_id = crate::command::Run::now().ok_or("no clock")?;
        let mut s = Session::new(
            Config::default(),
            "/nonexistent/fux.sock".into(),
            false,
            run_id,
        );
        let refused = run(&mut s, "opts new-workspace").err().unwrap_or_default();
        assert!(
            refused.starts_with("size: ") && refused.contains("give a size"),
            "{refused}"
        );
        assert!(s.parts.panes().is_empty());
        // A client of a size no view has starts nothing, nor does one that
        // only watches: there is nothing to watch.
        assert!(s.attach(view::MOST.saturating_add(1), 40, None).is_err());
        assert!(watch(&mut s, 11, 40).is_err());
        assert!(s.parts.panes().is_empty());
        let size = |s: &Session, p| s.parts.panes().get(&PaneId::of(p)).map(|p| p.size().into());
        let c = s.attach(11, 40, None).map_err(|e| e.to_string())?;
        assert_eq!(size(&s, 1), Some((10, 40)));
        // Nor from a caller with no screen or tab of its own, whatever
        // client is attached.
        let refused = run(&mut s, "opts str +1 workspace in new-tab").err();
        assert!(refused.is_some_and(|r| r.starts_with("size: ")));
        run(&mut s, "str %1 pane vertical split")?;
        s.detach(c);
        run(&mut s, "%2 kill")?;
        assert_eq!(
            size(&s, 1),
            Some((10, 40)),
            "laid out again on the last screen"
        );
        let refused = run(&mut s, "opts str +1 workspace in new-tab")
            .err()
            .unwrap_or_default();
        assert!(refused.starts_with("size: "), "{refused}");
        run(&mut s, "opts str +1 workspace in 30 8 size new-tab")?;
        assert_eq!(size(&s, 3), Some((8, 30)));
        Ok(())
    }

    /// `ls` of a workspace lists it whole; of a tab, that tab; of a pane,
    /// that pane.
    #[test]
    fn ls_of_a_target_lists_what_is_in_it() -> Result<(), String> {
        let (mut s, _) = attached(11, 40)?;
        run(&mut s, "opts 40 10 size str +1 workspace in new-tab")?;
        run(&mut s, "str %2 pane vertical split")?;
        let mut ids = |only| {
            let ls = Command::Ls {
                target: Some(only),
                get: None,
            };
            let text = said(&s.execute(&ls, &Origin::Cli(Caller::default())));
            let words = text.lines().filter_map(|l| l.split_whitespace().next());
            words.map(str::to_owned).collect::<Vec<_>>().join(" ")
        };
        let ws = "+1".parse::<WsId>().map_err(|e| e.to_string())?;
        let tab = "@2".parse::<TabId>().map_err(|e| e.to_string())?;
        assert_eq!(ids(AnyRef::Workspace(ws)), "+1 @1 %1 @2 %2 %3");
        assert_eq!(ids(AnyRef::Tab(tab)), "@2 %2 %3");
        assert_eq!(ids(AnyRef::Pane(PaneId::of(3))), "%3");
        Ok(())
    }

    /// docs/json.md names every field of a pane's `json`.
    #[test]
    fn the_json_help_names_every_pane_field() -> Result<(), Box<dyn std::error::Error>> {
        let (s, _) = attached(4, 30)?;
        let pane = s.parts.panes().values().next().ok_or("no pane")?;
        for (name, _) in s.pane_json(pane, Instant::now()) {
            let named = crate::help::JSON.contains(&format!("`{name}`"));
            assert!(named, "docs/json.md does not name a pane's {name:?}");
        }
        Ok(())
    }

    /// Names and titles are escaped in JSON, and a capture's lines read as
    /// the screen does.
    #[test]
    fn json_escapes_what_it_says() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(4, 30)?;
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, r#"str %1 pane str 'say "hi" \ 界' rename"#)?;
        s.output(PaneId::of(2), "a\tb 界\x1b]2;tab\\title\x07".as_bytes());
        let ls = output(&mut s, "ls json")?;
        assert!(ls.contains(r#""name":"say \"hi\" \\ 界""#), "{ls}");
        assert!(ls.contains(r#""title":"tab\\title""#), "{ls}");
        assert!(ls.contains(r#""truncated":[]"#), "{ls}");
        let long = format!(
            "\x1b]2;{}\x07",
            std::iter::repeat_n('t', crate::pane::TITLE_MAX + 1).collect::<String>()
        );
        s.output(PaneId::of(2), long.as_bytes());
        let capture = output(&mut s, "str %2 pane capture json")?;
        assert!(capture.contains(r#""truncated":["title"]"#), "{capture}");
        assert!(capture.contains(r#""lines":["a       b 界"]"#), "{capture}");
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
        run(&mut s, "str %1 pane horizontal split")?;
        s.exited(PaneId::of(2), crate::process::Ended::Exited(3));
        assert_eq!(notice(&s).as_deref(), Some("%2 sh exited with status 3"));
        // A second tab, shown; its only pane exits, and the tab with it.
        run(&mut s, "opts 40 9 size str +1 workspace in new-tab")?;
        run(&mut s, "str c1 client str @2 tab show-tab")?;
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
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, "opts 40 9 size str +1 workspace in new-tab")?;
        run(&mut s, "str %3 pane str @1 tab move-to")?;
        run(&mut s, "str %1 pane str right move-beside")?;
        let holding = |s: &Session, pane| {
            let tabs = s.parts.workspaces().iter().flat_map(|w| w.tabs());
            tabs.filter(|t| t.holds(pane)).count()
        };
        assert_eq!(holding(&s, PaneId::of(1)), 1);
        run(&mut s, "str %1 pane kill")?;
        assert_eq!(holding(&s, PaneId::of(1)), 0);
        Ok(())
    }

    /// A wait begun answers what comes after it, in order: output in one
    /// of a workspace's panes, that pane alone; its pane's close, `closed`
    /// (4). Begun first here, it cannot miss what it waits for.
    #[test]
    fn a_wait_begun_answers_the_change_and_the_close_after_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut s = started(Config::default())?;
        run(&mut s, "str %1 pane horizontal split")?;
        let origin = Origin::Cli(Caller::default());
        let wait = |target| {
            Request::Wait(command::Wait {
                since: command::Since::Now(target),
                timeout: None,
            })
        };
        let ws = s
            .parts
            .workspaces()
            .first()
            .map(|w| w.id())
            .ok_or("a workspace")?;
        let Begun::Waiting(waiting) = s.begin(wait(AnyRef::Workspace(ws)), &origin) else {
            return Err("the workspace's wait answered at once".into());
        };
        assert!(waiting.answer(&s, Instant::now()).is_none());
        s.output(PaneId::of(2), b"E");
        let answer = waiting.answer(&s, Instant::now()).ok_or("no answer")?;
        let text = said(&Ok(answer));
        assert!(text.starts_with("%2:") && !text.contains("%1"), "{text}");
        let Begun::Waiting(waiting) = s.begin(wait(AnyRef::Pane(PaneId::of(1))), &origin) else {
            return Err("the pane's wait answered at once".into());
        };
        run(&mut s, "str %1 pane kill")?;
        let answer = waiting.answer(&s, Instant::now()).ok_or("no answer")?;
        assert_eq!(answer.end.code(), Some(Code::Closed), "{answer:?}");
        Ok(())
    }

    /// A wait that failed answered too: a `try` that catches it gets that
    /// answer, with its version and status, in place of the message; a
    /// wait on it waits again from the version it is still at.
    #[test]
    fn a_failed_wait_keeps_its_answer() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let caught = "str timeout try %1 1 timeout wait catch drop end";
        let json = output(&mut s, &format!("{caught} json"))?;
        assert!(
            json.starts_with(r#"{"pane":"%1","version":"%1:"#) && json.contains(r#""status":124"#),
            "{json}"
        );
        let again = output(&mut s, &format!("{caught} 1 timeout wait")).err();
        assert!(
            again.as_ref().is_some_and(
                |e| e.starts_with("timeout: ") && e.contains("still at version %1:0.0.0.0.0:")
            ),
            "{again:?}"
        );
        Ok(())
    }

    /// A closed pane's answer is still its pane's: caught, a word that
    /// wants a pane finds that pane gone, rather than refusing the kind.
    #[test]
    fn a_closed_panes_answer_names_its_pane() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let version = output(&mut s, "%1 vertical split")?;
        run(&mut s, "%2 kill")?;
        let caught = format!(
            "str closed try str {} changed wait catch drop end",
            version.trim()
        );
        assert_eq!(
            output(&mut s, &format!("{caught} json"))?.trim(),
            r#"{"pane":"%2","status":4}"#
        );
        let gone = output(&mut s, &format!("{caught} capture")).err();
        assert!(
            gone.as_ref().is_some_and(|e| e.starts_with("no_pane: ")),
            "{gone:?}"
        );
        Ok(())
    }

    /// A version names its pane and its server run: a wait from it waits
    /// in that pane, one from another run is refused, as is one its pane
    /// never had, and one of a pane since closed answers `closed`. A
    /// pane part way through a frame at the limit has changed: its answer
    /// waits for the frame, never says it is unchanged.
    #[test]
    fn a_version_names_its_pane_and_its_server_run() -> Result<(), Box<dyn std::error::Error>> {
        let mut s = started(Config::default())?;
        run(&mut s, "str %1 pane horizontal split")?;
        let failed = |s: &mut Session, code: &str| output(s, code).err().unwrap_or_default();
        let of = |s: &Session, pane| s.pane(PaneId::of(pane)).map(|p| p.version(s.run));
        let (one, two) = (of(&s, 1)?, of(&s, 2)?);
        s.output(PaneId::of(2), b"E");
        // From %2's version, though asked nowhere else: %2's change.
        let answer = output(&mut s, &format!("str {two} changed 0 timeout wait"))?;
        assert!(
            answer.starts_with("%2:") && answer.ends_with(" screen\n"),
            "{answer}"
        );
        let answer = failed(&mut s, &format!("str {one} changed 0 timeout wait"));
        assert!(answer.contains("timeout"), "{answer}");
        let other = command::Version {
            run: "1-0".parse().map_err(|()| "a run")?,
            ..one
        };
        let refused = failed(&mut s, &format!("str {other} changed wait"));
        assert!(refused.contains("from another server run"), "{refused}");
        let never = command::Version {
            counts: command::Counts {
                bells: 9,
                ..one.counts
            },
            ..one
        };
        let refused = failed(&mut s, &format!("str {never} changed wait"));
        assert!(refused.contains("never had these counts"), "{refused}");
        let unmade = command::Version {
            pane: PaneId::of(99),
            ..one
        };
        let refused = failed(&mut s, &format!("str {unmade} changed wait"));
        assert!(refused.contains("never a pane of this run"), "{refused}");
        run(&mut s, "%2 kill")?;
        let closed = failed(&mut s, &format!("str {two} changed wait"));
        assert!(closed.starts_with("closed: "), "{closed}");
        // A frame part read at the limit.
        let mut frame = b"\x1b[?2026h".to_vec();
        frame.extend(crate::pane::tests::spends_the_budget(23, 80));
        frame.extend_from_slice(b"DONE\x1b[?2026l");
        let since = of(&s, 1)?;
        s.output(PaneId::of(1), &frame);
        let wait = Request::Wait(command::Wait {
            since: command::Since::Version(since),
            timeout: crate::config::Limit::secs(0),
        });
        let Begun::Waiting(waiting) = s.begin(wait, &Origin::Cli(Caller::default())) else {
            return Err("a wait mid-frame answered at once".into());
        };
        let due = waiting.due().ok_or("a wait with no time")?;
        while s.pane(PaneId::of(1))?.mid_frame() {
            assert!(waiting.answer(&s, due).is_none());
            s.fire(Timer::Unread(PaneId::of(1)));
        }
        let answer = waiting.answer(&s, due).ok_or("no answer")?;
        let text = said(&Ok(answer));
        assert!(text.ends_with(" screen"), "{text}");
        Ok(())
    }

    /// Workspaces are found by name, so no two share one: a name given is
    /// refused if taken, and a name made up skips those taken. Making one
    /// that fails part way uses up none of the IDs it took.
    #[test]
    fn workspace_names_stay_unique_and_failures_use_no_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut s = started(Config::default())?;
        assert!(run(&mut s, "opts 80 24 size str main name new-workspace").is_err());
        assert_eq!(
            output(&mut s, "opts 80 24 size str workspace-3 name new-workspace")?,
            format!("%2:0.0.0.0.0:{}\n", s.run)
        );
        // +3 would be workspace-3, which is taken.
        let third = format!("%3:0.0.0.0.0:{}\n", s.run);
        assert_eq!(output(&mut s, "opts 80 24 size new-workspace")?, third);
        let names: Vec<&str> = s.parts.workspaces().iter().map(|w| w.name()).collect();
        assert_eq!(names, ["main", "workspace-3", "workspace-4"]);
        // A move to a new workspace with no tab ID left takes no workspace
        // ID either.
        s.ids.exhaust_tabs();
        let ids = s.ids.clone();
        assert!(run(&mut s, "str %1 pane str new-workspace move-to").is_err());
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
        for line in [
            "ls",
            "str %1 pane capture",
            "list-keys",
            "str %1 pane str x text send-keys",
            "str c1 client str x text send-keys",
        ] {
            run(&mut s, line)?;
        }
        assert!(
            !dirty(&s, one) && !dirty(&s, two),
            "reading repaints no one"
        );
        assert!(run(&mut s, "str %9 pane kill").is_err());
        assert!(
            !dirty(&s, one) && !dirty(&s, two),
            "a failure repaints no one"
        );
        run(&mut s, "str %1 pane horizontal split")?;
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
        // Two rows of a cell or none, never three: 24 bytes a row, 8 a cell.
        let mut s = started(Config {
            history_bytes: crate::config::Number::new(2 * 32).ok_or("out of range")?,
            ..Config::default()
        })?;
        let client = s.attach(6, 20, None)?;
        s.output(PaneId::of(1), b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        run(&mut s, "str c1 client copy-mode")?;
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
        // Two rows of a cell or none, never three: 24 bytes a row, 8 a cell.
        let mut s = started(Config {
            history_bytes: crate::config::Number::new(2 * 32).ok_or("out of range")?,
            ..Config::default()
        })?;
        let client = s.attach(6, 20, None)?;
        s.output(PaneId::of(1), b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        run(&mut s, "str c1 client copy-mode")?;
        s.input(client, b"g");
        s.resize(client, 3, 20)?;
        let view = s.views.get(&client).ok_or("the view")?;
        assert!(matches!(view.mode, Mode::Normal), "copy mode ended");
        Ok(())
    }

    /// A client's size is at most `view::MOST` each way, and only a size
    /// that changes it resizes and repaints it: the same size again changes
    /// nothing, and one past the bound, or of no rows, is refused.
    #[test]
    fn only_a_size_that_changes_a_client_resizes_it() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = attached(10, 40)?;
        let past = view::MOST.saturating_add(1);
        for (rows, changes) in [
            (10, Some(false)),
            (view::MOST, Some(true)),
            (view::MOST, Some(false)),
            (past, None),
            (0, None),
            (3, Some(true)),
        ] {
            if let Some(view) = s.views.get_mut(&c) {
                view.dirty = false;
            }
            let resized = s.resize(c, rows, 40).ok();
            let dirty = s.views.get(&c).is_some_and(|v| v.dirty);
            assert_eq!(
                (resized, dirty),
                (changes, changes == Some(true)),
                "{rows} rows"
            );
        }
        assert!(s.attach(past, 40, None).is_err());
        Ok(())
    }

    /// What no command may do is refused: a name a target would read as
    /// an id, or blank, or too long, or taken; a move with nowhere to go.
    #[test]
    fn what_cannot_be_done_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        let long = format!(
            "str %1 pane str {} rename",
            std::iter::repeat_n("x", NAME_MAX.saturating_add(1)).collect::<String>()
        );
        for code in [
            "opts str %3 name new-workspace",
            "str +1 workspace str +2 rename",
            "str +1 workspace str @2 rename",
            "str %1 pane str '' rename",
            long.as_str(),
            "str %1 pane str up 1 move-border",
            "str c1 client str last focus",
            "str @1 tab str next reorder",
            "opts str main name new-workspace",
        ] {
            assert!(run(&mut s, code).is_err(), "{code}");
        }
        Ok(())
    }

    /// A client's screen asked for from elsewhere is relayed to the client,
    /// which opens it, or shows why it cannot.
    #[test]
    fn a_screen_asked_from_elsewhere_is_the_clients() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = attached(10, 40)?;
        run(&mut s, "str c1 client str %1 pane choose-pane")?;
        let notice = s
            .views
            .get(&c)
            .and_then(|v| v.notice.clone())
            .map(|n| n.text);
        assert_eq!(
            notice.as_deref(),
            Some("nothing_there: choose-pane: only one pane (key:1)")
        );
        run(&mut s, "str c1 client choose-tab")?;
        assert!(matches!(
            s.views.get(&c).map(|v| &v.mode),
            Some(Mode::Panel(crate::act::Panel::List(_)))
        ));
        Ok(())
    }

    /// A bare id names its pane, tab or workspace, but not as `str`'s text;
    /// a pane's name names it through `pane-named`, a title never; `json`
    /// after a capture gives its JSON.
    #[test]
    fn ids_and_json_read_as_written() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        assert_eq!(output(&mut s, "%1 str name get")?, "sh\n");
        assert_eq!(output(&mut s, "@1 str name get")?, "main\n");
        // A pane's name finds it by `pane-named` alone; a title its
        // program set never does.
        assert_eq!(output(&mut s, "str sh pane-named")?, "%1\n");
        assert!(output(&mut s, "str sh pane").is_err(), "a tab's name only");
        s.output(PaneId::of(1), b"\x1b]2;spool\x07");
        assert!(output(&mut s, "str spool pane-named").is_err(), "a title");
        assert_eq!(output(&mut s, "str %1")?, "%1\n");
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.starts_with(r#"{"pane":"%1""#), "{json}");
        Ok(())
    }

    /// A capture's cursor is the screen's row and column, as answers and
    /// a mouse event count them, whatever its lines join or take: on a
    /// blank row trimmed away too; none while hidden, or on the screen not
    /// captured.
    #[test]
    fn a_captures_cursor_is_the_screens() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        let cursor = |s: &mut Session, read: &str| -> Result<String, String> {
            let json = output(s, &format!("%1 {read} capture json"))?;
            let after = json.split("\"screen_cursor\":").nth(1).unwrap_or_default();
            Ok(after
                .split(",\"lines\"")
                .next()
                .unwrap_or_default()
                .to_owned())
        };
        s.output(PaneId::of(1), b"0123456789\x1b[1;6H");
        assert_eq!(cursor(&mut s, "")?, "[0,5]");
        assert_eq!(cursor(&mut s, "2 7 cols")?, "[0,5]");
        assert_eq!(cursor(&mut s, "0 2 cols")?, "[0,5]");
        s.output(PaneId::of(1), b"\x1b[?25l");
        assert_eq!(cursor(&mut s, "")?, "null", "hidden");
        s.output(PaneId::of(1), b"\x1b[?25h\x1b[3;1H");
        assert_eq!(cursor(&mut s, "")?, "[2,0]", "on a blank row trimmed");
        s.output(
            PaneId::of(1),
            &[b"\x1b[2J\x1b[H".as_slice(), &[b'A'; 45], b"\r\nBC"].concat(),
        );
        assert_eq!(cursor(&mut s, "")?, "[2,2]");
        assert_eq!(cursor(&mut s, "wrapped")?, "[2,2]");
        s.output(PaneId::of(1), b"\x1b[?1049h\x1b[HALT");
        assert_eq!(cursor(&mut s, "alt-screen")?, "[0,3]");
        assert_eq!(cursor(&mut s, "main-screen")?, "null");
        s.output(PaneId::of(1), b"\x1b[?1049l");
        assert_eq!(cursor(&mut s, "alt-screen")?, "null");
        assert_eq!(cursor(&mut s, "main-screen")?, "[2,2]");
        // What is highlighted is marked in JSON's lines too.
        s.output(PaneId::of(1), b"\x1b[7mSEL\x1b[0m");
        let json = output(&mut s, "%1 marks capture json")?;
        assert!(json.contains("\"BC⟦SEL⟧\""), "{json}");
        let inverted = output(&mut s, "%1 5 2 rows capture");
        assert!(
            inverted.as_ref().is_err_and(|e| e.starts_with("range: ")),
            "{inverted:?}"
        );
        // A glyph in the last column leaves the cursor on it, the wrap
        // pending: said so.
        s.output(PaneId::of(1), &[b"\r\n".as_slice(), &[b'D'; 40]].concat());
        assert_eq!(cursor(&mut s, "wrapped")?, "[3,39]");
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.contains(r#""wrap_pending":true"#), "{json}");
        s.output(PaneId::of(1), b"\r");
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.contains(r#""wrap_pending":false"#), "{json}");
        Ok(())
    }

    /// An action's answer is its pane, to every word that wants a pane:
    /// `pane`, `tab` and `workspace` too; and `json` gives it as it gives
    /// it alone, in a list too.
    #[test]
    fn an_actions_answer_is_its_pane() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        let act = "%1 str x text send-keys";
        assert_eq!(output(&mut s, &format!("{act} str name get"))?, "sh\n");
        assert_eq!(
            output(&mut s, &format!("{act} dup tab swap workspace"))?,
            "@1\n+1\n"
        );
        let alone = output(&mut s, &format!("{act} json"))?;
        assert!(
            alone.starts_with("{\"pane\":\"%1\",\"version\":"),
            "{alone}"
        );
        let listed = output(&mut s, &format!("list {act} end json"))?;
        assert_eq!(listed, format!("[{}]\n", alone.trim_end()));
        Ok(())
    }

    /// A failure is one line, says what a word takes and how it is used,
    /// and is of the kind it is: a string counted is a type error, a file
    /// not there says so.
    #[test]
    fn failures_say_what_a_word_takes() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        let failed = |s: &mut Session, code: &str| match output(s, code) {
            Ok(out) => format!("ran: {out}"),
            Err(error) => error,
        };
        let timeout = failed(&mut s, "%1 -1 timeout wait");
        assert!(timeout.starts_with("range: "), "{timeout}");
        assert!(
            timeout.contains("-1 is out of range: from 0 (use: "),
            "{timeout}"
        );
        let mouse = failed(&mut s, "%1 str click -1 0 mouse send-mouse");
        assert!(mouse.contains("out of range: 0 to 65535 (use: "), "{mouse}");
        // The core's words say how they are used too; a library word by
        // its own name.
        for code in ["slice", "str a 1 add"] {
            let core = failed(&mut s, code);
            assert!(core.contains(" (use: "), "{core}");
        }
        let upper = failed(&mut s, "3 upper");
        assert!(upper.contains("(use: S upper"), "{upper}");
        // Caught, it is the failing word's own message (here `each`'s,
        // inside `upper`): no place, word or usage added.
        let caught = output(&mut s, "str type try 3 upper catch drop end");
        assert_eq!(
            caught.as_deref(),
            Ok("each walks a string or a list, not an integer\n")
        );
        // A server word's answer that overflows the stack fails at the
        // word, as an immediate word's would: a `try` catches it.
        let full = "str stack_overflow try 0 1000000 range each end ls catch 2drop str caught end";
        assert_eq!(output(&mut s, full).as_deref(), Ok("caught\n"));
        // A wait's limit is a day at most: its deadline a time there is.
        let long = failed(&mut s, "%1 9223372036854775807 timeout wait");
        assert!(
            long.starts_with("range: ") && long.contains("at most 86400 s"),
            "{long}"
        );
        // A capture where text is taken: `string` makes it text.
        let text = failed(&mut s, "%1 capture lines");
        assert!(text.contains("`string` makes it text"), "{text}");
        // A capture's option given what is no pane says where it goes.
        for (code, shown) in [
            ("%1 capture last-output", "fux %3 last-output capture"),
            ("%1 capture 0 4 rows", "fux %3 0 4 rows capture"),
        ] {
            let option = failed(&mut s, code);
            assert!(option.contains("(use: a capture option, PANE "), "{option}");
            assert!(option.contains(&format!("; e.g. {shown})")), "{option}");
        }
        let env = failed(&mut s, "opts str 1bad str v env new-tab");
        assert!(env.contains("not first a digit"), "{env}");
        let missing = failed(&mut s, "str /no/such/file.fux str program check");
        assert!(missing.contains("No such file"), "{missing}");
        let split = failed(&mut s, "str 'two\nlines' fail");
        assert!(split.ends_with(r"two\nlines"), "{split}");
        let version = failed(&mut s, "str 1.2.3 changed wait");
        assert!(version.contains("is no version"), "{version}");
        Ok(())
    }

    /// What a stream of events subscribed now is told of what `change`
    /// does to `s`.
    fn told(s: &mut Session, change: impl FnOnce(&mut Session)) -> String {
        let stream = crate::events::Subscription::new(None, s.parts.journal(), s.run, None);
        // From now is never refused; if it were, the caller's comparison
        // shows why.
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(why) => return why,
        };
        change(s);
        stream
            .told(s.parts.journal(), usize::MAX, |t| s.holder(t))
            .0
    }

    /// A rename and its undo in one request are both told, in order: what
    /// changed and changed back is not lost.
    #[test]
    fn a_rename_and_its_undo_are_both_told() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let mut ran = Ok(());
        let lines = told(&mut s, |s| {
            ran = run(
                s,
                "str +1 workspace str x rename str +1 workspace str main rename",
            );
        });
        ran?;
        assert_eq!(
            lines,
            concat!(
                r#"{"event":"renamed","workspace":"+1","name":"x"}"#,
                "\n",
                r#"{"event":"renamed","workspace":"+1","name":"main"}"#,
                "\n"
            )
        );
        Ok(())
    }

    /// A tab made with a name is told made with it, not with another
    /// and then renamed.
    #[test]
    fn a_tab_made_with_a_name_is_told_made_with_it() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let mut ran = Ok(());
        let lines = told(&mut s, |s| {
            let tab = "opts 80 24 size str t name str +1 workspace in new-tab drop";
            ran = run(s, tab);
        });
        ran?;
        assert_eq!(
            lines,
            concat!(
                r#"{"event":"created","tab":"@2","workspace":"+1","name":"t"}"#,
                "\n",
                r#"{"event":"created","pane":"%2","tab":"@2","workspace":"+1"}"#,
                "\n"
            )
        );
        Ok(())
    }

    /// Changes to two panes and a tab are told in the order they happened,
    /// not by where they happened.
    #[test]
    fn events_are_told_in_the_order_they_happened() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "%1 split drop")?;
        let mut ran = Ok(());
        let lines = told(&mut s, |s| {
            s.output(PaneId::of(2), b"\x07");
            ran = run(s, "str @1 tab str t rename");
            s.output(PaneId::of(1), b"\x1b]2;title\x07");
        });
        ran?;
        let said = concat!(
            r#"{"event":"bell","pane":"%2","tab":"@1","workspace":"+1"}"#,
            "\n",
            r#"{"event":"renamed","tab":"@1","workspace":"+1","name":"t"}"#,
            "\n",
            r#"{"event":"title","pane":"%1","tab":"@1","workspace":"+1","title":"title"}"#,
            "\n"
        );
        assert_eq!(lines, said);
        Ok(())
    }

    /// A shell pane's exit is told, its status with it, then its close
    /// (and the pane that takes its room).
    #[test]
    fn a_shell_panes_exit_is_told_before_its_close() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "%1 split drop")?;
        let lines = told(&mut s, |s| s.exited(PaneId::of(2), Ended::Signalled(9)));
        let said = concat!(
            r#"{"event":"exited","pane":"%2","tab":"@1","workspace":"+1","signal":9}"#,
            "\n",
            r#"{"event":"closed","pane":"%2","tab":"@1","workspace":"+1"}"#,
            "\n",
            r#"{"event":"resized","pane":"%1","tab":"@1","workspace":"+1","cols":80,"rows":24}"#,
            "\n"
        );
        assert_eq!(lines, said);
        Ok(())
    }

    /// What a shell wrote as it exited, a frame it held too, is read
    /// before its exit is told: its bell and title are not lost.
    #[test]
    fn a_shells_last_output_is_told_before_its_exit() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "%1 split drop")?;
        let lines = told(&mut s, |s| {
            s.output(PaneId::of(2), b"\x1b[?2026h\x07\x1b]2;bye\x07");
            s.exited(PaneId::of(2), Ended::Exited(0));
        });
        let said = concat!(
            r#"{"event":"bell","pane":"%2","tab":"@1","workspace":"+1"}"#,
            "\n",
            r#"{"event":"title","pane":"%2","tab":"@1","workspace":"+1","title":"bye"}"#,
            "\n",
            r#"{"event":"exited","pane":"%2","tab":"@1","workspace":"+1","exited":0}"#,
            "\n",
            r#"{"event":"closed","pane":"%2","tab":"@1","workspace":"+1"}"#,
            "\n"
        );
        assert!(lines.starts_with(said), "{lines}");
        Ok(())
    }

    /// A dump names only the tabs given a name: one fux named by
    /// its place (`tab-3`, its place now 2) comes back named by its place
    /// again, not as a name given, which a tab named so later could not
    /// take.
    #[test]
    fn a_dump_names_only_the_tabs_given_a_name() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let tab = "opts 80 24 size str +1 workspace in new-tab drop";
        run(&mut s, &format!("{tab} {tab} str @2 tab kill"))?;
        run(&mut s, &format!("{tab} str @4 tab str given rename"))?;
        let dump = s.dump_layout(None);
        assert!(!dump.contains("tab-3"), "{dump}");
        assert!(dump.contains("given"), "{dump}");
        Ok(())
    }

    /// A shell whose directory cannot be read (here none runs) is dumped
    /// without one, never with `$HOME` in its place: a comment names it.
    #[test]
    fn a_dump_leaves_out_a_directory_it_cannot_read() -> Result<(), String> {
        let dump = started(Config::default())?.dump_layout(None);
        assert!(dump.contains("# %1: its directory is unknown"), "{dump}");
        assert!(!dump.contains("cwd"), "{dump}");
        Ok(())
    }

    /// A pane whose directory is not UTF-8 text, which a fuxth string
    /// cannot hold, is dumped without it, never with a changed path: a
    /// comment names the pane. Linux only: macOS's file systems refuse a
    /// name that is not UTF-8.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_dump_leaves_out_a_directory_that_is_not_text() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::ffi::OsStrExt;
        let dir = (fuxix::file::scratch()?.join(format!("fux-dump-{}", std::process::id())))
            .join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        std::fs::create_dir_all(&dir)?;
        let mut s = started(Config::default())?;
        let program = Command::Run {
            workspace: None,
            name: None,
            size: Some(crate::view::Screen::new(24, 80)?),
            cwd: Some(dir.clone()),
            env: crate::command::Env::default(),
            argv: vec!["cat".into()],
        };
        s.execute(&program, &Origin::Cli(Caller::default()))?;
        let dump = s.dump_layout(None);
        let _ = dir.parent().map(std::fs::remove_dir_all);
        let told = dump.contains("# %2: its directory \"") && dump.contains("caf\\xE9");
        let written = dump
            .lines()
            .any(|l| !l.starts_with('#') && l.contains("caf"));
        assert!(told && !written, "{dump}");
        let mut again = started(Config::default())?;
        run(&mut again, &dump)?;
        assert_eq!(again.parts.panes().len(), 3, "{dump}");
        Ok(())
    }

    /// A dump makes a tab that fits clients' screens at the size it has,
    /// and it fits them again; a tab given a size keeps it.
    #[test]
    fn a_dump_keeps_which_tabs_fit_clients() -> Result<(), String> {
        let (mut s, _) = attached(11, 40)?;
        run(&mut s, "opts 60 20 size str +1 workspace in new-tab drop")?;
        let dump = s.dump_layout(None);
        let mut again = started(Config::default())?;
        run(&mut again, &dump)?;
        let tabs = (again.parts.workspaces().iter()).flat_map(|w| w.tabs());
        let fits: Vec<_> = tabs.map(|t| matches!(t.fit(), Fit::Clients(_))).collect();
        assert!(fits.ends_with(&[true, false]), "{dump}");
        Ok(())
    }

    /// A tab as big as a client's screen may be, which a tab that fits
    /// clients takes, is made again from its dump.
    #[test]
    fn a_dump_makes_a_tab_as_big_as_a_screen_again() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 600 40 size str +1 workspace in new-tab drop")?;
        let mut again = started(Config::default())?;
        run(&mut again, &s.dump_layout(None))?;
        let tabs = (again.parts.workspaces().iter()).flat_map(|w| w.tabs());
        let mut sizes = tabs.map(|t| t.fit().size());
        let wide = sizes.any(|s| (s.cols(), s.rows()) == (600, 40));
        assert!(wide, "{}", s.dump_layout(None));
        Ok(())
    }

    /// A dump of a tab is made again however its clients showed it: a
    /// pane zoomed, which has the whole screen; a screen too small for
    /// every pane, whose size the tab does not take.
    #[test]
    fn a_dump_is_made_again_however_its_tab_was_shown() -> Result<(), String> {
        let (mut s, c) = attached(11, 40)?;
        run(&mut s, "%1 split drop %1 vertical split drop")?;
        run(&mut s, "str c1 client %1 focus-pane str c1 client zoom")?;
        let replays = |s: &Session| {
            let dump = s.dump_layout(None);
            let mut again = started(Config::default())?;
            run(&mut again, &dump).map_err(|e| format!("{e}\n{dump}"))?;
            Ok::<_, String>(again.parts.panes().len())
        };
        assert_eq!(replays(&s)?, 4);
        run(&mut s, "str c1 client zoom")?;
        s.resize(c, 2, 2).map_err(|e| e.to_string())?;
        assert_eq!(replays(&s)?, 4);
        Ok(())
    }

    /// Tab `tab`'s panes' places on its own size, in tree order.
    fn rects(s: &Session, tab: TabId) -> Vec<Rect> {
        let Some((root, area)) = s.tab(tab).map(|t| (t.root(), t.fit().area())) else {
            return Vec::new();
        };
        let placed = layout::place(root, area);
        root.panes()
            .iter()
            .filter_map(|p| placed.rect(*p))
            .collect()
    }

    /// Tab `tab` dumped and made again where no client is: its panes'
    /// places there.
    fn replayed(s: &Session, tab: TabId) -> Result<Vec<Rect>, String> {
        let dump = s.dump_layout(Some(AnyRef::Tab(tab)));
        let mut again = started(Config::default())?;
        run(&mut again, &dump).map_err(|e| format!("{e}\n{dump}"))?;
        let made = (again.parts.workspaces().iter())
            .flat_map(|w| w.tabs())
            .last();
        Ok(made.map(|t| rects(&again, t.id())).unwrap_or_default())
    }

    /// A tab given a size is split only while that size shows every pane,
    /// though a bigger screen shows it: no pane is hidden once that screen
    /// goes, and its dump makes it again.
    #[test]
    fn a_tab_given_a_size_splits_only_while_it_shows_every_pane() -> Result<(), String> {
        let (mut s, _) = attached(50, 200)?;
        let tab = "opts 40 12 size str +1 workspace in new-tab drop";
        run(&mut s, &format!("{tab} str c1 client str @2 tab show-tab"))?;
        // Four panes stacked take their 2 rows each and 3 separators.
        run(
            &mut s,
            "%2 vertical split drop %2 vertical split drop %2 vertical split drop",
        )?;
        let refused = run(&mut s, "%2 vertical split drop")
            .err()
            .unwrap_or_default();
        assert!(
            refused.starts_with("size: ") && refused.contains("40x12"),
            "{refused}"
        );
        let tab: TabId = "@2".parse().map_err(|_| "@2")?;
        assert_eq!(replayed(&s, tab)?, rects(&s, tab));
        Ok(())
    }

    /// A tab that fits clients is split only while every screen showing it
    /// shows every pane, but one that only watches: the size it keeps,
    /// which its dump makes it at, shows them all.
    #[test]
    fn a_tab_fitting_clients_splits_only_while_each_screen_shows_every_pane() -> Result<(), String>
    {
        let (mut s, _) = attached(50, 200)?;
        s.attach(50, 30, None).map_err(|e| e.to_string())?;
        watch(&mut s, 50, 8)?;
        // Ten panes side by side take their 2 columns each and 9 separators.
        for _ in 1..10 {
            run(&mut s, "%1 split drop")?;
        }
        let refused = run(&mut s, "%1 split drop").err().unwrap_or_default();
        assert!(
            refused.starts_with("size: ") && refused.contains("30x"),
            "{refused}"
        );
        let tab: TabId = "@1".parse().map_err(|_| "@1")?;
        assert_eq!(replayed(&s, tab)?, rects(&s, tab));
        Ok(())
    }

    /// Panes are made again at the places they had: 81, 9 and 8 of 100
    /// columns, each split's nearest to its shares.
    #[test]
    fn a_dump_makes_its_panes_again_where_they_were() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let tab = "opts 100 10 size str +1 workspace in new-tab drop";
        let splits = "%2 80 18 shares split drop %3 10 8 shares split drop";
        run(&mut s, &format!("{tab} {splits}"))?;
        let tab: TabId = "@2".parse().map_err(|_| "@2")?;
        let widths: Vec<u16> = rects(&s, tab).iter().map(Rect::w).collect();
        assert_eq!(widths, [81, 9, 8]);
        assert_eq!(replayed(&s, tab)?, rects(&s, tab));
        Ok(())
    }

    /// A pane split with shares starts at the size its share gives it,
    /// not resized to it after its program starts.
    #[test]
    fn a_split_with_shares_starts_at_its_share() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts 100 10 size str +1 workspace in new-tab drop")?;
        let lines = told(&mut s, |s| {
            let _ = run(s, "%2 80 18 shares split drop");
        });
        assert!(lines.contains(r#""created","pane":"%3""#), "{lines}");
        assert!(!lines.contains(r#""resized","pane":"%3""#), "{lines}");
        let size = s.parts.panes().get(&PaneId::of(3)).map(Pane::size);
        assert_eq!(size, Some(sized(10, 18)?));
        Ok(())
    }

    /// A pane starts at the size settling gives it, however many clients
    /// show its tab: a split's, the smallest of the screens showing it, a
    /// client zoomed that follows it unzoomed; a tab's its client follows,
    /// that client's screen, not the size given for while none shows it.
    #[test]
    fn a_pane_starts_at_the_size_its_screens_give_it() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let wide = s.attach(11, 101, None).map_err(|e| e.to_string())?;
        s.attach(11, 81, None).map_err(|e| e.to_string())?;
        let size = |s: &Session, id| s.parts.panes().get(&PaneId::of(id)).map(Pane::size);
        // The narrow one, zoomed, follows the split unzoomed.
        run(&mut s, "str c2 client zoom")?;
        let lines = told(&mut s, |s| {
            let _ = run(s, "%1 split drop");
        });
        assert!(
            lines.contains(r#""created","pane":"%2""#)
                && !lines.contains(r#""resized","pane":"%2""#),
            "{lines}"
        );
        assert_eq!(size(&s, 2), Some(sized(10, 40)?));
        let lines = told(&mut s, |s| {
            let _ = output_as(s, Some(wide), "opts 30 5 size new-tab drop");
        });
        assert!(
            lines.contains(r#""created","pane":"%3""#)
                && !lines.contains(r#""resized","pane":"%3""#),
            "{lines}"
        );
        assert_eq!(size(&s, 3), Some(sized(10, 101)?));
        Ok(())
    }

    /// A client with no room for a pane sizes no new tab it makes: its
    /// tab does; nor is a pane moved where the client following it would
    /// hide one.
    #[test]
    fn a_screen_with_no_room_sizes_no_pane() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let small = s.attach(5, 6, None).map_err(|e| e.to_string())?;
        // A client of one row has no room: its tab, which the other
        // client's screen sizes, sizes the tab it makes.
        let one = s.attach(1, 40, None).map_err(|e| e.to_string())?;
        s.attach(11, 40, None).map_err(|e| e.to_string())?;
        s.detach(small);
        output_as(&mut s, Some(one), "opts str +1 workspace in new-tab drop")?;
        let size = s.parts.panes().get(&PaneId::of(2)).map(Pane::size);
        assert_eq!(size, Some(sized(10, 40)?));
        let small = s.attach(5, 6, None).map_err(|e| e.to_string())?;
        let made = "opts 100 10 size str +1 workspace in new-tab \
                    dup split drop dup split drop split drop";
        run(&mut s, made)?;
        let refused = output_as(&mut s, Some(small), "%2 str @3 tab move-to").err();
        let refused = refused.unwrap_or_default();
        assert!(
            refused.contains("no room") && refused.contains("screen"),
            "{refused}"
        );
        Ok(())
    }

    /// A screen with no room below its bar shows no layout and sizes
    /// nothing: it refuses no split of the tab it shows, and a pane it
    /// moves to a tab made for it is in that tab, never leaving one empty.
    #[test]
    fn a_screen_with_no_room_refuses_nothing() -> Result<(), String> {
        let (mut s, _) = attached(11, 40)?;
        let tiny = s.attach(1, 40, None).map_err(|e| e.to_string())?;
        run(&mut s, "%1 split drop %1 split drop")?;
        for (pane, to) in [("%1", "new-tab"), ("%2", "new-workspace")] {
            let lines = told(&mut s, |s| {
                let moved = output_as(s, Some(tiny), &format!("{pane} str {to} move-to"));
                assert!(moved.is_ok(), "{moved:?}");
            });
            assert!(
                lines.contains(r#""created""#)
                    && lines.contains(&format!(r#""moved","pane":"{pane}""#)),
                "{lines}"
            );
        }
        Ok(())
    }

    /// No border moves on a screen too small to show every pane: a hidden
    /// pane has no place to move one from.
    #[test]
    fn no_border_moves_where_a_pane_is_hidden() -> Result<(), String> {
        let (mut s, c) = attached(11, 40)?;
        run(&mut s, "%1 split drop %2 split drop")?;
        s.resize(c, 11, 6).map_err(|e| e.to_string())?;
        let refused = run(&mut s, "%1 str right 1 move-border").err();
        let refused = refused.unwrap_or_default();
        assert!(
            refused.starts_with("size: ") && refused.contains("every pane on 6x"),
            "{refused}"
        );
        Ok(())
    }

    /// Shares divide the room of the pane split, whatever is beside it,
    /// exactly: the others keep theirs, and a dump makes the split again,
    /// whatever the shares.
    #[test]
    fn shares_divide_the_split_panes_room() -> Result<(), String> {
        let tab: TabId = "@2".parse().map_err(|_| "@2")?;
        let widths = |s: &Session| rects(s, tab).iter().map(Rect::w).collect::<Vec<u16>>();
        let made = "opts 101 10 size str +1 workspace in new-tab drop";
        for halved in ["", "1 1 shares"] {
            let mut s = started(Config::default())?;
            let split = format!("%2 {halved} split drop %3 2 1 shares split drop");
            run(&mut s, &format!("{made} {split}"))?;
            assert_eq!(widths(&s), [50, 33, 16], "{halved}");
            assert_eq!(replayed(&s, tab)?, rects(&s, tab));
        }
        let mut s = started(Config::default())?;
        run(&mut s, &format!("{made} %2 4294967294 1 shares split drop"))?;
        for split in [
            "%3 split drop",
            "%3 vertical 4294967295 1 shares split drop",
        ] {
            run(&mut s, split)?;
            assert_eq!(replayed(&s, tab)?, rects(&s, tab), "{split}");
        }
        Ok(())
    }

    /// Whatever changes a layout, a dump makes it again: each pane where
    /// it was, at every size.
    #[test]
    fn every_layout_change_dumps_as_it_is() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let made = "opts 100 30 size str +1 workspace in new-tab drop \
                    opts 100 30 size str +1 workspace in new-tab drop";
        run(&mut s, made)?;
        let tab: TabId = "@2".parse().map_err(|_| "@2")?;
        let areas = [
            Rect::screen(30, 100),
            Rect::screen(11, 37),
            Rect::screen(70, 250),
        ];
        for change in [
            "%2 3 2 shares split drop",
            "%4 vertical 1 2 shares split drop",
            "%5 split drop",
            "%5 vertical split drop",
            "%2 37 resize-x",
            "%5 7 resize-y",
            "%6 str left 3 move-border",
            "%2 30 percent resize-x",
            "%2 %6 swap-panes",
            "%4 str left move-beside",
            "%5 kill",
            "%3 str @2 tab move-to",
            "%6 kill",
        ] {
            run(&mut s, change)?;
            let dump = s.dump_layout(Some(AnyRef::Tab(tab)));
            let mut again = started(Config::default())?;
            run(&mut again, &dump).map_err(|e| format!("{change}: {e}\n{dump}"))?;
            let made = (again.parts.workspaces().iter())
                .flat_map(|w| w.tabs())
                .last();
            let (Some(root), Some(made)) = (s.root(tab), made.map(Tab::root)) else {
                return Err(format!("{change}: no layout"));
            };
            for area in areas {
                let places = |root: &Tree| {
                    let placed = layout::place(root, area);
                    root.panes()
                        .iter()
                        .map(|p| placed.rect(*p))
                        .collect::<Vec<_>>()
                };
                assert_eq!(places(made), places(root), "{change}\n{dump}");
            }
        }
        Ok(())
    }

    /// A pane is not moved where its tab cannot show it, as one is not
    /// split there: it would be hidden, and its dump not made again.
    #[test]
    fn a_pane_is_not_moved_where_it_would_be_hidden() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let split = "opts 5 4 size str +1 workspace in new-tab drop %2 split drop";
        run(&mut s, split)?;
        let refused = run(&mut s, "%1 str @2 tab move-to").err();
        let refused = refused.unwrap_or_default();
        assert!(
            refused.starts_with("size: ") && refused.contains("no room"),
            "{refused}"
        );
        let tab: TabId = "@2".parse().map_err(|_| "@2")?;
        assert_eq!(s.root(tab).map(|r| r.panes().len()), Some(2));
        Ok(())
    }

    /// What a caller tells apart, it tells by variant.
    #[test]
    fn failures_are_told_apart_by_kind() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = attached(10, 40)?;
        let origin = Origin::Client(c);
        let mut run =
            |code: &str| -> Result<Vec<Result<Reply, Error>>, Box<dyn std::error::Error>> {
                let commands = requests(&mut s, c, code)?;
                Ok(commands
                    .iter()
                    .map(|command| s.execute(command, &origin))
                    .collect())
            };
        assert!(matches!(
            run("me str next focus")?.as_slice(),
            [Err(Error::OnlyOne(Kind::Pane))]
        ));
        let kill = Command::Kill {
            target: AnyRef::Pane(PaneId::of(9)),
        };
        assert!(matches!(
            s.execute(&kill, &origin),
            Err(Error::NoPane(p)) if p == PaneId::of(9)
        ));
        assert!(matches!(
            s.attach(10, 40, Some("%1")),
            Err(e) if e.code() == Code::NoWorkspace
        ));
        assert!(matches!(
            s.attach(10, 40, Some("+x")),
            Err(e) if e.code() == Code::BadValue && e.to_string().contains("+x")
        ));
        Ok(())
    }
}

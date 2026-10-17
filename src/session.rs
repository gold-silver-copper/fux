//! The server's state: workspaces, tabs, panes and the clients' views, and
//! every command that changes them.
use crate::code::Code;
use crate::command::{
    self, AnyRef, Armed, ClientAction, Command, Extent, Input, Kind, Mouse, MoveTo, PanePick,
    PaneResize, Pick, Request, Resized, Sibling, SwapWith, WsRef,
};
use crate::config::Config;
use crate::copy::MAX_CELLS;
use crate::id::Wanted;
use crate::id::{ClientId, Ids, PaneId, TabId, WsId};
use crate::json::{Fields, Json};
use crate::keys::Direction;
use crate::layout::{self, Axis, Placement, Rect, Side, Tree};
use crate::pane::{InputQueue, Pane, Process, Runs, Typed, basename};
use crate::process::Child;
use crate::protocol::{End, Reply, ToAttached};
use crate::view::{ClientKind, Mode, View};
use crate::wait::{Follows, Waiting};
use crate::workspace::{Fit, Seat, Tab, Workspace};
use fuxth::Code as Lang;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
mod layouts;
use std::time::{Duration, Instant};

/// What the server must do outside the session.
#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// A frame for a client: bytes for its terminal outside any paint (a
    /// copy, OSC 52; questions for the terminal; the bell; the keyboard
    /// flags pushed), fuxth to run as its own, keys to take as typed at its
    /// terminal (`send-keys` to a client), its screen ended by the server,
    /// or its detach, saying why.
    To(ClientId, ToAttached<'static>),
    /// Stop the server, saying why; with `keep`, its layout is kept for
    /// the next server to bring back, as after a crash.
    Shutdown { reason: String, keep: bool },
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
#[derive(Clone, Debug)]
pub enum Origin {
    /// A client's key, prompt or menu: where the client is.
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
    pub env: Vec<(String, String)>,
    pub came: Came,
}

/// How a caller on the CLI came to the server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Came {
    /// By the server's own socket.
    #[default]
    Socket,
    /// By the socket of a grant: what panes it starts are told to reach
    /// the server by, so that what runs there is held to the grant too.
    Grant(PathBuf),
    /// It is the server's own restore of a saved session: the ids asked for
    /// are the ones it had, and programs wait for `respawn`.
    Restore,
}

/// The server whose pane a caller on the CLI runs in, as its
/// `FUX_SERVER` says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Within {
    /// None said: a pane it gives is taken as this server's.
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
            Within::Unsaid | Within::This => self.gave,
            Within::Other => None,
        }
    }
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
type Picked = (WsId, Option<Spot>);

/// Where in a workspace a client is shown: a tab, and a pane in it if
/// given.
pub(crate) type Spot = (TabId, Option<PaneId>);

/// What a command that names nothing acts on, found once from its origin.
#[derive(Clone, Copy)]
enum Here<'a> {
    /// An attached client, where it is.
    Client(ClientId, Place),
    /// The CLI: its caller's pane (if it gave one) and the workspace and
    /// tab that pane is in, if it is still there; and its working
    /// directory.
    Cli(Option<(PaneId, Option<(WsId, TabId)>)>, Option<&'a Path>),
}

impl Here<'_> {
    /// The CLI with no caller: a command run for no one.
    const NOWHERE: Here<'static> = Here::Cli(None, None);

    fn place(self) -> Option<Place> {
        match self {
            Here::Client(_, place) => Some(place),
            Here::Cli(pane, _) => pane.and_then(|(pane, at)| {
                at.map(|(ws, tab)| Place {
                    ws,
                    tab,
                    pane: Some(pane),
                })
            }),
        }
    }
    fn pane(self) -> Option<PaneId> {
        match self {
            Here::Client(_, place) => place.pane,
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
    /// `id` asked for an id given before, or one there is.
    IdTaken(AnyRef),
    NoClient(ClientId),
    NoBuffer(usize),
    /// The client focuses no pane.
    NoPaneToCopy,
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
    /// `run` with a name a tab has: `str NAME tab` would not know which, so the program is not started. The tab and its panes.
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
pub(crate) fn fail(code: impl Into<Code>, message: impl Into<String>) -> Error {
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
            Error::NoWorkspace(_) => Code::NoWorkspace,
            Error::NoPane(_) | Error::NoPaneToCopy => Code::NoPane,
            Error::NoTab(_) => Code::NoTab,
            Error::NoPaneGiven | Error::NoWorkspaceGiven => Code::NoTarget,
            Error::IdTaken(_) => Code::Id,
            Error::NoClient(_) => Code::NoClient,
            Error::NoBuffer(_) => Code::NoBuffer,
            Error::OnlyOne(_) | Error::AtEnd(_) | Error::NoNeighbor { .. } => Code::NothingThere,
            Error::NoRows | Error::SelectionTooLarge => Code::Selection,
            Error::NameReadAsTarget(_) => Code::Lang(Lang::BadName),
            Error::NameTaken(_) | Error::TabNameTaken(..) => Code::NameTaken,
            Error::Failed { code, .. } => *code,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoWorkspace(id) => write!(f, "no workspace {id}"),
            Error::NoPaneGiven => f.write_str("no pane given: name one (`str %N pane`)"),
            Error::NoPane(id) => write!(f, "no pane {id}"),
            Error::NoTab(id) => write!(f, "no tab {id}"),
            Error::NoWorkspaceGiven => f.write_str(
                "no workspace given: name one (`str NAME workspace`; for new-tab, the \
                     option `str NAME in`)",
            ),
            Error::IdTaken(id) => write!(
                f,
                "id {id}: that id, or a higher one, was given before, and ids are never given twice"
            ),
            Error::NoClient(id) => write!(f, "no client {id}"),
            Error::NoBuffer(index) => write!(f, "no buffer {index}"),
            Error::NoPaneToCopy => f.write_str("no pane to copy from"),
            Error::OnlyOne(kind) => write!(f, "only one {}", kind.name()),
            Error::AtEnd(kind) => write!(f, "the {} is already at that end", kind.name()),
            Error::NoNeighbor { from, direction } => {
                write!(f, "no pane {} of {from}", direction.name())
            }
            Error::NoRows => f.write_str("the pane has no rows"),
            Error::SelectionTooLarge => write!(f, "the selection is larger than {MAX_CELLS} cells"),
            Error::NameTaken(name) => write!(f, "a workspace is named {name:?}"),
            Error::TabNameTaken(name, tabs, panes) => {
                let tabs_then_panes = (tabs.iter().map(|t| AnyRef::Tab(*t)))
                    .chain(panes.iter().map(|p| AnyRef::Pane(*p)));
                let ids = joined(tabs_then_panes, " ");
                let kills = joined(tabs, " tab kill`, `fux str ");
                write!(
                    f,
                    "a tab is named {name:?} already ({ids}); close it (`fux str {kills} tab kill`) or choose another name"
                )
            }
            Error::NameReadAsTarget(name) => write!(
                f,
                "{name:?} cannot be a name: a target would read it as a pane, tab or workspace number"
            ),
            Error::Failed { message, .. } => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {}

impl From<crate::pane::Error> for Error {
    fn from(error: crate::pane::Error) -> Error {
        let code = match error {
            crate::pane::Error::NotReading => Code::NotReading,
            crate::pane::Error::Terminal { .. } => Code::Start,
        };
        fail(code, error.to_string())
    }
}

/// `pane`'s program has ended, with `status`: it takes no input.
fn ended(pane: PaneId, status: crate::process::Ended) -> Error {
    let was = match status {
        crate::process::Ended::Exited(_) => "",
        crate::process::Ended::Signalled(_) => "was ",
    };
    let told = crate::process::told(status);
    fail(Code::Exited, format!("the program in {pane} {was}{told}"))
}

/// What a command line from the CLI begins: its outcome, now, or a wait
/// for one (`wait`, or an action that settles).
pub enum Begun {
    Done(Reply),
    Waiting(Box<Waiting>),
    /// `fux events`: a stream, until the client goes.
    Events(Box<crate::events::Subscription>),
    /// A grant of a socket to the workspace (`grant`), or with none, its
    /// revoking (`revoke`): the server's, which listens.
    Grant(String, Option<WsRef>),
}

/// How long a closing pane's processes have to exit after the hangup.
pub const GRACE: Duration = Duration::from_millis(100);
/// The longest a typed command waits for the new shell to write and then
/// go quiet (`pane::QUIET`): a shell that writes nothing still gets it.
pub const TYPE_WAIT: Duration = Duration::from_secs(1);
/// The size a pane starts at, until it is laid out on a client's screen.
const DEFAULT_SIZE: (u16, u16) = (24, 80);
/// A typical terminal's screen, which a tab no client shows is laid out on.
const TYPICAL: Rect = Rect::screen(DEFAULT_SIZE.0, DEFAULT_SIZE.1);
/// The most rows, and columns, a client's view is kept to.
const MOST: u16 = 4096;

pub(crate) fn default_size() -> fux_vt::Size {
    crate::pane::size(DEFAULT_SIZE.0, DEFAULT_SIZE.1)
}

pub struct Session {
    pub workspaces: Vec<Workspace>,
    pub panes: BTreeMap<PaneId, Pane>,
    pub views: BTreeMap<ClientId, View>,
    pub config: Config,
    /// Paste buffers, newest first.
    pub buffers: VecDeque<String>,
    /// The socket a started pane is told to reach its server by, if panes
    /// start real processes; tests of the state alone start none.
    launch: Option<PathBuf>,
    /// Which run of a server this is: its panes' `FUX_SERVER`, by which a
    /// command from a pane tells whether the pane is this server's.
    pub server_id: String,
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
    /// The sockets `fux grant` made, each with the workspace it reaches,
    /// for `ls` to list: the server holds them.
    pub grants: Vec<Grant>,
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
    set: Vec<(String, String)>,
    unset: Vec<String>,
    /// The socket they reach the server by, if not its own: a grant's.
    socket: Option<PathBuf>,
    /// A restore is under way (`Session::restore`): ids come back as they
    /// were, and programs wait for `respawn`.
    restoring: bool,
}

/// The name of the first workspace, and of each workspace's first tab.
pub(crate) const MAIN: &str = "main";

/// The workspace `fux run` starts programs in unless told another.
pub const RUN_WORKSPACE: &str = "agent";

/// A log file, made anew: what a program writes may be secret, so it is
/// the user's alone.
/// A link is not followed, and a file there already is emptied and made
/// the user's alone too.
fn open_log(path: &Path) -> Result<std::fs::File, Error> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let failed = |e| fail(Code::Log, format!("the log {}: {e}", path.display()));
    if path.is_symlink() {
        let link = std::io::Error::other("a symbolic link, which a log does not follow");
        return Err(failed(link));
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(fuxix::file::NOFOLLOW)
        .open(path)
        .map_err(failed)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(failed)?;
    Ok(file)
}

/// What a command from a granted socket may reach: the granted workspace,
/// if it is there, and nothing outside it.
struct Reach<'a> {
    session: &'a Session,
    ws: Option<WsId>,
    scope: &'a str,
    here: Here<'a>,
}

impl Reach<'_> {
    fn outside(&self, what: String) -> Error {
        let scope = self.scope;
        let message =
            format!("{what} is not in workspace {scope}, the only one this socket reaches (grant)");
        fail(Code::NotGranted, message)
    }
    fn refuse(&self, what: &'static str) -> Result<(), Error> {
        let scope = self.scope;
        let message = format!("this socket reaches workspace {scope} alone (grant), not {what}");
        Err(fail(Code::NotGranted, message))
    }
    fn pane(&self, id: PaneId) -> Result<(), Error> {
        match self.session.locate(id) {
            Some((ws, _)) if Some(ws) == self.ws => Ok(()),
            Some(_) | None => Err(self.outside(id.to_string())),
        }
    }
    fn tab(&self, tab: TabId) -> Result<(), Error> {
        match self.session.tab_workspace(tab) {
            Ok(ws) if Some(ws) == self.ws => Ok(()),
            Ok(_) | Err(_) => Err(self.outside(tab.to_string())),
        }
    }
    fn workspace(&self, ws: &WsRef) -> Result<(), Error> {
        match self.session.resolve_ws(ws) {
            Ok(id) if Some(id) == self.ws => Ok(()),
            Ok(id) => Err(self.outside(id.to_string())),
            Err(_) => Err(self.outside(match ws {
                WsRef::Id(id) => id.to_string(),
                WsRef::Name(name) => format!("{name:?}"),
            })),
        }
    }
    fn any(&self, target: &AnyRef) -> Result<(), Error> {
        match target {
            AnyRef::Pane(p) => self.pane(*p),
            AnyRef::Tab(t) => self.tab(*t),
            AnyRef::Workspace(w) => self.workspace(&WsRef::Id(*w)),
        }
    }

    /// Whether `request` reaches only the granted workspace.
    fn request(&self, request: &Request) -> Result<(), Error> {
        match request {
            Request::Do(command) | Request::Then(command, _) => self.command(command),
            Request::Wait(target, _) => self.any(target),
            Request::Capture(target, ..) => self.pane(*target),
            Request::Events(target, _) => target.as_ref().map_or(Ok(()), |t| self.any(t)),
            Request::Grant(..) | Request::Revoke(_) => self.refuse("another grant"),
        }
    }

    /// Whether `command` reaches only the granted workspace.
    fn command(&self, command: &Command) -> Result<(), Error> {
        match command {
            Command::Ls { target, .. } => target.as_ref().map_or(Ok(()), |t| self.any(t)),
            Command::Resolve { text, kind } => {
                let found = self.session.resolve(text, *kind, self.here);
                let found = found.map_err(|_| self.outside(format!("{text:?}")))?;
                self.any(&found)
            }
            Command::KillServer { .. } => self.refuse("kill-server"),
            Command::NewWorkspace { .. } => self.refuse("new-workspace"),
            Command::Kill { target } => match target {
                AnyRef::Workspace(_) => self.refuse("killing the workspace"),
                AnyRef::Pane(_) | AnyRef::Tab(_) => self.any(target),
            },
            Command::NewTab { target, .. } => target.as_ref().map_or(Ok(()), |w| self.workspace(w)),
            Command::Run { workspace, .. } => match workspace {
                // Run makes the workspace it names if it is not there.
                Some(WsRef::Name(name)) if name == self.scope => Ok(()),
                Some(ws) => self.workspace(ws),
                None => Ok(()),
            },
            Command::Split { target, .. }
            | Command::Respawn { target, .. }
            | Command::SendMouse { target, .. }
            | Command::Signal { target, .. }
            | Command::ResizePane { target, .. }
            | Command::Log { target, .. }
            | Command::ClearHistory { target }
            | Command::CapturePane { target, .. } => self.pane(*target),
            Command::Notify { target, .. } => match target {
                Some(target) => self.any(target),
                None => self.refuse("a notice to every client"),
            },
            Command::Resize(what) => match what {
                Resized::Tabs(target, _) => self.any(target),
                Resized::Client(..) => self.refuse("a client's size"),
            },
            Command::SendKeys { target, .. } => self.any(target),
            Command::Rename { target, .. } => match target {
                AnyRef::Workspace(_) => self.refuse("renaming the workspace"),
                AnyRef::Pane(_) | AnyRef::Tab(_) => self.any(target),
            },
            Command::MovePane { target, to } => {
                self.pane(*target)?;
                match to {
                    MoveTo::Tab(tab) => self.tab(*tab),
                    MoveTo::Workspace(ws) => self.workspace(ws),
                    MoveTo::NewTab | MoveTo::Beside(_) => Ok(()),
                    MoveTo::NewWorkspace => self.refuse("a new workspace"),
                }
            }
            Command::SwapPane { target, with } => {
                self.pane(*target)?;
                match with {
                    SwapWith::Pane(pane) => self.pane(*pane),
                    SwapWith::Toward(_) => Ok(()),
                }
            }
            Command::Reorder { target, .. } => match target {
                AnyRef::Pane(pane) => self.pane(*pane),
                AnyRef::Tab(tab) => self.tab(*tab),
                AnyRef::Workspace(_) => self.refuse("reordering workspaces"),
            },
            Command::Set { .. } => self.refuse("the server's settings"),
            Command::ListBuffers | Command::ShowBuffer { .. } | Command::PasteBuffer { .. } => {
                self.refuse("the paste buffers, which are everyone's")
            }
            Command::Client { .. } => self.refuse("a client's screen"),
            Command::DumpLayout { target } => target.as_ref().map_or(Ok(()), |t| self.any(t)),
        }
    }
}

/// The one item of `items`; of several, why not: `many` says it from how
/// many there are and their ids.
fn one<T: Copy + std::fmt::Display>(
    items: &[T],
    many: impl FnOnce(usize, String) -> String,
) -> Result<T, Error> {
    match items {
        [item] => Ok(*item),
        items => Err(fail(Code::NoTarget, many(items.len(), joined(items, " ")))),
    }
}

/// `items`, each as it displays, with `between` between them.
fn joined<T: std::fmt::Display>(items: impl IntoIterator<Item = T>, between: &str) -> String {
    let items: Vec<String> = items.into_iter().map(|item| item.to_string()).collect();
    items.join(between)
}

/// A value a command can't take, as `Usage` says why.
fn bad_value(usage: command::Usage) -> Error {
    fail(Code::BadValue, usage.to_string())
}

/// A command that failed, told with its code: a usage error's
/// status is 2.
fn failure(error: Error, origin: &Origin) -> Reply {
    // A command that needed its caller's pane, tab or workspace, from a
    // caller whose `FUX_PANE` is another server's pane: why it got none.
    let error = match (error, origin) {
        (
            Error::NoPaneGiven | Error::NoWorkspaceGiven,
            Origin::Cli(Caller {
                gave: Some(pane),
                within: Within::Other,
                ..
            }),
        ) => fail(
            Code::StrayPane,
            format!(
                "FUX_PANE {pane} is a pane of another fux server, not of this one, so it is not \
                 acted on; name the pane"
            ),
        ),
        (error, _) => error,
    };
    Reply::failed(error.code(), error.to_string())
}

impl Session {
    pub fn new(config: Config, socket: PathBuf, launch: bool) -> Session {
        // The process and the moment it started: no two runs share both.
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let server_id = format!("{}-{since:x}", std::process::id());
        Session {
            workspaces: Vec::new(),
            panes: BTreeMap::new(),
            views: BTreeMap::new(),
            config,
            buffers: VecDeque::new(),
            launch: launch.then_some(socket),
            server_id,
            outbox: Vec::new(),
            dying: BTreeMap::new(),
            last_colours: crate::outer::Colours::default(),
            last_palette: crate::outer::Palette::default(),
            shown_sizes: Vec::new(),
            placed: Placement::default(),
            ids: Ids::default(),
            changes: 0,
            grants: Vec::new(),
            #[cfg(test)]
            test_keys: BTreeMap::new(),
            #[cfg(test)]
            test_keymap: crate::keymap::Keymap::default(),
        }
    }

    /// One workspace, holding one tab with one shell.
    pub fn start(&mut self) -> Result<(), Error> {
        let home = home();
        let start = (Start::Shell(&[], &[]), home.as_path(), &Starting::default());
        let names = (Some(MAIN.into()), None);
        self.create_workspace(names, start, (Fit::Clients, default_size()))
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
        self.shown_workspace(client)?.tab_of(client)
    }
    pub(crate) fn shown_tab_mut(&mut self, client: ClientId) -> Option<&mut Tab> {
        let ws = (self.workspaces.iter_mut()).find(|w| w.viewers.contains(&client))?;
        ws.tab_of_mut(client)
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
                .ok_or_else(|| fail(Code::NoWorkspace, format!("no workspace named {name:?}"))),
        }
        .map(|w| w.id)
    }
    /// `target`, if it is there; else why not.
    pub fn there(&self, target: AnyRef) -> Result<AnyRef, Error> {
        let there = match target {
            AnyRef::Pane(p) => self.panes.get(&p).map(|_| ()).ok_or(Error::NoPane(p)),
            AnyRef::Tab(t) => self.tab(t).map(|_| ()).ok_or(Error::NoTab(t)),
            AnyRef::Workspace(w) => self.workspace(w).map(|_| ()).ok_or(Error::NoWorkspace(w)),
        };
        there.map(|()| target)
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
                let pane = caller.pane().map(|p| (p, self.locate(p)));
                Ok(Here::Cli(pane, caller.cwd.as_deref()))
            }
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

    fn pane(&self, id: PaneId) -> Result<&Pane, Error> {
        self.panes.get(&id).ok_or(Error::NoPane(id))
    }

    fn pane_mut(&mut self, id: PaneId) -> Result<&mut Pane, Error> {
        self.panes.get_mut(&id).ok_or(Error::NoPane(id))
    }

    /// Starts `argv` with `env` as pane `id`'s program, in `cwd` at `size`,
    /// if panes start real processes: told of its server (`FUX_PANE`,
    /// `FUX_SOCKET`, a grant's socket if its starter came by one, and
    /// `FUX_SERVER`), given its starter's variables, and not those its
    /// starter had not, unless `env` sets them. A shell fux knows is started
    /// so that it marks its commands, unless `set shell-marks off`; its
    /// startup files go beside the server's socket.
    fn spawn(
        &self,
        id: PaneId,
        (argv, env): (&[String], &[(String, String)]),
        (cwd, fresh): (&Path, &Starting),
        size: fux_vt::Size,
    ) -> Result<Option<(crate::process::Child, Option<String>)>, Error> {
        let Some(socket) = &self.launch else {
            return Ok(None);
        };
        let mut dir = socket.as_os_str().to_owned();
        dir.push(".shell");
        let marked = (self.config.shell_marks)
            .then(|| crate::marks::marked(argv, env, Path::new(&dir)))
            .flatten();
        // A shell fux marks is given a nonce its marks carry: one printed
        // by a program it runs carries none (`fux_vt::Parser::require_mark_nonce`).
        let nonce = marked.as_ref().and_then(|_| crate::marks::nonce());
        let mut marks = marked.unwrap_or_else(|| crate::marks::Marked {
            argv: argv.to_vec(),
            ..crate::marks::Marked::default()
        });
        if let Some(nonce) = &nonce {
            marks.env.push(("__fux_k".to_owned(), nonce.clone()));
        }
        let socket = fresh.socket.as_deref().unwrap_or(socket);
        let mut vars = vec![
            ("FUX_PANE", id.to_string()),
            ("FUX_SOCKET", socket.to_string_lossy().into_owned()),
            ("FUX_SERVER", self.server_id.clone()),
        ];
        let given = (fresh.set.iter()).chain(env).chain(&marks.env);
        vars.extend(given.map(|(k, v)| (k.as_str(), v.clone())));
        vars.retain(|(k, _)| !marks.unset.iter().any(|u| u == k));
        let mut unset: Vec<String> = (fresh.unset.iter())
            .filter(|name| !env.iter().any(|(k, _)| k == *name))
            .cloned()
            .collect();
        unset.extend(marks.unset.iter().cloned());
        let child = crate::process::spawn(&marks.argv, cwd, (&vars, &unset), size)
            .map_err(|m| fail(Code::Start, m))?;
        Ok(Some((child, nonce)))
    }

    /// Starts the program pane `id` ran again in it: its words, or `argv`
    /// if given, where it ran, with what it was given and `env` set; first
    /// hanging up the one there with `kill`, else refused while it runs.
    fn respawn(
        &mut self,
        id: PaneId,
        kill: bool,
        (env, argv): (&[(String, String)], &[String]),
        starting: &Starting,
    ) -> Result<(), Error> {
        let pane = self.pane(id)?;
        let Runs::Program(launch) = &pane.runs else {
            let message = format!("{id} runs a shell; respawn starts again what `fux run` started");
            return Err(fail(Code::NotRun, message));
        };
        if pane.process.child().is_some() && !kill {
            let message = format!("the program in {id} still runs; `kill-first` ends it first");
            return Err(fail(Code::StillRunning, message));
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
        let spawned = self.spawn(
            id,
            (&launch.argv, &launch.env),
            (&launch.cwd, starting),
            size,
        )?;
        let (child, nonce) = spawned.unzip();
        let history = self.config.history_bytes;
        let pane = self.pane_mut(id)?;
        let old = pane.respawned(
            launch,
            child.map_or(Process::Absent, Process::Reading),
            history,
        )?;
        pane.mark_with(nonce.flatten());
        self.end(id, old, true);
        self.touch();
        Ok(())
    }

    /// The process group `fux signal` signals in a pane: the job its shell
    /// runs in the foreground, or the program `run` started; never a shell.
    fn signalled(&self, target: PaneId) -> Result<crate::process::Pid, Error> {
        let pane = self.pane(target)?;
        if let Some(status) = pane.process.exit_status() {
            return Err(ended(pane.id, status));
        }
        // The program `run` started, unless that is a shell (`run -- bash`):
        // its foreground job, if any, is what a signal is for.
        let program = match (&pane.runs, pane.process.child()) {
            (crate::pane::Runs::Program(launch), Some(child)) => {
                // A shell taking commands typed at it: no -c, no script.
                let shell = launch.argv.split_first().is_some_and(|(p, args)| {
                    let interactive = args.iter().all(|a| a.starts_with('-') && a != "-c");
                    interactive && SHELLS.contains(&basename(p).trim_start_matches('-'))
                });
                (!shell).then(|| child.leader.pid())
            }
            _ => None,
        };
        pane.process.job().or(program).ok_or_else(|| {
            let message = format!("nothing is running in {} but its shell", pane.id);
            fail(Code::OnlyShell, message)
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
        if self.workspaces.iter().any(|w| w.name == name) {
            return Err(Error::NameTaken(name.to_owned()));
        }
        no_tab_named(&self.workspaces, name, except)
    }

    /// Gives tab `id` the name `name`, checked. It takes it from each other
    /// tab fux named so (`tab-2`, made before a dump's line names one so):
    /// that one is named anew as fux names tabs, by its place, the next
    /// number free.
    fn give_tab_name(&mut self, id: TabId, name: String) -> Result<(), Error> {
        let tab = (tabs_mut(&mut self.workspaces).find(|t| t.id == id)).ok_or(Error::NoTab(id))?;
        tab.name.clone_from(&name);
        tab.given = true;
        let mut taken: std::collections::HashSet<String> = (self.workspaces.iter())
            .flat_map(|ws| std::iter::once(&ws.name).chain(ws.tabs().iter().map(|t| &t.name)))
            .cloned()
            .collect();
        for ws in &mut self.workspaces {
            for (place, tab) in (1usize..).zip(ws.tabs_mut().iter_mut()) {
                if tab.given || tab.name != name {
                    continue;
                }
                let free = (place..)
                    .map(crate::workspace::numbered)
                    .find(|n| !taken.contains(n));
                if let Some(free) = free {
                    taken.insert(free.clone());
                    tab.name = free;
                }
            }
        }
        Ok(())
    }

    /// Whether a client is in copy mode on pane `pane`: keys typed there
    /// go to copy mode, not the program.
    pub(crate) fn copying(&self, pane: PaneId) -> bool {
        self.views
            .values()
            .any(|v| matches!(&v.mode, Mode::Copy(copy) if copy.pane == pane))
    }

    /// What panes started for `origin` take of its environment: none of
    /// it for one that sent none (a test, a client of no terminal).
    fn starting_for(&self, origin: &Origin) -> Starting {
        let (env, socket, restoring) = match origin {
            Origin::Cli(caller) => match &caller.came {
                Came::Socket => (caller.env.as_slice(), None, false),
                Came::Grant(socket) => (caller.env.as_slice(), Some(socket.clone()), false),
                Came::Restore => (caller.env.as_slice(), None, true),
            },
            Origin::Client(client) => (
                self.views.get(client).map_or(&[][..], |v| v.env.as_slice()),
                None,
                false,
            ),
        };
        let mut fresh = Starting {
            socket,
            restoring,
            ..Starting::default()
        };
        for name in (self.config.update_environment.iter()).filter(|_| !env.is_empty()) {
            match env.iter().rev().find(|(k, _)| k == name) {
                Some((k, v)) => fresh.set.push((k.clone(), v.clone())),
                None => fresh.unset.push(name.clone()),
            }
        }
        fresh
    }

    /// The next ids made are `wanted`'s (`id`): none that is there or
    /// (outside a restore) was ever given.
    fn want(&mut self, wanted: Wanted, again: bool) -> Result<(), Error> {
        if let Some(id) = wanted.each().find(|id| self.there(*id).is_ok()) {
            return Err(Error::IdTaken(id));
        }
        self.ids.want(wanted, again)
    }

    /// What a command that makes a pane prints: the ids it made, from
    /// one of `from`'s kind down; all of them go in its reply.
    fn made_ids((ws, tab, pane): (WsId, TabId, PaneId), from: Kind) -> Reply {
        let ids = vec![AnyRef::Workspace(ws), AnyRef::Tab(tab), AnyRef::Pane(pane)];
        let said = joined(ids.iter().skip_while(|id| id.kind() != from), " ");
        Reply {
            ids,
            ..format!("{said}\n").into()
        }
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
            (AnyRef::Workspace(w), Kind::Tab) => {
                self.there(id)?;
                one(&self.tab_ids(w), many(Kind::Tab)).map(AnyRef::Tab)
            }
        }
    }

    /// What `text` names as a pane, tab or workspace, `kind`: its id (`%3`),
    /// or a tab's name (a pane, its tab's one pane); a workspace's name.
    /// A pane, nothing given: the caller's own.
    fn resolve(&self, text: &str, kind: Kind, here: Here) -> Result<AnyRef, Error> {
        let tabs: Vec<&Tab> = (self.workspaces.iter())
            .flat_map(Workspace::tabs)
            .filter(|t| t.name == text)
            .collect();
        let panes_of = |tab: &Tab| tab.root().map(Tree::panes).unwrap_or_default();
        let named = || -> Result<&Tab, Error> {
            match tabs.as_slice() {
                [tab] => Ok(*tab),
                [] => {
                    let code = match kind {
                        Kind::Pane => Code::NoPane,
                        Kind::Tab => Code::NoTab,
                        Kind::Workspace => Code::NoWorkspace,
                    };
                    Err(fail(code, format!("no {} named {text:?}", kind.name())))
                }
                many => {
                    let each: Vec<String> = (many.iter())
                        .map(|t| format!("{} {}", t.id, joined(panes_of(t), " ")))
                        .collect();
                    Err(fail(
                        Code::NoTarget,
                        format!(
                            "{text:?} names {} tabs ({}): use an id",
                            many.len(),
                            each.join(", ")
                        ),
                    ))
                }
            }
        };
        if let Ok(id) = text.parse() {
            return self.stands_for(id, text, kind);
        }
        match kind {
            Kind::Pane if text.is_empty() => Ok(AnyRef::Pane(self.pane(here.caller_pane()?)?.id)),
            Kind::Pane => {
                // A tab's name, else a pane's own (what `rename` gave
                // it, or its program's), else the title its program
                // set, as `ls` shows it.
                let tab = match named() {
                    Ok(tab) => tab,
                    Err(no_tab) => {
                        let with = |of: fn(&Pane) -> &str| {
                            let mut panes: Vec<PaneId> = (self.panes.values())
                                .filter(|p| of(p) == text)
                                .map(|p| p.id)
                                .collect();
                            panes.sort();
                            panes
                        };
                        let mut panes = with(|p| p.name.as_str());
                        if panes.is_empty() {
                            panes = with(|p| p.title.as_str());
                        }
                        if panes.is_empty() {
                            return Err(no_tab);
                        }
                        let many = |n, ids| format!("{text:?} names {n} panes ({ids}): use an id");
                        return one(&panes, many).map(AnyRef::Pane);
                    }
                };
                let many =
                    |n, ids| format!("{text:?} is a tab of {n} panes ({ids}): use a pane's id");
                one(&panes_of(tab), many).map(AnyRef::Pane)
            }
            Kind::Tab => named().map(|tab| AnyRef::Tab(tab.id)),
            Kind::Workspace => {
                let ws = command::parse_workspace(text).map_err(bad_value)?;
                Ok(AnyRef::Workspace(self.ws_target(Some(&ws), here)?))
            }
        }
    }

    /// The panes `target` names: a pane, or a tab's or workspace's every
    /// pane, in order.
    fn panes_of(&self, target: AnyRef) -> Result<Vec<PaneId>, Error> {
        Ok(match target {
            AnyRef::Pane(pane) => vec![self.pane(pane)?.id],
            AnyRef::Tab(tab) => self.root(tab).ok_or(Error::NoTab(tab))?.panes(),
            AnyRef::Workspace(ws) => (self.workspace(ws).ok_or(Error::NoWorkspace(ws))?)
                .tabs()
                .iter()
                .filter_map(Tab::root)
                .flat_map(Tree::panes)
                .collect(),
        })
    }

    /// Sends `inputs` to pane `id`'s program, a piece at a time (`units`).
    fn send_inputs(&mut self, id: PaneId, inputs: &[Input]) -> Result<(), Error> {
        let p = self.live_pane_mut(id)?;
        p.note_input();
        let units = units(p, inputs);
        Ok(p.send(units, Instant::now())?)
    }

    /// `pane_mut`, for input: a pane whose program has exited takes none.
    /// `pane_mut`, for input: a pane whose program has exited takes none.
    fn live_pane_mut(&mut self, id: PaneId) -> Result<&mut Pane, Error> {
        let pane = self.pane_mut(id)?;
        match pane.process.exit_status() {
            Some(status) => Err(ended(pane.id, status)),
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
        // No place for it, or one it takes from another pane: the tab is
        // too small to show another pane.
        let placed = layout::place(&root, area);
        let shows_all = root.panes().iter().all(|p| placed.rect(*p).is_some());
        let rect = placed
            .rect(new)
            .filter(|_| shows_all)
            .ok_or_else(|| {
                let message = format!(
                    "{target}'s tab has no room for another pane; make it bigger first (`TAB COLS ROWS resize`)"
                );
                fail(Code::Size, message)
            })?;
        Ok(crate::pane::size(rect.h(), rect.w()))
    }

    /// Resizes `pane`, placed in `area`, to `cols` and `rows`, either or
    /// both; nothing changes unless both can be met.
    fn resize_pane_to(
        &mut self,
        pane: PaneId,
        area: Rect,
        axis: Axis,
        extent: Extent,
    ) -> Result<String, Error> {
        let cells = |extent: Extent, of: u16| match extent {
            Extent::Cells(n) => n,
            Extent::Percent(p) => p.of(of),
        };
        let (_, tab) = self.locate(pane).ok_or(Error::NoPane(pane))?;
        let mut root = self.root(tab).cloned().ok_or(Error::NoPane(pane))?;
        let now = layout::place(&root, area)
            .rect(pane)
            .ok_or(Error::NoPane(pane))?;
        let len = cells(extent, area.len(axis));
        let want = match axis {
            Axis::Horizontal => (len, now.h()),
            Axis::Vertical => (now.w(), len),
        };
        let refused = || {
            let message = format!("{pane} cannot be made {}x{} in its tab", want.0, want.1);
            fail(Code::Size, message)
        };
        root = layout::resize_to(&root, area, pane, axis, len).ok_or_else(refused)?;
        // A size the layout cannot give exactly is not met.
        let got = layout::place(&root, area)
            .rect(pane)
            .map(|r| (r.w(), r.h()));
        if got != Some(want) {
            return Err(refused());
        }
        tab_of(&mut self.workspaces, pane)?.edit(|old| *old = Some(root));
        Ok(format!("{pane} {}x{}\n", want.0, want.1))
    }

    fn ws_target(&self, explicit: Option<&WsRef>, here: Here) -> Result<WsId, Error> {
        match explicit {
            Some(r) => self.resolve_ws(r),
            None => here.place().map(|p| p.ws).ok_or(Error::NoWorkspaceGiven),
        }
    }

    // ------------------------------------------------------------ creation

    fn cwd_for(&self, here: Here, near: Option<PaneId>) -> PathBuf {
        let near = match here {
            Here::Cli(_, Some(cwd)) => return cwd.to_owned(),
            Here::Cli(_, None) => near,
            Here::Client(_, place) => near.or(place.pane),
        };
        let leader = near.and_then(|p| self.panes.get(&p)?.process.child());
        leader
            .and_then(|c| fuxix::process::cwd(c.leader.pid()))
            .unwrap_or_else(home)
    }

    /// Where a new pane's program starts: `dir`, from the caller's
    /// directory, or with none the directory of the caller, or of `near`.
    fn start_dir(
        &self,
        here: Here,
        dir: Option<&str>,
        near: Option<PaneId>,
    ) -> Result<PathBuf, Error> {
        let cwd = match dir {
            Some(dir) => self.cwd_for(here, None).join(dir),
            None => self.cwd_for(here, near),
        };
        match cwd.is_dir() {
            true => Ok(cwd),
            false => Err(fail(
                Code::NotADirectory,
                format!("{} is not a directory", cwd.display()),
            )),
        }
    }

    /// The name a workspace gets when none is given: `workspace-N` after
    /// its ID, or the next number up that no workspace is named.
    fn workspace_name(&self, id: WsId) -> String {
        let free = |name: &String| !self.workspaces.iter().any(|ws| ws.name == *name);
        let mut names = (id.number()..=u32::MAX).map(|n| format!("workspace-{n}"));
        names
            .find(free)
            .unwrap_or_else(|| format!("workspace-{}", u32::MAX))
    }

    /// A workspace with one tab, laid out on `fit`, holding one pane that
    /// starts what `start` says, `size` to begin with.
    fn create_workspace(
        &mut self,
        (name, tab_name): (Option<String>, Option<String>),
        start: (Start<'_>, &Path, &Starting),
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
        let pane = new_pane(self, &mut ids, start, size)?;
        self.ids = ids;
        let name = name.unwrap_or_else(|| self.workspace_name(id));
        let made = (id, tab, pane.id);
        let root = Some(Tree::Pane(pane.id));
        let clients = self.views.keys().copied();
        let tab = (tab, tab_name.unwrap_or_else(|| MAIN.into()));
        (self.workspaces).push(Workspace::new(id, name, tab, (root, fit), clients));
        self.panes.insert(pane.id, pane);
        Ok(made)
    }

    /// Adds a tab named `name` to workspace `ws`, laid out on `fit`,
    /// holding one pane that starts what `start` says, `size` to begin with.
    /// A workspace not there that `wanted` names by its name is made, the
    /// tab its first.
    fn create_tab(
        &mut self,
        (ws, wanted): (Result<WsId, Error>, Option<&WsRef>),
        name: Option<String>,
        start: (Start<'_>, &Path, &Starting),
        place: (Fit, fux_vt::Size),
    ) -> Result<(WsId, TabId, PaneId), Error> {
        let ws = match (ws, wanted) {
            (Ok(ws), _) => ws,
            (Err(_), Some(WsRef::Name(ws_name))) => {
                return self.create_workspace((Some(ws_name.clone()), name), start, place);
            }
            (Err(error), _) => return Err(error),
        };
        if let Some(name) = &name {
            check_name(name)?;
        }
        // Found before its pane's program starts, which nothing can then
        // leave running. As for a workspace: the ID first, committed after
        // the pane.
        let at = self.workspaces.iter().position(|w| w.id == ws);
        let at = at.ok_or(Error::NoWorkspace(ws))?;
        let mut ids = self.ids;
        let id = ids.tab()?;
        let pane = new_pane(self, &mut ids, start, place.1)?;
        let workspace = self.workspaces.get_mut(at).ok_or(Error::NoWorkspace(ws))?;
        self.ids = ids;
        let made = (ws, id, pane.id);
        workspace.add_tab(id, name, (Some(Tree::Pane(pane.id)), place.0));
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
            Some(name) => self.resolve_ws(&command::parse_workspace(name).map_err(bad_value)?)?,
            None => {
                // A server whose session did not come back starts afresh.
                if self.workspaces.is_empty() {
                    self.start()?;
                }
                self.workspaces
                    .first()
                    .map(|w| w.id)
                    .ok_or_else(|| fail(Code::NoWorkspace, "the server has no workspace"))?
            }
        };
        let id = self.ids.client()?;
        let mut view = View::new(id, rows.clamp(1, MOST), cols.clamp(1, MOST));
        view.kind = kind;
        self.views.insert(id, view);
        self.workspaces.iter_mut().for_each(|w| w.attach(id));
        self.show(id, ws, None);
        self.ask(id, crate::outer::QUERIES);
        self.settle();
        Ok(id)
    }

    pub fn detach(&mut self, id: ClientId) {
        self.views.remove(&id);
        self.workspaces.iter_mut().for_each(|w| w.forget(id));
        self.settle();
    }

    /// Shows `client` workspace `ws`, one there is, and there the tab and
    /// pane `at` says, each if given and there.
    fn show(&mut self, client: ClientId, ws: WsId, at: Option<Spot>) {
        for w in &mut self.workspaces {
            if w.id != ws {
                w.hide(client);
            } else {
                w.viewers.insert(client);
                w.show(client, at);
            }
        }
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

    /// A client's terminal is `rows` by `cols`: its view, kept at most `MOST`
    /// each way, takes the size and settles if that changes it, and says
    /// whether it did.
    pub fn resize(&mut self, id: ClientId, rows: u16, cols: u16) -> bool {
        let size = (rows.clamp(1, MOST), cols.clamp(1, MOST));
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
        let own = here.client().and_then(|c| self.views.get(&c));
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
        let own = here.client().and_then(|c| self.views.get(&c));
        let client = own.or_else(|| self.views.values().next());
        fit.area(client.map(Self::pane_area)).unwrap_or(TYPICAL)
    }

    // -------------------------------------------------------------- repair

    /// Ends each view's mode if what it shows is gone, then sizes the PTYs:
    /// run after every event.
    pub fn settle(&mut self) {
        // Out of the session while they are repaired, which reads no view.
        let mut views = std::mem::take(&mut self.views);
        let ended: Vec<ClientId> = (views.values_mut())
            .filter_map(|view| self.repair(view).then_some(view.id))
            .collect();
        self.views = views;
        for client in ended {
            self.outbox.push(Outgoing::To(client, ToAttached::Ended));
        }
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
                self.outbox.push(Outgoing::To(view.id, ToAttached::Ended));
            }
        }
    }

    /// Ends what a view shows if what it is about is gone: copy mode whose
    /// pane closed or is no longer focused, a client's screen about a pane,
    /// tab or workspace that closed. Whether it did, which its client is
    /// told (`ToAttached::Ended`), the mode being the client's.
    fn repair(&self, view: &mut View) -> bool {
        use crate::act::{List, Panel};
        let focus = self.focused(view.id);
        // Copy mode's rows are looked for once the panes are sized
        // (`hold_copies`).
        let gone = match &view.mode {
            Mode::Copy(copy) if !self.panes.contains_key(&copy.pane) => {
                "copy mode ended: its pane closed".to_owned()
            }
            Mode::Copy(copy) if Some(copy.pane) != focus => {
                "copy mode ended: its pane is no longer focused".to_owned()
            }
            Mode::Panel(
                Panel::List(List {
                    about: Some(about), ..
                })
                | Panel::Prompt {
                    about: Some(about), ..
                }
                | Panel::Confirm { about, .. },
            ) if self.there(*about).is_err() => format!("closed: {} is gone", about),
            Mode::Normal | Mode::Panel(_) | Mode::Copy(_) => return false,
        };
        view.mode = Mode::Normal;
        view.error(gone);
        true
    }

    /// The settings changed: every pane keeps the history they say.
    fn configured(&mut self) {
        for pane in self.panes.values_mut() {
            pane.parser.set_history_bytes(self.config.history_bytes);
            pane.record.set_budget(self.config.journal_bytes);
        }
        self.keep_journals();
    }

    /// All panes' journals together keep at most `journal-total` bytes:
    /// the largest lets its oldest entries go first.
    fn keep_journals(&mut self) {
        let total = self.config.journal_total;
        let mut held: usize = self.panes.values().map(|p| p.record.held()).sum();
        while held > total {
            let Some(largest) = self.panes.values_mut().max_by_key(|p| p.record.held()) else {
                break;
            };
            let had = largest.record.held();
            largest
                .record
                .keep(had.saturating_sub(held.saturating_sub(total)));
            held = held.saturating_sub(had.saturating_sub(largest.record.held()));
            if largest.record.held() == had {
                break;
            }
        }
    }

    /// A PTY is the smallest rectangle any client shows it in. A tab no
    /// client shows whose layout changed is laid out on what its fit says,
    /// as commands resizing it are; otherwise its panes keep their size, so
    /// leaving a tab resizes nothing.
    fn size_panes(&mut self) {
        let mut sizes = std::mem::take(&mut self.shown_sizes);
        let mut placed = std::mem::take(&mut self.placed);
        sizes.clear();
        // A client that only watches sizes nothing: it shows the panes as
        // they are.
        for view in self.views.values().filter(|v| !v.read_only) {
            self.placement_into(view, &mut placed);
            sizes.extend(placed.panes.iter().map(|(p, r)| (*p, (r.h(), r.w()))));
        }
        // With no client, a tab that fits clients is laid out as a typical
        // terminal would show it (`tab_area`).
        let client = Some(self.views.values().next().map_or(TYPICAL, Self::pane_area));
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
    /// `closed`: fux closed it (`kill`, `kill-server`), not its program's
    /// end: the rest of its session goes too.
    fn end(&mut self, id: PaneId, process: Process, closed: bool) {
        if let Process::Reading(mut child) | Process::HungUp(mut child) = process {
            if closed {
                child.leader.closed();
            }
            child.leader.hang_up();
            let deadline = crate::after(Instant::now(), GRACE);
            self.dying.insert(id, Dying { child, deadline });
        }
    }

    /// Removes a pane and its process. A tab its closing empties closes too,
    /// as does a workspace left with no tabs; a tab emptied by moving its
    /// panes out stays.
    pub fn close_pane(&mut self, id: PaneId, why: Option<String>) {
        // Who sees the pane is known before its tab can go: a tab its
        // closing empties takes its viewers elsewhere.
        let viewers = self.viewers(self.locate(id).map(|(_, t)| t));
        let emptied = tab_of(&mut self.workspaces, id).ok().and_then(|t| {
            t.edit(|root| layout::remove(root, id));
            t.root().is_none().then_some(t.id)
        });
        if let Some(tab) = emptied {
            self.remove_tab(tab);
        }
        if let Some(pane) = self.panes.remove(&id) {
            // Closed for its program's end (a reason given), or by fux.
            let closed = why.is_none();
            if let Some(why) = why {
                for client in &viewers {
                    self.info_to(*client, format!("{id} {} {why}", pane.label()));
                }
            }
            self.end(id, pane.process, closed);
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
                if let Some(gone) = self.panes.remove(&pane) {
                    self.end(pane, gone.process, true);
                }
            });
        }
    }

    /// The tabs of workspace `ws`, in order.
    fn tab_ids(&self, ws: WsId) -> Vec<TabId> {
        let ws = self.workspace(ws).into_iter();
        ws.flat_map(|w| w.tabs().iter().map(|t| t.id)).collect()
    }

    /// The clients showing tab `tab`.
    fn viewers(&self, tab: Option<TabId>) -> Vec<ClientId> {
        let shows = |c: &ClientId| tab.is_some_and(|t| self.shows(*c, t));
        self.views.keys().copied().filter(shows).collect()
    }

    fn after_close(&mut self) {
        if self.panes.is_empty()
            && !self
                .outbox
                .iter()
                .any(|o| matches!(o, Outgoing::Shutdown { .. }))
        {
            self.outbox.push(Outgoing::Shutdown {
                reason: "the last pane closed".into(),
                keep: false,
            });
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
        let program = (self.panes.get(&id)).filter(|p| matches!(p.runs, Runs::Program(_)));
        let left = program.map(|p| unread(&p.process)).unwrap_or_default();
        if !left.is_empty() {
            self.output(id, &left);
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
                for client in self.viewers(self.locate(id).map(|(_, t)| t)) {
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
        self.panes.values().flat_map(|pane| {
            let typed = pane.input.due_at().map(|at| (at, Timer::Type(pane.id)));
            let frame = pane.frame_deadline().map(|at| (at, Timer::Frame(pane.id)));
            let paced = pane.pace_due().map(|at| (at, Timer::Pace(pane.id)));
            typed.into_iter().chain(frame).chain(paced)
        })
    }

    /// What `timer` waited for is done, now that it is due.
    pub fn fire(&mut self, timer: Timer) {
        match timer {
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
    /// does: its tab's host colours and palette given it first; after, its
    /// bell rung, and the views showing it repainted and told if a reply
    /// was dropped.
    fn read_with(&mut self, id: PaneId, read: impl FnOnce(&mut Pane)) {
        let tab = self.locate(id).map(|(_, t)| t);
        let (colours, palette, graphics) = self.host_for(tab);
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        pane.colours = colours;
        pane.parser.set_host_palette(palette);
        pane.parser.set_host_graphics(graphics);
        read(pane);
        pane.note_change(Instant::now());
        let dropped = pane.input.lost_reply();
        let bell = std::mem::take(&mut pane.bell);
        let copied = pane.copied.take();
        self.keep_journals();
        if bell {
            self.ring(id, Instant::now());
        }
        let viewers = self.viewers(tab);
        // A program's copy, as a copy in copy mode, to the clients showing
        // it; none with `set clipboard off`.
        if let Some(text) = copied.filter(|_| self.config.clipboard) {
            self.copied(text, &viewers);
        }
        for view in (self.views.values_mut()).filter(|v| viewers.contains(&v.id)) {
            view.dirty = true;
            if dropped {
                view.error(format!(
                    "{id} is not reading its input; a terminal reply was dropped"
                ));
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
                    self.outbox
                        .push(Outgoing::To(client, ToAttached::Paint(osc.clone().into())));
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
            self.end(pane.id, pane.process, true);
        }
    }

    // ------------------------------------------------------------ commands

    /// Begins a request from a client: one that waits gives its wait,
    /// which the server asks for its answer until it has one; a stream of
    /// events, its subscription; a grant, for the server to make; any other
    /// runs at once.
    pub fn begin(&mut self, request: Request, origin: &Origin) -> Begun {
        // A size a saved session asks for that cannot be had here is left
        // as it comes, so one does not keep the rest from coming back.
        if let Request::Do(command @ Command::ResizePane { .. }) = &request
            && self.starting_for(origin).restoring
        {
            let mut outcome = self.run_command(command, origin);
            if matches!(outcome.end, End::Failed { .. }) {
                outcome = Reply::default();
            }
            return Begun::Done(outcome);
        }
        self.begin_request(request, origin)
            .unwrap_or_else(|error| Begun::Done(failure(error, origin)))
    }

    /// `begin`'s work: what fails before a request runs is its error.
    fn begin_request(&mut self, request: Request, origin: &Origin) -> Result<Begun, Error> {
        let request = match self.scope(origin)? {
            Some(scope) => self.scoped(request, &scope, origin)?,
            None => request,
        };
        let now = Instant::now();
        let (target, wait, action) = match request {
            Request::Do(command) => return Ok(Begun::Done(self.run_command(&command, origin))),
            Request::Then(command, wait) => match command.armed() {
                Some(Armed::Before(target)) => (target, *wait, Some(command)),
                // Started, then waited for from its first output on.
                Some(Armed::Started) => return Ok(self.start_waiting(&command, *wait, origin)),
                None => return Err(fail(Code::Unused, "this command takes no wait")),
            },
            Request::Wait(target, wait) => (target, wait, None),
            // Read once the pane settles, said with its state.
            Request::Capture(target, history, view, timeout) => {
                let wait = crate::command::Wait {
                    until: vec![crate::command::Until::Settled],
                    timeout,
                    print: false,
                    after: false,
                    marks: false,
                };
                let pane = self.pane(target)?;
                let follows = Follows::Capture(history, view);
                let waiting = Waiting::new(&[pane], wait, follows, now, self.config.wait);
                return Ok(self.wait_on(waiting, now));
            }
            Request::Events(target, json) => {
                let target = target.map(|t| self.there(t)).transpose()?;
                return Ok(Begun::Events(Box::new(crate::events::Subscription {
                    target,
                    json,
                    seen: crate::events::Snapshot::of(self),
                })));
            }
            Request::Grant(workspace, socket) => return Ok(Begun::Grant(socket, Some(workspace))),
            Request::Revoke(socket) => return Ok(Begun::Grant(socket, None)),
        };
        let panes = (self.panes_of(target)?.into_iter())
            .map(|id| self.pane(id))
            .collect::<Result<Vec<_>, _>>()?;
        // Armed before any keys go in: what the keys bring is what it sees.
        let follows = action.as_ref().map_or(Follows::Wait, |_| Follows::Action);
        let waiting = Waiting::new(&panes, wait, follows, now, self.config.wait);
        if let Some(command) = action {
            let outcome = self.run_command(&command, origin);
            if matches!(outcome.end, End::Failed { .. }) {
                return Ok(Begun::Done(outcome));
            }
        }
        Ok(self.wait_on(waiting, now))
    }

    /// A wait's answer, if it has one already; else the wait.
    fn wait_on(&self, mut waiting: Waiting, now: Instant) -> Begun {
        match waiting.answer(self, now) {
            Some(outcome) => Begun::Done(outcome),
            None => Begun::Waiting(Box::new(waiting)),
        }
    }

    /// A request from a granted socket, if it may run: with no target, one
    /// that would act anywhere (`ls`, `run`, `new-tab`, `events`) acts on
    /// the granted workspace; one that names, or comes from, anything
    /// outside it, or reaches what is everyone's (the config, the buffers,
    /// clients, the server), is refused.
    fn scoped(&self, request: Request, scope: &str, origin: &Origin) -> Result<Request, Error> {
        // With no target, one that would act anywhere acts on the granted
        // workspace.
        let mut request = request;
        let ws = self.resolve_ws(&WsRef::Name(scope.to_owned())).ok();
        let place = || Some(WsRef::Name(scope.to_owned()));
        // The granted workspace, which may be gone: then nothing.
        let whole = || {
            let gone = || {
                fail(
                    Code::NotGranted,
                    format!("workspace {scope} (grant) is gone"),
                )
            };
            ws.map(AnyRef::Workspace).ok_or_else(gone)
        };
        if let Request::Do(command) | Request::Then(command, _) = &mut request
            && let Command::Run {
                workspace: at @ None,
                ..
            }
            | Command::NewTab {
                target: at @ None, ..
            } = command
        {
            *at = place();
        }
        if let Request::Do(Command::DumpLayout { target: at @ None })
        | Request::Events(at @ None, _) = &mut request
        {
            *at = Some(whole()?);
        }
        let reach = Reach {
            session: self,
            ws,
            scope,
            here: self.here(origin)?,
        };
        reach.request(&request)?;
        Ok(request)
    }

    /// Runs a command that starts a program and prints its pane's id, then
    /// waits on that pane, armed before the server reads anything the
    /// program writes; the id comes first in what the wait prints.
    fn start_waiting(&mut self, command: &Command, wait: command::Wait, origin: &Origin) -> Begun {
        let outcome = self.run_command(command, origin);
        let pane = outcome.ids.iter().find_map(|id| match id {
            AnyRef::Pane(p) => Some(*p),
            AnyRef::Tab(_) | AnyRef::Workspace(_) => None,
        });
        let Some(pane) = pane
            .filter(|_| outcome.end == End::Done)
            .and_then(|p| self.panes.get(&p))
        else {
            return Begun::Done(outcome);
        };
        let now = Instant::now();
        let waiting = Waiting::new(&[pane], wait, Follows::Started, now, self.config.wait);
        self.wait_on(waiting, now)
    }

    /// Runs a command, parsed already: from a line, a binding or a menu.
    pub fn run_command(&mut self, command: &Command, origin: &Origin) -> Reply {
        let outcome = self
            .execute(command, origin)
            .unwrap_or_else(|error| failure(error, origin));
        self.ids.unwant();
        if !command.reaches().shows_nothing_new() {
            self.changes = self.changes.wrapping_add(1);
            self.touch();
        }
        // Input and resizes grow journals too.
        self.keep_journals();
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
            Command::Ls { target, get } => {
                // Through a grant, the granted workspace alone.
                let scope = self.scope(origin)?;
                let scope = (scope.map(|name| self.resolve_ws(&WsRef::Name(name)))).transpose()?;
                // Its JSON, given as asked, or kept beside its text.
                let (read, only) = match target {
                    Some(target) => {
                        let target = self.there(*target)?;
                        let read = self.ls_one(target, get.as_deref())?;
                        if get.is_some() {
                            return Ok(read);
                        }
                        (read.text, Some(target))
                    }
                    None => (self.ls_json(scope), scope.map(AnyRef::Workspace)),
                };
                // A reading given as text keeps its JSON, made from the same
                // read: `json` converts what was read, never a later read.
                Ok(Reply {
                    json: read,
                    ..self.ls_text(only).into()
                })
            }
            &Command::KillServer { force } => {
                if let (Origin::Cli(caller), false) = (origin, force) {
                    let pane = caller.pane().filter(|p| self.panes.contains_key(p));
                    let inside = match caller.within {
                        Within::This => true,
                        Within::Unsaid | Within::Other => false,
                    };
                    if pane.is_some() || inside {
                        let pane = pane.map_or_else(String::new, |p| format!(" {p},"));
                        let message = format!(
                            "this runs in{pane} a pane of the server it would stop, with everything \
                             else in it; force-kill-server stops it anyway"
                        );
                        return Err(fail(Code::OwnServer, message));
                    }
                    let people = (self.views.values())
                        .filter(|v| v.kind == ClientKind::Terminal)
                        .count();
                    if people > 0 {
                        let who = match people {
                            1 => "a person is".to_owned(),
                            n => format!("{n} people are"),
                        };
                        let message = format!(
                            "{who} attached to this server; force-kill-server stops it anyway"
                        );
                        return Err(fail(Code::PeopleAttached, message));
                    }
                }
                let reason = match origin {
                    Origin::Client(_) => "stopped by kill-server, from a key or the prompt",
                    Origin::Cli(_) => "stopped by fux kill-server",
                };
                self.outbox.push(Outgoing::Shutdown {
                    reason: reason.into(),
                    keep: false,
                });
                Ok(Reply::default())
            }
            Command::NewWorkspace {
                name,
                cmd,
                size,
                cwd,
                env,
                ids,
            } => {
                self.want(*ids, starting.restoring)?;
                let cwd = self.start_dir(here, cwd.as_deref(), None)?;
                let fit = size.map_or(Fit::Clients, Fit::Fixed);
                let start = (Start::Shell(cmd, env.as_slice()), cwd.as_path(), &starting);
                let place = (fit, self.start_size(fit, here));
                let made = self.create_workspace((name.clone(), None), start, place)?;
                self.follow(here, made.0, None);
                Ok(Self::made_ids(made, Kind::Workspace))
            }
            Command::NewTab {
                target,
                name,
                cmd,
                size,
                cwd,
                env,
                ids,
            } => {
                self.want(*ids, starting.restoring)?;
                if let Some(name) = name {
                    self.check_tab_name(name, None)?;
                }
                let cwd = self.start_dir(here, cwd.as_deref(), None)?;
                let fit = size.map_or(Fit::Clients, Fit::Fixed);
                let start = (Start::Shell(cmd, env.as_slice()), cwd.as_path(), &starting);
                let place = (fit, self.start_size(fit, here));
                // A workspace named that is not there is made, as `run`
                // makes one, its first tab this one.
                let ws = (self.ws_target(target.as_ref(), here), target.as_ref());
                let (ws, tab, pane) = self.create_tab(ws, None, start, place)?;
                if let Some(name) = name {
                    self.give_tab_name(tab, name.clone())?;
                }
                self.follow(here, ws, Some((tab, None)));
                Ok(Self::made_ids((ws, tab, pane), Kind::Tab))
            }
            Command::Run {
                workspace,
                name,
                size,
                cwd,
                env,
                argv,
                log,
                ids,
            } => {
                self.want(*ids, starting.restoring)?;
                let cwd = self.start_dir(here, cwd.as_deref(), None)?;
                let fit = size.map_or(Fit::Clients, Fit::Fixed);
                // Its place is out of every client's way: no client follows.
                let place = (fit, self.start_size(fit, Here::NOWHERE));
                // A name given is one `str NAME tab` finds: it must be the only one.
                if let Some(name) = name {
                    self.check_tab_name(name, None)?;
                }
                // Named after its program, if that makes a name.
                let derived = Some(crate::pane::named(argv)).filter(|n| check_name(n).is_ok());
                let given = name.clone();
                let name = name.clone().or(derived);
                let start = (
                    Start::Program(argv, env.as_slice()),
                    cwd.as_path(),
                    &starting,
                );
                let workspace = workspace
                    .clone()
                    .unwrap_or(WsRef::Name(RUN_WORKSPACE.into()));
                let ws = (self.resolve_ws(&workspace), Some(&workspace));
                // Opened, and so emptied, once nothing else refuses the run.
                let log = log
                    .as_ref()
                    .map(|p| open_log(&self.cwd_for(here, None).join(p)))
                    .transpose()?;
                let made = self.create_tab(ws, name, start, place)?;
                if let Some(given) = given {
                    self.give_tab_name(made.1, given)?;
                }
                // Before any of its output is read: it all goes there.
                if let Some(pane) = self.panes.get_mut(&made.2) {
                    pane.log = log;
                }
                Ok(Self::made_ids(made, Kind::Pane))
            }
            &Command::Respawn {
                target,
                kill,
                ref env,
                ref argv,
            } => {
                let id = self.pane(target)?.id;
                self.respawn(id, kill, (env.as_slice(), argv), &starting)?;
                Ok(Reply {
                    ids: vec![AnyRef::Pane(id)],
                    ..format!("{id}\n").into()
                })
            }
            Command::Notify { target, text } => {
                let to: Vec<ClientId> = match target {
                    None => self.views.keys().copied().collect(),
                    Some(target) => match self.there(*target)? {
                        AnyRef::Pane(p) => self.viewers(self.locate(p).map(|(_, t)| t)),
                        AnyRef::Tab(t) => self.viewers(Some(t)),
                        AnyRef::Workspace(w) => self
                            .workspace(w)
                            .map(|w| w.viewers.iter().copied().collect())
                            .unwrap_or_default(),
                    },
                };
                for client in to {
                    self.info_to(client, text.clone());
                }
                Ok(Reply::default())
            }
            Command::Resize(what) => {
                match what {
                    Resized::Tabs(target, size) => {
                        // A size no pane could have is refused, not left unmet.
                        if let Some(size) = size
                            && !fux_vt::fits(*size)
                        {
                            let message =
                                format!("{}x{} is too big for a pane", size.cols(), size.rows());
                            return Err(fail(Code::Size, message));
                        }
                        let fit = size.map_or(Fit::Clients, Fit::Fixed);
                        let target = self.there(*target)?;
                        let tabs: Vec<TabId> = match target {
                            AnyRef::Tab(tab) => vec![tab],
                            AnyRef::Workspace(ws) => self.tab_ids(ws),
                            AnyRef::Pane(pane) => {
                                self.locate(pane).map(|(_, tab)| tab).into_iter().collect()
                            }
                        };
                        // A size that would hide one of a tab's panes is refused,
                        // as a split that would is.
                        if size.is_some() {
                            let area = self.tab_area(fit, here);
                            for tab in &tabs {
                                let Some(root) = self.root(*tab) else {
                                    continue;
                                };
                                let placed = layout::place(root, area);
                                if root.panes().iter().any(|p| placed.rect(*p).is_none()) {
                                    let message = format!(
                                        "{tab} cannot show its {} panes at that size; close some, or choose a bigger one",
                                        root.panes().len()
                                    );
                                    return Err(fail(Code::Size, message));
                                }
                            }
                        }
                        for tab in tabs_mut(&mut self.workspaces).filter(|t| tabs.contains(&t.id)) {
                            tab.set_fit(fit);
                        }
                    }
                    &Resized::Client(client, size) => {
                        let view = self.views.get(&client).ok_or(Error::NoClient(client))?;
                        if view.kind != ClientKind::Headless {
                            let message = format!(
                                "{client} has a terminal, which sets its size; resize sizes headless clients"
                            );
                            return Err(fail(Code::NotHeadless, message));
                        }
                        self.resize(client, size.rows(), size.cols());
                    }
                }
                Ok(Reply::default())
            }
            Command::DumpLayout { target } => {
                let only = match target {
                    Some(target) => Some(self.there(*target)?),
                    None => None,
                };
                Ok(self.dump_layout(only, false).into())
            }
            &Command::SendMouse {
                target,
                mouse,
                row,
                col,
            } => {
                let p = self.live_pane_mut(target)?;
                let size = p.size();
                if row >= size.rows() || col >= size.cols() {
                    let (rows, cols) = (size.rows(), size.cols());
                    let message = format!(
                        "{} is {cols}x{rows}: rows count 0-{} and columns 0-{}",
                        p.id,
                        rows.saturating_sub(1),
                        cols.saturating_sub(1)
                    );
                    return Err(fail(Code::Mouse, message));
                }
                let mut report = Vec::new();
                let event = |action, button| fux_vt::keys::mouse::MouseEvent {
                    action,
                    button,
                    mods: fux_vt::keys::Modifiers::default(),
                    row,
                    col,
                };
                use fux_vt::keys::mouse::MouseAction::{Motion, Press, Release};
                // A click is a press and, where the program takes them (not
                // in X10 mode), a release; a move while a button is held is
                // a drag.
                // Released, or clicked, it is held no more; another button's
                // release leaves it held.
                let unheld = |button| p.mouse_held.filter(|held| *held != button);
                let (events, held) = match mouse {
                    Mouse::Click(button) => {
                        let button = button.reported();
                        let events = vec![event(Press, Some(button)), event(Release, Some(button))];
                        (events, unheld(button))
                    }
                    Mouse::Press(button) => {
                        let button = button.reported();
                        (vec![event(Press, Some(button))], Some(button))
                    }
                    Mouse::Wheel(way) => (vec![event(Press, Some(way.reported()))], p.mouse_held),
                    Mouse::Release(button) => {
                        let button = button.reported();
                        (vec![event(Release, Some(button))], unheld(button))
                    }
                    Mouse::Move => (vec![event(Motion, p.mouse_held)], p.mouse_held),
                };
                let mode = p.screen().mouse_protocol_mode();
                let x10 = mode == fux_vt::MouseProtocolMode::Press;
                // X10 reports presses alone: a click is its press, and a
                // release or the wheel, which it cannot say, is refused.
                if x10 && matches!(mouse, Mouse::Wheel(_) | Mouse::Release(_)) {
                    let message = format!(
                        "the program in {} asked for X10 mouse reports: presses alone, no releases or wheel",
                        p.id
                    );
                    return Err(fail(Code::Mouse, message));
                }
                for event in events {
                    let release = event.action == Release;
                    if !p.screen().encode_mouse(event, &mut report) && !(release && x10) {
                        let pane = p.id;
                        let message = match (event.action, mode) {
                            (_, fux_vt::MouseProtocolMode::None) => {
                                format!("the program in {pane} has not asked for mouse reports")
                            }
                            (Motion, _) => format!(
                                "the program in {pane} takes no motion here: with no button pressed it asked for none, \
                                 and asking for clicks alone (mode 1000) it takes no drags either"
                            ),
                            _ => format!(
                                "the mouse encoding the program in {pane} asked for cannot say row {row}, column {col}"
                            ),
                        };
                        return Err(fail(Code::Mouse, message));
                    }
                }
                p.note_input();
                p.send(vec![(report, crate::log::Unit::Submit)], Instant::now())?;
                p.mouse_held = held;
                Ok(Reply::default())
            }
            &Command::Split {
                axis,
                target,
                ref cmd,
                ref cwd,
                ref env,
                run,
                id,
            } => {
                let wanted = Wanted {
                    pane: id,
                    ..Wanted::default()
                };
                self.want(wanted, starting.restoring)?;
                let target = self.pane(target)?.id;
                let cwd = self.start_dir(here, cwd.as_deref(), Some(target))?;
                // The splitting client follows the new pane; from the CLI, the
                // clients that focus the split one.
                let shown = |c: &ClientId| self.focused(*c) == Some(target);
                let followers: Vec<ClientId> = match here {
                    Here::Client(client, _) => vec![client],
                    Here::Cli(..) => self.views.keys().copied().filter(shown).collect(),
                };
                let size = self.split_size(target, axis, here)?;
                // Its tab found before the program starts, which nothing
                // can then leave running.
                tab_of(&mut self.workspaces, target)?;
                let mut ids = self.ids;
                let start = match run {
                    true => Start::Program(cmd, env.as_slice()),
                    false => Start::Shell(cmd, env.as_slice()),
                };
                let pane = new_pane(self, &mut ids, (start, &cwd, &starting), size)?;
                let tab = tab_of(&mut self.workspaces, target)?;
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
                Ok(Self::made_ids((at.0, at.1, new), Kind::Pane))
            }
            &Command::Signal { target, signal } => {
                let group = self.signalled(target)?;
                fuxix::process::kill_group(group, signal)
                    .map_err(|e| fail(Code::Signal, format!("sending the signal: {e}")))?;
                Ok(Reply::default())
            }
            // From the command line each says what it closed, so a script
            // or an agent knows; a key's close shows on the screen anyway.
            Command::Kill { target } => {
                // What may go with it, said once gone: a pane's tab, if it
                // was its last, and that tab's workspace; a tab's or a
                // workspace's panes.
                let mut may_go = vec![self.there(*target)?];
                // A tab there with no panes (moved out) has none to go.
                if let AnyRef::Tab(_) | AnyRef::Workspace(_) = target {
                    let panes = self.panes_of(*target).unwrap_or_default();
                    may_go.extend(panes.into_iter().map(AnyRef::Pane));
                }
                match *target {
                    AnyRef::Pane(pane) => {
                        let place = self.locate(pane).into_iter();
                        may_go.extend(
                            place.flat_map(|(w, t)| [AnyRef::Tab(t), AnyRef::Workspace(w)]),
                        );
                        self.close_pane(pane, None);
                    }
                    AnyRef::Tab(tab) => self.remove_tab(tab),
                    AnyRef::Workspace(ws) => {
                        self.tab_ids(ws)
                            .into_iter()
                            .for_each(|tab| self.remove_tab(tab));
                    }
                }
                if let AnyRef::Tab(_) | AnyRef::Workspace(_) = target {
                    self.after_close();
                }
                let gone: Vec<AnyRef> = (may_go.into_iter())
                    .filter(|it| self.there(*it).is_err())
                    .collect();
                Ok(closed(origin, &gone).into())
            }
            Command::Rename { target, name } => {
                let target = self.there(*target)?;
                self.rename(target, name.clone()).map(|()| Reply::default())
            }
            &Command::MovePane { target, ref to } => {
                self.move_pane(target, to, here).map(Reply::from)
            }
            &Command::SwapPane { target, with } => {
                let source = self.pane(target)?.id;
                let other = match with {
                    SwapWith::Pane(p) => self.pane(p)?.id,
                    SwapWith::Toward(direction) => self.neighbor(source, direction, here)?,
                };
                self.swap(source, other);
                Ok(Reply::default())
            }
            &Command::ResizePane { target, by } => {
                let pane = self.pane(target)?.id;
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
                            Ok(Reply::default())
                        } else {
                            let message =
                                format!("{pane} has no border to move {}", direction.name());
                            Err(fail(Code::NothingThere, message))
                        }
                    }
                    PaneResize::To(axis, extent) => self
                        .resize_pane_to(pane, area, axis, extent)
                        .map(Reply::from),
                }
            }
            Command::Log { target, file } => {
                self.pane(*target)?;
                let log = match file {
                    Some(file) => Some(open_log(&self.cwd_for(here, None).join(file))?),
                    None => None,
                };
                self.pane_mut(*target)?.log = log;
                Ok(Reply::default())
            }
            &Command::ClearHistory { target } => {
                self.pane_mut(target)?.parser.clear_history();
                Ok(Reply::default())
            }
            &Command::SendKeys { target, ref inputs } => {
                // Every pane must take them, or none is sent them: each
                // one's, as its program takes keys, are made and fit first.
                let mut each = Vec::new();
                for id in self.panes_of(target)? {
                    let pane = self.live_pane_mut(id)?;
                    let units = units(pane, inputs);
                    pane.room_for(&units)?;
                    each.push((id, units));
                }
                for (id, units) in each {
                    let pane = self.pane_mut(id)?;
                    pane.note_input();
                    pane.send(units, Instant::now())?;
                }
                Ok(Reply::default())
            }
            &Command::CapturePane {
                target,
                history,
                view,
            } => {
                let pane = self.pane(target)?;
                let copying = self.copying(pane.id);
                let (text, form) = crate::capture::pane(pane, history, view, true, copying);
                Ok(Reply {
                    json: form.unwrap_or_default(),
                    ..text.into()
                })
            }
            &Command::Reorder { ref target, toward } => {
                let target = self.there(*target)?;
                self.reorder(target, toward).map(|()| Reply::default())
            }
            Command::Set { option, value } => {
                // `run_command` marks every view to paint, as after any
                // command that can show something new: `titles` shows at
                // each client's next paint.
                self.config
                    .set(option, value)
                    .map_err(|m| fail(Code::Config, m))?;
                self.configured();
                Ok(Reply::default())
            }
            Command::Resolve { text, kind } => {
                let found = self.resolve(text, *kind, here)?;
                Ok(Reply {
                    ids: vec![found],
                    ..format!("{}\n", found).into()
                })
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
                .collect::<String>()
                .into()),
            &Command::ShowBuffer { index } => self
                .buffers
                .get(index)
                .cloned()
                .map(Reply::from)
                .ok_or(Error::NoBuffer(index)),
            &Command::PasteBuffer { index, target } => {
                let text = self.buffers.get(index).cloned();
                let paste = Input::Paste(text.ok_or(Error::NoBuffer(index))?);
                self.send_inputs(target, &[paste])
                    .map(|()| Reply::default())
            }
            &Command::Client { client, ref action } => {
                // The client named (`str c1 client`), else the one the command came from.
                let (client, place) = match (client, here) {
                    (Some(c), _) => (c, self.place(c).ok_or(Error::NoClient(c))?),
                    (None, Here::Client(client, place)) => (client, place),
                    (None, Here::Cli(..)) => {
                        return Err(fail(
                            Code::NoTarget,
                            "this acts on a client's screen: name one (`str c1 client`; `fux ls` lists them)",
                        ));
                    }
                };
                // A command on a client's screen. Targets it leaves out are
                // the client's own.
                let view = || self.views.get(&client).ok_or(Error::NoClient(client));
                let picked = match *action {
                    ClientAction::Choices(choices) => {
                        return crate::overlay::choices(self, client, choices);
                    }
                    ClientAction::Run(ref code) => {
                        self.outbox
                            .push(Outgoing::To(client, ToAttached::Run(code.clone())));
                        return Ok(Reply::default());
                    }
                    // Taken as typed at its terminal: its bindings see them.
                    ClientAction::Keys(ref inputs) => {
                        self.outbox
                            .push(Outgoing::To(client, ToAttached::Keys(inputs.clone())));
                        return Ok(Reply::default());
                    }
                    ClientAction::Detach => {
                        self.detach(client);
                        self.outbox
                            .push(Outgoing::To(client, ToAttached::Exit("detached")));
                        return Ok(Reply::default());
                    }
                    ClientAction::Capture => {
                        let mut grid = crate::render::Grid::new(0, 0);
                        let placement = &mut Placement::default();
                        crate::render::compose_view(self, view()?, &mut grid, placement);
                        let (text, form) = crate::capture::client(client, &grid);
                        return Ok(Reply {
                            json: form.unwrap_or_default(),
                            ..text.into()
                        });
                    }
                    ClientAction::Zoom => None,
                    ClientAction::SelectPane(pick) => Some(self.pick_pane(view()?, place, pick)?),
                    ClientAction::SelectTab(pick) => Some(self.pick_tab(place, pick)?),
                    ClientAction::SelectWorkspace(ref pick) => Some(self.pick_ws(view()?, pick)?),
                };
                // A select shows the client where it picked, unzoomed.
                match picked {
                    Some((ws, at)) => self.follow(Here::Client(client, place), ws, at),
                    None => (self.views.get_mut(&client))
                        .into_iter()
                        .for_each(|v| v.zoom = !v.zoom),
                }
                Ok(Reply::default())
            }
        }
    }

    fn rename(&mut self, target: AnyRef, name: String) -> Result<(), Error> {
        check_name(&name)?;
        match target {
            AnyRef::Pane(p) => {
                self.pane_mut(p)?.name = name;
            }
            AnyRef::Tab(t) => {
                self.check_tab_name(&name, Some(t))?;
                self.give_tab_name(t, name)?;
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

    fn move_pane(&mut self, target: PaneId, to: &MoveTo, here: Here) -> Result<String, Error> {
        let pane = self.pane(target)?.id;
        let (ws, tab) = match to {
            &MoveTo::Beside(direction) => {
                let destination = self.neighbor(pane, direction, here)?;
                tab_of(&mut self.workspaces, pane)?
                    .edit(|root| layout::move_beside(root, pane, destination, direction));
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
                let first = (tab, MAIN.into());
                let layout = (None, Fit::Clients);
                (self.workspaces).push(Workspace::new(id, name, first, layout, clients));
                (id, tab)
            }
        };
        if self.locate(pane).is_some_and(|(_, source)| source == tab) {
            return Ok(String::new());
        }
        self.relocate(pane, tab);
        // The moving client follows its pane.
        self.follow(here, ws, Some((tab, Some(pane))));
        Ok(format!("{tab}\n"))
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

    /// The pane `focus` picks for client `view`, which is at `at`.
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
            PanePick::Last => {
                last.ok_or_else(|| fail(Code::NoPane, "no previously focused pane"))?
            }
            PanePick::Toward(direction) => {
                let (root, from) = tab
                    .root()
                    .zip(current)
                    .ok_or_else(|| fail(Code::NoPane, "the tab is empty"))?;
                let placement = layout::place(root, Self::pane_area(view));
                layout::neighbor(&placement, from, direction)
                    .ok_or(Error::NoNeighbor { from, direction })?
            }
        };
        let (ws, tab) = self.locate(target).ok_or(Error::NoPane(target))?;
        Ok((ws, Some((tab, Some(target)))))
    }

    /// The tab `show-tab` picks for a client at `at`.
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
        Ok((ws, Some((target, None))))
    }

    /// The workspace `show-workspace` picks for client `view`.
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
        Ok((ws, None))
    }

    // ------------------------------------------------------------------ ls

    /// `ls`: everything, or with `only` that pane, tab or workspace and
    /// what is in it, a line each.
    fn ls_text(&self, only: Option<AnyRef>) -> String {
        let mut out = String::new();
        let listed_ws = |ws: &&Workspace| match only {
            Some(AnyRef::Workspace(w)) => w == ws.id,
            None | Some(AnyRef::Tab(_) | AnyRef::Pane(_)) => true,
        };
        let listed_tab = |tab: &&Tab| match only {
            Some(AnyRef::Tab(t)) => t == tab.id,
            Some(AnyRef::Pane(p)) => self.locate(p).is_some_and(|(_, t)| t == tab.id),
            None | Some(AnyRef::Workspace(_)) => true,
        };
        let listed_pane = |pane: PaneId| !matches!(only, Some(AnyRef::Pane(p)) if p != pane);
        for ws in self.workspaces.iter().filter(listed_ws) {
            if matches!(only, None | Some(AnyRef::Workspace(_))) {
                out.push_str(&format!("{} {}\n", ws.id, ws.name));
            }
            for tab in ws.tabs().iter().filter(listed_tab) {
                out.push_str(&format!(
                    "  {} {}{}\n",
                    tab.id,
                    tab.name,
                    if tab.root().is_none() { " (empty)" } else { "" }
                ));
                let Some(root) = tab.root() else { continue };
                root.for_each_pane(&mut |pane| {
                    if !listed_pane(pane) {
                        return;
                    }
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
        for grant in self.grants.iter().filter(|_| only.is_none()) {
            let (socket, ws) = (grant.socket.display(), &grant.workspace);
            out.push_str(&format!("grant {socket} {ws}\n"));
        }
        let shown = |id: Option<String>| id.unwrap_or_else(|| "-".into());
        for view in self.views.values().filter(|_| only.is_none()) {
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

    /// A pane's state, as `ls json` has it.
    fn pane_json<'a>(&'a self, p: &'a Pane, now: Instant) -> Fields<'a> {
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
        fields
    }

    /// A tab and its panes, as `ls json` has them.
    fn tab_json<'a>(&'a self, t: &'a Tab, now: Instant) -> Fields<'a> {
        let panes = (t.root().map(Tree::panes).unwrap_or_default().into_iter())
            .filter_map(|id| self.panes.get(&id))
            .map(|p| Json::Object(self.pane_json(p, now)))
            .collect();
        vec![
            ("id", Json::str(t.id.to_string())),
            ("name", Json::str(t.name.as_str())),
            ("panes", Json::Array(panes)),
        ]
    }

    /// A workspace, its tabs and their panes, as `ls json` has them.
    fn ws_json<'a>(&'a self, ws: &'a Workspace, now: Instant) -> Fields<'a> {
        let tabs = ws
            .tabs()
            .iter()
            .map(|t| Json::Object(self.tab_json(t, now)));
        vec![
            ("id", Json::str(ws.id.to_string())),
            ("name", Json::str(ws.name.as_str())),
            ("tabs", Json::Array(tabs.collect())),
        ]
    }

    /// `info` and `get`: one pane, tab or workspace, as JSON, or one field
    /// of its JSON (`get`), a string's text as it is.
    fn ls_one(&mut self, target: AnyRef, get: Option<&str>) -> Result<Reply, Error> {
        let now = Instant::now();
        let mut fields = match self.there(target)? {
            AnyRef::Pane(id) => self.panes.get(&id).map(|p| self.pane_json(p, now)),
            AnyRef::Tab(id) => self.tab(id).map(|t| self.tab_json(t, now)),
            AnyRef::Workspace(id) => self.workspace(id).map(|w| self.ws_json(w, now)),
        }
        .unwrap_or_default();
        let Some(field) = get else {
            return Ok(format!("{}\n", Json::Object(fields).render()).into());
        };
        // A tab of one pane (`run`'s) stands for its pane, whose
        // fields it gives too.
        let single = match target {
            AnyRef::Tab(id) => self.root(id).map(Tree::panes).filter(|p| p.len() == 1),
            AnyRef::Pane(_) | AnyRef::Workspace(_) => None,
        };
        let pane = single.and_then(|p| p.first().and_then(|id| self.panes.get(id)));
        if let Some(more) = pane.map(|p| self.pane_json(p, now)) {
            fields.extend(
                more.into_iter()
                    .filter(|(name, _)| !["id", "name"].contains(name)),
            );
        }
        let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
        let value = fields.into_iter().find(|(name, _)| *name == field);
        use crate::protocol::Scalar;
        let (scalar, text) = match value {
            Some((_, Json::String(text))) => {
                (Some(Scalar::Str(text.to_string())), format!("{text}\n"))
            }
            Some((_, value)) => {
                let scalar = match &value {
                    Json::Number(n) => Some(Scalar::Int(*n)),
                    Json::Bool(b) => Some(Scalar::Bool(*b)),
                    Json::Null
                    | Json::String(_)
                    | Json::Array(_)
                    | Json::Object(_)
                    | Json::Raw(_) => None,
                };
                (scalar, format!("{}\n", value.render()))
            }
            None => {
                let (target, fields) = (target, names.join(", "));
                let message = format!("{target} has no field {field:?}; it has {fields}");
                return Err(fail(Code::NoField, message));
            }
        };
        drop(names);
        Ok(Reply {
            value: scalar,
            ..text.into()
        })
    }

    /// `ls json`: everything, or `only` that workspace, with no clients
    /// or grants (what a granted socket sees).
    fn ls_json(&self, only: Option<WsId>) -> String {
        let now = Instant::now();
        let workspaces = (self.workspaces.iter())
            .filter(|ws| only.is_none_or(|id| id == ws.id))
            .map(|ws| Json::Object(self.ws_json(ws, now)))
            .collect();
        let opt = |v: Option<String>| v.map_or(Json::Null, Json::str);
        let clients = self
            .views
            .values()
            .filter(|_| only.is_none())
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
        let grants = (self.grants.iter())
            .filter(|_| only.is_none())
            .map(|grant| {
                Json::Object(vec![
                    ("socket", Json::str(grant.socket.to_string_lossy())),
                    ("workspace", Json::str(grant.workspace.as_str())),
                ])
            })
            .collect();
        let mut text = Json::Object(vec![
            ("workspaces", Json::Array(workspaces)),
            ("clients", Json::Array(clients)),
            ("grants", Json::Array(grants)),
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

/// A socket `fux grant` made: its commands reach `workspace` alone, the
/// workspace of that name it was given, or the first that has it once
/// there is one, whatever it is renamed after.
pub struct Grant {
    pub socket: PathBuf,
    pub workspace: String,
    pinned: Option<WsId>,
}

impl Session {
    /// The workspace's name a caller from `origin` may reach, and nothing
    /// else, if it came through a socket `fux grant` made: the one pinned,
    /// while it is there, else the name it was granted, pinned once a
    /// workspace has it. Through a grant revoked, nothing.
    fn scope(&mut self, origin: &Origin) -> Result<Option<String>, Error> {
        let Origin::Cli(Caller {
            came: Came::Grant(socket),
            ..
        }) = origin
        else {
            return Ok(None);
        };
        let Some(at) = self.grants.iter().position(|g| g.socket == *socket) else {
            let message = "the grant this socket came through was revoked";
            return Err(fail(Code::NotGranted, message));
        };
        let pinned = self.grants.get(at).and_then(|g| g.pinned);
        if let Some(ws) = pinned.and_then(|id| self.workspace(id)) {
            return Ok(Some(ws.name.clone()));
        }
        let name = self
            .grants
            .get(at)
            .map(|g| g.workspace.clone())
            .unwrap_or_default();
        let found = self.resolve_ws(&WsRef::Name(name.clone())).ok();
        if let Some(grant) = self.grants.get_mut(at) {
            grant.pinned = found;
        }
        Ok(Some(name))
    }

    /// The workspace's name a grant of `workspace` through `socket` would
    /// reach (by its id, there; by its name, maybe later, as `run` makes
    /// the one it names), or why there can be none.
    pub fn grantable(&self, socket: &Path, workspace: &WsRef) -> Result<String, Error> {
        if self.grants.iter().any(|g| g.socket == socket) {
            let message = format!("a grant made {} already", socket.display());
            return Err(fail(Code::Grant, message));
        }
        Ok(match workspace {
            WsRef::Name(name) => name.clone(),
            WsRef::Id(_) => {
                let id = self.resolve_ws(workspace)?;
                self.workspace(id)
                    .map_or_else(String::new, |w| w.name.clone())
            }
        })
    }

    /// Grant `socket`, which reaches workspace `workspace` alone.
    pub fn grant(&mut self, socket: PathBuf, workspace: String) {
        let pinned = self.resolve_ws(&WsRef::Name(workspace.clone())).ok();
        self.grants.push(Grant {
            socket,
            workspace,
            pinned,
        });
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

/// Shells a pane may run, taking commands typed at them.
const SHELLS: &[&str] = &[
    "bash", "zsh", "fish", "sh", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "elvish", "xonsh",
];

/// What a close from the command line prints: `closed` and the ids gone.
fn closed(origin: &Origin, ids: &[AnyRef]) -> String {
    match origin {
        Origin::Cli(_) => format!("closed {}\n", joined(ids, " ")),
        Origin::Client(_) => String::new(),
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

/// A new pane of `session` running what `start` says, in `cwd`, its ID
/// taken from `ids`.
fn new_pane(
    session: &Session,
    ids: &mut Ids,
    (start, cwd, starting): (Start<'_>, &Path, &Starting),
    size: fux_vt::Size,
) -> Result<Pane, Error> {
    let config = &session.config;
    let shell = config.shell.argv();
    let (argv, cmd, runs, env) = match start {
        Start::Shell(cmd, env) => (shell.as_slice(), cmd, Runs::Shell, env),
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
            let fish = basename(config.shell.program()) == "fish";
            let mut line = crate::words::shell_line(cmd, fish)
                .map_err(|m| fail(Code::Usage, m))?
                .into_bytes();
            line.push(b'\r');
            let deadline = crate::after(Instant::now(), TYPE_WAIT);
            Some(
                Typed::new(line, deadline)
                    .ok_or_else(|| fail(Lang::TooLong, "the command line is too long to type"))?,
            )
        }
    };
    let id = ids.pane()?;
    let name = cmd
        .first()
        .map(|c| basename(c))
        .unwrap_or_else(|| basename(config.shell.program()));
    let mut pane = Pane::new(id, name, runs, size, config.history_bytes)?;
    pane.record.set_budget(config.journal_bytes);
    // A program brought back by a restore waits for `respawn`.
    let paused = starting.restoring && matches!(start, Start::Program(..));
    if !paused
        && let Some((child, nonce)) = session.spawn(id, (argv, env), (cwd, starting), size)?
    {
        pane.process = Process::Reading(child);
        pane.mark_with(nonce);
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
    if name.trim().is_empty() {
        return Err(fail(Lang::BadName, "a name cannot be empty or blank"));
    }
    // Control characters, and the invisible ones that change how text
    // around them shows (zero-width, direction marks and overrides: UAX #9
    // 2.6 names U+061C beside U+200E and U+200F).
    let invisible = |c: char| matches!(c, '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
    if name.chars().any(|c| c.is_control() || invisible(c)) {
        let message = "a name cannot contain control or invisible characters";
        return Err(fail(Lang::BadName, message));
    }
    if name.len() > 256 {
        return Err(fail(Lang::BadName, "a name is at most 256 bytes"));
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
    if others.iter().any(|ws| ws.name == name) {
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
        .filter(|t| t.given && t.name == name && Some(t.id) != except)
        .collect();
    if tabs.is_empty() {
        return Ok(());
    }
    let panes = tabs
        .iter()
        .flat_map(|t| t.root().map(Tree::panes).unwrap_or_default());
    let panes = panes.collect();
    Err(Error::TabNameTaken(
        name.to_owned(),
        tabs.iter().map(|t| t.id).collect(),
        panes,
    ))
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
    use super::{Origin, Reply, Session};
    use crate::code::Code;
    use crate::command::{Command, Request};
    use crate::config::Config;
    use crate::id::ClientId;

    /// A session of `config`, but for its shell, `/bin/sh` (each pane is
    /// named after it), started.
    pub(crate) fn started(config: Config) -> Result<Session, String> {
        let config = Config {
            shell: crate::config::Shell::parse("/bin/sh")?,
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

    /// A session itself, as what a program's requests go to: each is begun
    /// at once, and a wait answered as it stands, there being no programs
    /// to wait on.
    pub(crate) struct InProcess<'s>(pub &'s mut Session);

    impl crate::script::Asks for InProcess<'_> {
        fn ask(
            &mut self,
            asked: &crate::protocol::Asked,
            out: &mut dyn std::io::Write,
        ) -> Result<Reply, crate::script::Failed> {
            let session = &mut *self.0;
            let origin = match asked.context.client {
                Some(client) if session.views.contains_key(&client) => Origin::Client(client),
                Some(_) | None => Origin::Cli(super::Caller::default()),
            };
            let outcome = match session.begin(asked.request.clone(), &origin) {
                super::Begun::Done(outcome) => outcome,
                super::Begun::Waiting(mut waiting) => {
                    let later = crate::after(std::time::Instant::now(), waiting.limit);
                    waiting.answer(session, later).unwrap_or_default()
                }
                super::Begun::Events(_) => {
                    let _ = writeln!(out, "events stream from a real server only");
                    Reply::default()
                }
                super::Begun::Grant(..) => {
                    Reply::failed(Code::Grant, "a grant is made by a real server only")
                }
            };
            Ok(outcome)
        }
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
        let (out, ended) = run_as(session, client, false, code);
        let ended = ended.map_err(|e| e.to_string())?;
        let mut text = String::from_utf8_lossy(&out).into_owned();
        for value in &ended.stack {
            text.push_str(&value.text());
            text.push('\n');
        }
        Ok(text)
    }

    /// Runs fuxth `code` as [`output_as`] does, settling as a command line
    /// does if `settle`: each value it left, its kind's name then its text's
    /// first line (`pane %1`), or the failure's code and status.
    pub(crate) fn stack_as(
        session: &mut Session,
        client: Option<ClientId>,
        settle: bool,
        code: &str,
    ) -> Vec<String> {
        match run_as(session, client, settle, code).1 {
            Ok(ended) => (ended.stack.iter())
                .map(|value| {
                    let text = value.text();
                    format!(
                        "{} {}",
                        value.kind_name(),
                        text.lines().next().unwrap_or("")
                    )
                })
                .collect(),
            Err(failed) => vec![format!("fails {} {}", failed.code, failed.status)],
        }
    }

    fn run_as(
        session: &mut Session,
        client: Option<ClientId>,
        settle: bool,
        code: &str,
    ) -> (Vec<u8>, Result<crate::script::Ended, crate::script::Failed>) {
        let mut out = Vec::new();
        let tokens = match fuxth::token::source("test", code) {
            Ok(tokens) => tokens,
            Err(e) => return (out, Err(crate::script::Failed::from(e))),
        };
        let mut machine = match crate::script::compile(&tokens) {
            Ok(machine) => machine,
            Err(failed) => return (out, Err(failed)),
        };
        let mut keymap = std::mem::take(&mut session.test_keymap);
        let context = crate::protocol::Context {
            client,
            ..crate::protocol::Context::default()
        };
        let mut host =
            crate::script::Host::new(context.clone(), Some(&mut keymap), &mut out, settle);
        let mut process = InProcess(session);
        let mut asks = crate::script::AsClient {
            asks: &mut process,
            context,
            opened: None,
        };
        let ended = crate::script::run(&mut machine, &mut host, &mut asks);
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
                super::Outgoing::To(client, ToAttached::Run(code)) => session.run_as(client, &code),
                super::Outgoing::To(client, ToAttached::Ended) => {
                    if let Some(keys) = session.test_keys.get_mut(&client)
                        && keys.screen.is_open()
                    {
                        keys.screen = crate::screens::Screen::default();
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
                    });
                    session.inputs(client, typed.collect());
                }
                other @ (super::Outgoing::To(
                    _,
                    ToAttached::Paint(_) | ToAttached::Exit(_) | ToAttached::Welcome(..),
                )
                | super::Outgoing::Shutdown { .. }) => {
                    kept.push(other);
                }
            }
        }
        kept.extend(std::mem::take(&mut session.outbox));
        session.outbox = kept;
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
                    Request::Do(Command::Client { action, .. }) => action.clone().here(),
                    Request::Do(Command::Resolve { .. })
                    | Request::Wait(..)
                    | Request::Capture(..)
                    | Request::Events(..)
                    | Request::Grant(..)
                    | Request::Revoke(_) => return self.0.ask(asked, out),
                    Request::Do(command) | Request::Then(command, _) => command.clone(),
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
        let mut host = crate::script::Host::new(context, None, &mut out, false);
        let mut recorder = Recorder(InProcess(s), Vec::new());
        crate::script::run(&mut machine, &mut host, &mut recorder).map_err(|e| e.to_string())?;
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
            acts.extend(screen.opened());
            keys.screen = screen;
            self.acts(client, acts);
            deliver(self);
        }

        /// Runs fuxth as client `client`: a failure, or what it says, in its
        /// notice, as its client shows them.
        pub(crate) fn run_as(&mut self, client: ClientId, code: &str) {
            match output_as(self, Some(client), code) {
                Err(error) => self.error_to(client, error),
                Ok(said) if !said.trim().is_empty() => {
                    self.info_to(client, said.trim().replace('\n', "  "));
                }
                Ok(_) => {}
            }
        }

        fn effects(&mut self, client: ClientId, effects: Vec<crate::screens::Effect>) {
            for effect in effects {
                match effect {
                    crate::screens::Effect::Send(act) => self.acts(client, vec![act]),
                    crate::screens::Effect::Run(code) => self.run_as(client, &code),
                }
                deliver(self);
            }
        }

        pub(crate) fn inputs(&mut self, client: ClientId, inputs: Vec<crate::decode::Input>) {
            use crate::act::Act;
            use crate::decode::Input;
            for input in inputs {
                let Some(read_only) = self.views.get(&client).map(|v| v.read_only) else {
                    return;
                };
                let keys = self.test_keys.entry(client).or_default();
                let open = keys.screen.is_open();
                let act = match input {
                    Input::Key(stroke) => {
                        let effects = keys.screen.key(&self.test_keymap, stroke, read_only);
                        self.effects(client, effects);
                        continue;
                    }
                    Input::Reply(reply) => Act::Reply(reply),
                    Input::Paste(_)
                    | Input::PasteTooLong
                    | Input::FocusIn
                    | Input::FocusOut
                    | Input::Mouse(_)
                        if read_only =>
                    {
                        continue;
                    }
                    Input::Paste(text) => {
                        let effects = keys.screen.paste(&text);
                        self.effects(client, effects);
                        continue;
                    }
                    Input::PasteTooLong => {
                        let limit = crate::decode::PASTE_LIMIT / 1024;
                        let text = format!("paste exceeds {limit} KiB; discarded");
                        Act::Notice { text, error: true }
                    }
                    Input::FocusIn => Act::Focus(true),
                    Input::FocusOut => Act::Focus(false),
                    Input::Mouse(_) if open => continue,
                    Input::Mouse(event) => Act::Mouse(event),
                };
                self.acts(client, vec![act]);
                deliver(self);
            }
        }
    }
}

/// What a paste is to the program reading it: an edit, bracketed or one
/// line; unbracketed with a line end, it submits lines.
fn paste_unit(text: &str, bracketed: bool) -> crate::log::Unit {
    match bracketed || !text.contains(['\r', '\n']) {
        true => crate::log::Unit::Edit,
        false => crate::log::Unit::Submit,
    }
}

/// `inputs` as pane `p`'s program takes them, a piece at a time
/// (`crate::log::Unit`): text typed in a row is one piece, keys that
/// edit the line in a row another, and each other key, a lone Escape
/// and a paste each their own. So in `keys str msg text str Enter key` the Enter is
/// read alone, as a submit, not as a newline pasted with the text, and
/// what follows each piece is told apart (`crate::classify`).
fn units(p: &Pane, inputs: &[Input]) -> Vec<(Vec<u8>, crate::log::Unit)> {
    use crate::log::Unit;
    let (mode, bracketed) = (
        p.screen().key_mode(),
        p.screen().mode(fux_vt::Mode::BracketedPaste),
    );
    let mut units: Vec<(Vec<u8>, Unit)> = Vec::new();
    // Whether the last piece takes more, an edit: text, or keys.
    let mut last: Option<bool> = None;
    for input in inputs {
        let mut bytes = Vec::new();
        let unit = match input {
            Input::Key(key) => {
                crate::encode::key_bytes((*key).into(), mode, &mut bytes);
                crate::command::unit(*key)
            }
            Input::Text(text) => {
                // A line end in text submits the line, read alone.
                let mut rest = text.as_str();
                while let Some(at) = rest.find(['\r', '\n']) {
                    let (part, end) = rest.split_at_checked(at).unwrap_or((rest, ""));
                    if !part.is_empty() {
                        push_piece(&mut units, &mut last, part.as_bytes(), true);
                    }
                    let (end, after) = end.split_at_checked(1).unwrap_or((end, ""));
                    units.push((end.as_bytes().to_vec(), Unit::Submit));
                    last = None;
                    rest = after;
                }
                if !rest.is_empty() {
                    push_piece(&mut units, &mut last, rest.as_bytes(), true);
                }
                continue;
            }
            Input::Paste(text) => {
                crate::encode::paste(text, bracketed, &mut bytes);
                units.push((bytes, paste_unit(text, bracketed)));
                last = None;
                continue;
            }
        };
        if unit == Unit::Edit && bytes != b"\x1b" {
            push_piece(&mut units, &mut last, &bytes, false);
        } else {
            units.push((bytes, unit));
            last = None;
        }
    }
    units
}

/// Adds `bytes`, an edit that is `text` typed or else keys, to the last
/// piece if that is an edit of the same kind, else as a piece of its own.
fn push_piece(
    units: &mut Vec<(Vec<u8>, crate::log::Unit)>,
    last: &mut Option<bool>,
    bytes: &[u8],
    text: bool,
) {
    match units.last_mut() {
        Some((piece, crate::log::Unit::Edit)) if *last == Some(text) => {
            piece.extend_from_slice(bytes);
        }
        _ => units.push((bytes.to_vec(), crate::log::Unit::Edit)),
    }
    *last = Some(text);
}

#[cfg(test)]
mod tests {
    use super::testing::{attached, output, requests, run, started};
    use super::*;

    /// A run refused (its name is taken) leaves the log file it names as it
    /// was: the file is emptied only for a run that starts.
    #[test]
    fn a_refused_run_leaves_its_log_alone() -> Result<(), Box<dyn std::error::Error>> {
        let mut s = started(Config::default())?;
        let dir = std::env::temp_dir().join(format!("fux-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let log = dir.join("out.log");
        let line = format!(
            "opts str a name str '{}' log-to str /bin/cat program run",
            log.display()
        );
        run(&mut s, &line)?;
        std::fs::write(&log, "kept")?;
        assert!(run(&mut s, &line).is_err_and(|e| e.contains("name_taken")));
        assert_eq!(std::fs::read_to_string(&log)?, "kept");
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// A paste into a pane whose program exited is refused, as keys are.
    #[test]
    fn a_paste_into_an_exited_pane_is_refused() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts str /bin/cat program run")?;
        s.exited(PaneId::of(2), crate::process::Ended::Exited(3));
        s.buffers.push_front("x".into());
        let pasted = run(&mut s, "%2 0 paste-buffer");
        assert!(pasted.is_err_and(|e| e.contains("exited")), "refused");
        Ok(())
    }

    /// Keys sent to a tab's panes go to every one of them or to none: a
    /// pane that has no room for them refuses them before any is sent.
    #[test]
    fn keys_for_several_panes_go_to_all_or_none() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "%1 opts split drop")?;
        let second = s.panes.get_mut(&PaneId::of(2)).ok_or("no %2")?;
        // Its program reads nothing: the queue refuses everything now.
        assert!(second.input.push(vec![0; 1 << 30]).is_err());
        let sent = run(&mut s, "@1 keys str x text send-keys");
        assert!(sent.is_err_and(|e| e.contains("not_reading")), "refused");
        let first = s.panes.get(&PaneId::of(1)).ok_or("no %1")?;
        assert_eq!(first.input.len(), 0, "the first pane got none");
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
            s.panes
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
                .any(|o| matches!(o, Outgoing::To(c, ToAttached::Paint(b)) if *c == client && **b == *b"\x1b]52;c;Zm9v\x07"))
        };
        assert!(sent(&s));
        s.outbox.clear();
        run(&mut s, "str clipboard str off set")?;
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
        run(&mut s, "opts str +1 workspace in new-tab")?;
        let size = |s: &Session, p| s.panes.get(&PaneId::of(p)).map(Pane::size);
        assert_eq!(size(&s, 2), Some(crate::pane::size(10, 40)));
        run(&mut s, "str %2 pane opts vertical split")?;
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
        assert_eq!(size(&s, 2), Some(crate::pane::size(10, 40)));
        Ok(())
    }

    /// `ls` of a workspace lists it whole; of a tab, that tab; of a pane,
    /// its tab with that pane alone.
    #[test]
    fn ls_of_a_target_lists_what_is_in_it() -> Result<(), String> {
        let (mut s, _) = attached(11, 40)?;
        run(&mut s, "opts str +1 workspace in new-tab")?;
        run(&mut s, "str %2 pane opts vertical split")?;
        let ids = |only| {
            let text = s.ls_text(Some(only));
            let words = text.lines().filter_map(|l| l.split_whitespace().next());
            words.map(str::to_owned).collect::<Vec<_>>().join(" ")
        };
        let ws = "+1".parse::<WsId>().map_err(|e| e.to_string())?;
        let tab = "@2".parse::<TabId>().map_err(|e| e.to_string())?;
        assert_eq!(ids(AnyRef::Workspace(ws)), "+1 @1 %1 @2 %2 %3");
        assert_eq!(ids(AnyRef::Tab(tab)), "@2 %2 %3");
        assert_eq!(ids(AnyRef::Pane(PaneId::of(3))), "@2 %3");
        Ok(())
    }

    /// The `json` outputs, byte for byte: escapes, borrowed names and
    /// formatted IDs alike.
    #[test]
    fn the_json_help_names_every_pane_field() -> Result<(), Box<dyn std::error::Error>> {
        let (s, _) = attached(4, 30)?;
        let pane = s.panes.values().next().ok_or("no pane")?;
        for (name, _) in s.pane_json(pane, Instant::now()) {
            let named = crate::help::JSON.contains(&format!("`{name}`"));
            assert!(named, "docs/json.md does not name a pane's {name:?}");
        }
        Ok(())
    }

    #[test]
    fn json_outputs_are_what_they_were() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(4, 30)?;
        run(&mut s, "str %1 pane opts horizontal split")?;
        run(&mut s, r#"str %1 pane str 'say "hi" \ 界' rename"#)?;
        run(&mut s, "opts str +1 workspace in str two name new-tab")?;
        run(&mut s, "str c1 client str @1 tab show-tab")?;
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
            quiet(output(&mut s, "ls json")?),
            concat!(
                r#"{"workspaces":[{"id":"+1","name":"main","tabs":[{"id":"@1","name":"main","panes":[{"id":"%1","name":"say \"hi\" \\ 界","title":"","rows":3,"cols":15,"pid":null,"program":null,"state":"empty","paused":false,"exited":null,"signal":null,"idle_ms":0,"alt_screen":false,"mouse":false,"mouse_mode":null,"bracketed_paste":false,"pictures":[],"cursor_visible":true,"history_lines":0,"copy_mode":false,"revision":0,"command_running":false,"last_status":null,"bells":0,"notification":null,"cwd":null,"argv":null},{"id":"%2","name":"sh","title":"tab\\title","rows":3,"cols":14,"pid":null,"program":null,"state":"empty","paused":false,"exited":null,"signal":null,"idle_ms":0,"alt_screen":false,"mouse":false,"mouse_mode":null,"bracketed_paste":false,"pictures":[],"cursor_visible":true,"history_lines":0,"copy_mode":false,"revision":1,"command_running":false,"last_status":null,"bells":0,"notification":null,"cwd":null,"argv":null}]},{"id":"@2","name":"two","panes":[{"id":"%3","name":"sh","title":"","rows":3,"cols":30,"pid":null,"program":null,"state":"empty","paused":false,"exited":null,"signal":null,"idle_ms":0,"alt_screen":false,"mouse":false,"mouse_mode":null,"bracketed_paste":false,"pictures":[],"cursor_visible":true,"history_lines":0,"copy_mode":false,"revision":0,"command_running":false,"last_status":null,"bells":0,"notification":null,"cwd":null,"argv":null}]}]}],"clients":[{"id":"c1","rows":4,"cols":30,"workspace":"+1","tab":"@1","pane":"%2","zoom":false}],"grants":[]}"#,
                "\n"
            )
        );
        assert_eq!(
            quiet(output(&mut s, "str %2 pane opts capture json")?),
            concat!(
                r#"{"pane":"%2","rows":3,"cols":14,"cursor":[1,2],"lines":["a       b \"q\"","界"],"program":null,"state":"empty","paused":false,"exited":null,"signal":null,"idle_ms":0,"alt_screen":false,"mouse":false,"mouse_mode":null,"bracketed_paste":false,"pictures":[],"cursor_visible":true,"history_lines":0,"copy_mode":false,"revision":1,"command_running":false,"last_status":null,"bells":0,"notification":null,"cwd":null,"argv":null}"#,
                "\n"
            )
        );
        assert_eq!(
            output(&mut s, "str c1 client capture-client json")?,
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
        run(&mut s, "str %1 pane opts horizontal split")?;
        s.exited(PaneId::of(2), crate::process::Ended::Exited(3));
        assert_eq!(notice(&s).as_deref(), Some("%2 sh exited with status 3"));
        // A second tab, shown; its only pane exits, and the tab with it.
        run(&mut s, "opts str +1 workspace in new-tab")?;
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
        run(&mut s, "str %1 pane opts horizontal split")?;
        run(&mut s, "opts str +1 workspace in new-tab")?;
        run(&mut s, "str %3 pane str @1 tab move-to")?;
        run(&mut s, "str %1 pane str right move-beside")?;
        let holding = |s: &Session, pane| {
            let tabs = s.workspaces.iter().flat_map(|w| w.tabs());
            tabs.filter(|t| t.holds(pane)).count()
        };
        assert_eq!(holding(&s, PaneId::of(1)), 1);
        run(&mut s, "str %1 pane kill")?;
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
        assert!(run(&mut s, "opts str main name new-workspace").is_err());
        assert_eq!(
            output(&mut s, "opts str workspace-3 name new-workspace")?,
            "%2\n"
        );
        // +3 would be workspace-3, which is taken.
        assert_eq!(output(&mut s, "opts new-workspace")?, "%3\n");
        let names: Vec<&str> = s.workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["main", "workspace-3", "workspace-4"]);
        // A move to a new workspace with no tab ID left takes no workspace
        // ID either.
        s.ids.exhaust_tabs();
        let ids = s.ids;
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
            "str %1 pane opts capture",
            "list-keys",
            "str %1 pane keys str x text send-keys",
            "str c1 client keys str x text send-keys",
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
        run(&mut s, "str %1 pane opts horizontal split")?;
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
            history_bytes: 2 * 32,
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
            history_bytes: 2 * 32,
            ..Config::default()
        })?;
        let client = s.attach(6, 20, None)?;
        s.output(PaneId::of(1), b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        run(&mut s, "str c1 client copy-mode")?;
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

    /// A failing request says what it said as a string, word for word:
    /// each program, run in turn on one session, and its message, as they
    /// were before requests failed with `Error`.
    #[test]
    fn command_errors_keep_their_words() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        let long = format!(
            "str %1 pane str {} rename",
            std::iter::repeat_n("x", 257).collect::<String>()
        );
        let not_a_name =
            "cannot be a name: a target would read it as a pane, tab or workspace number";
        for (code, message) in [
            ("str %99 pane kill", "no pane %99".to_owned()),
            ("str @99 tab kill", "no tab @99".to_owned()),
            ("str +99 workspace kill", "no workspace +99".to_owned()),
            ("str nope workspace kill", r#"no workspace named "nope""#.to_owned()),
            ("str c9 client zoom", "no client c9".to_owned()),
            ("0 buffer", "no buffer 0".to_owned()),
            ("str %1 pane 0 paste-buffer", "no buffer 0".to_owned()),
            // A workspace's name is what targets read as a name.
            ("opts str %3 name new-workspace", format!(r#""%3" {not_a_name}"#)),
            ("str +1 workspace str +2 rename", format!(r#""+2" {not_a_name}"#)),
            ("str +1 workspace str @2 rename", format!(r#""@2" {not_a_name}"#)),
            ("str %1 pane str '' rename", "a name cannot be empty or blank".to_owned()),
            (long.as_str(), "a name is at most 256 bytes".to_owned()),
            ("str %1 pane str up 1 move-border", "%1 has no border to move up".to_owned()),
            ("str %1 pane str left swap-toward", "no pane left of %1".to_owned()),
            ("str c1 client str next focus", "only one pane".to_owned()),
            ("str c1 client str last focus", "no previously focused pane".to_owned()),
            ("str c1 client str left focus", "no pane left of %1".to_owned()),
            ("str c1 client next-tab", "only one tab".to_owned()),
            ("str c1 client next-workspace", "only one workspace".to_owned()),
            ("str @1 tab str next reorder", "the tab is already at that end".to_owned()),
            ("str %1 pane str previous reorder", "the pane is already at that end".to_owned()),
            ("str +1 workspace str previous reorder", "the workspace is already at that end".to_owned()),
            ("str %1 pane str left move-beside", "no pane left of %1".to_owned()),
            (
                "str nope 1 set",
                r#"no setting "nope"; the settings are shell, history-bytes, wait-seconds, journal-bytes, journal-total, clipboard, buffers, bell, titles, shell-marks and update-environment"#.to_owned(),
            ),
            ("opts str '' name new-workspace", "a name cannot be empty or blank".to_owned()),
            (
                "str %1 pane opts horizontal str 'a\nb' typed split",
                r#"the command argument "a\nb" contains the control character '\n'; it would act as a key in the shell"#.to_owned(),
            ),
            ("opts str main name new-workspace", r#"a workspace is named "main""#.to_owned()),
        ] {
            // `CODE: test:LINE: WORD: MESSAGE`: the message after where.
            let failed = run(&mut s, code).err().unwrap_or_default();
            assert!(failed.ends_with(&format!(": {message}")), "{code}: {failed}");
        }
        run(&mut s, "opts str two name new-workspace")?;
        for (code, message) in [
            (
                "str +2 workspace str main rename",
                r#"a workspace is named "main""#,
            ),
            (
                "str two workspace str next reorder",
                "the workspace is already at that end",
            ),
        ] {
            let failed = run(&mut s, code).err().unwrap_or_default();
            assert!(
                failed.ends_with(&format!(": {message}")),
                "{code}: {failed}"
            );
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
            Some("nothing_there: test:1: choose-pane: only one pane")
        );
        run(&mut s, "str c1 client str %1 pane menu")?;
        assert!(matches!(
            s.views.get(&c).map(|v| &v.mode),
            Some(Mode::Panel(crate::act::Panel::List(list))) if list.title == "pane %1 sh"
        ));
        Ok(())
    }

    /// A bare id names its pane, tab or workspace, but not as `str`'s text;
    /// `json` among a capture's options gives JSON; `command` splits a line.
    #[test]
    fn ids_json_and_command_lines_read_as_written() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        assert_eq!(output(&mut s, "%1 str name get")?, "sh\n");
        assert_eq!(output(&mut s, "@1 str name get")?, "main\n");
        // A pane's title names it too, as `ls` shows it.
        s.output(PaneId::of(1), b"\x1b]2;spool\x07");
        assert_eq!(output(&mut s, "str spool pane")?, "%1\n");
        assert_eq!(output(&mut s, "str %1")?, "%1\n");
        let json = output(&mut s, "%1 opts json capture")?;
        assert!(json.starts_with(r#"{"pane":"%1""#), "{json}");
        let made = output(&mut s, "opts str 'sh -c \"exit 0\"' command run")?;
        assert!(made.starts_with('%'), "{made}");
        Ok(())
    }

    /// A capture's cursor is where it is among the lines printed: none
    /// while hidden, on a row trimmed away, outside the columns taken, or
    /// on the screen not captured; past the columns cut and the rows
    /// joined.
    #[test]
    fn a_captures_cursor_is_among_its_lines() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        let cursor = |s: &mut Session, read: &str| -> Result<String, String> {
            let json = output(s, &format!("%1 opts {read} json capture"))?;
            let after = json.split("\"cursor\":").nth(1).unwrap_or_default();
            Ok(after
                .split(",\"lines\"")
                .next()
                .unwrap_or_default()
                .to_owned())
        };
        s.output(PaneId::of(1), b"0123456789\x1b[1;6H");
        assert_eq!(cursor(&mut s, "")?, "[0,5]");
        assert_eq!(cursor(&mut s, "2 7 cols")?, "[0,3]");
        assert_eq!(cursor(&mut s, "0 2 cols")?, "null");
        s.output(PaneId::of(1), b"\x1b[?25l");
        assert_eq!(cursor(&mut s, "")?, "null", "hidden");
        s.output(PaneId::of(1), b"\x1b[?25h\x1b[3;1H");
        assert_eq!(cursor(&mut s, "")?, "null", "on a blank row trimmed");
        s.output(
            PaneId::of(1),
            &[b"\x1b[2J\x1b[H".as_slice(), &[b'A'; 45], b"\r\nBC"].concat(),
        );
        assert_eq!(cursor(&mut s, "")?, "[2,2]");
        assert_eq!(cursor(&mut s, "joined")?, "[1,2]");
        s.output(PaneId::of(1), b"\x1b[?1049h\x1b[HALT");
        assert_eq!(cursor(&mut s, "alt-screen")?, "[0,3]");
        assert_eq!(cursor(&mut s, "main-screen")?, "null");
        s.output(PaneId::of(1), b"\x1b[?1049l");
        assert_eq!(cursor(&mut s, "alt-screen")?, "null");
        assert_eq!(cursor(&mut s, "main-screen")?, "[2,2]");
        // What is highlighted is marked in JSON's lines too.
        s.output(PaneId::of(1), b"\x1b[7mSEL\x1b[0m");
        let json = output(&mut s, "%1 opts marks json capture")?;
        assert!(json.contains("\"BC⟦SEL⟧\""), "{json}");
        let inverted = output(&mut s, "%1 opts 5 2 rows capture");
        assert!(
            inverted.as_ref().is_err_and(|e| e.starts_with("range: ")),
            "{inverted:?}"
        );
        Ok(())
    }

    /// An action's answer is its pane, to every word that wants a pane:
    /// `pane`, `tab` and `workspace` too; and `json` gives it as it gives
    /// it alone, in a list too.
    #[test]
    fn an_actions_answer_is_its_pane() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        assert_eq!(output(&mut s, "%1 opts wait str name get")?, "sh\n");
        assert_eq!(
            output(&mut s, "%1 opts wait dup tab swap workspace")?,
            "@1\n+1\n"
        );
        let alone = output(&mut s, "%1 opts wait json")?;
        assert!(alone.starts_with(r#"{"pane":"%1","state":"#), "{alone}");
        let listed = output(&mut s, "list %1 opts wait end json")?;
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
        let timeout = failed(&mut s, "%1 opts -1 timeout wait");
        assert!(timeout.starts_with("range: "), "{timeout}");
        assert!(
            timeout.contains("-1 is out of range: from 0 (use: "),
            "{timeout}"
        );
        let mouse = failed(&mut s, "%1 str click -1 0 mouse send-mouse");
        assert!(mouse.contains("out of range: 0 to 65535 (use: "), "{mouse}");
        let times = failed(&mut s, "keys str a text str 2 times");
        assert!(times.starts_with("type: "), "{times}");
        let none = failed(&mut s, "keys str a text 0 times");
        assert!(none.contains("0 is out of range: 1 to 1000 (use: "), "{none}");
        // The core's words say how they are used too; a library word is
        // told by its own name, not the word inside it that fails.
        for code in ["slice", "str a 1 add"] {
            let core = failed(&mut s, code);
            assert!(core.contains(" (use: "), "{core}");
        }
        let upper = failed(&mut s, "3 upper");
        assert!(upper.contains("upper: every way"), "{upper}");
        assert!(
            upper.contains("(use: S upper") && !upper.contains("each"),
            "{upper}"
        );
        let env = failed(&mut s, "opts str 1bad str v env");
        assert!(env.contains("not first a digit"), "{env}");
        let missing = failed(&mut s, "str /no/such/file.fux check");
        assert!(missing.contains("No such file"), "{missing}");
        let split = failed(&mut s, "str 'two\nlines' fail");
        assert!(split.ends_with(r"two\nlines"), "{split}");
        // A tab in a waited text is a blank, as in the screen's.
        run(&mut s, "%1 opts str 'ready\tnow' wait-text drop drop")?;
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
        assert!(matches!(
            run("str nope 1 set")?.as_slice(),
            [Err(e)] if e.code() == Code::Config
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
            Err(e) if e.code() == Code::BadValue
        ));
        Ok(())
    }
}

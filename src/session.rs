//! The server's state: workspaces, tabs, panes and the clients' views, and
//! every command that changes them.
use crate::code::Code;
use crate::command::{
    self, AnyRef, ClientAction, Command, Extent, Input, Kind, Mouse, MoveTo, PanePick, PaneResize,
    Pick, Request, Resized, Sibling, SwapWith, WsRef,
};
use crate::config::Config;
use crate::copy::MAX_CELLS;
use crate::id::{ClientId, Ids, PaneId, TabId, Wanted, WsId};
use crate::json::{Fields, Json};
use crate::keys::Direction;
use crate::layout::{self, Axis, Placement, Rect, Side, Tree};
use crate::pane::{self, Pane, Process, Runs, basename};
use crate::process::{self, Child, Ended};
use crate::protocol::{End, Reply, ScreenKind, ToAttached};
use crate::view::{ClientKind, Mode, View};
use crate::wait::Waiting;
use crate::workspace::{Fit, Seat, Tab, Workspace};
use fux_vt::Size;
use fuxth::Code as Lang;
use std::collections::{BTreeMap, HashSet, VecDeque};
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
    /// server's other work of this turn.
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
    pub env: Vec<(String, std::ffi::OsString)>,
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

impl Made {
    /// What a command that makes a pane answers, as an action does: its
    /// pane and the pane's version as it starts (what a wait for its
    /// program waits to differ from); all the ids it made go in its reply.
    fn reply(self, version: command::Version) -> Reply {
        let ids = vec![
            AnyRef::Workspace(self.ws),
            AnyRef::Tab(self.tab),
            AnyRef::Pane(self.pane),
        ];
        Reply {
            ids,
            ..acted([version])
        }
    }
}

/// A pane to make: what it starts, where, what it takes of its starter,
/// and its size to begin with.
#[derive(Clone, Copy)]
struct NewPane<'a> {
    start: Start<'a>,
    cwd: &'a Path,
    starting: &'a Starting,
    size: Size,
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
    /// The CLI with no caller: a command run for no one.
    const NOWHERE: Here<'static> = Here::Cli(None, None);

    fn ws(self) -> Option<WsId> {
        match self {
            Here::Client(_, place) => Some(place.ws),
            Here::Cli(pane, _) => pane.and_then(|(_, ws)| ws),
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
    /// A tab name given before: those tabs, then their panes.
    /// A tab has the name: those tabs and their panes, and whether a
    /// restore brought one of them back.
    TabNameTaken(String, Vec<TabId>, Vec<PaneId>, bool),
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
            Self::NoWorkspace(_) => Code::NoWorkspace,
            Self::NoPane(_) => Code::NoPane,
            Self::NoTab(_) => Code::NoTab,
            Self::NoPaneGiven | Self::NoWorkspaceGiven => Code::NoTarget,
            Self::IdTaken(_) => Code::Id,
            Self::NoClient(_) => Code::NoClient,
            Self::NoBuffer(_) => Code::NoBuffer,
            Self::OnlyOne(_) | Self::AtEnd(_) | Self::NoNeighbor { .. } => Code::NothingThere,
            Self::NoRows | Self::SelectionTooLarge => Code::Selection,
            Self::NameReadAsTarget(_) => Code::from(Lang::BadName),
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
            Self::IdTaken(id) => write!(
                f,
                "id {id}: that id, or a higher one, was given before, and ids are never given twice"
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
            Self::TabNameTaken(name, tabs, panes, restored) => {
                let ids = (tabs.iter().map(|t| AnyRef::Tab(*t)))
                    .chain(panes.iter().map(|p| AnyRef::Pane(*p)));
                let ids = joined(ids, " ");
                let kills = joined(tabs, " tab kill`, `fux str ");
                // The surprise of a server that brought back what an earlier
                // one on its socket had, which was killed or crashed.
                let restored = match restored {
                    true => {
                        ", brought back from the last server on this socket, which did not \
                         stop cleanly (killed or crashed; fux saves a server's layout in \
                         $XDG_STATE_HOME/fux for that): its program waits for `respawn`"
                    }
                    false => "",
                };
                write!(
                    f,
                    "a tab is named {name:?} already ({ids}){restored}; close it (`fux str {kills} tab kill`) or choose another name"
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
            pane::Error::InputFull => Code::InputFull,
            pane::Error::Terminal { .. } => Code::Start,
        };
        fail(code, error.to_string())
    }
}

/// `pane`'s program has ended, with `status`: it takes no input.
fn ended(pane: PaneId, status: Ended) -> Error {
    let was = match status {
        Ended::Exited(_) => "",
        Ended::Signalled(_) => "was ",
    };
    let told = format!("the program in {pane} {was}{}", process::told(status));
    fail(Code::NotRunning, told)
}

/// What a command line from the CLI begins: its outcome, now, or a wait
/// for one (`wait`, or an action given wait words).
pub enum Begun {
    Done(Reply),
    Waiting(Box<Waiting>),
    /// `fux events`: a stream, until the client goes.
    Events(Box<crate::events::Subscription>),
    /// A grant of a socket to the workspace (`grant`), or with none, its
    /// revoking (`revoke`): the server's, which listens.
    Grant(String, Option<WsRef>),
}

/// The longest command line typed into a new shell: what a terminal in
/// line mode keeps of a line on macOS (`MAX_CANON`, 1024), less its end;
/// the smaller of macOS's and Linux's (4095).
const TYPED: usize = 1023;
/// How long a closing pane's processes have to exit after the hangup.
pub const GRACE: Duration = Duration::from_millis(100);
/// The size a pane starts at, until it is laid out on a client's screen.
const DEFAULT_SIZE: (u16, u16) = (24, 80);
/// A typical terminal's screen, which a tab no client shows is laid out on.
const TYPICAL: Rect = Rect::screen(DEFAULT_SIZE.0, DEFAULT_SIZE.1);
/// The most rows, and columns, a client's view is kept to.
const MOST: u16 = 4096;

pub(crate) fn default_size() -> Size {
    pane::size(DEFAULT_SIZE.0, DEFAULT_SIZE.1)
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
    /// The running fux, a started pane's `FUX`, if it can be had.
    exe: Option<PathBuf>,
    /// Which run of a server this is: its panes' `FUX_SERVER`, by which a
    /// command from a pane tells whether the pane is this server's, and
    /// every version's, by which a wait tells its own.
    pub run: command::Run,
    pub outbox: Vec<Outgoing>,
    /// Closed panes' programs: a pane respawned twice in its grace has two.
    pub dying: Vec<Dying>,
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
    /// Each pane's tab, as of the last settle: every change to the layout
    /// settles before the server reads output again.
    pane_tabs: BTreeMap<PaneId, TabId>,
    /// Something changed that the saved layout may show: a settle, or
    /// output (a shell's directory). The server takes it.
    pub(crate) edited: bool,
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
    set: Vec<(String, std::ffi::OsString)>,
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

/// A log file, made if it is not there: what a program writes may be
/// secret, so it is the user's alone.
/// A link is not followed. A file there already is made the user's alone
/// too, and emptied, by `emptied`, once nothing refuses the log.
fn open_log(path: &Path) -> Result<std::fs::File, Error> {
    use std::os::unix::fs::OpenOptionsExt;
    let failed = |e| fail(Code::Log, format!("the log {}: {e}", path.display()));
    if path.is_symlink() {
        let link = std::io::Error::other("a symbolic link, which a log does not follow");
        return Err(failed(link));
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(fuxix::file::NOFOLLOW)
        .open(path)
        .map_err(failed)?;
    // Its mode is set once the run starts: setting the mode it has asks
    // now whether that will be allowed, before anything runs.
    let mode = file.metadata().map_err(failed)?.permissions();
    file.set_permissions(mode).map_err(failed)?;
    Ok(file)
}

/// `file`, a log, made the user's alone and emptied: a FIFO or a terminal
/// has nothing to empty.
fn emptied(file: std::fs::File) -> Result<std::fs::File, Error> {
    use std::os::unix::fs::PermissionsExt;
    let failed = |e| fail(Code::Log, format!("the log: {e}"));
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(failed)?;
    if file.metadata().map_err(failed)?.is_file() {
        file.set_len(0).map_err(failed)?;
    }
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
            Request::Do(command) => self.command(command),
            Request::Wait(wait) => match wait.since {
                command::Since::Now(target) => self.any(&target),
                command::Since::Version(version) => self.pane(version.pane),
            },
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

/// The answer of an action on `panes` (keys, a mouse event, a signal, a
/// program started): each pane's version before it acted, or as it
/// starts, a line each (`%2:0.57.0.3.0:RUN`), and as JSON; what a wait for
/// its effect waits to differ from.
fn acted(versions: impl IntoIterator<Item = command::Version>) -> Reply {
    let mut reply = Reply::default();
    for version in versions {
        let fields = vec![
            ("pane", Json::str(version.pane.to_string())),
            ("version", Json::str(version.to_string())),
            ("status", Json::int(0)),
        ];
        reply.text.push_str(&format!("{version}\n"));
        reply
            .json
            .push_str(&format!("{}\n", Json::Object(fields).render()));
        reply.versions.push(version);
    }
    reply
}

/// A command that failed, told with its code (its status: 1, 4 or 124).
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
    pub fn new(config: Config, socket: PathBuf, launch: bool, run: command::Run) -> Self {
        Self {
            workspaces: Vec::new(),
            panes: BTreeMap::new(),
            views: BTreeMap::new(),
            config,
            buffers: VecDeque::new(),
            launch: launch.then_some(socket),
            exe: std::env::current_exe().ok(),
            run,
            outbox: Vec::new(),
            dying: Vec::new(),
            last_colours: crate::outer::Colours::default(),
            last_palette: crate::outer::Palette::default(),
            shown_sizes: Vec::new(),
            placed: Placement::default(),
            pane_tabs: BTreeMap::new(),
            edited: false,
            ids: Ids::default(),
            changes: 0,
            grants: Vec::new(),
            #[cfg(test)]
            test_keys: BTreeMap::new(),
            #[cfg(test)]
            test_keymap: crate::keymap::Keymap::empty(),
        }
    }

    /// One workspace, holding one tab with one shell.
    pub fn start(&mut self) -> Result<(), Error> {
        let new = NewPane {
            start: Start::Shell(None, &[]),
            cwd: &home(),
            starting: &Starting::default(),
            size: default_size(),
        };
        self.create_workspace(Some(MAIN.into()), None, Fit::Clients, new)
            .map(drop)
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
    /// `FUX_SOCKET`, a grant's socket if its starter came by one,
    /// `FUX_SERVER`, and `FUX`, the fux running it, unset if it cannot be
    /// had), given its starter's variables, and not those its starter had
    /// not (but for the server's own above), unless `env` sets them.
    fn spawn(
        &self,
        id: PaneId,
        launch: &pane::Launch,
        fresh: &Starting,
        size: Size,
    ) -> Result<Option<Child>, Error> {
        let Some(socket) = &self.launch else {
            return Ok(None);
        };
        let (argv, env) = (launch.argv.as_slice(), launch.env.as_slice());
        let socket = fresh.socket.as_deref().unwrap_or(socket);
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
            .map_err(|m| fail(Code::Start, m))?;
        Ok(Some(child))
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
        let child = self.spawn(id, &launch, starting, size)?;
        let history = self.config.history_bytes;
        let pane = self.pane_mut(id)?;
        let old = pane.respawned(
            launch,
            child.map_or(Process::Absent, Process::Reading),
            history,
        )?;
        self.end(old, true);
        self.touch();
        Ok(())
    }

    /// The process group `fux signal` signals in a pane: the terminal's
    /// foreground group (a job its shell runs), or the program `run`
    /// started; never the shell of a pane that runs one.
    fn signalled(&self, target: PaneId) -> Result<process::Pid, Error> {
        let pane = self.pane(target)?;
        if let Some(status) = pane.process.exit_status() {
            return Err(ended(pane.id, status));
        }
        if pane.paused() {
            let why =
                format!("{target}: paused since the server restarted: respawn starts its program");
            return Err(fail(Code::NotRunning, why));
        }
        let program = match (&pane.runs, pane.process.child()) {
            (Runs::Program(_), Some(child)) => Some(child.leader.pid()),
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
        let paused = |p: &PaneId| self.panes.get(p).is_some_and(Pane::paused);
        no_tab_named(&self.workspaces, name, except, paused)
    }

    /// Gives tab `id` the name `name`, checked. It takes it from each other
    /// tab fux named so (`tab-2`, made before a dump's line names one so):
    /// that one is named anew as fux names tabs, by its place, the next
    /// number free.
    fn give_tab_name(&mut self, id: TabId, name: String) -> Result<(), Error> {
        let tab = (tabs_mut(&mut self.workspaces).find(|t| t.id == id)).ok_or(Error::NoTab(id))?;
        tab.name.clone_from(&name);
        tab.given = true;
        let mut taken: HashSet<String> = (self.workspaces.iter())
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
        // A version names its pane: one of this server run's.
        if let Ok(version) = text.parse::<command::Version>() {
            self.counts_since(version)?;
            return self.stands_for(AnyRef::Pane(version.pane), text, kind);
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

    /// The tabs `target` is or holds: a pane's own, a tab, or a
    /// workspace's every tab.
    fn tabs_of(&self, target: AnyRef) -> Vec<TabId> {
        match target {
            AnyRef::Tab(tab) => vec![tab],
            AnyRef::Workspace(ws) => self.tab_ids(ws),
            AnyRef::Pane(pane) => self.locate(pane).map(|(_, tab)| tab).into_iter().collect(),
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

    /// Sends `inputs` to the programs of `target`'s panes, each encoded as
    /// its program asked for keys (`bytes`).
    fn send_keys(&mut self, target: AnyRef, inputs: &[Input]) -> Result<Reply, Error> {
        // Every pane must take them, or none is sent them: each one's are
        // made and fit first.
        let (run, mut each) = (self.run, Vec::new());
        for id in self.panes_of(target)? {
            let pane = self.live_pane_mut(id)?;
            let bytes = bytes(pane, inputs);
            pane.input.fits(bytes.len())?;
            each.push((pane.version(run), bytes));
        }
        for (version, bytes) in &each {
            self.pane_mut(version.pane)?.input.push(bytes)?;
        }
        Ok(acted(each.into_iter().map(|(version, _)| version)))
    }

    /// `pane_mut`, for input: a pane whose program has exited takes none.
    fn live_pane_mut(&mut self, id: PaneId) -> Result<&mut Pane, Error> {
        let pane = self.pane_mut(id)?;
        let why = match pane.process {
            Process::HungUp(_) => "its program closed its terminal: nothing reads input there",
            Process::Absent if pane.paused() => {
                "paused since the server restarted: respawn starts its program"
            }
            Process::Exited(status) => return Err(ended(id, status)),
            Process::Absent | Process::Reading(_) => return Ok(pane),
        };
        Err(fail(Code::NotRunning, format!("{id}: {why}")))
    }

    /// The size a pane starts at in a new tab laid out on `fit`.
    fn start_size(&self, fit: Fit, here: Here) -> Size {
        let area = self.tab_area(fit, here);
        pane::size(area.h(), area.w())
    }

    /// The size a pane split from `target` along `axis` starts at: its
    /// place in the layout once split.
    fn split_size(&self, target: PaneId, axis: Axis, here: Here) -> Result<Size, Error> {
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
        Ok(pane::size(rect.h(), rect.w()))
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
            None => here.ws().ok_or(Error::NoWorkspaceGiven),
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
        dir: Option<&Path>,
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

    /// A workspace with one tab, laid out on `fit`, holding one new pane.
    fn create_workspace(
        &mut self,
        name: Option<String>,
        tab_name: Option<String>,
        fit: Fit,
        new: NewPane<'_>,
    ) -> Result<Made, Error> {
        if let Some(name) = &name {
            check_workspace_name(name, &self.workspaces)?;
        }
        // The IDs are taken before the pane starts, so that none can run out
        // after, and are committed once it has.
        let mut ids = self.ids;
        let id = ids.workspace()?;
        let tab = ids.tab()?;
        let pane = self.new_pane(&mut ids, new)?;
        self.ids = ids;
        let name = name.unwrap_or_else(|| self.workspace_name(id));
        let made = Made {
            ws: id,
            tab,
            pane: pane.id,
        };
        let tab_name = tab_name.unwrap_or_else(|| MAIN.into());
        let root = Some(Tree::Pane(pane.id));
        let first = Tab::new(tab, tab_name, root, fit, self.views.keys().copied());
        self.workspaces.push(Workspace::new(id, name, first));
        self.panes.insert(pane.id, pane);
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
        let at = self.workspaces.iter().position(|w| w.id == id);
        let at = at.ok_or(Error::NoWorkspace(id))?;
        let mut ids = self.ids;
        let tab = ids.tab()?;
        let pane = self.new_pane(&mut ids, new)?;
        let workspace = self.workspaces.get_mut(at).ok_or(Error::NoWorkspace(id))?;
        self.ids = ids;
        workspace.add_tab(tab, name, Some(Tree::Pane(pane.id)), fit);
        let made = Made {
            ws: id,
            tab,
            pane: pane.id,
        };
        self.panes.insert(pane.id, pane);
        Ok(made)
    }

    /// A new pane, its ID taken from `ids`, running what `new` says.
    fn new_pane(&self, ids: &mut Ids, new: NewPane<'_>) -> Result<Pane, Error> {
        let config = &self.config;
        let start = new.start;
        let env = match start {
            Start::Shell(_, env) | Start::Program(_, env) => env,
        };
        let launch = pane::Launch {
            argv: match start {
                Start::Shell(..) => config.shell.argv(),
                Start::Program(argv, _) => argv.to_vec(),
            },
            cwd: new.cwd.to_path_buf(),
            env: env.to_vec(),
        };
        // A shell is named after the shell, whatever line is typed into it.
        let (named, runs) = match start {
            Start::Shell(..) => (None, Runs::Shell),
            Start::Program(argv, _) => (
                argv.first().map(String::as_str),
                Runs::Program(launch.clone()),
            ),
        };
        let id = ids.pane()?;
        let name = basename(named.unwrap_or(config.shell.program()));
        let mut pane = Pane::new(id, name, runs, new.size, config.history_bytes)?;
        // A command line typed into the new shell is queued at once, and
        // written before the shell reads: its terminal takes it in line
        // mode, which gives a control character its effect and keeps at
        // most `TYPED` bytes of a line. Refused so, it leaves no process
        // behind.
        if let Start::Shell(Some(line), _) = start {
            if line.len() > TYPED || line.chars().any(char::is_control) {
                let why = format!(
                    "a command line typed into a new shell is at most {TYPED} bytes, with no \
                     control character (a tab too): its terminal, or the shell, would act on it \
                     as a key"
                );
                return Err(fail(Code::BadValue, why));
            }
            pane.input.push(format!("{line}\r"))?;
        }
        // A program brought back by a restore waits for `respawn`.
        let paused = new.starting.restoring && matches!(start, Start::Program(..));
        if !paused && let Some(child) = self.spawn(id, &launch, new.starting, new.size)? {
            pane.process = Process::Reading(child);
        }
        Ok(pane)
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
        let workspace = workspace.map_err(bad_value)?;
        self.attach_as(ClientKind::Terminal, rows, cols, workspace.as_ref())
    }

    /// `attach`, for a client of `kind`.
    pub fn attach_as(
        &mut self,
        kind: ClientKind,
        rows: u16,
        cols: u16,
        workspace: Option<&WsRef>,
    ) -> Result<ClientId, Error> {
        let ws = match workspace {
            Some(workspace) => self.resolve_ws(workspace)?,
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

    /// Gives a client a notice, an error's if `error`, if it is still
    /// attached.
    pub fn notify_to(&mut self, client: ClientId, text: impl Into<String>, error: bool) {
        if let Some(view) = self.views.get_mut(&client) {
            view.notify(text, error);
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
        fit.area(client.map_or(TYPICAL, Self::pane_area))
    }

    // -------------------------------------------------------------- repair

    /// Ends each view's mode if what it shows is gone, then sizes the PTYs:
    /// run after every event.
    pub fn settle(&mut self) {
        // Out of the session while they are repaired, which reads no view.
        let mut views = std::mem::take(&mut self.views);
        let ended: Vec<(ClientId, ScreenKind)> = (views.values_mut())
            .filter_map(|view| self.repair(view).map(|kind| (view.id, kind)))
            .collect();
        self.views = views;
        for (client, kind) in ended {
            self.outbox
                .push(Outgoing::To(client, ToAttached::Ended(kind)));
        }
        self.size_panes();
        self.hold_copies();
        self.pane_tabs.clear();
        for ws in &self.workspaces {
            for tab in ws.tabs() {
                if let Some(root) = tab.root() {
                    root.for_each_pane(&mut |pane| {
                        self.pane_tabs.insert(pane, tab.id);
                    });
                }
            }
        }
        self.edited = true;
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
    /// pane closed or is no longer focused, a client's screen about a pane,
    /// tab or workspace that closed. Which kind it ended, if it did, which
    /// its client is told (`ToAttached::Ended`), the mode being the client's.
    fn repair(&self, view: &mut View) -> Option<ScreenKind> {
        use crate::act::{List, Panel};
        let focus = self.focused(view.id);
        // Copy mode's rows are looked for once the panes are sized
        // (`hold_copies`).
        let (kind, gone) = match &view.mode {
            Mode::Copy(copy) if !self.panes.contains_key(&copy.pane) => (
                ScreenKind::Copy,
                "copy mode ended: its pane closed".to_owned(),
            ),
            Mode::Copy(copy) if Some(copy.pane) != focus => (
                ScreenKind::Copy,
                "copy mode ended: its pane is no longer focused".to_owned(),
            ),
            Mode::Panel(
                Panel::List(List {
                    about: Some(about), ..
                })
                | Panel::Prompt {
                    about: Some(about), ..
                }
                | Panel::Confirm { about, .. },
            ) if self.there(*about).is_err() => {
                (ScreenKind::Panel, format!("closed: {} is gone", about))
            }
            Mode::Normal | Mode::Panel(_) | Mode::Copy(_) => return None,
        };
        view.mode = Mode::Normal;
        view.notify(gone, true);
        Some(kind)
    }

    /// The settings changed: every pane keeps the history they say.
    fn configured(&mut self) {
        for pane in self.panes.values_mut() {
            pane.parser.set_history_bytes(self.config.history_bytes);
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
        let client = self.views.values().next().map_or(TYPICAL, Self::pane_area);
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
            let size = pane::size(rows, cols);
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
                    self.notify_to(*client, format!("{id} {} {why}", pane.label()), false);
                }
            }
            self.end(pane.process, closed);
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
                    self.end(gone.process, true);
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
    /// What a program wrote as it exited may still be on its terminal, or
    /// read and waiting for the pane's next turns (`Pane::has_unread`): it
    /// is told exited once all of it is on its screen, from the turn that
    /// reads the last of it (`Session::fire`).
    pub fn exited(&mut self, id: PaneId, ended: Ended) {
        let status = process::shell_status(ended);
        let program = (self.panes.get(&id)).filter(|p| matches!(p.runs, Runs::Program(_)));
        let program = program.map(|p| p.exiting.is_none());
        // Once, at the first word of the exit, its terminal drained (what
        // a program left writing there is not waited for), and the status
        // kept till the last of it is read; at each, a frame it holds read,
        // one begun in output read since too.
        if program == Some(true) {
            let left = (self.panes.get(&id))
                .map(|p| unread(&p.process))
                .unwrap_or_default();
            if !left.is_empty() {
                self.output(id, &left);
            }
        }
        if program.is_some() {
            self.read_with(id, Pane::release_frame);
        }
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        if program.is_some() {
            pane.exiting = Some(ended);
            if pane.has_unread() {
                return;
            }
        }
        match pane.runs {
            Runs::Shell => self.close_pane(id, Some(format!("exited with status {status}"))),
            Runs::Program(_) => {
                // Its leader, exited and unreaped, is reaped with the dying.
                let gone = std::mem::replace(&mut pane.process, Process::Exited(ended));
                if let Process::Reading(child) | Process::HungUp(child) = gone {
                    let deadline = Instant::now();
                    self.dying.push(Dying { child, deadline });
                }
                for client in self.viewers(self.locate(id).map(|(_, t)| t)) {
                    self.notify_to(client, format!("{id} {}", process::told(ended)), false);
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
            let frame = pane.frame_deadline().map(|at| (at, Timer::Frame(pane.id)));
            let unread = pane.unread_due().map(|at| (at, Timer::Unread(pane.id)));
            frame.into_iter().chain(unread)
        })
    }

    /// What `timer` waited for is done, now that it is due.
    pub fn fire(&mut self, timer: Timer) {
        match timer {
            Timer::Frame(id) => self.read_with(id, Pane::release_frame),
            Timer::Unread(id) => {
                self.read_with(id, Pane::read_unread);
                // A program that exited with output unread exits now, if
                // the last of it is read.
                let pane = self.panes.get(&id).filter(|p| !p.has_unread());
                if let Some(ended) = pane.and_then(|p| p.exiting) {
                    self.exited(id, ended);
                }
            }
        }
    }

    /// Reads into pane `id` with `read`, as every read of a pane's output
    /// does: its tab's host colours and palette given it first; after, its
    /// bell rung, and the views showing it repainted and told if a reply
    /// was dropped.
    fn read_with(&mut self, id: PaneId, read: impl FnOnce(&mut Pane)) {
        let tab = self.pane_tabs.get(&id).copied();
        let (colours, palette, graphics) = self.host_for(tab);
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        pane.colours = colours;
        pane.parser.set_host_palette(palette);
        pane.parser.set_host_graphics(graphics);
        read(pane);
        self.edited = true;
        let dropped = pane.input.lost_reply();
        let bell = std::mem::take(&mut pane.bell);
        let copied = pane.copied.take();
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
                view.notify(
                    format!("{id}'s input queue is full; a terminal reply was dropped"),
                    true,
                );
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
            self.end(pane.process, true);
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
            let outcome = self.run_command(command, origin);
            return Begun::Done(match outcome.end {
                End::Failed { .. } => Reply::default(),
                End::Done => outcome,
            });
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
        let wait = match request {
            Request::Do(command) => return Ok(Begun::Done(self.run_command(&command, origin))),
            Request::Wait(wait) => wait,
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
        let watches = match wait.since {
            command::Since::Now(target) => (self.panes_of(target)?.into_iter())
                .map(|id| Ok((id, self.pane(id)?.counts())))
                .collect::<Result<Vec<_>, Error>>()?,
            command::Since::Version(since) => vec![(since.pane, self.counts_since(since)?)],
        };
        // A wait on no pane would only time out.
        if watches.is_empty() {
            return Err(fail(Code::NoPane, "it holds no pane to wait on"));
        }
        let limit = wait.timeout.unwrap_or(self.config.wait);
        let waiting = Waiting::new(watches, self.run, now, limit);
        // Its answer, if it has one already; else the wait.
        Ok(match waiting.answer(self, now) {
            Some(outcome) => Begun::Done(outcome),
            None => Begun::Waiting(Box::new(waiting)),
        })
    }

    /// The counts a wait from `since` waits to differ from: its own, if it
    /// is a version of this server run's that its pane has had. A pane of
    /// this run that is gone closed, as ids are never given twice in one
    /// run, and the wait says so.
    fn counts_since(&self, since: command::Version) -> Result<command::Counts, Error> {
        if since.run != self.run {
            let why = format!(
                "{since} is from another server run ({}), not this one ({}): a server since stopped or restored, or another socket's",
                since.run, self.run
            );
            return Err(fail(Code::BadValue, why));
        }
        // Within one program every count only grows.
        if let Some(pane) = self.panes.get(&since.pane)
            && !pane.counts().had(since.counts)
        {
            let why = format!(
                "{since}: {} never had these counts; it is at {}",
                since.pane,
                pane.counts()
            );
            return Err(fail(Code::BadValue, why));
        }
        Ok(since.counts)
    }

    /// A request from a granted socket, if it may run: with no target, one
    /// that would act anywhere (`ls`, `run`, `new-tab`, `events`) acts on
    /// the granted workspace; one that names, or comes from, anything
    /// outside it, or reaches what is everyone's (the config, the buffers,
    /// clients, the server), is refused.
    fn scoped(&self, mut request: Request, scope: &str, origin: &Origin) -> Result<Request, Error> {
        // With no target, one that would act anywhere acts on the granted
        // workspace.
        let ws = self.resolve_ws(&WsRef::Name(scope.to_owned())).ok();
        if let Request::Do(command) = &mut request
            && let Command::Run {
                workspace: at @ None,
                ..
            }
            | Command::NewTab {
                target: at @ None, ..
            } = command
        {
            *at = Some(WsRef::Name(scope.to_owned()));
        }
        if let Request::Do(Command::DumpLayout { target: at @ None })
        | Request::Events(at @ None, _) = &mut request
        {
            // The granted workspace, which may be gone: then nothing.
            let gone = fail(
                Code::NotGranted,
                format!("workspace {scope} (grant) is gone"),
            );
            *at = Some(ws.map(AnyRef::Workspace).ok_or(gone)?);
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

    /// Runs a command, parsed already: from a line, a binding or a chooser.
    pub fn run_command(&mut self, command: &Command, origin: &Origin) -> Reply {
        let outcome = self
            .execute(command, origin)
            .unwrap_or_else(|error| failure(error, origin));
        self.ids.unwant();
        if !command.reaches().shows_nothing_new() {
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
            Command::Ls { target, get } => self.ls(*target, get.as_deref(), origin),
            &Command::KillServer { force } => self.kill_server(force, origin),
            Command::NewWorkspace {
                name,
                typed,
                size,
                cwd,
                env,
                ids,
            } => {
                let (cwd, fit, size) =
                    self.opening(*ids, cwd.as_deref(), *size, here, &starting)?;
                let new = NewPane {
                    start: Start::Shell(typed.as_deref(), env.as_slice()),
                    cwd: &cwd,
                    starting: &starting,
                    size,
                };
                let made = self.create_workspace(name.clone(), None, fit, new)?;
                self.follow(here, made.ws, None);
                Ok(made.reply(self.pane(made.pane)?.version(self.run)))
            }
            Command::NewTab {
                target,
                name,
                typed,
                size,
                cwd,
                env,
                ids,
            } => {
                let (cwd, fit, size) =
                    self.opening(*ids, cwd.as_deref(), *size, here, &starting)?;
                if let Some(name) = name {
                    self.check_tab_name(name, None)?;
                }
                let new = NewPane {
                    start: Start::Shell(typed.as_deref(), env.as_slice()),
                    cwd: &cwd,
                    starting: &starting,
                    size,
                };
                // A workspace named that is not there is made, as `run`
                // makes one, its first tab this one.
                let made = self.create_tab(target.as_ref(), here, None, fit, new)?;
                if let Some(name) = name {
                    self.give_tab_name(made.tab, name.clone())?;
                }
                let at = Spot {
                    tab: made.tab,
                    pane: None,
                };
                self.follow(here, made.ws, Some(at));
                Ok(made.reply(self.pane(made.pane)?.version(self.run)))
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
                let (dir, fit, _) = self.opening(*ids, cwd.as_deref(), *size, here, &starting)?;
                // Its place is out of every client's way: no client follows.
                let size = self.start_size(fit, Here::NOWHERE);
                // A name given is one `str NAME tab` finds: it must be the only one.
                if let Some(name) = name {
                    self.check_tab_name(name, None)?;
                }
                // Named after its program, if that makes a name.
                let derived = Some(pane::named(argv)).filter(|n| check_name(n).is_ok());
                let new = NewPane {
                    start: Start::Program(argv, env.as_slice()),
                    cwd: &dir,
                    starting: &starting,
                    size,
                };
                let workspace = (workspace.clone()).unwrap_or(WsRef::Name(RUN_WORKSPACE.into()));
                // Opened before the program starts, and emptied after: a run
                // refused leaves the file as it was.
                let log = (log.as_ref())
                    .map(|p| open_log(&self.cwd_for(here, None).join(p)))
                    .transpose()?;
                let tab_name = name.clone().or(derived);
                let made = self.create_tab(Some(&workspace), here, tab_name, fit, new)?;
                // Before any of its output is read: it all goes there. One
                // that can't be emptied stops what was started, before a
                // name it gives another tab is moved.
                let log = log.map(emptied).transpose();
                let log = log.inspect_err(|_| self.close_pane(made.pane, None))?;
                if let Some(given) = name {
                    self.give_tab_name(made.tab, given.clone())?;
                }
                if let Some(pane) = self.panes.get_mut(&made.pane) {
                    pane.log = log;
                }
                Ok(made.reply(self.pane(made.pane)?.version(self.run)))
            }
            &Command::Respawn {
                target,
                kill,
                ref env,
                ref argv,
            } => {
                self.respawn(target, kill, env.as_slice(), argv, &starting)?;
                Ok(acted([self.pane(target)?.version(self.run)]))
            }
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
                self.resize_tabs(target, size, here)?;
                Ok(Reply::default())
            }
            &Command::Resize(Resized::Client(client, size)) => {
                let view = self.views.get(&client).ok_or(Error::NoClient(client))?;
                if view.kind != ClientKind::Headless {
                    let message = format!(
                        "{client} has a terminal, which sets its size; resize sizes headless clients"
                    );
                    return Err(fail(Code::NotHeadless, message));
                }
                self.resize(client, size.rows(), size.cols());
                Ok(Reply::default())
            }
            Command::DumpLayout { target } => {
                let only = target.map(|t| self.there(t)).transpose()?;
                Ok(self.dump_layout(only, false).into())
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
                ref typed,
                ref cwd,
                ref env,
                id,
            } => {
                let start = match argv.is_empty() {
                    true => Start::Shell(typed.as_deref(), env.as_slice()),
                    false => Start::Program(argv, env.as_slice()),
                };
                self.split(target, axis, start, cwd.as_deref(), id, origin)
            }
            &Command::Signal { target, signal } => {
                let group = self.signalled(target)?;
                let version = self.pane(target)?.version(self.run);
                fuxix::process::kill_group(group, signal)
                    .map_err(|e| fail(Code::Signal, format!("sending the signal: {e}")))?;
                Ok(acted([version]))
            }
            &Command::Kill { target } => self.kill(target, origin),
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
            &Command::ResizePane { target, by } => self.resize_pane(target, by, here),
            Command::Log { target, file } => {
                self.pane(*target)?;
                let open = |f: &String| emptied(open_log(&self.cwd_for(here, None).join(f))?);
                let log = file.as_ref().map(open).transpose()?;
                self.pane_mut(*target)?.log = log;
                Ok(Reply::default())
            }
            &Command::ClearHistory { target } => {
                self.pane_mut(target)?.parser.clear_history();
                Ok(Reply::default())
            }
            &Command::SendKeys { target, ref inputs } => self.send_keys(target, inputs),
            &Command::CapturePane {
                target,
                history,
                view,
            } => {
                let pane = self.pane(target)?;
                if view.source == command::Source::LastOutput && !pane.marked() {
                    return Err(fail(Code::NoMarks, format!("{target}: {}", pane::UNMARKED)));
                }
                let copying = self.copying(pane.id);
                let (text, form) = crate::capture::pane(pane, self.run, history, view, copying);
                Ok(Reply {
                    json: form,
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
                    .map_err(|crate::config::Refused(code, why)| fail(code, why))?;
                self.configured();
                Ok(Reply::default())
            }
            Command::Resolve { text, kind } => {
                let found = self.resolve(text, *kind, here)?;
                Ok(Reply {
                    ids: vec![found],
                    ..format!("{found}\n").into()
                })
            }
            &Command::ListBuffers => {
                let line = |(i, text): (usize, &String)| {
                    let preview: String = (text.chars())
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .take(50)
                        .collect();
                    format!("{i}: {} bytes: {preview}\n", text.len())
                };
                Ok(self
                    .buffers
                    .iter()
                    .enumerate()
                    .map(line)
                    .collect::<String>()
                    .into())
            }
            &Command::ShowBuffer { index } => (self.buffers.get(index).cloned())
                .map(Reply::from)
                .ok_or(Error::NoBuffer(index)),
            &Command::PasteBuffer { index, target } => {
                let text = self.buffers.get(index).cloned();
                let paste = Input::Paste(text.ok_or(Error::NoBuffer(index))?);
                self.send_keys(AnyRef::Pane(target), &[paste])
            }
            &Command::Client { client, ref action } => self.client(client, action, here),
        }
    }

    /// What `new-workspace`, `new-tab` and `run` do before their pane
    /// starts: the ids asked for taken, the directory it starts in found,
    /// and its tab's fit and its size there.
    fn opening(
        &mut self,
        ids: Wanted,
        dir: Option<&Path>,
        size: Option<Size>,
        here: Here,
        starting: &Starting,
    ) -> Result<(PathBuf, Fit, Size), Error> {
        self.want(ids, starting.restoring)?;
        let cwd = self.start_dir(here, dir, None)?;
        let fit = size.map_or(Fit::Clients, Fit::Fixed);
        Ok((cwd, fit, self.start_size(fit, here)))
    }

    /// `ls`: everything there is, or `target` and what is in it, as text
    /// with its JSON kept beside it; or one field of the target's JSON.
    fn ls(
        &mut self,
        target: Option<AnyRef>,
        get: Option<&str>,
        origin: &Origin,
    ) -> Result<Reply, Error> {
        // Through a grant, the granted workspace alone.
        let scope = self.scope(origin)?;
        let scope = (scope.map(|name| self.resolve_ws(&WsRef::Name(name)))).transpose()?;
        let (read, only) = match target {
            Some(target) => {
                let target = self.there(target)?;
                let read = self.ls_one(target, get)?;
                if get.is_some() {
                    return Ok(read);
                }
                (read.text, Some(target))
            }
            None => (self.ls_json(scope), scope.map(AnyRef::Workspace)),
        };
        // A reading given as text keeps its JSON, made from the same read:
        // `json` converts what was read, never a later read.
        Ok(Reply {
            json: read,
            ..self.ls_text(only).into()
        })
    }

    /// `kill-server`: from the CLI, refused from one of the server's own
    /// panes or with people attached, unless `force`.
    fn kill_server(&mut self, force: bool, origin: &Origin) -> Result<Reply, Error> {
        if let (Origin::Cli(caller), false) = (origin, force) {
            let pane = caller.pane().filter(|p| self.panes.contains_key(p));
            if pane.is_some() || caller.within == Within::This {
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
                let message =
                    format!("{who} attached to this server; force-kill-server stops it anyway");
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

    /// `resize` of `target`'s tabs: laid out on `size`, or with none on
    /// their clients' screens. A size no pane could have, or one that would
    /// hide one of a tab's panes, as a split that would is, is refused.
    fn resize_tabs(&mut self, target: AnyRef, size: Option<Size>, here: Here) -> Result<(), Error> {
        if let Some(size) = size
            && !fux_vt::fits(size)
        {
            let message = format!("{}x{} is too big for a pane", size.cols(), size.rows());
            return Err(fail(Code::Size, message));
        }
        let fit = size.map_or(Fit::Clients, Fit::Fixed);
        let tabs = self.tabs_of(self.there(target)?);
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
        let pane = p.id;
        if x10 && beyond_x10 {
            let message = format!(
                "the program in {pane} asked for X10 mouse reports: presses alone, no releases or wheel"
            );
            return Err(fail(Code::Mouse, message));
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
            return Err(fail(Code::Mouse, message));
        }
        let version = p.version(run);
        p.input.push(report)?;
        p.mouse_held = held;
        Ok(acted([version]))
    }

    /// `split`: a new pane beside `target`, along `axis`, running `start`
    /// in `dir`, with the id `id` asked for.
    fn split(
        &mut self,
        target: PaneId,
        axis: Axis,
        start: Start<'_>,
        dir: Option<&Path>,
        id: Option<PaneId>,
        origin: &Origin,
    ) -> Result<Reply, Error> {
        let (here, starting) = (self.here(origin)?, self.starting_for(origin));
        let wanted = Wanted {
            pane: id,
            ..Wanted::default()
        };
        self.want(wanted, starting.restoring)?;
        let target = self.pane(target)?.id;
        let cwd = self.start_dir(here, dir, Some(target))?;
        // The splitting client follows the new pane; from the CLI, the
        // clients that focus the split one.
        let shown = |c: &ClientId| self.focused(*c) == Some(target);
        let followers: Vec<ClientId> = match here {
            Here::Client(client, _) => vec![client],
            Here::Cli(..) => self.views.keys().copied().filter(shown).collect(),
        };
        // Its tab found (by `split_size`) before the program starts, which
        // nothing can then leave running.
        let size = self.split_size(target, axis, here)?;
        let mut ids = self.ids;
        let new = NewPane {
            start,
            cwd: &cwd,
            starting: &starting,
            size,
        };
        let pane = self.new_pane(&mut ids, new)?;
        let tab = tab_of(&mut self.workspaces, target)?;
        self.ids = ids;
        let new = pane.id;
        tab.edit(|root| {
            root.as_mut()
                .map(|r| r.split(target, new, axis, Side::After))
        });
        self.panes.insert(new, pane);
        for view in (self.views.values_mut()).filter(|v| followers.contains(&v.id)) {
            tab.set_focus(view.id, new);
            view.zoom = false;
        }
        let (ws, tab) = self.locate(new).ok_or(Error::NoPane(new))?;
        let version = self.pane(new)?.version(self.run);
        Ok(Made { ws, tab, pane: new }.reply(version))
    }

    /// `kill`: from the command line it says what it closed, so a script or
    /// an agent knows; a key's close shows on the screen anyway.
    fn kill(&mut self, target: AnyRef, origin: &Origin) -> Result<Reply, Error> {
        // What may go with it, said once gone: a tab's or a workspace's
        // panes; and what holds it, if it was the last there: a pane's tab
        // and that tab's workspace, a tab's workspace.
        let mut may_go = vec![self.there(target)?];
        let holders = match target {
            AnyRef::Pane(pane) => (self.locate(pane)).map_or([None, None], |(w, t)| {
                [Some(AnyRef::Tab(t)), Some(AnyRef::Workspace(w))]
            }),
            AnyRef::Tab(tab) => [self.tab_workspace(tab).ok().map(AnyRef::Workspace), None],
            AnyRef::Workspace(_) => [None, None],
        };
        match target {
            AnyRef::Pane(pane) => self.close_pane(pane, None),
            AnyRef::Tab(_) | AnyRef::Workspace(_) => {
                // A tab there with no panes (moved out) has none to go.
                let panes = self.panes_of(target).unwrap_or_default();
                may_go.extend(panes.into_iter().map(AnyRef::Pane));
                for tab in self.tabs_of(target) {
                    self.remove_tab(tab);
                }
                self.after_close();
            }
        }
        may_go.extend(holders.into_iter().flatten());
        let gone: Vec<AnyRef> = (may_go.into_iter())
            .filter(|it| self.there(*it).is_err())
            .collect();
        Ok(closed(origin, &gone).into())
    }

    /// `resize-pane`: a border of pane `pane` moved, or the pane made a
    /// size, its tab laid out as for the command.
    fn resize_pane(&mut self, pane: PaneId, by: PaneResize, here: Here) -> Result<Reply, Error> {
        let (_, tab) = self.locate(pane).ok_or(Error::NoPane(pane))?;
        let area = self.reference_area(tab, here);
        match by {
            PaneResize::Border(direction, amount) => {
                let tab = tab_of(&mut self.workspaces, pane)?;
                let resize = |root: &mut Option<Tree>| {
                    let root = root.as_mut();
                    root.is_some_and(|root| layout::resize(root, area, pane, direction, amount))
                };
                if !tab.edit(resize) {
                    let message = format!("{pane} has no border to move {}", direction.name());
                    return Err(fail(Code::NothingThere, message));
                }
                Ok(Reply::default())
            }
            PaneResize::To(axis, extent) => {
                (self.resize_pane_to(pane, area, axis, extent)).map(Reply::from)
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
                    Code::NoTarget,
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
            // Taken as typed at its terminal: its bindings see them.
            ClientAction::Keys(ref inputs) => {
                let keys = ToAttached::Keys(inputs.clone());
                self.outbox.push(Outgoing::To(client, keys));
                return Ok(Reply::default());
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
                let (text, form) = crate::capture::client(client, &grid);
                return Ok(Reply {
                    json: form,
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
                ws.add_tab(id, None, None, Fit::Clients);
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
                let first = Tab::new(tab, MAIN.into(), None, Fit::Clients, clients);
                self.workspaces.push(Workspace::new(id, name, first));
                (id, tab)
            }
        };
        if self.locate(pane).is_some_and(|(_, source)| source == tab) {
            return Ok(String::new());
        }
        self.relocate(pane, tab);
        // The moving client follows its pane.
        let pane = Some(pane);
        self.follow(here, ws, Some(Spot { tab, pane }));
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
                let index = tabs.iter().position(|t| t.id == at.tab);
                let next = round(tabs, index.unwrap_or(0), toward);
                (at.ws, next.ok_or(Error::OnlyOne(Kind::Tab))?.id)
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
                let workspaces = &self.workspaces;
                let index = workspaces.iter().position(|w| w.viewers.contains(&view.id));
                let next = round(workspaces, index.unwrap_or(0), *toward);
                next.ok_or(Error::OnlyOne(Kind::Workspace))?.id
            }
        };
        Ok(Picked { ws, at: None })
    }

    // ------------------------------------------------------------------ ls

    /// `ls`: everything, or with `only` that pane, tab or workspace and
    /// what is in it, a line each.
    fn ls_text(&self, only: Option<AnyRef>) -> String {
        let mut out = String::new();
        let listed_pane = |pane: PaneId| !matches!(only, Some(AnyRef::Pane(p)) if p != pane);
        for ws in &self.workspaces {
            if only.is_none_or(|o| o == AnyRef::Workspace(ws.id)) {
                out.push_str(&format!("{} {}\n", ws.id, ws.name));
            }
            for tab in ws
                .tabs()
                .iter()
                .filter(|t| self.takes_in(only, ws.id, t.id))
            {
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
                            (None, Some(ended)) => format!(" {}", process::told(ended)),
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
        fields.extend(crate::capture::state(p, self.run, now, self.copying(p.id)));
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
    /// of its JSON (`get`), a string's text as it is. `target` is there
    /// (`there`, by its caller).
    fn ls_one(&mut self, target: AnyRef, get: Option<&str>) -> Result<Reply, Error> {
        let now = Instant::now();
        let mut fields = match (target, get) {
            // A pane's version alone, which the recipes read after every
            // change: not the rest of its state (its foreground program's
            // name, argv and cwd).
            (AnyRef::Pane(id), Some("version")) => {
                let version = |p: &Pane| p.version(self.run).to_string();
                (self.panes.get(&id)).map(|p| vec![("version", Json::str(version(p)))])
            }
            (AnyRef::Pane(id), _) => self.panes.get(&id).map(|p| self.pane_json(p, now)),
            (AnyRef::Tab(id), _) => self.tab(id).map(|t| self.tab_json(t, now)),
            (AnyRef::Workspace(id), _) => self.workspace(id).map(|w| self.ws_json(w, now)),
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
        let Some(grant) = self.grants.iter().find(|g| g.socket == *socket) else {
            let message = "the grant this socket came through was revoked";
            return Err(fail(Code::NotGranted, message));
        };
        if let Some(ws) = grant.pinned.and_then(|id| self.workspace(id)) {
            return Ok(Some(ws.name.clone()));
        }
        let name = grant.workspace.clone();
        let found = self.resolve_ws(&WsRef::Name(name.clone())).ok();
        if let Some(grant) = self.grants.iter_mut().find(|g| g.socket == *socket) {
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
            WsRef::Id(id) => (self.workspace(*id).ok_or(Error::NoWorkspace(*id))?.name).clone(),
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
    Shell(Option<&'a str>, &'a [(String, String)]),
    /// A program, run in place of a shell, with variables set for it.
    Program(&'a [String], &'a [(String, String)]),
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
    no_tab_named(others, name, None, |_| false)
}

/// That no tab of `workspaces` but `except` was given the name `name`; else
/// why not, with those tabs and their panes, and whether one is `paused`
/// (a restore brought it back).
fn no_tab_named<'a>(
    workspaces: impl IntoIterator<Item = &'a Workspace>,
    name: &str,
    except: Option<TabId>,
    paused: impl Fn(&PaneId) -> bool,
) -> Result<(), Error> {
    let tabs: Vec<&Tab> = (workspaces.into_iter())
        .flat_map(Workspace::tabs)
        .filter(|t| t.given && t.name == name && Some(t.id) != except)
        .collect();
    if tabs.is_empty() {
        return Ok(());
    }
    // `run` with such a name: `str NAME tab` would not know which, so the
    // program is not started. The tabs, then their panes.
    let panes = (tabs.iter()).flat_map(|t| t.root().map(Tree::panes).unwrap_or_default());
    let tabs = tabs.iter().map(|t| t.id).collect();
    let panes: Vec<PaneId> = panes.collect();
    // One a restore brought back: the surprise is said.
    let restored = panes.iter().any(paused);
    Err(Error::TabNameTaken(name.to_owned(), tabs, panes, restored))
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
            shell: crate::config::Shell::parse("/bin/sh").map_err(|e| e.1)?,
            ..config
        };
        let run = crate::command::Run::now().ok_or("no clock")?;
        let mut session = Session::new(config, "/nonexistent/fux.sock".into(), false, run);
        session.start().map_err(|e| e.to_string())?;
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
                super::Begun::Waiting(waiting) => {
                    waiting.answer(session, waiting.due()).unwrap_or_default()
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
        let (out, ended) = run_as(session, client, code);
        let ended = ended.map_err(|e| e.to_string())?;
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
        code: &str,
    ) -> (
        Vec<u8>,
        Result<Vec<fuxth::Value<crate::script::Fux>>, crate::script::Failed>,
    ) {
        let mut out = Vec::new();
        let tokens = match fuxth::token::source("test", code) {
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
                    | Request::Events(..)
                    | Request::Grant(..)
                    | Request::Revoke(_) => return self.0.ask(asked, out),
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
                let open = keys.screen.kind().is_some();
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
    use super::testing::{attached, output, requests, run, started};
    use super::*;

    /// A run refused (its name is taken, its workspace is not there or
    /// can't be made) leaves the log file it names as it was, its contents
    /// and its mode: the file is emptied and made the user's alone only for
    /// a run that starts.
    #[test]
    fn a_refused_run_leaves_its_log_alone() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let mut s = started(Config::default())?;
        let log = fuxix::file::scratch()?.join(format!("fux-log-{}", std::process::id()));
        let line = |opts: &str| {
            let log = log.display();
            format!("opts {opts} str '{log}' log-to str /bin/cat program run")
        };
        run(&mut s, &line("str a name"))?;
        // The one that starts makes it the user's alone.
        assert_eq!(std::fs::metadata(&log)?.permissions().mode() & 0o777, 0o600);
        std::fs::write(&log, "kept")?;
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o640))?;
        let refused = [
            (line("str a name"), "name_taken"),
            (line("str +9 in"), "no_workspace"),
            (line("str %9 in"), "bad_name"),
            (line("str a in"), "name_taken"),
        ];
        let kept = refused.map(|(line, code)| {
            let failed = run(&mut s, &line).is_err_and(|e| e.contains(code));
            let mode = std::fs::metadata(&log).map(|m| m.permissions().mode() & 0o777);
            (code, failed, std::fs::read_to_string(&log).ok(), mode.ok())
        });
        std::fs::remove_file(&log)?;
        for (code, failed, text, mode) in kept {
            assert!(failed, "{code}");
            assert_eq!(text.as_deref(), Some("kept"), "{code}");
            assert_eq!(mode, Some(0o640), "{code}");
        }
        Ok(())
    }

    /// A run whose log can't be readied (procfs refuses a file's mode
    /// change) fails, and leaves no program of its own running.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_run_whose_log_fails_leaves_nothing_running() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let panes = s.panes.len();
        let ran = run(
            &mut s,
            "opts str /proc/self/comm log-to str /bin/cat program run",
        );
        assert!(ran.is_err_and(|e| e.contains("log")), "refused");
        assert_eq!(s.panes.len(), panes);
        // Refused before it started: its pane's id was never taken.
        run(&mut s, "opts str /bin/cat program run")?;
        assert!(
            s.panes.contains_key(&PaneId::of(2)),
            "the next run's pane is %2"
        );
        Ok(())
    }

    /// A log whose mode only another user may set (`/dev/null`, root's)
    /// refuses the run before anything starts: no pane is made for it, so
    /// the next run's pane is the next id (ids are never given twice).
    #[test]
    fn a_log_another_user_owns_refuses_the_run_first() -> Result<(), String> {
        if fuxix::process::geteuid() == 0 {
            return Ok(());
        }
        let mut s = started(Config::default())?;
        let ran = run(&mut s, "opts str /dev/null log-to str /bin/cat program run");
        assert!(ran.is_err_and(|e| e.contains("log")), "refused");
        run(&mut s, "opts str /bin/cat program run")?;
        assert!(
            s.panes.contains_key(&PaneId::of(2)),
            "the next run's pane is %2"
        );
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

    /// A pane respawned twice within its program's grace has both programs
    /// before dying: the second end never drops the first one's kill.
    #[test]
    fn a_pane_respawned_twice_in_its_grace_kills_both() -> Result<(), String> {
        let mut s = started(Config::default())?;
        run(&mut s, "opts str /bin/cat program run")?;
        // A test's session starts no program: each is put in the pane.
        let argv = ["/bin/cat".to_owned()];
        for _ in 0..2 {
            let child = process::spawn(&argv, Path::new("/"), (&[], &[]), pane::size(4, 20))?;
            let pane = s.panes.get_mut(&PaneId::of(2)).ok_or("the pane")?;
            pane.process = Process::Reading(child);
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
        run(&mut s, "opts str /bin/true program run")?;
        let id = PaneId::of(2);
        // /bin/sh, which macOS has where it has no /bin/true.
        let argv = ["/bin/sh", "-c", "exit 0"].map(str::to_owned);
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), pane::size(4, 20))?;
        (s.panes.get_mut(&id).ok_or("the pane")?).process = Process::Reading(child);
        let exited = |s: &Session| {
            (s.panes.get(&id))
                .and_then(|p| p.process.child()?.leader.exited())
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
        let pane = s.panes.get(&id).ok_or("the pane")?;
        assert!(pane.has_unread(), "the budget stopped no call");
        assert!(
            pane.process.exit_status().is_none(),
            "exited with output unread"
        );
        let mut turns = 0u32;
        while s.panes.get(&id).is_some_and(Pane::has_unread) && turns < 100 {
            s.fire(crate::session::Timer::Unread(id));
            turns = turns.saturating_add(1);
        }
        let pane = s.panes.get(&id).ok_or("the pane")?;
        assert_eq!(pane.process.exit_status(), Some(Ended::Exited(0)));
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
        run(&mut s, "opts str /bin/true program run")?;
        let id = PaneId::of(2);
        let argv = ["/bin/sh", "-c", "while :; do printf '\\033[2J'; done"].map(str::to_owned);
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), pane::size(4, 20))?;
        (s.panes.get_mut(&id).ok_or("the pane")?).process = Process::Reading(child);
        let rows = s.panes.get(&id).map(|p| p.screen().size().rows());
        assert!(rows.is_some_and(|r| r >= 100), "{rows:?} rows");
        std::thread::sleep(Duration::from_millis(200));
        let exited = |s: &Session| {
            s.panes
                .get(&id)
                .is_some_and(|p| p.process.exit_status().is_some())
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
        if let Some(child) = s.panes.get_mut(&id).and_then(|p| {
            match std::mem::replace(&mut p.process, Process::Absent) {
                Process::Reading(child) | Process::HungUp(child) => Some(child),
                Process::Absent | Process::Exited(_) => None,
            }
        }) {
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
        run(&mut s, "opts str /bin/true program run")?;
        let id = PaneId::of(2);
        s.output(id, b"\x1b[?2026hHELD");
        s.exited(id, Ended::Exited(0));
        let pane = s.panes.get(&id).ok_or("the pane")?;
        assert_eq!(pane.process.exit_status(), Some(Ended::Exited(0)));
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
        run(&mut s, "opts str /bin/true program run")?;
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
            .panes
            .get(&id)
            .is_some_and(|p| p.process.exit_status().is_none())
            && turns < 100
        {
            s.fire(crate::session::Timer::Unread(id));
            turns = turns.saturating_add(1);
        }
        let pane = s.panes.get(&id).ok_or("the pane")?;
        assert_eq!(pane.process.exit_status(), Some(Ended::Exited(0)));
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
        let second = s.panes.get_mut(&PaneId::of(2)).ok_or("no %2")?;
        // Its program reads nothing: the queue refuses everything now.
        assert!(second.input.push(vec![0; 1 << 30]).is_err());
        let sent = run(&mut s, "@1 str x text send-keys");
        assert!(sent.is_err_and(|e| e.contains("input_full")), "refused");
        let first = s.panes.get(&PaneId::of(1)).ok_or("no %1")?;
        assert_eq!(first.input.len(), 0, "the first pane got none");
        // Keys too many for the room left are refused, and so is a key
        // after them, until the program reads: none lands past a hole.
        let pane = s.panes.get_mut(&PaneId::of(1)).ok_or("no %1")?;
        let room = pane.input.room();
        pane.input
            .push(vec![0; room.saturating_sub(4)])
            .map_err(|e| e.to_string())?;
        assert!(run(&mut s, "%1 str 12345678 text send-keys").is_err());
        assert!(
            run(&mut s, "%1 str x text send-keys").is_err(),
            "a key after a refusal"
        );
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
        assert_eq!(size(&s, 2), Some(crate::pane::size(10, 40)));
        Ok(())
    }

    /// `ls` of a workspace lists it whole; of a tab, that tab; of a pane,
    /// its tab with that pane alone.
    #[test]
    fn ls_of_a_target_lists_what_is_in_it() -> Result<(), String> {
        let (mut s, _) = attached(11, 40)?;
        run(&mut s, "opts str +1 workspace in new-tab")?;
        run(&mut s, "str %2 pane vertical split")?;
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
        let capture = output(&mut s, "str %2 pane capture json")?;
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
        run(&mut s, "str %1 pane horizontal split")?;
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
        let ws = s.workspaces.first().map(|w| w.id).ok_or("a workspace")?;
        let Begun::Waiting(waiting) = s.begin(wait(AnyRef::Workspace(ws)), &origin) else {
            return Err("the workspace's wait answered at once".into());
        };
        assert!(waiting.answer(&s, Instant::now()).is_none());
        s.output(PaneId::of(2), b"E");
        let answer = waiting.answer(&s, Instant::now()).ok_or("no answer")?;
        assert!(
            answer.text.starts_with("%2:") && !answer.text.contains("%1"),
            "{}",
            answer.text
        );
        let Begun::Waiting(waiting) = s.begin(wait(AnyRef::Pane(PaneId::of(1))), &origin) else {
            return Err("the pane's wait answered at once".into());
        };
        run(&mut s, "str %1 pane kill")?;
        let answer = waiting.answer(&s, Instant::now()).ok_or("no answer")?;
        assert_eq!(answer.end.code(), Some(Code::Closed), "{}", answer.text);
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
            timeout: Some(Duration::ZERO),
        });
        let Begun::Waiting(waiting) = s.begin(wait, &Origin::Cli(Caller::default())) else {
            return Err("a wait mid-frame answered at once".into());
        };
        while s.pane(PaneId::of(1))?.mid_frame() {
            assert!(waiting.answer(&s, waiting.due()).is_none());
            s.fire(Timer::Unread(PaneId::of(1)));
        }
        let answer = waiting.answer(&s, waiting.due()).ok_or("no answer")?;
        assert!(answer.text.ends_with(" screen\n"), "{}", answer.text);
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
            format!("%2:0.0.0.0.0:{}\n", s.run)
        );
        // +3 would be workspace-3, which is taken.
        let third = format!("%3:0.0.0.0.0:{}\n", s.run);
        assert_eq!(output(&mut s, "opts new-workspace")?, third);
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

    /// What no command may do is refused: a name a target would read as
    /// an id, or blank, or too long, or taken; a move with nowhere to go.
    #[test]
    fn what_cannot_be_done_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        let long = format!(
            "str %1 pane str {} rename",
            std::iter::repeat_n("x", 257).collect::<String>()
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
            Some("nothing_there: test:1: choose-pane: only one pane")
        );
        run(&mut s, "str c1 client choose-tab")?;
        assert!(matches!(
            s.views.get(&c).map(|v| &v.mode),
            Some(Mode::Panel(crate::act::Panel::List(_)))
        ));
        Ok(())
    }

    /// A bare id names its pane, tab or workspace, but not as `str`'s text;
    /// `json` after a capture gives its JSON; `command` splits a line.
    #[test]
    fn ids_json_and_command_lines_read_as_written() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, _) = attached(10, 40)?;
        assert_eq!(output(&mut s, "%1 str name get")?, "sh\n");
        assert_eq!(output(&mut s, "@1 str name get")?, "main\n");
        // A pane's title names it too, as `ls` shows it.
        s.output(PaneId::of(1), b"\x1b]2;spool\x07");
        assert_eq!(output(&mut s, "str spool pane")?, "%1\n");
        assert_eq!(output(&mut s, "str %1")?, "%1\n");
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.starts_with(r#"{"pane":"%1""#), "{json}");
        let made = output(&mut s, "opts str 'sh -c \"exit 0\"' command run")?;
        assert!(made.starts_with('%'), "{made}");
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
            let after = json.split("\"cursor\":").nth(1).unwrap_or_default();
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
        let times = failed(&mut s, "%1 str a text str 2 times");
        assert!(times.starts_with("type: "), "{times}");
        // Refused by `times`, before it is repeated, past what send-keys
        // sends.
        let many = failed(&mut s, "%1 str x 100000 copies text 1000 times send-keys");
        assert!(many.starts_with("too_long: test:1: times: "), "{many}");
        let none = failed(&mut s, "%1 str a text 0 times send-keys");
        assert!(
            none.contains("0 is out of range: 1 to 1000 (use: "),
            "{none}"
        );
        // The core's words say how they are used too; a library word by
        // its own name.
        for code in ["slice", "str a 1 add"] {
            let core = failed(&mut s, code);
            assert!(core.contains(" (use: "), "{core}");
        }
        let upper = failed(&mut s, "3 upper");
        assert!(upper.contains("(use: S upper"), "{upper}");
        // A capture's option given what is no pane says where it goes.
        for (code, shown) in [
            ("%1 capture last-output", "fux %3 last-output capture"),
            ("%1 capture 0 4 rows", "fux %3 0 4 rows capture"),
        ] {
            let option = failed(&mut s, code);
            assert!(
                option.contains("(use: a capture option, PANE-OR-OPTS "),
                "{option}"
            );
            assert!(option.contains(&format!("; e.g. {shown})")), "{option}");
        }
        let env = failed(&mut s, "opts str 1bad str v env new-tab");
        assert!(env.contains("not first a digit"), "{env}");
        let missing = failed(&mut s, "str /no/such/file.fux check");
        assert!(missing.contains("No such file"), "{missing}");
        let split = failed(&mut s, "str 'two\nlines' fail");
        assert!(split.ends_with(r"two\nlines"), "{split}");
        let version = failed(&mut s, "str 1.2.3 changed wait");
        assert!(version.contains("is no version"), "{version}");
        Ok(())
    }

    /// A saved session names only the tabs given a name: one fux named by
    /// its place (`tab-3`, its place now 2) comes back named by its place
    /// again, not as a name given, which a tab named so later could not
    /// take.
    #[test]
    fn a_dump_names_only_the_tabs_given_a_name() -> Result<(), String> {
        let mut s = started(Config::default())?;
        let tab = "opts str +1 workspace in new-tab drop";
        run(&mut s, &format!("{tab} {tab} str @2 tab kill"))?;
        run(&mut s, &format!("{tab} str @4 tab str given rename"))?;
        let dump = s.dump_layout(None, true);
        assert!(!dump.contains("tab-3"), "{dump}");
        assert!(dump.contains("given"), "{dump}");
        Ok(())
    }

    /// A kill from the command line names all it closed: a tab's
    /// workspace, when it was the last tab, as a pane's.
    #[test]
    fn a_kill_names_the_workspace_its_last_tab_took() -> Result<(), Box<dyn std::error::Error>> {
        let mut s = started(Config::default())?;
        let cli = Origin::Cli(Caller::default());
        let kill = |s: &mut Session, target| {
            (s.execute(&Command::Kill { target }, &cli)).map(|reply| reply.text)
        };
        run(&mut s, "opts str two name new-workspace drop")?;
        assert_eq!(
            kill(&mut s, AnyRef::Tab("@2".parse()?))?,
            "closed @2 %2 +2\n"
        );
        run(&mut s, "opts str three name new-workspace drop")?;
        assert_eq!(
            kill(&mut s, AnyRef::Pane(PaneId::of(3)))?,
            "closed %3 @3 +3\n"
        );
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
            Err(e) if e.code() == Code::BadValue && e.to_string().contains("+x")
        ));
        Ok(())
    }
}

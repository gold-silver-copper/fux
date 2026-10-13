//! The server's state: workspaces, tabs, panes and the clients' views, and
//! every command that changes them.
use crate::command::{
    self, AnyRef, ClientAction, Command, Extent, Input, Kind, Mouse, MoveTo, PanePick, PaneResize,
    Pick, Resized, Sibling, Subject, SwapWith, WsRef,
};
use crate::config::Config;
use crate::copy::MAX_CELLS;
use crate::id::{AnyId, ClientId, Ids, PaneId, TabId, WsId};
use crate::json::Json;
use crate::keys::Direction;
use crate::layout::{self, Axis, Placement, Rect, Side, Tree};
use crate::pane::{InputQueue, Pane, Process, Runs, Typed};
use crate::process::Child;
use crate::view::{ClientKind, Mode, View};
use crate::workspace::{Fit, Seat, Tab, Workspace};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
mod layouts;
use std::time::{Duration, Instant};

/// What the server must do outside the session.
#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// Bytes for a client's terminal outside any paint: a copy (OSC 52),
    /// questions for the terminal, the bell, the keyboard flags pushed.
    Bytes(ClientId, Vec<u8>),
    /// Detach a client, telling it why.
    Exit(ClientId, String),
    /// fuxth for a client to run as its own, typed at its command prompt.
    Run(ClientId, String),
    /// Keys for a client to take as typed at its terminal (`send-keys` to
    /// a client): its bindings see them.
    Keys(ClientId, Vec<command::Input>),
    /// Tell a client the server ended its screen by itself.
    Ended(ClientId),
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
#[derive(Clone, Debug, Default)]
pub enum Origin {
    /// A client's key, prompt or menu: where the client is.
    Client(ClientId),
    /// The CLI: who runs the command, and from where.
    Cli(Caller),
    /// Nowhere: no client and no caller.
    #[default]
    Nowhere,
}

/// A command line's caller on the CLI.
#[derive(Clone, Debug, Default)]
pub struct Caller {
    /// Its pane (`FUX_PANE`), if it runs in one of this server's.
    pub pane: Option<PaneId>,
    /// Its working directory.
    pub cwd: Option<PathBuf>,
    /// The `FUX_PANE` it gave of another server's pane (its `FUX_SERVER`
    /// is not this server's), which nothing here is done to.
    pub stray: Option<String>,
    /// Its environment, of which panes it starts take what
    /// `update-environment` names.
    pub env: Vec<(String, String)>,
    /// The workspace it may reach, and nothing else, if it came through a
    /// socket `fux grant` made.
    pub scope: Option<String>,
    /// It runs in one of this server's panes (its `FUX_SERVER` is this
    /// server's), whether or not it gave the pane.
    pub inside: bool,
    /// It brings back a saved session: the ids asked for are the ones it
    /// had, and programs wait for `respawn`.
    pub restoring: bool,
    /// The socket of the grant it came through: what panes it starts are
    /// told to reach the server by, so that what runs there is held to the
    /// grant too.
    pub grant: Option<PathBuf>,
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
    /// The caller's `FUX_PANE`, a pane of another server.
    StrayPane(String),
    /// A `-t NAME` that names no tab, or several: why.
    NotNamed(String),
    /// A command through a socket whose grant was revoked.
    Revoked,
    /// A line of a layout file that could not be made: its number, and why.
    LayoutLine(usize, String),
    /// A layout file that could not be read.
    LayoutFile(PathBuf, String),
    /// `--id` asked for an id given before, or one there is.
    IdTaken(String),
    /// `--id` of a kind the command makes none of, or two of one kind.
    IdKind(String),
    /// `--id` so far ahead that ids would run out after it.
    IdTooHigh(String),
    /// A command from a granted socket that names something outside its
    /// workspace.
    OutOfScope {
        what: String,
        scope: String,
    },
    /// A command from a granted socket that reaches what is not its.
    NotGranted {
        what: &'static str,
        scope: String,
    },
    /// `ls --get` of a field the target has not: those it has.
    NoField {
        target: String,
        field: String,
        fields: String,
    },
    /// `kill-server` from the CLI, run in one of the server's own panes.
    OwnServer(Option<PaneId>),
    /// `kill-server` from the CLI, with people's terminals attached.
    PeopleAttached(usize),
    /// A kill from the CLI that names nothing, which would close the
    /// caller's own pane, tab or workspace: its usage.
    KillNeedsTarget(&'static str),
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
    /// It asked for clicks, not for motion (with no button held, or at all).
    NoMotion(PaneId),
    /// It asked for X10 reports: presses alone.
    OnlyPresses(PaneId),
    /// A cell the encoding it asked for cannot say (legacy: past 223).
    MouseUnencodable {
        pane: PaneId,
        row: u16,
        col: u16,
    },
    /// A size no pane can have, with this much history.
    TooBig(fux_vt::Size, usize),
    /// A size a tab's panes cannot all have.
    TooSmall(TabId, usize),
    /// A name several tabs have, and no workspace.
    Named(String),
    /// A split that leaves no room for the new pane.
    NoRoom(PaneId),
    /// A cell outside the pane.
    OutsidePane {
        pane: PaneId,
        rows: u16,
        cols: u16,
    },
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
    /// `run -n NAME` where a tab has the name: `-t NAME` would not know
    /// which, so the program is not started. The tab and its panes.
    TabNameTaken(String, String),
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
    Config(crate::config::Error),
    Reload(crate::config::Error),
}

impl Error {
    /// What kind of failure it is, as `--json` names it: stable, for a
    /// script to tell failures apart by.
    pub fn code(&self) -> &'static str {
        match self {
            Error::NoWorkspace(_) | Error::NoWorkspaceNamed(_) | Error::NoWorkspaces => {
                "no_workspace"
            }
            Error::NoPane(_) => "no_pane",
            Error::NoTab(_) => "no_tab",
            Error::NoPaneGiven
            | Error::NoTabGiven
            | Error::NoWorkspaceGiven
            | Error::NoClientGiven
            | Error::KillNeedsTarget(_) => "no_target",
            Error::StrayPane(_) => "stray_pane",
            Error::NoField { .. } => "no_field",
            Error::OutOfScope { .. } | Error::NotGranted { .. } | Error::Revoked => "not_granted",
            Error::NotNamed(_) => "no_target",
            Error::IdTaken(_) | Error::IdKind(_) | Error::IdTooHigh(_) => "id",
            Error::LayoutLine(..) | Error::LayoutFile(..) => "layout",
            Error::OwnServer(_) => "own_server",
            Error::PeopleAttached(_) => "people_attached",
            Error::NoClient(_) => "no_client",
            Error::NotHeadless(_) => "not_headless",
            Error::NoBuffer(_) => "no_buffer",
            Error::NoConfigFile => "no_config",
            Error::NoLastPane | Error::NoPaneToCopy | Error::TabEmpty => "no_pane",
            Error::OnlyOne(_)
            | Error::AtEnd(_)
            | Error::NoBorder { .. }
            | Error::NoNeighbor { .. } => "nothing_there",
            Error::OnlyShell(_) => "only_shell",
            Error::Exited { .. } => "exited",
            Error::NoMouse(_)
            | Error::NoMotion(_)
            | Error::OnlyPresses(_)
            | Error::MouseUnencodable { .. }
            | Error::OutsidePane { .. } => "mouse",
            Error::TooBig(..)
            | Error::TooSmall(..)
            | Error::NoRoom(_)
            | Error::CannotResize { .. } => "size",
            Error::Named(_) => "ambiguous",
            Error::WaitsOnlyFromCli => "cli_only",
            Error::NotADirectory(_) => "not_a_directory",
            Error::Log(..) => "log",
            Error::Signal(_) => "signal",
            Error::NotRun(_) => "not_run",
            Error::StillRunning(_) => "still_running",
            Error::NoRows | Error::SelectionTooLarge => "selection",
            Error::EmptyName
            | Error::ControlInName
            | Error::LongName
            | Error::NameReadAsTarget(_) => "bad_name",
            Error::NameTaken(_) | Error::TabNameTaken(..) => "name_taken",
            Error::IdsExhausted(_) => "ids_exhausted",
            Error::LineTooLong => "too_long",
            Error::Usage(_) | Error::Words(_) => "usage",
            Error::Pane(crate::pane::Error::NotReading) => "not_reading",
            Error::Pane(_) | Error::Process(_) => "start",
            Error::Config(_) | Error::Reload(_) => "config",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoWorkspace(id) => write!(f, "no workspace {id}"),
            Error::NoWorkspaceNamed(name) => write!(f, "no workspace named {name:?}"),
            Error::NoPaneGiven => f.write_str("no pane given: name one (`str %N pane`)"),
            Error::NoPane(id) => write!(f, "no pane {id}"),
            Error::NoTabGiven => f.write_str("no tab given: name one (`str @N tab`)"),
            Error::NoTab(id) => write!(f, "no tab {id}"),
            Error::NoWorkspaceGiven => f.write_str(
                "no workspace given: name one (`str NAME workspace`; for new-tab, the \
                     option `str NAME in`)",
            ),
            Error::NoField {
                target,
                field,
                fields,
            } => write!(f, "{target} has no field {field:?}; it has {fields}"),
            Error::NotNamed(why) => f.write_str(why),
            Error::Revoked => f.write_str("the grant this socket came through was revoked"),
            Error::LayoutLine(line, why) => write!(f, "line {line}: {why}; nothing was made"),
            Error::LayoutFile(path, why) => write!(f, "{}: {why}", path.display()),
            Error::IdTaken(id) => write!(
                f,
                "id {id}: that id, or a higher one, was given before, and ids are never given twice"
            ),
            Error::IdTooHigh(id) => write!(
                f,
                "id {id}: too far ahead of the ids given so far, which would run out after it"
            ),
            Error::IdKind(id) => write!(
                f,
                "id {id}: not an id this command makes, or one of a kind given twice"
            ),
            Error::OutOfScope { what, scope } => write!(
                f,
                "{what} is not in workspace {scope}, the only one this socket reaches (grant)"
            ),
            Error::NotGranted { what, scope } => write!(
                f,
                "this socket reaches workspace {scope} alone (grant), not {what}"
            ),
            Error::OwnServer(Some(pane)) => write!(
                f,
                "this runs in {pane}, a pane of the server it would stop, with everything else \
                 in it; force-kill-server stops it anyway"
            ),
            Error::OwnServer(None) => f.write_str(
                "this runs in a pane of the server it would stop, with everything else in it; \
                 force-kill-server stops it anyway",
            ),
            Error::PeopleAttached(1) => f.write_str(
                "a person is attached to this server; force-kill-server stops it anyway",
            ),
            Error::PeopleAttached(n) => {
                write!(
                    f,
                    "{n} people are attached to this server; force-kill-server stops it anyway"
                )
            }
            Error::KillNeedsTarget(usage) => write!(
                f,
                "from the command line, name what to close: {usage} (so that an empty variable \
                 closes nothing, nor the pane this runs in)"
            ),
            Error::StrayPane(pane) => write!(
                f,
                "FUX_PANE {pane} is a pane of another fux server, not of this one, so it is not \
                 acted on; name the pane"
            ),
            Error::NoClientGiven => f.write_str(
                "this acts on a client's screen: name one (`str c1 client`; `fux ls` lists them)",
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
            Error::Exited { pane, status } => match status {
                crate::process::Ended::Exited(_) => {
                    write!(f, "the program in {pane} {}", crate::process::told(*status))
                }
                crate::process::Ended::Signalled(_) => {
                    write!(
                        f,
                        "the program in {pane} was {}",
                        crate::process::told(*status)
                    )
                }
            },
            Error::NoMotion(pane) => write!(
                f,
                "the program in {pane} takes no motion here: with no button pressed it asked for none, \
                 and asking for clicks alone (mode 1000) it takes no drags either"
            ),
            Error::OnlyPresses(pane) => write!(
                f,
                "the program in {pane} asked for X10 mouse reports: presses alone, no releases or wheel"
            ),
            Error::MouseUnencodable { pane, row, col } => write!(
                f,
                "the mouse encoding the program in {pane} asked for cannot say row {row}, column {col}"
            ),
            Error::TooBig(size, history) => write!(
                f,
                "{}x{} is too big for a pane keeping {history} lines of history",
                size.cols(),
                size.rows()
            ),
            Error::TooSmall(tab, panes) => write!(
                f,
                "{tab} cannot show its {panes} panes at that size; close some, or choose a bigger one"
            ),
            Error::Named(name) => write!(f, "{name:?} names several tabs: use an id"),
            Error::NoRoom(pane) => write!(
                f,
                "{pane}'s tab has no room for another pane; make it bigger first (`TAB COLS ROWS resize`)"
            ),
            Error::OutsidePane { pane, rows, cols } => write!(
                f,
                "{pane} is {cols}x{rows}: rows count 0-{} and columns 0-{}",
                rows.saturating_sub(1),
                cols.saturating_sub(1)
            ),
            Error::NoMouse(pane) => {
                write!(f, "the program in {pane} has not asked for mouse reports")
            }
            Error::CannotResize { pane, cols, rows } => {
                write!(f, "{pane} cannot be made {cols}x{rows} in its tab")
            }
            Error::WaitsOnlyFromCli => {
                f.write_str("wait, events and grant run from the command line")
            }
            Error::NotHeadless(client) => write!(
                f,
                "{client} has a terminal, which sets its size; resize sizes headless clients"
            ),
            Error::NotADirectory(path) => write!(f, "{} is not a directory", path.display()),
            Error::Log(path, error) => write!(f, "the log {}: {error}", path.display()),
            Error::Signal(error) => write!(f, "sending the signal: {error}"),
            Error::NotRun(pane) => write!(
                f,
                "{pane} runs a shell; respawn starts again what `fux run` started"
            ),
            Error::StillRunning(pane) => {
                write!(
                    f,
                    "the program in {pane} still runs; `kill-first` ends it first"
                )
            }
            Error::NoRows => f.write_str("the pane has no rows"),
            Error::SelectionTooLarge => write!(f, "the selection is larger than {MAX_CELLS} cells"),
            Error::EmptyName => f.write_str("a name cannot be empty or blank"),
            Error::ControlInName => {
                f.write_str("a name cannot contain control or invisible characters")
            }
            Error::LongName => f.write_str("a name is at most 256 bytes"),
            Error::NameTaken(name) => write!(f, "a workspace is named {name:?}"),
            Error::TabNameTaken(name, ids) => write!(
                f,
                "a tab is named {name:?} already ({ids}); close it (`fux str {} tab kill`) or choose another name",
                ids.split(' ')
                    .filter(|id| id.starts_with('@'))
                    .collect::<Vec<_>>()
                    .join(" tab kill`, `fux str ")
            ),
            Error::NameReadAsTarget(name) => write!(
                f,
                "{name:?} cannot be a name: a target would read it as a pane, tab or workspace number"
            ),
            Error::IdsExhausted(what) => write!(f, "no {what} IDs are left"),
            Error::LineTooLong => f.write_str("the command line is too long to type"),
            Error::Usage(error) => error.fmt(f),
            Error::Words(error) => error.fmt(f),
            Error::Pane(error) => error.fmt(f),
            Error::Process(error) => error.fmt(f),
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
            Error::Signal(error) => Some(error),
            Error::Config(error) | Error::Reload(error) => Some(error),
            Error::NoWorkspace(_)
            | Error::NoWorkspaceNamed(_)
            | Error::NoPaneGiven
            | Error::NoPane(_)
            | Error::NoTabGiven
            | Error::NoTab(_)
            | Error::NoWorkspaceGiven
            | Error::StrayPane(_)
            | Error::NoField { .. }
            | Error::NotNamed(_)
            | Error::Revoked
            | Error::IdTaken(_)
            | Error::IdKind(_)
            | Error::IdTooHigh(_)
            | Error::LayoutLine(..)
            | Error::LayoutFile(..)
            | Error::OutOfScope { .. }
            | Error::NotGranted { .. }
            | Error::OwnServer(_)
            | Error::PeopleAttached(_)
            | Error::KillNeedsTarget(_)
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
            | Error::NoMotion(_)
            | Error::OnlyPresses(_)
            | Error::MouseUnencodable { .. }
            | Error::OutsidePane { .. }
            | Error::NoRoom(_)
            | Error::Named(_)
            | Error::TooSmall(..)
            | Error::TooBig(..)
            | Error::CannotResize { .. }
            | Error::WaitsOnlyFromCli
            | Error::NotHeadless(_)
            | Error::NotADirectory(_)
            | Error::Log(..)
            | Error::NotRun(_)
            | Error::StillRunning(_)
            | Error::NoRows
            | Error::SelectionTooLarge
            | Error::EmptyName
            | Error::ControlInName
            | Error::LongName
            | Error::NameTaken(_)
            | Error::TabNameTaken(..)
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub status: u8,
    pub stdout: String,
    pub stderr: String,
    /// Its JSON form, where it has one (a wait's).
    pub json: String,
    /// The panes, tabs and workspaces it made or found.
    pub ids: Vec<AnyRef<WsId>>,
    /// A field's value (`ls -t X --get FIELD`).
    pub value: Option<crate::protocol::Scalar>,
    /// Why it failed, as a stable code.
    pub code: Option<&'static str>,
}

impl From<Outcome> for crate::protocol::Reply {
    fn from(outcome: Outcome) -> crate::protocol::Reply {
        crate::protocol::Reply {
            status: outcome.status,
            code: outcome.code.map(str::to_owned),
            message: outcome.stderr,
            text: outcome.stdout,
            json: outcome.json,
            ids: outcome.ids,
            value: outcome.value,
        }
    }
}

/// What a command line from the CLI begins: its outcome, now, or a wait
/// for one (`wait`, or an action that settles).
pub enum Begun {
    Done(Outcome),
    Waiting(Box<crate::wait::Waiting>),
    /// `fux events`: a stream, until the client goes.
    Events(Box<crate::events::Subscription>),
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
    /// Paste buffers, newest first.
    pub buffers: VecDeque<String>,
    /// What a started pane is told of its server, if panes start real
    /// processes; tests of the state alone start none.
    launch: Option<Launcher>,
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
    /// The code of the last command's error, for `--json` to name it.
    last_error: Option<&'static str>,
    /// What the command under way made or found, for its reply.
    made: Vec<AnyRef<WsId>>,
    /// The field the command under way read (`ls --get`), for its reply.
    value: Option<crate::protocol::Scalar>,
    /// How the panes the command under way starts start.
    starting: Starting,
    /// A restore is under way (`Session::restore`): ids come back as they
    /// were, and programs wait for `respawn`.
    restoring: bool,
    /// The sockets `fux grant` made, each with the workspace it reaches,
    /// for `ls` to list: the server holds them.
    pub grants: Vec<(PathBuf, String)>,
    /// In tests, each client's keys as its own client would read them
    /// (`Session::input`), and the keymap every client and run shares.
    #[cfg(test)]
    pub(crate) test_keys: BTreeMap<ClientId, testing::Keys>,
    #[cfg(test)]
    pub(crate) test_keymap: crate::keymap::Keymap,
}

/// How the panes a command starts start: the variables they take from
/// whoever starts them rather than the server (those `update-environment`
/// names that the starter has, and those it has not, which they are not
/// given either); and whether a program waits for `respawn` (a restore).
#[derive(Clone, Debug, Default)]
struct Starting {
    set: Vec<(String, String)>,
    unset: Vec<String>,
    paused: bool,
    /// The socket they reach the server by, if not its own: a grant's.
    socket: Option<PathBuf>,
}

/// The name of the first workspace, and of each workspace's first tab.
pub(crate) const MAIN: &str = "main";

/// The workspace `fux run` starts programs in unless told another.
pub const RUN_WORKSPACE: &str = "agent";

/// Whether a command leaves every client's screen as it was: it reads the
/// state, or hands a pane's program something whose effect, if any, comes
/// back as output.
fn shows_nothing_new(command: &Command) -> bool {
    matches!(
        command,
        Command::Ls { .. }
            | Command::Resolve { .. }
            | Command::ListBuffers
            | Command::ShowBuffer { .. }
            | Command::CapturePane { .. }
            | Command::SendKeys { .. }
            | Command::Signal { .. }
            | Command::Client {
                action: ClientAction::Capture { .. },
                ..
            }
    )
}

/// A log file, made anew: what a program writes may be secret, so it is
/// the user's alone.
/// A link is not followed, and a file there already is emptied and made
/// the user's alone too.
fn open_log(path: &Path) -> Result<std::fs::File, Error> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let failed = |e| Error::Log(path.to_owned(), e);
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
        Error::OutOfScope {
            what,
            scope: self.scope.to_owned(),
        }
    }
    fn refuse(&self, what: &'static str) -> Result<(), Error> {
        Err(Error::NotGranted {
            what,
            scope: self.scope.to_owned(),
        })
    }
    fn pane(&self, id: Option<PaneId>) -> Result<(), Error> {
        let id = match self.session.pane(id, self.here) {
            Ok(pane) => pane.id,
            Err(Error::NoPane(id)) => return Err(self.outside(id.to_string())),
            Err(error) => return Err(error),
        };
        match self.session.locate(id) {
            Some((ws, _)) if Some(ws) == self.ws => Ok(()),
            Some(_) | None => Err(self.outside(id.to_string())),
        }
    }
    fn tab(&self, id: Option<TabId>) -> Result<(), Error> {
        let tab = match self.session.tab_target(id, self.here) {
            Ok(tab) => tab,
            Err(Error::NoTab(tab)) => return Err(self.outside(tab.to_string())),
            Err(error) => return Err(error),
        };
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
            AnyRef::Pane(p) => self.pane(Some(*p)),
            AnyRef::Tab(t) => self.tab(Some(*t)),
            AnyRef::Workspace(w) => self.workspace(w),
        }
    }

    /// Whether `command` reaches only the granted workspace.
    fn command(&self, command: &Command) -> Result<(), Error> {
        match command {
            Command::Ls { target, .. } => target.as_ref().map_or(Ok(()), |t| self.any(t)),
            Command::Resolve { text, kind } => {
                let found = self.session.resolve(text, *kind, self.here);
                let found = found.map_err(|_| self.outside(format!("{text:?}")))?;
                self.any(&found.map(WsRef::Id))
            }
            Command::KillServer { .. } => self.refuse("kill-server"),
            Command::NewWorkspace { .. } => self.refuse("new-workspace"),
            Command::KillWorkspace { .. } => self.refuse("kill-workspace"),
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
            | Command::KillPane { target }
            | Command::Signal { target, .. }
            | Command::ResizePane { target, .. }
            | Command::Log { target, .. }
            | Command::ClearHistory { target }
            | Command::CapturePane { target, .. } => self.pane(*target),
            Command::KillTab { target } => self.tab(*target),
            Command::Notify { target, .. } => match target {
                Some(target) => self.any(target),
                None => self.refuse("a notice to every client"),
            },
            Command::Resize { what, .. } => match what {
                Resized::Tabs(target) => self.any(target),
                Resized::Client(_) => self.refuse("a client's size"),
            },
            Command::Wait { targets, .. } if targets.is_empty() => self.pane(None),
            Command::SendKeys {
                client: Some(_), ..
            } => self.refuse("a client's keys"),
            Command::SendKeys { targets, .. } if targets.is_empty() => self.pane(None),
            Command::SendKeys { targets, .. }
            | Command::Events { targets, .. }
            | Command::Wait { targets, .. } => targets.iter().try_for_each(|t| self.any(t)),
            Command::Rename { target, .. } => match target {
                AnyRef::Workspace(_) => self.refuse("renaming the workspace"),
                AnyRef::Pane(_) | AnyRef::Tab(_) => self.any(target),
            },
            Command::MovePane { target, to } => {
                self.pane(*target)?;
                match to {
                    MoveTo::Tab(tab) => self.tab(Some(*tab)),
                    MoveTo::Workspace(ws) => self.workspace(ws),
                    MoveTo::NewTab | MoveTo::Beside(_) => Ok(()),
                    MoveTo::NewWorkspace => self.refuse("a new workspace"),
                }
            }
            Command::SwapPane { target, with } => {
                self.pane(*target)?;
                match with {
                    SwapWith::Pane(pane) => self.pane(Some(*pane)),
                    SwapWith::Toward(_) => Ok(()),
                }
            }
            Command::Reorder { subject, .. } => match subject {
                Subject::Pane(pane) => self.pane(*pane),
                Subject::Tab(tab) => self.tab(*tab),
                Subject::Workspace(_) => self.refuse("reordering workspaces"),
            },
            Command::Set { .. } => self.refuse("the server's settings"),
            Command::ListBuffers | Command::ShowBuffer { .. } | Command::PasteBuffer { .. } => {
                self.refuse("the paste buffers, which are everyone's")
            }
            Command::Client { .. } => self.refuse("a client's screen"),
            Command::Grant { .. } => self.refuse("another grant"),
            Command::DumpLayout { target } => target.as_ref().map_or(Ok(()), |t| self.any(t)),
        }
    }
}

/// A failed outcome as `--json` gives it: its message as JSON on stderr,
/// with `code`, or what its status says.
pub fn json_failure(mut outcome: Outcome, code: Option<&str>) -> Outcome {
    let text = outcome.stderr.trim_end();
    if outcome.status == 0 || text.is_empty() || text.starts_with('{') {
        return outcome;
    }
    let code = match (outcome.status, code) {
        (_, Some(code)) => code,
        (2, None) => "usage",
        (_, None) => "failed",
    };
    outcome.stderr = Json::Object(vec![
        ("error", Json::str(code)),
        ("message", Json::str(text)),
    ])
    .render();
    outcome
}

/// A command that needed its caller's pane, tab or workspace, from a caller
/// whose `FUX_PANE` is another server's pane: why it got none.
fn stray(error: Error, origin: &Origin) -> Error {
    match (error, origin) {
        (
            Error::NoPaneGiven | Error::NoTabGiven | Error::NoWorkspaceGiven,
            Origin::Cli(Caller {
                stray: Some(pane), ..
            }),
        ) => Error::StrayPane(pane.clone()),
        (error, _) => error,
    }
}

fn basename(program: &str) -> String {
    Path::new(program)
        .file_name()
        .map_or_else(|| program.to_owned(), |n| n.to_string_lossy().into_owned())
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
            launch: launch.then(|| Launcher {
                socket,
                server: server_id.clone(),
            }),
            server_id,
            outbox: Vec::new(),
            dying: BTreeMap::new(),
            last_colours: crate::outer::Colours::default(),
            last_palette: crate::outer::Palette::default(),
            shown_sizes: Vec::new(),
            placed: Placement::default(),
            ids: Ids::default(),
            changes: 0,
            last_error: None,
            made: Vec::new(),
            value: None,
            starting: Starting::default(),
            restoring: false,
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
            Origin::Cli(ref caller) => {
                let pane = caller.pane;
                let at = pane.and_then(|p| self.locate(p));
                let at = at.map(|(ws, tab)| Place { ws, tab, pane });
                Ok(Here::Cli(pane, at, caller.cwd.as_deref()))
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
        let child = match &self.launch {
            Some(launcher) => Some(launcher.spawn(
                (&self.config, &self.starting),
                id,
                (&launch.argv, &launch.env),
                &launch.cwd,
                size,
            )?),
            None => None,
        };
        let history = self.config.history_lines;
        let Some(pane) = self.panes.get_mut(&id) else {
            return Err(Error::NoPane(id));
        };
        let old = pane.respawned(launch, child.map(Process::Reading), history)?;
        if let Process::Reading(mut child) | Process::HungUp(mut child) = old {
            child.leader.closed();
            child.leader.hang_up();
            let deadline = crate::after(Instant::now(), GRACE);
            self.dying.insert(id, Dying { child, deadline });
        }
        self.touch();
        Ok(())
    }

    /// The process group `fux signal` signals in a pane: the job its shell
    /// runs in the foreground, or the program `run` started; never a shell.
    fn signalled(&self, target: Option<PaneId>, here: Here) -> Result<crate::process::Pid, Error> {
        let pane = self.pane(target, here)?;
        if let Some(status) = pane.process.exit_status() {
            return Err(Error::Exited {
                pane: pane.id,
                status,
            });
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
        pane.process
            .job()
            .or(program)
            .ok_or(Error::OnlyShell(pane.id))
    }

    /// Refuses a name given to a tab (other than `except`, being renamed)
    /// that `-t NAME` could not find it by: one a target reads as an id, or
    /// one another tab has.
    fn check_tab_name(&self, name: &str, except: Option<TabId>) -> Result<(), Error> {
        check_name(name)?;
        // One `-t` would read as an id (a name like `-x`, read as a flag,
        // is only one -t cannot give).
        if name.starts_with(['%', '@', '+']) {
            return Err(Error::NameReadAsTarget(name.to_owned()));
        }
        // A workspace's name finds the workspace, never this tab.
        if self.workspaces.iter().any(|w| w.name == name) {
            return Err(Error::NameTaken(name.to_owned()));
        }
        let taken: Vec<TabId> = (self.workspaces.iter())
            .flat_map(Workspace::tabs)
            .filter(|t| t.name == name && Some(t.id) != except)
            .map(|t| t.id)
            .collect();
        if taken.is_empty() {
            return Ok(());
        }
        let ids = [
            taken.iter().map(ToString::to_string).collect(),
            self.panes_of_tabs(&taken),
        ];
        Err(Error::TabNameTaken(name.to_owned(), ids.concat().join(" ")))
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
    pub(crate) fn copying(&self, pane: PaneId) -> bool {
        self.views
            .values()
            .any(|v| matches!(&v.mode, Mode::Copy(copy) if copy.pane == pane))
    }

    /// `pane_mut`, for input: a pane whose program has exited takes none.
    /// What panes started for `origin` take of its environment: none of
    /// it for one that sent none (a test, a client of no terminal).
    fn starting_for(&self, origin: &Origin) -> Starting {
        let env = match origin {
            Origin::Cli(caller) => caller.env.as_slice(),
            Origin::Client(client) => self.views.get(client).map_or(&[][..], |v| v.env.as_slice()),
            Origin::Nowhere => &[],
        };
        let paused = self.restoring;
        let socket = match origin {
            Origin::Cli(caller) => caller.grant.clone(),
            Origin::Client(_) | Origin::Nowhere => None,
        };
        if env.is_empty() {
            return Starting {
                paused,
                socket,
                ..Starting::default()
            };
        }
        let mut fresh = Starting {
            paused,
            socket,
            ..Starting::default()
        };
        for name in &self.config.update_environment {
            match env.iter().rev().find(|(k, _)| k == name) {
                Some((k, v)) => fresh.set.push((k.clone(), v.clone())),
                None => fresh.unset.push(name.clone()),
            }
        }
        fresh
    }

    /// The next ids made are `ids` (`--id`), each of a kind in `kinds`, at
    /// most one of each, and none that is there or (outside a restore)
    /// was ever given.
    fn want(&mut self, ids: &[AnyId], kinds: &[Kind]) -> Result<(), Error> {
        for (i, id) in ids.iter().enumerate() {
            let twice = ids.iter().take(i).any(|other| other.kind() == id.kind());
            if twice || !kinds.contains(&id.kind()) {
                return Err(Error::IdKind(describe(*id)));
            }
            if self.exists(*id) {
                return Err(Error::IdTaken(describe(*id)));
            }
            self.ids.want(*id, self.restoring)?;
        }
        Ok(())
    }

    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        (self.workspaces.iter_mut())
            .flat_map(Workspace::tabs_mut)
            .find(|t| t.id == id)
    }

    /// What a command that makes a pane prints, `text`, the ids it names,
    /// or with `json` where the pane is; the ids made go in its reply.
    fn made_ids(
        &mut self,
        (ws, tab, pane): (WsId, TabId, PaneId),
        text: &[String],
        json: bool,
    ) -> String {
        self.made = vec![AnyRef::Workspace(ws), AnyRef::Tab(tab), AnyRef::Pane(pane)];
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

    /// What `text` names as a pane, tab or workspace, `kind`: its id (`%3`),
    /// or a tab's name (a pane, its tab's one pane); a workspace's name.
    /// Nothing given, the caller's own.
    fn resolve(&self, text: &str, kind: Kind, here: Here) -> Result<AnyRef<WsId>, Error> {
        let tabs: Vec<&Tab> = (self.workspaces.iter())
            .flat_map(Workspace::tabs)
            .filter(|t| t.name == text)
            .collect();
        let panes_of = |tab: &Tab| tab.root().map(Tree::panes).unwrap_or_default();
        let ids = |panes: &[PaneId]| panes.iter().map(ToString::to_string).collect::<Vec<_>>();
        let named = || -> Result<&Tab, Error> {
            match tabs.as_slice() {
                [tab] => Ok(*tab),
                [] => Err(Error::NotNamed(format!(
                    "no {} named {text:?}",
                    kind.name()
                ))),
                many => {
                    let each: Vec<String> = (many.iter())
                        .map(|t| format!("{} {}", t.id, ids(&panes_of(t)).join(" ")))
                        .collect();
                    Err(Error::NotNamed(format!(
                        "{text:?} names {} tabs ({}): use an id",
                        many.len(),
                        each.join(", ")
                    )))
                }
            }
        };
        // A pane's or tab's id stands for its tab or workspace, and a tab
        // of one pane for that pane.
        let pane = text.parse::<PaneId>().ok();
        let tab = text.parse::<TabId>().ok();
        if let (Kind::Tab | Kind::Workspace, Some(p)) = (kind, pane) {
            let (ws, tab) = self.locate(p).ok_or(Error::NoPane(p))?;
            return Ok(match kind {
                Kind::Tab => AnyRef::Tab(tab),
                Kind::Pane | Kind::Workspace => AnyRef::Workspace(ws),
            });
        }
        if let (Kind::Workspace, Some(t)) = (kind, tab) {
            return self.tab_workspace(t).map(AnyRef::Workspace);
        }
        if let (Kind::Pane, Some(t)) = (kind, tab) {
            let panes = self.root(t).map(Tree::panes).ok_or(Error::NoTab(t))?;
            return match panes.as_slice() {
                [pane] => Ok(AnyRef::Pane(*pane)),
                panes => Err(Error::NotNamed(format!(
                    "{t} is a tab of {} panes ({}): use a pane's id",
                    panes.len(),
                    ids(panes).join(" ")
                ))),
            };
        }
        match kind {
            Kind::Pane if text.is_empty() => Ok(AnyRef::Pane(self.pane(None, here)?.id)),
            Kind::Pane => match text.parse::<PaneId>() {
                Ok(id) => Ok(AnyRef::Pane(self.pane(Some(id), here)?.id)),
                Err(_) => {
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
                            return match panes.as_slice() {
                                [pane] => Ok(AnyRef::Pane(*pane)),
                                [] => Err(no_tab),
                                panes => Err(Error::NotNamed(format!(
                                    "{text:?} names {} panes ({}): use an id",
                                    panes.len(),
                                    ids(panes).join(" ")
                                ))),
                            };
                        }
                    };
                    match panes_of(tab).as_slice() {
                        [pane] => Ok(AnyRef::Pane(*pane)),
                        panes => Err(Error::NotNamed(format!(
                            "{text:?} is a tab of {} panes ({}): use a pane's id",
                            panes.len(),
                            ids(panes).join(" ")
                        ))),
                    }
                }
            },
            Kind::Tab if text.is_empty() => Ok(AnyRef::Tab(self.tab_target(None, here)?)),
            Kind::Tab => match text.parse::<TabId>() {
                Ok(id) => Ok(AnyRef::Tab(self.tab_target(Some(id), here)?)),
                Err(_) => named().map(|tab| AnyRef::Tab(tab.id)),
            },
            Kind::Workspace if text.is_empty() => {
                Ok(AnyRef::Workspace(self.ws_target(None, here)?))
            }
            Kind::Workspace => {
                let ws = command::parse_workspace(text).map_err(Error::Usage)?;
                Ok(AnyRef::Workspace(self.ws_target(Some(&ws), here)?))
            }
        }
    }

    /// The panes `targets` name, a tab's every pane, in order and each
    /// once; the caller's without any.
    fn panes_of(&self, targets: &[AnyRef], here: Here) -> Result<Vec<PaneId>, Error> {
        if targets.is_empty() {
            return Ok(vec![self.pane(None, here)?.id]);
        }
        let mut out: Vec<PaneId> = Vec::new();
        for target in targets {
            let found = match *target {
                AnyRef::Pane(pane) => vec![self.pane(Some(pane), here)?.id],
                AnyRef::Tab(tab) => self.root(tab).ok_or(Error::NoTab(tab))?.panes(),
                AnyRef::Workspace(ref ws) => {
                    let ws = self.resolve_ws(ws)?;
                    (self.workspace(ws).into_iter())
                        .flat_map(Workspace::tabs)
                        .filter_map(Tab::root)
                        .flat_map(Tree::panes)
                        .collect()
                }
            };
            out.extend(
                found
                    .into_iter()
                    .filter(|p| !out.contains(p))
                    .collect::<Vec<_>>(),
            );
        }
        Ok(out)
    }

    /// Sends `inputs` to pane `id`'s program.
    fn send_inputs(&mut self, id: PaneId, inputs: &[Input]) -> Result<(), Error> {
        let p = self.panes.get_mut(&id).ok_or(Error::NoPane(id))?;
        p.note_input(inputs);
        let (mode, bracketed) = (
            p.screen().key_mode(),
            p.screen().mode(fux_vt::Mode::BracketedPaste),
        );
        // Inputs go in pieces, each read by the program on its own,
        // paced apart: text typed in a row is one piece, keys pressed
        // in a row another, and a lone Escape and a paste each their
        // own. So in `--text msg Enter` the Enter is read alone, as a
        // submit, not as a newline pasted with the text.
        #[derive(PartialEq)]
        enum Kind {
            Text,
            Keys,
            Alone,
        }
        let mut pieces: Vec<Vec<u8>> = Vec::new();
        let mut last = Kind::Alone;
        for input in inputs {
            let mut bytes = Vec::new();
            let kind = match input {
                Input::Key(key) => {
                    crate::encode::key_bytes((*key).into(), mode, &mut bytes);
                    if bytes == b"\x1b" {
                        Kind::Alone
                    } else {
                        Kind::Keys
                    }
                }
                Input::Text(text) => {
                    bytes.extend_from_slice(text.as_bytes());
                    Kind::Text
                }
                Input::Paste(text) => {
                    crate::encode::paste(text, bracketed, &mut bytes);
                    Kind::Alone
                }
            };
            match pieces.last_mut() {
                Some(piece) if kind != Kind::Alone && kind == last => {
                    piece.extend_from_slice(&bytes);
                }
                _ => pieces.push(bytes),
            }
            last = kind;
        }
        p.send(pieces, Instant::now())?;
        Ok(())
    }

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
        // No place for it, or one it takes from another pane: the tab is
        // too small to show another pane.
        let placed = layout::place(&root, area);
        let shows_all = root.panes().iter().all(|p| placed.rect(*p).is_some());
        let rect = placed
            .rect(new)
            .filter(|_| shows_all)
            .ok_or(Error::NoRoom(target))?;
        Ok(crate::pane::size(rect.h(), rect.w()))
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
            // A name no workspace has may be a tab's (`run -n NAME`).
            Subject::Workspace(w) => match (self.ws_target(w.as_ref(), here), w) {
                // A workspace's name is the workspace's, before a tab's
                // (`main` names both at first).
                (Ok(ws), _) => AnyRef::Workspace(ws),
                (Err(error), Some(WsRef::Name(name))) => {
                    let mut tabs = (self.workspaces.iter())
                        .flat_map(Workspace::tabs)
                        .filter(|t| t.name == *name);
                    match (tabs.next(), tabs.next()) {
                        (Some(tab), None) => AnyRef::Tab(tab.id),
                        (Some(_), Some(_)) => return Err(Error::Named(name.clone())),
                        _ => return Err(error),
                    }
                }
                (Err(error), _) => return Err(error),
            },
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
    /// Where a new tab's or workspace's shell starts: `dir`, from the
    /// caller's directory, or that directory itself.
    fn start_dir(&self, here: Here, dir: Option<&str>) -> Result<PathBuf, Error> {
        let cwd = self.cwd_for(here, None);
        let cwd = dir.map_or_else(|| cwd.clone(), |dir| cwd.join(dir));
        match cwd.is_dir() {
            true => Ok(cwd),
            false => Err(Error::NotADirectory(cwd)),
        }
    }

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
        let launch = self.launch.as_ref();
        let pane = new_pane(
            &self.config,
            (launch, &self.starting),
            &mut ids,
            start,
            cwd,
            size,
        )?;
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
        let launch = self.launch.as_ref();
        let pane = new_pane(
            &self.config,
            (launch, &self.starting),
            &mut ids,
            start,
            cwd,
            size,
        )?;
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
            None => {
                // A server whose session did not come back starts afresh.
                if self.workspaces.is_empty() {
                    self.start()?;
                }
                self.workspaces
                    .first()
                    .map(|w| w.id)
                    .ok_or(Error::NoWorkspaces)?
            }
        };
        let id = self.ids.client()?;
        let mut view = View::new(id, rows.clamp(1, 4096), cols.clamp(1, 4096));
        view.kind = kind;
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
            if w.id != ws {
                w.hide(client);
            } else {
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
        let ended: Vec<ClientId> = (views.values_mut())
            .filter_map(|view| self.repair(view).then_some(view.id))
            .collect();
        self.views = views;
        for client in ended {
            self.outbox.push(Outgoing::Ended(client));
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
                self.outbox.push(Outgoing::Ended(view.id));
            }
        }
    }

    /// Ends what a view shows if what it is about is gone: copy mode whose
    /// pane closed or is no longer focused, a client's screen about a pane,
    /// tab or workspace that closed. Whether it did, which its client is
    /// told (`Outgoing::Ended`), the mode being the client's.
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
            ) if !self.exists(*about) => format!("closed: {} is gone", describe(*about)),
            Mode::Normal | Mode::Panel(_) | Mode::Copy(_) => return false,
        };
        view.mode = Mode::Normal;
        view.error(gone);
        true
    }

    /// The settings changed: every pane keeps the history they say. A pane
    /// whose history cannot be that long at its size keeps its own, as a
    /// new pane of that size would fail.
    fn configured(&mut self) {
        let lines = self.config.history_lines;
        for pane in self.panes.values_mut() {
            let _ = pane.parser.set_history_lines(lines);
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
        let typical = Rect::screen(DEFAULT_SIZE.0, DEFAULT_SIZE.1);
        let client = Some(self.views.values().next().map_or(typical, Self::pane_area));
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
    /// `closed`: fux closed it (kill-pane, kill-server), not its program's
    /// end: the rest of its session goes too.
    fn end(&mut self, pane: Pane, closed: bool) {
        if let Process::Reading(mut child) | Process::HungUp(mut child) = pane.process {
            if closed {
                child.leader.closed();
            }
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
            // Closed for its program's end (a reason given), or by fux.
            let closed = why.is_none();
            if let Some(why) = why {
                for client in &viewers {
                    self.info_to(*client, format!("{id} {} {why}", pane.label()));
                }
            }
            self.end(pane, closed);
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
                    self.end(pane, true);
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
            self.end(pane, true);
        }
    }

    // ------------------------------------------------------------ commands

    /// Begins a request from a client: one that waits gives its wait,
    /// which the server asks for its answer until it has one; a stream of
    /// events, its subscription; any other runs at once.
    pub fn begin(&mut self, command: Command, origin: &Origin) -> Begun {
        self.last_error = None;
        self.restoring = matches!(
            origin,
            Origin::Cli(Caller {
                restoring: true,
                ..
            })
        );
        // A size a saved session asks for that cannot be had here is left
        // as it comes, so one does not keep the rest from coming back.
        if self.restoring && matches!(command, Command::ResizePane { .. }) {
            let mut outcome = self.run_command(&command, origin);
            if outcome.status != 0 {
                outcome = Outcome::default();
            }
            return Begun::Done(outcome);
        }
        let command = match origin {
            Origin::Cli(Caller {
                scope: Some(scope), ..
            }) => match self.scoped(command, scope, origin) {
                Ok(command) => command,
                Err(error) => return self.failed(error, origin),
            },
            Origin::Cli(_) | Origin::Client(_) | Origin::Nowhere => command,
        };
        self.begin_command(command, origin)
    }

    fn begin_command(&mut self, command: Command, origin: &Origin) -> Begun {
        let (targets, wait, keys) = match command {
            Command::Events { targets, json } => {
                let here = match self.here(origin) {
                    Ok(here) => here,
                    Err(error) => return self.failed(error, origin),
                };
                let targets: Result<Vec<_>, Error> = (targets.into_iter())
                    .map(|t| self.any_target(&Subject::from(t), here))
                    .collect();
                return match targets {
                    Ok(targets) => Begun::Events(Box::new(crate::events::Subscription {
                        targets,
                        json,
                        seen: crate::events::Snapshot::of(self),
                    })),
                    Err(error) => self.failed(error, origin),
                };
            }
            Command::Wait { targets, wait } => (targets, wait, None),
            // From a command line: read once the pane settles, said with
            // its state.
            Command::CapturePane {
                target,
                history,
                view,
                json,
                settle: Some(limit),
            } => {
                let wait = crate::command::Wait {
                    until: vec![crate::command::Until::Settled],
                    timeout: limit,
                    print: false,
                    after: false,
                    marks: false,
                    all: false,
                };
                let here = match self.here(origin) {
                    Ok(here) => here,
                    Err(error) => return self.failed(error, origin),
                };
                let pane = match self.pane(target, here) {
                    Ok(pane) => pane,
                    Err(error) => return self.failed(error, origin),
                };
                let now = Instant::now();
                let mut waiting = crate::wait::Waiting::new(&[pane], wait, now);
                waiting.capture = Some((history, view, json));
                return match waiting.answer(self, now) {
                    Some(outcome) => Begun::Done(outcome),
                    None => Begun::Waiting(Box::new(waiting)),
                };
            }
            command @ (Command::Ls { .. }
            | Command::KillServer { .. }
            | Command::NewWorkspace { .. }
            | Command::NewTab { .. }
            | Command::Split { .. }
            | Command::Run { .. }
            | Command::Respawn { .. }
            | Command::Notify { .. }
            | Command::Resize { .. }
            | Command::SendMouse { .. }
            | Command::KillPane { .. }
            | Command::Signal { .. }
            | Command::KillTab { .. }
            | Command::KillWorkspace { .. }
            | Command::Rename { .. }
            | Command::MovePane { .. }
            | Command::SwapPane { .. }
            | Command::ResizePane { .. }
            | Command::SendKeys { .. }
            | Command::DumpLayout { .. }
            | Command::Grant { .. }
            | Command::Log { .. }
            | Command::ClearHistory { .. }
            | Command::CapturePane { .. }
            | Command::Reorder { .. }
            | Command::Set { .. }
            | Command::Resolve { .. }
            | Command::ListBuffers
            | Command::ShowBuffer { .. }
            | Command::PasteBuffer { .. }
            | Command::Client { .. })
                if command.acting_wait().is_some() =>
            {
                let Some((targets, wait)) = command.acting_wait() else {
                    return Begun::Done(self.run_command(&command, origin));
                };
                (targets, wait, Some(command))
            }
            // Started, then waited for from its first output on.
            mut command @ (Command::Run { then: Some(_), .. }
            | Command::Split { then: Some(_), .. }
            | Command::Respawn { then: Some(_), .. }) => {
                return match command.take_start_wait() {
                    Some(wait) => self.start_waiting(&command, *wait, origin),
                    None => Begun::Done(self.run_command(&command, origin)),
                };
            }
            command @ (Command::Ls { .. }
            | Command::KillServer { .. }
            | Command::NewWorkspace { .. }
            | Command::NewTab { .. }
            | Command::Split { .. }
            | Command::Run { .. }
            | Command::Respawn { .. }
            | Command::Notify { .. }
            | Command::Resize { .. }
            | Command::SendMouse { .. }
            | Command::KillPane { .. }
            | Command::Signal { .. }
            | Command::KillTab { .. }
            | Command::KillWorkspace { .. }
            | Command::Rename { .. }
            | Command::MovePane { .. }
            | Command::SwapPane { .. }
            | Command::ResizePane { .. }
            | Command::SendKeys { .. }
            | Command::DumpLayout { .. }
            | Command::Grant { .. }
            | Command::Log { .. }
            | Command::ClearHistory { .. }
            | Command::CapturePane { .. }
            | Command::Reorder { .. }
            | Command::Set { .. }
            | Command::Resolve { .. }
            | Command::ListBuffers
            | Command::ShowBuffer { .. }
            | Command::PasteBuffer { .. }
            | Command::Client { .. }) => return Begun::Done(self.run_command(&command, origin)),
        };
        let here = match self.here(origin) {
            Ok(here) => here,
            Err(error) => return self.failed(error, origin),
        };
        let ids = match self.panes_of(&targets, here) {
            Ok(ids) => ids,
            Err(error) => return self.failed(error, origin),
        };
        let panes: Result<Vec<&Pane>, Error> = ids
            .into_iter()
            .map(|id| self.pane(Some(id), here))
            .collect();
        let now = Instant::now();
        // Armed before any keys go in: what the keys bring is what it sees.
        let mut waiting = match panes {
            Ok(panes) => crate::wait::Waiting::new(&panes, wait, now),
            Err(error) => return self.failed(error, origin),
        };
        match keys {
            Some(command) => {
                let outcome = self.run_command(&command, origin);
                if outcome.status != 0 {
                    return Begun::Done(outcome);
                }
                waiting.typed(self);
            }
            None => waiting.more = true,
        }
        match waiting.answer(self, now) {
            Some(outcome) => Begun::Done(outcome),
            None => Begun::Waiting(Box::new(waiting)),
        }
    }

    /// A command line from a granted socket, if it may run: with no target,
    /// one that would act anywhere (`ls`, `run`, `new-tab`, `events`) acts
    /// on the granted workspace; one that names, or comes from, anything
    /// outside it, or reaches what is everyone's (the config, the buffers,
    /// clients, the server), is refused.
    fn scoped(&self, command: Command, scope: &str, origin: &Origin) -> Result<Command, Error> {
        if scope.is_empty() {
            return Err(Error::Revoked);
        }
        let place = || Some(WsRef::Name(scope.to_owned()));
        let command = match command {
            Command::Run {
                workspace: None,
                name,
                size,
                cwd,
                env,
                argv,
                json,
                log,
                ids,
                then,
            } => Command::Run {
                workspace: place(),
                name,
                size,
                cwd,
                env,
                argv,
                json,
                log,
                ids,
                then,
            },
            Command::NewTab {
                target: None,
                name,
                cmd,
                size,
                cwd,
                env,
                ids,
                json,
            } => Command::NewTab {
                target: place(),
                name,
                cmd,
                size,
                cwd,
                env,
                ids,
                json,
            },
            Command::Events { targets, json } if targets.is_empty() => Command::Events {
                targets: vec![AnyRef::Workspace(WsRef::Name(scope.to_owned()))],
                json,
            },
            Command::DumpLayout { target: None } => Command::DumpLayout {
                target: Some(AnyRef::Workspace(WsRef::Name(scope.to_owned()))),
            },
            other @ (Command::Ls { .. }
            | Command::KillServer { .. }
            | Command::NewWorkspace { .. }
            | Command::NewTab { .. }
            | Command::Split { .. }
            | Command::Run { .. }
            | Command::Respawn { .. }
            | Command::Notify { .. }
            | Command::Resize { .. }
            | Command::Wait { .. }
            | Command::SendMouse { .. }
            | Command::KillPane { .. }
            | Command::Signal { .. }
            | Command::KillTab { .. }
            | Command::KillWorkspace { .. }
            | Command::Rename { .. }
            | Command::MovePane { .. }
            | Command::SwapPane { .. }
            | Command::ResizePane { .. }
            | Command::SendKeys { .. }
            | Command::Events { .. }
            | Command::DumpLayout { .. }
            | Command::Grant { .. }
            | Command::Log { .. }
            | Command::ClearHistory { .. }
            | Command::CapturePane { .. }
            | Command::Reorder { .. }
            | Command::Set { .. }
            | Command::Resolve { .. }
            | Command::ListBuffers
            | Command::ShowBuffer { .. }
            | Command::PasteBuffer { .. }
            | Command::Client { .. }) => other,
        };
        let reach = Reach {
            session: self,
            ws: self.resolve_ws(&WsRef::Name(scope.to_owned())).ok(),
            scope,
            here: self.here(origin)?,
        };
        reach.command(&command)?;
        Ok(command)
    }

    /// A command line from the CLI that failed before it ran: why, its code
    /// kept for `--json`.
    fn failed(&mut self, error: Error, origin: &Origin) -> Begun {
        let error = stray(error, origin);
        self.last_error = Some(error.code());
        Begun::Done(Outcome {
            status: 1,
            stderr: error.to_string(),
            code: Some(error.code()),
            ..Outcome::default()
        })
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
            .filter(|_| outcome.status == 0)
            .and_then(|p| self.panes.get(&p))
        else {
            return Begun::Done(outcome);
        };
        let now = Instant::now();
        let mut waiting = crate::wait::Waiting::new(&[pane], wait, now);
        waiting.lead = outcome.stdout;
        waiting.made = outcome.ids;
        waiting.fresh = true;
        match waiting.answer(self, now) {
            Some(outcome) => Begun::Done(outcome),
            None => Begun::Waiting(Box::new(waiting)),
        }
    }

    /// Runs a command, parsed already: from a line, a binding or a menu.
    pub fn run_command(&mut self, command: &Command, origin: &Origin) -> Outcome {
        self.made.clear();
        self.value = None;
        let executed = self.execute(command, origin);
        self.ids.unwant();
        let outcome = match executed {
            Ok(stdout) => Outcome {
                stdout,
                ids: std::mem::take(&mut self.made),
                value: self.value.take(),
                ..Outcome::default()
            },
            Err(error) => {
                let error = stray(error, origin);
                self.last_error = Some(error.code());
                let status = if matches!(error, Error::Usage(_)) {
                    2
                } else {
                    1
                };
                Outcome {
                    status,
                    stderr: error.to_string(),
                    code: Some(error.code()),
                    ..Outcome::default()
                }
            }
        };
        if !shows_nothing_new(command) {
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

    fn execute(&mut self, command: &Command, origin: &Origin) -> Result<String, Error> {
        let here = self.here(origin)?;
        self.starting = self.starting_for(origin);
        match command {
            Command::Ls { json, target, get } => {
                // Through a grant, the granted workspace alone.
                let scope = match origin {
                    Origin::Cli(Caller {
                        scope: Some(scope), ..
                    }) => Some(self.resolve_ws(&WsRef::Name(scope.clone()))?),
                    Origin::Cli(_) | Origin::Client(_) | Origin::Nowhere => None,
                };
                match target {
                    Some(target) => {
                        let target = self.any_target(&Subject::from(target.clone()), here)?;
                        self.ls_one(target, *json, get.as_deref())
                    }
                    None if *json => Ok(self.ls_json(scope)),
                    None => Ok(self.ls_text(scope.map(AnyRef::Workspace))),
                }
            }
            &Command::KillServer { force } => {
                if let (Origin::Cli(caller), false) = (origin, force) {
                    let pane = caller.pane.filter(|p| self.panes.contains_key(p));
                    if pane.is_some() || caller.inside {
                        return Err(Error::OwnServer(pane));
                    }
                    let people = (self.views.values())
                        .filter(|v| v.kind == ClientKind::Terminal)
                        .count();
                    if people > 0 {
                        return Err(Error::PeopleAttached(people));
                    }
                }
                let reason = match origin {
                    Origin::Client(_) => "stopped by kill-server, from a key or the prompt",
                    Origin::Cli(_) | Origin::Nowhere => "stopped by fux kill-server",
                };
                self.outbox.push(Outgoing::Shutdown {
                    reason: reason.into(),
                    keep: false,
                });
                Ok(String::new())
            }
            Command::NewWorkspace {
                name,
                cmd,
                size,
                cwd,
                env,
                ids,
                json,
            } => {
                self.want(ids, &[Kind::Workspace, Kind::Tab, Kind::Pane])?;
                let cwd = self.start_dir(here, cwd.as_deref())?;
                let fit = size.map_or(Fit::Clients, Fit::Fixed);
                let start = (Start::Shell(cmd, env), cwd.as_path());
                let made =
                    self.create_workspace(name.clone(), start, (fit, self.start_size(fit, here)))?;
                let (ws, tab, pane) = made;
                self.follow(here, ws, None, None);
                Ok(self.made_ids(
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
                cwd,
                env,
                ids,
                json,
            } => {
                self.want(ids, &[Kind::Workspace, Kind::Tab, Kind::Pane])?;
                if let Some(name) = name {
                    self.check_tab_name(name, None)?;
                }
                let cwd = self.start_dir(here, cwd.as_deref())?;
                let fit = size.map_or(Fit::Clients, Fit::Fixed);
                let start = (Start::Shell(cmd, env), cwd.as_path());
                let place = (fit, self.start_size(fit, here));
                // A workspace named that is not there is made, as `run`
                // makes one, its first tab this one.
                let (ws, tab, pane) = match (self.ws_target(target.as_ref(), here), target) {
                    (Ok(ws), _) => {
                        let (tab, pane) = self.create_tab(ws, name.clone(), start, place)?;
                        (ws, tab, pane)
                    }
                    (Err(_), Some(WsRef::Name(ws_name))) => {
                        let made = self.create_workspace(Some(ws_name.clone()), start, place)?;
                        if let (Some(name), Some(tab)) = (name, self.tab_mut(made.1)) {
                            tab.name = name.clone();
                        }
                        made
                    }
                    (Err(error), _) => return Err(error),
                };
                self.follow(here, ws, Some(tab), None);
                Ok(self.made_ids((ws, tab, pane), &[tab.to_string(), pane.to_string()], *json))
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
                ids,
                ..
            } => {
                self.want(ids, &[Kind::Workspace, Kind::Tab, Kind::Pane])?;
                let here_cwd = self.cwd_for(here, None);
                let log = log
                    .as_ref()
                    .map(|p| open_log(&here_cwd.join(p)))
                    .transpose()?;
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
                // A name given is one `-t NAME` finds: it must be the only one.
                if let Some(name) = name {
                    self.check_tab_name(name, None)?;
                }
                // Named after its program, if that makes a name.
                let derived = argv
                    .first()
                    .map(|p| basename(p))
                    .filter(|n| check_name(n).is_ok());
                let name = name.clone().or(derived);
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
                Ok(self.made_ids(made, &[made.2.to_string()], *json))
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
                self.made = vec![AnyRef::Pane(id)];
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
                        // A size no pane could have, with the history kept,
                        // is refused, not left unmet.
                        if let Some(size) = size
                            && !fux_vt::fits(*size, self.config.history_lines)
                        {
                            return Err(Error::TooBig(*size, self.config.history_lines));
                        }
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
                                    return Err(Error::TooSmall(*tab, root.panes().len()));
                                }
                            }
                        }
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
            Command::Wait { .. } | Command::Events { .. } | Command::Grant { .. } => {
                Err(Error::WaitsOnlyFromCli)
            }
            Command::DumpLayout { target } => {
                let only = match target {
                    Some(target) => Some(self.any_target(&Subject::from(target.clone()), here)?),
                    None => None,
                };
                Ok(self.dump_layout(only, false))
            }
            &Command::SendMouse {
                target,
                mouse,
                row,
                col,
                ..
            } => {
                let id = self.live_pane_mut(target, here)?.id;
                let p = self.live_pane_mut(Some(id), here)?;
                let size = p.size();
                if row >= size.rows() || col >= size.cols() {
                    let (rows, cols) = (size.rows(), size.cols());
                    return Err(Error::OutsidePane {
                        pane: p.id,
                        rows,
                        cols,
                    });
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
                    Mouse::Click(button) => (
                        vec![event(Press, Some(button)), event(Release, Some(button))],
                        unheld(button),
                    ),
                    Mouse::Press(button) => (vec![event(Press, Some(button))], Some(button)),
                    Mouse::Wheel(button) => (vec![event(Press, Some(button))], p.mouse_held),
                    Mouse::Release(button) => (vec![event(Release, Some(button))], unheld(button)),
                    Mouse::Move => (vec![event(Motion, p.mouse_held)], p.mouse_held),
                };
                let mode = p.screen().mouse_protocol_mode();
                let x10 = mode == fux_vt::MouseProtocolMode::Press;
                // X10 reports presses alone: a click is its press, and a
                // release or the wheel, which it cannot say, is refused.
                if x10 && matches!(mouse, Mouse::Wheel(_) | Mouse::Release(_)) {
                    return Err(Error::OnlyPresses(p.id));
                }
                for event in events {
                    let release = event.action == Release;
                    if !p.screen().encode_mouse(event, &mut report) && !(release && x10) {
                        return Err(match (event.action, mode) {
                            (_, fux_vt::MouseProtocolMode::None) => Error::NoMouse(p.id),
                            (Motion, _) => Error::NoMotion(p.id),
                            _ => Error::MouseUnencodable {
                                pane: p.id,
                                row,
                                col,
                            },
                        });
                    }
                }
                p.input.push(&report)?;
                p.note_input(&[]);
                p.mouse_held = held;
                Ok(String::new())
            }
            &Command::Split {
                axis,
                target,
                ref cmd,
                ref cwd,
                ref env,
                run,
                ref ids,
                json,
                then: _,
            } => {
                self.want(ids, &[Kind::Pane])?;
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
                let launch = self.launch.as_ref();
                let start = match run {
                    true => Start::Program(cmd, env),
                    false => Start::Shell(cmd, env),
                };
                let pane = new_pane(
                    &self.config,
                    (launch, &self.starting),
                    &mut ids,
                    start,
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
                Ok(self.made_ids((at.0, at.1, new), &[new.to_string()], json))
            }
            &Command::Signal { target, signal, .. } => {
                let group = self.signalled(target, here)?;
                fuxix::process::kill_group(group, signal).map_err(Error::Signal)?;
                Ok(String::new())
            }
            // From the command line each says what it closed, so a script
            // or an agent knows; a key's close shows on the screen anyway.
            &Command::KillPane { target } => {
                if let (Origin::Cli(_), None) = (origin, target) {
                    return Err(Error::KillNeedsTarget("str %N pane kill"));
                }
                let pane = self.pane(target, here)?.id;
                // A tab's last pane takes its tab, and a workspace's last
                // tab its workspace: all of it is said.
                let mut gone = vec![pane.to_string()];
                if let Some((ws, tab)) = self.locate(pane) {
                    let workspace = self.workspace(ws);
                    let tab_panes = (workspace.into_iter())
                        .flat_map(Workspace::tabs)
                        .find(|t| t.id == tab)
                        .and_then(Tab::root)
                        .map_or(0, |root| root.panes().len());
                    if tab_panes == 1 {
                        gone.push(tab.to_string());
                        if workspace.is_some_and(|w| w.tabs().len() == 1) {
                            gone.push(ws.to_string());
                        }
                    }
                }
                self.close_pane(pane, None);
                Ok(closed(origin, &gone))
            }
            &Command::KillTab { target } => {
                if let (Origin::Cli(_), None) = (origin, target) {
                    return Err(Error::KillNeedsTarget("str @N tab kill"));
                }
                let tab = self.tab_target(target, here)?;
                let panes = self.panes_of_tabs(&[tab]);
                self.remove_tab(tab);
                self.after_close();
                Ok(closed(origin, &[vec![tab.to_string()], panes].concat()))
            }
            Command::KillWorkspace { target } => {
                if let (Origin::Cli(_), None) = (origin, target) {
                    return Err(Error::KillNeedsTarget("str WS workspace kill"));
                }
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
            Command::Log { target, file } => {
                let log = match file {
                    Some(file) => Some(open_log(&self.cwd_for(here, None).join(file))?),
                    None => None,
                };
                self.pane_mut(*target, here)?.log = log;
                Ok(String::new())
            }
            &Command::ClearHistory { target } => {
                self.pane_mut(target, here)?.parser.clear_history();
                Ok(String::new())
            }
            &Command::SendKeys {
                client: Some(client),
                ref inputs,
                ..
            } => {
                if !self.views.contains_key(&client) {
                    return Err(Error::NoClient(client));
                }
                // The client takes them as typed at its terminal: its
                // bindings see them.
                self.outbox.push(Outgoing::Keys(client, inputs.clone()));
                Ok(String::new())
            }
            &Command::SendKeys {
                ref targets,
                ref inputs,
                client: None,
                ..
            } => {
                let panes = self.panes_of(targets, here)?;
                // Every pane must take them, or none is sent them.
                for &id in &panes {
                    self.live_pane_mut(Some(id), here)?;
                }
                for id in panes {
                    self.send_inputs(id, inputs)?;
                }
                Ok(String::new())
            }
            &Command::CapturePane {
                target,
                history,
                view,
                json,
                ..
            } => {
                let pane = self.pane(target, here)?;
                let copying = self.copying(pane.id);
                Ok(crate::capture::pane(pane, history, view, json, copying))
            }
            &Command::Reorder {
                ref subject,
                toward,
            } => {
                let target = self.any_target(subject, here)?;
                self.reorder(target, toward).map(|()| String::new())
            }
            Command::Set { option, value } => {
                // `run_command` marks every view to paint, as after any
                // command that can show something new: `titles` shows at
                // each client's next paint.
                self.config.set(option, value).map_err(Error::Config)?;
                self.configured();
                Ok(String::new())
            }
            Command::Resolve { text, kind } => {
                let found = self.resolve(text, *kind, here)?;
                self.made = vec![found];
                Ok(format!("{}\n", describe(found)))
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
                p.note_input(&[Input::Paste(text)]);
                Ok(String::new())
            }
            &Command::Client { client, ref action } => {
                // The client `-c` names, else the one the command came from.
                let (client, place) = match (client, here) {
                    (Some(c), _) => (c, self.place(c).ok_or(Error::NoClient(c))?),
                    (None, Here::Client(client, place)) => (client, place),
                    (None, Here::Cli(..)) => return Err(Error::NoClientGiven),
                };
                match action {
                    ClientAction::Choices(choices) => {
                        let chosen = crate::overlay::choices(self, client, *choices)?;
                        self.made = chosen.ids;
                        self.value = chosen
                            .current
                            .and_then(|i| i64::try_from(i).ok())
                            .map(crate::protocol::Scalar::Int);
                        return Ok(chosen.labels.iter().map(|l| format!("{l}\n")).collect());
                    }
                    ClientAction::Run(code) => {
                        self.outbox.push(Outgoing::Run(client, code.clone()));
                        return Ok(String::new());
                    }
                    ClientAction::Detach
                    | ClientAction::Capture { .. }
                    | ClientAction::Zoom
                    | ClientAction::SelectPane(_)
                    | ClientAction::SelectTab(_)
                    | ClientAction::SelectWorkspace(_) => {}
                }
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
            // Answered before the view is taken (`execute`).
            ClientAction::Choices(_) | ClientAction::Run(_) => Ok(String::new()),
        }
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
                self.check_tab_name(&name, Some(t))?;
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

    /// `ls`: everything, or with `only` that pane, tab or workspace and
    /// what is in it, a line each.
    fn ls_text(&self, only: Option<AnyRef<WsId>>) -> String {
        let mut out = String::new();
        let shows = |ws: WsId, tab: Option<TabId>, pane: Option<PaneId>| match only {
            None => true,
            Some(AnyRef::Workspace(w)) => w == ws,
            Some(AnyRef::Tab(t)) => tab.is_none_or(|tab| tab == t),
            Some(AnyRef::Pane(p)) => pane.is_none_or(|pane| pane == p),
        };
        for ws in &self.workspaces {
            let inside = |tab: TabId| match only {
                Some(AnyRef::Pane(p)) => self.locate(p).is_some_and(|(_, t)| t == tab),
                None | Some(AnyRef::Tab(_) | AnyRef::Workspace(_)) => true,
            };
            let contains = ws
                .tabs()
                .iter()
                .any(|t| inside(t.id) && shows(ws.id, Some(t.id), None));
            if !shows(ws.id, None, None) && !contains {
                continue;
            }
            if matches!(only, None | Some(AnyRef::Workspace(_))) {
                out.push_str(&format!("{} {}\n", ws.id, ws.name));
            }
            for tab in ws
                .tabs()
                .iter()
                .filter(|t| inside(t.id) && shows(ws.id, Some(t.id), None))
            {
                out.push_str(&format!(
                    "  {} {}{}\n",
                    tab.id,
                    tab.name,
                    if tab.root().is_none() { " (empty)" } else { "" }
                ));
                let Some(root) = tab.root() else { continue };
                root.for_each_pane(&mut |pane| {
                    if !shows(ws.id, Some(tab.id), Some(pane)) {
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
        for (socket, ws) in self.grants.iter().filter(|_| only.is_none()) {
            out.push_str(&format!("grant {} {ws}\n", socket.display()));
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

    /// A pane's state, as `ls --json` has it.
    fn pane_json<'a>(&'a self, p: &'a Pane, now: Instant) -> Json<'a> {
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
        Json::Object(fields)
    }

    /// A tab and its panes, as `ls --json` has them.
    fn tab_json<'a>(&'a self, t: &'a Tab, now: Instant) -> Json<'a> {
        let panes = (t.root().map(Tree::panes).unwrap_or_default().into_iter())
            .filter_map(|id| self.panes.get(&id))
            .map(|p| self.pane_json(p, now))
            .collect();
        Json::Object(vec![
            ("id", Json::str(t.id.to_string())),
            ("name", Json::str(t.name.as_str())),
            ("panes", Json::Array(panes)),
        ])
    }

    /// A workspace, its tabs and their panes, as `ls --json` has them.
    fn ws_json<'a>(&'a self, ws: &'a Workspace, now: Instant) -> Json<'a> {
        Json::Object(vec![
            ("id", Json::str(ws.id.to_string())),
            ("name", Json::str(ws.name.as_str())),
            (
                "tabs",
                Json::Array(ws.tabs().iter().map(|t| self.tab_json(t, now)).collect()),
            ),
        ])
    }

    /// `ls -t TARGET`: one pane, tab or workspace, as text or JSON, or one
    /// field of its JSON (`--get`), a string's text as it is.
    fn ls_one(
        &mut self,
        target: AnyRef<WsId>,
        json: bool,
        get: Option<&str>,
    ) -> Result<String, Error> {
        let now = Instant::now();
        let object = match target {
            AnyRef::Pane(id) => self.panes.get(&id).map(|p| self.pane_json(p, now)),
            AnyRef::Tab(id) => self.tab(id).map(|t| self.tab_json(t, now)),
            AnyRef::Workspace(id) => self.workspace(id).map(|w| self.ws_json(w, now)),
        };
        let object = object.ok_or(match target {
            AnyRef::Pane(id) => Error::NoPane(id),
            AnyRef::Tab(id) => Error::NoTab(id),
            AnyRef::Workspace(id) => Error::NoWorkspace(id),
        })?;
        let Some(field) = get else {
            return Ok(match json {
                true => format!("{}\n", object.render()),
                false => self.ls_text(Some(target)),
            });
        };
        let Json::Object(mut fields) = object else {
            return Ok(String::new());
        };
        // A tab of one pane (`run -n NAME`'s) stands for its pane, whose
        // fields it gives too.
        let single = match target {
            AnyRef::Tab(id) => self.root(id).map(Tree::panes).filter(|p| p.len() == 1),
            AnyRef::Pane(_) | AnyRef::Workspace(_) => None,
        };
        let pane = single.and_then(|p| p.first().and_then(|id| self.panes.get(id)));
        if let Some(Json::Object(more)) = pane.map(|p| self.pane_json(p, now)) {
            fields.extend(
                more.into_iter()
                    .filter(|(name, _)| !["id", "name"].contains(name)),
            );
        }
        let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
        let value = fields.into_iter().find(|(name, _)| *name == field);
        use crate::protocol::Scalar;
        let (scalar, text) = match value {
            Some((_, Json::String(text))) => (Scalar::Str(text.to_string()), format!("{text}\n")),
            Some((_, value)) => {
                let scalar = match &value {
                    Json::Number(n) => Scalar::Int(*n),
                    Json::Bool(b) => Scalar::Bool(*b),
                    Json::Null
                    | Json::String(_)
                    | Json::Array(_)
                    | Json::Object(_)
                    | Json::Raw(_) => Scalar::Null,
                };
                (scalar, format!("{}\n", value.render()))
            }
            None => {
                return Err(Error::NoField {
                    target: describe(target),
                    field: field.to_owned(),
                    fields: names.join(", "),
                });
            }
        };
        drop(names);
        self.value = Some(scalar);
        Ok(text)
    }

    /// `ls --json`: everything, or `only` that workspace, with no clients
    /// or grants (what a granted socket sees).
    fn ls_json(&self, only: Option<WsId>) -> String {
        let now = Instant::now();
        let workspaces = (self.workspaces.iter())
            .filter(|ws| only.is_none_or(|id| id == ws.id))
            .map(|ws| self.ws_json(ws, now))
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
            .map(|(socket, ws)| {
                Json::Object(vec![
                    ("socket", Json::str(socket.to_string_lossy())),
                    ("workspace", Json::str(ws.as_str())),
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

/// Shells a pane may run, taking commands typed at them.
const SHELLS: &[&str] = &[
    "bash", "zsh", "fish", "sh", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "elvish", "xonsh",
];

/// What a close from the command line prints: `closed` and the ids gone.
fn closed(origin: &Origin, ids: &[String]) -> String {
    match origin {
        Origin::Cli(_) => format!("closed {}\n", ids.join(" ")),
        Origin::Client(_) | Origin::Nowhere => String::new(),
    }
}

/// What a started pane is told of its server: the socket to reach it by,
/// and which run of it this is.
#[derive(Clone, Debug)]
struct Launcher {
    socket: PathBuf,
    server: String,
}

impl Launcher {
    /// The variables that say so, to pane `id`: the socket to reach the
    /// server by is `socket` if given (a grant's), else its own.
    fn vars(&self, id: PaneId, socket: Option<&Path>) -> [(&'static str, String); 3] {
        let socket = socket.unwrap_or(&self.socket);
        [
            ("FUX_PANE", id.to_string()),
            ("FUX_SOCKET", socket.to_string_lossy().into_owned()),
            ("FUX_SERVER", self.server.clone()),
        ]
    }

    /// Starts `argv` with `env` as pane `id`'s program, in `cwd` at `size`:
    /// told of its server, given its starter's variables (`fresh`), and not
    /// those its starter had not, unless `env` sets them. A shell fux knows
    /// is started so that it marks its commands, unless `set shell-marks
    /// off`; its startup files go beside the server's socket.
    fn spawn(
        &self,
        (config, fresh): (&Config, &Starting),
        id: PaneId,
        (argv, env): (&[String], &[(String, String)]),
        cwd: &Path,
        size: fux_vt::Size,
    ) -> Result<crate::process::Child, Error> {
        let mut dir = self.socket.as_os_str().to_owned();
        dir.push(".shell");
        let marks = (config.shell_marks)
            .then(|| crate::marks::marked(argv, env, Path::new(&dir)))
            .flatten()
            .unwrap_or_else(|| crate::marks::Marked {
                argv: argv.to_vec(),
                ..crate::marks::Marked::default()
            });
        let mut vars = self.vars(id, fresh.socket.as_deref()).to_vec();
        let given = (fresh.set.iter()).chain(env).chain(&marks.env);
        vars.extend(given.map(|(k, v)| (k.as_str(), v.clone())));
        vars.retain(|(k, _)| !marks.unset.iter().any(|u| u == k));
        let mut unset: Vec<String> = (fresh.unset.iter())
            .filter(|name| !env.iter().any(|(k, _)| k == *name))
            .cloned()
            .collect();
        unset.extend(marks.unset.iter().cloned());
        Ok(crate::process::spawn(
            &marks.argv,
            cwd,
            (&vars, &unset),
            size,
        )?)
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
    (launch, fresh): (Option<&Launcher>, &Starting),
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
    // A program brought back by a restore waits for `respawn`.
    let paused = fresh.paused && matches!(start, Start::Program(..));
    if let Some(launcher) = launch.filter(|_| !paused) {
        let child = launcher.spawn((config, fresh), id, (argv, env), cwd, size)?;
        pane.process = Process::Reading(child);
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
        return Err(Error::EmptyName);
    }
    // Control characters, and the invisible ones that change how text
    // around them shows (zero-width, direction overrides).
    let invisible = |c: char| matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
    if name.chars().any(|c| c.is_control() || invisible(c)) {
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
    // A tab's name too: the workspace's would hide it from `-t NAME`.
    let tabs: Vec<String> = (others.iter())
        .flat_map(|ws| ws.tabs())
        .filter(|t| t.name == name)
        .map(|t| t.id.to_string())
        .collect();
    if !tabs.is_empty() {
        return Err(Error::TabNameTaken(name.to_owned(), tabs.join(" ")));
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
    use super::{Origin, Outcome, Session};
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

    /// A session itself, as what a program's requests go to: each is begun
    /// at once, and a wait answered as it stands, there being no programs
    /// to wait on.
    pub(crate) struct InProcess<'s>(pub &'s mut Session);

    impl crate::script::Asks for InProcess<'_> {
        fn ask(
            &mut self,
            request: &crate::protocol::Request,
            out: &mut dyn std::io::Write,
        ) -> Result<crate::protocol::Reply, crate::script::Failed> {
            let session = &mut *self.0;
            let origin = match request.context.client {
                Some(client) if session.views.contains_key(&client) => Origin::Client(client),
                Some(_) | None => Origin::default(),
            };
            let outcome = match session.begin(request.command.clone(), &origin) {
                super::Begun::Done(outcome) => outcome,
                super::Begun::Waiting(mut waiting) => {
                    let later = crate::after(std::time::Instant::now(), waiting.wait.timeout);
                    waiting.answer(session, later).unwrap_or_default()
                }
                super::Begun::Events(_) => {
                    let _ = writeln!(out, "events stream from a real server only");
                    Outcome::default()
                }
            };
            Ok(outcome.into())
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
        let tokens = fuxth::token::source("test", code).map_err(|e| e.to_string())?;
        let mut machine = crate::script::compile(&tokens).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut keymap = std::mem::take(&mut session.test_keymap);
        let context = crate::protocol::Context {
            client,
            ..crate::protocol::Context::default()
        };
        let mut host = crate::script::Host {
            context: context.clone(),
            keymap: Some(&mut keymap),
            last_bound: None,
            out: &mut out,
            settle: false,
        };
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
        let ended = ended.map_err(|e| e.to_string())?;
        let mut text = String::from_utf8_lossy(&out).into_owned();
        for value in &ended.stack {
            text.push_str(&crate::script::display(value));
            text.push('\n');
        }
        Ok(text)
    }

    /// What the server sent clients, taken as their clients take it: a
    /// program to run, keys typed, copy mode ended. The rest is left.
    pub(crate) fn deliver(session: &mut Session) {
        let outbox = std::mem::take(&mut session.outbox);
        let mut kept = Vec::new();
        for outgoing in outbox {
            match outgoing {
                super::Outgoing::Run(client, code) => session.run_as(client, &code),
                super::Outgoing::Ended(client) => {
                    if let Some(keys) = session.test_keys.get_mut(&client) {
                        keys.screen.ended();
                    }
                }
                super::Outgoing::Keys(client, inputs) => {
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
                other @ (super::Outgoing::Bytes(..)
                | super::Outgoing::Exit(..)
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
                request: &crate::protocol::Request,
                out: &mut dyn std::io::Write,
            ) -> Result<crate::protocol::Reply, crate::script::Failed> {
                match &request.command {
                    crate::command::Command::Resolve { .. } => self.0.ask(request, out),
                    crate::command::Command::Client { action, .. } => {
                        self.1.push(action.clone().here());
                        Ok(crate::protocol::Reply::default())
                    }
                    other @ (crate::command::Command::Ls { .. }
                    | crate::command::Command::KillServer { .. }
                    | crate::command::Command::NewWorkspace { .. }
                    | crate::command::Command::NewTab { .. }
                    | crate::command::Command::Split { .. }
                    | crate::command::Command::Run { .. }
                    | crate::command::Command::Respawn { .. }
                    | crate::command::Command::Notify { .. }
                    | crate::command::Command::Resize { .. }
                    | crate::command::Command::Wait { .. }
                    | crate::command::Command::SendMouse { .. }
                    | crate::command::Command::KillPane { .. }
                    | crate::command::Command::Signal { .. }
                    | crate::command::Command::KillTab { .. }
                    | crate::command::Command::KillWorkspace { .. }
                    | crate::command::Command::Rename { .. }
                    | crate::command::Command::MovePane { .. }
                    | crate::command::Command::SwapPane { .. }
                    | crate::command::Command::ResizePane { .. }
                    | crate::command::Command::SendKeys { .. }
                    | crate::command::Command::Events { .. }
                    | crate::command::Command::DumpLayout { .. }
                    | crate::command::Command::Grant { .. }
                    | crate::command::Command::Log { .. }
                    | crate::command::Command::ClearHistory { .. }
                    | crate::command::Command::CapturePane { .. }
                    | crate::command::Command::Reorder { .. }
                    | crate::command::Command::Set { .. }
                    | crate::command::Command::ListBuffers
                    | crate::command::Command::ShowBuffer { .. }
                    | crate::command::Command::PasteBuffer { .. }) => {
                        self.1.push(other.clone());
                        Ok(crate::protocol::Reply::default())
                    }
                }
            }
        }
        let tokens = fuxth::token::source("test", code).map_err(|e| e.to_string())?;
        let mut machine = crate::script::compile(&tokens).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut host = crate::script::Host {
            context: crate::protocol::Context {
                client: Some(client),
                ..crate::protocol::Context::default()
            },
            keymap: None,
            last_bound: None,
            out: &mut out,
            settle: false,
        };
        let mut recorder = Recorder(InProcess(s), Vec::new());
        crate::script::run(&mut machine, &mut host, &mut recorder).map_err(|e| e.to_string())?;
        Ok(recorder.1)
    }

    /// A client's keys, as its client reads them: its decoder, what its
    /// keys are in the middle of, and the screen it has open.
    #[derive(Default)]
    pub(crate) struct Keys {
        held: crate::keymap::Held,
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
            keys.held = crate::keymap::Held::Normal;
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
            use crate::screens::{Effect, Screen};
            for input in inputs {
                let Some(read_only) = self.views.get(&client).map(|v| v.read_only) else {
                    return;
                };
                let keys = self.test_keys.entry(client).or_default();
                let open = keys.screen != Screen::None;
                let act = match input {
                    Input::Key(stroke) if open => {
                        let effects = keys.screen.key(stroke.press);
                        self.effects(client, effects);
                        continue;
                    }
                    Input::Key(stroke) => {
                        let keymap = self.test_keymap.clone();
                        let steps = keys.held.press(&keymap, stroke, read_only);
                        let effects = steps.into_iter().map(|step| match step {
                            crate::keymap::Step::Send(act) => Effect::Send(act),
                            crate::keymap::Step::Run(code) => Effect::Run(code),
                        });
                        self.effects(client, effects.collect());
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
                    Input::Paste(text) if open => {
                        let effects = keys.screen.paste(&text);
                        self.effects(client, effects);
                        continue;
                    }
                    Input::Paste(text) => Act::Paste(text),
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

#[cfg(test)]
mod tests {
    use super::testing::{attached, output, requests, run, started};
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
        run(&mut s, "str history-lines 2 set")?;
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
                .any(|o| matches!(o, Outgoing::Bytes(c, b) if *c == client && b == b"\x1b]52;c;Zm9v\x07"))
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

    /// The `json` outputs, byte for byte: escapes, borrowed names and
    /// formatted IDs alike.
    #[test]
    fn the_json_help_names_every_pane_field() -> Result<(), Box<dyn std::error::Error>> {
        let (s, _) = attached(4, 30)?;
        let pane = s.panes.values().next().ok_or("no pane")?;
        let Json::Object(fields) = s.pane_json(pane, Instant::now()) else {
            return Err("a pane's JSON is an object".into());
        };
        for (name, _) in fields {
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
        let mut s = started(Config {
            history_lines: 2,
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
        let mut s = started(Config {
            history_lines: 2,
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
                r#"no setting "nope"; the settings are shell, history-lines, clipboard, buffers, bell, titles, shell-marks and update-environment"#.to_owned(),
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
        assert!(none.contains("a count from 1 to 1000 (use: "), "{none}");
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
            |code: &str| -> Result<Vec<Result<String, Error>>, Box<dyn std::error::Error>> {
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
            [Err(Error::Config(_))]
        ));
        let kill = Command::KillPane {
            target: Some(PaneId::of(9)),
        };
        assert!(matches!(
            s.execute(&kill, &origin),
            Err(Error::NoPane(p)) if p == PaneId::of(9)
        ));
        assert!(matches!(
            s.attach(10, 40, Some("%1")),
            Err(Error::NoWorkspaceNamed(_))
        ));
        assert!(matches!(
            s.attach(10, 40, Some("+x")),
            Err(Error::Usage(command::Usage::Not(Kind::Workspace, _)))
        ));
        Ok(())
    }
}

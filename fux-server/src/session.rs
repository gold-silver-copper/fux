//! The server's state: workspaces, tabs, panes and the clients' views, and
//! every command that changes them.
use crate::pane::{self, Pane, PaneMut, Process, Runs};
use crate::process::{self, Child};
use crate::view::{ClientKind, Mode, View};
use crate::wait::{Waiting, Watches};
use crate::workspace::{Child as _, Destination, Fit, Parts, Split, Tab, Workspace};
use fux_vt::Size;
use fux_vt::keys::Direction;
use fux_wire::answer::{self, Ended, PaneAnswer, PaneResults};
use fux_wire::code::{Error, Raise, fail};
use fux_wire::command::Start;
use fux_wire::command::{
    self, AnyRef, ClientAction, Command, Extent, Input, Kind, Members, Mouse, MoveTo, PaneResize,
    Request, Resized, Sibling, SwapWith, Way, WsRef,
};
use fux_wire::id::{ClientId, Ids, PaneId, TabId, WsId};
use fux_wire::json::{Fields, Json};
use fux_wire::layout::{self, Axis, NoPane, Placement, Rect, Shares, Tree};
use fux_wire::protocol::{Attach, Body, Reply, Response, ToAttached};
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
mod inspect;
use std::time::{Duration, Instant};

/// What the server must do outside the session.
#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// A frame for a client: bytes for its terminal outside any paint
    /// (questions for the terminal; the bell; the keyboard flags pushed;
    /// clipboard copies),
    /// fuxth to run as its own, keys to take as typed at its terminal
    /// (`send-keys` to a client), its screen ended by the server, or its
    /// detach, saying why.
    To(ClientId, ToAttached<'static>),
    /// Stop the server, saying why, as this actor.
    Shutdown(String, fux_wire::events::Actor),
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
    /// A client's alert notice ends (`View::alert`).
    Notice(ClientId),
}

/// Where a command comes from, which says what it acts on when it names no
/// pane, tab or workspace.
#[derive(Clone, Debug)]
pub enum Origin {
    /// A client's key or prompt: where the client is.
    Client(ClientId),
    /// A request addressing a client, with its separately known source.
    ClientContext {
        client: ClientId,
        actor: fux_wire::events::Actor,
    },
    /// The CLI: who runs the command, and from where.
    Cli(Caller),
}

/// A command line's caller on the CLI.
#[derive(Clone, Debug, Default)]
pub struct Caller {
    /// The pane and program generation it gave (`FUX_PANE`), of `within`.
    pub gave: Option<(PaneId, u64)>,
    /// The server whose pane it runs in, by its `FUX_SERVER`.
    pub within: Within,
    /// Its working directory.
    pub cwd: Option<PathBuf>,
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
            Within::This => self.gave.map(|(pane, _)| pane),
            Within::Unsaid | Within::Other => None,
        }
    }
}

/// A workspace, tab and pane: a client's place, or what a command made.
#[derive(Clone, Copy)]
pub struct Place {
    pub ws: WsId,
    pub tab: TabId,
    pub pane: PaneId,
}

/// A pane's size to begin with, and the cell its program is told it
/// starts in: none in a tab with no host yet (`Session::graphics`).
type Starts = (Size, Option<fux_vt::CellSize>);

/// A pane to make: what it starts, where, and how it starts (`Starts`).
#[derive(Clone, Copy)]
struct NewPane<'a> {
    start: &'a Start,
    cwd: &'a Path,
    size: Size,
    cell: Option<fux_vt::CellSize>,
    options: command::PaneOptions,
}

impl<'a> NewPane<'a> {
    fn new(
        start: &'a Start,
        cwd: &'a Path,
        options: command::PaneOptions,
        (size, cell): Starts,
    ) -> Self {
        Self {
            start,
            cwd,
            options,
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

impl From<pane::Error> for Error {
    fn from(error: pane::Error) -> Self {
        match error {
            pane::Error::Terminal { .. } => fail(Raise::Start, error.to_string()),
        }
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
        Process::ReadFailed(_, error) => {
            return Some(fail(
                Raise::NotRunning,
                format!("{}: reading program terminal failed: {error}", pane.id()),
            ));
        }
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
    let told = format!("the program in {pane} {was}{}", answer::told(status));
    fail(Raise::NotRunning, told)
}

/// What a command line from the CLI begins: its outcome, now, or a wait
/// for one (`wait`, or an action given wait words).
pub enum Begun {
    Done(Reply),
    Waiting(Box<Waiting>),
}

/// A respawn under way: the pane made for the program started, and the
/// program, which take the place of the pane's own once all its old
/// program left is read, in turns as any pane's output is
/// (`Session::after_turn`). The new program's output waits on its
/// terminal till then. If the pane closes first, the respawn is abandoned
/// (`Session::end_pane`), and said to be.
struct Replacement {
    actor: fux_wire::events::Actor,
    fresh: Pane,
    process: Process,
    /// The program it is respawned over, while what it left on its
    /// terminal is still being read (`Session::read_left`).
    old: Option<Child>,
    /// When the old program's grace after the hangup ends: then it is
    /// killed, and what it left is still read. `None` once it is.
    grace: Option<Instant>,
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
    /// Programs whose exit is told, their terminal still read till it is
    /// empty or closed (`read_left`); then their pane is frozen (D-90).
    leftover: BTreeMap<PaneId, Child>,
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
    pub(crate) test_keymap: fux::client_state::Draft,
}

/// A value a command can't take, as `Usage` says why.
fn bad_value(usage: command::Usage) -> Error {
    fail(Raise::BadValue, usage.to_string())
}

/// The answer of an action on a pane (keys, a mouse event, a signal, a
/// program started): its version before it acted, or as it starts; what a
/// wait for its effect waits to differ from.
fn acted(version: command::Version) -> Reply {
    PaneResults::One(PaneAnswer::Acted(version)).into()
}

/// A command that failed, told with its code (its status: 1, 4 or 124).
fn failure(error: Error, origin: &Origin, session: &Session) -> Reply {
    // A command that needed its caller's pane, tab or workspace, from a
    // caller whose `FUX_PANE` is another server's pane, or no server's it
    // says: why it got none.
    let error = match (error, origin) {
        (
            Error::NoPaneGiven | Error::NoWorkspaceGiven,
            Origin::Cli(Caller {
                gave: Some((pane, generation)),
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
                format!(
                    "FUX_PANE {pane}:{generation} {whose}, so it is not acted on; name the pane"
                ),
            )
        }
        (
            Error::NoPaneGiven | Error::NoWorkspaceGiven,
            Origin::Cli(Caller {
                gave: Some((pane, generation)),
                within: Within::This,
                ..
            }),
        ) => match session.parts.panes().contains_key(pane) {
            true => fail(
                Raise::StrayPane,
                format!("FUX_PANE {pane}:{generation} no longer identifies its program"),
            ),
            false => Error::NoPane(*pane),
        },
        (error, _) => error,
    };
    Reply::failed(error.code(), error.to_string())
}

impl Session {
    pub fn new(
        socket: PathBuf,
        launch: bool,
        run: command::Run,
        journal: command::JournalEntries,
    ) -> Self {
        Self {
            parts: Parts::new(journal),
            views: BTreeMap::new(),
            launch: launch.then_some(socket),
            exe: std::env::current_exe().ok(),
            run,
            outbox: Vec::new(),
            dying: Vec::new(),
            respawning: BTreeMap::new(),
            leftover: BTreeMap::new(),
            shown_sizes: Vec::new(),
            placed: Placement::default(),
            sized: Vec::new(),
            ids: Ids::default(),
            changes: 0,
            #[cfg(test)]
            test_keys: BTreeMap::new(),
            #[cfg(test)]
            test_keymap: fux::client_state::Draft::default(),
        }
    }

    /// One workspace, holding one tab with its explicit launch, laid out on a
    /// client's screen of `size`.
    pub fn start(
        &mut self,
        size: Size,
        initial: &command::InitialWorkspace,
        now: Instant,
    ) -> Result<(), Error> {
        if !initial.cwd.is_absolute() {
            return Err(fail(
                Raise::Start,
                "the caller's own directory is unknown; give an absolute cwd",
            ));
        }
        let new = NewPane::new(&initial.start, &initial.cwd, initial.options, (size, None));
        let made = vec![initial.name.clone()];
        (self.create_workspaces(None, made, None, Fit::Clients(size), new, now)).map(drop)
    }

    // ---------------------------------------------------------------- lookup

    pub fn workspace(&self, id: WsId) -> Option<&Workspace> {
        self.parts.workspace(id)
    }
    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.parts.tab(id).map(|(_, tab)| tab)
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
        self.parts
            .tab(tab)
            .map(|(ws, _)| ws)
            .ok_or(Error::NoTab(tab))
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
    /// The workspace client `client` shows that holds the tab it shows.
    pub fn shown_workspace(&self, client: ClientId) -> Option<&Workspace> {
        self.parts.shown(client).map(|(ws, _)| ws)
    }
    /// The tab client `client` shows.
    pub fn shown_tab(&self, client: ClientId) -> Option<&Tab> {
        self.parts.shown(client).map(|(_, tab)| tab)
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
    fn resolve_ws(&self, r: &WsRef) -> Result<WsId, Error> {
        match r {
            WsRef::New => Err(fail(
                Raise::BadValue,
                "new-workspace is a creation destination, not an existing workspace",
            )),
            WsRef::Id(id) => Ok(self.workspace(*id).ok_or(Error::NoWorkspace(*id))?.id()),
            WsRef::Name(name) => match self.named(name) {
                Some(AnyRef::Workspace(id)) => Ok(id),
                _ => Err(fail(
                    Raise::NoWorkspace,
                    format!("no workspace named {name:?}"),
                )),
            },
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
            Origin::Client(client) | Origin::ClientContext { client, .. } => {
                let place = self.place(client).ok_or(Error::NoClient(client))?;
                Ok(Here::Client(client, place))
            }
            Origin::Cli(ref caller) => {
                let pane = caller
                    .pane()
                    .filter(|p| {
                        caller.gave.is_some_and(|(_, generation)| {
                            let current = self
                                .respawning
                                .get(p)
                                .map(|new| &new.fresh)
                                .or_else(|| self.parts.panes().get(p));
                            current.is_some_and(|pane| pane.respawns == generation)
                        })
                    })
                    .map(|p| (p, self.locate(p).map(|(ws, _)| ws)));
                Ok(Here::Cli(pane, caller.cwd.as_deref()))
            }
        }
    }

    /// Where client `client` is, if it is attached: the workspace it is
    /// shown, while there is any.
    pub fn place(&self, client: ClientId) -> Option<Place> {
        let ((ws, tab), pane) = self.parts.place(client)?;
        Some(Place { ws, tab, pane })
    }

    fn pane(&self, id: PaneId) -> Result<&Pane, Error> {
        self.parts.panes().get(&id).ok_or(Error::NoPane(id))
    }

    fn pane_mut(&mut self, id: PaneId) -> Result<PaneMut<'_>, Error> {
        self.parts.pane_mut(id).ok_or(Error::NoPane(id))
    }

    /// Starts `launch` as pane `id`'s program, in its `cwd` at `size` in
    /// cells of `cell`, if panes start real processes: in the server's
    /// environment, told of its server (`FUX_PANE`, `FUX_SOCKET`,
    /// `FUX_SERVER`, and `FUX`, the fux running it, unset if it cannot be
    /// had).
    fn spawn(
        &self,
        id: PaneId,
        generation: u64,
        launch: &pane::Launch,
        size: (Size, Option<fux_vt::CellSize>),
    ) -> Result<Option<Child>, Error> {
        let Some(socket) = &self.launch else {
            return Ok(None);
        };
        let mut vars: Vec<(&str, std::ffi::OsString)> = vec![
            ("FUX_PANE", format!("{id}:{generation}").into()),
            ("FUX_SOCKET", socket.as_os_str().to_owned()),
            ("FUX_SERVER", self.run.to_string().into()),
        ];
        vars.extend((self.exe.as_ref()).map(|exe| ("FUX", exe.as_os_str().to_owned())));
        // No `FUX` to give: not one the server was started with either.
        let unset: &[&str] = match self.exe {
            Some(_) => &[],
            None => &["FUX"],
        };
        let child = process::spawn(&launch.argv.argv(), &launch.cwd, (&vars, unset), size)
            .map_err(|m| fail(Raise::Start, m))?;
        Ok(Some(child))
    }

    /// Starts the program pane `id` ran again in it: its words, or `argv`
    /// if given, where it ran; first
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
        argv: Option<&command::Program>,
        now: Instant,
    ) -> Result<Reply, Error> {
        let pane = self.pane(id)?;
        let Runs::Explicit(launch) = &pane.runs else {
            let message =
                format!("{id} runs a shell; respawn restarts a program a pane was started with");
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
        if let Some(argv) = argv {
            launch.argv = argv.clone();
        }
        // The program it starts is told its host's cell.
        let graphics = self.graphics(id);
        let size = (pane.size(), graphics.cell());
        let respawns = pane.respawns.checked_add(1).ok_or_else(|| {
            fail(
                Raise::IdsExhausted,
                format!("{id}'s program generations exhausted"),
            )
        })?;
        let exiting = pane.exiting;
        // All that can refuse it is done before its program starts, which
        // nothing can then leave running.
        let runs = Runs::Explicit(launch.clone());
        let mut fresh = Pane::new(id, None, runs, size.0, pane.options(), now)?;
        fresh.respawns = respawns;
        // Its program starts sized in that cell, so no resize is due.
        let _ = fresh.host(Default::default(), Default::default(), graphics);
        let child = self.spawn(id, respawns, &launch, size)?;
        // Once another started (a start refused leaves it running), its
        // program is taken, and input is refused till the new one takes its
        // place. One that runs is hung up; it, or one whose exit waits on
        // what it left, is kept till what it left is read, over as many
        // turns as that takes or its grace lasts (`read_left`); one done
        // with is let go.
        let draining = self.pane(id).is_ok_and(|p| p.draining);
        let mut old = None;
        let taken = self.pane_mut(id).ok().and_then(|mut p| p.let_go());
        if let Some(mut child) = taken.or_else(|| self.leftover.remove(&id)) {
            if exiting.is_none() {
                child.leader.closed();
                child.leader.hang_up();
            }
            match exiting.is_none() || draining {
                true => old = Some(child),
                false => self.dying.push(Dying {
                    child,
                    deadline: now,
                }),
            }
        }
        let started = fresh.version(self.run);
        let process = child.map_or(Process::Absent, Process::Reading);
        let since = now;
        let new = Replacement {
            actor: self.parts.journal().current_actor(),
            fresh,
            process,
            old,
            grace: Some(crate::after(now, GRACE)),
            since,
        };
        self.respawning.insert(id, new);
        if let Ok(mut pane) = self.pane_mut(id) {
            pane.drain(true);
        }
        self.read_left(id, true, now);
        self.after_turn(id, now);
        Ok(acted(started))
    }

    /// What ends a turn of pane `id`'s reads: once nothing read waits, a
    /// program that exited with output unread exits, and a respawn under
    /// way takes its place, once a frame its old program holds is read
    /// too, by a turn of its own (`fire`).
    fn after_turn(&mut self, id: PaneId, now: Instant) {
        let respawning = self.respawning.contains_key(&id);
        let waits = |p: &&Pane| p.has_unread() || respawning && p.frame_deadline().is_some();
        let Some(pane) = self.parts.panes().get(&id).filter(|p| !waits(p)) else {
            return;
        };
        if let Some(ended) = pane.exiting {
            self.exited(id, ended, now);
        }
        // A respawn takes its place once what the old program left is read.
        if self.parts.panes().get(&id).is_some_and(|p| p.draining) {
            return;
        }
        let Some(mut new) = self.respawning.remove(&id) else {
            return;
        };
        let actor = self.parts.actor(new.actor);
        let Some(mut pane) = self.parts.pane_mut(id) else {
            self.parts.actor(actor);
            self.end(new.process, true, now);
            return;
        };
        // At the pane's size and history now, which may have changed while
        // it waited.
        let size = pane.size();
        new.fresh.set_history_bytes(pane.options().history_bytes);
        let (old, left) = pane.respawned(new.fresh, new.process);
        pane.resize(size, now);
        // An exit that came while it was no pane's program is told now: the
        // server looks for exits as they are signalled (`Server::reap`).
        let exited = pane.process().child().and_then(|c| c.leader.exited());
        self.parts.actor(actor);
        self.end(old, true, now);
        let left = left.told();
        if !left.is_empty() {
            let told = format!("{id} respawned{left}");
            for client in self.viewers(AnyRef::Pane(id)) {
                self.notify_to(client, told.clone(), true);
            }
        }
        if let Some(ended) = exited {
            self.exited(id, ended, now);
        }
        // Hosted as its pane is now.
        self.touch();
        self.settle(now);
    }

    /// The process group `fux signal` signals in a pane: the terminal's
    /// foreground group (a job its shell runs), or the program it was
    /// started with; never the shell of a pane that runs one.
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

    /// The workspace, tab or pane named `name`: one at most, as every
    /// name is checked by `check_free` as it is given.
    fn named(&self, name: &str) -> Option<AnyRef> {
        let workspaces = self.parts.every_workspace();
        let workspace = workspaces.iter().find(|w| w.name() == Some(name));
        let tab = || (workspaces.iter().flat_map(|w| w.tabs())).find(|t| t.name() == Some(name));
        let pane = || (self.parts.panes().values()).find(|p| p.name() == Some(name));
        (workspace.map(|w| AnyRef::Workspace(w.id())))
            .or_else(|| tab().map(|t| AnyRef::Tab(t.id())))
            .or_else(|| pane().map(|p| AnyRef::Pane(p.id())))
    }

    /// Refuses `name` for `naming` (none: one not made yet) if it cannot be
    /// given: one a target reads as an id, or one another workspace, tab or
    /// pane has, since the three share one namespace.
    fn check_free(&self, name: &str, naming: Option<AnyRef>) -> Result<(), Error> {
        check_name(name)?;
        match self.named(name) {
            Some(holder) if Some(holder) != naming => {
                Err(Error::NameTaken(name.to_owned(), holder))
            }
            _ => Ok(()),
        }
    }

    /// What `id`, written `text`, stands for as a `kind`: itself; a pane's
    /// or tab's id, its tab or workspace. Downward lookups require a member.
    fn stands_for(&self, id: AnyRef, text: &str, kind: Kind) -> Result<AnyRef, Error> {
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
            (AnyRef::Tab(_) | AnyRef::Workspace(_), Kind::Pane)
            | (AnyRef::Workspace(_), Kind::Tab) => Err(fail(
                Raise::NoTarget,
                format!(
                    "{text} is a {}, not a {}: choose a member explicitly",
                    id.kind().name(),
                    kind.name()
                ),
            )),
        }
    }

    /// What `text` names as a pane, tab or workspace, `kind`: its id (`%3`),
    /// or its given name.
    /// A pane, nothing given: the caller's own.
    fn resolve(&self, text: &str, kind: Kind, here: Here) -> Result<AnyRef, Error> {
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
            // Its given name: never its display label or title.
            Kind::Pane | Kind::Tab => (self.named(text))
                .filter(|found| found.kind() == kind)
                .ok_or_else(|| {
                    let code = match kind {
                        Kind::Pane => Raise::NoPane,
                        Kind::Tab => Raise::NoTab,
                        Kind::Workspace => Raise::NoWorkspace,
                    };
                    fail(code, format!("no {} named {text:?}", kind.name()))
                }),
            Kind::Workspace => {
                let ws = command::parse_workspace(text).map_err(bad_value)?;
                Ok(AnyRef::Workspace(self.ws_target(Some(&ws), here)?))
            }
        }
    }

    /// The tabs in `scope`, in order: a tab itself, a workspace's and
    /// those of the workspaces in it, or with none every one; a pane holds
    /// none.
    fn tabs_of(&self, scope: Option<AnyRef>) -> Result<Vec<TabId>, Error> {
        let tabs = |ws: Option<WsId>| {
            let tabs = self.parts.tabs_in(ws).into_iter();
            tabs.map(|(_, tab)| tab.id()).collect()
        };
        Ok(match scope.map(|target| self.there(target)).transpose()? {
            None => tabs(None),
            Some(AnyRef::Workspace(id)) => tabs(Some(id)),
            Some(AnyRef::Tab(tab)) => vec![tab],
            Some(AnyRef::Pane(_)) => Vec::new(),
        })
    }

    /// The panes in `scope`, in layout order: a pane itself, the panes of
    /// the tabs in it (`tabs_of`).
    fn panes_of(&self, scope: Option<AnyRef>) -> Result<Vec<PaneId>, Error> {
        if let Some(AnyRef::Pane(id)) = scope {
            return Ok(vec![self.pane(id)?.id()]);
        }
        let tabs = self.tabs_of(scope)?;
        Ok((tabs.into_iter().filter_map(|t| self.root(t)))
            .flat_map(Tree::panes)
            .collect())
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
        // made first.
        let (run, mut each) = (self.run, Vec::new());
        for id in self.panes_of(Some(to.target()))? {
            let pane = self.live_pane_mut(id)?;
            let bytes = bytes(&pane, inputs);
            each.push((pane.version(run), bytes));
        }
        let mut answers = each.iter().map(|(version, _)| PaneAnswer::Acted(*version));
        let mut reply: Reply = match (to.target(), answers.next()) {
            (AnyRef::Pane(_), Some(one)) => PaneResults::One(one),
            (_, first) => PaneResults::Many(first.into_iter().chain(answers).collect()),
        }
        .into();
        reply.run = Some(self.run);
        for (version, bytes) in &each {
            self.pane_mut(version.pane)?.push(bytes);
        }
        Ok(reply)
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
    /// unzoomed if `followers` focus it. Shown on no screen (the tab zoomed
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
        let zoomed = (self.tab(split.tab()).and_then(Tab::zoomed)).filter(|_| followers.is_empty());
        let (mut placed, mut shown) = (Placement::default(), Vec::new());
        self.shown_in((split.tab(), split.root()), zoomed, &mut placed, &mut shown);
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
        let mut allocation = usize::MAX;
        self.dir_with(pane, &mut allocation).ok()?.ok().flatten()
    }

    fn dir_with<A: fuxix::Allocation>(
        &self,
        pane: PaneId,
        budget: &mut A,
    ) -> Result<fuxix::Result<Option<PathBuf>>, A::Error> {
        let Some(p) = self.parts.panes().get(&pane) else {
            return Ok(Ok(None));
        };
        let child = p
            .process()
            .child()
            .or_else(|| self.respawning.get(&pane)?.process.child());
        match child {
            Some(child) => fuxix::process::cwd_observed(child.leader.pid(), budget),
            None => Ok(Ok(None)),
        }
    }

    /// Where a new pane's program starts: `dir`, from the caller's
    /// directory, or with none the directory of the caller, or of `near`.
    /// A CLI caller's own directory that is unknown fails the start;
    /// A pane's unreadable nearby directory also refuses the start.
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
        let base = known.ok_or_else(|| {
            fail(
                Raise::Start,
                "the nearby working directory is unknown; give an absolute cwd",
            )
        })?;
        Ok(match dir {
            Some(dir) => base.join(dir),
            None => base,
        })
    }

    /// Workspaces `made`, named as each says, each in the one before and
    /// the first in `parent` (none: at the top), the last with one tab
    /// named `tab_name`, laid out on `fit`, holding one new pane.
    fn create_workspaces(
        &mut self,
        parent: Option<WsId>,
        made: Vec<Option<String>>,
        tab_name: Option<String>,
        fit: Fit,
        new: NewPane<'_>,
        now: Instant,
    ) -> Result<Place, Error> {
        for name in made.iter().flatten() {
            self.check_free(name, None)?;
        }
        // The IDs are taken before the pane starts, so that none can run out
        // after, and are committed once it has.
        let mut ids = self.ids.clone();
        let made = (made.into_iter())
            .map(|name| Ok((ids.workspace()?, name)))
            .collect::<Result<Vec<_>, Error>>()?;
        let ws = made
            .last()
            .map(|(id, _)| *id)
            .ok_or(Error::NoWorkspaceGiven)?;
        let tab = ids.tab()?;
        let pane = self.new_pane(&mut ids, new, now)?;
        let place = Place {
            ws,
            tab,
            pane: pane.id(),
        };
        if let Err(pane) = (self.parts).add_workspaces(parent, made, (tab, tab_name, fit), pane) {
            self.end(pane.into_process(), true, now);
            return Err(parent.map_or(Error::NoWorkspaceGiven, Error::NoWorkspace));
        }
        self.ids = ids;
        Ok(place)
    }

    /// Where `in`'s workspaces `path` put a new tab named `tab`: the
    /// workspace there that holds the rest, and the names of the rest, made
    /// in it, each in the one before. Each from the first not there is
    /// made; one there is in the one before it, and the first anywhere.
    /// With none, `here`'s.
    fn nesting(
        &self,
        path: &[WsRef],
        here: Here,
        tab: Option<&str>,
    ) -> Result<(Option<WsId>, Vec<Option<String>>), Error> {
        if path.is_empty() {
            return Ok((Some(here.ws().ok_or(Error::NoWorkspaceGiven)?), Vec::new()));
        }
        let (mut parent, mut made) = (None, Vec::<Option<String>>::new());
        for (i, r) in path.iter().enumerate() {
            let found = match r {
                WsRef::Id(id) => Some(self.workspace(*id).ok_or(Error::NoWorkspace(*id))?.id()),
                WsRef::Name(name) => match self.named(name) {
                    Some(AnyRef::Workspace(id)) => Some(id),
                    Some(holder) => return Err(Error::NameTaken(name.clone(), holder)),
                    None => None,
                },
                WsRef::New => None,
            };
            match (found, r) {
                (Some(id), _) if made.is_empty() => {
                    if i > 0 && self.parts.parent(id) != Some(parent) {
                        let p = parent.map_or_else(String::new, |p| p.to_string());
                        let why = format!("{} is not in {p}", AnyRef::Workspace(id));
                        return Err(fail(Raise::BadValue, why));
                    }
                    parent = Some(id);
                }
                (Some(id), _) => {
                    let why = format!("{} is not in a workspace made here", AnyRef::Workspace(id));
                    return Err(fail(Raise::BadValue, why));
                }
                (None, WsRef::Name(name)) => {
                    if tab == Some(name) || made.contains(&Some(name.clone())) {
                        let why = format!(
                            "a workspace and its tab or another workspace made with it cannot both be named {name:?}"
                        );
                        return Err(fail(Raise::NameTaken, why));
                    }
                    made.push(Some(name.clone()));
                }
                (None, WsRef::Id(_) | WsRef::New) => made.push(None),
            }
        }
        Ok((parent, made))
    }

    /// Adds a tab named `name` (checked by `new-tab`) to the workspace
    /// `in`'s `path` names (`nesting`), or else `here`'s, laid out on
    /// `fit`, holding one new pane; the workspaces it names that are not
    /// there are made, the tab the first child of the last.
    fn create_tab(
        &mut self,
        path: &[WsRef],
        here: Here,
        name: Option<String>,
        fit: Fit,
        new: NewPane<'_>,
        now: Instant,
    ) -> Result<Place, Error> {
        if let Some(name) = &name {
            self.check_free(name, None)?;
        }
        let (parent, made) = self.nesting(path, here, name.as_deref())?;
        let id = match (parent, made.is_empty()) {
            (Some(id), true) => id,
            (parent, _) => return self.create_workspaces(parent, made, name, fit, new, now),
        };
        // Found before its pane's program starts, which nothing can then
        // leave running. As for a workspace: the ID first, committed after
        // the pane.
        self.workspace(id).ok_or(Error::NoWorkspace(id))?;
        let mut ids = self.ids.clone();
        let tab = ids.tab()?;
        let pane = self.new_pane(&mut ids, new, now)?;
        let made = Place {
            ws: id,
            tab,
            pane: pane.id(),
        };
        if let Err(pane) = self.parts.add_tab((id, tab), name, fit, pane) {
            self.end(pane.into_process(), true, now);
            return Err(Error::NoWorkspace(id));
        }
        self.ids = ids;
        Ok(made)
    }

    /// A new pane, its ID taken from `ids`, running what `new` says.
    fn new_pane(&self, ids: &mut Ids, new: NewPane<'_>, now: Instant) -> Result<Pane, Error> {
        let start = new.start;
        let launch = pane::Launch {
            argv: start.argv().clone(),
            cwd: new.cwd.to_path_buf(),
        };
        let runs = match start {
            Start::Shell(..) => Runs::Shell(launch.clone()),
            Start::Program(..) => Runs::Explicit(launch.clone()),
        };
        let id = ids.pane()?;
        let pane = Pane::new(id, None, runs, new.size, new.options, now)?;
        let child = self.spawn(id, 0, &launch, (new.size, new.cell))?;
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
        let state = fux::script::default_state().map_err(|e| fail(Raise::Config, e.to_string()))?;
        self.attach_as(
            Attach {
                initial: workspace.is_none().then(|| command::InitialWorkspace {
                    name: None,
                    start: command::Start::Shell(state.preferences.shell.clone()),
                    options: state.preferences.pane_options(),
                    cwd: std::env::current_dir().unwrap_or_default(),
                }),
                target: workspace.map(command::AttachTarget::Workspace),
                ..Attach::of(size, state.preferences.presentation())
            },
            Instant::now(),
        )
    }

    /// A client attaches as `attach` says: one that only watches is one
    /// from the start, and sizes nothing.
    pub fn attach_as(&mut self, attach: Attach, now: Instant) -> Result<ClientId, Error> {
        let client = self.ids.clone().client()?;
        let previous = self.parts.actor(fux_wire::events::Actor::Client(client));
        let result = self.attach_checked(attach, now);
        self.parts.actor(previous);
        result
    }

    fn attach_checked(&mut self, attach: Attach, now: Instant) -> Result<ClientId, Error> {
        self.check_run(attach.of)?;
        if attach.initial.is_some() && (attach.target.is_some() || attach.read_only) {
            return Err(fail(
                Raise::BadValue,
                "initial launch inputs require an untargeted writable attach",
            ));
        }
        // A size no view has is refused before anything starts on it.
        let (rows, cols) = attach.size.nonzero();
        let (rows, cols) = fux_wire::command::size(rows.get(), cols.get())?;
        // Its first pane focused, as every new seat is, or the zoomed one:
        // a tab's zoom stays.
        let at = match &attach.target {
            Some(command::AttachTarget::Workspace(workspace)) => {
                AnyRef::Workspace(self.resolve_ws(workspace)?)
            }
            Some(command::AttachTarget::Tab(tab)) => self.there(AnyRef::Tab(*tab))?,
            Some(command::AttachTarget::Pane(pane)) => self.there(AnyRef::Pane(*pane))?,
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
                    let size = Size::new(
                        rows.saturating_sub(u16::from(
                            attach.presentation.bar.position()
                                != fux_wire::presentation::BarPosition::Off,
                        )),
                        cols,
                    )
                    .map_err(|_| {
                        let why =
                            format!("a screen of {rows} rows has no room for a pane below its bar");
                        fail(Raise::Size, why)
                    })?;
                    let initial = attach.initial.as_ref().ok_or_else(|| {
                        let why =
                            "the server holds no workspace, and the attach no launch to start one";
                        fail(Raise::Start, why)
                    })?;
                    self.start(size, initial, now)?;
                }
                let first = self.parts.workspaces().first().map(|w| w.id());
                let why = "the server has no workspace";
                AnyRef::Workspace(first.ok_or_else(|| fail(Raise::NoWorkspace, why))?)
            }
        };
        let id = self.ids.client()?;
        let mut view = View::new(id, rows, cols, attach.presentation);
        view.kind = match attach.headless {
            true => ClientKind::Headless,
            false => ClientKind::Terminal,
        };
        view.read_only = attach.read_only;
        // The pane it runs in, if any: the one whose program it descends
        // from, or whose session it is in, as the OS says (a parent between
        // them may have gone). A headless client shows on no terminal, so on
        // none.
        let client = attach.ancestry.first();
        let session = client.and_then(|pid| process::Pid::from_raw(i32::try_from(*pid).ok()?));
        let session = session.and_then(fuxix::process::session);
        let leads = |pane: &Pane| {
            let Some(leader) = pane.process().child().map(|c| c.leader.pid()) else {
                return false;
            };
            let descends =
                u32::try_from(leader.as_raw()).is_ok_and(|pid| attach.ancestry.contains(&pid));
            !attach.headless && (descends || session == Some(leader))
        };
        let runs_in = self.parts.panes().iter().find(|(_, pane)| leads(pane));
        view.runs_in = runs_in.map(|(id, _)| *id);
        self.views.insert(id, view);
        self.parts.attach(id, at);
        self.view_event(id, crate::journal::ViewChange::Attached);
        self.ask(id, crate::outer::QUERIES);
        self.settle(now);
        Ok(id)
    }

    pub fn detach(&mut self, id: ClientId, now: Instant) {
        if self.views.contains_key(&id) {
            self.view_event(id, crate::journal::ViewChange::Detached);
        }
        self.views.remove(&id);
        self.parts.forget(id);
        self.settle(now);
    }

    /// The command's client follows what it made or moved.
    fn follow(&mut self, here: Here, to: AnyRef) {
        if let Here::Client(client, _) = here
            && self.views.contains_key(&client)
        {
            self.parts.show(client, to);
        }
    }

    /// A client's terminal is `rows` by `cols`: its view takes the size
    /// and settles if that changes it, and says whether it did; a size no
    /// view has (`command::size`) is refused, and the view kept.
    pub fn resize(
        &mut self,
        id: ClientId,
        rows: u16,
        cols: u16,
        now: Instant,
    ) -> Result<bool, Error> {
        let size = fux_wire::command::size(rows, cols)?;
        let view = self.views.get_mut(&id).filter(|v| (v.rows, v.cols) != size);
        let Some(view) = view else { return Ok(false) };
        (view.rows, view.cols) = size;
        view.dirty = true;
        self.view_event(id, crate::journal::ViewChange::Resized);
        self.settle(now);
        Ok(true)
    }

    /// Gives a client a notice, an error's if `error`, if it is still
    /// attached.
    pub fn notify_to(&mut self, client: ClientId, text: impl Into<String>, error: bool) {
        if let Some(view) = self.views.get_mut(&client) {
            view.notify(text.into(), error);
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
        match self.shown_tab(view.id) {
            Some(tab) => lay_out(tab.root(), tab.zoomed(), view.pane_area(), out),
            None => out.clear(),
        }
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
        (self.views.values()).filter(move |v| self.shows(v.id, tab) && self.sizes(v, tab))
    }

    /// Whether `view` sizes tab `tab`'s panes as it shows it: not if its
    /// client runs in one of them (`View::runs_in`), whose terminal would
    /// follow what it is sized to.
    fn sizes(&self, view: &View, tab: TabId) -> bool {
        let own = view.runs_in.and_then(|pane| self.locate(pane));
        view.sizes() && own.is_none_or(|(_, t)| t != tab)
    }

    /// The size a client's screen gives panes, below its bar, if any.
    fn screen_size(view: &View) -> Option<Size> {
        let area = view.pane_area();
        Size::new(area.h(), area.w()).ok()
    }

    // -------------------------------------------------------------- repair

    /// Ends each view's mode if what it shows is gone, then sizes the PTYs:
    /// run after every event.
    pub fn settle(&mut self, now: Instant) {
        // Out of the session while they are repaired, which reads no view.
        let mut views = std::mem::take(&mut self.views);
        let ended: Vec<Outgoing> = (views.values_mut())
            .filter_map(|view| Some(Outgoing::To(view.id, ToAttached::Ended(self.repair(view)?))))
            .collect();
        self.views = views;
        for ended in &ended {
            if let Outgoing::To(client, _) = ended {
                self.view_event(*client, crate::journal::ViewChange::Screen);
            }
        }
        self.outbox.extend(ended);
        self.size_panes(now);
        self.host_panes();
        self.tell_schemes();
        self.tell_lost();
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
            for client in self.viewers(AnyRef::Pane(id)) {
                if let Some(view) = self.views.get_mut(&client) {
                    view.dirty = true;
                }
            }
            if let Some((mut pane, true)) = self.hosted(id) {
                pane.size_terminal();
            }
        }
    }

    /// Ends what a view shows if what it is about is gone: a chooser about
    /// a pane, tab or workspace that closed. Which opening it ended, if it did, which
    /// its client is told (`ToAttached::Ended`), the mode being the client's.
    fn repair(&self, view: &mut View) -> Option<u64> {
        use fux_wire::act::{List, Panel};
        let gone = match &view.mode {
            Mode::Panel(Panel::List(List {
                about: Some(about), ..
            })) if self.there(*about).is_err() => format!("closed: {} is gone", about),
            Mode::Normal | Mode::Panel(_) => return None,
        };
        view.mode = Mode::Normal;
        view.notify(gone, true);
        Some(view.opening)
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
    fn size_panes(&mut self, now: Instant) {
        let mut sizes = std::mem::take(&mut self.shown_sizes);
        let mut placed = std::mem::take(&mut self.placed);
        sizes.clear();
        // Each tab a client sizes, and its smallest screen.
        let mut screens = BTreeMap::new();
        for view in self.views.values() {
            let size = Self::screen_size(view).map(Size::nonzero);
            if let (Some(tab), Some((rows, cols))) = (self.shown_tab(view.id), size)
                && self.sizes(view, tab.id())
            {
                let (r, c) = screens.entry(tab.id()).or_insert((rows, cols));
                (*r, *c) = ((*r).min(rows), (*c).min(cols));
            }
        }
        for (&tab, &size) in &screens {
            self.parts.shown_on(tab, size.into());
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
                let zoomed = self.tab(tab).and_then(Tab::zoomed);
                self.shown_in((tab, root), zoomed, &mut placed, &mut sizes);
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
                self.read_with(id, |pane| pane.resize(size, now), now);
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
    /// `root` and zoomed on `zoomed` is shown in, into `out` (empty ones
    /// left out): on the screen of each client that sizes the tab; with
    /// none, on what the tab's fit says. Settling sizes a pane to the
    /// smallest it has here, and leaves one with none at its size.
    fn shown_in(
        &self,
        (tab, root): (TabId, &Tree),
        zoomed: Option<PaneId>,
        placed: &mut Placement,
        out: &mut Vec<(PaneId, (u16, u16))>,
    ) {
        let Some(t) = self.tab(tab) else {
            return;
        };
        let mut sizing = self.sizing(tab).peekable();
        let fit = sizing.peek().is_none().then(|| t.fit().area());
        for area in sizing.map(View::pane_area).chain(fit) {
            lay_out(root, zoomed, area, placed);
            let shown = placed.panes.iter().filter(|(_, r)| !r.is_empty());
            out.extend(shown.map(|(p, r)| (*p, (r.h(), r.w()))));
        }
    }

    fn touch(&mut self) {
        self.views.values_mut().for_each(|view| view.dirty = true);
    }

    // ------------------------------------------------------------- closing

    /// Hangs up a pane's process; it is killed and reaped after `GRACE`.
    /// `closed`: fux closed it (`kill`, `kill-server`), not its program's
    /// end: the rest of its session goes too.
    fn end(&mut self, process: Process, closed: bool, now: Instant) {
        if let Process::Reading(mut child)
        | Process::HungUp(mut child)
        | Process::ReadFailed(mut child, _) = process
        {
            if closed {
                child.leader.closed();
            }
            child.leader.hang_up();
            let deadline = crate::after(now, GRACE);
            self.dying.push(Dying { child, deadline });
        }
    }

    /// Every program started and not yet reaped: the panes', those
    /// started for respawns under way, and the dying.
    pub fn children(&self) -> impl Iterator<Item = &Child> {
        let panes = (self.parts.panes().values()).filter_map(|p| p.process().child());
        let replacements = (self.respawning.values())
            .flat_map(|r| (r.process.child().into_iter()).chain(r.old.as_ref()));
        (panes.chain(replacements).chain(self.leftover.values()))
            .chain(self.dying.iter().map(|d| &d.child))
    }

    /// Removes a pane and its process. A tab its closing empties closes too,
    /// as does a workspace left with no tabs.
    pub fn close_pane(&mut self, id: PaneId, why: Option<String>, now: Instant) {
        // Who sees the pane is known before its tab can go: a tab its
        // closing empties takes its viewers elsewhere.
        let viewers = self.viewers(AnyRef::Pane(id));
        let mut notice = None;
        if let Some(mut pane) = self.parts.close_pane(id) {
            // Closed for its program's end (a reason given), or by fux.
            let closed = why.is_none();
            let failed = pane.has_terminal_errors();
            let status =
                why.map(|why| format!("{} {why}", fux_wire::words::labelled(id, &pane.label())));
            let told = status
                .into_iter()
                .chain(pane.take_terminal_errors())
                .collect::<Vec<_>>()
                .join("; ");
            if !told.is_empty() {
                if failed && self.parts.panes().is_empty() {
                    self.shut_down(told.clone());
                }
                notice = Some((told, failed));
            }
            self.end_pane(pane, closed, &viewers, now);
        }
        self.after_close(now);
        if let Some((told, failed)) = notice {
            for client in viewers {
                self.notify_to(client, told.clone(), failed);
            }
        }
    }

    /// Removes a tab and whatever panes are in it (`Parts::remove_tab`).
    fn remove_tab(&mut self, tab: TabId, now: Instant) {
        let viewers = self.viewers(AnyRef::Tab(tab));
        for gone in self.parts.remove_tab(tab) {
            self.end_pane(gone, true, &viewers, now);
        }
    }

    /// Ends the program of `pane`, closed (by fux if `closed`), and the one
    /// a respawn under way there started, telling `viewers` the respawn
    /// was abandoned.
    fn end_pane(&mut self, pane: Pane, closed: bool, viewers: &[ClientId], now: Instant) {
        let id = pane.id();
        self.end(pane.into_process(), closed, now);
        if let Some(child) = self.leftover.remove(&id) {
            self.end(Process::Reading(child), closed, now);
        }
        let Some(abandoned) = self.respawning.remove(&id) else {
            return;
        };
        self.end(abandoned.process, true, now);
        if let Some(child) = abandoned.old {
            let deadline = abandoned.grace.unwrap_or(now);
            self.dying.push(Dying { child, deadline });
        }
        let told = format!("{id} closed while respawning: the program started for it was ended");
        for client in viewers {
            self.notify_to(*client, told.clone(), true);
        }
    }

    /// The clients shown `of`, a tab or workspace, or a pane's tab.
    pub(crate) fn viewers(&self, of: AnyRef) -> Vec<ClientId> {
        let of = match of {
            AnyRef::Pane(p) => self.locate(p).map(|(_, tab)| AnyRef::Tab(tab)),
            AnyRef::Tab(_) | AnyRef::Workspace(_) => Some(of),
        };
        let at = |p: Place| match of {
            Some(AnyRef::Workspace(ws)) => self.parts.within(p.ws, ws),
            Some(tab) => tab == AnyRef::Tab(p.tab),
            None => false,
        };
        let shown = |c: &ClientId| self.place(*c).is_some_and(at);
        self.views.keys().copied().filter(shown).collect()
    }

    /// Asks the server to stop, saying why, as the journal's actor now.
    fn shut_down(&mut self, reason: String) {
        let actor = self.parts.journal().current_actor();
        self.outbox.push(Outgoing::Shutdown(reason, actor));
    }

    fn after_close(&mut self, now: Instant) {
        let shutting = (self.outbox.iter()).any(|o| matches!(o, Outgoing::Shutdown { .. }));
        if self.parts.panes().is_empty() && !shutting {
            self.shut_down("the last pane closed".into());
        }
        self.touch();
        self.settle(now);
    }

    /// A pane's program exited with `status`: a shell's pane closes, and a
    /// program's stays, the status kept, for its last screen to be read.
    /// What either wrote as it exited may still be on its terminal, or
    /// read and waiting for the pane's next turns (`Pane::has_unread`): it
    /// is told exited once all of it is on its screen, its bells, titles
    /// and notifications journaled, from the turn that reads the last of
    /// it (`Session::fire`); a shell's pane then closes.
    pub fn exited(&mut self, id: PaneId, ended: Ended, now: Instant) {
        let Some(generation) = self.parts.panes().get(&id).map(|p| p.respawns) else {
            return;
        };
        let previous = self
            .parts
            .actor(fux_wire::events::Actor::Program(id, generation));
        self.program_exited(id, ended, now);
        self.parts.actor(previous);
    }

    fn program_exited(&mut self, id: PaneId, ended: Ended, now: Instant) {
        let Some(pane) = self.parts.panes().get(&id) else {
            return;
        };
        // Told once.
        if pane.exiting.is_none() && pane.process().exit_status().is_some() {
            return;
        }
        let first = pane.exiting.is_none();
        // Once, at the first word of the exit, a turn's worth of its
        // terminal read, and the status kept till what is read is on its
        // screen; at each, a frame it holds read, one begun in output read
        // since too. Its terminal is read on after its exit is told, a turn
        // at a time, till it is empty or closed (D-90).
        if first {
            if let Some(mut pane) = self.parts.pane_mut(id) {
                pane.exiting(ended);
                pane.drain(true);
            }
            self.read_left(id, false, now);
        }
        self.read_with(id, |p| p.release_frame(now), now);
        let viewers = self.viewers(AnyRef::Pane(id));
        let Some(mut pane) = self.parts.pane_mut(id) else {
            return;
        };
        if pane.has_unread() {
            return;
        }
        let draining = pane.draining;
        match pane.runs {
            Runs::Shell(_) => {
                let left = pane.ended(ended).left.told();
                let why = answer::shell_status(ended).map_or_else(
                    || answer::told(ended),
                    |s| format!("exited with status {s}"),
                );
                self.close_pane(id, Some(format!("{why}{left}")), now);
            }
            Runs::Explicit(_) => {
                // Its leader, exited and unreaped, is reaped with the dying.
                let (end, gone) = pane.exit(ended);
                let errors: Vec<_> = if viewers.is_empty() {
                    Vec::new()
                } else {
                    pane.take_terminal_errors().collect()
                };
                if let Process::Reading(child)
                | Process::HungUp(child)
                | Process::ReadFailed(child, _) = gone
                {
                    match draining {
                        true => _ = self.leftover.insert(id, child),
                        false => self.dying.push(Dying {
                            child,
                            deadline: now,
                        }),
                    }
                }
                let status = format!("{id} {}{}", answer::told(end.ended), end.left.told());
                let left = end.left != pane::Left::default() || !errors.is_empty();
                let told = std::iter::once(status)
                    .chain(errors)
                    .collect::<Vec<_>>()
                    .join("; ");
                for client in viewers {
                    self.notify_to(client, told.clone(), left);
                }
                self.touch();
            }
        }
    }

    /// Reads a turn's worth of what pane `id`'s program left on its terminal
    /// as it ends, or is respawned over (`Replacement::old`); the rest on
    /// its next turns (`fire`), till its terminal is empty or closed. One
    /// that goes on writing there is read for as long as it does, a turn
    /// at a time, and holds up no other pane.
    /// `empty_ends`: whether a turn that finds it empty ends that; the
    /// read at an exit's first word does not, as the server's own read
    /// may have just emptied it.
    fn read_left(&mut self, id: PaneId, empty_ends: bool, now: Instant) {
        if !self.pane(id).is_ok_and(|p| p.draining) {
            return;
        }
        // A program respawned over that outlives its grace is killed; what
        // it left on its terminal is read on.
        if let Some(r) = self.respawning.get_mut(&id)
            && r.grace.is_some_and(|grace| grace <= now)
        {
            r.grace = None;
            if let Some(old) = &r.old {
                old.leader.kill();
            }
        }
        let old = self.respawning.get(&id).and_then(|r| r.old.as_ref());
        let old = old.or(self.leftover.get(&id));
        let own = old.is_none();
        let child = old.or_else(|| self.pane(id).ok().and_then(|p| p.process().child()));
        let (left, drain) = child.map_or((Vec::new(), Drain::Closed), |child| {
            left_turn(|bytes| fuxix::io::read(&child.master, bytes))
        });
        if !left.is_empty() {
            self.output(id, &left, now);
        }
        match drain {
            Drain::More => return,
            Drain::Empty if !empty_ends => return,
            Drain::Empty | Drain::Closed => {}
            Drain::Failed(error) => {
                if own && let Ok(mut pane) = self.pane_mut(id) {
                    pane.read_failed(error);
                }
            }
        }
        if let Ok(mut pane) = self.pane_mut(id) {
            pane.drain(false);
        }
        if let Some(r) = self.respawning.get_mut(&id)
            && let Some(child) = r.old.take()
        {
            let deadline = r.grace.unwrap_or(now);
            self.dying.push(Dying { child, deadline });
        }
        if let Some(child) = self.leftover.remove(&id) {
            let deadline = now;
            self.dying.push(Dying { child, deadline });
        }
    }

    /// Stops a failed reader and tells its viewers in this turn.
    pub(crate) fn read_failed(&mut self, id: PaneId, error: fuxix::Errno, now: Instant) {
        self.read_with(id, |pane| pane.read_failed(error), now);
    }

    /// Output from a pane's program.
    pub fn output(&mut self, id: PaneId, bytes: &[u8], now: Instant) {
        self.read_with(id, |pane| pane.output(bytes, now), now);
    }

    /// Everything the session waits for, each with when it is due: the
    /// server polls no longer than the soonest, and fires those due
    /// (`Session::fire`).
    pub fn timers(&self) -> impl Iterator<Item = (Instant, Timer)> + '_ {
        let panes = self.parts.panes().values().flat_map(|pane| {
            let respawn = self.respawning.get(&pane.id()).map(|r| r.since);
            // A respawn's turn reads a held frame: one parser call a round.
            let frame = (pane.frame_deadline().filter(|_| respawn.is_none()))
                .map(|at| (at, Timer::Frame(pane.id())));
            let unread = (pane.unread_due().or(respawn)).map(|at| (at, Timer::Unread(pane.id())));
            frame.into_iter().chain(unread)
        });
        let notices = (self.views.values())
            .filter_map(|v| Some((v.notice.as_ref()?.until?, Timer::Notice(v.id))));
        panes.chain(notices)
    }

    /// What `timer` waited for is done, now that it is due.
    pub fn fire(&mut self, timer: Timer, now: Instant) {
        match timer {
            // Only the notice that was due: a newer one keeps its own time.
            Timer::Notice(client) => {
                if let Some(view) = self.views.get_mut(&client)
                    && (view.notice.as_ref()).is_some_and(|n| n.until.is_some_and(|u| u <= now))
                {
                    view.notice = None;
                    view.dirty = true;
                }
            }
            Timer::Frame(id) => self.read_with(id, |p| p.release_frame(now), now),
            Timer::Unread(id) => {
                // A respawn under way reads a frame its old program holds.
                let frame = self.respawning.contains_key(&id)
                    && self.pane(id).is_ok_and(|p| p.frame_deadline().is_some());
                let left = self.pane(id).is_ok_and(|p| p.draining && !p.has_unread());
                match left {
                    true => self.read_left(id, true, now),
                    false => self.read_with(
                        id,
                        |p| match frame {
                            true => p.release_frame(now),
                            false => p.read_unread(now),
                        },
                        now,
                    ),
                }
                self.after_turn(id, now);
            }
        }
    }

    /// Reads into pane `id` with `read`, as every read of a pane's output
    /// does: its tab's host colours and palette given it first; after, its
    /// bell rung and notification told, its copies made in order, and the views showing it
    /// repainted and told what it refused (`tell_refused`).
    fn read_with(&mut self, id: PaneId, read: impl FnOnce(&mut PaneMut<'_>), now: Instant) {
        let place = self.locate(id);
        let viewers = self.viewers(AnyRef::Pane(id));
        let labelled = place.and_then(|(ws, tab)| {
            let tab = self.tab(tab)?;
            let pane = self.parts.panes().get(&id)?;
            if tab.name().is_some() || tab.root().first_pane() != id || pane.name().is_some() {
                return None;
            }
            let others: Vec<ClientId> = (self.viewers(AnyRef::Workspace(ws)).into_iter())
                .filter(|client| !viewers.contains(client))
                .collect();
            (!others.is_empty()).then(|| (pane.label().into_owned(), others))
        });
        let Some((mut pane, retold)) = self.hosted(id) else {
            return;
        };
        read(&mut pane);
        // Sized once for the new cell and what was read, a resize sized
        // so already, which the system does not signal again.
        if retold {
            pane.size_terminal();
        }
        // Program copies go to viewers of the tab, including zoomed views
        // and watchers, when this pane permits them. Invalid copies are
        // counted and reported.
        let (bell, notified, copies) = pane.take_read();
        let changed_label = labelled.filter(|(was, _)| was != &*pane.label());
        if bell || notified.is_some() {
            self.alert(id, bell, notified, now);
        }
        for text in copies {
            self.copied(&text, &viewers);
        }
        for view in self.views.values_mut() {
            if viewers.contains(&view.id)
                || changed_label
                    .as_ref()
                    .is_some_and(|(_, clients)| clients.contains(&view.id))
            {
                view.dirty = true;
            }
        }
        self.tell_refused(id, &viewers);
    }

    /// Tells the clients showing each pane what it refused and has not
    /// told (`tell_refused`); whether it told any. A pane no client shows
    /// tells it once one does.
    pub(crate) fn tell_lost(&mut self) -> bool {
        let untold: Vec<PaneId> = (self.parts.panes().iter())
            .filter(|(_, p)| p.loses() || p.has_terminal_errors())
            .map(|(id, _)| *id)
            .collect();
        let mut told = false;
        for id in untold {
            let viewers = self.viewers(AnyRef::Pane(id));
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
        let Some(mut pane) =
            pane.filter(|p| (p.loses() || p.has_terminal_errors()) && !viewers.is_empty())
        else {
            return false;
        };
        let mut notices = pane.take_untold().told(id, true);
        // An exit reports terminal failures with its final status, after
        // its remaining output and frame have been read.
        if pane.exiting.is_none() {
            notices.extend(pane.take_terminal_errors());
        }
        if notices.is_empty() {
            return false;
        }
        let told = notices.join("; ");
        for client in viewers {
            self.notify_to(*client, told.clone(), true);
        }
        true
    }

    /// `text`, copied, goes to each client in `to` whose terminal delivery
    /// is on (OSC 52), and is kept nowhere.
    pub(crate) fn copied(&mut self, text: &str, to: &[ClientId]) {
        let views = &self.views;
        let mut recipients = to
            .iter()
            .copied()
            .filter(|client| {
                views
                    .get(client)
                    .is_some_and(|v| v.presentation.terminal_clipboard)
            })
            .peekable();
        // Encoded once, for every recipient.
        if recipients.peek().is_some() {
            let encoded = fux_wire::json::base64(text.as_bytes());
            let osc = format!("\x1b]52;c;{encoded}\x07").into_bytes();
            self.outbox.extend(
                recipients
                    .map(|client| Outgoing::To(client, ToAttached::Paint(osc.clone().into()))),
            );
        }
    }

    /// Everything the server shuts down: every pane is hung up.
    pub fn shutdown(&mut self, now: Instant) {
        for pane in self.parts.close_all() {
            self.end_pane(pane, true, &[], now);
        }
    }

    // ------------------------------------------------------------ commands

    /// Begins a request from a client: one that waits gives its wait,
    /// which the server asks for its answer until it has one. Event reads
    /// use the same bounded wait; commands run at once.
    pub fn begin(&mut self, request: Request, origin: &Origin, now: Instant) -> Begun {
        self.begin_request(request, origin, now)
            .unwrap_or_else(|error| Begun::Done(failure(error, origin, self)))
    }

    /// `begin`'s work: what fails before a request runs is its error.
    fn begin_request(
        &mut self,
        request: Request,
        origin: &Origin,
        now: Instant,
    ) -> Result<Begun, Error> {
        let wait = match request {
            Request::Do(command) => {
                let reply = self.run_command(&command, origin, now);
                // A respawn is answered once its pane is at the version it
                // answers: at once, unless what the old program left is
                // still being read.
                let waiting = match (&command, &reply.response) {
                    (
                        Command::Respawn { .. },
                        Response::Answered(PaneResults::One(PaneAnswer::Acted(started))),
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
            Request::NextEvents {
                scope,
                from,
                timeout,
            } => {
                if !self.ids.gave_scope(scope) {
                    return Err(fail(
                        Raise::BadValue,
                        "event scope was never issued by this server run",
                    ));
                }
                let reader =
                    crate::journal::Reader::new(scope, self.parts.journal(), self.run, from)
                        .map_err(|why| fail(Raise::BadValue, why))?;
                let read = crate::journal::Read::new(reader, crate::after(now, timeout.duration()));
                return Ok(self.answered(Waiting::Events(read), now));
            }
        };
        let watches = match wait.since {
            command::Since::Now(AnyRef::Pane(id)) => Watches::Pane((id, self.pane(id)?.counts())),
            command::Since::Now(target) => Watches::Panes(
                (self.panes_of(Some(target))?.into_iter())
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
        let limit = wait.timeout.map(command::Limit::duration);
        Ok(self.answered(Waiting::new(watches, self.run, now, limit), now))
    }

    /// `waiting`'s answer, if it has one already at `now`; else the wait.
    fn answered(&self, mut waiting: Waiting, now: Instant) -> Begun {
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

    /// A value from another run must be refused before anything is acted on.
    pub fn check_run(&self, run: Option<command::Run>) -> Result<(), Error> {
        if run.is_some_and(|run| run != self.run) {
            return Err(fail(Raise::BadValue, "handle is from another server run"));
        }
        Ok(())
    }

    /// Runs a command, parsed already: from a line, a binding or a chooser.
    pub fn run_command(&mut self, command: &Command, origin: &Origin, now: Instant) -> Reply {
        let actor = self.request_actor(origin);
        let previous = self.parts.actor(actor);
        let outcome = self
            .execute(command, origin, now)
            .unwrap_or_else(|error| failure(error, origin, self));
        if !command.shows_nothing_new() {
            self.changes = self.changes.wrapping_add(1);
            self.touch();
        }
        self.settle(now);
        self.parts.actor(previous);
        outcome
    }

    pub(crate) fn request_actor(&self, origin: &Origin) -> fux_wire::events::Actor {
        use fux_wire::events::Actor;
        match origin {
            Origin::Client(client) => Actor::Client(*client),
            Origin::ClientContext { actor, .. } => *actor,
            Origin::Cli(caller) => match (caller.within, caller.gave) {
                (Within::This, Some((pane, generation)))
                    if self
                        .parts
                        .panes()
                        .get(&pane)
                        .is_some_and(|p| p.respawns == generation) =>
                {
                    Actor::Pane(pane, generation)
                }
                _ => Actor::Outside,
            },
        }
    }

    pub(crate) fn view_event(&mut self, client: ClientId, change: crate::journal::ViewChange) {
        let at = self.place(client).map(|p| ((p.ws, p.tab), p.pane));
        self.parts
            .journal_mut()
            .push(crate::journal::Event::View { client, at, change });
    }

    /// How many commands have run that may change what clients show.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    fn execute(
        &mut self,
        command: &Command,
        origin: &Origin,
        now: Instant,
    ) -> Result<Reply, Error> {
        let here = self.here(origin)?;
        match command {
            Command::Inspect { scope, projection } => {
                self.inspect(*scope, projection.clone()).map(Into::into)
            }
            Command::Arrange {
                target,
                revision,
                nodes,
            } => {
                let tab = self.tab(*target).ok_or(Error::NoTab(*target))?;
                let now = tab.revision(self.run);
                if now != Some(*revision) {
                    let behind = revision.tab == *target
                        && revision.run == self.run
                        && now.is_none_or(|n| revision.serial < n.serial);
                    let code = if behind {
                        Raise::Stale
                    } else {
                        Raise::BadValue
                    };
                    let now = now.map_or_else(|| "exhausted".into(), |r| r.to_string());
                    return Err(fail(code, format!("{target} is at {now}, not {revision}")));
                }
                let root = layout::from_nodes(nodes)?;
                if !root.same_panes(tab.root()) {
                    return Err(fail(
                        Raise::BadValue,
                        "layout must contain every current tab pane exactly once",
                    ));
                }
                self.room(*target, &root, None)?;
                self.parts
                    .arrange(*target, root)
                    .ok_or(Error::NoTab(*target))?;
                Ok(Reply::default())
            }
            Command::KillServer => self.kill_server(origin),
            Command::NewTab {
                target,
                name,
                size,
                cwd,
                start,
                options,
            } => {
                let (cwd, fit, sized) = self.opening((cwd.as_deref(), *size), here)?;
                let new = NewPane::new(start, &cwd, *options, sized);
                // A workspace named that is not there is made, its first tab
                // this one.
                let made = self.create_tab(target, here, name.clone(), fit, new, now)?;
                self.follow(here, AnyRef::Tab(made.tab));
                Ok(acted(self.pane(made.pane)?.version(self.run)))
            }
            &Command::Respawn {
                target,
                kill,
                ref argv,
            } => self.respawn(target, kill, argv.as_ref(), now),
            Command::Notify { target, text } => {
                let to: Vec<ClientId> = match target {
                    None => self.views.keys().copied().collect(),
                    Some(target) => self.viewers(self.there(*target)?),
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
                self.resize(client, size.rows(), size.cols(), now)?;
                Ok(Reply::default())
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
                ref start,
                ref cwd,
                shares,
                options,
            } => self.split(
                target,
                (axis, shares),
                (start, options),
                cwd.as_deref(),
                origin,
                now,
            ),

            &Command::Signal { target, signal } => {
                let group = self.signalled(target)?;
                let version = self.pane(target)?.version(self.run);
                fuxix::process::kill_group(group, signal)
                    .map_err(|e| fail(Raise::Signal, format!("sending the signal: {e}")))?;
                Ok(acted(version))
            }
            &Command::Kill { target } => self.kill(target, now).map(|()| Reply::default()),
            Command::Rename { target, name } => {
                let target = self.there(*target)?;
                self.rename(target, name.clone()).map(|()| Reply::default())
            }
            &Command::Move { target, ref to } => match target {
                AnyRef::Pane(pane) => self.move_pane(pane, to, here),
                AnyRef::Workspace(ws) => self.move_workspace(ws, to, here),
                AnyRef::Tab(_) => Err(fail(Raise::BadValue, "move takes a pane or workspace")),
            }
            .map(|()| Reply::default()),
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
            &Command::Zoom { target } => {
                self.parts.zoom(target).ok_or(Error::NoPane(target))?;
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
                let captured = crate::capture::pane(pane, self.run, history, view)?;
                Ok(Body::Capture(Box::new(captured)).into())
            }
            &Command::Reorder { ref target, toward } => {
                let target = self.there(*target)?;
                self.reorder(target, toward).map(|()| Reply::default())
            }
            &Command::HistoryBudget { target, bytes } => {
                self.pane_mut(target)?.set_history_bytes(bytes);
                Ok(Reply::default())
            }
            Command::Resolve { text, kind } => Ok(found(self.resolve(text, *kind, here)?)),
            &Command::Members { target, members } => {
                let ids = match members {
                    Members::Panes => {
                        (self.panes_of(target)?.into_iter().map(AnyRef::Pane)).collect()
                    }
                    Members::Tabs => (self.tabs_of(target)?.into_iter().map(AnyRef::Tab)).collect(),
                };
                Ok(Body::Handles(ids).into())
            }
            &Command::Client { client, ref action } => self.client(client, action, here, now),
        }
    }

    /// What `new-tab` does before its pane starts: the
    /// directory it starts in found, and its tab's fit and the size settling
    /// gives its pane: with a client asking, which then shows it, on its
    /// screen, in the cell its terminal said; else its fit's, in no cell, as
    /// no terminal draws it.
    fn opening(
        &mut self,
        (dir, size): (Option<&Path>, command::Size),
        here: Here,
    ) -> Result<(PathBuf, Fit, Starts), Error> {
        let cwd = self.start_dir(here, dir, None)?;
        let fit = self.new_fit(size, here)?;
        let follower = here.client().and_then(|c| self.views.get(&c));
        let screen = (follower.filter(|view| view.sizes())).and_then(Self::screen_size);
        let cell = follower.and_then(|view| view.terminal.cell);
        Ok((cwd, fit, (screen.unwrap_or(fit.size()), cell)))
    }

    /// `kill-server`: from the CLI, refused from one of the server's own
    /// panes or with people attached.
    fn kill_server(&mut self, origin: &Origin) -> Result<Reply, Error> {
        if let Origin::Cli(caller) = origin {
            let pane = caller.pane().filter(|p| self.parts.panes().contains_key(p));
            if pane.is_some() || caller.within == Within::This {
                let pane = pane.map_or_else(String::new, |p| format!(" {p},"));
                let message = format!(
                    "this runs in{pane} a pane of the server it would stop, with everything \
                     else in it; `kill` on status's server_pid stops it anyway"
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
                let message = format!(
                    "{who} attached to this server; `kill` on status's server_pid stops it anyway"
                );
                return Err(fail(Raise::PeopleAttached, message));
            }
        }
        let reason = match origin {
            Origin::Client(_) | Origin::ClientContext { .. } => {
                "stopped by kill-server, in a client context"
            }
            Origin::Cli(_) => "stopped by fux kill-server",
        };
        self.shut_down(reason.into());
        Ok(Reply::default())
    }

    /// `resize` of `target`'s tabs: laid out on `size`, or with none on
    /// their clients' screens. A size that would hide one of a tab's
    /// panes, as a split that would is, is refused.
    fn resize_tabs(&mut self, target: AnyRef, size: command::Size) -> Result<(), Error> {
        let size = size.map(fux_wire::command::Screen::size);
        // A pane's own tab.
        let target = match self.there(target)? {
            AnyRef::Pane(p) => AnyRef::Tab(self.locate(p).ok_or(Error::NoPane(p))?.1),
            target @ (AnyRef::Tab(_) | AnyRef::Workspace(_)) => target,
        };
        let tabs = self.tabs_of(Some(target))?;
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
            if let Some(chunks) = p
                .screen()
                .encode_mouse(event, p.options().alternate_scroll_lines)
            {
                report.extend(chunks.flatten());
                continue;
            }
            if release && x10 {
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
        p.push_mouse(report, held);
        Ok(acted(version))
    }

    /// `split`: a new pane beside `target`, along `axis`, the two taking
    /// `shares` if given, running `start` in `dir`, with the id `id` asked
    /// for.
    fn split(
        &mut self,
        target: PaneId,
        (axis, shares): (Axis, Option<(NonZeroU32, NonZeroU32)>),
        (start, options): (&Start, command::PaneOptions),
        dir: Option<&Path>,
        origin: &Origin,
        now: Instant,
    ) -> Result<Reply, Error> {
        let here = self.here(origin)?;
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
        let new = NewPane::new(start, &cwd, options, (size, self.graphics(target).cell()));
        let pane = self.new_pane(&mut ids, new, now)?;
        let new = pane.id();
        if let Err(pane) = self.parts.split(pane, split) {
            self.end(pane.into_process(), true, now);
            return Err(Error::NoPane(target));
        }
        self.ids = ids;
        for client in followers {
            self.parts.show(client, AnyRef::Pane(new));
        }
        let version = self.pane(new)?.version(self.run);
        Ok(acted(version))
    }

    /// `kill`: a pane closed, or every pane in `scope` (`tabs_of`), tab by
    /// tab.
    fn kill(&mut self, scope: Option<AnyRef>, now: Instant) -> Result<(), Error> {
        if let Some(AnyRef::Pane(pane)) = scope {
            self.pane(pane)?;
            self.close_pane(pane, None, now);
            return Ok(());
        }
        for tab in self.tabs_of(scope)? {
            self.remove_tab(tab, now);
        }
        self.after_close(now);
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
        now: Instant,
    ) -> Result<Reply, Error> {
        let client = match (client, here) {
            (Some(c), _) => self.views.get(&c).ok_or(Error::NoClient(c))?.id,
            (None, Here::Client(client, _)) => client,
            (None, Here::Cli(..)) => {
                return Err(fail(
                    Raise::NoTarget,
                    "this acts on a client's screen: name one (`str c1 client`; `fux ls` lists them)",
                ));
            }
        };
        let view = || self.views.get(&client).ok_or(Error::NoClient(client));
        let to = match *action {
            ClientAction::Open(ref screen) => {
                self.outbox
                    .push(Outgoing::To(client, ToAttached::Open(screen.clone())));
                return Ok(Reply::default());
            }
            // Taken as typed at its terminal: its bindings see them. No
            // pane answers: they go to the pane the client chooses.
            ClientAction::Keys(ref inputs) => {
                let keys = ToAttached::Keys(inputs.clone());
                self.outbox.push(Outgoing::To(client, keys));
                return Ok(PaneResults::Many(Vec::new()).into());
            }
            ClientAction::Detach => {
                self.detach(client, now);
                let exit = ToAttached::Exit("detached");
                self.outbox.push(Outgoing::To(client, exit));
                return Ok(Reply::default());
            }
            ClientAction::Capture => {
                let terminal = view()?;
                let mut grid = crate::render::Grid::default();
                let placement = &mut Placement::default();
                crate::render::compose_view(self, terminal, &mut grid, placement)
                    .map_err(|e| fail(Raise::Memory, format!("capturing {client}: {e}")))?;
                let captured = crate::capture::client(client, &grid);
                return Ok(Body::Capture(Box::new(captured)).into());
            }
            ClientAction::Show(target) => self.there(target)?,
            ClientAction::Step { from, way } => self.step(view()?, self.there(from)?, way)?,
        };
        self.parts.show(client, to);
        Ok(Reply::default())
    }

    fn rename(&mut self, target: AnyRef, name: String) -> Result<(), Error> {
        self.check_free(&name, Some(target))?;
        match target {
            AnyRef::Pane(p) => self.pane_mut(p)?.rename(name),
            AnyRef::Tab(t) => {
                self.parts.give_tab_name(t, name).ok_or(Error::NoTab(t))?;
            }
            AnyRef::Workspace(id) => {
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
        let dest = match to {
            &MoveTo::Beside(direction) => {
                let destination = self.neighbor(pane, direction, here)?;
                let root = self.root(from).ok_or(Error::NoTab(from))?;
                let root = layout::move_beside(root, pane, destination, direction)
                    .ok_or(Error::NoPane(pane))?;
                self.room(from, &root, None)?;
                self.parts.reshape(pane, |old| *old = root);
                return Ok(());
            }
            &MoveTo::Tab(tab) => Destination::Tab(self.tab(tab).ok_or(Error::NoTab(tab))?.id()),
            MoveTo::Workspace(r) => {
                let ws = self.resolve_ws(r)?;
                let first = self.parts.tabs_in(Some(ws)).first().map(|(_, t)| t.id());
                Destination::Tab(first.ok_or(Error::NoWorkspace(ws))?)
            }
            // A tab made for it holds it alone, which shows on every size
            // it is laid out on (each has a cell: its fit's, and a screen
            // that sizes panes, `View::sizes`): nothing after refuses the
            // move.
            MoveTo::NewTab => Destination::NewTab(from_ws, ids.tab()?, fit),
            MoveTo::NewWorkspace => {
                let (id, tab) = (ids.workspace()?, ids.tab()?);
                Destination::NewWorkspace((id, None), (tab, None, fit))
            }
            MoveTo::Top => {
                let why = "all is where a workspace moves; a pane moves to a tab or workspace";
                return Err(fail(Raise::BadValue, why));
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
        self.follow(here, AnyRef::Pane(pane));
        Ok(())
    }

    /// `move` of workspace `ws`: into the workspace `to` names, after what
    /// it holds, or with `all` to the top; never into itself or one in it.
    fn move_workspace(&mut self, ws: WsId, to: &MoveTo, here: Here) -> Result<(), Error> {
        let from = self.parts.parent(ws).ok_or(Error::NoWorkspace(ws))?;
        let into = match to {
            MoveTo::Workspace(r) => Some(self.resolve_ws(r)?),
            MoveTo::Top => None,
            MoveTo::Tab(_) | MoveTo::NewTab | MoveTo::NewWorkspace | MoveTo::Beside(_) => {
                let why = "a workspace moves into a workspace, or with all to the top";
                return Err(fail(Raise::BadValue, why));
            }
        };
        if let Some(into) = into.filter(|into| self.parts.within(*into, ws)) {
            let (ws, into) = (AnyRef::Workspace(ws), AnyRef::Workspace(into));
            let why = match ws == into {
                true => format!("{ws} cannot move into itself"),
                false => format!("{ws} cannot move into {into}, which is in it"),
            };
            return Err(fail(Raise::BadValue, why));
        }
        if from != into {
            (self.parts.move_workspace(ws, into)).ok_or(Error::NoWorkspace(ws))?;
            self.follow(here, AnyRef::Workspace(ws));
        }
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
            AnyRef::Tab(t) => self.parts.reorder(target, toward).ok_or(Error::NoTab(t))?,
            AnyRef::Workspace(w) => {
                (self.parts.reorder(target, toward)).ok_or(Error::NoWorkspace(w))?
            }
        };
        moved.then_some(()).ok_or(Error::AtEnd(target.kind()))
    }

    /// The sibling of `from` that `way` says client `view` is shown: the
    /// next or previous among what holds it, going round, the one it was
    /// shown there before, or a pane's neighbour on its screen.
    fn step(&self, view: &View, from: AnyRef, way: Way) -> Result<AnyRef, Error> {
        let kind = from.kind();
        let within = match from {
            AnyRef::Pane(p) => Some(AnyRef::Tab(self.locate(p).ok_or(Error::NoPane(p))?.1)),
            AnyRef::Tab(t) => Some(AnyRef::Workspace(self.tab_workspace(t)?)),
            AnyRef::Workspace(w) => {
                (self.parts.parent(w).ok_or(Error::NoWorkspace(w))?).map(AnyRef::Workspace)
            }
        };
        let siblings: Vec<AnyRef> = match (from, within) {
            (AnyRef::Pane(_), _) => {
                (self.panes_of(within)?.into_iter().map(AnyRef::Pane)).collect()
            }
            (_, Some(AnyRef::Workspace(w))) => (self.workspace(w).into_iter())
                .flat_map(|w| w.stack().children().iter().map(crate::workspace::Child::id))
                .collect(),
            (_, _) => (self.parts.workspaces().iter())
                .map(|w| AnyRef::Workspace(w.id()))
                .collect(),
        };
        match (way, from) {
            (Way::Step(toward), _) => {
                let index = siblings.iter().position(|s| *s == from);
                let next = round(&siblings, index.unwrap_or(0), toward);
                next.copied().ok_or(Error::OnlyOne(kind))
            }
            (Way::Last, _) => (self.parts.seats(view.id).into_iter())
                .find_map(|seat| seat.last.filter(|_| seat.within == within))
                .ok_or_else(|| {
                    let code = match kind {
                        Kind::Pane => Raise::NoPane,
                        Kind::Tab => Raise::NoTab,
                        Kind::Workspace => Raise::NoWorkspace,
                    };
                    fail(code, format!("no previously shown {}", kind.name()))
                }),
            (Way::Toward(direction), AnyRef::Pane(from)) => {
                let tab = self.locate(from).and_then(|(_, tab)| self.root(tab));
                let placement = tab.map(|root| layout::place(root, view.pane_area()));
                let found = placement.and_then(|p| layout::neighbor(&p, from, direction));
                found
                    .map(AnyRef::Pane)
                    .ok_or(Error::NoNeighbor { from, direction })
            }
            (Way::Toward(direction), AnyRef::Tab(_) | AnyRef::Workspace(_)) => {
                let (way, kind) = (direction.name(), kind.name());
                let why = format!("{way} steps among a tab's panes: {from} is a {kind}");
                Err(fail(Raise::BadValue, why))
            }
        }
    }

    // ------------------------------------------------------------------ ls

    /// A pane's state, as `ls json` has it.
    fn pane_json<'a>(&'a self, p: &'a Pane) -> Result<Fields, Error> {
        let pid = |c: &Child| Json::Int(i64::from(c.leader.pid().as_raw()));
        let values = [
            ("id", Json::str(format!("{}", p.id()))),
            ("name", p.name().map_or(Json::Null, Json::str)),
            ("label", Json::str(format!("{}", p.display_label()))),
            ("title", Json::str(p.title().as_str())),
            ("rows", Json::int(p.size().rows())),
            ("cols", Json::int(p.size().cols())),
            ("pid", p.process().child().map_or(Json::Null, pid)),
            ("version", Json::str(format!("{}", p.version(self.run)))),
        ];
        let mut fields = Vec::with_capacity(values.len());
        fields.extend(values);
        fields.extend(crate::capture::state(p)?);
        Ok(fields)
    }

    fn field(&self, target: AnyRef, field: &str) -> Result<Body, Error> {
        if field == "id" {
            return Ok(Body::Found(target));
        }
        if field == "panes" && !matches!(target, AnyRef::Pane(_)) {
            let panes = self.panes_of(Some(target))?;
            return Ok(Body::Handles(panes.into_iter().map(AnyRef::Pane).collect()));
        }
        let no_field = || fail(Raise::NoField, format!("{target} has no field {field:?}"));
        let value = match target {
            AnyRef::Pane(id) => {
                let pane = self.pane(id)?;
                match field {
                    "name" => pane.name().map_or(Json::Null, Json::str),
                    "label" => Json::str(format!("{}", pane.display_label())),
                    "rows" => Json::int(pane.size().rows()),
                    "cols" => Json::int(pane.size().cols()),
                    "pid" => pane
                        .process()
                        .child()
                        .map_or(Json::Null, |c| Json::int(c.leader.pid().as_raw())),
                    "version" => Json::str(format!("{}", pane.version(self.run))),
                    _ => self
                        .pane_json(pane)?
                        .into_iter()
                        .find(|(k, _)| *k == field)
                        .map(|(_, v)| v)
                        .ok_or_else(no_field)?,
                }
            }
            AnyRef::Tab(id) => {
                let tab = self.tab(id).ok_or(Error::NoTab(id))?;
                match field {
                    "host" => self
                        .host(Some(id))
                        .map_or(Json::Null, |c| Json::str(c.to_string())),
                    "layout_revision" => Json::str(format!(
                        "{}",
                        tab.revision(self.run)
                            .ok_or_else(|| fail(Raise::Range, "layout revision exhausted"))?
                    )),
                    "name" => tab.name().map_or(Json::Null, Json::str),
                    "label" => Json::str(format!("{}", tab.display_label(self.parts.panes()))),
                    _ => return Err(no_field()),
                }
            }
            AnyRef::Workspace(id) => {
                let ws = self.workspace(id).ok_or(Error::NoWorkspace(id))?;
                match field {
                    "name" => ws.name().map_or(Json::Null, Json::str),
                    "label" => Json::str(format!("{}", ws.display_label())),
                    "children" => {
                        let children = ws.stack().children().iter();
                        let ids = children.map(crate::workspace::Child::id).collect();
                        return Ok(Body::Handles(ids));
                    }
                    "parent" => match self.parts.parent(id).flatten() {
                        Some(parent) => return Ok(Body::Found(AnyRef::Workspace(parent))),
                        None => Json::Null,
                    },
                    _ => return Err(no_field()),
                }
            }
        };
        Ok(Body::Field(value))
    }
}

/// The answer of a lookup that found `found`: its id.
fn found(found: AnyRef) -> Reply {
    Body::Found(found).into()
}

/// How a turn of reading what a program left on its terminal ended.
#[derive(Debug, PartialEq, Eq)]
enum Drain {
    /// Some read: there may be more, which the next turn reads. A read
    /// that finds nothing just after one that found some is no empty
    /// terminal: a terminal is handed on a piece at a time.
    More,
    /// Nothing to read as the turn began.
    Empty,
    /// The terminal is closed.
    Closed,
    Failed(fuxix::Errno),
}

/// A turn's worth of what a terminal holds (`pane::SLICE`), read with
/// `read`, and whether it may hold more.
fn left_turn(mut read: impl FnMut(&mut [u8]) -> Result<usize, fuxix::Errno>) -> (Vec<u8>, Drain) {
    let mut left = Vec::new();
    let mut buffer = [0u8; pane::SLICE];
    loop {
        let room = pane::SLICE.saturating_sub(left.len());
        let Some(bytes) = buffer.get_mut(..room).filter(|b| !b.is_empty()) else {
            return (left, Drain::More);
        };
        match read(bytes) {
            Ok(n) if n > 0 => left.extend_from_slice(bytes.get(..n).unwrap_or_default()),
            Err(fuxix::Errno::INTR) => {}
            Err(fuxix::Errno::AGAIN) if !left.is_empty() => return (left, Drain::More),
            Err(fuxix::Errno::AGAIN) => return (left, Drain::Empty),
            Ok(_) | Err(fuxix::Errno::IO) => return (left, Drain::Closed),
            Err(error) => return (left, Drain::Failed(error)),
        }
    }
}

fn check_name(name: &str) -> Result<(), Error> {
    if name.trim().is_empty() {
        return Err(fail(Raise::BadName, "a name cannot be empty or blank"));
    }
    // An id's sigils, and what a destination reads as the request for a
    // fresh workspace.
    if name.starts_with(['%', '@', '+']) || name == "new-workspace" {
        return Err(Error::NameReadAsTarget(name.to_owned()));
    }
    // Control characters, and the invisible ones that change how text
    // around them shows (zero-width, direction marks and overrides: UAX #9
    // 2.6 names U+061C beside U+200E and U+200F).
    let invisible = |c: char| matches!(c, '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
    if name.chars().any(|c| c.is_control() || invisible(c)) {
        let message = "a name cannot contain control or invisible characters";
        return Err(fail(Raise::BadName, message));
    }
    Ok(())
}

/// Where each pane of a tab laid out as `root` is in `area`, into `out`:
/// the `zoomed` pane alone, filling it, if one is and it has room.
fn lay_out(root: &Tree, zoomed: Option<PaneId>, area: Rect, out: &mut Placement) {
    match zoomed.filter(|_| !area.is_empty()) {
        Some(pane) => {
            out.clear();
            out.panes.push((pane, area));
        }
        None => layout::place_into(root, area, out),
    }
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
/// and the fuzz targets (`fux::script::testing::InProcess`).
#[cfg(any(test, feature = "in-process"))]
impl Session {
    /// A program's request, answered in-process: begun at once, and a wait
    /// answered at its limit, there being no programs to wait on, once what
    /// is due to be read is. Finite event reads use the same deadlines.
    pub fn answer(&mut self, asked: &fux_wire::protocol::Asked) -> Reply {
        let origin = match asked.context.client {
            Some(client) if self.views.contains_key(&client) => Origin::Client(client),
            Some(_) | None => Origin::Cli(Caller {
                cwd: Some(asked.context.cwd.clone()),
                ..Caller::default()
            }),
        };
        let begun = match self.check_run(asked.context.of) {
            Ok(()) => self.begin(asked.request.clone(), &origin, Instant::now()),
            Err(error) => Begun::Done(Reply::failed(error.code(), error.to_string())),
        };
        let outcome = match begun {
            Begun::Done(outcome) => outcome,
            // As the server answers it: the panes' turns and frames come
            // as they fall due, the soonest first, each answered at its
            // time, until it has an answer; at its deadline, once none
            // falls due before it.
            Begun::Waiting(mut waiting) => loop {
                let soonest = self.timers().min_by_key(|(at, _)| *at);
                let due = soonest.map(|(at, _)| at).into_iter().chain(waiting.due());
                let now = due.min().unwrap_or_else(Instant::now);
                if let Some(answer) = waiting.answer(self, now) {
                    break answer;
                }
                let Some((_, timer)) = soonest else {
                    let why = "a wait nothing due answers is answered by a real server only";
                    break Reply::failed(fux_wire::code::Code::NotHere, why);
                };
                self.fire(timer, now);
            },
        };
        Reply {
            run: Some(self.run),
            ..outcome
        }
    }

    /// A client's presentation, as its program publishes it: client
    /// `client`'s, if it is attached and shows another.
    pub fn present(
        &mut self,
        client: Option<ClientId>,
        presentation: fux_wire::presentation::Presentation,
    ) {
        if let Some(client) = client
            && self
                .views
                .get(&client)
                .is_some_and(|view| view.presentation != presentation)
        {
            self.acts(
                client,
                vec![fux_wire::act::Act::Presentation(presentation)],
                Instant::now(),
            );
        }
    }
}

/// What the other modules' tests share: a session, a client attached,
/// and a command line run in it.
#[cfg(test)]
pub(crate) mod testing {
    pub(crate) type TestResult = Result<(), Box<dyn std::error::Error>>;

    use super::{Reply, Session};

    /// A session, as what a program's requests go to (`Session::answer`),
    /// and its keymap, as its client's.
    pub(crate) struct InProcess<'s>(pub(crate) &'s mut Session);

    impl fux::script::Asks for InProcess<'_> {
        fn published(
            &mut self,
            state: &fux::client_state::ClientState,
            context: &fux_wire::protocol::Context,
        ) {
            self.0.test_keymap = state.draft();
            self.0
                .present(context.client, state.preferences.presentation());
        }
        fn ask(&mut self, asked: &fux_wire::protocol::Asked) -> Result<Reply, fux::script::Failed> {
            Ok(self.0.answer(asked))
        }
    }
    use fux_wire::command::{self, Command, Request};
    use fux_wire::id::ClientId;

    impl Session {
        /// Exercise the library against exactly one held recipe, without mutating the session.
        pub(super) fn dump_recipe(
            &self,
            only: Option<command::AnyRef>,
        ) -> Result<String, fux::script::Failed> {
            struct Once(Option<Reply>);
            impl fux::script::Asks for Once {
                fn ask(
                    &mut self,
                    asked: &fux_wire::protocol::Asked,
                ) -> Result<Reply, fux::script::Failed> {
                    assert!(matches!(
                        asked.request,
                        Request::Do(Command::Inspect {
                            projection: fux_wire::inspect::Projection::Recipe,
                            ..
                        })
                    ));
                    self.0.take().ok_or_else(|| {
                        fux::script::Failed::new(
                            fux_wire::code::Code::Usage,
                            "dump made more than one request",
                        )
                    })
                }
            }
            let scope = only.map_or(
                fux_wire::inspect::Scope::All,
                fux_wire::inspect::Scope::Target,
            );
            let body = self
                .inspect(scope, fux_wire::inspect::Projection::Recipe)
                .map_err(|e| fux::script::Failed::new(e.code(), e.to_string()))?;
            let tokens =
                fuxth::token::source("dump", "all dump").map_err(fux::script::Failed::from)?;
            let mut machine = fux::script::compile(&tokens)?;
            let mut out = Vec::new();
            let mut host =
                fux::script::Host::new(fux_wire::protocol::Context::default(), None, &mut out);
            let reply = Reply {
                run: Some(self.run),
                ..body.into()
            };
            let stack = fux::script::run(&mut machine, &mut host, &mut Once(Some(reply)), None);
            assert!(
                out.is_empty(),
                "dump emitted before returning its complete source"
            );
            let stack = stack?;
            match stack.as_slice() {
                [fuxth::Value::Str(text)] => Ok(text.to_string()),
                _ => Err(fux::script::Failed::new(
                    fux_wire::code::Code::Usage,
                    "dump did not return one string",
                )),
            }
        }
    }
    use std::time::Instant;

    impl Session {
        pub(crate) fn out(&mut self, id: fux_wire::id::PaneId, bytes: &[u8]) {
            self.output(id, bytes, Instant::now())
        }
        pub(crate) fn apply(&mut self, client: ClientId, acts: Vec<fux_wire::act::Act>) {
            self.acts(client, acts, Instant::now())
        }
        pub(crate) fn size_client(
            &mut self,
            client: ClientId,
            rows: u16,
            cols: u16,
        ) -> Result<bool, super::Error> {
            self.resize(client, rows, cols, Instant::now())
        }
        pub(crate) fn end_program(
            &mut self,
            id: fux_wire::id::PaneId,
            status: fux_wire::answer::Ended,
        ) {
            self.exited(id, status, Instant::now())
        }
        pub(crate) fn settled(&mut self) {
            self.settle(Instant::now())
        }
        pub(crate) fn detach_client(&mut self, id: ClientId) {
            self.detach(id, Instant::now())
        }
        pub(crate) fn attach_client(
            &mut self,
            attach: fux_wire::protocol::Attach,
        ) -> Result<ClientId, super::Error> {
            self.attach_as(attach, Instant::now())
        }
        pub(crate) fn begin_now(
            &mut self,
            request: Request,
            origin: &super::Origin,
        ) -> super::Begun {
            self.begin(request, origin, Instant::now())
        }
        pub(crate) fn command_now(&mut self, command: &Command, origin: &super::Origin) -> Reply {
            self.run_command(command, origin, Instant::now())
        }
        pub(crate) fn start_now(
            &mut self,
            size: fux_vt::Size,
            initial: &command::InitialWorkspace,
        ) -> Result<(), super::Error> {
            self.start(size, initial, Instant::now())
        }
        pub(crate) fn fire_now(&mut self, timer: super::Timer) {
            self.fire(timer, Instant::now())
        }
    }

    /// A client's bar on a row of its own, the panes below it.
    pub(crate) const BAR_ROW: &str =
        "str bar list str bottom list str workspace str tabs end list str pane end false end set";

    /// A session with `/bin/sh`, started at 80 by 24.
    pub(crate) fn started() -> Result<Session, String> {
        // One run for every session of the tests: two that did the same
        // list the same, their cursors too.
        static RUN: std::sync::OnceLock<Option<fux_wire::command::Run>> =
            std::sync::OnceLock::new();
        let run = (*RUN.get_or_init(fux_wire::command::Run::now)).ok_or("no clock")?;
        let journal = fux::script::default_state()
            .map_err(|e| e.to_string())?
            .preferences
            .journal_entries;
        let mut session = Session::new("/nonexistent/fux.sock".into(), false, run, journal);
        let size = fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?;
        let initial = fux_wire::command::InitialWorkspace {
            options: fux::script::default_state()
                .map_err(|e| e.to_string())?
                .preferences
                .pane_options(),
            name: None,
            start: fux_wire::command::Start::Shell(command::Program::new(
                "/bin/sh".into(),
                Vec::new(),
            )?),
            cwd: std::env::current_dir().map_err(|e| e.to_string())?,
        };
        session
            .start_now(size, &initial)
            .map_err(|e| e.to_string())?;
        session.test_keymap = fux::script::default_state()
            .map_err(|e| e.to_string())?
            .draft();
        Ok(session)
    }

    /// A session with a client of `rows`
    /// by `cols` attached.
    pub(crate) fn attached(rows: u16, cols: u16) -> Result<(Session, ClientId), String> {
        let mut session = started()?;
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
        job: &fux::screens::Job,
    ) -> Result<String, String> {
        let (out, ended) = run_as(session, client, job);
        let ended = ended.map_err(|e| match client {
            Some(_) => e.notice(),
            None => e.to_string(),
        })?;
        let mut text = String::from_utf8_lossy(&out).into_owned();
        for value in &ended {
            text.push_str(&value.text().map_err(|f| f.message)?);
            text.push('\n');
        }
        Ok(text)
    }

    pub(crate) fn run_as(
        session: &mut Session,
        client: Option<ClientId>,
        job: &fux::screens::Job,
    ) -> (
        Vec<u8>,
        Result<Vec<fuxth::Value<fux::script::Fux>>, fux::script::Failed>,
    ) {
        let mut out = Vec::new();
        let mut machine = match job.machine(None, None) {
            Ok(machine) => machine,
            Err(failed) => return (out, Err(failed)),
        };
        let mut keymap = session.test_keymap.clone();
        if let Some(p) = client
            .and_then(|c| session.views.get(&c))
            .map(|v| v.presentation.clone())
        {
            keymap.preferences.presentation(p);
        }
        let context = fux_wire::protocol::Context {
            client,
            cwd: std::env::current_dir().unwrap_or_default(),
            ..fux_wire::protocol::Context::default()
        };
        let mut host = fux::script::Host::new(context.clone(), Some(&mut keymap), &mut out);
        let mut process = InProcess(session);
        let mut asks = fux::script::AsClient {
            asks: &mut process,
            context: context.clone(),
            opened: None,
            on_commit: None,
            stop: None,
        };
        let ended = fux::script::run(&mut machine, &mut host, &mut asks, None).and_then(|stack| {
            fux::script::Asks::commit(&mut asks, &mut keymap, context)?;
            Ok(stack)
        });
        let opened = asks.opened;
        if let (Some(client), Some(screen), true) = (client, opened, ended.is_ok()) {
            session.open_screen(client, screen);
        }
        deliver(session);
        (out, ended)
    }

    /// What the server sent clients, taken as their clients take it: a
    /// program to run, keys typed, a screen ended. The rest is left.
    pub(crate) fn deliver(session: &mut Session) {
        use fux_wire::protocol::ToAttached;
        let outbox = std::mem::take(&mut session.outbox);
        let mut kept = Vec::new();
        for outgoing in outbox {
            match outgoing {
                super::Outgoing::To(client, ToAttached::Open(open)) => {
                    let context = fux_wire::protocol::Context {
                        client: Some(client),
                        of: Some(session.run),
                        ..Default::default()
                    };
                    match fux::script::admit(open, Some(&session.test_keymap), &context, None) {
                        Ok(screen) => session.open_screen(client, screen),
                        Err(failed) => {
                            session.notify_to(client, failed.notice(), true);
                        }
                    }
                }
                super::Outgoing::To(client, ToAttached::Ended(opening)) => {
                    if let Some(keys) = session.test_keys.get_mut(&client) {
                        keys.screen.ended(opening);
                    }
                }
                super::Outgoing::To(client, ToAttached::Keys(inputs)) => {
                    use fux_vt::keys::decode::Input;
                    let typed = inputs.into_iter().flat_map(|input| match input {
                        fux_wire::command::Input::Key(press) => vec![Input::Key(press.into())],
                        fux_wire::command::Input::Text(text) => text
                            .chars()
                            .map(|c| Input::Key(fux_vt::keys::KeyPress::char(c).into()))
                            .collect(),
                        fux_wire::command::Input::Paste(text) => vec![Input::Paste(text)],
                        fux_wire::command::Input::Bytes(_) => Vec::new(),
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

    /// What a reply's answer prints, a value a line, or why it failed.
    pub(crate) fn said(reply: &Result<Reply, super::Error>) -> String {
        let values = match reply {
            Ok(reply) => fux::script::answered(reply.clone()).map_err(|f| f.to_string()),
            Err(error) => Err(error.to_string()),
        };
        match values {
            Ok(values) => values
                .iter()
                .map(fuxth::Value::text)
                .collect::<Result<Vec<_>, _>>()
                .map(|lines| lines.join("\n"))
                .unwrap_or_else(|f| f.message),
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
    ) -> Result<Vec<fux_wire::command::Command>, String> {
        struct Recorder<'s>(InProcess<'s>, Vec<fux_wire::command::Command>);
        impl fux::script::Asks for Recorder<'_> {
            fn ask(
                &mut self,
                asked: &fux_wire::protocol::Asked,
            ) -> Result<Reply, fux::script::Failed> {
                let recorded = match &asked.request {
                    Request::Do(Command::Client { action, .. }) => Command::Client {
                        client: None,
                        action: action.clone(),
                    },
                    Request::Do(Command::Resolve { .. })
                    | Request::Wait(..)
                    | Request::NextEvents { .. } => return self.0.ask(asked),
                    Request::Do(command) => command.clone(),
                };
                self.1.push(recorded);
                Ok(Reply::default())
            }
        }
        let tokens = fuxth::token::source("test", code).map_err(|e| e.to_string())?;
        let mut machine = fux::script::compile(&tokens).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let context = fux_wire::protocol::Context {
            client: Some(client),
            ..fux_wire::protocol::Context::default()
        };
        let mut host = fux::script::Host::new(context, None, &mut out);
        let mut recorder = Recorder(InProcess(s), Vec::new());
        fux::script::run(&mut machine, &mut host, &mut recorder, None)
            .map_err(|e| e.to_string())?;
        Ok(recorder.1)
    }

    /// A client's keys, as its client reads them: its decoder, and the
    /// screen it has open or what its keys are in the middle of.
    pub(crate) struct Keys {
        decoder: fux_vt::keys::decode::Decoder,
        pub(crate) screen: fux::screens::Display,
    }

    impl Default for Keys {
        fn default() -> Self {
            Self {
                decoder: fux_vt::keys::decode::Decoder::new(fux_vt::keys::decode::Timing::new(
                    35, 5000,
                )),
                screen: Default::default(),
            }
        }
    }

    impl Session {
        /// Bytes typed at client `client`'s terminal, as its client would
        /// take them: through its screen, else its keys (the session's
        /// `test_keymap`), each action sent, each program run as the client.
        pub(crate) fn input(&mut self, client: ClientId, bytes: &[u8]) {
            let mut inputs = Vec::new();
            let now = Instant::now();
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
        pub(crate) fn open_screen(&mut self, client: ClientId, screen: fux::screens::Screen) {
            let keys = self.test_keys.entry(client).or_default();
            let acts = keys.screen.open(screen, &self.test_keymap.keys.modes);
            self.apply(client, acts);
            deliver(self);
        }

        /// Runs fuxth as client `client`: a failure, or what it says, in its
        /// notice, as its client shows them.
        pub(crate) fn run_as(&mut self, client: ClientId, job: &fux::screens::Job) {
            match output_job(self, Some(client), job) {
                Err(error) => self.notify_to(client, error, true),
                Ok(said) if !said.trim().is_empty() => {
                    self.notify_to(client, said.trim().replace('\n', "  "), false);
                }
                Ok(_) => {}
            }
        }

        fn effects(&mut self, client: ClientId, effects: Vec<fux::screens::Dispatch>) {
            for effect in effects {
                match effect {
                    fux::screens::Dispatch::Send(act) => self.apply(client, vec![act]),
                    fux::screens::Dispatch::Run(job) => self.run_as(client, &job),
                    fux::screens::Dispatch::Fence => {}
                }
                deliver(self);
            }
        }

        pub(crate) fn inputs(
            &mut self,
            client: ClientId,
            inputs: Vec<fux_vt::keys::decode::Input>,
        ) {
            for input in inputs {
                let Some(read_only) = self.views.get(&client).map(|v| v.read_only) else {
                    return;
                };
                let keys = self.test_keys.entry(client).or_default();
                let effects = keys.screen.input(&self.test_keymap.keys, input, read_only);
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
                fux_vt::keys::encode::key_bytes((*key).into(), screen.key_mode(), &mut bytes)
            }
            Input::Text(text) => bytes.extend_from_slice(text.as_bytes()),
            Input::Paste(text) => {
                let bracketed = screen.mode(fux_vt::Mode::BracketedPaste);
                fux_vt::keys::encode::paste(text, bracketed, &mut bytes);
            }
            Input::Bytes(given) => bytes.extend_from_slice(given),
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use crate::session::testing::TestResult;
    /// `rows` by `cols`.
    fn sized(rows: u16, cols: u16) -> Result<fux_vt::Size, String> {
        fux_vt::Size::new(rows, cols).map_err(|e| e.to_string())
    }

    use super::testing::{
        BAR_ROW, InProcess, attached, output, output_as, requests, run, said, started,
    };
    use super::*;
    use crate::pane::Left;
    use fux_wire::code::Code;

    fn reap(s: &mut Session) {
        for dying in std::mem::take(&mut s.dying) {
            if let Some(pid) = dying.kill() {
                process::reaped(pid);
            }
        }
    }

    #[test]
    fn listings_report_held_process_outcomes() -> TestResult {
        use fux_wire::inspect::{Projection, Scope};
        let (mut s, _) = attached(12, 50)?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab drop",
        )?;
        s.end_program(PaneId::of(2), Ended::Lost);
        for code in ["ls listing", "%2 info listing"] {
            let text = output(&mut s, code)?;
            assert!(text.contains("ended, its status lost"), "{text}");
        }
        for (pid, exited, signal, expected) in [
            (Some(123), Json::Null, Json::Null, "pid 123"),
            (None, Json::Int(7), Json::Null, "exited 7"),
            (None, Json::Null, Json::Int(15), "signal 15"),
            (None, Json::Null, Json::Null, "ended, its status lost"),
        ] {
            for (scope, code) in [
                (Scope::All, "ls listing print"),
                (
                    Scope::Target(AnyRef::Pane(PaneId::of(2))),
                    "%2 info listing print",
                ),
            ] {
                let Body::Snapshot(mut snapshot) = s.inspect(scope, Projection::Facts)? else {
                    return Err("snapshot".into());
                };
                let pane = snapshot
                    .panes
                    .iter_mut()
                    .find(|p| p.id == PaneId::of(2))
                    .ok_or("pane")?;
                pane.pid = pid;
                pane.state = Some(Json::Object(vec![
                    ("exited".into(), exited.clone()),
                    ("signal".into(), signal.clone()),
                ]));
                snapshot.validate()?;
                let mut out = Vec::new();
                fux::script::testing::held_reply(
                    code,
                    Reply {
                        run: Some(s.run),
                        ..Body::Snapshot(snapshot).into()
                    },
                    &mut out,
                )?;
                let text = String::from_utf8(out)?;
                assert!(text.contains(expected), "{text}");
            }
        }
        Ok(())
    }

    #[test]
    fn listing_walks_many_containers_within_the_client_step_budget() -> TestResult {
        use fux_wire::inspect::{Projection, Scope};
        struct Once(Option<Reply>);
        impl fux::script::Asks for Once {
            fn ask(&mut self, _: &fux_wire::protocol::Asked) -> Result<Reply, fux::script::Failed> {
                self.0.take().ok_or_else(|| {
                    fux::script::Failed::new(fux_wire::code::Code::Usage, "held listing refreshed")
                })
            }
        }
        let (s, _) = attached(12, 80)?;
        let Body::Snapshot(template) = s.inspect(Scope::All, Projection::Topology)? else {
            return Err("snapshot".into());
        };
        let workspace = template.workspaces.first().ok_or("workspace")?;
        let tab = template.tabs.first().ok_or("tab")?;
        let pane = template.panes.first().ok_or("pane")?;
        for (workspaces, tabs_per_workspace) in [(1, 1000), (1000, 1)] {
            let mut snapshot = (*template).clone();
            snapshot.workspaces.clear();
            snapshot.tabs.clear();
            snapshot.panes.clear();
            snapshot.clients.clear();
            for ws in 1..=workspaces {
                let mut w = workspace.clone();
                w.id = WsId::of(ws);
                w.label = w.id.to_string();
                w.children.clear();
                for _ in 0..tabs_per_workspace {
                    let id = u32::try_from(snapshot.panes.len())?.saturating_add(1);
                    let mut t = tab.clone();
                    let mut p = pane.clone();
                    t.id = TabId::of(id);
                    t.workspace = w.id;
                    t.label = PaneId::of(id).to_string();
                    t.host = None;
                    t.revision = None;
                    t.nodes = vec![fux_wire::layout::Node::Pane(PaneId::of(id))];
                    p.id = PaneId::of(id);
                    p.tab = t.id;
                    p.label = p.id.to_string();
                    p.version.pane = p.id;
                    w.children.push(AnyRef::Tab(t.id));
                    snapshot.tabs.push(t);
                    snapshot.panes.push(p);
                }
                snapshot.workspaces.push(w);
            }
            snapshot.validate()?;
            let lines = snapshot
                .workspaces
                .len()
                .saturating_add(snapshot.tabs.len())
                .saturating_add(snapshot.panes.len())
                .saturating_add(1);
            let reply = Reply {
                run: Some(s.run),
                ..Body::Snapshot(Box::new(snapshot)).into()
            };
            let mut machine = fux::script::compile(&fuxth::token::source(
                "many-tab-listing",
                "all str topology snapshot listing print",
            )?)?;
            let mut out = Vec::new();
            let mut host =
                fux::script::Host::new(fux_wire::protocol::Context::default(), None, &mut out);
            fux::script::run(&mut machine, &mut host, &mut Once(Some(reply)), None)?;
            let text = String::from_utf8(out)?;
            assert_eq!(text.lines().count(), lines);
            assert!(
                text.lines()
                    .any(|line| line.trim_start().starts_with("%1000 "))
            );
        }
        Ok(())
    }

    #[test]
    fn held_fields_survive_the_source_pane_closing() -> TestResult {
        let (mut s, _) = attached(12, 50)?;
        assert_eq!(
            output(
                &mut s,
                "opts str agent in 80 24 size str /bin/cat program new-tab let held held str version get let before held pane kill held str version get before eq"
            )?,
            "true\n"
        );
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab drop",
        )?;
        assert_eq!(
            output(
                &mut s,
                "%3 capture let held %3 kill held str last_status get kind"
            )?,
            "null\n"
        );
        Ok(())
    }

    #[test]
    fn scoped_topology_keeps_full_membership_and_the_shared_layout_revision() -> TestResult {
        use fux_wire::inspect::{Projection, Scope};
        use fux_wire::layout::Node;
        let (mut s, _) = attached(12, 80)?;
        run(
            &mut s,
            "opts +1 in 80 24 size new-tab drop %2 str horizontal axis split drop opts +1 in 80 24 size new-tab drop",
        )?;
        // A tab's whole layout; a pane's tab as far as it is in it, the
        // pane alone.
        for (scope, panes) in [
            (
                Scope::Target(AnyRef::Tab("@2".parse().map_err(|_| "tab")?)),
                2,
            ),
            (Scope::Target(AnyRef::Pane(PaneId::of(2))), 1),
        ] {
            let Body::Snapshot(snapshot) = s.inspect(scope, Projection::Topology)? else {
                return Err("snapshot".into());
            };
            snapshot.validate()?;
            let ws = snapshot.workspaces.first().ok_or("workspace")?;
            assert_eq!(ws.children.len(), 3);
            assert_eq!((snapshot.tabs.len(), snapshot.panes.len()), (1, panes));
            let tab = snapshot.tabs.first().ok_or("tab")?;
            assert_eq!(tab.revision, s.tab(tab.id).and_then(|t| t.revision(s.run)));
            let split = matches!(
                tab.nodes.first(),
                Some(Node::Split {
                    first: 1,
                    second: 2,
                    ..
                })
            );
            assert_eq!(split, panes == 2);
            let held = fux::script::answered(Reply {
                run: Some(s.run),
                ..Body::Snapshot(snapshot).into()
            })?;
            assert!(!held.is_empty());
        }
        let before = s
            .tab("@2".parse().map_err(|_| "tab")?)
            .and_then(|t| t.revision(s.run));
        s.out(PaneId::of(2), b"unchanged layout");
        assert_eq!(
            before,
            s.tab("@2".parse().map_err(|_| "tab")?)
                .and_then(|t| t.revision(s.run))
        );
        run(
            &mut s,
            "@2 str topology snapshot str tabs get first let observed_tab observed_tab str id get observed_tab str layout_revision get observed_tab str nodes get arrange",
        )?;
        assert_ne!(
            before,
            s.tab("@2".parse().map_err(|_| "tab")?)
                .and_then(|t| t.revision(s.run))
        );
        Ok(())
    }

    #[test]
    fn snapshot_is_one_turn_and_local_fields_keep_their_typed_run() -> TestResult {
        let (mut s, _) = attached(12, 50)?;
        struct Once(Option<Reply>);
        impl fux::script::Asks for Once {
            fn ask(
                &mut self,
                asked: &fux_wire::protocol::Asked,
            ) -> Result<Reply, fux::script::Failed> {
                assert!(matches!(
                    asked.request,
                    Request::Do(Command::Inspect { .. })
                ));
                self.0
                    .take()
                    .ok_or_else(|| fux::script::Failed::new(Code::Usage, "a second roundtrip"))
            }
        }
        let body = s.inspect(
            fux_wire::inspect::Scope::All,
            fux_wire::inspect::Projection::Topology,
        )?;
        let mut once = Once(Some(Reply {
            run: Some(s.run),
            ..body.into()
        }));
        let values = fux::script::testing::evaluate(
            "all str topology snapshot let held held str panes get first str id get held str panes get first str name get kind",
            None,
            &mut once,
            &mut Vec::new(),
        )?;
        assert!(once.0.is_none());
        assert!(
            matches!(values.first(), Some(fuxth::Value::App(fux::script::Fux::Pane(p, Some(run)))) if *p == PaneId::of(1) && *run == s.run)
        );
        assert_eq!(values.last(), Some(&fuxth::Value::str("null")));
        assert_eq!(
            output(
                &mut s,
                "all str facts snapshot let held %1 str changed rename held str panes get first str name get kind"
            )?,
            "null\n"
        );
        Ok(())
    }

    /// D-112: each client is shown its own child of every stack and
    /// focuses its own pane of every tab, each with a last that is another
    /// child; a close takes the clients it showed to their last, else its
    /// neighbour.
    #[test]
    fn each_client_has_one_current_and_last_at_every_node() -> TestResult {
        use crate::workspace::{Child, Shown, Stack};
        fn holds<C: Child>(stack: &Stack<C>, client: ClientId) -> Result<(), String> {
            let Shown { current, last } = stack.shown(client).ok_or("not shown")?;
            let child = |id| stack.get(id).is_some();
            (child(current) && last.is_none_or(|l| l != current && child(l)))
                .then_some(())
                .ok_or_else(|| "a current or last that is not a child".into())
        }
        fn invariants(s: &Session) -> Result<(), String> {
            for &client in s.views.keys() {
                holds(s.parts.stack(), client)?;
                for ws in s.parts.every_workspace() {
                    holds(ws.stack(), client)?;
                    for tab in ws.tabs() {
                        let Shown { current, last } = tab.seat(client).ok_or("no seat")?.focus;
                        let ok =
                            tab.holds(current) && last.is_none_or(|l| l != current && tab.holds(l));
                        ok.then_some(()).ok_or("a focus that is not the tab's")?;
                    }
                }
            }
            Ok(())
        }
        let shown = |s: &Session, client| {
            s.workspace(WsId::of(1))
                .and_then(|w| w.stack().shown(client))
        };
        let at = |current, last: Option<u32>| {
            Some(Shown {
                current: AnyRef::Tab(TabId::of(current)),
                last: last.map(|l| AnyRef::Tab(TabId::of(l))),
            })
        };
        let (mut s, c) = attached(10, 40)?;
        let d = s.attach(10, 40, None)?;
        run(
            &mut s,
            "opts +1 in 80 24 size new-tab drop opts +1 in 80 24 size new-tab drop",
        )?;
        invariants(&s)?;
        output_as(&mut s, Some(c), "me @1 str next step me @2 str next step")?;
        assert_eq!((shown(&s, c), shown(&s, d)), (at(3, Some(2)), at(1, None)));
        output_as(&mut s, Some(c), "me @3 str last step")?;
        assert_eq!(shown(&s, c), at(2, Some(3)));
        // Showing it again or reordering it changes neither.
        output_as(&mut s, Some(c), "me @2 show @2 str previous reorder")?;
        assert_eq!(shown(&s, c), at(2, Some(3)));
        // Its current tab closed, a client goes to its last; with none, to
        // the next one, else the one before.
        run(&mut s, "@2 kill")?;
        assert_eq!((shown(&s, c), shown(&s, d)), (at(3, None), at(1, None)));
        run(&mut s, "@1 kill")?;
        assert_eq!((shown(&s, c), shown(&s, d)), (at(3, None), at(3, None)));
        invariants(&s)?;
        let error = output_as(&mut s, Some(c), "me @3 str last step")
            .err()
            .ok_or("a last")?;
        assert!(error.contains("no previously shown tab"), "{error}");
        // So do workspaces and panes.
        run(&mut s, "opts str other in 80 24 size new-tab drop")?;
        output_as(&mut s, Some(c), "me +1 str next step")?;
        assert_eq!(s.place(c).map(|p| p.ws), Some(WsId::of(2)));
        run(&mut s, "+2 kill %3 str horizontal axis split drop")?;
        assert_eq!(
            s.place(c).map(|p| (p.ws, p.pane)),
            Some((WsId::of(1), PaneId::of(5)))
        );
        invariants(&s)?;
        output_as(&mut s, Some(d), "me %5 str left step")?;
        run(&mut s, "%5 kill")?;
        assert_eq!(
            [c, d].map(|v| s.place(v).map(|p| p.pane)),
            [Some(PaneId::of(3)); 2]
        );
        // A move that closes its tab leaves a last the client was shown.
        run(
            &mut s,
            "opts +1 in 80 24 size new-tab drop opts +1 in 80 24 size new-tab drop",
        )?;
        output_as(&mut s, Some(c), "me @5 show me @6 show %7 @3 move")?;
        assert_eq!(shown(&s, c), at(3, Some(5)));
        invariants(&s)?;
        // A client attached anew has no last.
        s.detach_client(c);
        let e = s.attach(10, 40, None)?;
        assert_eq!(shown(&s, e), at(3, None));
        invariants(&s)?;
        Ok(())
    }

    /// D-115: a workspace is never in itself or in one in it, and one its
    /// last child leaves closes, and so does each that holds it that it
    /// leaves empty, each journaled closed in the one it was in.
    #[test]
    fn workspaces_nest_without_cycles_and_close_as_they_empty() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts +1 in str a in str b in 80 24 size new-tab drop",
        )?;
        let path = |s: &Session, ws| s.parts.path(WsId::of(ws));
        assert_eq!(path(&s, 3), [1, 2, 3].map(WsId::of));
        for (ws, into) in [(1, 1), (1, 2), (1, 3), (2, 3)] {
            let code = format!("+{ws} +{into} move");
            let refused = output(&mut s, &code).err().ok_or(code.clone())?;
            assert!(refused.starts_with("bad_value:"), "{code}: {refused}");
        }
        // Out of a, b leaves it empty: a closes.
        run(&mut s, "+3 all move")?;
        assert_eq!((path(&s, 3), path(&s, 2)), (vec![WsId::of(3)], Vec::new()));
        run(
            &mut s,
            "opts +3 in str c in str d in 80 24 size new-tab drop",
        )?;
        let closed = told(&mut s, |s| {
            let _ = run(s, "%3 kill");
        });
        assert_eq!(
            closed,
            [
                r#"{"event":"closed","pane":"%3","tab":"@3","workspace":"+5"}"#,
                r#"{"event":"closed","tab":"@3","workspace":"+5"}"#,
                r#"{"event":"closed","workspace":"+5","parent":"+4"}"#,
                r#"{"event":"closed","workspace":"+4","parent":"+3"}"#,
                "",
            ]
            .join("\n")
        );
        // b keeps its own tab.
        assert_eq!(path(&s, 3), [WsId::of(3)]);
        Ok(())
    }

    /// A client leaving a workspace that moves out of a project, or one in
    /// it that closes, is told left in the project and reached where it
    /// is: the project's reader reads every `shown` the whole journal does.
    #[test]
    fn a_projects_reader_reads_its_clients_leaving_what_moves_or_closes() -> TestResult {
        let (mut s, c) = attached(10, 40)?;
        for path in ["proj", "proj in str agent1", "proj in str agent2", "other"] {
            run(
                &mut s,
                &format!("opts str {path} in 80 24 size new-tab drop"),
            )?;
        }
        let proj = AnyRef::Workspace(WsId::of(2));
        let reader =
            |s: &Session, scope| crate::journal::Reader::new(scope, s.parts.journal(), s.run, None);
        let (mut project, mut all) = (
            reader(&s, fux_wire::inspect::Scope::Target(proj))?,
            reader(&s, fux_wire::inspect::Scope::All)?,
        );
        run(
            &mut s,
            "str c1 client str agent1 workspace show str agent1 workspace str other workspace move",
        )?;
        run(
            &mut s,
            "str c1 client str agent2 workspace show str agent2 workspace kill",
        )?;
        let shown = |lines: String| -> Vec<String> {
            (lines.lines().filter(|l| l.contains(r#""event":"shown""#)))
                .map(str::to_owned)
                .collect()
        };
        let (project, all) = (
            shown(project.lines(s.parts.journal(), |t| s.holder(t)).0),
            shown(all.lines(s.parts.journal(), |t| s.holder(t)).0),
        );
        assert_eq!(s.place(c).map(|p| p.ws), Some(WsId::of(2)));
        // Reached agent1; left it moved and reached proj; left proj for
        // agent2, and left agent2 closed for proj. Only leaving +1 is not
        // the project's.
        let at = |lines: &[String]| -> Vec<String> {
            let ws = |l: &String| {
                l.split(r#""workspace":""#)
                    .nth(1)?
                    .get(..2)
                    .map(str::to_owned)
            };
            lines.iter().filter_map(ws).collect()
        };
        assert_eq!(at(&project), ["+3", "+3", "+2", "+2", "+4", "+4", "+2"]);
        assert_eq!(all.get(1..), Some(&project[..]), "{all:?}");
        Ok(())
    }

    /// An attach shows its target and what holds it with nothing shown
    /// before (D-112): no last, and a close goes to the next one.
    #[test]
    fn an_attach_shows_its_target_with_no_last() -> TestResult {
        use crate::workspace::Shown;
        let (mut s, _) = attached(10, 40)?;
        for ws in ["two", "two", "two", "three"] {
            run(&mut s, &format!("opts str {ws} in 80 24 size new-tab drop"))?;
        }
        let state = fux::script::default_state().map_err(|e| e.to_string())?;
        let size = Size::new(10, 40).map_err(|e| e.to_string())?;
        let attach = Attach {
            target: Some(command::AttachTarget::Tab(TabId::of(3))),
            ..Attach::of(size, state.preferences.presentation())
        };
        let e = s.attach_as(attach, Instant::now())?;
        fn on<I>(current: I, last: Option<I>) -> Option<Shown<I>> {
            Some(Shown { current, last })
        }
        let tabs = |s: &Session, ws| s.workspace(WsId::of(ws)).and_then(|w| w.stack().shown(e));
        let on_tab = |tab| on(AnyRef::Tab(TabId::of(tab)), None);
        assert_eq!(s.parts.stack().shown(e), on(WsId::of(2), None));
        assert_eq!(tabs(&s, 2), on_tab(3));
        assert_eq!(tabs(&s, 1), on_tab(1));
        for (from, refused) in [
            ("@3", "no previously shown tab"),
            ("+2", "no previously shown workspace"),
        ] {
            let error = output_as(&mut s, Some(e), &format!("me {from} str last step")).err();
            assert!(error.is_some_and(|e| e.contains(refused)), "{from}");
        }
        run(&mut s, "@3 kill")?;
        assert_eq!(tabs(&s, 2), on_tab(4));
        run(&mut s, "+2 kill")?;
        assert_eq!(s.parts.stack().shown(e), on(WsId::of(3), None));
        Ok(())
    }

    #[test]
    fn snapshot_projections_keep_order_tree_fit_launch_and_cursor() -> TestResult {
        use fux_wire::inspect::{Projection, Scope};
        use fux_wire::layout::Node;
        let (mut s, c) = attached(12, 50)?;
        run(&mut s, "%1 str horizontal axis 2 3 shares split drop")?;
        for projection in [Projection::Facts, Projection::Topology, Projection::Recipe] {
            let Body::Snapshot(mut snapshot) = s.inspect(Scope::Client(c), projection.clone())?
            else {
                return Err("no snapshot".into());
            };
            snapshot.validate()?;
            assert_eq!(snapshot.cursor, s.parts.journal().end(s.run));
            assert_eq!(
                snapshot.panes.iter().map(|p| p.id).collect::<Vec<_>>(),
                [PaneId::of(1), PaneId::of(2)]
            );
            let tab = snapshot.tabs.first().ok_or("no tab")?;
            assert!(
                matches!(tab.nodes.first(), Some(Node::Split { first_share,second_share,.. }) if first_share.get() == 2 && second_share.get() == 3)
            );
            assert!(!tab.fixed);
            assert_eq!(
                snapshot.clients.first().map(|v| (v.rows, v.pane_rows)),
                Some((12, 12))
            );
            for pane in &snapshot.panes {
                assert_eq!(pane.state.is_some(), projection != Projection::Topology);
                assert_eq!(pane.cwd.is_some(), projection == Projection::Recipe);
                if let Some(launch) = &pane.launch {
                    assert!(launch.shell);
                    assert!(!launch.argv.is_empty());
                }
            }
            if let Some(launch) = snapshot.panes.first_mut().and_then(|p| p.launch.as_mut()) {
                launch.cwd = fux_wire::inspect::Path::Bytes(vec![255; 70_000]);
            }
            let bytes = fux_wire::wire::encode(&*snapshot);
            assert_eq!(
                fux_wire::wire::Reader::new(&bytes).whole::<fux_wire::inspect::Snapshot>()?,
                *snapshot
            );
            let mut malformed = *snapshot;
            if let Some(Node::Split { first, .. }) =
                malformed.tabs.first_mut().and_then(|t| t.nodes.first_mut())
            {
                *first = u32::MAX;
            }
            assert!(malformed.validate().is_err());
        }
        Ok(())
    }

    #[test]
    fn capability_facts_distinguish_reports_from_output_and_follow_one_host() -> TestResult {
        fn facts(s: &Session, id: ClientId) -> Result<Json, Box<dyn std::error::Error>> {
            let Body::Snapshot(snapshot) = s.inspect(
                fux_wire::inspect::Scope::Client(id),
                fux_wire::inspect::Projection::Facts,
            )?
            else {
                return Err("snapshot".into());
            };
            let client = snapshot
                .clients
                .iter()
                .find(|c| c.id == id)
                .ok_or("client")?;
            Ok(Json::Object(vec![
                ("terminal".into(), client.terminal_facts.clone()),
                ("output".into(), client.output.clone()),
            ]))
        }
        let (mut s, c) = attached(10, 40)?;
        let d = s.attach(10, 40, None)?;
        let initial = facts(&s, c)?;
        let terminal = initial.get("terminal").ok_or("terminal")?;
        for name in [
            "sixel_reported",
            "cell_reported",
            "underline_styles_reported",
            "kitty_flags_reported",
        ] {
            assert_eq!(terminal.get(name), Some(&Json::Null), "{name}");
        }
        assert_eq!(
            initial.get("output").and_then(|o| o.get("graphics")),
            Some(&Json::str("text"))
        );
        s.input(c, b"\x1b[?62c");
        assert_eq!(
            facts(&s, c)?
                .get("terminal")
                .and_then(|t| t.get("sixel_reported")),
            Some(&Json::Bool(false))
        );
        // Positive sixel evidence still needs an accepted cell size.
        s.terminal_reply(c, fux_vt::keys::decode::Reply::Attributes { sixel: true });
        s.terminal_reply(
            c,
            fux_vt::keys::decode::Reply::CellSize {
                width: 1,
                height: 2,
            },
        );
        let refused = facts(&s, c)?;
        assert_eq!(
            refused
                .get("terminal")
                .and_then(|t| t.get("cell_reported"))
                .and_then(|cell| cell.get("width")),
            Some(&Json::int(1))
        );
        assert_eq!(
            refused.get("output").and_then(|o| o.get("graphics")),
            Some(&Json::str("text"))
        );
        s.terminal_reply(
            c,
            fux_vt::keys::decode::Reply::CellSize {
                width: 10,
                height: 20,
            },
        );
        s.terminal_reply(c, fux_vt::keys::decode::Reply::UnderlineStyles);
        s.terminal_reply(c, fux_vt::keys::decode::Reply::KittyFlags(0));
        assert_eq!(
            facts(&s, c)?
                .get("terminal")
                .and_then(|t| t.get("kitty_flags_reported")),
            Some(&Json::int(0))
        );
        s.terminal_reply(c, fux_vt::keys::decode::Reply::KittyFlags(5));
        s.settled();
        let reported = facts(&s, c)?;
        let terminal = reported.get("terminal").ok_or("terminal")?;
        assert_eq!(terminal.get("sixel_reported"), Some(&Json::Bool(true)));
        assert_eq!(
            terminal.get("underline_styles_reported"),
            Some(&Json::Bool(true))
        );
        assert_eq!(terminal.get("kitty_flags_reported"), Some(&Json::int(5)));
        let effective = reported.get("output").ok_or("output")?;
        assert_eq!(effective.get("graphics"), Some(&Json::str("sixel")));
        assert_eq!(effective.get("underline_styles"), Some(&Json::Bool(true)));
        assert_eq!(effective.get("clipboard"), Some(&Json::str("osc52")));
        let requested = crate::outer::KITTY_FLAGS;
        assert!(crate::outer::KITTY_PUSH.starts_with(format!("\x1b[>{requested}u").as_bytes()));
        assert_eq!(
            effective.get("kitty_flags_requested"),
            Some(&Json::int(requested))
        );
        assert_eq!(output(&mut s, "@1 str host get")?, "c1\n");
        s.input(d, b"x");
        assert_eq!(output(&mut s, "@1 str host get")?, "c2\n");
        assert_eq!(s.graphics(PaneId::of(1)), fux_vt::Graphics::Text);
        assert_eq!(facts(&s, d)?.get("terminal"), initial.get("terminal"));
        s.terminal_reply(
            d,
            fux_vt::keys::decode::Reply::CellSize {
                width: 10,
                height: 20,
            },
        );
        s.views
            .get_mut(&d)
            .ok_or("view")?
            .presentation
            .terminal_clipboard = false;
        let other = facts(&s, d)?;
        let effective = other.get("output").ok_or("output")?;
        assert_eq!(effective.get("graphics"), Some(&Json::str("cell")));
        assert_eq!(effective.get("clipboard"), Some(&Json::Null));
        s.detach_client(d);
        assert_eq!(output(&mut s, "@1 str host get")?, "c1\n");
        s.detach_client(c);
        assert_eq!(output(&mut s, "@1 str host get")?, "null\n");
        assert_eq!(s.graphics(PaneId::of(1)), fux_vt::Graphics::Text);
        Ok(())
    }

    /// A client that only watches, of `rows` by `cols`, attached.
    fn watch(s: &mut Session, rows: u16, cols: u16) -> Result<ClientId, String> {
        let attach = Attach {
            read_only: true,
            ..Attach::of(
                sized(rows, cols)?,
                fux::script::default_state()
                    .map_err(|e| e.to_string())?
                    .preferences
                    .presentation(),
            )
        };
        s.attach_client(attach).map_err(|e| e.to_string())
    }

    #[test]
    fn arrange_refuses_invalid_or_stale_proposals_without_a_commit() -> TestResult {
        let (mut s, client) = attached(24, 80)?;
        run(
            &mut s,
            "%1 str horizontal axis split drop %2 str vertical axis split drop",
        )?;
        let tab = TabId::of(1);
        let revision = s
            .tab(tab)
            .and_then(|t| t.revision(s.run))
            .ok_or("revision")?;
        s.parts.show(client, AnyRef::Pane(PaneId::of(3)));
        let before = s.root(tab).cloned().ok_or("tree")?;
        let cursor = s.parts.journal().end(s.run);
        let origin = Origin::Cli(Caller::default());
        let valid = vec![
            layout::Node::Split {
                axis: Axis::Horizontal,
                first_share: NonZeroU32::MIN,
                second_share: NonZeroU32::MIN,
                first: 1,
                second: 2,
            },
            layout::Node::Pane(PaneId::of(1)),
            layout::Node::Split {
                axis: Axis::Horizontal,
                first_share: NonZeroU32::MIN,
                second_share: NonZeroU32::MIN,
                first: 3,
                second: 4,
            },
            layout::Node::Pane(PaneId::of(2)),
            layout::Node::Pane(PaneId::of(3)),
        ];
        let mut duplicate = valid.clone();
        if let Some(node) = duplicate.last_mut() {
            *node = layout::Node::Pane(PaneId::of(1));
        }
        for (given, nodes, code) in [
            (revision, vec![], Raise::BadValue),
            (
                revision,
                vec![layout::Node::Pane(PaneId::of(1))],
                Raise::BadValue,
            ),
            (
                revision,
                vec![layout::Node::Pane(PaneId::of(999))],
                Raise::BadValue,
            ),
            (revision, duplicate, Raise::BadValue),
            (
                command::LayoutRevision {
                    serial: revision.serial.saturating_sub(1),
                    ..revision
                },
                valid.clone(),
                Raise::Stale,
            ),
            (
                command::LayoutRevision {
                    tab: TabId::of(2),
                    ..revision
                },
                valid.clone(),
                Raise::BadValue,
            ),
            (
                command::LayoutRevision {
                    serial: revision.serial.saturating_add(1),
                    ..revision
                },
                valid.clone(),
                Raise::BadValue,
            ),
            (
                command::LayoutRevision {
                    run: "1-0".parse().map_err(|()| "run")?,
                    ..revision
                },
                valid,
                Raise::BadValue,
            ),
        ] {
            let command = Command::Arrange {
                target: tab,
                revision: given,
                nodes,
            };
            assert_eq!(
                s.execute(&command, &origin, Instant::now())
                    .err()
                    .map(|e| e.code()),
                Some(Code::from(code))
            );
            assert_eq!(s.root(tab), Some(&before));
            assert_eq!(s.tab(tab).and_then(|t| t.revision(s.run)), Some(revision));
            assert_eq!(s.parts.journal().end(s.run), cursor);
        }
        // Same membership after moving a border and moving it back is still stale.
        run(
            &mut s,
            "%1 str right 3 move-border %1 str left 3 move-border",
        )?;
        let stale = format!(
            "@1 str {revision} list list str horizontal 1 2 1 2 end %1 list str horizontal 1 1 3 4 end %2 %3 end arrange"
        );
        assert!(run(&mut s, &stale).is_err_and(|s| s.starts_with("stale:")));
        let before_fit = s.tab(tab).and_then(|t| t.revision(s.run));
        run(&mut s, "@1 90 30 resize")?;
        assert_ne!(s.tab(tab).and_then(|t| t.revision(s.run)), before_fit);
        for split in [
            "list str horizontal 0 1 1 2 end",
            "list str diagonal 1 1 1 2 end",
        ] {
            let invalid = format!("@1 dup str layout_revision get list {split} %1 %2 end arrange");
            let cursor = s.parts.journal().end(s.run);
            assert!(run(&mut s, &invalid).is_err());
            assert_eq!(s.parts.journal().end(s.run), cursor);
        }
        // The valid commit preserves each client's focus and journals once.
        let latest = output(&mut s, "@1 str layout_revision get")?;
        let command = format!(
            "@1 str {latest} list list str horizontal 1 2 1 2 end %3 list str horizontal 1 1 3 4 end %1 %2 end arrange"
        );
        let mut result = Ok(());
        let events = told(&mut s, |s| result = run(s, &command));
        result?;
        assert_eq!(
            events.matches("\"event\":\"laid_out\"").count(),
            1,
            "{events}"
        );
        assert_eq!(
            s.tab(tab).and_then(|t| t.focus(client)),
            Some(PaneId::of(3))
        );
        assert_eq!(
            s.root(tab).map(Tree::panes),
            Some(vec![PaneId::of(3), PaneId::of(1), PaneId::of(2)])
        );
        Ok(())
    }

    #[test]
    fn arrange_accepts_the_split_answer_as_a_pane() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "%1 str horizontal axis split let b @1 dup str layout_revision get list list str vertical 1 1 1 2 end %1 b end arrange",
        )?;
        assert_eq!(
            s.root(TabId::of(1)).map(Tree::panes),
            Some(vec![PaneId::of(1), PaneId::of(2)])
        );
        Ok(())
    }

    /// The layout presets' recipe file, loaded.
    const LAYOUTS: &str = concat!(
        "require ",
        env!("CARGO_MANIFEST_DIR"),
        "/../recipes/layouts.fux"
    );

    #[test]
    fn arrange_checks_background_and_all_sizing_views_but_excludes_watchers() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "@1 7 40 resize %1 str vertical axis split drop %2 str vertical axis split drop",
        )?;
        let propose = &format!("{LAYOUTS} @1 even-h");
        let before = s.root(TabId::of(1)).cloned();
        let revision = s.tab(TabId::of(1)).and_then(|t| t.revision(s.run));
        let cursor = s.parts.journal().end(s.run);
        let refused = run(&mut s, propose)
            .err()
            .ok_or("background fit accepted")?;
        assert!(refused.contains("its size, 7x40"), "{refused}");
        assert_eq!(s.root(TabId::of(1)), before.as_ref());
        assert_eq!(
            s.tab(TabId::of(1)).and_then(|t| t.revision(s.run)),
            revision
        );
        assert_eq!(s.parts.journal().end(s.run), cursor);
        run(&mut s, "@1 80 40 resize")?;
        let client = s.attach(40, 7, None)?;
        watch(&mut s, 3, 3)?;
        let refused = run(&mut s, propose)
            .err()
            .ok_or("small sizing client accepted")?;
        assert!(refused.contains(&format!("{client}'s screen")), "{refused}");
        s.size_client(client, 40, 80)?;
        run(&mut s, propose)?;
        Ok(())
    }

    #[test]
    fn layout_presets_keep_membership_and_their_stated_shares() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "@1 120 40 resize %1 str horizontal axis split drop %2 str horizontal axis split drop %3 str horizontal axis split drop",
        )?;
        let tab = TabId::of(1);
        run(
            &mut s,
            "@1 dup str layout_revision get list list str horizontal 2 2 1 4 end list str horizontal 1 1 2 3 end %4 %2 list str horizontal 1 1 5 6 end %1 %3 end arrange",
        )?;
        let order = vec![PaneId::of(4), PaneId::of(2), PaneId::of(1), PaneId::of(3)];
        for (recipe, axis) in [("even-h", Axis::Horizontal), ("even-v", Axis::Vertical)] {
            run(&mut s, &format!("{LAYOUTS} @1 {recipe}"))?;
            assert_eq!(s.root(tab).map(Tree::panes), Some(order.clone()));
            let area = s.tab(tab).ok_or("tab")?.fit().area();
            let placed = layout::place(s.root(tab).ok_or("tree")?, area);
            let lengths: Vec<_> = placed.panes.iter().map(|(_, r)| r.len(axis)).collect();
            let lo = lengths.iter().min().ok_or("lengths")?;
            let hi = lengths.iter().max().ok_or("lengths")?;
            assert!(hi.abs_diff(*lo) <= 1, "{lengths:?}");
        }
        for (recipe, axis, cross) in [
            ("main-h", Axis::Vertical, Axis::Horizontal),
            ("main-v", Axis::Horizontal, Axis::Vertical),
        ] {
            run(&mut s, &format!("{LAYOUTS} @1 {recipe}"))?;
            let Some(Tree::Split(split)) = s.root(tab) else {
                return Err("main layout".into());
            };
            assert_eq!(split.axis(), axis);
            assert_eq!(
                split.shares().get(),
                (NonZeroU32::MIN.saturating_add(1), NonZeroU32::MIN)
            );
            let (first, rest) = split.children();
            assert_eq!(first, &Tree::Pane(PaneId::of(4)));
            assert!(matches!(rest, Tree::Split(split) if split.axis() == cross));
            assert_eq!(s.root(tab).map(Tree::panes), Some(order.clone()));
        }
        run(&mut s, &format!("{LAYOUTS} @1 tiled"))?;
        let Some(Tree::Split(split)) = s.root(tab) else {
            return Err("tiled layout".into());
        };
        assert_eq!(split.axis(), Axis::Horizontal);
        for child in [split.children().0, split.children().1] {
            assert!(matches!(child, Tree::Split(split) if split.axis() == Axis::Vertical));
        }
        assert_eq!(s.root(tab).map(Tree::panes), Some(order));
        run(&mut s, "%2 kill %3 kill %4 kill")?;
        for recipe in ["even-h", "even-v", "main-h", "main-v", "tiled"] {
            run(&mut s, &format!("{LAYOUTS} @1 {recipe}"))?;
            assert_eq!(s.root(tab), Some(&Tree::Pane(PaneId::of(1))));
        }
        Ok(())
    }

    #[test]
    fn layout_presets_submit_one_observations_revision_without_retry() -> TestResult {
        struct Guarded<'s> {
            session: &'s mut Session,
            reads: usize,
            commits: usize,
            resize: bool,
        }
        impl fux::script::Asks for Guarded<'_> {
            fn ask(
                &mut self,
                asked: &fux_wire::protocol::Asked,
            ) -> Result<Reply, fux::script::Failed> {
                match &asked.request {
                    Request::Do(Command::Resolve { .. }) => {}
                    Request::Do(Command::Inspect { scope, projection }) => {
                        assert_eq!(
                            *scope,
                            fux_wire::inspect::Scope::Target(AnyRef::Tab(TabId::of(1)))
                        );
                        assert_eq!(*projection, fux_wire::inspect::Projection::Topology);
                        self.reads = self.reads.saturating_add(1);
                    }
                    Request::Do(Command::Arrange { revision, .. }) => {
                        self.commits = self.commits.saturating_add(1);
                        assert_eq!(asked.context.of, Some(revision.run));
                    }
                    Request::Do(_) | Request::Wait(_) | Request::NextEvents { .. } => {
                        return Err(fux::script::Failed::new(
                            Code::Usage,
                            "unexpected preset request",
                        ));
                    }
                }
                let reply = fux::script::Asks::ask(&mut InProcess(self.session), asked)?;
                if self.resize && matches!(asked.request, Request::Do(Command::Inspect { .. })) {
                    run(self.session, "@1 121 40 resize")
                        .map_err(|e| fux::script::Failed::new(Code::Usage, e))?;
                }
                Ok(reply)
            }
        }
        for recipe in ["even-h", "even-v", "main-h", "main-v", "tiled"] {
            for resize in [false, true] {
                let mut s = started()?;
                run(&mut s, "%1 str horizontal axis split drop")?;
                let before = s.root(TabId::of(1)).cloned();
                let tokens = fuxth::token::source("preset", &format!("{LAYOUTS} @1 {recipe}"))?;
                let mut machine = fux::script::compile(&tokens)?;
                let mut out = Vec::new();
                let mut host =
                    fux::script::Host::new(fux_wire::protocol::Context::default(), None, &mut out);
                let mut guarded = Guarded {
                    session: &mut s,
                    reads: 0,
                    commits: 0,
                    resize,
                };
                let result = fux::script::run(&mut machine, &mut host, &mut guarded, None);
                assert_eq!((guarded.reads, guarded.commits), (1, 1));
                if resize {
                    assert!(result.is_err_and(|e| e.code == Code::from(Raise::Stale)));
                    assert_eq!(s.root(TabId::of(1)), before.as_ref());
                } else {
                    assert!(result?.is_empty());
                }
            }
        }
        Ok(())
    }

    #[test]
    fn attach_targets_choose_the_exact_place_without_reusing_another_seat() -> TestResult {
        use command::AttachTarget as T;
        let (mut s, incumbent) = attached(24, 80)?;
        run(
            &mut s,
            "%1 str horizontal axis split drop opts str second in 80 24 size new-tab drop",
        )?;
        run(&mut s, "str c1 client %2 show")?;
        let presentation = fux::script::default_state()?.preferences.presentation();
        for (target, pane) in [
            (T::Tab(TabId::of(1)), PaneId::of(1)),
            (T::Pane(PaneId::of(2)), PaneId::of(2)),
            (T::Workspace(WsRef::Name("second".into())), PaneId::of(3)),
        ] {
            let client = s.attach_client(Attach {
                of: Some(s.run),
                headless: true,
                target: Some(target),
                ..Attach::of(sized(24, 80)?, presentation.clone())
            })?;
            assert_eq!(s.focused(client), Some(pane));
            s.detach_client(client);
        }
        assert_eq!(s.focused(incumbent), Some(PaneId::of(2)));
        let ids = s.ids.clone();
        for (of, target) in [
            (None, T::Pane(PaneId::of(99))),
            (None, T::Tab(TabId::of(99))),
            (None, T::Workspace(WsRef::Name("missing".into()))),
            (
                Some("1-2".parse().map_err(|()| "run")?),
                T::Pane(PaneId::of(1)),
            ),
        ] {
            assert!(
                s.attach_client(Attach {
                    of,
                    headless: true,
                    target: Some(target),
                    ..Attach::of(sized(24, 80)?, presentation.clone())
                })
                .is_err()
            );
            assert_eq!(s.ids, ids);
            assert_eq!(s.parts.panes().len(), 3);
        }
        Ok(())
    }

    #[test]
    fn failed_attach_does_not_consume_a_client_id() -> TestResult {
        let mut s = Session::new(
            "/nonexistent/fux.sock".into(),
            false,
            command::Run::now().ok_or("clock")?,
            fux::script::default_state()?.preferences.journal_entries,
        );
        let size = sized(24, 80)?;
        let presentation = fux::script::default_state()?.preferences.presentation();
        let initial = command::InitialWorkspace {
            options: fux::script::default_state()?.preferences.pane_options(),
            name: None,
            start: command::Start::Shell(command::Program::new("/bin/sh".into(), Vec::new())?),
            cwd: "relative".into(),
        };
        let ids = s.ids.clone();
        let refused = s.attach_client(Attach {
            headless: true,
            ..Attach::of(size, presentation.clone())
        });
        assert!(refused.is_err_and(|why| why.code() == Code::Start));
        for attach in [
            Attach {
                target: Some(command::AttachTarget::Workspace(WsRef::Name(
                    "missing".into(),
                ))),
                ..Attach::of(size, presentation.clone())
            },
            Attach {
                read_only: true,
                ..Attach::of(size, presentation.clone())
            },
            Attach::of(sized(1, 80)?, presentation.clone()),
            Attach {
                initial: Some(initial),
                ..Attach::of(size, presentation.clone())
            },
        ] {
            assert!(s.attach_client(attach).is_err());
            assert_eq!(s.ids, ids);
            assert!(s.views.is_empty() && s.parts.panes().is_empty());
        }
        let client = s.attach(24, 80, None)?;
        assert_eq!(client, ClientId::of(1));
        Ok(())
    }

    #[test]
    fn an_unknown_nearby_directory_refuses_creation() -> TestResult {
        let (mut s, client) = attached(24, 80)?;
        let before = s.parts.panes().len();
        let failure = output_as(&mut s, Some(client), "%1 str horizontal axis split")
            .err()
            .ok_or("unknown cwd accepted")?;
        assert!(
            failure.contains("working directory is unknown"),
            "{failure}"
        );
        assert_eq!(s.parts.panes().len(), before);
        output_as(
            &mut s,
            Some(client),
            "%1 str horizontal axis str / cwd split drop",
        )?;
        assert_eq!(s.parts.panes().len(), before + 1);
        Ok(())
    }

    #[test]
    fn an_unknown_fux_path_is_not_taken_from_the_starter() -> TestResult {
        let mut s = started()?;
        s.launch = Some("/nonexistent/fux.sock".into());
        s.exe = None;
        let launch = pane::Launch {
            argv: command::Program::new(
                "/bin/sh".into(),
                vec!["-c".into(), "[ -z \"${FUX+x}\" ]".into()],
            )?,
            cwd: "/".into(),
        };
        let child = s
            .spawn(PaneId::of(2), 0, &launch, (sized(24, 80)?, None))?
            .ok_or("a child")?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut status = None;
        while Instant::now() < deadline {
            status = child.leader.exited();
            if status.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = child.leader.finish();
        assert_eq!(status, Some(Ended::Exited(0)));
        Ok(())
    }

    #[test]
    fn callerless_resize_uses_the_smallest_screen_in_either_attach_order() -> TestResult {
        for sizes in [[(20, 100), (30, 60)], [(30, 60), (20, 100)]] {
            let mut s = started()?;
            watch(&mut s, 3, 5)?;
            for (rows, cols) in sizes {
                s.attach(rows, cols, None)?;
            }
            run(
                &mut s,
                "%1 str horizontal axis split drop %1 20 str horizontal resize-pane",
            )?;
            assert_eq!(
                s.parts.panes().get(&PaneId::of(1)).map(Pane::size),
                Some(sized(20, 20)?)
            );
        }
        Ok(())
    }

    #[test]
    fn a_watchers_paste_is_told_it_only_watches() -> TestResult {
        let mut s = started()?;
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

    #[test]
    fn watchers_leave_tab_sizing_to_its_own_fit() -> TestResult {
        for (rows, cols, resize) in [(10, 3, false), (30, 120, true)] {
            let mut s = started()?;
            watch(&mut s, rows, cols)?;
            assert_eq!(s.pane(PaneId::of(1))?.size(), sized(24, 80)?);
            if resize {
                run(&mut s, "@1 100 10 resize")?;
                assert_eq!(s.pane(PaneId::of(1))?.size(), sized(10, 100)?);
            }
            run(&mut s, "%1 str horizontal axis split drop")?;
            assert_eq!(
                s.pane(PaneId::of(1))?.size(),
                if resize {
                    sized(10, 50)?
                } else {
                    sized(24, 40)?
                }
            );
        }
        Ok(())
    }

    #[test]
    fn a_paste_goes_in_whole_as_one_native_paste() -> TestResult {
        let mut s = started()?;
        s.out(PaneId::of(1), b"\x1b[?2004h");
        let text = format!(
            "{}\n\n",
            std::iter::repeat_n('x', 2 << 20).collect::<String>()
        );
        let to = command::To::Now(AnyRef::Pane(PaneId::of(1)));
        s.send_keys(to, &[Input::Paste(text.clone())])?;
        let queued = s
            .parts
            .pane_mut(PaneId::of(1))
            .ok_or("no pane")?
            .drain_input();
        let expected = format!("\x1b[200~{}\x1b[201~", text.replace('\n', "\r"));
        assert!(queued == expected.as_bytes(), "the whole encoded paste");
        Ok(())
    }

    #[test]
    fn a_paste_into_an_exited_pane_is_refused() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        s.end_program(PaneId::of(2), fux_wire::answer::Ended::Exited(3));
        let pasted = run(&mut s, "%2 str x paste send-keys");
        assert!(pasted.is_err_and(|e| e.contains("exited")), "refused");
        Ok(())
    }

    #[test]
    fn an_ended_pane_asks_for_no_reports_and_refuses_keys() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        run(
            &mut s,
            "opts str agent in 80 23 size str /bin/cat program new-tab",
        )?;
        run(&mut s, &format!("str {c} client str +2 workspace show"))?;
        let id = PaneId::of(2);
        // A test's session starts no program: one is put in the pane.
        let argv = ["/bin/cat".to_owned()];
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        (s.parts.pane_mut(id).ok_or("the pane")?).set_process(Process::Reading(child));
        s.out(id, b"\x1b[?1000h\x1b[?1004h");
        let mouse = |s: &Session| crate::render::compose(s, c).map(|g| g.modes.mouse);
        assert_eq!(mouse(&s), Some(1000), "asked while it runs");
        s.end_program(id, Ended::Signalled(9));
        assert_eq!(mouse(&s), Some(0), "asked for none once it ended");
        let notice = |s: &mut Session, typed: &[u8]| {
            let view = s.views.get_mut(&c).map(|v| v.notice = None);
            s.input(c, typed);
            view.and(s.views.get(&c)).and_then(|v| v.notice.clone())
        };
        assert_eq!(notice(&mut s, b"\x1b[I").map(|n| n.text), None);
        let key = notice(&mut s, b"x").map(|n| n.text).unwrap_or_default();
        assert!(key.contains("was killed by SIGKILL"), "{key}");
        reap(&mut s);
        Ok(())
    }

    #[test]
    fn a_respawn_reads_what_the_old_program_left() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        let id = PaneId::of(2);
        s.out(id, b"\x1b[?2026h\x1b]2;old\x07\x07");
        let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
        pane.exiting(fux_wire::answer::Ended::Exited(3));
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

    #[test]
    fn what_is_left_is_read_a_turn_at_a_time_to_its_end() -> TestResult {
        // 2 MiB, then the end: all of it, a turn's worth each turn.
        let total = 2usize << 20;
        let (mut supplied, mut read, mut turns) = (0usize, 0usize, 0usize);
        loop {
            let (left, drain) = left_turn(|bytes| {
                let n = bytes.len().min(total.saturating_sub(supplied));
                supplied = supplied.saturating_add(n);
                Ok(n)
            });
            assert!(left.len() <= pane::SLICE, "{} in a turn", left.len());
            read = read.saturating_add(left.len());
            turns = turns.saturating_add(1);
            match drain {
                Drain::More => {}
                Drain::Closed => break,
                Drain::Empty => return Err("empty before its end".into()),
                Drain::Failed(error) => return Err(error.into()),
            }
        }
        assert_eq!((read, turns), (total, total / pane::SLICE + 1));
        let null = std::fs::File::open("/dev/null")?;
        assert_eq!(
            left_turn(|bytes| fuxix::io::read(&null, bytes)),
            (Vec::new(), Drain::Closed)
        );
        // Found empty just after some was read, it may hold more: the next
        // turn says; found empty as a turn begins, it is drained.
        let mut reads = [Ok(1), Err(fuxix::Errno::AGAIN)].into_iter();
        let mut read = |_: &mut [u8]| reads.next().unwrap_or(Err(fuxix::Errno::AGAIN));
        assert_eq!(left_turn(&mut read), (vec![0], Drain::More));
        assert_eq!(left_turn(&mut read), (Vec::new(), Drain::Empty));
        Ok(())
    }

    #[test]
    fn unexpected_terminal_read_errors_are_not_empty_output() -> TestResult {
        let mut reads = [Ok(1), Err(fuxix::Errno::INTR), Err(fuxix::Errno::INVAL)].into_iter();
        let (left, end) = left_turn(|bytes| {
            if let Some(first) = bytes.first_mut() {
                *first = b'x';
            }
            reads.next().ok_or(fuxix::Errno::INVAL)?
        });
        assert_eq!(
            (left, end),
            (b"x".to_vec(), Drain::Failed(fuxix::Errno::INVAL))
        );
        for end in [Ok(0), Err(fuxix::Errno::IO)] {
            assert_eq!(left_turn(|_| end), (Vec::new(), Drain::Closed));
        }
        let empty = left_turn(|_| Err(fuxix::Errno::AGAIN));
        assert_eq!(empty, (Vec::new(), Drain::Empty));
        let directory = std::fs::File::open(".")?;
        let (_, end) = left_turn(|bytes| fuxix::io::read(&directory, bytes));
        assert!(matches!(end, Drain::Failed(_)), "{end:?}");
        let (reader, _writer) = std::io::pipe()?;
        fuxix::io::set_nonblocking(&reader, true)?;
        assert_eq!(
            left_turn(|bytes| fuxix::io::read(&reader, bytes)),
            (Vec::new(), Drain::Empty)
        );
        let (mut session, client) = attached(10, 40)?;
        let id = *session.parts.panes().keys().next().ok_or("pane")?;
        session
            .parts
            .pane_mut(id)
            .ok_or("pane")?
            .read_failed(fuxix::Errno::INVAL);
        assert!(session.tell_lost());
        let view = session.views.get(&client).ok_or("client")?;
        let notice = &view.notice.as_ref().ok_or("notice")?.text;
        assert!(notice.contains("reading program terminal"), "{notice}");
        assert!(!session.tell_lost());
        Ok(())
    }

    #[test]
    fn a_final_read_failure_survives_the_exit_notice() -> TestResult {
        for (explicit, last, live) in [
            (true, false, true),
            (false, false, true),
            (false, true, true),
            (true, false, false),
            (false, false, false),
            (false, true, false),
        ] {
            let (mut session, client) = attached(10, 40)?;
            let id = PaneId::of(1);
            if !last {
                run(&mut session, "%1 str horizontal axis split")?;
            }
            if explicit {
                let runs = Runs::Explicit(pane::Launch {
                    argv: command::Program::new("/bin/sh".into(), Vec::new())?,
                    cwd: "/".into(),
                });
                let fresh = Pane::new(
                    id,
                    Some("test".into()),
                    runs,
                    sized(9, 40)?,
                    crate::pane::tests::options(1024)?,
                    Instant::now(),
                )?;
                session
                    .parts
                    .pane_mut(id)
                    .ok_or("pane")?
                    .respawned(fresh, Process::Absent);
            }
            session.out(id, b"saved");
            if live {
                session.read_failed(id, fuxix::Errno::INVAL, Instant::now());
            } else {
                session
                    .parts
                    .pane_mut(id)
                    .ok_or("pane")?
                    .read_failed(fuxix::Errno::INVAL);
            }
            session.end_program(id, Ended::Exited(0));
            let notice = session
                .views
                .get(&client)
                .and_then(|view| view.notice.as_ref())
                .ok_or("notice")?;
            assert!(
                notice.error
                    && notice.text.contains("reading program terminal")
                    && notice.text.contains(&fuxix::Errno::INVAL.to_string()),
                "{notice:?}"
            );
            assert!(notice.text.contains("exited"), "{notice:?}");
            assert_eq!(notice.text.matches("reading program terminal").count(), 1);
            if explicit {
                assert_eq!(session.pane(id)?.read_error(), Some(fuxix::Errno::INVAL));
                assert!(
                    session
                        .pane(id)?
                        .screen()
                        .rows()
                        .any(|row| row.cells_text().contains("saved"))
                );
            } else {
                assert!(session.pane(id).is_err());
            }
            if last {
                assert!(session.outbox.iter().any(|out| matches!(out, Outgoing::Shutdown(why, _) if why.contains(&fuxix::Errno::INVAL.to_string()))));
            }
        }
        Ok(())
    }

    #[cfg(feature = "checks")]
    #[test]
    fn refused_client_capture_returns_an_error() -> TestResult {
        let (mut session, client) = attached(10, 40)?;
        fux_vt::refuse_next(1);
        let result = session.client(
            Some(client),
            &ClientAction::Capture,
            Here::Cli(None, None),
            Instant::now(),
        );
        let error = result.err().ok_or("capture succeeded")?;
        assert_eq!(error.code(), Code::from(Raise::Memory));
        Ok(())
    }

    #[test]
    fn a_frame_a_resize_reads_is_read_whole() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        let id = PaneId::of(1);
        let copied = |s: &Session| {
            (s.outbox.iter()).any(|o| matches!(o, Outgoing::To(to, ToAttached::Paint(b)) if *to == c && **b == *b"\x1b]52;c;aGk=\x07"))
        };
        s.out(id, b"\x1b[?2026h\x07\x1b]52;c;aGk=\x07");
        assert!(!copied(&s), "held");
        s.size_client(c, 30, 100)?;
        assert!(copied(&s));
        assert!(!s.pane(id)?.bell, "rung");
        Ok(())
    }

    #[test]
    fn a_fixed_tab_no_client_sizes_is_laid_out_on_its_size() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        // A screen of one row has no room below its bar.
        output_as(&mut s, Some(c), BAR_ROW)?;
        run(&mut s, "opts 40 12 size str +1 workspace in new-tab drop")?;
        run(&mut s, &format!("str {c} client str @2 tab show"))?;
        let size = |s: &Session| {
            s.pane(PaneId::of(2))
                .map(|p| (p.size().cols(), p.size().rows()))
        };
        assert_eq!(size(&s).ok(), Some((80, 23)), "sized by the client");
        s.size_client(c, 1, 80)?;
        assert_eq!(size(&s).ok(), Some((40, 12)), "on its own size");
        Ok(())
    }

    #[test]
    fn a_small_screen_names_the_panes_it_cuts_and_hides() -> TestResult {
        let (mut s, _) = attached(25, 80)?;
        run(&mut s, "str %1 pane str vertical axis split drop")?;
        let bar = |s: &mut Session, rows, cols| {
            let grid = watch(s, rows, cols).map(|c| crate::render::compose(s, c));
            Ok::<_, String>(grid?.map(|g| g.row_text(g.rows.saturating_sub(1))))
        };
        let roomy = bar(&mut s, 30, 100)?.unwrap_or_default();
        assert!(!roomy.contains('['), "{roomy}");
        let small = bar(&mut s, 6, 80)?.unwrap_or_default();
        assert!(small.trim_end().ends_with("[cut %1 %2]"), "{small}");
        let short = bar(&mut s, 3, 80)?.unwrap_or_default();
        assert!(
            short.trim_end().ends_with("[hidden %2] [cut %1]"),
            "{short}"
        );
        // Named before the tabs and the title on a narrow bar.
        let narrow = bar(&mut s, 3, 24)?.unwrap_or_default();
        assert!(narrow.contains("[hidden %2] [cut %1]"), "{narrow}");
        Ok(())
    }

    #[test]
    fn a_new_tab_takes_the_screen_of_the_callers_tab() -> TestResult {
        let (mut s, _small) = attached(24, 80)?;
        let big = s.attach(50, 200, None)?;
        run(&mut s, "opts 100 30 size str other in new-tab drop")?;
        run(
            &mut s,
            &format!("str {big} client str other workspace show"),
        )?;
        let caller = Caller {
            gave: Some((PaneId::of(2), 0)),
            within: Within::This,
            ..Caller::default()
        };
        let new_tab = Command::NewTab {
            options: fux::script::default_state()?.preferences.pane_options(),
            start: command::Start::Shell(command::Program::new("/bin/sh".into(), Vec::new())?),
            target: Vec::new(),
            name: None,
            size: None,
            cwd: Some("/".into()),
        };
        s.execute(&new_tab, &Origin::Cli(caller), Instant::now())?;
        let tab = s.tab("@3".parse()?).ok_or("the new tab")?;
        let size = tab.fit().size();
        assert_eq!((size.cols(), size.rows()), (200, 50));
        Ok(())
    }

    #[test]
    fn in_process_waits_answer_only_after_the_frame_is_released() -> TestResult {
        for (budgeted, timeout) in [(true, 0), (false, 2)] {
            let mut s = started()?;
            let mut frame = b"\x1b[?2026h".to_vec();
            if budgeted {
                frame.extend(crate::pane::tests::spends_the_budget(24, 80));
            }
            frame.extend_from_slice(b"DONE");
            if budgeted {
                frame.extend_from_slice(b"\x1b[?2026l");
            }
            s.out(PaneId::of(1), &frame);
            if budgeted {
                assert!(s.pane(PaneId::of(1)).is_ok_and(Pane::mid_frame));
            }
            let answer = output(&mut s, &format!("%1 {timeout} timeout wait"))?;
            assert!(answer.ends_with(" screen\n"), "{answer}");
        }
        Ok(())
    }

    #[test]
    fn a_split_elsewhere_leaves_the_splitters_zoom() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "%1 str horizontal axis str / cwd split drop")?;
        run(&mut s, &format!("str {c} client focused zoom"))?;
        run(&mut s, "opts 80 24 size str other in new-tab drop")?;
        output_as(
            &mut s,
            Some(c),
            "%3 str horizontal axis str / cwd split drop",
        )?;
        let zoomed = s.tab(TabId::of(1)).and_then(Tab::zoomed);
        assert_eq!(zoomed, Some(PaneId::of(2)), "still zoomed");
        Ok(())
    }

    #[test]
    fn a_zoom_is_the_tabs_and_ends_with_its_pane() -> TestResult {
        let (mut s, c1) = attached(24, 80)?;
        let c2 = s.attach(24, 80, None)?;
        run(
            &mut s,
            "%1 str horizontal axis split drop %2 str vertical axis split drop",
        )?;
        run(
            &mut s,
            &format!("str {c1} client %1 show str {c2} client %2 show %3 zoom"),
        )?;
        let zoomed = |s: &Session| s.tab(TabId::of(1)).and_then(Tab::zoomed);
        let focused = |s: &Session| [c1, c2].map(|c| s.focused(c));
        let both = |p| [Some(PaneId::of(p)); 2];
        // Every client showing the tab focuses its zoomed pane, and its
        // own focus comes back after.
        assert_eq!(focused(&s), both(3));
        let lines = told(&mut s, |s| {
            let _ = run(s, "%3 zoom");
        });
        let unzoomed = r#"{"event":"zoomed","tab":"@1","workspace":"+1","pane":null}"#;
        assert!(lines.starts_with(unzoomed), "{lines}");
        assert_eq!(focused(&s), [1, 2].map(|p| Some(PaneId::of(p))));
        run(&mut s, "%3 zoom %1 kill")?;
        assert_eq!(zoomed(&s), Some(PaneId::of(3)), "another pane's close");
        run(&mut s, "%3 kill")?;
        assert_eq!(zoomed(&s), None);
        assert_eq!(focused(&s), both(2));
        Ok(())
    }

    #[test]
    fn attaching_to_a_zoomed_tab_keeps_its_zoom() -> TestResult {
        let (mut s, c1) = attached(24, 80)?;
        run(&mut s, "%1 str horizontal axis split drop")?;
        run(&mut s, &format!("str {c1} client %1 show %2 zoom"))?;
        let presentation = fux::script::default_state()?.preferences.presentation();
        let size = sized(24, 80)?;
        let mut clients = vec![c1];
        let lines = told(&mut s, |s| {
            for read_only in [false, true] {
                let attach = Attach {
                    target: Some(command::AttachTarget::Tab(TabId::of(1))),
                    read_only,
                    ..Attach::of(size, presentation.clone())
                };
                clients.extend(s.attach_as(attach, Instant::now()));
            }
        });
        assert_eq!(clients.len(), 3, "{lines}");
        assert!(!lines.contains("zoomed"), "{lines}");
        let zoomed = s.tab(TabId::of(1)).and_then(Tab::zoomed);
        assert_eq!(zoomed, Some(PaneId::of(2)));
        for c in clients {
            assert_eq!(s.focused(c), Some(PaneId::of(2)), "{c}");
        }
        Ok(())
    }

    #[test]
    fn a_zoomed_tab_its_client_detaches_from_keeps_its_sizes() -> TestResult {
        let sizes =
            |s: &Session| [1, 2].map(|p| s.parts.panes().get(&PaneId::of(p)).map(Pane::size));
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "%1 str horizontal axis split drop")?;
        run(&mut s, &format!("str {c} client focused zoom"))?;
        let zoomed = sizes(&s);
        s.detach_client(c);
        assert_eq!(sizes(&s), zoomed);
        Ok(())
    }

    #[test]
    fn a_respawn_refused_reads_nothing_of_the_old_program() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        let id = PaneId::of(2);
        s.out(id, b"\x1b[?2026h\x1b]2;old\x07");
        // Programs are started, as a server starts them.
        s.launch = Some("/nonexistent/fux.sock".into());
        let mut refused = String::new();
        let lines = told(&mut s, |s| {
            let respawn = run(s, "%2 str /no/such/bin program kill-first respawn");
            refused = respawn.err().unwrap_or_default();
        });
        assert!(refused.contains("/no/such/bin"), "{refused}");
        assert!(lines.is_empty(), "{lines}");
        let pane = s.pane(id)?;
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
    fn respawn_of(id: PaneId, argv: &[&str]) -> Result<Request, String> {
        Ok(Request::Do(Command::Respawn {
            target: id,
            kill: true,
            argv: match argv.split_first() {
                Some((p, args)) => Some(command::Program::new(
                    (*p).into(),
                    args.iter().map(|a| (*a).into()).collect(),
                )?),
                None => None,
            },
        }))
    }

    /// The turns that finish what `waiting` waits for, in pane `id`, to
    /// its answer; how many there were.
    fn turns(s: &mut Session, id: PaneId, waiting: &mut Waiting) -> Result<(Reply, usize), String> {
        for turns in 0..10_000 {
            if let Some(answer) = waiting.answer(s, Instant::now()) {
                return Ok((answer, turns));
            }
            assert!(s.timers().any(|t| t.1 == Timer::Unread(id)), "no turn due");
            s.fire_now(Timer::Unread(id));
        }
        Err("never answered".into())
    }

    #[test]
    fn a_replacements_caller_is_current_while_old_output_drains() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        let id = PaneId::of(2);
        let left: Vec<_> = std::iter::repeat_n(crate::pane::tests::spends_the_budget(24, 80), 4)
            .flatten()
            .collect();
        s.out(id, &left);
        let Begun::Waiting(mut waiting) =
            s.begin_now(respawn_of(id, &[])?, &Origin::Cli(Caller::default()))
        else {
            return Err("the predecessor's output must still need turns".into());
        };
        let caller = |generation| {
            Origin::Cli(Caller {
                gave: Some((id, generation)),
                within: Within::This,
                ..Caller::default()
            })
        };
        let here = Command::Resolve {
            text: String::new(),
            kind: Kind::Pane,
        };
        let found = s.command_now(&here, &caller(1));
        assert_eq!(
            found.end(),
            fux_wire::protocol::End::Done,
            "the replacement's own identity: {found:?}"
        );
        assert_eq!(found, Body::Found(AnyRef::Pane(id)).into());
        assert_eq!(
            s.command_now(&here, &caller(0)).end().code(),
            Some(Code::StrayPane)
        );
        assert!(s.respawning.contains_key(&id));
        let (_, turns) = turns(&mut s, id, &mut waiting)?;
        assert!(turns > 1, "the old output needed {turns} turns");
        assert_eq!(
            s.command_now(&here, &caller(1)).end(),
            fux_wire::protocol::End::Done
        );
        assert_eq!(
            s.command_now(&here, &caller(0)).end().code(),
            Some(Code::StrayPane)
        );
        run(&mut s, "%2 kill")?;
        assert_eq!(
            s.command_now(&here, &caller(1)).end().code(),
            Some(Code::NoPane)
        );
        Ok(())
    }

    #[test]
    fn a_respawn_reads_what_was_left_in_turns() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size 4096 history-bytes false reflow 3 alternate-scroll-lines 1024 picture-bytes false program-clipboard str /bin/cat program new-tab",
        )?;
        let (id, origin) = (PaneId::of(2), Origin::Cli(Caller::default()));
        let mut stream = crate::journal::Reader::new(
            fux_wire::inspect::Scope::All,
            s.parts.journal(),
            s.run,
            None,
        )?;
        let spends = crate::pane::tests::spends_the_budget(24, 80);
        let mut left: Vec<u8> = std::iter::repeat_n(spends, 4).flatten().collect();
        left.push(0x07);
        s.out(id, &left);
        let Begun::Waiting(mut waiting) = s.begin_now(respawn_of(id, &[])?, &origin) else {
            return Err("answered before what was left was read".into());
        };
        assert_eq!(waiting.due(), None);
        let Begun::Done(again) = s.begin_now(respawn_of(id, &[])?, &origin) else {
            return Err("a second respawn under way".into());
        };
        assert_eq!(
            again.end().code(),
            Some(Code::StillRunning),
            "{:?}",
            again.end()
        );
        assert_eq!(
            s.respawning.get(&id).map(|r| r.fresh.history_bytes()),
            Some(4096)
        );
        run(&mut s, "%2 128 history-budget")?;
        let (answer, turns) = turns(&mut s, id, &mut waiting)?;
        assert!(turns > 1, "read in {turns} turns");
        assert_eq!(answer.end(), fux_wire::protocol::End::Done);
        let pane = s.pane(id)?;
        assert_eq!(answer, acted(pane.version(s.run)));
        assert_eq!(pane.history_bytes(), 128);
        assert_eq!(pane.options().history_bytes.get(), 128);
        assert!(!pane.options().program_clipboard);
        assert!(!pane.options().reflow);
        assert_eq!(pane.options().alternate_scroll_lines.get(), 3);
        assert_eq!(pane.options().picture_bytes.get(), 1024);
        assert!(output(&mut s, "%2 str history_bytes get")?.contains("128"));
        assert!(output(&mut s, "all dump")?.contains("128 history-bytes false reflow 3 alternate-scroll-lines 1024 picture-bytes false program-clipboard"));
        let lines = stream.lines(s.parts.journal(), |t| s.holder(t)).0;
        let at = |what: &str| {
            lines
                .find(&format!(r#""{what}","pane":"%2""#))
                .ok_or(lines.clone())
        };
        assert!(at("bell")? < at("respawned")?, "{lines}");
        assert_eq!(lines.matches("history_budget").count(), 1, "{lines}");
        Ok(())
    }

    #[test]
    fn a_pane_closed_while_respawning_abandons_it() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        run(
            &mut s,
            "opts str agent in 80 23 size str /bin/cat program new-tab",
        )?;
        run(&mut s, &format!("str {c} client str +2 workspace show"))?;
        let id = PaneId::of(2);
        s.out(id, &crate::pane::tests::spends_the_budget(23, 80));
        // Programs are started, as a server starts them.
        s.launch = Some("/nonexistent/fux.sock".into());
        let begun = s.begin_now(respawn_of(id, &[])?, &Origin::Cli(Caller::default()));
        s.launch = None;
        let Begun::Waiting(mut waiting) = begun else {
            return Err("answered before what was left was read".into());
        };
        assert_eq!(s.children().count(), 1, "the program started for it");
        // The program it is respawned over, one that ignores the hangup,
        // as a respawn holds it while what it left is still read.
        let argv = ["/bin/sh", "-c", "trap '' HUP; exec sleep 60"].map(str::to_owned);
        let old = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        let old_pid = old.leader.pid();
        (s.respawning.get_mut(&id).ok_or("the respawn")?).old = Some(old);
        run(&mut s, "%2 kill")?;
        let answer = waiting.answer(&s, Instant::now()).ok_or("no answer")?;
        assert_eq!(
            answer.end().code(),
            Some(Code::Closed),
            "{:?}",
            answer.end()
        );
        let said = (s.views.get(&c))
            .and_then(|v| v.notice.clone())
            .map(|n| n.text);
        let said = said.unwrap_or_default();
        assert!(said.starts_with("%2 closed while respawning"), "{said}");
        assert_eq!(
            s.dying.len(),
            2,
            "the program started for it and the old, ended"
        );
        reap(&mut s);
        let mut gone = false;
        for _ in 0..200 {
            gone = process::reaped(old_pid);
            if gone {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(gone, "the program respawned over still runs");
        Ok(())
    }

    #[test]
    fn what_a_program_respawned_over_left_is_read_past_its_grace() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        let id = PaneId::of(2);
        // One that ignores the hangup, with a title and a bell left unread
        // on its terminal as its grace ends.
        let code = "trap '' HUP; printf '\\033]2;late\\007\\007'; exec sleep 60";
        let argv = ["/bin/sh", "-c", code].map(str::to_owned);
        let old = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(24, 80)?, None))?;
        std::thread::sleep(Duration::from_millis(200));
        let now = Instant::now();
        let pane = s.pane(id)?;
        let fresh = Pane::new(
            id,
            None,
            pane.runs.clone(),
            pane.size(),
            pane.options(),
            now,
        )?;
        let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
        if let Process::Reading(child) = pane.set_process(Process::Respawning) {
            s.dying.push(Dying {
                child,
                deadline: now,
            });
        }
        let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
        pane.drain(true);
        let new = Replacement {
            actor: s.parts.journal().current_actor(),
            fresh,
            process: Process::Absent,
            old: Some(old),
            grace: Some(now),
            since: now,
        };
        s.respawning.insert(id, new);
        let lines = told(&mut s, |s| {
            for _ in 0..100 {
                if !s.respawning.contains_key(&id) {
                    break;
                }
                s.fire(Timer::Unread(id), crate::after(Instant::now(), GRACE));
            }
        });
        reap(&mut s);
        let at = |what: &str| {
            lines
                .find(&format!(r#""{what}","pane":"%2""#))
                .ok_or(lines.clone())
        };
        assert!(lines.contains(r#""title":"late""#), "{lines}");
        assert!(at("title")? < at("respawned")?, "{lines}");
        assert!(at("bell")? < at("respawned")?, "{lines}");
        Ok(())
    }

    #[test]
    fn input_is_refused_while_respawning() -> TestResult {
        for (kill, exiting) in [(true, None), (false, Some(Ended::Exited(0)))] {
            let mut s = started()?;
            run(
                &mut s,
                "opts str agent in 80 24 size str /bin/cat program new-tab",
            )?;
            let id = PaneId::of(2);
            let argv = ["/bin/cat".to_owned()];
            let size = (sized(24, 80)?, None);
            let child = process::spawn(&argv, Path::new("/"), (&[], &[]), size)?;
            let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
            pane.set_process(Process::Reading(child));
            if let Some(ended) = exiting {
                pane.exiting(ended);
            }
            s.out(id, &crate::pane::tests::spends_the_budget(24, 80));
            let mut stream = crate::journal::Reader::new(
                fux_wire::inspect::Scope::All,
                s.parts.journal(),
                s.run,
                None,
            )?;
            let respawn = Request::Do(Command::Respawn {
                target: id,
                kill,
                argv: None,
            });
            let Begun::Waiting(mut waiting) = s.begin_now(respawn, &Origin::Cli(Caller::default()))
            else {
                return Err("answered before what was left was read".into());
            };
            let sent = run(&mut s, "%2 str abc text send-keys");
            assert!(sent.is_err_and(|e| e.contains("respawning")));
            let signalled = s.signalled(id).map(drop).map_err(|e| e.to_string());
            assert!(signalled.is_err_and(|e| e.contains("respawning")));
            turns(&mut s, id, &mut waiting)?;
            let lines = stream.lines(s.parts.journal(), |t| s.holder(t)).0;
            assert!(lines.contains(r#""respawned","pane":"%2""#), "{lines}");
            assert!(!lines.contains("input_unread"), "{lines}");
            reap(&mut s);
        }
        Ok(())
    }

    #[test]
    fn a_held_frame_is_a_turn_of_the_respawn() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        let id = PaneId::of(2);
        s.out(id, b"\x1b[?2026hheld");
        let begun = s.begin_now(respawn_of(id, &[])?, &Origin::Cli(Caller::default()));
        let Begun::Waiting(mut waiting) = begun else {
            return Err("answered with a frame held".into());
        };
        let timers: Vec<Timer> = s.timers().map(|(_, timer)| timer).collect();
        assert_eq!(timers, [Timer::Unread(id)]);
        let (_, turns) = turns(&mut s, id, &mut waiting)?;
        assert_eq!(turns, 1);
        Ok(())
    }

    #[test]
    fn a_resize_while_respawning_is_kept() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        let id = PaneId::of(2);
        s.out(id, &crate::pane::tests::spends_the_budget(24, 80));
        let begun = s.begin_now(respawn_of(id, &[])?, &Origin::Cli(Caller::default()));
        let Begun::Waiting(mut waiting) = begun else {
            return Err("answered before what was left was read".into());
        };
        run(&mut s, "%2 tab 100 30 resize")?;
        turns(&mut s, id, &mut waiting)?;
        let pane = s.pane(id)?;
        assert_eq!((pane.respawns, pane.size()), (1, sized(30, 100)?));
        Ok(())
    }

    #[test]
    fn a_respawned_program_that_ends_first_is_told_exited() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        let id = PaneId::of(2);
        s.out(id, &crate::pane::tests::spends_the_budget(24, 80));
        let mut stream = crate::journal::Reader::new(
            fux_wire::inspect::Scope::All,
            s.parts.journal(),
            s.run,
            None,
        )?;
        // Programs are started, as a server starts them.
        s.launch = Some("/nonexistent/fux.sock".into());
        let begun = s.begin_now(
            respawn_of(id, &["/bin/sh", "-c", "exit 0"])?,
            &Origin::Cli(Caller::default()),
        );
        s.launch = None;
        let Begun::Waiting(mut waiting) = begun else {
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
        turns(&mut s, id, &mut waiting)?;
        let lines = stream.lines(s.parts.journal(), |t| s.holder(t)).0;
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
        reap(&mut s);
        Ok(())
    }

    #[test]
    fn a_program_starts_in_the_cell_of_the_terminal_drawing_it() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        run(
            &mut s,
            "opts str agent in 80 23 size str /bin/cat program new-tab",
        )?;
        run(&mut s, &format!("str {c} client str +2 workspace show"))?;
        s.input(c, b"\x1b[6;20;10t");
        let id = PaneId::of(2);
        s.launch = Some("/nonexistent/fux.sock".into());
        (s.respawn(id, false, None, Instant::now()))?;
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

    #[test]
    fn a_tab_its_client_follows_starts_in_its_terminals_cell() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        s.input(c, b"\x1b[6;20;10t");
        let mut cell = |origin: Origin, size| {
            let here = s.here(&origin).map_err(|e| e.to_string())?;
            let opening = s.opening((Some(Path::new("/")), size), here);
            opening
                .map(|(_, _, (_, cell))| cell)
                .map_err(|e| e.to_string())
        };
        assert_eq!(
            cell(Origin::Client(c), None)?,
            fux_vt::CellSize::new(10, 20)
        );
        let size = Some(fux_wire::command::Screen::new(24, 80)?);
        assert_eq!(cell(Origin::Cli(Caller::default()), size)?, None);
        Ok(())
    }

    #[test]
    fn a_resize_and_a_new_cell_size_a_panes_terminal_once() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        let id = PaneId::of(1);
        let size = s.pane(id)?.size();
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
        let now = Instant::now();
        s.read_with(
            id,
            |pane| {
                between = pixels(pane);
                pane.resize(smaller, now);
            },
            now,
        );
        assert_eq!(between, None, "not sized before the resize");
        let pane = s.pane(id)?;
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

    #[test]
    fn a_tab_with_no_host_tells_its_programs_no_cell() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        let id = PaneId::of(1);
        let size = s.pane(id)?.size();
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
        run(&mut s, "opts 80 24 size str other in new-tab drop")?;
        let show = |ws: &str| format!("str {c} client str {ws} workspace show");
        let unknown = |s: &Session| winsize(s).is_some_and(|(_, pixels)| pixels.is_none());
        run(&mut s, &show("other"))?;
        assert!(unknown(&s), "hidden");
        run(&mut s, &show("+1"))?;
        assert_eq!(winsize(&s), told, "shown again");
        s.detach_client(c);
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

    #[test]
    fn a_respawn_with_no_host_is_told_no_cell() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        run(
            &mut s,
            "opts str agent in 80 23 size str /bin/cat program new-tab",
        )?;
        run(&mut s, &format!("str {c} client str +2 workspace show"))?;
        s.input(c, b"\x1b[6;20;10t");
        s.out(PaneId::of(2), b"x");
        s.detach_client(c);
        run(&mut s, "%2 respawn")?;
        let json = output(&mut s, "%2 capture json")?;
        assert!(json.contains(r#""cell":null"#), "{json}");
        Ok(())
    }

    #[test]
    fn input_an_ended_program_never_read_is_told() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        run(
            &mut s,
            "opts str agent in 80 23 size str /bin/cat program new-tab",
        )?;
        run(&mut s, &format!("str {c} client str +2 workspace show"))?;
        let id = PaneId::of(2);
        let notice = |s: &Session| s.views.get(&c).and_then(|v| v.notice.clone());
        let queue = |s: &mut Session, bytes: &[u8]| {
            let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
            pane.push(bytes);
            Ok::<_, String>(())
        };
        queue(&mut s, b"abc")?;
        let json = output(&mut s, "%2 capture json")?;
        assert!(json.contains(r#""input_queued":3"#), "{json}");
        let lines = told(&mut s, |s| s.end_program(id, Ended::Exited(0)));
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
        assert!(
            json.contains(r#""input_queued":0,"input_unread":3"#),
            "{json}"
        );
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
        reap(&mut s);
        Ok(())
    }

    #[test]
    fn the_input_modes_a_program_sets_are_its_facts() -> TestResult {
        let (mut s, _) = attached(24, 80)?;
        let modes = |s: &mut Session| -> Result<String, String> {
            let json = output(s, "%1 capture json")?;
            let from = json.find(r#""mouse_mode""#).ok_or(json.clone())?;
            let to = json.find(r#","pictures""#).ok_or(json.clone())?;
            Ok(json.get(from..to).unwrap_or_default().to_owned())
        };
        assert_eq!(
            modes(&mut s)?,
            r#""mouse_mode":null,"mouse_encoding":"default","bracketed_paste":false,"focus_events":false,"app_cursor":false,"app_keypad":false,"keyboard_flags":0"#
        );
        s.out(PaneId::of(1), b"\x1b[?1002;1006;2004;1004;1h\x1b=\x1b[>5u");
        assert_eq!(
            modes(&mut s)?,
            r#""mouse_mode":"button_motion","mouse_encoding":"sgr","bracketed_paste":true,"focus_events":true,"app_cursor":true,"app_keypad":true,"keyboard_flags":5"#
        );
        Ok(())
    }

    #[test]
    fn a_respawned_program_reads_no_report_the_ended_one_asked_for() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        run(
            &mut s,
            "opts str agent in 80 23 size str /bin/cat program new-tab",
        )?;
        run(&mut s, &format!("str {c} client str +2 workspace show"))?;
        let id = PaneId::of(2);
        let queued = |s: &Session| {
            s.pane(id)
                .map(|p| p.input().len())
                .map_err(|e| e.to_string())
        };
        s.out(id, b"\x1b[?2048h\x1b[?2031h");
        s.end_program(id, Ended::Exited(0));
        (s.parts.pane_mut(id).ok_or("the pane")?).resize(sized(10, 40)?, Instant::now());
        s.input(c, b"\x1b[?997;1n");
        assert_eq!(queued(&s)?, 0);
        run(&mut s, "%2 respawn")?;
        assert_eq!(queued(&s)?, 0);
        Ok(())
    }

    #[test]
    fn an_exit_told_is_not_told_again_at_a_respawn() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        let lines = told(&mut s, |s| {
            s.end_program(PaneId::of(2), fux_wire::answer::Ended::Exited(3));
            let _ = run(s, "%2 respawn");
        });
        let said: Vec<&str> = lines.lines().filter_map(|l| l.split('"').nth(3)).collect();
        assert_eq!(said, ["exited", "respawned"], "{lines}");
        Ok(())
    }

    #[test]
    fn a_pane_respawned_twice_in_its_grace_kills_both() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
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

    #[test]
    fn json_of_several_panes_is_one_array() -> TestResult {
        let mut s = started()?;
        for split in [false, true] {
            if split {
                run(&mut s, "%1 str horizontal axis split")?;
            }
            let alone = output(&mut s, "@1 str x text send-keys json")?;
            let mut expected = Vec::new();
            for id in s.root(TabId::of(1)).ok_or("layout")?.panes() {
                expected.push(format!(
                    r#"{{"pane":"{id}","version":"{}","status":0}}"#,
                    s.pane(id)?.version(s.run)
                ));
            }
            assert_eq!(alone, format!("[{}]\n", expected.join(",")));
            let listed = output(&mut s, "list @1 str x text send-keys end json")?;
            assert_eq!(listed, format!("[{}]\n", alone.trim_end()));
        }
        Ok(())
    }

    #[test]
    fn exit_waits_for_the_last_screen_including_held_frames() -> TestResult {
        for (held, budgeted) in [(false, true), (true, false), (true, true)] {
            let mut s = started()?;
            run(
                &mut s,
                "opts str agent in 80 24 size str /bin/true program new-tab",
            )?;
            let id = PaneId::of(2);
            if !held {
                let argv = ["/bin/sh", "-c", "exit 0"].map(str::to_owned);
                let child =
                    process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
                (s.parts.pane_mut(id).ok_or("the pane")?).set_process(Process::Reading(child));
                let exited = |s: &Session| {
                    s.parts
                        .panes()
                        .get(&id)
                        .and_then(|p| p.process().child()?.leader.exited())
                        .is_some()
                };
                let until = crate::after(Instant::now(), Duration::from_secs(10));
                while !exited(&s) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(exited(&s), "the child did not exit");
            }
            let mut out = Vec::new();
            if budgeted {
                out.extend(
                    std::iter::repeat_n(b"X\x1b[65535b", 2000)
                        .flatten()
                        .copied(),
                );
                out.extend_from_slice(b"\x1b[2J\x1b[H");
            }
            if held {
                out.extend_from_slice(b"\x1b[?2026h");
            }
            out.extend_from_slice(b"DONE");
            s.out(id, &out);
            s.end_program(id, Ended::Exited(0));
            if budgeted {
                let pane = s.parts.panes().get(&id).ok_or("the pane")?;
                assert!(pane.has_unread(), "the budget stopped no call");
                assert!(
                    pane.process().exit_status().is_none(),
                    "exited with output unread"
                );
            }
            let mut turns = 0u32;
            while s
                .parts
                .panes()
                .get(&id)
                .is_some_and(|p| p.process().exit_status().is_none())
                && turns < 10_000
            {
                s.fire_now(crate::session::Timer::Unread(id));
                turns = turns.saturating_add(1);
            }
            assert!(
                budgeted || turns == 0,
                "held={held}: exit was not immediate"
            );
            let pane = s.parts.panes().get(&id).ok_or("the pane")?;
            assert_eq!(pane.process().exit_status(), Some(Ended::Exited(0)));
            let text = pane
                .screen()
                .rows()
                .map(|r| r.cells_text())
                .collect::<String>();
            assert!(
                text.contains("DONE"),
                "held={held}, budgeted={budgeted}: {text:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_program_whose_terminal_is_written_on_still_exits() -> TestResult {
        // A large screen, where each ED is spent as 40,000 cells.
        let (mut s, _) = super::testing::attached(201, 200)?;
        run(
            &mut s,
            "opts str agent in 200 200 size str /bin/true program new-tab",
        )?;
        let id = PaneId::of(2);
        let argv = ["/bin/sh", "-c", "while :; do printf '\\033[2J'; done"].map(str::to_owned);
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        let writer = child.leader.pid();
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
        // Told exited, as each reap of another child does, while what it
        // left goes on writing: the exit is told as its leader is reaped,
        // once what was read is on its screen (D-90).
        let mut turns = 0u32;
        s.end_program(id, Ended::Exited(3));
        while !exited(&s) && turns < 10_000 {
            s.fire_now(crate::session::Timer::Unread(id));
            // As each reap of another child does.
            s.end_program(id, Ended::Exited(3));
            turns = turns.saturating_add(1);
        }
        let told = exited(&s);
        let writing = matches!(fuxix::process::ended(writer), Ok(None));
        let status = output(&mut s, "%2 1 until-ended str exited get");
        let screen = |s: &Session| s.pane(id).map(|p| p.version(s.run).counts.screen);
        let at_exit = screen(&s)?;
        // Its terminal is read on, a turn at a time, others served between:
        // its screen's revision grows.
        for _ in 0..20 {
            s.fire_now(crate::session::Timer::Unread(id));
            run(&mut s, "%1 str x text send-keys")?;
        }
        let read_on = s.leftover.contains_key(&id) && screen(&s)? > at_exit;
        // Once the writing stops, its terminal drained, the pane is frozen.
        let _ = fuxix::process::kill_group(writer, fuxix::process::Signal::Kill);
        let mut drained = 0u32;
        while s.leftover.contains_key(&id) && drained < 10_000 {
            s.fire_now(crate::session::Timer::Unread(id));
            drained = drained.saturating_add(1);
        }
        let drained_at = screen(&s)?;
        for _ in 0..20 {
            s.fire_now(crate::session::Timer::Unread(id));
        }
        let frozen = !s.leftover.contains_key(&id) && screen(&s)? == drained_at;
        if let Some(child) = s.leftover.remove(&id) {
            s.dying.push(Dying {
                child,
                deadline: Instant::now(),
            });
        }
        reap(&mut s);
        assert!(told, "not told exited in {turns} turns");
        assert!(
            writing,
            "the writer it left had stopped as the exit was told"
        );
        assert_eq!(status?.trim(), "3");
        assert!(read_on, "its terminal no longer read while written on");
        assert!(
            frozen,
            "still read in {drained} turns once the writing stopped"
        );
        Ok(())
    }

    #[test]
    fn an_empty_read_at_the_exit_leaves_the_next_turn_to_say_it_is_drained() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/true program new-tab",
        )?;
        let id = PaneId::of(2);
        // Its terminal empty as the exit is told: the server's own read
        // may just have emptied it.
        let argv = ["/bin/sh", "-c", "trap '' HUP; exec sleep 60"].map(str::to_owned);
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        let pid = child.leader.pid();
        (s.parts.pane_mut(id).ok_or("the pane")?).set_process(Process::Reading(child));
        s.end_program(id, Ended::Exited(0));
        let told = s.pane(id)?.process().exit_status().is_some();
        let read_on = s.leftover.contains_key(&id);
        s.fire_now(Timer::Unread(id));
        let frozen = !s.leftover.contains_key(&id);
        let _ = fuxix::process::kill_group(pid, fuxix::process::Signal::Kill);
        if let Some(child) = s.leftover.remove(&id) {
            s.dying.push(Dying {
                child,
                deadline: Instant::now(),
            });
        }
        reap(&mut s);
        assert!(told, "the exit not told");
        assert!(read_on, "drained by the read at the exit");
        assert!(frozen, "not drained by the next turn's empty read");
        Ok(())
    }

    #[test]
    fn keys_for_several_panes_go_to_all_or_none() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "%1 str horizontal axis str /bin/cat program split drop",
        )?;
        // Its program has ended, and the pane stays: nothing takes input there.
        s.end_program(PaneId::of(2), fux_wire::answer::Ended::Exited(0));
        let sent = run(&mut s, "@1 str x text send-keys");
        assert!(sent.is_err(), "refused");
        let first = s.parts.panes().get(&PaneId::of(1)).ok_or("no %1")?;
        assert_eq!(first.input().len(), 0, "the first pane got none");
        Ok(())
    }

    #[test]
    fn keys_at_a_version_go_only_while_the_pane_is_at_it() -> TestResult {
        let mut s = started()?;
        let queued =
            |s: &mut Session| (s.parts.pane_mut(PaneId::of(1))).map(|mut p| p.drain_input());
        run(&mut s, "%1 capture expect str x text send-keys")?;
        assert_eq!(queued(&mut s), Some(b"x".to_vec()));
        let seen = s.pane(PaneId::of(1))?.version(s.run);
        s.out(PaneId::of(1), b"E");
        let stale = output(&mut s, &format!("str {seen} expect str x text send-keys"));
        let stale = stale.err().unwrap_or_default();
        assert!(
            stale.starts_with("stale: ") && stale.contains("%1 changed since"),
            "{stale}"
        );
        assert_eq!(queued(&mut s), Some(Vec::new()), "nothing sent");
        // A program's end is a change too: stale, not `not_running`.
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        let seen = s.pane(PaneId::of(2))?.version(s.run);
        s.end_program(PaneId::of(2), fux_wire::answer::Ended::Exited(0));
        let ended = output(&mut s, &format!("str {seen} expect str x text send-keys"));
        let ended = ended.err().unwrap_or_default();
        assert!(ended.starts_with("stale: "), "{ended}");
        Ok(())
    }

    #[test]
    fn history_budget_only_changes_its_target() -> TestResult {
        let (mut s, _) = attached(5, 20)?;
        s.out(
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
        run(&mut s, "%1 str horizontal axis 4096 history-bytes split")?;
        let other = s.pane(PaneId::of(2))?.history_bytes();
        run(&mut s, "str history-bytes 0 set")?;
        assert!(history(&s) > Some(2));
        run(&mut s, "%1 112 history-budget")?;
        assert_eq!(s.pane(PaneId::of(2))?.history_bytes(), other);
        assert_eq!(history(&s), Some(2));
        Ok(())
    }

    #[test]
    fn a_programs_clipboard_write_reaches_the_terminals() -> TestResult {
        let (mut s, client) = attached(5, 20)?;
        s.outbox.clear();
        s.out(PaneId::of(1), b"\x1b]52;c;Zm9v\x07");
        let copies = |s: &Session| {
            (s.outbox.iter())
                .filter_map(|o| match o {
                    Outgoing::To(c, ToAttached::Paint(b))
                        if *c == client && b.starts_with(b"\x1b]52;c;") =>
                    {
                        Some(b.to_vec())
                    }
                    Outgoing::To(..) | Outgoing::Shutdown(..) => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(copies(&s), [b"\x1b]52;c;Zm9v\x07"]);
        s.outbox.clear();
        // One longer than fux-vt keeps of a string copies nothing, said.
        let long = format!(
            "\x1b]52;c;{}\x07",
            std::iter::repeat_n('A', fux_vt::OSC_PAYLOAD_LIMIT).collect::<String>()
        );
        s.out(PaneId::of(1), long.as_bytes());
        // Read a slice a turn, as the server does before it reads more.
        while s.pane(PaneId::of(1))?.has_unread() {
            s.fire_now(crate::session::Timer::Unread(PaneId::of(1)));
        }
        let notice = s.views.get(&client).and_then(|v| v.notice.clone());
        assert!(notice.is_some_and(|n| n.error && n.text.contains("copies (OSC 52) not made (1)")));
        assert!(copies(&s).is_empty());
        // In one read: each copy made, in order, and the refusal between them told.
        if let Some(view) = s.views.get_mut(&client) {
            view.notice = None;
        }
        s.out(
            PaneId::of(1),
            b"\x1b]52;c;YQ==\x07\x1b]52;c;***\x07\x1b]52;c;Yg==\x07",
        );
        assert_eq!(copies(&s), [b"\x1b]52;c;YQ==\x07", b"\x1b]52;c;Yg==\x07"]);
        let notice = s.views.get(&client).and_then(|v| v.notice.clone());
        assert!(notice.is_some_and(|n| n.text.contains("is not base64")));
        // One in a tab no client shows is told once one does.
        run(&mut s, "opts 20 4 size str other in new-tab")?;
        run(&mut s, "str c1 client str other workspace show")?;
        if let Some(view) = s.views.get_mut(&client) {
            view.notice = None;
        }
        s.out(PaneId::of(1), b"\x1b]52;c;***\x07\x1b]52;c;***\x07");
        assert!(s.views.get(&client).is_some_and(|v| v.notice.is_none()));
        run(&mut s, "str c1 client str +1 workspace show")?;
        let notice = s.views.get(&client).and_then(|v| v.notice.clone());
        assert!(notice.is_some_and(|n| n.text.contains("%1: copies (OSC 52) not made (2)")));
        // Counted in its JSON too, for a reader no notice reaches.
        let json = output(&mut s, "%1 capture json")?;
        assert!(
            json.contains(r#""copies_cut":1,"copies_not_text":3,"#),
            "{json}"
        );
        s.outbox.clear();
        run(
            &mut s,
            "%1 str horizontal axis false program-clipboard split",
        )?;
        s.outbox.clear();
        s.out(PaneId::of(3), b"\x1b]52;c;YmFy\x07");
        assert!(s.outbox.is_empty());
        Ok(())
    }

    #[test]
    fn invalid_presentation_data_discards_the_entire_local_draft() -> TestResult {
        let (mut session, client) = attached(8, 80)?;
        let before = session
            .views
            .get(&client)
            .ok_or("view")?
            .presentation
            .clone();
        let too_many = std::iter::repeat_n("str workspace ", 17).collect::<String>();
        let cases = [
            "str styles list list str typo 0 1 end end set".to_owned(),
            "str styles list list str bar 0 1 end list str bar 2 3 end end set".to_owned(),
            "str styles list list str bar str '#12345g' 1 end end set".to_owned(),
            "str styles list list str bar 256 1 end end set".to_owned(),
            "str styles list list str bar 0 1 str bold str bold end end set".to_owned(),
            "str bar list str bottom list str unknown end list end false end set".to_owned(),
            "str bar list str bottom list str pane end list str pane end false end set".to_owned(),
            format!("str bar list str bottom list {too_many} end list end false end set"),
            "str bar list str top list list str slot 16 end end list end false end set".to_owned(),
            "str bar-slots list list 0 str one end list 0 str two end end set".to_owned(),
            "str bar-slots list list 0 10 char end end set".to_owned(),
        ];
        for invalid in cases {
            // A valid preceding change is discarded with the refused draft.
            let code = format!("str show-column false set {invalid}");
            assert!(
                output_as(&mut session, Some(client), &code).is_err(),
                "{code}"
            );
            assert_eq!(
                session.views.get(&client).map(|v| &v.presentation),
                Some(&before),
                "{code}"
            );
        }
        Ok(())
    }

    #[test]
    fn attach_supplies_presentation_before_any_program_output() -> TestResult {
        let mut s = started()?;
        let presentation = fux_wire::presentation::Presentation {
            bell: true,
            titles: true,
            terminal_clipboard: true,
            cell_placement: false,
            ..fux::script::default_state()?.preferences.presentation()
        };
        let client = s.attach_client(Attach::of(sized(8, 40)?, presentation.clone()))?;
        assert_eq!(
            s.views.get(&client).map(|v| v.presentation.clone()),
            Some(presentation)
        );
        s.outbox.clear();
        s.out(PaneId::of(1), b"\x1b]2;initial\x07\x07\x1b]52;c;Zm9v\x07");
        assert!(
            s.outbox
                .iter()
                .any(|out| matches!(out, Outgoing::To(c, ToAttached::Paint(b)) if *c == client && b.starts_with(b"\x1b]52;c;")))
        );
        assert!(s.outbox.iter().any(|out| matches!(out, Outgoing::To(c, ToAttached::Paint(bytes)) if *c == client && bytes.as_ref() == crate::outer::BELL)));
        assert_eq!(s.before_paint(client), b"\x1b]2;initial\x1b\\");
        Ok(())
    }

    #[test]
    fn in_process_clients_keep_their_own_presentation() -> TestResult {
        let (mut s, a) = attached(8, 40)?;
        let b = s.attach(8, 40, None)?;
        output_as(
            &mut s,
            Some(a),
            "str titles true set str bell false set str terminal-clipboard false set",
        )?;
        output_as(&mut s, Some(b), "me focused zoom")?;
        output_as(&mut s, Some(a), "me focused zoom")?;
        for (client, titles, bell, terminal_clipboard) in
            [(a, true, false, false), (b, false, true, true)]
        {
            assert_eq!(
                s.views.get(&client).map(|v| v.presentation.clone()),
                Some(fux_wire::presentation::Presentation {
                    titles,
                    bell,
                    terminal_clipboard,
                    cell_placement: false,
                    ..fux::script::default_state()?.preferences.presentation()
                })
            );
        }
        Ok(())
    }

    #[test]
    fn in_process_commits_publish_presentation_at_each_checkpoint() -> TestResult {
        use fux::script::{AsClient, Asks, Host};
        let (mut s, client) = attached(8, 40)?;
        let mut state = fux::script::default_state()?;
        for (code, committed) in [
            ("str titles true set %999 capture", true),
            ("str titles false set str Nope prefix", false),
        ] {
            let mut draft = state.draft();
            let mut published = false;
            let mut publish = |next: &fux::client_state::ClientState| {
                published = true;
                state = next.clone();
            };
            let context = fux_wire::protocol::Context {
                client: Some(client),
                ..Default::default()
            };
            let mut process = InProcess(&mut s);
            let mut asks = AsClient {
                asks: &mut process,
                context: context.clone(),
                opened: None,
                on_commit: Some(&mut publish),
                stop: None,
            };
            let tokens = fuxth::token::source("commit", code)?;
            let mut machine = fux::script::compile(&tokens)?;
            let mut output = Vec::new();
            let mut host = Host::new(context.clone(), Some(&mut draft), &mut output);
            let result = fux::script::run(&mut machine, &mut host, &mut asks, None)
                .and_then(|_| asks.commit(&mut draft, context));
            assert!(result.is_err());
            assert_eq!(published, committed);
            assert!(state.preferences.titles);
            assert!(s.views.get(&client).is_some_and(|v| v.presentation.titles));
        }
        Ok(())
    }

    #[test]
    fn clipboard_permission_and_delivery_have_separate_owners() -> TestResult {
        use fux_wire::act::Act;
        let (mut s, a) = attached(8, 40)?;
        let b = s.attach(8, 40, None)?;
        let presentation = fux::script::default_state()?.preferences.presentation();
        s.apply(
            a,
            vec![Act::Presentation(fux_wire::presentation::Presentation {
                terminal_clipboard: false,
                cell_placement: false,
                ..presentation.clone()
            })],
        );
        run(
            &mut s,
            "%1 str horizontal axis false program-clipboard split",
        )?;
        run(&mut s, "%2 zoom")?;
        if let Some(view) = s.views.get_mut(&b) {
            view.read_only = true;
        }
        s.outbox.clear();
        s.out(PaneId::of(1), b"\x1b]52;c;Zm9v\x07");
        let copies = |s: &Session| {
            s.outbox
                .iter()
                .filter_map(|o| match o {
                    Outgoing::To(c, ToAttached::Paint(b)) if b.starts_with(b"\x1b]52;c;") => {
                        Some(*c)
                    }
                    Outgoing::To(..) | Outgoing::Shutdown(..) => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(copies(&s), [b]);
        s.outbox.clear();
        s.out(PaneId::of(2), b"\x1b]52;c;YmFy\x07");
        assert!(copies(&s).is_empty());
        // A copy goes only to a client whose terminal delivery is on.
        s.copied("manual", &[a]);
        assert!(copies(&s).is_empty());
        s.apply(a, vec![Act::Presentation(presentation)]);
        s.copied("mine", &[a]);
        assert_eq!(copies(&s), [a]);
        Ok(())
    }

    #[test]
    fn a_tab_no_client_shows_is_sized_as_on_a_clients_screen() -> TestResult {
        let (mut s, c) = attached(10, 40)?;
        output_as(
            &mut s,
            Some(c),
            "opts str +1 workspace in str / cwd new-tab drop",
        )?;
        run(&mut s, "str c1 client str @1 tab show")?;
        let size = |s: &Session, p| s.parts.panes().get(&PaneId::of(p)).map(Pane::size);
        assert_eq!(size(&s, 2), Some(sized(10, 40)?));
        run(&mut s, "str %2 pane str vertical axis str / cwd split")?;
        let (top, bottom) = (size(&s, 2), size(&s, 3));
        let rows = top
            .zip(bottom)
            .map(|(t, b)| (t.rows(), b.rows(), t.cols(), b.cols()));
        assert_eq!(rows, Some((5, 4, 40, 40)), "stacked, a separator between");
        // Leaving a tab whose layout is unchanged resizes nothing: a zoomed
        // pane keeps the whole screen.
        run(&mut s, "str c1 client str @2 tab show")?;
        run(&mut s, "%2 zoom")?;
        run(&mut s, "str c1 client str @1 tab show")?;
        assert_eq!(size(&s, 2), Some(sized(10, 40)?));
        // Nor does a swap or a move in another tab.
        run(
            &mut s,
            "%1 str horizontal axis str / cwd split drop %1 %4 swap-with %4 str new-tab move",
        )?;
        assert_eq!(size(&s, 2), Some(sized(10, 40)?));
        Ok(())
    }

    #[test]
    fn a_pane_is_made_only_at_a_size_known() -> TestResult {
        let run_id = fux_wire::command::Run::now().ok_or("no clock")?;
        let journal = fux::script::default_state()?.preferences.journal_entries;
        let mut s = Session::new("/nonexistent/fux.sock".into(), false, run_id, journal);
        s.test_keymap = fux::script::default_state()?.draft();
        let refused = run(&mut s, "opts str new-workspace in new-tab")
            .err()
            .unwrap_or_default();
        assert!(
            refused.starts_with("size: ") && refused.contains("give a size"),
            "{refused}"
        );
        assert!(s.parts.panes().is_empty());
        // A client of a size no view has starts nothing, nor does one that
        // only watches: there is nothing to watch.
        assert!(s.attach(u16::MAX, u16::MAX, None).is_err());
        assert!(watch(&mut s, 11, 40).is_err());
        assert!(s.parts.panes().is_empty());
        let size = |s: &Session, p| s.parts.panes().get(&PaneId::of(p)).map(|p| p.size().into());
        let c = s.attach(10, 40, None)?;
        assert_eq!(size(&s, 1), Some((10, 40)));
        // Nor from a caller with no screen or tab of its own, whatever
        // client is attached.
        let refused = run(&mut s, "opts str +1 workspace in new-tab").err();
        assert!(refused.is_some_and(|r| r.starts_with("size: ")));
        run(&mut s, "str %1 pane str vertical axis split")?;
        s.detach_client(c);
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

    #[test]
    fn ls_of_a_target_lists_what_is_in_it() -> TestResult {
        let (mut s, _) = attached(11, 40)?;
        run(&mut s, "opts 40 10 size str +1 workspace in new-tab")?;
        run(&mut s, "str %2 pane str vertical axis split")?;
        let mut ids = |only: AnyRef| -> Result<String, String> {
            let text = output(&mut s, &format!("{only} info listing"))?;
            let words = text.lines().filter_map(|l| l.split_whitespace().next());
            Ok(words.map(str::to_owned).collect::<Vec<_>>().join(" "))
        };
        let ws = "+1".parse::<WsId>()?;
        let tab = "@2".parse::<TabId>()?;
        assert_eq!(ids(AnyRef::Workspace(ws))?, "+1 @1 %1 @2 %2 %3");
        assert_eq!(ids(AnyRef::Tab(tab))?, "@2 %2 %3");
        assert_eq!(ids(AnyRef::Pane(PaneId::of(3)))?, "%3");
        Ok(())
    }

    #[test]
    fn the_json_help_names_every_pane_field() -> TestResult {
        let (s, _) = attached(4, 30)?;
        let pane = s.parts.panes().values().next().ok_or("no pane")?;
        for (name, _) in s.pane_json(pane)? {
            let named = include_str!("../../docs/json.md").contains(&format!("`{name}`"));
            assert!(named, "docs/json.md does not name a pane's {name:?}");
        }
        Ok(())
    }

    #[test]
    fn json_escapes_what_it_says() -> TestResult {
        let (mut s, _) = attached(4, 30)?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, r#"str %1 pane str 'say "hi" \ 界' rename"#)?;
        s.out(PaneId::of(2), "a\tb 界\x1b]2;tab\\title\x07".as_bytes());
        let ls = output(&mut s, "ls json")?;
        assert!(ls.contains(r#""name":"say \"hi\" \\ 界""#), "{ls}");
        assert!(ls.contains(r#""title":"tab\\title""#), "{ls}");
        assert!(ls.contains(r#""truncated":[]"#), "{ls}");
        let capture = output(&mut s, "str %2 pane capture json")?;
        assert!(capture.contains(r#""lines":["a       b 界"]"#), "{capture}");
        Ok(())
    }

    #[test]
    fn an_exit_is_told_even_when_it_closes_the_tab() -> TestResult {
        let (mut s, client) = attached(10, 40)?;
        let notice = |s: &Session| {
            s.views
                .get(&client)
                .and_then(|v| v.notice.as_ref())
                .map(|n| n.text.clone())
        };
        // Two panes: the tab stays.
        run(&mut s, "str %1 pane str horizontal axis split")?;
        s.end_program(PaneId::of(2), fux_wire::answer::Ended::Exited(3));
        assert_eq!(notice(&s).as_deref(), Some("%2 exited with status 3"));
        // A second tab, shown; its only pane exits, and the tab with it.
        run(&mut s, "opts 40 9 size str +1 workspace in new-tab")?;
        run(&mut s, "str c1 client str @2 tab show")?;
        s.end_program(PaneId::of(3), fux_wire::answer::Ended::Exited(7));
        assert_eq!(notice(&s).as_deref(), Some("%3 exited with status 7"));
        Ok(())
    }

    #[test]
    fn a_pane_moved_beside_another_stays_out_of_empty_tabs() -> TestResult {
        let (mut s, _) = attached(10, 40)?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, "opts 40 9 size str +1 workspace in new-tab")?;
        run(&mut s, "str %3 pane str @1 tab move")?;
        run(&mut s, "str %1 pane str right move")?;
        let holding = |s: &Session, pane| {
            let tabs = s.parts.workspaces().iter().flat_map(|w| w.tabs());
            tabs.filter(|t| t.holds(pane)).count()
        };
        assert_eq!(holding(&s, PaneId::of(1)), 1);
        run(&mut s, "str %1 pane kill")?;
        assert_eq!(holding(&s, PaneId::of(1)), 0);
        Ok(())
    }

    #[test]
    fn a_wait_begun_answers_the_change_and_the_close_after_it() -> TestResult {
        let mut s = started()?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        let origin = Origin::Cli(Caller::default());
        let timeout = Some(command::Limit::read(30)?);
        let wait = |target| {
            Request::Wait(command::Wait {
                since: command::Since::Now(target),
                timeout,
            })
        };
        let ws = s
            .parts
            .workspaces()
            .first()
            .map(|w| w.id())
            .ok_or("a workspace")?;
        let Begun::Waiting(mut waiting) = s.begin_now(wait(AnyRef::Workspace(ws)), &origin) else {
            return Err("the workspace's wait answered at once".into());
        };
        assert!(waiting.answer(&s, Instant::now()).is_none());
        s.out(PaneId::of(2), b"E");
        let answer = waiting.answer(&s, Instant::now()).ok_or("no answer")?;
        let text = said(&Ok(answer));
        assert!(text.starts_with("%2:") && !text.contains("%1"), "{text}");
        let Begun::Waiting(mut waiting) = s.begin_now(wait(AnyRef::Pane(PaneId::of(1))), &origin)
        else {
            return Err("the pane's wait answered at once".into());
        };
        run(&mut s, "str %1 pane kill")?;
        let answer = waiting.answer(&s, Instant::now()).ok_or("no answer")?;
        assert_eq!(answer.end().code(), Some(Code::Closed), "{answer:?}");
        Ok(())
    }

    #[test]
    fn a_failed_wait_keeps_its_answer() -> TestResult {
        let mut s = started()?;
        let caught = "str timeout try %1 1 timeout wait catch str value get end";
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

    #[test]
    fn counts_use_the_pane_version() -> TestResult {
        let (mut s, client) = attached(24, 80)?;
        let shown = "%2:0.57.1.3.0:48211-1867a3f9c2b8e7d6";
        s.out(PaneId::of(1), shown.as_bytes());
        let capture = format!("str {client} client capture");
        let text = output(&mut s, &format!("{capture} string"))?;
        assert_eq!(text.split_whitespace().next(), Some(shown));
        for source in [
            format!("{capture} counts"),
            format!("list {capture} end first counts"),
        ] {
            let refused = output(&mut s, &source)
                .err()
                .ok_or("client capture text supplied invented counts")?;
            assert!(refused.starts_with("type: "), "{refused}");
        }
        assert_eq!(
            output(&mut s, &format!("str {shown} counts json"))?.trim(),
            "[0,57,1,3,0]"
        );
        let counts = s.pane(PaneId::of(1))?.counts();
        let expected = format!(
            "[{},{},{},{},{}]",
            counts.respawns,
            counts.screen,
            counts.bells,
            counts.done,
            u8::from(counts.ended)
        );
        let baseline = output(&mut s, "%1 version")?;
        for source in [
            "%1 counts json",
            "%1 capture counts json",
            "%1 str x text send-keys counts json",
            "%1 capture let sample %1 kill sample counts json",
        ] {
            assert_eq!(output(&mut s, source)?.trim(), expected, "{source}");
        }
        let closed = format!(
            "str closed try str {} changed wait catch str value get end counts",
            baseline.trim()
        );
        let refused = output(&mut s, &closed)
            .err()
            .ok_or("closed answer supplied invented counts")?;
        assert!(refused.starts_with("type: "), "{refused}");
        Ok(())
    }

    #[test]
    fn capture_and_quiet_keep_the_pane_recipe() -> TestResult {
        let mut s = started()?;
        let json = output(
            &mut s,
            "%1 capture expect str x text send-keys let sent \
             str timeout try sent 0 timeout wait catch str value get end \
             0 quiet json",
        )?;
        assert!(
            json.contains(r#""pane":"%1""#) && json.contains(r#""version":"%1:"#),
            "{json}"
        );
        Ok(())
    }

    #[test]
    fn a_closed_panes_answer_names_its_pane() -> TestResult {
        let mut s = started()?;
        let version = output(&mut s, "%1 str vertical axis split")?;
        run(&mut s, "%2 kill")?;
        let caught = format!(
            "str closed try str {} changed wait catch str value get end",
            version.trim()
        );
        assert_eq!(
            output(&mut s, &format!("{caught} json"))?.trim(),
            r#"{"pane":"%2","status":4}"#
        );
        assert_eq!(
            output(&mut s, &format!("{caught} kind"))?.trim(),
            "closed-answer"
        );
        let gone = output(&mut s, &format!("{caught} capture")).err();
        assert!(
            gone.as_ref().is_some_and(|e| e.starts_with("no_pane: ")),
            "{gone:?}"
        );
        Ok(())
    }

    #[test]
    fn a_version_names_its_pane_and_its_server_run() -> TestResult {
        let mut s = started()?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        let failed = |s: &mut Session, code: &str| output(s, code).err().unwrap_or_default();
        let of = |s: &Session, pane| s.pane(PaneId::of(pane)).map(|p| p.version(s.run));
        let (one, two) = (of(&s, 1)?, of(&s, 2)?);
        s.out(PaneId::of(2), b"E");
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
        s.out(PaneId::of(1), &frame);
        let wait = Request::Wait(command::Wait {
            since: command::Since::Version(since),
            timeout: Some(command::Limit::read(0)?),
        });
        let Begun::Waiting(mut waiting) = s.begin_now(wait, &Origin::Cli(Caller::default())) else {
            return Err("a wait mid-frame answered at once".into());
        };
        let due = waiting.due().ok_or("a wait with no time")?;
        while s.pane(PaneId::of(1))?.mid_frame() {
            assert!(waiting.answer(&s, due).is_none());
            s.fire_now(Timer::Unread(PaneId::of(1)));
        }
        let answer = waiting.answer(&s, due).ok_or("no answer")?;
        let text = said(&Ok(answer));
        assert!(text.ends_with(" screen"), "{text}");
        Ok(())
    }

    #[test]
    fn workspace_names_stay_unique_and_failures_use_no_ids() -> TestResult {
        let mut s = started()?;
        assert!(run(&mut s, "opts 80 24 size str +9 in new-tab").is_err());
        assert_eq!(
            output(&mut s, "opts 80 24 size str workspace-3 in new-tab")?,
            format!("%2:0.0.0.0.0:{}\n", s.run)
        );
        let taken = "opts 80 24 size str workspace-3 in str workspace-3 name new-tab";
        assert!(run(&mut s, taken).is_err());
        let third = format!("%3:0.0.0.0.0:{}\n", s.run);
        assert_eq!(
            output(&mut s, "opts str new-workspace in 80 24 size new-tab")?,
            third
        );
        let names: Vec<Option<&str>> = s.parts.workspaces().iter().map(|w| w.name()).collect();
        assert_eq!(names, [None, Some("workspace-3"), None]);
        // A move to a new workspace with no tab ID left takes no workspace
        // ID either.
        s.ids.exhaust_tabs();
        let ids = s.ids.clone();
        assert!(run(&mut s, "str %1 pane str new-workspace move").is_err());
        assert_eq!(s.ids, ids);
        Ok(())
    }

    #[test]
    fn a_hidden_tabs_label_change_repaints_its_workspace() -> TestResult {
        let (mut s, client) = attached(10, 80)?;
        run(&mut s, "opts +1 in 80 24 size new-tab drop")?;
        let pane = PaneId::of(2);
        let clean = |s: &mut Session| {
            for view in s.views.values_mut() {
                view.dirty = false;
            }
        };
        let dirty = |s: &Session| s.views.get(&client).is_some_and(|view| view.dirty);
        clean(&mut s);
        s.out(pane, b"ordinary output");
        assert!(!dirty(&s), "ordinary hidden output");
        s.out(pane, b"\x1b]2;finished\x07");
        assert!(dirty(&s), "the hidden tab's label changed");
        clean(&mut s);
        s.out(pane, b"\x1b]2;finished\x07");
        assert!(!dirty(&s), "the label stayed the same");
        run(&mut s, "@2 str given rename")?;
        clean(&mut s);
        s.out(pane, b"\x1b]2;another title\x07");
        assert!(!dirty(&s), "a given tab name masks its pane's title");
        run(
            &mut s,
            "opts +1 in 80 24 size new-tab drop %3 str given-pane rename",
        )?;
        clean(&mut s);
        s.out(PaneId::of(3), b"\x1b]2;another title\x07");
        assert!(!dirty(&s), "a given pane name masks its title");
        Ok(())
    }

    #[test]
    fn only_screens_that_may_change_are_marked_for_painting() -> TestResult {
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
        s.out(fux_wire::id::PaneId::of(1), b"ls");
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
        run(&mut s, "str %1 pane str horizontal axis split")?;
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

    #[test]
    fn only_a_size_that_changes_a_client_resizes_it() -> TestResult {
        let (mut s, c) = attached(10, 40)?;
        for (rows, changes) in [
            (10, Some(false)),
            (4096, Some(true)),
            (4096, Some(false)),
            (0, None),
            (3, Some(true)),
        ] {
            if let Some(view) = s.views.get_mut(&c) {
                view.dirty = false;
            }
            let resized = s.size_client(c, rows, 40).ok();
            let dirty = s.views.get(&c).is_some_and(|v| v.dirty);
            assert_eq!(
                (resized, dirty),
                (changes, changes == Some(true)),
                "{rows} rows"
            );
        }
        assert!(s.attach(u16::MAX, u16::MAX, None).is_err());
        Ok(())
    }

    #[test]
    fn what_cannot_be_done_is_refused() -> TestResult {
        let (mut s, _) = attached(10, 40)?;
        for code in [
            "opts str %3 in new-tab",
            "str +1 workspace str +2 rename",
            "str +1 workspace str @2 rename",
            "str %1 pane str '' rename",
            "str %1 pane str up 1 move-border",
            "str c1 client dup focused str last step",
            "str @1 tab str next reorder",
        ] {
            assert!(run(&mut s, code).is_err(), "{code}");
        }
        Ok(())
    }

    #[test]
    fn a_screen_asked_from_elsewhere_is_the_clients() -> TestResult {
        let (mut s, c) = attached(10, 40)?;
        run(
            &mut s,
            "define sender-only 2drop end str c1 client str title list list 1 str inert end end list end list 0 end code sender-only end choose",
        )?;
        let notice = s
            .views
            .get(&c)
            .and_then(|v| v.notice.as_ref())
            .map(|n| n.text.as_str())
            .unwrap_or_default();
        assert!(notice.contains("sender-only: no such word"), "{notice}");
        assert!(matches!(
            s.views.get(&c).map(|v| &v.mode),
            Some(Mode::Normal)
        ));
        run(&mut s, "str c1 client dup focused tab choose-among")?;
        assert!(matches!(
            s.views.get(&c).map(|v| &v.mode),
            Some(Mode::Panel(fux_wire::act::Panel::List(_)))
        ));
        Ok(())
    }

    #[test]
    fn only_given_names_are_resolved() -> TestResult {
        let (mut s, _) = attached(10, 40)?;
        assert_eq!(
            output(&mut s, "%1 str name get @1 str name get +1 str name get")?,
            "null\nnull\nnull\n"
        );
        for lookup in [
            "str sh pane",
            "str main tab",
            "str main workspace",
            "str workspace-1 workspace",
        ] {
            assert!(output(&mut s, lookup).is_err(), "{lookup}");
        }
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab str web rename",
        )?;
        assert_eq!(
            output(&mut s, "str web pane dup tab str name get")?,
            "%2\nnull\n"
        );
        run(&mut s, "%2 str horizontal axis split drop")?;
        assert_eq!(output(&mut s, "str web pane")?, "%2\n");
        s.out(PaneId::of(2), b"\x1b]2;spool\x07");
        assert!(output(&mut s, "str spool pane").is_err());
        assert!(output(&mut s, "str cat pane").is_err());
        assert_eq!(output(&mut s, "str %1")?, "%1\n");
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.starts_with(r#"{"pane":"%1""#), "{json}");
        Ok(())
    }

    #[test]
    fn lookups_do_not_depend_on_layout_shape() -> TestResult {
        let mut s = started()?;
        for split in [false, true] {
            if split {
                run(&mut s, "%1 str horizontal axis split drop")?;
            }
            for lookup in ["@1 pane", "+1 pane", "+1 tab", "@1 str rows get"] {
                assert!(output(&mut s, lookup).is_err(), "{lookup}");
            }
            assert_eq!(output(&mut s, "%1 tab workspace")?, "+1\n");
        }
        Ok(())
    }

    #[test]
    fn split_requires_an_explicit_axis() -> TestResult {
        let (mut s, _) = attached(10, 40)?;
        let ids = s.ids.clone();
        for code in ["%1 split", "%1 str diagonal axis split"] {
            assert!(output(&mut s, code).is_err(), "{code}");
            assert_eq!(s.ids, ids);
        }
        run(&mut s, "%1 str vertical axis split drop")?;
        assert_eq!(s.parts.panes().len(), 2);
        Ok(())
    }

    #[test]
    fn scalar_request_slots_are_given_once() -> TestResult {
        let (mut s, _) = attached(10, 40)?;
        let ids = s.ids.clone();
        for (base, first, second, action) in [
            ("opts", "str a name", "str b name", "new-tab"),
            ("opts", "40 10 size", "40 10 size", "new-tab"),
            ("opts", "str / cwd", "str / cwd", "new-tab"),
            ("opts", "str cat program", "str sh program", "new-tab"),
            (
                "opts",
                "str cat program",
                "list str sh end shell-program",
                "new-tab",
            ),
            (
                "opts",
                "list str sh end shell-program",
                "str cat program",
                "new-tab",
            ),
            ("opts", "0 history-bytes", "1 history-bytes", "new-tab"),
            ("opts", "false reflow", "true reflow", "new-tab"),
            (
                "opts",
                "0 alternate-scroll-lines",
                "1 alternate-scroll-lines",
                "new-tab",
            ),
            ("opts", "0 picture-bytes", "1 picture-bytes", "new-tab"),
            (
                "opts",
                "false program-clipboard",
                "true program-clipboard",
                "new-tab",
            ),
            ("opts", "watch", "watch", "attach"),
            ("%1", "str horizontal axis", "str vertical axis", "split"),
            ("%1", "1 2 shares", "2 1 shares", "split"),
            ("%1", "kill-first", "kill-first", "respawn"),
            ("%1", "0 timeout", "1 timeout", "wait"),
            ("%1", "0 history", "all-history", "capture"),
            ("%1", "all-history", "0 history", "capture"),
            ("%1", "0 1 rows", "0 2 rows", "capture"),
            ("%1", "0 1 cols", "0 2 cols", "capture"),
        ] {
            let code = format!("{base} {first} {second} {action}");
            let error = output(&mut s, &code)
                .err()
                .ok_or_else(|| format!("ran {code}"))?;
            assert!(error.starts_with("unused: "), "{code}: {error}");
            if second == "all-history" {
                assert!(
                    error.contains("inside all-history") && error.contains("is already given"),
                    "{error}"
                );
            }
            assert_eq!(s.ids, ids);
        }
        for first in [
            "wrapped",
            "full",
            "styles",
            "marks",
            "main-screen",
            "alt-screen",
            "last-output",
        ] {
            let seconds: &[&str] = if matches!(first, "main-screen" | "alt-screen" | "last-output")
            {
                &["main-screen", "alt-screen", "last-output"]
            } else {
                &[first]
            };
            for second in seconds {
                let code = format!("%1 {first} {second} capture");
                let error = output(&mut s, &code)
                    .err()
                    .ok_or_else(|| format!("ran {code}"))?;
                assert!(error.starts_with("unused: "), "{code}: {error}");
            }
        }
        Ok(())
    }

    #[test]
    fn versions_keep_the_observation_they_name() -> TestResult {
        let (mut s, _) = attached(10, 40)?;
        let initial = s.pane(PaneId::of(1))?.version(s.run).to_string();
        assert_eq!(output(&mut s, "%1 version")?, format!("{initial}\n"));
        assert_eq!(
            output(&mut s, &format!("str {initial} version"))?,
            format!("{initial}\n")
        );
        assert!(output(&mut s, "str 1.2.3 version").is_err());
        assert!(output(&mut s, "%1 str idle_ms get").is_err());
        assert!(!output(&mut s, "%1 capture json")?.contains("idle_ms"));
        let held = output(
            &mut s,
            "%1 str x text send-keys let action %1 capture let sample %1 kill action version sample version",
        )?;
        assert_eq!(held, format!("{initial}\n{initial}\n"));
        Ok(())
    }

    #[test]
    fn structural_and_context_recovery_codes_are_shared() -> TestResult {
        let (mut s, _) = attached(10, 40)?;
        for source in ["opts str x arg new-tab", "%1 send-keys"] {
            let caught = output(
                &mut s,
                &format!("str usage try {source} drop catch drop str recovered end"),
            )?;
            assert_eq!(caught, "recovered\n", "{source}");
        }
        assert_eq!(
            output(
                &mut s,
                "str not_here try me drop catch drop str recovered end"
            )?,
            "recovered\n"
        );
        Ok(())
    }

    #[test]
    fn new_tab_follows_sizes_and_lives_as_its_caller_asks() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        let viewport = s.pane(PaneId::of(1))?.size();
        let viewport = (viewport.cols(), viewport.rows());
        // From a key or prompt it follows, on its client's screen; from the
        // command line no client moves, and it is laid out on its size.
        for (from, source, follows, size, program) in [
            (Some(c), "opts str / cwd new-tab", true, viewport, false),
            (
                Some(c),
                "opts 100 30 size str / cwd str cat program new-tab",
                true,
                viewport,
                true,
            ),
            (
                None,
                "opts str agent in 100 30 size str / cwd new-tab",
                false,
                (100, 30),
                false,
            ),
            (
                None,
                "opts str agent in 100 30 size str / cwd str cat program new-tab",
                false,
                (100, 30),
                true,
            ),
        ] {
            let shown = s.focused(c);
            output_as(&mut s, from, &format!("{source} drop"))?;
            let made = *s.parts.panes().keys().max().ok_or("a pane")?;
            let pane = s.pane(made)?;
            assert_eq!((pane.size().cols(), pane.size().rows()), size, "{source}");
            assert_eq!(matches!(pane.runs, Runs::Explicit(_)), program, "{source}");
            let now = s.focused(c);
            assert_eq!(now, if follows { Some(made) } else { shown }, "{source}");
        }
        // Refused before anything starts: no workspace or size from the
        // command line, and a name another tab or a workspace has.
        output(
            &mut s,
            "opts str agent in 80 24 size str web name str cat program new-tab drop",
        )?;
        for (source, code) in [
            ("opts 80 24 size new-tab", "no_target:"),
            ("opts str agent in new-tab", "size:"),
            (
                "opts str agent in 80 24 size str web name new-tab",
                "name_taken:",
            ),
            (
                "opts str agent in 80 24 size str agent name new-tab",
                "name_taken:",
            ),
        ] {
            let ids = s.ids.clone();
            let refused = output(&mut s, source).err().ok_or(source)?;
            assert!(refused.starts_with(code), "{source}: {refused}");
            assert_eq!(s.ids, ids, "{source}");
        }
        // The name is the tab's; its pane's is given by a rename of its own.
        assert_eq!(output(&mut s, "str web tab panes")?, "%6\n");
        assert!(output(&mut s, "str web pane").is_err());
        run(&mut s, "str web tab panes 0 at str ed rename")?;
        assert_eq!(output(&mut s, "str ed pane str web tab")?, "%6\n@6\n");
        Ok(())
    }

    #[test]
    fn names_are_not_reassigned_to_other_tabs() -> TestResult {
        let mut s = started()?;
        run(&mut s, "opts 80 24 size +1 in new-tab drop")?;
        run(&mut s, "@1 str tab-2 rename")?;
        assert_eq!(output(&mut s, "@2 str name get")?, "null\n");
        run(&mut s, "@1 str given rename @2 str tab-2 rename")?;
        assert_eq!(
            output(&mut s, "@1 str name get @2 str name get")?,
            "given\ntab-2\n"
        );
        assert!(s.dump_recipe(None)?.contains("str 'tab-2' name"));
        Ok(())
    }

    /// What a refused name leaves as it was: every workspace, tab and pane,
    /// their names and layout revisions, the journal's cursor, and the ids
    /// to come.
    fn named_state(s: &Session) -> Result<(Box<fux_wire::inspect::Snapshot>, Ids), String> {
        use fux_wire::inspect::{Projection, Scope};
        let topology = s.inspect(Scope::All, Projection::Topology);
        let Body::Snapshot(snapshot) = topology.map_err(|e| e.to_string())? else {
            return Err("a snapshot".into());
        };
        Ok((snapshot, s.ids.clone()))
    }

    /// Gap 6: two panes and their workspace were given one name, and `str
    /// NAME pane` then failed as ambiguous. A name names one thing.
    #[test]
    fn a_name_names_one_pane_tab_or_workspace() -> TestResult {
        let mut s = started()?;
        run(&mut s, "str %1 pane str horizontal axis split drop")?;
        run(&mut s, "str %1 pane str shared-name rename")?;
        for source in [
            "str %2 pane str shared-name rename",
            "str @1 tab str shared-name rename",
            "str +1 workspace str shared-name rename",
        ] {
            let refused = output(&mut s, source).err().ok_or(source)?;
            let taken = refused.starts_with("name_taken:") && refused.contains("%1, a pane,");
            assert!(taken, "{refused}");
        }
        assert_eq!(output(&mut s, "str shared-name pane")?, "%1\n");
        assert!(output(&mut s, "str shared-name tab").is_err());
        Ok(())
    }

    /// Every way a name is given refuses one a workspace, tab or pane
    /// already has, before anything changes: creating, renaming, and
    /// replaying a dump beside what it was dumped from, whether the name
    /// taken is a tab's, its first pane's or a split pane's.
    #[test]
    fn a_taken_name_is_refused_before_anything_changes() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "str +1 workspace str ws rename str @1 tab str tab rename str %1 pane str pane rename",
        )?;
        // @2: an unnamed tab, its first pane named; @3: only a split pane named.
        for (tab, split) in [("%2", "%3"), ("%4", "%5")] {
            run(&mut s, "opts 80 24 size +1 in new-tab drop")?;
            let split = format!("str {tab} pane str horizontal axis split drop str {split} pane");
            run(&mut s, &split)?;
        }
        run(
            &mut s,
            "str %2 pane str runner rename str %5 pane str split rename",
        )?;
        let dumps = [TabId::of(1), TabId::of(2), TabId::of(3)]
            .map(|tab| s.dump_recipe(Some(AnyRef::Tab(tab))));
        let [named, first, split] = dumps;
        let (named, first, split) = (named?, first?, split?);
        let before = named_state(&s)?;
        for (source, holder) in [
            ("opts 80 24 size str pane in new-tab", "%1, a pane,"),
            (
                "opts 80 24 size str ws in str pane name new-tab",
                "%1, a pane,",
            ),
            // A workspace `in` names that is not there is made by its name.
            ("opts 80 24 size str tab in new-tab", "@1, a tab,"),
            (
                "opts 80 24 size str fresh in str fresh name new-tab",
                "\"fresh\"",
            ),
            ("str %1 pane str ws rename", "+1, a workspace,"),
            ("str @1 tab str pane rename", "%1, a pane,"),
            ("str +1 workspace str tab rename", "@1, a tab,"),
            (&named, "@1, a tab,"),
            (&first, "%2, a pane,"),
            (&split, "%5, a pane,"),
        ] {
            let refused = output(&mut s, source).err().ok_or(source)?;
            let taken = refused.starts_with("name_taken:") && refused.contains(holder);
            assert!(taken, "{source}: {refused}");
            assert_eq!(named_state(&s)?, before, "{source}");
        }
        // Its own name is still its own.
        run(
            &mut s,
            "str %1 pane str pane rename str @1 tab str tab rename",
        )?;
        Ok(())
    }

    /// The name a config gives the first workspace (`attach-defaults`) goes
    /// to an empty server, where nothing can have it; it is then taken.
    #[test]
    fn a_configs_first_workspace_name_is_taken() -> TestResult {
        let mut s = Session::new(
            "/nonexistent/fux.sock".into(),
            false,
            command::Run::now().ok_or("clock")?,
            fux::script::default_state()?.preferences.journal_entries,
        );
        let initial = command::InitialWorkspace {
            options: fux::script::default_state()?.preferences.pane_options(),
            name: Some("main".into()),
            start: command::Start::Shell(command::Program::new("/bin/sh".into(), Vec::new())?),
            cwd: std::env::current_dir()?,
        };
        s.start_now(sized(24, 80)?, &initial)?;
        let refused = output(&mut s, "str %1 pane str main rename").err();
        let taken =
            |why: &String| why.starts_with("name_taken:") && why.contains("+1, a workspace,");
        assert!(refused.is_some_and(|why| taken(&why)));
        Ok(())
    }

    #[test]
    fn a_captures_cursor_is_the_screens() -> TestResult {
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
        s.out(PaneId::of(1), b"0123456789\x1b[1;6H");
        assert_eq!(cursor(&mut s, "")?, "[0,5]");
        assert_eq!(cursor(&mut s, "2 7 cols")?, "[0,5]");
        assert_eq!(cursor(&mut s, "0 2 cols")?, "[0,5]");
        s.out(PaneId::of(1), b"\x1b[?25l");
        assert_eq!(cursor(&mut s, "")?, "null", "hidden");
        s.out(PaneId::of(1), b"\x1b[?25h\x1b[3;1H");
        assert_eq!(cursor(&mut s, "")?, "[2,0]", "on a blank row trimmed");
        s.out(
            PaneId::of(1),
            &[b"\x1b[2J\x1b[H".as_slice(), &[b'A'; 45], b"\r\nBC"].concat(),
        );
        assert_eq!(cursor(&mut s, "")?, "[2,2]");
        assert_eq!(cursor(&mut s, "wrapped")?, "[2,2]");
        s.out(PaneId::of(1), b"\x1b[?1049h\x1b[HALT");
        assert_eq!(cursor(&mut s, "alt-screen")?, "[0,3]");
        assert_eq!(cursor(&mut s, "main-screen")?, "null");
        s.out(PaneId::of(1), b"\x1b[?1049l");
        assert_eq!(cursor(&mut s, "alt-screen")?, "null");
        assert_eq!(cursor(&mut s, "main-screen")?, "[2,2]");
        // What is highlighted is marked in JSON's lines too.
        s.out(PaneId::of(1), b"\x1b[7mSEL\x1b[0m");
        let json = output(&mut s, "%1 marks capture json")?;
        assert!(json.contains("\"BC⟦SEL⟧\""), "{json}");
        let inverted = output(&mut s, "%1 5 2 rows capture");
        assert!(
            inverted.as_ref().is_err_and(|e| e.starts_with("range: ")),
            "{inverted:?}"
        );
        // A glyph in the last column leaves the cursor on it, the wrap
        // pending: said so.
        s.out(PaneId::of(1), &[b"\r\n".as_slice(), &[b'D'; 40]].concat());
        assert_eq!(cursor(&mut s, "wrapped")?, "[3,39]");
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.contains(r#""wrap_pending":true"#), "{json}");
        s.out(PaneId::of(1), b"\r");
        let json = output(&mut s, "%1 capture json")?;
        assert!(json.contains(r#""wrap_pending":false"#), "{json}");
        Ok(())
    }

    #[test]
    fn an_actions_answer_keeps_held_fields_and_targets_its_pane() -> TestResult {
        let (mut s, _) = attached(10, 40)?;
        let act = "%1 str x text send-keys";
        assert!(
            output(&mut s, &format!("{act} str name get"))
                .is_err_and(|e| e.starts_with("no_field:"))
        );
        assert_eq!(
            output(&mut s, &format!("{act} pane str name get"))?,
            "null\n"
        );
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

    #[test]
    fn failures_say_what_a_word_takes() -> TestResult {
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
        let mouse = failed(&mut s, "%1 str click -1 0 send-mouse");
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
        let caught = output(&mut s, "str type try 3 upper catch str message get end");
        assert_eq!(
            caught.as_deref(),
            Ok("each walks a string or a list, not an integer\n")
        );
        // A wait's limit is what the wire's 32 bits hold.
        let long = failed(&mut s, "%1 9223372036854775807 timeout wait");
        assert!(
            long.starts_with("range: ") && long.contains("at most 4294967295 s"),
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
        let split = failed(&mut s, "str 'two\nlines' fail");
        assert!(split.ends_with(r"two\nlines"), "{split}");
        let version = failed(&mut s, "str 1.2.3 changed wait");
        assert!(version.contains("\"1.2.3\" is no version"), "{version}");
        Ok(())
    }

    /// What a stream of events subscribed now is told of what `change`
    /// does to `s`.
    fn told(s: &mut Session, change: impl FnOnce(&mut Session)) -> String {
        let stream = crate::journal::Reader::new(
            fux_wire::inspect::Scope::All,
            s.parts.journal(),
            s.run,
            None,
        );
        // From now is never refused; if it were, the caller's comparison
        // shows why.
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(why) => return why,
        };
        change(s);
        stream.lines(s.parts.journal(), |t| s.holder(t)).0
    }

    #[test]
    fn a_rename_and_its_undo_are_both_told() -> TestResult {
        let mut s = started()?;
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
                r#"{"event":"renamed","workspace":"+1","parent":null,"name":"x"}"#,
                "\n",
                r#"{"event":"renamed","workspace":"+1","parent":null,"name":"main"}"#,
                "\n"
            )
        );
        Ok(())
    }

    #[test]
    fn a_tab_made_with_a_name_is_told_made_with_it() -> TestResult {
        let mut s = started()?;
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

    #[test]
    fn events_are_told_in_the_order_they_happened() -> TestResult {
        let mut s = started()?;
        run(&mut s, "%1 str horizontal axis split drop")?;
        let mut ran = Ok(());
        let lines = told(&mut s, |s| {
            s.out(PaneId::of(2), b"\x07");
            ran = run(s, "str @1 tab str t rename");
            s.out(PaneId::of(1), b"\x1b]2;title\x07");
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

    #[test]
    fn final_output_precedes_exit_close_and_sibling_resize() -> TestResult {
        for (ended, status, output) in [
            (Ended::Signalled(9), "\"signal\":9", false),
            (Ended::Exited(0), "\"exited\":0", true),
        ] {
            let mut s = started()?;
            run(&mut s, "%1 str horizontal axis split drop")?;
            let lines = told(&mut s, |s| {
                if output {
                    s.out(PaneId::of(2), b"\x1b[?2026h\x07\x1b]2;bye\x07");
                }
                s.end_program(PaneId::of(2), ended);
            });
            let prefix = if output {
                concat!(
                    r#"{"event":"bell","pane":"%2","tab":"@1","workspace":"+1"}"#,
                    "\n",
                    r#"{"event":"title","pane":"%2","tab":"@1","workspace":"+1","title":"bye"}"#,
                    "\n"
                )
            } else {
                ""
            };
            assert_eq!(
                lines,
                format!(
                    "{prefix}{{\"event\":\"exited\",\"pane\":\"%2\",\"tab\":\"@1\",\"workspace\":\"+1\",{status}}}\n{}\n{}\n",
                    r#"{"event":"closed","pane":"%2","tab":"@1","workspace":"+1"}"#,
                    r#"{"event":"resized","pane":"%1","tab":"@1","workspace":"+1","cols":80,"rows":24}"#
                )
            );
        }
        Ok(())
    }

    #[test]
    fn library_dump_replays_exact_shares_and_launch_inputs_in_its_scope() -> TestResult {
        let mut source = started()?;
        let value = "apostrophe' backslash\\\nstr INJECTED print\n# 雪";
        let directory = format!("/saved/{value}");
        run(
            &mut source,
            &format!(
                "opts str new-workspace in 150 40 size str {} cwd str /bin/cat program str '' arg str {} arg new-tab drop",
                fux_wire::words::quote(&directory),
                fux_wire::words::quote(value)
            ),
        )?;
        run(
            &mut source,
            "%2 str vertical axis 5 3 shares list str /bin/sh str -l str '' end shell-program split drop %2 str horizontal axis 7 2 shares str /bin/cat program split drop @2 str given rename %2 str sh rename",
        )?;
        let dump = output(&mut source, "@2 dump")?;
        let mut again = started()?;
        run(&mut again, &dump)?;
        let recipe = |s: &Session, id| {
            let Body::Snapshot(snapshot) = s
                .inspect(
                    fux_wire::inspect::Scope::Target(AnyRef::Tab(id)),
                    fux_wire::inspect::Projection::Recipe,
                )
                .map_err(|e| e.to_string())?
            else {
                return Err("not a recipe".to_owned());
            };
            Ok(snapshot)
        };
        let before = recipe(&source, TabId::of(2))?;
        let after = recipe(&again, TabId::of(2))?;
        assert_eq!(
            again.parts.workspaces().len(),
            2,
            "scoped dump exported another workspace"
        );
        assert_eq!(
            before.workspaces.first().and_then(|w| w.name.as_ref()),
            after.workspaces.first().and_then(|w| w.name.as_ref())
        );
        let (a, b) = (
            before.tabs.first().ok_or("tab")?,
            after.tabs.first().ok_or("replayed tab")?,
        );
        assert_eq!(
            (&a.name, a.fixed, a.cols, a.rows),
            (&b.name, b.fixed, b.cols, b.rows)
        );
        let shape = |nodes: &[fux_wire::layout::Node]| {
            nodes
                .iter()
                .map(|node| match node {
                    fux_wire::layout::Node::Pane(_) => fux_wire::layout::Node::Pane(PaneId::of(1)),
                    split @ fux_wire::layout::Node::Split { .. } => split.clone(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&a.nodes), shape(&b.nodes), "{dump}");
        let launches = |s: &fux_wire::inspect::Snapshot| {
            s.panes
                .iter()
                .map(|p| {
                    let state = p.state.as_ref();
                    let options = [
                        "history_bytes",
                        "program_clipboard",
                        "reflow",
                        "alternate_scroll_lines",
                        "picture_bytes",
                    ]
                    .map(|key| state.and_then(|s| s.get(key)).cloned());
                    (p.name.clone(), p.launch.clone(), p.cwd.clone(), options)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(launches(&before), launches(&after), "{dump}");
        Ok(())
    }

    #[test]
    fn a_dump_preserves_given_and_absent_names() -> TestResult {
        let mut source = started()?;
        run(
            &mut source,
            "+1 str workspace-2 rename @1 str main rename %1 str sh rename",
        )?;
        run(&mut source, "%1 str horizontal axis split drop")?;
        run(
            &mut source,
            "opts +1 in 80 24 size str /bin/cat program new-tab dup str cat rename str new-workspace move",
        )?;
        run(
            &mut source,
            "opts +2 in 80 24 size new-tab drop opts +2 in 80 24 size new-tab drop @3 kill opts +2 in 80 24 size new-tab str given rename",
        )?;
        run(&mut source, "str shell list str /bin/other end set")?;
        let dump = source.dump_recipe(None)?;
        assert!(!dump.contains("str other rename"), "{dump}");
        assert!(dump.contains("given"), "{dump}");
        let mut again = started()?;
        run(&mut again, &dump).map_err(|error| format!("{error}\n{dump}"))?;
        let names = |s: &Session| {
            s.parts
                .workspaces()
                .iter()
                .map(|ws| {
                    (
                        ws.name().map(str::to_owned),
                        ws.tabs()
                            .map(|tab| {
                                (
                                    tab.name().map(str::to_owned),
                                    tab.root()
                                        .panes()
                                        .iter()
                                        .filter_map(|id| s.parts.panes().get(id))
                                        .map(|pane| pane.name().map(str::to_owned))
                                        .collect::<Vec<_>>(),
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&again).get(1..),
            Some(names(&source).as_slice()),
            "{dump}"
        );
        Ok(())
    }

    #[test]
    fn a_dump_keeps_the_saved_shell_directory() -> TestResult {
        let s = started()?;
        let dump = s.dump_recipe(None)?;
        let cwd = std::env::current_dir()?;
        let cwd = cwd.to_str().ok_or("cwd text")?;
        let tokens = fuxth::token::source("dump", &dump)?;
        assert!(
            tokens
                .iter()
                .zip(tokens.iter().skip(1))
                .zip(tokens.iter().skip(2))
                .any(|((a, b), c)| a.text.as_ref() == "str"
                    && b.text.as_ref() == cwd
                    && c.text.as_ref() == "cwd"),
            "{dump}"
        );
        assert!(dump.contains("shell-program"), "{dump}");
        assert!(
            dump.contains("its current directory is unknown, so it starts in its launch directory"),
            "{dump}"
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_dump_leaves_out_a_directory_that_is_not_text() -> TestResult {
        use std::os::unix::ffi::OsStrExt;
        let dir = (fuxix::file::scratch()?.join(format!("fux-dump-{}", std::process::id())))
            .join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        std::fs::create_dir_all(&dir)?;
        let mut s = started()?;
        let program = Command::NewTab {
            target: vec![WsRef::Name("agent".into())],
            name: None,
            size: Some(fux_wire::command::Screen::new(24, 80)?),
            cwd: Some(dir.clone()),
            start: command::Start::Program(command::Program::new("cat".into(), Vec::new())?),
            options: fux::script::default_state()?.preferences.pane_options(),
        };
        s.execute(&program, &Origin::Cli(Caller::default()), Instant::now())?;
        let dump = s.dump_recipe(None)?;
        let _ = dir.parent().map(std::fs::remove_dir_all);
        let told = dump.contains("# %2: directory bytes (hex) ") && dump.contains("636166e9");
        let written = dump
            .lines()
            .any(|l| !l.starts_with('#') && l.contains("caf"));
        assert!(told && !written, "{dump}");
        let mut again = started()?;
        run(&mut again, &dump)?;
        assert_eq!(again.parts.panes().len(), 3, "{dump}");
        Ok(())
    }

    #[test]
    fn dumps_preserve_tab_sizing_independently_of_client_views() -> TestResult {
        let (mut s, c) = attached(11, 40)?;
        run(&mut s, "opts 600 40 size +1 in new-tab drop")?;
        run(
            &mut s,
            "%1 str horizontal axis split drop %1 str vertical axis split drop",
        )?;
        run(&mut s, "str c1 client %1 show %1 zoom")?;
        for tiny in [false, true] {
            if tiny {
                run(&mut s, "%1 zoom")?;
                s.size_client(c, 2, 2)?;
            }
            let dump = s.dump_recipe(None)?;
            let mut again = started()?;
            run(&mut again, &dump).map_err(|e| format!("{e}\n{dump}"))?;
            let tabs = again
                .parts
                .workspaces()
                .iter()
                .flat_map(|w| w.tabs())
                .collect::<Vec<_>>();
            let fits = tabs
                .iter()
                .map(|t| matches!(t.fit(), Fit::Clients(_)))
                .collect::<Vec<_>>();
            assert!(fits.ends_with(&[true, false]), "{dump}");
            assert_eq!(tabs.last().map(|t| t.fit().size()), Some(sized(40, 600)?));
            assert_eq!(again.parts.panes().len(), s.parts.panes().len() + 1);
            assert_eq!(replayed(&s, TabId::of(1))?, rects(&s, TabId::of(1)));
        }
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
        let dump = s
            .dump_recipe(Some(AnyRef::Tab(tab)))
            .map_err(|e| e.to_string())?;
        let mut again = started()?;
        run(&mut again, &dump).map_err(|e| format!("{e}\n{dump}"))?;
        let made = (again.parts.workspaces().iter())
            .flat_map(|w| w.tabs())
            .last();
        Ok(made.map(|t| rects(&again, t.id())).unwrap_or_default())
    }

    #[test]
    fn a_tab_given_a_size_splits_only_while_it_shows_every_pane() -> TestResult {
        let (mut s, _) = attached(50, 200)?;
        let tab = "opts 40 12 size str +1 workspace in new-tab drop";
        run(&mut s, &format!("{tab} str c1 client str @2 tab show"))?;
        // Four panes stacked take their 2 rows each and 3 separators.
        run(
            &mut s,
            "%2 str vertical axis split drop %2 str vertical axis split drop %2 str vertical axis split drop",
        )?;
        let refused = run(&mut s, "%2 str vertical axis split drop")
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

    #[test]
    fn a_tab_fitting_clients_splits_only_while_each_screen_shows_every_pane() -> TestResult {
        let (mut s, _) = attached(50, 200)?;
        s.attach(50, 30, None)?;
        watch(&mut s, 50, 8)?;
        // Ten panes side by side take their 2 columns each and 9 separators.
        for _ in 1..10 {
            run(&mut s, "%1 str horizontal axis split drop")?;
        }
        let refused = run(&mut s, "%1 str horizontal axis split drop")
            .err()
            .unwrap_or_default();
        assert!(
            refused.starts_with("size: ") && refused.contains("30x"),
            "{refused}"
        );
        let tab: TabId = "@1".parse().map_err(|_| "@1")?;
        assert_eq!(replayed(&s, tab)?, rects(&s, tab));
        Ok(())
    }

    #[test]
    fn a_split_with_shares_starts_at_its_share() -> TestResult {
        let mut s = started()?;
        run(&mut s, "opts 100 10 size str +1 workspace in new-tab drop")?;
        let lines = told(&mut s, |s| {
            let _ = run(s, "%2 str horizontal axis 80 18 shares split drop");
        });
        assert!(lines.contains(r#""created","pane":"%3""#), "{lines}");
        assert!(!lines.contains(r#""resized","pane":"%3""#), "{lines}");
        let size = s.parts.panes().get(&PaneId::of(3)).map(Pane::size);
        assert_eq!(size, Some(sized(10, 18)?));
        Ok(())
    }

    #[test]
    fn a_pane_starts_at_the_size_its_screens_give_it() -> TestResult {
        let mut s = started()?;
        let wide = s.attach(10, 101, None)?;
        s.attach(10, 81, None)?;
        let size = |s: &Session, id| s.parts.panes().get(&PaneId::of(id)).map(Pane::size);
        // Zoomed, the tab is unzoomed by the clients following the split.
        run(&mut s, "%1 zoom")?;
        let lines = told(&mut s, |s| {
            let _ = run(s, "%1 str horizontal axis str / cwd split drop");
        });
        assert!(
            lines.contains(r#""created","pane":"%2""#)
                && !lines.contains(r#""resized","pane":"%2""#),
            "{lines}"
        );
        assert_eq!(size(&s, 2), Some(sized(10, 40)?));
        let lines = told(&mut s, |s| {
            let _ = output_as(s, Some(wide), "opts 30 5 size str / cwd new-tab drop");
        });
        assert!(
            lines.contains(r#""created","pane":"%3""#)
                && !lines.contains(r#""resized","pane":"%3""#),
            "{lines}"
        );
        assert_eq!(size(&s, 3), Some(sized(10, 101)?));
        Ok(())
    }

    #[test]
    fn a_screen_with_no_room_sizes_no_pane() -> TestResult {
        let mut s = started()?;
        let small = s.attach(5, 6, None)?;
        // A client of one row with a bar has no room: its tab, which the
        // other client's screen sizes, sizes the tab it makes.
        let one = s.attach(1, 40, None)?;
        output_as(&mut s, Some(one), BAR_ROW)?;
        s.attach(10, 40, None)?;
        s.detach_client(small);
        output_as(
            &mut s,
            Some(one),
            "opts str +1 workspace in str / cwd new-tab drop",
        )?;
        let size = s.parts.panes().get(&PaneId::of(2)).map(Pane::size);
        assert_eq!(size, Some(sized(10, 40)?));
        let small = s.attach(5, 6, None)?;
        let made = "opts 100 10 size str +1 workspace in str / cwd new-tab \
                    dup str horizontal axis str / cwd split drop dup str horizontal axis str / cwd split drop str horizontal axis str / cwd split drop";
        run(&mut s, made)?;
        let refused = output_as(&mut s, Some(small), "%2 str @3 tab move").err();
        let refused = refused.unwrap_or_default();
        assert!(
            refused.contains("no room") && refused.contains("screen"),
            "{refused}"
        );
        Ok(())
    }

    #[test]
    fn a_screen_with_no_room_refuses_nothing() -> TestResult {
        let (mut s, _) = attached(11, 40)?;
        let tiny = s.attach(1, 40, None)?;
        output_as(&mut s, Some(tiny), BAR_ROW)?;
        run(
            &mut s,
            "%1 str horizontal axis split drop %1 str horizontal axis split drop",
        )?;
        for (pane, to) in [("%1", "new-tab"), ("%2", "new-workspace")] {
            let lines = told(&mut s, |s| {
                let moved = output_as(s, Some(tiny), &format!("{pane} str {to} move"));
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

    #[test]
    fn no_border_moves_where_a_pane_is_hidden() -> TestResult {
        let (mut s, c) = attached(11, 40)?;
        run(
            &mut s,
            "%1 str horizontal axis split drop %2 str horizontal axis split drop",
        )?;
        s.size_client(c, 11, 6)?;
        let refused = run(&mut s, "%1 str right 1 move-border").err();
        let refused = refused.unwrap_or_default();
        assert!(
            refused.starts_with("size: ") && refused.contains("every pane on 6x"),
            "{refused}"
        );
        Ok(())
    }

    #[test]
    fn shares_divide_the_split_panes_room() -> TestResult {
        let tab: TabId = "@2".parse().map_err(|_| "@2")?;
        let widths = |s: &Session| rects(s, tab).iter().map(Rect::w).collect::<Vec<u16>>();
        let made = "opts 101 10 size +1 in new-tab drop";
        for (cols, split, expected) in [
            (
                100,
                "%2 str horizontal axis 80 18 shares split drop %3 str horizontal axis 10 8 shares split drop",
                [81, 9, 8],
            ),
            (
                101,
                "%2 str horizontal axis split drop %3 str horizontal axis 2 1 shares split drop",
                [50, 33, 16],
            ),
            (
                101,
                "%2 str horizontal axis 1 1 shares split drop %3 str horizontal axis 2 1 shares split drop",
                [50, 33, 16],
            ),
        ] {
            let mut s = started()?;
            run(
                &mut s,
                &format!("opts {cols} 10 size +1 in new-tab drop {split}"),
            )?;
            assert_eq!(widths(&s), expected, "{split}");
            assert_eq!(replayed(&s, tab)?, rects(&s, tab));
        }
        let mut s = started()?;
        run(
            &mut s,
            &format!("{made} %2 str horizontal axis 4294967294 1 shares split drop"),
        )?;
        for split in [
            "%3 str horizontal axis split drop",
            "%3 str vertical axis 4294967295 1 shares split drop",
        ] {
            run(&mut s, split)?;
            assert_eq!(replayed(&s, tab)?, rects(&s, tab), "{split}");
        }
        Ok(())
    }

    #[test]
    fn every_layout_change_dumps_as_it_is() -> TestResult {
        let mut s = started()?;
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
            "%2 str horizontal axis 3 2 shares split drop",
            "%4 str vertical axis 1 2 shares split drop",
            "%5 str horizontal axis split drop",
            "%5 str vertical axis split drop",
            "%2 37 str horizontal resize-pane",
            "%5 7 str vertical resize-pane",
            "%6 str left 3 move-border",
            "%2 30 percent str horizontal resize-pane",
            "%2 %6 swap-with",
            "%4 str left move",
            "%5 kill",
            "%3 str @2 tab move",
            "%6 kill",
        ] {
            run(&mut s, change)?;
            let dump = s.dump_recipe(Some(AnyRef::Tab(tab)))?;
            let mut again = started()?;
            run(&mut again, &dump).map_err(|e| format!("{change}: {e}\n{dump}"))?;
            let made = (again.parts.workspaces().iter())
                .flat_map(|w| w.tabs())
                .last();
            let (Some(root), Some(made)) = (s.root(tab), made.map(Tab::root)) else {
                return Err(format!("{change}: no layout").into());
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

    #[test]
    fn a_pane_is_not_moved_where_it_would_be_hidden() -> TestResult {
        let mut s = started()?;
        let split =
            "opts 5 4 size str +1 workspace in new-tab drop %2 str horizontal axis split drop";
        run(&mut s, split)?;
        let refused = run(&mut s, "%1 str @2 tab move").err();
        let refused = refused.unwrap_or_default();
        assert!(
            refused.starts_with("size: ") && refused.contains("no room"),
            "{refused}"
        );
        let tab: TabId = "@2".parse().map_err(|_| "@2")?;
        assert_eq!(s.root(tab).map(|r| r.panes().len()), Some(2));
        Ok(())
    }

    #[test]
    fn failures_are_told_apart_by_kind() -> TestResult {
        let (mut s, c) = attached(10, 40)?;
        let origin = Origin::Client(c);
        let mut run =
            |code: &str| -> Result<Vec<Result<Reply, Error>>, Box<dyn std::error::Error>> {
                let commands = requests(&mut s, c, code)?;
                Ok(commands
                    .iter()
                    .map(|command| s.execute(command, &origin, Instant::now()))
                    .collect())
            };
        assert!(matches!(
            run("me me focused str next step")?.as_slice(),
            [Err(Error::OnlyOne(Kind::Pane))]
        ));
        let kill = Command::Kill {
            target: Some(AnyRef::Pane(PaneId::of(9))),
        };
        assert!(matches!(
            s.execute(&kill, &origin, Instant::now()),
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

#[cfg(test)]
mod journal_tests {
    use super::*;
    use crate::journal::Reader;
    use crate::session::testing::TestResult;
    use fux_wire::events::{Actor, Batch};
    use fux_wire::inspect::Scope;
    use testing::{output, run, run_as, started};

    fn batch(s: &Session, from: fux_wire::events::Cursor) -> Result<Batch, String> {
        Reader::new(Scope::All, s.parts.journal(), s.run, Some(from))?
            .batch(s.parts.journal(), true)
    }
    fn named<'a>(b: &'a Batch, name: &str) -> Vec<&'a fux_wire::events::Item> {
        b.events
            .iter()
            .filter(|e| e.data.get("event") == Some(&Json::str(name)))
            .collect()
    }

    /// A client's reader is told every change of its current child: one
    /// a zoom hides from its place, and a repair in a tab it is not shown.
    #[test]
    fn every_change_of_a_clients_current_child_is_shown() -> TestResult {
        let (mut s, c) = testing::attached(24, 80)?;
        run(
            &mut s,
            "%1 str horizontal axis split drop opts +1 in 80 24 size new-tab drop",
        )?;
        run(&mut s, "%1 zoom")?;
        let shown = |s: &Session, from| -> Result<Vec<Option<TabId>>, String> {
            let b = Reader::new(Scope::Client(c), s.parts.journal(), s.run, Some(from))?
                .batch(s.parts.journal(), true)?;
            Ok(named(&b, "shown").iter().map(|e| e.tab).collect())
        };
        // Its own focus moves to the zoomed pane it already shows.
        let from = s.parts.journal().end(s.run);
        run(&mut s, &format!("str {c} client %1 show"))?;
        assert_eq!(shown(&s, from)?, [Some(TabId::of(1))]);
        // Its focus in @1 is repaired while it is shown @2.
        run(&mut s, &format!("str {c} client @2 show"))?;
        let from = s.parts.journal().end(s.run);
        run(&mut s, "%1 kill")?;
        assert_eq!(shown(&s, from)?, [Some(TabId::of(2))]);
        // A tab and a workspace it is not shown close: its seats there go,
        // and nothing it is shown changes.
        run(
            &mut s,
            "opts +1 in 80 24 size new-tab drop opts str other in 80 24 size new-tab drop",
        )?;
        let from = s.parts.journal().end(s.run);
        run(&mut s, "@3 kill +2 kill")?;
        assert_eq!(shown(&s, from)?, []);
        Ok(())
    }

    #[test]
    fn discrete_layout_and_view_changes_invalidate_in_the_same_turn() -> TestResult {
        let (mut s, client) = testing::attached(24, 80)?;
        run(
            &mut s,
            "%1 str horizontal axis split drop opts 80 24 size str +1 workspace in new-tab drop",
        )?;
        let from = s.parts.journal().end(s.run);
        run(&mut s, "%1 %2 swap-with @2 str previous reorder")?;
        run(&mut s, &format!("str {client} client %1 show %1 zoom"))?;
        run(
            &mut s,
            "%2 str right 3 move-border %1 128 history-budget %1 clear-history all str note notify",
        )?;
        let b = batch(&s, from)?;
        for name in [
            "laid_out",
            "reordered",
            "shown",
            "zoomed",
            "history_budget",
            "history_cleared",
        ] {
            assert!(!named(&b, name).is_empty(), "missing {name}: {b:?}");
        }
        let from = b.next;
        s.detach(client, Instant::now());
        assert_eq!(named(&batch(&s, from)?, "detached").len(), 1);
        Ok(())
    }

    #[test]
    fn origins_distinguish_supplied_context_and_deferred_respawn() -> TestResult {
        let (mut s, client) = testing::attached(24, 80)?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab drop",
        )?;
        let id = PaneId::of(2);
        let from = s.parts.journal().end(s.run);
        let rename = |name: &str| Command::Rename {
            target: AnyRef::Pane(id),
            name: name.into(),
        };
        s.run_command(&rename("client"), &Origin::Client(client), Instant::now());
        let caller = Caller {
            gave: Some((id, 0)),
            within: Within::This,
            ..Caller::default()
        };
        s.run_command(&rename("pane"), &Origin::Cli(caller), Instant::now());
        s.run_command(
            &rename("outside"),
            &Origin::Cli(Caller::default()),
            Instant::now(),
        );
        testing::output_as(&mut s, Some(client), "@2 90 24 resize")?;
        s.output(id, b"\x07", Instant::now());
        // Force the replacement to finish on a later parser turn.
        let spends = crate::pane::tests::spends_the_budget(24, 80);
        let left: Vec<u8> = std::iter::repeat_n(spends, 3).flatten().collect();
        s.output(id, &left, Instant::now());
        let replacement = Command::Respawn {
            target: id,
            kill: false,
            argv: None,
        };
        let begun = s.begin(
            Request::Do(replacement),
            &Origin::Client(client),
            Instant::now(),
        );
        assert!(matches!(begun, Begun::Waiting(_)));
        s.run_command(
            &rename("later"),
            &Origin::Cli(Caller::default()),
            Instant::now(),
        );
        for _ in 0..32 {
            if !s.respawning.contains_key(&id) {
                break;
            }
            s.fire(Timer::Unread(id), Instant::now());
        }
        assert!(!s.respawning.contains_key(&id));
        let b = batch(&s, from)?;
        let actors: Vec<_> = named(&b, "renamed").iter().map(|e| e.origin).collect();
        assert_eq!(
            actors,
            [
                Actor::Client(client),
                Actor::Pane(id, 0),
                Actor::Outside,
                Actor::Outside
            ]
        );
        assert_eq!(
            named(&b, "bell").first().map(|e| e.origin),
            Some(Actor::Program(id, 0))
        );
        assert_eq!(
            named(&b, "respawned").first().map(|e| e.origin),
            Some(Actor::Client(client))
        );
        assert_eq!(
            named(&b, "resized").first().map(|e| e.origin),
            Some(Actor::Client(client))
        );
        assert_eq!(s.parts.journal().current_actor(), Actor::Server);
        Ok(())
    }
    #[test]
    fn the_events_word_prints_events_till_its_scope_is_gone_and_fails_on_loss() -> TestResult {
        let mut s = started()?;
        let from = s.parts.journal().end(s.run);
        run(&mut s, "%1 str hello rename")?;
        let streamed = output(&mut s, &format!("%1 dup kill str {from} events"))?;
        let events: Vec<_> = streamed.lines().collect();
        assert!(
            matches!(&events[..], [renamed, .., closed]
                if renamed.contains(r#""event":"renamed""#) && closed.contains(r#""event":"closed""#)),
            "{streamed}"
        );
        assert!(
            events.iter().all(|e| e.contains(r#""next":""#)),
            "{streamed}"
        );
        // Real records retain the server run in their handles.
        let kind = output(
            &mut s,
            &format!("all str {from} 0 next-events str events get first str pane get kind"),
        )?;
        assert_eq!(kind, "pane\n");
        let mut s = started()?;
        let from = s.parts.journal().end(s.run);
        let kept = fux::script::default_state()?.preferences.journal_entries;
        for _ in 0..kept.get().saturating_add(1) {
            s.output(PaneId::of(1), b"\x07", Instant::now());
        }
        let job = format!("%1 dup kill str {from} events").into();
        let (printed, failed) = run_as(&mut s, None, &job);
        assert!(printed.is_empty(), "loss prints nothing");
        assert!(failed.is_err_and(|e| e.message.contains("journal entries missed; resnapshot")));
        Ok(())
    }
    #[test]
    fn client_departures_invalidate_former_tabs_and_workspaces() -> TestResult {
        for target in ["@2", "str +2 workspace"] {
            let (mut s, client) = testing::attached(24, 80)?;
            run(
                &mut s,
                "opts 80 24 size str +1 workspace in new-tab drop opts str new-workspace in 80 24 size new-tab drop @1 80 24 resize @2 80 24 resize @3 80 24 resize",
            )?;
            let from = s.parts.journal().end(s.run);
            run(&mut s, &format!("str {client} client {target} show"))?;
            for scope in [
                Scope::Target(AnyRef::Tab(TabId::of(1))),
                Scope::Target(AnyRef::Workspace(WsId::of(1))),
            ] {
                let b = Reader::new(scope, s.parts.journal(), s.run, Some(from))?
                    .batch(s.parts.journal(), true)?;
                assert!(
                    !named(&b, "shown").is_empty(),
                    "departure from {scope:?}: {b:?}"
                );
                assert!(named(&b, "resized").is_empty());
            }
        }
        Ok(())
    }

    #[test]
    fn typing_invalidates_a_changed_host_once() -> TestResult {
        let (mut s, first) = testing::attached(24, 80)?;
        let second = s.attach(24, 80, None)?;
        let tab = TabId::of(1);
        assert_eq!(s.host(Some(tab)), Some(first));
        let from = s.parts.journal().end(s.run);
        s.input(second, b"x");
        assert_eq!(s.host(Some(tab)), Some(second));
        let b = Reader::new(
            Scope::Target(AnyRef::Tab(tab)),
            s.parts.journal(),
            s.run,
            Some(from),
        )?
        .batch(s.parts.journal(), true)?;
        assert!(!b.events.is_empty(), "host changed without invalidation");
        assert!(b.events.iter().all(|e| e.origin == Actor::Client(second)));
        let after = s.parts.journal().end(s.run);
        s.input(second, b"y");
        assert_eq!(
            s.parts.journal().end(s.run),
            after,
            "unchanged host must not journal every key"
        );
        Ok(())
    }

    #[test]
    fn notice_dismissal_and_preferences_paint_without_journal_entries() -> TestResult {
        use fux_wire::act::Act;
        for paste in [false, true] {
            let (mut s, client) = testing::attached(24, 80)?;
            s.typed(client);
            let from = s.parts.journal().end(s.run);
            s.notify_to(client, "notice", false);
            assert!(s.views.get(&client).ok_or("view")?.dirty);
            s.views.get_mut(&client).ok_or("view")?.dirty = false;
            if paste {
                s.apply(client, vec![Act::Paste("quiet input".into())]);
            } else {
                s.input(client, b"x");
            }
            let view = s.views.get(&client).ok_or("view")?;
            assert!(view.notice.is_none());
            assert!(view.dirty);
            assert_eq!(s.parts.journal().end(s.run), from);
            let mut presentation = view.presentation.clone();
            presentation.titles = !presentation.titles;
            s.views.get_mut(&client).ok_or("view")?.dirty = false;
            s.apply(client, vec![Act::Presentation(presentation.clone())]);
            let view = s.views.get(&client).ok_or("view")?;
            assert_eq!(view.presentation, presentation);
            assert!(view.dirty);
            assert_eq!(s.parts.journal().end(s.run), from);
        }
        Ok(())
    }
}

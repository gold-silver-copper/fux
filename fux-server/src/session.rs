//! The server's state: workspaces, tabs, panes and the clients' views, and
//! every command that changes them.
use crate::copy::Copy as Pane;
use crate::journal::{Detach, ViewChange};
use crate::link::{Answered, Ask, Begin, FromPane, Hosting, Link, Remote, ToPane};
use crate::pane::{self, Runs};
use crate::view::{ClientKind, Mode, View};
use crate::wait::{Form, Pending, Waiting, Watches};
use crate::workspace::{Child as _, Destination, Fit, Killing, Parts, Split, Tab, Workspace};
use fux_vt::Size;
use fux_vt::keys::Direction;
use fux_wire::answer::{self, Ended, PaneAnswer, PaneResults};
use fux_wire::code::{Error, Raise, fail};
use fux_wire::command::Start;
use fux_wire::command::{
    self, AnyRef, Beside, ClientAction, Command, Extent, Input, Kind, Members, MoveTo, PaneResize,
    Request, Resized, Sibling, SwapWith, Way, WsRef,
};
use fux_wire::id::{ClientId, Ids, PaneId, TabId, WsId};
use fux_wire::json::Json;
use fux_wire::layout::{self, Axis, NoPane, Placement, Rect, Shares, Side, Tree};
use fux_wire::protocol::{Attach, Body, Reply, ToAttached};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
mod inspect;
use std::time::Instant;

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
    /// Keys for a client, as typed at its terminal: the body another
    /// client's connection keeps by this number and sends its connection.
    Keys(ClientId, u64),
    /// Stop the server, saying why, as this actor.
    Shutdown(String, fux_wire::events::Actor),
}

/// Something the session waits for until a moment (`Session::timers`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timer {
    /// A pane held in this process (`Link::Local`) has something to do.
    Pane(PaneId, crate::host::Timer),
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

/// How a tab is laid out: its layout revision, the size each of its panes
/// is laid out at, and its host.
type Laid = (
    Option<fux_wire::command::LayoutRevision>,
    Vec<(PaneId, Size)>,
    Option<ClientId>,
);

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

/// What a change undoes, taken before it (`Session::transact`).
struct Undo {
    structure: crate::workspace::Structure,
    ids: Ids,
    views: BTreeMap<ClientId, View>,
    journal: fux_wire::events::Seq,
    outbox: usize,
    /// The last question asked of a pane before it.
    asked: u64,
}

/// A program whose pane's own process ended first, adopted by the server
/// (`fuxix::process::adopt_orphans`): reaped, its status told; killed once
/// its grace is over, as a closed pane's program is.
pub struct Orphan {
    pub pane: PaneId,
    pub pid: fuxix::process::Pid,
    pub deadline: Instant,
}

/// What a command line from the CLI begins: its outcome, now, or a wait
/// for one (`wait`, or an action given wait words).
pub enum Begun {
    Done(Reply),
    Waiting(Box<Waiting>),
}

/// What a command run gives: its reply now, or what its panes were asked,
/// answered as they answer.
enum Done {
    Now(Reply),
    Asked(Pending),
}

pub struct Session {
    /// Its workspaces, tabs and panes, and the journal of what happened to
    /// them, which events read.
    pub parts: Parts,
    pub views: BTreeMap<ClientId, View>,
    /// The socket a started pane is told to reach its server by, if panes
    /// start real processes; tests of the state alone start none.
    launch: Option<PathBuf>,
    /// The running fux, a started pane's `FUX`, if it can be had.
    pub exe: Option<PathBuf>,
    /// Which run this is: its panes' `FUX_SERVER`, by which a command from
    /// a pane tells whether the pane is this run's, and every version's, by
    /// which a wait tells its own. It outlives a server process that
    /// crashes (D-124).
    pub run: command::Run,
    /// Whose journal this server process keeps, its cursors' second half:
    /// the run's, for its first server; one of its own for a server that
    /// took the run back (D-124 (5)).
    pub journal_run: command::Run,
    pub outbox: Vec<Outgoing>,
    /// Each pane's link to its own process, by its pane; and those of
    /// panes closed, till their processes end (`Session::ending`).
    pub links: BTreeMap<PaneId, Link>,
    pub ending: Vec<(PaneId, Link)>,
    /// What panes told that is still to be taken in (`Session::pump`), in
    /// order.
    inbox: std::collections::VecDeque<(PaneId, FromPane)>,
    pumping: bool,
    /// Each question asked of a pane not yet answered, by its number, with
    /// its pane; and the answers come, till their waits take them.
    asks: BTreeMap<u64, PaneId>,
    answers: BTreeMap<u64, Answered>,
    /// The last question's number.
    asked_pane: u64,
    /// The events its journal keeps.
    entries: command::JournalEntries,
    /// Its run's record as last written (`write_record`), and whether it
    /// is still written: not once the run ends (`unrecord`).
    recorded: Vec<u8>,
    recording: bool,
    /// What it tells its panes while a change may still be undone
    /// (`transact`): told once the change is recorded, else never.
    staged: Option<Vec<(PaneId, ToPane)>>,
    /// Why a change could not be recorded, which undoes it (`transact`).
    unrecorded: Option<Error>,
    /// The panes a change closes, their copies and links, kept whole till
    /// it is recorded: told to close then, or put back (`transact`).
    closing: Vec<(PaneId, Link, bool)>,
    closed: Vec<Pane>,
    /// The body the frame being taken has, kept by its connection
    /// (`link::FromConn::Frame`), till a request or act takes it; the
    /// bodies to go to panes, each its connection's number, its token and
    /// the panes; and the last token given.
    body: Option<u64>,
    pub deliveries: Vec<(u64, u64, Vec<PaneId>)>,
    /// What panes used up of what connections held them for (`Retired`):
    /// the server lets each go of its connection.
    pub retired: Vec<(PaneId, u64)>,
    /// A reply the asker's connection makes itself (`link::Making`): taken
    /// as the reply is sent.
    pub making: Option<crate::link::Making>,
    /// Programs adopted from panes whose own processes ended first.
    pub orphans: Vec<Orphan>,
    /// Where `size_panes` lays out each tab; reused by every settle.
    placed: Placement,
    /// How each tab was laid out at the last settle (`tell_laid_out`).
    laid: BTreeMap<TabId, Laid>,
    ids: Ids,
    /// Questions relayed to clients (`Waiting::Read`): the last one's
    /// number, which its answer carries back.
    asked: u64,
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

/// What a pane whose own process ended can't be asked to do.
fn lost(pane: PaneId) -> Error {
    fail(Raise::NotRunning, format!("{pane}: its own process ended"))
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
            journal_run: run,
            outbox: Vec::new(),
            links: BTreeMap::new(),
            ending: Vec::new(),
            inbox: std::collections::VecDeque::new(),
            pumping: false,
            asks: BTreeMap::new(),
            answers: BTreeMap::new(),
            asked_pane: 0,
            entries: journal,
            recorded: Vec::new(),
            recording: true,
            staged: None,
            unrecorded: None,
            closing: Vec::new(),
            closed: Vec::new(),
            body: None,
            deliveries: Vec::new(),
            retired: Vec::new(),
            making: None,
            orphans: Vec::new(),
            placed: Placement::default(),
            laid: BTreeMap::new(),
            ids: Ids::default(),
            asked: 0,
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
                            // Or the program a respawn under way started.
                            self.parts.panes().get(p).is_some_and(|pane| {
                                let current = match pane.respawning {
                                    true => pane.respawns.checked_add(1),
                                    false => Some(pane.respawns),
                                };
                                current == Some(generation)
                            })
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

    // ------------------------------------------------------------ panes

    /// Says `message` to pane `id`'s own process, if it has one.
    pub(crate) fn tell_pane(&mut self, id: PaneId, message: ToPane, now: Instant) {
        if let Some(staged) = &mut self.staged {
            staged.push((id, message));
        } else if let Some(link) = self.links.get_mut(&id) {
            link.send(id, message, now, &mut self.inbox);
        }
    }

    /// Whether what client `client` types waits: its focused pane's process
    /// takes nothing of what it was sent (stopped, say: `Remote::stalled`).
    /// It stays with the client's connection, never queued here.
    pub fn typing_waits(&self, client: ClientId) -> bool {
        let pane = self.focused(client);
        pane.and_then(|pane| self.links.get(&pane))
            .is_some_and(|link| matches!(link, Link::Remote(remote) if remote.stalled))
    }

    /// Where its run's panes listen, and keep what outlives its server
    /// (`socket::panes`): none in a session that starts no programs.
    pub fn panes(&self) -> Option<std::path::PathBuf> {
        self.launch.as_deref().map(crate::socket::panes)
    }

    /// Pane `id`'s screen, if it is held in this process (a session that
    /// starts no programs): one in a process of its own sends its screen to
    /// the connections showing it, never here (`conn`).
    pub fn screen(&self, id: PaneId) -> Option<&fux_vt::Screen> {
        match self.links.get(&id)? {
            Link::Local(host) => Some(host.pane().screen()),
            Link::Remote(_) => None,
        }
    }

    /// Asks pane `id` `what`, as the journal's actor now: the question's
    /// number, which its answer carries (`Waiting::Asked`). A pane whose
    /// own process is gone is asked nothing: what it can't do, it is told.
    pub(crate) fn ask_pane(&mut self, id: PaneId, what: Ask, now: Instant) -> Result<u64, Error> {
        let pane = self.pane(id)?;
        if !self.links.contains_key(&id) {
            return Err(pane.refuses_input().unwrap_or_else(|| lost(id)));
        }
        self.asked_pane = self.asked_pane.wrapping_add(1);
        let ask = self.asked_pane;
        self.asks.insert(ask, id);
        let actor = self.parts.journal().current_actor();
        self.tell_pane(id, ToPane::Ask { ask, actor, what }, now);
        Ok(ask)
    }

    /// The answer to question `ask`, if it came, taken.
    pub(crate) fn answer_to(&mut self, ask: u64) -> Option<Answered> {
        self.answers.remove(&ask)
    }

    /// Whether question `ask` is answered, or never will be.
    pub(crate) fn answered_yet(&self, ask: u64) -> bool {
        !self.asks.contains_key(&ask)
    }

    /// The reply of the questions `answers` answer, as `form` makes them
    /// one (`Pending`). A question its pane never answered (its process
    /// ended first) is `indeterminate`.
    pub(crate) fn replied(
        &mut self,
        answers: Vec<(PaneId, Option<Answered>)>,
        form: &Form,
    ) -> Reply {
        let unanswered = |pane: PaneId| crate::link::Refusal {
            code: fux_wire::code::Code::Indeterminate,
            message: format!("{pane} never answered: it may have acted"),
        };
        if let Form::Respawn(_) = form {
            for (pane, _) in &answers {
                if let Some(copy) = self.parts.pane_mut(*pane) {
                    copy.respawning = false;
                }
            }
        }
        let mut reply = match form {
            Form::Many => {
                let (mut took, mut sent, mut refused) = (Vec::new(), Vec::new(), None);
                for (pane, answer) in answers {
                    match answer {
                        Some(Answered::Acted(version)) => {
                            took.push(PaneAnswer::Acted(version));
                            sent.push(pane.to_string());
                        }
                        Some(Answered::Refused(refusal)) => _ = refused.get_or_insert(refusal),
                        Some(Answered::Done | Answered::Captured(_) | Answered::Held { .. }) => {}
                        None => _ = refused.get_or_insert_with(|| unanswered(pane)),
                    }
                }
                match refused {
                    None => PaneResults::Many(took).into(),
                    Some(refusal) if sent.is_empty() => {
                        Reply::failed(refusal.code, refusal.message)
                    }
                    Some(refusal) => {
                        let message =
                            format!("{}; already sent to {}", refusal.message, sent.join(" "));
                        Reply::failed(refusal.code, message)
                    }
                }
            }
            Form::One | Form::Capture | Form::Respawn(_) => {
                let Some((pane, answer)) = answers.into_iter().next() else {
                    return Reply::default();
                };
                match answer {
                    Some(Answered::Acted(version)) => {
                        if let (Form::Respawn(Some(argv)), Some(copy)) =
                            (form, self.parts.pane_mut(pane))
                            && let Runs::Explicit(launch) = &mut copy.runs
                        {
                            launch.argv = argv.clone();
                        }
                        acted(version)
                    }
                    Some(Answered::Done) => Reply::default(),
                    Some(Answered::Captured(captured)) => self.captured(pane, *captured),
                    Some(Answered::Held { token, cursor, .. }) => {
                        let placed = crate::link::Placed {
                            cursor,
                            ..self.placed(pane)
                        };
                        self.making = Some(crate::link::Making::Capture {
                            pane,
                            token,
                            placed,
                        });
                        Reply::default()
                    }
                    Some(Answered::Refused(refusal)) => {
                        Reply::failed(refusal.code, refusal.message)
                    }
                    None => {
                        let refusal = unanswered(pane);
                        Reply::failed(refusal.code, refusal.message)
                    }
                }
            }
        };
        reply.run = Some(self.run);
        reply
    }

    /// Pane `pane`'s capture, as its process made it, given its place, name
    /// and label now (`placed`), its cursor as it was taken.
    fn captured(&self, pane: PaneId, mut captured: fux_wire::answer::Captured) -> Reply {
        let mut placed = self.placed(pane);
        if let fux_wire::answer::Of::Pane { read, .. } = &captured.of {
            let cursor = read.iter().find(|(key, _)| key == "cursor");
            if let Some((_, Json::String(cursor))) = cursor {
                placed.cursor = cursor.to_string();
            }
        }
        crate::capture::place(&mut captured, &placed);
        Body::Capture(Box::new(captured)).into()
    }

    /// What the server knows of pane `pane` as it is captured: its tab and
    /// place, its name and label (`link::Placed`).
    fn placed(&self, pane: PaneId) -> crate::link::Placed {
        let mut placed = crate::link::Placed::default();
        if let Some((_, tab)) = self.locate(pane) {
            placed.tab = Some(tab);
            let at = self.tab(tab).and_then(|t| self.sized(t).rect(pane));
            placed.row = at.as_ref().map(Rect::y);
            placed.col = at.as_ref().map(Rect::x);
        }
        if let Some(copy) = self.parts.panes().get(&pane) {
            placed.name = copy.name().map(str::to_owned);
            placed.label = Some(format!("{}", copy.display_label()));
        }
        placed
    }

    /// The body of the frame taken next, kept by its connection: what a
    /// request or act it makes takes for its inputs (`send_keys`, `paste`).
    pub fn give_body(&mut self, body: Option<u64>) {
        self.body = body;
    }

    /// A body's token: its panes are asked for it, and its connection sends
    /// it them (`deliveries`). Numbered as questions are, so that no body
    /// and no capture a pane holds (a question's number) are ever one.
    pub(crate) fn body_token(&mut self) -> Option<(u64, u64)> {
        let body = self.body.take()?;
        self.asked_pane = self.asked_pane.wrapping_add(1);
        Some((body, self.asked_pane))
    }

    /// Questions `asks` and `tokens`, whose asker is gone: none is waited
    /// for, an answer that comes is let go, and what the panes hold or wait
    /// for of them is let go too (`ToPane::Drop`). What a pane did is done.
    pub fn abandon(&mut self, asks: &[(PaneId, u64)], tokens: &[(PaneId, u64)], now: Instant) {
        for (pane, ask) in asks {
            self.asks.remove(ask);
            if let Some(Answered::Held { token, .. }) = self.answers.remove(ask) {
                self.tell_pane(*pane, ToPane::Drop { token }, now);
            }
        }
        for (pane, token) in tokens {
            self.tell_pane(*pane, ToPane::Drop { token: *token }, now);
        }
    }

    /// What pane `id`'s own process said, to take in (`pump`).
    pub fn heard(&mut self, id: PaneId, report: FromPane) {
        self.inbox.push_back((id, report));
    }

    /// Takes in what panes told, in order, each as it came (`told`): one a
    /// pane held in this process told at once too.
    pub fn pump(&mut self, now: Instant) {
        if std::mem::replace(&mut self.pumping, true) {
            return;
        }
        while let Some((id, report)) = self.inbox.pop_front() {
            self.told(id, report, now);
        }
        // What closed panes answered is in: those done are let go.
        self.reap_ending();
        self.pumping = false;
    }

    /// Pane `id` told `report`. An event is journaled as its copy takes
    /// what it changed, in one step (`Parts::told`); its screen goes onto
    /// its copy, and the views showing it are painted again. A pane closed
    /// tells nothing more but its answers.
    fn told(&mut self, id: PaneId, report: FromPane, now: Instant) {
        // Its program's handle is copied whatever its copy's state: a pane
        // begun before its copy is made, or closing, still runs it.
        if let FromPane::Offer(offered) = report {
            self.adopt(id, offered);
            return;
        }
        if let FromPane::Answer { ask, mut answer } = report {
            // A capture is the pane's own facts as it answered, after all it
            // told before: its counts are the copy's, unless it is behind;
            // and its cursor the journal's as it is taken, before anything
            // the pane told after it (D-120 (3)).
            let cursor = self.parts.journal().end(self.journal_run).to_string();
            let counts = match &mut answer {
                Answered::Captured(captured) => {
                    let placed = crate::link::Placed {
                        cursor,
                        ..crate::link::Placed::default()
                    };
                    crate::capture::place(captured, &placed);
                    match &captured.of {
                        fux_wire::answer::Of::Pane { pane: observed, .. } => {
                            Some(observed.version.counts)
                        }
                        fux_wire::answer::Of::Client { .. } => None,
                    }
                }
                Answered::Held {
                    counts,
                    cursor: stamped,
                    ..
                } => {
                    *stamped = cursor;
                    Some(*counts)
                }
                Answered::Acted(_) | Answered::Done | Answered::Refused(_) => None,
            };
            if let (Some(counts), Some(pane)) = (counts, self.parts.pane_mut(id)) {
                pane.caught_up(counts);
            }
            // One its asker gave up on is let go (`abandon`).
            if self.asks.remove(&ask).is_some() {
                self.answers.insert(ask, answer);
            } else if let Answered::Held { token, .. } = answer {
                self.tell_pane(id, ToPane::Drop { token }, now);
            }
            return;
        }
        if !self.parts.panes().contains_key(&id) {
            return;
        }
        match report {
            FromPane::Begun(_) | FromPane::Greeting(_) | FromPane::Answer { .. } => {}
            FromPane::Told {
                told,
                actor,
                counts,
                facts,
            } => {
                if let (Some(facts), Some(pane)) = (facts, self.parts.pane_mut(id)) {
                    pane.told_facts(*facts);
                }
                use crate::journal::Told;
                let exited = if let Told::Exited(end) = &told {
                    Some(*end)
                } else {
                    None
                };
                // What shows: its title, in its viewers' bars and in those
                // of its workspace's where it names its tab; whether its
                // program takes the mouse.
                let shows = matches!(
                    told,
                    Told::Resized(_)
                        | Told::Exited(_)
                        | Told::HungUp
                        | Told::ReadFailed(_)
                        | Told::Respawned { .. }
                );
                let title = matches!(told, Told::Title(_));
                let labels = title.then(|| self.labels(id));
                self.parts.told(id, told, actor, counts);
                if shows {
                    self.touch();
                }
                if let Some((was, viewers)) = labels {
                    let changed = self.labels(id).0 != was;
                    let others = viewers.filter(|_| changed).unwrap_or_default();
                    for client in self.viewers(AnyRef::Pane(id)).into_iter().chain(others) {
                        if let Some(view) = self.views.get_mut(&client) {
                            view.dirty = true;
                        }
                    }
                }
                if let Some(end) = exited {
                    self.program_ended(id, end, now);
                }
            }
            FromPane::Update(update) => {
                // Its screen goes to the clients showing it from the pane
                // itself (`conn`): what they show of its facts here is its
                // label, on their bars.
                let (was, others) = self.labels(id);
                if let Some(pane) = self.parts.pane_mut(id) {
                    pane.update(*update);
                }
                if self.labels(id).0 != was {
                    let viewers = self.viewers(AnyRef::Pane(id));
                    for client in viewers.into_iter().chain(others.unwrap_or_default()) {
                        if let Some(view) = self.views.get_mut(&client) {
                            view.dirty = true;
                        }
                    }
                }
                self.tell_pane(id, ToPane::Taken, now);
            }
            FromPane::Alert { bell, notified } => self.alert(id, bell, notified, now),
            FromPane::Copied(text) => {
                let viewers = self.viewers(AnyRef::Pane(id));
                self.copied(&text, &viewers);
            }
            FromPane::Notice { text, error } => {
                for client in self.viewers(AnyRef::Pane(id)) {
                    self.notify_to(client, text.clone(), error);
                }
            }
            FromPane::Refused { client, why } => self.notify_to(client, why, true),
            FromPane::Retired(token) => self.retired.push((id, token)),
            FromPane::Offer(_) => {}
        }
    }

    /// Takes a copy of pane `id`'s program's handle from its process, as
    /// it `offered` it (Windows, D-134): its program is the server's to end
    /// and reap should that process crash. The pane's process is the one
    /// the server started, or the one a takeover's greeting named, checked
    /// against the pipe's maker (`Link::take`); the copy, against the
    /// program's pid and creation time. One not taken is logged: the
    /// program then ends unreaped if its pane's process crashes.
    fn adopt(&mut self, id: PaneId, offered: crate::link::Offered) {
        let Some(Link::Remote(remote)) = self.links.get_mut(&id) else {
            return;
        };
        let Some(offer) = offered.offer() else {
            return;
        };
        match fuxix::process::adopt(remote.pid, offer) {
            Ok(()) => {
                if let Some(old) = remote.adopted.replace(offer.program.pid)
                    && old != offer.program.pid
                {
                    fuxix::process::release(old);
                }
            }
            Err(error) => log!("{id}: its program's handle could not be copied: {error}"),
        }
    }

    /// Pane `id`'s label, and if it labels its tab (no name of the tab's or
    /// its own, and first in it), the clients shown its workspace and not
    /// the tab, whose bars show it.
    fn labels(&self, id: PaneId) -> (String, Option<Vec<ClientId>>) {
        let Some(pane) = self.parts.panes().get(&id) else {
            return (String::new(), None);
        };
        let label = pane.label().into_owned();
        let Some((ws, tab)) = self.locate(id) else {
            return (label, None);
        };
        let names = self
            .tab(tab)
            .is_none_or(|t| t.name().is_some() || t.root().first_pane() != id);
        if names || pane.name().is_some() {
            return (label, None);
        }
        let viewers = self.viewers(AnyRef::Pane(id));
        let others = (self.viewers(AnyRef::Workspace(ws)).into_iter())
            .filter(|client| !viewers.contains(client))
            .collect();
        (label, Some(others))
    }

    /// Pane `id`'s program ended so, its end told: a shell's pane closes,
    /// and a program's stays, its last screen kept (the role rule).
    fn program_ended(&mut self, id: PaneId, end: pane::End, now: Instant) {
        let Some(pane) = self.parts.panes().get(&id) else {
            return;
        };
        if let Runs::Shell(_) = pane.runs {
            let left = end.left.told();
            let why = answer::shell_status(end.ended).map_or_else(
                || answer::told(end.ended),
                |s| format!("exited with status {s}"),
            );
            self.close_pane(id, Some(format!("{why}{left}")), now);
        }
    }

    /// The socket its panes' programs reach it by, if they start any.
    pub fn socket(&self) -> Option<&Path> {
        self.launch.as_deref()
    }

    /// The pids of the panes' own processes, linked or ending, not yet
    /// reaped: the server's children it reaps by pid (`Server::reap`); and
    /// the programs it adopted from those that ended (`Orphan`).
    pub fn children(&self) -> Vec<fuxix::process::Pid> {
        let remote = |link: &Link| match link {
            Link::Remote(remote) => (remote.child && remote.ended.is_none()).then_some(remote.pid),
            Link::Local(_) => None,
        };
        (self.links.values().filter_map(remote))
            .chain(self.ending.iter().filter_map(|(_, link)| remote(link)))
            .chain(self.orphans.iter().map(|o| o.pid))
            .collect()
    }

    /// Process `pid`, the server's child, was reaped, ending `status`: a
    /// pane's own, or a program adopted from one (`Orphan`).
    pub fn reaped(
        &mut self,
        pid: fuxix::process::Pid,
        status: fuxix::process::Status,
        now: Instant,
    ) {
        let (ended, orphans) = std::mem::take(&mut self.orphans)
            .into_iter()
            .partition::<Vec<_>, _>(|o| o.pid == pid);
        self.orphans = orphans;
        if let Some(orphan) = ended.first() {
            self.orphan_ended(orphan.pane, Ended::from(status), now);
            return;
        }
        let linked = (self.links.iter_mut().map(|(id, link)| (*id, link)))
            .chain(self.ending.iter_mut().map(|(id, link)| (*id, link)));
        for (id, link) in linked {
            if let Link::Remote(remote) = link
                && remote.pid == pid
            {
                remote.ended = Some(status);
                self.gone(id, now);
                return;
            }
        }
    }

    /// Pane `id`'s link closed.
    pub fn link_closed(&mut self, id: PaneId, now: Instant) {
        let linked = (self.links.get_mut(&id).into_iter()).chain(
            self.ending
                .iter_mut()
                .filter(|(e, _)| *e == id)
                .map(|(_, l)| l),
        );
        for link in linked {
            if let Link::Remote(remote) = link {
                remote.closed = true;
            }
        }
        self.gone(id, now);
    }

    /// Pane `id`'s own process is gone once its link closed and it was
    /// reaped: a closed pane's link is let go, a pane's is lost.
    fn gone(&mut self, id: PaneId, now: Instant) {
        self.reap_ending();
        // One this server did not start is not its child to reap: its link
        // closing is all it learns.
        let how = match self.links.get(&id) {
            Some(Link::Remote(Remote {
                closed: true,
                ended: Some(status),
                ..
            })) => match status {
                fuxix::process::Status::Exited(code) => {
                    format!("its pane's own process exited with status {code}")
                }
                #[cfg(unix)]
                fuxix::process::Status::Signalled { signal, .. } => {
                    format!("its pane's own process was killed by signal {signal}")
                }
            },
            Some(Link::Remote(Remote {
                closed: true,
                child: false,
                ..
            })) => "its pane's own process ended".to_owned(),
            Some(_) | None => return,
        };
        self.pane_lost(id, &how, now);
    }

    /// Pane `id`'s own process is gone, ending `how`. Its pane reads as its
    /// program's terminal failing, saying how (`read_failed`); then its
    /// program's end: its real status, once the server reaps it (it adopts
    /// orphans), else none; then the role rule. Every question it was
    /// asked and did not answer is `indeterminate`: it may have done it.
    fn pane_lost(&mut self, id: PaneId, how: &str, now: Instant) {
        let link = self.links.remove(&id);
        // The copy of a program's handle it holds is let go, unless that
        // program still runs, to be reaped below.
        let running = (self.parts.panes().get(&id))
            .filter(|pane| pane.exit_status().is_none())
            .and_then(|pane| pane.pid());
        if let Some(Link::Remote(remote)) = link
            && let Some(adopted) = remote.adopted
            && running != Some(adopted)
        {
            fuxix::process::release(adopted);
        }
        let unanswered: Vec<u64> = (self.asks.iter())
            .filter(|(_, pane)| **pane == id)
            .map(|(ask, _)| *ask)
            .collect();
        for ask in unanswered {
            self.asks.remove(&ask);
            let refusal = crate::link::Refusal {
                code: fux_wire::code::Code::Indeterminate,
                message: format!("{id}: {how} before it answered: it may have acted"),
            };
            self.answers.insert(ask, Answered::Refused(refusal));
        }
        let Some(pane) = self.parts.panes().get(&id) else {
            return;
        };
        let (counts, generation, pid) = (pane.counts(), pane.respawns, pane.pid());
        if pane.exit_status().is_none() {
            let actor = fux_wire::events::Actor::Program(id, generation);
            let told = crate::journal::Told::ReadFailed(how.to_owned());
            self.parts.told(id, told, actor, counts);
            match pid {
                // Its program is the server's now, to reap, and kill once its
                // grace is over; else its status is lost.
                Some(pid) if fuxix::process::ended(pid).is_ok() => self.orphans.push(Orphan {
                    pane: id,
                    pid,
                    deadline: crate::after(now, crate::host::GRACE),
                }),
                Some(_) | None => self.orphan_ended(id, Ended::Lost, now),
            }
        }
        if let Some(pane) = self.parts.pane_mut(id) {
            pane.lost();
        }
        self.touch();
        self.settle(now);
    }

    /// Pane `id`'s program, its pane's own process gone, ended `ended`: told,
    /// then the role rule.
    fn orphan_ended(&mut self, id: PaneId, ended: Ended, now: Instant) {
        let Some(pane) = self.parts.panes().get(&id) else {
            return;
        };
        let (counts, generation) = (pane.counts(), pane.respawns);
        let actor = fux_wire::events::Actor::Program(id, generation);
        let end = pane::End {
            ended,
            left: pane::Left::default(),
        };
        let counts = fux_wire::command::Counts {
            ended: true,
            ..counts
        };
        self.parts
            .told(id, crate::journal::Told::Exited(end), actor, counts);
        self.touch();
        self.program_ended(id, end, now);
        self.settle(now);
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
    /// as its program asked for keys: to one seen, only if it is still at
    /// that version as they are queued, else none, failing `stale`. Each
    /// pane takes them or refuses on its own, as they reach it (D-120 (4)):
    /// the answer says which took them. That is all it promises: not what
    /// its program does next.
    fn send_keys(
        &mut self,
        to: command::To,
        inputs: &[Input],
        now: Instant,
    ) -> Result<Pending, Error> {
        let seen = match to {
            command::To::Seen(seen) => {
                self.counts_since(seen)?;
                Some(seen)
            }
            command::To::Now(_) => None,
        };
        let panes = self.panes_of(Some(to.target()))?;
        let many = !matches!(to.target(), AnyRef::Pane(_));
        // Inputs its connection kept go from it to each pane asked.
        let body = self.body_token();
        let mut asks = Vec::new();
        let mut asked = Vec::new();
        for id in panes {
            let what = Ask::Keys {
                inputs: if body.is_some() {
                    Vec::new()
                } else {
                    inputs.to_vec()
                },
                seen,
                body: body.map(|(_, token)| token),
            };
            match self.ask_pane(id, what, now) {
                Ok(ask) => {
                    asks.push((id, ask));
                    asked.push(id);
                }
                // A pane none can reach refuses as it would.
                Err(error) if many => asks.push((id, self.refused(error))),
                Err(error) => return Err(error),
            }
        }
        if let Some((body, token)) = body {
            self.deliveries.push((body, token, asked));
        }
        let form = match many {
            true => Form::Many,
            false => Form::One,
        };
        Ok(Pending::new(asks, form))
    }

    /// `error`, as the answer of a question no pane was asked.
    fn refused(&mut self, error: Error) -> u64 {
        self.asked_pane = self.asked_pane.wrapping_add(1);
        let ask = self.asked_pane;
        let refusal = crate::link::Refusal::from(error);
        self.answers.insert(ask, Answered::Refused(refusal));
        ask
    }

    /// The layout with a new pane `beside` `target`, and the size the new
    /// pane starts at: the one settling gives it, its place in that layout
    /// on its tab's effective size, which must have room for it.
    fn split_size(&self, target: PaneId, beside: Beside) -> Result<(Split, Size), Error> {
        let new = self.ids.clone().pane()?;
        let add = |root: &mut Tree| match beside {
            Beside::Split(axis, shares) => {
                let shares = shares.map_or(Shares::HALVED, |(a, b)| Shares::new(a, b));
                root.split((target, new), (axis, Side::After), shares)
            }
            Beside::Stack => root.stack((target, new)),
        };
        let split = (self.parts.plan_split(target, add)).map_err(|NoPane| Error::NoPane(target))?;
        self.room(split.tab(), split.root(), None)?;
        let tab = self.tab(split.tab()).ok_or(Error::NoTab(split.tab()))?;
        let area = self.effective(tab, split.root(), None);
        let rect = layout::place(split.root(), area).rect(new);
        let size = rect.and_then(|r| Size::new(r.h(), r.w()).ok());
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
        match self.parts.panes().get(&pane).and_then(Pane::pid) {
            Some(pid) => fuxix::process::cwd_observed(pid, budget),
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
        let (pane, begin) = self.new_pane(&mut ids, new)?;
        let place = Place {
            ws,
            tab,
            pane: pane.id(),
        };
        if (self.parts)
            .add_workspaces(parent, made, (tab, tab_name, fit), pane)
            .is_err()
        {
            return Err(parent.map_or(Error::NoWorkspaceGiven, Error::NoWorkspace));
        }
        self.ids = ids;
        self.start_pane(place.pane, begin, now)?;
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
        let (pane, begin) = self.new_pane(&mut ids, new)?;
        let made = Place {
            ws: id,
            tab,
            pane: pane.id(),
        };
        if self.parts.add_tab((id, tab), name, fit, pane).is_err() {
            return Err(Error::NoWorkspace(id));
        }
        self.ids = ids;
        self.start_pane(made.pane, begin, now)?;
        Ok(made)
    }

    /// A new pane, its ID taken from `ids`, running what `new` says.
    fn new_pane(&self, ids: &mut Ids, new: NewPane<'_>) -> Result<(Pane, Begin), Error> {
        let start = new.start;
        let launch = pane::Launch {
            argv: start.argv().clone(),
            cwd: new.cwd.to_path_buf(),
        };
        let shell = matches!(start, Start::Shell(..));
        let runs = match shell {
            true => Runs::Shell(launch.clone()),
            false => Runs::Explicit(launch.clone()),
        };
        let id = ids.pane()?;
        let pane = Pane::new(id, runs, new.size, new.options);
        let draws = match new.cell {
            Some(cell) => crate::link::Draws::Cell(cell.width(), cell.height()),
            None => crate::link::Draws::Text,
        };
        let begin = Begin {
            id,
            run: self.run,
            shell,
            argv: launch.argv,
            cwd: launch.cwd,
            size: new.size,
            options: new.options,
            hosting: Hosting {
                draws,
                ..Hosting::default()
            },
            socket: self.launch.clone(),
            exe: self.exe.clone(),
        };
        Ok((pane, begin))
    }

    /// Pane `id`'s link, its pane placed: what it told as it began is
    /// taken in.
    fn linked(&mut self, id: PaneId, mut link: Link, now: Instant) {
        match &mut link {
            Link::Local(host) => Link::told(id, host, &mut self.inbox),
            Link::Remote(remote) => {
                let first = std::mem::take(&mut remote.first);
                self.inbox.extend(first.into_iter().map(|said| (id, said)));
                // What else came as it began, read already, waits for no poll.
                while let Ok(Some(report)) = remote.end.next_message() {
                    self.inbox.push_back((id, report));
                }
            }
        }
        self.links.insert(id, link);
        self.pump(now);
    }

    /// Ends a pane's link, its pane closed, by fux if `closed`: its
    /// process is told, and kept till it ends (`Session::ending`).
    fn unlink(&mut self, id: PaneId, mut link: Link, closed: bool, now: Instant) {
        link.send(id, ToPane::Close { closed }, now, &mut self.inbox);
        self.ending.push((id, link));
    }

    /// Lets go of every link of a closed pane whose process ended, or is
    /// held here and done; and of their questions unanswered, which are
    /// `indeterminate`.
    pub fn reap_ending(&mut self) {
        let (done, ending): (Vec<_>, Vec<_>) = std::mem::take(&mut self.ending)
            .into_iter()
            .partition(|(_, link)| match link {
                Link::Remote(remote) => remote.closed && (remote.ended.is_some() || !remote.child),
                Link::Local(host) => host.finished(),
            });
        self.ending = ending;
        for (id, link) in done {
            // Its program ended with it: its handle's copy is let go.
            if let Link::Remote(Remote {
                adopted: Some(pid), ..
            }) = link
            {
                fuxix::process::release(pid);
            }
            let left: Vec<u64> = (self.asks.iter())
                .filter(|(_, pane)| **pane == id)
                .map(|(ask, _)| *ask)
                .collect();
            for ask in left {
                self.asks.remove(&ask);
            }
        }
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
        let result = self.transact(now, |s| s.attach_checked(attach, now));
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
        let attached = ViewChange::Attached {
            kind: view.kind,
            read_only: view.read_only,
        };
        // The pane it runs in, if any: the one whose program it descends
        // from, or whose session it is in, as the OS says (a parent between
        // them may have gone). A headless client shows on no terminal, so on
        // none.
        let client = attach.ancestry.first();
        let session = client.and_then(|pid| fuxix::process::Pid::from_u32(*pid));
        let session = session.and_then(fuxix::process::session);
        let leads = |pane: &Pane| {
            let Some(leader) = pane.pid() else {
                return false;
            };
            let descends = attach.ancestry.contains(&leader.as_u32());
            !attach.headless && (descends || session == Some(leader))
        };
        let runs_in = self.parts.panes().iter().find(|(_, pane)| leads(pane));
        view.runs_in = runs_in.map(|(id, _)| *id);
        self.views.insert(id, view);
        self.parts.attach(id, at);
        self.view_event(id, attached);
        self.ask(id, crate::outer::QUERIES);
        self.settle(now);
        Ok(id)
    }

    /// Client `id` detaches, for `why`.
    pub fn detach(&mut self, id: ClientId, why: Detach, now: Instant) {
        if self.views.contains_key(&id) {
            self.view_event(id, ViewChange::Detached(why));
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
        let (rows, cols) = size;
        self.view_event(id, ViewChange::Resized { rows, cols });
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
    /// `out`, whatever it held, reusing its buffers: laid out on its
    /// effective size (`effective`) at the top left of the view's pane
    /// area, or on that area if it is smaller.
    pub fn placement_into(&self, view: &View, out: &mut Placement) {
        match self.shown_tab(view.id) {
            Some(tab) => {
                let size = self.effective(tab, tab.root(), None);
                let area = view.pane_area().split(Axis::Horizontal, size.w()).0;
                let area = area.split(Axis::Vertical, size.h()).0;
                lay_out((tab.root(), Some(view.id)), tab.zoomed(), area, out);
            }
            None => out.clear(),
        }
    }

    /// The area tab `tab` laid out as `root` is laid out on, its effective
    /// size, with `joining` about to show it too: a fixed tab's own size;
    /// else the smallest pane area of the clients showing it that size its
    /// panes, if every pane fits there; else its stored size, the last
    /// that did.
    fn effective(&self, tab: &Tab, root: &Tree, joining: Option<ClientId>) -> Rect {
        let stored = tab.fit().area();
        if let Fit::Fixed(_) = tab.fit() {
            return stored;
        }
        let joining = (joining.and_then(|c| self.views.get(&c))).filter(|v| Self::sizes(v, root));
        let smallest = (self.sizing(tab.id(), root).chain(joining))
            .map(View::pane_area)
            .map(|area| (area.h(), area.w()))
            .reduce(|(rows, cols), (h, w)| (rows.min(h), cols.min(w)))
            .map(|(rows, cols)| Rect::screen(rows, cols));
        let fits = |area: &Rect| *area == stored || layout::shows_all(root, *area);
        smallest.filter(fits).unwrap_or(stored)
    }

    /// Where each pane of tab `tab` is in it, as its PTY is sized
    /// (`sized_on`): its row and column, rows and columns.
    fn sized(&self, tab: &Tab) -> Placement {
        let mut placed = Placement::default();
        let area = self.effective(tab, tab.root(), None);
        sized_on(tab.root(), tab.zoomed(), area, &mut placed);
        placed
    }

    /// Tab `tab`'s effective size (`effective`), as it is laid out now.
    fn area_of(&self, tab: TabId) -> Option<Rect> {
        let tab = self.tab(tab)?;
        Some(self.effective(tab, tab.root(), None))
    }

    /// What a new tab is laid out on while no client shows it: `size`, if
    /// given; else the caller's own screen, if it sizes panes; else the
    /// effective size of the caller's tab. Never the screen of a client
    /// that does not show the caller's tab: a caller with neither, and no
    /// size given, is refused.
    fn new_fit(&self, size: command::Size, here: Here) -> Result<Fit, Error> {
        if let Some(size) = size {
            return Ok(Fit::Fixed(size.size()));
        }
        let own = (here.client().and_then(|c| self.views.get(&c)))
            .filter(|view| view.sizes())
            .map(View::pane_area);
        let tab = || match here {
            Here::Client(_, place) => self.area_of(place.tab),
            Here::Cli(..) => {
                (here.pane().and_then(|p| self.locate(p))).and_then(|(_, tab)| self.area_of(tab))
            }
        };
        let size = (own.or_else(tab)).and_then(|area| Size::new(area.h(), area.w()).ok());
        let why = "the caller has no screen that sizes panes and no tab, and no size was given";
        (size.map(Fit::Clients)).ok_or_else(|| {
            fail(
                Raise::Size,
                format!("{why}: give a size (`COLS ROWS size`)"),
            )
        })
    }

    /// The clients showing tab `tab` that size it laid out as `root`.
    fn sizing<'a>(&'a self, tab: TabId, root: &'a Tree) -> impl Iterator<Item = &'a View> {
        (self.views.values()).filter(move |v| self.shows(v.id, tab) && Self::sizes(v, root))
    }

    /// Whether `view` sizes a tab it shows laid out as `root`: not if its
    /// client runs in one of its panes (`View::runs_in`), whose terminal
    /// would follow what it is sized to. Settling asks it of the tab's
    /// layout and `room` of the one proposed, so what `room` admits is what
    /// settling lays out.
    fn sizes(view: &View, root: &Tree) -> bool {
        view.sizes() && view.runs_in.is_none_or(|pane| !root.contains(pane))
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
                self.view_event(*client, ViewChange::Screen(None));
            }
        }
        self.outbox.extend(ended);
        // After the repairs: a screen a close ended says less than this.
        self.tell_displaced();
        self.size_panes(now);
        self.host_panes(now);
        self.pump(now);
        if let Err(error) = self.commit() {
            log!("{error}");
        }
    }

    /// Tells each client a close moved, by a request it did not make, what
    /// closed and where it is now (`Parts::take_displaced`), until its next
    /// key, which no program's alert replaces.
    fn tell_displaced(&mut self) {
        for (client, told) in self.parts.take_displaced() {
            if let Some(view) = self.views.get_mut(&client) {
                view.notify(told, false);
                view.notice.iter_mut().for_each(|n| n.kept = true);
            }
        }
    }

    /// Its run ends: its record and its fux go, so no server takes back the
    /// run, or the panes still ending; none is written again.
    pub fn unrecord(&mut self) {
        if let Some(socket) = self.launch.as_deref() {
            let panes = crate::socket::panes(socket);
            for kept in ["run", "exe"] {
                let _ = std::fs::remove_file(panes.join(kept));
            }
        }
        self.recording = false;
    }

    /// Its run's record, written whole if it changed (`record`): before
    /// anything that changed it is told (D-124 (3)). One that can't be
    /// written fails, and the change it was for is undone (`transact`);
    /// outside a change, a pane's end say, it is logged, and the record
    /// stays as it was: a server taking the run back finds such a pane
    /// gone.
    fn commit(&mut self) -> Result<(), Error> {
        let Some(socket) = self.launch.as_ref().filter(|_| self.recording) else {
            return Ok(());
        };
        let record = crate::record::Record {
            run: self.run,
            ids: self.ids.given(),
            workspaces: self.parts.record(),
        };
        let bytes = fux_wire::wire::encode(&record);
        if bytes == self.recorded {
            return Ok(());
        }
        let path = crate::socket::panes(socket).join("run");
        match crate::record::write(&path, &bytes) {
            Ok(()) => {
                self.recorded = bytes;
                Ok(())
            }
            Err(error) => {
                let why = format!(
                    "the run's record, {}, could not be written ({error}): nothing changed",
                    path.display()
                );
                if self.staged.is_some() {
                    self.unrecorded = Some(fail(Raise::Client, why.clone()));
                }
                Err(fail(Raise::Client, why))
            }
        }
    }

    /// Makes a change (`change`), told only once its record is written: one
    /// that can't be is undone, as though never made, and fails. What the
    /// change tells its panes waits till then.
    fn transact<T>(
        &mut self,
        now: Instant,
        change: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        // Only a run with a record has one to write first: a session that
        // starts no programs (the tests', the tools') records nothing.
        if self.staged.is_some() || self.launch.is_none() || !self.recording {
            return change(self);
        }
        let undo = Undo {
            structure: self.parts.structure(),
            ids: self.ids.clone(),
            views: self.views.clone(),
            journal: self.parts.journal().mark(),
            outbox: self.outbox.len(),
            asked: self.asked_pane,
        };
        self.staged = Some(Vec::new());
        let mut done = change(self);
        if self.unrecorded.is_none()
            && let Err(error) = self.commit()
        {
            done = Err(error);
        }
        let staged = self.staged.take().unwrap_or_default();
        match self.unrecorded.take() {
            Some(error) => {
                self.roll_back(undo, now);
                Err(error)
            }
            None => {
                for (id, message) in staged {
                    self.tell_pane(id, message, now);
                }
                self.closed.clear();
                for (id, link, closed) in std::mem::take(&mut self.closing) {
                    self.unlink(id, link, closed, now);
                }
                done
            }
        }
    }

    /// A change undone (`transact`): the tree, ids, views and names as they
    /// were, what it journaled, queued for clients or asked of panes let go
    /// untold, and the panes it made closed.
    fn roll_back(&mut self, undo: Undo, now: Instant) {
        // What it closed comes back whole, untold.
        self.parts.put_back(std::mem::take(&mut self.closed));
        for (id, link, _) in std::mem::take(&mut self.closing) {
            self.links.insert(id, link);
        }
        let made = self.parts.restructure(undo.structure);
        self.ids = undo.ids;
        self.views = undo.views;
        self.parts.truncate_journal(undo.journal);
        self.outbox.truncate(undo.outbox);
        self.asks.retain(|ask, _| *ask <= undo.asked);
        self.answers.retain(|ask, _| *ask <= undo.asked);
        self.inbox.retain(|(id, _)| !made.contains(id));
        for id in made {
            if let Some(link) = self.links.remove(&id) {
                self.unlink(id, link, true, now);
            }
        }
        for view in self.views.values_mut() {
            view.dirty = true;
        }
        if let Err(error) = self.commit() {
            log!("{error}");
        }
    }

    /// Pane `id`, placed, starts: its place and id written in the record
    /// first, so that once its program runs a crash finds it there
    /// (D-124 (3)); a crash before leaves a pane the next server finds
    /// gone. One that can't start fails, and undoes the change.
    fn start_pane(&mut self, id: PaneId, begin: Begin, now: Instant) -> Result<(), Error> {
        self.commit()?;
        let link = match (&self.launch, &self.exe) {
            (Some(socket), Some(exe)) => {
                let at = crate::socket::panes(socket).join(id.number().to_string());
                Link::start(exe, &at, begin)
            }
            _ => crate::host::Host::begin(begin, now).map(|host| Link::Local(Box::new(host))),
        };
        match link {
            Ok(link) => {
                self.linked(id, link, now);
                Ok(())
            }
            Err(why) => {
                match self.staged.is_some() {
                    true => self.unrecorded = Some(fail(Raise::Start, why.clone())),
                    false => self.close_pane(id, None, now),
                }
                Err(fail(Raise::Start, why))
            }
        }
    }

    /// Takes back the run `record` holds, whose server crashed (D-124):
    /// each of its panes still waiting, at its place, with its name, its
    /// ids and versions going on. One whose own process is gone stays as
    /// its record keeps it: ended, if it had, else its program lost as
    /// though its process had died under this server (`pane_lost`), and
    /// then its role's rule, as a shell that ended meanwhile closes. One
    /// that answers but can't be taken refuses the run: it may still run.
    /// Its journal starts afresh, this server's own (D-124 (5)).
    pub fn resume(&mut self, record: crate::record::Record, now: Instant) -> Result<(), Error> {
        let Some(socket) = self.launch.clone() else {
            return Err(fail(
                Raise::Start,
                "a session that starts no programs takes back none",
            ));
        };
        let panes = crate::socket::panes(&socket);
        self.run = record.run;
        self.ids = Ids::after(&record.ids);
        let mut copies = BTreeMap::new();
        let mut links = Vec::new();
        let mut lost = Vec::new();
        for recorded in crate::record::panes(&record.workspaces) {
            let id = recorded.id;
            let at = panes.join(id.number().to_string());
            let copy = match Link::take(&at, record.run) {
                Ok((link, greeting)) => {
                    links.push((id, link));
                    let mut copy = Pane::greeted(greeting);
                    if let Some(name) = &recorded.name {
                        copy.rename(name.clone());
                    }
                    copy
                }
                Err(crate::link::Untaken::Gone(why)) => {
                    log!("{id}: its own process is gone: {why}");
                    if recorded.ended.is_none() {
                        lost.push(id);
                    }
                    Pane::recorded(recorded)
                }
                // A pane that answers may still run: never taken for ended.
                Err(crate::link::Untaken::Unknown(why)) => {
                    return Err(fail(Raise::Start, format!("{id} answers, but: {why}")));
                }
            };
            copies.insert(id, copy);
        }
        self.parts = Parts::restore(self.entries, &record.workspaces, copies)?;
        for (id, link) in links {
            self.linked(id, link, now);
        }
        for id in lost {
            self.pane_lost(id, "its pane's own process ended while no server ran", now);
        }
        let shells: Vec<(PaneId, Ended)> = (self.parts.panes().values())
            .filter(|pane| matches!(pane.runs, Runs::Shell(_)))
            .filter_map(|pane| Some((pane.id(), pane.exit_status()?)))
            .collect();
        for (id, ended) in shells {
            let end = pane::End {
                ended,
                left: pane::Left::default(),
            };
            self.program_ended(id, end, now);
        }
        self.recorded.clear();
        self.settle(now);
        Ok(())
    }

    /// Each pane is told what its host's terminal said, as that changes
    /// (`outer`): its colours, palette and scheme, and what it draws, which
    /// sizes its terminal in pixels; and whether a client shows it. Its
    /// host's terminal reporting a scheme change tells it nothing new till
    /// the colours that come with it are in (`Session::tell_change`). The
    /// clients showing a pane told another host are painted again, as its
    /// pictures show only while its host draws sixel (`render::pictures`).
    fn host_panes(&mut self, now: Instant) {
        let ids: Vec<PaneId> = self.parts.panes().keys().copied().collect();
        for id in ids {
            let shown = !self.viewers(AnyRef::Pane(id)).is_empty();
            let hosting = self.hosting(id);
            let Some(pane) = self.parts.pane_mut(id) else {
                continue;
            };
            let told_shown = std::mem::replace(&mut pane.shown, shown) != shown;
            let hosting = hosting.filter(|h| pane.hosting.as_ref() != Some(h));
            if let Some(hosting) = &hosting {
                pane.hosting = Some(hosting.clone());
            }
            if told_shown {
                self.tell_pane(id, ToPane::Shown(shown), now);
            }
            if let Some(hosting) = hosting {
                for client in self.viewers(AnyRef::Pane(id)) {
                    if let Some(view) = self.views.get_mut(&client) {
                        view.dirty = true;
                    }
                }
                self.tell_pane(id, ToPane::Host(hosting), now);
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

    /// Sizes each pane's PTY to its place in its tab laid out on the
    /// tab's effective size (`effective`): each tab a client sizes, whose
    /// effective size follows its screens, and each whose layout changed.
    /// A tab that fits clients keeps its effective size as its stored one.
    /// A pane with no place there keeps its size.
    fn size_panes(&mut self, now: Instant) {
        let mut placed = std::mem::take(&mut self.placed);
        let mut tabs: BTreeSet<TabId> = (self.views.values())
            .filter_map(|view| {
                self.shown_tab(view.id)
                    .filter(|t| Self::sizes(view, t.root()))
            })
            .map(Tab::id)
            .collect();
        tabs.extend(self.parts.take_changed());
        let mut sizes = BTreeMap::new();
        for tab in tabs {
            let Some(area) = self.area_of(tab) else {
                continue;
            };
            if let Ok(size) = Size::new(area.h(), area.w()) {
                self.parts.shown_on(tab, size);
            }
            if let Some(t) = self.tab(tab) {
                sized_on(t.root(), t.zoomed(), area, &mut placed);
                let size = |r: &Rect| Size::new(r.h(), r.w()).ok();
                sizes.extend((placed.panes.iter()).filter_map(|(p, r)| Some((*p, size(r)?))));
            }
        }
        self.placed = placed;
        self.tell_laid_out(&sizes);
        // Each pane is told a size it was not told yet; it resizes as it
        // reads its program's output, and tells so (`Told::Resized`).
        for (id, size) in sizes {
            if let Some(pane) = self.parts.pane_mut(id)
                && pane.ask_size(size)
            {
                let actor = self.parts.journal().current_actor();
                self.tell_pane(id, ToPane::Resize { size, actor }, now);
            }
        }
    }

    /// Journals `laid_out` for each tab laid out otherwise than at the last
    /// settle, at the same revision: a pane of it at another size (`sized`,
    /// as this settle sizes them, else as it is), or another host. A new
    /// revision was journaled as it was made, and a new tab is told
    /// `created`.
    fn tell_laid_out(&mut self, sized: &BTreeMap<PaneId, Size>) {
        let size = |pane: PaneId| {
            sized
                .get(&pane)
                .copied()
                .or(self.pane(pane).ok().map(Pane::size))
        };
        let laid: BTreeMap<TabId, Laid> = (self.parts.tabs_in(None).into_iter())
            .map(|(_, tab)| {
                let id = tab.id();
                let panes = tab.root().panes().into_iter();
                let sizes = panes.filter_map(|p| Some((p, size(p)?))).collect();
                (id, (tab.revision(self.run), sizes, self.host(Some(id))))
            })
            .collect();
        let moved = |(id, now): (&TabId, &Laid)| {
            let was = self.laid.get(id)?;
            (was != now && was.0 == now.0).then_some(*id)
        };
        let moved: Vec<TabId> = laid.iter().filter_map(moved).collect();
        for tab in moved {
            self.parts.laid_out(tab);
        }
        self.laid = laid;
    }

    fn touch(&mut self) {
        self.views.values_mut().for_each(|view| view.dirty = true);
    }

    // ------------------------------------------------------------- closing

    /// Removes a pane, its process told to end. A tab its closing empties
    /// closes too, as does a workspace left with no tabs.
    pub fn close_pane(&mut self, id: PaneId, why: Option<String>, now: Instant) {
        // Who sees the pane is known before its tab can go: a tab its
        // closing empties takes its viewers elsewhere.
        let viewers = self.viewers(AnyRef::Pane(id));
        // Closed for its program's end (a reason given), or by fux.
        let closed = why.is_none();
        let close = match closed {
            true => crate::journal::Close::Killed,
            false => crate::journal::Close::Exited,
        };
        let mut notice = None;
        if let Some(pane) = self.parts.close_pane(id, close) {
            // A program whose terminal could not be read says so as its
            // pane closes, and as the server stops if it was the last.
            let failed = (pane.read_error.as_ref())
                .map(|error| format!("{id}: reading program terminal: {error}"));
            let status =
                why.map(|why| format!("{} {why}", fux_wire::words::labelled(id, &pane.label())));
            let told = status
                .into_iter()
                .chain(failed.clone())
                .collect::<Vec<_>>()
                .join("; ");
            if !told.is_empty() {
                if failed.is_some() && self.parts.panes().is_empty() {
                    self.shut_down(told.clone());
                }
                notice = Some((told, failed.is_some()));
            }
            self.end_pane(id, closed, now);
            // A change's close is undone if its record can't be written.
            if self.staged.is_some() {
                self.closed.push(pane);
            }
        }
        // Before it settles: one a close moved is told that instead.
        if let Some((told, failed)) = notice {
            for client in viewers {
                self.notify_to(client, told.clone(), failed);
            }
        }
        self.after_close(now);
    }

    /// Removes a tab and whatever panes are in it, killed as `killing`
    /// kills it (`Parts::remove_tab`).
    fn remove_tab(&mut self, tab: TabId, killing: Killing, now: Instant) {
        for gone in self.parts.remove_tab(tab, killing) {
            self.end_pane(gone.id(), true, now);
        }
    }

    /// Ends pane `id`'s process, its pane closed (by fux if `closed`).
    fn end_pane(&mut self, id: PaneId, closed: bool, now: Instant) {
        if let Some(link) = self.links.remove(&id) {
            match self.staged.is_some() {
                true => self.closing.push((id, link, closed)),
                false => self.unlink(id, link, closed, now),
            }
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

    /// Output from pane `id`'s program, as a pane held here reads it: for
    /// the tests and the tools.
    #[cfg(any(test, feature = "in-process"))]
    pub fn output(&mut self, id: PaneId, bytes: &[u8], now: Instant) {
        if let Some(Link::Local(host)) = self.links.get_mut(&id) {
            host.output(bytes, now);
            Link::told(id, host, &mut self.inbox);
        }
        self.pump(now);
    }

    /// Pane `id`, held here, to change as the tests and the tools do; what
    /// it then tells is taken in.
    #[cfg(any(test, feature = "in-process"))]
    pub fn host_mut<T>(
        &mut self,
        id: PaneId,
        change: impl FnOnce(&mut crate::host::Host) -> T,
        now: Instant,
    ) -> Option<T> {
        let Some(Link::Local(host)) = self.links.get_mut(&id) else {
            return None;
        };
        let changed = change(host);
        Link::told(id, host, &mut self.inbox);
        self.pump(now);
        Some(changed)
    }

    /// Each pane held here, changed with `change`, as the tools change them;
    /// what each then tells is taken in.
    #[cfg(any(test, feature = "in-process"))]
    pub fn each_host_mut(&mut self, mut change: impl FnMut(&mut crate::host::Host), now: Instant) {
        let ids: Vec<PaneId> = self.links.keys().copied().collect();
        for id in ids {
            self.host_mut(id, &mut change, now);
        }
    }

    /// Pane `id`'s program, held here, ended so: as the tools end one.
    #[cfg(any(test, feature = "in-process"))]
    pub fn exited(&mut self, id: PaneId, ended: Ended, now: Instant) {
        self.host_mut(id, |host| host.exited(ended, now), now);
    }

    /// Everything the session waits for, each with when it is due: the
    /// server polls no longer than the soonest, and fires those due
    /// (`Session::fire`).
    pub fn timers(&self) -> impl Iterator<Item = (Instant, Timer)> + '_ {
        let panes = self.links.iter().flat_map(|(id, link)| {
            let timers = match link {
                Link::Local(host) => Some(host.timers()),
                Link::Remote(_) => None,
            };
            let id = *id;
            (timers.into_iter().flatten()).map(move |(at, timer)| (at, Timer::Pane(id, timer)))
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
            Timer::Pane(id, timer) => {
                if let Some(Link::Local(host)) = self.links.get_mut(&id) {
                    host.fire(timer, now);
                    Link::told(id, host, &mut self.inbox);
                }
                self.pump(now);
            }
        }
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
            self.end_pane(pane.id(), true, now);
        }
        self.pump(now);
    }

    // ------------------------------------------------------------ commands

    /// Begins a request from a client: one that waits gives its wait,
    /// which the server asks for its answer until it has one. Event reads
    /// use the same bounded wait; commands run at once.
    pub fn begin(&mut self, request: Request, origin: &Origin, now: Instant) -> Begun {
        self.transact(now, |s| s.begin_request(request, origin, now))
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
            Request::Read { client, read } => {
                self.views.get(&client).ok_or(Error::NoClient(client))?;
                self.asked = self.asked.wrapping_add(1);
                let asked = self.asked;
                (self.outbox).push(Outgoing::To(client, ToAttached::Read(asked, read)));
                return Ok(Begun::Waiting(Box::new(Waiting::Read { client, asked })));
            }
            Request::Do(command) => {
                return Ok(match self.run(&command, origin, now) {
                    Done::Now(reply) => Begun::Done(reply),
                    Done::Asked(pending) => self.answered(Waiting::Asked(pending), now),
                });
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
                let reader = crate::journal::Reader::new(
                    scope,
                    self.parts.journal(),
                    self.journal_run,
                    from,
                )
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
    fn answered(&mut self, mut waiting: Waiting, now: Instant) -> Begun {
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
    /// One a pane answers is answered here only by a pane held here.
    pub fn run_command(&mut self, command: &Command, origin: &Origin, now: Instant) -> Reply {
        match self.run(command, origin, now) {
            Done::Now(reply) => reply,
            Done::Asked(pending) => match self.answered(Waiting::Asked(pending), now) {
                Begun::Done(reply) => reply,
                Begun::Waiting(_) => Reply::failed(
                    fux_wire::code::Code::NotHere,
                    "its panes answer it in a server, not in this session",
                ),
            },
        }
    }

    /// Runs a command: its reply now, or the questions its panes answer.
    fn run(&mut self, command: &Command, origin: &Origin, now: Instant) -> Done {
        let actor = self.request_actor(origin);
        let previous = self.parts.actor(actor);
        let done = match self.asking(command, now) {
            Some(asked) => asked.map(Done::Asked),
            None => self.execute(command, origin, now).map(Done::Now),
        };
        let outcome = done.unwrap_or_else(|error| Done::Now(failure(error, origin, self)));
        if !command.shows_nothing_new() {
            self.changes = self.changes.wrapping_add(1);
            self.touch();
        }
        self.settle(now);
        self.parts.actor(previous);
        outcome
    }

    /// The commands a pane does itself, asked of it: none, for another
    /// command.
    fn asking(&mut self, command: &Command, now: Instant) -> Option<Result<Pending, Error>> {
        let one = |session: &mut Self, target, what| {
            let ask = session.ask_pane(target, what, now)?;
            Ok(Pending::new(vec![(target, ask)], Form::One))
        };
        Some(match *command {
            Command::SendKeys { to, ref inputs } => self.send_keys(to, inputs, now),
            Command::SendMouse {
                target,
                mouse,
                row,
                col,
            } => one(self, target, Ask::Mouse { mouse, row, col }),
            Command::Signal { target, signal } => one(self, target, Ask::Signal(signal)),
            Command::ClearHistory { target } => one(self, target, Ask::ClearHistory),
            Command::HistoryBudget { target, bytes } => one(self, target, Ask::HistoryBytes(bytes)),
            Command::Respawn {
                target,
                kill,
                ref argv,
            } => {
                let what = Ask::Respawn {
                    kill,
                    argv: argv.clone(),
                };
                let asked = self.ask_pane(target, what, now);
                if let (Ok(_), Some(copy)) = (&asked, self.parts.pane_mut(target)) {
                    copy.respawning = true;
                }
                asked.map(|ask| Pending::new(vec![(target, ask)], Form::Respawn(argv.clone())))
            }
            Command::CapturePane {
                target,
                history,
                view,
            } => self.capture(target, history, view, now),
            Command::Inspect { .. }
            | Command::KillServer
            | Command::NewTab { .. }
            | Command::Split { .. }
            | Command::Notify { .. }
            | Command::Resize(_)
            | Command::Kill { .. }
            | Command::Rename { .. }
            | Command::Move { .. }
            | Command::SwapPane { .. }
            | Command::Arrange { .. }
            | Command::ResizePane { .. }
            | Command::Zoom { .. }
            | Command::Reorder { .. }
            | Command::Resolve { .. }
            | Command::Members { .. }
            | Command::Client { .. } => return None,
        })
    }

    /// `capture` of pane `target`, asked of it: its own screen, live.
    fn capture(
        &mut self,
        target: PaneId,
        history: Option<usize>,
        view: command::CaptureView,
        now: Instant,
    ) -> Result<Pending, Error> {
        if let Some(since) = view.since {
            self.counts_since(since)?;
            if since.pane != target {
                let why = format!("{since} is {}'s version, not {target}'s", since.pane);
                return Err(fail(Raise::BadValue, why));
            }
        }
        let (_, tab) = self.locate(target).ok_or(Error::NoPane(target))?;
        // One in a process of its own is held there, for its asker's
        // connection to fetch: the server never holds it (D-123).
        let hold = matches!(self.links.get(&target), Some(Link::Remote(_)));
        let what = Ask::Capture {
            history,
            view,
            tab,
            hold,
        };
        let ask = self.ask_pane(target, what, now)?;
        Ok(Pending::new(vec![(target, ask)], Form::Capture))
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

    pub(crate) fn view_event(&mut self, client: ClientId, change: ViewChange) {
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
                let root = layout::from_nodes(nodes, &tab.root().panes())?;
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
            &Command::Split {
                beside,
                target,
                ref start,
                ref cwd,
                options,
            } => self.split(
                target,
                beside,
                (start, options),
                cwd.as_deref(),
                origin,
                now,
            ),

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
            &Command::ResizePane { target, by } => self.resize_pane(target, by),
            &Command::Zoom { target } => {
                self.parts.zoom(target).ok_or(Error::NoPane(target))?;
                Ok(Reply::default())
            }
            &Command::Reorder { ref target, toward } => {
                let target = self.there(*target)?;
                self.reorder(target, toward).map(|()| Reply::default())
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
            // What a pane does itself is asked of it (`asking`), never here.
            Command::SendKeys { .. }
            | Command::SendMouse { .. }
            | Command::Signal { .. }
            | Command::Respawn { .. }
            | Command::ClearHistory { .. }
            | Command::HistoryBudget { .. }
            | Command::CapturePane { .. } => Err(fail(
                Raise::NotHere,
                "a pane's own process does this, asked of it",
            )),
        }
    }

    /// What `new-tab` does before its pane starts: the directory it starts
    /// in found, and its tab's fit and the size settling gives its pane,
    /// its fit's: in the cell the terminal of a client asking, which then
    /// shows it, said; else in none, as no terminal draws it.
    fn opening(
        &mut self,
        (dir, size): (Option<&Path>, command::Size),
        here: Here,
    ) -> Result<(PathBuf, Fit, Starts), Error> {
        let cwd = self.start_dir(here, dir, None)?;
        let fit = self.new_fit(size, here)?;
        let cell = (here.client().and_then(|c| self.views.get(&c))).and_then(|v| v.terminal.cell);
        Ok((cwd, fit, (fit.size(), cell)))
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

    /// `split` and `stack`: a new pane `beside` `target`, running `start`
    /// in `dir`.
    fn split(
        &mut self,
        target: PaneId,
        beside: Beside,
        (start, options): (&Start, command::PaneOptions),
        dir: Option<&Path>,
        origin: &Origin,
        now: Instant,
    ) -> Result<Reply, Error> {
        let here = self.here(origin)?;
        let target = self.pane(target)?.id();
        let cwd = self.start_dir(here, dir, Some(target))?;
        // The splitting client follows the new pane, if it shows its tab;
        // from the CLI, the clients that focus the split one, while a stack
        // made there shows no client its new child.
        let tab = self.locate(target).map(|(_, tab)| tab);
        let shows = |c: &ClientId| tab.is_some_and(|t| self.shows(*c, t));
        let shown = |c: &ClientId| self.focused(*c) == Some(target) && beside != Beside::Stack;
        let followers: Vec<ClientId> = match here {
            Here::Client(client, _) => [client].into_iter().filter(shows).collect(),
            Here::Cli(..) => self.views.keys().copied().filter(shown).collect(),
        };
        // Its tab found (by `split_size`) before the program starts, which
        // nothing can then leave running.
        let (split, size) = self.split_size(target, beside)?;
        let mut ids = self.ids.clone();
        let new = NewPane::new(start, &cwd, options, (size, self.graphics(target).cell()));
        let (pane, begin) = self.new_pane(&mut ids, new)?;
        let new = pane.id();
        if self.parts.split(pane, split).is_err() {
            return Err(Error::NoPane(target));
        }
        self.ids = ids;
        self.start_pane(new, begin, now)?;
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
        let killing = match scope {
            None => Killing::All,
            Some(AnyRef::Workspace(ws)) => Killing::Within(ws),
            Some(AnyRef::Tab(_) | AnyRef::Pane(_)) => Killing::None,
        };
        for tab in self.tabs_of(scope)? {
            self.remove_tab(tab, killing, now);
        }
        self.after_close(now);
        Ok(())
    }

    /// `resize-pane`: a border of pane `pane` moved, or the pane made a
    /// size, its tab laid out on its effective size.
    fn resize_pane(&mut self, pane: PaneId, by: PaneResize) -> Result<Reply, Error> {
        let (_, tab) = self.locate(pane).ok_or(Error::NoPane(pane))?;
        let area = self.area_of(tab).ok_or(Error::NoPane(pane))?;
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
                // Inputs its asker's connection kept go from it to the
                // client's connection, which sends them its client: the
                // asker's answers once they are there.
                if let Some((body, token)) = self.body_token() {
                    self.outbox.push(Outgoing::Keys(client, token));
                    // It answers once it took them, as it takes what came
                    // before in order.
                    self.asked = self.asked.wrapping_add(1);
                    let asked = self.asked;
                    let read = ToAttached::Read(asked, command::ClientRead::Keys);
                    self.outbox.push(Outgoing::To(client, read));
                    let keys = crate::link::Making::Keys {
                        client,
                        body,
                        token,
                        asked,
                    };
                    self.making = Some(keys);
                    return Ok(Reply::default());
                }
                let keys = ToAttached::Keys(inputs.clone());
                self.outbox.push(Outgoing::To(client, keys));
                return Ok(PaneResults::Many(Vec::new()).into());
            }
            ClientAction::Detach => {
                self.detach(client, Detach::Detached, now);
                let exit = ToAttached::Exit("detached");
                self.outbox.push(Outgoing::To(client, exit));
                return Ok(Reply::default());
            }
            ClientAction::Capture => {
                view()?;
                let scene = crate::render::scene(self, client).ok_or(Error::NoClient(client))?;
                // Its panes in processes of their own send their screens to
                // the asker's connection, which composes it (D-123).
                if self.launch.is_some() {
                    self.making = Some(crate::link::Making::Client {
                        client,
                        scene: Box::new(scene),
                    });
                    return Ok(Reply::default());
                }
                let mut grid = crate::render::Grid::default();
                crate::render::compose_scene(&scene, &|id| self.screen(id), &mut grid)
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
            AnyRef::Pane(p) => self.parts.rename_pane(p, name).ok_or(Error::NoPane(p))?,
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

    /// The pane beside `from` toward `direction`, its tab laid out on its
    /// effective size.
    fn neighbor(&self, from: PaneId, direction: Direction, here: Here) -> Result<PaneId, Error> {
        let found = self.locate(from).and_then(|(_, tab)| {
            let area = self.area_of(tab)?;
            let placed = placed_from(self.root(tab)?, area, here.client(), from);
            layout::neighbor(&placed, from, direction)
        });
        found.ok_or(Error::NoNeighbor { from, direction })
    }

    fn move_pane(&mut self, target: PaneId, to: &MoveTo, here: Here) -> Result<(), Error> {
        let pane = self.pane(target)?.id();
        // A tab made for it fits clients at its tab's size until one shows it.
        let (from_ws, from) = self.locate(pane).ok_or(Error::NoPane(pane))?;
        let area = self.area_of(from).ok_or(Error::NoTab(from))?;
        let size = Size::new(area.h(), area.w()).map_err(|_| Error::NoTab(from))?;
        let fit = Fit::Clients(size);
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

    /// Refused unless tab `tab` laid out as `root` shows every pane on its
    /// effective size (`effective`), with `joining` about to show it: the
    /// size settling gives it, sized by the clients that will size it laid
    /// out so (`sizes`). So a split or a move hides no pane, and a dump can
    /// make every layout there is again.
    fn room(&self, tab: TabId, root: &Tree, joining: Option<ClientId>) -> Result<(), Error> {
        let area = self.effective(self.tab(tab).ok_or(Error::NoTab(tab))?, root, joining);
        if layout::shows_all(root, area) {
            return Ok(());
        }
        let (cols, rows) = (area.w(), area.h());
        let message = format!(
            "{tab} has no room to show every pane laid out so on its size, {cols}x{rows} \
             (`TAB COLS ROWS resize` makes it bigger)"
        );
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
    /// shown there before, or a pane's neighbour on its screen. What holds
    /// a pane is the stack nearest it in its tab's layout, whose child
    /// shown is a pane there (`Tree::first_shown`), else its tab, of
    /// whose panes those shown.
    fn step(&self, view: &View, from: AnyRef, way: Way) -> Result<AnyRef, Error> {
        let kind = from.kind();
        let root = |p| self.locate(p).and_then(|(_, tab)| self.root(tab));
        if let (AnyRef::Pane(p), Way::Step(_) | Way::Last) = (from, way)
            && let Some(stack) = root(p).and_then(|r| r.stack_of(p))
        {
            let children: Vec<&Tree> = stack.children().collect();
            let child = match way {
                Way::Step(toward) => {
                    let index = children.iter().position(|c| c.contains(p));
                    round(&children, index.unwrap_or(0), toward)
                }
                Way::Last | Way::Toward(_) => {
                    let last = stack.shown(view.id).last;
                    last.and_then(|i| children.get(i))
                }
            };
            let child = child.ok_or_else(|| fail(Raise::NoPane, "no previously shown pane"))?;
            return Ok(AnyRef::Pane(child.first_shown(view.id)));
        }
        let within = match from {
            AnyRef::Pane(p) => Some(AnyRef::Tab(self.locate(p).ok_or(Error::NoPane(p))?.1)),
            AnyRef::Tab(t) => Some(AnyRef::Workspace(self.tab_workspace(t)?)),
            AnyRef::Workspace(w) => {
                (self.parts.parent(w).ok_or(Error::NoWorkspace(w))?).map(AnyRef::Workspace)
            }
        };
        let siblings: Vec<AnyRef> = match (from, within) {
            (AnyRef::Pane(p), _) => (root(p).into_iter())
                .flat_map(|r| r.shown_panes(view.id))
                .map(AnyRef::Pane)
                .collect(),
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
                let found = self.locate(from).and_then(|(_, tab)| {
                    let (root, area) = (self.root(tab)?, self.area_of(tab)?);
                    let placed = placed_from(root, area, Some(view.id), from);
                    layout::neighbor(&placed, from, direction)
                });
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
    fn field(&self, target: AnyRef, field: &str) -> Result<Body, Error> {
        let no_field = || fail(Raise::NoField, format!("{target} has no field {field:?}"));
        if let AnyRef::Pane(id) = target {
            return self.observed_field(id, field)?.ok_or_else(no_field);
        }
        if field == "id" {
            return Ok(Body::Found(target));
        }
        if field == "panes" {
            let panes = self.panes_of(Some(target))?;
            return Ok(Body::Handles(panes.into_iter().map(AnyRef::Pane).collect()));
        }
        let value = match target {
            AnyRef::Pane(_) => return Err(no_field()),
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
/// the `zoomed` pane alone, filling it, if one is and it has room; else
/// what `client` is shown, or with none every pane (`layout::place_into`).
fn lay_out(
    (root, client): (&Tree, Option<ClientId>),
    zoomed: Option<PaneId>,
    area: Rect,
    out: &mut Placement,
) {
    match zoomed.filter(|_| !area.is_empty()) {
        Some(pane) => {
            out.clear();
            out.panes.push((pane, area));
        }
        None => layout::place_into(root, area, client, out),
    }
}

/// Where each pane of a tab laid out as `root` and zoomed on `zoomed` is in
/// `area`, into `out`, as its PTY is sized: every child of a stack on the
/// stack's rectangle (D-114), and the zoomed pane on the whole area.
fn sized_on(root: &Tree, zoomed: Option<PaneId>, area: Rect, out: &mut Placement) {
    layout::place_into(root, area, None, out);
    if let Some(zoomed) = zoomed.filter(|_| !area.is_empty()) {
        out.panes.retain(|(p, _)| *p != zoomed);
        out.panes.push((zoomed, area));
    }
}

/// Where each pane of a tab laid out as `root` is in `area` that `client`
/// is shown, or with none every pane, `from` among them: at the stacks that
/// hold it, the child holding it, as a zoom shows it to a client that chose
/// another, its choices kept.
fn placed_from(root: &Tree, area: Rect, client: Option<ClientId>, from: PaneId) -> Placement {
    let mut root = std::borrow::Cow::Borrowed(root);
    if let Some(client) = client {
        root.to_mut().reveal(client, from);
    }
    let mut placed = Placement::default();
    layout::place_into(&root, area, client, &mut placed);
    placed
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
            let now = Instant::now();
            self.host_mut(id, |host| host.exited(status, now), now);
        }
        pub(crate) fn settled(&mut self) {
            self.settle(Instant::now())
        }
        pub(crate) fn detach_client(&mut self, id: ClientId) {
            self.detach(id, crate::journal::Detach::Detached, Instant::now())
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
        /// Pane `id`'s screen made `size` alone, its place as it was, as a
        /// resize its screen refused leaves it.
        pub(crate) fn resize_screen(&mut self, id: fux_wire::id::PaneId, size: fux_vt::Size) {
            let now = Instant::now();
            let actor = fux_wire::events::Actor::Server;
            let resize = crate::link::ToPane::Resize { size, actor };
            self.host_mut(id, |host| host.handle(resize, now), now);
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
        // Windows' built-in ConPTY: a folder with no pinned one.
        #[cfg(windows)]
        let _ = fuxix::pty::load(std::path::Path::new("no-pinned-conpty"));
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
                    ToAttached::Paint(_)
                    | ToAttached::Exit(_)
                    | ToAttached::Welcome(..)
                    | ToAttached::Read(..),
                )
                | super::Outgoing::Keys(..)
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
                    | Request::NextEvents { .. }
                    | Request::Read { .. } => return self.0.ask(asked),
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
                decoder: fux_vt::keys::decode::Decoder::with_paste_limit(
                    fux_vt::keys::decode::Timing::new(35, 5000),
                    usize::MAX,
                ),
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
                    fux::screens::Dispatch::Fence | fux::screens::Dispatch::Answer(..) => {}
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
    use crate::pane::{Left, Process};
    use crate::process;
    use fux_wire::code::Code;
    use std::num::NonZeroU32;
    use std::time::Duration;

    /// Pane `id`'s host, held here, changed by `change`: what it tells
    /// then is taken in.
    fn host<T>(
        s: &mut Session,
        id: PaneId,
        change: impl FnOnce(&mut crate::host::Host) -> T,
    ) -> Result<T, String> {
        (s.host_mut(id, change, Instant::now())).ok_or_else(|| format!("{id} is held nowhere"))
    }

    /// What is queued for pane `id`'s program, taken as if written.
    fn queued(s: &mut Session, id: PaneId) -> Result<Vec<u8>, String> {
        host(s, id, |h| h.pane_mut().drain_input())
    }

    /// Kills and reaps every program the panes wait to end.
    fn reap(s: &mut Session) {
        let ids: Vec<PaneId> = s.links.keys().copied().collect();
        for id in ids {
            let _ = host(s, id, crate::host::Host::end_dying);
        }
        for (_, link) in &mut s.ending {
            if let Link::Local(host) = link {
                host.end_dying();
            }
        }
    }

    /// Gives pane `id` `process`, as a test's pane runs a child of the
    /// test's: the session starts none.
    fn give(s: &mut Session, id: PaneId, process: crate::pane::Process) -> Result<(), String> {
        host(s, id, |h| drop(h.set_process(process)))
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
                pane.facts.as_mut().ok_or("facts")?.program = Json::Object(vec![
                    ("pid".into(), pid.map_or(Json::Null, Json::Int)),
                    ("exited".into(), exited.clone()),
                    ("signal".into(), signal.clone()),
                ]);
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
    fn programs_alike_in_two_panes_are_two_programs() -> TestResult {
        let mut s = started()?;
        for _ in 0..2 {
            run(
                &mut s,
                "opts str agent in 80 24 size str /bin/cat program new-tab drop",
            )?;
        }
        for pane in [2, 3] {
            s.end_program(PaneId::of(pane), Ended::Exited(7));
        }
        let records = ["%2", "%3"].map(|p| output(&mut s, &format!("{p} str program get json")));
        let [two, three] = records.map(|r| r.unwrap_or_default());
        assert!(two.contains(r#""pane":"%2","generation":0"#), "{two}");
        assert_eq!(two.replace("%2", "%3"), three);
        // Each way of reading it names its pane by handle.
        for read in ["", "capture", "info str panes get first"] {
            let code = format!("%3 {read} str program get str pane get dup kind swap string");
            assert_eq!(output(&mut s, &code)?.trim(), "pane\n%3", "{read}");
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
                r#"{"event":"closed","pane":"%3","tab":"@3","workspace":"+5","why":"killed"}"#,
                r#"{"event":"closed","tab":"@3","workspace":"+5","why":"emptied"}"#,
                r#"{"event":"closed","workspace":"+5","parent":"+4","why":"emptied"}"#,
                r#"{"event":"closed","workspace":"+4","parent":"+3","why":"emptied"}"#,
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
                assert_eq!(pane.facts.is_some(), projection != Projection::Topology);
                assert_eq!(pane.cwd.is_some(), projection == Projection::Recipe);
                if let Some(facts) = &pane.facts {
                    assert!(facts.launch.shell);
                    let Json::Object(program) = &facts.program else {
                        return Err("no program".into());
                    };
                    let argv = program.iter().find(|(k, _)| k == "argv");
                    assert!(matches!(argv, Some((_, Json::Array(a))) if !a.is_empty()));
                }
            }
            // A launch directory of 70,000 bytes not UTF-8.
            if let Some(facts) = snapshot.panes.first_mut().and_then(|p| p.facts.as_mut()) {
                facts.launch.cwd = fux_wire::inspect::Path::Bytes(vec![255; 70_000]);
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

    /// `arrange` is checked on the tab's effective size: its own while no
    /// client shows it, else the smallest screen of those that size it,
    /// leaving watchers out, while every pane fits there.
    #[test]
    fn arrange_is_checked_on_the_effective_size_watchers_left_out() -> TestResult {
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
        run(&mut s, "@1 80 40 resize @1 resize-auto")?;
        let client = s.attach(40, 7, None)?;
        watch(&mut s, 3, 3)?;
        let refused = run(&mut s, propose)
            .err()
            .ok_or("small sizing client accepted")?;
        assert!(refused.contains("its size, 7x40"), "{refused}");
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
                    Request::Do(_)
                    | Request::Wait(_)
                    | Request::NextEvents { .. }
                    | Request::Read { .. } => {
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
        let command = Command::SendKeys {
            to,
            inputs: vec![Input::Paste(text.clone())],
        };
        s.command_now(&command, &Origin::Cli(Caller::default()));
        let queued = queued(&mut s, PaneId::of(1))?;
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
        give(&mut s, id, Process::Reading(child))?;
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
        host(&mut s, id, |h| h.pane_mut().exiting(Ended::Exited(3)))?;
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
    fn unexpected_terminal_read_errors_are_told_once() -> TestResult {
        let (mut session, client) = attached(10, 40)?;
        let id = *session.parts.panes().keys().next().ok_or("pane")?;
        host(&mut session, id, |h| h.read_failed(fuxix::Errno::SRCH))?;
        let view = session.views.get_mut(&client).ok_or("client")?;
        let notice = view.notice.take().ok_or("notice")?.text;
        assert!(notice.contains("reading program terminal"), "{notice}");
        session.out(id, b"more");
        let view = session.views.get(&client).ok_or("client")?;
        assert!(view.notice.is_none(), "told once");
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
                let fresh = crate::pane::Pane::new(
                    id,
                    None,
                    runs.clone(),
                    sized(9, 40)?,
                    crate::pane::tests::options(1024)?,
                    Instant::now(),
                )?;
                host(&mut session, id, |h| {
                    drop(h.pane_mut().respawned(fresh, Process::Absent))
                })?;
                session.parts.pane_mut(id).ok_or("pane")?.runs = runs;
            }
            session.out(id, b"saved");
            if live {
                host(&mut session, id, |h| h.read_failed(fuxix::Errno::SRCH))?;
            } else {
                host(&mut session, id, |h| {
                    h.pane_mut().read_failed(fuxix::Errno::SRCH)
                })?;
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
                    && notice.text.contains(&fuxix::Errno::SRCH.to_string()),
                "{notice:?}"
            );
            assert!(notice.text.contains("exited"), "{notice:?}");
            assert_eq!(notice.text.matches("reading program terminal").count(), 1);
            if explicit {
                let error = Some(fuxix::Errno::SRCH.to_string());
                assert_eq!(session.pane(id)?.read_error, error);
                assert!(
                    session
                        .screen(id)
                        .ok_or("its screen")?
                        .rows()
                        .any(|row| row.text().contains("saved"))
                );
            } else {
                assert!(session.pane(id).is_err());
            }
            if last {
                assert!(session.outbox.iter().any(|out| matches!(out, Outgoing::Shutdown(why, _) if why.contains(&fuxix::Errno::SRCH.to_string()))));
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
        assert!(!host(&mut s, id, |h| h.pane().bell)?, "rung");
        Ok(())
    }

    /// A fixed tab is laid out on its own size, whatever screens show it.
    #[test]
    fn a_fixed_tab_ignores_the_clients_showing_it() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 40 12 size str +1 workspace in new-tab drop")?;
        run(&mut s, &format!("str {c} client str @2 tab show"))?;
        let size = |s: &Session| {
            s.pane(PaneId::of(2))
                .map(|p| (p.size().cols(), p.size().rows()))
        };
        assert_eq!(size(&s).ok(), Some((40, 12)), "on a larger screen");
        s.size_client(c, 6, 20)?;
        assert_eq!(size(&s).ok(), Some((40, 12)), "on a smaller screen");
        Ok(())
    }

    /// A tab that fits clients is laid out on the smallest pane area of
    /// the clients showing it while every pane fits there, else on its
    /// stored size, the last that did.
    #[test]
    fn a_fitting_tab_takes_the_smallest_screen_that_shows_every_pane() -> TestResult {
        let (mut s, _) = attached(24, 80)?;
        run(&mut s, "%1 str horizontal axis split drop")?;
        let tab = TabId::of(1);
        let laid = |s: &Session| -> Result<_, String> {
            let root = s.root(tab).ok_or("a layout")?;
            let sizes = (root.panes().into_iter()).map(|p| s.pane(p).map(Pane::size));
            let sizes = sizes
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok((s.area_of(tab), sizes))
        };
        let on = |s: &Session, rows, cols| -> Result<_, String> {
            let area = Rect::screen(rows, cols);
            let placed = layout::place(s.root(tab).ok_or("a layout")?, area);
            let size = |(_, r): &(PaneId, Rect)| sized(r.h(), r.w());
            Ok((
                Some(area),
                placed.panes.iter().map(size).collect::<Result<_, _>>()?,
            ))
        };
        let small = s.attach(20, 60, None)?;
        assert_eq!(laid(&s)?, on(&s, 20, 60)?, "the smallest screen");
        // Too narrow for both panes: the last size that showed them.
        s.size_client(small, 20, 2)?;
        assert_eq!(laid(&s)?, on(&s, 20, 60)?, "its stored size");
        s.detach_client(small);
        assert_eq!(laid(&s)?, on(&s, 24, 80)?, "the one screen left");
        Ok(())
    }

    /// A pane's `row` and `col` are where its tab's layout on its
    /// effective size places it, as its `rows` and `cols` are: a zoomed
    /// pane at the top left, filling it.
    #[test]
    fn a_panes_position_is_its_place_in_its_tab() -> TestResult {
        let (mut s, _) = attached(24, 80)?;
        run(
            &mut s,
            "%1 str horizontal axis split drop %2 str vertical axis split drop",
        )?;
        let fields = |s: &mut Session, pane: &str| {
            let get = format!(
                "{pane} str row get {pane} str col get {pane} str rows get {pane} str cols get"
            );
            output(s, &get)
        };
        let placed = layout::place(
            s.root(TabId::of(1)).ok_or("a layout")?,
            Rect::screen(24, 80),
        );
        for (id, rect) in &placed.panes {
            let want = format!("{}\n{}\n{}\n{}\n", rect.y(), rect.x(), rect.h(), rect.w());
            assert_eq!(fields(&mut s, &id.to_string())?, want, "{id}");
        }
        run(&mut s, "%3 zoom")?;
        assert_eq!(fields(&mut s, "%3")?, "0\n0\n24\n80\n");
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
        run(
            &mut s,
            "opts 100 30 size str other in new-tab drop @2 resize-auto",
        )?;
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
    fn each_client_chooses_its_own_stack_child_and_no_program_resizes() -> TestResult {
        let (mut s, c1) = attached(24, 81)?;
        let c2 = s.attach(24, 81, None)?;
        output_as(&mut s, Some(c1), BAR_ROW)?;
        // An editor on the left, two alternatives on the right.
        run(&mut s, "%1 str horizontal axis split drop %2 stack drop")?;
        s.out(PaneId::of(2), b"second");
        s.out(PaneId::of(3), b"third");
        let sizes = |s: &Session| [1, 2, 3].map(|p| s.pane(PaneId::of(p)).map(Pane::size).ok());
        let before = sizes(&s);
        assert_eq!(
            before[1], before[2],
            "both children are sized on the stack's room"
        );
        let lines = told(&mut s, |s| _ = run(s, &format!("str {c2} client %3 show")));
        assert!(lines.contains(r#""event":"shown""#), "{lines}");
        assert!(!lines.contains("resized"), "{lines}");
        assert_eq!(sizes(&s), before);
        let screen = |s: &mut Session, c| output(s, &format!("str {c} client capture string"));
        let (one, two) = (screen(&mut s, c1)?, screen(&mut s, c2)?);
        assert!(one.contains("second") && !one.contains("third"), "{one}");
        assert!(two.contains("third") && !two.contains("second"), "{two}");
        // The bar lists the children of the focus's stack.
        let bar = one.lines().last().unwrap_or_default();
        assert!(bar.contains("%2") && bar.contains("%3"), "{one}");
        // A pane's next steps among its stack's children, going round.
        let step =
            |s: &mut Session, way| run(s, &format!("str {c1} client dup focused str {way} step"));
        for (way, shown) in [("next", 3), ("next", 2), ("last", 3)] {
            step(&mut s, way)?;
            assert_eq!(s.focused(c1), Some(PaneId::of(shown)), "{way}");
        }
        assert_eq!(s.focused(c2), Some(PaneId::of(3)));
        // The client record has each choice with the tab's revision.
        use fux_wire::inspect::{Projection, Scope};
        let Body::Snapshot(snapshot) = s.inspect(Scope::All, Projection::Facts)? else {
            return Err("snapshot".into());
        };
        snapshot.validate()?;
        let choices = |c| {
            let client = snapshot.clients.iter().find(|r| r.id == c);
            client.map(|r| {
                r.choices
                    .iter()
                    .map(|ch| (ch.stack, ch.current, ch.last))
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(choices(c1), Some(vec![(2, 4, Some(3))]));
        assert_eq!(choices(c2), Some(vec![(2, 4, Some(3))]));
        // A child closed shows each client shown it the one it was before.
        run(&mut s, "%3 kill")?;
        assert_eq!([c1, c2].map(|c| s.focused(c)), [Some(PaneId::of(2)); 2]);
        assert_eq!(sizes(&s)[1], before[1]);
        Ok(())
    }

    #[test]
    fn a_focus_gone_follows_its_stack_back_to_the_child_shown_before() -> TestResult {
        let shown = |s: &Session, c| s.root(TabId::of(1)).map(|r| r.shown_panes(c));
        // `[%1, %2 | %4, %3]`: the stack's last child is %1, the seat's
        // last pane %2, which closes first.
        let (mut s, c) = attached(24, 80)?;
        run(
            &mut s,
            "%1 stack drop %2 stack drop %2 str horizontal axis split drop",
        )?;
        run(
            &mut s,
            &format!("str {c} client %2 show str {c} client %4 show"),
        )?;
        run(&mut s, "%2 kill %4 kill")?;
        assert_eq!(s.focused(c), Some(PaneId::of(1)));
        assert_eq!(shown(&s, c), Some(vec![PaneId::of(1)]));
        // Moved out, as closed: `[%1 | %5, %3, %4]`, shown %4, then %1 and
        // %5; %5 leaves for its last pane, then %1 for the stack's last.
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 80 24 size str other in new-tab drop")?;
        run(
            &mut s,
            "%1 stack drop %3 stack drop %1 str horizontal axis split drop",
        )?;
        for pane in [4, 1, 5] {
            run(&mut s, &format!("str {c} client %{pane} show"))?;
        }
        run(&mut s, "%5 str @2 tab move")?;
        assert_eq!(s.focused(c), Some(PaneId::of(1)));
        run(&mut s, "%1 str @2 tab move")?;
        assert_eq!(s.focused(c), Some(PaneId::of(4)));
        assert_eq!(shown(&s, c), Some(vec![PaneId::of(4)]));
        Ok(())
    }

    #[test]
    fn a_step_from_a_zoom_finds_the_zoomed_panes_neighbour() -> TestResult {
        let (mut s, c1) = attached(24, 80)?;
        let c2 = s.attach(24, 80, None)?;
        run(&mut s, "%1 str horizontal axis split drop %2 stack drop")?;
        run(
            &mut s,
            &format!("str {c1} client %2 show str {c2} client %3 show %2 zoom"),
        )?;
        run(
            &mut s,
            &format!("str {c2} client dup focused str left step"),
        )?;
        assert_eq!(s.tab(TabId::of(1)).and_then(Tab::zoomed), None);
        assert_eq!(s.focused(c2), Some(PaneId::of(1)));
        let shown = s.root(TabId::of(1)).map(|r| r.shown_panes(c2));
        assert_eq!(
            shown,
            Some(vec![PaneId::of(1), PaneId::of(3)]),
            "its choice kept"
        );
        Ok(())
    }

    #[test]
    fn a_layout_deeper_than_64_dumps_and_replays_as_it_is() -> TestResult {
        let mut s = started()?;
        run(&mut s, "opts 240 24 size str deep in new-tab drop")?;
        for _ in 0..65 {
            run(
                &mut s,
                "opts 10 10 size str other in new-tab str @2 tab move",
            )?;
        }
        let tab = TabId::of(2);
        let dump = s.dump_recipe(Some(AnyRef::Tab(tab)))?;
        let mut again = started()?;
        run(&mut again, &dump).map_err(|e| format!("{e}\n{dump}"))?;
        let made = (again.parts.workspaces().iter())
            .flat_map(|w| w.tabs())
            .last();
        // The same tree, each pane by its place in the layout.
        let shape = |root: &Tree| {
            let panes = root.panes();
            let at = |p: &PaneId| panes.iter().position(|q| q == p);
            let nodes = root.nodes().into_iter();
            nodes
                .map(|n| match n {
                    fux_wire::layout::Node::Pane(p) => format!("{:?}", at(&p)),
                    node @ (fux_wire::layout::Node::Split { .. }
                    | fux_wire::layout::Node::Stack { .. }) => format!("{node:?}"),
                })
                .collect::<Vec<_>>()
        };
        let (Some(root), Some(made)) = (s.root(tab), made.map(Tab::root)) else {
            return Err("no layout".into());
        };
        assert_eq!(root.panes().len(), 66);
        assert_eq!(shape(made), shape(root));
        Ok(())
    }

    #[test]
    fn a_stack_child_no_client_is_shown_needs_its_room_too() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        run(&mut s, "opts 20 9 size str +1 workspace in new-tab drop")?;
        run(&mut s, &format!("str {c} client %2 show"))?;
        // `[%2, %3 / %4 / %5]`, shown %2: the hidden child needs 8 rows.
        let made = "%2 stack drop %3 str vertical axis split drop %4 str vertical axis split drop";
        run(&mut s, made)?;
        assert_eq!(s.focused(c), Some(PaneId::of(2)));
        for refused in ["%5 str vertical axis split", "@2 20 7 resize"] {
            let why = run(&mut s, refused).err().unwrap_or_default();
            assert!(why.starts_with("size: "), "{refused}: {why}");
        }
        assert_eq!(s.parts.panes().len(), 5);
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
        host(&mut s, id, |h| {
            h.starts(Some("/nonexistent/fux.sock".into()))
        })?;
        let mut refused = String::new();
        let lines = told(&mut s, |s| {
            let respawn = run(s, "%2 str /no/such/bin program kill-first respawn");
            refused = respawn.err().unwrap_or_default();
        });
        assert!(refused.contains("/no/such/bin"), "{refused}");
        assert!(lines.is_empty(), "{lines}");
        let held = host(&mut s, id, |h| {
            h.pane().frame_deadline().is_some() && h.pane().left == Left::default()
        })?;
        assert!(held, "the frame still held, nothing left");
        host(&mut s, id, |h| h.starts(None))?;
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
            let unread = Timer::Pane(id, crate::host::Timer::Unread);
            assert!(s.timers().any(|t| t.1 == unread), "no turn due");
            s.fire_now(unread);
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
        assert!(host(&mut s, id, |h| h.respawning())?);
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
        let fresh = host(&mut s, id, |h| h.replacement().map(|(_, bytes)| bytes))?;
        assert_eq!(fresh, Some(4096));
        run(&mut s, "%2 128 history-budget")?;
        let (answer, turns) = turns(&mut s, id, &mut waiting)?;
        assert!(turns > 1, "read in {turns} turns");
        assert_eq!(answer.end(), fux_wire::protocol::End::Done);
        assert_eq!(host(&mut s, id, |h| h.pane().history_bytes())?, 128);
        let pane = s.pane(id)?;
        assert_eq!(answer.response, acted(pane.version(s.run)).response);
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
        host(&mut s, id, |h| {
            h.starts(Some("/nonexistent/fux.sock".into()))
        })?;
        let begun = s.begin_now(respawn_of(id, &[])?, &Origin::Cli(Caller::default()));
        host(&mut s, id, |h| h.starts(None))?;
        let Begun::Waiting(mut waiting) = begun else {
            return Err("answered before what was left was read".into());
        };
        let children = host(&mut s, id, |h| h.children().len())?;
        assert_eq!(children, 1, "the program started for it");
        // The program it is respawned over, one that ignores the hangup,
        // as a respawn holds it while what it left is still read.
        let argv = ["/bin/sh", "-c", "trap '' HUP; exec sleep 60"].map(str::to_owned);
        let old = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        let old_pid = old.leader.pid();
        host(&mut s, id, |h| h.respawned_over(old))?;
        run(&mut s, "%2 kill")?;
        let answer = waiting.answer(&mut s, Instant::now()).ok_or("no answer")?;
        let said = format!("{:?}", answer.end());
        assert_eq!(answer.end().code(), Some(Code::Closed), "{said}");
        assert!(said.contains("%2 closed while respawning"), "{said}");
        let dying = s.ending.iter().map(|(_, link)| match link {
            Link::Local(host) => host.dying(),
            Link::Remote(_) => 0,
        });
        assert_eq!(
            dying.sum::<usize>(),
            2,
            "the program started for it and the old, ended"
        );
        reap(&mut s);
        let mut gone = false;
        for _ in 0..200 {
            gone = !matches!(fuxix::process::reap(old_pid), Ok(None));
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
        let fresh = crate::pane::Pane::new(
            id,
            None,
            pane.runs.clone(),
            pane.size(),
            pane.options(),
            now,
        )?;
        host(&mut s, id, |h| h.respawning_over(fresh, old, now))?;
        let lines = told(&mut s, |s| {
            for _ in 0..100 {
                if !host(s, id, |h| h.respawning()).unwrap_or(false) {
                    break;
                }
                let unread = Timer::Pane(id, crate::host::Timer::Unread);
                s.fire(unread, crate::after(Instant::now(), crate::host::GRACE));
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
            host(&mut s, id, |h| {
                drop(h.set_process(Process::Reading(child)));
                if let Some(ended) = exiting {
                    h.pane_mut().exiting(ended);
                }
            })?;
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
            let signalled = run(&mut s, "%2 str INT signal");
            assert!(signalled.is_err_and(|e| e.contains("respawning")));
            // Nor has it a program of its own till the new one takes its place.
            assert_eq!(output(&mut s, "%2 str program get json")?.trim(), "null");
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
        assert_eq!(timers, [Timer::Pane(id, crate::host::Timer::Unread)]);
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
        host(&mut s, id, |h| {
            h.starts(Some("/nonexistent/fux.sock".into()))
        })?;
        let begun = s.begin_now(
            respawn_of(id, &["/bin/sh", "-c", "exit 0"])?,
            &Origin::Cli(Caller::default()),
        );
        host(&mut s, id, |h| h.starts(None))?;
        let Begun::Waiting(mut waiting) = begun else {
            return Err("answered before what was left was read".into());
        };
        let ended = |s: &mut Session| {
            host(s, id, |h| {
                let replacement = h.replacement().and_then(|(child, _)| child);
                replacement.and_then(|c| c.leader.exited())
            })
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while ended(&mut s)?.is_none() {
            assert!(Instant::now() < deadline, "the program never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
        // A pane reaps no orphan it holds: none but its children.
        let held = host(&mut s, id, |h| {
            let started = h
                .replacement()
                .and_then(|(child, _)| child)
                .map(|c| c.leader.pid());
            h.children().iter().any(|c| Some(*c) == started)
        })?;
        assert!(held);
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
        host(&mut s, id, |h| {
            h.starts(Some("/nonexistent/fux.sock".into()))
        })?;
        run(&mut s, "%2 respawn")?;
        let size = s.pane(id)?.size();
        let child = host(&mut s, id, |h| h.set_process(Process::Absent))?;
        let Process::Reading(child) = child else {
            return Err("no program started".into());
        };
        let told = fuxix::terminal::window_size(&child.master).and_then(|(_, p)| p);
        let told = told.map(|p| (p.width.get(), p.height.get()));
        drop(child.master);
        let _ = child.leader.finish().and_then(process::reaped);
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
        give(&mut s, id, Process::Reading(child))?;
        let pixels = |s: &mut Session| {
            host(s, id, |h| {
                let master = &h.pane().process().child()?.master;
                let (_, pixels) = fuxix::terminal::window_size(master)?;
                pixels.map(|p| (p.width.get(), p.height.get()))
            })
        };
        let view = s.views.get_mut(&c).ok_or("the view")?;
        view.terminal.cell = fux_vt::CellSize::new(10, 20);
        s.settled();
        let smaller = sized(10, 40)?;
        s.resize_screen(id, smaller);
        assert_eq!(pixels(&mut s)?, Some((400, 200)));
        if let Process::Reading(child) = host(&mut s, id, |h| h.set_process(Process::Absent))? {
            drop(child.master);
            let _ = child.leader.finish().and_then(process::reaped);
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
        give(&mut s, id, Process::Reading(child))?;
        let winsize = |s: &mut Session| {
            host(s, id, |h| {
                fuxix::terminal::window_size(&h.pane().process().child()?.master)
            })
            .ok()
            .flatten()
        };
        s.input(c, b"\x1b[6;20;10t");
        let told = winsize(&mut s);
        assert!(told.is_some_and(|(_, pixels)| pixels.is_some()), "{told:?}");
        run(&mut s, "opts 80 24 size str other in new-tab drop")?;
        let show = |ws: &str| format!("str {c} client str {ws} workspace show");
        let unknown = |s: &mut Session| winsize(s).is_some_and(|(_, pixels)| pixels.is_none());
        run(&mut s, &show("other"))?;
        assert!(unknown(&mut s), "hidden");
        run(&mut s, &show("+1"))?;
        assert_eq!(winsize(&mut s), told, "shown again");
        s.detach_client(c);
        assert!(unknown(&mut s), "detached");
        if let Process::Reading(child) = host(&mut s, id, |h| h.set_process(Process::Absent))? {
            drop(child.master);
            let _ = child.leader.finish().and_then(process::reaped);
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
            host(s, id, |h| {
                h.pane_mut()
                    .push_with(|_, out| out.extend_from_slice(bytes))
            })
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
        assert!(host(&mut s, id, |h| h.pane().input().is_empty())?);
        let json = output(&mut s, "%2 capture json")?;
        assert!(
            json.contains(r#""input_unread":3,"#) && json.contains(r#""input_queued":0,"#),
            "{json}"
        );
        // Respawned over while it runs, with input it has not read.
        let argv = ["/bin/cat".to_owned()];
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        run(&mut s, "%2 respawn")?;
        give(&mut s, id, Process::Reading(child))?;
        queue(&mut s, b"de")?;
        let lines = told(&mut s, |s| _ = run(s, "%2 kill-first respawn"));
        assert!(
            lines.contains(
                r#"{"event":"respawned","pane":"%2","tab":"@2","workspace":"+2","generation":2,"input_unread":2}"#
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
        let queued = |s: &mut Session| host(s, id, |h| h.pane().input().len());
        s.out(id, b"\x1b[?2048h\x1b[?2031h");
        s.end_program(id, Ended::Exited(0));
        s.resize_screen(id, sized(10, 40)?);
        s.input(c, b"\x1b[?997;1n");
        assert_eq!(queued(&mut s)?, 0);
        run(&mut s, "%2 respawn")?;
        assert_eq!(queued(&mut s)?, 0);
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
            give(&mut s, PaneId::of(2), Process::Reading(child))?;
            run(&mut s, "%2 kill-first respawn")?;
        }
        let ended = host(&mut s, PaneId::of(2), |h| h.dying())?;
        reap(&mut s);
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
                let version = s.pane(id)?.version(s.run);
                let counts = version
                    .counts
                    .named()
                    .map(|(name, n)| format!(r#","{name}":{n}"#));
                expected.push(format!(
                    r#"{{"pane":"{id}","version":"{version}"{},"status":0}}"#,
                    counts.concat()
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
                give(&mut s, id, Process::Reading(child))?;
                let exited = |s: &mut Session| {
                    host(s, id, |h| h.pane().process().child()?.leader.exited())
                        .ok()
                        .flatten()
                        .is_some()
                };
                let until = crate::after(Instant::now(), Duration::from_secs(10));
                while !exited(&mut s) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(exited(&mut s), "the child did not exit");
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
                assert!(
                    host(&mut s, id, |h| h.pane().has_unread())?,
                    "the budget stopped no call"
                );
                assert!(
                    s.pane(id)?.exit_status().is_none(),
                    "exited with output unread"
                );
            }
            let mut turns = 0u32;
            while s.pane(id)?.exit_status().is_none() && turns < 10_000 {
                s.fire_now(Timer::Pane(id, crate::host::Timer::Unread));
                turns = turns.saturating_add(1);
            }
            assert!(
                budgeted || turns == 0,
                "held={held}: exit was not immediate"
            );
            let pane = s.parts.panes().get(&id).ok_or("the pane")?;
            assert_eq!(pane.exit_status(), Some(Ended::Exited(0)));
            let text = (s.screen(id).ok_or("its screen")?)
                .rows()
                .map(|r| r.text())
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
        give(&mut s, id, Process::Reading(child))?;
        let rows = s.parts.panes().get(&id).map(|p| p.size().rows());
        assert!(rows.is_some_and(|r| r >= 100), "{rows:?} rows");
        std::thread::sleep(Duration::from_millis(200));
        let exited = |s: &Session| {
            s.parts
                .panes()
                .get(&id)
                .is_some_and(|p| p.exit_status().is_some())
        };
        // Told exited, as each reap of another child does, while what it
        // left goes on writing: the exit is told as its leader is reaped,
        // once what was read is on its screen (D-90).
        let mut turns = 0u32;
        s.end_program(id, Ended::Exited(3));
        while !exited(&s) && turns < 10_000 {
            s.fire_now(Timer::Pane(id, crate::host::Timer::Unread));
            // As each reap of another child does.
            s.end_program(id, Ended::Exited(3));
            turns = turns.saturating_add(1);
        }
        let told = exited(&s);
        let writing = matches!(fuxix::process::ended(writer), Ok(None));
        let status = output(&mut s, "%2 1 until-ended str program get str exited get");
        let screen = |s: &Session| s.pane(id).map(|p| p.version(s.run).counts.screen);
        let at_exit = screen(&s)?;
        // Its terminal is read on, a turn at a time, others served between:
        // its screen's revision grows.
        for _ in 0..20 {
            s.fire_now(Timer::Pane(id, crate::host::Timer::Unread));
            run(&mut s, "%1 str x text send-keys")?;
        }
        let read_on = host(&mut s, id, |h| h.leftover())? && screen(&s)? > at_exit;
        // Once the writing stops, its terminal drained, the pane is frozen.
        let _ = fuxix::process::kill_group(writer, fuxix::process::Signal::Kill);
        let mut drained = 0u32;
        while host(&mut s, id, |h| h.leftover())? && drained < 10_000 {
            s.fire_now(Timer::Pane(id, crate::host::Timer::Unread));
            drained = drained.saturating_add(1);
        }
        let drained_at = screen(&s)?;
        for _ in 0..20 {
            s.fire_now(Timer::Pane(id, crate::host::Timer::Unread));
        }
        let frozen = !host(&mut s, id, |h| h.leftover())? && screen(&s)? == drained_at;
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

    /// An ended program's terminal found empty before its last output
    /// can have come (`fuxix::pty::SETTLE`) is waited on, its next turn due
    /// when that time is up; then, or as it closes, it is done with.
    #[test]
    fn a_terminal_found_empty_is_waited_on_till_it_settles() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/true program new-tab",
        )?;
        let id = PaneId::of(2);
        run(&mut s, "%2 respawn")?;
        let argv = ["/bin/sh", "-c", "trap '' HUP; exec sleep 60"].map(str::to_owned);
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        let pid = child.leader.pid();
        give(&mut s, id, Process::Reading(child))?;
        s.end_program(id, Ended::Exited(0));
        let now = Instant::now();
        let at = crate::after(now, Duration::from_secs(5));
        host(&mut s, id, |h| h.settle_till(at))?;
        let empty = |_: &fuxix::pty::Master, _: &mut [u8]| Err(fuxix::Errno::AGAIN);
        host(&mut s, id, |h| h.read_left_with(true, empty, now))?;
        let waited = host(&mut s, id, |h| h.leftover())?;
        let due = host(&mut s, id, |h| h.timer())?;
        host(&mut s, id, |h| h.read_left_with(true, empty, at))?;
        let drained = !host(&mut s, id, |h| h.leftover())?;
        let _ = fuxix::process::kill_group(pid, fuxix::process::Signal::Kill);
        reap(&mut s);
        assert!(waited, "done with before its time was up");
        assert_eq!(due, Some((at, crate::host::Timer::Unread)));
        assert!(drained, "still waited on after its time was up");
        Ok(())
    }

    /// A drain's read that fails, of the terminal an exited program left a
    /// writer on, is journaled with its errno, from that program's
    /// generation; the pane stays as its exit left it.
    #[test]
    fn a_failed_read_of_a_terminal_left_behind_is_journaled() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/true program new-tab",
        )?;
        let id = PaneId::of(2);
        run(&mut s, "%2 respawn")?;
        let argv = ["/bin/sh", "-c", "trap '' HUP; exec sleep 60"].map(str::to_owned);
        let child = process::spawn(&argv, Path::new("/"), (&[], &[]), (sized(4, 20)?, None))?;
        let pid = child.leader.pid();
        give(&mut s, id, Process::Reading(child))?;
        s.end_program(id, Ended::Exited(0));
        let left = host(&mut s, id, |h| h.leftover())?;
        let from = s.parts.journal().end(s.run);
        let fail = |_: &fuxix::pty::Master, _: &mut [u8]| Err(fuxix::Errno::SRCH);
        host(&mut s, id, |h| h.read_left_with(true, fail, Instant::now()))?;
        let told = crate::journal::Reader::new(
            fux_wire::inspect::Scope::All,
            s.parts.journal(),
            s.run,
            Some(from),
        )?
        .batch(s.parts.journal(), true)?
        .events;
        let pane = s.pane(id)?;
        let after = (pane.respawns, pane.exit_status(), pane.read_error.clone());
        let drained = !host(&mut s, id, |h| h.leftover())?;
        let _ = fuxix::process::kill_group(pid, fuxix::process::Signal::Kill);
        reap(&mut s);
        assert!(left, "no writer left on its terminal");
        let failed = Json::object(vec![
            ("event", Json::str("read_failed")),
            ("read_error", Json::str(fuxix::Errno::SRCH.to_string())),
        ]);
        assert!(
            matches!(&told[..], [one] if one.data == failed
                && one.origin == fux_wire::events::Actor::Program(id, 1)),
            "{told:?}"
        );
        assert_eq!(after, (1, Some(Ended::Exited(0)), None));
        assert!(drained, "still read after its read failed");
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
        give(&mut s, id, Process::Reading(child))?;
        s.end_program(id, Ended::Exited(0));
        let told = s.pane(id)?.exit_status().is_some();
        let read_on = host(&mut s, id, |h| h.leftover())?;
        s.fire_now(Timer::Pane(id, crate::host::Timer::Unread));
        let frozen = !host(&mut s, id, |h| h.leftover())?;
        let _ = fuxix::process::kill_group(pid, fuxix::process::Signal::Kill);
        reap(&mut s);
        assert!(told, "the exit not told");
        assert!(read_on, "drained by the read at the exit");
        assert!(frozen, "not drained by the next turn's empty read");
        Ok(())
    }

    #[test]
    /// Keys for a tab's panes go to each that takes them, each on its own
    /// (D-120 (4)): one refusing fails the whole, saying which took them.
    fn keys_for_several_panes_go_to_each_that_takes_them() -> TestResult {
        let mut s = started()?;
        run(
            &mut s,
            "%1 str horizontal axis str /bin/cat program split drop",
        )?;
        // Its program has ended, and the pane stays: nothing takes input there.
        s.end_program(PaneId::of(2), fux_wire::answer::Ended::Exited(0));
        let sent = run(&mut s, "@1 str x text send-keys");
        let sent = sent.err().unwrap_or_default();
        assert!(
            sent.starts_with("not_running: ") && sent.ends_with("; already sent to %1"),
            "{sent}"
        );
        assert_eq!(
            queued(&mut s, PaneId::of(1))?,
            b"x",
            "the first pane took them"
        );
        Ok(())
    }

    #[test]
    fn keys_at_a_version_go_only_while_the_pane_is_at_it() -> TestResult {
        let mut s = started()?;
        let queued = |s: &mut Session| queued(s, PaneId::of(1)).ok();
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
        let history =
            |s: &mut Session| host(s, PaneId::of(1), |h| h.pane().screen().history_len()).ok();
        assert!(history(&mut s) > Some(2));
        // Two rows of four cells: 24 bytes a row and 8 a cell.
        run(&mut s, "%1 str horizontal axis 4096 history-bytes split")?;
        let other = host(&mut s, PaneId::of(2), |h| h.pane().history_bytes())?;
        run(&mut s, "str history-bytes 0 set")?;
        assert!(history(&mut s) > Some(2));
        run(&mut s, "%1 112 history-budget")?;
        assert_eq!(
            host(&mut s, PaneId::of(2), |h| h.pane().history_bytes())?,
            other
        );
        assert_eq!(history(&mut s), Some(2));
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
                    Outgoing::To(..) | Outgoing::Keys(..) | Outgoing::Shutdown(..) => None,
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
        while host(&mut s, PaneId::of(1), |h| h.pane().has_unread())? {
            s.fire_now(Timer::Pane(PaneId::of(1), crate::host::Timer::Unread));
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
            ("str titles false set str Nope unbind", false),
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
                    Outgoing::To(..) | Outgoing::Keys(..) | Outgoing::Shutdown(..) => None,
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

    /// A pane's facts are one observation: its snapshot's record, a
    /// capture and a live get read each field alike, and a write between
    /// two observations moves its version and the journal's cursor.
    #[test]
    fn a_pane_reads_as_one_observation_wherever_it_is_read() -> TestResult {
        let (mut s, _) = attached(6, 30)?;
        run(&mut s, "%1 str horizontal axis split drop")?;
        s.out(PaneId::of(1), b"hi\x1b]2;title\x07\x1b[?1h");
        for (name, _) in s.observed(PaneId::of(1))?.fields() {
            let read =
                |s: &mut Session, from: &str| output(s, &format!("{from} str {name} get json"));
            let listed = read(&mut s, "%1 info str panes get first")?;
            assert_eq!(read(&mut s, "%1 capture")?, listed, "{name}");
            assert_eq!(read(&mut s, "%1")?, listed, "{name}");
        }
        let seen =
            |s: &mut Session| output(s, "%1 capture dup str version get swap str cursor get");
        let before = seen(&mut s)?;
        s.out(PaneId::of(1), b"more");
        run(&mut s, "%1 str written rename")?;
        let after = seen(&mut s)?;
        let moved = before.lines().zip(after.lines()).filter(|(a, b)| a != b);
        assert_eq!(moved.count(), 2, "{before} {after}");
        Ok(())
    }

    #[test]
    fn the_json_help_names_every_pane_field() -> TestResult {
        let (s, _) = attached(4, 30)?;
        let pane = *s.parts.panes().keys().next().ok_or("no pane")?;
        for (name, _) in s.observed(pane)?.fields() {
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
        assert!(waiting.answer(&mut s, Instant::now()).is_none());
        s.out(PaneId::of(2), b"E");
        let answer = waiting.answer(&mut s, Instant::now()).ok_or("no answer")?;
        let text = said(&Ok(answer));
        assert!(text.starts_with("%2:") && !text.contains("%1"), "{text}");
        let Begun::Waiting(mut waiting) = s.begin_now(wait(AnyRef::Pane(PaneId::of(1))), &origin)
        else {
            return Err("the pane's wait answered at once".into());
        };
        run(&mut s, "str %1 pane kill")?;
        let answer = waiting.answer(&mut s, Instant::now()).ok_or("no answer")?;
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
    fn counts_are_read_by_name_from_the_pane_version() -> TestResult {
        let (mut s, client) = attached(24, 80)?;
        let shown = "%2:0.57.1.3.0:48211-1867a3f9c2b8e7d6";
        s.out(PaneId::of(1), shown.as_bytes());
        let capture = format!("str {client} client capture");
        let text = output(&mut s, &format!("{capture} string"))?;
        assert_eq!(text.split_whitespace().next(), Some(shown));
        let refused = output(&mut s, &format!("{capture} str done get"))
            .err()
            .ok_or("client capture supplied invented counts")?;
        assert!(refused.starts_with("no_field: "), "{refused}");
        let counts = s.pane(PaneId::of(1))?.counts().named();
        let expected: Vec<String> = counts.iter().map(|(_, n)| n.to_string()).collect();
        let expected = format!("[{}]", expected.join(","));
        let read: Vec<String> = counts
            .iter()
            .map(|(name, _)| format!("o str {name} get"))
            .collect();
        let read = format!("let o list {} end json", read.join(" "));
        let baseline = output(&mut s, "%1 version")?;
        for source in [
            "%1",
            "%1 capture",
            "%1 str x text send-keys",
            "%1 capture let sample %1 kill sample",
        ] {
            let source = format!("{source} {read}");
            assert_eq!(output(&mut s, &source)?.trim(), expected, "{source}");
        }
        let closed = format!(
            "str closed try str {} changed wait catch str value get end str done get",
            baseline.trim()
        );
        let refused = output(&mut s, &closed)
            .err()
            .ok_or("closed answer supplied invented counts")?;
        assert!(refused.starts_with("no_field: "), "{refused}");
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
        // Its output goes to the clients' connections from the pane itself
        // (`conn`): what they are made of here does not change.
        s.out(fux_wire::id::PaneId::of(1), b"ls");
        assert!(
            !dirty(&s, one) && !dirty(&s, two),
            "its echo changes no scene"
        );
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
        assert!(json.starts_with(r#"{"id":"%1""#), "{json}");
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
        // From a key or prompt it follows, on its client's screen unless
        // given a size; from the command line no client moves, and it is
        // laid out on its size.
        for (from, source, follows, size, program) in [
            (Some(c), "opts str / cwd new-tab", true, viewport, false),
            (
                Some(c),
                "opts 100 30 size str / cwd str cat program new-tab",
                true,
                (100, 30),
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

    /// A capture's cursor is the journal's as its pane's answer is taken,
    /// before what the pane told after it, though both came at once: from
    /// it, what the capture did not see is read (D-120 (3)).
    #[test]
    fn a_captures_cursor_is_the_journals_as_its_answer_is_taken() -> TestResult {
        let mut s = started()?;
        let id = PaneId::of(1);
        let view = command::CaptureView::default();
        let tab = s.locate(id).map(|(_, tab)| tab).ok_or("its tab")?;
        let now = Instant::now();
        let what = Ask::Capture {
            history: None,
            view,
            tab,
            hold: false,
        };
        let ask = s.ask_pane(id, what, now)?;
        // Told after its answer, read with it.
        let actor = fux_wire::events::Actor::Program(id, 0);
        let counts = s.pane(id)?.counts();
        s.heard(
            id,
            FromPane::Told {
                told: crate::journal::Told::Bell,
                actor,
                counts,
                facts: None,
            },
        );
        s.pump(now);
        let Some(Answered::Captured(captured)) = s.answer_to(ask) else {
            return Err("no capture".into());
        };
        let fux_wire::answer::Of::Pane { read, .. } = &captured.of else {
            return Err("a client's capture".into());
        };
        let cursor = read
            .iter()
            .find(|(key, _)| key == "cursor")
            .map(|(_, v)| v.render());
        let cursor = cursor.ok_or("its cursor")?;
        let after = output(&mut s, &format!("all str {cursor} 0 next-events"))?;
        assert!(after.contains(r#""event":"bell""#), "{after}");
        Ok(())
    }

    /// The bodies panes are sent and the captures they hold are numbered as
    /// one: dropping a body is never dropping a capture (D-123).
    #[test]
    fn bodies_and_held_captures_are_numbered_as_one() -> TestResult {
        let mut s = started()?;
        let id = PaneId::of(1);
        let tab = s.locate(id).map(|(_, tab)| tab).ok_or("its tab")?;
        let now = Instant::now();
        let what = Ask::Capture {
            history: None,
            view: command::CaptureView::default(),
            tab,
            hold: true,
        };
        let held = s.ask_pane(id, what, now)?;
        s.give_body(Some(1));
        let (_, body) = s.body_token().ok_or("a token")?;
        assert_ne!(body, held);
        s.tell_pane(id, ToPane::Drop { token: body }, now);
        assert!(
            host(&mut s, id, |h| h.fetch(held).is_some())?,
            "its capture dropped"
        );
        Ok(())
    }

    /// Each body a pane uses up is told retired, so its sender keeps
    /// nothing of a stream of small pastes once each is typed (D-123).
    #[test]
    fn each_body_used_up_is_retired() -> TestResult {
        let mut s = started()?;
        let id = PaneId::of(1);
        let to = command::To::Now(AnyRef::Pane(id));
        for n in 0..32 {
            s.give_body(Some(n));
            let pending = s.send_keys(to, &[], Instant::now())?;
            let (_, token, _) = s.deliveries.pop().ok_or("its delivery")?;
            let inputs = vec![Input::Text("x".into())];
            host(&mut s, id, |h| h.body(token, inputs))?;
            assert_eq!(s.retired.pop(), Some((id, token)));
            let [(_, ask)] = pending.asks.as_slice() else {
                return Err("one ask".into());
            };
            assert!(s.answered_yet(*ask), "its keys typed");
        }
        assert_eq!(queued(&mut s, id)?, [b'x'; 32]);
        Ok(())
    }

    /// Questions whose asker is gone are waited for no more: an answer
    /// that comes after is let go, not kept (D-123).
    #[test]
    fn questions_whose_asker_went_are_let_go() -> TestResult {
        let mut s = started()?;
        let id = PaneId::of(1);
        let tab = s.locate(id).map(|(_, tab)| tab).ok_or("its tab")?;
        let now = Instant::now();
        let what = Ask::Capture {
            history: None,
            view: command::CaptureView::default(),
            tab,
            hold: false,
        };
        let ask = s.ask_pane(id, what, now)?;
        s.abandon(&[(id, ask)], &[], now);
        s.pump(now);
        assert!(s.answered_yet(ask), "still waited for");
        assert!(s.answer_to(ask).is_none(), "its answer kept");
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

    /// Keys and a paste typed at an attached terminal are no request's:
    /// the journal tells no `input` of them, as it does of `send-keys`.
    #[test]
    fn typing_at_a_terminal_journals_no_input() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        let typed = told(&mut s, |s| s.input(c, b"typed\x1b[200~pasted\x1b[201~"));
        assert!(!typed.contains(r#""event":"input""#), "{typed}");
        let mut ran = Ok(());
        let sent = told(&mut s, |s| ran = run(s, "%1 str sent text send-keys"));
        ran?;
        assert!(sent.contains(r#""event":"input""#), "{sent}");
        Ok(())
    }

    /// Each change of a tab's layout journals `laid_out` once, at the
    /// revision the tab then has: one of its tree, fit or stored size,
    /// which makes a new revision, or of its host, which keeps it.
    #[test]
    fn every_layout_change_journals_its_new_revision() -> TestResult {
        /// `change` journals one `laid_out` of tab `@1`, at its revision.
        fn laid_out(
            s: &mut Session,
            what: &str,
            change: impl FnOnce(&mut Session) -> Result<(), String>,
        ) -> TestResult {
            let mut changed = Ok(());
            let lines = told(s, |s| changed = change(s));
            changed?;
            let revision = s.tab(TabId::of(1)).and_then(|t| t.revision(s.run));
            let revision = revision.ok_or("a revision")?;
            let laid_out = format!(
                r#"{{"event":"laid_out","tab":"@1","workspace":"+1","layout_revision":"{revision}"}}"#
            );
            let laid: Vec<&str> = (lines.lines())
                .filter(|l| l.contains(r#""event":"laid_out","tab":"@1""#))
                .collect();
            assert_eq!(laid, [laid_out.as_str()], "{what}");
            Ok(())
        }
        let mut s = started()?;
        let arrange = "@1 str topology snapshot str tabs get first let t \
            t str id get t str layout_revision get t str nodes get arrange";
        for code in [
            "%1 str horizontal axis split drop",
            "%1 %2 swap-with",
            "%1 str right 3 move-border",
            "@1 90 30 resize",
            arrange,
            "%1 str vertical axis split drop",
            "%3 kill",
            "%2 str new-tab move",
            "@1 resize-auto",
        ] {
            laid_out(&mut s, code, |s| run(s, code))?;
        }
        // Fitting clients, it is laid out on the screens that show it.
        let attach = |s: &mut Session, rows, cols| {
            s.attach(rows, cols, Some("+1")).map_err(|e| e.to_string())
        };
        let mut first = None;
        laid_out(&mut s, "a client shows it", |s| {
            first = Some(attach(s, 24, 80)?);
            Ok(())
        })?;
        let first = first.ok_or("a client")?;
        laid_out(&mut s, "its screen resized", |s| {
            s.size_client(first, 30, 100)
                .map(drop)
                .map_err(|e| e.to_string())
        })?;
        let second = attach(&mut s, 30, 100)?;
        let revision = s.tab(TabId::of(1)).and_then(|t| t.revision(s.run));
        laid_out(&mut s, "its host detached", |s| {
            s.detach_client(first);
            Ok(())
        })?;
        laid_out(&mut s, "its host shows another tab", |s| {
            run(s, &format!("str {second} client str @2 tab show"))
        })?;
        let now = s.tab(TabId::of(1)).and_then(|t| t.revision(s.run));
        assert_eq!(now, revision, "laid out otherwise, not anew");
        Ok(())
    }

    /// A kill closes what it kills as killed, and each workspace it
    /// leaves empty that it does not kill as emptied.
    #[test]
    fn a_kill_closes_what_it_kills_and_empties_the_rest() -> TestResult {
        let mut s = started()?;
        run(&mut s, "opts str a in str b in 80 24 size new-tab drop")?;
        run(&mut s, "opts +2 in 80 24 size new-tab drop")?;
        let mut ran = Ok(());
        let lines = told(&mut s, |s| ran = run(s, "+2 kill"));
        ran?;
        let closed: Vec<&str> = (lines.lines())
            .filter(|l| l.contains(r#""event":"closed""#))
            .collect();
        assert_eq!(closed.len(), 6, "{lines}");
        assert!(
            closed.iter().all(|l| l.ends_with(r#""why":"killed"}"#)),
            "{lines}"
        );
        run(&mut s, "opts str c in 80 24 size new-tab drop")?;
        let mut ran = Ok(());
        let lines = told(&mut s, |s| ran = run(s, "@4 kill"));
        ran?;
        assert_eq!(
            lines,
            [
                r#"{"event":"closed","pane":"%4","tab":"@4","workspace":"+4","why":"killed"}"#,
                r#"{"event":"closed","tab":"@4","workspace":"+4","why":"killed"}"#,
                r#"{"event":"closed","workspace":"+4","parent":null,"why":"emptied"}"#,
                "",
            ]
            .join("\n")
        );
        Ok(())
    }

    /// A client moved out of a workspace another closed is told what
    /// closed, who closed it and where it is now, until its next key: the
    /// chooser the close ended and a program's bell after it don't replace
    /// it. The client that closed it is told nothing.
    #[test]
    fn a_client_moved_by_anothers_kill_is_told_and_the_killer_is_not() -> TestResult {
        let (mut s, watcher) = attached(24, 80)?;
        let killer = s.attach(24, 80, None)?;
        run(&mut s, "+1 str harmony rename")?;
        run(
            &mut s,
            "opts +1 in str meta-design in 80 24 size new-tab drop",
        )?;
        run(&mut s, "opts +2 in 80 24 size new-tab drop")?;
        run(
            &mut s,
            "opts +1 in str desktop-simple in 80 24 size new-tab drop",
        )?;
        for client in [watcher, killer] {
            run(&mut s, &format!("str {client} client str @2 tab show"))?;
        }
        output_as(&mut s, Some(watcher), "me %2 %2 tab choose-move")?;
        output_as(&mut s, Some(killer), "+2 kill")?;
        super::testing::deliver(&mut s);
        let notice = |s: &Session, c| s.views.get(&c).and_then(|v| v.notice.clone());
        assert!(notice(&s, killer).is_none());
        // The killer is told the bell; the watcher keeps what moved it.
        s.out(PaneId::of(1), b"\x07");
        assert!(notice(&s, killer).is_some_and(|n| n.until.is_some()));
        assert_eq!(
            notice(&s, watcher).map(|n| n.text).as_deref(),
            Some(
                "harmony › meta-design killed by c2: 2 tabs closed · now harmony · C-b w g shows all"
            )
        );
        s.input(watcher, b"a");
        assert!(notice(&s, watcher).is_none());
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
                r#"{"event":"created","pane":"%2","tab":"@2","workspace":"+1","role":"shell"}"#,
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
                    "{prefix}{{\"event\":\"exited\",\"pane\":\"%2\",\"tab\":\"@1\",\"workspace\":\"+1\",{status}}}\n{}\n{}\n{}\n",
                    r#"{"event":"closed","pane":"%2","tab":"@1","workspace":"+1","why":"exited"}"#,
                    format_args!(
                        r#"{{"event":"laid_out","tab":"@1","workspace":"+1","layout_revision":"@1:2:{}"}}"#,
                        s.run
                    ),
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
                    node @ (fux_wire::layout::Node::Split { .. }
                    | fux_wire::layout::Node::Stack { .. }) => node.clone(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&a.nodes), shape(&b.nodes), "{dump}");
        let launches = |s: &fux_wire::inspect::Snapshot| {
            s.panes
                .iter()
                .map(|p| {
                    [
                        "name",
                        "launch",
                        "cwd",
                        "history_bytes",
                        "program_clipboard",
                        "reflow",
                        "alternate_scroll_lines",
                        "picture_bytes",
                    ]
                    .map(|key| {
                        let mut fields = p.fields();
                        fields.find_map(|(k, v)| (k == key).then(|| v.into_owned()))
                    })
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
        assert_eq!(size(&s, 3), Some(sized(5, 30)?));
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
        // A tab too big for the screen of a client that follows a pane
        // into it keeps its own size.
        output_as(&mut s, Some(small), "%2 str @3 tab move")?;
        assert_eq!(s.area_of(TabId::of(3)), Some(Rect::screen(10, 100)));
        Ok(())
    }

    /// A client moving the pane it runs in into a tab is not one that
    /// will size that tab: the move is checked on the size the tab will
    /// have without it, its stored one, and refused if that hides a pane.
    #[test]
    fn a_client_moving_its_own_pane_does_not_size_where_it_goes() -> TestResult {
        let (mut s, c) = attached(24, 80)?;
        run(
            &mut s,
            "opts 4 24 size str +1 workspace in new-tab drop @2 resize-auto",
        )?;
        s.views.get_mut(&c).ok_or("its view")?.runs_in = Some(PaneId::of(1));
        let roots = |s: &Session| [1, 2].map(|t| s.root(TabId::of(t)).cloned());
        let before = roots(&s);
        let refused = output_as(&mut s, Some(c), "%1 str @2 tab move").err();
        let refused = refused.unwrap_or_default();
        assert!(
            refused.contains("no room") && refused.contains("4x24"),
            "{refused}"
        );
        assert_eq!(roots(&s), before);
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
            "%2 stack drop",
            "%8 str vertical axis split drop",
            "%2 stack drop",
            "%10 kill",
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

    /// A capture's `cursor` is the journal's end as it reads the screen:
    /// `events` from it holds each change after the read, none before.
    #[test]
    fn events_from_a_captures_cursor_follow_its_read() -> TestResult {
        let mut s = started()?;
        run(&mut s, "%1 str before text send-keys")?;
        let cursor = output(&mut s, "%1 capture str cursor get")?;
        let cursor = cursor.trim().parse().map_err(|()| cursor.clone())?;
        assert_eq!(cursor, s.parts.journal().end(s.run));
        run(&mut s, "%1 str after text send-keys")?;
        let after = batch(&s, cursor)?;
        assert_eq!(after.events.len(), 1, "{after:?}");
        assert_eq!(named(&after, "input").len(), 1, "{after:?}");
        Ok(())
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
        s.detach(client, Detach::Detached, Instant::now());
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
            if s.host_mut(id, |h| h.respawning(), Instant::now()) != Some(true) {
                break;
            }
            s.fire(Timer::Pane(id, crate::host::Timer::Unread), Instant::now());
        }
        assert_eq!(
            s.host_mut(id, |h| h.respawning(), Instant::now()),
            Some(false)
        );
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

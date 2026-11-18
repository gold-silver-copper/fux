//! Bounded journal invalidations, read as finite cursor batches
//! (`fux_wire::events`). A later snapshot may see newer state; events do not
//! replicate live OS facts.
use crate::pane::End;
#[cfg(any(test, feature = "in-process"))]
use crate::pane::{Left, Said};
use crate::view::ClientKind;
pub use fux_pane::told::{Sent, Teller, Told};
use fux_wire::act::Opening;
use fux_wire::command::{AnyRef, Run, Version};
use fux_wire::events::{Actor, Batch, Cursor, Item, Seq, Status};
use fux_wire::id::{ClientId, PaneId, TabId, WsId};
use fux_wire::inspect::Scope;
use fux_wire::json::Json;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewChange {
    /// Attached: as a terminal or headless, and only watching if
    /// `read_only`.
    Attached {
        kind: ClientKind,
        read_only: bool,
    },
    Detached(Detach),
    Shown,
    /// Its terminal is now `rows` by `cols`.
    Resized {
        rows: u16,
        cols: u16,
    },
    /// A screen of its opened, or its last closed (none).
    Screen(Option<Opening>),
}
impl ViewChange {
    /// Its name, and what else it says.
    fn said(self) -> (&'static str, Fields) {
        match self {
            Self::Attached { kind, read_only } => {
                let kind = match kind {
                    ClientKind::Terminal => "terminal",
                    ClientKind::Headless => "headless",
                };
                let fields = vec![
                    ("kind", Json::str(kind)),
                    ("read_only", Json::Bool(read_only)),
                ];
                ("attached", fields)
            }
            Self::Detached(why) => ("detached", vec![("why", Json::str(why.name()))]),
            Self::Shown => ("shown", Vec::new()),
            Self::Resized { rows, cols } => {
                let fields = vec![("cols", Json::int(cols)), ("rows", Json::int(rows))];
                ("view_resized", fields)
            }
            Self::Screen(opening) => {
                let kind = opening.map_or(Json::Null, |o| Json::str(o.name()));
                ("screen", vec![("kind", kind)])
            }
        }
    }
}

/// Why a client detached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detach {
    /// It was asked to: by its own terminal, or by a `detach`.
    Detached,
    /// Its connection closed or failed.
    Disconnected,
    /// The server stopped.
    ServerStopped,
    /// The server could not paint it, and ended it.
    Failed,
}
impl Detach {
    fn name(self) -> &'static str {
        match self {
            Self::Detached => "detached",
            Self::Disconnected => "disconnected",
            Self::ServerStopped => "server_stopped",
            Self::Failed => "failed",
        }
    }
}

/// Why a pane, tab or workspace closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Close {
    /// A `kill` of it, or of what holds it.
    Killed,
    /// Its shell exited.
    Exited,
    /// Its last pane or tab left it.
    Emptied,
}
impl Close {
    fn field(self) -> (&'static str, Json) {
        let why = match self {
            Self::Killed => "killed",
            Self::Exited => "exited",
            Self::Emptied => "emptied",
        };
        ("why", Json::str(why))
    }
}

#[derive(Debug)]
struct Entry {
    seq: Seq,
    event: Event,
    actor: Actor,
}

/// The rows the ring's table gains at once, at least.
const ROWS: usize = 16;

/// What happened, as many of the last events as `room` bytes hold, the
/// last of them at least: a reader further behind is told how many it
/// missed. What it holds is counted by `held`: each entry and each text at
/// the size stored.
#[derive(Debug)]
pub struct Journal {
    entries: VecDeque<Entry>,
    /// The bytes it may hold, and those its entries' texts hold.
    room: usize,
    text: usize,
    next: Seq,
    actor: Actor,
    exhausted: bool,
}
impl Journal {
    pub fn new(room: fux_wire::command::JournalBytes) -> Journal {
        Journal {
            entries: VecDeque::new(),
            room: room.get(),
            text: 0,
            next: Seq::default(),
            actor: Actor::default(),
            exhausted: false,
        }
    }
    /// The bytes it holds: its texts and each entry. So an entry let go
    /// makes room at once, whatever it is. The ring's room for entries it
    /// does not hold is not counted and is bounded apart: it grows only
    /// into what `room` has left, and `evict` gives back what is beyond
    /// twice its rows.
    fn held(&self) -> usize {
        (self.text)
            .saturating_add((self.entries.len()).saturating_mul(std::mem::size_of::<Entry>()))
    }
    /// The entries it holds.
    #[cfg(test)]
    pub(crate) fn kept(&self) -> usize {
        self.entries.len()
    }
    /// The most entries its room holds: those that hold no text.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> usize {
        (self.room)
            .checked_div(std::mem::size_of::<Entry>())
            .unwrap_or(0)
    }
    /// Request scopes restore the previous actor before returning; deferred
    /// work retains its own actor rather than inheriting a later request's.
    pub(crate) fn actor(&mut self, actor: Actor) -> Actor {
        std::mem::replace(&mut self.actor, actor)
    }
    pub(crate) fn current_actor(&self) -> Actor {
        self.actor
    }
    pub(crate) fn push(&mut self, event: Event) {
        self.push_from(event, self.actor);
    }
    pub(crate) fn push_from(&mut self, event: Event, actor: Actor) {
        let Some(next) = self.next.0.checked_add(1) else {
            self.exhausted = true;
            return;
        };
        let seq = self.next;
        self.next = Seq(next);
        // The ring's table grows by the rows its room has left, this
        // entry's among them, and no more than half again: with no row
        // left, the oldest entry's is the new one's.
        if self.entries.len() == self.entries.capacity() {
            let held = self.held().saturating_add(event.stored());
            let left = (self.room.saturating_sub(held))
                .checked_div(std::mem::size_of::<Entry>())
                .unwrap_or(0);
            let more = left.min((self.entries.len() / 2).max(ROWS));
            match more {
                0 if self.entries.len() > 1 => self.oldest_goes(),
                _ => self.entries.reserve_exact(more.max(1)),
            }
        }
        self.text = self.text.saturating_add(event.stored());
        self.entries.push_back(Entry { seq, event, actor });
        self.evict();
    }
    /// The oldest entry is let go.
    fn oldest_goes(&mut self) {
        if let Some(evicted) = self.entries.pop_front() {
            self.text = self.text.saturating_sub(evicted.event.stored());
        }
    }
    /// Lets go the oldest entries, but for the newest, till what is held
    /// fits its room: no more of them than that. The table then gives back
    /// its rows beyond twice those it holds: a ring that held many small
    /// entries holds no room for them beside long texts.
    fn evict(&mut self) {
        while self.held() > self.room && self.entries.len() > 1 {
            self.oldest_goes();
        }
        let rows = self.entries.len();
        let most = (rows.saturating_mul(2)).saturating_add(ROWS.saturating_mul(2));
        if self.entries.capacity() > most {
            let kept = rows.saturating_add(rows / 2).saturating_add(ROWS);
            self.entries.shrink_to(kept);
        }
    }
    pub fn end(&self, run: Run) -> Cursor {
        Cursor {
            seq: self.next,
            run,
        }
    }
    /// The oldest entry it still holds, or its end when it holds none.
    fn first(&self) -> Seq {
        self.entries.front().map_or(self.next, |entry| entry.seq)
    }
    fn from(&self, from: Seq) -> (u64, impl Iterator<Item = &Entry>) {
        let first = self.first();
        let skip = self.entries.partition_point(|entry| entry.seq < from);
        (
            first.0.saturating_sub(from.0),
            self.entries.iter().skip(skip),
        )
    }
}
/// A pane's place: its workspace and tab, and the pane.
pub type PaneAt = ((WsId, TabId), PaneId);

/// One thing that happened in the session, where: what can happen to a
/// pane, at a pane, and what can happen to a tab or a workspace, at one.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Pane(PaneAt, PaneEvent),
    Tab((WsId, TabId), Change),
    Workspace(WsId, Change),
    /// A tab's tree, fit, size or host changed, at its layout revision
    /// then (none once its counter is exhausted). Consumers take a new
    /// snapshot.
    Layout((WsId, TabId), Option<u64>),
    /// A tab's zoomed pane is now this one, or none.
    Zoomed((WsId, TabId), Option<PaneId>),
    /// A client's discrete view state changed at this place, if any.
    View {
        client: ClientId,
        at: Option<PaneAt>,
        change: ViewChange,
    },
    /// The server stopped, saying why: the run's last entry, which every
    /// reader reads.
    Stopped(String),
}

/// What happens to a tab or a workspace.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Made, with its name.
    Created(Option<String>),
    /// Named otherwise: its new name.
    Renamed(String),
    Closed(Close),
    Reordered,
}

/// What happens to a pane, and in it.
#[derive(Clone, Debug, PartialEq)]
pub enum PaneEvent {
    /// Made, to run a shell, or a program if not `shell`.
    Created {
        shell: bool,
    },
    Closed(Close),
    /// To another tab, from this workspace and tab.
    Moved((WsId, TabId)),
    /// What the pane itself tells: these alone a `Teller` takes; its
    /// place and lifetime are the session's parts' to journal.
    Told(Told),
}

/// An event's fields after its name and place.
type Fields = Vec<(&'static str, Json)>;

/// A name, as the field `name`.
fn named(name: &str) -> Fields {
    vec![("name", Json::str(name.to_owned()))]
}

impl Change {
    /// The bytes it holds beyond itself.
    fn stored(&self) -> usize {
        match self {
            Change::Created(Some(name)) | Change::Renamed(name) => name.capacity(),
            Change::Created(None) | Change::Closed(_) | Change::Reordered => 0,
        }
    }

    /// Its name, and what else it says.
    fn said(&self) -> (&'static str, Fields) {
        match self {
            Change::Created(name) => (
                "created",
                vec![("name", name.as_ref().map_or(Json::Null, Json::str))],
            ),
            Change::Renamed(name) => ("renamed", named(name)),
            Change::Closed(why) => ("closed", vec![why.field()]),
            Change::Reordered => ("reordered", Vec::new()),
        }
    }
}

impl PaneEvent {
    /// Its name, and what else it says.
    fn said(&self) -> (&'static str, Fields) {
        match self {
            PaneEvent::Created { shell } => {
                let role = if *shell { "shell" } else { "program" };
                ("created", vec![("role", Json::str(role))])
            }
            PaneEvent::Closed(why) => ("closed", vec![why.field()]),
            PaneEvent::Moved(_) => ("moved", Vec::new()),
            PaneEvent::Told(told) => told_said(told),
        }
    }
}

/// The bytes `told` holds beyond itself.
fn told_stored(told: &Told) -> usize {
    {
        match told {
            Told::Renamed(text) | Told::ReadFailed(text) => text.capacity(),
            Told::Notification(said) | Told::Title(said) => said.stored(),
            Told::Input { expected, .. } => {
                (expected.as_ref()).map_or(0, |_| std::mem::size_of::<Version>())
            }
            Told::Respawned { .. }
            | Told::Bell
            | Told::CommandStarted
            | Told::Done(_)
            | Told::Resized(_)
            | Told::HistoryBudget(_)
            | Told::HungUp
            | Told::Signalled { .. }
            | Told::Exited(_) => 0,
        }
    }
}

/// What a pane told: its name, and what else it says.
fn told_said(told: &Told) -> (&'static str, Fields) {
    {
        // A program's text, and `truncated` if it sent more.
        let said = |key, text: &str, cut: bool| {
            let cut = cut.then_some(("truncated", Json::Bool(true)));
            [(key, Json::str(text.to_owned()))]
                .into_iter()
                .chain(cut)
                .collect()
        };
        match told {
            Told::Renamed(name) => ("renamed", named(name)),
            Told::Respawned {
                left, generation, ..
            } => {
                let generation = ("generation", Json::uint(*generation));
                (
                    "respawned",
                    std::iter::once(generation).chain(left.fields()).collect(),
                )
            }
            Told::Bell => ("bell", Vec::new()),
            Told::Notification(n) => ("notification", said("text", n.as_str(), n.cut())),
            Told::Title(title) => ("title", said("title", title.as_str(), title.cut())),
            Told::CommandStarted => ("command_started", Vec::new()),
            Told::Done(status) => (
                "done",
                vec![("status", status.map_or(Json::Null, Json::int))],
            ),
            Told::HistoryBudget(bytes) => (
                "history_budget",
                vec![("history_bytes", Json::count(*bytes))],
            ),
            Told::HungUp => ("hung_up", Vec::new()),
            Told::ReadFailed(error) => (
                "read_failed",
                vec![("read-error", Json::str(error.clone()))],
            ),
            Told::Input {
                kind,
                bytes,
                expected,
                generation,
            } => {
                let kind = match kind {
                    Sent::Keys => "keys",
                    Sent::Paste => "paste",
                    Sent::Mouse => "mouse",
                };
                let expected =
                    (expected.as_ref()).map(|seen| ("expected", Json::str(seen.to_string())));
                let fields = [
                    ("generation", Json::uint(*generation)),
                    ("kind", Json::str(kind)),
                    ("bytes", Json::count(*bytes)),
                ];
                ("input", fields.into_iter().chain(expected).collect())
            }
            Told::Signalled { signal, generation } => (
                "signalled",
                vec![
                    ("generation", Json::uint(*generation)),
                    ("signal", Json::int(signal.raw())),
                ],
            ),
            Told::Resized(size) => {
                let cols = ("cols", Json::int(size.cols()));
                ("resized", vec![cols, ("rows", Json::int(size.rows()))])
            }
            Told::Exited(End { ended, left }) => {
                let ended = std::iter::once(ended.field());
                ("exited", ended.chain(left.fields()).collect())
            }
        }
    }
}

impl Event {
    /// Its pane, if it is a pane's, its tab, if it is of one or in one,
    /// and its workspace.
    fn ids(&self) -> (Option<PaneId>, Option<TabId>, Option<WsId>) {
        match self {
            Event::Pane(((ws, tab), pane), _) => (Some(*pane), Some(*tab), Some(*ws)),
            Event::Tab((ws, tab), _) | Event::Layout((ws, tab), _) => (None, Some(*tab), Some(*ws)),
            Event::Zoomed((ws, tab), pane) => (*pane, Some(*tab), Some(*ws)),
            Event::Workspace(ws, _) => (None, None, Some(*ws)),
            Event::View { at, .. } => at.map_or((None, None, None), |((ws, tab), pane)| {
                (Some(pane), Some(tab), Some(ws))
            }),
            Event::Stopped(_) => (None, None, None),
        }
    }

    /// The bytes it holds beyond itself: its text, at the size it is
    /// stored.
    fn stored(&self) -> usize {
        match self {
            Event::Pane(_, PaneEvent::Told(told)) => told_stored(told),
            Event::Tab(_, change) | Event::Workspace(_, change) => change.stored(),
            Event::Stopped(reason) => reason.capacity(),
            Event::Pane(..) | Event::Layout(..) | Event::Zoomed(..) | Event::View { .. } => 0,
        }
    }

    /// What it closed, if it is a close.
    fn closed(&self) -> Option<AnyRef> {
        match self {
            Event::Pane((_, pane), PaneEvent::Closed(_)) => Some(AnyRef::Pane(*pane)),
            Event::Tab((_, tab), Change::Closed(_)) => Some(AnyRef::Tab(*tab)),
            Event::Workspace(ws, Change::Closed(_)) => Some(AnyRef::Workspace(*ws)),
            Event::Pane(..)
            | Event::Tab(..)
            | Event::Workspace(..)
            | Event::Layout(..)
            | Event::Zoomed(..)
            | Event::View { .. }
            | Event::Stopped(_) => None,
        }
    }

    /// What it says, in server run `run`.
    fn json(&self, run: Run) -> Json {
        let (name, fields) = match self {
            Event::Pane(_, what) => what.said(),
            Event::Layout((_, tab), serial) => {
                let revision = serial.map(|serial| fux_wire::command::LayoutRevision {
                    tab: *tab,
                    serial,
                    run,
                });
                let revision = revision.map_or(Json::Null, |r| Json::str(r.to_string()));
                ("laid_out", vec![("layout_revision", revision)])
            }
            // Unzoomed, `pane` is there as null.
            Event::Zoomed(_, pane) => (
                "zoomed",
                pane.map_or_else(|| vec![("pane", Json::Null)], |_| Vec::new()),
            ),
            Event::View { change, .. } => change.said(),
            Event::Tab(_, change) | Event::Workspace(_, change) => change.said(),
            Event::Stopped(reason) => ("stopped", vec![("reason", Json::str(reason))]),
        };
        Json::object(
            std::iter::once(("event", Json::str(name)))
                .chain(fields)
                .collect(),
        )
    }
}

/// One event of every kind at `at`, each with every field it can say:
/// `name` for what is named, `said` a title and a notification a pane
/// told, `left` for what an end left, `size` resized to, `seen` the
/// version an input was expected at. The test
/// `the_event_and_status_tables_match_the_finite_schema` does not compile
/// while a kind is missing; fux's list of records (`docs/records.txt`)
/// reads them all.
#[cfg(any(test, feature = "in-process"))]
pub fn every_kind(
    at: PaneAt,
    name: &str,
    said: [Told; 2],
    left: Left,
    size: fux_vt::Size,
    seen: Version,
) -> Vec<Event> {
    let mut events = vec![
        Event::Pane(at, PaneEvent::Created { shell: true }),
        Event::Pane(at, PaneEvent::Created { shell: false }),
        Event::Pane(at, PaneEvent::Moved(at.0)),
        // The most a revision is, and one exhausted.
        Event::Layout(at.0, Some(u64::MAX)),
        Event::Layout(at.0, None),
        Event::Zoomed(at.0, Some(at.1)),
        Event::Zoomed(at.0, None),
    ];
    let closes = [Close::Killed, Close::Exited, Close::Emptied];
    events.extend(closes.map(|why| Event::Pane(at, PaneEvent::Closed(why))));
    let changes = [
        Change::Created(None),
        Change::Created(Some(name.to_owned())),
        Change::Renamed(name.to_owned()),
        Change::Reordered,
    ];
    for change in changes.into_iter().chain(closes.map(Change::Closed)) {
        events.push(Event::Tab(at.0, change.clone()));
        events.push(Event::Workspace(at.0.0, change));
    }
    let [title, notification] = said;
    let ended = |ended| Told::Exited(End { ended, left });
    for told in [
        Told::Renamed(name.to_owned()),
        Told::Respawned {
            left,
            generation: i64::MAX.unsigned_abs(),
            argv: None,
        },
        Told::Bell,
        notification,
        title,
        Told::CommandStarted,
        Told::Done(None),
        Told::Done(Some(i32::MIN)),
        Told::Resized(size),
        // The most a pane's budget is (`command::HistoryBytes`).
        Told::HistoryBudget(isize::MAX.unsigned_abs()),
        Told::HungUp,
        Told::ReadFailed(fuxix::Errno::SRCH.to_string()),
        // The most a grift integer says, as it reads them.
        Told::Input {
            kind: Sent::Keys,
            bytes: isize::MAX.unsigned_abs(),
            expected: Some(Box::new(seen)),
            generation: i64::MAX.unsigned_abs(),
        },
        Told::Input {
            kind: Sent::Mouse,
            bytes: isize::MAX.unsigned_abs(),
            expected: None,
            generation: i64::MAX.unsigned_abs(),
        },
        Told::Signalled {
            signal: fuxix::process::Signal::Usr1,
            generation: i64::MAX.unsigned_abs(),
        },
        ended(fux_wire::answer::Ended::Lost),
        ended(fux_wire::answer::Ended::Exited(i32::MIN)),
        ended(fux_wire::answer::Ended::Signalled(i32::MIN)),
    ] {
        events.push(Event::Pane(at, PaneEvent::Told(told)));
    }
    let attached = |kind, read_only| ViewChange::Attached { kind, read_only };
    let detaches = [
        Detach::Detached,
        Detach::Disconnected,
        Detach::ServerStopped,
        Detach::Failed,
    ];
    let screens = [
        Opening::Column,
        Opening::Repeat,
        Opening::List,
        Opening::Prompt,
    ];
    let views = [
        attached(ClientKind::Terminal, false),
        attached(ClientKind::Headless, true),
        ViewChange::Shown,
        ViewChange::Resized {
            rows: u16::MAX,
            cols: u16::MAX,
        },
        ViewChange::Screen(None),
    ];
    let views = (views.into_iter())
        .chain(detaches.map(ViewChange::Detached))
        .chain(screens.map(|s| ViewChange::Screen(Some(s))));
    for change in views {
        events.push(Event::View {
            client: ClientId::of(u32::MAX),
            at: Some(at),
            change,
        });
    }
    // Last, as it ends every read.
    events.push(Event::Stopped(name.to_owned()));
    events
}

/// `every_kind`'s events, one batch read from a journal of them alone in
/// server run `run`, each told by the next of every kind of teller.
#[cfg(feature = "in-process")]
pub fn every_kind_read(run: Run) -> Result<Batch, String> {
    let at = ((WsId::of(1), TabId::of(2)), PaneId::of(3));
    let said = [
        Told::Title(Said::sent("title", true)),
        Told::Notification(Said::sent("text", true)),
    ];
    let left = Left { input: 1 };
    let actors = [
        Actor::Client(ClientId::of(1)),
        Actor::Pane(at.1, 1),
        Actor::Program(at.1, 1),
        Actor::Server,
        Actor::Outside,
    ];
    let kept = fux_wire::command::JournalBytes::new(1 << 20).ok_or("journal")?;
    let mut journal = Journal::new(kept);
    let mut reader = Reader::new(Scope::All, &journal, run, None)?;
    let size = fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?;
    let counts = fux_wire::command::Counts::default();
    let seen = Version {
        pane: at.1,
        counts,
        run,
    };
    let events = every_kind(at, "name", said, left, size, seen);
    for (event, actor) in events.into_iter().zip(actors.into_iter().cycle()) {
        journal.push_from(event, actor);
    }
    reader.batch(&journal, true)
}

/// The bytes `value` is, sent.
fn sent(value: &impl fux_wire::wire::Wire) -> usize {
    struct Count(usize);
    impl fux_wire::wire::Sink for Count {
        fn extend_from_slice(&mut self, bytes: &[u8]) {
            self.0 = self.0.saturating_add(bytes.len());
        }
    }
    let mut count = Count(0);
    value.put(&mut count);
    count.0
}

/// The bytes a batch's events may be, sent, for its reply to be one frame:
/// a frame's payload, less what is said with it on its way
/// (`protocol::ENVELOPE`) and less the rest of the reply, which is its
/// run, three numbers and two cursors, far less than `REST`.
const BATCH: usize = fux_wire::protocol::MAX_PAYLOAD - fux_wire::protocol::ENVELOPE - REST;
const REST: usize = 256;

/// One bounded scan, including entries excluded by its scope.
#[derive(Clone, Copy)]
pub struct Reader {
    scope: Scope,
    next: Cursor,
}
impl Reader {
    pub fn new(
        scope: Scope,
        journal: &Journal,
        run: Run,
        from: Option<Cursor>,
    ) -> Result<Self, String> {
        if journal.exhausted {
            return Err("journal cursor IDs exhausted".into());
        }
        let end = journal.end(run);
        let next = from.unwrap_or(end);
        if next.run != run {
            return Err(format!(
                "{next} is not this server's journal ({run}): a journal ends with its server \
                 process, and one that takes a run back after a crash keeps one of its own, \
                 so what came between is lost; take a new snapshot"
            ));
        }
        if next.seq > end.seq {
            return Err(format!(
                "{next} is past the journal's end ({end}): this run never gave it"
            ));
        }
        Ok(Self { scope, next })
    }
    fn wants(&self, entry: &Entry) -> bool {
        let event = &entry.event;
        let (pane, tab, ws) = event.ids();
        let from = if let Event::Pane(_, PaneEvent::Moved(from)) = event {
            Some(*from)
        } else {
            None
        };
        match self.scope {
            _ if matches!(event, Event::Stopped(_)) => true,
            Scope::All => true,
            Scope::Target(AnyRef::Pane(p)) => pane == Some(p),
            Scope::Target(AnyRef::Tab(t)) => tab == Some(t) || from.is_some_and(|(_, f)| f == t),
            Scope::Target(AnyRef::Workspace(w)) => {
                ws == Some(w) || from.is_some_and(|(f, _)| f == w)
            }
            Scope::Client(c) => matches!(event, Event::View { client, .. } if *client == c),
        }
    }
    fn ends(&self, event: &Event) -> bool {
        match self.scope {
            _ if matches!(event, Event::Stopped(_)) => true,
            Scope::All => false,
            Scope::Target(t) => event.closed() == Some(t),
            Scope::Client(c) => {
                matches!(event, Event::View { client, change: ViewChange::Detached(_), .. } if *client == c)
            }
        }
    }
    /// The events it wants from its cursor on, as many as one frame holds
    /// and one at least: to the journal's end, to the event that ends what
    /// it reads (its scope's close, or the stop), or to the first that the
    /// frame has no room for, which the next batch begins with. One event
    /// longer than a frame is a batch alone, sent as any long reply is, in
    /// pieces its reader joins: the cursor after it comes with the last.
    pub fn batch(&mut self, journal: &Journal, exists: bool) -> Result<Batch, String> {
        if journal.exhausted {
            return Err("journal cursor IDs exhausted".into());
        }
        let mut room = BATCH;
        let (mut missed, entries) = journal.from(self.next.seq);
        self.next.seq = Seq(self.next.seq.0.saturating_add(missed));
        let mut events = Vec::new();
        let mut end = None;
        let mut entries = entries;
        loop {
            let Some(entry) = entries.next() else {
                missed = missed.saturating_add(journal.next.0.saturating_sub(self.next.seq.0));
                self.next.seq = journal.next;
                break;
            };
            missed = missed.saturating_add(entry.seq.0.saturating_sub(self.next.seq.0));
            self.next.seq = entry.seq;
            let mut value = None;
            if self.wants(entry) {
                let next = Cursor {
                    seq: Seq(entry.seq.0.saturating_add(1)),
                    run: self.next.run,
                };
                let (pane, tab, workspace) = entry.event.ids();
                let client = match entry.event {
                    Event::View { client, .. } => Some(client),
                    Event::Pane(..)
                    | Event::Tab(..)
                    | Event::Workspace(..)
                    | Event::Layout(..)
                    | Event::Zoomed(..)
                    | Event::Stopped(_) => None,
                };
                let moved_from = if let Event::Pane(_, PaneEvent::Moved(from)) = entry.event {
                    Some(from)
                } else {
                    None
                };
                let item = Item {
                    data: entry.event.json(self.next.run),
                    pane,
                    tab,
                    workspace,
                    client,
                    moved_from,
                    origin: entry.actor,
                    next,
                };
                // A stop's reason is said twice: by its event, and as the
                // batch's status.
                let stop = match &entry.event {
                    Event::Stopped(reason) => reason.len(),
                    Event::Pane(..)
                    | Event::Tab(..)
                    | Event::Workspace(..)
                    | Event::Layout(..)
                    | Event::Zoomed(..)
                    | Event::View { .. } => 0,
                };
                let bytes = sent(&item).saturating_add(stop);
                if bytes > room && !events.is_empty() {
                    break;
                }
                room = room.saturating_sub(bytes);
                value = Some(item);
            }
            self.next.seq = Seq(entry.seq.0.saturating_add(1));
            events.extend(value);
            if self.ends(&entry.event) {
                end = Some(&entry.event);
                break;
            }
        }
        let caught_up = self.next == journal.end(self.next.run);
        let status = match end {
            Some(Event::Stopped(reason)) => Status::Stopped(reason.clone()),
            Some(_) => Status::Gone,
            None if caught_up && !exists => Status::Gone,
            None => Status::Continuing,
        };
        Ok(Batch {
            events,
            first: Cursor {
                seq: journal.first(),
                run: self.next.run,
            },
            next: self.next,
            missed,
            status,
        })
    }
    #[cfg(test)]
    pub(crate) fn lines(
        &mut self,
        journal: &Journal,
        holder: impl Fn(AnyRef) -> Option<WsId>,
    ) -> (String, bool) {
        let exists = match self.scope {
            Scope::Target(t) => holder(t).is_some(),
            Scope::All | Scope::Client(_) => true,
        };
        let batch = match self.batch(journal, exists) {
            Ok(batch) => batch,
            Err(why) => return (why, false),
        };
        let gone = batch.status == Status::Gone;
        match fux::script::testing::event_lines(batch, &["origin", "seq", "next"]) {
            Ok(lines) => (lines, gone),
            Err(why) => (why.message, false),
        }
    }
}

/// A pending finite read, owned by its requesting connection.
pub struct Read {
    reader: Reader,
    pub deadline: std::time::Instant,
}

/// The code of every id of a kind given.
fn exhausted() -> fux_wire::code::Code {
    fux_wire::code::Code::IdsExhausted
}

fn reply(batch: Result<Batch, String>) -> fux_wire::protocol::Reply {
    match batch {
        Ok(batch) => fux_wire::protocol::Body::Events(Box::new(batch)).into(),
        Err(why) => fux_wire::protocol::Reply::failed(exhausted(), why),
    }
}

impl Read {
    pub fn new(reader: Reader, deadline: std::time::Instant) -> Self {
        Self { reader, deadline }
    }
    pub fn answer(
        &mut self,
        session: &crate::session::Session,
        now: std::time::Instant,
    ) -> Option<fux_wire::protocol::Reply> {
        let reader = &mut self.reader;
        let journal = session.parts.journal();
        if journal.exhausted {
            return Some(fux_wire::protocol::Reply::failed(
                exhausted(),
                "journal cursor IDs exhausted",
            ));
        }
        let exists = match reader.scope {
            Scope::All => true,
            Scope::Target(target) => session.holder(target).is_some(),
            Scope::Client(client) => session.views.contains_key(&client),
        };
        if reader.next == journal.end(session.journal_run) && exists && now < self.deadline {
            return None;
        }
        let mut batch = match reader.batch(journal, exists) {
            Ok(batch) => batch,
            Err(why) => return Some(reply(Err(why))),
        };
        if batch.events.is_empty() && batch.missed == 0 && batch.status == Status::Continuing {
            if now < self.deadline {
                return None;
            }
            batch.status = Status::Timeout;
        }
        Some(reply(Ok(batch)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testing::TestResult;
    use crate::session::{Begun, Caller, Origin, testing::started};
    use fux_wire::command::{AnyRef, Limit, Request};
    use fux_wire::protocol::{Body, Response};
    use std::time::{Duration, Instant};

    fn run() -> Result<Run, String> {
        "1-0".parse().map_err(|()| "run".into())
    }
    /// The events a test's journal keeps, of those that hold nothing
    /// beyond themselves (a bell).
    const KEPT: usize = 1 << 14;
    fn journal() -> Result<Journal, String> {
        let room = KEPT.saturating_mul(std::mem::size_of::<Entry>());
        let room = fux_wire::command::JournalBytes::new(room).ok_or("journal")?;
        Ok(Journal::new(room))
    }
    fn bell(pane: u32) -> Event {
        Event::Pane(
            ((WsId::of(1), TabId::of(1)), PaneId::of(pane)),
            PaneEvent::Told(Told::Bell),
        )
    }

    fn maximum_events(ch: char) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let at = (
            (WsId::of(u32::MAX), TabId::of(u32::MAX)),
            PaneId::of(u32::MAX),
        );
        let name: String = std::iter::repeat_n("\"\\", 128).collect();
        let mut session = started()?;
        // Exercise the real producer: its Said is cloned into the journal.
        let sent: String = std::iter::repeat_n(ch, 501).collect();
        session.output(
            PaneId::of(1),
            format!("\x1b]2;{sent}\x07\x1b]9;{sent}\x07").as_bytes(),
            Instant::now(),
        );
        let mut texts = session
            .parts
            .journal()
            .entries
            .iter()
            .filter_map(|e| match &e.event {
                Event::Pane(_, PaneEvent::Told(t @ (Told::Notification(_) | Told::Title(_)))) => {
                    Some(t.clone())
                }
                Event::Pane(..)
                | Event::Tab(..)
                | Event::Workspace(..)
                | Event::Layout(..)
                | Event::Zoomed(..)
                | Event::View { .. }
                | Event::Stopped(_) => None,
            });
        let title = texts.next().ok_or("title event")?;
        let notification = texts.next().ok_or("notification event")?;
        let left = Left { input: usize::MAX };
        let size = fux_vt::Size::new(4096, 4096)?;
        let counts = fux_wire::command::Counts {
            respawns: u64::MAX,
            screen: u64::MAX,
            bells: u64::MAX,
            done: u64::MAX,
            ended: true,
        };
        let seen = Version {
            pane: at.1,
            counts,
            run: run()?,
        };
        Ok(every_kind(
            at,
            &name,
            [title, notification],
            left,
            size,
            seen,
        ))
    }

    #[test]
    fn the_event_and_status_tables_match_the_finite_schema() -> TestResult {
        use std::collections::BTreeSet;
        let events = maximum_events('𐀀')?;
        let mut names = BTreeSet::new();
        for event in events {
            // New variants must bring this schema fixture up to date.
            match &event {
                Event::Pane(
                    _,
                    PaneEvent::Created { .. }
                    | PaneEvent::Closed(_)
                    | PaneEvent::Moved(_)
                    | PaneEvent::Told(
                        Told::Renamed(_)
                        | Told::Respawned { .. }
                        | Told::Bell
                        | Told::Notification(_)
                        | Told::Title(_)
                        | Told::CommandStarted
                        | Told::Done(_)
                        | Told::Resized(_)
                        | Told::HistoryBudget(_)
                        | Told::HungUp
                        | Told::ReadFailed(_)
                        | Told::Input { .. }
                        | Told::Signalled { .. }
                        | Told::Exited(_),
                    ),
                )
                | Event::Tab(
                    _,
                    Change::Created(_) | Change::Renamed(_) | Change::Closed(_) | Change::Reordered,
                )
                | Event::Workspace(
                    _,
                    Change::Created(_) | Change::Renamed(_) | Change::Closed(_) | Change::Reordered,
                )
                | Event::View {
                    change:
                        ViewChange::Attached { .. }
                        | ViewChange::Detached(_)
                        | ViewChange::Shown
                        | ViewChange::Resized { .. }
                        | ViewChange::Screen(_),
                    ..
                }
                | Event::Layout(..)
                | Event::Zoomed(..)
                | Event::Stopped(_) => {}
            }
            let json = event.json(run()?);
            let Some(Json::String(name)) = json.get("event") else {
                return Err("event name".into());
            };
            names.insert(name.replace('_', "-"));
        }
        let docs = include_str!("../../docs/records.md");
        let table = |anchor: &str| -> Result<BTreeSet<String>, String> {
            let rows = docs
                .split_once(anchor)
                .and_then(|(_, rest)| rest.split_once("| --- | --- |\n"))
                .ok_or_else(|| format!("missing table: {anchor}"))?
                .1;
            Ok(rows
                .lines()
                .take_while(|row| row.starts_with('|'))
                .filter_map(|row| row.strip_prefix("| `")?.split(" |").next())
                .flat_map(|cell| cell.split(", "))
                .map(|name| name.trim_matches('`').to_owned())
                .collect())
        };
        assert_eq!(names, table("The finite coverage schema is:")?);
        let mut statuses = BTreeSet::new();
        for status in [
            Status::Continuing,
            Status::Timeout,
            Status::Gone,
            Status::Stopped(String::new()),
        ] {
            match status {
                Status::Continuing | Status::Timeout | Status::Gone | Status::Stopped(_) => {}
            }
            statuses.insert(status.name().to_owned());
        }
        assert_eq!(statuses, table("| `status` | Meaning |")?);
        assert!(docs.contains("as `journal-bytes` bytes hold (2 MiB"));
        Ok(())
    }

    /// The ring keeps what its room holds: each entry counts itself and
    /// its text as stored, the oldest go before the room is passed, and a
    /// reader behind them is told how many. An entry too long for the room
    /// is kept alone till the next.
    #[test]
    fn a_full_ring_keeps_what_its_bytes_hold_and_its_newest() -> TestResult {
        let events = maximum_events('𐀀')?;
        let maximum = events
            .iter()
            .find(|e| matches!(e, Event::Pane(_, PaneEvent::Told(Told::Notification(_)))))
            .ok_or("notification")?;
        let each = std::mem::size_of::<Entry>().saturating_add(maximum.stored());
        assert!(maximum.stored() >= 501 * 4, "its text counts");
        let texts = |n: usize| maximum.stored().saturating_mul(n);
        let room = each.saturating_mul(8).saturating_add(each / 2);
        let mut journal =
            Journal::new(fux_wire::command::JournalBytes::new(room).ok_or("journal")?);
        assert_eq!(journal.entries.capacity(), 0, "no eager ring allocation");
        let run = run()?;
        let from = |seq| Cursor { seq: Seq(seq), run };
        for n in 1..=8 {
            journal.push(maximum.clone());
            assert_eq!((journal.entries.len(), journal.text), (n, texts(n)));
            assert!(journal.held() <= room);
        }
        let mut oldest = Reader::new(Scope::All, &journal, run, Some(from(0)))?;
        let full = oldest.batch(&journal, true)?;
        assert_eq!((full.missed, full.first.seq), (0, Seq(0)));
        journal.push(maximum.clone());
        assert_eq!((journal.entries.len(), journal.text), (8, texts(8)));
        assert!(journal.held() <= room);
        // Its first is the oldest it holds, as many as a reader from 0 missed.
        let mut lost = Reader::new(Scope::All, &journal, run, Some(from(0)))?;
        let wrapped = lost.batch(&journal, true)?;
        assert_eq!((wrapped.missed, wrapped.first.seq), (1, Seq(1)));
        assert_eq!(lost.batch(&journal, true)?.missed, 0);
        // One longer than the room: kept, alone.
        let long = Event::Stopped(std::iter::repeat_n('x', room).collect());
        journal.push(long.clone());
        assert_eq!(journal.entries.len(), 1);
        assert_eq!(lost.batch(&journal, true)?.events.len(), 1);
        journal.push(bell(1));
        assert_eq!((journal.entries.len(), journal.text), (1, 0));
        assert!(journal.held() <= room);
        if cfg!(target_pointer_width = "64") {
            assert_eq!(std::mem::size_of::<Entry>(), 80);
            assert_eq!(std::mem::size_of::<Event>(), 56);
        }
        Ok(())
    }

    /// F002: a thousand titles of 65,000 bytes that are no UTF-8, each
    /// stored three times as long as it was sent, leave the journal within
    /// its room, counted at the size they are stored, and the thread
    /// holding little more: they were 190 MiB, kept once the pane closed.
    #[test]
    fn a_thousand_titles_of_invalid_utf8_stay_within_the_journals_room() -> TestResult {
        let mut session = started()?;
        let room = fux::script::default_state()?
            .preferences
            .journal_bytes
            .get();
        // Each another title, as one set again is no event.
        let title = |n: usize| -> Vec<u8> {
            let mut title = format!("\x1b]2;{n:04}").into_bytes();
            title.extend(std::iter::repeat_n(0xff, 65_000));
            title.push(7);
            // Whole reads of 8,192 bytes: what follows a title is ignored.
            title.resize(8 * 8192, 0);
            title
        };
        // As a pane reads its program: some thousands of bytes at once.
        let say = |session: &mut crate::session::Session, n: usize| {
            for read in title(n).as_chunks::<8192>().0 {
                session.output(PaneId::of(1), read, Instant::now());
            }
        };
        // Once, for what the pane itself holds of a title that long.
        say(&mut session, 0);
        let start = fux_vt_peak::ThreadPeak.live();
        let mut longest = 0;
        for n in 1..=1000 {
            say(&mut session, n);
            let journal = session.parts.journal();
            assert!(journal.held() <= room, "{} held of {room}", journal.held());
            longest = longest.max(journal.entries.back().map_or(0, |e| e.event.stored()));
        }
        assert!(longest > 65_000 * 3, "a title holds {longest} bytes");
        let held = fux_vt_peak::ThreadPeak.live().saturating_sub(start);
        assert!(held < room.saturating_mul(2), "{held} bytes held more");
        Ok(())
    }

    /// One small row more than a full ring holds lets go the entries whose
    /// room it takes and no more: a workspace's leaving, a `shown` told as
    /// of before its change, a bell, each over a ring full of bells. When
    /// the room its tables had for rows unused was counted, letting a bell
    /// go made none, and half the ring went before the table gave any back.
    #[test]
    fn a_small_row_over_a_full_ring_lets_go_only_what_it_takes() -> TestResult {
        let mut journal = journal()?;
        for _ in 0..KEPT {
            journal.push(bell(1));
        }
        assert_eq!(journal.entries.len(), KEPT);
        let oldest = Cursor {
            seq: journal.first(),
            run: run()?,
        };
        let mut reader = Reader::new(Scope::All, &journal, run()?, Some(oldest))?;
        let shown = Event::View {
            client: ClientId::of(1),
            at: None,
            change: ViewChange::Shown,
        };
        journal.push(shown);
        journal.push(bell(1));
        // Two entries: two entries' room.
        assert!(
            journal.entries.len() >= KEPT - 2,
            "{}",
            journal.entries.len()
        );
        assert!(journal.held() <= KEPT * 80);
        let batch = reader.batch(&journal, true)?;
        assert!(batch.missed <= 2, "{} missed", batch.missed);
        // The room of rows unused is bounded, though not counted.
        assert!(journal.entries.capacity() <= KEPT + KEPT / 2 + ROWS);
        Ok(())
    }

    /// A ring full of small entries, then of long texts, holds few rows: the
    /// table gives back the room of those it held, which the byte count
    /// leaves out, so what it keeps is D-177's: twice its rows and a little.
    #[test]
    fn a_ring_gone_over_to_long_texts_gives_back_the_rows_it_held() -> TestResult {
        let mut journal = journal()?;
        for _ in 0..KEPT {
            journal.push(bell(1));
        }
        let long: String = std::iter::repeat_n('x', 4000).collect();
        for _ in 0..1000 {
            let told = Told::Title(Said::sent(&long, false));
            let at = ((WsId::of(1), TabId::of(1)), PaneId::of(1));
            journal.push(Event::Pane(at, PaneEvent::Told(told)));
        }
        let rows = journal.entries.len();
        assert!(rows < KEPT / 8, "{rows} rows");
        let capacity = journal.entries.capacity();
        assert!(
            capacity <= 2 * rows + 2 * ROWS,
            "{capacity} for {rows} rows"
        );
        Ok(())
    }

    /// The reply a batch is, sent.
    fn reply_bytes(batch: Batch) -> usize {
        fux_wire::wire::encode(&reply(Ok(batch))).len()
    }

    /// F020: a batch is one frame at most, and the next begins where it
    /// stopped: 100 titles of 64 KiB are read in order, none twice and
    /// none missed, in batches that each fit a frame with room for what is
    /// said with it on its way.
    #[test]
    fn a_batch_fits_one_frame_and_the_next_carries_on() -> TestResult {
        let room = fux_wire::command::JournalBytes::new(64 << 20).ok_or("journal")?;
        let mut journal = Journal::new(room);
        let mut reader = Reader::new(Scope::All, &journal, run()?, None)?;
        let at = ((WsId::of(1), TabId::of(1)), PaneId::of(1));
        for n in 0..100 {
            let text: String = std::iter::repeat_n('t', 64 << 10).collect();
            let title = format!("{n:03}{text}");
            let told = Told::Title(Said::sent(&title, false));
            journal.push(Event::Pane(at, PaneEvent::Told(told)));
        }
        let frame = fux_wire::protocol::MAX_PAYLOAD - fux_wire::protocol::ENVELOPE;
        let (mut read, mut batches) = (0_usize, 0);
        while reader.next != journal.end(run()?) {
            let batch = reader.batch(&journal, true)?;
            assert_eq!((batch.missed, &batch.status), (0, &Status::Continuing));
            assert!(
                batch.events.len() >= 10,
                "{} in a batch",
                batch.events.len()
            );
            for item in &batch.events {
                let Some(Json::String(title)) = item.data.get("title") else {
                    return Err("title".into());
                };
                assert_eq!(title.get(..3), Some(format!("{read:03}").as_str()));
                read = read.saturating_add(1);
            }
            assert_eq!(batch.events.last().map(|e| e.next), Some(batch.next));
            let sent = reply_bytes(batch);
            assert!(sent <= frame, "a batch of {sent} bytes");
            batches += 1;
        }
        assert_eq!((read, batches), (100, 7));
        Ok(())
    }

    /// An event longer than a frame (a rename to a name as long as a
    /// request holds) is a batch alone, whole, the cursor past it: what
    /// came before it is read first, what came after it next. The server
    /// sends such a reply as any other, in pieces of a frame, and its
    /// reader joins them (`protocol::Joining`).
    #[test]
    fn an_event_longer_than_a_frame_is_read_whole_and_the_cursor_passes_it() -> TestResult {
        let room = fux_wire::command::JournalBytes::new(64 << 20).ok_or("journal")?;
        let mut journal = Journal::new(room);
        let mut reader = Reader::new(Scope::All, &journal, run()?, None)?;
        let name: String = std::iter::repeat_n('n', fux_wire::protocol::MAX_PAYLOAD).collect();
        let renamed = Change::Renamed(name.clone());
        journal.push(bell(1));
        journal.push(Event::Workspace(WsId::of(1), renamed));
        journal.push(bell(2));
        let before = reader.batch(&journal, true)?;
        assert_eq!((before.events.len(), before.next.seq), (1, Seq(1)));
        let long = reader.batch(&journal, true)?;
        let [item] = long.events.as_slice() else {
            return Err("one event".into());
        };
        assert_eq!(item.data.get("name"), Some(&Json::str(name)));
        assert_eq!((long.next.seq, long.missed), (Seq(2), 0));
        assert!(reply_bytes(long) > fux_wire::protocol::MAX_FRAME);
        let after = reader.batch(&journal, true)?;
        assert_eq!((after.events.len(), after.next), (1, journal.end(run()?)));
        Ok(())
    }

    #[test]
    fn scoped_reads_wait_through_unrelated_entries() -> TestResult {
        let mut session = started()?;
        let now = Instant::now();
        let request = Request::NextEvents {
            scope: Scope::Target(AnyRef::Pane(PaneId::of(1))),
            from: None,
            timeout: Limit::read(5)?,
        };
        let Begun::Waiting(mut waiting) =
            session.begin(request, &Origin::Cli(Caller::default()), now)
        else {
            return Err("did not wait".into());
        };
        for _ in 0..1000 {
            session
                .parts
                .journal_mut()
                .push(Event::Workspace(WsId::of(2), Change::Reordered));
        }
        for _ in 0..3 {
            assert!(
                waiting.answer(&mut session, now).is_none(),
                "unrelated entries ended the wait"
            );
        }
        Ok(())
    }

    #[test]
    fn batches_resume_at_the_exact_consumed_prefix() -> TestResult {
        let mut journal = journal()?;
        let mut reader = Reader::new(Scope::All, &journal, run()?, None)?;
        for _ in 0..1000 {
            journal.push(bell(1));
        }
        let first = reader.batch(&journal, true)?;
        assert_eq!(first.events.len(), 1000);
        assert_eq!(first.events.last().map(|e| e.next), Some(first.next));
        for _ in 0..3 {
            journal.push(bell(1));
        }
        let second =
            Reader::new(Scope::All, &journal, run()?, Some(first.next))?.batch(&journal, true)?;
        assert_eq!(second.events.len(), 3);
        assert_eq!(second.next, journal.end(run()?));
        Ok(())
    }

    #[test]
    fn filtered_entries_advance_the_cursor() -> TestResult {
        let mut journal = journal()?;
        let mut reader = Reader::new(
            Scope::Target(AnyRef::Pane(PaneId::of(2))),
            &journal,
            run()?,
            None,
        )?;
        for _ in 0..1000 {
            journal.push(bell(1));
        }
        let empty = reader.batch(&journal, true)?;
        assert!(empty.events.is_empty());
        assert_eq!(empty.next, journal.end(run()?));
        assert_eq!(empty.status, Status::Continuing);
        journal.push(bell(2));
        let matching = reader.batch(&journal, true)?;
        assert_eq!(matching.events.len(), 1);
        assert_eq!(matching.next, journal.end(run()?));
        Ok(())
    }

    #[test]
    fn retention_loss_is_exact_even_for_a_filtered_scope() -> TestResult {
        let mut journal = journal()?;
        let mut reader = Reader::new(
            Scope::Target(AnyRef::Pane(PaneId::of(2))),
            &journal,
            run()?,
            None,
        )?;
        for _ in 0..KEPT.saturating_add(3) {
            journal.push(bell(1));
        }
        let batch = reader.batch(&journal, true)?;
        assert_eq!(batch.missed, 3);
        assert!(batch.events.is_empty());
        assert_eq!(reader.batch(&journal, true)?.missed, 0);
        Ok(())
    }

    #[test]
    fn exhausted_cursors_refuse_without_appending() -> TestResult {
        let mut journal = journal()?;
        let mut reader = Reader::new(Scope::All, &journal, run()?, None)?;
        journal.next = Seq(u64::MAX);
        journal.push(bell(1));
        assert!(journal.entries.is_empty());
        assert!(Reader::new(Scope::All, &journal, run()?, None).is_err());
        assert!(reader.batch(&journal, true).is_err());
        Ok(())
    }

    #[test]
    fn cursors_refuse_other_runs_future_positions_and_noncanonical_text() -> TestResult {
        let journal = journal()?;
        for text in ["1:1-0", "0:2-0"] {
            let cursor = text.parse().map_err(|()| "cursor")?;
            assert!(Reader::new(Scope::All, &journal, run()?, Some(cursor)).is_err());
        }
        for text in ["00:1-0", "+0:1-0", "0:1-00", "0", "0:1-0:2"] {
            assert!(text.parse::<Cursor>().is_err(), "{text}");
        }
        Ok(())
    }

    #[test]
    fn moves_reach_both_places_and_closure_stops_the_prefix() -> TestResult {
        let mut journal = journal()?;
        let from = (WsId::of(1), TabId::of(1));
        let to = (WsId::of(2), TabId::of(2));
        let pane = PaneId::of(1);
        let mut old = Reader::new(Scope::Target(AnyRef::Tab(from.1)), &journal, run()?, None)?;
        let mut new = Reader::new(Scope::Target(AnyRef::Tab(to.1)), &journal, run()?, None)?;
        journal.push(Event::Pane((to, pane), PaneEvent::Moved(from)));
        journal.push(Event::Tab(from, Change::Closed(Close::Emptied)));
        journal.push(bell(1));
        let closed = old.batch(&journal, false)?;
        assert_eq!(closed.events.len(), 2);
        assert_eq!(closed.status, Status::Gone);
        assert!(closed.next.seq < journal.end(run()?).seq);
        assert_eq!(new.batch(&journal, true)?.events.len(), 1);
        Ok(())
    }

    /// A read of one pane waits, then times out; the server's stop is the
    /// journal's last entry, which ends it.
    #[test]
    fn empty_reads_wait_once_then_return_timeout_or_stopped() -> TestResult {
        let mut session = started()?;
        let now = Instant::now();
        let scope = Scope::Target(AnyRef::Pane(PaneId::of(1)));
        let request = Request::NextEvents {
            scope,
            from: None,
            timeout: Limit::read(5)?,
        };
        let Begun::Waiting(mut waiting) =
            session.begin(request, &Origin::Cli(Caller::default()), now)
        else {
            return Err("did not wait".into());
        };
        assert_eq!(
            waiting.due(),
            Some(crate::after(now, Duration::from_secs(5)))
        );
        assert!(waiting.answer(&mut session, now).is_none());
        let answer = waiting
            .answer(&mut session, crate::after(now, Duration::from_secs(5)))
            .ok_or("timeout")?;
        let Response::Returned(Body::Events(batch)) = answer.response else {
            return Err("batch".into());
        };
        assert_eq!(batch.status, Status::Timeout);
        assert_eq!(batch.next, session.parts.journal().end(session.run));
        let journal = session.parts.journal_mut();
        journal.push_from(Event::Stopped("test".into()), Actor::Outside);
        journal.push(bell(1));
        let answer = waiting.answer(&mut session, now).ok_or("stopped")?;
        let Response::Returned(Body::Events(stopped)) = answer.response else {
            return Err("stopped batch".into());
        };
        assert_eq!(stopped.status, Status::Stopped("test".into()));
        let [item] = stopped.events.as_slice() else {
            return Err("one stopped entry".into());
        };
        assert_eq!(item.data.get("reason"), Some(&Json::str("test")));
        assert_eq!(item.origin, Actor::Outside);
        assert_eq!(Seq(item.next.seq.0.saturating_sub(1)), batch.next.seq);
        assert_eq!(stopped.next, item.next);
        // Dropping the connection's wait owns all cancellation; no journal
        // cursor or subscription remains registered in the session.
        drop(waiting);
        assert!(matches!(
            session.begin(
                Request::NextEvents {
                    scope,
                    from: None,
                    timeout: Limit::read(0)?
                },
                &Origin::Cli(Caller::default()),
                now
            ),
            Begun::Done(_)
        ));
        Ok(())
    }
}

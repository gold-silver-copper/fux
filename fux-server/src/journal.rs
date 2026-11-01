//! Bounded journal invalidations, read as finite cursor batches
//! (`fux_wire::events`). A later snapshot may see newer state; events do not
//! replicate live OS facts.
use crate::pane::{End, Left, Said};
use fux_wire::command::{AnyRef, Run};
use fux_wire::events::{Actor, Batch, Cursor, Item, Seq, Status};
use fux_wire::id::{ClientId, PaneId, TabId, WsId};
use fux_wire::inspect::Scope;
use fux_wire::json::Json;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewChange {
    Attached,
    Detached,
    Shown,
    Resized,
    Screen,
}
impl ViewChange {
    fn name(self) -> &'static str {
        match self {
            Self::Attached => "attached",
            Self::Detached => "detached",
            Self::Shown => "shown",
            Self::Resized => "view_resized",
            Self::Screen => "screen",
        }
    }
}

#[derive(Debug)]
struct Entry {
    seq: Seq,
    event: Event,
    actor: Actor,
}

/// What happened, the last `kept` events of it: a reader further behind is
/// told how many it missed.
#[derive(Debug)]
pub struct Journal {
    entries: VecDeque<Entry>,
    kept: usize,
    next: Seq,
    actor: Actor,
    exhausted: bool,
    /// The workspace each workspace in one is in, as the session's parts
    /// keep it (`nest`).
    parents: BTreeMap<WsId, WsId>,
    /// For each entry kept that happened in a workspace in another, the
    /// workspaces that held those it names then, beyond them: so a
    /// workspace's reader reads what happens in those in it.
    within: BTreeMap<Seq, Box<[WsId]>>,
}
impl Journal {
    pub fn new(kept: fux_wire::command::JournalEntries) -> Journal {
        Journal {
            entries: VecDeque::new(),
            kept: kept.get(),
            next: Seq::default(),
            actor: Actor::default(),
            exhausted: false,
            parents: BTreeMap::new(),
            within: BTreeMap::new(),
        }
    }
    /// Workspace `ws` is in `parent` from now on, or at the top (or gone).
    pub(crate) fn nest(&mut self, ws: WsId, parent: Option<WsId>) {
        match parent {
            Some(parent) => self.parents.insert(ws, parent),
            None => self.parents.remove(&ws),
        };
    }
    /// The workspaces nested as they are now, for journaling an event at a
    /// place as it was before a change (`push_then`).
    pub(crate) fn nesting(&self) -> BTreeMap<WsId, WsId> {
        self.parents.clone()
    }
    /// The workspaces holding those `event` names, beyond them, nested as
    /// `parents` says.
    fn within_of(event: &Event, parents: &BTreeMap<WsId, WsId>) -> Vec<WsId> {
        let parent = |ws: &WsId| parents.get(ws).copied();
        let up = |ws: WsId| std::iter::successors(parent(&ws), parent).take(parents.len());
        let (_, _, ws) = event.ids();
        let from = match event {
            Event::Pane(_, PaneEvent::Moved((from, _))) => Some(*from),
            Event::Workspace(_, Change::Moved(from)) => *from,
            Event::Pane(..)
            | Event::Tab(..)
            | Event::Workspace(..)
            | Event::Layout(_)
            | Event::Zoomed(..)
            | Event::View { .. }
            | Event::Stopped(_) => None,
        };
        let from = from
            .into_iter()
            .flat_map(|f| std::iter::once(f).chain(up(f)));
        ws.into_iter().flat_map(up).chain(from).collect()
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
        let within = Self::within_of(&event, &self.parents);
        self.record(event, actor, within);
    }
    /// `event` at a place as the workspaces were nested `then`, and as they
    /// are now too if `now`.
    pub(crate) fn push_then(&mut self, event: Event, then: &BTreeMap<WsId, WsId>, now: bool) {
        let mut within = Self::within_of(&event, then);
        if now {
            within.extend(Self::within_of(&event, &self.parents));
        }
        self.record(event, self.actor, within);
    }
    fn record(&mut self, event: Event, actor: Actor, within: Vec<WsId>) {
        let Some(next) = self.next.0.checked_add(1) else {
            self.exhausted = true;
            return;
        };
        let seq = self.next;
        self.next = Seq(next);
        // Evict first so a full ring never grows.
        if self.entries.len() == self.kept
            && let Some(evicted) = self.entries.pop_front()
        {
            self.within.remove(&evicted.seq);
        }
        if !within.is_empty() {
            self.within.insert(seq, within.into_boxed_slice());
        }
        self.entries.push_back(Entry { seq, event, actor });
    }
    pub(crate) fn teller(&mut self, at: PaneAt, generation: u64) -> Teller<'_> {
        Teller {
            at,
            generation,
            journal: self,
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

/// The journal at a pane's place: what a pane's methods that change what
/// events tell write to, at once, through a `pane::PaneMut`, which makes
/// it from the pane's own id. Only the session's parts, which hold the
/// session's journal, give one on it.
pub struct Teller<'a> {
    at: PaneAt,
    generation: u64,
    journal: &'a mut Journal,
}

impl Teller<'_> {
    /// The pane tells `what`.
    pub(crate) fn tell(&mut self, what: Told) {
        let actor = match what {
            Told::Bell
            | Told::Notification(_)
            | Told::Title(_)
            | Told::CommandStarted
            | Told::Done(_)
            | Told::HungUp
            | Told::ReadFailed(_)
            | Told::Exited(_) => Actor::Program(self.at.1, self.generation),
            Told::Renamed(_)
            | Told::Respawned(_)
            | Told::Resized(_)
            | Told::HistoryBudget(_)
            | Told::HistoryCleared => self.journal.current_actor(),
        };
        self.journal
            .push_from(Event::Pane(self.at, PaneEvent::Told(what)), actor);
    }
}

/// One thing that happened in the session, where: what can happen to a
/// pane, at a pane, and what can happen to a tab or a workspace, at one.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Pane(PaneAt, PaneEvent),
    Tab((WsId, TabId), Change),
    /// At the workspace it is in (none: the top), and the workspace.
    Workspace((Option<WsId>, WsId), Change),
    /// A tab's tree or fit changed. Consumers take a new snapshot.
    Layout((WsId, TabId)),
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
    Closed,
    Reordered,
    /// A workspace moved here from the one it was in (none: the top).
    Moved(Option<WsId>),
}

/// What happens to a pane, and in it.
#[derive(Clone, Debug, PartialEq)]
pub enum PaneEvent {
    Created,
    Closed,
    /// To another tab, from this workspace and tab.
    Moved((WsId, TabId)),
    /// What the pane itself tells: these alone a `Teller` takes; its
    /// place and lifetime are the session's parts' to journal.
    Told(Told),
}

/// What a pane tells of itself and its program (`Teller::tell`).
#[derive(Clone, Debug, PartialEq)]
pub enum Told {
    /// Named otherwise: its new name.
    Renamed(String),
    /// Its program started again (`respawn`), over one that ran: what
    /// that one left.
    Respawned(Left),
    Bell,
    Notification(Said),
    Title(Said),
    /// A shell command's output began (OSC 133 `C`).
    CommandStarted,
    /// A shell command ended, with its status if it is known.
    Done(Option<i32>),
    Resized(fux_vt::Size),
    /// The pane's retained history budget changed.
    HistoryBudget(usize),
    HistoryCleared,
    /// Its program closed its terminal and runs on.
    HungUp,
    /// Reading its program's terminal failed unexpectedly, so.
    ReadFailed(fuxix::Errno),
    /// Its program ended.
    Exited(End),
}

/// An event's fields after its name and place.
type Fields = Vec<(&'static str, Json)>;

/// A name, as the field `name`.
fn named(name: &str) -> Fields {
    vec![("name", Json::str(name.to_owned()))]
}

impl Change {
    /// Its name, and what else it says.
    fn said(&self) -> (&'static str, Fields) {
        match self {
            Change::Created(name) => (
                "created",
                vec![("name", name.as_ref().map_or(Json::Null, Json::str))],
            ),
            Change::Renamed(name) => ("renamed", named(name)),
            Change::Closed => ("closed", Vec::new()),
            Change::Reordered => ("reordered", Vec::new()),
            Change::Moved(_) => ("moved", Vec::new()),
        }
    }
}

impl PaneEvent {
    /// Its name, and what else it says.
    fn said(&self) -> (&'static str, Fields) {
        match self {
            PaneEvent::Created => ("created", Vec::new()),
            PaneEvent::Closed => ("closed", Vec::new()),
            PaneEvent::Moved(_) => ("moved", Vec::new()),
            PaneEvent::Told(told) => told.said(),
        }
    }
}

impl Told {
    /// Its name, and what else it says.
    fn said(&self) -> (&'static str, Fields) {
        // A program's text, and `truncated` if it sent more.
        let said = |key, text: &str, cut: bool| {
            let cut = cut.then_some(("truncated", Json::Bool(true)));
            [(key, Json::str(text.to_owned()))]
                .into_iter()
                .chain(cut)
                .collect()
        };
        match self {
            Told::Renamed(name) => ("renamed", named(name)),
            Told::Respawned(left) => ("respawned", left.fields().collect()),
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
            Told::HistoryCleared => ("history_cleared", Vec::new()),
            Told::HungUp => ("hung_up", Vec::new()),
            Told::ReadFailed(error) => (
                "read_failed",
                vec![("read_error", Json::str(error.to_string()))],
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
            Event::Tab((ws, tab), _) | Event::Layout((ws, tab)) => (None, Some(*tab), Some(*ws)),
            Event::Zoomed((ws, tab), pane) => (*pane, Some(*tab), Some(*ws)),
            Event::Workspace((_, ws), _) => (None, None, Some(*ws)),
            Event::View { at, .. } => at.map_or((None, None, None), |((ws, tab), pane)| {
                (Some(pane), Some(tab), Some(ws))
            }),
            Event::Stopped(_) => (None, None, None),
        }
    }

    /// What it closed, if it is a close.
    fn closed(&self) -> Option<AnyRef> {
        match self {
            Event::Pane((_, pane), PaneEvent::Closed) => Some(AnyRef::Pane(*pane)),
            Event::Tab((_, tab), Change::Closed) => Some(AnyRef::Tab(*tab)),
            Event::Workspace((_, ws), Change::Closed) => Some(AnyRef::Workspace(*ws)),
            Event::Pane(..)
            | Event::Tab(..)
            | Event::Workspace(..)
            | Event::Layout(..)
            | Event::Zoomed(..)
            | Event::View { .. }
            | Event::Stopped(_) => None,
        }
    }

    fn json(&self) -> Json {
        let (name, fields) = match self {
            Event::Pane(_, what) => what.said(),
            Event::Layout(_) => ("laid_out", Vec::new()),
            // Unzoomed, `pane` is there as null.
            Event::Zoomed(_, pane) => (
                "zoomed",
                pane.map_or_else(|| vec![("pane", Json::Null)], |_| Vec::new()),
            ),
            Event::View { change, .. } => (change.name(), Vec::new()),
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
/// told, `left` for what an end left, `size` resized to. The test
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
) -> Vec<Event> {
    let mut events = vec![
        Event::Pane(at, PaneEvent::Created),
        Event::Pane(at, PaneEvent::Closed),
        Event::Pane(at, PaneEvent::Moved(at.0)),
        Event::Layout(at.0),
        Event::Zoomed(at.0, Some(at.1)),
        Event::Zoomed(at.0, None),
    ];
    for change in [
        Change::Created(None),
        Change::Created(Some(name.to_owned())),
        Change::Renamed(name.to_owned()),
        Change::Closed,
        Change::Reordered,
    ] {
        events.push(Event::Tab(at.0, change.clone()));
        events.push(Event::Workspace((Some(at.0.0), at.0.0), change));
    }
    events.push(Event::Workspace(
        (None, at.0.0),
        Change::Moved(Some(at.0.0)),
    ));
    let [title, notification] = said;
    let ended = |ended| Told::Exited(End { ended, left });
    for told in [
        Told::Renamed(name.to_owned()),
        Told::Respawned(left),
        Told::Bell,
        notification,
        title,
        Told::CommandStarted,
        Told::Done(None),
        Told::Done(Some(i32::MIN)),
        Told::Resized(size),
        // The most a pane's budget is (`command::HistoryBytes`).
        Told::HistoryBudget(isize::MAX.unsigned_abs()),
        Told::HistoryCleared,
        Told::HungUp,
        Told::ReadFailed(fuxix::Errno::INVAL),
        ended(fux_wire::answer::Ended::Lost),
        ended(fux_wire::answer::Ended::Exited(i32::MIN)),
        ended(fux_wire::answer::Ended::Signalled(i32::MIN)),
    ] {
        events.push(Event::Pane(at, PaneEvent::Told(told)));
    }
    for change in [
        ViewChange::Attached,
        ViewChange::Detached,
        ViewChange::Shown,
        ViewChange::Resized,
        ViewChange::Screen,
    ] {
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
    let kept = fux_wire::command::JournalEntries::new(64).ok_or("journal")?;
    let mut journal = Journal::new(kept);
    let mut reader = Reader::new(Scope::All, &journal, run, None)?;
    let size = fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?;
    let events = every_kind(at, "name", said, left, size);
    for (event, actor) in events.into_iter().zip(actors.into_iter().cycle()) {
        journal.push_from(event, actor);
    }
    reader.batch(&journal, true)
}

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
                "{next} is from another server run, not this one ({run})"
            ));
        }
        if next.seq > end.seq {
            return Err(format!(
                "{next} is past the journal's end ({end}): this run never gave it"
            ));
        }
        Ok(Self { scope, next })
    }
    fn wants(&self, journal: &Journal, entry: &Entry) -> bool {
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
                let within = journal.within.get(&entry.seq);
                ws == Some(w) || within.is_some_and(|within| within.contains(&w))
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
                matches!(event, Event::View { client, change: ViewChange::Detached, .. } if *client == c)
            }
        }
    }
    /// Every event it wants from its cursor to the journal's end, or to
    /// the event that ends what it reads: its scope's close, or the stop.
    pub fn batch(&mut self, journal: &Journal, exists: bool) -> Result<Batch, String> {
        if journal.exhausted {
            return Err("journal cursor IDs exhausted".into());
        }
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
            if self.wants(journal, entry) {
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
                    | Event::Layout(_)
                    | Event::Zoomed(..)
                    | Event::Stopped(_) => None,
                };
                let moved_from = if let Event::Pane(_, PaneEvent::Moved(from)) = entry.event {
                    Some(from)
                } else {
                    None
                };
                let parent = match entry.event {
                    Event::Workspace((parent, _), _) => parent,
                    Event::Pane(..)
                    | Event::Tab(..)
                    | Event::Layout(_)
                    | Event::Zoomed(..)
                    | Event::View { .. }
                    | Event::Stopped(_) => None,
                };
                let item = Item {
                    data: entry.event.json(),
                    pane,
                    tab,
                    workspace,
                    parent,
                    client,
                    moved_from,
                    origin: entry.actor,
                    next,
                };
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
    fux_wire::code::Raise::IdsExhausted.into()
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
        if reader.next == journal.end(session.run) && exists && now < self.deadline {
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
    /// The events a test's journal keeps: as many as the bundled
    /// `journal-entries`.
    const KEPT: usize = 1 << 14;
    fn journal() -> Result<Journal, String> {
        let kept = fux_wire::command::JournalEntries::new(KEPT).ok_or("journal")?;
        Ok(Journal::new(kept))
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
                | Event::Layout(_)
                | Event::Zoomed(..)
                | Event::View { .. }
                | Event::Stopped(_) => None,
            });
        let title = texts.next().ok_or("title event")?;
        let notification = texts.next().ok_or("notification event")?;
        let left = Left { input: usize::MAX };
        let size = fux_vt::Size::new(4096, 4096)?;
        Ok(every_kind(at, &name, [title, notification], left, size))
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
                    PaneEvent::Created
                    | PaneEvent::Closed
                    | PaneEvent::Moved(_)
                    | PaneEvent::Told(
                        Told::Renamed(_)
                        | Told::Respawned(_)
                        | Told::Bell
                        | Told::Notification(_)
                        | Told::Title(_)
                        | Told::CommandStarted
                        | Told::Done(_)
                        | Told::Resized(_)
                        | Told::HistoryBudget(_)
                        | Told::HistoryCleared
                        | Told::HungUp
                        | Told::ReadFailed(_)
                        | Told::Exited(_),
                    ),
                )
                | Event::Tab(
                    _,
                    Change::Created(_)
                    | Change::Renamed(_)
                    | Change::Closed
                    | Change::Reordered
                    | Change::Moved(_),
                )
                | Event::Workspace(
                    _,
                    Change::Created(_)
                    | Change::Renamed(_)
                    | Change::Closed
                    | Change::Reordered
                    | Change::Moved(_),
                )
                | Event::View {
                    change:
                        ViewChange::Attached
                        | ViewChange::Detached
                        | ViewChange::Shown
                        | ViewChange::Resized
                        | ViewChange::Screen,
                    ..
                }
                | Event::Layout(_)
                | Event::Zoomed(..)
                | Event::Stopped(_) => {}
            }
            let json = event.json();
            let Some(Json::String(name)) = json.get("event") else {
                return Err("event name".into());
            };
            names.insert(name.clone());
        }
        let docs = include_str!("../../docs/json.md");
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
        assert!(docs.contains("last `journal-entries` entries (16,384"));
        Ok(())
    }

    #[test]
    fn a_full_ring_keeps_its_count_window_without_growing() -> TestResult {
        let events = maximum_events('𐀀')?;
        let maximum = events
            .iter()
            .find(|e| matches!(e, Event::Pane(_, PaneEvent::Told(Told::Notification(_)))))
            .ok_or("notification")?;
        let mut journal = journal()?;
        assert_eq!(journal.entries.capacity(), 0, "no eager ring allocation");
        let mut oldest = Reader::new(Scope::All, &journal, run()?, None)?;
        for n in 1..=KEPT {
            let old_capacity = journal.entries.capacity();
            journal.push(maximum.clone());
            assert_eq!(journal.entries.len(), n);
            assert!(journal.entries.capacity() <= KEPT);
            if n > old_capacity {
                assert_eq!(journal.entries.capacity(), n.max(4).next_power_of_two());
            }
        }
        let capacity = journal.entries.capacity();
        let full = oldest.batch(&journal, true)?;
        assert_eq!((full.missed, full.first.seq), (0, Seq(0)));
        let mut lost = Reader::new(
            Scope::All,
            &journal,
            run()?,
            Some(Cursor {
                seq: Seq(0),
                run: run()?,
            }),
        )?;
        journal.push(maximum.clone());
        assert_eq!(journal.entries.capacity(), capacity);
        assert_eq!(journal.entries.len(), KEPT);
        // Its first is the oldest it holds, as many as a reader from 0 missed.
        let wrapped = lost.batch(&journal, true)?;
        assert_eq!((wrapped.missed, wrapped.first.seq), (1, Seq(1)));
        assert_eq!(lost.batch(&journal, true)?.missed, 0);
        if cfg!(target_pointer_width = "64") {
            assert_eq!(std::mem::size_of::<Entry>(), 80);
            assert_eq!(std::mem::size_of::<Journal>(), 120);
            assert_eq!(std::mem::size_of::<Event>(), 56);
        }
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
                .push(Event::Workspace((None, WsId::of(2)), Change::Reordered));
        }
        for _ in 0..3 {
            assert!(
                waiting.answer(&session, now).is_none(),
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
        journal.push(Event::Tab(from, Change::Closed));
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
        assert!(waiting.answer(&session, now).is_none());
        let answer = waiting
            .answer(&session, crate::after(now, Duration::from_secs(5)))
            .ok_or("timeout")?;
        let Response::Returned(Body::Events(batch)) = answer.response else {
            return Err("batch".into());
        };
        assert_eq!(batch.status, Status::Timeout);
        assert_eq!(batch.next, session.parts.journal().end(session.run));
        let journal = session.parts.journal_mut();
        journal.push_from(Event::Stopped("test".into()), Actor::Outside);
        journal.push(bell(1));
        let answer = waiting.answer(&session, now).ok_or("stopped")?;
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

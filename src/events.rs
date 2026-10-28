//! Bounded journal invalidations and finite cursor batches. A later snapshot
//! may see newer state; events do not replicate live OS facts.
use crate::command::{AnyRef, Run};
use crate::id::{BufferId, ClientId, PaneId, TabId, WsId};
use crate::inspect::Scope;
use crate::json::Json;
use crate::pane::{End, Left, NOTIFICATION_MAX, Said, TITLE_MAX};
use crate::wire::wire;
use std::cell::Cell;
use std::collections::VecDeque;

pub(crate) const KEPT: usize = 1 << 14;
pub(crate) const KEPT_BYTES: usize = 8 << 20;
pub const BATCH_EVENTS: usize = 64;
pub const BATCH_SCAN: usize = 256;
pub const BATCH_BYTES: usize = 256 << 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Seq(u64);

/// The next entry to consume, in exactly one server run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub seq: Seq,
    pub run: Run,
}
impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.seq.0, self.run)
    }
}
impl std::str::FromStr for Cursor {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, ()> {
        let (seq, run) = text.split_once(':').ok_or(())?;
        let cursor = Self {
            seq: Seq(crate::id::number(seq).ok_or(())?),
            run: run.parse()?,
        };
        (cursor.to_string() == text).then_some(cursor).ok_or(())
    }
}

wire! {
/// Supplied context, never authenticated human or agent identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Actor {
    Unknown,
    Client(client: ClientId),
    Pane(pane: PaneId, generation: u64),
    Program(pane: PaneId, generation: u64),
    #[default]
    Server,
}
}
impl Actor {
    pub(crate) fn kind(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Server => "server",
            Self::Client(_) => "client",
            Self::Pane(..) => "pane",
            Self::Program(..) => "program",
        }
    }
    fn json(self) -> Json {
        let extra = match self {
            Self::Unknown | Self::Server => Vec::new(),
            Self::Client(c) => vec![("client", Json::str(c.to_string()))],
            Self::Pane(p, generation) | Self::Program(p, generation) => vec![
                ("pane", Json::str(p.to_string())),
                ("generation", Json::uint(generation)),
            ],
        };
        Json::object(
            std::iter::once(("kind", Json::str(self.kind())))
                .chain(extra)
                .collect(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewChange {
    Attached,
    Detached,
    Shown,
    Focused,
    Zoomed,
    Resized,
    Presentation,
    Screen,
    Notice,
}
impl ViewChange {
    fn name(self) -> &'static str {
        match self {
            Self::Attached => "attached",
            Self::Detached => "detached",
            Self::Shown => "shown",
            Self::Focused => "focused",
            Self::Zoomed => "zoomed",
            Self::Resized => "view_resized",
            Self::Presentation => "presentation",
            Self::Screen => "screen",
            Self::Notice => "notice",
        }
    }
}

#[derive(Debug)]
struct Entry {
    seq: Seq,
    event: Event,
    actor: Actor,
    bytes: usize,
}

#[derive(Debug, Default)]
pub struct Journal {
    entries: VecDeque<Entry>,
    next: Seq,
    bytes: usize,
    actor: Actor,
    exhausted: bool,
}
impl Journal {
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
    fn push_from(&mut self, event: Event, actor: Actor) {
        let Some(next) = self.next.0.checked_add(1) else {
            self.exhausted = true;
            return;
        };
        let heap = match &event {
            Event::Tab(_, Change::Created(name)) | Event::Workspace(_, Change::Created(name)) => {
                name.as_ref().map_or(0, String::capacity)
            }
            Event::Tab(_, Change::Renamed(name))
            | Event::Workspace(_, Change::Renamed(name))
            | Event::Pane(_, PaneEvent::Told(Told::Renamed(name))) => name.capacity(),
            Event::Pane(_, PaneEvent::Told(Told::Notification(n))) => n.retained_bytes(),
            Event::Pane(_, PaneEvent::Told(Told::Title(t))) => t.retained_bytes(),
            Event::Pane(
                _,
                PaneEvent::Created
                | PaneEvent::Closed
                | PaneEvent::Moved(_)
                | PaneEvent::Told(
                    Told::Respawned(_)
                    | Told::Bell
                    | Told::Done(_)
                    | Told::Resized(_)
                    | Told::HistoryBudget(_)
                    | Told::HistoryCleared
                    | Told::Exited(_),
                ),
            )
            | Event::Tab(_, Change::Closed | Change::Reordered)
            | Event::Workspace(_, Change::Closed | Change::Reordered)
            | Event::Layout(_)
            | Event::Arranged(_)
            | Event::View { .. }
            | Event::Copied { .. } => 0,
        };
        let bytes = heap.saturating_add(std::mem::size_of::<Entry>());
        let seq = self.next;
        self.next = Seq(next);
        if bytes > KEPT_BYTES {
            return;
        }
        while self.entries.len() >= KEPT || self.bytes.saturating_add(bytes) > KEPT_BYTES {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.entries.push_back(Entry {
            seq,
            event,
            actor,
            bytes,
        });
        self.bytes = self.bytes.saturating_add(bytes);
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
    fn from(&self, from: Seq) -> (u64, impl Iterator<Item = &Entry>) {
        let first = self.entries.front().map_or(self.next, |entry| entry.seq);
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
            | Told::Done(_)
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
    /// One complete tab layout replacement.
    Arranged((WsId, TabId)),
    Workspace(WsId, Change),
    /// A tab's tree or fit changed. Consumers take a new snapshot.
    Layout((WsId, TabId)),
    /// A client's discrete view state changed at this place, if any.
    View {
        client: ClientId,
        at: Option<PaneAt>,
        change: ViewChange,
    },
    /// An admitted retained copy; its bytes are held only in the buffer.
    Copied {
        buffer: BufferId,
        at: Option<PaneAt>,
    },
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
    Notification(Said<NOTIFICATION_MAX>),
    Title(Said<TITLE_MAX>),
    /// A shell command ended, with its status if it is known.
    Done(Option<i32>),
    Resized(fux_vt::Size),
    /// The pane's retained history budget changed.
    HistoryBudget(usize),
    HistoryCleared,
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
            Told::Done(status) => (
                "done",
                vec![("status", status.map_or(Json::Null, Json::int))],
            ),
            Told::HistoryBudget(bytes) => (
                "history_budget",
                vec![("history_bytes", Json::count(*bytes))],
            ),
            Told::HistoryCleared => ("history_cleared", Vec::new()),
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
            Event::Tab((ws, tab), _) | Event::Arranged((ws, tab)) | Event::Layout((ws, tab)) => {
                (None, Some(*tab), Some(*ws))
            }
            Event::Workspace(ws, _) => (None, None, Some(*ws)),
            Event::View { at, .. } | Event::Copied { at, .. } => at
                .map_or((None, None, None), |((ws, tab), pane)| {
                    (Some(pane), Some(tab), Some(ws))
                }),
        }
    }

    /// What it closed, if it is a close.
    fn closed(&self) -> Option<AnyRef> {
        match self {
            Event::Pane((_, pane), PaneEvent::Closed) => Some(AnyRef::Pane(*pane)),
            Event::Tab((_, tab), Change::Closed) => Some(AnyRef::Tab(*tab)),
            Event::Workspace(ws, Change::Closed) => Some(AnyRef::Workspace(*ws)),
            Event::Pane(..)
            | Event::Tab(..)
            | Event::Workspace(..)
            | Event::Arranged(..)
            | Event::Layout(..)
            | Event::View { .. }
            | Event::Copied { .. } => None,
        }
    }

    fn json(&self) -> Json {
        let (name, fields) = match self {
            Event::Pane(_, what) => what.said(),
            Event::Arranged(_) => ("arranged", Vec::new()),
            Event::Layout(_) => ("laid_out", Vec::new()),
            Event::View { change, .. } => (change.name(), Vec::new()),
            Event::Copied { buffer, .. } => {
                ("copied", vec![("buffer", Json::str(buffer.to_string()))])
            }
            Event::Tab(_, change) | Event::Workspace(_, change) => change.said(),
        };
        Json::object(
            std::iter::once(("event", Json::str(name)))
                .chain(fields)
                .collect(),
        )
    }
}

/// One bounded scan, including entries excluded by its scope.
#[derive(Clone, Copy)]
pub struct Reader {
    scope: Scope,
    next: Cursor,
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Item {
        pub data: Json,
        pub pane: Option<PaneId>,
        pub tab: Option<TabId>,
        pub workspace: Option<WsId>,
        pub client: Option<ClientId>,
        pub moved_from: Option<(WsId, TabId)>,
        pub origin: Actor,
        pub next: Cursor,
    }
}
impl Item {
    pub fn json(&self) -> Json {
        let mut fields = Vec::new();
        if let Some(event) = self.data.get("event") {
            fields.push(("event".into(), event.clone()));
        }
        for (key, value) in [
            ("pane", self.pane.map(|id| id.to_string())),
            ("tab", self.tab.map(|id| id.to_string())),
            ("workspace", self.workspace.map(|id| id.to_string())),
            ("client", self.client.map(|id| id.to_string())),
            ("from_tab", self.moved_from.map(|(_, tab)| tab.to_string())),
            (
                "from_workspace",
                self.moved_from.map(|(ws, _)| ws.to_string()),
            ),
        ] {
            if let Some(value) = value {
                fields.push((key.into(), Json::str(value)));
            }
        }
        if let Json::Object(data) = &self.data {
            fields.extend(data.iter().filter(|(k, _)| k != "event").cloned());
        }
        fields.push(("origin".into(), self.origin.json()));
        fields.push(("next".into(), Json::str(self.next.to_string())));
        Json::Object(fields)
    }
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Status { Continuing, Timeout, Gone, Stopped(reason: String), }
}
impl Status {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Continuing => "continuing",
            Self::Timeout => "timeout",
            Self::Gone => "gone",
            Self::Stopped(_) => "stopped",
        }
    }
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Batch {
        pub events: Vec<Item>,
        pub next: Cursor,
        pub missed: u64,
        pub status: Status,
    }
}
impl Batch {
    pub fn json(&self) -> Json {
        let mut fields = vec![
            (
                "events",
                Json::Array(self.events.iter().map(Item::json).collect()),
            ),
            ("next", Json::str(self.next.to_string())),
            ("missed", Json::uint(self.missed)),
            ("status", Json::str(self.status.name())),
        ];
        if let Status::Stopped(reason) = &self.status {
            fields.push(("reason", Json::str(reason)));
        }
        Json::object(fields)
    }
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
    fn wants(&self, event: &Event) -> bool {
        let (pane, tab, ws) = event.ids();
        let from = if let Event::Pane(_, PaneEvent::Moved(from)) = event {
            Some(*from)
        } else {
            None
        };
        match self.scope {
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
            Scope::All => false,
            Scope::Target(t) => event.closed() == Some(t),
            Scope::Client(c) => {
                matches!(event, Event::View { client, change: ViewChange::Detached, .. } if *client == c)
            }
        }
    }
    pub fn batch(&mut self, journal: &Journal, exists: bool) -> Result<Batch, String> {
        self.scan(journal, exists, BATCH_SCAN)
            .map(|(batch, _)| batch)
    }
    fn scan(
        &mut self,
        journal: &Journal,
        exists: bool,
        limit: usize,
    ) -> Result<(Batch, usize), String> {
        if journal.exhausted {
            return Err("journal cursor IDs exhausted".into());
        }
        let (mut missed, entries) = journal.from(self.next.seq);
        self.next.seq = Seq(self.next.seq.0.saturating_add(missed));
        let mut events = Vec::new();
        let mut bytes = 0usize;
        let mut gone = false;
        let mut entries = entries;
        let mut scanned = 0usize;
        for _ in 0..limit {
            if events.len() == BATCH_EVENTS {
                break;
            }
            let Some(entry) = entries.next() else {
                missed = missed.saturating_add(journal.next.0.saturating_sub(self.next.seq.0));
                self.next.seq = journal.next;
                break;
            };
            missed = missed.saturating_add(entry.seq.0.saturating_sub(self.next.seq.0));
            self.next.seq = entry.seq;
            let mut value = None;
            if self.wants(&entry.event) {
                let next = Cursor {
                    seq: Seq(entry.seq.0.saturating_add(1)),
                    run: self.next.run,
                };
                let (pane, tab, workspace) = entry.event.ids();
                let client = match entry.event {
                    Event::View { client, .. } => Some(client),
                    Event::Pane(..)
                    | Event::Tab(..)
                    | Event::Arranged(_)
                    | Event::Workspace(..)
                    | Event::Layout(_)
                    | Event::Copied { .. } => None,
                };
                let moved_from = if let Event::Pane(_, PaneEvent::Moved(from)) = entry.event {
                    Some(from)
                } else {
                    None
                };
                let item = Item {
                    data: entry.event.json(),
                    pane,
                    tab,
                    workspace,
                    client,
                    moved_from,
                    origin: entry.actor,
                    next,
                };
                let json = item.json();
                let size = json.render().len();
                // One event fits by the journal's payload bounds; stop before
                // advancing over an event this reply cannot hold.
                if size > BATCH_BYTES {
                    return Err("one journal event exceeds the batch byte limit".into());
                }
                if bytes.saturating_add(size) > BATCH_BYTES {
                    break;
                }
                bytes = bytes.saturating_add(size);
                value = Some(item);
            }
            self.next.seq = Seq(entry.seq.0.saturating_add(1));
            scanned = scanned.saturating_add(1);
            events.extend(value);
            if self.ends(&entry.event) {
                gone = true;
                break;
            }
        }
        let caught_up = self.next == journal.end(self.next.run);
        Ok((
            Batch {
                events,
                next: self.next,
                missed,
                status: if gone || caught_up && !exists {
                    Status::Gone
                } else {
                    Status::Continuing
                },
            },
            scanned,
        ))
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
        let mut lines = String::new();
        for item in batch.events {
            let mut event = item.json();
            if let Json::Object(fields) = &mut event {
                fields.retain(|(k, _)| k != "origin" && k != "next");
            }
            lines.push_str(&event.render());
            lines.push('\n');
        }
        (lines, batch.status == Status::Gone)
    }
}

/// A pending finite read, owned by its requesting connection.
pub struct Read {
    reader: Cell<Reader>,
    scanned: Cell<usize>,
    pub deadline: std::time::Instant,
}

fn reply(batch: Result<Batch, String>) -> crate::protocol::Reply {
    match batch {
        Ok(batch) => crate::protocol::Body::Events(Box::new(batch)).into(),
        Err(why) => crate::protocol::Reply::failed(
            crate::code::Code::from(crate::code::Raise::TooLong),
            why,
        ),
    }
}

impl Read {
    pub fn new(reader: Reader, deadline: std::time::Instant) -> Self {
        Self {
            reader: Cell::new(reader),
            scanned: Cell::new(0),
            deadline,
        }
    }
    pub fn answer(
        &self,
        session: &crate::session::Session,
        now: std::time::Instant,
    ) -> Option<crate::protocol::Reply> {
        let mut reader = self.reader.get();
        let journal = session.parts.journal();
        if journal.exhausted {
            return Some(crate::protocol::Reply::failed(
                crate::code::Code::IdsExhausted,
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
        let (mut batch, scanned) = match reader.scan(
            journal,
            exists,
            BATCH_SCAN.saturating_sub(self.scanned.get()),
        ) {
            Ok(batch) => batch,
            Err(why) => return Some(reply(Err(why))),
        };
        self.reader.set(reader);
        self.scanned.set(self.scanned.get().saturating_add(scanned));
        if batch.events.is_empty() && batch.missed == 0 && batch.status == Status::Continuing {
            if now >= self.deadline {
                batch.status = Status::Timeout;
            } else if self.scanned.get() < BATCH_SCAN {
                return None;
            }
        }
        Some(reply(Ok(batch)))
    }
    pub(crate) fn stopped(
        &self,
        session: &crate::session::Session,
        reason: &str,
    ) -> crate::protocol::Reply {
        let mut reader = self.reader.get();
        reply(
            reader
                .scan(
                    session.parts.journal(),
                    true,
                    BATCH_SCAN.saturating_sub(self.scanned.get()),
                )
                .map(|(mut batch, _)| {
                    batch.status = Status::Stopped(reason.to_owned());
                    batch
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{AnyRef, Limit, Request};
    use crate::protocol::{Body, Response};
    use crate::session::testing::TestResult;
    use crate::session::{Begun, Caller, Origin, testing::started};
    use std::time::{Duration, Instant};

    fn run() -> Result<Run, String> {
        "1-0".parse().map_err(|()| "run".into())
    }
    fn bell(pane: u32) -> Event {
        Event::Pane(
            ((WsId::of(1), TabId::of(1)), PaneId::of(pane)),
            PaneEvent::Told(Told::Bell),
        )
    }

    #[test]
    fn the_event_and_status_tables_match_the_finite_schema() -> TestResult {
        use std::collections::BTreeSet;
        let at = ((WsId::of(1), TabId::of(1)), PaneId::of(1));
        let mut events = vec![
            Event::Pane(at, PaneEvent::Created),
            Event::Pane(at, PaneEvent::Closed),
            Event::Pane(at, PaneEvent::Moved(at.0)),
            Event::Arranged(at.0),
            Event::Layout(at.0),
            Event::Copied {
                buffer: "b1:1-0".parse().map_err(|()| "buffer")?,
                at: Some(at),
            },
        ];
        for change in [
            Change::Created(None),
            Change::Renamed(String::new()),
            Change::Closed,
            Change::Reordered,
        ] {
            events.push(Event::Tab(at.0, change.clone()));
            events.push(Event::Workspace(at.0.0, change));
        }
        for told in [
            Told::Renamed(String::new()),
            Told::Respawned(Left::default()),
            Told::Bell,
            Told::Notification(Said::default()),
            Told::Title(Said::default()),
            Told::Done(None),
            Told::Resized(fux_vt::Size::new(24, 80)?),
            Told::HistoryBudget(0),
            Told::HistoryCleared,
            Told::Exited(End {
                ended: crate::process::Ended::Lost,
                left: Left::default(),
            }),
        ] {
            events.push(Event::Pane(at, PaneEvent::Told(told)));
        }
        for change in [
            ViewChange::Attached,
            ViewChange::Detached,
            ViewChange::Shown,
            ViewChange::Focused,
            ViewChange::Zoomed,
            ViewChange::Resized,
            ViewChange::Presentation,
            ViewChange::Screen,
            ViewChange::Notice,
        ] {
            events.push(Event::View {
                client: ClientId::of(1),
                at: Some(at),
                change,
            });
        }
        let mut names = BTreeSet::from(["ready".to_owned(), "missed".to_owned()]);
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
                        | Told::Done(_)
                        | Told::Resized(_)
                        | Told::HistoryBudget(_)
                        | Told::HistoryCleared
                        | Told::Exited(_),
                    ),
                )
                | Event::Tab(
                    _,
                    Change::Created(_) | Change::Renamed(_) | Change::Closed | Change::Reordered,
                )
                | Event::Workspace(
                    _,
                    Change::Created(_) | Change::Renamed(_) | Change::Closed | Change::Reordered,
                )
                | Event::View {
                    change:
                        ViewChange::Attached
                        | ViewChange::Detached
                        | ViewChange::Shown
                        | ViewChange::Focused
                        | ViewChange::Zoomed
                        | ViewChange::Resized
                        | ViewChange::Presentation
                        | ViewChange::Screen
                        | ViewChange::Notice,
                    ..
                }
                | Event::Arranged(_)
                | Event::Layout(_)
                | Event::Copied { .. } => {}
            }
            let json = event.json();
            let Some(Json::String(name)) = json.get("event") else {
                return Err("event name".into());
            };
            names.insert(name.clone());
        }
        let docs = include_str!("../docs/json.md");
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
        assert!(docs.contains(&format!("last {},{:03} entries", KEPT / 1000, KEPT % 1000)));
        assert!(docs.contains(&format!("{} MiB retained-entry", KEPT_BYTES / (1 << 20))));
        Ok(())
    }

    #[test]
    fn scoped_reads_wait_through_unrelated_entries_without_rescanning() -> TestResult {
        let mut session = started()?;
        let now = Instant::now();
        let request = Request::NextEvents {
            scope: Scope::Target(AnyRef::Pane(PaneId::of(1))),
            from: None,
            timeout: Limit::read(5)?,
        };
        let Begun::Waiting(waiting) = session.begin(request, &Origin::Cli(Caller::default()), now)
        else {
            return Err("did not wait".into());
        };
        for _ in 0..BATCH_SCAN / 2 {
            session
                .parts
                .journal_mut()
                .push(Event::Workspace(WsId::of(2), Change::Reordered));
        }
        for _ in 0..3 {
            assert!(
                waiting.answer(&session, now).is_none(),
                "unrelated entries ended the wait"
            );
        }
        for _ in 0..BATCH_SCAN / 2 {
            session
                .parts
                .journal_mut()
                .push(Event::Workspace(WsId::of(2), Change::Reordered));
        }
        let reply = waiting
            .answer(&session, now)
            .ok_or("scan limit did not return progress")?;
        let Response::Returned(Body::Events(batch)) = reply.response else {
            return Err("batch".into());
        };
        assert!(batch.events.is_empty());
        assert_eq!(batch.status, Status::Continuing);
        assert_eq!(batch.next, session.parts.journal().end(session.run));
        Ok(())
    }

    #[test]
    fn batches_resume_at_the_exact_consumed_prefix() -> TestResult {
        let mut journal = Journal::default();
        let mut reader = Reader::new(Scope::All, &journal, run()?, None)?;
        for _ in 0..BATCH_EVENTS.saturating_add(3) {
            journal.push(bell(1));
        }
        let first = reader.batch(&journal, true)?;
        assert_eq!(first.events.len(), BATCH_EVENTS);
        assert_eq!(first.events.last().map(|e| e.next), Some(first.next));
        assert!(first.json().render().len() < BATCH_BYTES);
        let second =
            Reader::new(Scope::All, &journal, run()?, Some(first.next))?.batch(&journal, true)?;
        assert_eq!(second.events.len(), 3);
        assert_eq!(second.next, journal.end(run()?));
        Ok(())
    }

    #[test]
    fn filtered_entries_spend_the_scan_budget_and_advance_the_cursor() -> TestResult {
        let mut journal = Journal::default();
        let mut reader = Reader::new(
            Scope::Target(AnyRef::Pane(PaneId::of(2))),
            &journal,
            run()?,
            None,
        )?;
        for _ in 0..BATCH_SCAN.saturating_add(1) {
            journal.push(bell(1));
        }
        journal.push(bell(2));
        let empty = reader.batch(&journal, true)?;
        assert!(empty.events.is_empty());
        assert_eq!(empty.next.seq, Seq(u64::try_from(BATCH_SCAN)?));
        assert_eq!(empty.status, Status::Continuing);
        let matching = reader.batch(&journal, true)?;
        assert_eq!(matching.events.len(), 1);
        assert_eq!(matching.next, journal.end(run()?));
        Ok(())
    }

    #[test]
    fn retention_loss_is_exact_even_for_a_filtered_scope() -> TestResult {
        let mut journal = Journal::default();
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
        let start = journal.end(run()?);
        let mut strings = Journal::default();
        let mut behind = Reader::new(Scope::All, &strings, run()?, None)?;
        // Mutation owners admit names of at most NAME_MAX bytes. Make the
        // stored capacity large to exercise byte retention independently.
        for _ in 0..KEPT {
            let mut name = String::with_capacity(1024);
            name.push('x');
            strings.push(Event::Workspace(WsId::of(1), Change::Renamed(name)));
        }
        assert!(strings.entries.len() < KEPT);
        assert!(strings.bytes <= KEPT_BYTES);
        let loss = behind.batch(&strings, true)?;
        assert_eq!(
            loss.missed,
            u64::try_from(KEPT.saturating_sub(strings.entries.len()))?
        );
        assert!(start.seq > Seq(0));
        Ok(())
    }

    #[test]
    fn impossible_entries_and_exhausted_cursors_refuse_without_unbounded_storage() -> TestResult {
        let mut journal = Journal::default();
        let mut reader = Reader::new(Scope::All, &journal, run()?, None)?;
        journal.push(bell(1));
        journal.push(Event::Workspace(
            WsId::of(1),
            Change::Renamed(String::with_capacity(KEPT_BYTES.saturating_add(1))),
        ));
        assert!(journal.bytes <= KEPT_BYTES);
        let batch = reader.batch(&journal, true)?;
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.missed, 1);
        assert_eq!(batch.next, journal.end(run()?));
        journal.push(Event::Workspace(
            WsId::of(1),
            Change::Renamed(std::iter::repeat_n('x', BATCH_BYTES.saturating_add(1)).collect()),
        ));
        assert!(reader.batch(&journal, true).is_err());
        journal.next = Seq(u64::MAX);
        journal.push(bell(1));
        assert!(Reader::new(Scope::All, &journal, run()?, None).is_err());
        assert!(reader.batch(&journal, true).is_err());
        Ok(())
    }

    #[test]
    fn cursors_refuse_other_runs_future_positions_and_noncanonical_text() -> TestResult {
        let journal = Journal::default();
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
        let mut journal = Journal::default();
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

    #[test]
    fn empty_reads_wait_once_then_return_timeout_or_stopped() -> TestResult {
        let mut session = started()?;
        let now = Instant::now();
        let scope = Scope::All;
        let request = Request::NextEvents {
            scope,
            from: None,
            timeout: Limit::read(5)?,
        };
        let Begun::Waiting(waiting) = session.begin(request, &Origin::Cli(Caller::default()), now)
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
        let crate::wait::Waiting::Events(read) = &*waiting else {
            return Err("event read".into());
        };
        let Response::Returned(Body::Events(stopped)) = read.stopped(&session, "test").response
        else {
            return Err("stopped batch".into());
        };
        assert_eq!(stopped.status, Status::Stopped("test".into()));
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

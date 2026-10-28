//! `fux all events`: what happens in the session, a line each as it happens,
//! for a script that would otherwise poll `fux ls json`. Each change to the
//! session's workspaces, tabs and panes, and what each pane's program does
//! that events tell (a bell, a notification, a title, a shell command's
//! end, a resize, a respawn, an exit), is written in one journal where it
//! happens, in the order it happens (`workspace::Parts`, and a pane's own
//! methods through a [`Teller`]); each subscriber reads the journal from
//! its own place in it, after each round of what the server read and did.
use crate::code::Code;
use crate::command::{AnyRef, Run};
use crate::id::{PaneId, TabId, WsId};
use crate::json::Json;
use crate::pane::{End, Left, NOTIFICATION_MAX, Said, TITLE_MAX};
use crate::protocol::Reply;
use std::collections::VecDeque;

/// The most a subscriber's output may hold unsent before it is told no
/// more for now: what it has yet to read waits in the journal.
pub const BACKLOG: usize = 1 << 20;

/// How many entries the journal keeps: a subscriber that falls further
/// behind is told how many it missed.
pub(crate) const KEPT: usize = 1 << 14;

/// A number of the journal's: the order things happened in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Seq(u64);

/// A place in one server run's journal: the number of the next entry it
/// records, as `ls` and a stream's `ready` give it. Written
/// `42:48211-1867a3f9c2b8e7d6`, `SEQ:RUN`, one word: a stream from it
/// misses nothing that came after unsaid, and a server tells its own
/// run's from another's.
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

/// `SEQ:RUN`, as a cursor is written: one spelling only.
impl std::str::FromStr for Cursor {
    type Err = ();
    fn from_str(text: &str) -> Result<Cursor, ()> {
        let (seq, run) = text.split_once(':').ok_or(())?;
        let cursor = Cursor {
            seq: Seq(crate::id::number(seq).ok_or(())?),
            run: run.parse()?,
        };
        (cursor.to_string() == text).then_some(cursor).ok_or(())
    }
}

/// What happened in the session, in order, each entry numbered: the last
/// [`KEPT`] entries.
#[derive(Debug, Default)]
pub struct Journal {
    entries: VecDeque<(Seq, Event)>,
    /// The number the next entry is given.
    next: Seq,
}

impl Journal {
    /// Records `event`, numbered after the last.
    pub(crate) fn push(&mut self, event: Event) {
        if self.entries.len() >= KEPT {
            self.entries.pop_front();
        }
        self.entries.push_back((self.next, event));
        // 2^64 entries are never reached: the number never stands still.
        self.next = Seq(self.next.0.saturating_add(1));
    }

    /// Where pane `at` tells what happens in it: here, as it happens.
    pub(crate) fn teller(&mut self, at: PaneAt) -> Teller<'_> {
        Teller { at, journal: self }
    }

    /// Where a reader starting now reads from, in server run `run`:
    /// nothing recorded yet.
    pub fn end(&self, run: Run) -> Cursor {
        let seq = self.next;
        Cursor { seq, run }
    }

    /// What came from `from` on: how many of those entries were let go,
    /// and those kept, oldest first.
    fn from(&self, from: Seq) -> (u64, impl Iterator<Item = &(Seq, Event)>) {
        let first = self.entries.front().map_or(self.next, |(seq, _)| *seq);
        let skip = self.entries.partition_point(|(seq, _)| *seq < from);
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
    journal: &'a mut Journal,
}

impl Teller<'_> {
    /// The pane tells `what`.
    pub(crate) fn tell(&mut self, what: Told) {
        self.journal
            .push(Event::Pane(self.at, PaneEvent::Told(what)));
    }
}

/// One thing that happened in the session, where: what can happen to a
/// pane, at a pane, and what can happen to a tab or a workspace, at one.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Pane(PaneAt, PaneEvent),
    Tab((WsId, TabId), Change),
    Workspace(WsId, Change),
}

/// What happens to a tab or a workspace.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Made, with its name.
    Created(Option<String>),
    /// Named otherwise: its new name.
    Renamed(String),
    Closed,
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
        }
    }
}

impl PaneEvent {
    /// Its name, and what else it says.
    fn said(&self) -> (&'static str, Fields) {
        match self {
            PaneEvent::Created => ("created", Vec::new()),
            PaneEvent::Closed => ("closed", Vec::new()),
            PaneEvent::Moved((ws, tab)) => (
                "moved",
                vec![
                    ("from_tab", Json::str(tab.to_string())),
                    ("from_workspace", Json::str(ws.to_string())),
                ],
            ),
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
            Event::Tab((ws, tab), _) => (None, Some(*tab), Some(*ws)),
            Event::Workspace(ws, _) => (None, None, Some(*ws)),
        }
    }

    /// What it closed, if it is a close.
    fn closed(&self) -> Option<AnyRef> {
        match self {
            Event::Pane((_, pane), PaneEvent::Closed) => Some(AnyRef::Pane(*pane)),
            Event::Tab((_, tab), Change::Closed) => Some(AnyRef::Tab(*tab)),
            Event::Workspace(ws, Change::Closed) => Some(AnyRef::Workspace(*ws)),
            Event::Pane(..) | Event::Tab(..) | Event::Workspace(..) => None,
        }
    }

    /// The JSON line that tells it.
    pub fn line(&self) -> String {
        let (name, fields) = match self {
            Event::Pane(_, what) => what.said(),
            Event::Tab(_, change) | Event::Workspace(_, change) => change.said(),
        };
        line(name, self.ids(), fields)
    }
}

/// How a stream of events ends: its last line, and its reply.
#[derive(Clone, Debug, PartialEq)]
pub enum Ending {
    /// Everything it watched has closed (status 4).
    Gone,
    /// The server stopped, for this reason: a stream that is done.
    Stopped(String),
}

impl Ending {
    /// Its line, the stream's last, which is of no pane.
    pub fn line(&self) -> String {
        let (name, fields) = match self {
            Self::Gone => ("gone", Vec::new()),
            Self::Stopped(reason) => ("stopped", vec![("reason", Json::str(reason.clone()))]),
        };
        line(name, (None, None, None), fields)
    }

    /// The stream's reply, as it ends.
    pub fn reply(&self) -> Reply {
        match self {
            Self::Gone => Reply::failed(Code::Gone, "everything it watched is gone"),
            Self::Stopped(_) => Reply::default(),
        }
    }
}

/// The line that says `count` entries of the journal were let go before
/// the stream was told them: of its target or not, which is not known.
fn missed(count: u64) -> String {
    line(
        "missed",
        (None, None, None),
        vec![("count", Json::uint(count))],
    )
}

/// The JSON line of an event named `name` at the pane, tab and workspace
/// given (none for the stream's own), with the rest of its `fields`.
fn line(
    name: &str,
    (pane, tab, ws): (Option<PaneId>, Option<TabId>, Option<WsId>),
    fields: Vec<(&'static str, Json)>,
) -> String {
    let at = [
        ("pane", pane.map(|p| p.to_string())),
        ("tab", tab.map(|t| t.to_string())),
        ("workspace", ws.map(|w| w.to_string())),
    ];
    let mut all = vec![("event", Json::str(name))];
    all.extend(at.into_iter().filter_map(|(k, v)| Some((k, Json::str(v?)))));
    all.extend(fields);
    format!("{}\n", Json::object(all).render())
}

/// A connection that streams events: which pane, tab or workspace it is
/// told of (all without one), and where in the journal it reads next.
pub struct Subscription {
    target: Option<AnyRef>,
    next: Cursor,
}

impl Subscription {
    /// One told of what happens in `journal`, of server run `run`, from
    /// `from` on, else from now. Refused from a cursor of another run, or
    /// past the journal's end, which this run never gave; one the journal
    /// let go of is told as missed when read.
    pub fn new(
        target: Option<AnyRef>,
        journal: &Journal,
        run: Run,
        from: Option<Cursor>,
    ) -> Result<Self, String> {
        let end = journal.end(run);
        let next = from.unwrap_or(end);
        if next.run != run {
            return Err(format!(
                "{next} is from another server run ({}), not this one ({run}): a server since stopped, or another socket's",
                next.run
            ));
        }
        if next.seq > end.seq {
            let why = format!("{next} is past the journal's end ({end}): this run never gave it");
            return Err(why);
        }
        Ok(Self { target, next })
    }

    /// Its first line, once it is armed: from here on, nothing is missed
    /// unsaid; `from`, where in the journal it reads from.
    pub fn ready(&self) -> String {
        let from = ("from", Json::str(self.next.to_string()));
        line("ready", (None, None, None), vec![from])
    }

    /// Whether it is told of `event`: where its target is or holds, or a
    /// pane's move from there.
    fn wants(&self, event: &Event) -> bool {
        let (pane, tab, ws) = event.ids();
        let from = match event {
            Event::Pane(_, PaneEvent::Moved(from)) => Some(*from),
            Event::Pane(..) | Event::Tab(..) | Event::Workspace(..) => None,
        };
        self.target.is_none_or(|target| match target {
            AnyRef::Pane(p) => pane == Some(p),
            AnyRef::Tab(t) => tab == Some(t) || from.is_some_and(|(_, f)| f == t),
            AnyRef::Workspace(w) => ws == Some(w) || from.is_some_and(|(f, _)| f == w),
        })
    }

    /// Whether `event` ends what it can be told of its target: the target
    /// closed.
    fn ends(&self, event: &Event) -> bool {
        self.target.is_some() && event.closed() == self.target
    }

    /// The lines it is told of what the journal holds that it has not
    /// read, until they are `room` bytes or more (the rest waits for its
    /// next turn); and its end, once everything it watched is gone, which
    /// `holder` (the workspace that holds a target, if any) tells for what
    /// the journal no longer holds the close of. With no room it is told
    /// nothing, not even what it missed, which adds up until it has room.
    pub fn told(
        &mut self,
        journal: &Journal,
        room: usize,
        holder: impl Fn(AnyRef) -> Option<WsId>,
    ) -> (String, Option<Ending>) {
        let mut lines = String::new();
        if room == 0 {
            return (lines, None);
        }
        let (lost, entries) = journal.from(self.next.seq);
        if lost > 0 {
            lines.push_str(&missed(lost));
            self.next.seq = Seq(self.next.seq.0.saturating_add(lost));
        }
        for (seq, event) in entries {
            if lines.len() >= room {
                return (lines, None);
            }
            self.next.seq = Seq(seq.0.saturating_add(1));
            if self.wants(event) {
                lines.push_str(&event.line());
            }
            if self.ends(event) {
                return (lines, Some(Ending::Gone));
            }
        }
        let gone = (self.target).is_some_and(|target| holder(target).is_none());
        (lines, gone.then_some(Ending::Gone))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::Output;
    use crate::process::Ended;

    fn run(text: &str) -> Result<Run, String> {
        text.parse().map_err(|()| format!("{text} is no run"))
    }

    /// A reader with no room left reads on from where it stopped; and one
    /// the journal let entries go past is told how many it missed, once.
    #[test]
    fn a_reader_reads_on_and_is_told_what_it_missed() -> Result<(), String> {
        let ws: WsId = "+1".parse().map_err(|_| "+1")?;
        let tab: TabId = "@1".parse().map_err(|_| "@1")?;
        let bell = Event::Pane(((ws, tab), PaneId::of(1)), PaneEvent::Told(Told::Bell));
        let mut journal = Journal::default();
        let mut stream = Subscription::new(None, &journal, run("1-0")?, None)?;
        journal.push(bell.clone());
        journal.push(Event::Workspace(ws, Change::Closed));
        assert_eq!(
            stream.told(&journal, 1, |_| Some(ws)).0,
            concat!(
                r#"{"event":"bell","pane":"%1","tab":"@1","workspace":"+1"}"#,
                "\n"
            )
        );
        assert_eq!(
            stream.told(&journal, 1, |_| Some(ws)).0,
            concat!(r#"{"event":"closed","workspace":"+1"}"#, "\n")
        );
        for _ in 0..=KEPT {
            journal.push(bell.clone());
        }
        // A reader with no room is told nothing, and what it missed adds
        // up, told once when it has room.
        assert_eq!(stream.told(&journal, 0, |_| Some(ws)).0, "");
        journal.push(bell);
        assert_eq!(stream.told(&journal, 0, |_| Some(ws)).0, "");
        let (lines, _) = stream.told(&journal, usize::MAX, |_| Some(ws));
        assert!(
            lines.starts_with(concat!(
                r#"{"event":"missed","count":2}"#,
                "\n",
                r#"{"event":"bell","pane":"%1","tab":"@1","workspace":"+1"}"#
            )),
            "{lines}"
        );
        assert_eq!(lines.lines().count(), KEPT.saturating_add(1));
        assert_eq!(stream.told(&journal, usize::MAX, |_| Some(ws)).0, "");
        Ok(())
    }

    /// A stream from a cursor is told everything after it, or what it
    /// missed if the journal let that go; a cursor of another run, or past
    /// the end, is refused; and a cursor has one spelling.
    #[test]
    fn a_stream_from_a_cursor_misses_nothing_after_it() -> Result<(), String> {
        let ws: WsId = "+1".parse().map_err(|_| "+1")?;
        let mut journal = Journal::default();
        let ours = run("7-1f")?;
        let cursor = journal.end(ours);
        assert_eq!(cursor.to_string(), "0:7-1f");
        assert_eq!("0:7-1f".parse(), Ok(cursor));
        for bad in ["00:7-1f", "+0:7-1f", "0:7-1F", "0", "0:7-1f:1"] {
            assert_eq!(bad.parse::<Cursor>(), Err(()), "{bad}");
        }
        journal.push(Event::Workspace(ws, Change::Renamed("x".into())));
        let from = |text: &str| -> Result<_, String> {
            let cursor = text.parse().map_err(|()| "a cursor")?;
            Ok(Subscription::new(None, &journal, ours, Some(cursor)))
        };
        let mut stream = from("0:7-1f")??;
        assert_eq!(
            stream.told(&journal, usize::MAX, |_| Some(ws)).0,
            concat!(r#"{"event":"renamed","workspace":"+1","name":"x"}"#, "\n")
        );
        let past = from("2:7-1f")?.err().unwrap_or_default();
        assert!(past.contains("past the journal's end (1:7-1f)"), "{past}");
        let theirs = from("0:7-20")?.err().unwrap_or_default();
        assert!(theirs.contains("another server run"), "{theirs}");
        // One the journal let go of is told as missed.
        for _ in 0..KEPT {
            journal.push(Event::Workspace(ws, Change::Closed));
        }
        let mut stream = Subscription::new(None, &journal, ours, Some(cursor))?;
        let (lines, _) = stream.told(&journal, usize::MAX, |_| Some(ws));
        assert!(
            lines.starts_with(r#"{"event":"missed","count":1}"#),
            "{lines}"
        );
        Ok(())
    }

    /// Every event's name, a pane's, a tab's or workspace's and the
    /// stream's own, is in docs/json.md's table of events, and the table
    /// names no other; and the docs say how many entries the journal
    /// keeps as it does.
    #[test]
    fn every_event_is_in_the_docs() -> Result<(), String> {
        let ws: WsId = "+1".parse().map_err(|_| "+1")?;
        let tab: TabId = "@1".parse().map_err(|_| "@1")?;
        let at = ((ws, tab), PaneId::of(1));
        let size = fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?;
        let cut = Left {
            output: Output::Cut,
            input: 2,
        };
        let events = [
            Event::Pane(at, PaneEvent::Created),
            Event::Pane(at, PaneEvent::Told(Told::Renamed(String::new()))),
            Event::Pane(at, PaneEvent::Closed),
            Event::Pane(at, PaneEvent::Moved((ws, tab))),
            Event::Pane(at, PaneEvent::Told(Told::Respawned(cut))),
            Event::Pane(at, PaneEvent::Told(Told::Bell)),
            Event::Pane(at, PaneEvent::Told(Told::Notification(Said::default()))),
            Event::Pane(at, PaneEvent::Told(Told::Title(Said::default()))),
            Event::Pane(at, PaneEvent::Told(Told::Done(None))),
            Event::Pane(at, PaneEvent::Told(Told::Resized(size))),
            Event::Pane(at, PaneEvent::Told(Told::HistoryBudget(0))),
            Event::Pane(
                at,
                PaneEvent::Told(Told::Exited(End {
                    ended: Ended::Exited(0),
                    left: Left::default(),
                })),
            ),
            Event::Pane(
                at,
                PaneEvent::Told(Told::Exited(End {
                    ended: Ended::Signalled(9),
                    left: cut,
                })),
            ),
            Event::Tab((ws, tab), Change::Created(None)),
            Event::Tab((ws, tab), Change::Renamed(String::new())),
            Event::Tab((ws, tab), Change::Closed),
            Event::Workspace(ws, Change::Created(None)),
            Event::Workspace(ws, Change::Renamed(String::new())),
            Event::Workspace(ws, Change::Closed),
        ];
        let mut lines = Vec::new();
        for event in events {
            // A new kind of event does not compile here until it has an
            // arm; whoever adds the arm adds its value to the list above.
            match event {
                Event::Pane(
                    _,
                    PaneEvent::Created
                    | PaneEvent::Closed
                    | PaneEvent::Moved(..)
                    | PaneEvent::Told(
                        Told::Renamed(_)
                        | Told::Respawned(_)
                        | Told::Bell
                        | Told::Notification(_)
                        | Told::Title(_)
                        | Told::Done(_)
                        | Told::Resized(_)
                        | Told::HistoryBudget(_)
                        | Told::Exited(_),
                    ),
                )
                | Event::Tab(_, Change::Created(_) | Change::Renamed(_) | Change::Closed)
                | Event::Workspace(_, Change::Created(_) | Change::Renamed(_) | Change::Closed) => {
                    lines.push(event.line())
                }
            }
        }
        for ending in [Ending::Gone, Ending::Stopped(String::new())] {
            match ending {
                Ending::Gone | Ending::Stopped(_) => lines.push(ending.line()),
            }
        }
        let ready = Subscription::new(None, &Journal::default(), run("1-0")?, None)?.ready();
        lines.extend([ready, missed(1)]);
        // Each begins `{"event":"NAME"`.
        let names: std::collections::BTreeSet<&str> = (lines.iter())
            .filter_map(|line| line.split('"').nth(3))
            .collect();
        let docs = include_str!("../docs/json.md");
        let table = (docs.split("## `events`").nth(1))
            .and_then(|rest| rest.split("\n## ").next())
            .and_then(|section| section.split("| --- | --- |").nth(1))
            .ok_or("docs/json.md has no events table")?;
        let documented: std::collections::BTreeSet<&str> = (table.lines())
            .filter_map(|row| row.strip_prefix("| `")?.split(" |").next())
            .flat_map(|cell| cell.split(", "))
            .map(|name| name.trim_matches('`'))
            .collect();
        assert_eq!(names, documented);
        // The bound it states is the one the journal keeps.
        let kept = format!("last {},{:03} entries", KEPT / 1000, KEPT % 1000);
        assert!(docs.contains(&kept), "{kept}");
        Ok(())
    }
}

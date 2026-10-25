//! `fux events`: what happens in the session, a line each as it happens,
//! for a script that would otherwise poll `fux ls json`. Each change to the
//! session's workspaces, tabs and panes, and what each pane's program does
//! that events tell (a bell, a notification, a title, a shell command's
//! end, a resize, a respawn, an exit), is written in one journal where it
//! happens, in the order it happens (`workspace::Parts`, and a pane's own
//! methods through a [`Teller`]); each subscriber reads the journal from
//! its own place in it, after each round of what the server read and did.
use crate::code::Code;
use crate::command::AnyRef;
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
const KEPT: usize = 1 << 14;

/// A number of the journal's: the order things happened in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Seq(u64);

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

    /// Where a reader starting now reads from: nothing recorded yet.
    pub fn end(&self) -> Seq {
        self.next
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
/// events tell write to, at once. Only the session's parts, which hold
/// the journal, give one (`workspace::PaneMut`).
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
    Created(String),
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
    /// Its program ended.
    Exited(End),
}

/// An event's fields after its name and place.
type Fields = Vec<(&'static str, Json<'static>)>;

/// A name, as the field `name`.
fn named(name: &str) -> Fields {
    vec![("name", Json::str(name.to_owned()))]
}

impl Change {
    /// Its name, and what else it says.
    fn said(&self) -> (&'static str, Fields) {
        match self {
            Change::Created(name) => ("created", named(name)),
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

    /// The line that tells it: JSON, or its name, where, and `key=value`
    /// for the rest.
    pub fn line(&self, json: bool) -> String {
        let (name, fields) = match self {
            Event::Pane(_, what) => what.said(),
            Event::Tab(_, change) | Event::Workspace(_, change) => change.said(),
        };
        line(name, self.ids(), fields, json)
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
    pub fn line(&self, json: bool) -> String {
        let (name, fields) = match self {
            Self::Gone => ("gone", Vec::new()),
            Self::Stopped(reason) => ("stopped", vec![("reason", Json::str(reason.clone()))]),
        };
        line(name, (None, None, None), fields, json)
    }

    /// The stream's reply, as it ends.
    pub fn reply(&self) -> Reply {
        match self {
            Self::Gone => Reply::failed(Code::Gone, "everything it watched is gone"),
            Self::Stopped(_) => Reply::default(),
        }
    }
}

/// A stream's first line, once it is armed: from here on, nothing is
/// missed unsaid.
pub fn ready(json: bool) -> String {
    line("ready", (None, None, None), Vec::new(), json)
}

/// The line that says `count` entries of the journal were let go before
/// the stream was told them: of its target or not, which is not known.
fn missed(count: u64, json: bool) -> String {
    line(
        "missed",
        (None, None, None),
        vec![("count", Json::int(count))],
        json,
    )
}

/// The line of an event named `name` at the pane, tab and workspace given
/// (none for the stream's own): JSON, or its name, the ids and
/// `key=value` for the rest of `fields`.
fn line(
    name: &str,
    (pane, tab, ws): (Option<PaneId>, Option<TabId>, Option<WsId>),
    fields: Vec<(&'static str, Json<'static>)>,
    json: bool,
) -> String {
    let at = [
        ("pane", pane.map(|p| p.to_string())),
        ("tab", tab.map(|t| t.to_string())),
        ("workspace", ws.map(|w| w.to_string())),
    ];
    let at = at.into_iter().filter_map(|(k, v)| Some((k, v?)));
    if json {
        let mut all = vec![("event", Json::str(name))];
        all.extend(at.map(|(k, v)| (k, Json::str(v))));
        all.extend(fields);
        return format!("{}\n", Json::Object(all).render());
    }
    let mut line = name.to_owned();
    for (_, id) in at {
        line.push(' ');
        line.push_str(&id);
    }
    for (key, value) in fields {
        let value = match value {
            Json::String(text) => crate::words::quote(&text),
            other @ (Json::Null
            | Json::Bool(_)
            | Json::Number(_)
            | Json::Array(_)
            | Json::Object(_)
            | Json::Raw(_)) => other.render(),
        };
        line.push_str(&format!(" {key}={value}"));
    }
    line.push('\n');
    line
}

/// A connection that streams events: which pane, tab or workspace it is
/// told of (all without one), how, and where in the journal it reads next.
pub struct Subscription {
    target: Option<AnyRef>,
    pub json: bool,
    next: Seq,
}

impl Subscription {
    /// One told of what happens from now on.
    pub fn new(target: Option<AnyRef>, json: bool, journal: &Journal) -> Self {
        let next = journal.end();
        Self { target, json, next }
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
        let (lost, entries) = journal.from(self.next);
        if lost > 0 {
            lines.push_str(&missed(lost, self.json));
            self.next = Seq(self.next.0.saturating_add(lost));
        }
        for (seq, event) in entries {
            if lines.len() >= room {
                return (lines, None);
            }
            self.next = Seq(seq.0.saturating_add(1));
            if self.wants(event) {
                lines.push_str(&event.line(self.json));
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

    /// A reader with no room left reads on from where it stopped; and one
    /// the journal let entries go past is told how many it missed, once.
    #[test]
    fn a_reader_reads_on_and_is_told_what_it_missed() -> Result<(), String> {
        let ws: WsId = "+1".parse().map_err(|_| "+1")?;
        let tab: TabId = "@1".parse().map_err(|_| "@1")?;
        let bell = Event::Pane(((ws, tab), PaneId::of(1)), PaneEvent::Told(Told::Bell));
        let mut journal = Journal::default();
        let mut stream = Subscription::new(None, false, &journal);
        journal.push(bell.clone());
        journal.push(Event::Workspace(ws, Change::Closed));
        assert_eq!(stream.told(&journal, 1, |_| Some(ws)).0, "bell %1 @1 +1\n");
        assert_eq!(stream.told(&journal, 1, |_| Some(ws)).0, "closed +1\n");
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
            lines.starts_with("missed count=2\nbell %1 @1 +1\n"),
            "{lines}"
        );
        assert_eq!(lines.lines().count(), KEPT.saturating_add(1));
        assert_eq!(stream.told(&journal, usize::MAX, |_| Some(ws)).0, "");
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
            Event::Tab((ws, tab), Change::Created(String::new())),
            Event::Tab((ws, tab), Change::Renamed(String::new())),
            Event::Tab((ws, tab), Change::Closed),
            Event::Workspace(ws, Change::Created(String::new())),
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
                        | Told::Exited(_),
                    ),
                )
                | Event::Tab(_, Change::Created(_) | Change::Renamed(_) | Change::Closed)
                | Event::Workspace(_, Change::Created(_) | Change::Renamed(_) | Change::Closed) => {
                    lines.push(event.line(false))
                }
            }
        }
        for ending in [Ending::Gone, Ending::Stopped(String::new())] {
            match ending {
                Ending::Gone | Ending::Stopped(_) => lines.push(ending.line(false)),
            }
        }
        lines.extend([ready(false), missed(1, false)]);
        let names: std::collections::BTreeSet<&str> = (lines.iter())
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        let docs = include_str!("../docs/json.md");
        let table = (docs.split("## `events-json`").nth(1))
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

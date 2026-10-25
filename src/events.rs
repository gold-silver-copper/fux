//! `fux events`: what happens in the session, a line each as it happens,
//! for a script that would otherwise poll `fux ls json`. Each change to the
//! session's workspaces, tabs and panes, and what each pane's program does
//! that events tell (a bell, a notification, a title, a shell command's
//! end, a resize, a respawn, an exit), is written in one journal where it
//! happens, in the order it happens (`workspace::Parts`); each subscriber
//! reads the journal from its own place in it, after each round of what
//! the server read and did.
use crate::code::Code;
use crate::command::AnyRef;
use crate::id::{PaneId, TabId, WsId};
use crate::json::Json;
use crate::pane::{NOTIFICATION_MAX, Said, TITLE_MAX};
use crate::process::Ended;
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
    /// How many changes there have been to what a saved session holds:
    /// entries that made, closed, moved, renamed or respawned something,
    /// kept or let go, and the changes to a layout no event tells
    /// (`Journal::reshaped`).
    shapes: u64,
}

impl Journal {
    /// Records that `what` happened at `place`, numbered after the last.
    pub(crate) fn push(&mut self, place: Place, what: What) {
        if self.entries.len() >= KEPT {
            self.entries.pop_front();
        }
        let shapes = matches!(
            what,
            What::Created(_) | What::Closed | What::Moved(_) | What::Renamed(_) | What::Respawned
        );
        if shapes {
            self.reshaped();
        }
        self.entries.push_back((self.next, Event { place, what }));
        // 2^64 entries are never reached: the number never stands still.
        self.next = Seq(self.next.0.saturating_add(1));
    }

    /// Where a reader starting now reads from: nothing recorded yet.
    pub fn end(&self) -> Seq {
        self.next
    }

    /// A layout changed as no event tells (an order, a border, a size):
    /// counted with the changes events tell that a saved session holds.
    pub(crate) fn reshaped(&mut self) {
        // 2^64 changes are never reached: the count never stands still.
        self.shapes = self.shapes.saturating_add(1);
    }

    /// How many changes there have been to what a saved session holds.
    pub fn shapes(&self) -> u64 {
        self.shapes
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

/// Where a thing happened, for `events-of` to choose by: a pane in its tab
/// and workspace, a tab in its workspace, or a workspace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place {
    Pane((WsId, TabId), PaneId),
    Tab(WsId, TabId),
    Workspace(WsId),
}

impl Place {
    /// Its pane, if it is one, its tab, if it is one or in one, and its
    /// workspace.
    fn ids(self) -> (Option<PaneId>, Option<TabId>, Option<WsId>) {
        match self {
            Place::Pane((ws, tab), pane) => (Some(pane), Some(tab), Some(ws)),
            Place::Tab(ws, tab) => (None, Some(tab), Some(ws)),
            Place::Workspace(ws) => (None, None, Some(ws)),
        }
    }

    /// What it is the place of.
    fn thing(self) -> AnyRef {
        match self {
            Place::Pane(_, pane) => AnyRef::Pane(pane),
            Place::Tab(_, tab) => AnyRef::Tab(tab),
            Place::Workspace(ws) => AnyRef::Workspace(ws),
        }
    }
}

/// One thing that happened in the session, and where.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    place: Place,
    what: What,
}

/// What happened: to a pane, tab or workspace, or (from `Moved` on) to a
/// pane. The type does not tie it to its `Place`: `Parts`, which records
/// every event, gives the pane-only ones a pane's place.
#[derive(Clone, Debug, PartialEq)]
pub enum What {
    /// Made; a tab or workspace with its name.
    Created(Option<String>),
    /// Named otherwise: its new name.
    Renamed(String),
    Closed,
    /// To another tab, from this workspace and tab.
    Moved((WsId, TabId)),
    /// Its program started again (`respawn`).
    Respawned,
    Bell,
    Notification(Said<NOTIFICATION_MAX>),
    Title(Said<TITLE_MAX>),
    /// A shell command ended, with its status if it is known.
    Done(Option<i32>),
    Resized(fux_vt::Size),
    /// Its program ended: with a status or by a signal.
    Exited(Ended),
}

impl What {
    /// Its name, and what else it says.
    fn said(&self) -> (&'static str, Vec<(&'static str, Json<'static>)>) {
        let text = |key, text: &String| vec![(key, Json::str(text.clone()))];
        // A program's text, and `truncated` if it sent more.
        let said = |key, text: &str, cut: bool| {
            let cut = cut.then_some(("truncated", Json::Bool(true)));
            [(key, Json::str(text.to_owned()))]
                .into_iter()
                .chain(cut)
                .collect()
        };
        match self {
            What::Created(name) => (
                "created",
                name.iter().flat_map(|n| text("name", n)).collect(),
            ),
            What::Renamed(name) => ("renamed", text("name", name)),
            What::Closed => ("closed", Vec::new()),
            What::Moved((ws, tab)) => (
                "moved",
                vec![
                    ("from_tab", Json::str(tab.to_string())),
                    ("from_workspace", Json::str(ws.to_string())),
                ],
            ),
            What::Respawned => ("respawned", Vec::new()),
            What::Bell => ("bell", Vec::new()),
            What::Notification(n) => ("notification", said("text", n.as_str(), n.cut())),
            What::Title(title) => ("title", said("title", title.as_str(), title.cut())),
            What::Done(status) => (
                "done",
                vec![("status", status.map_or(Json::Null, Json::int))],
            ),
            What::Resized(size) => {
                let cols = ("cols", Json::int(size.cols()));
                ("resized", vec![cols, ("rows", Json::int(size.rows()))])
            }
            What::Exited(Ended::Exited(code)) => ("exited", vec![("status", Json::int(*code))]),
            What::Exited(Ended::Signalled(signal)) => {
                ("exited", vec![("signal", Json::int(*signal))])
            }
            What::Exited(Ended::Lost) => ("exited", vec![("status", Json::Null)]),
        }
    }
}

impl Event {
    /// The line that tells it: JSON, or its name, where, and `key=value`
    /// for the rest; `within` a workspace (a grant's), no tab or workspace
    /// outside it, as a pane moved in from one, or out to one.
    pub fn line(&self, json: bool, within: Option<WsId>) -> String {
        let outside = |ws: WsId| within.is_some_and(|w| w != ws);
        let (name, mut fields) = self.what.said();
        if let What::Moved((from, _)) = self.what
            && outside(from)
        {
            fields.clear();
        }
        let (pane, tab, ws) = self.place.ids();
        let at = match ws.is_some_and(outside) {
            true => (pane, None, None),
            false => (pane, tab, ws),
        };
        line(name, at, fields, json)
    }
}

/// How a stream of events ends: its last line, and its reply.
#[derive(Clone, Debug, PartialEq)]
pub enum Ending {
    /// Everything it watched has closed, or left the granted workspace
    /// (status 4).
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
/// told of (all without one), how, the workspace whose ids alone it is
/// told (a grant's), and where in the journal it reads next.
pub struct Subscription {
    target: Option<AnyRef>,
    pub json: bool,
    within: Option<WsId>,
    next: Seq,
}

impl Subscription {
    /// One told of what happens from now on.
    pub fn new(
        target: Option<AnyRef>,
        json: bool,
        within: Option<WsId>,
        journal: &Journal,
    ) -> Self {
        let next = journal.end();
        Self {
            target,
            json,
            within,
            next,
        }
    }

    /// Whether it is told of `event`: where its target is or holds, or a
    /// pane's move from there; `within` a workspace, only what is there,
    /// or a pane's move from there.
    fn wants(&self, event: &Event) -> bool {
        let (pane, tab, ws) = event.place.ids();
        let from = if let What::Moved(from) = event.what {
            Some(from)
        } else {
            None
        };
        let seen = self
            .within
            .is_none_or(|w| ws == Some(w) || from.is_some_and(|(f, _)| f == w));
        seen && self.target.is_none_or(|target| match target {
            AnyRef::Pane(p) => pane == Some(p),
            AnyRef::Tab(t) => tab == Some(t) || from.is_some_and(|(_, f)| f == t),
            AnyRef::Workspace(w) => ws == Some(w) || from.is_some_and(|(f, _)| f == w),
        })
    }

    /// Whether `event` ends what it can be told of its target: the target
    /// closed, or (a pane) moved out of the workspace it is `within`.
    fn ends(&self, event: &Event) -> bool {
        let (_, _, ws) = event.place.ids();
        let left = matches!(event.what, What::Moved(_)) && !self.reaches(ws);
        (event.what == What::Closed || left) && self.target == Some(event.place.thing())
    }

    /// Whether what is in workspace `ws` (none: gone) is there for it.
    fn reaches(&self, ws: Option<WsId>) -> bool {
        ws.is_some_and(|ws| self.within.is_none_or(|w| w == ws))
    }

    /// The lines it is told of what the journal holds that it has not
    /// read, until they are `room` bytes or more (the rest waits for its
    /// next turn); and its end, once everything it watched is gone or out
    /// of its reach, which `holder` (the workspace that holds a target,
    /// if any) tells for what the journal no longer holds the close or
    /// move of. With no room it is told nothing, not even what it missed,
    /// which adds up until it has room.
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
                lines.push_str(&event.line(self.json, self.within));
            }
            if self.ends(event) {
                return (lines, Some(Ending::Gone));
            }
        }
        let gone = self
            .target
            .is_some_and(|target| !self.reaches(holder(target)));
        (lines, gone.then_some(Ending::Gone))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader with no room left reads on from where it stopped; and one
    /// the journal let entries go past is told how many it missed, once.
    #[test]
    fn a_reader_reads_on_and_is_told_what_it_missed() -> Result<(), String> {
        let ws: WsId = "+1".parse().map_err(|_| "+1")?;
        let mut journal = Journal::default();
        let mut stream = Subscription::new(None, false, None, &journal);
        journal.push(Place::Workspace(ws), What::Bell);
        journal.push(Place::Workspace(ws), What::Closed);
        assert_eq!(stream.told(&journal, 1, |_| Some(ws)).0, "bell +1\n");
        assert_eq!(stream.told(&journal, 1, |_| Some(ws)).0, "closed +1\n");
        for _ in 0..=KEPT {
            journal.push(Place::Workspace(ws), What::Bell);
        }
        // A reader with no room is told nothing, and what it missed adds
        // up, told once when it has room.
        assert_eq!(stream.told(&journal, 0, |_| Some(ws)).0, "");
        journal.push(Place::Workspace(ws), What::Bell);
        assert_eq!(stream.told(&journal, 0, |_| Some(ws)).0, "");
        let (lines, _) = stream.told(&journal, usize::MAX, |_| Some(ws));
        assert!(lines.starts_with("missed count=2\nbell +1\n"), "{lines}");
        assert_eq!(lines.lines().count(), KEPT.saturating_add(1));
        assert_eq!(stream.told(&journal, usize::MAX, |_| Some(ws)).0, "");
        Ok(())
    }

    /// A stream within a workspace (a grant's) is told no tab or
    /// workspace outside it: of a pane moved in from one or out to one,
    /// only the pane and the side it sees.
    #[test]
    fn a_stream_within_a_workspace_is_told_no_ids_outside_it() -> Result<(), String> {
        let id = |text: &str| text.parse::<WsId>().map_err(|_| text.to_owned());
        let tab = |text: &str| text.parse::<TabId>().map_err(|_| text.to_owned());
        let (inside, outside) = ((id("+1")?, tab("@1")?), (id("+2")?, tab("@2")?));
        let mut journal = Journal::default();
        let target = Some(AnyRef::Workspace(inside.0));
        let mut stream = Subscription::new(target, false, Some(inside.0), &journal);
        let pane = PaneId::of(3);
        journal.push(Place::Pane(inside, pane), What::Moved(outside));
        journal.push(Place::Pane(outside, pane), What::Moved(inside));
        let told = stream.told(&journal, usize::MAX, |_| Some(inside.0)).0;
        assert_eq!(
            told,
            "moved %3 @1 +1\nmoved %3 from_tab=@1 from_workspace=+1\n"
        );
        Ok(())
    }

    /// A stream of a pane within a workspace (a grant's) is told nothing
    /// of it once it is moved out, but the move, and ends there; or, if
    /// the journal let the move go, once it reads on and finds it out.
    #[test]
    fn a_stream_of_a_pane_within_a_workspace_ends_as_it_leaves() -> Result<(), String> {
        let id = |text: &str| text.parse::<WsId>().map_err(|_| text.to_owned());
        let tab = |text: &str| text.parse::<TabId>().map_err(|_| text.to_owned());
        let (inside, outside) = ((id("+1")?, tab("@1")?), (id("+2")?, tab("@2")?));
        let pane = PaneId::of(3);
        let (target, within) = (Some(AnyRef::Pane(pane)), Some(inside.0));
        let mut journal = Journal::default();
        let mut stream = Subscription::new(target, false, within, &journal);
        journal.push(Place::Pane(inside, pane), What::Bell);
        journal.push(Place::Pane(outside, pane), What::Moved(inside));
        journal.push(Place::Pane(outside, pane), What::Bell);
        let told = stream.told(&journal, usize::MAX, |_| Some(outside.0));
        let lines = "bell %3 @1 +1\nmoved %3 from_tab=@1 from_workspace=+1\n";
        assert_eq!(told, (lines.to_owned(), Some(Ending::Gone)));
        // The move let go: what it reads of the pane is outside, and it
        // ends once it finds the pane there.
        let mut stream = Subscription::new(target, false, within, &journal);
        for _ in 0..=KEPT {
            journal.push(Place::Pane(outside, pane), What::Bell);
        }
        let (lines, end) = stream.told(&journal, usize::MAX, |_| Some(outside.0));
        assert_eq!(
            (lines.as_str(), end),
            ("missed count=1\n", Some(Ending::Gone))
        );
        Ok(())
    }

    /// What changed a saved session is known past what the journal keeps:
    /// a flood of bells after it is not taken for a change.
    #[test]
    fn what_changed_a_saved_session_is_known_past_what_is_kept() -> Result<(), String> {
        let ws: WsId = "+1".parse().map_err(|_| "+1")?;
        let mut journal = Journal::default();
        journal.push(Place::Workspace(ws), What::Closed);
        let after = journal.shapes();
        for _ in 0..=KEPT {
            journal.push(Place::Workspace(ws), What::Bell);
        }
        assert_eq!((journal.shapes(), after), (1, 1));
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
        let place = Place::Pane((ws, tab), PaneId::of(1));
        let size = fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?;
        let what = [
            What::Created(None),
            What::Renamed(String::new()),
            What::Closed,
            What::Moved((ws, tab)),
            What::Respawned,
            What::Bell,
            What::Notification(Said::default()),
            What::Title(Said::default()),
            What::Done(None),
            What::Resized(size),
            What::Exited(Ended::Exited(0)),
            What::Exited(Ended::Signalled(9)),
        ];
        let mut lines = Vec::new();
        for what in what {
            // A new kind of event does not compile here until it has an
            // arm; whoever adds the arm adds its value to the list above.
            match what {
                What::Created(_)
                | What::Renamed(_)
                | What::Closed
                | What::Moved(..)
                | What::Respawned
                | What::Bell
                | What::Notification(_)
                | What::Title(_)
                | What::Done(_)
                | What::Resized(_)
                | What::Exited(_) => lines.push(Event { place, what }.line(false, None)),
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

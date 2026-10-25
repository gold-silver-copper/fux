//! `fux events`: what happens in the session, a line each as it happens,
//! for a script that would otherwise poll `fux ls json`. After each
//! round of what the server read and did, each subscriber is told what
//! happened since it last looked: what each pane's journal recorded, in
//! order (a bell, a notification, a title, a shell command's end, a
//! resize); and, from the session's layout, panes, tabs and workspaces
//! made, closed, moved or renamed, a program's restart and exit.
use crate::code::Code;
use crate::command::AnyRef;
use crate::id::{PaneId, TabId, WsId};
use crate::json::Json;
use crate::log::{Event as Recorded, Seq};
use crate::pane::{NOTIFICATION_MAX, Said, TITLE_MAX};
use crate::process::Ended;
use crate::protocol::Reply;
use crate::session::Session;
use std::collections::BTreeMap;

/// The most a subscriber's output may hold unsent before it is told it lost
/// events, and its stream ends: a reader too slow to keep up.
pub const BACKLOG: usize = 1 << 20;

/// What the session holds, as far as events tell of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    workspaces: BTreeMap<WsId, String>,
    tabs: BTreeMap<TabId, (WsId, String)>,
    panes: BTreeMap<PaneId, Seen>,
}

/// Where a pane is, how often its program started, how it ended, and the
/// last entry of its journal: what events tell of it.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    ws: WsId,
    tab: TabId,
    ended: Option<Ended>,
    respawns: u64,
    last: Seq,
}

impl Snapshot {
    pub fn of(session: &Session) -> Snapshot {
        let mut snapshot = Snapshot::default();
        for ws in &session.workspaces {
            snapshot.workspaces.insert(ws.id, ws.name.clone());
            for tab in ws.tabs() {
                snapshot.tabs.insert(tab.id, (ws.id, tab.name.clone()));
                let panes = tab.root().map(crate::layout::Tree::panes);
                for id in panes.unwrap_or_default() {
                    let Some(pane) = session.panes.get(&id) else {
                        continue;
                    };
                    snapshot.panes.insert(
                        id,
                        Seen {
                            ws: ws.id,
                            tab: tab.id,
                            ended: pane.process.exit_status(),
                            respawns: pane.respawns,
                            last: pane.record.last(),
                        },
                    );
                }
            }
        }
        snapshot
    }
}

/// Where a thing happened, for `events-of` to choose by: a pane in its tab
/// and workspace, a tab in its workspace, or a workspace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place {
    Pane(WsId, TabId, PaneId),
    Tab(WsId, TabId),
    Workspace(WsId),
}

impl Place {
    /// Its pane, if it is one, its tab, if it is one or in one, and its
    /// workspace.
    fn ids(self) -> (Option<PaneId>, Option<TabId>, Option<WsId>) {
        match self {
            Place::Pane(ws, tab, pane) => (Some(pane), Some(tab), Some(ws)),
            Place::Tab(ws, tab) => (None, Some(tab), Some(ws)),
            Place::Workspace(ws) => (None, None, Some(ws)),
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
/// pane. The type does not tie it to its `Place`: `between`, which makes
/// every event, gives the pane-only ones a pane's place.
#[derive(Clone, Debug, PartialEq)]
pub enum What {
    /// Made; a tab or workspace with its name.
    Created(Option<String>),
    /// A tab or workspace named otherwise, its new name.
    Renamed(String),
    Closed,
    /// To another tab or workspace.
    Moved,
    /// Its program started again (`respawn`).
    Respawned,
    /// Entries of its journal were let go before they were told.
    Missed,
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
            What::Moved => ("moved", Vec::new()),
            What::Respawned => ("respawned", Vec::new()),
            What::Missed => ("missed", Vec::new()),
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
        }
    }
}

impl Event {
    /// The line that tells it: JSON, or its name, where, and `key=value`
    /// for the rest.
    pub fn line(&self, json: bool) -> String {
        let (name, fields) = self.what.said();
        line(name, Some(self.place), fields, json)
    }
}

/// How a stream of events ends: its last line, and its reply.
#[derive(Clone, Debug, PartialEq)]
pub enum Ending {
    /// Everything it watched has closed (status 4).
    Gone,
    /// Its reader fell too far behind.
    Lost,
    /// The server stopped, for this reason: a stream that is done.
    Stopped(String),
}

impl Ending {
    /// Its line, the stream's last, which is of no pane.
    pub fn line(&self, json: bool) -> String {
        let (name, fields) = match self {
            Self::Gone => ("gone", Vec::new()),
            Self::Lost => ("lost", Vec::new()),
            Self::Stopped(reason) => ("stopped", vec![("reason", Json::str(reason.clone()))]),
        };
        line(name, None, fields, json)
    }

    /// The stream's reply, as it ends.
    pub fn reply(&self) -> Reply {
        match self {
            Self::Gone => Reply::failed(Code::Gone, "everything it watched has closed"),
            Self::Lost => Reply::failed(Code::Lost, "events: the reader fell too far behind"),
            Self::Stopped(_) => Reply::default(),
        }
    }
}

/// A stream's first line, once it is armed: from here on, nothing is
/// missed.
pub fn ready(json: bool) -> String {
    line("ready", None, Vec::new(), json)
}

/// The line of an event named `name` at `place` (none for the stream's
/// own): JSON, or its name, the ids and `key=value` for the rest of
/// `fields`.
fn line(
    name: &str,
    place: Option<Place>,
    fields: Vec<(&'static str, Json<'static>)>,
    json: bool,
) -> String {
    let (pane, tab, ws) = place.map_or((None, None, None), Place::ids);
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

/// What changed from `old` to `new` in `session`, in order: what was made,
/// what happened to each pane (what its journal recorded since, in order),
/// then what closed.
pub fn between(old: &Snapshot, new: &Snapshot, session: &Session) -> Vec<Event> {
    let mut events = Vec::new();
    // A workspace or tab made since, or named otherwise.
    let named = |place, was: Option<&String>, name: &String| {
        let what = match was {
            None => What::Created(Some(name.clone())),
            Some(was) if was != name => What::Renamed(name.clone()),
            Some(_) => return None,
        };
        Some(Event { place, what })
    };
    for (&ws, name) in &new.workspaces {
        events.extend(named(Place::Workspace(ws), old.workspaces.get(&ws), name));
    }
    for (&tab, (ws, name)) in &new.tabs {
        let was = old.tabs.get(&tab).map(|(_, was)| was);
        events.extend(named(Place::Tab(*ws, tab), was, name));
    }
    for (&pane, now) in &new.panes {
        let place = Place::Pane(now.ws, now.tab, pane);
        let at = |what| Event { place, what };
        // A pane new since, or its program started again: its journal from
        // its start.
        let mut from = Seq(0);
        match old.panes.get(&pane) {
            None => events.push(at(What::Created(None))),
            Some(was) => {
                if (was.ws, was.tab) != (now.ws, now.tab) {
                    events.push(at(What::Moved));
                }
                match now.respawns == was.respawns {
                    true => from = was.last,
                    false => events.push(at(What::Respawned)),
                }
            }
        }
        let pane_now = session.panes.get(&pane);
        // Entries it had not told were let go (past what the journal
        // keeps): said so.
        if pane_now.is_some_and(|p| p.record.lost_after(from)) {
            events.push(at(What::Missed));
        }
        let journal = pane_now.map(|p| p.record.after(from));
        for entry in journal.into_iter().flatten() {
            events.push(at(match &entry.event {
                Recorded::Bell => What::Bell,
                Recorded::Notification(text) => What::Notification(text.clone()),
                Recorded::Title(title) => What::Title(title.clone()),
                Recorded::Done(status) => What::Done(*status),
                Recorded::Resize(size) => What::Resized(*size),
            }));
        }
        let was = old.panes.get(&pane).and_then(|w| w.ended);
        if let (Some(ended), true) = (now.ended, now.ended != was) {
            events.push(at(What::Exited(ended)));
        }
    }
    let panes = (old.panes.iter()).filter(|(pane, _)| !new.panes.contains_key(pane));
    let tabs = (old.tabs.iter()).filter(|(tab, _)| !new.tabs.contains_key(tab));
    let workspaces = (old.workspaces.keys()).filter(|ws| !new.workspaces.contains_key(ws));
    let closed = (panes.map(|(&pane, was)| Place::Pane(was.ws, was.tab, pane)))
        .chain(tabs.map(|(&tab, (ws, _))| Place::Tab(*ws, tab)))
        .chain(workspaces.map(|&ws| Place::Workspace(ws)));
    events.extend(closed.map(|place| Event {
        place,
        what: What::Closed,
    }));
    events
}

/// A connection that streams events: which pane, tab or workspace it is
/// told of (all without one), how, and what it has seen.
pub struct Subscription {
    pub target: Option<AnyRef>,
    pub json: bool,
    pub seen: Snapshot,
}

impl Subscription {
    /// Whether it is told of `event`: where its target is or holds.
    pub fn wants(&self, event: &Event) -> bool {
        let (pane, tab, ws) = event.place.ids();
        self.target.is_none_or(|target| match target {
            AnyRef::Pane(p) => pane == Some(p),
            AnyRef::Tab(t) => tab == Some(t),
            AnyRef::Workspace(w) => ws == Some(w),
        })
    }

    /// What it is told of what changed since it last looked, now that the
    /// session is `now`; and its end, if everything it watched is gone.
    pub fn told(&mut self, now: &Snapshot, session: &Session) -> (String, Option<Ending>) {
        if *now == self.seen {
            return (String::new(), None);
        }
        let events = between(&self.seen, now, session);
        self.seen = now.clone();
        let lines: String = (events.iter())
            .filter(|e| self.wants(e))
            .map(|e| e.line(self.json))
            .collect();
        let over = self.target.is_some_and(|target| match target {
            AnyRef::Pane(p) => !now.panes.contains_key(&p),
            AnyRef::Tab(t) => !now.tabs.contains_key(&t),
            AnyRef::Workspace(w) => !now.workspaces.contains_key(&w),
        });
        (lines, over.then_some(Ending::Gone))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every event's name, a pane's, a tab's or workspace's and the
    /// stream's own, is in docs/json.md's table of events, and the table
    /// names no other.
    #[test]
    fn every_event_is_in_the_docs() -> Result<(), String> {
        let ws: WsId = "+1".parse().map_err(|_| "+1")?;
        let tab: TabId = "@1".parse().map_err(|_| "@1")?;
        let place = Place::Pane(ws, tab, PaneId::of(1));
        let size = fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?;
        let what = [
            What::Created(None),
            What::Renamed(String::new()),
            What::Closed,
            What::Moved,
            What::Respawned,
            What::Missed,
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
                | What::Moved
                | What::Respawned
                | What::Missed
                | What::Bell
                | What::Notification(_)
                | What::Title(_)
                | What::Done(_)
                | What::Resized(_)
                | What::Exited(_) => lines.push(Event { place, what }.line(false)),
            }
        }
        for ending in [Ending::Gone, Ending::Lost, Ending::Stopped(String::new())] {
            match ending {
                Ending::Gone | Ending::Lost | Ending::Stopped(_) => lines.push(ending.line(false)),
            }
        }
        lines.push(ready(false));
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
        Ok(())
    }
}

//! `fux events`: what happens in the session, a line each as it happens,
//! for a script that would otherwise poll `fux ls --json`. After each
//! round of what the server read and did, each subscriber is told what
//! happened since it last looked: what each pane's journal recorded, in
//! order (a bell, a notification, a title, a shell command's end, a
//! resize); and, from the session's layout, panes, tabs and workspaces
//! made, closed, moved or renamed, a program's restart and exit.
use crate::command::AnyRef;
use crate::id::{PaneId, TabId, WsId};
use crate::json::Json;
use crate::log::{Event as Recorded, Seq};
use crate::process::Ended;
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
    starts: u64,
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
                            starts: pane.starts,
                            last: pane.record.last(),
                        },
                    );
                }
            }
        }
        snapshot
    }
}

/// One thing that happened: its name, where (for `-t` to choose by), and
/// what else it says.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    name: &'static str,
    ws: Option<WsId>,
    tab: Option<TabId>,
    pane: Option<PaneId>,
    fields: Vec<(&'static str, Json<'static>)>,
}

impl Event {
    fn new(name: &'static str, ws: WsId, tab: Option<TabId>, pane: Option<PaneId>) -> Event {
        Event {
            name,
            ws: Some(ws),
            tab,
            pane,
            fields: Vec::new(),
        }
    }

    /// An event of the stream itself, of no pane: `ready`, `lost`,
    /// `stopped`.
    pub fn stream(name: &'static str, fields: Vec<(&'static str, Json<'static>)>) -> Event {
        Event {
            name,
            ws: None,
            tab: None,
            pane: None,
            fields,
        }
    }

    fn with(mut self, key: &'static str, value: Json<'static>) -> Event {
        self.fields.push((key, value));
        self
    }

    /// The line that tells it: JSON, or its name, where, and `key=value`
    /// for the rest.
    pub fn line(&self, json: bool) -> String {
        let at = [
            ("pane", self.pane.map(|p| p.to_string())),
            ("tab", self.tab.map(|t| t.to_string())),
            ("workspace", self.ws.map(|w| w.to_string())),
        ];
        if json {
            let mut fields = vec![("event", Json::str(self.name))];
            fields.extend(at.into_iter().filter_map(|(k, v)| Some((k, Json::str(v?)))));
            fields.extend(self.fields.iter().cloned());
            return format!("{}\n", Json::Object(fields).render());
        }
        let mut line = self.name.to_owned();
        for id in at.into_iter().filter_map(|(_, v)| v) {
            line.push(' ');
            line.push_str(&id);
        }
        for (key, value) in &self.fields {
            let value = match value {
                Json::String(text) => crate::words::quote(text),
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
}

/// What changed from `old` to `new` in `session`, in order: what was made,
/// what happened to each pane (what its journal recorded since, in order),
/// then what closed.
pub fn between(old: &Snapshot, new: &Snapshot, session: &Session) -> Vec<Event> {
    let mut events = Vec::new();
    // A workspace or tab made since, or named otherwise.
    let named = |ws, tab, was: Option<&String>, name: &String| {
        let what = match was {
            None => "created",
            Some(was) if was != name => "renamed",
            Some(_) => return None,
        };
        Some(Event::new(what, ws, tab, None).with("name", Json::str(name.clone())))
    };
    for (&ws, name) in &new.workspaces {
        events.extend(named(ws, None, old.workspaces.get(&ws), name));
    }
    for (&tab, (ws, name)) in &new.tabs {
        let was = old.tabs.get(&tab).map(|(_, was)| was);
        events.extend(named(*ws, Some(tab), was, name));
    }
    for (&pane, now) in &new.panes {
        let at = |name| Event::new(name, now.ws, Some(now.tab), Some(pane));
        // A pane new since, or its program started again: its journal from
        // its start.
        let mut from = Seq(0);
        match old.panes.get(&pane) {
            None => events.push(at("created")),
            Some(was) => {
                if (was.ws, was.tab) != (now.ws, now.tab) {
                    events.push(at("moved"));
                }
                match now.starts == was.starts {
                    true => from = was.last,
                    false => events.push(at("respawned")),
                }
            }
        }
        let pane_now = session.panes.get(&pane);
        // Entries it had not told were let go (journal-bytes): said so.
        if pane_now.is_some_and(|p| p.record.lost_after(from)) {
            events.push(at("missed"));
        }
        let journal = pane_now.map(|p| p.record.after(from));
        for entry in journal.into_iter().flatten() {
            events.push(match &entry.event {
                Recorded::Bell => at("bell"),
                Recorded::Notification(text) => {
                    at("notification").with("text", Json::str(text.clone()))
                }
                Recorded::Title(title) => at("title").with("title", Json::str(title.clone())),
                Recorded::Done(status) => {
                    at("done").with("status", status.map_or(Json::Null, Json::int))
                }
                Recorded::Resize(size) => (at("resized"))
                    .with("cols", Json::int(size.cols()))
                    .with("rows", Json::int(size.rows())),
                Recorded::Output { .. } | Recorded::Input { .. } | Recorded::Action => continue,
            });
        }
        let was = old.panes.get(&pane).and_then(|w| w.ended);
        if let (Some(ended), true) = (now.ended, now.ended != was) {
            events.push(match ended {
                Ended::Exited(code) => at("exited").with("status", Json::int(code)),
                Ended::Signalled(signal) => at("exited").with("signal", Json::int(signal)),
            });
        }
    }
    for (&pane, was) in &old.panes {
        if !new.panes.contains_key(&pane) {
            events.push(Event::new("closed", was.ws, Some(was.tab), Some(pane)));
        }
    }
    for (&tab, (ws, _)) in &old.tabs {
        if !new.tabs.contains_key(&tab) {
            events.push(Event::new("closed", *ws, Some(tab), None));
        }
    }
    for &ws in old.workspaces.keys() {
        if !new.workspaces.contains_key(&ws) {
            events.push(Event::new("closed", ws, None, None));
        }
    }
    events
}

/// A connection that streams events: which panes, tabs and workspaces it
/// is told of (all without any), how, and what it has seen.
pub struct Subscription {
    pub targets: Vec<AnyRef<WsId>>,
    pub json: bool,
    pub seen: Snapshot,
}

impl Subscription {
    /// Whether it is told of `event`: one of the stream itself, or where
    /// one of its targets is or holds.
    pub fn wants(&self, event: &Event) -> bool {
        event.ws.is_none()
            || self.targets.is_empty()
            || self.targets.iter().any(|target| match *target {
                AnyRef::Pane(p) => event.pane == Some(p),
                AnyRef::Tab(t) => event.tab == Some(t),
                AnyRef::Workspace(w) => event.ws == Some(w),
            })
    }

    /// What it is told of what changed since it last looked, now that the
    /// session is `now`; and whether it is over, everything it watched
    /// gone (then its last line says so).
    pub fn told(&mut self, now: &Snapshot, session: &Session) -> (String, bool) {
        if *now == self.seen {
            return (String::new(), false);
        }
        let events = between(&self.seen, now, session);
        self.seen = now.clone();
        let mut lines: String = (events.iter())
            .filter(|e| self.wants(e))
            .map(|e| e.line(self.json))
            .collect();
        let there = |target: &AnyRef<WsId>| match *target {
            AnyRef::Pane(p) => now.panes.contains_key(&p),
            AnyRef::Tab(t) => now.tabs.contains_key(&t),
            AnyRef::Workspace(w) => now.workspaces.contains_key(&w),
        };
        let over = !self.targets.is_empty() && !self.targets.iter().any(there);
        if over {
            lines.push_str(&Event::stream("gone", Vec::new()).line(self.json));
        }
        (lines, over)
    }
}

//! `fux events`: what happens in the session, a line each as it happens,
//! for a script that would otherwise poll `fux ls --json`. The server looks
//! at the session after each round of what it read and did, and tells each
//! subscriber what changed since it last looked: panes, tabs and workspaces
//! made, closed, moved or renamed; a program's exit, restart, title, size,
//! bells and notifications; a shell command's end.
use crate::command::AnyRef;
use crate::id::{PaneId, TabId, WsId};
use crate::json::Json;
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

/// What events tell of a pane.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    ws: WsId,
    tab: TabId,
    title: String,
    size: (u16, u16),
    ended: Option<Ended>,
    starts: u64,
    finished: u64,
    last_status: Option<i32>,
    bells: u64,
    notifications: u64,
    last_notification: Option<String>,
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
                    let commands = pane.screen().commands();
                    let size = pane.size();
                    snapshot.panes.insert(
                        id,
                        Seen {
                            ws: ws.id,
                            tab: tab.id,
                            title: pane.title.as_str().to_owned(),
                            size: (size.cols(), size.rows()),
                            ended: pane.process.exit_status(),
                            starts: pane.starts,
                            finished: commands.finished,
                            last_status: commands.last_status,
                            bells: pane.alerts.bells,
                            notifications: pane.alerts.notifications,
                            last_notification: pane.alerts.last.clone(),
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
                | Json::Object(_)) => other.render(),
            };
            line.push_str(&format!(" {key}={value}"));
        }
        line.push('\n');
        line
    }
}

/// What changed from `old` to `new`, in order: what was made, what happened
/// to panes there all along, then what closed.
pub fn between(old: &Snapshot, new: &Snapshot) -> Vec<Event> {
    let mut events = Vec::new();
    let name = |n: &str| Json::str(n.to_owned());
    for (&ws, ws_name) in &new.workspaces {
        match old.workspaces.get(&ws) {
            None => events.push(Event::new("created", ws, None, None).with("name", name(ws_name))),
            Some(was) if was != ws_name => {
                events.push(Event::new("renamed", ws, None, None).with("name", name(ws_name)));
            }
            Some(_) => {}
        }
    }
    for (&tab, (ws, tab_name)) in &new.tabs {
        match old.tabs.get(&tab) {
            None => {
                let event = Event::new("created", *ws, Some(tab), None);
                events.push(event.with("name", name(tab_name)));
            }
            Some((_, was)) if was != tab_name => {
                let event = Event::new("renamed", *ws, Some(tab), None);
                events.push(event.with("name", name(tab_name)));
            }
            Some(_) => {}
        }
    }
    for (&pane, now) in &new.panes {
        let at = |name| Event::new(name, now.ws, Some(now.tab), Some(pane));
        let Some(was) = old.panes.get(&pane) else {
            events.push(at("created"));
            continue;
        };
        if (was.ws, was.tab) != (now.ws, now.tab) {
            events.push(at("moved"));
        }
        if now.starts != was.starts {
            events.push(at("respawned"));
        }
        if now.finished > was.finished {
            let status = now
                .last_status
                .map_or(Json::Null, |s| Json::Number(i64::from(s)));
            events.push(at("done").with("status", status));
        }
        if let (Some(ended), true) = (now.ended, now.ended != was.ended) {
            let event = match ended {
                Ended::Exited(code) => at("exited").with("status", Json::Number(i64::from(code))),
                Ended::Signalled(signal) => {
                    at("exited").with("signal", Json::Number(i64::from(signal)))
                }
            };
            events.push(event);
        }
        if now.bells > was.bells {
            events.push(at("bell"));
        }
        if now.notifications > was.notifications {
            let text = now.last_notification.clone().unwrap_or_default();
            events.push(at("notification").with("text", Json::str(text)));
        }
        if now.title != was.title {
            events.push(at("title").with("title", Json::str(now.title.clone())));
        }
        if now.size != was.size {
            let (cols, rows) = now.size;
            let event = at("resized").with("cols", Json::Number(i64::from(cols)));
            events.push(event.with("rows", Json::Number(i64::from(rows))));
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
    /// session is `now`.
    pub fn told(&mut self, now: &Snapshot) -> String {
        if *now == self.seen {
            return String::new();
        }
        let events = between(&self.seen, now);
        self.seen = now.clone();
        (events.iter())
            .filter(|e| self.wants(e))
            .map(|e| e.line(self.json))
            .collect()
    }
}

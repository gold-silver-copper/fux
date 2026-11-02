use super::{Error, Session};
use crate::pane::Pane;
use crate::view::ClientKind;
use crate::workspace::{Child, Fit};
use fux_wire::command::AnyRef;
use fux_wire::id::{PaneId, TabId};
use fux_wire::inspect::{self, Projection, Scope, Snapshot};
use fux_wire::json::Json;
use fux_wire::layout::{Node, Rect};
use fux_wire::protocol::Body;
use fux_wire::wire::Memory;
use std::collections::BTreeMap;

impl Session {
    /// A pane's observation, read in this turn, as `projection` asks:
    /// where it is in its tab, `at`; but in a topology, its program, launch
    /// and terminal's facts; a recipe's, also where its program is now,
    /// which `dump` starts a shell in.
    fn observe(
        &self,
        pane: &Pane,
        tab: TabId,
        at: Option<Rect>,
        projection: &Projection,
    ) -> Result<inspect::Pane, Error> {
        let facts = if *projection == Projection::Topology {
            None
        } else {
            let terminal = inspect::Terminal::ALL.iter().copied();
            Some(inspect::Facts {
                program: crate::capture::program(pane)?,
                launch: crate::capture::launch(pane),
                terminal: terminal
                    .map(|t| (t, crate::capture::terminal(t, pane)))
                    .collect(),
            })
        };
        let cwd = if *projection == Projection::Recipe {
            Some(match self.dir_with(pane.id(), &mut Memory)? {
                Ok(Some(dir)) => inspect::Directory::Known(crate::capture::path(dir.as_os_str())),
                Ok(None) => inspect::Directory::Unknown,
                Err(errno) => inspect::Directory::Failed(errno.raw()),
            })
        } else {
            None
        };
        Ok(inspect::Pane {
            id: pane.id(),
            tab,
            version: pane.version(self.run),
            name: pane.name().map(str::to_owned),
            label: format!("{}", pane.display_label()),
            title: pane.title().as_str().to_owned(),
            rows: pane.size().rows(),
            cols: pane.size().cols(),
            row: at.as_ref().map(Rect::y),
            col: at.as_ref().map(Rect::x),
            facts,
            cwd,
        })
    }

    /// One pane's facts observation, alone: what a capture holds.
    pub fn observed(&self, id: PaneId) -> Result<inspect::Pane, Error> {
        let pane = self.pane(id)?;
        let (_, tab) = self.locate(id).ok_or(Error::NoPane(id))?;
        let at = self.tab(tab).and_then(|t| self.sized(t).rect(id));
        self.observe(pane, tab, at, &Projection::Facts)
    }

    /// One field of a pane's facts observation, read alone from what
    /// `observe` reads it from, as `inspect::Pane::fields` gives it: a
    /// version or program read lays out no tab and reads no pictures.
    pub(super) fn observed_field(&self, id: PaneId, field: &str) -> Result<Option<Body>, Error> {
        let pane = self.pane(id)?;
        let tab = || self.locate(id).map(|(_, tab)| tab).ok_or(Error::NoPane(id));
        let at = || Ok::<_, Error>(self.tab(tab()?).and_then(|t| self.sized(t).rect(id)));
        let place = |at: Option<u16>| at.map_or(Json::Null, Json::int);
        let value = match field {
            "id" => return Ok(Some(Body::Found(AnyRef::Pane(id)))),
            "tab" => return Ok(Some(Body::Found(AnyRef::Tab(tab()?)))),
            "version" => Json::str(pane.version(self.run).to_string()),
            "name" => pane.name().map_or(Json::Null, Json::str),
            "label" => Json::str(format!("{}", pane.display_label())),
            "title" => Json::str(pane.title().as_str()),
            "rows" => Json::int(pane.size().rows()),
            "cols" => Json::int(pane.size().cols()),
            "row" => place(at()?.as_ref().map(Rect::y)),
            "col" => place(at()?.as_ref().map(Rect::x)),
            "program" => crate::capture::program(pane)?,
            "launch" => crate::capture::launch(pane).json(),
            _ => {
                let count = pane.counts().named().into_iter().find(|(n, _)| *n == field);
                match (count, inspect::Terminal::named(field)) {
                    (Some((_, n)), _) => Json::uint(n),
                    (None, Some(fact)) => crate::capture::terminal(fact, pane),
                    (None, None) => return Ok(None),
                }
            }
        };
        Ok(Some(Body::Field(value)))
    }

    pub(super) fn inspect(&self, scope: Scope, projection: Projection) -> Result<Body, Error> {
        if let Projection::Field(field) = &projection {
            let Scope::Target(target) = scope else {
                return Err(super::fail(
                    fux_wire::code::Raise::BadValue,
                    "a field projection needs a pane, tab or workspace",
                ));
            };
            return self.field(self.there(target)?, field);
        }
        let target = match scope {
            Scope::All => None,
            Scope::Target(target) => Some(self.there(target)?),
            Scope::Client(client) => {
                self.views.get(&client).ok_or(Error::NoClient(client))?;
                self.place(client).map(|place| AnyRef::Workspace(place.ws))
            }
        };
        let selected_place = match target {
            Some(AnyRef::Pane(p)) => Some(self.locate(p).ok_or(Error::NoPane(p))?),
            Some(AnyRef::Tab(t)) => Some((self.tab_workspace(t)?, t)),
            Some(AnyRef::Workspace(_)) | None => None,
        };
        let empty = matches!(scope, Scope::Client(_)) && target.is_none();
        let mut snapshot = Snapshot {
            run: self.run,
            cursor: self.parts.journal().end(self.run),
            scope,
            projection: projection.clone(),
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            clients: Vec::new(),
        };
        // The workspaces in scope, each before those in it, and their tabs
        // in order: a workspace and those in it, or a tab's or pane's own.
        let (workspaces, tabs) = match (selected_place, target) {
            _ if empty => (Vec::new(), Vec::new()),
            (Some((ws, tab)), _) => {
                let tabs = self.parts.tab(tab).into_iter().collect();
                (self.workspace(ws).into_iter().collect(), tabs)
            }
            (None, Some(AnyRef::Workspace(ws))) => (
                self.parts.workspaces_in(Some(ws)),
                self.parts.tabs_in(Some(ws)),
            ),
            (None, _) => (self.parts.every_workspace(), self.parts.tabs_in(None)),
        };
        // The tabs whose whole layout is here, which choices index.
        let mut laid_out = Vec::new();
        for (ws, tab) in tabs {
            // Where each pane is, found once for the tab.
            let at: BTreeMap<PaneId, Rect> = self.sized(tab).panes.into_iter().collect();
            // A pane's tab as far as it is in scope: the pane alone.
            let tree = match target {
                Some(AnyRef::Pane(pane)) => vec![Node::Pane(pane)],
                Some(AnyRef::Tab(_) | AnyRef::Workspace(_)) | None => {
                    laid_out.push(tab);
                    tab.root().nodes()
                }
            };
            for node in &tree {
                let Node::Pane(id) = node else {
                    continue;
                };
                let pane = self.parts.panes().get(id).ok_or(Error::NoPane(*id))?;
                let fact = self.observe(pane, tab.id(), at.get(id).copied(), &projection)?;
                snapshot.panes.push(fact);
            }
            let size = tab.fit().size();
            let fact = inspect::Tab {
                id: tab.id(),
                workspace: ws,
                name: tab.name().map(str::to_owned),
                label: format!("{}", tab.display_label(self.parts.panes())),
                fixed: matches!(tab.fit(), Fit::Fixed(_)),
                cols: size.cols(),
                rows: size.rows(),
                nodes: tree,
                revision: tab.revision(self.run),
                host: self.host(Some(tab.id())),
                zoomed: tab.zoomed(),
            };
            snapshot.tabs.push(fact);
        }
        for ws in workspaces {
            let fact = inspect::Workspace {
                id: ws.id(),
                parent: self.parts.parent(ws.id()).flatten(),
                name: ws.name().map(str::to_owned),
                label: format!("{}", ws.display_label()),
                children: ws.stack().children().iter().map(Child::id).collect(),
            };
            snapshot.workspaces.push(fact);
        }
        for view in self.views.values() {
            let place = self.place(view.id);
            let selected = match scope {
                Scope::All => true,
                Scope::Client(id) => id == view.id,
                Scope::Target(AnyRef::Workspace(id)) => {
                    place.is_some_and(|p| self.parts.within(p.ws, id))
                }
                Scope::Target(AnyRef::Tab(id)) => place.is_some_and(|p| p.tab == id),
                Scope::Target(AnyRef::Pane(id)) => place.is_some_and(|p| p.pane == id),
            };
            if !selected {
                continue;
            }
            let terminal = view.terminal.facts();
            let graphics = match view.terminal.graphics() {
                fux_vt::Graphics::Text => "text",
                fux_vt::Graphics::Cell(_) => "cell",
                fux_vt::Graphics::Sixel(_) => "sixel",
            };
            let graphics = Json::str(graphics);
            let clipboard = view
                .presentation
                .terminal_clipboard
                .then_some("osc52")
                .map_or(Json::Null, Json::str);
            let output = Json::object(vec![
                ("graphics", graphics),
                (
                    "underline_styles",
                    Json::Bool(view.terminal.underline_styles),
                ),
                ("clipboard", clipboard),
                (
                    "kitty_flags_requested",
                    view.terminal
                        .kitty
                        .map_or(Json::Null, |_| Json::int(crate::outer::KITTY_FLAGS)),
                ),
            ]);
            let fact = inspect::Client {
                id: view.id,
                terminal: view.kind == ClientKind::Terminal,
                terminal_facts: terminal,
                output,
                read_only: view.read_only,
                rows: view.rows,
                cols: view.cols,
                pane_rows: view.pane_area().h(),
                pane_cols: view.pane_area().w(),
                workspace: place.map(|p| p.ws),
                tab: place.map(|p| p.tab),
                pane: place.map(|p| p.pane),
                seats: self.parts.seats(view.id),
                choices: (laid_out.iter())
                    .filter_map(|tab| Some((tab.revision(self.run)?, tab.root())))
                    .flat_map(|(revision, root)| {
                        let choices = root.choices(view.id).into_iter();
                        choices.map(move |(stack, shown)| inspect::Choice {
                            revision,
                            stack,
                            current: shown.current,
                            last: shown.last,
                        })
                    })
                    .collect(),
                opening: view.mode.opening(),
            };
            snapshot.clients.push(fact);
        }
        Ok(Body::Snapshot(Box::new(snapshot)))
    }
}

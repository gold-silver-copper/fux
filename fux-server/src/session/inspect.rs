use super::{Error, Session};
use crate::view::{ClientKind, Mode};
use crate::workspace::{Child, Fit};
use fux_wire::command::AnyRef;
use fux_wire::inspect::{self, Projection, Scope, Snapshot};
use fux_wire::json::Json;
use fux_wire::layout::{Node, Tree};
use fux_wire::protocol::Body;
use fux_wire::wire::Memory;

impl Session {
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
        let selected_tab = selected_place.map(|(_, tab)| tab);
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
        for ws in self.parts.workspaces().iter().filter(|_| !empty) {
            if selected_place.is_some_and(|(id, _)| id != ws.id())
                || matches!(target,Some(AnyRef::Workspace(id)) if id != ws.id())
            {
                continue;
            }
            let selected = |tab: &&crate::workspace::Tab| match target {
                Some(AnyRef::Workspace(id)) => ws.id() == id,
                Some(AnyRef::Tab(_) | AnyRef::Pane(_)) => selected_tab == Some(tab.id()),
                None => true,
            };
            if !ws.tabs().iter().any(|tab| selected(&tab)) {
                continue;
            }
            let tabs = ws.tabs().iter().map(|tab| tab.id()).collect();
            for tab in ws.tabs().iter().filter(selected) {
                // A pane's tab as far as it is in scope: the pane alone.
                let tree = match target {
                    Some(AnyRef::Pane(pane)) => flatten(&Tree::Pane(pane)),
                    Some(AnyRef::Tab(_) | AnyRef::Workspace(_)) | None => flatten(tab.root()),
                };
                for node in &tree {
                    let Node::Pane(id) = node else {
                        continue;
                    };
                    let pane = self.parts.panes().get(id).ok_or(Error::NoPane(*id))?;
                    let (state, launch) = if projection == Projection::Topology {
                        (None, None)
                    } else {
                        let fields = crate::capture::state(pane)?;
                        let fields = fields.into_iter().map(|(k, v)| (k.to_owned(), v));
                        let launch = pane.runs.launch();
                        let launch = inspect::Launch {
                            shell: matches!(pane.runs, crate::pane::Runs::Shell(_)),
                            argv: launch.argv.iter().map(str::to_owned).collect(),
                            cwd: path(launch.cwd.as_os_str()),
                        };
                        (Some(Json::Object(fields.collect())), Some(launch))
                    };
                    let cwd = if projection == Projection::Recipe {
                        Some(match self.dir_with(*id, &mut Memory)? {
                            Ok(Some(dir)) => inspect::Directory::Known(path(dir.as_os_str())),
                            Ok(None) => inspect::Directory::Unknown,
                            Err(errno) => inspect::Directory::Failed(errno.raw()),
                        })
                    } else {
                        None
                    };
                    let fact = inspect::Pane {
                        id: *id,
                        tab: tab.id(),
                        name: pane.name().map(str::to_owned),
                        label: format!("{}", pane.display_label()),
                        title: pane.title().as_str().to_owned(),
                        rows: pane.size().rows(),
                        cols: pane.size().cols(),
                        version: pane.version(self.run),
                        state,
                        launch,
                        cwd,
                        pid: pane.process().child().map(|c| c.leader.pid().as_raw()),
                    };
                    snapshot.panes.push(fact);
                }
                let size = tab.fit().size();
                let fact = inspect::Tab {
                    id: tab.id(),
                    workspace: ws.id(),
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
            let fact = inspect::Workspace {
                id: ws.id(),
                name: ws.name().map(str::to_owned),
                label: format!("{}", ws.display_label()),
                tabs,
            };
            snapshot.workspaces.push(fact);
        }
        for view in self.views.values() {
            let place = self.place(view.id);
            let selected = match scope {
                Scope::All => true,
                Scope::Client(id) => id == view.id,
                Scope::Target(AnyRef::Workspace(id)) => place.is_some_and(|p| p.ws == id),
                Scope::Target(AnyRef::Tab(id)) => place.is_some_and(|p| p.tab == id),
                Scope::Target(AnyRef::Pane(id)) => place.is_some_and(|p| p.pane == id),
            };
            if !selected {
                continue;
            }
            let mode = match view.mode {
                Mode::Normal => "normal",
                Mode::Panel(_) => "panel",
            };
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
                opening: (!view.mode.is_normal()).then_some(view.opening),
                mode: mode.to_owned(),
            };
            snapshot.clients.push(fact);
        }
        Ok(Body::Snapshot(Box::new(snapshot)))
    }
}

fn path(value: &std::ffi::OsStr) -> inspect::Path {
    use std::os::unix::ffi::OsStrExt;
    match value.to_str() {
        Some(text) => inspect::Path::Utf8(text.to_owned()),
        None => inspect::Path::Bytes(value.as_bytes().to_vec()),
    }
}

/// Preorder nodes share arrange's schema and fixed root index zero.
fn flatten(root: &Tree) -> Vec<Node> {
    let mut work = vec![(root, None)];
    let mut nodes = Vec::new();
    while let Some((tree, parent)) = work.pop() {
        let at = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
        if let Some((index, second_child)) = parent
            && let Some(Node::Split { first, second, .. }) = nodes.get_mut(index)
        {
            if second_child {
                *second = at;
            } else {
                *first = at;
            }
        }
        let node = match tree {
            Tree::Pane(pane) => Node::Pane(*pane),
            Tree::Split(split) => {
                let index = nodes.len();
                let (first, second) = split.children();
                work.extend([(second, Some((index, true))), (first, Some((index, false)))]);
                let (first_share, second_share) = split.shares().get();
                Node::Split {
                    axis: split.axis(),
                    first_share,
                    second_share,
                    first: 0,
                    second: 0,
                }
            }
        };
        nodes.push(node);
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_trees_are_flat() {
        let mut tree = Tree::Pane(fux_wire::id::PaneId::of(1));
        for n in 2..70 {
            tree.append(fux_wire::id::PaneId::of(n));
        }
        let nodes = flatten(&tree);
        assert!(matches!(nodes.first(), Some(Node::Split { .. })));
        assert_eq!(
            nodes.iter().filter(|n| matches!(n, Node::Pane(_))).count(),
            69
        );
    }

    #[test]
    fn recipe_paths_keep_exact_os_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"/a\xff/b");
        assert_eq!(path(raw), inspect::Path::Bytes(b"/a\xff/b".to_vec()));
    }
}

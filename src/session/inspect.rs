use super::{Error, Session};
use crate::command::AnyRef;
use crate::inspect::{self, Projection, Scope, Snapshot};
use crate::layout::{Node, Tree};
use crate::protocol::Body;
use crate::view::{ClientKind, Mode};
use crate::wire::Budget;
use crate::workspace::Fit;

impl Session {
    pub(super) fn inspect(&self, scope: Scope, projection: Projection) -> Result<Body, Error> {
        if let Projection::Field(field) = &projection {
            let Scope::Target(target) = scope else {
                return Err(super::fail(
                    crate::code::Raise::BadValue,
                    "a field projection needs a pane, tab or workspace",
                ));
            };
            if field.len() > super::NAME_MAX {
                return Err(super::fail(
                    crate::code::Raise::TooLong,
                    "field name is too long",
                ));
            }
            return self.field(self.there(target)?, field);
        }
        let budget = &mut Budget::with_limit(inspect::BYTES);
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
            events: self.parts.journal().end(self.run),
            scope,
            projection: projection.clone(),
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            clients: Vec::new(),
        };
        let now = std::time::Instant::now();
        let mut nodes = 0usize;
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
            let mut tabs = Vec::new();
            for tab in ws.tabs() {
                add(&mut tabs, tab.id(), budget, &mut nodes)?;
            }
            for tab in ws.tabs().iter().filter(selected) {
                let tree = flatten(tab.root(), budget, &mut nodes)?;
                for node in &tree {
                    let Node::Pane(id) = node else {
                        continue;
                    };
                    if projection == Projection::Facts
                        && matches!(target, Some(AnyRef::Pane(p)) if p != *id)
                    {
                        continue;
                    }
                    let pane = self.parts.panes().get(id).ok_or(Error::NoPane(*id))?;
                    let state = if projection == Projection::Facts {
                        let fields = crate::capture::state(pane, now, self.copying(*id), budget)?;
                        Some(crate::json::Json::Object(budget.fields(fields)?))
                    } else {
                        None
                    };
                    let recipe = if projection == Projection::Recipe {
                        let launch = pane.runs.launch();
                        let mut argv = budget.vector(launch.argv.len())?;
                        for word in launch.argv.iter() {
                            argv.push(budget.string(word)?);
                        }
                        let mut env = budget.vector(launch.env.as_slice().len())?;
                        for (name, value) in launch.env.as_slice() {
                            env.push((
                                budget.string(name)?,
                                value.as_ref().map(|v| path(v, budget)).transpose()?,
                            ));
                        }
                        Some(inspect::Recipe {
                            shell: matches!(pane.runs, crate::pane::Runs::Shell(_)),
                            argv,
                            env,
                            initial_cwd: path(launch.cwd.as_os_str(), budget)?,
                            observed_cwd: match self.dir_with(*id, budget)? {
                                Ok(Some(dir)) => {
                                    inspect::Directory::Known(path(dir.as_os_str(), budget)?)
                                }
                                Ok(None) => inspect::Directory::Unknown,
                                Err(errno) => inspect::Directory::Failed(errno.raw()),
                            },
                            options: pane.options(),
                        })
                    } else {
                        None
                    };
                    let fact = inspect::Pane {
                        id: *id,
                        tab: tab.id(),
                        name: optional(pane.name(), budget)?,
                        label: budget.format(format_args!("{}", pane.display_label()))?,
                        title: budget.string(pane.title().as_str())?,
                        rows: pane.size().rows(),
                        cols: pane.size().cols(),
                        version: pane.version(self.run),
                        state,
                        recipe,
                        pid: pane.process().child().map(|c| c.leader.pid().as_raw()),
                    };
                    add(&mut snapshot.panes, fact, budget, &mut nodes)?;
                }
                let size = tab.fit().size();
                let fact = inspect::Tab {
                    id: tab.id(),
                    workspace: ws.id(),
                    name: optional(tab.name(), budget)?,
                    label: budget
                        .format(format_args!("{}", tab.display_label(self.parts.panes())))?,
                    fixed: matches!(tab.fit(), Fit::Fixed(_)),
                    cols: size.cols(),
                    rows: size.rows(),
                    nodes: tree,
                    revision: tab.revision(self.run),
                };
                add(&mut snapshot.tabs, fact, budget, &mut nodes)?;
            }
            let fact = inspect::Workspace {
                id: ws.id(),
                name: optional(ws.name(), budget)?,
                label: budget.format(format_args!("{}", ws.display_label()))?,
                tabs,
            };
            add(&mut snapshot.workspaces, fact, budget, &mut nodes)?;
        }
        for view in self.views.values() {
            let place = self.place(view.id);
            let selected = match scope {
                Scope::All => true,
                Scope::Client(id) => id == view.id,
                Scope::Target(AnyRef::Workspace(id)) => place.is_some_and(|p| p.ws == id),
                Scope::Target(AnyRef::Tab(id)) => place.is_some_and(|p| p.tab == id),
                Scope::Target(AnyRef::Pane(_)) => {
                    place.is_some_and(|p| Some(p.tab) == selected_tab)
                }
            };
            if !selected {
                continue;
            }
            let mode = match view.mode {
                Mode::Normal => "normal",
                Mode::Panel(_) => "panel",
                Mode::Copy(_) => "copy",
            };
            let fact = inspect::Client {
                id: view.id,
                terminal: view.kind == ClientKind::Terminal,
                read_only: view.read_only,
                rows: view.rows,
                cols: view.cols,
                pane_rows: view.pane_area().h(),
                pane_cols: view.pane_area().w(),
                workspace: place.map(|p| p.ws),
                tab: place.map(|p| p.tab),
                pane: place.map(|p| p.pane),
                zoom: view.zoom,
                opening: (!view.mode.is_normal()).then_some(view.opening),
                mode: budget.string(mode)?,
            };
            add(&mut snapshot.clients, fact, budget, &mut nodes)?;
        }
        if snapshot.node_count() != Some(nodes) {
            return Err(Budget::exceeded());
        }
        budget.slots::<Snapshot>(1)?;
        Ok(Body::Snapshot(Box::new(snapshot)))
    }
}

fn optional(text: Option<&str>, budget: &mut Budget) -> Result<Option<String>, Error> {
    text.map(|s| budget.string(s)).transpose()
}

fn path(value: &std::ffi::OsStr, budget: &mut Budget) -> Result<inspect::Path, Error> {
    use std::os::unix::ffi::OsStrExt;
    Ok(match value.to_str() {
        Some(text) => inspect::Path::Utf8(budget.string(text)?),
        None => {
            let mut bytes = budget.vector(value.len())?;
            bytes.extend_from_slice(value.as_bytes());
            inspect::Path::Bytes(bytes)
        }
    })
}

fn add<T>(out: &mut Vec<T>, value: T, budget: &mut Budget, nodes: &mut usize) -> Result<(), Error> {
    *nodes = nodes
        .checked_add(1)
        .filter(|n| *n <= inspect::NODES)
        .ok_or_else(Budget::exceeded)?;
    budget.reserve(out, 1)?;
    out.push(value);
    Ok(())
}

/// Preorder nodes share arrange's schema and fixed root index zero.
fn flatten(root: &Tree, budget: &mut Budget, count: &mut usize) -> Result<Vec<Node>, Error> {
    let mut work = budget.vector(1)?;
    work.push((root, None));
    let mut nodes = Vec::new();
    while let Some((tree, parent)) = work.pop() {
        let at = u32::try_from(nodes.len()).map_err(|_| Budget::exceeded())?;
        if let Some((index, second_child)) = parent {
            let Some(Node::Split { first, second, .. }) = nodes.get_mut(index) else {
                return Err(Budget::exceeded());
            };
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
                budget.reserve(&mut work, 2)?;
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
        add(&mut nodes, node, budget, count)?;
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_trees_are_flat_and_extraction_refuses_its_bounds() -> Result<(), Error> {
        let mut tree = Tree::Pane(crate::id::PaneId::of(1));
        for n in 2..70 {
            tree.append(crate::id::PaneId::of(n));
        }
        let mut count = 0;
        let nodes = flatten(&tree, &mut Budget::with_limit(inspect::BYTES), &mut count)?;
        assert!(matches!(nodes.first(), Some(Node::Split { .. })));
        assert_eq!(
            nodes.iter().filter(|n| matches!(n, Node::Pane(_))).count(),
            69
        );
        assert!(flatten(&tree, &mut Budget::with_limit(0), &mut 0).is_err());
        let mut exhausted = inspect::NODES;
        assert!(flatten(&tree, &mut Budget::new(), &mut exhausted).is_err());
        Ok(())
    }

    #[test]
    fn recipe_paths_keep_exact_os_bytes() -> Result<(), Error> {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"/a\xff/b");
        assert_eq!(
            path(raw, &mut Budget::new())?,
            inspect::Path::Bytes(b"/a\xff/b".to_vec())
        );
        Ok(())
    }
}

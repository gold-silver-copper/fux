//! One turn's retained hierarchy and optional process or launch observations.
use crate::command::{AnyRef, Run, Version};
use crate::events::Cursor;
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::json::Json;
use crate::layout::Node;
use crate::wire::wire;

/// Nesting of records and their lists; layout trees are flat.
pub const DEPTH: u8 = 32;

wire! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Scope {
        All,
        Target(target: AnyRef),
        Client(client: ClientId),
    }
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Projection { Facts, Topology, Recipe, Field(field: String), }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub run: Run,
    pub cursor: Cursor,
    pub scope: Scope,
    pub projection: Projection,
    pub workspaces: Vec<Workspace>,
    pub tabs: Vec<Tab>,
    pub panes: Vec<Pane>,
    pub clients: Vec<Client>,
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Workspace {
        pub id: WsId,
        /// The workspace it is in, none at the top.
        pub parent: Option<WsId>,
        pub name: Option<String>,
        pub label: String,
        /// Its tabs and workspaces, in order.
        pub children: Vec<crate::command::AnyRef>,
    }
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Tab {
        pub id: TabId,
        pub workspace: WsId,
        pub name: Option<String>,
        pub label: String,
        pub fixed: bool,
        pub cols: u16,
        pub rows: u16,
        pub nodes: Vec<Node>,
        pub revision: Option<crate::command::LayoutRevision>,
        pub host: Option<ClientId>,
        /// The pane that fills it on every screen showing it, if one does.
        pub zoomed: Option<PaneId>,
    }
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Pane {
        pub id: PaneId,
        pub tab: TabId,
        pub name: Option<String>,
        pub label: String,
        pub title: String,
        pub rows: u16,
        pub cols: u16,
        /// Where it is in its tab, as it is sized; none if it has no place.
        pub row: Option<u16>,
        pub col: Option<u16>,
        pub version: Version,
        pub pid: Option<i32>,
        pub state: Option<Json>,
        pub launch: Option<Launch>,
        /// Where it is now, which only the recipe reads.
        pub cwd: Option<Directory>,
    }
}
wire! {
    /// What it was started as: a shell or a program, and where.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Launch {
        pub shell: bool,
        pub argv: Vec<String>,
        pub cwd: Path,
    }
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Directory { Known(path: Path), Unknown, Failed(errno: i32), }
}
wire! {
    /// Exact OS bytes, never lossily converted to text.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Path { Utf8(text: String), Bytes(bytes: Vec<u8>), }
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Client {
        pub id: ClientId,
        pub terminal: bool,
        pub terminal_facts: Json,
        pub output: Json,
        pub read_only: bool,
        pub rows: u16,
        pub cols: u16,
        pub pane_rows: u16,
        pub pane_cols: u16,
        pub workspace: Option<WsId>,
        pub tab: Option<TabId>,
        pub pane: Option<PaneId>,
        /// What it is shown in each stack and focuses in each tab.
        pub seats: Vec<Seat>,
        /// What it is shown at each stack in the layouts of the tabs here.
        pub choices: Vec<Choice>,
        /// The screen it shows over its panes, if one.
        pub opening: Option<crate::act::Opening>,
    }
}
wire! {
    /// A client's choice among what `within` holds (the server's
    /// workspaces, with none): the one it is shown, and the one before.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Seat {
        pub within: Option<AnyRef>,
        pub current: AnyRef,
        pub last: Option<AnyRef>,
    }
}

wire! {
    /// A client's choice at a stack in a tab's layout, at the tab's
    /// `revision`: the stack, the child it is shown and the one before, as
    /// indices of the tab's `nodes`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Choice {
        pub revision: crate::command::LayoutRevision,
        pub stack: u32,
        pub current: u32,
        pub last: Option<u32>,
    }
}

crate::wire::shaped!(
    codec struct Snapshot {
        run: Run,
        cursor: Cursor,
        scope: Scope,
        projection: Projection,
        workspaces: Vec<Workspace>,
        tabs: Vec<Tab>,
        panes: Vec<Pane>,
        clients: Vec<Client>,
    }
);

impl crate::wire::Wire for Snapshot {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.put_fields(out);
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        let (run, cursor, scope, projection, workspaces, tabs, panes, clients) =
            Self::get_fields(r)?;
        Ok(Self {
            run,
            cursor,
            scope,
            projection,
            workspaces,
            tabs,
            panes,
            clients,
        })
    }
}

impl Snapshot {
    pub fn node_count(&self) -> Option<usize> {
        let entities = self
            .workspaces
            .len()
            .checked_add(self.tabs.len())?
            .checked_add(self.panes.len())?
            .checked_add(self.clients.len())?;
        let nodes = self
            .tabs
            .iter()
            .try_fold(entities, |n, t| n.checked_add(t.nodes.len()))?;
        self.workspaces
            .iter()
            .try_fold(nodes, |n, w| n.checked_add(w.children.len()))
    }
}

#[cfg(any(test, feature = "testing"))]
impl Snapshot {
    /// A closed schema also checks its cross references and flat topology.
    pub fn validate(&self) -> Result<(), crate::wire::Error> {
        use crate::wire::Error;
        let bad = || Error::Bad("snapshot topology or projection");
        if self.run != self.cursor.run || matches!(self.projection, Projection::Field(_)) {
            return Err(bad());
        }
        fn sorted<T: Ord + Copy>(items: impl Iterator<Item = T>) -> Result<Vec<T>, Error> {
            let mut out: Vec<T> = items.collect();
            out.sort_unstable();
            if out.iter().zip(out.iter().skip(1)).any(|(a, b)| a == b) {
                return Err(Error::Bad("duplicate snapshot identity"));
            }
            Ok(out)
        }
        let ws = sorted(self.workspaces.iter().map(|w| w.id))?;
        sorted(self.tabs.iter().map(|t| t.id))?;
        let panes = sorted(self.panes.iter().map(|p| p.id))?;
        sorted(self.clients.iter().map(|c| c.id))?;
        // Each workspace holds what names it as its own, and something.
        let holds = |w: &Workspace, child| w.children.contains(&child);
        let held_by = |id: Option<WsId>, child| {
            id.and_then(|id| self.workspaces.iter().find(|w| w.id == id))
                .is_none_or(|w| holds(w, child))
        };
        for w in &self.workspaces {
            sorted(w.children.iter().copied())?;
            if w.children.is_empty() || !held_by(w.parent, AnyRef::Workspace(w.id)) {
                return Err(bad());
            }
        }
        if !(self.tabs.iter()).all(|t| held_by(Some(t.workspace), AnyRef::Tab(t.id))) {
            return Err(bad());
        }
        let mut ordered_panes = self.panes.iter();
        let mut leaves = Vec::with_capacity(panes.len());
        for t in &self.tabs {
            if ws.binary_search(&t.workspace).is_err()
                || t.rows == 0
                || t.cols == 0
                || t.nodes.is_empty()
                || t.revision
                    .is_some_and(|r| r.run != self.run || r.tab != t.id)
            {
                return Err(bad());
            }
            let mut seen = vec![false; t.nodes.len()];
            let mut pending = vec![0usize];
            while let Some(at) = pending.pop() {
                let visited = seen.get_mut(at).ok_or_else(bad)?;
                if *visited {
                    return Err(bad());
                }
                *visited = true;
                match t.nodes.get(at).ok_or_else(bad)? {
                    Node::Pane(pane) => {
                        leaves.push((*pane, t.id));
                        let omitted = matches!((self.scope, &self.projection), (Scope::Target(AnyRef::Pane(id)), Projection::Facts) if id != *pane);
                        if !omitted
                            && !ordered_panes
                                .next()
                                .is_some_and(|p| p.id == *pane && p.tab == t.id)
                        {
                            return Err(bad());
                        }
                    }
                    Node::Split { first, second, .. } => {
                        pending.extend([*second as usize, *first as usize]);
                    }
                    Node::Stack { children } => {
                        pending.extend(children.iter().rev().map(|c| *c as usize));
                    }
                }
            }
            if seen.iter().any(|visited| !visited) {
                return Err(bad());
            }
        }
        if ordered_panes.next().is_some() {
            return Err(bad());
        }
        leaves.sort_unstable();
        if leaves
            .iter()
            .zip(leaves.iter().skip(1))
            .any(|(a, b)| a.0 == b.0)
        {
            return Err(bad());
        }
        for p in &self.panes {
            if p.version.run != self.run
                || p.version.pane != p.id
                || p.state.is_some() != (self.projection != Projection::Topology)
                || p.launch.is_some() != (self.projection != Projection::Topology)
                || p.cwd.is_some() != (self.projection == Projection::Recipe)
            {
                return Err(bad());
            }
        }
        let owners = sorted(self.tabs.iter().map(|t| (t.id, t.workspace)))?;
        for c in &self.clients {
            let tab_ws = c
                .tab
                .and_then(|id| owners.binary_search_by_key(&id, |(t, _)| *t).ok())
                .and_then(|i| owners.get(i))
                .map(|(_, w)| *w);
            let pane_tab = c
                .pane
                .and_then(|id| leaves.binary_search_by_key(&id, |(p, _)| *p).ok())
                .and_then(|i| leaves.get(i))
                .map(|(_, t)| *t);
            if c.workspace != tab_ws || c.tab != pane_tab {
                return Err(bad());
            }
            // A choice is at a stack of a tab here, at its revision.
            for choice in &c.choices {
                let tab = self
                    .tabs
                    .iter()
                    .find(|t| t.revision == Some(choice.revision));
                let stack = tab.and_then(|t| t.nodes.get(choice.stack as usize));
                let Some(Node::Stack { children }) = stack else {
                    return Err(bad());
                };
                let child = |i: u32| children.contains(&i);
                if !child(choice.current) || choice.last.is_some_and(|l| !child(l)) {
                    return Err(bad());
                }
            }
        }
        Ok(())
    }
}

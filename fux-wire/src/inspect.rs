//! One turn's retained hierarchy and optional process or launch observations.
use crate::command::{AnyRef, Run, Version};
use crate::events::Cursor;
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::json::Json;
use crate::layout::Node;
use crate::wire::wire;
use std::borrow::Cow;

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
    /// A pane's observation, read in one server turn (`Session::observe`):
    /// its identity, tab and version, its name, label, title, size and
    /// place; but in a topology, its program, launch and terminal's facts;
    /// a recipe's, also where its program is now. `fields` gives it flat.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Pane {
        pub id: PaneId,
        pub tab: TabId,
        pub version: Version,
        pub name: Option<String>,
        pub label: String,
        pub title: String,
        pub rows: u16,
        pub cols: u16,
        /// Where it is in its tab, as it is sized; none if it has no place.
        pub row: Option<u16>,
        pub col: Option<u16>,
        /// None in a topology.
        pub facts: Option<Facts>,
        /// Where it is now, which only the recipe reads.
        pub cwd: Option<Directory>,
    }
}
wire! {
    /// What a pane holds and was started as: its program, by generation
    /// (null while a respawn is under way); its launch; and its terminal's
    /// facts and its options, in order.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Facts {
        pub program: Json,
        pub launch: Launch,
        pub terminal: Vec<(Terminal, Json)>,
    }
}
wire! {
    /// What it was started as, a shell or a program, and where; its
    /// program's `argv` says with what.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Launch {
        pub shell: bool,
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

/// `named!(Ty { Variant "name", … })`: a closed set of named values, an
/// enum, with `Ty::ALL` in order, each one's `name` and the one `named`.
macro_rules! named {
    ($(#[$doc:meta])* $ty:ident { $($var:ident $name:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $ty {
            $($var),+
        }
        impl $ty {
            pub const ALL: &[$ty] = &[$($ty::$var),+];
            pub fn name(self) -> &'static str {
                match self {
                    $($ty::$var => $name),+
                }
            }
            pub fn named(name: &str) -> Option<$ty> {
                Self::ALL.iter().copied().find(|t| t.name() == name)
            }
        }
    };
}

named!(
    /// The facts a pane's terminal says, each named once, in order: what a
    /// pane's `Facts::terminal` holds, each read by the server's
    /// `capture::terminal`. A name is none of the pane's own fields.
    Terminal {
        AltScreen "alt_screen",
        MouseMode "mouse_mode",
        MouseEncoding "mouse_encoding",
        BracketedPaste "bracketed_paste",
        FocusEvents "focus_events",
        AppCursor "app_cursor",
        AppKeypad "app_keypad",
        KeyboardFlags "keyboard_flags",
        Pictures "pictures",
        Lost "lost",
        Cell "cell",
        CursorVisible "cursor_visible",
        HistoryBytes "history_bytes",
        ProgramClipboard "program_clipboard",
        Reflow "reflow",
        AlternateScrollLines "alternate_scroll_lines",
        PictureBytes "picture_bytes",
        HistoryLines "history_lines",
        CommandRunning "command_running",
        LastStatus "last_status",
        LastOutput "last_output",
        Notification "notification",
        Truncated "truncated",
        InputQueued "input_queued",
    }
);

crate::wire::closed_wire!(Terminal, "terminal fact");

/// A field of a pane's observation after its `id`, `tab` and `version`,
/// not yet read: its version's counts, by their place in `Counts::named`;
/// its terminal's, by its place in `Facts::terminal`.
#[derive(Clone, Copy)]
enum Field {
    Count(usize),
    Name,
    Label,
    Title,
    Rows,
    Cols,
    Row,
    Col,
    Program,
    Launch,
    Terminal(usize),
    Cwd,
}

impl Pane {
    /// Its fields after `id`, `tab` and `version`, in order, as `json`
    /// gives them: what a snapshot's record and a capture hold.
    pub fn fields(&self) -> impl Iterator<Item = (&'static str, Cow<'_, Json>)> {
        self.names().map(|(name, field)| (name, self.read(field)))
    }

    /// One of `fields`, read alone.
    pub fn field(&self, name: &str) -> Option<Cow<'_, Json>> {
        let (_, field) = self.names().find(|(n, _)| *n == name)?;
        Some(self.read(field))
    }

    /// Its fields' names, in order: its counts, its own, then those it
    /// holds.
    fn names(&self) -> impl Iterator<Item = (&'static str, Field)> + '_ {
        let counts = self.version.counts.named().into_iter().enumerate();
        let counts = counts.map(|(at, (name, _))| (name, Field::Count(at)));
        let own = [
            ("name", Field::Name),
            ("label", Field::Label),
            ("title", Field::Title),
            ("rows", Field::Rows),
            ("cols", Field::Cols),
            ("row", Field::Row),
            ("col", Field::Col),
        ];
        let facts = self.facts.iter().flat_map(|facts| {
            let terminal = facts.terminal.iter().enumerate();
            let terminal = terminal.map(|(at, (t, _))| (t.name(), Field::Terminal(at)));
            [("program", Field::Program), ("launch", Field::Launch)]
                .into_iter()
                .chain(terminal)
        });
        let cwd = self.cwd.iter().map(|_| ("cwd", Field::Cwd));
        counts.chain(own).chain(facts).chain(cwd)
    }

    /// A field `names` gives: null where it is not held.
    fn read(&self, field: Field) -> Cow<'_, Json> {
        let place = |at: Option<u16>| Cow::Owned(at.map_or(Json::Null, Json::int));
        let facts = self.facts.as_ref();
        let none = || Cow::Owned(Json::Null);
        match field {
            Field::Count(at) => Cow::Owned(
                (self.version.counts.named().get(at)).map_or(Json::Null, |(_, n)| Json::uint(*n)),
            ),
            Field::Name => Cow::Owned(self.name.as_deref().map_or(Json::Null, Json::str)),
            Field::Label => Cow::Owned(Json::str(self.label.as_str())),
            Field::Title => Cow::Owned(Json::str(self.title.as_str())),
            Field::Rows => Cow::Owned(Json::int(self.rows)),
            Field::Cols => Cow::Owned(Json::int(self.cols)),
            Field::Row => place(self.row),
            Field::Col => place(self.col),
            Field::Program => facts.map_or_else(none, |f| Cow::Borrowed(&f.program)),
            Field::Launch => facts.map_or_else(none, |f| Cow::Owned(f.launch.json())),
            Field::Terminal(at) => (facts.and_then(|f| f.terminal.get(at)))
                .map_or_else(none, |(_, v)| Cow::Borrowed(v)),
            Field::Cwd => self
                .cwd
                .as_ref()
                .map_or_else(none, |c| Cow::Owned(c.json())),
        }
    }
}

impl Launch {
    pub fn json(&self) -> Json {
        let choice = if self.shell { "shell" } else { "program" };
        Json::object(vec![
            ("choice", Json::str(choice)),
            ("cwd", self.cwd.json()),
        ])
    }
}

impl Directory {
    /// Null when the OS says none.
    pub fn json(&self) -> Json {
        match self {
            Directory::Known(path) => {
                Json::object(vec![("status", Json::str("known")), ("path", path.json())])
            }
            Directory::Unknown => Json::Null,
            Directory::Failed(errno) => Json::object(vec![
                ("status", Json::str("failed")),
                ("operation", Json::str("cwd")),
                ("errno", Json::int(*errno)),
            ]),
        }
    }
}

impl Path {
    /// UTF-8 as itself, else its bytes in hex.
    pub fn json(&self) -> Json {
        let (encoding, text) = match self {
            Path::Utf8(text) => ("utf8", text.clone()),
            Path::Bytes(bytes) => ("hex", bytes.iter().map(|b| format!("{b:02x}")).collect()),
        };
        Json::object(vec![
            ("encoding", Json::str(encoding)),
            ("text", Json::str(text)),
        ])
    }
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
                || p.facts.is_some() != (self.projection != Projection::Topology)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane's flat fields name each fact once: a terminal fact, keyed by
    /// the one table of them, can't be a `rows` or `launch` over the
    /// typed ones, and a name past the table is refused as it is read.
    #[test]
    fn a_terminal_fact_names_none_of_the_panes_own() -> Result<(), crate::wire::Error> {
        let version: Version = "%1:0.0.0.0.0:1-1"
            .parse()
            .map_err(|()| crate::wire::Error::Bad("version"))?;
        let pane = Pane {
            id: version.pane,
            tab: TabId::of(1),
            version,
            name: None,
            label: String::new(),
            title: String::new(),
            rows: 6,
            cols: 30,
            row: None,
            col: None,
            facts: Some(Facts {
                program: Json::Null,
                launch: Launch {
                    shell: true,
                    cwd: Path::Utf8("/".into()),
                },
                terminal: Terminal::ALL.iter().map(|t| (*t, Json::Null)).collect(),
            }),
            cwd: Some(Directory::Unknown),
        };
        let mut names: Vec<&str> = pane.fields().map(|(name, _)| name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "{names:?}");
        assert_eq!(pane.field("rows"), Some(Cow::Owned(Json::int(6u16))));
        let past = [u8::try_from(Terminal::ALL.len()).unwrap_or(u8::MAX)];
        assert!(crate::wire::Reader::new(&past).whole::<Terminal>().is_err());
        Ok(())
    }
}

//! One turn's retained hierarchy and optional process or launch observations.
use crate::command::{AnyRef, Run, Version};
use crate::events::Cursor;
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::json::Json;
use crate::layout::Axis;
use crate::wire::wire;
use std::num::NonZeroU32;

/// Aggregate construction space for a snapshot, including its temporary vectors.
pub const BYTES: usize = 8 << 20;
/// Aggregate records, tree nodes and scalar fields imported by a snapshot.
pub const NODES: usize = 65_536;
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
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Projection { Facts, Topology, Recipe, Field(field: Field), }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub run: Run,
    pub events: Cursor,
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
        pub name: Option<String>,
        pub label: String,
        pub tabs: Vec<TabId>,
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
        pub root: usize,
    }
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Node {
        Leaf(pane: PaneId),
        Split { axis: Axis, first: usize, second: usize, a: NonZeroU32, b: NonZeroU32 },
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
        pub version: Version,
        pub pid: Option<i32>,
        pub state: Option<Json>,
        pub recipe: Option<Recipe>,
    }
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Recipe {
        pub shell: bool,
        pub argv: Vec<String>,
        pub env: Vec<(String, Option<Path>)>,
        pub initial_cwd: Path,
        pub observed_cwd: Directory,
        pub history_bytes: usize,
        pub program_clipboard: bool,
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
        pub read_only: bool,
        pub rows: u16,
        pub cols: u16,
        pub pane_rows: u16,
        pub pane_cols: u16,
        pub workspace: Option<WsId>,
        pub tab: Option<TabId>,
        pub pane: Option<PaneId>,
        pub zoom: bool,
        pub opening: Option<u64>,
        pub mode: String,
    }
}

impl crate::wire::Wire for Snapshot {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.run.put(out);
        self.events.put(out);
        self.scope.put(out);
        self.projection.put(out);
        self.workspaces.put(out);
        self.tabs.put(out);
        self.panes.put(out);
        self.clients.put(out);
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        r.limited(BYTES, NODES, |r| {
            let snapshot = Self {
                run: Run::get(r)?,
                events: Cursor::get(r)?,
                scope: Scope::get(r)?,
                projection: Projection::get(r)?,
                workspaces: Vec::get(r)?,
                tabs: Vec::get(r)?,
                panes: Vec::get(r)?,
                clients: Vec::get(r)?,
            };
            snapshot.validate()?;
            Ok(snapshot)
        })
    }
}

impl Snapshot {
    /// A closed schema also checks its cross references and flat topology.
    pub(crate) fn validate(&self) -> Result<(), crate::wire::Error> {
        use crate::wire::Error;
        let bad = || Error::Bad("snapshot topology or projection");
        if self.run != self.events.run || matches!(self.projection, Projection::Field(_)) {
            return Err(bad());
        }
        let budget = &mut crate::wire::Budget::with_limit(BYTES);
        fn sorted<T: Ord + Copy>(
            items: impl Iterator<Item = T>,
            count: usize,
            budget: &mut crate::wire::Budget,
        ) -> Result<Vec<T>, Error> {
            let mut out = budget.vector(count).map_err(|_| Error::Limit)?;
            out.extend(items);
            out.sort_unstable();
            if out.iter().zip(out.iter().skip(1)).any(|(a, b)| a == b) {
                return Err(Error::Bad("duplicate snapshot identity"));
            }
            Ok(out)
        }
        let ws = sorted(
            self.workspaces.iter().map(|w| w.id),
            self.workspaces.len(),
            budget,
        )?;
        let tabs = sorted(self.tabs.iter().map(|t| t.id), self.tabs.len(), budget)?;
        let panes = sorted(self.panes.iter().map(|p| p.id), self.panes.len(), budget)?;
        let _clients = sorted(
            self.clients.iter().map(|c| c.id),
            self.clients.len(),
            budget,
        )?;
        let mut ordered_tabs = self.tabs.iter();
        for w in &self.workspaces {
            if w.tabs.is_empty() {
                return Err(bad());
            }
            for id in &w.tabs {
                if !ordered_tabs
                    .next()
                    .is_some_and(|t| t.id == *id && t.workspace == w.id)
                {
                    return Err(bad());
                }
            }
        }
        if ordered_tabs.next().is_some() {
            return Err(bad());
        }
        let mut ordered_panes = self.panes.iter();
        let mut leaves = budget.vector(panes.len()).map_err(|_| Error::Limit)?;
        for t in &self.tabs {
            if ws.binary_search(&t.workspace).is_err()
                || t.rows == 0
                || t.cols == 0
                || t.nodes.len() > NODES
                || t.root.checked_add(1) != Some(t.nodes.len())
            {
                return Err(bad());
            }
            let mut references = budget
                .vector::<u8>(t.nodes.len())
                .map_err(|_| Error::Limit)?;
            references.resize(t.nodes.len(), 0);
            for (at, node) in t.nodes.iter().enumerate() {
                match node {
                    Node::Leaf(pane) => {
                        budget.reserve(&mut leaves, 1).map_err(|_| Error::Limit)?;
                        leaves.push((*pane, t.id));
                        let omitted = matches!((self.scope, self.projection), (Scope::Target(AnyRef::Pane(id)), Projection::Facts) if id != *pane);
                        if !omitted
                            && !ordered_panes
                                .next()
                                .is_some_and(|p| p.id == *pane && p.tab == t.id)
                        {
                            return Err(bad());
                        }
                        if !matches!(
                            (self.scope, self.projection),
                            (Scope::Target(AnyRef::Pane(_)), Projection::Facts)
                        ) && panes.binary_search(pane).is_err()
                        {
                            return Err(bad());
                        }
                    }
                    Node::Split { first, second, .. } => {
                        for child in [first, second] {
                            if *child >= at {
                                return Err(bad());
                            }
                            let n = references.get_mut(*child).ok_or_else(bad)?;
                            if *n != 0 {
                                return Err(bad());
                            }
                            *n = 1;
                        }
                    }
                }
            }
            if references.iter().take(t.root).any(|n| *n != 1) {
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
                || tabs.binary_search(&p.tab).is_err()
                || leaves.binary_search_by_key(&p.id, |(id, _)| *id).is_err()
                || p.state.is_some() != (self.projection == Projection::Facts)
                || p.recipe.is_some() != (self.projection == Projection::Recipe)
            {
                return Err(bad());
            }
        }
        let owners = sorted(
            self.tabs.iter().map(|t| (t.id, t.workspace)),
            self.tabs.len(),
            budget,
        )?;
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
            if c.workspace.is_some_and(|w| ws.binary_search(&w).is_err())
                || c.tab.is_some_and(|t| tabs.binary_search(&t).is_err())
            {
                return Err(bad());
            }
        }
        Ok(())
    }
}

macro_rules! fields {
    ($($variant:ident $name:literal,)*) => {
        wire! {
            #[derive(Clone, Copy, Debug, PartialEq, Eq)]
            pub enum Field { $($variant,)* }
        }
        impl Field {
            pub fn named(name: &str) -> Option<Self> { match name { $($name => Some(Self::$variant),)* _ => None } }
            pub fn name(self) -> &'static str { match self { $(Self::$variant => $name,)* } }
        }
    };
}
fields! {
    Id "id",
    Name "name",
    Label "label",
    Rows "rows",
    Cols "cols",
    Pid "pid",
    Version "version",
    Panes "panes",
    Tabs "tabs",
    Title "title",
    Program "program",
    ForegroundGroup "foreground_group",
    ForegroundLeader "foreground_leader",
    LaunchArgv "launch_argv",
    HungUp "hung_up",
    ReadError "read_error",
    Exited "exited",
    Signal "signal",
    IdleMs "idle_ms",
    AltScreen "alt_screen",
    Mouse "mouse",
    MouseMode "mouse_mode",
    BracketedPaste "bracketed_paste",
    Pictures "pictures",
    Lost "lost",
    Cell "cell",
    CursorVisible "cursor_visible",
    HistoryBytes "history_bytes",
    ProgramClipboard "program_clipboard",
    HistoryLines "history_lines",
    CopyMode "copy_mode",
    CommandRunning "command_running",
    LastStatus "last_status",
    Bells "bells",
    Notification "notification",
    Truncated "truncated",
    InputUnread "input_unread",
    Cwd "cwd",
    Argv "argv",
}

impl Snapshot {
    pub(crate) fn heap_bytes(&self) -> Result<usize, fuxth::Failure> {
        let mut budget = crate::wire::Budget::with_limit(BYTES);
        let result = (|| {
            budget.slots::<Self>(1)?;
            budget.slots::<Workspace>(self.workspaces.capacity())?;
            budget.slots::<Tab>(self.tabs.capacity())?;
            budget.slots::<Pane>(self.panes.capacity())?;
            budget.slots::<Client>(self.clients.capacity())?;
            for w in &self.workspaces {
                budget.charge(
                    w.name
                        .as_ref()
                        .map_or(0, String::capacity)
                        .saturating_add(w.label.capacity()),
                )?;
                budget.slots::<TabId>(w.tabs.capacity())?;
            }
            for t in &self.tabs {
                budget.charge(
                    t.name
                        .as_ref()
                        .map_or(0, String::capacity)
                        .saturating_add(t.label.capacity()),
                )?;
                budget.slots::<Node>(t.nodes.capacity())?;
            }
            for p in &self.panes {
                budget.charge(
                    p.name
                        .as_ref()
                        .map_or(0, String::capacity)
                        .saturating_add(p.label.capacity())
                        .saturating_add(p.title.capacity()),
                )?;
                if let Some(state) = &p.state {
                    state.charge(&mut budget)?;
                }
                if let Some(r) = &p.recipe {
                    budget.slots::<String>(r.argv.capacity())?;
                    for arg in &r.argv {
                        budget.charge(arg.capacity())?;
                    }
                    budget.slots::<(String, Option<Path>)>(r.env.capacity())?;
                    for (key, value) in &r.env {
                        budget.charge(key.capacity())?;
                        if let Some(value) = value {
                            budget.charge(value.bytes())?;
                        }
                    }
                    budget.charge(r.initial_cwd.bytes())?;
                    if let Directory::Known(path) = &r.observed_cwd {
                        budget.charge(path.bytes())?;
                    }
                }
            }
            for c in &self.clients {
                budget.charge(c.mode.capacity())?;
            }
            Ok::<_, crate::session::Error>(())
        })();
        result.map_err(|e| fuxth::Failure::new(fuxth::Code::TooLong, e.to_string()))?;
        Ok(BYTES.saturating_sub(budget.remaining()))
    }

    pub(crate) fn write_text(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        struct Lines<'a> {
            out: &'a mut dyn std::fmt::Write,
            first: bool,
        }
        impl Lines<'_> {
            fn line(&mut self, args: std::fmt::Arguments<'_>) -> std::fmt::Result {
                if !self.first {
                    self.out.write_char('\n')?;
                }
                self.first = false;
                self.out.write_fmt(args)
            }
        }
        let mut lines = Lines { out, first: true };
        let mut tabs = self.tabs.iter().peekable();
        let mut panes = self.panes.iter().peekable();
        let whole = matches!(
            self.scope,
            Scope::All | Scope::Client(_) | Scope::Target(AnyRef::Workspace(_))
        );
        for w in &self.workspaces {
            if whole {
                lines.line(format_args!(
                    "{}",
                    crate::words::Labelled(
                        crate::words::Label::Id(AnyRef::Workspace(w.id)),
                        crate::words::Label::Text(&w.label)
                    )
                ))?;
            }
            while let Some(t) = tabs.next_if(|t| t.workspace == w.id) {
                if !matches!(self.scope, Scope::Target(AnyRef::Pane(_))) {
                    lines.line(format_args!(
                        "{}{}",
                        if whole { "  " } else { "" },
                        crate::words::Labelled(
                            crate::words::Label::Id(AnyRef::Tab(t.id)),
                            crate::words::Label::Text(&t.label)
                        )
                    ))?;
                }
                while let Some(p) = panes.next_if(|p| p.tab == t.id) {
                    let indent = if whole {
                        "    "
                    } else if matches!(self.scope, Scope::Target(AnyRef::Tab(_))) {
                        "  "
                    } else {
                        ""
                    };
                    lines.line(format_args!(
                        "{indent}{} {}x{}",
                        crate::words::Labelled(
                            crate::words::Label::Id(AnyRef::Pane(p.id)),
                            crate::words::Label::Text(&p.label)
                        ),
                        p.cols,
                        p.rows
                    ))?;
                    if let Some(pid) = p.pid {
                        write!(lines.out, " pid {pid}")?;
                    } else if let Some(Json::Int(code)) =
                        p.state.as_ref().and_then(|s| s.get("exited"))
                    {
                        write!(lines.out, " exited {code}")?;
                    } else if let Some(Json::Int(signal)) =
                        p.state.as_ref().and_then(|s| s.get("signal"))
                    {
                        write!(
                            lines.out,
                            " {}",
                            crate::process::Told(crate::process::Ended::Signalled(
                                i32::try_from(*signal).map_err(|_| std::fmt::Error)?
                            ))
                        )?;
                    }
                }
            }
        }
        if matches!(self.scope, Scope::All | Scope::Client(_)) {
            for c in &self.clients {
                lines.line(format_args!("client {} {}x{} ", c.id, c.cols, c.rows))?;
                match c.workspace {
                    Some(w) => write!(lines.out, "{w}")?,
                    None => lines.out.write_char('-')?,
                }
                lines.out.write_char(' ')?;
                match c.tab {
                    Some(t) => write!(lines.out, "{t}")?,
                    None => lines.out.write_char('-')?,
                }
                lines.out.write_char(' ')?;
                match c.pane {
                    Some(p) => write!(lines.out, "{p}")?,
                    None => lines.out.write_char('-')?,
                }
            }
            lines.line(format_args!("events {}", self.events))?;
        }
        Ok(())
    }
}

impl Path {
    fn bytes(&self) -> usize {
        match self {
            Self::Utf8(s) => s.capacity(),
            Self::Bytes(b) => b.capacity(),
        }
    }
}

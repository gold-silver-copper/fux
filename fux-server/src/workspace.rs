//! Workspaces, their tabs and panes, and where each attached client is
//! among them. The server is a [`Stack`] of workspaces and a workspace a
//! stack of tabs and workspaces: a stack shows each client one of its
//! children, its own, and keeps the one it showed before. A tab keeps each
//! client's focus in a [`Seat`] the same way, and its layout's stacks each
//! client's choice there. A child's leaving repairs what each client is
//! shown, and a tab's layout changes only through [`Tab::edit`], which
//! keeps each focus on one of its panes, and shown: no client names a tab
//! or pane that is gone.
//!
//! [`Parts`] holds them with the journal of what happened to them: what
//! events tell is changed only here, by methods that journal the change
//! as they make it, and by a pane's own, which a [`PaneMut`] gives the
//! journal at the pane's place.
use crate::journal::{Change, Event, Journal, PaneEvent, ViewChange};
use crate::pane::{Pane, PaneMut};
use fux_wire::command::{AnyRef, Sibling};
use fux_wire::events::Actor;
use fux_wire::id::{ClientId, PaneId, TabId, WsId};
use fux_wire::layout::{self, NoPane, Rect, Removed, Tree};
use std::collections::{BTreeMap, BTreeSet};

pub use fux_wire::layout::Shown;

/// A stack's child: a workspace or a tab, by its id.
pub trait Child {
    type Id: Copy + Eq;
    fn id(&self) -> Self::Id;
}

/// Children shown one at a time: each attached client is shown its own,
/// while there is any.
pub struct Stack<C: Child> {
    children: Vec<C>,
    shown: BTreeMap<ClientId, Shown<C::Id>>,
}

impl<C: Child> Stack<C> {
    pub fn children(&self) -> &[C] {
        &self.children
    }

    pub fn get(&self, id: C::Id) -> Option<&C> {
        self.children.iter().find(|c| c.id() == id)
    }

    /// What `client` is shown here.
    pub fn shown(&self, client: ClientId) -> Option<Shown<C::Id>> {
        self.shown.get(&client).copied()
    }

    /// The child `client` is shown.
    pub fn current(&self, client: ClientId) -> Option<&C> {
        self.get(self.shown.get(&client)?.current)
    }

    /// Shows `client` child `id`, if it is one.
    fn show(&mut self, client: ClientId, id: C::Id) {
        let child = self.children.iter().any(|c| c.id() == id);
        if let Some(shown) = self.shown.get_mut(&client).filter(|_| child) {
            shown.show(id);
        }
    }

    /// Takes child `id` out: each client it was shown to is shown the one
    /// it was shown before, else its neighbour.
    fn remove(&mut self, id: C::Id) -> Option<C> {
        let was: Vec<C::Id> = self.children.iter().map(C::id).collect();
        let (gone, kept) = (std::mem::take(&mut self.children).into_iter())
            .partition::<Vec<C>, _>(|c| c.id() == id);
        self.children = kept;
        let child = gone.into_iter().next()?;
        for shown in self.shown.values_mut() {
            shown.keep(&was, |c| c != id, None);
        }
        self.shown.retain(|_, shown| shown.current != id);
        Some(child)
    }

    /// Swaps child `id` with its sibling `toward`: none if there is no such
    /// child, false if it has no sibling that way.
    fn reorder(&mut self, id: C::Id, toward: Sibling) -> Option<bool> {
        let index = self.children.iter().position(|c| c.id() == id)?;
        let first = match toward {
            Sibling::Next => Some(index),
            Sibling::Previous => index.checked_sub(1),
        };
        let pair = first.and_then(|i| self.children.get_mut(i..i.checked_add(2)?));
        Some(pair.map(<[C]>::reverse).is_some())
    }
}

/// A workspace's child: one of its tabs, or a workspace in it.
pub enum Node {
    Tab(Tab),
    Workspace(Workspace),
}

impl Child for Node {
    type Id = AnyRef;
    fn id(&self) -> AnyRef {
        match self {
            Node::Tab(tab) => AnyRef::Tab(tab.id),
            Node::Workspace(ws) => AnyRef::Workspace(ws.id),
        }
    }
}

/// A workspace and its children, of which it always has one or more: each
/// is made with one, and closes as its last leaves.
pub struct Workspace {
    /// Given as it is made, and never changed: its events' place.
    id: WsId,
    name: Option<String>,
    children: Stack<Node>,
}

impl Child for Workspace {
    type Id = WsId;
    fn id(&self) -> WsId {
        self.id
    }
}

pub struct Tab {
    /// Given as it is made, and never changed: its events' place.
    id: TabId,
    name: Option<String>,
    /// Its panes, of which it has one or more: a tab is made with one, and
    /// closes as its last leaves.
    root: Tree,
    /// Exhaustion invalidates guarded edits permanently rather than reusing a revision.
    revision: Option<u64>,
    /// Its layout changed since its panes were sized for it: one no client
    /// shows is then laid out again (`Session::size_panes`).
    changed: bool,
    fit: Fit,
    /// The pane that fills it on every screen showing it, if one does:
    /// each client showing it focuses that pane, and its own focus comes
    /// back when it no longer does.
    zoomed: Option<PaneId>,
    seats: BTreeMap<ClientId, Seat>,
    /// The client that last typed into it, which hosts its panes while it
    /// shows it (`Session::host`).
    pub(crate) typist: Option<ClientId>,
}

impl Child for Tab {
    type Id = TabId;
    fn id(&self) -> TabId {
        self.id
    }
}

/// What a tab no client shows is laid out on: a size every tab has, given
/// or a client's, never made up. A client that shows it lays it out on its
/// own screen, whichever this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// The screens of the clients showing it: while none does, the last
    /// it was laid out on (the smallest, of several) that showed every
    /// pane, or the one it was made on.
    Clients(fux_vt::Size),
    /// A screen of this size, while no client sizes the tab: `size` and
    /// `resize`.
    Fixed(fux_vt::Size),
}

impl Fit {
    /// The size it lays a tab out in while no client shows it.
    pub fn size(self) -> fux_vt::Size {
        match self {
            Fit::Clients(size) | Fit::Fixed(size) => size,
        }
    }
    /// That size, as an area.
    pub fn area(self) -> Rect {
        Rect::screen(self.size().rows(), self.size().cols())
    }
}

/// A client's place in a tab: every attached client has one in every tab.
#[derive(Clone, Copy)]
pub struct Seat {
    /// The pane it focuses, one of the tab's, and the one before.
    pub focus: Shown<PaneId>,
    /// The bell rang here while it was shown another tab, marked in its bar
    /// until it shows this one (`Session::ring`).
    pub(crate) rang: bool,
    /// A mouse button was pressed in its focus and not yet released: the
    /// motion and release go there (`Session::mouse`).
    pub(crate) held: bool,
}

impl Seat {
    /// A seat focusing `pane`, new.
    fn on(focus: PaneId) -> Seat {
        Seat {
            focus: Shown::on(focus),
            rang: false,
            held: false,
        }
    }
}

impl Workspace {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Its given name, or its id for display.
    pub fn label(&self) -> std::borrow::Cow<'_, str> {
        self.display_label().into()
    }

    pub(crate) fn display_label(&self) -> fux_wire::words::Label<'_> {
        self.name().map_or(
            fux_wire::words::Label::Id(AnyRef::Workspace(self.id)),
            fux_wire::words::Label::Text,
        )
    }

    /// Its tabs, not those of the workspaces in it.
    pub fn tabs(&self) -> impl Iterator<Item = &Tab> {
        (self.children.children.iter()).filter_map(|child| match child {
            Node::Tab(tab) => Some(tab),
            Node::Workspace(_) => None,
        })
    }

    /// The workspaces in it, not those in them.
    pub fn workspaces(&self) -> impl Iterator<Item = &Workspace> {
        (self.children.children.iter()).filter_map(|child| match child {
            Node::Workspace(ws) => Some(ws),
            Node::Tab(_) => None,
        })
    }

    /// Its tabs and workspaces, and what each client is shown of them.
    pub fn stack(&self) -> &Stack<Node> {
        &self.children
    }

    /// The tab `client` is shown here, if it is shown one of its own.
    pub fn tab_of(&self, client: ClientId) -> Option<&Tab> {
        match self.children.current(client)? {
            Node::Tab(tab) => Some(tab),
            Node::Workspace(_) => None,
        }
    }

    /// It, or workspace `id` in it at any depth.
    fn find(&self, id: WsId) -> Option<&Workspace> {
        match self.id == id {
            true => Some(self),
            false => self.workspaces().find_map(|ws| ws.find(id)),
        }
    }

    /// The first tab in it at any depth, in order, that `found` takes, and
    /// its workspace.
    fn find_tab(&self, found: &mut impl FnMut(&Tab) -> bool) -> Option<(WsId, &Tab)> {
        (self.children.children.iter()).find_map(|child| match child {
            Node::Tab(tab) => found(tab).then_some((self.id, tab)),
            Node::Workspace(ws) => ws.find_tab(found),
        })
    }

    /// Pushes the workspaces from it down to `id` on `path`, if `id` is it
    /// or in it: whether it is.
    fn path_to(&self, id: WsId, path: &mut Vec<WsId>) -> bool {
        path.push(self.id);
        let found = self.id == id || self.workspaces().any(|ws| ws.path_to(id, path));
        if !found {
            path.pop();
        }
        found
    }

    /// `visit`s each tab in it at any depth, in order, with its workspace.
    fn each_tab<'a>(&'a self, visit: &mut impl FnMut(WsId, &'a Tab)) {
        for child in &self.children.children {
            match child {
                Node::Tab(tab) => visit(self.id, tab),
                Node::Workspace(ws) => ws.each_tab(visit),
            }
        }
    }

    /// `visit`s it and each workspace in it, each before those in it.
    fn each<'a>(&'a self, visit: &mut impl FnMut(&'a Workspace)) {
        visit(self);
        self.workspaces().for_each(|ws| ws.each(visit));
    }

    /// `each`, to change.
    fn each_mut(&mut self, visit: &mut impl FnMut(&mut Workspace)) {
        visit(self);
        for child in &mut self.children.children {
            if let Node::Workspace(ws) = child {
                ws.each_mut(visit);
            }
        }
    }

    /// Its tabs, to change, not those of the workspaces in it.
    fn tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        (self.children.children.iter_mut()).filter_map(|child| match child {
            Node::Tab(tab) => Some(tab),
            Node::Workspace(_) => None,
        })
    }
}

impl Tab {
    /// A tab holding `pane` alone, laid out on `fit`, with a seat for each
    /// of `clients`.
    fn new(
        id: TabId,
        name: Option<String>,
        fit: Fit,
        clients: impl Iterator<Item = ClientId>,
        pane: PaneId,
    ) -> Tab {
        Tab {
            id,
            name,
            root: Tree::Pane(pane),
            revision: Some(0),
            changed: true,
            fit,
            zoomed: None,
            seats: clients.map(|c| (c, Seat::on(pane))).collect(),
            typist: None,
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Its given name, or its first pane's label for display.
    pub fn label<'a>(&'a self, panes: &'a BTreeMap<PaneId, Pane>) -> std::borrow::Cow<'a, str> {
        self.display_label(panes).into()
    }

    pub(crate) fn display_label<'a>(
        &'a self,
        panes: &'a BTreeMap<PaneId, Pane>,
    ) -> fux_wire::words::Label<'a> {
        if let Some(name) = self.name() {
            return fux_wire::words::Label::Text(name);
        }
        panes.get(&self.root.first_pane()).map_or(
            fux_wire::words::Label::Id(fux_wire::command::AnyRef::Tab(self.id)),
            Pane::display_label,
        )
    }

    pub fn root(&self) -> &Tree {
        &self.root
    }

    pub fn revision(
        &self,
        run: fux_wire::command::Run,
    ) -> Option<fux_wire::command::LayoutRevision> {
        Some(fux_wire::command::LayoutRevision {
            tab: self.id,
            serial: self.revision?,
            run,
        })
    }

    fn revised(&mut self) {
        self.revision = self.revision.and_then(|n| n.checked_add(1));
    }

    pub fn fit(&self) -> Fit {
        self.fit
    }

    /// Lays it out on what `fit` says from now on.
    fn set_fit(&mut self, fit: Fit) {
        if self.fit != fit {
            self.revised();
        }
        self.fit = fit;
        self.changed = true;
    }

    /// Clients showing it lay it out on `size`, the smallest of their
    /// screens: laid out on clients' screens, that is its size while none
    /// does, if it shows every pane. A screen too small to is not: the tab
    /// keeps the last size that showed them all, which a dump makes it at.
    pub(crate) fn shown_on(&mut self, size: fux_vt::Size) {
        let area = Fit::Clients(size).area();
        let shows_all = layout::shows_all(&self.root, area);
        if let (Fit::Clients(last), true) = (&mut self.fit, shows_all)
            && *last != size
        {
            *last = size;
            self.revised();
        }
    }

    pub fn holds(&self, pane: PaneId) -> bool {
        self.root.contains(pane)
    }

    pub fn seat(&self, client: ClientId) -> Option<&Seat> {
        self.seats.get(&client)
    }

    pub(crate) fn seat_mut(&mut self, client: ClientId) -> Option<&mut Seat> {
        self.seats.get_mut(&client)
    }

    /// The pane `client` focuses here: the zoomed one while there is one.
    pub fn focus(&self, client: ClientId) -> Option<PaneId> {
        Some(self.zoomed.unwrap_or(self.seat(client)?.focus.current))
    }

    pub fn zoomed(&self) -> Option<PaneId> {
        self.zoomed
    }

    /// Zooms `pane`, one of the tab's, or none, journaled if that changes
    /// it. A button held in a pane a client no longer focuses is let go.
    fn zoom(&mut self, journal: &mut Journal, ws: WsId, pane: Option<PaneId>) {
        if self.zoomed != pane {
            for seat in self.seats.values_mut() {
                let focus = |zoomed: Option<PaneId>| zoomed.unwrap_or(seat.focus.current);
                seat.held &= focus(self.zoomed) == focus(pane);
            }
            self.zoomed = pane;
            self.changed = true;
            journal.push(Event::Zoomed((ws, self.id), pane));
        }
    }

    /// Focuses `pane` for `client`, if the tab holds it, remembering the
    /// pane it replaces. Another pane than the zoomed one ends the zoom.
    fn set_focus(&mut self, journal: &mut Journal, ws: WsId, client: ClientId, pane: PaneId) {
        if !self.holds(pane) {
            return;
        }
        if self.zoomed.is_some_and(|z| z != pane) {
            self.zoom(journal, ws, None);
        }
        if let Some(seat) = self.seats.get_mut(&client)
            && seat.focus.show(pane)
        {
            seat.held = false;
        }
        self.root.reveal(client, pane);
    }

    /// Changes the layout with `change`; each client then focuses the pane
    /// it did if the tab still holds it, else of those it is shown the one
    /// before, else its neighbour.
    fn edit<R>(
        &mut self,
        journal: &mut Journal,
        ws: WsId,
        change: impl FnOnce(&mut Tree) -> R,
    ) -> R {
        let was = self.root.panes();
        let out = change(&mut self.root);
        self.revised();
        self.changed = true;
        if self.zoomed.is_some_and(|z| !self.root.contains(z)) {
            self.zoom(journal, ws, None);
        }
        let root = &mut self.root;
        for (client, seat) in &mut self.seats {
            // A focus gone goes among what the client is shown, its stacks
            // repaired first: its last pane, else the neighbour, else the
            // first.
            let moved = match root.contains(seat.focus.current) {
                true => seat.focus.keep(&was, |p| root.contains(p), None),
                false => {
                    let shown: BTreeSet<PaneId> = root.shown_panes(*client).into_iter().collect();
                    let first = Some(root.first_shown(*client));
                    seat.focus.keep(&was, |p| shown.contains(&p), first)
                }
            };
            seat.held &= !moved;
            root.reveal(*client, seat.focus.current);
        }
        out
    }
}

/// The session's workspaces, their tabs and panes, and the journal of what
/// happened to them: what events tell of them changes only by its methods,
/// each of which journals what it changes as it changes it.
pub struct Parts {
    workspaces: Stack<Workspace>,
    /// Each in one tab: a pane is added in its place and leaves with it.
    panes: BTreeMap<PaneId, Pane>,
    journal: Journal,
    /// Panes given input since the last sweep; at most one entry per pane.
    input: BTreeSet<PaneId>,
}

/// A split planned (`Parts::plan_split`): the layout tab `tab` had, and
/// the one it takes, holding the new pane too.
pub struct Split {
    tab: TabId,
    was: Tree,
    root: Tree,
}

impl Split {
    pub fn tab(&self) -> TabId {
        self.tab
    }
    pub fn root(&self) -> &Tree {
        &self.root
    }
}

/// Where a pane moves to (`Parts::relocate`).
pub enum Destination {
    /// The end of tab `0`'s layout (`Tree::append`).
    Tab(TabId),
    /// A new unnamed tab of workspace `0`, laid out on `2`.
    NewTab(WsId, TabId, Fit),
    /// A new workspace at the top with an optional name, and its first tab
    /// and fit.
    NewWorkspace((WsId, Option<String>), (TabId, Option<String>, Fit)),
}

/// The workspaces from the top of `workspaces` down to `id`, none if it
/// is not there.
fn path(workspaces: &Stack<Workspace>, id: WsId) -> Vec<WsId> {
    let mut path = Vec::new();
    (workspaces.children.iter()).any(|ws| ws.path_to(id, &mut path));
    path
}

/// Workspace `id` of `workspaces`, at any depth, to change.
fn ws_mut(workspaces: &mut Stack<Workspace>, id: WsId) -> Option<&mut Workspace> {
    fn find(ws: &mut Workspace, id: WsId) -> Option<&mut Workspace> {
        if ws.id == id {
            return Some(ws);
        }
        (ws.children.children.iter_mut()).find_map(|child| match child {
            Node::Workspace(inner) => find(inner, id),
            Node::Tab(_) => None,
        })
    }
    workspaces.children.iter_mut().find_map(|ws| find(ws, id))
}

/// Tab `id` of `workspaces`, to change, and its workspace's id.
fn tab_mut(workspaces: &mut Stack<Workspace>, id: TabId) -> Option<(WsId, &mut Tab)> {
    fn find(ws: &mut Workspace, id: TabId) -> Option<(WsId, &mut Tab)> {
        let at = ws.id;
        (ws.children.children.iter_mut()).find_map(|child| match child {
            Node::Tab(tab) => (tab.id == id).then_some((at, tab)),
            Node::Workspace(inner) => find(inner, id),
        })
    }
    workspaces.children.iter_mut().find_map(|ws| find(ws, id))
}

/// Names `old` `new`, journaled as `renamed` makes it if it is another
/// name.
fn rename(
    journal: &mut Journal,
    renamed: impl FnOnce(Change) -> Event,
    old: &mut Option<String>,
    new: String,
) {
    if old.as_ref() != Some(&new) {
        journal.push(renamed(Change::Renamed(new.clone())));
        *old = Some(new);
    }
}

/// A client's place, its current child in each node, and whether it is
/// shown each pane (`Parts::places`).
type Place = (
    Option<crate::journal::PaneAt>,
    BTreeMap<Option<AnyRef>, AnyRef>,
    BTreeMap<PaneId, bool>,
);

/// The tab `client` is shown, following what it is shown from the top,
/// and the workspace that holds it.
fn shown(workspaces: &Stack<Workspace>, client: ClientId) -> Option<(&Workspace, &Tab)> {
    let mut ws = workspaces.current(client)?;
    loop {
        match ws.children.current(client)? {
            Node::Workspace(inner) => ws = inner,
            Node::Tab(tab) => return Some((ws, tab)),
        }
    }
}

/// Where `client` is: the workspace holding the tab it is shown, the tab,
/// and the pane it focuses in it.
fn place_of(workspaces: &Stack<Workspace>, client: ClientId) -> Option<crate::journal::PaneAt> {
    let (ws, tab) = shown(workspaces, client)?;
    Some(((ws.id, tab.id), tab.focus(client)?))
}

impl Parts {
    /// Nothing yet, its journal keeping `journal` events.
    pub fn new(journal: fux_wire::command::JournalEntries) -> Parts {
        Parts {
            workspaces: Stack {
                children: Vec::new(),
                shown: BTreeMap::new(),
            },
            panes: BTreeMap::new(),
            journal: Journal::new(journal),
            input: BTreeSet::new(),
        }
    }

    /// The workspaces at the top.
    pub fn workspaces(&self) -> &[Workspace] {
        self.workspaces.children()
    }

    /// The workspaces at the top, and what each client is shown of them.
    pub fn stack(&self) -> &Stack<Workspace> {
        &self.workspaces
    }

    /// Every workspace, each before those in it.
    pub fn every_workspace(&self) -> Vec<&Workspace> {
        self.workspaces_in(None)
    }

    /// Workspace `ws` and those in it at any depth, or with none every
    /// workspace, each before those in it.
    pub fn workspaces_in(&self, ws: Option<WsId>) -> Vec<&Workspace> {
        let mut all = Vec::new();
        for ws in self.tops(ws) {
            ws.each(&mut |ws| all.push(ws));
        }
        all
    }

    /// The tabs in workspace `ws` at any depth, or with none every tab, in
    /// order, each with its workspace.
    pub fn tabs_in(&self, ws: Option<WsId>) -> Vec<(WsId, &Tab)> {
        let mut tabs = Vec::new();
        for ws in self.tops(ws) {
            ws.each_tab(&mut |ws, tab| tabs.push((ws, tab)));
        }
        tabs
    }

    /// Workspace `ws`, or with none each at the top.
    fn tops(&self, ws: Option<WsId>) -> impl Iterator<Item = &Workspace> {
        let top = self.workspaces().iter();
        top.filter_map(move |w| ws.map_or(Some(w), |id| w.find(id)))
    }

    /// Workspace `id`, at any depth.
    pub fn workspace(&self, id: WsId) -> Option<&Workspace> {
        self.workspaces().iter().find_map(|ws| ws.find(id))
    }

    /// Tab `id`, and its workspace.
    pub fn tab(&self, id: TabId) -> Option<(WsId, &Tab)> {
        self.find_tab(&mut |t| t.id == id)
    }

    fn find_tab(&self, found: &mut impl FnMut(&Tab) -> bool) -> Option<(WsId, &Tab)> {
        self.workspaces().iter().find_map(|ws| ws.find_tab(found))
    }

    /// The workspaces from the top down to `id`, none if it is not there.
    pub fn path(&self, id: WsId) -> Vec<WsId> {
        path(&self.workspaces, id)
    }

    /// The workspace workspace `id` is in, none at the top; none at all if
    /// it is not there.
    pub fn parent(&self, id: WsId) -> Option<Option<WsId>> {
        let path = self.path(id);
        let (_, above) = path.split_last()?;
        Some(above.last().copied())
    }

    /// Whether workspace `ws` is `of` or in it.
    pub fn within(&self, ws: WsId, of: WsId) -> bool {
        self.workspace(of).is_some_and(|of| of.find(ws).is_some())
    }

    /// What holds each from the top down to `target`, then it.
    fn path_of(&self, target: AnyRef) -> Vec<AnyRef> {
        let (ws, tail) = match target {
            AnyRef::Pane(pane) => match self.locate(pane) {
                Some((ws, tab)) => (ws, vec![AnyRef::Tab(tab), target]),
                None => return Vec::new(),
            },
            AnyRef::Tab(tab) => match self.tab(tab) {
                Some((ws, _)) => (ws, vec![target]),
                None => return Vec::new(),
            },
            AnyRef::Workspace(ws) => (ws, Vec::new()),
        };
        let path = self.path(ws);
        let tail = tail.into_iter().filter(|_| !path.is_empty());
        (path.iter().copied().map(AnyRef::Workspace))
            .chain(tail)
            .collect()
    }

    /// Where `client` is (`place_of`).
    pub fn place(&self, client: ClientId) -> Option<crate::journal::PaneAt> {
        place_of(&self.workspaces, client)
    }

    /// The tab `client` is shown, and the workspace that holds it.
    pub fn shown(&self, client: ClientId) -> Option<(&Workspace, &Tab)> {
        shown(&self.workspaces, client)
    }

    /// Tab `id`, to change what events do not tell: its bell marks, its
    /// fit. One tab at a time, and none is made but here, so none is put
    /// in another's place (`mem::swap`), its panes moved with no journal
    /// entry.
    pub(crate) fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        tab_mut(&mut self.workspaces, id).map(|(_, tab)| tab)
    }

    /// Changes every workspace with `change`, each before those in it.
    fn each_ws_mut(&mut self, mut change: impl FnMut(&mut Workspace)) {
        for ws in &mut self.workspaces.children {
            ws.each_mut(&mut change);
        }
    }

    /// What `client` is shown of the workspaces at the top, of each
    /// workspace's children and of each tab's panes, each workspace before
    /// what it holds.
    pub fn seats(&self, client: ClientId) -> Vec<fux_wire::inspect::Seat> {
        let seat = |within, shown: Option<Shown<AnyRef>>| {
            shown.map(|Shown { current, last }| fux_wire::inspect::Seat {
                within,
                current,
                last,
            })
        };
        let root = self
            .workspaces
            .shown(client)
            .map(|s| s.map(AnyRef::Workspace));
        let mut seats: Vec<_> = seat(None, root).into_iter().collect();
        for ws in self.every_workspace() {
            seats.extend(seat(
                Some(AnyRef::Workspace(ws.id)),
                ws.children.shown(client),
            ));
            for tab in ws.tabs() {
                let focus = tab.seat(client).map(|s| s.focus.map(AnyRef::Pane));
                seats.extend(seat(Some(AnyRef::Tab(tab.id)), focus));
            }
        }
        seats
    }

    /// Does `change`, journaling `shown` for each client whose place, or
    /// current child anywhere, it changes: at the place it left, if
    /// another, and at the one it is at (none, with the last workspace
    /// gone).
    fn viewing<R>(&mut self, change: impl FnOnce(&mut Parts) -> R) -> R {
        let (before, nesting) = (self.places(), self.journal.nesting());
        let out = change(self);
        self.moved(before, &nesting);
        out
    }

    /// Where each client is, its current child at every node, and what it
    /// is shown of each tab's panes.
    fn places(&self) -> BTreeMap<ClientId, Place> {
        let clients = self.workspaces.shown.keys().copied();
        let currents = |c| {
            (self.seats(c).iter())
                .map(|s| (s.within, s.current))
                .collect()
        };
        let tabs = self.tabs_in(None);
        let shown = |c| {
            let mut shown = BTreeMap::new();
            for (_, tab) in &tabs {
                let seen: BTreeSet<PaneId> = tab.root.shown_panes(c).into_iter().collect();
                tab.root
                    .for_each_pane(&mut |p| _ = shown.insert(p, seen.contains(&p)));
            }
            shown
        };
        let place = |c| (place_of(&self.workspaces, c), currents(c), shown(c));
        clients.map(|c| (c, place(c))).collect()
    }

    /// Journals `shown` for each client whose place, current child in a
    /// node there before and after (not one made or closed), or whether it
    /// is shown a pane there before and after, is not as `before`: the
    /// place left in the workspaces as they were nested then (`nesting`),
    /// the one reached as they are now, and one place that stays in both.
    fn moved(&mut self, before: BTreeMap<ClientId, Place>, nesting: &BTreeMap<WsId, WsId>) {
        let after = self.places();
        let clients: BTreeSet<ClientId> = before.keys().chain(after.keys()).copied().collect();
        let moved = |c: &ClientId| match (before.get(c), after.get(c)) {
            (Some((was, then, seen)), Some((now, currents, shown))) => {
                was != now
                    || (then.iter()).any(|(node, c)| currents.get(node).is_some_and(|n| n != c))
                    || (seen.iter()).any(|(pane, s)| shown.get(pane).is_some_and(|n| n != s))
            }
            (was, now) => was != now,
        };
        for client in clients.into_iter().filter(moved) {
            let place = |of: &BTreeMap<ClientId, Place>| of.get(&client).and_then(|p| p.0);
            let (was, now) = (place(&before), place(&after));
            let shown = |at| Event::View {
                client,
                at,
                change: ViewChange::Shown,
            };
            match was.filter(|was| Some(*was) != now) {
                Some(was) => {
                    self.journal.push_then(shown(Some(was)), nesting, false);
                    self.journal.push(shown(now));
                }
                None => self.journal.push_then(shown(now), nesting, true),
            }
        }
    }

    /// A client attaches, shown `at` and each that holds it, and elsewhere
    /// each first child, with nothing shown before.
    pub(crate) fn attach(&mut self, client: ClientId, at: AnyRef) {
        let path = self.path_of(at);
        let Some(AnyRef::Workspace(top)) = path.first().copied() else {
            return;
        };
        // The child of `node` on the path, if it is on it.
        let on = |node: AnyRef| {
            let index = path.iter().position(|n| *n == node)?;
            path.get(index.checked_add(1)?).copied()
        };
        self.viewing(|parts| {
            parts.workspaces.shown.insert(client, Shown::on(top));
            let Parts {
                workspaces,
                journal,
                ..
            } = parts;
            for ws in &mut workspaces.children {
                ws.each_mut(&mut |w| {
                    let first = w.children.children.first().map(Node::id);
                    if let Some(current) = on(AnyRef::Workspace(w.id)).or(first) {
                        w.children.shown.insert(client, Shown::on(current));
                    }
                    let id = w.id;
                    for t in w.tabs_mut() {
                        let focus = match on(AnyRef::Tab(t.id)) {
                            Some(AnyRef::Pane(pane)) => Some(pane),
                            Some(AnyRef::Tab(_) | AnyRef::Workspace(_)) | None => None,
                        };
                        let seat = Seat::on(focus.unwrap_or(t.root.first_pane()));
                        t.root.reveal(client, seat.focus.current);
                        t.seats.insert(client, seat);
                        // Focusing another pane than the zoomed one ends the zoom.
                        if let Some(focus) = focus.filter(|f| t.zoomed.is_some_and(|z| z != *f)) {
                            t.set_focus(journal, id, client, focus);
                        }
                    }
                });
            }
        });
    }

    /// Disabling bells clears only this client's pending tab marks.
    pub(crate) fn clear_bells(&mut self, client: ClientId) {
        self.each_ws_mut(|ws| {
            for tab in ws.tabs_mut() {
                if let Some(seat) = tab.seat_mut(client) {
                    seat.rang = false;
                }
            }
        });
    }

    /// A client detaches: nothing names it.
    pub(crate) fn forget(&mut self, client: ClientId) {
        self.workspaces.shown.remove(&client);
        self.each_ws_mut(|ws| {
            ws.children.shown.remove(&client);
            for tab in ws.tabs_mut() {
                tab.seats.remove(&client);
                tab.root.forget(client);
                tab.typist = tab.typist.filter(|t| *t != client);
            }
        });
    }

    /// Shows `client` `target`, a pane, tab or workspace, and each that
    /// holds it; nothing if it is not there.
    pub(crate) fn show(&mut self, client: ClientId, target: AnyRef) {
        self.viewing(|parts| parts.show_path(client, target));
    }

    fn show_path(&mut self, client: ClientId, target: AnyRef) {
        let path = self.path_of(target);
        if let Some(AnyRef::Workspace(top)) = path.first() {
            self.workspaces.show(client, *top);
        }
        for (&node, &child) in path.iter().zip(path.iter().skip(1)) {
            match (node, child) {
                (AnyRef::Workspace(ws), child) => {
                    if let Some(w) = ws_mut(&mut self.workspaces, ws) {
                        w.children.show(client, child);
                    }
                }
                (AnyRef::Tab(tab), AnyRef::Pane(pane)) => {
                    if let Some((ws, t)) = tab_mut(&mut self.workspaces, tab) {
                        t.set_focus(&mut self.journal, ws, client, pane);
                    }
                }
                (AnyRef::Tab(_), AnyRef::Tab(_) | AnyRef::Workspace(_)) | (AnyRef::Pane(_), _) => {}
            }
        }
    }

    /// Swaps `target`, a tab or workspace, with its sibling `toward` among
    /// what holds it: none if there is no such one, false if it has no
    /// sibling that way.
    pub(crate) fn reorder(&mut self, target: AnyRef, toward: Sibling) -> Option<bool> {
        let (event, changed) = match target {
            AnyRef::Tab(tab) => {
                let (ws, _) = self.tab(tab)?;
                let changed = ws_mut(&mut self.workspaces, ws)?
                    .children
                    .reorder(target, toward)?;
                (Event::Tab((ws, tab), Change::Reordered), changed)
            }
            AnyRef::Workspace(ws) => {
                let parent = self.parent(ws)?;
                let changed = match parent {
                    None => self.workspaces.reorder(ws, toward),
                    Some(parent) => ws_mut(&mut self.workspaces, parent)?
                        .children
                        .reorder(target, toward),
                }?;
                (Event::Workspace((parent, ws), Change::Reordered), changed)
            }
            AnyRef::Pane(_) => return None,
        };
        if changed {
            self.journal.push(event);
        }
        Some(changed)
    }

    pub(crate) fn typed(&mut self, id: TabId, client: ClientId, changed_host: bool) {
        if let Some((ws, tab)) = tab_mut(&mut self.workspaces, id) {
            tab.typist = Some(client);
            if changed_host {
                self.journal.push(Event::Layout((ws, id)));
            }
        }
    }

    pub(crate) fn shown_on(&mut self, id: TabId, size: fux_vt::Size) {
        if let Some((ws, tab)) = tab_mut(&mut self.workspaces, id) {
            let before = tab.fit;
            tab.shown_on(size);
            if before != tab.fit {
                self.journal.push(Event::Layout((ws, id)));
            }
        }
    }

    /// Zooms `pane` in its tab, or, if it is zoomed, ends the zoom.
    pub(crate) fn zoom(&mut self, pane: PaneId) -> Option<()> {
        let (ws, tab) = self.locate(pane)?;
        self.viewing(|parts| {
            let (_, tab) = tab_mut(&mut parts.workspaces, tab)?;
            let zoomed = (tab.zoomed != Some(pane)).then_some(pane);
            tab.zoom(&mut parts.journal, ws, zoomed);
            Some(())
        })
    }

    pub(crate) fn set_fit(&mut self, tab: TabId, fit: impl FnOnce(Fit) -> Fit) -> Option<()> {
        let (ws, tab) = tab_mut(&mut self.workspaces, tab)?;
        let fit = fit(tab.fit);
        if tab.fit != fit {
            tab.set_fit(fit);
            self.journal.push(Event::Layout((ws, tab.id)));
        }
        Some(())
    }

    /// The tabs no client sizes (`sized` says which do) whose layout
    /// changed since their panes were sized, for sizing them on their fit.
    /// Those are then taken as sized, as is a sized one, by its clients.
    pub(crate) fn changed_unshown(&mut self, sized: impl Fn(TabId) -> bool) -> Vec<TabId> {
        let mut changed = Vec::new();
        self.each_ws_mut(|ws| {
            for tab in ws.tabs_mut() {
                if std::mem::take(&mut tab.changed) && !sized(tab.id) {
                    changed.push(tab.id);
                }
            }
        });
        changed
    }

    pub fn panes(&self) -> &BTreeMap<PaneId, Pane> {
        &self.panes
    }

    pub(crate) fn journal_mut(&mut self) -> &mut Journal {
        &mut self.journal
    }

    pub(crate) fn actor(&mut self, actor: Actor) -> Actor {
        self.journal.actor(actor)
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// The workspace and tab that hold pane `pane`.
    pub fn locate(&self, pane: PaneId) -> Option<(WsId, TabId)> {
        (self.find_tab(&mut |t| t.holds(pane))).map(|(ws, t)| (ws, t.id))
    }

    /// Pane `id`, to change.
    pub fn pane_mut(&mut self, id: PaneId) -> Option<PaneMut<'_>> {
        let at = self.locate(id)?;
        Some(PaneMut::new(
            self.panes.get_mut(&id)?,
            at,
            &mut self.journal,
            &mut self.input,
        ))
    }

    /// Changes each pane with `change`, tab by tab: each at the place it
    /// is found, not looked for again.
    pub fn each_pane_mut(&mut self, mut change: impl FnMut(&mut PaneMut<'_>)) {
        let Parts {
            workspaces,
            panes,
            journal,
            input,
        } = self;
        for ws in &workspaces.children {
            ws.each(&mut |ws| {
                for tab in ws.tabs() {
                    for id in tab.root.panes() {
                        if let Some(pane) = panes.get_mut(&id) {
                            change(&mut PaneMut::new(pane, (ws.id, tab.id), journal, input));
                        }
                    }
                }
            });
        }
    }

    /// Writes only newly queued input. An unwritable remainder waits for
    /// PTY readiness, rather than being retried by every unrelated event.
    pub(crate) fn write_input(&mut self) -> bool {
        let mut wrote = false;
        while let Some(id) = self.input.pop_first() {
            if let Some(pane) = self.panes.get_mut(&id) {
                wrote |= pane.write_input();
            }
        }
        wrote
    }

    /// Adds workspaces `made`, each in the one before and the first in
    /// `parent` (none: at the top), the last holding one tab `first`,
    /// named and laid out as it says, holding `pane`, new; the pane given
    /// back if there is no such parent.
    #[expect(clippy::result_large_err, reason = "a pane given back")]
    pub(crate) fn add_workspaces(
        &mut self,
        parent: Option<WsId>,
        made: Vec<(WsId, Option<String>)>,
        first: (TabId, Option<String>, Fit),
        pane: Pane,
    ) -> Result<(), Pane> {
        let Some((at, _)) = made.last() else {
            return Err(pane);
        };
        let at = (*at, first.0);
        if self
            .push_workspaces(parent, made, first, pane.id())
            .is_none()
        {
            return Err(pane);
        }
        self.placed(at, pane);
        Ok(())
    }

    /// `add_workspaces`, holding pane `pane`, in the session: each client
    /// is shown the first child of each, and focuses the pane there.
    fn push_workspaces(
        &mut self,
        parent: Option<WsId>,
        made: Vec<(WsId, Option<String>)>,
        (tab, tab_name, fit): (TabId, Option<String>, Fit),
        pane: PaneId,
    ) -> Option<()> {
        let (last, _) = made.last()?;
        let last = *last;
        if parent.is_some_and(|p| self.workspace(p).is_none()) {
            return None;
        }
        let mut above = parent;
        for (id, name) in &made {
            self.journal.nest(*id, above);
            let created = Change::Created(name.clone());
            self.journal.push(Event::Workspace((above, *id), created));
            above = Some(*id);
        }
        self.journal
            .push(Event::Tab((last, tab), Change::Created(tab_name.clone())));
        let clients: Vec<ClientId> = self.workspaces.shown.keys().copied().collect();
        let stack = |child: Node| Stack {
            shown: (clients.iter())
                .map(|c| (*c, Shown::on(child.id())))
                .collect(),
            children: vec![child],
        };
        let mut node = Node::Tab(Tab::new(tab, tab_name, fit, clients.iter().copied(), pane));
        let mut made = made.into_iter().rev();
        let (id, name) = made.next()?;
        let mut ws = Workspace {
            id,
            name,
            children: stack(node),
        };
        for (id, name) in made {
            node = Node::Workspace(ws);
            ws = Workspace {
                id,
                name,
                children: stack(node),
            };
        }
        match parent {
            None => self.workspaces.children.push(ws),
            Some(parent) => {
                (ws_mut(&mut self.workspaces, parent)?.children.children).push(Node::Workspace(ws))
            }
        }
        Some(())
    }

    /// Adds tab `id` to workspace `ws`, with optional name `name`,
    /// laid out on `fit`, holding `pane`, new; the pane given back if
    /// there is no such workspace.
    #[expect(clippy::result_large_err, reason = "a pane given back")]
    pub(crate) fn add_tab(
        &mut self,
        (ws, id): (WsId, TabId),
        name: Option<String>,
        fit: Fit,
        pane: Pane,
    ) -> Result<(), Pane> {
        if self.push_tab((ws, id), name, fit, pane.id()).is_none() {
            return Err(pane);
        }
        self.placed((ws, id), pane);
        Ok(())
    }

    /// `add_tab`, holding pane `pane`, in the session; `None` if there is
    /// no workspace `ws`.
    fn push_tab(
        &mut self,
        (ws, id): (WsId, TabId),
        name: Option<String>,
        fit: Fit,
        pane: PaneId,
    ) -> Option<()> {
        let clients: Vec<ClientId> = self.workspaces.shown.keys().copied().collect();
        let workspace = ws_mut(&mut self.workspaces, ws)?;
        let tab = Tab::new(id, name.clone(), fit, clients.into_iter(), pane);
        workspace.children.children.push(Node::Tab(tab));
        self.journal
            .push(Event::Tab((ws, id), Change::Created(name)));
        Some(())
    }

    /// `pane`, new, is in tab `at` of its workspace.
    fn placed(&mut self, at: (WsId, TabId), pane: Pane) {
        let id = pane.id();
        self.journal.push(Event::Pane((at, id), PaneEvent::Created));
        self.panes.insert(id, pane);
    }

    /// The layout `target`'s tab takes with a new pane `add`ed beside it
    /// (`Tree::split`, `Tree::stack`): what the new pane starts at and is
    /// put in by (`split`).
    pub(crate) fn plan_split(
        &self,
        target: PaneId,
        add: impl FnOnce(&mut Tree) -> Result<(), NoPane>,
    ) -> Result<Split, NoPane> {
        let (_, tab) = self.locate(target).ok_or(NoPane)?;
        let was = self.tab(tab).map(|(_, t)| t.root.clone()).ok_or(NoPane)?;
        let mut root = was.clone();
        add(&mut root)?;
        Ok(Split { tab, was, root })
    }

    /// Puts `pane`, new, in its tab laid out as `split` planned: the pane
    /// given back if it is not the one planned for, or the tab's layout
    /// changed since.
    #[expect(clippy::result_large_err, reason = "a pane given back")]
    pub(crate) fn split(&mut self, pane: Pane, split: Split) -> Result<(), Pane> {
        let Split { tab, was, root } = split;
        let planned = root.contains(pane.id()) && !was.contains(pane.id());
        let Some((ws, t)) =
            tab_mut(&mut self.workspaces, tab).filter(|(_, t)| planned && t.root == was)
        else {
            return Err(pane);
        };
        t.edit(&mut self.journal, ws, |r| *r = root);
        self.placed((ws, tab), pane);
        Ok(())
    }

    /// Takes pane `id` out of its tab: given back, closed. A tab its going
    /// empties closes too, as does each workspace that leaves empty.
    pub(crate) fn close_pane(&mut self, id: PaneId) -> Option<Pane> {
        let at = self.locate(id)?;
        self.viewing(|parts| {
            let (_, tab) = tab_mut(&mut parts.workspaces, at.1)?;
            let emptied = match tab.root.remove(id) {
                Removed::Left(rest) => {
                    tab.edit(&mut parts.journal, at.0, |root| *root = rest);
                    false
                }
                Removed::Emptied => true,
                Removed::Absent => return None,
            };
            parts.journal.push(Event::Pane((at, id), PaneEvent::Closed));
            if emptied {
                parts.drop_tab(at.1);
            }
            parts.input.remove(&id);
            parts.panes.remove(&id)
        })
    }

    /// Removes tab `tab` and gives back the panes in it, closed. Each
    /// workspace it leaves empty goes too.
    pub(crate) fn remove_tab(&mut self, tab: TabId) -> Vec<Pane> {
        let Some((ws, t)) = self.tab(tab) else {
            return Vec::new();
        };
        let panes = t.root.panes();
        self.viewing(|parts| {
            for &pane in &panes {
                parts.input.remove(&pane);
                (parts.journal).push(Event::Pane(((ws, tab), pane), PaneEvent::Closed));
            }
            parts.drop_tab(tab);
            (panes.into_iter())
                .filter_map(|pane| parts.panes.remove(&pane))
                .collect()
        })
    }

    /// Takes tab `tab` out of its workspace, and each workspace that leaves
    /// empty: each client shown one is shown the one it was shown before,
    /// else the neighbour (`Stack::remove`).
    fn drop_tab(&mut self, tab: TabId) {
        let Some((ws, _)) = self.tab(tab) else {
            return;
        };
        let Some(w) = ws_mut(&mut self.workspaces, ws) else {
            return;
        };
        w.children.remove(AnyRef::Tab(tab));
        let emptied = w.children.children.is_empty();
        self.journal.push(Event::Tab((ws, tab), Change::Closed));
        if emptied {
            self.drop_workspace(ws);
        }
    }

    /// Takes workspace `ws`, emptied, out of what holds it, closed, and so
    /// each that holds it that it leaves empty.
    fn drop_workspace(&mut self, ws: WsId) {
        let mut emptied = Some(ws);
        while let Some(ws) = emptied {
            let Some(parent) = self.parent(ws) else {
                return;
            };
            self.take_workspace(ws);
            self.journal
                .push(Event::Workspace((parent, ws), Change::Closed));
            self.journal.nest(ws, None);
            emptied = parent
                .filter(|p| (self.workspace(*p)).is_some_and(|w| w.children.children.is_empty()));
        }
    }

    /// Workspace `ws`, taken out of what holds it: each client shown it
    /// there is shown the one it was shown before, else the neighbour.
    fn take_workspace(&mut self, ws: WsId) -> Option<Workspace> {
        match self.parent(ws)? {
            None => self.workspaces.remove(ws),
            Some(parent) => match ws_mut(&mut self.workspaces, parent)?
                .children
                .remove(AnyRef::Workspace(ws))?
            {
                Node::Workspace(ws) => Some(ws),
                Node::Tab(_) => None,
            },
        }
    }

    /// Moves workspace `ws` into workspace `to`, last, or to the top with
    /// none: what held it lets go of it, closing if it held it alone.
    /// `None`, changing nothing, if either is not there, `to` is `ws` or in
    /// it, or holds it already.
    pub(crate) fn move_workspace(&mut self, ws: WsId, to: Option<WsId>) -> Option<()> {
        let from = self.parent(ws)?;
        let into = |to: WsId| self.workspace(to).is_some() && !self.within(to, ws);
        if from == to || to.is_some_and(|to| !into(to)) {
            return None;
        }
        self.viewing(|parts| {
            let moving = parts.take_workspace(ws)?;
            match to {
                None => parts.workspaces.children.push(moving),
                Some(to) => (ws_mut(&mut parts.workspaces, to)?.children.children)
                    .push(Node::Workspace(moving)),
            }
            parts.journal.nest(ws, to);
            (parts.journal).push(Event::Workspace((to, ws), Change::Moved(from)));
            if let Some(from) = from
                && (parts.workspace(from)).is_some_and(|w| w.children.children.is_empty())
            {
                parts.drop_workspace(from);
            }
            Some(())
        })
    }

    /// Closes everything: the panes given back, to end.
    pub(crate) fn close_all(&mut self) -> Vec<Pane> {
        let tabs: Vec<TabId> = (self.tabs_in(None).into_iter())
            .map(|(_, t)| t.id)
            .collect();
        tabs.into_iter()
            .flat_map(|tab| self.remove_tab(tab))
            .collect()
    }

    /// Names workspace `id` `name`, if there is one.
    pub(crate) fn rename_workspace(&mut self, id: WsId, name: String) -> Option<()> {
        let parent = self.parent(id)?;
        let ws = ws_mut(&mut self.workspaces, id)?;
        let renamed = |change| Event::Workspace((parent, id), change);
        rename(&mut self.journal, renamed, &mut ws.name, name);
        Some(())
    }

    /// Gives tab `id` the name `name`, if there is one.
    pub(crate) fn give_tab_name(&mut self, id: TabId, name: String) -> Option<()> {
        let (ws, tab) = tab_mut(&mut self.workspaces, id)?;
        let renamed = |change| Event::Tab((ws, id), change);
        rename(&mut self.journal, renamed, &mut tab.name, name);
        Some(())
    }

    /// Swaps two panes' places: in one tab, or each in the other's, which
    /// moves both.
    pub(crate) fn swap(&mut self, a: PaneId, b: PaneId) {
        if a == b {
            return;
        }
        let (Some(was_a), Some(was_b)) = (self.locate(a), self.locate(b)) else {
            return;
        };
        self.viewing(|parts| {
            // Only the tabs that hold them: another, laid out anew, would
            // lose what its panes have (a zoomed pane, its whole screen).
            for tab in [was_a.1, was_b.1].into_iter().collect::<BTreeSet<_>>() {
                if let Some((ws, tab)) = tab_mut(&mut parts.workspaces, tab) {
                    tab.edit(&mut parts.journal, ws, |root| root.swap(a, b));
                }
            }
            parts.journal.push(Event::Layout(was_a));
            if was_a != was_b {
                parts.journal.push(Event::Layout(was_b));
                (parts.journal).push(Event::Pane((was_b, a), PaneEvent::Moved(was_a)));
                (parts.journal).push(Event::Pane((was_a, b), PaneEvent::Moved(was_b)));
            }
        });
    }

    /// Moves `pane` from its tab to `to`, another: its tab lets go of it,
    /// closing if it held it alone, as each workspace does that leaves
    /// empty, and `to` takes it; no other tab changes. `None`, changing
    /// nothing, for a pane in no tab, its own tab, or a tab or workspace
    /// there is not.
    pub(crate) fn relocate(&mut self, pane: PaneId, to: Destination) -> Option<()> {
        let was = self.locate(pane)?;
        self.viewing(|parts| parts.relocated(pane, was, to))
    }

    fn relocated(&mut self, pane: PaneId, was: (WsId, TabId), to: Destination) -> Option<()> {
        let (_, from) = tab_mut(&mut self.workspaces, was.1)?;
        let rest = match from.root.remove(pane) {
            Removed::Left(rest) => Some(rest),
            Removed::Emptied => None,
            Removed::Absent => return None,
        };
        // Taken in before its tab lets go: the workspace a new tab goes in
        // may be the one that would close.
        let at = match to {
            Destination::Tab(to) => {
                let (ws, tab) = tab_mut(&mut self.workspaces, to).filter(|_| to != was.1)?;
                tab.edit(&mut self.journal, ws, |root| root.append(pane));
                (ws, to)
            }
            Destination::NewTab(ws, tab, fit) => {
                self.push_tab((ws, tab), None, fit, pane)?;
                (ws, tab)
            }
            Destination::NewWorkspace(ws, first) => {
                let at = (ws.0, first.0);
                self.push_workspaces(None, vec![ws], first, pane)?;
                at
            }
        };
        (self.journal).push(Event::Pane((at, pane), PaneEvent::Moved(was)));
        match (rest, tab_mut(&mut self.workspaces, was.1)) {
            (Some(rest), Some((ws, from))) => from.edit(&mut self.journal, ws, |root| *root = rest),
            (None, _) | (_, None) => self.drop_tab(was.1),
        }
        Some(())
    }

    /// Commits a complete validated replacement and one layout invalidation.
    /// Each client is shown its focus, and the first child of every other
    /// stack.
    pub(crate) fn arrange(&mut self, id: TabId, root: Tree) -> Option<()> {
        self.viewing(|parts| {
            let (ws, tab) = tab_mut(&mut parts.workspaces, id)?;
            tab.edit(&mut parts.journal, ws, |old| *old = root);
            parts.journal.push(Event::Layout((ws, id)));
            Some(())
        })
    }

    /// Changes the layout of `pane`'s tab with `change`, which keeps its
    /// panes: moves one, moves a border, or lays them out anew. A change
    /// that would not keep them is undone, and is `None`, as is a pane in
    /// no tab.
    pub(crate) fn reshape<R>(
        &mut self,
        pane: PaneId,
        change: impl FnOnce(&mut Tree) -> R,
    ) -> Option<R> {
        let (ws, id) = self.locate(pane)?;
        self.viewing(|parts| {
            let (_, tab) = tab_mut(&mut parts.workspaces, id)?;
            let mut root = tab.root.clone();
            let out = change(&mut root);
            if !root.same_panes(&tab.root) {
                return None;
            }
            if root != tab.root {
                tab.edit(&mut parts.journal, ws, |old| *old = root);
                parts.journal.push(Event::Layout((ws, id)));
            }
            Some(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testing::{run, started};
    use std::time::Instant;

    #[test]
    fn only_new_input_schedules_a_pane_for_the_sweep() -> Result<(), String> {
        let mut session = started()?;
        run(&mut session, "%1 str horizontal axis split drop")?;
        let one = PaneId::of(1);
        let two = PaneId::of(2);
        let now = Instant::now();
        session.output(one, b"\x1b[6n", now);
        assert_eq!(session.parts.input, BTreeSet::from([one]));
        assert!(!session.parts.write_input(), "no processes in this session");
        session.parts.each_pane_mut(|p| p.keeps_size());
        session.output(two, b"plain output", now);
        assert!(session.parts.input.is_empty(), "old input is left to poll");
        session
            .parts
            .pane_mut(two)
            .ok_or("pane")?
            .push_with(|_, bytes| bytes.extend_from_slice(b"key"));
        assert_eq!(session.parts.input, BTreeSet::from([two]));
        session.parts.close_pane(two).ok_or("pane")?;
        assert!(
            session.parts.input.is_empty(),
            "closed panes leave no entries"
        );
        Ok(())
    }
}

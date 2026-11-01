//! Workspaces, their tabs and panes, and where each attached client is
//! among them. The server is a [`Stack`] of workspaces and a workspace a
//! stack of tabs: a stack shows each client one of its children, its own,
//! and keeps the one it showed before. A tab keeps each client's focus in a
//! [`Seat`] the same way. A child's leaving repairs what each client is
//! shown, and a tab's layout changes only through [`Tab::edit`], which
//! keeps each focus on one of its panes: no client names a tab or pane that
//! is gone.
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
use fux_wire::layout::{self, Axis, NoPane, Rect, Removed, Side, Tree};
use std::collections::{BTreeMap, BTreeSet};

/// What a client is shown of a stack, or focuses in a tab: its current
/// child, and the one before it, another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown<I> {
    pub current: I,
    pub last: Option<I>,
}

impl<I: Copy + Eq> Shown<I> {
    fn on(current: I) -> Shown<I> {
        Shown {
            current,
            last: None,
        }
    }

    pub fn map<J>(self, f: impl Fn(I) -> J) -> Shown<J> {
        Shown {
            current: f(self.current),
            last: self.last.map(f),
        }
    }

    /// Shows `id`, the current one becoming the last: whether it changed.
    fn show(&mut self, id: I) -> bool {
        let changed = self.current != id;
        if changed {
            self.last = Some(std::mem::replace(&mut self.current, id));
        }
        changed
    }

    /// Of the children `was` before a change, those `kept` are left, and
    /// `first` heads them all: a current one gone becomes the last, else
    /// its neighbour (the next one left, else the one before), else
    /// `first`. Whether the current one changed.
    fn keep(&mut self, was: &[I], kept: impl Fn(I) -> bool, first: Option<I>) -> bool {
        self.last = self.last.filter(|last| kept(*last));
        if kept(self.current) {
            return false;
        }
        let at = was.iter().position(|c| *c == self.current).unwrap_or(0);
        let (before, after) = was.split_at(at);
        let mut near = after.iter().chain(before.iter().rev()).copied();
        let next = self.last.take().or_else(|| near.find(|c| kept(*c)));
        match next.or(first) {
            Some(next) => {
                self.current = next;
                true
            }
            None => false,
        }
    }
}

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
    /// `first` alone, shown to each of `clients`.
    fn of(first: C, clients: impl Iterator<Item = ClientId>) -> Stack<C> {
        let shown = clients.map(|c| (c, Shown::on(first.id()))).collect();
        Stack {
            children: vec![first],
            shown,
        }
    }

    pub fn children(&self) -> &[C] {
        &self.children
    }

    pub fn get(&self, id: C::Id) -> Option<&C> {
        self.children.iter().find(|c| c.id() == id)
    }

    fn get_mut(&mut self, id: C::Id) -> Option<&mut C> {
        self.children.iter_mut().find(|c| c.id() == id)
    }

    /// What `client` is shown here.
    pub fn shown(&self, client: ClientId) -> Option<Shown<C::Id>> {
        self.shown.get(&client).copied()
    }

    /// The child `client` is shown.
    pub fn current(&self, client: ClientId) -> Option<&C> {
        self.get(self.shown.get(&client)?.current)
    }

    /// The clients shown something here.
    fn clients(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.shown.keys().copied()
    }

    /// `client` is shown the first child, unless it is shown one.
    fn attach(&mut self, client: ClientId) {
        if let Some(first) = self.children.first().map(C::id) {
            self.shown.entry(client).or_insert(Shown::on(first));
        }
    }

    fn forget(&mut self, client: ClientId) {
        self.shown.remove(&client);
    }

    /// Shows `client` child `id`, if it is one.
    fn show(&mut self, client: ClientId, id: C::Id) {
        let child = self.children.iter().any(|c| c.id() == id);
        if let Some(shown) = self.shown.get_mut(&client).filter(|_| child) {
            shown.show(id);
        }
    }

    fn push(&mut self, child: C) {
        self.children.push(child);
    }

    /// Takes child `id` out: each client it was shown to is shown the one
    /// it was shown before, else its neighbour.
    fn remove(&mut self, id: C::Id) -> Option<C> {
        let was: Vec<C::Id> = self.children.iter().map(C::id).collect();
        let index = was.iter().position(|c| *c == id)?;
        let child = self.children.remove(index);
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

/// A workspace and its tabs, of which it always has one or more: each is
/// made with one, and the session removes a workspace its last tab leaves.
pub struct Workspace {
    /// Given as it is made, and never changed: its events' place.
    id: WsId,
    name: Option<String>,
    tabs: Stack<Tab>,
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
    pub fn id(&self) -> WsId {
        self.id
    }

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

    pub fn tabs(&self) -> &[Tab] {
        self.tabs.children()
    }

    /// Its tabs, and what each client is shown of them.
    pub fn stack(&self) -> &Stack<Tab> {
        &self.tabs
    }

    /// The tab `client` is shown here.
    pub fn tab_of(&self, client: ClientId) -> Option<&Tab> {
        self.tabs.current(client)
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

    pub fn id(&self) -> TabId {
        self.id
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
    }

    /// Changes the layout with `change`; each client then focuses the pane
    /// it did if the tab still holds it, else the one before, else its
    /// neighbour.
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
        let root = &self.root;
        for seat in self.seats.values_mut() {
            let moved = seat.focus.keep(&was, |p| root.contains(p), Some(root.first_pane()));
            seat.held &= !moved;
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
    /// A new workspace with an optional name, and its first tab and fit.
    NewWorkspace((WsId, Option<String>), (TabId, Option<String>, Fit)),
}

/// Tab `id` of `workspaces`, and its workspace's id.
fn tab_mut(workspaces: &mut Stack<Workspace>, id: TabId) -> Option<(WsId, &mut Tab)> {
    let mut tabs = (workspaces.children.iter_mut())
        .flat_map(|w| w.tabs.children.iter_mut().map(|t| (w.id, t)));
    tabs.find(|(_, t)| t.id == id)
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

/// Where `client` is: the workspace it is shown, the tab it is shown there
/// and the pane it focuses in it.
fn place_of(workspaces: &Stack<Workspace>, client: ClientId) -> Option<crate::journal::PaneAt> {
    let ws = workspaces.current(client)?;
    let tab = ws.tab_of(client)?;
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

    pub fn workspaces(&self) -> &[Workspace] {
        self.workspaces.children()
    }

    /// The workspaces, and what each client is shown of them.
    pub fn stack(&self) -> &Stack<Workspace> {
        &self.workspaces
    }

    /// Where `client` is (`place_of`).
    pub fn place(&self, client: ClientId) -> Option<crate::journal::PaneAt> {
        place_of(&self.workspaces, client)
    }

    /// The children of what holds `of`, in order, and what `client` is
    /// shown of them: a tab's panes, a workspace's tabs, or the
    /// workspaces.
    pub fn siblings(
        &self,
        of: AnyRef,
        client: ClientId,
    ) -> Option<(Vec<AnyRef>, Option<Shown<AnyRef>>)> {
        fn among<C: Child>(
            stack: &Stack<C>,
            client: ClientId,
            id: impl Fn(C::Id) -> AnyRef,
        ) -> (Vec<AnyRef>, Option<Shown<AnyRef>>) {
            let children = stack.children.iter().map(|c| id(c.id())).collect();
            (children, stack.shown(client).map(|s| s.map(id)))
        }
        Some(match of {
            AnyRef::Pane(pane) => {
                let (ws, tab) = self.locate(pane)?;
                let tab = self.workspaces.get(ws)?.tabs.get(tab)?;
                let panes = tab.root.panes().into_iter().map(AnyRef::Pane).collect();
                let seat = tab.seat(client).map(|s| s.focus.map(AnyRef::Pane));
                (panes, seat)
            }
            AnyRef::Tab(tab) => {
                let ws = self.workspaces.children.iter().find(|w| w.tabs.get(tab).is_some())?;
                among(&ws.tabs, client, AnyRef::Tab)
            }
            AnyRef::Workspace(_) => among(&self.workspaces, client, AnyRef::Workspace),
        })
    }

    /// Tab `id`, to change what events do not tell: its bell marks, its
    /// fit. One tab at a time, and none is made but here, so none is put
    /// in another's place (`mem::swap`), its panes moved with no journal
    /// entry.
    pub(crate) fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        tab_mut(&mut self.workspaces, id).map(|(_, tab)| tab)
    }

    /// Does `change`, journaling `shown` for each client whose place it
    /// changes: at the place it left, if any, and at the one it reached
    /// (none, with the last workspace gone).
    fn viewing<R>(&mut self, change: impl FnOnce(&mut Parts) -> R) -> R {
        let places = |parts: &Parts| -> BTreeMap<ClientId, _> {
            let clients = parts.workspaces.clients();
            clients.map(|c| (c, place_of(&parts.workspaces, c))).collect()
        };
        let before = places(self);
        let out = change(self);
        let after = places(self);
        let clients: BTreeSet<ClientId> = before.keys().chain(after.keys()).copied().collect();
        for client in clients {
            let place = |places: &BTreeMap<ClientId, _>| places.get(&client).copied().flatten();
            let (was, now) = (place(&before), place(&after));
            if was != now {
                for at in was.map(Some).into_iter().chain([now]) {
                    let change = ViewChange::Shown;
                    self.journal.push(Event::View { client, at, change });
                }
            }
        }
        out
    }

    /// A client attaches: it is shown each workspace's first tab and each
    /// tab's first pane, and then `at`, if given, and each that holds it.
    pub(crate) fn attach(&mut self, client: ClientId, at: Option<AnyRef>) {
        self.viewing(|parts| {
            parts.workspaces.attach(client);
            for w in &mut parts.workspaces.children {
                w.tabs.attach(client);
                for tab in &mut w.tabs.children {
                    let first = tab.root.first_pane();
                    tab.seats.entry(client).or_insert(Seat::on(first));
                }
            }
            if let Some(at) = at {
                parts.show_path(client, at);
            }
        });
    }

    /// Disabling bells clears only this client's pending tab marks.
    pub(crate) fn clear_bells(&mut self, client: ClientId) {
        for ws in &mut self.workspaces.children {
            for tab in &mut ws.tabs.children {
                if let Some(seat) = tab.seat_mut(client) {
                    seat.rang = false;
                }
            }
        }
    }

    /// A client detaches: nothing names it.
    pub(crate) fn forget(&mut self, client: ClientId) {
        self.workspaces.forget(client);
        for w in &mut self.workspaces.children {
            w.tabs.forget(client);
            for tab in &mut w.tabs.children {
                tab.seats.remove(&client);
                tab.typist = tab.typist.filter(|t| *t != client);
            }
        }
    }

    /// Shows `client` `target`, a pane, tab or workspace, and each that
    /// holds it; nothing if it is not there.
    pub(crate) fn show(&mut self, client: ClientId, target: AnyRef) {
        self.viewing(|parts| parts.show_path(client, target));
    }

    fn show_path(&mut self, client: ClientId, target: AnyRef) {
        let ws = match target {
            AnyRef::Pane(pane) => {
                let Some((ws, tab)) = self.locate(pane) else {
                    return;
                };
                if let Some((_, t)) = tab_mut(&mut self.workspaces, tab) {
                    t.set_focus(&mut self.journal, ws, client, pane);
                }
                self.workspaces.get_mut(ws).map(|w| w.tabs.show(client, tab));
                ws
            }
            AnyRef::Tab(tab) => {
                let Some(w) = (self.workspaces.children.iter_mut()).find(|w| w.tabs.get(tab).is_some())
                else {
                    return;
                };
                w.tabs.show(client, tab);
                w.id
            }
            AnyRef::Workspace(ws) => ws,
        };
        self.workspaces.show(client, ws);
    }

    /// Swaps tab `tab` with its sibling `toward` in its workspace: none if
    /// there is no such tab, false if it has no sibling that way.
    pub(crate) fn reorder_tab(&mut self, tab: TabId, toward: Sibling) -> Option<bool> {
        let ws = (self.workspaces.children.iter_mut()).find(|w| w.tabs.get(tab).is_some())?;
        let changed = ws.tabs.reorder(tab, toward)?;
        if changed {
            self.journal
                .push(Event::Tab((ws.id, tab), Change::Reordered));
        }
        Some(changed)
    }

    pub(crate) fn reorder_workspace(&mut self, ws: WsId, toward: Sibling) -> Option<bool> {
        let changed = self.workspaces.reorder(ws, toward)?;
        if changed {
            self.journal.push(Event::Workspace(ws, Change::Reordered));
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
        let tabs = (self.workspaces.children.iter_mut()).flat_map(|w| w.tabs.children.iter_mut());
        let changed = |tab: &mut Tab| std::mem::take(&mut tab.changed) && !sized(tab.id);
        tabs.filter_map(|tab| changed(tab).then_some(tab.id))
            .collect()
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
        let mut tabs = (self.workspaces.children.iter())
            .flat_map(|w| w.tabs.children.iter().map(|t| (w.id, t)));
        tabs.find(|(_, t)| t.holds(pane)).map(|(w, t)| (w, t.id))
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
            for tab in ws.tabs() {
                for id in tab.root.panes() {
                    if let Some(pane) = panes.get_mut(&id) {
                        change(&mut PaneMut::new(pane, (ws.id, tab.id), journal, input));
                    }
                }
            }
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

    /// Adds workspace `id` named `name`, its one tab `tab` named
    /// `tab_name` and laid out on `fit`, holding `pane`, new.
    pub(crate) fn add_workspace(
        &mut self,
        ws: (WsId, Option<String>),
        first: (TabId, Option<String>, Fit),
        pane: Pane,
    ) {
        let at = (ws.0, first.0);
        self.push_workspace(ws, first, pane.id());
        self.placed(at, pane);
    }

    /// `add_workspace`, holding pane `pane`, in the session: each client
    /// is shown the tab there and focuses the pane, and is shown the
    /// workspace if it is the first.
    fn push_workspace(
        &mut self,
        (id, name): (WsId, Option<String>),
        (tab, tab_name, fit): (TabId, Option<String>, Fit),
        pane: PaneId,
    ) {
        self.journal
            .push(Event::Workspace(id, Change::Created(name.clone())));
        self.journal
            .push(Event::Tab((id, tab), Change::Created(tab_name.clone())));
        let clients: Vec<ClientId> = self.workspaces.clients().collect();
        let first = Tab::new(tab, tab_name, fit, clients.iter().copied(), pane);
        let tabs = Stack::of(first, clients.into_iter());
        self.workspaces.push(Workspace { id, name, tabs });
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
        let clients: Vec<ClientId> = self.workspaces.clients().collect();
        let workspace = self.workspaces.get_mut(ws)?;
        let tab = Tab::new(id, name.clone(), fit, clients.into_iter(), pane);
        workspace.tabs.push(tab);
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

    /// The layout `target`'s tab takes split for pane `new` beside it,
    /// along `axis`, after it, the two dividing what the target had as
    /// `shares` says (`Tree::split`): what the new pane starts at and is
    /// put in by (`split`).
    pub(crate) fn plan_split(
        &self,
        (target, new): (PaneId, PaneId),
        axis: Axis,
        shares: layout::Shares,
    ) -> Result<Split, NoPane> {
        let (ws, tab) = self.locate(target).ok_or(NoPane)?;
        let was = self.workspaces.get(ws).and_then(|w| w.tabs.get(tab));
        let was = was.map(|t| t.root.clone()).ok_or(NoPane)?;
        let mut root = was.clone();
        root.split((target, new), (axis, Side::After), shares)?;
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
    /// empties closes too, as does a workspace left with no tabs.
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

    /// Removes tab `tab` and gives back the panes in it, closed. A
    /// workspace it leaves with none goes too.
    pub(crate) fn remove_tab(&mut self, tab: TabId) -> Vec<Pane> {
        let Some((ws, t)) = tab_mut(&mut self.workspaces, tab) else {
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

    /// Takes tab `tab` out of its workspace, and the workspace if it is
    /// left with none: each client shown either is shown the one it was
    /// shown before, else the neighbour (`Stack::remove`).
    fn drop_tab(&mut self, tab: TabId) {
        let ws = (self.workspaces.children.iter_mut()).find(|w| w.tabs.get(tab).is_some());
        let Some(ws) = ws else {
            return;
        };
        ws.tabs.remove(tab);
        let (id, emptied) = (ws.id, ws.tabs.children.is_empty());
        self.journal.push(Event::Tab((id, tab), Change::Closed));
        if emptied {
            self.workspaces.remove(id);
            self.journal.push(Event::Workspace(id, Change::Closed));
        }
    }

    /// Closes everything: the panes given back, to end.
    pub(crate) fn close_all(&mut self) -> Vec<Pane> {
        let tabs: Vec<TabId> = (self.workspaces.children.iter())
            .flat_map(|w| w.tabs().iter().map(|t| t.id))
            .collect();
        tabs.into_iter()
            .flat_map(|tab| self.remove_tab(tab))
            .collect()
    }

    /// Names workspace `id` `name`, if there is one.
    pub(crate) fn rename_workspace(&mut self, id: WsId, name: String) -> Option<()> {
        let ws = self.workspaces.get_mut(id)?;
        let renamed = |change| Event::Workspace(id, change);
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
            let tabs = (parts.workspaces.children.iter_mut())
                .flat_map(|w| w.tabs.children.iter_mut().map(|t| (w.id, t)));
            for (ws, tab) in tabs.filter(|(_, t)| t.id == was_a.1 || t.id == was_b.1) {
                tab.edit(&mut parts.journal, ws, |root| root.swap(a, b));
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
    /// closing if it held it alone, as its workspace does if that leaves
    /// it none, and `to` takes it; no other tab changes. `None`, changing
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
                self.push_workspace(ws, first, pane);
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
    pub(crate) fn arrange(&mut self, id: TabId, root: Tree) -> Option<()> {
        let (ws, tab) = tab_mut(&mut self.workspaces, id)?;
        tab.edit(&mut self.journal, ws, |old| *old = root);
        self.journal.push(Event::Layout((ws, id)));
        Some(())
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
        let (_, tab) = tab_mut(&mut self.workspaces, id)?;
        let mut root = tab.root.clone();
        let out = change(&mut root);
        if !root.same_panes(&tab.root) {
            return None;
        }
        if root != tab.root {
            tab.edit(&mut self.journal, ws, |old| *old = root);
            self.journal.push(Event::Layout((ws, id)));
        }
        Some(out)
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

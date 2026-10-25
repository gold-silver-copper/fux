//! Workspaces, their tabs and panes, and where each attached client is
//! among them, kept in what it names: a workspace holds the clients showing
//! it, and each tab a [`Seat`] for every client. Closing a tab takes its
//! seats with it, and its layout changes only through [`Tab::edit`], which
//! keeps each focus on one of its panes: no client names a tab or pane that
//! is gone.
//!
//! [`Parts`] holds them with the journal of what happened to them: what
//! events tell is changed only here, by methods that journal the change
//! as they make it, and a pane is changed only through a [`PaneMut`], which
//! journals what it says happened in it as the borrow ends.
use crate::events::{Journal, Place, What};
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::layout::{self, Axis, Rect, Side, Tree};
use crate::pane::Pane;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// A workspace and its tabs, of which it always has one or more: each is
/// made with one, and the session removes a workspace its last tab leaves.
pub struct Workspace {
    pub id: WsId,
    name: String,
    tabs: Vec<Tab>,
    /// The clients showing it: each attached client shows one workspace
    /// while there is any.
    pub(crate) viewers: BTreeSet<ClientId>,
    /// The tab each attached client is shown when it shows the workspace.
    shown: BTreeMap<ClientId, TabId>,
}

pub struct Tab {
    pub id: TabId,
    name: String,
    /// Its name was given (`name`, `rename`), not fux's (`main`, `tab-N`, a
    /// program's): a name given is the only one, and takes fux's.
    given: bool,
    root: Option<Tree>,
    /// Its layout changed since its panes were sized for it: one no client
    /// shows is then laid out again (`Session::size_panes`).
    changed: bool,
    fit: Fit,
    seats: BTreeMap<ClientId, Seat>,
    /// The client that last typed into it, whose terminal answers its panes'
    /// colour queries (`outer`).
    pub(crate) typist: Option<ClientId>,
}

/// What a tab no client shows is laid out on: a size every tab has, given
/// or a client's, never made up. A client that shows it lays it out on its
/// own screen, whichever this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// The screens of the clients showing it: while none does, the last
    /// it was laid out on (the smallest, of several), or the one it was
    /// made on.
    Clients(fux_vt::Size),
    /// A screen of this size, client or none: `size` and `resize`.
    Fixed(fux_vt::Size),
}

/// What the tab at `place`, counted from 1, is named if it is given no
/// name and is not a workspace's first.
fn numbered(place: usize) -> String {
    format!("tab-{place}")
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
#[derive(Clone, Copy, Default)]
pub struct Seat {
    /// The pane it focuses, one of the tab's, unless the tab is empty.
    focus: Option<PaneId>,
    /// The pane it focused before (`str last focus`), another of the
    /// tab's.
    last: Option<PaneId>,
    /// The bell rang here while it was shown another tab, marked in its bar
    /// until it shows this one (`Session::ring`).
    pub(crate) rang: bool,
    /// A mouse button was pressed in its focus and not yet released: the
    /// motion and release go there (`Session::mouse`).
    pub(crate) held: bool,
}

impl Seat {
    pub fn last(&self) -> Option<PaneId> {
        self.last
    }
}

impl Workspace {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// The tabs, to change or reorder but not to add or remove.
    pub fn tabs_mut(&mut self) -> &mut [Tab] {
        &mut self.tabs
    }

    /// The tab `client` is shown here.
    pub fn tab_of(&self, client: ClientId) -> Option<&Tab> {
        let shown = self.shown.get(&client)?;
        self.tabs.iter().find(|t| t.id == *shown)
    }

    pub(crate) fn tab_of_mut(&mut self, client: ClientId) -> Option<&mut Tab> {
        let shown = self.shown.get(&client)?;
        self.tabs.iter_mut().find(|t| t.id == *shown)
    }

    /// Adds an empty tab, named after its place if no name is given, with
    /// a seat for every client that has one in the others; its name.
    fn add_tab(&mut self, id: TabId, name: Option<String>, fit: Fit) -> String {
        let number = self.tabs.len().saturating_add(1);
        let name = name.unwrap_or_else(|| numbered(number));
        let clients = self.tabs.first().map(|t| t.seats.keys().copied());
        let tab = Tab::new(id, name.clone(), fit, clients.into_iter().flatten());
        self.tabs.push(tab);
        name
    }

    /// Shows `client` the tab `at` says here, if given and one of this
    /// workspace's, and in it focuses the pane it says, if given and there.
    pub(crate) fn show(&mut self, client: ClientId, at: Option<crate::session::Spot>) {
        let Some(crate::session::Spot { tab, pane }) =
            at.filter(|at| self.tabs.iter().any(|t| t.id == at.tab))
        else {
            return;
        };
        let Some(shown) = self.shown.get_mut(&client) else {
            return;
        };
        let was = std::mem::replace(shown, tab);
        if self.viewers.contains(&client) && was != tab {
            self.tabs
                .iter_mut()
                .filter(|t| t.id == was)
                .for_each(Tab::hidden);
        }
        if let Some((t, pane)) = self.tabs.iter_mut().find(|t| t.id == tab).zip(pane) {
            t.set_focus(client, pane);
        }
    }

    /// The layouts of its tabs no client shows that changed since their
    /// panes were sized, each with the area its fit lays it out in, for
    /// sizing them. Those are then taken as sized, as is a shown one, by
    /// its clients.
    pub(crate) fn changed_unshown(&mut self) -> impl Iterator<Item = (&Tree, Rect)> {
        let (viewers, shown_to) = (&self.viewers, &self.shown);
        self.tabs.iter_mut().filter_map(move |tab| {
            let shown = viewers.iter().any(|c| shown_to.get(c) == Some(&tab.id));
            let changed = std::mem::take(&mut tab.changed);
            let tab = &*tab;
            (tab.root.as_ref())
                .filter(|_| changed && !shown)
                .map(|root| (root, tab.fit.area()))
        })
    }

    /// Removes tab `tab`, giving back its layout: the clients it was shown
    /// to are shown its neighbour.
    fn remove_tab(&mut self, tab: TabId) -> Option<Tree> {
        let index = self.tabs.iter().position(|t| t.id == tab)?;
        let root = self.tabs.get_mut(index)?.root.take();
        // Tab IDs are unique: this removes the tab at `index`.
        self.tabs.retain(|t| t.id != tab);
        let next = index.min(self.tabs.len().saturating_sub(1));
        if let Some(next) = self.tabs.get(next).map(|t| t.id) {
            self.shown
                .values_mut()
                .filter(|t| **t == tab)
                .for_each(|t| *t = next);
        }
        root
    }

    /// A client attaches: it is shown the first tab, and focuses each tab's
    /// first pane.
    pub(crate) fn attach(&mut self, client: ClientId) {
        if let Some(first) = self.tabs.first() {
            self.shown.insert(client, first.id);
        }
        for tab in &mut self.tabs {
            tab.seats.entry(client).or_default();
            tab.refocus();
        }
    }

    /// `client` no longer looks here.
    pub(crate) fn hide(&mut self, client: ClientId) {
        if self.viewers.remove(&client)
            && let Some(tab) = self.tab_of_mut(client)
        {
            tab.hidden();
        }
    }

    /// A client detaches: nothing here names it.
    pub(crate) fn forget(&mut self, client: ClientId) {
        let viewed = self.viewers.remove(&client);
        let shown = self.shown.remove(&client);
        for tab in &mut self.tabs {
            // A tab the client showed is laid out again on its own fit (its
            // `size`), not left at the client's.
            tab.changed |= viewed && shown == Some(tab.id);
            tab.seats.remove(&client);
            tab.typist = tab.typist.filter(|t| *t != client);
        }
    }
}

impl Tab {
    /// An empty tab, laid out on `fit`, with a seat for each of `clients`.
    fn new(id: TabId, name: String, fit: Fit, clients: impl Iterator<Item = ClientId>) -> Tab {
        Tab {
            id,
            name,
            given: false,
            root: None,
            changed: true,
            fit,
            seats: clients.map(|c| (c, Seat::default())).collect(),
            typist: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn given(&self) -> bool {
        self.given
    }

    /// Its layout, unless it is empty.
    pub fn root(&self) -> Option<&Tree> {
        self.root.as_ref()
    }

    pub fn fit(&self) -> Fit {
        self.fit
    }

    /// Lays it out on what `fit` says from now on.
    pub(crate) fn set_fit(&mut self, fit: Fit) {
        self.fit = fit;
        self.changed = true;
    }

    /// Clients showing it lay it out on `size`, the smallest of their
    /// screens: laid out on clients' screens, that is its size while none
    /// does.
    pub(crate) fn shown_on(&mut self, size: fux_vt::Size) {
        if let Fit::Clients(last) = &mut self.fit {
            *last = size;
        }
    }

    pub fn holds(&self, pane: PaneId) -> bool {
        self.root.as_ref().is_some_and(|r| r.contains(pane))
    }

    pub fn seat(&self, client: ClientId) -> Option<&Seat> {
        self.seats.get(&client)
    }

    pub(crate) fn seat_mut(&mut self, client: ClientId) -> Option<&mut Seat> {
        self.seats.get_mut(&client)
    }

    /// A client it was shown to, which looked at its workspace, no longer
    /// sees it: with a size of its own, it is laid out on that again, not
    /// left at the client's (laid out on clients' screens, it keeps what it
    /// has: a zoomed pane, its whole screen).
    fn hidden(&mut self) {
        self.changed |= matches!(self.fit, Fit::Fixed(_));
    }

    /// The pane `client` focuses here.
    pub fn focus(&self, client: ClientId) -> Option<PaneId> {
        self.seat(client)?.focus
    }

    /// Focuses `pane` for `client`, if the tab holds it, remembering the
    /// pane it replaces.
    pub(crate) fn set_focus(&mut self, client: ClientId, pane: PaneId) {
        let holds = self.holds(pane);
        let seat = self.seats.get_mut(&client);
        if let Some(seat) = seat.filter(|s| holds && s.focus != Some(pane)) {
            (seat.last, seat.focus, seat.held) = (seat.focus, Some(pane), false);
        }
    }

    /// Changes the layout with `change`; each client then focuses the pane
    /// it did if the tab still holds it, else the one before, else the
    /// first.
    fn edit<R>(&mut self, change: impl FnOnce(&mut Option<Tree>) -> R) -> R {
        let out = change(&mut self.root);
        self.changed = true;
        self.refocus();
        out
    }

    fn refocus(&mut self) {
        let root = self.root.as_ref();
        let inside = |p: &PaneId| root.is_some_and(|r| r.contains(*p));
        for seat in self.seats.values_mut() {
            let (focus, last) = (seat.focus.filter(inside), seat.last.filter(inside));
            seat.held &= focus.is_some();
            seat.focus = focus.or(last).or_else(|| root.and_then(Tree::first_pane));
            seat.last = last.filter(|l| Some(*l) != seat.focus);
        }
    }
}

/// The session's workspaces, their tabs and panes, and the journal of what
/// happened to them: what events tell of them changes only by its methods,
/// each of which journals what it changes as it changes it.
#[derive(Default)]
pub struct Parts {
    workspaces: Vec<Workspace>,
    /// Each in one tab: a pane is added in its place and leaves with it.
    panes: BTreeMap<PaneId, Pane>,
    journal: Journal,
}

/// A pane of the session's, to change: what it says happened in it
/// (`Pane::said`) goes in the journal, at its place, as the borrow ends.
pub struct PaneMut<'a>(&'a mut Pane, Place, &'a mut Journal);

impl std::ops::Deref for PaneMut<'_> {
    type Target = Pane;
    fn deref(&self) -> &Pane {
        self.0
    }
}

impl std::ops::DerefMut for PaneMut<'_> {
    fn deref_mut(&mut self) -> &mut Pane {
        self.0
    }
}

impl Drop for PaneMut<'_> {
    fn drop(&mut self) {
        for what in self.0.said() {
            self.2.push(self.1, what);
        }
    }
}

/// Tab `id` of `workspaces`, and its workspace's id.
fn tab_mut(workspaces: &mut [Workspace], id: TabId) -> Option<(WsId, &mut Tab)> {
    let mut tabs = (workspaces.iter_mut()).flat_map(|w| w.tabs.iter_mut().map(|t| (w.id, t)));
    tabs.find(|(_, t)| t.id == id)
}

/// Names `old` `new`, journaled at `place` if it is another name.
fn rename(journal: &mut Journal, place: Place, old: &mut String, new: String) {
    if *old != new {
        journal.push(place, What::Renamed(new.clone()));
        *old = new;
    }
}

impl Parts {
    pub fn workspaces(&self) -> &[Workspace] {
        &self.workspaces
    }

    /// The workspaces, to change what events do not tell: where clients
    /// are, what they focus, the tabs' fits, the order.
    pub(crate) fn workspaces_mut(&mut self) -> &mut [Workspace] {
        &mut self.workspaces
    }

    pub fn panes(&self) -> &BTreeMap<PaneId, Pane> {
        &self.panes
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Journals that `what` happened at `place`.
    fn tell(&mut self, place: Place, what: What) {
        self.journal.push(place, what);
    }

    /// The workspace and tab that hold pane `pane`.
    pub fn locate(&self, pane: PaneId) -> Option<(WsId, TabId)> {
        let mut tabs = (self.workspaces.iter()).flat_map(|w| w.tabs.iter().map(|t| (w.id, t)));
        tabs.find(|(_, t)| t.holds(pane)).map(|(w, t)| (w, t.id))
    }

    /// Pane `id`, to change.
    pub fn pane_mut(&mut self, id: PaneId) -> Option<PaneMut<'_>> {
        let place = Place::Pane(self.locate(id)?, id);
        Some(PaneMut(self.panes.get_mut(&id)?, place, &mut self.journal))
    }

    /// Changes each pane with `change`.
    pub fn each_pane_mut(&mut self, mut change: impl FnMut(&mut Pane)) {
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in ids {
            if let Some(mut pane) = self.pane_mut(id) {
                change(&mut pane);
            }
        }
    }

    /// Adds workspace `id` named `name`, its one tab `tab` named
    /// `tab_name` and laid out on `fit`, a seat there for each of
    /// `clients`, holding `pane` if one is given.
    pub(crate) fn add_workspace(
        &mut self,
        (id, name): (WsId, String),
        (tab, tab_name, fit): (TabId, String, Fit),
        clients: impl Iterator<Item = ClientId>,
        pane: Option<Pane>,
    ) {
        self.tell(Place::Workspace(id), What::Created(Some(name.clone())));
        self.tell(Place::Tab(id, tab), What::Created(Some(tab_name.clone())));
        let first = Tab::new(tab, tab_name, fit, clients);
        let shown = first.seats.keys().map(|c| (*c, tab)).collect();
        let viewers = BTreeSet::new();
        let tabs = vec![first];
        self.workspaces.push(Workspace {
            id,
            name,
            tabs,
            viewers,
            shown,
        });
        self.put(id, tab, pane);
    }

    /// Adds tab `id` to workspace `ws`, named `name` or after its place,
    /// laid out on `fit`, holding `pane` if one is given; the pane given
    /// back if there is no such workspace.
    #[expect(clippy::result_large_err, reason = "a pane given back")]
    pub(crate) fn add_tab(
        &mut self,
        (ws, id): (WsId, TabId),
        name: Option<String>,
        fit: Fit,
        pane: Option<Pane>,
    ) -> Result<(), Option<Pane>> {
        let Some(workspace) = self.workspaces.iter_mut().find(|w| w.id == ws) else {
            return Err(pane);
        };
        let name = workspace.add_tab(id, name, fit);
        self.tell(Place::Tab(ws, id), What::Created(Some(name)));
        self.put(ws, id, pane);
        Ok(())
    }

    /// Puts `pane`, new, in tab `tab`, made empty, of workspace `ws`.
    fn put(&mut self, ws: WsId, tab: TabId, pane: Option<Pane>) {
        let Some(pane) = pane else {
            return;
        };
        if let Some((_, t)) = tab_mut(&mut self.workspaces, tab) {
            t.edit(|root| *root = Some(Tree::Pane(pane.id)));
        }
        self.placed((ws, tab), pane);
    }

    /// `pane`, new, is in tab `at` of its workspace.
    fn placed(&mut self, at: (WsId, TabId), pane: Pane) {
        self.tell(Place::Pane(at, pane.id), What::Created(None));
        self.panes.insert(pane.id, pane);
    }

    /// Puts `pane`, new, beside `target`, along `axis`, after it; the pane
    /// given back if `target` is in no tab.
    #[expect(clippy::result_large_err, reason = "a pane given back")]
    pub(crate) fn split(&mut self, target: PaneId, pane: Pane, axis: Axis) -> Result<(), Pane> {
        let (Some(at), new) = (self.locate(target), pane.id) else {
            return Err(pane);
        };
        let split = |root: &mut Option<Tree>| {
            (root.as_mut()).is_some_and(|r| r.split(target, new, axis, Side::After))
        };
        if !tab_mut(&mut self.workspaces, at.1).is_some_and(|(_, t)| t.edit(split)) {
            return Err(pane);
        }
        self.placed(at, pane);
        Ok(())
    }

    /// Takes pane `id` out of its tab: given back, closed. A tab its going
    /// empties closes too, as does a workspace left with no tabs.
    pub(crate) fn close_pane(&mut self, id: PaneId) -> Option<Pane> {
        let at = self.locate(id)?;
        let (_, tab) = tab_mut(&mut self.workspaces, at.1)?;
        let emptied = tab.edit(|root| layout::remove(root, id) && root.is_none());
        self.tell(Place::Pane(at, id), What::Closed);
        if emptied {
            self.remove_tab(at.1);
        }
        self.panes.remove(&id)
    }

    /// Removes tab `tab` and gives back the panes in it, closed. A
    /// workspace it leaves with none goes too, its viewers shown the first
    /// workspace left.
    pub(crate) fn remove_tab(&mut self, tab: TabId) -> Vec<Pane> {
        let ws = (self.workspaces.iter_mut()).find(|w| w.tabs.iter().any(|t| t.id == tab));
        let Some(ws) = ws else {
            return Vec::new();
        };
        let (id, panes) = (ws.id, ws.remove_tab(tab).map(|r| r.panes()));
        let viewers = ws.tabs.is_empty().then(|| std::mem::take(&mut ws.viewers));
        for &pane in panes.iter().flatten() {
            self.tell(Place::Pane((id, tab), pane), What::Closed);
        }
        self.tell(Place::Tab(id, tab), What::Closed);
        if let Some(viewers) = viewers {
            self.workspaces.retain(|w| w.id != id);
            if let Some(first) = self.workspaces.first_mut() {
                first.viewers.extend(viewers);
            }
            self.tell(Place::Workspace(id), What::Closed);
        }
        let gone = panes.into_iter().flatten();
        gone.filter_map(|pane| self.panes.remove(&pane)).collect()
    }

    /// Closes everything: the panes given back, to end.
    pub(crate) fn close_all(&mut self) -> Vec<Pane> {
        let tabs = self
            .workspaces
            .iter()
            .flat_map(|w| w.tabs.iter().map(|t| t.id));
        let tabs: Vec<TabId> = tabs.collect();
        tabs.into_iter()
            .flat_map(|tab| self.remove_tab(tab))
            .collect()
    }

    /// Names workspace `id` `name`: whether there is one.
    pub(crate) fn rename_workspace(&mut self, id: WsId, name: String) -> bool {
        let ws = self.workspaces.iter_mut().find(|w| w.id == id);
        ws.map(|ws| rename(&mut self.journal, Place::Workspace(id), &mut ws.name, name))
            .is_some()
    }

    /// Gives tab `id` the name `name`: whether there is one. It takes it
    /// from each other tab fux named so (`tab-2`, made before a dump's line
    /// names one so): that one is named anew as fux names tabs, by its
    /// place, the next number free.
    pub(crate) fn give_tab_name(&mut self, id: TabId, name: String) -> bool {
        let mut taken: HashSet<String> = (self.workspaces.iter())
            .flat_map(|ws| std::iter::once(&ws.name).chain(ws.tabs.iter().map(|t| &t.name)))
            .cloned()
            .collect();
        let mut names = vec![(id, name.clone())];
        for ws in &self.workspaces {
            for (place, tab) in (1usize..).zip(&ws.tabs) {
                let free = (place..).map(numbered).find(|n| !taken.contains(n));
                if let (false, Some(free)) = (tab.given || tab.name != name || tab.id == id, free) {
                    taken.insert(free.clone());
                    names.push((tab.id, free));
                }
            }
        }
        let Some((_, tab)) = tab_mut(&mut self.workspaces, id) else {
            return false;
        };
        tab.given = true;
        for (tab, name) in names {
            if let Some((ws, t)) = tab_mut(&mut self.workspaces, tab) {
                rename(&mut self.journal, Place::Tab(ws, tab), &mut t.name, name);
            }
        }
        true
    }

    /// Swaps two panes' places: in one tab, or each in the other's, which
    /// moves both.
    pub(crate) fn swap(&mut self, a: PaneId, b: PaneId) {
        let (Some(was_a), Some(was_b)) = (self.locate(a), self.locate(b)) else {
            return;
        };
        for tab in self.workspaces.iter_mut().flat_map(|w| w.tabs.iter_mut()) {
            tab.edit(|root| root.as_mut().map(|r| layout::swap(r, a, b)));
        }
        if was_a != was_b {
            self.tell(Place::Pane(was_b, a), What::Moved(was_a));
            self.tell(Place::Pane(was_a, b), What::Moved(was_b));
        }
    }

    /// Moves `pane` from its tab to the end of tab `to`, another: every tab
    /// lets go of it, and `to` takes it.
    pub(crate) fn relocate(&mut self, pane: PaneId, to: TabId) {
        let ws = tab_mut(&mut self.workspaces, to).map(|(ws, _)| ws);
        let (Some(was), Some(ws)) = (self.locate(pane), ws) else {
            return;
        };
        for tab in self.workspaces.iter_mut().flat_map(|w| w.tabs.iter_mut()) {
            let takes = tab.id == to;
            tab.edit(|root| {
                layout::remove(root, pane);
                if takes {
                    layout::append(root, pane);
                }
            });
        }
        self.tell(Place::Pane((ws, to), pane), What::Moved(was));
    }

    /// Changes the layout of `pane`'s tab with `change`, which keeps its
    /// panes: moves one, moves a border, or lays them out anew. A change
    /// that would not keep them is undone, and is `None`, as is a pane in
    /// no tab.
    pub(crate) fn reshape<R>(
        &mut self,
        pane: PaneId,
        change: impl FnOnce(&mut Option<Tree>) -> R,
    ) -> Option<R> {
        let (_, tab) = self.locate(pane)?;
        let (_, tab) = tab_mut(&mut self.workspaces, tab)?;
        let sorted = |root: &Option<Tree>| {
            let mut panes = root.as_ref().map(Tree::panes);
            panes.iter_mut().for_each(|p| p.sort_unstable());
            panes
        };
        let was = tab.root.clone();
        let out = tab.edit(change);
        if sorted(&tab.root) == sorted(&was) {
            return Some(out);
        }
        tab.edit(|root| *root = was);
        None
    }
}

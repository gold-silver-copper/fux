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
use crate::command::Sibling;
use crate::events::{Journal, Place, What};
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::layout::{self, Axis, Rect, Side, Tree};
use crate::pane::{Loose, Pane};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::num::NonZeroU32;

/// A workspace and its tabs, of which it always has one or more: each is
/// made with one, and the session removes a workspace its last tab leaves.
pub struct Workspace {
    /// Given as it is made, and never changed: its events' place.
    id: WsId,
    name: String,
    tabs: Vec<Tab>,
    /// The clients showing it: each attached client shows one workspace
    /// while there is any.
    pub(crate) viewers: BTreeSet<ClientId>,
    /// The tab each attached client is shown when it shows the workspace.
    shown: BTreeMap<ClientId, TabId>,
}

pub struct Tab {
    /// Given as it is made, and never changed: its events' place.
    id: TabId,
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
    /// it was laid out on (the smallest, of several) that showed every
    /// pane, or the one it was made on.
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
    pub fn id(&self) -> WsId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// The tab `client` is shown here.
    pub fn tab_of(&self, client: ClientId) -> Option<&Tab> {
        let shown = self.shown.get(&client)?;
        self.tabs.iter().find(|t| t.id == *shown)
    }

    fn tab_of_mut(&mut self, client: ClientId) -> Option<&mut Tab> {
        let shown = self.shown.get(&client)?;
        self.tabs.iter_mut().find(|t| t.id == *shown)
    }

    /// Shows `client` the tab `at` says here, if given and one of this
    /// workspace's, and in it focuses the pane it says, if given and there.
    fn show(&mut self, client: ClientId, at: Option<crate::session::Spot>) {
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

    /// `client` no longer looks here.
    fn hide(&mut self, client: ClientId) {
        if self.viewers.remove(&client)
            && let Some(tab) = self.tab_of_mut(client)
        {
            tab.hidden();
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

    pub fn id(&self) -> TabId {
        self.id
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
    fn set_fit(&mut self, fit: Fit) {
        self.fit = fit;
        self.changed = true;
    }

    /// Clients showing it lay it out on `size`, the smallest of their
    /// screens: laid out on clients' screens, that is its size while none
    /// does, if it shows every pane. A screen too small to is not: the tab
    /// keeps the last size that showed them all, which a dump makes it at.
    pub(crate) fn shown_on(&mut self, size: fux_vt::Size) {
        let area = Fit::Clients(size).area();
        let shows_all = (self.root.as_ref()).is_none_or(|r| layout::shows_all(r, area));
        if let (Fit::Clients(last), true) = (&mut self.fit, shows_all) {
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
/// No whole pane can be put in its place (`*pane = other`), losing what
/// the old one said: a pane in the session is `Placed`, which only this
/// module makes, and no other is had by value.
pub struct PaneMut<'a>(&'a mut Pane, Place, &'a mut Journal);

/// What a pane in the session is (`Pane<Placed>`): made here alone, as
/// it is put in.
pub struct Placed(());

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

/// Leave to take what a pane said (`Pane::said`), which only this module
/// gives, as it puts what it takes in the journal: code elsewhere cannot
/// take it and drop it.
pub struct Journaling(());

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
        for what in self.0.said(Journaling(())) {
            self.2.push(self.1, what);
        }
    }
}

/// Tab `id` of `workspaces`, and its workspace's id.
fn tab_mut(workspaces: &mut [Workspace], id: TabId) -> Option<(WsId, &mut Tab)> {
    let mut tabs = (workspaces.iter_mut()).flat_map(|w| w.tabs.iter_mut().map(|t| (w.id, t)));
    tabs.find(|(_, t)| t.id == id)
}

/// Swaps the item `is` picks with its sibling `toward`: none if `is` picks
/// none, false if it has no sibling that way.
fn swap_sibling<T>(items: &mut [T], is: impl Fn(&T) -> bool, toward: Sibling) -> Option<bool> {
    let index = items.iter().position(is)?;
    let first = match toward {
        Sibling::Next => Some(index),
        Sibling::Previous => index.checked_sub(1),
    };
    let pair = first.and_then(|i| items.get_mut(i..i.checked_add(2)?));
    Some(pair.map(<[T]>::reverse).is_some())
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

    /// Tab `id`, to change what events do not tell: what its clients
    /// focus, its fit. One tab at a time, and none is made but here, so
    /// none is put in another's place (`mem::swap`), its panes moved with
    /// no journal entry.
    pub(crate) fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        tab_mut(&mut self.workspaces, id).map(|(_, tab)| tab)
    }

    /// A client attaches: it is shown each workspace's first tab, and
    /// focuses each tab's first pane.
    pub(crate) fn attach(&mut self, client: ClientId) {
        for w in &mut self.workspaces {
            if let Some(first) = w.tabs.first() {
                w.shown.insert(client, first.id);
            }
            for tab in &mut w.tabs {
                tab.seats.entry(client).or_default();
                tab.refocus();
            }
        }
    }

    /// A client detaches: nothing names it.
    pub(crate) fn forget(&mut self, client: ClientId) {
        for w in &mut self.workspaces {
            let viewed = w.viewers.remove(&client);
            let shown = w.shown.remove(&client);
            for tab in &mut w.tabs {
                // A tab the client showed is laid out again on its own fit
                // (its `size`), not left at the client's.
                tab.changed |= viewed && shown == Some(tab.id);
                tab.seats.remove(&client);
                tab.typist = tab.typist.filter(|t| *t != client);
            }
        }
    }

    /// Shows `client` workspace `ws`, and there the tab and pane `at`
    /// says, each if given and there.
    pub(crate) fn show(&mut self, client: ClientId, ws: WsId, at: Option<crate::session::Spot>) {
        for w in &mut self.workspaces {
            if w.id != ws {
                w.hide(client);
            } else {
                w.viewers.insert(client);
                w.show(client, at);
            }
        }
    }

    /// Swaps tab `tab` with its sibling `toward` in its workspace: none if
    /// there is no such tab, false if it has no sibling that way.
    pub(crate) fn reorder_tab(&mut self, tab: TabId, toward: Sibling) -> Option<bool> {
        let swapped = (self.workspaces.iter_mut())
            .find_map(|w| swap_sibling(&mut w.tabs, |t| t.id == tab, toward));
        self.reordered(swapped)
    }

    /// Swaps workspace `ws` with its sibling `toward`, as `reorder_tab`.
    pub(crate) fn reorder_workspace(&mut self, ws: WsId, toward: Sibling) -> Option<bool> {
        let swapped = swap_sibling(&mut self.workspaces, |w| w.id == ws, toward);
        self.reordered(swapped)
    }

    /// A reorder done (`swapped`), which no event tells, counted as a
    /// change a saved session holds.
    fn reordered(&mut self, swapped: Option<bool>) -> Option<bool> {
        if swapped == Some(true) {
            self.journal.reshaped();
        }
        swapped
    }

    /// Lays tab `tab` out on what `fit` makes of its fit from now on: a
    /// change a saved session holds.
    pub(crate) fn set_fit(&mut self, tab: TabId, fit: impl FnOnce(Fit) -> Fit) -> Option<()> {
        let (_, tab) = tab_mut(&mut self.workspaces, tab)?;
        let fit = fit(tab.fit);
        tab.set_fit(fit);
        self.journal.reshaped();
        Some(())
    }

    /// The tabs no client sizes (`sized` says which do) whose layout
    /// changed since their panes were sized, for sizing them on their fit.
    /// Those are then taken as sized, as is a sized one, by its clients.
    pub(crate) fn changed_unshown(&mut self, sized: impl Fn(TabId) -> bool) -> Vec<TabId> {
        let tabs = self.workspaces.iter_mut().flat_map(|w| w.tabs.iter_mut());
        let changed = |tab: &mut Tab| std::mem::take(&mut tab.changed) && !sized(tab.id);
        tabs.filter_map(|tab| changed(tab).then_some(tab.id))
            .collect()
    }

    pub fn panes(&self) -> &BTreeMap<PaneId, Pane> {
        &self.panes
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
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
        pane: Option<Pane<Loose>>,
    ) {
        self.journal
            .push(Place::Workspace(id), What::Created(Some(name.clone())));
        self.journal
            .push(Place::Tab(id, tab), What::Created(Some(tab_name.clone())));
        let first = Tab::new(tab, tab_name, fit, clients);
        let shown = first.seats.keys().map(|c| (*c, tab)).collect();
        self.workspaces.push(Workspace {
            id,
            name,
            tabs: vec![first],
            viewers: BTreeSet::new(),
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
        pane: Option<Pane<Loose>>,
    ) -> Result<(), Option<Pane<Loose>>> {
        let Some(workspace) = self.workspaces.iter_mut().find(|w| w.id == ws) else {
            return Err(pane);
        };
        // Named after its place if no name is given, with a seat for every
        // client that has one in the others.
        let name = name.unwrap_or_else(|| numbered(workspace.tabs.len().saturating_add(1)));
        let clients = workspace.tabs.first().map(|t| t.seats.keys().copied());
        let tab = Tab::new(id, name.clone(), fit, clients.into_iter().flatten());
        workspace.tabs.push(tab);
        self.journal
            .push(Place::Tab(ws, id), What::Created(Some(name)));
        self.put(ws, id, pane);
        Ok(())
    }

    /// Puts `pane`, new, in tab `tab`, made empty, of workspace `ws`.
    fn put(&mut self, ws: WsId, tab: TabId, pane: Option<Pane<Loose>>) {
        let Some(pane) = pane else {
            return;
        };
        if let Some((_, t)) = tab_mut(&mut self.workspaces, tab) {
            t.edit(|root| *root = Some(Tree::Pane(pane.id())));
        }
        self.placed((ws, tab), pane);
    }

    /// `pane`, new, is in tab `at` of its workspace.
    fn placed(&mut self, at: (WsId, TabId), pane: Pane<Loose>) {
        self.journal
            .push(Place::Pane(at, pane.id()), What::Created(None));
        self.panes.insert(pane.id(), pane.placed(Placed(())));
    }

    /// The layout `target`'s tab takes split for pane `new` beside it,
    /// along `axis`, after it, the two dividing what the target had as
    /// `shares` says if given (`Tree::divide`): what the new pane starts at
    /// and is put in by (`split`).
    pub(crate) fn plan_split(
        &self,
        (target, new): (PaneId, PaneId),
        axis: Axis,
        shares: Option<(NonZeroU32, NonZeroU32)>,
    ) -> Option<Split> {
        let (_, tab) = self.locate(target)?;
        let mut tabs = (self.workspaces.iter()).flat_map(|w| &w.tabs);
        let was = tabs.find(|t| t.id == tab)?.root.clone()?;
        let mut root = was.clone();
        root.split(target, new, axis, Side::After).then_some(())?;
        if let Some(shares) = shares {
            root.divide((target, new), shares);
        }
        Some(Split { tab, was, root })
    }

    /// Puts `pane`, new, in its tab laid out as `split` planned: the pane
    /// given back if it is not the one planned for, or the tab's layout
    /// changed since.
    #[expect(clippy::result_large_err, reason = "a pane given back")]
    pub(crate) fn split(&mut self, pane: Pane<Loose>, split: Split) -> Result<(), Pane<Loose>> {
        let Split { tab, was, root } = split;
        let planned = root.contains(pane.id()) && !was.contains(pane.id());
        let Some((ws, t)) = tab_mut(&mut self.workspaces, tab)
            .filter(|(_, t)| planned && t.root.as_ref() == Some(&was))
        else {
            return Err(pane);
        };
        t.edit(|r| *r = Some(root));
        self.placed((ws, tab), pane);
        Ok(())
    }

    /// Takes pane `id` out of its tab: given back, closed. A tab its going
    /// empties closes too, as does a workspace left with no tabs.
    pub(crate) fn close_pane(&mut self, id: PaneId) -> Option<Pane<Loose>> {
        let at = self.locate(id)?;
        let (_, tab) = tab_mut(&mut self.workspaces, at.1)?;
        let emptied = tab.edit(|root| layout::remove(root, id) && root.is_none());
        self.journal.push(Place::Pane(at, id), What::Closed);
        if emptied {
            self.remove_tab(at.1);
        }
        self.panes.remove(&id).map(|pane| pane.placed(Loose))
    }

    /// Removes tab `tab` and gives back the panes in it, closed. A
    /// workspace it leaves with none goes too, its viewers shown the first
    /// workspace left.
    pub(crate) fn remove_tab(&mut self, tab: TabId) -> Vec<Pane<Loose>> {
        let ws = (self.workspaces.iter_mut()).find(|w| w.tabs.iter().any(|t| t.id == tab));
        let Some(ws) = ws else {
            return Vec::new();
        };
        let (id, panes) = (ws.id, ws.remove_tab(tab).map(|r| r.panes()));
        let viewers = ws.tabs.is_empty().then(|| std::mem::take(&mut ws.viewers));
        for &pane in panes.iter().flatten() {
            self.journal
                .push(Place::Pane((id, tab), pane), What::Closed);
        }
        self.journal.push(Place::Tab(id, tab), What::Closed);
        if let Some(viewers) = viewers {
            self.workspaces.retain(|w| w.id != id);
            if let Some(first) = self.workspaces.first_mut() {
                first.viewers.extend(viewers);
            }
            self.journal.push(Place::Workspace(id), What::Closed);
        }
        let gone = panes.into_iter().flatten();
        let gone = gone.filter_map(|pane| self.panes.remove(&pane));
        gone.map(|pane| pane.placed(Loose)).collect()
    }

    /// Closes everything: the panes given back, to end.
    pub(crate) fn close_all(&mut self) -> Vec<Pane<Loose>> {
        let tabs: Vec<TabId> = (self.workspaces.iter())
            .flat_map(|w| w.tabs.iter().map(|t| t.id))
            .collect();
        tabs.into_iter()
            .flat_map(|tab| self.remove_tab(tab))
            .collect()
    }

    /// Names workspace `id` `name`, if there is one.
    pub(crate) fn rename_workspace(&mut self, id: WsId, name: String) -> Option<()> {
        let ws = self.workspaces.iter_mut().find(|w| w.id == id)?;
        rename(&mut self.journal, Place::Workspace(id), &mut ws.name, name);
        Some(())
    }

    /// Gives tab `id` the name `name`, if there is one. It takes it
    /// from each other tab fux named so (`tab-2`, made before a dump's line
    /// names one so): that one is named anew as fux names tabs, by its
    /// place, the next number free.
    pub(crate) fn give_tab_name(&mut self, id: TabId, name: String) -> Option<()> {
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
        tab_mut(&mut self.workspaces, id)?.1.given = true;
        for (tab, name) in names {
            if let Some((ws, t)) = tab_mut(&mut self.workspaces, tab) {
                rename(&mut self.journal, Place::Tab(ws, tab), &mut t.name, name);
            }
        }
        Some(())
    }

    /// Swaps two panes' places: in one tab, or each in the other's, which
    /// moves both.
    pub(crate) fn swap(&mut self, a: PaneId, b: PaneId) {
        let (Some(was_a), Some(was_b)) = (self.locate(a), self.locate(b)) else {
            return;
        };
        // Only the tabs that hold them: another, laid out anew, would lose
        // what its panes have (a zoomed pane, its whole screen).
        let tabs = self.workspaces.iter_mut().flat_map(|w| w.tabs.iter_mut());
        for tab in tabs.filter(|t| t.id == was_a.1 || t.id == was_b.1) {
            tab.edit(|root| root.as_mut().map(|r| layout::swap(r, a, b)));
        }
        if was_a != was_b {
            self.journal.push(Place::Pane(was_b, a), What::Moved(was_a));
            self.journal.push(Place::Pane(was_a, b), What::Moved(was_b));
        } else {
            self.journal.reshaped();
        }
    }

    /// Moves `pane` from its tab to the end of tab `to`, another: its tab
    /// lets go of it, and `to` takes it; no other tab changes.
    pub(crate) fn relocate(&mut self, pane: PaneId, to: TabId) {
        let ws = tab_mut(&mut self.workspaces, to).map(|(ws, _)| ws);
        let (Some(was), Some(ws)) = (self.locate(pane), ws) else {
            return;
        };
        if let Some((_, from)) = tab_mut(&mut self.workspaces, was.1) {
            from.edit(|root| layout::remove(root, pane));
        }
        if let Some((_, to)) = tab_mut(&mut self.workspaces, to) {
            to.edit(|root| layout::append(root, pane));
        }
        self.journal
            .push(Place::Pane((ws, to), pane), What::Moved(was));
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
            if tab.root != was {
                self.journal.reshaped();
            }
            return Some(out);
        }
        tab.edit(|root| *root = was);
        None
    }
}

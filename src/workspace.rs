//! Workspaces, their tabs and panes, and where each attached client is
//! among them, kept in what it names: a workspace holds the clients showing
//! it, and each tab a [`Seat`] for every client. Closing a tab takes its
//! seats with it, and its layout changes only through [`Tab::edit`], which
//! keeps each focus on one of its panes: no client names a tab or pane that
//! is gone.
//!
//! [`Parts`] holds them with the journal of what happened to them: what
//! events tell is changed only here, by methods that journal the change
//! as they make it, and by a pane's own, which a [`PaneMut`] gives the
//! journal at the pane's place.
use crate::command::Sibling;
use crate::events::{Change, Event, Journal, PaneEvent};
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::layout::{self, Axis, NoPane, Rect, Removed, Side, Tree};
use crate::pane::{Pane, PaneMut};
use std::collections::{BTreeMap, BTreeSet};

/// A workspace and its tabs, of which it always has one or more: each is
/// made with one, and the session removes a workspace its last tab leaves.
pub struct Workspace {
    /// Given as it is made, and never changed: its events' place.
    id: WsId,
    name: Option<String>,
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
    name: Option<String>,
    /// Its panes, of which it has one or more: a tab is made with one, and
    /// closes as its last leaves.
    root: Tree,
    /// Its layout changed since its panes were sized for it: one no client
    /// shows is then laid out again (`Session::size_panes`).
    changed: bool,
    fit: Fit,
    seats: BTreeMap<ClientId, Seat>,
    /// The client that last typed into it, which hosts its panes while it
    /// shows it (`Session::host`).
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
    /// The pane it focuses, one of the tab's.
    focus: PaneId,
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
    /// A seat focusing `pane`, new.
    fn on(focus: PaneId) -> Seat {
        Seat {
            focus,
            last: None,
            rang: false,
            held: false,
        }
    }
    pub fn last(&self) -> Option<PaneId> {
        self.last
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

    pub(crate) fn display_label(&self) -> crate::words::Label<'_> {
        self.name().map_or(
            crate::words::Label::Id(crate::command::AnyRef::Workspace(self.id)),
            crate::words::Label::Text,
        )
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// The tab `client` is shown here.
    pub fn tab_of(&self, client: ClientId) -> Option<&Tab> {
        let shown = self.shown.get(&client)?;
        self.tabs.iter().find(|t| t.id == *shown)
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
        *shown = tab;
        if let Some((t, pane)) = self.tabs.iter_mut().find(|t| t.id == tab).zip(pane) {
            t.set_focus(client, pane);
        }
    }

    /// Removes tab `tab`: the clients it was shown to are shown its
    /// neighbour.
    fn remove_tab(&mut self, tab: TabId) {
        let Some(index) = self.tabs.iter().position(|t| t.id == tab) else {
            return;
        };
        // Tab IDs are unique: this removes the tab at `index`.
        self.tabs.retain(|t| t.id != tab);
        let next = index.min(self.tabs.len().saturating_sub(1));
        if let Some(next) = self.tabs.get(next).map(|t| t.id) {
            self.shown
                .values_mut()
                .filter(|t| **t == tab)
                .for_each(|t| *t = next);
        }
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
            changed: true,
            fit,
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
    ) -> crate::words::Label<'a> {
        if let Some(name) = self.name() {
            return crate::words::Label::Text(name);
        }
        panes.get(&self.root.first_pane()).map_or(
            crate::words::Label::Id(crate::command::AnyRef::Tab(self.id)),
            Pane::display_label,
        )
    }

    pub fn root(&self) -> &Tree {
        &self.root
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
        let shows_all = layout::shows_all(&self.root, area);
        if let (Fit::Clients(last), true) = (&mut self.fit, shows_all) {
            *last = size;
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

    /// The pane `client` focuses here.
    pub fn focus(&self, client: ClientId) -> Option<PaneId> {
        Some(self.seat(client)?.focus)
    }

    /// Focuses `pane` for `client`, if the tab holds it, remembering the
    /// pane it replaces.
    pub(crate) fn set_focus(&mut self, client: ClientId, pane: PaneId) {
        let holds = self.holds(pane);
        let seat = self.seats.get_mut(&client);
        if let Some(seat) = seat.filter(|s| holds && s.focus != pane) {
            (seat.last, seat.focus, seat.held) = (Some(seat.focus), pane, false);
        }
    }

    /// Changes the layout with `change`; each client then focuses the pane
    /// it did if the tab still holds it, else the one before, else the
    /// first.
    fn edit<R>(&mut self, change: impl FnOnce(&mut Tree) -> R) -> R {
        let out = change(&mut self.root);
        self.changed = true;
        self.refocus();
        out
    }

    fn refocus(&mut self) {
        let root = &self.root;
        let inside = |p: &PaneId| root.contains(*p);
        for seat in self.seats.values_mut() {
            let (focus, last) = (Some(seat.focus).filter(inside), seat.last.filter(inside));
            seat.held &= focus.is_some();
            seat.focus = focus.or(last).unwrap_or_else(|| root.first_pane());
            seat.last = last.filter(|l| *l != seat.focus);
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
                let first = tab.root.first_pane();
                tab.seats.entry(client).or_insert(Seat::on(first));
                tab.refocus();
            }
        }
    }

    /// Disabling bells clears only this client's pending tab marks.
    pub(crate) fn clear_bells(&mut self, client: ClientId) {
        for ws in &mut self.workspaces {
            for tab in &mut ws.tabs {
                if let Some(seat) = tab.seat_mut(client) {
                    seat.rang = false;
                }
            }
        }
    }

    /// A client detaches: nothing names it.
    pub(crate) fn forget(&mut self, client: ClientId) {
        for w in &mut self.workspaces {
            w.viewers.remove(&client);
            w.shown.remove(&client);
            for tab in &mut w.tabs {
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
                w.viewers.remove(&client);
            } else {
                w.viewers.insert(client);
                w.show(client, at);
            }
        }
    }

    /// Swaps tab `tab` with its sibling `toward` in its workspace: none if
    /// there is no such tab, false if it has no sibling that way.
    pub(crate) fn reorder_tab(&mut self, tab: TabId, toward: Sibling) -> Option<bool> {
        (self.workspaces.iter_mut())
            .find_map(|w| swap_sibling(&mut w.tabs, |t| t.id == tab, toward))
    }

    /// Swaps workspace `ws` with its sibling `toward`, as `reorder_tab`.
    pub(crate) fn reorder_workspace(&mut self, ws: WsId, toward: Sibling) -> Option<bool> {
        swap_sibling(&mut self.workspaces, |w| w.id == ws, toward)
    }

    /// Lays tab `tab` out on what `fit` makes of its fit from now on.
    pub(crate) fn set_fit(&mut self, tab: TabId, fit: impl FnOnce(Fit) -> Fit) -> Option<()> {
        let (_, tab) = tab_mut(&mut self.workspaces, tab)?;
        let fit = fit(tab.fit);
        tab.set_fit(fit);
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
        let at = self.locate(id)?;
        Some(PaneMut::new(
            self.panes.get_mut(&id)?,
            at,
            &mut self.journal,
        ))
    }

    /// Changes each pane with `change`, tab by tab: each at the place it
    /// is found, not looked for again.
    pub fn each_pane_mut(&mut self, mut change: impl FnMut(&mut PaneMut<'_>)) {
        let Parts {
            workspaces,
            panes,
            journal,
        } = self;
        for ws in workspaces.iter() {
            for tab in &ws.tabs {
                for id in tab.root.panes() {
                    if let Some(pane) = panes.get_mut(&id) {
                        change(&mut PaneMut::new(pane, (ws.id, tab.id), journal));
                    }
                }
            }
        }
    }

    /// Adds workspace `id` named `name`, its one tab `tab` named
    /// `tab_name` and laid out on `fit`, a seat there for each of
    /// `clients`, holding `pane`, new.
    pub(crate) fn add_workspace(
        &mut self,
        ws: (WsId, Option<String>),
        first: (TabId, Option<String>, Fit),
        clients: impl Iterator<Item = ClientId>,
        pane: Pane,
    ) {
        let at = (ws.0, first.0);
        self.push_workspace(ws, first, clients, pane.id());
        self.placed(at, pane);
    }

    /// `add_workspace`, holding pane `pane`, in the session.
    fn push_workspace(
        &mut self,
        (id, name): (WsId, Option<String>),
        (tab, tab_name, fit): (TabId, Option<String>, Fit),
        clients: impl Iterator<Item = ClientId>,
        pane: PaneId,
    ) {
        self.journal
            .push(Event::Workspace(id, Change::Created(name.clone())));
        self.journal
            .push(Event::Tab((id, tab), Change::Created(tab_name.clone())));
        let first = Tab::new(tab, tab_name, fit, clients, pane);
        let shown = first.seats.keys().map(|c| (*c, tab)).collect();
        self.workspaces.push(Workspace {
            id,
            name,
            tabs: vec![first],
            viewers: BTreeSet::new(),
            shown,
        });
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
        let workspace = self.workspaces.iter_mut().find(|w| w.id == ws)?;
        let clients = workspace.tabs.first().map(|t| t.seats.keys().copied());
        let tab = Tab::new(id, name.clone(), fit, clients.into_iter().flatten(), pane);
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
        let (_, tab) = self.locate(target).ok_or(NoPane)?;
        let mut tabs = (self.workspaces.iter()).flat_map(|w| &w.tabs);
        let was = tabs.find(|t| t.id == tab).map(|t| t.root.clone());
        let was = was.ok_or(NoPane)?;
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
        t.edit(|r| *r = root);
        self.placed((ws, tab), pane);
        Ok(())
    }

    /// Takes pane `id` out of its tab: given back, closed. A tab its going
    /// empties closes too, as does a workspace left with no tabs.
    pub(crate) fn close_pane(&mut self, id: PaneId) -> Option<Pane> {
        let at = self.locate(id)?;
        let (_, tab) = tab_mut(&mut self.workspaces, at.1)?;
        let emptied = match tab.root.remove(id) {
            Removed::Left(rest) => {
                tab.edit(|root| *root = rest);
                false
            }
            Removed::Emptied => true,
            Removed::Absent => return None,
        };
        self.journal.push(Event::Pane((at, id), PaneEvent::Closed));
        if emptied {
            self.drop_tab(at.1);
        }
        self.panes.remove(&id)
    }

    /// Removes tab `tab` and gives back the panes in it, closed. A
    /// workspace it leaves with none goes too, its viewers shown the first
    /// workspace left.
    pub(crate) fn remove_tab(&mut self, tab: TabId) -> Vec<Pane> {
        let Some((ws, t)) = tab_mut(&mut self.workspaces, tab) else {
            return Vec::new();
        };
        let panes = t.root.panes();
        for &pane in &panes {
            self.journal
                .push(Event::Pane(((ws, tab), pane), PaneEvent::Closed));
        }
        self.drop_tab(tab);
        (panes.into_iter())
            .filter_map(|pane| self.panes.remove(&pane))
            .collect()
    }

    /// Takes tab `tab` out of its workspace, and the workspace if it is
    /// left with none, its viewers shown the first workspace left.
    fn drop_tab(&mut self, tab: TabId) {
        let ws = (self.workspaces.iter_mut()).find(|w| w.tabs.iter().any(|t| t.id == tab));
        let Some(ws) = ws else {
            return;
        };
        ws.remove_tab(tab);
        let id = ws.id;
        let viewers = ws.tabs.is_empty().then(|| std::mem::take(&mut ws.viewers));
        self.journal.push(Event::Tab((id, tab), Change::Closed));
        if let Some(viewers) = viewers {
            self.workspaces.retain(|w| w.id != id);
            if let Some(first) = self.workspaces.first_mut() {
                first.viewers.extend(viewers);
            }
            self.journal.push(Event::Workspace(id, Change::Closed));
        }
    }

    /// Closes everything: the panes given back, to end.
    pub(crate) fn close_all(&mut self) -> Vec<Pane> {
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
        let (Some(was_a), Some(was_b)) = (self.locate(a), self.locate(b)) else {
            return;
        };
        // Only the tabs that hold them: another, laid out anew, would lose
        // what its panes have (a zoomed pane, its whole screen).
        let tabs = self.workspaces.iter_mut().flat_map(|w| w.tabs.iter_mut());
        for tab in tabs.filter(|t| t.id == was_a.1 || t.id == was_b.1) {
            tab.edit(|root| root.swap(a, b));
        }
        if was_a != was_b {
            (self.journal).push(Event::Pane((was_b, a), PaneEvent::Moved(was_a)));
            (self.journal).push(Event::Pane((was_a, b), PaneEvent::Moved(was_b)));
        }
    }

    /// Moves `pane` from its tab to `to`, another: its tab lets go of it,
    /// closing if it held it alone, as its workspace does if that leaves
    /// it none, and `to` takes it; no other tab changes. `None`, changing
    /// nothing, for a pane in no tab, its own tab, or a tab or workspace
    /// there is not.
    pub(crate) fn relocate(&mut self, pane: PaneId, to: Destination) -> Option<()> {
        let was = self.locate(pane)?;
        let (_, from) = tab_mut(&mut self.workspaces, was.1)?;
        let rest = match from.root.remove(pane) {
            Removed::Left(rest) => Some(rest),
            Removed::Emptied => None,
            Removed::Absent => return None,
        };
        let clients: Vec<ClientId> = from.seats.keys().copied().collect();
        // Taken in before its tab lets go: the workspace a new tab goes in
        // may be the one that would close.
        let at = match to {
            Destination::Tab(to) => {
                let (ws, tab) = tab_mut(&mut self.workspaces, to).filter(|_| to != was.1)?;
                tab.edit(|root| root.append(pane));
                (ws, to)
            }
            Destination::NewTab(ws, tab, fit) => {
                self.push_tab((ws, tab), None, fit, pane)?;
                (ws, tab)
            }
            Destination::NewWorkspace(ws, first) => {
                let at = (ws.0, first.0);
                self.push_workspace(ws, first, clients.into_iter(), pane);
                at
            }
        };
        (self.journal).push(Event::Pane((at, pane), PaneEvent::Moved(was)));
        match (rest, tab_mut(&mut self.workspaces, was.1)) {
            (Some(rest), Some((_, from))) => from.edit(|root| *root = rest),
            (None, _) | (_, None) => self.drop_tab(was.1),
        }
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
        let (_, tab) = self.locate(pane)?;
        let (_, tab) = tab_mut(&mut self.workspaces, tab)?;
        let sorted = |root: &Tree| {
            let mut panes = root.panes();
            panes.sort_unstable();
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

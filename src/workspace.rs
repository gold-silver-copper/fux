//! Workspaces, their tabs, and where each attached client is among them,
//! kept in what it names: a workspace holds the clients showing it, and each
//! tab a [`Seat`] for every client. Closing a tab takes its seats with it,
//! and its layout changes only through [`Tab::edit`], which keeps each focus
//! on one of its panes: no client names a tab or pane that is gone.
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::layout::{Rect, Tree};
use std::collections::{BTreeMap, BTreeSet};

/// A workspace and its tabs, of which it always has one or more: each is
/// made with one, and the session removes a workspace its last tab leaves.
pub struct Workspace {
    pub id: WsId,
    pub name: String,
    tabs: Vec<Tab>,
    /// The clients showing it: each attached client shows one workspace
    /// while there is any.
    pub(crate) viewers: BTreeSet<ClientId>,
    /// The tab each attached client is shown when it shows the workspace.
    shown: BTreeMap<ClientId, TabId>,
}

pub struct Tab {
    pub id: TabId,
    pub name: String,
    /// Its name was given (`name`, `rename`), not fux's (`main`, `tab-N`, a
    /// program's): a name given is the only one, and takes fux's.
    pub given: bool,
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

/// What a tab no client shows is laid out on. A client that shows it lays
/// it out on its own screen, whichever this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// A client's screen, any attached client's: with none attached, its
    /// panes keep their sizes.
    Clients,
    /// A screen of this size, client or none: `size` and `resize`.
    Fixed(fux_vt::Size),
}

/// What the tab at `place`, counted from 1, is named if it is given no
/// name and is not a workspace's first.
pub(crate) fn numbered(place: usize) -> String {
    format!("tab-{place}")
}

impl Fit {
    /// The area it lays a tab out in: a fixed size's, or `client`, a
    /// client's screen, if there is one.
    pub fn area(self, client: Option<Rect>) -> Option<Rect> {
        match self {
            Fit::Fixed(size) => Some(Rect::screen(size.rows(), size.cols())),
            Fit::Clients => client,
        }
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
    pub fn focus(&self) -> Option<PaneId> {
        self.focus
    }
    pub fn last(&self) -> Option<PaneId> {
        self.last
    }
}

impl Workspace {
    /// A workspace whose one tab, `tab` with its name, holds `root` and is
    /// shown to every attached client, `clients`.
    pub(crate) fn new(
        id: WsId,
        name: String,
        (tab, tab_name): (TabId, String),
        (root, fit): (Option<Tree>, Fit),
        clients: impl Iterator<Item = ClientId>,
    ) -> Workspace {
        let tab = Tab::new(tab, tab_name, (root, fit), clients);
        let shown = tab.seats.keys().map(|c| (*c, tab.id)).collect();
        let (tabs, viewers) = (vec![tab], BTreeSet::new());
        Workspace {
            id,
            name,
            tabs,
            viewers,
            shown,
        }
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

    /// Adds a tab, named after its place if no name is given, with a seat
    /// for every client that has one in the others.
    pub(crate) fn add_tab(&mut self, id: TabId, name: Option<String>, layout: (Option<Tree>, Fit)) {
        let number = self.tabs.len().saturating_add(1);
        let name = name.unwrap_or_else(|| numbered(number));
        let clients = self.tabs.first().map(|t| t.seats.keys().copied());
        let tab = Tab::new(id, name, layout, clients.into_iter().flatten());
        self.tabs.push(tab);
    }

    /// Shows `client` tab `tab` here, if given and one of this workspace's,
    /// and in it focuses `pane`, if given and there.
    pub(crate) fn show(&mut self, client: ClientId, tab: Option<TabId>, pane: Option<PaneId>) {
        let Some(tab) = tab.filter(|id| self.tabs.iter().any(|t| t.id == *id)) else {
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
    /// panes were sized, each with the area its fit lays it out in given
    /// `client`, any client's screen, for sizing them. Those are then taken
    /// as sized, as is a shown one, by its clients; one with no area yet
    /// waits for a client.
    pub(crate) fn changed_unshown(
        &mut self,
        client: Option<Rect>,
    ) -> impl Iterator<Item = (&Tree, Rect)> {
        let (viewers, shown_to) = (&self.viewers, &self.shown);
        self.tabs.iter_mut().filter_map(move |tab| {
            let shown = viewers.iter().any(|c| shown_to.get(c) == Some(&tab.id));
            let area = tab.fit.area(client).filter(|_| !shown);
            if area.is_some() || shown {
                let changed = std::mem::take(&mut tab.changed);
                let tab = &*tab;
                return tab.root.as_ref().filter(|_| changed).zip(area);
            }
            None
        })
    }

    /// Removes tab `tab`, giving back its layout: the clients it was shown
    /// to are shown its neighbour.
    pub(crate) fn remove_tab(&mut self, tab: TabId) -> Option<Tree> {
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
    fn new(
        id: TabId,
        name: String,
        (root, fit): (Option<Tree>, Fit),
        clients: impl Iterator<Item = ClientId>,
    ) -> Tab {
        let (seats, typist) = (clients.map(|c| (c, Seat::default())).collect(), None);
        let mut tab = Tab {
            id,
            name,
            given: false,
            root,
            changed: true,
            fit,
            seats,
            typist,
        };
        tab.refocus();
        tab
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
    pub(crate) fn edit<R>(&mut self, change: impl FnOnce(&mut Option<Tree>) -> R) -> R {
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

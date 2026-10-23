//! Workspaces, their tabs, and where each attached client is among them,
//! kept in what it names: a workspace holds the clients showing it, and each
//! tab a [`Seat`] for every client. Closing a tab takes its seats with it,
//! and its layout changes only through [`Tab::edit`], which keeps each focus
//! on one of its panes: no client names a tab or pane that is gone.
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::layout::{Layout, Rect, Tree};
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
    layout: Layout,
    /// Its layout changed since its panes were sized for it: one no client
    /// shows is then laid out again, and of one clients show, the floating
    /// panes none of them shows (`Session::size_panes`).
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
    /// A client's screen, any attached client's: with none attached, a
    /// typical terminal's (`session::TYPICAL`).
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
    /// client's screen.
    pub fn area(self, client: Rect) -> Rect {
        match self {
            Fit::Fixed(size) => Rect::screen(size.rows(), size.cols()),
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
    /// The pane it focused before it last changed layer, tiled or
    /// floating: the one `toggle-floating` goes back to while it is still
    /// in the other layer.
    away: Option<PaneId>,
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
    pub fn away(&self) -> Option<PaneId> {
        self.away
    }
}

impl Workspace {
    /// A workspace whose one tab is `tab`, shown to every client it seats.
    pub(crate) fn new(id: WsId, name: String, tab: Tab) -> Self {
        let shown = tab.seats.keys().map(|c| (*c, tab.id)).collect();
        Self {
            id,
            name,
            tabs: vec![tab],
            viewers: BTreeSet::new(),
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
    pub(crate) fn add_tab(
        &mut self,
        id: TabId,
        name: Option<String>,
        root: Option<Tree>,
        fit: Fit,
    ) {
        let number = self.tabs.len().saturating_add(1);
        let name = name.unwrap_or_else(|| numbered(number));
        let clients = self.tabs.first().map(|t| t.seats.keys().copied());
        let tab = Tab::new(id, name, root, fit, clients.into_iter().flatten());
        self.tabs.push(tab);
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

    /// Its tabs whose layouts changed since their panes were sized, each
    /// with whether a client shows it, for sizing them: they are then
    /// taken as sized.
    pub(crate) fn changed(&mut self) -> impl Iterator<Item = (TabId, bool)> {
        let (viewers, shown_to) = (&self.viewers, &self.shown);
        self.tabs.iter_mut().filter_map(move |tab| {
            let shown = viewers.iter().any(|c| shown_to.get(c) == Some(&tab.id));
            std::mem::take(&mut tab.changed).then_some((tab.id, shown))
        })
    }

    /// Removes tab `tab`, giving back its layout: the clients it was shown
    /// to are shown its neighbour.
    pub(crate) fn remove_tab(&mut self, tab: TabId) -> Option<Layout> {
        let index = self.tabs.iter().position(|t| t.id == tab)?;
        let layout = std::mem::take(&mut self.tabs.get_mut(index)?.layout);
        // Tab IDs are unique: this removes the tab at `index`.
        self.tabs.retain(|t| t.id != tab);
        let next = index.min(self.tabs.len().saturating_sub(1));
        if let Some(next) = self.tabs.get(next).map(|t| t.id) {
            self.shown
                .values_mut()
                .filter(|t| **t == tab)
                .for_each(|t| *t = next);
        }
        Some(layout)
    }

    /// A client attaches: it is shown the first tab, and focuses each tab's
    /// first pane.
    pub(crate) fn attach(&mut self, client: ClientId) {
        if let Some(first) = self.tabs.first() {
            self.shown.insert(client, first.id);
        }
        for tab in &mut self.tabs {
            tab.seats.entry(client).or_default();
            tab.refocus(|_| false);
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
    /// A tab holding `root`, laid out on `fit`, with a seat for each of
    /// `clients`.
    pub(crate) fn new(
        id: TabId,
        name: String,
        root: Option<Tree>,
        fit: Fit,
        clients: impl Iterator<Item = ClientId>,
    ) -> Tab {
        let mut tab = Tab {
            id,
            name,
            given: false,
            layout: Layout {
                tiled: root,
                ..Layout::default()
            },
            changed: true,
            fit,
            seats: clients.map(|c| (c, Seat::default())).collect(),
            typist: None,
        };
        tab.refocus(|_| false);
        tab
    }

    /// Its tiled panes' tree, unless it has none.
    pub fn root(&self) -> Option<&Tree> {
        self.layout.tiled.as_ref()
    }

    /// Its panes, tiled and floating.
    pub fn layout(&self) -> &Layout {
        &self.layout
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
        self.layout.contains(pane)
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
    /// pane it replaces, and it as the other layer's if it is.
    pub(crate) fn set_focus(&mut self, client: ClientId, pane: PaneId) {
        let layout = &self.layout;
        let (holds, floats) = (layout.contains(pane), layout.floats(pane));
        let seat = self.seats.get_mut(&client);
        if let Some(seat) = seat.filter(|s| holds && s.focus != Some(pane)) {
            if seat.focus.is_some_and(|f| layout.floats(f) != floats) {
                seat.away = seat.focus;
            }
            (seat.last, seat.focus, seat.held) = (seat.focus, Some(pane), false);
        }
    }

    /// Changes the layout with `change`; each client then focuses the pane
    /// it did if the tab still holds it, else, from a floating pane, the
    /// top one; else the one before (from a tiled pane, a tiled one), else
    /// the first.
    pub(crate) fn edit<R>(&mut self, change: impl FnOnce(&mut Layout) -> R) -> R {
        let floated: Vec<ClientId> = (self.seats.iter())
            .filter(|(_, s)| s.focus.is_some_and(|f| self.layout.floats(f)))
            .map(|(c, _)| *c)
            .collect();
        let out = change(&mut self.layout);
        self.changed = true;
        self.refocus(|c| floated.contains(c));
        out
    }

    /// Each client focuses a pane the tab holds; `floated` says which
    /// focused a floating pane before the change.
    fn refocus(&mut self, floated: impl Fn(&ClientId) -> bool) {
        let layout = &self.layout;
        let inside = |p: &PaneId| layout.contains(*p);
        for (client, seat) in &mut self.seats {
            let (focus, last) = (seat.focus.filter(inside), seat.last.filter(inside));
            // A client loses its focus to its own layer: from a floating
            // pane to the top one, from a tiled one to a tiled one.
            let floated = floated(client);
            let top = layout.top().filter(|_| floated);
            let back = last.filter(|l| floated || layout.tiles(*l));
            seat.held &= focus.is_some();
            seat.focus = focus.or(top).or(back).or_else(|| layout.first_pane());
            seat.last = last.filter(|l| Some(*l) != seat.focus);
            seat.away = seat.away.filter(inside);
        }
    }
}

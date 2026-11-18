//! Workspaces, their tabs and panes, and where each attached client is
//! among them. They are three tables, each keyed by id: a workspace lists
//! its tabs, a tab names its workspace, and a pane is in one tab's layout.
//! A workspace is a [`Stack`] of its tabs' ids, as the server is one of its
//! workspaces: a stack shows each client one of its children, its own, and
//! keeps the one it showed before. A tab keeps each client's focus in a
//! [`Seat`] the same way, and its layout's stacks each client's choice
//! there. A child's leaving repairs what each client is shown, and a tab's
//! layout changes only through [`Parts::edit`], which keeps each focus on
//! one of its panes, and shown: no client names a tab or pane that is gone.
//!
//! [`Parts`] holds them with the journal of what happened to them: what
//! events tell is changed only here, by methods that journal the change
//! as they make it, and by what each pane tells, which the session
//! journals at the pane's place as it updates its copy (`copy`).
use crate::copy::Copy as Pane;
use crate::journal::{Change, Close, Event, Journal, PaneEvent, ViewChange};
use fux_wire::command::{AnyRef, Sibling};
use fux_wire::events::Actor;
use fux_wire::id::{ClientId, PaneId, TabId, WsId};
use fux_wire::layout::{NoPane, Rect, Removed, Tree};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

pub use fux_wire::layout::Shown;

/// Children shown one at a time, by their ids: each attached client is
/// shown its own, while there is any.
#[derive(Clone)]
pub struct Stack<I> {
    children: Vec<I>,
    shown: BTreeMap<ClientId, Shown<I>>,
}

impl<I: Copy + Eq> Stack<I> {
    /// A stack of `child` alone, shown to each of `clients`.
    fn of(child: I, clients: &[ClientId]) -> Stack<I> {
        Stack {
            children: vec![child],
            shown: clients.iter().map(|c| (*c, Shown::on(child))).collect(),
        }
    }

    pub fn children(&self) -> &[I] {
        &self.children
    }

    pub fn holds(&self, id: I) -> bool {
        self.children.contains(&id)
    }

    /// What `client` is shown here.
    pub fn shown(&self, client: ClientId) -> Option<Shown<I>> {
        self.shown.get(&client).copied()
    }

    /// The child `client` is shown.
    pub fn current(&self, client: ClientId) -> Option<I> {
        Some(self.shown.get(&client)?.current).filter(|id| self.holds(*id))
    }

    /// Shows `client` child `id`, if it is one.
    fn show(&mut self, client: ClientId, id: I) {
        let child = self.holds(id);
        if let Some(shown) = self.shown.get_mut(&client).filter(|_| child) {
            shown.show(id);
        }
    }

    /// Takes child `id` out, if it is one: each client it was shown to is
    /// shown the one it was shown before, else its neighbour.
    fn remove(&mut self, id: I) -> bool {
        if !self.holds(id) {
            return false;
        }
        let was = std::mem::take(&mut self.children);
        self.children = was.iter().copied().filter(|c| *c != id).collect();
        for shown in self.shown.values_mut() {
            shown.keep(&was, |c| c != id, None);
        }
        self.shown.retain(|_, shown| shown.current != id);
        true
    }

    /// Swaps child `id` with its sibling `toward`: none if there is no such
    /// child, false if it has no sibling that way.
    fn reorder(&mut self, id: I, toward: Sibling) -> Option<bool> {
        let index = self.children.iter().position(|c| *c == id)?;
        let first = match toward {
            Sibling::Next => Some(index),
            Sibling::Previous => index.checked_sub(1),
        };
        let pair = first.and_then(|i| self.children.get_mut(i..i.checked_add(2)?));
        Some(pair.map(<[I]>::reverse).is_some())
    }
}

/// A workspace: its tabs, of which it always has one or more: each is made
/// with one, and closes as its last leaves.
#[derive(Clone)]
pub struct Workspace {
    /// Given as it is made, and never changed: its events' place.
    id: WsId,
    name: Option<Rc<str>>,
    tabs: Stack<TabId>,
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

    /// Its tabs, by their ids, and what each client is shown of them.
    pub fn stack(&self) -> &Stack<TabId> {
        &self.tabs
    }
}

#[derive(Clone)]
pub struct Tab {
    /// Given as it is made, and never changed: its events' place.
    id: TabId,
    /// The workspace it is in.
    ws: WsId,
    name: Option<Rc<str>>,
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

/// The size a tab stores, given or a client's, never made up, and whether
/// the clients showing it size it (`Session::effective`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// The smallest screen of the clients showing it, if every pane fits
    /// there; else this, the last that did, or the one it was made on.
    Clients(fux_vt::Size),
    /// Always this size, whatever shows it: `size` and `resize`.
    Fixed(fux_vt::Size),
}

impl Fit {
    /// Its stored size.
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
impl Tab {
    /// A tab holding `pane` alone, laid out on `fit`, with a seat for each
    /// of `clients`.
    fn new(
        (ws, id): (WsId, TabId),
        name: Option<Rc<str>>,
        fit: Fit,
        clients: &[ClientId],
        pane: PaneId,
    ) -> Tab {
        Tab {
            id,
            ws,
            name,
            root: Tree::Pane(pane),
            revision: Some(0),
            changed: true,
            fit,
            zoomed: None,
            seats: clients.iter().map(|c| (*c, Seat::on(pane))).collect(),
            typist: None,
        }
    }

    pub fn id(&self) -> TabId {
        self.id
    }

    /// The workspace it is in.
    pub fn workspace(&self) -> WsId {
        self.ws
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

    /// Its layout changed: a new revision, which it journals.
    fn revised(&mut self, journal: &mut Journal) {
        self.revision = self.revision.and_then(|n| n.checked_add(1));
        self.laid_out(journal);
    }

    /// Journals `laid_out`, at its revision.
    fn laid_out(&self, journal: &mut Journal) {
        journal.push(Event::Layout((self.ws, self.id), self.revision));
    }

    pub fn fit(&self) -> Fit {
        self.fit
    }

    /// Lays it out on what `fit` says from now on.
    fn set_fit(&mut self, journal: &mut Journal, fit: Fit) {
        if self.fit != fit {
            self.fit = fit;
            self.changed = true;
            self.revised(journal);
        }
    }

    /// It is laid out on `size`, its effective size: fitting clients,
    /// that is its stored size, the last that showed every pane, which a
    /// dump makes it at.
    fn shown_on(&mut self, journal: &mut Journal, size: fux_vt::Size) {
        if let Fit::Clients(last) = &mut self.fit
            && *last != size
        {
            *last = size;
            self.revised(journal);
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
    fn zoom(&mut self, journal: &mut Journal, pane: Option<PaneId>) {
        if self.zoomed != pane {
            for seat in self.seats.values_mut() {
                let focus = |zoomed: Option<PaneId>| zoomed.unwrap_or(seat.focus.current);
                seat.held &= focus(self.zoomed) == focus(pane);
            }
            self.zoomed = pane;
            self.changed = true;
            journal.push(Event::Zoomed((self.ws, self.id), pane));
        }
    }

    /// Focuses `pane` for `client`, if the tab holds it, remembering the
    /// pane it replaces. Another pane than the zoomed one ends the zoom.
    fn set_focus(&mut self, journal: &mut Journal, client: ClientId, pane: PaneId) {
        if !self.holds(pane) {
            return;
        }
        if self.zoomed.is_some_and(|z| z != pane) {
            self.zoom(journal, None);
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
    fn edit<R>(&mut self, journal: &mut Journal, change: impl FnOnce(&mut Tree) -> R) -> R {
        let was = self.root.panes();
        let out = change(&mut self.root);
        self.changed = true;
        if self.zoomed.is_some_and(|z| !self.root.contains(z)) {
            self.zoom(journal, None);
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
        self.revised(journal);
        out
    }
}
/// The session's workspaces, their tabs and panes, and the journal of what
/// happened to them: what events tell of them changes only by its methods,
/// each of which journals what it changes as it changes it.
pub struct Parts {
    /// The workspaces, in order.
    top: Stack<WsId>,
    workspaces: BTreeMap<WsId, Workspace>,
    tabs: BTreeMap<TabId, Tab>,
    /// Each in one tab: a pane is added in its place and leaves with it.
    panes: BTreeMap<PaneId, Pane>,
    /// The tab whose layout holds each pane: what `edit` leaves, which
    /// alone changes a layout.
    homes: BTreeMap<PaneId, TabId>,
    journal: Journal,
    /// The workspaces tabs closed in since the session last settled.
    closing: BTreeMap<WsId, Closing>,
    /// The clients a close moved since then, each with what to tell it
    /// (`take_displaced`).
    displaced: Vec<(ClientId, String)>,
}

/// A workspace tabs closed in (`Parts::closing`): how many, and, if it
/// closed too, its label and why.
#[derive(Default)]
struct Closing {
    tabs: usize,
    closed: Option<(String, Close)>,
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
    /// A new unnamed workspace `0`, its first tab `1` laid out on `2`.
    NewWorkspace(WsId, TabId, Fit),
}

/// Names `old` `new`, journaled as `renamed` makes it if it is another
/// name.
fn rename(
    journal: &mut Journal,
    renamed: impl FnOnce(Change) -> Event,
    old: &mut Option<Rc<str>>,
    new: String,
) {
    if old.as_deref() != Some(new.as_str()) {
        journal.push(renamed(Change::Renamed(new.clone())));
        *old = Some(new.into());
    }
}

/// A client's place, its current child in each node, and whether it is
/// shown each pane (`Parts::places`).
type Place = (
    Option<crate::journal::PaneAt>,
    BTreeMap<Option<AnyRef>, AnyRef>,
    BTreeMap<PaneId, bool>,
);

impl Parts {
    /// Nothing yet, its journal keeping `journal` bytes of events.
    pub fn new(journal: fux_wire::command::JournalBytes) -> Parts {
        Parts {
            top: Stack {
                children: Vec::new(),
                shown: BTreeMap::new(),
            },
            workspaces: BTreeMap::new(),
            tabs: BTreeMap::new(),
            panes: BTreeMap::new(),
            homes: BTreeMap::new(),
            journal: Journal::new(journal),
            closing: BTreeMap::new(),
            displaced: Vec::new(),
        }
    }

    /// The workspaces, in order.
    pub fn workspaces(&self) -> Vec<&Workspace> {
        (self.top.children.iter())
            .filter_map(|id| self.workspaces.get(id))
            .collect()
    }

    /// The workspaces, by their ids, and what each client is shown of them.
    pub fn stack(&self) -> &Stack<WsId> {
        &self.top
    }

    /// The tabs of workspace `ws`, or with none every tab, in order, each
    /// with its workspace.
    pub fn tabs_in(&self, ws: Option<WsId>) -> Vec<(WsId, &Tab)> {
        let all = || self.top.children.clone();
        (ws.map_or_else(all, |ws| vec![ws]).into_iter())
            .flat_map(|ws| self.tabs_of(ws))
            .map(|tab| (tab.ws, tab))
            .collect()
    }

    /// The tabs of workspace `ws`, in order.
    pub fn tabs_of(&self, ws: WsId) -> impl Iterator<Item = &Tab> {
        let tabs = self.workspaces.get(&ws).map(|ws| &ws.tabs.children);
        (tabs.into_iter().flatten()).filter_map(|tab| self.tabs.get(tab))
    }

    /// The tab `client` is shown in workspace `ws`.
    pub fn tab_of(&self, ws: WsId, client: ClientId) -> Option<&Tab> {
        self.tabs
            .get(&self.workspaces.get(&ws)?.tabs.current(client)?)
    }

    /// Workspace `id`.
    pub fn workspace(&self, id: WsId) -> Option<&Workspace> {
        self.workspaces.get(&id)
    }

    /// Tab `id`, and its workspace.
    pub fn tab(&self, id: TabId) -> Option<(WsId, &Tab)> {
        self.tabs.get(&id).map(|tab| (tab.ws, tab))
    }

    /// The clients moved out of a workspace that closed since this was last
    /// asked, by requests they did not make, each with what to tell it
    /// (`moved`).
    pub(crate) fn take_displaced(&mut self) -> Vec<(ClientId, String)> {
        self.closing.clear();
        std::mem::take(&mut self.displaced)
    }

    /// The workspace, tab and pane `target` is or is in, as far as it says.
    fn spot(&self, target: AnyRef) -> Option<(WsId, Option<TabId>, Option<PaneId>)> {
        match target {
            AnyRef::Pane(pane) => {
                let (ws, tab) = self.locate(pane)?;
                Some((ws, Some(tab), Some(pane)))
            }
            AnyRef::Tab(tab) => Some((self.tab(tab)?.0, Some(tab), None)),
            AnyRef::Workspace(ws) => {
                (self.workspaces.contains_key(&ws)).then_some((ws, None, None))
            }
        }
    }

    /// Where `client` is: the workspace holding the tab it is shown, the
    /// tab, and the pane it focuses in it.
    pub fn place(&self, client: ClientId) -> Option<crate::journal::PaneAt> {
        let (ws, tab) = self.shown(client)?;
        Some(((ws.id, tab.id), tab.focus(client)?))
    }

    /// The tab `client` is shown, and the workspace that holds it.
    pub fn shown(&self, client: ClientId) -> Option<(&Workspace, &Tab)> {
        let ws = self.workspaces.get(&self.top.current(client)?)?;
        Some((ws, self.tabs.get(&ws.tabs.current(client)?)?))
    }

    /// Tab `id`, to change what events do not tell: its bell marks, its
    /// fit. One tab at a time, and none is made but here, so none is put
    /// in another's place (`mem::swap`), its panes moved with no journal
    /// entry.
    pub(crate) fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.get_mut(&id)
    }

    /// Changes tab `tab`'s layout with `change` (`Tab::edit`), and has each
    /// pane it then holds as its own, and none it let go.
    fn edit<R>(&mut self, tab: TabId, change: impl FnOnce(&mut Tree) -> R) -> Option<R> {
        let t = self.tabs.get_mut(&tab)?;
        let was = t.root.panes();
        let out = t.edit(&mut self.journal, change);
        for pane in was {
            if self.homes.get(&pane) == Some(&tab) {
                self.homes.remove(&pane);
            }
        }
        for pane in t.root.panes() {
            self.homes.insert(pane, tab);
        }
        Some(out)
    }

    /// What `client` is shown of the workspaces, of each workspace's tabs
    /// and of each tab's panes, each workspace before what it holds.
    pub fn seats(&self, client: ClientId) -> Vec<fux_wire::inspect::Seat> {
        let seat = |within, shown: Option<Shown<AnyRef>>| {
            shown.map(|Shown { current, last }| fux_wire::inspect::Seat {
                within,
                current,
                last,
            })
        };
        let root = self.top.shown(client).map(|s| s.map(AnyRef::Workspace));
        let mut seats: Vec<_> = seat(None, root).into_iter().collect();
        for ws in self.workspaces() {
            seats.extend(seat(
                Some(AnyRef::Workspace(ws.id)),
                ws.tabs.shown(client).map(|s| s.map(AnyRef::Tab)),
            ));
            for tab in self.tabs_of(ws.id) {
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
        let before = self.places();
        let out = change(self);
        self.moved(before);
        out
    }

    /// Where each client is, its current child at every node, and what it
    /// is shown of each tab's panes.
    fn places(&self) -> BTreeMap<ClientId, Place> {
        let clients = self.top.shown.keys().copied();
        let currents = |c| {
            (self.seats(c).iter())
                .map(|s| (s.within, s.current))
                .collect()
        };
        let shown = |c| {
            let mut shown = BTreeMap::new();
            for tab in self.tabs.values() {
                let seen: BTreeSet<PaneId> = tab.root.shown_panes(c).into_iter().collect();
                shown.extend(tab.root.panes().into_iter().map(|p| (p, seen.contains(&p))));
            }
            shown
        };
        let place = |c| (self.place(c), currents(c), shown(c));
        clients.map(|c| (c, place(c))).collect()
    }

    /// Journals `shown` for each client whose place, current child in a
    /// node there before and after (not one made or closed), or whether it
    /// is shown a pane there before and after, is not as `before`: the
    /// place left and the one reached. One a workspace's close moved, by a
    /// request not its own, is `displaced`: told the workspace that closed,
    /// why, the tabs that closed in it, who closed it, and where it is now.
    fn moved(&mut self, before: BTreeMap<ClientId, Place>) {
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
            if let Some(was) = was.filter(|was| Some(*was) != now) {
                self.journal.push(shown(Some(was)));
            }
            self.journal.push(shown(now));
            let by = self.journal.current_actor();
            if by == Actor::Client(client) {
                continue;
            }
            let closing = was.and_then(|((ws, _), _)| self.closing.get(&ws));
            let closed = closing.and_then(|c| Some((c.closed.as_ref()?, c.tabs)));
            let (Some(((label, why), tabs)), Some(((here, _), _))) = (closed, now) else {
                continue;
            };
            let why = match why {
                Close::Killed => "killed",
                Close::Exited | Close::Emptied => "closed",
            };
            let by = match by {
                Actor::Client(client) => client.to_string(),
                Actor::Pane(pane, _) | Actor::Program(pane, _) => pane.to_string(),
                Actor::Outside => "a command outside fux".to_owned(),
                Actor::Server => "fux".to_owned(),
            };
            let tabs = match tabs {
                1 => "(tab 1)".to_owned(),
                n => format!("{n} tabs"),
            };
            let Some(here) = self.workspace(here).map(Workspace::label) else {
                continue;
            };
            let told =
                format!("{label} {why} by {by}: {tabs} closed · now {here} · C-b w g shows all");
            self.displaced.push((client, told));
        }
    }

    /// A client attaches, shown `at` and what holds it, and elsewhere each
    /// first child, with nothing shown before.
    pub(crate) fn attach(&mut self, client: ClientId, at: AnyRef) {
        let Some((top, at_tab, at_pane)) = self.spot(at) else {
            return;
        };
        self.viewing(|parts| {
            parts.top.shown.insert(client, Shown::on(top));
            for ws in parts.workspaces.values_mut() {
                let here = at_tab.filter(|_| ws.id == top);
                if let Some(current) = here.or(ws.tabs.children.first().copied()) {
                    ws.tabs.shown.insert(client, Shown::on(current));
                }
            }
            for tab in parts.tabs.values_mut() {
                let focus = at_pane.filter(|_| Some(tab.id) == at_tab);
                let seat = Seat::on(focus.unwrap_or(tab.root.first_pane()));
                tab.root.reveal(client, seat.focus.current);
                tab.seats.insert(client, seat);
                // Focusing another pane than the zoomed one ends the zoom.
                if let Some(focus) = focus.filter(|f| tab.zoomed.is_some_and(|z| z != *f)) {
                    tab.set_focus(&mut parts.journal, client, focus);
                }
            }
        });
    }

    /// Disabling bells clears only this client's pending tab marks.
    pub(crate) fn clear_bells(&mut self, client: ClientId) {
        for tab in self.tabs.values_mut() {
            if let Some(seat) = tab.seat_mut(client) {
                seat.rang = false;
            }
        }
    }

    /// A client detaches: nothing names it.
    pub(crate) fn forget(&mut self, client: ClientId) {
        self.top.shown.remove(&client);
        for ws in self.workspaces.values_mut() {
            ws.tabs.shown.remove(&client);
        }
        for tab in self.tabs.values_mut() {
            tab.seats.remove(&client);
            tab.root.forget(client);
            tab.typist = tab.typist.filter(|t| *t != client);
        }
    }

    /// Shows `client` `target`, a pane, tab or workspace, and what holds
    /// it; nothing if it is not there.
    pub(crate) fn show(&mut self, client: ClientId, target: AnyRef) {
        let Some((ws, tab, pane)) = self.spot(target) else {
            return;
        };
        self.viewing(|parts| {
            parts.top.show(client, ws);
            let Some(tab) = tab else {
                return;
            };
            if let Some(ws) = parts.workspaces.get_mut(&ws) {
                ws.tabs.show(client, tab);
            }
            if let (Some(pane), Some(tab)) = (pane, parts.tabs.get_mut(&tab)) {
                tab.set_focus(&mut parts.journal, client, pane);
            }
        });
    }

    /// Swaps `target`, a tab or workspace, with its sibling `toward`: none
    /// if there is no such one, false if it has no sibling that way.
    pub(crate) fn reorder(&mut self, target: AnyRef, toward: Sibling) -> Option<bool> {
        let (event, changed) = match target {
            AnyRef::Tab(tab) => {
                let (ws, _) = self.tab(tab)?;
                let changed = (self.workspaces.get_mut(&ws)?.tabs).reorder(tab, toward)?;
                (Event::Tab((ws, tab), Change::Reordered), changed)
            }
            AnyRef::Workspace(ws) => {
                let changed = self.top.reorder(ws, toward)?;
                (Event::Workspace(ws, Change::Reordered), changed)
            }
            AnyRef::Pane(_) => return None,
        };
        if changed {
            self.journal.push(event);
        }
        Some(changed)
    }

    pub(crate) fn typed(&mut self, id: TabId, client: ClientId) {
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.typist = Some(client);
        }
    }

    /// Journals `laid_out` for tab `id`, at its revision: it is laid out
    /// otherwise, on another size or host, its layout as it was.
    pub(crate) fn laid_out(&mut self, id: TabId) {
        if let Some(tab) = self.tabs.get(&id) {
            tab.laid_out(&mut self.journal);
        }
    }

    /// Tab `id` is laid out on `size`, its effective size (`Tab::shown_on`).
    pub(crate) fn shown_on(&mut self, id: TabId, size: fux_vt::Size) {
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.shown_on(&mut self.journal, size);
        }
    }

    /// Zooms `pane` in its tab, or, if it is zoomed, ends the zoom.
    pub(crate) fn zoom(&mut self, pane: PaneId) -> Option<()> {
        let (_, tab) = self.locate(pane)?;
        self.viewing(|parts| {
            let tab = parts.tabs.get_mut(&tab)?;
            let zoomed = (tab.zoomed != Some(pane)).then_some(pane);
            tab.zoom(&mut parts.journal, zoomed);
            Some(())
        })
    }

    pub(crate) fn set_fit(&mut self, tab: TabId, fit: impl FnOnce(Fit) -> Fit) -> Option<()> {
        let tab = self.tabs.get_mut(&tab)?;
        let fit = fit(tab.fit);
        tab.set_fit(&mut self.journal, fit);
        Some(())
    }

    /// The tabs whose layout changed since their panes were sized, for
    /// sizing them, in order; they are then taken as sized.
    pub(crate) fn take_changed(&mut self) -> Vec<TabId> {
        if !self.tabs.values().any(|tab| tab.changed) {
            return Vec::new();
        }
        let ordered: Vec<TabId> = (self.tabs_in(None).into_iter())
            .map(|(_, tab)| tab.id)
            .collect();
        let changed = |tab: &TabId| {
            (self.tabs.get_mut(tab)).is_some_and(|tab| std::mem::take(&mut tab.changed))
        };
        ordered.into_iter().filter(changed).collect()
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
        let tab = self.tabs.get(self.homes.get(&pane)?)?;
        Some((tab.ws, tab.id))
    }

    /// Pane `id`, to change as its pane tells (`Session::told`).
    pub(crate) fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.get_mut(&id)
    }

    /// Pane `id`, told it is at `at` and what it told there: journaled at
    /// its place, as `actor`, and its copy changed to match, in one step.
    pub(crate) fn told(
        &mut self,
        id: PaneId,
        mut told: crate::journal::Told,
        actor: Actor,
        counts: fux_wire::command::Counts,
    ) -> Option<()> {
        let at = self.locate(id)?;
        let pane = self.panes.get_mut(&id)?;
        pane.told(&mut told, counts);
        let event = Event::Pane((at, id), PaneEvent::Told(told));
        self.journal.push_from(event, actor);
        Some(())
    }

    /// Names pane `id` `name`: journaled, if it is another.
    pub(crate) fn rename_pane(&mut self, id: PaneId, name: String) -> Option<()> {
        let at = self.locate(id)?;
        let pane = self.panes.get_mut(&id)?;
        if pane.rename(&name) {
            let told = PaneEvent::Told(crate::journal::Told::Renamed(name));
            self.journal.push(Event::Pane((at, id), told));
        }
        Some(())
    }

    /// Adds workspace `id`, named `name`, holding one tab `first`, named
    /// and laid out as it says, holding `pane`, new.
    pub(crate) fn add_workspace(
        &mut self,
        (id, name): (WsId, Option<String>),
        first: (TabId, Option<String>, Fit),
        pane: Pane,
    ) {
        let at = (id, first.0);
        self.push_workspace((id, name), first, pane.id());
        self.placed(at, pane);
    }

    /// `add_workspace`, holding pane `pane`, in the session: each client
    /// is shown its tab, and focuses the pane there.
    fn push_workspace(
        &mut self,
        (id, name): (WsId, Option<String>),
        (tab, tab_name, fit): (TabId, Option<String>, Fit),
        pane: PaneId,
    ) {
        let clients: Vec<ClientId> = self.top.shown.keys().copied().collect();
        (self.journal).push(Event::Workspace(id, Change::Created(name.clone())));
        let ws = Workspace {
            id,
            name: name.map(Rc::from),
            tabs: Stack::of(tab, &clients),
        };
        self.workspaces.insert(id, ws);
        self.journal
            .push(Event::Tab((id, tab), Change::Created(tab_name.clone())));
        let name = tab_name.map(Rc::from);
        (self.tabs).insert(tab, Tab::new((id, tab), name, fit, &clients, pane));
        self.homes.insert(pane, tab);
        self.top.children.push(id);
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
        let clients: Vec<ClientId> = self.top.shown.keys().copied().collect();
        let workspace = self.workspaces.get_mut(&ws)?;
        workspace.tabs.children.push(id);
        let text = name.as_deref().map(Rc::from);
        (self.tabs).insert(id, Tab::new((ws, id), text, fit, &clients, pane));
        self.homes.insert(pane, id);
        self.journal
            .push(Event::Tab((ws, id), Change::Created(name)));
        Some(())
    }

    /// `pane`, new, is in tab `at` of its workspace.
    fn placed(&mut self, at: (WsId, TabId), pane: Pane) {
        let id = pane.id();
        let shell = matches!(pane.runs, crate::pane::Runs::Shell(_));
        let created = PaneEvent::Created { shell };
        self.journal.push(Event::Pane((at, id), created));
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
        let Some(ws) = (self.tabs.get(&tab))
            .filter(|t| planned && t.root == was)
            .map(|t| t.ws)
        else {
            return Err(pane);
        };
        self.placed((ws, tab), pane);
        self.edit(tab, |r| *r = root);
        Ok(())
    }

    /// Takes pane `id` out of its tab: given back, closed, `why` says.
    /// A tab its going empties closes too, as does a workspace that leaves
    /// empty.
    pub(crate) fn close_pane(&mut self, id: PaneId, why: Close) -> Option<Pane> {
        let at = self.locate(id)?;
        self.viewing(|parts| {
            let rest = match parts.tabs.get(&at.1)?.root.remove(id) {
                Removed::Left(rest) => Some(rest),
                Removed::Emptied => None,
                Removed::Absent => return None,
            };
            parts
                .journal
                .push(Event::Pane((at, id), PaneEvent::Closed(why)));
            match rest {
                Some(rest) => parts.edit(at.1, |root| *root = rest)?,
                None => parts.drop_tab(at.1, Close::Emptied),
            }
            parts.panes.remove(&id)
        })
    }

    /// Removes tab `tab`, killed, and gives back the panes in it, closed.
    /// A workspace it leaves empty goes too, emptied.
    pub(crate) fn remove_tab(&mut self, tab: TabId) -> Vec<Pane> {
        let Some((ws, t)) = self.tab(tab) else {
            return Vec::new();
        };
        let panes = t.root.panes();
        self.viewing(|parts| {
            for &pane in &panes {
                let closed = PaneEvent::Closed(Close::Killed);
                (parts.journal).push(Event::Pane(((ws, tab), pane), closed));
            }
            parts.drop_tab(tab, Close::Killed);
            (panes.into_iter())
                .filter_map(|pane| parts.panes.remove(&pane))
                .collect()
        })
    }

    /// Removes workspace `ws`, or with none every one, killed, and gives
    /// back the panes there, closed: tab by tab in order, each workspace as
    /// its last tab leaves. What each client is shown is compared once,
    /// before and after.
    pub(crate) fn remove_workspace(&mut self, ws: Option<WsId>) -> Vec<Pane> {
        let tabs: Vec<(WsId, TabId)> = (self.tabs_in(ws).into_iter())
            .map(|(ws, tab)| (ws, tab.id))
            .collect();
        for (ws, _) in &tabs {
            let closing = self.closing.entry(*ws).or_default();
            closing.tabs = closing.tabs.saturating_add(1);
        }
        self.viewing(|parts| {
            let mut closed = Vec::new();
            for (ws, tab) in tabs {
                let panes = (parts.tabs.get(&tab)).map_or_else(Vec::new, |t| t.root.panes());
                for &pane in &panes {
                    let gone = PaneEvent::Closed(Close::Killed);
                    (parts.journal).push(Event::Pane(((ws, tab), pane), gone));
                }
                if let Some(emptied) = parts.take_tab(tab, Close::Killed) {
                    parts.close_workspace(emptied, Close::Killed);
                }
                closed.extend(panes.iter().filter_map(|pane| parts.panes.remove(pane)));
            }
            closed
        })
    }

    /// Takes tab `tab` out of its workspace, closed `why`, and the workspace
    /// if that empties it: each client shown one is shown the one it was
    /// shown before, else the neighbour (`Stack::remove`).
    fn drop_tab(&mut self, tab: TabId, why: Close) {
        let Some(ws) = self.tabs.get(&tab).map(|tab| tab.ws) else {
            return;
        };
        let closing = self.closing.entry(ws).or_default();
        closing.tabs = closing.tabs.saturating_add(1);
        if let Some(emptied) = self.take_tab(tab, why) {
            self.close_workspace(emptied, Close::Emptied);
        }
    }

    /// Takes tab `tab` out of its workspace, closed `why`, its row gone
    /// and its panes no tab's: its workspace, if that leaves it empty.
    fn take_tab(&mut self, tab: TabId, why: Close) -> Option<WsId> {
        let gone = self.tabs.remove(&tab)?;
        let ws = gone.ws;
        for pane in gone.root.panes() {
            if self.homes.get(&pane) == Some(&tab) {
                self.homes.remove(&pane);
            }
        }
        let w = self.workspaces.get_mut(&ws)?;
        w.tabs.remove(tab);
        let emptied = w.tabs.children.is_empty();
        self.journal
            .push(Event::Tab((ws, tab), Change::Closed(why)));
        emptied.then_some(ws)
    }

    /// Workspace `ws`, emptied, closes, `why` says: each client shown it is
    /// shown the one it was shown before, else the neighbour.
    fn close_workspace(&mut self, ws: WsId, why: Close) {
        self.top.remove(ws);
        let Some(gone) = self.workspaces.remove(&ws) else {
            return;
        };
        self.closing.entry(ws).or_default().closed = Some((gone.label().into_owned(), why));
        (self.journal).push(Event::Workspace(ws, Change::Closed(why)));
    }

    /// Names workspace `id` `name`, if there is one.
    pub(crate) fn rename_workspace(&mut self, id: WsId, name: String) -> Option<()> {
        let ws = self.workspaces.get_mut(&id)?;
        let renamed = |change| Event::Workspace(id, change);
        rename(&mut self.journal, renamed, &mut ws.name, name);
        Some(())
    }

    /// Gives tab `id` the name `name`, if there is one.
    pub(crate) fn give_tab_name(&mut self, id: TabId, name: String) -> Option<()> {
        let tab = self.tabs.get_mut(&id)?;
        let ws = tab.ws;
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
                parts.edit(tab, |root| root.swap(a, b));
            }
            if was_a != was_b {
                (parts.journal).push(Event::Pane((was_b, a), PaneEvent::Moved(was_a)));
                (parts.journal).push(Event::Pane((was_a, b), PaneEvent::Moved(was_b)));
            }
        });
    }

    /// Moves `pane` from its tab to `to`, another: its tab lets go of it,
    /// closing if it held it alone, as a workspace does that leaves
    /// empty, and `to` takes it; no other tab changes. `None`, changing
    /// nothing, for a pane in no tab, its own tab, or a tab or workspace
    /// there is not.
    pub(crate) fn relocate(&mut self, pane: PaneId, to: Destination) -> Option<()> {
        let was = self.locate(pane)?;
        self.viewing(|parts| parts.relocated(pane, was, to))
    }

    fn relocated(&mut self, pane: PaneId, was: (WsId, TabId), to: Destination) -> Option<()> {
        let rest = match self.tabs.get(&was.1)?.root.remove(pane) {
            Removed::Left(rest) => Some(rest),
            Removed::Emptied => None,
            Removed::Absent => return None,
        };
        // Taken in before its tab lets go: the workspace a new tab goes in
        // may be the one that would close.
        let at = match to {
            Destination::Tab(to) => {
                let ws = self.tabs.get(&to).filter(|_| to != was.1)?.ws;
                self.edit(to, |root| root.append(pane))?;
                (ws, to)
            }
            Destination::NewTab(ws, tab, fit) => {
                self.push_tab((ws, tab), None, fit, pane)?;
                (ws, tab)
            }
            Destination::NewWorkspace(ws, tab, fit) => {
                self.push_workspace((ws, None), (tab, None, fit), pane);
                (ws, tab)
            }
        };
        (self.journal).push(Event::Pane((at, pane), PaneEvent::Moved(was)));
        match rest {
            Some(rest) => self.edit(was.1, |root| *root = rest)?,
            None => self.drop_tab(was.1, Close::Emptied),
        }
        Some(())
    }

    /// Commits a complete validated replacement and one layout invalidation.
    /// Each client is shown its focus, and the first child of every other
    /// stack.
    pub(crate) fn arrange(&mut self, id: TabId, root: Tree) -> Option<()> {
        self.viewing(|parts| parts.edit(id, |old| *old = root))
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
        let (_, id) = self.locate(pane)?;
        self.viewing(|parts| {
            let tab = parts.tabs.get(&id)?;
            let mut root = tab.root.clone();
            let out = change(&mut root);
            if !root.same_panes(&tab.root) {
                return None;
            }
            if root != tab.root {
                parts.edit(id, |old| *old = root)?;
            }
            Some(out)
        })
    }
}

impl Parts {
    /// Its tree, as its run's record holds it (`record`).
    pub(crate) fn record(&self) -> crate::record::Tree {
        let named = |text: &Option<Rc<str>>| text.as_deref().map(String::from);
        let workspaces = (self.workspaces().into_iter())
            .map(|ws| crate::record::Workspace {
                id: ws.id,
                name: named(&ws.name),
                tabs: ws.tabs.children.clone(),
            })
            .collect();
        let mut tabs = Vec::with_capacity(self.tabs.len());
        for tab in self.tabs.values() {
            let panes = (tab.root.panes().into_iter())
                .filter_map(|p| self.panes.get(&p))
                .map(|pane| pane.record())
                .collect();
            tabs.push(crate::record::Tab {
                id: tab.id,
                name: named(&tab.name),
                nodes: tab.root.nodes(),
                revision: tab.revision,
                fixed: matches!(tab.fit, Fit::Fixed(_)),
                size: tab.fit.size(),
                zoomed: tab.zoomed,
                panes,
            });
        }
        crate::record::Tree { workspaces, tabs }
    }

    /// The tree `tree` records (`record`), holding `panes`, its journal
    /// keeping `journal` bytes of events, no client shown it yet: as a
    /// server takes a run back. One that is no tree (a tab held twice or
    /// missing, a workspace with none) is refused.
    pub(crate) fn restore(
        journal: fux_wire::command::JournalBytes,
        tree: &crate::record::Tree,
        panes: BTreeMap<PaneId, Pane>,
    ) -> Result<Parts, fux_wire::code::Error> {
        use fux_wire::code::{Code, fail};
        let bad = |why: String| fail(Code::BadValue, format!("the run's record: {why}"));
        let named = |name: &Option<String>| name.as_deref().map(Rc::from);
        let mut parts = Parts::new(journal);
        // Where each tab is, as the workspace that holds it says, below.
        let mut homes: BTreeMap<TabId, WsId> = BTreeMap::new();
        for recorded in &tree.workspaces {
            let ws = Workspace {
                id: recorded.id,
                name: named(&recorded.name),
                tabs: Stack {
                    children: recorded.tabs.clone(),
                    shown: BTreeMap::new(),
                },
            };
            if recorded.tabs.is_empty() || parts.workspaces.insert(ws.id, ws).is_some() {
                return Err(bad(format!(
                    "{} holds nothing, or is there twice",
                    recorded.id
                )));
            }
            parts.top.children.push(recorded.id);
            for tab in &recorded.tabs {
                if homes.insert(*tab, recorded.id).is_some() {
                    return Err(bad(format!("{tab} in {} is held twice", recorded.id)));
                }
            }
        }
        for recorded in &tree.tabs {
            let ids: Vec<PaneId> = recorded.panes.iter().map(|p| p.id).collect();
            let fit = match recorded.fit_fixed() {
                true => Fit::Fixed(recorded.size),
                false => Fit::Clients(recorded.size),
            };
            let ws = (homes.remove(&recorded.id)).ok_or_else(|| {
                bad(format!(
                    "{} is in no workspace, or is there twice",
                    recorded.id
                ))
            })?;
            let tab = Tab {
                id: recorded.id,
                ws,
                name: named(&recorded.name),
                root: fux_wire::layout::from_nodes(&recorded.nodes, &ids)?,
                revision: recorded.revision,
                changed: true,
                fit,
                zoomed: recorded.zoomed,
                seats: BTreeMap::new(),
                typist: None,
            };
            if let Some(zoomed) = tab.zoomed.filter(|zoomed| !tab.root.contains(*zoomed)) {
                return Err(bad(format!(
                    "{} is zoomed on {zoomed}, which is not in it",
                    tab.id
                )));
            }
            for pane in ids {
                if parts.homes.insert(pane, tab.id).is_some() || !panes.contains_key(&pane) {
                    return Err(bad(format!("{pane} is in two tabs, or is not there")));
                }
            }
            parts.tabs.insert(tab.id, tab);
        }
        if let Some((tab, ws)) = homes.first_key_value() {
            return Err(bad(format!("{tab}, in {ws}, is not there")));
        }
        parts.panes = panes;
        Ok(parts)
    }
}

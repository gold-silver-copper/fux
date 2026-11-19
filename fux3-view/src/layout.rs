//! The layout: one plain file, `.view/layout` in the session directory,
//! shared by every viewer: workspaces, tabs, and per tab a tree of splits
//! with relative shares over panes named with their ids (`NAME@ID`). Every
//! change is one operation on the file as it is then, under the layout's
//! lock, written whole: a new file beside it, renamed over it, so a reader
//! sees the old file or the new one. A file that cannot be read is never
//! written over. The holders never read it.
//!
//! What is shown is derived: the stored layout, plus each pane that has no
//! tile of its own, put in the first tab whose filter it passes ("routed").
use fux3::Failure;
use fux3::session::check_name;
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

pub const FILE: &str = "layout";
const TEMP: &str = ".layout.new";
const LOCK: &str = "layout.lock";
const HEADER: &str = "fux3-layout 1";
/// The largest layout file read or written.
pub const MAX_BYTES: usize = 64 << 10;
const MAX_DEPTH: usize = 8;
const MAX_WORKSPACES: usize = 32;
const MAX_TABS: usize = 64;
const MAX_LEAVES: usize = 2 * fux3::session::MAX_PANES;
const MAX_TITLE: usize = 64;
const MAX_SHARE: u64 = 1_000_000;
/// How long a change waits for the layout's lock.
const LOCK_WAIT: Duration = Duration::from_secs(2);
/// What each part of a new split gets: shares are relative.
const SHARE: u64 = 1000;
/// The least share of a split a border move leaves on a side, in thousandths.
const LEAST: u64 = 50;
/// The id of the tab that holds routed panes no tab's filter takes, until
/// an edit stores it.
pub const OTHER: u32 = 0;

fn bad(why: impl Into<String>) -> Failure {
    Failure::new(why)
}

/// Which way a split runs: side by side, or stacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Columns,
    Rows,
}

/// A direction on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    pub fn parse(text: &str) -> Result<Dir, Failure> {
        match text {
            "left" => Ok(Dir::Left),
            "right" => Ok(Dir::Right),
            "up" => Ok(Dir::Up),
            "down" => Ok(Dir::Down),
            other => Err(bad(format!(
                "no direction {other:?}: left, right, up or down"
            ))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dir::Left => "left",
            Dir::Right => "right",
            Dir::Up => "up",
            Dir::Down => "down",
        }
    }

    fn axis(self) -> Axis {
        match self {
            Dir::Left | Dir::Right => Axis::Columns,
            Dir::Up | Dir::Down => Axis::Rows,
        }
    }

    fn forward(self) -> bool {
        matches!(self, Dir::Right | Dir::Down)
    }
}

/// Whether `id` is a pane id: 16 lowercase hex digits.
pub fn is_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One life of a pane: `NAME@ID`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneRef {
    pub name: String,
    pub id: String,
}

impl fmt::Display for PaneRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.id)
    }
}

impl PaneRef {
    pub fn parse(text: &str) -> Result<PaneRef, Failure> {
        match Target::parse(text)? {
            Target { name, id: Some(id) } => Ok(PaneRef { name, id }),
            Target { id: None, .. } => Err(bad(format!("{text:?} is not NAME@ID"))),
        }
    }
}

/// A pane as a command names it: `NAME`, or `NAME@ID` for one life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub id: Option<String>,
}

impl Target {
    pub fn parse(text: &str) -> Result<Target, Failure> {
        let (name, id) = match text.split_once('@') {
            None => (text, None),
            Some((_, id)) if !is_id(id) => {
                return Err(bad(format!("bad pane id in {text:?}: 16 hex digits")));
            }
            Some((name, id)) => (name, Some(id.to_owned())),
        };
        check_name(name)?;
        Ok(Target {
            name: name.to_owned(),
            id,
        })
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.id {
            Some(id) => write!(f, "{}@{id}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Leaf(PaneRef),
    /// Two or more parts, each with its share of the room.
    Split {
        axis: Axis,
        parts: Vec<(u64, Node)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub id: u32,
    pub title: String,
    /// Labels a routed pane must all carry to land here (`name` is the
    /// pane's name); none takes any.
    pub tests: Vec<(String, String)>,
    pub zoom: Option<PaneRef>,
    pub root: Option<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub id: u32,
    pub title: String,
    pub tabs: Vec<Tab>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Advanced by every write.
    pub version: u64,
    /// The next id a tab or workspace gets: ids are never reused.
    pub next: u32,
    pub workspaces: Vec<Workspace>,
}

/// A pane that exists, as routing needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Live {
    pub pane: PaneRef,
    pub labels: Vec<(String, String)>,
}

/// Whether a pane passes every test of a tab's filter.
fn matches(live: &Live, tests: &[(String, String)]) -> bool {
    tests.iter().all(|(key, value)| match key.as_str() {
        "name" => live.pane.name == *value,
        _ => live.labels.iter().any(|(k, v)| k == key && v == value),
    })
}

/// A filter's test, `KEY=VALUE`.
fn parse_test(text: &str) -> Result<(String, String), Failure> {
    match text.split_once('=') {
        Some((key, value)) if !key.is_empty() && check_title(text).is_ok() => {
            Ok((key.to_owned(), value.to_owned()))
        }
        _ => Err(bad(format!("{text:?}: a test is KEY=VALUE"))),
    }
}

impl Default for Layout {
    /// What a session without a layout file shows: one tab holding every pane.
    fn default() -> Layout {
        Layout {
            version: 0,
            next: 3,
            workspaces: vec![Workspace {
                id: 1,
                title: "main".to_owned(),
                tabs: vec![Tab::new(2, "all")],
            }],
        }
    }
}

impl Tab {
    fn new(id: u32, title: &str) -> Tab {
        Tab {
            id,
            title: title.to_owned(),
            tests: Vec::new(),
            zoom: None,
            root: None,
        }
    }

    /// Its panes, in order.
    pub fn leaves(&self) -> Vec<&PaneRef> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.leaves(&mut out);
        }
        out
    }

    pub fn holds(&self, pane: &PaneRef) -> bool {
        self.root.as_ref().is_some_and(|root| root.contains(pane))
    }

    /// The pane shown alone, if the tab is zoomed on one it holds.
    pub fn zoomed(&self) -> Option<&PaneRef> {
        self.zoom.as_ref().filter(|pane| self.holds(pane))
    }
}

impl Node {
    fn leaves<'a>(&'a self, out: &mut Vec<&'a PaneRef>) {
        match self {
            Node::Leaf(pane) => out.push(pane),
            Node::Split { parts, .. } => parts.iter().for_each(|(_, node)| node.leaves(out)),
        }
    }

    fn first(&self) -> Option<&PaneRef> {
        match self {
            Node::Leaf(pane) => Some(pane),
            Node::Split { parts, .. } => parts.first().and_then(|(_, node)| node.first()),
        }
    }

    fn last(&self) -> Option<&PaneRef> {
        match self {
            Node::Leaf(pane) => Some(pane),
            Node::Split { parts, .. } => parts.last().and_then(|(_, node)| node.last()),
        }
    }

    fn contains(&self, pane: &PaneRef) -> bool {
        match self {
            Node::Leaf(leaf) => leaf == pane,
            Node::Split { parts, .. } => parts.iter().any(|(_, node)| node.contains(pane)),
        }
    }

    /// This tree without `pane`'s leaf, its part's room shared among the
    /// rest; `None` if nothing is left.
    fn without(self, pane: &PaneRef) -> Option<Node> {
        match self {
            Node::Leaf(leaf) if leaf == *pane => None,
            Node::Leaf(leaf) => Some(Node::Leaf(leaf)),
            Node::Split { axis, parts } => {
                let mut kept: Vec<(u64, Node)> = (parts.into_iter())
                    .filter_map(|(share, node)| node.without(pane).map(|node| (share, node)))
                    .collect();
                match kept.len() {
                    0 => None,
                    1 => kept.pop().map(|(_, node)| node),
                    _ => Some(Node::Split { axis, parts: kept }),
                }
            }
        }
    }

    /// Renames every leaf `from` to `to`.
    fn rename(&mut self, from: &PaneRef, to: &PaneRef) {
        match self {
            Node::Leaf(leaf) if leaf == from => *leaf = to.clone(),
            Node::Leaf(_) => {}
            Node::Split { parts, .. } => {
                parts.iter_mut().for_each(|(_, node)| node.rename(from, to))
            }
        }
    }

    /// Puts `new` beside `target`'s leaf on side `dir`: in its split if
    /// that runs along `dir`'s axis, taking half of `target`'s share, else
    /// in a new split of the two. True if `target` was found.
    fn split(&mut self, target: &PaneRef, new: &PaneRef, dir: Dir) -> bool {
        let (axis, before) = (dir.axis(), !dir.forward());
        if let Node::Leaf(leaf) = self
            && leaf == target
        {
            let old = Node::Leaf(leaf.clone());
            let mut parts = vec![(SHARE, old), (SHARE, Node::Leaf(new.clone()))];
            if before {
                parts.reverse();
            }
            *self = Node::Split { axis, parts };
            return true;
        }
        let Node::Split { axis: own, parts } = self else {
            return false;
        };
        let at = parts
            .iter()
            .position(|(_, node)| matches!(node, Node::Leaf(leaf) if leaf == target));
        if let Some(at) = at
            && *own == axis
        {
            let mut out = Vec::with_capacity(parts.len().saturating_add(1));
            for (index, (share, node)) in std::mem::take(parts).into_iter().enumerate() {
                if index != at {
                    out.push((share, node));
                    continue;
                }
                let half = share.div_ceil(2).max(1);
                let rest = share.saturating_sub(half).max(1);
                let new = (rest, Node::Leaf(new.clone()));
                if before {
                    out.push(new);
                    out.push((half, node));
                } else {
                    out.push((half, node));
                    out.push(new);
                }
            }
            *parts = out;
            return true;
        }
        parts
            .iter_mut()
            .any(|(_, node)| node.split(target, new, dir))
    }

    /// Moves the border that `pane` has on side `dir` of the nearest split
    /// along that axis with a part there, else the one on its other side,
    /// by `cells` in `area`. True if a border moved.
    fn border(&mut self, pane: &PaneRef, dir: Dir, cells: u32, area: Rect) -> bool {
        [dir.forward(), !dir.forward()]
            .into_iter()
            .any(|forward| self.border_on(pane, dir, forward, cells, area))
    }

    /// Tries the border on one side (`forward`: right or down) of `pane`.
    fn border_on(
        &mut self,
        pane: &PaneRef,
        dir: Dir,
        forward: bool,
        cells: u32,
        area: Rect,
    ) -> bool {
        let Node::Split { axis, parts } = self else {
            return false;
        };
        let rects = part_rects(*axis, parts, area);
        let Some(holder) = parts.iter().position(|(_, node)| node.contains(pane)) else {
            return false;
        };
        // The nearest split first: inside the part that holds the pane.
        if let Some((_, node)) = parts.get_mut(holder) {
            let inner = rects.get(holder).copied().unwrap_or_default();
            if node.border_on(pane, dir, forward, cells, inner) {
                return true;
            }
        }
        if *axis != dir.axis() {
            return false;
        }
        let other = match forward {
            true => holder.checked_add(1).filter(|i| *i < parts.len()),
            false => holder.checked_sub(1),
        };
        let Some(other) = other else {
            return false;
        };
        // Toward `dir`: the part before the border grows when it moves
        // right or down.
        let (first, second) = (holder.min(other), holder.max(other));
        let total: u64 = parts.iter().map(|(share, _)| *share).sum();
        let room = match axis {
            Axis::Columns => area.w.saturating_sub(gaps(parts.len())),
            Axis::Rows => area.h.saturating_sub(gaps(parts.len())),
        };
        let delta = shares(cells, total, room);
        let share = |index: usize| parts.get(index).map_or(0, |(share, _)| *share);
        let (a, b) = (share(first), share(second));
        let least = total
            .saturating_mul(LEAST)
            .checked_div(1000)
            .unwrap_or(0)
            .max(1);
        let (a, b) = match dir.forward() {
            true => {
                let moved = delta.min(b.saturating_sub(least));
                (a.saturating_add(moved), b.saturating_sub(moved))
            }
            false => {
                let moved = delta.min(a.saturating_sub(least));
                (a.saturating_sub(moved), b.saturating_add(moved))
            }
        };
        for (index, value) in [(first, a), (second, b)] {
            if let Some((share, _)) = parts.get_mut(index) {
                *share = value.max(1);
            }
        }
        true
    }

    /// Adds `node` as a new part at the end: of this split, taking an
    /// average share, or beside this leaf.
    fn attach(self, node: Node) -> Node {
        match self {
            Node::Split { axis, mut parts } => {
                let count = u64::try_from(parts.len()).unwrap_or(1).max(1);
                let total: u64 = parts.iter().map(|(share, _)| *share).sum();
                let share = total.checked_div(count).unwrap_or(SHARE).max(1);
                parts.push((share, node));
                Node::Split { axis, parts }
            }
            leaf @ Node::Leaf(_) => Node::Split {
                axis: Axis::Columns,
                parts: vec![(SHARE, leaf), (SHARE, node)],
            },
        }
    }
}

/// How far a border moves: `N` cells as seen in an area.
fn parse_cells(text: &str) -> Result<u32, Failure> {
    (text.parse::<u32>().ok())
        .filter(|n| (1..=10_000).contains(n))
        .ok_or_else(|| bad(format!("{text:?}: a number of cells, 1 to 10000")))
}

/// `cells` in shares of a split whose shares add to `total`, `room` long:
/// rounded up, so a cell asked for is at least a cell moved.
fn shares(cells: u32, total: u64, room: u32) -> u64 {
    (total
        .saturating_mul(cells.into())
        .div_ceil(room.max(1).into()))
    .max(1)
}

/// A rectangle of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.w)
            && y < self.y.saturating_add(self.h)
    }
}

/// The cells a split gives to the borders between its parts.
fn gaps(parts: usize) -> u32 {
    u32::try_from(parts.saturating_sub(1)).unwrap_or(u32::MAX)
}

/// Each part's rectangle, with a column (side by side) or a row (stacked)
/// between two parts for their border.
fn part_rects(axis: Axis, parts: &[(u64, Node)], area: Rect) -> Vec<Rect> {
    let total: u64 = parts.iter().map(|(share, _)| *share).sum();
    let room = match axis {
        Axis::Columns => area.w.saturating_sub(gaps(parts.len())),
        Axis::Rows => area.h.saturating_sub(gaps(parts.len())),
    };
    let mut out = Vec::with_capacity(parts.len());
    let (mut sum, mut start) = (0u64, 0u32);
    for (index, (share, _)) in parts.iter().enumerate() {
        sum = sum.saturating_add(*share);
        let end = u64::from(room)
            .saturating_mul(sum)
            .checked_div(total)
            .and_then(|end| u32::try_from(end).ok())
            .unwrap_or(room);
        let length = end.saturating_sub(start);
        let gap = u32::try_from(index).unwrap_or(u32::MAX);
        out.push(match axis {
            Axis::Columns => Rect {
                x: area.x.saturating_add(start).saturating_add(gap),
                w: length,
                ..area
            },
            Axis::Rows => Rect {
                y: area.y.saturating_add(start).saturating_add(gap),
                h: length,
                ..area
            },
        });
        start = end;
    }
    out
}

fn place(node: &Node, area: Rect, out: &mut Vec<(PaneRef, Rect)>) {
    match node {
        Node::Leaf(pane) => out.push((pane.clone(), area)),
        Node::Split { axis, parts } => {
            for ((_, node), rect) in parts.iter().zip(part_rects(*axis, parts, area)) {
                place(node, rect, out);
            }
        }
    }
}

/// A border a mouse can drag: between the part ending in `before` and the
/// part starting with `after`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Border {
    pub axis: Axis,
    /// The column (side by side) or row (stacked) it is drawn on.
    pub at: u32,
    /// Where it runs along the other axis.
    pub from: u32,
    pub length: u32,
    pub before: PaneRef,
    pub after: PaneRef,
}

impl Border {
    /// Whether the cell `(x, y)` is on it.
    pub fn holds(&self, x: u32, y: u32) -> bool {
        let along = self.from..self.from.saturating_add(self.length);
        match self.axis {
            Axis::Columns => x == self.at && along.contains(&y),
            Axis::Rows => y == self.at && along.contains(&x),
        }
    }
}

fn borders_of(node: &Node, area: Rect, out: &mut Vec<Border>) {
    let Node::Split { axis, parts } = node else {
        return;
    };
    let rects = part_rects(*axis, parts, area);
    for (index, ((_, node), rect)) in parts.iter().zip(&rects).enumerate() {
        borders_of(node, *rect, out);
        let next = index.checked_add(1).and_then(|i| parts.get(i));
        let (Some(before), Some(after)) = (node.last(), next.and_then(|(_, n)| n.first())) else {
            continue;
        };
        let (at, from, length) = match axis {
            Axis::Columns => (rect.x.saturating_add(rect.w), area.y, area.h),
            Axis::Rows => (rect.y.saturating_add(rect.h), area.x, area.w),
        };
        out.push(Border {
            axis: *axis,
            at,
            from,
            length,
            before: before.clone(),
            after: after.clone(),
        });
    }
}

impl Tab {
    /// Where each pane is drawn in `area`: the zoomed pane alone, if any.
    pub fn tiles(&self, area: Rect) -> Vec<(PaneRef, Rect)> {
        if let Some(zoom) = self.zoomed() {
            return vec![(zoom.clone(), area)];
        }
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            place(root, area, &mut out);
        }
        out
    }

    /// The borders a mouse can drag in `area`: none while zoomed.
    pub fn borders(&self, area: Rect) -> Vec<Border> {
        let mut out = Vec::new();
        if let (Some(root), None) = (&self.root, self.zoomed()) {
            borders_of(root, area, &mut out);
        }
        out
    }

    /// The pane beside `pane` toward `dir` in `area`: the nearest one there
    /// that shares the most of its edge.
    pub fn neighbor(&self, pane: &PaneRef, dir: Dir, area: Rect) -> Option<PaneRef> {
        let tiles = self.tiles(area);
        let (_, from) = tiles.iter().find(|(p, _)| p == pane)?;
        let overlap = |a0: u32, a1: u32, b0: u32, b1: u32| a1.min(b1).saturating_sub(a0.max(b0));
        let end = |start: u32, length: u32| start.saturating_add(length);
        let across = |r: &Rect| overlap(r.y, end(r.y, r.h), from.y, end(from.y, from.h));
        let along = |r: &Rect| overlap(r.x, end(r.x, r.w), from.x, end(from.x, from.w));
        (tiles.iter())
            .filter(|(p, _)| p != pane)
            .filter_map(|(p, r)| {
                let (gap, shared) = match dir {
                    Dir::Left if end(r.x, r.w) <= from.x => {
                        (from.x.saturating_sub(end(r.x, r.w)), across(r))
                    }
                    Dir::Right if r.x >= end(from.x, from.w) => {
                        (r.x.saturating_sub(end(from.x, from.w)), across(r))
                    }
                    Dir::Up if end(r.y, r.h) <= from.y => {
                        (from.y.saturating_sub(end(r.y, r.h)), along(r))
                    }
                    Dir::Down if r.y >= end(from.y, from.h) => {
                        (r.y.saturating_sub(end(from.y, from.h)), along(r))
                    }
                    Dir::Left | Dir::Right | Dir::Up | Dir::Down => return None,
                };
                (shared > 0).then_some((gap, std::cmp::Reverse(shared), p))
            })
            .min_by_key(|(gap, shared, _)| (*gap, *shared))
            .map(|(_, _, p)| p.clone())
    }
}

/// `panes` in a grid: about as many columns as rows, each column stacked.
fn grid(panes: &[PaneRef]) -> Option<Node> {
    let count = panes.len();
    let mut columns = 1usize;
    while columns.saturating_mul(columns) < count {
        columns = columns.saturating_add(1);
    }
    let per = count.div_ceil(columns).max(1);
    let joined = |axis: Axis, mut parts: Vec<(u64, Node)>| match parts.len() {
        0 => None,
        1 => parts.pop().map(|(_, node)| node),
        _ => Some(Node::Split { axis, parts }),
    };
    let mut parts = Vec::new();
    let mut rest = panes;
    while !rest.is_empty() {
        let (column, after) = rest.split_at_checked(per).unwrap_or((rest, &[]));
        rest = after;
        let rows = column
            .iter()
            .map(|pane| (SHARE, Node::Leaf(pane.clone())))
            .collect();
        parts.extend(joined(Axis::Rows, rows).map(|node| (SHARE, node)));
    }
    joined(Axis::Columns, parts)
}

impl Layout {
    pub fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.workspaces.iter().flat_map(|space| space.tabs.iter())
    }

    fn tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.workspaces
            .iter_mut()
            .flat_map(|space| space.tabs.iter_mut())
    }

    pub fn tab(&self, id: u32) -> Option<&Tab> {
        self.tabs().find(|tab| tab.id == id)
    }

    fn tab_mut(&mut self, id: u32) -> Option<&mut Tab> {
        self.tabs_mut().find(|tab| tab.id == id)
    }

    /// The tab holding `pane`'s tile.
    pub fn tab_of(&self, pane: &PaneRef) -> Option<&Tab> {
        self.tabs().find(|tab| tab.holds(pane))
    }

    /// The workspace holding tab `id`.
    pub fn workspace_of(&self, tab: u32) -> Option<&Workspace> {
        (self.workspaces.iter()).find(|space| space.tabs.iter().any(|t| t.id == tab))
    }

    fn placed(&self) -> Vec<PaneRef> {
        self.tabs()
            .flat_map(|tab| tab.leaves().into_iter().cloned())
            .collect()
    }

    fn take_id(&mut self) -> Result<u32, Failure> {
        let id = self.next;
        self.next =
            (self.next.checked_add(1)).ok_or_else(|| bad("no ids left: reset the layout"))?;
        Ok(id)
    }

    /// What is shown: this layout with every live pane that has no tile
    /// put in the first tab, in order, whose filter it passes, or else in a
    /// tab `other` at the end of the first workspace, as a grid.
    pub fn shown(&self, live: &[Live]) -> Layout {
        let placed = self.placed();
        let mut sorted: Vec<&Live> = live.iter().filter(|l| !placed.contains(&l.pane)).collect();
        sorted.sort_by(|a, b| a.pane.cmp(&b.pane));
        let mut routed: Vec<(u32, Vec<PaneRef>)> = Vec::new();
        for pane in sorted {
            let tab =
                (self.tabs().find(|tab| matches(pane, &tab.tests))).map_or(OTHER, |tab| tab.id);
            match routed.iter_mut().find(|(id, _)| *id == tab) {
                Some((_, list)) => list.push(pane.pane.clone()),
                None => routed.push((tab, vec![pane.pane.clone()])),
            }
        }
        let mut shown = self.clone();
        for (id, panes) in routed {
            if id == OTHER {
                let mut tab = Tab::new(OTHER, "other");
                tab.root = grid(&panes);
                if let Some(space) = shown.workspaces.first_mut() {
                    space.tabs.push(tab);
                }
            } else if let Some(tab) = shown.tab_mut(id) {
                tab.root = match (tab.root.take(), grid(&panes)) {
                    (Some(root), Some(more)) => Some(root.attach(more)),
                    (root, more) => root.or(more),
                };
            }
        }
        shown
    }

    /// Where each shown pane is, in `area`: its tab and its rectangle.
    fn rects(&self, area: Rect) -> Vec<(PaneRef, Rect)> {
        self.tabs().flat_map(|tab| tab.tiles(area)).collect()
    }
}

/// The panes whose tile moved or changed size from `before` to `after`, as
/// shown in `area`, and which still have one.
pub fn changed(before: &Layout, after: &Layout, area: Rect) -> Vec<PaneRef> {
    let old = before.rects(area);
    (after.rects(area).into_iter())
        .filter(|tile| !old.contains(tile))
        .map(|(pane, _)| pane)
        .collect()
}

// ---- The file's text ----

fn quoted(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
}

fn write_node(node: &Node, depth: usize, share: Option<u64>, out: &mut String) {
    out.extend(std::iter::repeat_n("  ", depth));
    if let Some(share) = share {
        out.push_str(&format!("{share} "));
    }
    match node {
        Node::Leaf(pane) => out.push_str(&format!("pane {pane}\n")),
        Node::Split { axis, parts } => {
            let axis = match axis {
                Axis::Columns => "columns",
                Axis::Rows => "rows",
            };
            out.push_str(&format!("split {axis}\n"));
            for (share, part) in parts {
                write_node(part, depth.saturating_add(1), Some(*share), out);
            }
        }
    }
}

impl fmt::Display for Layout {
    /// The file's text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = format!("{HEADER}\nversion {}\nnext {}\n", self.version, self.next);
        for space in &self.workspaces {
            out.push_str(&format!("workspace w{} ", space.id));
            quoted(&space.title, &mut out);
            out.push('\n');
            for tab in &space.tabs {
                out.push_str(&format!("tab t{} ", tab.id));
                quoted(&tab.title, &mut out);
                out.push('\n');
                if !tab.tests.is_empty() {
                    out.push_str("  match");
                    for (key, value) in &tab.tests {
                        out.push(' ');
                        quoted(&format!("{key}={value}"), &mut out);
                    }
                    out.push('\n');
                }
                if let Some(zoom) = &tab.zoom {
                    out.push_str(&format!("  zoom {zoom}\n"));
                }
                if let Some(root) = &tab.root {
                    write_node(root, 1, None, &mut out);
                }
            }
        }
        f.write_str(&out)
    }
}

/// A line's words: bare words, and `"…"` strings with `\"` and `\\`; each
/// says whether it was quoted.
pub fn words(line: &str) -> Result<Vec<(String, bool)>, String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == ' ' {
            chars.next();
            continue;
        }
        if c != '"' {
            let mut word = String::new();
            while let Some(&c) = chars.peek().filter(|c| **c != ' ') {
                if c == '"' {
                    return Err("a quote inside a word".to_owned());
                }
                word.push(c);
                chars.next();
            }
            out.push((word, false));
            continue;
        }
        chars.next();
        let mut text = String::new();
        loop {
            match chars.next() {
                Some('"') => break,
                Some('\\') => match chars.next() {
                    Some(c @ ('"' | '\\')) => text.push(c),
                    _ => return Err("only \\\" and \\\\ are escapes".to_owned()),
                },
                Some(c) => text.push(c),
                None => return Err("a string is not closed".to_owned()),
            }
        }
        if chars.peek().is_some_and(|c| *c != ' ') {
            return Err("text right after a string".to_owned());
        }
        out.push((text, true));
    }
    Ok(out)
}

fn title(word: Option<&(String, bool)>) -> Result<String, String> {
    match word {
        Some((text, true)) => check_title(text).map(|()| text.clone()),
        _ => Err("a title is a \"string\"".to_owned()),
    }
}

/// Refuses a title that is too long or holds a control character.
pub fn check_title(text: &str) -> Result<(), String> {
    if text.len() > MAX_TITLE {
        return Err(format!("a title is at most {MAX_TITLE} bytes"));
    }
    if text.chars().any(char::is_control) {
        return Err("a title holds no control characters".to_owned());
    }
    Ok(())
}

fn id(word: Option<&(String, bool)>, prefix: char) -> Result<u32, String> {
    word.filter(|(_, quoted)| !quoted)
        .and_then(|(text, _)| text.strip_prefix(prefix))
        .and_then(|digits| digits.parse::<u32>().ok())
        .filter(|id| *id != OTHER)
        .ok_or_else(|| format!("expected an id {prefix}N"))
}

/// One tree line: its depth, share and node head.
struct TreeLine {
    depth: usize,
    share: Option<u64>,
    words: Vec<(String, bool)>,
    number: usize,
}

fn parse_node(lines: &[TreeLine], at: &mut usize, depth: usize) -> Result<Node, String> {
    let line = lines.get(*at).ok_or("a split ends early")?;
    let here = |why: &str| format!("line {}: {why}", line.number);
    if line.depth != depth {
        return Err(here("wrong indentation"));
    }
    if depth > MAX_DEPTH {
        return Err(here(&format!("splits nest at most {MAX_DEPTH} deep")));
    }
    *at = at.saturating_add(1);
    let head: Vec<&str> = line.words.iter().map(|(w, _)| w.as_str()).collect();
    match head.as_slice() {
        ["pane", pane] => Ok(Node::Leaf(PaneRef::parse(pane).map_err(|f| here(&f.0))?)),
        ["split", axis] => {
            let axis = match *axis {
                "columns" => Axis::Columns,
                "rows" => Axis::Rows,
                _ => return Err(here("an axis is columns or rows")),
            };
            let mut parts = Vec::new();
            while lines.get(*at).is_some_and(|next| next.depth > depth) {
                let share = lines.get(*at).and_then(|next| next.share);
                let share =
                    share.ok_or_else(|| here("each part of a split starts with its share"))?;
                parts.push((share, parse_node(lines, at, depth.saturating_add(1))?));
            }
            if parts.len() < 2 {
                return Err(here("a split has two parts or more"));
            }
            Ok(Node::Split { axis, parts })
        }
        _ => Err(here("expected pane NAME@ID or split AXIS")),
    }
}

/// Reads a layout file's text. Every fault is refused, naming its line.
pub fn parse(text: &str) -> Result<Layout, String> {
    if text.len() > MAX_BYTES {
        return Err(format!("{} bytes, at most {MAX_BYTES}", text.len()));
    }
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(i, l)| (i.saturating_add(1), l));
    if lines.next().map(|(_, l)| l) != Some(HEADER) {
        return Err(format!("line 1: expected {HEADER:?}"));
    }
    let mut number_line = |key: &str| -> Result<u64, String> {
        let (n, line) = lines.next().ok_or(format!("no {key} line"))?;
        (line.strip_prefix(key))
            .and_then(|rest| rest.strip_prefix(' '))
            .and_then(|digits| digits.parse::<u64>().ok())
            .ok_or_else(|| format!("line {n}: expected {key} N"))
    };
    let version = number_line("version")?;
    let next = u32::try_from(number_line("next")?).map_err(|_| "next is too large".to_owned())?;
    let mut layout = Layout {
        version,
        next,
        workspaces: Vec::new(),
    };
    let mut tree: Vec<TreeLine> = Vec::new();
    let finish_tab = |layout: &mut Layout, tree: &mut Vec<TreeLine>| -> Result<(), String> {
        if tree.is_empty() {
            return Ok(());
        }
        let mut at = 0;
        let root = parse_node(tree, &mut at, 1)?;
        if let Some(extra) = tree.get(at) {
            return Err(format!("line {}: a tab has one tree", extra.number));
        }
        tree.clear();
        match (layout.workspaces.last_mut()).and_then(|space| space.tabs.last_mut()) {
            Some(tab) => tab.root = Some(root),
            None => return Err("a tree outside a tab".to_owned()),
        }
        Ok(())
    };
    for (number, line) in lines {
        let here = |why: &str| format!("line {number}: {why}");
        if line.trim().is_empty() {
            return Err(here("empty line"));
        }
        let body = line.trim_start_matches(' ');
        let indent = line.len().saturating_sub(body.len());
        if indent % 2 != 0 {
            return Err(here("indentation is two spaces a level"));
        }
        let depth = indent / 2;
        let words = words(body).map_err(|why| here(&why))?;
        let first = words.first().map(|(w, _)| w.as_str());
        if depth == 0 {
            finish_tab(&mut layout, &mut tree)?;
            match first {
                Some("workspace") if words.len() == 3 => {
                    let id = id(words.get(1), 'w').map_err(|why| here(&why))?;
                    let title = title(words.get(2)).map_err(|why| here(&why))?;
                    layout.workspaces.push(Workspace {
                        id,
                        title,
                        tabs: Vec::new(),
                    });
                }
                Some("tab") if words.len() == 3 => {
                    let id = id(words.get(1), 't').map_err(|why| here(&why))?;
                    let title = title(words.get(2)).map_err(|why| here(&why))?;
                    let space = layout
                        .workspaces
                        .last_mut()
                        .ok_or(here("a tab before any workspace"))?;
                    space.tabs.push(Tab::new(id, &title));
                }
                _ => return Err(here("expected workspace wN \"TITLE\" or tab tN \"TITLE\"")),
            }
            continue;
        }
        let tab = (layout.workspaces.last_mut())
            .and_then(|space| space.tabs.last_mut())
            .ok_or(here("indented text outside a tab"))?;
        let in_tree = !tree.is_empty();
        match (depth, first) {
            (1, Some("match")) if !in_tree && tab.tests.is_empty() && tab.zoom.is_none() => {
                for (word, quoted) in words.iter().skip(1) {
                    if !quoted {
                        return Err(here("a test is a \"key=value\" string"));
                    }
                    tab.tests.push(parse_test(word).map_err(|f| here(&f.0))?);
                }
                if tab.tests.is_empty() {
                    return Err(here("match needs a test"));
                }
            }
            (1, Some("zoom")) if !in_tree && tab.zoom.is_none() && words.len() == 2 => {
                let pane = words.get(1).map_or("", |(w, _)| w.as_str());
                tab.zoom = Some(PaneRef::parse(pane).map_err(|f| here(&f.0))?);
            }
            (1, _) if !in_tree && tab.root.is_none() => tree.push(TreeLine {
                depth,
                share: None,
                words,
                number,
            }),
            (_, Some(share)) if depth > 1 && in_tree => {
                let share = (share.parse::<u64>().ok())
                    .filter(|s| (1..=MAX_SHARE).contains(s))
                    .ok_or(here(&format!("a share is 1 to {MAX_SHARE}")))?;
                tree.push(TreeLine {
                    depth,
                    share: Some(share),
                    words: words.into_iter().skip(1).collect(),
                    number,
                });
            }
            _ => return Err(here("not expected here")),
        }
    }
    finish_tab(&mut layout, &mut tree)?;
    check(&layout)?;
    Ok(layout)
}

/// What every layout read or written holds to.
fn check(layout: &Layout) -> Result<(), String> {
    if layout.workspaces.is_empty() {
        return Err("no workspace".to_owned());
    }
    if layout.workspaces.len() > MAX_WORKSPACES {
        return Err(format!("more than {MAX_WORKSPACES} workspaces"));
    }
    if layout.tabs().count() > MAX_TABS {
        return Err(format!("more than {MAX_TABS} tabs"));
    }
    let mut ids = Vec::new();
    for space in &layout.workspaces {
        if space.tabs.is_empty() {
            return Err(format!("workspace w{} has no tab", space.id));
        }
        ids.push(space.id);
        ids.extend(space.tabs.iter().map(|tab| tab.id));
    }
    for (index, id) in ids.iter().enumerate() {
        if *id >= layout.next || *id == OTHER {
            return Err(format!("id {id} is not below next {}", layout.next));
        }
        if ids.iter().take(index).any(|other| other == id) {
            return Err(format!("id {id} is used twice"));
        }
    }
    let placed = layout.placed();
    if placed.len() > MAX_LEAVES {
        return Err(format!("more than {MAX_LEAVES} tiles"));
    }
    for (index, pane) in placed.iter().enumerate() {
        if placed.iter().take(index).any(|other| other == pane) {
            return Err(format!("pane {pane} has two tiles"));
        }
    }
    Ok(())
}

// ---- The file ----

/// The layout's lock, while held.
pub struct LayoutLock {
    _file: File,
}

/// Takes the layout's lock in `view` (the session's `.view`), waiting a
/// bounded time: a stalled layout write holds up only other layout writes.
pub fn lock(view: &Path) -> Result<LayoutLock, Failure> {
    let path = view.join(LOCK);
    let mut options = File::options();
    options.read(true).write(true).create(true).truncate(false);
    let file = (fuxix::file::private_file(&mut options).open(&path))
        .map_err(|e| fux3::io(path.display(), e))?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(LayoutLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => return Err(fux3::io(path.display(), e)),
        }
        if start.elapsed() >= LOCK_WAIT {
            return Err(bad(format!(
                "{} was held for over {LOCK_WAIT:?}",
                path.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The layout file's text, or `None` if there is none; a file that is not
/// a regular file, is too large or is not UTF-8 is refused.
pub fn read_text(view: &Path) -> Result<Option<String>, Failure> {
    let path = view.join(FILE);
    match fux3::files::read_small(&path, MAX_BYTES) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| bad(format!("{} is not UTF-8", path.display()))),
        Err(_) if matches!(std::fs::symlink_metadata(&path), Err(e) if e.kind() == std::io::ErrorKind::NotFound) => {
            Ok(None)
        }
        Err(failure) => Err(failure),
    }
}

/// The session's layout: the default if there is no file. A file that
/// cannot be read or is wrong is refused, saying why.
pub fn read(view: &Path) -> Result<Layout, Failure> {
    match read_text(view)? {
        Some(text) => {
            parse(&text).map_err(|why| bad(format!("{}: {why}", view.join(FILE).display())))
        }
        None => Ok(Layout::default()),
    }
}

/// Replaces `name` in `dir` with `text`, whole: written beside it, then
/// renamed over it.
pub fn replace(dir: &Path, name: &str, temp: &str, text: &str) -> Result<(), Failure> {
    let (temp, path) = (dir.join(temp), dir.join(name));
    let mut options = File::options();
    options.write(true).create(true).truncate(true);
    let mut file = (fuxix::file::private_file(&mut options).open(&temp))
        .map_err(|e| fux3::io(temp.display(), e))?;
    file.write_all(text.as_bytes())
        .map_err(|e| fux3::io(temp.display(), e))?;
    drop(file);
    std::fs::rename(&temp, &path).map_err(|e| fux3::io(path.display(), e))
}

/// What an edit did: the version now, any tab or workspace it made, any
/// pane it started, and the panes whose tiles changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edited {
    pub version: u64,
    pub made: Option<String>,
    pub started: Option<PaneRef>,
    pub changed: Vec<PaneRef>,
}

impl fmt::Display for Edited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "version {}", self.version)?;
        if let Some(made) = &self.made {
            writeln!(f, "made {made}")?;
        }
        if let Some(started) = &self.started {
            writeln!(f, "started {started}")?;
        }
        self.changed
            .iter()
            .try_for_each(|pane| writeln!(f, "changed {pane}"))
    }
}

/// Applies `op` to the layout as it is now, under the lock, and writes it,
/// if `expected` (when given) is still its version. `live` is what exists.
pub fn edit(
    view: &Path,
    expected: Option<u64>,
    live: &[Live],
    area: Rect,
    op: &Op,
) -> Result<Edited, Failure> {
    let lock = lock(view)?;
    // A reset is the one way past a file that cannot be read.
    let reset = matches!(op, Op::Reset);
    let mut layout = match read(view) {
        Ok(layout) => layout,
        Err(_) if reset => Layout::default(),
        Err(failure) => return Err(failure),
    };
    if let Some(expected) = expected
        && expected != layout.version
    {
        return Err(bad(format!(
            "the layout is at version {}, not {expected}",
            layout.version
        )));
    }
    let before = layout.shown(&routing(op, live));
    let old = layout.clone();
    let made = apply(&mut layout, live, area, op)?;
    if layout == old && !reset {
        return Ok(Edited {
            version: layout.version,
            made,
            ..Edited::default()
        });
    }
    layout.version = old.version.saturating_add(1);
    check(&layout).map_err(bad)?;
    let text = layout.to_string();
    if text.len() > MAX_BYTES {
        return Err(bad(format!(
            "the layout would be {} bytes, at most {MAX_BYTES}",
            text.len()
        )));
    }
    replace(view, FILE, TEMP, &text)?;
    drop(lock);
    Ok(Edited {
        version: layout.version,
        made,
        started: None,
        changed: changed(&before, &layout.shown(live), area),
    })
}

// ---- Operations ----

/// A tab or workspace as a command names it: its id (`t3`, `w1`), or its
/// title if no other has it.
fn find_id<'a>(
    kind: &str,
    prefix: char,
    text: &str,
    items: impl Iterator<Item = (u32, &'a str)>,
) -> Result<u32, Failure> {
    let items: Vec<(u32, &str)> = items.collect();
    if let Some(id) = text
        .strip_prefix(prefix)
        .and_then(|d| d.parse::<u32>().ok())
        && items.iter().any(|(i, _)| *i == id)
    {
        return Ok(id);
    }
    let titled: Vec<u32> = (items.iter())
        .filter(|(_, title)| *title == text)
        .map(|(id, _)| *id)
        .collect();
    match titled.as_slice() {
        [id] => Ok(*id),
        [] => Err(bad(format!("no {kind} {text}"))),
        _ => {
            let ids: Vec<String> = titled.iter().map(|id| format!("{prefix}{id}")).collect();
            Err(bad(format!(
                "{} {kind}s are called {text:?}: {}",
                ids.len(),
                ids.join(", ")
            )))
        }
    }
}

/// Where a pane goes, for `move`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Whither {
    /// Swap with the pane beside it that way.
    Beside(Dir),
    /// Swap with the one after it in its tab (true), or before.
    Order(bool),
    /// To a tab (`None`: a new tab after its own).
    Tab(Option<String>),
    /// To a workspace's first tab (`None`: a new workspace).
    Workspace(Option<String>),
}

/// What may change the layout: one command each (see the README).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// `split PANE DIR NEW`: puts pane NEW beside PANE on side DIR.
    Split(Target, Target, Dir),
    /// `split PANE DIR`: the same with a new shell, which whoever runs the
    /// command starts first ([`Op::Split`] then): no layout edit starts a pane.
    SplitNew(Target, Dir),
    /// Takes a pane's tile away.
    Close(Target),
    Swap(Target, Target),
    Move(Target, Whither),
    /// Moves a pane's border toward DIR, by a number of cells.
    Border(Target, Dir, u32),
    /// Zooms a pane in its tab, or restores the tab if it is zoomed.
    Zoom(Target),
    /// A tab: its title, the tab it comes after, its filter.
    NewTab(Option<String>, Option<String>, Vec<(String, String)>),
    RenameTab(String, String),
    CloseTab(String),
    /// A tab one place later (true) or earlier.
    MoveTab(String, bool),
    NewWorkspace(Option<String>),
    RenameWorkspace(String, String),
    CloseWorkspace(String),
    MoveWorkspace(String, bool),
    /// The default layout, over whatever is there.
    Reset,
}

/// One command line read: what it does, and the version it insists on
/// (`... if VERSION`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub op: Op,
    pub expected: Option<u64>,
}

fn later_of(word: &str) -> Result<bool, Failure> {
    match word {
        "earlier" => Ok(false),
        "later" => Ok(true),
        other => Err(bad(format!("{other:?}: earlier or later"))),
    }
}

/// Reads one layout command (the grammar is in the README).
pub fn parse_command(line: &str) -> Result<Command, Failure> {
    let all = words(line.trim()).map_err(bad)?;
    let mut words: Vec<&str> = all.iter().map(|(w, _)| w.as_str()).collect();
    let mut expected = None;
    if let [.., "if", version] = words.as_slice() {
        let version = version
            .parse()
            .map_err(|_| bad(format!("if {version:?}: a version number")))?;
        expected = Some(version);
        words.truncate(words.len().saturating_sub(2));
    }
    let target = Target::parse;
    let owned = |w: &str| w.to_owned();
    let title = |w: &str| check_title(w).map(|()| w.to_owned()).map_err(bad);
    let tests = |rest: &[&str]| {
        rest.iter()
            .map(|w| parse_test(w))
            .collect::<Result<Vec<_>, _>>()
    };
    let dir = Dir::parse;
    let op = match words.as_slice() {
        ["split", pane, way] => Op::SplitNew(target(pane)?, dir(way)?),
        ["split", pane, way, new] => Op::Split(target(pane)?, target(new)?, dir(way)?),
        ["close", pane] => Op::Close(target(pane)?),
        ["swap", a, b] => Op::Swap(target(a)?, target(b)?),
        ["move", pane, "tab", tab] => Op::Move(
            target(pane)?,
            Whither::Tab((*tab != "new").then(|| owned(tab))),
        ),
        ["move", pane, "workspace", space] => Op::Move(
            target(pane)?,
            Whither::Workspace((*space != "new").then(|| owned(space))),
        ),
        ["move", pane, way @ ("earlier" | "later")] => {
            Op::Move(target(pane)?, Whither::Order(later_of(way)?))
        }
        ["move", pane, way] => Op::Move(target(pane)?, Whither::Beside(dir(way)?)),
        ["border", pane, way, cells] => Op::Border(target(pane)?, dir(way)?, parse_cells(cells)?),
        ["zoom", pane] => Op::Zoom(target(pane)?),
        ["tab", "new", rest @ ..] => {
            let (mut name, mut after, mut filter) = (None, None, Vec::new());
            let mut rest = rest;
            while let Some((word, more)) = rest.split_first() {
                match (*word, more) {
                    ("after", [tab, more @ ..]) => (after, rest) = (Some(owned(tab)), more),
                    ("match", tests_given) => (filter, rest) = (tests(tests_given)?, &[]),
                    (text, more) if name.is_none() => (name, rest) = (Some(title(text)?), more),
                    (other, _) => return Err(bad(format!("tab new: unexpected {other:?}"))),
                }
            }
            Op::NewTab(name, after, filter)
        }
        ["tab", "rename", tab, text] => Op::RenameTab(owned(tab), title(text)?),
        ["tab", "close", tab] => Op::CloseTab(owned(tab)),
        ["tab", "move", tab, way] => Op::MoveTab(owned(tab), later_of(way)?),
        ["workspace", "new"] => Op::NewWorkspace(None),
        ["workspace", "new", text] => Op::NewWorkspace(Some(title(text)?)),
        ["workspace", "rename", space, text] => Op::RenameWorkspace(owned(space), title(text)?),
        ["workspace", "close", space] => Op::CloseWorkspace(owned(space)),
        ["workspace", "move", space, way] => Op::MoveWorkspace(owned(space), later_of(way)?),
        ["reset"] => Op::Reset,
        [] => return Err(bad("an empty command")),
        _ => return Err(bad("no such layout command; see fux3-view's README")),
    };
    Ok(Command { op, expected })
}

/// The tile `target` names in the shown layout: a live pane of that name
/// first, else the one tile of that name.
fn resolve(shown: &Layout, live: &[Live], target: &Target) -> Result<PaneRef, Failure> {
    let tiles = shown.placed();
    let missing = || bad(format!("no tile for pane {target}"));
    if let Some(id) = &target.id {
        let pane = PaneRef {
            name: target.name.clone(),
            id: id.clone(),
        };
        return tiles.contains(&pane).then_some(pane).ok_or_else(missing);
    }
    if let Some(live) = live.iter().find(|l| l.pane.name == target.name)
        && tiles.contains(&live.pane)
    {
        return Ok(live.pane.clone());
    }
    let named: Vec<&PaneRef> = tiles.iter().filter(|p| p.name == target.name).collect();
    match named.as_slice() {
        [pane] => Ok((*pane).clone()),
        [] => Err(missing()),
        _ => Err(bad(format!(
            "several tiles are named {target}: give NAME@ID"
        ))),
    }
}

/// The live pane `target` names.
fn resolve_live(live: &[Live], target: &Target) -> Result<PaneRef, Failure> {
    match (live.iter().find(|l| l.pane.name == target.name), &target.id) {
        (None, _) => Err(bad(format!("no pane {target}"))),
        (Some(l), Some(id)) if l.pane.id != *id => Err(bad(format!(
            "{} is now pane {}, not {id}",
            target.name, l.pane
        ))),
        (Some(l), _) => Ok(l.pane.clone()),
    }
}

/// Stores the tab that shows `pane`, routed panes and all, so an edit can
/// name them, and gives its id; the tab `other` becomes a real tab.
fn materialize(layout: &mut Layout, shown: &Layout, pane: &PaneRef) -> Result<u32, Failure> {
    let tab = shown
        .tab_of(pane)
        .ok_or_else(|| bad(format!("no tile for pane {pane}")))?;
    let root = tab.root.clone();
    let id = store_tab(layout, shown, tab.id)?;
    if let Some(stored) = layout.tab_mut(id) {
        stored.root = root;
    }
    Ok(id)
}

/// Makes sure tab `id` is stored, without its routed panes: the tab
/// `other` becomes a real tab, empty, which still takes them.
fn store_tab(layout: &mut Layout, shown: &Layout, id: u32) -> Result<u32, Failure> {
    let tab = shown.tab(id).ok_or_else(|| bad(format!("no tab t{id}")))?;
    if id != OTHER {
        return Ok(id);
    }
    let made = Tab::new(layout.take_id()?, &tab.title);
    let id = made.id;
    layout
        .workspaces
        .first_mut()
        .ok_or_else(|| bad("no workspace"))?
        .tabs
        .push(made);
    Ok(id)
}

/// The pane `op` gives a tile to: while the edit reads the layout, it is
/// not routed anywhere, so a pane just started moves no other tile.
fn placing(op: &Op) -> Option<&Target> {
    if let Op::Split(_, new, _) | Op::Move(new, Whither::Tab(_) | Whither::Workspace(_)) = op {
        return Some(new);
    }
    None
}

/// `live` as routing sees it during `op`.
fn routing(op: &Op, live: &[Live]) -> Vec<Live> {
    let placed = placing(op);
    let named = |l: &&Live| {
        placed.is_some_and(|t| {
            t.name == l.pane.name && t.id.as_ref().is_none_or(|id| *id == l.pane.id)
        })
    };
    live.iter().filter(|l| !named(l)).cloned().collect()
}

/// Takes `pane`'s tile out of whatever stored tab has it.
fn unplace(layout: &mut Layout, pane: &PaneRef) {
    for tab in layout.tabs_mut() {
        if tab.zoom.as_ref() == Some(pane) {
            tab.zoom = None;
        }
        tab.root = tab.root.take().and_then(|root| root.without(pane));
    }
}

fn add_to_tab(layout: &mut Layout, tab: u32, pane: &PaneRef) -> Result<(), Failure> {
    let tab = layout
        .tab_mut(tab)
        .ok_or_else(|| bad(format!("no tab t{tab}")))?;
    if tab.holds(pane) {
        return Ok(());
    }
    tab.zoom = None;
    let leaf = Node::Leaf(pane.clone());
    tab.root = Some(match tab.root.take() {
        Some(root) => root.attach(leaf),
        None => leaf,
    });
    Ok(())
}

fn tab_id(shown: &Layout, text: &str) -> Result<u32, Failure> {
    find_id(
        "tab",
        't',
        text,
        shown.tabs().map(|tab| (tab.id, tab.title.as_str())),
    )
}

fn workspace_id(layout: &Layout, text: &str) -> Result<u32, Failure> {
    find_id(
        "workspace",
        'w',
        text,
        layout.workspaces.iter().map(|s| (s.id, s.title.as_str())),
    )
}

fn space_of(layout: &Layout, tab: u32) -> usize {
    (layout.workspaces.iter())
        .position(|s| s.tabs.iter().any(|t| t.id == tab))
        .unwrap_or(0)
}

/// A new tab, put after `after` in its workspace, or at the end of
/// workspace number `space`.
fn new_tab(
    layout: &mut Layout,
    space: usize,
    after: Option<u32>,
    title: Option<&str>,
) -> Result<u32, Failure> {
    let id = layout.take_id()?;
    let space = layout
        .workspaces
        .get_mut(space)
        .ok_or_else(|| bad("no such workspace"))?;
    let title = title.map_or_else(
        || space.tabs.len().saturating_add(1).to_string(),
        str::to_owned,
    );
    let at = (after.and_then(|after| space.tabs.iter().position(|t| t.id == after)))
        .map_or(space.tabs.len(), |at| at.saturating_add(1));
    let mut rest = std::mem::take(&mut space.tabs).into_iter();
    space.tabs = rest.by_ref().take(at).collect();
    space.tabs.push(Tab::new(id, &title));
    space.tabs.extend(rest);
    Ok(id)
}

fn new_workspace(layout: &mut Layout, title: Option<&str>) -> Result<(u32, u32), Failure> {
    let id = layout.take_id()?;
    let title = title.map_or_else(
        || layout.workspaces.len().saturating_add(1).to_string(),
        str::to_owned,
    );
    layout.workspaces.push(Workspace {
        id,
        title,
        tabs: Vec::new(),
    });
    let index = layout.workspaces.len().saturating_sub(1);
    let tab = new_tab(layout, index, None, None)?;
    Ok((id, tab))
}

/// Moves the item at `at` one place earlier or later among `items`.
fn step_item<T>(items: &mut [T], at: usize, later: bool) {
    let other = match later {
        true => at.checked_add(1).filter(|i| *i < items.len()),
        false => at.checked_sub(1),
    };
    if let Some(other) = other
        && let Ok([a, b]) = items.get_disjoint_mut([at, other])
    {
        std::mem::swap(a, b);
    }
}

/// Applies `op` to `layout`; `live` is what exists. Gives a tab or
/// workspace it made.
pub fn apply(
    layout: &mut Layout,
    live: &[Live],
    area: Rect,
    op: &Op,
) -> Result<Option<String>, Failure> {
    let everything = layout.shown(live);
    let shown = layout.shown(&routing(op, live));
    let went = || bad("the tab went");
    match op {
        Op::Split(target, new, dir) => {
            let target = resolve(&shown, live, target)?;
            let new = resolve_live(live, new)?;
            if new == target {
                return Err(bad(format!("{new} cannot split itself")));
            }
            let tab = materialize(layout, &shown, &target)?;
            unplace(layout, &new);
            let tab = layout.tab_mut(tab).ok_or_else(went)?;
            tab.zoom = None;
            let placed = tab
                .root
                .as_mut()
                .is_some_and(|root| root.split(&target, &new, *dir));
            match placed {
                true => Ok(None),
                false => Err(bad(format!("no tile for pane {target}"))),
            }
        }
        Op::SplitNew(..) => Err(bad("a split with a new shell starts the shell first")),
        Op::Close(pane) => {
            let pane = resolve(&shown, live, pane)?;
            if layout.tab_of(&pane).is_some() {
                materialize(layout, &shown, &pane)?;
                unplace(layout, &pane);
            }
            Ok(None)
        }
        Op::Swap(a, b) => {
            let (a, b) = (resolve(&shown, live, a)?, resolve(&shown, live, b)?);
            materialize(layout, &shown, &a)?;
            materialize(layout, &shown, &b)?;
            exchange(layout, &a, &b);
            Ok(None)
        }
        Op::Move(pane, to) => {
            let (to, made) = match to {
                Whither::Beside(_) | Whither::Order(_) => {
                    let pane = resolve(&shown, live, pane)?;
                    if let Some(other) = swapped_with(&shown, &pane, to, area)? {
                        materialize(layout, &shown, &pane)?;
                        exchange(layout, &pane, &other);
                    }
                    return Ok(None);
                }
                Whither::Tab(Some(text)) => (
                    store_tab(layout, &everything, tab_id(&everything, text)?)?,
                    None,
                ),
                Whither::Tab(None) => {
                    let pane = resolve(&everything, live, pane)?;
                    let from = everything.tab_of(&pane).map_or(OTHER, |tab| tab.id);
                    let id = new_tab(layout, space_of(layout, from), Some(from), None)?;
                    (id, Some(format!("t{id}")))
                }
                Whither::Workspace(Some(text)) => {
                    let id = workspace_id(layout, text)?;
                    let first = (layout.workspaces.iter().find(|s| s.id == id))
                        .and_then(|s| s.tabs.first());
                    (
                        first
                            .map(|t| t.id)
                            .ok_or_else(|| bad("an empty workspace"))?,
                        None,
                    )
                }
                Whither::Workspace(None) => {
                    let (space, tab) = new_workspace(layout, None)?;
                    (tab, Some(format!("w{space}")))
                }
            };
            // A routed pane just stops being routed.
            let pane = resolve(&everything, live, pane)?;
            if !layout.tab(to).is_some_and(|tab| tab.holds(&pane)) {
                unplace(layout, &pane);
                add_to_tab(layout, to, &pane)?;
            }
            Ok(made)
        }
        Op::Border(pane, dir, cells) => {
            let pane = resolve(&shown, live, pane)?;
            let id = materialize(layout, &shown, &pane)?;
            let tab = layout.tab_mut(id).ok_or_else(went)?;
            let moved =
                (tab.root.as_mut()).is_some_and(|root| root.border(&pane, *dir, *cells, area));
            match moved {
                true => Ok(None),
                false => Err(bad(format!("{pane} has no border to move that way"))),
            }
        }
        Op::Zoom(pane) => {
            let pane = resolve(&shown, live, pane)?;
            let id = shown.tab_of(&pane).map_or(OTHER, |tab| tab.id);
            let id = store_tab(layout, &shown, id)?;
            let tab = layout.tab_mut(id).ok_or_else(went)?;
            tab.zoom = match tab.zoom.take() {
                Some(zoomed) if zoomed == pane => None,
                _ => Some(pane),
            };
            Ok(None)
        }
        Op::NewTab(title, after, tests) => {
            let (space, after) = match after {
                Some(after) => {
                    let id = store_tab(layout, &shown, tab_id(&shown, after)?)?;
                    (space_of(layout, id), Some(id))
                }
                None => (0, None),
            };
            let id = new_tab(layout, space, after, title.as_deref())?;
            if let Some(tab) = layout.tab_mut(id) {
                tab.tests = tests.clone();
            }
            Ok(Some(format!("t{id}")))
        }
        Op::RenameTab(tab, title) => {
            let id = store_tab(layout, &shown, tab_id(&shown, tab)?)?;
            if let Some(tab) = layout.tab_mut(id) {
                tab.title = title.clone();
            }
            Ok(None)
        }
        Op::CloseTab(tab) => {
            let id = tab_id(&shown, tab)?;
            if id == OTHER {
                return Err(bad(
                    "the tab other holds routed panes; it closes when they go",
                ));
            }
            for space in &mut layout.workspaces {
                space.tabs.retain(|tab| tab.id != id);
            }
            layout.workspaces.retain(|space| !space.tabs.is_empty());
            if layout.workspaces.is_empty() {
                fresh(layout)?;
            }
            Ok(None)
        }
        Op::MoveTab(tab, later) => {
            let id = store_tab(layout, &shown, tab_id(&shown, tab)?)?;
            for space in &mut layout.workspaces {
                if let Some(at) = space.tabs.iter().position(|t| t.id == id) {
                    step_item(&mut space.tabs, at, *later);
                }
            }
            Ok(None)
        }
        Op::NewWorkspace(title) => {
            let (space, _) = new_workspace(layout, title.as_deref())?;
            Ok(Some(format!("w{space}")))
        }
        Op::RenameWorkspace(workspace, title) => {
            let id = workspace_id(layout, workspace)?;
            if let Some(space) = layout.workspaces.iter_mut().find(|s| s.id == id) {
                space.title = title.clone();
            }
            Ok(None)
        }
        Op::CloseWorkspace(workspace) => {
            let id = workspace_id(layout, workspace)?;
            layout.workspaces.retain(|space| space.id != id);
            if layout.workspaces.is_empty() {
                fresh(layout)?;
            }
            Ok(None)
        }
        Op::MoveWorkspace(workspace, later) => {
            let id = workspace_id(layout, workspace)?;
            if let Some(at) = layout.workspaces.iter().position(|s| s.id == id) {
                step_item(&mut layout.workspaces, at, *later);
            }
            Ok(None)
        }
        Op::Reset => fresh(layout).map(|()| None),
    }
}

/// The pane `pane` swaps with to move `to` (beside it, or before or
/// after it in its tab); none past the tab's first or last.
fn swapped_with(
    shown: &Layout,
    pane: &PaneRef,
    to: &Whither,
    area: Rect,
) -> Result<Option<PaneRef>, Failure> {
    let tab = shown.tab_of(pane).ok_or_else(|| bad("no tab"))?;
    let leaves = tab.leaves();
    let at = leaves.iter().position(|leaf| *leaf == pane).unwrap_or(0);
    let other = match to {
        Whither::Beside(dir) => {
            let other = tab.neighbor(pane, *dir, area);
            return other
                .map(Some)
                .ok_or_else(|| bad(format!("no pane beside {pane} that way")));
        }
        Whither::Order(true) => at.checked_add(1),
        Whither::Order(false) | Whither::Tab(_) | Whither::Workspace(_) => at.checked_sub(1),
    };
    Ok(other
        .and_then(|at| leaves.get(at))
        .map(|leaf| (*leaf).clone()))
}

/// The default layout in place of `layout`, its version kept and its ids
/// new, so no old id names any of it.
fn fresh(layout: &mut Layout) -> Result<(), Failure> {
    let (version, next) = (layout.version, layout.next.max(Layout::default().next));
    *layout = Layout {
        version,
        next,
        workspaces: Vec::new(),
    };
    let (space, tab) = (layout.take_id()?, layout.take_id()?);
    layout.workspaces.push(Workspace {
        id: space,
        title: "main".to_owned(),
        tabs: vec![Tab::new(tab, "all")],
    });
    Ok(())
}

/// Exchanges the tiles of `a` and `b`.
fn exchange(layout: &mut Layout, a: &PaneRef, b: &PaneRef) {
    // A placeholder first, so the second rename cannot find the first.
    let hole = PaneRef {
        name: String::new(),
        id: String::new(),
    };
    for (from, to) in [(a, &hole), (b, a), (&hole, b)] {
        for tab in layout.tabs_mut() {
            if let Some(root) = tab.root.as_mut() {
                root.rename(from, to);
            }
            if tab.zoom.as_ref() == Some(from) {
                tab.zoom = Some(to.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(name: &str, n: char) -> PaneRef {
        PaneRef {
            name: name.to_owned(),
            id: std::iter::repeat_n(n, 16).collect(),
        }
    }

    fn live(name: &str, n: char, labels: &[(&str, &str)]) -> Live {
        Live {
            pane: pane(name, n),
            labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    const AREA: Rect = Rect {
        x: 0,
        y: 0,
        w: 101,
        h: 10,
    };

    fn run(layout: &mut Layout, live: &[Live], line: &str) -> Result<Option<String>, Failure> {
        apply(layout, live, AREA, &parse_command(line)?.op)
    }

    #[test]
    fn the_text_reads_back_as_written_and_faults_name_their_line() -> Result<(), Failure> {
        let mut layout = Layout::default();
        let panes = [
            live("a", '1', &[]),
            live("b", '2', &[]),
            live("c", '3', &[]),
        ];
        run(&mut layout, &panes, "tab new \"build x\" match role=build")?;
        run(&mut layout, &panes, "split a down b")?;
        run(&mut layout, &panes, "zoom b")?;
        let text = layout.to_string();
        let read = parse(&text).map_err(bad)?;
        assert_eq!(read, layout, "{text}");
        assert_eq!(read.to_string(), text);
        for (wrong, line) in [
            (text.replace("version", "versio"), "line 2"),
            (text.replacen("split rows", "split sideways", 1), "line"),
            (format!("{text}tab t99 \"x\"\n"), "id 99"),
            (
                format!("{text}  pane a@{}\n", pane("a", '1').id),
                "two tiles",
            ),
        ] {
            let refused = parse(&wrong).err().unwrap_or_default();
            assert!(refused.contains(line), "{refused:?} for\n{wrong}");
        }
        Ok(())
    }

    #[test]
    fn a_pane_goes_where_it_is_placed_else_to_the_first_tab_that_takes_it() -> Result<(), Failure> {
        let mut layout = Layout::default();
        run(&mut layout, &[], "tab new build match role=build")?;
        run(&mut layout, &[], "tab move all later")?;
        let panes = [
            live("make", '1', &[("role", "build")]),
            live("odd", '3', &[]),
        ];
        let tab_of = |layout: &Layout, name: &str, n: char| -> String {
            let shown = layout.shown(&panes);
            shown
                .tab_of(&pane(name, n))
                .map(|t| t.title.clone())
                .unwrap_or_default()
        };
        assert_eq!(tab_of(&layout, "make", '1'), "build");
        assert_eq!(tab_of(&layout, "odd", '3'), "all");
        run(&mut layout, &panes, "move make tab all")?;
        assert_eq!(tab_of(&layout, "make", '1'), "all");
        run(&mut layout, &panes, "tab close all")?;
        assert_eq!(tab_of(&layout, "odd", '3'), "other");
        Ok(())
    }

    #[test]
    fn an_ended_pane_keeps_its_tile_and_its_name_does_not_take_it() -> Result<(), Failure> {
        let mut layout = Layout::default();
        let first = [live("job", '1', &[]), live("shell", '2', &[])];
        run(&mut layout, &first, "split shell right job")?;
        let later = [live("job", '9', &[]), live("shell", '2', &[])];
        let shown = layout.shown(&later);
        let names: Vec<String> = shown
            .tabs()
            .flat_map(|t| t.leaves())
            .map(PaneRef::to_string)
            .collect();
        assert!(names.contains(&pane("job", '1').to_string()), "{names:?}");
        assert!(names.contains(&pane("job", '9').to_string()), "{names:?}");
        assert_eq!(names.len(), 3, "{names:?}");
        Ok(())
    }

    #[test]
    fn borders_move_within_bounds_and_a_split_changes_only_its_tiles() -> Result<(), Failure> {
        let mut layout = Layout::default();
        let panes = [
            live("a", '1', &[]),
            live("b", '2', &[]),
            live("c", '3', &[]),
        ];
        run(&mut layout, &panes, "split a right b")?;
        let width = |layout: &Layout, name: &str, n: char| -> u32 {
            let shown = layout.shown(&panes);
            let tiles: Vec<_> = shown.tabs().flat_map(|t| t.tiles(AREA)).collect();
            tiles
                .iter()
                .find(|(p, _)| *p == pane(name, n))
                .map_or(0, |(_, r)| r.w)
        };
        // c is routed in beside them; place it so only a and b split the row.
        run(&mut layout, &panes, "move c tab new")?;
        assert_eq!(width(&layout, "a", '1'), 50);
        run(&mut layout, &panes, "border a right 10")?;
        assert_eq!(width(&layout, "a", '1'), 60);
        run(&mut layout, &panes, "border b right 1000")?;
        assert!(width(&layout, "b", '2') >= 5);
        let before = layout.shown(&panes);
        run(&mut layout, &panes, "split b down c")?;
        let mut moved: Vec<String> = (changed(&before, &layout.shown(&panes), AREA).iter())
            .map(|p| p.name.clone())
            .collect();
        moved.sort();
        assert_eq!(moved, ["b", "c"]);
        Ok(())
    }
}

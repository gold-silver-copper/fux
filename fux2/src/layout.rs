//! The stored layout: one file, `layout`, in the session directory, holding
//! every workspace, tab and split of the session. Viewers and the command
//! line read it; every change is one operation, made under the layout's own
//! lock on the file as it is then, and written whole: a new file is written
//! beside it and renamed over it, so a reader sees the old file or the new
//! one, never a mix. The pane daemons never read it.
//!
//! What is shown is derived: the stored layout, plus each live pane that has
//! no tile of its own, put in the first tab whose filter it passes ("routed").
use crate::dir::{self, Session};
use crate::wire::{Code, Failure, PaneId};
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

const FILE: &str = "layout";
const TEMP: &str = "layout.new";
const LOCK: &str = "layout.lock";
const HEADER: &str = "fux2-layout 1";
/// The largest layout file read or written.
pub const MAX_BYTES: u64 = 64 << 10;
const MAX_DEPTH: usize = 8;
const MAX_WORKSPACES: usize = 32;
const MAX_TABS: usize = 64;
const MAX_LEAVES: usize = 2 * dir::MAX_PANES;
const MAX_TITLE: usize = 64;
const MAX_SHARE: u64 = 1_000_000;
/// How long a change waits for the layout's lock.
const LOCK_WAIT: Duration = Duration::from_secs(2);
/// What each part of a new split gets: shares are relative.
const SHARE: u64 = 1000;
/// The area changes are measured in when no `--area` is given.
pub const NOMINAL: Rect = Rect {
    x: 0,
    y: 0,
    w: 1000,
    h: 1000,
};
/// The id of the tab that holds routed panes no tab's filter takes, until
/// an edit stores it.
pub const OTHER: u32 = 0;

fn invalid(why: impl Into<String>) -> Failure {
    Failure::new(Code::Invalid, why)
}

fn io(what: impl fmt::Display, error: impl fmt::Display) -> Failure {
    Failure::new(Code::Io, format!("{what}: {error}"))
}

/// Which way a split runs: side by side, or stacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Columns,
    Rows,
}

impl Axis {
    fn name(self) -> &'static str {
        match self {
            Axis::Columns => "columns",
            Axis::Rows => "rows",
        }
    }

    pub fn parse(text: &str) -> Result<Axis, Failure> {
        match text {
            "columns" => Ok(Axis::Columns),
            "rows" => Ok(Axis::Rows),
            other => Err(invalid(format!("no axis {other:?}: columns or rows"))),
        }
    }
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
            other => Err(invalid(format!("no direction {other:?}"))),
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

/// One life of a pane: `NAME@ID`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PaneRef {
    pub name: String,
    pub id: PaneId,
}

impl fmt::Display for PaneRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.id)
    }
}

impl PaneRef {
    pub fn parse(text: &str) -> Result<PaneRef, Failure> {
        let (name, id) = text
            .split_once('@')
            .ok_or_else(|| invalid(format!("{text:?} is not NAME@ID")))?;
        dir::check_name(name)?;
        let id = PaneId::parse(id).ok_or_else(|| invalid(format!("bad pane id in {text:?}")))?;
        Ok(PaneRef {
            name: name.to_owned(),
            id,
        })
    }
}

/// A pane as an argument names it: `NAME`, or `NAME@ID` for one life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub id: Option<PaneId>,
}

impl Target {
    pub fn parse(text: &str) -> Result<Target, Failure> {
        let (name, id) = match text.split_once('@') {
            None => (text, None),
            Some((name, id)) => {
                let id =
                    PaneId::parse(id).ok_or_else(|| invalid(format!("bad pane id in {text}")))?;
                (name, Some(id))
            }
        };
        dir::check_name(name)?;
        Ok(Target {
            name: name.to_owned(),
            id,
        })
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.id {
            Some(id) => write!(f, "{}@{id}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

/// A tile: a pane, and the title shown instead of its name, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leaf {
    pub pane: PaneRef,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Leaf(Leaf),
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
    /// Labels a routed pane must all carry to land here; none takes any.
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
    pub fn leaves(&self) -> Vec<&Leaf> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.leaves(&mut out);
        }
        out
    }

    pub fn holds(&self, pane: &PaneRef) -> bool {
        self.leaves().iter().any(|leaf| leaf.pane == *pane)
    }
}

impl Node {
    fn leaves<'a>(&'a self, out: &mut Vec<&'a Leaf>) {
        match self {
            Node::Leaf(leaf) => out.push(leaf),
            Node::Split { parts, .. } => parts.iter().for_each(|(_, node)| node.leaves(out)),
        }
    }

    fn leaf(pane: PaneRef) -> Node {
        Node::Leaf(Leaf {
            pane,
            title: String::new(),
        })
    }

    fn first(&self) -> Option<&PaneRef> {
        match self {
            Node::Leaf(leaf) => Some(&leaf.pane),
            Node::Split { parts, .. } => parts.first().and_then(|(_, node)| node.first()),
        }
    }

    fn last(&self) -> Option<&PaneRef> {
        match self {
            Node::Leaf(leaf) => Some(&leaf.pane),
            Node::Split { parts, .. } => parts.last().and_then(|(_, node)| node.last()),
        }
    }

    /// This tree without `pane`'s leaf, its part's room shared among the
    /// rest; `None` if nothing is left.
    fn without(self, pane: &PaneRef) -> Option<Node> {
        match self {
            Node::Leaf(leaf) if leaf.pane == *pane => None,
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

    /// The leaf of `pane`, to change in place.
    fn leaf_mut(&mut self, pane: &PaneRef) -> Option<&mut Leaf> {
        match self {
            Node::Leaf(leaf) => (leaf.pane == *pane).then_some(leaf),
            Node::Split { parts, .. } => parts.iter_mut().find_map(|(_, node)| node.leaf_mut(pane)),
        }
    }

    /// Puts `new` beside `target`'s leaf: in its split if that runs along
    /// `axis`, taking half of `target`'s share, else in a new split of the
    /// two. True if `target` was found.
    fn split(&mut self, target: &PaneRef, new: &Leaf, axis: Axis, before: bool) -> bool {
        if let Node::Leaf(leaf) = self
            && leaf.pane == *target
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
            .position(|(_, node)| matches!(node, Node::Leaf(leaf) if leaf.pane == *target));
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
            .any(|(_, node)| node.split(target, new, axis, before))
    }

    /// Moves the border that `pane` has on side `dir` of the nearest split
    /// along that axis with a part there, else the one on its other side, by
    /// `amount` (see [`Amount`]) in `area`. True if a border moved.
    fn border(
        &mut self,
        pane: &PaneRef,
        dir: Dir,
        amount: Amount,
        area: Rect,
    ) -> Result<bool, Failure> {
        for wanted_side in [dir.forward(), !dir.forward()] {
            if let Some(found) = self.border_on(pane, dir, wanted_side, amount, area)? {
                return Ok(found);
            }
        }
        Ok(false)
    }

    /// Tries the border on one side (`forward`: right or down) of `pane`.
    /// `None`: there is none on that side.
    fn border_on(
        &mut self,
        pane: &PaneRef,
        dir: Dir,
        forward: bool,
        amount: Amount,
        area: Rect,
    ) -> Result<Option<bool>, Failure> {
        let Node::Split { axis, parts } = self else {
            return Ok(None);
        };
        let rects = part_rects(*axis, parts, area);
        let holder = parts.iter().position(|(_, node)| node.contains(pane));
        let Some(holder) = holder else {
            return Ok(None);
        };
        // The nearest split first: inside the part that holds the pane.
        if let Some((_, node)) = parts.get_mut(holder) {
            let inner = rects.get(holder).copied().unwrap_or_default();
            if let Some(found) = node.border_on(pane, dir, forward, amount, inner)? {
                return Ok(Some(found));
            }
        }
        if *axis != dir.axis() {
            return Ok(None);
        }
        let other = match forward {
            true => holder.checked_add(1).filter(|i| *i < parts.len()),
            false => holder.checked_sub(1),
        };
        let Some(other) = other else {
            return Ok(None);
        };
        // Toward `dir`: the part before the border grows when it moves
        // right or down.
        let (first, second) = (holder.min(other), holder.max(other));
        let total: u64 = parts.iter().map(|(share, _)| *share).sum();
        let room = match axis {
            Axis::Columns => area.w,
            Axis::Rows => area.h,
        };
        let (a, b) = (share_of(parts, first), share_of(parts, second));
        // Cells are counted where the border is drawn: the shares that put
        // it exactly that many cells away, no more.
        let delta = match amount {
            Amount::Cells(cells) => {
                let before: u64 = parts
                    .iter()
                    .take(first.saturating_add(1))
                    .map(|(s, _)| *s)
                    .sum();
                let room = u64::from(room);
                let at = room.saturating_mul(before).checked_div(total).unwrap_or(0);
                let target = match dir.forward() {
                    true => at.saturating_add(u64::from(cells)),
                    false => at.saturating_sub(u64::from(cells)),
                };
                let wanted = target
                    .saturating_mul(total)
                    .checked_next_multiple_of(room)
                    .and_then(|whole| whole.checked_div(room))
                    .ok_or_else(|| invalid("the split has no room to move a border in"))?;
                wanted.abs_diff(before)
            }
            Amount::Percent(_) => amount.shares(total, room)?,
        };
        // Each side keeps at least a cell, as it is drawn in `area`.
        let least = Amount::Cells(1).shares(total, room)?;
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
        if a == share_of(parts, first) {
            let why = format!("{pane}'s border cannot move further that way");
            return Err(invalid(why));
        }
        if let Some((share, _)) = parts.get_mut(first) {
            *share = a.max(1);
        }
        if let Some((share, _)) = parts.get_mut(second) {
            *share = b.max(1);
        }
        Ok(Some(true))
    }

    fn contains(&self, pane: &PaneRef) -> bool {
        match self {
            Node::Leaf(leaf) => leaf.pane == *pane,
            Node::Split { parts, .. } => parts.iter().any(|(_, node)| node.contains(pane)),
        }
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

    /// Keeps every share within bounds, keeping their proportions.
    fn bounded(self) -> Node {
        match self {
            Node::Leaf(leaf) => Node::Leaf(leaf),
            Node::Split { axis, parts } => {
                let total: u64 = parts.iter().map(|(share, _)| *share).sum();
                let count = u64::try_from(parts.len()).unwrap_or(1);
                let scale = total > MAX_SHARE || total < count.saturating_mul(10);
                let parts = parts
                    .into_iter()
                    .map(|(share, node)| {
                        let share = match scale {
                            true => share
                                .saturating_mul(SHARE.saturating_mul(count))
                                .checked_div(total)
                                .unwrap_or(SHARE)
                                .max(1),
                            false => share,
                        };
                        (share, node.bounded())
                    })
                    .collect();
                Node::Split { axis, parts }
            }
        }
    }
}

fn share_of(parts: &[(u64, Node)], index: usize) -> u64 {
    parts.get(index).map_or(0, |(share, _)| *share)
}

/// How a border moves: a percentage of its split, or cells as seen in an
/// area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Amount {
    Percent(u32),
    Cells(u32),
}

impl Amount {
    /// `N%` or `N` (cells).
    pub fn parse(text: &str) -> Result<Amount, Failure> {
        let bad = || invalid(format!("{text:?}: an amount is N% or a number of cells"));
        match text.strip_suffix('%') {
            Some(number) => match number.parse::<u32>() {
                Ok(n) if (1..=100).contains(&n) => Ok(Amount::Percent(n)),
                _ => Err(bad()),
            },
            None => match text.parse::<u32>() {
                Ok(n) if n > 0 => Ok(Amount::Cells(n)),
                _ => Err(bad()),
            },
        }
    }

    /// In shares of a split whose shares add to `total`, `room` long.
    fn shares(self, total: u64, room: u32) -> Result<u64, Failure> {
        let (num, den) = match self {
            Amount::Percent(n) => (u64::from(n), 100),
            Amount::Cells(n) => (u64::from(n), u64::from(room)),
        };
        // Rounded up, so a cell asked for is at least a cell moved.
        let shares = total
            .saturating_mul(num)
            .checked_next_multiple_of(den)
            .and_then(|whole| whole.checked_div(den))
            .ok_or_else(|| invalid("the split has no room to move a border in"))?;
        Ok(shares.max(1))
    }
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
    /// `COLSxROWS`.
    pub fn parse_area(text: &str) -> Result<Rect, Failure> {
        let bad = || invalid(format!("{text:?}: an area is COLSxROWS"));
        let (w, h) = text.split_once('x').ok_or_else(bad)?;
        let (w, h) = (
            w.parse::<u32>().map_err(|_| bad())?,
            h.parse::<u32>().map_err(|_| bad())?,
        );
        if w == 0 || h == 0 || w > 10_000 || h > 10_000 {
            return Err(bad());
        }
        Ok(Rect { x: 0, y: 0, w, h })
    }

    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.w)
            && y < self.y.saturating_add(self.h)
    }
}

/// Each part's rectangle, with a column (side by side) or a row (stacked)
/// between two parts for their border.
fn part_rects(axis: Axis, parts: &[(u64, Node)], area: Rect) -> Vec<Rect> {
    let total: u64 = parts.iter().map(|(share, _)| *share).sum();
    let length = match axis {
        Axis::Columns => area.w,
        Axis::Rows => area.h,
    };
    let last = parts.len().saturating_sub(1);
    let mut out = Vec::with_capacity(parts.len());
    let mut sum = 0u64;
    let mut start = 0u32;
    for (index, (share, _)) in parts.iter().enumerate() {
        sum = sum.saturating_add(*share);
        let end = u64::from(length)
            .saturating_mul(sum)
            .checked_div(total)
            .and_then(|end| u32::try_from(end).ok())
            .unwrap_or(length);
        // Each part but the last gives its own last cell to the border
        // after it, so a part added or moved shifts no other part.
        let border = u32::from(index < last);
        let size = end.saturating_sub(start).saturating_sub(border);
        out.push(match axis {
            Axis::Columns => Rect {
                x: area.x.saturating_add(start),
                w: size,
                ..area
            },
            Axis::Rows => Rect {
                y: area.y.saturating_add(start),
                h: size,
                ..area
            },
        });
        start = end;
    }
    out
}

fn place(node: &Node, area: Rect, out: &mut Vec<(PaneRef, Rect)>) {
    match node {
        Node::Leaf(leaf) => out.push((leaf.pane.clone(), area)),
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

fn borders_of(node: &Node, area: Rect, out: &mut Vec<Border>) {
    let Node::Split { axis, parts } = node else {
        return;
    };
    let rects = part_rects(*axis, parts, area);
    for (index, ((_, node), rect)) in parts.iter().zip(&rects).enumerate() {
        borders_of(node, *rect, out);
        let next = index
            .checked_add(1)
            .and_then(|i| parts.get(i).zip(rects.get(i)));
        let (Some(before), Some(((_, after), _))) = (node.last(), next) else {
            continue;
        };
        let Some(after) = after.first() else {
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
        if let Some(zoom) = self.zoom.as_ref().filter(|pane| self.holds(pane)) {
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
        let zoomed = self.zoom.as_ref().is_some_and(|pane| self.holds(pane));
        if let (Some(root), false) = (&self.root, zoomed) {
            borders_of(root, area, &mut out);
        }
        out
    }

    /// The pane beside `pane` toward `dir` in `area`: the nearest one there
    /// that shares the most of its edge.
    pub fn neighbor(&self, pane: &PaneRef, dir: Dir, area: Rect) -> Option<PaneRef> {
        // A tile with no room is no neighbour: nothing there can be seen.
        let tiles: Vec<(PaneRef, Rect)> = (self.tiles(area).into_iter())
            .filter(|(p, r)| p == pane || (r.w > 0 && r.h > 0))
            .collect();
        let (_, from) = tiles.iter().find(|(p, _)| p == pane)?;
        let overlap = |a0: u32, a1: u32, b0: u32, b1: u32| a1.min(b1).saturating_sub(a0.max(b0));
        let end = |start: u32, length: u32| start.saturating_add(length);
        (tiles.iter())
            .filter(|(p, _)| p != pane)
            .filter_map(|(p, r)| {
                let (gap, shared) = match dir {
                    Dir::Left if end(r.x, r.w) <= from.x => (
                        from.x.saturating_sub(end(r.x, r.w)),
                        overlap(r.y, end(r.y, r.h), from.y, end(from.y, from.h)),
                    ),
                    Dir::Right if r.x >= end(from.x, from.w) => (
                        r.x.saturating_sub(end(from.x, from.w)),
                        overlap(r.y, end(r.y, r.h), from.y, end(from.y, from.h)),
                    ),
                    Dir::Up if end(r.y, r.h) <= from.y => (
                        from.y.saturating_sub(end(r.y, r.h)),
                        overlap(r.x, end(r.x, r.w), from.x, end(from.x, from.w)),
                    ),
                    Dir::Down if r.y >= end(from.y, from.h) => (
                        r.y.saturating_sub(end(from.y, from.h)),
                        overlap(r.x, end(r.x, r.w), from.x, end(from.x, from.w)),
                    ),
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
    let per = count.div_ceil(columns.max(1)).max(1);
    let mut parts = Vec::new();
    let mut rest = panes;
    while !rest.is_empty() {
        let (column, after) = rest.split_at_checked(per).unwrap_or((rest, &[]));
        rest = after;
        let mut rows: Vec<(u64, Node)> = column
            .iter()
            .map(|pane| (SHARE, Node::leaf(pane.clone())))
            .collect();
        let node = match rows.len() {
            1 => rows.pop().map(|(_, node)| node),
            _ => Some(Node::Split {
                axis: Axis::Rows,
                parts: rows,
            }),
        };
        parts.extend(node.map(|node| (SHARE, node)));
    }
    match parts.len() {
        0 => None,
        1 => parts.pop().map(|(_, node)| node),
        _ => Some(Node::Split {
            axis: Axis::Columns,
            parts,
        }),
    }
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
            .flat_map(|tab| tab.leaves().into_iter().map(|leaf| leaf.pane.clone()))
            .collect()
    }

    fn take_id(&mut self) -> Result<u32, Failure> {
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| Failure::new(Code::Limit, "no ids left: fux2 layout reset"))?;
        Ok(id)
    }

    /// What is shown: this layout with every live pane that has no tile
    /// put in the first tab, in order, whose filter it passes, or else in a
    /// tab `other` at the end of the first workspace. Panes are taken by
    /// name; a tab's routed panes are added to its tree as a grid.
    pub fn shown(&self, live: &[Live]) -> Layout {
        let placed = self.placed();
        let mut sorted: Vec<&Live> = live.iter().filter(|l| !placed.contains(&l.pane)).collect();
        sorted.sort_by(|a, b| a.pane.name.cmp(&b.pane.name));
        let mut routed: Vec<(u32, Vec<PaneRef>)> = Vec::new();
        for pane in sorted {
            let tab = self
                .tabs()
                .find(|tab| dir::matches(&pane.pane.name, &pane.labels, &tab.tests))
                .map_or(OTHER, |tab| tab.id);
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
                // Last of all, so stored, it still takes only what no other
                // tab takes.
                if let Some(space) = shown.workspaces.last_mut() {
                    space.tabs.push(tab);
                }
                continue;
            }
            if let Some(tab) = shown.tab_mut(id) {
                let routed = grid(&panes);
                tab.root = match (tab.root.take(), routed) {
                    (Some(root), Some(more)) => Some(root.attach(more)),
                    (root, more) => root.or(more),
                };
            }
        }
        shown
    }

    /// Where each shown pane is, in `area`: its tab and its rectangle.
    fn rects(&self, area: Rect) -> Vec<(PaneRef, u32, Rect)> {
        self.tabs()
            .flat_map(|tab| {
                (tab.tiles(area).into_iter()).map(move |(pane, rect)| (pane, tab.id, rect))
            })
            .collect()
    }
}

/// The panes whose tile moved or changed size from `before` to `after`, as
/// shown in `area`, and which still have one.
pub fn changed(before: &Layout, after: &Layout, area: Rect) -> Vec<PaneRef> {
    let old = before.rects(area);
    (after.rects(area).into_iter())
        .filter(|(pane, _, rect)| !old.iter().any(|(was, _, old)| was == pane && old == rect))
        .map(|(pane, _, _)| pane)
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
        Node::Leaf(leaf) => {
            out.push_str(&format!("pane {}", leaf.pane));
            if !leaf.title.is_empty() {
                out.push(' ');
                quoted(&leaf.title, out);
            }
            out.push('\n');
        }
        Node::Split { axis, parts } => {
            out.push_str(&format!("split {}\n", axis.name()));
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

/// A line's words: bare words, and `"…"` strings with `\"` and `\\`.
fn words(line: &str) -> Result<Vec<(String, bool)>, String> {
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
        ["pane", pane, rest @ ..] => {
            let pane = PaneRef::parse(pane).map_err(|f| here(&f.message))?;
            let title = match rest {
                [] => String::new(),
                [_] => title(line.words.get(2)).map_err(|why| here(&why))?,
                _ => return Err(here("too many words")),
            };
            Ok(Node::Leaf(Leaf { pane, title }))
        }
        ["split", axis] => {
            let axis = Axis::parse(axis).map_err(|f| here(&f.message))?;
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
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_BYTES {
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
        line.strip_prefix(key)
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
        let tab = (layout.workspaces.last_mut()).and_then(|space| space.tabs.last_mut());
        match tab {
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
                    tab.tests
                        .push(dir::parse_label(word).map_err(|f| here(&f.message))?);
                }
                if tab.tests.is_empty() {
                    return Err(here("match needs a test"));
                }
            }
            (1, Some("zoom")) if !in_tree && tab.zoom.is_none() && words.len() == 2 => {
                let pane = words.get(1).map_or("", |(w, _)| w.as_str());
                tab.zoom = Some(PaneRef::parse(pane).map_err(|f| here(&f.message))?);
            }
            (1, _) if !in_tree && tab.root.is_none() => tree.push(TreeLine {
                depth,
                share: None,
                words,
                number,
            }),
            (_, Some(share)) if depth > 1 && in_tree => {
                let share = share
                    .parse::<u64>()
                    .ok()
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
    for tab in layout.tabs() {
        let bad = (tab.tests.iter()).any(|(k, v)| k.chars().chain(v.chars()).any(char::is_control));
        if bad {
            return Err(format!(
                "tab t{}: a match test holds a control character",
                tab.id
            ));
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

/// Takes the layout's lock, waiting a bounded time: a stalled layout write
/// holds up only other layout writes, never a pane.
pub fn lock(session: &Session) -> Result<LayoutLock, Failure> {
    let path = session.root.join(LOCK);
    let mut options = File::options();
    options.read(true).write(true).create(true).truncate(false);
    let file = fuxix::file::private_file(&mut options)
        .open(&path)
        .map_err(|e| io(path.display(), e))?;
    let now = Instant::now();
    let until = now.checked_add(LOCK_WAIT).unwrap_or(now);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(LayoutLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => return Err(io(path.display(), e)),
        }
        if Instant::now() >= until {
            let why = format!(
                "{} is held by another fux2 for over {LOCK_WAIT:?}",
                path.display()
            );
            return Err(Failure::new(Code::Busy, why));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Reads `path` if it is a regular file of at most `most` bytes. It is
/// opened without waiting, so a FIFO put in its place holds nobody.
pub fn read_small_file(path: &Path, most: u64) -> std::io::Result<String> {
    let mut options = File::options();
    options.read(true);
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Not through a link: what stands at the name is what is read.
        options.custom_flags(fuxix::file::NONBLOCK | fuxix::file::NOFOLLOW);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    if meta.len() > most {
        return Err(std::io::Error::other(format!(
            "{} bytes, at most {most}",
            meta.len()
        )));
    }
    let mut text = String::new();
    file.take(most.saturating_add(1))
        .read_to_string(&mut text)?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > most {
        return Err(std::io::Error::other(format!("more than {most} bytes")));
    }
    Ok(text)
}

/// The session's layout: the default if there is no file. A file that
/// cannot be read or is wrong is refused, saying why.
pub fn read(session: &Session) -> Result<Layout, Failure> {
    let path = session.root.join(FILE);
    match read_small_file(&path, MAX_BYTES) {
        Ok(text) => parse(&text).map_err(|why| invalid(format!("{}: {why}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Layout::default()),
        Err(e) => Err(io(path.display(), e)),
    }
}

/// Replaces the file with `layout`, whole: written beside it, flushed, then
/// renamed over it. Only under the lock.
fn write(session: &Session, _lock: &LayoutLock, text: &str) -> Result<(), Failure> {
    let temp = session.root.join(TEMP);
    let path = session.root.join(FILE);
    // Whatever was left at the new file's name (a stale file, a link to the
    // layout, a FIFO) goes, and the new file is made fresh, private and
    // ours alone: nothing written there can reach another file.
    match std::fs::remove_file(&temp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io(temp.display(), e)),
    }
    let mut options = File::options();
    options.write(true).create_new(true);
    let mut file = fuxix::file::private_file(&mut options)
        .open(&temp)
        .map_err(|e| io(temp.display(), e))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| io(temp.display(), e))?;
    drop(file);
    std::fs::rename(&temp, &path).map_err(|e| io(path.display(), e))?;
    // The rename itself is kept too; if that cannot be confirmed, say so.
    File::open(&session.root)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| {
            let why = format!(
                "the layout was replaced, but {} could not be flushed ({e}): read it before trying again",
                session.root.display()
            );
            Failure::new(Code::Io, why)
        })
}

/// The file's text for `layout`, if the reader would read back exactly
/// this layout: nothing is written that the reader would refuse.
pub fn encode(layout: &Layout) -> Result<String, Failure> {
    let text = layout.to_string();
    let refuse = |why: String| {
        let why = format!(
            "the edit would make a layout that cannot be read back ({why}); nothing was changed"
        );
        Failure::new(Code::Limit, why)
    };
    match parse(&text) {
        Ok(read) if read == *layout => Ok(text),
        Ok(_) => Err(refuse("it reads back differently".to_owned())),
        Err(why) => Err(refuse(why)),
    }
}

/// What an edit did: the version now, any tab or workspace it made, and the
/// panes whose tiles changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edited {
    pub version: u64,
    pub made: Option<String>,
    pub changed: Vec<PaneRef>,
    /// The panes a closed tab or workspace held, as the layout had them
    /// under the lock: what `--end-panes` ends.
    pub let_go: Vec<PaneRef>,
}

impl fmt::Display for Edited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "version {}", self.version)?;
        if let Some(made) = &self.made {
            writeln!(f, "made {made}")?;
        }
        self.changed
            .iter()
            .try_for_each(|pane| writeln!(f, "changed {pane}"))
    }
}

/// Applies `op` to the layout as it is now, under the lock, and writes it,
/// if `expected` (when given) is still its version. `live` is what exists.
pub fn edit(
    session: &Session,
    expected: Option<u64>,
    live: &[Live],
    area: Rect,
    op: &Op,
) -> Result<Edited, Failure> {
    let lock = lock(session)?;
    // A reset is the one way past a file that cannot be read; such a file
    // has no version to compare with `--if`.
    let reset = matches!(op, Op::Reset);
    let mut layout = match read(session) {
        Ok(layout) => layout,
        Err(failure) if reset && expected.is_some() => {
            let why = format!(
                "{}; it has no version for --if: reset without it",
                failure.message
            );
            return Err(Failure::new(Code::Stale, why));
        }
        Err(_) if reset => Layout::default(),
        Err(failure) => return Err(failure),
    };
    if let Some(expected) = expected
        && expected != layout.version
    {
        let why = format!(
            "the layout is at version {}, not {expected}",
            layout.version
        );
        return Err(Failure::new(Code::Stale, why));
    }
    let before = layout.shown(&routing(op, live));
    let let_go = held_by(&layout, &layout.shown(live), op);
    let old = layout.clone();
    let made = apply(&mut layout, live, area, op)?;
    if layout == old && !reset {
        return Ok(Edited {
            version: layout.version,
            made,
            changed: Vec::new(),
            let_go: Vec::new(),
        });
    }
    // A version never wraps, or an old `--if` could pass again; a reset
    // starts the count over.
    layout.version = match old.version.checked_add(1) {
        Some(version) => version,
        None if reset => 1,
        None => {
            let why = "the layout's version is at its end: fux2 layout reset";
            return Err(Failure::new(Code::Limit, why));
        }
    };
    let text = encode(&layout)?;
    write(session, &lock, &text)?;
    Ok(Edited {
        version: layout.version,
        made,
        changed: changed(&before, &layout.shown(live), area),
        let_go,
    })
}

/// The panes shown in the tab or workspace `op` closes.
fn held_by(layout: &Layout, shown: &Layout, op: &Op) -> Vec<PaneRef> {
    let tabs: Vec<u32> = match op {
        Op::CloseTab(tab) => tab_id(shown, tab).into_iter().collect(),
        Op::CloseWorkspace(space) => match workspace_id(layout, space) {
            Ok(id) => (shown.workspaces.iter())
                .filter(|s| s.id == id)
                .flat_map(|s| s.tabs.iter().map(|t| t.id))
                .collect(),
            Err(_) => Vec::new(),
        },
        Op::Split { .. }
        | Op::Remove(_)
        | Op::Replace { .. }
        | Op::Swap(..)
        | Op::Step { .. }
        | Op::Reorder { .. }
        | Op::ToTab { .. }
        | Op::ToWorkspace { .. }
        | Op::Border { .. }
        | Op::Zoom(_)
        | Op::Title { .. }
        | Op::NewTab { .. }
        | Op::RenameTab { .. }
        | Op::MoveTab { .. }
        | Op::MatchTab { .. }
        | Op::NewWorkspace { .. }
        | Op::RenameWorkspace { .. }
        | Op::MoveWorkspace { .. }
        | Op::Reset => Vec::new(),
    };
    (shown.tabs())
        .filter(|tab| tabs.contains(&tab.id))
        .flat_map(|tab| tab.leaves().into_iter().map(|leaf| leaf.pane.clone()))
        .collect()
}

// ---- Operations ----

/// A tab or workspace as an argument names it: its id (`t3`, `w1`), or its
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
        [] => Err(Failure::new(Code::Missing, format!("no {kind} {text}"))),
        _ => {
            let ids: Vec<String> = titled.iter().map(|id| format!("{prefix}{id}")).collect();
            Err(invalid(format!(
                "{} {kind}s are called {text:?}: {}",
                ids.len(),
                ids.join(", ")
            )))
        }
    }
}

/// What may change the layout. Panes are `NAME` or `NAME@ID`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Puts the existing pane `new` beside `target`, splitting its tile.
    Split {
        target: Target,
        new: Target,
        axis: Axis,
        before: bool,
    },
    /// Takes `pane`'s tile away.
    Remove(Target),
    /// `new` takes `old`'s tile.
    Replace {
        old: Target,
        new: Target,
    },
    Swap(Target, Target),
    /// Swaps `pane` with the pane beside it toward `dir`.
    Step {
        pane: Target,
        dir: Dir,
    },
    /// Swaps `pane` with the one before or after it in its tab.
    Reorder {
        pane: Target,
        later: bool,
    },
    /// Moves `pane` to a tab (`None`: a new tab after its own).
    ToTab {
        pane: Target,
        tab: Option<String>,
    },
    /// Moves `pane` to a workspace's first tab (`None`: a new workspace).
    ToWorkspace {
        pane: Target,
        workspace: Option<String>,
    },
    Border {
        pane: Target,
        dir: Dir,
        amount: Amount,
    },
    /// Zooms `pane` in its tab, or restores the tab if it is zoomed.
    Zoom(Target),
    Title {
        pane: Target,
        title: String,
    },
    NewTab {
        after: Option<String>,
        workspace: Option<String>,
        title: Option<String>,
        tests: Vec<(String, String)>,
    },
    RenameTab {
        tab: String,
        title: String,
    },
    CloseTab(String),
    MoveTab {
        tab: String,
        later: bool,
    },
    MatchTab {
        tab: String,
        tests: Vec<(String, String)>,
    },
    NewWorkspace {
        title: Option<String>,
    },
    RenameWorkspace {
        workspace: String,
        title: String,
    },
    CloseWorkspace(String),
    MoveWorkspace {
        workspace: String,
        later: bool,
    },
    /// The default layout, over whatever is there.
    Reset,
}

/// The tile `target` names in the shown layout: a live pane of that name
/// first, else the one tile of that name.
fn resolve(shown: &Layout, live: &[Live], target: &Target) -> Result<PaneRef, Failure> {
    let tiles = shown.placed();
    if let Some(id) = target.id {
        let pane = PaneRef {
            name: target.name.clone(),
            id,
        };
        return match tiles.contains(&pane) {
            true => Ok(pane),
            false => Err(Failure::new(
                Code::Missing,
                format!("no tile for pane {target}"),
            )),
        };
    }
    if let Some(live) = live.iter().find(|l| l.pane.name == target.name)
        && tiles.contains(&live.pane)
    {
        return Ok(live.pane.clone());
    }
    let named: Vec<&PaneRef> = tiles.iter().filter(|p| p.name == target.name).collect();
    match named.as_slice() {
        [pane] => Ok((*pane).clone()),
        [] => Err(Failure::new(
            Code::Missing,
            format!("no tile for pane {target}"),
        )),
        _ => Err(invalid(format!(
            "several tiles are named {target}: give NAME@ID"
        ))),
    }
}

/// The live pane `target` names.
fn resolve_live(live: &[Live], target: &Target) -> Result<PaneRef, Failure> {
    let found = live.iter().find(|l| l.pane.name == target.name);
    match (found, target.id) {
        (None, _) => Err(Failure::new(Code::Missing, format!("no pane {target}"))),
        (Some(l), Some(id)) if l.pane.id != id => {
            let why = format!("{} is now pane {}, not {id}", target.name, l.pane.id);
            Err(Failure::new(Code::Replaced, why))
        }
        (Some(l), _) => Ok(l.pane.clone()),
    }
}

/// Stores tab `id` as it is shown, routed panes and all, so an edit can
/// name them; the tab `other` becomes a real tab.
fn materialize(layout: &mut Layout, shown: &Layout, id: u32) -> Result<u32, Failure> {
    let Some(tab) = shown.tab(id) else {
        return Err(Failure::new(Code::Missing, format!("no tab t{id}")));
    };
    let root = tab.root.clone().map(Node::bounded);
    let id = store_tab(layout, shown, id)?;
    if let Some(stored) = layout.tab_mut(id) {
        stored.root = root;
    }
    Ok(id)
}

/// Makes sure tab `id` is stored, without its routed panes: the tab
/// `other` becomes a real tab, empty, which still takes them.
fn store_tab(layout: &mut Layout, shown: &Layout, id: u32) -> Result<u32, Failure> {
    let Some(tab) = shown.tab(id) else {
        return Err(Failure::new(Code::Missing, format!("no tab t{id}")));
    };
    if id != OTHER {
        return Ok(id);
    }
    let made = Tab::new(layout.take_id()?, &tab.title);
    let id = made.id;
    let space = layout
        .workspaces
        .last_mut()
        .ok_or_else(|| invalid("no workspace"))?;
    space.tabs.push(made);
    Ok(id)
}

/// The pane a split or a replace gives a tile to, most often one just
/// started: while the edit reads the layout it is not routed anywhere, so
/// placing it moves no other tile. A move is of a pane already shown.
fn placing(op: &Op) -> Option<&Target> {
    match op {
        Op::Split { new, .. } | Op::Replace { new, .. } => Some(new),
        Op::ToTab { .. }
        | Op::ToWorkspace { .. }
        | Op::Remove(_)
        | Op::Swap(..)
        | Op::Step { .. }
        | Op::Reorder { .. }
        | Op::Border { .. }
        | Op::Zoom(_)
        | Op::Title { .. }
        | Op::NewTab { .. }
        | Op::RenameTab { .. }
        | Op::CloseTab(_)
        | Op::MoveTab { .. }
        | Op::MatchTab { .. }
        | Op::NewWorkspace { .. }
        | Op::RenameWorkspace { .. }
        | Op::CloseWorkspace(_)
        | Op::MoveWorkspace { .. }
        | Op::Reset => None,
    }
}

/// `live` as routing sees it during `op`.
fn routing(op: &Op, live: &[Live]) -> Vec<Live> {
    let placed = placing(op);
    let named = |l: &&Live| {
        placed.is_some_and(|t| t.name == l.pane.name && t.id.is_none_or(|id| id == l.pane.id))
    };
    live.iter().filter(|l| !named(l)).cloned().collect()
}

/// Stores the tab that shows `pane` and gives its id.
fn materialize_pane(layout: &mut Layout, shown: &Layout, pane: &PaneRef) -> Result<u32, Failure> {
    let id = shown.tab_of(pane).map(|tab| tab.id);
    let id = id.ok_or_else(|| Failure::new(Code::Missing, format!("no tile for pane {pane}")))?;
    materialize(layout, shown, id)
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

/// Takes `pane`'s tile out of whatever stored tab has it, and gives it:
/// its title goes with it. A routed pane gets a tile of its own.
fn take_leaf(layout: &mut Layout, pane: &PaneRef) -> Leaf {
    let found = (layout.tabs())
        .flat_map(|tab| tab.leaves())
        .find(|leaf| leaf.pane == *pane)
        .cloned();
    unplace(layout, pane);
    found.unwrap_or_else(|| Leaf {
        pane: pane.clone(),
        title: String::new(),
    })
}

/// Moves `pane` into tab `to`, at the end of its tree. The routed panes
/// already shown there get tiles of their own first, as any edit of a
/// tab's tree gives them.
fn move_into(
    layout: &mut Layout,
    live: &[Live],
    shown: &Layout,
    pane: &PaneRef,
    to: u32,
) -> Result<(), Failure> {
    if shown.tab_of(pane).is_some_and(|tab| tab.id == to) {
        return Ok(());
    }
    let leaf = take_leaf(layout, pane);
    let others: Vec<Live> = live.iter().filter(|l| l.pane != *pane).cloned().collect();
    materialize(layout, &layout.shown(&others), to)?;
    let tab = layout
        .tab_mut(to)
        .ok_or_else(|| Failure::new(Code::Missing, format!("no tab t{to}")))?;
    tab.zoom = None;
    let leaf = Node::Leaf(leaf);
    tab.root = Some(match tab.root.take() {
        Some(root) => root.attach(leaf),
        None => leaf,
    });
    Ok(())
}

fn tab_id(shown: &Layout, text: &str) -> Result<u32, Failure> {
    let items = shown.tabs().map(|tab| (tab.id, tab.title.as_str()));
    find_id("tab", 't', text, items)
}

fn workspace_id(layout: &Layout, text: &str) -> Result<u32, Failure> {
    let items = (layout.workspaces.iter()).map(|space| (space.id, space.title.as_str()));
    find_id("workspace", 'w', text, items)
}

fn checked_title(text: &str) -> Result<String, Failure> {
    check_title(text).map_err(invalid)?;
    Ok(text.to_owned())
}

/// A new tab, put after `after` in its workspace, or at the end of
/// `workspace` (index).
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
        .ok_or_else(|| invalid("no such workspace"))?;
    let title = title.map_or_else(
        || (space.tabs.len().saturating_add(1)).to_string(),
        str::to_owned,
    );
    let tab = Tab::new(id, &title);
    let at = after
        .and_then(|after| space.tabs.iter().position(|t| t.id == after))
        .map_or(space.tabs.len(), |at| at.saturating_add(1));
    let mut tabs = std::mem::take(&mut space.tabs);
    let tail = tabs.split_off_checked(at);
    tabs.push(tab);
    tabs.extend(tail);
    space.tabs = tabs;
    Ok(id)
}

trait SplitOffChecked<T> {
    fn split_off_checked(&mut self, at: usize) -> Vec<T>;
}

impl<T> SplitOffChecked<T> for Vec<T> {
    /// The items from `at` on, taken out; none if `at` is past the end.
    fn split_off_checked(&mut self, at: usize) -> Vec<T> {
        let mut tail = Vec::new();
        while self.len() > at {
            tail.extend(self.pop());
        }
        tail.reverse();
        tail
    }
}

fn new_workspace(layout: &mut Layout, title: Option<&str>) -> Result<(u32, u32), Failure> {
    let id = layout.take_id()?;
    let title = title.map_or_else(
        || (layout.workspaces.len().saturating_add(1)).to_string(),
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

/// Moves item `id` one place earlier or later among `items`.
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
    let made = apply_op(layout, live, area, op)?;
    // A zoom names a pane its tab shows; one that left the tab is let go.
    let shown = layout.shown(live);
    for tab in layout.tabs_mut() {
        let kept = (tab.zoom.as_ref())
            .is_some_and(|zoom| shown.tab(tab.id).is_some_and(|t| t.holds(zoom)));
        if !kept {
            tab.zoom = None;
        }
    }
    Ok(made)
}

fn apply_op(
    layout: &mut Layout,
    live: &[Live],
    area: Rect,
    op: &Op,
) -> Result<Option<String>, Failure> {
    let everything = layout.shown(live);
    let shown = layout.shown(&routing(op, live));
    match op {
        Op::Split {
            target,
            new,
            axis,
            before,
        } => {
            let target = resolve(&shown, live, target)?;
            let new = resolve_live(live, new)?;
            if new == target {
                return Err(invalid(format!("{new} cannot split itself")));
            }
            let tab = materialize_pane(layout, &shown, &target)?;
            let leaf = take_leaf(layout, &new);
            let tab = layout.tab_mut(tab).ok_or_else(|| invalid("the tab went"))?;
            tab.zoom = None;
            let placed = tab
                .root
                .as_mut()
                .is_some_and(|root| root.split(&target, &leaf, *axis, *before));
            if !placed {
                return Err(Failure::new(
                    Code::Missing,
                    format!("no tile for pane {target}"),
                ));
            }
            Ok(None)
        }
        Op::Remove(pane) => {
            let pane = resolve(&shown, live, pane)?;
            if layout.tab_of(&pane).is_some() {
                let tab = materialize_pane(layout, &shown, &pane)?;
                unplace(layout, &pane);
                if let Some(tab) = layout.tab_mut(tab) {
                    tab.zoom = tab.zoom.take().filter(|z| *z != pane);
                }
            }
            Ok(None)
        }
        Op::Replace { old, new } => {
            let old = resolve(&shown, live, old)?;
            let new = resolve_live(live, new)?;
            // A pane still there, ended or not, would only be routed beside
            // its replacement.
            if old != new && live.iter().any(|l| l.pane == old) {
                let why = format!("{old} is still there: fux2 kill it first, or swap the two");
                return Err(invalid(why));
            }
            if old == new {
                return Ok(None);
            }
            let tab = materialize_pane(layout, &shown, &old)?;
            unplace(layout, &new);
            let tab = layout.tab_mut(tab).ok_or_else(|| invalid("the tab went"))?;
            if tab.zoom.as_ref() == Some(&old) {
                tab.zoom = Some(new.clone());
            }
            if let Some(leaf) = tab.root.as_mut().and_then(|root| root.leaf_mut(&old)) {
                leaf.pane = new;
            }
            Ok(None)
        }
        Op::Swap(a, b) => {
            let (a, b) = (resolve(&shown, live, a)?, resolve(&shown, live, b)?);
            materialize_pane(layout, &shown, &a)?;
            // Read again: the first may have stored the tab both are in.
            materialize_pane(layout, &layout.shown(live), &b)?;
            exchange(layout, &a, &b);
            Ok(None)
        }
        Op::Step { pane, dir } => {
            let pane = resolve(&shown, live, pane)?;
            let tab = shown.tab_of(&pane).ok_or_else(|| invalid("no tab"))?;
            let other = tab.neighbor(&pane, *dir, area);
            let other = other.ok_or_else(|| invalid(format!("no pane beside {pane} that way")))?;
            materialize_pane(layout, &shown, &pane)?;
            exchange(layout, &pane, &other);
            Ok(None)
        }
        Op::Reorder { pane, later } => {
            let pane = resolve(&shown, live, pane)?;
            let tab = shown.tab_of(&pane).ok_or_else(|| invalid("no tab"))?;
            let leaves = tab.leaves();
            let at = leaves
                .iter()
                .position(|leaf| leaf.pane == pane)
                .unwrap_or(0);
            let other = match later {
                true => at.checked_add(1).and_then(|i| leaves.get(i)),
                false => at.checked_sub(1).and_then(|i| leaves.get(i)),
            };
            let Some(other) = other.map(|leaf| leaf.pane.clone()) else {
                return Ok(None);
            };
            materialize_pane(layout, &shown, &pane)?;
            exchange(layout, &pane, &other);
            Ok(None)
        }
        Op::ToTab { pane, tab } => {
            // A routed pane just stops being routed; its old tab is left
            // as it is stored.
            let pane = resolve(&everything, live, pane)?;
            let from = everything.tab_of(&pane).map_or(OTHER, |tab| tab.id);
            let (to, made) = match tab {
                Some(text) => (
                    store_tab(layout, &everything, tab_id(&everything, text)?)?,
                    None,
                ),
                None => {
                    let space = (layout.workspaces.iter())
                        .position(|s| s.tabs.iter().any(|t| t.id == from))
                        .unwrap_or(0);
                    let id = new_tab(layout, space, Some(from), None)?;
                    (id, Some(format!("t{id}")))
                }
            };
            move_into(layout, live, &everything, &pane, to)?;
            Ok(made)
        }
        Op::ToWorkspace { pane, workspace } => {
            let pane = resolve(&everything, live, pane)?;
            let (to, made) = match workspace {
                Some(text) => {
                    let id = workspace_id(layout, text)?;
                    let space = layout.workspaces.iter().find(|s| s.id == id);
                    let first = space.and_then(|s| s.tabs.first()).map(|t| t.id);
                    (first.ok_or_else(|| invalid("an empty workspace"))?, None)
                }
                None => {
                    let (space, tab) = new_workspace(layout, None)?;
                    (tab, Some(format!("w{space}")))
                }
            };
            move_into(layout, live, &everything, &pane, to)?;
            Ok(made)
        }
        Op::Border { pane, dir, amount } => {
            let pane = resolve(&shown, live, pane)?;
            let id = materialize_pane(layout, &shown, &pane)?;
            let tab = layout.tab_mut(id).ok_or_else(|| invalid("the tab went"))?;
            let moved = match tab.root.as_mut() {
                Some(root) => root.border(&pane, *dir, *amount, area)?,
                None => false,
            };
            if !moved {
                return Err(invalid(format!("{pane} has no border to move that way")));
            }
            Ok(None)
        }
        Op::Zoom(pane) => {
            let pane = resolve(&shown, live, pane)?;
            let id = shown.tab_of(&pane).map_or(OTHER, |tab| tab.id);
            let id = store_tab(layout, &shown, id)?;
            let tab = layout.tab_mut(id).ok_or_else(|| invalid("the tab went"))?;
            tab.zoom = match tab.zoom.take() {
                Some(zoomed) if zoomed == pane => None,
                _ => Some(pane),
            };
            Ok(None)
        }
        Op::Title { pane, title } => {
            let pane = resolve(&shown, live, pane)?;
            let title = checked_title(title)?;
            let id = materialize_pane(layout, &shown, &pane)?;
            let leaf = (layout.tab_mut(id))
                .and_then(|tab| tab.root.as_mut())
                .and_then(|root| root.leaf_mut(&pane));
            if let Some(leaf) = leaf {
                leaf.title = title;
            }
            Ok(None)
        }
        Op::NewTab {
            after,
            workspace,
            title,
            tests,
        } => {
            if let Some(title) = title {
                check_title(title).map_err(invalid)?;
            }
            let (space, after) = match (after, workspace) {
                (Some(after), _) => {
                    let id = tab_id(&shown, after)?;
                    let id = store_tab(layout, &shown, id)?;
                    let space = (layout.workspaces.iter())
                        .position(|s| s.tabs.iter().any(|t| t.id == id))
                        .unwrap_or(0);
                    (space, Some(id))
                }
                (None, Some(workspace)) => {
                    let id = workspace_id(layout, workspace)?;
                    let space = layout
                        .workspaces
                        .iter()
                        .position(|s| s.id == id)
                        .unwrap_or(0);
                    (space, None)
                }
                (None, None) => (0, None),
            };
            let id = new_tab(layout, space, after, title.as_deref())?;
            if let Some(tab) = layout.tab_mut(id) {
                tab.tests = tests.clone();
            }
            Ok(Some(format!("t{id}")))
        }
        Op::RenameTab { tab, title } => {
            let title = checked_title(title)?;
            let id = store_tab(layout, &shown, tab_id(&shown, tab)?)?;
            if let Some(tab) = layout.tab_mut(id) {
                tab.title = title;
            }
            Ok(None)
        }
        Op::CloseTab(tab) => {
            let id = tab_id(&shown, tab)?;
            if id == OTHER {
                return Err(invalid(
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
        Op::MoveTab { tab, later } => {
            let id = store_tab(layout, &shown, tab_id(&shown, tab)?)?;
            for space in &mut layout.workspaces {
                if let Some(at) = space.tabs.iter().position(|t| t.id == id) {
                    step_item(&mut space.tabs, at, *later);
                }
            }
            Ok(None)
        }
        Op::MatchTab { tab, tests } => {
            let id = store_tab(layout, &shown, tab_id(&shown, tab)?)?;
            if let Some(tab) = layout.tab_mut(id) {
                tab.tests = tests.clone();
            }
            Ok(None)
        }
        Op::NewWorkspace { title } => {
            if let Some(title) = title {
                check_title(title).map_err(invalid)?;
            }
            let (space, _) = new_workspace(layout, title.as_deref())?;
            Ok(Some(format!("w{space}")))
        }
        Op::RenameWorkspace { workspace, title } => {
            let title = checked_title(title)?;
            let id = workspace_id(layout, workspace)?;
            if let Some(space) = layout.workspaces.iter_mut().find(|s| s.id == id) {
                space.title = title;
            }
            Ok(None)
        }
        Op::CloseWorkspace(workspace) => {
            let id = workspace_id(layout, workspace)?;
            let tabs: Vec<u32> = (layout.workspaces.iter())
                .filter(|s| s.id == id)
                .flat_map(|s| s.tabs.iter().map(|t| t.id))
                .collect();
            for tab in tabs {
                apply_op(layout, live, area, &Op::CloseTab(format!("t{tab}")))?;
            }
            Ok(None)
        }
        Op::MoveWorkspace { workspace, later } => {
            let id = workspace_id(layout, workspace)?;
            if let Some(at) = layout.workspaces.iter().position(|s| s.id == id) {
                step_item(&mut layout.workspaces, at, *later);
            }
            Ok(None)
        }
        Op::Reset => {
            fresh(layout)?;
            Ok(None)
        }
    }
}

/// The default layout in place of `layout`, its version kept and its ids
/// new, so no old id names any of it.
fn fresh(layout: &mut Layout) -> Result<(), Failure> {
    let first = Layout::default().next;
    // Ids that ran out start over: nothing old is left for them to name.
    let next = match layout.next.checked_add(2) {
        Some(_) => layout.next.max(first),
        None => first,
    };
    let version = layout.version;
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

/// Exchanges the tiles of `a` and `b`, titles going with their panes.
fn exchange(layout: &mut Layout, a: &PaneRef, b: &PaneRef) {
    if a == b {
        return;
    }
    let find = |layout: &Layout, pane: &PaneRef| -> Option<Leaf> {
        layout
            .tabs()
            .flat_map(|tab| tab.leaves())
            .find(|leaf| leaf.pane == *pane)
            .cloned()
    };
    let (Some(leaf_a), Some(leaf_b)) = (find(layout, a), find(layout, b)) else {
        return;
    };
    // A placeholder first, so the second rename cannot find the first.
    let hole = PaneRef {
        name: String::new(),
        id: PaneId::default(),
    };
    let mut rename = |from: &PaneRef, to: &Leaf| {
        for tab in layout.tabs_mut() {
            if let Some(leaf) = tab.root.as_mut().and_then(|root| root.leaf_mut(from)) {
                *leaf = to.clone();
            }
        }
    };
    let placeholder = Leaf {
        pane: hole.clone(),
        title: String::new(),
    };
    rename(a, &placeholder);
    rename(b, &leaf_a);
    rename(&hole, &leaf_b);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(name: &str, n: u8) -> PaneRef {
        PaneRef {
            name: name.to_owned(),
            id: PaneId([n; 16]),
        }
    }

    fn live(name: &str, n: u8, labels: &[(&str, &str)]) -> Live {
        Live {
            pane: pane(name, n),
            labels: (labels.iter())
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn target(name: &str) -> Target {
        Target {
            name: name.to_owned(),
            id: None,
        }
    }

    /// A layout with two filtered tabs, and nothing placed.
    fn two_tabs() -> Result<Layout, Failure> {
        let mut layout = Layout::default();
        for (title, test) in [("build", "role=build"), ("logs", "role=logs")] {
            let (key, value) = dir::parse_label(test)?;
            apply(
                &mut layout,
                &[],
                NOMINAL,
                &Op::NewTab {
                    after: None,
                    workspace: None,
                    title: Some(title.to_owned()),
                    tests: vec![(key, value)],
                },
            )?;
        }
        // The catch-all goes last, so the filtered tabs come first.
        apply(
            &mut layout,
            &[],
            NOMINAL,
            &Op::MoveTab {
                tab: "all".to_owned(),
                later: true,
            },
        )?;
        apply(
            &mut layout,
            &[],
            NOMINAL,
            &Op::MoveTab {
                tab: "all".to_owned(),
                later: true,
            },
        )?;
        Ok(layout)
    }

    #[test]
    fn the_text_reads_back_as_written_and_faults_name_their_line() -> Result<(), Failure> {
        let mut layout = two_tabs()?;
        let panes = [live("a", 1, &[]), live("b", 2, &[]), live("c", 3, &[])];
        apply(
            &mut layout,
            &panes,
            NOMINAL,
            &Op::Split {
                target: target("a"),
                new: target("b"),
                axis: Axis::Rows,
                before: false,
            },
        )?;
        apply(
            &mut layout,
            &panes,
            NOMINAL,
            &Op::Title {
                pane: target("b"),
                title: "say \"hi\" \\ there".to_owned(),
            },
        )?;
        let text = layout.to_string();
        let read = parse(&text).map_err(invalid)?;
        assert_eq!(read, layout, "{text}");
        assert_eq!(read.to_string(), text);
        for (bad, line) in [
            (text.replace("version", "versio"), "line 2"),
            (text.replacen("split rows", "split sideways", 1), "line"),
            (format!("{text}tab t99 \"x\"\n"), "id 99"),
            (format!("{text}  pane a@{}\n", PaneId([1; 16])), "line"),
        ] {
            let refused = parse(&bad).err().unwrap_or_default();
            assert!(refused.contains(line), "{refused:?} for\n{bad}");
        }
        Ok(())
    }

    #[test]
    fn a_pane_goes_where_it_is_placed_else_to_the_first_tab_that_takes_it() -> Result<(), Failure> {
        let mut layout = two_tabs()?;
        let panes = [
            live("make", 1, &[("role", "build")]),
            live("tail", 2, &[("role", "logs")]),
            live("odd", 3, &[]),
        ];
        let shown = layout.shown(&panes);
        let tab_of = |shown: &Layout, name: &str, n: u8| -> String {
            shown
                .tab_of(&pane(name, n))
                .map(|t| t.title.clone())
                .unwrap_or_default()
        };
        assert_eq!(tab_of(&shown, "make", 1), "build");
        assert_eq!(tab_of(&shown, "tail", 2), "logs");
        assert_eq!(tab_of(&shown, "odd", 3), "all");
        // An explicit placement wins over the filter.
        apply(
            &mut layout,
            &panes,
            NOMINAL,
            &Op::ToTab {
                pane: target("make"),
                tab: Some("logs".to_owned()),
            },
        )?;
        assert_eq!(tab_of(&layout.shown(&panes), "make", 1), "logs");
        // With no catch-all, the rest go to `other`.
        apply(
            &mut layout,
            &panes,
            NOMINAL,
            &Op::CloseTab("all".to_owned()),
        )?;
        assert_eq!(tab_of(&layout.shown(&panes), "odd", 3), "other");
        Ok(())
    }

    #[test]
    fn an_ended_pane_keeps_its_tile_and_its_name_does_not_take_it() -> Result<(), Failure> {
        let mut layout = Layout::default();
        let first = [live("job", 1, &[]), live("shell", 2, &[])];
        apply(
            &mut layout,
            &first,
            NOMINAL,
            &Op::Split {
                target: target("shell"),
                new: target("job"),
                axis: Axis::Columns,
                before: false,
            },
        )?;
        // `job` is gone; a new pane takes its name.
        let later = [live("job", 9, &[]), live("shell", 2, &[])];
        let shown = layout.shown(&later);
        let tab = shown.tabs().next().ok_or_else(|| invalid("no tab"))?;
        let names: Vec<String> = tab.leaves().iter().map(|l| l.pane.to_string()).collect();
        assert!(names.contains(&pane("job", 1).to_string()), "{names:?}");
        assert!(names.contains(&pane("job", 9).to_string()), "{names:?}");
        assert_eq!(names.len(), 3, "{names:?}");
        // Replacing puts the new life in the old tile.
        apply(
            &mut layout,
            &later,
            NOMINAL,
            &Op::Replace {
                old: Target {
                    name: "job".to_owned(),
                    id: Some(PaneId([1; 16])),
                },
                new: target("job"),
            },
        )?;
        let shown = layout.shown(&later);
        let count = shown.tabs().flat_map(|t| t.leaves()).count();
        assert_eq!(count, 2);
        assert!(shown.tab_of(&pane("job", 9)).is_some());
        Ok(())
    }

    #[test]
    fn a_split_changes_exactly_the_tiles_it_splits() -> Result<(), Failure> {
        let id = |n: u8| PaneId([n; 16]);
        let text = format!(
            "{HEADER}\nversion 4\nnext 5\nworkspace w1 \"main\"\ntab t2 \"all\"\n  split columns\n    \
             1000 pane a@{}\n    1000 pane b@{}\n    1000 pane c@{}\ntab t3 \"spare\"\n  pane d@{}\n",
            id(1),
            id(2),
            id(3),
            id(4)
        );
        let mut layout = parse(&text).map_err(invalid)?;
        let panes = [
            live("a", 1, &[]),
            live("b", 2, &[]),
            live("c", 3, &[]),
            live("d", 4, &[]),
        ];
        let before = layout.shown(&panes);
        apply(
            &mut layout,
            &panes,
            NOMINAL,
            &Op::Split {
                target: target("b"),
                new: target("d"),
                axis: Axis::Rows,
                before: false,
            },
        )?;
        let mut moved: Vec<String> = changed(&before, &layout.shown(&panes), NOMINAL)
            .iter()
            .map(|p| p.name.clone())
            .collect();
        moved.sort();
        assert_eq!(moved, ["b", "d"]);
        // Moved away, it gives its room back to `b` alone.
        let before = layout.shown(&panes);
        let to_spare = Op::ToTab {
            pane: target("d"),
            tab: Some("spare".to_owned()),
        };
        apply(&mut layout, &panes, NOMINAL, &to_spare)?;
        let after_remove = layout.shown(&panes);
        let mut moved: Vec<String> = (changed(&before, &after_remove, NOMINAL).iter())
            .map(|p| p.name.clone())
            .collect();
        moved.sort();
        assert_eq!(moved, ["b", "d"]);
        apply(
            &mut layout,
            &panes,
            NOMINAL,
            &Op::Split {
                target: target("c"),
                new: target("d"),
                axis: Axis::Rows,
                before: false,
            },
        )?;
        let mut moved: Vec<String> = changed(&after_remove, &layout.shown(&panes), NOMINAL)
            .iter()
            .map(|p| p.name.clone())
            .collect();
        moved.sort();
        assert_eq!(moved, ["c", "d"]);
        Ok(())
    }

    #[test]
    fn a_layout_too_deep_or_with_a_newline_is_never_encoded() -> Result<(), Failure> {
        let mut layout = Layout::default();
        let mut panes = vec![live("a", 1, &[])];
        for n in 2..=10u8 {
            panes.push(live(&format!("p{n}"), n, &[]));
            let axis = if n % 2 == 0 {
                Axis::Columns
            } else {
                Axis::Rows
            };
            let split = Op::Split {
                target: target("a"),
                new: target(&format!("p{n}")),
                axis,
                before: false,
            };
            apply(&mut layout, &panes, NOMINAL, &split)?;
        }
        assert!(encode(&layout).is_err(), "{layout}");
        let mut layout = Layout::default();
        let (key, value) = ("role".to_owned(), "a\nb".to_owned());
        apply(
            &mut layout,
            &[],
            NOMINAL,
            &Op::MatchTab {
                tab: "all".to_owned(),
                tests: vec![(key, value)],
            },
        )?;
        assert!(encode(&layout).is_err());
        Ok(())
    }

    fn names(panes: Vec<PaneRef>) -> Vec<String> {
        let mut names: Vec<String> = panes.into_iter().map(|p| p.name).collect();
        names.sort();
        names
    }

    /// Three columns a | b | c stored, and `d` in a spare tab.
    fn three_columns() -> Result<(Layout, Vec<Live>), Failure> {
        let id = |n: u8| PaneId([n; 16]);
        let text = format!(
            "{HEADER}\nversion 4\nnext 5\nworkspace w1 \"main\"\ntab t2 \"all\"\n  split columns\n    \
             1000 pane a@{}\n    1000 pane b@{}\n    1000 pane c@{}\ntab t3 \"spare\"\n  pane d@{} \"the d\"\n",
            id(1),
            id(2),
            id(3),
            id(4)
        );
        let live = vec![
            live("a", 1, &[]),
            live("b", 2, &[]),
            live("c", 3, &[]),
            live("d", 4, &[]),
        ];
        Ok((parse(&text).map_err(invalid)?, live))
    }

    #[test]
    fn a_split_along_its_own_axis_moves_no_neighbour_and_keeps_titles() -> Result<(), Failure> {
        let (mut layout, panes) = three_columns()?;
        let area = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 40,
        };
        let before = layout.shown(&panes);
        apply(
            &mut layout,
            &panes,
            area,
            &Op::Split {
                target: target("a"),
                new: target("d"),
                axis: Axis::Columns,
                before: false,
            },
        )?;
        assert_eq!(
            names(changed(&before, &layout.shown(&panes), area)),
            ["a", "d"]
        );
        let titled = |layout: &Layout| -> String {
            (layout.tabs().flat_map(|t| t.leaves()))
                .find(|l| l.pane.name == "d")
                .map(|l| l.title.clone())
                .unwrap_or_default()
        };
        assert_eq!(titled(&layout), "the d");
        apply(
            &mut layout,
            &panes,
            area,
            &Op::ToTab {
                pane: target("d"),
                tab: None,
            },
        )?;
        assert_eq!(titled(&layout), "the d");
        Ok(())
    }

    #[test]
    fn moving_a_routed_pane_names_the_neighbour_it_widens() -> Result<(), Failure> {
        let mut layout = Layout::default();
        let panes = [live("a", 1, &[]), live("b", 2, &[])];
        let before = layout.shown(&panes);
        let edited_layout = {
            apply(
                &mut layout,
                &panes,
                NOMINAL,
                &Op::ToTab {
                    pane: target("a"),
                    tab: None,
                },
            )?;
            layout.shown(&panes)
        };
        assert_eq!(names(changed(&before, &edited_layout, NOMINAL)), ["a", "b"]);
        Ok(())
    }

    #[test]
    fn the_tab_other_takes_only_what_no_tab_takes() -> Result<(), Failure> {
        let mut layout = Layout::default();
        apply(
            &mut layout,
            &[],
            NOMINAL,
            &Op::MatchTab {
                tab: "all".to_owned(),
                tests: vec![("name".to_owned(), "build".to_owned())],
            },
        )?;
        apply(
            &mut layout,
            &[],
            NOMINAL,
            &Op::NewWorkspace {
                title: Some("two".to_owned()),
            },
        )?;
        apply(
            &mut layout,
            &[],
            NOMINAL,
            &Op::MatchTab {
                tab: "1".to_owned(),
                tests: vec![("name".to_owned(), "logs".to_owned())],
            },
        )?;
        let panes = [
            live("logs", 1, &[]),
            live("misc", 2, &[]),
            live("odd", 3, &[]),
        ];
        let tab_of = |layout: &Layout, name: &str, n: u8| {
            layout
                .shown(&panes)
                .tab_of(&pane(name, n))
                .map(|t| t.title.clone())
                .unwrap_or_default()
        };
        assert_eq!(tab_of(&layout, "logs", 1), "1");
        assert_eq!(tab_of(&layout, "misc", 2), "other");
        // Stored by a rename, it still takes nothing another tab takes.
        apply(
            &mut layout,
            &panes,
            NOMINAL,
            &Op::RenameTab {
                tab: "other".to_owned(),
                title: "spare".to_owned(),
            },
        )?;
        assert_eq!(tab_of(&layout, "logs", 1), "1");
        assert_eq!(tab_of(&layout, "misc", 2), "spare");
        // Two panes there swap.
        apply(
            &mut layout,
            &panes,
            NOMINAL,
            &Op::Swap(target("misc"), target("odd")),
        )?;
        assert!(encode(&layout).is_ok());
        Ok(())
    }

    #[test]
    fn a_zoom_follows_no_pane_out_of_its_tab_and_no_one_moves_into_nothing() -> Result<(), Failure>
    {
        let (mut layout, panes) = three_columns()?;
        apply(&mut layout, &panes, NOMINAL, &Op::Zoom(target("a")))?;
        apply(
            &mut layout,
            &panes,
            NOMINAL,
            &Op::Swap(target("a"), target("d")),
        )?;
        assert!(layout.tabs().all(|t| t.zoom.is_none()), "{layout}");
        // A column with no room is no neighbour.
        let mut layout = Layout::default();
        let text = format!(
            "{HEADER}\nversion 1\nnext 3\nworkspace w1 \"main\"\ntab t2 \"all\"\n  split columns\n    \
             500 pane a@{}\n    1 pane b@{}\n    499 pane c@{}\n",
            PaneId([1; 16]),
            PaneId([2; 16]),
            PaneId([3; 16])
        );
        layout = parse(&text).map_err(invalid).unwrap_or(layout);
        let area = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 40,
        };
        let tab = layout
            .tabs()
            .next()
            .cloned()
            .unwrap_or_else(|| Tab::new(2, "x"));
        assert_eq!(
            tab.neighbor(&pane("a", 1), Dir::Right, area),
            Some(pane("c", 3))
        );
        Ok(())
    }

    #[test]
    fn a_border_moved_a_cell_moves_one_cell_at_any_width() -> Result<(), Failure> {
        for w in (0u32..26).map(|i| i.saturating_mul(7).saturating_add(40)) {
            let (mut layout, panes) = three_columns()?;
            let area = Rect {
                x: 0,
                y: 0,
                w,
                h: 30,
            };
            let edge = |layout: &Layout| -> u32 {
                let tab = layout
                    .tabs()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| Tab::new(2, "x"));
                tab.tiles(area)
                    .iter()
                    .find(|(p, _)| p.name == "a")
                    .map_or(0, |(_, r)| r.w)
            };
            for _ in 0..5 {
                let was = edge(&layout);
                apply(
                    &mut layout,
                    &panes,
                    area,
                    &Op::Border {
                        pane: target("a"),
                        dir: Dir::Right,
                        amount: Amount::Cells(1),
                    },
                )?;
                assert_eq!(edge(&layout), was.saturating_add(1), "at width {w}");
            }
        }
        // A pane still there is not replaced: it would be routed beside.
        let (mut layout, panes) = three_columns()?;
        let refused = apply(
            &mut layout,
            &panes,
            NOMINAL,
            &Op::Replace {
                old: target("a"),
                new: target("d"),
            },
        );
        assert!(refused.is_err());
        Ok(())
    }

    #[test]
    fn borders_move_within_bounds_and_removal_closes_the_gap() -> Result<(), Failure> {
        let mut layout = Layout::default();
        let panes = [live("a", 1, &[]), live("b", 2, &[])];
        apply(&mut layout, &panes, NOMINAL, &Op::Zoom(target("a")))?;
        let width = |layout: &Layout, name: &str, n: u8| -> u32 {
            let shown = layout.shown(&panes);
            let tab = shown
                .tabs()
                .next()
                .cloned()
                .unwrap_or_else(|| Tab::new(1, "x"));
            let tiles = tab.tiles(Rect {
                x: 0,
                y: 0,
                w: 101,
                h: 10,
            });
            tiles
                .iter()
                .find(|(p, _)| *p == pane(name, n))
                .map_or(0, |(_, r)| r.w)
        };
        apply(&mut layout, &panes, NOMINAL, &Op::Zoom(target("a")))?;
        // 101 columns: a's half ends at 50, its last column the border.
        assert_eq!(width(&layout, "a", 1), 49);
        let area = Rect {
            x: 0,
            y: 0,
            w: 101,
            h: 10,
        };
        apply(
            &mut layout,
            &panes,
            area,
            &Op::Border {
                pane: target("a"),
                dir: Dir::Right,
                amount: Amount::Cells(10),
            },
        )?;
        assert_eq!(width(&layout, "a", 1), 59);
        // As far as it goes: the other side keeps a cell, and a border
        // that cannot move further says so.
        apply(
            &mut layout,
            &panes,
            area,
            &Op::Border {
                pane: target("b"),
                dir: Dir::Right,
                amount: Amount::Percent(100),
            },
        )?;
        assert!(width(&layout, "b", 2) >= 1, "{}", width(&layout, "b", 2));
        let stuck = apply(
            &mut layout,
            &panes,
            area,
            &Op::Border {
                pane: target("b"),
                dir: Dir::Right,
                amount: Amount::Cells(1),
            },
        );
        assert!(stuck.is_err(), "{stuck:?}");
        apply(&mut layout, &panes, area, &Op::Remove(target("b")))?;
        let stored = layout.tabs().next().and_then(|t| t.root.clone());
        assert!(matches!(stored, Some(Node::Leaf(_))), "{stored:?}");
        Ok(())
    }
}

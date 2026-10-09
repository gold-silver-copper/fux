//! A tab's layout: a tree of splits whose leaves are panes, and the
//! rectangles it gives each pane at a given size.
use crate::id::PaneId;
use crate::keys::Direction;
use std::num::NonZeroU64;
use std::ops::Range;

/// How a split arranges its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Side by side, divided by vertical separators (`split -h`).
    Horizontal,
    /// Stacked, divided by horizontal separators (`split -v`).
    Vertical,
}

impl Axis {
    pub fn of(direction: Direction) -> Axis {
        match direction {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Pane(PaneId),
    /// Children with their weights; never fewer than two after `normalize`.
    Split {
        axis: Axis,
        children: Vec<(u32, Node)>,
    },
}

/// The weight a new child starts with.
pub const WEIGHT: u32 = 1000;
/// The smallest pane, in each dimension.
pub const MIN: u16 = 2;

/// The separators between `children` side by side. Past what a u16 screen
/// holds the count saturates, as the lengths added to it do.
fn separators(children: usize) -> u16 {
    u16::try_from(children.saturating_sub(1)).unwrap_or(u16::MAX)
}

/// A rectangle of cells on a screen. Its right and bottom edges are within a
/// u16, as a screen's are: one is made only as a whole screen
/// ([`Rect::screen`]) or cut from another ([`Rect::split`], [`Rect::corner`],
/// [`Rect::lines`]), so every position inside one is exact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

impl Rect {
    /// A whole screen of `rows` by `cols`.
    pub fn screen(rows: u16, cols: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: cols,
            h: rows,
        }
    }
    pub fn x(&self) -> u16 {
        self.x
    }
    pub fn y(&self) -> u16 {
        self.y
    }
    pub fn w(&self) -> u16 {
        self.w
    }
    pub fn h(&self) -> u16 {
        self.h
    }
    // Exact: a rect is inside a screen.
    pub fn right(&self) -> u16 {
        self.x.saturating_add(self.w)
    }
    pub fn bottom(&self) -> u16 {
        self.y.saturating_add(self.h)
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    /// How many cells it has; exact in a u32.
    pub fn area(&self) -> u32 {
        u32::from(self.w).saturating_mul(u32::from(self.h))
    }
    /// Its rows on the screen, top to bottom.
    pub fn rows(&self) -> Range<u16> {
        self.y..self.bottom()
    }
    /// Its columns on the screen, left to right.
    pub fn cols(&self) -> Range<u16> {
        self.x..self.right()
    }
    /// Each of its rows, as a rect one row high.
    pub fn lines(self) -> impl Iterator<Item = Rect> {
        self.rows().map(move |y| Rect { y, h: 1, ..self })
    }
    /// Where (y, x) of the rect is on the screen; none outside it.
    pub fn at(&self, y: u16, x: u16) -> Option<(u16, u16)> {
        (y < self.h && x < self.w).then(|| (self.y.saturating_add(y), self.x.saturating_add(x)))
    }
    /// Whether (y, x) on the screen is inside the rect.
    pub fn contains(&self, y: u16, x: u16) -> bool {
        self.rows().contains(&y) && self.cols().contains(&x)
    }
    /// Whether (y, x) on the screen is inside the rect or beside it, edges
    /// and corners: within a cell of it.
    pub fn near(&self, y: u16, x: u16) -> bool {
        y.saturating_add(1) >= self.y
            && y <= self.bottom()
            && x.saturating_add(1) >= self.x
            && x <= self.right()
    }
    /// (y, x) on the screen as a position of the rect, moved to its nearest
    /// cell if outside; (0, 0) in an empty one.
    pub fn clamp(&self, y: u16, x: u16) -> (u16, u16) {
        let inside =
            |at: u16, start: u16, len: u16| at.saturating_sub(start).min(len.saturating_sub(1));
        (inside(y, self.y, self.h), inside(x, self.x, self.w))
    }
    /// Its length along `axis`.
    fn len(&self, axis: Axis) -> u16 {
        match axis {
            Axis::Horizontal => self.w,
            Axis::Vertical => self.h,
        }
    }
    /// Its first `len` cells along `axis`, or all of it if it has fewer,
    /// and the rest.
    pub fn split(self, axis: Axis, len: u16) -> (Rect, Rect) {
        let len = len.min(self.len(axis));
        let rest = self.len(axis).saturating_sub(len);
        match axis {
            Axis::Horizontal => (
                Rect { w: len, ..self },
                Rect {
                    x: self.x.saturating_add(len),
                    w: rest,
                    ..self
                },
            ),
            Axis::Vertical => (
                Rect { h: len, ..self },
                Rect {
                    y: self.y.saturating_add(len),
                    h: rest,
                    ..self
                },
            ),
        }
    }
    /// Its bottom-right corner, at most `h` by `w`.
    pub fn corner(self, h: u16, w: u16) -> Rect {
        let (h, w) = (h.min(self.h), w.min(self.w));
        Rect {
            x: self.right().saturating_sub(w),
            y: self.bottom().saturating_sub(h),
            w,
            h,
        }
    }
    /// Twice the centre, so that it is whole.
    fn centre2(&self) -> (u32, u32) {
        (
            u32::from(self.x).saturating_add(u32::from(self.right())),
            u32::from(self.y).saturating_add(u32::from(self.bottom())),
        )
    }
}

/// A separator line, a rect one cell wide or high: vertical between
/// side-by-side children, horizontal between stacked ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Separator {
    /// The axis of the split it divides: a `Horizontal` split's line is
    /// vertical.
    pub axis: Axis,
    pub rect: Rect,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    pub panes: Vec<(PaneId, Rect)>,
    pub separators: Vec<Separator>,
    /// Room for each split's sizes while it is placed; empty between
    /// placements.
    scratch: Vec<u16>,
}

impl Placement {
    /// Nothing placed, the buffers kept.
    pub fn clear(&mut self) {
        self.panes.clear();
        self.separators.clear();
    }
    pub fn rect(&self, pane: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r)
    }
}

impl Node {
    /// Every pane, in tree order: for when a list is needed, as to step
    /// through it by index.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.for_each_pane(&mut |pane| out.push(pane));
        out
    }
    /// Calls `f` with every pane, in tree order.
    pub fn for_each_pane(&self, f: &mut impl FnMut(PaneId)) {
        match self {
            Node::Pane(p) => f(*p),
            Node::Split { children, .. } => {
                for (_, child) in children {
                    child.for_each_pane(f);
                }
            }
        }
    }
    /// The first pane in tree order.
    pub fn first_pane(&self) -> Option<PaneId> {
        match self {
            Node::Pane(p) => Some(*p),
            Node::Split { children, .. } => children.first()?.1.first_pane(),
        }
    }
    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Node::Pane(p) => *p == pane,
            Node::Split { children, .. } => children.iter().any(|(_, c)| c.contains(pane)),
        }
    }

    /// The smallest length this node can have along `axis`.
    fn min_len(&self, axis: Axis) -> u16 {
        match self {
            Node::Pane(_) => MIN,
            Node::Split {
                axis: own,
                children,
            } => {
                let mins = children.iter().map(|(_, c)| c.min_len(axis));
                if *own == axis {
                    mins.fold(separators(children.len()), u16::saturating_add)
                } else {
                    mins.max().unwrap_or(MIN)
                }
            }
        }
    }
}

/// Which side of its target a split puts the new pane: before it, left or
/// above, or after it, right or below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Before,
    After,
}

/// Adds `new` beside `target`, on `side` of it, along `axis`. A split
/// already along `axis` takes it as a sibling sharing the target's weight;
/// otherwise the target becomes a new split of the two. An empty tree becomes
/// the new pane alone. False if `target` is not in the tree.
pub fn split(root: &mut Option<Node>, target: PaneId, new: PaneId, axis: Axis, side: Side) -> bool {
    let Some(node) = root else {
        *root = Some(Node::Pane(new));
        return true;
    };
    let done = insert(node, target, new, axis, side);
    normalize(node);
    done
}

fn insert(node: &mut Node, target: PaneId, new: PaneId, axis: Axis, side: Side) -> bool {
    match node {
        Node::Pane(p) if *p == target => {
            let pair = match side {
                Side::After => vec![(WEIGHT, Node::Pane(target)), (WEIGHT, Node::Pane(new))],
                Side::Before => vec![(WEIGHT, Node::Pane(new)), (WEIGHT, Node::Pane(target))],
            };
            *node = Node::Split {
                axis,
                children: pair,
            };
            true
        }
        Node::Pane(_) => false,
        Node::Split {
            axis: own,
            children,
        } => {
            let index = children
                .iter()
                .position(|(_, c)| matches!(c, Node::Pane(p) if *p == target));
            if let Some(index) = index
                && *own == axis
            {
                let Some((weight, _)) = children.get_mut(index) else {
                    return false;
                };
                let half = (*weight / 2).max(1);
                *weight = weight.saturating_sub(half).max(1);
                let at = match side {
                    Side::After => index.checked_add(1),
                    Side::Before => Some(index),
                };
                let Some(at) = at else {
                    return false;
                };
                // The new pane goes in at `at`, before the rest.
                let mut rest = std::mem::take(children).into_iter();
                let mut placed: Vec<_> = rest.by_ref().take(at).collect();
                placed.push((half, Node::Pane(new)));
                placed.extend(rest);
                *children = placed;
                return true;
            }
            children
                .iter_mut()
                .any(|(_, c)| insert(c, target, new, axis, side))
        }
    }
}

/// Removes a pane. An emptied split disappears, a split left with one child
/// becomes that child, and nested splits along one axis merge. The tree is
/// `None` once its last pane is gone.
pub fn remove(root: &mut Option<Node>, pane: PaneId) -> bool {
    let Some(node) = root else {
        return false;
    };
    if matches!(node, Node::Pane(p) if *p == pane) {
        *root = None;
        return true;
    }
    let removed = remove_from(node, pane);
    normalize(node);
    removed
}

fn remove_from(node: &mut Node, pane: PaneId) -> bool {
    let Node::Split { children, .. } = node else {
        return false;
    };
    // A pane is in the tree once, so this removes it or nothing.
    let before = children.len();
    children.retain(|(_, c)| !matches!(c, Node::Pane(p) if *p == pane));
    if children.len() < before {
        return true;
    }
    children.iter_mut().any(|(_, c)| remove_from(c, pane))
}

/// Collapses one-child splits and merges a child split along its parent's
/// axis into the parent, scaling its weights to the share it had.
pub fn normalize(node: &mut Node) {
    let Node::Split { axis, children } = node else {
        return;
    };
    for (_, child) in children.iter_mut() {
        normalize(child);
    }
    children.retain(|(_, c)| !matches!(c, Node::Split { children, .. } if children.is_empty()));
    let axis = *axis;
    let mut merged = Vec::with_capacity(children.len());
    for (weight, child) in std::mem::take(children) {
        match child {
            Node::Split {
                axis: inner,
                children: grand,
            } if inner == axis => {
                let total: u64 = grand.iter().map(|(w, _)| u64::from(*w)).sum();
                let total = NonZeroU64::new(total).unwrap_or(NonZeroU64::MIN);
                for (w, g) in grand {
                    // A u32 by a u32 is exact in a u64.
                    let scaled = (u64::from(weight).saturating_mul(u64::from(w)) / total).max(1);
                    merged.push((u32::try_from(scaled).unwrap_or(u32::MAX), g));
                }
            }
            other @ (Node::Pane(_) | Node::Split { .. }) => merged.push((weight, other)),
        }
    }
    *children = merged;
    if children.len() == 1
        && let Some((_, only)) = children.pop()
    {
        *node = only;
    }
}

/// Swaps two panes' places in one tree, or, given two trees in turn, each
/// takes the other's place.
pub fn swap(node: &mut Node, a: PaneId, b: PaneId) {
    match node {
        Node::Pane(p) if *p == a => *p = b,
        Node::Pane(p) if *p == b => *p = a,
        Node::Pane(_) => {}
        Node::Split { children, .. } => {
            for (_, child) in children {
                swap(child, a, b);
            }
        }
    }
}

/// Shares `len` cells among `children` by weight, into `sizes`, which are
/// zeros, giving each at least its minimum in `mins`; `len` is at least
/// their minimums together (`place_split` lays out a split too small for
/// them without it). `mins` is spent: it ends up 0 for the children that
/// shared what was left, rather than being fixed at their minimum.
fn distribute(len: u16, children: &[(u32, Node)], sizes: &mut [u16], mins: &mut [u16]) {
    let weight = |i: usize| u64::from(children.get(i).map_or(1, |(w, _)| *w).max(1));
    // A child whose share falls short of its minimum is fixed at it, and the
    // rest share again. A minimum is never 0, so a size of 0 is a child not
    // fixed yet.
    loop {
        // The fixed children have their minimums, which together fit.
        let fixed_len: u32 = sizes.iter().map(|s| u32::from(*s)).sum();
        let Some(free_len) = u32::from(len).checked_sub(fixed_len) else {
            return;
        };
        let free_weight: u64 = sizes
            .iter()
            .zip(0..)
            .filter(|(s, _)| **s == 0)
            .map(|(_, i)| weight(i))
            .sum();
        let free_weight = NonZeroU64::new(free_weight).unwrap_or(NonZeroU64::MIN);
        // A u32 by a u32 is exact in a u64.
        let exact = |i: usize| u64::from(free_len).saturating_mul(weight(i));
        let share = |i: usize| exact(i) / free_weight;
        let mut changed = false;
        for ((size, min), i) in sizes.iter_mut().zip(mins.iter()).zip(0..) {
            if *size == 0 && share(i) < u64::from(*min) {
                *size = *min;
                changed = true;
            }
        }
        if changed {
            continue;
        }
        // A share is a fraction of `free_len`, which fits `len`.
        let cells = |i: usize| u16::try_from(share(i)).unwrap_or(len);
        let used = sizes
            .iter()
            .zip(0..)
            .filter(|(s, _)| **s == 0)
            // The shares are parts of `free_len`.
            .fold(0u32, |used, (_, i)| {
                used.saturating_add(u32::from(cells(i)))
            });
        // Fewer cells are left over than there are children sharing: one
        // each to the largest fractions of a cell, the first on a tie.
        let left = free_len.saturating_sub(used);
        for (min, size) in mins.iter_mut().zip(sizes.iter()) {
            if *size == 0 {
                *min = 0;
            }
        }
        let fraction = |i: usize| exact(i) % free_weight;
        for (size, i) in sizes.iter_mut().zip(0..) {
            if mins.get(i) != Some(&0) {
                continue;
            }
            let ahead = mins
                .iter()
                .zip(0..)
                .filter(|(m, j)| {
                    **m == 0
                        && (fraction(*j) > fraction(i) || fraction(*j) == fraction(i) && *j < i)
                })
                .count();
            let extra = u32::try_from(ahead).is_ok_and(|ahead| ahead < left);
            *size = cells(i);
            if extra && let Some(grown) = size.checked_add(1) {
                *size = grown;
            }
        }
        return;
    }
}

/// The rectangles of every pane that fits in `area`, and the separators.
pub fn place(root: &Node, area: Rect) -> Placement {
    let mut out = Placement::default();
    place_into(root, area, &mut out);
    out
}

/// Places `root` in `area` into `out`, whatever it held, reusing its
/// buffers: a placement made into again allocates nothing.
pub fn place_into(root: &Node, area: Rect, out: &mut Placement) {
    out.clear();
    if !area.is_empty() {
        place_node(root, area, out);
    }
}

fn place_node(node: &Node, area: Rect, out: &mut Placement) {
    match node {
        Node::Pane(p) => out.panes.push((*p, area)),
        Node::Split { axis, children } => place_split(*axis, children, area, out),
    }
}

/// Places a split's `children`, side by side along `axis`, in `area`.
fn place_split(axis: Axis, children: &[(u32, Node)], area: Rect, out: &mut Placement) {
    let along = area.len(axis);
    let separators = separators(children.len());
    // The children's sizes, then their minimums, above those of the splits
    // this one is inside, until it is placed.
    let base = out.scratch.len();
    out.scratch.extend(children.iter().map(|_| 0));
    out.scratch
        .extend(children.iter().map(|(_, c)| c.min_len(axis)));
    let (sizes, mins) = out
        .scratch
        .get_mut(base..)
        .and_then(|s| s.split_at_mut_checked(children.len()))
        .unwrap_or_default();
    let needed = mins.iter().fold(separators, |a, m| a.saturating_add(*m));
    let room = along.checked_sub(separators).filter(|_| along >= needed);
    if let Some(room) = room {
        distribute(room, children, sizes, mins);
    } else {
        // Too small for all: children in order while they fit, each after
        // the first needing a separator; the last one shown takes what is
        // left.
        let mut left = along;
        let mut last = None;
        for (i, (size, min)) in sizes.iter_mut().zip(mins.iter()).enumerate() {
            let cost = min.saturating_add(u16::from(i > 0));
            let Some(rest) = left.checked_sub(cost) else {
                break;
            };
            left = rest;
            *size = *min;
            last = Some(i);
        }
        // What is left fits: the sizes add up to at most `along`.
        if let Some(size) = last.and_then(|i| sizes.get_mut(i)) {
            *size = size.saturating_add(left);
        }
    }
    // Each child shown is cut from what is left of `area`, after a
    // separator if one was before it.
    let mut rest = area;
    let mut first = true;
    for (i, (_, child)) in children.iter().enumerate() {
        let size = out.scratch.get(base..).and_then(|s| s.get(i)).copied();
        let Some(size) = size.filter(|s| *s > 0) else {
            continue;
        };
        if !std::mem::replace(&mut first, false) {
            let (line, after) = rest.split(axis, 1);
            out.separators.push(Separator { axis, rect: line });
            rest = after;
        }
        let (rect, after) = rest.split(axis, size);
        place_node(child, rect, out);
        rest = after;
    }
    out.scratch.truncate(base);
}

/// The pane in `direction` from `from`, among the placed panes: rectangles
/// beyond `from`'s edge, ranked by the distance between centres across the
/// direction, then by the distance along it, then by ID.
pub fn neighbor(placement: &Placement, from: PaneId, direction: Direction) -> Option<PaneId> {
    let source = placement.rect(from)?;
    let (sx, sy) = source.centre2();
    placement
        .panes
        .iter()
        .filter(|(p, _)| *p != from)
        .filter_map(|(p, r)| {
            let beyond = match direction {
                Direction::Left => r.right() <= source.x,
                Direction::Right => r.x >= source.right(),
                Direction::Up => r.bottom() <= source.y,
                Direction::Down => r.y >= source.bottom(),
            };
            if !beyond {
                return None;
            }
            let (cx, cy) = r.centre2();
            let (cross, forward) = match direction {
                Direction::Left | Direction::Right => (sy.abs_diff(cy), sx.abs_diff(cx)),
                Direction::Up | Direction::Down => (sx.abs_diff(cx), sy.abs_diff(cy)),
            };
            Some(((cross, forward, *p), *p))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, p)| p)
}

/// Resizes `pane` by `amount` cells in `direction`, as placed in `area`:
/// the nearest split along the direction's axis that has a sibling on that
/// side moves the border between them, growing the pane's side. Only when
/// no split has one does the nearest split along the axis shrink the pane
/// from its far side instead, moving that border the same way. Weights
/// become the new cell sizes.
pub fn resize(
    node: &mut Node,
    area: Rect,
    pane: PaneId,
    direction: Direction,
    amount: u16,
) -> bool {
    resize_by(node, area, pane, direction, amount, true)
        || resize_by(node, area, pane, direction, amount, false)
}

/// `resize`, at one split and those below it: with `toward`, only a split
/// with a sibling on the direction's side acts; without, the nearest along
/// the axis does, from whichever side it can.
fn resize_by(
    node: &mut Node,
    area: Rect,
    pane: PaneId,
    direction: Direction,
    amount: u16,
    toward: bool,
) -> bool {
    let Node::Split { axis, children } = node else {
        return false;
    };
    let Some(index) = children.iter().position(|(_, c)| c.contains(pane)) else {
        return false;
    };
    // Deeper splits first: the border nearest the pane moves.
    let mut placement = Placement::default();
    if !area.is_empty() {
        place_split(*axis, children, area, &mut placement);
    }
    let mut sizes: Vec<u16> = Vec::with_capacity(children.len());
    let mut inner = Rect::default();
    for (i, (_, child)) in children.iter().enumerate() {
        let rect = placed_span(*axis, child, area, &placement);
        sizes.push(rect.len(*axis));
        if i == index {
            inner = rect;
        }
    }
    if let Some((_, child)) = children.get_mut(index)
        && resize_by(child, inner, pane, direction, amount, toward)
    {
        return true;
    }
    if *axis != Axis::of(direction) {
        return false;
    }
    let toward_start = matches!(direction, Direction::Left | Direction::Up);
    // Grow toward the direction when there is a neighbour that way; else
    // shrink from the far side, moving the other border the same way.
    let before = index.checked_sub(1);
    let after = index.checked_add(1).filter(|i| *i < children.len());
    let (grow, shrink) = match (toward_start, before, after) {
        (true, Some(before), _) => (index, before),
        (false, _, Some(after)) => (index, after),
        _ if toward => return false,
        (true, None, Some(after)) => (after, index),
        (false, Some(before), None) => (before, index),
        (_, None, None) => return false,
    };
    let min = children.get(shrink).map_or(MIN, |(_, c)| c.min_len(*axis));
    let Ok([grown, shrunk]) = sizes.get_disjoint_mut([grow, shrink]) else {
        return false;
    };
    // One border moves: what one side gains, the other gives, within `area`.
    let moved = amount.min(shrunk.saturating_sub(min));
    if moved == 0 {
        return true;
    }
    *grown = grown.saturating_add(moved);
    *shrunk = shrunk.saturating_sub(moved);
    for ((weight, _), size) in children.iter_mut().zip(&sizes) {
        *weight = u32::from(*size).max(1);
    }
    true
}

/// The rect a child of a split along `axis` in `area` has in `placement`:
/// along the axis, the span of its placed panes, none if none is; across,
/// all of `area`.
fn placed_span(axis: Axis, child: &Node, area: Rect, placement: &Placement) -> Rect {
    let mut span: Option<Range<u16>> = None;
    child.for_each_pane(&mut |p| {
        if let Some(r) = placement.rect(p) {
            let (start, end) = match axis {
                Axis::Horizontal => (r.x, r.right()),
                Axis::Vertical => (r.y, r.bottom()),
            };
            span = Some(
                span.as_ref()
                    .map_or(start..end, |s| s.start.min(start)..s.end.max(end)),
            );
        }
    });
    // Inside `area`, as the panes are.
    let Range { start, end } = span.unwrap_or_default();
    let len = end.saturating_sub(start);
    match axis {
        Axis::Horizontal => Rect {
            x: start,
            w: len,
            ..area
        },
        Axis::Vertical => Rect {
            y: start,
            h: len,
            ..area
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u32) -> PaneId {
        PaneId::of(n)
    }
    fn area(w: u16, h: u16) -> Rect {
        Rect::screen(h, w)
    }
    /// `0 | 1`.
    fn tree() -> Option<Node> {
        let mut root = Some(Node::Pane(p(0)));
        split(&mut root, p(0), p(1), Axis::Horizontal, Side::After);
        root
    }

    #[test]
    fn splitting_shares_the_space_with_one_separator() {
        let root = tree();
        let Some(node) = &root else { return };
        let placed = place(node, area(80, 24));
        assert_eq!(
            placed.panes,
            vec![
                (
                    p(0),
                    Rect {
                        x: 0,
                        y: 0,
                        w: 40,
                        h: 24
                    }
                ),
                (
                    p(1),
                    Rect {
                        x: 41,
                        y: 0,
                        w: 39,
                        h: 24
                    }
                )
            ]
        );
        assert_eq!(
            placed.separators,
            vec![Separator {
                axis: Axis::Horizontal,
                rect: Rect {
                    x: 40,
                    y: 0,
                    w: 1,
                    h: 24
                }
            }]
        );
        // A second split of the right pane shares its weight: 1000:500:500.
        let mut root = tree();
        assert!(split(&mut root, p(1), p(2), Axis::Horizontal, Side::After));
        let Some(node) = &root else { return };
        let widths: Vec<u16> = place(node, area(80, 24))
            .panes
            .iter()
            .map(|(_, r)| r.w)
            .collect();
        assert_eq!(widths, vec![39, 20, 19]);
    }

    #[test]
    fn nested_splits_place_every_pane_without_overlap() {
        let mut root = tree();
        split(&mut root, p(1), p(2), Axis::Horizontal, Side::After);
        split(&mut root, p(2), p(3), Axis::Vertical, Side::After);
        split(&mut root, p(1), p(4), Axis::Vertical, Side::Before);
        let Some(node) = &root else { return };
        assert_eq!(node.panes(), vec![p(0), p(4), p(1), p(2), p(3)]);
        let placed = place(node, area(81, 25));
        assert_eq!(placed.panes.len(), 5);
        let mut covered = vec![0u8; 81 * 25];
        let separators = placed.separators.iter().map(|s| &s.rect);
        for r in placed.panes.iter().map(|(_, r)| r).chain(separators) {
            for (y, x) in r.rows().flat_map(|y| r.cols().map(move |x| (y, x))) {
                if let Some(c) = covered.get_mut(usize::from(y) * 81 + usize::from(x)) {
                    *c += 1;
                }
            }
        }
        assert!(
            covered.iter().all(|c| *c == 1),
            "every cell is a pane or a separator, once"
        );
    }

    /// A placement made into again, whatever it held, is a fresh one.
    #[test]
    fn placing_into_a_used_placement_is_placing_afresh() {
        let mut root = tree();
        split(&mut root, p(0), p(1), Axis::Horizontal, Side::After);
        split(&mut root, p(1), p(2), Axis::Vertical, Side::After);
        let Some(node) = &root else { return };
        let mut used = place(node, area(120, 40));
        for (w, h) in [(80, 24), (3, 3), (0, 9), (200, 60)] {
            place_into(node, area(w, h), &mut used);
            assert_eq!(used, place(node, area(w, h)), "{w}x{h}");
        }
        place_into(&Node::Pane(p(7)), area(10, 10), &mut used);
        assert_eq!(used.panes, vec![(p(7), area(10, 10))]);
        assert!(used.separators.is_empty());
    }

    #[test]
    fn a_small_area_keeps_minimums_and_hides_what_does_not_fit() {
        let mut root = tree();
        for n in 2..10 {
            split(&mut root, p(n - 1), p(n), Axis::Horizontal, Side::After);
        }
        let Some(node) = &root else { return };
        let placed = place(node, area(10, 3));
        assert!(placed.panes.iter().all(|(_, r)| r.w >= MIN && r.h >= MIN));
        let used: usize = placed
            .panes
            .iter()
            .map(|(_, r)| usize::from(r.w))
            .sum::<usize>()
            + placed.separators.len();
        assert_eq!(used, 10);
        assert!(placed.panes.len() < 9);
        assert!(place(node, area(0, 5)).panes.is_empty());
    }

    #[test]
    fn removal_collapses_and_merges() {
        let mut root = tree();
        split(&mut root, p(1), p(2), Axis::Vertical, Side::After);
        split(&mut root, p(2), p(3), Axis::Horizontal, Side::After);
        assert!(remove(&mut root, p(1)));
        // (0 | (1 / (2 | 3))) minus 1 is (0 | 2 | 3), merged along one axis.
        assert!(
            matches!(&root, Some(Node::Split { axis: Axis::Horizontal, children }) if children.len() == 3)
        );
        remove(&mut root, p(2));
        remove(&mut root, p(3));
        assert_eq!(root, Some(Node::Pane(p(0))));
        assert!(remove(&mut root, p(0)));
        assert_eq!(root, None);
        assert!(!remove(&mut root, p(0)));
        let mut empty = None;
        assert!(split(&mut empty, p(9), p(5), Axis::Vertical, Side::After));
        assert_eq!(empty, Some(Node::Pane(p(5))));
    }

    #[test]
    fn directional_neighbours_prefer_alignment_then_distance_then_id() {
        // 0 | (1 / 2)
        let mut root = tree();
        split(&mut root, p(1), p(2), Axis::Vertical, Side::After);
        let Some(node) = &root else { return };
        let placed = place(node, area(80, 24));
        assert_eq!(neighbor(&placed, p(0), Direction::Right), Some(p(1)));
        assert_eq!(neighbor(&placed, p(2), Direction::Left), Some(p(0)));
        assert_eq!(neighbor(&placed, p(1), Direction::Down), Some(p(2)));
        assert_eq!(neighbor(&placed, p(2), Direction::Up), Some(p(1)));
        assert_eq!(neighbor(&placed, p(0), Direction::Left), None);
        assert_eq!(neighbor(&placed, p(1), Direction::Right), None);
    }

    #[test]
    fn resizing_moves_the_nearest_border_and_respects_minimums() {
        let mut root = tree();
        let Some(node) = &mut root else { return };
        let a = area(81, 24);
        assert!(resize(node, a, p(0), Direction::Right, 10));
        let placed = place(node, a);
        assert_eq!(placed.rect(p(0)).map(|r| r.w), Some(50));
        assert_eq!(placed.rect(p(1)).map(|r| r.w), Some(30));
        // Left from the leftmost pane shrinks it, moving its right border.
        assert!(resize(node, a, p(0), Direction::Left, 5));
        assert_eq!(place(node, a).rect(p(0)).map(|r| r.w), Some(45));
        // Never below the minimum.
        assert!(resize(node, a, p(1), Direction::Left, 200));
        assert_eq!(place(node, a).rect(p(0)).map(|r| r.w), Some(MIN));
        // No split along the vertical axis: nothing to move.
        assert!(!resize(node, a, p(0), Direction::Up, 1));
    }

    /// In nested splits the border that moves is the nearest one on the
    /// side asked for, however deep the pane is: `0 | (1 / (2 | 3))`,
    /// left from 2 moves the border between 0 and the rest, not 2's own
    /// right border (which would shrink 2 while 0 stood still).
    #[test]
    fn resizing_in_nested_splits_moves_the_border_on_that_side() {
        let mut root = tree();
        split(&mut root, p(1), p(2), Axis::Vertical, Side::After);
        split(&mut root, p(2), p(3), Axis::Horizontal, Side::After);
        let Some(node) = &mut root else { return };
        let a = area(81, 24);
        let before = place(node, a);
        let width = |placed: &Placement, n| placed.rect(p(n)).map(|r| r.w);
        assert!(resize(node, a, p(2), Direction::Left, 5));
        let after = place(node, a);
        assert_eq!(width(&after, 0), width(&before, 0).map(|w| w - 5));
        assert_eq!(width(&after, 1), width(&before, 1).map(|w| w + 5));
        assert_eq!(
            after.rect(p(2)).map(|r| r.x),
            before.rect(p(2)).map(|r| r.x - 5)
        );
        // Right from 3, at the right edge: no split has a sibling there, so
        // 3 shrinks from its left, the nearest border along the axis.
        let before = after;
        assert!(resize(node, a, p(3), Direction::Right, 2));
        let after = place(node, a);
        assert_eq!(width(&after, 3), width(&before, 3).map(|w| w - 2));
        assert_eq!(width(&after, 0), width(&before, 0));
    }

    #[test]
    fn swapping_exchanges_places() {
        let mut root = tree();
        let Some(node) = &mut root else { return };
        swap(node, p(0), p(1));
        assert_eq!(node.panes(), vec![p(1), p(0)]);
    }
}

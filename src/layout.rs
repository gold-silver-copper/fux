//! A tab's layout: a tree of splits whose leaves are panes, and the
//! rectangles it gives each pane at a given size.
use crate::keys::Direction;
use std::num::NonZeroU64;

/// A pane's number, `%N` on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaneId(pub u32);

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "%{}", self.0)
    }
}

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    /// Where (y, x) inside the rect is on the screen, if that is a position.
    pub fn at(&self, y: u16, x: u16) -> Option<(u16, u16)> {
        Some((self.y.checked_add(y)?, self.x.checked_add(x)?))
    }
    // Sums of two u16s are exact in a u32.
    fn right(&self) -> u32 {
        u32::from(self.x).saturating_add(u32::from(self.w))
    }
    fn bottom(&self) -> u32 {
        u32::from(self.y).saturating_add(u32::from(self.h))
    }
    /// Whether (x, y) is inside the rect.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && u32::from(x) < self.right() && y >= self.y && u32::from(y) < self.bottom()
    }
    /// Twice the centre, so that it is whole.
    fn centre2(&self) -> (u32, u32) {
        (
            u32::from(self.x).saturating_add(self.right()),
            u32::from(self.y).saturating_add(self.bottom()),
        )
    }
}

/// A separator line: vertical between side-by-side children, horizontal
/// between stacked ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Separator {
    /// The axis of the split it divides: a `Horizontal` split's line is
    /// vertical.
    pub axis: Axis,
    pub x: u16,
    pub y: u16,
    pub len: u16,
}

impl Separator {
    /// The cells it covers: a rectangle one cell wide or high.
    pub fn rect(&self) -> Rect {
        let (w, h) = match self.axis {
            Axis::Horizontal => (1, self.len),
            Axis::Vertical => (self.len, 1),
        };
        Rect {
            x: self.x,
            y: self.y,
            w,
            h,
        }
    }
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

    /// Replaces `old` with `new` wherever it is.
    pub fn replace(&mut self, old: PaneId, new: PaneId) -> bool {
        match self {
            Node::Pane(p) if *p == old => {
                *p = new;
                true
            }
            Node::Pane(_) => false,
            Node::Split { children, .. } => children.iter_mut().any(|(_, c)| c.replace(old, new)),
        }
    }
}

/// Adds `new` beside `target`, after it or before it, along `axis`. A split
/// already along `axis` takes it as a sibling sharing the target's weight;
/// otherwise the target becomes a new split of the two. An empty tree becomes
/// the new pane alone. False if `target` is not in the tree.
pub fn split(
    root: &mut Option<Node>,
    target: PaneId,
    new: PaneId,
    axis: Axis,
    after: bool,
) -> bool {
    let Some(node) = root else {
        *root = Some(Node::Pane(new));
        return true;
    };
    let done = insert(node, target, new, axis, after);
    normalize(node);
    done
}

fn insert(node: &mut Node, target: PaneId, new: PaneId, axis: Axis, after: bool) -> bool {
    match node {
        Node::Pane(p) if *p == target => {
            let pair = if after {
                vec![(WEIGHT, Node::Pane(target)), (WEIGHT, Node::Pane(new))]
            } else {
                vec![(WEIGHT, Node::Pane(new)), (WEIGHT, Node::Pane(target))]
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
                let at = if after {
                    index.checked_add(1)
                } else {
                    Some(index)
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
                .any(|(_, c)| insert(c, target, new, axis, after))
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

/// Swaps two panes' places in one tree.
pub fn swap(node: &mut Node, a: PaneId, b: PaneId) {
    // Via a placeholder no real pane uses.
    let hole = PaneId(u32::MAX);
    node.replace(a, hole);
    node.replace(b, a);
    node.replace(hole, b);
}

/// Shares `len` cells among `children` by weight, into `sizes`, which are
/// zeros, giving each at least its minimum in `mins`; `len` is at least
/// their minimums together (`place_split` lays out a split too small for
/// them without it). `mins` is spent: it ends up marking the children that
/// got more than their minimum.
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
    if area.w > 0 && area.h > 0 {
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
    let along = match axis {
        Axis::Horizontal => area.w,
        Axis::Vertical => area.h,
    };
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
        if let Some(size) = last.and_then(|i| sizes.get_mut(i))
            && let Some(grown) = size.checked_add(left)
        {
            *size = grown;
        }
    }
    // Where along the axis each child and separator starts. The sizes fit
    // `area`, so past the largest position nothing is placed.
    let start = match axis {
        Axis::Horizontal => area.x,
        Axis::Vertical => area.y,
    };
    let mut at = Some(start);
    let mut first = true;
    for (i, (_, child)) in children.iter().enumerate() {
        let size = out
            .scratch
            .get(base..)
            .and_then(|sizes| sizes.get(i))
            .copied()
            .unwrap_or(0);
        if size == 0 {
            continue;
        }
        if !first {
            let Some(x) = at else {
                break;
            };
            let separator = match axis {
                Axis::Horizontal => Separator {
                    axis,
                    x,
                    y: area.y,
                    len: area.h,
                },
                Axis::Vertical => Separator {
                    axis,
                    x: area.x,
                    y: x,
                    len: area.w,
                },
            };
            out.separators.push(separator);
            at = x.checked_add(1);
        }
        first = false;
        let Some(x) = at else {
            break;
        };
        let rect = match axis {
            Axis::Horizontal => Rect { x, w: size, ..area },
            Axis::Vertical => Rect {
                y: x,
                h: size,
                ..area
            },
        };
        place_node(child, rect, out);
        at = x.checked_add(size);
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
                Direction::Left => r.right() <= u32::from(source.x),
                Direction::Right => u32::from(r.x) >= source.right(),
                Direction::Up => r.bottom() <= u32::from(source.y),
                Direction::Down => u32::from(r.y) >= source.bottom(),
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
/// side moves the border between them. Weights become the new cell sizes.
pub fn resize(
    node: &mut Node,
    area: Rect,
    pane: PaneId,
    direction: Direction,
    amount: u16,
) -> bool {
    let Node::Split { axis, children } = node else {
        return false;
    };
    let Some(index) = children.iter().position(|(_, c)| c.contains(pane)) else {
        return false;
    };
    // Deeper splits first: the border nearest the pane moves.
    let mut placement = Placement::default();
    if area.w > 0 && area.h > 0 {
        place_split(*axis, children, area, &mut placement);
    }
    let child_area = child_rects(*axis, children, area, &placement);
    if let (Some((_, child)), Some(inner)) = (children.get_mut(index), child_area.get(index))
        && resize(child, *inner, pane, direction, amount)
    {
        return true;
    }
    if *axis != Axis::of(direction) {
        return false;
    }
    let sizes: Vec<u16> = child_area
        .iter()
        .map(|r| match axis {
            Axis::Horizontal => r.w,
            Axis::Vertical => r.h,
        })
        .collect();
    let toward_start = matches!(direction, Direction::Left | Direction::Up);
    // Grow toward the direction when there is a neighbour that way; else
    // shrink from the far side, moving the other border the same way.
    let before = index.checked_sub(1);
    let after = index.checked_add(1).filter(|i| *i < children.len());
    let (grow, shrink) = match (toward_start, before, after) {
        (true, Some(before), _) => (index, before),
        (true, None, Some(after)) => (after, index),
        (false, _, Some(after)) => (index, after),
        (false, Some(before), None) => (before, index),
        (_, None, None) => return false,
    };
    let min = children.get(shrink).map_or(MIN, |(_, c)| c.min_len(*axis));
    let available = sizes.get(shrink).copied().unwrap_or(0).saturating_sub(min);
    let moved = amount.min(available);
    if moved == 0 {
        return true;
    }
    // One border moves: what one side gains, the other gives, within `area`.
    let grown = sizes.get(grow).and_then(|s| s.checked_add(moved));
    let shrunk = sizes.get(shrink).and_then(|s| s.checked_sub(moved));
    let (Some(grown), Some(shrunk)) = (grown, shrunk) else {
        return false;
    };
    for (i, ((weight, _), size)) in children.iter_mut().zip(&sizes).enumerate() {
        let size = if i == grow {
            grown
        } else if i == shrink {
            shrunk
        } else {
            *size
        };
        *weight = u32::from(size).max(1);
    }
    true
}

/// The rectangle each child of a split gets, from a placement of the split.
fn child_rects(
    axis: Axis,
    children: &[(u32, Node)],
    area: Rect,
    placement: &Placement,
) -> Vec<Rect> {
    children
        .iter()
        .map(|(_, child)| {
            // The span of the child's placed panes.
            let mut span: Option<(u16, u16, u32, u32)> = None;
            child.for_each_pane(&mut |p| {
                if let Some(r) = placement.rect(p) {
                    span = Some(match span {
                        None => (r.x, r.y, r.right(), r.bottom()),
                        Some((x0, y0, x1, y1)) => (
                            x0.min(r.x),
                            y0.min(r.y),
                            x1.max(r.right()),
                            y1.max(r.bottom()),
                        ),
                    });
                }
            });
            let Some((x0, y0, x1, y1)) = span else {
                return Rect { w: 0, h: 0, ..area };
            };
            // The children lie inside `area`, so their span fits its size.
            match axis {
                Axis::Horizontal => Rect {
                    x: x0,
                    w: x1
                        .checked_sub(u32::from(x0))
                        .and_then(|w| u16::try_from(w).ok())
                        .unwrap_or(area.w),
                    ..area
                },
                Axis::Vertical => Rect {
                    y: y0,
                    h: y1
                        .checked_sub(u32::from(y0))
                        .and_then(|h| u16::try_from(h).ok())
                        .unwrap_or(area.h),
                    ..area
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u32) -> PaneId {
        PaneId(n)
    }
    fn area(w: u16, h: u16) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }
    /// `0 | 1`.
    fn tree() -> Option<Node> {
        let mut root = Some(Node::Pane(p(0)));
        split(&mut root, p(0), p(1), Axis::Horizontal, true);
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
                x: 40,
                y: 0,
                len: 24
            }]
        );
        // A second split of the right pane shares its weight: 1000:500:500.
        let mut root = tree();
        assert!(split(&mut root, p(1), p(2), Axis::Horizontal, true));
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
        split(&mut root, p(1), p(2), Axis::Horizontal, true);
        split(&mut root, p(2), p(3), Axis::Vertical, true);
        split(&mut root, p(1), p(4), Axis::Vertical, false);
        let Some(node) = &root else { return };
        assert_eq!(node.panes(), vec![p(0), p(4), p(1), p(2), p(3)]);
        let placed = place(node, area(81, 25));
        assert_eq!(placed.panes.len(), 5);
        let mut covered = vec![0u8; 81 * 25];
        for (_, r) in &placed.panes {
            for y in r.y..r.y + r.h {
                for x in r.x..r.x + r.w {
                    if let Some(c) = covered.get_mut(usize::from(y) * 81 + usize::from(x)) {
                        *c += 1;
                    }
                }
            }
        }
        for r in placed.separators.iter().map(Separator::rect) {
            for y in r.y..r.y + r.h {
                for x in r.x..r.x + r.w {
                    if let Some(c) = covered.get_mut(usize::from(y) * 81 + usize::from(x)) {
                        *c += 1;
                    }
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
        split(&mut root, p(0), p(1), Axis::Horizontal, true);
        split(&mut root, p(1), p(2), Axis::Vertical, true);
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
            split(&mut root, p(n - 1), p(n), Axis::Horizontal, true);
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
        split(&mut root, p(1), p(2), Axis::Vertical, true);
        split(&mut root, p(2), p(3), Axis::Horizontal, true);
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
        assert!(split(&mut empty, p(9), p(5), Axis::Vertical, true));
        assert_eq!(empty, Some(Node::Pane(p(5))));
    }

    #[test]
    fn directional_neighbours_prefer_alignment_then_distance_then_id() {
        // 0 | (1 / 2)
        let mut root = tree();
        split(&mut root, p(1), p(2), Axis::Vertical, true);
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

    #[test]
    fn swapping_exchanges_places() {
        let mut root = tree();
        let Some(node) = &mut root else { return };
        swap(node, p(0), p(1));
        assert_eq!(node.panes(), vec![p(1), p(0)]);
    }
}

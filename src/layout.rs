//! A tab's layout: a tree of splits whose leaves are panes, and the
//! rectangles it gives each pane at a given size.
use crate::keys::Direction;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x
            && y >= self.y
            && u32::from(x) < u32::from(self.x) + u32::from(self.w)
            && u32::from(y) < u32::from(self.y) + u32::from(self.h)
    }
    fn right(&self) -> u32 {
        u32::from(self.x) + u32::from(self.w)
    }
    fn bottom(&self) -> u32 {
        u32::from(self.y) + u32::from(self.h)
    }
}

/// A separator line: vertical between side-by-side children, horizontal
/// between stacked ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Separator {
    pub vertical: bool,
    pub x: u16,
    pub y: u16,
    pub len: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    pub panes: Vec<(PaneId, Rect)>,
    pub separators: Vec<Separator>,
}

impl Placement {
    pub fn rect(&self, pane: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r)
    }
}

impl Node {
    /// Every pane, in tree order.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }
    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Pane(p) => out.push(*p),
            Node::Split { children, .. } => {
                for (_, child) in children {
                    child.collect(out);
                }
            }
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
                    let separators = children.len().saturating_sub(1) as u16;
                    mins.fold(separators, u16::saturating_add)
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
                let at = if after { index + 1 } else { index };
                children.insert(at, (half, Node::Pane(new)));
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
    if let Some(index) = children
        .iter()
        .position(|(_, c)| matches!(c, Node::Pane(p) if *p == pane))
    {
        children.remove(index);
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
                let total: u64 = grand.iter().map(|(w, _)| u64::from(*w)).sum::<u64>().max(1);
                for (w, g) in grand {
                    let scaled = (u64::from(weight) * u64::from(w) / total).max(1);
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

/// Shares `len` cells among children with `weights`, giving each at least
/// its minimum while there is room; children that do not fit get zero.
fn distribute(len: u16, weights: &[u32], mins: &[u16]) -> Vec<u16> {
    let n = weights.len();
    let needed: u32 = mins.iter().map(|m| u32::from(*m)).sum();
    if u32::from(len) < needed {
        // Not enough room: minimums in order while they fit; the last child
        // shown takes what is left.
        let mut sizes = vec![0u16; n];
        let mut left = len;
        let mut last = None;
        for (i, min) in mins.iter().enumerate() {
            if *min <= left {
                if let Some(size) = sizes.get_mut(i) {
                    *size = *min;
                }
                left -= *min;
                last = Some(i);
            } else {
                break;
            }
        }
        if let Some(size) = last.and_then(|i| sizes.get_mut(i)) {
            *size += left;
        }
        return sizes;
    }
    let mut fixed = vec![false; n];
    let mut sizes = vec![0u16; n];
    loop {
        let free_len: u32 = u32::from(len)
            - sizes
                .iter()
                .zip(&fixed)
                .filter(|(_, f)| **f)
                .map(|(s, _)| u32::from(*s))
                .sum::<u32>();
        let free_weight: u64 = weights
            .iter()
            .zip(&fixed)
            .filter(|(_, f)| !**f)
            .map(|(w, _)| u64::from((*w).max(1)))
            .sum::<u64>()
            .max(1);
        let mut changed = false;
        let mut shares = Vec::with_capacity(n);
        for i in 0..n {
            if fixed.get(i).copied().unwrap_or(true) {
                shares.push((i, 0u64, 0u64));
                continue;
            }
            let w = u64::from(weights.get(i).copied().unwrap_or(1).max(1));
            let exact = u64::from(free_len) * w;
            let share = exact / free_weight;
            let min = u64::from(mins.get(i).copied().unwrap_or(MIN));
            if share < min {
                if let (Some(f), Some(s)) = (fixed.get_mut(i), sizes.get_mut(i)) {
                    *f = true;
                    *s = min as u16;
                }
                changed = true;
            }
            shares.push((i, share, exact % free_weight));
        }
        if changed {
            continue;
        }
        let mut used: u32 = 0;
        for (i, share, _) in &shares {
            if !fixed.get(*i).copied().unwrap_or(true) {
                if let Some(s) = sizes.get_mut(*i) {
                    *s = *share as u16;
                }
                used += *share as u32;
            }
        }
        // Hand out the rounding remainder, largest fraction first.
        let mut order: Vec<_> = shares
            .into_iter()
            .filter(|(i, _, _)| !fixed.get(*i).copied().unwrap_or(true))
            .collect();
        order.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        let mut left = free_len.saturating_sub(used);
        for (i, _, _) in order.iter().cycle().take(order.len() * 2) {
            if left == 0 {
                break;
            }
            if let Some(s) = sizes.get_mut(*i) {
                *s += 1;
                left -= 1;
            }
        }
        return sizes;
    }
}

/// The rectangles of every pane that fits in `area`, and the separators.
pub fn place(root: &Node, area: Rect) -> Placement {
    let mut out = Placement::default();
    if area.w > 0 && area.h > 0 {
        place_node(root, area, &mut out);
    }
    out
}

fn place_node(node: &Node, area: Rect, out: &mut Placement) {
    match node {
        Node::Pane(p) => out.panes.push((*p, area)),
        Node::Split { axis, children } => {
            let along = match axis {
                Axis::Horizontal => area.w,
                Axis::Vertical => area.h,
            };
            let separators = children.len().saturating_sub(1) as u16;
            let weights: Vec<u32> = children.iter().map(|(w, _)| *w).collect();
            let mins: Vec<u16> = children.iter().map(|(_, c)| c.min_len(*axis)).collect();
            let needed = mins.iter().fold(separators, |a, m| a.saturating_add(*m));
            let sizes = if along >= needed {
                distribute(along - separators, &weights, &mins)
            } else {
                // Too small for all: children in order while they fit, each
                // after the first needing a separator; the last one shown
                // takes what is left.
                let mut sizes = vec![0u16; children.len()];
                let mut left = along;
                let mut last = None;
                for (i, min) in mins.iter().enumerate() {
                    let cost = min.saturating_add(u16::from(i > 0));
                    if cost > left {
                        break;
                    }
                    left -= cost;
                    if let Some(size) = sizes.get_mut(i) {
                        *size = *min;
                    }
                    last = Some(i);
                }
                if let Some(size) = last.and_then(|i| sizes.get_mut(i)) {
                    *size += left;
                }
                sizes
            };
            let mut offset = 0u16;
            let mut first = true;
            for ((_, child), size) in children.iter().zip(sizes) {
                if size == 0 {
                    continue;
                }
                if !first {
                    let separator = match axis {
                        Axis::Horizontal => Separator {
                            vertical: true,
                            x: area.x + offset,
                            y: area.y,
                            len: area.h,
                        },
                        Axis::Vertical => Separator {
                            vertical: false,
                            x: area.x,
                            y: area.y + offset,
                            len: area.w,
                        },
                    };
                    out.separators.push(separator);
                    offset += 1;
                }
                first = false;
                let rect = match axis {
                    Axis::Horizontal => Rect {
                        x: area.x + offset,
                        w: size,
                        ..area
                    },
                    Axis::Vertical => Rect {
                        y: area.y + offset,
                        h: size,
                        ..area
                    },
                };
                place_node(child, rect, out);
                offset += size;
            }
        }
    }
}

/// The pane in `direction` from `from`, among the placed panes: rectangles
/// beyond `from`'s edge, ranked by the distance between centres across the
/// direction, then by the distance along it, then by ID.
pub fn neighbor(placement: &Placement, from: PaneId, direction: Direction) -> Option<PaneId> {
    let source = placement.rect(from)?;
    let centre = |r: &Rect| {
        (
            u32::from(r.x) * 2 + u32::from(r.w),
            u32::from(r.y) * 2 + u32::from(r.h),
        )
    };
    let (sx, sy) = centre(&source);
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
            let (cx, cy) = centre(r);
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
    root: &mut Node,
    area: Rect,
    pane: PaneId,
    direction: Direction,
    amount: u16,
) -> bool {
    resize_node(root, area, pane, direction, amount)
}

fn resize_node(
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
    let placement = place(
        &Node::Split {
            axis: *axis,
            children: children.clone(),
        },
        area,
    );
    let child_area = child_rects(*axis, children, area, &placement);
    if let (Some((_, child)), Some(inner)) = (children.get_mut(index), child_area.get(index))
        && resize_node(child, *inner, pane, direction, amount)
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
    let (grow, shrink) = if toward_start {
        if index > 0 {
            (index, index - 1)
        } else if index + 1 < children.len() {
            (index + 1, index)
        } else {
            return false;
        }
    } else if index + 1 < children.len() {
        (index, index + 1)
    } else if index > 0 {
        (index - 1, index)
    } else {
        return false;
    };
    let min = children.get(shrink).map_or(MIN, |(_, c)| c.min_len(*axis));
    let available = sizes.get(shrink).copied().unwrap_or(0).saturating_sub(min);
    let moved = amount.min(available);
    if moved == 0 {
        return true;
    }
    for (i, ((weight, _), size)) in children.iter_mut().zip(&sizes).enumerate() {
        let size = if i == grow {
            size + moved
        } else if i == shrink {
            size - moved
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
            let rects: Vec<Rect> = child
                .panes()
                .iter()
                .filter_map(|p| placement.rect(*p))
                .collect();
            let Some(first) = rects.first() else {
                return Rect { w: 0, h: 0, ..area };
            };
            let (mut x0, mut y0, mut x1, mut y1) = (
                u32::from(first.x),
                u32::from(first.y),
                first.right(),
                first.bottom(),
            );
            for r in &rects {
                x0 = x0.min(u32::from(r.x));
                y0 = y0.min(u32::from(r.y));
                x1 = x1.max(r.right());
                y1 = y1.max(r.bottom());
            }
            match axis {
                Axis::Horizontal => Rect {
                    x: x0 as u16,
                    w: (x1 - x0) as u16,
                    ..area
                },
                Axis::Vertical => Rect {
                    y: y0 as u16,
                    h: (y1 - y0) as u16,
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
                vertical: true,
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
        for s in &placed.separators {
            for i in 0..s.len {
                let (x, y) = if s.vertical {
                    (s.x, s.y + i)
                } else {
                    (s.x + i, s.y)
                };
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

    #[test]
    fn a_small_area_keeps_minimums_and_hides_what_does_not_fit() {
        let mut root = tree();
        for n in 2..10 {
            split(&mut root, p(n - 1), p(n), Axis::Horizontal, true);
        }
        let Some(node) = &root else { return };
        let placed = place(node, area(10, 3));
        assert!(placed.panes.iter().all(|(_, r)| r.w >= MIN && r.h >= MIN));
        let used: u16 =
            placed.panes.iter().map(|(_, r)| r.w).sum::<u16>() + placed.separators.len() as u16;
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

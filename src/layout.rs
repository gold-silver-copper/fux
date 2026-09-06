//! Compact typed split tree for one tab. Leaves are pane handles (`Entity` in the server,
//! any `Copy + Eq + Hash` value in tests). The tree validates itself after every edit:
//! acyclic, unique leaves, bounded ratios, no dangling nodes.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::hash::Hash;
use std::num::NonZeroU16;

pub const RATIO_SCALE: u16 = 10_000;
pub const MIN_RATIO: u16 = 500;
pub const MAX_RATIO: u16 = RATIO_SCALE - MIN_RATIO;
/// Absolute ceiling on leaves per tree; the configured pane limit is usually lower.
pub const MAX_LEAVES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Axis {
    /// Children sit side by side.
    Horizontal,
    /// Children are stacked.
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    #[must_use]
    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x
            && x < self.x.saturating_add(self.width)
            && y >= self.y
            && y < self.y.saturating_add(self.height)
    }
    /// The area inside a one-cell border.
    #[must_use]
    pub fn inner(self) -> Self {
        Self {
            x: self.x.saturating_add(1),
            y: self.y.saturating_add(1),
            width: self.width.saturating_sub(2),
            height: self.height.saturating_sub(2),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Node<L> {
    Leaf(L),
    Split {
        axis: Axis,
        ratio: NonZeroU16,
        first: NodeId,
        second: NodeId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutTree<L> {
    nodes: Vec<Option<Node<L>>>,
    root: Option<NodeId>,
    free: Vec<NodeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutError {
    Empty,
    MissingNode,
    MissingPane,
    DuplicatePane,
    Cycle,
    InvalidRatio,
    Limit,
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for LayoutError {}

impl<L: Copy + Eq + Hash> LayoutTree<L> {
    #[must_use]
    pub fn new(pane: L) -> Self {
        Self {
            nodes: vec![Some(Node::Leaf(pane))],
            root: Some(NodeId(0)),
            free: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    #[must_use]
    pub fn contains(&self, pane: L) -> bool {
        self.find_leaf(pane).is_some()
    }

    /// Leaves in depth-first order (first child before second).
    #[must_use]
    pub fn leaves(&self) -> Vec<L> {
        let mut out = Vec::new();
        if let Some(root) = self.root {
            self.walk_leaves(root, &mut out, &mut HashSet::new());
        }
        out
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.leaves().len()
    }

    pub fn split(
        &mut self,
        target: L,
        new_pane: L,
        axis: Axis,
        ratio: NonZeroU16,
    ) -> Result<(), LayoutError> {
        self.validate()?;
        if self.len() >= MAX_LEAVES {
            return Err(LayoutError::Limit);
        }
        if self.contains(new_pane) {
            return Err(LayoutError::DuplicatePane);
        }
        Self::check_ratio(ratio)?;
        let target_id = self.find_leaf(target).ok_or(LayoutError::MissingPane)?;
        let old = self
            .node(target_id)
            .copied()
            .ok_or(LayoutError::MissingNode)?;
        let first = self.insert(old)?;
        let second = self.insert(Node::Leaf(new_pane))?;
        self.set(
            target_id,
            Node::Split {
                axis,
                ratio,
                first,
                second,
            },
        )?;
        self.validate()
    }

    /// Removes a leaf and returns the pane that should receive focus afterwards (the directional
    /// neighbour across the removed split, else the first leaf of the surviving sibling).
    pub fn close(&mut self, pane: L) -> Result<Option<L>, LayoutError> {
        self.validate()?;
        let leaf = self.find_leaf(pane).ok_or(LayoutError::MissingPane)?;
        if self.root == Some(leaf) {
            self.remove(leaf);
            self.root = None;
            self.validate()?;
            return Ok(None);
        }
        let (parent, sibling) = self
            .parent_and_sibling(leaf)
            .ok_or(LayoutError::MissingNode)?;
        let direction = match self.node(parent) {
            Some(Node::Split {
                axis: Axis::Horizontal,
                first,
                ..
            }) if *first == leaf => Direction::Right,
            Some(Node::Split {
                axis: Axis::Horizontal,
                ..
            }) => Direction::Left,
            Some(Node::Split {
                axis: Axis::Vertical,
                first,
                ..
            }) if *first == leaf => Direction::Down,
            Some(Node::Split {
                axis: Axis::Vertical,
                ..
            }) => Direction::Up,
            _ => return Err(LayoutError::MissingNode),
        };
        let directional = self.neighbour(
            pane,
            direction,
            Rect {
                x: 0,
                y: 0,
                width: RATIO_SCALE,
                height: RATIO_SCALE,
            },
        );
        let replacement = self
            .node(sibling)
            .copied()
            .ok_or(LayoutError::MissingNode)?;
        let next = self.first_leaf(sibling);
        self.set(parent, replacement)?;
        self.remove(leaf);
        self.remove(sibling);
        self.validate()?;
        Ok(directional.or(next))
    }

    /// Moves the split boundary enclosing `pane` by `delta` ratio units (positive grows the pane).
    pub fn resize(&mut self, pane: L, delta: i16) -> Result<(), LayoutError> {
        self.validate()?;
        let leaf = self.find_leaf(pane).ok_or(LayoutError::MissingPane)?;
        let (parent, is_first) = self.parent_of(leaf).ok_or(LayoutError::MissingNode)?;
        let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = self.node(parent).copied().ok_or(LayoutError::MissingNode)?
        else {
            return Err(LayoutError::MissingNode);
        };
        let signed = if is_first {
            i32::from(ratio.get()) + i32::from(delta)
        } else {
            i32::from(ratio.get()) - i32::from(delta)
        };
        let bounded = u16::try_from(signed.clamp(i32::from(MIN_RATIO), i32::from(MAX_RATIO)))
            .map_err(|_| LayoutError::InvalidRatio)?;
        let ratio = NonZeroU16::new(bounded).ok_or(LayoutError::InvalidRatio)?;
        self.set(
            parent,
            Node::Split {
                axis,
                ratio,
                first,
                second,
            },
        )?;
        self.validate()
    }

    /// Outer rectangles (including borders) for every leaf within `area`.
    pub fn geometry(&self, area: Rect) -> Result<Vec<(L, Rect)>, LayoutError> {
        self.validate()?;
        let mut output = Vec::new();
        if let Some(root) = self.root {
            self.geometry_node(root, area, &mut output)?;
        }
        Ok(output)
    }

    /// The nearest leaf in `direction`, preferring the largest overlap then the shortest gap.
    #[must_use]
    pub fn neighbour(&self, pane: L, direction: Direction, area: Rect) -> Option<L> {
        let geometry = self.geometry(area).ok()?;
        let (_, source) = geometry.iter().find(|(id, _)| *id == pane)?;
        geometry
            .iter()
            .enumerate()
            .filter(|(_, (id, _))| *id != pane)
            .filter_map(|(order, (id, rect))| {
                let eligible = match direction {
                    Direction::Left => rect.x.saturating_add(rect.width) <= source.x,
                    Direction::Right => rect.x >= source.x.saturating_add(source.width),
                    Direction::Up => rect.y.saturating_add(rect.height) <= source.y,
                    Direction::Down => rect.y >= source.y.saturating_add(source.height),
                };
                if !eligible {
                    return None;
                }
                let (overlap, distance) = match direction {
                    Direction::Left => (
                        overlap(source.y, source.height, rect.y, rect.height),
                        source.x.saturating_sub(rect.x.saturating_add(rect.width)),
                    ),
                    Direction::Right => (
                        overlap(source.y, source.height, rect.y, rect.height),
                        rect.x.saturating_sub(source.x.saturating_add(source.width)),
                    ),
                    Direction::Up => (
                        overlap(source.x, source.width, rect.x, rect.width),
                        source.y.saturating_sub(rect.y.saturating_add(rect.height)),
                    ),
                    Direction::Down => (
                        overlap(source.x, source.width, rect.x, rect.width),
                        rect.y
                            .saturating_sub(source.y.saturating_add(source.height)),
                    ),
                };
                if overlap == 0 {
                    return None;
                }
                Some(((std::cmp::Reverse(overlap), distance, order), *id))
            })
            .min_by_key(|(key, _)| *key)
            .map(|(_, id)| id)
    }

    pub fn validate(&self) -> Result<(), LayoutError> {
        if self.nodes.len() > MAX_LEAVES.saturating_mul(2).saturating_sub(1)
            || self.free.len() > self.nodes.len()
        {
            return Err(LayoutError::Limit);
        }
        let free: HashSet<_> = self.free.iter().copied().collect();
        if free.len() != self.free.len()
            || self.nodes.iter().enumerate().any(|(index, node)| {
                let Ok(index) = u32::try_from(index) else {
                    return true;
                };
                node.is_none() != free.contains(&NodeId(index))
            })
        {
            return Err(LayoutError::MissingNode);
        }
        let Some(root) = self.root else {
            return if self.nodes.iter().all(Option::is_none) {
                Ok(())
            } else {
                Err(LayoutError::MissingNode)
            };
        };
        let mut seen_nodes = HashSet::new();
        let mut panes = HashSet::new();
        self.validate_node(root, &mut seen_nodes, &mut panes)?;
        if seen_nodes.len() != self.nodes.iter().filter(|node| node.is_some()).count() {
            return Err(LayoutError::MissingNode);
        }
        Ok(())
    }

    fn node(&self, id: NodeId) -> Option<&Node<L>> {
        self.nodes.get(id.0 as usize)?.as_ref()
    }

    fn validate_node(
        &self,
        id: NodeId,
        seen: &mut HashSet<NodeId>,
        panes: &mut HashSet<L>,
    ) -> Result<(), LayoutError> {
        if !seen.insert(id) {
            return Err(LayoutError::Cycle);
        }
        match self.node(id).ok_or(LayoutError::MissingNode)? {
            Node::Leaf(pane) => {
                if panes.insert(*pane) {
                    Ok(())
                } else {
                    Err(LayoutError::DuplicatePane)
                }
            }
            Node::Split {
                ratio,
                first,
                second,
                ..
            } => {
                Self::check_ratio(*ratio)?;
                if first == second {
                    return Err(LayoutError::Cycle);
                }
                self.validate_node(*first, seen, panes)?;
                self.validate_node(*second, seen, panes)
            }
        }
    }

    fn geometry_node(
        &self,
        id: NodeId,
        rect: Rect,
        out: &mut Vec<(L, Rect)>,
    ) -> Result<(), LayoutError> {
        match self.node(id).ok_or(LayoutError::MissingNode)? {
            Node::Leaf(pane) => out.push((*pane, rect)),
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let extent = match axis {
                    Axis::Horizontal => rect.width,
                    Axis::Vertical => rect.height,
                };
                let first_extent = u16::try_from(
                    u32::from(extent) * u32::from(ratio.get()) / u32::from(RATIO_SCALE),
                )
                .unwrap_or(extent);
                let second_extent = extent.saturating_sub(first_extent);
                let (a, b) = match axis {
                    Axis::Horizontal => (
                        Rect {
                            width: first_extent,
                            ..rect
                        },
                        Rect {
                            x: rect.x.saturating_add(first_extent),
                            width: second_extent,
                            ..rect
                        },
                    ),
                    Axis::Vertical => (
                        Rect {
                            height: first_extent,
                            ..rect
                        },
                        Rect {
                            y: rect.y.saturating_add(first_extent),
                            height: second_extent,
                            ..rect
                        },
                    ),
                };
                self.geometry_node(*first, a, out)?;
                self.geometry_node(*second, b, out)?;
            }
        }
        Ok(())
    }

    fn find_leaf(&self, pane: L) -> Option<NodeId> {
        self.nodes.iter().enumerate().find_map(|(index, node)| {
            matches!(node, Some(Node::Leaf(id)) if *id == pane)
                .then(|| u32::try_from(index).ok().map(NodeId))
                .flatten()
        })
    }
    fn first_leaf(&self, id: NodeId) -> Option<L> {
        match self.node(id)? {
            Node::Leaf(pane) => Some(*pane),
            Node::Split { first, .. } => self.first_leaf(*first),
        }
    }
    fn parent_of(&self, child: NodeId) -> Option<(NodeId, bool)> {
        self.nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| match node {
                Some(Node::Split { first, .. }) if *first == child => {
                    u32::try_from(index).ok().map(|index| (NodeId(index), true))
                }
                Some(Node::Split { second, .. }) if *second == child => u32::try_from(index)
                    .ok()
                    .map(|index| (NodeId(index), false)),
                _ => None,
            })
    }
    fn parent_and_sibling(&self, child: NodeId) -> Option<(NodeId, NodeId)> {
        self.nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| match node {
                Some(Node::Split { first, second, .. }) if *first == child => u32::try_from(index)
                    .ok()
                    .map(|index| (NodeId(index), *second)),
                Some(Node::Split { first, second, .. }) if *second == child => u32::try_from(index)
                    .ok()
                    .map(|index| (NodeId(index), *first)),
                _ => None,
            })
    }
    fn walk_leaves(&self, id: NodeId, out: &mut Vec<L>, seen: &mut HashSet<NodeId>) {
        if !seen.insert(id) {
            return;
        }
        match self.node(id) {
            Some(Node::Leaf(pane)) => out.push(*pane),
            Some(Node::Split { first, second, .. }) => {
                self.walk_leaves(*first, out, seen);
                self.walk_leaves(*second, out, seen);
            }
            None => {}
        }
    }
    fn insert(&mut self, node: Node<L>) -> Result<NodeId, LayoutError> {
        if let Some(id) = self.free.pop() {
            self.set(id, node)?;
            return Ok(id);
        }
        let id = NodeId(u32::try_from(self.nodes.len()).map_err(|_| LayoutError::Limit)?);
        self.nodes.push(Some(node));
        Ok(id)
    }
    fn set(&mut self, id: NodeId, node: Node<L>) -> Result<(), LayoutError> {
        let slot = self
            .nodes
            .get_mut(id.0 as usize)
            .ok_or(LayoutError::MissingNode)?;
        *slot = Some(node);
        Ok(())
    }
    fn remove(&mut self, id: NodeId) {
        if let Some(slot) = self.nodes.get_mut(id.0 as usize)
            && slot.take().is_some()
        {
            self.free.push(id);
        }
    }
    fn check_ratio(ratio: NonZeroU16) -> Result<(), LayoutError> {
        if (MIN_RATIO..=MAX_RATIO).contains(&ratio.get()) {
            Ok(())
        } else {
            Err(LayoutError::InvalidRatio)
        }
    }
}

fn overlap(a_start: u16, a_len: u16, b_start: u16, b_len: u16) -> u16 {
    a_start
        .saturating_add(a_len)
        .min(b_start.saturating_add(b_len))
        .saturating_sub(a_start.max(b_start))
}

/// The even split every new pane starts with.
#[must_use]
pub fn half() -> NonZeroU16 {
    NonZeroU16::new(RATIO_SCALE / 2).unwrap_or(NonZeroU16::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        }
    }

    #[test]
    fn split_geometry_and_direction_are_stable() {
        let mut tree = LayoutTree::new(1_u32);
        tree.split(1, 2, Axis::Horizontal, half())
            .unwrap_or_default();
        let geometry = tree
            .geometry(Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            })
            .unwrap_or_default();
        assert_eq!(
            geometry,
            vec![
                (
                    1,
                    Rect {
                        x: 0,
                        y: 0,
                        width: 40,
                        height: 24
                    }
                ),
                (
                    2,
                    Rect {
                        x: 40,
                        y: 0,
                        width: 40,
                        height: 24
                    }
                )
            ]
        );
        assert_eq!(tree.neighbour(1, Direction::Right, area()), Some(2));
        assert_eq!(tree.neighbour(2, Direction::Left, area()), Some(1));
        assert_eq!(tree.neighbour(2, Direction::Up, area()), None);
        assert_eq!(tree.close(1), Ok(Some(2)));
        assert_eq!(tree.leaves(), vec![2]);
        assert_eq!(tree.close(2), Ok(None));
        assert!(tree.is_empty());
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn nested_geometry_matches_reference_cases() {
        let mut tree = LayoutTree::new(1_u32);
        let ratio = |value| NonZeroU16::new(value).unwrap_or(NonZeroU16::MIN);
        assert!(tree.split(1, 2, Axis::Horizontal, ratio(3_000)).is_ok());
        assert!(tree.split(2, 3, Axis::Vertical, ratio(6_000)).is_ok());
        assert!(tree.split(3, 4, Axis::Horizontal, ratio(4_000)).is_ok());
        let geometry = tree.geometry(area()).unwrap_or_default();
        let rect = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        assert_eq!(
            geometry,
            vec![
                (1, rect(0, 0, 30, 40)),
                (2, rect(30, 0, 70, 24)),
                (3, rect(30, 24, 28, 16)),
                (4, rect(58, 24, 42, 16)),
            ]
        );
        assert_eq!(tree.neighbour(3, Direction::Right, area()), Some(4));
        assert_eq!(tree.neighbour(4, Direction::Up, area()), Some(2));
        assert_eq!(tree.neighbour(1, Direction::Right, area()), Some(2));
    }

    #[test]
    fn close_focus_prefers_larger_directional_overlap() {
        let mut tree = LayoutTree::new(1_u32);
        let small = NonZeroU16::new(2_000).unwrap_or(NonZeroU16::MIN);
        assert!(tree.split(1, 2, Axis::Horizontal, half()).is_ok());
        assert!(tree.split(2, 3, Axis::Vertical, small).is_ok());
        assert_eq!(tree.close(1), Ok(Some(3)));
    }

    #[test]
    fn resize_is_clamped_and_rejects_root() {
        let mut tree = LayoutTree::new(1_u32);
        assert_eq!(tree.resize(1, 100), Err(LayoutError::MissingNode));
        assert!(tree.split(1, 2, Axis::Vertical, half()).is_ok());
        for _ in 0..100 {
            assert!(tree.resize(1, 1_000).is_ok());
        }
        let geometry = tree.geometry(area()).unwrap_or_default();
        assert_eq!(geometry.first().map(|(_, rect)| rect.height), Some(38));
        assert!(tree.resize(2, 30_000).is_ok());
        assert_eq!(
            tree.geometry(area())
                .unwrap_or_default()
                .first()
                .map(|(_, rect)| rect.height),
            Some(2)
        );
    }

    #[test]
    fn duplicate_and_missing_panes_are_rejected() {
        let mut tree = LayoutTree::new(1_u32);
        assert_eq!(
            tree.split(1, 1, Axis::Horizontal, half()),
            Err(LayoutError::DuplicatePane)
        );
        assert_eq!(
            tree.split(9, 2, Axis::Horizontal, half()),
            Err(LayoutError::MissingPane)
        );
        assert_eq!(tree.close(9), Err(LayoutError::MissingPane));
        assert_eq!(
            tree.split(1, 2, Axis::Horizontal, NonZeroU16::MIN),
            Err(LayoutError::InvalidRatio)
        );
    }

    proptest! {
        #[test]
        fn random_edits_preserve_invariants(operations in proptest::collection::vec((0_u8..4, any::<u8>()), 0..120)) {
            let mut tree = LayoutTree::new(0_u32);
            let mut next = 1_u32;
            let mut expected: Vec<u32> = vec![0];
            for (operation, pick) in operations {
                let leaves = tree.leaves();
                prop_assert_eq!(&leaves.iter().copied().collect::<std::collections::BTreeSet<_>>(), &expected.iter().copied().collect::<std::collections::BTreeSet<_>>());
                let Some(target) = leaves.get(usize::from(pick) % leaves.len()).copied() else { break };
                match operation {
                    0 | 1 if leaves.len() < 24 => {
                        let axis = if operation == 0 { Axis::Horizontal } else { Axis::Vertical };
                        prop_assert!(tree.split(target, next, axis, half()).is_ok());
                        expected.push(next);
                        next += 1;
                    }
                    2 if leaves.len() > 1 => {
                        let focus = tree.close(target);
                        prop_assert!(focus.is_ok());
                        expected.retain(|leaf| *leaf != target);
                        if let Ok(Some(focus)) = focus { prop_assert!(expected.contains(&focus)); }
                    }
                    _ => { let _ = tree.resize(target, i16::from(pick) - 100); }
                }
                prop_assert!(tree.validate().is_ok());
                let geometry = tree.geometry(area()).unwrap_or_default();
                prop_assert_eq!(geometry.len(), expected.len());
                let total: u32 = geometry.iter().map(|(_, rect)| u32::from(rect.width) * u32::from(rect.height)).sum();
                prop_assert_eq!(total, 100 * 40);
            }
        }
    }
}

use super::{MAX_NAME_BYTES, MAX_PANES, PaneId, TabId};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::num::NonZeroU16;

pub const RATIO_SCALE: u16 = 10_000;
pub const MIN_RATIO: u16 = 500;
pub const MAX_RATIO: u16 = RATIO_SCALE - MIN_RATIO;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        ratio: NonZeroU16,
        first: NodeId,
        second: NodeId,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LayoutTree {
    nodes: Vec<Option<Node>>,
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

impl LayoutTree {
    pub(crate) fn allocation_units(&self) -> usize {
        self.nodes
            .capacity()
            .saturating_mul(std::mem::size_of::<Option<Node>>())
            .saturating_add(
                self.free
                    .capacity()
                    .saturating_mul(std::mem::size_of::<NodeId>()),
            )
    }
    #[cfg(test)]
    pub(super) fn from_raw(
        nodes: Vec<Option<Node>>,
        root: Option<NodeId>,
        free: Vec<NodeId>,
    ) -> Self {
        Self { nodes, root, free }
    }

    #[must_use]
    pub fn new(pane: PaneId) -> Self {
        Self {
            nodes: vec![Some(Node::Leaf(pane))],
            root: Some(NodeId(0)),
            free: Vec::new(),
        }
    }
    #[must_use]
    pub const fn root(&self) -> Option<NodeId> {
        self.root
    }
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.0 as usize)?.as_ref()
    }
    #[must_use]
    pub fn contains(&self, pane: PaneId) -> bool {
        self.leaves().contains(&pane)
    }
    #[must_use]
    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        if let Some(root) = self.root {
            self.walk_leaves(root, &mut out, &mut HashSet::new());
        }
        out
    }

    pub fn split(
        &mut self,
        target: PaneId,
        new_pane: PaneId,
        axis: Axis,
        ratio: NonZeroU16,
    ) -> Result<(), LayoutError> {
        self.validate()?;
        if self.leaves().len() >= MAX_PANES {
            return Err(LayoutError::Limit);
        }
        if self.contains(new_pane) {
            return Err(LayoutError::DuplicatePane);
        }
        Self::check_ratio(ratio)?;
        let target_id = self.find_leaf(target).ok_or(LayoutError::MissingPane)?;
        let old = self
            .node(target_id)
            .cloned()
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

    pub fn close(&mut self, pane: PaneId) -> Result<Option<PaneId>, LayoutError> {
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
            .cloned()
            .ok_or(LayoutError::MissingNode)?;
        let next = self.first_leaf(sibling);
        self.set(parent, replacement)?;
        self.remove(leaf);
        self.remove(sibling);
        self.validate()?;
        Ok(directional.or(next))
    }

    pub fn swap(&mut self, first: PaneId, second: PaneId) -> Result<(), LayoutError> {
        self.validate()?;
        let first_id = self.find_leaf(first).ok_or(LayoutError::MissingPane)?;
        let second_id = self.find_leaf(second).ok_or(LayoutError::MissingPane)?;
        self.set(first_id, Node::Leaf(second))?;
        self.set(second_id, Node::Leaf(first))?;
        self.validate()
    }

    pub fn resize(&mut self, pane: PaneId, delta: i16) -> Result<(), LayoutError> {
        self.validate()?;
        let leaf = self.find_leaf(pane).ok_or(LayoutError::MissingPane)?;
        let (parent, is_first) = self.parent_of(leaf).ok_or(LayoutError::MissingNode)?;
        let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = self.node(parent).cloned().ok_or(LayoutError::MissingNode)?
        else {
            return Err(LayoutError::MissingNode);
        };
        let signed = if is_first {
            i32::from(ratio.get()) + i32::from(delta)
        } else {
            i32::from(ratio.get()) - i32::from(delta)
        };
        let bounded = signed.clamp(i32::from(MIN_RATIO), i32::from(MAX_RATIO)) as u16;
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

    pub fn geometry(&self, area: Rect) -> Result<Vec<(PaneId, Rect)>, LayoutError> {
        self.validate()?;
        let mut output = Vec::new();
        if let Some(root) = self.root {
            self.geometry_node(root, area, &mut output)?;
        }
        Ok(output)
    }

    pub fn neighbour(&self, pane: PaneId, direction: Direction, area: Rect) -> Option<PaneId> {
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
                Some((std::cmp::Reverse(overlap), distance, order, *id))
            })
            .min()
            .map(|value| value.3)
    }

    pub fn validate(&self) -> Result<(), LayoutError> {
        if self.nodes.len() > MAX_PANES.saturating_mul(2).saturating_sub(1)
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

    fn validate_node(
        &self,
        id: NodeId,
        seen: &mut HashSet<NodeId>,
        panes: &mut HashSet<PaneId>,
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
        out: &mut Vec<(PaneId, Rect)>,
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
                let first_extent =
                    (u32::from(extent) * u32::from(ratio.get()) / u32::from(RATIO_SCALE)) as u16;
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

    fn find_leaf(&self, pane: PaneId) -> Option<NodeId> {
        self.nodes.iter().enumerate().find_map(|(index, node)| {
            matches!(node, Some(Node::Leaf(id)) if *id == pane).then(|| NodeId(index as u32))
        })
    }
    fn first_leaf(&self, id: NodeId) -> Option<PaneId> {
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
                    Some((NodeId(index as u32), true))
                }
                Some(Node::Split { second, .. }) if *second == child => {
                    Some((NodeId(index as u32), false))
                }
                _ => None,
            })
    }
    fn parent_and_sibling(&self, child: NodeId) -> Option<(NodeId, NodeId)> {
        self.nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| match node {
                Some(Node::Split { first, second, .. }) if *first == child => {
                    Some((NodeId(index as u32), *second))
                }
                Some(Node::Split { first, second, .. }) if *second == child => {
                    Some((NodeId(index as u32), *first))
                }
                _ => None,
            })
    }
    fn walk_leaves(&self, id: NodeId, out: &mut Vec<PaneId>, seen: &mut HashSet<NodeId>) {
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
    fn insert(&mut self, node: Node) -> Result<NodeId, LayoutError> {
        if let Some(id) = self.free.pop() {
            self.set(id, node)?;
            return Ok(id);
        }
        let id = NodeId(u32::try_from(self.nodes.len()).map_err(|_| LayoutError::Limit)?);
        self.nodes.push(Some(node));
        Ok(id)
    }
    fn set(&mut self, id: NodeId, node: Node) -> Result<(), LayoutError> {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub name: String,
    pub layout: LayoutTree,
    pub focused: PaneId,
    pub zoomed: Option<PaneId>,
}

impl Tab {
    pub fn validate(&self) -> Result<(), LayoutError> {
        if self.name.len() > MAX_NAME_BYTES {
            return Err(LayoutError::Limit);
        }
        self.layout.validate()?;
        if !self.layout.contains(self.focused)
            || self.zoomed.is_some_and(|id| !self.layout.contains(id))
        {
            return Err(LayoutError::MissingPane);
        }
        Ok(())
    }
}

//! Generic layout edits and a bounded, flat interchange format. This module is owned by fux;
//! it does not load or depend on a reference layout engine.

use super::{Axis, Direction, LayoutError, LayoutTree, MAX_LEAVES, Node, NodeId, Rect, half};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::HashSet;
use std::hash::Hash;
use std::num::NonZeroU16;

/// A flat node array avoids recursively deserializing untrusted split trees. Child indices refer
/// to this document only, never to the server's arena. Ratios use fux's 10,000-unit scale.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LayoutNode<L> {
    Pane {
        pane: L,
    },
    Split {
        axis: Axis,
        ratio: u16,
        first: u32,
        second: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutDocument<L> {
    pub root: Option<u32>,
    #[serde(
        deserialize_with = "bounded_nodes",
        bound(deserialize = "L: Deserialize<'de>")
    )]
    pub nodes: Vec<LayoutNode<L>>,
}

fn bounded_nodes<'de, D, L>(deserializer: D) -> Result<Vec<LayoutNode<L>>, D::Error>
where
    D: Deserializer<'de>,
    L: Deserialize<'de>,
{
    struct Nodes<L>(std::marker::PhantomData<L>);
    impl<'de, L: Deserialize<'de>> de::Visitor<'de> for Nodes<L> {
        type Value = Vec<LayoutNode<L>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a bounded array of layout nodes")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut nodes = Vec::new();
            while let Some(node) = seq.next_element()? {
                if nodes.len() >= MAX_LEAVES * 2 - 1 {
                    return Err(de::Error::custom("layout node limit exceeded"));
                }
                nodes.push(node);
            }
            Ok(nodes)
        }
    }
    deserializer.deserialize_seq(Nodes(std::marker::PhantomData))
}

impl<L: Copy + Eq + Hash> LayoutTree<L> {
    /// Canonical preorder export, independent of allocation/free-list history.
    pub fn document<M>(
        &self,
        mut map: impl FnMut(L) -> M,
    ) -> Result<LayoutDocument<M>, LayoutError> {
        self.validate()?;
        let mut nodes = Vec::new();
        let root = self
            .root
            .map(|root| self.export_node(root, &mut nodes, &mut map))
            .transpose()?;
        Ok(LayoutDocument { root, nodes })
    }

    fn export_node<M>(
        &self,
        id: NodeId,
        nodes: &mut Vec<LayoutNode<M>>,
        map: &mut impl FnMut(L) -> M,
    ) -> Result<u32, LayoutError> {
        let index = u32::try_from(nodes.len()).map_err(|_| LayoutError::Limit)?;
        match self.node(id).ok_or(LayoutError::MissingNode)? {
            Node::Leaf(pane) => nodes.push(LayoutNode::Pane { pane: map(*pane) }),
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                nodes.push(LayoutNode::Split {
                    axis: *axis,
                    ratio: ratio.get(),
                    first: 0,
                    second: 0,
                });
                let first = self.export_node(*first, nodes, map)?;
                let second = self.export_node(*second, nodes, map)?;
                *nodes
                    .get_mut(index as usize)
                    .ok_or(LayoutError::MissingNode)? = LayoutNode::Split {
                    axis: *axis,
                    ratio: ratio.get(),
                    first,
                    second,
                };
            }
        }
        Ok(index)
    }

    /// Construct and validate without touching a live tree. `expected` must be exactly the pane
    /// set owned by the destination. Mapping to server entities happens before this call.
    pub fn from_document(document: LayoutDocument<L>, expected: &[L]) -> Result<Self, LayoutError> {
        if document.nodes.len() > MAX_LEAVES * 2 - 1 || expected.len() > MAX_LEAVES {
            return Err(LayoutError::Limit);
        }
        let nodes = document
            .nodes
            .into_iter()
            .map(|node| {
                Ok(Some(match node {
                    LayoutNode::Pane { pane } => Node::Leaf(pane),
                    LayoutNode::Split {
                        axis,
                        ratio,
                        first,
                        second,
                    } => Node::Split {
                        axis,
                        ratio: NonZeroU16::new(ratio).ok_or(LayoutError::InvalidRatio)?,
                        first: NodeId(first),
                        second: NodeId(second),
                    },
                }))
            })
            .collect::<Result<Vec<_>, LayoutError>>()?;
        let tree = Self {
            nodes,
            root: document.root.map(NodeId),
            free: Vec::new(),
        };
        tree.validate()?;
        let expected_set: HashSet<_> = expected.iter().copied().collect();
        if expected_set.len() != expected.len() {
            return Err(LayoutError::DuplicatePane);
        }
        if tree.leaves().into_iter().collect::<HashSet<_>>() != expected_set {
            return Err(LayoutError::MissingPane);
        }
        Ok(tree)
    }

    /// Exchange positions only. The caller's focused pane identity remains unchanged.
    pub fn swap(&mut self, a: L, b: L) -> Result<(), LayoutError> {
        self.validate()?;
        let a_id = self.find_leaf(a).ok_or(LayoutError::MissingPane)?;
        let b_id = self.find_leaf(b).ok_or(LayoutError::MissingPane)?;
        self.set(a_id, Node::Leaf(b))?;
        self.set(b_id, Node::Leaf(a))
    }

    /// Remove a pane and reinsert it beside a target. Failure leaves the original tree intact.
    pub fn relocate(&mut self, pane: L, target: L, side: Direction) -> Result<(), LayoutError> {
        self.validate()?;
        if pane == target {
            return Err(LayoutError::DuplicatePane);
        }
        if !self.contains(pane) || !self.contains(target) {
            return Err(LayoutError::MissingPane);
        }
        let mut next = self.clone();
        next.close(pane)?;
        let axis = match side {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        };
        next.split(target, pane, axis, half())?;
        if matches!(side, Direction::Left | Direction::Up) {
            next.swap(pane, target)?;
        }
        *self = next;
        Ok(())
    }

    /// Depth-first traversal with wraparound, including leaves temporarily hidden by small areas.
    pub fn cycle(&self, pane: L, forward: bool) -> Option<L> {
        let leaves = self.leaves();
        let index = leaves.iter().position(|id| *id == pane)?;
        let next = if forward {
            (index + 1) % leaves.len()
        } else {
            (index + leaves.len() - 1) % leaves.len()
        };
        leaves.get(next).copied()
    }

    /// Nearest ancestor whose boundary lies on the requested side of the pane. Positive delta
    /// grows that branch toward the boundary; negative shrinks it. Other orientations are skipped.
    pub fn resize_toward(
        &mut self,
        pane: L,
        direction: Direction,
        delta: i16,
    ) -> Result<(), LayoutError> {
        self.validate()?;
        let mut child = self.find_leaf(pane).ok_or(LayoutError::MissingPane)?;
        while let Some((parent, is_first, _)) = self.parent(child) {
            let Some(Node::Split { axis, ratio, .. }) = self.node(parent).copied() else {
                return Err(LayoutError::MissingNode);
            };
            let matches = match direction {
                Direction::Right => axis == Axis::Horizontal && is_first,
                Direction::Left => axis == Axis::Horizontal && !is_first,
                Direction::Down => axis == Axis::Vertical && is_first,
                Direction::Up => axis == Axis::Vertical && !is_first,
            };
            if matches {
                let delta = i32::from(delta) * if is_first { 1 } else { -1 };
                let ratio = (i32::from(ratio.get()) + delta)
                    .clamp(i32::from(super::MIN_RATIO), i32::from(super::MAX_RATIO));
                return self.set_ratio(
                    parent,
                    u16::try_from(ratio).map_err(|_| LayoutError::InvalidRatio)?,
                );
            }
            child = parent;
        }
        Err(LayoutError::MissingNode)
    }

    /// Set one split ratio. Node IDs are arena-local; callers must guard them with a generation.
    pub fn set_ratio(&mut self, split: NodeId, ratio: u16) -> Result<(), LayoutError> {
        let ratio = NonZeroU16::new(ratio).ok_or(LayoutError::InvalidRatio)?;
        Self::check_ratio(ratio)?;
        let Node::Split {
            axis,
            first,
            second,
            ..
        } = self.node(split).copied().ok_or(LayoutError::MissingNode)?
        else {
            return Err(LayoutError::MissingNode);
        };
        self.set(
            split,
            Node::Split {
                axis,
                ratio,
                first,
                second,
            },
        )
    }

    /// Geometry of split containers for border hit testing and absolute pointer resizing.
    pub fn splits(&self, area: Rect) -> Result<Vec<(NodeId, Axis, Rect, u16)>, LayoutError> {
        self.validate()?;
        let mut output = Vec::new();
        let mut pending = self.root.map(|root| vec![(root, area)]).unwrap_or_default();
        while let Some((id, rect)) = pending.pop() {
            if let Some(Node::Split {
                axis,
                ratio,
                first,
                second,
            }) = self.node(id)
            {
                let (a, b) = super::split_rect(rect, *axis, *ratio);
                output.push((id, *axis, rect, ratio.get()));
                pending.push((*second, b));
                pending.push((*first, a));
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn tree() -> LayoutTree<u32> {
        let mut tree = LayoutTree::new(1);
        assert!(tree.split(1, 2, Axis::Horizontal, half()).is_ok());
        assert!(tree.split(1, 3, Axis::Vertical, half()).is_ok());
        tree
    }

    #[test]
    fn swap_and_relocate_have_distinct_geometry_and_preserve_membership() {
        let mut tree = tree();
        assert!(tree.swap(1, 2).is_ok());
        assert_eq!(tree.leaves(), vec![2, 3, 1]);
        assert!(tree.relocate(1, 3, Direction::Up).is_ok());
        assert_eq!(tree.leaves(), vec![2, 1, 3]);
        assert_eq!(tree.cycle(3, true), Some(2));
        assert_eq!(tree.cycle(2, false), Some(3));
        let before = tree.clone();
        assert_eq!(
            tree.relocate(1, 999, Direction::Left),
            Err(LayoutError::MissingPane)
        );
        assert_eq!(tree, before);
    }

    #[test]
    fn directional_resize_skips_unrelated_parent_and_reports_missing_edge() {
        let mut tree = tree();
        assert!(tree.resize_toward(3, Direction::Right, 1_000).is_ok());
        let doc = tree.document(|pane| pane).ok();
        assert!(matches!(
            doc.and_then(|doc| doc.nodes.first().cloned()),
            Some(LayoutNode::Split { ratio: 6_000, .. })
        ));
        let before = tree.clone();
        assert_eq!(
            tree.resize_toward(3, Direction::Left, 100),
            Err(LayoutError::MissingNode)
        );
        assert_eq!(tree, before);
    }

    #[test]
    fn export_is_canonical_and_mapping_is_explicit() -> Result<(), Box<dyn std::error::Error>> {
        let mut tree = tree();
        tree.relocate(2, 3, Direction::Right)?;
        let document = tree.document(|pane| pane + 10)?;
        let encoded = serde_json::to_string(&document)?;
        let imported: LayoutDocument<u32> = serde_json::from_str(&encoded)?;
        let restored = LayoutTree::from_document(imported, &[11, 12, 13])?;
        assert_eq!(restored.document(|pane| pane)?, document);
        assert_eq!(restored.leaves(), vec![11, 13, 12]);
        assert!(LayoutTree::from_document(document.clone(), &[11, 12]).is_err());
        assert!(LayoutTree::from_document(document, &[11, 12, 99]).is_err());
        Ok(())
    }

    #[test]
    fn untrusted_documents_reject_cycles_aliases_unreachable_nodes_and_invalid_ratios() {
        let split = |ratio, first, second| LayoutNode::Split {
            axis: Axis::Horizontal,
            ratio,
            first,
            second,
        };
        let cases = [
            vec![split(5_000, 0, 1), LayoutNode::Pane { pane: 1 }],
            vec![split(5_000, 1, 1), LayoutNode::Pane { pane: 1 }],
            vec![LayoutNode::Pane { pane: 1 }, LayoutNode::Pane { pane: 2 }],
            vec![
                split(0, 1, 2),
                LayoutNode::Pane { pane: 1 },
                LayoutNode::Pane { pane: 2 },
            ],
            vec![
                split(10_000, 1, 2),
                LayoutNode::Pane { pane: 1 },
                LayoutNode::Pane { pane: 2 },
            ],
            vec![
                split(5_000, 1, 2),
                LayoutNode::Pane { pane: 1 },
                LayoutNode::Pane { pane: 1 },
            ],
            vec![split(5_000, 1, u32::MAX), LayoutNode::Pane { pane: 1 }],
        ];
        for nodes in cases {
            assert!(
                LayoutTree::from_document(
                    LayoutDocument {
                        root: Some(0),
                        nodes
                    },
                    &[1, 2]
                )
                .is_err()
            );
        }
    }

    #[test]
    fn depth_limit_rejects_split_without_partial_edit() {
        let mut tree = LayoutTree::new(0_u32);
        for pane in 1..=super::super::MAX_DEPTH {
            assert!(tree.split(0, pane as u32, Axis::Horizontal, half()).is_ok());
        }
        let before = tree.clone();
        assert_eq!(
            tree.split(0, 999, Axis::Horizontal, half()),
            Err(LayoutError::Limit)
        );
        assert_eq!(tree, before);
    }

    #[test]
    fn serialized_node_count_is_bounded_before_tree_construction() {
        let node = r#"{"kind":"pane","pane":1}"#;
        let input = format!(
            r#"{{"root":0,"nodes":[{}]}}"#,
            vec![node; MAX_LEAVES * 2].join(",")
        );
        assert!(serde_json::from_str::<LayoutDocument<u32>>(&input).is_err());
    }

    proptest! {
        #[test]
        fn mutation_sequences_round_trip_and_keep_geometry_disjoint(
            ops in proptest::collection::vec((0_u8..5, any::<u8>(), any::<u8>()), 0..80),
            width in 0_u16..150, height in 0_u16..80,
        ) {
            let mut tree = tree();
            for (op, a, b) in ops {
                let a = u32::from(a % 3) + 1;
                let b = u32::from(b % 3) + 1;
                let direction = match op % 4 { 0 => Direction::Left, 1 => Direction::Right, 2 => Direction::Up, _ => Direction::Down };
                let before = tree.clone();
                let result = match op {
                    0 => tree.swap(a, b),
                    1..=3 => tree.relocate(a, b, direction),
                    _ => tree.resize_toward(a, direction, 1_234),
                };
                if result.is_err() { prop_assert_eq!(&tree, &before); }
                prop_assert!(tree.validate().is_ok());
                let doc = tree.document(|pane| pane)?;
                let restored = LayoutTree::from_document(doc.clone(), &[1, 2, 3])?;
                prop_assert_eq!(restored.document(|pane| pane)?, doc);
                let geometry = tree.geometry(Rect { x: 0, y: 0, width, height })?;
                for (i, (_, a)) in geometry.iter().enumerate() {
                    prop_assert!(a.x <= width && a.y <= height);
                    prop_assert!(u32::from(a.x) + u32::from(a.width) <= u32::from(width));
                    prop_assert!(u32::from(a.y) + u32::from(a.height) <= u32::from(height));
                    for (_, b) in geometry.iter().skip(i + 1) {
                        prop_assert!(a.width == 0 || a.height == 0 || b.width == 0 || b.height == 0
                            || a.x + a.width <= b.x || b.x + b.width <= a.x
                            || a.y + a.height <= b.y || b.y + b.height <= a.y);
                    }
                }
            }
        }
    }
}

//! Layout trees, laid out and changed by the baseline and the current fux:
//! the same placements, the same resizes (what they return and the trees
//! they leave), and the same trees after random splits and removals.
//! Trees are both normalized, as fux keeps them, and not; weights run from
//! 0 to `u32::MAX`, and areas from empty to `u16::MAX` wide, on the screen
//! and past its edge.
use crate::rng::Rng;
use crate::{Outcome, bump, same, times};

/// A tree, for both to build.
#[derive(Clone, Debug)]
enum Tree {
    Pane(u32),
    Split(bool, Vec<(u32, Tree)>),
}

fn weight(r: &mut Rng) -> u32 {
    match r.below(8) {
        0 => 0,
        1 => 1,
        2 => 1000,
        3 => u32::MAX,
        4 => u32::MAX.saturating_sub(1),
        5 => u32::try_from(r.next() >> 32).unwrap_or(0),
        _ => u32::try_from(r.below(3000)).unwrap_or(0).saturating_add(1),
    }
}

/// A tree of up to about 15 panes; a normalized one alternates its axes and
/// gives every split at least two children.
fn tree(r: &mut Rng, depth: u32, next: &mut u32, normal: bool, parent: Option<bool>) -> Tree {
    let leaf = if depth == 0 { 12 } else { 33 };
    if depth >= 4 || *next >= 14 || r.chance(leaf) {
        *next = next.saturating_add(1);
        return Tree::Pane(*next);
    }
    let horizontal = match (normal, parent) {
        (true, Some(parent)) => !parent,
        _ => r.chance(50),
    };
    let count = if normal {
        r.below(3).saturating_add(2)
    } else {
        r.below(5)
    };
    let children = (0..count)
        .map(|_| {
            (
                weight(r),
                tree(r, depth.saturating_add(1), next, normal, Some(horizontal)),
            )
        })
        .collect();
    Tree::Split(horizontal, children)
}

fn length(r: &mut Rng) -> u16 {
    let n = match r.below(10) {
        0 => 0,
        1 => r.below(4).saturating_add(1),
        2 => usize::from(u16::MAX),
        3 => r.below(65_536),
        4..=6 => r.below(30),
        _ => r.below(300),
    };
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// One change to a tree.
#[derive(Clone, Copy, Debug)]
enum Change {
    Resize {
        pane: u32,
        direction: usize,
        amount: u16,
    },
    Split {
        target: u32,
        new: u32,
        horizontal: bool,
        after: bool,
    },
    Remove(u32),
    Swap(u32, u32),
}

macro_rules! stack {
    ($name:ident, $fux:ident, $ids:ident) => {
        mod $name {
            use super::{Change, Tree};
            use $fux::keys::Direction;
            use $fux::layout::{self, Axis, Node, Placement, Rect, Side};
            use $fux::$ids::PaneId;

            fn axis(horizontal: bool) -> Axis {
                if horizontal {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                }
            }

            /// Pane `n` as a person names it, which both sides read alike:
            /// the current side makes IDs from nothing else.
            fn pane(n: u32) -> Option<PaneId> {
                $fux::command::parse_pane(&format!("%{n}")).ok()
            }

            fn node(tree: &Tree) -> Option<Node> {
                Some(match tree {
                    Tree::Pane(p) => Node::Pane(pane(*p)?),
                    Tree::Split(horizontal, children) => Node::Split {
                        axis: axis(*horizontal),
                        children: children
                            .iter()
                            .map(|(w, c)| Some((*w, node(c)?)))
                            .collect::<Option<_>>()?,
                    },
                })
            }

            pub struct Laid {
                root: Option<Node>,
                area: Rect,
                placed: Placement,
            }

            impl Laid {
                pub fn new(tree: &Tree, (x, y, w, h): (u16, u16, u16, u16)) -> Laid {
                    Laid {
                        root: node(tree),
                        area: Rect { x, y, w, h },
                        placed: Placement::default(),
                    }
                }

                /// The tree and where its panes and separators are placed.
                pub fn shown(&mut self) -> String {
                    match &self.root {
                        Some(root) => {
                            layout::place_into(root, self.area, &mut self.placed);
                            format!(
                                "{root:?}\n{:?}\n{:?}\n{:?}",
                                self.placed.panes,
                                self.placed.separators,
                                layout::place(root, self.area).panes
                            )
                        }
                        None => "empty".into(),
                    }
                }

                pub fn panes(&self) -> Vec<u32> {
                    self.root
                        .as_ref()
                        .map(Node::panes)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|p| p.to_string().get(1..)?.parse().ok())
                        .collect()
                }

                /// Makes the change; what it returned.
                pub fn change(&mut self, change: Change) -> String {
                    match change {
                        Change::Resize {
                            pane: p,
                            direction,
                            amount,
                        } => {
                            let direction = Direction::ALL
                                .get(direction)
                                .copied()
                                .unwrap_or(Direction::Left);
                            match (&mut self.root, pane(p)) {
                                (Some(root), Some(p)) => {
                                    layout::resize(root, self.area, p, direction, amount)
                                        .to_string()
                                }
                                _ => String::new(),
                            }
                        }
                        Change::Split {
                            target,
                            new,
                            horizontal,
                            after,
                        } => {
                            let side = if after { Side::After } else { Side::Before };
                            let axis = axis(horizontal);
                            let split = |(t, n)| layout::split(&mut self.root, t, n, axis, side);
                            pane(target).zip(pane(new)).is_some_and(split).to_string()
                        }
                        Change::Remove(p) => pane(p)
                            .is_some_and(|p| layout::remove(&mut self.root, p))
                            .to_string(),
                        Change::Swap(a, b) => {
                            if let (Some(root), Some(a), Some(b)) =
                                (&mut self.root, pane(a), pane(b))
                            {
                                layout::swap(root, a, b);
                            }
                            String::new()
                        }
                    }
                }

                /// The neighbour of every placed pane in every direction.
                pub fn neighbors(&self) -> String {
                    let mut out = String::new();
                    for (pane, _) in &self.placed.panes {
                        for direction in Direction::ALL {
                            out.push_str(&format!(
                                "{:?} ",
                                layout::neighbor(&self.placed, *pane, direction)
                            ));
                        }
                    }
                    out
                }
            }
        }
    };
}

stack!(base, baseline, layout);
stack!(cur, fux, id);

fn change(r: &mut Rng, panes: &[u32], next: &mut u32) -> Change {
    let pane = |r: &mut Rng| {
        if r.chance(5) {
            99
        } else {
            r.pick(panes).copied().unwrap_or(99)
        }
    };
    match r.below(10) {
        0..=5 => Change::Resize {
            pane: pane(r),
            direction: r.below(4),
            amount: match r.below(4) {
                0 => u16::MAX,
                1 => u16::try_from(r.below(65_536)).unwrap_or(0),
                _ => u16::try_from(r.below(40)).unwrap_or(0),
            },
        },
        6 | 7 => {
            *next = next.saturating_add(1);
            Change::Split {
                target: pane(r),
                new: *next,
                horizontal: r.chance(50),
                after: r.chance(50),
            }
        }
        8 => Change::Remove(pane(r)),
        _ => Change::Swap(pane(r), pane(r)),
    }
}

pub fn run(r: &mut Rng, scale: usize) -> Outcome {
    let (mut trees, mut changes, mut shown, mut unnormal) = (0u64, 0u64, 0u64, 0u64);
    for case in 0..times(20_000, scale) {
        let normal = r.chance(75);
        let mut next = 0;
        let t = tree(r, 0, &mut next, normal, None);
        let (w, h) = (length(r), length(r));
        // Anywhere its far edge still fits a u16 screen, or anywhere.
        let (x, y) = if r.chance(90) {
            let near = r.chance(50);
            let at = |r: &mut Rng, len: u16| {
                let room = u16::MAX.saturating_sub(len);
                let n = if near {
                    r.below(4)
                } else {
                    r.below(usize::from(room).saturating_add(1))
                };
                u16::try_from(n).unwrap_or(0).min(room)
            };
            (at(r, w), at(r, h))
        } else {
            (length(r), length(r))
        };
        let area = (x, y, w, h);
        let (mut a, mut b) = (base::Laid::new(&t, area), cur::Laid::new(&t, area));
        let context = |log: &[Change]| format!("tree {case} {t:?} in {area:?}, after {log:?}");
        let mut log = Vec::new();
        same(&context(&log), a.shown(), b.shown())?;
        same(&context(&log), a.neighbors(), b.neighbors())?;
        for _ in 0..r.below(12).saturating_add(1) {
            let panes = a.panes();
            same(&context(&log), &panes, &b.panes())?;
            let c = change(r, &panes, &mut next);
            log.push(c);
            same(&context(&log), a.change(c), b.change(c))?;
            same(&context(&log), a.shown(), b.shown())?;
            same(&context(&log), a.neighbors(), b.neighbors())?;
            bump(&mut changes);
            bump(&mut shown);
        }
        bump(&mut trees);
        if !normal {
            bump(&mut unnormal);
        }
    }
    Ok(format!(
        "{trees} trees ({unnormal} not normalized), {changes} resizes, splits, removals and swaps, {shown} placements after them"
    ))
}

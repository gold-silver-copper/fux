use fux::state::{Axis, Direction, PaneId, RATIO_SCALE, Rect};

#[derive(Clone, Debug)]
pub enum Tree {
    Leaf(PaneId),
    Split {
        axis: Axis,
        ratio: u16,
        first: Box<Tree>,
        second: Box<Tree>,
    },
}

impl Tree {
    pub fn split(&mut self, target: PaneId, new_pane: PaneId, axis: Axis, ratio: u16) -> bool {
        match self {
            Self::Leaf(pane) if *pane == target => {
                *self = Self::Split {
                    axis,
                    ratio,
                    first: Box::new(Self::Leaf(target)),
                    second: Box::new(Self::Leaf(new_pane)),
                };
                true
            }
            Self::Split { first, second, .. } => {
                first.split(target, new_pane, axis, ratio)
                    || second.split(target, new_pane, axis, ratio)
            }
            Self::Leaf(_) => false,
        }
    }

    pub fn geometry(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut output = Vec::new();
        self.geometry_into(area, &mut output);
        output
    }

    pub fn neighbour(&self, pane: PaneId, direction: Direction, area: Rect) -> Option<PaneId> {
        let geometry = self.geometry(area);
        let source = geometry
            .iter()
            .find_map(|(id, rect)| (*id == pane).then_some(rect))?;
        let project = |rect: &Rect| match direction {
            Direction::Left | Direction::Right => (
                u32::from(rect.x),
                u32::from(rect.x) + u32::from(rect.width),
                u32::from(rect.y)..u32::from(rect.y) + u32::from(rect.height),
            ),
            Direction::Up | Direction::Down => (
                u32::from(rect.y),
                u32::from(rect.y) + u32::from(rect.height),
                u32::from(rect.x)..u32::from(rect.x) + u32::from(rect.width),
            ),
        };
        let (source_start, source_end, source_cross) = project(source);
        let mut candidates = geometry
            .iter()
            .enumerate()
            .filter(|(_, (id, _))| *id != pane)
            .filter_map(|(order, (id, rect))| {
                let (start, end, cross) = project(rect);
                let gap = match direction {
                    Direction::Left | Direction::Up if end <= source_start => source_start - end,
                    Direction::Right | Direction::Down if start >= source_end => start - source_end,
                    _ => return None,
                };
                let shared = source_cross
                    .end
                    .min(cross.end)
                    .checked_sub(source_cross.start.max(cross.start))?;
                (shared != 0).then_some((*id, shared, gap, order))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.3.cmp(&right.3))
        });
        candidates.first().map(|candidate| candidate.0)
    }

    fn geometry_into(&self, area: Rect, output: &mut Vec<(PaneId, Rect)>) {
        match self {
            Self::Leaf(pane) => output.push((*pane, area)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let extent = match axis {
                    Axis::Horizontal => area.width,
                    Axis::Vertical => area.height,
                };
                let first_extent =
                    (u32::from(extent) * u32::from(*ratio) / u32::from(RATIO_SCALE)) as u16;
                let second_extent = extent.saturating_sub(first_extent);
                let (first_area, second_area) = match axis {
                    Axis::Horizontal => (
                        Rect {
                            width: first_extent,
                            ..area
                        },
                        Rect {
                            x: area.x.saturating_add(first_extent),
                            width: second_extent,
                            ..area
                        },
                    ),
                    Axis::Vertical => (
                        Rect {
                            height: first_extent,
                            ..area
                        },
                        Rect {
                            y: area.y.saturating_add(first_extent),
                            height: second_extent,
                            ..area
                        },
                    ),
                };
                first.geometry_into(first_area, output);
                second.geometry_into(second_area, output);
            }
        }
    }
}

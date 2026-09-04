use fux::state::{Axis, PaneId, RATIO_SCALE, Rect};

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

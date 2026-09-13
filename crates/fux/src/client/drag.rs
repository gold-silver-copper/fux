//! Viewer-local layout gestures. The server validates the original geometry revision on drop.
use crate::ids::{PaneId, TabId};
use crate::layout::{Direction, Rect};
use crate::proto::attach::MouseEvent;
use crate::proto::control::{LayoutAction, Request};
use crate::view::Frame;

#[derive(Clone, Copy)]
enum Kind {
    Border,
    Pane(PaneId),
}

pub struct Drag {
    destinations: Vec<crate::view::TabEntry>,
    wheel_tab: Option<TabId>,
    tabs: Vec<(ratatui_core::layout::Rect, crate::view::TabEntry)>,
    instance: String,
    viewer: crate::ids::ViewerId,
    workspace: String,
    tab: TabId,
    generation: u64,
    start: (u16, u16),
    pointer: (u16, u16),
    kind: Kind,
}

impl Drag {
    /// Only left-button reports belong to a captured layout gesture. In particular, wheel
    /// and extra-button reports can share the low button bits with the left button.
    pub fn accepts(mouse: MouseEvent) -> bool {
        mouse.code & !(4 | 8 | 16 | 32) == 0 && !(mouse.release && mouse.motion())
    }

    pub fn start(
        mouse: MouseEvent,
        frame: &Frame,
        regions: &[(ratatui_core::layout::Rect, TabId)],
    ) -> Option<Self> {
        if !Self::accepts(mouse)
            || mouse.release
            || mouse.motion()
            || mouse.wheel()
            || mouse.button() != 0
            || mouse.shift()
            || frame.zoomed.is_some()
        {
            return None;
        }
        let point = (mouse.column.checked_sub(1)?, mouse.row.checked_sub(1)?);
        let kind = match frame.pane_at(point.0, point.1) {
            Some(pane) if mouse.code & 8 != 0 => Kind::Pane(pane.pane),
            Some(_) => return None,
            None => {
                let width = frame
                    .layout
                    .iter()
                    .map(|p| p.rect.x.saturating_add(p.rect.width))
                    .max()?;
                let height = frame
                    .layout
                    .iter()
                    .map(|p| p.rect.y.saturating_add(p.rect.height))
                    .max()?;
                if point.0 >= width || point.1 >= height || frame.layout.len() < 2 {
                    return None;
                }
                Kind::Border
            }
        };
        Some(Self {
            destinations: frame
                .tabs
                .iter()
                .filter(|tab| Some(tab.id) != frame.active_tab && tab.first_pane.is_some())
                .cloned()
                .collect(),
            wheel_tab: None,
            tabs: regions
                .iter()
                .filter_map(|(rect, id)| {
                    frame
                        .tabs
                        .iter()
                        .find(|tab| tab.id == *id)
                        .cloned()
                        .map(|tab| (*rect, tab))
                })
                .collect(),
            instance: frame.server_instance.clone(),
            viewer: frame.viewer,
            workspace: frame.workspace.clone(),
            tab: frame.active_tab?,
            generation: frame.layout_generation,
            start: point,
            pointer: point,
            kind,
        })
    }

    pub fn valid(&self, frame: &Frame) -> bool {
        frame.server_instance == self.instance
            && frame.viewer == self.viewer
            && frame.workspace == self.workspace
            && frame.active_tab == Some(self.tab)
            && frame.layout_generation == self.generation
            && frame.zoomed.is_none()
            && self.destinations.iter().all(|saved| {
                frame.tabs.iter().any(|tab| {
                    tab.id == saved.id
                        && tab.layout_generation == saved.layout_generation
                        && tab.first_pane == saved.first_pane
                })
            })
            && self.tabs.iter().all(|(_, saved)| {
                frame.tabs.iter().any(|tab| {
                    tab.id == saved.id
                        && tab.layout_generation == saved.layout_generation
                        && tab.first_pane == saved.first_pane
                })
            })
            && match self.kind {
                Kind::Border => true,
                Kind::Pane(pane) => frame.pane(pane).is_some(),
            }
    }

    pub fn update(&mut self, mouse: MouseEvent) {
        let pointer = (mouse.column.saturating_sub(1), mouse.row.saturating_sub(1));
        if pointer != self.pointer {
            self.wheel_tab = None;
        }
        self.pointer = pointer;
    }

    /// Wheel selection reaches destinations whose labels do not fit. It is captured only over
    /// painted tab labels; moving the pointer resumes direct hit testing.
    pub fn scroll_tabs(&mut self, mouse: MouseEvent) -> bool {
        let point = (mouse.column.saturating_sub(1), mouse.row.saturating_sub(1));
        if !matches!(self.kind, Kind::Pane(_))
            || self.destinations.is_empty()
            || !mouse.wheel()
            || mouse.release
            || mouse.code & !(4 | 8 | 16 | 64 | 1) != 0
            || !self
                .tabs
                .iter()
                .any(|(rect, _)| rect.contains(point.into()))
        {
            return false;
        }
        self.update(mouse);
        let current = self.target_tab().and_then(|tab| {
            self.destinations
                .iter()
                .position(|entry| entry.id == tab.id)
        });
        let count = self.destinations.len();
        let index = match (current, mouse.button() == 1) {
            (Some(index), true) => (index + 1) % count,
            (Some(index), false) => (index + count - 1) % count,
            (None, true) => 0,
            (None, false) => count - 1,
        };
        self.wheel_tab = self.destinations.get(index).map(|tab| tab.id);
        true
    }

    fn target(&self, frame: &Frame) -> Option<(PaneId, Direction)> {
        let target = frame.pane_at(self.pointer.0, self.pointer.1)?;
        if matches!(self.kind, Kind::Pane(pane) if pane == target.pane) {
            return None;
        }
        Some((target.pane, side(target.rect, self.pointer)))
    }

    fn target_tab(&self) -> Option<&crate::view::TabEntry> {
        if !matches!(self.kind, Kind::Pane(_)) {
            return None;
        }
        if let Some(id) = self.wheel_tab {
            return self.destinations.iter().find(|tab| tab.id == id);
        }
        self.tabs
            .iter()
            .find(|(rect, tab)| {
                tab.id != self.tab && tab.first_pane.is_some() && rect.contains(self.pointer.into())
            })
            .map(|(_, tab)| tab)
    }

    pub fn hint(&self, frame: &Frame) -> String {
        match self.kind {
            Kind::Border => format!(
                "Resize separator to {},{} · release to apply · Esc cancel",
                self.pointer.0 + 1,
                self.pointer.1 + 1
            ),
            Kind::Pane(pane) if self.workspace_pane().is_some() => {
                format!("Move pane {pane} to another workspace · release to choose · Esc cancel")
            }
            Kind::Pane(pane) if self.target_tab().is_some() => format!(
                "Move pane {pane} to tab {} · wheel: other tabs · release to apply · Esc cancel",
                self.target_tab().map_or("", |tab| tab.label.as_str())
            ),
            Kind::Pane(pane) => match self.target(frame) {
                Some((target, side)) => format!(
                    "Move pane {pane} to {side:?} of pane {target} · release to apply · Esc cancel"
                ),
                None => format!("Move pane {pane}: drag to another pane or tab · Esc cancel"),
            },
        }
    }

    pub fn preview(&self, frame: &Frame) -> Option<(PaneId, Direction)> {
        if self.valid(frame) && matches!(self.kind, Kind::Pane(_)) {
            self.target(frame)
        } else {
            None
        }
    }

    pub fn preview_tab(&self, frame: &Frame) -> Option<TabId> {
        self.valid(frame)
            .then(|| self.target_tab().map(|tab| tab.id))
            .flatten()
    }

    pub fn workspace_pane(&self) -> Option<PaneId> {
        let Kind::Pane(pane) = self.kind else {
            return None;
        };
        let (rect, _) = self.tabs.iter().min_by_key(|(rect, _)| rect.x)?;
        (self.pointer.1 == rect.y && self.pointer.0 < rect.x).then_some(pane)
    }

    pub fn finish(&self, frame: &Frame) -> Option<Request> {
        if !self.valid(frame) || self.pointer == self.start {
            return None;
        }
        let action = match self.kind {
            Kind::Border => LayoutAction::ResizeBorder {
                column: self.start.0,
                row: self.start.1,
                to_column: self.pointer.0,
                to_row: self.pointer.1,
            },
            Kind::Pane(pane) => {
                if let Some(tab) = self.target_tab() {
                    return Some(Request::Layout {
                        id: 0,
                        instance: None,
                        tab: self.tab,
                        generation: Some(self.generation),
                        action: LayoutAction::Transfer {
                            focus: false,
                            pane,
                            destination: crate::proto::control::PaneDestination::Tab {
                                ratio: 5000,
                                tab: tab.id,
                                generation: tab.layout_generation,
                                target: tab.first_pane?,
                            },
                            side: Direction::Right,
                        },
                    });
                }
                let (target, side) = self.target(frame)?;
                LayoutAction::Relocate { pane, target, side }
            }
        };
        Some(Request::Layout {
            id: 0,
            instance: None,
            tab: self.tab,
            generation: Some(self.generation),
            action,
        })
    }
}

fn side(rect: Rect, point: (u16, u16)) -> Direction {
    let x = u32::from(point.0.saturating_sub(rect.x));
    let y = u32::from(point.1.saturating_sub(rect.y));
    let width = u32::from(rect.width).max(1);
    let height = u32::from(rect.height).max(1);
    [
        (x * height, Direction::Left),
        ((width.saturating_sub(x + 1)) * height, Direction::Right),
        (y * width, Direction::Up),
        ((height.saturating_sub(y + 1)) * width, Direction::Down),
    ]
    .into_iter()
    .min_by_key(|(distance, _)| *distance)
    .map_or(Direction::Right, |(_, side)| side)
}

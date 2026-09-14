//! Contextual subsets of the command registry, bound to observed server-side identities.
use super::effects::Identity;
use super::hints::HintPanel;
use crate::commands::{Action, Target};
use crate::ids::{PaneId, TabId};
use crate::proto::attach::MouseEvent;
use crate::view::{Frame, MouseMode, TabEntry};
use std::collections::BTreeMap;

/// What a context menu was opened on.
#[derive(Clone, Copy)]
pub enum Subject {
    Pane(PaneId),
    Tab(TabId),
    Workspace,
}

pub struct Menu {
    subject: Subject,
    identity: Identity,
    focused: Option<PaneId>,
    tab: Option<TabId>,
    generation: u64,
    catalog: Vec<TabEntry>,
    title: String,
    pub actions: Vec<Action>,
    pub disabled: BTreeMap<usize, &'static str>,
    pub selected: usize,
}

pub struct SwapPicker {
    pane: PaneId,
    tab: TabId,
    identity: Identity,
    generation: u64,
    pub choices: Vec<(PaneId, String)>,
    pub selected: usize,
}

impl SwapPicker {
    pub fn new(frame: &Frame) -> Option<Self> {
        let pane = frame.focused?;
        if frame.server_instance.is_empty() {
            return None;
        }
        let choices: Vec<_> = frame
            .layout
            .iter()
            .filter(|entry| entry.pane != pane)
            .filter_map(|entry| {
                let view = frame.pane(entry.pane)?;
                let name = view.label.as_deref().unwrap_or(&view.title);
                Some((entry.pane, format!("{}: {name}", entry.pane)))
            })
            .collect();
        if choices.is_empty() {
            return None;
        }
        Some(Self {
            pane,
            tab: frame.active_tab?,
            identity: Identity::of(frame),
            generation: frame.layout_generation,
            choices,
            selected: 0,
        })
    }

    pub fn valid(&self, frame: &Frame) -> bool {
        self.identity.matches(frame)
            && frame.active_tab == Some(self.tab)
            && frame.layout_generation == self.generation
            && frame.pane(self.pane).is_some()
            && self
                .choices
                .iter()
                .all(|(pane, _)| frame.pane(*pane).is_some())
    }

    pub fn request(&self) -> Option<crate::proto::control::Request> {
        Some(crate::proto::control::Request::Layout {
            id: 0,
            instance: Some(self.identity.instance.clone()),
            tab: self.tab,
            generation: Some(self.generation),
            action: crate::proto::control::LayoutAction::Swap {
                pane: self.pane,
                target: self.choices.get(self.selected)?.0,
            },
        })
    }

    pub fn panel(&self) -> HintPanel {
        HintPanel::context(
            format!("Swap pane {} with", self.pane),
            self.choices
                .iter()
                .map(|(_, label)| label.clone())
                .collect(),
            "↑/↓ or j/k · Enter or click swap · Esc cancel",
            Some(self.selected),
        )
    }
}

impl Menu {
    pub fn new(subject: Subject, frame: &Frame, workspaces: bool) -> Option<Self> {
        if frame.server_instance.is_empty() {
            return None;
        }
        let (title, actions) = match subject {
            Subject::Pane(pane) => {
                let policy = frame.pane(pane)?.right_click;
                (
                    format!("Pane {pane} actions · right-click: {}", policy.name()),
                    Action::PANE_CONTEXT,
                )
            }
            Subject::Tab(tab) => {
                let entry = frame.tabs.iter().find(|entry| entry.id == tab)?;
                (
                    format!("Tab {} ({tab}) actions", entry.label),
                    Action::TAB_CONTEXT,
                )
            }
            Subject::Workspace => (
                format!("Workspace {} actions", frame.workspace),
                Action::WORKSPACE_CONTEXT,
            ),
        };
        let mut menu = Self {
            subject,
            title,
            actions: actions.to_vec(),
            disabled: BTreeMap::new(),
            selected: 0,
            identity: Identity::of(frame),
            focused: frame.focused,
            tab: frame.active_tab,
            generation: frame.layout_generation,
            catalog: frame.tabs.clone(),
        };
        let target = menu.target();
        menu.disabled = menu
            .actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                action
                    .unavailable(frame, target, workspaces)
                    .map(|reason| (index, reason))
            })
            .collect();
        Some(menu)
    }

    pub fn valid(&self, frame: &Frame) -> bool {
        self.identity.matches(frame)
            && frame.tabs == self.catalog
            && match self.subject {
                Subject::Pane(pane) => {
                    frame.active_tab == self.tab
                        && frame.layout_generation == self.generation
                        && frame.pane(pane).is_some_and(|pane| pane.exit.is_none())
                }
                Subject::Tab(tab) => frame.tabs.iter().any(|entry| entry.id == tab),
                Subject::Workspace => true,
            }
    }

    /// What the chosen action acts on. It is used by the same command dispatcher as key
    /// bindings; the viewer's real focus and tab never change.
    pub fn target(&self) -> Target {
        match self.subject {
            Subject::Pane(pane) => Target {
                focused: Some(pane),
                tab: self.tab,
                generation: self.generation,
            },
            // A tab menu acts on that tab alone: no pane of the visible tab is its target.
            Subject::Tab(tab) => Target {
                focused: None,
                tab: Some(tab),
                generation: self
                    .catalog
                    .iter()
                    .find(|entry| entry.id == tab)
                    .map_or(0, |entry| entry.layout_generation),
            },
            Subject::Workspace => Target {
                focused: self.focused,
                tab: self.tab,
                generation: self.generation,
            },
        }
    }

    pub fn panel(&self) -> HintPanel {
        let mut panel = HintPanel::context(
            self.title.clone(),
            self.actions
                .iter()
                .enumerate()
                .map(|(index, action)| {
                    self.disabled.get(&index).map_or_else(
                        || action.label().to_owned(),
                        |reason| format!("{} · {reason}", action.label()),
                    )
                })
                .collect(),
            "↑/↓ or j/k · Enter or click select · Esc dismiss",
            Some(self.selected),
        );
        panel.disable(self.disabled.keys().copied());
        panel
    }
}

pub fn mouse_subject(
    mouse: MouseEvent,
    frame: &Frame,
    tabs: &[(ratatui_core::layout::Rect, TabId)],
) -> Option<Subject> {
    let point = (mouse.column.saturating_sub(1), mouse.row.saturating_sub(1));
    if let Some((_, tab)) = tabs.iter().find(|(rect, _)| rect.contains(point.into())) {
        return Some(Subject::Tab(*tab));
    }
    if tabs
        .first()
        .is_some_and(|(rect, _)| point.1 == rect.y && point.0 < rect.x)
    {
        return Some(Subject::Workspace);
    }
    let entry = frame.pane_at(point.0, point.1)?;
    let pane = frame.pane(entry.pane)?;
    (mouse.code & 8 != 0
        || match pane.right_click {
            crate::view::RightClickPolicy::Auto => pane.modes.mouse_mode == MouseMode::None,
            crate::view::RightClickPolicy::Fux => true,
            crate::view::RightClickPolicy::Pane => false,
        })
    .then_some(Subject::Pane(entry.pane))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_accepts_output_but_rejects_replaced_identity_and_geometry() -> Result<(), &'static str>
    {
        let mut frame = Frame {
            server_instance: "server".into(),
            workspace: "default".into(),
            viewer: crate::ids::ViewerId(1),
            active_tab: Some(TabId(1)),
            focused: Some(PaneId(1)),
            ..Frame::default()
        };
        frame
            .panes
            .insert(PaneId(1), crate::view::PaneView::default());
        let menu = Menu::new(Subject::Pane(PaneId(1)), &frame, true).ok_or("menu")?;
        frame.generation += 1;
        if let Some(pane) = frame.panes.get_mut(&PaneId(1)) {
            pane.title = "new application title".into();
        }
        assert!(
            menu.valid(&frame),
            "ordinary output must not cancel the menu"
        );
        for kind in 0..5 {
            let mut changed = frame.clone();
            match kind {
                0 => changed.server_instance = "replacement".into(),
                1 => changed.workspace = "other".into(),
                2 => changed.viewer = crate::ids::ViewerId(2),
                3 => changed.layout_generation += 1,
                _ => {
                    changed.panes.remove(&PaneId(1));
                }
            }
            assert!(!menu.valid(&changed));
        }
        Ok(())
    }
}

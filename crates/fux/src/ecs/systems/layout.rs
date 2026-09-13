//! Layout phase: deterministic pane geometry per tab. A tab shown by several viewers is laid out
//! over the smallest of their areas; hidden tabs keep their last geometry.

use crate::ecs::components::{Pane, PaneState, Tab, TabOf, Viewer, Workspace};
use crate::ecs::messages::Effect;
use crate::ecs::support::{Effects, tab_area};
use bevy_ecs::prelude::*;

pub fn resolve_layout(
    mut tabs: Query<(Entity, &mut Tab)>,
    members: Query<&TabOf>,
    mut viewers: Query<&mut Viewer>,
    mut panes: Query<&mut Pane>,
    workspaces: Query<&Workspace>,
    mut effects: Effects,
) {
    for (tab, mut component) in &mut tabs {
        if !members
            .get(tab)
            .is_ok_and(|member| member.0 == component.workspace)
        {
            continue;
        }
        let showing = |viewer: &Viewer| viewer.selection.tab == Some(tab) && !viewer.detaching;
        let smallest = viewers
            .iter()
            .filter(|viewer| showing(viewer))
            .map(|viewer| (viewer.rows, viewer.cols))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1)));
        let area = smallest.map_or(component.area, |(rows, cols)| tab_area(rows, cols));
        if !component.layout_changed && area == component.area {
            continue;
        }
        if component.area != area {
            component.layout_generation = component.layout_generation.saturating_add(1);
        }
        component.area = area;
        let geometry = component
            .zoomed
            .filter(|pane| component.layout.contains(*pane))
            .map_or_else(
                || component.layout.geometry(area).unwrap_or_default(),
                |pane| vec![(pane, area)],
            );
        for (pane, rect) in &geometry {
            let Ok(mut pane) = panes.get_mut(*pane) else {
                continue;
            };
            let (rows, cols) = Pane::terminal_size(*rect);
            let size_changed = pane.terminal.size() != (rows, cols);
            if pane.rect == *rect && !size_changed {
                continue;
            }
            pane.rect = *rect;
            // Position belongs to the layout frame, not the terminal grid. Moving an
            // unchanged grid must not resize its emulator or notify the child PTY.
            if size_changed {
                pane.terminal.resize(rows, cols);
                pane.dirty = true;
                if matches!(pane.state, PaneState::Live { .. }) {
                    effects.emit(Effect::ResizePty {
                        pane: pane.id,
                        rows,
                        cols,
                    });
                }
            }
        }
        if component.geometry != geometry
            && let Ok(workspace) = workspaces.get(component.workspace)
        {
            effects.event(
                component.workspace,
                &workspace.name,
                crate::proto::control::Event::WorkspaceChanged { id: 0 },
            );
        }
        component.geometry = geometry;
        component.layout_changed = false;
        for mut viewer in &mut viewers {
            // Every viewer carries the tab catalog's transfer targets and revisions, including
            // tabs it is not currently showing. Ordinary pane output does not enter this path.
            if viewer.workspace == component.workspace && !viewer.detaching {
                viewer.dirty = true;
            }
        }
    }
}

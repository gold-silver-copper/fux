//! Layout phase: deterministic pane geometry per tab. A tab shown by several viewers is laid out
//! over the smallest of their areas; hidden tabs keep their last geometry.

use crate::ecs::components::{Pane, PaneState, Tab, Viewer, Workspace};
use crate::ecs::messages::Effect;
use crate::ecs::support::{Effects, tab_area};
use bevy_ecs::prelude::*;

pub fn resolve_layout(
    mut tabs: Query<(Entity, &mut Tab)>,
    workspaces: Query<&Workspace>,
    mut viewers: Query<&mut Viewer>,
    mut panes: Query<&mut Pane>,
    mut effects: Effects,
) {
    for (tab, mut component) in &mut tabs {
        if !workspaces
            .get(component.workspace)
            .is_ok_and(|workspace| workspace.tabs.contains(&tab))
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
        component.area = area;
        let geometry = component.layout.geometry(area).unwrap_or_default();
        for (pane, rect) in &geometry {
            let Ok(mut pane) = panes.get_mut(*pane) else {
                continue;
            };
            let (rows, cols) = Pane::terminal_size(*rect);
            if pane.rect == *rect && pane.terminal.size() == (rows, cols) {
                continue;
            }
            pane.rect = *rect;
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
        component.geometry = geometry;
        component.layout_changed = false;
        for mut viewer in &mut viewers {
            if showing(&viewer) {
                viewer.dirty = true;
            }
        }
    }
}

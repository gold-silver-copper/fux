//! Layout phase: deterministic pane geometry per tab. A tab shown by several viewers is laid out
//! over the smallest of their areas; hidden tabs keep their last geometry.

use crate::ecs::components::{Pane, PaneState, Tab, Viewer, Workspace};
use crate::ecs::messages::Effect;
use crate::ecs::support::{effect, mark_tab_dirty, tab_area};
use bevy_ecs::prelude::*;

pub fn resolve_layout(world: &mut World) {
    let tabs: Vec<(Entity, Entity)> = world
        .query::<(Entity, &Tab)>()
        .iter(world)
        .map(|(entity, tab)| (entity, tab.workspace))
        .collect();
    for (tab, workspace) in tabs {
        if !world
            .get::<Workspace>(workspace)
            .is_some_and(|workspace| workspace.tabs.contains(&tab))
        {
            continue;
        }
        let smallest = world
            .query::<&Viewer>()
            .iter(world)
            .filter(|viewer| viewer.selection.tab == Some(tab) && !viewer.detaching)
            .map(|viewer| (viewer.rows, viewer.cols))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1)));
        let Some((changed, area)) = world.get_mut::<Tab>(tab).map(|mut component| {
            let area = smallest.map_or(component.area, |(rows, cols)| tab_area(rows, cols));
            let changed = component.layout_changed || area != component.area;
            component.area = area;
            (changed, area)
        }) else {
            continue;
        };
        if !changed {
            continue;
        }
        let geometry = world
            .get::<Tab>(tab)
            .and_then(|component| component.layout.geometry(area).ok())
            .unwrap_or_default();
        for (pane, rect) in &geometry {
            let Some(mut component) = world.get_mut::<Pane>(*pane) else {
                continue;
            };
            if component.rect == *rect && component.terminal.size() == Pane::terminal_size(*rect) {
                continue;
            }
            component.rect = *rect;
            let (rows, cols) = Pane::terminal_size(*rect);
            component.terminal.resize(rows, cols);
            component.dirty = true;
            let id = component.id;
            let live = matches!(component.state, PaneState::Live { .. });
            if live {
                effect(
                    world,
                    Effect::ResizePty {
                        pane: id,
                        rows,
                        cols,
                    },
                );
            }
        }
        if let Some(mut component) = world.get_mut::<Tab>(tab) {
            component.geometry = geometry;
            component.layout_changed = false;
        }
        mark_tab_dirty(world, tab);
    }
}

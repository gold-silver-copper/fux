//! Snapshot phase: derive one frame per dirty viewer and publish it before the replies it
//! promises. Detaching viewers and retiring workspaces end with `exited` and a close.

use crate::ecs::components::{Pane, PaneState, Tab, Viewer, Workspace};
use crate::ecs::messages::{Effect, Inbound};
use crate::ecs::resources::Registry;
use crate::ecs::support::{Effects, ViewerExit};
use crate::proto::attach::ServerMessage;
use crate::view::{Frame, PaneRect, PaneView, TabEntry};
use bevy_ecs::prelude::*;

/// Everything the snapshot reads besides the viewer being published.
#[derive(bevy_ecs::system::SystemParam)]
pub struct Scene<'w, 's> {
    workspaces: Query<'w, 's, &'static Workspace>,
    tabs: Query<'w, 's, &'static Tab>,
    panes: Query<'w, 's, &'static Pane>,
    registry: Res<'w, Registry>,
}

pub fn publish_frames(
    mut viewers: Query<(Entity, &mut Viewer)>,
    scene: Scene,
    mut exit: ViewerExit,
    mut effects: Effects,
) {
    let mut order: Vec<(crate::ids::ViewerId, Entity)> = viewers
        .iter()
        .map(|(entity, viewer)| (viewer.id, entity))
        .collect();
    order.sort();
    for (_, entity) in order {
        let Ok((_, mut viewer)) = viewers.get_mut(entity) else {
            continue;
        };
        let id = viewer.id;
        if viewer.detaching {
            // A viewer detaching because its workspace retired was already told the exit status.
            if !viewer.exit_sent {
                effects.emit(Effect::ToViewer {
                    viewer: id,
                    message: ServerMessage::Exited { code: None },
                });
            }
            exit.despawn(entity, id, viewer.workspace, &mut effects);
            continue;
        }
        let retiring = scene
            .workspaces
            .get(viewer.workspace)
            .ok()
            .and_then(|workspace| workspace.retiring);
        let tab_dirty = viewer
            .selection
            .tab
            .and_then(|tab| scene.tabs.get(tab).ok())
            .is_some_and(|tab| {
                tab.geometry
                    .iter()
                    .any(|(pane, _)| scene.panes.get(*pane).is_ok_and(|pane| pane.dirty))
            });
        let needs_frame = viewer.dirty || tab_dirty || retiring.is_some();
        if !needs_frame && viewer.after_frame.is_empty() {
            continue;
        }
        if needs_frame {
            let frame = build_frame(&scene, &mut viewer, retiring.and_then(|r| r.exit_code));
            if frame.valid() {
                effects.emit(Effect::ToViewer {
                    viewer: id,
                    message: ServerMessage::State {
                        state: Box::new(frame),
                    },
                });
            } else {
                tracing::error!(
                    viewer = id.0,
                    "derived frame failed validation; not published"
                );
            }
        }
        viewer.dirty = false;
        for message in std::mem::take(&mut viewer.after_frame) {
            effects.emit(Effect::ToViewer {
                viewer: id,
                message,
            });
        }
        if let Some(retiring) = retiring {
            effects.emit(Effect::ToViewer {
                viewer: id,
                message: ServerMessage::Exited {
                    code: retiring.exit_code,
                },
            });
            viewer.detaching = true;
            viewer.exit_sent = true;
        }
    }
}

fn build_frame(scene: &Scene, viewer: &mut Viewer, exit_code: Option<u32>) -> Frame {
    let (name, tab_entities) = scene
        .workspaces
        .get(viewer.workspace)
        .map(|workspace| (workspace.name.clone(), workspace.tabs.clone()))
        .unwrap_or_default();
    let tabs: Vec<TabEntry> = tab_entities
        .iter()
        .filter_map(|tab| scene.tabs.get(*tab).ok())
        .map(|tab| TabEntry {
            id: tab.id,
            label: tab.label.clone(),
        })
        .collect();
    let active = viewer
        .selection
        .tab
        .filter(|tab| tab_entities.contains(tab))
        .and_then(|tab| scene.tabs.get(tab).ok().map(|component| (tab, component)));
    let geometry = active
        .map(|(_, tab)| tab.geometry.clone())
        .unwrap_or_default();
    let mut layout = Vec::with_capacity(geometry.len());
    let mut panes = std::collections::BTreeMap::new();
    for (pane, rect) in &geometry {
        let Ok(component) = scene.panes.get(*pane) else {
            continue;
        };
        if matches!(component.state, PaneState::Starting) {
            continue;
        }
        let Ok(view) = PaneView::from_screen(
            component.terminal.screen(),
            &component.published_title,
            0,
            component.state.exit_code(),
        ) else {
            continue;
        };
        layout.push(PaneRect {
            pane: component.id,
            rect: *rect,
        });
        panes.insert(component.id, view);
    }
    let focused = active
        .and_then(|(tab, component)| {
            viewer
                .selection
                .focus
                .get(&tab)
                .copied()
                .filter(|pane| component.layout.contains(*pane))
                .or_else(|| component.layout.leaves().first().copied())
        })
        .and_then(|pane| scene.panes.get(pane).ok())
        .map(|pane| pane.id)
        .filter(|id| panes.contains_key(id));
    viewer.generation = viewer.generation.wrapping_add(1);
    viewer.layout = geometry;
    Frame {
        workspace: name,
        generation: viewer.generation,
        tabs,
        active_tab: active.map(|(_, tab)| tab.id),
        focused,
        layout,
        panes,
        bindings: scene.registry.bindings.clone(),
        exit_code,
        message: viewer.notice.take(),
    }
}

/// End of step: clear pane dirty flags and the consumed inbound messages.
pub fn finish_step(mut panes: Query<&mut Pane>, mut inbound: ResMut<Messages<Inbound>>) {
    for mut pane in &mut panes {
        if pane.dirty {
            pane.dirty = false;
        }
    }
    inbound.clear();
}

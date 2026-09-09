//! Snapshot phase: derive one frame per dirty viewer and publish it before the replies it
//! promises. Detaching viewers and retiring workspaces end with `exited` and a close.

use crate::ecs::components::{Pane, PaneState, Tab, Viewer, Workspace};
use crate::ecs::messages::{Effect, Inbound};
use crate::ecs::resources::{Deadlines, Registry};
use crate::ecs::support::effect;
use crate::proto::attach::ServerMessage;
use crate::view::{Frame, PaneRect, PaneView, TabEntry};
use bevy_ecs::prelude::*;

pub fn publish_frames(world: &mut World) {
    let viewers: Vec<Entity> = {
        let mut entries: Vec<(crate::ids::ViewerId, Entity)> = world
            .query::<(Entity, &Viewer)>()
            .iter(world)
            .map(|(entity, viewer)| (viewer.id, entity))
            .collect();
        entries.sort();
        entries.into_iter().map(|(_, entity)| entity).collect()
    };
    for viewer in viewers {
        publish_viewer(world, viewer);
    }
}

fn publish_viewer(world: &mut World, viewer: Entity) {
    let Some((id, workspace, tab, dirty, detaching, exit_sent)) =
        world.get::<Viewer>(viewer).map(|viewer| {
            (
                viewer.id,
                viewer.workspace,
                viewer.selection.tab,
                viewer.dirty,
                viewer.detaching,
                viewer.exit_sent,
            )
        })
    else {
        return;
    };
    if detaching {
        // A viewer detaching because its workspace retired was already told the exit status.
        if !exit_sent {
            effect(
                world,
                Effect::ToViewer {
                    viewer: id,
                    message: ServerMessage::Exited { code: None },
                },
            );
        }
        crate::ecs::systems::requests::despawn_viewer(world, viewer);
        return;
    }
    let retiring = world
        .get::<Workspace>(workspace)
        .and_then(|workspace| workspace.retiring);
    let tab_dirty = tab.is_some_and(|tab| {
        world.get::<Tab>(tab).is_some_and(|component| {
            component
                .geometry
                .iter()
                .any(|(pane, _)| world.get::<Pane>(*pane).is_some_and(|pane| pane.dirty))
        })
    });
    let needs_frame = dirty || tab_dirty || retiring.is_some();
    let has_replies = world
        .get::<Viewer>(viewer)
        .is_some_and(|viewer| !viewer.after_frame.is_empty());
    if !needs_frame && !has_replies {
        return;
    }
    if needs_frame {
        let frame = build_frame(
            world,
            viewer,
            workspace,
            tab,
            retiring.and_then(|r| r.exit_code),
        );
        if frame.valid() {
            effect(
                world,
                Effect::ToViewer {
                    viewer: id,
                    message: ServerMessage::State {
                        state: Box::new(frame),
                    },
                },
            );
        } else {
            tracing::error!(
                viewer = id.0,
                "derived frame failed validation; not published"
            );
        }
    }
    let after: Vec<ServerMessage> = world
        .get_mut::<Viewer>(viewer)
        .map(|mut viewer| {
            viewer.dirty = false;
            std::mem::take(&mut viewer.after_frame)
        })
        .unwrap_or_default();
    for message in after {
        effect(
            world,
            Effect::ToViewer {
                viewer: id,
                message,
            },
        );
    }
    if let Some(retiring) = retiring {
        effect(
            world,
            Effect::ToViewer {
                viewer: id,
                message: ServerMessage::Exited {
                    code: retiring.exit_code,
                },
            },
        );
        if let Some(mut component) = world.get_mut::<Viewer>(viewer) {
            component.detaching = true;
            component.exit_sent = true;
        }
    }
}

fn build_frame(
    world: &mut World,
    viewer: Entity,
    workspace: Entity,
    tab: Option<Entity>,
    exit_code: Option<u32>,
) -> Frame {
    let (name, tab_entities) = world
        .get::<Workspace>(workspace)
        .map(|workspace| (workspace.name.clone(), workspace.tabs.clone()))
        .unwrap_or_default();
    let tabs: Vec<TabEntry> = tab_entities
        .iter()
        .filter_map(|tab| {
            let component = world.get::<Tab>(*tab)?;
            Some(TabEntry {
                id: component.id,
                label: component.label.clone(),
            })
        })
        .collect();
    let active = tab.filter(|tab| tab_entities.contains(tab));
    let geometry = active
        .and_then(|tab| world.get::<Tab>(tab))
        .map(|tab| tab.geometry.clone())
        .unwrap_or_default();
    let mut layout = Vec::with_capacity(geometry.len());
    let mut panes = std::collections::BTreeMap::new();
    for (pane, rect) in &geometry {
        let Some(component) = world.get::<Pane>(*pane) else {
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
        .and_then(|tab| {
            let selection = world.get::<Viewer>(viewer)?.selection.clone();
            crate::ecs::support::focus_in_tab(world, &selection, tab)
        })
        .and_then(|pane| world.get::<Pane>(pane))
        .map(|pane| pane.id)
        .filter(|id| panes.contains_key(id));
    let bindings = world.resource::<Registry>().bindings.clone();
    let (generation, message) = world
        .get_mut::<Viewer>(viewer)
        .map(|mut component| {
            component.generation = component.generation.wrapping_add(1);
            component.layout = geometry.clone();
            (component.generation, component.notice.take())
        })
        .unwrap_or((0, None));
    Frame {
        workspace: name,
        generation,
        tabs,
        active_tab: active
            .and_then(|tab| world.get::<Tab>(tab))
            .map(|tab| tab.id),
        focused,
        layout,
        panes,
        bindings,
        exit_code,
        message,
    }
}

/// End of step: clear dirty flags and consumed messages, publish the next deadline.
pub fn finish_step(world: &mut World) {
    let panes: Vec<Entity> = world
        .query::<(Entity, &Pane)>()
        .iter(world)
        .filter(|(_, pane)| pane.dirty)
        .map(|(entity, _)| entity)
        .collect();
    for pane in panes {
        if let Some(mut pane) = world.get_mut::<Pane>(pane) {
            pane.dirty = false;
        }
    }
    world.resource_mut::<Messages<Inbound>>().clear();
    let deadline = world.resource::<Deadlines>().next_ms;
    world.resource_mut::<Deadlines>().next_ms = deadline;
}

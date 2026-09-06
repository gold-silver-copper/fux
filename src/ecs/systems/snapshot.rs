//! Snapshot phase: derive one frame per dirty viewer and publish it before the replies it
//! promises. Detaching viewers and retiring workspaces end with `exited` and a close.

use crate::ecs::components::{Pane, PaneState, Sent, Tab, Tabs, Viewer, Workspace};
use crate::ecs::messages::{Effect, Inbound};
use crate::ecs::resources::{Clock, Ids};
use crate::ecs::support::{Effects, ViewerExit};
use crate::proto::attach::ServerMessage;
use crate::proto::control::Event;
use crate::view::{FrameUpdate, PaneRect, TabEntry};
use bevy_ecs::prelude::*;
use std::collections::HashSet;

/// The clock, the pacing limits and the deadline register the grid refresh works with.
#[derive(bevy_ecs::system::SystemParam)]
pub struct Pacing<'w> {
    clock: Res<'w, Clock>,
    limits: Res<'w, crate::ecs::resources::Limits>,
    deadlines: ResMut<'w, crate::ecs::resources::Deadlines>,
}

/// Everything the snapshot reads besides the viewer being published.
#[derive(bevy_ecs::system::SystemParam)]
pub struct Scene<'w, 's> {
    workspaces: Query<'w, 's, &'static Workspace>,
    members: Query<'w, 's, &'static Tabs>,
    tabs: Query<'w, 's, &'static Tab>,
    panes: Query<'w, 's, &'static Pane>,
    clock: Res<'w, Clock>,
}

/// Decides which viewers publish this step and reads the changed panes they show once into
/// their retained grids (rows stamped with the pane's output sequence), so frames for any number
/// of viewers derive from the grid instead of the screen. Output-driven frames are paced: a
/// viewer whose last frame went out less than the frame interval ago waits for a deadline; frames
/// that carry a reply, a selection change or a retirement are never delayed. `pane.output` events
/// are published here too, at most one per pane per output event interval, carrying the sequence
/// the change produced; a hidden pane is refreshed when its event is due.
pub fn refresh_grids(
    mut viewers: Query<&mut Viewer>,
    mut panes: Query<(Entity, &mut Pane)>,
    tabs: Query<&Tab>,
    workspaces: Query<&Workspace>,
    mut pacing: Pacing,
    mut effects: Effects,
) {
    let clock = *pacing.clock;
    let frame_interval_ms = pacing.limits.frame_interval_ms;
    let interval = pacing.limits.output_event_interval_ms;
    let deadlines = &mut *pacing.deadlines;
    let mut refresh: HashSet<Entity> = HashSet::new();
    for mut viewer in &mut viewers {
        if viewer.detaching {
            continue;
        }
        let shown: Vec<Entity> = viewer
            .selection
            .tab
            .and_then(|tab| tabs.get(tab).ok())
            .map(|tab| tab.geometry.iter().map(|(pane, _)| *pane).collect())
            .unwrap_or_default();
        let output = shown
            .iter()
            .any(|pane| panes.get(*pane).is_ok_and(|(_, pane)| pane.dirty));
        viewer.pending |= output;
        // Output within two intervals of the viewer's own input is its echo, shown at once.
        let echoing = clock.now_ms
            <= viewer
                .input_ms
                .saturating_add(frame_interval_ms.saturating_mul(2));
        let forced = viewer.dirty
            || echoing
            || !viewer.after_frame.is_empty()
            || workspaces
                .get(viewer.workspace)
                .is_ok_and(|workspace| workspace.retiring.is_some());
        let due_at = viewer.last_frame_ms.saturating_add(frame_interval_ms);
        if forced || clock.now_ms >= due_at {
            viewer.publish_now = true;
            refresh.extend(shown);
        } else if viewer.pending {
            deadlines.propose(due_at);
        }
    }
    for (entity, mut pane) in &mut panes {
        if !pane.dirty && !pane.event_pending {
            continue;
        }
        let due_at = pane
            .last_output_event_ms
            .map(|previous| previous.saturating_add(interval));
        let event_due = due_at.is_none_or(|due| clock.now_ms >= due);
        if pane.dirty && (refresh.contains(&entity) || event_due) {
            pane.refresh();
        }
        if !pane.event_pending {
            continue;
        }
        if !event_due {
            if let Some(due) = due_at {
                deadlines.propose(due);
            }
            continue;
        }
        pane.event_pending = false;
        let seq = pane.terminal.grid().seq();
        // Bytes that changed nothing (a bell, a no-op escape) advance no sequence and owe no event.
        if seq == pane.last_event_seq {
            continue;
        }
        pane.last_event_seq = seq;
        pane.last_output_event_ms = Some(clock.now_ms);
        let workspace = tabs
            .get(pane.tab)
            .and_then(|tab| workspaces.get(tab.workspace))
            .map(|workspace| workspace.name.clone());
        if let Ok(workspace) = workspace {
            effects.event(
                &workspace,
                Event::PaneOutput {
                    id: 0,
                    pane: pane.id,
                    seq,
                },
            );
        }
    }
}

pub fn publish_frames(
    mut viewers: Query<(Entity, &mut Viewer)>,
    scene: Scene,
    mut ids: ResMut<Ids>,
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
            let name = scene
                .workspaces
                .get(viewer.workspace)
                .map(|workspace| workspace.name.clone())
                .unwrap_or_default();
            exit.despawn(&mut ids, entity, id, &name, &mut effects);
            continue;
        }
        let retiring = scene
            .workspaces
            .get(viewer.workspace)
            .ok()
            .and_then(|workspace| workspace.retiring);
        let needs_frame =
            viewer.publish_now && (viewer.dirty || viewer.pending || retiring.is_some());
        viewer.publish_now = false;
        if !needs_frame && viewer.after_frame.is_empty() {
            continue;
        }
        if needs_frame {
            let frame = build_frame(&scene, &mut viewer, retiring.and_then(|r| r.exit_code));
            effects.emit(Effect::ToViewer {
                viewer: id,
                message: ServerMessage::State {
                    state: Box::new(frame),
                },
            });
        }
        viewer.dirty = false;
        viewer.pending = false;
        viewer.last_frame_ms = scene.clock.now_ms;
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

fn build_frame(scene: &Scene, viewer: &mut Viewer, exit_code: Option<u32>) -> FrameUpdate {
    let name = scene
        .workspaces
        .get(viewer.workspace)
        .map(|workspace| workspace.name.clone())
        .unwrap_or_default();
    let tab_entities: Vec<Entity> = scene
        .members
        .get(viewer.workspace)
        .map(|tabs| tabs.to_vec())
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
    // A viewer that holds nothing yet (attach, workspace switch) gets every pane in full.
    let full = viewer.sent.is_empty();
    let mut layout = Vec::with_capacity(geometry.len());
    let mut panes = std::collections::BTreeMap::new();
    for (pane, rect) in &geometry {
        let Ok(component) = scene.panes.get(*pane) else {
            continue;
        };
        if matches!(component.state, PaneState::Starting) {
            continue;
        }
        layout.push(PaneRect {
            pane: component.id,
            rect: *rect,
        });
        let grid = component.terminal.grid();
        let (rows, columns) = grid.size();
        let held = viewer
            .sent
            .get(&component.id)
            .filter(|sent| (sent.rows, sent.columns) == (rows, columns));
        let since = held.map(|sent| sent.seq);
        let seq = grid.seq();
        if since.is_some_and(|since| seq <= since) {
            continue;
        }
        panes.insert(component.id, grid.update(since));
        viewer
            .sent
            .insert(component.id, Sent { rows, columns, seq });
    }
    viewer
        .sent
        .retain(|id, _| layout.iter().any(|entry| entry.pane == *id));
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
        .filter(|id| layout.iter().any(|entry| entry.pane == *id));
    viewer.generation = viewer.generation.wrapping_add(1);
    viewer.layout = geometry;
    FrameUpdate {
        workspace: name,
        generation: viewer.generation,
        tabs,
        active_tab: active.map(|(_, tab)| tab.id),
        focused,
        layout,
        panes,
        exit_code,
        message: viewer.notice.take(),
        full,
    }
}

/// End of step: clear the consumed inbound messages.
pub fn finish_step(mut inbound: ResMut<Messages<Inbound>>) {
    inbound.clear();
}

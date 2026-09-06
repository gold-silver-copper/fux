//! Helpers shared by the ordered systems: id lookups, effect/event emission, reply routing,
//! layout membership edits and explicit ownership cascades.

use super::components::{Creation, Pane, PaneState, Selection, Tab, Viewer, Workspace};
use super::messages::{Effect, Requester};
use super::resources::{Ids, Registry};
use crate::ids::{PaneId, TabId, ViewerId};
use crate::layout::Rect;
use crate::proto::attach::ServerMessage;
use crate::proto::control::{self, ErrorCode, Reply, RequestId};
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemParam;

pub fn effect(world: &mut World, effect: Effect) {
    world.resource_mut::<Messages<Effect>>().write(effect);
}

/// Deferred viewer removal for typed systems: the entity goes at the next sync point, its id is
/// released now, and the outbox is closed after the messages already queued for it.
#[derive(SystemParam)]
pub struct ViewerExit<'w, 's> {
    commands: Commands<'w, 's>,
    ids: ResMut<'w, Ids>,
}

impl ViewerExit<'_, '_> {
    pub fn despawn(
        &mut self,
        viewer: Entity,
        id: ViewerId,
        workspace: Entity,
        effects: &mut Effects,
    ) {
        self.ids.viewers.remove(&id);
        self.commands.entity(viewer).despawn();
        effects.emit(Effect::CloseViewer { viewer: id });
        effects.event(
            workspace,
            control::Event::ClientDetached {
                id: 0,
                client: id.0,
            },
        );
    }
}

/// The effect outlet of a typed system: effects and control events, the latter named by the
/// workspace they concern.
#[derive(SystemParam)]
pub struct Effects<'w, 's> {
    writer: MessageWriter<'w, Effect>,
    workspaces: Query<'w, 's, &'static Workspace>,
}

impl Effects<'_, '_> {
    pub fn emit(&mut self, effect: Effect) {
        self.writer.write(effect);
    }

    /// Publishes a control event for `workspace`; a despawned workspace has no subscribers.
    pub fn event(&mut self, workspace: Entity, event: control::Event) {
        if let Ok(workspace) = self.workspaces.get(workspace) {
            self.writer.write(Effect::Event {
                workspace: workspace.name.clone(),
                event,
            });
        }
    }
}

pub fn event(world: &mut World, workspace: Entity, event: control::Event) {
    let Some(name) = world
        .get::<Workspace>(workspace)
        .map(|workspace| workspace.name.clone())
    else {
        return;
    };
    effect(
        world,
        Effect::Event {
            workspace: name,
            event,
        },
    );
}

/// Routes a reply to whoever asked. Viewer replies wait for the next frame so an acknowledgement
/// never overtakes the state it promises.
pub fn reply(world: &mut World, requester: Requester, reply: Reply) {
    match requester {
        Requester::Viewer(id) => {
            if let Some(entity) = viewer_entity(world, id)
                && let Some(mut viewer) = world.get_mut::<Viewer>(entity)
            {
                if let Reply::Failed { error, .. } = &reply {
                    viewer.notice = Some(sanitize_notice(&error.message));
                }
                viewer.after_frame.push(ServerMessage::Reply { reply });
                viewer.dirty = true;
            }
        }
        Requester::Control(token) => effect(world, Effect::ControlReply { token, reply }),
        Requester::Manager(token) => {
            let outcome = match reply {
                Reply::Completed {
                    result: control::CommandResult::Workspace { name },
                    ..
                } => super::messages::ManagerOutcome::Attach {
                    name,
                    created: true,
                },
                Reply::Failed { error, .. } => {
                    super::messages::ManagerOutcome::Failed(error.message)
                }
                other => super::messages::ManagerOutcome::Failed(format!(
                    "unexpected manager result {other:?}"
                )),
            };
            effect(world, Effect::Manager { token, outcome });
        }
    }
}

pub fn failed(id: RequestId, code: ErrorCode, message: impl Into<String>) -> Reply {
    Reply::failed(id, code, message)
}

pub fn sanitize_notice(message: &str) -> String {
    crate::view::printable(message, crate::view::MAX_MESSAGE_BYTES / 4)
}

pub fn viewer_entity(world: &World, id: ViewerId) -> Option<Entity> {
    world.resource::<Ids>().viewer(id)
}

pub fn pane_entity(world: &World, id: PaneId) -> Option<Entity> {
    world.resource::<Ids>().pane(id)
}

pub fn tab_entity(world: &World, id: TabId) -> Option<Entity> {
    world.resource::<Ids>().tab(id)
}

pub fn workspace_entity(world: &World, name: &str) -> Option<Entity> {
    world.resource::<Ids>().workspace(name)
}

pub fn pane_id(world: &World, pane: Entity) -> Option<PaneId> {
    world.get::<Pane>(pane).map(|pane| pane.id)
}

pub fn tab_id(world: &World, tab: Entity) -> Option<TabId> {
    world.get::<Tab>(tab).map(|tab| tab.id)
}

pub fn pane_tab(world: &World, pane: Entity) -> Option<Entity> {
    world.get::<Pane>(pane).map(|pane| pane.tab)
}

pub fn tab_workspace(world: &World, tab: Entity) -> Option<Entity> {
    world.get::<Tab>(tab).map(|tab| tab.workspace)
}

/// The workspace a pane belongs to, through its tab.
pub fn pane_workspace(world: &World, pane: Entity) -> Option<Entity> {
    tab_workspace(world, pane_tab(world, pane)?)
}

/// True when `pane` is a leaf of its tab's layout (visible somewhere).
pub fn pane_in_layout(world: &World, pane: Entity) -> bool {
    pane_tab(world, pane)
        .and_then(|tab| world.get::<Tab>(tab))
        .is_some_and(|tab| tab.layout.contains(pane))
}

pub fn live_panes_in_workspace(world: &mut World, workspace: Entity) -> usize {
    world
        .query::<&Pane>()
        .iter(world)
        .filter(|pane| {
            world
                .get::<Tab>(pane.tab)
                .is_some_and(|tab| tab.workspace == workspace)
        })
        .count()
}

pub fn viewers_of_workspace(world: &mut World, workspace: Entity) -> Vec<Entity> {
    world
        .query::<(Entity, &Viewer)>()
        .iter(world)
        .filter(|(_, viewer)| viewer.workspace == workspace && !viewer.detaching)
        .map(|(entity, _)| entity)
        .collect()
}

pub fn viewers_on_tab(world: &mut World, tab: Entity) -> Vec<Entity> {
    world
        .query::<(Entity, &Viewer)>()
        .iter(world)
        .filter(|(_, viewer)| viewer.selection.tab == Some(tab) && !viewer.detaching)
        .map(|(entity, _)| entity)
        .collect()
}

pub fn mark_workspace_dirty(world: &mut World, workspace: Entity) {
    let viewers = viewers_of_workspace(world, workspace);
    for viewer in viewers {
        if let Some(mut viewer) = world.get_mut::<Viewer>(viewer) {
            viewer.dirty = true;
        }
    }
}

pub fn mark_tab_dirty(world: &mut World, tab: Entity) {
    let viewers = viewers_on_tab(world, tab);
    for viewer in viewers {
        if let Some(mut viewer) = world.get_mut::<Viewer>(viewer) {
            viewer.dirty = true;
        }
    }
}

/// Every viewer looking at `tab` whose focus is `old` now focuses `next`; the workspace default
/// follows too. Focus entries for a removed pane are dropped when there is no successor.
pub fn retarget_focus(world: &mut World, tab: Entity, old: Entity, next: Option<Entity>) {
    let workspace = tab_workspace(world, tab);
    let viewers: Vec<Entity> = world
        .query::<(Entity, &Viewer)>()
        .iter(world)
        .filter(|(_, viewer)| viewer.selection.focus.get(&tab) == Some(&old))
        .map(|(entity, _)| entity)
        .collect();
    for viewer in viewers {
        if let Some(mut viewer) = world.get_mut::<Viewer>(viewer) {
            match next {
                Some(next) => viewer.selection.set_focus(tab, next),
                None => {
                    viewer.selection.focus.remove(&tab);
                }
            }
            viewer.dirty = true;
        }
    }
    if let Some(workspace) = workspace
        && let Some(mut workspace) = world.get_mut::<Workspace>(workspace)
        && workspace.selection.focus.get(&tab) == Some(&old)
    {
        match next {
            Some(next) => workspace.selection.set_focus(tab, next),
            None => {
                workspace.selection.focus.remove(&tab);
            }
        }
    }
}

/// The pane a selection focuses in `tab`, falling back to the tab's first leaf.
pub fn focus_in_tab(world: &World, selection: &Selection, tab: Entity) -> Option<Entity> {
    let layout = &world.get::<Tab>(tab)?.layout;
    selection
        .focus
        .get(&tab)
        .copied()
        .filter(|pane| layout.contains(*pane))
        .or_else(|| layout.leaves().first().copied())
}

/// Removes `pane` from its tab's layout and returns the pane that inherits focus.
pub fn remove_from_layout(world: &mut World, pane: Entity) -> Option<Option<Entity>> {
    let tab = pane_tab(world, pane)?;
    let next = {
        let mut tab_component = world.get_mut::<Tab>(tab)?;
        if !tab_component.layout.contains(pane) {
            return None;
        }
        let next = tab_component.layout.close(pane).ok()?;
        tab_component.layout_changed = true;
        next
    };
    retarget_focus(world, tab, pane, next);
    mark_tab_dirty(world, tab);
    Some(next)
}

/// Despawns a pane and tells the adapter to drop its handles. The tab's layout must no longer
/// reference it.
pub fn despawn_pane(world: &mut World, pane: Entity) {
    let Some(id) = pane_id(world, pane) else {
        return;
    };
    world.resource_mut::<Ids>().panes.remove(&id);
    let viewers: Vec<Entity> = world
        .query::<(Entity, &Viewer)>()
        .iter(world)
        .filter(|(_, viewer)| viewer.barrier == Some(pane))
        .map(|(entity, _)| entity)
        .collect();
    for viewer in viewers {
        if let Some(mut viewer) = world.get_mut::<Viewer>(viewer) {
            viewer.barrier = None;
        }
    }
    world.despawn(pane);
    effect(world, Effect::ReleasePane { pane: id });
}

/// Requests termination of a pane's process if it is running.
pub fn terminate_pane(world: &mut World, pane: Entity, now_ms: u64, grace_ms: u64) {
    let Some(mut component) = world.get_mut::<Pane>(pane) else {
        return;
    };
    let id = component.id;
    match component.state {
        PaneState::Live { pid } | PaneState::Eof { pid } => {
            component.state = PaneState::Terminating {
                pid,
                since_ms: now_ms,
            };
            effect(world, Effect::Terminate { pane: id, grace_ms });
        }
        PaneState::Starting => {
            // The spawn has not been reported; the completion step rolls it back.
        }
        PaneState::Terminating { .. } | PaneState::Exited { .. } => {}
    }
}

/// Moves viewers showing `tab` to a neighbouring tab and removes the tab from its workspace.
/// Panes still in the layout are terminated (their exit reports finish the cleanup).
pub fn close_tab(world: &mut World, tab: Entity, now_ms: u64, grace_ms: u64) {
    let Some((workspace, id)) = world
        .get::<Tab>(tab)
        .map(|component| (component.workspace, component.id))
    else {
        return;
    };
    let (index, neighbour) = {
        let Some(component) = world.get::<Workspace>(workspace) else {
            return;
        };
        let index = component.tabs.iter().position(|entry| *entry == tab);
        let neighbour = index.and_then(|index| {
            component
                .tabs
                .get(index.wrapping_sub(1))
                .or_else(|| component.tabs.get(index + 1))
                .copied()
        });
        (index, neighbour)
    };
    let panes: Vec<Entity> = world
        .get::<Tab>(tab)
        .map(|component| component.layout.leaves())
        .unwrap_or_default();
    for pane in &panes {
        if let Some(mut component) = world.get_mut::<Tab>(tab) {
            let _ = component.layout.close(*pane);
        }
        terminate_pane(world, *pane, now_ms, grace_ms);
    }
    if let Some(mut component) = world.get_mut::<Workspace>(workspace) {
        if index.is_some() {
            component.tabs.retain(|entry| *entry != tab);
        }
        component.selection.forget_tab(tab);
        if component.selection.tab.is_none() {
            component.selection.tab = neighbour.or_else(|| component.tabs.first().copied());
        }
    }
    let viewers: Vec<Entity> = world
        .query::<(Entity, &Viewer)>()
        .iter(world)
        .filter(|(_, viewer)| viewer.workspace == workspace)
        .map(|(entity, _)| entity)
        .collect();
    for viewer in viewers {
        if let Some(mut viewer) = world.get_mut::<Viewer>(viewer) {
            let was_showing = viewer.selection.tab == Some(tab);
            viewer.selection.forget_tab(tab);
            if was_showing {
                viewer.selection.tab = neighbour;
            }
            viewer.dirty = true;
        }
    }
    world.resource_mut::<Ids>().tabs.remove(&id);
    for pane in panes {
        // Panes that already exited leave immediately; running ones wait for their exit report
        // and are despawned by the lifecycle system when it arrives.
        if world
            .get::<Pane>(pane)
            .is_some_and(|component| matches!(component.state, PaneState::Exited { .. }))
        {
            despawn_pane(world, pane);
        }
    }
    fail_pending_creations_in_tab(world, tab, "tab closed before the pane started");
    world.despawn(tab);
    if index.is_some() {
        event(
            world,
            workspace,
            control::Event::TabClosed { id: 0, tab: id },
        );
    }
    mark_workspace_dirty(world, workspace);
}

/// Reservations still starting in `tab` are released before the tab goes: their requesters get
/// a failure now and the late completion (if the spawn succeeds) is stopped by the completion
/// phase because the pane id is no longer registered.
pub fn fail_pending_creations_in_tab(world: &mut World, tab: Entity, reason: &str) {
    let pending: Vec<Entity> = world
        .query_filtered::<(Entity, &Pane), With<Creation>>()
        .iter(world)
        .filter(|(_, pane)| pane.tab == tab)
        .map(|(entity, _)| entity)
        .collect();
    for entity in pending {
        let Some(creation) = world.entity_mut(entity).take::<Creation>() else {
            continue;
        };
        for (requester, request_id) in creation.requesters {
            reply(
                world,
                requester,
                failed(request_id, control::ErrorCode::Conflict, reason),
            );
        }
        despawn_pane(world, entity);
    }
}

/// Bounded pane input: chunks the bytes into attachment-sized writes.
pub fn write_pane(world: &mut World, pane: Entity, bytes: &[u8]) -> bool {
    let Some(component) = world.get::<Pane>(pane) else {
        return false;
    };
    if !component.state.accepts_input() {
        return false;
    }
    let id = component.id;
    for chunk in bytes.chunks(crate::proto::attach::MAX_INPUT_CHUNK) {
        effect(
            world,
            Effect::WriteInput {
                pane: id,
                bytes: chunk.to_vec(),
            },
        );
    }
    true
}

pub fn default_command(world: &World) -> Vec<String> {
    world.resource::<Registry>().default_command.clone()
}

/// Body area available to a tab for a viewer of the given size.
/// The pane area of a viewer: everything above the always-present one-row bar, which is the last
/// row.
pub fn tab_area(rows: u16, cols: u16) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: cols,
        height: rows.saturating_sub(1),
    }
}

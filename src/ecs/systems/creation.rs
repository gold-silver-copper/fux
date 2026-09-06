//! Pane/tab/workspace creation: reservations before the spawn, and the completion phase that
//! either inserts the new pane where it was requested or rolls the reservation back.

use crate::ecs::components::{
    Creation, CreationKind, Pane, PaneState, Selection, Tab, TabOf, Viewer, Workspace,
};
use crate::ecs::messages::{Effect, Inbound, Requester};
use crate::ecs::resources::{Clock, Ids, Limits};
use crate::ecs::support::{
    clear_barriers, default_command, despawn_pane, despawn_tab, despawn_workspace, effect, event,
    failed, focus_in_tab, mark_tab_dirty, mark_workspace_dirty, member_tabs, pane_entity,
    panes_in_workspace, reply, reply_all, tab_area, tab_workspace, terminate_pane, viewer_entity,
};
use crate::ecs::systems::lifecycle::TERMINATE_GRACE_MS;
use crate::ecs::systems::requests::switch_viewer_workspace;
use crate::ids::{PaneId, TabId};
use crate::layout::{LayoutTree, Rect, half};
use crate::proto::control::{CommandResult, ErrorCode, Event, Reply, RequestId};
use crate::terminal::ServerTerminal;
use bevy_ecs::prelude::*;
use std::path::PathBuf;

pub struct NewPane {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub requester: Requester,
    pub request_id: RequestId,
}

/// Reserves a pane entity and asks the adapter to spawn its process. Returns the reservation.
pub fn reserve_pane(
    world: &mut World,
    workspace: Entity,
    new: NewPane,
    kind: CreationKind,
    size: (u16, u16),
) -> Result<Entity, Reply> {
    let limits = world.resource::<Limits>().clone();
    if panes_in_workspace(world, workspace).len() >= limits.max_panes {
        return Err(failed(
            new.request_id,
            ErrorCode::Limit,
            "configured pane limit reached",
        ));
    }
    let argv = if new.argv.is_empty() {
        default_command(world)
    } else {
        new.argv
    };
    let id = world.resource_mut::<Ids>().next_pane().ok_or_else(|| {
        failed(
            new.request_id,
            ErrorCode::Limit,
            "pane identifiers exhausted",
        )
    })?;
    let tab = match &kind {
        CreationKind::Split { tab, .. }
        | CreationKind::NewTab { tab }
        | CreationKind::Workspace { tab } => *tab,
    };
    let (rows, cols) = crate::terminal::clamp_dims(size.0, size.1);
    let cwd = new
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    let entity = world
        .spawn((
            Pane {
                id,
                tab,
                argv: argv.clone(),
                cwd,
                state: PaneState::Starting,
                terminal: ServerTerminal::new(rows, cols, limits.scrollback_lines),
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: cols.saturating_add(2),
                    height: rows.saturating_add(2),
                },
                dirty: true,
                published_title: String::new(),
                last_output_event_ms: None,
            },
            Creation {
                requesters: vec![(new.requester, new.request_id)],
                kind,
            },
        ))
        .id();
    world.resource_mut::<Ids>().panes.insert(id, entity);
    if let Requester::Viewer(viewer) = new.requester
        && let Some(viewer) = viewer_entity(world, viewer)
        && let Some(mut viewer) = world.get_mut::<Viewer>(viewer)
    {
        viewer.barrier = Some(entity);
    }
    effect(
        world,
        Effect::SpawnPane {
            pane: id,
            argv,
            cwd: new.cwd,
            rows,
            cols,
        },
    );
    Ok(entity)
}

/// Spawns a tab entity that is not yet a member of its workspace.
pub fn reserve_tab(
    world: &mut World,
    workspace: Entity,
    label: Option<String>,
) -> Result<Entity, Reply> {
    let ids = world.resource_mut::<Ids>().next_tab();
    let Some(id) = ids else {
        return Err(failed(0, ErrorCode::Limit, "tab identifiers exhausted"));
    };
    let label = label.unwrap_or_else(|| {
        world
            .get_mut::<Workspace>(workspace)
            .map(|mut workspace| {
                workspace.tab_counter = workspace.tab_counter.saturating_add(1);
                if workspace.tab_counter == 1 {
                    "main".to_owned()
                } else {
                    format!("tab-{}", workspace.tab_counter)
                }
            })
            .unwrap_or_default()
    });
    let entity = world
        .spawn(Tab {
            id,
            workspace,
            label,
            layout: LayoutTree::new(Entity::PLACEHOLDER),
            geometry: Vec::new(),
            // Until a viewer shows the tab, lay it out for a conventional 80x24 terminal.
            area: tab_area(24, 80),
            layout_changed: true,
        })
        .id();
    world.resource_mut::<Ids>().tabs.insert(id, entity);
    Ok(entity)
}

/// Spawns a closed workspace entity with one reserved tab and pane.
pub fn reserve_workspace(
    world: &mut World,
    name: String,
    requester: Requester,
    request_id: RequestId,
) -> Result<Entity, Reply> {
    let limits = world.resource::<Limits>().clone();
    if world.resource::<Ids>().workspaces.len() >= limits.max_workspaces {
        return Err(failed(
            request_id,
            ErrorCode::Limit,
            "configured workspace limit reached",
        ));
    }
    if world.resource::<Ids>().workspace(&name).is_some() {
        return Err(failed(
            request_id,
            ErrorCode::Conflict,
            format!("workspace {name} already exists"),
        ));
    }
    let step = world.resource::<Clock>().step;
    let workspace = world
        .spawn(Workspace {
            name: name.clone(),
            selection: Selection::default(),
            last_attached: step,
            open: false,
            retiring: None,
            tab_counter: 0,
        })
        .id();
    world
        .resource_mut::<Ids>()
        .workspaces
        .insert(name.clone(), workspace);
    let tab = match reserve_tab(world, workspace, None) {
        Ok(tab) => tab,
        Err(reply) => {
            despawn_workspace(world, workspace);
            return Err(reply);
        }
    };
    match reserve_pane(
        world,
        workspace,
        NewPane {
            argv: Vec::new(),
            cwd: None,
            requester,
            request_id,
        },
        CreationKind::Workspace { tab },
        (crate::terminal::MIN_DIM.max(22), 80),
    ) {
        Ok(_) => Ok(workspace),
        Err(reply) => {
            despawn_tab(world, tab);
            despawn_workspace(world, workspace);
            Err(reply)
        }
    }
}

pub fn apply_spawn_completions(world: &mut World) {
    let completions: Vec<(PaneId, Result<u32, String>)> = world
        .resource::<Messages<Inbound>>()
        .iter_current_update_messages()
        .filter_map(|message| match message {
            Inbound::SpawnCompleted { pane, result } => Some((*pane, result.clone())),
            _ => None,
        })
        .collect();
    for (id, result) in completions {
        let Some(entity) = pane_entity(world, id) else {
            // The reservation was released (workspace killed, shutdown) before the process
            // reported in. Nothing owns it any more, so the adapter must stop and reap it.
            if result.is_ok() {
                effect(
                    world,
                    Effect::Terminate {
                        pane: id,
                        grace_ms: TERMINATE_GRACE_MS,
                    },
                );
                effect(world, Effect::ReleasePane { pane: id });
            }
            continue;
        };
        let Some(creation) = world.entity_mut(entity).take::<Creation>() else {
            continue;
        };
        clear_barriers(world, entity);
        match result {
            Ok(pid) => complete(world, entity, id, pid, creation),
            Err(message) => roll_back(world, entity, creation, &message),
        }
    }
}

fn complete(world: &mut World, entity: Entity, id: PaneId, pid: u32, creation: Creation) {
    let Creation { requesters, kind } = creation;
    let (tab, missing) = match kind {
        CreationKind::Split { tab, .. } => (tab, "tab closed before the pane started"),
        CreationKind::NewTab { tab } => (tab, "tab was discarded"),
        CreationKind::Workspace { tab } => (tab, "workspace was discarded"),
    };
    let Some(workspace) = tab_workspace(world, tab) else {
        return abandon(world, entity, pid, &requesters, missing);
    };
    match kind {
        CreationKind::Split { target, axis, .. } => {
            let mut target = Some(target).filter(|target| {
                world
                    .get::<Tab>(tab)
                    .is_some_and(|tab| tab.layout.contains(*target))
            });
            if target.is_none() {
                // The original target closed while the process started. Split the requester's
                // current focus in that tab instead, or its first pane.
                let selection = requesters
                    .first()
                    .and_then(|(requester, _)| match requester {
                        Requester::Viewer(viewer) => viewer_entity(world, *viewer)
                            .and_then(|viewer| world.get::<Viewer>(viewer))
                            .map(|viewer| viewer.selection.clone()),
                        _ => None,
                    })
                    .or_else(|| {
                        world
                            .get::<Workspace>(workspace)
                            .map(|w| w.selection.clone())
                    });
                target = selection.and_then(|selection| focus_in_tab(world, &selection, tab));
            }
            let inserted = target.is_some_and(|target| {
                world.get_mut::<Tab>(tab).is_some_and(|mut component| {
                    let ok = component.layout.split(target, entity, axis, half()).is_ok();
                    component.layout_changed |= ok;
                    ok
                })
            });
            if !inserted {
                return abandon(
                    world,
                    entity,
                    pid,
                    &requesters,
                    "the pane to split is no longer available",
                );
            }
            go_live(world, entity, pid);
            focus_requesters(world, &requesters, workspace, tab, entity);
            announce_pane(world, workspace, tab, entity, id);
            reply_all(world, requesters, |request| Reply::Completed {
                id: request,
                result: CommandResult::Pane { pane: id },
            });
        }
        CreationKind::NewTab { .. } => {
            let open = world
                .get::<Workspace>(workspace)
                .is_some_and(|workspace| workspace.retiring.is_none());
            let tab_limit = world.resource::<Limits>().max_tabs;
            if !open || member_tabs(world, workspace).len() >= tab_limit {
                let reason = if open {
                    "configured tab limit reached"
                } else {
                    "workspace is closing"
                };
                despawn_tab(world, tab);
                return abandon(world, entity, pid, &requesters, reason);
            }
            place_first_pane(world, tab, entity, pid);
            let tab_id = world.get::<Tab>(tab).map(|tab| tab.id).unwrap_or_default();
            let label = world
                .get::<Tab>(tab)
                .map(|tab| tab.label.clone())
                .unwrap_or_default();
            world.entity_mut(tab).insert(TabOf(workspace));
            if let Some(mut component) = world.get_mut::<Workspace>(workspace) {
                component.selection.tab = Some(tab);
            }
            focus_requesters(world, &requesters, workspace, tab, entity);
            event(
                world,
                workspace,
                Event::TabOpened {
                    id: 0,
                    tab: tab_id,
                    name: label,
                },
            );
            announce_pane(world, workspace, tab, entity, id);
            mark_workspace_dirty(world, workspace);
            reply_all(world, requesters, |request| Reply::Completed {
                id: request,
                result: CommandResult::Tab { tab: tab_id },
            });
        }
        CreationKind::Workspace { .. } => {
            if world
                .get::<Workspace>(workspace)
                .is_none_or(|workspace| workspace.retiring.is_some())
            {
                return abandon(
                    world,
                    entity,
                    pid,
                    &requesters,
                    "workspace was killed before it opened",
                );
            }
            place_first_pane(world, tab, entity, pid);
            world.entity_mut(tab).insert(TabOf(workspace));
            let name = world
                .get_mut::<Workspace>(workspace)
                .map(|mut component| {
                    component.selection.select(tab, Some(entity));
                    component.open = true;
                    component.name.clone()
                })
                .unwrap_or_default();
            effect(world, Effect::WorkspaceOpened { name: name.clone() });
            announce_pane(world, workspace, tab, entity, id);
            // A manager requester receives the attach outcome through the reply routing.
            for (requester, request_id) in requesters {
                if let Requester::Viewer(viewer) = requester
                    && let Some(viewer) = viewer_entity(world, viewer)
                {
                    switch_viewer_workspace(world, viewer, workspace);
                }
                reply(
                    world,
                    requester,
                    Reply::Completed {
                        id: request_id,
                        result: CommandResult::Workspace { name: name.clone() },
                    },
                );
            }
        }
    }
}

/// A new tab's or workspace's first pane becomes its whole layout.
fn place_first_pane(world: &mut World, tab: Entity, pane: Entity, pid: u32) {
    if let Some(mut component) = world.get_mut::<Tab>(tab) {
        component.layout = LayoutTree::new(pane);
        component.layout_changed = true;
    }
    go_live(world, pane, pid);
}

fn go_live(world: &mut World, entity: Entity, pid: u32) {
    if let Some(mut pane) = world.get_mut::<Pane>(entity) {
        // Exit reports may already have arrived for a short-lived process.
        if matches!(pane.state, PaneState::Starting) {
            pane.state = PaneState::Live { pid };
        }
        pane.dirty = true;
    }
}

/// The requesting viewers (and the workspace default) show `tab` with `pane` focused.
fn focus_requesters(
    world: &mut World,
    requesters: &[(Requester, RequestId)],
    workspace: Entity,
    tab: Entity,
    pane: Entity,
) {
    for (requester, _) in requesters {
        if let Requester::Viewer(viewer) = requester
            && let Some(viewer) = viewer_entity(world, *viewer)
            && let Some(mut viewer) = world.get_mut::<Viewer>(viewer)
            && viewer.workspace == workspace
        {
            viewer.selection.select(tab, Some(pane));
            viewer.dirty = true;
        }
    }
    if let Some(mut component) = world.get_mut::<Workspace>(workspace) {
        component.selection.set_focus(tab, pane);
    }
    mark_tab_dirty(world, tab);
}

fn announce_pane(world: &mut World, workspace: Entity, tab: Entity, pane: Entity, id: PaneId) {
    let command = world
        .get::<Pane>(pane)
        .map(|pane| pane.argv.clone())
        .unwrap_or_default();
    let tab_id: TabId = world.get::<Tab>(tab).map(|tab| tab.id).unwrap_or_default();
    event(
        world,
        workspace,
        Event::PaneOpened {
            id: 0,
            pane: id,
            tab: tab_id,
            command,
        },
    );
}

/// A process started but its destination disappeared: terminate it and fail the request.
fn abandon(
    world: &mut World,
    entity: Entity,
    pid: u32,
    requesters: &[(Requester, RequestId)],
    reason: &str,
) {
    if let Some(mut pane) = world.get_mut::<Pane>(entity)
        && matches!(pane.state, PaneState::Starting)
    {
        pane.state = PaneState::Live { pid };
    }
    let now = world.resource::<Clock>().now_ms;
    terminate_pane(world, entity, now, TERMINATE_GRACE_MS);
    reply_all(world, requesters.iter().copied(), |id| {
        failed(id, ErrorCode::Conflict, reason)
    });
}

fn roll_back(world: &mut World, entity: Entity, creation: Creation, message: &str) {
    match creation.kind {
        CreationKind::Split { .. } => {}
        CreationKind::NewTab { tab } => despawn_tab(world, tab),
        CreationKind::Workspace { tab } => {
            if let Some(workspace) = tab_workspace(world, tab) {
                despawn_workspace(world, workspace);
            }
            despawn_tab(world, tab);
        }
    }
    despawn_pane(world, entity);
    reply_all(world, creation.requesters, |id| {
        failed(
            id,
            ErrorCode::Internal,
            format!("could not start the pane: {message}"),
        )
    });
}

/// Adds another waiting party to a pending workspace creation.
pub fn join_pending_workspace(
    world: &mut World,
    workspace: Entity,
    requester: Requester,
    request_id: RequestId,
) -> bool {
    let panes: Vec<Entity> = world
        .query::<(Entity, &Creation)>()
        .iter(world)
        .filter(|(_, creation)| matches!(creation.kind, CreationKind::Workspace { tab } if world.get::<Tab>(tab).is_some_and(|tab| tab.workspace == workspace)))
        .map(|(entity, _)| entity)
        .collect();
    let Some(pane) = panes.first().copied() else {
        return false;
    };
    if let Some(mut creation) = world.get_mut::<Creation>(pane) {
        creation.requesters.push((requester, request_id));
        return true;
    }
    false
}

/// Whether a workspace is still waiting for its initial pane.
pub fn workspace_pending(world: &World, workspace: Entity) -> bool {
    world
        .get::<Workspace>(workspace)
        .is_some_and(|workspace| !workspace.open && workspace.retiring.is_none())
}

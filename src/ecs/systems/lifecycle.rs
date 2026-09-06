//! Lifecycle phase: natural exits, confirmed closes, tab and workspace retirement, shutdown.
//! Ownership cascades are explicit despawns; the adapter releases OS handles per `ReleasePane`.

use crate::ecs::components::{Pane, PaneState, Tab, Tabs, Viewer, Workspace};
use crate::ecs::messages::Effect;
use crate::ecs::resources::{Clock, Deadlines, Ids, Limits, ShuttingDown};
use crate::ecs::support::{
    close_tab, despawn_pane, effect, event, mark_workspace_dirty, member_tabs, pane_in_layout,
    remove_from_layout, terminate_pane, viewers_of_workspace,
};
use crate::ecs::systems::requests::kill_workspace;
use crate::proto::control::Event;
use bevy_ecs::prelude::*;

/// SIGHUP is followed by SIGKILL after this many milliseconds.
pub const TERMINATE_GRACE_MS: u64 = 1_000;

pub fn resolve_lifecycle(world: &mut World) {
    let now = world.resource::<Clock>().now_ms;
    let limits = world.resource::<Limits>().clone();
    if world.resource::<ShuttingDown>().0 {
        let workspaces: Vec<Entity> = world
            .query::<(Entity, &Workspace)>()
            .iter(world)
            .filter(|(_, workspace)| workspace.retiring.is_none())
            .map(|(entity, _)| entity)
            .collect();
        for workspace in workspaces {
            kill_workspace(world, workspace);
        }
    }
    handle_exited_panes(world, now);
    drop_overdue_terminations(world, now, limits.terminate_deadline_ms);
    retire_empty_workspaces(world, now);
    finalize_retirements(world, now, limits.retire_grace_ms);
}

fn handle_exited_panes(world: &mut World, now: u64) {
    let exited: Vec<(Entity, u32)> = world
        .query::<(Entity, &Pane)>()
        .iter(world)
        .filter_map(|(entity, pane)| match pane.state {
            PaneState::Exited { code } => Some((entity, code)),
            _ => None,
        })
        .collect();
    for (pane, code) in exited {
        if world
            .get::<crate::ecs::components::Creation>(pane)
            .is_some()
        {
            // Exited before its completion was applied; the completion phase places it and the
            // next pass closes it with this status.
            continue;
        }
        let Some((id, tab)) = world.get::<Pane>(pane).map(|pane| (pane.id, pane.tab)) else {
            continue;
        };
        let Some(workspace) = crate::ecs::support::tab_workspace(world, tab) else {
            // The tab is already gone (closed or rolled back); just release the pane.
            despawn_pane(world, pane);
            continue;
        };
        if pane_in_layout(world, pane) {
            let sole_pane = world
                .get::<Tab>(tab)
                .is_some_and(|tab| tab.layout.len() == 1);
            let sole_tab = member_tabs(world, workspace).len() == 1;
            if sole_pane && sole_tab {
                // Natural exit of the last pane: keep its final screen visible and retire the
                // workspace with the process status once viewers have seen it.
                let already = world
                    .get::<Workspace>(workspace)
                    .is_some_and(|workspace| workspace.retiring.is_some());
                if !already {
                    if let Some(mut component) = world.get_mut::<Workspace>(workspace) {
                        component.retiring = Some(crate::ecs::components::Retiring {
                            since_ms: now,
                            exit_code: Some(code),
                        });
                    }
                    event(
                        world,
                        workspace,
                        Event::PaneClosed {
                            id: 0,
                            pane: id,
                            exit_status: Some(i32::try_from(code).unwrap_or(i32::MAX)),
                        },
                    );
                    mark_workspace_dirty(world, workspace);
                }
                continue;
            }
            remove_from_layout(world, pane);
            event(
                world,
                workspace,
                Event::PaneClosed {
                    id: 0,
                    pane: id,
                    exit_status: Some(i32::try_from(code).unwrap_or(i32::MAX)),
                },
            );
            despawn_pane(world, pane);
            if world
                .get::<Tab>(tab)
                .is_some_and(|tab| tab.layout.is_empty())
            {
                close_tab(world, tab, now, TERMINATE_GRACE_MS);
            }
        } else {
            // Killed, or its tab closed: the exit report finishes the cleanup.
            event(
                world,
                workspace,
                Event::PaneClosed {
                    id: 0,
                    pane: id,
                    exit_status: Some(i32::try_from(code).unwrap_or(i32::MAX)),
                },
            );
            despawn_pane(world, pane);
        }
    }
}

fn drop_overdue_terminations(world: &mut World, now: u64, deadline_ms: u64) {
    let mut overdue = Vec::new();
    let mut next_deadline: Option<u64> = None;
    for (entity, pane) in world.query::<(Entity, &Pane)>().iter(world) {
        if let PaneState::Terminating { since_ms, .. } = pane.state {
            let due = since_ms.saturating_add(deadline_ms);
            if now >= due {
                overdue.push(entity);
            } else {
                next_deadline = Some(next_deadline.map_or(due, |current| current.min(due)));
            }
        }
    }
    if let Some(due) = next_deadline {
        world.resource_mut::<Deadlines>().propose(due);
    }
    for pane in overdue {
        if pane_in_layout(world, pane) {
            remove_from_layout(world, pane);
        }
        if let Some(workspace) = crate::ecs::support::pane_workspace(world, pane)
            && let Some(id) = crate::ecs::support::pane_id(world, pane)
        {
            event(
                world,
                workspace,
                Event::PaneClosed {
                    id: 0,
                    pane: id,
                    exit_status: None,
                },
            );
        }
        despawn_pane(world, pane);
    }
}

/// A workspace whose last tab closed retires with code 0; viewers exit cleanly.
fn retire_empty_workspaces(world: &mut World, now: u64) {
    let empty: Vec<Entity> = world
        .query::<(Entity, &Workspace)>()
        .iter(world)
        .filter(|(entity, workspace)| {
            workspace.open && workspace.retiring.is_none() && world.get::<Tabs>(*entity).is_none()
        })
        .map(|(entity, _)| entity)
        .collect();
    for workspace in empty {
        if let Some(mut component) = world.get_mut::<Workspace>(workspace) {
            component.retiring = Some(crate::ecs::components::Retiring {
                since_ms: now,
                exit_code: Some(0),
            });
        }
        mark_workspace_dirty(world, workspace);
    }
}

fn finalize_retirements(world: &mut World, now: u64, grace_ms: u64) {
    let retiring: Vec<(Entity, u64)> = world
        .query::<(Entity, &Workspace)>()
        .iter(world)
        .filter_map(|(entity, workspace)| {
            workspace
                .retiring
                .map(|retiring| (entity, retiring.since_ms))
        })
        .collect();
    for (workspace, since) in retiring {
        let viewers = viewers_of_workspace(world, workspace);
        // Viewers still attached are waiting to paint the final frame; the snapshot phase marks
        // them detaching after publishing it.
        let waiting = world
            .query::<&Viewer>()
            .iter(world)
            .any(|viewer| viewer.workspace == workspace && !viewer.detaching);
        if (waiting || !viewers.is_empty()) && now.saturating_sub(since) < grace_ms {
            world
                .resource_mut::<Deadlines>()
                .propose(since.saturating_add(grace_ms));
            continue;
        }
        finalize(world, workspace, now);
    }
    let remaining = world.query::<&Workspace>().iter(world).count();
    if remaining == 0 && world.resource::<Ids>().workspaces.is_empty() {
        effect(world, Effect::Idle);
    }
}

fn finalize(world: &mut World, workspace: Entity, now: u64) {
    let Some(name) = world
        .get::<Workspace>(workspace)
        .map(|workspace| workspace.name.clone())
    else {
        return;
    };
    let viewers: Vec<Entity> = world
        .query::<(Entity, &Viewer)>()
        .iter(world)
        .filter(|(_, viewer)| viewer.workspace == workspace)
        .map(|(entity, _)| entity)
        .collect();
    for viewer in viewers {
        crate::ecs::systems::requests::despawn_viewer(world, viewer);
    }
    // Creations still in flight answer their requesters; the late process is stopped by the
    // completion phase when it reports in.
    crate::ecs::systems::creation::fail_pending_creations(
        world,
        workspace,
        "workspace closed before the pane started",
    );
    // Panes reserved for tabs that never joined the workspace, or still terminating, are
    // released too: the adapter kills and reaps anything still running.
    let panes: Vec<Entity> = world
        .query::<(Entity, &Pane)>()
        .iter(world)
        .filter(|(_, pane)| {
            world
                .get::<Tab>(pane.tab)
                .is_some_and(|tab| tab.workspace == workspace)
        })
        .map(|(entity, _)| entity)
        .collect();
    for pane in panes {
        terminate_pane(world, pane, now, TERMINATE_GRACE_MS);
        despawn_pane(world, pane);
    }
    let all_tabs: Vec<Entity> = world
        .query::<(Entity, &Tab)>()
        .iter(world)
        .filter(|(_, tab)| tab.workspace == workspace)
        .map(|(entity, _)| entity)
        .collect();
    for tab in all_tabs {
        if let Some(id) = world.get::<Tab>(tab).map(|tab| tab.id) {
            world.resource_mut::<Ids>().tabs.remove(&id);
            world.despawn(tab);
        }
    }
    world.resource_mut::<Ids>().workspaces.remove(&name);
    world.despawn(workspace);
    effect(world, Effect::WorkspaceClosed { name });
}

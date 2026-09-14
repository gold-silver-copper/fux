//! Control-request action handlers dispatched by `requests::apply_control`: splits, focus,
//! kill, resize, tab and workspace actions, and viewer admission. Behavior-identical to the
//! handlers previously inlined in `requests.rs`; extracted to keep the request module focused.
#![allow(clippy::too_many_arguments)]

use super::requests::{
    Context, Outcome, kill_workspace, list_workspaces, next_workspace_name, switch_viewer_workspace,
};
use crate::ecs::components::{CreationKind, Pane, PaneState, Tab, Viewer, Workspace};
use crate::ecs::resources::{Clock, Limits, ServerIdentity};
use crate::ecs::support::{
    close_tab, despawn_tab, failed, focus_in_tab, is_member, mark_workspace_dirty, member_tabs,
    pane_entity, pane_id, pane_in_layout, pane_tab, pane_workspace, remove_from_layout, tab_entity,
    tab_id, terminate_pane, workspace_entity,
};
use crate::ecs::systems::creation::{NewPane, reserve_pane, reserve_tab, reserve_workspace};
use crate::ecs::systems::lifecycle::TERMINATE_GRACE_MS;
use crate::ids::PaneId;
use crate::layout::{Axis, Direction};
use crate::proto::control::{
    self, CommandResult, ErrorCode, FocusTarget, Reply, TabAction, WorkspaceAction,
};
use bevy_ecs::prelude::*;

pub(super) fn set_right_click(
    world: &mut World,
    context: &Context,
    id: u64,
    pane: PaneId,
    right_click: crate::view::RightClickPolicy,
) -> Result<CommandResult, Reply> {
    let entity = pane_in_workspace(world, context, pane)
        .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane no longer exists"))?;
    let component = world
        .get::<Pane>(entity)
        .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane no longer exists"))?;
    if component.right_click == right_click {
        Ok(CommandResult::Pane { pane })
    } else {
        let tab = component.tab;
        let revision = world
            .get::<Tab>(tab)
            .and_then(|tab| tab.layout_generation.checked_add(1))
            .ok_or_else(|| failed(id, ErrorCode::Limit, "layout generation exhausted"))?;
        if let Some(mut component) = world.get_mut::<Pane>(entity) {
            component.right_click = right_click;
        }
        if let Some(mut component) = world.get_mut::<Tab>(tab) {
            component.layout_generation = revision;
        }
        mark_workspace_dirty(world, context.workspace);
        Ok(CommandResult::Pane { pane })
    }
}

/// A pane id that belongs to the requester's workspace.
pub(super) fn pane_in_workspace(world: &World, context: &Context, pane: PaneId) -> Option<Entity> {
    let entity = pane_entity(world, pane)?;
    (pane_workspace(world, entity)? == context.workspace).then_some(entity)
}

pub(super) fn tab_in_workspace(
    world: &World,
    context: &Context,
    tab: crate::ids::TabId,
) -> Option<Entity> {
    let entity = tab_entity(world, tab)?;
    is_member(world, context.workspace, entity).then_some(entity)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn split(
    world: &mut World,
    context: &Context,
    id: u64,
    axis: Axis,
    target: Option<PaneId>,
    cwd: Option<std::path::PathBuf>,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    rows: Option<u16>,
    columns: Option<u16>,
    final_retain_ms: u64,
    fixed_workspace: bool,
    right_click: crate::view::RightClickPolicy,
    ratio: u16,
    focus: bool,
) -> Result<Outcome, Reply> {
    let ratio = std::num::NonZeroU16::new(ratio)
        .filter(|ratio| {
            (crate::layout::MIN_RATIO..=crate::layout::MAX_RATIO).contains(&ratio.get())
        })
        .ok_or_else(|| {
            failed(
                id,
                ErrorCode::InvalidRequest,
                "split ratio must be 500..=9500",
            )
        })?;
    let selection = context.selection(world);
    let target = match target {
        Some(pane) => pane_in_workspace(world, context, pane)
            .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?,
        None => selection
            .tab
            .and_then(|tab| focus_in_tab(world, &selection, tab))
            .filter(|pane| pane_in_layout(world, *pane))
            .ok_or_else(|| failed(id, ErrorCode::NotFound, "no focused pane to split"))?,
    };
    let tab =
        pane_tab(world, target).ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
    let base = world
        .get::<Pane>(target)
        .map_or((24, 80), |pane| pane.terminal.size());
    // Estimate the new (second) pane's initial extent using the requested ratio.
    let second = |extent: u16| {
        let usable = extent.saturating_sub(1);
        let first =
            u32::from(usable) * u32::from(ratio.get()) / u32::from(crate::layout::RATIO_SCALE);
        usable.saturating_sub(u16::try_from(first).unwrap_or(usable))
    };
    let initial = match axis {
        Axis::Horizontal => (base.0, second(base.1)),
        Axis::Vertical => (second(base.0), base.1),
    };
    // A requested size wins where no viewer resizes the tab (a headless workspace).
    let size = (rows.unwrap_or(initial.0), columns.unwrap_or(initial.1));
    reserve_pane(
        world,
        context.workspace,
        NewPane {
            argv,
            cwd,
            env,
            requester: context.requester,
            request_id: id,
            final_retain_ms,
            fixed_workspace,
            right_click,
        },
        CreationKind::Split {
            tab,
            target,
            axis,
            ratio,
            focus,
        },
        size,
    )?;
    // The reply follows the spawn report.
    Ok(Outcome::Deferred)
}

pub(super) fn focus(
    world: &mut World,
    context: &Context,
    id: u64,
    target: FocusTarget,
) -> Result<CommandResult, Reply> {
    crate::ecs::support::refresh_focus_history(world, context.workspace);
    let selection = context.selection(world);
    match target {
        FocusTarget::Last => {
            let entity = selection
                .history
                .previous
                .filter(|entity| Some(*entity) != selection.history.current)
                .filter(|entity| pane_in_layout(world, *entity))
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no previous pane"))?;
            let workspace = pane_workspace(world, entity)
                .filter(|workspace| *workspace == context.workspace || context.viewer.is_some())
                .filter(|workspace| {
                    world
                        .get::<Workspace>(*workspace)
                        .is_some_and(|workspace| workspace.open && workspace.retiring.is_none())
                })
                .ok_or_else(|| {
                    failed(
                        id,
                        ErrorCode::NotFound,
                        "previous pane is unavailable in this workspace",
                    )
                })?;
            let tab = pane_tab(world, entity)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "previous pane is unavailable"))?;
            let pane = pane_id(world, entity)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "previous pane is unavailable"))?;
            if let Some(viewer) = context.viewer {
                check_viewer_admission(world, viewer, workspace, id)?;
                switch_viewer_workspace(world, viewer, workspace);
            }
            Context {
                requester: context.requester,
                workspace,
                viewer: context.viewer,
            }
            .select(world, tab, Some(entity));
            Ok(CommandResult::Pane { pane })
        }
        FocusTarget::Pane(pane) => {
            let entity = pane_in_workspace(world, context, pane)
                .filter(|pane| pane_in_layout(world, *pane))
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
            let tab = pane_tab(world, entity)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
            context.select(world, tab, Some(entity));
            Ok(CommandResult::Pane { pane })
        }
        FocusTarget::Next | FocusTarget::Previous => {
            let tab = selection
                .tab
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no active tab"))?;
            let current = focus_in_tab(world, &selection, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no focused pane"))?;
            let next = world
                .get::<Tab>(tab)
                .and_then(|component| component.layout.cycle(current, target == FocusTarget::Next))
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no next pane"))?;
            context.select(world, tab, Some(next));
            Ok(CommandResult::Pane {
                pane: pane_id(world, next).unwrap_or_default(),
            })
        }
        directional => {
            let direction = match directional {
                FocusTarget::Left => Direction::Left,
                FocusTarget::Right => Direction::Right,
                FocusTarget::Up => Direction::Up,
                FocusTarget::Down
                | FocusTarget::Pane(_)
                | FocusTarget::Next
                | FocusTarget::Previous
                | FocusTarget::Last => Direction::Down,
            };
            let tab = selection
                .tab
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no active tab"))?;
            let current = focus_in_tab(world, &selection, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no focused pane"))?;
            let next = world
                .get::<Tab>(tab)
                .and_then(|component| {
                    let area = component.area.navigation_area();
                    component.layout.neighbour(current, direction, area)
                })
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no pane in that direction"))?;
            context.select(world, tab, Some(next));
            let pane = pane_id(world, next).unwrap_or_default();
            Ok(CommandResult::Pane { pane })
        }
    }
}

pub(super) fn kill(
    world: &mut World,
    context: &Context,
    id: u64,
    pane: PaneId,
) -> Result<CommandResult, Reply> {
    let entity = pane_in_workspace(world, context, pane)
        .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
    if world
        .get::<Pane>(entity)
        .is_some_and(|pane| matches!(pane.state, PaneState::Starting))
    {
        return Err(failed(id, ErrorCode::Conflict, "pane is still starting"));
    }
    let now = world.resource::<Clock>().now_ms;
    let tab = pane_tab(world, entity);
    remove_from_layout(world, entity);
    // An already exited pane has nothing to wait for: the lifecycle phase publishes pane.closed
    // and despawns it.
    terminate_pane(world, entity, now, TERMINATE_GRACE_MS);
    if let Some(tab) = tab
        && world
            .get::<Tab>(tab)
            .is_some_and(|tab| tab.layout.is_empty())
    {
        close_tab(world, tab, now, TERMINATE_GRACE_MS);
    }
    Ok(CommandResult::Unit)
}

pub(super) fn resize(
    world: &mut World,
    context: &Context,
    id: u64,
    pane: PaneId,
    delta: i16,
) -> Result<CommandResult, Reply> {
    let entity = pane_in_workspace(world, context, pane)
        .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
    let tab =
        pane_tab(world, entity).ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
    let resized = world.get_mut::<Tab>(tab).is_some_and(|mut component| {
        let ok = component.layout.resize(entity, delta).is_ok();
        component.layout_changed |= ok;
        if ok {
            component.layout_generation = component.layout_generation.saturating_add(1);
        }
        ok
    });
    if resized {
        Ok(CommandResult::Unit)
    } else {
        Err(failed(id, ErrorCode::Conflict, "pane cannot be resized"))
    }
}

pub(super) fn tab_action(
    world: &mut World,
    context: &Context,
    id: u64,
    action: TabAction,
) -> Result<Outcome, Reply> {
    let tabs = member_tabs(world, context.workspace);
    let selection = context.selection(world);
    match action {
        TabAction::Reorder { tab, before } => {
            let entity = tab_in_workspace(world, context, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab not found"))?;
            let before = before
                .map(|tab| {
                    tab_in_workspace(world, context, tab)
                        .ok_or_else(|| failed(id, ErrorCode::NotFound, "reference tab not found"))
                })
                .transpose()?;
            if !world
                .get_mut::<crate::ecs::components::Tabs>(context.workspace)
                .is_some_and(|mut tabs| tabs.place_before(entity, before))
            {
                return Err(failed(id, ErrorCode::Conflict, "tab order changed"));
            }
            mark_workspace_dirty(world, context.workspace);
            Ok(Outcome::Now(CommandResult::Tab { tab }))
        }
        TabAction::New { name } => {
            let limit = world.resource::<Limits>().max_tabs;
            if tabs.len() >= limit {
                return Err(failed(id, ErrorCode::Limit, "configured tab limit reached"));
            }
            let tab = reserve_tab(world, context.workspace, name)?;
            let size = selection
                .focused()
                .and_then(|pane| world.get::<Pane>(pane))
                .map_or((24, 80), |pane| pane.terminal.size());
            if let Err(reply) = reserve_pane(
                world,
                context.workspace,
                NewPane {
                    argv: Vec::new(),
                    cwd: None,
                    env: Vec::new(),
                    requester: context.requester,
                    request_id: id,
                    final_retain_ms: world.resource::<Limits>().final_retain_ms,
                    fixed_workspace: false,
                    right_click: Default::default(),
                },
                CreationKind::NewTab { tab },
                size,
            ) {
                despawn_tab(world, tab);
                return Err(reply);
            }
            Ok(Outcome::Deferred)
        }
        TabAction::Next | TabAction::Previous => {
            if tabs.is_empty() {
                return Err(failed(id, ErrorCode::NotFound, "no tabs"));
            }
            let current = selection
                .tab
                .and_then(|tab| tabs.iter().position(|entry| *entry == tab))
                .unwrap_or(0);
            let index = if matches!(action, TabAction::Next) {
                (current + 1) % tabs.len()
            } else {
                current.checked_sub(1).unwrap_or(tabs.len() - 1)
            };
            let tab = *tabs
                .get(index)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab not found"))?;
            context.select_tab(world, tab);
            Ok(Outcome::Now(CommandResult::Tab {
                tab: tab_id(world, tab).unwrap_or_default(),
            }))
        }
        TabAction::Select { target } => {
            let entity = match target {
                control::TabTarget::Index(index) => *usize::try_from(index)
                    .ok()
                    .and_then(|index| tabs.get(index))
                    .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab not found"))?,
                control::TabTarget::Id(tab) => tab_in_workspace(world, context, tab)
                    .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab no longer exists"))?,
            };
            context.select_tab(world, entity);
            Ok(Outcome::Now(CommandResult::Tab {
                tab: tab_id(world, entity).unwrap_or_default(),
            }))
        }
        TabAction::Rename { tab, name } => {
            let entity = tab_in_workspace(world, context, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab no longer exists"))?;
            if let Some(mut component) = world.get_mut::<Tab>(entity) {
                component.label = name;
            }
            mark_workspace_dirty(world, context.workspace);
            Ok(Outcome::Now(CommandResult::Tab { tab }))
        }
        TabAction::Close { tab } => {
            let entity = tab_in_workspace(world, context, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab no longer exists"))?;
            let now = world.resource::<Clock>().now_ms;
            close_tab(world, entity, now, TERMINATE_GRACE_MS);
            Ok(Outcome::Now(CommandResult::Tab { tab }))
        }
    }
}

pub(super) fn workspace_action(
    world: &mut World,
    context: &Context,
    id: u64,
    action: WorkspaceAction,
) -> Result<Outcome, Reply> {
    match action {
        WorkspaceAction::Rename { label } => {
            let label = (!label.is_empty()).then_some(label);
            let mut workspace = world
                .get_mut::<Workspace>(context.workspace)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "workspace does not exist"))?;
            let name = workspace.name.clone();
            let changed = workspace.label != label;
            workspace.label = label;
            if changed {
                mark_workspace_dirty(world, context.workspace);
            }
            Ok(Outcome::Now(CommandResult::Workspace { name }))
        }
        WorkspaceAction::List => Ok(Outcome::Now(CommandResult::Listing {
            instance: world.resource::<ServerIdentity>().instance_nonce.clone(),
            workspaces: list_workspaces(world),
        })),
        WorkspaceAction::New { name } => {
            let name = match name {
                Some(name) => name,
                None => next_workspace_name(world),
            };
            reserve_workspace(world, name, context.requester, id)?;
            Ok(Outcome::Deferred)
        }
        WorkspaceAction::Kill { name } => {
            let entity = workspace_entity(world, &name)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "workspace does not exist"))?;
            if entity != context.workspace {
                return Err(failed(
                    id,
                    ErrorCode::Unauthorized,
                    "a workspace connection may only kill its own workspace; use `fux workspace kill`",
                ));
            }
            kill_workspace(world, entity);
            Ok(Outcome::Now(CommandResult::Workspace { name }))
        }
        WorkspaceAction::Select { name } => {
            let viewer = context.viewer.ok_or_else(|| {
                failed(
                    id,
                    ErrorCode::InvalidRequest,
                    "only attached viewers switch workspaces",
                )
            })?;
            let entity = workspace_entity(world, &name)
                .filter(|entity| {
                    world
                        .get::<Workspace>(*entity)
                        .is_some_and(|workspace| workspace.open && workspace.retiring.is_none())
                })
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "workspace does not exist"))?;
            check_viewer_admission(world, viewer, entity, id)?;
            switch_viewer_workspace(world, viewer, entity);
            crate::ecs::support::refresh_focus_history(world, entity);
            Ok(Outcome::Now(CommandResult::Workspace { name }))
        }
    }
}

pub(super) fn check_viewer_admission(
    world: &mut World,
    viewer: Entity,
    workspace: Entity,
    id: u64,
) -> Result<(), Reply> {
    if world
        .get::<Viewer>(viewer)
        .is_some_and(|v| v.workspace == workspace)
    {
        return Ok(());
    }
    let limit = world.resource::<Limits>().max_viewers;
    let occupied = world
        .query::<&Viewer>()
        .iter(world)
        .filter(|other| other.attached_to(workspace))
        .count();
    if occupied >= limit {
        return Err(failed(
            id,
            ErrorCode::Limit,
            "that workspace already has the maximum number of viewers",
        ));
    }
    Ok(())
}

//! Control-request action handlers dispatched by `requests::apply_control`: splits, focus,
//! kill, resize, tab and workspace actions, and viewer admission. Handlers return a [`Failure`]
//! without a request id; `apply_control` attaches the id once when it replies.

use super::requests::{
    Context, Outcome, kill_workspace, list_workspaces, next_workspace_name, switch_viewer_workspace,
};
use crate::ecs::components::{CreationKind, Pane, PaneState, Tab, Viewer, Workspace};
use crate::ecs::resources::{Clock, Limits, ServerIdentity};
use crate::ecs::support::{
    Failure, attached_viewers, close_tab, despawn_tab, focus_in_tab, is_accepting,
    mark_workspace_dirty, member_tabs, pane_id, pane_in_layout, pane_tab, pane_workspace,
    remove_from_layout, tab_id, terminate_pane, workspace_entity,
};
use crate::ecs::systems::creation::{NewPane, reserve_pane, reserve_tab, reserve_workspace};
use crate::ecs::systems::lifecycle::TERMINATE_GRACE_MS;
use crate::ids::PaneId;
use crate::layout::{Axis, Direction};
use crate::proto::control::{
    self, CommandResult, ErrorCode, FocusTarget, Request, TabAction, WorkspaceAction,
};
use bevy_ecs::prelude::*;

/// Sets one pane field, bumping the tab's layout generation and republishing the workspace
/// when the value actually changes.
pub(super) fn update_pane_field<T: PartialEq>(
    world: &mut World,
    context: &Context,
    pane: PaneId,
    value: T,
    field: impl Fn(&mut Pane) -> &mut T,
) -> Result<CommandResult, Failure> {
    let (entity, component) = context.pane(world, pane)?;
    let tab = component.tab;
    let revision = world
        .get::<Tab>(tab)
        .and_then(|tab| tab.layout_generation.checked_add(1))
        .ok_or_else(|| Failure::limit("layout generation exhausted"))?;
    let changed = world.get_mut::<Pane>(entity).is_some_and(|mut component| {
        let slot = field(&mut component);
        if *slot == value {
            return false;
        }
        *slot = value;
        true
    });
    if changed {
        if let Some(mut component) = world.get_mut::<Tab>(tab) {
            component.layout_generation = revision;
        }
        mark_workspace_dirty(world, context.workspace);
    }
    Ok(CommandResult::Pane { pane })
}

/// A pane id that belongs to the requester's workspace.
pub(super) fn pane_in_workspace(world: &World, context: &Context, pane: PaneId) -> Option<Entity> {
    let entity = crate::ecs::support::pane_entity(world, pane)?;
    (pane_workspace(world, entity)? == context.workspace).then_some(entity)
}

/// What `split` needs from a `Request::Split`, destructured once by the dispatcher.
pub(super) struct SplitSpec {
    pub axis: Axis,
    pub target: Option<PaneId>,
    pub cwd: Option<std::path::PathBuf>,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub rows: Option<u16>,
    pub columns: Option<u16>,
    pub final_retain_ms: u64,
    pub fixed_workspace: bool,
    pub right_click: crate::view::RightClickPolicy,
    pub ratio: u16,
    pub focus: bool,
}

impl SplitSpec {
    pub(super) fn from_request(request: &Request) -> Option<Self> {
        let Request::Split {
            axis,
            target,
            cwd,
            argv,
            env,
            rows,
            columns,
            final_retain_ms,
            fixed_workspace,
            right_click,
            ratio,
            focus,
            ..
        } = request
        else {
            return None;
        };
        Some(Self {
            axis: *axis,
            target: *target,
            cwd: cwd.clone(),
            argv: argv.clone(),
            env: env.clone(),
            rows: *rows,
            columns: *columns,
            final_retain_ms: *final_retain_ms,
            fixed_workspace: *fixed_workspace,
            right_click: *right_click,
            ratio: *ratio,
            focus: *focus,
        })
    }
}

pub(super) fn split(
    world: &mut World,
    context: &Context,
    spec: SplitSpec,
) -> Result<Outcome, Failure> {
    let ratio = std::num::NonZeroU16::new(spec.ratio)
        .filter(|ratio| {
            (crate::layout::MIN_RATIO..=crate::layout::MAX_RATIO).contains(&ratio.get())
        })
        .ok_or_else(|| Failure::invalid("split ratio must be 500..=9500"))?;
    let selection = context.selection(world);
    let target = match spec.target {
        Some(pane) => context.pane(world, pane)?.0,
        None => selection
            .tab
            .and_then(|tab| focus_in_tab(world, &selection, tab))
            .filter(|pane| pane_in_layout(world, *pane))
            .ok_or_else(|| Failure::not_found("no focused pane to split"))?,
    };
    let tab = pane_tab(world, target).ok_or_else(|| Failure::not_found("pane not found"))?;
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
    let initial = match spec.axis {
        Axis::Horizontal => (base.0, second(base.1)),
        Axis::Vertical => (second(base.0), base.1),
    };
    // A requested size wins where no viewer resizes the tab (a headless workspace).
    let size = (
        spec.rows.unwrap_or(initial.0),
        spec.columns.unwrap_or(initial.1),
    );
    reserve_pane(
        world,
        context.workspace,
        NewPane {
            argv: spec.argv,
            cwd: spec.cwd,
            env: spec.env,
            requester: context.requester,
            request_id: context.id,
            final_retain_ms: spec.final_retain_ms,
            fixed_workspace: spec.fixed_workspace,
            right_click: spec.right_click,
        },
        CreationKind::Split {
            tab,
            target,
            axis: spec.axis,
            ratio,
            focus: spec.focus,
        },
        size,
    )?;
    // The reply follows the spawn report.
    Ok(Outcome::Deferred)
}

pub(super) fn focus(
    world: &mut World,
    context: &Context,
    target: FocusTarget,
) -> Result<CommandResult, Failure> {
    crate::ecs::support::refresh_focus_history(world, context.workspace);
    let selection = context.selection(world);
    match target {
        FocusTarget::Last => {
            let entity = selection
                .history
                .previous
                .filter(|entity| Some(*entity) != selection.history.current)
                .filter(|entity| pane_in_layout(world, *entity))
                .ok_or_else(|| Failure::not_found("no previous pane"))?;
            let workspace = pane_workspace(world, entity)
                .filter(|workspace| *workspace == context.workspace || context.viewer.is_some())
                .filter(|workspace| is_accepting(world, *workspace))
                .ok_or_else(|| {
                    Failure::not_found("previous pane is unavailable in this workspace")
                })?;
            let tab = pane_tab(world, entity)
                .ok_or_else(|| Failure::not_found("previous pane is unavailable"))?;
            let pane = pane_id(world, entity)
                .ok_or_else(|| Failure::not_found("previous pane is unavailable"))?;
            if let Some(viewer) = context.viewer {
                check_viewer_admission(world, viewer, workspace)?;
                switch_viewer_workspace(world, viewer, workspace);
            }
            Context {
                requester: context.requester,
                id: context.id,
                workspace,
                viewer: context.viewer,
            }
            .select(world, tab, Some(entity));
            Ok(CommandResult::Pane { pane })
        }
        FocusTarget::Pane(pane) => {
            let entity = pane_in_workspace(world, context, pane)
                .filter(|pane| pane_in_layout(world, *pane))
                .ok_or_else(|| Failure::not_found("pane not found"))?;
            let tab =
                pane_tab(world, entity).ok_or_else(|| Failure::not_found("pane not found"))?;
            context.select(world, tab, Some(entity));
            Ok(CommandResult::Pane { pane })
        }
        FocusTarget::Next | FocusTarget::Previous => {
            let tab = selection
                .tab
                .ok_or_else(|| Failure::not_found("no active tab"))?;
            let current = focus_in_tab(world, &selection, tab)
                .ok_or_else(|| Failure::not_found("no focused pane"))?;
            let next = world
                .get::<Tab>(tab)
                .and_then(|component| component.layout.cycle(current, target == FocusTarget::Next))
                .ok_or_else(|| Failure::not_found("no next pane"))?;
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
                .ok_or_else(|| Failure::not_found("no active tab"))?;
            let current = focus_in_tab(world, &selection, tab)
                .ok_or_else(|| Failure::not_found("no focused pane"))?;
            let next = world
                .get::<Tab>(tab)
                .and_then(|component| {
                    let area = component.area.navigation_area();
                    component.layout.neighbour(current, direction, area)
                })
                .ok_or_else(|| Failure::not_found("no pane in that direction"))?;
            context.select(world, tab, Some(next));
            let pane = pane_id(world, next).unwrap_or_default();
            Ok(CommandResult::Pane { pane })
        }
    }
}

pub(super) fn kill(
    world: &mut World,
    context: &Context,
    pane: PaneId,
) -> Result<CommandResult, Failure> {
    let (entity, component) = context.pane(world, pane)?;
    if matches!(component.state, PaneState::Starting) {
        return Err(Failure::conflict("pane is still starting"));
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
    pane: PaneId,
    delta: i16,
) -> Result<CommandResult, Failure> {
    let (entity, component) = context.pane(world, pane)?;
    let tab = component.tab;
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
        Err(Failure::conflict("pane cannot be resized"))
    }
}

pub(super) fn tab_action(
    world: &mut World,
    context: &Context,
    action: TabAction,
) -> Result<Outcome, Failure> {
    let tabs = member_tabs(world, context.workspace);
    let selection = context.selection(world);
    match action {
        TabAction::Reorder { tab, before } => {
            let entity = context.tab(world, tab)?.0;
            let before = before
                .map(|tab| {
                    context
                        .tab(world, tab)
                        .map(|(entity, _)| entity)
                        .map_err(|_| Failure::not_found("reference tab not found"))
                })
                .transpose()?;
            if !world
                .get_mut::<crate::ecs::components::Tabs>(context.workspace)
                .is_some_and(|mut tabs| tabs.place_before(entity, before))
            {
                return Err(Failure::conflict("tab order changed"));
            }
            mark_workspace_dirty(world, context.workspace);
            Ok(Outcome::Now(CommandResult::Tab { tab }))
        }
        TabAction::New { name } => {
            let limit = world.resource::<Limits>().max_tabs;
            if tabs.len() >= limit {
                return Err(Failure::limit("configured tab limit reached"));
            }
            let tab = reserve_tab(world, context.workspace, name)?;
            let size = selection
                .focused()
                .and_then(|pane| world.get::<Pane>(pane))
                .map_or((24, 80), |pane| pane.terminal.size());
            if let Err(failure) = reserve_pane(
                world,
                context.workspace,
                NewPane {
                    argv: Vec::new(),
                    cwd: None,
                    env: Vec::new(),
                    requester: context.requester,
                    request_id: context.id,
                    final_retain_ms: world.resource::<Limits>().final_retain_ms,
                    fixed_workspace: false,
                    right_click: Default::default(),
                },
                CreationKind::NewTab { tab },
                size,
            ) {
                despawn_tab(world, tab);
                return Err(failure);
            }
            Ok(Outcome::Deferred)
        }
        TabAction::Next | TabAction::Previous => {
            if tabs.is_empty() {
                return Err(Failure::not_found("no tabs"));
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
                .ok_or_else(|| Failure::not_found("tab not found"))?;
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
                    .ok_or_else(|| Failure::not_found("tab not found"))?,
                control::TabTarget::Id(tab) => context.tab(world, tab)?.0,
            };
            context.select_tab(world, entity);
            Ok(Outcome::Now(CommandResult::Tab {
                tab: tab_id(world, entity).unwrap_or_default(),
            }))
        }
        TabAction::Rename { tab, name } => {
            let entity = context.tab(world, tab)?.0;
            if let Some(mut component) = world.get_mut::<Tab>(entity) {
                component.label = name;
            }
            mark_workspace_dirty(world, context.workspace);
            Ok(Outcome::Now(CommandResult::Tab { tab }))
        }
        TabAction::Close { tab } => {
            let entity = context.tab(world, tab)?.0;
            let now = world.resource::<Clock>().now_ms;
            close_tab(world, entity, now, TERMINATE_GRACE_MS);
            Ok(Outcome::Now(CommandResult::Tab { tab }))
        }
    }
}

pub(super) fn workspace_action(
    world: &mut World,
    context: &Context,
    action: WorkspaceAction,
) -> Result<Outcome, Failure> {
    match action {
        WorkspaceAction::Rename { label } => {
            let label = (!label.is_empty()).then_some(label);
            let mut workspace = world
                .get_mut::<Workspace>(context.workspace)
                .ok_or_else(|| Failure::not_found("workspace does not exist"))?;
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
            reserve_workspace(world, name, context.requester, context.id)?;
            Ok(Outcome::Deferred)
        }
        WorkspaceAction::Kill { name } => {
            let entity = workspace_entity(world, &name)
                .ok_or_else(|| Failure::not_found("workspace does not exist"))?;
            if entity != context.workspace {
                return Err(Failure::new(
                    ErrorCode::Unauthorized,
                    "a workspace connection may only kill its own workspace; use `fux workspace kill`",
                ));
            }
            kill_workspace(world, entity);
            Ok(Outcome::Now(CommandResult::Workspace { name }))
        }
        WorkspaceAction::Select { name } => {
            let viewer = context
                .viewer
                .ok_or_else(|| Failure::invalid("only attached viewers switch workspaces"))?;
            let entity = workspace_entity(world, &name)
                .filter(|entity| is_accepting(world, *entity))
                .ok_or_else(|| Failure::not_found("workspace does not exist"))?;
            check_viewer_admission(world, viewer, entity)?;
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
) -> Result<(), Failure> {
    if world
        .get::<Viewer>(viewer)
        .is_some_and(|v| v.workspace == workspace)
    {
        return Ok(());
    }
    let limit = world.resource::<Limits>().max_viewers;
    if attached_viewers(world, workspace) >= limit {
        return Err(Failure::limit(
            "that workspace already has the maximum number of viewers",
        ));
    }
    Ok(())
}

//! Re-home existing pane entities. Process and retained input identities remain unchanged.
use crate::ecs::components::{Creation, Pane, Tab, TabOf};
use crate::ecs::resources::{Clock, Limits};
use crate::ecs::support::{
    Failure, close_tab, event, is_member, mark_tab_dirty, mark_workspace_dirty, member_tabs,
    pane_entity, retarget_focus, tab_entity, tab_id,
};
use crate::ids::PaneId;
use crate::layout::{Axis, Direction, LayoutTree};
use crate::proto::control::{CommandResult, Event, LayoutAction, PaneDestination};
use bevy_ecs::prelude::*;

/// One pane move: `pane` leaves the layout of `source` for `destination`, on `side` of the
/// destination pane, focusing it when `focus` is set.
pub struct MoveSpec {
    pub source: Entity,
    pub pane: PaneId,
    pub destination: PaneDestination,
    pub side: Direction,
    pub focus: bool,
}

fn move_pane(
    world: &mut World,
    workspace: Entity,
    destination_workspace: Entity,
    spec: MoveSpec,
) -> Result<CommandResult, Failure> {
    let MoveSpec {
        source,
        pane,
        destination,
        side,
        focus,
    } = spec;
    let target_ratio = match &destination {
        PaneDestination::Tab { ratio, .. } => *ratio,
        PaneDestination::NewTab { .. } => crate::layout::RATIO_SCALE / 2,
    };
    if !(crate::layout::MIN_RATIO..=crate::layout::MAX_RATIO).contains(&target_ratio) {
        return Err(Failure::invalid("transfer ratio must be 500..=9500"));
    }
    let entity = pane_entity(world, pane)
        .filter(|entity| {
            world
                .get::<Pane>(*entity)
                .is_some_and(|pane| pane.tab == source && pane.state.accepts_input())
        })
        .ok_or_else(|| Failure::not_found("live source pane not found"))?;
    if workspace != destination_workspace {
        if world
            .get::<Pane>(entity)
            .is_some_and(|pane| pane.workspace_pin.is_fixed())
        {
            return Err(Failure::conflict("pane is fixed to its current workspace"));
        }
        if crate::ecs::support::panes_in_workspace(world, destination_workspace).len()
            >= world.resource::<Limits>().max_panes
        {
            return Err(Failure::limit("configured destination pane limit reached"));
        }
        if world
            .resource::<crate::ecs::resources::InputOperations>()
            .records
            .values()
            .any(|record| {
                record.receipt.pane == pane
                    && record.receipt.state == crate::proto::control::InputState::Queued
            })
        {
            return Err(Failure::conflict(
                "tracked input is still queued for this pane",
            ));
        }
    }
    let source_component = world
        .get::<Tab>(source)
        .ok_or_else(|| Failure::not_found("source tab not found"))?;
    let mut source_tree = source_component.layout.clone();
    let source_generation = source_component
        .layout_generation
        .checked_add(1)
        .ok_or_else(|| Failure::limit("layout generation exhausted"))?;
    let next_focus = source_tree
        .close(entity)
        .map_err(|error| Failure::conflict(error.to_string()))?;
    let (existing, mut destination_tree, target) = match &destination {
        PaneDestination::Tab {
            tab,
            generation,
            target,
            ..
        } => {
            let target_tab = tab_entity(world, *tab)
                .filter(|tab| *tab != source && is_member(world, destination_workspace, *tab))
                .ok_or_else(|| {
                    Failure::not_found(
                        "destination must be another tab in the destination workspace",
                    )
                })?;
            super::layout_control::ensure_settled(world, target_tab)?;
            let component = world
                .get::<Tab>(target_tab)
                .ok_or_else(|| Failure::not_found("destination tab not found"))?;
            if component.layout_generation != *generation {
                return Err(Failure::conflict("destination layout changed"));
            }
            let target = pane_entity(world, *target)
                .filter(|target| component.layout.contains(*target))
                .ok_or_else(|| Failure::not_found("destination pane not found"))?;
            (Some(target_tab), component.layout.clone(), Some(target))
        }
        PaneDestination::NewTab { .. } => {
            if member_tabs(world, destination_workspace).len()
                >= world.resource::<Limits>().max_tabs
            {
                return Err(Failure::limit("configured tab limit reached"));
            }
            (None, LayoutTree::new(entity), None)
        }
    };
    let pending = world
        .query::<(&Pane, &Creation)>()
        .iter(world)
        .any(|(pane, _)| pane.tab == source || existing == Some(pane.tab));
    if pending {
        return Err(Failure::conflict(
            "a pane is still starting in an affected tab",
        ));
    }
    if let Some(target) = target {
        let axis = match side {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        };
        // Left/up inserts the moved pane first, so complement the target's requested share.
        let first_ratio = if matches!(side, Direction::Left | Direction::Up) {
            crate::layout::RATIO_SCALE - target_ratio
        } else {
            target_ratio
        };
        let ratio = std::num::NonZeroU16::new(first_ratio)
            .ok_or_else(|| Failure::invalid("invalid transfer ratio"))?;
        destination_tree
            .split(target, entity, axis, ratio)
            .map_err(|error| Failure::conflict(error.to_string()))?;
        if matches!(side, Direction::Left | Direction::Up) {
            destination_tree
                .swap(entity, target)
                .map_err(|error| Failure::conflict(error.to_string()))?;
        }
    }
    let destination_generation = existing
        .and_then(|tab| world.get::<Tab>(tab))
        .map_or(Some(1), |tab| tab.layout_generation.checked_add(1))
        .ok_or_else(|| Failure::limit("destination generation exhausted"))?;
    // Everything fallible is checked before publishing either layout. Creating a new tab allocates
    // only an ECS entity and public ID; it never starts a replacement pane process.
    let destination_tab = match (existing, destination) {
        (Some(tab), _) => tab,
        (None, PaneDestination::NewTab { label }) => {
            super::creation::reserve_tab(world, destination_workspace, label)?
        }
        _ => return Err(Failure::not_found("destination unavailable")),
    };
    if let Some(mut tab) = world.get_mut::<Tab>(source) {
        tab.layout = source_tree;
        tab.layout_generation = source_generation;
        tab.layout_changed = true;
        if tab.zoomed == Some(entity) {
            tab.zoomed = None;
        }
    }
    if let Some(mut tab) = world.get_mut::<Tab>(destination_tab) {
        tab.layout = destination_tree;
        tab.layout_generation = destination_generation;
        tab.layout_changed = true;
        if focus {
            tab.zoomed = None;
        }
    }
    if focus
        && let Some(mut component) =
            world.get_mut::<crate::ecs::components::Workspace>(destination_workspace)
    {
        component.selection.select(destination_tab, Some(entity));
    }
    if let Some(mut pane) = world.get_mut::<Pane>(entity) {
        pane.tab = destination_tab;
        pane.routing_workspace = destination_workspace;
    }
    if workspace != destination_workspace {
        // Invalidate unused reservations permanently, including after a move away and back.
        // A layout change is not new input: delivered prompt sequence evidence stays valid.
        for record in world
            .resource_mut::<crate::ecs::resources::InputOperations>()
            .records
            .values_mut()
        {
            if record.receipt.pane == pane
                && record.receipt.state == crate::proto::control::InputState::Reserved
            {
                record.receipt.state = crate::proto::control::InputState::Failed;
                record.receipt.error = Some(
                    "pane changed workspace before submission; reserve on its current route".into(),
                );
            }
        }
    }
    let destination_id = tab_id(world, destination_tab)
        .ok_or_else(|| Failure::not_found("destination disappeared"))?;
    if existing.is_none() {
        world
            .entity_mut(destination_tab)
            .insert(TabOf(destination_workspace));
        let label = world
            .get::<Tab>(destination_tab)
            .map(|tab| tab.label.clone())
            .unwrap_or_default();
        event(
            world,
            destination_workspace,
            Event::TabOpened {
                id: 0,
                tab: destination_id,
                name: label,
            },
        );
    }
    retarget_focus(world, source, entity, next_focus);
    if next_focus.is_none() {
        close_tab(world, source, world.resource::<Clock>().now_ms, 0);
    } else {
        mark_tab_dirty(world, source);
    }
    mark_tab_dirty(world, destination_tab);
    mark_workspace_dirty(world, workspace);
    if destination_workspace != workspace {
        mark_workspace_dirty(world, destination_workspace);
    }
    super::layout_control::apply(
        world,
        destination_workspace,
        destination_id,
        None,
        LayoutAction::Export,
    )
}

/// Workspace control connections cannot broaden their own routing authority.
pub fn within_workspace(
    world: &mut World,
    workspace: Entity,
    spec: MoveSpec,
) -> Result<CommandResult, Failure> {
    move_pane(world, workspace, workspace, spec)
}

/// Only the manager endpoint calls this entry point. Both workspace lifetimes and all layout
/// revisions are resolved before any mutation; pane launch attribution remains immutable.
pub fn across_workspaces(
    world: &mut World,
    transfer: crate::proto::control::WorkspaceTransfer,
) -> Result<CommandResult, Failure> {
    use crate::ecs::components::Workspace;
    use crate::ecs::resources::{ServerIdentity, ShuttingDown};
    use crate::ecs::support::workspace_entity;
    let request = crate::proto::control::Request::Layout {
        id: 0,
        instance: Some(transfer.instance.clone()),
        tab: transfer.source,
        generation: Some(transfer.generation),
        action: LayoutAction::Transfer {
            focus: transfer.focus,
            pane: transfer.pane,
            destination: transfer.destination.clone(),
            side: transfer.side,
        },
    };
    request.validate().map_err(|error| Failure::from(&error))?;
    if world.resource::<ShuttingDown>().0
        || world.resource::<ServerIdentity>().instance_nonce != transfer.instance
    {
        return Err(Failure::conflict("server changed or is shutting down"));
    }
    let source = tab_entity(world, transfer.source)
        .ok_or_else(|| Failure::not_found("source tab not found"))?;
    super::layout_control::ensure_settled(world, source)?;
    let component = world
        .get::<Tab>(source)
        .ok_or_else(|| Failure::not_found("source tab not found"))?;
    if component.layout_generation != transfer.generation {
        return Err(Failure::conflict("source layout changed"));
    }
    let workspace = component.workspace;
    if world
        .get::<Workspace>(workspace)
        .is_none_or(|component| !component.open || component.retiring.is_some())
    {
        return Err(Failure::conflict("source workspace is not open"));
    }
    let following = transfer
        .follow
        .map(|id| {
            crate::ecs::support::viewer_entity(world, id)
                .filter(|entity| {
                    world
                        .get::<crate::ecs::components::Viewer>(*entity)
                        .is_some_and(|viewer| {
                            !viewer.detaching
                                && viewer.workspace == workspace
                                && viewer.selection.tab == Some(source)
                        })
                })
                .ok_or_else(|| Failure::conflict("following viewer changed tab or disconnected"))
        })
        .transpose()?;
    // Following is one atomic focus outcome; do not retain the source fallback as history.
    let following_history = following.and_then(|viewer| {
        world
            .get::<crate::ecs::components::Viewer>(viewer)
            .map(|viewer| viewer.selection.history)
    });
    let (destination, created) = match transfer.workspace {
        crate::proto::control::WorkspaceDestination::Existing { name, stream } => {
            let entity = workspace_entity(world, &name)
                .filter(|entity| {
                    world
                        .get::<crate::ecs::events::EventLog>(*entity)
                        .is_some_and(|log| log.cursor().stream == stream)
                })
                .ok_or_else(|| Failure::conflict("destination workspace lifetime changed"))?;
            if world
                .get::<Workspace>(entity)
                .is_none_or(|component| !component.open || component.retiring.is_some())
            {
                return Err(Failure::conflict("destination workspace is not open"));
            }
            (entity, false)
        }
        crate::proto::control::WorkspaceDestination::New { name } => {
            if !matches!(transfer.destination, PaneDestination::NewTab { .. }) {
                return Err(Failure::invalid(
                    "a new workspace requires a new tab destination",
                ));
            }
            (super::creation::reserve_empty_workspace(world, name)?, true)
        }
    };
    let admission = following.map_or(Ok(()), |viewer| {
        super::requests_control::check_viewer_admission(world, viewer, destination)
    });
    let result = admission.and_then(|()| {
        move_pane(
            world,
            workspace,
            destination,
            MoveSpec {
                source,
                pane: transfer.pane,
                destination: transfer.destination,
                side: transfer.side,
                focus: transfer.focus || following.is_some(),
            },
        )
    });
    if created {
        if result.is_err() {
            // The reservation has no pane, tab or published endpoint when preflight fails.
            crate::ecs::support::despawn_workspace(world, destination);
        } else {
            let selection = pane_entity(world, transfer.pane)
                .and_then(|entity| world.get::<Pane>(entity).map(|pane| (pane.tab, entity)));
            if let Some(mut component) = world.get_mut::<Workspace>(destination) {
                component.open = true;
                if let Some((tab, pane)) = selection {
                    component.selection.select(tab, Some(pane));
                }
            }
            let name = world
                .get::<Workspace>(destination)
                .map(|component| component.name.clone())
                .unwrap_or_default();
            let stream = world
                .get::<crate::ecs::events::EventLog>(destination)
                .map_or(0, |log| log.cursor().stream);
            crate::ecs::support::effect(
                world,
                crate::ecs::messages::Effect::WorkspaceOpened { name, stream },
            );
        }
    }
    if result.is_ok()
        && let Some(viewer) = following
    {
        super::requests::switch_viewer_workspace(world, viewer, destination);
        let selection = pane_entity(world, transfer.pane)
            .and_then(|entity| world.get::<Pane>(entity).map(|pane| (pane.tab, entity)));
        if let Some((tab, pane)) = selection
            && let Some(mut component) = world.get_mut::<crate::ecs::components::Viewer>(viewer)
        {
            component.selection.select(tab, Some(pane));
            if let Some(history) = following_history {
                component.selection.history = history;
            }
            component.selection.history.observe(Some(pane));
            component.dirty = true;
        }
    }
    result
}

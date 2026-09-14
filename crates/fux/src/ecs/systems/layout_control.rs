//! Atomic existing-pane layout mutations shared by viewers and control clients.

use crate::ecs::components::{Pane, Tab, Viewer};
use crate::ecs::support::{Failure, is_member, mark_tab_dirty, pane_id, tab_entity};
use crate::ids::TabId;
use crate::layout::{LayoutDocument, LayoutNode, LayoutTree};
use crate::proto::control::{CommandResult, LayoutAction};
use bevy_ecs::prelude::*;

pub fn apply(
    world: &mut World,
    workspace: Entity,
    tab_id: TabId,
    generation: Option<u64>,
    action: LayoutAction,
) -> Result<CommandResult, Failure> {
    let tab = tab_entity(world, tab_id)
        .filter(|tab| is_member(world, workspace, *tab))
        .ok_or_else(|| Failure::not_found("tab not found"))?;
    match action {
        LayoutAction::Export => read(world, tab, tab_id, None),
        LayoutAction::Inspect { pane } => {
            ensure_settled(world, tab)?;
            read(world, tab, tab_id, Some(pane))
        }
        action => edit(world, workspace, tab, tab_id, generation, action),
    }
}

/// Export the tab, or inspect one of its panes. Never touches the World.
fn read(
    world: &mut World,
    tab: Entity,
    tab_id: TabId,
    inspect_pane: Option<crate::ids::PaneId>,
) -> Result<CommandResult, Failure> {
    let component = world
        .get::<Tab>(tab)
        .ok_or_else(|| Failure::not_found("tab not found"))?;
    match inspect_pane {
        Some(pane) => inspect(world, component, pane),
        None => export(world, tab, tab_id),
    }
}

/// Apply one mutation against the generation the client observed.
fn edit(
    world: &mut World,
    workspace: Entity,
    tab: Entity,
    tab_id: TabId,
    generation: Option<u64>,
    action: LayoutAction,
) -> Result<CommandResult, Failure> {
    ensure_settled(world, tab)?;
    let component = world
        .get::<Tab>(tab)
        .ok_or_else(|| Failure::not_found("tab not found"))?;
    let revision = component.layout_generation;
    if generation != Some(revision) {
        return Err(Failure::conflict(
            "layout changed; export and retry with its generation",
        ));
    }
    let mut zoomed = component.zoomed;
    let mut label_plan = Vec::new();
    let error = |err: crate::layout::LayoutError| Failure::invalid(err.to_string());
    let current = component
        .layout
        .document(|entity| pane_id(world, entity))
        .map_err(error)?;
    let panes: Vec<_> = component
        .layout
        .leaves()
        .into_iter()
        .filter_map(|entity| world.get::<Pane>(entity).map(|pane| (pane.id, entity)))
        .collect();
    let resolve = |pane| {
        panes
            .iter()
            .find_map(|(id, entity)| (*id == pane).then_some(*entity))
            .ok_or_else(|| Failure::not_found("pane does not belong to this tab"))
    };
    // Public split indices refer to the canonical exported document, not arena allocation IDs.
    let mut next = LayoutTree::from_document(
        component.layout.document(|entity| entity).map_err(error)?,
        &component.layout.leaves(),
    )
    .map_err(error)?;
    match action {
        LayoutAction::Export | LayoutAction::Inspect { .. } => {
            return Err(Failure::invalid("not a layout edit"));
        }
        LayoutAction::Transfer {
            focus,
            pane,
            destination,
            side,
        } => {
            return super::transfer::within_workspace(
                world,
                workspace,
                super::transfer::MoveSpec {
                    source: tab,
                    pane,
                    destination,
                    side,
                    focus,
                },
            );
        }
        LayoutAction::ResizeBorder {
            column,
            row,
            to_column,
            to_row,
        } => {
            if component.zoomed.is_some() {
                return Err(Failure::conflict("no separator while zoomed"));
            }
            let found = next
                .splits(component.area)
                .map_err(error)?
                .into_iter()
                .rev()
                .find(|(_, axis, rect, ratio)| {
                    let extent = if *axis == crate::layout::Axis::Horizontal {
                        rect.width
                    } else {
                        rect.height
                    };
                    let first = u32::from(extent.saturating_sub(1)) * u32::from(*ratio)
                        / u32::from(crate::layout::RATIO_SCALE);
                    rect.contains(column, row)
                        && if *axis == crate::layout::Axis::Horizontal {
                            u32::from(column) == u32::from(rect.x) + first
                        } else {
                            u32::from(row) == u32::from(rect.y) + first
                        }
                })
                .ok_or_else(|| Failure::not_found("separator not found"))?;
            let (split, axis, rect, _) = found;
            let (position, origin, extent) = if axis == crate::layout::Axis::Horizontal {
                (to_column, rect.x, rect.width)
            } else {
                (to_row, rect.y, rect.height)
            };
            let ratio = (u32::from(position.saturating_sub(origin))
                * u32::from(crate::layout::RATIO_SCALE))
            .div_ceil(u32::from(extent.saturating_sub(1).max(1)));
            let ratio = ratio.clamp(
                u32::from(crate::layout::MIN_RATIO),
                u32::from(crate::layout::MAX_RATIO),
            ) as u16;
            next.set_ratio(split, ratio).map_err(error)?;
        }
        LayoutAction::SwapDirection { pane, direction }
        | LayoutAction::MoveDirection { pane, direction } => {
            let pane = resolve(pane)?;
            let target = next
                .neighbour(pane, direction, component.area)
                .ok_or_else(|| Failure::not_found("no pane in that direction"))?;
            if matches!(action, LayoutAction::SwapDirection { .. }) {
                next.swap(pane, target).map_err(error)?;
            } else {
                next.relocate(pane, target, direction).map_err(error)?;
            }
        }
        LayoutAction::Zoom { pane } => zoomed = pane.map(resolve).transpose()?,
        LayoutAction::Swap { pane, target } => {
            next.swap(resolve(pane)?, resolve(target)?).map_err(error)?;
        }
        LayoutAction::Relocate { pane, target, side } => next
            .relocate(resolve(pane)?, resolve(target)?, side)
            .map_err(error)?,
        LayoutAction::ResizeToward {
            pane,
            direction,
            delta,
        } => next
            .resize_toward(resolve(pane)?, direction, delta)
            .map_err(error)?,
        LayoutAction::SetRatio { split, ratio } => next.set_ratio(split, ratio).map_err(error)?,
        LayoutAction::Apply {
            document,
            remap,
            zoom,
            labels,
        } => {
            let mapping: std::collections::BTreeMap<_, _> = remap.iter().copied().collect();
            if !remap.is_empty() {
                let sources: std::collections::BTreeSet<_> = document
                    .nodes
                    .iter()
                    .filter_map(|node| match node {
                        LayoutNode::Pane { pane } => Some(*pane),
                        _ => None,
                    })
                    .collect();
                if remap.len() > crate::layout::MAX_LEAVES
                    || mapping.len() != remap.len()
                    || mapping
                        .keys()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>()
                        != sources
                {
                    return Err(Failure::invalid(
                        "remap must name every source pane exactly once",
                    ));
                }
            }
            if let crate::proto::control::LayoutZoom::Set { pane } = zoom {
                zoomed = pane
                    .map(|pane| {
                        if !document.nodes.iter().any(
                            |node| matches!(node, LayoutNode::Pane { pane: id } if *id == pane),
                        ) {
                            return Err(Failure::invalid(
                                "zoom pane is not in the imported layout",
                            ));
                        }
                        resolve(mapping.get(&pane).copied().unwrap_or(pane))
                    })
                    .transpose()?;
            }
            if let Some(labels) = labels {
                let sources: Vec<_> = document
                    .nodes
                    .iter()
                    .filter_map(|node| match node {
                        LayoutNode::Pane { pane } => Some(*pane),
                        _ => None,
                    })
                    .collect();
                let labels = validate_labels(&labels, &sources)
                    .map_err(Failure::invalid)?;
                for pane in sources {
                    label_plan.push((
                        resolve(mapping.get(&pane).copied().unwrap_or(pane))?,
                        labels.get(&pane).cloned(),
                    ));
                }
            }
            let nodes = document
                .nodes
                .into_iter()
                .map(|node| {
                    Ok(match node {
                        LayoutNode::Pane { pane } => LayoutNode::Pane {
                            pane: resolve(mapping.get(&pane).copied().unwrap_or(pane))?,
                        },
                        LayoutNode::Split {
                            axis,
                            ratio,
                            first,
                            second,
                        } => LayoutNode::Split {
                            axis,
                            ratio,
                            first,
                            second,
                        },
                    })
                })
                .collect::<Result<Vec<_>, Failure>>()?;
            next = LayoutTree::from_document(
                LayoutDocument {
                    root: document.root,
                    nodes,
                },
                &component.layout.leaves(),
            )
            .map_err(error)?;
        }
    }
    let changed = label_plan.iter().any(|(entity, label)| {
        world
            .get::<Pane>(*entity)
            .is_some_and(|pane| &pane.label != label)
    }) || zoomed != component.zoomed
        || next
            .document(|entity| pane_id(world, entity))
            .map_err(error)?
            != current;
    if changed {
        let revision = revision
            .checked_add(1)
            .ok_or_else(|| Failure::limit("layout generation exhausted"))?;
        if let Some(mut component) = world.get_mut::<Tab>(tab) {
            component.layout = next;
            component.zoomed = zoomed;
            component.layout_generation = revision;
            component.layout_changed = true;
        }
        for (entity, label) in label_plan {
            if let Some(mut pane) = world.get_mut::<Pane>(entity) {
                pane.label = label;
            }
        }
        mark_tab_dirty(world, tab);
    }
    export(world, tab, tab_id)
}

fn export(world: &World, tab: Entity, tab_id: TabId) -> Result<CommandResult, Failure> {
    let component = world
        .get::<Tab>(tab)
        .ok_or_else(|| Failure::not_found("tab not found"))?;
    Ok(CommandResult::Layout {
        instance: world
            .resource::<crate::ecs::resources::ServerIdentity>()
            .instance_nonce
            .clone(),
        tab: tab_id,
        generation: component.layout_generation,
        labels: export_labels(world, &component.layout.leaves()),
        zoomed: component.zoomed.and_then(|entity| pane_id(world, entity)),
        document: export_document(world, component)?,
    })
}

pub fn export_labels(world: &World, panes: &[Entity]) -> crate::proto::control::PaneLabels {
    let mut labels: Vec<_> = panes
        .iter()
        .filter_map(|entity| {
            let pane = world.get::<Pane>(*entity)?;
            Some((pane.id, pane.label.clone()?))
        })
        .collect();
    labels.sort_by_key(|(pane, _)| *pane);
    labels
}

/// Validate before any entity changes. Empty names clear, absent entries clear, duplicates fail.
pub fn validate_labels(
    labels: &[(crate::ids::PaneId, String)],
    panes: &[crate::ids::PaneId],
) -> Result<std::collections::BTreeMap<crate::ids::PaneId, String>, String> {
    if labels.len() > crate::layout::MAX_LEAVES {
        return Err("too many pane labels".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut result = std::collections::BTreeMap::new();
    for (pane, label) in labels {
        if !panes.contains(pane) || !seen.insert(*pane) {
            return Err("pane labels must name unique panes in the imported layout".into());
        }
        if label.len() > crate::proto::control::MAX_NAME_BYTES
            || label.chars().any(char::is_control)
        {
            return Err("invalid pane label".into());
        }
        if !label.is_empty() {
            result.insert(*pane, label.clone());
        }
    }
    Ok(result)
}

/// A pending viewer resize has not advanced the revision yet. Reject edits until layout resolves.
pub fn ensure_settled(world: &mut World, tab: Entity) -> Result<(), Failure> {
    let smallest = world
        .query::<&Viewer>()
        .iter(world)
        .filter(|viewer| viewer.selection.tab == Some(tab) && !viewer.detaching)
        .map(|viewer| (viewer.rows, viewer.cols))
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1)))
        .map(|(rows, cols)| crate::ecs::support::tab_area(rows, cols));
    let component = world
        .get::<Tab>(tab)
        .ok_or_else(|| Failure::not_found("tab not found"))?;
    if smallest.is_some_and(|area| area != component.area) {
        return Err(Failure::conflict(
            "viewer area changed; wait for the next layout",
        ));
    }
    Ok(())
}

/// No mutation or cached geometry dependency: derive everything from the same current tree/area.
fn inspect(world: &World, tab: &Tab, pane: crate::ids::PaneId) -> Result<CommandResult, Failure> {
    use crate::layout::Direction;
    use crate::proto::control::{PaneEdges, PaneGeometry, PaneNeighbors};
    let entity = tab
        .layout
        .leaves()
        .into_iter()
        .find(|entity| {
            world
                .get::<Pane>(*entity)
                .is_some_and(|entry| entry.id == pane)
        })
        .ok_or_else(|| Failure::not_found("pane does not belong to this tab"))?;
    let error = |error: crate::layout::LayoutError| Failure::invalid(error.to_string());
    let area = tab.area;
    let rect = tab
        .layout
        .geometry(area)
        .map_err(error)?
        .into_iter()
        .find_map(|(candidate, rect)| (candidate == entity).then_some(rect))
        .ok_or_else(|| Failure::not_found("pane geometry missing"))?;
    let navigation_area = area.navigation_area();
    let neighbor = |direction| {
        tab.layout
            .neighbour(entity, direction, navigation_area)
            .and_then(|entity| pane_id(world, entity))
    };
    let nonempty = rect.width != 0 && rect.height != 0;
    let visible_rect = if tab.zoomed.is_some_and(|zoomed| zoomed != entity) {
        None
    } else {
        let visible = if tab.zoomed == Some(entity) {
            area
        } else {
            rect
        };
        (visible.width != 0 && visible.height != 0).then_some(visible)
    };
    Ok(CommandResult::PaneGeometry {
        geometry: Box::new(PaneGeometry {
            instance: world
                .resource::<crate::ecs::resources::ServerIdentity>()
                .instance_nonce
                .clone(),
            tab: tab.id,
            generation: tab.layout_generation,
            pane,
            area,
            navigation_area,
            rect,
            visible_rect,
            neighbors: PaneNeighbors {
                left: neighbor(Direction::Left),
                right: neighbor(Direction::Right),
                up: neighbor(Direction::Up),
                down: neighbor(Direction::Down),
            },
            edges: PaneEdges {
                left: nonempty && rect.x == area.x,
                right: nonempty
                    && rect.x.saturating_add(rect.width) == area.x.saturating_add(area.width),
                up: nonempty && rect.y == area.y,
                down: nonempty
                    && rect.y.saturating_add(rect.height) == area.y.saturating_add(area.height),
            },
            zoomed: tab.zoomed.and_then(|entity| pane_id(world, entity)),
            document: export_document(world, tab)?,
        }),
    })
}

fn export_document(
    world: &World,
    component: &Tab,
) -> Result<LayoutDocument<crate::ids::PaneId>, Failure> {
    let document = component
        .layout
        .document(|entity| pane_id(world, entity))
        .map_err(|error| Failure::invalid(error.to_string()))?;
    let nodes = document
        .nodes
        .into_iter()
        .map(|node| {
            Ok(match node {
                LayoutNode::Pane { pane } => LayoutNode::Pane {
                    pane: pane.ok_or_else(|| Failure::conflict("layout pane unavailable"))?,
                },
                LayoutNode::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => LayoutNode::Split {
                    axis,
                    ratio,
                    first,
                    second,
                },
            })
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    Ok(LayoutDocument {
        root: document.root,
        nodes,
    })
}

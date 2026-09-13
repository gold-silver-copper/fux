//! A coherent manager-level archive of existing containers and their default presentation.
use super::requests::ordered_workspaces;
use crate::ecs::components::{Pane, PaneState, Selection, Tab, Tabs, Viewer, Workspace};
use crate::ecs::events::EventLog;
use crate::ecs::resources::ServerIdentity;
use crate::proto::control::{
    CommandResult, LayoutAction, LayoutArchive, TabLayout, WorkspaceLayout,
};
use bevy_ecs::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

pub fn export(world: &mut World) -> Result<LayoutArchive, String> {
    let mut archive = LayoutArchive {
        version: 1,
        instance: world.resource::<ServerIdentity>().instance_nonce.clone(),
        workspaces: Vec::new(),
    };
    for (name, workspace) in ordered_workspaces(world) {
        let component = world
            .get::<Workspace>(workspace)
            .ok_or("workspace unavailable")?;
        let selection = component.selection.clone();
        let label = component.label.clone();
        let selected = selection
            .tab
            .and_then(|tab| world.get::<Tab>(tab).map(|tab| tab.id));
        let stream = world
            .get::<EventLog>(workspace)
            .ok_or("workspace stream unavailable")?
            .cursor()
            .stream;
        let tabs: Vec<_> = world
            .get::<Tabs>(workspace)
            .map(|tabs| tabs.iter().collect())
            .unwrap_or_default();
        let mut saved = WorkspaceLayout {
            name,
            label,
            stream,
            selected,
            tabs: Vec::new(),
        };
        for entity in tabs {
            let tab = world.get::<Tab>(entity).ok_or("tab unavailable")?;
            let id = tab.id;
            let label = tab.label.clone();
            let focused = selection
                .focus
                .get(&entity)
                .copied()
                .filter(|pane| tab.layout.contains(*pane))
                .or_else(|| tab.layout.first())
                .and_then(|pane| world.get::<Pane>(pane).map(|pane| pane.id));
            let result =
                super::layout_control::apply(world, workspace, 0, id, None, LayoutAction::Export)
                    .map_err(|error| format!("layout export failed: {error:?}"))?;
            let CommandResult::Layout {
                generation,
                zoomed,
                document,
                labels,
                ..
            } = result
            else {
                return Err("layout export returned an unexpected result".into());
            };
            saved.tabs.push(TabLayout {
                id,
                label,
                generation,
                focused,
                zoomed,
                document,
                labels,
            });
        }
        archive.workspaces.push(saved);
    }
    // Reserve space for the manager reply wrapper; oversized archives fail explicitly instead
    // of dropping the connection while the protocol writer rejects the reply.
    let bytes = serde_json::to_vec(&archive).map_err(|error| error.to_string())?;
    if bytes.len() > crate::proto::control::MAX_FRAME_BYTES.saturating_sub(128) {
        return Err("layout archive exceeds the manager frame limit".into());
    }
    Ok(archive)
}

/// Restore existing containers only. All validation and fallible planning precede the commit.
pub fn apply(
    world: &mut World,
    expected: LayoutArchive,
    mut archive: LayoutArchive,
) -> Result<LayoutArchive, String> {
    let current = export(world)?;
    if expected != current {
        return Err("layout archive changed; export a fresh expected state".into());
    }
    if archive.version != 1 || archive.instance != current.instance {
        return Err("archive version or server instance does not match".into());
    }
    let workspaces = ordered_workspaces(world);
    let names: BTreeSet<_> = archive
        .workspaces
        .iter()
        .map(|workspace| workspace.name.as_str())
        .collect();
    if names.len() != archive.workspaces.len()
        || names
            != current
                .workspaces
                .iter()
                .map(|workspace| workspace.name.as_str())
                .collect()
    {
        return Err("archive must contain every existing workspace exactly once".into());
    }
    if world
        .query::<&Pane>()
        .iter(world)
        .any(|pane| matches!(pane.state, PaneState::Starting))
    {
        return Err("wait for pending pane creation before applying an archive".into());
    }
    struct PlannedTab {
        entity: Entity,
        tree: crate::layout::LayoutTree<Entity>,
        zoomed: Option<Entity>,
        label: String,
        generation: u64,
        changed: bool,
    }
    let mut plans = Vec::new();
    let mut workspace_plans = Vec::new();
    let mut memberships = BTreeMap::new();
    let mut label_plan = BTreeMap::new();
    for saved in &mut archive.workspaces {
        if saved
            .label
            .as_ref()
            .is_some_and(|label| label.len() > 128 || label.chars().any(char::is_control))
        {
            return Err("invalid workspace label".into());
        }
        saved.label = saved.label.take().filter(|label| !label.is_empty());
        let workspace = workspaces
            .iter()
            .find(|(name, _)| name == &saved.name)
            .map(|(_, entity)| *entity)
            .ok_or("workspace missing")?;
        let original = current
            .workspaces
            .iter()
            .find(|entry| entry.name == saved.name)
            .ok_or("workspace missing")?;
        if saved.stream != original.stream {
            return Err("workspace lifetime changed".into());
        }
        let tab_entities: BTreeMap<_, _> = world
            .get::<Tabs>(workspace)
            .ok_or("workspace tabs unavailable")?
            .iter()
            .filter_map(|entity| world.get::<Tab>(entity).map(|tab| (tab.id, entity)))
            .collect();
        let tab_ids: BTreeSet<_> = saved.tabs.iter().map(|tab| tab.id).collect();
        if tab_ids.len() != saved.tabs.len() || tab_ids != tab_entities.keys().copied().collect() {
            return Err("archive must contain every workspace tab exactly once".into());
        }
        let selected = saved
            .selected
            .and_then(|id| tab_entities.get(&id).copied())
            .ok_or("selected tab is outside its workspace")?;
        let pane_entities: BTreeMap<_, _> = world
            .query::<(Entity, &Pane)>()
            .iter(world)
            .filter(|(_, pane)| {
                pane.routing_workspace == workspace
                    && tab_entities.values().any(|tab| *tab == pane.tab)
            })
            .filter(|(entity, pane)| {
                world
                    .get::<Tab>(pane.tab)
                    .is_some_and(|tab| tab.layout.contains(*entity))
            })
            .map(|(entity, pane)| (pane.id, entity))
            .collect();
        let mut used = BTreeSet::new();
        let mut order = Vec::new();
        let mut selection = Selection {
            tab: Some(selected),
            ..Selection::default()
        };
        for tab in &mut saved.tabs {
            let entity = *tab_entities.get(&tab.id).ok_or("tab missing")?;
            super::layout_control::ensure_settled(world, entity, 0)
                .map_err(|error| format!("layout is not settled: {error:?}"))?;
            if tab.label.len() > crate::proto::control::MAX_NAME_BYTES
                || tab.label.chars().any(char::is_control)
            {
                return Err("invalid tab label".into());
            }
            let ids: Vec<_> = tab
                .document
                .nodes
                .iter()
                .filter_map(|node| match node {
                    crate::layout::LayoutNode::Pane { pane } => Some(*pane),
                    _ => None,
                })
                .collect();
            if ids.is_empty()
                || ids
                    .iter()
                    .any(|id| !pane_entities.contains_key(id) || !used.insert(*id))
            {
                return Err(
                    "pane membership must be unique and remain within its workspace".into(),
                );
            }
            let tree = crate::layout::LayoutTree::from_document(tab.document.clone(), &ids)
                .map_err(|error| error.to_string())?;
            let labels = super::layout_control::validate_labels(&tab.labels, &ids)?;
            for pane in &ids {
                let entity = *pane_entities.get(pane).ok_or("pane missing")?;
                label_plan.insert(entity, labels.get(pane).cloned());
            }
            tab.labels = labels.into_iter().collect();
            tab.document = tree
                .document(|pane| pane)
                .map_err(|error| error.to_string())?;
            let focus = tab
                .focused
                .filter(|pane| ids.contains(pane))
                .and_then(|pane| pane_entities.get(&pane).copied())
                .ok_or("focused pane is outside its tab")?;
            let zoomed = tab
                .zoomed
                .map(|pane| {
                    if ids.contains(&pane) {
                        pane_entities.get(&pane).copied().ok_or("zoom pane missing")
                    } else {
                        Err("zoom pane is outside its tab")
                    }
                })
                .transpose()?;
            let mut nodes = Vec::with_capacity(tab.document.nodes.len());
            for node in &tab.document.nodes {
                nodes.push(match *node {
                    crate::layout::LayoutNode::Pane { pane } => {
                        let pane_entity = *pane_entities.get(&pane).ok_or("pane missing")?;
                        let component = world.get::<Pane>(pane_entity).ok_or("pane missing")?;
                        if component.tab != entity && !component.state.accepts_input() {
                            return Err("only live panes can change tabs".into());
                        }
                        crate::layout::LayoutNode::Pane { pane: pane_entity }
                    }
                    crate::layout::LayoutNode::Split {
                        axis,
                        ratio,
                        first,
                        second,
                    } => crate::layout::LayoutNode::Split {
                        axis,
                        ratio,
                        first,
                        second,
                    },
                });
            }
            let leaves: Vec<_> = ids
                .iter()
                .filter_map(|id| pane_entities.get(id).copied())
                .collect();
            let next = crate::layout::LayoutTree::from_document(
                crate::layout::LayoutDocument {
                    root: tab.document.root,
                    nodes,
                },
                &leaves,
            )
            .map_err(|error| error.to_string())?;
            // Serialized node indices need not follow traversal order. Focus fallback must use
            // the restored tree's first leaf, not whichever pane appeared first in the file.
            let leaves = next.leaves();
            let old = original
                .tabs
                .iter()
                .find(|old| old.id == tab.id)
                .ok_or("tab missing")?;
            let changed = tab.document != old.document
                || tab.zoomed != old.zoomed
                || tab.labels != old.labels;
            tab.generation = if changed {
                old.generation
                    .checked_add(1)
                    .ok_or("layout generation exhausted")?
            } else {
                old.generation
            };
            plans.push(PlannedTab {
                entity,
                tree: next,
                zoomed,
                label: tab.label.clone(),
                generation: tab.generation,
                changed,
            });
            memberships.insert(entity, leaves);
            selection.set_focus(entity, focus);
            order.push(entity);
        }
        if used != pane_entities.keys().copied().collect() {
            return Err("archive omitted an existing pane".into());
        }
        workspace_plans.push((workspace, order, selection, saved.label.clone()));
    }
    if serde_json::to_vec(&archive)
        .map_err(|error| error.to_string())?
        .len()
        > crate::proto::control::MAX_FRAME_BYTES.saturating_sub(128)
    {
        return Err("layout archive exceeds the manager frame limit".into());
    }
    if archive == current {
        return Ok(current);
    }
    // Commit: existing entities and workspace routes are retained. No process operation occurs.
    for (entity, label) in label_plan {
        if let Some(mut pane) = world.get_mut::<Pane>(entity) {
            pane.label = label;
        }
    }
    for plan in plans {
        if let Some(mut tab) = world.get_mut::<Tab>(plan.entity) {
            tab.layout = plan.tree;
            tab.zoomed = plan.zoomed;
            tab.label = plan.label;
            tab.layout_generation = plan.generation;
            tab.layout_changed |= plan.changed;
        }
    }
    for (tab, panes) in &memberships {
        for pane in panes {
            if let Some(mut pane) = world.get_mut::<Pane>(*pane) {
                pane.tab = *tab;
            }
        }
    }
    for mut viewer in world.query::<&mut Viewer>().iter_mut(world) {
        for (tab, panes) in &memberships {
            if viewer
                .selection
                .focus
                .get(tab)
                .is_some_and(|pane| !panes.contains(pane))
            {
                viewer.selection.retarget(*tab, panes.first().copied());
            }
        }
    }
    world
        .resource_mut::<crate::ecs::resources::WorkspaceOrder>()
        .0 = workspace_plans
        .iter()
        .map(|(entity, _, _, _)| *entity)
        .collect();
    for (workspace, order, selection, label) in workspace_plans {
        if let Some(mut tabs) = world.get_mut::<Tabs>(workspace) {
            for tab in order {
                tabs.place_before(tab, None);
            }
        }
        if let Some(mut component) = world.get_mut::<Workspace>(workspace) {
            let history = component.selection.history;
            component.selection = selection;
            component.selection.history = history;
            component.label = label;
        }
        crate::ecs::support::mark_workspace_dirty(world, workspace);
    }
    Ok(archive)
}

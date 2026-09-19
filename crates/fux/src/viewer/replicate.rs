//! Replication of the server's instance scene (prompt 3.10/3.11): every `ServerFrame::Scene` is
//! a RON `DynamicWorld` written into this World with `write_to_world_with` through a persistent
//! map keyed by the server's entity bits, plus terminal deltas that update per-pane `Grid`s. The
//! replicated root is parented under the chrome's pane area so chrome and layout are one
//! `bevy_ui` tree laid out against the local camera.

use bevy_app::prelude::*;
use bevy_asset::AssetServer;
use bevy_ecs::entity::EntityHashMap;
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_input_focus::tab_navigation::TabIndex;
use bevy_input_focus::{FocusCause, InputFocus};
use bevy_platform::collections::HashSet;
use bevy_ui::{Node, UiTargetCamera};
use bevy_world_serialization::serde::WorldDeserializer;
use serde::de::DeserializeSeed as _;

use super::chrome::{ChromeRoots, PendingNotice};
use super::paint::{CellText, ScreenCell};
use super::{Exit, Inbox, LocalCamera, Outbox};
use crate::model::{Ids, NodeId, PaneId, Shows, Surface};
use crate::wire::{
    ByeReason, ClientFrame, Cursor, Modes, ProcessSummary, RootEntry, SceneFrame, ServerFrame,
    Style, TerminalDelta, Welcome,
};

/// Marks entities created by scene replication (never chrome).
#[derive(Component, Debug, Default)]
pub struct Replicated;

/// Server entity bits → local entity, persistent across frames.
#[derive(Resource, Default, Debug)]
pub struct EntityMap(pub EntityHashMap<Entity>);

/// The workspace's roots in `RootOrder`, for the tab strip.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Roots(pub Vec<RootEntry>);

/// The pane the server says this viewer's input targets.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetPane(pub Option<PaneId>);

/// The template root the viewer shows.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowingRoot(pub Option<NodeId>);

/// The `Welcome` reply, once received.
#[derive(Resource, Default, Debug, Clone)]
pub struct Session(pub Option<Welcome>);

/// Last applied scene revision (acknowledged to the server).
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneRevision(pub u64);

/// Last scene composed for the terminal, not merely received or acknowledged.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaintedSceneRevision(pub u64);

/// Old input may survive terminal-output frames, but not a changed input scene.
#[derive(Resource, Default)]
pub struct InputSceneRevision(pub u64);

/// Original paint revision carried by the currently dispatched terminal batch.
#[derive(Resource, Default)]
pub struct InputRevision(pub u64);

fn record_paint(revision: Res<SceneRevision>, mut painted: ResMut<PaintedSceneRevision>) {
    painted.0 = revision.0;
}

/// A shown pane wrote OSC 52 since its last delta (the base64 payload as the pane sent it); the
/// painter forwards it to the outer terminal.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct ClipboardWrite(pub String);

/// One pane's screen as last delivered: rows of cells (storage reused across deltas), cursor,
/// modes and process summary.
#[derive(Component, Debug)]
pub struct Grid {
    rows: u16,
    cols: u16,
    cells: Vec<ScreenCell>,
    pub seq: u64,
    pub cursor: Cursor,
    pub modes: Modes,
    pub title: Option<String>,
    pub process: ProcessSummary,
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            rows,
            cols,
            cells: vec![ScreenCell::BLANK; usize::from(cols) * usize::from(rows)],
            seq: 0,
            cursor: Cursor::default(),
            modes: Modes::default(),
            title: None,
            process: ProcessSummary::Starting,
        }
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn cell(&self, col: u16, row: u16) -> Option<&ScreenCell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }

    fn set(&mut self, col: u16, row: u16, cell: ScreenCell) {
        if col >= self.cols || row >= self.rows {
            return;
        }
        let i = usize::from(row) * usize::from(self.cols) + usize::from(col);
        if let Some(slot) = self.cells.get_mut(i) {
            *slot = cell;
        }
    }

    /// Applies one delta: resizes on dimension change (cells are reused otherwise), then
    /// overwrites the delivered rows.
    pub fn apply(&mut self, delta: &TerminalDelta) {
        if delta.rows != self.rows || delta.cols != self.cols {
            self.rows = delta.rows;
            self.cols = delta.cols;
            self.cells.clear();
            self.cells.resize(
                usize::from(delta.cols) * usize::from(delta.rows),
                ScreenCell::BLANK,
            );
        } else if delta.full {
            self.cells.fill(ScreenCell::BLANK);
        }
        for line in &delta.lines {
            let row = line.row;
            let mut col: u16 = 0;
            for wire in &line.cells {
                let width = wire.width.max(1);
                self.set(
                    col,
                    row,
                    ScreenCell {
                        text: CellText::new(&wire.text),
                        width,
                        style: wire.style,
                    },
                );
                for extra in 1..width {
                    self.set(
                        col.saturating_add(u16::from(extra)),
                        row,
                        ScreenCell {
                            text: CellText::SPACE,
                            width: 0,
                            style: wire.style,
                        },
                    );
                }
                col = col.saturating_add(u16::from(width));
            }
            // A delivered row is complete: anything past its last run is blank.
            let blank = ScreenCell::blank(Style::default());
            while col < self.cols {
                self.set(col, row, blank);
                col += 1;
            }
        }
        self.seq = delta.seq;
        self.cursor = delta.cursor;
        self.modes = delta.modes;
        if let Some(title) = &delta.title {
            match &mut self.title {
                Some(current) => current.clone_from(title),
                None => self.title = Some(title.clone()),
            }
        }
        self.process = delta.process;
    }
}

/// `First`: drains the inbox of server frames into the World.
pub fn ingest_frames(world: &mut World) -> Result<(), BevyError> {
    let frames = core::mem::take(&mut world.resource_mut::<Inbox>().frames);
    for frame in frames {
        match frame {
            ServerFrame::Welcome(welcome) => world.resource_mut::<Session>().0 = Some(welcome),
            ServerFrame::Scene(scene) => apply_scene(world, &scene)?,
            ServerFrame::Bye { reason, message } => {
                let code = match reason {
                    ByeReason::Detached | ByeReason::ServerShutdown => 0,
                    ByeReason::Refused
                    | ByeReason::WorkspaceRetired
                    | ByeReason::ExactTargetLost
                    | ByeReason::Protocol => 1,
                };
                world.insert_resource(Exit {
                    code,
                    message: Some(format!("fux: {reason:?}: {message}")),
                });
            }
        }
    }
    Ok(())
}

/// Applies one scene frame: entities, despawns, terminal deltas, projection resources, ack.
pub fn apply_scene(world: &mut World, frame: &SceneFrame) -> Result<(), BevyError> {
    let surface_focus = world.resource::<InputFocus>().get().and_then(|entity| {
        world.get::<Surface>(entity)?;
        world.get::<NodeId>(entity).copied()
    });
    let same_target = world.resource::<TargetPane>().0 == frame.target
        && world.resource::<ShowingRoot>().0 == frame.showing;
    if frame.full
        || !same_target
        || !frame.scene.is_empty()
        || !frame.despawned.is_empty()
        || frame.roots.is_some()
    {
        world.resource_mut::<InputSceneRevision>().0 = frame.revision;
    }
    if !frame.scene.is_empty() {
        apply_dynamic_world(world, &frame.scene)?;
    }
    if !frame.despawned.is_empty() {
        world.resource_scope(|world, mut map: Mut<EntityMap>| {
            for &bits in &frame.despawned {
                let Some(server) = Entity::try_from_bits(bits) else {
                    continue;
                };
                if let Some(local) = map.0.remove(&server) {
                    // Cascading despawns may already have removed it.
                    let _ = world.try_despawn(local);
                }
            }
        });
    }
    if !frame.scene.is_empty() || !frame.despawned.is_empty() {
        collect_unshown_panes(world);
    }
    for delta in &frame.terminals {
        apply_terminal(world, delta);
    }
    if let Some(roots) = &frame.roots {
        let mut current = world.resource_mut::<Roots>();
        if current.0 != *roots {
            current.0.clone_from(roots);
        }
    }
    let mut target = world.resource_mut::<TargetPane>();
    target.set_if_neq(TargetPane(frame.target));
    let mut showing = world.resource_mut::<ShowingRoot>();
    showing.set_if_neq(ShowingRoot(frame.showing));
    // Instance entities are replaced on template edits; a stable surface leaf remains the
    // keyboard owner. Restore it in First, before PreUpdate dispatches the next input batch.
    if same_target && let Some(node) = surface_focus {
        let replacement = world
            .query_filtered::<(Entity, &NodeId), (With<Surface>, With<Replicated>)>()
            .iter(world)
            .find_map(|(entity, id)| (*id == node).then_some(entity));
        let mut focus = world.resource_mut::<InputFocus>();
        if let Some(entity) = replacement {
            focus.set(entity, FocusCause::Navigated);
        } else {
            focus.clear();
        }
    }
    if let Some(notice) = &frame.notice {
        world.resource_mut::<PendingNotice>().0 = Some(notice.clone());
    }
    world.resource_mut::<SceneRevision>().0 = frame.revision;
    world.resource_mut::<Outbox>().0.push(ClientFrame::Ack {
        revision: frame.revision,
    });
    Ok(())
}

fn apply_dynamic_world(world: &mut World, ron_text: &str) -> Result<(), BevyError> {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let mut asset_server = world.resource::<AssetServer>().clone();
    let dynamic = {
        let mut de = ron::de::Deserializer::from_str(ron_text)?;
        let seed = WorldDeserializer {
            type_registry: &registry.read(),
            load_from_path: &mut asset_server,
        };
        seed.deserialize(&mut de).map_err(|e| de.span_error(e))?
    };
    world.resource_scope(|world, mut map: Mut<EntityMap>| {
        dynamic.write_to_world_with(world, &mut map.0, &registry.read())
    })?;
    let camera = world.resource::<LocalCamera>().0;
    let area = world.resource::<ChromeRoots>().pane_area;
    let map = world.resource::<EntityMap>();
    let touched: Vec<Entity> = dynamic
        .entities
        .iter()
        .filter_map(|e| map.0.get(&e.entity).copied())
        .collect();
    for local in touched {
        let Ok(mut entity) = world.get_entity_mut(local) else {
            continue;
        };
        if !entity.contains::<Replicated>() {
            entity.insert(Replicated);
        }
        if entity.contains::<Node>() && !entity.contains::<ChildOf>() {
            // The server's instance root becomes a child of the local pane area; the layout target
            // camera is rewritten to the local one so the whole tree lays out here.
            entity.insert((ChildOf(area), UiTargetCamera(camera)));
        }
        if (entity.contains::<Shows>() || entity.contains::<Surface>())
            && !entity.contains::<TabIndex>()
        {
            entity.insert(TabIndex(0));
        }
    }
    Ok(())
}

/// Pane entities are never listed in `despawned`; they go when no leaf shows them.
fn collect_unshown_panes(world: &mut World) {
    let shown: HashSet<Entity> = world
        .query_filtered::<&Shows, With<Replicated>>()
        .iter(world)
        .map(|s| s.0)
        .collect();
    let stale: Vec<Entity> = world
        .query_filtered::<Entity, (With<PaneId>, With<Replicated>)>()
        .iter(world)
        .filter(|e| !shown.contains(e))
        .collect();
    if stale.is_empty() {
        return;
    }
    world.resource_scope(|world, mut map: Mut<EntityMap>| {
        map.0.retain(|_, local| !stale.contains(local));
        for entity in stale {
            let _ = world.try_despawn(entity);
        }
    });
}

fn apply_terminal(world: &mut World, delta: &TerminalDelta) {
    let Some(pane) = world.resource::<Ids>().pane(delta.pane) else {
        bevy_log::debug!("terminal delta for unknown pane {}", delta.pane);
        return;
    };
    let Ok(mut entity) = world.get_entity_mut(pane) else {
        return;
    };
    match entity.get_mut::<Grid>() {
        Some(mut grid) => grid.apply(delta),
        None => {
            let mut grid = Grid::new(delta.cols, delta.rows);
            grid.apply(delta);
            entity.insert(grid);
        }
    }
    if let Some(payload) = &delta.clipboard {
        world.write_message(ClipboardWrite(payload.clone()));
    }
}

/// Drops the replicated scene before a new attachment (another workspace): every replicated
/// entity, the server map and the projection resources; focus is released so the next scene's
/// target takes it.
pub fn reset(world: &mut World) {
    let replicated: Vec<Entity> = world
        .query_filtered::<Entity, With<Replicated>>()
        .iter(world)
        .collect();
    for entity in replicated {
        let _ = world.try_despawn(entity);
    }
    world.resource_mut::<EntityMap>().0.clear();
    world.resource_mut::<Roots>().0.clear();
    world.insert_resource(TargetPane::default());
    world.insert_resource(ShowingRoot::default());
    world.insert_resource(Session::default());
    world.insert_resource(SceneRevision::default());
    world.insert_resource(PaintedSceneRevision::default());
    world.insert_resource(InputSceneRevision::default());
    world.insert_resource(InputRevision::default());
    world.resource_mut::<InputFocus>().clear();
}

pub struct ReplicatePlugin;

impl Plugin for ReplicatePlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<crate::surface::Text>()
            .init_resource::<EntityMap>()
            .init_resource::<Roots>()
            .init_resource::<TargetPane>()
            .init_resource::<ShowingRoot>()
            .init_resource::<Session>()
            .init_resource::<SceneRevision>()
            .init_resource::<PaintedSceneRevision>()
            .init_resource::<InputSceneRevision>()
            .init_resource::<InputRevision>()
            .add_systems(Last, record_paint)
            .add_message::<ClipboardWrite>()
            .add_systems(First, ingest_frames.in_set(super::ViewerSystems::Ingest));
    }
}

//! The dashboard as a `bsn!` scene (prompt 4.5): a column root whose children are one-cell
//! rows, each a `Node` with a `Name` (the row key) and fux's `Text` leaf. The scene lives in
//! its own [`World`] so entity ids are the provider's and stay stable across updates
//! ([`fux::surface`] delta contract); [`SceneWorld::sync`] diffs a row list against the rows
//! the scene holds (the last streamed set) and exports the difference as a RON `DynamicWorld`
//! restricted to the surface vocabulary.

use bevy_asset::{AssetServer, Assets};
use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_scene::prelude::*;
use bevy_scene::{ResolvedSceneRoot, ScenePatch};
use bevy_ui::{
    BackgroundColor, BorderColor, FlexDirection, Node, Overflow, ScrollPosition, ZIndex, percent,
    px,
};
use bevy_world_serialization::DynamicWorldBuilder;
use fux::surface::Text;

use super::Row;

/// The root entity's `Name`.
pub const ROOT_NAME: &str = "zor-dashboard";

/// One row: a one-cell line of text named by its key.
pub fn row(key: &str, text: &str) -> impl Scene {
    let key = key.to_owned();
    let text = text.to_owned();
    bsn! {
        Node { width: percent(100.0), height: px(1.0), flex_shrink: 0.0 }
        Name({key})
        Text({text})
    }
}

/// The dashboard: a column filling the surface, `rows` as its children in order.
pub fn dashboard(rows: &[Row]) -> impl Scene {
    let rows: Vec<_> = rows.iter().map(|r| row(&r.key, &r.text)).collect();
    bsn! {
        Node {
            width: percent(100.0),
            height: percent(100.0),
            flex_direction: FlexDirection::Column,
            overflow: Overflow::scroll_y(),
        }
        Name(ROOT_NAME)
        ScrollPosition({bevy_math::Vec2::ZERO})
        Children [ {rows} ]
    }
}

/// What one sync produced: a full snapshot (structure changed) or the rewritten rows only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub full: bool,
    pub ron: String,
    /// Nodes under the surface after this delta (root included).
    pub nodes: usize,
}

/// The provider's World: the root, and the rows it currently holds with their entities.
pub struct SceneWorld {
    world: World,
    registry: AppTypeRegistry,
    root: Entity,
    rows: Vec<Row>,
    entities: Vec<Entity>,
    /// The next export is a full snapshot whatever changed (first update, or after a refused
    /// one whose effect on fux is unknown).
    resend_full: bool,
    /// Scratch for the reused-entity lookup and the next row set.
    next: Vec<Entity>,
    changed: Vec<Entity>,
}

impl SceneWorld {
    /// Spawns `dashboard(rows)` into a fresh World, resolving the scene with `main`'s asset
    /// resources (bevy_scene needs them even for a scene without assets).
    pub fn new(main: &World, rows: &[Row]) -> Result<Self, String> {
        let mut world = World::new();
        let registry = main.resource::<AppTypeRegistry>().clone();
        let root = spawn(main, &mut world, dashboard(rows))?;
        let entities: Vec<Entity> = world
            .get::<Children>(root)
            .map(|c| c.iter().collect())
            .unwrap_or_default();
        if entities.len() != rows.len() {
            return Err("dashboard scene row/entity count mismatch".into());
        }
        Ok(Self {
            world,
            registry,
            root,
            rows: rows.to_vec(),
            entities,
            resend_full: true,
            next: Vec::new(),
            changed: Vec::new(),
        })
    }

    /// The rows the scene holds: the last streamed set.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn root(&self) -> Entity {
        self.root
    }

    /// Nodes in the scene, root included.
    pub fn nodes(&self) -> usize {
        self.entities.len() + 1
    }

    /// Exact provider identities, never positional row numbers or fux's remapped NodeIds.
    pub fn row_nodes(&self) -> Vec<(String, u64)> {
        self.rows
            .iter()
            .zip(&self.entities)
            .map(|(row, entity)| (row.key.clone(), entity.to_bits()))
            .collect()
    }

    pub fn key_for_node(&self, bits: u64) -> Option<&str> {
        self.rows
            .iter()
            .zip(&self.entities)
            .find(|(_, entity)| entity.to_bits() == bits)
            .map(|(row, _)| row.key.as_str())
    }

    /// The next delta is a full snapshot.
    pub fn force_full(&mut self) {
        self.resend_full = true;
    }

    /// Brings the scene to `rows`: rows keyed the same keep their entity (their `Text` is
    /// rewritten when it changed), new keys are spawned through [`row`], absent keys are
    /// despawned, and the root's children are set to the new order. Returns the delta to
    /// stream, `None` when nothing changed.
    pub fn sync(&mut self, main: &World, rows: &[Row]) -> Result<Option<Delta>, String> {
        if self.rows.len() != self.entities.len() {
            return Err("dashboard scene row/entity count mismatch".into());
        }
        if !self.resend_full && self.rows == rows {
            return Ok(None);
        }
        let mut structural = self.resend_full;
        self.next.clear();
        self.changed.clear();
        // Reused entities: keys are unique per row set, so a linear probe over the old rows
        // suffices at dashboard sizes (the bound is `MAX_ROWS`).
        let mut reused = vec![false; self.entities.len()];
        for row in rows {
            let old = self
                .rows
                .iter()
                .zip(&self.entities)
                .zip(&mut reused)
                .find(|((old, _), used)| old.key == row.key && !**used);
            match old {
                Some(((old, &entity), used)) => {
                    *used = true;
                    if old.text != row.text {
                        let mut text = self
                            .world
                            .get_mut::<Text>(entity)
                            .ok_or("dashboard scene row text disappeared")?;
                        text.0.clear();
                        text.0.push_str(&row.text);
                        self.changed.push(entity);
                    }
                    self.next.push(entity);
                }
                None => {
                    let entity = spawn(main, &mut self.world, self::row(&row.key, &row.text))?;
                    self.world
                        .get_entity_mut(self.root)
                        .map_err(|e| e.to_string())?
                        .add_child(entity);
                    self.next.push(entity);
                    structural = true;
                }
            }
        }
        for (&entity, used) in self.entities.iter().zip(reused) {
            if !used {
                self.world.despawn(entity);
                structural = true;
            }
        }
        if !structural && self.next != self.entities {
            structural = true;
        }
        if !structural && self.changed.is_empty() {
            return Ok(None);
        }
        if structural {
            let order = self.next.clone();
            self.world
                .get_entity_mut(self.root)
                .map_err(|e| e.to_string())?
                .replace_children(&order);
        }
        core::mem::swap(&mut self.entities, &mut self.next);
        self.rows.clear();
        self.rows.extend_from_slice(rows);

        let ron = if structural {
            let mut all = Vec::with_capacity(self.entities.len() + 1);
            all.push(self.root);
            all.extend_from_slice(&self.entities);
            self.export(&all)?
        } else {
            self.export(&self.changed)?
        };
        self.resend_full = false;
        Ok(Some(Delta {
            full: structural,
            ron,
            nodes: self.nodes(),
        }))
    }

    /// RON of `entities` over the surface vocabulary.
    pub fn export(&self, entities: &[Entity]) -> Result<String, String> {
        let registry = self.registry.read();
        DynamicWorldBuilder::from_world(&self.world, &registry)
            .deny_all()
            .allow_component::<Node>()
            .allow_component::<Name>()
            .allow_component::<ZIndex>()
            .allow_component::<BackgroundColor>()
            .allow_component::<BorderColor>()
            .allow_component::<ScrollPosition>()
            .allow_component::<Text>()
            .allow_component::<ChildOf>()
            .allow_component::<Children>()
            .extract_entities(entities.iter().copied())
            .build()
            .serialize(&registry)
            .map_err(|e| e.to_string())
    }

    /// The scene's World, for tests.
    pub fn world(&self) -> &World {
        &self.world
    }
}

/// Resolves and spawns a `bsn!` scene into `target` with `main`'s asset resources.
fn spawn(main: &World, target: &mut World, scene: impl Scene) -> Result<Entity, String> {
    let assets = main
        .get_resource::<AssetServer>()
        .ok_or("dashboard: no AssetServer (bevy_asset::AssetPlugin missing)")?;
    let patches = main
        .get_resource::<Assets<ScenePatch>>()
        .ok_or("dashboard: no Assets<ScenePatch> (bevy_scene::ScenePlugin missing)")?;
    let resolved =
        ResolvedSceneRoot::resolve(Box::new(scene), assets, patches).map_err(|e| e.to_string())?;
    Ok(resolved.spawn(target).map_err(|e| e.to_string())?.id())
}

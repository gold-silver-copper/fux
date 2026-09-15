//! Relationships (prompt 3.2 table). The relationship side is the source of truth; the reverse
//! side is an index and is never edited directly.

use bevy_ecs::prelude::*;
use bevy_reflect::prelude::*;

/// Every template root (a "tab") belongs to one workspace.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
#[relationship(relationship_target = Roots)]
pub struct RootOf(pub Entity);

#[derive(Component, Reflect, Debug)]
#[reflect(Component)]
#[relationship_target(relationship = RootOf)]
pub struct Roots(Vec<Entity>);

/// Each instance node points at the template node it was cloned from.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
#[relationship(relationship_target = Instances)]
pub struct InstanceOf(pub Entity);

#[derive(Component, Reflect, Debug)]
#[reflect(Component)]
#[relationship_target(relationship = InstanceOf)]
pub struct Instances(Vec<Entity>);

/// From an instance leaf to the shared pane entity it shows. The pane's emulator size is the
/// minimum over its `ShownBy` instances' content sizes.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
#[relationship(relationship_target = ShownBy)]
pub struct Shows(pub Entity);

#[derive(Component, Reflect, Debug)]
#[reflect(Component)]
#[relationship_target(relationship = Shows)]
pub struct ShownBy(Vec<Entity>);

/// From a template leaf to the pane it places; exactly one placing leaf per pane.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
#[relationship(relationship_target = PlacedIn)]
pub struct Places(pub Entity);

#[derive(Component, Reflect, Debug)]
#[reflect(Component)]
#[relationship_target(relationship = Places)]
pub struct PlacedIn(Vec<Entity>);

/// Pane ownership for authority checks.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
#[relationship(relationship_target = WorkspacePanes)]
pub struct PaneIn(pub Entity);

#[derive(Component, Reflect, Debug)]
#[reflect(Component)]
#[relationship_target(relationship = PaneIn)]
pub struct WorkspacePanes(Vec<Entity>);

/// Viewer attachment to a workspace.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
#[relationship(relationship_target = ViewedBy)]
pub struct Viewing(pub Entity);

#[derive(Component, Reflect, Debug)]
#[reflect(Component)]
#[relationship_target(relationship = Viewing)]
pub struct ViewedBy(Vec<Entity>);

/// Where a viewer's bytes go: the server's whole notion of "focus". An exact attachment is a
/// `Targets` that cannot be retargeted (`ExactTarget` marker on the viewer).
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
#[relationship(relationship_target = TargetedBy)]
pub struct Targets(pub Entity);

#[derive(Component, Reflect, Debug)]
#[reflect(Component)]
#[relationship_target(relationship = Targets)]
pub struct TargetedBy(Vec<Entity>);

/// Input receipts on a pane; a receipt cannot outlive its pane.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
#[relationship(relationship_target = PaneOperations)]
pub struct OperationOn(pub Entity);

#[derive(Component, Reflect, Debug)]
#[reflect(Component)]
#[relationship_target(relationship = OperationOn, linked_spawn)]
pub struct PaneOperations(Vec<Entity>);

/// From a reflected projection entity (`remote::projection`) to the authoritative entity it
/// mirrors; `linked_spawn` despawns the projection with its model, so no sweep is needed.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
#[relationship(relationship_target = Projections)]
pub struct Mirrors(pub Entity);

#[derive(Component, Debug)]
#[relationship_target(relationship = Mirrors, linked_spawn)]
pub struct Projections(Vec<Entity>);

/// User-visible order of a workspace's template roots; validated against `Roots`. The entities
/// are mapped when a scene document is written into a World (`scene::apply`).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq)]
#[reflect(Component)]
pub struct RootOrder(#[entities] pub Vec<Entity>);

/// Which template root a viewer currently shows (a plain component, not a relationship, because
/// a viewer shows at most one root and the instance graph already links the rest).
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
pub struct Showing(pub Entity);

//! One guard in front of every stock BRP method.
//!
//! Before a request changes anything, the guard:
//! 1. parses it with the stock parameter type;
//! 2. resolves every entity it names: alive, never a resource entity, and for
//!    writes never an entity fux or Bevy uses internally (finding 028);
//! 3. checks every type it names against the type's `ReflectPolicy`;
//! 4. validates every new value (`ReflectValidate`), and the hierarchy as it
//!    would be after the request: kinds, parents, cycles, pane targets,
//!    viewer relationships (findings 023–027, 029);
//! 5. delegates to the stock handler, or for mutations, applies a value it
//!    validated on a copy;
//! 6. settles: flushes, collapses emptied splits and repairs viewers before
//!    the response goes out, so no client sees an intermediate state.
//!
//! A request either passes every check and is applied whole, or changes
//! nothing and gets an error that says why.
use crate::{
    invariants,
    model::*,
    policy::{Check, ReflectPolicy, ReflectValidate},
};
use bevy_ecs::{
    entity::EntityHashMap,
    prelude::*,
    reflect::{AppTypeRegistry, ReflectComponent, ReflectEvent},
    resource::IsResource,
};
use bevy_reflect::{
    GetPath, PartialReflect, TypeRegistration, TypeRegistry, serde::TypedReflectDeserializer,
};
use bevy_remote::{BrpError, BrpResult, RemotePlugin, builtin_methods as stock, error_codes};
use serde::de::DeserializeSeed;
use serde_json::Value;
use std::any::TypeId;
use std::collections::{HashMap, HashSet};

/// Replaces every stock method with its guarded form.
pub fn guard(plugin: RemotePlugin) -> RemotePlugin {
    use stock::*;
    plugin
        .with_method_main(BRP_GET_COMPONENTS_METHOD, get_components)
        .with_method_main(BRP_LIST_COMPONENTS_METHOD, list_components)
        .with_method_main(BRP_SPAWN_ENTITY_METHOD, spawn_entity)
        .with_method_main(BRP_INSERT_COMPONENTS_METHOD, insert_components)
        .with_method_main(BRP_REMOVE_COMPONENTS_METHOD, remove_components)
        .with_method_main(BRP_DESPAWN_COMPONENTS_METHOD, despawn_entity)
        .with_method_main(BRP_REPARENT_ENTITIES_METHOD, reparent_entities)
        .with_method_main(BRP_MUTATE_COMPONENTS_METHOD, mutate_components)
        .with_method_main(BRP_INSERT_RESOURCE_METHOD, insert_resources)
        .with_method_main(BRP_REMOVE_RESOURCE_METHOD, remove_resources)
        .with_method_main(BRP_MUTATE_RESOURCE_METHOD, mutate_resources)
        .with_method_main(BRP_TRIGGER_EVENT_METHOD, trigger_event)
        .with_method_main(BRP_WRITE_MESSAGE_METHOD, write_message)
}

fn parse<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, BrpError> {
    let params = params.ok_or_else(|| BrpError {
        code: error_codes::INVALID_PARAMS,
        message: "params are required".into(),
        data: None,
    })?;
    serde_json::from_value(params).map_err(|error| BrpError {
        code: error_codes::INVALID_PARAMS,
        message: error.to_string(),
        data: None,
    })
}

fn refused(message: impl Into<String>) -> BrpError {
    BrpError {
        code: error_codes::INVALID_PARAMS,
        message: message.into(),
        data: None,
    }
}

fn registry(world: &World) -> Result<AppTypeRegistry, BrpError> {
    world
        .get_resource::<AppTypeRegistry>()
        .cloned()
        .ok_or_else(|| BrpError::internal("the type registry is missing"))
}

fn registration<'r>(
    registry: &'r TypeRegistry,
    path: &str,
) -> Result<&'r TypeRegistration, BrpError> {
    registry
        .get_with_type_path(path)
        .ok_or_else(|| BrpError::component_error(format!("Unknown type: `{path}`")))
}

fn policy(registration: &TypeRegistration) -> Option<ReflectPolicy> {
    registration.data::<ReflectPolicy>().copied()
}

/// The operation a request performs on a type, for the policy check.
#[derive(Clone, Copy)]
enum Op {
    Write,
    Spawn,
    Remove,
    Trigger,
}

fn allowed(registration: &TypeRegistration, op: Op) -> Result<(), BrpError> {
    let path = registration.type_info().type_path();
    let Some(policy) = policy(registration) else {
        return Err(refused(format!(
            "`{path}` is read-only over BRP; see `fux.policy` for what clients may change"
        )));
    };
    let (ok, verb) = match op {
        Op::Write => (policy.access.write, "written"),
        Op::Spawn => (policy.access.spawn, "spawned"),
        Op::Remove => (policy.access.remove && !policy.required, "removed"),
        Op::Trigger => (policy.access.trigger, "triggered"),
    };
    if ok {
        Ok(())
    } else {
        Err(refused(format!(
            "`{path}` cannot be {verb} over BRP ({})",
            policy.note
        )))
    }
}

fn validate(
    registration: &TypeRegistration,
    new: &dyn PartialReflect,
    old: Option<&dyn PartialReflect>,
    check: &Check,
) -> Result<(), BrpError> {
    match registration.data::<ReflectValidate>() {
        Some(validator) => validator.validate(new, old, check).map_err(|reason| {
            refused(format!(
                "`{}`: {reason}",
                registration.type_info().type_path()
            ))
        }),
        None => Ok(()),
    }
}

fn deserialize(
    registry: &TypeRegistry,
    registration: &TypeRegistration,
    value: &Value,
) -> Result<Box<dyn PartialReflect>, BrpError> {
    TypedReflectDeserializer::new(registration, registry)
        .deserialize(value)
        .map_err(|error| {
            BrpError::component_error(format!(
                "{} is invalid: {error}",
                registration.type_info().type_path()
            ))
        })
}

// ---------------------------------------------------------------- entities

/// Whether an entity is one of fux's own kinds.
fn fux_kind(entity: &EntityRef) -> bool {
    entity.contains::<Workspace>()
        || entity.contains::<Tab>()
        || entity.contains::<Split>()
        || entity.contains::<PaneView>()
        || entity.contains::<Viewer>()
        || entity.contains::<Launch>()
        || entity.contains::<ProcessState>()
}

/// An entity a request may name at all: it exists.
fn alive(world: &World, entity: Entity) -> Result<(), BrpError> {
    world
        .get_entity(entity)
        .map(|_| ())
        .map_err(|_| BrpError::entity_not_found(entity))
}

/// An entity a client may change: alive, not a resource, and either one of
/// fux's kinds or made only of types clients may spawn. Observers, systems
/// and every other entity Bevy or fux keeps internally are refused: despawning
/// fux's observers silenced it (finding 028).
fn writable(world: &World, registry: &TypeRegistry, entity: Entity) -> Result<(), BrpError> {
    let entity_ref = world
        .get_entity(entity)
        .map_err(|_| BrpError::entity_not_found(entity))?;
    if entity_ref.contains::<IsResource>() {
        return Err(refused(format!(
            "{entity} holds a resource; use the resource methods"
        )));
    }
    if fux_kind(&entity_ref) {
        return Ok(());
    }
    let hierarchy = [TypeId::of::<ChildOf>(), TypeId::of::<Children>()];
    for info in world
        .inspect_entity(entity)
        .map_err(|_| BrpError::entity_not_found(entity))?
    {
        let Some(type_id) = info.type_id() else {
            return Err(internal(entity, &info.name().to_string()));
        };
        if hierarchy.contains(&type_id) {
            continue;
        }
        let spawnable = registry
            .get(type_id)
            .and_then(policy)
            .is_some_and(|p| p.access.spawn);
        if !spawnable {
            return Err(internal(entity, &info.name().to_string()));
        }
    }
    Ok(())
}

fn internal(entity: Entity, component: &str) -> BrpError {
    refused(format!(
        "{entity} is internal to fux or Bevy (it has `{component}`); clients may not change it"
    ))
}

// ------------------------------------------------------------------- plan

/// A request's changes, so the hierarchy can be checked as it would be after
/// the request, before anything is written.
#[derive(Default)]
struct Edit {
    add: HashMap<TypeId, Box<dyn PartialReflect>>,
    remove: HashSet<TypeId>,
    /// A reparent: `Some(None)` removes the parent.
    parent: Option<Option<Entity>>,
}

/// The entity a spawn creates, before it exists.
const NEW: Entity = Entity::PLACEHOLDER;

struct Plan<'w> {
    world: &'w World,
    edits: EntityHashMap<Edit>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Workspace,
    Tab,
    Split,
    Pane,
    Viewer,
    Process,
    Plain,
    Missing,
}

impl<'w> Plan<'w> {
    fn new(world: &'w World) -> Self {
        Self {
            world,
            edits: EntityHashMap::default(),
        }
    }

    fn edit(&mut self, entity: Entity) -> &mut Edit {
        self.edits.entry(entity).or_default()
    }

    fn exists(&self, entity: Entity) -> bool {
        entity == NEW || self.world.get_entity(entity).is_ok()
    }

    fn has<T: Component>(&self, entity: Entity) -> bool {
        let id = TypeId::of::<T>();
        if let Some(edit) = self.edits.get(&entity) {
            if edit.remove.contains(&id) {
                return false;
            }
            if edit.add.contains_key(&id) {
                return true;
            }
        }
        entity != NEW && self.world.get::<T>(entity).is_some()
    }

    fn added<T: Component + bevy_reflect::FromReflect>(&self, entity: Entity) -> Option<T> {
        self.edits
            .get(&entity)
            .and_then(|edit| edit.add.get(&TypeId::of::<T>()))
            .and_then(|value| T::from_reflect(value.as_ref()))
    }

    fn parent(&self, entity: Entity) -> Option<Entity> {
        if let Some(edit) = self.edits.get(&entity) {
            if let Some(parent) = edit.parent {
                return parent;
            }
            if edit.remove.contains(&TypeId::of::<ChildOf>()) {
                return None;
            }
            if let Some(child_of) = self.added::<ChildOf>(entity) {
                return Some(child_of.parent());
            }
        }
        if entity == NEW {
            return None;
        }
        self.world.get::<ChildOf>(entity).map(ChildOf::parent)
    }

    fn kind(&self, entity: Entity) -> Kind {
        if !self.exists(entity) {
            Kind::Missing
        } else if self.has::<Workspace>(entity) {
            Kind::Workspace
        } else if self.has::<Tab>(entity) {
            Kind::Tab
        } else if self.has::<Split>(entity) {
            Kind::Split
        } else if self.has::<PaneView>(entity) {
            Kind::Pane
        } else if self.has::<Viewer>(entity) {
            Kind::Viewer
        } else if self.has::<Launch>(entity) || self.has::<ProcessState>(entity) {
            Kind::Process
        } else {
            Kind::Plain
        }
    }

    fn is_container(&self, entity: Entity) -> bool {
        entity != NEW && (self.has::<Tab>(entity) || self.has::<Split>(entity))
    }

    /// The kind of an entity a request refers to. The placeholder stands for
    /// the entity a spawn creates, but as a reference it names nothing: a
    /// client could send its bits, and Bevy would then relate to an entity
    /// that was never spawned.
    fn target_kind(&self, entity: Entity) -> Kind {
        if entity == NEW {
            Kind::Missing
        } else {
            self.kind(entity)
        }
    }

    /// The pane a view shows, after the request.
    fn pane_of(&self, entity: Entity) -> Option<Entity> {
        if let Some(view) = self.added::<PaneView>(entity) {
            return Some(view.pane);
        }
        if entity == NEW {
            return None;
        }
        self.world.get::<PaneView>(entity).map(|view| view.pane)
    }

    /// Every entity whose placement a request can change: the edited ones and
    /// the current children of edited ones, whose parent's kind may change.
    fn affected(&self) -> Vec<Entity> {
        let mut out: Vec<Entity> = self.edits.keys().copied().collect();
        for entity in self.edits.keys() {
            if *entity != NEW
                && let Some(children) = self.world.get::<Children>(*entity)
            {
                out.extend(children.iter());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn check(&self) -> Result<(), BrpError> {
        for entity in self.affected() {
            self.check_entity(entity).map_err(refused)?;
        }
        Ok(())
    }

    fn check_entity(&self, entity: Entity) -> Result<(), String> {
        let shown = if entity == NEW {
            "the new entity".to_owned()
        } else {
            entity.to_string()
        };
        let layout = [
            self.has::<Workspace>(entity),
            self.has::<Tab>(entity),
            self.has::<Split>(entity),
            self.has::<PaneView>(entity),
        ];
        // A tab or a workspace may also carry Split (older scenes did);
        // otherwise one layout role per entity.
        let roles = layout.iter().filter(|r| **r).count()
            - usize::from(layout[2] && (layout[0] || layout[1]));
        if roles > 1 {
            return Err(format!("{shown} would have more than one layout role"));
        }
        let is_layout = roles > 0;
        let viewer = self.has::<Viewer>(entity);
        let process = self.has::<Launch>(entity) || self.has::<ProcessState>(entity);
        if [is_layout, viewer, process].iter().filter(|x| **x).count() > 1 {
            return Err(format!(
                "{shown} would be more than one of a layout node, a viewer and a process"
            ));
        }
        if viewer && !(entity != NEW && self.world.get::<Viewer>(entity).is_some()) {
            return Err("viewers are created by fux.attach".into());
        }
        // A viewer's relationships belong on viewers only: on a layout node
        // they made its workspace unprojectable (029's route through BRP).
        if !viewer
            && (self.has::<Viewing>(entity)
                || self.has::<OnTab>(entity)
                || self.has::<Focused>(entity))
        {
            return Err(format!(
                "{shown} is not a viewer, so it cannot view, be on a tab or focus"
            ));
        }
        // Where it sits.
        let parent = self.parent(entity);
        let parent_kind = parent.map(|p| self.target_kind(p));
        if parent_kind == Some(Kind::Missing) {
            return Err(format!("{shown} would have a parent that does not exist"));
        }
        let kind = self.kind(entity);
        match kind {
            Kind::Workspace => {
                if let Some(p) = parent {
                    return Err(format!("workspace {shown} cannot have a parent ({p})"));
                }
                if !self.has::<WorkspaceOrder>(entity) {
                    return Err(format!("workspace {shown} needs a WorkspaceOrder"));
                }
            }
            // The README's way to show a custom process spawns the view
            // first and reparents it under a tab next, and clients add a tab
            // the same way, so a new or still unplaced tab or view may have no
            // parent. Taking a placed one out is what stranded processes (024).
            Kind::Tab | Kind::Pane
                if parent.is_none()
                    && (entity == NEW || self.world.get::<ChildOf>(entity).is_none()) => {}
            Kind::Tab => {
                if parent_kind != Some(Kind::Workspace) {
                    return Err(format!("tab {shown} must be a child of a workspace"));
                }
            }
            Kind::Split | Kind::Pane => {
                if !parent.is_some_and(|p| self.is_container(p)) {
                    return Err(format!(
                        "{} {shown} must be a child of a tab or a split",
                        if kind == Kind::Pane {
                            "pane view"
                        } else {
                            "split"
                        }
                    ));
                }
            }
            Kind::Viewer | Kind::Process => {
                if let Some(p) = parent {
                    return Err(format!("{shown} cannot have a parent ({p})"));
                }
                if kind == Kind::Process && entity == NEW && !self.has::<Launch>(entity) {
                    return Err("a ProcessState is spawned only with its Launch".into());
                }
            }
            Kind::Plain => {
                if parent_kind.is_some_and(|k| k != Kind::Plain) {
                    return Err(format!(
                        "{shown} has no layout role, so it cannot be a child of a {}",
                        match parent_kind {
                            Some(Kind::Workspace) => "workspace",
                            Some(Kind::Tab) => "tab",
                            Some(Kind::Split) => "split",
                            Some(Kind::Pane) => "pane view",
                            Some(Kind::Viewer) => "viewer",
                            _ => "process",
                        }
                    ));
                }
            }
            Kind::Missing => return Ok(()),
        }
        // No cycles.
        let mut cursor = parent;
        let mut steps = 0u32;
        while let Some(p) = cursor {
            if p == entity || steps > 4096 {
                return Err(format!("{shown} would be inside its own subtree"));
            }
            steps += 1;
            cursor = self.parent(p);
        }
        // A view shows a process.
        if kind == Kind::Pane {
            let pane = self.pane_of(entity).ok_or("a pane view without a pane")?;
            if self.target_kind(pane) != Kind::Process {
                return Err(format!(
                    "pane view {shown} must show a process; {pane} is not one"
                ));
            }
        }
        // A viewer's relationships fit each other.
        if kind == Kind::Viewer {
            self.check_viewer(entity)?;
        }
        Ok(())
    }

    fn check_viewer(&self, viewer: Entity) -> Result<(), String> {
        let workspace = self
            .added::<Viewing>(viewer)
            .map(|v| v.0)
            .or_else(|| self.world.get::<Viewing>(viewer).map(|v| v.0));
        if let Some(target) = self.added::<Viewing>(viewer).map(|v| v.0)
            && self.target_kind(target) != Kind::Workspace
        {
            return Err(format!(
                "viewer {viewer} can only view a workspace; {target} is not one"
            ));
        }
        let tab = self
            .added::<OnTab>(viewer)
            .map(|t| t.0)
            .or_else(|| self.world.get::<OnTab>(viewer).map(|t| t.0));
        if let Some(target) = self.added::<OnTab>(viewer).map(|t| t.0)
            && (self.target_kind(target) != Kind::Tab || self.parent(target) != workspace)
        {
            return Err(format!(
                "viewer {viewer} can only be on a tab of its workspace"
            ));
        }
        if let Some(target) = self.added::<Focused>(viewer).map(|f| f.0) {
            let mut cursor = Some(target);
            let mut inside = false;
            let mut steps = 0u32;
            while let Some(entity) = cursor {
                if Some(entity) == tab {
                    inside = true;
                    break;
                }
                steps += 1;
                if steps > 4096 {
                    break;
                }
                cursor = self.parent(entity);
            }
            if self.target_kind(target) != Kind::Pane || !inside {
                return Err(format!("viewer {viewer} can only focus a pane of its tab"));
            }
        }
        Ok(())
    }
}

// ----------------------------------------------------------------- settle

/// Brings the world to rest before the response: pending commands, emptied
/// splits, viewer repair. Under tests, or with `FUX_CHECK_INVARIANTS=1`, the
/// invariants are then checked, and a violation is logged with the request.
fn settle(world: &mut World, method: &str) {
    world.flush();
    if let Err(error) = world.run_system_cached(crate::layout::collapse_layout) {
        bevy_log::error!("collapsing layout after {method}: {error}");
    }
    crate::navigation::repair(world);
    world.flush();
    // Checking every invariant costs a pass over the whole world, which a
    // large one pays on every request; it is a diagnostic, so it runs only
    // under tests or when asked for.
    if cfg!(test) || check_invariants() {
        let broken = invariants::violations(world);
        if !broken.is_empty() {
            bevy_log::error!(
                "{method} left the world inconsistent: {}",
                broken.join("; ")
            );
        }
    }
}

/// `FUX_CHECK_INVARIANTS=1` makes the server check every invariant after each
/// write and log what a request broke.
fn check_invariants() -> bool {
    static CHECK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CHECK.get_or_init(|| std::env::var_os("FUX_CHECK_INVARIANTS").is_some_and(|v| v == "1"))
}

// ------------------------------------------------------------------ reads

fn get_components(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let checked: stock::BrpGetComponentsParams = parse(params.clone())?;
    alive(world, checked.entity)?;
    stock::process_remote_get_components_request(In(params), world)
}

fn list_components(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let checked: stock::BrpListComponentsParams = parse(params.clone())?;
    alive(world, checked.entity)?;
    stock::process_remote_list_components_request(In(params), world)
}

// ----------------------------------------------------------------- writes

/// The next free workspace order.
fn next_order(world: &mut World) -> i64 {
    world
        .query::<&WorkspaceOrder>()
        .iter(world)
        .map(|order| order.0)
        .max()
        .map_or(0, |max| max.saturating_add(1))
}

const ORDER_PATH: &str = "fux::model::WorkspaceOrder";
const WORKSPACE_PATH: &str = "fux::model::Workspace";

/// Checks the components a spawn or insert would add to `entity`.
fn check_components<'a>(
    world: &World,
    registry: &TypeRegistry,
    entity: Entity,
    components: impl IntoIterator<Item = (&'a String, &'a Value)>,
    op: Op,
    plan: &mut Plan,
) -> Result<(), BrpError> {
    for (path, value) in components {
        let registration = registration(registry, path)?;
        if registration.data::<ReflectComponent>().is_none() {
            return Err(BrpError::component_error(format!(
                "`{path}` is not a component"
            )));
        }
        allowed(registration, op)?;
        let new = deserialize(registry, registration, value)?;
        let old = (entity != NEW)
            .then(|| world.get_entity(entity).ok())
            .flatten()
            .and_then(|e| registration.data::<ReflectComponent>()?.reflect(e))
            .map(|old| old.as_partial_reflect());
        let check = Check {
            world,
            entity: (entity != NEW).then_some(entity),
        };
        validate(registration, new.as_ref(), old, &check)?;
        plan.edit(entity).add.insert(registration.type_id(), new);
    }
    Ok(())
}

fn spawn_entity(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let mut request: stock::BrpSpawnEntityParams = parse(params)?;
    // A workspace needs an order; a spawn that leaves it out gets the next.
    if request.components.contains_key(WORKSPACE_PATH)
        && !request.components.contains_key(ORDER_PATH)
    {
        let order = next_order(world);
        request
            .components
            .insert(ORDER_PATH.to_owned(), Value::from(order));
    }
    let registry = registry(world)?;
    {
        let registry = registry.read();
        let mut plan = Plan::new(world);
        check_components(
            world,
            &registry,
            NEW,
            &request.components,
            Op::Spawn,
            &mut plan,
        )?;
        plan.check()?;
    }
    let params = serde_json::to_value(&request).map_err(|e| BrpError::internal(e.to_string()))?;
    let result = stock::process_remote_spawn_entity_request(In(Some(params)), world);
    settle(world, "world.spawn_entity");
    result
}

fn insert_components(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let mut request: stock::BrpInsertComponentsParams = parse(params)?;
    let registry = registry(world)?;
    {
        let registry = registry.read();
        writable(world, &registry, request.entity)?;
    }
    // Turning an entity into a workspace gives it the next order too.
    if request.components.contains_key(WORKSPACE_PATH)
        && !request.components.contains_key(ORDER_PATH)
        && world.get::<WorkspaceOrder>(request.entity).is_none()
    {
        let order = next_order(world);
        request
            .components
            .insert(ORDER_PATH.to_owned(), Value::from(order));
    }
    {
        let registry = registry.read();
        let mut plan = Plan::new(world);
        check_components(
            world,
            &registry,
            request.entity,
            &request.components,
            Op::Write,
            &mut plan,
        )?;
        plan.check()?;
    }
    let params = serde_json::to_value(&request).map_err(|e| BrpError::internal(e.to_string()))?;
    let result = stock::process_remote_insert_components_request(In(Some(params)), world);
    settle(world, "world.insert_components");
    result
}

fn remove_components(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let request: stock::BrpRemoveComponentsParams = parse(params.clone())?;
    let registry = registry(world)?;
    {
        let registry = registry.read();
        writable(world, &registry, request.entity)?;
        let mut plan = Plan::new(world);
        for path in &request.components {
            let registration = registration(&registry, path)?;
            allowed(registration, Op::Remove)?;
            plan.edit(request.entity)
                .remove
                .insert(registration.type_id());
            // Removing a Viewer detaches it; fux strips its relationships too.
            if registration.type_id() == TypeId::of::<Viewer>() {
                let edit = plan.edit(request.entity);
                edit.remove.insert(TypeId::of::<Viewing>());
                edit.remove.insert(TypeId::of::<OnTab>());
                edit.remove.insert(TypeId::of::<Focused>());
            }
        }
        plan.check()?;
    }
    let result = stock::process_remote_remove_components_request(In(params), world);
    settle(world, "world.remove_components");
    result
}

fn despawn_entity(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let request: stock::BrpDespawnEntityParams = parse(params.clone())?;
    {
        let registry = registry(world)?;
        let registry = registry.read();
        writable(world, &registry, request.entity)?;
    }
    let result = stock::process_remote_despawn_entity_request(In(params), world);
    settle(world, "world.despawn_entity");
    result
}

fn reparent_entities(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let request: stock::BrpReparentEntitiesParams = parse(params.clone())?;
    {
        let registry = registry(world)?;
        let registry = registry.read();
        if let Some(parent) = request.parent {
            writable(world, &registry, parent)?;
        }
        let mut plan = Plan::new(world);
        for entity in &request.entities {
            writable(world, &registry, *entity)?;
            if request.parent == Some(*entity) {
                return Err(BrpError::self_reparent(*entity));
            }
            plan.edit(*entity).parent = Some(request.parent);
        }
        plan.check()?;
    }
    let result = stock::process_remote_reparent_entities_request(In(params), world);
    settle(world, "world.reparent_entities");
    result
}

/// Applies a reflect path to a copy of a value, returning the new value.
fn mutated(
    registry: &TypeRegistry,
    current: &dyn bevy_reflect::Reflect,
    path: &str,
    value: &Value,
) -> Result<Box<dyn bevy_reflect::Reflect>, BrpError> {
    let mut copy = current
        .reflect_clone()
        .map_err(|e| BrpError::component_error(format!("cannot copy the value: {e}")))?;
    let field = copy
        .reflect_path_mut(path)
        .map_err(|e| BrpError::component_error(e.to_string()))?;
    let field_type = registry
        .get_with_type_path(field.reflect_type_path())
        .ok_or_else(|| BrpError::component_error(format!("unknown field type at `{path}`")))?;
    let new_field = deserialize(registry, field_type, value)?;
    field
        .try_apply(new_field.as_ref())
        .map_err(|e| BrpError::component_error(e.to_string()))?;
    Ok(copy)
}

/// A mutation of the component of `registration` on `entity`: validated on a
/// copy, then applied in place, so change detection sees an ordinary change
/// and no insert or remove hook runs.
fn mutate_on(
    world: &mut World,
    registry: &TypeRegistry,
    registration: &TypeRegistration,
    entity: Entity,
    path: &str,
    value: &Value,
) -> Result<(), BrpError> {
    let type_path = registration.type_info().type_path();
    let component = registration
        .data::<ReflectComponent>()
        .ok_or_else(|| BrpError::component_error(format!("`{type_path}` is not a component")))?;
    if let Some(id) = world.components().get_id(registration.type_id())
        && world
            .components()
            .get_info(id)
            .is_some_and(|info| !info.mutable())
    {
        return Err(refused(format!(
            "`{type_path}` is immutable (a relationship); replace it with world.insert_components"
        )));
    }
    let entity_ref = world
        .get_entity(entity)
        .map_err(|_| BrpError::entity_not_found(entity))?;
    let current = component
        .reflect(entity_ref)
        .ok_or_else(|| BrpError::component_not_present(type_path, entity))?;
    let new = mutated(registry, current, path, value)?;
    let check = Check {
        world,
        entity: Some(entity),
    };
    validate(
        registration,
        new.as_partial_reflect(),
        Some(current.as_partial_reflect()),
        &check,
    )?;
    let mut target = world.entity_mut(entity);
    component.apply(&mut target, new.as_partial_reflect());
    Ok(())
}

fn mutate_components(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let request: stock::BrpMutateComponentsParams = parse(params)?;
    let registry = registry(world)?;
    let registry = registry.read();
    writable(world, &registry, request.entity)?;
    let registration = registration(&registry, &request.component)?;
    allowed(registration, Op::Write)?;
    mutate_on(
        world,
        &registry,
        registration,
        request.entity,
        &request.path,
        &request.value,
    )?;
    drop(registry);
    settle(world, "world.mutate_components");
    Ok(Value::Null)
}

/// The entity holding a resource, if the resource exists.
fn resource_entity(world: &World, registration: &TypeRegistration) -> Option<Entity> {
    let id = world.components().get_id(registration.type_id())?;
    let mut resources = world.try_query_filtered::<EntityRef, With<IsResource>>()?;
    resources
        .iter(world)
        .find(|entity| entity.contains_id(id))
        .map(|entity| entity.id())
}

fn resource_registration<'r>(
    registry: &'r TypeRegistry,
    path: &str,
) -> Result<&'r TypeRegistration, BrpError> {
    let registration = registry
        .get_with_type_path(path)
        .ok_or_else(|| BrpError::resource_error(format!("Unknown resource type: `{path}`")))?;
    if registration
        .data::<bevy_ecs::reflect::ReflectResource>()
        .is_none()
    {
        return Err(BrpError::resource_error(format!(
            "`{path}` is not a resource"
        )));
    }
    Ok(registration)
}

fn insert_resources(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let request: stock::BrpInsertResourcesParams = parse(params.clone())?;
    {
        let registry = registry(world)?;
        let registry = registry.read();
        let registration = resource_registration(&registry, &request.resource)?;
        allowed(registration, Op::Write)?;
        let new = deserialize(&registry, registration, &request.value)?;
        let old = resource_entity(world, registration)
            .and_then(|entity| world.get_entity(entity).ok())
            .and_then(|entity| registration.data::<ReflectComponent>()?.reflect(entity))
            .map(|old| old.as_partial_reflect());
        let check = Check {
            world,
            entity: None,
        };
        validate(registration, new.as_ref(), old, &check)?;
    }
    let result = stock::process_remote_insert_resources_request(In(params), world);
    settle(world, "world.insert_resources");
    result
}

fn remove_resources(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let request: stock::BrpRemoveResourcesParams = parse(params.clone())?;
    {
        let registry = registry(world)?;
        let registry = registry.read();
        let registration = resource_registration(&registry, &request.resource)?;
        allowed(registration, Op::Remove)?;
    }
    let result = stock::process_remote_remove_resources_request(In(params), world);
    settle(world, "world.remove_resources");
    result
}

fn mutate_resources(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let request: stock::BrpMutateResourcesParams = parse(params)?;
    let registry = registry(world)?;
    let registry = registry.read();
    let registration = resource_registration(&registry, &request.resource)?;
    allowed(registration, Op::Write)?;
    let entity = resource_entity(world, registration)
        .ok_or_else(|| BrpError::resource_not_present(&request.resource))?;
    mutate_on(
        world,
        &registry,
        registration,
        entity,
        &request.path,
        &request.value,
    )?;
    drop(registry);
    settle(world, "world.mutate_resources");
    Ok(Value::Null)
}

fn trigger_event(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let request: stock::BrpTriggerEventParams = parse(params.clone())?;
    {
        let registry = registry(world)?;
        let registry = registry.read();
        let registration = registry.get_with_type_path(&request.event).ok_or_else(|| {
            BrpError::resource_error(format!("Unknown event type: `{}`", request.event))
        })?;
        if registration.data::<ReflectEvent>().is_none() {
            return Err(BrpError::resource_error(format!(
                "`{}` is not an event",
                request.event
            )));
        }
        allowed(registration, Op::Trigger)?;
        let value = request.value.clone().unwrap_or(Value::Null);
        let new = deserialize(&registry, registration, &value)?;
        let check = Check {
            world,
            entity: None,
        };
        // Every triggerable event has validation, and building the value is
        // part of it: a partial event panicked in Bevy's fallback (022).
        if registration.data::<ReflectValidate>().is_none() {
            return Err(refused(format!(
                "`{}` has no validation, so it cannot be triggered",
                request.event
            )));
        }
        validate(registration, new.as_ref(), None, &check)?;
    }
    let result = stock::process_remote_trigger_event_request(In(params), world);
    settle(world, "world.trigger_event");
    result
}

fn write_message(In(params): In<Option<Value>>, _world: &mut World) -> BrpResult {
    let request: stock::BrpWriteMessageParams = parse(params)?;
    Err(refused(format!(
        "`{}`: no message can be written over BRP; see `fux.policy`",
        request.message
    )))
}

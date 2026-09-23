//! Workspace layouts as scenes: the cached projection each viewer paints from,
//! scene files saved, loaded and reloaded off the ECS thread, and collapsing
//! splits left with one child.
#[cfg(test)]
mod tests;
use crate::{assets, model::*, server::first_leaf};
use bevy_ecs::{
    prelude::*,
    system::SystemChangeTick,
    world::{CommandQueue, EntityRefExcept},
};
use bevy_world_serialization::DynamicWorld;
use std::io::Read;
use std::sync::Arc;

/// A scene save or load in flight for the viewer that asked. Detaching the
/// viewer drops the task with it; the file operation still completes.
#[derive(Component)]
pub(crate) struct PendingScene(bevy_tasks::Task<CommandQueue>);

/// While a scene task runs, the runner polls on a deadline instead of parking:
/// a task cannot wake the runner after its own result is stored.
pub(crate) fn pending_scenes(world: &mut World) -> bool {
    world
        .query_filtered::<(), With<PendingScene>>()
        .iter(world)
        .next()
        .is_some()
}

pub(crate) fn scene_completions(
    mut pending: Query<(Entity, &mut PendingScene)>,
    mut commands: Commands,
) {
    for (viewer, mut task) in &mut pending {
        if let Some(mut queue) = bevy_tasks::futures::check_ready(&mut task.0) {
            commands.entity(viewer).remove::<PendingScene>();
            commands.append(&mut queue);
        }
    }
}

#[expect(
    clippy::type_complexity,
    reason = "the exception list is the point of this query"
)]
pub(crate) fn invalidate_layouts(
    mut layouts: Query<(Entity, &mut LayoutCache), With<Workspace>>,
    // Every layout entity without the cache itself and without viewer bookkeeping.
    entities: Query<
        EntityRefExcept<(LayoutCache, Viewers, TabViewers, FocusedBy)>,
        Without<bevy_ecs::resource::IsResource>,
    >,
    children: Query<&Children>,
    ticks: SystemChangeTick,
    components: &bevy_ecs::component::Components,
) {
    let ignored = [
        components.component_id::<Viewers>(),
        components.component_id::<TabViewers>(),
        components.component_id::<FocusedBy>(),
    ];
    for (root, mut cache) in &mut layouts {
        if cache.scene.is_none() {
            continue;
        }
        let mut count = 0;
        // Unrestricted reflection requires checking every component, not just Node.
        // Stop at the first change; uncached workspaces need no scan at all.
        let dirty = std::iter::once(root)
            .chain(children.iter_descendants(root))
            .any(|id| {
                let Ok(entity) = entities.get(id) else {
                    return false;
                };
                count += 1;
                let entity = entity.into_filtered();
                let archetype = entity.archetype();
                let mut shape: Vec<_> = archetype
                    .components()
                    .iter()
                    .copied()
                    .filter(|id| !ignored.contains(&Some(*id)))
                    .collect();
                shape.sort();
                cache.members.get(&id).is_none_or(|known| **known != *shape)
                    || shape.iter().any(|id| {
                        entity.get_change_ticks_by_id(*id).is_some_and(|change| {
                            change.is_changed(cache.built_at, ticks.this_run())
                        })
                    })
            });
        // Missing members cover removal/despawn/reparent out of the old root.
        if dirty || count != cache.members.len() {
            cache.scene = None;
        }
    }
}

pub(crate) fn reload_layouts(
    mut reloads: MessageReader<assets::LayoutReload>,
    mut commands: Commands,
) {
    for reload in reloads.read() {
        let handle = reload.handle.clone();
        commands.queue(move |world: &mut World| {
            world.resource_scope(|world, collection: Mut<bevy_asset::Assets<DynamicWorld>>| {
                if let Some(scene) = collection.get(&handle) {
                    match assets::apply_layout(world, scene, &[]) {
                        Ok(root) => {
                            let scene_name = world.get::<Name>(root).cloned();
                            let old = world
                                .query_filtered::<(Entity, &Name), With<Workspace>>()
                                .iter(world)
                                .find(|(entity, name)| {
                                    *entity != root && scene_name.as_ref() == Some(*name)
                                })
                                .map(|(entity, _)| entity);
                            if let Some(old) = old {
                                replace_workspace(world, old, root);
                            } else {
                                // Added beside the existing workspaces: the
                                // file's saved order is meaningless here and
                                // may collide with a live one.
                                let order = crate::navigation::workspaces(world)
                                    .into_iter()
                                    .filter(|e| *e != root)
                                    .filter_map(|e| world.get::<WorkspaceOrder>(e).map(|o| o.0))
                                    .max()
                                    .unwrap_or(-1)
                                    .saturating_add(1);
                                world.entity_mut(root).insert(WorkspaceOrder(order));
                            }
                        }
                        Err(error) => bevy_log::error!("layout reload: {error}"),
                    }
                }
            });
        });
    }
}

fn replace_workspace(world: &mut World, old: Entity, new: Entity) {
    // The new workspace takes the replaced one's place in the order. The
    // order saved in the file belongs to the session that saved it and can
    // collide with a workspace created or reordered since.
    if let Some(order) = world.get::<WorkspaceOrder>(old).copied() {
        world.entity_mut(new).insert(order);
    }
    let first = first_leaf(world, new);
    let viewers: Vec<Entity> = world
        .get::<Viewers>(old)
        .map(|viewers| viewers.iter().collect())
        .unwrap_or_default();
    for id in viewers {
        let mut viewer = world.entity_mut(id);
        viewer.remove::<(OnTab, Focused)>().insert(Viewing(new));
        if let Some(first) = first {
            viewer.insert(Focused(first));
        }
        if let Some(mut v) = world.get_mut::<Viewer>(id) {
            v.zoom = false;
        }
    }
    // Replacing is a close: the old hierarchy goes, and any process it alone
    // referenced is terminated rather than left running with no view.
    crate::interaction::close(world, old);
}

pub(crate) fn scene(world: &mut World, root: Entity) -> Result<(u32, Arc<DynamicWorld>), String> {
    if world.get::<Workspace>(root).is_none() {
        return Err("layout root is not a workspace".into());
    }
    let cache = world
        .entity(root)
        .get_ref::<LayoutCache>()
        .ok_or("layout cache is missing")?;
    if let Some(scene) = &cache.scene {
        return Ok((cache.last_changed().get(), Arc::clone(scene)));
    }
    // Advance the native tick at extraction: later mutations in this same
    // exclusive transaction must be distinguishable from the captured scene.
    let built_at = world.increment_change_tick();
    let scene = Arc::new(assets::extract_layout(world, root)?);
    let members = scene
        .entities
        .iter()
        .map(|entity| (entity.entity, shape(world, entity.entity)))
        .collect();
    let mut cache = world
        .get_mut::<LayoutCache>(root)
        .ok_or("layout cache is missing")?;
    cache.built_at = built_at;
    cache.scene = Some(Arc::clone(&scene));
    cache.members = members;
    Ok((cache.last_changed().get(), scene))
}

/// Serializes on the World (only native scene serialization needs it), then
/// reads or writes the file on the task pool and returns through ECS.
/// The largest scene file `load_layout` will read. A saved layout is a
/// hierarchy of nodes, not history or output, so even a workspace of a
/// thousand panes is far below this; the bound exists because the path is a
/// caller's choice and `read_to_string` had none, so pointing it at `/dev/zero`
/// or a huge file grew the server without limit and parsing a huge one blocked
/// every session (hunt 8 finding 016). It matches the transport's other reply
/// bound rather than being tuned to a measured maximum, because no legitimate
/// layout comes close.
pub const MAX_SCENE: u64 = 8 << 20;

/// Reads a scene file, refusing one larger than `MAX_SCENE` before holding it
/// whole, and reading at most that many bytes so an endless file such as
/// `/dev/zero` cannot grow the buffer without bound.
fn read_scene(path: &str) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    if let Ok(meta) = file.metadata()
        && meta.is_file()
        && meta.len() > MAX_SCENE
    {
        return Err(format!(
            "scene file is {} bytes, over the {MAX_SCENE}-byte limit",
            meta.len()
        ));
    }
    // A non-regular file (a pipe, /dev/zero) reports no length, so cap the read
    // itself: one byte past the limit is enough to tell it was exceeded.
    let mut text = String::new();
    let read = file
        .take(MAX_SCENE + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if read as u64 > MAX_SCENE {
        return Err(format!("scene file exceeds the {MAX_SCENE}-byte limit"));
    }
    Ok(text)
}

pub(crate) fn scene_io(
    world: &mut World,
    id: Entity,
    root: Entity,
    path: String,
    load: Option<Vec<(Entity, Entity)>>,
) {
    let serialized = match &load {
        Some(_) => Ok(None),
        None => assets::serialize_layout(world, root).map(Some),
    };
    let mapping = load.unwrap_or_default();
    let wake = world.resource::<Wake>().clone();
    let task = bevy_tasks::IoTaskPool::get().spawn(async move {
        let result = serialized.and_then(|text| match text {
            Some(text) => std::fs::write(&path, text)
                .map(|_| None)
                .map_err(|e| e.to_string()),
            None => read_scene(&path).map(Some),
        });
        let mut queue = CommandQueue::default();
        queue.push(move |world: &mut World| {
            let result = match result {
                // The workspace was checked when the request arrived, but the
                // file read happened off-thread: a close in between must fail
                // the load, not add a workspace nobody asked for.
                Ok(Some(_)) if world.get::<Workspace>(root).is_none() => {
                    Err("target no longer exists".to_owned())
                }
                Ok(Some(text)) => assets::deserialize_layout(world, &text, &mapping).map(|new| {
                    replace_workspace(world, root, new);
                    format!("loaded {path}")
                }),
                Ok(None) => Ok(format!("saved {path}")),
                Err(error) => Err(error),
            };
            notify(
                world,
                id,
                match result {
                    Ok(text) => Notice::info(text),
                    Err(error) => Notice::error(error),
                },
            );
        });
        // Wakes the runner for the common case; `pending_scenes` covers the rest.
        wake.notify();
        queue
    });
    if let Ok(mut viewer) = world.get_entity_mut(id) {
        viewer.insert(PendingScene(task));
    }
}

type Collapsible = (With<Split>, Without<Tab>, Without<Workspace>);

pub(crate) fn collapse_layout(
    mut commands: Commands,
    containers: Query<(Entity, &ChildOf, Option<&Children>), Collapsible>,
    children: Query<&Children>,
    wake: Res<Wake>,
) {
    for (entity, parent, descendants) in &containers {
        let count = descendants.map_or(0, Children::len);
        if count > 1 {
            continue;
        }
        // Collapse bottom-up so two deferred operations never destroy each
        // other's still-parented children.
        let child = descendants.and_then(|children| children.first()).copied();
        // Defer only to a child container that will itself collapse this
        // frame. A healthy child split is hoisted like a leaf; otherwise a
        // single-child wrapper would survive every later frame.
        if child.is_some_and(|child| {
            containers
                .get(child)
                .is_ok_and(|(_, _, kids)| kids.map_or(0, Children::len) <= 1)
        }) {
            continue;
        }
        if let Some(child) = child {
            let Ok(siblings) = children.get(parent.parent()) else {
                continue;
            };
            let Some(index) = siblings.iter().position(|e| e == entity) else {
                continue;
            };
            commands
                .entity(parent.parent())
                .insert_children(index, &[child]);
        }
        commands.entity(entity).despawn();
        wake.notify();
    }
}

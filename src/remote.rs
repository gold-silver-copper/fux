//! The BRP method registry fux serves: every stock method, two of them guarded,
//! and the fux extensions.
use crate::frame;
use bevy_ecs::prelude::*;
use bevy_remote::RemotePlugin;

pub fn remote() -> RemotePlugin {
    use bevy_remote::builtin_methods::{
        BRP_DESPAWN_COMPONENTS_METHOD, BRP_MUTATE_COMPONENTS_METHOD,
    };
    // Registered after the stock methods, so these two replace them by name.
    // The registry stays unfiltered: each guard answers for the entity the
    // request names and then hands the request to the stock handler.
    RemotePlugin::default()
        .with_method_main(BRP_MUTATE_COMPONENTS_METHOD, mutate_components)
        .with_method_main(BRP_DESPAWN_COMPONENTS_METHOD, despawn_entity)
        .with_method_main("fux.attach", frame::attach)
        .with_method_main("fux.frame", frame::frame)
        .with_watching_method_main("fux.frame+watch", frame::frame_watch)
}

/// The entity a stock request names in `params.entity`, if it parses as one.
/// A request that does not parse is left to the stock handler, which answers
/// it with its own error.
fn named_entity(params: Option<&serde_json::Value>) -> Option<Entity> {
    serde_json::from_value(params?.get("entity")?.clone()).ok()
}

/// Stock `world.mutate_components`, answering for the entity first. The stock
/// handler resolves it with `World::entity_mut`, which panics on an entity
/// that is not alive, so one request ended the server (hunt 6 finding 005). A
/// despawned id is the ordinary way in: read an id, have it closed underneath
/// you, write back. The siblings (`get_components`, `insert_components`,
/// `remove_components`) already answer `entity_not_found`; so does this now.
fn mutate_components(
    In(params): In<Option<serde_json::Value>>,
    world: &mut World,
) -> bevy_remote::BrpResult {
    if let Some(entity) = named_entity(params.as_ref())
        && world.get_entity(entity).is_err()
    {
        return Err(bevy_remote::BrpError::entity_not_found(entity));
    }
    bevy_remote::builtin_methods::process_remote_mutate_components_request(In(params), world)
}

/// Stock `world.despawn_entity`, refusing an entity that holds a resource.
/// Resources are entities in Bevy 0.20, and despawning one leaves the resource
/// cache pointing at a dead entity, so the next command flush panicked far from
/// the request (hunt 6 finding 004). A caller cannot tell these entities apart
/// by id -- `Entity::to_bits` complements the index, so they sit at the top of
/// the id space counting down -- and `world.query` never lists them.
fn despawn_entity(
    In(params): In<Option<serde_json::Value>>,
    world: &mut World,
) -> bevy_remote::BrpResult {
    if let Some(entity) = named_entity(params.as_ref())
        && world
            .get_entity(entity)
            .is_ok_and(|found| found.contains::<bevy_ecs::resource::IsResource>())
    {
        return Err(bevy_remote::BrpError {
            code: bevy_remote::error_codes::RESOURCE_ERROR,
            message: format!("Entity {entity} holds a resource and cannot be despawned"),
            data: None,
        });
    }
    bevy_remote::builtin_methods::process_remote_despawn_entity_request(In(params), world)
}

use super::*;

#[derive(Component, bevy_reflect::Reflect)]
#[reflect(Component)]
struct Extra(u32);

/// A command that fails must report it and leave the server running.
/// Bevy's default routes a failed command to `panic`, which would let one
/// request end every session; `ServerPlugin` replaces that with logging.
#[test]
fn a_failing_command_is_reported_rather_than_fatal() -> crate::testing::Outcome {
    let mut app = App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.add_plugins((
        bevy_app::TaskPoolPlugin::default(),
        bevy_asset::AssetPlugin::default(),
        ServerPlugin,
    ));
    let configured: bevy_ecs::error::ErrorHandler = app
        .world()
        .resource::<bevy_ecs::error::FallbackErrorHandler>()
        .0;
    assert!(
        std::ptr::fn_addr_eq(
            configured,
            bevy_ecs::error::error as bevy_ecs::error::ErrorHandler
        ),
        "a failed command must be logged, not a panic"
    );
    // A command against an entity that is gone is the ordinary shape of
    // this: it fails, it is reported, and the next update still runs.
    let gone = app.world_mut().spawn_empty().id();
    app.world_mut().despawn(gone);
    app.world_mut().commands().entity(gone).insert(Extra(1));
    app.update();
    app.update();
    assert!(app.world().get_entity(gone).is_err());
    Ok(())
}

/// A BRP insert builds the component through `from_reflect_with_fallback`,
/// which panics on a partial payload unless the registration carries a
/// serde `Deserialize` (rejects it), a `Default` or a `FromWorld`. Every
/// reflected component must carry one, or one request kills the server.
#[test]
fn every_reflected_component_survives_a_partial_payload() -> crate::testing::Outcome {
    use bevy_ecs::reflect::{ReflectComponent, ReflectFromWorld};
    use bevy_reflect::{ReflectDeserialize, std_traits::ReflectDefault};
    let mut app = App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.add_plugins((
        bevy_app::TaskPoolPlugin::default(),
        bevy_asset::AssetPlugin::default(),
        ServerPlugin,
    ));
    let registry = app.world().resource::<AppTypeRegistry>().read();
    let unguarded: Vec<&str> = registry
        .iter()
        .filter(|registration| registration.data::<ReflectComponent>().is_some())
        .filter(|registration| {
            registration.data::<ReflectDeserialize>().is_none()
                && registration.data::<ReflectDefault>().is_none()
                && registration.data::<ReflectFromWorld>().is_none()
        })
        .map(|registration| registration.type_info().type_path())
        .collect();
    assert!(
        unguarded.is_empty(),
        "reflected components without Deserialize, Default or FromWorld: {unguarded:?}"
    );
    Ok(())
}

#[test]
fn removing_viewer_drops_presentation_without_despawning_entity() -> crate::testing::Outcome {
    let mut app = App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.add_plugins(ServerPlugin);
    let registry = app.world().resource::<AppTypeRegistry>().clone();
    let viewer = app
        .world_mut()
        .spawn((
            Viewer {
                rows: 24,
                cols: 80,
                zoom: false,
                scrollback: 0,
                notice: None,
            },
            Presentation::new(registry),
        ))
        .id();
    assert!(app.world().get::<Presentation>(viewer).is_some());
    app.world_mut().entity_mut(viewer).remove::<Viewer>();
    assert!(app.world().get_entity(viewer).is_ok());
    assert!(app.world().get::<Presentation>(viewer).is_none());
    app.world_mut().despawn(viewer);
    Ok(())
}

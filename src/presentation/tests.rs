use super::*;
use crate::{model::Workspace, testing::*};

#[test]
fn chrome_rectangles_keep_native_half_open_picking_and_empty_bounds() -> Outcome {
    let mut view = Presentation::new(AppTypeRegistry::default());
    view.viewport = UVec2::new(10, 9);
    view.world
        .get_mut::<Window>(view.window)
        .need()?
        .resolution
        .set_physical_resolution(10, 10);
    view.world
        .get_mut::<Camera>(view.camera)
        .need()?
        .computed
        .target_info = Some(RenderTargetInfo {
        physical_size: UVec2::new(10, 10),
        scale_factor: 1.0,
    });
    let target = Entity::from_bits(42);
    view.chrome(vec![(target, URect::new(2, 3, 5, 4))]);
    assert_eq!(view.pointer(2, 3, false), Some(target));
    assert_eq!(view.pointer(4, 3, false), Some(target));
    assert_eq!(view.pointer(5, 3, false), None);
    assert_eq!(view.pointer(1, 3, false), None);
    assert_eq!(view.pointer(2, 4, false), None);
    view.chrome(vec![(target, URect::new(2, 3, 2, 4))]);
    assert_eq!(view.pointer(2, 3, false), None);
    Ok(())
}

#[test]
fn extracted_world_matches_app_across_scene_resize_focus_and_removal() -> Outcome {
    #[derive(Message)]
    struct Probe;
    fn component<T: Component + Send + Sync>() {}
    component::<Presentation>();
    let mut source = App::new();
    source
        .register_type::<Workspace>()
        .register_type::<Tab>()
        .register_type::<PaneView>();
    register_types(&mut source);
    let registry = source.world().resource::<AppTypeRegistry>().clone();
    let (mut reference, window, camera, container, pointer) = Presentation::build(registry.clone());
    let (mut extracted, other_window, other_camera, other_container, other_pointer) =
        Presentation::build(registry);
    assert_eq!(reference.sub_apps().iter().count(), 1);
    assert_eq!(extracted.sub_apps().iter().count(), 1);
    assert_eq!(
        (window, camera, container, pointer),
        (other_window, other_camera, other_container, other_pointer)
    );
    reference.add_message::<Probe>();
    extracted.add_message::<Probe>();
    let mut world = std::mem::take(extracted.world_mut());
    assert!(world.storages().non_sends.is_empty());
    drop(extracted);

    let process = source.world_mut().spawn_empty().id();
    let root = source.world_mut().spawn(Workspace).id();
    let tab = source.world_mut().spawn((Tab, ChildOf(root))).id();
    let leaf = source
        .world_mut()
        .spawn((PaneView { pane: process }, ChildOf(tab)))
        .id();
    let mut maps = [EntityHashMap::default(), EntityHashMap::default()];
    for (destination, map) in [reference.world_mut(), &mut world]
        .into_iter()
        .zip(&mut maps)
    {
        map.insert(process, destination.spawn_empty().id());
    }
    let mut previous = Vec::new();
    for (step, (cols, rows)) in [(80, 24), (120, 40), (120, 40), (120, 40)]
        .into_iter()
        .enumerate()
    {
        if step == 2 {
            source.world_mut().get_mut::<Node>(leaf).need()?.width = Val::Px(31.0);
            source.world_mut().get_mut::<Node>(leaf).need()?.flex_grow = 0.0;
            source.world_mut().get_mut::<Node>(leaf).need()?.flex_basis = Val::Auto;
        }
        if step == 3 {
            source.world_mut().despawn(leaf);
        }
        let scene = crate::assets::extract_layout(source.world(), root)?;
        for (destination, map) in [reference.world_mut(), &mut world]
            .into_iter()
            .zip(&mut maps)
        {
            if let Some(local_root) = map.get(&root).copied() {
                destination.despawn(local_root);
            }
            let local_process = *map.get(&process).need()?;
            map.clear();
            map.insert(process, local_process);
            scene.write_to_world(destination, map)?;
            destination
                .entity_mut(*map.get(&root).need()?)
                .insert(ChildOf(container));
            destination
                .get_mut::<Window>(window)
                .need()?
                .resolution
                .set_physical_resolution(cols, rows);
            destination
                .get_mut::<Camera>(camera)
                .need()?
                .computed
                .target_info = Some(RenderTargetInfo {
                physical_size: UVec2::new(cols, rows),
                scale_factor: 1.0,
            });
            if let Some(local) = map.get(&leaf).copied() {
                destination
                    .entity_mut(local)
                    .insert((FocusPolicy::Block, TabIndex(0)));
                destination
                    .resource_mut::<InputFocus>()
                    .set(local, FocusCause::Navigated);
            } else {
                destination.resource_mut::<InputFocus>().clear();
            }
        }
        reference.update();
        world.run_schedule(bevy_app::Main);
        world.clear_trackers();
        let geometry = |world: &World| -> Vec<Vec2> {
            world
                .resource::<UiStack>()
                .uinodes
                .iter()
                .filter_map(|e| {
                    world.get::<PaneView>(*e)?;
                    Some(world.get::<ComputedNode>(*e)?.size())
                })
                .collect()
        };
        let actual = geometry(&world);
        assert_eq!(actual, geometry(reference.world()));
        if step < 3 {
            let expected_width = if step == 2 { 31.0 } else { cols as f32 };
            assert_eq!(actual, vec![Vec2::new(expected_width, rows as f32)]);
            assert_ne!(actual, previous);
            // The inert world must pick the same node as a real app does, so
            // both run the stock UI picking backend over one synthetic pointer.
            let target =
                NormalizedRenderTarget::Window(WindowRef::Primary.normalize(Some(window)).need()?);
            let picked =
                |destination: &mut World| -> Result<Vec<Entity>, Box<dyn std::error::Error>> {
                    destination.entity_mut(pointer).insert(PointerLocation {
                        location: Some(Location {
                            target: target.clone(),
                            position: Vec2::new(1.5, 1.5),
                        }),
                    });
                    destination
                        .resource_mut::<ButtonInput<MouseButton>>()
                        .press(MouseButton::Left);
                    destination.run_system_cached(ui_picking)?;
                    let hits: Vec<PointerHits> = destination
                        .resource_mut::<Messages<PointerHits>>()
                        .drain()
                        .collect();
                    Ok(hits
                        .iter()
                        .flat_map(|hit| hit.picks.iter())
                        .map(|(entity, _)| *entity)
                        .filter(|entity| destination.get::<PaneView>(*entity).is_some())
                        .collect())
                };
            let panes: Vec<_> = world
                .query_filtered::<Entity, With<PaneView>>()
                .iter(&world)
                .collect();
            assert_eq!(picked(&mut world)?, panes);
            assert_eq!(picked(reference.world_mut())?.len(), panes.len());
            // Focus still follows a press, through the same path `pointer` uses.
            if let Some(local) = panes.first() {
                world
                    .resource_mut::<InputFocus>()
                    .set(*local, FocusCause::Pressed);
                world.run_system_cached(process_recorded_focus_changes)?;
            }
            assert!(world.resource::<InputFocus>().get().is_some());
        } else {
            assert!(actual.is_empty());
            assert!(world.resource::<InputFocus>().get().is_none());
        }
        previous = actual;
        assert!(world.storages().non_sends.is_empty());
        assert_eq!(
            world
                .query_filtered::<Entity, Added<Node>>()
                .iter(&world)
                .count(),
            0
        );
        reference.update();
        world.run_schedule(bevy_app::Main);
        world.clear_trackers();
        assert_eq!(geometry(&world), geometry(reference.world()));
    }
    reference.world_mut().write_message(Probe);
    world.write_message(Probe);
    for _ in 0..2 {
        reference.update();
        world.run_schedule(bevy_app::Main);
        world.clear_trackers();
    }
    assert!(world.resource::<Messages<Probe>>().is_empty());
    assert!(reference.world().resource::<Messages<Probe>>().is_empty());
    Ok(())
}

// Only the viewed tab is written into the projection. Inactive tabs were only
// ever written to be hidden, and writing them made every rebuild cost O(tabs):
// 424 ms a move at a thousand tabs in fux-fuzz's `scale` on ubuntu-24.04.
#[test]
fn a_rebuild_writes_only_the_viewed_tab() -> Outcome {
    let mut source = App::new();
    source
        .register_type::<Workspace>()
        .register_type::<Tab>()
        .register_type::<PaneView>();
    register_types(&mut source);
    let registry = source.world().resource::<AppTypeRegistry>().clone();
    let root = source.world_mut().spawn((Workspace, Node::default())).id();
    let mut tabs = Vec::new();
    for _ in 0..5 {
        let process = source.world_mut().spawn_empty().id();
        let tab = source
            .world_mut()
            .spawn((Tab, Node::default(), ChildOf(root)))
            .id();
        let leaf = source
            .world_mut()
            .spawn((PaneView { pane: process }, Node::default(), ChildOf(tab)))
            .id();
        tabs.push((tab, leaf));
    }
    let scene = crate::assets::extract_layout(source.world(), root)?;
    let viewer = Viewer {
        rows: 24,
        cols: 80,
        zoom: false,
        scrollback: 0,
        notice: None,
    };
    let tab_count = |view: &mut Presentation| {
        view.world
            .query_filtered::<(), With<Tab>>()
            .iter(&view.world)
            .count()
    };
    let (tab, leaf) = *tabs.get(2).need()?;
    let mut view = Presentation::new(registry.clone());
    view.sync(&scene, 1, root, &viewer, (Some(tab), Some(leaf)))?;
    assert_eq!(tab_count(&mut view), 1);
    assert!(view.source_to_local.contains_key(&leaf));
    assert!(
        tabs.iter()
            .all(|(t, l)| *t == tab || !view.source_to_local.contains_key(l))
    );
    // A focus outside the viewed tab, which only raw edits produce, keeps the
    // whole scene rather than failing the frame.
    let (_, elsewhere) = *tabs.first().need()?;
    let mut view = Presentation::new(registry);
    view.sync(&scene, 1, root, &viewer, (Some(tab), Some(elsewhere)))?;
    assert_eq!(tab_count(&mut view), 5);
    Ok(())
}

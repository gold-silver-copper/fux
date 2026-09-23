use super::*;
use crate::{server::ServerPlugin, testing::*};
use bevy_app::{App, Update};
use bevy_ui::Node;

#[derive(Component, bevy_reflect::Reflect)]
#[reflect(Component)]
struct Extra(u32);

#[test]
fn scene_tasks_complete_on_deadline_and_report_io_failures() -> crate::testing::Outcome {
    fn settle(world: &mut World) -> crate::testing::Outcome {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pending_scenes(world) {
            if std::time::Instant::now() >= deadline {
                return Err("scene task did not complete".into());
            }
            world.run_system_cached(scene_completions)?;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(())
    }
    let mut app = App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.add_plugins((
        bevy_app::TaskPoolPlugin::default(),
        bevy_asset::AssetPlugin::default(),
        ServerPlugin,
    ));
    let world = app.world_mut();
    let root = world.spawn(Workspace).id();
    world.spawn((Tab, ChildOf(root)));
    let id = world
        .spawn((
            Viewer {
                rows: 24,
                cols: 80,
                zoom: false,
                scrollback: 0,
                notice: None,
            },
            Viewing(root),
        ))
        .id();
    let path = std::env::temp_dir().join(format!("fux-scene-task-{}.ron", std::process::id()));
    let path = path.to_str().need()?.to_owned();
    scene_io(world, id, root, path.clone(), None);
    settle(world)?;
    assert_eq!(
        world.get::<Viewer>(id).need()?.notice,
        Notice::info(format!("saved {path}"))
    );
    scene_io(world, id, root, path.clone(), Some(Vec::new()));
    settle(world)?;
    assert_eq!(
        world.get::<Viewer>(id).need()?.notice,
        Notice::info(format!("loaded {path}"))
    );
    let root = viewing(world, id).need()?;
    std::fs::remove_file(&path)?;
    let expected = std::fs::read_to_string(&path).err().need()?.to_string();
    scene_io(world, id, root, path, Some(Vec::new()));
    settle(world)?;
    assert_eq!(
        world.get::<Viewer>(id).need()?.notice,
        Notice::error(expected)
    );
    // Completion commands see the pending marker already removed.
    let task = bevy_tasks::IoTaskPool::get().spawn(async move {
        let mut queue = CommandQueue::default();
        queue.push(move |world: &mut World| {
            assert!(world.get::<PendingScene>(id).is_none());
            world.entity_mut(id).insert(Name::new("completed once"));
        });
        queue
    });
    world.entity_mut(id).insert(PendingScene(task));
    settle(world)?;
    assert_eq!(world.get::<Name>(id).need()?.as_str(), "completed once");
    world.run_system_cached(scene_completions)?;
    Ok(())
}

#[test]
fn detaching_during_synchronous_scene_io_finishes_the_operation() -> crate::testing::Outcome {
    let mut app = App::new();
    app.add_plugins(bevy_app::TaskPoolPlugin::default());
    let (started, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let (done, completed) = std::sync::mpsc::channel();
    let path = std::env::temp_dir().join(format!("fux-detached-scene-{}.ron", std::process::id()));
    let destination = path.clone();
    let task = bevy_tasks::IoTaskPool::get().spawn(async move {
        let _ = started.send(());
        // Like scene_io's filesystem call, this interval has no await point.
        let _ = wait.recv();
        let _ = done.send(std::fs::write(destination, "completed"));
        CommandQueue::default()
    });
    let id = app.world_mut().spawn(PendingScene(task)).id();
    ready.recv_timeout(std::time::Duration::from_secs(5))?;
    app.world_mut().despawn(id);
    release.send(())?;
    completed.recv_timeout(std::time::Duration::from_secs(5))??;
    assert_eq!(std::fs::read_to_string(&path)?, "completed");
    std::fs::remove_file(path)?;
    assert!(!pending_scenes(app.world_mut()));
    Ok(())
}

#[test]
fn arbitrary_layout_changes_invalidate_only_the_owning_workspace() -> crate::testing::Outcome {
    let mut app = App::new();
    app.register_type::<Workspace>()
        .register_type::<Name>()
        .register_type::<Node>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<Extra>()
        .add_systems(Update, invalidate_layouts);
    let left = app.world_mut().spawn(Workspace).id();
    let right = app.world_mut().spawn(Workspace).id();
    let nested = app.world_mut().spawn((Node::default(), ChildOf(left))).id();
    let leaf = app
        .world_mut()
        .spawn((Name::new("before"), ChildOf(nested)))
        .id();
    app.update();
    let untouched = scene(app.world_mut(), right)?.1;
    let initial = scene(app.world_mut(), left)?.1;
    app.update();
    assert!(Arc::ptr_eq(&initial, &scene(app.world_mut(), left)?.1));
    // The descendant deliberately has no Node. Newly inserted, previously
    // absent types and ordinary in-place writes must still reach the scene.
    app.world_mut().entity_mut(leaf).insert(Extra(7));
    // A synchronous control can observe this mutation before another Update.
    app.world_mut().run_system_cached(invalidate_layouts)?;
    let inserted = scene(app.world_mut(), left)?.1;
    assert!(!Arc::ptr_eq(&initial, &inserted));
    assert!(Arc::ptr_eq(&untouched, &scene(app.world_mut(), right)?.1));
    app.world_mut().get_mut::<Extra>(leaf).need()?.0 = 9;
    app.update();
    let modified = scene(app.world_mut(), left)?.1;
    assert!(!Arc::ptr_eq(&inserted, &modified));
    app.world_mut().entity_mut(leaf).remove::<Extra>();
    app.update();
    let removed = scene(app.world_mut(), left)?.1;
    assert!(!Arc::ptr_eq(&modified, &removed));
    assert!(Arc::ptr_eq(&untouched, &scene(app.world_mut(), right)?.1));
    app.world_mut().entity_mut(nested).insert(ChildOf(right));
    app.update();
    let emptied = scene(app.world_mut(), left)?.1;
    let moved = scene(app.world_mut(), right)?.1;
    assert!(!Arc::ptr_eq(&removed, &emptied));
    assert!(!Arc::ptr_eq(&untouched, &moved));
    assert!(!emptied.entities.iter().any(|entity| entity.entity == leaf));
    assert!(moved.entities.iter().any(|entity| entity.entity == leaf));
    app.world_mut().despawn(nested);
    app.update();
    let despawned = scene(app.world_mut(), right)?.1;
    assert!(
        !despawned
            .entities
            .iter()
            .any(|entity| entity.entity == leaf)
    );
    assert!(Arc::ptr_eq(&emptied, &scene(app.world_mut(), left)?.1));
    app.world_mut().entity_mut(right).remove::<Workspace>();
    assert!(scene(app.world_mut(), right).is_err());
    app.world_mut().despawn(right);
    let replacement = app.world_mut().spawn(Workspace).id();
    app.update();
    assert_eq!(scene(app.world_mut(), replacement)?.1.entities.len(), 1);
    Ok(())
}

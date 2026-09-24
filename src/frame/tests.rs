use super::*;
use crate::testing::*;

#[test]
fn sync_uses_viewer_state_after_focus_repair_resets_zoom() -> Outcome {
    let mut app = bevy_app::App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.add_plugins(crate::server::ServerPlugin);
    let world = app.world_mut();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let process = world.spawn_empty().id();
    let hidden = world
        .spawn((
            PaneView { pane: process },
            ChildOf(tab),
            bevy_camera::visibility::Visibility::Hidden,
        ))
        .id();
    let visible = world.spawn((PaneView { pane: process }, ChildOf(tab))).id();
    let id = world
        .spawn((
            Viewer {
                rows: 24,
                cols: 80,
                zoom: true,
                scrollback: 7,
                notice: None,
            },
            Viewing(root),
            OnTab(tab),
            Focused(hidden),
        ))
        .id();
    sync_view(world, id)?;
    let viewer = world.get::<Viewer>(id).need()?;
    assert!(!viewer.zoom);
    assert_eq!(viewer.scrollback, 0);
    assert!(
        world
            .get::<Presentation>(id)
            .need()?
            .rects()
            .iter()
            .any(|rect| rect.leaf == visible)
    );
    Ok(())
}

#[test]
fn attach_preserves_permissive_defaults_and_clamps_before_narrowing() -> Outcome {
    let mut app = bevy_app::App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.add_plugins(crate::server::ServerPlugin);
    let root = app.world_mut().spawn((Workspace, Name::new("main"))).id();
    app.world_mut().spawn((Tab, ChildOf(root)));
    for (params, expected) in [
        (None, (24, 80)),
        (Some(Value::Null), (24, 80)),
        (Some(json!(false)), (24, 80)),
        (Some(json!(7)), (24, 80)),
        (Some(json!("not an object")), (24, 80)),
        (Some(json!([])), (24, 80)),
        (Some(json!({})), (24, 80)),
        (
            Some(json!({"rows":null,"cols":null,"workspace":null})),
            (24, 80),
        ),
        (Some(json!({"rows":-1,"cols":3.5,"workspace":7})), (24, 80)),
        (
            Some(json!({"rows":"12","cols":false,"unknown":true})),
            (24, 80),
        ),
        (Some(json!({"rows":0,"cols":0})), (0, 0)),
        (Some(json!({"rows":u64::MAX,"cols":65536})), (4096, 4096)),
        (
            Some(json!({"workspace":"main","rows":12,"cols":32})),
            (12, 32),
        ),
        (Some(json!({"rows":1})), (1, 80)),
        (Some(json!({"cols":1})), (24, 1)),
    ] {
        let response = attach(In(params), app.world_mut()).map_err(|e| e.message)?;
        let id = Entity::from_bits(response.get("viewer").and_then(Value::as_u64).need()?);
        let v = app.world().get::<Viewer>(id).need()?;
        assert_eq!((v.rows, v.cols), expected);
        app.world_mut().despawn(id);
    }
    let error = attach(In(Some(json!({"workspace":"missing"}))), app.world_mut())
        .err()
        .need()?;
    assert_eq!(error.message, "workspace not found");
    Ok(())
}

#[test]
fn a_zoomed_pane_taken_out_of_the_scene_paints_the_tab_instead_of_failing() -> Outcome {
    let mut app = bevy_app::App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.insert_resource(crate::assets::Settings::default());
    app.add_plugins(crate::server::ServerPlugin);
    let world = app.world_mut();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let process = world.spawn_empty().id();
    let a = world.spawn((PaneView { pane: process }, ChildOf(tab))).id();
    let b = world.spawn((PaneView { pane: process }, ChildOf(tab))).id();
    let id = world
        .spawn((
            Viewer {
                rows: 24,
                cols: 80,
                zoom: true,
                scrollback: 0,
                notice: None,
            },
            Viewing(root),
            OnTab(tab),
            Focused(a),
        ))
        .id();
    sync_view(world, id)?;
    assert!(world.get::<Viewer>(id).need()?.zoom);
    // Unlinked from its tab, the focused pane is no longer in the scene,
    // yet it still exists so no relationship hook repairs the focus.
    world.entity_mut(a).remove::<ChildOf>();
    let frame = make_frame(world, id)?;
    assert!(!frame.detach);
    assert!(!world.get::<Viewer>(id).need()?.zoom);
    assert_eq!(focused(world, id), Some(b));
    // The same when the focused entity stops being a pane view.
    world.entity_mut(id).insert(Focused(b));
    world.get_mut::<Viewer>(id).need()?.zoom = true;
    world.entity_mut(b).remove::<PaneView>();
    let frame = make_frame(world, id)?;
    assert!(!frame.detach);
    assert!(!world.get::<Viewer>(id).need()?.zoom);
    Ok(())
}

#[test]
fn one_viewer_without_a_layout_does_not_fail_another_viewer_frame() -> Outcome {
    let mut app = bevy_app::App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.insert_resource(crate::assets::Settings::default());
    app.add_plugins(crate::server::ServerPlugin);
    let world = app.world_mut();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let process = world.spawn_empty().id();
    let pane = world.spawn((PaneView { pane: process }, ChildOf(tab))).id();
    let healthy = world
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
    let broken = world
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
    // The broken viewer's workspace loses its Workspace component. Its
    // viewers are repaired onto the surviving workspace, and even a viewer
    // that cannot be projected costs nobody else a frame.
    let other = world.spawn(Workspace).id();
    world.spawn((Tab, ChildOf(other)));
    world.entity_mut(broken).insert(Viewing(other));
    world.entity_mut(other).remove::<Workspace>();
    world.flush();
    assert_eq!(viewing(world, broken), Some(root));
    let frame = make_frame(world, healthy);
    assert!(frame.is_ok(), "{frame:?}");
    let frame = make_frame(world, broken)?;
    assert!(!frame.detach);
    // A projection failure paints the failure in the bar.
    let degraded = degraded(world, broken, "layout root is not a workspace");
    assert!(degraded.paint.contains("layout root is not a workspace"));
    let _ = (pane, tab);
    Ok(())
}

// The frame that resizes a pane's PTY publishes its size in the same step, as
// `terminate` publishes an ended process: a query right after the frame must
// not read the size from before it. The sync system used to republish it only
// on the next update, so a test querying straight after a paint raced it.
#[test]
fn a_frame_publishes_the_pty_size_it_sets() -> Outcome {
    let mut app = bevy_app::App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.insert_resource(crate::assets::Settings::default());
    app.add_plugins(crate::server::ServerPlugin);
    let world = app.world_mut();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let terminal = Terminal::for_test(fux_vt::Parser::new(24, 80, 0)?);
    let published = terminal.state();
    let process = world.spawn((terminal, published)).id();
    let leaf = world.spawn((PaneView { pane: process }, ChildOf(tab))).id();
    let id = world
        .spawn((
            Viewer {
                rows: 12,
                cols: 40,
                zoom: false,
                scrollback: 0,
                notice: None,
            },
            Viewing(root),
            OnTab(tab),
            Focused(leaf),
        ))
        .id();
    make_frame(world, id)?;
    let size = world.get::<Terminal>(process).need()?.screen().size();
    assert_eq!(size, (11, 40));
    let state = world.get::<ProcessState>(process).need()?;
    assert_eq!((state.rows, state.cols), size);
    Ok(())
}

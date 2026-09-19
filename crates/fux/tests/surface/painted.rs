use super::*;
use fux::model::ProjectionBaseline;

fn revision(app: &App, viewer: Entity) -> u64 {
    app.world()
        .get::<ProjectionBaseline>(viewer)
        .unwrap()
        .scene_revision
}

fn click_at(revision: u64, row: u16) -> ViewerRequest {
    ViewerRequest::Pointer(PointerEvent {
        revision,
        col: 45,
        row,
        kind: PointerKind::Press,
        button: PointerButton::Left,
        modifiers: 0,
    })
}

#[test]
fn delayed_click_and_key_cannot_name_the_row_now_occupying_the_old_cell() {
    let mut app = app();
    let (ws, _, _, leaf) = opened(&mut app);
    let viewer =
        ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    let mut provider = Provider::new(&app);
    let (column, rows) = provider.column(&["A", "B"]);
    let delta = provider.export_all();
    surface::update(app.world_mut(), leaf, 1, true, &delta).unwrap();
    app.update();
    mouse(&mut app, viewer, 45, 0, PointerKind::Move);
    let painted = revision(&app, viewer);
    let surface_id = NodeId(node_id(app.world(), leaf));

    provider
        .world
        .entity_mut(column)
        .replace_children(&[rows[1], rows[0]]);
    let delta = provider.export_all();
    surface::update(app.world_mut(), leaf, 2, true, &delta).unwrap();
    // The old projection is still the latest one: admission must also check pending layout.
    request(&mut app, viewer, click_at(painted, 0));
    assert!(surface_inputs(app.world()).is_empty());
    // The reordered scene has now been projected but not painted by this viewer.
    request(&mut app, viewer, click_at(painted, 0));
    request(
        &mut app,
        viewer,
        ViewerRequest::SurfaceKey {
            revision: painted,
            node: surface_id,
            bytes: b"s".to_vec(),
        },
    );
    assert!(surface_inputs(app.world()).is_empty());

    mouse(&mut app, viewer, 45, 0, PointerKind::Move);
    let current = revision(&app, viewer);
    request(&mut app, viewer, click_at(current, 0));
    let inputs = surface_inputs(app.world());
    let row_b = app
        .world()
        .get::<SurfaceState>(leaf)
        .unwrap()
        .entity(rows[1])
        .unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["node"], json!(node_id(app.world(), row_b)));
    assert_eq!(inputs[0]["kind"], json!("Press"));
}

#[test]
fn queued_surface_key_cannot_cross_provider_replacement() {
    let mut app = app();
    let (ws, _, _, leaf) = opened(&mut app);
    let viewer =
        ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    app.update();
    let old = revision(&app, viewer);
    let node = NodeId(node_id(app.world(), leaf));
    surface::close(app.world_mut(), leaf).unwrap();
    surface::open(app.world_mut(), leaf, "replacement").unwrap();
    request(
        &mut app,
        viewer,
        ViewerRequest::SurfaceKey {
            revision: old,
            node,
            bytes: b"s".to_vec(),
        },
    );
    assert!(surface_inputs(app.world()).is_empty());
    let current = revision(&app, viewer);
    request(
        &mut app,
        viewer,
        ViewerRequest::SurfaceKey {
            revision: current,
            node,
            bytes: b"j".to_vec(),
        },
    );
    let inputs = surface_inputs(app.world());
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["bytes"], json!([106]));
}

#[test]
fn scroll_and_resize_refuse_old_clicks_without_invalidating_another_viewer() {
    let mut app = app();
    let (ws, _, _, leaf) = opened(&mut app);
    let a = ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 4, cols: 80 }, None).unwrap();
    let b = ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 4, cols: 80 }, None).unwrap();
    let mut provider = Provider::new(&app);
    let (column, rows) = provider.column(&["A", "B", "C", "D", "E", "F"]);
    for row in rows {
        provider.world.get_mut::<Node>(row).unwrap().flex_shrink = 0.0;
    }
    provider.world.get_mut::<Node>(column).unwrap().overflow = bevy_ui::Overflow::scroll_y();
    let delta = provider.export_all();
    surface::update(app.world_mut(), leaf, 1, true, &delta).unwrap();
    app.update();
    app.update();
    let old_a = revision(&app, a);
    let old_b = revision(&app, b);
    let template_column = app
        .world()
        .get::<SurfaceState>(leaf)
        .unwrap()
        .entity(column)
        .unwrap();
    ops::scroll(app.world_mut(), a, template_column, 1).unwrap();
    request(&mut app, a, click_at(old_a, 0));
    assert!(surface_inputs(app.world()).is_empty());
    let node = NodeId(node_id(app.world(), leaf));
    request(
        &mut app,
        b,
        ViewerRequest::SurfaceKey {
            revision: old_b,
            node,
            bytes: b"j".to_vec(),
        },
    );
    let inputs = surface_inputs(app.world());
    assert_eq!(inputs.len(), 1);
    assert_eq!(
        inputs[0]["viewer"],
        json!(app.world().get::<ViewerId>(b).unwrap().0)
    );
    let before_resize = revision(&app, a);
    ops::resize_viewer(app.world_mut(), a, Viewport { rows: 6, cols: 60 }).unwrap();
    request(&mut app, a, click_at(before_resize, 0));
    assert_eq!(surface_inputs(app.world()).len(), 1);
}

#[test]
fn terminal_output_frames_preserve_surface_admission_and_pty_typing() {
    let mut app = app();
    let (ws, _, pane_leaf, leaf) = opened(&mut app);
    let viewer =
        ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    let pane = app.world().get::<fux::model::Places>(pane_leaf).unwrap().0;
    app.update();
    app.world_mut()
        .write_message(Inbound::PaneSpawned { pane, pid: 7 });
    app.update();
    app.update();
    let painted = revision(&app, viewer);
    for _ in 0..3 {
        app.world_mut().write_message(Inbound::PaneOutput {
            pane,
            bytes: b"busy output\r\n".to_vec(),
        });
        app.update();
    }
    let node = NodeId(node_id(app.world(), leaf));
    request(
        &mut app,
        viewer,
        ViewerRequest::SurfaceKey {
            revision: painted,
            node,
            bytes: b"j".to_vec(),
        },
    );
    let inputs = surface_inputs(app.world());
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["bytes"], json!([106]));
    app.world_mut()
        .resource_mut::<Messages<Effect>>()
        .drain()
        .for_each(drop);
    request(&mut app, viewer, ViewerRequest::Input(b"x".to_vec()));
    assert!(app.world_mut().resource_mut::<Messages<Effect>>().drain().any(|effect| {
        matches!(effect, Effect::WritePty { pane: target, bytes } if target == pane && bytes == b"x")
    }));
}

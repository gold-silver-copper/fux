use super::*;

#[test]
fn painted_pointer_never_writes_a_different_exact_attachment_pane() {
    let mut app = app();
    let (ws, _, left, right) = two_pane_row(app.world_mut());
    let left_pane = app.world().get::<Places>(left).unwrap().0;
    let right_pane = app.world().get::<Places>(right).unwrap().0;
    let viewer = ops::attach_viewer(app.world_mut(), ws, viewport(24, 80), None).unwrap();
    app.update();
    for pane in [left_pane, right_pane] {
        app.world_mut().write_message(Inbound::PaneSpawned { pane, pid: 7 });
    }
    app.update();
    for pane in [left_pane, right_pane] {
        app.world_mut().write_message(Inbound::PaneOutput {
            pane, bytes: b"\x1b[?1002h\x1b[?1006h".to_vec(),
        });
    }
    ops::target(app.world_mut(), viewer, left_pane).unwrap();
    app.world_mut().entity_mut(viewer).insert(ExactTarget);
    app.update();
    drain_effects(&mut app);
    hover(&mut app, viewer, 45, 5);
    mouse(&mut app, viewer, 45, 5, PointerKind::Press, PointerButton::Left, 0);
    assert!(pty_writes(&drain_effects(&mut app), right_pane).is_empty());
    mouse(&mut app, viewer, 45, 5, PointerKind::ScrollDown, PointerButton::None, 0);
    assert_eq!(app.world().get::<Targets>(viewer).unwrap().0, left_pane);
    assert!(pty_writes(&drain_effects(&mut app), right_pane).is_empty());
    mouse(&mut app, viewer, 45, 5, PointerKind::Release, PointerButton::Left, 0);
    assert!(pty_writes(&drain_effects(&mut app), right_pane).is_empty());
    hover(&mut app, viewer, 5, 5);
    mouse(&mut app, viewer, 5, 5, PointerKind::Press, PointerButton::Left, 0);
    assert_eq!(pty_writes(&drain_effects(&mut app), left_pane), vec![b"\x1b[<0;5;5M".to_vec()]);
}

#[test]
fn stale_release_cancels_a_held_pointer_without_writing_the_new_scene() {
    let (mut app, viewer, pane) = live_mouse_pane(b"\x1b[?1002h\x1b[?1006h");
    hover(&mut app, viewer, 10, 5);
    let revision = app.world().get::<ProjectionBaseline>(viewer).unwrap().scene_revision;
    mouse(&mut app, viewer, 10, 5, PointerKind::Press, PointerButton::Left, 0);
    drain_effects(&mut app);
    ops::resize_viewer(app.world_mut(), viewer, viewport(30, 90)).unwrap();
    app.world_mut().write_message(Inbound::ViewerRequest {
        viewer,
        request: ViewerRequest::Pointer(PointerEvent {
            revision, col: 10, row: 5, kind: PointerKind::Release,
            button: PointerButton::Left, modifiers: 0,
        }),
    });
    app.update();
    let pointer = pointer_entity(app.world(), viewer);
    assert!(!app.world().get::<bevy_picking::pointer::PointerPress>(pointer).unwrap().is_any_pressed());
    assert!(pty_writes(&drain_effects(&mut app), pane).is_empty());
}

#[test]
fn captured_border_drag_keeps_applying_steps_after_its_own_layout_changes() {
    let mut app = app();
    let (ws, _, left, right) = two_pane_row(app.world_mut());
    let viewer = ops::attach_viewer(app.world_mut(), ws, viewport(24, 80), None).unwrap();
    app.update();
    hover(&mut app, viewer, 39, 10);
    mouse(&mut app, viewer, 39, 10, PointerKind::Press, PointerButton::Left, 0);
    mouse(&mut app, viewer, 44, 10, PointerKind::Move, PointerButton::Left, 0);
    assert_eq!(instance_size(&mut app, viewer, left), UVec2::new(45, 24));
    assert_eq!(instance_size(&mut app, viewer, right), UVec2::new(35, 24));
    // The pressed instance has been replaced, but the captured template pair remains valid.
    mouse(&mut app, viewer, 49, 10, PointerKind::Move, PointerButton::Left, 0);
    assert_eq!(instance_size(&mut app, viewer, left), UVec2::new(50, 24));
    assert_eq!(instance_size(&mut app, viewer, right), UVec2::new(30, 24));
    mouse(&mut app, viewer, 49, 10, PointerKind::Release, PointerButton::Left, 0);
    assert!(app.world().get::<PointerDrag>(pointer_entity(app.world(), viewer)).is_none());
}

#[test]
fn stale_move_cancels_a_resize_instead_of_recapturing_the_replacement_scene() {
    let mut app = app();
    let (ws, _, left, right) = two_pane_row(app.world_mut());
    let viewer = ops::attach_viewer(app.world_mut(), ws, viewport(24, 80), None).unwrap();
    app.update();
    hover(&mut app, viewer, 39, 10);
    mouse(&mut app, viewer, 39, 10, PointerKind::Press, PointerButton::Left, 0);
    mouse(&mut app, viewer, 44, 10, PointerKind::Move, PointerButton::Left, 0);
    assert_eq!(instance_size(&mut app, viewer, left), UVec2::new(45, 24));
    let revision = app.world().get::<ProjectionBaseline>(viewer).unwrap().scene_revision;
    ops::resize_viewer(app.world_mut(), viewer, viewport(24, 100)).unwrap();
    app.update();
    let left_size = instance_size(&mut app, viewer, left);
    let right_size = instance_size(&mut app, viewer, right);
    app.world_mut().write_message(Inbound::ViewerRequest {
        viewer,
        request: ViewerRequest::Pointer(PointerEvent {
            revision, col: 60, row: 10, kind: PointerKind::Move,
            button: PointerButton::Left, modifiers: 0,
        }),
    });
    app.update();
    check(&mut app);
    let pointer = pointer_entity(app.world(), viewer);
    assert!(app.world().get::<PointerDrag>(pointer).is_none());
    assert!(!app.world().get::<bevy_picking::pointer::PointerPress>(pointer).unwrap().is_any_pressed());
    // Even a freshly stamped movement cannot revive the cancelled capture.
    mouse(&mut app, viewer, 70, 10, PointerKind::Move, PointerButton::Left, 0);
    mouse(&mut app, viewer, 70, 10, PointerKind::Release, PointerButton::Left, 0);
    assert_eq!(instance_size(&mut app, viewer, left), left_size);
    assert_eq!(instance_size(&mut app, viewer, right), right_size);
}

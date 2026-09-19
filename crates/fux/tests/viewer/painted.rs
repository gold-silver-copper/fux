use super::*;
use fux::viewer::replicate::PaintedSceneRevision;

#[test]
fn mixed_terminal_and_replacement_frames_keep_old_surface_key_provenance() {
    let mut scene = ServerScene::beside_surface();
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    viewer::update(&mut app);
    chord(&mut app, 'l');
    take_requests(&mut app);

    let old = scene.leaves[1];
    let child = scene
        .world
        .get::<Children>(old)
        .unwrap()
        .iter()
        .next()
        .unwrap();
    scene.world.entity_mut(old).despawn();
    let mut removed = scene.frame(2);
    removed.despawned = vec![old.to_bits(), child.to_bits()];
    viewer::queue_terminal(app.world_mut(), key(TKey::Char('s'), Modifiers::NONE), 1);
    push_frame(&mut app, removed);
    viewer::update(&mut app);
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::SurfaceKey {
            revision: 1,
            node: NodeId(3),
            bytes: b"s".to_vec(),
        }]
    );
    assert_eq!(app.world().resource::<PaintedSceneRevision>().0, 2);

    // A reader event left behind by MAX_BATCH is still old even though a new pane is focused.
    viewer::queue_terminal(app.world_mut(), key(TKey::Char('x'), Modifiers::NONE), 1);
    viewer::update(&mut app);
    assert!(take_requests(&mut app).is_empty());
    viewer::queue_terminal(app.world_mut(), key(TKey::Char('y'), Modifiers::NONE), 2);
    viewer::update(&mut app);
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Input(b"y".to_vec())]
    );
}

#[test]
fn continuous_surface_input_does_not_defer_new_paints() {
    let scene = ServerScene::beside_surface();
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    viewer::update(&mut app);
    chord(&mut app, 'l');
    take_requests(&mut app);
    for revision in 1..=8 {
        viewer::queue_terminal(
            app.world_mut(),
            key(TKey::Char('j'), Modifiers::NONE),
            revision,
        );
        push_frame(&mut app, scene.frame(revision + 1));
        viewer::update(&mut app);
        assert_eq!(
            take_requests(&mut app),
            vec![ViewerRequest::SurfaceKey {
                revision,
                node: NodeId(3),
                bytes: b"j".to_vec(),
            }]
        );
        assert_eq!(
            app.world().resource::<PaintedSceneRevision>().0,
            revision + 1
        );
        assert!(app.world().resource::<Inbox>().frames.is_empty());
    }
}

#[test]
fn output_only_frames_do_not_discard_queued_terminal_or_surface_typing() {
    let scene = ServerScene::beside_surface();
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    viewer::update(&mut app);
    take_requests(&mut app);
    push_frame(
        &mut app,
        SceneFrame {
            revision: 2,
            target: Some(PaneId(1)),
            showing: Some(NodeId(1)),
            terminals: vec![delta(1, 40, 23, "busy output")],
            ..Default::default()
        },
    );
    viewer::update(&mut app);
    take_requests(&mut app);
    viewer::queue_terminal(app.world_mut(), key(TKey::Char('x'), Modifiers::NONE), 1);
    viewer::update(&mut app);
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Input(b"x".to_vec())]
    );
    chord(&mut app, 'l');
    take_requests(&mut app);
    viewer::queue_terminal(app.world_mut(), key(TKey::Char('j'), Modifiers::NONE), 1);
    viewer::update(&mut app);
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::SurfaceKey {
            revision: 1,
            node: NodeId(3),
            bytes: b"j".to_vec(),
        }]
    );
}

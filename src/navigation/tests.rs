use super::*;

fn viewer(world: &mut World, workspace: Entity) -> Entity {
    world
        .spawn(Viewer {
            workspace,
            tab: None,
            focus: None,
            rows: 24,
            cols: 80,
            zoom: false,
            scrollback: 0,
            notice: String::new(),
            notice_error: false,
            help_scroll: 0,
            prefix: false,
        })
        .id()
}
fn leaf(world: &mut World, parent: Entity) -> Entity {
    let pane = world.spawn_empty().id();
    world.spawn((PaneView { pane }, ChildOf(parent))).id()
}

#[test]
fn legacy_children_are_wrapped_without_process_ownership_or_node_changes() {
    let mut world = World::new();
    let root = world
        .spawn((
            Workspace,
            Node {
                column_gap: Val::Px(7.0),
                ..Default::default()
            },
        ))
        .id();
    let leaf = leaf(&mut world, root);
    let process = world.get::<PaneView>(leaf).unwrap().pane;
    normalize_workspace(&mut world, root);
    let tab = tabs(&world, root)[0];
    assert_eq!(world.get::<ChildOf>(leaf).unwrap().parent(), tab);
    assert_eq!(world.get::<Node>(tab).unwrap().column_gap, Val::Px(7.0));
    normalize_workspace(&mut world, root);
    assert_eq!(tabs(&world, root), vec![tab]);
    world.despawn(root);
    assert!(world.get_entity(process).is_ok());
}

#[test]
fn viewers_remember_independent_tabs_focus_and_last_focus() {
    let mut world = World::new();
    let root = world.spawn(Workspace).id();
    let first = world.spawn((Tab, ChildOf(root))).id();
    let second = world.spawn((Tab, ChildOf(root))).id();
    let a = leaf(&mut world, first);
    let b = leaf(&mut world, first);
    let c = leaf(&mut world, second);
    let left = viewer(&mut world, root);
    let right = viewer(&mut world, root);
    repair(&mut world);
    world.get_mut::<Viewer>(left).unwrap().focus = Some(b);
    repair(&mut world);
    control(&mut world, left, Action::TabNext, None, "").unwrap();
    assert_eq!(world.get::<Viewer>(left).unwrap().focus, Some(c));
    assert_eq!(world.get::<Viewer>(right).unwrap().focus, Some(a));
    control(&mut world, left, Action::TabPrevious, None, "").unwrap();
    assert_eq!(world.get::<Viewer>(left).unwrap().focus, Some(b));
    control(&mut world, left, Action::FocusLast, None, "").unwrap();
    assert_eq!(world.get::<Viewer>(left).unwrap().focus, Some(a));
    control(&mut world, left, Action::FocusLast, None, "").unwrap();
    assert_eq!(world.get::<Viewer>(left).unwrap().focus, Some(b));
    world.despawn(b);
    repair(&mut world);
    assert_eq!(world.get::<Viewer>(left).unwrap().focus, Some(a));
    world.despawn(first);
    repair(&mut world);
    assert_eq!(world.get::<Viewer>(left).unwrap().focus, Some(c));
    assert_eq!(world.get::<Viewer>(right).unwrap().focus, Some(c));
}

use super::*;
use crate::testing::*;

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
        })
        .id()
}
fn leaf(world: &mut World, parent: Entity) -> Entity {
    let pane = world.spawn_empty().id();
    world.spawn((PaneView { pane }, ChildOf(parent))).id()
}

#[test]
fn legacy_children_are_wrapped_without_process_ownership_or_node_changes() -> crate::testing::Outcome
{
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
    let process = world.get::<PaneView>(leaf).need()?.pane;
    normalize_workspace(&mut world, root);
    let tab = *tabs(&world, root).first().need()?;
    assert_eq!(world.get::<ChildOf>(leaf).need()?.parent(), tab);
    assert_eq!(world.get::<Node>(tab).need()?.column_gap, Val::Px(7.0));
    normalize_workspace(&mut world, root);
    assert_eq!(tabs(&world, root), vec![tab]);
    world.despawn(root);
    assert!(world.get_entity(process).is_ok());
    Ok(())
}

#[test]
fn viewers_remember_independent_tabs_focus_and_last_focus() -> crate::testing::Outcome {
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
    world.get_mut::<Viewer>(left).need()?.focus = Some(b);
    repair(&mut world);
    control(&mut world, left, Action::TabNext, None, "")?;
    assert_eq!(world.get::<Viewer>(left).need()?.focus, Some(c));
    assert_eq!(world.get::<Viewer>(right).need()?.focus, Some(a));
    control(&mut world, left, Action::TabPrevious, None, "")?;
    assert_eq!(world.get::<Viewer>(left).need()?.focus, Some(b));
    control(&mut world, left, Action::FocusLast, None, "")?;
    assert_eq!(world.get::<Viewer>(left).need()?.focus, Some(a));
    control(&mut world, left, Action::FocusLast, None, "")?;
    assert_eq!(world.get::<Viewer>(left).need()?.focus, Some(b));
    world.despawn(b);
    repair(&mut world);
    assert_eq!(world.get::<Viewer>(left).need()?.focus, Some(a));
    world.despawn(first);
    repair(&mut world);
    assert_eq!(world.get::<Viewer>(left).need()?.focus, Some(c));
    assert_eq!(world.get::<Viewer>(right).need()?.focus, Some(c));
    Ok(())
}

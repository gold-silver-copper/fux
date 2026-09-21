use super::*;
use crate::testing::*;
use bevy_ui::Val;

fn viewer(world: &mut World, workspace: Entity) -> Entity {
    world
        .spawn((
            Viewer {
                rows: 24,
                cols: 80,
                zoom: false,
                scrollback: 0,
                notice: None,
            },
            Viewing(workspace),
        ))
        .id()
}
fn focus_of(world: &World, viewer: Entity) -> Option<Entity> {
    focused(world, viewer)
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
fn relationship_hooks_repair_without_observer_registration() -> Outcome {
    let mut world = World::new();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let a = leaf(&mut world, tab);
    let b = leaf(&mut world, tab);
    let id = viewer(&mut world, root);
    assert_eq!(on_tab(&world, id), Some(tab));
    assert_eq!(focused(&world, id), Some(a));
    world.entity_mut(id).insert(Focused(b));
    assert_eq!(world.get::<Memory>(id).need()?.previous.get(&tab), Some(&a));
    world.entity_mut(id).remove::<Focused>();
    assert_eq!(focused(&world, id), Some(b));
    world.entity_mut(id).insert(OnTab(a));
    assert_eq!(on_tab(&world, id), Some(tab));
    world.entity_mut(id).insert(Viewing(tab));
    assert_eq!(viewing(&world, id), Some(root));
    world.despawn(b);
    assert_eq!(focused(&world, id), Some(a));
    Ok(())
}

#[test]
fn inert_scene_worlds_do_not_normalize_tab_removal() {
    let mut world = World::new();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    world.despawn(tab);
    assert!(tabs(&world, root).is_empty());
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
    observe(&mut world);
    let left = viewer(&mut world, root);
    let right = viewer(&mut world, root);
    repair(&mut world);
    world.entity_mut(left).insert(Focused(b));
    select(&mut world, left, Scope::Tab, Pick::Next)?;
    assert_eq!(focus_of(&world, left), Some(c));
    assert_eq!(focus_of(&world, right), Some(a));
    select(&mut world, left, Scope::Tab, Pick::Previous)?;
    assert_eq!(focus_of(&world, left), Some(b));
    focus_last(&mut world, left)?;
    assert_eq!(focus_of(&world, left), Some(a));
    focus_last(&mut world, left)?;
    assert_eq!(focus_of(&world, left), Some(b));
    world.despawn(b);
    repair(&mut world);
    assert_eq!(focus_of(&world, left), Some(a));
    world.despawn(first);
    repair(&mut world);
    assert_eq!(focus_of(&world, left), Some(c));
    assert_eq!(focus_of(&world, right), Some(c));
    Ok(())
}

#[test]
fn a_child_of_into_its_own_subtree_is_rejected_like_self_parenting() -> Outcome {
    let mut app = bevy_app::App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.add_plugins(crate::server::ServerPlugin);
    let world = app.world_mut();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let split = world.spawn((Split, ChildOf(tab))).id();
    let pane = leaf(world, split);
    // A raw client parents the tab under a pane inside it: a cycle that
    // every hierarchy walk would follow forever.
    world.entity_mut(tab).insert(ChildOf(pane));
    world.flush();
    assert!(world.get::<ChildOf>(tab).is_none());
    assert_eq!(world.get::<ChildOf>(split).need()?.parent(), tab);
    assert!(
        world
            .get::<Children>(pane)
            .is_none_or(|children| !children.contains(&tab))
    );
    assert_eq!(leaves(world, tab), vec![pane]);
    // An ordinary reparent still works.
    let other = world.spawn((Tab, ChildOf(root))).id();
    world.entity_mut(pane).insert(ChildOf(other));
    world.flush();
    assert_eq!(world.get::<ChildOf>(pane).need()?.parent(), other);
    Ok(())
}

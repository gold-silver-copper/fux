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

fn viewer_component() -> Viewer {
    Viewer {
        rows: 24,
        cols: 80,
        zoom: false,
        scrollback: 0,
        notice: None,
    }
}

/// A workspace > tab > split > pane view hierarchy with one real viewer.
fn layout_world() -> (World, [Entity; 4], Entity) {
    let mut world = World::new();
    observe(&mut world);
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let split = world.spawn((Split, ChildOf(tab))).id();
    let pane = leaf(&mut world, split);
    let _other = leaf(&mut world, split);
    let id = viewer(&mut world, root);
    world.flush();
    (world, [root, tab, split, pane], id)
}

fn assert_plain_layout(world: &World, entity: Entity) {
    let entity = world.entity(entity);
    assert!(!entity.contains::<Viewer>());
    assert!(!entity.contains::<Memory>());
    assert!(!entity.contains::<crate::paste::Ownership>());
    assert!(!entity.contains::<Viewing>());
    assert!(!entity.contains::<OnTab>());
    assert!(!entity.contains::<Focused>());
}

fn assert_consistent(world: &World, id: Entity, root: Entity, tab: Entity) -> Outcome {
    assert_eq!(viewing(world, id), Some(root));
    assert_eq!(on_tab(world, id), Some(tab));
    let focus = focused(world, id).need()?;
    assert!(leaves(world, tab).contains(&focus));
    Ok(())
}

#[test]
fn a_viewer_inserted_onto_any_layout_node_is_removed_and_repair_still_settles() -> Outcome {
    for index in 0..4 {
        let (mut world, nodes, id) = layout_world();
        let [root, tab, ..] = nodes;
        let node = *nodes.get(index).need()?;
        let parent = world.get::<ChildOf>(node).map(ChildOf::parent);
        world.entity_mut(node).insert(viewer_component());
        world.flush();
        assert_plain_layout(&world, node);
        assert_eq!(world.get::<ChildOf>(node).map(ChildOf::parent), parent);
        // The second half of hunt 5's finding 001: any relationship insert on
        // a real viewer queues repair, which recursed until the stack overflowed.
        for insert in 0..3 {
            match insert {
                0 => world.entity_mut(id).insert(Viewing(root)),
                1 => world.entity_mut(id).insert(OnTab(tab)),
                _ => world.entity_mut(id).insert(Focused(nodes[3])),
            };
            world.flush();
            assert_consistent(&world, id, root, tab)?;
        }
        assert_eq!(leaves(&world, root).len(), 2);
    }
    Ok(())
}

#[test]
fn a_layout_node_inserted_onto_a_viewer_or_with_one_takes_the_layout_role() -> Outcome {
    let (mut world, [root, tab, ..], id) = layout_world();
    // Reverse order: the entity was a real viewer, then became a tab.
    let second = viewer(&mut world, root);
    world.flush();
    world.entity_mut(second).insert((Tab, ChildOf(root)));
    world.flush();
    assert!(world.get::<Tab>(second).is_some());
    assert_plain_layout(&world, second);
    // Both roles in one insertion.
    let both = world.spawn((Tab, viewer_component(), ChildOf(root))).id();
    world.flush();
    assert!(world.get::<Tab>(both).is_some());
    assert_plain_layout(&world, both);
    world.entity_mut(id).insert(OnTab(tab));
    world.flush();
    assert_consistent(&world, id, root, tab)?;
    Ok(())
}

#[test]
fn repair_ignores_a_viewer_on_a_layout_node_even_before_normalization() -> Outcome {
    // No observers: only the relationship hooks and repair's own filter run.
    let mut world = World::new();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let _pane = leaf(&mut world, tab);
    let id = viewer(&mut world, root);
    world.entity_mut(tab).insert(viewer_component());
    world.entity_mut(id).insert(Viewing(root));
    world.flush();
    assert_consistent(&world, id, root, tab)?;
    assert!(world.get::<Viewing>(tab).is_none());
    Ok(())
}

#[test]
fn repair_absorbs_requests_made_during_a_pass_and_is_bounded() -> Outcome {
    // A change flushed inside a pass (here a queued removal of another
    // viewer's focus) is repaired by a further pass, not lost.
    let (mut world, [root, tab, ..], a) = layout_world();
    let b = viewer(&mut world, root);
    world.flush();
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = fired.clone();
    world.add_observer(
        move |inserted: On<Insert, Focused>, mut commands: Commands| {
            if inserted.entity == a && !once.swap(true, std::sync::atomic::Ordering::Relaxed) {
                commands.entity(b).remove::<Focused>();
            }
        },
    );
    world.entity_mut(a).remove::<Focused>();
    world.flush();
    assert!(fired.load(std::sync::atomic::Ordering::Relaxed));
    assert_consistent(&world, a, root, tab)?;
    assert_consistent(&world, b, root, tab)?;
    assert!(!world.resource::<Repairing>().running);

    // A viewer that can never become consistent costs MAX_PASSES passes, not
    // the process: every tab repair gives it is taken away again.
    let passes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = passes.clone();
    world.add_observer(move |inserted: On<Insert, OnTab>, mut commands: Commands| {
        if inserted.entity == a {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            commands.entity(a).remove::<OnTab>();
        }
    });
    world.entity_mut(a).remove::<OnTab>();
    world.flush();
    let passes = passes.load(std::sync::atomic::Ordering::Relaxed);
    assert!((2..=MAX_PASSES + 1).contains(&passes), "{passes} passes");
    assert!(!world.resource::<Repairing>().running);
    assert_consistent(&world, b, root, tab)?;
    Ok(())
}

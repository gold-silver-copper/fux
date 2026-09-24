use super::*;
use crate::testing::*;

/// One workspace, one tab, a split holding two panes, and one viewer on it.
fn world() -> (World, Layout) {
    let mut world = World::new();
    world.insert_resource(Settings::default());
    let registry = AppTypeRegistry::default();
    {
        let mut types = registry.write();
        types.register::<Workspace>();
        types.register::<Tab>();
        types.register::<Split>();
        types.register::<PaneView>();
        types.register::<WorkspaceOrder>();
        types.register::<Name>();
        types.register::<ChildOf>();
        types.register::<Children>();
        types.register::<bevy_ui::Node>();
        types.register::<crate::interaction::Prefix>();
    }
    world.insert_resource(registry);
    let workspace = world.spawn((Workspace, WorkspaceOrder(0))).id();
    let tab = world.spawn((Tab, ChildOf(workspace))).id();
    let split = world.spawn((Split, ChildOf(tab))).id();
    let a = world.spawn(ProcessState::default()).id();
    let b = world.spawn(ProcessState::default()).id();
    let left = world.spawn((PaneView { pane: a }, ChildOf(split))).id();
    let right = world.spawn((PaneView { pane: b }, ChildOf(split))).id();
    let viewer = world
        .spawn((
            Viewer {
                rows: 24,
                cols: 80,
                zoom: false,
                scrollback: 0,
                notice: None,
            },
            Viewing(workspace),
            OnTab(tab),
            Focused(left),
        ))
        .id();
    world.flush();
    let layout = Layout {
        workspace,
        tab,
        split,
        left,
        right,
        a,
        viewer,
    };
    (world, layout)
}

#[derive(Clone, Copy)]
struct Layout {
    workspace: Entity,
    tab: Entity,
    split: Entity,
    left: Entity,
    right: Entity,
    a: Entity,
    viewer: Entity,
}

fn has(world: &mut World, needle: &str) -> Result<(), String> {
    let found = violations(world);
    if found.iter().any(|line| line.contains(needle)) {
        Ok(())
    } else {
        Err(format!(
            "expected a violation containing {needle:?}, got {found:?}"
        ))
    }
}

#[test]
fn a_consistent_world_has_no_violations() -> Outcome {
    let (mut world, _) = world();
    assert_eq!(violations(&mut world), Vec::<String>::new());
    Ok(())
}

#[test]
fn missing_settings_is_reported() -> Outcome {
    let (mut world, _) = world();
    world.remove_resource::<Settings>();
    has(&mut world, "Settings resource is missing")?;
    Ok(())
}

#[test]
fn hierarchy_kinds_are_checked() -> Outcome {
    let (mut world, l) = world();
    let loose_tab = world.spawn((Tab, ChildOf(l.split))).id();
    // An unplaced tab (no parent at all) is allowed; a misplaced one is not.
    let unplaced = world.spawn(Tab).id();
    assert!(
        !violations(&mut world)
            .iter()
            .any(|v| v.contains(&unplaced.to_string()))
    );
    world.despawn(unplaced);
    has(&mut world, &format!("tab {loose_tab} has parent"))?;
    world.despawn(loose_tab);
    let stray = world.spawn(ProcessState::default()).id();
    let pane = world
        .spawn((PaneView { pane: stray }, ChildOf(l.workspace)))
        .id();
    has(&mut world, &format!("pane view {pane} has parent"))?;
    world.despawn(pane);
    let nested = world
        .spawn((Workspace, WorkspaceOrder(9), ChildOf(l.tab)))
        .id();
    has(&mut world, &format!("workspace {nested} has a parent"))?;
    Ok(())
}

#[test]
fn a_cycle_is_reported_instead_of_followed() -> Outcome {
    let (mut world, l) = world();
    // Bevy refuses only self-parenting; a longer cycle is representable.
    world.entity_mut(l.tab).insert(ChildOf(l.left));
    has(&mut world, "inside its own subtree")?;
    Ok(())
}

#[test]
fn a_split_with_one_child_must_have_collapsed() -> Outcome {
    let (mut world, l) = world();
    world.despawn(l.right);
    has(
        &mut world,
        &format!("split container {} has 1 children", l.split),
    )?;
    Ok(())
}

#[test]
fn views_must_refer_to_processes() -> Outcome {
    let (mut world, l) = world();
    world.despawn(l.a);
    // Despawning the process takes its views with it (PaneView is a
    // relationship), so a dangling view needs a process without state.
    let bare = world.spawn_empty().id();
    world.spawn((PaneView { pane: bare }, ChildOf(l.split)));
    has(&mut world, "with no process state")?;
    Ok(())
}

#[test]
fn process_sizes_and_liveness_are_checked() -> Outcome {
    let (mut world, l) = world();
    world.get_mut::<ProcessState>(l.a).need()?.rows = 0;
    has(&mut world, "outside 1..=4096")?;
    let mut state = world.get_mut::<ProcessState>(l.a).need()?;
    state.rows = 24;
    state.status = Status::Running {
        pid: u32::MAX - 1,
        error: None,
    };
    has(&mut world, "which is dead")?;
    Ok(())
}

#[test]
fn workspace_order_is_unique_and_present() -> Outcome {
    let (mut world, _) = world();
    let other = world.spawn((Workspace, WorkspaceOrder(0))).id();
    has(&mut world, "share a WorkspaceOrder")?;
    world.entity_mut(other).remove::<WorkspaceOrder>();
    has(
        &mut world,
        &format!("workspace {other} has no WorkspaceOrder"),
    )?;
    Ok(())
}

#[test]
fn viewers_are_bounded_and_never_layout_nodes() -> Outcome {
    let (mut world, l) = world();
    world.get_mut::<Viewer>(l.viewer).need()?.rows = 5000;
    has(&mut world, "over 4096")?;
    world.get_mut::<Viewer>(l.viewer).need()?.rows = 24;
    world.entity_mut(l.split).insert(Viewer {
        rows: 1,
        cols: 1,
        zoom: false,
        scrollback: 0,
        notice: None,
    });
    has(
        &mut world,
        &format!("layout node {} is also a viewer", l.split),
    )?;
    Ok(())
}

#[test]
fn resource_entities_are_never_layout_nodes() -> Outcome {
    let (mut world, _) = world();
    let resource = world
        .query_filtered::<Entity, With<IsResource>>()
        .iter(&world)
        .next()
        .need()?;
    world.entity_mut(resource).insert(Split);
    has(
        &mut world,
        &format!("resource entity {resource} is a layout node"),
    )?;
    Ok(())
}

#[test]
fn a_workspace_that_cannot_be_projected_is_reported() -> Outcome {
    let (mut world, l) = world();
    world.spawn((crate::interaction::Prefix::default(), ChildOf(l.tab)));
    has(
        &mut world,
        &format!("workspace {} cannot be projected", l.workspace),
    )?;
    Ok(())
}

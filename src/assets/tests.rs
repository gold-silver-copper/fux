mod reload_spike;

use super::*;
use crate::testing::*;
use crate::{model::*, navigation};
use bevy_ui::{Display, Node, RepeatedGridTrack, UiRect, Val};

fn app() -> App {
    let mut app = App::new();
    app.register_type::<Workspace>()
        .register_type::<Tab>()
        .register_type::<WorkspaceOrder>()
        .register_type::<Split>()
        .register_type::<PaneView>()
        .register_type::<PaneViews>()
        .register_type::<Launch>()
        .register_type::<ProcessState>()
        .register_type::<Name>()
        .register_type::<Viewer>();
    crate::presentation::register_types(&mut app);
    app
}
fn process(world: &mut World) -> Entity {
    world
        .spawn(Launch {
            argv: vec!["unused".into()],
            cwd: String::new(),
            history_lines: 12,
        })
        .id()
}

#[test]
fn native_tab_scene_round_trip_preserves_order_names_and_remapped_processes()
-> crate::testing::Outcome {
    let mut app = app();
    let world = app.world_mut();
    let one = process(world);
    let two = process(world);
    let root = world
        .spawn((Workspace, Name::new("project"), WorkspaceOrder(7)))
        .id();
    let a = world.spawn((Tab, Name::new("first"), ChildOf(root))).id();
    let b = world.spawn((Tab, Name::new("second"), ChildOf(root))).id();
    world.spawn((PaneView { pane: one }, ChildOf(a)));
    world.spawn((PaneView { pane: two }, ChildOf(b)));
    world.entity_mut(root).replace_children(&[b, a]);
    let text = serialize_layout(world, root)?;
    for absent in [
        "Launch",
        "ProcessState",
        "LayoutCache",
        "Memory",
        "Viewers",
        "TabViewers",
        "FocusedBy",
        "Selection",
        "Ownership",
        "Overlay",
    ] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
    let scene = extract_layout(world, root)?;
    let loaded = apply_layout(world, &scene, &[(one, two), (two, one)])?;
    assert_eq!(world.get::<WorkspaceOrder>(loaded).need()?.0, 7);
    let tabs = navigation::tabs(world, loaded);
    assert_eq!(tabs.len(), 2);
    assert_eq!(
        world.get::<Name>(*tabs.first().need()?).need()?.as_str(),
        "second"
    );
    assert_eq!(
        world.get::<Name>(*tabs.get(1).need()?).need()?.as_str(),
        "first"
    );
    assert_eq!(
        world
            .get::<PaneView>(
                *navigation::leaves(world, *tabs.first().need()?)
                    .first()
                    .need()?
            )
            .need()?
            .pane,
        one
    );
    assert_eq!(
        world
            .get::<PaneView>(
                *navigation::leaves(world, *tabs.get(1).need()?)
                    .first()
                    .need()?
            )
            .need()?
            .pane,
        two
    );
    assert_eq!(world.query::<&Launch>().iter(world).count(), 2);
    Ok(())
}

#[test]
fn tabless_scene_migration_moves_the_layout_box_once_without_losing_panes()
-> crate::testing::Outcome {
    let mut app = app();
    let world = app.world_mut();
    let pane = process(world);
    let node = Node {
        display: Display::Grid,
        grid_template_columns: vec![RepeatedGridTrack::flex(2, 1.0)],
        column_gap: Val::Px(3.0),
        padding: UiRect::all(Val::Px(2.0)),
        margin: UiRect::all(Val::Px(1.0)),
        ..crate::model::tab_node()
    };
    let root = world.spawn((Workspace, node.clone())).id();
    world.spawn((PaneView { pane }, ChildOf(root)));
    world.spawn((PaneView { pane }, ChildOf(root)));
    let scene = extract_layout(world, root)?;
    let loaded = apply_layout(world, &scene, &[])?;
    let tabs = navigation::tabs(world, loaded);
    assert_eq!(tabs.len(), 1);
    assert_eq!(world.get::<Node>(*tabs.first().need()?).need()?, &node);
    assert_eq!(world.get::<Node>(loaded).need()?, &crate::model::tab_node());
    assert_eq!(navigation::leaves(world, loaded).len(), 2);
    assert_eq!(world.query::<&Launch>().iter(world).count(), 1);
    let resaved = extract_layout(world, loaded)?;
    let reloaded = apply_layout(world, &resaved, &[])?;
    assert_eq!(navigation::tabs(world, reloaded).len(), 1);
    assert_eq!(navigation::leaves(world, reloaded).len(), 2);
    Ok(())
}

#[test]
fn coherent_defaults_have_exact_unique_keys_and_action_pairs() -> crate::testing::Outcome {
    let settings = Settings::default();
    assert_eq!(settings.prefix.as_str(), "ctrl-b");
    let actual: std::collections::BTreeMap<_, _> = settings
        .bindings
        .iter()
        .map(|b| (b.key.as_str(), b.action.to_string()))
        .collect();
    assert_eq!(actual.len(), settings.bindings.len());
    let expected = [
        ("[", "tab_previous"),
        ("]", "tab_next"),
        ("{", "workspace_previous"),
        ("}", "workspace_next"),
        ("tab", "focus_next"),
        ("shift-tab", "focus_previous"),
        ("backspace", "focus_last"),
        ("alt-left", "focus_left"),
        ("alt-right", "focus_right"),
        ("alt-up", "focus_up"),
        ("alt-down", "focus_down"),
        ("t", "tab_new"),
        ("T", "tab_choose"),
        ("w", "workspace_new"),
        ("W", "workspace_choose"),
        ("p", "pane_menu"),
        ("s", "tab_menu"),
        ("S", "workspace_menu"),
        ("h", "split_horizontal"),
        ("v", "split_vertical"),
        ("z", "zoom"),
        ("r", "rename_pane"),
        ("x", "close"),
        ("ctrl-left", "shrink_width"),
        ("ctrl-right", "grow_width"),
        ("ctrl-up", "grow_height"),
        ("ctrl-down", "shrink_height"),
        ("shift-left", "move_left"),
        ("shift-right", "move_right"),
        ("shift-up", "move_up"),
        ("shift-down", "move_down"),
        ("c", "copy_mode"),
        ("y", "copy"),
        ("d", "detach"),
    ]
    .into_iter()
    .map(|(key, action)| (key, action.to_owned()))
    .collect();
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn invalid_tab_placement_and_runtime_viewers_are_not_scene_content() -> crate::testing::Outcome {
    let mut app = app();
    let world = app.world_mut();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let nested = world.spawn((Tab, ChildOf(tab))).id();
    assert!(
        extract_layout(world, root)
            .err()
            .need()?
            .contains("direct workspace")
    );
    world.despawn(nested);
    world.spawn((
        Viewer {
            rows: 24,
            cols: 80,
            zoom: false,
            scrollback: 0,
            notice: None,
        },
        Viewing(root),
        OnTab(tab),
        ChildOf(root),
    ));
    assert!(
        extract_layout(world, root)
            .err()
            .need()?
            .contains("viewers")
    );
    Ok(())
}

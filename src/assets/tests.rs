mod reload_spike;

use super::*;
use crate::testing::*;
use crate::{model::*, navigation};
use bevy_ui::{Display, Node, RepeatedGridTrack, UiRect, Val};

#[test]
fn initial_shell_waits_for_settings_and_is_not_recreated_on_reload() -> Outcome {
    let mut app = App::new();
    app.add_plugins(bevy_app::TaskPoolPlugin::default());
    app.insert_resource(Wake(std::thread::current()));
    app.insert_resource(Settings {
        shell: vec!["must-not-launch-before-config".into()],
        ..Default::default()
    });
    app.insert_resource(ConfigAssets {
        settings: Handle::default(),
        settings_path: "fux.json".into(),
        initial_settings_settled: false,
        layout: None,
        layout_needs_apply: false,
    });
    app.add_plugins(crate::server::ServerPlugin);
    app.insert_resource(crate::server::Disconnected(async_channel::unbounded().1));
    app.add_message::<LayoutReload>();
    // Waiting updates must not consume the one-shot initialization condition.
    for _ in 0..2 {
        app.update();
        assert_eq!(
            app.world_mut().query::<&Launch>().iter(app.world()).count(),
            0
        );
        assert!(navigation::workspaces(app.world_mut()).is_empty());
    }
    let configured = vec!["/bin/sh".to_owned(), "-c".into(), "exit 0".into()];
    app.world_mut().resource_mut::<Settings>().shell = configured.clone();
    app.world_mut()
        .resource_mut::<ConfigAssets>()
        .initial_settings_settled = true;
    app.update();
    let launches: Vec<_> = app
        .world_mut()
        .query::<&Launch>()
        .iter(app.world())
        .cloned()
        .collect();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches.first().need()?.argv, configured);
    assert_eq!(navigation::workspaces(app.world_mut()).len(), 1);

    let loading = PendingAssets::default();
    loading.start("fux.json".into());
    app.insert_resource(loading);
    app.world_mut().resource_mut::<Settings>().shell =
        vec!["reload-must-not-replace-live-shell".into()];
    assert!(pending(app.world()));
    assert!(initial_settings_settled(app.world()));
    for _ in 0..2 {
        app.update();
    }
    let launches: Vec<_> = app
        .world_mut()
        .query::<&Launch>()
        .iter(app.world())
        .cloned()
        .collect();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches.first().need()?.argv, configured);
    assert_eq!(navigation::workspaces(app.world_mut()).len(), 1);
    Ok(())
}

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
fn reflected_interactions_stay_out_of_layout_scenes() -> Outcome {
    use crate::{
        actions::Target,
        interaction::{Mode, Overlay, Prefix},
    };
    let mut app = app();
    app.register_type::<Prefix>().register_type::<Overlay>();
    let world = app.world_mut();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let overlay = Overlay {
        serial: 1,
        target: Target {
            workspace: root,
            tab: Some(tab),
            leaf: None,
        },
        mode: Mode::Text {
            action: crate::actions::Action::RenameTab,
            buffer: "draft".into(),
        },
    };
    // Normal viewer-local interactions are not traversed by layout extraction.
    world.spawn((Prefix::default(), overlay.clone()));
    let text = serialize_layout(world, root)?;
    assert!(!text.contains("Prefix"));
    assert!(!text.contains("Overlay"));
    for prefix in [true, false] {
        if prefix {
            world.entity_mut(tab).insert(Prefix::default());
        } else {
            world.entity_mut(tab).insert(overlay.clone());
        }
        assert!(
            extract_layout(world, root)
                .err()
                .need()?
                .contains("interaction state")
        );
        // A foreign/native scene can bypass fux's save validation. Reject it
        // before spawning any entities, even when the component is on a tab.
        let scene = {
            let registry = world.resource::<AppTypeRegistry>().read();
            DynamicWorldBuilder::from_world(world, &registry)
                .extract_entities([root, tab].into_iter())
                .build()
        };
        let before = world.entities().len();
        assert!(
            apply_layout(world, &scene, &[])
                .err()
                .need()?
                .contains("interaction state")
        );
        assert_eq!(world.entities().len(), before);
        world.entity_mut(tab).remove::<(Prefix, Overlay)>();
    }
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

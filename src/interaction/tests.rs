use super::*;

fn setup() -> (World, Entity, Target, Entity) {
    let mut world = World::new();
    let workspace = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(workspace))).id();
    let pane = world.spawn_empty().id();
    let leaf = world.spawn((PaneView { pane }, ChildOf(tab))).id();
    let other = world.spawn((PaneView { pane }, ChildOf(tab))).id();
    let id = world
        .spawn(Viewer {
            workspace,
            tab: Some(tab),
            focus: Some(leaf),
            rows: 24,
            cols: 80,
            zoom: false,
            scrollback: 0,
            notice: String::new(),
            notice_error: false,
            help_scroll: 0,
            prefix: false,
            prompt: None,
            buffer: String::new(),
        })
        .id();
    (
        world,
        id,
        Target {
            workspace,
            tab: Some(tab),
            leaf: Some(leaf),
        },
        other,
    )
}
fn key(key: &str) -> Input {
    Input::Key {
        key: key.into(),
        ctrl: false,
        alt: false,
        shift: false,
    }
}

#[test]
fn menus_keep_unbound_actions_and_share_navigation_including_layout_prompts() {
    let (mut world, id, target, _) = setup();
    for (menu, required) in [
        (
            "pane_menu",
            vec![
                "terminate",
                "reorder_prev",
                "reorder_next",
                "move_workspace",
                "scroll_up",
                "scroll_down",
                "swap_choose",
                "move_tab",
                "move_new_tab",
                "move_new_workspace",
            ],
        ),
        (
            "tab_menu",
            vec![
                "rename_tab",
                "tab_close",
                "tab_reorder_previous",
                "tab_reorder_next",
            ],
        ),
        (
            "workspace_menu",
            vec![
                "rename_workspace",
                "workspace_close",
                "workspace_reorder_previous",
                "workspace_reorder_next",
                "save_layout",
                "load_layout",
            ],
        ),
    ] {
        invoke(
            &mut world,
            id,
            target,
            menu.parse().unwrap(),
            None,
            "",
            true,
        )
        .unwrap();
        let Mode::List { entries, .. } = &world.get::<Overlay>(id).unwrap().mode else {
            panic!()
        };
        for action in required {
            assert!(
                entries.iter().any(|e| e.action.to_string() == action),
                "{menu} lacks {action}"
            );
        }
        let count = entries.len();
        for rows in 0..8 {
            world.get_mut::<Viewer>(id).unwrap().rows = rows;
            input(&mut world, id, &key("home"));
            input(&mut world, id, &key("pagedown"));
            let Mode::List { selected, .. } = &world.get::<Overlay>(id).unwrap().mode else {
                panic!()
            };
            assert_eq!(*selected, list_capacity(rows).min(count - 1));
            input(&mut world, id, &key("end"));
            let rendered = lines(&world, world.get::<Overlay>(id).unwrap(), rows);
            if rows >= 2 {
                assert!(
                    rendered
                        .iter()
                        .any(|(text, style)| text.starts_with('›') && style.contains('7'))
                );
            }
            input(&mut world, id, &key("pageup"));
            let Mode::List { selected, .. } = &world.get::<Overlay>(id).unwrap().mode else {
                panic!()
            };
            assert_eq!(*selected, (count - 1).saturating_sub(list_capacity(rows)));
        }
        input(&mut world, id, &key("escape"));
        assert!(world.get::<Overlay>(id).is_none());
    }
    for action in ["save_layout", "load_layout"] {
        invoke(
            &mut world,
            id,
            target,
            "workspace_menu".parse().unwrap(),
            None,
            "",
            true,
        )
        .unwrap();
        let mut overlay = world.get_mut::<Overlay>(id).unwrap();
        let Mode::List {
            entries, selected, ..
        } = &mut overlay.mode
        else {
            panic!()
        };
        *selected = entries
            .iter()
            .position(|e| e.action.to_string() == action)
            .unwrap();
        input(&mut world, id, &key("enter"));
        assert!(
            matches!(&world.get::<Overlay>(id).unwrap().mode, Mode::Text { action: a, .. } if a.to_string() == action)
        );
        input(&mut world, id, &key("escape"));
    }
}

#[test]
fn confirmation_captures_target_and_preserves_a_shared_process() {
    let (mut world, id, target, other) = setup();
    let process = world.get::<PaneView>(other).unwrap().pane;
    invoke(
        &mut world,
        id,
        target,
        "close".parse().unwrap(),
        None,
        "",
        true,
    )
    .unwrap();
    world.get_mut::<Viewer>(id).unwrap().focus = Some(other);
    assert!(input(&mut world, id, &Input::Paste { text: "y".into() }));
    assert!(world.get_entity(target.leaf.unwrap()).is_ok());
    assert!(input(&mut world, id, &key("y")));
    assert!(world.get_entity(target.leaf.unwrap()).is_err());
    assert!(world.get_entity(other).is_ok());
    assert!(world.get_entity(process).is_ok());
    close(&mut world, other);
    assert!(world.get_entity(process).is_err());
}

#[test]
fn removed_confirmation_target_cancels_without_retargeting() {
    let (mut world, id, target, other) = setup();
    invoke(
        &mut world,
        id,
        target,
        "close".parse().unwrap(),
        None,
        "",
        true,
    )
    .unwrap();
    world.despawn(target.leaf.unwrap());
    world.get_mut::<Viewer>(id).unwrap().focus = Some(other);
    assert!(input(&mut world, id, &key("y")));
    assert!(world.get_entity(other).is_ok());
    assert!(world.get::<Overlay>(id).is_none());
    assert!(
        world
            .get::<Viewer>(id)
            .unwrap()
            .notice
            .contains("cancelled")
    );
}

#[test]
fn tab_close_keeps_an_empty_tab_and_workspace_close_repairs_other_viewers() {
    let (mut world, id, target, _) = setup();
    invoke(
        &mut world,
        id,
        target,
        "tab_close".parse().unwrap(),
        None,
        "",
        false,
    )
    .unwrap();
    let v = world.get::<Viewer>(id).unwrap();
    assert_ne!(v.tab, target.tab);
    assert!(v.focus.is_none());
    assert!(v.tab.is_some());
    let target = Target::viewer(v);
    let replacement = world.spawn(Workspace).id();
    invoke(
        &mut world,
        id,
        target,
        "workspace_close".parse().unwrap(),
        None,
        "",
        false,
    )
    .unwrap();
    assert_eq!(world.get::<Viewer>(id).unwrap().workspace, replacement);
    let target = Target::viewer(world.get::<Viewer>(id).unwrap());
    invoke(
        &mut world,
        id,
        target,
        "workspace_close".parse().unwrap(),
        None,
        "",
        false,
    )
    .unwrap();
    assert!(world.get_entity(id).is_err());
}

#[test]
fn rearrangement_keeps_entity_identity_and_native_child_order() {
    let (mut world, id, target, other) = setup();
    let source = target.leaf.unwrap();
    swap(&mut world, source, other).unwrap();
    assert_eq!(
        navigation::leaves(&world, target.tab.unwrap()),
        vec![other, source]
    );
    move_beside(&mut world, source, other, "up").unwrap();
    let split = world.get::<ChildOf>(source).unwrap().parent();
    assert!(world.get::<Split>(split).is_some());
    assert_eq!(
        world
            .get::<Children>(split)
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        vec![source, other]
    );
    invoke(
        &mut world,
        id,
        target,
        "move_new_workspace".parse().unwrap(),
        None,
        "destination",
        false,
    )
    .unwrap();
    let v = world.get::<Viewer>(id).unwrap();
    assert_ne!(v.workspace, target.workspace);
    assert_eq!(v.focus, Some(source));
    assert!(world.get::<PaneView>(other).is_some());
}

#[test]
fn rename_prompt_keeps_its_original_target_and_escape_cancels() {
    let (mut world, id, target, other) = setup();
    invoke(
        &mut world,
        id,
        target,
        "rename_tab".parse().unwrap(),
        None,
        "",
        true,
    )
    .unwrap();
    world.get_mut::<Viewer>(id).unwrap().focus = Some(other);
    input(
        &mut world,
        id,
        &Input::Paste {
            text: "界 tab".into(),
        },
    );
    input(&mut world, id, &key("enter"));
    assert_eq!(
        world.get::<Name>(target.tab.unwrap()).unwrap().as_str(),
        "界 tab"
    );
    invoke(
        &mut world,
        id,
        target,
        "close".parse().unwrap(),
        None,
        "",
        true,
    )
    .unwrap();
    input(&mut world, id, &key("escape"));
    assert!(world.get::<Overlay>(id).is_none());
    assert!(world.get_entity(target.leaf.unwrap()).is_ok());
}

#[test]
fn each_close_scope_requires_confirmation_and_cancel_keeps_its_hierarchy() {
    for action in ["close", "tab_close", "workspace_close"] {
        let (mut world, id, target, _) = setup();
        invoke(
            &mut world,
            id,
            target,
            action.parse().unwrap(),
            None,
            "",
            true,
        )
        .unwrap();
        assert!(world.get::<Overlay>(id).is_some());
        input(&mut world, id, &Input::Paste { text: "y".into() });
        assert!(world.get_entity(target.leaf.unwrap()).is_ok());
        input(&mut world, id, &key("n"));
        assert!(world.get_entity(target.leaf.unwrap()).is_ok());
        invoke(
            &mut world,
            id,
            target,
            action.parse().unwrap(),
            None,
            "",
            true,
        )
        .unwrap();
        input(&mut world, id, &key("y"));
        assert!(world.get_entity(target.leaf.unwrap()).is_err());
        assert_eq!(
            world.get_entity(target.workspace).is_err(),
            action == "workspace_close"
        );
    }
}

#[test]
fn stale_chooser_destination_never_changes_the_source_or_an_unrelated_target() {
    let (mut world, id, target, _) = setup();
    let destination = world.spawn((Tab, ChildOf(target.workspace))).id();
    let overlay = Overlay {
        serial: 1,
        target,
        mode: Mode::List {
            title: "move".into(),
            selected: 0,
            entries: vec![Entry {
                label: "removed".into(),
                action: Action::MoveTab,
                destination: Some(destination),
            }],
        },
    };
    world.entity_mut(id).insert(overlay);
    world.despawn(destination);
    input(&mut world, id, &key("enter"));
    assert_eq!(
        world.get::<ChildOf>(target.leaf.unwrap()).unwrap().parent(),
        target.tab.unwrap()
    );
    assert!(world.get::<Viewer>(id).unwrap().notice.contains("removed"));
}

#[test]
fn every_chooser_entry_remains_visible_on_tiny_terminals() {
    let (world, _, target, _) = setup();
    for rows in 0..10 {
        for selected in 0..10 {
            let entries = (0..10)
                .map(|i| Entry {
                    label: format!("entry-{i}"),
                    action: Action::TabSelect,
                    destination: target.tab,
                })
                .collect();
            let overlay = Overlay {
                serial: 1,
                target,
                mode: Mode::List {
                    title: "Choose".into(),
                    entries,
                    selected,
                },
            };
            let lines = lines(&world, &overlay, rows);
            assert!(lines.len() <= rows.saturating_sub(1) as usize);
            if rows >= 2 {
                assert!(
                    lines
                        .iter()
                        .any(|(s, _)| s.contains(&format!("entry-{selected}"))),
                    "{rows}: {lines:?}"
                );
            }
        }
    }
}

#[test]
fn pasted_text_cannot_cross_a_cancelled_or_reopened_prompt() {
    let (mut world, id, target, _) = setup();
    invoke(
        &mut world,
        id,
        target,
        "rename_tab".parse().unwrap(),
        None,
        "",
        true,
    )
    .unwrap();
    assert!(crate::paste::input(&mut world, id, &Input::PasteBegin));
    input(&mut world, id, &key("escape"));
    invoke(
        &mut world,
        id,
        target,
        "rename_tab".parse().unwrap(),
        None,
        "",
        true,
    )
    .unwrap();
    assert!(crate::paste::input(
        &mut world,
        id,
        &Input::Paste {
            text: "old prompt text".into()
        }
    ));
    let Mode::Text { buffer, .. } = &world.get::<Overlay>(id).unwrap().mode else {
        panic!()
    };
    assert!(buffer.is_empty());
}

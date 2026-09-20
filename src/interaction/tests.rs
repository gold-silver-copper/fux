use super::*;
use crate::testing::*;

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
fn menus_keep_unbound_actions_and_share_navigation_including_layout_prompts()
-> crate::testing::Outcome {
    let (mut world, id, target, _) = setup();
    for (menu, required) in [
        (
            Action::PaneMenu,
            vec![
                Action::Terminate,
                Action::ReorderPrev,
                Action::ReorderNext,
                Action::MoveWorkspace,
                Action::ScrollUp,
                Action::ScrollDown,
                Action::SwapChoose,
                Action::MoveTab,
                Action::MoveNewTab,
                Action::MoveNewWorkspace,
            ],
        ),
        (
            Action::TabMenu,
            vec![
                Action::RenameTab,
                Action::TabClose,
                Action::TabReorderPrevious,
                Action::TabReorderNext,
            ],
        ),
        (
            Action::WorkspaceMenu,
            vec![
                Action::RenameWorkspace,
                Action::WorkspaceClose,
                Action::WorkspaceReorderPrevious,
                Action::WorkspaceReorderNext,
                Action::SaveLayout,
                Action::LoadLayout,
            ],
        ),
    ] {
        invoke(&mut world, id, target, menu, None, "", true)?;
        let Mode::List { entries, .. } = &world.get::<Overlay>(id).need()?.mode else {
            return Err("unexpected overlay mode".into());
        };
        for action in required {
            assert!(
                entries.iter().any(|e| e.action == action),
                "{menu:?} lacks {action:?}"
            );
        }
        let count = entries.len();
        for rows in 0..8 {
            world.get_mut::<Viewer>(id).need()?.rows = rows;
            input(&mut world, id, &key("home"));
            input(&mut world, id, &key("pagedown"));
            let Mode::List { selected, .. } = &world.get::<Overlay>(id).need()?.mode else {
                return Err("unexpected overlay mode".into());
            };
            assert_eq!(*selected, list_capacity(rows).min(count - 1));
            input(&mut world, id, &key("end"));
            let rendered = lines(&world, world.get::<Overlay>(id).need()?, rows);
            if rows >= 2 {
                assert!(
                    rendered
                        .iter()
                        .any(|(text, style)| text.starts_with('›') && style.contains('7'))
                );
            }
            input(&mut world, id, &key("pageup"));
            let Mode::List { selected, .. } = &world.get::<Overlay>(id).need()?.mode else {
                return Err("unexpected overlay mode".into());
            };
            assert_eq!(*selected, (count - 1).saturating_sub(list_capacity(rows)));
        }
        input(&mut world, id, &key("escape"));
        assert!(world.get::<Overlay>(id).is_none());
    }
    for action in [Action::SaveLayout, Action::LoadLayout] {
        invoke(
            &mut world,
            id,
            target,
            Action::WorkspaceMenu,
            None,
            "",
            true,
        )?;
        let mut overlay = world.get_mut::<Overlay>(id).need()?;
        let Mode::List {
            entries, selected, ..
        } = &mut overlay.mode
        else {
            return Err("unexpected overlay mode".into());
        };
        *selected = entries.iter().position(|e| e.action == action).need()?;
        input(&mut world, id, &key("enter"));
        assert!(
            matches!(&world.get::<Overlay>(id).need()?.mode, Mode::Text { action: a, .. } if *a == action)
        );
        input(&mut world, id, &key("escape"));
    }
    Ok(())
}

#[test]
fn confirmation_captures_target_and_preserves_a_shared_process() -> crate::testing::Outcome {
    let (mut world, id, target, other) = setup();
    let process = world.get::<PaneView>(other).need()?.pane;
    invoke(&mut world, id, target, Action::Close, None, "", true)?;
    world.get_mut::<Viewer>(id).need()?.focus = Some(other);
    assert!(input(&mut world, id, &Input::Paste { text: "y".into() }));
    assert!(world.get_entity(target.leaf.need()?).is_ok());
    assert!(input(&mut world, id, &key("y")));
    assert!(world.get_entity(target.leaf.need()?).is_err());
    assert!(world.get_entity(other).is_ok());
    assert!(world.get_entity(process).is_ok());
    close(&mut world, other);
    assert!(world.get_entity(process).is_err());
    Ok(())
}

#[test]
fn removed_confirmation_target_cancels_without_retargeting() -> crate::testing::Outcome {
    let (mut world, id, target, other) = setup();
    invoke(&mut world, id, target, Action::Close, None, "", true)?;
    world.despawn(target.leaf.need()?);
    world.get_mut::<Viewer>(id).need()?.focus = Some(other);
    assert!(input(&mut world, id, &key("y")));
    assert!(world.get_entity(other).is_ok());
    assert!(world.get::<Overlay>(id).is_none());
    assert!(world.get::<Viewer>(id).need()?.notice.contains("cancelled"));
    Ok(())
}

#[test]
fn tab_close_keeps_an_empty_tab_and_workspace_close_repairs_other_viewers()
-> crate::testing::Outcome {
    let (mut world, id, target, _) = setup();
    invoke(&mut world, id, target, Action::TabClose, None, "", false)?;
    let v = world.get::<Viewer>(id).need()?;
    assert_ne!(v.tab, target.tab);
    assert!(v.focus.is_none());
    assert!(v.tab.is_some());
    let target = Target::viewer(v);
    let replacement = world.spawn(Workspace).id();
    invoke(
        &mut world,
        id,
        target,
        Action::WorkspaceClose,
        None,
        "",
        false,
    )?;
    assert_eq!(world.get::<Viewer>(id).need()?.workspace, replacement);
    let target = Target::viewer(world.get::<Viewer>(id).need()?);
    invoke(
        &mut world,
        id,
        target,
        Action::WorkspaceClose,
        None,
        "",
        false,
    )?;
    assert!(world.get_entity(id).is_err());
    Ok(())
}

#[test]
fn rearrangement_keeps_entity_identity_and_native_child_order() -> crate::testing::Outcome {
    let (mut world, id, target, other) = setup();
    let source = target.leaf.need()?;
    swap(&mut world, source, other)?;
    assert_eq!(
        navigation::leaves(&world, target.tab.need()?),
        vec![other, source]
    );
    move_beside(&mut world, source, other, "up")?;
    let split = world.get::<ChildOf>(source).need()?.parent();
    assert!(world.get::<Split>(split).is_some());
    assert_eq!(
        world
            .get::<Children>(split)
            .need()?
            .iter()
            .collect::<Vec<_>>(),
        vec![source, other]
    );
    invoke(
        &mut world,
        id,
        target,
        Action::MoveNewWorkspace,
        None,
        "destination",
        false,
    )?;
    let v = world.get::<Viewer>(id).need()?;
    assert_ne!(v.workspace, target.workspace);
    assert_eq!(v.focus, Some(source));
    assert!(world.get::<PaneView>(other).is_some());
    Ok(())
}

#[test]
fn rename_prompt_keeps_its_original_target_and_escape_cancels() -> crate::testing::Outcome {
    let (mut world, id, target, other) = setup();
    invoke(&mut world, id, target, Action::RenameTab, None, "", true)?;
    world.get_mut::<Viewer>(id).need()?.focus = Some(other);
    input(
        &mut world,
        id,
        &Input::Paste {
            text: "界 tab".into(),
        },
    );
    input(&mut world, id, &key("enter"));
    assert_eq!(
        world.get::<Name>(target.tab.need()?).need()?.as_str(),
        "界 tab"
    );
    invoke(&mut world, id, target, Action::Close, None, "", true)?;
    input(&mut world, id, &key("escape"));
    assert!(world.get::<Overlay>(id).is_none());
    assert!(world.get_entity(target.leaf.need()?).is_ok());
    Ok(())
}

#[test]
fn each_close_scope_requires_confirmation_and_cancel_keeps_its_hierarchy() -> crate::testing::Outcome
{
    for action in [Action::Close, Action::TabClose, Action::WorkspaceClose] {
        let (mut world, id, target, _) = setup();
        invoke(&mut world, id, target, action, None, "", true)?;
        assert!(world.get::<Overlay>(id).is_some());
        input(&mut world, id, &Input::Paste { text: "y".into() });
        assert!(world.get_entity(target.leaf.need()?).is_ok());
        input(&mut world, id, &key("n"));
        assert!(world.get_entity(target.leaf.need()?).is_ok());
        invoke(&mut world, id, target, action, None, "", true)?;
        input(&mut world, id, &key("y"));
        assert!(world.get_entity(target.leaf.need()?).is_err());
        assert_eq!(
            world.get_entity(target.workspace).is_err(),
            action == Action::WorkspaceClose
        );
    }
    Ok(())
}

#[test]
fn stale_chooser_destination_never_changes_the_source_or_an_unrelated_target()
-> crate::testing::Outcome {
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
        world.get::<ChildOf>(target.leaf.need()?).need()?.parent(),
        target.tab.need()?
    );
    assert!(world.get::<Viewer>(id).need()?.notice.contains("removed"));
    Ok(())
}

#[test]
fn every_chooser_entry_remains_visible_on_tiny_terminals() -> crate::testing::Outcome {
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
    Ok(())
}

#[test]
fn pasted_text_cannot_cross_a_cancelled_or_reopened_prompt() -> crate::testing::Outcome {
    let (mut world, id, target, _) = setup();
    invoke(&mut world, id, target, Action::RenameTab, None, "", true)?;
    assert!(crate::paste::input(&mut world, id, &Input::PasteBegin));
    input(&mut world, id, &key("escape"));
    invoke(&mut world, id, target, Action::RenameTab, None, "", true)?;
    assert!(crate::paste::input(
        &mut world,
        id,
        &Input::Paste {
            text: "old prompt text".into()
        }
    ));
    let Mode::Text { buffer, .. } = &world.get::<Overlay>(id).need()?.mode else {
        return Err("unexpected overlay mode".into());
    };
    assert!(buffer.is_empty());
    Ok(())
}

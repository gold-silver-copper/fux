use super::*;
use crate::testing::*;

fn setup() -> (World, Entity, Target, Entity) {
    let mut world = World::new();
    navigation::observe(&mut world);
    let workspace = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(workspace))).id();
    let pane = world.spawn_empty().id();
    let leaf = world.spawn((PaneView { pane }, ChildOf(tab))).id();
    let other = world.spawn((PaneView { pane }, ChildOf(tab))).id();
    let id = world
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
            Focused(leaf),
        ))
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
        // A misspelled test key becomes F12, which no test binds or expects.
        key: key.parse().unwrap_or(Key::F(12)),
        modifiers: Modifiers::default(),
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
        bound(&mut world, id, target, menu)?;
        let Mode::List { entries, .. } = &world.get::<Overlay>(id).need()?.mode else {
            return Err("unexpected overlay mode".into());
        };
        for action in required {
            assert!(
                entries
                    .iter()
                    .any(|e| matches!(e.run, Run::Action(a) if a == action)),
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
        bound(&mut world, id, target, Action::WorkspaceMenu)?;
        let mut overlay = world.get_mut::<Overlay>(id).need()?;
        let Mode::List {
            entries, selected, ..
        } = &mut overlay.mode
        else {
            return Err("unexpected overlay mode".into());
        };
        *selected = entries
            .iter()
            .position(|e| matches!(e.run, Run::Action(a) if a == action))
            .need()?;
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
    bound(&mut world, id, target, Action::Close)?;
    world.entity_mut(id).insert(Focused(other));
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
    bound(&mut world, id, target, Action::Close)?;
    world.despawn(target.leaf.need()?);
    world.entity_mut(id).insert(Focused(other));
    assert!(input(&mut world, id, &key("y")));
    assert!(world.get_entity(other).is_ok());
    assert!(world.get::<Overlay>(id).is_none());
    assert!(
        world
            .get::<Viewer>(id)
            .need()?
            .notice
            .as_ref()
            .is_some_and(|n| n.text.contains("cancelled"))
    );
    Ok(())
}

#[test]
fn tab_close_keeps_an_empty_tab_and_workspace_close_repairs_other_viewers()
-> crate::testing::Outcome {
    let (mut world, id, target, _) = setup();
    crate::server::execute(
        &mut world,
        id,
        Command::Close {
            subject: Subject::Tab(target.tab.need()?),
        },
    )?;
    assert_ne!(on_tab(&world, id), target.tab);
    assert!(focused(&world, id).is_none());
    assert!(on_tab(&world, id).is_some());
    let target = Target::of(&world, id).need()?;
    let replacement = world.spawn(Workspace).id();
    crate::server::execute(
        &mut world,
        id,
        Command::Close {
            subject: Subject::Workspace(target.workspace),
        },
    )?;
    assert_eq!(viewing(&world, id), Some(replacement));
    let target = Target::of(&world, id).need()?;
    crate::server::execute(
        &mut world,
        id,
        Command::Close {
            subject: Subject::Workspace(target.workspace),
        },
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
    move_beside(&mut world, source, other, Direction::Up)?;
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
    crate::server::execute(
        &mut world,
        id,
        Command::Move {
            to: MoveTo::NewWorkspace {
                name: Some("destination".into()),
            },
        },
    )?;
    assert_ne!(viewing(&world, id), Some(target.workspace));
    assert_eq!(focused(&world, id), Some(source));
    assert!(world.get::<PaneView>(other).is_some());
    Ok(())
}

#[test]
fn rename_prompt_keeps_its_original_target_and_escape_cancels() -> crate::testing::Outcome {
    let (mut world, id, target, other) = setup();
    bound(&mut world, id, target, Action::RenameTab)?;
    world.entity_mut(id).insert(Focused(other));
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
    bound(&mut world, id, target, Action::Close)?;
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
        bound(&mut world, id, target, action)?;
        assert!(world.get::<Overlay>(id).is_some());
        input(&mut world, id, &Input::Paste { text: "y".into() });
        assert!(world.get_entity(target.leaf.need()?).is_ok());
        input(&mut world, id, &key("n"));
        assert!(world.get_entity(target.leaf.need()?).is_ok());
        bound(&mut world, id, target, action)?;
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
                run: Run::Command(Command::Move {
                    to: MoveTo::Tab { tab: destination },
                }),
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
    assert!(
        world
            .get::<Viewer>(id)
            .need()?
            .notice
            .as_ref()
            .is_some_and(|n| n.text.contains("removed"))
    );
    Ok(())
}

#[test]
fn every_chooser_entry_remains_visible_on_tiny_terminals() -> crate::testing::Outcome {
    let (world, _, target, _) = setup();
    let tab = target.tab.need()?;
    for rows in 0..10 {
        for selected in 0..10 {
            let entries = (0..10)
                .map(|i| Entry {
                    label: format!("entry-{i}"),
                    run: Run::Command(Command::Select {
                        scope: Scope::Tab,
                        entity: tab,
                    }),
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
    bound(&mut world, id, target, Action::RenameTab)?;
    assert!(crate::paste::input(&mut world, id, &Input::PasteBegin));
    input(&mut world, id, &key("escape"));
    bound(&mut world, id, target, Action::RenameTab)?;
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

#[test]
fn menu_titles_and_entries_keep_their_order() -> crate::testing::Outcome {
    use Action::*;
    let (mut world, id, target, _) = setup();
    for (menu, title, expected) in [
        (
            PaneMenu,
            "Panes: ",
            vec![
                SplitHorizontal,
                SplitVertical,
                RenamePane,
                Close,
                Terminate,
                Zoom,
                GrowWidth,
                ShrinkWidth,
                GrowHeight,
                ShrinkHeight,
                ReorderPrev,
                ReorderNext,
                SwapChoose,
                SwapLeft,
                SwapRight,
                SwapUp,
                SwapDown,
                MoveLeft,
                MoveRight,
                MoveUp,
                MoveDown,
                MoveTab,
                MoveNewTab,
                MoveWorkspace,
                MoveNewWorkspace,
                CopyMode,
                ScrollUp,
                ScrollDown,
                Copy,
            ],
        ),
        (
            TabMenu,
            "Tabs: ",
            vec![
                TabNew,
                RenameTab,
                TabClose,
                TabReorderPrevious,
                TabReorderNext,
            ],
        ),
        (
            WorkspaceMenu,
            "Workspaces: ",
            vec![
                WorkspaceNew,
                WorkspaceChoose,
                RenameWorkspace,
                WorkspaceClose,
                WorkspaceReorderPrevious,
                WorkspaceReorderNext,
                SaveLayout,
                LoadLayout,
            ],
        ),
    ] {
        bound(&mut world, id, target, menu)?;
        let Mode::List {
            title: t, entries, ..
        } = &world.get::<Overlay>(id).need()?.mode
        else {
            return Err("unexpected overlay mode".into());
        };
        assert!(t.starts_with(title), "{menu:?}: {t}");
        let actions: Vec<_> = entries
            .iter()
            .filter_map(|e| match e.run {
                Run::Action(a) => Some(a),
                Run::Command(_) => None,
            })
            .collect();
        assert_eq!(actions, expected, "{menu:?}");
        assert!(
            entries
                .iter()
                .zip(&expected)
                .all(|(e, a)| e.label == a.label()),
            "{menu:?}"
        );
        input(&mut world, id, &key("escape"));
    }
    Ok(())
}

#[test]
fn close_confirmation_names_the_subject_and_what_it_removes() -> crate::testing::Outcome {
    let (world, _, target, _) = setup();
    let tab = target.tab.need()?;
    let leaf = target.leaf.need()?;
    for (subject, heading, consequence) in [
        (
            Subject::Pane(leaf),
            "Close pane ",
            "Remove pane; stop if last view",
        ),
        (
            Subject::Tab(tab),
            "Close tab ",
            "Remove all contained pane views",
        ),
        (
            Subject::Workspace(target.workspace),
            "Close workspace ",
            "Remove all contained pane views",
        ),
    ] {
        let overlay = Overlay {
            serial: 0,
            target,
            mode: Mode::Confirm {
                command: Command::Close { subject },
            },
        };
        let rendered = lines(&world, &overlay, 24);
        let text: Vec<&str> = rendered.iter().map(|(t, _)| t.as_str()).collect();
        assert!(text.first().need()?.starts_with(heading), "{subject:?}");
        assert_eq!(text.get(1), Some(&consequence), "{subject:?}");
    }
    // A confirmation of anything that is not a close keeps the neutral wording.
    let other = Overlay {
        serial: 0,
        target,
        mode: Mode::Confirm {
            command: Command::Zoom,
        },
    };
    let rendered = lines(&world, &other, 24);
    let text: Vec<&str> = rendered.iter().map(|(t, _)| t.as_str()).collect();
    assert!(text.first().need()?.starts_with("Close target "));
    assert_eq!(text.get(1), Some(&"Remove all contained pane views"));
    Ok(())
}

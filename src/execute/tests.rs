use super::*;
use crate::{actions::Action, testing::*};

#[test]
fn execution_keeps_its_guard_order_and_settles_ui_before_failure() -> crate::testing::Outcome {
    let mut world = World::new();
    let root = world.spawn(Workspace).id();
    world.spawn((Tab, ChildOf(root)));
    let id = world
        .spawn((
            Viewer {
                rows: 24,
                cols: 80,
                zoom: false,
                scrollback: 0,
                notice: Notice::info("old"),
            },
            Viewing(root),
            Prefix::default(),
        ))
        .id();
    let target = crate::actions::Target::of(&world, id).need()?;
    assert_eq!(
        crate::actions::unavailable(&world, target, Action::MoveLeft),
        Some("no pane")
    );
    let command = Command::MoveDirection {
        direction: crate::protocol::Direction::Left,
    };
    assert_eq!(
        execute(&mut world, id, command),
        Err("only one pane".into())
    );
    assert!(world.get::<Prefix>(id).is_none());
    assert!(world.get::<Viewer>(id).need()?.notice.is_none());
    assert_eq!(
        execute(&mut world, id, Command::ReorderPane { order: Order::Next }),
        Err("no pane".into())
    );
    assert_eq!(
        execute(&mut world, id, Command::Next { scope: Scope::Tab }),
        Err("only one tab".into())
    );
    assert!(
        execute(
            &mut world,
            id,
            Command::Next {
                scope: Scope::Workspace
            }
        )
        .is_ok()
    );
    Ok(())
}

/// `terminate` ends the process and reaps it inside the command. The status a
/// BRP caller reads must say so in the same step: a query landing after the
/// command and before the next update used to see `running` with the pid of a
/// process already gone, which is what made
/// `blocked_terminal_paint_does_not_block_stream_drain` fail on loaded
/// machines, and what an agent polling after a terminate would see.
#[test]
fn terminate_publishes_the_exit_in_the_same_step() -> crate::testing::Outcome {
    use crate::terminal::TerminalPlugin;
    let mut app = bevy_app::App::new();
    app.insert_resource(Wake(std::thread::current()))
        .add_plugins((bevy_app::TaskPoolPlugin::default(), TerminalPlugin));
    let world = app.world_mut();
    let pane = world
        .spawn(Launch {
            argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
            cwd: String::new(),
            history_lines: 20,
        })
        .id();
    let root = world.spawn(Workspace).id();
    let tab = world.spawn((Tab, ChildOf(root))).id();
    let leaf = world.spawn((PaneView { pane }, ChildOf(tab))).id();
    let viewer = world
        .spawn((
            Viewer {
                rows: 24,
                cols: 80,
                zoom: false,
                scrollback: 0,
                notice: None,
            },
            Viewing(root),
            OnTab(tab),
            Focused(leaf),
        ))
        .id();
    app.update();
    let running = |state: &ProcessState| match state.status {
        Status::Running { pid, .. } => Some(pid),
        _ => None,
    };
    let pid = app
        .world()
        .get::<ProcessState>(pane)
        .and_then(running)
        .need()?;
    execute(app.world_mut(), viewer, Command::Terminate)?;
    // No update in between: this is what a query right after the command reads.
    let state = app.world().get::<ProcessState>(pane).need()?;
    assert!(
        matches!(state.status, Status::Exited { .. }),
        "still published as {:?} after pid {pid} was reaped",
        state.status
    );
    Ok(())
}

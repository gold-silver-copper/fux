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

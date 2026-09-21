use super::*;
use crate::testing::Outcome;
use serde_json::{Value, json};

fn round_trip(command: Command, expected: Value) -> Outcome {
    assert_eq!(serde_json::to_value(&command)?, expected);
    assert_eq!(serde_json::from_value::<Command>(expected)?, command);
    Ok(())
}

#[test]
fn scoped_commands_have_exact_wire_shapes() -> Outcome {
    let entity = Entity::from_bits(42);
    for (scope, name, subject) in [
        (Scope::Tab, "tab", Subject::Tab(entity)),
        (Scope::Workspace, "workspace", Subject::Workspace(entity)),
    ] {
        round_trip(
            Command::Close { subject },
            json!({"kind":"close","subject":{name:42}}),
        )?;
        round_trip(
            Command::Select { scope, entity },
            json!({"kind":"select","scope":name,"entity":42}),
        )?;
        round_trip(Command::Next { scope }, json!({"kind":"next","scope":name}))?;
        round_trip(
            Command::Previous { scope },
            json!({"kind":"previous","scope":name}),
        )?;
        for (order, label) in [(Order::Previous, "previous"), (Order::Next, "next")] {
            round_trip(
                Command::Reorder { scope, order },
                json!({"kind":"reorder","scope":name,"order":label}),
            )?;
            round_trip(
                Command::ReorderPane { order },
                json!({"kind":"reorder_pane","order":label}),
            )?;
        }
    }
    for (to, expected) in [
        (MoveTo::Tab { tab: entity }, json!({"kind":"tab","tab":42})),
        (
            MoveTo::Workspace { workspace: entity },
            json!({"kind":"workspace","workspace":42}),
        ),
        (
            MoveTo::NewTab { name: None },
            json!({"kind":"new_tab","name":null}),
        ),
        (
            MoveTo::NewWorkspace {
                name: Some("work".into()),
            },
            json!({"kind":"new_workspace","name":"work"}),
        ),
    ] {
        round_trip(Command::Move { to }, json!({"kind":"move","to":expected}))?;
    }
    Ok(())
}

#[test]
fn removed_shapes_and_invalid_scopes_are_rejected() {
    for kind in [
        "tab_select",
        "workspace_select",
        "tab_next",
        "workspace_next",
        "tab_previous",
        "workspace_previous",
        "tab_reorder",
        "workspace_reorder",
        "tab_close",
        "workspace_close",
        "move_to_tab",
        "move_to_new_tab",
        "move_to_workspace",
        "move_to_new_workspace",
    ] {
        let old = json!({"kind":kind,"tab":42,"workspace":42,"order":"next","name":null});
        assert!(serde_json::from_value::<Command>(old).is_err(), "{kind}");
    }
    for invalid in [
        json!({"kind":"reorder","order":"next"}),
        json!({"kind":"next"}),
        json!({"kind":"next","scope":"pane"}),
        json!({"kind":"select","scope":"tab","tab":42}),
        json!({"kind":"select","scope":"tab","entity":"42"}),
        json!({"kind":"move","to":{"tab":42}}),
        json!({"kind":"move","to":{"kind":"tab"}}),
    ] {
        assert!(
            serde_json::from_value::<Command>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

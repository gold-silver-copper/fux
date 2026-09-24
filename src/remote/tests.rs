//! The guard, finding by finding (docs/prompt-brp-policy.md): each test sends
//! the request that broke fux before the guard, through the method registry
//! the server uses, and requires a refusal that says why, an unchanged world,
//! and a server that still obeys. The powers the guard keeps have tests too.
use super::property::{call, check, fixture};
use crate::{model::*, testing::*};
use bevy_ecs::prelude::*;
use serde_json::{Value, json};

fn bits(entity: Entity) -> Value {
    json!(entity.to_bits())
}

fn first<F: bevy_ecs::query::QueryFilter>(world: &mut World) -> Result<Entity, String> {
    world
        .query_filtered::<Entity, F>()
        .iter(world)
        .next()
        .need()
}

fn viewer(world: &mut World) -> Result<Entity, String> {
    first::<IsViewer>(world)
}

/// Requires `method` to be refused with a message containing `reason` (or
/// one of several, separated by `|`, when a request breaks more than one
/// rule), and the world to pass every check afterwards.
fn refused(world: &mut World, method: &str, params: Value, reason: &str) -> Outcome {
    let error = call(world, method, Some(params))
        .err()
        .ok_or_else(|| format!("{method} was accepted"))?;
    assert!(
        reason.split('|').any(|r| error.contains(r)),
        "{method}: {error}"
    );
    check(world)?;
    Ok(())
}

/// 022: the three requests whose stock handlers panicked get real errors.
#[test]
fn requests_that_panicked_are_refused_with_a_reason() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let viewer = viewer(world)?;
    let focus = world.get::<Focused>(viewer).need()?.0;
    let tab = world.get::<OnTab>(viewer).need()?.0;
    for component in [
        "fux::model::Focused",
        "fux::model::Viewing",
        "fux::model::OnTab",
        "bevy_ecs::hierarchy::ChildOf",
    ] {
        let entity = if component.contains("ChildOf") {
            focus
        } else {
            viewer
        };
        refused(
            world,
            "world.mutate_components",
            json!({"entity":bits(entity),"component":component,"path":"","value":{}}),
            "immutable",
        )?;
    }
    refused(
        world,
        "world.reparent_entities",
        json!({"entities":[bits(focus), 6_450_128_621_605_086_059u64],"parent":bits(tab)}),
        "not found",
    )?;
    for event in ["fux::control::Control", "fux::control::UserInput"] {
        let error = call(
            world,
            "world.trigger_event",
            Some(json!({"event":event,"value":null})),
        )
        .err()
        .need()?;
        assert!(
            error.contains("invalid") || error.contains("not a complete"),
            "{error}"
        );
    }
    check(world)?;
    Ok(())
}

/// 023: a view cannot name something that is not a process.
#[test]
fn a_view_must_show_a_process() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let viewer = viewer(world)?;
    let focus = world.get::<Focused>(viewer).need()?.0;
    let workspace = world.get::<Viewing>(viewer).need()?.0;
    refused(
        world,
        "world.insert_components",
        json!({"entity":bits(focus),"components":{"fux::model::PaneView":{"pane":bits(workspace)}}}),
        "must show a process",
    )?;
    assert!(world.get::<Workspace>(workspace).is_some());
    Ok(())
}

/// 024: a placed tab, split or view cannot be taken out of the hierarchy,
/// and nothing can be put where fux has no rules for it.
#[test]
fn the_hierarchy_cannot_be_broken() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let viewer = viewer(world)?;
    let tab = world.get::<OnTab>(viewer).need()?.0;
    let focus = world.get::<Focused>(viewer).need()?.0;
    let workspace = world.get::<Viewing>(viewer).need()?.0;
    refused(
        world,
        "world.reparent_entities",
        json!({"entities":[bits(tab)],"parent":null}),
        "must be a child of a workspace",
    )?;
    refused(
        world,
        "world.reparent_entities",
        json!({"entities":[bits(focus)],"parent":null}),
        "must be a child of a tab or a split",
    )?;
    refused(
        world,
        "world.remove_components",
        json!({"entity":bits(focus),"components":["bevy_ecs::hierarchy::ChildOf"]}),
        "must be a child of a tab or a split",
    )?;
    refused(
        world,
        "world.reparent_entities",
        json!({"entities":[bits(workspace)],"parent":bits(tab)}),
        "cannot have a parent|inside its own subtree",
    )?;
    refused(
        world,
        "world.reparent_entities",
        json!({"entities":[bits(tab)],"parent":bits(focus)}),
        "inside its own subtree",
    )?;
    refused(
        world,
        "world.insert_components",
        json!({"entity":bits(focus),"components":{"fux::model::Tab":{}}}),
        "more than one layout role",
    )?;
    // Layout roles go with their entity; a split's may go while what it holds
    // stays placed, which it cannot while it holds panes.
    refused(
        world,
        "world.remove_components",
        json!({"entity":bits(tab),"components":["fux::model::Tab"]}),
        "removed only by despawning",
    )?;
    let split = world
        .query_filtered::<Entity, (With<Split>, With<Children>, Without<Tab>)>()
        .iter(world)
        .next()
        .need()?;
    refused(
        world,
        "world.remove_components",
        json!({"entity":bits(split),"components":["fux::model::Split"]}),
        "must be a child of a tab or a split",
    )?;
    refused(
        world,
        "world.spawn_entity",
        json!({"components":{"fux::model::Tab":{},"bevy_ecs::hierarchy::ChildOf":bits(focus)}}),
        "must be a child of a workspace",
    )?;
    // A new tab may start unplaced and be put in a workspace next.
    let spawned = call(
        world,
        "world.spawn_entity",
        Some(json!({"components":{"fux::model::Tab":{}}})),
    )?;
    let spawned = spawned.get("entity").cloned().need()?;
    check(world)?;
    call(
        world,
        "world.reparent_entities",
        Some(json!({"entities":[spawned],"parent":bits(workspace)})),
    )?;
    check(world)?;
    Ok(())
}

/// 025: every workspace has an order, all distinct.
#[test]
fn workspace_order_is_kept_whole() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let viewer = viewer(world)?;
    let workspace = world.get::<Viewing>(viewer).need()?.0;
    let taken = world.get::<WorkspaceOrder>(workspace).need()?.0;
    // A spawn without one gets the next free order.
    let spawned = call(
        world,
        "world.spawn_entity",
        Some(json!({"components":{"fux::model::Workspace":{}}})),
    )?;
    let spawned = serde_json::from_value::<Entity>(spawned.get("entity").cloned().need()?)?;
    let order = world.get::<WorkspaceOrder>(spawned).need()?.0;
    assert!(order > taken);
    check(world)?;
    refused(
        world,
        "world.insert_components",
        json!({"entity":bits(spawned),"components":{"fux::model::WorkspaceOrder":taken}}),
        "already has order",
    )?;
    refused(
        world,
        "world.remove_components",
        json!({"entity":bits(spawned),"components":["fux::model::WorkspaceOrder"]}),
        "cannot be removed",
    )?;
    Ok(())
}

/// 026: a process's state stays; clients resize it within bounds.
#[test]
fn process_state_is_kept_and_bounded() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let process = first::<With<Launch>>(world)?;
    refused(
        world,
        "world.remove_components",
        json!({"entity":bits(process),"components":["fux::model::ProcessState"]}),
        "cannot be removed",
    )?;
    refused(
        world,
        "world.mutate_components",
        json!({"entity":bits(process),"component":"fux::model::ProcessState","path":".rows","value":5000}),
        "outside 1..=4096",
    )?;
    refused(
        world,
        "world.mutate_components",
        json!({"entity":bits(process),"component":"fux::model::ProcessState","path":".revision","value":7}),
        "written by fux",
    )?;
    // Resizing through the state is documented power, and keeps working.
    call(
        world,
        "world.mutate_components",
        Some(
            json!({"entity":bits(process),"component":"fux::model::ProcessState","path":".rows","value":12}),
        ),
    )?;
    assert_eq!(world.get::<ProcessState>(process).need()?.rows, 12);
    Ok(())
}

/// 027: viewers come only from fux.attach; removing one is a clean detach.
#[test]
fn viewers_are_attached_and_detach_cleanly() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let viewer_value = json!({"rows":10,"cols":30,"zoom":false,"scrollback":0,"notice":null});
    refused(
        world,
        "world.spawn_entity",
        json!({"components":{"fux::model::Viewer":viewer_value}}),
        "cannot be spawned",
    )?;
    let plain = call(
        world,
        "world.spawn_entity",
        Some(json!({"components":{"bevy_ecs::name::Name":"plain"}})),
    )?;
    let plain = serde_json::from_value::<Entity>(plain.get("entity").cloned().need()?)?;
    refused(
        world,
        "world.insert_components",
        json!({"entity":bits(plain),"components":{"fux::model::Viewer":viewer_value}}),
        "fux.attach",
    )?;
    let viewer = viewer(world)?;
    call(
        world,
        "world.remove_components",
        Some(json!({"entity":bits(viewer),"components":["fux::model::Viewer"]})),
    )?;
    assert!(world.get::<Viewing>(viewer).is_none());
    assert!(world.get::<OnTab>(viewer).is_none());
    check(world)?;
    Ok(())
}

/// 028: entities fux and Bevy keep internally cannot be changed.
#[test]
fn internal_entities_cannot_be_changed() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let observer = first::<With<bevy_ecs::observer::Observer>>(world)?;
    refused(
        world,
        "world.despawn_entity",
        json!({"entity":bits(observer)}),
        "internal to fux or Bevy",
    )?;
    refused(
        world,
        "world.insert_components",
        json!({"entity":bits(observer),"components":{"bevy_ecs::name::Name":"x"}}),
        "internal to fux or Bevy",
    )?;
    let resource = first::<With<bevy_ecs::resource::IsResource>>(world)?;
    refused(
        world,
        "world.despawn_entity",
        json!({"entity":bits(resource)}),
        "holds a resource",
    )?;
    Ok(())
}

/// 029: nothing without a layout role goes under a layout node, and a viewer
/// that cannot act is told why.
#[test]
fn layout_holds_only_layout_and_failures_are_told() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let viewer = viewer(world)?;
    let tab = world.get::<OnTab>(viewer).need()?.0;
    refused(
        world,
        "world.spawn_entity",
        json!({"components":{"bevy_ecs::hierarchy::ChildOf":bits(tab),"bevy_ecs::name::Name":"junk"}}),
        "cannot be a child of a tab",
    )?;
    refused(
        world,
        "world.insert_components",
        json!({"entity":bits(tab),"components":{"fux::model::Viewing":bits(tab)}}),
        "is not a viewer",
    )?;
    // Past the guard (a direct world edit), the viewer is told why it cannot act.
    world.spawn((crate::interaction::Prefix::default(), ChildOf(tab)));
    world.flush();
    world.trigger(crate::control::Control {
        viewer,
        command: crate::control::Command::Zoom,
    });
    world.flush();
    let notice = world.get::<Viewer>(viewer).need()?.notice.clone().need()?;
    assert!(
        notice.error && notice.text.starts_with("cannot act here"),
        "{}",
        notice.text
    );
    Ok(())
}

/// The README's way to show a custom process still works: spawn a Launch,
/// spawn an unplaced view of it, then reparent the view under a tab.
#[test]
fn the_documented_custom_pane_flow_still_works() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let viewer = viewer(world)?;
    let tab = world.get::<OnTab>(viewer).need()?.0;
    let process = call(
        world,
        "world.spawn_entity",
        Some(
            json!({"components":{"fux::model::Launch":{"argv":["/bin/sleep","600"],"cwd":"/","history_lines":10}}}),
        ),
    )?;
    let process = process.get("entity").cloned().need()?;
    let view = call(
        world,
        "world.spawn_entity",
        Some(json!({"components":{"fux::model::PaneView":{"pane":process}}})),
    )?;
    let view = view.get("entity").cloned().need()?;
    check(world)?;
    call(
        world,
        "world.reparent_entities",
        Some(json!({"entities":[view],"parent":bits(tab)})),
    )?;
    check(world)?;
    Ok(())
}

/// Values are validated whatever the method.
#[test]
fn values_are_validated() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let viewer = viewer(world)?;
    let focus = world.get::<Focused>(viewer).need()?.0;
    refused(
        world,
        "world.insert_components",
        json!({"entity":bits(focus),"components":{"bevy_ecs::name::Name":"\u{1b}]52;c;x\u{7}"}}),
        "control characters",
    )?;
    refused(
        world,
        "world.mutate_components",
        json!({"entity":bits(viewer),"component":"fux::model::Viewer","path":".rows","value":60000}),
        "outside 0..=4096",
    )?;
    // Reflection applies a list element by element and never shortens it,
    // so `.shell = []` leaves the shell as it was; a field that does change
    // shows the validation.
    call(
        world,
        "world.mutate_resources",
        Some(json!({"resource":"fux::assets::Settings","path":".shell","value":[]})),
    )?;
    assert!(!world.resource::<crate::assets::Settings>().shell.is_empty());
    refused(
        world,
        "world.mutate_resources",
        json!({"resource":"fux::assets::Settings","path":".prefix","value":""}),
        "must not be empty",
    )?;
    refused(
        world,
        "world.remove_resources",
        json!({"resource":"fux::assets::Settings"}),
        "cannot be removed",
    )?;
    refused(
        world,
        "world.write_message",
        json!({"message":"bevy_app::AppExit","value":null}),
        "no message",
    )?;
    Ok(())
}

/// The README table and `fux.policy` are the same table.
#[test]
fn fux_policy_serves_the_table() -> Outcome {
    let mut app = fixture()?;
    let served = call(app.world_mut(), "fux.policy", None)?;
    let rows = served.get("types").and_then(Value::as_array).need()?;
    assert!(
        rows.iter()
            .any(|row| row.get("type") == Some(&json!("fux::model::Launch")))
    );
    assert_eq!(rows.len(), crate::policy::table(app.world()).len());
    Ok(())
}

/// The guard names the entity a spawn creates with Bevy's placeholder. A
/// client that sends the placeholder's bits as a reference must not be taken
/// to mean that entity: it names nothing, and is refused like any missing one.
#[test]
fn the_placeholder_is_not_a_reference() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let placeholder = bits(Entity::PLACEHOLDER);
    let plain = call(
        world,
        "world.spawn_entity",
        Some(json!({"components":{"bevy_ecs::name::Name":"plain"}})),
    )?;
    let plain = plain.get("entity").cloned().need()?;
    refused(
        world,
        "world.insert_components",
        json!({"entity":plain,"components":{"bevy_ecs::hierarchy::ChildOf":placeholder}}),
        "does not exist",
    )?;
    refused(
        world,
        "world.spawn_entity",
        json!({"components":{"bevy_ecs::name::Name":"x","bevy_ecs::hierarchy::ChildOf":placeholder}}),
        "does not exist",
    )?;
    refused(
        world,
        "world.reparent_entities",
        json!({"entities":[plain],"parent":placeholder}),
        "not found",
    )?;
    Ok(())
}

/// The guard is the only way in: the server registers the guarded form of
/// every stock method and fux's own methods, and nothing else.
#[test]
fn every_method_is_guarded() -> Outcome {
    let app = fixture()?;
    let mut served = app
        .world()
        .resource::<bevy_remote::RemoteMethods>()
        .methods();
    served.sort();
    let mut expected: Vec<String> = super::guard::GUARDED
        .iter()
        .chain(&[
            "fux.policy",
            "fux.invariants",
            "fux.attach",
            "fux.frame",
            "fux.frame+watch",
        ])
        .map(|m| (*m).to_owned())
        .collect();
    expected.sort();
    assert_eq!(served, expected);
    // Each stock name runs the guard's system, not the stock one: the guard
    // refuses a resource entity even for a read, which stock answers.
    let resource = app
        .world()
        .iter_entities()
        .find(|e| e.contains::<bevy_ecs::resource::IsResource>())
        .map(|e| e.id())
        .need()?;
    let mut app = app;
    let world = app.world_mut();
    for method in [
        "world.get_components",
        "world.list_components",
        "world.get_components+watch",
        "world.list_components+watch",
    ] {
        refused(
            world,
            method,
            json!({"entity":bits(resource),"components":["fux::model::Viewer"]}),
            "holds a resource",
        )?;
    }
    refused(
        world,
        "world.observe+watch",
        json!({"event":"fux::control::Control","entity":bits(resource)}),
        "holds a resource",
    )?;
    Ok(())
}

/// A Launch names a program and a history a terminal can hold.
#[test]
fn launch_and_viewer_values_are_bounded() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    for (launch, reason) in [
        (
            json!({"argv":[],"cwd":"/","history_lines":0}),
            "argv must start with a program",
        ),
        (
            json!({"argv":[""],"cwd":"/","history_lines":0}),
            "argv must start with a program",
        ),
        (
            json!({"argv":["sleep","600"],"cwd":"/","history_lines":u64::MAX}),
            "more than a terminal can hold",
        ),
    ] {
        refused(
            world,
            "world.spawn_entity",
            json!({"components":{"fux::model::Launch":launch}}),
            reason,
        )?;
    }
    let spawned = call(
        world,
        "world.spawn_entity",
        Some(
            json!({"components":{"fux::model::Launch":{"argv":["sleep","600"],"cwd":"/","history_lines":100}}}),
        ),
    )?;
    let process = spawned.get("entity").cloned().need()?;
    refused(
        world,
        "world.mutate_components",
        json!({"entity":process,"component":"fux::model::Launch","path":".argv[0]","value":""}),
        "argv must start with a program",
    )?;
    // Scrollback past the history is kept, and painting clamps it.
    let viewer = viewer(world)?;
    call(
        world,
        "world.mutate_components",
        Some(
            json!({"entity":bits(viewer),"component":"fux::model::Viewer","path":".scrollback","value":1_000_000_000u64}),
        ),
    )?;
    check(world)?;
    Ok(())
}

/// 031: a tab spawned straight into a workspace, in one request, is a tab of
/// that workspace. The stock handler inserts a request's components one at a
/// time in hash order, and when `ChildOf` came first, fux wrapped the still
/// roleless entity in a new tab before `Tab` arrived: a tab inside a tab.
#[test]
fn a_tab_spawned_into_a_workspace_is_its_tab() -> Outcome {
    let mut app = fixture()?;
    let world = app.world_mut();
    let viewer = viewer(world)?;
    let workspace = world.get::<Viewing>(viewer).need()?.0;
    for n in 0..24 {
        let spawned = call(
            world,
            "world.spawn_entity",
            Some(json!({"components":{
                "fux::model::Tab":{},
                "bevy_ecs::hierarchy::ChildOf":bits(workspace),
                "bevy_ecs::name::Name":format!("tab {n}"),
            }})),
        )?;
        let tab = spawned.get("entity").and_then(Value::as_u64).need()?;
        let tab = Entity::try_from_bits(tab).need()?;
        assert_eq!(
            world.get::<ChildOf>(tab).map(ChildOf::parent),
            Some(workspace),
            "tab {n}"
        );
        check(world)?;
    }
    Ok(())
}

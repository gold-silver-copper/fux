use super::*;

const PREFIX: &str = "fux::interaction::Prefix";
const OVERLAY: &str = "fux::interaction::Overlay";

fn component(server: &Server, viewer: u64, name: &str) -> Result<Value, String> {
    Ok(server
        .rpc(
            "world.get_components",
            json!({"entity":viewer,"components":[name],"strict":true}),
        )?
        .at(name))
}

fn key(server: &Server, viewer: u64, key: &str) -> Result<(), String> {
    server.input(
        viewer,
        json!({"kind":"key","key":key,"ctrl":false,"alt":false,"shift":false}),
    )
}

#[test]
fn stock_brp_inspects_existing_interactions_and_raw_indices() -> Outcome {
    let server = Server::start()?;
    let viewer = server.attach()?;
    let other = server.attach()?;
    let schemas = server.rpc("registry.schema", Value::Null)?;
    for name in [
        PREFIX,
        OVERLAY,
        "fux::interaction::Mode",
        "fux::interaction::Run",
        "fux::interaction::Entry",
        "fux::actions::Target",
    ] {
        assert!(schemas.get(name).is_some(), "missing schema {name}");
    }
    let methods = server.rpc("rpc.discover", Value::Null)?.to_string();
    assert!(methods.contains("world.get_components+watch"));

    server.command(viewer, "help")?;
    assert_eq!(component(&server, viewer, PREFIX)?.at("scroll"), 0);
    key(&server, viewer, "down")?;
    assert_eq!(component(&server, viewer, PREFIX)?.at("scroll"), 1);
    assert_eq!(server.query(PREFIX)?.rows().count(), 1);
    assert!(component(&server, other, PREFIX).is_err());
    server.rpc(
        "world.insert_components",
        json!({"entity":viewer,"components":{PREFIX:{"scroll":usize::MAX}}}),
    )?;
    server.screen(viewer)?;
    key(&server, viewer, "down")?;
    assert!(
        component(&server, viewer, PREFIX)?
            .at("scroll")
            .as_u64()
            .need()?
            < 1000
    );
    key(&server, viewer, "escape")?;
    assert!(component(&server, viewer, PREFIX).is_err());

    let pane = server.focused(viewer)?.as_u64().need()?;
    server.control(viewer, json!({"kind":"menu","subject":{"pane":pane}}))?;
    let mut overlay = component(&server, viewer, OVERLAY)?;
    assert_eq!(overlay.at("target").at("leaf"), pane);
    let list = overlay.at("mode").at("List");
    assert_eq!(list.at("selected"), 0);
    assert!(!list.at("entries").as_array().need()?.is_empty());
    assert!(
        list.at("entries")
            .rows()
            .all(|entry| entry.at("label").is_string())
    );
    key(&server, viewer, "down")?;
    assert_eq!(
        component(&server, viewer, OVERLAY)?
            .at("mode")
            .at("List")
            .at("selected"),
        1
    );

    // Native reflected insertion retains the real component, not a snapshot.
    // Exercise each increment and painting with an arbitrary maximum index.
    for input in [
        json!({"kind":"key","key":"down","ctrl":false,"alt":false,"shift":false}),
        json!({"kind":"key","key":"pagedown","ctrl":false,"alt":false,"shift":false}),
        json!({"kind":"mouse","action":"scroll_down","button":"none","x":0,"y":0,"ctrl":false,"alt":false,"shift":false}),
    ] {
        *overlay.pointer_mut("/mode/List/selected").need()? = json!(usize::MAX);
        server.rpc(
            "world.insert_components",
            json!({"entity":viewer,"components":{OVERLAY:overlay}}),
        )?;
        server.screen(viewer)?;
        server.input(viewer, input)?;
        assert!(
            component(&server, viewer, OVERLAY)?
                .at("mode")
                .at("List")
                .at("selected")
                .as_u64()
                .need()?
                < 1000
        );
    }
    key(&server, viewer, "escape")?;

    // Human prefix bindings open the real text prompt and confirmation.
    server.command(viewer, "help")?;
    key(&server, viewer, "r")?;
    let text = component(&server, viewer, OVERLAY)?.at("mode").at("Text");
    assert_eq!(text.at("action"), "RenamePane");
    server.input(viewer, json!({"kind":"paste","text":"BRP named pane"}))?;
    assert_eq!(
        component(&server, viewer, OVERLAY)?
            .at("mode")
            .at("Text")
            .at("buffer"),
        "BRP named pane"
    );
    server.enter(viewer)?;
    assert!(component(&server, viewer, OVERLAY).is_err());
    assert!(server.screen(viewer)?.contains("BRP named pane"));

    server.command(viewer, "help")?;
    key(&server, viewer, "x")?;
    assert_eq!(
        component(&server, viewer, OVERLAY)?
            .at("mode")
            .at("Confirm")
            .at("command"),
        json!({"kind":"close","subject":{"pane":pane}})
    );
    key(&server, viewer, "n")?;
    assert_eq!(server.focused(viewer)?, pane);
    assert!(component(&server, viewer, OVERLAY).is_err());

    server.control(viewer, json!({"kind":"choose","chooser":"tab"}))?;
    let list = component(&server, viewer, OVERLAY)?.at("mode").at("List");
    assert_eq!(list.at("entries").rows().count(), 1);
    assert!(
        list.at("entries")
            .rows()
            .all(|entry| entry.at("run").at("Command").at("kind") == "select")
    );
    server.enter(viewer)?;
    assert!(component(&server, viewer, OVERLAY).is_err());

    // Copy storage stays private, but native component listing exposes presence.
    server.command(viewer, "copy_mode")?;
    let listed = server.rpc("world.list_components", json!({"entity":viewer}))?;
    assert!(
        listed
            .rows()
            .any(|name| name == "fux::selection::Selection")
    );
    assert!(component(&server, viewer, "fux::selection::Selection").is_err());
    key(&server, viewer, "escape")?;
    let listed = server.rpc("world.list_components", json!({"entity":viewer}))?;
    assert!(
        !listed
            .rows()
            .any(|name| name == "fux::selection::Selection")
    );
    Ok(())
}

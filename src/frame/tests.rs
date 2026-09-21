use super::*;
use crate::testing::*;

#[test]
fn attach_preserves_permissive_defaults_and_clamps_before_narrowing() -> Outcome {
    let mut app = bevy_app::App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.add_plugins(crate::server::ServerPlugin);
    let root = app.world_mut().spawn((Workspace, Name::new("main"))).id();
    app.world_mut().spawn((Tab, ChildOf(root)));
    for (params, expected) in [
        (None, (24, 80)),
        (Some(Value::Null), (24, 80)),
        (Some(json!(false)), (24, 80)),
        (Some(json!(7)), (24, 80)),
        (Some(json!("not an object")), (24, 80)),
        (Some(json!([])), (24, 80)),
        (Some(json!({})), (24, 80)),
        (
            Some(json!({"rows":null,"cols":null,"workspace":null})),
            (24, 80),
        ),
        (Some(json!({"rows":-1,"cols":3.5,"workspace":7})), (24, 80)),
        (
            Some(json!({"rows":"12","cols":false,"unknown":true})),
            (24, 80),
        ),
        (Some(json!({"rows":0,"cols":0})), (0, 0)),
        (Some(json!({"rows":u64::MAX,"cols":65536})), (4096, 4096)),
        (
            Some(json!({"workspace":"main","rows":12,"cols":32})),
            (12, 32),
        ),
        (Some(json!({"rows":1})), (1, 80)),
        (Some(json!({"cols":1})), (24, 1)),
    ] {
        let response = attach(In(params), app.world_mut()).map_err(|e| e.message)?;
        let id = Entity::from_bits(response.get("viewer").and_then(Value::as_u64).need()?);
        let v = app.world().get::<Viewer>(id).need()?;
        assert_eq!((v.rows, v.cols), expected);
        app.world_mut().despawn(id);
    }
    let error = attach(In(Some(json!({"workspace":"missing"}))), app.world_mut())
        .err()
        .need()?;
    assert_eq!(error.message, "workspace not found");
    Ok(())
}

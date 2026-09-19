//! Private catalog activation and durable controller intent regressions.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "test assertions")]
use std::collections::BTreeMap;
use std::os::unix::fs::{PermissionsExt, symlink};
use zor::machines::{self, MachinesFile, catalog::{Catalog, MachineEntry}, intents::{ActionIntent, IntentLog}, supervision::{ActionKind, ActionPhase, ActionRecord}};
use zor::{config::Config, model::Ids};

fn entry(id: &str, name: &str) -> MachineEntry { MachineEntry {id:id.into(),name:name.into(),control:None,attachments:BTreeMap::new()} }

fn transport(instance: &str, token: &str) -> machines::transport::Transport {
    use zor::remote::descriptor::{AttachDescriptor, Descriptor, Endpoint};
    machines::transport::Transport::Direct {
        host: "127.0.0.1".into(), port: 9,
        brp: Descriptor {
            instance: instance.into(), pid: 42,
            http: Endpoint { host: "127.0.0.1".into(), port: 9 },
            attach: Some(AttachDescriptor { host: "127.0.0.1".into(), port: 10, token: format!("{token}-attach") }),
            token: token.into(),
        },
    }
}

#[test]
fn endpoint_credentials_require_admin_and_follow_only_active_bindings() {
    use bevy_ecs::prelude::In;
    use serde_json::json;
    use zor::remote::{Capabilities, Grant, Tokens, machine_methods, methods::codes};

    fn call(app: &mut bevy_app::App, method: &str, params: serde_json::Value) -> bevy_remote::BrpResult {
        let spec = machine_methods::METHODS.iter().find(|spec| spec.name == method).unwrap();
        let zor::remote::methods::Handler::Instant(handler) = spec.handler else { panic!("expected instant method"); };
        handler(In(Some(params)), app.world_mut())
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private/machines.json");
    let control = transport("zor-original", "control-secret");
    let attachment = transport("fux-original", "attachment-secret");
    let mut configured = entry("m-one", "first");
    configured.control = Some(control.clone());
    configured.attachments.insert("workspace".into(), attachment.clone());
    let mut catalog = Catalog::empty();
    catalog.add(configured).unwrap();
    catalog.save(&path).unwrap();
    let mut app = zor::app::build_headless(&Config::default(), &dir.path().join("state"));
    app.insert_resource(MachinesFile { path:path.clone(), asset_root:None });
    let mut tokens = Tokens::new("admin".into(), 2);
    tokens.mint("read".into(), Grant {workspace:None, capabilities:Capabilities::READ}).unwrap();
    tokens.mint("plugin".into(), Grant {workspace:None, capabilities:Capabilities::READ.union(Capabilities::MUTATE)}).unwrap();
    app.insert_resource(tokens);
    app.update();

    for token in ["read", "plugin"] {
        for workspace in [None, Some("workspace")] {
            let error = call(&mut app, "zor/machine.endpoint", json!({
                "token":token, "machine":"first", "workspace":workspace,
            })).unwrap_err();
            assert_eq!(error.code, codes::UNAUTHORIZED);
        }
        for value in [
            call(&mut app, "zor/machine.list", json!({"token":token})).unwrap(),
            call(&mut app, "zor/machine.inspect", json!({"token":token, "machine":"first"})).unwrap(),
        ] {
            let text = value.to_string();
            assert!(!text.contains("control-secret"));
            assert!(!text.contains("attachment-secret"));
        }
    }

    let resolve = |app: &mut bevy_app::App, machine: &str, workspace: Option<&str>| {
        let value = call(app, "zor/machine.endpoint", json!({
            "token":"admin", "machine":machine, "workspace":workspace,
        })).unwrap();
        serde_json::from_value::<machine_methods::MachineEndpoint>(value).unwrap().descriptor
    };
    assert_eq!(resolve(&mut app, "first", None), control.resolve().unwrap().descriptor);
    assert_eq!(resolve(&mut app, "m-one", Some("workspace")), attachment.resolve().unwrap().descriptor);
    assert!(call(&mut app, "zor/machine.endpoint", json!({
        "token":"admin", "machine":"first", "workspace":"other",
    })).is_err());

    // A valid draft is not active merely because it was saved or loaded.
    let replacement = transport("zor-replacement", "replacement-secret");
    catalog.find_mut("m-one").unwrap().control = Some(replacement);
    catalog.find_mut("m-one").unwrap().attachments.clear();
    catalog.save(&path).unwrap();
    assert_eq!(resolve(&mut app, "first", None), control.resolve().unwrap().descriptor);
    machines::reload(app.world_mut()).unwrap();
    assert_eq!(resolve(&mut app, "m-one", Some("workspace")), attachment.resolve().unwrap().descriptor);

    // Replace the pending draft with the original before activation, then corrupt disk.
    catalog.find_mut("m-one").unwrap().control = Some(control.clone());
    catalog.find_mut("m-one").unwrap().attachments.insert("workspace".into(), attachment.clone());
    catalog.find_mut("m-one").unwrap().name = "renamed".into();
    catalog.save(&path).unwrap();
    machines::reload(app.world_mut()).unwrap();
    app.update();
    std::fs::write(&path, b"{broken").unwrap();
    assert!(machines::reload(app.world_mut()).is_err());
    app.update();
    assert_eq!(resolve(&mut app, "renamed", None), control.resolve().unwrap().descriptor);
    assert_eq!(resolve(&mut app, "m-one", Some("workspace")), attachment.resolve().unwrap().descriptor);
    assert!(call(&mut app, "zor/machine.endpoint", json!({
        "token":"admin", "machine":"first",
    })).is_err());
}

#[test]
fn invalid_reload_retains_entities_and_rename_preserves_stable_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private/machines.json");
    let mut catalog = Catalog::empty();
    catalog.add(entry("m-one","first")).unwrap();
    catalog.save(&path).unwrap();
    let mut app = zor::app::build_headless(&Config::default(), &dir.path().join("state"));
    app.insert_resource(MachinesFile {path:path.clone(),asset_root:None});
    app.update();
    let original = app.world().resource::<Ids>().machine("m-one").unwrap();
    std::fs::write(&path,b"{broken").unwrap();
    assert!(machines::reload(app.world_mut()).is_err());
    app.update();
    assert_eq!(machines::snapshot(app.world(),"first").unwrap().id,"m-one");
    catalog.find_mut("m-one").unwrap().name = "renamed".into();
    catalog.save(&path).unwrap();
    machines::reload(app.world_mut()).unwrap();
    app.update();
    assert_eq!(app.world().resource::<Ids>().machine("m-one"),Some(original));
    assert!(machines::snapshot(app.world(),"first").is_err());
    assert_eq!(machines::snapshot(app.world(),"renamed").unwrap().id,"m-one");
    catalog.machines.clear();
    catalog.save(&path).unwrap();
    machines::reload(app.world_mut()).unwrap();
    app.update();
    assert!(machines::snapshot(app.world(),"m-one").is_err());
    assert!(app.world().get_entity(original).is_err());
}

#[test]
fn private_catalog_refuses_symlinks_permissions_and_ambiguous_selectors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private/machines.json");
    let mut catalog = Catalog::empty();
    catalog.add(entry("m-one","first")).unwrap();
    let before = catalog.clone();
    assert!(catalog.add(entry("first","other")).is_err());
    assert_eq!(catalog,before);
    assert!(catalog.add(entry("m-local","Local")).is_err());
    assert_eq!(catalog,before);
    catalog.save(&path).unwrap();
    let link = dir.path().join("link.json");
    symlink(&path,&link).unwrap();
    assert!(Catalog::load(&link).is_err());
    std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Catalog::load(&path).is_err());
}

#[test]
fn durable_uncertain_actions_survive_restart_and_cannot_be_retargeted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private/machines.json");
    let mut log = IntentLog::load(&path).unwrap();
    let record = ActionRecord {id:1,kind:Some(ActionKind::Stop),task:"task".into(),instance:"zor-original".into(),attempt:Some(7),phase:ActionPhase::Submitting,..Default::default()};
    let intent = ActionIntent {operation:"stop-once".into(),machine:"m-original".into(),control:"127.0.0.1:1234".into(),record:record.clone()};
    log.commit(intent.clone()).unwrap();
    drop(log);
    let mut recovered = IntentLog::load(&path).unwrap();
    let restored = recovered.find("stop-once").unwrap();
    assert_eq!(restored.record.phase,ActionPhase::Uncertain);
    assert_eq!(restored.record.attempt,Some(7));
    assert_eq!(restored.record.instance,"zor-original");
    assert_eq!(restored.control,intent.control);
    assert!(recovered.commit(intent.clone()).is_err());
    let mut redirected = intent.clone();
    redirected.machine = "m-replacement".into();
    assert!(recovered.commit(redirected).is_err());
    let uncertain = ActionRecord {phase:ActionPhase::Uncertain,problem:Some("lost reply".into()),..record};
    recovered.complete(&uncertain).unwrap();
    let restarted = IntentLog::load(&path).unwrap();
    assert_eq!(restarted.find("stop-once").unwrap().record,uncertain);
    assert_eq!(restarted.find("stop-once").unwrap().machine,"m-original");
}

#[test]
fn unavailable_durability_never_accepts_an_intent() {
    let mut log = IntentLog::default();
    assert!(log.commit(ActionIntent {operation:"once".into(),machine:"m".into(),control:"127.0.0.1:1234".into(),record:ActionRecord::default()}).is_err());
    assert!(log.find("once").is_none());
}

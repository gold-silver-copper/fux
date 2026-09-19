#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "behavioral regression assertions"
)]
mod common;
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};
use zor::plugins::{self, RunState};

fn fixture(path: &Path, name: &str) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("zor-plugin.toml"), format!("name = {name:?}\nversion = \"1\"\n[[actions]]\nid = \"run\"\ntitle = \"Run\"\ncommand = [\"/bin/sh\", \"-c\", \"echo executed\"]\n")).unwrap();
}
fn link(server: &common::Server, path: &Path) -> Value {
    let reply = server
        .call("zor/plugin.link", json!({"path": path, "enabled": true}))
        .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let op = server
            .call(
                "zor/plugin.operation",
                json!({"operation": reply["operation"]}),
            )
            .unwrap();
        if op["state"] == "complete" {
            break;
        }
        assert_eq!(op["state"], "pending", "{op}");
        assert!(Instant::now() < until, "import did not complete");
        std::thread::sleep(Duration::from_millis(10));
    }
    let name = std::fs::read_to_string(path.join("zor-plugin.toml")).unwrap();
    let manifest = plugins::Manifest::parse(&name).unwrap();
    loop {
        let record = server
            .call("zor/plugin.inspect", json!({"name": manifest.name}))
            .unwrap();
        if record["plugin"]["active"] == true {
            return record;
        }
        assert!(Instant::now() < until, "plugin never activated: {record}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn disable_revokes_authority_and_cancels_all_live_runs() {
    let server = common::Server::start();
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), "owned");
    link(&server, dir.path());
    let descriptor = server.with_world(|world| {
        let plugin = plugins::find(world, "owned").unwrap();
        fux::remote::client::read_descriptor(
            &world.get::<plugins::Active>(plugin).unwrap().descriptor,
        )
        .unwrap()
    });
    assert!(
        fux::remote::client::call_with(&descriptor, "zor/plugin.disable", json!({"name":"owned"}))
            .is_err()
    );
    let first = server
        .call("zor/plugin.run", json!({"name":"owned","action":"run"}))
        .unwrap();
    let second = server
        .call("zor/plugin.run", json!({"name":"owned","action":"run"}))
        .unwrap();
    assert_ne!(first["run"], second["run"]);
    let disabled = server
        .call("zor/plugin.disable", json!({"name":"owned"}))
        .unwrap();
    assert_eq!(disabled["plugin"]["active"], false);
    assert!(fux::remote::client::call_with(&descriptor, "zor/server.info", json!({})).is_err());
    assert!(
        server
            .call("zor/plugin.run", json!({"name":"owned","action":"run"}))
            .is_err()
    );
    server.with_world(|world| {
        let plugin = plugins::find(world, "owned").unwrap();
        assert!(
            plugins::record(world, plugin)
                .runs
                .iter()
                .all(|r| matches!(r.state, RunState::Exited { .. } | RunState::Stopping))
        );
    });
}

#[test]
fn cursor_grants_cannot_claim_another_plugin_and_failed_persistence_keeps_memory() {
    let server = common::Server::start();
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    fixture(&a, "alpha");
    fixture(&b, "beta");
    link(&server, &a);
    link(&server, &b);
    let descriptor = server.with_world(|world| {
        let plugin = plugins::find(world, "alpha").unwrap();
        fux::remote::client::read_descriptor(
            &world.get::<plugins::Active>(plugin).unwrap().descriptor,
        )
        .unwrap()
    });
    assert!(
        fux::remote::client::call_with(
            &descriptor,
            "zor/plugin.cursor",
            json!({"name":"beta","zor":99})
        )
        .is_err()
    );
    server.with_world(|world| {
        let plugin = plugins::find(world, "alpha").unwrap();
        let before = world.get::<plugins::Hook>(plugin).unwrap().cursors.clone();
        let path = world.resource::<plugins::PluginPaths>().cursors("alpha");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(plugins::set_cursor(world, plugin, Some(999), None).is_err());
        assert_eq!(world.get::<plugins::Hook>(plugin).unwrap().cursors, before);
    });
}

#[test]
fn imports_report_invalid_manifest_without_replacing_last_valid_plugin() {
    let server = common::Server::start();
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), "valid");
    link(&server, dir.path());
    std::fs::write(dir.path().join("zor-plugin.toml"), "name = invalid toml").unwrap();
    let submitted = server
        .call("zor/plugin.link", json!({"path":dir.path(),"enabled":true}))
        .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let op = server
            .call(
                "zor/plugin.operation",
                json!({"operation":submitted["operation"]}),
            )
            .unwrap();
        if op["state"] == "failed" {
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    let inspected = server
        .call("zor/plugin.inspect", json!({"name":"valid"}))
        .unwrap();
    assert_eq!(inspected["plugin"]["version"], "1");
    assert!(
        server
            .call("zor/plugin.run", json!({"name":"valid","action":"run"}))
            .is_ok()
    );
}

#[test]
fn killing_before_adapter_spawn_never_executes_command() {
    use zor::model::{Effect, Inbound};
    use zor::runner::Adapter;
    let dir = tempfile::tempdir().unwrap();
    let _app = zor::app::build_headless(&zor::config::Config::default(), dir.path());
    let (send, recv) = async_channel::bounded(4);
    let mut adapter =
        plugins::host::PluginAdapter::with_executable(send, env!("CARGO_BIN_EXE_zor").into());
    let plugin = bevy_ecs::world::World::new().spawn_empty().id();
    let marker = dir.path().join("executed");
    let _ = adapter
        .apply(Effect::RunPlugin {
            plugin,
            run: 1,
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("sleep 2; touch '{}'", marker.display()),
            ],
            cwd: None,
            env: vec![],
            log: dir.path().join("log"),
        })
        .unwrap();
    let _ = adapter
        .apply(Effect::KillPlugin { plugin, run: 1 })
        .unwrap();
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(Inbound::PluginExited { run: 1, .. }) = recv.try_recv() {
            break;
        }
        assert!(Instant::now() < until, "canceled process did not terminate");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!marker.exists(), "canceled run executed its side effect");
}

#[test]
fn repeated_stars_match_without_exponential_backtracking() {
    let pattern = format!("zor/{}x", "*a".repeat(50));
    assert!(!plugins::manifest::pattern_matches(
        &pattern,
        &format!("zor/{}y", "a".repeat(100))
    ));
    assert!(plugins::manifest::pattern_matches(
        "zor/*Closed",
        "zor/TaskClosed"
    ));
    assert!(!plugins::manifest::pattern_matches(
        "zor/*Closed",
        "fux/PaneClosed"
    ));
}

#[test]
fn binary_hot_output_keeps_recent_bounded_logs() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("plugin.log");
    for _ in 0..80 {
        plugins::host::append_line(
            &log,
            "[run] ",
            &vec![b'x'; plugins::host::MAX_LINE_BYTES * 2],
        )
        .unwrap();
    }
    plugins::host::append_line(&log, "[run] ", b"binary \xff output").unwrap();
    plugins::host::append_line(&log, "[run] ", b"final marker").unwrap();
    let tail = plugins::host::read_tail(&log, 2);
    assert_eq!(
        tail,
        vec!["[run] binary \u{fffd} output", "[run] final marker"]
    );
    let bound = plugins::host::MAX_LOG_BYTES + plugins::host::MAX_LINE_BYTES as u64 + 1024;
    assert!(std::fs::metadata(&log).unwrap().len() <= bound);
    assert!(
        std::fs::metadata(log.with_extension("log.1"))
            .unwrap()
            .len()
            <= bound
    );
}

#[path = "../../fux/tests/common/mod.rs"]
mod fux_common;

/// Drive the real composition protocol without launching child commands. Exit observations
/// remain explicit inputs, just as they are with the production process adapter.
struct PaneHost {
    app: bevy_app::App,
    fux: fux_common::Server,
    effects: bevy_ecs::message::MessageCursor<zor::model::Effect>,
    adapter: zor::fux_client::FuxAdapter,
    replies: async_channel::Receiver<zor::model::Inbound>,
    plugin: bevy_ecs::entity::Entity,
    _dir: tempfile::TempDir,
}

impl PaneHost {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fux = fux_common::Server::start();
        let mut app = common::build(&dir.path().join("run"), &dir.path().join("state"), "panes");
        app.world_mut()
            .insert_resource(plugins::FuxDescriptor(fux.brp.clone()));
        app.finish();
        app.cleanup();
        app.update();
        let source = dir.path().join("plugin");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(
            source.join("zor-plugin.toml"),
            r#"
name = "panes"
version = "1"
[[actions]]
id = "busy"
title = "Busy"
command = ["/bin/sleep", "60"]
[[panes]]
id = "surface"
kind = "surface"
placement = "overlay"
command = ["/bin/true"]
[[panes]]
id = "terminal"
kind = "terminal"
placement = "tab"
command = ["/bin/sleep", "60"]
"#,
        )
        .unwrap();
        plugins::link(app.world_mut(), &source, true).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let plugin = loop {
            app.update();
            if let Ok(plugin) = plugins::find(app.world(), "panes")
                && app.world().get::<plugins::Active>(plugin).is_some()
            {
                break plugin;
            }
            assert!(Instant::now() < until, "plugin did not activate");
            std::thread::sleep(Duration::from_millis(10));
        };
        let (inbound, replies) = async_channel::unbounded();
        let adapter = zor::fux_client::FuxAdapter::new(fux.brp.clone(), inbound);
        Self {
            app,
            fux,
            effects: Default::default(),
            adapter,
            replies,
            plugin,
            _dir: dir,
        }
    }

    fn step(&mut self) {
        use zor::model::Effect;
        use zor::runner::Adapter;
        self.app.update();
        let effects: Vec<_> = self
            .effects
            .read(
                self.app
                    .world()
                    .resource::<bevy_ecs::message::Messages<Effect>>(),
            )
            .filter_map(|effect| match effect {
                Effect::FuxCall {
                    call,
                    method,
                    params,
                } => Some((*call, method.clone(), params.clone())),
                _ => None,
            })
            .collect();
        for (call, method, params) in effects {
            // Exercise the production descriptor pin check and reserved-field removal,
            // rather than sending effect-only metadata directly to the RPC server.
            let immediate = self
                .adapter
                .apply(Effect::FuxCall {
                    call,
                    method,
                    params,
                })
                .unwrap();
            self.app
                .world_mut()
                .write_message(immediate.unwrap_or_else(|| self.replies.recv_blocking().unwrap()));
        }
    }

    fn until(&mut self, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition(self) {
            assert!(
                Instant::now() < deadline,
                "pane transition timed out: {:?}",
                plugins::record(self.app.world(), self.plugin)
            );
            self.step();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn open(&mut self, kind: &str) -> u64 {
        plugins::open_pane(
            self.app.world_mut(),
            self.plugin,
            kind,
            "default",
            None,
            None,
        )
        .unwrap()
    }

    fn pane(&self, id: u64) -> plugins::OpenPane {
        self.app
            .world()
            .get::<plugins::Panes>(self.plugin)
            .unwrap()
            .0
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .clone()
    }
}

#[test]
fn closed_owned_surface_panes_reuse_capacity_and_revoke_grants() {
    let mut host = PaneHost::new();
    let initial = host.fux.call("fux/server.info", json!({})).unwrap()["tokens_minted"]
        .as_u64()
        .unwrap();
    for _ in 0..(plugins::MAX_RUNS_RETAINED + 2) {
        let opening = host.open("surface");
        host.until(|h| {
            h.app
                .world()
                .get::<plugins::Runs>(h.plugin)
                .unwrap()
                .0
                .iter()
                .any(|r| r.surface.is_some() && r.state == RunState::Running)
        });
        let run = host
            .app
            .world()
            .get::<plugins::Runs>(host.plugin)
            .unwrap()
            .0
            .iter()
            .find(|r| r.surface.is_some() && r.state == RunState::Running)
            .unwrap()
            .id;
        host.app
            .world_mut()
            .write_message(zor::model::Inbound::PluginExited {
                plugin: host.plugin,
                run,
                code: Some(0),
            });
        host.until(|h| matches!(h.pane(opening).state, plugins::PaneState::Closed));
        host.step();
        assert_eq!(
            host.fux.call("fux/server.info", json!({})).unwrap()["tokens_minted"],
            initial
        );
    }
}

#[test]
fn stale_surface_driver_exit_preserves_replacement_provider() {
    let mut host = PaneHost::new();
    let opening = host.open("surface");
    host.until(|h| {
        h.app
            .world()
            .get::<plugins::Runs>(h.plugin)
            .unwrap()
            .0
            .iter()
            .any(|r| r.surface.is_some() && r.state == RunState::Running)
    });
    let run = host
        .app
        .world()
        .get::<plugins::Runs>(host.plugin)
        .unwrap()
        .0
        .iter()
        .find(|r| r.surface.is_some())
        .unwrap()
        .id;
    let plugins::PaneState::Open {
        surface: Some(surface),
        ..
    } = host.pane(opening).state
    else {
        panic!("surface opening did not retain its surface identity");
    };
    let closed = host
        .fux
        .call("fux/surface.close", json!({"surface":surface}))
        .unwrap();
    host.fux
        .call(
            "fux/surface.open",
            json!({"workspace":"default","node":surface,
        "provider":"replacement","generation":closed["generation"]}),
        )
        .unwrap();
    host.app
        .world_mut()
        .write_message(zor::model::Inbound::PluginExited {
            plugin: host.plugin,
            run,
            code: Some(0),
        });
    for _ in 0..4 {
        host.step();
    }
    assert!(matches!(
        host.pane(opening).state,
        plugins::PaneState::Open { .. }
    ));
    let provider = host.fux.with_world(move |world| {
        let node = world
            .resource::<fux::model::Ids>()
            .node(fux::model::NodeId(surface))
            .unwrap();
        world
            .get::<fux::surface::SurfaceState>(node)
            .unwrap()
            .provider
            .clone()
    });
    assert_eq!(provider, "replacement");
}

#[test]
fn surface_driver_refusal_removes_owned_surface_and_container() {
    let mut host = PaneHost::new();
    for _ in 0..plugins::MAX_RUNS_RETAINED {
        plugins::run_action(
            host.app.world_mut(),
            host.plugin,
            "busy",
            plugins::RunContext::default(),
        )
        .unwrap();
    }
    host.until(|h| {
        h.app
            .world()
            .get::<plugins::Runs>(h.plugin)
            .unwrap()
            .0
            .iter()
            .all(|r| r.state == RunState::Running)
    });
    let initial = host.fux.call("fux/server.info", json!({})).unwrap()["tokens_minted"].clone();
    let nodes = host
        .fux
        .with_world(|world| world.query::<&fux::model::NodeId>().iter(world).count());
    let opening = host.open("surface");
    host.until(|h| matches!(h.pane(opening).state, plugins::PaneState::Failed { .. }));
    let failed = host.pane(opening);
    assert!(
        matches!(failed.state, plugins::PaneState::Failed { ref problem } if problem.contains("live run limit"))
    );
    // Complete guarded close, node removal and token retirement, then repeat beyond capacity.
    for _ in 0..4 {
        host.step();
    }
    assert_eq!(
        host.fux.call("fux/server.info", json!({})).unwrap()["tokens_minted"],
        initial
    );
    for _ in 0..plugins::MAX_RUNS_RETAINED {
        let opening = host.open("surface");
        host.until(|h| matches!(h.pane(opening).state, plugins::PaneState::Failed { .. }));
        for _ in 0..4 {
            host.step();
        }
    }
    assert_eq!(
        host.fux
            .with_world(|world| world.query::<&fux::model::NodeId>().iter(world).count()),
        nodes
    );
}

#[test]
fn terminal_closure_requires_same_incarnation_evidence() {
    let mut host = PaneHost::new();
    let opening = host.open("terminal");
    host.until(|h| {
        matches!(
            h.pane(opening).state,
            plugins::PaneState::Open { pane: Some(_), .. }
        )
    });
    let plugins::PaneState::Open {
        pane: Some(pane), ..
    } = host.pane(opening).state
    else {
        panic!("terminal opening did not retain its pane identity");
    };
    host.app
        .world_mut()
        .write_message(zor::model::Inbound::FuxLink {
            instance: Some("replacement".into()),
        });
    host.app
        .world_mut()
        .write_message(zor::model::Inbound::FuxEvent {
            cursor: 1,
            name: "PaneClosed".into(),
            body: json!({"pane":pane}),
        });
    host.step();
    assert!(matches!(
        host.pane(opening).state,
        plugins::PaneState::Open { .. }
    ));
    let entity = host.fux.with_world(move |world| {
        world.query_filtered::<(bevy_ecs::prelude::Entity, &fux::model::PaneId), bevy_ecs::query::Allow<bevy_ecs::entity_disabling::Disabled>>().iter(world)
            .find_map(|(entity, id)| (id.0 == pane).then_some(entity))
    }).unwrap();
    host.fux
        .call("fux/pane.close", json!({"pane":pane}))
        .unwrap();
    // This HTTP fixture has no PTY adapter: closing requests termination but is not exit evidence.
    host.fux.with_world(move |world| {
        world.write_message(fux::model::Inbound::PaneExited {
            pane: entity,
            code: 0,
        });
    });
    host.app
        .world_mut()
        .write_message(zor::model::Inbound::FuxLink {
            instance: Some(host.fux.descriptor.instance.clone()),
        });
    host.until(|h| matches!(h.pane(opening).state, plugins::PaneState::Closed));
}

#[test]
fn activation_cursor_survives_disconnect_before_first_event_without_replay() {
    let fux = fux_common::Server::start();
    // Headless bootstrap has no process adapter, so it need not publish any events.
    // A real viewer attachment supplies retained history that reconnect must not replay.
    let (viewer, viewer_id) = fux.with_world(|world| {
        let workspace = world
            .resource::<fux::model::Ids>()
            .workspace("default")
            .unwrap();
        let viewer = fux::layout::ops::attach_viewer(
            world,
            workspace,
            fux::model::Viewport { rows: 24, cols: 80 },
            None,
        )
        .unwrap();
        (viewer, world.get::<fux::model::ViewerId>(viewer).unwrap().0)
    });
    let server = common::Server::start();
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), "hook");
    link(&server, dir.path());
    let (zor_brp, cursors) = server.with_world(|world| {
        let plugin = plugins::find(world, "hook").unwrap();
        (
            world
                .get::<plugins::Active>(plugin)
                .unwrap()
                .descriptor
                .clone(),
            world.get::<plugins::Hook>(plugin).unwrap().cursors.clone(),
        )
    });
    let retained = fux.call("fux/events.poll", json!({"cursor":0})).unwrap();
    assert!(retained["events"].as_array().unwrap().iter().any(|event|
        event["name"] == "ViewerAttached" && event["event"]["viewer"] == viewer_id
    ), "viewer attachment must be retained before hook activation: {retained}");
    let baseline = retained["cursor"].as_u64().unwrap();
    let marker = dir.path().join("events");
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _child = Child(
        std::process::Command::new(env!("CARGO_BIN_EXE_zor"))
            .args(["plugin", "hook", "hook"])
            .env("ZOR_PLUGIN_NAME", "hook")
            .env("ZOR_BRP", zor_brp)
            .env("FUX_BRP", &fux.brp)
            .env(
                "ZOR_PLUGIN_CURSORS",
                serde_json::to_string(&cursors).unwrap(),
            )
            .env(
                "ZOR_PLUGIN_HOOKS",
                json!([{"pattern":"fux/*","command":["/bin/sh","-c",
            "printf '%s\\n' \"$ZOR_PLUGIN_EVENT_CURSOR\" >> \"$1\"", "hook", marker]}])
                .to_string(),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let wait_for = |condition: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "hook did not reach expected observation"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    wait_for(&|| {
        fux.call("fux/server.info", json!({})).unwrap()["watches"]
            .as_u64()
            .unwrap()
            > 0
    });
    let persisted = server.with_world(|world| {
        let plugin = plugins::find(world, "hook").unwrap();
        world.get::<plugins::Hook>(plugin).unwrap().cursors.clone()
    });
    assert_eq!(persisted.fux, baseline);
    assert_eq!(persisted.fux_instance, fux.descriptor.instance);
    // Disconnect transport without revoking authority or adding an event.
    let token = fux.descriptor.token.clone();
    fux.with_world(move |world| fux::remote::watch::revoke(world, &token));
    wait_for(&|| {
        fux.call("fux/server.info", json!({})).unwrap()["watches"]
            .as_u64()
            .unwrap()
            > 0
    });
    fux.with_world(move |world| fux::layout::ops::detach_viewer(world, viewer).unwrap());
    let latest = fux.call("fux/events.poll", json!({"cursor":null})).unwrap()["cursor"]
        .as_u64()
        .unwrap();
    wait_for(&|| {
        std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .lines()
            .any(|line| line.parse::<u64>().ok() == Some(latest))
    });
    let events = std::fs::read_to_string(&marker).unwrap();
    let dispatched: Vec<u64> = events.lines().map(|line| line.parse().unwrap()).collect();
    assert!(
        dispatched.iter().all(|cursor| *cursor > baseline),
        "preactivation history replayed: {events}"
    );
    assert!(
        dispatched.windows(2).all(|pair| pair[0] < pair[1]),
        "duplicate dispatch: {events}"
    );
}

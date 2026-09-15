//! Pane and viewer lifecycle through the World only: `Inbound` in, `Effect` out, invariants
//! after every update.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
use bevy_app::App;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use fux::app::build_headless;
use fux::config::Config;
use fux::layout::ops;
use fux::lifecycle::{self, Clock};
use fux::model::invariants::check_invariants;
use fux::model::*;

struct Harness {
    app: App,
}

impl Harness {
    /// A `default` workspace with one starting pane and one viewer at 80x24.
    fn new() -> (Self, Entity, Entity) {
        let mut app = build_headless(&Config::default());
        app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
        let world = app.world_mut();
        lifecycle::bootstrap(world, "default", &["/bin/sh".into()]).unwrap();
        let ws = world.resource::<Ids>().workspace("default").unwrap();
        let viewer = ops::attach_viewer(world, ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
        let mut h = Self { app };
        let pane = h.panes()[0].0;
        (h, viewer, pane)
    }

    fn step(&mut self, messages: Vec<Inbound>) -> Vec<Effect> {
        let world = self.app.world_mut();
        world
            .resource_mut::<Messages<Inbound>>()
            .write_batch(messages);
        self.app.update();
        let world = self.app.world_mut();
        let effects: Vec<Effect> = world.resource_mut::<Messages<Effect>>().drain().collect();
        check_invariants(world).unwrap();
        effects
    }

    fn panes(&mut self) -> Vec<(Entity, Process)> {
        self.app
            .world_mut()
            .query_filtered::<(Entity, &Process), (With<Pane>, Allow<Disabled>)>()
            .iter(self.app.world())
            .map(|(e, p)| (e, *p))
            .collect()
    }

    fn world(&mut self) -> &mut World {
        self.app.world_mut()
    }

    fn request(viewer: Entity, request: ViewerRequest) -> Inbound {
        Inbound::ViewerRequest { viewer, request }
    }

    /// Runs the spawn handshake for every starting pane and returns them.
    fn go_live(&mut self) -> Vec<Entity> {
        let starting: Vec<Entity> = self
            .panes()
            .into_iter()
            .filter(|(_, p)| *p == Process::Starting)
            .map(|(e, _)| e)
            .collect();
        let messages = starting
            .iter()
            .enumerate()
            .map(|(i, &pane)| Inbound::PaneSpawned {
                pane,
                pid: 100 + i as u32,
            })
            .collect();
        self.step(messages);
        starting
    }
}

fn spawn_panes(effects: &[Effect]) -> Vec<Entity> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::SpawnPane { pane, .. } => Some(*pane),
            _ => None,
        })
        .collect()
}

#[test]
fn starting_pane_spawns_once_and_goes_live() {
    let (mut h, _viewer, pane) = Harness::new();
    assert!(h.world().get::<Disabled>(pane).is_some());
    let first = h.step(vec![]);
    assert_eq!(spawn_panes(&first), vec![pane]);
    let Some(Effect::SpawnPane {
        rows, cols, argv, ..
    }) = first.iter().find(|e| matches!(e, Effect::SpawnPane { .. }))
    else {
        panic!("no spawn");
    };
    assert_eq!(argv, &["/bin/sh".to_owned()]);
    assert_eq!(
        (*rows, *cols),
        (24, 80),
        "spawned at the viewer's folded size"
    );
    let second = h.step(vec![]);
    assert!(spawn_panes(&second).is_empty(), "SpawnPane is emitted once");
    assert!(h.world().get::<Creation>(pane).is_some());

    h.step(vec![Inbound::PaneSpawned { pane, pid: 7 }]);
    assert!(h.world().get::<Disabled>(pane).is_none());
    assert!(h.world().get::<Creation>(pane).is_none());
    assert_eq!(
        h.world().get::<Process>(pane),
        Some(&Process::Live { pid: 7 })
    );
}

#[test]
fn split_queues_input_until_the_new_pane_is_live() {
    let (mut h, viewer, pane) = Harness::new();
    h.step(vec![]);
    h.step(vec![Inbound::PaneSpawned { pane, pid: 7 }]);
    let effects = h.step(vec![
        Harness::request(
            viewer,
            ViewerRequest::Split {
                direction: SplitDirection::Right,
                template: None,
            },
        ),
        Harness::request(viewer, ViewerRequest::Input(b"ls\n".to_vec())),
    ]);
    let spawned = spawn_panes(&effects);
    assert_eq!(spawned.len(), 1, "the split spawned one pane");
    let new_pane = spawned[0];
    assert_ne!(new_pane, pane);
    assert!(
        !effects.iter().any(|e| matches!(e, Effect::WritePty { .. })),
        "input after a split waits for the new pane"
    );
    assert_eq!(
        h.world().get::<CreationBarrier>(viewer),
        Some(&CreationBarrier(Some(new_pane)))
    );
    assert_eq!(h.world().get::<Targets>(viewer), Some(&Targets(new_pane)));

    let idle = h.step(vec![]);
    assert!(!idle.iter().any(|e| matches!(e, Effect::WritePty { .. })));

    let effects = h.step(vec![Inbound::PaneSpawned {
        pane: new_pane,
        pid: 8,
    }]);
    let writes: Vec<(Entity, &[u8])> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::WritePty { pane, bytes } => Some((*pane, bytes.as_slice())),
            _ => None,
        })
        .collect();
    assert_eq!(writes, vec![(new_pane, b"ls\n".as_slice())]);
    assert_eq!(
        h.world().get::<CreationBarrier>(viewer),
        Some(&CreationBarrier(None))
    );
}

#[test]
fn exited_pane_leaves_a_record_and_the_viewer_retargets() {
    let (mut h, viewer, pane) = Harness::new();
    h.step(vec![]);
    h.step(vec![Inbound::PaneSpawned { pane, pid: 7 }]);
    h.step(vec![Harness::request(
        viewer,
        ViewerRequest::Split {
            direction: SplitDirection::Below,
            template: None,
        },
    )]);
    let new_pane = h.go_live()[0];
    assert_eq!(h.world().get::<Targets>(viewer), Some(&Targets(new_pane)));
    let leaf = h
        .world()
        .get::<PlacedIn>(new_pane)
        .unwrap()
        .iter()
        .next()
        .unwrap();

    h.world().resource_mut::<Clock>().now_ms = 5_000;
    let effects = h.step(vec![Inbound::PaneExited {
        pane: new_pane,
        code: 3,
    }]);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ReleasePty { pane } if *pane == new_pane))
    );
    assert_eq!(h.world().get::<Targets>(viewer), Some(&Targets(pane)));
    assert!(
        h.world().get_entity(leaf).is_err(),
        "the placing leaf is gone"
    );
    let retain = Config::default().limits().final_retain_ms;
    let records: Vec<FinalRecord> = h
        .world()
        .query::<&FinalRecord>()
        .iter(h.app.world())
        .cloned()
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].exit_code, 3);
    assert_eq!(records[0].workspace, "default");
    assert_eq!(records[0].exited_ms, 5_000);
    assert_eq!(records[0].expires_ms, 5_000 + retain);

    h.step(vec![]);
    assert!(
        h.world().get_entity(new_pane).is_err(),
        "the pane is despawned next update"
    );
    assert_eq!(h.panes().len(), 1);

    h.world().resource_mut::<Clock>().now_ms = 5_000 + retain;
    h.step(vec![]);
    assert_eq!(
        h.world()
            .query::<&FinalRecord>()
            .iter(h.app.world())
            .count(),
        0
    );
}

#[test]
fn exact_viewer_detaches_when_its_pane_exits() {
    let (mut h, viewer, pane) = Harness::new();
    h.step(vec![]);
    h.step(vec![Inbound::PaneSpawned { pane, pid: 7 }]);
    h.step(vec![Harness::request(
        viewer,
        ViewerRequest::Split {
            direction: SplitDirection::Right,
            template: None,
        },
    )]);
    let new_pane = h.go_live()[0];
    let ws = h.world().resource::<Ids>().workspace("default").unwrap();
    let exact = ops::attach_viewer(
        h.world(),
        ws,
        Viewport { rows: 10, cols: 40 },
        Some(new_pane),
    )
    .unwrap();
    h.step(vec![]);
    assert_eq!(h.world().get::<Targets>(exact), Some(&Targets(new_pane)));

    h.step(vec![Inbound::PaneExited {
        pane: new_pane,
        code: 0,
    }]);
    assert!(h.world().get::<Detaching>(exact).is_some());
    assert!(h.world().get::<Detaching>(viewer).is_none());
    assert_eq!(h.world().get::<Targets>(viewer), Some(&Targets(pane)));
}

#[test]
fn viewer_gone_detaches_and_last_pane_exit_retires_the_workspace() {
    let (mut h, viewer, pane) = Harness::new();
    h.step(vec![]);
    h.step(vec![Inbound::PaneSpawned { pane, pid: 7 }]);
    let ws = h.world().resource::<Ids>().workspace("default").unwrap();

    h.step(vec![Inbound::PaneExited { pane, code: 0 }]);
    assert!(
        h.world().get::<Retiring>(ws).is_some(),
        "no roots left: retiring"
    );
    assert!(h.world().get::<Detaching>(viewer).is_some());
    h.step(vec![]);
    assert!(
        h.world().get_entity(ws).is_ok(),
        "kept while a viewer refers to it"
    );

    h.step(vec![Inbound::ViewerGone { viewer }]);
    assert!(h.world().get_entity(viewer).is_err());
    assert!(
        h.world().get_entity(ws).is_err(),
        "no panes, roots or viewers: despawned"
    );
    assert!(h.world().resource::<Ids>().workspace("default").is_none());
}

#[test]
fn close_and_detach_requests() {
    let (mut h, viewer, pane) = Harness::new();
    h.step(vec![]);
    h.step(vec![Inbound::PaneSpawned { pane, pid: 7 }]);
    let effects = h.step(vec![Harness::request(viewer, ViewerRequest::ClosePane)]);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Terminate { pane: p } if *p == pane))
    );
    assert!(matches!(
        h.world().get::<Process>(pane),
        Some(Process::Terminating { pid: 7, .. })
    ));
    let again = h.step(vec![Harness::request(viewer, ViewerRequest::ClosePane)]);
    assert!(
        !again.iter().any(|e| matches!(e, Effect::Terminate { .. })),
        "a terminating pane is not terminated twice"
    );
    h.step(vec![Harness::request(viewer, ViewerRequest::Detach)]);
    assert!(h.world().get::<Detaching>(viewer).is_some());
    let ignored = h.step(vec![Harness::request(
        viewer,
        ViewerRequest::Input(b"x".to_vec()),
    )]);
    assert!(!ignored.iter().any(|e| matches!(e, Effect::WritePty { .. })));
}

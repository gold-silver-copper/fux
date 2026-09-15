//! Public lifecycle events and the retained log through the headless App: every event fires
//! exactly once at its transition, `PaneOutput` is paced, bodies carry ids only, and the log
//! reports gaps for evicted, replaced and unknown cursors.

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
use fux::app::build_headless;
use fux::config::Config;
use fux::events::*;
use fux::layout::ops;
use fux::lifecycle::{self, Clock};
use fux::model::invariants::check_invariants;
use fux::model::*;
use fux::pty::PacingWake;
use fux::terminal::Terminal;
use serde_json::Value;

/// Every public event as triggered (independent of the log): `(name, body)`.
#[derive(Resource, Default)]
struct Seen(Vec<(&'static str, Value)>);

fn record<E: Logged>(event: On<E>, mut seen: ResMut<Seen>) {
    seen.0
        .push((E::NAME, serde_json::to_value(event.event()).unwrap()));
}

struct Harness {
    app: App,
}

impl Harness {
    /// A `default` workspace with one starting pane and one viewer at 80x24, every event
    /// recorded.
    fn new() -> (Self, Entity, Entity) {
        let mut app = build_headless(&Config::default());
        app.init_resource::<Seen>()
            .add_observer(record::<PaneSpawned>)
            .add_observer(record::<PaneOutput>)
            .add_observer(record::<PaneTitleChanged>)
            .add_observer(record::<PaneExited>)
            .add_observer(record::<PaneClosed>)
            .add_observer(record::<RootEmptied>)
            .add_observer(record::<WorkspaceRetired>)
            .add_observer(record::<ViewerAttached>)
            .add_observer(record::<ViewerDetached>)
            .add_observer(record::<Bell>);
        app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
        let (ws, pane) = Self::bootstrap(&mut app);
        let viewer =
            ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
        (Self { app }, viewer, pane)
    }

    fn bootstrap(app: &mut App) -> (Entity, Entity) {
        let world = app.world_mut();
        lifecycle::bootstrap(world, "default", &["/bin/sh".into()]).unwrap();
        let ws = world.resource::<Ids>().workspace("default").unwrap();
        let pane = world
            .query_filtered::<Entity, (With<Pane>, With<Disabled>, Without<Terminal>)>()
            .iter(world)
            .next()
            .unwrap();
        (ws, pane)
    }

    fn step(&mut self, messages: Vec<Inbound>) {
        let world = self.app.world_mut();
        world
            .resource_mut::<Messages<Inbound>>()
            .write_batch(messages);
        self.app.update();
        let world = self.app.world_mut();
        world.resource_mut::<Messages<Effect>>().clear();
        check_invariants(world).unwrap();
    }

    fn advance(&mut self, ms: u64) {
        self.app.world_mut().resource_mut::<Clock>().now_ms += ms;
    }

    fn world(&mut self) -> &mut World {
        self.app.world_mut()
    }

    fn go_live(&mut self, pane: Entity, pid: u32) {
        self.step(vec![]);
        self.step(vec![Inbound::PaneSpawned { pane, pid }]);
    }

    fn output(&mut self, pane: Entity, bytes: &[u8]) {
        self.step(vec![Inbound::PaneOutput {
            pane,
            bytes: bytes.to_vec(),
        }]);
    }

    fn seen(&mut self, name: &str) -> Vec<Value> {
        self.world()
            .resource::<Seen>()
            .0
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn take_seen(&mut self) -> Vec<(&'static str, Value)> {
        std::mem::take(&mut self.world().resource_mut::<Seen>().0)
    }

    fn logged(&mut self, name: &str) -> Vec<Entry> {
        self.world()
            .resource::<EventLog>()
            .read_after("default", 0)
            .unwrap()
            .iter()
            .filter(|e| e.name == name)
            .cloned()
            .collect()
    }

    fn all_logged(&mut self) -> Vec<Entry> {
        let mut out = Vec::new();
        self.world()
            .resource::<EventLog>()
            .read_any_after(0, &mut out)
            .unwrap();
        out.into_iter().cloned().collect()
    }
}

fn pane_id(world: &mut World, pane: Entity) -> u64 {
    world.get::<PaneId>(pane).unwrap().0
}

fn assert_body_is_ids_only(name: &str, body: &Value) {
    let text = serde_json::to_string(body).unwrap();
    assert!(text.len() < 256, "{name} body too large: {text}");
    let object = body
        .as_object()
        .unwrap_or_else(|| panic!("{name} body is not an object"));
    for forbidden in [
        "lines", "cells", "screen", "text", "entity", "scope", "bytes",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "{name} body carries `{forbidden}`: {text}"
        );
    }
    for (key, value) in object {
        assert!(
            value.is_number() || value.is_string(),
            "{name}.{key} is not an id or counter: {value}"
        );
    }
}

#[test]
fn every_lifecycle_event_fires_once_at_its_transition() {
    let (mut h, viewer, pane) = Harness::new();
    let viewer_id = h.world().get::<ViewerId>(viewer).unwrap().0;
    let id = pane_id(h.world(), pane);
    let root_id = h
        .world()
        .query_filtered::<&NodeId, With<TemplateRoot>>()
        .single(h.world())
        .unwrap()
        .0;

    h.step(vec![]);
    let attached = h.seen("ViewerAttached");
    assert_eq!(attached.len(), 1, "attach_viewer announces once");
    assert_eq!(attached[0]["viewer"], viewer_id);
    assert!(
        h.seen("PaneSpawned").is_empty(),
        "starting panes are not spawned"
    );

    h.step(vec![Inbound::PaneSpawned { pane, pid: 4242 }]);
    h.step(vec![]);
    let spawned = h.seen("PaneSpawned");
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0]["pane"], id);
    assert_eq!(spawned[0]["pid"], 4242);
    assert_eq!(h.logged("PaneSpawned").len(), 1, "the log retains it once");
    assert!(
        h.seen("PaneOutput").is_empty(),
        "a blank emulator and its resize announce no output"
    );

    h.output(pane, b"\x1b]2;hello\x07");
    h.step(vec![]);
    let titles = h.seen("PaneTitleChanged");
    assert_eq!(titles.len(), 1, "one title change, no event at insertion");
    assert_eq!(titles[0]["pane"], id);
    assert_eq!(h.world().get::<Title>(pane).unwrap().0, "hello");
    h.output(pane, b"\x1b]2;hello\x07");
    assert_eq!(
        h.seen("PaneTitleChanged").len(),
        1,
        "an equal title is no change"
    );

    h.output(pane, b"\x07\x07");
    assert_eq!(
        h.seen("Bell").len(),
        1,
        "a burst of bells rings once per update"
    );
    h.step(vec![]);
    assert_eq!(h.seen("Bell").len(), 1);
    h.output(pane, b"\x07");
    assert_eq!(h.seen("Bell").len(), 2, "a later bell rings again");
    assert_eq!(h.seen("Bell")[1]["pane"], id);

    h.step(vec![
        Inbound::PaneEof { pane },
        Inbound::PaneExited { pane, code: 3 },
    ]);
    let exited = h.seen("PaneExited");
    assert_eq!(exited.len(), 1);
    assert_eq!(exited[0]["pane"], id);
    assert_eq!(exited[0]["code"], 3);
    let emptied = h.seen("RootEmptied");
    assert_eq!(emptied.len(), 1, "the last pane leaving closes the root");
    assert_eq!(emptied[0]["root"], root_id);
    let retired = h.seen("WorkspaceRetired");
    assert_eq!(
        retired.len(),
        1,
        "the last root leaving retires the workspace"
    );
    assert_eq!(retired[0]["workspace"], "default");
    assert!(
        h.seen("PaneClosed").is_empty(),
        "the pane entity survives the update"
    );

    h.step(vec![]);
    let closed = h.seen("PaneClosed");
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0]["pane"], id);
    assert!(h.world().get_entity(pane).is_err());

    h.step(vec![Inbound::ViewerGone { viewer }]);
    let detached = h.seen("ViewerDetached");
    assert_eq!(detached.len(), 1);
    assert_eq!(detached[0]["viewer"], viewer_id);
    h.step(vec![]);
    for _ in 0..3 {
        h.step(vec![]);
    }
    let counts = |h: &mut Harness, name: &str| h.seen(name).len();
    for name in [
        "ViewerAttached",
        "PaneSpawned",
        "PaneTitleChanged",
        "PaneExited",
        "RootEmptied",
        "WorkspaceRetired",
        "PaneClosed",
        "ViewerDetached",
    ] {
        assert_eq!(counts(&mut h, name), 1, "{name} fired more than once");
    }

    let entries = h.all_logged();
    assert!(entries.len() >= 10);
    assert!(
        entries.windows(2).all(|w| w[0].cursor < w[1].cursor),
        "cursors are strictly increasing"
    );
    assert!(
        entries
            .iter()
            .all(|e| e.workspace == "default" && e.ms == 1_000)
    );
    for (name, body) in h.take_seen() {
        assert_body_is_ids_only(name, &body);
    }
    for entry in &entries {
        assert_body_is_ids_only(entry.name, &entry.event);
    }
    let order: Vec<&str> = entries.iter().map(|e| e.name).collect();
    let position = |name: &str| order.iter().position(|n| *n == name).unwrap();
    assert!(position("ViewerAttached") < position("PaneSpawned"));
    assert!(position("PaneExited") < position("RootEmptied"));
    assert!(position("RootEmptied") < position("WorkspaceRetired"));
    assert!(position("WorkspaceRetired") < position("PaneClosed"));
}

#[test]
fn pane_output_is_paced_with_a_trailing_event() {
    let (mut h, _viewer, pane) = Harness::new();
    let pacing = h.world().resource::<Limits>().output_pacing_ms;
    h.go_live(pane, 7);
    h.step(vec![]);
    h.advance(pacing);
    h.take_seen();

    for i in 0..10 {
        h.advance(1);
        h.output(pane, format!("line {i}\r\n").as_bytes());
    }
    let burst = h.seen("PaneOutput");
    assert_eq!(burst.len(), 1, "one event within the pacing window");
    let first_seq = burst[0]["seq"].as_u64().unwrap();
    assert!(first_seq > 0);
    let now = h.world().resource::<Clock>().now_ms;
    let wake = h.world().resource::<PacingWake>().at_ms.unwrap();
    assert!(
        wake > now && wake <= now + pacing,
        "the runner is asked to wake at the end of the window ({wake} for now {now})"
    );

    h.advance(pacing);
    h.step(vec![]);
    let trailing = h.seen("PaneOutput");
    assert_eq!(
        trailing.len(),
        2,
        "the window elapsing sends the unsent sequence"
    );
    let last_seq = trailing[1]["seq"].as_u64().unwrap();
    let terminal_seq = h.world().get::<Terminal>(pane).unwrap().seq();
    assert_eq!(
        last_seq, terminal_seq,
        "the trailing event carries the latest sequence"
    );
    assert!(last_seq > first_seq);
    assert_eq!(h.world().resource::<PacingWake>().at_ms, None);

    h.advance(pacing);
    h.step(vec![]);
    assert_eq!(h.seen("PaneOutput").len(), 2, "nothing new, nothing sent");
    assert_eq!(h.logged("PaneOutput").len(), 2);
    assert_eq!(
        h.world().get::<OutputPacing>(pane).unwrap().last_event_seq,
        terminal_seq
    );
}

#[test]
fn recreated_workspace_replaces_its_stream() {
    let (mut h, viewer, pane) = Harness::new();
    h.go_live(pane, 7);
    h.step(vec![
        Inbound::PaneEof { pane },
        Inbound::PaneExited { pane, code: 0 },
    ]);
    h.step(vec![]);
    h.step(vec![Inbound::ViewerGone { viewer }]);
    h.step(vec![]);
    assert!(
        h.world().resource::<Ids>().workspace("default").is_none(),
        "the retired workspace is gone"
    );
    let old_cursor = h.world().resource::<EventLog>().cursor("default");
    assert!(old_cursor > 0);
    assert!(
        h.world()
            .resource::<EventLog>()
            .read_after("default", old_cursor)
            .unwrap()
            .is_empty(),
        "the retired stream is still readable at its tail"
    );
    let earlier = old_cursor - 2;
    assert!(
        !h.world()
            .resource::<EventLog>()
            .read_after("default", earlier)
            .unwrap()
            .is_empty()
    );

    let (_ws, pane) = Harness::bootstrap(&mut h.app);
    h.go_live(pane, 9);
    let log = h.world().resource::<EventLog>();
    assert_eq!(
        log.read_after("default", earlier),
        Err(Gap {
            since: earlier,
            resume: old_cursor
        }),
        "a cursor into the replaced stream is a gap"
    );
    let fresh = log.read_after("default", old_cursor).unwrap();
    assert_eq!(
        fresh.len(),
        1,
        "only the spawn: a blank emulator is no output: {fresh:?}"
    );
    assert_eq!(fresh[0].name, "PaneSpawned");
    assert!(fresh[0].cursor > old_cursor);
    assert_eq!(log.cursor("default"), fresh[0].cursor);
    let mut all = Vec::new();
    assert_eq!(
        log.read_any_after(earlier, &mut all),
        Err(Gap {
            since: earlier,
            resume: old_cursor
        })
    );
}

// ---------------------------------------------------------------------------------------------
// The log on its own
// ---------------------------------------------------------------------------------------------

fn log_with(entries: usize, workspaces: usize) -> EventLog {
    EventLog::new(&Limits {
        event_log_entries: entries,
        workspaces,
        ..Limits::default()
    })
}

fn output(scope: Entity, seq: u64) -> PaneOutput {
    PaneOutput {
        entity: Entity::PLACEHOLDER,
        scope,
        pane: PaneId(1),
        seq,
    }
}

fn scope(n: u32) -> Entity {
    Entity::from_raw_u32(n).unwrap()
}

#[test]
fn eviction_by_entries_reports_a_gap() {
    let mut log = log_with(4, 8);
    let a = scope(1);
    for seq in 1..=6 {
        log.append(&output(a, seq), seq, || Some("a".into()));
    }
    assert_eq!(log.latest(), 6);
    assert_eq!(log.cursor("a"), 6);
    assert_eq!(
        log.read_after("a", 0),
        Err(Gap {
            since: 0,
            resume: 2
        })
    );
    assert_eq!(
        log.read_after("a", 1),
        Err(Gap {
            since: 1,
            resume: 2
        })
    );
    let retained = log.read_after("a", 2).unwrap();
    assert_eq!(
        retained.iter().map(|e| e.cursor).collect::<Vec<_>>(),
        [3, 4, 5, 6]
    );
    assert_eq!(retained[0].ms, 3);
    assert_eq!(retained[0].event["seq"], 3);
    assert_eq!(log.read_after("a", 5).unwrap().len(), 1);
    assert!(log.read_after("a", 6).unwrap().is_empty());
    assert_eq!(
        log.read_after("a", 7),
        Err(Gap {
            since: 7,
            resume: 6
        })
    );
    assert!(log.read_after("nope", 3).unwrap().is_empty());
    assert_eq!(
        log.read_after("nope", 9),
        Err(Gap {
            since: 9,
            resume: 6
        })
    );
    assert_eq!(log.retained(), 4);
    let mut any = Vec::new();
    assert_eq!(
        log.read_any_after(1, &mut any),
        Err(Gap {
            since: 1,
            resume: 2
        })
    );
    log.read_any_after(4, &mut any).unwrap();
    assert_eq!(any.iter().map(|e| e.cursor).collect::<Vec<_>>(), [5, 6]);
}

#[test]
fn eviction_by_bytes_bounds_the_stream() {
    let mut log = log_with(1_000_000, 8);
    let a = scope(1);
    for seq in 1..=20_000 {
        log.append(&output(a, seq), 0, || Some("a".into()));
    }
    let retained = log.read_after("a", log.latest() - 1).unwrap().len();
    assert_eq!(retained, 1);
    let total = log.retained();
    assert!(total < 20_000, "bytes evicted something");
    assert!(
        total * 60 <= MAX_STREAM_BYTES,
        "retained entries fit the byte bound"
    );
    assert!(
        total * 200 >= MAX_STREAM_BYTES,
        "the bound is not wildly conservative"
    );
    assert!(matches!(log.read_after("a", 0), Err(Gap { since: 0, .. })));
}

#[test]
fn unscoped_reads_merge_streams_in_cursor_order() {
    let mut log = log_with(8, 8);
    let (a, b) = (scope(1), scope(2));
    log.append(&output(a, 1), 0, || Some("a".into()));
    log.append(&output(b, 1), 0, || Some("b".into()));
    log.append(&output(a, 2), 0, || Some("a".into()));
    let mut all = Vec::new();
    log.read_any_after(0, &mut all).unwrap();
    assert_eq!(
        all.iter()
            .map(|e| (e.cursor, e.workspace.as_str()))
            .collect::<Vec<_>>(),
        [(1, "a"), (2, "b"), (3, "a")]
    );
    let mut names: Vec<&str> = log.workspaces().collect();
    names.sort_unstable();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(log.cursor("b"), 2);
    assert_eq!(log.read_after("b", 1).unwrap().len(), 1);
}

#[test]
fn retired_streams_are_pruned_into_tombstones() {
    let mut log = log_with(8, 1);
    let (a, b) = (scope(1), scope(2));
    log.append(&output(a, 1), 0, || Some("a".into()));
    let retire = |scope: Entity, name: &str| WorkspaceRetired {
        entity: scope,
        scope,
        workspace: name.into(),
    };
    log.append(&retire(a, "a"), 0, || Some("a".into()));
    assert_eq!(
        log.read_after("a", 0).unwrap().len(),
        2,
        "one retired stream is kept"
    );
    log.append(&output(b, 1), 0, || Some("b".into()));
    log.append(&retire(b, "b"), 0, || Some("b".into()));
    assert_eq!(
        log.read_after("a", 0),
        Err(Gap {
            since: 0,
            resume: 2
        }),
        "the oldest retired stream was dropped whole"
    );
    assert!(log.read_after("a", 2).unwrap().is_empty());
    assert_eq!(log.read_after("b", 2).unwrap().len(), 2);
    assert_eq!(log.workspaces().collect::<Vec<_>>(), ["b"]);
    let mut all = Vec::new();
    assert_eq!(
        log.read_any_after(1, &mut all),
        Err(Gap {
            since: 1,
            resume: 2
        })
    );

    // A workspace named like a tombstone starts above the dropped tail.
    let a2 = scope(3);
    log.append(&output(a2, 1), 0, || Some("a".into()));
    assert_eq!(
        log.read_after("a", 1),
        Err(Gap {
            since: 1,
            resume: 2
        })
    );
    assert_eq!(log.read_after("a", 2).unwrap().len(), 1);
}

#[test]
fn tombstones_are_bounded_at_twice_the_retired_cap() {
    let mut log = log_with(8, 1);
    let retire = |scope: Entity, name: &str| WorkspaceRetired {
        entity: scope,
        scope,
        workspace: name.into(),
    };
    // Retiring `a`..`d` in turn keeps one retired stream (`d`) and tombstones the rest; the
    // bound (2) forgets the oldest tombstone, `a`.
    for (n, name) in ["a", "b", "c", "d"].into_iter().enumerate() {
        let ws = scope(n as u32 + 1);
        log.append(&output(ws, 1), 0, || Some(name.into()));
        log.append(&retire(ws, name), 0, || Some(name.into()));
    }
    assert_eq!(log.workspaces().collect::<Vec<_>>(), ["d"]);
    assert_eq!(
        log.read_after("b", 0),
        Err(Gap {
            since: 0,
            resume: 4
        }),
        "a tombstone within the bound still reports its gap"
    );
    assert_eq!(
        log.read_after("c", 0),
        Err(Gap {
            since: 0,
            resume: 6
        })
    );
    assert!(
        log.read_after("a", 0).unwrap().is_empty(),
        "the oldest tombstone aged out: the name answers like an unknown one"
    );
    let mut all = Vec::new();
    assert_eq!(
        log.read_any_after(0, &mut all),
        Err(Gap {
            since: 0,
            resume: 6
        }),
        "the server-wide floor still covers what was dropped"
    );
}

#[test]
fn unknown_scope_is_not_retained() {
    let mut log = log_with(8, 8);
    log.append(&output(scope(1), 1), 0, || None);
    assert_eq!(log.latest(), 0);
    assert_eq!(log.retained(), 0);
}

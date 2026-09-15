//! Surfaces (prompt 3.13): a provider streams RON `DynamicWorld` deltas into a template leaf;
//! fux validates them against the layout limits, applies them under the leaf, lays them out
//! with everyone else and replicates them to viewers. `check_invariants` runs after every step.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "integration-test helpers outside #[test] fns; clippy.toml only relaxes test fns"
)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_math::Vec2;
use bevy_ui::{
    BackgroundColor, BorderColor, ComputedNode, FlexDirection, GlobalZIndex, Node, ScrollPosition,
    UiGlobalTransform, UiTargetCamera, Val, ZIndex,
};
use bevy_world_serialization::DynamicWorldBuilder;
use serde::Serialize;
use serde_json::{Value, json};

use fux::attach::{AttachAdapter, AttachEndpoint, AttachPlugin, AttachToken};
use fux::config::Config;
use fux::layout::{LayoutError, ops};
use fux::lifecycle::Clock;
use fux::model::invariants::check_invariants;
use fux::model::{
    Effect, Inbound, InstanceNode, InstanceOf, LayoutGeneration, Limits, NodeId, PaneTemplate,
    ServerInstance, Surface, TemplateNode, ViewerCamera, Viewport,
};
use fux::remote::methods::{self, codes};
use fux::remote::token::Tokens;
use fux::surface::{self, MAX_UPDATES_PER_SECOND, SurfaceError, SurfaceState, Text};
use fux::viewer::{self, Inbox, Painter};
use fux::wire::{self, Hello, SceneFrame, ServerFrame};

const TOKEN: &str = "surface-test-token";
const NONCE: &str = "nonce-surface";

// ---------------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------------

fn app() -> App {
    let mut app = fux::app::build_headless(&Config::default());
    app.insert_resource(Tokens::new(TOKEN.to_owned(), 8))
        .insert_resource(ServerInstance {
            name: "test".into(),
            nonce: NONCE.into(),
            pid: std::process::id(),
            started_ms: 0,
        });
    app.finish();
    app.cleanup();
    app.update();
    app
}

fn check(app: &mut App) {
    check_invariants(app.world_mut()).unwrap();
}

fn grow() -> Node {
    Node {
        flex_grow: 1.0,
        ..Default::default()
    }
}

/// A root with a pane leaf on the left and an empty leaf on the right.
fn scene(app: &mut App) -> (Entity, Entity, Entity, Entity) {
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "default").unwrap();
    let root = ops::new_root(world, ws, "main").unwrap();
    let pane_leaf = ops::spawn_node(
        world,
        root,
        None,
        grow(),
        Some(PaneTemplate {
            argv: vec!["sh".to_owned()],
            ..Default::default()
        }),
    )
    .unwrap();
    let leaf = ops::spawn_node(world, root, None, grow(), None).unwrap();
    check(app);
    (ws, root, pane_leaf, leaf)
}

fn node_id(world: &World, e: Entity) -> u64 {
    world.get::<NodeId>(e).unwrap().0
}

fn generation(world: &World, root: Entity) -> u64 {
    world.get::<LayoutGeneration>(root).unwrap().0
}

/// Calls a `fux/*` handler directly with the envelope filled in.
fn call(app: &mut App, method: &str, mut params: Value) -> Result<Value, bevy_remote::BrpError> {
    let spec = methods::all_specs()
        .find(|s| s.name == method)
        .unwrap_or_else(|| panic!("{method} is not in the table"));
    params["token"] = json!(TOKEN);
    params["instance"] = json!(NONCE);
    let result = (spec.handler)(In(Some(params)), app.world_mut());
    check(app);
    result
}

/// The provider's own World: entity ids are its and stay stable across exports.
struct Provider {
    world: World,
    registry: AppTypeRegistry,
}

impl Provider {
    fn new(app: &App) -> Self {
        Self {
            world: World::new(),
            registry: app.world().resource::<AppTypeRegistry>().clone(),
        }
    }

    fn export(&self, entities: &[Entity]) -> String {
        let registry = self.registry.read();
        DynamicWorldBuilder::from_world(&self.world, &registry)
            .deny_all()
            .allow_component::<Node>()
            .allow_component::<Name>()
            .allow_component::<ZIndex>()
            .allow_component::<BackgroundColor>()
            .allow_component::<BorderColor>()
            .allow_component::<ScrollPosition>()
            .allow_component::<Text>()
            .allow_component::<ChildOf>()
            .allow_component::<Children>()
            .allow_component::<GlobalZIndex>()
            .extract_entities(entities.iter().copied())
            .build()
            .serialize(&registry)
            .unwrap()
    }

    /// Every entity in pre-order: a full update.
    fn export_all(&mut self) -> String {
        let all = self.all();
        self.export(&all)
    }

    fn all(&mut self) -> Vec<Entity> {
        let mut roots: Vec<Entity> = self
            .world
            .query_filtered::<Entity, (With<Node>, Without<ChildOf>)>()
            .iter(&self.world)
            .collect();
        roots.sort();
        let mut out = Vec::new();
        for root in roots {
            let mut stack = vec![root];
            while let Some(e) = stack.pop() {
                out.push(e);
                if let Some(children) = self.world.get::<Children>(e) {
                    stack.extend(children.iter().rev());
                }
            }
        }
        out
    }

    /// A column of `rows` one-cell-high text lines: `(column, [lines])`.
    fn column(&mut self, lines: &[&str]) -> (Entity, Vec<Entity>) {
        let column = self
            .world
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    ..Default::default()
                },
                Name::new("column"),
            ))
            .id();
        let rows = lines
            .iter()
            .map(|line| {
                self.world
                    .spawn((
                        Node {
                            width: Val::Percent(100.0),
                            height: Val::Px(1.0),
                            ..Default::default()
                        },
                        Text((*line).to_owned()),
                        ChildOf(column),
                    ))
                    .id()
            })
            .collect();
        (column, rows)
    }
}

fn instance_root(world: &mut World, viewer: Entity) -> Entity {
    let camera = world.get::<ViewerCamera>(viewer).unwrap().0;
    let roots: Vec<Entity> = world
        .query_filtered::<(Entity, &UiTargetCamera), (With<InstanceNode>, Without<ChildOf>)>()
        .iter(world)
        .filter(|(_, c)| c.entity() == camera)
        .map(|(e, _)| e)
        .collect();
    assert_eq!(roots.len(), 1, "exactly one instance root per viewer");
    roots[0]
}

/// The instance of `template` in the tree under `root`.
fn instance_of(world: &World, root: Entity, template: Entity) -> Entity {
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if world.get::<InstanceOf>(e).map(|i| i.0) == Some(template) {
            return e;
        }
        if let Some(children) = world.get::<Children>(e) {
            stack.extend(children.iter());
        }
    }
    panic!("no instance of {template}");
}

/// `(min, max)` in cells.
fn rect(world: &World, e: Entity) -> (Vec2, Vec2) {
    let node = world.get::<ComputedNode>(e).unwrap();
    let transform = world.get::<UiGlobalTransform>(e).unwrap();
    let center = transform.translation;
    (center - node.size / 2.0, center + node.size / 2.0)
}

fn contains(outer: (Vec2, Vec2), inner: (Vec2, Vec2)) -> bool {
    inner.0.x >= outer.0.x - 0.01
        && inner.0.y >= outer.0.y - 0.01
        && inner.1.x <= outer.1.x + 0.01
        && inner.1.y <= outer.1.y + 0.01
}

fn opened(app: &mut App) -> (Entity, Entity, Entity, Entity) {
    let (ws, root, pane_leaf, leaf) = scene(app);
    surface::open(app.world_mut(), leaf, "zor").unwrap();
    check(app);
    (ws, root, pane_leaf, leaf)
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

#[test]
fn open_refuses_pane_leaves_containers_and_repeats() {
    let mut app = app();
    let (_, root, pane_leaf, leaf) = scene(&mut app);
    assert_eq!(
        surface::open(app.world_mut(), pane_leaf, "zor"),
        Err(SurfaceError::PlacesAPane(pane_leaf))
    );
    assert_eq!(
        surface::open(app.world_mut(), root, "zor"),
        Err(SurfaceError::HasChildren(root))
    );
    let before = generation(app.world(), root);
    surface::open(app.world_mut(), leaf, "zor").unwrap();
    check(&mut app);
    assert!(app.world().get::<Surface>(leaf).is_some());
    assert_eq!(
        app.world().get::<SurfaceState>(leaf).unwrap().provider,
        "zor"
    );
    assert!(generation(app.world(), root) > before, "open re-instances");
    assert_eq!(
        surface::open(app.world_mut(), leaf, "other"),
        Err(SurfaceError::AlreadyASurface(leaf))
    );
    // The subtree is the provider's: layout edits under the leaf are refused.
    assert_eq!(
        ops::spawn_node(app.world_mut(), leaf, None, grow(), None),
        Err(LayoutError::SurfaceSubtree(leaf))
    );
    check(&mut app);
}

#[test]
fn open_over_brp_checks_workspace_and_generation() {
    let mut app = app();
    let (_, root, pane_leaf, leaf) = scene(&mut app);
    let (leaf_id, pane_leaf_id, gen0) = {
        let world = app.world();
        (
            node_id(world, leaf),
            node_id(world, pane_leaf),
            generation(world, root),
        )
    };
    let stale = call(
        &mut app,
        "fux/surface.open",
        json!({ "workspace": "default", "node": leaf_id, "provider": "zor", "generation": gen0 + 1 }),
    );
    assert_eq!(stale.unwrap_err().code, codes::STALE_GENERATION);
    let wrong_ws = call(
        &mut app,
        "fux/surface.open",
        json!({ "workspace": "nope", "node": leaf_id, "provider": "zor", "generation": gen0 }),
    );
    assert_eq!(wrong_ws.unwrap_err().code, codes::NOT_FOUND);
    let pane = call(
        &mut app,
        "fux/surface.open",
        json!({ "workspace": "default", "node": pane_leaf_id, "provider": "zor", "generation": gen0 }),
    );
    assert_eq!(pane.unwrap_err().code, codes::INVALID);
    let ok = call(
        &mut app,
        "fux/surface.open",
        json!({ "workspace": "default", "node": leaf_id, "provider": "zor", "generation": gen0 }),
    )
    .unwrap();
    assert_eq!(ok["surface"], json!(leaf_id));
    assert_eq!(ok["revision"], json!(0));
    assert_eq!(ok["generation"], json!(gen0 + 1));
    // Updates address the surface, not just any node.
    let not_surface = call(
        &mut app,
        "fux/surface.update",
        json!({ "surface": pane_leaf_id, "revision": 1, "full": true, "delta": "" }),
    );
    assert_eq!(not_surface.unwrap_err().code, codes::NOT_FOUND);
}

// ---------------------------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------------------------

#[test]
fn full_update_lays_out_under_the_leaf() {
    let mut app = app();
    let (ws, root, _, leaf) = opened(&mut app);
    let viewer =
        ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    let mut provider = Provider::new(&app);
    let (column, rows) = provider.column(&["tasks", "checks", "notes"]);
    let delta = provider.export_all();
    let before = generation(app.world(), root);
    let nodes = surface::update(app.world_mut(), leaf, 1, true, &delta).unwrap();
    check(&mut app);
    assert_eq!(nodes, 4);
    assert!(generation(app.world(), root) > before);
    {
        let world = app.world();
        let state = world.get::<SurfaceState>(leaf).unwrap();
        assert_eq!(state.revision, 1);
        let t_column = state.entity(column).unwrap();
        assert_eq!(world.get::<ChildOf>(t_column).unwrap().parent(), leaf);
        assert!(world.get::<TemplateNode>(t_column).is_some());
        assert!(
            world.get::<NodeId>(t_column).is_none(),
            "provider nodes are not addressable"
        );
        let kids: Vec<Entity> = world.get::<Children>(t_column).unwrap().iter().collect();
        let expected: Vec<Entity> = rows.iter().map(|r| state.entity(*r).unwrap()).collect();
        assert_eq!(kids, expected, "sibling order follows the provider");
        assert_eq!(
            world.get::<Text>(expected[1]).map(|t| t.0.as_str()),
            Some("checks")
        );
    }
    app.update();
    app.update();
    check(&mut app);
    let world = app.world_mut();
    let iroot = instance_root(world, viewer);
    let ileaf = instance_of(world, iroot, leaf);
    let leaf_rect = rect(world, ileaf);
    assert_eq!(leaf_rect.0, Vec2::new(40.0, 0.0), "{leaf_rect:?}");
    assert_eq!(leaf_rect.1, Vec2::new(80.0, 24.0), "{leaf_rect:?}");
    let state = world.get::<SurfaceState>(leaf).unwrap();
    let t_rows: Vec<Entity> = rows.iter().map(|r| state.entity(*r).unwrap()).collect();
    let column_rect = rect(
        world,
        instance_of(world, iroot, state.entity(column).unwrap()),
    );
    assert_eq!(column_rect, leaf_rect);
    for (i, t_row) in t_rows.iter().enumerate() {
        let instance = instance_of(world, iroot, *t_row);
        let r = rect(world, instance);
        assert!(
            contains(leaf_rect, r),
            "row {i}: {r:?} outside {leaf_rect:?}"
        );
        assert_eq!(r.0, Vec2::new(40.0, i as f32), "row {i}: {r:?}");
        assert_eq!(r.1 - r.0, Vec2::new(40.0, 1.0), "row {i}: {r:?}");
        assert!(
            world.get::<Text>(instance).is_some(),
            "instances carry Text"
        );
    }
}

#[test]
fn partial_update_rewrites_and_full_update_prunes() {
    let mut app = app();
    let (ws, _, _, leaf) = opened(&mut app);
    let viewer =
        ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    let mut provider = Provider::new(&app);
    let (column, rows) = provider.column(&["tasks", "checks", "notes"]);
    let delta = provider.export_all();
    surface::update(app.world_mut(), leaf, 1, true, &delta).unwrap();
    check(&mut app);
    let template_rows: Vec<Entity> = {
        let state = app.world().get::<SurfaceState>(leaf).unwrap();
        rows.iter().map(|r| state.entity(*r).unwrap()).collect()
    };

    // Partial: one entity's Text changes; ids, hierarchy and order are untouched.
    provider.world.get_mut::<Text>(rows[1]).unwrap().0 = "CHECKS".to_owned();
    let partial = provider.export(&[rows[1]]);
    assert_eq!(
        surface::update(app.world_mut(), leaf, 2, false, &partial).unwrap(),
        4
    );
    check(&mut app);
    {
        let world = app.world();
        let state = world.get::<SurfaceState>(leaf).unwrap();
        assert_eq!(state.entity(rows[1]), Some(template_rows[1]));
        assert_eq!(
            world.get::<Text>(template_rows[1]).map(|t| t.0.as_str()),
            Some("CHECKS")
        );
        let kids: Vec<Entity> = world
            .get::<Children>(state.entity(column).unwrap())
            .unwrap()
            .iter()
            .collect();
        assert_eq!(kids, template_rows);
    }
    // Partial: a new entity appended under an existing parent.
    let extra = provider
        .world
        .spawn((
            Node {
                height: Val::Px(1.0),
                ..Default::default()
            },
            Text("extra".to_owned()),
            ChildOf(column),
        ))
        .id();
    let partial = provider.export(&[extra]);
    assert_eq!(
        surface::update(app.world_mut(), leaf, 3, false, &partial).unwrap(),
        5
    );
    check(&mut app);
    let t_extra = {
        let world = app.world();
        let state = world.get::<SurfaceState>(leaf).unwrap();
        let t_extra = state.entity(extra).unwrap();
        let kids: Vec<Entity> = world
            .get::<Children>(state.entity(column).unwrap())
            .unwrap()
            .iter()
            .collect();
        assert_eq!(kids.last(), Some(&t_extra));
        t_extra
    };
    app.update();
    check(&mut app);
    let iroot = instance_root(app.world_mut(), viewer);
    instance_of(app.world(), iroot, t_extra);

    // Full without `notes` and `extra`: both template nodes go, instances follow.
    provider.world.despawn(rows[2]);
    provider.world.despawn(extra);
    let full = provider.export_all();
    assert_eq!(
        surface::update(app.world_mut(), leaf, 4, true, &full).unwrap(),
        3
    );
    check(&mut app);
    {
        let world = app.world();
        assert!(world.get_entity(template_rows[2]).is_err());
        assert!(world.get_entity(t_extra).is_err());
        let state = world.get::<SurfaceState>(leaf).unwrap();
        assert_eq!(state.entity(rows[2]), None);
        assert_eq!(state.nodes(), 3);
    }
    app.update();
    check(&mut app);
    let world = app.world_mut();
    let iroot = instance_root(world, viewer);
    let gone = world
        .query::<&InstanceOf>()
        .iter(world)
        .filter(|i| i.0 == template_rows[2] || i.0 == t_extra)
        .count();
    assert_eq!(gone, 0, "instances of despawned nodes are gone");
    instance_of(world, iroot, template_rows[1]);
}

#[test]
fn full_update_honours_children_order_and_reparents() {
    let mut app = app();
    let (_, _, _, leaf) = opened(&mut app);
    let mut provider = Provider::new(&app);
    let (column, rows) = provider.column(&["a", "b", "c"]);
    let delta = provider.export_all();
    surface::update(app.world_mut(), leaf, 1, true, &delta).unwrap();
    check(&mut app);
    // Reverse the order in the provider's World and move `c` to the top level.
    provider
        .world
        .entity_mut(column)
        .replace_children(&[rows[1], rows[0]]);
    provider.world.entity_mut(rows[2]).remove::<ChildOf>();
    let delta = provider.export_all();
    surface::update(app.world_mut(), leaf, 2, true, &delta).unwrap();
    check(&mut app);
    let world = app.world();
    let state = world.get::<SurfaceState>(leaf).unwrap();
    let t = |e: Entity| state.entity(e).unwrap();
    // The surface's own children have no `Children` list in the delta: they come in delta
    // order (a builder export orders by entity id), so only membership is contractual here.
    let mut top: Vec<Entity> = world.get::<Children>(leaf).unwrap().iter().collect();
    top.sort();
    let mut expected = vec![t(column), t(rows[2])];
    expected.sort();
    assert_eq!(top, expected);
    assert_eq!(world.get::<ChildOf>(t(rows[2])).unwrap().parent(), leaf);
    let kids: Vec<Entity> = world.get::<Children>(t(column)).unwrap().iter().collect();
    assert_eq!(
        kids,
        vec![t(rows[1]), t(rows[0])],
        "the provider's Children order"
    );
    assert_eq!(state.nodes(), 4);
}

#[test]
fn stale_revision_is_refused_with_the_generation_code() {
    let mut app = app();
    let (_, _, _, leaf) = opened(&mut app);
    let leaf_id = node_id(app.world(), leaf);
    let mut provider = Provider::new(&app);
    provider.column(&["x"]);
    let delta = provider.export_all();
    let ok = call(
        &mut app,
        "fux/surface.update",
        json!({ "surface": leaf_id, "revision": 5, "full": true, "delta": delta }),
    )
    .unwrap();
    assert_eq!(ok["revision"], json!(5));
    assert_eq!(ok["nodes"], json!(2));
    for stale in [5, 4, 0] {
        let err = call(
            &mut app,
            "fux/surface.update",
            json!({ "surface": leaf_id, "revision": stale, "full": true, "delta": delta }),
        )
        .unwrap_err();
        assert_eq!(err.code, codes::STALE_GENERATION, "revision {stale}");
        assert_eq!(err.data, Some(json!({ "revision": 5 })));
    }
    assert_eq!(
        app.world().get::<SurfaceState>(leaf).unwrap().revision,
        5,
        "a refused update leaves the revision"
    );
}

/// A hand-written delta of `count` partial `Node`s (ids `1..=count`), flat or as one chain,
/// each with `text` when given: what a provider that sends only what changed writes.
fn compact(count: usize, chain: bool, text: Option<&str>) -> String {
    let mut ron = String::from("(resources: {}, entities: {");
    for id in 1..=count {
        ron.push_str(&format!(
            "{id}: (components: {{\"bevy_ui::ui_node::Node\": (display: Flex)"
        ));
        if chain && id > 1 {
            ron.push_str(&format!(", \"bevy_ecs::hierarchy::ChildOf\": ({})", id - 1));
        }
        if let Some(text) = text {
            ron.push_str(&format!(", \"fux::surface::Text\": (\"{text}\")"));
        }
        ron.push_str("}), ");
    }
    ron.push_str("})");
    ron
}

#[test]
fn oversize_deep_and_foreign_deltas_are_refused_untouched() {
    let mut app = app();
    let (_, root, _, leaf) = opened(&mut app);
    let per_surface = app.world().resource::<Limits>().nodes_per_workspace / 4;
    let gen0 = generation(app.world(), root);

    // Too many nodes.
    let delta = compact(per_surface + 1, false, None);
    assert_eq!(
        surface::update(app.world_mut(), leaf, 1, true, &delta),
        Err(SurfaceError::TooManyNodes {
            count: per_surface + 1,
            max: per_surface,
        })
    );

    // Too deep: a chain that would exceed MAX_DEPTH from the leaf.
    let mut provider = Provider::new(&app);
    let mut parent = provider.world.spawn(Node::default()).id();
    for _ in 0..fux::model::MAX_DEPTH {
        parent = provider
            .world
            .spawn((Node::default(), ChildOf(parent)))
            .id();
    }
    let delta = provider.export_all();
    assert!(matches!(
        surface::update(app.world_mut(), leaf, 1, true, &delta),
        Err(SurfaceError::DepthExceeded { .. })
    ));

    // Outside the vocabulary.
    let mut provider = Provider::new(&app);
    provider.world.spawn((Node::default(), GlobalZIndex(1)));
    let delta = provider.export_all();
    assert_eq!(
        surface::update(app.world_mut(), leaf, 1, true, &delta),
        Err(SurfaceError::UnknownType(
            "bevy_ui::ui_node::GlobalZIndex".to_owned()
        ))
    );

    // A parent outside the delta, a new entity without a Node, malformed RON.
    let mut provider = Provider::new(&app);
    let a = provider.world.spawn(Node::default()).id();
    let b = provider.world.spawn((Node::default(), ChildOf(a))).id();
    let delta = provider.export(&[b]);
    assert_eq!(
        surface::update(app.world_mut(), leaf, 1, true, &delta),
        Err(SurfaceError::UnknownParent {
            child: b,
            parent: a
        })
    );
    let c = provider.world.spawn(Name::new("no node")).id();
    let delta = provider.export(&[c]);
    assert_eq!(
        surface::update(app.world_mut(), leaf, 1, true, &delta),
        Err(SurfaceError::MissingNode(c))
    );
    assert!(matches!(
        surface::update(app.world_mut(), leaf, 1, true, "(entities: "),
        Err(SurfaceError::Malformed(_))
    ));
    let long = Text("x".repeat(surface::MAX_TEXT_BYTES + 1));
    let d = provider.world.spawn((Node::default(), long)).id();
    let delta = provider.export(&[d]);
    assert!(matches!(
        surface::update(app.world_mut(), leaf, 1, true, &delta),
        Err(SurfaceError::TextTooLong { .. })
    ));

    check(&mut app);
    let world = app.world();
    assert!(world.get::<Children>(leaf).is_none_or(|c| c.is_empty()));
    assert_eq!(world.get::<SurfaceState>(leaf).unwrap().revision, 0);
    assert_eq!(generation(world, root), gen0, "refusals commit nothing");
}

#[test]
fn updates_are_paced_per_surface() {
    let mut app = app();
    let (_, _, _, leaf) = opened(&mut app);
    let delta = compact(1, false, Some("tick"));
    for revision in 1..=u64::from(MAX_UPDATES_PER_SECOND) {
        surface::update(app.world_mut(), leaf, revision, true, &delta).unwrap();
    }
    let next = u64::from(MAX_UPDATES_PER_SECOND) + 1;
    assert_eq!(
        surface::update(app.world_mut(), leaf, next, true, &delta),
        Err(SurfaceError::RateLimited)
    );
    // The window moves with the server clock.
    app.world_mut().resource_mut::<Clock>().now_ms = 1000;
    surface::update(app.world_mut(), leaf, next, true, &delta).unwrap();
    check(&mut app);
    // Bytes count too: one delta past the per-second budget is refused even when it is the
    // only one in its window.
    app.world_mut().resource_mut::<Clock>().now_ms = 2000;
    let text = "x".repeat(4000);
    let big = compact(70, false, Some(&text));
    assert!(big.len() > surface::MAX_BYTES_PER_SECOND);
    assert_eq!(
        surface::update(app.world_mut(), leaf, next + 1, true, &big),
        Err(SurfaceError::RateLimited)
    );
    // Refusals spend the window: what fits still goes through afterwards.
    surface::update(app.world_mut(), leaf, next + 1, true, &delta).unwrap();
    assert_eq!(
        app.world().get::<SurfaceState>(leaf).unwrap().revision,
        next + 1
    );
    check(&mut app);
}

// ---------------------------------------------------------------------------------------------
// Close
// ---------------------------------------------------------------------------------------------

#[test]
fn close_despawns_the_subtree_and_frees_the_leaf() {
    let mut app = app();
    let (ws, root, _, leaf) = opened(&mut app);
    let viewer =
        ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    let mut provider = Provider::new(&app);
    let (column, rows) = provider.column(&["a", "b"]);
    let delta = provider.export_all();
    surface::update(app.world_mut(), leaf, 1, true, &delta).unwrap();
    app.update();
    check(&mut app);
    let templates: Vec<Entity> = {
        let state = app.world().get::<SurfaceState>(leaf).unwrap();
        [column, rows[0], rows[1]]
            .iter()
            .map(|e| state.entity(*e).unwrap())
            .collect()
    };
    let leaf_id = node_id(app.world(), leaf);
    let before = generation(app.world(), root);
    let closed = call(&mut app, "fux/surface.close", json!({ "surface": leaf_id })).unwrap();
    assert_eq!(closed["generation"], json!(before + 1));
    {
        let world = app.world();
        assert!(world.get::<Surface>(leaf).is_none());
        assert!(world.get::<SurfaceState>(leaf).is_none());
        assert!(world.get::<TemplateNode>(leaf).is_some(), "the leaf stays");
        assert!(world.get::<Children>(leaf).is_none_or(|c| c.is_empty()));
        for t in &templates {
            assert!(world.get_entity(*t).is_err(), "{t} despawned");
        }
    }
    app.update();
    check(&mut app);
    let world = app.world_mut();
    let iroot = instance_root(world, viewer);
    let ileaf = instance_of(world, iroot, leaf);
    assert!(world.get::<Children>(ileaf).is_none_or(|c| c.is_empty()));
    assert!(world.get::<Surface>(ileaf).is_none());
    // The leaf is an ordinary leaf again.
    ops::spawn_node(world, leaf, None, grow(), None).unwrap();
    let again = call(&mut app, "fux/surface.close", json!({ "surface": leaf_id }));
    assert_eq!(again.unwrap_err().code, codes::NOT_FOUND);
}

// ---------------------------------------------------------------------------------------------
// Replication: a viewer's SceneFrame carries the surface subtree and paints its Text
// ---------------------------------------------------------------------------------------------

const STEP: Duration = Duration::from_millis(2);
const PATIENCE: Duration = Duration::from_secs(5);

type Probe = Box<dyn FnOnce(&mut World) + Send>;

struct Server {
    port: u16,
    token: String,
    probes: mpsc::Sender<Probe>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    fn start() -> Self {
        let (inbound_tx, inbound_rx) = async_channel::unbounded::<Inbound>();
        let (probes, probe_rx) = mpsc::channel::<Probe>();
        let (ready_tx, ready_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut app = fux::app::build_headless(&Config::default());
            app.add_plugins(AttachPlugin {
                inbound: inbound_tx,
            })
            .insert_resource(ServerInstance {
                name: "test".into(),
                nonce: NONCE.into(),
                pid: std::process::id(),
                started_ms: 0,
            });
            app.finish();
            app.cleanup();
            let mut adapter = AttachAdapter::from_app(&app).unwrap();
            app.update();
            scene(&mut app);
            let port = app.world().resource::<AttachEndpoint>().port;
            let token = app.world().resource::<AttachToken>().0.clone();
            ready_tx.send((port, token)).unwrap();
            while !stopping.load(Ordering::Relaxed) {
                while let Ok(probe) = probe_rx.try_recv() {
                    probe(app.world_mut());
                }
                while let Ok(message) = inbound_rx.try_recv() {
                    app.world_mut().write_message(message);
                }
                app.update();
                check_invariants(app.world_mut()).unwrap();
                let effects: Vec<Effect> = app
                    .world_mut()
                    .resource_mut::<Messages<Effect>>()
                    .drain()
                    .collect();
                for effect in effects {
                    if AttachAdapter::handles(&effect) {
                        assert!(adapter.apply(effect));
                    }
                }
                thread::sleep(STEP);
            }
        });
        let (port, token) = ready_rx.recv().unwrap();
        Self {
            port,
            token,
            probes,
            stop,
            thread: Some(thread),
        }
    }

    fn with_world<T: Send + 'static>(&self, f: impl FnOnce(&mut World) -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        self.probes
            .send(Box::new(move |world| {
                let _ = tx.send(f(world));
            }))
            .unwrap();
        rx.recv_timeout(PATIENCE).expect("server thread answers")
    }

    fn connect(&self, rows: u16, cols: u16) -> Client {
        let stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        stream.set_nodelay(true).unwrap();
        let mut client = Client {
            stream,
            buf: Vec::new(),
        };
        client.send(&Hello {
            token: self.token.clone(),
            instance: NONCE.into(),
            workspace: "default".into(),
            stream: String::new(),
            viewport: Viewport { rows, cols },
            exact_target: None,
        });
        match client.recv() {
            Some(ServerFrame::Welcome(_)) => {}
            other => panic!("expected Welcome, got {other:?}"),
        }
        client
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Client {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Client {
    fn send<T: Serialize>(&mut self, frame: &T) {
        wire::encode(frame, &mut self.buf).unwrap();
        self.stream.write_all(&self.buf).unwrap();
    }

    fn recv(&mut self) -> Option<ServerFrame> {
        let mut prefix = [0u8; wire::FRAME_PREFIX_BYTES];
        match self.stream.read_exact(&mut prefix) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(error) => panic!("read failed: {error}"),
        }
        let len = wire::payload_len(&prefix).unwrap().unwrap();
        self.buf.clear();
        self.buf.resize(len, 0);
        self.stream.read_exact(&mut self.buf).unwrap();
        Some(serde_json::from_slice(&self.buf).unwrap())
    }

    fn expect_scene(&mut self) -> SceneFrame {
        match self.recv() {
            Some(ServerFrame::Scene(scene)) => scene,
            other => panic!("expected Scene, got {other:?}"),
        }
    }
}

#[test]
fn viewers_receive_and_paint_the_surface_subtree() {
    let server = Server::start();
    let mut client = server.connect(23, 80);
    let mut viewer_app = viewer::build(80, 24);
    let first = client.expect_scene();
    assert!(first.full);
    let mut frames = vec![first];

    let registry = server.with_world(|world| world.resource::<AppTypeRegistry>().clone());
    let mut provider = Provider {
        world: World::new(),
        registry,
    };
    provider.column(&["tasks", "checks"]);
    let delta = provider.export_all();
    server.with_world(move |world| {
        let leaf = world
            .query_filtered::<Entity, (
                With<TemplateNode>,
                Without<fux::model::Places>,
                Without<Children>,
                With<ChildOf>,
            )>()
            .iter(world)
            .next()
            .expect("the empty leaf");
        surface::open(world, leaf, "zor").unwrap();
        surface::update(world, leaf, 1, true, &delta).unwrap();
    });

    let deadline = Instant::now() + PATIENCE;
    let scene = loop {
        let scene = client.expect_scene();
        let hit = scene.scene.contains("fux::surface::Text")
            && scene.scene.contains("tasks")
            && scene.scene.contains("fux::model::components::Surface");
        frames.push(scene);
        if hit {
            break frames.last().unwrap().clone();
        }
        assert!(Instant::now() < deadline, "no surface scene arrived");
    };
    assert!(!scene.full);
    assert!(scene.scene.contains("checks"), "{}", scene.scene);

    // The viewer replicates the frames it was sent and paints the text in the leaf's rect.
    for frame in frames {
        viewer_app
            .world_mut()
            .resource_mut::<Inbox>()
            .frames
            .push(ServerFrame::Scene(frame));
        viewer_app.update();
    }
    viewer_app.update();
    let texts = viewer_app
        .world_mut()
        .query::<&Text>()
        .iter(viewer_app.world())
        .map(|t| t.0.clone())
        .collect::<Vec<_>>();
    assert_eq!(texts.len(), 2, "{texts:?}");
    let cell = |app: &App, col: u16, row: u16| -> String {
        app.world()
            .resource::<Painter>()
            .screen()
            .get(col, row)
            .map(|c| String::from(c.text.as_str()))
            .unwrap_or_default()
    };
    let row0: String = (40..45).map(|c| cell(&viewer_app, c, 0)).collect();
    let row1: String = (40..46).map(|c| cell(&viewer_app, c, 1)).collect();
    assert_eq!(row0, "tasks");
    assert_eq!(row1, "checks");
}

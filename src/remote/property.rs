//! The BRP surface as a property: whatever request arrives, through whichever
//! method, fux does not panic, its world keeps every structural invariant
//! (`crate::invariants`), and every viewer can still be painted.
//!
//! Requests go straight into the method registry the server uses, with no
//! HTTP, so this checks exactly what a client reaches. Values are real ones,
//! serialized from the live world, from defaults and from examples, then
//! mutated: boundaries, wrong types, missing and extra fields, entity IDs of
//! every class, and so on.
//!
//! `FUX_BRP_CASES` sets the number of requests (default 2,000) and
//! `FUX_BRP_SEED` the seed. `FUX_BRP_COLLECT=1` keeps going after a failure
//! and reports each distinct one, for exploring. A failure prints its seed,
//! case number and request, so it can be replayed.
use crate::{
    assets::Settings,
    control::{Axis, Command, Control, Order, Scope, Shutdown, Subject, UserInput},
    model::*,
    protocol::{Direction, Input, Key, Modifiers},
    server::ServerPlugin,
    testing::*,
};
use bevy_app::App;
use bevy_ecs::{
    prelude::*,
    reflect::{AppTypeRegistry, ReflectComponent, ReflectEvent, ReflectResource},
    resource::IsResource,
};
use bevy_reflect::{serde::TypedReflectSerializer, std_traits::ReflectDefault};
use bevy_remote::{BrpError, RemoteMethodSystemId, RemoteMethods};
use serde_json::{Value, json};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// A small deterministic generator: xorshift64*.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.below(items.len()))
    }
}

fn env(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
}

/// Strings a mutation may put anywhere. Nothing here names a program that
/// does harm if a mutated `Launch` or `Settings.shell` runs it.
const STRINGS: &[&str] = &[
    "",
    "x",
    "sleep",
    "/bin/sleep",
    "600",
    "main",
    "界界",
    "ctrl-b",
    "\u{1b}[31m",
    "fux::model::Viewer",
    "bevy_ecs::hierarchy::ChildOf",
];

const NUMBERS: &[f64] = &[
    0.0,
    1.0,
    -1.0,
    2.0,
    4096.0,
    4097.0,
    65535.0,
    65536.0,
    4_294_967_295.0,
    -2_147_483_648.0,
    1e300,
    0.5,
];

/// Methods and how often each is drawn: writes more often than reads.
const METHODS: &[(&str, usize)] = &[
    ("world.get_components", 2),
    ("world.query", 2),
    ("world.list_components", 1),
    ("world.get_components+watch", 1),
    ("world.list_components+watch", 1),
    ("world.get_resources", 1),
    ("world.list_resources", 1),
    ("registry.schema", 1),
    ("rpc.discover", 1),
    ("world.spawn_entity", 6),
    ("world.insert_components", 8),
    ("world.remove_components", 6),
    ("world.despawn_entity", 5),
    ("world.reparent_entities", 6),
    ("world.mutate_components", 10),
    ("world.insert_resources", 3),
    ("world.remove_resources", 3),
    ("world.mutate_resources", 4),
    ("world.trigger_event", 8),
    ("world.write_message", 1),
    ("fux.attach", 2),
    ("fux.frame", 2),
    ("fux.frame+watch", 1),
    ("fux.invariants", 1),
];

pub(super) fn call(
    world: &mut World,
    method: &str,
    params: Option<Value>,
) -> Result<Value, String> {
    let handler = world
        .resource::<RemoteMethods>()
        .get(method)
        .cloned()
        .ok_or_else(|| format!("no method {method}"))?;
    let outcome: Result<Result<Option<Value>, BrpError>, String> = match handler {
        RemoteMethodSystemId::Instant(id) => world
            .run_system_with(id, params)
            .map(|result| result.map(Some))
            .map_err(|error| error.to_string()),
        RemoteMethodSystemId::Watching(id) => world
            .run_system_with(id, params)
            .map_err(|error| error.to_string()),
    };
    match outcome? {
        Ok(value) => Ok(value.unwrap_or(Value::Null)),
        Err(error) => Err(error.message),
    }
}

fn bits(entity: Entity) -> Value {
    json!(entity.to_bits())
}

/// The starting world: a server with its first workspace, a split, a second
/// tab and workspace, and two viewers of different sizes. Panes run `sleep`.
pub(super) fn fixture() -> Result<App, String> {
    let mut app = App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.insert_resource(Settings {
        shell: vec!["/bin/sleep".into(), "600".into()],
        ..Default::default()
    });
    app.add_plugins((
        bevy_app::TaskPoolPlugin::default(),
        bevy_asset::AssetPlugin::default(),
        ServerPlugin,
        super::remote(),
    ));
    app.update();
    let world = app.world_mut();
    let large = call(world, "fux.attach", Some(json!({"rows":24,"cols":80})))?;
    let small = call(world, "fux.attach", Some(json!({"rows":10,"cols":30})))?;
    let viewer = large.get("viewer").cloned().ok_or("no viewer")?;
    let other = small.get("viewer").cloned().ok_or("no viewer")?;
    for (target, command) in [
        (
            &viewer,
            json!({"kind":"split","axis":"horizontal","program":null}),
        ),
        (
            &viewer,
            json!({"kind":"split","axis":"vertical","program":null}),
        ),
        (&viewer, json!({"kind":"tab_new","name":"second"})),
        (&other, json!({"kind":"workspace_new","name":"other"})),
    ] {
        call(
            world,
            "world.trigger_event",
            Some(
                json!({"event":"fux::control::Control","value":{"viewer":target,"command":command}}),
            ),
        )?;
    }
    app.update();
    Ok(app)
}

/// Every entity a request may name, by class.
struct Pools {
    layout: Vec<Entity>,
    viewers: Vec<Entity>,
    processes: Vec<Entity>,
    resources: Vec<Entity>,
    others: Vec<Entity>,
    dead: Vec<u64>,
    components: Vec<String>,
    resource_types: Vec<String>,
    events: Vec<String>,
    messages: Vec<String>,
}

impl Pools {
    fn read(world: &mut World, dead: &[u64]) -> Self {
        let layout = {
            let mut q = world.query_filtered::<Entity, LayoutRole>();
            q.iter(world).collect()
        };
        let viewers = {
            let mut q = world.query_filtered::<Entity, With<Viewer>>();
            q.iter(world).collect()
        };
        let processes = {
            let mut q = world.query_filtered::<Entity, With<ProcessState>>();
            q.iter(world).collect()
        };
        let resources = {
            let mut q = world.query_filtered::<Entity, With<IsResource>>();
            q.iter(world).collect()
        };
        let others = {
            let mut q = world.query::<Entity>();
            q.iter(world).take(512).collect()
        };
        let registry = world.resource::<AppTypeRegistry>().clone();
        let registry = registry.read();
        let names = |filter: &dyn Fn(&bevy_reflect::TypeRegistration) -> bool| -> Vec<String> {
            let mut names: Vec<String> = registry
                .iter()
                .filter(|r| filter(r))
                .map(|r| r.type_info().type_path().to_owned())
                .collect();
            names.sort();
            names
        };
        Self {
            layout,
            viewers,
            processes,
            resources,
            others,
            dead: dead.to_vec(),
            components: names(&|r| r.data::<ReflectComponent>().is_some()),
            resource_types: names(&|r| r.data::<ReflectResource>().is_some()),
            events: names(&|r| r.data::<ReflectEvent>().is_some()),
            messages: names(&|r| r.data::<bevy_ecs::reflect::ReflectMessage>().is_some()),
        }
    }

    /// An entity ID from a random class, as JSON.
    fn entity(&self, rng: &mut Rng) -> Value {
        let pick = |rng: &mut Rng, list: &[Entity]| rng.pick(list).copied().map(bits);
        let chosen = match rng.below(10) {
            0..=2 => pick(rng, &self.layout),
            3 => pick(rng, &self.viewers),
            4 => pick(rng, &self.processes),
            5 => pick(rng, &self.resources),
            6 => pick(rng, &self.others),
            7 => rng.pick(&self.dead).map(|b| json!(b)),
            8 => rng
                .pick(&[0u64, 1, u64::MAX, 1 << 32, (1u64 << 32) | 12345])
                .map(|b| json!(b)),
            _ => Some(json!(rng.next())),
        };
        chosen.unwrap_or_else(|| json!(rng.next()))
    }

    fn component(&self, rng: &mut Rng) -> String {
        if rng.chance(70) {
            // Weight fux's own types: they are what the guard is about.
            let own: Vec<&String> = self
                .components
                .iter()
                .filter(|n| n.starts_with("fux::") || n.contains("ChildOf") || n.contains("Name"))
                .collect();
            if let Some(name) = rng.pick(&own) {
                return (*name).clone();
            }
        }
        rng.pick(&self.components)
            .cloned()
            .unwrap_or_else(|| "fux::model::Nothing".into())
    }
}

/// A value of `type_path` as BRP would serialize it: from a live instance if
/// one exists, else from the type's default, else from fux's examples.
fn sample(world: &mut World, rng: &mut Rng, pools: &Pools, type_path: &str) -> Value {
    // From a live component.
    let holders: Vec<Entity> = pools
        .layout
        .iter()
        .chain(&pools.viewers)
        .chain(&pools.processes)
        .copied()
        .collect();
    if rng.chance(60) {
        for _ in 0..4 {
            let Some(&entity) = rng.pick(&holders) else {
                break;
            };
            if let Ok(found) = call(
                world,
                "world.get_components",
                Some(json!({"entity":bits(entity),"components":[type_path]})),
            ) && let Some(value) =
                found.pointer(&format!("/components/{}", type_path.replace('/', "~1")))
            {
                return value.clone();
            }
        }
    }
    if let Some(value) = example(rng, pools, type_path) {
        return value;
    }
    // From the default.
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    if let Some(registration) = registry.get_with_type_path(type_path)
        && let Some(default) = registration.data::<ReflectDefault>()
    {
        let value = default.default();
        let serializer = TypedReflectSerializer::new(value.as_partial_reflect(), &registry);
        if let Ok(value) = serde_json::to_value(&serializer) {
            return value;
        }
    }
    json!({})
}

/// Hand-written examples for types with no default and no live instance:
/// fux's events, whose commands carry the whole control vocabulary.
fn example(rng: &mut Rng, pools: &Pools, type_path: &str) -> Option<Value> {
    let entity = |rng: &mut Rng| {
        serde_json::from_value::<Entity>(pools.entity(rng)).unwrap_or(Entity::PLACEHOLDER)
    };
    let viewer = entity(rng);
    let target = entity(rng);
    let value = match type_path {
        "fux::control::Control" => {
            let commands = [
                Command::Split {
                    axis: Axis::Horizontal,
                    program: None,
                },
                Command::Split {
                    axis: Axis::Vertical,
                    program: Some("sleep 600".into()),
                },
                Command::Close {
                    subject: Subject::Pane(target),
                },
                Command::Close {
                    subject: Subject::Tab(target),
                },
                Command::Close {
                    subject: Subject::Workspace(target),
                },
                Command::Terminate,
                Command::Zoom,
                Command::Rename {
                    subject: Subject::Tab(target),
                    name: "renamed".into(),
                },
                Command::Resize {
                    axis: Axis::Horizontal,
                    grow: true,
                },
                Command::ReorderPane { order: Order::Next },
                Command::Swap { with: target },
                Command::SwapDirection {
                    direction: Direction::Left,
                },
                Command::MoveDirection {
                    direction: Direction::Up,
                },
                Command::CopyMode,
                Command::Scroll {
                    order: Order::Previous,
                },
                Command::Focus { pane: target },
                Command::FocusNext,
                Command::FocusLast,
                Command::TabNew { name: None },
                Command::Select {
                    scope: Scope::Tab,
                    entity: target,
                },
                Command::Select {
                    scope: Scope::Workspace,
                    entity: target,
                },
                Command::Next { scope: Scope::Tab },
                Command::Previous {
                    scope: Scope::Workspace,
                },
                Command::Reorder {
                    scope: Scope::Tab,
                    order: Order::Previous,
                },
                Command::WorkspaceNew { name: None },
                Command::Help,
                Command::Detach,
            ];
            let command = rng.pick(&commands).cloned()?;
            serde_json::to_value(Control { viewer, command }).ok()
        }
        "fux::control::UserInput" => {
            let inputs = [
                Input::Key {
                    key: Key::Char('b'),
                    modifiers: Modifiers {
                        ctrl: true,
                        ..Default::default()
                    },
                },
                Input::Key {
                    key: Key::Char('x'),
                    modifiers: Modifiers::default(),
                },
                Input::Paste {
                    text: "pasted".into(),
                },
                Input::PasteBegin,
                Input::Resize { rows: 5, cols: 20 },
                Input::Resize {
                    rows: 65535,
                    cols: 65535,
                },
            ];
            let input = rng.pick(&inputs).cloned()?;
            serde_json::to_value(UserInput { viewer, input }).ok()
        }
        "fux::control::Shutdown" => serde_json::to_value(Shutdown).ok(),
        _ => None,
    }?;
    Some(value)
}

/// Mutates a JSON value in place, a little or a lot.
fn mutate(rng: &mut Rng, pools: &Pools, value: &mut Value, depth: u32) {
    if depth > 6 {
        return;
    }
    match value {
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            if !keys.is_empty() && rng.chance(15) {
                if let Some(key) = rng.pick(&keys) {
                    map.remove(key);
                }
            } else if rng.chance(8) {
                map.insert("unexpected".into(), json!(1));
            }
            for key in keys {
                if let Some(child) = map.get_mut(&key)
                    && rng.chance(35)
                {
                    mutate(rng, pools, child, depth + 1);
                }
            }
        }
        Value::Array(items) => {
            if rng.chance(15) {
                items.clear();
            } else if rng.chance(10)
                && let Some(first) = items.first().cloned()
            {
                items.push(first);
            }
            for item in items.iter_mut() {
                if rng.chance(35) {
                    mutate(rng, pools, item, depth + 1);
                }
            }
        }
        Value::Number(n) => {
            // Any integer may be an entity -- one of generation 0 has bits
            // below 2^32 -- so sometimes swap in one of any class, and more
            // often for one that can only be an entity.
            let entity_like = n.as_u64().is_some_and(|n| n > u64::from(u32::MAX));
            if (entity_like && rng.chance(60)) || (n.as_u64().is_some() && rng.chance(25)) {
                *value = pools.entity(rng);
            } else if let Some(&number) = rng.pick(NUMBERS) {
                *value = if number.fract() == 0.0 && number.abs() < 1e18 && rng.chance(70) {
                    json!(number as i64)
                } else {
                    serde_json::Number::from_f64(number)
                        .map(Value::Number)
                        .unwrap_or(Value::Null)
                };
            }
        }
        Value::String(_) => {
            *value = json!(rng.pick(STRINGS).copied().unwrap_or(""));
        }
        Value::Bool(b) => *b = !*b,
        Value::Null => {}
    }
    if rng.chance(4) {
        *value = match rng.below(4) {
            0 => Value::Null,
            1 => json!("wrong"),
            2 => json!([]),
            _ => json!(-1),
        };
    }
}

/// Reflect paths into a JSON value: `.field` and `[index]`, nested.
fn paths(value: &Value, prefix: &str, out: &mut Vec<String>, depth: u32) {
    if depth > 4 {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = format!("{prefix}.{key}");
                out.push(path.clone());
                paths(child, &path, out, depth + 1);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate().take(3) {
                let path = format!("{prefix}[{i}]");
                out.push(path.clone());
                paths(child, &path, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// One generated request.
fn request(world: &mut World, rng: &mut Rng, pools: &Pools) -> (String, Option<Value>) {
    let total: usize = METHODS.iter().map(|(_, w)| w).sum();
    let mut roll = rng.below(total);
    let mut method = "rpc.discover";
    for (name, weight) in METHODS {
        if roll < *weight {
            method = name;
            break;
        }
        roll -= weight;
    }
    let value_of = |world: &mut World, rng: &mut Rng, type_path: &str| {
        let mut value = sample(world, rng, pools, type_path);
        if rng.chance(50) {
            mutate(rng, pools, &mut value, 0);
        }
        value
    };
    let params = match method {
        "world.get_components" | "world.get_components+watch" => {
            let component = pools.component(rng);
            Some(
                json!({"entity":pools.entity(rng),"components":[component],"strict":rng.chance(50)}),
            )
        }
        "world.query" => {
            let component = pools.component(rng);
            let other = pools.component(rng);
            Some(
                json!({"data":{"components":[component],"option":"all"},"filter":{"with":[other]},"strict":rng.chance(30)}),
            )
        }
        "world.list_components" | "world.list_components+watch" => {
            Some(json!({"entity":pools.entity(rng)}))
        }
        "world.get_resources" | "world.remove_resources" => {
            let resource = rng.pick(&pools.resource_types).cloned().unwrap_or_default();
            Some(json!({"resource":resource}))
        }
        "world.list_resources" | "rpc.discover" | "fux.invariants" => None,
        "registry.schema" => Some(json!({})),
        "world.spawn_entity" => {
            let mut components = serde_json::Map::new();
            for _ in 0..1 + rng.below(3) {
                let component = pools.component(rng);
                let value = value_of(world, rng, &component);
                components.insert(component, value);
            }
            Some(json!({"components":components}))
        }
        "world.insert_components" => {
            let mut components = serde_json::Map::new();
            for _ in 0..1 + rng.below(2) {
                let component = pools.component(rng);
                let value = value_of(world, rng, &component);
                components.insert(component, value);
            }
            Some(json!({"entity":pools.entity(rng),"components":components}))
        }
        "world.remove_components" => {
            let component = pools.component(rng);
            Some(json!({"entity":pools.entity(rng),"components":[component]}))
        }
        "world.despawn_entity" => Some(json!({"entity":pools.entity(rng)})),
        "world.reparent_entities" => {
            let entities: Vec<Value> = (0..1 + rng.below(3)).map(|_| pools.entity(rng)).collect();
            let parent = if rng.chance(20) {
                Value::Null
            } else {
                pools.entity(rng)
            };
            Some(json!({"entities":entities,"parent":parent}))
        }
        "world.mutate_components" => {
            let component = pools.component(rng);
            let whole = sample(world, rng, pools, &component);
            let mut found = Vec::new();
            paths(&whole, "", &mut found, 0);
            found.extend([".nope".to_owned(), "[9]".to_owned(), String::new()]);
            let path = rng.pick(&found).cloned().unwrap_or_default();
            let mut value = sample_at(&whole, &path).unwrap_or(json!(0));
            mutate(rng, pools, &mut value, 0);
            Some(
                json!({"entity":pools.entity(rng),"component":component,"path":path,"value":value}),
            )
        }
        "world.insert_resources" | "world.mutate_resources" => {
            let resource = rng.pick(&pools.resource_types).cloned().unwrap_or_default();
            let whole = value_of(world, rng, &resource);
            if method == "world.insert_resources" {
                Some(json!({"resource":resource,"value":whole}))
            } else {
                let mut found = Vec::new();
                paths(&whole, "", &mut found, 0);
                found.push(".nope".into());
                let path = rng.pick(&found).cloned().unwrap_or_default();
                let mut value = sample_at(&whole, &path).unwrap_or(json!(0));
                mutate(rng, pools, &mut value, 0);
                Some(json!({"resource":resource,"path":path,"value":value}))
            }
        }
        "world.trigger_event" => {
            let own = [
                "fux::control::Control",
                "fux::control::UserInput",
                "fux::control::Shutdown",
            ];
            let event = if rng.chance(85) {
                rng.pick(&own).map(|s| (*s).to_owned()).unwrap_or_default()
            } else {
                rng.pick(&pools.events).cloned().unwrap_or_default()
            };
            // Shutdown ends the real server; it is triggered rarely, and the
            // property is that everything before it held.
            if event.ends_with("Shutdown") && !rng.chance(5) {
                return ("rpc.discover".into(), None);
            }
            let value = value_of(world, rng, &event);
            Some(json!({"event":event,"value":value}))
        }
        "world.write_message" => {
            let message = rng.pick(&pools.messages).cloned().unwrap_or_default();
            let value = value_of(world, rng, &message);
            Some(json!({"message":message,"value":value}))
        }
        "fux.attach" => {
            let rows = rng.pick(&[0u64, 1, 24, 4096, 65535]).copied().unwrap_or(24);
            let cols = rng.pick(&[0u64, 1, 80, 4096, 65535]).copied().unwrap_or(80);
            Some(json!({"rows":rows,"cols":cols}))
        }
        "fux.frame" | "fux.frame+watch" => Some(json!({"viewer":pools.entity(rng)})),
        _ => None,
    };
    (method.to_owned(), params)
}

/// The JSON at a reflect-style path (`.a[0].b`), if there is one.
fn sample_at(value: &Value, path: &str) -> Option<Value> {
    let mut current = value;
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(['.', '[']).unwrap_or(after.len());
            let (key, tail) = after.split_at(end);
            current = current.get(key)?;
            rest = tail;
        } else {
            let after = rest.strip_prefix('[')?;
            let end = after.find(']')?;
            let index: usize = after.get(..end)?.parse().ok()?;
            current = current.get(index)?;
            rest = after.get(end + 1..)?;
        }
    }
    Some(current.clone())
}

/// What the property requires after each request.
pub(super) fn check(world: &mut World) -> Result<(), String> {
    let broken = crate::invariants::violations(world);
    if !broken.is_empty() {
        return Err(format!("invariants: {}", broken.join("; ")));
    }
    let viewers: Vec<Entity> = world
        .query_filtered::<Entity, IsViewer>()
        .iter(world)
        .collect();
    for &viewer in &viewers {
        let frame = call(world, "fux.frame", Some(json!({"viewer":bits(viewer)})))
            .map_err(|error| format!("fux.frame for viewer {viewer}: {error}"))?;
        if frame.get("paint").and_then(Value::as_str).is_none() {
            return Err(format!("fux.frame for viewer {viewer} has no paint"));
        }
    }
    obeys(world, &viewers)
}

/// fux still acts on its events: a `Control` opens the command column and a
/// `UserInput` resize changes the viewer. Every invariant can hold while fux
/// ignores commands -- despawning its observers did exactly that (028).
fn obeys(world: &mut World, viewers: &[Entity]) -> Result<(), String> {
    use crate::interaction::{Overlay, Prefix};
    // A viewer large enough to show a column, with no modal state: an open
    // column, overlay or copy mode owns input and may consume the probe.
    let Some(&viewer) = viewers.iter().find(|v| {
        world
            .get::<Viewer>(**v)
            .is_some_and(|v| v.rows >= 4 && v.cols >= 4)
    }) else {
        return Ok(());
    };
    world
        .entity_mut(viewer)
        .remove::<(Prefix, Overlay, crate::selection::Selection)>();
    world.trigger(Control {
        viewer,
        command: Command::Help,
    });
    world.flush();
    if world.get::<Prefix>(viewer).is_none() {
        let notice = world.get::<Viewer>(viewer).and_then(|v| v.notice.clone());
        let components: Vec<String> = world
            .inspect_entity(viewer)
            .map(|infos| {
                infos
                    .map(|info| info.name().shortname().to_string())
                    .collect()
            })
            .unwrap_or_default();
        return Err(format!(
            "fux ignored a Control event for viewer {viewer} (notice {:?}; components {components:?})",
            notice.map(|n| n.text)
        ));
    }
    world.entity_mut(viewer).remove::<Prefix>();
    let (rows, cols) = world
        .get::<Viewer>(viewer)
        .map(|v| (v.rows, v.cols))
        .ok_or("the viewer has no Viewer")?;
    let other = if rows == 7 { 8 } else { 7 };
    world.trigger(UserInput {
        viewer,
        input: Input::Resize { rows: other, cols },
    });
    world.flush();
    let resized = world.get::<Viewer>(viewer).map(|v| v.rows) == Some(other);
    world.trigger(UserInput {
        viewer,
        input: Input::Resize { rows, cols },
    });
    world.flush();
    if !resized {
        return Err(format!("fux ignored a UserInput event for viewer {viewer}"));
    }
    Ok(())
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "panic".into())
}

/// Runs `cases` requests from `seed`. Returns every distinct failure (all of
/// them when `collect`, else only the first).
fn run(seed: u64, cases: u64, collect: bool) -> Result<Vec<String>, String> {
    let mut rng = Rng(seed | 1);
    let mut failures: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut app = fixture()?;
    let mut dead: Vec<u64> = Vec::new();
    for case in 0..cases {
        // A fresh world now and then keeps processes and entities bounded.
        if case % 150 == 149 {
            app = fixture()?;
            dead.clear();
        }
        let world = app.world_mut();
        let pools = Pools::read(world, &dead);
        let (method, params) = request(world, &mut rng, &pools);
        if method == "world.despawn_entity"
            && let Some(entity) = params
                .as_ref()
                .and_then(|p| p.get("entity"))
                .and_then(Value::as_u64)
        {
            dead.push(entity);
        }
        let shown = format!(
            "seed {seed} case {case}: {method} {}",
            params.as_ref().map(Value::to_string).unwrap_or_default()
        );
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let _ = call(app.world_mut(), &method, params.clone());
            app.update();
            check(app.world_mut())
        }));
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(broken)) => Some(broken),
            Err(payload) => Some(format!("panic: {}", panic_text(payload))),
        };
        if let Some(failure) = failure {
            let key = format!("{method}: {}", failure.chars().take(80).collect::<String>());
            let fresh = seen.insert(key);
            if fresh {
                failures.push(format!("{failure}\n    after {shown}"));
            }
            if !collect {
                return Ok(failures);
            }
            // The world may be half-changed or poisoned; start over.
            app = fixture()?;
            dead.clear();
        }
    }
    Ok(failures)
}

#[test]
fn no_brp_request_breaks_the_server() -> Outcome {
    let cases = env("FUX_BRP_CASES").unwrap_or(2_000);
    let seed = env("FUX_BRP_SEED").unwrap_or(0x5eed_f00d);
    let collect = env("FUX_BRP_COLLECT") == Some(1);
    let failures = run(seed, cases, collect)?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} distinct failure(s):\n{}",
            failures.len(),
            failures.join("\n")
        )
        .into())
    }
}

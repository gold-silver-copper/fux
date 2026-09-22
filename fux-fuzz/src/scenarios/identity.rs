//! Hunt 6, area 3: caller-chosen entity identifiers, as a class rather than as
//! examples. Every surface that lets a request name an entity is sent the same
//! generated value classes, and after each one the server must still answer and
//! the driver must still paint.
//!
//! This scenario asserts the fix, so a vulnerable binary FAILS here. It is
//! registered but excluded from `all` until finding 003 is fixed; run it with
//! `--scenario identity`. See fux-fuzz/BREAKS.md.
//!
//! `Entity::to_bits` is an opaque encoding: its low 32 bits are the bitwise
//! complement of the entity index, so bits `0xFFFF_FFFF` is entity index 0,
//! `0xFFFF_FFFE` is index 1, and so on. Those first indices are the entities
//! Bevy allocates for resources, which is why they are in the value table.
use super::*;
use crate::runtime;

/// The value classes. `Live` names are resolved against the running world.
#[derive(Clone, Copy)]
enum Id {
    Bits(u64),
    Despawned,
    Tab,
    Viewer,
    Leaf,
    Json(&'static str),
}

const VALUES: &[(&str, Id)] = &[
    ("zero", Id::Bits(0)),
    ("entity index 0 (bits 0xFFFFFFFF)", Id::Bits(0xFFFF_FFFF)),
    ("entity index 1 (bits 0xFFFFFFFE)", Id::Bits(0xFFFF_FFFE)),
    ("entity index 2 (bits 0xFFFFFFFD)", Id::Bits(0xFFFF_FFFD)),
    ("low 32 bits zero", Id::Bits(1 << 32)),
    (
        "max generation, low 32 zero",
        Id::Bits(0xFFFF_FFFF_0000_0000),
    ),
    ("u64::MAX", Id::Bits(u64::MAX)),
    ("u64::MAX - 1", Id::Bits(u64::MAX - 1)),
    ("never allocated", Id::Bits(12_345)),
    ("despawned", Id::Despawned),
    ("wrong kind (a tab)", Id::Tab),
    ("the viewer itself", Id::Viewer),
    ("a live pane view", Id::Leaf),
    ("negative", Id::Json("-1")),
    ("float", Id::Json("1.5")),
    ("string", Id::Json("\"17\"")),
    ("null", Id::Json("null")),
    ("bool", Id::Json("true")),
    ("array", Id::Json("[1,2]")),
    ("object", Id::Json("{\"a\":1}")),
];

struct World {
    viewer: u64,
    tab: u64,
    workspace: u64,
    leaf: u64,
    despawned: u64,
}

fn survey(s: &mut Server) -> Result<World> {
    let viewer = {
        let f = s.attach(24, 80)?;
        s.frontend(f)?.viewer
    };
    s.wait("first shell output", |s| {
        Ok(s.frame(viewer, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    s.control(
        viewer,
        json!({"kind":"split","axis":"horizontal","program":null}),
    )?;
    s.wait("the split settled", |s| {
        Ok(s.query("fux::model::PaneView")?.len() >= 2)
    })?;
    let tab = runtime::id(s.query("fux::model::Tab")?.first().ok_or("no tab")?)?;
    let workspace = runtime::id(
        s.query("fux::model::Workspace")?
            .first()
            .ok_or("no workspace")?,
    )?;
    let leaf = runtime::id(s.query("fux::model::PaneView")?.first().ok_or("no leaf")?)?;
    let despawned = s
        .rpc("world.spawn_entity", json!({"components":{}}))?
        .get("entity")
        .and_then(Value::as_u64)
        .ok_or("spawn returned no entity")?;
    s.rpc("world.despawn_entity", json!({ "entity": despawned }))?;
    Ok(World {
        viewer,
        tab,
        workspace,
        leaf,
        despawned,
    })
}

fn value(w: &World, id: Id) -> Value {
    match id {
        Id::Bits(bits) => json!(bits),
        Id::Despawned => json!(w.despawned),
        Id::Tab => json!(w.tab),
        Id::Viewer => json!(w.viewer),
        Id::Leaf => json!(w.leaf),
        Id::Json(text) => serde_json::from_str(text).unwrap_or(Value::Null),
    }
}

/// Sends one value at one surface, then proves the server is still there.
fn probe(
    s: &mut Server,
    w: &World,
    surface: &str,
    label: &str,
    request: (&str, Value),
) -> Result<()> {
    let (method, params) = request;
    // The request may be refused; that is fine. What may not happen is the
    // server dying, hanging, or losing the driver's ability to paint.
    let _ = s.rpc(method, params);
    s.healthy().map_err(|e| {
        format!("application: {surface} <- {label}: the server did not survive: {e}")
    })?;
    s.rpc("rpc.discover", Value::Null).map_err(|e| {
        format!("application: {surface} <- {label}: the server stopped answering: {e}")
    })?;
    ensure(
        !s.frame(w.viewer, 24, 80)?.is_empty(),
        &format!("application: {surface} <- {label}: the driver stopped painting"),
    )?;
    Ok(())
}

fn control(w: &World, command: Value) -> (&'static str, Value) {
    (
        "world.trigger_event",
        json!({"event":"fux::control::Control","value":{"viewer":w.viewer,"command":command}}),
    )
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let w = survey(s)?;
    for (label, id) in VALUES {
        let v = value(&w, *id);
        // 1. the fux.* methods, which parse "viewer" out of params by hand
        probe(
            s,
            &w,
            "fux.frame",
            label,
            ("fux.frame", json!({ "viewer": v })),
        )?;
        probe(
            s,
            &w,
            "fux.frame+watch",
            label,
            ("fux.frame+watch", json!({ "viewer": v })),
        )?;
        // 2. the entity event targets
        probe(
            s,
            &w,
            "Control .viewer",
            label,
            (
                "world.trigger_event",
                json!({"event":"fux::control::Control",
                       "value":{"viewer":v,"command":{"kind":"zoom"}}}),
            ),
        )?;
        probe(
            s,
            &w,
            "UserInput .viewer",
            label,
            (
                "world.trigger_event",
                json!({"event":"fux::control::UserInput",
                       "value":{"viewer":v,"input":{"kind":"key","key":"a",
                                "ctrl":false,"alt":false,"shift":false}}}),
            ),
        )?;
        // 3. command subjects and targets
        for (name, command) in [
            ("close pane", json!({"kind":"close","subject":{"pane":v}})),
            ("close tab", json!({"kind":"close","subject":{"tab":v}})),
            (
                "close workspace",
                json!({"kind":"close","subject":{"workspace":v}}),
            ),
            ("focus pane", json!({"kind":"focus","pane":v})),
            (
                "rename pane",
                json!({"kind":"rename","subject":{"pane":v},"name":"x"}),
            ),
            (
                "select tab",
                json!({"kind":"select","scope":"tab","entity":v}),
            ),
            ("swap pane", json!({"kind":"swap","pane":v})),
            (
                "move to tab",
                json!({"kind":"move","to":{"kind":"tab","tab":v}}),
            ),
            (
                "load_layout workspace",
                json!({"kind":"load_layout","workspace":v,"path":"l.scn.ron","mapping":[]}),
            ),
            (
                "load_layout mapping",
                json!({"kind":"load_layout","workspace":w.workspace,
                       "path":"l.scn.ron","mapping":[[v, w.leaf]]}),
            ),
        ] {
            probe(s, &w, name, label, control(&w, command))?;
        }
        // 4. entity-typed fields inside reflected components
        for (component, body) in [
            ("fux::model::Viewing", v.clone()),
            ("fux::model::OnTab", v.clone()),
            ("fux::model::Focused", v.clone()),
            ("fux::model::PaneView", json!({ "pane": v })),
            ("bevy_ecs::hierarchy::ChildOf", v.clone()),
        ] {
            probe(
                s,
                &w,
                component,
                label,
                (
                    "world.insert_components",
                    json!({"entity":w.viewer,"components":{component:body}}),
                ),
            )?;
        }
        probe(
            s,
            &w,
            "Children with a duplicated id",
            label,
            (
                "world.insert_components",
                json!({"entity":w.tab,
                       "components":{"bevy_ecs::hierarchy::Children":[v, v]}}),
            ),
        )?;
        probe(
            s,
            &w,
            "Overlay.target.leaf",
            label,
            (
                "world.insert_components",
                json!({"entity":w.viewer,"components":{"fux::interaction::Overlay":{
                    "serial":1,
                    "target":{"leaf":v,"tab":w.tab,"workspace":w.workspace},
                    "mode":{"Confirm":{"command":{"kind":"zoom"}}}}}}),
            ),
        )?;
        // 5. the entity argument of the stock methods
        for (method, params) in [
            (
                "world.get_components",
                json!({"entity":v,"components":["fux::model::Viewer"]}),
            ),
            ("world.list_components", json!({ "entity": v })),
            ("world.despawn_entity", json!({ "entity": v })),
            (
                "world.remove_components",
                json!({"entity":v,"components":["bevy_ecs::name::Name"]}),
            ),
            (
                "world.mutate_components",
                json!({"entity":v,"component":"bevy_ecs::name::Name","path":"","value":"y"}),
            ),
            (
                "world.reparent_entities",
                json!({"entities":[w.leaf],"parent":v}),
            ),
        ] {
            probe(s, &w, method, label, (method, params))?;
        }
    }
    Ok(())
}

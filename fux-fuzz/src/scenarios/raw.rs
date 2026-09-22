//! Raw API mutation, the README's "not a close" path: what a careless client
//! could do over BRP. Ordinary steps interleave with despawns, component
//! removals, dangling relationships, absurd viewer sizes and a tab reparented
//! under a pane. The oracle is narrower and stated per mutation: the server
//! never panics, every remaining viewer keeps painting within its viewport,
//! the driver's relationships are repaired as the README promises, and the
//! next ordinary step succeeds or reports a documented notice.
use super::invariant::{self, World};
use super::walk::{self, Oracle, Walker};
use super::*;
use crate::trace::Step;

/// Distinct raw mutations; `Step::Raw { kind }` selects one modulo this.
const KINDS: u8 = 23;
const VIEWING: &str = "fux::model::Viewing";
const ON_TAB: &str = "fux::model::OnTab";
const FOCUSED: &str = "fux::model::Focused";

fn pick<T: Copy>(list: &[T], index: u8) -> Option<T> {
    if list.is_empty() {
        None
    } else {
        list.get(usize::from(index) % list.len()).copied()
    }
}
fn despawn(s: &mut Server, entity: u64) -> Result<()> {
    s.rpc("world.despawn_entity", json!({"entity":entity}))?;
    Ok(())
}
fn remove(s: &mut Server, entity: u64, component: &str) -> Result<()> {
    s.rpc(
        "world.remove_components",
        json!({"entity":entity,"components":[component]}),
    )?;
    Ok(())
}
fn insert(s: &mut Server, entity: u64, component: &str, value: Value) -> Result<()> {
    s.rpc(
        "world.insert_components",
        json!({"entity":entity,"components":{component:value}}),
    )?;
    Ok(())
}
/// An API viewer to abuse: an existing one, or a fresh one.
fn victim(s: &mut Server, walker: &mut Walker) -> Result<u64> {
    if let Some(id) = walker.api_viewers.pop() {
        return Ok(id);
    }
    s.rpc("fux.attach", json!({"rows":20,"cols":60}))?
        .get("viewer")
        .and_then(Value::as_u64)
        .ok_or("attach returned no viewer".into())
}
fn viewer_exists(s: &mut Server, id: u64) -> Result<bool> {
    Ok(s.query(VIEWER)?
        .iter()
        .any(|r| runtime::id(r).ok() == Some(id)))
}
use crate::runtime;

/// What the README promises after a mutation. Despawns and relationships
/// whose target is gone or of the wrong kind are repaired from memory or
/// the first available entity. A tab unlinked from its workspace by a raw
/// hierarchy edit is not: nothing scans for it, so only painting is
/// promised.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Repair {
    Full,
    Painting,
}

/// The driver's relationships after a mutation: a workspace it views, and
/// under `Repair::Full` a tab inside it and a focused pane inside that tab
/// unless the tab is empty.
fn repaired(s: &mut Server, driver: u64, repair: Repair) -> Result<Option<String>> {
    let w = World::read(s)?;
    let Ok(ws) = s.relation(driver, VIEWING) else {
        return Ok(Some("driver has no Viewing".into()));
    };
    if !w.workspaces.contains(&ws) {
        return Ok(Some(format!("driver Viewing {ws} is not a workspace")));
    }
    if repair == Repair::Painting {
        return Ok(None);
    }
    let Ok(tab) = s.relation(driver, ON_TAB) else {
        return Ok(Some("driver has no OnTab".into()));
    };
    if !w.tabs_of(ws).contains(&tab) {
        return Ok(Some(format!("driver OnTab {tab} is not a tab of {ws}")));
    }
    let leaves = w.leaves_under(tab);
    match s.relation(driver, FOCUSED) {
        Ok(f) if leaves.contains(&f) => Ok(None),
        Ok(f) => Ok(Some(format!("driver Focused {f} is not in tab {tab}"))),
        Err(_) if leaves.is_empty() => Ok(None),
        Err(_) => Ok(Some(format!(
            "driver has no focus although tab {tab} has panes"
        ))),
    }
}
fn wait_repaired(s: &mut Server, walker: &mut Walker, what: &str, repair: Repair) -> Result<()> {
    let driver = walker.driver;
    let mut last = None;
    s.wait("driver relationships repaired", |s| {
        last = repaired(s, driver, repair)?;
        Ok(last.is_none())
    })
    .map_err(|e| {
        format!(
            "application: after {what} the driver was not repaired: {}: {e}",
            last.clone().unwrap_or_default()
        )
    })?;
    Ok(())
}

/// A partial payload for a reflected component, the read-modify-write that
/// once panicked the whole server (agent exercises, F1). Every one must be a
/// JSON-RPC rejection naming the missing field, and change nothing.
fn partial(
    s: &mut Server,
    walker: &mut Walker,
    index: u8,
    w: &World,
) -> Result<(&'static str, Value)> {
    let driver = walker.driver;
    let processes: Vec<u64> = w.states.iter().map(|(e, _)| *e).collect();
    let leaves = w.leaves();
    let (what, entity, component, value, field): (&str, u64, &str, Value, &str) = match index % 4 {
        0 => (
            "insert a Viewer without rows",
            driver,
            VIEWER,
            json!({"cols":60,"zoom":false,"scrollback":0,"notice":null}),
            "rows",
        ),
        1 => {
            let Some(pane) = pick(&processes, index) else {
                return Ok(("skip", json!("no process")));
            };
            (
                "insert a Launch with argv only",
                pane,
                LAUNCH,
                json!({"argv":["/bin/sh"]}),
                "cwd",
            )
        }
        2 => {
            let Some(leaf) = pick(&leaves, index) else {
                return Ok(("skip", json!("no pane view")));
            };
            (
                "insert an empty PaneView",
                leaf,
                invariant::PANE_VIEW,
                json!({}),
                "pane",
            )
        }
        _ => (
            "insert an empty Prefix",
            driver,
            "fux::interaction::Prefix",
            json!({}),
            "scroll",
        ),
    };
    let before = states(s)?;
    let outcome = insert(s, entity, component, value);
    let error = match outcome {
        Ok(()) => {
            return Err(format!("application: {what} was accepted instead of rejected").into());
        }
        Err(error) => error.to_string(),
    };
    ensure(
        error.contains(&format!("missing field `{field}`")),
        &format!("application: {what} was rejected without naming `{field}`: {error}"),
    )?;
    ensure(
        states(s)? == before,
        &format!("application: {what} changed a process state"),
    )?;
    ensure(
        !walk::raw_paint(s, driver)?.is_empty(),
        &format!("application: the driver painted nothing after {what}"),
    )?;
    Ok((what, json!({"entity":entity,"field":field})))
}

/// Applies one raw mutation and its per-mutation oracle. Laying out a
/// 4096x4096 viewer takes a debug build well over the ordinary request
/// timeout, so mutations run with a generous one.
pub(super) fn mutate(
    s: &mut Server,
    walker: &mut Walker,
    kind: u8,
    index: u8,
    w: &World,
) -> Result<(String, Value)> {
    let ordinary = s.request_timeout;
    s.request_timeout = std::time::Duration::from_secs(5);
    let outcome = mutate_with(s, walker, kind, index, w);
    s.request_timeout = ordinary;
    outcome
}
fn mutate_with(
    s: &mut Server,
    walker: &mut Walker,
    kind: u8,
    index: u8,
    w: &World,
) -> Result<(String, Value)> {
    let driver = walker.driver;
    let viewing = s.relation(driver, VIEWING).ok();
    let tabs_here: Vec<u64> = viewing.map(|ws| w.tabs_of(ws)).unwrap_or_default();
    let containers: Vec<u64> = w
        .splits
        .iter()
        .copied()
        .filter(|e| !w.tabs.contains(e) && !w.workspaces.contains(e))
        .collect();
    let leaves = w.leaves();
    let (what, detail): (&str, Value) = match kind % KINDS {
        0 => {
            let id = victim(s, walker)?;
            despawn(s, id)?;
            s.wait("despawned viewer gone", |s| Ok(!viewer_exists(s, id)?))?;
            ("despawn an API viewer", json!(id))
        }
        1 => {
            let Some(tab) = pick(&tabs_here, index) else {
                return Ok(("skip".into(), json!("no tab")));
            };
            despawn(s, tab)?;
            wait_repaired(s, walker, "despawning a tab", Repair::Full)?;
            ("despawn a tab", json!(tab))
        }
        2 => {
            let Some(split) = pick(&containers, index) else {
                return Ok(("skip".into(), json!("no split container")));
            };
            despawn(s, split)?;
            wait_repaired(s, walker, "despawning a split container", Repair::Full)?;
            ("despawn a split container", json!(split))
        }
        3 => {
            let Some(leaf) = pick(&leaves, index) else {
                return Ok(("skip".into(), json!("no pane view")));
            };
            despawn(s, leaf)?;
            wait_repaired(s, walker, "despawning a pane view", Repair::Full)?;
            ("despawn a pane view", json!(leaf))
        }
        4 => {
            remove(s, driver, VIEWING)?;
            wait_repaired(s, walker, "removing the driver's Viewing", Repair::Full)?;
            ("remove Viewing", json!(driver))
        }
        5 => {
            remove(s, driver, ON_TAB)?;
            wait_repaired(s, walker, "removing the driver's OnTab", Repair::Full)?;
            ("remove OnTab", json!(driver))
        }
        6 => {
            remove(s, driver, FOCUSED)?;
            wait_repaired(s, walker, "removing the driver's Focused", Repair::Full)?;
            ("remove Focused", json!(driver))
        }
        7 => {
            let Some(ws) = pick(&w.workspaces, index) else {
                return Ok(("skip".into(), json!("no workspace")));
            };
            remove(s, ws, invariant::ORDER)?;
            ("remove WorkspaceOrder", json!(ws))
        }
        8 => {
            let parents: Vec<u64> = w.children.iter().map(|(p, _)| *p).collect();
            let Some(parent) = pick(&parents, index) else {
                return Ok(("skip".into(), json!("no parent")));
            };
            remove(s, parent, invariant::CHILDREN)?;
            wait_repaired(s, walker, "removing a Children component", Repair::Full)?;
            ("remove Children", json!(parent))
        }
        9 => {
            let Some(tab) = pick(&tabs_here, index) else {
                return Ok(("skip".into(), json!("no tab")));
            };
            remove(s, tab, invariant::CHILD_OF)?;
            wait_repaired(s, walker, "removing a tab's ChildOf", Repair::Full)?;
            ("remove a tab's ChildOf", json!(tab))
        }
        10 => {
            let dead = s
                .rpc("world.spawn_entity", json!({"components":{}}))?
                .get("entity")
                .and_then(Value::as_u64)
                .ok_or("spawn returned no entity")?;
            despawn(s, dead)?;
            insert(s, driver, VIEWING, json!(dead))?;
            wait_repaired(
                s,
                walker,
                "inserting a Viewing that points at a despawned entity",
                Repair::Full,
            )?;
            ("insert Viewing at a despawned entity", json!(dead))
        }
        11 => {
            let Some(leaf) = pick(&leaves, index) else {
                return Ok(("skip".into(), json!("no pane view")));
            };
            insert(s, driver, VIEWING, json!(leaf))?;
            wait_repaired(
                s,
                walker,
                "inserting a Viewing that points at a pane",
                Repair::Full,
            )?;
            ("insert Viewing at a pane view", json!(leaf))
        }
        12 | 13 => {
            let id = victim(s, walker)?;
            let (rows, cols) = if kind % KINDS == 12 {
                (0, 0)
            } else {
                (4096, 4096)
            };
            insert(
                s,
                id,
                VIEWER,
                json!({"rows":rows,"cols":cols,"zoom":false,"scrollback":0,"notice":null}),
            )?;
            s.wait("absurd size reflected", |s| dims(s, id, rows, cols))?;
            // The driver frames beside it; the absurd viewer's own frame is
            // too large for the harness to read at 4096, so it is restored.
            ensure(
                !walk::raw_paint(s, driver)?.is_empty(),
                &format!("application: the driver painted nothing beside a {rows}x{cols} viewer"),
            )?;
            if rows == 0 {
                ensure(
                    s.frame(id, 1, 1)?.is_empty(),
                    "application: a zero-sized viewer painted content",
                )?;
            }
            insert(
                s,
                id,
                VIEWER,
                json!({"rows":20,"cols":60,"zoom":false,"scrollback":0,"notice":null}),
            )?;
            s.wait("size restored", |s| dims(s, id, 20, 60))?;
            walker.api_viewers.push(id);
            (
                "insert an absurd Viewer size",
                json!({"viewer":id,"rows":rows,"cols":cols}),
            )
        }
        14 => {
            // A different index for the pane, or with as many tabs as panes
            // it would always be the tab's own pane and hit the cycle guard.
            let (Some(tab), Some(leaf)) = (
                pick(&tabs_here, index),
                pick(&leaves, index.wrapping_add(1)),
            ) else {
                return Ok(("skip".into(), json!("no tab or pane")));
            };
            insert(s, tab, invariant::CHILD_OF, json!(leaf))?;
            wait_repaired(s, walker, "reparenting a tab under a pane", Repair::Full)?;
            (
                "reparent a tab under a pane",
                json!({"tab":tab,"pane":leaf}),
            )
        }
        15 => {
            let id = victim(s, walker)?;
            remove(s, id, VIEWER)?;
            s.wait("viewer without Viewer leaves the query", |s| {
                Ok(!viewer_exists(s, id)?)
            })?;
            ("remove Viewer from an API viewer", json!(id))
        }
        16 => {
            let processes: Vec<u64> = w.states.iter().map(|(e, _)| *e).collect();
            let Some(pane) = pick(&processes, index) else {
                return Ok(("skip".into(), json!("no process")));
            };
            remove(s, pane, LAUNCH)?;
            s.wait("process without Launch stops", |s| {
                Ok(states(s)?.iter().all(|(e, st)| {
                    *e != pane || st.pointer("/status/kind") != Some(&json!("running"))
                }))
            })
            .map_err(|e| {
                format!("application: removing Launch did not terminate the process: {e}")
            })?;
            ("remove Launch", json!(pane))
        }
        17 => {
            let Some(leaf) = pick(&leaves, index) else {
                return Ok(("skip".into(), json!("no pane view")));
            };
            remove(s, leaf, invariant::PANE_VIEW)?;
            wait_repaired(s, walker, "removing a PaneView", Repair::Full)?;
            ("remove PaneView", json!(leaf))
        }
        18 => {
            let Some(tab) = pick(&tabs_here, index) else {
                return Ok(("skip".into(), json!("no tab")));
            };
            insert(s, tab, invariant::CHILD_OF, json!(tab))?;
            wait_repaired(s, walker, "making a tab its own parent", Repair::Full)?;
            ("insert ChildOf(self)", json!(tab))
        }
        19 => {
            let processes: Vec<u64> = w.states.iter().map(|(e, _)| *e).collect();
            let Some(pane) = pick(&processes, index) else {
                return Ok(("skip".into(), json!("no process")));
            };
            despawn(s, pane)?;
            ("despawn a process entity", json!(pane))
        }
        20 => {
            let Some(tab) = pick(&tabs_here, index) else {
                return Ok(("skip".into(), json!("no tab")));
            };
            insert(s, driver, FOCUSED, json!(tab))?;
            wait_repaired(s, walker, "focusing a tab", Repair::Full)?;
            ("insert Focused at a tab", json!(tab))
        }
        22 => partial(s, walker, index, w)?,
        _ => {
            let Some(tab) = pick(&w.tabs, index) else {
                return Ok(("skip".into(), json!("no tab")));
            };
            insert(s, driver, ON_TAB, json!(tab))?;
            wait_repaired(s, walker, "inserting OnTab at any tab", Repair::Full)?;
            ("insert OnTab at any tab", json!(tab))
        }
    };
    s.journal
        .record("raw_mutation", json!({"what":what,"detail":detail}))?;
    Ok(("raw".into(), json!({"what":what,"detail":detail})))
}

pub(super) fn run(s: &mut Server, seed: u64, steps: &[Step]) -> Result<()> {
    let mut walker = Walker::new(s)?;
    walker.oracle = Oracle::Narrow;
    s.quiet = std::env::var_os("FUX_FUZZ_VERBOSE").is_none();
    s.journal
        .record("raw_begin", json!({"seed":seed,"steps":steps.len()}))?;
    let mut mutations = 0;
    let mut tried = std::collections::BTreeSet::new();
    for (i, st) in steps.iter().enumerate() {
        walk::step(s, &mut walker, i, st, &[])?;
        if let Step::Raw { kind, .. } = st {
            mutations += 1;
            tried.insert(kind % KINDS);
        }
    }
    s.journal.record(
        "raw_end",
        json!({"seed":seed,"steps":steps.len(),"mutations":mutations,"kinds":tried}),
    )?;
    println!(
        "RAW-MUTATIONS seed {seed}: {mutations} mutations over {} kinds {:?}",
        tried.len(),
        tried
    );
    Ok(())
}

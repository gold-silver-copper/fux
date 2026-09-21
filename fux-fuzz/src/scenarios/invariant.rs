//! The structural invariants every generated step must preserve, checked for
//! every viewer. Shared by the walk, the concurrent walk and scale scenarios.
use super::*;

pub(super) const TAB: &str = "fux::model::Tab";
pub(super) const WORKSPACE: &str = "fux::model::Workspace";
pub(super) const PANE_VIEW: &str = "fux::model::PaneView";
pub(super) const SPLIT: &str = "fux::model::Split";
pub(super) const CHILD_OF: &str = "bevy_ecs::hierarchy::ChildOf";
pub(super) const CHILDREN: &str = "bevy_ecs::hierarchy::Children";
pub(super) const ORDER: &str = "fux::model::WorkspaceOrder";

/// A snapshot of the layout world, taken with one query per component.
pub(super) struct World {
    pub workspaces: Vec<u64>,
    pub tabs: Vec<u64>,
    pub splits: Vec<u64>,
    /// (leaf, pane)
    pub views: Vec<(u64, u64)>,
    /// (child, parent)
    pub parents: Vec<(u64, u64)>,
    /// (parent, children)
    pub children: Vec<(u64, Vec<u64>)>,
    pub orders: Vec<(u64, i64)>,
    pub viewers: Vec<u64>,
    /// (pane entity, state)
    pub states: Vec<(u64, Value)>,
}
impl World {
    pub fn read(s: &mut Server) -> Result<Self> {
        let ids =
            |s: &mut Server, c: &str| -> Result<Vec<u64>> { s.query(c)?.iter().map(id).collect() };
        let views = s
            .query(PANE_VIEW)?
            .iter()
            .filter_map(|r| {
                Some((
                    id(r).ok()?,
                    r.pointer("/components/fux::model::PaneView/pane")?
                        .as_u64()?,
                ))
            })
            .collect();
        let parents = s
            .query(CHILD_OF)?
            .iter()
            .filter_map(|r| {
                Some((
                    id(r).ok()?,
                    r.pointer("/components/bevy_ecs::hierarchy::ChildOf")?
                        .as_u64()?,
                ))
            })
            .collect();
        let children = s
            .query(CHILDREN)?
            .iter()
            .filter_map(|r| {
                let list = r
                    .pointer("/components/bevy_ecs::hierarchy::Children")?
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_u64)
                    .collect();
                Some((id(r).ok()?, list))
            })
            .collect();
        let orders = s
            .query(ORDER)?
            .iter()
            .filter_map(|r| {
                Some((
                    id(r).ok()?,
                    r.pointer("/components/fux::model::WorkspaceOrder")?
                        .as_i64()?,
                ))
            })
            .collect();
        Ok(Self {
            workspaces: ids(s, WORKSPACE)?,
            tabs: ids(s, TAB)?,
            splits: ids(s, SPLIT)?,
            views,
            parents,
            children,
            orders,
            viewers: ids(s, VIEWER)?,
            states: states(s)?,
        })
    }
    pub fn parent(&self, e: u64) -> Option<u64> {
        self.parents.iter().find(|(c, _)| *c == e).map(|(_, p)| *p)
    }
    pub fn leaves(&self) -> Vec<u64> {
        self.views.iter().map(|(l, _)| *l).collect()
    }
    /// Leaves beneath an entity, in hierarchy order.
    pub fn leaves_under(&self, root: u64) -> Vec<u64> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            if self.views.iter().any(|(l, _)| *l == e) {
                out.push(e);
            }
            if let Some((_, kids)) = self.children.iter().find(|(p, _)| *p == e) {
                for k in kids.iter().rev() {
                    stack.push(*k);
                }
            }
        }
        out
    }
    pub fn tabs_of(&self, workspace: u64) -> Vec<u64> {
        self.children
            .iter()
            .find(|(p, _)| *p == workspace)
            .map(|(_, kids)| {
                kids.iter()
                    .copied()
                    .filter(|k| self.tabs.contains(k))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Every structural rule, as one list of violations. Empty means healthy.
pub(super) fn violations(s: &mut Server, w: &World, process_cap: usize) -> Result<Vec<String>> {
    let mut v = Vec::new();
    for t in &w.tabs {
        match w.parent(*t) {
            Some(p) if w.workspaces.contains(&p) => {}
            p => v.push(format!("tab {t} has parent {p:?}, not a workspace")),
        }
    }
    for (leaf, pane) in &w.views {
        match w.states.iter().find(|(e, _)| e == pane) {
            None => v.push(format!(
                "view {leaf} refers to pane {pane} with no process state"
            )),
            Some((_, st)) => {
                if st.pointer("/status/kind") == Some(&json!("running"))
                    && let Ok(p) = pid(st)
                    && !alive(p)
                {
                    v.push(format!("pane {pane} reports running pid {p} which is dead"));
                }
            }
        }
    }
    let mut orders: Vec<i64> = w.orders.iter().map(|(_, o)| *o).collect();
    let n = orders.len();
    orders.sort_unstable();
    orders.dedup();
    if orders.len() != n {
        v.push("duplicate WorkspaceOrder values".into());
    }
    if w.workspaces.len() != w.orders.len() {
        v.push(format!(
            "{} workspaces but {} WorkspaceOrder components",
            w.workspaces.len(),
            w.orders.len()
        ));
    }
    // Children and ChildOf must agree in both directions for layout entities.
    for (parent, kids) in &w.children {
        for k in kids {
            if w.parent(*k) != Some(*parent) {
                v.push(format!(
                    "{parent} lists child {k} whose ChildOf is {:?}",
                    w.parent(*k)
                ));
            }
        }
    }
    for (child, parent) in &w.parents {
        let listed = w
            .children
            .iter()
            .find(|(p, _)| p == parent)
            .is_some_and(|(_, kids)| kids.contains(child));
        if !listed {
            v.push(format!(
                "{child} has ChildOf {parent} but is not in its Children"
            ));
        }
    }
    // A Split with fewer than two children should have collapsed.
    for split in &w.splits {
        if w.tabs.contains(split) || w.workspaces.contains(split) {
            continue;
        }
        let count = w
            .children
            .iter()
            .find(|(p, _)| p == split)
            .map_or(0, |(_, k)| k.len());
        if count < 2 {
            v.push(format!(
                "split container {split} has {count} children and did not collapse"
            ));
        }
    }
    let leaves = w.leaves();
    for viewer in &w.viewers {
        let viewing = s.relation(*viewer, "fux::model::Viewing");
        let on_tab = s.relation(*viewer, "fux::model::OnTab");
        let focused = s.relation(*viewer, "fux::model::Focused");
        match viewing {
            Ok(ws) if w.workspaces.contains(&ws) => match on_tab {
                Ok(t) if w.tabs.contains(&t) => {
                    if w.parent(t) != Some(ws) {
                        v.push(format!(
                            "viewer {viewer} is on tab {t} outside its workspace {ws}"
                        ));
                    }
                    let under = w.leaves_under(t);
                    match focused {
                        Ok(f) if under.contains(&f) => {}
                        Ok(f) if leaves.contains(&f) => v.push(format!(
                            "viewer {viewer} focuses {f} which is not in its tab {t}"
                        )),
                        Ok(f) => v.push(format!("viewer {viewer} Focused {f} dangles")),
                        Err(_) if under.is_empty() => {}
                        Err(_) => v.push(format!(
                            "viewer {viewer} has no focus although tab {t} has panes"
                        )),
                    }
                }
                Ok(t) => v.push(format!("viewer {viewer} OnTab {t} dangles")),
                Err(_) => v.push(format!("viewer {viewer} has no tab")),
            },
            Ok(ws) => v.push(format!("viewer {viewer} Viewing {ws} dangles")),
            Err(_) => v.push(format!("viewer {viewer} has no workspace")),
        }
        match s.rpc("fux.frame", json!({"viewer":viewer})) {
            Ok(frame) => {
                if frame.get("paint").and_then(Value::as_str).is_none() {
                    v.push(format!("viewer {viewer}: fux.frame returned no paint"));
                }
            }
            Err(e) => v.push(format!("viewer {viewer}: fux.frame failed: {e}")),
        }
    }
    if w.states.len() > process_cap {
        v.push(format!(
            "{} processes exceed the cap of {process_cap}",
            w.states.len()
        ));
    }
    Ok(v)
}

/// Every pane of the viewer's current tab must paint its own marker exactly
/// once when the viewer is not zoomed: two panes sharing a rectangle would
/// clobber each other's marker.
pub(super) fn markers_visible(
    s: &mut Server,
    w: &World,
    viewer: u64,
    marker_of: &dyn Fn(u64) -> Option<String>,
) -> Result<Vec<String>> {
    let mut v = Vec::new();
    let Ok(tab) = s.relation(viewer, "fux::model::OnTab") else {
        return Ok(v);
    };
    let zoomed = s
        .query(VIEWER)?
        .iter()
        .find(|r| id(r).ok() == Some(viewer))
        .and_then(|r| r.pointer("/components/fux::model::Viewer/zoom")?.as_bool())
        .unwrap_or(false);
    if zoomed {
        return Ok(v);
    }
    let frame = s.frame(viewer, 24, 80)?;
    for leaf in w.leaves_under(tab) {
        let Some(marker) = marker_of(leaf) else {
            continue;
        };
        let count = frame.matches(&marker).count();
        if count != 1 {
            v.push(format!(
                "viewer {viewer}: marker {marker} of pane {leaf} appears {count} times"
            ));
        }
    }
    Ok(v)
}

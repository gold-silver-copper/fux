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

/// East Asian wide characters occupy two cells; box drawing, arrows and the
/// rest of the 3-byte range do not.
fn is_wide(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x1100..=0x115F
            | 0x2E80..=0x303E
            | 0x3041..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA000..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1F64F
            | 0x1F900..=0x1F9FF
            | 0x20000..=0x3FFFD)
}

/// Content-row spans painted by absolute cursor moves: `(row, start, end)`
/// with `end` exclusive, one per pane row. Every pane paints each of its rows
/// as one move plus a line padded to its width; separators are single cells.
pub(super) fn spans(paint: &str, content_rows: u16) -> Vec<(u16, u16, u16)> {
    let chars: Vec<char> = paint.chars().collect();
    let mut out = Vec::new();
    // (row, start, end, text)
    let mut current: Option<(u16, u16, u16, String)> = None;
    let flush = |current: &mut Option<(u16, u16, u16, String)>, out: &mut Vec<(u16, u16, u16)>| {
        if let Some((r, a, b, text)) = current.take() {
            let t = text.trim();
            // fux paints status such as `[exit:N]` inside a pane's rectangle
            // on its last row; that is chrome, not a pane row. A narrow pane
            // shows it cut with an ellipsis.
            let status =
                t.starts_with('[') && (t.ends_with(']') || t.ends_with('…')) && b - a <= 16;
            if b > a && b - a > 1 && !status {
                out.push((r, a, b));
            }
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars.get(i).copied().unwrap_or(' ');
        if c == '\x1b' {
            match chars.get(i + 1).copied() {
                Some('[') => {
                    let mut j = i + 2;
                    let mut params = String::new();
                    while let Some(&p) = chars.get(j) {
                        if p.is_ascii_alphabetic() {
                            break;
                        }
                        params.push(p);
                        j += 1;
                    }
                    if chars.get(j).copied() == Some('H') {
                        flush(&mut current, &mut out);
                        let mut it = params.split(';').map(|p| p.parse::<u16>().unwrap_or(1));
                        let r = it.next().unwrap_or(1);
                        let col = it.next().unwrap_or(1);
                        if r <= content_rows {
                            current = Some((r, col, col, String::new()));
                        }
                    }
                    i = j + 1;
                }
                Some(']') => {
                    while let Some(&p) = chars.get(i) {
                        i += 1;
                        if p == '\x07' {
                            break;
                        }
                    }
                }
                _ => i += 1,
            }
            continue;
        }
        if let Some(span) = current.as_mut() {
            let width = if is_wide(c) { 2 } else { 1 };
            span.2 = span.2.saturating_add(width);
            span.3.push(c);
        }
        i += 1;
    }
    flush(&mut current, &mut out);
    out
}

/// Painted pane rectangles: groups of spans sharing a start column over
/// contiguous rows. Stacked neighbours are separated by a separator row,
/// which paints single cells and so breaks the run. A one-row rectangle
/// strictly inside a taller one is status text fux paints within a pane,
/// such as an exited pane's `[exit:N]`, and is not a pane.
pub(super) fn painted_panes(paint: &str, content_rows: u16) -> Vec<(u16, u16, u16, u16)> {
    let mut spans = spans(paint, content_rows);
    spans.sort_unstable();
    let mut panes: Vec<(u16, u16, u16, u16)> = Vec::new(); // (row0, col0, col1, rows)
    for (r, a, b) in spans {
        if let Some(last) = panes
            .iter_mut()
            .find(|p| p.1 == a && p.2 == b && p.0 + p.3 == r)
        {
            last.3 += 1;
        } else {
            panes.push((r, a, b, 1));
        }
    }
    let inside = |x: &(u16, u16, u16, u16), y: &(u16, u16, u16, u16)| {
        x.3 == 1 && y.3 > 1 && x.0 >= y.0 && x.0 < y.0 + y.3 && x.1 >= y.1 && x.2 <= y.2
    };
    let all = panes.clone();
    panes.retain(|x| !all.iter().any(|y| y != x && inside(x, y)));
    panes
}

/// Pairs of painted pane rectangles that share cells.
pub(super) fn overlaps(paint: &str, content_rows: u16) -> Vec<String> {
    let panes = painted_panes(paint, content_rows);
    let mut v = Vec::new();
    for (i, a) in panes.iter().enumerate() {
        for b in panes.iter().skip(i + 1) {
            let rows = a.0 < b.0 + b.3 && b.0 < a.0 + a.3;
            let cols = a.1 < b.2 && b.1 < a.2;
            if rows && cols {
                v.push(format!(
                    "rects (row {}, cols [{},{}), {} rows) and (row {}, cols [{},{}), {} rows) overlap",
                    a.0, a.1, a.2, a.3, b.0, b.1, b.2, b.3
                ));
            }
        }
    }
    v.truncate(3);
    v
}

fn viewer_field(s: &mut Server, viewer: u64, field: &str) -> Result<Option<Value>> {
    Ok(s.query(VIEWER)?
        .iter()
        .find(|r| id(r).ok() == Some(viewer))
        .and_then(|r| {
            r.pointer(&format!("/components/fux::model::Viewer/{field}"))
                .cloned()
        }))
}

/// Every structural rule, as one list of violations. Empty means healthy.
/// Viewers in `skip_paint` have an overlay open (the command column, a menu
/// or a chooser), which legitimately paints over panes.
pub(super) fn violations(
    s: &mut Server,
    w: &World,
    process_cap: usize,
    skip_paint: &[u64],
) -> Result<Vec<String>> {
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
    // A process nobody views can never be shown again; only a raw hierarchy
    // despawn, which the walks never issue, may leave one behind.
    for (pane, st) in &w.states {
        if !w.views.iter().any(|(_, p)| p == pane) {
            v.push(format!(
                "process {pane} ({}) has no view",
                st.pointer("/status/kind")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
            ));
        }
    }
    let leaves = w.leaves();
    for viewer in &w.viewers {
        let viewing = s.relation(*viewer, "fux::model::Viewing");
        let on_tab = s.relation(*viewer, "fux::model::OnTab");
        let focused = s.relation(*viewer, "fux::model::Focused");
        let mut tab_leaves = Vec::new();
        match viewing {
            Ok(ws) if w.workspaces.contains(&ws) => match on_tab {
                Ok(t) if w.tabs.contains(&t) => {
                    if w.parent(t) != Some(ws) {
                        v.push(format!(
                            "viewer {viewer} is on tab {t} outside its workspace {ws}"
                        ));
                    }
                    tab_leaves = w.leaves_under(t);
                    match focused {
                        Ok(f) if tab_leaves.contains(&f) => {}
                        Ok(f) if leaves.contains(&f) => v.push(format!(
                            "viewer {viewer} focuses {f} which is not in its tab {t}"
                        )),
                        Ok(f) => v.push(format!("viewer {viewer} Focused {f} dangles")),
                        Err(_) if tab_leaves.is_empty() => {}
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
            Ok(frame) => match frame.get("paint").and_then(Value::as_str) {
                None => v.push(format!("viewer {viewer}: fux.frame returned no paint")),
                Some(paint) if !skip_paint.contains(viewer) => {
                    let rows = viewer_field(s, *viewer, "rows")?
                        .and_then(|x| x.as_u64())
                        .unwrap_or(24) as u16;
                    let cols = viewer_field(s, *viewer, "cols")?
                        .and_then(|x| x.as_u64())
                        .unwrap_or(80);
                    let zoom = viewer_field(s, *viewer, "zoom")?
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                    let content = rows.saturating_sub(1);
                    for o in overlaps(paint, content) {
                        v.push(format!("viewer {viewer}: {o}"));
                    }
                    // Every pane of the current tab is painted, unless zoomed
                    // (one pane by design) or the viewer is too small to hold
                    // them at two cells each.
                    let painted = painted_panes(paint, content);
                    let expected = if zoom {
                        1.min(tab_leaves.len())
                    } else {
                        tab_leaves.len()
                    };
                    let room = usize::from(content) * usize::from(cols as u16) / 4;
                    // A pane already at one cell means the layout is squeezed
                    // in this viewer, which another, larger viewer's split may
                    // do; a missing rectangle is then a squeeze, not a defect.
                    let squeezed = painted.iter().any(|r| r.3 < 2 || r.2 - r.1 < 2);
                    if content >= 2
                        && expected > 0
                        && expected <= room
                        && painted.len() != expected
                        && !squeezed
                    {
                        v.push(format!(
                            "viewer {viewer}: {} panes painted but the tab has {expected} (zoom {zoom}); rects {painted:?}",
                            painted.len()
                        ));
                    }
                }
                Some(_) => {}
            },
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

/// The background every overlay surface and the command column paint with;
/// the bar uses a different foreground, so this marks an open overlay.
pub(super) const PANEL: &str = "\x1b[0;97;100m";
const BOLD_SEPARATOR: &str = "\x1b[0;1m";
const BOX_GLYPHS: [char; 7] = ['│', '─', '┼', '┤', '├', '┴', '┬'];

pub(super) struct ViewerPaint {
    pub rows: u16,
    pub cols: u16,
    pub zoom: bool,
    pub notice: Value,
    pub paint: String,
    /// The paint rendered at the viewer's size.
    pub text: String,
}
pub(super) fn viewer_paint(s: &mut Server, viewer: u64) -> Result<ViewerPaint> {
    let row = s
        .query(VIEWER)?
        .into_iter()
        .find(|r| id(r).ok() == Some(viewer))
        .ok_or("viewer disappeared")?;
    let v = component(&row, VIEWER)?;
    let rows = v.get("rows").and_then(Value::as_u64).unwrap_or(24) as u16;
    let cols = v.get("cols").and_then(Value::as_u64).unwrap_or(80) as u16;
    let paint = s
        .rpc("fux.frame", json!({"viewer":viewer}))?
        .get("paint")
        .and_then(Value::as_str)
        .ok_or("missing paint")?
        .to_owned();
    let mut parser = vt100::Parser::new(rows.max(2), cols.max(2), 0);
    parser.process(paint.as_bytes());
    Ok(ViewerPaint {
        rows,
        cols,
        zoom: v.get("zoom").and_then(Value::as_bool).unwrap_or(false),
        notice: v.get("notice").cloned().unwrap_or(Value::Null),
        paint,
        text: parser.screen().contents(),
    })
}
pub(super) fn name_of(s: &mut Server, entity: u64) -> Result<Option<String>> {
    Ok(s.query("bevy_ecs::name::Name")?
        .iter()
        .find(|r| id(r).ok() == Some(entity))
        .and_then(|r| {
            r.pointer("/components/bevy_ecs::name::Name")?
                .as_str()
                .map(str::to_owned)
        }))
}
/// Where `[exit:N]`-style status labels are painted: (row, col) one-based.
fn status_labels(paint: &str) -> Vec<(u16, u16, String)> {
    let mut out = Vec::new();
    let mut rest = paint;
    const DIM_REVERSE: &str = "\x1b[0;2;7m";
    while let Some(i) = rest.find(DIM_REVERSE) {
        let head = rest.get(..i).unwrap_or_default();
        let tail = rest.get(i + DIM_REVERSE.len()..).unwrap_or_default();
        let label: String = tail.chars().take_while(|c| *c != '\x1b').collect();
        if let Some(h) = head.rfind("\x1b[")
            && let Some(m) = head.get(h + 2..)
            && let Some((coords, _)) = m.split_once('H')
            && let Some((r, c)) = coords.split_once(';')
            && let (Ok(r), Ok(c)) = (r.parse::<u16>(), c.parse::<u16>())
        {
            out.push((r, c, label));
        }
        rest = tail;
    }
    out
}
fn find_text(text: &str, needle: &str) -> Option<(u16, u16)> {
    for (y, line) in text.lines().enumerate() {
        if let Some(i) = line.find(needle) {
            let x = line.get(..i)?.chars().count();
            return Some((y as u16 + 1, x as u16 + 1));
        }
    }
    None
}
fn first_char(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(1).collect()
}

/// What is painted, not only where: the bar names the active tab and
/// workspace and reverses the active tab, a zoomed viewer says so, the
/// focused pane's separators are bold, an exited pane shows its exit marker
/// inside its own rectangle, and nothing paints outside the viewport or
/// starts a wide glyph in the last column.
pub(super) fn presentation(
    s: &mut Server,
    w: &World,
    viewer: u64,
    marker_of: &dyn Fn(u64) -> Option<String>,
) -> Result<Vec<String>> {
    let mut v = Vec::new();
    let p = viewer_paint(s, viewer)?;
    for fault in super::chrome::inspect(&p.paint, p.rows, p.cols)? {
        v.push(format!("viewer {viewer}: {fault}"));
    }
    if p.rows < 2 || p.cols < 4 {
        return Ok(v);
    }
    let bar = p.text.lines().last().unwrap_or_default().to_owned();
    let Ok(ws) = s.relation(viewer, "fux::model::Viewing") else {
        return Ok(v);
    };
    let tab = s.relation(viewer, "fux::model::OnTab").ok();
    let focused = s.relation(viewer, "fux::model::Focused").ok();
    let overlay = p.paint.contains(PANEL);
    if let Some(tab) = tab {
        let name = name_of(s, tab)?.unwrap_or_default();
        let first = first_char(&name);
        if !first.is_empty() && !bar.contains(&first) {
            v.push(format!(
                "viewer {viewer}: the bar {bar:?} does not show the active tab {name:?}"
            ));
        }
        if !p.paint.contains("\x1b[7;1m") {
            v.push(format!(
                "viewer {viewer}: the bar does not mark the active tab {name:?}"
            ));
        }
    }
    // The workspace label gets a third of the tab allowance, which is half
    // the width once a right zone exists; below twenty columns it may be
    // nothing but an ellipsis, which the README permits.
    if p.cols >= 20 {
        let name = name_of(s, ws)?.unwrap_or_default();
        let first = first_char(&name);
        if !first.is_empty() && !bar.contains(&first) {
            v.push(format!(
                "viewer {viewer}: the bar {bar:?} does not show the workspace {name:?}"
            ));
        }
    }
    let quiet = p.notice.is_null();
    if p.zoom && quiet && p.cols >= 40 && !bar.contains("zoom") {
        v.push(format!(
            "viewer {viewer}: zoomed, but the bar {bar:?} has no zoom indicator"
        ));
    }
    let leaves = tab.map(|t| w.leaves_under(t)).unwrap_or_default();
    let content = p.rows - 1;
    let painted = painted_panes(&p.paint, content);
    if !overlay
        && !p.zoom
        && leaves.len() >= 2
        && painted.len() == leaves.len()
        && content >= 3
        && p.cols >= 10
        && focused.is_some()
    {
        let bold = p.paint.match_indices(BOLD_SEPARATOR).any(|(i, m)| {
            p.paint
                .get(i + m.len()..)
                .and_then(|r| r.chars().next())
                .is_some_and(|c| BOX_GLYPHS.contains(&c))
        });
        if !bold {
            v.push(format!(
                "viewer {viewer}: {} panes but no separator is painted bold next to the focused pane",
                leaves.len()
            ));
        }
    }
    if !overlay {
        let labels = status_labels(&p.paint);
        for leaf in &leaves {
            let Some((_, pane)) = w.views.iter().find(|(l, _)| l == leaf) else {
                continue;
            };
            let Some((_, st)) = w.states.iter().find(|(e, _)| e == pane) else {
                continue;
            };
            let Some(code) = st.pointer("/status/code").and_then(Value::as_i64) else {
                continue;
            };
            let label = format!("[exit:{code}]");
            if Some(*leaf) == focused {
                if quiet && p.cols >= 60 && !bar.contains(&label) {
                    v.push(format!(
                        "viewer {viewer}: focused pane {leaf} exited with {code} but the bar {bar:?} does not say so"
                    ));
                }
                continue;
            }
            let Some(marker) = marker_of(*leaf) else {
                continue;
            };
            let Some((my, mx)) = find_text(&p.text, &marker) else {
                continue;
            };
            let Some(rect) = painted
                .iter()
                .find(|(r, a, b, n)| my >= *r && my < r + n && mx >= *a && mx < *b)
            else {
                continue;
            };
            if rect.2 - rect.1 < label.chars().count() as u16 + 1 {
                continue;
            }
            // A stopped terminal keeps its last size, so its painted content
            // may be smaller than its layout rectangle, whose bottom-right
            // corner carries the label. The label must start at or after the
            // content's origin and lie inside no other pane's content.
            let inside = labels.iter().any(|(r, c, l)| {
                l.contains(&label)
                    && *r >= rect.0
                    && *c >= rect.1
                    && !painted
                        .iter()
                        .any(|o| o != rect && *r >= o.0 && *r < o.0 + o.3 && *c >= o.1 && *c < o.2)
            });
            if !inside {
                v.push(format!(
                    "viewer {viewer}: pane {leaf} ({marker}) exited with {code} but {label} is not painted within its own rectangle {rect:?}; labels {labels:?}"
                ));
            }
        }
    }
    Ok(v)
}

/// The narrow oracle for walks that include raw API mutations: every viewer
/// still paints, within its viewport, and nothing else is promised.
pub(super) fn narrow(s: &mut Server, w: &World) -> Result<Vec<String>> {
    let mut v = Vec::new();
    for viewer in &w.viewers {
        match viewer_paint(s, *viewer) {
            Ok(p) => {
                for fault in super::chrome::inspect(&p.paint, p.rows, p.cols)? {
                    v.push(format!("viewer {viewer}: {fault}"));
                }
            }
            Err(e) => v.push(format!("viewer {viewer}: fux.frame failed: {e}")),
        }
    }
    Ok(v)
}

/// A marker painted twice means a pane painted twice. Absence proves nothing:
/// a narrow pane clips it and a fresh child may not have printed it yet.
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
    let frame = s.frame(viewer, 24, 80)?;
    for leaf in w.leaves_under(tab) {
        let Some(marker) = marker_of(leaf) else {
            continue;
        };
        // WK1 is a prefix of WK10..WK19: count only whole tokens.
        let count = frame
            .match_indices(&marker)
            .filter(|(i, m)| {
                !frame
                    .get(i + m.len()..)
                    .and_then(|rest| rest.chars().next())
                    .is_some_and(|c| c.is_ascii_digit())
            })
            .count();
        if count > 1 {
            v.push(format!(
                "viewer {viewer}: marker {marker} of pane {leaf} appears {count} times"
            ));
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spans_and_panes_from_a_synthetic_paint() {
        // Two side-by-side panes of width 3 on rows 1-2, a separator column,
        // and the bar on row 4 (excluded).
        let paint =
            "\x1b[1;1Habc\x1b[1;5Hdef\x1b[2;1Hghi\x1b[2;5Hjkl\x1b[1;4H│\x1b[2;4H│\x1b[4;1Hbar";
        let s = spans(paint, 3);
        assert_eq!(s.len(), 4);
        let p = painted_panes(paint, 3);
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(overlaps(paint, 3).is_empty());
        let bad = "\x1b[1;1Habcd\x1b[2;1Habcd\x1b[1;3Hxyz\x1b[2;3Hxyz";
        assert_eq!(overlaps(bad, 3).len(), 1);
        // Status text inside a pane is not a pane.
        let status = "\x1b[1;1Habcdef\x1b[2;1Habcdef\x1b[3;1Habcdef\x1b[3;4H[x]";
        assert_eq!(painted_panes(status, 3).len(), 1);
        // Status text outside the painted rows, as when a smaller viewer
        // constrains the PTY, is also not a pane.
        let below = "\x1b[1;1Habcdef\x1b[2;1Habcdef\x1b[5;3H[exit:1]";
        assert_eq!(painted_panes(below, 6).len(), 1);
    }
}

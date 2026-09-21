//! The scene file as input: a seeded mutator over a saved layout, each
//! mutation loaded into a live workspace with live processes. Either the
//! load applies and the resulting world satisfies every walk invariant, or
//! it is refused with a notice and the current layout and its processes are
//! untouched. Anything in between, and any panic, is a finding.
use super::invariant::{self, World};
use super::ron::{Block, assemble, blocks, has};
use super::*;
use crate::trace::SceneCase;

const BASE: &str = "base.scn.ron";
const MUTANT: &str = "mutant.scn.ron";

fn notice(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("scene viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .get("notice")
        .cloned()
        .unwrap_or(Value::Null))
}
fn settled(s: &mut Server, viewer: u64, label: &str) -> Result<Value> {
    let mut last = Value::Null;
    s.wait(label, |s| {
        last = notice(s, viewer)?;
        let text = last.get("text").and_then(Value::as_str).unwrap_or_default();
        Ok(!text.is_empty() && !text.ends_with("..."))
    })?;
    Ok(last)
}
fn pick_index(len: usize, i: u8) -> usize {
    if len == 0 { 0 } else { usize::from(i) % len }
}
/// The saved form is `"bevy_ecs::hierarchy::Children": ([` with one id per
/// line, ten spaces deep.
const CHILDREN: &str = "Children\": ([";
fn children_of(block: &Block) -> Vec<u64> {
    let Some(start) = block.text.find(CHILDREN) else {
        return Vec::new();
    };
    let rest = block.text.get(start..).unwrap_or_default();
    let Some(end) = rest.find(']') else {
        return Vec::new();
    };
    rest.get(CHILDREN.len()..end)
        .unwrap_or_default()
        .split(',')
        .filter_map(|t| t.trim().parse().ok())
        .collect()
}
fn remove_child(block: &mut Block, child: u64) {
    block.text = block
        .text
        .replace(&format!("\n          {child},"), "")
        .replace(&format!("{child},"), "");
}

/// Applies one mutation to the saved text. Returns the mutant and a label.
pub(super) fn mutate(ron: &str, case: SceneCase) -> Result<(String, String)> {
    let (head, mut list, tail) = blocks(ron)?;
    let n = list.len();
    let label;
    match case {
        SceneCase::DropBlock(i) => {
            let k = pick_index(n, i);
            label = format!("drop block {k} ({})", kind_of(list.get(k)));
            list.remove(k);
        }
        SceneCase::DuplicateBlock(i) => {
            let k = pick_index(n, i);
            label = format!("duplicate block {k} ({})", kind_of(list.get(k)));
            let copy = Block {
                id: list.get(k).map_or(0, |b| b.id),
                text: list.get(k).map(|b| b.text.clone()).unwrap_or_default(),
            };
            list.insert(k, copy);
        }
        SceneCase::SwapIds(i, j) => {
            let (a, b) = (pick_index(n, i), pick_index(n, j));
            label = format!(
                "swap ids of blocks {a} ({}) and {b} ({})",
                kind_of(list.get(a)),
                kind_of(list.get(b))
            );
            let ia = list.get(a).map_or(0, |x| x.id);
            let ib = list.get(b).map_or(0, |x| x.id);
            if let Some(x) = list.get_mut(a) {
                x.id = ib;
            }
            if let Some(x) = list.get_mut(b) {
                x.id = ia;
            }
        }
        SceneCase::DanglingChildOf(i) => {
            let parented: Vec<usize> = (0..n)
                .filter(|k| {
                    list.get(*k)
                        .is_some_and(|b| b.text.contains("ChildOf\": ("))
                })
                .collect();
            let k = *parented
                .get(pick_index(parented.len(), i))
                .ok_or("no parented block")?;
            label = format!("dangling ChildOf on block {k} ({})", kind_of(list.get(k)));
            if let Some(b) = list.get_mut(k)
                && let Some(start) = b.text.find("ChildOf\": (")
                && let Some(end) = b.text.get(start..).and_then(|r| r.find(')'))
            {
                b.text
                    .replace_range(start + "ChildOf\": (".len()..start + end, "4294900000");
            }
        }
        SceneCase::SplitOneChild(i) => {
            let splits: Vec<usize> = (0..n)
                .filter(|k| {
                    list.get(*k)
                        .is_some_and(|b| has(b, "fux::model::Split") && children_of(b).len() >= 2)
                })
                .collect();
            let k = *splits
                .get(pick_index(splits.len(), i))
                .ok_or("no split with two children")?;
            let kids = list.get(k).map(children_of).unwrap_or_default();
            let gone = *kids.get(pick_index(kids.len(), i >> 2)).ok_or("no child")?;
            label = format!("split block {k} keeps one child: drop {gone}");
            if let Some(b) = list.get_mut(k) {
                remove_child(b, gone);
            }
            // The dropped child's whole subtree goes with it.
            let mut doomed = vec![gone];
            let mut idx = 0;
            while idx < doomed.len() {
                let e = *doomed.get(idx).unwrap_or(&0);
                for b in &list {
                    if b.text.contains(&format!("ChildOf\": ({e})")) {
                        doomed.push(b.id);
                    }
                }
                idx += 1;
            }
            list.retain(|b| !doomed.contains(&b.id));
        }
        SceneCase::SplitAncestorChild(i) => {
            let splits: Vec<usize> = (0..n)
                .filter(|k| list.get(*k).is_some_and(|b| has(b, "fux::model::Split")))
                .collect();
            let k = *splits.get(pick_index(splits.len(), i)).ok_or("no split")?;
            let parent: Option<u64> = list.get(k).and_then(|b| {
                let start = b.text.find("ChildOf\": (")? + "ChildOf\": (".len();
                let rest = b.text.get(start..)?;
                rest.get(..rest.find(')')?)?.trim().parse().ok()
            });
            let parent = parent.ok_or("split has no parent")?;
            label = format!("split block {k} lists its ancestor {parent} as a child");
            if let Some(b) = list.get_mut(k) {
                b.text = b
                    .text
                    .replacen(CHILDREN, &format!("{CHILDREN}\n          {parent},"), 1);
            }
            ensure(
                list.get(k)
                    .is_some_and(|b| children_of(b).contains(&parent)),
                "the ancestor was not added to the split's children",
            )?;
        }
        SceneCase::FlexZero(i) | SceneCase::FlexNegative(i) | SceneCase::FlexNan(i) => {
            let value = match case {
                SceneCase::FlexZero(_) => "0.0",
                SceneCase::FlexNegative(_) => "-1.0",
                _ => "NaN",
            };
            let noded: Vec<usize> = (0..n)
                .filter(|k| list.get(*k).is_some_and(|b| b.text.contains("flex_grow: ")))
                .collect();
            let k = *noded
                .get(pick_index(noded.len(), i))
                .ok_or("no Node block")?;
            label = format!("flex_grow {value} on block {k} ({})", kind_of(list.get(k)));
            if let Some(b) = list.get_mut(k)
                && let Some(start) = b.text.find("flex_grow: ")
                && let Some(end) = b.text.get(start..).and_then(|r| r.find(','))
            {
                b.text
                    .replace_range(start + "flex_grow: ".len()..start + end, value);
            }
        }
        SceneCase::GapThousand(i) => {
            let gapped: Vec<usize> = (0..n)
                .filter(|k| {
                    list.get(*k)
                        .is_some_and(|b| b.text.contains("column_gap: Px(1.0)"))
                })
                .collect();
            let k = *gapped
                .get(pick_index(gapped.len(), i))
                .ok_or("no gapped block")?;
            label = format!("gap of a thousand cells on block {k}");
            if let Some(b) = list.get_mut(k) {
                b.text = b
                    .text
                    .replace("column_gap: Px(1.0)", "column_gap: Px(1000.0)")
                    .replace("row_gap: Px(1.0)", "row_gap: Px(1000.0)");
            }
        }
        SceneCase::HugeName(i) => {
            let named: Vec<usize> = (0..n)
                .filter(|k| list.get(*k).is_some_and(|b| has(b, "bevy_ecs::name::Name")))
                .collect();
            let k = *named
                .get(pick_index(named.len(), i))
                .ok_or("no named block")?;
            label = format!("four-kilobyte name on block {k} ({})", kind_of(list.get(k)));
            let huge = "N".repeat(4096);
            if let Some(b) = list.get_mut(k)
                && let Some(start) = b.text.find("bevy_ecs::name::Name\": \"")
                && let Some(end) = b
                    .text
                    .get(start + "bevy_ecs::name::Name\": \"".len()..)
                    .and_then(|r| r.find('"'))
            {
                let from = start + "bevy_ecs::name::Name\": \"".len();
                b.text.replace_range(from..from + end, &huge);
            }
        }
        SceneCase::StripTab => {
            label = "strip the tab so the split hangs off the workspace".into();
            let root = list
                .iter()
                .find(|b| has(b, invariant::WORKSPACE))
                .ok_or("no workspace block")?
                .id;
            let tab = list
                .iter()
                .find(|b| has(b, invariant::TAB))
                .ok_or("no tab block")?
                .id;
            let kids: Vec<u64> = list
                .iter()
                .find(|b| b.id == tab)
                .map(children_of)
                .unwrap_or_default();
            list.retain(|b| b.id != tab);
            for b in &mut list {
                if kids.contains(&b.id) {
                    b.text = b.text.replace(
                        &format!("ChildOf\": ({tab})"),
                        &format!("ChildOf\": ({root})"),
                    );
                }
                if b.id == root {
                    let replacement = kids
                        .iter()
                        .map(|k| format!("{k},"))
                        .collect::<Vec<_>>()
                        .join("\n          ");
                    b.text = b.text.replace(&format!("{tab},"), &replacement);
                }
            }
        }
        SceneCase::Truncate(i) => {
            let cut = ron.len() * usize::from(i) / 256;
            let mut end = cut.min(ron.len());
            while !ron.is_char_boundary(end) {
                end -= 1;
            }
            let text = ron.get(..end).unwrap_or_default().to_owned();
            return Ok((text, format!("truncate at byte {end} of {}", ron.len())));
        }
    }
    Ok((assemble(&head, &list, &tail), label))
}
fn kind_of(block: Option<&Block>) -> &'static str {
    let Some(b) = block else { return "?" };
    if has(b, invariant::WORKSPACE) {
        "workspace"
    } else if has(b, invariant::TAB) {
        "tab"
    } else if has(b, invariant::SPLIT) {
        "split"
    } else if has(b, invariant::PANE_VIEW) {
        "pane view"
    } else {
        "other"
    }
}

/// The process entities the mutant's pane views refer to.
fn referenced_panes(mutant: &str) -> Vec<u64> {
    mutant
        .split("\"fux::model::PaneView\": (")
        .skip(1)
        .filter_map(|rest| {
            let start = rest.find("pane: ")? + "pane: ".len();
            let tail = rest.get(start..)?;
            tail.get(..tail.find(',')?)?.trim().parse().ok()
        })
        .collect()
}

/// A snapshot that must not change when a load is refused.
#[derive(PartialEq, Debug)]
struct Shape {
    workspaces: Vec<u64>,
    tabs: Vec<u64>,
    views: Vec<(u64, u64)>,
    parents: Vec<(u64, u64)>,
    pids: Vec<i32>,
    /// (pane entity, pid)
    processes: Vec<(u64, i32)>,
}
impl Shape {
    fn pid_of(&self, pane: u64) -> Option<i32> {
        self.processes
            .iter()
            .find(|(e, _)| *e == pane)
            .map(|(_, p)| *p)
    }
}
fn shape(s: &mut Server) -> Result<Shape> {
    let w = World::read(s)?;
    let processes: Vec<(u64, i32)> = w
        .states
        .iter()
        .filter_map(|(e, st)| Some((*e, pid(st).ok()?)))
        .collect();
    let mut pids: Vec<i32> = processes.iter().map(|(_, p)| *p).collect();
    pids.sort_unstable();
    let mut parents = w.parents.clone();
    parents.sort_unstable();
    let mut views = w.views.clone();
    views.sort_unstable();
    Ok(Shape {
        workspaces: w.workspaces.clone(),
        tabs: w.tabs.clone(),
        views,
        parents,
        pids,
        processes,
    })
}

pub(super) fn run(s: &mut Server, seed: u64, cases: &[SceneCase]) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    // Two tabs, three panes under two splits, all live.
    let mut markers = std::collections::BTreeMap::new();
    let shell = s.relation(v, "fux::model::Focused")?;
    markers.insert(shell, "DEFAULT-SHELL".to_owned());
    for (i, axis) in ["horizontal", "vertical"].iter().enumerate() {
        s.control(v, json!({"kind":"split","axis":axis,"program":format!("stty raw -echo; W=SF; printf \"\\033[2J\\033[H${{W}}{i}\"; exec cat > /dev/null")}))?;
        markers.insert(s.relation(v, "fux::model::Focused")?, format!("SF{i}"));
    }
    s.control(v, json!({"kind":"tab_new","name":"second"}))?;
    let first_tab = s
        .query(invariant::TAB)?
        .iter()
        .map(id)
        .collect::<Result<Vec<_>>>()?;
    s.control(
        v,
        json!({"kind":"select","scope":"tab","entity":first_tab.first().copied().ok_or("tab")?}),
    )?;
    s.wait("base layout painted", |s| {
        let frame = s.frame(v, 24, 80)?;
        Ok(frame.contains("SF0") && frame.contains("SF1"))
    })?;
    running(s)?;
    let save = |s: &mut Server| -> Result<String> {
        let ws = s.relation(v, "fux::model::Viewing")?;
        s.control(v, json!({"kind":"save_layout","workspace":ws,"path":BASE}))?;
        let done = settled(s, v, "base save")?;
        ensure(
            done.get("error") != Some(&json!(true)),
            &format!("base save failed: {done}"),
        )?;
        Ok(fs::read_to_string(s.directory.join(BASE))?)
    };
    let mut base = save(s)?;
    // The last base whose geometry is untouched; its processes are all
    // alive because every case that closes one also refreshes it.
    let mut clean = base.clone();
    s.quiet = std::env::var_os("FUX_FUZZ_VERBOSE").is_none();
    s.journal.record(
        "scene_fuzz_begin",
        json!({"seed":seed,"cases":cases.len(),"base_bytes":base.len()}),
    )?;
    let (mut applied, mut refused) = (0, 0);
    for (i, case) in cases.iter().enumerate() {
        let (mutant, label) = match mutate(&base, *case) {
            Ok(m) => m,
            Err(e) => {
                s.journal.record(
                    "scene_case_skipped",
                    json!({"index":i,"case":case,"reason":e.to_string()}),
                )?;
                continue;
            }
        };
        fs::write(s.directory.join(MUTANT), &mutant)?;
        fs::write(s.directory.join(format!("mutant-{i:03}.scn.ron")), &mutant)?;
        let before = shape(s)?;
        // Content rows only: the bar carries the load's own notice.
        let content =
            |frame: String| -> String { frame.lines().take(23).collect::<Vec<_>>().join("\n") };
        let frame_before = content(s.frame(v, 24, 80)?);
        let panics = s.stderr_text()?.matches("panicked").count();
        let ws = s.relation(v, "fux::model::Viewing")?;
        // The load clears the previous notice itself; a key would reach the
        // shell, and two Escapes make bash list completions.
        s.control(
            v,
            json!({"kind":"load_layout","workspace":ws,"path":MUTANT,"mapping":[]}),
        )?;
        let done = settled(s, v, "mutant load")?;
        let error = done.get("error") == Some(&json!(true));
        let text = done
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let geometry = matches!(
            case,
            SceneCase::FlexZero(_)
                | SceneCase::FlexNegative(_)
                | SceneCase::FlexNan(_)
                | SceneCase::GapThousand(_)
        );
        let mut problems = Vec::new();
        if s.stderr_text()?.matches("panicked").count() > panics {
            problems.push("server stderr reports a panic".to_owned());
        }
        if error {
            refused += 1;
            let after = shape(s)?;
            if after != before {
                problems.push(format!(
                    "refused with {text:?} but the world changed: {before:?} -> {after:?}"
                ));
            }
            let frame_after = content(s.frame(v, 24, 80)?);
            if frame_after != frame_before {
                let diff = frame_before
                    .lines()
                    .zip(frame_after.lines())
                    .enumerate()
                    .find(|(_, (a, b))| a != b)
                    .map(|(i, (a, b))| format!("line {i}: {a:?} -> {b:?}"))
                    .unwrap_or_else(|| "line counts differ".into());
                problems.push(format!(
                    "refused with {text:?} but the paint changed: {diff}"
                ));
            }
        } else {
            applied += 1;
            let w = World::read(s)?;
            // A scene that asks for a thousand-cell gap or a non-positive
            // flex factor gets exactly that: loaded Nodes are not rewritten,
            // so its panes may legitimately have no cells. Overlaps and every
            // structural rule still apply; only the painted count does not.
            problems.extend(
                invariant::violations(s, &w, 64, &[])?
                    .into_iter()
                    .filter(|p| !(geometry && p.contains("panes painted but the tab has"))),
            );
            problems.extend(invariant::presentation(s, &w, v, &|leaf| {
                markers.get(&leaf).cloned()
            })?);
            // Loading launches nothing. Replacing the workspace closes the
            // old one, so exactly the processes the mutant no longer views
            // are terminated; every process it still views survives.
            let viewed = referenced_panes(&mutant);
            let mut expected: Vec<i32> = before
                .views
                .iter()
                .filter(|(_, pane)| viewed.contains(pane))
                .filter_map(|(_, pane)| before.pid_of(*pane))
                .collect();
            expected.sort_unstable();
            expected.dedup();
            let mut now: Vec<i32> = w.states.iter().filter_map(|(_, st)| pid(st).ok()).collect();
            now.sort_unstable();
            if now != expected {
                problems.push(format!(
                    "an applied load left pids {now:?} running; the mutant views {expected:?}"
                ));
            }
            if w.states.len() != expected.len() {
                problems.push(format!(
                    "an applied load left {} processes; the mutant views {}",
                    w.states.len(),
                    expected.len()
                ));
            }
            // The mutant replaced the workspace, so later cases mutate a
            // fresh save of what is live now. A geometry mutation would
            // otherwise become every later case's baseline: put the last
            // clean layout back first, which its live processes allow.
            if geometry {
                fs::write(s.directory.join(MUTANT), &clean)?;
                let ws = s.relation(v, "fux::model::Viewing")?;
                s.control(
                    v,
                    json!({"kind":"load_layout","workspace":ws,"path":MUTANT,"mapping":[]}),
                )?;
                let restored = settled(s, v, "clean restore")?;
                ensure(
                    restored.get("error") != Some(&json!(true)),
                    &format!(
                        "application: reloading the clean layout after case {i} failed: {restored}"
                    ),
                )?;
                base = save(s)?;
            } else {
                markers.clear();
                base = save(s)?;
                clean = base.clone();
            }
        }
        s.journal.record(
            "scene_case",
            json!({"index":i,"case":case,"label":label,"refused":error,"notice":text,"problems":problems}),
        )?;
        ensure(
            problems.is_empty(),
            &format!(
                "application: scene case {i} ({label}; {}) broke {} rule(s): {}",
                if error {
                    format!("refused: {text}")
                } else {
                    "applied".into()
                },
                problems.len(),
                problems.join(" | ")
            ),
        )?;
    }
    s.journal.record(
        "scene_fuzz_end",
        json!({"applied":applied,"refused":refused}),
    )?;
    println!("SCENE-FUZZ seed {seed}: {applied} applied, {refused} refused");
    Ok(())
}

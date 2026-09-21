//! A seeded random walk over the whole command set, with every structural
//! invariant checked after every step. Steps carry small indices resolved
//! against the live world, so a saved trace replays the same choices.
use super::invariant::{self, World};
use super::*;
use crate::trace::Step;
use std::collections::BTreeMap;

pub(super) const PROCESS_CAP: usize = 12;

/// Availability messages any command may report when its target is absent,
/// singular or gone. These are the documented "explain why" notices.
const AVAILABILITY: &[&str] = &[
    "no pane",
    "no tab",
    "no active tab",
    "no focused pane",
    "only one pane",
    "only one tab",
    "target no longer exists",
    "target has the wrong kind for this action",
    "target is not a pane in this workspace",
    "target disappeared",
    "target removed",
    "selection target no longer exists",
    "process is not running",
    "process has exited",
    "target changed; action cancelled",
];

/// Step-specific messages beyond the availability set. Everything else,
/// including every internal or I/O error, is a finding.
fn specific(step: &Step) -> &'static [&'static str] {
    use Step::*;
    match step {
        SwapDirection(_) | MoveDirection(_) | FocusDirection(_) => &[
            "no pane in that direction",
            "destination removed",
            "destination has no parent",
            "source has no parent",
            "swap destination removed",
        ],
        MoveTo { .. } => &["destination tab removed", "destination workspace removed"],
        Rename(_) => &["pane removed"],
        CopyMode | LeaveCopyMode => &[
            "no visible content to select",
            "copy viewport exceeds 262144 cells",
            "Space starts a selection",
        ],
        Save | Load => &["os error", "missing live pane", "No such file"],
        _ => &[],
    }
}
fn allowed(step: &Step, text: &str) -> bool {
    AVAILABILITY
        .iter()
        .chain(specific(step).iter())
        .any(|a| text.contains(a))
}

fn notice(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("walk viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .get("notice")
        .cloned()
        .unwrap_or(Value::Null))
}
fn pick<T: Copy>(list: &[T], index: u8) -> Option<T> {
    if list.is_empty() {
        None
    } else {
        list.get(usize::from(index) % list.len()).copied()
    }
}
fn direction(i: u8) -> &'static str {
    ["left", "right", "up", "down"]
        .get(usize::from(i) % 4)
        .copied()
        .unwrap_or("left")
}
fn key_input(s: &mut Server, v: u64, key: &str) -> Result<()> {
    s.rpc(
        "world.trigger_event",
        json!({"event":"fux::control::UserInput","value":{"viewer":v,"input":{"kind":"key","key":key,"ctrl":false,"alt":false,"shift":false}}}),
    )?;
    Ok(())
}

pub(super) struct Walker {
    pub driver: u64,
    pub api_viewers: Vec<u64>,
    pub markers: BTreeMap<u64, String>,
    pub next_marker: u32,
    pub saved: bool,
    pub cap: usize,
    /// Walk-created panes capture what they receive to pane-<marker>.bin.
    pub capture: bool,
}
impl Walker {
    pub fn new(s: &mut Server) -> Result<Self> {
        let f = s.attach(24, 80)?;
        let v = s.frontend(f)?.viewer;
        s.wait("first shell output", |s| {
            Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
        })?;
        running(s)?;
        Ok(Self {
            driver: v,
            api_viewers: Vec::new(),
            markers: BTreeMap::new(),
            next_marker: 1,
            saved: false,
            cap: PROCESS_CAP,
            capture: false,
        })
    }
    /// A step, resolved and applied. Returns the command actually sent and
    /// its JSON, for the journal.
    pub fn apply(&mut self, s: &mut Server, step: &Step, w: &World) -> Result<(String, Value)> {
        use Step::*;
        let v = self.driver;
        // Growth beyond the cap turns into a close, deterministically.
        let step =
            if w.states.len() >= self.cap && matches!(step, Split { .. } | TabNew | WorkspaceNew) {
                &ClosePane(0)
            } else {
                step
            };
        let viewing = s.relation(v, "fux::model::Viewing").ok();
        let on_tab = s.relation(v, "fux::model::OnTab").ok();
        let focused = s.relation(v, "fux::model::Focused").ok();
        let tabs_here: Vec<u64> = viewing.map(|ws| w.tabs_of(ws)).unwrap_or_default();
        let leaves_here: Vec<u64> = on_tab.map(|t| w.leaves_under(t)).unwrap_or_default();
        let (target, command) = match step {
            Split { horizontal } => {
                let n = self.next_marker;
                self.next_marker += 1;
                let axis = if *horizontal {
                    "horizontal"
                } else {
                    "vertical"
                };
                let sink = if self.capture {
                    format!("pane-WK{n}.bin")
                } else {
                    "/dev/null".to_owned()
                };
                let program = format!(
                    "stty raw -echo; W=WK; printf \"\\033[2J\\033[H${{W}}{n}\"; exec cat > {sink}"
                );
                let before = focused;
                s.control(v, json!({"kind":"split","axis":axis,"program":program}))?;
                if let Ok(leaf) = s.relation(v, "fux::model::Focused")
                    && Some(leaf) != before
                {
                    self.markers.insert(leaf, format!("WK{n}"));
                }
                return Ok((
                    "split".into(),
                    json!({"axis":axis,"marker":format!("WK{n}")}),
                ));
            }
            TabNew => (v, json!({"kind":"tab_new","name":Value::Null})),
            WorkspaceNew => (v, json!({"kind":"workspace_new","name":Value::Null})),
            ClosePane(i) => match pick(&leaves_here, *i).or_else(|| pick(&w.leaves(), *i)) {
                Some(leaf) => (v, json!({"kind":"close","subject":{"pane":leaf}})),
                None => return Ok(("skip".into(), json!("no pane to close"))),
            },
            CloseTab(i) => match pick(&tabs_here, *i) {
                Some(t) => (v, json!({"kind":"close","subject":{"tab":t}})),
                None => return Ok(("skip".into(), json!("no tab to close"))),
            },
            CloseWorkspace(i) => {
                // Never close the last workspace: that detaches the driver.
                if w.workspaces.len() < 2 {
                    return Ok(("skip".into(), json!("last workspace")));
                }
                match pick(&w.workspaces, *i) {
                    Some(ws) => (v, json!({"kind":"close","subject":{"workspace":ws}})),
                    None => return Ok(("skip".into(), json!("no workspace"))),
                }
            }
            Terminate => (v, json!({"kind":"terminate"})),
            Zoom => (v, json!({"kind":"zoom"})),
            Rename(i) => {
                let name = format!("n{i}");
                let subject = match i % 3 {
                    0 => focused.map(|p| json!({"pane":p})),
                    1 => on_tab.map(|t| json!({"tab":t})),
                    _ => viewing.map(|ws| json!({"workspace":ws})),
                };
                match subject {
                    Some(subject) => (v, json!({"kind":"rename","subject":subject,"name":name})),
                    None => return Ok(("skip".into(), json!("nothing to rename"))),
                }
            }
            Resize { horizontal, grow } => (
                v,
                json!({"kind":"resize","axis":if *horizontal {"horizontal"} else {"vertical"},"grow":grow}),
            ),
            ReorderPane(i) => (
                v,
                json!({"kind":"reorder_pane","order":if i.is_multiple_of(2) {"previous"} else {"next"}}),
            ),
            Reorder { tab, next } => (
                v,
                json!({"kind":"reorder","scope":if *tab {"tab"} else {"workspace"},"order":if *next {"next"} else {"previous"}}),
            ),
            SwapDirection(i) => (
                v,
                json!({"kind":"swap_direction","direction":direction(*i)}),
            ),
            MoveDirection(i) => (
                v,
                json!({"kind":"move_direction","direction":direction(*i)}),
            ),
            MoveTo { kind, index } => {
                let to = match kind % 4 {
                    0 => pick(&tabs_here, *index).map(|t| json!({"kind":"tab","tab":t})),
                    1 => pick(&w.workspaces, *index)
                        .map(|ws| json!({"kind":"workspace","workspace":ws})),
                    2 => Some(json!({"kind":"new_tab","name":format!("mt{index}")})),
                    _ => Some(json!({"kind":"new_workspace","name":format!("mw{index}")})),
                };
                match to {
                    Some(to) => (v, json!({"kind":"move","to":to})),
                    None => return Ok(("skip".into(), json!("no move destination"))),
                }
            }
            Select { tab, index } => {
                let entity = if *tab {
                    pick(&tabs_here, *index)
                } else {
                    pick(&w.workspaces, *index)
                };
                match entity {
                    Some(e) => (
                        v,
                        json!({"kind":"select","scope":if *tab {"tab"} else {"workspace"},"entity":e}),
                    ),
                    None => return Ok(("skip".into(), json!("nothing to select"))),
                }
            }
            Next { tab } => (
                v,
                json!({"kind":"next","scope":if *tab {"tab"} else {"workspace"}}),
            ),
            Previous { tab } => (
                v,
                json!({"kind":"previous","scope":if *tab {"tab"} else {"workspace"}}),
            ),
            Focus(i) => match pick(&leaves_here, *i) {
                Some(leaf) => (v, json!({"kind":"focus","pane":leaf})),
                None => return Ok(("skip".into(), json!("no pane to focus"))),
            },
            FocusNext => (v, json!({"kind":"focus_next"})),
            FocusPrevious => (v, json!({"kind":"focus_previous"})),
            FocusLast => (v, json!({"kind":"focus_last"})),
            FocusDirection(i) => (
                v,
                json!({"kind":"focus_direction","direction":direction(*i)}),
            ),
            Scroll(i) => (
                v,
                json!({"kind":"scroll","order":if i.is_multiple_of(2) {"previous"} else {"next"}}),
            ),
            CopyMode => (v, json!({"kind":"copy_mode"})),
            LeaveCopyMode => {
                key_input(s, v, "q")?;
                return Ok(("key".into(), json!("q")));
            }
            Save => {
                let Some(ws) = viewing else {
                    return Ok(("skip".into(), json!("no workspace")));
                };
                self.saved = true;
                (
                    v,
                    json!({"kind":"save_layout","workspace":ws,"path":"walk.scn.ron"}),
                )
            }
            Load => {
                let Some(ws) = viewing else {
                    return Ok(("skip".into(), json!("no workspace")));
                };
                if !self.saved {
                    return Ok(("skip".into(), json!("nothing saved yet")));
                }
                (
                    v,
                    json!({"kind":"load_layout","workspace":ws,"path":"walk.scn.ron","mapping":[]}),
                )
            }
            Help => (v, json!({"kind":"help"})),
            Menu => match focused {
                Some(p) => (v, json!({"kind":"menu","subject":{"pane":p}})),
                None => return Ok(("skip".into(), json!("no pane for menu"))),
            },
            Choose => (v, json!({"kind":"choose","chooser":"tab"})),
            AttachViewer => {
                if self.api_viewers.len() >= 2 {
                    return Ok(("skip".into(), json!("viewer cap")));
                }
                let id = s
                    .rpc("fux.attach", json!({"rows":20,"cols":60}))?
                    .get("viewer")
                    .and_then(Value::as_u64)
                    .ok_or("attach returned no viewer")?;
                self.api_viewers.push(id);
                return Ok(("attach".into(), json!(id)));
            }
            DetachViewer => match self.api_viewers.pop() {
                Some(id) => (id, json!({"kind":"detach"})),
                None => return Ok(("skip".into(), json!("no api viewer"))),
            },
            Key(i) => {
                let keys = ["enter", "a", "up", "tab", "escape", "z"];
                let key = keys
                    .get(usize::from(*i) % keys.len())
                    .copied()
                    .unwrap_or("a");
                key_input(s, v, key)?;
                return Ok(("key".into(), json!(key)));
            }
        };
        s.control(target, command.clone())?;
        // Wait for load/save to settle so their result is judged, not "...".
        if matches!(step, Save | Load) {
            let mut text = String::new();
            s.wait("scene io settled", |s| {
                text = notice(s, v)?
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                Ok(!text.ends_with("..."))
            })?;
        }
        Ok((
            command
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned(),
            command,
        ))
    }
    /// Notices are cleared only by key input, so a lingering error would be
    /// misattributed to the next step. A lone Escape is harmless to a cat
    /// pane, and if the viewer is in copy mode it simply leaves it.
    pub fn clear_notice(&self, s: &mut Server) -> Result<()> {
        key_input(s, self.driver, "escape")
    }
}

/// One step with full checking; the shared engine for the walk scenarios.
pub(super) fn step(s: &mut Server, walker: &mut Walker, index: usize, st: &Step) -> Result<()> {
    walker.clear_notice(s)?;
    let before = World::read(s)?;
    let notice_before = notice(s, walker.driver)?;
    let (kind, command) = walker.apply(s, st, &before)?;
    // Let spawned processes report before liveness is judged.
    s.wait("processes settle", |s| {
        Ok(states(s)?
            .iter()
            .all(|(_, st)| st.pointer("/status/kind") != Some(&json!("starting"))))
    })?;
    let after = World::read(s)?;
    // The command column, a menu or a chooser legitimately paints over pane
    // content; judge the paint only when no overlay was just opened.
    let overlay = matches!(st, Step::Help | Step::Menu | Step::Choose);
    let mut problems = invariant::violations(s, &after, walker.cap, !overlay)?;
    let markers = walker.markers.clone();
    if !overlay {
        problems.extend(invariant::markers_visible(
            s,
            &after,
            walker.driver,
            &|leaf| markers.get(&leaf).cloned(),
        )?);
    }
    let n = notice(s, walker.driver)?;
    let changed = n != notice_before;
    let text = n
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let is_error = n.get("error") == Some(&json!(true));
    if changed && is_error && !allowed(st, &text) {
        problems.push(format!("unexpected error notice after {kind}: {text:?}"));
    }
    if text.to_lowercase().contains("panic") {
        problems.push(format!("notice mentions a panic: {text:?}"));
    }
    // Keep the journal small enough for walks of thousands of steps: the
    // command and notice are recorded only when something went wrong.
    let record = if problems.is_empty() {
        json!({"i":index,"step":st,"sent":kind,"p":after.states.len(),"l":after.views.len(),"t":after.tabs.len(),"w":after.workspaces.len()})
    } else {
        json!({"index":index,"step":st,"sent":kind,"command":command,"notice":n,"processes":after.states.len(),"leaves":after.views.len(),"tabs":after.tabs.len(),"workspaces":after.workspaces.len(),"problems":problems})
    };
    s.journal.record("walk_step", record)?;
    ensure(
        problems.is_empty(),
        &format!(
            "application: walk step {index} ({kind} {command}) broke {} invariant(s): {}",
            problems.len(),
            problems.join(" | ")
        ),
    )
}

pub(super) fn run(s: &mut Server, seed: u64, steps: &[Step]) -> Result<()> {
    let mut walker = Walker::new(s)?;
    s.journal
        .record("walk_begin", json!({"seed":seed,"steps":steps.len()}))?;
    for (i, st) in steps.iter().enumerate() {
        step(s, &mut walker, i, st)?;
    }
    let processes = states(s)?.len();
    s.journal.record(
        "walk_end",
        json!({"seed":seed,"steps":steps.len(),"processes":processes}),
    )?;
    Ok(())
}

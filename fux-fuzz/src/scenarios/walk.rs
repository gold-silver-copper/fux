//! A seeded random walk over the whole command set, with every structural
//! invariant checked after every step. Steps carry small indices resolved
//! against the live world, so a saved trace replays the same choices.
//!
//! Steps come from two sources: BRP commands, and keystrokes or mouse events
//! on the driver's real frontend. Between them the world also changes in
//! ways no command expresses: the viewer resizes, children exit or change
//! their terminal modes, the configuration is rewritten, and a second
//! frontend attaches and is hung up.
use super::invariant::{self, PANEL, World};
use super::*;
use crate::trace::{Keyed, Step};
use nix::sys::signal::Signal;
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
    "clipboard disabled",
];
const DIRECTIONAL: &[&str] = &[
    "no pane in that direction",
    "destination removed",
    "destination has no parent",
    "source has no parent",
    "swap destination removed",
];
const COPY: &[&str] = &[
    "no visible content to select",
    "copy viewport exceeds 262144 cells",
    "Space starts a selection",
    "no selection",
    "selection",
];
const SCENE: &[&str] = &["os error", "missing live pane", "No such file"];
const MOVE: &[&str] = &["destination tab removed", "destination workspace removed"];

/// Step-specific messages beyond the availability set. Everything else,
/// including every internal or I/O error, is a finding.
fn specific(step: &Step) -> Vec<&'static str> {
    use Step::*;
    match step {
        SwapDirection(_) | MoveDirection(_) | FocusDirection(_) => DIRECTIONAL.to_vec(),
        MoveTo { .. } => MOVE.to_vec(),
        Rename(_) => vec!["pane removed"],
        CopyMode | LeaveCopyMode => COPY.to_vec(),
        Save | Load => SCENE.to_vec(),
        Split { .. } | ChildExit(_) | ChildMode(_) => vec!["pane too small to split"],
        Typed(k) => match k {
            Keyed::Split { .. } => vec!["pane too small to split"],
            Keyed::FocusDirection(_) | Keyed::MoveDirection(_) => DIRECTIONAL.to_vec(),
            Keyed::Rename { .. } => vec!["pane removed"],
            Keyed::CopyMode | Keyed::CopyKey(_) | Keyed::Copy => COPY.to_vec(),
            // A menu can run any pane, tab or workspace action.
            Keyed::Menu { .. } | Keyed::Choose { .. } => DIRECTIONAL
                .iter()
                .chain(COPY)
                .chain(SCENE)
                .chain(MOVE)
                .chain(["pane removed", "pane too small to split"].iter())
                .copied()
                .collect(),
            _ => Vec::new(),
        },
        // A wheel scrolls, a right click opens a menu, a click may anchor.
        Mouse { .. } => COPY.to_vec(),
        _ => Vec::new(),
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
/// xterm's modified-arrow encodings: `\x1b[1;{modifier}{letter}`.
fn arrow(i: u8, modifier: u8) -> Vec<u8> {
    let letter = ["D", "C", "A", "B"]
        .get(usize::from(i) % 4)
        .copied()
        .unwrap_or("D");
    format!("\x1b[1;{modifier}{letter}").into_bytes()
}
fn key_input(s: &mut Server, v: u64, key: &str) -> Result<()> {
    s.rpc(
        "world.trigger_event",
        json!({"event":"fux::control::UserInput","value":{"viewer":v,"input":{"kind":"key","key":key,"ctrl":false,"alt":false,"shift":false}}}),
    )?;
    Ok(())
}
pub(super) fn raw_paint(s: &mut Server, v: u64) -> Result<String> {
    Ok(s.rpc("fux.frame", json!({"viewer":v}))?
        .get("paint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
pub(super) fn panel_painted(s: &mut Server, v: u64) -> Result<bool> {
    Ok(raw_paint(s, v)?.contains(PANEL))
}
fn screen(s: &mut Server, f: usize) -> Result<String> {
    Ok(s.frontend(f)?.parser.screen().contents())
}
fn settings(s: &mut Server) -> Result<Value> {
    Ok(s.rpc(
        "world.get_resources",
        json!({"resource":"fux::assets::Settings"}),
    )?
    .get("value")
    .cloned()
    .unwrap_or(Value::Null))
}
fn occurrences(s: &mut Server, needle: &str) -> Result<usize> {
    Ok(s.stderr_text()?.matches(needle).count())
}
/// Outer-terminal SGR mouse bytes for a zero-based cell.
fn sgr(code: u16, x: u16, y: u16, release: bool) -> Vec<u8> {
    format!(
        "\x1b[<{code};{};{}{}",
        x + 1,
        y + 1,
        if release { 'm' } else { 'M' }
    )
    .into_bytes()
}

/// What a key under the open column led to.
enum Pressed {
    /// The column closed: the command ran.
    Closed,
    /// The column stayed open with a reason and was then escaped.
    Kept,
    /// A prompt, confirmation, chooser or menu replaced the column.
    Overlay(String),
}

/// How a step is judged.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Oracle {
    /// Every structural and presentation invariant.
    Full,
    /// After raw API mutations: no panic, every viewer paints within its
    /// viewport, and notices stay documented.
    Narrow,
}

pub(super) struct Walker {
    pub driver: u64,
    pub frontend: usize,
    pub rows: u16,
    pub cols: u16,
    /// The configured prefix byte; a configuration rewrite may change it.
    pub prefix: u8,
    pub api_viewers: Vec<u64>,
    pub extra_frontends: Vec<usize>,
    pub markers: BTreeMap<u64, String>,
    pub next_marker: u32,
    pub saved: bool,
    pub cap: usize,
    /// Walk-created panes capture what they receive to pane-<marker>.bin.
    pub capture: bool,
    /// An overlay opened by this walker's last step and not yet dismissed.
    pub overlay_open: bool,
    pub copy_mode: bool,
    /// The `layout:` path the configuration currently names, if any.
    pub layout: Option<String>,
    pub oracle: Oracle,
    /// A raw hierarchy edit has left something nothing repairs, such as a
    /// tab unlinked from its workspace; later mutations promise less.
    pub degraded: bool,
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
            frontend: f,
            rows: 24,
            cols: 80,
            prefix: 0x02,
            api_viewers: Vec::new(),
            extra_frontends: Vec::new(),
            markers: BTreeMap::new(),
            next_marker: 1,
            saved: false,
            cap: PROCESS_CAP,
            capture: false,
            overlay_open: false,
            copy_mode: false,
            layout: None,
            oracle: Oracle::Full,
            degraded: false,
        })
    }
    /// Keyboard steps need room for the column and its headings; smaller
    /// viewers fall back to the BRP form of the same command.
    fn keyboard_ok(&self) -> bool {
        self.rows >= 6 && self.cols >= 40
    }
    fn send(&self, s: &mut Server, bytes: &[u8]) -> Result<()> {
        s.send(self.frontend, bytes)
    }
    fn wait_panel(&self, s: &mut Server, open: bool, label: &str) -> Result<()> {
        let v = self.driver;
        s.wait(label, |s| Ok(panel_painted(s, v)? == open))
            .map_err(|e| format!("application: {label}: {e}").into())
    }
    fn wait_text(&self, s: &mut Server, needle: &str, present: bool, label: &str) -> Result<()> {
        let (v, rows, cols) = (self.driver, self.rows, self.cols);
        let mut seen = String::new();
        s.wait(label, |s| {
            seen = s.frame(v, rows, cols)?;
            Ok(seen.contains(needle) == present)
        })
        .map_err(|e| {
            let shown: Vec<&str> = seen
                .lines()
                .map(str::trim_end)
                .filter(|l| !l.is_empty())
                .collect();
            format!("application: {label}: {e}; frame {shown:?}").into()
        })
    }
    /// Presses the prefix and waits for the column, checking that it shows
    /// the documented headings and the first binding's label.
    fn open_column(&self, s: &mut Server) -> Result<()> {
        ensure(
            !panel_painted(s, self.driver)?,
            "harness: an overlay was already open before the prefix",
        )?;
        self.send(s, &[self.prefix])?;
        self.wait_panel(s, true, "the prefix opened the command column")?;
        let frame = s.frame(self.driver, self.rows, self.cols)?;
        for needle in ["Commands", "Panes", "split side by side"] {
            ensure(
                frame.contains(needle),
                &format!(
                    "application: the command column does not show {needle:?}: {:?}",
                    frame
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .collect::<Vec<_>>()
                ),
            )?;
        }
        Ok(())
    }
    /// A lone Escape: the decoder resolves it after 35 ms; pace the next key.
    fn escape(&self, s: &mut Server) -> Result<()> {
        self.send(s, b"\x1b")?;
        std::thread::sleep(std::time::Duration::from_millis(80));
        s.pump()
    }
    /// Proves every byte sent so far was processed: the prefix opens the
    /// column, Escape closes it. An overlay a click opened closes the same way.
    fn barrier(&self, s: &mut Server) -> Result<()> {
        self.send(s, &[self.prefix])?;
        self.wait_panel(s, true, "barrier prefix")?;
        self.escape(s)?;
        self.wait_panel(s, false, "barrier escape")?;
        Ok(())
    }
    /// What a key under the column led to.
    fn press(&self, s: &mut Server, bytes: &[u8]) -> Result<Pressed> {
        let (v, rows, cols) = (self.driver, self.rows, self.cols);
        let before = notice(s, v)?;
        self.send(s, bytes)?;
        let mut frame = String::new();
        let mut panel = true;
        s.wait("shortcut processed", |s| {
            let paint = raw_paint(s, v)?;
            panel = paint.contains(PANEL);
            let mut parser = vt100::Parser::new(rows.max(2), cols.max(2), 0);
            parser.process(paint.as_bytes());
            frame = parser.screen().contents();
            let column = panel && frame.contains("Commands");
            Ok(!column || notice(s, v)? != before)
        })
        .map_err(|e| format!("application: a shortcut under the prefix was not processed: {e}"))?;
        if !panel {
            return Ok(Pressed::Closed);
        }
        if frame.contains("Commands") {
            // Unavailable or unbound: the column stays open with a reason.
            self.escape(s)?;
            self.wait_panel(s, false, "escape after an unavailable shortcut")?;
            return Ok(Pressed::Kept);
        }
        Ok(Pressed::Overlay(frame))
    }
    /// A shortcut under the open column that runs a command right away.
    fn shortcut(&self, s: &mut Server, bytes: &[u8]) -> Result<()> {
        self.open_column(s)?;
        match self.press(s, bytes)? {
            Pressed::Closed | Pressed::Kept => Ok(()),
            Pressed::Overlay(frame) => Err(format!(
                "application: a direct shortcut opened an overlay: {:?}",
                frame
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .collect::<Vec<_>>()
            )
            .into()),
        }
    }
    /// Answers whatever the last Enter opened, until no overlay remains.
    fn finish_overlay(&mut self, s: &mut Server, accept: bool, text: &str) -> Result<()> {
        for _ in 0..4 {
            if !panel_painted(s, self.driver)? {
                return Ok(());
            }
            let frame = s.frame(self.driver, self.rows, self.cols)?;
            if frame.contains("Enter accept") {
                let label = if frame.contains("save layout") || frame.contains("load layout") {
                    "walk.scn.ron"
                } else {
                    text
                };
                if frame.contains("save layout") {
                    self.saved = true;
                }
                self.send(s, label.as_bytes())?;
                self.wait_text(s, label, true, "prompt text painted")?;
                if accept {
                    self.send(s, b"\r")?;
                } else {
                    self.escape(s)?;
                }
                self.wait_text(s, "Enter accept", false, "prompt closed")?;
            } else if frame.contains("y confirm") || frame.contains("y/n") {
                self.send(s, if accept { b"y" } else { b"n" })?;
                self.wait_panel(s, false, "confirmation answered")?;
            } else if frame.contains("No destinations") {
                self.send(s, b"q")?;
                self.wait_panel(s, false, "empty chooser cancelled")?;
            } else if frame.contains('›') {
                let title = frame
                    .lines()
                    .rev()
                    .skip(1)
                    .find(|l| l.contains("choose") || l.contains("move to") || l.contains("swap"))
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_owned();
                self.send(s, if accept { b"\r" } else { b"q" })?;
                if title.is_empty() {
                    self.wait_panel(s, false, "chooser closed")?;
                } else {
                    self.wait_text(s, &title, false, "chooser closed")?;
                }
            } else {
                self.escape(s)?;
                self.wait_panel(s, false, "unknown overlay closed")?;
            }
        }
        ensure(
            !panel_painted(s, self.driver)?,
            "application: an overlay survived four answers",
        )
    }
    /// A command as keystrokes on the real frontend.
    fn typed(&mut self, s: &mut Server, k: Keyed, w: &World) -> Result<(String, Value)> {
        let v = self.driver;
        match k {
            // The new pane runs the configured shell, whose banner every
            // shell pane shares, so it gets no marker of its own.
            Keyed::Split { horizontal } => {
                self.shortcut(s, if horizontal { b"h" } else { b"v" })?
            }
            Keyed::Close { confirm } => {
                self.open_column(s)?;
                if let Pressed::Overlay(frame) = self.press(s, b"x")? {
                    ensure(
                        frame.contains("Close pane"),
                        &format!(
                            "application: `x` opened something other than a close confirmation: {frame:?}"
                        ),
                    )?;
                    self.send(s, if confirm { b"y" } else { b"n" })?;
                    self.wait_panel(s, false, "close confirmation answered")?;
                }
            }
            Keyed::FocusNext => self.shortcut(s, b"\t")?,
            Keyed::FocusPrevious => self.shortcut(s, b"\x1b[Z")?,
            Keyed::FocusLast => self.shortcut(s, b"\x7f")?,
            Keyed::FocusDirection(i) => self.shortcut(s, &arrow(i, 3))?,
            Keyed::Zoom => self.shortcut(s, b"z")?,
            Keyed::Rename { accept, wide } => {
                self.open_column(s)?;
                if let Pressed::Overlay(frame) = self.press(s, b"r")? {
                    ensure(
                        frame.contains("rename pane"),
                        &format!(
                            "application: `r` opened something other than a rename prompt: {frame:?}"
                        ),
                    )?;
                    let n = self.next_marker;
                    self.next_marker += 1;
                    let name = if wide {
                        format!("界{n}")
                    } else {
                        format!("k{n}")
                    };
                    self.finish_overlay(s, accept, &name)?;
                }
            }
            Keyed::Resize { horizontal, grow } => {
                let i = match (horizontal, grow) {
                    (true, true) => 1,
                    (true, false) => 0,
                    (false, true) => 2,
                    (false, false) => 3,
                };
                self.shortcut(s, &arrow(i, 5))?;
            }
            Keyed::MoveDirection(i) => self.shortcut(s, &arrow(i, 2))?,
            Keyed::TabNew => self.shortcut(s, b"t")?,
            Keyed::TabNext => self.shortcut(s, b"]")?,
            Keyed::TabPrevious => self.shortcut(s, b"[")?,
            Keyed::WorkspaceNew => self.shortcut(s, b"w")?,
            Keyed::WorkspaceNext => self.shortcut(s, b"}")?,
            Keyed::WorkspacePrevious => self.shortcut(s, b"{")?,
            Keyed::CopyMode => {
                self.shortcut(s, b"c")?;
                self.copy_mode = true;
            }
            Keyed::CopyKey(i) => {
                self.shortcut(s, b"c")?;
                self.copy_mode = true;
                let keys: [&[u8]; 12] = [
                    b"j", b"k", b"l", b"h", b" ", b"y", b"g", b"u", b"d", b"\x1b[6~", b"c", b"q",
                ];
                let n = usize::from(i) % 4 + 1;
                for j in 0..n {
                    let key = keys
                        .get((usize::from(i) + j) % keys.len())
                        .copied()
                        .unwrap_or(b"j");
                    self.send(s, key)?;
                }
                // The keys reach the server in order; the barrier proves it.
                // Copy mode consumes the prefix byte, so leave it first.
                self.send(s, b"q")?;
                self.copy_mode = false;
                self.barrier(s)?;
            }
            Keyed::Copy => self.shortcut(s, b"y")?,
            Keyed::Choose { tab, entry, accept } => {
                self.open_column(s)?;
                self.send(s, if tab { b"T" } else { b"W" })?;
                let title = if tab {
                    "choose tab"
                } else {
                    "choose workspace"
                };
                self.wait_text(s, title, true, "chooser open")?;
                let count = if tab {
                    s.relation(v, "fux::model::Viewing")
                        .map(|ws| w.tabs_of(ws).len())
                        .unwrap_or(1)
                } else {
                    w.workspaces.len()
                };
                for _ in 0..usize::from(entry) % count.max(1) {
                    self.send(s, b"j")?;
                }
                self.send(s, if accept { b"\r" } else { b"q" })?;
                self.wait_text(s, title, false, "chooser closed")?;
                self.finish_overlay(s, accept, "k0")?;
            }
            Keyed::Menu {
                which,
                entry,
                accept,
            } => {
                let (key, title, count): (&[u8], &str, usize) = match which % 3 {
                    0 => (b"p", "Panes:", 29),
                    1 => (b"s", "Tabs:", 5),
                    _ => (b"S", "Workspaces:", 8),
                };
                // Closing the only workspace detaches the driver by design;
                // that entry's confirmation is answered `n`.
                let accept = accept
                    && !(which % 3 == 2
                        && usize::from(entry) % count == 3
                        && w.workspaces.len() < 2);
                self.open_column(s)?;
                let Pressed::Overlay(frame) = self.press(s, key)? else {
                    return Ok(("typed".into(), json!(format!("{k:?}"))));
                };
                ensure(
                    frame.contains(title),
                    &format!("application: the menu key opened something else: {frame:?}"),
                )?;
                for _ in 0..usize::from(entry) % count {
                    self.send(s, b"j")?;
                }
                self.send(s, if accept { b"\r" } else { b"q" })?;
                self.wait_text(s, title, false, "menu closed")?;
                let n = self.next_marker;
                self.next_marker += 1;
                self.finish_overlay(s, accept, &format!("m{n}"))?;
            }
        }
        Ok(("typed".into(), json!(format!("{k:?}"))))
    }
    /// An SGR mouse event placed from the current paint.
    fn mouse(&mut self, s: &mut Server, kind: u8, at: u8) -> Result<(String, Value)> {
        let v = self.driver;
        let paint = raw_paint(s, v)?;
        let text = s.frame(v, self.rows, self.cols)?;
        let content = self.rows.saturating_sub(1);
        let panes = invariant::painted_panes(&paint, content);
        let (label, events): (&str, Vec<Vec<u8>>) = match kind % 8 {
            k @ 0..=3 => {
                let Some(p) = pick(&panes, at) else {
                    return Ok(("skip".into(), json!("no painted pane")));
                };
                // Inside the rectangle, never its first cell, so a marker
                // there is not what the click lands on.
                let x = p.1 - 1 + (u16::from(at) % (p.2 - p.1)).min(p.2 - p.1 - 1);
                let y = p.0 - 1 + (u16::from(at >> 2) % p.3).min(p.3 - 1);
                match k {
                    0 => (
                        "left click on pane",
                        vec![sgr(0, x, y, false), sgr(0, x, y, true)],
                    ),
                    1 => (
                        "right click on pane",
                        vec![sgr(2, x, y, false), sgr(2, x, y, true)],
                    ),
                    2 => ("wheel up on pane", vec![sgr(64, x, y, false)]),
                    _ => ("wheel down on pane", vec![sgr(65, x, y, false)]),
                }
            }
            4 => {
                // A tab label in the bar: the reversed active label or any
                // tab name that is painted.
                let bar = text.lines().last().unwrap_or_default();
                let y = self.rows - 1;
                let x = bar
                    .char_indices()
                    .filter(|(_, c)| !c.is_whitespace() && *c != '│')
                    .map(|(i, _)| bar.get(..i).unwrap_or_default().chars().count() as u16)
                    .nth(usize::from(at) % 6 + 1)
                    .unwrap_or(2);
                (
                    "click on the bar",
                    vec![sgr(0, x, y, false), sgr(0, x, y, true)],
                )
            }
            5 => (
                "click on the workspace name",
                vec![
                    sgr(0, 1, self.rows - 1, false),
                    sgr(0, 1, self.rows - 1, true),
                ],
            ),
            6 => {
                let mut cell = None;
                for (y, line) in text.lines().take(usize::from(content)).enumerate() {
                    if let Some(x) = line
                        .chars()
                        .position(|c| ['│', '─', '┼', '┤', '├', '┴', '┬'].contains(&c))
                    {
                        cell = Some((x as u16, y as u16));
                        break;
                    }
                }
                let Some((x, y)) = cell else {
                    return Ok(("skip".into(), json!("no separator painted")));
                };
                (
                    "click on a separator",
                    vec![sgr(0, x, y, false), sgr(0, x, y, true)],
                )
            }
            _ => {
                let (x, y) = if at.is_multiple_of(2) {
                    (self.cols - 1, self.rows - 2)
                } else {
                    (self.cols - 1, self.rows - 1)
                };
                (
                    "click on the far corner",
                    vec![
                        sgr(0, x, y, false),
                        sgr(0, x, y, true),
                        sgr(64, x, y, false),
                    ],
                )
            }
        };
        for e in &events {
            self.send(s, e)?;
        }
        self.barrier(s)?;
        Ok((
            "mouse".into(),
            json!({"what":label,"bytes":events.iter().map(|e| String::from_utf8_lossy(e).into_owned()).collect::<Vec<_>>()}),
        ))
    }
    fn resize_viewer(&mut self, s: &mut Server, i: u8) -> Result<(String, Value)> {
        const SIZES: [(u16, u16); 12] = [
            (24, 80),
            (2, 2),
            (3, 12),
            (5, 20),
            (8, 30),
            (12, 60),
            (30, 120),
            (40, 160),
            (24, 80),
            (10, 40),
            (4, 16),
            (16, 100),
        ];
        let (rows, cols) = pick(&SIZES, i).unwrap_or((24, 80));
        if (rows, cols) == (self.rows, self.cols) {
            return Ok(("skip".into(), json!("same size")));
        }
        let v = self.driver;
        s.resize(self.frontend, rows, cols)?;
        s.wait("viewer size reflected", |s| dims(s, v, rows, cols))
            .map_err(|e| format!("application: resize to {rows}x{cols} not reflected: {e}"))?;
        self.rows = rows;
        self.cols = cols;
        Ok(("resize".into(), json!({"rows":rows,"cols":cols})))
    }
    /// The 4096 clamp, through an API viewer whose oversized frame the
    /// harness never has to read.
    fn clamp_viewer(&mut self, s: &mut Server) -> Result<(String, Value)> {
        if self.api_viewers.len() >= 2 {
            return Ok(("skip".into(), json!("viewer cap")));
        }
        let id = s
            .rpc("fux.attach", json!({"rows":20,"cols":60}))?
            .get("viewer")
            .and_then(Value::as_u64)
            .ok_or("attach returned no viewer")?;
        s.rpc(
            "world.trigger_event",
            json!({"event":"fux::control::UserInput","value":{"viewer":id,"input":{"kind":"resize","rows":9000,"cols":9000}}}),
        )?;
        s.wait("oversized viewer clamped", |s| dims(s, id, 4096, 4096))
            .map_err(|e| format!("application: a 9000x9000 resize was not clamped to 4096: {e}"))?;
        // The driver's frame is built while the huge viewer exists.
        ensure(
            !raw_paint(s, self.driver)?.is_empty(),
            "application: the driver painted nothing beside a 4096x4096 viewer",
        )?;
        s.rpc(
            "world.trigger_event",
            json!({"event":"fux::control::UserInput","value":{"viewer":id,"input":{"kind":"resize","rows":20,"cols":60}}}),
        )?;
        s.wait("clamped viewer restored", |s| dims(s, id, 20, 60))?;
        self.api_viewers.push(id);
        Ok(("clamp".into(), json!(id)))
    }
    /// A pane created by a split running `program`, with its marker recorded.
    fn split_program(
        &mut self,
        s: &mut Server,
        horizontal: bool,
        program: &str,
        marker: &str,
    ) -> Result<()> {
        let v = self.driver;
        let before = s.relation(v, "fux::model::Focused").ok();
        let axis = if horizontal { "horizontal" } else { "vertical" };
        s.control(v, json!({"kind":"split","axis":axis,"program":program}))?;
        if let Ok(leaf) = s.relation(v, "fux::model::Focused")
            && Some(leaf) != before
        {
            self.markers.insert(leaf, marker.to_owned());
        }
        Ok(())
    }
    fn child_exit(&mut self, s: &mut Server, i: u8) -> Result<(String, Value)> {
        let v = self.driver;
        let code = i % 5 + 1;
        let n = self.next_marker;
        self.next_marker += 1;
        if i.is_multiple_of(2) && self.keyboard_ok() {
            // A shell-script pane that exits when `exit N` is typed into it.
            // The tty's canonical mode, not readline, reads the line, so the
            // Escapes that clear notices cannot swallow the next byte: Ctrl-U
            // kills them before the command.
            let program = format!(
                "W=WK; printf \"\\033[2J\\033[H${{W}}{n}\"; while read l; do case \"$l\" in *exit*) exit ${{l##*exit }};; esac; done"
            );
            let before = s.relation(v, "fux::model::Focused").ok();
            self.split_program(s, (i >> 1).is_multiple_of(2), &program, &format!("WK{n}"))?;
            let Some(leaf) = s
                .relation(v, "fux::model::Focused")
                .ok()
                .filter(|l| Some(*l) != before)
            else {
                // Refused, with the documented notice.
                return Ok(("child_exit".into(), json!({"code":code,"split":"refused"})));
            };
            // The reader must be running before the line is typed; other
            // panes may already have exited.
            let pane = s
                .query(invariant::PANE_VIEW)?
                .iter()
                .find(|r| id(r).ok() == Some(leaf))
                .and_then(|r| r.pointer("/components/fux::model::PaneView/pane")?.as_u64());
            s.wait("reader pane running", |s| {
                Ok(states(s)?.iter().any(|(e, st)| {
                    Some(*e) == pane && st.pointer("/status/kind") == Some(&json!("running"))
                }))
            })?;
            self.send(s, format!("\x15exit {code}\r").as_bytes())?;
        } else {
            let before = s.relation(v, "fux::model::Focused").ok();
            let program = format!("W=WK; printf \"\\033[2J\\033[H${{W}}{n}\"; exit {code}");
            self.split_program(s, i.is_multiple_of(2), &program, &format!("WK{n}"))?;
            if s.relation(v, "fux::model::Focused").ok() == before {
                return Ok(("child_exit".into(), json!({"code":code,"split":"refused"})));
            }
        }
        let Some(leaf) = s.relation(v, "fux::model::Focused").ok() else {
            return Ok(("child_exit".into(), json!({"code":code})));
        };
        let pane = s
            .query(invariant::PANE_VIEW)?
            .iter()
            .find(|r| id(r).ok() == Some(leaf))
            .and_then(|r| r.pointer("/components/fux::model::PaneView/pane")?.as_u64());
        let Some(pane) = pane else {
            return Ok(("child_exit".into(), json!({"code":code})));
        };
        s.wait("child exit published", |s| {
            Ok(states(s)?.iter().any(|(e, st)| {
                *e == pane && st.pointer("/status/code") == Some(&json!(code))
            }))
        })
        .map_err(|e| format!("application: a child that ran `exit {code}` was not published as exited {code}: {e}"))?;
        // Input into the exited pane must report, not vanish.
        key_input(s, v, "a")?;
        let mut text = String::new();
        s.wait("input into an exited pane reports", |s| {
            text = notice(s, v)?
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Ok(text.contains("exited"))
        })
        .map_err(|e| {
            format!("application: a key into an exited pane did not report; notice {text:?}: {e}")
        })?;
        Ok(("child_exit".into(), json!({"code":code,"pane":pane})))
    }
    fn child_mode(&mut self, s: &mut Server, i: u8) -> Result<(String, Value)> {
        let n = self.next_marker;
        self.next_marker += 1;
        let (what, setup) = match i % 5 {
            0 => ("alternate screen", "printf '\\033[?1049h'"),
            1 => ("mouse reporting", "printf '\\033[?1000h\\033[?1006h'"),
            2 => ("application cursor keys", "printf '\\033[?1h'"),
            3 => ("bracketed paste", "printf '\\033[?2004h'"),
            _ => ("stty rows 5 cols 20", "stty rows 5 cols 20"),
        };
        let program = format!(
            "stty raw -echo; {setup}; W=WK; printf \"\\033[2J\\033[H${{W}}{n}\"; exec cat > /dev/null"
        );
        self.split_program(s, (i >> 3).is_multiple_of(2), &program, &format!("WK{n}"))?;
        Ok((
            "child_mode".into(),
            json!({"what":what,"marker":format!("WK{n}")}),
        ))
    }
    fn config(&mut self, s: &mut Server, i: u8) -> Result<(String, Value)> {
        let watched = "watched.scn.ron";
        let (label, text, prefix, clipboard, layout) = match i % 6 {
            0 => (
                "prefix ctrl-a",
                json!({"prefix":"ctrl-a"}).to_string(),
                0x01,
                false,
                None,
            ),
            1 => (
                "clipboard write-only",
                json!({"clipboard":"write-only"}).to_string(),
                0x02,
                true,
                None,
            ),
            2 => (
                "watched layout",
                json!({"layout":watched}).to_string(),
                0x02,
                false,
                Some(watched.to_owned()),
            ),
            3 => (
                "malformed",
                "{ not valid json".to_owned(),
                self.prefix,
                false,
                None,
            ),
            4 => ("empty", "{}".to_owned(), 0x02, false, None),
            _ => (
                "all three",
                json!({"prefix":"ctrl-a","clipboard":"write-only","layout":watched}).to_string(),
                0x01,
                true,
                Some(watched.to_owned()),
            ),
        };
        let malformed = i % 6 == 3;
        // The watched file names the walk's own workspace: it is a copy of
        // the walk's last save, and its load replaces that workspace.
        let file_exists = if layout.is_some() && self.saved {
            fs::copy(s.directory.join("walk.scn.ron"), s.directory.join(watched))?;
            true
        } else {
            layout.is_some() && s.directory.join(watched).is_file()
        };
        // A changed path loads the file; a fresh copy under the same path
        // is a modification the watcher reloads. Both replace the workspace
        // asynchronously, so both are awaited before the world is judged.
        let copied = layout.is_some() && self.saved;
        let reload = layout.is_some() && (layout != self.layout || copied);
        let before_ws: Vec<u64> = s
            .query(invariant::WORKSPACE)?
            .iter()
            .map(id)
            .collect::<Result<_>>()?;
        let kept = occurrences(s, "keeping previous usable configuration")?;
        let layout_kept = occurrences(s, "keeping previous layout")?;
        let layout_failed = occurrences(s, "layout reload:")?;
        s.journal
            .record("write_config", json!({"label":label,"text":text}))?;
        fs::write(s.directory.join("fux.json"), &text)?;
        if malformed {
            s.wait("malformed configuration rejected", |s| {
                Ok(occurrences(s, "keeping previous usable configuration")? > kept)
            })
            .map_err(|e| format!("application: a malformed configuration was not reported: {e}"))?;
        } else {
            let want_prefix = if prefix == 0x01 { "ctrl-a" } else { "ctrl-b" };
            // Reflection serializes the policy by variant name.
            let want_clipboard = if clipboard { "WriteOnly" } else { "Disabled" };
            let mut seen = Value::Null;
            s.wait("configuration applied", |s| {
                seen = settings(s)?;
                Ok(
                    seen.get("prefix").and_then(Value::as_str) == Some(want_prefix)
                        && seen.get("clipboard").and_then(Value::as_str) == Some(want_clipboard)
                        && seen.get("layout").and_then(Value::as_str) == layout.as_deref(),
                )
            })
            .map_err(|e| {
                format!(
                    "application: configuration {label:?} was not applied; settings {seen}: {e}"
                )
            })?;
            self.prefix = prefix;
            if reload {
                if file_exists {
                    s.wait("watched layout applied", |s| {
                        let now: Vec<u64> = s
                            .query(invariant::WORKSPACE)?
                            .iter()
                            .map(id)
                            .collect::<Result<_>>()?;
                        Ok(now != before_ws || occurrences(s, "layout reload:")? > layout_failed)
                    })
                    .map_err(|e| {
                        format!(
                            "application: the watched layout was neither applied nor refused: {e}"
                        )
                    })?;
                } else {
                    s.wait("missing watched layout reported", |s| {
                        Ok(occurrences(s, "keeping previous layout")? > layout_kept)
                    })
                    .map_err(|e| {
                        format!("application: a missing watched layout was not reported: {e}")
                    })?;
                }
            }
            self.layout = layout;
        }
        Ok((
            "config".into(),
            json!({"label":label,"reload":reload,"file":file_exists}),
        ))
    }
    fn attach_frontend(&mut self, s: &mut Server) -> Result<(String, Value)> {
        if s.frontends.iter().filter(|f| !f.exited).count() >= 3 {
            return Ok(("skip".into(), json!("frontend cap")));
        }
        let f = s.attach(20, 60)?;
        let v = s.frontend(f)?.viewer;
        self.extra_frontends.push(f);
        Ok(("attach_frontend".into(), json!({"frontend":f,"viewer":v})))
    }
    fn hangup_frontend(&mut self, s: &mut Server) -> Result<(String, Value)> {
        let Some(f) = self.extra_frontends.pop() else {
            return Ok(("skip".into(), json!("no extra frontend")));
        };
        let v = s.frontend(f)?.viewer;
        s.signal_frontend(f, Signal::SIGHUP)?;
        s.wait("hung-up frontend exits", |s| Ok(s.frontend(f)?.exited))
            .map_err(|e| format!("application: a frontend sent SIGHUP did not exit: {e}"))?;
        ensure(
            s.frontend(f)?.exit_success,
            "application: a frontend sent SIGHUP exited abnormally",
        )?;
        ensure(
            s.frontend(f)?.terminal_restored()?,
            "application: a frontend sent SIGHUP did not restore its terminal",
        )?;
        s.wait("hung-up viewer removed", |s| {
            Ok(!s.query(VIEWER)?.iter().any(|r| id(r).ok() == Some(v)))
        })
        .map_err(|e| format!("application: the viewer of a hung-up frontend lingered: {e}"))?;
        Ok(("hangup_frontend".into(), json!({"frontend":f,"viewer":v})))
    }
    /// A step, resolved and applied. Returns the command actually sent and
    /// its JSON, for the journal.
    pub fn apply(&mut self, s: &mut Server, step: &Step, w: &World) -> Result<(String, Value)> {
        use Step::*;
        let v = self.driver;
        // Growth beyond the cap turns into a close, deterministically.
        let grows = matches!(
            step,
            Split { .. }
                | TabNew
                | WorkspaceNew
                | ChildExit(_)
                | ChildMode(_)
                | Typed(Keyed::Split { .. } | Keyed::TabNew | Keyed::WorkspaceNew)
        );
        let step = if w.states.len() >= self.cap && grows {
            &ClosePane(0)
        } else {
            step
        };
        // Keyboard steps need a viewer large enough for the column.
        let step = match step {
            Typed(_)
            | LoneEscape { .. }
            | DoublePrefix
            | UnknownPrefixKey
            | PasteInColumn
            | Mouse { .. }
                if !self.keyboard_ok() =>
            {
                &Key(0)
            }
            other => other,
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
                let sink = if self.capture {
                    format!("pane-WK{n}.bin")
                } else {
                    "/dev/null".to_owned()
                };
                let program = format!(
                    "stty raw -echo; W=WK; printf \"\\033[2J\\033[H${{W}}{n}\"; exec cat > {sink}"
                );
                self.split_program(s, *horizontal, &program, &format!("WK{n}"))?;
                return Ok((
                    "split".into(),
                    json!({"horizontal":horizontal,"marker":format!("WK{n}")}),
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
                let name = if i % 4 == 3 {
                    format!("界{i}")
                } else {
                    format!("n{i}")
                };
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
            CopyMode => {
                self.copy_mode = true;
                (v, json!({"kind":"copy_mode"}))
            }
            LeaveCopyMode => {
                key_input(s, v, "q")?;
                self.copy_mode = false;
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
            Help => {
                self.overlay_open = true;
                (v, json!({"kind":"help"}))
            }
            Menu => match focused {
                Some(p) => {
                    self.overlay_open = true;
                    (v, json!({"kind":"menu","subject":{"pane":p}}))
                }
                None => return Ok(("skip".into(), json!("no pane for menu"))),
            },
            Choose => {
                self.overlay_open = true;
                (v, json!({"kind":"choose","chooser":"tab"}))
            }
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
            Typed(k) => return self.typed(s, *k, w),
            LoneEscape { inside } => {
                if *inside {
                    self.open_column(s)?;
                    self.escape(s)?;
                    self.wait_panel(s, false, "a lone Escape closed the column")?;
                } else {
                    self.escape(s)?;
                    self.barrier(s)?;
                }
                return Ok(("escape".into(), json!({"inside":inside})));
            }
            DoublePrefix => {
                self.open_column(s)?;
                self.send(s, &[self.prefix])?;
                self.wait_panel(s, false, "the doubled prefix closed the column")?;
                return Ok(("double_prefix".into(), json!(self.prefix)));
            }
            UnknownPrefixKey => {
                self.open_column(s)?;
                self.send(s, b"\x1b[24~")?;
                let mut text = String::new();
                s.wait("unbound key reported", |s| {
                    text = notice(s, v)?
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    Ok(text.contains("unbound"))
                })
                .map_err(|e| format!("application: F12 under the prefix was not reported as unbound; notice {text:?}: {e}"))?;
                ensure(
                    panel_painted(s, v)?,
                    "application: an unknown shortcut closed the command column",
                )?;
                self.escape(s)?;
                self.wait_panel(s, false, "escape after an unbound key")?;
                return Ok(("unknown_prefix_key".into(), json!(text)));
            }
            PasteInColumn => {
                let (panes, tabs) = (w.views.len(), w.tabs.len());
                self.open_column(s)?;
                self.send(s, b"\x1b[200~hvxt\x1b[201~")?;
                self.escape(s)?;
                self.wait_panel(s, false, "escape after a paste in the column")?;
                let after = World::read(s)?;
                ensure(
                    after.views.len() == panes && after.tabs.len() == tabs,
                    &format!(
                        "application: a paste in the command column executed shortcuts: {panes} panes, {tabs} tabs became {}, {}",
                        after.views.len(),
                        after.tabs.len()
                    ),
                )?;
                return Ok(("paste_in_column".into(), json!("hvxt")));
            }
            Mouse { kind, at } => return self.mouse(s, *kind, *at),
            ViewerResize(i) => return self.resize_viewer(s, *i),
            ClampViewer => return self.clamp_viewer(s),
            ChildExit(i) => return self.child_exit(s, *i),
            ChildMode(i) => return self.child_mode(s, *i),
            Config(i) => return self.config(s, *i),
            AttachFrontend => return self.attach_frontend(s),
            HangupFrontend => return self.hangup_frontend(s),
            Raw { kind, index } => return super::raw::mutate(s, self, *kind, *index, w),
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
    pub fn clear_notice(&mut self, s: &mut Server) -> Result<()> {
        self.overlay_open = false;
        self.copy_mode = false;
        key_input(s, self.driver, "escape")
    }
    /// The driver's frontend shows exactly the server's frame once a step
    /// settles. Returns the first differing line on timeout.
    pub fn converged(&self, s: &mut Server) -> Result<Option<String>> {
        if s.frontend(self.frontend)?.exited {
            return Ok(Some("frontend exited".into()));
        }
        let (v, f, rows, cols) = (self.driver, self.frontend, self.rows, self.cols);
        let mut server_view = String::new();
        let mut front_view = String::new();
        let ok = s
            .wait("frontend converges with the server frame", |s| {
                server_view = s.frame(v, rows, cols)?;
                front_view = screen(s, f)?;
                Ok(server_view == front_view)
            })
            .is_ok();
        if ok {
            return Ok(None);
        }
        Ok(Some(
            server_view
                .lines()
                .zip(front_view.lines())
                .enumerate()
                .find(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| format!("line {i}: server {a:?} frontend {b:?}"))
                .unwrap_or_else(|| "line counts differ".into()),
        ))
    }
}

/// One step with full checking; the shared engine for the walk scenarios.
pub(super) fn step(
    s: &mut Server,
    walker: &mut Walker,
    index: usize,
    st: &Step,
    others_with_overlay: &[u64],
) -> Result<()> {
    walker.clear_notice(s)?;
    let before = World::read(s)?;
    let notice_before = notice(s, walker.driver)?;
    let panics_before = occurrences(s, "panicked")?;
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
    let mut skip: Vec<u64> = others_with_overlay.to_vec();
    if overlay || walker.overlay_open {
        skip.push(walker.driver);
    }
    let markers = walker.markers.clone();
    let marker_of = |leaf: u64| markers.get(&leaf).cloned();
    let mut problems = match walker.oracle {
        Oracle::Full => {
            let mut p = invariant::violations(s, &after, walker.cap, &skip)?;
            if !overlay && !walker.overlay_open {
                p.extend(invariant::markers_visible(
                    s,
                    &after,
                    walker.driver,
                    &marker_of,
                )?);
                // What is painted, for every viewer that is not mid-overlay.
                for viewer in &after.viewers {
                    if !skip.contains(viewer) {
                        p.extend(invariant::presentation(s, &after, *viewer, &marker_of)?);
                    }
                }
            }
            p
        }
        Oracle::Narrow => invariant::narrow(s, &after)?,
    };
    // The frontend shows what the server frames.
    if let Some(diff) = walker.converged(s)? {
        problems.push(format!(
            "frontend did not converge with the server frame: {diff}"
        ));
    }
    // No overlay is open unless this step opened one.
    if !overlay && !walker.overlay_open && panel_painted(s, walker.driver)? {
        problems.push("an overlay is open although the step closed or opened none".into());
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
    if occurrences(s, "panicked")? > panics_before {
        problems.push("server stderr reports a panic".into());
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
    // Thousands of steps: the per-step summary is the evidence; a failure
    // records its command and notice in full, and the minimizer reproduces it.
    s.quiet = true;
    s.journal
        .record("walk_begin", json!({"seed":seed,"steps":steps.len()}))?;
    let mut sources = (0usize, 0usize);
    for (i, st) in steps.iter().enumerate() {
        step(s, &mut walker, i, st, &[])?;
        match st {
            Step::Typed(_)
            | Step::LoneEscape { .. }
            | Step::DoublePrefix
            | Step::UnknownPrefixKey
            | Step::PasteInColumn
            | Step::Mouse { .. } => sources.0 += 1,
            _ => sources.1 += 1,
        }
    }
    let processes = states(s)?.len();
    s.journal.record(
        "walk_end",
        json!({"seed":seed,"steps":steps.len(),"frontend_steps":sources.0,"other_steps":sources.1,"processes":processes}),
    )?;
    println!(
        "WALK-SOURCES seed {seed}: {} frontend steps, {} other steps",
        sources.0, sources.1
    );
    Ok(())
}

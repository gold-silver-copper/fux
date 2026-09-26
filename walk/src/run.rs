//! Taking a step, waiting until it has settled, and checking everything
//! that must hold after it.
use crate::fixture::{CONFIG_VALID, Fixture, after};
use crate::notices::Notices;
use crate::step::{Act, Config, Step, Stop};
use crate::world::{World, alive};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How a walk stopped short of its steps.
#[derive(Debug)]
pub enum End {
    /// An invariant failed after step `at`.
    Failed(Failure),
    /// The last pane closed, and the server stopped with it, as documented.
    ServerDone,
}

#[derive(Debug, Clone)]
pub struct Failure {
    /// The step after which it failed, counted from 0.
    pub at: usize,
    /// Which invariant: the same one is what minimizing keeps.
    pub invariant: String,
    pub detail: String,
}

/// What checking remembers between steps.
pub struct Checker {
    notices: Notices,
    /// Pane processes whose pane is gone, and since when.
    gone: BTreeMap<i32, (String, Instant)>,
    /// Every pane process seen, by pane.
    pids: BTreeMap<String, i32>,
    /// How long a step may take to settle.
    pub settle: Duration,
}

/// How long a pane's process may outlive its pane: the hang-up grace, and
/// room for a loaded machine.
const GRACE: Duration = Duration::from_secs(3);

/// Text overlays show, to tell whether one is open.
const OVERLAY_MARKS: &[&str] = &[
    "Enter selects",
    "Enter accepts",
    "? (y/n)",
    "Commands",
    "C-b t:",
    "C-b w:",
];

impl Checker {
    pub fn new(settle: Duration) -> Checker {
        Checker {
            notices: Notices::from_source(),
            gone: BTreeMap::new(),
            pids: BTreeMap::new(),
            settle,
        }
    }
}

fn fail(at: usize, invariant: &str, detail: impl Into<String>) -> End {
    End::Failed(Failure {
        at,
        invariant: invariant.to_owned(),
        detail: detail.into(),
    })
}

/// Takes `step`. A step that names a client or pane that is not there does
/// nothing: in a minimized trace, what made it may be gone.
pub fn take(fixture: &mut Fixture, step: &Step, at: usize) -> Result<(), End> {
    match step {
        Step::Keys { client, bytes } => {
            if let Some(c) = fixture.clients.get_mut(client) {
                c.type_bytes(bytes)
                    .map_err(|e| fail(at, "typing", format!("{client}: {e}")))?;
            }
        }
        Step::Cli(args) => {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            fixture
                .fux(&args)
                .map_err(|e| fail(at, "the server answers", e))?;
        }
        Step::Resize { client, rows, cols } => {
            if let Some(c) = fixture.clients.get_mut(client) {
                c.resize(*rows, *cols)
                    .map_err(|e| fail(at, "resizing", format!("{client}: {e}")))?;
            }
        }
        Step::Attach { rows, cols } => {
            fixture
                .attach(*rows, *cols)
                .map_err(|e| fail(at, "attaching", e))?;
        }
        Step::Hangup { client } => {
            if let Some(c) = fixture.clients.get_mut(client) {
                c.hang_up();
            }
        }
        Step::Child { pane, act } => {
            let serial = fixture.serial();
            let script = fixture.dir.join("act.sh");
            let args = match act {
                Act::Alternate => "alternate".to_owned(),
                Act::Mouse => "mouse".to_owned(),
                Act::Bracketed => "bracketed".to_owned(),
                Act::AppCursor => "appcursor".to_owned(),
                Act::Stty(r, c) => format!("stty {r} {c}"),
                Act::Burst(n) => format!("burst {n}"),
                Act::Deaf(s) => format!("deaf {s}"),
            };
            let line = format!("sh {} {args} {serial}", script.display());
            let _ = fixture.fux(&["send-keys", "-t", pane, "-l", &line]);
            let _ = fixture.fux(&["send-keys", "-t", pane, "Enter"]);
            // Done when its marker shows, if the pane shows it at all: it
            // may be busy, stopped, or in the alternate screen.
            let marker = format!("act-done-{}", serial.saturating_add(1000));
            let wait = match act {
                Act::Deaf(s) => Duration::from_secs(u64::from(*s).saturating_add(3)),
                Act::Alternate
                | Act::Mouse
                | Act::Bracketed
                | Act::AppCursor
                | Act::Stty(..)
                | Act::Burst(_) => Duration::from_secs(3),
            };
            let deadline = after(wait);
            while Instant::now() < deadline {
                let shown = fixture
                    .fux(&["capture-pane", "-t", pane, "-S", "-500"])
                    .map(|o| o.stdout.contains(&marker))
                    .unwrap_or(false);
                if shown {
                    break;
                }
                for c in fixture.clients.values_mut() {
                    c.pump();
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        Step::Exit { pane, code } => {
            let _ = fixture.fux(&["send-keys", "-t", pane, "-l", &format!("exit {code}")]);
            let _ = fixture.fux(&["send-keys", "-t", pane, "Enter"]);
        }
        Step::Signal { pane, stop } => {
            let world = World::read(fixture).map_err(|e| fail(at, "the server answers", e))?;
            if let Some(pid) = world
                .pane(pane)
                .and_then(|p| fuxix::process::Pid::from_raw(p.pid))
            {
                let signal = match stop {
                    Stop::Stop => fuxix::process::Signal::Stop,
                    Stop::Cont => fuxix::process::Signal::Cont,
                };
                let _ = fuxix::process::kill(pid, signal);
            }
        }
        Step::Config(config) => {
            let text = match config {
                Config::Valid => CONFIG_VALID.to_owned(),
                Config::Invalid => format!("{CONFIG_VALID}bind C-Left resize-pane -L\n"),
                Config::PrefixA => format!("{CONFIG_VALID}set prefix C-a\n"),
                Config::PrefixB => format!("{CONFIG_VALID}set prefix C-b\n"),
            };
            std::fs::write(&fixture.config, text)
                .map_err(|e| fail(at, "writing the configuration", e.to_string()))?;
            let _ = fixture.fux(&["reload"]);
        }
        Step::Clamp => {
            let (rows, cols) = fixture.clamped().map_err(|e| fail(at, "attaching", e))?;
            if (rows, cols) != (4096, 4096) {
                return Err(fail(
                    at,
                    "a size is clamped to 4096",
                    format!("9000 by 9000 became {rows} by {cols}"),
                ));
            }
        }
    }
    Ok(())
}

/// Whether a screen shows an overlay.
fn overlay(lines: &[String]) -> bool {
    let text = lines.join("\n");
    OVERLAY_MARKS.iter().any(|m| text.contains(m))
}

/// The error notice a client's bar shows, if one does: the run of cells
/// in fux's error colour.
fn error_notice(client: &crate::fixture::Client) -> Option<String> {
    let screen = client.screen.screen();
    let window = screen.window(0, client.rows, client.cols);
    let bar = window.row(client.rows.saturating_sub(1))?;
    let text: String = bar
        .cells
        .iter()
        .filter(|c| c.fgcolor() == fux_vt::Color::Idx(9) && !c.is_wide_continuation())
        .map(|c| if c.has_contents() { c.contents() } else { " " })
        .collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// What a client shows before a step, for the overlay check.
pub fn overlays(fixture: &mut Fixture) -> BTreeMap<String, bool> {
    fixture
        .clients
        .iter_mut()
        .map(|(id, c)| {
            c.pump();
            (id.clone(), overlay(&c.lines()))
        })
        .collect()
}

impl Checker {
    /// Waits for `step` to settle, then checks every invariant.
    pub fn check(
        &mut self,
        fixture: &mut Fixture,
        step: &Step,
        at: usize,
        before: &BTreeMap<String, bool>,
    ) -> Result<(), End> {
        // Clients that went: detached, hung up, or told to exit.
        let went: Vec<String> = fixture
            .clients
            .iter_mut()
            .filter_map(|(id, c)| c.gone().then(|| id.clone()))
            .collect();
        for id in went {
            fixture.clients.remove(&id);
        }
        if !fixture.server_alive() {
            let log = fixture.log();
            if log.contains("the last pane closed") {
                return Err(End::ServerDone);
            }
            return Err(fail(
                at,
                "the server runs",
                format!("it stopped:\n{}", tail(&log)),
            ));
        }
        let log = fixture.log();
        if log.contains("panicked") {
            return Err(fail(at, "the server does not panic", tail(&log)));
        }
        let world = World::read(fixture).map_err(|e| fail(at, "the server answers", e))?;
        self.structure(&world, at)?;
        self.processes(&world, at)?;
        self.sizes(&world, at)?;
        self.frames(fixture, &world, at)?;
        // Keys and commands open overlays by design. A resize only shows or
        // hides what is open: one drawn nowhere on a tiny screen appears
        // when it grows, so the screen before says nothing.
        if !step.types() && !matches!(step, Step::Cli(_) | Step::Resize { .. }) {
            for (id, c) in &fixture.clients {
                if before.get(id) == Some(&false) && overlay(&c.lines()) {
                    return Err(fail(
                        at,
                        "no overlay opens unless a step opens one",
                        format!("{id} shows:\n{}", c.lines().join("\n")),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Ids are unique; every pane is in exactly one tab; every client's
    /// workspace, tab and pane exist and belong together.
    fn structure(&self, world: &World, at: usize) -> Result<(), End> {
        let mut seen = std::collections::BTreeSet::new();
        for id in world
            .workspaces
            .iter()
            .map(|w| &w.id)
            .chain(world.tabs().map(|t| &t.id))
            .chain(world.panes().map(|p| &p.id))
            .chain(world.clients.iter().map(|c| &c.id))
        {
            if !seen.insert(id.clone()) {
                return Err(fail(at, "ids are unique", format!("{id} is listed twice")));
            }
        }
        for c in &world.clients {
            let ws = world.workspace(&c.workspace).ok_or_else(|| {
                fail(
                    at,
                    "a client's workspace exists",
                    format!("{} is on {}", c.id, c.workspace),
                )
            })?;
            match (&c.tab, &c.pane) {
                (Some(tab), pane) => {
                    let t = ws.tabs.iter().find(|t| &t.id == tab).ok_or_else(|| {
                        fail(
                            at,
                            "a client's tab is in its workspace",
                            format!("{} shows {tab}, not in {}", c.id, ws.id),
                        )
                    })?;
                    match pane {
                        Some(pane) if !t.panes.iter().any(|p| &p.id == pane) => {
                            return Err(fail(
                                at,
                                "a client's pane is in its tab",
                                format!("{} focuses {pane}, not in {tab}", c.id),
                            ));
                        }
                        None if !t.panes.is_empty() => {
                            return Err(fail(
                                at,
                                "a client focuses a pane of a tab that has one",
                                format!("{} focuses nothing in {tab}", c.id),
                            ));
                        }
                        Some(_) | None => {}
                    }
                }
                (None, _) if !ws.tabs.is_empty() => {
                    return Err(fail(
                        at,
                        "a client shows a tab",
                        format!("{} shows no tab of {}", c.id, ws.id),
                    ));
                }
                (None, _) => {}
            }
        }
        Ok(())
    }

    /// Every listed pane's process is alive (stopped counts), and none
    /// outlives its pane past the hang-up grace.
    fn processes(&mut self, world: &World, at: usize) -> Result<(), End> {
        let now = Instant::now();
        for pane in world.panes() {
            if let Some(pid) = fuxix::process::Pid::from_raw(pane.pid) {
                self.pids.insert(pane.id.clone(), pane.pid);
                // A pane whose shell just exited is listed until its exit
                // is read; it may be dead but not yet reaped.
                let _ = alive(pid);
            }
        }
        let listed: Vec<&String> = world.panes().map(|p| &p.id).collect();
        let closed: Vec<(String, i32)> = self
            .pids
            .iter()
            .filter(|(id, _)| !listed.contains(id))
            .map(|(id, pid)| (id.clone(), *pid))
            .collect();
        for (id, pid) in closed {
            self.pids.remove(&id);
            self.gone.insert(pid, (id, now));
        }
        let late: Vec<(i32, String)> = self
            .gone
            .iter()
            .filter(|(_, (_, since))| now.duration_since(*since) > GRACE)
            .map(|(pid, (id, _))| (*pid, id.clone()))
            .collect();
        for (pid, id) in late {
            self.gone.remove(&pid);
            if fuxix::process::Pid::from_raw(pid).is_some_and(alive) {
                return Err(fail(
                    at,
                    "a pane's process ends with its pane",
                    format!("{id}'s process {pid} is still running"),
                ));
            }
        }
        Ok(())
    }

    /// A pane a client shows is no bigger than that client's pane area:
    /// a PTY is the smallest rectangle among the clients showing it.
    ///
    /// Narrowed: a pane the layout cannot fit is hidden and keeps its size,
    /// as the README says, and `ls` does not say which panes are placed. So
    /// only panes that surely are: the one pane of a tab, and a zoomed
    /// client's focused pane, each of which fills the client's pane area.
    fn sizes(&self, world: &World, at: usize) -> Result<(), End> {
        for c in &world.clients {
            let Some(tab) = c.tab.as_ref().and_then(|t| world.tab(t)) else {
                continue;
            };
            // Under two rows there is no room for a pane beside the bar.
            if c.rows < 2 {
                continue;
            }
            let (rows, cols) = (c.rows.saturating_sub(1), c.cols);
            let shown: Vec<&crate::world::Pane> = match (tab.panes.as_slice(), c.zoom) {
                ([only], _) => vec![only],
                (panes, true) => panes
                    .iter()
                    .filter(|p| c.pane.as_ref() == Some(&p.id))
                    .collect(),
                (_, false) => Vec::new(),
            };
            for p in shown {
                if p.rows > rows || p.cols > cols {
                    return Err(fail(
                        at,
                        "a pane fits every client that shows it",
                        format!(
                            "{} is {}x{} but {} has room for {cols}x{rows}",
                            p.id, p.cols, p.rows, c.id
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Every client's terminal shows what the server composes for it, and
    /// what it shows is painted properly.
    fn frames(&self, fixture: &mut Fixture, world: &World, at: usize) -> Result<(), End> {
        let ids: Vec<String> = fixture.clients.keys().cloned().collect();
        for id in ids {
            if !world.clients.iter().any(|c| c.id == id) {
                continue;
            }
            let deadline = after(self.settle);
            let mut captured: Vec<String>;
            loop {
                let out = fixture
                    .fux(&["capture-client", "-c", &id])
                    .map_err(|e| fail(at, "the server answers", e))?;
                if out.status != 0 {
                    // It went while we looked.
                    captured = Vec::new();
                    break;
                }
                captured = out.stdout.lines().map(str::to_owned).collect();
                let Some(client) = fixture.clients.get_mut(&id) else {
                    break;
                };
                client.pump();
                if client.lines() == captured {
                    break;
                }
                if Instant::now() > deadline {
                    return Err(fail(
                        at,
                        "a client shows what the server composes",
                        format!(
                            "{id} shows:\n{}\n\nthe server composes:\n{}",
                            client.lines().join("\n"),
                            captured.join("\n")
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let Some(client) = fixture.clients.get(&id) else {
                continue;
            };
            if captured.is_empty() {
                continue;
            }
            self.painted(client, &captured, world, at)?;
        }
        Ok(())
    }

    fn painted(
        &self,
        client: &crate::fixture::Client,
        rows: &[String],
        world: &World,
        at: usize,
    ) -> Result<(), End> {
        let id = &client.id;
        if rows.len() != usize::from(client.rows) {
            return Err(fail(
                at,
                "a screen is the client's size",
                format!("{id} has {} rows, and {} captured", client.rows, rows.len()),
            ));
        }
        if let Some(wide) = rows.iter().find(|r| fux::render::width(r) > client.cols) {
            return Err(fail(
                at,
                "nothing paints outside the screen",
                format!("{id} is {} wide: {wide:?}", client.cols),
            ));
        }
        let screen = client.screen.screen();
        let window = screen.window(0, client.rows, client.cols);
        for y in 0..client.rows {
            let last = window
                .row(y)
                .and_then(|r| r.cells.get(usize::from(client.cols.saturating_sub(1))));
            if last.is_some_and(|c| c.is_wide()) {
                return Err(fail(
                    at,
                    "no wide glyph starts in the last column",
                    format!("{id}, row {y}"),
                ));
            }
        }
        let view = world.clients.iter().find(|c| &c.id == id);
        let bar = rows.last().cloned().unwrap_or_default();
        if let Some(view) = view
            && client.cols >= 60
            && !bar.starts_with(" COPY")
            && !bar.starts_with(" /")
            && !bar.starts_with(" ?")
        {
            // A notice may take three quarters of the bar: names are
            // checked only where the quarter left has room for them.
            let room = usize::from(client.cols) / 4;
            let ws = world.workspace(&view.workspace);
            if let Some(ws) = ws
                && ws.name.len().saturating_add(2) <= room
                && !bar.starts_with(&format!(" {} ", ws.name))
            {
                return Err(fail(
                    at,
                    "the bar names the client's workspace",
                    format!("{id} is on {} ({}); the bar is {bar:?}", ws.id, ws.name),
                ));
            }
            let tab = view.tab.as_ref().and_then(|t| world.tab(t));
            let names: usize = ws
                .map(|w| {
                    w.tabs
                        .iter()
                        .map(|t| t.name.len().saturating_add(2))
                        .sum::<usize>()
                        .saturating_add(w.name.len().saturating_add(2))
                })
                .unwrap_or(usize::MAX);
            if let Some(tab) = tab
                && names <= room
                && !bar.contains(&format!(" {} ", tab.name))
            {
                return Err(fail(
                    at,
                    "the bar names the client's tab",
                    format!("{id} shows {} ({}); the bar is {bar:?}", tab.id, tab.name),
                ));
            }
            // The right of the bar says `[zoom]` unless a notice, a
            // layer's "…" or a repeat mode's keys have it, or it is cut.
            let right_taken = has_notice(client) || bar.ends_with('…') || bar.contains("· Esc");
            if view.zoom && !right_taken && !bar.contains("[zoom]") {
                return Err(fail(
                    at,
                    "a zoomed client says so",
                    format!("{id}'s bar is {bar:?}"),
                ));
            }
        }
        // A notice cut to a few characters on a tiny screen says nothing.
        if let Some(notice) = error_notice(client)
            && !(notice.ends_with('…') && notice.chars().count() < 4)
            && !self.notices.known(&notice)
        {
            return Err(fail(
                at,
                "an error notice is one fux's source has",
                format!("{id} shows {notice:?}"),
            ));
        }
        Ok(())
    }
}

/// Whether a client's bar shows any notice, error or not.
fn has_notice(client: &crate::fixture::Client) -> bool {
    let screen = client.screen.screen();
    let window = screen.window(0, client.rows, client.cols);
    window
        .row(client.rows.saturating_sub(1))
        .is_some_and(|bar| {
            bar.cells
                .iter()
                .any(|c| matches!(c.fgcolor(), fux_vt::Color::Idx(9 | 11)) && c.has_contents())
        })
}

/// The end of a log.
pub fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(40);
    lines.get(from..).unwrap_or_default().join("\n")
}

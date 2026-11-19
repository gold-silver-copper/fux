//! fux3-view: shows a fux3 session's panes as fux does, split, in this
//! terminal, through their files alone, and sends keys to the focused one.
//! It owns nothing: end it however you like and every pane goes on.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("fux3-view runs on Linux and macOS only");

mod command;
mod keys;
mod layout;
mod panel;
mod panes;
mod pictures;
mod render;
mod script;

use fux_vt::keys::Keystroke;
use fux_vt::keys::decode::{Decoder, Input, Reply, Timing};
use fux_vt::keys::mouse::{MouseAction, MouseButton, MouseEvent};
use fux3::Failure;
use fux3::session::Session;
use fuxix::Errno;
use fuxix::poll::{Events, Poke, Poll, PollFd, Wake, waker};
use fuxix::process::Signal;
use keys::{Action, Keys};
use layout::{Dir, Layout, Live, PaneRef, Rect, Tab as LayoutTab, Target, Workspace};
use panel::{Column, Outcome, Panel};
use panes::Pane;
use render::{Frame, Tile};
use script::{Context, Job, Run, Script, ToViewer, Verb};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::{Duration, Instant};

/// How often the session directory is looked at again.
const RESCAN: Duration = Duration::from_secs(1);
/// How often the files of the panes shown are looked at.
const LOOK: Duration = Duration::from_millis(20);
/// How soon keys a pane's `in` would not take are tried again.
const RETRY_KEYS: Duration = Duration::from_millis(10);
/// The least time between paints.
const PAINT: Duration = Duration::from_millis(16);
/// The bar's colours, as fux's: the bar, the tab shown, the keys typed, a
/// notice and an error.
const BAR: &str = ";38;5;250;48;5;236";
const BAR_BOLD: &str = ";1;38;5;250;48;5;236";
const TAB: &str = ";1;30;42";
const KEYS: &str = ";1;30;103";
const NOTICE: &str = ";93;48;5;236";
/// What an empty tab says, in the borders' colour.
const BORDER_TEXT: &str = render::BORDER;
const ERROR: &str = ";91;48;5;236";
/// How long past its time a stopped binding gets before it is forced.
const FORCE_AFTER: Duration = Duration::from_secs(1);
/// How long leaving waits for the terminal to take the last bytes.
const LEAVE_WAIT: Duration = Duration::from_millis(500);
/// Bindings waiting behind the one running.
const MAX_WAITING_RUNS: usize = 16;
/// How long leaving waits for a binding to be stopped.
const END_RUN_WAIT: Duration = Duration::from_secs(1);
/// How many messages from the language are taken in one turn.
const SCRIPT_TURN: usize = 64;
/// The longest paste passed on.
const MAX_PASTE: usize = 1 << 20;
/// The longest line `.view/ctl` takes, and the most refusals `.view/errors`
/// keeps.
const MAX_CTL_LINE: usize = fux3::files::PIPE_BUF;
const MAX_ERRORS: usize = 32;
/// How long leaving waits for pastes to be taken.
const PASTE_LEAVE_WAIT: Duration = Duration::from_secs(1);
/// Escape's wait for the rest of a sequence, and a paste's, in ms.
const TIMING: Timing = Timing::new(50, 1000);

/// The alternate screen, pastes bracketed, the cursor hidden, and the mouse
/// reported (buttons and drags, SGR encoding); leaving undoes them.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?2004h\x1b[?25l\x1b[?1002h\x1b[?1006h\x1b[2J\x1b[c";
const LEAVE: &str = "\x1b[0m\x1b[0 q\x1b[?1006l\x1b[?1002l\x1b[?25h\x1b[?2004l\x1b[?1049l";

fn after(now: Instant, wait: Duration) -> Instant {
    now.checked_add(wait).unwrap_or(now)
}

fn io_failure(what: &str, error: impl std::fmt::Display) -> Failure {
    fux3::io(what, error)
}

/// Writes frames on a thread of its own, so that a terminal that stopped
/// reading holds up no key: one frame at a time, the next made once it is
/// out.
struct Painter {
    frames: SyncSender<Vec<u8>>,
    busy: Arc<AtomicBool>,
}

impl Painter {
    fn start(poke: Poke) -> std::io::Result<Painter> {
        let (frames, incoming) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        let busy = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&busy);
        std::thread::Builder::new()
            .name("fux3-paint".to_owned())
            .spawn(move || {
                let mut out = std::io::stdout();
                while let Ok(frame) = incoming.recv() {
                    if out.write_all(&frame).and_then(|()| out.flush()).is_err() {
                        return;
                    }
                    done.store(false, Ordering::SeqCst);
                    poke.poke();
                }
            })?;
        Ok(Painter { frames, busy })
    }

    fn is_idle(&self) -> bool {
        !self.busy.load(Ordering::SeqCst)
    }

    /// Hands over a frame, unless one is being written.
    fn send(&self, bytes: Vec<u8>) -> bool {
        if !self.is_idle() {
            return false;
        }
        // Busy before the writer can see the frame, so that its "done"
        // always comes after.
        self.busy.store(true, Ordering::SeqCst);
        let sent = self.frames.try_send(bytes).is_ok();
        if !sent {
            self.busy.store(false, Ordering::SeqCst);
        }
        sent
    }

    /// Hands over the last bytes once the frame in progress is out, and
    /// waits for them to be written, each wait bounded.
    fn finish(&self, bytes: Vec<u8>) {
        let wait = || {
            let until = after(Instant::now(), LEAVE_WAIT);
            while !self.is_idle() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        wait();
        if self.send(bytes) {
            wait();
        }
    }
}

/// The binding running now.
struct Running {
    stop: grift::Interrupter,
    at: Instant,
    forced: bool,
}

/// A border being dragged: where the press was, and the layout version it
/// started from.
struct Drag {
    border: layout::Border,
    from: (u16, u16),
    to: (u16, u16),
    version: u64,
}

struct View {
    session: Session,
    /// The session's `.view`: the layout, its lock, `ctl` and `errors`.
    view: PathBuf,
    /// The last good layout read, and the text it came from.
    stored: Layout,
    layout_text: Option<String>,
    /// What the layout file's last refused reading said.
    layout_failed: Option<String>,
    /// A good layout file has been read: one gone since is refused.
    layout_seen: bool,
    /// What is shown: the stored layout with routed panes, derived.
    arranged: Arc<Layout>,
    /// The live panes `arranged` was made with.
    live: Vec<Live>,
    /// The workspace shown, and the one before it, by id.
    workspace: u32,
    /// The tab each workspace shows.
    places: BTreeMap<u32, u32>,
    panes: BTreeMap<String, Pane>,
    focus: Option<PaneRef>,
    /// The pane focused before, for `last-pane`.
    last_focus: Option<PaneRef>,
    term: (u16, u16),
    keys: Keys,
    /// The keys typed so far of a chord: their menu is shown.
    column: Option<Column>,
    /// The group whose keys act again without the keys before them.
    panel: Option<Panel>,
    drag: Option<Drag>,
    /// A notice for the bar's corner, until the next key: its text and
    /// colours.
    notice: Option<(String, &'static str)>,
    /// `.view/ctl`, read by every viewer, and a line not yet whole.
    ctl: Option<File>,
    ctl_part: Vec<u8>,
    /// What the run in flight was started for: a `.view/ctl` line's
    /// refusal goes to `.view/errors`.
    run_label: String,
    /// Pastes handed over, to name their files.
    pasted: u64,
    script: Option<Script>,
    from_script: Receiver<ToViewer>,
    running: Option<Running>,
    waiting_runs: VecDeque<Run>,
    /// The keys are still loading (each part in its time): keys typed
    /// wait in the terminal until then, so none goes to a pane it was not
    /// meant for.
    loading: bool,
    /// A run was handed to the language and has not finished.
    run_sent: bool,
    painter: Painter,
    drawn: Frame,
    /// The terminal draws sixel (its DA1 said), and the pictures drawn.
    sixel: bool,
    pictures: Vec<pictures::Drawn>,
    dirty: bool,
    next_paint: Instant,
    next_scan: Instant,
    next_look: Instant,
    quit: bool,
}

/// `index` moved by one through `len` places, wrapping.
fn stepped(index: usize, len: usize, forward: bool) -> usize {
    let last = len.saturating_sub(1);
    match (forward, index) {
        (true, i) if i >= last => 0,
        (true, i) => i.saturating_add(1),
        (false, 0) => last,
        (false, i) => i.saturating_sub(1),
    }
}

fn tile_of(rect: Rect) -> Tile {
    let small = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
    Tile {
        x: small(rect.x),
        y: small(rect.y),
        w: small(rect.w),
        h: small(rect.h),
    }
}

impl View {
    /// An error, in the bar's corner until the next key.
    fn say(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), ERROR));
        self.dirty = true;
    }

    /// A notice that is no error.
    fn note(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), NOTICE));
        self.dirty = true;
    }

    /// The room tiles share: the whole screen. The bar, when it shows, is
    /// drawn over its last row and takes no room.
    fn area(&self) -> Rect {
        let (cols, rows) = self.term;
        Rect {
            x: 0,
            y: 0,
            w: u32::from(cols.min(render::MAX_COLS)),
            h: u32::from(rows.min(render::MAX_ROWS)),
        }
    }

    fn space(&self) -> Option<&Workspace> {
        (self.arranged.workspaces.iter()).find(|space| space.id == self.workspace)
    }

    /// The tab the workspace shown shows.
    fn place(&self) -> u32 {
        self.places
            .get(&self.workspace)
            .copied()
            .unwrap_or_default()
    }

    fn shown_tab(&self) -> Option<&LayoutTab> {
        let tab = self.place();
        self.space()?.tabs.iter().find(|t| t.id == tab)
    }

    /// The tiles drawn: those of the shown tab with room for a cell.
    fn tiles(&self) -> Vec<(PaneRef, Rect)> {
        let area = self.area();
        (self.shown_tab())
            .map(|tab| tab.tiles(area))
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, rect)| rect.w > 0 && rect.h > 0)
            .collect()
    }

    fn visible(&self) -> Vec<PaneRef> {
        self.tiles().into_iter().map(|(pane, _)| pane).collect()
    }

    /// The pane a tile shows, while that life of it has a directory.
    fn live_pane(&self, pane: &PaneRef) -> Option<&Pane> {
        (self.panes.get(&pane.name)).filter(|p| p.id() == Some(pane.id.as_str()))
    }

    fn live_pane_mut(&mut self, pane: &PaneRef) -> Option<&mut Pane> {
        (self.panes.get_mut(&pane.name)).filter(|p| p.id() == Some(pane.id.as_str()))
    }

    /// Shows tab `id`, in whatever workspace holds it.
    fn show_tab(&mut self, id: u32) {
        let Some(space) = self.arranged.workspace_of(id).map(|space| space.id) else {
            return self.say(format!("no tab t{id}"));
        };
        self.show_workspace(space);
        if self.places.insert(space, id) != Some(id) {
            self.tab_changed();
        }
    }

    fn show_workspace(&mut self, id: u32) {
        let known = self.arranged.workspaces.iter().any(|space| space.id == id);
        if id != self.workspace && known {
            self.workspace = id;
            self.tab_changed();
        }
    }

    fn tab_changed(&mut self) {
        self.dirty = true;
        self.drag = None;
        self.settle_focus();
    }

    /// The panes whose status has said who they are.
    fn live_now(&self) -> Vec<Live> {
        self.panes.values().filter_map(Pane::live).collect()
    }

    /// Derives what is shown again, if the panes or the layout changed, and
    /// keeps a workspace and tab shown that still exist.
    fn refresh(&mut self, layout_changed: bool) {
        let live = self.live_now();
        if layout_changed || live != self.live {
            self.live = live;
            self.arranged = Arc::new(self.stored.shown(&self.live));
            let spaces: Vec<(u32, Vec<u32>)> = (self.arranged.workspaces.iter())
                .map(|space| (space.id, space.tabs.iter().map(|t| t.id).collect()))
                .collect();
            if !spaces.iter().any(|(id, _)| *id == self.workspace) {
                self.workspace = spaces.first().map_or(0, |(id, _)| *id);
            }
            self.places
                .retain(|id, _| spaces.iter().any(|(s, _)| s == id));
            for (space, tabs) in &spaces {
                let first = tabs.first().copied().unwrap_or(0);
                let tab = self.places.entry(*space).or_insert(first);
                if !tabs.contains(tab) {
                    *tab = first;
                }
            }
            self.dirty = true;
        }
        self.settle_focus();
    }

    /// Keeps the focus on a tile of this tab: one that went gives it back
    /// to the pane focused before it, if shown, else to the first tile.
    fn settle_focus(&mut self) {
        let visible = self.visible();
        if self.focus.as_ref().is_some_and(|f| visible.contains(f)) {
            return;
        }
        let back = self.last_focus.take().filter(|p| visible.contains(p));
        let chosen = back.or_else(|| visible.first().cloned());
        self.dirty |= chosen != self.focus;
        self.focus = chosen;
    }

    /// Moves the focus to `pane`, showing its tab.
    fn focus_on(&mut self, pane: PaneRef) {
        if let Some(tab) = self.arranged.tab_of(&pane).map(|t| t.id) {
            self.show_tab(tab);
        }
        if self.focus.as_ref() != Some(&pane) {
            self.last_focus = self.focus.replace(pane);
        }
        self.dirty = true;
    }

    /// Lists the session again: a pane directory new since is watched,
    /// one gone is let go (its tile stays, shown as gone), and every
    /// pane's status is looked at, for routing.
    fn rescan(&mut self, now: Instant) {
        self.next_scan = after(now, RESCAN);
        let changed = self.reread_layout();
        let names = match self.session.names() {
            Ok(names) => names,
            Err(failure) => return self.say(failure.to_string()),
        };
        self.panes.retain(|name, _| names.contains(name));
        for name in names {
            let dir = self.session.pane_dir(&name);
            self.panes
                .entry(name.clone())
                .or_insert_with(|| Pane::new(&name, dir));
        }
        let visible = self.visible();
        let names: Vec<String> = self.panes.keys().cloned().collect();
        for name in names {
            self.look_at(&name, visible.iter().any(|p| p.name == name), now);
        }
        self.refresh(changed);
    }

    /// Looks at the files of the panes shown.
    fn look(&mut self, now: Instant) {
        self.next_look = after(now, LOOK);
        let before = self.live.len();
        for pane in self.visible() {
            if self.live_pane(&pane).is_some() {
                self.look_at(&pane.name, true, now);
            }
        }
        let pasting: Vec<String> = (self.panes.iter())
            .filter(|(_, p)| p.has_pastes())
            .map(|(n, _)| n.clone())
            .collect();
        for name in pasting {
            self.look_at(&name, false, now);
        }
        if self.live_now().len() != before {
            self.refresh(false);
        }
    }

    /// Looks at one pane's files; tells of its program's end once.
    fn look_at(&mut self, name: &str, shown: bool, now: Instant) {
        let Some(pane) = self.panes.get_mut(name) else {
            return;
        };
        // A pane still starting, or going, is seen at the next look.
        if let Ok(changed) = pane.look(shown, now) {
            self.dirty |= changed;
        }
    }

    /// Reads the layout file again: true if what it holds changed. A file
    /// that cannot be read, is wrong or has gone leaves the last good
    /// layout shown, and says why until it is good again.
    fn reread_layout(&mut self) -> bool {
        let text = match layout::read_text(&self.view) {
            Ok(None) if self.layout_seen => Err("the file is gone".to_owned()),
            Ok(text) => Ok(text),
            Err(failure) => Err(failure.0),
        };
        let parsed = text.and_then(|text| {
            let layout = text
                .as_deref()
                .map_or_else(|| Ok(Layout::default()), layout::parse)?;
            Ok((text, layout))
        });
        match parsed {
            Ok((text, stored)) => {
                if self.layout_text == text && self.layout_failed.is_none() {
                    return false;
                }
                // The file is good again: what it said wrong is no longer so.
                if let Some(said) = self.layout_failed.take()
                    && self.notice.as_ref().is_some_and(|(text, _)| *text == said)
                {
                    self.notice = None;
                }
                self.layout_seen |= text.is_some();
                self.layout_text = text;
                // A drag in progress was made against the old one.
                self.drag = None;
                let changed = stored != self.stored;
                self.stored = stored;
                changed
            }
            Err(why) => {
                let said = format!("layout refused, the last good one shown: {why}");
                if self.layout_failed.as_deref() != Some(said.as_str()) {
                    self.say(said.clone());
                }
                self.layout_failed = Some(said);
                false
            }
        }
    }

    /// The pane keys go to, or why there is none.
    fn focused_pane(&mut self) -> Result<&mut Pane, String> {
        let focus = self.focus.clone().ok_or("no pane has the focus")?;
        let gone = format!("pane {} is gone: it takes no keys", focus.name);
        self.live_pane_mut(&focus).ok_or(gone)
    }

    fn type_key(&mut self, stroke: Keystroke) {
        let typed = self.focused_pane().and_then(|pane| {
            let bytes = pane.key_bytes(stroke);
            pane.type_bytes(&bytes).map_err(|f| f.0)
        });
        if let Err(why) = typed {
            self.say(why);
        }
    }

    fn paste(&mut self, text: &str) {
        self.pasted = self.pasted.saturating_add(1);
        let (view, number, now) = (self.view.clone(), self.pasted, Instant::now());
        let pasted = (self.focused_pane())
            .and_then(|pane| pane.paste(&view, text, number, now).map_err(|f| f.0));
        if let Err(why) = pasted {
            self.say(why);
        }
    }

    fn apply_verb(&mut self, verb: Verb) {
        let tabs: Vec<u32> = (self.space())
            .map(|space| space.tabs.iter().map(|t| t.id).collect())
            .unwrap_or_default();
        let spaces: Vec<u32> = self.arranged.workspaces.iter().map(|s| s.id).collect();
        let tab_at = tabs.iter().position(|t| *t == self.place()).unwrap_or(0);
        let space_at = spaces
            .iter()
            .position(|s| *s == self.workspace)
            .unwrap_or(0);
        let nth = |list: &[u32], at: usize| list.get(at).copied();
        match verb {
            Verb::Detach => self.quit = true,
            Verb::NextTab | Verb::PreviousTab => {
                let at = stepped(tab_at, tabs.len(), verb == Verb::NextTab);
                if let Some(tab) = nth(&tabs, at) {
                    self.show_tab(tab);
                }
            }
            Verb::ShowTab(tab) => self.show_tab(tab),
            Verb::NextWorkspace | Verb::PreviousWorkspace => {
                let at = stepped(space_at, spaces.len(), verb == Verb::NextWorkspace);
                if let Some(space) = nth(&spaces, at) {
                    self.show_workspace(space);
                }
            }
            Verb::ShowWorkspace(space) => match spaces.contains(&space) {
                true => self.show_workspace(space),
                false => self.say(format!("no workspace w{space}")),
            },
            Verb::NextPane => self.step_pane(),
            Verb::LastPane => match self
                .last_focus
                .clone()
                .filter(|p| self.visible().contains(p))
            {
                Some(pane) => self.focus_on(pane),
                None => self.say("no other pane was focused in this tab"),
            },
            Verb::Focus(dir) => {
                let next = (self.focus.as_ref())
                    .zip(self.shown_tab())
                    .and_then(|(focus, tab)| tab.neighbor(focus, dir, self.area()));
                match next {
                    Some(pane) => self.focus_on(pane),
                    None => self.say("no pane that way"),
                }
            }
            Verb::Fit => match self.focus.clone() {
                Some(pane) => self.fit(&pane),
                None => self.say("no pane has the focus"),
            },
            Verb::Chord(chord) => self.perform_chord(chord),
        }
    }

    fn step_pane(&mut self) {
        let visible = self.visible();
        let at = (visible.iter()).position(|p| Some(p) == self.focus.as_ref());
        let next = at.map_or(0, |at| stepped(at, visible.len(), true));
        if let Some(pane) = visible.get(next).cloned() {
            self.focus_on(pane);
        }
    }

    /// Asks `pane`, once, to take the size of its tile in this viewer, in
    /// whichever tab holds it.
    fn fit(&mut self, pane: &PaneRef) {
        let area = self.area();
        let rect = (self.arranged.tab_of(pane))
            .and_then(|tab| tab.tiles(area).into_iter().find(|(p, _)| p == pane))
            .map(|(_, rect)| rect);
        let Some(rect) = rect.filter(|r| r.w > 0 && r.h > 0) else {
            return self.say(format!("{} has no room on the screen", pane.name));
        };
        if self.live_pane(pane).is_none() {
            return;
        }
        let tile = tile_of(rect);
        let cols = tile.w.clamp(1, fux3::files::MAX_COLS);
        let rows = tile.h.clamp(1, fux3::files::MAX_ROWS);
        let dir = self.session.pane_dir(&pane.name);
        if let Err(failure) = panes::ctl_line(&dir, &format!("resize {cols} {rows}")) {
            self.say(format!("fit: {failure}"));
        }
    }

    /// What a run sees of this viewer.
    fn context(&self) -> Context {
        let keys = (self.keys.bindings.iter())
            .map(|(chord, b)| (chord.clone(), b.caption.clone(), b.heading.clone()))
            .collect();
        Context {
            focus: self.focus.as_ref().map(PaneRef::to_string),
            tab: self.shown_tab().map(|tab| tab.id),
            workspace: self.space().map(|space| space.id),
            area: self.area(),
            layout: Arc::clone(&self.arranged),
            keys: Arc::new(keys),
        }
    }

    fn on_key(&mut self, stroke: Keystroke) {
        let press = stroke.press;
        let name = press.to_string();
        // A notice lasts until the next key.
        if self.notice.take().is_some() {
            self.dirty = true;
        }
        if let Some(panel) = &mut self.panel {
            self.dirty = true;
            let id = panel.id();
            let (value, action) = match panel.key(press) {
                Outcome::Stay => return,
                Outcome::Cancel => {
                    self.panel = None;
                    return self.dispatch_job(String::new(), Job::Drop(id));
                }
                Outcome::Chosen(value) => (value, None),
                Outcome::Action(n, value) => (value, Some(n)),
            };
            self.panel = None;
            let job = Job::Answer { id, value, action };
            return self.dispatch_job(String::new(), job);
        }
        if let Some(mut column) = self.column.take() {
            self.dirty = true;
            let lines = self.keys.menu(&column.keys);
            let entries = Column::entries(&lines);
            if column.moves(press, entries.len()) {
                self.column = Some(column);
                return;
            }
            let chord = match name.as_str() {
                "Escape" => return,
                "Enter" => match entries.get(column.selected) {
                    Some(key) => format!("{} {key}", column.keys),
                    None => return,
                },
                _ => format!("{} {name}", column.keys),
            };
            if self.keys.bindings.contains_key(&chord) || self.keys.opens(&chord) {
                return self.perform_chord(chord);
            }
            // The prefix again, where its map does not bind it, goes to the pane.
            let first = column.keys.split(' ').next().unwrap_or_default();
            if name == first {
                return self.type_key(stroke);
            }
            self.column = Some(column);
            return self.say(format!("{chord} is not bound"));
        }
        if self.keys.bindings.contains_key(&name) || self.keys.opens(&name) {
            return self.perform_chord(name);
        }
        self.type_key(stroke);
    }

    /// Does what `chord` is bound to; a chord that holds keys opens their
    /// menu.
    fn perform_chord(&mut self, chord: String) {
        self.dirty = true;
        if let Some(binding) = self.keys.bindings.get(&chord).cloned() {
            self.column = None;
            return self.perform(chord, binding.action);
        }
        if self.keys.opens(&chord) {
            self.column = Some(Column {
                keys: chord,
                selected: 0,
            });
            return;
        }
        self.say(format!("{chord} is not bound"));
    }

    fn perform(&mut self, chord: String, action: Action) {
        match action {
            Action::Verb(verb) => self.apply_verb(verb),
            Action::Script(generation) => {
                let job = Job::Binding {
                    chord: chord.clone(),
                    generation,
                };
                self.dispatch_job(chord, job);
            }
        }
    }

    /// Runs `job` with what this viewer shows now.
    fn dispatch_job(&mut self, label: String, job: Job) {
        let run = Run {
            label,
            job,
            context: self.context(),
        };
        if !self.run_sent && self.waiting_runs.is_empty() {
            self.start_run(run);
        } else if self.waiting_runs.len() < MAX_WAITING_RUNS {
            self.waiting_runs.push_back(run);
        } else {
            self.say(format!("too many bindings waiting: {} dropped", run.label));
        }
    }

    fn start_run(&mut self, run: Run) {
        self.run_sent = true;
        self.run_label.clone_from(&run.label);
        let sent = self
            .script
            .as_ref()
            .is_some_and(|s| s.runs.send(run).is_ok());
        if !sent {
            self.say("the configuration's thread has stopped");
        }
    }

    fn on_script(&mut self, message: ToViewer, now: Instant) {
        self.dirty = true;
        match message {
            ToViewer::Bind(chord, binding) => {
                self.keys.groups.remove(&chord);
                self.keys.bindings.insert(chord, binding);
            }
            ToViewer::Group(chord, group) => {
                self.keys.bindings.remove(&chord);
                self.keys.groups.insert(chord, group);
            }
            ToViewer::Unbind(chord) => {
                self.keys.bindings.remove(&chord);
                self.keys.groups.remove(&chord);
            }
            ToViewer::Focus(pane) => {
                // It may have just been started: look now.
                self.rescan(now);
                let target = Target::parse(&pane).ok();
                let found = target.and_then(|t| {
                    let live = self.live.iter().find(|l| l.pane.name == t.name)?;
                    t.id.is_none_or(|id| live.pane.id == id)
                        .then(|| live.pane.clone())
                });
                match found {
                    Some(pane) => self.focus_on(pane),
                    None => self.say(format!("no pane {pane} to focus")),
                }
            }
            ToViewer::Verb(verb) => self.apply_verb(verb),
            ToViewer::Panel(panel) => {
                (self.column, self.notice) = (None, None);
                self.panel = Some(panel);
            }
            // What a binding does next may name what the edit made.
            ToViewer::Reread => self.rescan(now),
            ToViewer::Fit(panes) => {
                for pane in panes.iter().filter_map(|p| PaneRef::parse(p).ok()) {
                    self.fit(&pane);
                }
            }
            ToViewer::Say(text) => self.note(text),
            ToViewer::Type(pane, bytes) => self.type_into(&pane, |_| bytes),
            ToViewer::Key(pane, press) => {
                self.type_into(&pane, |p| p.key_bytes(Keystroke { press, kitty: None }))
            }
            ToViewer::Started(stop, limit) => {
                self.running = Some(Running {
                    stop,
                    at: after(now, limit),
                    forced: false,
                });
            }
            ToViewer::Done(why) => {
                self.running = None;
                self.run_sent = false;
                if let Some(why) = why {
                    let label = std::mem::take(&mut self.run_label);
                    match label.strip_prefix("ctl: ") {
                        Some(line) => self.refuse_line(line, &why),
                        None => self.say(why),
                    }
                }
                // The keys have loaded: they are read from now on.
                self.loading = false;
                if let Some(run) = self.waiting_runs.pop_front() {
                    self.start_run(run);
                }
            }
        }
    }

    /// Bytes from a binding for pane `NAME` or `NAME@ID`, made for it.
    fn type_into(&mut self, pane: &str, bytes: impl FnOnce(&Pane) -> Vec<u8>) {
        let target = match Target::parse(pane) {
            Ok(target) => target,
            Err(failure) => return self.say(failure.0),
        };
        let found = (self.panes.get_mut(&target.name))
            .filter(|p| target.id.as_deref().is_none_or(|id| p.id() == Some(id)));
        let typed = match found {
            Some(found) => {
                let bytes = bytes(found);
                found.type_bytes(&bytes).map_err(|f| f.0)
            }
            None => Err(format!("pane {pane} is gone")),
        };
        if let Err(why) = typed {
            self.say(why);
        }
    }

    /// Says a `.view/ctl` line was refused, here and in `.view/errors`
    /// (its last lines, replaced whole under the layout's lock).
    fn refuse_line(&mut self, line: &str, why: &str) {
        let said = format!("{line}: {why}").replace('\n', " ");
        self.say(said.clone());
        let written = layout::lock(&self.view).and_then(|_lock| {
            let path = self.view.join("errors");
            let old = fux3::files::read_small(&path, 64 << 10).unwrap_or_default();
            let old = String::from_utf8_lossy(&old);
            let mut lines: Vec<&str> = old.lines().collect();
            lines.push(&said);
            let keep = lines.len().saturating_sub(MAX_ERRORS);
            let text: String = lines.iter().skip(keep).flat_map(|l| [*l, "\n"]).collect();
            layout::replace(&self.view, "errors", ".errors.new", &text)
        });
        if let Err(failure) = written {
            self.say(format!("{said} (and .view/errors: {failure})"));
        }
    }

    /// Takes the whole lines waiting in `.view/ctl`: each runs the
    /// library's `layout-command`, in turn with the bindings.
    fn read_ctl(&mut self) {
        let mut buffer = [0u8; 4096];
        let read = match &self.ctl {
            Some(ctl) => fuxix::io::read(ctl, &mut buffer),
            None => return,
        };
        let got = match read {
            Ok(n) => buffer.get(..n).unwrap_or_default().to_vec(),
            Err(Errno::AGAIN | Errno::INTR) => return,
            Err(errno) => {
                self.ctl = None;
                return self.say(format!(".view/ctl: {errno}; no longer read"));
            }
        };
        self.take_ctl(&got);
    }

    /// Runs the whole lines in `got`, keeping a part line for later.
    fn take_ctl(&mut self, got: &[u8]) {
        for &byte in got {
            if byte != b'\n' {
                if self.ctl_part.len() <= MAX_CTL_LINE {
                    self.ctl_part.push(byte);
                }
                continue;
            }
            let bytes = std::mem::take(&mut self.ctl_part);
            let line = String::from_utf8_lossy(&bytes).trim().to_owned();
            if line.is_empty() {
                continue;
            }
            if bytes.len() > MAX_CTL_LINE {
                let start: String = line.chars().take(40).collect();
                self.refuse_line(
                    &format!("{start}…"),
                    &format!("a line is at most {MAX_CTL_LINE} bytes"),
                );
                continue;
            }
            if self.waiting_runs.len() >= MAX_WAITING_RUNS {
                self.refuse_line(&line, "too many commands waiting; dropped");
                continue;
            }
            let job = Job::Call {
                name: "layout-command".to_owned(),
                args: vec![line.clone()],
            };
            self.dispatch_job(format!("ctl: {line}"), job);
        }
    }

    /// Leaving: a binding still running is stopped, and what it started is
    /// ended with it, a bounded time, so no `fux3` it ran outlives the viewer.
    fn end_run(&mut self) {
        if !self.run_sent {
            return;
        }
        if let Some(script) = &self.script {
            script.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(running) = &self.running {
            running.stop.force();
        }
        let until = after(Instant::now(), END_RUN_WAIT);
        while Instant::now() < until {
            match self.from_script.recv_timeout(Duration::from_millis(20)) {
                Ok(ToViewer::Done(_)) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return;
                }
                Ok(ToViewer::Started(stop, _)) => {
                    stop.force();
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Stops the binding whose time is up, and then insists.
    fn police_run(&mut self, now: Instant) {
        let Some(running) = &mut self.running else {
            return;
        };
        if now < running.at {
            return;
        }
        if running.forced {
            running.stop.force();
            running.at = after(now, FORCE_AFTER);
            return;
        }
        running.stop.stop();
        running.forced = true;
        running.at = after(now, FORCE_AFTER);
        if let Some(script) = &self.script {
            script.cancel.store(true, Ordering::Relaxed);
        }
        self.say("a binding ran out of time and is being stopped");
    }

    fn on_inputs(&mut self, inputs: Vec<Input>) {
        for input in inputs {
            match input {
                Input::Key(stroke) => self.on_key(stroke),
                // Typed into an open panel, never a pane.
                Input::Paste(text) if self.panel.is_some() => {
                    if let Some(panel) = &mut self.panel {
                        panel.paste(&text);
                    }
                    self.dirty = true;
                }
                Input::Paste(text) if text.len() <= MAX_PASTE => self.paste(&text),
                Input::Paste(_) | Input::PasteTooLong => {
                    self.say(format!("a paste over {MAX_PASTE} bytes was dropped"));
                }
                Input::Mouse(event) if self.panel.is_none() => self.on_mouse(event),
                Input::Reply(Reply::Attributes { sixel }) => {
                    self.sixel = sixel;
                    self.dirty = true;
                }
                Input::Mouse(_) | Input::FocusIn | Input::FocusOut | Input::Reply(_) => {}
            }
        }
    }

    /// A click focuses the tile under it; a border dragged moves, and is
    /// stored when let go.
    fn on_mouse(&mut self, event: MouseEvent) {
        let at = (event.col, event.row);
        match event.action {
            MouseAction::Press(MouseButton::Left) => {
                let area = self.area();
                let border = (self.shown_tab())
                    .map(|tab| tab.borders(area))
                    .unwrap_or_default()
                    .into_iter()
                    .find(|b| b.holds(u32::from(at.0), u32::from(at.1)));
                match border {
                    Some(border) => {
                        self.drag = Some(Drag {
                            border,
                            from: at,
                            to: at,
                            version: self.stored.version,
                        });
                    }
                    None => self.click(at),
                }
            }
            MouseAction::Motion(Some(MouseButton::Left)) => {
                if let Some(drag) = &mut self.drag {
                    drag.to = at;
                }
            }
            MouseAction::Release(_) => {
                let Some(drag) = self.drag.take() else {
                    return;
                };
                drag_or_click(self, drag, at);
            }
            MouseAction::Press(_) | MouseAction::Motion(_) => {}
        }
    }

    fn click(&mut self, (x, y): (u16, u16)) {
        let hit = (self.tiles().into_iter())
            .find(|(_, rect)| rect.contains(u32::from(x), u32::from(y)))
            .map(|(pane, _)| pane);
        if let Some(pane) = hit {
            self.focus_on(pane);
        }
    }

    /// Composes the screen and hands what changed to the painter.
    fn paint(&mut self, now: Instant) {
        self.dirty = false;
        self.next_paint = after(now, PAINT);
        let (cols, rows) = self.term;
        let mut frame = Frame::new(cols, rows);
        let area = self.area();
        let tiles = self.tiles();
        let focused_tile = (tiles.iter())
            .find(|(pane, _)| Some(pane) == self.focus.as_ref())
            .map(|(_, rect)| tile_of(*rect));
        let mut cursor = None;
        for (pane, rect) in &tiles {
            let tile = tile_of(*rect);
            let Some(live) = self.live_pane(pane) else {
                let text = format!(" {} gone ", pane.name);
                frame.label(tile.x, tile.y, tile.w, &text, NOTICE);
                continue;
            };
            let Some(screen) = live.screen.as_ref().filter(|s| s.head.id == pane.id) else {
                continue;
            };
            let at = frame.pane(tile, screen);
            if Some(pane) == self.focus.as_ref() {
                cursor = at;
            }
            if live.state() != "running" {
                let text = format!(" {} {} ", pane.name, live.state());
                let width = u16::try_from(fux_vt::str_width(&text)).unwrap_or(u16::MAX);
                let x = tile.x.saturating_add(tile.w.saturating_sub(width));
                let y = tile.y.saturating_add(tile.h.saturating_sub(1));
                frame.label(x, y, tile.w, &text, NOTICE);
            }
        }
        if let Some(tab) = self.shown_tab() {
            frame.borders(&edges(&tab.borders(area)), focused_tile);
        }
        let all = self.shown_tab().map_or(0, |tab| tab.tiles(area).len());
        if all == 0 {
            let text = "no panes here: fux3 start NAME, or C-b t n";
            let middle = u16::try_from(area.h / 2).unwrap_or(0);
            frame.label(1, middle, tile_of(area).w, text, BORDER_TEXT);
        }
        // Panels and the bar cover the panes; the cursor hides beneath them.
        let above_bar = Tile {
            h: tile_of(area).h.saturating_sub(1),
            ..tile_of(area)
        };
        if let Some(column) = &self.column {
            let lines = self.keys.menu(&column.keys);
            column.draw(&mut frame, above_bar, &lines);
            cursor = None;
        }
        if let Some(open) = &self.panel {
            open.draw(&mut frame, above_bar);
            cursor = None;
        }
        let bar = self.draw_bar(&mut frame);
        if bar {
            let last = rows.saturating_sub(1);
            if cursor.is_some_and(|(_, y)| y >= last) {
                cursor = None;
            }
        }
        // Pictures go over the panes, never under a menu or list, nor on
        // the bar's row; any change draws everything again, as a picture's
        // pixels stay until the cells under them are written.
        let mut drawn = Vec::new();
        if self.sixel && self.column.is_none() && self.panel.is_none() {
            let bottom = if bar { rows.saturating_sub(1) } else { rows };
            for (pane, rect) in &tiles {
                let (tile, live) = (tile_of(*rect), self.live_pane(pane));
                let Some((screen, dir)) = live.and_then(|p| Some((p.screen.as_ref()?, &p.dir)))
                else {
                    continue;
                };
                let first = render::first_row(screen, tile);
                let shown = screen.pictures.iter();
                drawn.extend(
                    shown.filter_map(|placed| pictures::place(tile, first, bottom, dir, placed)),
                );
            }
        }
        let fresh = drawn != self.pictures;
        if fresh {
            self.drawn = Frame::default();
        }
        let mut bytes = b"\x1b[?2026h\x1b[?25l".to_vec();
        bytes.extend(frame.diff(&self.drawn));
        if fresh {
            pictures::draw(&drawn, &mut bytes);
        }
        bytes.extend_from_slice(b"\x1b[0m");
        if let Some((x, y)) = cursor {
            let (row, col) = (y.saturating_add(1), x.saturating_add(1));
            bytes.extend_from_slice(format!("\x1b[{row};{col}H\x1b[?25h").as_bytes());
        }
        bytes.extend_from_slice(b"\x1b[?2026l");
        if self.painter.send(bytes) {
            self.drawn = frame;
            self.pictures = drawn;
        } else {
            self.pictures.clear();
            self.dirty = true;
        }
    }

    /// The bar, on the last row, as fux draws it: all of it while a menu,
    /// a repeating group, a list or a prompt is open; else only its right
    /// corner, and only for a notice or the zoom marker. True if drawn.
    fn draw_bar(&self, frame: &mut Frame) -> bool {
        let (cols, rows) = (frame_cols(self.term), self.term.1);
        let y = rows.saturating_sub(1);
        let whole = self.column.is_some() || self.panel.is_some();
        let mut right: Vec<(String, &str)> = Vec::new();
        if let Some((text, sgr)) = &self.notice {
            right.push((text.replace('\n', "  "), sgr));
        } else if let Some(column) = &self.column {
            right.push((format!("{} \u{2026}", column.keys), KEYS));
        } else {
            let zoomed = (self.shown_tab())
                .is_some_and(|tab| tab.zoom.as_ref().is_some_and(|z| tab.holds(z)));
            let mut text = String::new();
            if zoomed {
                text.push_str("[zoom] ");
            }
            if whole {
                text.push_str(&self.pane_label());
            }
            if !text.is_empty() {
                right.push((text, BAR));
            }
        }
        if !whole && right.is_empty() {
            return false;
        }
        let width = |parts: &[(String, &str)]| {
            parts
                .iter()
                .map(|(t, _)| fux_vt::str_width(t))
                .sum::<usize>()
        };
        let cols_n = usize::from(cols);
        let share = match whole && cols >= 4 {
            true => (cols_n / 2).saturating_add(cols_n / 4),
            false => cols_n,
        };
        let right_width = width(&right).min(share).min(cols_n);
        let start = cols_n.saturating_sub(right_width).saturating_sub(1);
        let left_limit = cols_n
            .saturating_sub(right_width)
            .saturating_sub(usize::from(right_width > 0));
        let small = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        if whole {
            frame.fill(0, y, cols, " ", BAR);
            let mut x = 0usize;
            for (text, sgr) in self.bar_left() {
                let room = left_limit.saturating_sub(x);
                if room == 0 {
                    break;
                }
                let shown = panel::cut(&text, small(room));
                frame.label(small(x), y, small(room), &shown, sgr);
                x = x.saturating_add(fux_vt::str_width(&shown));
            }
        } else {
            let from = start.saturating_sub(1);
            frame.fill(small(from), y, small(cols_n.saturating_sub(from)), " ", BAR);
        }
        let mut x = start;
        for (text, sgr) in right {
            let room = start.saturating_add(right_width).saturating_sub(x);
            if room == 0 {
                break;
            }
            let shown = panel::cut(&text, small(room));
            frame.label(small(x), y, small(room), &shown, sgr);
            x = x.saturating_add(fux_vt::str_width(&shown));
        }
        true
    }

    /// The workspace shown, then its tabs, the shown one marked.
    fn bar_left(&self) -> Vec<(String, &'static str)> {
        let mut parts = Vec::new();
        let Some(space) = self.space() else {
            return parts;
        };
        parts.push((format!(" {} ", space.title), BAR_BOLD));
        let shown = self.place();
        for tab in &space.tabs {
            let sgr = if tab.id == shown { TAB } else { BAR };
            parts.push((format!(" {} ", tab.title), sgr));
        }
        parts
    }

    /// The focused pane, as the bar names it.
    fn pane_label(&self) -> String {
        self.focus
            .as_ref()
            .map(|pane| pane.name.clone())
            .unwrap_or_default()
    }

    fn timeout(&self, now: Instant, decoder: &Decoder) -> Duration {
        let mut at = self.next_scan;
        let mut consider = |other: Instant| at = at.min(other);
        if self.dirty && self.painter.is_idle() {
            consider(self.next_paint);
        }
        if let Some(running) = &self.running {
            consider(running.at);
        }
        if let Some(due) = decoder.deadline() {
            consider(due);
        }
        if !self.tiles().is_empty() || self.panes.values().any(Pane::has_pastes) {
            consider(self.next_look);
        }
        if self.panes.values().any(Pane::has_queued) {
            consider(after(now, RETRY_KEYS));
        }
        at.saturating_duration_since(now)
    }
}

/// The cells of `borders`, as the frame draws them.
fn edges(borders: &[layout::Border]) -> Vec<render::Edge> {
    let small = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
    let mut out = Vec::new();
    for border in borders {
        let along = border.from..border.from.saturating_add(border.length);
        for at in along {
            out.push(match border.axis {
                layout::Axis::Columns => render::Edge {
                    x: small(border.at),
                    y: small(at),
                    upright: true,
                },
                layout::Axis::Rows => render::Edge {
                    x: small(at),
                    y: small(border.at),
                    upright: false,
                },
            });
        }
    }
    out
}

/// The columns a frame of the terminal's size has.
fn frame_cols((cols, _): (u16, u16)) -> u16 {
    cols.min(render::MAX_COLS)
}

/// The edit a drag makes: the pane whose border moves, which way, and how
/// many cells; `None` if it has not moved.
fn drag_edit(drag: &Drag) -> Option<(PaneRef, Dir, u32)> {
    let (moved, forward) = match drag.border.axis {
        layout::Axis::Columns => (drag.to.0.abs_diff(drag.from.0), drag.to.0 > drag.from.0),
        layout::Axis::Rows => (drag.to.1.abs_diff(drag.from.1), drag.to.1 > drag.from.1),
    };
    if moved == 0 {
        return None;
    }
    // The pane on the side the border moves away from, so the nearest
    // split along that axis is the one this border belongs to.
    let edit = match (drag.border.axis, forward) {
        (layout::Axis::Columns, true) => (drag.border.before.clone(), Dir::Right),
        (layout::Axis::Columns, false) => (drag.border.after.clone(), Dir::Left),
        (layout::Axis::Rows, true) => (drag.border.before.clone(), Dir::Down),
        (layout::Axis::Rows, false) => (drag.border.after.clone(), Dir::Up),
    };
    Some((edit.0, edit.1, u32::from(moved)))
}

/// A border let go: stored through the configuration's `drag-border`, or,
/// if it never moved, a click.
fn drag_or_click(view: &mut View, mut drag: Drag, at: (u16, u16)) {
    drag.to = at;
    let Some((pane, dir, cells)) = drag_edit(&drag) else {
        return view.click(drag.from);
    };
    let args = vec![
        pane.to_string(),
        dir.name().to_owned(),
        cells.to_string(),
        drag.version.to_string(),
    ];
    view.dispatch_job(
        "drag-border".to_owned(),
        Job::Call {
            name: "drag-border".to_owned(),
            args,
        },
    );
}

fn terminal_size() -> (u16, u16) {
    fuxix::terminal::stdin_size().map_or((80, 24), |(size, _)| (size.cols.get(), size.rows.get()))
}

const USAGE: &str = "\
fux3-view: shows a fux3 session's panes as fux does, through their files

  fux3-view                show the session FUX3_DIR names (fux3 view sets it)
  fux3-view --do 'LINE'    apply one layout command to .view/layout and print
                           what it did; a refusal goes to stderr, exit 1

The layout commands, the keys and the configuration: see the README.
";

/// What the command line asks for.
enum Asked {
    View,
    Do(String),
    Help,
}

fn parse_args() -> Result<Asked, Failure> {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => Ok(Asked::View),
        ["--do", line] => Ok(Asked::Do((*line).to_owned())),
        ["--help" | "-h" | "help"] => Ok(Asked::Help),
        _ => Err(Failure::new(format!(
            "unexpected arguments {args:?}; try fux3-view --help"
        ))),
    }
}

/// `--do LINE`: one layout command, on the file, with no viewer, in the
/// size of this terminal (else 80x24).
fn do_line(line: &str) -> Result<(), Failure> {
    let session = Session::open()?;
    let (cols, rows) = terminal_size();
    let area = Rect {
        x: 0,
        y: 0,
        w: cols.into(),
        h: rows.into(),
    };
    let edited = command::run_line(&session, line, area, &AtomicBool::new(false))?;
    let mut out = std::io::stdout();
    out.write_all(edited.to_string().as_bytes())
        .map_err(|e| io_failure("stdout", e))
}

/// `.view/ctl`, made if missing, opened to read without waiting (and to
/// write, so it never reads as ended while no writer has it open). A
/// regular file there was written with no viewer running (a shell's `>`
/// makes one): its lines are given back to be run, and the FIFO takes its
/// place.
fn open_ctl(view: &std::path::Path) -> Result<(File, Vec<u8>), Failure> {
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    let path = view.join("ctl");
    let mut written = Vec::new();
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        written = fux3::files::read_small(&path, 64 << 10)?;
        std::fs::remove_file(&path).map_err(|e| io_failure(&path.display().to_string(), e))?;
    }
    match fuxix::file::private_fifo(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io_failure(&path.display().to_string(), e)),
    }
    let file = File::options()
        .read(true)
        .write(true)
        .custom_flags(fuxix::file::NOFOLLOW | fuxix::file::NONBLOCK)
        .open(&path)
        .map_err(|e| io_failure(&path.display().to_string(), e))?;
    match file.metadata().is_ok_and(|m| m.file_type().is_fifo()) {
        true => Ok((file, written)),
        false => Err(Failure::new(format!("{} is not a FIFO", path.display()))),
    }
}

fn run() -> Result<(), Failure> {
    match parse_args()? {
        Asked::View => {}
        Asked::Do(line) => return do_line(&line),
        Asked::Help => {
            let mut out = std::io::stdout();
            return out
                .write_all(USAGE.as_bytes())
                .map_err(|e| io_failure("stdout", e));
        }
    }
    if !fuxix::terminal::on_one_terminal() {
        return Err(Failure::new(
            "the viewer needs one terminal on stdin and stdout",
        ));
    }
    let session = Session::open()?;
    let view = command::view_dir(&session)?;
    let (ctl, written) = open_ctl(&view)?;
    let stops = [
        Signal::Winch,
        Signal::Term,
        Signal::Hup,
        Signal::Int,
        Signal::Quit,
    ];
    let signals = fuxix::signal::Pipe::new(&stops).map_err(|e| io_failure("signals", e))?;
    let (paint_wake, paint_poke) = waker().map_err(|e| io_failure("a waker", e))?;
    let (script_wake, script_poke) = waker().map_err(|e| io_failure("a waker", e))?;
    let painter = Painter::start(paint_poke).map_err(|e| io_failure("a thread", e))?;
    let (to_viewer, from_script) = std::sync::mpsc::sync_channel(64);
    let (config, note) = match script::load_config() {
        Ok(config) => (config, None),
        Err(why) => (None, Some(why)),
    };
    let script = script::start(
        config,
        command::fux3_program()?,
        session.clone(),
        to_viewer,
        script_poke,
    )
    .map_err(|e| io_failure("a thread", e))?;
    let raw =
        fuxix::terminal::Raw::enter(std::io::stdin()).map_err(|e| io_failure("raw mode", e))?;
    // An abort still leaves the screen it found.
    fuxix::terminal::leaving_on_abort(LEAVE);
    let now = Instant::now();
    let mut view = View {
        session,
        view,
        stored: Layout::default(),
        layout_text: None,
        layout_failed: None,
        layout_seen: false,
        arranged: Arc::new(Layout::default()),
        live: Vec::new(),
        workspace: 0,
        places: BTreeMap::new(),
        panes: BTreeMap::new(),
        focus: None,
        last_focus: None,
        term: terminal_size(),
        keys: Keys::default(),
        column: None,
        panel: None,
        drag: None,
        notice: None,
        ctl: Some(ctl),
        ctl_part: Vec::new(),
        run_label: String::new(),
        pasted: 0,
        script: Some(script),
        from_script,
        running: None,
        waiting_runs: VecDeque::new(),
        loading: true,
        run_sent: false,
        painter,
        drawn: Frame::default(),
        sixel: false,
        pictures: Vec::new(),
        dirty: true,
        next_paint: now,
        next_scan: now,
        next_look: now,
        quit: false,
    };
    view.refresh(true);
    view.take_ctl(&written);
    if let Some(note) = note {
        view.say(note);
    }
    view.painter.send(ENTER.to_vec());
    let ended = serve(&mut view, &signals, &paint_wake, &script_wake);
    view.end_run();
    view.leave_pastes();
    view.painter.finish(LEAVE.as_bytes().to_vec());
    drop(raw);
    ended
}

impl View {
    /// Leaving: pastes handed over get a bounded time to be taken, then
    /// their files go.
    fn leave_pastes(&mut self) {
        let until = after(Instant::now(), PASTE_LEAVE_WAIT);
        while self.panes.values().any(Pane::has_pastes) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
            let now = Instant::now();
            for pane in self.panes.values_mut() {
                let _ = pane.look(false, now);
            }
        }
        self.panes.values_mut().for_each(Pane::drop_pastes);
    }
}

/// What one poll found.
struct Ready {
    stdin: bool,
    signals: bool,
    paint: bool,
    script: bool,
    ctl: bool,
}

fn wait(
    view: &View,
    poll: &mut Poll,
    timeout: Duration,
    sources: (&fuxix::signal::Pipe, &Wake, &Wake),
) -> Result<Ready, Failure> {
    let (signals, paint, script) = sources;
    let stdin = std::io::stdin();
    let mut fds = poll.list();
    let keys = (!view.loading).then(|| fds.push(PollFd::new(&stdin, Events::IN)));
    let signal_entry = fds.push(PollFd::new(signals.reader(), Events::IN));
    let paint_entry = fds.push(PollFd::new(paint, Events::IN));
    let script_entry = fds.push(PollFd::new(script, Events::IN));
    let ctl = view
        .ctl
        .as_ref()
        .map(|ctl| fds.push(PollFd::new(ctl, Events::IN)));
    match fds.poll(Some(timeout)) {
        Ok(_) | Err(Errno::INTR) => {}
        Err(errno) => return Err(io_failure("poll", errno)),
    }
    Ok(Ready {
        stdin: keys.is_some_and(|entry| !fds.events(entry).is_empty()),
        signals: !fds.events(signal_entry).is_empty(),
        paint: !fds.events(paint_entry).is_empty(),
        script: !fds.events(script_entry).is_empty(),
        ctl: ctl.is_some_and(|entry| !fds.events(entry).is_empty()),
    })
}

fn serve(
    view: &mut View,
    signals: &fuxix::signal::Pipe,
    paint_wake: &Wake,
    script_wake: &Wake,
) -> Result<(), Failure> {
    let mut poll = Poll::default();
    let mut decoder = Decoder::with_paste_limit(TIMING, MAX_PASTE);
    // ENTER asked for the primary device attributes: whether sixel shows.
    decoder.expect(Instant::now());
    let mut buffer = vec![0u8; 16 << 10];
    let stdin = std::io::stdin();
    while !view.quit {
        let now = Instant::now();
        if now >= view.next_scan {
            view.rescan(now);
        }
        if now >= view.next_look {
            view.look(now);
        }
        for pane in view.panes.values_mut().filter(|p| p.has_queued()) {
            if let Err(failure) = pane.flush() {
                view.notice = Some((failure.0, ERROR));
                view.dirty = true;
            }
        }
        view.police_run(now);
        if view.dirty && view.painter.is_idle() && now >= view.next_paint {
            view.paint(now);
        }
        let timeout = view.timeout(Instant::now(), &decoder);
        let ready = wait(view, &mut poll, timeout, (signals, paint_wake, script_wake))?;
        let now = Instant::now();
        if ready.paint {
            paint_wake.drain();
        }
        if ready.script {
            script_wake.drain();
        }
        // A turn's worth: a binding that keeps asking cannot hold off keys,
        // painting or its own time limit.
        for _ in 0..SCRIPT_TURN {
            let Ok(message) = view.from_script.try_recv() else {
                break;
            };
            view.on_script(message, now);
        }
        if ready.signals {
            while let Some(signal) = signals.next() {
                if signal == Signal::Winch {
                    view.term = terminal_size();
                    view.dirty = true;
                } else {
                    view.quit = true;
                }
            }
        }
        if ready.ctl {
            view.read_ctl();
        }
        let mut inputs = Vec::new();
        if ready.stdin {
            match fuxix::io::read(&stdin, &mut buffer) {
                // The terminal went away: nothing is left to show.
                Ok(0) => view.quit = true,
                Ok(n) => {
                    decoder.bytes(buffer.get(..n).unwrap_or_default(), &mut inputs);
                    decoder.mark(now);
                }
                Err(Errno::AGAIN | Errno::INTR) => {}
                Err(errno) => return Err(io_failure("reading keys", errno)),
            }
        }
        if decoder.deadline().is_some_and(|due| now >= due) {
            decoder.timeout(&mut inputs);
        }
        view.on_inputs(inputs);
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            let mut err = std::io::stderr();
            let _ = writeln!(err, "fux3-view: {failure}");
            ExitCode::FAILURE
        }
    }
}

//! fux3-view: shows a workspace of a fux3 session as fux does, split, in
//! this terminal, through its files alone, and sends keys to the focused
//! pane.
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
use fux3::session::{DEFAULT_WORKSPACE, Session, Workspace};
use fuxix::Errno;
use fuxix::poll::{Events, Poke, Poll, PollFd, Wake, waker};
use fuxix::process::Signal;
use keys::{Action, Keys};
use layout::{Layout, Live, PaneRef, Rect, Tab as LayoutTab, Target};
use panel::{Column, Outcome, Panel};
use panes::Pane;
use render::{Frame, Tile};
use script::{Context, Job, Run, Script, ToViewer, Verb};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::{Duration, Instant};

/// How often the workspace's directory is looked at again.
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
/// How many chords may lead one to the next (a chord bound to `chord`, or
/// a binding that runs one): past it the next is refused, so a chord bound
/// to itself ends.
const MAX_CHORD_HOPS: u8 = 8;
/// How many messages from the language are taken in one turn.
const SCRIPT_TURN: usize = 64;
/// The longest paste passed on.
const MAX_PASTE: usize = 1 << 20;
/// How long a paste's file is kept for its pane to take it; the viewer
/// can't tell when it has (another writer's command moves the same
/// `commands` count), so it keeps each the whole time.
const PASTE_WAIT: Duration = Duration::from_secs(10);
/// Leaving keeps a paste this long after it was made, at most, before its
/// file goes.
const PASTE_LEAVE_WAIT: Duration = Duration::from_secs(1);
/// Paste files kept at once.
const MAX_PASTES: usize = 16;
/// Escape's wait for the rest of a sequence, and a paste's, in ms.
const TIMING: Timing = Timing::new(50, 1000);

/// The alternate screen, pastes bracketed, the cursor hidden, and the mouse
/// reported (buttons, SGR encoding); leaving undoes them.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?2004h\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b[2J\x1b[c";
const LEAVE: &str = "\x1b[0m\x1b[0 q\x1b[?1006l\x1b[?1000l\x1b[?25h\x1b[?2004l\x1b[?1049l";

fn after(now: Instant, wait: Duration) -> Instant {
    now.checked_add(wait).unwrap_or(now)
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

struct View {
    /// The workspace shown, and its `.view`: the layout and its lock.
    workspace: Workspace,
    view: File,
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
    /// The tab shown.
    tab: u32,
    /// The tab and focus each workspace shown before was left with.
    places: BTreeMap<String, (u32, Option<PaneRef>)>,
    panes: BTreeMap<String, Pane>,
    focus: Option<PaneRef>,
    /// The pane focused before, for `last-pane`.
    last_focus: Option<PaneRef>,
    /// The terminal's size, capped at the largest frame.
    term: (u16, u16),
    keys: Keys,
    /// The keys typed so far of a chord: their menu is shown.
    column: Option<Column>,
    /// A list or a line to type, open over the panes.
    panel: Option<Panel>,
    /// A notice for the bar's corner, until the next key: its text and
    /// colours.
    notice: Option<(String, &'static str)>,
    /// The next key is dropped: the workspace it was typed at was removed.
    swallow: bool,
    /// The workspace this viewer is closing, by its directory's id, and
    /// whether the close has removed it.
    closing: Option<(String, bool)>,
    /// The panes `when-gone` waits for the going of, each with its
    /// workspace's directory (`Workspace::id`).
    watched: Vec<(String, Target)>,
    /// The run in flight, where it came from.
    in_flight: Option<Origin>,
    /// Pastes handed over, to name their files, and the files kept, each
    /// with when it was made.
    pasted: u64,
    pastes: Vec<(File, String, Instant)>,
    script: Option<Script>,
    from_script: Receiver<ToViewer>,
    running: Option<Running>,
    waiting_runs: VecDeque<(Run, u8)>,
    /// The keys are still loading (each part in its time): keys typed
    /// wait in the terminal until then, so none goes to a pane it was not
    /// meant for.
    loading: bool,
    /// A run was handed to the language and has not finished; the
    /// configuration's load counts as one.
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

/// Where a run came from: the workspace shown when it was asked for (what
/// it does to panes and panels is for that workspace alone), and how many
/// chords led to it.
struct Origin {
    workspace: Workspace,
    hops: u8,
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

    /// The room tiles share: the whole screen. The bar, when it shows, is
    /// drawn over its last row and takes no room.
    fn area(&self) -> Rect {
        let (cols, rows) = self.term;
        Rect {
            x: 0,
            y: 0,
            w: u32::from(cols),
            h: u32::from(rows),
        }
    }

    fn shown_tab(&self) -> Option<&LayoutTab> {
        self.arranged.tab(self.tab)
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

    fn show_tab(&mut self, id: u32) {
        if self.arranged.tab(id).is_none() {
            return self.say(format!("no tab t{id}"));
        }
        if self.tab != id {
            self.tab = id;
            self.dirty = true;
            self.settle_focus();
        }
    }

    /// Shows the workspace `name`, made if missing: its own layout and
    /// panes, with the tab and focus this viewer left it with, if it showed
    /// it before.
    fn show_workspace(&mut self, name: &str, now: Instant) {
        if name == self.workspace.name {
            return;
        }
        let opened = (self.workspace.session)
            .workspace(name, true)
            .and_then(|workspace| {
                let view = command::view_dir(&workspace)?;
                Ok((workspace, view))
            });
        let (workspace, view) = match opened {
            Ok(opened) => opened,
            Err(failure) => return self.say(format!("workspace {name}: {failure}")),
        };
        // One whose directory went (its name emptied) is not remembered.
        let left = (self.tab, self.focus.take());
        if !self.workspace.name.is_empty() {
            self.places.insert(self.workspace.name.clone(), left);
        }
        let (tab, focus) = self.places.remove(name).unwrap_or_default();
        // A list or prompt was about the workspace left: its answer must not
        // act on this one.
        self.column = None;
        if let Some(panel) = self.panel.take() {
            self.dispatch_job(String::new(), Job::Drop(panel.id()), 0);
        }
        (self.workspace, self.view) = (workspace, view);
        (self.tab, self.focus, self.last_focus) = (tab, focus, None);
        self.panes.clear();
        self.stored = Layout::default();
        (self.layout_text, self.layout_failed, self.layout_seen) = (None, None, false);
        self.rescan(now, false);
        self.refresh(true);
    }

    /// The workspace `step` places from the one shown, among the session's
    /// by name, wrapping.
    fn step_workspace(&mut self, forward: bool, now: Instant) {
        let names = match self.workspace.session.workspaces() {
            Ok(names) => names,
            Err(failure) => return self.say(failure.to_string()),
        };
        let at = names.iter().position(|n| *n == self.workspace.name);
        let next = at.map_or(0, |at| stepped(at, names.len(), forward));
        if let Some(name) = names.get(next).cloned() {
            self.show_workspace(&name, now);
        }
    }

    /// The panes whose status has said who they are.
    fn live_now(&self) -> Vec<Live> {
        self.panes.values().filter_map(Pane::live).collect()
    }

    /// Derives what is shown again, if the panes or the layout changed, and
    /// keeps a tab shown that still exists.
    fn refresh(&mut self, layout_changed: bool) {
        let live = self.live_now();
        if layout_changed || live != self.live {
            self.live = live;
            // The focus stays on a pane renamed: it is its id.
            for held in [&mut self.focus, &mut self.last_focus]
                .into_iter()
                .flatten()
            {
                let now = self.live.iter().find(|l| l.pane.id == held.id);
                held.name = now.map_or(held.name.clone(), |l| l.pane.name.clone());
            }
            self.arranged = Arc::new(self.stored.shown(&self.live));
            if self.arranged.tab(self.tab).is_none() {
                self.tab = self.arranged.tabs.first().map_or(0, |tab| tab.id);
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

    /// Lists the workspace again: a pane directory new since is watched,
    /// one gone is let go (its tile stays, shown as gone), and every
    /// pane's status is looked at, for routing. A workspace whose
    /// directory went (it was closed) gives way to the first one left,
    /// else to a new `main`. With `fit`, a layout file changed since it was
    /// last read was edited from outside (`--do`, another viewer), which
    /// fits nothing: the panes whose tiles it changed are fitted here.
    fn rescan(&mut self, now: Instant, fit: bool) {
        self.next_scan = after(now, RESCAN);
        // As it is called now: it may have been renamed.
        if let Ok(now) = command::named(&self.workspace) {
            self.workspace = now;
        } else {
            // Another directory may have its name now: that one is shown.
            let left = (self.workspace.session.workspaces()).unwrap_or_default();
            // This viewer's close of it is under way: its word is waited
            // for, and only a close that removed it makes this its own.
            let closing = (self.closing.as_ref()).filter(|(id, _)| *id == self.workspace.id());
            let Some(own) = closing.map_or(Some(false), |(_, done)| done.then_some(true)) else {
                return;
            };
            self.closing = None;
            let name = std::mem::take(&mut self.workspace.name);
            let next = match left.contains(&name) {
                true => &name,
                false => left.first().map_or(DEFAULT_WORKSPACE, String::as_str),
            };
            self.show_workspace(next, now);
            // Said where it is seen, unless this viewer's own close of it
            // took it away; the next key was meant for what was shown
            // before, and is dropped with the notice still up.
            if own {
                return;
            }
            self.swallow = true;
            return self.say(format!("workspace {name} was removed"));
        }
        let old = (fit && self.layout_seen).then(|| self.stored.clone());
        let changed = self.reread_layout();
        let names = match self.workspace.names() {
            Ok(names) => names,
            Err(failure) => return self.say(failure.to_string()),
        };
        // A name is only where a pane is listed now: each pane is kept,
        // with the keys waiting for it, under the name that lists its
        // directory now.
        let mut before = std::mem::take(&mut self.panes);
        for name in names {
            let Ok(dir) = self.workspace.pane(&name) else {
                continue;
            };
            let old = before.iter().find(|(_, pane)| pane.is(&dir));
            let old = old.map(|(old, _)| old.clone());
            let mut pane =
                (old.and_then(|old| before.remove(&old))).unwrap_or_else(|| Pane::new(&name, dir));
            pane.name.clone_from(&name);
            self.panes.insert(name, pane);
        }
        let visible = self.visible();
        let names: Vec<String> = self.panes.keys().cloned().collect();
        for name in names {
            self.look_at(&name, visible.iter().any(|p| p.name == name));
        }
        self.refresh(changed);
        if let Some(old) = old.filter(|_| changed) {
            for pane in layout::changed_file(&old, &self.stored, &self.live, self.area()) {
                self.fit(&pane);
            }
        }
        // A watched life is gone once no pane here has its id; a name, once
        // no pane has it.
        let shown = self.workspace.id();
        let gone = |(space, pane): &(String, Target)| {
            let there = match &pane.id {
                // One of its name whose status is not read yet may be it.
                Some(id) => (self.panes.values())
                    .any(|p| p.id().map_or(p.name == pane.name, |now| now == id)),
                None => self.panes.contains_key(&pane.name),
            };
            *space == shown && !there
        };
        let (gone, watched) = std::mem::take(&mut self.watched)
            .into_iter()
            .partition(gone);
        self.watched = watched;
        for (_, pane) in Vec::from_iter(gone) {
            self.dispatch_job(String::new(), Job::Gone(pane.to_string()), 0);
        }
    }

    /// Looks at the files of the panes shown.
    fn look(&mut self, now: Instant) {
        self.next_look = after(now, LOOK);
        let before = self.live.len();
        for pane in self.visible() {
            if self.live_pane(&pane).is_some() {
                self.look_at(&pane.name, true);
            }
        }
        if self.live_now().len() != before {
            self.refresh(false);
        }
    }

    /// Looks at one pane's files.
    fn look_at(&mut self, name: &str, shown: bool) {
        let Some(pane) = self.panes.get_mut(name) else {
            return;
        };
        // A pane still starting, or going, is seen at the next look.
        if let Ok(changed) = pane.look(shown) {
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
        let live = (self.panes.get_mut(&focus.name)).filter(|p| p.id() == Some(&focus.id));
        live.ok_or(gone)
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
        if self.pastes.len() >= MAX_PASTES {
            return self.say(format!("paste dropped: {MAX_PASTES} are waiting"));
        }
        self.pasted = self.pasted.saturating_add(1);
        let now = command::named(&self.workspace).unwrap_or_else(|_| self.workspace.clone());
        let (shown, number) = (now.dir.join(command::VIEW), self.pasted);
        let pasted = self
            .view
            .try_clone()
            .map_err(|e| e.to_string())
            .and_then(|view| {
                let name =
                    (self.focused_pane()?.paste((&view, &shown), text, number)).map_err(|f| f.0)?;
                Ok((view, name, Instant::now()))
            });
        match pasted {
            Ok(paste) => self.pastes.push(paste),
            Err(why) => self.say(why),
        }
    }

    /// Removes the paste files kept their whole wait.
    fn settle_pastes(&mut self, now: Instant) {
        self.pastes.retain(|(view, name, made)| {
            let done = now >= after(*made, PASTE_WAIT);
            if done {
                let _ = fuxix::file::at::remove_file(view, name);
            }
            !done
        });
    }

    fn apply_verb(&mut self, verb: Verb, now: Instant, hops: u8) {
        let tabs: Vec<u32> = self.arranged.tabs.iter().map(|t| t.id).collect();
        let tab_at = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
        match verb {
            Verb::Detach => self.quit = true,
            Verb::NextTab | Verb::PreviousTab => {
                let at = stepped(tab_at, tabs.len(), verb == Verb::NextTab);
                if let Some(tab) = tabs.get(at).copied() {
                    self.show_tab(tab);
                }
            }
            Verb::ShowTab(tab) => self.show_tab(tab),
            Verb::NextWorkspace => self.step_workspace(true, now),
            Verb::PreviousWorkspace => self.step_workspace(false, now),
            Verb::ShowWorkspace(name) => self.show_workspace(&name, now),
            Verb::NextPane => {
                let visible = self.visible();
                let at = (visible.iter()).position(|p| Some(p) == self.focus.as_ref());
                let next = at.map_or(0, |at| stepped(at, visible.len(), true));
                if let Some(pane) = visible.get(next).cloned() {
                    self.focus_on(pane);
                }
            }
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
            Verb::Chord(chord) => self.perform_chord(chord, hops.saturating_add(1)),
        }
    }

    /// Asks `pane`, once, to take the size of its tile in this viewer, in
    /// whichever tab holds it.
    fn fit(&mut self, pane: &PaneRef) {
        let area = self.area();
        let rect = (self.arranged.tab_of(pane))
            .and_then(|tab| tab.tiles(area).into_iter().find(|(p, _)| p == pane))
            .map(|(_, rect)| rect);
        // No tile any more (the change closed it): nothing to fit.
        let Some(rect) = rect else {
            return;
        };
        if rect.w == 0 || rect.h == 0 {
            return self.say(format!("{} has no room on the screen", pane.name));
        }
        let Some(live) = self.live_pane(pane) else {
            return;
        };
        let tile = tile_of(rect);
        let cols = tile.w.clamp(1, fux3::files::MAX_COLS);
        let rows = tile.h.clamp(1, fux3::files::MAX_ROWS);
        // A pane going meanwhile (a kill takes its name first) needs no fit.
        if let Err(failure) = panes::ctl_line(&live.dir, &format!("resize {cols} {rows}"))
            && (self.workspace.pane(&pane.name)).is_ok_and(|now| live.is(&now))
        {
            self.say(format!("fit: {failure}"));
        }
    }

    /// What a run sees of this viewer.
    fn context(&self) -> Context {
        let keys = (self.keys.bindings.iter())
            .map(|(chord, b)| (chord.clone(), b.caption.clone(), b.heading.clone()))
            .collect();
        Context {
            focus: (self.focus.as_ref()).map(|p| format!("{}@{}", p.name, p.id)),
            tab: self.shown_tab().map(|tab| tab.id),
            workspace: self.workspace.clone(),
            area: self.area(),
            layout: Arc::clone(&self.arranged),
            keys: Arc::new(keys),
        }
    }

    fn on_key(&mut self, stroke: Keystroke) {
        let press = stroke.press;
        let name = press.to_string();
        if std::mem::take(&mut self.swallow) {
            return;
        }
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
                    return self.dispatch_job(String::new(), Job::Drop(id), 0);
                }
                Outcome::Chosen(value) => (value, None),
                Outcome::Action(n, value) => (value, Some(n)),
            };
            self.panel = None;
            let job = Job::Answer { id, value, action };
            return self.dispatch_job(String::new(), job, 0);
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
                return self.perform_chord(chord, 0);
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
            return self.perform_chord(name, 0);
        }
        self.type_key(stroke);
    }

    /// Does what `chord` is bound to, `hops` chords after the key that
    /// led to it; a chord that holds keys opens their menu.
    fn perform_chord(&mut self, chord: String, hops: u8) {
        self.dirty = true;
        if hops > MAX_CHORD_HOPS {
            return self.say(format!(
                "{chord}: refused, it came after {MAX_CHORD_HOPS} chords in a row"
            ));
        }
        if let Some(binding) = self.keys.bindings.get(&chord).cloned() {
            self.column = None;
            return match binding.action {
                Action::Verb(verb) => self.apply_verb(verb, Instant::now(), hops),
                Action::Script(generation) => {
                    let job = Job::Binding {
                        chord: chord.clone(),
                        generation,
                    };
                    self.dispatch_job(chord, job, hops)
                }
            };
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

    /// Runs `job` with what this viewer shows now, `hops` chords after the
    /// key that led to it.
    fn dispatch_job(&mut self, label: String, job: Job, hops: u8) {
        let run = Run {
            label,
            job,
            context: self.context(),
        };
        if !self.run_sent && self.waiting_runs.is_empty() {
            self.start_run(run, hops);
        } else if self.waiting_runs.len() < MAX_WAITING_RUNS {
            self.waiting_runs.push_back((run, hops));
        } else {
            self.say(format!("too many bindings waiting: {} dropped", run.label));
        }
    }

    fn start_run(&mut self, run: Run, hops: u8) {
        self.run_sent = true;
        self.in_flight = Some(Origin {
            workspace: run.context.workspace.clone(),
            hops,
        });
        let sent = self
            .script
            .as_ref()
            .is_some_and(|s| s.runs.send(run).is_ok());
        if !sent {
            self.say("the configuration's thread has stopped");
        }
    }

    /// The workspace the run in flight was asked for in, if it is no
    /// longer the one shown: what it does to panes and panels is refused.
    fn left_behind(&self) -> Option<String> {
        (self.in_flight.as_ref())
            .filter(|run| run.workspace.id() != self.workspace.id())
            .map(|run| run.workspace.name.clone())
    }

    fn on_script(&mut self, message: ToViewer, now: Instant) {
        self.dirty = true;
        if let Some(space) = self.left_behind() {
            let refused = match &message {
                ToViewer::Focus(pane) => Some(format!("focus {pane}")),
                ToViewer::Fit(_) => Some("fit".to_owned()),
                ToViewer::Watch(pane) => Some(format!("when-gone {pane}")),
                ToViewer::Type(pane, _) => Some(format!("send to {pane}")),
                ToViewer::Panel(panel) => {
                    self.dispatch_job(String::new(), Job::Drop(panel.id()), 0);
                    Some("a list or prompt".to_owned())
                }
                // Its binding would start with the workspace now shown.
                ToViewer::Verb(Verb::Chord(chord)) => Some(format!("chord {chord}")),
                ToViewer::Bind(..)
                | ToViewer::Group(..)
                | ToViewer::Unbind(_)
                | ToViewer::Verb(_)
                | ToViewer::Reread
                | ToViewer::Closing(..)
                | ToViewer::Say(_)
                | ToViewer::Started(..)
                | ToViewer::Done(_)
                | ToViewer::Loaded(_) => None,
            };
            if let Some(what) = refused {
                return self.say(format!(
                    "{what}: refused, it was for workspace {space}, no longer shown"
                ));
            }
        }
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
                self.rescan(now, false);
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
            ToViewer::Verb(verb) => {
                let hops = self.in_flight.as_ref().map_or(0, |run| run.hops);
                self.apply_verb(verb, now, hops);
            }
            ToViewer::Panel(panel) => {
                (self.column, self.notice) = (None, None);
                self.panel = Some(panel);
            }
            // What a binding does next may name what the edit made.
            ToViewer::Reread => {
                self.closing = None;
                self.rescan(now, false);
            }
            ToViewer::Closing(id, done) => {
                self.closing = Some((id, done));
                self.rescan(now, false);
            }
            ToViewer::Watch(pane) => {
                let space = self.workspace.id();
                (self.watched).extend(Target::parse(&pane).ok().map(|pane| (space, pane)));
                // One gone already is seen now.
                self.rescan(now, false);
            }
            ToViewer::Fit(panes) => {
                for pane in panes.iter().filter_map(|p| PaneRef::parse(p).ok()) {
                    self.fit(&pane);
                }
            }
            ToViewer::Say(text) => {
                // A notice that is no error.
                (self.notice, self.dirty) = (Some((text, NOTICE)), true);
            }
            ToViewer::Type(pane, bytes) => self.type_into(&pane, &bytes),
            ToViewer::Started(stop, limit) => {
                self.running = Some(Running {
                    stop,
                    at: after(now, limit),
                    forced: false,
                });
            }
            ToViewer::Loaded(why) => {
                // The keys have loaded: they are read from now on.
                self.loading = false;
                if let Some(why) = why {
                    self.say(why);
                }
                self.next_run();
            }
            ToViewer::Done(why) => {
                self.in_flight = None;
                if let Some(why) = why {
                    self.say(why);
                }
                self.next_run();
            }
        }
    }

    /// The run in flight is over: the next one waiting starts.
    fn next_run(&mut self) {
        self.running = None;
        self.run_sent = false;
        if let Some((run, hops)) = self.waiting_runs.pop_front() {
            self.start_run(run, hops);
        }
    }

    /// Bytes from a binding for pane `NAME` or `NAME@ID`, made for it.
    fn type_into(&mut self, pane: &str, bytes: &[u8]) {
        let target = match Target::parse(pane) {
            Ok(target) => target,
            Err(failure) => return self.say(failure.0),
        };
        let found = (self.panes.get_mut(&target.name))
            .filter(|p| target.id.as_deref().is_none_or(|id| p.id() == Some(id)));
        let typed = match found {
            Some(found) => found.type_bytes(bytes).map_err(|f| f.0),
            None => Err(format!("pane {pane} is gone")),
        };
        if let Err(why) = typed {
            self.say(why);
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
                Ok(ToViewer::Done(_) | ToViewer::Loaded(_))
                | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
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
                Input::Mouse(event) if self.panel.is_none() => self.click(event),
                Input::Reply(Reply::Attributes { sixel }) => {
                    self.sixel = sixel;
                    self.dirty = true;
                }
                Input::Mouse(_) | Input::FocusIn | Input::FocusOut | Input::Reply(_) => {}
            }
        }
    }

    /// A click focuses the tile under it.
    fn click(&mut self, event: MouseEvent) {
        let (x, y) = (u32::from(event.col), u32::from(event.row));
        let hit = (self.tiles().into_iter()).find(|(_, rect)| rect.contains(x, y));
        if let (MouseAction::Press(MouseButton::Left), Some((pane, _))) = (event.action, hit) {
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
            frame.borders(&tab.borders(area), focused_tile);
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
                let Some(screen) = live.and_then(|p| p.screen.as_ref()) else {
                    continue;
                };
                let first = render::first_row(screen, tile);
                let shown = screen.pictures.iter();
                drawn.extend(
                    shown.filter_map(|placed| {
                        pictures::place(tile, first, bottom, &pane.id, placed)
                    }),
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
            let dirs = drawn.iter().filter_map(|d| {
                let pane = self.panes.values().find(|p| p.id() == Some(&d.pane))?;
                Some((d, &pane.dir))
            });
            pictures::draw(dirs, &mut bytes);
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
    /// a list or a prompt is open; else only its right
    /// corner, and only for a notice or the zoom marker. True if drawn.
    fn draw_bar(&self, frame: &mut Frame) -> bool {
        let (cols, rows) = self.term;
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
            if let (true, Some(focus)) = (whole, &self.focus) {
                text.push_str(&focus.name);
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
        let mut parts = vec![(format!(" {} ", self.workspace.name), BAR_BOLD)];
        for tab in &self.arranged.tabs {
            let sgr = if tab.id == self.tab { TAB } else { BAR };
            parts.push((format!(" {} ", tab.title), sgr));
        }
        parts
    }

    /// Leaving: a paste just made gets the rest of `PASTE_LEAVE_WAIT` to
    /// be taken, then every paste file goes.
    fn leave_pastes(&mut self) {
        if let Some((_, _, newest)) = self.pastes.last() {
            std::thread::sleep(
                after(*newest, PASTE_LEAVE_WAIT).saturating_duration_since(Instant::now()),
            );
        }
        for (view, name, _) in std::mem::take(&mut self.pastes) {
            let _ = fuxix::file::at::remove_file(&view, &name);
        }
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
        if !self.tiles().is_empty() {
            consider(self.next_look);
        }
        if let Some((_, _, made)) = self.pastes.first() {
            consider(after(*made, PASTE_WAIT));
        }
        if self.panes.values().any(Pane::has_queued) {
            consider(after(now, RETRY_KEYS));
        }
        at.saturating_duration_since(now)
    }
}

/// The terminal's size, as far as a frame reaches: a bigger terminal is
/// left blank past it, and the bar is on the frame's last row.
fn terminal_size() -> (u16, u16) {
    let (cols, rows) = fuxix::terminal::stdin_size()
        .map_or((80, 24), |(size, _)| (size.cols.get(), size.rows.get()));
    (cols.min(render::MAX_COLS), rows.min(render::MAX_ROWS))
}

const USAGE: &str = "\
fux3-view: shows a workspace of a fux3 session as fux does, through its files

  fux3-view [WORKSPACE]              show WORKSPACE (default main, made if
                                     missing) of the session FUX3_DIR names
                                     (fux3 view sets it)
  fux3-view [WORKSPACE] --do 'LINE'  apply one layout command to the
                                     workspace's .view/layout and print what
                                     it did; a refusal goes to stderr, exit 1

The layout commands, the keys and the configuration: see the README.
";

/// What the command line asks for, and of which workspace.
enum Asked {
    View(String),
    Do(String, String),
    Help,
}

fn parse_args() -> Result<Asked, Failure> {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let (space, rest) = match words.as_slice() {
        ["--help" | "-h" | "help"] => return Ok(Asked::Help),
        [first, rest @ ..] if !first.starts_with('-') => (*first, rest),
        rest => (DEFAULT_WORKSPACE, rest),
    };
    fux3::session::check_name(space)?;
    match rest {
        [] => Ok(Asked::View(space.to_owned())),
        ["--do", line] => Ok(Asked::Do(space.to_owned(), (*line).to_owned())),
        _ => Err(Failure::new(format!(
            "unexpected arguments {args:?}; try fux3-view --help"
        ))),
    }
}

/// `--do LINE`: one layout command, on the workspace's file, with no
/// viewer, in the size of this terminal (else 80x24).
fn do_line(space: &str, line: &str) -> Result<(), Failure> {
    let workspace = Session::open()?.workspace(space, true)?;
    let (cols, rows) = terminal_size();
    let area = Rect {
        x: 0,
        y: 0,
        w: cols.into(),
        h: rows.into(),
    };
    let edited = command::run_line(&workspace, line, area, &AtomicBool::new(false))?;
    let mut out = std::io::stdout();
    out.write_all(edited.to_string().as_bytes())
        .map_err(|e| fux3::io("stdout", e))
}

fn run() -> Result<(), Failure> {
    let space = match parse_args()? {
        Asked::View(space) => space,
        Asked::Do(space, line) => return do_line(&space, &line),
        Asked::Help => {
            let mut out = std::io::stdout();
            return out
                .write_all(USAGE.as_bytes())
                .map_err(|e| fux3::io("stdout", e));
        }
    };
    if !fuxix::terminal::on_one_terminal() {
        return Err(Failure::new(
            "the viewer needs one terminal on stdin and stdout",
        ));
    }
    let session = Session::open()?;
    let workspace = session.workspace(&space, true)?;
    let view = command::view_dir(&workspace)?;
    let stops = [
        Signal::Winch,
        Signal::Term,
        Signal::Hup,
        Signal::Int,
        Signal::Quit,
    ];
    let signals = fuxix::signal::Pipe::new(&stops).map_err(|e| fux3::io("signals", e))?;
    let (paint_wake, paint_poke) = waker().map_err(|e| fux3::io("a waker", e))?;
    let (script_wake, script_poke) = waker().map_err(|e| fux3::io("a waker", e))?;
    let painter = Painter::start(paint_poke).map_err(|e| fux3::io("a thread", e))?;
    let (to_viewer, from_script) = std::sync::mpsc::sync_channel(64);
    let (config, note) = match script::load_config() {
        Ok(config) => (config, None),
        Err(why) => (None, Some(why)),
    };
    let script = script::start(
        config,
        command::fux3_program()?,
        workspace.clone(),
        to_viewer,
        script_poke,
    )
    .map_err(|e| fux3::io("a thread", e))?;
    let raw = fuxix::terminal::Raw::enter(std::io::stdin()).map_err(|e| fux3::io("raw mode", e))?;
    // An abort still leaves the screen it found.
    fuxix::terminal::leaving_on_abort(LEAVE);
    let now = Instant::now();
    let mut view = View {
        workspace,
        view,
        stored: Layout::default(),
        layout_text: None,
        layout_failed: None,
        layout_seen: false,
        arranged: Arc::new(Layout::default()),
        live: Vec::new(),
        tab: 0,
        places: BTreeMap::new(),
        panes: BTreeMap::new(),
        focus: None,
        last_focus: None,
        term: terminal_size(),
        keys: Keys::default(),
        column: None,
        panel: None,
        notice: None,
        swallow: false,
        closing: None,
        watched: Vec::new(),
        in_flight: None,
        pasted: 0,
        pastes: Vec::new(),
        script: Some(script),
        from_script,
        running: None,
        waiting_runs: VecDeque::new(),
        loading: true,
        run_sent: true,
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

/// What one poll found.
struct Ready {
    stdin: bool,
    signals: bool,
    paint: bool,
    script: bool,
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
    match fds.poll(Some(timeout)) {
        Ok(_) | Err(Errno::INTR) => {}
        Err(errno) => return Err(fux3::io("poll", errno)),
    }
    Ok(Ready {
        stdin: keys.is_some_and(|entry| !fds.events(entry).is_empty()),
        signals: !fds.events(signal_entry).is_empty(),
        paint: !fds.events(paint_entry).is_empty(),
        script: !fds.events(script_entry).is_empty(),
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
            // What changes while a binding runs is its own edit: not fitted.
            view.rescan(now, !view.run_sent);
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
        view.settle_pastes(now);
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
                    // The clear a new size brings erases every picture.
                    view.pictures.clear();
                    view.dirty = true;
                } else {
                    view.quit = true;
                }
            }
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
                Err(errno) => return Err(fux3::io("reading keys", errno)),
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

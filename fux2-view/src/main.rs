//! The fux2 viewer: shows a session's panes, split, in this terminal, and
//! sends keys to the focused one. It owns nothing: end it however you like
//! and every pane goes on.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("fux2-view runs on Linux and macOS only");

mod keys;
mod panel;
mod render;
mod script;

use fux_vt::keys::KeyPress;
use fux_vt::keys::decode::{Decoder, Input, Timing};
use fux_vt::keys::mouse::{MouseAction, MouseButton, MouseEvent};
use fux2::client::{check_hello, dial};
use fux2::conn::Conn;
use fux2::dir::{self, Session};
use fux2::layout::{
    self, Amount, Dir, Layout, Live, Op, PaneRef, Rect, Tab as LayoutTab, Target, Workspace,
};
use fux2::model::apply;
use fux2::wire::{
    Answer, Code, Failure, Hello, MAX_COLS, MAX_PARTS, MAX_ROWS, Meta, PaneId, Part, REVISION,
    Request, Row, State, Token,
};
use fuxix::Errno;
use fuxix::poll::{Events, Poke, Poll, PollFd, Wake, waker};
use fuxix::process::Signal;
use keys::{Action, Binding, Group, Keys};
use panel::{Ask, Outcome, Panel, Pick};
use render::{Frame, Tile};
use script::{Context, Job, Run, Script, ToViewer, Verb};
use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

/// How often the session directory is looked at again.
const RESCAN: Duration = Duration::from_secs(1);
/// How long one wait asks for; it is renewed.
const WAIT_MS: u32 = 10_000;
/// How long before a refused wait is tried again.
const RETRY: Duration = Duration::from_secs(1);
/// The least time between paints.
const PAINT: Duration = Duration::from_millis(16);
/// How long a message stays on the status line.
const MESSAGE: Duration = Duration::from_secs(5);
/// How long past its time a stopped binding gets before it is forced.
const FORCE_AFTER: Duration = Duration::from_secs(1);
/// How long leaving waits for the terminal to take the last bytes.
const LEAVE_WAIT: Duration = Duration::from_millis(500);
/// Keys not yet taken by a pane: past this, keys are left unread.
const MAX_QUEUED_PARTS: usize = 1024;
/// Bindings waiting behind the one running.
const MAX_WAITING_RUNS: usize = 16;
/// How long held input waits for a pane to take it once the keys load.
const HOLD_FOR_FOCUS: Duration = Duration::from_secs(2);
/// How long leaving waits for a binding to be stopped.
const END_RUN_WAIT: Duration = Duration::from_secs(1);
/// How long a focus asked for waits for its pane.
const FOCUS_WAIT: Duration = Duration::from_secs(10);
/// Panes waiting to be fitted.
const MAX_FITS: usize = 256;
/// How many messages from the language are taken in one turn.
const SCRIPT_TURN: usize = 64;
/// Input held while the configuration loads.
const MAX_HELD_KEYS: usize = 256;
/// The longest paste passed on.
const MAX_PASTE: usize = 60 << 10;
/// Descriptors the viewer needs apart from its panes'.
const BASE_FILES: u64 = 32;
/// Two connections for each of a session's panes.
const FILES_PER_PANE: u64 = 2;
/// Escape's wait for the rest of a sequence, and a paste's, in ms.
const TIMING: Timing = Timing::new(50, 1000);

/// The alternate screen, pastes bracketed, the cursor hidden, and the mouse
/// reported (buttons and drags, SGR encoding); leaving undoes them.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?2004h\x1b[?25l\x1b[?1002h\x1b[?1006h\x1b[2J";
const LEAVE: &str = "\x1b[0m\x1b[0 q\x1b[?1006l\x1b[?1002l\x1b[?25h\x1b[?2004l\x1b[?1049l";

/// Where a pane's watching connection is in its round.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Greeting,
    Reading,
    Waiting,
    /// Nothing asked: the pane has ended, or a wait is to be tried again.
    Resting,
}

/// A pane's input: its own connection, so that a parked wait never holds
/// up a key, and what is queued for exactly this pane.
struct Inlet {
    conn: Conn,
    greeted: bool,
    busy: bool,
    parts: VecDeque<Part>,
    resize: Option<(u16, u16)>,
}

struct Pane {
    name: String,
    hello: Hello,
    expected: Option<PaneId>,
    conn: Conn,
    phase: Phase,
    retry: Option<Instant>,
    token: Option<Token>,
    meta: Option<Meta>,
    rows: Vec<Row>,
    inlet: Option<Inlet>,
}

fn after(now: Instant, wait: Duration) -> Instant {
    now.checked_add(wait).unwrap_or(now)
}

fn io_failure(what: &str, error: impl std::fmt::Display) -> Failure {
    Failure::new(Code::Io, format!("{what}: {error}"))
}

/// Writes frames on a thread of its own, so that a terminal that stopped
/// reading holds up no key: while it is busy, frames are merged.
struct Painter {
    frames: SyncSender<Vec<u8>>,
    /// How many frames were handed over, and how many are written.
    sent: std::cell::Cell<u64>,
    written: Arc<std::sync::atomic::AtomicU64>,
}

impl Painter {
    fn start(poke: Poke) -> std::io::Result<Painter> {
        let (frames, incoming): (_, Receiver<Vec<u8>>) = std::sync::mpsc::sync_channel(1);
        let written = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let count = Arc::clone(&written);
        std::thread::Builder::new()
            .name("fux2-paint".to_owned())
            .spawn(move || {
                let mut out = std::io::stdout();
                while let Ok(frame) = incoming.recv() {
                    if out.write_all(&frame).and_then(|()| out.flush()).is_err() {
                        return;
                    }
                    count.fetch_add(1, Ordering::SeqCst);
                    poke.poke();
                }
            })?;
        Ok(Painter {
            frames,
            sent: std::cell::Cell::new(0),
            written,
        })
    }

    fn is_idle(&self) -> bool {
        self.written.load(Ordering::SeqCst) >= self.sent.get()
    }

    fn send(&self, bytes: Vec<u8>) -> bool {
        match self.frames.try_send(bytes) {
            Ok(()) => {
                self.sent.set(self.sent.get().saturating_add(1));
                true
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }

    /// Hands over the last bytes once the frame in progress is out, and
    /// waits for them to be written: each step has its own time.
    fn finish(&self, bytes: Vec<u8>) {
        let wait_idle = |budget: Duration| {
            let until = after(Instant::now(), budget);
            while !self.is_idle() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        wait_idle(LEAVE_WAIT);
        // Queued as soon as there is room; its own time starts then.
        let until = after(Instant::now(), LEAVE_WAIT);
        let mut bytes = Some(bytes);
        while let Some(next) = bytes.take() {
            match self.frames.try_send(next) {
                Ok(()) => self.sent.set(self.sent.get().saturating_add(1)),
                Err(TrySendError::Full(back)) if Instant::now() < until => {
                    bytes = Some(back);
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => return,
            }
        }
        wait_idle(LEAVE_WAIT);
    }
}

/// The binding running now.
struct Running {
    stop: grift::Interrupter,
    at: Instant,
    forced: bool,
}

/// Where a workspace was left: its tab shown, and the one before it.
#[derive(Clone, Copy, Default)]
struct Place {
    tab: u32,
    last: u32,
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
    /// The last good layout read, and the text it came from.
    stored: Layout,
    layout_text: Option<String>,
    /// What the layout file's last refused reading said.
    layout_failed: Option<String>,
    /// What is shown: the stored layout with routed panes, derived.
    arranged: Arc<Layout>,
    /// The live panes `arranged` was made with.
    live: Vec<Live>,
    /// The workspace shown, and the one before it, by id.
    workspace: u32,
    last_workspace: u32,
    places: BTreeMap<u32, Place>,
    panes: BTreeMap<String, Pane>,
    focus: Option<PaneRef>,
    focus_name: Option<String>,
    /// The pane focused before, for `last-pane`.
    last_focus: Option<PaneRef>,
    /// A pane to focus as soon as it is there: its name, its id if known,
    /// and until when it is waited for.
    wanted: Option<(String, Option<PaneId>, Instant)>,
    /// That pane's name, and what had the focus before it: it comes back
    /// there when the pane goes.
    back: Option<(String, PaneRef)>,
    term: (u16, u16),
    keys: Keys,
    /// The keys typed so far of a chord: its menu is shown.
    prefix: Option<String>,
    panel: Option<Panel>,
    drag: Option<Drag>,
    /// Panes to fit once the layout reaches a version and they are seen,
    /// until when.
    fits: Vec<(u64, PaneRef, Instant)>,
    message: Option<(String, Instant)>,
    file_limit: Option<u64>,
    script: Option<Script>,
    from_script: Receiver<ToViewer>,
    running: Option<Running>,
    waiting_runs: VecDeque<Run>,
    /// The configuration is still being read: keys it might bind wait.
    loading: bool,
    held: Vec<Input>,
    /// The last key typed while loading was `C-b`.
    loading_prefix: bool,
    /// The focus was moved by a key or a binding, not only settled.
    focus_moved: bool,
    /// The keys have loaded; held input waits for a focused pane until then.
    keys_loaded: Option<Instant>,
    /// A run was handed to the language and has not finished.
    run_sent: bool,
    painter: Painter,
    drawn: Frame,
    dirty: bool,
    next_paint: Instant,
    next_scan: Instant,
    quit: bool,
}

/// What holds before the default keys have loaded: a way out.
fn first_keys() -> Keys {
    let mut keys = Keys::default();
    keys.bindings.insert(
        "C-b d".to_owned(),
        Binding {
            action: Action::Verb(Verb::Detach),
            caption: "detach".to_owned(),
            heading: "Session".to_owned(),
            order: 0,
        },
    );
    keys
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

impl Inlet {
    fn open(conn: Conn) -> Result<Inlet, Failure> {
        let mut inlet = Inlet {
            conn,
            greeted: false,
            busy: true,
            parts: VecDeque::new(),
            resize: None,
        };
        let hello = Request::Hello { revision: REVISION };
        inlet.conn.queue(&hello.encode())?;
        Ok(inlet)
    }

    /// Sends what waits, one request at a time.
    fn advance(&mut self) -> Result<(), Failure> {
        if self.busy || !self.greeted {
            return Ok(());
        }
        let request = if let Some((cols, rows)) = self.resize.take() {
            Request::Resize {
                cols,
                rows,
                expected: None,
            }
        } else if self.parts.is_empty() {
            return Ok(());
        } else {
            let mut parts = Vec::new();
            let mut size = 0usize;
            while let Some(part) = self.parts.front() {
                let len = match part {
                    Part::Bytes(bytes) => bytes.len(),
                    Part::Key(name) => name.len(),
                    Part::Paste(text) => text.len(),
                };
                let full = size.saturating_add(len) > MAX_PASTE || parts.len() >= MAX_PARTS;
                if full && !parts.is_empty() {
                    break;
                }
                size = size.saturating_add(len);
                parts.extend(self.parts.pop_front());
            }
            Request::Send {
                parts,
                expected: None,
            }
        };
        self.conn.queue(&request.encode())?;
        self.busy = true;
        Ok(())
    }
}

impl Pane {
    fn ask(&mut self, request: &Request, phase: Phase) -> Result<(), Failure> {
        self.conn.queue(&request.encode())?;
        self.phase = phase;
        Ok(())
    }

    fn read_screen(&mut self) -> Result<(), Failure> {
        let request = Request::Screen { since: self.token };
        self.ask(&request, Phase::Reading)
    }

    fn wait_or_rest(&mut self) -> Result<(), Failure> {
        let ended = self
            .meta
            .as_ref()
            .is_some_and(|m| m.state != State::Running);
        match (self.token, ended) {
            (Some(after), false) => self.ask(&Request::Wait { after, ms: WAIT_MS }, Phase::Waiting),
            _ => {
                self.phase = Phase::Resting;
                Ok(())
            }
        }
    }

    /// One answer on the watching connection; true if the screen changed.
    fn answered(&mut self, body: &[u8], now: Instant) -> Result<bool, Failure> {
        let answer = Answer::decode(body)?;
        if let Answer::Refused(failure) = &answer {
            if self.phase == Phase::Waiting && failure.code == Code::Timeout {
                self.wait_or_rest()?;
                return Ok(false);
            }
            if failure.code == Code::Busy {
                self.phase = Phase::Resting;
                self.retry = Some(after(now, RETRY));
                return Ok(false);
            }
        }
        let answer = answer.accepted()?;
        match (self.phase, answer) {
            (Phase::Greeting, answer @ Answer::Hello(_)) => {
                self.hello = check_hello(answer, self.expected, &self.name)?;
                self.read_screen()?;
                Ok(false)
            }
            (
                Phase::Reading,
                Answer::Screen {
                    token,
                    meta,
                    change,
                },
            ) => {
                apply(&mut self.rows, meta.rows, change);
                self.token = Some(token);
                self.meta = Some(*meta);
                self.wait_or_rest()?;
                Ok(true)
            }
            (Phase::Waiting, Answer::Waited { .. }) => {
                self.read_screen()?;
                Ok(false)
            }
            _ => Err(Failure::new(Code::Protocol, "an answer out of turn")),
        }
    }

    /// Its title bar: the title given in the layout, if any, and its name.
    fn title(&self, tile: Tile, own: &str) -> String {
        let mut title = match own {
            "" => format!(" {} ", self.name),
            own => format!(" {own} ({}) ", self.name),
        };
        if let Some(meta) = &self.meta {
            if meta.state != State::Running {
                title.push_str(&format!("[{}] ", meta.state));
            }
            if meta.cols > tile.w || meta.rows > tile.h.saturating_sub(1) {
                title.push_str(&format!("[clipped {}x{}] ", meta.cols, meta.rows));
            }
            if let Some(limit) = meta.limitations.last() {
                title.push_str(&format!("[{limit}] "));
            }
        }
        title
    }
}

impl View {
    fn say(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), after(Instant::now(), MESSAGE)));
        self.dirty = true;
    }

    /// The room tiles share: the screen above the status line.
    fn area(&self) -> Rect {
        let (cols, rows) = self.term;
        Rect {
            x: 0,
            y: 0,
            w: u32::from(cols.min(render::MAX_COLS)),
            h: u32::from(rows.min(render::MAX_ROWS).saturating_sub(1)),
        }
    }

    fn space(&self) -> Option<&Workspace> {
        (self.arranged.workspaces.iter()).find(|space| space.id == self.workspace)
    }

    fn place(&self) -> Place {
        self.places
            .get(&self.workspace)
            .copied()
            .unwrap_or_default()
    }

    fn shown_tab(&self) -> Option<&LayoutTab> {
        let tab = self.place().tab;
        self.space()?.tabs.iter().find(|t| t.id == tab)
    }

    /// The tiles drawn: those of the shown tab with room for a title and a row.
    fn tiles(&self) -> Vec<(PaneRef, Rect)> {
        let area = self.area();
        (self.shown_tab())
            .map(|tab| tab.tiles(area))
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, rect)| rect.w > 0 && rect.h >= 2)
            .collect()
    }

    fn visible(&self) -> Vec<PaneRef> {
        self.tiles().into_iter().map(|(pane, _)| pane).collect()
    }

    /// The pane a tile shows, once it has said who it is.
    fn live_pane(&self, pane: &PaneRef) -> Option<&Pane> {
        (self.panes.get(&pane.name)).filter(|p| p.hello.id == pane.id && p.phase != Phase::Greeting)
    }

    /// Shows tab `id`, in whatever workspace holds it.
    fn show_tab(&mut self, id: u32) {
        let Some(space) = self.arranged.workspace_of(id).map(|space| space.id) else {
            return self.say(format!("no tab t{id}"));
        };
        self.show_workspace(space);
        let place = self.places.entry(space).or_default();
        if place.tab != id {
            *place = Place {
                tab: id,
                last: place.tab,
            };
            self.tab_changed();
        }
    }

    fn show_workspace(&mut self, id: u32) {
        let known = self.arranged.workspaces.iter().any(|space| space.id == id);
        if id != self.workspace && known {
            self.last_workspace = self.workspace;
            self.workspace = id;
            self.tab_changed();
        }
    }

    fn tab_changed(&mut self) {
        self.dirty = true;
        self.drag = None;
        self.settle_focus();
    }

    /// The panes that have said who they are.
    fn live_now(&self) -> Vec<Live> {
        (self.panes.values())
            .filter(|pane| pane.phase != Phase::Greeting)
            .map(|pane| Live {
                pane: PaneRef {
                    name: pane.name.clone(),
                    id: pane.hello.id,
                },
                labels: pane.hello.labels.clone(),
            })
            .collect()
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
            if !spaces.iter().any(|(id, _)| *id == self.last_workspace) {
                self.last_workspace = self.workspace;
            }
            self.places
                .retain(|id, _| spaces.iter().any(|(s, _)| s == id));
            for (space, tabs) in &spaces {
                let first = tabs.first().copied().unwrap_or(0);
                let place = self.places.entry(*space).or_insert(Place {
                    tab: first,
                    last: first,
                });
                if !tabs.contains(&place.tab) {
                    place.tab = first;
                }
                if !tabs.contains(&place.last) {
                    place.last = place.tab;
                }
            }
            self.dirty = true;
        }
        self.settle_focus();
    }

    /// Keeps the focus on a tile of this tab. A pane asked for takes it when
    /// it appears, its tab shown; when that pane goes, the focus returns to
    /// where it was; any other that went gives way to the next by name.
    fn settle_focus(&mut self) {
        let now = Instant::now();
        let wanted = (self.wanted.clone())
            .filter(|(_, _, until)| now < *until)
            .and_then(|(name, id, _)| {
                let live = self.live.iter().find(|l| l.pane.name == name)?;
                id.is_none_or(|id| live.pane.id == id)
                    .then(|| live.pane.clone())
            });
        if let Some(pane) = &wanted
            && !self.visible().contains(pane)
            && let Some(tab) = self.arranged.tab_of(pane).map(|t| t.id)
        {
            self.show_tab(tab);
        }
        let visible = self.visible();
        let wanted = wanted.filter(|pane| visible.contains(pane));
        // Until the focus is moved by hand, it is the first tile.
        let first = visible.first().cloned();
        let settled = self.focus_moved || self.focus == first;
        let present = self.focus.as_ref().is_some_and(|f| visible.contains(f));
        if wanted.is_none() && settled && present {
            return;
        }
        let returning = (self.back.as_ref())
            .filter(|(name, _)| self.focus_name.as_deref() == Some(name.as_str()))
            .map(|(_, pane)| pane.clone())
            .filter(|pane| visible.contains(pane));
        let last = match self.focus_moved {
            true => self.focus_name.clone().unwrap_or_default(),
            false => String::new(),
        };
        let mut by_name = visible.clone();
        by_name.sort_by(|a, b| a.name.cmp(&b.name));
        let chosen = wanted
            .clone()
            .or(returning)
            .or_else(|| match self.focus_moved {
                true => by_name.iter().find(|p| p.name > last).cloned(),
                false => None,
            })
            .or(first);
        if let Some(pane) = &chosen
            && self.wanted.as_ref().is_some_and(|w| w.0 == pane.name)
        {
            self.wanted = None;
        } else if chosen.is_some() {
            self.back = None;
        }
        self.dirty |= chosen != self.focus;
        self.focus_name = chosen
            .as_ref()
            .map(|p| p.name.clone())
            .or_else(|| self.focus_name.take());
        self.focus = chosen;
    }

    /// Moves the focus to `pane` by hand.
    fn focus_on(&mut self, pane: PaneRef) {
        if self.focus.as_ref() != Some(&pane) {
            self.last_focus = self.focus.take();
        }
        self.focus_name = Some(pane.name.clone());
        self.focus = Some(pane);
        self.focus_moved = true;
        self.back = None;
        self.dirty = true;
    }

    fn rescan(&mut self, now: Instant) {
        self.next_scan = after(now, RESCAN);
        let changed = self.reread_layout();
        let names = match self.session.names() {
            Ok(names) => names,
            Err(failure) => return self.say(failure.to_string()),
        };
        for name in names {
            if self.panes.contains_key(&name) {
                continue;
            }
            let count = u64::try_from(self.panes.len()).unwrap_or(u64::MAX);
            let need = count
                .saturating_add(1)
                .saturating_mul(FILES_PER_PANE)
                .saturating_add(BASE_FILES);
            if let Some(limit) = self.file_limit
                && need > limit
            {
                self.say(format!(
                    "pane {name} not shown: it needs {need} open files, and the limit is {limit}"
                ));
                continue;
            }
            match self.watch(&name) {
                Ok(pane) => {
                    self.panes.insert(name, pane);
                }
                // A pane that is going, or still starting, is seen next time.
                Err(failure) if matches!(failure.code, Code::Missing | Code::Busy) => {}
                Err(failure) => self.say(format!("{name}: {failure}")),
            }
        }
        self.refresh(changed);
        self.fit_due();
    }

    fn watch(&self, name: &str) -> Result<Pane, Failure> {
        let (mut conn, expected) = dial(&self.session, name, Instant::now())?;
        conn.queue(&Request::Hello { revision: REVISION }.encode())?;
        Ok(Pane {
            name: name.to_owned(),
            hello: Hello {
                revision: REVISION,
                id: expected.unwrap_or_default(),
                labels: vec![("name".to_owned(), name.to_owned())],
                cols: 0,
                rows: 0,
                state: State::Running,
                pid: 0,
            },
            expected,
            conn,
            phase: Phase::Greeting,
            retry: None,
            token: None,
            meta: None,
            rows: Vec::new(),
            inlet: None,
        })
    }

    /// Reads the layout file again: true if what it holds changed. A file
    /// that cannot be read or is wrong leaves the last good layout shown,
    /// and says why until it is good again.
    fn reread_layout(&mut self) -> bool {
        let path = self.session.root.join("layout");
        let text = match layout::read_small_file(&path, layout::MAX_BYTES) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                let said = format!("layout: {e}");
                self.layout_failed = Some(said.clone());
                self.say(said);
                return false;
            }
        };
        if self.layout_text == text && self.layout_failed.is_none() {
            return false;
        }
        let parsed = match &text {
            Some(text) => layout::parse(text),
            None => Ok(Layout::default()),
        };
        match parsed {
            Ok(stored) => {
                // The file is good again: what it said wrong is no longer so.
                if let Some(said) = self.layout_failed.take()
                    && self.message.as_ref().is_some_and(|(text, _)| *text == said)
                {
                    self.message = None;
                }
                self.layout_text = text;
                // A drag in progress was made against the old one.
                self.drag = None;
                let changed = stored != self.stored;
                self.stored = stored;
                changed
            }
            Err(why) => {
                let said = format!("layout: {why}");
                if self.layout_failed.as_deref() != Some(said.as_str()) {
                    self.say(said.clone());
                }
                self.layout_failed = Some(said);
                false
            }
        }
    }

    /// Opens `name`'s input connection if it has none.
    fn inlet(&mut self, name: &str) -> Result<&mut Inlet, Failure> {
        let session = self.session.clone();
        let pane = (self.panes.get_mut(name))
            .ok_or_else(|| Failure::new(Code::Missing, format!("no pane {name}")))?;
        if pane.inlet.is_none() {
            let (conn, _) = dial(&session, &pane.name, Instant::now())?;
            pane.inlet = Some(Inlet::open(conn)?);
        }
        pane.inlet
            .as_mut()
            .ok_or_else(|| Failure::new(Code::Missing, format!("no pane {name}")))
    }

    /// Queues `parts` for the focused pane, on its input connection.
    fn type_parts(&mut self, parts: Vec<Part>) {
        let Some(focus) = self.focus.clone() else {
            return self.say("no pane has the focus");
        };
        if self.live_pane(&focus).is_none() {
            return self.say(format!("pane {} is missing: it takes no keys", focus.name));
        }
        let inlet = match self.inlet(&focus.name) {
            Ok(inlet) => inlet,
            Err(failure) => return self.say(format!("keys not delivered: {failure}")),
        };
        if inlet.parts.len() >= MAX_QUEUED_PARTS {
            return self.say(format!("keys dropped: {} is not taking them", focus.name));
        }
        inlet.parts.extend(parts);
        if let Err(failure) = inlet.advance() {
            self.say(format!("keys not delivered: {failure}"));
        }
    }

    fn type_key(&mut self, press: KeyPress) {
        let name = press.to_string();
        let exact = name.parse::<KeyPress>().is_ok_and(|parsed| parsed == press);
        let part = match (exact, press.key) {
            (true, _) => Part::Key(name),
            (false, fux_vt::keys::Key::Char(c)) => {
                Part::Bytes(c.encode_utf8(&mut [0; 4]).as_bytes().to_vec())
            }
            (false, _) => return self.say(format!("the key {name} cannot be sent")),
        };
        self.type_parts(vec![part]);
    }

    fn input_pressure(&self) -> bool {
        (self.panes.values())
            .filter_map(|pane| pane.inlet.as_ref())
            .any(|inlet| inlet.parts.len() >= MAX_QUEUED_PARTS)
    }

    fn apply_verb(&mut self, verb: Verb) {
        let tabs: Vec<u32> = (self.space())
            .map(|space| space.tabs.iter().map(|t| t.id).collect())
            .unwrap_or_default();
        let spaces: Vec<u32> = self.arranged.workspaces.iter().map(|s| s.id).collect();
        let tab_at = tabs
            .iter()
            .position(|t| *t == self.place().tab)
            .unwrap_or(0);
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
            Verb::LastTab => self.show_tab(self.place().last),
            Verb::GoToTab(number) => match nth(&tabs, number.saturating_sub(1)) {
                Some(tab) => self.show_tab(tab),
                None => self.say(format!(
                    "no tab {number}: this workspace has {}",
                    tabs.len()
                )),
            },
            Verb::ShowTab(tab) => self.show_tab(tab),
            Verb::NextWorkspace | Verb::PreviousWorkspace => {
                let at = stepped(space_at, spaces.len(), verb == Verb::NextWorkspace);
                if let Some(space) = nth(&spaces, at) {
                    self.show_workspace(space);
                }
            }
            Verb::LastWorkspace => self.show_workspace(self.last_workspace),
            Verb::ShowWorkspace(space) => match spaces.contains(&space) {
                true => self.show_workspace(space),
                false => self.say(format!("no workspace w{space}")),
            },
            Verb::NextPane => self.step_pane(true),
            Verb::PreviousPane => self.step_pane(false),
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
            // Handled where the chord is known.
            Verb::Literal => {}
        }
    }

    fn step_pane(&mut self, forward: bool) {
        let visible = self.visible();
        let at = (visible.iter()).position(|p| Some(p) == self.focus.as_ref());
        let next = at.map_or(0, |at| stepped(at, visible.len(), forward));
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
        let Some(rect) = rect.filter(|r| r.w > 0 && r.h >= 2) else {
            return self.say(format!("{} has no room on the screen", pane.name));
        };
        if self.live_pane(pane).is_none() {
            return;
        }
        let tile = tile_of(rect);
        let cols = tile.w.clamp(1, MAX_COLS);
        let rows = tile.h.saturating_sub(1).clamp(1, MAX_ROWS);
        let advanced = self.inlet(&pane.name).and_then(|inlet| {
            inlet.resize = Some((cols, rows));
            inlet.advance()
        });
        if let Err(failure) = advanced {
            self.say(format!("fit: {failure}"));
        }
    }

    /// Fits the panes an edit changed, once the layout has reached it and
    /// each pane has said who it is; one not seen in time is let go.
    fn fit_due(&mut self) {
        let (version, now) = (self.stored.version, Instant::now());
        let fits = std::mem::take(&mut self.fits);
        for (wanted, pane, until) in fits {
            let ready = wanted <= version && self.live_pane(&pane).is_some();
            if ready {
                self.fit(&pane);
            } else if now < until {
                self.fits.push((wanted, pane, until));
            }
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

    fn on_key(&mut self, press: KeyPress) {
        let name = press.to_string();
        if let Some(panel) = &mut self.panel {
            self.dirty = true;
            let id = panel.id();
            match panel.key(press) {
                Outcome::Stay => {}
                Outcome::Cancel => {
                    self.panel = None;
                    self.dispatch_job("the panel".to_owned(), Job::Drop(id));
                }
                Outcome::Chosen(value) => {
                    self.panel = None;
                    self.dispatch_job("the panel".to_owned(), Job::Answer { id, value });
                }
            }
            return;
        }
        let chord = match self.prefix.take() {
            Some(prefix) => {
                self.dirty = true;
                if name == "Escape" {
                    return;
                }
                format!("{prefix} {name}")
            }
            None => name,
        };
        let had_prefix = chord.contains(' ');
        if self.keys.bindings.contains_key(&chord) || self.keys.opens(&chord) {
            return self.perform_chord(chord);
        }
        match had_prefix {
            // The keys are still loading: this chord may be among them.
            true => self.say(format!("no binding for {chord}")),
            false => self.type_key(press),
        }
    }

    /// Does what `chord` is bound to; a chord that holds keys opens them.
    fn perform_chord(&mut self, chord: String) {
        if let Some(binding) = self.keys.bindings.get(&chord).cloned() {
            self.prefix = self.keys.repeating_parent(&chord);
            return self.perform(chord, binding.action);
        }
        if self.keys.opens(&chord) {
            self.prefix = Some(chord);
            self.dirty = true;
            return;
        }
        self.say(format!("no binding for {chord}"));
    }

    fn perform(&mut self, chord: String, action: Action) {
        match action {
            Action::Verb(Verb::Literal) => {
                let first = chord.split(' ').next().unwrap_or_default();
                match first.parse::<KeyPress>() {
                    Ok(press) => self.type_key(press),
                    Err(e) => self.say(e.to_string()),
                }
            }
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
            ToViewer::Bind {
                chord,
                caption,
                heading,
                generation,
            } => {
                self.keys.groups.remove(&chord);
                let action = Action::Script(generation);
                let binding = Binding {
                    action,
                    caption,
                    heading,
                    order: generation,
                };
                self.keys.bindings.insert(chord, binding);
            }
            ToViewer::BindVerb {
                chord,
                caption,
                heading,
                generation,
                verb,
            } => {
                self.keys.groups.remove(&chord);
                let binding = Binding {
                    action: Action::Verb(verb),
                    caption,
                    heading,
                    order: generation,
                };
                self.keys.bindings.insert(chord, binding);
            }
            ToViewer::Group {
                chord,
                caption,
                heading,
                repeat,
                generation,
            } => {
                self.keys.bindings.remove(&chord);
                let group = Group {
                    caption,
                    heading,
                    repeat,
                    order: generation,
                };
                self.keys.groups.insert(chord, group);
            }
            ToViewer::Unbind(chord) => {
                self.keys.bindings.remove(&chord);
                self.keys.groups.remove(&chord);
            }
            ToViewer::Focus(pane) => {
                let (name, id) = match pane.split_once('@') {
                    Some((name, id)) => (name.to_owned(), PaneId::parse(id)),
                    None => (pane, None),
                };
                if let Some(focus) = self.focus.clone() {
                    self.back = Some((name.clone(), focus));
                }
                self.wanted = Some((name, id, after(now, FOCUS_WAIT)));
                self.focus_moved = true;
                // It may have just been started: look now, not at the next scan.
                self.next_scan = now;
            }
            ToViewer::Verb(verb) => self.apply_verb(verb),
            ToViewer::Pick { id, title, items } => {
                self.prefix = None;
                self.panel = Some(Panel::Pick(Pick {
                    id,
                    title,
                    items,
                    filter: String::new(),
                    selected: 0,
                }));
            }
            ToViewer::Ask {
                id,
                prompt,
                initial,
            } => {
                self.prefix = None;
                let text: Vec<char> = initial.chars().collect();
                self.panel = Some(Panel::Ask(Ask {
                    id,
                    prompt,
                    cursor: text.len(),
                    text,
                }));
            }
            ToViewer::Reread => self.next_scan = now,
            ToViewer::Fit { version, panes } => {
                for pane in panes.iter().filter_map(|p| PaneRef::parse(p).ok()) {
                    match self.fits.len() < MAX_FITS {
                        true => self.fits.push((version, pane, after(now, FOCUS_WAIT))),
                        false => {
                            self.say(format!("too many panes to fit: {} not fitted", pane.name))
                        }
                    }
                }
                self.next_scan = now;
            }
            ToViewer::Say(text) => self.say(text),
            ToViewer::Started { stop, limit } => {
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
                    self.say(why);
                }
                // Held keys go once a pane can take them: the panes may not
                // have greeted yet.
                if self.loading && self.keys_loaded.is_none() {
                    self.keys_loaded = Some(after(now, HOLD_FOR_FOCUS));
                }
                if let Some(run) = self.waiting_runs.pop_front() {
                    self.start_run(run);
                }
            }
        }
    }

    /// Leaving: a binding still running is stopped, and what it started is
    /// ended with it, a bounded time, so no `fux2` it ran outlives the viewer.
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
                Ok(ToViewer::Started { stop, .. }) => {
                    stop.force();
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Lets held input go once the keys have loaded and a pane has the focus,
    /// or the wait for one is over.
    fn release_held(&mut self, now: Instant) {
        let Some(until) = self.keys_loaded else {
            return;
        };
        if self.focus.is_some() || now >= until {
            self.keys_loaded = None;
            self.loading = false;
            let held = std::mem::take(&mut self.held);
            self.on_inputs(held);
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
            // The keys are still loading, and may bind what was typed: it
            // waits, in order, the load's own time at most. The way out
            // does not wait.
            if self.loading {
                let key = match &input {
                    Input::Key(k) => Some(k.press.to_string()),
                    Input::Paste(_)
                    | Input::PasteTooLong
                    | Input::FocusIn
                    | Input::FocusOut
                    | Input::Mouse(_)
                    | Input::Reply(_) => None,
                };
                let prefixed =
                    std::mem::replace(&mut self.loading_prefix, key.as_deref() == Some("C-b"));
                if prefixed && key.as_deref() == Some("d") {
                    self.quit = true;
                } else if self.held.len() < MAX_HELD_KEYS {
                    self.held.push(input);
                } else {
                    self.say("keys typed while the keys load were dropped");
                }
                continue;
            }
            match input {
                Input::Key(stroke) => self.on_key(stroke.press),
                // Typed into an open panel, never a pane.
                Input::Paste(text) if self.panel.is_some() => {
                    if let Some(panel) = &mut self.panel {
                        panel.paste(&text);
                    }
                    self.dirty = true;
                }
                Input::Paste(text) if text.len() <= MAX_PASTE => {
                    self.type_parts(vec![Part::Paste(text)]);
                }
                Input::Paste(_) | Input::PasteTooLong => {
                    self.say(format!("a paste over {MAX_PASTE} bytes was dropped"));
                }
                Input::Mouse(event) if self.panel.is_none() => self.on_mouse(event),
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
                    .find(|b| on_border(b, at));
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
                    self.preview();
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

    /// Shows the drag as it would land, from the stored layout.
    fn preview(&mut self) {
        let Some(drag) = &self.drag else {
            return;
        };
        let Some((pane, dir, cells)) = drag_edit(drag) else {
            self.arranged = Arc::new(self.stored.shown(&self.live));
            self.dirty = true;
            return;
        };
        let mut copy = self.stored.clone();
        let op = Op::Border {
            pane: Target {
                name: pane.name.clone(),
                id: Some(pane.id),
            },
            dir,
            amount: Amount::Cells(cells),
        };
        if layout::apply(&mut copy, &self.live, self.area(), &op).is_ok() {
            self.arranged = Arc::new(copy.shown(&self.live));
            self.dirty = true;
        }
    }

    /// Serves both connections of `name`: false once the pane is gone.
    fn pump(&mut self, name: &str, now: Instant) -> bool {
        let mut said = None;
        let mut changed = false;
        let mut greeted = false;
        let Some(pane) = self.panes.get_mut(name) else {
            return true;
        };
        let mut alive = pane.conn.flush().is_ok();
        while alive {
            match pane.conn.receive() {
                Ok(Some(body)) => {
                    let was_greeting = pane.phase == Phase::Greeting;
                    match pane.answered(&body, now) {
                        Ok(screen) => changed |= screen,
                        Err(failure) => {
                            said = Some(format!("{name}: {failure}"));
                            alive = false;
                        }
                    }
                    greeted |= was_greeting && pane.phase != Phase::Greeting;
                }
                Ok(None) => break,
                Err(_) => alive = false,
            }
            alive = alive && pane.conn.flush().is_ok();
        }
        if let Some(inlet) = &mut pane.inlet {
            let mut lost = inlet.conn.flush().is_err();
            while !lost {
                match inlet.conn.receive() {
                    Ok(Some(body)) => {
                        inlet.busy = false;
                        match Answer::decode(&body).and_then(Answer::accepted) {
                            Ok(Answer::Hello(hello)) if hello.id == pane.hello.id => {
                                inlet.greeted = true;
                            }
                            // The name has passed to another pane: nothing
                            // typed for this one may reach it.
                            Ok(Answer::Hello(_)) => lost = true,
                            Ok(_) => {}
                            Err(failure) => {
                                // Refused before it was greeted, it never will be.
                                lost |= !inlet.greeted;
                                said = Some(format!("{name}: {failure}"));
                            }
                        }
                        lost = lost || inlet.advance().is_err();
                    }
                    Ok(None) => break,
                    Err(_) => lost = true,
                }
                lost = lost || inlet.conn.flush().is_err();
            }
            if lost {
                let queued = inlet.parts.len();
                pane.inlet = None;
                if queued > 0 {
                    said = Some(format!("{name}: {queued} keys were not delivered"));
                }
            }
        }
        self.dirty |= changed;
        // A pane that just said who it is may be routed, or be the one to focus.
        if greeted {
            self.refresh(false);
            self.fit_due();
        }
        if let Some(text) = said {
            self.say(text);
        }
        alive
    }

    fn retry_rested(&mut self, now: Instant) {
        for pane in self.panes.values_mut() {
            if pane.retry.is_some_and(|at| now >= at) {
                pane.retry = None;
                let asked = match pane.token {
                    Some(_) => pane.wait_or_rest(),
                    None => pane.read_screen(),
                };
                drop(asked);
                let _ = pane.conn.flush();
            }
        }
    }

    /// Composes the screen and hands what changed to the painter.
    fn paint(&mut self, now: Instant) {
        self.dirty = false;
        self.next_paint = after(now, PAINT);
        if self
            .message
            .as_ref()
            .is_some_and(|(_, until)| now >= *until)
        {
            self.message = None;
        }
        let (cols, rows) = self.term;
        let mut frame = Frame::new(cols, rows);
        let area = tile_of(self.area());
        let titles: Vec<(PaneRef, String)> = (self.shown_tab())
            .map(|tab| {
                tab.leaves()
                    .into_iter()
                    .map(|l| (l.pane.clone(), l.title.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let tiles = self.tiles();
        let mut cursor = None;
        for (pane, rect) in &tiles {
            let tile = tile_of(*rect);
            let focused = Some(pane) == self.focus.as_ref();
            let sgr = if focused { ";7" } else { ";2" };
            let own = (titles.iter())
                .find(|(p, _)| p == pane)
                .map(|(_, title)| title.as_str())
                .unwrap_or_default();
            let title = match self.live_pane(pane) {
                Some(live) => live.title(tile, own),
                None => format!(" {} [missing] ", pane.name),
            };
            frame.fill(tile.x, tile.y, tile.w, "\u{2500}", sgr);
            frame.label(tile.x, tile.y, tile.w, &title, sgr);
            if tile.x > 0 {
                let border = tile.x.saturating_sub(1);
                for y in tile.y..tile.y.saturating_add(tile.h) {
                    frame.fill(border, y, 1, "\u{2502}", ";2");
                }
            }
            if let Some(live) = self.live_pane(pane)
                && let Some(meta) = &live.meta
            {
                let at = frame.pane(tile, &live.rows, meta);
                if focused {
                    cursor = at.map(|at| (at, meta.cursor_shape));
                }
            }
        }
        let all = self
            .shown_tab()
            .map_or(0, |tab| tab.tiles(self.area()).len());
        if all == 0 {
            let text = "no panes here: fux2 start --name NAME, or C-b c";
            frame.label(1, area.h.checked_div(2).unwrap_or(0), area.w, text, ";2");
        }
        if let Some(prefix) = &self.prefix {
            let lines = self.keys.menu(prefix);
            panel::draw_menu(&mut frame, area, prefix, &lines);
            cursor = None;
        }
        if let Some(open) = &self.panel {
            open.draw(&mut frame, area);
            cursor = None;
        }
        let hidden = all.saturating_sub(tiles.len());
        let status = self.status_line(hidden);
        let last = rows.saturating_sub(1);
        frame.fill(0, last, area.w, " ", ";7");
        frame.label(0, last, area.w, &status, ";7");
        let mut bytes = b"\x1b[?25l".to_vec();
        bytes.extend(frame.diff(&self.drawn));
        bytes.extend_from_slice(b"\x1b[0m");
        if let Some(((x, y), shape)) = cursor {
            let (row, col) = (y.saturating_add(1), x.saturating_add(1));
            let shape = if shape == 7 { 0 } else { shape };
            bytes.extend_from_slice(format!("\x1b[{shape} q\x1b[{row};{col}H\x1b[?25h").as_bytes());
        }
        if self.painter.send(bytes) {
            self.drawn = frame;
        } else {
            self.dirty = true;
        }
    }

    fn status_line(&self, hidden: usize) -> String {
        let mut line = String::new();
        let spaces = &self.arranged.workspaces;
        let space_at = spaces
            .iter()
            .position(|s| s.id == self.workspace)
            .unwrap_or(0);
        let tabs = self.space().map_or(&[][..], |space| space.tabs.as_slice());
        let tab_at = tabs
            .iter()
            .position(|t| t.id == self.place().tab)
            .unwrap_or(0);
        line.push_str(&format!(
            " {} {}/{} | {} {}/{} ",
            self.space().map_or("", |space| space.title.as_str()),
            space_at.saturating_add(1),
            spaces.len(),
            self.shown_tab().map_or("", |tab| tab.title.as_str()),
            tab_at.saturating_add(1),
            tabs.len(),
        ));
        let zoomed =
            (self.shown_tab()).is_some_and(|tab| tab.zoom.as_ref().is_some_and(|z| tab.holds(z)));
        if zoomed {
            line.push_str("| zoomed ");
        }
        if let Some(prefix) = &self.prefix {
            line.push_str(&format!("| {prefix} … "));
        }
        if hidden > 0 {
            line.push_str(&format!("| {hidden} panes do not fit "));
        }
        if self.input_pressure() {
            line.push_str("| keys are waiting for a pane ");
        }
        if self.running.is_some() {
            line.push_str("| a binding is running ");
        }
        if let Some((text, _)) = &self.message {
            line.push_str("| ");
            line.push_str(text);
        }
        line
    }

    fn timeout(&self, now: Instant, decoder: &Decoder) -> Duration {
        let mut at = self.next_scan;
        let mut consider = |other: Instant| at = at.min(other);
        if let Some(until) = self.keys_loaded {
            consider(until);
        }
        if self.dirty && self.painter.is_idle() {
            consider(self.next_paint);
        }
        if let Some(running) = &self.running {
            consider(running.at);
        }
        if let Some((_, until)) = &self.message {
            consider(*until);
        }
        if let Some(due) = decoder.deadline() {
            consider(due);
        }
        for pane in self.panes.values() {
            if let Some(retry) = pane.retry {
                consider(retry);
            }
        }
        at.saturating_duration_since(now)
    }
}

/// Whether the cell `at` is on `border`.
fn on_border(border: &layout::Border, (x, y): (u16, u16)) -> bool {
    let (x, y) = (u32::from(x), u32::from(y));
    let end = border.from.saturating_add(border.length);
    match border.axis {
        layout::Axis::Columns => x == border.at && (border.from..end).contains(&y),
        layout::Axis::Rows => y == border.at && (border.from..end).contains(&x),
    }
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
        view.refresh(true);
        return view.click(drag.from);
    };
    let way = match dir {
        Dir::Left => "left",
        Dir::Right => "right",
        Dir::Up => "up",
        Dir::Down => "down",
    };
    let args = vec![
        pane.to_string(),
        way.to_owned(),
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

/// Raises the limit on open files far enough for a full session, or says
/// how far it got: panes past that are refused with the numbers.
fn file_limit() -> Result<Option<u64>, Failure> {
    let panes = u64::try_from(dir::MAX_PANES).unwrap_or(u64::MAX);
    let need = panes
        .saturating_mul(FILES_PER_PANE)
        .saturating_add(BASE_FILES);
    fuxix::io::raise_open_files(need)
        .map(Some)
        .map_err(|e| io_failure("the limit on open files", e))
}

fn fux2_program() -> Result<PathBuf, Failure> {
    let exe = std::env::current_exe().map_err(|e| io_failure("finding this program", e))?;
    Ok(exe.with_file_name("fux2"))
}

fn parse_args() -> Result<(), Failure> {
    match std::env::args_os().nth(1) {
        None => Ok(()),
        Some(arg) => {
            let why = format!("unexpected argument: {}", arg.to_string_lossy());
            Err(Failure::new(Code::Invalid, why))
        }
    }
}

fn run() -> Result<(), Failure> {
    parse_args()?;
    if !fuxix::terminal::on_one_terminal() {
        let why = "the viewer needs one terminal on stdin and stdout";
        return Err(Failure::new(Code::Invalid, why));
    }
    let session = Session::open()?;
    let file_limit = file_limit()?;
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
    let script = script::start(config, fux2_program()?, to_viewer, script_poke)
        .map_err(|e| io_failure("a thread", e))?;
    let raw =
        fuxix::terminal::Raw::enter(std::io::stdin()).map_err(|e| io_failure("raw mode", e))?;
    // An abort still leaves the screen it found.
    fuxix::terminal::leaving_on_abort(LEAVE);
    let now = Instant::now();
    let mut view = View {
        session,
        stored: Layout::default(),
        layout_text: None,
        layout_failed: None,
        arranged: Arc::new(Layout::default()),
        live: Vec::new(),
        workspace: 0,
        last_workspace: 0,
        places: BTreeMap::new(),
        panes: BTreeMap::new(),
        focus: None,
        focus_name: None,
        last_focus: None,
        wanted: None,
        back: None,
        term: terminal_size(),
        keys: first_keys(),
        prefix: None,
        panel: None,
        drag: None,
        fits: Vec::new(),
        message: None,
        file_limit,
        script: Some(script),
        from_script,
        running: None,
        waiting_runs: VecDeque::new(),
        loading: true,
        held: Vec::new(),
        loading_prefix: false,
        focus_moved: false,
        keys_loaded: None,
        run_sent: false,
        painter,
        drawn: Frame::default(),
        dirty: true,
        next_paint: now,
        next_scan: now,
        quit: false,
    };
    view.refresh(true);
    if let Some(note) = note {
        view.say(note);
    }
    view.painter.send(ENTER.to_vec());
    let ended = serve(&mut view, &signals, &paint_wake, &script_wake);
    view.end_run();
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
    panes: Vec<(String, bool)>,
}

fn wait(
    view: &View,
    poll: &mut Poll,
    timeout: Duration,
    sources: (&fuxix::signal::Pipe, &Wake, &Wake),
    read_keys: bool,
) -> Result<Ready, Failure> {
    let (signals, paint, script) = sources;
    let stdin = std::io::stdin();
    let mut fds = poll.list();
    let keys = read_keys.then(|| fds.push(PollFd::new(&stdin, Events::IN)));
    let signal_entry = fds.push(PollFd::new(signals.reader(), Events::IN));
    let paint_entry = fds.push(PollFd::new(paint, Events::IN));
    let script_entry = fds.push(PollFd::new(script, Events::IN));
    let mut entries = Vec::new();
    for pane in view.panes.values() {
        let watch = fds.push(PollFd::new(&pane.conn.stream, pane.conn.events()));
        let inlet = (pane.inlet.as_ref())
            .map(|inlet| fds.push(PollFd::new(&inlet.conn.stream, inlet.conn.events())));
        entries.push((pane.name.clone(), watch, inlet));
    }
    match fds.poll(Some(timeout)) {
        Ok(_) | Err(Errno::INTR) => {}
        Err(errno) => return Err(io_failure("poll", errno)),
    }
    let keys_ready = keys.is_some_and(|entry| !fds.events(entry).is_empty());
    let panes = entries
        .into_iter()
        .map(|(name, watch, inlet)| {
            let inlet = inlet.is_some_and(|entry| !fds.events(entry).is_empty());
            (name, inlet || !fds.events(watch).is_empty())
        })
        .collect();
    Ok(Ready {
        stdin: keys_ready,
        signals: !fds.events(signal_entry).is_empty(),
        paint: !fds.events(paint_entry).is_empty(),
        script: !fds.events(script_entry).is_empty(),
        panes,
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
    let mut buffer = vec![0u8; 16 << 10];
    let stdin = std::io::stdin();
    while !view.quit {
        let now = Instant::now();
        if now >= view.next_scan {
            view.rescan(now);
        }
        view.retry_rested(now);
        view.release_held(now);
        view.police_run(now);
        if view
            .message
            .as_ref()
            .is_some_and(|(_, until)| now >= *until)
        {
            view.message = None;
            view.dirty = true;
        }
        if view.dirty && view.painter.is_idle() && now >= view.next_paint {
            view.paint(now);
        }
        let read_keys = true;
        let timeout = view.timeout(Instant::now(), &decoder);
        let sources = (signals, paint_wake, script_wake);
        let ready = wait(view, &mut poll, timeout, sources, read_keys)?;
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
        let mut gone = false;
        for (name, active) in ready.panes {
            if active && !view.pump(&name, now) {
                view.panes.remove(&name);
                gone = true;
            }
        }
        if gone {
            view.dirty = true;
            view.refresh(false);
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            let mut err = std::io::stderr();
            let _ = writeln!(err, "fux2 view: {failure}");
            ExitCode::from(failure.code.exit())
        }
    }
}

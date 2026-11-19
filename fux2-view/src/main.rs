//! The fux2 viewer: shows a session's panes, split, in this terminal, and
//! sends keys to the focused one. It owns nothing: end it however you like
//! and every pane goes on.
mod layout;
mod render;
mod script;

use fux_vt::keys::KeyPress;
use fux_vt::keys::decode::{Decoder, Input, Timing};
use fux2::client::{check_hello, dial};
use fux2::conn::Conn;
use fux2::dir::{self, Session};
use fux2::model::apply;
use fux2::wire::{
    Answer, Code, Failure, Hello, MAX_COLS, MAX_PARTS, MAX_ROWS, Meta, PaneId, Part, REVISION,
    Request, Row, State, Token,
};
use fuxix::Errno;
use fuxix::poll::{Events, Poke, Poll, PollFd, Wake, waker};
use fuxix::process::Signal;
use layout::{Tab, Workspace};
use render::{Frame, Tile};
use script::{Run, Script, ToViewer, Verb};
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
/// How long leaving waits for a binding to be stopped.
const END_RUN_WAIT: Duration = Duration::from_secs(1);
/// How long a focus asked for waits for its pane.
const FOCUS_WAIT: Duration = Duration::from_secs(10);
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

const ENTER: &[u8] = b"\x1b[?1049h\x1b[?2004h\x1b[?25l\x1b[2J";
const LEAVE: &str = "\x1b[0m\x1b[0 q\x1b[?25h\x1b[?2004l\x1b[?1049l";

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

/// A binding's action.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Verb(Verb),
    /// Runs the configuration's function bound under this number.
    Script(u64),
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
        if self.send(bytes) {
            wait_idle(LEAVE_WAIT);
        }
    }
}

/// The binding running now.
struct Running {
    stop: grift::Interrupter,
    at: Instant,
    forced: bool,
}

/// Where a workspace was left.
#[derive(Clone, Copy, Default)]
struct Place {
    tab: usize,
    last: usize,
}

struct View {
    session: Session,
    layout_path: Option<PathBuf>,
    layout_text: Option<String>,
    workspaces: Vec<Workspace>,
    /// The workspace shown, and the one before it.
    workspace: usize,
    last_workspace: usize,
    /// For each workspace, its tab shown and the one before it.
    places: Vec<Place>,
    panes: BTreeMap<String, Pane>,
    focus: Option<PaneId>,
    focus_name: Option<String>,
    /// A pane to focus as soon as it is there: its name, its id if known,
    /// and until when it is waited for.
    wanted: Option<(String, Option<PaneId>, Instant)>,
    /// That pane, and what had the focus before it: it comes back there.
    back: Option<(String, PaneId)>,
    term: (u16, u16),
    bindings: BTreeMap<String, Action>,
    prefix: Option<String>,
    message: Option<(String, Instant)>,
    file_limit: Option<u64>,
    script: Option<Script>,
    from_script: Receiver<ToViewer>,
    running: Option<Running>,
    waiting_runs: VecDeque<Run>,
    /// The configuration is still being read: keys it might bind wait.
    loading: bool,
    /// The layout file's last reading was refused.
    layout_failed: bool,
    held: Vec<Input>,
    /// A run was handed to the language and has not finished.
    run_sent: bool,
    painter: Painter,
    shown: Frame,
    tiles: Vec<(String, Tile)>,
    dirty: bool,
    next_paint: Instant,
    next_scan: Instant,
    quit: bool,
}

/// What holds before the default keys have loaded: a way out.
fn first_bindings() -> BTreeMap<String, Action> {
    BTreeMap::from([("C-b d".to_owned(), Action::Verb(Verb::Detach))])
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

    fn title(&self, tile: Tile) -> String {
        let mut title = format!(" {} ", self.name);
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

    fn tabs(&self) -> &[Tab] {
        (self.workspaces.get(self.workspace)).map_or(&[], |space| space.tabs.as_slice())
    }

    fn place(&self) -> Place {
        self.places.get(self.workspace).copied().unwrap_or_default()
    }

    fn shown_tab(&self) -> Option<&Tab> {
        self.tabs().get(self.place().tab)
    }

    /// Shows tab `tab` of this workspace, remembering the one it leaves.
    fn show_tab(&mut self, tab: usize) {
        let workspace = self.workspace;
        if let Some(place) = self.places.get_mut(workspace)
            && place.tab != tab
        {
            *place = Place {
                tab,
                last: place.tab,
            };
            self.tab_changed();
        }
    }

    fn show_workspace(&mut self, workspace: usize) {
        if workspace != self.workspace && workspace < self.workspaces.len() {
            self.last_workspace = self.workspace;
            self.workspace = workspace;
            self.tab_changed();
        }
    }

    fn tab_changed(&mut self) {
        self.dirty = true;
        self.settle_focus();
    }

    /// The labels a new pane must carry to appear in the shown tab.
    fn labels_for_new_pane(&self) -> Result<Vec<(String, String)>, String> {
        let tests = self.shown_tab().map_or(&[][..], |tab| tab.tests.as_slice());
        if tests.iter().any(|(key, _)| key == "name") {
            return Err("this tab selects panes by name, so no label puts a new pane in it".into());
        }
        Ok(tests.to_vec())
    }

    fn visible(&self) -> Vec<&Pane> {
        let tests = self.shown_tab().map_or(&[][..], |tab| tab.tests.as_slice());
        (self.panes.values())
            .filter(|pane| pane.phase != Phase::Greeting)
            .filter(|pane| dir::matches(&pane.name, &pane.hello.labels, tests))
            .collect()
    }

    /// Keeps the focus on a pane of this tab. A pane asked for takes it when
    /// it appears; when that pane goes, the focus returns to where it was;
    /// any other pane that went gives way to the next by name.
    fn settle_focus(&mut self) {
        let chosen = {
            let visible = self.visible();
            let named = |name: &str| visible.iter().find(|p| p.name == name);
            let by_id = |id: PaneId| visible.iter().find(|p| p.hello.id == id);
            let wanted = (self.wanted.as_ref())
                .filter(|(_, _, until)| Instant::now() < *until)
                .and_then(|(name, id, _)| {
                    named(name).filter(|pane| id.is_none_or(|id| pane.hello.id == id))
                });
            if wanted.is_none() && self.focus.and_then(by_id).is_some() {
                return;
            }
            let returning = (self.back.as_ref())
                .filter(|(name, _)| self.focus_name.as_deref() == Some(name.as_str()))
                .and_then(|(_, id)| by_id(*id));
            let last = self.focus_name.as_deref().unwrap_or("");
            wanted
                .or(returning)
                .or_else(|| visible.iter().find(|p| p.name.as_str() > last))
                .or_else(|| visible.first())
                .map(|p| (p.hello.id, p.name.clone()))
        };
        if let Some((_, name)) = &chosen
            && self.wanted.as_ref().is_some_and(|w| w.0 == *name)
        {
            self.wanted = None;
        } else if chosen.is_some() {
            self.back = None;
        }
        let focus = chosen.as_ref().map(|c| c.0);
        self.dirty |= focus != self.focus;
        self.focus = focus;
        self.focus_name = chosen.map(|c| c.1).or_else(|| self.focus_name.take());
    }

    fn focused_name(&self) -> Option<String> {
        let id = self.focus?;
        (self.panes.values())
            .find(|pane| pane.hello.id == id)
            .map(|pane| pane.name.clone())
    }

    fn rescan(&mut self, now: Instant) {
        self.next_scan = after(now, RESCAN);
        self.reread_layout();
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
        self.settle_focus();
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

    fn reread_layout(&mut self) {
        let Some(path) = self.layout_path.clone() else {
            return;
        };
        let text = match script::read_small_file(&path, layout::MAX_LAYOUT_BYTES) {
            Ok(text) => text,
            Err(e) => return self.say(format!("{}: {e}", path.display())),
        };
        if self.layout_text.as_deref() == Some(text.as_str()) {
            return;
        }
        match layout::parse(&text) {
            Ok(workspaces) => {
                // The file is good again: what it said wrong is no longer so.
                if std::mem::take(&mut self.layout_failed) {
                    self.message = None;
                }
                self.places.resize(workspaces.len(), Place::default());
                for (place, space) in self.places.iter_mut().zip(&workspaces) {
                    let most = space.tabs.len().saturating_sub(1);
                    place.tab = place.tab.min(most);
                    place.last = place.last.min(most);
                }
                let most = workspaces.len().saturating_sub(1);
                self.workspace = self.workspace.min(most);
                self.last_workspace = self.last_workspace.min(most);
                self.workspaces = workspaces;
                self.tab_changed();
            }
            // The last good layout stays on the screen.
            Err(why) => {
                self.layout_failed = true;
                let file = path.file_name().unwrap_or(path.as_os_str());
                self.say(format!("{}: {why}", file.to_string_lossy()));
            }
        }
        self.layout_text = Some(text);
    }

    fn focused_pane(&mut self) -> Option<&mut Pane> {
        let id = self.focus?;
        self.panes.values_mut().find(|pane| pane.hello.id == id)
    }

    /// Queues `parts` for the focused pane, on its input connection.
    fn type_parts(&mut self, parts: Vec<Part>) {
        let session = self.session.clone();
        let Some(pane) = self.focused_pane() else {
            return self.say("no pane has the focus");
        };
        let opened = match pane.inlet.is_some() {
            true => Ok(()),
            false => dial(&session, &pane.name, Instant::now())
                .and_then(|(conn, _)| Inlet::open(conn))
                .map(|inlet| pane.inlet = Some(inlet)),
        };
        if let Err(failure) = opened {
            return self.say(format!("keys not delivered: {failure}"));
        }
        let full = (pane.inlet.as_ref()).is_some_and(|inlet| inlet.parts.len() >= MAX_QUEUED_PARTS);
        if full {
            let name = pane.name.clone();
            return self.say(format!("{name} is not taking keys: they are dropped"));
        }
        let advanced = pane.inlet.as_mut().map_or(Ok(()), |inlet| {
            inlet.parts.extend(parts);
            inlet.advance()
        });
        if let Err(failure) = advanced {
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
        let (tab, tabs) = (self.place().tab, self.tabs().len());
        let spaces = self.workspaces.len();
        match verb {
            Verb::Detach => self.quit = true,
            Verb::NextTab => self.show_tab(stepped(tab, tabs, true)),
            Verb::PreviousTab => self.show_tab(stepped(tab, tabs, false)),
            Verb::LastTab => self.show_tab(self.place().last),
            Verb::GoToTab(number) => {
                let index = number.saturating_sub(1);
                match index < tabs {
                    true => self.show_tab(index),
                    false => self.say(format!("no tab {number}: this workspace has {tabs}")),
                }
            }
            Verb::NextWorkspace => self.show_workspace(stepped(self.workspace, spaces, true)),
            Verb::PreviousWorkspace => self.show_workspace(stepped(self.workspace, spaces, false)),
            Verb::LastWorkspace => self.show_workspace(self.last_workspace),
            Verb::NextPane => self.step_pane(true),
            Verb::PreviousPane => self.step_pane(false),
            Verb::Fit => self.fit(),
            // Handled where the chord is known.
            Verb::Literal => {}
        }
    }

    fn step_pane(&mut self, forward: bool) {
        let (focus, name) = {
            let visible = self.visible();
            let at = visible.iter().position(|p| Some(p.hello.id) == self.focus);
            let next = at.map_or(0, |at| stepped(at, visible.len(), forward));
            let pane = visible.get(next);
            (pane.map(|p| p.hello.id), pane.map(|p| p.name.clone()))
        };
        self.focus = focus;
        self.focus_name = name;
        self.back = None;
        self.dirty = true;
    }

    /// Asks the focused pane, once, to take its tile's size.
    fn fit(&mut self) {
        let Some(name) = self.focused_name() else {
            return self.say("no pane has the focus");
        };
        let tile = (self.tiles.iter())
            .find(|(tiled, _)| *tiled == name)
            .map(|t| t.1);
        let Some(tile) = tile else {
            return self.say("the focused pane has no room on the screen");
        };
        let cols = tile.w.clamp(1, MAX_COLS);
        let rows = tile.h.saturating_sub(1).clamp(1, MAX_ROWS);
        self.type_parts(Vec::new());
        if let Some(inlet) = self.focused_pane().and_then(|p| p.inlet.as_mut()) {
            inlet.resize = Some((cols, rows));
            if let Err(failure) = inlet.advance() {
                self.say(format!("fit: {failure}"));
            }
        }
    }

    fn focus_label(&mut self) -> Option<String> {
        let pane = self.focused_pane()?;
        Some(format!("{}@{}", pane.name, pane.hello.id))
    }

    fn on_key(&mut self, press: KeyPress) {
        let name = press.to_string();
        if let Some(prefix) = self.prefix.take() {
            let chord = format!("{prefix} {name}");
            self.dirty = true;
            return match self.bindings.get(&chord).cloned() {
                Some(action) => self.perform(chord, action),
                // The keys are still loading: this chord may be among them.
                None => self.say(format!("no binding for {chord}")),
            };
        }
        if let Some(action) = self.bindings.get(&name).cloned() {
            return self.perform(name, action);
        }
        let opens = format!("{name} ");
        if self.bindings.keys().any(|key| key.starts_with(&opens)) {
            self.prefix = Some(name);
            self.dirty = true;
            return;
        }
        self.type_key(press);
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
                // The pane focused now, whatever the binding does later.
                let focus = self.focus_label();
                let labels = self.labels_for_new_pane();
                self.dispatch(Run {
                    chord,
                    generation,
                    focus,
                    labels,
                });
            }
        }
    }

    fn dispatch(&mut self, run: Run) {
        if !self.run_sent && self.waiting_runs.is_empty() {
            self.start_run(run);
        } else if self.waiting_runs.len() < MAX_WAITING_RUNS {
            self.waiting_runs.push_back(run);
        } else {
            self.say(format!("too many bindings waiting: {} dropped", run.chord));
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
        match message {
            ToViewer::Bind(chord, generation) => {
                self.bindings.insert(chord, Action::Script(generation));
            }
            ToViewer::BindVerb(chord, verb) => {
                self.bindings.insert(chord, Action::Verb(verb));
            }
            ToViewer::Focus(pane) => {
                let (name, id) = match pane.split_once('@') {
                    Some((name, id)) => (name.to_owned(), PaneId::parse(id)),
                    None => (pane, None),
                };
                if let Some(id) = self.focus {
                    self.back = Some((name.clone(), id));
                }
                self.wanted = Some((name, id, after(now, FOCUS_WAIT)));
                // It has just been started: look now, not at the next scan.
                self.next_scan = now;
            }
            ToViewer::Unbind(chord) => {
                self.bindings.remove(&chord);
            }
            ToViewer::Verb(verb) => self.apply_verb(verb),
            ToViewer::Started { stop, limit } => {
                self.dirty = true;
                self.running = Some(Running {
                    stop,
                    at: after(now, limit),
                    forced: false,
                });
            }
            ToViewer::Done(why) => {
                self.running = None;
                self.run_sent = false;
                self.dirty = true;
                if let Some(why) = why {
                    self.say(why);
                }
                if std::mem::take(&mut self.loading) {
                    let held = std::mem::take(&mut self.held);
                    self.on_inputs(held);
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
                let named = |input: Option<&Input>, name: &str| matches!(input, Some(Input::Key(k)) if k.press.to_string() == name);
                if named(Some(&input), "d") && named(self.held.last(), "C-b") {
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
                Input::Paste(text) if text.len() <= MAX_PASTE => {
                    self.type_parts(vec![Part::Paste(text)]);
                }
                Input::Paste(_) | Input::PasteTooLong => {
                    self.say(format!("a paste over {MAX_PASTE} bytes was dropped"));
                }
                Input::FocusIn | Input::FocusOut | Input::Mouse(_) | Input::Reply(_) => {}
            }
        }
    }

    /// Serves both connections of `name`: false once the pane is gone.
    fn pump(&mut self, name: &str, now: Instant) -> bool {
        let mut said = None;
        let mut changed = false;
        let Some(pane) = self.panes.get_mut(name) else {
            return true;
        };
        let mut alive = pane.conn.flush().is_ok();
        while alive {
            match pane.conn.receive() {
                Ok(Some(body)) => match pane.answered(&body, now) {
                    Ok(screen) => changed |= screen,
                    Err(failure) => {
                        said = Some(format!("{name}: {failure}"));
                        alive = false;
                    }
                },
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
        // A pane that just said who it is may be the one to focus.
        self.settle_focus();
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
        let area = Tile {
            x: 0,
            y: 0,
            w: cols.min(render::MAX_COLS),
            h: rows.min(render::MAX_ROWS).saturating_sub(1),
        };
        let axis = self.shown_tab().map_or(layout::Axis::Auto, |t| t.axis);
        let visible = self.visible();
        let tiles = render::split(area, visible.len(), axis);
        let mut cursor = None;
        let mut placed = Vec::new();
        for (pane, tile) in visible.iter().zip(&tiles) {
            let focused = Some(pane.hello.id) == self.focus;
            let sgr = if focused { ";7" } else { ";2" };
            frame.fill(tile.x, tile.y, tile.w, "\u{2500}", sgr);
            frame.label(tile.x, tile.y, tile.w, &pane.title(*tile), sgr);
            if tile.x > 0 {
                let border = tile.x.saturating_sub(1);
                for y in tile.y..tile.y.saturating_add(tile.h) {
                    frame.fill(border, y, 1, "\u{2502}", ";2");
                }
            }
            if let Some(meta) = &pane.meta {
                let at = frame.pane(*tile, &pane.rows, meta);
                if focused {
                    cursor = at.map(|at| (at, meta.cursor_shape));
                }
            }
            placed.push((pane.name.clone(), *tile));
        }
        if visible.is_empty() {
            let text = "no panes here: fux2 start --name NAME";
            frame.label(1, area.h.checked_div(2).unwrap_or(0), area.w, text, ";2");
        }
        let hidden = visible.len().saturating_sub(tiles.len());
        let status = self.status_line(hidden);
        let last = rows.saturating_sub(1);
        frame.fill(0, last, area.w, " ", ";7");
        frame.label(0, last, area.w, &status, ";7");
        self.tiles = placed;
        let mut bytes = b"\x1b[?25l".to_vec();
        bytes.extend(frame.diff(&self.shown));
        bytes.extend_from_slice(b"\x1b[0m");
        if let Some(((x, y), shape)) = cursor {
            let (row, col) = (y.saturating_add(1), x.saturating_add(1));
            let shape = if shape == 7 { 0 } else { shape };
            bytes.extend_from_slice(format!("\x1b[{shape} q\x1b[{row};{col}H\x1b[?25h").as_bytes());
        }
        if self.painter.send(bytes) {
            self.shown = frame;
        } else {
            self.dirty = true;
        }
    }

    fn status_line(&self, hidden: usize) -> String {
        let mut line = String::new();
        let place = self.place();
        let space = self.workspaces.get(self.workspace);
        line.push_str(&format!(
            " {} {}/{} | {} {}/{} ",
            space.map_or("", |space| space.name.as_str()),
            self.workspace.saturating_add(1),
            self.workspaces.len(),
            self.shown_tab().map_or("", |tab| tab.title.as_str()),
            place.tab.saturating_add(1),
            self.tabs().len(),
        ));
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

fn terminal_size() -> (u16, u16) {
    fuxix::terminal::stdin_size().map_or((80, 24), |(size, _)| (size.cols.get(), size.rows.get()))
}

/// Raises the limit on open files far enough for a full session, or says
/// how far it got: panes past that are refused with the numbers.
fn file_limit() -> Result<Option<u64>, Failure> {
    #[cfg(unix)]
    {
        let panes = u64::try_from(dir::MAX_PANES).unwrap_or(u64::MAX);
        let need = panes
            .saturating_mul(FILES_PER_PANE)
            .saturating_add(BASE_FILES);
        fuxix::io::raise_open_files(need)
            .map(Some)
            .map_err(|e| io_failure("the limit on open files", e))
    }
    #[cfg(windows)]
    Ok(None)
}

fn fux2_program() -> Result<PathBuf, Failure> {
    let exe = std::env::current_exe().map_err(|e| io_failure("finding this program", e))?;
    Ok(exe.with_file_name(format!("fux2{}", std::env::consts::EXE_SUFFIX)))
}

fn parse_args() -> Result<Option<PathBuf>, Failure> {
    let mut args = std::env::args_os().skip(1);
    let mut layout = None;
    while let Some(arg) = args.next() {
        if arg != "--layout" {
            let why = format!("unexpected argument: {}", arg.to_string_lossy());
            return Err(Failure::new(Code::Invalid, why));
        }
        let path = args
            .next()
            .ok_or_else(|| Failure::new(Code::Invalid, "--layout needs a file"))?;
        layout = Some(PathBuf::from(path));
    }
    Ok(layout)
}

fn run() -> Result<(), Failure> {
    let layout_path = parse_args()?;
    if !fuxix::terminal::on_one_terminal() {
        let why = "the viewer needs one terminal on stdin and stdout";
        return Err(Failure::new(Code::Invalid, why));
    }
    let session = Session::open()?;
    let file_limit = file_limit()?;
    let workspaces = match &layout_path {
        None => layout::everything(),
        Some(path) => {
            let text = script::read_small_file(path, layout::MAX_LAYOUT_BYTES)
                .map_err(|e| Failure::new(Code::Invalid, format!("{}: {e}", path.display())))?;
            layout::parse(&text)
                .map_err(|why| Failure::new(Code::Invalid, format!("{}: {why}", path.display())))?
        }
    };
    #[cfg(unix)]
    let stops = [
        Signal::Winch,
        Signal::Term,
        Signal::Hup,
        Signal::Int,
        Signal::Quit,
    ];
    #[cfg(windows)]
    let stops = [Signal::Winch, Signal::Term, Signal::Hup, Signal::Int];
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
        layout_path,
        layout_text: None,
        places: vec![Place::default(); workspaces.len()],
        workspaces,
        workspace: 0,
        last_workspace: 0,
        panes: BTreeMap::new(),
        focus: None,
        focus_name: None,
        wanted: None,
        back: None,
        term: terminal_size(),
        bindings: first_bindings(),
        prefix: None,
        message: None,
        file_limit,
        script: Some(script),
        from_script,
        running: None,
        waiting_runs: VecDeque::new(),
        loading: true,
        layout_failed: false,
        held: Vec::new(),
        run_sent: false,
        painter,
        shown: Frame::default(),
        tiles: Vec::new(),
        dirty: true,
        next_paint: now,
        next_scan: now,
        quit: false,
    };
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
            view.settle_focus();
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

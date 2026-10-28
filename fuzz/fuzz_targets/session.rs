#![no_main]
//! Input: two bytes for the first client's size, then operations, each a
//! tag byte and its arguments: bytes a client types, the Escape deadline,
//! a resize, a client attaching or detaching, a fuxth program from the
//! side, a pane's output, a pane's shell exiting. Two sessions run the
//! same operations: one given each client's bytes as the input says, the
//! other one byte at a time.
//!
//! Each client's keys are read as its client reads them: by the screen it
//! has open (`screens::Screen`), else through its bindings (`keymap::Held`,
//! one keymap all clients share). Its actions go to the session, and its
//! programs run in-process as that client (a screen one opens opens), as do
//! the programs the session hands a client (`ToAttached::Run`) and keys sent
//! to it (`ToAttached::Keys`).
use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use fux::act::Act;
use fux::act::Panel;
use fux::client_state::ClientState;
use fux::copy::MAX_CLIPBOARD;
use fux::decode::{Decoder, Input};
use fux::id::{ClientId, PaneId};
use fux::keymap::Held;
use fux::keys::{Key, Keystroke};
use fux::outer;
use fux::protocol::{Context, ToAttached};
use fux::render::{Grid, compose_view, paint_into};
use fux::screens::{Dispatch, Job, Screen};
use fux::script::{AsClient, Host, run_capped};
use fux::session::{InProcess, Outgoing, Session};
use fux::view::Mode;
use libfuzzer_sys::fuzz_target;

/// The client's screen as it should look now, in a grid of its own.
fn compose(s: &Session, c: ClientId) -> Option<Grid> {
    let view = s.views.get(&c)?;
    let mut grid = Grid::default();
    let result = compose_view(s, view, &mut grid, &mut fux::layout::Placement::default());
    assert!(result.is_ok(), "{result:?}");
    Some(grid)
}

/// The bytes that turn `old` (or nothing) into `new`.
fn paint(old: Option<&Grid>, new: &Grid) -> Vec<u8> {
    let mut out = Vec::new();
    let blank = Grid::default();
    paint_into(old.unwrap_or(&blank), old.is_some(), new, &mut out);
    out
}

/// Programs from the command line, while clients type: they change what
/// clients are looking at, and the bindings and layers they are in.
/// Seeds pick them by position, so a new one goes at the end.
const SIDE: &[&str] = &[
    "opts 80 23 size str +1 workspace in new-tab drop",
    "opts 80 23 size str +1 workspace in str two name str htop program new-tab drop",
    "opts 80 23 size str ws name new-workspace drop",
    "str %1 pane str horizontal axis split drop",
    "str %2 pane str vertical axis split drop",
    "str %2 pane str horizontal axis split drop",
    "str %3 pane str horizontal axis split drop",
    "str %1 pane kill",
    "str %2 pane kill",
    "str @1 tab kill",
    "str +2 workspace kill",
    "str %1 pane str right move",
    "str %2 pane str new-tab move",
    "str %1 pane str %2 pane swap-with",
    "str %1 pane str right 5 move-border",
    "str @1 tab str renamed rename",
    "str @1 tab str next reorder",
    "str c1 client zoom",
    "str c1 client str next focus",
    "str c1 client str next step-tab",
    "str c2 client str next step-workspace",
    "str c1 client str C-b key send-keys",
    "str c1 client copy-mode",
    "str c2 client str c2 client focused choose-pane",
    "str c1 client choose-tab",
    "str c1 client str c1 client focused tab confirm-close",
    "str c2 client str c2 client focused workspace rename-prompt",
    "str c1 client command-prompt",
    "str t str 'me zoom' bind",
    "str t unbind",
    "str r unbind",
    "str 't m' unbind",
    "unbind-all",
    "str 'g h' str 'me focused str left 1 move-border' bind repeat",
    "str 't m j' str 'me zoom' bind str Grow group repeat",
    "str 'g n' str 'opts new-tab drop' bind",
    "str 'y l' str 'me focused str right 1 move-border' bind str Grow group repeat",
    "str 'y h' str 'me focused str left 1 move-border' bind str Grow group repeat",
    "str c1 client str %1 pane show",
    "str c1 client str %2 pane show",
    "str c1 client str %3 pane show",
    "str c1 client str @1 tab show",
    "str g unbind",
    "str C-a prefix",
    "str C-b prefix",
    "str program-clipboard false set",
    "str program-clipboard true set",
    "str c1 client str hjkl text send-keys",
    "str c1 client detach",
    "str %1 pane newest paste-buffer",
    "str %1 pane str x key send-keys",
    "str %1 pane str C-b key send-keys",
    "str V str 'me focused str vertical axis split drop' bind",
    "str Up str 'me zoom' bind",
    "str C-b str 'opts new-tab drop' bind",
    "str 'g Enter' str 'me focused str right 1 move-border' bind repeat",
    "str M-t str 'opts new-tab drop' bind-root",
    "str x str 'me zoom' bind-root",
    "str y str 'str %1 pane str z key send-keys' bind-root",
    "str x unbind-root",
    "str titles true set",
    "str titles false set",
    "str bell false set",
    "str bell true set",
    "str c2 client str ls text str Enter key send-keys",
    "str terminal-clipboard false set",
    "str terminal-clipboard true set",
];

/// What a pane's program may say of the mouse.
const MOUSE_MODES: &[&[u8]] = &[
    b"\x1b[?1000h",
    b"\x1b[?1002h",
    b"\x1b[?1003h",
    b"\x1b[?9h",
    b"\x1b[?1006h",
    b"\x1b[?1005h",
    b"\x1b[?1006l",
    b"\x1b[?1000l",
    b"\x1b[?1003l",
];

/// Words that write to a pane: a repeating binding of one may.
const WRITES: &[&str] = &["send-keys", "paste-buffer"];

struct Bytes<'a>(&'a [u8]);
impl<'a> Bytes<'a> {
    fn next(&mut self) -> u8 {
        let Some((&b, rest)) = self.0.split_first() else {
            return 0;
        };
        self.0 = rest;
        b
    }
    fn take(&mut self, n: usize) -> &'a [u8] {
        let (taken, rest) = self.0.split_at_checked(n).unwrap_or((self.0, &[]));
        self.0 = rest;
        taken
    }
}

/// `bytes` in pieces of `size`, the last one maybe shorter.
fn pieces(mut rest: &[u8], size: NonZeroUsize) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (piece, after) = rest.split_at_checked(size.get()).unwrap_or((rest, &[]));
        rest = after;
        (!piece.is_empty()).then_some(piece)
    })
}

fn size(rows: u8, cols: u8) -> (u16, u16) {
    (1 + u16::from(rows) % 60, 1 + u16::from(cols) % 200)
}

/// Words that read files, which a program typed at a prompt is not run
/// with: a fuzzer finds /dev/zero.
const READS: &[&str] = &["check"];

/// How many steps a program may take: enough for any binding, few enough
/// for a loop typed at the prompt to end.
const BUDGET: u64 = 20_000;

/// What anyone can tell apart: the session's items, each client's mode,
/// notice and screen, the paste buffers, and each
/// client's keymap and keys.
type State = (
    String,
    Vec<(ClientId, String, Option<Grid>)>,
    Vec<(fux::id::BufferId, String)>,
    ClientState,
    Vec<(ClientId, fux::screens::Display)>,
);

/// A client's keys, as its client reads them: its decoder, and the screen
/// it has open or what its keys are in the middle of.
#[derive(Default)]
struct Keys {
    screen: fux::screens::Display,
    decoder: Decoder,
}

/// One session, its clients' keys, and what drove it that the checks need.
struct Run {
    s: Session,
    /// Shared bindings; presentation comes from the addressed client's view.
    keymap: ClientState,
    keys: BTreeMap<ClientId, Keys>,
    /// Each client's grids as the server keeps them: composed into the
    /// spare, painted against the shown, swapped.
    grids: BTreeMap<ClientId, (Grid, Grid, fux::layout::Placement, bool)>,
    shut: bool,
}

/// The default keys, built once.
fn defaults() -> &'static ClientState {
    static DEFAULTS: std::sync::OnceLock<ClientState> = std::sync::OnceLock::new();
    DEFAULTS.get_or_init(|| fux::script::default_state().expect("the default keys bind"))
}

impl Run {
    fn new(rows: u16, cols: u16, run: fux::command::Run) -> Run {
        let s = Session::new("/nonexistent/fux.sock".into(), false, run);
        // The client attaching starts its first shell, on its screen.
        let mut run = Run {
            s,
            keymap: defaults().clone(),
            keys: BTreeMap::new(),
            grids: BTreeMap::new(),
            shut: false,
        };
        run.attach(rows, cols);
        run
    }

    fn attach(&mut self, rows: u16, cols: u16) {
        let kind = fux::view::ClientKind::Terminal;
        if let Ok(size) = fux_vt::Size::new(rows, cols)
            && let Ok(id) = self.s.attach_as(
                kind,
                fux::protocol::Attach {
                    initial: Some(fux::command::InitialWorkspace {
                        options: defaults().preferences.pane_options(),
                        name: None,
                        start: fux::command::Start::Shell(defaults().preferences.shell.clone()),
                        cwd: std::env::current_dir().unwrap_or_default(),
                        env: fux::command::EnvPatch::default(),
                    }),
                    ..fux::protocol::Attach::of(size, defaults().preferences.presentation())
                },
            )
        {
            self.keys.insert(id, Keys::default());
        }
    }

    fn detach(&mut self, client: ClientId) {
        self.s.detach(client);
        self.keys.remove(&client);
    }

    fn client(&self, pick: u8) -> Option<ClientId> {
        let clients: Vec<ClientId> = self.s.views.keys().copied().collect();
        clients
            .get(usize::from(pick) % clients.len().max(1))
            .copied()
    }

    fn pane(&self, pick: u8) -> Option<PaneId> {
        let panes: Vec<PaneId> = self.s.parts.panes().keys().copied().collect();
        panes.get(usize::from(pick) % panes.len().max(1)).copied()
    }

    /// Runs fuxth `code` as `client` (or from no client) runs it, the
    /// keymap its own: what it printed and left, a value a line, or why it
    /// failed; and the screen it opened, if it opened one.
    fn program(
        &mut self,
        client: Option<ClientId>,
        code: &str,
    ) -> (Result<String, String>, Option<Screen>) {
        let mut opened = None;
        let said = self.program_opening(client, &code.into(), &mut opened);
        (said, opened)
    }

    fn program_opening(
        &mut self,
        client: Option<ClientId>,
        job: &Job,
        opened: &mut Option<Screen>,
    ) -> Result<String, String> {
        let told = |failed: fux::script::Failed| match client {
            Some(_) => failed.notice(),
            None => failed.to_string(),
        };
        let tokens = job.tokens().map_err(|e| told(e.into()))?;
        if tokens.iter().any(|t| READS.contains(&&*t.text)) {
            return Err("reads a file".to_owned());
        }
        let mut machine = fux::script::compile(&tokens).map_err(told)?;
        let mut keymap = self.keymap.draft();
        if let Some(view) = client.and_then(|c| self.s.views.get(&c)) {
            keymap.preferences.presentation(view.presentation);
        }
        let mut out = Vec::new();
        let context = Context {
            client,
            ..Context::default()
        };
        let mut host = Host::new(context.clone(), Some(&mut keymap), &mut out);
        // As a client runs it: the real loop, a screen it opens built with
        // what the session answers.
        // InProcess publishes this client's presentation synchronously at
        // the same checkpoint; pending failures publish neither consumer.
        let mut publish = |committed: &ClientState| self.keymap = committed.clone();
        let mut asks = AsClient {
            asks: &mut InProcess(&mut self.s),
            context: context.clone(),
            opened: None,
            on_commit: Some(&mut publish),
        };
        let ended =
            run_capped(&mut machine, &mut host, &mut asks, Some(BUDGET), None).and_then(|stack| {
                fux::script::Asks::commit(&mut asks, &mut keymap, context)?;
                Ok(stack)
            });
        *opened = asks.opened.filter(|_| client.is_some() && ended.is_ok());
        let mut text = String::from_utf8_lossy(&out).into_owned();
        for value in &ended.map_err(told)? {
            text.push_str(&value.text().map_err(|failure| failure.message)?);
            text.push('\n');
        }
        Ok(text)
    }

    /// Presentation setters run as a picked client; CLI programs retain their
    /// own local semantics. Existing SIDE entries keep their positions.
    fn side(&mut self, pick: u8) -> Result<String, String> {
        let code = SIDE[usize::from(pick) % SIDE.len()];
        let client = match code {
            "str titles true set"
            | "str titles false set"
            | "str bell true set"
            | "str bell false set"
            | "str terminal-clipboard true set"
            | "str terminal-clipboard false set" => self.client(pick),
            _ => None,
        };
        let before: Vec<_> = self
            .s
            .views
            .iter()
            .map(|(id, view)| (*id, view.presentation))
            .collect();
        let result = self.program(client, code).0;
        if let Some(client) = client {
            assert!(result.is_ok(), "{code}: {result:?}");
            assert_eq!(
                self.s.views.get(&client).map(|v| v.presentation),
                Some(self.keymap.preferences.presentation()),
                "{code} did not reach its view"
            );
            for (other, presentation) in before {
                if other != client {
                    assert_eq!(
                        self.s.views.get(&other).map(|v| v.presentation),
                        Some(presentation),
                        "{code} changed another view"
                    );
                }
            }
        }
        result
    }

    fn before_paint(&mut self, client: ClientId) -> Vec<u8> {
        let before = self.s.views.get(&client).and_then(|v| v.title.clone());
        let bytes = self.s.before_paint(client);
        let view = self.s.views.get(&client).expect("the painted view");
        assert_eq!(
            view.title.is_some(),
            view.presentation.titles,
            "title policy was not applied"
        );
        if before != view.title {
            assert!(!bytes.is_empty(), "a title change was not sent");
        }
        if let Some(title) = &view.title {
            assert!(!title.chars().any(char::is_control), "unclean title");
        }
        bytes
    }

    /// Runs `code` as `client`, telling it what came of it, as a client
    /// does with its bindings' and its prompt's programs.
    fn run_as(&mut self, client: ClientId, job: &Job) {
        let mut opened = None;
        let said = self.program_opening(Some(client), job, &mut opened);
        if let Some(screen) = opened {
            self.open(client, screen);
        }
        let notice = match said {
            Err(text) => Some((text, true)),
            Ok(said) => {
                let said: Vec<&str> = said.lines().filter(|l| !l.trim().is_empty()).collect();
                (!said.is_empty()).then(|| (said.join("  "), false))
            }
        };
        if let Some((text, error)) = notice {
            self.s.acts(client, vec![Act::Notice { text, error }]);
        }
    }

    /// Opens `screen` at `client`, in place of what was open.
    fn open(&mut self, client: ClientId, screen: Screen) {
        let mut acts = Vec::new();
        if let Some(keys) = self.keys.get_mut(&client) {
            acts.extend(keys.screen.open(screen, &self.keymap.keys.modes));
        }
        self.s.acts(client, acts);
    }

    /// What the server does after every event: settle, then act on the
    /// outbox. The panes' queued input is taken, as a PTY would.
    fn after(&mut self) {
        // What the clients do with what they were sent can send more: a
        // few rounds, the rest left for the next event.
        for _ in 0..4 {
            self.s.settle();
            let outbox = std::mem::take(&mut self.s.outbox);
            if outbox.is_empty() {
                break;
            }
            for outgoing in outbox {
                self.outgoing(outgoing);
            }
        }
        self.drain_panes();
        self.check_memo();
    }

    fn outgoing(&mut self, outgoing: Outgoing) {
        match outgoing {
            // What fux asks a client's terminal: when it attaches, and
            // after the terminal's answers.
            Outgoing::To(_, ToAttached::Paint(bytes))
                if [
                    outer::QUERIES,
                    outer::COLOUR_QUERIES,
                    outer::REPORTS_ON,
                    outer::SCHEME_QUERY,
                    outer::KITTY_PUSH,
                    outer::BELL,
                ]
                .contains(&&*bytes) => {}
            Outgoing::To(_, ToAttached::Paint(bytes)) => panic!("not a question: {bytes:?}"),
            // A copy: OSC 52, with at most MAX_CLIPBOARD bytes of base64.
            Outgoing::Copy(_, bytes) => {
                let payload = bytes
                    .strip_prefix(b"\x1b]52;c;")
                    .and_then(|b| b.strip_suffix(b"\x07"))
                    .unwrap_or_else(|| panic!("not OSC 52: {bytes:?}"));
                assert!(payload.len() <= MAX_CLIPBOARD, "{} bytes", payload.len());
                assert!(
                    payload
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(b))
                );
            }
            Outgoing::To(client, ToAttached::Exit(_)) => self.detach(client),
            // Sent only as a client attaches, by the server.
            Outgoing::To(_, ToAttached::NeedsInitial | ToAttached::Welcome(..)) => {
                panic!("an attachment reply from the session")
            }
            Outgoing::Shutdown { .. } => self.shut = true,
            Outgoing::To(client, ToAttached::Run(code)) => self.run_as(client, &code.into()),
            Outgoing::To(client, ToAttached::Ended(opening)) => {
                if let Some(keys) = self.keys.get_mut(&client) {
                    keys.screen.ended(opening);
                }
            }
            Outgoing::To(client, ToAttached::Keys(typed)) => {
                for input in typed {
                    match input {
                        fux::command::Input::Key(press) => self.press(client, press.into()),
                        fux::command::Input::Text(text) => {
                            for c in text.chars() {
                                self.press(client, fux::keys::KeyPress::char(c).into());
                            }
                        }
                        fux::command::Input::Paste(text) => {
                            self.inputs(client, vec![Input::Paste(text)]);
                        }
                        // A pane's alone: send-keys refuses them for a client.
                        fux::command::Input::Bytes(_) => {}
                    }
                }
            }
        }
    }

    /// Composing a client's screen into the grid it was composed into last
    /// but one, as the server does, gives what composing it whole gives;
    /// and its paint, rows skipped by their memo, is the paint comparing
    /// every row.
    fn check_memo(&mut self) {
        let clients: Vec<ClientId> = self.s.views.keys().copied().collect();
        self.grids.retain(|c, _| clients.contains(c));
        for c in clients {
            let (spare, shown, placement, painted) = self.grids.entry(c).or_insert_with(|| {
                (
                    Grid::default(),
                    Grid::default(),
                    fux::layout::Placement::default(),
                    false,
                )
            });
            let Some(view) = self.s.views.get(&c) else {
                continue;
            };
            let result = compose_view(&self.s, view, spare, placement);
            assert!(result.is_ok(), "{result:?}");
            let whole = compose(&self.s, c);
            assert!(
                whole.as_ref() == Some(&*spare),
                "{c}: composed in part, not as whole"
            );
            let old = painted.then_some(&*shown);
            let fast = paint(old, spare);
            // Copies, which have no memo.
            let (plain_old, plain_new) = (shown.clone(), spare.clone());
            let slow = paint(painted.then_some(&plain_old), &plain_new);
            assert!(fast == slow, "{c}: the memo's paint differs");
            assert_eq!(
                fast.is_empty(),
                *painted && *spare == *shown,
                "{c}: painted nothing, or something, for the same screen"
            );
            std::mem::swap(spare, shown);
            *painted = true;
        }
    }

    /// Types `bytes` in pieces of `piece` bytes (all at once for 0).
    fn type_in(&mut self, client: ClientId, bytes: &[u8], piece: usize) {
        let chunks: Vec<&[u8]> = match NonZeroUsize::new(piece) {
            Some(piece) => pieces(bytes, piece).collect(),
            None => vec![bytes],
        };
        for chunk in chunks {
            let mut inputs = Vec::new();
            if let Some(keys) = self.keys.get_mut(&client) {
                keys.decoder.bytes(chunk, &mut inputs);
            }
            self.inputs(client, inputs);
        }
    }

    /// The decoder's wait passed: what waited is taken as it is.
    fn escape(&mut self, client: ClientId) {
        let mut inputs = Vec::new();
        if let Some(keys) = self.keys.get_mut(&client) {
            keys.decoder.timeout(&mut inputs);
        }
        self.inputs(client, inputs);
    }

    /// What a client's terminal sent, decoded, as its client takes it
    /// (`Screen::input`): a key as `press` checks it.
    fn inputs(&mut self, client: ClientId, inputs: Vec<Input>) {
        for input in inputs {
            let Some(read_only) = self.s.views.get(&client).map(|v| v.read_only) else {
                return;
            };
            if let Input::Key(stroke) = input {
                self.press(client, stroke);
                continue;
            }
            let keys = self.keys.entry(client).or_default();
            let effects = keys.screen.input(&self.keymap.keys, input, read_only);
            self.effects(client, effects);
        }
    }

    /// What a screen's key or paste led to: actions sent, programs run.
    fn effects(&mut self, client: ClientId, effects: Vec<Dispatch>) {
        for effect in effects {
            match effect {
                Dispatch::Send(act) => self.s.acts(client, vec![act]),
                Dispatch::Run(job) => self.run_as(client, &job),
                Dispatch::Fence => {}
            }
        }
    }

    /// A key typed at `client`: to the screen it has open, else through its
    /// bindings, the actions sent and the programs run. A key typed in a
    /// repeat mode reaches no pane, unless the binding it runs there writes
    /// to one.
    fn press(&mut self, client: ClientId, stroke: Keystroke) {
        let Some(view) = self.s.views.get(&client) else {
            return;
        };
        let read_only = view.read_only;
        let mut keys = self.keys.remove(&client).unwrap_or_default();
        if keys.screen.screen.is_open() && !read_only {
            let effects = keys
                .screen
                .input(&self.keymap.keys, Input::Key(stroke), read_only);
            self.keys.insert(client, keys);
            self.effects(client, effects);
            return;
        }
        let repeating =
            matches!(keys.screen.screen, Screen::Keys(Held::Repeat { .. })) && !read_only;
        let steps = keys
            .screen
            .input(&self.keymap.keys, Input::Key(stroke), read_only);
        self.keys.insert(client, keys);
        if repeating {
            self.drain_panes();
        }
        let mut writes = false;
        for step in steps {
            match step {
                Dispatch::Send(act) => {
                    let key = matches!(act, Act::Key(_));
                    assert!(
                        !(repeating && key),
                        "{} sent on in a repeat mode",
                        stroke.press
                    );
                    self.s.acts(client, vec![act]);
                }
                Dispatch::Fence => {}
                Dispatch::Run(job) => {
                    writes |= WRITES.iter().any(|w| job.code.contains(w));
                    self.run_as(client, &job);
                }
            }
        }
        if repeating && !writes {
            self.assert_no_pane_input(stroke.press);
        }
    }

    /// Types a mouse report in pieces of `piece` bytes (whole for 0), its
    /// client's held input flushed first so that the report is one: no pane
    /// may hear it but the focused pane of a client with nothing of fux's
    /// open, whose program asked for the mouse. A character begun before it
    /// outlasts the flush, as no deadline cuts a character short (fux-vt's
    /// README), and the report's ESC ends it as one U+FFFD
    /// (Unicode 17.0, 3.9, maximal subparts): that key, the first input, is
    /// typed before the report, and what panes hear of it is not the
    /// report's.
    fn mouse(&mut self, client: ClientId, report: &[u8], piece: usize) {
        self.escape(client);
        self.drain_panes();
        let chunks: Vec<&[u8]> = match NonZeroUsize::new(piece) {
            Some(piece) => pieces(report, piece).collect(),
            None => vec![report],
        };
        let mut first = true;
        for chunk in chunks {
            let mut inputs = Vec::new();
            if let Some(keys) = self.keys.get_mut(&client) {
                keys.decoder.bytes(chunk, &mut inputs);
            }
            for input in inputs {
                let ended = first
                    && matches!(&input, Input::Key(k) if k.press.key == Key::Char(char::REPLACEMENT_CHARACTER));
                first = false;
                self.inputs(client, vec![input]);
                if ended {
                    self.drain_panes();
                }
            }
        }
        let view = self.s.views.get(&client);
        let normal = view.is_some_and(|v| matches!(v.mode, Mode::Normal));
        let focus = self.s.focused(client).filter(|_| normal);
        self.s.parts.each_pane_mut(|pane| {
            let (id, heard) = (pane.id(), pane.drain_input());
            let asked = pane.screen().mouse_protocol_mode() != fux_vt::MouseProtocolMode::None;
            assert!(
                heard.is_empty() || (Some(id) == focus && asked),
                "{id} heard {heard:?} from {report:?}"
            );
        });
    }

    fn drain_panes(&mut self) {
        self.s.parts.each_pane_mut(|pane| drop(pane.drain_input()));
    }

    fn assert_no_pane_input(&mut self, key: fux::keys::KeyPress) {
        self.s.parts.each_pane_mut(|pane| {
            let queued = pane.drain_input();
            let id = pane.id();
            assert!(
                queued.is_empty(),
                "{key} in a repeat mode reached {id}: {queued:?}"
            );
        });
    }

    /// What anyone can tell apart, as `State` lists it.
    fn state(&mut self) -> State {
        let ls = self.program(None, "ls").0.unwrap_or_else(|e| e);
        let screens = self
            .s
            .views
            .iter()
            .map(|(&c, view)| {
                let notice = view.notice.as_ref().map(|n| n.text.as_str());
                let seen = format!("{} | {notice:?}", mode_text(&view.mode));
                (c, seen, compose(&self.s, c))
            })
            .collect();
        let held = (self.keys.iter())
            .map(|(&c, keys)| (c, keys.screen.clone()))
            .collect();
        (
            ls,
            screens,
            self.s.buffers.iter().cloned().collect(),
            self.keymap.clone(),
            held,
        )
    }
}

/// A mode as text: `Mode` has no `Debug` or `PartialEq`.
fn mode_text(mode: &Mode) -> String {
    match mode {
        Mode::Normal => "normal".into(),
        Mode::Panel(panel) => format!("{panel:?}"),
        Mode::Copy(copy) => format!(
            "copy {} {:?} {:?} {:?}",
            copy.pane, copy.top, copy.cursor, copy.typing
        ),
    }
}

/// Everything that must hold between events.
fn check(run: &Run) {
    let s = &run.s;
    // Every pane is in exactly one tab's layout, and every pane there is.
    let mut seen: BTreeMap<PaneId, usize> = BTreeMap::new();
    for ws in s.parts.workspaces() {
        for tab in ws.tabs() {
            for pane in tab.root().panes() {
                assert!(
                    s.parts.panes().contains_key(&pane),
                    "{pane} is in {} but gone",
                    tab.id()
                );
                *seen.entry(pane).or_default() += 1;
            }
        }
    }
    for pane in s.parts.panes().keys() {
        assert_eq!(
            seen.get(pane),
            Some(&1),
            "{pane} is in {:?} layouts",
            seen.get(pane)
        );
    }
    for (id, view) in &s.views {
        let grid = compose(s, *id).expect("a client's screen composes");
        assert_eq!((grid.rows, grid.cols), (view.rows, view.cols));
        assert!(run.keys.contains_key(id), "{id} attached, but has no keys");
    }
    // A repeat mode's bar is shown only to a client in one.
    for (id, keys) in &run.keys {
        let shown = s.views.get(id).map(|v| &v.mode);
        if matches!(shown, Some(Mode::Panel(Panel::Repeat(_)))) {
            assert!(
                matches!(keys.screen.screen, Screen::Keys(Held::Repeat { .. })),
                "{id} shows a repeat bar, its keys {:?}",
                keys.screen
            );
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes(data);
    let (rows, cols) = size(input.next(), input.next());
    // `given` gets each client's bytes as the input splits them; `bytewise`
    // one at a time. However the bytes arrive, the same result.
    // One server run for both: what they list (`ls`'s cursor) names it.
    let run = fux::command::Run::now().expect("a clock after 1970");
    let mut given = Run::new(rows, cols, run);
    let mut bytewise = Run::new(rows, cols, run);
    check(&given);
    while !input.0.is_empty() && !given.shut {
        let tag = input.next();
        match tag % 18 {
            16 => {
                let pick = input.next();
                let mode = MOUSE_MODES[usize::from(input.next()) % MOUSE_MODES.len()];
                if let Some(p) = given.pane(pick) {
                    given.s.output(p, mode);
                    bytewise.s.output(p, mode);
                }
            }
            17 => {
                let (pick, button, x, y) = (input.next(), input.next(), input.next(), input.next());
                let end = if button & 0x80 == 0 { 'M' } else { 'm' };
                let report = format!("\x1b[<{};{};{}{end}", button & 0x7f, x, y);
                if let Some(c) = given.client(pick) {
                    given.mouse(c, report.as_bytes(), 0);
                    bytewise.mouse(c, report.as_bytes(), 1);
                }
            }
            0..=7 | 15 => {
                let pick = input.next();
                let len = if tag % 16 == 15 {
                    usize::from(input.next())
                } else {
                    1 + usize::from(input.next() % 16)
                };
                let piece = usize::from(input.next() % 8);
                let bytes = input.take(len);
                if let Some(c) = given.client(pick) {
                    given.type_in(c, bytes, piece);
                    bytewise.type_in(c, bytes, 1);
                }
            }
            8 => {
                let pick = input.next();
                if let Some(c) = given.client(pick) {
                    given.escape(c);
                    bytewise.escape(c);
                }
            }
            9 => {
                let (pick, (rows, cols)) = (input.next(), size(input.next(), input.next()));
                if let Some(c) = given.client(pick) {
                    let given_resized = given.s.resize(c, rows, cols).is_ok();
                    let bytewise_resized = bytewise.s.resize(c, rows, cols).is_ok();
                    assert_eq!(given_resized, bytewise_resized);
                }
            }
            10 => {
                let (pick, (rows, cols)) = (input.next(), size(input.next(), input.next()));
                if given.s.views.len() < 3 {
                    given.attach(rows, cols);
                    bytewise.attach(rows, cols);
                } else if let Some(c) = given.client(pick) {
                    given.detach(c);
                    bytewise.detach(c);
                }
            }
            11 | 12 => {
                let pick = input.next();
                let a = given.side(pick);
                let b = bytewise.side(pick);
                assert_eq!(a, b, "side {pick}");
            }
            13 => {
                let pick = input.next();
                let len = 1 + usize::from(input.next() % 64);
                let bytes = input.take(len);
                if let Some(p) = given.pane(pick) {
                    given.s.output(p, bytes);
                    bytewise.s.output(p, bytes);
                }
            }
            _ => {
                let pick = input.next();
                if let Some(p) = given.pane(pick) {
                    given.s.exited(p, fux::process::Ended::Exited(0));
                    bytewise.s.exited(p, fux::process::Ended::Exited(0));
                }
            }
        }
        given.after();
        bytewise.after();
        // What each client's terminal is sent before a paint: the same
        // titles, however the bytes came.
        let clients: Vec<ClientId> = given.s.views.keys().copied().collect();
        for c in clients {
            let (a, b) = (given.before_paint(c), bytewise.before_paint(c));
            assert_eq!(a, b, "{c}'s title");
        }
        assert_eq!(given.shut, bytewise.shut);
        if given.shut {
            break;
        }
        // Between one and three clients.
        if given.s.views.is_empty() {
            given.attach(rows, cols);
            bytewise.attach(rows, cols);
        }
        check(&given);
    }
    // Compared once, at the end: composing every screen after every
    // operation fills AddressSanitizer's quarantine past the RSS limit.
    assert!(
        given.state() == bytewise.state(),
        "the same bytes, one at a time, gave a different result"
    );
});

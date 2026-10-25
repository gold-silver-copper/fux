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
use fux::act::{CopyAct, Panel};
use fux::config::Config;
use fux::copy::MAX_CLIPBOARD;
use fux::decode::{Decoder, Input};
use fux::id::{ClientId, PaneId};
use fux::keymap::{Held, Keymap};
use fux::keys::{Key, Keystroke};
use fux::outer;
use fux::protocol::{Asked, Context, Reply, ToAttached};
use fux::render::{Grid, compose_view, paint_into};
use fux::screens::{Effect, Screen};
use fux::script::{Ask, Failed, Host, Local};
use fux::session::{Begun, Origin, Outgoing, Session};
use fux::view::Mode;
use fuxth::Outcome as Ran;
use libfuzzer_sys::fuzz_target;

/// The client's screen as it should look now, in a grid of its own.
fn compose(s: &Session, c: ClientId) -> Option<Grid> {
    let view = s.views.get(&c)?;
    let mut grid = Grid::new(0, 0);
    compose_view(s, view, &mut grid, &mut fux::layout::Placement::default());
    Some(grid)
}

/// The bytes that turn `old` (or nothing) into `new`.
fn paint(old: Option<&Grid>, new: &Grid) -> Vec<u8> {
    let mut out = Vec::new();
    let blank = Grid::new(new.rows, new.cols);
    paint_into(old.unwrap_or(&blank), old.is_some(), new, &mut out);
    out
}

/// Programs from the command line, while clients type: they change what
/// clients are looking at, and the bindings and layers they are in.
/// Seeds pick them by position, so a new one goes at the end.
const SIDE: &[&str] = &[
    "opts str +1 workspace in new-tab drop",
    "opts str +1 workspace in str two name str htop program new-tab drop",
    "opts str ws name new-workspace drop",
    "str %1 pane horizontal split drop",
    "str %2 pane vertical split drop",
    "str %2 pane horizontal split drop",
    "str %3 pane horizontal split drop",
    "str %1 pane kill",
    "str %2 pane kill",
    "str @1 tab kill",
    "str +2 workspace kill",
    "str %1 pane str right move-beside",
    "str %2 pane str new-tab move-to",
    "str %1 pane str %2 pane swap-panes",
    "str %1 pane str right 5 move-border",
    "str @1 tab str renamed rename",
    "str @1 tab str next reorder",
    "str c1 client zoom",
    "str c1 client str next focus",
    "str c1 client next-tab",
    "str c2 client next-workspace",
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
    "str c1 client str %1 pane focus-pane",
    "str c1 client str %2 pane focus-pane",
    "str c1 client str %3 pane focus-pane",
    "str c1 client str @1 tab show-tab",
    "str g unbind",
    "str C-a prefix",
    "str C-b prefix",
    "str clipboard str off set",
    "str clipboard str on set",
    "str c1 client str hjkl text send-keys",
    "str c1 client detach",
    "str %1 pane 0 paste-buffer",
    "str %1 pane str x key send-keys",
    "str %1 pane str C-b key send-keys",
    "str V str 'me focused vertical split drop' bind",
    "str Up str 'me zoom' bind",
    "str C-b str 'opts new-tab drop' bind",
    "str 'g Enter' str 'me focused str right 1 move-border' bind repeat",
    "str M-t str 'opts new-tab drop' bind-root",
    "str x str 'me zoom' bind-root",
    "str y str 'str %1 pane str z key send-keys' bind-root",
    "str x unbind-root",
    "str titles str on set",
    "str titles str off set",
    "str bell str off set",
    "str bell str on set",
    "str c2 client str ls text str Enter key send-keys",
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
/// notice and screen, the configuration, the paste buffers, and each
/// client's keymap and keys.
type State = (
    String,
    Vec<(ClientId, String, Option<Grid>)>,
    Config,
    Vec<String>,
    Keymap,
    Vec<(ClientId, Screen)>,
);

/// A client's keys, as its client reads them: its decoder, and the screen
/// it has open or what its keys are in the middle of.
#[derive(Default)]
struct Keys {
    screen: Screen,
    decoder: Decoder,
}

/// One session, its clients' keys, and what drove it that the checks need.
struct Run {
    s: Session,
    /// The keymap of every client: bindings change it, as programs run.
    keymap: Keymap,
    keys: BTreeMap<ClientId, Keys>,
    /// Each client's grids as the server keeps them: composed into the
    /// spare, painted against the shown, swapped.
    grids: BTreeMap<ClientId, (Grid, Grid, fux::layout::Placement, bool)>,
    shut: bool,
}

/// The default keys, built once.
fn defaults() -> &'static Keymap {
    static DEFAULTS: std::sync::OnceLock<Keymap> = std::sync::OnceLock::new();
    DEFAULTS.get_or_init(|| fux::script::default_keys().expect("the default keys bind"))
}

impl Run {
    fn new(rows: u16, cols: u16) -> Run {
        let run = fux::command::Run::now().expect("a clock after 1970");
        let mut s = Session::new(
            Config::default(),
            "/nonexistent/fux.sock".into(),
            false,
            run,
        );
        s.start().expect("a session starts");
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
        if let Ok(id) = self
            .s
            .attach_as(fux::view::ClientKind::Terminal, rows, cols, None)
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
        let panes: Vec<PaneId> = self.s.panes.keys().copied().collect();
        panes.get(usize::from(pick) % panes.len().max(1)).copied()
    }

    /// A request of the session, as a server answers it: begun at once; a
    /// wait not waited on, there being no programs, and no time that both
    /// sessions share.
    fn ask(&mut self, asked: Asked) -> Reply {
        let origin = match asked.context.client {
            Some(client) if self.s.views.contains_key(&client) => Origin::Client(client),
            Some(_) | None => Origin::Cli(fux::session::Caller::default()),
        };
        match self.s.begin(asked.request, &origin) {
            Begun::Done(outcome) => outcome,
            Begun::Waiting(_) | Begun::Events(_) | Begun::Grant(..) => Reply::default(),
        }
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
        let said = self.program_opening(client, code, &mut opened);
        (said, opened)
    }

    fn program_opening(
        &mut self,
        client: Option<ClientId>,
        code: &str,
        opened: &mut Option<Screen>,
    ) -> Result<String, String> {
        let tokens = fuxth::token::source("fuzz", code).map_err(|e| e.to_string())?;
        if tokens.iter().any(|t| READS.contains(&&*t.text)) {
            return Err("reads a file".to_owned());
        }
        let mut machine = fux::script::compile(&tokens).map_err(|e| e.to_string())?;
        let mut keymap = self.keymap.clone();
        let mut out = Vec::new();
        let mut host = Host {
            context: Context {
                client,
                ..Context::default()
            },
            keymap: Some(&mut keymap),
            last_bound: None,
            out: &mut out,
        };
        let ended = loop {
            let ran = machine.run(&mut host, BUDGET).map_err(Failed::from);
            match ran {
                Err(failed) => break Err(failed.to_string()),
                Ok(Ran::Finished) => break Ok(machine.stack().to_vec()),
                Ok(Ran::Paused) => break Err("too long".to_owned()),
                Ok(Ran::Suspended(Ask::Local(Local::Open(open)))) if client.is_some() => {
                    // Built with what the session answers, as a client
                    // builds it.
                    let context = host.context.clone();
                    let built = open.build(&mut |command| {
                        Ok(self.ask(Asked {
                            request: command.into(),
                            context: context.clone(),
                        }))
                    });
                    match built {
                        Ok(screen) => *opened = Some(screen),
                        Err(failed) => break Err(failed.to_string()),
                    }
                }
                Ok(Ran::Suspended(Ask::Local(_))) => break Err("not in-process".to_owned()),
                Ok(Ran::Suspended(Ask::Server {
                    request,
                    client,
                    then,
                })) => {
                    let mut context = host.context.clone();
                    if client.is_some() {
                        context.client = client;
                    }
                    let reply = self.ask(Asked {
                        request: *request,
                        context,
                    });
                    // A failure raised in the program, for a `try` to
                    // catch, as a client's run raises it.
                    let values = fux::script::answered(reply, then).or_else(|failed| {
                        let failure = fuxth::Failure::new(failed.code.into(), failed.message);
                        machine
                            .raise(failure)
                            .map(|()| Vec::new())
                            .map_err(Failed::from)
                    });
                    let pushed = values.and_then(|values| {
                        values
                            .into_iter()
                            .try_for_each(|v| machine.push(v).map_err(Failed::from))
                    });
                    if let Err(failed) = pushed {
                        break Err(failed.to_string());
                    }
                }
            }
        };
        // As a client keeps what its program did to its keymap.
        self.keymap = keymap;
        let mut text = String::from_utf8_lossy(&out).into_owned();
        for value in &ended? {
            text.push_str(&value.text());
            text.push('\n');
        }
        Ok(text)
    }

    /// Runs `code` as `client`, telling it what came of it, as a client
    /// does with its bindings' and its prompt's programs.
    fn run_as(&mut self, client: ClientId, code: &str) {
        let (said, opened) = self.program(Some(client), code);
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
            if let Screen::Copy(_) = keys.screen {
                acts.push(Act::Copy(CopyAct::Leave));
            }
            acts.extend(screen.opened());
            keys.screen = screen;
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
            Outgoing::To(_, ToAttached::Paint(bytes)) => {
                // Else OSC 52, with at most MAX_CLIPBOARD bytes of base64.
                let payload = bytes
                    .strip_prefix(b"\x1b]52;c;")
                    .and_then(|b| b.strip_suffix(b"\x07"))
                    .unwrap_or_else(|| panic!("not a question nor OSC 52: {bytes:?}"));
                assert!(payload.len() <= MAX_CLIPBOARD, "{} bytes", payload.len());
                assert!(
                    payload
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(b))
                );
            }
            Outgoing::To(client, ToAttached::Exit(_)) => self.detach(client),
            // Sent only as a client attaches, by the server.
            Outgoing::To(_, ToAttached::Welcome(..)) => panic!("a welcome from the session"),
            Outgoing::Shutdown { .. } => self.shut = true,
            Outgoing::To(client, ToAttached::Run(code)) => self.run_as(client, &code),
            Outgoing::To(client, ToAttached::Ended(kind)) => {
                if let Some(keys) = self.keys.get_mut(&client) {
                    keys.screen.ended(kind);
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
                    Grid::new(0, 0),
                    Grid::new(0, 0),
                    fux::layout::Placement::default(),
                    false,
                )
            });
            let Some(view) = self.s.views.get(&c) else {
                continue;
            };
            compose_view(&self.s, view, spare, placement);
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

    /// What a client's terminal sent, decoded, as its client takes it.
    fn inputs(&mut self, client: ClientId, inputs: Vec<Input>) {
        for input in inputs {
            let Some(view) = self.s.views.get(&client) else {
                return;
            };
            let read_only = view.read_only;
            let act = match input {
                Input::Key(stroke) => {
                    self.press(client, stroke);
                    continue;
                }
                Input::Reply(reply) => Act::Reply(reply),
                Input::Paste(_)
                | Input::PasteTooLong
                | Input::FocusIn
                | Input::FocusOut
                | Input::Mouse(_)
                    if read_only =>
                {
                    continue;
                }
                Input::Paste(text) => {
                    let effects = match self.keys.get_mut(&client) {
                        Some(keys) => keys.screen.paste(&text),
                        None => Vec::new(),
                    };
                    self.effects(client, effects);
                    continue;
                }
                Input::Mouse(_) if self.screen_open(client) => continue,
                Input::PasteTooLong => {
                    let limit = fux::decode::PASTE_LIMIT / 1024;
                    let text = format!("paste exceeds {limit} KiB; discarded");
                    Act::Notice { text, error: true }
                }
                Input::FocusIn => Act::Focus(true),
                Input::FocusOut => Act::Focus(false),
                Input::Mouse(event) => Act::Mouse(event),
            };
            self.s.acts(client, vec![act]);
        }
    }

    fn screen_open(&self, client: ClientId) -> bool {
        self.keys
            .get(&client)
            .is_some_and(|k| k.screen.kind().is_some())
    }

    /// What a screen's key or paste led to: actions sent, programs run.
    fn effects(&mut self, client: ClientId, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Send(act) => self.s.acts(client, vec![act]),
                Effect::Run(code) => self.run_as(client, &code),
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
        if keys.screen.kind().is_some() && !read_only {
            let effects = keys.screen.key(&self.keymap, stroke, read_only);
            self.keys.insert(client, keys);
            self.effects(client, effects);
            return;
        }
        let repeating = matches!(keys.screen, Screen::Keys(Held::Repeat { .. })) && !read_only;
        let steps = keys.screen.key(&self.keymap, stroke, read_only);
        self.keys.insert(client, keys);
        if repeating {
            self.drain_panes();
        }
        let mut writes = false;
        for step in steps {
            match step {
                Effect::Send(act) => {
                    let key = matches!(act, Act::Key(_));
                    assert!(
                        !(repeating && key),
                        "{} sent on in a repeat mode",
                        stroke.press
                    );
                    self.s.acts(client, vec![act]);
                }
                Effect::Run(code) => {
                    writes |= WRITES.iter().any(|w| code.contains(w));
                    self.run_as(client, &code);
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
        for (id, pane) in &mut self.s.panes {
            let heard = pane.input.drain_all();
            let asked = pane.screen().mouse_protocol_mode() != fux_vt::MouseProtocolMode::None;
            assert!(
                heard.is_empty() || (Some(*id) == focus && asked),
                "{id} heard {heard:?} from {report:?}"
            );
        }
    }

    fn drain_panes(&mut self) {
        for pane in self.s.panes.values_mut() {
            pane.input.drain_all();
        }
    }

    fn assert_no_pane_input(&mut self, key: fux::keys::KeyPress) {
        for (id, pane) in &mut self.s.panes {
            let queued = pane.input.drain_all();
            assert!(
                queued.is_empty(),
                "{key} in a repeat mode reached {id}: {queued:?}"
            );
        }
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
            self.s.config.clone(),
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
    for ws in &s.workspaces {
        for tab in ws.tabs() {
            for pane in tab.root().iter().flat_map(|r| r.panes()) {
                assert!(
                    s.panes.contains_key(&pane),
                    "{pane} is in {} but gone",
                    tab.id
                );
                *seen.entry(pane).or_default() += 1;
            }
        }
    }
    for pane in s.panes.keys() {
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
                matches!(keys.screen, Screen::Keys(Held::Repeat { .. })),
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
    let mut given = Run::new(rows, cols);
    let mut bytewise = Run::new(rows, cols);
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
                    given.s.resize(c, rows, cols);
                    bytewise.s.resize(c, rows, cols);
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
                let code = SIDE[usize::from(input.next()) % SIDE.len()];
                let a = given.program(None, code).0;
                let b = bytewise.program(None, code).0;
                assert_eq!(a, b, "{code}");
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
            let (a, b) = (given.s.before_paint(c), bytewise.s.before_paint(c));
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

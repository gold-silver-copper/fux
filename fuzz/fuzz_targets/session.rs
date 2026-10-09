#![no_main]
//! Input: two bytes for the first client's size, then operations, each a
//! tag byte and its arguments: bytes a client types, the Escape deadline,
//! a resize, a client attaching or detaching, a command from the side, a
//! pane's output, a pane's shell exiting. Two sessions run the same
//! operations: one given each client's bytes as the input says, the other
//! one byte at a time.
use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use fux::config::{Config, Node};
use fux::copy::MAX_CLIPBOARD;
use fux::decode::{Decoder, Input};
use fux::id::{ClientId, PaneId};
use fux::keys::KeyPress;
use fux::outer;
use fux::render::{Grid, compose};
use fux::session::{Ctx, Outgoing, Session};
use fux::view::{Choice, Mode};
use libfuzzer_sys::fuzz_target;

/// Commands from the command line, while clients type: they change what
/// clients are looking at, and the bindings and layers they are in.
/// Seeds pick them by position, so a new one goes at the end.
const SIDE: &[&str] = &[
    "new-tab -t +1",
    "new-tab -t +1 -n two -- htop",
    "new-workspace -n ws",
    "split -h -t %1",
    "split -v -t %2",
    "split -h -t %2",
    "split -h -t %3",
    "kill-pane -t %1",
    "kill-pane -t %2",
    "kill-tab -t @1",
    "kill-workspace -t +2",
    "move-pane -t %1 -R",
    "move-pane -t %2 --to new-tab",
    "swap-pane -t %1 %2",
    "resize-pane -t %1 -R 5",
    "rename -t @1 renamed",
    "reorder tab -t @1 --next",
    "zoom -c c1",
    "select-pane -c c1 --next",
    "select-tab -c c1 --next",
    "select-workspace -c c2 --next",
    "command-column -c c1",
    "copy-mode -c c1",
    "menu -c c2 pane",
    "choose-tab -c c1",
    "confirm-close -c c1 tab",
    "rename-prompt -c c2 workspace",
    "command-prompt -c c1",
    "bind t zoom",
    "unbind t",
    "unbind r",
    "unbind t m",
    "unbind-all",
    "bind -r g h resize-pane -L",
    "bind -g Grow -r t m j zoom",
    "bind g n new-tab",
    "bind -g Grow -r y l resize-pane -R",
    "bind -g Grow -r y h resize-pane -L",
    "select-pane -c c1 -t %1",
    "select-pane -c c1 -t %2",
    "select-pane -c c1 -t %3",
    "select-tab -c c1 -t @1",
    "unbind g",
    "set prefix C-a",
    "set prefix C-b",
    "set clipboard off",
    "set clipboard on",
    "reload",
    "detach -c c1",
    "paste-buffer -t %1",
    "send-keys -t %1 x",
    "send-prefix -t %1",
    "bind V split -v",
    "bind Up zoom",
    "bind C-b new-tab",
    "bind -r g Enter resize-pane -R",
    "bind -n M-t new-tab",
    "bind -n x zoom",
    "bind -n y send-keys -t %1 z",
    "unbind -n x",
    "set titles on",
    "set titles off",
    "set bell off",
    "set bell on",
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

/// Commands that write to a pane: a repeating binding of one may.
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

/// What anyone can tell apart: the session's items, each client's mode,
/// notice and screen, the configuration and the paste buffers.
type State = (
    String,
    Vec<(ClientId, String, Option<Grid>)>,
    Config,
    Vec<String>,
);

/// One session, and what drove it that the checks need.
struct Run {
    s: Session,
    /// Each client's grids as the server keeps them: composed into the
    /// spare, painted against the shown, swapped.
    grids: BTreeMap<ClientId, (Grid, Grid, fux::layout::Placement, bool)>,
    /// A decoder beside each client's, fed the same bytes, to tell which
    /// inputs each byte completes.
    shadows: BTreeMap<ClientId, Decoder>,
    shut: bool,
}

impl Run {
    fn new(rows: u16, cols: u16) -> Run {
        let mut s = Session::new(Config::default(), "/nonexistent/fux.sock".into(), false);
        s.start().expect("a session starts");
        let mut run = Run {
            s,
            grids: BTreeMap::new(),
            shadows: BTreeMap::new(),
            shut: false,
        };
        run.attach(rows, cols);
        run
    }

    fn attach(&mut self, rows: u16, cols: u16) {
        if let Ok(id) = self.s.attach(rows, cols, None) {
            self.shadows.insert(id, Decoder::default());
        }
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

    /// What the server does after every event: settle, then act on the
    /// outbox. The panes' queued input is taken, as a PTY would.
    fn after(&mut self) {
        self.s.settle();
        for outgoing in std::mem::take(&mut self.s.outbox) {
            match outgoing {
                // What fux asks a client's terminal: when it attaches, and
                // after the terminal's answers.
                Outgoing::Bytes(_, bytes)
                    if [
                        outer::QUERIES,
                        outer::COLOUR_QUERIES,
                        outer::REPORTS_ON,
                        outer::SCHEME_QUERY,
                        outer::KITTY_PUSH,
                        outer::BELL,
                    ]
                    .contains(&bytes.as_slice()) => {}
                Outgoing::Bytes(_, bytes) => {
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
                Outgoing::Exit(client, _) => {
                    self.s.detach(client);
                    self.shadows.remove(&client);
                }
                Outgoing::Shutdown(_) => self.shut = true,
            }
        }
        for pane in self.s.panes.values_mut() {
            pane.input.drain_all();
        }
        self.check_memo();
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
            if !fux::render::compose_into(&self.s, c, spare, placement) {
                continue;
            }
            let whole = fux::render::compose(&self.s, c);
            assert!(
                whole.as_ref() == Some(&*spare),
                "{c}: composed in part, not as whole"
            );
            let old = painted.then_some(&*shown);
            let fast = fux::render::paint(old, spare);
            let (mut plain_old, mut plain_new) = (shown.clone(), spare.clone());
            plain_old.forget_memo();
            plain_new.forget_memo();
            let slow = fux::render::paint(painted.then_some(&plain_old), &plain_new);
            assert!(fast == slow, "{c}: the memo's paint differs");
            assert_eq!(
                spare.same_as(shown),
                *spare == *shown,
                "{c}: same_as is not =="
            );
            std::mem::swap(spare, shown);
            *painted = true;
        }
    }

    /// Types `bytes` in pieces of `piece` bytes (all at once for 0).
    fn type_in(&mut self, client: ClientId, bytes: &[u8], piece: usize) {
        if let Some(shadow) = self.shadows.get_mut(&client) {
            shadow.bytes(bytes, &mut Vec::new());
        }
        match NonZeroUsize::new(piece) {
            Some(piece) => {
                for chunk in pieces(bytes, piece) {
                    self.s.input(client, chunk);
                }
            }
            None => self.s.input(client, bytes),
        }
    }

    /// Types `bytes` one at a time, checking that a key typed in a repeat
    /// mode reaches no pane.
    fn type_bytewise(&mut self, client: ClientId, bytes: &[u8]) {
        for byte in bytes {
            let mut inputs = Vec::new();
            if let Some(shadow) = self.shadows.get_mut(&client) {
                shadow.bytes(std::slice::from_ref(byte), &mut inputs);
            }
            let checked = self.repeat_key(client, &inputs);
            self.s.input(client, std::slice::from_ref(byte));
            if let Some(key) = checked {
                self.assert_no_pane_input(key);
            }
            // Between bytes, so the next byte's check starts empty.
            for pane in self.s.panes.values_mut() {
                pane.input.drain_all();
            }
        }
    }

    fn escape(&mut self, client: ClientId) {
        let mut inputs = Vec::new();
        if let Some(shadow) = self.shadows.get_mut(&client) {
            shadow.timeout(&mut inputs);
        }
        let checked = self.repeat_key(client, &inputs);
        self.s.escape(client);
        if let Some(key) = checked {
            self.assert_no_pane_input(key);
        }
    }

    /// The key `inputs` is, if it is one key, typed in a repeat mode, and
    /// not a key of a binding there that writes to a pane.
    fn repeat_key(&self, client: ClientId, inputs: &[Input]) -> Option<KeyPress> {
        let [Input::Key(stroke)] = inputs else {
            return None;
        };
        let press = &stroke.press;
        let Some(Mode::Repeat(repeat)) = self.s.views.get(&client).map(|v| &v.mode) else {
            return None;
        };
        let layer = self.s.config.bindings.layer(&repeat.path);
        let writes = matches!(layer.and_then(|l| l.get(*press)),
            Some(Node::Run { binding, .. }) if binding.command.first()
                .is_some_and(|c| WRITES.contains(&c.as_str())));
        (!writes).then_some(*press)
    }

    /// Types a mouse report in pieces of `piece` bytes (whole for 0), its
    /// client's held input flushed first so that the report is one: no pane may hear it but the focused pane of a
    /// client with nothing of fux's open, whose program asked for the mouse.
    fn mouse(&mut self, client: ClientId, report: &[u8], piece: usize) {
        self.escape(client);
        for pane in self.s.panes.values_mut() {
            pane.input.drain_all();
        }
        self.type_in(client, report, piece);
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

    fn assert_no_pane_input(&mut self, key: KeyPress) {
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
        let ls = self.s.run(&["ls".to_owned()], &Ctx::default()).stdout;
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
        (
            ls,
            screens,
            self.s.config.clone(),
            self.s.buffers.iter().cloned().collect(),
        )
    }
}

/// A mode as text: `Mode` has no `Debug` or `PartialEq`.
fn mode_text(mode: &Mode) -> String {
    match mode {
        Mode::Normal => "normal".into(),
        Mode::Column(column) => {
            let selected = column.entries.as_ref().map(Choice::index);
            format!("column {:?} {selected:?}", column.path)
        }
        Mode::Repeat(repeat) => format!("repeat {:?}", repeat.path),
        Mode::List(list) => format!("list {} {}", list.title, list.items.index()),
        Mode::Prompt(prompt) => {
            format!(
                "prompt {:?} {:?}",
                prompt.line.before(),
                prompt.line.after()
            )
        }
        Mode::Confirm(confirm) => format!("confirm {:?}", confirm.question),
        Mode::Copy(copy) => format!("copy {} {:?} {:?}", copy.pane, copy.top, copy.cursor),
    }
}

/// Everything that must hold between events.
fn check(s: &Session) {
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
        let bindings = &s.config.bindings;
        // Found again whenever the bindings change.
        match &view.mode {
            Mode::Column(column) => assert!(
                bindings.layer(&column.path).is_some(),
                "{id}'s column shows a layer that is gone: {:?}",
                column.path
            ),
            Mode::Repeat(repeat) => assert!(
                bindings.layer(&repeat.path).is_some_and(|l| l
                    .iter()
                    .any(|(_, node)| matches!(node, Node::Run { repeat: true, .. }))),
                "{id} repeats {:?}, which holds no repeating binding",
                repeat.path
            ),
            Mode::Normal | Mode::List(_) | Mode::Prompt(_) | Mode::Confirm(_) | Mode::Copy(_) => {}
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
    check(&given.s);
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
                    bytewise.type_bytewise(c, bytes);
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
                    given.s.detach(c);
                    bytewise.s.detach(c);
                }
            }
            11 | 12 => {
                let line = SIDE[usize::from(input.next()) % SIDE.len()];
                let argv: Vec<String> = line.split(' ').map(str::to_owned).collect();
                let a = given.s.run(&argv, &Ctx::default());
                let b = bytewise.s.run(&argv, &Ctx::default());
                assert_eq!(
                    (a.status, a.stdout, a.stderr),
                    (b.status, b.stdout, b.stderr)
                );
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
                    given.s.exited(p, 0);
                    bytewise.s.exited(p, 0);
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
        check(&given.s);
    }
    // Compared once, at the end: composing every screen after every
    // operation fills AddressSanitizer's quarantine past the RSS limit.
    assert!(
        given.state() == bytewise.state(),
        "the same bytes, one at a time, gave a different result"
    );
});

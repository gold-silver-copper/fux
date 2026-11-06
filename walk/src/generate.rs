//! Choosing the next step from the live state. Choices are small random
//! indices resolved against the session's listing when the step is made, and the step
//! records what they resolved to. Commands are grift programs.
use crate::rng::Rng;
use crate::step::{Act, Config, Step, Stop};
use crate::world::World;
use std::collections::BTreeMap;

/// What the generator remembers between steps.
pub struct State {
    /// The prefix the configuration file gives a client that attaches,
    /// as its byte.
    pub prefix: u8,
    /// Each client's prefix, by id: what the configuration file gave it
    /// as it attached. Keys are each client's own.
    pub prefixes: BTreeMap<String, u8>,
    /// Panes whose shell it stopped, by id.
    pub stopped: Vec<String>,
}

impl State {
    pub fn new() -> State {
        State {
            prefix: 0x02,
            prefixes: BTreeMap::new(),
            stopped: Vec::new(),
        }
    }

    /// The clients there are now: one new since the last step attached
    /// after it, with the configuration file as it is.
    pub fn clients(&mut self, clients: &[String]) {
        self.prefixes.retain(|id, _| clients.contains(id));
        for id in clients {
            self.prefixes.entry(id.clone()).or_insert(self.prefix);
        }
    }

    /// Keeps track of what a step changes that the generator relies on.
    pub fn after(&mut self, step: &Step) {
        match step {
            Step::Config(Config::PrefixA) => self.prefix = 0x01,
            // A configuration that fails gives the defaults.
            Step::Config(Config::PrefixB | Config::Valid | Config::Invalid) => self.prefix = 0x02,
            Step::Signal {
                pane,
                stop: Stop::Stop,
            } => {
                if !self.stopped.contains(pane) {
                    self.stopped.push(pane.clone());
                }
            }
            Step::Signal {
                pane,
                stop: Stop::Cont,
            } => self.stopped.retain(|p| p != pane),
            Step::Keys { .. }
            | Step::Cli(_)
            | Step::Resize { .. }
            | Step::Attach { .. }
            | Step::Hangup { .. }
            | Step::Child { .. }
            | Step::Exit { .. }
            | Step::TooBig => {}
        }
    }
}

/// The default bindings' keys after the prefix, layers and repeat modes
/// included.
const BINDINGS: &[&str] = &[
    "v", "s", "x", "z", "a", "c", "p", "n", "b", "e", "o", "q", "h", "j", "k", "l", "tn", "th",
    "tl", "tg", "tr", "tx", "ta", "tmh", "tml", "wn", "wh", "wl", "wg", "wr", "wx", "wa", "wmh",
    "wml", "rh", "rj", "rk", "rl", "mh", "mj", "mk", "ml",
];

/// Keys for overlays: arrows, paging, Enter, Esc, and letters lists take.
const OVERLAY: &[&str] = &[
    "\x1b[A", "\x1b[B", "\x1b[5~", "\x1b[6~", "\x1b[H", "\x1b[F", "\r", "\x1b", "j", "k", "y", "n",
    "q", "r", "x", "h", "l", "\x7f",
];

/// Letters, to whatever has the keys: a pane, or a screen's keys.
const LETTERS: &[u8] = b"hjklwbaeudtzfrnpvsxoyq";

/// Lines typed into the command prompt, grift the client runs as its
/// own, some of it wrong.
const PROMPT: &[&str] = &[
    "(split (axis (focused (me)) \"horizontal\"))\r",
    "(zoom (focused (me)))\r",
    "(list-session)\r",
    "(step (me) (tab (focused (me))) \"next\")\r",
    "nonsense\r",
    "(kill (pane \"%99\"))\r",
    "(rename-prompt (me) (tab (focused (me))))\r",
    "(bind-key \"C-b g l\" (code! (lambda! () (move-border (focused (me)) \"right\" 1) #inert))) (repeat \"C-b g\")\r",
];

/// Sizes a client's terminal is resized to: tiny, one row, one column,
/// and ordinary.
const SIZES: &[(u16, u16)] = &[
    (1, 1),
    (2, 2),
    (1, 200),
    (40, 1),
    (5, 20),
    (10, 30),
    (24, 80),
    (40, 120),
    (60, 200),
];

/// The next step.
pub fn step(rng: &mut Rng, world: &World, clients: &[String], state: &State) -> Step {
    let panes: Vec<&str> = world.panes().map(|p| p.id.as_str()).collect();
    // One step can close at most what one command or key closes: while
    // there are fewer than two panes, the walk makes one first.
    if panes.len() < 2 {
        let pane = panes.first().copied().unwrap_or("%1");
        return Step::Cli(format!(r#"(split (axis (pane "{pane}") "horizontal"))"#));
    }
    if clients.is_empty() {
        return Step::Attach { rows: 24, cols: 80 };
    }
    // Every stopped pane is continued within a few steps.
    if let Some(pane) = state.stopped.first()
        && rng.chance(30)
    {
        return Step::Signal {
            pane: pane.clone(),
            stop: Stop::Cont,
        };
    }
    let client = rng.pick(clients).cloned().unwrap_or_default();
    let pane = rng.pick(&panes).copied().unwrap_or("%1").to_owned();
    let prefix = state.prefixes.get(&client).copied().unwrap_or(state.prefix);
    match rng.weighted(&[45, 30, 6, 7, 2, 3, 2, 3, 1]) {
        0 => Step::Keys {
            client,
            bytes: keys(rng, prefix),
        },
        1 => Step::Cli(cli(rng, world, clients, state)),
        2 => {
            let (rows, cols) = rng.pick(SIZES).copied().unwrap_or((24, 80));
            Step::Resize { client, rows, cols }
        }
        3 => Step::Child {
            pane,
            act: match rng.below(7) {
                0 => Act::Alternate,
                1 => Act::Mouse,
                2 => Act::Bracketed,
                3 => Act::AppCursor,
                4 => Act::Stty(
                    u16::try_from(rng.below(30)).unwrap_or(0).saturating_add(1),
                    u16::try_from(rng.below(100)).unwrap_or(0).saturating_add(1),
                ),
                5 => Act::Burst(u32::try_from(rng.below(400)).unwrap_or(0).saturating_add(1)),
                _ => Act::Deaf(u32::try_from(rng.below(2)).unwrap_or(0).saturating_add(1)),
            },
        },
        4 => Step::Exit {
            pane,
            code: u8::try_from(rng.below(4)).unwrap_or(0),
        },
        5 => Step::Signal {
            pane,
            stop: Stop::Stop,
        },
        6 => Step::Config(match rng.below(4) {
            0 => Config::Valid,
            1 => Config::Invalid,
            2 => Config::PrefixA,
            _ => Config::PrefixB,
        }),
        7 => {
            if clients.len() < 3 && rng.chance(60) {
                let (rows, cols) = rng.pick(SIZES).copied().unwrap_or((24, 80));
                Step::Attach { rows, cols }
            } else if clients.len() > 1 {
                Step::Hangup { client }
            } else {
                Step::Attach {
                    rows: 30,
                    cols: 100,
                }
            }
        }
        _ => Step::TooBig,
    }
}

/// Keys a client types.
fn keys(rng: &mut Rng, prefix: u8) -> Vec<u8> {
    let p = [prefix];
    let mut out = Vec::new();
    match rng.weighted(&[30, 18, 8, 6, 4, 3, 3, 3, 3, 4, 6, 4]) {
        0 => {
            out.extend_from_slice(&p);
            out.extend_from_slice(rng.pick(BINDINGS).copied().unwrap_or("z").as_bytes());
        }
        1 => {
            for _ in 0..rng.below(3).saturating_add(1) {
                out.extend_from_slice(rng.pick(OVERLAY).copied().unwrap_or("\r").as_bytes());
            }
        }
        2 => {
            for _ in 0..rng.below(6).saturating_add(1) {
                out.push(rng.pick(LETTERS).copied().unwrap_or(b'j'));
            }
        }
        3 => {
            out.extend_from_slice(&p);
            out.push(b'e');
            out.extend_from_slice(rng.pick(PROMPT).copied().unwrap_or("ls\r").as_bytes());
        }
        // A lone Esc.
        4 => out.push(0x1b),
        // The prefix twice sends it.
        5 => {
            out.extend_from_slice(&p);
            out.extend_from_slice(&p);
        }
        // A letter no binding has, a capital, and a letter with Ctrl.
        6 => out.extend_from_slice(&[prefix, b'f']),
        7 => out.extend_from_slice(&[prefix, b'T', b'N']),
        8 => out.extend_from_slice(&[prefix, 0x14]),
        9 => out.extend_from_slice(b"\x1b[200~pasted text\nsecond line\x1b[201~"),
        10 => out.extend_from_slice(b"echo typed\r"),
        _ => out.extend_from_slice(&[prefix, b'd']),
    }
    out
}

/// A command line from the whole vocabulary, targets taken from the
/// state, some of them wrong.
fn cli(rng: &mut Rng, world: &World, clients: &[String], state: &State) -> String {
    let pane = |rng: &mut Rng| -> String {
        let panes: Vec<&str> = world.panes().map(|p| p.id.as_str()).collect();
        if rng.chance(5) {
            return "%999".into();
        }
        rng.pick(&panes).copied().unwrap_or("%1").to_owned()
    };
    let tab = |rng: &mut Rng| -> String {
        let tabs: Vec<&str> = world.tabs().map(|t| t.id.as_str()).collect();
        if rng.chance(5) {
            return "@999".into();
        }
        rng.pick(&tabs).copied().unwrap_or("@1").to_owned()
    };
    let workspace = |rng: &mut Rng| -> String {
        let ws: Vec<&str> = world.workspaces.iter().map(|w| w.id.as_str()).collect();
        if rng.chance(5) {
            return "+99".into();
        }
        rng.pick(&ws).copied().unwrap_or("+1").to_owned()
    };
    let client = |rng: &mut Rng| -> String {
        if rng.chance(5) {
            return "c99".into();
        }
        rng.pick(clients).cloned().unwrap_or_else(|| "c1".into())
    };
    let direction = |rng: &mut Rng| -> &'static str {
        rng.pick(&["left", "right", "up", "down"])
            .copied()
            .unwrap_or("left")
    };
    let kind = |rng: &mut Rng| -> &'static str {
        rng.pick(&["pane", "tab", "workspace"])
            .copied()
            .unwrap_or("pane")
    };
    // A client and its prefix, as a key's name (`C-b`).
    let prefixed = |rng: &mut Rng| -> (String, String) {
        let client = client(rng);
        let prefix = state.prefixes.get(&client).copied().unwrap_or(state.prefix);
        let key = format!("C-{}", char::from(prefix.saturating_add(0x60)));
        (client, key)
    };
    // The bundled keys' map for a pane, tab or workspace.
    let map =
        |rng: &mut Rng| -> &'static str { rng.pick(&["a", "t", "w"]).copied().unwrap_or("a") };
    let panes = world.panes().count();
    let tabs = world.tabs().count();
    let spaces = world.workspaces.len();
    match rng.below(40) {
        0 => format!(
            r#"(new-tab (in (size (options) 80 23) (workspace "{}")))"#,
            workspace(rng)
        ),
        1 => format!(
            r#"(new-tab (name (in (size (options) 80 23) (workspace "{}")) "t{}"))"#,
            workspace(rng),
            rng.below(9)
        ),
        2 => format!(
            r#"(new-tab (in (size (options) 80 23) "w{}"))"#,
            rng.below(9)
        ),
        3 => format!(r#"(split (axis (pane "{}") "horizontal"))"#, pane(rng)),
        4 => format!(
            r#"(split (program-clipboard (axis (pane "{}") "horizontal") #{}))"#,
            pane(rng),
            rng.chance(50)
        ),
        5 => format!(
            r#"(split (argument (program (axis (pane "{}") "vertical") "echo") "split-typed"))"#,
            pane(rng)
        ),
        6 if panes > 2 => format!(r#"(kill (pane "{}"))"#, pane(rng)),
        7 if tabs > 1 && panes > 2 => format!(r#"(kill (tab "{}"))"#, tab(rng)),
        8 if spaces > 1 && panes > 2 => format!(r#"(kill (workspace "{}"))"#, workspace(rng)),
        9 => format!(
            r#"(rename (pane "{}") "renamed-{}")"#,
            pane(rng),
            rng.below(9)
        ),
        10 => format!(r#"(rename (tab "{}") "tab-{}")"#, tab(rng), rng.below(9)),
        11 => format!(
            r#"(move (pane "{}") {})"#,
            pane(rng),
            match rng.below(4) {
                0 => format!(r#"(tab "{}")"#, tab(rng)),
                1 => format!(r#"(workspace "{}")"#, workspace(rng)),
                2 => r#""new-tab""#.into(),
                _ => r#""new-workspace""#.into(),
            }
        ),
        12 => format!(r#"(move (pane "{}") "{}")"#, pane(rng), direction(rng)),
        13 => format!(
            r#"(swap-with (pane "{}") (pane "{}"))"#,
            pane(rng),
            pane(rng)
        ),
        14 => format!(r#"(swap-with (pane "{}") "{}")"#, pane(rng), direction(rng)),
        15 => format!(
            r#"(move-border (pane "{}") "{}" {})"#,
            pane(rng),
            direction(rng),
            rng.below(6)
        ),
        16 => format!(
            r#"(reorder ({} "{}") "{}")"#,
            kind(rng),
            match rng.below(3) {
                0 => pane(rng),
                1 => tab(rng),
                _ => workspace(rng),
            },
            if rng.chance(50) { "next" } else { "previous" }
        ),
        17 => format!(
            r#"(show (client "{}") (pane "{}"))"#,
            client(rng),
            pane(rng)
        ),
        18 => {
            let client = client(rng);
            format!(
                r#"(step (client "{client}") (focused (client "{client}")) "{}")"#,
                rng.pick(&["next", "previous", "last", "left", "right", "up", "down"])
                    .copied()
                    .unwrap_or("next")
            )
        }
        19 => format!(r#"(show (client "{}") (tab "{}"))"#, client(rng), tab(rng)),
        20 => {
            let client = client(rng);
            format!(
                r#"(step (client "{client}") (tab (focused (client "{client}"))) "{}")"#,
                rng.pick(&["next", "previous", "last"])
                    .copied()
                    .unwrap_or("next")
            )
        }
        21 => format!(
            r#"(show (client "{}") (workspace "{}"))"#,
            client(rng),
            workspace(rng)
        ),
        22 => format!(r#"(zoom (pane "{}"))"#, pane(rng)),
        // The prefix sent to a client is taken as typed there: it opens
        // the column if it is that client's prefix.
        23 => match rng.below(4) {
            0 => format!(r#"(send-keys (key (client "{}") "C-b"))"#, client(rng)),
            1 => format!(r#"(command-prompt (client "{}"))"#, client(rng)),
            // A tab's or workspace's chooser, as its key opens it.
            n => {
                let (client, prefix) = prefixed(rng);
                let map = if n == 2 { "t" } else { "w" };
                format!(r#"(send-keys (text (key (client "{client}") "{prefix}") "{map}g"))"#)
            }
        },
        24 => {
            let client = client(rng);
            let focused = format!(r#"(focused (client "{client}"))"#);
            let target = match kind(rng) {
                "pane" => focused,
                kind => format!("({kind} {focused})"),
            };
            format!(r#"(choose-among (client "{client}") {target})"#)
        }
        // The pane's, tab's or workspace's rename prompt, then its close
        // confirmation, as their keys open them.
        n @ (25 | 26) => {
            let (client, prefix) = prefixed(rng);
            let key = if n == 25 { "r" } else { "x" };
            format!(
                r#"(send-keys (text (key (client "{client}") "{prefix}") "{}{key}"))"#,
                map(rng)
            )
        }
        27 => match rng.below(2) {
            0 => format!(r#"(history-budget (pane "{}") 0)"#, pane(rng)),
            _ => format!(r#"(history-budget (pane "{}") 100000)"#, pane(rng)),
        },
        28 => "(dump (all))".into(),
        29 => format!(
            r#"(send-keys (text (pane "{}") "typed-by-send-keys"))"#,
            pane(rng)
        ),
        30 => format!(r#"(send-keys (key (pane "{}") "Enter"))"#, pane(rng)),
        31 => format!(r#"(capture (history (pane "{}") 20))"#, pane(rng)),
        32 => "(list-session)".into(),
        33 => "(list-keys)".into(),
        34 => format!(r#"(send-keys (paste (pane "{}") "pasted"))"#, pane(rng)),
        35 => format!(r#"(capture (all-history (pane "{}")))"#, pane(rng)),
        36 => format!(r#"(signal (pane "{}") "TERM")"#, pane(rng)),
        37 if clients.len() > 1 => format!(r#"(detach (client "{}"))"#, client(rng)),
        38 => format!(r#"(capture (client "{}"))"#, client(rng)),
        _ => {
            let client = client(rng);
            format!(
                r#"(step (client "{client}") (workspace (focused (client "{client}"))) "{}")"#,
                rng.pick(&["next", "previous", "last"])
                    .copied()
                    .unwrap_or("next")
            )
        }
    }
}

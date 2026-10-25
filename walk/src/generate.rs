//! Choosing the next step from the live state. Choices are small random
//! indices resolved against `ls json` when the step is made, and the step
//! records what they resolved to. Commands are fuxth programs.
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

/// Keys for overlays: arrows, paging, Enter, Esc, and letters lists and
/// confirmations take.
const OVERLAY: &[&str] = &[
    "\x1b[A", "\x1b[B", "\x1b[5~", "\x1b[6~", "\x1b[H", "\x1b[F", "\r", "\x1b", "j", "k", "y", "n",
    "q", "r", "x", "h", "l", "\x7f",
];

/// Copy mode's letters.
const COPY: &[u8] = b"hjklwbaeudtzfrnpvsxoyq";

/// Lines typed into the command prompt, fuxth the client runs as its
/// own, some of it wrong.
const PROMPT: &[&str] = &[
    "me focused opts horizontal split drop\r",
    "me zoom\r",
    "ls\r",
    "me next-tab\r",
    "nonsense\r",
    "str %99 pane kill\r",
    "me me focused tab rename-prompt\r",
    "me copy-mode\r",
    "str 'g l' str 'me focused str right 1 move-border' bind repeat\r",
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

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_owned).collect()
}

/// The next step.
pub fn step(rng: &mut Rng, world: &World, clients: &[String], state: &State) -> Step {
    let panes: Vec<&str> = world.panes().map(|p| p.id.as_str()).collect();
    // One step can close at most what one command or key closes: while
    // there are fewer than two panes, the walk makes one first.
    if panes.len() < 2 {
        let pane = panes.first().copied().unwrap_or("%1");
        return Step::Cli(words(&format!("str {pane} pane horizontal split")));
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
        1 => Step::Cli(cli(rng, world, clients)),
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
                out.push(rng.pick(COPY).copied().unwrap_or(b'j'));
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
fn cli(rng: &mut Rng, world: &World, clients: &[String]) -> Vec<String> {
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
    let panes = world.panes().count();
    let tabs = world.tabs().count();
    let spaces = world.workspaces.len();
    let line = match rng.below(40) {
        0 => format!(
            "opts 80 24 size str {} workspace in new-tab",
            workspace(rng)
        ),
        1 => format!(
            "opts 80 24 size str {} workspace in str t{} name new-tab",
            workspace(rng),
            rng.below(9)
        ),
        2 => format!("opts 80 24 size str w{} name new-workspace", rng.below(9)),
        3 | 4 => format!("str {} pane horizontal split", pane(rng)),
        5 => format!(
            "str {} pane vertical str echo program str split-typed arg split",
            pane(rng)
        ),
        6 if panes > 2 => format!("str {} pane kill", pane(rng)),
        7 if tabs > 1 && panes > 2 => format!("str {} tab kill", tab(rng)),
        8 if spaces > 1 && panes > 2 => format!("str {} workspace kill", workspace(rng)),
        9 => format!("str {} pane str renamed-{} rename", pane(rng), rng.below(9)),
        10 => format!("str {} tab str tab-{} rename", tab(rng), rng.below(9)),
        11 => format!(
            "str {} pane {} move-to",
            pane(rng),
            match rng.below(4) {
                0 => format!("str {} tab", tab(rng)),
                1 => format!("str {} workspace", workspace(rng)),
                2 => "str new-tab".into(),
                _ => "str new-workspace".into(),
            }
        ),
        12 => format!("str {} pane str {} move-beside", pane(rng), direction(rng)),
        13 => format!("str {} pane str {} pane swap-panes", pane(rng), pane(rng)),
        14 => format!("str {} pane str {} swap-toward", pane(rng), direction(rng)),
        15 => format!(
            "str {} pane str {} {} move-border",
            pane(rng),
            direction(rng),
            rng.below(6)
        ),
        16 => format!(
            "str {} {} str {} reorder",
            match rng.below(3) {
                0 => pane(rng),
                1 => tab(rng),
                _ => workspace(rng),
            },
            kind(rng),
            if rng.chance(50) { "next" } else { "previous" }
        ),
        17 => format!(
            "str {} client str {} pane focus-pane",
            client(rng),
            pane(rng)
        ),
        18 => format!(
            "str {} client str {} focus",
            client(rng),
            rng.pick(&["next", "previous", "last", "left", "right", "up", "down"])
                .copied()
                .unwrap_or("next")
        ),
        19 => format!("str {} client str {} tab show-tab", client(rng), tab(rng)),
        20 => format!("str {} client next-tab", client(rng)),
        21 => format!(
            "str {} client str {} workspace show-workspace",
            client(rng),
            workspace(rng)
        ),
        22 => format!("str {} client zoom", client(rng)),
        // The prefix sent to a client is taken as typed there: it opens
        // the column if it is that client's prefix.
        23 => match rng.below(5) {
            0 => format!("str {} client str C-b key send-keys", client(rng)),
            n => format!(
                "str {} client {}",
                client(rng),
                match n {
                    1 => "command-prompt",
                    2 => "copy-mode",
                    3 => "choose-tab",
                    _ => "choose-workspace",
                }
            ),
        },
        24 => format!("str {} client dup focused {} menu", client(rng), kind(rng)),
        25 => format!(
            "str {} client dup focused {} rename-prompt",
            client(rng),
            kind(rng)
        ),
        26 => format!(
            "str {} client dup focused {} confirm-close",
            client(rng),
            kind(rng)
        ),
        27 => rng
            .pick(&[
                "str clipboard false set",
                "str clipboard true set",
                "str history-bytes 100000 set",
                "str buffers 4 set",
            ])
            .copied()
            .unwrap_or("str buffers 4 set")
            .to_owned(),
        28 => "dump".into(),
        29 => format!(
            "str {} pane str typed-by-send-keys text send-keys",
            pane(rng)
        ),
        30 => format!("str {} pane str Enter key send-keys", pane(rng)),
        31 => format!("str {} pane 20 history capture", pane(rng)),
        32 => "ls".into(),
        33 => "list-keys".into(),
        34 => "buffers".into(),
        35 => format!("str {} pane 0 paste-buffer", pane(rng)),
        36 => format!("str {} pane str TERM signal", pane(rng)),
        37 if clients.len() > 1 => format!("str {} client detach", client(rng)),
        38 => format!("str {} client capture-client", client(rng)),
        _ => format!("str {} client next-workspace", client(rng)),
    };
    words(&line)
}

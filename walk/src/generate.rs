//! Choosing the next step from the live state. Choices are small random
//! indices resolved against `ls --json` when the step is made, and the
//! step records what they resolved to.
use crate::rng::Rng;
use crate::step::{Act, Config, Step, Stop};
use crate::world::World;

/// What the generator remembers between steps.
pub struct State {
    /// The prefix key, as its byte.
    pub prefix: u8,
    /// Panes whose shell it stopped, by id.
    pub stopped: Vec<String>,
}

impl State {
    pub fn new() -> State {
        State {
            prefix: 0x02,
            stopped: Vec::new(),
        }
    }

    /// Keeps track of what a step changes that the generator relies on.
    pub fn after(&mut self, step: &Step) {
        match step {
            Step::Config(Config::PrefixA) => self.prefix = 0x01,
            Step::Config(Config::PrefixB | Config::Valid) => self.prefix = 0x02,
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
            Step::Config(Config::Invalid)
            | Step::Keys { .. }
            | Step::Cli(_)
            | Step::Resize { .. }
            | Step::Attach { .. }
            | Step::Hangup { .. }
            | Step::Child { .. }
            | Step::Exit { .. }
            | Step::Clamp => {}
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

/// Lines typed into the command prompt, some of them wrong.
const PROMPT: &[&str] = &[
    "split -h\r",
    "zoom\r",
    "ls\r",
    "select-tab --next\r",
    "nonsense\r",
    "kill-pane -t %99\r",
    "rename-prompt tab\r",
    "copy-mode\r",
    "bind -r g l resize-pane -R\r",
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
        return Step::Cli(words(&format!("split -h -t {pane}")));
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
    match rng.weighted(&[45, 30, 6, 7, 2, 3, 2, 3, 1]) {
        0 => Step::Keys {
            client,
            bytes: keys(rng, state.prefix),
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
        _ => Step::Clamp,
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

/// A command line from the whole command set, targets taken from the
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
        rng.pick(&["-L", "-R", "-U", "-D"]).copied().unwrap_or("-L")
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
        0 => format!("new-tab -t {}", workspace(rng)),
        1 => format!("new-tab -t {} -n t{}", workspace(rng), rng.below(9)),
        2 => format!("new-workspace -n w{}", rng.below(9)),
        3 | 4 => format!("split -h -t {}", pane(rng)),
        5 => format!("split -v -t {} -- echo split-typed", pane(rng)),
        6 if panes > 2 => format!("kill-pane -t {}", pane(rng)),
        7 if tabs > 1 && panes > 2 => format!("kill-tab -t {}", tab(rng)),
        8 if spaces > 1 && panes > 2 => format!("kill-workspace -t {}", workspace(rng)),
        9 => format!("rename -t {} renamed-{}", pane(rng), rng.below(9)),
        10 => format!("rename -t {} tab-{}", tab(rng), rng.below(9)),
        11 => format!(
            "move-pane -t {} --to {}",
            pane(rng),
            match rng.below(4) {
                0 => tab(rng),
                1 => workspace(rng),
                2 => "new-tab".into(),
                _ => "new-workspace".into(),
            }
        ),
        12 => format!("move-pane -t {} {}", pane(rng), direction(rng)),
        13 => format!("swap-pane -t {} {}", pane(rng), pane(rng)),
        14 => format!("swap-pane -t {} {}", pane(rng), direction(rng)),
        15 => format!(
            "resize-pane -t {} {} {}",
            pane(rng),
            direction(rng),
            rng.below(6)
        ),
        16 => format!(
            "reorder {} -t {} {}",
            kind(rng),
            match rng.below(3) {
                0 => pane(rng),
                1 => tab(rng),
                _ => workspace(rng),
            },
            if rng.chance(50) {
                "--next"
            } else {
                "--previous"
            }
        ),
        17 => format!("select-pane -c {} -t {}", client(rng), pane(rng)),
        18 => format!(
            "select-pane -c {} {}",
            client(rng),
            rng.pick(&["--next", "--previous", "--last", "-L", "-R", "-U", "-D"])
                .copied()
                .unwrap_or("--next")
        ),
        19 => format!("select-tab -c {} -t {}", client(rng), tab(rng)),
        20 => format!("select-tab -c {} --next", client(rng)),
        21 => format!("select-workspace -c {} -t {}", client(rng), workspace(rng)),
        22 => format!("zoom -c {}", client(rng)),
        23 => format!(
            "{} -c {}",
            rng.pick(&[
                "command-column",
                "command-prompt",
                "copy-mode",
                "choose-tab",
                "choose-workspace"
            ])
            .copied()
            .unwrap_or("command-column"),
            client(rng)
        ),
        24 => format!("menu -c {} {}", client(rng), kind(rng)),
        25 => format!("rename-prompt -c {} {}", client(rng), kind(rng)),
        26 => format!("confirm-close -c {} {}", client(rng), kind(rng)),
        27 => rng
            .pick(&[
                "bind -r g h resize-pane -L",
                "bind g n new-tab",
                "unbind g",
                "unbind t",
                "bind t n new-tab",
                "unbind-all",
                "set clipboard off",
                "set clipboard on",
                "set history-lines 100",
                "set buffers 4",
            ])
            .copied()
            .unwrap_or("unbind g")
            .to_owned(),
        28 => "reload".into(),
        29 => format!("send-keys -t {} -l typed-by-send-keys", pane(rng)),
        30 => format!("send-keys -t {} Enter", pane(rng)),
        31 => format!("capture-pane -t {} -S -20", pane(rng)),
        32 => "ls".into(),
        33 => "list-keys".into(),
        34 => "list-buffers".into(),
        35 => format!("paste-buffer -t {}", pane(rng)),
        36 => format!("terminate -t {}", pane(rng)),
        37 if clients.len() > 1 => format!("detach -c {}", client(rng)),
        38 => format!("capture-client -c {}", client(rng)),
        _ => format!("select-workspace -c {} --next", client(rng)),
    };
    words(&line)
}

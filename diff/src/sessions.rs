//! Whole sessions, the baseline's and the current one, driven by the same
//! random events: keys through every mode and overlay, pastes, commands
//! with and without a client, program output, resizes, attaches, detaches,
//! exits and shutdowns. After every event, what it returned and the whole
//! observable state are compared; `screens` compares, as well, what each
//! client is shown and the bytes that paint it, from nothing and from the
//! screen before.
use crate::rng::Rng;
use crate::{Outcome, bump, same, same_lines, times};
use std::collections::BTreeMap;

#[derive(Clone)]
pub enum Event {
    Key(u32, Vec<u8>),
    Escape(u32),
    Run(Vec<String>, Option<u32>, Option<u32>),
    Output(u32, Vec<u8>),
    Resize(u32, u16, u16),
    Attach(u16, u16, Option<String>),
    Detach(u32),
    Exited(u32, i32),
    /// The pane's program reads this many bytes of its input.
    Drain(u32, usize),
    /// The client was painted: its view is clean.
    Painted(u32),
    Notice(u32, bool, String),
    Shutdown,
}

/// Bytes as text, long runs cut short, for a report.
fn shown(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out: String = text.chars().take(60).collect();
    if text.chars().nth(60).is_some() {
        out.push_str(&format!("… ({} bytes)", bytes.len()));
    }
    format!("{out:?}")
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Key(c, bytes) => write!(f, "keys c{c} {}", shown(bytes)),
            Event::Escape(c) => write!(f, "escape due c{c}"),
            Event::Run(argv, client, pane) => {
                write!(f, "run {argv:?} client {client:?} pane {pane:?}")
            }
            Event::Output(p, bytes) => write!(f, "output %{p} {}", shown(bytes)),
            Event::Resize(c, rows, cols) => write!(f, "resize c{c} {rows}x{cols}"),
            Event::Attach(rows, cols, ws) => write!(f, "attach {rows}x{cols} {ws:?}"),
            Event::Detach(c) => write!(f, "detach c{c}"),
            Event::Exited(p, status) => write!(f, "exited %{p} {status}"),
            Event::Drain(p, n) => write!(f, "drain %{p} {n}"),
            Event::Painted(c) => write!(f, "painted c{c}"),
            Event::Notice(c, error, text) => write!(f, "notice c{c} error {error} {text:?}"),
            Event::Shutdown => f.write_str("shutdown"),
        }
    }
}

/// What the generator needs to know of a session: the IDs and names there are.
#[derive(Default)]
pub struct World {
    clients: Vec<u32>,
    panes: Vec<u32>,
    names: Vec<String>,
    tabs: Vec<u32>,
    workspaces: Vec<u32>,
}

/// The same code for the baseline's crates and the current ones.
macro_rules! stack {
    ($name:ident, $fux:ident, $id:ident, $layout:ident, $side:ident) => {
        pub mod $name {
            use super::Event;
            use std::collections::BTreeMap;
            use std::fmt::Write;
            use $fux::$id::ClientId;
            use $fux::$layout::PaneId;
            use $fux::config::Config;
            use $fux::render::Grid;
            use $fux::session::Session;
            use $fux::view::Mode;

            pub fn make(history: usize, clipboard: bool, buffers: usize) -> Result<Session, String> {
                let mut config = Config::default();
                config.history_lines = history;
                config.clipboard = clipboard;
                config.buffers = buffers;
                let mut s = Session::new(config, "/nonexistent/fux.sock".into(), false);
                s.start().map_err(|e| e.to_string())?;
                Ok(s)
            }

            /// Client `n` as a person names it, which both sides read alike:
            /// the current side makes IDs from nothing else.
            fn client(n: &u32) -> Option<ClientId> {
                $fux::command::parse_client(&n.to_string()).ok()
            }

            /// Pane `n`, likewise.
            fn pane(n: &u32) -> Option<PaneId> {
                $fux::command::parse_pane(&format!("%{n}")).ok()
            }

            /// Applies an event; what it returned, if anything.
            pub fn apply(s: &mut Session, e: &Event) -> String {
                match e {
                    Event::Key(c, bytes) => client(c).into_iter().for_each(|c| s.input(c, bytes)),
                    Event::Escape(c) => client(c).into_iter().for_each(|c| s.escape(c)),
                    Event::Run(argv, c, p) => {
                        let (c, p) = (c.as_ref().and_then(client), p.as_ref().and_then(pane));
                        return format!("{:?}", s.run(argv, &super::$side::origin(c, p)));
                    }
                    Event::Output(p, bytes) => pane(p).into_iter().for_each(|p| s.output(p, bytes)),
                    Event::Resize(c, rows, cols) => {
                        client(c).into_iter().for_each(|c| s.resize(c, *rows, *cols))
                    }
                    Event::Attach(rows, cols, ws) => {
                        return match s.attach(*rows, *cols, ws.as_deref()) {
                            Ok(id) => id.to_string(),
                            Err(error) => format!("error: {error}"),
                        };
                    }
                    Event::Detach(c) => client(c).into_iter().for_each(|c| s.detach(c)),
                    Event::Exited(p, status) => pane(p).into_iter().for_each(|p| s.exited(p, *status)),
                    Event::Drain(p, n) => {
                        if let Some(pane) = pane(p).and_then(|p| s.panes.get_mut(&p)) {
                            let queued = pane.input.front().map_or(0, <[u8]>::len);
                            pane.input.advance((*n).min(queued));
                        }
                    }
                    Event::Painted(c) => {
                        if let Some(view) = client(c).and_then(|c| s.views.get_mut(&c)) {
                            view.dirty = false;
                        }
                    }
                    Event::Notice(c, error, text) => {
                        if let Some(view) = client(c).and_then(|c| s.views.get_mut(&c)) {
                            if *error {
                                view.error(text.clone());
                            } else {
                                view.info(text.clone());
                            }
                        }
                    }
                    Event::Shutdown => s.shutdown(),
                }
                String::new()
            }

            fn mode(mode: &Mode) -> String {
                match mode {
                    Mode::Normal => "normal".into(),
                    Mode::Confirm(confirm) => format!("{confirm:?}"),
                    Mode::Copy(c) => format!(
                        "copy {} top {:?} cursor {:?} selection {:?} search {:?} typing {:?} held {:?}",
                        c.pane,
                        c.top,
                        c.cursor,
                        c.selection.map(|(kind, at)| (format!("{kind:?}"), at)),
                        c.search.as_ref().map(|s| (s.query.clone(), format!("{:?}", s.seek))),
                        c.typing.as_ref().map(|(seek, text)| (format!("{seek:?}"), text.clone())),
                        c.held_at
                    ),
                    other => super::$side::overlay(other),
                }
            }

            /// Everything a session shows of itself, a line for each part;
            /// the outbox and the dying are taken, as the server takes them.
            pub fn state(s: &mut Session) -> String {
                let mut out = String::new();
                for v in s.views.values() {
                    let _ = writeln!(
                        out,
                        "view {} {}x{} {} zoom {} dirty {} notice {:?} {}",
                        v.id,
                        v.rows,
                        v.cols,
                        super::$side::place(s, v),
                        v.zoom,
                        v.dirty,
                        v.notice,
                        mode(&v.mode)
                    );
                }
                for (id, p) in &s.panes {
                    let _ = writeln!(
                        out,
                        "pane {id} {:?} title {:?} {:?} queued {} typed {}",
                        p.name,
                        p.title,
                        p.size,
                        super::shown(p.input.front().unwrap_or_default()),
                        p.typed.is_some()
                    );
                }
                for ws in &s.workspaces {
                    let _ = writeln!(out, "workspace {} {:?}", ws.id, ws.name);
                    for (t, root) in super::$side::tabs(ws) {
                        let root = root.map(crate::layout::Shape::shape);
                        let _ = writeln!(out, "  tab {} {:?} {root:?}", t.id, t.name);
                    }
                }
                let _ = writeln!(
                    out,
                    "outbox {:?}\nbuffers {:?}\ndying {} bindings {} prefix {}",
                    s.outbox,
                    s.buffers,
                    s.dying.len(),
                    s.config.bindings.len(),
                    s.config.prefix
                );
                s.outbox.clear();
                s.dying.clear();
                out
            }

            /// Every client's paint, from nothing and from the screen it was
            /// last painted, each after its client's number.
            pub fn paints(s: &Session, shown: &mut BTreeMap<ClientId, Grid>) -> Vec<u8> {
                let mut out = Vec::new();
                for c in s.views.keys() {
                    out.extend_from_slice(c.to_string().as_bytes());
                    if let Some(grid) = $fux::render::compose(s, *c) {
                        out.extend($fux::render::paint(None, &grid));
                        out.extend($fux::render::paint(shown.get(c), &grid));
                        shown.insert(*c, grid);
                    }
                }
                out
            }

        }
    };
}

stack!(base, baseline, command, layout, base_side);
stack!(cur, fux, id, id, cur_side);

/// The column, a repeat mode, a list and a prompt as each side keeps them,
/// a client's place and a workspace's tabs with their layouts, written alike.
mod base_side {
    use baseline::command::ClientId;
    use baseline::layout::Node;
    use baseline::layout::PaneId;
    use baseline::session::{Ctx, Session, Tab, Workspace};
    use baseline::view::{Mode, View};

    pub fn origin(c: Option<ClientId>, p: Option<PaneId>) -> Ctx {
        let cwd = Some("/".into());
        Ctx {
            client: c,
            pane: p,
            cwd,
        }
    }

    pub fn place(s: &Session, v: &View) -> String {
        let ws = s.workspace(v.workspace).map(|w| w.id);
        format!("{ws:?} tab {:?} focus {:?}", v.tab(), v.focus())
    }

    pub fn tabs(w: &Workspace) -> impl Iterator<Item = (&Tab, Option<&Node>)> {
        w.tabs.iter().map(|t| (t, t.root.as_ref()))
    }

    pub fn overlay(mode: &Mode) -> String {
        match mode {
            Mode::Column { path, selected } => format!("column {path:?} {selected}"),
            Mode::Repeat { path } => format!("repeat {path:?}"),
            Mode::List(l) => format!(
                "list {:?} {:?} {} {:?}",
                l.title, l.items, l.selected, l.about
            ),
            Mode::Prompt(p) => format!(
                "prompt {:?} {:?} {:?} {}",
                p.title, p.purpose, p.text, p.cursor
            ),
            Mode::Normal | Mode::Confirm(_) | Mode::Copy(_) => String::new(),
        }
    }
}

mod cur_side {
    use fux::id::{ClientId, PaneId};
    use fux::layout::Tree;
    use fux::session::{Origin, Session};
    use fux::view::{Choice, Mode, View};
    use fux::workspace::{Tab, Workspace};

    /// A client's command, or the CLI's: a command has one origin.
    pub fn origin(c: Option<ClientId>, p: Option<PaneId>) -> Origin {
        let cwd = Some("/".into());
        c.map_or(Origin::Cli { pane: p, cwd }, Origin::Client)
    }

    pub fn place(s: &Session, v: &View) -> String {
        let (ws, tab) = (s.shown_workspace(v.id), s.shown_tab(v.id));
        let (ws, tab) = (ws.map(|w| w.id), tab.map(|t| t.id));
        format!("{ws:?} tab {tab:?} focus {:?}", s.focused(v.id))
    }

    pub fn tabs(w: &Workspace) -> impl Iterator<Item = (&Tab, Option<&Tree>)> {
        w.tabs().iter().map(|t| (t, t.root()))
    }

    pub fn overlay(mode: &Mode) -> String {
        match mode {
            Mode::Column(c) => {
                let selected = c.entries.as_ref().map_or(0, Choice::index);
                format!("column {:?} {selected}", c.path)
            }
            Mode::Repeat(r) => format!("repeat {:?}", r.path),
            Mode::List(l) => {
                let items: Vec<_> = l.items.iter().collect();
                format!(
                    "list {:?} {items:?} {} {:?}",
                    l.title,
                    l.items.index(),
                    l.about
                )
            }
            Mode::Prompt(p) => {
                let (before, after) = (p.line.before(), p.line.after());
                let text = format!("{before}{after}");
                let cursor = before.chars().count();
                format!("prompt {:?} {:?} {text:?} {cursor}", p.title, p.purpose)
            }
            Mode::Normal | Mode::Confirm(_) | Mode::Copy(_) => String::new(),
        }
    }
}

/// Whether any of the baseline's clients is in copy mode, a list or a
/// repeat mode: for the summary.
fn modes(s: &baseline::session::Session) -> [bool; 3] {
    use baseline::view::Mode;
    let any = |f: fn(&Mode) -> bool| s.views.values().any(|v| f(&v.mode));
    [
        any(|m| matches!(m, Mode::Copy(_))),
        any(|m| matches!(m, Mode::List(_))),
        any(|m| matches!(m, Mode::Repeat { .. })),
    ]
}

/// The IDs and names in the baseline's session, which events are chosen
/// among.
fn world(s: &baseline::session::Session) -> World {
    World {
        clients: s.views.keys().map(|c| c.0).collect(),
        panes: s.panes.keys().map(|p| p.0).collect(),
        names: s.workspaces.iter().map(|w| w.name.clone()).collect(),
        tabs: s
            .workspaces
            .iter()
            .flat_map(|w| w.tabs.iter().map(|t| t.id.0))
            .collect(),
        workspaces: s.workspaces.iter().map(|w| w.id.0).collect(),
    }
}

fn one(r: &mut Rng, items: &[&str]) -> String {
    r.pick(items).copied().unwrap_or_default().to_owned()
}

fn id(r: &mut Rng, ids: &[u32], missing: u32, percent_missing: u64) -> u32 {
    if r.chance(percent_missing) {
        return missing;
    }
    r.pick(ids).copied().unwrap_or(missing)
}

const KEYS: &[&[u8]] = &[
    b"\x02",
    b"\x02",
    b"\x02",
    b"\x02",
    b"h",
    b"j",
    b"k",
    b"l",
    b"o",
    b"q",
    b"v",
    b"s",
    b"x",
    b"z",
    b"a",
    b"c",
    b"p",
    b"r",
    b"m",
    b"n",
    b"b",
    b"t",
    b"w",
    b"e",
    b"g",
    b"d",
    b"y",
    b"Y",
    b"N",
    b"\r",
    b"\x1b",
    b"\x1b[A",
    b"\x1b[B",
    b"\x1b[C",
    b"\x1b[D",
    b"\x1b[5~",
    b"\x1b[6~",
    b"\x1b[H",
    b"\x1b[F",
    b"\x7f",
    b"\x1b[3~",
    b"\t",
    b"u",
    b"f",
    b"i",
    b"H",
    b"L",
    b"Q",
    b"\x03",
    b"\x1bh",
    b"1",
    b"-",
    b" ",
    b"\x1b[I",
    b"\x1b[O",
    b"\x1b[200~pasted text\x1b[201~",
    b"\x1b[200~line\nnext\x1b[201~",
    b"\x1b[1;5A",
    b"\x1bOP",
    "é".as_bytes(),
    "界".as_bytes(),
    b"'",
    b"\"",
    b"%",
    b"@",
    b"+",
];

const OUTPUT: &[&[u8]] = &[
    b"hello world ",
    "界界 wide ".as_bytes(),
    b"\r\n",
    b"\r\n\r\n\r\n\r\n\r\n",
    b"\x1b[6n",
    b"\x1b[c",
    b"\x1b[2J",
    b"\x1b[?1049h",
    b"\x1b[?1049l",
    b"\x1bc",
    b"\x1b[?1004h",
    b"\x1b[?2004h",
    b"\x1b[?1h",
    b"\x1b]2;title\x07",
    b"word_one two-three  four\r\n",
    b"\x1b[5;3H",
    b"FIND me find ME\r\n",
    b"\x1b[1;2;3;4;7mall\x1b[0m \x1b[2;3mdim italic\x1b[m",
    b"\x1b[3;4;38;5;200;48;2;1;2;3mx\x1b[0m",
];

/// A bracketed paste of `len` copies of `byte`.
fn paste(byte: u8, len: usize) -> Vec<u8> {
    let mut out = b"\x1b[200~".to_vec();
    out.extend(std::iter::repeat_n(byte, len));
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// A command line, mostly of the commands that change a session, with
/// targets that exist and some that do not.
fn line(r: &mut Rng, w: &World) -> Vec<String> {
    let p = format!("%{}", id(r, &w.panes, 99, 8));
    let t = format!("@{}", id(r, &w.tabs, 77, 10));
    let ws = if r.chance(30) && !w.names.is_empty() {
        r.pick(&w.names).cloned().unwrap_or_default()
    } else {
        format!("+{}", id(r, &w.workspaces, 55, 10))
    };
    let any = match r.below(3) {
        0 => p.clone(),
        1 => t.clone(),
        _ => ws.clone(),
    };
    let dir = one(r, &["-L", "-R", "-U", "-D"]);
    let pick = one(r, &["--next", "--previous", "--last"]);
    let kind = one(r, &["pane", "tab", "workspace"]);
    let s = |text: &str| text.to_owned();
    let mut words: Vec<String> = match r.below(41) {
        0..=15 => {
            let name = one(r, &["select-pane", "select-tab", "select-workspace"]);
            let target = if name == "select-pane" {
                p
            } else if name == "select-tab" {
                t
            } else {
                ws
            };
            let mut v = vec![name];
            match r.below(6) {
                0 | 1 => v.push(pick),
                2 => v.extend([s("-t"), target]),
                3 => v.push(dir),
                4 => v.extend([s("-t"), any]),
                _ => {}
            }
            v
        }
        16 => vec![s("split"), one(r, &["-h", "-v"]), s("-t"), p],
        17 => vec![s("new-tab")],
        18 => vec![s("new-workspace")],
        19 => vec![s("kill-pane"), s("-t"), p],
        20 => vec![s("kill-tab"), s("-t"), t],
        21 => vec![s("kill-workspace"), s("-t"), ws],
        22 => {
            let long: [String; 5] = [
                std::iter::repeat_n('a', 100).collect(),
                std::iter::repeat_n('界', 80).collect(),
                s("tab-with-a-long-name-xxxxxxxxxxxxxxxxxxxxxxxx"),
                std::iter::repeat_n("é\u{301}界x", 20).collect(),
                std::iter::repeat_n('x', 256).collect(),
            ];
            let name = if r.chance(50) {
                r.pick(&long).cloned().unwrap_or_default()
            } else {
                one(r, &["x", "main", "界界", "a b", ""])
            };
            vec![s("rename"), s("-t"), any, name]
        }
        23 => {
            let to = one(r, &["new-tab", "new-workspace", "@1", "+1", "main"]);
            vec![s("move-pane"), s("-t"), p, s("--to"), to]
        }
        24 => vec![s("swap-pane"), s("-t"), p, dir],
        25 => {
            let cells = r.below(30).saturating_add(1).to_string();
            vec![s("resize-pane"), s("-t"), p, dir, cells]
        }
        26 => vec![s("reorder"), kind, pick],
        27 => vec![one(
            r,
            &[
                "copy-mode",
                "command-column",
                "command-prompt",
                "zoom",
                "choose-tab",
                "choose-workspace",
                "choose-pane",
            ],
        )],
        28 => vec![one(r, &["menu", "confirm-close", "rename-prompt"]), kind],
        29 => vec![s("choose-tab"), s("-t"), p, s("--move")],
        30 => vec![s("paste-buffer")],
        31 => vec![s("send-keys"), s("-t"), p, s("hello"), s("Enter")],
        32 => vec![
            s("bind"),
            s("-r"),
            one(r, &["y", "t", "r", "u"]),
            one(r, &["h", "l", "q"]),
            one(
                r,
                &["select-tab", "select-pane", "resize-pane", "zoom", "split"],
            ),
            pick,
        ],
        33 => vec![s("unbind"), one(r, &["r", "t", "m", "w", "c"])],
        34 => vec![
            s("unbind"),
            one(r, &["r", "t", "m", "w"]),
            one(r, &["h", "l", "m"]),
        ],
        35 => vec![s("capture-client")],
        36 => vec![s("terminate"), s("-t"), p],
        37 => vec![s("show-buffer")],
        38 => vec![s("detach")],
        39 => {
            let mut v = match r.below(4) {
                0 => vec![s("new-tab")],
                1 => vec![s("new-workspace")],
                2 => vec![s("split"), s("-h")],
                _ => {
                    let shell = one(r, &["/usr/bin/fish", "/bin/sh", "fish -l", "zsh"]);
                    return vec![s("set"), s("shell"), shell];
                }
            };
            v.push(s("--"));
            for _ in 0..r.below(3).saturating_add(1) {
                v.push(one(
                    r,
                    &[
                        "vim",
                        "a b",
                        "it's",
                        "back\\slash",
                        "tab\there",
                        "x\u{7}y",
                        "",
                        "$HOME",
                        "\"q\"",
                        "界",
                        "ok_word-1.2",
                    ],
                ));
            }
            v
        }
        _ => {
            if r.chance(50) {
                vec![s("ls")]
            } else {
                vec![s("ls"), s("--json")]
            }
        }
    };
    let screen = words.first().is_some_and(|w| {
        w.starts_with("select") || w.starts_with('c') || w.starts_with("menu") || w == "zoom"
    });
    if screen && r.chance(33) {
        words.extend([s("-c"), format!("c{}", id(r, &w.clients, 44, 10))]);
    }
    words
}

pub fn event(r: &mut Rng, w: &World) -> Event {
    let client = id(r, &w.clients, 9, 3);
    let pane = id(r, &w.panes, 99, 3);
    match r.below(100) {
        0..=44 => {
            let mut bytes = Vec::new();
            for _ in 0..r.below(3).saturating_add(1) {
                bytes.extend_from_slice(r.pick(KEYS).copied().unwrap_or_default());
            }
            if r.chance(1) {
                // Past the queue's limit.
                bytes = (0..20).flat_map(|_| paste(b'y', 60_000)).collect();
            } else if r.chance(1) {
                // Past the paste limit.
                bytes = paste(b'z', 70_000);
            } else if r.chance(2) {
                bytes = paste(b'z', 60_000);
            }
            Event::Key(client, bytes)
        }
        45..=48 => Event::Escape(client),
        49..=66 => {
            let ctx_client = if r.chance(33) { None } else { Some(client) };
            let ctx_pane = if r.chance(50) { Some(pane) } else { None };
            Event::Run(line(r, w), ctx_client, ctx_pane)
        }
        67..=80 => {
            let mut bytes = Vec::new();
            for _ in 0..r.below(12).saturating_add(1) {
                bytes.extend_from_slice(r.pick(OUTPUT).copied().unwrap_or_default());
            }
            Event::Output(pane, bytes)
        }
        81..=83 => {
            let rows = r
                .pick(&[1u16, 2, 3, 4, 6, 10, 24, 40])
                .copied()
                .unwrap_or(24);
            let cols = r
                .pick(&[1u16, 2, 5, 10, 20, 40, 80, 200])
                .copied()
                .unwrap_or(80);
            Event::Resize(client, rows, cols)
        }
        84 => {
            let ws = r
                .chance(33)
                .then(|| one(r, &["main", "+1", "+2", "nope", "%1"]));
            let rows = r.pick(&[1u16, 2, 5, 24]).copied().unwrap_or(24);
            let cols = r.pick(&[1u16, 10, 80]).copied().unwrap_or(80);
            Event::Attach(rows, cols, ws)
        }
        85 if r.chance(25) => Event::Detach(client),
        86 if r.chance(33) => Event::Exited(pane, i32::try_from(r.below(3)).unwrap_or(0)),
        86 => Event::Drain(pane, r.below(100_000)),
        87..=92 | 85 => Event::Painted(client),
        93 if r.chance(12) => Event::Shutdown,
        93 => {
            let text = one(
                r,
                &[
                    "short note",
                    "a much longer notice that will not fit in a narrow bar at all 界界界",
                    "界",
                ],
            );
            Event::Notice(client, r.chance(50), text)
        }
        _ => {
            let keys = one(
                r,
                &[
                    "\x02c", "\x02r", "\x02m", "\x02t", "\x02tm", "\x02w", "\x02e", "\x02g",
                    "\x02a",
                ],
            );
            Event::Key(client, keys.into_bytes())
        }
    }
}

/// What led to a difference: the case and the events before it.
fn trail(case: usize, log: &[Event]) -> String {
    let start = log.len().saturating_sub(40);
    let events: Vec<String> = log
        .iter()
        .enumerate()
        .skip(start)
        .map(|(i, e)| format!("  {i}: {e:?}"))
        .collect();
    format!(
        "session {case}, after {} events (the last {} shown):\n{}",
        log.len(),
        log.len().saturating_sub(start),
        events.join("\n")
    )
}

/// Runs `cases` sessions of random events, comparing after every event;
/// with `paint`, the screens and paints too.
fn sessions(r: &mut Rng, cases: usize, paint: bool) -> Outcome {
    let (mut events, mut commands, mut screens, mut painted) = (0u64, 0u64, 0u64, 0u64);
    let (mut copying, mut listing, mut repeating) = (0u64, 0u64, 0u64);
    for case in 0..cases {
        let history = r.pick(&[0usize, 3, 10, 50, 1000]).copied().unwrap_or(10);
        let clipboard = r.chance(50);
        let buffers = r.pick(&[1usize, 3, 50]).copied().unwrap_or(3);
        let mut a = base::make(history, clipboard, buffers)?;
        let mut b = cur::make(history, clipboard, buffers)?;
        let (mut shown_a, mut shown_b) = (BTreeMap::new(), BTreeMap::new());
        let mut log = Vec::new();
        for _ in 0..r.below(3) {
            let rows = r.pick(&[2u16, 5, 24, 40]).copied().unwrap_or(24);
            let cols = r.pick(&[10u16, 40, 80, 200]).copied().unwrap_or(80);
            log.push(Event::Attach(rows, cols, None));
        }
        let steps = r.below(200).saturating_add(20);
        for step in 0..steps.saturating_add(log.len()) {
            let e = match log.get(step) {
                Some(e) => e.clone(),
                None => {
                    let e = event(r, &world(&a));
                    log.push(e.clone());
                    e
                }
            };
            let (ra, rb) = (base::apply(&mut a, &e), cur::apply(&mut b, &e));
            let (sa, sb) = (base::state(&mut a), cur::state(&mut b));
            let context = || trail(case, &log);
            same(
                &format!("what the last event returned\n{}", context()),
                ra,
                rb,
            )?;
            same_lines(&format!("the state\n{}", context()), &sa, &sb)?;
            if paint {
                let (pa, pb) = (
                    base::paints(&a, &mut shown_a),
                    cur::paints(&b, &mut shown_b),
                );
                crate::same_bytes(&format!("the paints\n{}", context()), &pa, &pb)?;
                screens = screens.saturating_add(u64::try_from(a.views.len()).unwrap_or(0));
                painted = painted.saturating_add(u64::try_from(pa.len()).unwrap_or(0));
            }
            bump(&mut events);
            if matches!(e, Event::Run(..)) {
                bump(&mut commands);
            }
            let [copy, list, repeat] = modes(&a);
            for (on, n) in [
                (copy, &mut copying),
                (list, &mut listing),
                (repeat, &mut repeating),
            ] {
                if on {
                    bump(n);
                }
            }
        }
    }
    let mut summary = format!(
        "{cases} sessions, {events} events ({commands} commands; with a client in copy mode {copying}, in a list {listing}, in a repeat mode {repeating})"
    );
    if paint {
        summary.push_str(&format!(", {screens} screens, {painted} bytes of paint"));
    }
    Ok(summary)
}

pub fn states(r: &mut Rng, scale: usize) -> Outcome {
    sessions(r, times(300, scale), false)
}

pub fn screens(r: &mut Rng, scale: usize) -> Outcome {
    sessions(r, times(100, scale), true)
}

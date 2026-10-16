//! The oracle: fux's answers checked against what the program itself says
//! it did. A fixture program (`tests/oracle/fixture.py`) writes a side log
//! of every byte it writes, whether that byte is the echo of what was typed
//! or its own output, what it read and when it blocked
//! (`tests/oracle/README.md`; the format, frozen, in
//! `tests/oracle/side-log-format.md`). The checker drives it through fux
//! as an agent does (`run`, `send-keys` with a `wait-text`, `wait`, `resize`),
//! then decides from the side log what each answer should have been, and
//! names every disagreement by its class.
//!
//! The deterministic part runs a fixed tour of every behaviour in every
//! input mode, the cases in `tests/oracle/corpus/`, and two metamorphic
//! checks (another terminal size, output written in small chunks). The
//! fuzzing part is ignored by default:
//!
//! ```sh
//! ORACLE_SEED=7 ORACLE_RUNS=40 ORACLE_JOBS=4 cargo test --test oracle -- --ignored
//! ```
//!
//! It turns on a random subset of behaviours per run (swarm testing),
//! generates a sequence of actions, shrinks any failing case and saves it
//! to the corpus as `<class>-<n>.json`.
#[path = "oracle/json.rs"]
mod json;
mod support;

use json::{Value, object};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use support::{Outcome, PROBED, Server, e};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/oracle/fixture.py");
const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/oracle/corpus");
/// History short enough that `scroll` lets the reply go past it: about 100
/// rows of its lines.
const CONFIG: &str = "str history-bytes 12000 set";

/// Disagreements known to be fux's, each a defect in src/ to fix, listed
/// here so the deterministic part reports them without failing: its
/// class, the classify source, the fixture's mode, and why. Remove an
/// entry once fixed; the tests then hold it.
const KNOWN: &[Known] = &[Known {
    class: "echo-taken-as-output",
    sources: &["screen"],
    modes: &["full"],
    chunked: false,
    why: "a kept rule (docs/agents.md, notes kept-rules.md): a full-screen program's \
          screen is all output, so a form's field showing the text typed meets a wait \
          for it, which agents use to know the keys went in (agentbench's form tasks)",
}];

struct Known {
    class: &'static str,
    /// The classify sources and fixture modes it is seen with (none
    /// listed: any).
    sources: &'static [&'static str],
    modes: &'static [&'static str],
    /// Seen only with the output written a few bytes at a time.
    chunked: bool,
    why: &'static str,
}

// ---------------------------------------------------------------------
// Cases

/// How the fixture takes a line (its `--mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Line,
    Password,
    Readline,
    Ptk,
    Full,
}

const MODES: [Mode; 5] = [
    Mode::Line,
    Mode::Password,
    Mode::Readline,
    Mode::Ptk,
    Mode::Full,
];

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Line => "line",
            Mode::Password => "password",
            Mode::Readline => "readline",
            Mode::Ptk => "ptk",
            Mode::Full => "full",
        }
    }

    fn named(name: &str) -> Option<Mode> {
        MODES.into_iter().find(|m| m.name() == name)
    }

    /// The fixture edits the line itself: edit keys are its to take.
    fn raw(self) -> bool {
        matches!(self, Mode::Readline | Mode::Ptk | Mode::Full)
    }
}

/// A key as `send-keys` takes it.
#[derive(Clone, Debug, PartialEq)]
enum Key {
    Text(String),
    Name(String),
}

impl Key {
    fn name(name: &str) -> Key {
        Key::Name(name.to_owned())
    }

    /// The bytes fux writes for it (normal cursor keys).
    fn len(&self) -> usize {
        match self {
            Key::Text(text) => text.len(),
            Key::Name(name) => match name.as_str() {
                "Left" | "Right" | "Home" | "End" => 3,
                "Delete" => 4,
                _ => 1,
            },
        }
    }
}

/// What an action waits for: a text, a bell, both (whichever comes
/// first), or nothing (it settles); within `secs` (0: answers at once).
#[derive(Clone, Debug, PartialEq)]
struct Probe {
    text: Option<String>,
    bell: bool,
    secs: u32,
}

#[derive(Clone, Debug, PartialEq)]
enum Step {
    /// `send-keys` with the probe's waits.
    Keys { keys: Vec<Key>, probe: Probe },
    /// `wait`: what came since the last input.
    Wait { probe: Probe },
    /// The pane's tab takes a new size.
    Resize { cols: u16, rows: u16 },
}

impl Step {
    fn probe(&self) -> Option<&Probe> {
        match self {
            Step::Keys { probe, .. } | Step::Wait { probe } => Some(probe),
            Step::Resize { .. } => None,
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Step::Keys { keys, .. } => keys.iter().map(Key::len).sum(),
            Step::Wait { .. } | Step::Resize { .. } => 0,
        }
    }

    /// The behaviour it exercises, as `KNOWN` names it.
    fn behaviour(&self) -> String {
        match self {
            Step::Wait { .. } => "wait".into(),
            Step::Resize { .. } => "resize".into(),
            Step::Keys { keys, .. } => {
                let text: Vec<&str> = (keys.iter())
                    .filter_map(|k| match k {
                        Key::Text(t) => Some(t.as_str()),
                        Key::Name(_) => None,
                    })
                    .collect();
                let first = text.first().and_then(|t| t.split_whitespace().next());
                let entered = keys.iter().any(|k| *k == Key::name("Enter"));
                let edited = keys
                    .iter()
                    .any(|k| matches!(k, Key::Name(n) if n != "Enter"));
                match first {
                    Some(word) if !word.starts_with("IN") && entered => word.to_owned(),
                    _ if edited => "edit".into(),
                    _ if !entered => "typed".into(),
                    _ if text.is_empty() => "enter".into(),
                    _ => "reply".into(),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Case {
    mode: Mode,
    seed: u64,
    cols: u16,
    rows: u16,
    /// The fixture writes this many bytes at a time (0: each write whole).
    chunk: u32,
    steps: Vec<Step>,
}

fn probe_json(probe: &Probe) -> Value {
    object([
        ("text", probe.text.clone().map_or(Value::Null, Value::Str)),
        ("bell", Value::Bool(probe.bell)),
        ("secs", number(probe.secs)),
    ])
}

fn probe_of(value: &Value) -> Probe {
    Probe {
        text: value.str("text").map(str::to_owned),
        bell: value.bool("bell").unwrap_or(false),
        secs: (value.int("secs"))
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(3),
    }
}

fn number(n: impl TryInto<i64>) -> Value {
    n.try_into().map_or(Value::Null, Value::Number)
}

impl Case {
    fn to_json(&self, class: &str) -> Value {
        let steps = self.steps.iter().map(|step| match step {
            Step::Keys { keys, probe } => {
                let keys = keys.iter().map(|k| match k {
                    Key::Text(t) => {
                        Value::List(vec![Value::Str("text".into()), Value::Str(t.clone())])
                    }
                    Key::Name(n) => {
                        Value::List(vec![Value::Str("key".into()), Value::Str(n.clone())])
                    }
                });
                object([
                    ("keys", Value::List(keys.collect())),
                    ("probe", probe_json(probe)),
                ])
            }
            Step::Wait { probe } => object([("wait", probe_json(probe))]),
            Step::Resize { cols, rows } => {
                object([("resize", Value::List(vec![number(*cols), number(*rows)]))])
            }
        });
        object([
            ("class", Value::Str(class.to_owned())),
            ("mode", Value::Str(self.mode.name().into())),
            ("seed", number(self.seed)),
            ("cols", number(self.cols)),
            ("rows", number(self.rows)),
            ("chunk", number(self.chunk)),
            ("steps", Value::List(steps.collect())),
        ])
    }

    fn of_json(value: &Value) -> Result<Case, String> {
        let size = |key: &str| {
            (value.int(key))
                .and_then(|n| u16::try_from(n).ok())
                .ok_or_else(|| format!("no {key}"))
        };
        let mut steps = Vec::new();
        for step in value.list("steps") {
            if let Some(probe) = step.get("wait") {
                steps.push(Step::Wait {
                    probe: probe_of(probe),
                });
            } else if let Some(Value::List(size)) = step.get("resize") {
                let dim = |n: usize| match size.get(n) {
                    Some(Value::Number(v)) => u16::try_from(*v).map_err(e),
                    _ => Err("a resize's size".to_owned()),
                };
                steps.push(Step::Resize {
                    cols: dim(0)?,
                    rows: dim(1)?,
                });
            } else {
                let mut keys = Vec::new();
                for key in step.list("keys") {
                    let Value::List(pair) = key else {
                        return Err("a key".into());
                    };
                    match (pair.first(), pair.get(1)) {
                        (Some(Value::Str(kind)), Some(Value::Str(k))) if kind == "text" => {
                            keys.push(Key::Text(k.clone()));
                        }
                        (Some(Value::Str(_)), Some(Value::Str(k))) => {
                            keys.push(Key::Name(k.clone()))
                        }
                        _ => return Err("a key".into()),
                    }
                }
                let probe = step.get("probe").map(probe_of).ok_or("a step's probe")?;
                steps.push(Step::Keys { keys, probe });
            }
        }
        Ok(Case {
            mode: (value.str("mode").and_then(Mode::named)).ok_or("a mode")?,
            seed: (value.int("seed").and_then(|n| u64::try_from(n).ok())).unwrap_or(0),
            cols: size("cols")?,
            rows: size("rows")?,
            chunk: (value.int("chunk").and_then(|n| u32::try_from(n).ok())).unwrap_or(0),
            steps,
        })
    }
}

/// splitmix64: cases from a seed, the same on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        let n64 = u64::try_from(n).unwrap_or(u64::MAX).max(1);
        usize::try_from(self.next().checked_rem(n64).unwrap_or(0)).unwrap_or(0)
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    /// A seed for a fixture: below 2^53, so JSON keeps it whole.
    fn seed(&mut self) -> u64 {
        self.next() >> 11
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.below(items.len()))
    }
}

/// The behaviours a case can have; a fuzzing run turns on some of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behaviour {
    Reply,
    Typed,
    Edits,
    Repeat,
    Bell,
    Over,
    Wrap,
    Scroll,
    Sleep,
    TypeAhead,
    Timed,
    Poll,
    Ticker,
    ResizeBurst,
    Alt,
    Never,
}

const BEHAVIOURS: [Behaviour; 16] = [
    Behaviour::Reply,
    Behaviour::Typed,
    Behaviour::Edits,
    Behaviour::Repeat,
    Behaviour::Bell,
    Behaviour::Over,
    Behaviour::Wrap,
    Behaviour::Scroll,
    Behaviour::Sleep,
    Behaviour::TypeAhead,
    Behaviour::Timed,
    Behaviour::Poll,
    Behaviour::Ticker,
    Behaviour::ResizeBurst,
    Behaviour::Alt,
    Behaviour::Never,
];

/// Builds a case's steps, each line typed naming a fresh number.
struct Builder {
    rng: Rng,
    mode: Mode,
    id: u32,
    steps: Vec<Step>,
}

fn token(prefix: &str, id: u32) -> String {
    format!("{prefix}{id:04}")
}

fn text(t: &str) -> Key {
    Key::Text(t.to_owned())
}

fn waits(text: Option<String>, secs: u32) -> Probe {
    Probe {
        text,
        bell: false,
        secs,
    }
}

impl Builder {
    fn new(mode: Mode, seed: u64) -> Builder {
        Builder {
            rng: Rng(seed),
            mode,
            id: 0,
            steps: Vec::new(),
        }
    }

    fn fresh(&mut self) -> u32 {
        self.id = self.id.saturating_add(1);
        self.id
    }

    fn keys(&mut self, keys: Vec<Key>, probe: Probe) {
        self.steps.push(Step::Keys { keys, probe });
    }

    /// Keys that leave `IN<id>` on the line: typed straight, or edited
    /// into it where the fixture edits the line itself.
    fn typing(&mut self, id: u32, edits: bool) -> Vec<Key> {
        let word = token("IN", id);
        if !(edits && self.mode.raw()) {
            return vec![Key::Text(word)];
        }
        let tail = word.get(1..).unwrap_or_default().to_owned();
        match self.rng.below(6) {
            0 => vec![
                Key::Text(format!("{word}zz")),
                Key::name("BSpace"),
                Key::name("BSpace"),
            ],
            1 => vec![text("junk"), Key::name("C-u"), Key::Text(word)],
            2 => vec![Key::Text(format!("{word} junk")), Key::name("C-w")],
            3 => vec![
                Key::Text(tail),
                Key::name("Home"),
                text("I"),
                Key::name("End"),
            ],
            4 => vec![
                Key::Text(format!("{word}x")),
                Key::name("Left"),
                Key::name("Delete"),
            ],
            _ => vec![
                Key::Text(word),
                Key::name("Left"),
                Key::name("Left"),
                Key::name("Right"),
                Key::name("Right"),
            ],
        }
    }

    /// A line typed and entered; the wait is for its reply, or for the
    /// line itself (`repeat`: met only if the reply repeats it).
    fn reply(&mut self, edits: bool, repeat: bool) {
        let id = self.fresh();
        let mut keys = self.typing(id, edits);
        keys.push(Key::name("Enter"));
        let wanted = if repeat { "IN" } else { "OUT" };
        self.keys(keys, waits(Some(token(wanted, id)), 3));
    }

    /// The line typed with no Enter: a wait for it is never met by its
    /// echo; then Enter, and its reply.
    fn typed(&mut self, edits: bool) {
        let id = self.fresh();
        let keys = self.typing(id, edits);
        self.keys(keys, waits(Some(token("IN", id)), 2));
        let wanted = if self.rng.chance(30) { "IN" } else { "OUT" };
        self.keys(vec![Key::name("Enter")], waits(Some(token(wanted, id)), 3));
    }

    /// A command, and a wait for its reply.
    fn command(&mut self, command: &str, bell: bool) {
        let id = self.fresh();
        let line = format!("{command} {}", token("IN", id));
        let probe = Probe {
            text: Some(token("OUT", id)),
            bell,
            secs: 3,
        };
        self.keys(vec![Key::Text(line), Key::name("Enter")], probe);
    }

    /// Keys typed while the program is busy: they wait for its read.
    fn type_ahead(&mut self) {
        let id = self.fresh();
        let line = format!("work {}", token("IN", id));
        self.keys(vec![Key::Text(line), Key::name("Enter")], waits(None, 0));
        self.reply(false, false);
        if let Some(Step::Keys { probe, .. }) = self.steps.last_mut() {
            probe.secs = 4;
        }
    }

    /// Another thread prints on a timer: a wait for one of its lines,
    /// then a line typed meanwhile.
    fn ticker(&mut self) {
        self.command("ticker", false);
        let id = self.id;
        self.steps.push(Step::Wait {
            probe: waits(Some(format!("TK{id:04}C")), 3),
        });
        let edits = self.rng.chance(50);
        self.typed(edits);
    }

    fn size(&mut self) -> (u16, u16) {
        let cols = 40usize.saturating_add(self.rng.below(80));
        let rows = 10usize.saturating_add(self.rng.below(30));
        (
            u16::try_from(cols).unwrap_or(80),
            u16::try_from(rows).unwrap_or(24),
        )
    }

    /// A burst of output, the terminal resized in the middle of it, then
    /// a wait for the reply in it.
    fn resize_burst(&mut self) {
        let id = self.fresh();
        let line = format!("scroll {}", token("IN", id));
        self.keys(vec![Key::Text(line), Key::name("Enter")], waits(None, 0));
        let (cols, rows) = self.size();
        self.steps.push(Step::Resize { cols, rows });
        self.steps.push(Step::Wait {
            probe: waits(Some(token("OUT", id)), 3),
        });
    }

    /// A wait for a text nothing prints.
    fn never(&mut self) {
        let id = self.fresh();
        self.steps.push(Step::Wait {
            probe: waits(Some(token("NEVER", id)), 2),
        });
    }

    fn add(&mut self, behaviour: Behaviour) {
        match behaviour {
            Behaviour::Reply => self.reply(false, false),
            Behaviour::Typed => {
                let edits = self.rng.chance(50);
                self.typed(edits);
            }
            Behaviour::Edits => self.reply(true, false),
            Behaviour::Repeat => self.reply(false, true),
            Behaviour::Bell => self.command("bell", true),
            Behaviour::Over => self.command("over", false),
            Behaviour::Wrap => self.command("wrap", false),
            Behaviour::Scroll => self.command("scroll", false),
            Behaviour::Sleep => {
                let command = if self.rng.chance(50) { "sleep" } else { "work" };
                self.command(command, false);
            }
            Behaviour::TypeAhead => self.type_ahead(),
            Behaviour::Timed => self.command("timed", false),
            Behaviour::Poll => self.command("poll", false),
            Behaviour::Ticker => self.ticker(),
            Behaviour::ResizeBurst => self.resize_burst(),
            Behaviour::Alt => self.command("alt", false),
            Behaviour::Never => self.never(),
        }
    }

    fn case(self, cols: u16, rows: u16, chunk: u32, seed: u64) -> Case {
        Case {
            mode: self.mode,
            seed,
            cols,
            rows,
            chunk,
            steps: self.steps,
        }
    }
}

/// Every behaviour once, in `mode`: the deterministic part's case.
fn tour(mode: Mode, seed: u64) -> Case {
    let mut b = Builder::new(mode, seed);
    for behaviour in BEHAVIOURS {
        b.add(behaviour);
    }
    // Back from the alternate screen (or onto it, for `full`), a reply
    // there, and the program reading with nothing to come.
    b.add(Behaviour::Reply);
    b.add(Behaviour::Alt);
    b.add(Behaviour::Typed);
    b.add(Behaviour::Never);
    b.case(80, 24, 0, seed)
}

/// A random case: a random subset of behaviours (swarm testing), a
/// random sequence of them, a random size and chunking.
fn random_case(seed: u64) -> Case {
    let mut rng = Rng(seed);
    let mode = rng.pick(&MODES).copied().unwrap_or(Mode::Line);
    let mut on: Vec<Behaviour> = BEHAVIOURS.into_iter().filter(|_| rng.chance(50)).collect();
    if on.is_empty() {
        on.push(Behaviour::Reply);
    }
    let fixture_seed = rng.seed();
    let mut b = Builder::new(mode, rng.next());
    let count = 4usize.saturating_add(rng.below(10));
    for _ in 0..count {
        if let Some(behaviour) = rng.pick(&on).copied() {
            b.add(behaviour);
        }
        if rng.chance(8) {
            let (cols, rows) = b.size();
            b.steps.push(Step::Resize { cols, rows });
        }
    }
    b.add(Behaviour::Never);
    let (cols, rows) = b.size();
    let chunk = if rng.chance(25) {
        u32::try_from(rng.below(7).saturating_add(1)).unwrap_or(1)
    } else {
        0
    };
    b.case(cols, rows, chunk, fixture_seed)
}

// ---------------------------------------------------------------------
// Running a case through fux

/// fux's answer to a step.
#[derive(Clone, Debug, Default)]
struct Answer {
    status: i32,
    /// The state (`reading`, `waiting`, `busy`, `exited`…), when said.
    state: String,
    /// What a wait met: `text`, `bell`, or nothing.
    met: String,
    /// The text met.
    text: String,
    raw: String,
}

#[derive(Clone, Debug)]
struct Done {
    /// Side-log records before the step was sent, and once answered.
    before: usize,
    after: usize,
    answer: Option<Answer>,
}

/// A case run: each step's answer, and the side log.
struct Run {
    done: Vec<Done>,
    log: Vec<Rec>,
}

fn records(path: &Path) -> usize {
    std::fs::read(path)
        .map(|b| b.iter().filter(|c| **c == b'\n').count())
        .unwrap_or(0)
}

fn answer_of(out: &support::Output) -> Result<Answer, String> {
    // At its time limit (124) a wait says so on stderr alone:
    // `fux: timeout: …; %4 busy, the screen unchanged …`.
    if out.stdout.trim().is_empty() && out.status == 124 {
        let state = (out.stderr.split("; %").nth(1))
            .and_then(|rest| rest.split([' ', ',']).nth(1))
            .unwrap_or_default();
        return Ok(Answer {
            status: out.status,
            state: state.to_owned(),
            raw: out.stderr.trim().to_owned(),
            ..Answer::default()
        });
    }
    let line = out.stdout.lines().next().unwrap_or_default();
    let value = json::parse(line)
        .map_err(|err| format!("fux answered {:?} {:?}: {err}", out.stdout, out.stderr))?;
    Ok(Answer {
        status: (value.int("status"))
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(out.status),
        state: value.str("state").unwrap_or_default().to_owned(),
        met: value.str("met").unwrap_or_default().to_owned(),
        text: value.str("text").unwrap_or_default().to_owned(),
        raw: line.to_owned(),
    })
}

fn probe_words(probe: &Probe, words: &mut Vec<String>) {
    if let Some(text) = &probe.text {
        words.extend(["str".into(), text.clone(), "wait-text".into()]);
    }
    if probe.bell {
        words.push("wait-bell".into());
    }
    words.extend([probe.secs.to_string(), "timeout".into()]);
}

static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn run_case(server: &Server, case: &Case) -> Result<Run, String> {
    let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let log = server.dir.join(format!("oracle-{n}.jsonl"));
    let mut start: Vec<String> = vec![
        "opts".into(),
        case.cols.to_string(),
        case.rows.to_string(),
        "size".into(),
        "str".into(),
        "FUX_ORACLE_LOG".into(),
        "str".into(),
        log.to_string_lossy().into_owned(),
        "env".into(),
        "str".into(),
        "python3".into(),
        "program".into(),
    ];
    let mut argv = vec![
        "-I".to_owned(),
        FIXTURE.to_owned(),
        "--mode".into(),
        case.mode.name().into(),
        "--seed".into(),
        case.seed.to_string(),
    ];
    if case.chunk > 0 {
        argv.extend(["--chunk".into(), case.chunk.to_string()]);
    }
    for arg in argv {
        start.extend(["str".into(), arg, "arg".into()]);
    }
    start.push("run".into());
    let started = server.ok(&start.iter().map(String::as_str).collect::<Vec<_>>())?;
    let pane = started
        .split_whitespace()
        .next()
        .ok_or("run answered nothing")?
        .to_owned();
    let mut done = Vec::new();
    for step in &case.steps {
        let mut words: Vec<String> = vec!["str".into(), pane.clone(), "pane".into()];
        match step {
            Step::Keys { keys, probe } => {
                words.push("keys".into());
                for key in keys {
                    match key {
                        Key::Text(t) => words.extend(["str".into(), t.clone(), "text".into()]),
                        Key::Name(n) => words.extend(["str".into(), n.clone(), "key".into()]),
                    }
                }
                probe_words(probe, &mut words);
                words.extend(["send-keys".into(), "json".into()]);
            }
            Step::Wait { probe } => {
                words.push("opts".into());
                probe_words(probe, &mut words);
                words.extend(["wait".into(), "json".into()]);
            }
            Step::Resize { cols, rows } => {
                words.extend([
                    "tab".into(),
                    cols.to_string(),
                    rows.to_string(),
                    "resize".into(),
                ]);
            }
        }
        let before = records(&log);
        let out = server.fux(&words.iter().map(String::as_str).collect::<Vec<_>>())?;
        let after = records(&log);
        let answer = match step {
            Step::Resize { .. } if out.status == 0 => None,
            Step::Resize { .. } => return Err(format!("resize failed: {}", out.stderr)),
            Step::Keys { .. } | Step::Wait { .. } => Some(answer_of(&out)?),
        };
        done.push(Done {
            before,
            after,
            answer,
        });
    }
    let _ = server.fux(&["str", &pane, "pane", "kill"]);
    // ORACLE_KEEP=DIR keeps each side log, with the steps' answers.
    if let Some(keep) = std::env::var_os("ORACLE_KEEP") {
        let keep = Path::new(&keep);
        std::fs::create_dir_all(keep).map_err(e)?;
        let name = format!("{}-{}-{n}", case.mode.name(), case.seed);
        std::fs::copy(&log, keep.join(format!("{name}.jsonl"))).map_err(e)?;
        let answers: Vec<String> = (case.steps.iter().zip(&done))
            .map(|(step, d)| format!("{step:?}\n  {} {} {:?}\n", d.before, d.after, d.answer))
            .collect();
        std::fs::write(keep.join(format!("{name}.steps")), answers.concat()).map_err(e)?;
    }
    Ok(Run {
        done,
        log: side_log(&log)?,
    })
}

// ---------------------------------------------------------------------
// The side log, and what it says

#[derive(Clone, Debug)]
enum Ev {
    Mode { raw: bool, echo: bool, alt: bool },
    Block(String),
    Read { consumed: usize },
    Write { bytes: Vec<u8>, output: bool },
    Other,
}

#[derive(Clone, Debug)]
struct Rec {
    seq: usize,
    ev: Ev,
}

fn side_log(path: &Path) -> Result<Vec<Rec>, String> {
    let text = std::fs::read_to_string(path).map_err(e)?;
    let mut recs = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let value = json::parse(line)?;
        let seq = (value.int("seq").and_then(|n| usize::try_from(n).ok())).ok_or("a seq")?;
        if seq != recs.len().saturating_add(1) {
            return Err(format!("side log out of order at {seq}"));
        }
        let bytes = || fux::json::unbase64(value.str("b64").unwrap_or_default().as_bytes());
        let ev = match value.str("ev").unwrap_or_default() {
            "mode" => Ev::Mode {
                raw: value.bool("raw").unwrap_or(false),
                echo: value.bool("echo").unwrap_or(false),
                alt: value.bool("alt").unwrap_or(false),
            },
            "block" => Ev::Block(value.str("on").unwrap_or_default().to_owned()),
            "read" => Ev::Read {
                consumed: (value.int("consumed").and_then(|n| usize::try_from(n).ok()))
                    .ok_or("consumed")?,
            },
            "write" => Ev::Write {
                bytes: bytes().ok_or("a write's bytes")?,
                output: value.str("role") == Some("output"),
            },
            _ => Ev::Other,
        };
        recs.push(Rec { seq, ev });
    }
    Ok(recs)
}

/// Text as a reader sees it: escape sequences (which move the cursor
/// or erase) and control characters as line ends, which no token spans.
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_control() {
            out.push('\n');
        }
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => while chars.next().is_some_and(|c| !('@'..='~').contains(&c)) {},
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Whether `text` holds `token` as fux finds a text: case ignored, from
/// a word's start.
fn holds(text: &str, token: &str) -> bool {
    let text = text.to_uppercase();
    let token = token.to_uppercase();
    text.match_indices(&token).any(|(at, _)| {
        text.get(..at)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|c| !c.is_alphanumeric())
    })
}

/// The writes with `lo < seq <= hi`: their bytes, and whether output.
fn writes(log: &[Rec], lo: usize, hi: usize) -> impl Iterator<Item = (&[u8], bool)> {
    log.iter()
        .filter(move |r| r.seq > lo && r.seq <= hi)
        .filter_map(|r| match &r.ev {
            Ev::Write { bytes, output } => Some((bytes.as_slice(), *output)),
            Ev::Mode { .. } | Ev::Block(_) | Ev::Read { .. } | Ev::Other => None,
        })
}

/// The bytes written with `lo < seq <= hi` in one role (output or echo),
/// in the order written, every thread's: a token split by another
/// thread's write is no token on the screen either.
fn stream(log: &[Rec], lo: usize, hi: usize, output: bool) -> Vec<u8> {
    let mut all = Vec::new();
    for (bytes, out) in writes(log, lo, hi) {
        if out == output {
            all.extend_from_slice(bytes);
        }
    }
    all
}

fn printed(log: &[Rec], lo: usize, hi: usize, token: &str) -> bool {
    holds(&plain(&stream(log, lo, hi, true)), token)
}

fn echoed(log: &[Rec], lo: usize, hi: usize, token: &str) -> bool {
    holds(&plain(&stream(log, lo, hi, false)), token)
}

/// Whether a line of output that holds the token also holds a line that
/// was typed, `INnnnn` (a reply that repeats the input: the fixture
/// prints such a word in no other way).
fn repeats_typed(log: &[Rec], (lo, hi): (usize, usize), token: &str) -> bool {
    let typed = |word: &str| {
        word.strip_prefix("IN")
            .is_some_and(|n| n.len() == 4 && n.chars().all(|c| c.is_ascii_digit()))
    };
    plain(&stream(log, lo, hi, true))
        .split('\n')
        .any(|line| holds(line, token) && line.split_whitespace().any(typed))
}

/// Whether the token was printed and then drawn over (`READY\r_____`, a
/// line or the screen erased), in one write or several.
fn overwritten(log: &[Rec], lo: usize, hi: usize, token: &str) -> bool {
    let text = String::from_utf8_lossy(&stream(log, lo, hi, true)).to_uppercase();
    text.find(&token.to_uppercase()).is_some_and(|at| {
        let rest = text.get(at..).unwrap_or_default();
        let row = rest.split('\n').next().unwrap_or_default();
        row.contains("\r_") || row.contains("\r\u{1b}[2K") || rest.contains("\u{1b}[2J")
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum First {
    Text,
    Bell,
}

/// Which came first after `lo`, up to `hi`: the token printed, or a bell.
fn first_of(log: &[Rec], lo: usize, hi: usize, token: &str) -> Option<First> {
    let mut text = Vec::new();
    for (bytes, output) in writes(log, lo, hi) {
        if !output {
            continue;
        }
        for (n, segment) in bytes.split(|b| *b == 7).enumerate() {
            if n > 0 {
                return Some(First::Bell);
            }
            text.extend_from_slice(segment);
            if holds(&plain(&text), token) {
                return Some(First::Text);
            }
        }
    }
    None
}

/// The terminal's modes as of `seq`.
fn modes_at(log: &[Rec], seq: usize) -> (bool, bool, bool) {
    let mut modes = (false, true, false);
    for rec in log.iter().take_while(|r| r.seq <= seq) {
        if let Ev::Mode { raw, echo, alt } = rec.ev {
            modes = (raw, echo, alt);
        }
    }
    modes
}

/// Which of classify's sources tells the output after `seq` from echo:
/// the full screen's, the kernel's echo (line mode), or order (raw mode,
/// the program echoing for itself). No fixture is a shell: never marks.
fn source_at(log: &[Rec], seq: usize) -> &'static str {
    match modes_at(log, seq) {
        (_, _, true) => "screen",
        (false, _, _) => "kernel",
        (true, _, _) => "order",
    }
}

/// Whether the program is blocked reading at `seq`, every byte it can
/// read read: of `sent`, or in line mode of `lines`, the bytes up to the
/// last Enter (the line after it is the terminal's until Enter).
fn reading_at(log: &[Rec], seq: usize, (sent, lines): (usize, usize)) -> bool {
    let sent = if modes_at(log, seq).0 { sent } else { lines };
    let mut last = None;
    let mut consumed = 0;
    for rec in log.iter().take_while(|r| r.seq <= seq) {
        match &rec.ev {
            Ev::Block(on) => last = Some(on.as_str()),
            Ev::Read { consumed: c } => {
                last = None;
                consumed = *c;
            }
            Ev::Mode { .. } | Ev::Write { .. } | Ev::Other => {}
        }
    }
    last == Some("read") && consumed == sent
}

// ---------------------------------------------------------------------
// Judging

#[derive(Clone, Debug)]
struct Disagreement {
    class: &'static str,
    source: &'static str,
    mode: &'static str,
    chunked: bool,
    step: usize,
    behaviour: String,
    detail: String,
}

/// Each step's verdict: whether fux met its text, and whether the side
/// log says it should have (None: it cannot tell).
#[derive(Clone, Debug, Default)]
struct Verdict {
    met: Option<bool>,
    truth: Option<bool>,
}

fn said_reading(answer: &Answer) -> bool {
    answer.status == 7 || answer.state.starts_with("reading")
}

fn judge(case: &Case, run: &Run) -> (Vec<Disagreement>, Vec<Verdict>) {
    let log = &run.log;
    let mut found = Vec::new();
    let mut verdicts = Vec::new();
    // The bytes sent before each step, and the last step that sent any.
    let mut sent = 0usize;
    let mut lines = 0usize;
    let mut since: Option<(usize, usize, usize)> = None; // (step, bytes before, records before)
    let mut typed: Vec<String> = Vec::new();
    let mut tickers: Vec<String> = Vec::new();
    for (n, (step, done)) in case.steps.iter().zip(&run.done).enumerate() {
        if step.bytes() > 0 {
            since = Some((n, sent, done.before));
        }
        if let Step::Keys { keys, .. } = step {
            for key in keys {
                sent = sent.saturating_add(key.len());
                if *key == Key::name("Enter") {
                    lines = sent;
                }
                if let Key::Text(t) = key {
                    typed.extend(t.split_whitespace().map(str::to_owned));
                    if let Some(id) = t.strip_prefix("ticker IN") {
                        tickers.push(format!("TK{id}H"));
                    }
                }
            }
        }
        let mut verdict = Verdict::default();
        let (Some(probe), Some(answer)) = (step.probe(), done.answer.as_ref()) else {
            verdicts.push(verdict);
            continue;
        };
        // What counts for this wait: what came after the first byte of
        // the last input was read (certain), or after the input was sent
        // and before the program read it (it may or may not count).
        let (first_read, issued) = match since {
            Some((_, lo, issued)) => {
                let first = log.iter().find_map(|r| match r.ev {
                    Ev::Read { consumed } => (consumed > lo).then_some(r.seq),
                    Ev::Mode { .. } | Ev::Block(_) | Ev::Write { .. } | Ev::Other => None,
                });
                (first.unwrap_or(done.after), issued)
            }
            None => (0, 0),
        };
        let hi = done.after;
        let source = source_at(log, first_read);
        let mut disagree = |class: &'static str, detail: String| {
            found.push(Disagreement {
                class,
                source,
                mode: case.mode.name(),
                chunked: case.chunk > 0,
                step: n,
                behaviour: step.behaviour(),
                detail,
            });
        };
        let ticking = tickers.iter().any(|t| !printed(log, 0, done.before, t));
        let exited = answer.status == 3 || answer.state.starts_with("exited");
        if exited {
            disagree("fixture-exited", answer.raw.clone());
        }
        if let Some(token) = probe.text.as_deref().filter(|_| probe.secs > 0 && !exited) {
            let met_text = answer.met == "text" && answer.text.eq_ignore_ascii_case(token);
            let truth = printed(log, first_read, hi, token);
            let maybe = !truth && printed(log, issued, first_read, token);
            verdict.met = Some(met_text);
            verdict.truth = (!maybe).then_some(truth);
            let kernel_echo = typed.iter().any(|t| t == token) && modes_at(log, hi).1;
            let echo = echoed(log, issued, hi, token) || kernel_echo;
            match (met_text, truth, maybe) {
                (true, false, false) => {
                    let class = if echo {
                        "echo-taken-as-output"
                    } else if printed(log, 0, issued, token) {
                        "stale-output"
                    } else {
                        "met-without-text"
                    };
                    disagree(class, format!("met {token:?}: {}", answer.raw));
                }
                (false, true, _) if answer.met != "bell" => {
                    let class = if overwritten(log, first_read, hi, token) {
                        "missed-overwritten"
                    } else if echo || repeats_typed(log, (first_read, hi), token) {
                        "output-taken-as-echo"
                    } else if answer.status == 7 {
                        "ended-early"
                    } else {
                        "missed-output"
                    };
                    disagree(class, format!("{token:?} was printed: {}", answer.raw));
                }
                _ => {}
            }
            if probe.bell && answer.status == 0 {
                let first = first_of(log, first_read, hi, token);
                let said = match answer.met.as_str() {
                    "bell" => Some(First::Bell),
                    "text" => Some(First::Text),
                    _ => None,
                };
                if let (Some(first), Some(said)) = (first, said)
                    && first != said
                    && first_of(log, issued, first_read, token).is_none()
                {
                    // The text, drawn over, missed: the bell met instead.
                    let class = if first == First::Text && overwritten(log, first_read, hi, token) {
                        "missed-overwritten"
                    } else {
                        "wrong-order"
                    };
                    disagree(
                        class,
                        format!("{first:?} came first, fux met {said:?}: {}", answer.raw),
                    );
                }
            }
        }
        // The state, where the side log is unambiguous (Linux alone tells
        // a program reading its terminal).
        if PROBED && !exited {
            if said_reading(answer) && !reading_at(log, hi, (sent, lines)) {
                disagree(
                    "wrong-state",
                    format!(
                        "said reading, but the program was not blocked reading every key: {}",
                        answer.raw
                    ),
                );
            }
            let still = done.before == done.after && reading_at(log, done.before, (sent, lines));
            let plain_wait = matches!(step, Step::Wait { .. }) && probe.secs > 0;
            if plain_wait && still && !ticking && !said_reading(answer) && answer.met.is_empty() {
                disagree(
                    "wrong-state",
                    format!(
                        "the program read every key and sat in its read throughout: {}",
                        answer.raw
                    ),
                );
            }
        }
        verdicts.push(verdict);
    }
    (found, verdicts)
}

/// The entry of `KNOWN` that lists a disagreement.
fn listed(d: &Disagreement) -> Option<&'static Known> {
    let any = |list: &[&str], item: &str| list.is_empty() || list.contains(&item);
    KNOWN.iter().find(|k| {
        k.class == d.class
            && any(k.sources, d.source)
            && any(k.modes, d.mode)
            && (!k.chunked || d.chunked)
    })
}

fn known(d: &Disagreement) -> bool {
    listed(d).is_some()
}

fn describe(case: &Case, found: &[Disagreement]) -> String {
    let mut by_class: BTreeMap<&str, Vec<&Disagreement>> = BTreeMap::new();
    for d in found {
        by_class.entry(d.class).or_default().push(d);
    }
    let mut out = format!(
        "mode {} seed {} size {}x{} chunk {}:\n",
        case.mode.name(),
        case.seed,
        case.cols,
        case.rows,
        case.chunk
    );
    for (class, list) in by_class {
        out.push_str(&format!("  {class}:\n"));
        for d in list {
            out.push_str(&format!(
                "    step {} ({}, source {}): {}\n",
                d.step, d.behaviour, d.source, d.detail
            ));
        }
    }
    out
}

/// Runs a case and judges it.
fn check(server: &Server, case: &Case) -> Result<(Vec<Disagreement>, Vec<Verdict>), String> {
    let run = run_case(server, case)?;
    Ok(judge(case, &run))
}

/// A case, its disagreements, and its steps' verdicts.
type Checked = (Case, Vec<Disagreement>, Vec<Verdict>);

/// Runs `cases` on a server per job, `jobs` at a time, none started after
/// `until`; what each case found, and the cases fux failed to answer.
fn run_all(cases: &[Case], jobs: usize, until: Option<Instant>) -> (Vec<Checked>, Vec<String>) {
    let queue = std::sync::Mutex::new(cases.to_vec());
    let results = std::sync::Mutex::new(Vec::new());
    let errors = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                let server = match Server::start(CONFIG) {
                    Ok(server) => server,
                    Err(err) => {
                        if let Ok(mut errors) = errors.lock() {
                            errors.push(err);
                        }
                        return;
                    }
                };
                while let Some(case) =
                    pop(&queue).filter(|_| until.is_none_or(|t| Instant::now() < t))
                {
                    match check(&server, &case) {
                        Ok((found, verdicts)) => {
                            if let Ok(mut results) = results.lock() {
                                results.push((case, found, verdicts));
                            }
                        }
                        Err(err) => {
                            if let Ok(mut errors) = errors.lock() {
                                errors.push(format!("{}: {err}", describe(&case, &[])));
                            }
                        }
                    }
                }
            });
        }
    });
    (
        results.into_inner().unwrap_or_default(),
        errors.into_inner().unwrap_or_default(),
    )
}

/// The same, every case answered.
fn check_all(cases: &[Case], jobs: usize) -> Result<Vec<Checked>, String> {
    let (results, errors) = run_all(cases, jobs, None);
    if errors.is_empty() {
        Ok(results)
    } else {
        Err(errors.join("\n"))
    }
}

fn pop(queue: &std::sync::Mutex<Vec<Case>>) -> Option<Case> {
    queue.lock().ok()?.pop()
}

/// Fails listing every disagreement not known, by class; known ones are
/// printed.
#[expect(
    clippy::print_stderr,
    reason = "the harness shows what a test prints when it fails, or with --nocapture"
)]
fn report(results: &[Checked]) -> Outcome {
    let mut failed = String::new();
    for (case, found, _) in results {
        let (old, new): (Vec<_>, Vec<_>) = found.iter().cloned().partition(known);
        if !old.is_empty() {
            let why: Vec<&str> = old.iter().filter_map(listed).map(|k| k.why).collect();
            eprintln!(
                "known disagreements (KNOWN):\n{}  why: {why:?}",
                describe(case, &old)
            );
        }
        if !new.is_empty() {
            failed.push_str(&describe(case, &new));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("fux disagrees with the side log:\n{failed}"))
    }
}

// ---------------------------------------------------------------------
// The deterministic part

/// Every behaviour, in every way of taking a line.
#[test]
fn every_behaviour_agrees_with_the_side_log() -> Outcome {
    let cases: Vec<Case> = (MODES.iter().zip(1u64..))
        .map(|(mode, seed)| tour(*mode, seed))
        .collect();
    let results = check_all(&cases, 3)?;
    // The oracle decided: in each case, texts it says came and texts it
    // says did not, most of the waits.
    for (case, _, verdicts) in &results {
        let decided = |truth: bool| {
            (verdicts.iter())
                .filter(|v| v.met.is_some() && v.truth == Some(truth))
                .count()
        };
        let waits = verdicts.iter().filter(|v| v.met.is_some()).count();
        let (came, not) = (decided(true), decided(false));
        if came < 5
            || not < 2
            || came.saturating_add(not).saturating_mul(4) < waits.saturating_mul(3)
        {
            return Err(format!(
                "{}: the side log decided too few waits: {came} came, {not} did not, of {waits}",
                case.mode.name()
            ));
        }
    }
    report(&results)
}

fn corpus() -> Result<Vec<(PathBuf, Case)>, String> {
    let mut cases = Vec::new();
    let Ok(dir) = std::fs::read_dir(CORPUS) else {
        return Ok(cases);
    };
    let mut paths: Vec<PathBuf> = dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(e)?;
        let value = json::parse(&text).map_err(|err| format!("{}: {err}", path.display()))?;
        cases.push((path, Case::of_json(&value)?));
    }
    Ok(cases)
}

/// Every case the fuzzer shrank and saved.
#[test]
fn the_corpus_agrees_with_the_side_log() -> Outcome {
    let cases: Vec<Case> = corpus()?.into_iter().map(|(_, case)| case).collect();
    report(&check_all(&cases, 2)?)
}

/// Where a step's text was met in one run and not in the other, though
/// the side log says the same of both (or cannot tell).
/// Steps whose waits were met in one run and not in the other, though the
/// side log says the same of both (or cannot tell); steps with a known
/// disagreement in either aside.
fn compare(class: &str, a: &Checked, b: &Checked) -> Vec<String> {
    let listed = |run: &Checked, n: usize| run.1.iter().any(|d| d.step == n && known(d));
    (a.2.iter().zip(&b.2).enumerate())
        .filter(|(n, (x, y))| {
            let same_truth = match (x.truth, y.truth) {
                (Some(p), Some(q)) => p == q,
                _ => true,
            };
            let both = x.met.is_some() && y.met.is_some();
            both && same_truth && x.met != y.met && !listed(a, *n) && !listed(b, *n)
        })
        .map(|(n, (x, y))| {
            format!(
                "{class}: mode {} step {n} met {:?} at {}x{} chunk {}, {:?} at {}x{} chunk {}",
                a.0.mode.name(),
                x.met,
                a.0.cols,
                a.0.rows,
                a.0.chunk,
                y.met,
                b.0.cols,
                b.0.rows,
                b.0.chunk
            )
        })
        .collect()
}

/// The same keys at another terminal size, and the same output written a
/// few bytes at a time, give the same answers.
#[test]
fn answers_hold_across_sizes_and_chunks() -> Outcome {
    let mut cases = Vec::new();
    for (mode, seed) in [(Mode::Readline, 11u64), (Mode::Line, 12)] {
        let mut b = Builder::new(mode, seed);
        for behaviour in [
            Behaviour::Reply,
            Behaviour::Typed,
            Behaviour::Edits,
            Behaviour::Repeat,
            Behaviour::Wrap,
            Behaviour::Over,
            Behaviour::Bell,
            Behaviour::Never,
        ] {
            b.add(behaviour);
        }
        let base = b.case(80, 24, 0, seed);
        let small = Case {
            cols: 43,
            rows: 11,
            ..base.clone()
        };
        let chunked = Case {
            chunk: 3,
            ..base.clone()
        };
        cases.push([base, small, chunked]);
    }
    let all: Vec<Case> = cases.iter().flatten().cloned().collect();
    let results = check_all(&all, 3)?;
    let of = |case: &Case| {
        (results.iter().find(|r| r.0 == *case)).ok_or_else(|| "a case's result".to_owned())
    };
    let mut failed = Vec::new();
    for [base, small, chunked] in &cases {
        failed.extend(compare("size-dependent", of(base)?, of(small)?));
        failed.extend(compare("chunk-dependent", of(base)?, of(chunked)?));
    }
    let reported = report(&results);
    if failed.is_empty() {
        reported
    } else {
        Err(format!(
            "{}\n{}",
            failed.join("\n"),
            reported.err().unwrap_or_default()
        ))
    }
}

// ---------------------------------------------------------------------
// Fuzzing

/// A disagreement's class, and the entry of `KNOWN` that lists it.
fn kind(d: &Disagreement) -> String {
    match KNOWN
        .iter()
        .position(|k| listed(d).is_some_and(|l| std::ptr::eq(k, l)))
    {
        Some(n) => format!("{} (KNOWN {n})", d.class),
        None => d.class.to_owned(),
    }
}

/// Whether `case` still shows a disagreement of this `kind`.
fn still_fails(server: &Server, case: &Case, kind_of: &str) -> bool {
    check(server, case).is_ok_and(|(found, _)| found.iter().any(|d| kind(d) == kind_of))
}

/// Delta debugging: drops chunks of steps, then single keys, while the
/// case still fails with `class`; at most `budget` runs.
fn shrink(server: &Server, case: &Case, class: &str, mut budget: usize, until: Instant) -> Case {
    let mut best = case.clone();
    let mut parts = 2usize;
    while best.steps.len() > 1 && budget > 0 && Instant::now() < until {
        let len = best.steps.len();
        let size = len.div_ceil(parts).max(1);
        let mut reduced = false;
        let mut start = 0usize;
        while start < len && budget > 0 {
            let end = start.saturating_add(size).min(len);
            let steps: Vec<Step> = (best.steps.iter().enumerate())
                .filter(|(i, _)| *i < start || *i >= end)
                .map(|(_, s)| s.clone())
                .collect();
            let candidate = Case {
                steps,
                ..best.clone()
            };
            budget = budget.saturating_sub(1);
            if !candidate.steps.is_empty() && still_fails(server, &candidate, class) {
                best = candidate;
                parts = parts.saturating_sub(1).max(2);
                reduced = true;
                break;
            }
            start = end;
        }
        if !reduced {
            if size <= 1 {
                break;
            }
            parts = parts.saturating_mul(2).min(best.steps.len());
        }
    }
    // Single keys within each step.
    let mut n = 0usize;
    while n < best.steps.len() && budget > 0 && Instant::now() < until {
        let mut k = 0usize;
        while let Some(Step::Keys { keys, probe }) = best.steps.get(n) {
            if k >= keys.len() || keys.len() < 2 || budget == 0 {
                break;
            }
            let fewer: Vec<Key> = (keys.iter().enumerate())
                .filter(|(i, _)| *i != k)
                .map(|(_, key)| key.clone())
                .collect();
            let step = Step::Keys {
                keys: fewer,
                probe: probe.clone(),
            };
            let steps = (best.steps.iter().enumerate())
                .map(|(i, s)| if i == n { step.clone() } else { s.clone() })
                .collect();
            let candidate = Case {
                steps,
                ..best.clone()
            };
            budget = budget.saturating_sub(1);
            if still_fails(server, &candidate, class) {
                best = candidate;
            } else {
                k = k.saturating_add(1);
            }
        }
        n = n.saturating_add(1);
    }
    best
}

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Random cases, swarm-generated; each failing one shrunk and saved to
/// the corpus. `ORACLE_SEED`, `ORACLE_RUNS` (cases at most),
/// `ORACLE_SECS` (wall-clock limit, 240 by default: two thirds running
/// cases, the rest shrinking), `ORACLE_JOBS` (at most 4), `ORACLE_SHRINK`
/// (runs per shrink). Known disagreements (`KNOWN`) are counted, not
/// shrunk.
#[test]
#[ignore = "fuzzing, on demand: cargo test --test oracle -- --ignored"]
fn fuzz() -> Outcome {
    let seed = env_number(
        "ORACLE_SEED",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    );
    let runs = usize::try_from(env_number("ORACLE_RUNS", 1000)).unwrap_or(1000);
    let secs = env_number("ORACLE_SECS", 240);
    let jobs = usize::try_from(env_number("ORACLE_JOBS", 4))
        .unwrap_or(4)
        .clamp(1, 4);
    let budget = usize::try_from(env_number("ORACLE_SHRINK", 20)).unwrap_or(20);
    let started = Instant::now();
    let until = support::after(Duration::from_secs(secs));
    let running = support::after(Duration::from_secs(secs.saturating_mul(2) / 3));
    let mut rng = Rng(seed);
    let cases: Vec<Case> = (0..runs).map(|_| random_case(rng.next())).collect();
    let (results, errors) = run_all(&cases, jobs, Some(running));
    let steps: usize = results.iter().map(|r| r.2.len()).sum();
    let mut classes: BTreeMap<(&str, bool), usize> = BTreeMap::new();
    for d in results.iter().flat_map(|r| &r.1) {
        let count = classes.entry((d.class, known(d))).or_default();
        *count = count.saturating_add(1);
    }
    eprintln!(
        "oracle fuzz: seed {seed}, {} cases, {steps} steps in {:.1}s; disagreements (class, known): {classes:?}; unanswered: {}\n{}",
        results.len(),
        started.elapsed().as_secs_f64(),
        errors.len(),
        errors.join("\n")
    );
    // ORACLE_BENCH=FILE: every case's steps and disagreements as JSON, the
    // same cases on any build (FUX_BIN), nothing shrunk or saved.
    if let Some(path) = std::env::var_os("ORACLE_BENCH") {
        let cases = (results.iter()).map(|(case, found, verdicts)| {
            let found = found.iter().map(|d| {
                object([
                    ("step", number(d.step)),
                    ("class", Value::Str(d.class.into())),
                    ("source", Value::Str(d.source.into())),
                    ("known", Value::Bool(known(d))),
                ])
            });
            object([
                ("mode", Value::Str(case.mode.name().into())),
                ("seed", number(case.seed)),
                ("chunk", number(case.chunk)),
                ("steps", number(verdicts.len())),
                ("disagreements", Value::List(found.collect())),
            ])
        });
        let out = object([
            ("seed", number(seed)),
            ("unanswered", number(errors.len())),
            ("cases", Value::List(cases.collect())),
        ]);
        std::fs::write(&path, format!("{}\n", out.render())).map_err(e)?;
        return Ok(());
    }
    // One shrunk case per class not known (and, with ORACLE_SAVE_KNOWN,
    // per entry of KNOWN).
    let save_known = std::env::var_os("ORACLE_SAVE_KNOWN").is_some();
    let server = Server::start(CONFIG)?;
    let mut saved: BTreeMap<String, String> = BTreeMap::new();
    let mut failed = String::new();
    for (case, found, _) in &results {
        let new: Vec<Disagreement> = found.iter().filter(|d| !known(d)).cloned().collect();
        if !new.is_empty() {
            failed.push_str(&describe(case, &new));
        }
        for d in found.iter().filter(|d| save_known || !known(d)) {
            let kind = kind(d);
            if saved.contains_key(&kind) || Instant::now() >= until {
                continue;
            }
            if !still_fails(&server, case, &kind) {
                saved.insert(kind, "did not happen again: not saved".into());
                continue;
            }
            let small = shrink(&server, case, &kind, budget, until);
            std::fs::create_dir_all(CORPUS).map_err(e)?;
            let mut n = 1usize;
            let path = loop {
                let path = Path::new(CORPUS).join(format!("{}-{n}.json", d.class));
                if !path.exists() {
                    break path;
                }
                n = n.saturating_add(1);
            };
            std::fs::write(&path, format!("{}\n", small.to_json(d.class).render())).map_err(e)?;
            saved.insert(
                kind,
                format!("{} ({} steps)", path.display(), small.steps.len()),
            );
        }
    }
    eprintln!(
        "oracle fuzz: {:.1}s in all; saved: {saved:?}",
        started.elapsed().as_secs_f64()
    );
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "fux disagrees with the side log:\n{failed}\nsaved: {saved:?}"
        ))
    }
}
